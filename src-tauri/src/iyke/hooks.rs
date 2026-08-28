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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{extract::Query, http::StatusCode, response::IntoResponse, Extension, Json};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

use crate::commands::db::PaDb;
use crate::iyke::hook_settings::GATE_HOLD_SECS;

static HOOK_EVENTS_STORE: OnceLock<Arc<Mutex<Vec<HookPayload>>>> = OnceLock::new();

fn get_events_store() -> &'static Arc<Mutex<Vec<HookPayload>>> {
    HOOK_EVENTS_STORE.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookPayload {
    #[serde(default, rename = "ikenga_terminal_id")]
    pub ikenga_terminal_id: Option<String>,
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
    /// Backend-assigned request id for held `PreToolUse` gates. The frontend
    /// uses this id to post a decision back to `/iyke/hooks/decision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// True when this hook event is being held pending a human decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookEventQuery {
    pub terminal: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecision {
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// `approved` | `denied`.
    pub decision: String,
}

/// Pending decision for a held `PreToolUse` hook.
struct HeldRequest {
    tx: oneshot::Sender<HookDecision>,
}

static HELD_REQUESTS: OnceLock<Arc<Mutex<HashMap<String, HeldRequest>>>> = OnceLock::new();

fn get_held_requests() -> &'static Arc<Mutex<HashMap<String, HeldRequest>>> {
    HELD_REQUESTS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn hold_setting_key(terminal_id: &str) -> String {
    format!("permissions.hold_terminal_{terminal_id}")
}

async fn pre_tool_use_gate_enabled(app: &AppHandle, terminal_id: Option<&str>) -> bool {
    let Some(terminal_id) = terminal_id else {
        return false;
    };

    let db: Arc<PaDb> = match app.try_state::<Arc<PaDb>>() {
        Some(db) => db.inner().clone(),
        None => return false,
    };

    let pool = match db.ensure_pool().await {
        Ok(pool) => pool,
        Err(_) => return false,
    };

    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings_kv WHERE key = ?")
        .bind(hold_setting_key(terminal_id))
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();

    matches!(row.map(|r| r.0).as_deref(), Some("true") | Some("1"))
}

fn mint_request_id() -> String {
    format!(
        "perm-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        rand::random::<u32>()
    )
}

/// Post route handler: POST /iyke/hooks/event
pub async fn post_hook_event(
    Extension(app): Extension<AppHandle>,
    Query(query): Query<HookEventQuery>,
    Json(mut payload): Json<HookPayload>,
) -> impl IntoResponse {
    // The query parameter is authoritative: the settings file is generated
    // per-terminal and the URL it POSTs to carries `?terminal=<id>`.
    if payload.ikenga_terminal_id.is_none() {
        payload.ikenga_terminal_id = query.terminal.clone();
    }

    if let Ok(mut store) = get_events_store().lock() {
        store.push(payload.clone());
        if store.len() > 200 {
            store.remove(0);
        }
    }

    // `PreToolUse` gating is opt-in per terminal. Only hold when the
    // `permissions.hold_terminal_<id>` setting is on. Without it, every
    // `PreToolUse` immediately returns `continue: true` and the inbox only
    // records the event for audit.
    let should_gate = payload.hook_event_name.as_deref() == Some("PreToolUse")
        && pre_tool_use_gate_enabled(&app, payload.ikenga_terminal_id.as_deref()).await;

    if should_gate {
        let request_id = mint_request_id();
        payload.request_id = Some(request_id.clone());
        payload.held = Some(true);

        let (tx, rx) = oneshot::channel::<HookDecision>();
        {
            let held = HeldRequest { tx };
            if let Ok(mut map) = get_held_requests().lock() {
                map.insert(request_id.clone(), held);
            }
        }

        let _ = app.emit("hooks://event", &payload);

        // Hold the hook response open until the human decides, bounded by
        // `GATE_HOLD_SECS`. That bound is not arbitrary: it must stay strictly
        // below the `curl --max-time` and the Claude Code hook timeout that
        // `hook_settings` writes for this event, or curl gives up first, the
        // decision never reaches Claude Code, and the gate silently passes the
        // tool call through. See the nesting comment in `hook_settings`.
        let decision = timeout(Duration::from_secs(GATE_HOLD_SECS), rx).await;

        // Clean up the pending request regardless of outcome.
        let _ = get_held_requests()
            .lock()
            .map(|mut map| map.remove(&request_id));

        let allowed = match decision {
            Ok(Ok(HookDecision { decision, .. })) => decision == "approved",
            _ => {
                // Timeout or channel closed. Emit a synthetic timeout so the
                // permission inbox can mark the request as closed.
                let _ = app.emit(
                    "hooks://decision",
                    &HookDecision {
                        request_id: request_id.clone(),
                        decision: "denied".into(),
                    },
                );
                false
            }
        };

        // Deny is expressed as a `PreToolUse` permission decision, NOT as
        // `continue: false`. `continue: false` stops the entire Claude session
        // — denying one Bash call would end the conversation. The
        // `hookSpecificOutput.permissionDecision` field blocks just this tool
        // call and hands Claude a reason it can respond to.
        let (permission_decision, reason) = if allowed {
            ("allow", "Approved in the Ikenga permission inbox.")
        } else {
            (
                "deny",
                "Denied in the Ikenga permission inbox (or the request timed out).",
            )
        };

        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "continue": true,
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": permission_decision,
                    "permissionDecisionReason": reason,
                },
                "request_id": request_id,
                "gated": true,
            })),
        );
    }

    let _ = app.emit("hooks://event", &payload);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "continue": true
        })),
    )
}

/// Post route handler: POST /iyke/hooks/decision
///
/// The permission inbox posts the operator's approve/deny here. If the
/// `PreToolUse` hook was held, the decision is forwarded to the pending
/// receiver and the HTTP response is recorded. If no pending request exists,
/// the decision is still emitted as an event so callers can observe it.
pub async fn post_hook_decision(
    Extension(app): Extension<AppHandle>,
    Json(decision): Json<HookDecision>,
) -> impl IntoResponse {
    let was_gated = {
        if let Ok(mut map) = get_held_requests().lock() {
            if let Some(held) = map.remove(&decision.request_id) {
                // If the receiver is already gone (timeout), the send fails;
                // that's fine — we still record the decision.
                let _ = held.tx.send(decision.clone());
                true
            } else {
                false
            }
        } else {
            false
        }
    };

    let _ = app.emit("hooks://decision", &decision);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "recorded": true,
            "gated": was_gated,
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
        assert_eq!(
            payload.tool_input.as_ref().unwrap()["file_path"],
            "src/main.rs"
        );
    }

    #[test]
    fn held_requests_map_isolated() {
        let map = get_held_requests();
        let (tx, _rx) = oneshot::channel::<HookDecision>();
        map.lock()
            .unwrap()
            .insert("perm-123".into(), HeldRequest { tx });
        assert!(map.lock().unwrap().contains_key("perm-123"));
    }
}
