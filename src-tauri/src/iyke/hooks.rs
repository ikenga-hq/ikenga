//! Claude Code hook installer & receiver handlers (WP-07).
//!
//! Auto-configures hook handlers in overlay settings.json, receives live event
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
