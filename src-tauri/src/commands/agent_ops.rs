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
//! at `platform::home_dir()` here. Slice 6 moved run-now's body there too
//! (`agent_ops::run_now`), so the daemon's approve-gate arms can wake the
//! mutation worker; the `agent_ops_run_now` verb itself stays desktop-only.

use serde_json::Value;

use crate::server::shared::agent_ops;

// ─── run-now ─────────────────────────────────────────────────────────────────

/// POST the daemon's localhost trigger endpoint to fire an out-of-schedule run.
/// Always resolves `Ok(Value)` carrying a `{ ok, ... }` payload (incl. typed
/// `code` on failure) so the FE always has a structured result to render; only
/// a genuinely unexpected internal error returns `Err`.
#[tauri::command]
pub async fn agent_ops_run_now(job_id: String) -> Result<Value, String> {
    agent_ops::run_now(crate::platform::home_dir().as_deref(), &job_id).await
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
    agent_ops::upsert_job(
        crate::platform::home_dir().as_deref(),
        job,
        agent_ops::MissingConfig::Error,
    )
    .await
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
    agent_ops::list_jobs(
        crate::platform::home_dir().as_deref(),
        agent_ops::MissingConfig::Error,
    )
    .await
}

/// A job's live (or last) run output by byte range.
#[tauri::command]
pub async fn agent_ops_tail_run(job_id: String, offset: Option<u64>) -> Result<Value, String> {
    agent_ops::tail_run(crate::platform::home_dir().as_deref(), job_id, offset).await
}
