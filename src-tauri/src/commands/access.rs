//! G-ACCESS desktop commands (§9.1, §9.2, P-20).
//!
//! The desktop never opens the access store. Every command here is a thin
//! proxy: it POSTs `{cmd, args}` to the local daemon's `/api/rpc` with
//! `DaemonState`'s operator bearer, so the daemon serves it in-process
//! against `<app_data>/daemon/access.db`. The desktop window is the host,
//! so it is the T0 operator; Tauri `invoke` is not capability-checked
//! (§1.6).
//!
//! * On a connection failure the proxy re-runs `init_daemon` (find or spawn,
//!   `pty/daemon_client.rs`) once, then answers `store_unavailable` (§2.5,
//!   review M-3).
//! * In the ephemeral in-process fallback (no `ikenga-server` binary) there
//!   is no T0 store: every command answers `store_unavailable`.
//! * **`permission_decide` is the exception** (review C-05): the asks it
//!   decides live in this process and the desktop's own `ikenga.db`, so it
//!   is served in-process through the shared decide core
//!   (`server::shared::notifications::routing::decide`, WP-75).
//!
//! Args are forwarded exactly as the front end sent them (`src/lib/access/
//! client.ts`), so the browser transport and `invoke` carry one shape.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Manager};

use crate::pty::daemon_client::{DaemonInfo, DaemonState};

const STORE_UNAVAILABLE: &str =
    "store_unavailable: no local ikenga-server daemon is running, so there is no access store";

/// The invoke args object, as sent.
fn args_of(request: &Request<'_>) -> Value {
    match request.body() {
        InvokeBody::Json(v) => v.clone(),
        InvokeBody::Raw(_) => Value::Null,
    }
}

enum PostError {
    /// Nothing answered (the daemon idled out or died): worth one re-init.
    Connect,
    Other(String),
}

async fn post(info: &DaemonInfo, cmd: &str, args: &Value) -> Result<Value, PostError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| PostError::Other(format!("internal: {e}")))?;
    let res = client
        .post(format!("{}/api/rpc", info.http_url))
        .bearer_auth(&info.token)
        .header("content-type", "application/json")
        .body(json!({ "cmd": cmd, "args": args }).to_string())
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                PostError::Connect
            } else {
                PostError::Other(format!("internal: daemon request failed: {e}"))
            }
        })?;
    if res.status().as_u16() == 401 {
        return Err(PostError::Other(
            "unauthenticated: the local daemon refused the desktop's token".into(),
        ));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| PostError::Other(format!("internal: bad daemon reply: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| PostError::Other(format!("internal: bad daemon reply: {e}")))?;
    if body.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(PostError::Other(
            body.get("error")
                .and_then(Value::as_str)
                .unwrap_or("internal: the daemon answered without an error string")
                .to_string(),
        ))
    }
}

/// The one generic proxy.
async fn proxy(app: &AppHandle, cmd: &str, args: Value) -> Result<Value, String> {
    let Some(state) = app.try_state::<Arc<DaemonState>>() else {
        return Err(STORE_UNAVAILABLE.into());
    };
    let state: Arc<DaemonState> = state.inner().clone();
    let mut info = state.get_info();
    for attempt in 0..2 {
        if !info.available || info.mode != "persistent" {
            return Err(STORE_UNAVAILABLE.into());
        }
        match post(&info, cmd, &args).await {
            Ok(v) => return Ok(v),
            Err(PostError::Other(e)) => return Err(e),
            Err(PostError::Connect) if attempt == 0 => {
                // §2.5 (M-3): the daemon may have idled out; find or spawn it
                // once (blocking: it probes and may wait for readiness).
                let data_dir = app.path().app_data_dir().ok();
                let s = state.clone();
                info = tokio::task::spawn_blocking(move || s.reinit(data_dir))
                    .await
                    .map_err(|e| format!("internal: {e}"))?;
            }
            Err(PostError::Connect) => break,
        }
    }
    Err(STORE_UNAVAILABLE.into())
}

// ── devices (WP-74) ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn access_status(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_status", args_of(&request)).await
}

#[tauri::command]
pub async fn access_devices_list(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_devices_list", args_of(&request)).await
}

#[tauri::command]
pub async fn access_device_set_tier(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_device_set_tier", args_of(&request)).await
}

#[tauri::command]
pub async fn access_device_revoke(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_device_revoke", args_of(&request)).await
}

#[tauri::command]
pub async fn access_pair_begin(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_pair_begin", args_of(&request)).await
}

#[tauri::command]
pub async fn access_pair_cancel(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_pair_cancel", args_of(&request)).await
}

#[tauri::command]
pub async fn access_pair_pending(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_pair_pending", args_of(&request)).await
}

#[tauri::command]
pub async fn access_pair_decide(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_pair_decide", args_of(&request)).await
}

// ── permission routing (WP-75) ──────────────────────────────────────────

#[tauri::command]
pub async fn access_routing_get(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_routing_get", args_of(&request)).await
}

#[tauri::command]
pub async fn access_routing_set(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_routing_set", args_of(&request)).await
}

/// Served in-process, never proxied (review C-05): the desktop's own asks.
#[tauri::command]
pub async fn permission_decide(
    pa_db: tauri::State<'_, Arc<crate::commands::db::PaDb>>,
    request: Request<'_>,
) -> Result<Value, String> {
    let args = args_of(&request);
    crate::server::shared::notifications::routing::decide(
        Some(pa_db.inner().as_ref()),
        &crate::server::shared::notifications::routing::NoResolvers,
        None,
        &args,
    )
    .await
}

// ── access / members (WP-76) ────────────────────────────────────────────

#[tauri::command]
pub async fn access_members_list(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_members_list", args_of(&request)).await
}

#[tauri::command]
pub async fn access_member_set_role(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_member_set_role", args_of(&request)).await
}

#[tauri::command]
pub async fn access_member_remove(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_member_remove", args_of(&request)).await
}

#[tauri::command]
pub async fn access_member_restore(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_member_restore", args_of(&request)).await
}

#[tauri::command]
pub async fn access_policy_get(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_policy_get", args_of(&request)).await
}

#[tauri::command]
pub async fn access_policy_set_cell(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_policy_set_cell", args_of(&request)).await
}

#[tauri::command]
pub async fn access_policy_set_owner_approval(
    app: AppHandle,
    request: Request<'_>,
) -> Result<Value, String> {
    proxy(&app, "access_policy_set_owner_approval", args_of(&request)).await
}

#[tauri::command]
pub async fn access_invite_issue(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_invite_issue", args_of(&request)).await
}

#[tauri::command]
pub async fn access_invite_revoke(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_invite_revoke", args_of(&request)).await
}

#[tauri::command]
pub async fn access_shares_list(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_shares_list", args_of(&request)).await
}

// ── audit (WP-77) ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn access_audit_list(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_audit_list", args_of(&request)).await
}

#[tauri::command]
pub async fn access_audit_verify(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_audit_verify", args_of(&request)).await
}

#[tauri::command]
pub async fn access_audit_export(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_audit_export", args_of(&request)).await
}

#[tauri::command]
pub async fn access_audit_record_local(
    app: AppHandle,
    request: Request<'_>,
) -> Result<Value, String> {
    proxy(&app, "access_audit_record_local", args_of(&request)).await
}

#[tauri::command]
pub async fn access_audit_reseal(app: AppHandle, request: Request<'_>) -> Result<Value, String> {
    proxy(&app, "access_audit_reseal", args_of(&request)).await
}
