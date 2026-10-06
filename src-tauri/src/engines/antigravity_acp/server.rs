use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionResponse, McpCapabilities, PromptCapabilities, PromptResponse, ProtocolVersion,
    RequestPermissionResponse, SessionUpdate, StopReason, TextContent,
};
// Session setup and channel emission are the desktop ACP entry points; the
// daemon drives `run_prompt` directly and never builds these envelopes.
#[cfg(feature = "desktop")]
use agent_client_protocol::schema::{
    NewSessionRequest, NewSessionResponse, PromptRequest, SessionId, SessionNotification,
};
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex as TokioMutex;

use crate::executor::{PipedOpts, SpawnSpec, StdioMode};

/// Default antigravity CLI binary name.
const DEFAULT_AGY_CMD: &str = "agy";

/// How much of the CLI's stderr to keep for the error surfaced to the client.
/// Enough for a stack-free error line; bounded so a chatty CLI can't grow the
/// session unboundedly across a long turn.
const STDERR_KEEP_BYTES: usize = 4096;

/// State for a single Antigravity CLI session.
pub struct AntigravitySession {
    pub cwd: String,
    pub conversation_id: Option<String>,
    pub in_flight: Option<Arc<TokioMutex<Child>>>,
    /// Set by `handle_cancel`, read (and cleared) by `run_prompt` once the
    /// child's stdout closes. Without it a killed child is indistinguishable
    /// from one that finished, and the turn reports `EndTurn` for what the
    /// user experienced as a cancel — which ACP explicitly forbids.
    pub cancelled: bool,
    /// Sticky per-session model / mode, set by `handle_set_model` /
    /// `handle_set_mode`. Applied to every subsequent `run_prompt`.
    pub model: Option<String>,
    pub mode: Option<String>,
}

impl AntigravitySession {
    pub fn new(cwd: String) -> Self {
        Self {
            cwd,
            conversation_id: None,
            in_flight: None,
            cancelled: false,
            model: None,
            mode: None,
        }
    }
}

/// One decoded line of the CLI's `--output-format stream-json` NDJSON.
///
/// Kept as a separate type (rather than matching inline on `serde_json::Value`)
/// so the wire decoding is unit-testable without spawning the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum AgyEvent {
    /// Session handshake — carries the conversation id to resume with.
    Init { conversation_id: Option<String> },
    /// A streamed text delta. `thought` marks reasoning rather than output.
    Delta { text: String, thought: bool },
    /// Terminal event for the turn. `status` is the CLI's own status string.
    Result {
        status: String,
        error: Option<String>,
    },
    /// A line we parsed but don't model.
    Other,
}

/// Decode one NDJSON line from `agy --output-format stream-json`.
///
/// Returns `None` for blank lines and for anything that isn't JSON — the CLI
/// interleaves human-readable notices on stdout, and those are not errors.
pub fn parse_agy_line(line: &str) -> Option<AgyEvent> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let event = val.get("event").and_then(|e| e.as_str())?;

    match event {
        "init" => Some(AgyEvent::Init {
            conversation_id: val
                .get("conversation_id")
                .and_then(|id| id.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
        }),
        "step_update" => {
            let step = val.get("step_update")?;
            let text = step.get("text_delta").and_then(|d| d.as_str())?;
            if text.is_empty() {
                return Some(AgyEvent::Other);
            }
            let thought = step.get("step_type").and_then(|t| t.as_str()) == Some("thought");
            Some(AgyEvent::Delta {
                text: text.to_string(),
                thought,
            })
        }
        "result" => {
            let result = val.get("result")?;
            Some(AgyEvent::Result {
                status: result
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
                error: result
                    .get("error")
                    .and_then(|e| e.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            })
        }
        _ => Some(AgyEvent::Other),
    }
}

/// Antigravity CLI engine adapter normalizing to SessionUpdate ACP envelopes.
pub struct AntigravityEngine {
    sessions: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<AntigravitySession>>>>>,
    /// The CLI to run instead of resolving `agy` on PATH. Only tests set it.
    binary_override: Option<PathBuf>,
}

/// The argv for one `agy` turn. It never carries the prompt (I-7): a
/// process's argv is in `/proc/<pid>/cmdline`, which every user on the host
/// can read unless procfs is mounted `hidepid` — and on a multi-user (T1)
/// server a principal's shell session sees the host `/proc`, not the unit's
/// `ProtectProc=invisible` one. With `--input-format stream-json` (agy
/// 1.1.15+) the turn's text is read from stdin instead: one
/// `{"event":"user",…}` line ([`agy_stdin_payload`]), then EOF. It is the
/// same shape chi runs use (`chi_exec::build_engine_command_with`).
fn agy_args(conversation: Option<&str>, model: Option<&str>, mode: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(id) = conversation {
        args.extend(["--conversation".to_string(), id.to_string()]);
    }
    if let Some(m) = model {
        args.extend(["--model".to_string(), m.to_string()]);
    }
    if let Some(m) = mode {
        args.extend(["--mode".to_string(), m.to_string()]);
    }
    args
}

/// The bytes written to `agy`'s stdin for one turn (stdin is closed after
/// them, which ends the turn). Shared with chi runs so the two can't drift.
fn agy_stdin_payload(text: &str) -> String {
    crate::server::shared::chi_exec::stdin_payload("antigravity-cli", text)
}

impl Default for AntigravityEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AntigravityEngine {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(TokioMutex::new(HashMap::new())),
            binary_override: None,
        }
    }

    /// An engine that runs `binary` instead of `agy` (a test stub).
    #[cfg(test)]
    fn with_binary(binary: PathBuf) -> Self {
        Self {
            binary_override: Some(binary),
            ..Self::new()
        }
    }

    pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1;

    pub fn handle_initialize(&self, req: InitializeRequest) -> InitializeResponse {
        let negotiated = std::cmp::min(req.protocol_version, Self::PROTOCOL_VERSION);
        let prompt_caps = PromptCapabilities::default()
            .image(false)
            .embedded_context(true)
            .audio(false);
        let mcp_caps = McpCapabilities::default().http(true).sse(true);
        let mut caps = AgentCapabilities::default();
        caps.load_session = true;
        caps.prompt_capabilities = prompt_caps;
        caps.mcp_capabilities = mcp_caps;
        InitializeResponse::new(negotiated)
            .agent_capabilities(caps)
            .auth_methods(Vec::new())
    }

    /// Get (or create) the session for `thread_id`.
    ///
    /// An empty `cwd` means "caller doesn't know" — it only supplies the
    /// `$HOME` default when the session is genuinely new, so a later
    /// `run_prompt` can't downgrade a cwd that `handle_new_session` set.
    pub async fn register_session(
        &self,
        thread_id: String,
        cwd: String,
    ) -> Arc<TokioMutex<AntigravitySession>> {
        let mut guard = self.sessions.lock().await;
        guard
            .entry(thread_id)
            .or_insert_with(|| {
                let resolved = if cwd.is_empty() {
                    // $HOME is unset on Windows; fall back to the platform
                    // resolver, and only to "/" if that also comes up empty.
                    crate::platform::home_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".to_string())
                } else {
                    cwd
                };
                Arc::new(TokioMutex::new(AntigravitySession::new(resolved)))
            })
            .clone()
    }

    #[cfg(feature = "desktop")]
    pub async fn handle_new_session(
        &self,
        _app: AppHandle,
        req: NewSessionRequest,
    ) -> Result<NewSessionResponse, String> {
        let thread_id = req
            .meta
            .as_ref()
            .and_then(|meta| meta.get("threadId"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let cwd = req.cwd.to_string_lossy().into_owned();
        self.register_session(thread_id.clone(), cwd).await;
        Ok(NewSessionResponse::new(SessionId::new(thread_id)))
    }

    /// Desktop entry point: emit each delta on the Tauri channel the shell's
    /// chat layer listens to.
    ///
    /// The channel emit is expressed as an `on_update` callback rather than by
    /// handing `run_prompt` an `AppHandle`. Those were two ways of saying the
    /// same thing, and the `AppHandle` one is what made the engine adapters —
    /// and through them the whole daemon — unbuildable without a webview
    /// runtime. See the crate docs in `lib.rs`.
    #[cfg(feature = "desktop")]
    pub async fn handle_prompt(
        &self,
        app: AppHandle,
        req: PromptRequest,
    ) -> Result<PromptResponse, String> {
        let thread_id = req.session_id.0.to_string();
        let prompt_text = extract_prompt_text(&req);
        let channel = format!("chat://session/{thread_id}/antigravity");
        let session_id = SessionId::new(thread_id.clone());

        let emit = move |update: SessionUpdate| {
            let notif = SessionNotification::new(session_id.clone(), update);
            let _ = app.emit(&channel, &notif);
        };
        let cb: &(dyn Fn(SessionUpdate) + Send + Sync) = &emit;

        self.run_prompt(&thread_id, &prompt_text, None, Some(cb))
            .await
    }

    /// Run one turn against the `agy` CLI, streaming deltas as they arrive.
    ///
    /// `model` overrides the session's sticky model for this turn only; pass
    /// `None` to use whatever `handle_set_model` last stored.
    pub async fn run_prompt(
        &self,
        thread_id: &str,
        text: &str,
        model: Option<&str>,
        on_update_cb: Option<&(dyn Fn(SessionUpdate) + Send + Sync)>,
    ) -> Result<PromptResponse, String> {
        let session_arc = self
            .register_session(thread_id.to_string(), String::new())
            .await;
        let (cwd, conv_id, session_model, session_mode) = {
            let mut s = session_arc.lock().await;
            // Clear any cancel left over from a previous turn so it can't
            // retroactively cancel this one.
            s.cancelled = false;
            (
                s.cwd.clone(),
                s.conversation_id.clone(),
                s.model.clone(),
                s.mode.clone(),
            )
        };

        let cmd_binary = match &self.binary_override {
            Some(path) => path.clone(),
            None => which::which_in(DEFAULT_AGY_CMD, Some(crate::runtime::augmented_path()), ".")
                .or_else(|_| {
                    which::which_in("antigravity", Some(crate::runtime::augmented_path()), ".")
                })
                .unwrap_or_else(|_| PathBuf::from(DEFAULT_AGY_CMD)),
        };

        // Built as a `SpawnSpec` and spawned through the session executor
        // (WP-18); the T0 executor replays it onto a `tokio::process::Command`
        // unchanged, and on a T1 server it runs as the signed-in principal.
        // The prompt is never an argument (I-7, see `agy_args`).
        let mut cmd = SpawnSpec::new(cmd_binary);
        let turn_model = model.map(|m| m.to_string()).or(session_model);
        cmd.args(agy_args(
            conv_id.as_deref(),
            turn_model.as_deref(),
            session_mode.as_deref(),
        ));

        if !cwd.is_empty() {
            cmd.current_dir(&cwd);
        }

        cmd.env("PATH", crate::runtime::augmented_path());
        let piped = PipedOpts {
            // The turn's text goes in on stdin, which is closed right after
            // it: agy's stream-json input waits for more lines until EOF.
            stdin: StdioMode::Piped,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
            kill_on_drop: true,
            no_console_window: true,
            detached: false,
            new_process_group: false,
        };

        let mut child = crate::executor::current()
            .spawn_piped(cmd, piped)
            .map_err(|e| format!("spawn antigravity CLI: {e}"))?;

        // Drain stderr into a bounded buffer. It is the only place the CLI
        // explains a failure, so it has to reach the caller rather than a log
        // line nobody reads at the default filter level.
        let stderr_buf: Arc<TokioMutex<String>> = Arc::new(TokioMutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let tid = thread_id.to_string();
            let sink = stderr_buf.clone();
            tauri::async_runtime::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            tracing::warn!(
                                target: "ikenga::engines::antigravity_acp",
                                thread = %tid,
                                "antigravity stderr: {line}"
                            );
                            let mut buf = sink.lock().await;
                            if buf.len() < STDERR_KEEP_BYTES {
                                buf.push_str(&line);
                                buf.push('\n');
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(
                                target: "ikenga::engines::antigravity_acp",
                                thread = %tid,
                                "antigravity stderr read failed: {e}"
                            );
                            break;
                        }
                    }
                }
            });
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "antigravity stdout not piped".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "antigravity stdin not piped".to_string())?;
        let child_handle = Arc::new(TokioMutex::new(child));
        {
            let mut s = session_arc.lock().await;
            s.in_flight = Some(child_handle.clone());
        }

        // The turn's text goes to stdin — a pipe only this process and the
        // CLI hold — and stdin is closed, which ends the turn (I-7). Written
        // after `in_flight` is set so a cancel can still kill a child that
        // stalls on the write. A failed write leaves the CLI with no prompt,
        // so it is killed rather than left waiting.
        let mut stdin_error: Option<String> = None;
        if let Err(e) =
            crate::server::shared::chi_exec::write_prompt(stdin, &agy_stdin_payload(text)).await
        {
            let _ = child_handle.lock().await.start_kill();
            stdin_error = Some(format!("antigravity stdin write failed: {e}"));
        }

        let mut lines = BufReader::new(stdout).lines();
        let mut stop_reason = StopReason::EndTurn;
        let mut turn_error: Option<String> = None;
        // A read error mid-stream truncates the turn. Reporting EndTurn for a
        // half-delivered answer is the worst outcome, so it is tracked and
        // surfaced like any other failure.
        let mut read_error: Option<String> = None;

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let Some(event) = parse_agy_line(&line) else {
                        continue;
                    };
                    match event {
                        AgyEvent::Init { conversation_id } => {
                            if let Some(cid) = conversation_id {
                                let mut s = session_arc.lock().await;
                                if s.conversation_id.is_none() {
                                    s.conversation_id = Some(cid);
                                }
                            }
                        }
                        AgyEvent::Delta { text, thought } => {
                            let chunk =
                                ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
                            let update = if thought {
                                SessionUpdate::AgentThoughtChunk(chunk)
                            } else {
                                SessionUpdate::AgentMessageChunk(chunk)
                            };

                            if let Some(cb) = on_update_cb {
                                cb(update);
                            }
                        }
                        AgyEvent::Result { status, error } => {
                            if !status.eq_ignore_ascii_case("SUCCESS") {
                                stop_reason = StopReason::Refusal;
                                turn_error =
                                    Some(error.unwrap_or_else(|| format!("antigravity: {status}")));
                            }
                        }
                        AgyEvent::Other => {}
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    read_error = Some(format!("antigravity stdout read failed: {e}"));
                    break;
                }
            }
        }

        let cancelled = {
            let mut s = session_arc.lock().await;
            s.in_flight = None;
            std::mem::take(&mut s.cancelled)
        };

        {
            let mut child_guard = child_handle.lock().await;
            let _ = child_guard.wait().await;
        }

        // Cancellation wins over everything: ACP requires `Cancelled` after a
        // `session/cancel`, even when the kill made the underlying read fail.
        if cancelled {
            return Ok(PromptResponse::new(StopReason::Cancelled));
        }
        if let Some(e) = stdin_error {
            return Err(e);
        }
        if let Some(e) = read_error {
            return Err(e);
        }
        if let Some(e) = turn_error {
            let stderr_tail = stderr_buf.lock().await.trim().to_string();
            return Err(if stderr_tail.is_empty() {
                e
            } else {
                format!("{e}\n{stderr_tail}")
            });
        }

        Ok(PromptResponse::new(stop_reason))
    }

    pub async fn handle_cancel(&self, thread_id: String) -> Result<(), String> {
        let session_arc = {
            let guard = self.sessions.lock().await;
            guard.get(&thread_id).cloned()
        };
        if let Some(session) = session_arc {
            let mut s = session.lock().await;
            // Marked even when nothing is in flight: a cancel that races the
            // spawn still has to be honoured by the turn it lands on.
            s.cancelled = true;
            if let Some(child_handle) = s.in_flight.take() {
                let mut child = child_handle.lock().await;
                let _ = child.start_kill();
            }
        }
        Ok(())
    }

    pub async fn resolve_permission(
        &self,
        _request_id: String,
        _response: RequestPermissionResponse,
    ) -> Result<(), String> {
        Ok(())
    }

    pub async fn handle_load_session(
        &self,
        _thread_id: String,
    ) -> Result<LoadSessionResponse, String> {
        Ok(LoadSessionResponse::new())
    }

    /// Store the mode for subsequent turns. Applied as `--mode` by
    /// `run_prompt`; the CLI has no way to change it mid-turn.
    pub async fn handle_set_mode(&self, thread_id: String, mode_id: String) -> Result<(), String> {
        let session = self.register_session(thread_id, String::new()).await;
        session.lock().await.mode = Some(mode_id);
        Ok(())
    }

    /// Store the model for subsequent turns. Applied as `--model` by
    /// `run_prompt` unless that call passes an explicit per-turn override.
    pub async fn handle_set_model(
        &self,
        thread_id: String,
        model: Option<String>,
    ) -> Result<(), String> {
        let session = self.register_session(thread_id, String::new()).await;
        session.lock().await.model = model.filter(|m| !m.is_empty());
        Ok(())
    }

    /// The `agy` CLI exposes no reasoning-effort control, so this is reported
    /// as unsupported rather than silently accepted — an `Ok(())` here would
    /// tell the UI the setting took when nothing changed.
    #[cfg(feature = "desktop")]
    pub async fn handle_set_effort(
        &self,
        _thread_id: String,
        _effort: crate::claude::session::EffortLevel,
    ) -> Result<(), String> {
        Err("antigravity CLI does not support reasoning-effort selection".to_string())
    }
}

pub type AntigravityEngineState = Arc<AntigravityEngine>;

#[cfg(feature = "desktop")]
fn extract_prompt_text(req: &PromptRequest) -> String {
    let mut parts = Vec::new();
    for block in &req.prompt {
        if let ContentBlock::Text(t) = block {
            parts.push(t.text.as_str());
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_returns_negotiated_version_and_caps() {
        let engine = AntigravityEngine::new();
        let req = InitializeRequest::new(ProtocolVersion::V1);
        let resp = engine.handle_initialize(req);
        assert_eq!(resp.protocol_version, ProtocolVersion::V1);
        assert!(resp.agent_capabilities.load_session);
        assert!(resp.agent_capabilities.mcp_capabilities.http);
    }

    #[tokio::test]
    async fn session_registration_and_cancel_work() {
        let engine = AntigravityEngine::new();
        engine.register_session("t-123".into(), "/tmp".into()).await;
        let cancel_res = engine.handle_cancel("t-123".into()).await;
        assert!(cancel_res.is_ok());
    }

    #[tokio::test]
    async fn cancel_marks_the_session_even_with_nothing_in_flight() {
        let engine = AntigravityEngine::new();
        engine.register_session("t-1".into(), "/tmp".into()).await;
        engine.handle_cancel("t-1".into()).await.unwrap();
        let s = engine.register_session("t-1".into(), String::new()).await;
        assert!(
            s.lock().await.cancelled,
            "cancel must be sticky until a turn consumes it"
        );
    }

    #[tokio::test]
    async fn register_session_does_not_downgrade_a_known_cwd() {
        let engine = AntigravityEngine::new();
        engine
            .register_session("t-2".into(), "/work/project".into())
            .await;
        // A later call with an unknown cwd (what `run_prompt` passes) must not
        // replace the real one with the $HOME fallback.
        let s = engine.register_session("t-2".into(), String::new()).await;
        assert_eq!(s.lock().await.cwd, "/work/project");
    }

    #[tokio::test]
    async fn set_model_and_mode_are_stored() {
        let engine = AntigravityEngine::new();
        engine
            .handle_set_model("t-3".into(), Some("gemini-3-pro".into()))
            .await
            .unwrap();
        engine
            .handle_set_mode("t-3".into(), "plan".into())
            .await
            .unwrap();
        let s = engine.register_session("t-3".into(), String::new()).await;
        {
            let g = s.lock().await;
            assert_eq!(g.model.as_deref(), Some("gemini-3-pro"));
            assert_eq!(g.mode.as_deref(), Some("plan"));
        }
        // Empty string clears rather than pinning a meaningless flag value.
        engine
            .handle_set_model("t-3".into(), Some(String::new()))
            .await
            .unwrap();
        assert!(s.lock().await.model.is_none());
    }

    // `handle_set_effort` takes an `EffortLevel` from the desktop-only `claude`
    // module, so this assertion only exists in that build.
    #[cfg(feature = "desktop")]
    #[tokio::test]
    async fn set_effort_is_refused_rather_than_silently_accepted() {
        let engine = AntigravityEngine::new();
        assert!(
            engine
                .handle_set_effort("t-4".into(), crate::claude::session::EffortLevel::High)
                .await
                .is_err(),
            "the agy CLI has no effort control; reporting Ok would tell the UI it took"
        );
    }

    #[test]
    fn parses_an_agent_response_delta() {
        let line = r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"Hello world\n"}}"#;
        assert_eq!(
            parse_agy_line(line),
            Some(AgyEvent::Delta {
                text: "Hello world\n".to_string(),
                thought: false
            })
        );
    }

    #[test]
    fn thought_steps_are_flagged_separately_from_output() {
        let line = r#"{"event":"step_update","step_update":{"step_type":"thought","text_delta":"considering"}}"#;
        assert_eq!(
            parse_agy_line(line),
            Some(AgyEvent::Delta {
                text: "considering".to_string(),
                thought: true
            })
        );
    }

    #[test]
    fn init_carries_the_conversation_id_and_ignores_an_empty_one() {
        assert_eq!(
            parse_agy_line(r#"{"event":"init","conversation_id":"c-42"}"#),
            Some(AgyEvent::Init {
                conversation_id: Some("c-42".to_string())
            })
        );
        assert_eq!(
            parse_agy_line(r#"{"event":"init","conversation_id":""}"#),
            Some(AgyEvent::Init {
                conversation_id: None
            })
        );
    }

    #[test]
    fn result_keeps_the_status_and_error_text() {
        assert_eq!(
            parse_agy_line(
                r#"{"event":"result","result":{"status":"ERROR","error":"quota exceeded"}}"#
            ),
            Some(AgyEvent::Result {
                status: "ERROR".to_string(),
                error: Some("quota exceeded".to_string())
            })
        );
    }

    #[test]
    fn non_json_and_blank_lines_are_skipped_not_treated_as_failures() {
        // The CLI interleaves plain notices on stdout; they must not abort the
        // turn or be mistaken for output.
        assert_eq!(parse_agy_line(""), None);
        assert_eq!(parse_agy_line("   "), None);
        assert_eq!(parse_agy_line("Downloading model..."), None);
        assert_eq!(parse_agy_line(r#"{"no_event":1}"#), None);
    }

    #[test]
    fn empty_deltas_do_not_produce_an_output_chunk() {
        assert_eq!(
            parse_agy_line(r#"{"event":"step_update","step_update":{"text_delta":""}}"#),
            Some(AgyEvent::Other)
        );
    }

    // ── I-7: the turn's text never reaches argv ─────────────────────────

    const SENTINEL: &str = "I7-AGY-SENTINEL-4c1f";

    #[test]
    fn agy_argv_never_carries_the_prompt_and_reads_it_from_stdin() {
        let args = agy_args(Some("conv-1"), Some("gemini-x"), Some("plan"));
        assert_eq!(
            args,
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--conversation",
                "conv-1",
                "--model",
                "gemini-x",
                "--mode",
                "plan",
            ]
        );
        assert!(!args.iter().any(|a| a == "-p" || a == "--prompt"));

        let prompt = format!("{SENTINEL} line one\nline \"two\" $HOME `id`");
        let payload = agy_stdin_payload(&prompt);
        assert!(payload.ends_with('\n'));
        assert_eq!(payload.matches('\n').count(), 1, "one NDJSON line");
        let v: serde_json::Value = serde_json::from_str(payload.trim_end()).unwrap();
        assert_eq!(v["event"], "user");
        assert_eq!(v["message"]["content"], prompt);
    }

    /// A fake `agy`: records its argv, its own `/proc/<pid>/cmdline` (what
    /// another user on the host would see) and its stdin into the cwd, then
    /// speaks stream-json. It reads stdin to EOF first, so a turn whose stdin
    /// is never closed hangs instead of passing.
    ///
    /// Written once per test process: writing an executable while other test
    /// threads fork can make a concurrent exec of it fail with ETXTBSY.
    #[cfg(unix)]
    fn stub_agy() -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        static STUB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        STUB.get_or_init(|| {
            let dir =
                std::env::temp_dir().join(format!("ikenga-agy-stub-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("agy");
            std::fs::write(
                &path,
                r#"#!/bin/sh
printf '%s\n' "$@" > argv
if [ -r /proc/$$/cmdline ]; then tr '\000' ' ' < /proc/$$/cmdline > cmdline; fi
cat >> stdin
printf '%s\n' '{"event":"init","conversation_id":"conv-stub"}'
printf '%s\n' '{"event":"step_update","step_update":{"step_type":"response","text_delta":"PONG"}}'
printf '%s\n' '{"event":"result","result":{"status":"SUCCESS"}}'
"#,
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        })
        .clone()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn chat_turns_send_the_prompt_on_stdin_never_argv() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().canonicalize().unwrap();
        let read = |name: &str| std::fs::read_to_string(cwd.join(name)).unwrap_or_default();
        let engine = AntigravityEngine::with_binary(stub_agy());
        engine
            .register_session("t-i7".into(), cwd.to_string_lossy().into_owned())
            .await;

        let out = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = out.clone();
        let cb = move |u: SessionUpdate| {
            if let SessionUpdate::AgentMessageChunk(c) = u {
                if let ContentBlock::Text(t) = c.content {
                    sink.lock().unwrap().push_str(&t.text);
                }
            }
        };
        let cb: &(dyn Fn(SessionUpdate) + Send + Sync) = &cb;
        let turn = |text: String| {
            let engine = &engine;
            async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    engine.run_prompt("t-i7", &text, Some("m-1"), Some(cb)),
                )
                .await
                .expect("turn hung: stdin was not closed")
            }
        };

        // First turn: a new conversation.
        let first = format!("{SENTINEL} first turn, it's $HOME; `id` \"q\"\nsecond line");
        let res = turn(first.clone()).await.unwrap();
        assert_eq!(res.stop_reason, StopReason::EndTurn);
        assert_eq!(out.lock().unwrap().as_str(), "PONG");
        let argv = read("argv");
        assert!(!argv.is_empty(), "the stub ran");
        assert!(!argv.contains(SENTINEL), "argv: {argv}");
        assert!(!argv.lines().any(|a| a == "-p"), "argv: {argv}");
        assert!(
            argv.contains("--input-format\nstream-json\n"),
            "argv: {argv}"
        );
        assert!(!argv.contains("--conversation"), "argv: {argv}");
        #[cfg(target_os = "linux")]
        {
            let cmdline = read("cmdline");
            assert!(!cmdline.is_empty(), "the stub read its own cmdline");
            assert!(!cmdline.contains(SENTINEL), "cmdline: {cmdline}");
        }
        let stdin = read("stdin");
        let v: serde_json::Value = serde_json::from_str(stdin.trim_end()).unwrap();
        assert_eq!(v["event"], "user");
        assert_eq!(v["message"]["content"], first);

        // Second turn resumes the conversation by id; the follow-up goes to
        // stdin too.
        let follow_up = format!("{SENTINEL} follow-up");
        turn(follow_up.clone()).await.unwrap();
        let argv = read("argv");
        assert!(!argv.contains(SENTINEL), "resume argv: {argv}");
        assert!(argv.contains("--conversation\nconv-stub\n"), "argv: {argv}");
        #[cfg(target_os = "linux")]
        assert!(!read("cmdline").contains(SENTINEL));
        let stdin = read("stdin");
        let second: serde_json::Value =
            serde_json::from_str(stdin.lines().nth(1).unwrap()).unwrap();
        assert_eq!(second["message"]["content"], follow_up);
    }
}
