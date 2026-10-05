//! G-ACCESS §9.1 — the desktop's `access_*` Tauri commands (WP-74a,
//! skeleton-first §9.2).
//!
//! The desktop window is the host, i.e. the T0 **operator**: it never opens
//! the access store (P-20, A-32). Every command here is a thin proxy that
//! POSTs `{cmd, args}` to the local daemon's `/api/rpc` with `DaemonState`'s
//! bearer, so the daemon — the store's one opener — serves it and closes
//! revoked devices' sockets in-process. On a connection failure the proxy
//! re-runs `init_daemon` (find or spawn) once, then answers
//! `store_unavailable` (§2.5, review M-3). In the ephemeral in-process
//! fallback (no `ikenga-server` binary) there is no T0 store: every command
//! answers `store_unavailable`.
//!
//! **Exception:** `permission_decide` is served in-process against the
//! desktop's own asks and `ikenga.db` (§5.5, review C-05) — WP-75 fills
//! `server::shared::notifications::routing::decide_local`.
//!
//! Argument names are the §9.1 TS shapes (camelCase on the wire; Tauri maps
//! them onto these snake_case parameters).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tauri::{AppHandle, Manager, State};

use crate::pty::daemon_client::{DaemonInfo, DaemonState};

type Daemon<'a> = State<'a, Arc<DaemonState>>;

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            // Loopback only: never through an HTTP(S)_PROXY.
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

const STORE_UNAVAILABLE: &str =
    "store_unavailable: the local ikenga-server daemon is not running, so there is no access store";

enum Sent {
    Answer(Result<Value, String>),
    /// Nothing listened: worth one `init_daemon` re-run.
    Unreachable,
}

async fn post(info: &DaemonInfo, cmd: &str, args: &Value) -> Sent {
    let res = client()
        .post(format!("{}/api/rpc", info.http_url))
        .bearer_auth(&info.token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(json!({ "cmd": cmd, "args": args }).to_string())
        .send()
        .await;
    let res = match res {
        Ok(r) => r,
        Err(e) if e.is_connect() => return Sent::Unreachable,
        Err(e) => return Sent::Answer(Err(format!("internal: daemon request failed: {e}"))),
    };
    if res.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Sent::Answer(Err(
            "unauthenticated: the daemon refused this app's token".into()
        ));
    }
    let body = res
        .bytes()
        .await
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()));
    match body {
        Ok(body) if body.get("ok").and_then(Value::as_bool) == Some(true) => {
            Sent::Answer(Ok(body.get("data").cloned().unwrap_or(Value::Null)))
        }
        Ok(body) => Sent::Answer(Err(body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("internal: malformed daemon reply")
            .to_string())),
        Err(e) => Sent::Answer(Err(format!("internal: malformed daemon reply: {e}"))),
    }
}

/// The one generic proxy (P-20). `pub(crate)` for the desktop's permission
/// routing (WP-75: the relay task and the host's routing / audit calls).
pub(crate) async fn proxy(
    app: &AppHandle,
    daemon: &DaemonState,
    cmd: &str,
    args: Value,
) -> Result<Value, String> {
    let info = daemon.get_info();
    if !info.available || info.mode != "persistent" {
        return Err(STORE_UNAVAILABLE.into());
    }
    match post(&info, cmd, &args).await {
        Sent::Answer(r) => return r,
        Sent::Unreachable => {}
    }
    // §2.5 (2): the daemon idled out or died — find or spawn it once.
    let app_data_dir = app.path().app_data_dir().ok();
    let state = app.state::<Arc<DaemonState>>().inner().clone();
    let fresh = tauri::async_runtime::spawn_blocking(move || state.reconnect(app_data_dir))
        .await
        .map_err(|e| format!("internal: {e}"))?;
    if !fresh.available || fresh.mode != "persistent" {
        return Err(STORE_UNAVAILABLE.into());
    }
    match post(&fresh, cmd, &args).await {
        Sent::Answer(r) => r,
        Sent::Unreachable => Err(STORE_UNAVAILABLE.into()),
    }
}

/// `{key: value}` without the `None`s.
fn args(pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

fn s(v: impl Into<String>) -> Option<Value> {
    Some(Value::String(v.into()))
}

fn opt<T: serde::Serialize>(v: Option<T>) -> Option<Value> {
    v.and_then(|v| serde_json::to_value(v).ok())
}

// ── devices (WP-74) ────────────────────────────────────────────────────────

#[tauri::command]
pub async fn access_status(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_status", json!({})).await
}

#[tauri::command]
pub async fn access_devices_list(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_devices_list", json!({})).await
}

#[tauri::command]
pub async fn access_device_set_tier(
    app: AppHandle,
    daemon: Daemon<'_>,
    device_id: String,
    tier: String,
) -> Result<Value, String> {
    let a = args(vec![("deviceId", s(device_id)), ("tier", s(tier))]);
    proxy(&app, &daemon, "access_device_set_tier", a).await
}

#[tauri::command]
pub async fn access_device_revoke(
    app: AppHandle,
    daemon: Daemon<'_>,
    device_id: String,
) -> Result<Value, String> {
    let a = args(vec![("deviceId", s(device_id))]);
    proxy(&app, &daemon, "access_device_revoke", a).await
}

#[tauri::command]
pub async fn access_pair_begin(
    app: AppHandle,
    daemon: Daemon<'_>,
    public_base: Option<String>,
) -> Result<Value, String> {
    let a = args(vec![("publicBase", opt(public_base))]);
    proxy(&app, &daemon, "access_pair_begin", a).await
}

#[tauri::command]
pub async fn access_pair_cancel(
    app: AppHandle,
    daemon: Daemon<'_>,
    pairing_id: String,
) -> Result<Value, String> {
    let a = args(vec![("pairingId", s(pairing_id))]);
    proxy(&app, &daemon, "access_pair_cancel", a).await
}

#[tauri::command]
pub async fn access_pair_pending(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_pair_pending", json!({})).await
}

#[tauri::command]
pub async fn access_pair_decide(
    app: AppHandle,
    daemon: Daemon<'_>,
    pairing_id: String,
    decision: String,
    tier: Option<String>,
) -> Result<Value, String> {
    let a = args(vec![
        ("pairingId", s(pairing_id)),
        ("decision", s(decision)),
        ("tier", opt(tier)),
    ]);
    proxy(&app, &daemon, "access_pair_decide", a).await
}

// ── access / members (WP-76) ───────────────────────────────────────────────

#[tauri::command]
pub async fn access_members_list(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
) -> Result<Value, String> {
    let a = args(vec![("projectId", s(project_id))]);
    proxy(&app, &daemon, "access_members_list", a).await
}

#[tauri::command]
pub async fn access_member_set_role(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    principal_id: String,
    role: String,
    artifact_path: Option<String>,
    expires_at: Option<i64>,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("principalId", s(principal_id)),
        ("role", s(role)),
        ("artifactPath", opt(artifact_path)),
        ("expiresAt", opt(expires_at)),
    ]);
    proxy(&app, &daemon, "access_member_set_role", a).await
}

#[tauri::command]
pub async fn access_member_remove(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    principal_id: String,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("principalId", s(principal_id)),
    ]);
    proxy(&app, &daemon, "access_member_remove", a).await
}

#[tauri::command]
pub async fn access_member_restore(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    principal_id: String,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("principalId", s(principal_id)),
    ]);
    proxy(&app, &daemon, "access_member_restore", a).await
}

#[tauri::command]
pub async fn access_policy_get(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
) -> Result<Value, String> {
    let a = args(vec![("projectId", s(project_id))]);
    proxy(&app, &daemon, "access_policy_get", a).await
}

#[tauri::command]
pub async fn access_policy_set_cell(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    role: String,
    cap: String,
    allowed: bool,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("role", s(role)),
        ("cap", s(cap)),
        ("allowed", Some(Value::Bool(allowed))),
    ]);
    proxy(&app, &daemon, "access_policy_set_cell", a).await
}

#[tauri::command]
pub async fn access_policy_set_owner_approval(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    required: bool,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("required", Some(Value::Bool(required))),
    ]);
    proxy(&app, &daemon, "access_policy_set_owner_approval", a).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn access_invite_issue(
    app: AppHandle,
    daemon: Daemon<'_>,
    project_id: String,
    mode: String,
    invitee_label: Option<String>,
    role: String,
    scope: String,
    artifact_path: Option<String>,
    member_expires_at: Option<i64>,
) -> Result<Value, String> {
    let a = args(vec![
        ("projectId", s(project_id)),
        ("mode", s(mode)),
        ("inviteeLabel", opt(invitee_label)),
        ("role", s(role)),
        ("scope", s(scope)),
        ("artifactPath", opt(artifact_path)),
        ("memberExpiresAt", opt(member_expires_at)),
    ]);
    proxy(&app, &daemon, "access_invite_issue", a).await
}

#[tauri::command]
pub async fn access_invite_revoke(
    app: AppHandle,
    daemon: Daemon<'_>,
    invite_id: String,
) -> Result<Value, String> {
    let a = args(vec![("inviteId", s(invite_id))]);
    proxy(&app, &daemon, "access_invite_revoke", a).await
}

#[tauri::command]
pub async fn access_shares_list(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_shares_list", json!({})).await
}

// ── permission routing (WP-75) ─────────────────────────────────────────────

#[tauri::command]
pub async fn access_routing_get(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_routing_get", json!({})).await
}

#[tauri::command]
pub async fn access_routing_set(
    app: AppHandle,
    daemon: Daemon<'_>,
    mode: String,
    device_id: Option<String>,
) -> Result<Value, String> {
    let a = args(vec![("mode", s(mode)), ("deviceId", opt(device_id))]);
    proxy(&app, &daemon, "access_routing_set", a).await
}

/// Served **in-process**, not proxied (§5.5, review C-05): the asks it
/// decides live in the desktop process and the desktop's own `ikenga.db`.
#[tauri::command]
pub async fn permission_decide(notification_id: i64, decision: String) -> Result<Value, String> {
    crate::server::shared::notifications::routing::decide_local(notification_id, &decision)
        .await
        .map_err(|e| e.to_string())
}

// ── audit (WP-77) ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn access_audit_list(
    app: AppHandle,
    daemon: Daemon<'_>,
    filter: Option<Value>,
    before: Option<i64>,
    limit: Option<i64>,
) -> Result<Value, String> {
    let a = args(vec![
        ("filter", Some(filter.unwrap_or_else(|| json!({})))),
        ("before", opt(before)),
        ("limit", opt(limit)),
    ]);
    proxy(&app, &daemon, "access_audit_list", a).await
}

#[tauri::command]
pub async fn access_audit_verify(app: AppHandle, daemon: Daemon<'_>) -> Result<Value, String> {
    proxy(&app, &daemon, "access_audit_verify", json!({})).await
}

#[tauri::command]
pub async fn access_audit_export(
    app: AppHandle,
    daemon: Daemon<'_>,
    filter: Option<Value>,
    dest_path: Option<String>,
) -> Result<Value, String> {
    let a = args(vec![
        ("filter", Some(filter.unwrap_or_else(|| json!({})))),
        ("destPath", opt(dest_path)),
    ]);
    proxy(&app, &daemon, "access_audit_export", a).await
}

#[tauri::command]
pub async fn access_audit_record_local(
    app: AppHandle,
    daemon: Daemon<'_>,
    kind: String,
    target: String,
    detail: Option<Value>,
) -> Result<Value, String> {
    let a = args(vec![
        ("kind", s(kind)),
        ("target", s(target)),
        ("detail", detail),
    ]);
    proxy(&app, &daemon, "access_audit_record_local", a).await
}

#[tauri::command]
pub async fn access_audit_reseal(
    app: AppHandle,
    daemon: Daemon<'_>,
    ack_seq: i64,
) -> Result<Value, String> {
    let a = args(vec![("ackSeq", Some(json!(ack_seq)))]);
    proxy(&app, &daemon, "access_audit_reseal", a).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_drop_absent_optionals_and_keep_camel_case() {
        let a = args(vec![
            ("deviceId", s("d")),
            ("tier", opt(None::<String>)),
            ("expiresAt", opt(Some(5i64))),
        ]);
        assert_eq!(a, json!({"deviceId": "d", "expiresAt": 5}));
    }
}
