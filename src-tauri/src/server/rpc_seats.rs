//! `/api/rpc` bodies for the 13 `seats_*` commands (Chi seats, G-SEATS §9.2;
//! gap audit 2026-10-06 rank 7). The store CRUD was WP-19's, `seats_resume`
//! and `seats_fill` WP-18b's.
//!
//! Every body calls the core the desktop `#[tauri::command]` calls
//! (`server::shared::seats`), so the JSON shapes, the hold rules and DEC-69
//! are the desktop's by construction. A refusal answers `"<cmd>: <code>:
//! <message>"` with the serialized `SeatError` (`{code, message, details?}`,
//! §9.5) as the response's `error_data`, which the web transport puts back on
//! the thrown `Error` — so `seatErrorOf` reads the same typed rejection a
//! Tauri `invoke` gives. A malformed argument is a plain error, as Tauri's
//! own argument deserialization is.
//!
//! **Scope.** The seats live in `--data-dir`'s `ikenga.db`, keyed by that
//! database's projects. Under T1 every principal is its own child process
//! with its own data dir, so a principal reaches only its own seats: another
//! principal's seat id or address is "that seat no longer exists", the same
//! answer an unknown one gets. All 13 are owner-class (`rpc_requirements.rs`):
//! a share never reaches them.
//!
//! **The daemon's world** ([`daemon_world`]) differs from the desktop's
//! where the daemon cannot see what the desktop sees, and says so instead of
//! answering as if it could:
//! - no terminal can be seated — the daemon has neither the `hooks://event`
//!   bridge (agent liveness, the resume-id capture) nor pane state, so its
//!   PTYs never enter the world. Binding one is refused with
//!   [`NO_TERMINAL_SEATS`], `seats_resolve` grants no path-T claim, and
//!   `seats_engines` reports no terminal wrap: a vacant seat's first send is a
//!   headless Chi run (path H);
//! - `openrouter` (in-process, needs the desktop's engine registry + vault)
//!   is not seatable, with `chi_exec`'s reason, and `seats_create` refuses it;
//! - engine install state comes from the chi resolver: the augmented-PATH
//!   lookup `detect_agents` uses, and the very one a seat's run spawns
//!   through. A missing CLI reads "not installed on this server".
//!
//! **Engine calls** (`seats_resume`, `seats_fill`, the §4.5 queue) go through
//! `shared::chi_exec` with the env `chi_run` uses (`rpc_local::chi_env`), so
//! every spawn goes through `executor::current()` — under T1, as the
//! principal's own uid — and lands in the principal's own `chi_cache`.
//!
//! **Events.** The `seats://changed` events the cores return are published on
//! the daemon's event bus (`server::events`, `/ws/events`) under the
//! desktop's name and payload; so are the queue poller's. The scratchpad
//! wake-ups (`Effects::pads`) are desktop-only and dropped.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use super::events::{EventBus, Topic};
use super::rpc::RpcResponse;
use super::rpc_local::chi_env;
use super::rpc_shell::targ;
use super::shared::chi_exec::{self, ChiEnv, ChiRunOpts, NoInProcessEngines};
use super::shared::seats::{
    active_project, clear_core, create_core, drain_one, engines_info, fill_locked, list_core,
    list_rows, move_core, queue_core, release_core, remove_core, rename_core, resolve_address,
    resolve_core, resume_locked, store, view_of, CreateSeatReq, Effects, EngineCall, FillOpts,
    MoveOpts, RemoveOpts, ResolveOpts, ResumeOpts, SeatActor, SeatAddress, SeatError,
    SeatSessionRef, WorldSnapshot, ENGINE_CAPS, OPENROUTER_ENGINE, QUEUE_POLL, SEAT_RUN_OWNER,
};
use super::AppState;

/// Publish each `seats://changed` a committed command produced.
fn publish_effects(state: &AppState, effects: &Effects) {
    for event in &effects.events {
        state.events.publish(Topic::SeatsChanged, event);
    }
}

/// Why no terminal can be seated on the daemon (see the module doc).
pub(crate) const NO_TERMINAL_SEATS: &str =
    "Not available on this server: a terminal can't be seated here (the server has no \
     agent-liveness hooks or panes) — a seat here runs headless Chi runs instead";

/// What `seats_engines` says of an engine whose CLI the daemon can't find.
const NOT_INSTALLED_HERE: &str = "not installed on this server";

// ─── replies ─────────────────────────────────────────────────────────────────

/// A failed seat call: the store's typed refusal, or a plain error (a bad
/// argument, no `--data-dir`) as Tauri gives before reaching the command.
enum Fail {
    Seat(SeatError),
    Plain(String),
}

impl From<SeatError> for Fail {
    fn from(e: SeatError) -> Self {
        Fail::Seat(e)
    }
}

impl From<String> for Fail {
    fn from(e: String) -> Self {
        Fail::Plain(e)
    }
}

fn reply<T: Serialize>(cmd: &str, result: Result<T, Fail>) -> RpcResponse {
    match result {
        Ok(v) => RpcResponse::success(v),
        Err(Fail::Seat(e)) => {
            RpcResponse::error_with_data(format!("{cmd}: {e}"), serde_json::to_value(&e).ok())
        }
        Err(Fail::Plain(e)) => RpcResponse::error(format!("{cmd}: {e}")),
    }
}

async fn pool(state: &AppState) -> Result<SqlitePool, Fail> {
    let db = state
        .pa_db
        .as_deref()
        .ok_or_else(|| super::rpc::NO_DB.to_string())?;
    Ok(db.ensure_pool().await.map_err(SeatError::internal)?)
}

/// `actor` (§5.1), required on every write.
fn actor(args: &Value) -> Result<SeatActor, String> {
    targ(args, &["actor"])
}

fn seat_id(args: &Value) -> Result<String, String> {
    targ(args, &["seatId", "seat_id"])
}

// ─── the daemon's world and engine ───────────────────────────────────────────

/// The world derivation reads on the daemon: no terminals, no agent liveness,
/// no openrouter adapter, and every engine's install state (a handful of
/// PATH lookups, so it is probed whole on every call and is the same world
/// for any seat — no re-derive after a bind is needed).
fn daemon_world(state: &AppState) -> WorldSnapshot {
    let resolver = &state.chi.resolver;
    let unavailable_engines = ENGINE_CAPS
        .iter()
        .filter_map(|cap| {
            let binary = cap.binary?;
            let found = resolver.native(binary).is_some() || resolver.in_wsl(binary);
            (!found).then(|| cap.engine_id.to_string())
        })
        .collect();
    WorldSnapshot {
        unavailable_engines,
        terminals_unavailable: Some(NO_TERMINAL_SEATS),
        openrouter_unavailable: Some(chi_exec::HEADLESS_OPENROUTER),
        missing_engine_reason: Some(NOT_INSTALLED_HERE),
        ..WorldSnapshot::default()
    }
}

/// One engine call (§9.2) through the `chi_run` / `chi_resume` core.
async fn call_engine(env: &ChiEnv, call: EngineCall) -> Result<String, String> {
    let result = match call {
        EngineCall::ResumeRun { run_id, prompt } => {
            chi_exec::resume_run(env, &NoInProcessEngines, run_id, prompt).await?
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
            chi_exec::spawn_run(env, &NoInProcessEngines, opts, SEAT_RUN_OWNER).await?
        }
    };
    Ok(result.run_id)
}

// ─── reads ───────────────────────────────────────────────────────────────────

/// Views of every seat in `projectId` (default: the active project).
pub(super) async fn seats_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
        let pool = pool(state).await?;
        let project = match project_id.filter(|p| !p.is_empty()) {
            Some(p) => p,
            None => active_project(&pool).await?,
        };
        let rows = list_rows(&pool, &project).await?;
        Ok(list_core(&pool, &daemon_world(state), rows).await?)
    }
    .await;
    reply("seats_list", r)
}

/// One view, by id or by any §1.3 address.
pub(super) async fn seats_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat: SeatAddress = targ(args, &["seat"])?;
        let pool = pool(state).await?;
        let row = resolve_address(&pool, &seat).await?;
        Ok(view_of(&pool, &daemon_world(state), row).await?)
    }
    .await;
    reply("seats_get", r)
}

/// §6.1 with this server's install state, for the create form. Reads no
/// database, so it answers without `--data-dir` too.
pub(super) async fn seats_engines(state: &AppState) -> RpcResponse {
    reply(
        "seats_engines",
        Ok::<_, Fail>(engines_info(&daemon_world(state))),
    )
}

// ─── writes ──────────────────────────────────────────────────────────────────

/// Where a send goes now; applies holds, may take a resume claim (never on
/// the daemon: no path T here).
pub(super) async fn seats_resolve(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat: SeatAddress = targ(args, &["seat"])?;
        let actor = actor(args)?;
        let opts: Option<ResolveOpts> = targ(args, &["opts"])?;
        let pool = pool(state).await?;
        let row = resolve_address(&pool, &seat).await?;
        let claim_resume = opts.map(|o| o.claim_resume).unwrap_or(false);
        let world = daemon_world(state);
        let (route, effects) = resolve_core(&pool, &world, &row.id, &actor, claim_resume).await?;
        publish_effects(state, &effects);
        Ok(route)
    }
    .await;
    reply("seats_resolve", r)
}

pub(super) async fn seats_create(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let req: CreateSeatReq = targ(args, &["req"])?;
        let actor = actor(args)?;
        if req.engine_id == OPENROUTER_ENGINE {
            // Statically seatable, but its runs can never start here.
            return Err(
                SeatError::new("engine_unsupported", chi_exec::HEADLESS_OPENROUTER)
                    .with_details(json!({ "engine_id": req.engine_id }))
                    .into(),
            );
        }
        let pool = pool(state).await?;
        let (result, effects) = create_core(&pool, &daemon_world(state), req, &actor).await?;
        publish_effects(state, &effects);
        Ok(result)
    }
    .await;
    reply("seats_create", r)
}

/// DEC-69c. On the daemon only a Chi run can be moved in.
pub(super) async fn seats_move(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let session: SeatSessionRef = targ(args, &["session"])?;
        let to_seat_id: String = targ(args, &["toSeatId", "to_seat_id"])?;
        let actor = actor(args)?;
        let opts: Option<MoveOpts> = targ(args, &["opts"])?;
        let pool = pool(state).await?;
        let claim = opts.and_then(|o| o.claim);
        let world = daemon_world(state);
        let (result, effects) = move_core(
            &pool,
            &world,
            &session,
            &to_seat_id,
            &actor,
            claim.as_deref(),
        )
        .await?;
        publish_effects(state, &effects);
        Ok(result)
    }
    .await;
    reply("seats_move", r)
}

/// DEC-69a path H: resume a vacant seat with `prompt` as its first turn (or,
/// `fallback: 'fresh'`, start a new run), bound after the engine returns.
pub(super) async fn seats_resume(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let prompt: String = targ(args, &["prompt"])?;
        let actor = actor(args)?;
        let opts: ResumeOpts = targ(args, &["opts"])?;
        let pool = pool(state).await?;
        let env = chi_env(state)?;
        // The seat's mutex first, as the desktop command takes it (§4.1).
        let _guard = store().lock_one(&seat_id).await;
        let world = daemon_world(state);
        let env = &env;
        let (result, effects) = resume_locked(
            &pool,
            &world,
            &seat_id,
            prompt,
            &actor,
            opts.fallback,
            move |call| async move { call_engine(env, call).await },
        )
        .await?;
        publish_effects(state, &effects);
        Ok(result)
    }
    .await;
    reply("seats_resume", r)
}

/// A new run on the seat's engine in the project root; the previous session
/// is unseated.
pub(super) async fn seats_fill(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let prompt: String = targ(args, &["prompt"])?;
        let actor = actor(args)?;
        let opts: Option<FillOpts> = targ(args, &["opts"])?;
        let pool = pool(state).await?;
        let env = chi_env(state)?;
        let _guard = store().lock_one(&seat_id).await;
        let world = daemon_world(state);
        let persistent = opts.map(|o| o.persistent).unwrap_or(false);
        let env = &env;
        let (result, effects) = fill_locked(
            &pool,
            &world,
            &seat_id,
            prompt,
            &actor,
            persistent,
            move |call| async move { call_engine(env, call).await },
        )
        .await?;
        publish_effects(state, &effects);
        Ok(result)
    }
    .await;
    reply("seats_fill", r)
}

/// §4.5: park one text for a seat whose run has a turn in flight; this
/// process's poller sends it when the run ends.
pub(super) async fn seats_queue(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let prompt: String = targ(args, &["prompt"])?;
        let actor = actor(args)?;
        let pool = pool(state).await?;
        // Built first: a queue the poller could never send is refused here.
        let env = Arc::new(chi_env(state)?);
        let (seat, effects) =
            queue_core(&pool, &daemon_world(state), &seat_id, prompt, &actor).await?;
        publish_effects(state, &effects);
        watch_queue(seat_id, env, state.events.clone());
        Ok(seat)
    }
    .await;
    reply("seats_queue", r)
}

/// DEC-69b: drop the session pointer; the pad and all memory stay.
pub(super) async fn seats_clear(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let actor = actor(args)?;
        let pool = pool(state).await?;
        let (seat, effects) = clear_core(&pool, &daemon_world(state), &seat_id, &actor).await?;
        publish_effects(state, &effects);
        Ok(seat)
    }
    .await;
    reply("seats_clear", r)
}

pub(super) async fn seats_rename(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let name: String = targ(args, &["name"])?;
        let actor = actor(args)?;
        let pool = pool(state).await?;
        let world = daemon_world(state);
        let (seat, effects) = rename_core(&pool, &world, &seat_id, &name, &actor).await?;
        publish_effects(state, &effects);
        Ok(seat)
    }
    .await;
    reply("seats_rename", r)
}

pub(super) async fn seats_remove(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let opts: RemoveOpts = targ(args, &["opts"])?;
        let actor = actor(args)?;
        let pool = pool(state).await?;
        let (result, effects) = remove_core(&pool, &seat_id, opts.remove_memory, &actor).await?;
        publish_effects(state, &effects);
        Ok(result)
    }
    .await;
    reply("seats_remove", r)
}

pub(super) async fn seats_release(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let seat_id = seat_id(args)?;
        let actor = actor(args)?;
        let pool = pool(state).await?;
        let (seat, effects) = release_core(&pool, &daemon_world(state), &seat_id, &actor).await?;
        publish_effects(state, &effects);
        Ok(seat)
    }
    .await;
    reply("seats_release", r)
}

// ─── the §4.5 queue poller ───────────────────────────────────────────────────
//
// The queue slot itself is the store's (in-process, one per seat). Each text
// queued here gets its own poll loop on the caller's runtime, holding the env
// of the principal whose run it waits on, and ending once the slot is empty
// (sent, dropped, cleared or removed). `drain_one` takes the seat's mutex and
// empties the slot before it sends, so a loop that outlives its text (a new
// one was queued) never sends twice.

fn watch_queue(seat_id: String, env: Arc<ChiEnv>, events: Arc<EventBus>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(QUEUE_POLL).await;
            if store().queued(&seat_id).is_none() {
                return;
            }
            let pool = match env.db.ensure_pool().await {
                Ok(pool) => pool,
                Err(e) => {
                    tracing::warn!(target: "ikenga::seats", "queue: open db for {seat_id}: {e}");
                    continue;
                }
            };
            let env = &env;
            // A sent or dropped text is a §10 event (`queue-dropped` carries the
            // reason); a dropped one is also logged by `drain_one`.
            let event = drain_one(&pool, &seat_id, move |call| async move {
                call_engine(env, call).await
            })
            .await;
            if let Some(event) = event {
                events.publish(Topic::SeatsChanged, &event);
            }
        }
    });
}

#[cfg(test)]
#[path = "rpc_seats_tests.rs"]
mod tests;
