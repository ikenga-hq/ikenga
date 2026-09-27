//! agent-ops file surface (WP-09 / WP-13 / WP-14), shared by the desktop
//! `#[tauri::command]`s in `commands::agent_ops` and the daemon's `/api/rpc`
//! arms (WP-19 slice 3).
//!
//! Everything here reads or atomically rewrites files under a home directory
//! the CALLER passes in: the project-scoped job config
//! (`<home>/.atelier/skill-agent-ops/jobs.json`), the agent-ops daemon's
//! `<home>/.agent-ops/daemon.lock` and per-run tail files
//! (`<home>/.agent-ops/runs/`), and its runtime state file (see
//! [`jobs_state_path`]). None of it spawns, signals or talks to the agent-ops
//! daemon — `agent_ops_run_now`, which POSTs to that daemon, stays in
//! `commands::agent_ops` and is not served (its job-id rule,
//! [`trigger_path_segment`], lives here next to the `tail_run` one it builds on).
//!
//! The desktop passes `platform::home_dir()`. The headless daemon passes its
//! router's home seam (`server::router_with_home`), which is the daemon
//! PROCESS's home in production. Single-user seam (G-PRINCIPAL / WP-20): every
//! caller holding the daemon's token manages the same jobs file; under
//! per-principal isolation (T1) this must become the principal's home.
//!
//! Every function resolves `Ok(Value)` carrying `{ ok, ... }` (a typed `code`
//! on failure), exactly as the commands always have; a missing home is
//! `code: "io_error"`, "home directory not found", at the same point the
//! desktop resolved it.

use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

// ─── path resolution ─────────────────────────────────────────────────────────

/// The caller's home, or the error every command has always returned for a
/// host with none. (Desktop: `platform::home_dir()`, since $HOME is unset on
/// Windows. Daemon: the router's home seam.)
fn home(home_dir: Option<&Path>) -> Result<PathBuf, String> {
    home_dir
        .map(Path::to_path_buf)
        .ok_or_else(|| "home directory not found".to_string())
}

fn daemon_lock_path(home_dir: Option<&Path>) -> Result<PathBuf, String> {
    Ok(home(home_dir)?.join(".agent-ops/daemon.lock"))
}

/// Per-run tail directory — sibling of `daemon.lock`. The daemon's script-mode
/// executor tees combined stdout/stderr here; this command reads it back by
/// byte-range. Created mode 0o700 by the daemon's boot sweep (run-tail.ts).
fn runs_dir(home_dir: Option<&Path>) -> Result<PathBuf, String> {
    Ok(home(home_dir)?.join(".agent-ops/runs"))
}

/// Slugify a job id for on-disk file names. **MUST stay identical to `slug()`
/// in the daemon's `lib/run-tail.ts`** — both sides derive the same marker /
/// tail file names from the job id, so any divergence silently breaks the read.
fn run_slug(job_id: &str) -> String {
    job_id.replace(':', "-")
}

/// `<slug>.marker.json` for a job id, or `None` when that name would not be a
/// single plain file name inside the runs dir: a separator (`/`, or `\\` on
/// any host so the rule is the same everywhere), a `..` / `.` component, a
/// root or drive prefix, a NUL, or an empty id. Real ids (`ns:name`, slugged
/// to `ns-name`) are plain names and pass unchanged.
fn marker_file_name(job_id: &str) -> Option<String> {
    let slug = run_slug(job_id);
    if slug.is_empty() || slug.contains(['/', '\\', '\0']) {
        return None;
    }
    let name = format!("{slug}.marker.json");
    let mut parts = Path::new(&name).components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(_)), None) => Some(name),
        _ => None,
    }
}

/// Characters percent-encoded when a job id becomes ONE path segment of the
/// daemon's trigger URL: everything except RFC 3986 unreserved (`A-Z a-z 0-9
/// - . _ ~`) and `:` (a legal `pchar`, and the `ns:name` separator real ids
/// use — left literal so a valid id's URL is byte-identical to before).
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
const TRIGGER_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b':');

/// Validate a caller-supplied job id and encode it as a single path segment
/// of `http://127.0.0.1:<port>/jobs/<segment>/trigger`, or say why not.
///
/// SECURITY: `agent_ops_run_now` sends the daemon's secret with that POST, so
/// the id must not be able to steer it to another path, query or fragment.
/// Refused: anything [`marker_file_name`] refuses (empty, `/`, `\`, NUL, a
/// non-plain name — the same plain-name rule `tail_run` applies), a `.` / `..`
/// dot segment (URL parsers collapse those, `%2e%2e` included), `?`, `#`, `%`
/// (no pre-encoded ids: `%2e%2e` / `%2f` would be decoded by the daemon's
/// router), and any control character. What is left is percent-encoded
/// (non-ASCII as its UTF-8 bytes) so nothing in it can act as a delimiter.
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) fn trigger_path_segment(job_id: &str) -> Result<String, &'static str> {
    if job_id == "." || job_id == ".." {
        return Err("job id is a dot segment");
    }
    if job_id.contains(['?', '#', '%']) || job_id.chars().any(char::is_control) {
        return Err("job id contains a URL delimiter or control character");
    }
    if marker_file_name(job_id).is_none() {
        return Err("job id is not a plain name");
    }
    Ok(percent_encoding::utf8_percent_encode(job_id, TRIGGER_SEGMENT).to_string())
}

/// Project-scoped job config the skill + daemon read new-wins. Under $HOME.
fn project_config_path(home_dir: Option<&Path>) -> Result<PathBuf, String> {
    Ok(home(home_dir)?.join(".atelier/skill-agent-ops/jobs.json"))
}

/// The daemon's runtime state file (`nextRunAtMs`, last-status, totals). Lives
/// in the royalti-co monorepo working tree, not under the app data dir. Resolve
/// it the way the daemon does, with overrides for non-default checkouts:
///   1. `AGENT_OPS_STATE_PATH` — full path to jobs-state.json
///   2. `AGENT_OPS_REPO_ROOT`  — `<root>/.company/cron/jobs-state.json`
///   3. default `$HOME/royalti-co/.company/cron/jobs-state.json`
/// Missing file is not an error — listJobs degrades to config-only (no
/// next-fire / state), which the pkg renders honestly.
fn jobs_state_path(home_dir: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("AGENT_OPS_STATE_PATH") {
        return Ok(PathBuf::from(p));
    }
    if let Some(root) = std::env::var_os("AGENT_OPS_REPO_ROOT") {
        return Ok(PathBuf::from(root).join(".company/cron/jobs-state.json"));
    }
    Ok(home(home_dir)?.join("royalti-co/.company/cron/jobs-state.json"))
}

// ─── daemon.lock ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct DaemonLock {
    pub(crate) pid: u32,
    /// `port` / `secret` are read only by the desktop's `agent_ops_run_now`
    /// (not served), so the daemon build never reads them.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub(crate) port: u16,
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub(crate) secret: String,
}

/// Read + parse the daemon lock. `None` (with a reason) means the daemon is not
/// running / the lock is absent or malformed — callers map this to
/// `code: "daemon_down"`.
pub(crate) async fn read_daemon_lock(home_dir: Option<&Path>) -> Result<DaemonLock, String> {
    let path = daemon_lock_path(home_dir)?;
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

pub(crate) fn err_value(code: &str, status: Option<u16>, error: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "code": code,
        "status": status,
        "error": error.into(),
    })
}

// ─── set-enabled ─────────────────────────────────────────────────────────────

/// Flip a job's `enabled` flag in the project-scoped config. Atomic rewrite
/// (temp + rename on the same fs) so the live daemon never reads a torn file.
/// Preserves the file's array-vs-`{jobs:[]}` shape and pretty formatting.
pub(crate) async fn set_enabled(
    home_dir: Option<&Path>,
    job_id: String,
    enabled: bool,
) -> Result<Value, String> {
    let path = match project_config_path(home_dir) {
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
        return Ok(err_value(
            "not_found",
            None,
            format!("no job \"{job_id}\" in config"),
        ));
    }

    let serialized = match serde_json::to_string_pretty(&root) {
        Ok(s) => s + "\n",
        Err(e) => {
            return Ok(err_value(
                "io_error",
                None,
                format!("serialize config: {e}"),
            ))
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(e) = tokio::fs::write(&tmp, serialized.as_bytes()).await {
        return Ok(err_value(
            "io_error",
            None,
            format!("write temp config: {e}"),
        ));
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
async fn read_config_root(home_dir: Option<&Path>) -> Result<Value, Value> {
    let path = project_config_path(home_dir).map_err(|e| err_value("io_error", None, e))?;
    let raw = tokio::fs::read(&path)
        .await
        .map_err(|e| err_value("io_error", None, format!("read config: {e}")))?;
    serde_json::from_slice(&raw)
        .map_err(|e| err_value("io_error", None, format!("parse config: {e}")))
}

/// Atomic write of the config root back to disk (temp + rename, same fs) so the
/// live daemon never reads a torn file.
async fn write_config_root(home_dir: Option<&Path>, root: &Value) -> Result<(), Value> {
    let path = project_config_path(home_dir).map_err(|e| err_value("io_error", None, e))?;
    let serialized = serde_json::to_string_pretty(root)
        .map(|s| s + "\n")
        .map_err(|e| err_value("io_error", None, format!("serialize config: {e}")))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serialized.as_bytes())
        .await
        .map_err(|e| err_value("io_error", None, format!("write temp config: {e}")))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| err_value("io_error", None, format!("rename config: {e}")))?;
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
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
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
            if schedule.split_whitespace().count() >= 6 {
                "6f"
            } else {
                "5f"
            }
        }
    };
    let timezone = nonempty_str(input, "timezone").unwrap_or("Africa/Lagos");
    let enabled = input
        .get("enabled")
        .and_then(|e| e.as_bool())
        .unwrap_or(true);
    let timeout_ms = input
        .get("timeout_ms")
        .and_then(|t| t.as_u64())
        .unwrap_or(300_000);
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
pub(crate) async fn upsert_job(home_dir: Option<&Path>, job: Value) -> Result<Value, String> {
    let (id, full) = match build_job(&job) {
        Ok(v) => v,
        Err(e) => return Ok(e),
    };
    let mut root = match read_config_root(home_dir).await {
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
    if let Err(e) = write_config_root(home_dir, &root).await {
        return Ok(e);
    }
    Ok(json!({ "ok": true, "jobId": id, "created": created }))
}

/// Remove a job from the project-scoped config by id. Atomic write; the daemon
/// stops scheduling it on next load.
pub(crate) async fn delete_job(home_dir: Option<&Path>, job_id: String) -> Result<Value, String> {
    let mut root = match read_config_root(home_dir).await {
        Ok(r) => r,
        Err(e) => return Ok(e),
    };
    let Some(arr) = jobs_array_mut(&mut root) else {
        return Ok(err_value("io_error", None, "config is not a jobs array"));
    };
    let before = arr.len();
    arr.retain(|j| j.get("id").and_then(|v| v.as_str()) != Some(job_id.as_str()));
    if arr.len() == before {
        return Ok(err_value(
            "not_found",
            None,
            format!("no job \"{job_id}\" in config"),
        ));
    }
    if let Err(e) = write_config_root(home_dir, &root).await {
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
pub(crate) async fn list_jobs(home_dir: Option<&Path>) -> Result<Value, String> {
    // Config (required for the job list).
    let cfg_path = match project_config_path(home_dir) {
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
    let state_map: Map<String, Value> = match jobs_state_path(home_dir) {
        Ok(p) => match tokio::fs::read(&p).await {
            Ok(r) => serde_json::from_slice::<Map<String, Value>>(&r).unwrap_or_default(),
            Err(_) => Map::new(),
        },
        Err(_) => Map::new(),
    };

    // Daemon liveness from the lock.
    let (daemon_up, daemon_pid) = match read_daemon_lock(home_dir).await {
        Ok(l) => (pid_alive(l.pid), Some(l.pid)),
        Err(_) => (false, None),
    };

    let mut jobs: Vec<Value> = Vec::with_capacity(cfg_jobs.len());
    for job in cfg_jobs {
        let Some(obj) = job.as_object() else { continue };
        let id = obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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
/// the view degrades to 'no output yet'. Only a path-escape attempt (a job id
/// that is not a plain file name, or the marker's `tailPath` pointing outside
/// `runs_dir()`) maps to `code:"io_error"` — and a missing home, as everywhere.
pub(crate) async fn tail_run(
    home_dir: Option<&Path>,
    job_id: String,
    offset: Option<u64>,
) -> Result<Value, String> {
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

    let runs = match runs_dir(home_dir) {
        Ok(p) => p,
        Err(e) => return Ok(err_value("io_error", None, e)),
    };

    // ── marker ───────────────────────────────────────────────────────────────
    // Absent / unreadable / unparseable marker is NOT an error: the job simply
    // has no run on disk yet → empty, status:null, mode:null.
    //
    // SECURITY: the caller's job id becomes a file name inside `runs`. Before
    // WP-19 slice 3 it was joined unchecked, so `../x` or an absolute `/x`
    // opened `<anywhere>.marker.json` outside the runs dir (the tail read
    // below was already confined; the marker read was not). Refused here, in
    // the shared core, so the desktop command gets the fix too.
    let Some(marker_name) = marker_file_name(&job_id) else {
        return Ok(err_value(
            "io_error",
            None,
            "job id escapes the runs directory",
        ));
    };
    let marker_path = runs.join(marker_name);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_file_name_keeps_real_ids_and_refuses_escapes() {
        assert_eq!(
            marker_file_name("ns:nightly").as_deref(),
            Some("ns-nightly.marker.json")
        );
        assert_eq!(
            marker_file_name("plain").as_deref(),
            Some("plain.marker.json")
        );
        // A dotted name is still one plain file name inside the dir.
        assert_eq!(marker_file_name("..").as_deref(), Some("...marker.json"));
        for bad in ["", "../x", "a/b", "/etc/x", "..\\x", "a\\b", "a\0b"] {
            assert_eq!(marker_file_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn trigger_path_segment_encodes_real_ids_and_refuses_steering() {
        // Real ids pass through byte-identical (`:` stays literal).
        assert_eq!(
            trigger_path_segment("ns:nightly").as_deref(),
            Ok("ns:nightly")
        );
        assert_eq!(
            trigger_path_segment("daily-digest_v2.1~x").as_deref(),
            Ok("daily-digest_v2.1~x")
        );
        // Unicode / spaces / sub-delims are allowed but encoded in-segment.
        assert_eq!(
            trigger_path_segment("caf\u{e9} job").as_deref(),
            Ok("caf%C3%A9%20job")
        );
        assert_eq!(
            trigger_path_segment("a;b=c&d").as_deref(),
            Ok("a%3Bb%3Dc%26d")
        );
        // Dotted but not a dot segment: still one plain name.
        assert_eq!(trigger_path_segment("...").as_deref(), Ok("..."));

        for bad in [
            "", ".", "..", "../x", "..\\x", "a/b", "/abs", "/", "a\\b", "a?b", "a#b", "?", "#",
            "%2e%2e", "%2F", "a%b", "a\0b", "a\nb", "a\rb", "a\tb", "a\u{7f}b",
        ] {
            assert!(
                trigger_path_segment(bad).is_err(),
                "{bad:?} must be refused"
            );
        }

        // Whatever passes lands as exactly one segment of the trigger URL.
        for id in [
            "ns:x",
            "caf\u{e9}",
            "a b",
            "x@y",
            "a+b",
            "[v6]",
            "a'b\"c",
            "...",
        ] {
            let seg = trigger_path_segment(id).unwrap();
            let url = format!("http://127.0.0.1:1/jobs/{seg}/trigger");
            let parsed = url::Url::parse(&url).unwrap();
            let segs: Vec<_> = parsed.path_segments().unwrap().collect();
            assert_eq!(segs, ["jobs", seg.as_str(), "trigger"], "{id:?}");
            assert_eq!(parsed.query(), None, "{id:?}");
            assert_eq!(parsed.fragment(), None, "{id:?}");
        }
    }
}
