//! `CodexPtyEngine` — Codex CLI adapter using `codex exec --json` per turn.
//!
//! ADR-013 Phase 3. Despite the "PTY" suffix on the module path (kept to
//! avoid a churn-only rename — see ADR-013 §6 negatives), this engine does
//! NOT use a PTY. It spawns `codex exec --json` as a one-shot per turn,
//! line-reads the JSONL stream off stdout, parses each event with
//! `parser::parse_event`, and emits ACP `SessionUpdate`s on
//! `chat://session/{thread_id}`.
//!
//! ### Per-turn lifecycle
//!
//! 1. First turn (no `codex_thread_id` cached): spawn
//!    `codex exec --json --skip-git-repo-check --cd <cwd> -`. Write the
//!    user prompt to stdin, close stdin. Capture the `thread.started`
//!    event's `thread_id` and cache it on the session row.
//! 2. Subsequent turns: spawn `codex exec resume <thread_id> --json
//!    --skip-git-repo-check --cd <cwd> -`. Codex restores context from its
//!    on-disk session store; we re-attach as if it were a fresh turn.
//! 3. When `turn.completed` arrives → return `StopReason::EndTurn`.
//!    `turn.failed` → emit an error chunk + return `StopReason::Refusal`.
//!    EOF without a terminal event → fall back to `EndTurn`.
//!
//! ### Why per-turn
//!
//! Codex's non-interactive exec mode is one-prompt-per-process by design.
//! There's no long-lived stdio session like `claude --output-format
//! stream-json`. The `codex exec resume` subcommand papers over this for
//! us — context is preserved on disk between turns.
//!
//! ### What was retired vs. the previous PTY-wrap stub
//!
//! - `strip_ansi_escapes` use — no longer relevant; stream is structured.
//! - `PtyManager` plumbing — kept as a constructor param to avoid a
//!   `lib.rs` ripple, but unused. TODO: drop the param in the next ADR
//!   that lets us rename the module path.
//! - 60s wallclock idle-marker timeout — the JSONL stream has explicit
//!   turn boundaries, so no timer is needed.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use agent_client_protocol::schema::{
    AgentCapabilities, InitializeRequest, InitializeResponse, LoadSessionResponse, McpCapabilities,
    NewSessionResponse, PromptCapabilities, PromptResponse, ProtocolVersion, SessionId,
    SessionNotification, StopReason,
};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::engines::codex_pty::parser::{parse_event, to_session_updates, ParsedEvent};
use crate::pty::PtyManager;

/// Default codex executable name. Resolved via `$PATH` at spawn time.
const DEFAULT_CODEX_CMD: &str = "codex";

/// One entry in the engine's session table.
///
/// `codex_thread_id` is the id returned by codex's first `thread.started`
/// event; we feed it back via `codex exec resume <id>` on subsequent turns
/// so Codex restores context from its on-disk session store.
struct CodexSession {
    cwd: String,
    /// Resume id captured from `thread.started`. None until the first
    /// successful turn.
    codex_thread_id: Option<String>,
    /// The currently in-flight child, if any. Used by `handle_cancel`
    /// to send SIGINT.
    in_flight: Option<Arc<Mutex<Child>>>,
}

impl CodexSession {
    fn new(cwd: String) -> Self {
        Self {
            cwd,
            codex_thread_id: None,
            in_flight: None,
        }
    }
}

pub struct CodexPtyEngine {
    // PtyManager is retained for ABI compatibility with `lib.rs::run()`
    // but no longer used — codex exec runs through tokio::process. TODO:
    // drop this param in the same change that renames the module path
    // off `codex_pty`.
    #[allow(dead_code)]
    pty: Arc<PtyManager>,
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<CodexSession>>>>>,
}

impl CodexPtyEngine {
    pub fn new(pty: Arc<PtyManager>) -> Self {
        Self {
            pty,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1;

    /// Static capabilities advertisement. Codex via `codex exec --json`:
    /// - No image input through this surface (CLI is text in, JSON out).
    /// - No streaming-thinking surface; reasoning items arrive as
    ///   completed chunks.
    /// - Has MCP via codex's own MCP integration (configured per-user
    ///   in `~/.codex/config.toml`, not through the AppBridge).
    /// - Supports session resume (`codex exec resume <thread_id>`).
    pub fn handle_initialize(&self, req: InitializeRequest) -> InitializeResponse {
        let negotiated = std::cmp::min(req.protocol_version, Self::PROTOCOL_VERSION);
        let prompt_caps = PromptCapabilities::default()
            .image(false)
            .embedded_context(true)
            .audio(false);
        let mcp_caps = McpCapabilities::default().http(false).sse(false);
        let agent_caps = AgentCapabilities::default()
            .load_session(true)
            .prompt_capabilities(prompt_caps)
            .mcp_capabilities(mcp_caps);
        InitializeResponse::new(negotiated)
            .agent_capabilities(agent_caps)
            .auth_methods(Vec::new())
    }

    /// Register a session id with the engine. Idempotent — re-registering
    /// the same id updates the recorded cwd but otherwise no-ops. The
    /// codex child is NOT spawned here; that's deferred to `handle_prompt`.
    pub async fn handle_new_session(
        &self,
        thread_id: String,
        cwd: String,
    ) -> Result<NewSessionResponse, String> {
        let cwd = if cwd.is_empty() {
            std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
        } else {
            cwd
        };
        let mut sessions = self.sessions.lock().await;
        sessions
            .entry(thread_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(CodexSession::new(cwd))));
        Ok(NewSessionResponse::new(SessionId::new(thread_id)))
    }

    /// Spawn `codex exec --json` for one turn, feed the user prompt on
    /// stdin, drain the JSONL stream, emit SessionUpdates as we go, and
    /// return when we see a terminal event.
    pub async fn handle_prompt(
        &self,
        app: AppHandle,
        thread_id: String,
        text: String,
    ) -> Result<PromptResponse, String> {
        let session_arc = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&thread_id)
                .cloned()
                .ok_or_else(|| format!("no codex session for thread {thread_id}"))?
        };

        let (cwd, resume_id) = {
            let s = session_arc.lock().await;
            (s.cwd.clone(), s.codex_thread_id.clone())
        };

        // Build the per-turn command. `codex exec` reads the prompt from
        // stdin when given `-` as the positional arg. `--skip-git-repo-check`
        // makes the spawn predictable inside arbitrary project dirs
        let mut cmd = {
            #[cfg(windows)]
            {
                let resolved = which::which_in(DEFAULT_CODEX_CMD, Some(crate::runtime::augmented_path()), ".")
                    .or_else(|_| which::which_in("codex.cmd", Some(crate::runtime::augmented_path()), "."))
                    .or_else(|_| which::which_in("codex.exe", Some(crate::runtime::augmented_path()), "."));
                if let Ok(p) = resolved {
                    let is_batch = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
                        .unwrap_or(false);
                    if is_batch {
                        let mut c = Command::new("cmd.exe");
                        c.arg("/c").arg(p);
                        c
                    } else {
                        Command::new(p)
                    }
                } else {
                    Command::new(DEFAULT_CODEX_CMD)
                }
            }
            #[cfg(not(windows))]
            {
                Command::new(DEFAULT_CODEX_CMD)
            }
        };
        if let Some(resume) = &resume_id {
            cmd.args(["exec", "resume", resume, "--json"]);
        } else {
            cmd.args(["exec", "--json"]);
        }
        cmd.args(["--skip-git-repo-check", "--cd", &cwd, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Augmented PATH so codex (and any node it shells out to) resolves
            // under nvm/npm/homebrew when the app has a thin GUI $PATH
            // (ADR-013 §Addendum Decision 2).
            .env("PATH", crate::runtime::augmented_path())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| format!("spawn codex exec: {e}"))?;

        // Write the prompt to stdin and close it so codex knows the user
        // input is complete.
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "codex stdin not piped".to_string())?;
        let prompt = text.clone();
        let mut stdin = stdin;
        if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
            return Err(format!("write codex stdin: {e}"));
        }
        // Drop closes the pipe — equivalent to EOF on stdin.
        drop(stdin);

        // Drain stderr in the background so the pipe doesn't fill up.
        if let Some(stderr) = child.stderr.take() {
            let tid = thread_id.clone();
            tauri::async_runtime::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!(
                        target: "ikenga::engines::codex_pty",
                        "codex[{tid}] stderr: {line}",
                    );
                }
            });
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "codex stdout not piped".to_string())?;

        // Stash the child in the session so `handle_cancel` can SIGINT it.
        // The Arc<Mutex<>> wrapping is so the cancel path and the post-loop
        // wait path can both take ownership semantics.
        let child_handle = Arc::new(Mutex::new(child));
        {
            let mut s = session_arc.lock().await;
            s.in_flight = Some(child_handle.clone());
        }

        // Per-engine channel suffix (`.../codex`) so two adapters attached
        // to the same thread don't both render every event — see the same
        // change in claude_code/server.rs + gemini_acp/transport.rs.
        let channel = format!("chat://session/{thread_id}/codex");
        let mut lines = BufReader::new(stdout).lines();
        let mut stop_reason: Option<StopReason> = None;
        let mut error_message: Option<String> = None;

        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let parsed = match parse_event(&line) {
                        Ok(ev) => ev,
                        Err(e) => {
                            log::warn!(
                                target: "ikenga::engines::codex_pty",
                                "codex[{thread_id}] failed to parse line: {e} (raw: {line})",
                            );
                            continue;
                        }
                    };

                    match &parsed {
                        ParsedEvent::ThreadStarted {
                            thread_id: codex_tid,
                        } => {
                            // Capture the resume id for subsequent turns.
                            // Only update if we don't already have one (defensive —
                            // codex might re-emit it on resume too).
                            let mut s = session_arc.lock().await;
                            if s.codex_thread_id.is_none() && !codex_tid.is_empty() {
                                s.codex_thread_id = Some(codex_tid.clone());
                            }
                        }
                        ParsedEvent::TurnCompleted { .. } => {
                            stop_reason = Some(StopReason::EndTurn);
                        }
                        ParsedEvent::TurnFailed { message } => {
                            error_message = Some(message.clone());
                            stop_reason = Some(StopReason::Refusal);
                        }
                        ParsedEvent::Unknown(raw) => {
                            log::debug!(
                                target: "ikenga::engines::codex_pty",
                                "codex[{thread_id}] unknown event: {raw}",
                            );
                        }
                        _ => {}
                    }

                    // Emit translated SessionUpdates regardless of phase
                    // (top-level events return empty, items return the
                    // mapped chunk/tool call/plan).
                    for upd in to_session_updates(&parsed) {
                        let notif =
                            SessionNotification::new(SessionId::new(thread_id.clone()), upd);
                        let _ = app.emit(&channel, &notif);
                    }

                    // turn.completed / turn.failed are terminal; break
                    // here so we don't keep blocking on stdout if the
                    // child takes a moment to exit.
                    if stop_reason.is_some() {
                        break;
                    }
                }
                Ok(None) => break, // EOF
                Err(e) => {
                    log::warn!(
                        target: "ikenga::engines::codex_pty",
                        "codex[{thread_id}] stdout read error: {e}",
                    );
                    break;
                }
            }
        }

        // Reap the child so we don't leak zombies even on short turns.
        // Best-effort — if it's already gone, that's fine.
        {
            let mut guard = child_handle.lock().await;
            let _ = guard.wait().await;
        }
        // Clear the in_flight handle so a stale cancel doesn't kill a
        // future turn.
        {
            let mut s = session_arc.lock().await;
            s.in_flight = None;
        }

        match stop_reason {
            Some(StopReason::Refusal) => {
                log::warn!(
                    target: "ikenga::engines::codex_pty",
                    "codex[{thread_id}] turn.failed: {}",
                    error_message.as_deref().unwrap_or("(no message)"),
                );
                Ok(PromptResponse::new(StopReason::Refusal))
            }
            Some(reason) => Ok(PromptResponse::new(reason)),
            None => {
                // EOF without a terminal event — codex exited cleanly but
                // never emitted turn.completed. Fall back to EndTurn so
                // the FE's request future resolves.
                log::warn!(
                    target: "ikenga::engines::codex_pty",
                    "codex[{thread_id}] stdout closed without turn.completed; defaulting to EndTurn",
                );
                Ok(PromptResponse::new(StopReason::EndTurn))
            }
        }
    }

    /// Best-effort cancel: send SIGKILL to the in-flight `codex exec`
    /// child. Codex doesn't have a graceful interrupt envelope on its
    /// non-interactive surface, so this is the available lever. Stale
    /// `threadId` is a no-op.
    pub async fn handle_cancel(&self, thread_id: String) -> Result<(), String> {
        let Some(session_arc) = self.sessions.lock().await.get(&thread_id).cloned() else {
            return Ok(());
        };
        let child = {
            let s = session_arc.lock().await;
            s.in_flight.clone()
        };
        let Some(child) = child else {
            return Ok(());
        };
        let mut guard = child.lock().await;
        // tokio::process::Child::start_kill sends SIGKILL on Unix. We
        // don't bother with SIGINT-then-SIGKILL because codex exec
        // doesn't checkpoint between events — a SIGINT race is no
        // friendlier than a hard kill, and slower to surface to the FE.
        let _ = guard.start_kill();
        Ok(())
    }

    /// Re-attach to a session by thread id. Codex's resume context lives
    /// on disk; here we just confirm the in-memory row exists.
    pub async fn handle_load_session(
        &self,
        thread_id: String,
    ) -> Result<LoadSessionResponse, String> {
        let sessions = self.sessions.lock().await;
        if sessions.contains_key(&thread_id) {
            Ok(LoadSessionResponse::new())
        } else {
            Err(format!("no codex session for thread {thread_id}"))
        }
    }

    /// Modes / model / effort are no-ops for codex today. Codex's CLI
    /// reads model + reasoning settings from its own config; per-turn
    /// switching via the chat header is deferred.
    pub async fn handle_set_mode(
        &self,
        _thread_id: String,
        _mode_id: String,
    ) -> Result<(), String> {
        Ok(())
    }

    pub async fn handle_set_model(
        &self,
        _thread_id: String,
        _model: Option<String>,
    ) -> Result<(), String> {
        Ok(())
    }

    pub async fn handle_set_effort(
        &self,
        _thread_id: String,
        _effort: crate::claude::session::EffortLevel,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Codex exec runs with its own sandbox policy (configured via
    /// `--sandbox`), so we don't drive interactive permission round-trips
    /// through this engine today. Resolving a permission is therefore a
    /// no-op — kept for dispatcher symmetry with Claude / Gemini.
    pub async fn resolve_permission(
        &self,
        _request_id: String,
        _response: agent_client_protocol::schema::RequestPermissionResponse,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Tauri-friendly wrapper around the engine.
pub type CodexPtyEngineState = Arc<CodexPtyEngine>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_session_is_idempotent() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        let resp1 = engine
            .handle_new_session("t1".into(), "/tmp".into())
            .await
            .expect("first new_session");
        let resp2 = engine
            .handle_new_session("t1".into(), "/tmp".into())
            .await
            .expect("second new_session");
        assert_eq!(resp1.session_id.0.as_ref(), "t1");
        assert_eq!(resp2.session_id.0.as_ref(), "t1");
        assert_eq!(engine.sessions.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn new_session_falls_back_to_home_when_cwd_empty() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        let _ = engine
            .handle_new_session("t_empty".into(), String::new())
            .await
            .expect("ok");
        let sessions = engine.sessions.lock().await;
        let s = sessions.get("t_empty").unwrap().lock().await;
        assert!(!s.cwd.is_empty());
    }

    #[tokio::test]
    async fn cancel_on_unknown_thread_is_ok() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        engine
            .handle_cancel("never_registered".into())
            .await
            .expect("unknown thread cancel ok");
    }

    #[tokio::test]
    async fn cancel_with_no_in_flight_child_is_ok() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        engine
            .handle_new_session("t_idle".into(), "/tmp".into())
            .await
            .expect("new_session ok");
        engine
            .handle_cancel("t_idle".into())
            .await
            .expect("idle cancel ok");
    }

    #[tokio::test]
    async fn load_session_for_known_thread_returns_ok() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        engine
            .handle_new_session("t_load".into(), "/tmp".into())
            .await
            .expect("new_session ok");
        let _resp = engine
            .handle_load_session("t_load".into())
            .await
            .expect("load ok");
    }

    #[tokio::test]
    async fn load_session_for_unknown_thread_errors() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        let err = engine
            .handle_load_session("nope".into())
            .await
            .expect_err("unknown thread errors");
        assert!(err.contains("no codex session for thread"));
    }

    #[tokio::test]
    async fn set_mode_model_effort_are_noops() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        engine
            .handle_set_mode("t".into(), "auto".into())
            .await
            .expect("set_mode no-op");
        engine
            .handle_set_model("t".into(), Some("gpt-5".into()))
            .await
            .expect("set_model no-op");
        engine
            .handle_set_effort("t".into(), crate::claude::session::EffortLevel::Off)
            .await
            .expect("set_effort no-op");
    }

    #[test]
    fn initialize_advertises_codex_capabilities() {
        let pty = Arc::new(PtyManager::new());
        let engine = CodexPtyEngine::new(pty);
        let req = InitializeRequest::new(ProtocolVersion::V1);
        let resp = engine.handle_initialize(req);
        assert_eq!(resp.protocol_version, ProtocolVersion::V1);
        // Codex via exec doesn't accept images through this surface.
        assert!(!resp.agent_capabilities.prompt_capabilities.image);
        assert!(resp.agent_capabilities.load_session);
    }
}
