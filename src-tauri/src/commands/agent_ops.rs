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

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::time::Duration;

// ─── path resolution ─────────────────────────────────────────────────────────

fn home() -> Result<PathBuf, String> {
    // $HOME is unset on Windows; route through the platform resolver.
    crate::platform::home_dir().ok_or_else(|| "home directory not found".to_string())
}

fn daemon_lock_path() -> Result<PathBuf, String> {
    Ok(home()?.join(".agent-ops/daemon.lock"))
}

/// Per-run tail directory — sibling of `daemon.lock`. The daemon's script-mode
/// executor tees combined stdout/stderr here; this command reads it back by
/// byte-range. Created mode 0o700 by the daemon's boot sweep (run-tail.ts).
fn runs_dir() -> Result<PathBuf, String> {
    Ok(home()?.join(".agent-ops/runs"))
}

/// Slugify a job id for on-disk file names. **MUST stay identical to `slug()`
/// in the daemon's `lib/run-tail.ts`** — both sides derive the same marker /
/// tail file names from the job id, so any divergence silently breaks the read.
fn run_slug(job_id: &str) -> String {
    job_id.replace(':', "-")
}

/// Project-scoped job config the skill + daemon read new-wins. Under $HOME.
fn project_config_path() -> Result<PathBuf, String> {
    Ok(home()?.join(".atelier/skill-agent-ops/jobs.json"))
}

/// The daemon's runtime state file (`nextRunAtMs`, last-status, totals). Lives
/// in the royalti-co monorepo working tree, not under the app data dir. Resolve
/// it the way the daemon does, with overrides for non-default checkouts:
///   1. `AGENT_OPS_STATE_PATH` — full path to jobs-state.json
///   2. `AGENT_OPS_REPO_ROOT`  — `<root>/.company/cron/jobs-state.json`
///   3. default `$HOME/royalti-co/.company/cron/jobs-state.json`
/// Missing file is not an error — listJobs degrades to config-only (no
/// next-fire / state), which the pkg renders honestly.
fn jobs_state_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("AGENT_OPS_STATE_PATH") {
        return Ok(PathBuf::from(p));
    }
    if let Some(root) = std::env::var_os("AGENT_OPS_REPO_ROOT") {
        return Ok(PathBuf::from(root).join(".company/cron/jobs-state.json"));
    }
    Ok(home()?.join("royalti-co/.company/cron/jobs-state.json"))
}

// ─── daemon.lock ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DaemonLock {
    pid: u32,
    port: u16,
    secret: String,
}

/// Read + parse the daemon lock. `None` (with a reason) means the daemon is not
/// running / the lock is absent or malformed — callers map this to
/// `code: "daemon_down"`.
async fn read_daemon_lock() -> Result<DaemonLock, String> {
    let path = daemon_lock_path()?;
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|e| format!("read daemon.lock: {e}"))?;
    serde_json::from_slice::<DaemonLock>(&raw).map_err(|e| format!("parse daemon.lock: {e}"))
}

/// Best-effort liveness: on Linux a live pid has a `/proc/<pid>` entry. On other
/// platforms, treat lock-present as up (the systemd supervisor keeps it alive).
fn pid_alive(pid: u32) -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = pid;
        true
    }
}

fn err_value(code: &str, status: Option<u16>, error: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "code": code,
        "status": status,
        "error": error.into(),
    })
}

// ─── run-now ─────────────────────────────────────────────────────────────────

/// POST the daemon's localhost trigger endpoint to fire an out-of-schedule run.
/// Always resolves `Ok(Value)` carrying a `{ ok, ... }` payload (incl. typed
/// `code` on failure) so the FE always has a structured result to render; only
/// a genuinely unexpected internal error returns `Err`.
#[tauri::command]
pub async fn agent_ops_run_now(job_id: String) -> Result<Value, String> {
    let lock = match read_daemon_lock().await {
        Ok(l) => l,
        Err(e) => return Ok(err_value("daemon_down", None, e)),
    };

    let url = format!("http://127.0.0.1:{}/jobs/{}/trigger", lock.port, job_id);
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
        Err(e) => return Ok(err_value("daemon_down", None, format!("trigger POST failed: {e}"))),
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

// ─── set-enabled ─────────────────────────────────────────────────────────────

/// Flip a job's `enabled` flag in the project-scoped config. Atomic rewrite
/// (temp + rename on the same fs) so the live daemon never reads a torn file.
/// Preserves the file's array-vs-`{jobs:[]}` shape and pretty formatting.
#[tauri::command]
pub async fn agent_ops_set_enabled(job_id: String, enabled: bool) -> Result<Value, String> {
    let path = match project_config_path() {
        Ok(p) => p,
        Err(e) => return Ok(err_value("io_error", None, e)),
    };
    let raw = match tokio::fs::read(&path).await {
        Ok(r) => r,
        Err(e) => return Ok(err_value("io_error", None, format!("read config: {e}"))),
    };
    let mut root: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => return Ok(err_value("io_error", None, format!("parse config: {e}"))),
    };

    // The jobs array may be the document root or under a `jobs` key.
    let found = {
        let arr: Option<&mut Vec<Value>> = if root.is_array() {
            root.as_array_mut()
        } else {
            root.get_mut("jobs").and_then(|j| j.as_array_mut())
        };
        let Some(arr) = arr else {
            return Ok(err_value("io_error", None, "config is not a jobs array"));
        };
        let mut hit = false;
        for job in arr.iter_mut() {
            if job.get("id").and_then(|v| v.as_str()) == Some(job_id.as_str()) {
                if let Some(obj) = job.as_object_mut() {
                    obj.insert("enabled".into(), Value::Bool(enabled));
                    hit = true;
                }
            }
        }
        hit
    };
    if !found {
        return Ok(err_value("not_found", None, format!("no job \"{job_id}\" in config")));
    }

    let serialized = match serde_json::to_string_pretty(&root) {
        Ok(s) => s + "\n",
        Err(e) => return Ok(err_value("io_error", None, format!("serialize config: {e}"))),
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = tokio::fs::write(&tmp, serialized.as_bytes()).await {
        return Ok(err_value("io_error", None, format!("write temp config: {e}")));
    }
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Ok(err_value("io_error", None, format!("rename config: {e}")));
    }

    Ok(json!({ "ok": true, "jobId": job_id, "enabled": enabled }))
}

// ─── create / edit / delete (WP-14) ──────────────────────────────────────────

/// Read + parse the project config root, returning `(root, err)`. On failure
/// returns the err_value to bubble straight back to the FE.
async fn read_config_root() -> Result<Value, Value> {
    let path = project_config_path().map_err(|e| err_value("io_error", None, e))?;
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|e| err_value("io_error", None, format!("read config: {e}")))?;
    serde_json::from_slice(&raw).map_err(|e| err_value("io_error", None, format!("parse config: {e}")))
}

/// Atomic write of the config root back to disk (temp + rename, same fs) so the
/// live daemon never reads a torn file.
async fn write_config_root(root: &Value) -> Result<(), Value> {
    let path = project_config_path().map_err(|e| err_value("io_error", None, e))?;
    let serialized = serde_json::to_string_pretty(root)
        .map(|s| s + "\n")
        .map_err(|e| err_value("io_error", None, format!("serialize config: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serialized.as_bytes())
        .await
        .map_err(|e| err_value("io_error", None, format!("write temp config: {e}")))?;
    tokio::fs::rename(&tmp, &path).await.map_err(|e| {
        err_value("io_error", None, format!("rename config: {e}"))
    })?;
    Ok(())
}

/// Borrow the jobs array out of the config root (array root, or `{ jobs: [...] }`).
fn jobs_array_mut(root: &mut Value) -> Option<&mut Vec<Value>> {
    if root.is_array() {
        root.as_array_mut()
    } else {
        root.get_mut("jobs").and_then(|j| j.as_array_mut())
    }
}

fn nonempty_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty())
}

/// Build a full JobDefinition from the form's `AgentOpsJobInput`, filling the
/// behavior-preserving defaults the daemon expects (G-11: concurrency `allow`).
/// Returns the validated id + the complete job object, or an err_value.
fn build_job(input: &Value) -> Result<(String, Value), Value> {
    let id = nonempty_str(input, "id")
        .ok_or_else(|| err_value("error", None, "job.id is required"))?
        .to_string();
    let label = nonempty_str(input, "label")
        .ok_or_else(|| err_value("error", None, "job.label is required"))?;
    let schedule = nonempty_str(input, "schedule")
        .ok_or_else(|| err_value("error", None, "job.schedule is required"))?;
    let command = nonempty_str(input, "command")
        .ok_or_else(|| err_value("error", None, "job.command is required"))?;

    let mode = match input.get("mode").and_then(|m| m.as_str()) {
        Some("script") => "script",
        _ => "agent",
    };
    // Default dialect from field count unless explicitly provided.
    let dialect = match input.get("schedule_dialect").and_then(|d| d.as_str()) {
        Some("6f") => "6f",
        Some("5f") => "5f",
        _ => {
            if schedule.split_whitespace().count() >= 6 { "6f" } else { "5f" }
        }
    };
    let timezone = nonempty_str(input, "timezone").unwrap_or("Africa/Lagos");
    let enabled = input.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
    let timeout_ms = input.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(300_000);
    let model = input.get("model").and_then(|m| m.as_str());
    let agent = input.get("agent").and_then(|a| a.as_str());

    let mut job = serde_json::Map::new();
    job.insert("id".into(), json!(id));
    job.insert("label".into(), json!(label));
    job.insert("schedule".into(), json!(schedule));
    job.insert("schedule_dialect".into(), json!(dialect));
    job.insert("timezone".into(), json!(timezone));
    job.insert("enabled".into(), json!(enabled));
    job.insert("command".into(), json!(command));
    job.insert("mode".into(), json!(mode));
    job.insert("timeout_ms".into(), json!(timeout_ms));
    job.insert("retries".into(), json!(0));
    job.insert("backoff".into(), json!({ "type": "fixed", "delay_ms": 0 }));
    job.insert("concurrency_policy".into(), json!("allow"));
    if let Some(m) = model {
        job.insert("model".into(), json!(m));
    }
    if let Some(a) = agent {
        job.insert("agent".into(), json!(a));
    }
    Ok((id, Value::Object(job)))
}

/// Create-or-update a job in the project-scoped config. Replace by id if present
/// (preserving the daemon-written fields the on-disk entry already had where the
/// form doesn't override them is NOT attempted — the form owns the definition),
/// else append. Atomic write; daemon honors on next load. The shell never runs
/// the job — this is a config write only.
#[tauri::command]
pub async fn agent_ops_upsert_job(job: Value) -> Result<Value, String> {
    let (id, full) = match build_job(&job) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let mut root = match read_config_root().await {
        Ok(r) => r,
        Err(e) => return Ok(e),
    };
    let Some(arr) = jobs_array_mut(&mut root) else {
        return Ok(err_value("io_error", None, "config is not a jobs array"));
    };
    let mut created = true;
    for slot in arr.iter_mut() {
        if slot.get("id").and_then(|v| v.as_str()) == Some(id.as_str()) {
            *slot = full.clone();
            created = false;
            break;
        }
    }
    if created {
        arr.push(full);
    }
    if let Err(e) = write_config_root(&root).await {
        return Ok(e);
    }
    Ok(json!({ "ok": true, "jobId": id, "created": created }))
}

/// Remove a job from the project-scoped config by id. Atomic write; the daemon
/// stops scheduling it on next load.
#[tauri::command]
pub async fn agent_ops_delete_job(job_id: String) -> Result<Value, String> {
    let mut root = match read_config_root().await {
        Ok(r) => r,
        Err(e) => return Ok(e),
    };
    let Some(arr) = jobs_array_mut(&mut root) else {
        return Ok(err_value("io_error", None, "config is not a jobs array"));
    };
    let before = arr.len();
    arr.retain(|j| j.get("id").and_then(|v| v.as_str()) != Some(job_id.as_str()));
    if arr.len() == before {
        return Ok(err_value("not_found", None, format!("no job \"{job_id}\" in config")));
    }
    if let Err(e) = write_config_root(&root).await {
        return Ok(e);
    }
    Ok(json!({ "ok": true, "jobId": job_id }))
}

// ─── list-jobs ───────────────────────────────────────────────────────────────

fn jobs_array_from(root: Value) -> Vec<Value> {
    match root {
        Value::Array(a) => a,
        Value::Object(mut o) => match o.remove("jobs") {
            Some(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Read the project-scoped config + the daemon state file and return both,
/// merged per job, plus daemon liveness. Run history is NOT included (the pkg
/// reads cron_job_runs / agent_runs directly via host.dbQuery).
#[tauri::command]
pub async fn agent_ops_list_jobs() -> Result<Value, String> {
    // Config (required for the job list).
    let cfg_path = match project_config_path() {
        Ok(p) => p,
        Err(e) => return Ok(err_value("io_error", None, e)),
    };
    let cfg_raw = match tokio::fs::read(&cfg_path).await {
        Ok(r) => r,
        Err(e) => return Ok(err_value("io_error", None, format!("read config: {e}"))),
    };
    let cfg_root: Value = match serde_json::from_slice(&cfg_raw) {
        Ok(v) => v,
        Err(e) => return Ok(err_value("io_error", None, format!("parse config: {e}"))),
    };
    let cfg_jobs = jobs_array_from(cfg_root);

    // State (optional — missing degrades to config-only).
    let state_map: Map<String, Value> = match jobs_state_path() {
        Ok(p) => match tokio::fs::read(&p).await {
            Ok(r) => serde_json::from_slice::<Map<String, Value>>(&r).unwrap_or_default(),
            Err(_) => Map::new(),
        },
        Err(_) => Map::new(),
    };

    // Daemon liveness from the lock.
    let (daemon_up, daemon_pid) = match read_daemon_lock().await {
        Ok(l) => (pid_alive(l.pid), Some(l.pid)),
        Err(_) => (false, None),
    };

    let mut jobs: Vec<Value> = Vec::with_capacity(cfg_jobs.len());
    for job in cfg_jobs {
        let Some(obj) = job.as_object() else { continue };
        let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let state = state_map.get(&id).cloned().unwrap_or(Value::Null);
        let s = |k: &str| obj.get(k).and_then(|v| v.as_str()).map(str::to_string);
        jobs.push(json!({
            "id": id,
            "label": s("label").unwrap_or_default(),
            "schedule": s("schedule").unwrap_or_default(),
            "schedule_dialect": s("schedule_dialect").unwrap_or_else(|| "5f".into()),
            "timezone": s("timezone").unwrap_or_else(|| "UTC".into()),
            "enabled": obj.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            "command": s("command").unwrap_or_default(),
            "mode": s("mode").unwrap_or_else(|| "agent".into()),
            "model": obj.get("model").and_then(|v| v.as_str()),
            "agent": obj.get("agent").and_then(|v| v.as_str()),
            "_disabledReason": obj.get("_disabledReason").and_then(|v| v.as_str()),
            "state": state,
        }));
    }

    Ok(json!({
        "ok": true,
        "daemon_up": daemon_up,
        "daemon_pid": daemon_pid,
        "jobs": jobs,
    }))
}

// ─── tail-run (WP-13 live tail) ───────────────────────────────────────────────

/// Cap on bytes returned per `agent_ops_tail_run` call (256 KiB). The pkg polls
/// with `nextOffset` to drain anything larger across multiple reads.
const TAIL_CHUNK_CAP: u64 = 256 * 1024;

/// Read the live (or last-completed) run output for a job by byte-range, for the
/// agent-ops pkg's Live-output view. Pure filesystem read on the **shell's own**
/// event loop — never touches the daemon — so the daemon being blocked mid-run
/// (synchronously executing a job) is irrelevant: we still stream whatever the
/// child has teed to `~/.agent-ops/runs/<slug>.<startedAtMs>.tail`.
///
/// Mechanism is script-mode only. Agent jobs (`claude -p`) have no tail file, so
/// we return an empty chunk with `mode:"agent"` and let the pkg render a graceful
/// 'live output not available for agent jobs' state from the marker's status.
///
/// Best-effort + non-throwing: a missing marker / missing tail / unreadable file
/// resolves to `ok:true` with an empty chunk (NOT an Err, NOT a daemon-down) so
/// the view degrades to 'no output yet'. Only a path-escape attempt (marker's
/// `tailPath` pointing outside `runs_dir()`) maps to `code:"io_error"`.
#[tauri::command]
pub async fn agent_ops_tail_run(job_id: String, offset: Option<u64>) -> Result<Value, String> {
    let offset = offset.unwrap_or(0);

    // The "no run yet" success shape — absent marker / nothing to show.
    let empty = |status: Value, started: Value, mode: Value| {
        json!({
            "ok": true,
            "running": false,
            "status": status,
            "startedAtMs": started,
            "mode": mode,
            "chunk": "",
            "nextOffset": offset,
            "eof": true,
        })
    };

    let runs = match runs_dir() {
        Ok(p) => p,
        Err(e) => return Ok(err_value("io_error", None, e)),
    };

    // ── marker ───────────────────────────────────────────────────────────────
    // Absent / unreadable / unparseable marker is NOT an error: the job simply
    // has no run on disk yet → empty, status:null, mode:null.
    let marker_path = runs.join(format!("{}.marker.json", run_slug(&job_id)));
    let marker_raw = match tokio::fs::read(&marker_path).await {
        Ok(r) => r,
        Err(_) => return Ok(empty(Value::Null, Value::Null, Value::Null)),
    };
    let marker: Value = match serde_json::from_slice(&marker_raw) {
        Ok(v) => v,
        Err(_) => return Ok(empty(Value::Null, Value::Null, Value::Null)),
    };

    let status = marker.get("status").and_then(|v| v.as_str());
    let status_json = match status {
        Some("running") => json!("running"),
        Some("done") => json!("done"),
        _ => Value::Null,
    };
    let started_json = marker
        .get("startedAtMs")
        .and_then(|v| v.as_u64())
        .map(|n| json!(n))
        .unwrap_or(Value::Null);
    let mode = marker.get("mode").and_then(|v| v.as_str());
    let mode_json = match mode {
        Some("script") => json!("script"),
        Some("agent") => json!("agent"),
        _ => Value::Null,
    };

    // running = status:"running" AND the marker's pid is actually alive.
    let pid = marker.get("pid").and_then(|v| v.as_u64());
    let running = status == Some("running") && pid.map(|p| pid_alive(p as u32)).unwrap_or(false);

    // Agent mode never produces a tail file — return empty but carry the marker
    // status/started/mode so the pkg can show a spinner while running.
    if mode == Some("agent") {
        return Ok(json!({
            "ok": true,
            "running": running,
            "status": status_json,
            "startedAtMs": started_json,
            "mode": mode_json,
            "chunk": "",
            "nextOffset": offset,
            "eof": true,
        }));
    }

    // ── tail path resolution + security ────────────────────────────────────────
    // No tailPath on the marker → nothing to read, but still surface the run's
    // status (running:false/true) so the view doesn't go blank.
    let Some(tail_path_str) = marker.get("tailPath").and_then(|v| v.as_str()) else {
        return Ok(json!({
            "ok": true,
            "running": running,
            "status": status_json,
            "startedAtMs": started_json,
            "mode": mode_json,
            "chunk": "",
            "nextOffset": offset,
            "eof": true,
        }));
    };
    let tail_path = PathBuf::from(tail_path_str);

    // SECURITY: confirm the marker's tailPath canonicalizes to a location UNDER
    // runs_dir() before opening — a malicious / corrupt marker must not be able
    // to make the shell read an arbitrary file via path-escape.
    let runs_canon = match tokio::fs::canonicalize(&runs).await {
        Ok(p) => p,
        // runs_dir() itself missing → no runs ever; treat as empty.
        Err(_) => return Ok(empty(status_json, started_json, mode_json)),
    };
    match tokio::fs::canonicalize(&tail_path).await {
        Ok(canon) => {
            if !canon.starts_with(&runs_canon) {
                return Ok(err_value(
                    "io_error",
                    None,
                    "tail path escapes the runs directory",
                ));
            }
        }
        // Missing tail file → empty chunk at the current offset, eof.
        Err(_) => {
            return Ok(json!({
                "ok": true,
                "running": running,
                "status": status_json,
                "startedAtMs": started_json,
                "mode": mode_json,
                "chunk": "",
                "nextOffset": offset,
                "eof": true,
            }));
        }
    }

    // ── byte-range read ────────────────────────────────────────────────────────
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = match tokio::fs::File::open(&tail_path).await {
        Ok(f) => f,
        Err(_) => {
            return Ok(json!({
                "ok": true,
                "running": running,
                "status": status_json,
                "startedAtMs": started_json,
                "mode": mode_json,
                "chunk": "",
                "nextOffset": offset,
                "eof": true,
            }));
        }
    };
    let file_len = file.metadata().await.map(|m| m.len()).unwrap_or(0);

    // Offset past EOF (file truncated / rotated) → empty, eof, hold the offset.
    if offset >= file_len {
        return Ok(json!({
            "ok": true,
            "running": running,
            "status": status_json,
            "startedAtMs": started_json,
            "mode": mode_json,
            "chunk": "",
            "nextOffset": offset,
            "eof": true,
        }));
    }

    if file.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
        return Ok(json!({
            "ok": true,
            "running": running,
            "status": status_json,
            "startedAtMs": started_json,
            "mode": mode_json,
            "chunk": "",
            "nextOffset": offset,
            "eof": true,
        }));
    }

    let want = (file_len - offset).min(TAIL_CHUNK_CAP);
    let mut buf = vec![0u8; want as usize];
    let read = match file.read(&mut buf).await {
        Ok(n) => n,
        Err(_) => 0,
    };
    buf.truncate(read);
    let next_offset = offset + read as u64;
    let eof = next_offset >= file_len;
    let chunk = String::from_utf8_lossy(&buf).into_owned();

    Ok(json!({
        "ok": true,
        "running": running,
        "status": status_json,
        "startedAtMs": started_json,
        "mode": mode_json,
        "chunk": chunk,
        "nextOffset": next_offset,
        "eof": eof,
    }))
}
