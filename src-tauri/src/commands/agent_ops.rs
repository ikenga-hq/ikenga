//! agent-ops host bridge (WP-09 / **G-TRIGGER**).
//!
//! The privileged hops the agent-ops iframe pkg cannot make itself:
//!   * `agent_ops_run_now`     — fire an out-of-schedule run on the always-on
//!                               cron daemon via its localhost trigger endpoint
//!                               (WP-06). Reads the 0600 `~/.agent-ops/daemon.lock`
//!                               for `{ port, secret }` and POSTs with BOTH
//!                               required headers.
//!   * `agent_ops_set_enabled` — flip a job's `enabled` flag in the
//!                               project-scoped config (atomic rewrite); the
//!                               daemon honors it on next config load.
//!   * `agent_ops_list_jobs`   — read the project-scoped config + the daemon's
//!                               runtime state file, merged per job, + liveness.
//!
//! The shell is **observability / management only — it never becomes the
//! executor.** run-now does nothing but POST the daemon's own endpoint; the
//! daemon stays the single thing that fires jobs. All three commands are gated
//! FE-side by `capabilities.agentOps` (`pkg-iframe-host.tsx`) since `host.*`
//! verbs bypass the kernel's scope enforcement.
//!
//! WP-19 slice 3: every command but run-now is a thin wrapper over
//! `server::shared::agent_ops`, which the headless daemon serves too, rooted
//! at `platform::home_dir()` here.

use serde_json::{json, Value};
use std::time::Duration;

use crate::server::shared::agent_ops::{self, err_value, read_daemon_lock};

// ─── run-now ─────────────────────────────────────────────────────────────────

/// POST the daemon's localhost trigger endpoint to fire an out-of-schedule run.
/// Always resolves `Ok(Value)` carrying a `{ ok, ... }` payload (incl. typed
/// `code` on failure) so the FE always has a structured result to render; only
/// a genuinely unexpected internal error returns `Err`.
#[tauri::command]
pub async fn agent_ops_run_now(job_id: String) -> Result<Value, String> {
    // SECURITY: the id becomes a path segment of a POST that carries the
    // daemon's secret. Validate + encode it before reading the lock or touching
    // the network, so a crafted id (`../x`, `a?b`, `%2e%2e`, …) can never steer
    // the secret-bearing request to another path.
    let segment = match agent_ops::trigger_path_segment(&job_id) {
        Ok(s) => s,
        Err(why) => return Ok(err_value("error", None, format!("invalid job id: {why}"))),
    };

    let lock = match read_daemon_lock(crate::platform::home_dir().as_deref()).await {
        Ok(l) => l,
        Err(e) => return Ok(err_value("daemon_down", None, e)),
    };

    let url = format!("http://127.0.0.1:{}/jobs/{}/trigger", lock.port, segment);
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        // Auth secret (timing-safe compared on the daemon).
        .header("x-agent-ops-token", &lock.secret)
        // Non-CORS-safelisted presence header — the daemon's DNS-rebinding
        // defense (rejects any request lacking it with 403). Value is ignored.
        .header("x-agent-ops-trigger", "1")
        .header("content-type", "application/json")
        .timeout(Duration::from_secs(10))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        // Connection refused / timeout → daemon not actually listening.
        Err(e) => {
            return Ok(err_value(
                "daemon_down",
                None,
                format!("trigger POST failed: {e}"),
            ))
        }
    };

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let message = parsed
        .get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| body.clone());

    if (200..300).contains(&status) {
        return Ok(json!({ "ok": true, "status": status, "message": message }));
    }
    let code = match status {
        401 => "unauthorized",
        403 | 405 => "forbidden",
        404 => "not_found",
        409 => "disabled",
        _ => "error",
    };
    Ok(err_value(code, Some(status), message))
}

// ─── config + tail (shared with the daemon) ──────────────────────────────────

/// Flip a job's `enabled` flag in the project-scoped config (atomic rewrite).
#[tauri::command]
pub async fn agent_ops_set_enabled(job_id: String, enabled: bool) -> Result<Value, String> {
    agent_ops::set_enabled(crate::platform::home_dir().as_deref(), job_id, enabled).await
}

/// Create-or-update a job in the project-scoped config. Config write only.
#[tauri::command]
pub async fn agent_ops_upsert_job(job: Value) -> Result<Value, String> {
    agent_ops::upsert_job(crate::platform::home_dir().as_deref(), job).await
}

/// Remove a job from the project-scoped config by id.
#[tauri::command]
pub async fn agent_ops_delete_job(job_id: String) -> Result<Value, String> {
    agent_ops::delete_job(crate::platform::home_dir().as_deref(), job_id).await
}

/// The project-scoped config merged with the daemon's state file, per job,
/// plus daemon liveness.
#[tauri::command]
pub async fn agent_ops_list_jobs() -> Result<Value, String> {
    agent_ops::list_jobs(crate::platform::home_dir().as_deref()).await
}

/// A job's live (or last) run output by byte range.
#[tauri::command]
pub async fn agent_ops_tail_run(job_id: String, offset: Option<u64>) -> Result<Value, String> {
    agent_ops::tail_run(crate::platform::home_dir().as_deref(), job_id, offset).await
}
