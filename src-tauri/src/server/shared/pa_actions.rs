//! Store for the approve-gate run-then-pause draft queue (WP-3), shared by the
//! desktop `#[tauri::command]`s in `commands::pa_actions` (and the iyke bridge
//! via `pa_actions_pause_inner`) and the daemon's `/api/rpc` arms (WP-19
//! slice 6).
//!
//! The producer side of the approve-gate seam
//! (`plans/atelier/10-approve-gate-seam.md`; behaviour `07-fe-button-renderer.md`
//! §3.5): a batch of drafts becomes `awaiting` rows in `pa_action_drafts`
//! (migrations 0050 / 0051); the operator edits (`edited`), commits
//! (`committed`) or rejects (`rejected`); the EXTERNAL mutation worker claims a
//! committed row and performs the real send. **Neither surface ever sends.**
//!
//! **What lives here.** The SQL — every statement, guard and error string the
//! commands always had — plus the wire types. What does NOT: the Tauri event
//! emits (`pa-action-paused`, `-committed`, `-retried`, `-rejected`). The
//! desktop commands emit them after calling in here, exactly as before; the
//! daemon has no event channel (the web transport's `listen()` is a no-op), so
//! its arms change the same rows and emit nothing — the approvals panel polls.
//!
//! **The wake.** Commit and retry end with a fire-and-forget POST to the local
//! agent-ops daemon's run-now endpoint for the ONE hardcoded job
//! [`SEND_WORKER_JOB`] (WP-09 / DEC-11), so the worker fires now instead of on
//! its next poll. The desktop does that through `agent_ops_run_now`; the
//! daemon through [`wake_send_worker`], rooted at its router home. Both go
//! through `agent_ops::run_now`. A general "run any job" verb is deliberately
//! not served (`agent_ops_run_now` stays allowlisted).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row as _;

use super::agent_ops;
use crate::db::PaDb;

/// The only agent-ops job the approve gate ever wakes: the mutation worker.
/// A compile-time literal — never caller input on either surface.
pub const SEND_WORKER_JOB: &str = "mutation:send-worker";

const COLS: &str = "id, batch_id, action_id, status, channel, payload_json, \
                    edited_json, scheduled_at, created_at, committed_at, sent_at, \
                    claimed_at, attempts, last_attempt_at, error_text, \
                    external_id, delivery_status, delivery_checked_at";

/// Active gate statuses — rows the approve-gate panel still surfaces. `sent` and
/// `rejected` are terminal and excluded from the default list. `failed` rows are
/// included so the operator can see errors and retry (WP-12 / G-09).
const ACTIVE_STATUSES: &str = "('awaiting', 'edited', 'committed', 'failed')";

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Normalise a `scheduledAt` ISO-8601 string (e.g. `"2026-06-09T07:00:00+01:00"` or
/// `"2026-06-09T07:00:00Z"`) to the UTC space-format SQLite expects for lexical date
/// comparison: `"YYYY-MM-DD HH:MM:SS"`.
///
/// SQLite's `datetime('now')` returns `"YYYY-MM-DD HH:MM:SS"` in UTC. If the
/// producer inserts a raw ISO string (which uses `T` + a timezone offset), the
/// predicate `scheduled_at <= datetime('now')` is a broken lexical compare that
/// either fires immediately (offset < `T`) or never fires (offset > space). DEC-10 /
/// G-07 requires the shell to normalise at pause-time so the worker can trust the
/// column.
///
/// Behaviour:
/// * `None` → `None` (no scheduled time).
/// * Already in the space-format (`"YYYY-MM-DD HH:MM:SS"`) → returned as-is.
/// * Valid RFC 3339 / ISO 8601 with offset or `Z` → converted to UTC, formatted as
///   `"YYYY-MM-DD HH:MM:SS"`.
/// * Unparseable → original string returned unchanged (logged; the row inserts; the
///   worker's defensive parse can handle degraded inputs rather than blocking the
///   commit with a hard error).
fn normalize_scheduled_at(iso: Option<String>) -> Option<String> {
    let s = match iso {
        None => return None,
        Some(s) if s.is_empty() => return None,
        Some(s) => s,
    };

    // Fast-path: already in `"YYYY-MM-DD HH:MM:SS"` space-format (no T, no offset).
    // The SQLite datetime format is exactly 19 chars: "2026-06-09 07:00:00".
    if s.len() == 19 && !s.contains('T') && !s.contains('+') {
        return Some(s);
    }

    // Parse as RFC 3339 and convert to UTC.
    match chrono::DateTime::parse_from_rfc3339(&s) {
        Ok(dt) => {
            use chrono::TimeZone;
            let utc = chrono::Utc.from_utc_datetime(&dt.naive_utc());
            Some(utc.format("%Y-%m-%d %H:%M:%S").to_string())
        }
        Err(e) => {
            // Non-fatal: log and pass through. The row still inserts; the worker
            // performs its own defensive parse against malformed values.
            tracing::warn!(
                scheduled_at = %s,
                error = %e,
                "pa_actions_pause: failed to normalise scheduled_at — inserting raw"
            );
            Some(s)
        }
    }
}

// ── Wire shapes ─────────────────────────────────────────────────────────────

/// One draft row as returned to the FE. `payload_json` / `edited_json` are
/// opaque JSON the FE parses (DraftItem + ApproveGateMeta) to derive a
/// `PausedDraft`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaActionDraftRow {
    pub id: String,
    #[serde(rename = "batchId")]
    pub batch_id: String,
    #[serde(rename = "actionId")]
    pub action_id: String,
    /// `awaiting` | `edited` | `committed` | `sending` | `sent` | `failed` | `rejected`.
    pub status: String,
    pub channel: String,
    #[serde(rename = "payloadJson")]
    pub payload_json: String,
    #[serde(rename = "editedJson")]
    pub edited_json: Option<String>,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "committedAt")]
    pub committed_at: Option<String>,
    #[serde(rename = "sentAt")]
    pub sent_at: Option<String>,
    // ── 0051 mutation-worker columns ─────────────────────────────────────────
    #[serde(rename = "claimedAt")]
    pub claimed_at: Option<String>,
    pub attempts: i64,
    #[serde(rename = "lastAttemptAt")]
    pub last_attempt_at: Option<String>,
    #[serde(rename = "errorText")]
    pub error_text: Option<String>,
    #[serde(rename = "externalId")]
    pub external_id: Option<String>,
    #[serde(rename = "deliveryStatus")]
    pub delivery_status: Option<String>,
    #[serde(rename = "deliveryCheckedAt")]
    pub delivery_checked_at: Option<String>,
}

/// Map a raw `SqliteRow` (from `query(COLS).fetch_*`) into `PaActionDraftRow`.
///
/// We use manual column indexing rather than a tuple `FromRow` impl because the
/// 18-column COLS projection exceeds sqlx's 16-element tuple `FromRow` limit.
/// Column order must match the `COLS` constant exactly.
#[rustfmt::skip]
fn row_to_draft(r: sqlx::sqlite::SqliteRow) -> PaActionDraftRow {
    PaActionDraftRow {
        id:                 r.get(0),
        batch_id:           r.get(1),
        action_id:          r.get(2),
        status:             r.get(3),
        channel:            r.get(4),
        payload_json:       r.get(5),
        edited_json:        r.get(6),
        scheduled_at:       r.get(7),
        created_at:         r.get(8),
        committed_at:       r.get(9),
        sent_at:            r.get(10),
        claimed_at:         r.get(11),
        attempts:           r.get::<Option<i64>, _>(12).unwrap_or(0),
        last_attempt_at:    r.get(13),
        error_text:         r.get(14),
        external_id:        r.get(15),
        delivery_status:    r.get(16),
        delivery_checked_at: r.get(17),
    }
}

/// One draft in a `pa_actions_pause` batch. `payload` (DraftItem + ApproveGateMeta)
/// is stored verbatim; the FE parses it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaPauseDraftInput {
    pub id: String,
    pub channel: String,
    #[serde(default)]
    pub scheduled_at: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaActionPausedEvent {
    #[serde(rename = "batchId")]
    pub batch_id: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaActionCommittedEvent {
    #[serde(rename = "draftId")]
    pub draft_id: String,
    pub channel: String,
    /// The DraftItem + ApproveGateMeta the action produced (worker sends from this).
    #[serde(rename = "payloadJson")]
    pub payload_json: String,
    /// Operator subject/body overrides, if any.
    #[serde(rename = "editedJson")]
    pub edited_json: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaActionRejectedEvent {
    #[serde(rename = "draftId")]
    pub draft_id: String,
}

// ── Operations ──────────────────────────────────────────────────────────────

/// Insert one `awaiting` row per draft, atomically. Returns the count. The
/// caller emits `pa-action-paused` (desktop) or nothing (daemon).
pub async fn pause(
    db: &PaDb,
    batch_id: &str,
    action_id: &str,
    drafts: &[PaPauseDraftInput],
) -> Result<usize, String> {
    if drafts.is_empty() {
        // §3.5 invariant: the gate must never surface an empty list.
        return Err("pa_actions_pause: drafts cannot be empty".into());
    }
    let pool = db.ensure_pool().await?;
    let count = drafts.len();

    let mut tx = pool.begin().await.map_err(|e| format!("begin tx: {e}"))?;
    for d in drafts {
        let payload = serde_json::to_string(&d.payload)
            .map_err(|e| format!("serialize draft payload: {e}"))?;
        sqlx::query(
            "INSERT INTO pa_action_drafts \
             (id, batch_id, action_id, status, channel, payload_json, scheduled_at) \
             VALUES (?, ?, ?, 'awaiting', ?, ?, ?)",
        )
        .bind(&d.id)
        .bind(batch_id)
        .bind(action_id)
        .bind(&d.channel)
        .bind(payload)
        .bind(normalize_scheduled_at(d.scheduled_at.clone()))
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("insert draft {}: {e}", d.id))?;
    }
    tx.commit().await.map_err(|e| format!("commit tx: {e}"))?;
    Ok(count)
}

/// Drafts in the gate: the active set (`awaiting`/`edited`/`committed`/
/// `failed`) by default, or exactly `status` when given.
pub async fn list(db: &PaDb, status: Option<&str>) -> Result<Vec<PaActionDraftRow>, String> {
    let pool = db.ensure_pool().await?;
    let rows = if let Some(s) = status {
        sqlx::query(&format!(
            "SELECT {COLS} FROM pa_action_drafts WHERE status = ? ORDER BY created_at ASC"
        ))
        .bind(s)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query(&format!(
            "SELECT {COLS} FROM pa_action_drafts \
             WHERE status IN {ACTIVE_STATUSES} ORDER BY created_at ASC"
        ))
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| format!("list drafts: {e}"))?;
    Ok(rows.into_iter().map(row_to_draft).collect())
}

/// Store operator edits in `edited_json`; `awaiting` → `edited`. Only while
/// the row is still in the gate.
pub async fn update(db: &PaDb, draft_id: &str, patch: &Value) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let edited = serde_json::to_string(patch).map_err(|e| format!("serialize patch: {e}"))?;
    let affected = sqlx::query(
        "UPDATE pa_action_drafts \
         SET edited_json = ?, \
             status = CASE WHEN status = 'awaiting' THEN 'edited' ELSE status END \
         WHERE id = ? AND status IN ('awaiting', 'edited')",
    )
    .bind(edited)
    .bind(draft_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("update draft {draft_id}: {e}"))?
    .rows_affected();
    if affected == 0 {
        return Err(format!("draft {draft_id} not found or not editable"));
    }
    Ok(())
}

/// `awaiting` / `edited` → `committed`, stamping `committed_at`. Returns what
/// the desktop's `pa-action-committed` event carries. The caller wakes the
/// worker.
pub async fn commit(db: &PaDb, draft_id: &str) -> Result<PaActionCommittedEvent, String> {
    let pool = db.ensure_pool().await?;
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT channel, payload_json, edited_json FROM pa_action_drafts \
         WHERE id = ? AND status IN ('awaiting', 'edited')",
    )
    .bind(draft_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("read draft {draft_id}: {e}"))?;
    let (channel, payload_json, edited_json) =
        row.ok_or_else(|| format!("draft {draft_id} not found or not committable"))?;

    sqlx::query(
        "UPDATE pa_action_drafts SET status = 'committed', committed_at = datetime('now') \
         WHERE id = ?",
    )
    .bind(draft_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("commit draft {draft_id}: {e}"))?;

    Ok(PaActionCommittedEvent {
        draft_id: draft_id.to_string(),
        channel,
        payload_json,
        edited_json,
    })
}

/// Re-queue a `failed` draft (WP-12 / G-09): `failed` → `committed`,
/// `committed_at` = now, `claimed_at` / `error_text` cleared, so the worker's
/// claimable predicate picks it up. Only `failed` rows: the worker owns the
/// others. The caller wakes the worker.
pub async fn retry(db: &PaDb, draft_id: &str) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let affected = sqlx::query(
        "UPDATE pa_action_drafts \
         SET status = 'committed', \
             committed_at = datetime('now'), \
             claimed_at = NULL, \
             error_text = NULL \
         WHERE id = ? AND status = 'failed'",
    )
    .bind(draft_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("retry draft {draft_id}: {e}"))?
    .rows_affected();

    if affected == 0 {
        return Err(format!("draft {draft_id} not found or not in failed state"));
    }
    Ok(())
}

/// → `rejected`, from any state but `sending` (claimed by the worker right
/// now) and the terminal ones.
pub async fn reject(db: &PaDb, draft_id: &str) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let affected = sqlx::query(
        // `failed` is here deliberately. The panel renders Reject on failed rows —
        // a send that will not succeed is precisely the thing an operator wants to
        // discard — but this clause used to exclude it, so the button was offered
        // on rows the command would refuse. Combined with the view-model never
        // carrying the row id (see pausedDraftFromRow), rejecting a failed draft
        // was broken twice over and failed silently: the panel removed the row
        // optimistically while the DB was never written.
        // `sending` stays out: that row is claimed by the worker right now.
        "UPDATE pa_action_drafts SET status = 'rejected' \
         WHERE id = ? AND status IN ('awaiting', 'edited', 'committed', 'failed')",
    )
    .bind(draft_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("reject draft {draft_id}: {e}"))?
    .rows_affected();
    if affected == 0 {
        return Err(format!("draft {draft_id} not found or already terminal"));
    }
    Ok(())
}

/// The daemon's event-wake after a commit / retry: POST the local agent-ops
/// daemon's run-now for [`SEND_WORKER_JOB`] — and only that job — resolving
/// `~/.agent-ops/daemon.lock` under `home` (the router home seam). Spawned and
/// detached by the caller, so the RPC answer never waits on or depends on it;
/// an absent lock / home or an unreachable daemon resolves `daemon_down` and
/// the worker's poll catches up. The handle is returned for tests.
pub fn wake_send_worker(home: Option<PathBuf>) -> tokio::task::JoinHandle<Result<Value, String>> {
    tokio::spawn(async move { agent_ops::run_now(home.as_deref(), SEND_WORKER_JOB).await })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_at_is_normalised_to_sqlite_utc() {
        assert_eq!(normalize_scheduled_at(None), None);
        assert_eq!(normalize_scheduled_at(Some(String::new())), None);
        assert_eq!(
            normalize_scheduled_at(Some("2026-06-09 07:00:00".into())).as_deref(),
            Some("2026-06-09 07:00:00")
        );
        assert_eq!(
            normalize_scheduled_at(Some("2026-06-09T07:00:00+01:00".into())).as_deref(),
            Some("2026-06-09 06:00:00")
        );
        assert_eq!(
            normalize_scheduled_at(Some("2026-06-09T07:00:00Z".into())).as_deref(),
            Some("2026-06-09 07:00:00")
        );
        assert_eq!(
            normalize_scheduled_at(Some("tomorrow".into())).as_deref(),
            Some("tomorrow")
        );
    }

    /// No lock under the home → `daemon_down` from the lock read: the POST is
    /// never attempted (its failure would read "trigger POST failed").
    #[tokio::test]
    async fn the_wake_without_a_lock_makes_no_call() {
        let tmp = tempfile::tempdir().unwrap();
        let v = wake_send_worker(Some(tmp.path().to_path_buf()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["code"], "daemon_down");
        let e = v["error"].as_str().unwrap();
        assert!(e.starts_with("read daemon.lock"), "{e}");

        let v = wake_send_worker(None).await.unwrap().unwrap();
        assert_eq!(v["code"], "daemon_down");
        assert_eq!(v["error"], "home directory not found");
    }
}
