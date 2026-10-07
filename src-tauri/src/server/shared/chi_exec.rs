//! The write side of Chi runs — `chi_run`, `chi_resume`, `chi_cancel` and the
//! detached-run sweep — shared by the desktop `#[tauri::command]`s in
//! `commands::chi` and the headless daemon's `/api/rpc` arms (WP-P10).
//!
//! Moved out of the desktop-only `commands::chi` on the WP-19 pattern: the
//! logic lives here, compiled into both binaries, and each surface resolves
//! its own state into a [`ChiEnv`] and calls in. One implementation, so the
//! two surfaces cannot drift in behaviour, row shape or output-file shape.
//!
//! What stays on the desktop is only what needs an `AppHandle`: the
//! CLI-less `openrouter` engine (its adapter lives in the managed
//! `EngineRegistry` and reads its key from the vault). It plugs in through
//! [`InProcessEngines`]; the daemon passes [`NoInProcessEngines`], which
//! refuses such runs cleanly instead of failing binary resolution.
//!
//! # Identity (T1)
//!
//! Every process this module starts goes through `executor::current()` —
//! the engine child here, the detached `chi-runner` in [`super::chi_runner`].
//! In a T1 principal child that executor spawns as the child's own uid (the
//! signed-in principal's), which the child verified at boot (no caps,
//! `NoNewPrivs`). Nothing here reads a principal from the request: the run
//! belongs to whoever's child serves the RPC. [`guard_identity`] additionally
//! refuses any spawn or signal from a process that is root while the
//! session tier is above T0 — the broker, which must never run an arm.
//!
//! Run ids are looked up in this process's own `ikenga.db`, so another
//! principal's run id is indistinguishable from an unknown one ("chi run not
//! found"): there is no cross-principal read, resume or cancel to refuse.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use sqlx::Row;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

use super::acp_mode::AcpSessionMode;
use super::agents::WslLookup;
use super::chi::{
    cache_get, pid_alive, read_output_file, resolve_output_path, ChiCacheRow, ChiRunResult,
    OutputFiles, RunOutputFile,
};
use super::chi_runner::{self, RunLiveness};
use super::wsl_health::WslHealth;
use super::claude_sessions::event::ChatEvent;
use super::claude_sessions::stream_parser::StreamParser;
use crate::db::PaDb;
use crate::engines::codex_pty::parser as codex_parser;
use crate::executor::{ExecutorTier, PipedOpts, SpawnSpec, StdioMode};

// ═══════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════

/// `chi_run`'s options. The desktop command and `/iyke/chi/run` deserialize
/// it directly (camelCase); the daemon arm decodes it field by field
/// (`server::rpc_local::chi_run_opts`) so both spellings are accepted.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChiRunOpts {
    pub engine_id: String,
    pub prompt: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub timeout_seconds: Option<u32>,
    pub parent_id: Option<String>,
    #[serde(rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    /// If true, launch the run as a detached `chi-runner` (WP-18b) so it
    /// survives an app (or daemon) restart. Falls back to in-process when
    /// chi-runner can't be found or spawned. The runner's pid is stored in
    /// `chi_cache.pid`.
    #[serde(default)]
    pub persistent: bool,
}

/// Rebuild a run's opts from its cache row for `chi_resume` — only the fields
/// the resume paths actually read (engine id, prompt, model, mode). Only the
/// desktop's in-process (openrouter) resume needs it.
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) fn row_into_resume_opts(row: &ChiCacheRow, prompt: String) -> ChiRunOpts {
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

/// Live in-process Chi runs, so `chi_cancel` can reach them. The desktop
/// `.manage()`s one; the daemon holds one in `AppState`.
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

    /// Drop `handle` once its run has finished — but only if it is still the
    /// one registered: a resume may have put a newer turn under the same id.
    async fn release(&self, run_id: &str, handle: &Arc<ChiRunHandle>) {
        let mut running = self.running.lock().await;
        if running.get(run_id).is_some_and(|h| Arc::ptr_eq(h, handle)) {
            running.remove(run_id);
        }
    }

    #[cfg(test)]
    pub(crate) async fn len(&self) -> usize {
        self.running.lock().await.len()
    }
}

/// How `chi_cancel` interrupts a run that has no OS child (an in-process
/// engine: the desktop's openrouter adapter).
pub trait RunInterrupt: Send + Sync {
    fn interrupt(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// Handle to a live Chi run so `chi_cancel` can interrupt it.
pub struct ChiRunHandle {
    /// The actual OS child process. Shared with the reader task. `None` for
    /// CLI-less engines driven in-process — there is no child to kill, and
    /// `in_process` is the interrupt instead.
    pub child: Option<Arc<Mutex<Child>>>,
    /// Set to true by `chi_cancel`, read by the reader task.
    pub cancelled: Arc<AtomicBool>,
    /// Cancel hook for in-process engines (no child process to kill).
    pub in_process: Option<Arc<dyn RunInterrupt>>,
}

/// Where engine binaries are looked up. A trait so tests can stand in for
/// the host PATH and WSL.
pub(crate) trait EngineResolver: Send + Sync {
    /// Resolved host path of `binary`, if it is on the (augmented) PATH.
    fn native(&self, binary: &str) -> Option<PathBuf>;
    /// Whether `binary` is on `distro`'s login PATH (`None` = the default
    /// distro) — or that WSL couldn't be asked, which is not the same as "no".
    fn in_wsl<'a>(&'a self, binary: &'a str, distro: Option<&'a str>) -> BoxFuture<'a, WslLookup>;
    /// The distro WSL launches use (`engines.agentWslDistro`); `None` = the
    /// default distro.
    fn wsl_distro(&self) -> Option<String> {
        None
    }
    /// WSL network health for a launch in `distro` (D-21), and whether it
    /// was freshly measured; `None` = don't probe (stub resolvers, hosts
    /// without WSL). Only asked for a WSL launch, never a native one.
    ///
    /// Contract: this only *measures*. Raising / resolving the
    /// `fix.wsl_network` notification is [`ReportingResolver`]'s job, so a
    /// caller of [`resolve_engine`] / [`build_engine_command_with`] that
    /// holds a `PaDb` passes its resolver wrapped in one (as `spawn_run` /
    /// `resume_run` do via `ChiEnv::reporting_resolver`); a bare resolver
    /// still fails the launch, it just raises no notification.
    fn wsl_health<'a>(
        &'a self,
        _distro: Option<&'a str>,
    ) -> BoxFuture<'a, Option<(WslHealth, bool)>> {
        Box::pin(std::future::ready(None))
    }
}

/// The real resolver: augmented host PATH first, then WSL on Windows.
pub(crate) struct HostResolver;

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

    #[cfg(all(windows, feature = "desktop"))]
    fn in_wsl<'a>(&'a self, binary: &'a str, distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        Box::pin(async move {
            // `wsl.exe … command -v` costs ~0.5–2 s warm (far more cold), so
            // remember hits — per distro — for the life of the process.
            // Misses are re-probed: the user may install the CLI while the
            // shell is running. "WSL unavailable" is never remembered.
            type Hits = std::sync::Mutex<std::collections::HashSet<(Option<String>, String)>>;
            static FOUND: std::sync::OnceLock<Hits> = std::sync::OnceLock::new();
            let found = FOUND.get_or_init(Default::default);
            let key = (distro.map(str::to_string), binary.to_string());
            if found.lock().map(|s| s.contains(&key)).unwrap_or(false) {
                return WslLookup::Found(binary.to_string());
            }
            let lookup = super::agents::wsl_which(binary, distro).await;
            if matches!(lookup, WslLookup::Found(_)) {
                if let Ok(mut s) = found.lock() {
                    s.insert(key);
                }
            }
            lookup
        })
    }

    /// The daemon (and every non-Windows build) resolves on the host PATH
    /// only.
    #[cfg(not(all(windows, feature = "desktop")))]
    fn in_wsl<'a>(&'a self, _binary: &'a str, _distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        Box::pin(std::future::ready(WslLookup::NotFound))
    }

    fn wsl_distro(&self) -> Option<String> {
        if cfg!(windows) {
            super::wsl::configured_distro()
        } else {
            None
        }
    }

    /// The shared WSL health probe, reusing a result younger than its 30 s
    /// cache (D-7). Bounded: every spawn in it has a timeout.
    #[cfg(windows)]
    fn wsl_health<'a>(
        &'a self,
        distro: Option<&'a str>,
    ) -> BoxFuture<'a, Option<(WslHealth, bool)>> {
        Box::pin(async move { Some(super::wsl_health::probe(distro, false).await) })
    }
}

/// An [`EngineResolver`] whose fresh WSL health probes are reported to the
/// `notifications` table (`fix.wsl_network`, D-21) — the same record /
/// resolve the desktop's `wsl_health_probe` command does. Wraps the run's
/// resolver for the spawn paths, which hold the run's `PaDb`.
struct ReportingResolver<'a> {
    inner: &'a dyn EngineResolver,
    db: &'a PaDb,
}

impl EngineResolver for ReportingResolver<'_> {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        self.inner.native(binary)
    }
    fn in_wsl<'a>(&'a self, binary: &'a str, distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        self.inner.in_wsl(binary, distro)
    }
    fn wsl_distro(&self) -> Option<String> {
        self.inner.wsl_distro()
    }
    fn wsl_health<'a>(
        &'a self,
        distro: Option<&'a str>,
    ) -> BoxFuture<'a, Option<(WslHealth, bool)>> {
        Box::pin(async move {
            let probed = self.inner.wsl_health(distro).await;
            // A cached result was reported when it was measured; an
            // inconclusive one (wsl.exe timed out) neither raises nor ends
            // an episode — it doesn't fail the run either.
            if let Some((health, true)) = &probed {
                if !health.inconclusive {
                    super::notifications::wsl::report_with_db(self.db, health).await;
                }
            }
            probed
        })
    }
}

/// How long the pre-run WSL check may hold up a run's start. A warm probe
/// takes a few seconds; a `wsl.exe` that times out (twice) would take ~54 s.
const WSL_PREFLIGHT_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// The pre-run WSL check (D-21): a launch inside WSL whose network is
/// broken in a way that is WSL's own fault (`no_route`, `dns_only`,
/// `wsl_down`) fails now with the cause, instead of spawning an engine that
/// can't sign in or reach its API. `ok` and `host_offline` (Windows is
/// offline too — not WSL's fault, and not ours to block) proceed, as does
/// `not_installed` (the binary lookup that got us here already answered).
///
/// "Couldn't tell" proceeds too: a probe that outruns
/// [`WSL_PREFLIGHT_BUDGET`], or a `wsl_down` that rests only on `wsl.exe`
/// timing out ([`WslHealth::inconclusive`]) — the binary lookup that got us
/// here has just seen WSL answer, so a slow probe is not evidence it's down.
async fn wsl_preflight(resolver: &dyn EngineResolver, distro: Option<&str>) -> Result<(), String> {
    wsl_preflight_within(resolver, distro, WSL_PREFLIGHT_BUDGET).await
}

async fn wsl_preflight_within(
    resolver: &dyn EngineResolver,
    distro: Option<&str>,
    budget: std::time::Duration,
) -> Result<(), String> {
    let probed = match tokio::time::timeout(budget, resolver.wsl_health(distro)).await {
        Ok(probed) => probed,
        Err(_) => {
            tracing::warn!(
                target: "ikenga::chi",
                "WSL health check took over {}s; launching without it",
                budget.as_secs()
            );
            return Ok(());
        }
    };
    match probed {
        Some((health, _)) if health.state.is_wsl_fault() && health.inconclusive => {
            tracing::warn!(
                target: "ikenga::chi",
                "WSL health check inconclusive ({}); launching anyway",
                health.detail
            );
            Ok(())
        }
        Some((health, _)) if health.state.is_wsl_fault() => {
            Err(format!("WSL has no network — {}", health.detail))
        }
        _ => Ok(()),
    }
}

/// Whether an engine binary is installed, for seat install state: on the
/// host PATH, or inside WSL on Windows. `None` = couldn't tell (WSL didn't
/// answer); callers must not read that as "not installed".
#[cfg_attr(not(feature = "desktop"), allow(dead_code))]
pub(crate) async fn engine_installed(binary: &str) -> Option<bool> {
    let resolver = HostResolver;
    if resolver.native(binary).is_some() {
        return Some(true);
    }
    let distro = resolver.wsl_distro();
    match resolver.in_wsl(binary, distro.as_deref()).await {
        WslLookup::Found(_) => Some(true),
        WslLookup::NotFound => Some(false),
        WslLookup::WslUnavailable(reason) => {
            tracing::warn!(
                target: "ikenga::chi",
                "couldn't check WSL for `{binary}`: {reason}"
            );
            None
        }
    }
}

/// Everything a Chi write needs from the surface calling it.
///
/// * Desktop: the managed `PaDb`, `<app_data_dir>/chi-cache`, the managed
///   `ChiRuntime`, no default cwd (the process cwd, as always), output paths
///   [`OutputFiles::AsStored`], the host resolver.
/// * Daemon: `<data-dir>/ikenga.db` and `<data-dir>/chi-cache`, the
///   `AppState` runtime, the router home as the default cwd (the principal's
///   home under T1), output paths [`OutputFiles::InCacheDir`].
pub(crate) struct ChiEnv {
    pub db: Arc<PaDb>,
    pub cache_dir: PathBuf,
    pub runtime: Arc<ChiRuntime>,
    /// cwd for a run that names none. `None` keeps the desktop's historical
    /// fallback, the process's current directory.
    pub default_cwd: Option<PathBuf>,
    /// Whether a row's stored `output_path` may point anywhere (desktop) or
    /// must stay inside `cache_dir` (daemon; `db_exec` can plant rows).
    pub files: OutputFiles,
    pub resolver: Arc<dyn EngineResolver>,
    /// How a run's `cwd` is expanded (see [`CwdExpansion`]).
    pub cwd_expansion: CwdExpansion,
}

/// How a run's `cwd` is expanded before it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CwdExpansion {
    /// `~` and `$VAR`, against this process's environment: the desktop's
    /// historical behaviour. The desktop user owns that environment.
    Full,
    /// `~` only (the daemon). The daemon's environment carries operator
    /// credentials — every `IKENGA_SECRET_*` default — that a caller holding
    /// only Dispatch must not read (they need Secrets, `secrets_get`), and a
    /// run's cwd reaches the engine's argv (codex `--cd`) and the detached
    /// runner's conf file. A `$` in a daemon-supplied cwd stays literal.
    TildeOnly,
}

// ── The prompt never rides argv (I-7) ─────────────────────────────────────
//
// Any uid on the host can read any process's argv from `/proc/<pid>/cmdline`
// unless the *reader's* procfs is mounted `hidepid` — and that is a property
// of the reader's mount namespace, not ours: the T1 unit's
// `ProtectProc=invisible` hides nothing from a principal who is signed in
// over SSH on the host `/proc`. So no check this process can make proves the
// argv private, and every engine takes the run's prompt (and a resume's
// follow-up) on stdin instead — a pipe only this process and the engine
// hold. `build_engine_command_with` takes no prompt at all, so an arm cannot
// put it back on the command line; `no_engine_ever_sees_the_prompt_in_argv`
// runs each engine against a stub that records its argv and stdin.
//
// The detached `chi-runner` (iyke-cli) is a separate binary that builds its
// own engine argv: it puts the antigravity-cli prompt on the command line
// (`agy -p <prompt>`) and doesn't run opencode or pi at all. Only the engines
// it feeds on stdin ([`RUNNER_STDIN_ENGINES`]) go detached; a persistent run
// of any other engine runs in-process and says so (see [`spawn_run`]).

/// How an engine reads a run's prompt from stdin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptStdin {
    /// `claude --input-format stream-json`: one `{"type":"user",…}` line.
    ClaudeStreamJson,
    /// `agy --input-format stream-json` (agy 1.1.15+): one
    /// `{"event":"user","message":{"content":…}}` line per turn.
    AgyStreamJson,
    /// The bare text, read to EOF: `codex exec -`, `opencode run` (reads a
    /// non-TTY stdin as the message), `pi --mode json` (merges piped stdin
    /// into the initial prompt).
    Raw,
}

/// How `engine_id` takes its prompt. Every engine takes it on stdin.
pub(crate) fn prompt_stdin(engine_id: &str) -> PromptStdin {
    match engine_id {
        "claude-code" => PromptStdin::ClaudeStreamJson,
        "antigravity-cli" => PromptStdin::AgyStreamJson,
        _ => PromptStdin::Raw,
    }
}

/// The bytes written to `engine_id`'s stdin for `prompt` (stdin is closed
/// after them, which ends the turn).
pub(crate) fn stdin_payload(engine_id: &str, prompt: &str) -> String {
    match prompt_stdin(engine_id) {
        PromptStdin::ClaudeStreamJson => user_envelope(prompt),
        PromptStdin::AgyStreamJson => {
            let value = serde_json::json!({
                "event": "user",
                "message": { "content": prompt },
            });
            let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
            s.push('\n');
            s
        }
        PromptStdin::Raw => prompt.to_string(),
    }
}

/// Engines the detached `chi-runner` hands the prompt on stdin. It builds
/// `agy -p <prompt>` for antigravity-cli and refuses opencode / pi, so a
/// persistent run of those stays in-process instead.
pub(crate) const RUNNER_STDIN_ENGINES: &[&str] = &["claude-code", "codex"];

/// The warning a persistent run of an engine outside
/// [`RUNNER_STDIN_ENGINES`] carries: it ran in-process, so it will not
/// survive a restart. Never names the prompt.
pub(crate) fn not_detachable_warning(engine_id: &str) -> String {
    format!(
        "persistent run fell back to in-process: chi-runner would pass the {engine_id} prompt \
         on the command line, where other users on this host can read it, so {engine_id} runs \
         only in-process. This run will NOT survive quitting the app."
    )
}

impl ChiEnv {
    /// The desktop's shape: no default cwd, stored output paths, host PATH,
    /// full cwd expansion.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub(crate) fn new(db: Arc<PaDb>, cache_dir: PathBuf, runtime: Arc<ChiRuntime>) -> Self {
        Self {
            db,
            cache_dir,
            runtime,
            default_cwd: None,
            files: OutputFiles::AsStored,
            resolver: Arc::new(HostResolver),
            cwd_expansion: CwdExpansion::Full,
        }
    }

    /// The run's resolver, reporting its WSL pre-run probes (D-21) to the
    /// run's `notifications` table.
    fn reporting_resolver(&self) -> ReportingResolver<'_> {
        ReportingResolver {
            inner: &*self.resolver,
            db: &self.db,
        }
    }

    /// Per-run artifact / output tail file.
    pub(crate) fn run_output_path(&self, run_id: &str) -> PathBuf {
        self.cache_dir.join(format!("{run_id}.json"))
    }

    /// Create the cache dir, owner-only on Unix: it holds each run's output
    /// and a persistent run's runner conf (which carries the prompt).
    pub(crate) fn ensure_cache_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| format!("chi-cache dir: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.cache_dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("chi-cache dir permissions: {e}"))?;
        }
        Ok(())
    }

    /// cwd for a run: the caller's, else the surface default, else the
    /// process cwd, else `.`; expanded per [`CwdExpansion`] (`~` / `$VAR` on
    /// the desktop as it always has, `~` only on the daemon).
    pub(crate) fn run_cwd(&self, requested: Option<&str>) -> String {
        let cwd = requested
            .map(str::to_string)
            .or_else(|| {
                self.default_cwd
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
            })
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| ".".to_string());
        match self.cwd_expansion {
            CwdExpansion::Full => shellexpand::full(&cwd)
                .map(|c| c.into_owned())
                .unwrap_or(cwd),
            CwdExpansion::TildeOnly => shellexpand::tilde(&cwd).into_owned(),
        }
    }
}

/// Engines driven in-process (no CLI to spawn). The desktop implements it for
/// `openrouter`; the daemon passes [`NoInProcessEngines`].
pub(crate) trait InProcessEngines: Send + Sync {
    /// Whether `engine_id` is one of these (and so must not go to the CLI
    /// path).
    fn handles(&self, engine_id: &str) -> bool;

    /// Start a new run whose cache row already exists (`queued`). Must close
    /// the row (`cache_update_done`) itself on failure.
    fn start<'a>(
        &'a self,
        env: &'a ChiEnv,
        run_id: String,
        output_path: PathBuf,
        opts: &'a ChiRunOpts,
        cwd: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>>;

    /// Resume `row` with `prompt`. Any detached runner the row recorded has
    /// already been settled.
    fn resume<'a>(
        &'a self,
        env: &'a ChiEnv,
        row: ChiCacheRow,
        output_path: PathBuf,
        prompt: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>>;
}

/// What the headless daemon answers for an in-process engine.
pub(crate) const HEADLESS_OPENROUTER: &str =
    "openrouter chi runs need the desktop app (engine registry + vault); not available on the \
     headless daemon";

/// The daemon's [`InProcessEngines`]: `openrouter` is recognised (so it is
/// not mistaken for a CLI engine) and refused — fail-closed, the row closed
/// out as `failed` with the reason.
pub(crate) struct NoInProcessEngines;

impl InProcessEngines for NoInProcessEngines {
    fn handles(&self, engine_id: &str) -> bool {
        engine_id == "openrouter"
    }

    fn start<'a>(
        &'a self,
        env: &'a ChiEnv,
        run_id: String,
        _output_path: PathBuf,
        _opts: &'a ChiRunOpts,
        _cwd: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>> {
        Box::pin(async move {
            tracing::warn!(target: "ikenga::chi", "chi run {run_id}: {HEADLESS_OPENROUTER}");
            cache_update_done(
                &env.db,
                &run_id,
                "failed",
                Some(HEADLESS_OPENROUTER),
                false,
                None,
            )
            .await
            .ok();
            Err(HEADLESS_OPENROUTER.to_string())
        })
    }

    fn resume<'a>(
        &'a self,
        _env: &'a ChiEnv,
        _row: ChiCacheRow,
        _output_path: PathBuf,
        _prompt: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>> {
        Box::pin(async { Err(HEADLESS_OPENROUTER.to_string()) })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Identity guard and environment hygiene
// ═══════════════════════════════════════════════════════════════════════

/// The I-1 belt (principal contract): above T0 a Chi run must execute as
/// the signed-in principal, so a root process — the broker — may neither
/// spawn one nor signal one. The principal child it should be running in is
/// never root (verified at its boot). `euid` is `None` where there is no
/// such notion (Windows), which is never above T0.
pub(crate) fn identity_refusal(tier: ExecutorTier, euid: Option<u32>) -> Option<String> {
    (tier != ExecutorTier::T0 && euid == Some(0)).then(|| {
        format!(
            "refusing to run or signal a chi run as root under executor tier {}: chi runs \
             execute in the signed-in principal's own process",
            tier.as_str()
        )
    })
}

fn guard_identity() -> Result<(), String> {
    #[cfg(unix)]
    let euid = Some(unsafe { libc::geteuid() });
    #[cfg(not(unix))]
    let euid = None;
    match identity_refusal(crate::executor::current().tier(), euid) {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

/// `vars` minus the host-only secrets (`pty::is_host_only_env`: the daemon
/// bearer, the vault and principal-store keys, every `IKENGA_SECRET_*`
/// operator default). Pure so tests need not touch the process env.
pub(crate) fn scrubbed_env<I>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .filter(|(k, _)| !crate::pty::is_host_only_env(&k.to_string_lossy()))
        .collect()
}

/// Give `spec` this process's environment minus the host-only secrets,
/// explicitly (`env_clear` first — filtering without clearing filters
/// nothing, see `pty::spawn`). Agent CLIs need the user's real environment;
/// they must not get the daemon's own credentials.
pub(crate) fn inherit_scrubbed_env(spec: &mut SpawnSpec) {
    spec.env_clear();
    spec.envs(scrubbed_env(std::env::vars_os()));
}

/// The output file a resume will write, confined to the cache dir when the
/// surface asks for it. Stricter than the read-side confine in
/// `shared::chi`: a missing file is only fine when its parent IS inside the
/// cache dir, and a symlink is refused outright (a write follows it).
fn confine_for_write(cache_dir: &Path, path: &Path) -> Result<(), String> {
    let refuse = || {
        format!(
            "chi run output path is outside the chi cache dir: {}",
            path.display()
        )
    };
    let dir = std::fs::canonicalize(cache_dir).map_err(|_| refuse())?;
    if std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(refuse());
    }
    if let Ok(canon) = std::fs::canonicalize(path) {
        return if canon.starts_with(&dir) {
            Ok(())
        } else {
            Err(refuse())
        };
    }
    let parent = path.parent().ok_or_else(refuse)?;
    let parent = std::fs::canonicalize(parent).map_err(|_| refuse())?;
    if parent.starts_with(&dir) && path.file_name().is_some() {
        Ok(())
    } else {
        Err(refuse())
    }
}

// ═══════════════════════════════════════════════════════════════════════
// chi_cache writers
// ═══════════════════════════════════════════════════════════════════════

pub(crate) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn one_hour_from_now_iso() -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::hours(1))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Insert a cache row from the options (`queued`).
pub(crate) async fn cache_insert(
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

pub(crate) async fn cache_update_status(
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

pub(crate) async fn cache_update_external_id(
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
pub(crate) async fn cache_mark_detached(db: &PaDb, run_id: &str, pid: u32) -> Result<(), String> {
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

pub(crate) async fn cache_update_done(
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
pub(crate) async fn cache_update_done_if_live(
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
/// failures, stdin failures, and — via [`reconcile_detached_runs_with`] —
/// detached chi-runner runs that finish out of process), so producing here
/// covers them all. `cancelled` produces nothing (a human did it).
/// Best-effort. The run's artifacts ride along (count + first path) so the
/// row can "Open artifact". On the daemon the row lands in this process's own
/// `notifications` table — the principal's, under T1.
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
            tracing::warn!(target: "ikenga::chi", "chi run {run_id}: notification lookup failed: {e}");
            return;
        }
    };
    if let Some(new) = super::notifications::run::run_terminal_with_artifacts(
        run_id,
        status,
        &row.engine_id,
        row.brief.as_deref(),
        row.cwd.as_deref(),
        error,
        artifacts,
    ) {
        super::notifications::record_with_db(db, new).await;
    }
}

pub(crate) async fn write_output_file(
    path: &Path,
    output: &str,
    error: Option<&str>,
) -> Result<(), String> {
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

// ═══════════════════════════════════════════════════════════════════════
// Engine command construction
// ═══════════════════════════════════════════════════════════════════════

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
pub(crate) enum EngineLaunch {
    /// A host executable. On Windows a resolved `.cmd` / `.bat` shim runs
    /// through `cmd.exe /c`.
    Native(PathBuf),
    /// Found only inside WSL (e.g. Claude Code installed in the distro, not
    /// on Windows). Launched as
    /// `wsl.exe [-d <distro>] --cd <cwd> -e bash -l -c '<bin> <args>'` — the
    /// same shape, and the same configured distro, the interactive terminal
    /// uses (`src/terminal/claude-wrap.ts`).
    Wsl {
        binary: String,
        /// The distro the run's cwd lives in when it is a distro share path,
        /// else `engines.agentWslDistro`; `None` = the default distro.
        distro: Option<String>,
    },
}

/// The distro a WSL run in `cwd` launches in: the one a distro share path
/// (`\\wsl.localhost\<distro>\…`) names — its directory exists only there —
/// else the configured one.
fn launch_distro(cwd: &str, resolver: &dyn EngineResolver) -> Option<String> {
    match super::wsl::unc_to_linux(cwd) {
        Some((distro, _)) => Some(distro),
        None => resolver.wsl_distro(),
    }
}

pub(crate) async fn resolve_engine(
    binary: &str,
    resolver: &dyn EngineResolver,
    cwd: &str,
) -> Result<EngineLaunch, String> {
    if let Some(path) = resolver.native(binary) {
        return Ok(EngineLaunch::Native(path));
    }
    let distro = launch_distro(cwd, resolver);
    match resolver.in_wsl(binary, distro.as_deref()).await {
        WslLookup::Found(_) => {
            wsl_preflight(resolver, distro.as_deref()).await?;
            Ok(EngineLaunch::Wsl {
                binary: binary.to_string(),
                distro,
            })
        }
        // Not "install it": the CLI may well be installed in a WSL that
        // didn't answer, and reinstalling would not help.
        WslLookup::WslUnavailable(reason) => Err(format!(
            "engine binary `{binary}` is not on PATH and couldn't be looked up inside WSL — \
             WSL unavailable: {reason}"
        )),
        WslLookup::NotFound => {
            let searched = if cfg!(windows) {
                "on PATH or inside WSL"
            } else {
                "on PATH"
            };
            Err(format!(
                "engine binary `{binary}` not found {searched} — install it or add it to PATH"
            ))
        }
    }
}

/// Single-quote `s` for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Stdio wiring for every in-process engine child: all three streams piped
/// (the run task writes the prompt to stdin and reads stdout/stderr),
/// `kill_on_drop` off (tokio's default — cancel goes through
/// `ChiRunHandle`), console flash suppressed on Windows.
pub(crate) const ENGINE_PIPED_OPTS: PipedOpts = PipedOpts {
    stdin: StdioMode::Piped,
    stdout: StdioMode::Piped,
    stderr: StdioMode::Piped,
    kill_on_drop: false,
    no_console_window: true,
    detached: false,
    new_process_group: false,
};

/// The `--cd` a WSL launch hands `wsl.exe`. A drive path stays a Windows
/// path, which `wsl.exe` translates itself (honouring the distro's automount
/// root); a distro share path becomes the Linux path inside the distro.
fn wsl_cd(cwd: &str) -> String {
    match super::wsl::unc_to_linux(cwd) {
        Some((_distro, linux)) => linux,
        None => cwd.replace('\\', "/"),
    }
}

/// Build the engine spawn spec for `launch` with `args`; spawned with
/// [`ENGINE_PIPED_OPTS`]. `set_cwd` is false for engines that take the
/// directory as a flag instead (codex `--cd`). A WSL launch never sets a
/// host-side cwd: the directory may exist only inside the distro, where
/// Windows can't enter it ("The directory name is invalid", os error 267) —
/// it goes to `wsl.exe --cd` alone. The env is this process's minus the
/// host-only secrets ([`inherit_scrubbed_env`]), plus the augmented `PATH`
/// on a native launch.
pub(crate) fn engine_command(
    launch: &EngineLaunch,
    args: &[String],
    cwd: &str,
    set_cwd: bool,
) -> SpawnSpec {
    let mut spec = match launch {
        EngineLaunch::Native(path) => {
            let is_batch = cfg!(windows)
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
                    .unwrap_or(false);
            let mut spec = if is_batch {
                let mut spec = SpawnSpec::new("cmd.exe");
                spec.arg("/c").arg(path);
                spec
            } else {
                SpawnSpec::new(path)
            };
            spec.args(args);
            inherit_scrubbed_env(&mut spec);
            spec.env("PATH", crate::runtime::augmented_path());
            spec
        }
        EngineLaunch::Wsl { binary, distro } => {
            let script = std::iter::once(binary.as_str())
                .chain(args.iter().map(String::as_str))
                .map(sh_quote)
                .collect::<Vec<_>>()
                .join(" ");
            let mut spec = SpawnSpec::new("wsl.exe");
            spec.args(super::wsl::distro_args(distro.as_deref()));
            spec.args(["--cd", &wsl_cd(cwd), "-e", "bash", "-l", "-c", &script]);
            inherit_scrubbed_env(&mut spec);
            return spec;
        }
    };
    if set_cwd {
        spec.current_dir(cwd);
    }
    spec
}

/// The `--model` a Claude Code chi run launches with: the run's own model,
/// else the catalog default for the `chi` role (WP-11).
pub(crate) fn chi_claude_model(model: Option<&str>) -> Option<String> {
    super::claude_launch::resolve_model(model, Some("chi"))
}

/// The model a run hands chi-runner: resolved like the in-process spawn for
/// Claude Code, passed through unchanged for every other engine.
pub(crate) fn runner_model(engine_id: &str, model: Option<&str>) -> Option<String> {
    if engine_id == "claude-code" {
        chi_claude_model(model)
    } else {
        model.map(str::to_string)
    }
}

/// Return the command for the requested engine. It takes no prompt: every
/// engine reads it from stdin ([`stdin_payload`]), never from argv (I-7).
pub(crate) async fn build_engine_command_with(
    resolver: &dyn EngineResolver,
    engine_id: &str,
    cwd: &str,
    model: Option<&str>,
    mode: Option<&str>,
    resume_id: Option<&str>,
) -> Result<SpawnSpec, String> {
    let s = |v: &str| v.to_string();
    match engine_id {
        "claude-code" => {
            let permission_mode = mode
                .and_then(AcpSessionMode::from_acp_id)
                .unwrap_or_default()
                .as_claude_flag();

            let launch = resolve_engine("claude", resolver, cwd).await?;
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
            // Chi's role default (Sonnet, from the model catalog) applies only
            // when the run names no model; `iyke chi run --model` still wins.
            if let Some(m) = chi_claude_model(model) {
                args.extend([s("--model"), m]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        // agy's print mode takes the prompt as `-p <prompt>`; its stream-json
        // input (no `-p`) reads one `{"event":"user",…}` line per turn from
        // stdin instead, and ends at EOF.
        "antigravity-cli" => {
            let launch = resolve_engine("agy", resolver, cwd).await?;
            let mut args = vec![
                s("--input-format"),
                s("stream-json"),
                s("--output-format"),
                s("stream-json"),
            ];
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
            let launch = resolve_engine("codex", resolver, cwd).await?;
            let mut args = match resume_id {
                Some(id) => vec![s("exec"), s("resume"), s(id), s("--json")],
                None => vec![s("exec"), s("--json")],
            };
            // `--cd` is read by codex itself. A WSL codex is already started
            // in the directory (`wsl.exe --cd`, which translates drive paths
            // through the distro's own automount root), so it gets `.` rather
            // than a guessed `/mnt/<drive>` path.
            let codex_cwd = match launch {
                EngineLaunch::Wsl { .. } => s("."),
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
        // `opencode run` with no message reads a non-TTY stdin as the message;
        // `--format json` streams its events. (`-p` here is `--password`.)
        "opencode" => {
            let launch = resolve_engine("opencode", resolver, cwd).await?;
            let mut args = vec![s("run"), s("--format"), s("json")];
            if let Some(id) = resume_id {
                args.extend([s("--session"), s(id)]);
            }
            if let Some(m) = model {
                args.extend([s("--model"), s(m)]);
            }
            Ok(engine_command(&launch, &args, cwd, true))
        }
        // `pi --mode json` merges piped stdin into the initial prompt and
        // streams its session events as JSON lines.
        "pi" => {
            let launch = resolve_engine("pi", resolver, cwd).await?;
            let mut args = vec![s("--mode"), s("json")];
            if let Some(id) = resume_id {
                args.extend([s("--session"), s(id)]);
            }
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

pub(crate) type EngineChild = (
    Child,
    tokio::process::ChildStdin,
    tokio::process::ChildStdout,
    Option<tokio::process::ChildStderr>,
);

/// Spawns the engine child through the session executor and returns the
/// (child, stdin, stdout, stderr).
pub(crate) fn spawn_engine_child(spec: SpawnSpec) -> Result<EngineChild, String> {
    let mut child = crate::executor::current()
        .spawn_piped(spec, ENGINE_PIPED_OPTS)
        .map_err(|e| format!("spawn engine: {e}"))?;
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

/// Spawn the engine for a run whose cache row already exists. If building or
/// spawning fails (engine not installed, bad cwd, …) the row is closed out as
/// `failed` with the error, so `chi_status` and the Sessions list show why
/// instead of the run sitting `queued` / `running` forever.
pub(crate) async fn spawn_engine_or_fail(
    db: &PaDb,
    run_id: &str,
    cmd: Result<SpawnSpec, String>,
) -> Result<EngineChild, String> {
    match cmd.and_then(spawn_engine_child) {
        Ok(child) => Ok(child),
        Err(e) => {
            tracing::warn!(target: "ikenga::chi", "chi run {run_id} failed to start: {e}");
            if let Err(db_err) =
                cache_update_done(db, run_id, "failed", Some(&e), false, None).await
            {
                tracing::warn!(target: "ikenga::chi", "chi run {run_id}: could not record spawn failure: {db_err}");
            }
            Err(e)
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Reader tasks (one per CLI engine)
// ═══════════════════════════════════════════════════════════════════════

/// Log an engine's stderr at debug (it can carry prompts and paths, so never
/// louder), keeping the last few lines in memory so a failed run can name its
/// cause — see [`explain_failure`].
fn log_stderr(engine: &'static str, stderr: Option<tokio::process::ChildStderr>) -> StderrTail {
    const KEEP: usize = 40;
    StderrTail(stderr.map(|stderr| {
        tokio::spawn(async move {
            let mut tail = std::collections::VecDeque::with_capacity(KEEP);
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "ikenga::chi", "{engine} stderr: {line}");
                if tail.len() == KEEP {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Vec::from(tail)
        })
    }))
}

/// The tail of an engine's stderr, collected by [`log_stderr`].
struct StderrTail(Option<tokio::task::JoinHandle<Vec<String>>>);

impl StderrTail {
    /// The infrastructure cause named in the tail, if any. Waits briefly for
    /// stderr to reach EOF — the child has usually exited by now.
    async fn cause(self) -> Option<String> {
        let lines = tokio::time::timeout(std::time::Duration::from_millis(500), self.0?)
            .await
            .ok()?
            .ok()?;
        super::failure_class::classify(&lines.join("
")).map(|c| c.describe())
    }
}

/// `error` with the cause from the engine's stderr appended, when there is
/// one: "engine child exited without a done envelope" alone hides a WSL with
/// no network or a missing distro. Only the classified cause is surfaced,
/// never the raw stderr.
async fn explain_failure(error: Option<&str>, tail: StderrTail) -> Option<String> {
    let base = error?;
    Some(match tail.cause().await {
        Some(cause) => format!("{base} — {cause}"),
        None => base.to_string(),
    })
}

/// Register the run's cancel handle and start its reader task. The task
/// holds an `access::KeepAlive` until the run ends, so a daemon's idle
/// watcher (a T1 principal child is started with `--idle-timeout`) does not
/// shut the process — and the run with it — down mid-run.
async fn start_reader(
    env: &ChiEnv,
    engine_id: String,
    run_id: &str,
    output_path: PathBuf,
    engine: EngineChild,
    prompt: String,
) {
    let (child, stdin, stdout, stderr) = engine;
    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(ChiRunHandle {
        child: Some(child.clone()),
        cancelled: cancelled.clone(),
        in_process: None,
    });
    env.runtime.insert(run_id, handle.clone()).await;

    let db = env.db.clone();
    let runtime = env.runtime.clone();
    let run_id = run_id.to_string();
    let payload = stdin_payload(&engine_id, &prompt);
    drop(prompt);
    let keepalive = crate::access::KeepAlive::hold();
    tokio::spawn(async move {
        let _keepalive = keepalive;
        let task_run_id = run_id.clone();
        // Every engine reads its prompt here, never from argv (I-7).
        if let Err(e) = write_prompt(stdin, &payload).await {
            // Terminal failure: close the row out through `cache_update_done`
            // so it gets `ended_at` and the WP-40 `run_failed` notification.
            cache_update_done(
                &db,
                &run_id,
                "failed",
                Some(&format!("stdin write: {e}")),
                false,
                None,
            )
            .await
            .ok();
            runtime.release(&run_id, &handle).await;
            return;
        }
        drop(payload);
        match engine_id.as_str() {
            "antigravity-cli" => {
                antigravity_one_off_task(
                    db,
                    task_run_id,
                    output_path,
                    child,
                    cancelled,
                    stdout,
                    stderr,
                )
                .await
            }
            "codex" => {
                codex_one_off_task(
                    db,
                    task_run_id,
                    output_path,
                    child,
                    cancelled,
                    stdout,
                    stderr,
                )
                .await
            }
            "opencode" => {
                json_lines_task(
                    db,
                    task_run_id,
                    output_path,
                    child,
                    cancelled,
                    stdout,
                    stderr,
                    "opencode",
                    parse_opencode_line,
                )
                .await
            }
            "pi" => {
                json_lines_task(
                    db,
                    task_run_id,
                    output_path,
                    child,
                    cancelled,
                    stdout,
                    stderr,
                    "pi",
                    parse_pi_line,
                )
                .await
            }
            _ => {
                claude_one_off_task(
                    db,
                    task_run_id,
                    output_path,
                    child,
                    cancelled,
                    stdout,
                    stderr,
                )
                .await
            }
        }
        // The run is over: drop its handle (and with it the `Child`, which
        // tokio then reaps) unless a resume has already replaced it.
        runtime.release(&run_id, &handle).await;
    });
}

/// Write the prompt payload and close stdin for real. `shutdown()` on a
/// child pipe does not close the handle, so it is dropped: until the write
/// end closes, a stream-json engine waits for more input and a read-to-EOF
/// engine (codex `-`, opencode, pi) never starts. The daemon's chat-socket
/// antigravity engine feeds its turns through this too (I-7).
pub(crate) async fn write_prompt(
    mut stdin: tokio::process::ChildStdin,
    payload: &str,
) -> std::io::Result<()> {
    stdin.write_all(payload.as_bytes()).await?;
    let _ = stdin.flush().await;
    let _ = stdin.shutdown().await;
    drop(stdin);
    Ok(())
}

/// Whether a finished run's output is flagged `output_truncated`.
fn is_truncated(output: &str) -> bool {
    output.len() > 100_000
}

/// Background task for a Claude Code one-off (its prompt envelope already
/// written to stdin by [`start_reader`]). Reads `stdout`, writes partial
/// output to `output_path`, and updates `chi_cache` as the run progresses.
async fn claude_one_off_task(
    db: Arc<PaDb>,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
) {
    let stderr_tail = log_stderr("claude", stderr);

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
                tracing::debug!(target: "ikenga::chi", "claude reader closed: {e}");
                break;
            }
        }
    }

    // Determine final status.
    let (status, error) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None)
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            ("failed", Some("claude reported stop_reason error"))
        } else {
            ("done", None)
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
        )
    };

    let output_truncated = is_truncated(&output);

    // Write final output file.
    let error = explain_failure(error, stderr_tail).await;
    let file_error = if let Err(e) = write_output_file(&output_path, &output, error.as_deref()).await {
        Some(format!("write output file: {e}"))
    } else {
        error
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
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
) {
    let stderr_tail = log_stderr("antigravity", stderr);

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

    let (status, error) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None)
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            ("failed", Some("antigravity reported stop_reason error"))
        } else {
            ("done", None)
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
        )
    };

    let output_truncated = is_truncated(&output);
    let error = explain_failure(error, stderr_tail).await;
    let file_error = if let Err(e) = write_output_file(&output_path, &output, error.as_deref()).await {
        Some(format!("write output file: {e}"))
    } else {
        error
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
/// Reads the JSONL event stream from stdout via `codex_pty::parser`, extracts
/// `agent_message` text chunks and the `thread.started` thread id (stored as
/// `external_id` so `chi_resume` can pass it back as `--resume <id>`). The
/// prompt was already written to stdin (the `-` positional) and closed.
async fn codex_one_off_task(
    db: Arc<PaDb>,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
) {
    let stderr_tail = log_stderr("codex", stderr);

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
                tracing::debug!(target: "ikenga::chi", "codex parse error: {e}");
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

    let (status, error) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None)
    } else if saw_done && !failed {
        ("done", None)
    } else if failed {
        ("failed", Some("codex reported turn.failed"))
    } else {
        ("failed", Some("codex child exited without turn.completed"))
    };

    let output_truncated = is_truncated(&output);
    let error = explain_failure(error, stderr_tail).await;
    let file_error = if let Err(e) = write_output_file(&output_path, &output, error.as_deref()).await {
        Some(format!("write output file: {e}"))
    } else {
        error
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

/// What one JSON line of an engine's event stream contributes to the run.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LineEvent {
    /// The engine-native session id (stored as `external_id` for resume).
    pub session_id: Option<String>,
    /// Assistant text to append to the output.
    pub text: Option<String>,
    /// The turn finished.
    pub done: bool,
    /// The engine reported a failure.
    pub error: Option<String>,
}

fn str_at<'a>(v: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |v, k| v.get(*k))
        .and_then(|v| v.as_str())
}

/// `opencode run --format json`: every event carries `sessionID`; a `text`
/// event carries a finished text part, `step_finish` ends a step (the turn,
/// unless its reason is `tool-calls`), `error` a failure.
pub(crate) fn parse_opencode_line(v: &serde_json::Value) -> LineEvent {
    let mut ev = LineEvent {
        session_id: str_at(v, &["sessionID"])
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        ..LineEvent::default()
    };
    match str_at(v, &["type"]) {
        Some("text") => ev.text = str_at(v, &["part", "text"]).map(str::to_string),
        Some("step_finish") => ev.done = str_at(v, &["part", "reason"]) != Some("tool-calls"),
        Some("error") => {
            ev.error = Some(
                str_at(v, &["error", "data", "message"])
                    .or_else(|| str_at(v, &["error", "name"]))
                    .unwrap_or("opencode reported an error")
                    .to_string(),
            )
        }
        _ => {}
    }
    ev
}

/// `pi --mode json`: a `session` header (its `id`), `message_update` events
/// whose `assistantMessageEvent` is a `text_delta`, and `agent_end` — whose
/// last assistant message has `stopReason` `error` / `aborted` on failure.
pub(crate) fn parse_pi_line(v: &serde_json::Value) -> LineEvent {
    let mut ev = LineEvent::default();
    match str_at(v, &["type"]) {
        Some("session") => {
            ev.session_id = str_at(v, &["id"])
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }
        Some("message_update") => {
            if str_at(v, &["assistantMessageEvent", "type"]) == Some("text_delta") {
                ev.text = str_at(v, &["assistantMessageEvent", "delta"]).map(str::to_string);
            }
        }
        Some("agent_end") => {
            ev.done = true;
            let last = v.get("messages").and_then(|m| m.as_array()).and_then(|m| {
                m.iter()
                    .rev()
                    .find(|m| str_at(m, &["role"]) == Some("assistant"))
            });
            if let Some(last) = last {
                if let Some(reason @ ("error" | "aborted")) = str_at(last, &["stopReason"]) {
                    ev.error = Some(
                        str_at(last, &["errorMessage"])
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("request {reason}")),
                    );
                }
            }
        }
        _ => {}
    }
    ev
}

/// Background task for an engine that streams JSON lines (opencode, pi),
/// each line folded through `parse`. Its prompt was already written to stdin.
#[allow(clippy::too_many_arguments)]
async fn json_lines_task(
    db: Arc<PaDb>,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    engine: &'static str,
    parse: fn(&serde_json::Value) -> LineEvent,
) {
    let stderr_tail = log_stderr(engine, stderr);

    let mut reader = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut engine_error: Option<String> = None;

    while let Ok(Some(line)) = reader.next_line().await {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let ev = parse(&val);
        if let Some(id) = ev.session_id {
            if external_id.is_none() {
                cache_update_external_id(&db, &run_id, &id).await.ok();
                cache_update_status(&db, &run_id, "running", None)
                    .await
                    .ok();
                external_id = Some(id);
            }
        }
        if let Some(text) = ev.text {
            output.push_str(&text);
        }
        saw_done |= ev.done;
        if ev.error.is_some() {
            engine_error = ev.error;
        }
        write_output_file(&output_path, &output, None).await.ok();
    }

    let error = if cancelled.load(Ordering::SeqCst) {
        None
    } else if let Some(e) = engine_error {
        Some(format!("{engine} reported an error: {e}"))
    } else if saw_done {
        None
    } else {
        Some("engine child exited without a done envelope".to_string())
    };
    let status = if cancelled.load(Ordering::SeqCst) {
        "cancelled"
    } else if error.is_some() {
        "failed"
    } else {
        "done"
    };

    let output_truncated = is_truncated(&output);
    let error = explain_failure(error.as_deref(), stderr_tail).await;
    let file_error = match write_output_file(&output_path, &output, error.as_deref()).await {
        Err(e) => Some(format!("write output file: {e}")),
        Ok(()) => error,
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

// ═══════════════════════════════════════════════════════════════════════
// run / resume / cancel
// ═══════════════════════════════════════════════════════════════════════

/// The result of an in-process run that started. `fallback_warning` is set
/// only when a *persistent* run could not go detached (see [`spawn_run`]):
/// it rides in `error` while `status` stays `running`, so it is visible to
/// every caller without changing the result shape or failing the run.
pub(crate) fn in_process_started(run_id: String, fallback_warning: Option<String>) -> ChiRunResult {
    ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: fallback_warning,
    }
}

/// `chi_run`: insert the row, then start the engine — in-process engine,
/// detached chi-runner (`persistent`), or an in-process child — and return
/// the run id immediately. `owner` tags the row (`cli`, `comment`, a seat).
pub(crate) async fn spawn_run(
    env: &ChiEnv,
    engines: &dyn InProcessEngines,
    opts: ChiRunOpts,
    owner: &str,
) -> Result<ChiRunResult, String> {
    guard_identity()?;
    env.ensure_cache_dir()?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let output_path = env.run_output_path(&run_id);

    // Initial one-off TTL is 1 hour; long-lived sessions will refresh this.
    cache_insert(&env.db, &run_id, &opts, &output_path, owner).await?;

    let cwd = env.run_cwd(opts.cwd.as_deref());

    // ── CLI-less (in-process) engine path ────────────────────────────────────
    // `openrouter` has no CLI to spawn — the adapter IS the HTTP client
    // (WP-20). Runs before the detached path because chi-runner can only
    // launch CLI engines.
    if engines.handles(&opts.engine_id) {
        return engines.start(env, run_id, output_path, &opts, cwd).await;
    }

    // ── Persistent (detached chi-runner) path ────────────────────────────────
    // Try this first so we never spawn a redundant in-process child. When it
    // can't run, the in-process fallback is NOT durable, and the caller asked
    // for durability — so the fallback is carried back as a warning in the
    // result's `error` (status stays `running`: every FE caller only treats
    // `error` as fatal with `status: "failed"`) instead of passing silently.
    // chi-runner builds its own engine argv, and only feeds
    // [`RUNNER_STDIN_ENGINES`] their prompt on stdin: any other engine stays
    // in-process, where its prompt goes to stdin too (I-7).
    let mut fallback_warning: Option<String> = None;
    if opts.persistent && !RUNNER_STDIN_ENGINES.contains(&opts.engine_id.as_str()) {
        let warning = not_detachable_warning(&opts.engine_id);
        tracing::warn!(target: "ikenga::chi", "chi run {run_id}: {warning}");
        fallback_warning = Some(warning);
    } else if opts.persistent {
        let model = runner_model(&opts.engine_id, opts.model.as_deref());
        let conf = chi_runner::RunnerConf {
            run_id: &run_id,
            engine_id: &opts.engine_id,
            prompt: &opts.prompt,
            cwd: &cwd,
            model: model.as_deref(),
            mode: opts.mode.as_deref(),
            resume_session_id: opts.resume_session_id.as_deref(),
            output_path: &output_path.to_string_lossy(),
            timeout_seconds: opts.timeout_seconds.map(|s| s as u64),
        };
        match chi_runner::spawn_detached_runner(&conf, &env.cache_dir) {
            Ok(pid) => {
                if let Err(e) = cache_mark_detached(&env.db, &run_id, pid).await {
                    // An unrecorded runner could be neither reconciled nor
                    // cancelled: take it down rather than leave it orphaned.
                    let _ = chi_runner::kill_process_group(pid, chi_runner::CANCEL_GRACE).await;
                    chi_runner::remove_conf(&env.cache_dir, &run_id);
                    cache_update_done(&env.db, &run_id, "failed", Some(&e), false, None)
                        .await
                        .ok();
                    return Err(e);
                }
                tracing::info!(
                    target: "ikenga::chi",
                    "chi run {run_id} started detached (chi-runner pid {pid})"
                );
                // Someone has to fold the runner's end into the row. The
                // desktop installs its sweep at boot (so this is a no-op
                // there); the daemon starts it on its first detached run.
                ensure_detached_sweep(env.db.clone(), env.cache_dir.clone());
                return Ok(ChiRunResult {
                    run_id,
                    status: "running".to_string(),
                    output: None,
                    output_truncated: None,
                    error: None,
                });
            }
            Err(reason) => {
                let warning = chi_runner::persistent_fallback_warning(&reason);
                tracing::warn!(target: "ikenga::chi", "chi run {run_id}: {warning}");
                fallback_warning = Some(warning);
            }
        }
    }

    // ── In-process (non-persistent) path ─────────────────────────────────────
    let cmd = build_engine_command_with(
        &env.reporting_resolver(),
        &opts.engine_id,
        &cwd,
        opts.model.as_deref(),
        opts.mode.as_deref(),
        opts.resume_session_id.as_deref(),
    )
    .await;
    let engine = spawn_engine_or_fail(&env.db, &run_id, cmd).await?;
    cache_update_status(&env.db, &run_id, "running", None).await?;
    start_reader(
        env,
        opts.engine_id.clone(),
        &run_id,
        output_path,
        engine,
        opts.prompt.clone(),
    )
    .await;

    Ok(in_process_started(run_id, fallback_warning))
}

/// `chi_resume`: continue an existing run (same `run_id`) with a new prompt
/// against its engine-native session id.
pub(crate) async fn resume_run(
    env: &ChiEnv,
    engines: &dyn InProcessEngines,
    run_id: String,
    prompt: String,
) -> Result<ChiRunResult, String> {
    guard_identity()?;
    let mut row = cache_get(&env.db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    // The output file this turn will write. On the daemon a row's path is
    // confined to the cache dir before anything reads or writes it: `db_exec`
    // can plant a row whose `output_path` aims the reader elsewhere.
    let output_path = resolve_output_path(&env.cache_dir, row.output_path.as_deref());
    if let (OutputFiles::InCacheDir, Some(path)) = (env.files, output_path.as_deref()) {
        confine_for_write(&env.cache_dir, path)?;
    }

    // ── A detached (chi-runner) run ──────────────────────────────────────────
    // The resume runs in-process, so the old runner has to be finished first:
    // refuse while it still runs (two writers on one output file), otherwise
    // settle its terminal status now — the sweep may not have seen it yet —
    // pick up the engine session id it wrote, and drop the pid so the sweep
    // doesn't judge the new in-process turn by the old runner's exit.
    if let Some(pid) = row.pid {
        let seen = observe_detached(pid, output_path.as_deref(), &chi_runner::probe_runner).await;
        if seen.liveness == RunLiveness::Running {
            return Err(format!(
                "chi run {run_id} is still running (detached chi-runner pid {pid})"
            ));
        }
        let external_id = seen.external_id.clone();
        apply_detached(
            &env.db,
            &env.cache_dir,
            &run_id,
            row.external_id.as_deref(),
            seen,
        )
        .await?;
        if row.external_id.is_none() {
            row.external_id = external_id;
        }
        cache_clear_pid(&env.db, &run_id).await?;
    }

    let output_path = output_path.unwrap_or_default();

    // ── CLI-less (in-process) engine path ────────────────────────────────────
    if engines.handles(&row.engine_id) {
        return engines.resume(env, row, output_path, prompt).await;
    }

    let resume_id = row
        .external_id
        .clone()
        .ok_or_else(|| format!("chi run {run_id} has no engine session id to resume against"))?;

    cache_update_status(&env.db, &run_id, "running", None).await?;

    let cwd = env.run_cwd(row.cwd.as_deref());
    let cmd = build_engine_command_with(
        &env.reporting_resolver(),
        &row.engine_id,
        &cwd,
        row.model.as_deref(),
        row.mode.as_deref(),
        Some(&resume_id),
    )
    .await;
    let engine = spawn_engine_or_fail(&env.db, &run_id, cmd).await?;
    start_reader(
        env,
        row.engine_id.clone(),
        &run_id,
        output_path,
        engine,
        prompt,
    )
    .await;

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

/// `chi_cancel`: interrupt a live in-process run, kill a detached runner's
/// process group, drop its runner conf (which carries the prompt) from
/// `cache_dir`, and mark the row `cancelled`.
pub(crate) async fn cancel_run(
    db: &PaDb,
    runtime: &ChiRuntime,
    cache_dir: &Path,
    run_id: &str,
) -> Result<ChiRunResult, String> {
    guard_identity()?;
    let row = cache_get(db, run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    if let Some(handle) = runtime.remove(run_id).await {
        handle.cancelled.store(true, Ordering::SeqCst);
        // CLI-less engines: interrupt through the adapter itself.
        if let Some(interrupt) = &handle.in_process {
            interrupt.interrupt().await?;
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
    // *our* runner, running as our own uid: a dead, reused or foreign pid is
    // never signalled. The sweep is held off this run meanwhile so it can't
    // record the kill as a `failed`.
    let _cancelling = CancellingGuard::hold(run_id);
    if let Some(pid) = row.pid.and_then(|p| u32::try_from(p).ok()) {
        match chi_runner::probe_runner(pid) {
            chi_runner::PidProbe::Ours if chi_runner::pid_owned_by_us(pid) => {
                chi_runner::kill_process_group(pid, chi_runner::CANCEL_GRACE)
                    .await
                    .map_err(|e| format!("kill chi-runner: {e}"))?;
            }
            chi_runner::PidProbe::Ours => tracing::warn!(
                target: "ikenga::chi",
                "chi run {run_id}: not signalling pid {pid}: it runs as another user"
            ),
            other => tracing::info!(
                target: "ikenga::chi",
                "chi run {run_id}: not signalling pid {pid} ({other:?})"
            ),
        }
    }
    // The runner is gone (or was never ours to signal): its conf has no
    // reader left.
    if row.pid.is_some() {
        chi_runner::remove_conf(cache_dir, &row.run_id);
    }

    cache_update_status(db, run_id, "cancelled", None).await?;

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
pub(crate) const DETACHED_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

static SWEEP_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Claim the process's one detached-run sweep. `true` exactly once.
pub(crate) fn claim_detached_sweep() -> bool {
    !SWEEP_INSTALLED.swap(true, Ordering::SeqCst)
}

/// The sweep, forever: once now, then every [`DETACHED_SWEEP_EVERY`].
pub(crate) async fn detached_sweep_loop(db: Arc<PaDb>, cache_dir: PathBuf) {
    loop {
        if let Err(e) =
            reconcile_detached_runs_with(&db, &cache_dir, &chi_runner::probe_runner).await
        {
            tracing::warn!(target: "ikenga::chi", "detached run sweep: {e}");
        }
        tokio::time::sleep(DETACHED_SWEEP_EVERY).await;
    }
}

/// Start the sweep on the current Tokio runtime unless one already runs in
/// this process. The daemon calls it at boot when `chi-cache` exists (runs
/// that ended while it was down) and on its first detached run.
pub(crate) fn ensure_detached_sweep(db: Arc<PaDb>, cache_dir: PathBuf) {
    if claim_detached_sweep() {
        tokio::spawn(detached_sweep_loop(db, cache_dir));
    }
}

/// Runs `chi_cancel` is killing right now. The sweep skips them: a runner
/// seen dead mid-cancel is a cancel, not a failure.
static CANCELLING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub(crate) struct CancellingGuard(String);

impl CancellingGuard {
    pub(crate) fn hold(run_id: &str) -> Self {
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
/// `cache_update_done_if_live` (which produces the WP-40 notification),
/// dropping its runner conf from `cache_dir`. Returns whether this call
/// finished the run.
async fn apply_detached(
    db: &PaDb,
    cache_dir: &Path,
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
            // The runner has ended: nothing will read its conf again.
            chi_runner::remove_conf(cache_dir, run_id);
            let finished = cache_update_done_if_live(db, run_id, status, error.as_deref()).await?;
            if finished {
                tracing::info!(
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
pub(crate) async fn reconcile_detached_runs_with(
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
        match apply_detached(db, cache_dir, &run_id, external_id.as_deref(), seen).await {
            Ok(true) => finished += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(target: "ikenga::chi", "chi run {run_id}: reconcile: {e}"),
        }
    }
    Ok(finished)
}

#[cfg(test)]
pub(crate) mod tests;
