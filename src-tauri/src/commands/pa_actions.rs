//! Tauri commands for the approve-gate run-then-pause draft queue (WP-3).
//!
//! The producer side of the approve-gate seam
//! (`plans/atelier/10-approve-gate-seam.md`; behaviour `07-fe-button-renderer.md`
//! §3.5). An approve-aware action does its work, then — instead of sending —
//! pauses by handing the shell a batch of drafts (`pa_actions_pause`). Each
//! becomes a row in `pa_action_drafts` (migration 0050) with `status='awaiting'`
//! and a `pa-action-paused` event fires. The approve-gate panel at
//! `/outbox/approvals` reads rows via `pa_actions_list`, the operator edits in
//! place (`pa_actions_update`), and on Approve & Send (after the FE's 10s undo)
//! `pa_actions_commit` flips the row to `committed` and emits
//! `pa-action-committed` — consumed by the EXTERNAL mutation worker, which
//! performs the real SMTP/Resend/Listmonk/Buffer send and writes
//! `status='sent'`. Reject → `pa_actions_reject` → `pa-action-rejected`. **The
//! shell never sends.**
//!
//! Rust stays a thin store: `payload_json` (the DraftItem + ApproveGateMeta) is
//! opaque here and parsed FE-side via `@ikenga/contract` `fromDraftItem`.
//!
//! WP-09 additions (mutation-worker event-wake bridge):
//! * `pa_actions_commit` fires a fire-and-forget POST to the daemon run-now
//!   endpoint via `agent_ops::agent_ops_run_now` after the row is committed
//!   (DEC-11 — the daemon wakes immediately; poll stays the backstop).
//! * `pa_actions_pause_inner` normalises `scheduled_at` from ISO-8601 (with
//!   `T`-separator and optional timezone offset) to SQLite UTC `YYYY-MM-DD HH:MM:SS`
//!   before INSERT, so the worker's lexical `scheduled_at <= datetime('now')`
//!   predicate is correct by construction (DEC-10 / G-07).
//!
//! WP-19 slice 6: the SQL and the wire types live in
//! `server::shared::pa_actions`, which the headless daemon serves too. These
//! commands are thin delegates that add what only the desktop has — the
//! `pa-action-*` events, emitted exactly as before.

use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Emitter, State};

use super::db::PaDb;
use crate::server::shared::pa_actions as core;

pub use crate::server::shared::pa_actions::{
    PaActionCommittedEvent, PaActionDraftRow, PaActionPausedEvent, PaActionRejectedEvent,
    PaPauseDraftInput,
};

// ── Commands ────────────────────────────────────────────────────────────────

/// Pause a batch of drafts (the producer hand-off). Inserts one `awaiting` row
/// per draft atomically, then emits `pa-action-paused { batchId, count }` so the
/// shell can mount the approve gate. Backs the `host.paActionsPause` verb (WP-8).
#[tauri::command]
pub async fn pa_actions_pause(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    batch_id: String,
    action_id: String,
    drafts: Vec<PaPauseDraftInput>,
) -> Result<usize, String> {
    pa_actions_pause_inner(&app, db.inner(), batch_id, action_id, drafts).await
}

/// Shared pause logic — used by the Tauri command above and the iyke bridge
/// handler (`iyke::pa_actions`), so an MCP/CLI caller (mcp-iyke `pa_actions_pause`
/// tool, WP-8) and the FE hit the exact same insert + `pa-action-paused` emit.
/// The insert is `server::shared::pa_actions::pause` (the daemon's too).
pub async fn pa_actions_pause_inner(
    app: &AppHandle,
    db: &Arc<PaDb>,
    batch_id: String,
    action_id: String,
    drafts: Vec<PaPauseDraftInput>,
) -> Result<usize, String> {
    let count = core::pause(db, &batch_id, &action_id, &drafts).await?;
    let _ = app.emit(
        "pa-action-paused",
        PaActionPausedEvent {
            batch_id: batch_id.clone(),
            count,
        },
    );
    Ok(count)
}

/// List drafts in the gate. Defaults to the active set (`awaiting`/`edited`/
/// `committed`/`failed`); pass an explicit `status` to filter (e.g. `sent`,
/// `rejected`).
#[tauri::command]
pub async fn pa_actions_list(
    db: State<'_, Arc<PaDb>>,
    status: Option<String>,
) -> Result<Vec<PaActionDraftRow>, String> {
    core::list(&db, status.as_deref()).await
}

/// Persist operator inline edits (`{ subject?, body? }`) into `edited_json` and
/// move an `awaiting` row to `edited`. Only editable while in the gate.
#[tauri::command]
pub async fn pa_actions_update(
    db: State<'_, Arc<PaDb>>,
    draft_id: String,
    patch: Value,
) -> Result<(), String> {
    core::update(&db, &draft_id, &patch).await
}

/// Commit a draft (post-undo). Flips it to `committed`, stamps `committed_at`,
/// and emits `pa-action-committed` with the channel + payload for the external
/// mutation worker. The shell does NOT perform the send.
#[tauri::command]
pub async fn pa_actions_commit(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    draft_id: String,
) -> Result<(), String> {
    let committed = core::commit(&db, &draft_id).await?;

    let _ = app.emit("pa-action-committed", committed);

    // WP-09 / DEC-11 — event-wake: POST the daemon run-now so the mutation worker
    // fires immediately (low latency; the poll backstop catches any missed wake).
    // Fire-and-forget: spawn a detached task so the commit response returns without
    // waiting for the HTTP round-trip. Failures are silent by design — an absent /
    // stale daemon.lock (`daemon_down`) or a disabled job (`disabled` / 409) both
    // degrade gracefully; the poll catches up within 60 s.
    tokio::spawn(super::agent_ops::agent_ops_run_now(
        core::SEND_WORKER_JOB.to_string(),
    ));

    Ok(())
}

/// Re-queue a `failed` draft for another send attempt (WP-12 / G-09).
///
/// Flips `failed → committed`, resets `error_text` and `claimed_at` to NULL, and
/// stamps `committed_at = datetime('now')` so the mutation worker's claimable
/// predicate (`status='committed' AND scheduled_at <= now`) picks it up on its
/// next poll (or immediately via the event-wake POST).
///
/// Only operates on `failed` rows — idempotent guard: a row already committed or
/// sent cannot be retried again (the worker owns those states).
#[tauri::command]
pub async fn pa_actions_retry(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    draft_id: String,
) -> Result<(), String> {
    core::retry(&db, &draft_id).await?;

    // Event-wake: POST the daemon run-now so the mutation worker fires immediately.
    // Fire-and-forget — same pattern as pa_actions_commit (DEC-11).
    tokio::spawn(super::agent_ops::agent_ops_run_now(
        core::SEND_WORKER_JOB.to_string(),
    ));

    let _ = app.emit(
        "pa-action-retried",
        serde_json::json!({ "draftId": draft_id }),
    );

    Ok(())
}

/// Reject a draft. Flips it to `rejected` and emits `pa-action-rejected` so the
/// producing action can terminate cleanly.
#[tauri::command]
pub async fn pa_actions_reject(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    draft_id: String,
) -> Result<(), String> {
    core::reject(&db, &draft_id).await?;
    let _ = app.emit(
        "pa-action-rejected",
        PaActionRejectedEvent {
            draft_id: draft_id.clone(),
        },
    );
    Ok(())
}
