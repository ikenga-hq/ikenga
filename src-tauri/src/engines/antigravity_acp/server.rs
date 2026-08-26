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

/// State for a single Antigravity CLI session.
pub struct AntigravitySession {
    pub cwd: String,
    pub conversation_id: Option<String>,
    pub in_flight: Option<Arc<TokioMutex<Child>>>,
}

impl AntigravitySession {
    pub fn new(cwd: String) -> Self {
        Self {
            cwd,
            conversation_id: None,
            in_flight: None,
        }
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

    pub async fn register_session(&self, thread_id: String, cwd: String) -> Arc<TokioMutex<AntigravitySession>> {
        let cwd = if cwd.is_empty() {
            std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
        } else {
            cwd
        };
        let mut guard = self.sessions.lock().await;
        guard
            .entry(thread_id)
            .or_insert_with(|| Arc::new(TokioMutex::new(AntigravitySession::new(cwd))))
            .clone()
    }

    pub async fn handle_new_session(
        &self,
        _app: AppHandle,
        req: NewSessionRequest,
    ) -> Result<NewSessionResponse, String> {
        let thread_id = if let Some(meta) = &req.meta {
            meta.get("threadId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let thread_id = if thread_id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            thread_id
        };

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

    pub async fn run_prompt(
        &self,
        app: Option<&AppHandle>,
        thread_id: &str,
        text: &str,
        model: Option<&str>,
        on_update_cb: Option<&(dyn Fn(SessionUpdate) + Send + Sync)>,
    ) -> Result<PromptResponse, String> {
        let session_arc = self.register_session(thread_id.to_string(), String::new()).await;
        let (cwd, conv_id) = {
            let s = session_arc.lock().await;
            (s.cwd.clone(), s.conversation_id.clone())
        };

        let cmd_binary = which::which_in(DEFAULT_AGY_CMD, Some(crate::runtime::augmented_path()), ".")
            .or_else(|_| which::which_in("antigravity", Some(crate::runtime::augmented_path()), "."))
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
        if let Some(m) = model {
            cmd.arg("--model").arg(m);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PATH", crate::runtime::augmented_path())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| format!("spawn antigravity CLI: {e}"))?;

        if let Some(stderr) = child.stderr.take() {
            let tid = thread_id.to_string();
            tauri::async_runtime::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!(target: "ikenga::engines::antigravity_acp", "antigravity[{tid}] stderr: {line}");
                }
            });
        }

        let stdout = child.stdout.take().ok_or_else(|| "antigravity stdout not piped".to_string())?;
        let child_handle = Arc::new(TokioMutex::new(child));
        {
            let mut s = session_arc.lock().await;
            s.in_flight = Some(child_handle.clone());
        }

        let channel = format!("chat://session/{thread_id}/antigravity");
        let mut lines = BufReader::new(stdout).lines();
        let mut stop_reason = StopReason::EndTurn;

        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(event) = val.get("event").and_then(|e| e.as_str()) {
                    match event {
                        "init" => {
                            if let Some(cid) = val.get("conversation_id").and_then(|id| id.as_str()) {
                                let mut s = session_arc.lock().await;
                                if s.conversation_id.is_none() && !cid.is_empty() {
                                    s.conversation_id = Some(cid.to_string());
                                }
                            }
                        }
                        "step_update" => {
                            if let Some(step_update) = val.get("step_update") {
                                let step_type = step_update
                                    .get("step_type")
                                    .and_then(|t| t.as_str())
                                    .unwrap_or("");
                                let text_delta = step_update
                                    .get("text_delta")
                                    .and_then(|d| d.as_str());

                                if let Some(delta) = text_delta {
                                    if !delta.is_empty() {
                                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(delta)));
                                        let update = if step_type == "thought" {
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
                                }
                            }
                        }
                        "result" => {
                            if let Some(result) = val.get("result") {
                                if let Some(status) = result.get("status").and_then(|s| s.as_str()) {
                                    if status != "SUCCESS" {
                                        stop_reason = StopReason::Refusal;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        {
            let mut s = session_arc.lock().await;
            s.in_flight = None;
        }

        let mut child_guard = child_handle.lock().await;
        let _ = child_guard.wait().await;

        Ok(PromptResponse::new(stop_reason))
    }

    pub async fn handle_cancel(&self, thread_id: String) -> Result<(), String> {
        let session_arc = {
            let guard = self.sessions.lock().await;
            guard.get(&thread_id).cloned()
        };
        if let Some(session) = session_arc {
            let mut s = session.lock().await;
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

    #[test]
    fn parses_ndjson_deltas_correctly() {
        let line = r#"{"event":"step_update","step_update":{"conversation_id":"c1","step_index":2,"state":"DONE","step_type":"agent_response","text_delta":"Hello world\n"}}"#;
        let val: serde_json::Value = serde_json::from_str(line).unwrap();
        let event = val.get("event").and_then(|e| e.as_str()).unwrap();
        assert_eq!(event, "step_update");
        let step_update = val.get("step_update").unwrap();
        let delta = step_update.get("text_delta").and_then(|d| d.as_str()).unwrap();
        assert_eq!(delta, "Hello world\n");
    }
}

