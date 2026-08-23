//! Claude Code hook installer & receiver handlers (WP-07).
//!
//! Auto-configures hook handlers in overlay settings.json, receives live event
//! callbacks (`PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`, `Notification`,
//! `PermissionRequest`, `UserPromptSubmit`, `PreCompact`) on `POST /iyke/hooks/event`,
//! updates host hook event log, and broadcasts events over `hooks://event`.

use std::path::Path;
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

/// Configures hook handlers in an overlay settings.json file.
pub fn configure_overlay_hooks(overlay_dir: &Path, port: u16, token: Option<&str>) -> std::io::Result<()> {
    let settings_path = overlay_dir.join("settings.json");
    let mut root: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|_| "{}".to_string());
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let auth_header = match token {
        Some(t) => format!("-H 'Authorization: Bearer {}' ", t),
        None => String::new(),
    };

    let hook_cmd = format!(
        "curl -s -X POST {} -H 'Content-Type: application/json' --data-binary @- http://127.0.0.1:{}/iyke/hooks/event",
        auth_header, port
    );

    let default_hook_block = serde_json::json!([
        {
            "type": "command",
            "command": hook_cmd
        }
    ]);

    let matched_hook_block = serde_json::json!([
        {
            "matcher": "*",
            "hooks": default_hook_block
        }
    ]);

    let hooks_map = serde_json::json!({
        "PreToolUse": matched_hook_block,
        "PostToolUse": matched_hook_block,
        "SessionStart": serde_json::json!([{"hooks": default_hook_block}]),
        "SessionEnd": serde_json::json!([{"hooks": default_hook_block}]),
        "UserPromptSubmit": serde_json::json!([{"hooks": default_hook_block}]),
        "Notification": serde_json::json!([{"hooks": default_hook_block}]),
        "PermissionRequest": serde_json::json!([{"hooks": default_hook_block}])
    });

    if let Some(obj) = root.as_object_mut() {
        obj.insert("hooks".to_string(), hooks_map);
    }

    let serialized = serde_json::to_string_pretty(&root)?;
    std::fs::write(settings_path, serialized)
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
