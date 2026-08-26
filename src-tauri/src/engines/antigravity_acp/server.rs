use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    LoadSessionResponse, McpCapabilities, NewSessionRequest, NewSessionResponse,
    PromptCapabilities, PromptRequest, PromptResponse, ProtocolVersion, RequestPermissionResponse,
    SessionId, SessionNotification, SessionUpdate, StopReason, TextContent,
};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as TokioMutex;

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
                    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
                } else {
                    cwd
                };
                Arc::new(TokioMutex::new(AntigravitySession::new(resolved)))
            })
            .clone()
    }

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

    pub async fn handle_prompt(
        &self,
        app: AppHandle,
        req: PromptRequest,
    ) -> Result<PromptResponse, String> {
        let thread_id = req.session_id.0.to_string();
        let prompt_text = extract_prompt_text(&req);
        self.run_prompt(Some(&app), &thread_id, &prompt_text, None, None)
            .await
    }

    /// Run one turn against the `agy` CLI, streaming deltas as they arrive.
    ///
    /// `model` overrides the session's sticky model for this turn only; pass
    /// `None` to use whatever `handle_set_model` last stored.
    pub async fn run_prompt(
        &self,
        app: Option<&AppHandle>,
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

        let cmd_binary =
            which::which_in(DEFAULT_AGY_CMD, Some(crate::runtime::augmented_path()), ".")
                .or_else(|_| {
                    which::which_in("antigravity", Some(crate::runtime::augmented_path()), ".")
                })
                .unwrap_or_else(|_| PathBuf::from(DEFAULT_AGY_CMD));

        let mut cmd = Command::new(cmd_binary);
        cmd.arg("-p")
            .arg(text)
            .arg("--output-format")
            .arg("stream-json");

        if !cwd.is_empty() {
            cmd.current_dir(&cwd);
        }

        if let Some(id) = &conv_id {
            cmd.arg("--conversation").arg(id);
        }
        if let Some(m) = model.map(|m| m.to_string()).or(session_model) {
            cmd.arg("--model").arg(m);
        }
        if let Some(m) = session_mode {
            cmd.arg("--mode").arg(m);
        }

        // The prompt goes in on argv, so the child has no use for stdin.
        // Leaving it as an open pipe we never write to and never close risks
        // the CLI blocking forever on a read that can't complete.
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PATH", crate::runtime::augmented_path())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
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
        let child_handle = Arc::new(TokioMutex::new(child));
        {
            let mut s = session_arc.lock().await;
            s.in_flight = Some(child_handle.clone());
        }

        let channel = format!("chat://session/{thread_id}/antigravity");
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
                                cb(update.clone());
                            }

                            if let Some(a) = app {
                                let notif = SessionNotification::new(
                                    SessionId::new(thread_id.to_string()),
                                    update,
                                );
                                let _ = a.emit(&channel, &notif);
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
    pub async fn handle_set_effort(
        &self,
        _thread_id: String,
        _effort: crate::claude::session::EffortLevel,
    ) -> Result<(), String> {
        Err("antigravity CLI does not support reasoning-effort selection".to_string())
    }
}

pub type AntigravityEngineState = Arc<AntigravityEngine>;

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
    async fn set_model_and_mode_are_stored_set_effort_is_refused() {
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

        assert!(
            engine
                .handle_set_effort("t-3".into(), crate::claude::session::EffortLevel::High)
                .await
                .is_err(),
            "effort is unsupported and must not report success"
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
}
