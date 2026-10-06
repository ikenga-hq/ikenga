//! Chi-first agent surface — the desktop half.
//!
//! WP-01: a thin cache-backed command surface for running, resuming, listing,
//! and cancelling agent sessions.
//! WP-02: wires the Claude Code engine so `iyke chi run` / `resume` / `cancel`
//! and `list` / `status` actually spawn, monitor, and read the agent child.
//! WP-07: multi-engine parity — Codex (`codex exec --json`) wired;
//!         cursor-agent returns `RUNTIME_NOT_IMPLEMENTED` cleanly.
//!
//! WP-P10: the tauri-free core (cache writers, engine commands, the reader
//! tasks, run / resume / cancel and the detached-run sweep) moved to the
//! ungated `server::shared::chi_exec`, which the headless daemon's
//! `chi_run` / `chi_resume` / `chi_cancel` arms call too. What stays here is
//! the `#[tauri::command]` wrappers, the managed-state glue, and the CLI-less
//! `openrouter` engine — the one thing that needs an `AppHandle` (managed
//! `EngineRegistry` + vault) — plugged in through `chi_exec::InProcessEngines`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_client_protocol::schema::{ContentBlock, PromptResponse, SessionUpdate, StopReason};
use futures_util::future::BoxFuture;
use tauri::{AppHandle, Manager, State};

use crate::commands::db::PaDb;
use crate::engines::{EngineHandle, EngineRegistryState, OpenRouterHttpEngineState};
use crate::server::shared::chi_exec::{
    self, cache_update_done, cache_update_external_id, cache_update_status, now_iso,
    write_output_file, ChiEnv, ChiRunHandle, InProcessEngines, RunInterrupt,
};
use crate::server::shared::chi_runner;

use crate::server::shared::chi::{self as chi_read, RunOutputFile};
/// The run / row shapes and the read path (`cache_get`, the output file,
/// `chi_status`'s liveness read) live in the ungated `server::shared::chi`
/// (WP-19 slice 3), the write side in `server::shared::chi_exec` (WP-P10).
/// Re-exported: `iyke::handlers`, the seat store and `comment_route` name
/// them through here.
pub use crate::server::shared::chi::{ChiCacheRow, ChiRunResult};
pub use crate::server::shared::chi_exec::{ChiRunOpts, ChiRuntime};

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
    #[cfg(test)]
    pub fn run_output_path(&self, run_id: &str) -> PathBuf {
        self.cache_dir().join(format!("{run_id}.json"))
    }

    /// The shared core's view of the desktop's state: the process cwd as the
    /// default, output paths read as stored, the host PATH (+ WSL).
    fn env(&self, db: Arc<PaDb>, runtime: &Arc<ChiRuntime>) -> ChiEnv {
        ChiEnv::new(db, self.cache_dir(), runtime.clone())
    }
}

// ═══════════════════════════════════════════════════════════════════════
// The CLI-less engine (openrouter): the desktop's `InProcessEngines`
// ═══════════════════════════════════════════════════════════════════════

/// The desktop's in-process engines. `app` is `None` only from tests;
/// openrouter needs it for the managed `EngineRegistry` + vault and fails
/// the run cleanly without it.
struct DesktopEngines<'a> {
    app: Option<&'a AppHandle>,
}

impl InProcessEngines for DesktopEngines<'_> {
    fn handles(&self, engine_id: &str) -> bool {
        engine_id == "openrouter"
    }

    fn start<'a>(
        &'a self,
        env: &'a ChiEnv,
        run_id: String,
        output_path: PathBuf,
        opts: &'a ChiRunOpts,
        cwd: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>> {
        Box::pin(async move {
            let Some(app) = self.app else {
                let msg = "openrouter chi runs need the desktop app handle (engine registry + \
                           vault); refusing the run"
                    .to_string();
                tracing::warn!(target: "ikenga::chi", "chi run {run_id}: {msg}");
                cache_update_done(&env.db, &run_id, "failed", Some(&msg), false, None)
                    .await
                    .ok();
                return Err(msg);
            };
            openrouter_spawn_run(
                app,
                env.db.clone(),
                &env.runtime,
                run_id,
                output_path,
                opts,
                cwd,
                None,
            )
            .await
        })
    }

    fn resume<'a>(
        &'a self,
        env: &'a ChiEnv,
        row: ChiCacheRow,
        output_path: PathBuf,
        prompt: String,
    ) -> BoxFuture<'a, Result<ChiRunResult, String>> {
        Box::pin(async move {
            // History is process-local: resume = same thread id while this
            // process lives. Refuse (rather than run an empty-context turn)
            // when the transcript is gone — the same honesty rule the adapter
            // enforces over ACP in `handle_load_session`.
            //
            // One turn at a time per thread: a second resume while a turn is
            // in flight would share the adapter session, lose the first turn's
            // abort slot and write the same output file and cache row twice.
            if matches!(row.status.as_str(), "running" | "queued") {
                return Err(format!("chi run {} is still in progress", row.run_id));
            }
            let Some(app) = self.app else {
                return Err(
                    "openrouter engine is not registered; restart the shell to resume its runs"
                        .to_string(),
                );
            };
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
            engine.handle_load_session(thread_id.clone(), None).await?;
            openrouter_spawn_run(
                app,
                env.db.clone(),
                &env.runtime,
                row.run_id.clone(),
                output_path,
                &chi_exec::row_into_resume_opts(&row, prompt),
                row.cwd.clone().unwrap_or_else(|| ".".to_string()),
                Some(thread_id),
            )
            .await
        })
    }
}

/// `chi_cancel` for an openrouter turn: `handle_cancel` flags the session and
/// triggers the abort signal the streaming read selects on; `run_prompt`
/// then reports `Cancelled`.
struct OpenRouterInterrupt {
    engine: OpenRouterHttpEngineState,
    thread_id: String,
}

impl RunInterrupt for OpenRouterInterrupt {
    fn interrupt(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            self.engine
                .handle_cancel(self.thread_id.clone())
                .await
                .map_err(|e| format!("cancel openrouter turn: {e}"))
        })
    }
}

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
                in_process: Some(Arc::new(OpenRouterInterrupt {
                    engine: engine.clone(),
                    thread_id: thread_id.clone(),
                })),
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
/// `output_path` come from `chi_exec::spawn_run`'s cache bookkeeping; `thread_id`
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
    chi_exec::spawn_run(
        &cache.env(db, runtime),
        &DesktopEngines { app },
        opts,
        owner,
    )
    .await
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
/// (G-SEATS §4.1, §4.5). The run keeps its `run_id`.
pub(crate) async fn resume_chi_run(
    app: &AppHandle,
    db: Arc<PaDb>,
    cache: &ChiCache,
    runtime: &Arc<ChiRuntime>,
    run_id: String,
    prompt: String,
) -> Result<ChiRunResult, String> {
    chi_exec::resume_run(
        &cache.env(db, runtime),
        &DesktopEngines { app: Some(app) },
        run_id,
        prompt,
    )
    .await
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
    // Cache rows + Claude's JSONL sessions from `~/.claude/projects`, sorted
    // and truncated — the shared core the daemon's `chi_list` arm also calls.
    chi_read::list_merged(
        &db,
        engineId.as_deref(),
        limit,
        crate::claude::projects_root().as_deref(),
        crate::server::shared::projects::FsReach::Follow,
    )
    .await
}

/// Cancel a Chi run. Kills the engine child process (or the detached
/// chi-runner's process group) — the shared core the daemon arm calls.
#[tauri::command]
pub async fn chi_cancel(
    db: State<'_, Arc<PaDb>>,
    runtime: State<'_, Arc<ChiRuntime>>,
    #[allow(non_snake_case)] runId: String,
) -> Result<ChiRunResult, String> {
    chi_exec::cancel_run(&db, &runtime, &runId).await
}

// ═══════════════════════════════════════════════════════════════════════
// Detached-run reconciliation (WP-18b, G-88) — the desktop's sweep
// ═══════════════════════════════════════════════════════════════════════

/// [`chi_exec::reconcile_detached_runs_with`] against the app's db, cache dir
/// and the real pid probe.
pub(crate) async fn reconcile_detached_runs(app: &AppHandle) -> Result<usize, String> {
    let db = app
        .try_state::<Arc<PaDb>>()
        .map(|s| s.inner().clone())
        .ok_or("PaDb is not managed")?;
    let cache_dir = app
        .try_state::<ChiCache>()
        .map(|s| s.cache_dir())
        .ok_or("ChiCache is not managed")?;
    chi_exec::reconcile_detached_runs_with(&db, &cache_dir, &chi_runner::probe_runner).await
}

/// Start the detached-run sweep: once now (boot — runs that finished or died
/// while the app was down), then every `chi_exec::DETACHED_SWEEP_EVERY`.
/// Idempotent, and shares the process's one sweep claim with the core (so a
/// detached run started before this ran does not start a second sweep).
/// Called from `iyke::start` next to the seat store's own boot hook.
pub(crate) fn install_detached_reconciler(app: &AppHandle) {
    if !chi_exec::claim_detached_sweep() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(e) = reconcile_detached_runs(&app).await {
                tracing::warn!(target: "ikenga::chi", "detached run sweep: {e}");
            }
            tokio::time::sleep(chi_exec::DETACHED_SWEEP_EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    //! The desktop half's own tests: the openrouter engine and the wrapper
    //! that refuses it without an app handle. The shared core's tests live in
    //! `server::shared::chi_exec::tests`.
    use super::*;
    use crate::server::shared::chi::cache_list;

    async fn test_db() -> PaDb {
        let file_name = format!("ikenga-chi-test-{}.db", uuid::Uuid::new_v4());
        let db_path = std::env::temp_dir().join(file_name);
        PaDb::new(db_path)
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

    /// The desktop wrapper still drives the shared core end to end: a CLI
    /// engine that cannot start closes its row as `failed`.
    #[tokio::test]
    async fn spawn_chi_run_records_failed_status_when_the_engine_cannot_start() {
        let db = Arc::new(test_db().await);
        let cache =
            ChiCache::new(std::env::temp_dir().join(format!("chi-test-{}", uuid::Uuid::new_v4())));
        let runtime = Arc::new(ChiRuntime::new());
        let opts = ChiRunOpts {
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
        assert_eq!(rows[0].status, "failed");
        assert!(
            cache.cache_dir().is_dir(),
            "the wrapper's cache dir is used"
        );
        assert_eq!(
            rows[0].output_path.as_deref(),
            Some(
                cache
                    .run_output_path(&rows[0].run_id)
                    .to_string_lossy()
                    .as_ref()
            )
        );
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
}
