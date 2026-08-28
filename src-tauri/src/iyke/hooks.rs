//! Claude Code hook receiver handlers (WP-07).
//!
//! The hooks are installed by `iyke::hook_settings`, which builds the
//! `--settings` document Ikenga hands `claude` in the terminals it launches.
//! (It used to be an overlay `settings.json` written with a placeholder
//! `port: 0`, so nothing here ever received an event — see ikenga#149.)
//! Receives live event
//! callbacks (`PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`, `Notification`,
//! `PermissionRequest`, `UserPromptSubmit`, `PreCompact`) on `POST /iyke/hooks/event`,
//! updates host hook event log, and broadcasts events over `hooks://event`.

use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

static HOOK_EVENTS_STORE: OnceLock<Arc<Mutex<Vec<HookPayload>>>> = OnceLock::new();

fn get_events_store() -> &'static Arc<Mutex<Vec<HookPayload>>> {
    HOOK_EVENTS_STORE.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookPayload {
    #[serde(default, rename = "hook_event_name")]
    pub hook_event_name: Option<String>,
    #[serde(default, rename = "session_id")]
    pub session_id: Option<String>,
    #[serde(default, rename = "transcript_path")]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default, rename = "permission_mode")]
    pub permission_mode: Option<String>,
    #[serde(default, rename = "tool_name")]
    pub tool_name: Option<String>,
    #[serde(default, rename = "tool_input")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default, rename = "tool_output")]
    pub tool_output: Option<serde_json::Value>,
    #[serde(default, rename = "tool_use_id")]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default, rename = "sessionTitle")]
    pub session_title: Option<String>,
    #[serde(default, rename = "stopReason")]
    pub stop_reason: Option<String>,
}

/// Post route handler: POST /iyke/hooks/event
pub async fn post_hook_event(
    Extension(app): Extension<AppHandle>,
    Json(payload): Json<HookPayload>,
) -> impl IntoResponse {
    if let Ok(mut store) = get_events_store().lock() {
        store.push(payload.clone());
        if store.len() > 200 {
            store.remove(0);
        }
    }

    let _ = app.emit("hooks://event", &payload);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "continue": true
        })),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecision {
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// `approved` | `denied`.
    pub decision: String,
}

/// Post route handler: POST /iyke/hooks/decision
///
/// The permission inbox (`shell/src/terminal/permission-inbox.tsx`) posts the
/// operator's approve/deny here. RECORD-ONLY today, deliberately: the hook that
/// produced the request already returned `{"continue": true}` by the time a
/// human sees it, so this cannot retroactively gate the tool call. It exists so
/// the decision is captured and broadcast rather than dropped on the floor —
/// which is what it was doing, since this route did not exist at all and the
/// frontend's POST 404'd.
///
/// Turning this into a real gate means holding the `PreToolUse` response open
/// until a decision arrives (Claude Code honours `{"continue": false}` and a
/// permission decision in the hook's stdout), bounded by the hook timeout. That
/// is a separate design — see ikenga#149.
pub async fn post_hook_decision(
    Extension(app): Extension<AppHandle>,
    Json(decision): Json<HookDecision>,
) -> impl IntoResponse {
    let _ = app.emit("hooks://decision", &decision);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "recorded": true, "gated": false })),
    )
}

/// Get route handler: GET /iyke/hooks/events
pub async fn get_hook_events() -> impl IntoResponse {
    let events = get_events_store()
        .lock()
        .ok()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    (StatusCode::OK, Json(events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_payload_defensive_parse() {
        let json_str = r#"{
            "hook_event_name": "PreToolUse",
            "session_id": "sess-456",
            "tool_name": "Edit",
            "tool_input": { "file_path": "src/main.rs" }
        }"#;

        let payload: HookPayload = serde_json::from_str(json_str).expect("parse hook payload");
        assert_eq!(payload.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(payload.tool_name.as_deref(), Some("Edit"));
        assert_eq!(payload.tool_input.as_ref().unwrap()["file_path"], "src/main.rs");
    }
}
