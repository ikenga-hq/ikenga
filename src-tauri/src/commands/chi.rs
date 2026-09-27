//! Chi-first agent surface.
//!
//! WP-01: a thin cache-backed command surface for running, resuming, listing,
//! and cancelling agent sessions.
//! WP-02: wires the Claude Code engine so `iyke chi run` / `resume` / `cancel`
//! and `list` / `status` actually spawn, monitor, and read the agent child.
//! WP-07: multi-engine parity — Codex (`codex exec --json`) wired;
//!         cursor-agent returns `RUNTIME_NOT_IMPLEMENTED` cleanly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::{ContentBlock, PromptResponse, SessionUpdate, StopReason};
use serde::Deserialize;
use sqlx::Row;
use tauri::{AppHandle, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::claude::event::ChatEvent;
use crate::claude::stream_parser::StreamParser;
use crate::commands::chi_runner::{self, RunLiveness};
use crate::commands::claude::claude_list_sessions;
use crate::commands::db::PaDb;
use crate::engines::claude_code::mode::AcpSessionMode;
use crate::engines::codex_pty::parser as codex_parser;
use crate::engines::{EngineHandle, EngineRegistryState, OpenRouterHttpEngineState};
#[cfg(windows)]
use crate::platform::NoConsoleWindow;

/// Cache state. Lives in `app_data_dir` and is `.manage()`d in `lib.rs`.
#[derive(Clone, Debug)]
pub struct ChiCache {
    app_data_dir: PathBuf,
}

impl ChiCache {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self { app_data_dir }
    }

    /// `<app-data-dir>/chi-cache/`
    pub fn cache_dir(&self) -> PathBuf {
        self.app_data_dir.join(chi_read::CACHE_DIR)
    }

    /// Per-run artifact / output tail file.
    pub fn run_output_path(&self, run_id: &str) -> PathBuf {
        self.cache_dir().join(format!("{run_id}.json"))
    }

    /// Ensure the JSON cache directory exists.
    pub fn ensure_cache_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.cache_dir()).map_err(|e| format!("chi-cache dir: {e}"))
    }
}

/// Runtime state for live Chi children. `.manage()`d in `lib.rs`.
#[derive(Default, Clone)]
pub struct ChiRuntime {
    running: Arc<Mutex<HashMap<String, Arc<ChiRunHandle>>>>,
}

impl ChiRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, run_id: &str, handle: Arc<ChiRunHandle>) {
        self.running.lock().await.insert(run_id.to_string(), handle);
    }

    pub async fn remove(&self, run_id: &str) -> Option<Arc<ChiRunHandle>> {
        self.running.lock().await.remove(run_id)
    }
}

/// Handle to a live Chi run so `chi_cancel` can interrupt it.
pub struct ChiRunHandle {
    /// The actual OS child process. Shared with the reader task. `None` for
    /// CLI-less engines driven in-process — there is no child to kill, and
    /// `in_process` is the interrupt instead.
    pub child: Option<Arc<Mutex<Child>>>,
    /// Set to true by `chi_cancel`, read (and cleared) by the reader task.
    pub cancelled: Arc<AtomicBool>,
    /// Cancel hook for in-process engines (no child process to kill).
    pub in_process: Option<InProcessCancel>,
}

/// How `chi_cancel` interrupts a run that has no OS child.
#[derive(Clone)]
pub enum InProcessCancel {
    /// The OpenRouter HTTP adapter: `handle_cancel` flags the session and
    /// triggers the abort signal the streaming read selects on.
    OpenRouter {
        engine: OpenRouterHttpEngineState,
        thread_id: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChiRunOpts {
    pub engine_id: String,
    pub prompt: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    #[allow(dead_code)]
    pub timeout_seconds: Option<u32>,
    pub parent_id: Option<String>,
    #[serde(rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    /// If true, launch the run as a detached `chi-runner` (WP-18b) so it
    /// survives an app restart. Falls back to in-process when chi-runner
    /// can't be found or spawned. The runner's pid is stored in
    /// `chi_cache.pid`.
    #[serde(default)]
    pub persistent: bool,
}

/// Rebuild a run's opts from its cache row for `chi_resume` — only the fields
/// the resume paths actually read (engine id, prompt, model, mode).
fn row_into_resume_opts(row: &ChiCacheRow, prompt: String) -> ChiRunOpts {
    ChiRunOpts {
        engine_id: row.engine_id.clone(),
        prompt,
        cwd: None,
        model: row.model.clone(),
        mode: row.mode.clone(),
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: false,
    }
}

/// The run / row shapes and the read path (`cache_get`, the output file,
/// `chi_status`'s liveness read) live in the ungated `server::shared::chi`
/// so the daemon's `chi_status` / `chi_list` arms serve the same JSON
/// (WP-19 slice 3). Re-exported: `iyke::handlers` names them through here.
pub use crate::server::shared::chi::{ChiCacheRow, ChiRunResult};
use crate::server::shared::chi::{
    self as chi_read, cache_get, pid_alive, read_output_file, resolve_output_path, RunOutputFile,
};

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn one_hour_from_now_iso() -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::hours(1))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Upsert a cache row from the options. Returns the run_id.
async fn cache_insert(
    db: &PaDb,
    run_id: &str,
    opts: &ChiRunOpts,
    output_path: &Path,
    owner: &str,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    let expires = one_hour_from_now_iso();
    let output_path_string = output_path.to_string_lossy().to_string();

    sqlx::query(
        "INSERT INTO chi_cache (
            run_id, engine_id, external_id, brief, cwd, model, mode, status,
            output_path, output_truncated, error, artifacts, parent_id, owner,
            pid, started_at, ended_at, last_seen_at, expires_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(&opts.engine_id)
    .bind(&opts.resume_session_id) // initial external_id is the resume id, if any
    .bind(&opts.prompt)
    .bind(&opts.cwd)
    .bind(&opts.model)
    .bind(&opts.mode)
    .bind("queued")
    .bind(output_path_string)
    .bind(0i64) // output_truncated; boolean stored as integer
    .bind::<Option<String>>(None)
    .bind::<Option<String>>(None) // artifacts as JSON string
    .bind(&opts.parent_id)
    .bind(owner)
    .bind::<Option<i64>>(None)
    .bind(&now)
    .bind::<Option<String>>(None)
    .bind(&now)
    .bind(&expires)
    .execute(&pool)
    .await
    .map_err(|e| format!("chi_cache insert: {e}"))?;
    Ok(())
}

async fn cache_update_status(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    sqlx::query("UPDATE chi_cache SET status = ?, error = ?, last_seen_at = ? WHERE run_id = ?")
        .bind(status)
        .bind(error)
        .bind(&now)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache update status: {e}"))?;
    Ok(())
}

async fn cache_update_external_id(
    db: &PaDb,
    run_id: &str,
    external_id: &str,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    sqlx::query("UPDATE chi_cache SET external_id = ?, last_seen_at = ? WHERE run_id = ?")
        .bind(external_id)
        .bind(&now)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache update external_id: {e}"))?;
    Ok(())
}

/// A run just handed to a detached chi-runner: `running`, with its pid.
async fn cache_mark_detached(db: &PaDb, run_id: &str, pid: u32) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "UPDATE chi_cache SET status = 'running', pid = ?, last_seen_at = ? WHERE run_id = ?",
    )
    .bind(i64::from(pid))
    .bind(now_iso())
    .bind(run_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("chi_cache record chi-runner pid: {e}"))?;
    Ok(())
}

/// A detached run being resumed in-process: the old runner's pid no longer
/// describes the run, and the sweep must not judge the new turn by it.
async fn cache_clear_pid(db: &PaDb, run_id: &str) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    sqlx::query("UPDATE chi_cache SET pid = NULL WHERE run_id = ?")
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache clear pid: {e}"))?;
    Ok(())
}

async fn cache_update_done(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    output_truncated: bool,
    artifacts: Option<&serde_json::Value>,
) -> Result<(), String> {
    cache_finish(
        db,
        run_id,
        status,
        error,
        output_truncated,
        artifacts,
        false,
    )
    .await
    .map(|_| ())
}

/// [`cache_update_done`] guarded by the status transition: the row is only
/// finished (and the notification only produced) while it is still `queued` /
/// `running`. Returns whether this call made the transition. The detached-run
/// sweep uses it so a run is notified exactly once however many sweeps, or a
/// racing `chi_cancel`, see it.
async fn cache_update_done_if_live(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<bool, String> {
    cache_finish(db, run_id, status, error, false, None, true).await
}

async fn cache_finish(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    output_truncated: bool,
    artifacts: Option<&serde_json::Value>,
    only_if_live: bool,
) -> Result<bool, String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    let ended = now_iso();
    let artifacts_json = artifacts.map(|v| v.to_string());
    let sql = if only_if_live {
        "UPDATE chi_cache SET
            status = ?, error = ?, output_truncated = ?, artifacts = ?,
            ended_at = ?, last_seen_at = ?
         WHERE run_id = ? AND status IN ('queued', 'running')"
    } else {
        "UPDATE chi_cache SET
            status = ?, error = ?, output_truncated = ?, artifacts = ?,
            ended_at = ?, last_seen_at = ?
         WHERE run_id = ?"
    };
    let res = sqlx::query(sql)
        .bind(status)
        .bind(error)
        .bind(output_truncated as i64)
        .bind(artifacts_json)
        .bind(&ended)
        .bind(&now)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache update done: {e}"))?;
    if only_if_live && res.rows_affected() == 0 {
        return Ok(false);
    }
    notify_run_terminal(db, run_id, status, error, artifacts).await;
    Ok(true)
}

/// WP-40 `run_finished` / `run_failed` producer. `cache_update_done` (and its
/// transition-guarded twin `cache_update_done_if_live`) is the single place a
/// Chi run reaches a terminal status (every engine's one-off task, spawn
/// failures, stdin failures, and — via [`reconcile_detached_runs`] — detached
/// chi-runner runs that finish out of process), so producing here covers them
/// all. `cancelled` produces nothing (a human did it). Best-effort. The run's
/// artifacts ride along (count + first path) so the row can "Open artifact".
///
/// Not covered (tracked WP-40 follow-up): agent-ops schedules (D-07's
/// "pulse-refresh finished" example rows) have no in-shell completion signal
/// at all yet.
async fn notify_run_terminal(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    artifacts: Option<&serde_json::Value>,
) {
    let row = match cache_get(db, run_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            log::warn!(target: "ikenga::chi", "chi run {run_id}: notification lookup failed: {e}");
            return;
        }
    };
    if let Some(new) = crate::notifications::producers::run_terminal_with_artifacts(
        run_id,
        status,
        &row.engine_id,
        row.brief.as_deref(),
        row.cwd.as_deref(),
        error,
        artifacts,
    ) {
        crate::notifications::record_with_db(db, new).await;
    }
}

/// Build the line-delimited user envelope that streaming-input mode expects.
fn user_envelope(text: &str) -> String {
    let value = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

/// How a headless Chi engine binary gets launched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EngineLaunch {
    /// A host executable. On Windows a resolved `.cmd` / `.bat` shim runs
    /// through `cmd.exe /c`.
    Native(PathBuf),
    /// Found only inside WSL (e.g. Claude Code installed in the distro, not
    /// on Windows). Launched as `wsl.exe --cd <cwd> -e bash -l -c '<bin> <args>'`
    /// — the same shape the interactive terminal uses (`src/terminal/claude-wrap.ts`).
    Wsl { binary: String },
}

/// Where engine binaries are looked up. A trait so tests can stand in for
/// the host PATH and WSL.
trait EngineResolver {
    /// Resolved host path of `binary`, if it is on the (augmented) PATH.
    fn native(&self, binary: &str) -> Option<PathBuf>;
    /// Whether `binary` is on the default WSL distro's login PATH.
    fn in_wsl(&self, binary: &str) -> bool;
}

/// The real resolver: augmented host PATH first, then WSL on Windows.
struct HostResolver;

impl EngineResolver for HostResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        let path = crate::runtime::augmented_path();
        let found = which::which_in(binary, Some(path), ".");
        #[cfg(windows)]
        let found = found
            .or_else(|_| which::which_in(format!("{binary}.cmd"), Some(path), "."))
            .or_else(|_| which::which_in(format!("{binary}.exe"), Some(path), "."));
        found.ok()
    }

    #[cfg(windows)]
    fn in_wsl(&self, binary: &str) -> bool {
        // `wsl.exe bash -l -c which …` costs ~0.5–2 s, so remember hits for
        // the life of the process. Misses are re-probed: the user may install
        // the CLI while the shell is running.
        static FOUND: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
            std::sync::OnceLock::new();
        let found = FOUND.get_or_init(Default::default);
        if found.lock().map(|s| s.contains(binary)).unwrap_or(false) {
            return true;
        }
        let hit = crate::agent_detect::agents::wsl_which(binary).is_some();
        if hit {
            if let Ok(mut s) = found.lock() {
                s.insert(binary.to_string());
            }
        }
        hit
    }

    #[cfg(not(windows))]
    fn in_wsl(&self, _binary: &str) -> bool {
        false
    }
}

fn resolve_engine(binary: &str, resolver: &dyn EngineResolver) -> Result<EngineLaunch, String> {
    if let Some(path) = resolver.native(binary) {
        return Ok(EngineLaunch::Native(path));
    }
    if resolver.in_wsl(binary) {
        return Ok(EngineLaunch::Wsl {
            binary: binary.to_string(),
        });
    }
    let searched = if cfg!(windows) {
        "on PATH or inside WSL"
    } else {
        "on PATH"
    };
    Err(format!(
        "engine binary `{binary}` not found {searched} — install it or add it to PATH"
    ))
}

/// Single-quote `s` for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `C:\Users\x` → `/mnt/c/Users/x` (WSL's default automount); anything else
/// passes through. Mirrors `toWslPath` in `src/terminal/claude-wrap.ts`.
fn to_wsl_path(p: &str) -> String {
    let bytes = p.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("/mnt/{drive}/{}", p[3..].replace('\\', "/"));
    }
    p.to_string()
}

/// Build the piped engine command for `launch` with `args`. `set_cwd` is
/// false for engines that take the directory as a flag instead (codex `--cd`);
/// a WSL launch always passes `--cd` to `wsl.exe`.
fn engine_command(launch: &EngineLaunch, args: &[String], cwd: &str, set_cwd: bool) -> Command {
    let mut cmd = match launch {
        EngineLaunch::Native(path) => {
            let is_batch = cfg!(windows)
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
                    .unwrap_or(false);
            let mut cmd = if is_batch {
                let mut cmd = Command::new("cmd.exe");
                cmd.arg("/c").arg(path);
                cmd
            } else {
                Command::new(path)
            };
            cmd.args(args).env("PATH", crate::runtime::augmented_path());
            cmd
        }
        EngineLaunch::Wsl { binary } => {
            let script = std::iter::once(binary.as_str())
                .chain(args.iter().map(String::as_str))
                .map(sh_quote)
                .collect::<Vec<_>>()
                .join(" ");
            let mut cmd = Command::new("wsl.exe");
            cmd.args([
                "--cd",
                &cwd.replace('\\', "/"),
                "-e",
                "bash",
                "-l",
                "-c",
                &script,
            ]);
            cmd
        }
    };
    if set_cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    cmd.no_console_window();
    cmd
}

/// Return the command for the requested engine.
fn build_engine_command(
    engine_id: &str,
    prompt: &str,
    cwd: &str,
    model: Option<&str>,
    mode: Option<&str>,
    resume_id: Option<&str>,
) -> Result<Command, String> {
    build_engine_command_with(
        &HostResolver,
        engine_id,
        prompt,
        cwd,
        model,
        mode,
        resume_id,
    )
}

fn build_engine_command_with(
    resolver: &dyn EngineResolver,
    engine_id: &str,
    prompt: &str,
    cwd: &str,
    model: Option<&str>,
    mode: Option<&str>,
    resume_id: Option<&str>,
) -> Result<Command, String> {
    let s = |v: &str| v.to_string();
    match engine_id {
        "claude-code" => {
            let permission_mode = mode
                .and_then(AcpSessionMode::from_acp_id)
                .unwrap_or_default()
                .as_claude_flag();

            let launch = resolve_engine("claude", resolver)?;
            let mut args: Vec<String> = [
                "--permission-prompt-tool",
                "stdio",
                "--permission-mode",
                permission_mode,
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
            ]
            .map(s)
            .to_vec();
            if let Some(id) = resume_id {
                args.extend([s("--resume"), s(id)]);
            }
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        "antigravity-cli" => {
            let launch = resolve_engine("agy", resolver)?;
            let mut args = vec![s("-p"), s(prompt), s("--output-format"), s("stream-json")];
            if let Some(id) = resume_id {
                args.extend([s("--conversation"), s(id)]);
            }
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            if let Some(mo) = mode {
                args.extend([s("--mode"), s(mo)]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        // Codex uses `codex exec --json` for new sessions and
        // `codex exec resume <thread_id> --json` for subsequent turns.
        // `--skip-git-repo-check` makes the spawn predictable inside
        // arbitrary project dirs (codex defaults to refusing outside a
        // git repo). `-` as the positional arg means "read prompt from stdin".
        "codex" => {
            let launch = resolve_engine("codex", resolver)?;
            let mut args = match resume_id {
                Some(id) => vec![s("exec"), s("resume"), s(id), s("--json")],
                None => vec![s("exec"), s("--json")],
            };
            // `--cd` is read by codex itself, so a WSL codex needs a Linux path.
            let codex_cwd = match launch {
                EngineLaunch::Wsl { .. } => to_wsl_path(cwd),
                EngineLaunch::Native(_) => s(cwd),
            };
            args.extend([s("--skip-git-repo-check"), s("--cd"), codex_cwd, s("-")]);
            // `--model` is a codex global flag (before the subcommand);
            // codex itself selects the default model from its config if
            // omitted, so we only pass it when explicitly set.
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            Ok(engine_command(&launch, &args, cwd, false))
        }
        "opencode" => {
            let launch = resolve_engine("opencode", resolver)?;
            let mut args = vec![s("run"), s("-p"), s(prompt)];
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        "pi" => {
            let launch = resolve_engine("pi", resolver)?;
            let mut args = vec![s("-p"), s(prompt)];
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        // cursor-agent is scaffolded but not yet runnable through the chi
        // surface. Return a clean error rather than falling through to an
        // unhelpful "command not found" OS error.
        "cursor-agent" => Err("cursor-agent runtime not implemented — \
             the cursor-agent CLI does not yet expose a stable non-interactive mode \
             compatible with the chi pipe protocol (ADR-013 Phase 4)"
            .to_string()),
        _ => Err(format!("engine not yet supported by iyke chi: {engine_id}")),
    }
}

/// Spawns the engine child and returns the (child, stdin, stdout, stderr).
fn spawn_engine_child(
    mut cmd: Command,
) -> Result<
    (
        Child,
        tokio::process::ChildStdin,
        tokio::process::ChildStdout,
        Option<tokio::process::ChildStderr>,
    ),
    String,
> {
    let mut child = cmd.spawn().map_err(|e| format!("spawn engine: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "engine stdin pipe missing".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "engine stdout pipe missing".to_string())?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stdout, stderr))
}

type EngineChild = (
    Child,
    tokio::process::ChildStdin,
    tokio::process::ChildStdout,
    Option<tokio::process::ChildStderr>,
);

/// Look up the OpenRouter HTTP adapter in the managed `EngineRegistry`.
///
/// `None` means "not drivable from this process": the registry is missing
/// (headless build) or the handle is not the HTTP adapter. Callers fall
/// through to the CLI path, whose error explains what the engine id supports.
async fn openrouter_adapter(app: &AppHandle) -> Option<OpenRouterHttpEngineState> {
    let registry = app.state::<EngineRegistryState>();
    match registry
        .get(crate::engines::openrouter_http::server::OPENROUTER_ENGINE_ID)
        .await
    {
        Some(EngineHandle::OpenRouterHttp(engine)) => Some(engine),
        _ => None,
    }
}

/// Seat-store probe (G-SEATS §2.2, §6.2): does the openrouter adapter still
/// hold the transcript for `thread_id`? `None` when the adapter is not
/// registered at all (engine unavailable); `Some(false)` when it is, but the
/// thread's history is gone (the app restarted). Read-only — it never
/// registers a session.
pub(crate) async fn openrouter_holds_thread(app: &AppHandle, thread_id: &str) -> Option<bool> {
    let engine = openrouter_adapter(app).await?;
    Some(engine.existing_session(thread_id).await.is_some())
}

/// Run-start tail shared by `chi_run` (new run) and `chi_resume` (continuing
/// turn) for the CLI-less engine. Marks the row running, registers the cancel
/// handle, and spawns the in-process turn task.
#[allow(clippy::too_many_arguments)]
async fn openrouter_start_run(
    db: Arc<PaDb>,
    runtime: &Arc<ChiRuntime>,
    run_id: String,
    output_path: PathBuf,
    engine: OpenRouterHttpEngineState,
    thread_id: String,
    prompt: String,
    model: Option<String>,
) -> Result<ChiRunResult, String> {
    cache_update_status(&db, &run_id, "running", None).await?;
    // The thread id doubles as the engine-native resume id: history is
    // process-local, so resume = same thread id while this process lives.
    cache_update_external_id(&db, &run_id, &thread_id)
        .await
        .ok();

    let cancelled = Arc::new(AtomicBool::new(false));
    runtime
        .insert(
            &run_id,
            Arc::new(ChiRunHandle {
                child: None,
                cancelled: cancelled.clone(),
                in_process: Some(InProcessCancel::OpenRouter {
                    engine: engine.clone(),
                    thread_id: thread_id.clone(),
                }),
            }),
        )
        .await;

    let run_id_for_task = run_id.clone();
    tauri::async_runtime::spawn(async move {
        openrouter_one_off_task(
            db,
            run_id_for_task,
            output_path,
            cancelled,
            engine,
            thread_id,
            prompt,
            model,
        )
        .await;
    });

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

/// Shared tail of the CLI-less run paths: attach the app, (re)register the
/// thread with its cwd, and hand off to `openrouter_start_run`. `run_id` and
/// `output_path` come from `spawn_chi_run`'s cache bookkeeping; `thread_id`
/// is `None` for a new run (thread id = run id) and the existing thread id
/// for a resumed turn.
async fn openrouter_spawn_run(
    app: &AppHandle,
    db: Arc<PaDb>,
    runtime: &Arc<ChiRuntime>,
    run_id: String,
    output_path: PathBuf,
    opts: &ChiRunOpts,
    cwd: String,
    resume_thread_id: Option<String>,
) -> Result<ChiRunResult, String> {
    let Some(engine) = openrouter_adapter(app).await else {
        let msg = format!(
            "engine '{}' is not registered as the CLI-less HTTP adapter (openrouter)",
            opts.engine_id
        );
        cache_update_done(&db, &run_id, "failed", Some(&msg), false, None)
            .await
            .ok();
        return Err(msg);
    };

    let thread_id = resume_thread_id.unwrap_or_else(|| run_id.clone());
    engine.attach_app(app.clone());
    engine.register_session(thread_id.clone(), cwd).await;

    openrouter_start_run(
        db,
        runtime,
        run_id,
        output_path,
        engine,
        thread_id,
        opts.prompt.clone(),
        opts.model.clone(),
    )
    .await
}

/// Partial output shared between the ACP update callback and the turn task.
/// `flushed` coalesces disk writes: flush at most once per 512 new bytes.
struct OpenRouterOutputBuffer {
    text: String,
    flushed: usize,
}

/// Pull the visible reply text out of one ACP update. Only
/// `AgentMessageChunk` text blocks count — thinking and tool-call envelopes
/// are surfaced to ACP clients but are not the run's output, matching what
/// the CLI readers accumulate (`TurnState::assistant_text`).
fn agent_message_delta(update: &SessionUpdate) -> Option<String> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Map one finished turn to the chi status pair. `PromptResponse` carries the
/// adapter's stop reason; a refusal is a completed turn the model declined to
/// answer, so it closes the run as `failed` with a reason (same treatment the
/// claude reader gives `stop_reason: "error"`).
fn openrouter_turn_outcome(result: &Result<PromptResponse, String>) -> (String, Option<String>) {
    match result {
        Ok(resp) => match resp.stop_reason {
            StopReason::Cancelled => ("cancelled".to_string(), None),
            StopReason::Refusal => (
                "failed".to_string(),
                Some("openrouter reported stop_reason refusal".to_string()),
            ),
            // EndTurn, MaxTokens, and any future variant are complete turns.
            _ => ("done".to_string(), None),
        },
        Err(e) => ("failed".to_string(), Some(e.clone())),
    }
}

/// Background task for a CLI-less engine turn (`openrouter`). No child
/// process: the adapter streams HTTP SSE in-process and emits ACP updates on
/// the callback; reply text lands in the same partial-flushed output file the
/// CLI readers write.
async fn openrouter_one_off_task(
    db: Arc<PaDb>,
    run_id: String,
    output_path: PathBuf,
    cancelled: Arc<AtomicBool>,
    engine: OpenRouterHttpEngineState,
    thread_id: String,
    prompt: String,
    model: Option<String>,
) {
    // A cancel that landed before this task started must win: `run_prompt`
    // clears the adapter's own cancel flag on entry, so without this check
    // the request would still go out (and spend tokens) under a row that
    // says `cancelled`.
    if cancelled.load(Ordering::SeqCst) {
        let file_error = write_output_file(&output_path, "", None)
            .await
            .err()
            .map(|e| format!("write output file: {e}"));
        cache_update_done(&db, &run_id, "cancelled", file_error.as_deref(), false, None)
            .await
            .ok();
        return;
    }

    let shared = Arc::new(std::sync::Mutex::new(OpenRouterOutputBuffer {
        text: String::new(),
        flushed: 0,
    }));
    let cb_shared = shared.clone();
    let cb_output_path = output_path.clone();
    let cb = move |update: SessionUpdate| {
        let Some(delta) = agent_message_delta(&update) else {
            return;
        };
        let should_flush = {
            let mut buf = cb_shared.lock().expect("output buffer poisoned");
            buf.text.push_str(&delta);
            buf.text.len() - buf.flushed >= OPENROUTER_FLUSH_BYTES
        };
        if !should_flush {
            return;
        }
        let text = {
            let mut buf = cb_shared.lock().expect("output buffer poisoned");
            buf.flushed = buf.text.len();
            buf.text.clone()
        };
        write_output_file_sync(&cb_output_path, &text, None);
    };
    let cb_ref: &(dyn Fn(SessionUpdate) + Send + Sync) = &cb;

    // A panic in the adapter or the callback must still close the row;
    // otherwise it stays `running` forever and no WP-40 notification fires.
    use futures_util::FutureExt as _;
    let result = std::panic::AssertUnwindSafe(engine.run_prompt(
        &thread_id,
        &prompt,
        model.as_deref(),
        Some(cb_ref),
    ))
    .catch_unwind()
    .await
    .unwrap_or_else(|_| Err("openrouter turn panicked".to_string()));

    let output = {
        let mut buf = shared.lock().expect("output buffer poisoned");
        buf.flushed = buf.text.len();
        std::mem::take(&mut buf.text)
    };

    // Determine final status. `cancelled` is set by `chi_cancel` before it
    // calls `handle_cancel`; the adapter's own Cancelled stop reason is the
    // authoritative signal that the abort landed.
    let (status, turn_error) = openrouter_turn_outcome(&result);
    let (status, turn_error) = if cancelled.load(Ordering::SeqCst) && status != "cancelled" {
        // Cancel raced the natural end: keep the cancel.
        ("cancelled".to_string(), None)
    } else {
        (status, turn_error)
    };

    let output_truncated = output.len() > 100_000;
    let file_error = match write_output_file(&output_path, &output, turn_error.as_deref()).await {
        Ok(()) => turn_error.clone(),
        Err(e) => Some(format!("write output file: {e}")),
    };

    cache_update_done(
        &db,
        &run_id,
        &status,
        file_error.as_deref(),
        output_truncated,
        None,
    )
    .await
    .ok();
}

/// Synchronous sibling of [`write_output_file`] for the ACP update callback,
/// which runs inside the adapter's streaming loop and cannot await. Best
/// effort: a failed flush is retried by the next delta or the final write.
fn write_output_file_sync(path: &Path, output: &str, error: Option<&str>) {
    let file = RunOutputFile {
        output: Some(output.to_string()),
        error: error.map(|s| s.to_string()),
        done_at: Some(now_iso()),
        status: None,
        external_id: None,
    };
    if let Ok(json) = serde_json::to_string(&file) {
        let _ = std::fs::write(path, json);
    }
}

/// Minimum number of new bytes before the callback flushes partial output.
const OPENROUTER_FLUSH_BYTES: usize = 512;

/// Spawn the engine for a run whose cache row already exists. If building or
/// spawning fails (engine not installed, bad cwd, …) the row is closed out as
/// `failed` with the error, so `/iyke/chi/status` and the Sessions list show
/// why instead of the run sitting `queued` / `running` forever.
async fn spawn_engine_or_fail(
    db: &PaDb,
    run_id: &str,
    cmd: Result<Command, String>,
) -> Result<EngineChild, String> {
    match cmd.and_then(spawn_engine_child) {
        Ok(child) => Ok(child),
        Err(e) => {
            log::warn!(target: "ikenga::chi", "chi run {run_id} failed to start: {e}");
            if let Err(db_err) =
                cache_update_done(db, run_id, "failed", Some(&e), false, None).await
            {
                log::warn!(target: "ikenga::chi", "chi run {run_id}: could not record spawn failure: {db_err}");
            }
            Err(e)
        }
    }
}

/// Background task for a Claude Code one-off. Reads `stdout`, writes partial
/// output to `output_path`, and updates `chi_cache` as the run progresses.
async fn claude_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    prompt: String,
) {
    // Send the initial prompt envelope.
    let envelope = user_envelope(&prompt);
    if let Err(e) = stdin.write_all(envelope.as_bytes()).await {
        // Terminal failure: close the row out through `cache_update_done` so it
        // gets `ended_at` and the WP-40 `run_failed` notification.
        cache_update_done(&db, &run_id, "failed", Some(&format!("stdin write: {e}")), false, None)
            .await
            .ok();
        return;
    }
    let _ = stdin.flush().await;
    // Close stdin so claude knows no more input is coming for this turn.
    // `shutdown()` on a child pipe does not close the handle, so drop it:
    // until the write end closes, stream-json claude waits for more input
    // and the run never finishes.
    let _ = stdin.shutdown().await;
    drop(stdin);

    // Spawn stderr logger.
    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "claude stderr: {line}");
            }
        });
    }

    let mut parser = StreamParser::new();
    let mut reader = BufReader::new(stdout);
    let mut buf = vec![0u8; 8 * 1024];
    let mut output = String::new();
    let mut artifacts = Vec::<serde_json::Value>::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut stop_reason: Option<String> = None;

    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let events = parser.feed(&buf[..n]);
                for event in events {
                    match event {
                        ChatEvent::SessionInit { session_id, .. } if !session_id.is_empty() => {
                            if external_id.is_none() {
                                external_id = Some(session_id.clone());
                                cache_update_external_id(&db, &run_id, &session_id)
                                    .await
                                    .ok();
                                cache_update_status(&db, &run_id, "running", None)
                                    .await
                                    .ok();
                            }
                        }
                        ChatEvent::Text { delta, .. } => {
                            output.push_str(&delta);
                        }
                        ChatEvent::Artifact {
                            path,
                            mime,
                            produced_by,
                        } => {
                            artifacts.push(serde_json::json!({
                                "path": path,
                                "mime": mime,
                                "producedBy": produced_by,
                            }));
                        }
                        ChatEvent::Done { stop_reason: s, .. } => {
                            saw_done = true;
                            stop_reason = s.clone();
                        }
                        ChatEvent::ControlRequest { subtype, .. } if subtype == "permission" => {
                            cache_update_status(&db, &run_id, "awaiting_auth", None)
                                .await
                                .ok();
                        }
                        ChatEvent::AskUserQuestion { .. } => {
                            cache_update_status(&db, &run_id, "awaiting_auth", None)
                                .await
                                .ok();
                        }
                        _ => {}
                    }
                }

                // Periodically flush partial output to disk.
                write_output_file(&output_path, &output, None).await.ok();
            }
            Err(e) => {
                log::debug!(target: "ikenga::chi", "claude reader closed: {e}");
                break;
            }
        }
    }

    // Determine final status.
    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            (
                "failed",
                Some("claude reported stop_reason error"),
                Some(output),
            )
        } else {
            ("done", None, Some(output))
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
            Some(output),
        )
    };

    let output_truncated = done_output
        .as_ref()
        .map(|s| s.len() > 100_000)
        .unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    // Write final output file.
    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    let artifacts_value = if artifacts.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(artifacts))
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        artifacts_value.as_ref(),
    )
    .await
    .ok();

    // Reap the child so the OS handle is released.
    let mut child = child.lock().await;
    let _ = child.try_wait();
}

async fn antigravity_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    _stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    _prompt: String,
) {
    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "antigravity stderr: {line}");
            }
        });
    }

    let mut reader = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut stop_reason: Option<String> = None;

    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(event) = val.get("event").and_then(|e| e.as_str()) {
                match event {
                    "init" => {
                        if let Some(conv_id) = val.get("conversation_id").and_then(|id| id.as_str())
                        {
                            if external_id.is_none() {
                                external_id = Some(conv_id.to_string());
                                cache_update_external_id(&db, &run_id, conv_id).await.ok();
                                cache_update_status(&db, &run_id, "running", None)
                                    .await
                                    .ok();
                            }
                        }
                    }
                    "step_update" => {
                        if let Some(step_update) = val.get("step_update") {
                            if let Some(step_type) =
                                step_update.get("step_type").and_then(|t| t.as_str())
                            {
                                if step_type == "agent_response" {
                                    if let Some(delta) =
                                        step_update.get("text_delta").and_then(|d| d.as_str())
                                    {
                                        output.push_str(delta);
                                    }
                                }
                            }
                        }
                    }
                    "result" => {
                        saw_done = true;
                        if let Some(result) = val.get("result") {
                            if let Some(status) = result.get("status").and_then(|s| s.as_str()) {
                                if status != "SUCCESS" {
                                    stop_reason = Some("error".to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        write_output_file(&output_path, &output, None).await.ok();
    }

    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            (
                "failed",
                Some("antigravity reported stop_reason error"),
                Some(output),
            )
        } else {
            ("done", None, Some(output))
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
            Some(output),
        )
    };

    let output_truncated = done_output
        .as_ref()
        .map(|s| s.len() > 100_000)
        .unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        None,
    )
    .await
    .ok();

    let mut child = child.lock().await;
    let _ = child.try_wait();
}

/// Background task for a Codex one-off (`codex exec --json`).
///
/// Reads the JSONL event stream from stdout via the existing
/// `codex_pty::parser`, extracts `agent_message` text chunks and the
/// `thread.started` thread id (stored as `external_id` so `chi_resume`
/// can pass it back as `--resume <id>`).
async fn codex_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    prompt: String,
) {
    // Write prompt to stdin then close it so codex knows EOF.
    if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
        // Terminal failure: see the matching note in `claude_one_off_task`.
        cache_update_done(&db, &run_id, "failed", Some(&format!("stdin write: {e}")), false, None)
            .await
            .ok();
        return;
    }
    let _ = stdin.flush().await;
    let _ = stdin.shutdown().await;
    // Close the pipe for real so codex sees EOF on the `-` prompt (see
    // claude_one_off_task).
    drop(stdin);

    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "codex stderr: {line}");
            }
        });
    }

    let mut reader = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut failed = false;

    while let Ok(Some(line)) = reader.next_line().await {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        let event = match codex_parser::parse_event(&line) {
            Ok(e) => e,
            Err(e) => {
                log::debug!(target: "ikenga::chi", "codex parse error: {e}");
                continue;
            }
        };
        match &event {
            codex_parser::ParsedEvent::ThreadStarted { thread_id } => {
                if external_id.is_none() {
                    external_id = Some(thread_id.clone());
                    cache_update_external_id(&db, &run_id, thread_id).await.ok();
                    cache_update_status(&db, &run_id, "running", None)
                        .await
                        .ok();
                }
            }
            codex_parser::ParsedEvent::TurnCompleted { .. } => {
                saw_done = true;
            }
            codex_parser::ParsedEvent::TurnFailed { message } => {
                saw_done = true;
                failed = true;
                output.push_str(&format!("[error] {message}\n"));
            }
            codex_parser::ParsedEvent::Error { message } => {
                output.push_str(&format!("[warning] {message}\n"));
            }
            codex_parser::ParsedEvent::Item { phase, kind } => {
                // Accumulate agent_message text chunks (completed/updated phases only).
                if matches!(
                    phase,
                    codex_parser::ItemPhase::Completed | codex_parser::ItemPhase::Updated
                ) {
                    if let codex_parser::ItemKind::AgentMessage { text, .. } = kind {
                        if !text.is_empty() {
                            output.push_str(text);
                        }
                    }
                }
            }
            _ => {}
        }
        write_output_file(&output_path, &output, None).await.ok();
    }

    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done && !failed {
        ("done", None, Some(output))
    } else if failed {
        ("failed", Some("codex reported turn.failed"), Some(output))
    } else {
        (
            "failed",
            Some("codex child exited without turn.completed"),
            Some(output),
        )
    };

    let output_truncated = done_output
        .as_ref()
        .map(|s| s.len() > 100_000)
        .unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        None,
    )
    .await
    .ok();

    let mut child = child.lock().await;
    let _ = child.try_wait();
}

async fn write_output_file(path: &Path, output: &str, error: Option<&str>) -> Result<(), String> {
    let file = RunOutputFile {
        output: Some(output.to_string()),
        error: error.map(|s| s.to_string()),
        done_at: Some(now_iso()),
        status: None,
        external_id: None,
    };
    let json = serde_json::to_string(&file).map_err(|e| format!("serialize output: {e}"))?;
    tokio::fs::write(path, json)
        .await
        .map_err(|e| format!("write output file: {e}"))?;
    Ok(())
}

/// Run a Chi. Spawns the engine child in the background and returns the
/// run id immediately.
#[tauri::command]
pub async fn chi_run(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    runtime: State<'_, Arc<ChiRuntime>>,
    opts: ChiRunOpts,
) -> Result<ChiRunResult, String> {
    spawn_chi_run(
        db.inner().clone(),
        cache.inner(),
        runtime.inner(),
        Some(&app),
        opts,
        "cli",
    )
    .await
}

/// Core of `chi_run`, callable from other commands that need to launch an
/// agent without going through the Tauri command boundary (e.g.
/// `comment_route`'s `chi` sink). `owner` tags the cache row so the audit
/// trail distinguishes a CLI-initiated run from a pin-initiated one.
///
/// `app` is `None` only from tests; CLI-less engines (openrouter) need it for
/// the managed `EngineRegistry` + vault and fail the run cleanly without it.
pub(crate) async fn spawn_chi_run(
    db: Arc<PaDb>,
    cache: &ChiCache,
    runtime: &Arc<ChiRuntime>,
    app: Option<&AppHandle>,
    opts: ChiRunOpts,
    owner: &str,
) -> Result<ChiRunResult, String> {
    cache.ensure_cache_dir()?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let output_path = cache.run_output_path(&run_id);

    // Initial one-off TTL is 1 hour; long-lived sessions will refresh this.
    cache_insert(&db, &run_id, &opts, &output_path, owner).await?;

    let cwd = opts
        .cwd
        .clone()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| ".".to_string());
    let cwd = shellexpand::full(&cwd)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| cwd.clone());

    // ── CLI-less (in-process) engine path ────────────────────────────────────
    // `openrouter` has no CLI to spawn — the adapter IS the HTTP client
    // (WP-20). Dispatch it in-process through the managed `EngineRegistry`
    // instead of failing binary resolution; the output file + cache contract
    // stays identical to the CLI readers. Runs before the detached path
    // because chi-runner can only launch CLI engines.
    if opts.engine_id == "openrouter" {
        let Some(app) = app else {
            let msg = "openrouter chi runs need the desktop app handle (engine registry + \
                       vault); refusing the run"
                .to_string();
            log::warn!(target: "ikenga::chi", "chi run {run_id}: {msg}");
            cache_update_done(&db, &run_id, "failed", Some(&msg), false, None)
                .await
                .ok();
            return Err(msg);
        };
        return openrouter_spawn_run(app, db, runtime, run_id, output_path, &opts, cwd, None).await;
    }

    // ── Persistent (detached chi-runner) path ────────────────────────────────
    // Try this first so we never spawn a redundant in-process child.
    if opts.persistent {
        let conf = chi_runner::RunnerConf {
            run_id: &run_id,
            engine_id: &opts.engine_id,
            prompt: &opts.prompt,
            cwd: &cwd,
            model: opts.model.as_deref(),
            mode: opts.mode.as_deref(),
            resume_session_id: opts.resume_session_id.as_deref(),
            output_path: &output_path.to_string_lossy(),
            timeout_seconds: opts.timeout_seconds.map(|s| s as u64),
        };
        match chi_runner::spawn_detached_runner(&conf, &cache.cache_dir()) {
            Ok(pid) => {
                if let Err(e) = cache_mark_detached(&db, &run_id, pid).await {
                    // An unrecorded runner could be neither reconciled nor
                    // cancelled: take it down rather than leave it orphaned.
                    let _ = chi_runner::kill_process_group(pid, chi_runner::CANCEL_GRACE).await;
                    cache_update_done(&db, &run_id, "failed", Some(&e), false, None)
                        .await
                        .ok();
                    return Err(e);
                }
                log::info!(
                    target: "ikenga::chi",
                    "chi run {run_id} started detached (chi-runner pid {pid})"
                );
                return Ok(ChiRunResult {
                    run_id,
                    status: "running".to_string(),
                    output: None,
                    output_truncated: None,
                    error: None,
                });
            }
            Err(reason) => {
                log::warn!(
                    target: "ikenga::chi",
                    "chi run {run_id}: detached chi-runner unavailable ({reason}), \
                     falling back to in-process task"
                );
            }
        }
    }

    // ── In-process (non-persistent) path ─────────────────────────────────────
    let cmd = build_engine_command(
        &opts.engine_id,
        &opts.prompt,
        &cwd,
        opts.model.as_deref(),
        opts.mode.as_deref(),
        opts.resume_session_id.as_deref(),
    );
    let (child, stdin, stdout, stderr) = spawn_engine_or_fail(&db, &run_id, cmd).await?;
    cache_update_status(&db, &run_id, "running", None).await?;

    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(ChiRunHandle {
        child: Some(child.clone()),
        cancelled: cancelled.clone(),
        in_process: None,
    });
    runtime.insert(&run_id, handle).await;

    let cache = cache.clone();
    let output_path = output_path.clone();
    let prompt = opts.prompt.clone();
    let run_id_for_task = run_id.clone();
    let engine_id = opts.engine_id.clone();
    tauri::async_runtime::spawn(async move {
        if engine_id == "antigravity-cli" {
            antigravity_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else if engine_id == "codex" {
            codex_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else {
            claude_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        }
    });

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

/// Resume an existing Chi session using its engine-native `external_id`.
#[tauri::command]
pub async fn chi_resume(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    runtime: State<'_, Arc<ChiRuntime>>,
    #[allow(non_snake_case)] runId: String,
    prompt: String,
) -> Result<ChiRunResult, String> {
    resume_chi_run(
        &app,
        db.inner().clone(),
        cache.inner(),
        runtime.inner(),
        runId,
        prompt,
    )
    .await
}

/// Core of `chi_resume`, callable from other commands without going through
/// the Tauri command boundary — the seat store's resume and queue paths
/// (G-SEATS §4.1, §4.5). The body is the command's own, unchanged: the run
/// keeps its `run_id`.
pub(crate) async fn resume_chi_run(
    app: &AppHandle,
    db: Arc<PaDb>,
    cache: &ChiCache,
    runtime: &Arc<ChiRuntime>,
    run_id: String,
    prompt: String,
) -> Result<ChiRunResult, String> {
    let mut row = cache_get(&db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    // ── A detached (chi-runner) run ──────────────────────────────────────────
    // The resume runs in-process, so the old runner has to be finished first:
    // refuse while it still runs (two writers on one output file), otherwise
    // settle its terminal status now — the sweep may not have seen it yet —
    // pick up the engine session id it wrote, and drop the pid so the sweep
    // doesn't judge the new in-process turn by the old runner's exit.
    if let Some(pid) = row.pid {
        let path = resolve_output_path(&cache.cache_dir(), row.output_path.as_deref());
        let seen = observe_detached(pid, path.as_deref(), &chi_runner::probe_runner).await;
        if seen.liveness == RunLiveness::Running {
            return Err(format!(
                "chi run {run_id} is still running (detached chi-runner pid {pid})"
            ));
        }
        let external_id = seen.external_id.clone();
        apply_detached(&db, &run_id, row.external_id.as_deref(), seen).await?;
        if row.external_id.is_none() {
            row.external_id = external_id;
        }
        cache_clear_pid(&db, &run_id).await?;
    }

    let cache = cache.clone();
    let output_path = PathBuf::from(row.output_path.as_deref().unwrap_or(""));

    // ── CLI-less (in-process) engine path ────────────────────────────────────
    // History is process-local: resume = same thread id while this process
    // lives. Refuse (rather than run an empty-context turn) when the
    // transcript is gone — the same honesty rule the adapter enforces over
    // ACP in `handle_load_session`.
    if row.engine_id == "openrouter" {
        // One turn at a time per thread: a second resume while a turn is in
        // flight would share the adapter session, lose the first turn's abort
        // slot and write the same output file and cache row twice.
        if matches!(row.status.as_str(), "running" | "queued") {
            return Err(format!("chi run {run_id} is still in progress"));
        }
        let Some(engine) = openrouter_adapter(app).await else {
            return Err(
                "openrouter engine is not registered; restart the shell to resume its runs"
                    .to_string(),
            );
        };
        let thread_id = row
            .external_id
            .clone()
            .unwrap_or_else(|| row.run_id.clone());
        if let Err(e) = engine.handle_load_session(thread_id.clone(), None).await {
            return Err(e);
        }
        return openrouter_spawn_run(
            app,
            db,
            runtime,
            run_id,
            output_path,
            &row_into_resume_opts(&row, prompt),
            row.cwd.clone().unwrap_or_else(|| ".".to_string()),
            Some(thread_id),
        )
        .await;
    }

    let resume_id = row
        .external_id
        .ok_or_else(|| format!("chi run {run_id} has no engine session id to resume against"))?;

    cache_update_status(&db, &run_id, "running", None).await?;

    let cwd = row.cwd.unwrap_or_else(|| ".".to_string());
    let cwd = shellexpand::full(&cwd)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| cwd.clone());

    let cmd = build_engine_command(
        &row.engine_id,
        &prompt,
        &cwd,
        row.model.as_deref(),
        row.mode.as_deref(),
        Some(&resume_id),
    );
    let (child, stdin, stdout, stderr) = spawn_engine_or_fail(&db, &run_id, cmd).await?;

    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(ChiRunHandle {
        child: Some(child.clone()),
        cancelled: cancelled.clone(),
        in_process: None,
    });
    runtime.insert(&run_id, handle).await;

    let db = db.clone();
    let cache = cache.clone();
    let output_path = output_path.clone();
    let engine_id = row.engine_id.clone();
    let run_id_for_task = run_id.clone();
    tauri::async_runtime::spawn(async move {
        if engine_id == "antigravity-cli" {
            antigravity_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else if engine_id == "codex" {
            codex_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else {
            claude_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        }
    });

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

/// Read the status of a Chi run from the cache and its output file.
#[tauri::command]
pub async fn chi_status(
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    #[allow(non_snake_case)] runId: String,
) -> Result<ChiRunResult, String> {
    // The read path (row + output file + detached-run liveness, no writes)
    // is shared with the daemon's `chi_status` arm.
    chi_read::status(
        &db,
        &cache.cache_dir(),
        &runId,
        &chi_runner::probe_runner,
        chi_read::OutputFiles::AsStored,
    )
    .await
}

/// List cached Chi runs, optionally filtered by engine. Merges with agent-native
/// session records on disk.
#[tauri::command]
pub async fn chi_list(
    db: State<'_, Arc<PaDb>>,
    #[allow(non_snake_case)] engineId: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<ChiCacheRow>, String> {
    let engine_id = engineId.as_deref();
    let mut rows = chi_read::list(&db, engine_id, limit).await?;

    // Merge with Claude JSONL records when no engine filter or claude-code.
    if engine_id.is_none() || engine_id == Some("claude-code") {
        match claude_list_sessions(None, Some(limit.unwrap_or(50).clamp(1, 200) as usize)).await {
            Ok(sessions) => {
                let mut seen: std::collections::HashSet<String> =
                    rows.iter().filter_map(|r| r.external_id.clone()).collect();
                for s in sessions {
                    if seen.contains(&s.session_id) {
                        // Refresh last_seen_at on matching cache rows.
                        for row in rows.iter_mut() {
                            if row.external_id.as_deref() == Some(&s.session_id) {
                                row.last_seen_at =
                                    s.last_message_at.clone().or(Some(s.started_at.clone()));
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

    let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
    rows.truncate(limit);
    Ok(rows)
}

/// Cancel a Chi run. Kills the engine child process.
#[tauri::command]
pub async fn chi_cancel(
    db: State<'_, Arc<PaDb>>,
    runtime: State<'_, Arc<ChiRuntime>>,
    #[allow(non_snake_case)] runId: String,
) -> Result<ChiRunResult, String> {
    let run_id = runId;
    let row = cache_get(&db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    if let Some(handle) = runtime.remove(&run_id).await {
        handle.cancelled.store(true, Ordering::SeqCst);
        match &handle.in_process {
            // CLI-less engines: interrupt through the adapter itself. The
            // abort signal breaks the in-flight stream read; `run_prompt`
            // then reports `Cancelled`.
            Some(InProcessCancel::OpenRouter { engine, thread_id }) => {
                if let Err(e) = engine.handle_cancel(thread_id.clone()).await {
                    return Err(format!("cancel openrouter turn: {e}"));
                }
            }
            None => {}
        }
        if let Some(child) = &handle.child {
            let mut child = child.lock().await;
            if let Err(e) = child.start_kill() {
                return Err(format!("kill child: {e}"));
            }
        }
    }

    // A persistent run is a detached chi-runner with no local child handle,
    // so the kill goes to its recorded pid — its whole process group, so the
    // engine it spawned goes too. Only when the probe says the pid is still
    // *our* runner: a dead or reused pid is never signalled. The sweep is
    // held off this run meanwhile so it can't record the kill as a `failed`.
    let _cancelling = CancellingGuard::hold(&run_id);
    if let Some(pid) = row.pid.and_then(|p| u32::try_from(p).ok()) {
        match chi_runner::probe_runner(pid) {
            chi_runner::PidProbe::Ours => {
                chi_runner::kill_process_group(pid, chi_runner::CANCEL_GRACE)
                    .await
                    .map_err(|e| format!("kill chi-runner: {e}"))?;
            }
            other => log::info!(
                target: "ikenga::chi",
                "chi run {run_id}: not signalling pid {pid} ({other:?})"
            ),
        }
    }

    cache_update_status(&db, &run_id, "cancelled", None).await?;

    Ok(ChiRunResult {
        run_id: row.run_id,
        status: "cancelled".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

// ═══════════════════════════════════════════════════════════════════════
// Detached-run reconciliation (WP-18b, G-88)
// ═══════════════════════════════════════════════════════════════════════

/// Sweep cadence. Same order of magnitude as the seat store's queue poll; the
/// query is one scan of a small table.
const DETACHED_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

static SWEEP_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Runs `chi_cancel` is killing right now. The sweep skips them: a runner
/// seen dead mid-cancel is a cancel, not a failure.
static CANCELLING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct CancellingGuard(String);

impl CancellingGuard {
    fn hold(run_id: &str) -> Self {
        if let Ok(mut ids) = CANCELLING.lock() {
            ids.push(run_id.to_string());
        }
        Self(run_id.to_string())
    }

    fn contains(run_id: &str) -> bool {
        CANCELLING
            .lock()
            .map(|ids| ids.iter().any(|id| id == run_id))
            .unwrap_or(false)
    }
}

impl Drop for CancellingGuard {
    fn drop(&mut self) {
        if let Ok(mut ids) = CANCELLING.lock() {
            if let Some(i) = ids.iter().position(|id| *id == self.0) {
                ids.swap_remove(i);
            }
        }
    }
}

/// What a detached run looks like from outside: the liveness decision and
/// the engine session id chi-runner recorded, if any.
#[derive(Debug)]
struct DetachedObservation {
    liveness: RunLiveness,
    external_id: Option<String>,
}

/// Probe `pid`, *then* read the status file — the order the liveness rule
/// depends on (`chi_runner::decide_liveness`).
async fn observe_detached(
    pid: i64,
    output_path: Option<&Path>,
    probe: &(dyn Fn(u32) -> chi_runner::PidProbe + Sync),
) -> DetachedObservation {
    let alive = pid_alive(pid, probe);
    let file = match output_path {
        Some(path) => read_output_file(path).await,
        None => None,
    };
    let liveness = chi_runner::decide_liveness(
        alive,
        file.as_ref().and_then(|f| f.status.as_deref()),
        file.as_ref().and_then(|f| f.error.as_deref()),
    );
    DetachedObservation {
        liveness,
        external_id: file.and_then(|f| f.external_id).filter(|id| !id.is_empty()),
    }
}

/// Fold an observation into the row: record a newly seen engine session id,
/// and finish a terminal run through the transition-guarded
/// `cache_update_done_if_live` (which produces the WP-40 notification).
/// Returns whether this call finished the run.
async fn apply_detached(
    db: &PaDb,
    run_id: &str,
    row_external_id: Option<&str>,
    seen: DetachedObservation,
) -> Result<bool, String> {
    if let Some(ext) = seen.external_id.as_deref() {
        if row_external_id != Some(ext) {
            cache_update_external_id(db, run_id, ext).await?;
        }
    }
    match seen.liveness {
        RunLiveness::Running => Ok(false),
        RunLiveness::Terminal { status, error } => {
            let finished = cache_update_done_if_live(db, run_id, status, error.as_deref()).await?;
            if finished {
                log::info!(
                    target: "ikenga::chi",
                    "chi run {run_id}: detached run reconciled as {status}"
                );
            }
            Ok(finished)
        }
    }
}

/// G-88: every `queued` / `running` row with a detached runner pid gets the
/// liveness decision applied; a finished or vanished runner moves the row to
/// its terminal status exactly once. Returns how many runs it finished.
async fn reconcile_detached_runs_with(
    db: &PaDb,
    cache_dir: &Path,
    probe: &(dyn Fn(u32) -> chi_runner::PidProbe + Sync),
) -> Result<usize, String> {
    let pool = db.ensure_pool().await?;
    let rows = sqlx::query(
        "SELECT run_id, pid, output_path, external_id FROM chi_cache
         WHERE pid IS NOT NULL AND status IN ('queued', 'running')",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("chi_cache detached runs: {e}"))?;

    let mut finished = 0;
    for r in rows {
        let run_id: String = r.get("run_id");
        if CancellingGuard::contains(&run_id) {
            continue;
        }
        let pid: i64 = r.get("pid");
        let output_path: Option<String> = r.get("output_path");
        let external_id: Option<String> = r.get("external_id");
        let path = resolve_output_path(cache_dir, output_path.as_deref());
        let seen = observe_detached(pid, path.as_deref(), probe).await;
        match apply_detached(db, &run_id, external_id.as_deref(), seen).await {
            Ok(true) => finished += 1,
            Ok(false) => {}
            Err(e) => log::warn!(target: "ikenga::chi", "chi run {run_id}: reconcile: {e}"),
        }
    }
    Ok(finished)
}

/// [`reconcile_detached_runs_with`] against the app's db, cache dir and the
/// real pid probe.
pub(crate) async fn reconcile_detached_runs(app: &AppHandle) -> Result<usize, String> {
    let db = app
        .try_state::<Arc<PaDb>>()
        .map(|s| s.inner().clone())
        .ok_or("PaDb is not managed")?;
    let cache_dir = app
        .try_state::<ChiCache>()
        .map(|s| s.cache_dir())
        .ok_or("ChiCache is not managed")?;
    reconcile_detached_runs_with(&db, &cache_dir, &chi_runner::probe_runner).await
}

/// Start the detached-run sweep: once now (boot — runs that finished or died
/// while the app was down), then every [`DETACHED_SWEEP_EVERY`]. Idempotent;
/// called from `iyke::start` next to the seat store's own boot hook.
pub(crate) fn install_detached_reconciler(app: &AppHandle) {
    if SWEEP_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(e) = reconcile_detached_runs(&app).await {
                log::warn!(target: "ikenga::chi", "detached run sweep: {e}");
            }
            tokio::time::sleep(DETACHED_SWEEP_EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::shared::chi::{cache_list, parse_output_file};

    async fn test_db() -> PaDb {
        let file_name = format!("ikenga-chi-test-{}.db", uuid::Uuid::new_v4());
        let db_path = std::env::temp_dir().join(file_name);
        PaDb::new(db_path)
    }

    #[tokio::test]
    async fn chi_cache_round_trip() {
        let db = test_db().await;
        let cache = ChiCache::new(std::env::temp_dir());
        cache.ensure_cache_dir().unwrap();

        let run_id = uuid::Uuid::new_v4().to_string();
        let output_path = cache.run_output_path(&run_id);
        let opts = ChiRunOpts {
            engine_id: "claude-code".into(),
            prompt: "hello".into(),
            cwd: Some("/tmp".into()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };

        cache_insert(&db, &run_id, &opts, &output_path, "cli")
            .await
            .unwrap();

        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.run_id, run_id);
        assert_eq!(row.engine_id, "claude-code");
        assert_eq!(row.status, "queued");
        assert_eq!(row.cwd.as_deref(), Some("/tmp"));

        cache_update_status(&db, &run_id, "running", None)
            .await
            .unwrap();
        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "running");
        assert!(row.last_seen_at.is_some());

        let rows = cache_list(&db, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_id, run_id);
    }

    #[tokio::test]
    async fn chi_output_file_round_trip() {
        let cache = ChiCache::new(std::env::temp_dir());
        cache.ensure_cache_dir().unwrap();
        let path = cache.run_output_path("test-run");
        write_output_file(&path, "partial output", None)
            .await
            .unwrap();
        let file = read_output_file(&path).await.unwrap();
        assert_eq!(file.output.as_deref(), Some("partial output"));
    }

    #[test]
    fn test_build_engine_command_antigravity() {
        let cmd = build_engine_command_with(
            &FakeResolver::native(&["agy"]),
            "antigravity-cli",
            "hello",
            "/tmp",
            Some("gemini-2.0-flash"),
            Some("plan"),
            Some("conv-123"),
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "agy");
        let args: Vec<&str> = cmd
            .as_std()
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec![
                "-p",
                "hello",
                "--output-format",
                "stream-json",
                "--conversation",
                "conv-123",
                "--model",
                "gemini-2.0-flash",
                "--mode",
                "plan"
            ]
        );
    }

    #[test]
    fn test_build_engine_command_opencode() {
        let cmd = build_engine_command_with(
            &FakeResolver::native(&["opencode"]),
            "opencode",
            "fix the bug",
            "/tmp",
            Some("claude-3-7-sonnet"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "opencode");
        let args: Vec<&str> = cmd
            .as_std()
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec!["run", "-p", "fix the bug", "--model", "claude-3-7-sonnet",]
        );
    }

    #[test]
    fn test_build_engine_command_pi() {
        let cmd = build_engine_command_with(
            &FakeResolver::native(&["pi"]),
            "pi",
            "refactor this file",
            "/tmp",
            Some("claude-3-7-sonnet"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "pi");
        let args: Vec<&str> = cmd
            .as_std()
            .get_args()
            .map(|s| s.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            vec!["-p", "refactor this file", "--model", "claude-3-7-sonnet",]
        );
    }

    /// Stands in for the host PATH and WSL. `native` binaries resolve to a
    /// bare path of the same name so program assertions stay readable.
    struct FakeResolver {
        native: Vec<&'static str>,
        wsl: Vec<&'static str>,
    }

    impl FakeResolver {
        fn native(bins: &[&'static str]) -> Self {
            Self {
                native: bins.to_vec(),
                wsl: vec![],
            }
        }
        fn wsl(bins: &[&'static str]) -> Self {
            Self {
                native: vec![],
                wsl: bins.to_vec(),
            }
        }
    }

    impl EngineResolver for FakeResolver {
        fn native(&self, binary: &str) -> Option<PathBuf> {
            self.native.contains(&binary).then(|| PathBuf::from(binary))
        }
        fn in_wsl(&self, binary: &str) -> bool {
            self.wsl.contains(&binary)
        }
    }

    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn resolve_engine_prefers_host_path_over_wsl() {
        let r = FakeResolver {
            native: vec!["claude"],
            wsl: vec!["claude"],
        };
        assert_eq!(
            resolve_engine("claude", &r).unwrap(),
            EngineLaunch::Native(PathBuf::from("claude"))
        );
    }

    #[test]
    fn resolve_engine_falls_back_to_wsl() {
        assert_eq!(
            resolve_engine("claude", &FakeResolver::wsl(&["claude"])).unwrap(),
            EngineLaunch::Wsl {
                binary: "claude".into()
            }
        );
    }

    #[test]
    fn resolve_engine_errors_clearly_when_nothing_resolves() {
        let err = resolve_engine("claude", &FakeResolver::native(&[])).unwrap_err();
        assert!(err.contains("`claude` not found"), "{err}");
        assert!(err.contains("install it or add it to PATH"), "{err}");
        // …and build_engine_command surfaces it instead of an OS spawn error.
        let err = build_engine_command_with(
            &FakeResolver::native(&[]),
            "claude-code",
            "hi",
            "/tmp",
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("`claude` not found"), "{err}");
    }

    #[test]
    fn claude_code_in_wsl_launches_like_the_terminal() {
        let cmd = build_engine_command_with(
            &FakeResolver::wsl(&["claude"]),
            "claude-code",
            "ignored: claude reads the prompt from stdin",
            r"C:\work\proj",
            Some("opus"),
            None,
            Some("sess-1"),
        )
        .unwrap();
        assert_eq!(cmd.as_std().get_program(), "wsl.exe");
        let args = args_of(&cmd);
        assert_eq!(
            &args[..6],
            ["--cd", "C:/work/proj", "-e", "bash", "-l", "-c"]
        );
        assert_eq!(
            args[6],
            "'claude' '--permission-prompt-tool' 'stdio' '--permission-mode' 'default' \
             '--print' '--input-format' 'stream-json' '--output-format' 'stream-json' \
             '--verbose' '--resume' 'sess-1' '--model' 'opus'"
        );
        assert_eq!(args.len(), 7);
    }

    #[test]
    fn wsl_launch_quotes_prompts_for_bash() {
        let cmd = build_engine_command_with(
            &FakeResolver::wsl(&["pi"]),
            "pi",
            "it's $HOME; rm -rf /",
            "/tmp",
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(args_of(&cmd)[6], r"'pi' '-p' 'it'\''s $HOME; rm -rf /'");
    }

    #[test]
    fn codex_in_wsl_gets_a_linux_cd_path() {
        let cmd = build_engine_command_with(
            &FakeResolver::wsl(&["codex"]),
            "codex",
            "",
            r"C:\Users\x\proj",
            None,
            None,
            None,
        )
        .unwrap();
        let args = args_of(&cmd);
        assert_eq!(args[1], "C:/Users/x/proj");
        assert!(
            args[6].contains("'--cd' '/mnt/c/Users/x/proj'"),
            "{}",
            args[6]
        );
        assert_eq!(to_wsl_path("/already/linux"), "/already/linux");
    }

    #[tokio::test]
    async fn spawn_failure_marks_the_run_failed() {
        let db = test_db().await;
        let cache = ChiCache::new(std::env::temp_dir());
        let run_id = uuid::Uuid::new_v4().to_string();
        let opts = ChiRunOpts {
            engine_id: "claude-code".into(),
            prompt: "hello".into(),
            cwd: None,
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };
        cache_insert(&db, &run_id, &opts, &cache.run_output_path(&run_id), "cli")
            .await
            .unwrap();

        // Resolution failure (engine not installed).
        let err = spawn_engine_or_fail(
            &db,
            &run_id,
            Err("engine binary `claude` not found on PATH or inside WSL".into()),
        )
        .await
        .unwrap_err();
        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.error.as_deref(), Some(err.as_str()));
        assert!(row.ended_at.is_some());

        // OS spawn failure (resolved, but the program can't start).
        cache_update_status(&db, &run_id, "running", None)
            .await
            .unwrap();
        let bogus = Command::new("ikenga-definitely-not-a-real-binary");
        let err = spawn_engine_or_fail(&db, &run_id, Ok(bogus))
            .await
            .unwrap_err();
        assert!(err.starts_with("spawn engine:"), "{err}");
        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(row.error.as_deref(), Some(err.as_str()));
    }

    /// WP-40: a Chi run reaching `failed` / `done` produces a `run_failed` /
    /// `run_finished` notification; `cancelled` produces none.
    #[tokio::test]
    async fn terminal_run_statuses_produce_run_notifications() {
        use crate::notifications::{self, ListQuery, NotificationKind};

        let db = test_db().await;
        let cache = ChiCache::new(std::env::temp_dir());
        let mk_opts = || ChiRunOpts {
            engine_id: "claude-code".into(),
            prompt: "pulse-refresh".into(),
            cwd: Some("/tmp/royalti-co".into()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };
        let failed_id = uuid::Uuid::new_v4().to_string();
        let done_id = uuid::Uuid::new_v4().to_string();
        let cancelled_id = uuid::Uuid::new_v4().to_string();
        for id in [&failed_id, &done_id, &cancelled_id] {
            cache_insert(&db, id, &mk_opts(), &cache.run_output_path(id), "cli")
                .await
                .unwrap();
        }
        cache_update_done(&db, &failed_id, "failed", Some("exit 1"), false, None)
            .await
            .unwrap();
        let produced = serde_json::json!([
            { "path": "/tmp/royalti-co/snap-1.json", "mime": "application/json", "producedBy": "Write" }
        ]);
        cache_update_done(&db, &done_id, "done", None, false, Some(&produced))
            .await
            .unwrap();
        cache_update_done(&db, &cancelled_id, "cancelled", None, false, None)
            .await
            .unwrap();

        let pool = db.ensure_pool().await.unwrap();
        let rows = notifications::list(&pool, &ListQuery::default()).await.unwrap();
        let for_run = |id: &str| {
            rows.iter()
                .filter(|n| n.action.as_ref().and_then(|a| a["runId"].as_str()) == Some(id))
                .collect::<Vec<_>>()
        };
        let failed = for_run(&failed_id);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].kind, NotificationKind::RunFailed);
        assert_eq!(failed[0].title, "pulse-refresh failed");
        let done = for_run(&done_id);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].kind, NotificationKind::RunFinished);
        let action = done[0].action.as_ref().unwrap();
        assert_eq!(action["artifactCount"], 1);
        assert_eq!(action["firstArtifactPath"], "/tmp/royalti-co/snap-1.json");
        assert_eq!(failed[0].action.as_ref().unwrap()["artifactCount"], 0);
        assert!(for_run(&cancelled_id).is_empty());
    }

    #[tokio::test]
    async fn spawn_chi_run_records_failed_status_when_the_engine_cannot_start() {
        let db = Arc::new(test_db().await);
        let cache =
            ChiCache::new(std::env::temp_dir().join(format!("chi-test-{}", uuid::Uuid::new_v4())));
        let runtime = Arc::new(ChiRuntime::new());
        let opts = ChiRunOpts {
            // cursor-agent always refuses to build, so this drives the real
            // spawn_chi_run path without depending on what's installed.
            engine_id: "cursor-agent".into(),
            prompt: "hello".into(),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };
        let err = spawn_chi_run(db.clone(), &cache, &runtime, None, opts, "cli")
            .await
            .err()
            .expect("cursor-agent must not start");
        assert!(
            err.contains("cursor-agent runtime not implemented"),
            "{err}"
        );

        let rows = cache_list(&db, Some("cursor-agent"), 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed", "run must not stay queued");
        assert_eq!(rows[0].error.as_deref(), Some(err.as_str()));
        assert!(rows[0].ended_at.is_some());
    }

    #[tokio::test]
    async fn chi_run_openrouter_without_app_handle_fails_closed() {
        let db = Arc::new(test_db().await);
        let cache =
            ChiCache::new(std::env::temp_dir().join(format!("chi-test-{}", uuid::Uuid::new_v4())));
        let runtime = Arc::new(ChiRuntime::new());
        let opts = ChiRunOpts {
            engine_id: "openrouter".into(),
            prompt: "hello".into(),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };
        let err = spawn_chi_run(db.clone(), &cache, &runtime, None, opts, "cli")
            .await
            .err()
            .expect("openrouter without an app handle must not start");
        assert!(err.contains("app handle"), "{err}");

        let rows = cache_list(&db, Some("openrouter"), 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "failed", "run must not stay queued");
        assert_eq!(rows[0].error.as_deref(), Some(err.as_str()));
        assert!(rows[0].ended_at.is_some());
    }

    #[test]
    fn agent_message_delta_extracts_only_visible_reply_text() {
        let chunk = |text: &str| {
            agent_client_protocol::schema::ContentChunk::new(ContentBlock::Text(
                agent_client_protocol::schema::TextContent::new(text.to_string()),
            ))
        };
        assert_eq!(
            agent_message_delta(&SessionUpdate::AgentMessageChunk(chunk("hello "))),
            Some("hello ".to_string())
        );
        // Thinking and tool-call envelopes are surfaced to ACP clients but are
        // not run output.
        assert_eq!(
            agent_message_delta(&SessionUpdate::AgentThoughtChunk(chunk("hmm"))),
            None
        );
        assert_eq!(
            agent_message_delta(&SessionUpdate::UserMessageChunk(chunk("user text"))),
            None
        );
    }

    #[test]
    fn openrouter_turn_outcome_maps_stop_reasons_to_chi_status() {
        let (status, error) =
            openrouter_turn_outcome(&Ok(PromptResponse::new(StopReason::EndTurn)));
        assert_eq!(status, "done");
        assert!(error.is_none());

        let (status, _) = openrouter_turn_outcome(&Ok(PromptResponse::new(StopReason::MaxTokens)));
        assert_eq!(status, "done");

        let (status, error) =
            openrouter_turn_outcome(&Ok(PromptResponse::new(StopReason::Refusal)));
        assert_eq!(status, "failed");
        assert_eq!(
            error.as_deref(),
            Some("openrouter reported stop_reason refusal")
        );

        let (status, error) =
            openrouter_turn_outcome(&Ok(PromptResponse::new(StopReason::Cancelled)));
        assert_eq!(status, "cancelled");
        assert!(error.is_none());

        let (status, error) =
            openrouter_turn_outcome(&Err("openrouter HTTP 429: rate limited".into()));
        assert_eq!(status, "failed");
        assert_eq!(error.as_deref(), Some("openrouter HTTP 429: rate limited"));
    }

    #[test]
    fn partial_output_file_is_valid_run_output() {
        let path = std::env::temp_dir().join(format!("chi-or-flush-{}.json", uuid::Uuid::new_v4()));
        write_output_file_sync(&path, "partial", None);
        let read = std::fs::read_to_string(&path).unwrap();
        let file: RunOutputFile = serde_json::from_str(&read).unwrap();
        assert_eq!(file.output.as_deref(), Some("partial"));
        assert!(file.error.is_none());
        std::fs::remove_file(path).ok();
    }

    // ── WP-18b: detached chi-runner runs ────────────────────────────────

    /// chi-runner's status file parses with its `status` / `external_id`; an
    /// in-process file still parses; a half-written one is "no info".
    #[test]
    fn output_file_parse_reads_runner_fields_and_tolerates_partial_writes() {
        let runner = parse_output_file(
            r#"{"output":"hi","error":null,"done_at":"2026-09-27T10:00:00Z","status":"done","external_id":"sess-1"}"#,
        )
        .unwrap();
        assert_eq!(runner.status.as_deref(), Some("done"));
        assert_eq!(runner.external_id.as_deref(), Some("sess-1"));
        assert_eq!(runner.output.as_deref(), Some("hi"));

        let in_process = parse_output_file(r#"{"output":"x","done_at":"t"}"#).unwrap();
        assert_eq!(in_process.status, None);
        assert_eq!(in_process.external_id, None);

        for partial in [
            "",
            "{",
            r#"{"output":"hel"#,
            r#"{"status":"do"#,
            r#"{"status":"done","#,
        ] {
            assert!(
                parse_output_file(partial).is_none(),
                "{partial:?} must be no-info"
            );
        }
    }

    /// Insert a `running` detached row with `pid`, and (optionally) its
    /// status file with `contents`.
    async fn detached_row(
        db: &PaDb,
        dir: &Path,
        run_id: &str,
        pid: Option<i64>,
        contents: Option<&str>,
    ) {
        let output_path = dir.join(format!("{run_id}.json"));
        let opts = ChiRunOpts {
            engine_id: "claude-code".into(),
            prompt: format!("brief {run_id}"),
            cwd: Some("/tmp/work".into()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: true,
        };
        cache_insert(db, run_id, &opts, &output_path, "cli")
            .await
            .unwrap();
        match pid {
            Some(pid) => cache_mark_detached(db, run_id, pid as u32).await.unwrap(),
            None => cache_update_status(db, run_id, "running", None)
                .await
                .unwrap(),
        }
        if let Some(contents) = contents {
            std::fs::write(&output_path, contents).unwrap();
        }
    }

    /// G-88: every branch of the sweep, and each finished run notified once.
    #[tokio::test]
    async fn reconcile_detached_runs_finishes_each_run_once_and_notifies_once() {
        use crate::commands::chi_runner::PidProbe;
        use crate::notifications::{self, ListQuery, NotificationKind};

        let db = test_db().await;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        detached_row(
            &db,
            dir,
            "done",
            Some(1001),
            Some(r#"{"output":"ok","status":"done","external_id":"sess-done"}"#),
        )
        .await;
        detached_row(
            &db,
            dir,
            "timed-out",
            Some(1002),
            Some(r#"{"status":"timed_out","error":"timed out after 5s"}"#),
        )
        .await;
        detached_row(
            &db,
            dir,
            "crashed",
            Some(1003),
            Some(r#"{"status":"running"}"#),
        )
        .await;
        detached_row(
            &db,
            dir,
            "crashed-partial",
            Some(1004),
            Some(r#"{"status":"do"#),
        )
        .await;
        detached_row(
            &db,
            dir,
            "reused-pid",
            Some(1005),
            Some(r#"{"status":"running"}"#),
        )
        .await;
        detached_row(
            &db,
            dir,
            "alive",
            Some(1006),
            Some(r#"{"status":"running","external_id":"sess-alive"}"#),
        )
        .await;
        detached_row(&db, dir, "alive-no-file", Some(1007), None).await;
        detached_row(&db, dir, "alive-partial", Some(1008), Some(r#"{"outp"#)).await;
        detached_row(&db, dir, "in-process", None, None).await;
        detached_row(&db, dir, "being-cancelled", Some(1009), None).await;

        let probe = |pid: u32| match pid {
            1005 => PidProbe::Foreign,
            1006 | 1007 => PidProbe::Ours,
            1008 => PidProbe::Unverified,
            _ => PidProbe::Dead,
        };
        let guard = CancellingGuard::hold("being-cancelled");

        assert_eq!(
            reconcile_detached_runs_with(&db, dir, &probe)
                .await
                .unwrap(),
            5
        );
        // Idempotent: nothing left to transition, nothing re-notified.
        assert_eq!(
            reconcile_detached_runs_with(&db, dir, &probe)
                .await
                .unwrap(),
            0
        );

        let row = |id: &'static str| {
            let db = &db;
            async move { cache_get(db, id).await.unwrap().unwrap() }
        };
        let done = row("done").await;
        assert_eq!(
            (done.status.as_str(), done.error.as_deref()),
            ("done", None)
        );
        assert_eq!(done.external_id.as_deref(), Some("sess-done"));
        assert!(done.ended_at.is_some());
        let timed_out = row("timed-out").await;
        assert_eq!(timed_out.status, "failed");
        assert_eq!(timed_out.error.as_deref(), Some("timed out after 5s"));
        for id in ["crashed", "crashed-partial", "reused-pid"] {
            let r = row(id).await;
            assert_eq!(r.status, "failed", "{id}");
            assert_eq!(
                r.error.as_deref(),
                Some(crate::server::shared::chi_liveness::RUNNER_EXITED_ERROR),
                "{id}"
            );
        }
        for id in [
            "alive",
            "alive-no-file",
            "alive-partial",
            "in-process",
            "being-cancelled",
        ] {
            assert_eq!(row(id).await.status, "running", "{id}");
        }
        // A live run's engine session id is picked up before it finishes.
        assert_eq!(
            row("alive").await.external_id.as_deref(),
            Some("sess-alive")
        );

        // Released: the next sweep may settle it.
        drop(guard);
        assert_eq!(
            reconcile_detached_runs_with(&db, dir, &probe)
                .await
                .unwrap(),
            1
        );

        let pool = db.ensure_pool().await.unwrap();
        let rows = notifications::list(&pool, &ListQuery::default())
            .await
            .unwrap();
        let for_run = |id: &str| {
            rows.iter()
                .filter(|n| n.action.as_ref().and_then(|a| a["runId"].as_str()) == Some(id))
                .collect::<Vec<_>>()
        };
        let finished = for_run("done");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].kind, NotificationKind::RunFinished);
        assert_eq!(finished[0].count, 1, "notified exactly once");
        for id in [
            "timed-out",
            "crashed",
            "crashed-partial",
            "reused-pid",
            "being-cancelled",
        ] {
            let n = for_run(id);
            assert_eq!(n.len(), 1, "{id}");
            assert_eq!(n[0].kind, NotificationKind::RunFailed, "{id}");
            assert_eq!(n[0].count, 1, "{id} notified exactly once");
        }
        for id in ["alive", "alive-no-file", "alive-partial", "in-process"] {
            assert!(for_run(id).is_empty(), "{id}");
        }
    }

    /// End-to-end against a real `chi-runner` (built from iyke-cli): spawn
    /// detached, record the pid, let the runner fail an unknown engine, and
    /// reconcile. Ignored by default — needs the binary on PATH:
    /// `PATH=<dir with chi-runner>:$PATH cargo test --lib detached_chi_runner_smoke -- --ignored`
    #[tokio::test]
    #[ignore]
    async fn detached_chi_runner_smoke() {
        let db = Arc::new(test_db().await);
        let cache =
            ChiCache::new(std::env::temp_dir().join(format!("chi-smoke-{}", uuid::Uuid::new_v4())));
        let runtime = Arc::new(ChiRuntime::new());
        assert!(
            chi_runner::resolve_runner_path().is_some(),
            "chi-runner not on PATH"
        );
        let opts = ChiRunOpts {
            engine_id: "not-a-runner-engine".into(),
            prompt: "hello".into(),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            model: None,
            mode: None,
            timeout_seconds: Some(30),
            parent_id: None,
            resume_session_id: None,
            persistent: true,
        };
        let res = spawn_chi_run(db.clone(), &cache, &runtime, None, opts, "cli")
            .await
            .unwrap();
        let row = cache_get(&db, &res.run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "running");
        assert!(row.pid.is_some(), "detached run records its pid");

        let mut finished = 0;
        for _ in 0..100 {
            finished +=
                reconcile_detached_runs_with(&db, &cache.cache_dir(), &chi_runner::probe_runner)
                    .await
                    .unwrap();
            if finished > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(finished, 1);
        let row = cache_get(&db, &res.run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "failed");
        assert_eq!(
            row.error.as_deref(),
            Some("engine not supported by chi-runner: not-a-runner-engine")
        );
    }

    /// The transition guard: a run already finished elsewhere (a cancel, the
    /// in-process path) is neither overwritten nor re-notified by the sweep.
    #[tokio::test]
    async fn guarded_finish_leaves_an_already_finished_run_alone() {
        let db = test_db().await;
        let tmp = tempfile::tempdir().unwrap();
        detached_row(&db, tmp.path(), "cancelled", Some(2001), None).await;
        cache_update_status(&db, "cancelled", "cancelled", None)
            .await
            .unwrap();

        assert!(
            !cache_update_done_if_live(&db, "cancelled", "failed", Some("x"))
                .await
                .unwrap()
        );
        let r = cache_get(&db, "cancelled").await.unwrap().unwrap();
        assert_eq!(r.status, "cancelled");
        assert_eq!(r.error, None);
        assert_eq!(r.pid, Some(2001));
    }
}
