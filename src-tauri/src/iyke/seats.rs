//! Chi seats — the desktop glue over the seat store (G-SEATS §9.2, §10;
//! WP-65).
//!
//! The store itself — derivation, holds, the atomic bind, every command
//! core — is AppHandle-free and lives in `server::shared::seats`, shared with
//! the daemon's `/api/rpc` arms (`server/rpc_seats.rs`); it is re-exported
//! here, so every `iyke::seats::*` path stands. What stays here is what needs
//! the app: the live world (`PtyManager`, pane state, the `hooks://event`
//! listener, the openrouter adapter), engine calls through `commands::chi`,
//! the §4.5 queue poller, `seats://changed` emission, and the 13
//! `#[tauri::command]`s.

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::json;
use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, Listener, Manager, State};

use crate::commands::chi::{
    openrouter_holds_thread, resume_chi_run, spawn_chi_run, ChiCache, ChiRunOpts, ChiRuntime,
};
use crate::commands::db::PaDb;
use crate::iyke::hooks::HookPayload;
use crate::iyke::memory::scratchpad_changed;
use crate::iyke::state::IykeState;
use crate::iyke::terminal::enrich_terminals;
use crate::pty::PtyManager;
use crate::window::registry::WindowRegistry;

pub(crate) use crate::server::shared::seats::*;

/// Re-emitted Claude hook events (`iyke/hooks.rs`); the liveness source (§2.5).
const HOOKS_EVENT: &str = "hooks://event";
/// Same event `memory.rs` emits on a scratchpad write; a rename moves pads.
const SCRATCHPAD_CHANGED_EVENT: &str = "iyke://scratchpad-changed";

/// After commit: wake the in-process scratchpad watchers for every pad a
/// rename or remove moved, then one `seats://changed` per affected seat, then
/// the scratchpad events (§10).
fn emit_effects(app: &AppHandle, effects: Effects) {
    for pad in &effects.pads {
        scratchpad_changed(&pad.scope, &pad.name, pad.version, pad.deleted);
    }
    for event in effects.events {
        let _ = app.emit(SEATS_CHANGED_EVENT, &event);
    }
    for pad in effects.pads {
        let _ = app.emit(
            SCRATCHPAD_CHANGED_EVENT,
            json!({
                "scope": pad.scope,
                "name": pad.name,
                "action": if pad.deleted { "delete" } else { "write" },
                "updated_at": pad.version,
            }),
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tauri glue: the live world, engine calls, listener, poller
// ═══════════════════════════════════════════════════════════════════════

async fn pool_of(db: &PaDb) -> Result<SqlitePool, SeatError> {
    db.ensure_pool().await.map_err(SeatError::internal)
}

/// Re-derive a seat's view after a bind, with a world probed for the seat's
/// *new* session (a create or move builds its world before the destination
/// holds the session, so e.g. a bound openrouter run's thread was never
/// probed and a `done` run would read vacant instead of idle, §2.2).
async fn refreshed_view(
    app: &AppHandle,
    pool: &SqlitePool,
    seat_id: &str,
) -> Result<SeatView, SeatError> {
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(app, pool, std::slice::from_ref(&row), false).await;
    view_of(pool, &world, row).await
}

async fn app_pool(app: &AppHandle) -> Result<SqlitePool, SeatError> {
    let db = app
        .try_state::<Arc<PaDb>>()
        .ok_or_else(|| SeatError::internal("the database is not ready"))?;
    pool_of(&db).await
}

/// WP-70 bridge: a PTY write through the seat went out (§7.2 pty route).
/// Bumps `last_active_at` (§1.1) and emits `updated` (§10).
pub(crate) async fn note_pty_send(app: &AppHandle, seat_id: &str) -> Result<(), SeatError> {
    let pool = app_pool(app).await?;
    touch_last_active(&pool, seat_id, now_ms()).await?;
    if let Some(row) = fetch_seat(&pool, seat_id).await? {
        let _ = app.emit(
            SEATS_CHANGED_EVENT,
            &changed(&row, vec!["updated"], None),
        );
    }
    Ok(())
}

/// WP-70 bridge glue for `send_idle_core`: emits its §10 events.
pub(crate) async fn send_idle<F, Fut>(
    app: &AppHandle,
    seat_id: &str,
    run_id: &str,
    actor: &SeatActor,
    send: F,
) -> Result<IdleSend, SeatError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, SeatError>>,
{
    let pool = app_pool(app).await?;
    let (outcome, effects) = send_idle_core(&pool, seat_id, run_id, actor, send).await?;
    emit_effects(app, effects);
    Ok(outcome)
}

/// Build the world derivation reads, for `rows` (openrouter threads and
/// engine install state are probed only for these seats, or for every engine
/// with `all_engines`).
async fn tauri_world(
    app: &AppHandle,
    pool: &SqlitePool,
    rows: &[SeatRow],
    all_engines: bool,
) -> WorldSnapshot {
    let mut world = WorldSnapshot {
        agents: store().agents_snapshot(),
        ..WorldSnapshot::default()
    };

    // Terminals, with `mount` from `enrich_terminals` (§1.6).
    let descriptors = app
        .try_state::<Arc<PtyManager>>()
        .map(|m| m.list_terminals());
    if let Some(mut descriptors) = descriptors {
        let iyke_state: Option<Arc<IykeState>> =
            app.try_state::<Arc<IykeState>>().map(|s| s.inner().clone());
        let panes = match iyke_state {
            Some(s) => s.snapshot().await.panes,
            None => None,
        };
        let windows = app
            .try_state::<WindowRegistry>()
            .map(|r| r.list_live(app))
            .unwrap_or_default();
        enrich_terminals(&mut descriptors, panes.as_ref(), &windows);
        let now = now_ms().max(0) as u64;
        world.terminals = descriptors.into_iter().map(|d| term_live(d, now)).collect();
    }

    // Engine install state.
    let mut engines: BTreeSet<String> = rows.iter().map(|r| r.engine_id.clone()).collect();
    if all_engines {
        engines.extend(ENGINE_CAPS.iter().map(|c| c.engine_id.to_string()));
    }
    // Probed concurrently: each may wait on a cold WSL start.
    let probes = engines.iter().filter_map(|engine| {
        let binary = engine_cap(engine).and_then(|c| c.binary)?;
        Some(async move { (engine.clone(), binary_available(binary).await) })
    });
    for (engine, available) in futures_util::future::join_all(probes).await {
        if !available {
            world.unavailable_engines.push(engine);
        }
    }

    // The openrouter adapter: registered, and which threads it still holds.
    if engines.contains(OPENROUTER_ENGINE) {
        let mut threads: BTreeSet<String> = BTreeSet::new();
        for row in rows.iter().filter(|r| r.engine_id == OPENROUTER_ENGINE) {
            if let Some(ext) = &row.external_id {
                threads.insert(ext.clone());
            }
            if let (Some("run"), Some(run_id)) =
                (row.session_kind.as_deref(), row.session_ref.as_deref())
            {
                threads.insert(run_id.to_string());
                if let Ok(Some(chi)) = fetch_chi(pool, run_id).await {
                    if let Some(ext) = chi.external_id {
                        threads.insert(ext);
                    }
                }
            }
        }
        world.openrouter_registered = openrouter_holds_thread(app, "").await.is_some();
        if world.openrouter_registered {
            for thread in threads {
                let held = openrouter_holds_thread(app, &thread).await.unwrap_or(false);
                world.openrouter_threads.insert(thread, held);
            }
        }
    }
    world
}

async fn call_engine(app: &AppHandle, call: EngineCall) -> Result<String, String> {
    let db: Arc<PaDb> = app
        .try_state::<Arc<PaDb>>()
        .ok_or("the database is not ready")?
        .inner()
        .clone();
    let cache: ChiCache = app
        .try_state::<ChiCache>()
        .ok_or("the chi cache is not ready")?
        .inner()
        .clone();
    let runtime: Arc<ChiRuntime> = app
        .try_state::<Arc<ChiRuntime>>()
        .ok_or("the chi runtime is not ready")?
        .inner()
        .clone();
    let result = match call {
        EngineCall::ResumeRun { run_id, prompt } => {
            resume_chi_run(app, db, &cache, &runtime, run_id, prompt).await?
        }
        EngineCall::Start {
            engine_id,
            prompt,
            cwd,
            resume_session_id,
            persistent,
        } => {
            let opts = ChiRunOpts {
                engine_id,
                prompt,
                cwd,
                model: None,
                mode: None,
                timeout_seconds: None,
                parent_id: None,
                resume_session_id,
                persistent,
            };
            spawn_chi_run(db, &cache, &runtime, Some(app), opts, SEAT_RUN_OWNER).await?
        }
    };
    Ok(result.run_id)
}

/// Install the §2.5 `hooks://event` listener and the §4.5 queue poller.
/// Idempotent; called once from `iyke::start`.
pub(crate) fn install(app: &AppHandle) {
    if store().installed.swap(true, Ordering::SeqCst) {
        return;
    }
    let app_for_hooks = app.clone();
    let _ = app.listen(HOOKS_EVENT, move |event| {
        match serde_json::from_str::<HookPayload>(event.payload()) {
            Ok(payload) => on_hook_event(&app_for_hooks, payload),
            Err(e) => log::debug!(target: "ikenga::seats", "hooks://event parse: {e}"),
        }
    });
    let app_for_queue = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(QUEUE_POLL).await;
            drain_queue(&app_for_queue).await;
        }
    });
}

/// `(terminal_id, pty_id)` of the running PTY for a hook's terminal id.
fn current_terminal(app: &AppHandle, id: &str) -> Option<(String, String)> {
    let manager = app.try_state::<Arc<PtyManager>>()?;
    manager
        .list_terminals()
        .into_iter()
        .filter(|d| d.status == "running" && (d.terminal_id == id || d.pty_id == id))
        .max_by_key(|d| d.created_at)
        .map(|d| (d.terminal_id, d.pty_id))
}

fn on_hook_event(app: &AppHandle, payload: HookPayload) {
    let Some(raw_id) = payload.ikenga_terminal_id.clone().filter(|t| !t.is_empty()) else {
        return;
    };
    let Some(event) = payload.hook_event_name.as_deref() else {
        return;
    };
    if !matches!(
        event,
        "SessionStart" | "UserPromptSubmit" | "Stop" | "SessionEnd"
    ) {
        return;
    }
    let (terminal_id, pty_id) = match current_terminal(app, &raw_id) {
        Some((t, p)) => (t, Some(p)),
        None => (raw_id, None),
    };
    let session_id = payload.session_id.clone().filter(|s| !s.is_empty());
    {
        let mut agents = guard(&store().agents);
        apply_hook(&mut agents, &terminal_id, event, session_id.clone(), pty_id);
    }
    if event == "SessionStart" {
        if let Some(session_id) = session_id {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let Some(db) = app.try_state::<Arc<PaDb>>().map(|s| s.inner().clone()) else {
                    return;
                };
                let Ok(pool) = db.ensure_pool().await else {
                    return;
                };
                match capture_core(&pool, &terminal_id, &session_id).await {
                    Ok(events) => {
                        for event in events {
                            let _ = app.emit(SEATS_CHANGED_EVENT, &event);
                        }
                    }
                    Err(e) => log::warn!(
                        target: "ikenga::seats",
                        "record resume id for terminal {terminal_id}: {e}"
                    ),
                }
            });
        }
    }
}

/// §4.5: send each queued text once its run leaves `queued` / `running`.
async fn drain_queue(app: &AppHandle) {
    let ids = store().queued_ids();
    if ids.is_empty() {
        return;
    }
    let Some(db) = app.try_state::<Arc<PaDb>>().map(|s| s.inner().clone()) else {
        return;
    };
    let Ok(pool) = db.ensure_pool().await else {
        return;
    };
    for id in ids {
        let engine_app = app.clone();
        let event = drain_one(&pool, &id, move |call| async move {
            call_engine(&engine_app, call).await
        })
        .await;
        if let Some(event) = event {
            let _ = app.emit(SEATS_CHANGED_EVENT, &event);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tauri commands (§9.2) — frozen names and signatures
// ═══════════════════════════════════════════════════════════════════════

/// Views of every seat in `projectId` (default: the active project), ordered
/// by `created_at`. Never writes.
#[tauri::command]
pub async fn seats_list(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    project_id: Option<String>,
) -> Result<Vec<SeatView>, SeatError> {
    let pool = pool_of(&db).await?;
    let project = match project_id.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => active_project(&pool).await?,
    };
    let rows = list_rows(&pool, &project).await?;
    let world = tauri_world(&app, &pool, &rows, false).await;
    list_core(&pool, &world, rows).await
}

/// One view, by id or by any §1.3 address.
#[tauri::command]
pub async fn seats_get(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat: SeatAddress,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = resolve_address(&pool, &seat).await?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    view_of(&pool, &world, row).await
}

/// §6.1 with install state, for the create form.
#[tauri::command]
pub async fn seats_engines(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
) -> Result<Vec<SeatEngineInfo>, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], true).await;
    Ok(engines_info(&world))
}

#[tauri::command]
pub async fn seats_resolve(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat: SeatAddress,
    actor: SeatActor,
    opts: Option<ResolveOpts>,
) -> Result<SeatRoute, SeatError> {
    let pool = pool_of(&db).await?;
    let row = resolve_address(&pool, &seat).await?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let claim_resume = opts.map(|o| o.claim_resume).unwrap_or(false);
    let (route, effects) = resolve_core(&pool, &world, &row.id, &actor, claim_resume).await?;
    emit_effects(&app, effects);
    Ok(route)
}

#[tauri::command]
pub async fn seats_create(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    req: CreateSeatReq,
    actor: SeatActor,
) -> Result<SeatMoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], false).await;
    let (mut result, effects) = create_core(&pool, &world, req, &actor).await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    let seat_id = result.seat.id.clone();
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_move(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    session: SeatSessionRef,
    to_seat_id: String,
    actor: SeatActor,
    opts: Option<MoveOpts>,
) -> Result<SeatMoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], false).await;
    let claim = opts.and_then(|o| o.claim);
    let (mut result, effects) = move_core(
        &pool,
        &world,
        &session,
        &to_seat_id,
        &actor,
        claim.as_deref(),
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    let seat_id = result.seat.id.clone();
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_resume(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
    opts: ResumeOpts,
) -> Result<SeatResumeResult, SeatError> {
    let pool = pool_of(&db).await?;
    // Take the seat's mutex before building the world, so the vacancy check
    // reads liveness from after any resume that raced this one (§4.1).
    let _guard = store().lock_one(&seat_id).await;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let engine_app = app.clone();
    let (mut result, effects) = resume_locked(
        &pool,
        &world,
        &seat_id,
        prompt,
        &actor,
        opts.fallback,
        move |call| async move { call_engine(&engine_app, call).await },
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_fill(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
    opts: Option<FillOpts>,
) -> Result<SeatFillResult, SeatError> {
    let pool = pool_of(&db).await?;
    let _guard = store().lock_one(&seat_id).await;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let persistent = opts.map(|o| o.persistent).unwrap_or(false);
    let engine_app = app.clone();
    let (mut result, effects) = fill_locked(
        &pool,
        &world,
        &seat_id,
        prompt,
        &actor,
        persistent,
        move |call| async move { call_engine(&engine_app, call).await },
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_queue(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = queue_core(&pool, &world, &seat_id, prompt, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_clear(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = clear_core(&pool, &world, &seat_id, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_rename(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    name: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = rename_core(&pool, &world, &seat_id, &name, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_remove(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    opts: RemoveOpts,
    actor: SeatActor,
) -> Result<SeatRemoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let (result, effects) = remove_core(&pool, &seat_id, opts.remove_memory, &actor).await?;
    emit_effects(&app, effects);
    Ok(result)
}

#[tauri::command]
pub async fn seats_release(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = release_core(&pool, &world, &seat_id, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

// ═══════════════════════════════════════════════════════════════════════
// Tests — written under DEC-50 (not run before CI).
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::collections::HashMap;

    // ── fixtures ────────────────────────────────────────────────────────

    /// Comment-stripping `;` splitter for the migration files used here.
    fn split_sql(sql: &str) -> Vec<String> {
        let stripped: Vec<&str> = sql
            .lines()
            .map(|line| match line.find("--") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect();
        stripped
            .join("\n")
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            include_str!("../../migrations/0016_iyke_memory.sql"),
            include_str!("../../migrations/0059_chi_cache.sql"),
            include_str!("../../migrations/0067_iyke_seats.sql"),
            include_str!("../../migrations/0068_chi_cache_runner_pid.sql"),
        ] {
            for stmt in split_sql(sql) {
                sqlx::query(&stmt).execute(&pool).await.unwrap();
            }
        }
        for stmt in [
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY, display_name TEXT NOT NULL, root_path TEXT, icon TEXT,
                color TEXT, description TEXT, position INTEGER NOT NULL DEFAULT 0,
                is_default INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
                archived_at INTEGER)",
            "CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL,
                updated_at INTEGER NOT NULL)",
            "INSERT INTO projects (id, display_name, root_path, is_default, created_at)
             VALUES ('default', 'Default', '/work/default', 1, 0)",
            "INSERT INTO projects (id, display_name, root_path, created_at)
             VALUES ('royalti-co', 'Royalti', '/work/royalti', 0)",
            "INSERT INTO projects (id, display_name, root_path, created_at, archived_at)
             VALUES ('old', 'Old', NULL, 0, 1)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    fn actor(client: &str) -> SeatActor {
        SeatActor {
            client: client.to_string(),
            hold: false,
            takeover: false,
            hold_ttl_ms: None,
        }
    }

    fn ui() -> SeatActor {
        actor("ui")
    }

    fn term(id: &str) -> TermLive {
        TermLive {
            terminal_id: id.to_string(),
            pty_id: format!("pty-{id}"),
            running: true,
            cwd: "/work/default".to_string(),
            lease_holder: None,
            mount: Some(SeatMount {
                window_label: "main".to_string(),
                pane_ids: vec!["pane-1".to_string()],
            }),
        }
    }

    fn world_with(terminals: &[&str]) -> WorldSnapshot {
        WorldSnapshot {
            terminals: terminals.iter().map(|t| term(t)).collect(),
            ..WorldSnapshot::default()
        }
    }

    fn term_ref(id: &str, engine: &str) -> SeatSessionRef {
        SeatSessionRef::Terminal {
            terminal_id: id.to_string(),
            engine_id: engine.to_string(),
            cwd: None,
            external_id: None,
        }
    }

    fn empty_req(name: &str, engine: &str, project: &str) -> CreateSeatReq {
        CreateSeatReq {
            project_id: Some(project.to_string()),
            name: name.to_string(),
            engine_id: engine.to_string(),
            start: SeatStart::Empty,
        }
    }

    async fn create(pool: &SqlitePool, name: &str, engine: &str) -> String {
        create_core(
            pool,
            &WorldSnapshot::default(),
            empty_req(name, engine, "default"),
            &ui(),
        )
        .await
        .unwrap()
        .0
        .seat
        .id
    }

    async fn row(pool: &SqlitePool, id: &str) -> SeatRow {
        fetch_seat(pool, id).await.unwrap().unwrap()
    }

    async fn insert_run(
        pool: &SqlitePool,
        run_id: &str,
        engine: &str,
        status: &str,
        ext: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO chi_cache (run_id, engine_id, external_id, cwd, status, owner)
             VALUES (?, ?, ?, '/work/run', ?, 'test')",
        )
        .bind(run_id.to_string())
        .bind(engine.to_string())
        .bind(ext.map(str::to_string))
        .bind(status.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn set_session(pool: &SqlitePool, seat: &str, kind: &str, sref: &str, ext: Option<&str>) {
        sqlx::query(
            "UPDATE iyke_seats SET session_kind = ?, session_ref = ?, external_id = ?,
                    session_cwd = '/work/docs' WHERE id = ?",
        )
        .bind(kind.to_string())
        .bind(sref.to_string())
        .bind(ext.map(str::to_string))
        .bind(seat.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn exec(pool: &SqlitePool, sql: &str, scope: &str) {
        sqlx::query(sql)
            .bind(scope.to_string())
            .execute(pool)
            .await
            .unwrap();
    }

    /// One row under `scope` in each of the five scope-keyed tables.
    async fn seed_memory(pool: &SqlitePool, scope: &str) {
        exec(
            pool,
            "INSERT INTO iyke_scratchpads (id, scope, name, body, created_at, updated_at)
             VALUES (lower(hex(randomblob(16))), ?, 'notes', 'kept', 1, 5)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_todos (id, scope, title, created_at, updated_at)
             VALUES (lower(hex(randomblob(16))), ?, 'todo', 1, 1)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_kv (scope, key, value, updated_at) VALUES (?, 'k', '1', 1)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_locks (scope, resource, holder, acquired_at, expires_at)
             VALUES (?, 'r', 'h', 1, 2)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_timers (id, scope, fire_at, title, created_at)
             VALUES (lower(hex(randomblob(16))), ?, 9, 'timer', 1)",
            scope,
        )
        .await;
    }

    async fn counts_under(pool: &SqlitePool, scope: &str) -> Vec<i64> {
        let mut out = Vec::new();
        for table in SCOPE_TABLES {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE scope = ?");
            let n: i64 = sqlx::query_scalar(&sql)
                .bind(scope.to_string())
                .fetch_one(pool)
                .await
                .unwrap();
            out.push(n);
        }
        out
    }

    async fn count(pool: &SqlitePool, sql: &str, arg: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(arg.to_string())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn seat_row(
        engine: &str,
        kind: Option<&str>,
        sref: Option<&str>,
        ext: Option<&str>,
    ) -> SeatRow {
        SeatRow {
            id: "s1".into(),
            project_id: "default".into(),
            name: "lead".into(),
            engine_id: engine.into(),
            session_kind: kind.map(str::to_string),
            session_ref: sref.map(str::to_string),
            external_id: ext.map(str::to_string),
            session_cwd: None,
            hold_client: None,
            hold_since: None,
            hold_expires_at: None,
            displaced_client: None,
            displaced_by: None,
            displaced_at: None,
            created_at: 0,
            last_active_at: 0,
        }
    }

    fn chi(engine: &str, status: &str, ext: Option<&str>) -> ChiLite {
        ChiLite {
            engine_id: engine.into(),
            status: status.into(),
            external_id: ext.map(str::to_string),
            cwd: None,
        }
    }

    fn never(_call: EngineCall) -> std::future::Ready<Result<String, String>> {
        panic!("the engine must not be called here")
    }

    // ── grammar (§1.2, §1.3, §3.1) ──────────────────────────────────────

    #[test]
    fn seat_name_grammar() {
        let max = "a".repeat(32);
        let over = "a".repeat(33);
        for ok in ["a", "lead", "nightly-2", "0", "a-b-c", max.as_str()] {
            assert!(validate_seat_name(ok).is_ok(), "should accept {ok:?}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "Lead",
            "le_ad",
            "le.ad",
            "le ad",
            "a/b",
            "ä",
            over.as_str(),
        ] {
            assert!(validate_seat_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn address_forms() {
        let q = |p: &str, n: &str| ParsedAddress::Qualified {
            project: p.into(),
            name: n.into(),
        };
        let b = |n: &str| ParsedAddress::Bare { name: n.into() };
        assert_eq!(
            parse_seat_address("seat:royalti-co/lead").unwrap(),
            q("royalti-co", "lead")
        );
        assert_eq!(
            parse_seat_address("royalti-co/lead").unwrap(),
            q("royalti-co", "lead")
        );
        assert_eq!(parse_seat_address("@lead").unwrap(), b("lead"));
        assert_eq!(parse_seat_address("lead").unwrap(), b("lead"));
        for bad in [
            "seat:lead",
            "seat:a/b/c",
            "@a/b",
            "Lead",
            "a/b/c",
            "",
            "seat:/x",
        ] {
            assert_eq!(
                parse_seat_address(bad).unwrap_err().code,
                "invalid_address",
                "{bad:?}"
            );
        }
        assert_eq!(seat_address("royalti-co", "lead"), "seat:royalti-co/lead");
    }

    /// §3.2: the `seat:` arm's project rule is `projects.rs::validate_slug`.
    /// That function is private, so this drives it through `create_project`,
    /// which calls it first, and requires the shared copy to agree.
    #[tokio::test]
    async fn project_slug_copy_agrees_with_projects_rs() {
        use crate::commands::projects::{create_project, CreateArgs};
        let pool = pool().await;
        let long_ok = "q".repeat(64);
        let long_bad = "r".repeat(65);
        let candidates: Vec<&str> = vec![
            "a",
            "abc123",
            "music-2026",
            "x_y_z",
            "0lead",
            "z-",
            "y_",
            "",
            "-bad",
            "_bad",
            "Bad",
            "with space",
            "with.dot",
            "with!",
            "ä",
            "a/b",
            long_ok.as_str(),
            long_bad.as_str(),
        ];
        for id in candidates {
            let shared = validate_project_slug(id).is_ok();
            let projects = create_project(
                &pool,
                CreateArgs {
                    id: id.to_string(),
                    display_name: "x".to_string(),
                    root_path: None,
                    icon: None,
                    color: None,
                    description: None,
                },
            )
            .await;
            if let Err(e) = &projects {
                assert!(
                    e.starts_with("invalid project id"),
                    "{id:?}: unexpected error {e}"
                );
            }
            assert_eq!(shared, projects.is_ok(), "slug rules disagree on {id:?}");
        }
    }

    // ── engine capability (§6.1) ────────────────────────────────────────

    /// Walks `build_engine_command_with`'s match arms in
    /// `server/shared/chi_exec.rs` (moved from `commands/chi.rs` by WP-P10):
    /// every arm needs a capability row; an arm that refuses to build can't
    /// hold a seat, and every other arm can.
    #[test]
    fn every_chi_engine_arm_has_a_capability_row() {
        let src = include_str!("../server/shared/chi_exec.rs");
        let start = src
            .find("fn build_engine_command_with(")
            .expect("build_engine_command_with in chi_exec.rs");
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("end of build_engine_command_with");
        let body = &body[..end];
        let arm = regex::Regex::new(r#"(?m)^\s*"([a-z0-9][a-z0-9-]*)"\s*=>(.*)$"#).unwrap();
        let mut arms: Vec<String> = Vec::new();
        for c in arm.captures_iter(body) {
            let id = c[1].to_string();
            let cap = engine_cap(&id).unwrap_or_else(|| {
                panic!("chi engine arm {id:?} has no seat capability row (G-SEATS §6.1)")
            });
            if c[2].contains("Err(") {
                assert!(
                    cap.resume.is_none(),
                    "{id} refuses to build but is seatable"
                );
            } else {
                assert!(cap.resume.is_some(), "{id} builds but is not seatable");
                assert!(cap.binary.is_some(), "{id} has no binary for install state");
            }
            arms.push(id);
        }
        assert!(arms.len() >= 6, "arms found: {arms:?}");
        // The fallback arm: anything else can't hold a seat.
        assert!(body.contains("engine not yet supported by iyke chi"));
        assert!(!engine_seatable("gemini"));
        assert!(!engine_seatable("no-such-engine"));
        // openrouter is dispatched in-process, before the arms: the shared
        // core asks its in-process engines first, and the desktop's (here)
        // and the daemon's both claim openrouter.
        assert!(src.contains("if engines.handles(&opts.engine_id)"));
        assert!(include_str!("../commands/chi.rs").contains("engine_id == \"openrouter\""));
        {
            use crate::server::shared::chi_exec::{InProcessEngines, NoInProcessEngines};
            assert!(NoInProcessEngines.handles("openrouter"));
        }
        assert_eq!(
            engine_resume("openrouter"),
            Some(EngineResume::ProcessLocal)
        );
        // Every seatable CLI row maps to an arm.
        for cap in ENGINE_CAPS {
            if cap.resume.is_some() && cap.engine_id != OPENROUTER_ENGINE {
                assert!(
                    arms.iter().any(|a| a == cap.engine_id),
                    "{} has no arm",
                    cap.engine_id
                );
            }
        }
    }

    #[test]
    fn capability_values_and_engines_info() {
        assert_eq!(engine_resume("claude-code"), Some(EngineResume::Durable));
        assert_eq!(engine_resume("codex"), Some(EngineResume::Durable));
        assert_eq!(
            engine_resume("antigravity-cli"),
            Some(EngineResume::Durable)
        );
        assert_eq!(engine_resume("opencode"), Some(EngineResume::None));
        assert_eq!(engine_resume("pi"), Some(EngineResume::None));
        assert_eq!(engine_resume("cursor-agent"), None);
        let world = WorldSnapshot {
            unavailable_engines: vec!["codex".into()],
            ..WorldSnapshot::default()
        };
        let info = engines_info(&world);
        let get = |id: &str| info.iter().find(|i| i.engine_id == id).unwrap().clone();
        assert!(get("claude-code").seatable);
        assert!(!get("codex").seatable);
        assert_eq!(get("codex").reason.as_deref(), Some("not installed"));
        assert!(!get("gemini").seatable);
        assert!(
            !get("openrouter").seatable,
            "the adapter is not registered in this world"
        );
        assert_eq!(
            serde_json::to_value(EngineResume::ProcessLocal).unwrap(),
            json!("process-local")
        );
    }

    // ── storage (§1.4, §1.5) ────────────────────────────────────────────

    #[tokio::test]
    async fn database_refuses_a_double_seated_session() {
        let pool = pool().await;
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        set_session(&pool, &a, "terminal", "t1", Some("conv-1")).await;
        // The same ref in a second seat.
        let same_ref = sqlx::query(
            "UPDATE iyke_seats SET session_kind = 'terminal', session_ref = 't1' WHERE id = ?",
        )
        .bind(b.clone())
        .execute(&pool)
        .await;
        assert!(same_ref.is_err());
        // The same conversation through another ref.
        let same_conv = sqlx::query(
            "UPDATE iyke_seats SET session_kind = 'run', session_ref = 'r9',
                    external_id = 'conv-1'
             WHERE id = ?",
        )
        .bind(b.clone())
        .execute(&pool)
        .await;
        assert!(same_conv.is_err());
        // Half a session pointer is refused by the CHECK.
        let half = sqlx::query("UPDATE iyke_seats SET session_kind = 'run' WHERE id = ?")
            .bind(b.clone())
            .execute(&pool)
            .await;
        assert!(half.is_err());
        // The seat's agent row (§1.5).
        let (name, metadata): (String, String) =
            sqlx::query_as("SELECT name, metadata FROM iyke_agents WHERE id = ?")
                .bind(a.clone())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "seat:default/a");
        assert_eq!(
            serde_json::from_str::<Value>(&metadata).unwrap(),
            json!({ "seat": true })
        );
    }

    #[tokio::test]
    async fn create_refusals() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let code = |r: Result<(SeatMoveResult, Effects), SeatError>| r.unwrap_err().code;
        assert_eq!(
            code(create_core(&pool, &w, empty_req("Bad", "claude-code", "default"), &ui()).await),
            "invalid_seat_name"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("g", "gemini", "default"), &ui()).await),
            "engine_unsupported"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("c", "cursor-agent", "default"), &ui()).await),
            "engine_unsupported"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("x", "codex", "old"), &ui()).await),
            "project_not_found"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("x", "codex", "nope"), &ui()).await),
            "project_not_found"
        );
        create(&pool, "lead", "claude-code").await;
        assert_eq!(
            code(create_core(&pool, &w, empty_req("lead", "codex", "default"), &ui()).await),
            "seat_name_taken"
        );
        // The same name in another project is fine: unique per project.
        assert!(
            create_core(&pool, &w, empty_req("lead", "codex", "royalti-co"), &ui())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn create_with_an_open_session_is_a_move() {
        let pool = pool().await;
        let w = world_with(&["t5"]);
        let old = create(&pool, "old-seat", "claude-code").await;
        move_core(&pool, &w, &term_ref("t5", "claude-code"), &old, &ui(), None)
            .await
            .unwrap();
        let req = CreateSeatReq {
            project_id: Some("default".into()),
            name: "fresh".into(),
            engine_id: "claude-code".into(),
            start: SeatStart::Session {
                session: term_ref("t5", "claude-code"),
            },
        };
        let (result, effects) = create_core(&pool, &w, req, &ui()).await.unwrap();
        assert_eq!(result.from_seat_ids, vec![old.clone()]);
        assert!(row(&pool, &old).await.session_ref.is_none());
        assert_eq!(effects.events[0].kinds, vec!["created", "bound"]);
        // A session on another engine is refused.
        let req = CreateSeatReq {
            project_id: Some("default".into()),
            name: "mismatch".into(),
            engine_id: "claude-code".into(),
            start: SeatStart::Session {
                session: term_ref("t5", "codex"),
            },
        };
        assert_eq!(
            create_core(&pool, &w, req, &ui()).await.unwrap_err().code,
            "engine_mismatch"
        );
    }

    // ── DEC-69c: moves (§4.3) ───────────────────────────────────────────

    #[tokio::test]
    async fn move_unbinds_from_every_other_seat_atomically() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");

        let (first, _) = move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();
        assert!(first.from_seat_ids.is_empty());
        assert_eq!(row(&pool, &a).await.session_ref.as_deref(), Some("t1"));

        let (second, effects) = move_core(&pool, &w, &t1, &b, &ui(), None).await.unwrap();
        assert_eq!(second.from_seat_ids, vec![a.clone()]);
        assert!(row(&pool, &a).await.session_ref.is_none());
        assert_eq!(row(&pool, &b).await.session_ref.as_deref(), Some("t1"));
        // One event per affected seat (§10).
        assert_eq!(effects.events.len(), 2);
        assert_eq!(effects.events[0].seat_id, b);
        assert_eq!(effects.events[0].kinds, vec!["bound"]);
        assert_eq!(effects.events[0].from_seat_ids, Some(vec![a.clone()]));
        assert_eq!(effects.events[1].seat_id, a);
        assert_eq!(effects.events[1].kinds, vec!["unbound"]);

        // Moving into the seat that already holds it is a no-op refresh.
        let (again, _) = move_core(&pool, &w, &t1, &b, &ui(), None).await.unwrap();
        assert!(again.from_seat_ids.is_empty());
        assert_eq!(row(&pool, &b).await.session_ref.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn move_by_conversation_unbinds_the_other_ref() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        set_session(&pool, &a, "terminal", "t-old", Some("conv-7")).await;
        insert_run(&pool, "run-7", "claude-code", "done", Some("conv-7")).await;
        let run = SeatSessionRef::Run {
            run_id: "run-7".into(),
        };
        let (moved, _) = move_core(&pool, &w, &run, &b, &ui(), None).await.unwrap();
        assert_eq!(moved.from_seat_ids, vec![a.clone()]);
        assert!(row(&pool, &a).await.external_id.is_none());
        assert_eq!(row(&pool, &b).await.external_id.as_deref(), Some("conv-7"));
    }

    #[tokio::test]
    async fn concurrent_moves_never_seat_a_session_twice() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let c = create(&pool, "c", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");
        move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();

        let actor = ui();
        let (r1, r2) = tokio::join!(
            move_core(&pool, &w, &t1, &b, &actor, None),
            move_core(&pool, &w, &t1, &c, &actor, None),
        );
        assert!(r1.is_ok() || r2.is_ok());
        for r in [&r1, &r2] {
            if let Err(e) = r {
                assert_eq!(e.code, "conflict");
            }
        }
        let seated = count(
            &pool,
            "SELECT COUNT(*) FROM iyke_seats WHERE session_ref = ?",
            "t1",
        )
        .await;
        assert_eq!(seated, 1, "one session, at most one seat");
        assert!(row(&pool, &a).await.session_ref.is_none());
    }

    #[tokio::test]
    async fn move_checks_engine_and_visibility() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let e = move_core(&pool, &w, &term_ref("t1", "codex"), &a, &ui(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "engine_mismatch");
        // Not a PTY this app can see (e.g. a daemon terminal, P-10).
        let e = move_core(
            &pool,
            &w,
            &term_ref("daemon-1", "claude-code"),
            &a,
            &ui(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "terminal_not_found");
        assert!(row(&pool, &a).await.session_ref.is_none());
    }

    // ── DEC-69b: Clear keeps the pad (§4.2) ─────────────────────────────

    #[tokio::test]
    async fn clear_keeps_the_pad_and_all_memory() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        move_core(&pool, &w, &term_ref("t1", "claude-code"), &a, &ui(), None)
            .await
            .unwrap();
        seed_memory(&pool, "seat:default/a").await;
        exec(
            &pool,
            "INSERT INTO iyke_agent_inbox (id, agent_id, kind, payload, created_at)
             VALUES ('i1', ?, 'timer-fired', '{}', 1)",
            &a,
        )
        .await;

        let (view, effects) = clear_core(&pool, &w, &a, &ui()).await.unwrap();
        assert!(view.session.is_none());
        assert_eq!(view.status, SeatStatus::Vacant);
        assert_eq!(view.resume.reason, Some(NotResumableReason::NoSession));
        assert_eq!(
            counts_under(&pool, "seat:default/a").await,
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(view.pad.count, 1);
        assert_eq!(view.inbox_count, 1);
        assert_eq!(effects.events[0].kinds, vec!["cleared"]);
    }

    /// Round 47 erratum E-4: clearing a seat with a queued text reports the
    /// drop on its event instead of only logging it.
    #[tokio::test]
    async fn clear_reports_a_dropped_queued_text() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let a = create(&pool, "q", "claude-code").await;
        assert!(store().enqueue(
            &a,
            QueuedText {
                prompt: "later".into(),
                since: 1,
                client: "ui".into(),
            },
        ));
        let (_view, effects) = clear_core(&pool, &w, &a, &ui()).await.unwrap();
        assert_eq!(effects.events[0].kinds, vec!["cleared", "queue-dropped"]);
        assert_eq!(effects.events[0].queue_dropped, Some("cleared"));
        assert!(store().queued(&a).is_none());
    }

    // ── DEC-69a: resume, then send (§4.1 path H, §6.2) ──────────────────

    #[tokio::test]
    async fn resume_binds_only_after_the_engine_call_returns() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let docs = create(&pool, "docs", "claude-code").await;
        set_session(&pool, &docs, "terminal", "tab-9", Some("conv-9")).await;

        let probe_pool = pool.clone();
        let probe_seat = docs.clone();
        let (result, effects) = resume_core(
            &pool,
            &w,
            &docs,
            "hello".to_string(),
            &ui(),
            ResumeFallback::Refuse,
            move |call| async move {
                // At call time nothing is bound yet: the seat still points
                // at its old terminal session.
                let before = fetch_seat(&probe_pool, &probe_seat).await.unwrap().unwrap();
                assert_eq!(before.session_ref.as_deref(), Some("tab-9"));
                // The text is the resumed conversation's first turn.
                assert_eq!(
                    call,
                    EngineCall::Start {
                        engine_id: "claude-code".into(),
                        prompt: "hello".into(),
                        cwd: Some("/work/docs".into()),
                        resume_session_id: Some("conv-9".into()),
                        persistent: false,
                    }
                );
                insert_run(
                    &probe_pool,
                    "run-new",
                    "claude-code",
                    "running",
                    Some("conv-9"),
                )
                .await;
                Ok("run-new".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::Resumed);
        assert_eq!(result.run_id, "run-new");
        assert!(matches!(
            result.previous,
            Some(SeatSession::Terminal { .. })
        ));
        assert_eq!(result.seat.status, SeatStatus::Run);
        let after = row(&pool, &docs).await;
        assert_eq!(after.session_kind.as_deref(), Some("run"));
        assert_eq!(after.session_ref.as_deref(), Some("run-new"));
        assert_eq!(after.external_id.as_deref(), Some("conv-9"));
        assert_eq!(effects.events[0].kinds, vec!["bound"]);

        // Now occupied: a second resume is refused, and never reaches the engine.
        let e = resume_core(
            &pool,
            &w,
            &docs,
            "x".into(),
            &ui(),
            ResumeFallback::Fresh,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_not_vacant");
    }

    #[tokio::test]
    async fn resume_of_a_run_continues_the_same_run_id() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "codex").await;
        insert_run(&pool, "run-1", "codex", "failed", Some("thread-1")).await;
        set_session(&pool, &s, "run", "run-1", Some("thread-1")).await;
        let (result, _) = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Refuse,
            |call| async move {
                assert_eq!(
                    call,
                    EngineCall::ResumeRun {
                        run_id: "run-1".into(),
                        prompt: "go".into()
                    }
                );
                Ok("run-1".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn engine_failure_writes_nothing() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "docs", "claude-code").await;
        set_session(&pool, &s, "terminal", "tab-1", Some("conv-1")).await;
        let before = row(&pool, &s).await;
        let e = resume_core(
            &pool,
            &w,
            &s,
            "hi".into(),
            &ui(),
            ResumeFallback::Fresh,
            |_| async { Err::<String, String>("claude not found".to_string()) },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "engine_failed");
        assert_eq!(row(&pool, &s).await, before);
    }

    #[tokio::test]
    async fn not_resumable_is_flagged_refused_and_falls_back_fresh() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "openrouter").await;
        insert_run(&pool, "run-or", "openrouter", "done", None).await;
        set_session(&pool, &s, "run", "run-or", None).await;
        // After a restart the adapter holds no transcript.
        let restarted = WorldSnapshot {
            openrouter_registered: true,
            ..WorldSnapshot::default()
        };
        let view = view_by_id(&pool, &restarted, &s).await.unwrap();
        assert_eq!(view.status, SeatStatus::Vacant);
        assert_eq!(view.resume.reason, Some(NotResumableReason::ProcessLocal));
        // The "not resumable after restart" flag is carried at all times.
        assert_eq!(view.engine_resume, EngineResume::ProcessLocal);

        // An explicit resume never falls back.
        let e = resume_core(
            &pool,
            &restarted,
            &s,
            "x".into(),
            &ui(),
            ResumeFallback::Refuse,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "not_resumable");
        assert_eq!(e.details, Some(json!({ "reason": "process_local" })));
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-or"));

        // A dispatch starts fresh, and says why.
        let probe_pool = pool.clone();
        let (result, _) = resume_core(
            &pool,
            &restarted,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Fresh,
            move |call| async move {
                assert_eq!(
                    call,
                    EngineCall::Start {
                        engine_id: "openrouter".into(),
                        prompt: "go".into(),
                        cwd: Some("/work/default".into()),
                        resume_session_id: None,
                        persistent: false,
                    }
                );
                insert_run(&probe_pool, "run-or-2", "openrouter", "running", None).await;
                Ok("run-or-2".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::StartedFresh);
        assert_eq!(result.reason, Some(NotResumableReason::ProcessLocal));
        assert_eq!(
            row(&pool, &s).await.session_ref.as_deref(),
            Some("run-or-2")
        );

        // Before a restart the same kind of done run is idle.
        let live = WorldSnapshot {
            openrouter_registered: true,
            openrouter_threads: HashMap::from([("run-or-2".to_string(), true)]),
            ..WorldSnapshot::default()
        };
        sqlx::query("UPDATE chi_cache SET status = 'done' WHERE run_id = 'run-or-2'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            view_by_id(&pool, &live, &s).await.unwrap().status,
            SeatStatus::Idle
        );
    }

    #[tokio::test]
    async fn fill_starts_a_new_run_and_unseats_the_previous() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let s = create(&pool, "lead", "claude-code").await;
        move_core(&pool, &w, &term_ref("t1", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        let probe_pool = pool.clone();
        let (result, _) = fill_core(
            &pool,
            &w,
            &s,
            "start".into(),
            &ui(),
            true,
            move |call| async move {
                assert!(matches!(
                    &call,
                    EngineCall::Start {
                        resume_session_id: None,
                        persistent: true,
                        ..
                    }
                ));
                insert_run(&probe_pool, "run-f", "claude-code", "running", None).await;
                Ok("run-f".to_string())
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            result.previous,
            Some(SeatSession::Terminal { .. })
        ));
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-f"));
    }

    // ── §4.1 claim, §4.5 queue, resolve ─────────────────────────────────

    #[tokio::test]
    async fn resume_claim_blocks_a_second_resume() {
        let pool = pool().await;
        let w = world_with(&["t2"]);
        let s = create(&pool, "c1", "claude-code").await;
        let (route, _) = resolve_core(&pool, &w, &s, &ui(), true).await.unwrap();
        let claim = match route {
            SeatRoute::Vacant { claim, resume, .. } => {
                assert_eq!(resume.reason, Some(NotResumableReason::NoSession));
                claim.expect("a claim")
            }
            other => panic!("expected vacant, got {other:?}"),
        };
        assert_eq!(
            resolve_core(&pool, &w, &s, &ui(), true)
                .await
                .unwrap_err()
                .code,
            "seat_resuming"
        );
        let e = resume_core(
            &pool,
            &w,
            &s,
            "x".into(),
            &ui(),
            ResumeFallback::Fresh,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_resuming");
        // Path T step 3: the bind clears the claim.
        let (moved, _) = move_core(
            &pool,
            &w,
            &term_ref("t2", "claude-code"),
            &s,
            &ui(),
            Some(claim.as_str()),
        )
        .await
        .unwrap();
        assert!(!moved.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_none());
    }

    #[tokio::test]
    async fn resolve_routes_by_agent_liveness() {
        let pool = pool().await;
        let s = create(&pool, "lead", "claude-code").await;
        let mut w = world_with(&["t7"]);
        move_core(&pool, &w, &term_ref("t7", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        // Not reported since the PTY spawned: fail closed.
        assert_eq!(
            resolve_core(&pool, &w, &s, &ui(), false)
                .await
                .unwrap_err()
                .code,
            "agent_not_live"
        );
        // Live, between turns.
        w.agents.insert(
            "t7".into(),
            AgentLive {
                pty_id: Some("pty-t7".into()),
                ..AgentLive::default()
            },
        );
        match resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0 {
            SeatRoute::Pty { agent, seat, .. } => {
                assert_eq!(agent, AgentState::Live);
                assert_eq!(seat.status, SeatStatus::Idle);
            }
            other => panic!("expected pty, got {other:?}"),
        }
        // Exited to its shell: vacant, never a PTY route.
        w.agents.get_mut("t7").unwrap().exited = true;
        assert!(matches!(
            resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0,
            SeatRoute::Vacant { .. }
        ));
    }

    #[tokio::test]
    async fn queue_has_one_slot() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-q", "claude-code", "running", Some("conv-q")).await;
        set_session(&pool, &s, "run", "run-q", Some("conv-q")).await;
        match resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0 {
            SeatRoute::ChiResume { busy, run_id, .. } => {
                assert!(busy);
                assert_eq!(run_id, "run-q");
            }
            other => panic!("expected chi-resume, got {other:?}"),
        }
        let (view, _) = queue_core(&pool, &w, &s, "next".into(), &ui())
            .await
            .unwrap();
        assert!(view.queued.is_some());
        assert_eq!(
            queue_core(&pool, &w, &s, "again".into(), &ui())
                .await
                .unwrap_err()
                .code,
            "seat_busy"
        );
        store().dequeue(&s);
    }

    // ── §5 holds and takeover ───────────────────────────────────────────

    #[tokio::test]
    async fn hold_takeover_and_displaced_once() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "lead", "claude-code").await;
        let orch_hold = SeatActor {
            hold: true,
            ..actor("orch")
        };
        clear_core(&pool, &w, &s, &orch_hold).await.unwrap();
        assert_eq!(row(&pool, &s).await.hold_client.as_deref(), Some("orch"));

        // Another client is refused, with "held by X since T".
        let e = clear_core(&pool, &w, &s, &ui()).await.unwrap_err();
        assert_eq!(e.code, "seat_held");
        let details = e.details.clone().unwrap();
        assert_eq!(details["client"], json!("orch"));
        assert!(details.get("since").is_some());

        // An explicit takeover transfers the hold and records the displaced.
        let take = SeatActor {
            takeover: true,
            ..ui()
        };
        let (view, effects) = clear_core(&pool, &w, &s, &take).await.unwrap();
        assert_eq!(view.hold.as_ref().unwrap().client, "ui");
        assert_eq!(effects.events[0].kinds, vec!["cleared", "taken-over"]);
        let after = row(&pool, &s).await;
        assert_eq!(after.displaced_client.as_deref(), Some("orch"));
        assert_eq!(after.displaced_by.as_deref(), Some("ui"));

        // The displaced client is told once, on its next call…
        let e = clear_core(&pool, &w, &s, &actor("orch")).await.unwrap_err();
        assert_eq!(e.code, "seat_taken_over");
        assert_eq!(e.details.clone().unwrap()["by"], json!("ui"));
        assert!(row(&pool, &s).await.displaced_client.is_none());
        // …then treated as any other client.
        assert_eq!(
            clear_core(&pool, &w, &s, &actor("orch"))
                .await
                .unwrap_err()
                .code,
            "seat_held"
        );

        // Release: someone else's hold needs takeover; one's own doesn't.
        assert_eq!(
            release_core(&pool, &w, &s, &actor("orch"))
                .await
                .unwrap_err()
                .code,
            "seat_held"
        );
        let (released, _) = release_core(&pool, &w, &s, &ui()).await.unwrap();
        assert!(released.hold.is_none());
        assert!(clear_core(&pool, &w, &s, &actor("orch")).await.is_ok());

        // An expired hold is absent.
        exec(
            &pool,
            "UPDATE iyke_seats SET hold_client = 'orch', hold_since = 1, hold_expires_at = 2
             WHERE id = ?",
            &s,
        )
        .await;
        assert!(clear_core(&pool, &w, &s, &ui()).await.is_ok());
    }

    #[tokio::test]
    async fn a_move_checks_the_hold_on_the_seat_it_unbinds() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");
        move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();
        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'orch', hold_since = 1, hold_expires_at = ?
             WHERE id = ?",
        )
        .bind(now_ms() + 600_000)
        .bind(a.clone())
        .execute(&pool)
        .await
        .unwrap();

        let e = move_core(&pool, &w, &t1, &b, &ui(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "seat_held");
        assert_eq!(row(&pool, &a).await.session_ref.as_deref(), Some("t1"));
        assert!(row(&pool, &b).await.session_ref.is_none());

        let take = SeatActor {
            takeover: true,
            ..ui()
        };
        let (moved, effects) = move_core(&pool, &w, &t1, &b, &take, None).await.unwrap();
        assert_eq!(moved.from_seat_ids, vec![a.clone()]);
        let a_after = row(&pool, &a).await;
        assert!(a_after.session_ref.is_none());
        assert_eq!(a_after.displaced_client.as_deref(), Some("orch"));
        assert_eq!(effects.events[1].kinds, vec!["unbound", "taken-over"]);
    }

    // ── §3.4 rename, §4.2 remove ────────────────────────────────────────

    #[tokio::test]
    async fn rename_rewrites_all_five_scope_tables() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "lead", "claude-code").await;
        seed_memory(&pool, "seat:default/lead").await;
        let (view, effects) = rename_core(&pool, &w, &s, "captain", &ui()).await.unwrap();
        assert_eq!(view.name, "captain");
        assert_eq!(view.address, "seat:default/captain");
        assert_eq!(
            counts_under(&pool, "seat:default/lead").await,
            vec![0, 0, 0, 0, 0]
        );
        assert_eq!(
            counts_under(&pool, "seat:default/captain").await,
            vec![1, 1, 1, 1, 1]
        );
        let agent: String = sqlx::query_scalar("SELECT name FROM iyke_agents WHERE id = ?")
            .bind(s.clone())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(agent, "seat:default/captain");
        assert_eq!(effects.events[0].kinds, vec!["renamed"]);
        // Watchers of the moved pad wake under both keys.
        assert!(effects
            .pads
            .iter()
            .any(|p| p.scope == "seat:default/lead" && p.name == "notes" && p.deleted));
        assert!(effects
            .pads
            .iter()
            .any(|p| p.scope == "seat:default/captain" && p.name == "notes" && !p.deleted));
    }

    #[tokio::test]
    async fn rename_refuses_on_conflict_and_changes_nothing() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "a1", "claude-code").await;
        create(&pool, "taken", "claude-code").await;
        seed_memory(&pool, "seat:default/a1").await;
        // Memory already under the new address (e.g. a removed seat's).
        exec(
            &pool,
            "INSERT INTO iyke_kv (scope, key, value, updated_at) VALUES (?, 'k', '1', 1)",
            "seat:default/b1",
        )
        .await;
        let e = rename_core(&pool, &w, &s, "b1", &ui()).await.unwrap_err();
        assert_eq!(e.code, "rename_scope_conflict");
        assert_eq!(row(&pool, &s).await.name, "a1");
        assert_eq!(
            counts_under(&pool, "seat:default/a1").await,
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(
            rename_core(&pool, &w, &s, "taken", &ui())
                .await
                .unwrap_err()
                .code,
            "seat_name_taken"
        );
        assert_eq!(
            rename_core(&pool, &w, &s, "Nope", &ui())
                .await
                .unwrap_err()
                .code,
            "invalid_seat_name"
        );
    }

    #[tokio::test]
    async fn remove_with_and_without_memory() {
        let pool = pool().await;
        let keep = create(&pool, "keep", "claude-code").await;
        seed_memory(&pool, "seat:default/keep").await;
        exec(
            &pool,
            "INSERT INTO iyke_agent_inbox (id, agent_id, kind, payload, created_at)
             VALUES ('i2', ?, 'timer-fired', '{}', 1)",
            &keep,
        )
        .await;
        let (removed, effects) = remove_core(&pool, &keep, false, &ui()).await.unwrap();
        assert_eq!(removed.seat_id, keep);
        assert_eq!(effects.events[0].kinds, vec!["removed"]);
        assert!(fetch_seat(&pool, &keep).await.unwrap().is_none());
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM iyke_agents WHERE id = ?",
                &keep
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM iyke_agent_inbox WHERE agent_id = ?",
                &keep
            )
            .await,
            0
        );
        assert_eq!(
            counts_under(&pool, "seat:default/keep").await,
            vec![1, 1, 1, 1, 1]
        );

        let wipe = create(&pool, "wipe", "claude-code").await;
        seed_memory(&pool, "seat:default/wipe").await;
        remove_core(&pool, &wipe, true, &ui()).await.unwrap();
        assert_eq!(
            counts_under(&pool, "seat:default/wipe").await,
            vec![0, 0, 0, 0, 0]
        );
    }

    // ── §2.5 hooks ──────────────────────────────────────────────────────

    #[test]
    fn hook_events_drive_the_liveness_map() {
        let mut agents = HashMap::new();
        let pty = || Some("pty-1".to_string());
        apply_hook(
            &mut agents,
            "t1",
            "SessionStart",
            Some("sess".into()),
            pty(),
        );
        assert_eq!(
            agents["t1"],
            AgentLive {
                pty_id: pty(),
                exited: false,
                turn_in_flight: false,
                session_id: Some("sess".into()),
            }
        );
        apply_hook(&mut agents, "t1", "UserPromptSubmit", None, pty());
        assert!(agents["t1"].turn_in_flight);
        apply_hook(&mut agents, "t1", "Stop", None, pty());
        assert!(!agents["t1"].turn_in_flight);
        apply_hook(&mut agents, "t1", "SessionEnd", None, pty());
        assert!(agents["t1"].exited);
        apply_hook(&mut agents, "t1", "PreToolUse", None, pty());
        assert!(agents["t1"].exited, "other events change nothing");
        // A new PTY on the reused tab id starts over.
        apply_hook(
            &mut agents,
            "t1",
            "UserPromptSubmit",
            None,
            Some("pty-2".into()),
        );
        assert_eq!(agents["t1"].pty_id.as_deref(), Some("pty-2"));
        assert!(!agents["t1"].exited);
        assert!(agents["t1"].session_id.is_none());
    }

    #[tokio::test]
    async fn session_start_captures_the_resume_id() {
        let pool = pool().await;
        let w = world_with(&["t8"]);
        let s = create(&pool, "lead", "claude-code").await;
        move_core(&pool, &w, &term_ref("t8", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        let events = capture_core(&pool, "t8", "sess-1").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kinds, vec!["updated"]);
        assert_eq!(row(&pool, &s).await.external_id.as_deref(), Some("sess-1"));
        assert!(capture_core(&pool, "t8", "sess-1")
            .await
            .unwrap()
            .is_empty());
        assert!(capture_core(&pool, "unseated", "sess-2")
            .await
            .unwrap()
            .is_empty());
    }

    /// DEC-69c on the capture path: the conversation a new terminal resumed
    /// leaves the seat that still recorded it — unless that seat is held.
    #[tokio::test]
    async fn capture_moves_the_conversation_off_another_seat() {
        let pool = pool().await;
        let old = create(&pool, "old", "claude-code").await;
        let new = create(&pool, "new", "claude-code").await;
        set_session(&pool, &old, "terminal", "t-ended", Some("conv-1")).await;
        let w = world_with(&["t9"]);
        move_core(&pool, &w, &term_ref("t9", "claude-code"), &new, &ui(), None)
            .await
            .unwrap();
        let events = capture_core(&pool, "t9", "conv-1").await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seat_id, new);
        assert_eq!(events[0].from_seat_ids, Some(vec![old.clone()]));
        assert_eq!(events[1].seat_id, old);
        assert_eq!(events[1].kinds, vec!["unbound"]);
        assert_eq!(
            row(&pool, &new).await.external_id.as_deref(),
            Some("conv-1")
        );
        let old_row = row(&pool, &old).await;
        assert!(old_row.session_kind.is_none() && old_row.external_id.is_none());

        // A held seat keeps its conversation; the capture is skipped.
        let held = create(&pool, "held", "claude-code").await;
        let other = create(&pool, "other", "claude-code").await;
        set_session(&pool, &held, "terminal", "t-gone", Some("conv-2")).await;
        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'iyke', hold_since = ?, hold_expires_at = ?
             WHERE id = ?",
        )
        .bind(now_ms())
        .bind(now_ms() + 60_000)
        .bind(held.clone())
        .execute(&pool)
        .await
        .unwrap();
        let w = world_with(&["t10"]);
        move_core(
            &pool,
            &w,
            &term_ref("t10", "claude-code"),
            &other,
            &ui(),
            None,
        )
        .await
        .unwrap();
        assert!(capture_core(&pool, "t10", "conv-2")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            row(&pool, &held).await.external_id.as_deref(),
            Some("conv-2")
        );
        assert!(row(&pool, &other).await.external_id.is_none());
    }

    // ── §2.2 derivation table ───────────────────────────────────────────

    #[test]
    fn derivation_table() {
        let w = WorldSnapshot::default();
        let d = derive(&seat_row("claude-code", None, None, None), None, &w);
        assert_eq!(
            (d.status, d.resume.reason),
            (SeatStatus::Vacant, Some(NotResumableReason::NoSession))
        );

        let run = |engine: &str, ext: Option<&str>| seat_row(engine, Some("run"), Some("r1"), ext);
        let d = derive(&run("claude-code", None), None, &w);
        assert_eq!(d.resume.reason, Some(NotResumableReason::RunMissing));
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "running", None)),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Run);
        assert_eq!(
            d.hint,
            RouteHint::Chi {
                run_id: "r1".into(),
                busy: true
            }
        );
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "done", Some("c"))),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Idle);
        assert_eq!(
            d.hint,
            RouteHint::Chi {
                run_id: "r1".into(),
                busy: false
            }
        );
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "done", None)),
            &w,
        );
        assert_eq!(
            (d.status, d.resume.reason),
            (SeatStatus::Vacant, Some(NotResumableReason::NoResumeId))
        );
        let d = derive(
            &run("opencode", None),
            Some(&chi("opencode", "done", None)),
            &w,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::NoResumeSupport));
        let d = derive(
            &run("openrouter", None),
            Some(&chi("openrouter", "done", None)),
            &w,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::EngineUnavailable));
        let d = derive(
            &run("claude-code", Some("c")),
            Some(&chi("claude-code", "failed", Some("c"))),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Vacant);
        assert!(d.resume.resumable);
        let gone = WorldSnapshot {
            unavailable_engines: vec!["claude-code".into()],
            ..WorldSnapshot::default()
        };
        let d = derive(
            &run("claude-code", Some("c")),
            Some(&chi("claude-code", "cancelled", Some("c"))),
            &gone,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::EngineUnavailable));

        let tab = |engine: &str| seat_row(engine, Some("terminal"), Some("t1"), Some("c"));
        let d = derive(&tab("claude-code"), None, &w);
        assert_eq!(d.status, SeatStatus::Vacant, "unknown to PtyManager");
        assert!(d.resume.resumable);
        let mut live = world_with(&["t1"]);
        let d = derive(&tab("codex"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Unreported))
        );
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Starting))
        );
        assert_eq!(d.hint, RouteHint::AgentStarting);
        assert_eq!(d.mount.as_ref().unwrap().window_label, "main");
        live.agents.insert(
            "t1".into(),
            AgentLive {
                pty_id: Some("pty-t1".into()),
                turn_in_flight: true,
                ..AgentLive::default()
            },
        );
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Live))
        );
        live.agents.get_mut("t1").unwrap().pty_id = Some("pty-stale".into());
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            d.agent,
            Some(AgentState::Starting),
            "a stale PTY's report doesn't count"
        );
        live.agents.get_mut("t1").unwrap().pty_id = Some("pty-t1".into());
        live.agents.get_mut("t1").unwrap().exited = true;
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(d.status, SeatStatus::Vacant);
        live.terminals[0].running = false;
        live.agents.clear();
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(d.status, SeatStatus::Vacant, "PTY exited");
    }

    /// §9.4: the UI resolves with `claimResume` before every dispatch; a
    /// run-kind vacant seat then goes path H, which its own claim must not
    /// block — so such a seat gets no claim.
    #[tokio::test]
    async fn a_run_kind_vacant_seat_gets_no_claim() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "codex").await;
        insert_run(&pool, "run-v", "codex", "failed", Some("thread-v")).await;
        set_session(&pool, &s, "run", "run-v", Some("thread-v")).await;
        match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, resume, .. } => {
                assert!(claim.is_none());
                assert!(resume.resumable);
            }
            other => panic!("expected vacant, got {other:?}"),
        }
        let (result, _) = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Fresh,
            |_| async { Ok::<String, String>("run-v".to_string()) },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::Resumed);
    }

    /// An empty seat on a runs-only engine can only go path H, so resolving
    /// it with `claimResume` takes no claim and `seats_resume` goes through.
    #[tokio::test]
    async fn an_empty_runs_only_seat_gets_no_claim() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "scout", "opencode").await;
        match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, .. } => assert!(claim.is_none()),
            other => panic!("expected vacant, got {other:?}"),
        }
        assert!(store().live_claim(&s, now_ms()).is_none());
        let e = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Refuse,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(
            e.code, "not_resumable",
            "refused for its session, not a claim"
        );
    }

    /// A move without a claim leaves another client's live claim alone; the
    /// claim-carrying move then clears it and reports nothing lost.
    #[tokio::test]
    async fn a_plain_move_keeps_another_clients_claim() {
        let pool = pool().await;
        let w = world_with(&["t3", "t4"]);
        let s = create(&pool, "c2", "claude-code").await;
        let claim = match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, .. } => claim.expect("a claim"),
            other => panic!("expected vacant, got {other:?}"),
        };
        let (plain, _) = move_core(
            &pool,
            &w,
            &term_ref("t3", "claude-code"),
            &s,
            &actor("iyke"),
            None,
        )
        .await
        .unwrap();
        assert!(!plain.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_some());
        let (claimed, _) = move_core(
            &pool,
            &w,
            &term_ref("t4", "claude-code"),
            &s,
            &ui(),
            Some(claim.as_str()),
        )
        .await
        .unwrap();
        assert!(!claimed.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_none());
    }

    /// A bind that fails after the engine call returned keeps the refusal's
    /// details and adds the run id, so no caller resends the text blindly.
    #[test]
    fn a_failed_bind_after_the_engine_call_carries_the_run_id() {
        let hold = SeatHold {
            client: "iyke".into(),
            since: 1,
            expires_at: 2,
        };
        let e = SeatError::held("lead", &hold).after_engine("run-9");
        assert_eq!(e.code, "seat_held");
        let d = e.details.unwrap();
        assert_eq!(d["run_id"], "run-9");
        assert_eq!(d["client"], "iyke");
        let e = SeatError::conflict("busy").after_engine("run-8");
        assert_eq!(e.details.unwrap(), json!({ "run_id": "run-8" }));
    }

    #[test]
    fn wire_shapes() {
        let session = SeatSession::Run {
            run_id: "r".into(),
            external_id: None,
            cwd: None,
        };
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            json!({ "kind": "run", "run_id": "r", "external_id": null, "cwd": null })
        );
        assert_eq!(
            serde_json::to_value(SeatResume::no(NotResumableReason::NoResumeId)).unwrap(),
            json!({ "resumable": false, "reason": "no_resume_id" })
        );
        assert_eq!(
            serde_json::to_value(SeatResume::yes()).unwrap(),
            json!({ "resumable": true })
        );
        let r: SeatSessionRef = serde_json::from_value(json!({
            "kind": "terminal", "terminalId": "t", "engineId": "claude-code", "cwd": "/x"
        }))
        .unwrap();
        assert!(
            matches!(r, SeatSessionRef::Terminal { ref terminal_id, .. } if terminal_id == "t")
        );
        let a: SeatAddress = serde_json::from_value(json!({ "seatId": "abc" })).unwrap();
        assert!(matches!(a, SeatAddress::Id { ref seat_id } if seat_id == "abc"));
        let a: SeatAddress = serde_json::from_value(json!({ "address": "@lead" })).unwrap();
        assert!(matches!(a, SeatAddress::Address { .. }));
        let actor: SeatActor =
            serde_json::from_value(json!({ "client": "iyke", "hold": true, "holdTtlMs": 5 }))
                .unwrap();
        assert_eq!(hold_ttl(&actor), HOLD_TTL_MIN_MS);
        let start: SeatStart = serde_json::from_value(json!({ "kind": "empty" })).unwrap();
        assert!(matches!(start, SeatStart::Empty));
        assert_eq!(
            SeatError::not_resumable(NotResumableReason::ProcessLocal).http_status(),
            422
        );
        assert_eq!(SeatError::seat_not_found().http_status(), 404);
        assert_eq!(SeatError::engine_failed("x".into()).http_status(), 502);
        assert_eq!(SeatError::resuming("a").http_status(), 409);
    }

    // ── WP-70 bridge sends ──────────────────────────────────────────────

    async fn must_not_send(why: &'static str) -> Result<String, SeatError> {
        panic!("{why}")
    }

    #[tokio::test]
    async fn touch_last_active_bumps_any_seat() {
        let pool = pool().await;
        let s = create(&pool, "lead", "claude-code").await;
        touch_last_active(&pool, &s, 9_999_999_999_999).await.unwrap();
        assert_eq!(row(&pool, &s).await.last_active_at, 9_999_999_999_999);
    }

    #[tokio::test]
    async fn send_idle_sends_once_and_bumps_the_seat() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-i", "claude-code", "done", Some("conv-i")).await;
        set_session(&pool, &s, "run", "run-i", Some("conv-i")).await;
        let before = row(&pool, &s).await.last_active_at;
        let (out, effects) = send_idle_core(&pool, &s, "run-i", &ui(), || async {
            Ok::<_, SeatError>("run-i".to_string())
        })
        .await
        .unwrap();
        assert_eq!(
            out,
            IdleSend::Sent {
                run_id: "run-i".into()
            }
        );
        assert_eq!(effects.events.len(), 1);
        assert_eq!(effects.events[0].kinds, vec!["updated"]);
        assert!(row(&pool, &s).await.last_active_at >= before);
    }

    #[tokio::test]
    async fn send_idle_queues_when_the_run_went_busy_or_a_text_waits() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-b", "claude-code", "running", Some("conv-b")).await;
        set_session(&pool, &s, "run", "run-b", Some("conv-b")).await;
        let (out, _) = send_idle_core(&pool, &s, "run-b", &ui(), || {
            must_not_send("a busy run is never resumed over (§4.5)")
        })
        .await
        .unwrap();
        assert_eq!(out, IdleSend::Queue);

        sqlx::query("UPDATE chi_cache SET status = 'done' WHERE run_id = 'run-b'")
            .execute(&pool)
            .await
            .unwrap();
        store().enqueue(
            &s,
            QueuedText {
                prompt: "first".into(),
                since: 0,
                client: "ui".into(),
            },
        );
        let (out, _) = send_idle_core(&pool, &s, "run-b", &ui(), || {
            must_not_send("a direct send never overtakes a queued text (§4.5)")
        })
        .await
        .unwrap();
        assert_eq!(out, IdleSend::Queue);
        store().dequeue(&s);
    }

    #[tokio::test]
    async fn send_idle_refuses_a_seat_that_moved_or_is_held() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-m", "claude-code", "done", Some("conv-m")).await;
        insert_run(&pool, "run-n", "claude-code", "done", Some("conv-n")).await;
        set_session(&pool, &s, "run", "run-n", Some("conv-n")).await;
        let e = send_idle_core(&pool, &s, "run-m", &ui(), || {
            must_not_send("nothing is sent to a run the seat no longer holds")
        })
        .await
        .unwrap_err();
        assert_eq!(e.code, "conflict");

        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'orchestrator', hold_since = 0,
                    hold_expires_at = ? WHERE id = ?",
        )
        .bind(now_ms() + 60_000)
        .bind(s.clone())
        .execute(&pool)
        .await
        .unwrap();
        let e = send_idle_core(&pool, &s, "run-n", &ui(), || {
            must_not_send("a hold taken since resolve is honoured")
        })
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_held");
    }
}
