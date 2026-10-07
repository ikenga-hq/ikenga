//! `/api/rpc` bodies for the local-state commands served in WP-19 slice 2
//! (Supabase config, env-backed secret names, settings, data health, backup
//! list/delete and pkg settings), slice 3 (the chi run-cache reads, the
//! agent-ops job files, the OS-username fallback) and slice 6 (the approve-gate
//! draft queue, the pkg permission-violation audit, `pkg_db_diag`).
//!
//! The arm *names* stay in `rpc.rs`'s dispatch `match` (the parity ratchet
//! reads them there); the arms delegate here. Every body calls the same core
//! the desktop `#[tauri::command]` calls (`server::shared::*`,
//! `pkg::settings_values`), rooted at the daemon's `--data-dir` instead of
//! `app_data_dir`, and returns the same serialized type — so the JSON shapes
//! are the desktop's by construction.
//!
//! **Arguments.** The web transport forwards `tauri-cmd.ts`'s argument object
//! verbatim: camelCase (`projectId`, `anonKey`, `pkgId`), because on desktop
//! Tauri does the camel → snake conversion and nothing does it here. The
//! snake_case spelling is accepted as well, for curl'd probes and callers
//! using the Rust names. `null` reads as absent, which is how Tauri
//! deserializes an `Option<T>` argument.
//!
//! **No `--data-dir`.** Every command here reads or writes something under
//! the data dir, so without one each returns an error naming the missing flag
//! — never an empty answer that would read as authoritative.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::OnceCell;
use tracing::warn;

use super::rpc::RpcResponse;
use super::rpc_shell::targ;
use super::shared::chi::OutputFiles;
use super::shared::settings::{SettingsManager, SettingsScope};
use super::shared::{
    agent_ops, backups, chi, chi_exec, chi_liveness, data_health, identity, pa_actions, pkg_db,
    supabase_config,
};
use super::AppState;
use crate::db::PaDb;

const NO_DATA_DIR_SUPABASE: &str =
    "no data dir: the daemon was started without --data-dir, so there is no supabase.json to read or write";
const NO_DATA_DIR_BACKUPS: &str =
    "no data dir: the daemon was started without --data-dir, so there is no backups directory";
const NO_DATA_DIR_SETTINGS: &str =
    "no settings store: the daemon was started without --data-dir, so there is no ikenga.db (settings_kv) to open";
const NO_HOME_SETTINGS: &str =
    "no settings store: the daemon has no HOME in its environment, so there is no ~/.ikenga/settings.json to resolve";

// ─── argument helpers ────────────────────────────────────────────────────────

/// The first of `names` present in `args` with a non-null value.
pub(super) fn arg<'a>(args: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names
        .iter()
        .find_map(|name| args.get(*name).filter(|v| !v.is_null()))
}

/// An optional string argument; present-but-not-a-string is a caller error,
/// as it is when Tauri deserializes an `Option<String>`.
pub(super) fn opt_str(args: &Value, names: &[&str]) -> Result<Option<String>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{}` must be a string", names[0])),
    }
}

pub(super) fn req_str(args: &Value, names: &[&str]) -> Result<String, String> {
    opt_str(args, names)?.ok_or_else(|| format!("`{}` is required", names[0]))
}

fn opt_bool(args: &Value, names: &[&str]) -> Result<Option<bool>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("`{}` must be a boolean", names[0])),
    }
}

/// An optional integer argument (Tauri's `Option<i64>`): a JSON float or a
/// string is a caller error, not a silent default.
fn opt_i64(args: &Value, names: &[&str]) -> Result<Option<i64>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("`{}` must be an integer", names[0])),
    }
}

/// An optional non-negative integer argument (Tauri's `Option<u64>`).
fn opt_u64(args: &Value, names: &[&str]) -> Result<Option<u64>, String> {
    match arg(args, names) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{}` must be a non-negative integer", names[0])),
    }
}

fn req_bool(args: &Value, names: &[&str]) -> Result<bool, String> {
    opt_bool(args, names)?.ok_or_else(|| format!("`{}` is required", names[0]))
}

/// `Ok(v)` → success with `v`'s JSON; `Err(e)` → `"<cmd>: <e>"`.
pub(super) fn respond<T: serde::Serialize>(cmd: &str, result: Result<T, String>) -> RpcResponse {
    match result {
        Ok(v) => RpcResponse::success(v),
        Err(e) => RpcResponse::error(format!("{cmd}: {e}")),
    }
}

pub(super) fn data_dir<'a>(state: &'a AppState, missing: &str) -> Result<&'a Path, String> {
    state
        .config
        .data_dir
        .as_deref()
        .ok_or_else(|| missing.to_string())
}

pub(super) fn pa_db(state: &AppState) -> Result<&PaDb, String> {
    state
        .pa_db
        .as_deref()
        .ok_or_else(|| super::rpc::NO_DB.to_string())
}

// ─── Supabase config ─────────────────────────────────────────────────────────

pub(super) fn supabase_config_get(state: &AppState) -> RpcResponse {
    let r = data_dir(state, NO_DATA_DIR_SUPABASE).and_then(supabase_config::get);
    respond("supabase_config_get", r)
}

pub(super) fn supabase_config_set(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let dir = data_dir(state, NO_DATA_DIR_SUPABASE)?;
        let url = req_str(args, &["url"])?;
        let anon_key = req_str(args, &["anonKey", "anon_key"])?;
        // Absent / null → preserve the stored key; "" → clear it. Same three
        // cases as the desktop, decided in the shared core.
        let service_role_key = opt_str(args, &["serviceRoleKey", "service_role_key"])?;
        supabase_config::set(dir, url, anon_key, service_role_key)
    })();
    respond("supabase_config_set", r)
}

pub(super) fn supabase_config_clear(state: &AppState) -> RpcResponse {
    let r = data_dir(state, NO_DATA_DIR_SUPABASE).and_then(supabase_config::clear);
    respond("supabase_config_clear", r)
}

// ─── Settings ────────────────────────────────────────────────────────────────

/// The daemon's `SettingsManager`: the desktop's, rooted at `--data-dir`
/// (`ikenga.db` for `settings_kv`, `screenshot-config.json`) with no change
/// notifier — so no `settings://changed` emits and no file watchers.
///
/// Initialized on first use, not at boot: `PaDb` is lazy so a daemon nobody
/// queries never touches `ikenga.db`, and `initialize` opens it (and runs the
/// kv → file migration). A failed initialize is logged and retried on the next
/// call; the call itself still goes to the manager, whose own
/// migration-ready gate answers exactly as it does on a desktop whose boot-time
/// initialize failed.
pub(crate) struct DaemonSettings {
    manager: SettingsManager,
    ready: OnceCell<()>,
}

impl DaemonSettings {
    pub(crate) fn new(db: Arc<PaDb>, data_dir: PathBuf, home: PathBuf) -> Self {
        Self {
            manager: SettingsManager::with_notifier(None, db, data_dir, home),
            ready: OnceCell::new(),
        }
    }

    async fn manager(&self) -> &SettingsManager {
        let init = self
            .ready
            .get_or_try_init(|| async { self.manager.initialize().await })
            .await;
        if let Err(e) = init {
            warn!("[settings] initialization failed: {e}");
        }
        &self.manager
    }
}

pub(super) async fn settings(state: &AppState) -> Result<&SettingsManager, String> {
    match &state.settings {
        Some(s) => Ok(s.manager().await),
        None if state.config.data_dir.is_none() || state.pa_db.is_none() => {
            Err(NO_DATA_DIR_SETTINGS.to_string())
        }
        None => Err(NO_HOME_SETTINGS.to_string()),
    }
}

pub(super) async fn settings_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let key = req_str(args, &["key"])?;
        settings(state).await?.get_legacy(&key).await
    }
    .await;
    respond("settings_get", r)
}

pub(super) async fn settings_set(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let key = req_str(args, &["key"])?;
        let value = req_str(args, &["value"])?;
        settings(state).await?.set_legacy(&key, &value).await
    }
    .await;
    respond("settings_set", r)
}

pub(super) async fn settings_get_all(state: &AppState) -> RpcResponse {
    let r = async { settings(state).await?.get_all().await }.await;
    respond("settings_get_all", r)
}

/// Destructive, with the desktop's exact blast radius (`SettingsManager::
/// clear_all`): the settings-owned `settings_kv` rows, the personal
/// `settings.json`, and `<data-dir>/screenshot-config.json`. Nothing else.
pub(super) async fn settings_clear_all(state: &AppState) -> RpcResponse {
    let r = async { settings(state).await?.clear_all().await }.await;
    respond("settings_clear_all", r)
}

pub(super) async fn settings_read_file(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope = opt_str(args, &["scope"])?;
        let scope = SettingsScope::parse(scope.as_deref().unwrap_or("project"))?;
        let project_id = opt_str(args, &["projectId", "project_id"])?;
        settings(state)
            .await?
            .read(scope, project_id.as_deref())
            .await
    }
    .await;
    respond("settings_read_file", r)
}

pub(super) async fn settings_write_field(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope = SettingsScope::parse(&req_str(args, &["scope"])?)?;
        let field = req_str(args, &["field"])?;
        // Tauri's `value: Value` takes JSON null as a value (a removal sends
        // one), so read it raw rather than through `arg`, which skips nulls.
        let value = args
            .get("value")
            .cloned()
            .ok_or_else(|| "`value` is required".to_string())?;
        let project_id = opt_str(args, &["projectId", "project_id"])?;
        let remove = opt_bool(args, &["remove"])?.unwrap_or(false);
        settings(state)
            .await?
            .write_field(scope, project_id.as_deref(), &field, value, remove)
            .await
    }
    .await;
    respond("settings_write_field", r)
}

// ─── Data health ─────────────────────────────────────────────────────────────

pub(super) async fn data_health_scan(state: &AppState) -> RpcResponse {
    let r = async { data_health::scan(pa_db(state)?).await }.await;
    respond("data_health_scan", r)
}

/// The daemon's `ikenga.db` under `--data-dir`, stat'ed only.
pub(super) fn data_health_db_size(state: &AppState) -> RpcResponse {
    respond(
        "data_health_db_size",
        pa_db(state).and_then(data_health::db_size),
    )
}

// ─── Backups (list / delete) ─────────────────────────────────────────────────

/// `<data-dir>/backups` — the daemon's one data dir standing in for the
/// desktop's `app_local_data_dir`, with the same `backups` leaf.
fn backups_dir(state: &AppState) -> Result<PathBuf, String> {
    Ok(data_dir(state, NO_DATA_DIR_BACKUPS)?.join(backups::BACKUPS_DIR))
}

pub(super) fn backup_list(state: &AppState) -> RpcResponse {
    let r = backups_dir(state).and_then(|dir| backups::list(&dir));
    respond("backup_list", r)
}

pub(super) fn backup_delete(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let dir = backups_dir(state)?;
        let path = req_str(args, &["path"])?;
        backups::delete(&dir, &path)
    })();
    respond("backup_delete", r)
}

// ─── Pkg settings ────────────────────────────────────────────────────────────

/// Schema from the daemon's `--pkgs-dir` index, values from `pkg_settings`.
/// An unknown pkg is the desktop's answer: `schema: null` + whatever rows the
/// table holds for that id — not an error.
///
/// The stored rows are those `pkg_settings_set` (`server::rpc_exec`) wrote
/// to this daemon's `ikenga.db` — under T1, the principal's own — over the
/// manifest defaults.
pub(super) async fn pkg_settings_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id = req_str(args, &["pkgId", "pkg_id"])?;
        let db = pa_db(state)?;
        let schema = state.pkg_index.settings_schema(&pkg_id);
        crate::pkg::settings_values::get(db, pkg_id, schema).await
    }
    .await;
    respond("pkg_settings_get", r)
}

// ─── Chi runs ────────────────────────────────────────────────────────────────
//
// Reads since WP-19 slice 3; the writes (`chi_run` / `chi_resume` /
// `chi_cancel`) since WP-P10, over `shared::chi_exec` — the core the desktop
// commands call. Every spawn and signal goes through `executor::current()`:
// in a T1 principal child that is the principal's own uid, against the
// principal's own `<data-dir>/ikenga.db` and `<data-dir>/chi-cache`, so a run
// id from another principal is simply "chi run not found" here. The desktop
// deltas: a run naming no cwd starts in the router home (the principal's
// home under T1) instead of the process cwd; output paths are confined to the
// cache dir; the in-process `openrouter` engine is refused (it needs the
// desktop's engine registry + vault); and a cwd gets `~` expansion only,
// never `$VAR` (the daemon's env holds operator secrets). As on the desktop,
// no engine gets the prompt on its command line (I-7, see `chi_exec`).

/// The Chi write arms' process state, held in `AppState::chi`: the live-run
/// registry `chi_cancel` reaches in-process runs through, and where engine
/// binaries resolve (the host PATH in production, a stub in tests).
pub(crate) struct DaemonChi {
    pub(crate) runtime: Arc<chi_exec::ChiRuntime>,
    pub(crate) resolver: Arc<dyn chi_exec::EngineResolver>,
}

impl DaemonChi {
    pub(crate) fn host() -> Self {
        Self {
            runtime: Arc::new(chi_exec::ChiRuntime::new()),
            resolver: Arc::new(chi_exec::HostResolver),
        }
    }
}

/// The daemon's [`chi_exec::ChiEnv`]: `--data-dir`'s db and chi-cache, the
/// router home as the default cwd, output files confined to the cache dir,
/// and `~`-only cwd expansion (this process's env holds the operator's
/// `IKENGA_SECRET_*` defaults).
pub(super) fn chi_env(state: &AppState) -> Result<chi_exec::ChiEnv, String> {
    let db = state
        .pa_db
        .clone()
        .ok_or_else(|| super::rpc::NO_DB.to_string())?;
    Ok(chi_exec::ChiEnv {
        db,
        cache_dir: chi_cache_dir(state)?,
        runtime: state.chi.runtime.clone(),
        default_cwd: state.home.clone(),
        files: OutputFiles::InCacheDir,
        resolver: state.chi.resolver.clone(),
        cwd_expansion: chi_exec::CwdExpansion::TildeOnly,
    })
}

/// `chi_run`'s options. `tauri-cmd.ts` sends them nested under `opts`
/// (`invoke('chi_run', { opts })`); a caller without that wrapper (curl, the
/// Mattermost bridge) may send the same fields flat. camelCase and
/// snake_case both accepted, `null` read as absent, a wrong type an error —
/// as Tauri's own `ChiRunOpts` deserialization would.
pub(super) fn chi_run_opts(args: &Value) -> Result<chi_exec::ChiRunOpts, String> {
    let src = match args.get("opts") {
        None | Some(Value::Null) => args,
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err("`opts` must be an object".to_string()),
    };
    let timeout_seconds = opt_u64(src, &["timeoutSeconds", "timeout_seconds"])?
        .map(|t| u32::try_from(t).map_err(|_| "`timeoutSeconds` must fit in 32 bits".to_string()))
        .transpose()?;
    Ok(chi_exec::ChiRunOpts {
        engine_id: req_str(src, &["engineId", "engine_id"])?,
        prompt: req_str(src, &["prompt"])?,
        cwd: opt_str(src, &["cwd"])?,
        model: opt_str(src, &["model"])?,
        mode: opt_str(src, &["mode"])?,
        timeout_seconds,
        parent_id: opt_str(src, &["parentId", "parent_id"])?,
        resume_session_id: opt_str(src, &["resumeSessionId", "resume_session_id"])?,
        persistent: opt_bool(src, &["persistent"])?.unwrap_or(false),
    })
}

/// Start a Chi run (the desktop's `chi_run`, owner `cli`). Returns the run
/// id at once; the engine runs in this process (or detached, `persistent`).
pub(super) async fn chi_run(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let opts = chi_run_opts(args)?;
        let env = chi_env(state)?;
        chi_exec::spawn_run(&env, &chi_exec::NoInProcessEngines, opts, "cli").await
    }
    .await;
    respond("chi_run", r)
}

/// Continue a run of this daemon's (same run id) with a new prompt.
pub(super) async fn chi_resume(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let run_id = req_str(args, &["runId", "run_id"])?;
        let prompt = req_str(args, &["prompt"])?;
        let env = chi_env(state)?;
        chi_exec::resume_run(&env, &chi_exec::NoInProcessEngines, run_id, prompt).await
    }
    .await;
    respond("chi_resume", r)
}

/// Cancel a run of this daemon's: kill its engine child or its detached
/// runner's process group (only when that pid is our runner, as our uid).
pub(super) async fn chi_cancel(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let run_id = req_str(args, &["runId", "run_id"])?;
        let env = chi_env(state)?;
        chi_exec::cancel_run(&env.db, &env.runtime, &env.cache_dir, &run_id).await
    }
    .await;
    respond("chi_cancel", r)
}

/// `<data-dir>/chi-cache`, mirroring the desktop's `<app_data_dir>/chi-cache`.
fn chi_cache_dir(state: &AppState) -> Result<PathBuf, String> {
    Ok(data_dir(state, super::rpc::NO_DB)?.join(chi::CACHE_DIR))
}

/// The desktop's read: row + output file + detached-run liveness decision,
/// nothing written (the reconciliation sweep — `chi_exec`, started at boot
/// when a chi-cache exists or on the first detached run — persists a detached
/// run's end). Output files are confined to the cache dir here — see
/// [`OutputFiles::InCacheDir`].
pub(super) async fn chi_status(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let run_id = req_str(args, &["runId", "run_id"])?;
        let db = pa_db(state)?;
        let cache_dir = chi_cache_dir(state)?;
        chi::status(
            db,
            &cache_dir,
            &run_id,
            &chi_liveness::probe_runner,
            OutputFiles::InCacheDir,
        )
        .await
    }
    .await;
    respond("chi_status", r)
}

/// The desktop's `chi_list`: the daemon's `chi_cache` rows merged with
/// Claude's JSONL sessions (served since WP-19 slice 5b as
/// `claude_list_sessions`) — `shared::chi::list_merged`, the same core the
/// desktop command calls. The sessions are read from the router home's
/// `~/.claude/projects` (the daemon PROCESS's home: single-user seam,
/// G-PRINCIPAL / WP-20) with `FsReach::Confined`, so a log symlinked out of
/// that dir is skipped.
pub(super) async fn chi_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let engine_id = opt_str(args, &["engineId", "engine_id"])?;
        let limit = opt_i64(args, &["limit"])?;
        let projects_root = state
            .home
            .as_deref()
            .map(super::shared::claude_sessions::projects_root_in);
        chi::list_merged(
            pa_db(state)?,
            engine_id.as_deref(),
            limit,
            projects_root.as_deref(),
            super::shared::projects::FsReach::Confined,
        )
        .await
    }
    .await;
    respond("chi_list", r)
}

// ─── agent-ops job files ─────────────────────────────────────────────────────
//
// Rooted at `state.home` — the router's home seam, the daemon PROCESS's home
// in production. Single-user seam (G-PRINCIPAL / WP-20): every token holder
// manages the same `~/.atelier/skill-agent-ops/jobs.json`; under T1 this must
// be the calling principal's home. Each core resolves `{ ok, ... }` exactly
// as the desktop command does (a missing home is its `io_error`), so these
// are RPC successes carrying that value. `agent_ops_run_now` lives in
// `server::rpc_exec`, confined to the principal's own jobs and agent-ops
// daemon; the approve gate's hardcoded mutation-worker wake below is the other
// daemon caller of `agent_ops::run_now`.
//
// A fresh host has no `jobs.json` (gap audit 2026-10-06 rank 12), so list and
// upsert pass `MissingConfig::Empty`: an empty list, and the first upsert
// creates the directory (0700) and the file. The desktop commands keep their
// `io_error` for a missing config.

pub(super) async fn agent_ops_list_jobs(state: &AppState) -> RpcResponse {
    respond(
        "agent_ops_list_jobs",
        agent_ops::list_jobs(state.home.as_deref(), agent_ops::MissingConfig::Empty).await,
    )
}

pub(super) async fn agent_ops_tail_run(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let job_id = req_str(args, &["jobId", "job_id"])?;
        let offset = opt_u64(args, &["offset"])?;
        agent_ops::tail_run(state.home.as_deref(), job_id, offset).await
    }
    .await;
    respond("agent_ops_tail_run", r)
}

pub(super) async fn agent_ops_upsert_job(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        // Tauri's `job: Value` takes whatever is sent, JSON null included;
        // the core's own validation answers a bad one.
        let job = args
            .get("job")
            .cloned()
            .ok_or_else(|| "`job` is required".to_string())?;
        agent_ops::upsert_job(state.home.as_deref(), job, agent_ops::MissingConfig::Empty).await
    }
    .await;
    respond("agent_ops_upsert_job", r)
}

pub(super) async fn agent_ops_delete_job(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let job_id = req_str(args, &["jobId", "job_id"])?;
        agent_ops::delete_job(state.home.as_deref(), job_id).await
    }
    .await;
    respond("agent_ops_delete_job", r)
}

pub(super) async fn agent_ops_set_enabled(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let job_id = req_str(args, &["jobId", "job_id"])?;
        let enabled = req_bool(args, &["enabled"])?;
        agent_ops::set_enabled(state.home.as_deref(), job_id, enabled).await
    }
    .await;
    respond("agent_ops_set_enabled", r)
}

// ─── Secrets (G-30; per-principal layer, remote-access WP-21) ─────────────────
//
// Over `state.secrets` (`crate::secrets_env::DaemonSecrets`): a T1 principal
// child's own store over the `IKENGA_SECRET_*` operator default, or the
// default alone (T0). The arg names are the desktop commands' (`key`, `value`,
// `scope`); `scope` decodes as the desktop's `Scope`.

pub(super) fn secrets_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = req_str(args, &["key"]).and_then(|key| state.secrets.get(&key));
    respond("secrets_get", r)
}

pub(super) fn secrets_list_keys(state: &AppState) -> RpcResponse {
    respond("secrets_list_keys", state.secrets.list_keys())
}

pub(super) fn secrets_index_names(state: &AppState) -> RpcResponse {
    respond("secrets_index_names", state.secrets.index_names())
}

pub(super) fn secrets_default_names(state: &AppState) -> RpcResponse {
    RpcResponse::success(state.secrets.default_names())
}

pub(super) fn secrets_get_scoped(state: &AppState, args: &Value) -> RpcResponse {
    let r = super::rpc::scope_kind(args).and_then(|scope| {
        let key = req_str(args, &["key"])?;
        state.secrets.get_scoped(&scope, &key)
    });
    respond("secrets_get_scoped", r)
}

pub(super) fn secrets_list_keys_scoped(state: &AppState, args: &Value) -> RpcResponse {
    let r = super::rpc::scope_kind(args).and_then(|scope| state.secrets.list_keys_scoped(&scope));
    respond("secrets_list_keys_scoped", r)
}

/// The four writes. Into the principal's own store; without one (T0) every
/// write is refused with `secrets_env::WRITE_REFUSAL`, the operator runbook.
pub(super) fn secrets_write(state: &AppState, cmd: &str, args: &Value) -> RpcResponse {
    let secrets = &state.secrets;
    let r = (|| match cmd {
        "secrets_set" => {
            let key = req_str(args, &["key"])?;
            let value = req_str(args, &["value"])?;
            secrets.set(&key, &value)
        }
        "secrets_delete" => secrets.delete(&req_str(args, &["key"])?),
        "secrets_set_scoped" => {
            let scope = super::rpc::scope_kind(args)?;
            let key = req_str(args, &["key"])?;
            let value = req_str(args, &["value"])?;
            secrets.set_scoped(&scope, &key, &value)
        }
        "secrets_delete_scoped" => {
            let scope = super::rpc::scope_kind(args)?;
            secrets.delete_scoped(&scope, &req_str(args, &["key"])?)
        }
        other => Err(format!("not a secrets write: {other}")),
    })();
    respond(cmd, r)
}

/// `secrets_lock_state`, `secrets_lock`, `secrets_set_passphrase`,
/// `secrets_unlock`. Served only with a principal layer (T1). A T0 daemon
/// answers exactly what `rpc_handler`'s unknown-command fallthrough always
/// answered for these, because Settings → Secrets reads a successful
/// unlocked state as "writable" and every T0 write is refused
/// (`secrets_env`, "No passphrase layer"). The string is pinned to the
/// fallthrough's by `secrets_lock_family_on_t0_is_the_unknown_command_error`.
pub(super) fn secrets_lock_family(state: &AppState, cmd: &str) -> RpcResponse {
    let Some(lock) = state.secrets.lock_state() else {
        tracing::debug!("Unimplemented or pass-through RPC command: {cmd}");
        return RpcResponse::error(format!(
            "Command '{cmd}' not implemented in headless daemon"
        ));
    };
    match cmd {
        "secrets_lock_state" | "secrets_lock" => RpcResponse::success(lock),
        _ => RpcResponse::error(format!("{cmd}: {}", crate::secrets_env::NO_PASSPHRASE)),
    }
}

// ─── Identity ────────────────────────────────────────────────────────────────

/// The daemon PROCESS's OS user (the desktop's function, unchanged).
/// Single-user seam (G-PRINCIPAL / WP-20): under T1 it must be the calling
/// principal's username, not the daemon's.
pub(super) fn os_username() -> RpcResponse {
    RpcResponse::success(identity::os_username())
}

// ─── Approve gate + pkg DB audit / diagnostics (WP-19 slice 6) ──────────────
//
// Over `server::shared::{pa_actions, pkg_db}` — the cores the desktop commands
// call — and the daemon's `ikenga.db`; without `--data-dir` each is `NO_DB`.
//
// **No events.** After the same writes the desktop emits `pa-action-paused`,
// `pa-action-committed`, `pa-action-retried` and `pa-action-rejected`. The
// daemon has no event channel (the web transport's `listen()` is a no-op), so
// these arms change the same rows and emit nothing; `/outbox/approvals` polls
// `pa_actions_list`, and the FE invalidates on each call's own result.
//
// **The wake.** Commit and retry then wake the mutation worker the way the
// desktop does: a detached, fire-and-forget POST to the local agent-ops
// daemon's run-now for `pa_actions::SEND_WORKER_JOB` — the literal, never a
// caller's id — rooted at the router home (`state.home`, the seam the agent-ops
// arms use). The RPC answer neither waits on nor depends on it; no home or no
// `daemon.lock` means no call, and the worker's poll catches up.

/// [`respond`], without doubling a prefix the desktop's error already carries
/// (`"pa_actions_pause: drafts cannot be empty"`).
pub(super) fn respond_named<T: serde::Serialize>(
    cmd: &str,
    result: Result<T, String>,
) -> RpcResponse {
    match result {
        Err(e) if e.starts_with(&format!("{cmd}: ")) => RpcResponse::error(e),
        r => respond(cmd, r),
    }
}

pub(super) async fn pa_actions_pause(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let batch_id: String = targ(args, &["batchId", "batch_id"])?;
        let action_id: String = targ(args, &["actionId", "action_id"])?;
        let drafts: Vec<pa_actions::PaPauseDraftInput> = targ(args, &["drafts"])?;
        pa_actions::pause(pa_db(state)?, &batch_id, &action_id, &drafts).await
    }
    .await;
    respond_named("pa_actions_pause", r)
}

pub(super) async fn pa_actions_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let status: Option<String> = targ(args, &["status"])?;
        pa_actions::list(pa_db(state)?, status.as_deref()).await
    }
    .await;
    respond("pa_actions_list", r)
}

pub(super) async fn pa_actions_update(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let draft_id: String = targ(args, &["draftId", "draft_id"])?;
        // Tauri's `patch: Value` takes JSON null as a value; read it raw.
        let patch = args
            .get("patch")
            .cloned()
            .ok_or_else(|| "`patch` is required".to_string())?;
        pa_actions::update(pa_db(state)?, &draft_id, &patch).await
    }
    .await;
    respond("pa_actions_update", r)
}

pub(super) async fn pa_actions_commit(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let draft_id: String = targ(args, &["draftId", "draft_id"])?;
        // The returned row is the desktop's `pa-action-committed` payload;
        // there is no channel to emit it on here.
        pa_actions::commit(pa_db(state)?, &draft_id).await?;
        drop(pa_actions::wake_send_worker(state.home.clone()));
        Ok(())
    }
    .await;
    respond("pa_actions_commit", r)
}

pub(super) async fn pa_actions_retry(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let draft_id: String = targ(args, &["draftId", "draft_id"])?;
        pa_actions::retry(pa_db(state)?, &draft_id).await?;
        drop(pa_actions::wake_send_worker(state.home.clone()));
        Ok(())
    }
    .await;
    respond("pa_actions_retry", r)
}

pub(super) async fn pa_actions_reject(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let draft_id: String = targ(args, &["draftId", "draft_id"])?;
        pa_actions::reject(pa_db(state)?, &draft_id).await
    }
    .await;
    respond("pa_actions_reject", r)
}

pub(super) async fn pkg_permission_violations_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id: Option<String> = targ(args, &["pkgId", "pkg_id"])?;
        let limit: Option<i64> = targ(args, &["limit"])?;
        pkg_db::violations_list(pa_db(state)?, pkg_id, limit).await
    }
    .await;
    respond("pkg_permission_violations_list", r)
}

pub(super) async fn pkg_permission_violations_clear(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id: String = targ(args, &["pkgId", "pkg_id"])?;
        pkg_db::violations_clear(pa_db(state)?, &pkg_id).await
    }
    .await;
    respond("pkg_permission_violations_clear", r)
}

/// Every field is the daemon's `PaDb`'s — its `ikenga.db` path and that
/// file's `pkg_installed` — exactly as the desktop fills them from its own.
pub(super) async fn pkg_db_diag(state: &AppState) -> RpcResponse {
    let r = async { pkg_db::db_diag(pa_db(state)?).await }.await;
    respond("pkg_db_diag", r)
}

#[cfg(test)]
mod tests {
    //! House pattern (see the slice-1 tests in `server/mod.rs`): a literal
    //! `ServerConfig` → the router → `oneshot` POST `/api/rpc` with the bearer
    //! token. `router_with_home` is `create_router` with the settings home
    //! pinned to a temp dir, so nothing here can touch the real `~/.ikenga`.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::shared::settings::{schema, SettingsManager, SettingsScope};
    use crate::server::shared::{backups, data_health, supabase_config};
    use crate::server::{router_with_home, ServerConfig};

    fn config(data_dir: Option<PathBuf>, pkgs_dir: Option<PathBuf>) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir,
            data_dir,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        }
    }

    /// A daemon with `--data-dir`, `--pkgs-dir` and a home, all under one
    /// tempdir. `db` is the same `PaDb` the router holds.
    struct Daemon {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        data: PathBuf,
        home: PathBuf,
        pkgs: PathBuf,
        db: Arc<PaDb>,
        router: Router,
    }

    fn daemon() -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, home, pkgs) = (root.join("data"), root.join("home"), root.join("pkgs"));
        for d in [&data, &home, &pkgs] {
            std::fs::create_dir_all(d).unwrap();
        }
        write_settings_pkg(&pkgs);
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with_home(
            config(Some(data.clone()), Some(pkgs.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db.clone()),
            None,
            Some(home.clone()),
        );
        Daemon {
            _tmp: tmp,
            root,
            data,
            home,
            pkgs,
            db,
            router,
        }
    }

    /// No `--data-dir` (so no `PaDb` either), with or without a home.
    fn bare(home: Option<PathBuf>) -> Router {
        router_with_home(
            config(None, None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
            home,
        )
    }

    async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The `data` of a successful response (`null` for a unit result).
    async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    async fn err(router: &Router, cmd: &str, args: Value) -> String {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
        res["error"].as_str().unwrap().to_string()
    }

    // ── Supabase config ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn supabase_config_keeps_the_desktop_semantics() {
        let d = daemon();
        let r = &d.router;
        let file = d.data.join(supabase_config::FILENAME);

        assert_eq!(ok(r, "supabase_config_get", json!({})).await, Value::Null);

        // camelCase (what tauri-cmd.ts sends).
        let set = json!({
            "url": "https://x.supabase.co", "anonKey": "anon1", "serviceRoleKey": "srk1"
        });
        assert_eq!(ok(r, "supabase_config_set", set).await, Value::Null);
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(
            got,
            json!({
                "url": "https://x.supabase.co", "anon_key": "anon1", "service_role_key": "srk1"
            })
        );
        // Same serialized type as the desktop command returns.
        assert_eq!(
            got,
            serde_json::to_value(supabase_config::get(&d.data).unwrap()).unwrap()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "supabase.json must be owner-only");
        }

        // snake_case, key absent → preserved.
        let set = json!({ "url": "https://y.supabase.co", "anon_key": "anon2" });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(got["url"], "https://y.supabase.co");
        assert_eq!(got["anon_key"], "anon2");
        assert_eq!(
            got["service_role_key"], "srk1",
            "absent key must be preserved"
        );

        // null (what `serviceRoleKey ?? null` sends) → preserved too.
        let set = json!({ "url": "u", "anonKey": "a", "serviceRoleKey": null });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert_eq!(got["service_role_key"], "srk1");

        // explicit "" → cleared (field omitted, as the desktop serializes it).
        let set = json!({ "url": "u", "anonKey": "a", "service_role_key": "" });
        ok(r, "supabase_config_set", set).await;
        let got = ok(r, "supabase_config_get", json!({})).await;
        assert!(got.get("service_role_key").is_none(), "{got}");

        let e = err(
            r,
            "supabase_config_set",
            json!({ "url": " ", "anonKey": "a" }),
        )
        .await;
        assert!(e.contains("url and anon_key are required"), "{e}");
        let e = err(r, "supabase_config_set", json!({ "url": "u" })).await;
        assert!(e.contains("`anonKey` is required"), "{e}");

        assert_eq!(ok(r, "supabase_config_clear", json!({})).await, Value::Null);
        assert!(!file.exists());
        assert_eq!(ok(r, "supabase_config_get", json!({})).await, Value::Null);
    }

    #[tokio::test]
    async fn supabase_config_without_data_dir_names_the_flag() {
        let r = bare(None);
        for (cmd, args) in [
            ("supabase_config_get", json!({})),
            ("supabase_config_set", json!({ "url": "u", "anonKey": "a" })),
            ("supabase_config_clear", json!({})),
        ] {
            let e = err(&r, cmd, args).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }
    }

    // ── Secrets ─────────────────────────────────────────────────────────────

    /// The daemon's names are its own namespace's: `secrets_env::list_keys`,
    /// in the desktop's bare `string[]` shape. No data dir needed.
    #[tokio::test]
    async fn secrets_index_names_is_the_env_namespace() {
        for r in [daemon().router, bare(None)] {
            let names = ok(&r, "secrets_index_names", json!({})).await;
            assert!(names.is_array(), "{names}");
            assert_eq!(names, json!(crate::secrets_env::list_keys()));
        }
    }

    /// R1 (WP-21 review): T0 must look to the FE exactly as it did on
    /// `main`, where the lock family was unserved. Settings → Secrets marks
    /// the vault writable on `available && configured && !locked`, and T0
    /// reports `available: true`, so a served unlocked state would show add /
    /// edit controls whose writes all fail with `WRITE_REFUSAL`. Each of the
    /// four must answer the fallthrough's own unknown-command error.
    #[tokio::test]
    async fn secrets_lock_family_on_t0_is_the_unknown_command_error() {
        for r in [daemon().router, bare(None)] {
            let unknown = err(&r, "wp21_no_such_command", json!({})).await;
            assert_eq!(
                unknown, "Command 'wp21_no_such_command' not implemented in headless daemon",
                "the fallthrough's shape drifted; update secrets_lock_family with it"
            );
            for (cmd, args) in [
                ("secrets_lock_state", json!({})),
                ("secrets_lock", json!({})),
                ("secrets_set_passphrase", json!({ "passphrase": "p" })),
                ("secrets_unlock", json!({ "passphrase": "p" })),
            ] {
                let e = err(&r, cmd, args).await;
                assert_eq!(e, unknown.replace("wp21_no_such_command", cmd), "{cmd}");
            }
            // And the rest of T0 is as before: readable, never writable.
            let status = ok(&r, "secrets_vault_status", json!({})).await;
            assert_eq!(status["available"], true);
            assert_eq!(status["writable"], false);
            let e = err(&r, "secrets_set", json!({ "key": "K", "value": "v" })).await;
            assert!(e.contains(crate::secrets_env::WRITE_REFUSAL), "{e}");
        }
    }

    // ── Settings ────────────────────────────────────────────────────────────

    /// A second, directly-constructed manager over the same db / data dir /
    /// home: what the desktop command would return for the same state.
    async fn direct_manager(d: &Daemon) -> SettingsManager {
        let m = SettingsManager::with_notifier(None, d.db.clone(), d.data.clone(), d.home.clone());
        m.initialize().await.unwrap();
        m
    }

    #[tokio::test]
    async fn settings_read_write_read_round_trips_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let personal = d.home.join(".ikenga").join("settings.json");

        // First use runs `initialize`, whose kv → file migration writes the
        // personal file — on the desktop too, at boot.
        let before = ok(r, "settings_read_file", json!({ "scope": "personal" })).await;
        assert_eq!(before["personalPresent"], true);
        assert_eq!(before["personalPath"], personal.to_string_lossy().as_ref());
        assert_ne!(before["personal"]["appearance"]["theme"], "B");

        let args = json!({
            "scope": "personal", "field": "appearance.theme", "value": "B",
            "remove": false, "projectId": null
        });
        let written = ok(r, "settings_write_field", args).await;
        assert_eq!(written["personal"]["appearance"]["theme"], "B");

        let after = ok(r, "settings_read_file", json!({ "scope": "personal" })).await;
        assert_eq!(after["personalPresent"], true);
        assert_eq!(after["personal"]["appearance"]["theme"], "B");
        assert!(std::fs::read_to_string(&personal)
            .unwrap()
            .contains("\"B\""));

        // Byte-for-byte the core's `SettingsReadResult`.
        let direct = direct_manager(&d)
            .await
            .read(SettingsScope::Personal, None)
            .await
            .unwrap();
        assert_eq!(after, serde_json::to_value(direct).unwrap());

        // Removal: the FE sends the field with `remove: true`.
        let args = json!({
            "scope": "personal", "field": "appearance.theme", "value": null, "remove": true
        });
        let removed = ok(r, "settings_write_field", args).await;
        assert_ne!(removed["personal"]["appearance"]["theme"], "B");

        let args = json!({ "scope": "personal", "field": "appearance.theme" });
        let e = err(r, "settings_write_field", args).await;
        assert!(e.contains("`value` is required"), "{e}");
        let e = err(r, "settings_read_file", json!({ "scope": "nope" })).await;
        assert!(e.starts_with("settings_read_file: "), "{e}");
    }

    #[tokio::test]
    async fn settings_project_scope_takes_both_spellings() {
        let d = daemon();
        let r = &d.router;
        let root = d.root.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        crate::db::exec(
            &d.db,
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('p1', 'P1', ?, 1, 0, 0)",
            &[json!(root.to_string_lossy())],
        )
        .await
        .unwrap();

        let args = json!({
            "scope": "project", "field": "appearance.theme", "value": "C", "project_id": "p1"
        });
        ok(r, "settings_write_field", args).await;
        let camel = json!({ "scope": "project", "projectId": "p1" });
        let snake = json!({ "scope": "project", "project_id": "p1" });
        let camel = ok(r, "settings_read_file", camel).await;
        let snake = ok(r, "settings_read_file", snake).await;
        assert_eq!(camel, snake);
        assert_eq!(camel["projectId"], "p1");
        assert_eq!(camel["projectPresent"], true);
        assert_eq!(camel["effective"]["appearance"]["theme"], "C");
        assert_eq!(
            camel["projectPath"],
            root.join(".ikenga")
                .join("settings.json")
                .to_string_lossy()
                .as_ref()
        );

        let args = json!({ "scope": "project", "projectId": "nope" });
        let e = err(r, "settings_read_file", args).await;
        assert!(e.contains("project not found"), "{e}");
    }

    #[tokio::test]
    async fn settings_kv_get_set_get_all() {
        let d = daemon();
        let r = &d.router;

        // Not a settings-owned key → plain kv row.
        ok(
            r,
            "settings_set",
            json!({ "key": "wp19.probe", "value": "v1" }),
        )
        .await;
        assert_eq!(
            ok(r, "settings_get", json!({ "key": "wp19.probe" })).await,
            "v1"
        );
        let absent = ok(r, "settings_get", json!({ "key": "wp19.absent" })).await;
        assert_eq!(absent, Value::Null);

        // A legacy key → written through to the personal file.
        let args = json!({ "key": "appearance.theme", "value": "\"C\"" });
        ok(r, "settings_set", args).await;
        let theme = ok(r, "settings_get", json!({ "key": "appearance.theme" })).await;
        let direct = direct_manager(&d).await;
        let expected = direct.get_legacy("appearance.theme").await.unwrap();
        assert_eq!(theme, json!(expected));
        let personal = d.home.join(".ikenga").join("settings.json");
        assert!(std::fs::read_to_string(personal).unwrap().contains("\"C\""));

        let all = ok(r, "settings_get_all", json!({})).await;
        assert_eq!(all["wp19.probe"], "v1");
        assert_eq!(all, json!(direct.get_all().await.unwrap()));

        let e = err(r, "settings_set", json!({ "key": "k" })).await;
        assert!(e.contains("`value` is required"), "{e}");
    }

    /// `settings_clear_all` deletes exactly what the desktop deletes: the
    /// settings-owned kv rows (`schema::is_settings_owned_key`), the personal
    /// file and `<data-dir>/screenshot-config.json`. Every other kv row and
    /// every other file survives.
    #[tokio::test]
    async fn settings_clear_all_has_the_desktop_blast_radius() {
        let d = daemon();
        let r = &d.router;

        let owned = json!({ "key": "appearance.theme", "value": "\"B\"" });
        ok(r, "settings_set", owned).await;
        let foreign = json!({ "key": "wp19.foreign", "value": "keep" });
        ok(r, "settings_set", foreign).await;
        let active = json!({ "key": "shell.activeProjectId", "value": "default" });
        ok(r, "settings_set", active).await;
        let personal = d.home.join(".ikenga").join("settings.json");
        let screenshot = d.data.join("screenshot-config.json");
        std::fs::write(&screenshot, br#"{"override_dir":null}"#).unwrap();
        let bystanders = [
            d.home.join(".ikenga").join("actions.json"),
            d.data.join("supabase.json"),
            d.data.join("unrelated.txt"),
        ];
        for f in &bystanders {
            std::fs::write(f, b"{}").unwrap();
        }
        assert!(personal.exists());

        type Kv = std::collections::BTreeMap<String, String>;
        async fn kv(db: &PaDb) -> Kv {
            let q = "SELECT key, value FROM settings_kv ORDER BY key";
            crate::db::query_json(db, q, &[])
                .await
                .unwrap()
                .into_iter()
                .map(|row| {
                    let k = row["key"].as_str().unwrap().to_string();
                    (k, row["value"].as_str().unwrap().to_string())
                })
                .collect()
        }
        let before = kv(&d.db).await;
        assert!(before.keys().any(|k| schema::is_settings_owned_key(k)));

        assert_eq!(ok(r, "settings_clear_all", json!({})).await, Value::Null);

        let after = kv(&d.db).await;
        let expected: Kv = before
            .into_iter()
            .filter(|(k, _)| !schema::is_settings_owned_key(k))
            .collect();
        assert_eq!(after, expected);
        assert_eq!(after["wp19.foreign"], "keep");
        assert_eq!(after["shell.activeProjectId"], "default");

        assert!(
            !personal.exists(),
            "personal settings.json is desktop-cleared"
        );
        assert!(
            !screenshot.exists(),
            "screenshot-config.json is desktop-cleared"
        );
        for f in &bystanders {
            assert!(f.exists(), "{} must survive clear_all", f.display());
        }
        assert!(d.data.join("ikenga.db").exists());
    }

    #[tokio::test]
    async fn settings_name_what_is_missing() {
        let cmds = [
            ("settings_get", json!({ "key": "k" })),
            ("settings_set", json!({ "key": "k", "value": "v" })),
            ("settings_get_all", json!({})),
            ("settings_clear_all", json!({})),
            ("settings_read_file", json!({ "scope": "personal" })),
            (
                "settings_write_field",
                json!({ "scope": "personal", "field": "appearance.theme", "value": "B" }),
            ),
        ];
        let no_data = bare(Some(PathBuf::from("/nonexistent-home")));
        for (cmd, args) in &cmds {
            let e = err(&no_data, cmd, args.clone()).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }

        // Data dir + db, but no home: nowhere to put the personal file.
        let tmp = tempfile::tempdir().unwrap();
        let no_home = router_with_home(
            config(Some(tmp.path().to_path_buf()), None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(Arc::new(PaDb::new(tmp.path().join("ikenga.db")))),
            None,
            None,
        );
        for (cmd, args) in &cmds {
            let e = err(&no_home, cmd, args.clone()).await;
            assert!(e.contains("HOME"), "{cmd}: {e}");
        }
    }

    // ── Data health ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn data_health_matches_the_shared_core() {
        let d = daemon();
        let r = &d.router;

        assert_eq!(ok(r, "data_health_scan", json!({})).await, json!([]));

        crate::db::exec(
            &d.db,
            "INSERT INTO sales_deals (id, company, research_item_id) \
             VALUES ('deal-orphan', 'Beta', 'rn-missing')",
            &[],
        )
        .await
        .unwrap();
        let scan = ok(r, "data_health_scan", json!({})).await;
        let pool = d.db.ensure_reader_pool().await.unwrap();
        let direct = data_health::scan_orphans(&pool).await.unwrap();
        assert_eq!(scan, serde_json::to_value(direct).unwrap());
        assert_eq!(scan[0]["table"], "sales_deals");
        assert_eq!(scan[0]["sample_ids"], json!(["deal-orphan"]));

        let size = ok(r, "data_health_db_size", json!({})).await;
        let db_path = d.data.join("ikenga.db");
        assert_eq!(size["db_path"], db_path.to_string_lossy().as_ref());
        assert!(size["db_bytes"].as_u64().unwrap() > 0);
        let direct = data_health::measure_db_files(&db_path).unwrap();
        assert_eq!(size, serde_json::to_value(direct).unwrap());

        let r = bare(None);
        for cmd in ["data_health_scan", "data_health_db_size"] {
            let e = err(&r, cmd, json!({})).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }
    }

    // ── Backups ─────────────────────────────────────────────────────────────

    fn write_ikbak(path: &Path, created_at: &str) {
        use std::io::Write;
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        let manifest = json!({
            "format_version": 3, "schema_version": 7, "created_at": created_at,
            "hostname": "h", "username": "u", "path_mode": "tokenized",
            "home_dir": "/home/u", "has_secrets": true, "pkg_count": 2
        });
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    #[tokio::test]
    async fn backup_list_and_delete_stay_inside_the_backups_dir() {
        let d = daemon();
        let r = &d.router;
        let dir = d.data.join(backups::BACKUPS_DIR);

        // No dir yet → empty, as on desktop.
        assert_eq!(ok(r, "backup_list", json!({})).await, json!([]));

        std::fs::create_dir_all(&dir).unwrap();
        write_ikbak(&dir.join("old.ikbak"), "2026-01-01T00:00:00Z");
        write_ikbak(&dir.join("new.ikbak"), "2026-09-01T00:00:00Z");
        std::fs::write(dir.join("broken.ikbak"), b"not a zip").unwrap();
        std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();

        let list = ok(r, "backup_list", json!({})).await;
        let direct = backups::list(&dir).unwrap();
        assert_eq!(list, serde_json::to_value(direct).unwrap());
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 3, "{list}");
        assert_eq!(
            rows[0]["created_at"], "2026-09-01T00:00:00Z",
            "newest first"
        );
        assert_eq!(rows[0]["path_mode"], "tokenized");
        assert_eq!(rows[0]["pkg_count"], 2);
        assert_eq!(rows[2]["created_at"], "", "unreadable bundle still listed");

        // Outside the dir: refused, and the target survives.
        let victim = d.data.join("ikenga.db");
        std::fs::write(&victim, b"x").unwrap();
        let escape = dir.join("..").join("ikenga.db");
        for path in [escape.to_string_lossy().into_owned(), "/etc/passwd".into()] {
            let e = err(r, "backup_delete", json!({ "path": path })).await;
            assert!(
                e.contains("refusing to delete file outside backups dir"),
                "{path}: {e}"
            );
        }
        assert!(victim.exists());
        // A symlink inside the dir pointing out is resolved before the check.
        #[cfg(unix)]
        {
            let link = dir.join("link.ikbak");
            std::os::unix::fs::symlink(&victim, &link).unwrap();
            let e = err(r, "backup_delete", json!({ "path": link })).await;
            assert!(e.contains("outside backups dir"), "{e}");
            assert!(victim.exists());
            std::fs::remove_file(&link).unwrap();
        }

        let target = dir.join("old.ikbak");
        let res = ok(r, "backup_delete", json!({ "path": target })).await;
        assert_eq!(res, Value::Null);
        assert!(!target.exists());
        let list = ok(r, "backup_list", json!({})).await;
        assert_eq!(list.as_array().unwrap().len(), 2);

        let r = bare(None);
        let e = err(&r, "backup_list", json!({})).await;
        assert!(e.contains("--data-dir"), "{e}");
        let e = err(&r, "backup_delete", json!({ "path": "/x" })).await;
        assert!(e.contains("--data-dir"), "{e}");
    }

    // ── Pkg settings ────────────────────────────────────────────────────────

    fn write_settings_pkg(pkgs: &Path) {
        let dir = pkgs.join("com.test.settings");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"com.test.settings","name":"S","version":"0.1.0","ikenga_api":"1",
                "settings":{"schema":[
                  {"key":"theme","type":"string","label":"Theme","default":"dark"},
                  {"key":"count","type":"number","label":"Count","default":3}
                ]}}"#,
        )
        .unwrap();
        let bare = pkgs.join("com.test.nosettings");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::write(
            bare.join("manifest.json"),
            r#"{"id":"com.test.nosettings","name":"N","version":"0.1.0","ikenga_api":"1"}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn pkg_settings_get_merges_manifest_defaults_with_stored_rows() {
        use crate::pkg::settings_values;

        let d = daemon();
        let r = &d.router;
        let id = "com.test.settings";
        let pkg = crate::pkg::manifest::Package::load(&d.pkgs.join(id)).unwrap();
        let schema = settings_values::declared_schema(&pkg);

        let got = ok(r, "pkg_settings_get", json!({ "pkgId": id })).await;
        assert_eq!(got["values"], json!({ "theme": "dark", "count": 3 }));
        assert_eq!(got["schema"][0]["key"], "theme");
        let direct = settings_values::get(&d.db, id.into(), schema.clone())
            .await
            .unwrap();
        assert_eq!(got, serde_json::to_value(direct).unwrap());

        // Rows as a desktop-written `ikenga.db` holds them (the FK needs the
        // `pkg_installed` row first; the daemon never writes either).
        crate::db::exec(
            &d.db,
            "INSERT INTO pkg_installed \
             (id, version, ikenga_api, manifest_json, install_path, enabled, installed_at) \
             VALUES (?, '0.1.0', '1', '{}', '/x', 1, 0)",
            &[json!(id)],
        )
        .await
        .unwrap();
        for (key, value) in [("theme", json!("light")), ("extra", json!({ "a": [1] }))] {
            settings_values::set(&d.db, id, key, &value).await.unwrap();
        }
        let snake = ok(r, "pkg_settings_get", json!({ "pkg_id": id })).await;
        assert_eq!(
            snake["values"],
            json!({ "theme": "light", "count": 3, "extra": { "a": [1] } })
        );
        let direct = settings_values::get(&d.db, id.into(), schema)
            .await
            .unwrap();
        assert_eq!(snake, serde_json::to_value(direct).unwrap());

        // No settings block, and unknown: schema null, not an error — the
        // desktop's `SettingsRegistry::schema_for` is `None` for both.
        for other in ["com.test.nosettings", "com.test.unknown"] {
            let got = ok(r, "pkg_settings_get", json!({ "pkgId": other })).await;
            assert_eq!(
                got,
                json!({ "pkg_id": other, "schema": null, "values": {} })
            );
        }

        let e = err(r, "pkg_settings_get", json!({})).await;
        assert!(e.contains("`pkgId` is required"), "{e}");

        let r = bare(None);
        let e = err(&r, "pkg_settings_get", json!({ "pkgId": id })).await;
        assert!(e.contains("--data-dir"), "{e}");
    }

    // ── Slice 3: chi reads, agent-ops files, identity ───────────────────────

    mod slice3 {
        use super::*;
        use crate::server::shared::chi::{self, OutputFiles};
        use crate::server::shared::chi_liveness::{probe_runner, RUNNER_EXITED_ERROR};
        use crate::server::shared::identity;

        // ── chi ──────────────────────────────────────────────────────────────

        struct ChiRow<'a> {
            run_id: &'a str,
            engine: &'a str,
            status: &'a str,
            brief: &'a str,
            output_path: Option<&'a str>,
            pid: Option<i64>,
            last_seen: &'a str,
        }

        async fn seed_chi(db: &PaDb, row: ChiRow<'_>) {
            let pool = db.ensure_pool().await.unwrap();
            sqlx::query(
                "INSERT INTO chi_cache (
                    run_id, engine_id, brief, status, output_path, output_truncated,
                    owner, pid, started_at, last_seen_at
                ) VALUES (?, ?, ?, ?, ?, 0, 'cli', ?, ?, ?)",
            )
            .bind(row.run_id)
            .bind(row.engine)
            .bind(row.brief)
            .bind(row.status)
            .bind(row.output_path)
            .bind(row.pid)
            .bind(row.last_seen)
            .bind(row.last_seen)
            .execute(&pool)
            .await
            .unwrap();
        }

        /// `(status, last_seen_at, ended_at, error)` straight from the table.
        async fn raw_row(
            db: &PaDb,
            run_id: &str,
        ) -> (String, String, Option<String>, Option<String>) {
            use sqlx::Row;
            let pool = db.ensure_pool().await.unwrap();
            let r = sqlx::query(
                "SELECT status, last_seen_at, ended_at, error FROM chi_cache WHERE run_id = ?",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            (
                r.get("status"),
                r.get("last_seen_at"),
                r.get("ended_at"),
                r.get("error"),
            )
        }

        /// A pid that is certainly not a live chi-runner: a child spawned and
        /// reaped here. Should the kernel hand the number on, its new holder
        /// is not `chi-runner`, which the probe reads as `Foreign` — not
        /// alive — so the verdict is the same.
        #[cfg(unix)]
        fn dead_pid() -> i64 {
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            i64::from(pid)
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn chi_status_reads_detached_liveness_without_writing() {
            let d = daemon();
            let r = &d.router;
            let cache_dir = d.data.join(chi::CACHE_DIR);
            std::fs::create_dir_all(&cache_dir).unwrap();

            // Detached, runner gone, status file says done → `done`.
            let done_file =
                r#"{"output":"all good","done_at":"t","status":"done","external_id":"sess-1"}"#;
            std::fs::write(cache_dir.join("r-done.json"), done_file).unwrap();
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-done",
                    engine: "claude-code",
                    status: "running",
                    brief: "the brief",
                    output_path: Some("r-done.json"), // relative → the cache dir
                    pid: Some(dead_pid()),
                    last_seen: "2026-01-01T00:00:00Z",
                },
            )
            .await;
            // Detached, runner gone, file never reached a terminal status.
            let crashed_file = r#"{"output":"partial","status":"running"}"#;
            let crashed_path = cache_dir.join("r-crashed.json");
            std::fs::write(&crashed_path, crashed_file).unwrap();
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-crashed",
                    engine: "claude-code",
                    status: "running",
                    brief: "b",
                    output_path: Some(crashed_path.to_str().unwrap()), // absolute, inside
                    pid: Some(dead_pid()),
                    last_seen: "2026-01-02T00:00:00Z",
                },
            )
            .await;
            // In-process run, finished: no pid, row status as stored.
            std::fs::write(
                cache_dir.join("r-inproc.json"),
                r#"{"output":"x","error":"boom","done_at":"t"}"#,
            )
            .unwrap();
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-inproc",
                    engine: "codex",
                    status: "failed",
                    brief: "b",
                    output_path: Some("r-inproc.json"),
                    pid: None,
                    last_seen: "2026-01-03T00:00:00Z",
                },
            )
            .await;
            // No output file at all → the brief stands in, as on desktop.
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-queued",
                    engine: "codex",
                    status: "queued",
                    brief: "just the brief",
                    output_path: Some("r-queued.json"),
                    pid: None,
                    last_seen: "2026-01-04T00:00:00Z",
                },
            )
            .await;

            let before = raw_row(&d.db, "r-done").await;

            let got = ok(r, "chi_status", json!({ "runId": "r-done" })).await;
            assert_eq!(
                got,
                json!({
                    "run_id": "r-done",
                    "status": "done",
                    "output": "all good",
                    "output_truncated": false,
                    "error": null,
                })
            );
            // snake_case spelling reads the same.
            assert_eq!(
                ok(r, "chi_status", json!({ "run_id": "r-done" })).await,
                got
            );
            // Same JSON as the core the desktop command calls.
            let direct = chi::status(
                &d.db,
                &cache_dir,
                "r-done",
                &probe_runner,
                OutputFiles::AsStored,
            )
            .await
            .unwrap();
            assert_eq!(got, serde_json::to_value(direct).unwrap());

            let crashed = ok(r, "chi_status", json!({ "runId": "r-crashed" })).await;
            assert_eq!(crashed["status"], "failed");
            assert_eq!(crashed["error"], RUNNER_EXITED_ERROR);
            assert_eq!(crashed["output"], "partial");

            let inproc = ok(r, "chi_status", json!({ "runId": "r-inproc" })).await;
            assert_eq!(inproc["status"], "failed");
            assert_eq!(inproc["output"], "x");
            assert_eq!(inproc["error"], "boom");

            let queued = ok(r, "chi_status", json!({ "runId": "r-queued" })).await;
            assert_eq!(queued["status"], "queued");
            assert_eq!(queued["output"], "just the brief");

            // A read, not the sweep: nothing was written back.
            assert_eq!(raw_row(&d.db, "r-done").await, before);
            let (status, _, ended, error) = raw_row(&d.db, "r-crashed").await;
            assert_eq!((status.as_str(), ended, error), ("running", None, None));
            assert_eq!(
                std::fs::read_to_string(cache_dir.join("r-done.json")).unwrap(),
                done_file
            );
            assert_eq!(
                std::fs::read_to_string(&crashed_path).unwrap(),
                crashed_file
            );
        }

        #[tokio::test]
        async fn chi_status_errors_like_the_desktop_and_names_what_is_missing() {
            let d = daemon();
            let r = &d.router;

            // Unknown run: the desktop's error, verbatim after the prefix.
            let e = err(r, "chi_status", json!({ "runId": "nope" })).await;
            assert_eq!(e, "chi_status: chi run not found: nope");

            let e = err(r, "chi_status", json!({})).await;
            assert!(e.contains("`runId` is required"), "{e}");

            // An output_path pointing outside the cache dir is refused, not
            // read (it would bypass the fs_roots allowlist).
            let outside = d.root.join("outside.json");
            std::fs::write(&outside, r#"{"output":"secret"}"#).unwrap();
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-escape",
                    engine: "codex",
                    status: "done",
                    brief: "b",
                    output_path: Some(outside.to_str().unwrap()),
                    pid: None,
                    last_seen: "2026-01-01T00:00:00Z",
                },
            )
            .await;
            let e = err(r, "chi_status", json!({ "runId": "r-escape" })).await;
            assert!(e.contains("outside the chi cache dir"), "{e}");
            assert!(!e.contains("secret"), "{e}");
            // ...and so is a relative one that climbs out.
            seed_chi(
                &d.db,
                ChiRow {
                    run_id: "r-climb",
                    engine: "codex",
                    status: "done",
                    brief: "b",
                    output_path: Some("../../outside.json"),
                    pid: None,
                    last_seen: "2026-01-01T00:00:00Z",
                },
            )
            .await;
            std::fs::create_dir_all(d.data.join(chi::CACHE_DIR)).unwrap();
            let e = err(r, "chi_status", json!({ "runId": "r-climb" })).await;
            assert!(e.contains("outside the chi cache dir"), "{e}");

            // No --data-dir: the flag is named, nothing is guessed.
            let r = bare(Some(d.home.clone()));
            let e = err(&r, "chi_status", json!({ "runId": "r-escape" })).await;
            assert!(e.contains("--data-dir"), "{e}");
            let e = err(&r, "chi_list", json!({})).await;
            assert!(e.contains("--data-dir"), "{e}");
        }

        #[tokio::test]
        async fn chi_list_serves_the_cache_rows_in_the_desktop_shape() {
            let d = daemon();
            let r = &d.router;

            // A daemon never starts chi runs: honest and empty.
            assert_eq!(ok(r, "chi_list", json!({})).await, json!([]));

            for (id, engine, seen) in [
                ("a", "claude-code", "2026-01-01T00:00:00Z"),
                ("b", "codex", "2026-01-03T00:00:00Z"),
                ("c", "claude-code", "2026-01-02T00:00:00Z"),
            ] {
                seed_chi(
                    &d.db,
                    ChiRow {
                        run_id: id,
                        engine,
                        status: "done",
                        brief: "b",
                        output_path: None,
                        pid: None,
                        last_seen: seen,
                    },
                )
                .await;
            }
            let ids = |v: &Value| -> Vec<String> {
                v.as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["run_id"].as_str().unwrap().to_string())
                    .collect()
            };

            let all = ok(r, "chi_list", json!({})).await;
            assert_eq!(ids(&all), ["b", "c", "a"]);
            let direct = chi::list(&d.db, None, None).await.unwrap();
            assert_eq!(all, serde_json::to_value(direct).unwrap());
            // Every `ChiCacheRow` field, snake_case, as the desktop serializes.
            let mut keys: Vec<_> = all[0].as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(
                keys,
                [
                    "artifacts",
                    "brief",
                    "cwd",
                    "ended_at",
                    "engine_id",
                    "error",
                    "expires_at",
                    "external_id",
                    "last_seen_at",
                    "mode",
                    "model",
                    "output_path",
                    "output_truncated",
                    "owner",
                    "parent_id",
                    "pid",
                    "run_id",
                    "started_at",
                    "status",
                ]
            );

            let claude = ok(r, "chi_list", json!({ "engineId": "claude-code" })).await;
            assert_eq!(ids(&claude), ["c", "a"]);
            let snake = ok(r, "chi_list", json!({ "engine_id": "claude-code" })).await;
            assert_eq!(snake, claude);
            let one = ok(r, "chi_list", json!({ "limit": 1 })).await;
            assert_eq!(ids(&one), ["b"]);
            // Clamped to 1..=200 like the desktop, not an error.
            let zero = ok(r, "chi_list", json!({ "limit": 0 })).await;
            assert_eq!(ids(&zero), ["b"]);

            let e = err(r, "chi_list", json!({ "limit": "3" })).await;
            assert!(e.contains("`limit` must be an integer"), "{e}");
        }

        // ── agent-ops ────────────────────────────────────────────────────────

        fn jobs_file(home: &Path) -> PathBuf {
            home.join(".atelier/skill-agent-ops/jobs.json")
        }

        fn runs_dir(home: &Path) -> PathBuf {
            home.join(".agent-ops/runs")
        }

        /// Gap audit 2026-10-06 rank 12: on a fresh host (a T1 principal's
        /// home on first use) there is no `jobs.json`, and the first schedule
        /// could never be created — list and upsert both answered io_error.
        #[tokio::test]
        async fn agent_ops_first_job_on_a_fresh_host() {
            use crate::server::shared::agent_ops::{self, MissingConfig};
            let d = daemon();
            let r = &d.router;
            let file = jobs_file(&d.home);
            assert!(!d.home.join(".atelier").exists());

            // No config: an empty list, not an error.
            let none = ok(r, "agent_ops_list_jobs", json!({})).await;
            assert_eq!(none["ok"], true, "{none}");
            assert_eq!(none["jobs"], json!([]));
            assert_eq!(none["daemon_up"], false);
            // Listing creates nothing.
            assert!(!d.home.join(".atelier").exists());

            let job = json!({
                "id": "ns:first",
                "label": "First",
                "schedule": "0 3 * * *",
                "command": "echo hi",
                "mode": "script",
            });
            let up = ok(r, "agent_ops_upsert_job", json!({ "job": job })).await;
            assert_eq!(
                up,
                json!({ "ok": true, "jobId": "ns:first", "created": true })
            );
            // The bare-array root the agent-ops daemon and skill write.
            let written: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert_eq!(written.as_array().unwrap().len(), 1);
            assert_eq!(written[0]["id"], "ns:first");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for dir in [
                    d.home.join(".atelier"),
                    file.parent().unwrap().to_path_buf(),
                ] {
                    let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
                    assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
                }
            }
            let listed = ok(r, "agent_ops_list_jobs", json!({})).await;
            assert_eq!(listed["jobs"][0]["id"], "ns:first");

            // A config that exists but does not parse is still an io_error —
            // only "not found" reads as empty — and is left untouched.
            std::fs::write(&file, b"{ not json").unwrap();
            let bad = ok(r, "agent_ops_list_jobs", json!({})).await;
            assert_eq!(bad["code"], "io_error");
            let bad_up = ok(r, "agent_ops_upsert_job", json!({ "job": job })).await;
            assert_eq!(bad_up["code"], "io_error");
            assert_eq!(std::fs::read(&file).unwrap(), b"{ not json");

            // The desktop policy is unchanged: a missing config is io_error,
            // and its upsert creates nothing.
            std::fs::remove_dir_all(d.home.join(".atelier")).unwrap();
            let desk = agent_ops::list_jobs(Some(&d.home), MissingConfig::Error)
                .await
                .unwrap();
            assert_eq!(desk["code"], "io_error");
            let desk_up = agent_ops::upsert_job(Some(&d.home), job, MissingConfig::Error)
                .await
                .unwrap();
            assert_eq!(desk_up["code"], "io_error");
            assert!(!d.home.join(".atelier").exists());
        }

        #[tokio::test]
        async fn agent_ops_jobs_round_trip_under_the_router_home() {
            let d = daemon();
            let r = &d.router;
            let file = jobs_file(&d.home);

            // An existing `{ jobs: [] }` root (the object shape) is kept.
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, r#"{ "jobs": [] }"#).unwrap();

            let job = json!({
                "id": "ns:nightly",
                "label": "Nightly",
                "schedule": "0 3 * * *",
                "command": "echo hi",
                "mode": "script",
            });
            let up = ok(r, "agent_ops_upsert_job", json!({ "job": job })).await;
            assert_eq!(
                up,
                json!({ "ok": true, "jobId": "ns:nightly", "created": true })
            );
            let again = ok(r, "agent_ops_upsert_job", json!({ "job": job })).await;
            assert_eq!(again["created"], false);

            let listed = ok(r, "agent_ops_list_jobs", json!({})).await;
            assert_eq!(listed["ok"], true);
            assert_eq!(listed["daemon_up"], false);
            assert_eq!(listed["daemon_pid"], Value::Null);
            let jobs = listed["jobs"].as_array().unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0]["id"], "ns:nightly");
            assert_eq!(jobs[0]["schedule_dialect"], "5f");
            assert_eq!(jobs[0]["enabled"], true);
            // Same value the core (and so the desktop command) produces.
            let direct = crate::server::shared::agent_ops::list_jobs(
                Some(&d.home),
                crate::server::shared::agent_ops::MissingConfig::Empty,
            )
            .await
            .unwrap();
            assert_eq!(listed, direct);

            // Both spellings of the id.
            let off = ok(
                r,
                "agent_ops_set_enabled",
                json!({ "jobId": "ns:nightly", "enabled": false }),
            )
            .await;
            assert_eq!(
                off,
                json!({ "ok": true, "jobId": "ns:nightly", "enabled": false })
            );
            let listed = ok(r, "agent_ops_list_jobs", json!({})).await;
            assert_eq!(listed["jobs"][0]["enabled"], false);
            let on = ok(
                r,
                "agent_ops_set_enabled",
                json!({ "job_id": "ns:nightly", "enabled": true }),
            )
            .await;
            assert_eq!(on["enabled"], true);
            let missing = ok(
                r,
                "agent_ops_set_enabled",
                json!({ "jobId": "nope", "enabled": true }),
            )
            .await;
            assert_eq!(missing["code"], "not_found");
            let e = err(r, "agent_ops_set_enabled", json!({ "jobId": "ns:nightly" })).await;
            assert!(e.contains("`enabled` is required"), "{e}");

            let del = ok(r, "agent_ops_delete_job", json!({ "job_id": "ns:nightly" })).await;
            assert_eq!(del, json!({ "ok": true, "jobId": "ns:nightly" }));
            let del = ok(r, "agent_ops_delete_job", json!({ "jobId": "ns:nightly" })).await;
            assert_eq!(del["code"], "not_found");

            // The `{ jobs: [...] }` shape survived every rewrite.
            let on_disk: Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            assert_eq!(on_disk, json!({ "jobs": [] }));

            // Validation errors come back as the core's value.
            let bad = ok(r, "agent_ops_upsert_job", json!({ "job": { "id": "x" } })).await;
            assert_eq!(bad["ok"], false);
            let e = err(r, "agent_ops_upsert_job", json!({})).await;
            assert!(e.contains("`job` is required"), "{e}");
        }

        #[tokio::test]
        async fn agent_ops_tail_run_reads_by_offset_and_refuses_escapes() {
            let d = daemon();
            let r = &d.router;
            let runs = runs_dir(&d.home);
            std::fs::create_dir_all(&runs).unwrap();
            let tail = runs.join("ns-nightly.1.tail");
            std::fs::write(&tail, "hello world").unwrap();
            std::fs::write(
                runs.join("ns-nightly.marker.json"),
                json!({
                    "status": "done",
                    "startedAtMs": 1,
                    "mode": "script",
                    "tailPath": tail.to_str().unwrap(),
                })
                .to_string(),
            )
            .unwrap();

            let all = ok(r, "agent_ops_tail_run", json!({ "jobId": "ns:nightly" })).await;
            assert_eq!(
                all,
                json!({
                    "ok": true,
                    "running": false,
                    "status": "done",
                    "startedAtMs": 1,
                    "mode": "script",
                    "chunk": "hello world",
                    "nextOffset": 11,
                    "eof": true,
                })
            );
            let tail_end = ok(
                r,
                "agent_ops_tail_run",
                json!({ "jobId": "ns:nightly", "offset": 6 }),
            )
            .await;
            assert_eq!(tail_end["chunk"], "world");
            assert_eq!(tail_end["nextOffset"], 11);
            let snake = ok(
                r,
                "agent_ops_tail_run",
                json!({ "job_id": "ns:nightly", "offset": 6 }),
            )
            .await;
            assert_eq!(snake, tail_end);
            // Past EOF: empty, the offset held.
            let past = ok(
                r,
                "agent_ops_tail_run",
                json!({ "jobId": "ns:nightly", "offset": 100 }),
            )
            .await;
            assert_eq!(
                (past["chunk"].as_str(), past["nextOffset"].as_u64()),
                (Some(""), Some(100))
            );
            // No run on disk for this job: ok, empty, nulls.
            let never = ok(r, "agent_ops_tail_run", json!({ "jobId": "other" })).await;
            assert_eq!(
                (never["ok"].as_bool(), never["status"].is_null()),
                (Some(true), true)
            );

            // A marker planted OUTSIDE the runs dir must not be reachable via
            // the job id — the shared core refuses the id itself.
            std::fs::write(
                d.home.join(".agent-ops/evil.marker.json"),
                json!({ "status": "running", "startedAtMs": 7, "mode": "agent" }).to_string(),
            )
            .unwrap();
            let abs = d.home.join(".agent-ops/evil");
            for bad in [
                "../evil",
                "../../.agent-ops/evil",
                abs.to_str().unwrap(),
                "a/b",
                "..\\evil",
                "",
            ] {
                let got = ok(r, "agent_ops_tail_run", json!({ "jobId": bad })).await;
                assert_eq!(
                    got,
                    json!({
                        "ok": false,
                        "code": "io_error",
                        "status": null,
                        "error": "job id escapes the runs directory",
                    }),
                    "job id {bad:?}"
                );
            }

            // A marker whose tailPath escapes is refused too (pre-existing check).
            let secret = d.home.join("secret.txt");
            std::fs::write(&secret, "do not read").unwrap();
            std::fs::write(
                runs.join("leak.marker.json"),
                json!({ "status": "done", "mode": "script", "tailPath": secret.to_str().unwrap() })
                    .to_string(),
            )
            .unwrap();
            let leak = ok(r, "agent_ops_tail_run", json!({ "jobId": "leak" })).await;
            assert_eq!(leak["code"], "io_error");
            assert_eq!(leak["error"], "tail path escapes the runs directory");

            let e = err(r, "agent_ops_tail_run", json!({})).await;
            assert!(e.contains("`jobId` is required"), "{e}");
            let e = err(
                r,
                "agent_ops_tail_run",
                json!({ "jobId": "ns:nightly", "offset": -1 }),
            )
            .await;
            assert!(e.contains("`offset` must be a non-negative integer"), "{e}");
        }

        #[tokio::test]
        async fn agent_ops_without_a_home_answer_the_desktop_io_error() {
            let r = bare(None);
            let home_missing = json!({
                "ok": false,
                "code": "io_error",
                "status": null,
                "error": "home directory not found",
            });
            assert_eq!(ok(&r, "agent_ops_list_jobs", json!({})).await, home_missing);
            assert_eq!(
                ok(&r, "agent_ops_tail_run", json!({ "jobId": "x" })).await,
                home_missing
            );
            assert_eq!(
                ok(&r, "agent_ops_delete_job", json!({ "jobId": "x" })).await,
                home_missing
            );
        }

        // ── identity ─────────────────────────────────────────────────────────

        #[tokio::test]
        async fn os_username_is_the_daemon_process_user() {
            let r = bare(None);
            let got = ok(&r, "os_username", json!({})).await;
            let name = got.as_str().expect("a string");
            assert!(!name.is_empty());
            assert_eq!(name, identity::os_username());
        }
    }

    mod slice6 {
        //! The approve-gate queue, the pkg violation audit and `pkg_db_diag`
        //! over `/api/rpc`, against the router's temp `--data-dir`. Nothing
        //! here reaches the network: the router home has no
        //! `~/.agent-ops/daemon.lock`, so the commit / retry wake stops at the
        //! lock read (asserted below through the same helper the arms spawn).

        use super::*;
        use crate::server::shared::{pa_actions, pkg_db};

        fn draft(id: &str, scheduled_at: Option<&str>) -> Value {
            json!({
                "id": id,
                "channel": "email",
                "scheduledAt": scheduled_at,
                "payload": { "subject": format!("s-{id}"), "body": "b" }
            })
        }

        async fn status_of(db: &PaDb, id: &str) -> (String, Option<String>, Option<String>) {
            let pool = db.ensure_pool().await.unwrap();
            sqlx::query_as(
                "SELECT status, committed_at, edited_json FROM pa_action_drafts WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }

        async fn set_status(db: &PaDb, id: &str, status: &str) {
            let pool = db.ensure_pool().await.unwrap();
            sqlx::query(
                "UPDATE pa_action_drafts SET status = ?, claimed_at = 'x', error_text = 'boom' \
                 WHERE id = ?",
            )
            .bind(status)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }

        /// The rows' ids, sorted: `created_at` has one-second resolution, so
        /// rows paused in the same second have no defined order.
        fn ids(rows: &Value) -> Vec<String> {
            let mut ids: Vec<String> = rows
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect();
            ids.sort();
            ids
        }

        #[tokio::test]
        async fn every_arm_without_data_dir_is_no_db() {
            let r = bare(None);
            for (cmd, args) in [
                (
                    "pa_actions_pause",
                    json!({ "batchId": "b", "actionId": "a", "drafts": [draft("d", None)] }),
                ),
                ("pa_actions_list", json!({})),
                ("pa_actions_update", json!({ "draftId": "d", "patch": {} })),
                ("pa_actions_commit", json!({ "draftId": "d" })),
                ("pa_actions_retry", json!({ "draftId": "d" })),
                ("pa_actions_reject", json!({ "draftId": "d" })),
                ("pkg_permission_violations_list", json!({})),
                ("pkg_permission_violations_clear", json!({ "pkgId": "p" })),
                ("pkg_db_diag", json!({})),
            ] {
                let e = err(&r, cmd, args).await;
                assert_eq!(e, format!("{cmd}: {}", crate::server::rpc::NO_DB), "{cmd}");
            }
        }

        /// pause → list → update → commit, and reject, in the desktop shape.
        #[tokio::test]
        async fn the_gate_lifecycle_matches_the_desktop() {
            let d = daemon();
            let r = &d.router;
            let n = ok(
                r,
                "pa_actions_pause",
                json!({
                    "batchId": "b1",
                    "actionId": "mail.send",
                    "drafts": [draft("d1", Some("2026-06-09T07:00:00+01:00")), draft("d2", None)]
                }),
            )
            .await;
            assert_eq!(n, 2);
            // The snake_case spelling lands too.
            let n = ok(
                r,
                "pa_actions_pause",
                json!({ "batch_id": "b2", "action_id": "mail.send", "drafts": [draft("d3", None)] }),
            )
            .await;
            assert_eq!(n, 1);

            let listed = ok(r, "pa_actions_list", json!({})).await;
            assert_eq!(ids(&listed), ["d1", "d2", "d3"]);
            let d1 = listed
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "d1")
                .unwrap();
            assert_eq!(d1["batchId"], "b1");
            assert_eq!(d1["actionId"], "mail.send");
            assert_eq!(d1["status"], "awaiting");
            assert_eq!(d1["attempts"], 0);
            // Normalised to SQLite UTC at pause time (DEC-10).
            assert_eq!(d1["scheduledAt"], "2026-06-09 06:00:00");
            let payload: Value = serde_json::from_str(d1["payloadJson"].as_str().unwrap()).unwrap();
            assert_eq!(payload["subject"], "s-d1");
            // Shape parity: the core the desktop command calls, same db.
            let direct = pa_actions::list(&d.db, None).await.unwrap();
            assert_eq!(listed, serde_json::to_value(&direct).unwrap());
            assert_eq!(
                ok(r, "pa_actions_list", json!({ "status": null })).await,
                listed
            );

            // update: awaiting → edited, edits stored verbatim.
            let patch = json!({ "subject": "better" });
            assert_eq!(
                ok(
                    r,
                    "pa_actions_update",
                    json!({ "draftId": "d1", "patch": patch })
                )
                .await,
                Value::Null
            );
            let (status, _, edited) = status_of(&d.db, "d1").await;
            assert_eq!(status, "edited");
            assert_eq!(edited.as_deref(), Some(r#"{"subject":"better"}"#));
            ok(
                r,
                "pa_actions_update",
                json!({ "draft_id": "d1", "patch": { "body": "b2" } }),
            )
            .await;
            assert_eq!(status_of(&d.db, "d1").await.0, "edited");

            // commit (both spellings), the DB transition is the desktop's.
            assert_eq!(
                ok(r, "pa_actions_commit", json!({ "draftId": "d1" })).await,
                Value::Null
            );
            let (status, committed_at, _) = status_of(&d.db, "d1").await;
            assert_eq!(status, "committed");
            assert!(committed_at.is_some());
            ok(r, "pa_actions_commit", json!({ "draft_id": "d2" })).await;
            assert_eq!(status_of(&d.db, "d2").await.0, "committed");

            // reject (both spellings); rejected rows leave the default list.
            ok(r, "pa_actions_reject", json!({ "draftId": "d3" })).await;
            ok(r, "pa_actions_reject", json!({ "draft_id": "d2" })).await;
            assert_eq!(ids(&ok(r, "pa_actions_list", json!({})).await), ["d1"]);
            let rejected = ok(r, "pa_actions_list", json!({ "status": "rejected" })).await;
            assert_eq!(ids(&rejected), ["d2", "d3"]);
            let direct = pa_actions::list(&d.db, Some("rejected")).await.unwrap();
            assert_eq!(rejected, serde_json::to_value(&direct).unwrap());

            // The wake the commit spawned: no lock under the router home, so
            // it stopped at the lock read — no POST was attempted.
            let wake = pa_actions::wake_send_worker(Some(d.home.clone()))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(wake["code"], "daemon_down");
            assert!(
                wake["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("read daemon.lock"),
                "{wake}"
            );
            assert!(!d.home.join(".agent-ops").exists());
        }

        #[tokio::test]
        async fn retry_requeues_only_a_failed_row() {
            let d = daemon();
            let r = &d.router;
            ok(
                r,
                "pa_actions_pause",
                json!({ "batchId": "b", "actionId": "a", "drafts": [draft("f", None)] }),
            )
            .await;
            let e = err(r, "pa_actions_retry", json!({ "draftId": "f" })).await;
            assert_eq!(
                e,
                "pa_actions_retry: draft f not found or not in failed state"
            );

            set_status(&d.db, "f", "failed").await;
            assert_eq!(
                ok(r, "pa_actions_retry", json!({ "draftId": "f" })).await,
                Value::Null
            );
            let row = &ok(r, "pa_actions_list", json!({})).await[0];
            assert_eq!(row["status"], "committed");
            assert_eq!(row["claimedAt"], Value::Null);
            assert_eq!(row["errorText"], Value::Null);
            assert!(row["committedAt"].is_string());

            set_status(&d.db, "f", "failed").await;
            ok(r, "pa_actions_retry", json!({ "draft_id": "f" })).await;
            assert_eq!(status_of(&d.db, "f").await.0, "committed");
            // Failed rows can be rejected too (the desktop's clause).
            set_status(&d.db, "f", "failed").await;
            ok(r, "pa_actions_reject", json!({ "draftId": "f" })).await;
            assert_eq!(status_of(&d.db, "f").await.0, "rejected");
        }

        /// Each command's status guard answers exactly as the desktop's.
        #[tokio::test]
        async fn the_status_guards_are_the_desktops() {
            let d = daemon();
            let r = &d.router;
            ok(
                r,
                "pa_actions_pause",
                json!({ "batchId": "b", "actionId": "a", "drafts": [
                    draft("rej", None), draft("com", None), draft("snd", None)
                ] }),
            )
            .await;
            ok(r, "pa_actions_reject", json!({ "draftId": "rej" })).await;
            ok(r, "pa_actions_commit", json!({ "draftId": "com" })).await;
            set_status(&d.db, "snd", "sending").await;

            for (cmd, id, why) in [
                ("pa_actions_commit", "rej", "not found or not committable"),
                ("pa_actions_commit", "com", "not found or not committable"),
                ("pa_actions_commit", "snd", "not found or not committable"),
                ("pa_actions_update", "rej", "not found or not editable"),
                ("pa_actions_update", "com", "not found or not editable"),
                (
                    "pa_actions_retry",
                    "rej",
                    "not found or not in failed state",
                ),
                (
                    "pa_actions_retry",
                    "com",
                    "not found or not in failed state",
                ),
                ("pa_actions_reject", "rej", "not found or already terminal"),
                ("pa_actions_reject", "snd", "not found or already terminal"),
                // Unknown ids: the same desktop errors.
                ("pa_actions_commit", "nope", "not found or not committable"),
                ("pa_actions_update", "nope", "not found or not editable"),
                (
                    "pa_actions_retry",
                    "nope",
                    "not found or not in failed state",
                ),
                ("pa_actions_reject", "nope", "not found or already terminal"),
            ] {
                let e = err(r, cmd, json!({ "draftId": id, "patch": {} })).await;
                assert_eq!(e, format!("{cmd}: draft {id} {why}"), "{cmd} {id}");
            }
            // Nothing moved.
            assert_eq!(status_of(&d.db, "rej").await.0, "rejected");
            assert_eq!(status_of(&d.db, "com").await.0, "committed");
            assert_eq!(status_of(&d.db, "snd").await.0, "sending");
            // A committed row is still rejectable (the worker has not claimed it).
            ok(r, "pa_actions_reject", json!({ "draftId": "com" })).await;
            assert_eq!(status_of(&d.db, "com").await.0, "rejected");
        }

        #[tokio::test]
        async fn pause_refusals_are_the_desktops_and_atomic() {
            let d = daemon();
            let r = &d.router;
            // The desktop's own string, not "pa_actions_pause: pa_actions_pause: …".
            let e = err(
                r,
                "pa_actions_pause",
                json!({ "batchId": "b", "actionId": "a", "drafts": [] }),
            )
            .await;
            assert_eq!(e, "pa_actions_pause: drafts cannot be empty");
            // A duplicate id fails the insert and rolls the whole batch back.
            let e = err(
                r,
                "pa_actions_pause",
                json!({ "batchId": "b", "actionId": "a", "drafts": [draft("x", None), draft("x", None)] }),
            )
            .await;
            assert!(e.starts_with("pa_actions_pause: insert draft x: "), "{e}");
            assert_eq!(ok(r, "pa_actions_list", json!({})).await, json!([]));

            let e = err(
                r,
                "pa_actions_pause",
                json!({ "actionId": "a", "drafts": [] }),
            )
            .await;
            assert!(e.contains("`batchId` is required"), "{e}");
            let e = err(
                r,
                "pa_actions_pause",
                json!({ "batchId": "b", "actionId": "a", "drafts": [{ "id": "y" }] }),
            )
            .await;
            assert!(e.contains("invalid `drafts`"), "{e}");
            let e = err(r, "pa_actions_update", json!({ "draftId": "x" })).await;
            assert!(e.contains("`patch` is required"), "{e}");
            let e = err(r, "pa_actions_commit", json!({})).await;
            assert!(e.contains("`draftId` is required"), "{e}");
        }

        // ── pkg permission violations / pkg_db_diag ─────────────────────────

        async fn violation(db: &PaDb, pkg: &str, attempted: &str, at: i64) {
            let pool = db.ensure_pool().await.unwrap();
            sqlx::query(
                "INSERT INTO pkg_permission_violations
                 (pkg_id, scope_kind, attempted, declared, occurred_at)
                 VALUES (?, 'shell.execute', ?, 'git', ?)",
            )
            .bind(pkg)
            .bind(attempted)
            .bind(at)
            .execute(&pool)
            .await
            .unwrap();
        }

        #[tokio::test]
        async fn violations_list_and_clear_in_the_desktop_shape() {
            let d = daemon();
            let r = &d.router;
            assert_eq!(
                ok(r, "pkg_permission_violations_list", json!({})).await,
                json!([])
            );
            violation(&d.db, "p1", "a", 1).await;
            violation(&d.db, "p2", "b", 2).await;
            violation(&d.db, "p1", "c", 3).await;

            let all = ok(
                r,
                "pkg_permission_violations_list",
                json!({ "pkgId": null, "limit": null }),
            )
            .await;
            let attempted: Vec<&str> = all
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["attempted"].as_str().unwrap())
                .collect();
            assert_eq!(attempted, ["c", "b", "a"], "newest first");
            assert_eq!(all[0]["pkg_id"], "p1");
            assert_eq!(all[0]["scope_kind"], "shell.execute");
            let direct = pkg_db::violations_list(&d.db, None, None).await.unwrap();
            assert_eq!(all, serde_json::to_value(&direct).unwrap());

            let camel = ok(
                r,
                "pkg_permission_violations_list",
                json!({ "pkgId": "p1", "limit": 1 }),
            )
            .await;
            let snake = ok(
                r,
                "pkg_permission_violations_list",
                json!({ "pkg_id": "p1", "limit": 1 }),
            )
            .await;
            assert_eq!(camel, snake);
            assert_eq!(camel.as_array().unwrap().len(), 1);
            assert_eq!(camel[0]["attempted"], "c");
            // The desktop's clamp: 0 → 1.
            let clamped = ok(r, "pkg_permission_violations_list", json!({ "limit": 0 })).await;
            assert_eq!(clamped.as_array().unwrap().len(), 1);
            let e = err(r, "pkg_permission_violations_list", json!({ "limit": 1.5 })).await;
            assert!(e.contains("invalid `limit`"), "{e}");

            // Unknown pkg: 0 deleted — the desktop's answer, not an error.
            assert_eq!(
                ok(
                    r,
                    "pkg_permission_violations_clear",
                    json!({ "pkgId": "nope" })
                )
                .await,
                0
            );
            assert_eq!(
                ok(
                    r,
                    "pkg_permission_violations_clear",
                    json!({ "pkgId": "p1" })
                )
                .await,
                2
            );
            assert_eq!(
                ok(
                    r,
                    "pkg_permission_violations_clear",
                    json!({ "pkg_id": "p2" })
                )
                .await,
                1
            );
            assert_eq!(
                ok(r, "pkg_permission_violations_list", json!({})).await,
                json!([])
            );
            let e = err(r, "pkg_permission_violations_clear", json!({})).await;
            assert!(e.contains("`pkgId` is required"), "{e}");
        }

        #[tokio::test]
        async fn pkg_db_diag_reports_the_daemons_own_db() {
            let d = daemon();
            let r = &d.router;
            let empty = ok(r, "pkg_db_diag", json!({})).await;
            assert_eq!(
                empty,
                json!({
                    "db_path": d.data.join("ikenga.db").display().to_string(),
                    "pkg_installed_count": 0,
                    "ids": [],
                })
            );
            let pool = d.db.ensure_pool().await.unwrap();
            for id in ["com.b", "com.a"] {
                sqlx::query(
                    "INSERT INTO pkg_installed
                     (id, version, ikenga_api, manifest_json, install_path, installed_at)
                     VALUES (?, '1.0.0', '1', '{}', '/x', 0)",
                )
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            }
            let diag = ok(r, "pkg_db_diag", json!({})).await;
            assert_eq!(diag["pkg_installed_count"], 2);
            assert_eq!(diag["ids"], json!(["com.a", "com.b"]));
            let direct = pkg_db::db_diag(&d.db).await.unwrap();
            assert_eq!(diag, serde_json::to_value(&direct).unwrap());
        }
    }

    mod chi_write {
        //! WP-P10: `chi_run` / `chi_resume` / `chi_cancel` over `/api/rpc`.
        //! The engine is the stub `claude` from `chi_exec`'s tests, injected
        //! through `router_with_chi`, so nothing here needs a real CLI.

        use super::*;
        use crate::server::rpc_local::{chi_run_opts, DaemonChi};
        use crate::server::shared::chi_exec;
        use crate::server::shared::chi_exec::tests::wait_for;

        struct ChiDaemon {
            _tmp: tempfile::TempDir,
            home: PathBuf,
            db: Arc<PaDb>,
            router: Router,
        }

        fn chi_daemon() -> ChiDaemon {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let (data, home) = (root.join("data"), root.join("home"));
            for d in [&data, &home] {
                std::fs::create_dir_all(d).unwrap();
            }
            let db = Arc::new(PaDb::new(data.join("ikenga.db")));
            #[cfg(unix)]
            let resolver: Arc<dyn chi_exec::EngineResolver> =
                Arc::new(chi_exec::tests::StubResolver(chi_exec::tests::stub_claude()));
            #[cfg(not(unix))]
            let resolver: Arc<dyn chi_exec::EngineResolver> = Arc::new(chi_exec::HostResolver);
            let chi = Arc::new(DaemonChi {
                runtime: Arc::new(chi_exec::ChiRuntime::new()),
                resolver,
            });
            let router = crate::server::router_with_chi(
                config(Some(data), None),
                Some(db.clone()),
                Some(home.clone()),
                chi,
            );
            ChiDaemon {
                _tmp: tmp,
                home,
                db,
                router,
            }
        }

        async fn status_of(r: &Router, run_id: &str) -> Value {
            ok(r, "chi_status", json!({ "runId": run_id })).await
        }

        #[test]
        fn chi_run_opts_accepts_nested_flat_camel_and_snake() {
            let nested = chi_run_opts(&json!({ "opts": {
                "engineId": "claude-code", "prompt": "p", "cwd": "/w", "model": "m",
                "mode": "plan", "timeoutSeconds": 30, "parentId": "parent",
                "resumeSessionId": "sess", "persistent": true,
            }}))
            .unwrap();
            assert_eq!(nested.engine_id, "claude-code");
            assert_eq!(nested.prompt, "p");
            assert_eq!(nested.cwd.as_deref(), Some("/w"));
            assert_eq!(nested.model.as_deref(), Some("m"));
            assert_eq!(nested.mode.as_deref(), Some("plan"));
            assert_eq!(nested.timeout_seconds, Some(30));
            assert_eq!(nested.parent_id.as_deref(), Some("parent"));
            assert_eq!(nested.resume_session_id.as_deref(), Some("sess"));
            assert!(nested.persistent);

            let flat = chi_run_opts(&json!({
                "engine_id": "codex", "prompt": "q", "timeout_seconds": 5,
                "parent_id": "pp", "resume_session_id": "s2", "cwd": null,
            }))
            .unwrap();
            assert_eq!(flat.engine_id, "codex");
            assert_eq!(flat.timeout_seconds, Some(5));
            assert_eq!(flat.parent_id.as_deref(), Some("pp"));
            assert_eq!(flat.resume_session_id.as_deref(), Some("s2"));
            assert_eq!(flat.cwd, None, "null reads as absent");
            assert!(!flat.persistent, "persistent defaults to false");

            // `opts: null` falls back to the flat fields.
            let null_opts =
                chi_run_opts(&json!({ "opts": null, "engineId": "pi", "prompt": "r" })).unwrap();
            assert_eq!(null_opts.engine_id, "pi");

            let e = |args: Value| chi_run_opts(&args).err().unwrap();
            assert_eq!(e(json!({ "opts": "x" })), "`opts` must be an object");
            assert_eq!(
                e(json!({ "opts": { "prompt": "p" } })),
                "`engineId` is required"
            );
            assert_eq!(e(json!({ "engineId": "x" })), "`prompt` is required");
            assert_eq!(
                e(json!({ "engineId": "x", "prompt": "p", "persistent": "yes" })),
                "`persistent` must be a boolean"
            );
            assert_eq!(
                e(json!({ "engineId": "x", "prompt": "p", "timeoutSeconds": 1u64 << 40 })),
                "`timeoutSeconds` must fit in 32 bits"
            );
            assert_eq!(
                e(json!({ "engineId": "x", "prompt": "p", "timeoutSeconds": -1 })),
                "`timeoutSeconds` must be a non-negative integer"
            );
            assert_eq!(
                e(json!({ "engineId": 7, "prompt": "p" })),
                "`engineId` must be a string"
            );
        }

        #[tokio::test]
        async fn chi_write_arms_need_a_data_dir() {
            let r = bare(None);
            let run = err(
                &r,
                "chi_run",
                json!({ "opts": { "engineId": "claude-code", "prompt": "hi" } }),
            )
            .await;
            assert_eq!(run, format!("chi_run: {}", crate::server::rpc::NO_DB));
            for cmd in ["chi_resume", "chi_cancel"] {
                let e = err(&r, cmd, json!({ "runId": "r1", "prompt": "x" })).await;
                assert_eq!(e, format!("{cmd}: {}", crate::server::rpc::NO_DB));
            }
        }

        #[tokio::test]
        async fn chi_write_arms_reject_missing_args() {
            let d = chi_daemon();
            let r = &d.router;
            assert_eq!(
                err(r, "chi_run", json!({})).await,
                "chi_run: `engineId` is required"
            );
            assert_eq!(
                err(r, "chi_resume", json!({ "runId": "x" })).await,
                "chi_resume: `prompt` is required"
            );
            assert_eq!(
                err(r, "chi_resume", json!({ "prompt": "x" })).await,
                "chi_resume: `runId` is required"
            );
            assert_eq!(
                err(r, "chi_cancel", json!({})).await,
                "chi_cancel: `runId` is required"
            );
            for cmd in ["chi_resume", "chi_cancel"] {
                assert_eq!(
                    err(r, cmd, json!({ "run_id": "nope", "prompt": "x" })).await,
                    format!("{cmd}: chi run not found: nope")
                );
            }
        }

        /// An engine the daemon cannot run fails the call AND closes its row
        /// as `failed`, so `chi_list` says why.
        #[tokio::test]
        async fn an_engine_that_cannot_start_leaves_a_failed_row() {
            let d = chi_daemon();
            let r = &d.router;
            for (engine, want) in [
                ("cursor-agent", "cursor-agent runtime not implemented"),
                ("openrouter", chi_exec::HEADLESS_OPENROUTER),
                (
                    "no-such-engine",
                    "engine not yet supported by iyke chi: no-such-engine",
                ),
            ] {
                let e = err(
                    r,
                    "chi_run",
                    json!({ "opts": { "engineId": engine, "prompt": "hi", "cwd": "/tmp" } }),
                )
                .await;
                assert!(e.starts_with("chi_run: ") && e.contains(want), "{e}");
                let rows = ok(r, "chi_list", json!({ "engineId": engine })).await;
                let rows = rows.as_array().unwrap();
                assert_eq!(rows.len(), 1, "{engine}");
                assert_eq!(rows[0]["status"], "failed", "{engine}");
            }
        }

        /// A `cwd` sent to the daemon is never `$VAR`-expanded against the
        /// daemon's own environment (which holds the operator's
        /// `IKENGA_SECRET_*` defaults): `<home>/$HOME` is that literal
        /// directory. Before the fix it became `<home>/<the daemon's HOME>`,
        /// which does not exist, and the run failed to start.
        #[cfg(unix)]
        #[tokio::test]
        async fn a_daemon_run_cwd_is_not_expanded_against_the_daemon_env() {
            let d = chi_daemon();
            let r = &d.router;
            let literal = d.home.join("$HOME");
            std::fs::create_dir_all(&literal).unwrap();
            let started = ok(
                r,
                "chi_run",
                json!({ "opts": {
                    "engineId": "claude-code",
                    "prompt": "hello",
                    "cwd": literal.to_string_lossy(),
                } }),
            )
            .await;
            let run_id = started["run_id"].as_str().unwrap().to_string();
            assert!(wait_for(|| async { status_of(r, &run_id).await["status"] == "done" }).await);
            let out = status_of(r, &run_id).await["output"]
                .as_str()
                .unwrap()
                .to_string();
            assert!(
                out.contains(&format!("cwd={} ", literal.display())),
                "the engine ran in the literal directory: {out}"
            );
        }

        /// The whole loop a bridge drives: run → status → list → resume →
        /// cancel, nested camelCase and flat snake_case args alike.
        #[cfg(unix)]
        #[tokio::test]
        async fn chi_run_status_resume_cancel_round_trip() {
            let d = chi_daemon();
            let r = &d.router;

            let started = ok(
                r,
                "chi_run",
                json!({ "opts": { "engineId": "claude-code", "prompt": "hello" } }),
            )
            .await;
            assert_eq!(started["status"], "running");
            assert_eq!(started["error"], Value::Null);
            let run_id = started["run_id"].as_str().unwrap().to_string();
            assert!(wait_for(|| async { status_of(r, &run_id).await["status"] == "done" }).await);
            let done = status_of(r, &run_id).await;
            let out = done["output"].as_str().unwrap();
            assert!(out.contains("--permission-mode default"), "{out}");
            assert!(!out.contains("--resume"), "{out}");

            // The row: owner `cli`, the engine's session id, and — no cwd was
            // given — the router home as the run's working directory.
            let list = ok(r, "chi_list", json!({ "engine_id": "claude-code" })).await;
            let row = list
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["run_id"] == run_id.as_str())
                .expect("the run is listed")
                .clone();
            assert_eq!(row["owner"], "cli");
            assert_eq!(row["external_id"], "sess-stub");
            let row = crate::server::shared::chi::cache_get(&d.db, &run_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.cwd, None, "the row keeps what the caller sent");
            let output_path = PathBuf::from(row.output_path.unwrap());
            assert!(output_path.starts_with(d.home.parent().unwrap().join("data/chi-cache")));

            // Resume (snake_case): same id, relaunched with --resume.
            let resumed = ok(
                r,
                "chi_resume",
                json!({ "run_id": run_id, "prompt": "and again" }),
            )
            .await;
            assert_eq!(resumed["run_id"], run_id.as_str());
            assert_eq!(resumed["status"], "running");
            assert!(
                wait_for(|| async {
                    let s = status_of(r, &run_id).await;
                    s["status"] == "done"
                        && s["output"]
                            .as_str()
                            .is_some_and(|o| o.contains("--resume sess-stub"))
                })
                .await
            );

            // Cancel (flat snake_case run): a sleeping run is killed.
            let sleeper = ok(
                r,
                "chi_run",
                json!({ "engine_id": "claude-code", "prompt": "please sleep" }),
            )
            .await;
            let sleeper_id = sleeper["run_id"].as_str().unwrap().to_string();
            let cancelled = ok(r, "chi_cancel", json!({ "runId": sleeper_id })).await;
            assert_eq!(cancelled["status"], "cancelled");
            assert_eq!(cancelled["run_id"], sleeper_id.as_str());
            assert_eq!(status_of(r, &sleeper_id).await["status"], "cancelled");
        }

        /// Under T1 each principal is its own daemon process (its own child,
        /// `--data-dir`, `ikenga.db` and chi-cache). A run id from one is
        /// unknown to another: no read, resume or cancel crosses, and the
        /// answer is the same "not found" an unknown id gets (no existence
        /// oracle). The process-level half — the child's uid — is the
        /// `t1-root` broker test.
        #[tokio::test]
        async fn another_principals_runs_are_not_found() {
            let ada = chi_daemon();
            let bob = chi_daemon();
            // Any run will do; cursor-agent leaves a (failed) row without
            // needing an engine.
            let _ = err(
                &ada.router,
                "chi_run",
                json!({ "opts": { "engineId": "cursor-agent", "prompt": "hi", "cwd": "/tmp" } }),
            )
            .await;
            let rows = ok(
                &ada.router,
                "chi_list",
                json!({ "engineId": "cursor-agent" }),
            )
            .await;
            let ada_run = rows[0]["run_id"].as_str().unwrap().to_string();

            let not_found = format!("chi run not found: {ada_run}");
            assert_eq!(
                err(&bob.router, "chi_status", json!({ "runId": ada_run })).await,
                format!("chi_status: {not_found}")
            );
            assert_eq!(
                err(
                    &bob.router,
                    "chi_resume",
                    json!({ "runId": ada_run, "prompt": "x" })
                )
                .await,
                format!("chi_resume: {not_found}")
            );
            assert_eq!(
                err(&bob.router, "chi_cancel", json!({ "runId": ada_run })).await,
                format!("chi_cancel: {not_found}")
            );
            let bob_rows = ok(
                &bob.router,
                "chi_list",
                json!({ "engineId": "cursor-agent" }),
            )
            .await;
            assert_eq!(bob_rows, json!([]));
            // Ada's run is untouched.
            assert_eq!(status_of(&ada.router, &ada_run).await["status"], "failed");
        }
    }
}
