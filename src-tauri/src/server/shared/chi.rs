//! The read side of the Chi run cache (WP-19 slice 3): `chi_status` and
//! `chi_list`'s cache half, shared by the desktop `#[tauri::command]`s in
//! `commands::chi` and the daemon's `/api/rpc` arms.
//!
//! What is here is exactly the read path: the `chi_cache` row fetch, the
//! per-run output-file merge, and the detached-run (WP-18b) liveness
//! decision. None of it writes. The reconciliation sweep that *persists* a
//! detached run's terminal status (`cache_update_done_if_live`) and the run
//! notification it raises stay in `commands::chi`: they need the desktop's
//! `AppHandle`, and a read must not have side effects anyway.
//!
//! Layout: the desktop keeps per-run output files in
//! `<app_data_dir>/`[`CACHE_DIR`]; the daemon mirrors that under its
//! `--data-dir`. Relative `output_path`s resolve against that directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::chi_liveness::{decide_liveness, PidProbe, RunLiveness};
use crate::db::PaDb;

/// The cache directory's name under the app data dir (desktop) or
/// `--data-dir` (daemon).
pub(crate) const CACHE_DIR: &str = "chi-cache";

#[derive(Serialize)]
pub struct ChiRunResult {
    pub run_id: String,
    pub status: String,
    pub output: Option<String>,
    pub output_truncated: Option<bool>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ChiCacheRow {
    pub run_id: String,
    pub engine_id: String,
    pub external_id: Option<String>,
    pub brief: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub status: String,
    pub output_path: Option<String>,
    pub output_truncated: Option<bool>,
    pub error: Option<String>,
    pub artifacts: Option<serde_json::Value>,
    pub parent_id: Option<String>,
    pub owner: String,
    /// The detached chi-runner's pid (WP-18b); `None` for in-process runs.
    pub pid: Option<i64>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub expires_at: Option<String>,
}

/// On-disk shape for a per-run output file. The cache row points at this file.
///
/// In-process runs write `output` / `error` / `done_at`. A detached
/// chi-runner additionally writes `status` (`running`, then one of `done` /
/// `failed` / `timed_out`) and the engine's `external_id` — see
/// `chi_liveness::runner_terminal_status`.
#[derive(Serialize, Deserialize)]
pub(crate) struct RunOutputFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

/// Parse an output file. chi-runner rewrites it in place, so a reader can
/// catch it half-written: anything unparseable is "no information yet".
pub(crate) fn parse_output_file(s: &str) -> Option<RunOutputFile> {
    serde_json::from_str(s).ok()
}

pub(crate) async fn read_output_file(path: &Path) -> Option<RunOutputFile> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => parse_output_file(&s),
        Err(_) => None,
    }
}

/// A row's `output_path`, relative paths resolved against the cache dir.
pub(crate) fn resolve_output_path(cache_dir: &Path, output_path: Option<&str>) -> Option<PathBuf> {
    output_path
        .filter(|p| !p.is_empty())
        .map(Path::new)
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cache_dir.join(p)
            }
        })
}

pub(crate) fn pid_alive(pid: i64, probe: &(dyn Fn(u32) -> PidProbe + Sync)) -> bool {
    u32::try_from(pid)
        .map(|p| probe(p).alive())
        .unwrap_or(false)
}

/// Convert a SQLite row into a `ChiCacheRow`. Explicit typing keeps sqlx happy
/// when the query is built from a `&'static str`.
fn row_to_cache_row(r: &sqlx::sqlite::SqliteRow) -> Result<ChiCacheRow, String> {
    let artifacts: Option<String> = r.try_get("artifacts").ok().flatten();
    let artifacts = artifacts.and_then(|s| serde_json::from_str(&s).ok());

    let output_truncated: Option<i64> = r.try_get("output_truncated").ok().flatten();
    let output_truncated = output_truncated.map(|v| v != 0);

    Ok(ChiCacheRow {
        run_id: r.get("run_id"),
        engine_id: r.get("engine_id"),
        external_id: r.get("external_id"),
        brief: r.get("brief"),
        cwd: r.get("cwd"),
        model: r.get("model"),
        mode: r.get("mode"),
        status: r.get("status"),
        output_path: r.get("output_path"),
        output_truncated,
        error: r.get("error"),
        artifacts,
        parent_id: r.get("parent_id"),
        owner: r.get("owner"),
        pid: r.get("pid"),
        started_at: r.get("started_at"),
        ended_at: r.get("ended_at"),
        last_seen_at: r.get("last_seen_at"),
        expires_at: r.get("expires_at"),
    })
}

pub(crate) async fn cache_get(db: &PaDb, run_id: &str) -> Result<Option<ChiCacheRow>, String> {
    let pool = db.ensure_pool().await?;
    let row = sqlx::query(
        "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                output_path, output_truncated, error, artifacts, parent_id, owner,
                pid, started_at, ended_at, last_seen_at, expires_at
         FROM chi_cache WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("chi_cache get: {e}"))?;

    row.as_ref().map(row_to_cache_row).transpose()
}

pub(crate) async fn cache_list(
    db: &PaDb,
    engine_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ChiCacheRow>, String> {
    let pool = db.ensure_pool().await?;
    let rows = if let Some(engine) = engine_id {
        sqlx::query(
            "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                    output_path, output_truncated, error, artifacts, parent_id, owner,
                    pid, started_at, ended_at, last_seen_at, expires_at
             FROM chi_cache
             WHERE engine_id = ?
             ORDER BY last_seen_at DESC
             LIMIT ?",
        )
        .bind(engine)
        .bind(limit)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query(
            "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                    output_path, output_truncated, error, artifacts, parent_id, owner,
                    pid, started_at, ended_at, last_seen_at, expires_at
             FROM chi_cache
             ORDER BY last_seen_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| format!("chi_cache list: {e}"))?;

    rows.iter().map(row_to_cache_row).collect()
}

/// `chi_list`'s limit: 50 by default, clamped to `1..=200`.
pub(crate) fn list_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

/// The cached half of `chi_list`: `chi_cache` rows, newest `last_seen_at`
/// first, optionally one engine's. [`list_merged`] is the whole command.
pub(crate) async fn list(
    db: &PaDb,
    engine_id: Option<&str>,
    limit: Option<i64>,
) -> Result<Vec<ChiCacheRow>, String> {
    cache_list(db, engine_id, list_limit(limit)).await
}

/// `chi_list`, both surfaces: the cache rows ([`list`]) merged with Claude's
/// on-disk JSONL sessions from `projects_root` (`<home>/.claude/projects`,
/// via `claude_sessions::list_sessions`) when no engine filter or
/// `claude-code` is asked for, then sorted newest `last_seen_at` first and
/// truncated to the limit.
///
/// A session whose id matches a cache row's `external_id` refreshes that
/// row's `last_seen_at` instead of adding a row. A failed session scan (or
/// no home: `projects_root = None`, the desktop's "HOME unset") is logged at
/// debug and leaves the cache rows as the answer — as the desktop always has.
///
/// The desktop passes its process home's root and `FsReach::Follow`; the
/// daemon (since WP-19 slice 5b, which serves `claude_list_sessions`) its
/// router home's root and `FsReach::Confined`.
pub(crate) async fn list_merged(
    db: &PaDb,
    engine_id: Option<&str>,
    limit: Option<i64>,
    projects_root: Option<&Path>,
    reach: super::projects::FsReach,
) -> Result<Vec<ChiCacheRow>, String> {
    let mut rows = list(db, engine_id, limit).await?;

    // Merge with Claude JSONL records when no engine filter or claude-code.
    if engine_id.is_none() || engine_id == Some("claude-code") {
        let sessions = projects_root
            .ok_or_else(|| "HOME unset".to_string())
            .and_then(|root| {
                super::claude_sessions::list_sessions(
                    root,
                    None,
                    Some(list_limit(limit) as usize),
                    reach,
                )
            });
        match sessions {
            Ok(sessions) => merge_claude_sessions(&mut rows, sessions),
            Err(e) => {
                log::debug!(target: "ikenga::chi", "claude_list_sessions failed: {e}");
            }
        }
    }

    rows.sort_by(|a, b| {
        b.last_seen_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.last_seen_at.as_deref().unwrap_or(""))
    });

    rows.truncate(list_limit(limit) as usize);
    Ok(rows)
}

/// Fold Claude sessions into `rows` (see [`list_merged`]).
fn merge_claude_sessions(
    rows: &mut Vec<ChiCacheRow>,
    sessions: Vec<super::claude_sessions::SessionSummary>,
) {
    let mut seen: std::collections::HashSet<String> =
        rows.iter().filter_map(|r| r.external_id.clone()).collect();
    for s in sessions {
        if seen.contains(&s.session_id) {
            // Refresh last_seen_at on matching cache rows.
            for row in rows.iter_mut() {
                if row.external_id.as_deref() == Some(&s.session_id) {
                    row.last_seen_at = s.last_message_at.clone().or(Some(s.started_at.clone()));
                }
            }
            continue;
        }
        seen.insert(s.session_id.clone());
        rows.push(ChiCacheRow {
            run_id: s.session_id.clone(),
            engine_id: "claude-code".to_string(),
            external_id: Some(s.session_id.clone()),
            brief: s.title.clone(),
            cwd: Some(s.project_dir.clone()),
            model: s.model.clone(),
            mode: None,
            status: "done".to_string(),
            output_path: None,
            output_truncated: None,
            error: None,
            artifacts: None,
            parent_id: None,
            owner: "agent".to_string(),
            pid: None,
            started_at: Some(s.started_at.clone()),
            ended_at: s.last_message_at.clone(),
            last_seen_at: s.last_message_at.clone().or(Some(s.started_at.clone())),
            expires_at: None,
        });
    }
}

/// Whether `status` may open a row's output file wherever it points, or only
/// inside the cache dir.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputFiles {
    /// Desktop: the row's `output_path` is read as stored. Rows are written by
    /// the app's own `chi_run` (absolute paths into its cache dir).
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    AsStored,
    /// Daemon: an `output_path` that resolves outside the cache dir is refused.
    /// `db_exec` is served over the network, so a token holder could otherwise
    /// point a row at any file and have `chi_status` read it past the
    /// `fs_roots` allowlist every `fs_*` arm enforces.
    InCacheDir,
}

/// Refuse a path that exists outside `cache_dir`. A missing file is left to
/// the read (which finds nothing) — there is nothing to leak.
fn confine(cache_dir: &Path, path: &Path) -> Result<(), String> {
    let Ok(canon) = std::fs::canonicalize(path) else {
        return Ok(());
    };
    let inside = std::fs::canonicalize(cache_dir)
        .map(|dir| canon.starts_with(dir))
        .unwrap_or(false);
    if inside {
        Ok(())
    } else {
        Err(format!(
            "chi run output path is outside the chi cache dir: {}",
            path.display()
        ))
    }
}

/// Read the status of a Chi run from the cache and its output file.
///
/// A detached run (WP-18b) whose row still says `queued` / `running` gets the
/// liveness decision applied to what is returned — the pid probe, then the
/// runner's status file — but NOT persisted: the row only learns its end from
/// the desktop's reconciliation sweep. `probe` is `chi_liveness::probe_runner`
/// in production.
pub(crate) async fn status(
    db: &PaDb,
    cache_dir: &Path,
    run_id: &str,
    probe: &(dyn Fn(u32) -> PidProbe + Sync),
    files: OutputFiles,
) -> Result<ChiRunResult, String> {
    let row = cache_get(db, run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    let output_path = resolve_output_path(cache_dir, row.output_path.as_deref());
    if let (OutputFiles::InCacheDir, Some(path)) = (files, output_path.as_deref()) {
        confine(cache_dir, path)?;
    }

    // A live detached run: the row only learns its end from the sweep, so
    // report what the runner's pid + status file say now (the same decision
    // the sweep will persist). The pid is probed before the file is read —
    // see `chi_liveness::decide_liveness`.
    let detached_live = row
        .pid
        .filter(|_| matches!(row.status.as_str(), "queued" | "running"))
        .map(|pid| pid_alive(pid, probe));

    let file = if let Some(path) = output_path {
        read_output_file(&path).await
    } else {
        None
    };

    let (status, decided_error) = match detached_live {
        Some(alive) => match decide_liveness(
            alive,
            file.as_ref().and_then(|f| f.status.as_deref()),
            file.as_ref().and_then(|f| f.error.as_deref()),
        ) {
            RunLiveness::Running => (row.status, None),
            RunLiveness::Terminal { status, error } => (status.to_string(), error),
        },
        None => (row.status, None),
    };

    Ok(ChiRunResult {
        run_id: row.run_id,
        status,
        output: file.as_ref().and_then(|f| f.output.clone()).or(row.brief),
        output_truncated: row.output_truncated,
        error: decided_error.or(file.and_then(|f| f.error)).or(row.error),
    })
}
