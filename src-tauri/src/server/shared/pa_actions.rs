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
use super::social_draft;
use crate::db::PaDb;

/// The only agent-ops job the approve gate ever wakes: the mutation worker.
/// A compile-time literal — never caller input on either surface.
pub const SEND_WORKER_JOB: &str = "mutation:send-worker";

const COLS: &str = "id, batch_id, action_id, status, channel, payload_json, \
                    edited_json, scheduled_at, created_at, committed_at, sent_at, \
                    claimed_at, attempts, last_attempt_at, error_text, \
                    external_id, delivery_status, delivery_checked_at, reject_reason";

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
    // ── 0074 ─────────────────────────────────────────────────────────────────
    /// Why the operator rejected the draft (`pa_actions_reject { reason }`);
    /// `None` when none was given or the row was never rejected.
    #[serde(rename = "rejectReason")]
    pub reject_reason: Option<String>,
}

/// Map a raw `SqliteRow` (from `query(COLS).fetch_*`) into `PaActionDraftRow`.
///
/// We use manual column indexing rather than a tuple `FromRow` impl because the
/// 19-column COLS projection exceeds sqlx's 16-element tuple `FromRow` limit.
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
        reject_reason:      r.get(18),
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
    /// The stored reject reason, when one was given (left out otherwise, so a
    /// reasonless reject emits exactly the payload it always did).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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

/// What `pa_actions_update` answers (after its `pa_actions_update: ` prefix) when
/// the request has no `patch`. The child checks this before it reads the DB, and
/// the box broker answers the same words itself for a draft the viewer may not
/// see, so a request without a patch cannot tell a hidden row from a missing one.
pub const PATCH_REQUIRED: &str = "`patch` is required";

/// What `update` answers for a draft that is missing or not editable. Public
/// because the box broker answers a draft the viewer may not see with exactly
/// these words (`broker::approvals`), so a hidden row reads as a missing one.
pub fn update_refusal(draft_id: &str) -> String {
    format!("draft {draft_id} not found or not editable")
}

/// What `reject` answers for a draft that is missing or already terminal (see
/// [`update_refusal`]).
pub fn reject_refusal(draft_id: &str) -> String {
    format!("draft {draft_id} not found or already terminal")
}

/// What `pa_actions_update` answers (after its prefix) for a `patch` that is
/// not a JSON object: an edit is merged key by key, so there is nothing to
/// merge from a string, a list or `null`.
pub const PATCH_NOT_OBJECT: &str = "`patch` must be an object";

/// What `pa_actions_update` answers (after its prefix) for a `patch.scheduledIso`
/// that is neither an ISO-8601 string nor `null`.
pub const PATCH_SCHEDULE_INVALID: &str = "`patch.scheduledIso` must be a string or null";

/// Most attachments one edit may carry (G-CONTRACT r2, D-16).
pub const MAX_ATTACHMENTS: usize = 20;

/// What `pa_actions_update` answers (after its prefix) for a `patch.attachments`
/// that is not the contract's list.
pub const PATCH_ATTACHMENTS_INVALID: &str =
    "`patch.attachments` must be an array of at most 20 { url: string (https), name?: string }";

/// `patch.attachments` (D-16): `Array<{ url: string /* https */, name?: string }>`,
/// at most [`MAX_ATTACHMENTS`]. Each entry is an object holding exactly `url`
/// (an absolute `https:` URL) and optionally `name` (a string); nothing else.
fn attachments_ok(v: &Value) -> bool {
    let Some(list) = v.as_array() else {
        return false;
    };
    list.len() <= MAX_ATTACHMENTS
        && list.iter().all(|a| {
            let Some(o) = a.as_object() else {
                return false;
            };
            let url_ok = o
                .get("url")
                .and_then(Value::as_str)
                .and_then(|u| url::Url::parse(u).ok())
                .is_some_and(|u| u.scheme() == "https");
            let name_ok = o.get("name").is_none_or(Value::is_string);
            url_ok && name_ok && o.keys().all(|k| k == "url" || k == "name")
        })
}

/// Everything `update` checks about the request itself, before it reads the
/// DB: `None` when `patch` is acceptable. Public because the box broker runs the
/// same checks, in the same place, before it looks a draft up for a share
/// viewer (`broker::approvals::gate`), so a malformed patch reads the same for a
/// hidden row as for a missing one. Checks that depend on the row (a
/// source-bound social draft, [`social_draft::plan_source_edit`]) run in
/// `update`, after the lookup, on both surfaces alike.
pub fn patch_error(patch: Option<&Value>) -> Option<&'static str> {
    let Some(patch) = patch else {
        return Some(PATCH_REQUIRED);
    };
    let Some(obj) = patch.as_object() else {
        return Some(PATCH_NOT_OBJECT);
    };
    match obj.get("scheduledIso") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(_) => return Some(PATCH_SCHEDULE_INVALID),
    }
    match obj.get("attachments") {
        Some(v) if !attachments_ok(v) => Some(PATCH_ATTACHMENTS_INVALID),
        _ => None,
    }
}

/// The edit after `patch` is shallow-merged onto what `edited_json` already
/// holds: each top-level key of `patch` replaces the stored one, every other
/// stored key stays. A stored edit that is not a JSON object (a pre-merge row
/// or a damaged one) is replaced, as every update did before merging.
fn merge_edit(stored: Option<&str>, patch: &serde_json::Map<String, Value>) -> Value {
    let mut merged = stored
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    for (k, v) in patch {
        merged.insert(k.clone(), v.clone());
    }
    Value::Object(merged)
}

/// How many times `update` / `commit` re-read a row whose `edited_json` changed
/// between their read and their guarded write before giving up.
const RACE_RETRIES: usize = 3;

/// Shallow-merge operator edits into `edited_json`; `awaiting` → `edited`.
/// Only while the row is still in the gate. A `scheduledIso` key also rewrites
/// `scheduled_at` (normalised to SQLite UTC like at pause time; `null` clears
/// it). The write is guarded on the `edited_json` it merged onto, so two edits
/// racing each other cannot drop one another's keys.
///
/// **A source-bound social draft (`item.social`) is edited at its source**
/// (D-15, G-CONTRACT r2): `edited_json` is not written; the patch is applied to
/// `item.social` and the derived Buffer fields ([`social_draft::plan_source_edit`]),
/// and the draft and its `social_queue` row are written in one transaction
/// ([`update_social_source`]). A refusal reads `social_refused: <code>: …`.
pub async fn update(db: &PaDb, draft_id: &str, patch: &Value) -> Result<(), String> {
    if let Some(e) = patch_error(Some(patch)) {
        return Err(e.to_string());
    }
    let obj = patch.as_object().expect("patch_error admits only objects");
    let schedule = obj
        .get("scheduledIso")
        .map(|v| normalize_scheduled_at(v.as_str().map(str::to_string)));
    let pool = db.ensure_pool().await?;

    for _ in 0..RACE_RETRIES {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT payload_json, edited_json FROM pa_action_drafts \
             WHERE id = ? AND status IN ('awaiting', 'edited')",
        )
        .bind(draft_id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| format!("read draft {draft_id}: {e}"))?;
        let Some((payload_json, stored)) = row else {
            return Err(update_refusal(draft_id));
        };
        if social_draft::is_source_bound(&payload_json) {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let Some(plan) =
                social_draft::plan_source_edit(&payload_json, stored.as_deref(), obj, now_ms)?
            else {
                return Ok(());
            };
            if update_social_source(&pool, draft_id, &payload_json, stored.as_deref(), &plan)
                .await?
            {
                return Ok(());
            }
            continue;
        }
        let merged = serde_json::to_string(&merge_edit(stored.as_deref(), obj))
            .map_err(|e| format!("serialize patch: {e}"))?;
        let affected = sqlx::query(
            "UPDATE pa_action_drafts \
             SET edited_json = ?, \
                 status = CASE WHEN status = 'awaiting' THEN 'edited' ELSE status END, \
                 scheduled_at = CASE WHEN ? THEN ? ELSE scheduled_at END \
             WHERE id = ? AND status IN ('awaiting', 'edited') AND edited_json IS ?",
        )
        .bind(merged)
        .bind(schedule.is_some())
        .bind(schedule.clone().flatten())
        .bind(draft_id)
        .bind(stored)
        .execute(&pool)
        .await
        .map_err(|e| format!("update draft {draft_id}: {e}"))?
        .rows_affected();
        if affected == 1 {
            return Ok(());
        }
        // Zero rows: the row left the gate, or another edit landed in between.
        // Re-read: the first answers `update_refusal`, the second merges again.
    }
    Err(format!(
        "update draft {draft_id}: it kept changing while being saved; try again"
    ))
}

/// Write a planned source edit: the draft's `payload_json` (`awaiting` →
/// `edited`, `edited_json` and `scheduled_at` untouched — Buffer holds a social
/// post's schedule) and its `social_queue` row (`content`, `media_url`,
/// `scheduled_for`), in ONE transaction, so the sender's link check
/// (`social_queue` row == `item.social`) holds before and after.
///
/// The draft write is guarded on the `payload_json` and `edited_json` the plan
/// was made from: `Ok(false)` (rolled back) when either changed in between, for
/// the caller to plan again. The queue row must still equal `item.social` as it
/// was (`social_queue_changed`) and must exist (`social_queue_missing`, the way a
/// queued post is cancelled); either refuses the edit and writes nothing.
async fn update_social_source(
    pool: &sqlx::SqlitePool,
    draft_id: &str,
    payload_json: &str,
    edited_json: Option<&str>,
    plan: &social_draft::SourceEdit,
) -> Result<bool, String> {
    let mut tx = pool.begin().await.map_err(|e| format!("begin tx: {e}"))?;
    // The draft first: its write takes the database's write lock, so the queue
    // row read below cannot change before this transaction commits.
    let flipped = sqlx::query(
        "UPDATE pa_action_drafts \
         SET payload_json = ?, \
             status = CASE WHEN status = 'awaiting' THEN 'edited' ELSE status END \
         WHERE id = ? AND status IN ('awaiting', 'edited') \
           AND payload_json = ? AND edited_json IS ?",
    )
    .bind(&plan.payload_json)
    .bind(draft_id)
    .bind(payload_json)
    .bind(edited_json)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("update draft {draft_id}: {e}"))?
    .rows_affected();
    if flipped != 1 {
        tx.rollback().await.map_err(|e| format!("rollback: {e}"))?;
        return Ok(false);
    }
    let queue: Option<(String, String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT platform, account, content, media_url, scheduled_for \
         FROM social_queue WHERE id = ?",
    )
    .bind(&plan.queue_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| format!("read social_queue row {}: {e}", plan.queue_id))?;
    let refuse = |code: &str, msg: String| {
        social_draft::refusal(&[social_draft::Problem {
            code: code.into(),
            message: msg,
        }])
    };
    let Some((platform, account, content, media_url, scheduled_for)) = queue else {
        tx.rollback().await.map_err(|e| format!("rollback: {e}"))?;
        return Err(refuse(
            "social_queue_missing",
            format!(
                "social_queue row {} no longer exists (cancelled); nothing was changed",
                plan.queue_id
            ),
        ));
    };
    let row_post = social_draft::SocialPost {
        platform,
        account,
        content,
        media_url,
        scheduled_for,
    };
    if row_post != plan.before {
        tx.rollback().await.map_err(|e| format!("rollback: {e}"))?;
        return Err(refuse(
            "social_queue_changed",
            format!(
                "social_queue row {} was changed after it was queued and no longer matches this draft; \
                 nothing was changed",
                plan.queue_id
            ),
        ));
    }
    sqlx::query(
        "UPDATE social_queue SET content = ?, media_url = ?, scheduled_for = ? WHERE id = ?",
    )
    .bind(&plan.after.content)
    .bind(&plan.after.media_url)
    .bind(&plan.after.scheduled_for)
    .bind(&plan.queue_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("update social_queue row {}: {e}", plan.queue_id))?;
    tx.commit().await.map_err(|e| format!("commit tx: {e}"))?;
    Ok(true)
}

/// The edit keys `commit` folds into the payload's `item`: the patch keys of
/// `host.paActions.update` (G-CONTRACT, + `attachments` in r2), which are
/// DraftItem field names (or the producer's extra `item` fields — `hashtags`,
/// `preheader`, `subjectB`, `attachments`).
pub const FOLD_KEYS: [&str; 9] = [
    "subject",
    "body",
    "subjectB",
    "preheader",
    "firstComment",
    "hashtags",
    "media",
    "scheduledIso",
    "attachments",
];

/// `payload_json` with the operator's edit folded into its `item` (D-08): the
/// external send worker sends from `payload_json.item` and honours only
/// `edited_json.subject/body`, so without the fold an edited image list,
/// hashtag set or first comment would never reach the send. Each [`FOLD_KEYS`]
/// key present in the edit replaces the item's; `media` is merged one level
/// deep (an edit carries `{kind, imageUrls}`, a thread's `thread` posts stay);
/// a `null` value is skipped except for `scheduledIso`, where it means
/// "unscheduled". The fold goes into `item` when it is an object, else into
/// the payload itself (the shape the approvals hash also reads, contract §3.2).
///
/// `None` when nothing changes: no edit, an edit with none of these keys, or a
/// payload / edit that is not a JSON object (left for the worker to refuse,
/// exactly as before the fold existed).
///
/// **Source-bound social drafts are never folded.** An item carrying `social`
/// (a post built from a `social_queue` row) has its adapter fields —
/// `body`, `firstComment`, `media`, `scheduledIso`, … — re-derived from
/// `item.social` by the sender, which refuses the row (`social_mismatch`) when
/// they disagree. Folding an edit into them would make an approved post
/// unsendable. Since G-CONTRACT r2 `update` edits such a draft at its source
/// (`item.social` + the `social_queue` row) and writes no `edited_json`, so
/// there is nothing to fold.
pub fn fold_edits(payload_json: &str, edited_json: Option<&str>) -> Option<String> {
    let edited: Value = serde_json::from_str(edited_json.filter(|s| !s.is_empty())?).ok()?;
    let edited = edited.as_object()?;
    let mut payload: Value = serde_json::from_str(payload_json).ok()?;
    let root = payload.as_object_mut()?;
    let target = if root.get("item").is_some_and(Value::is_object) {
        root.get_mut("item").and_then(Value::as_object_mut)?
    } else {
        root
    };
    if target.get("social").is_some_and(Value::is_object) {
        return None;
    }
    let mut changed = false;
    for key in FOLD_KEYS {
        let Some(v) = edited.get(key) else { continue };
        if v.is_null() && key != "scheduledIso" {
            continue;
        }
        let next = match (key, target.get(key), v) {
            ("media", Some(Value::Object(old)), Value::Object(new)) => {
                let mut m = old.clone();
                for (k, x) in new {
                    m.insert(k.clone(), x.clone());
                }
                Value::Object(m)
            }
            _ => v.clone(),
        };
        if target.get(key) != Some(&next) {
            target.insert(key.to_string(), next);
            changed = true;
        }
    }
    if !changed {
        return None;
    }
    serde_json::to_string(&payload).ok()
}

/// `awaiting` / `edited` → `committed`, stamping `committed_at`, with the
/// operator's edit folded into `payload_json.item` ([`fold_edits`]) in the same
/// guarded write, so the worker sends what was approved. Returns what the
/// desktop's `pa-action-committed` event carries (the folded payload). The
/// caller wakes the worker.
///
/// A share approval on the box never comes here: the broker records it in the
/// sender's ledger and forwards nothing (`broker::approvals`), and the sender
/// sends the hashed row as it stands — folding there would change the bytes
/// the approval is bound to.
pub async fn commit(db: &PaDb, draft_id: &str) -> Result<PaActionCommittedEvent, String> {
    let pool = db.ensure_pool().await?;
    for _ in 0..RACE_RETRIES {
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
        let folded = fold_edits(&payload_json, edited_json.as_deref()).unwrap_or(payload_json);

        // Guarded like the read: a row claimed in between (by the sender, on a
        // ledger approval) must not be flipped back to `committed`; and an edit
        // saved in between must not be committed without being folded.
        let flipped = sqlx::query(
            "UPDATE pa_action_drafts \
             SET status = 'committed', committed_at = datetime('now'), payload_json = ? \
             WHERE id = ? AND status IN ('awaiting', 'edited') AND edited_json IS ?",
        )
        .bind(&folded)
        .bind(draft_id)
        .bind(&edited_json)
        .execute(&pool)
        .await
        .map_err(|e| format!("commit draft {draft_id}: {e}"))?
        .rows_affected();
        if flipped == 1 {
            return Ok(PaActionCommittedEvent {
                draft_id: draft_id.to_string(),
                channel,
                payload_json: folded,
                edited_json,
            });
        }
    }
    Err(format!("draft {draft_id} not found or not committable"))
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

/// Longest reject reason stored, in characters; a longer one is cut there.
pub const REJECT_REASON_MAX: usize = 2000;

/// A reject reason as stored: trimmed, at most [`REJECT_REASON_MAX`]
/// characters, `None` when nothing is left. Never an error, so a reason the
/// store would not keep reads the same for every draft id.
pub fn clean_reject_reason(reason: Option<&str>) -> Option<String> {
    let t = reason?.trim();
    if t.is_empty() {
        return None;
    }
    Some(
        t.chars()
            .take(REJECT_REASON_MAX)
            .collect::<String>()
            .trim_end()
            .to_string(),
    )
}

/// → `rejected`, from any state but `sending` (claimed by the worker right
/// now) and the terminal ones, storing `reason` ([`clean_reject_reason`]) in
/// `reject_reason`. Returns the reason as stored.
pub async fn reject(
    db: &PaDb,
    draft_id: &str,
    reason: Option<&str>,
) -> Result<Option<String>, String> {
    let reason = clean_reject_reason(reason);
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
        "UPDATE pa_action_drafts SET status = 'rejected', reject_reason = ? \
         WHERE id = ? AND status IN ('awaiting', 'edited', 'committed', 'failed')",
    )
    .bind(&reason)
    .bind(draft_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("reject draft {draft_id}: {e}"))?
    .rows_affected();
    if affected == 0 {
        return Err(reject_refusal(draft_id));
    }
    Ok(reason)
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

    // ── store: merge-update, schedule, fold-on-commit, reject reason ────────

    async fn db_with(drafts: &[(&str, Value)]) -> (PaDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db = PaDb::new(tmp.path().join("pa.db"));
        let inputs: Vec<PaPauseDraftInput> = drafts
            .iter()
            .map(|(id, payload)| PaPauseDraftInput {
                id: id.to_string(),
                channel: "buffer".into(),
                scheduled_at: Some("2026-11-03T09:00:00Z".into()),
                payload: payload.clone(),
            })
            .collect();
        pause(&db, "b1", "act", &inputs).await.unwrap();
        (db, tmp)
    }

    async fn row(db: &PaDb, id: &str, status: Option<&str>) -> PaActionDraftRow {
        list(db, status)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
    }

    fn social_payload() -> Value {
        json!({
            "item": {
                "id": "d1", "subject": "S", "body": "B", "channel": "buffer",
                "scheduledIso": "2026-11-03T09:00:00Z",
                "media": {"kind": "thread", "imageUrls": [], "thread": [{"text": "1"}, {"text": "2"}]},
                "hashtags": ["#old"]
            },
            "meta": {"actionId": "act"}
        })
    }

    use serde_json::json;

    #[test]
    fn patch_error_names_what_is_wrong_with_the_patch() {
        assert_eq!(patch_error(None), Some(PATCH_REQUIRED));
        for bad in [json!(null), json!("x"), json!([1]), json!(3)] {
            assert_eq!(patch_error(Some(&bad)), Some(PATCH_NOT_OBJECT), "{bad}");
        }
        assert_eq!(
            patch_error(Some(&json!({"scheduledIso": 7}))),
            Some(PATCH_SCHEDULE_INVALID)
        );
        for ok in [
            json!({}),
            json!({"scheduledIso": null}),
            json!({"scheduledIso": "2026-11-03T09:00:00Z", "body": "x"}),
        ] {
            assert_eq!(patch_error(Some(&ok)), None, "{ok}");
        }
    }

    #[tokio::test]
    async fn update_shallow_merges_into_the_stored_edit() {
        let (db, _t) = db_with(&[("d1", social_payload())]).await;
        update(&db, "d1", &json!({"subject": "S1", "hashtags": ["#a"]}))
            .await
            .unwrap();
        update(&db, "d1", &json!({"body": "B2", "hashtags": ["#b", "#c"]}))
            .await
            .unwrap();
        let r = row(&db, "d1", None).await;
        assert_eq!(r.status, "edited");
        let edited: Value = serde_json::from_str(r.edited_json.as_deref().unwrap()).unwrap();
        // The first edit's subject survives the second; a re-sent key is replaced whole.
        assert_eq!(
            edited,
            json!({"subject": "S1", "body": "B2", "hashtags": ["#b", "#c"]})
        );
        // Not an object: refused, nothing written.
        assert_eq!(
            update(&db, "d1", &json!(null)).await.unwrap_err(),
            PATCH_NOT_OBJECT
        );
        assert_eq!(row(&db, "d1", None).await.edited_json, r.edited_json);
        assert_eq!(
            update(&db, "nope", &json!({"body": "x"}))
                .await
                .unwrap_err(),
            update_refusal("nope")
        );
    }

    #[tokio::test]
    async fn a_pre_merge_edit_that_is_not_an_object_is_replaced() {
        let (db, _t) = db_with(&[("d1", social_payload())]).await;
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query("UPDATE pa_action_drafts SET edited_json = 'null', status = 'edited'")
            .execute(&pool)
            .await
            .unwrap();
        update(&db, "d1", &json!({"body": "x"})).await.unwrap();
        assert_eq!(
            row(&db, "d1", None).await.edited_json.as_deref(),
            Some(r#"{"body":"x"}"#)
        );
    }

    #[tokio::test]
    async fn scheduled_iso_rewrites_scheduled_at_and_null_clears_it() {
        let (db, _t) = db_with(&[("d1", social_payload())]).await;
        assert_eq!(
            row(&db, "d1", None).await.scheduled_at.as_deref(),
            Some("2026-11-03 09:00:00")
        );
        update(
            &db,
            "d1",
            &json!({"scheduledIso": "2026-11-04T10:30:00+01:00"}),
        )
        .await
        .unwrap();
        assert_eq!(
            row(&db, "d1", None).await.scheduled_at.as_deref(),
            Some("2026-11-04 09:30:00")
        );
        // An edit without the key leaves the schedule alone.
        update(&db, "d1", &json!({"body": "x"})).await.unwrap();
        assert_eq!(
            row(&db, "d1", None).await.scheduled_at.as_deref(),
            Some("2026-11-04 09:30:00")
        );
        update(&db, "d1", &json!({"scheduledIso": null}))
            .await
            .unwrap();
        assert_eq!(row(&db, "d1", None).await.scheduled_at, None);
        assert_eq!(
            update(&db, "d1", &json!({"scheduledIso": 5}))
                .await
                .unwrap_err(),
            PATCH_SCHEDULE_INVALID
        );
    }

    #[tokio::test]
    async fn commit_folds_the_edit_into_the_payload_item() {
        let (db, _t) = db_with(&[("d1", social_payload())]).await;
        update(
            &db,
            "d1",
            &json!({
                "subject": "S2", "body": "B2", "subjectB": "SB", "preheader": "P",
                "firstComment": "FC", "hashtags": ["#new"],
                "media": {"kind": "carousel", "imageUrls": ["https://x/1.png", "https://x/2.png"]},
                "scheduledIso": "2026-11-05T08:00:00Z",
                "unknownKey": 1
            }),
        )
        .await
        .unwrap();
        let ev = commit(&db, "d1").await.unwrap();
        let r = row(&db, "d1", Some("committed")).await;
        assert_eq!(ev.payload_json, r.payload_json);
        let p: Value = serde_json::from_str(&r.payload_json).unwrap();
        let item = &p["item"];
        assert_eq!(item["subject"], "S2");
        assert_eq!(item["body"], "B2");
        assert_eq!(item["subjectB"], "SB");
        assert_eq!(item["preheader"], "P");
        assert_eq!(item["firstComment"], "FC");
        assert_eq!(item["hashtags"], json!(["#new"]));
        assert_eq!(item["scheduledIso"], "2026-11-05T08:00:00Z");
        // `media` merges one level: the edit's kind + images, the thread posts kept.
        assert_eq!(item["media"]["kind"], "carousel");
        assert_eq!(
            item["media"]["imageUrls"],
            json!(["https://x/1.png", "https://x/2.png"])
        );
        assert_eq!(
            item["media"]["thread"],
            json!([{"text": "1"}, {"text": "2"}])
        );
        // Keys outside the fold set stay in the edit only; siblings of `item` stay.
        assert!(item.get("unknownKey").is_none());
        assert_eq!(p["meta"], json!({"actionId": "act"}));
        // The edit is kept: the worker still reads its subject/body.
        assert_eq!(ev.edited_json, r.edited_json);
        assert_eq!(r.scheduled_at.as_deref(), Some("2026-11-05 08:00:00"));
    }

    #[tokio::test]
    async fn commit_without_an_edit_leaves_the_payload_bytes_alone() {
        let raw = r#"{"meta":{"z":1},"item":{"subject":"S","body":"B"}}"#;
        let payload: Value = serde_json::from_str(raw).unwrap();
        let (db, _t) = db_with(&[("d1", payload)]).await;
        let before = row(&db, "d1", None).await.payload_json;
        let ev = commit(&db, "d1").await.unwrap();
        assert_eq!(ev.payload_json, before);
        assert_eq!(row(&db, "d1", Some("committed")).await.payload_json, before);
        assert!(commit(&db, "d1").await.is_err(), "not committable twice");
    }

    #[test]
    fn fold_edits_never_touches_a_source_bound_social_draft() {
        // The sender re-derives body/media/firstComment/scheduledIso from
        // item.social and refuses a mismatch: folding would make it unsendable.
        let payload = r#"{"item":{"body":"B","media":{"kind":"single","imageUrls":["https://a/1.png"]},"social":{"platform":"linkedin","account":"royalti","content":"B","media_url":"https://a/1.png","scheduled_for":null}}}"#;
        let edit = r#"{"body":"B2","media":{"kind":"single","imageUrls":["https://a/2.png"]},"scheduledIso":null}"#;
        assert_eq!(fold_edits(payload, Some(edit)), None);
        // A Buffer draft without item.social is still folded.
        let plain =
            r#"{"item":{"body":"B","media":{"kind":"single","imageUrls":["https://a/1.png"]}}}"#;
        let out: Value = serde_json::from_str(&fold_edits(plain, Some(edit)).unwrap()).unwrap();
        assert_eq!(out["item"]["media"]["imageUrls"][0], "https://a/2.png");
        assert_eq!(out["item"]["body"], "B2");
    }

    #[test]
    fn fold_edits_targets_item_else_the_payload_and_skips_what_it_cannot_read() {
        // No item object: the payload itself is the item (as the approvals hash reads it).
        let out = fold_edits(r#"{"subject":"S"}"#, Some(r#"{"subject":"T"}"#)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"subject": "T"})
        );
        // Nothing to fold.
        assert_eq!(fold_edits(r#"{"item":{"subject":"S"}}"#, None), None);
        assert_eq!(fold_edits(r#"{"item":{"subject":"S"}}"#, Some("")), None);
        assert_eq!(
            fold_edits(r#"{"item":{"subject":"S"}}"#, Some("null")),
            None
        );
        assert_eq!(
            fold_edits(r#"{"item":{"subject":"S"}}"#, Some(r#"{"x":1}"#)),
            None
        );
        assert_eq!(
            fold_edits(r#"{"item":{"subject":"S"}}"#, Some(r#"{"subject":"S"}"#)),
            None
        );
        // Unreadable payload or edit: left as is.
        assert_eq!(fold_edits("not json", Some(r#"{"subject":"T"}"#)), None);
        assert_eq!(fold_edits(r#"[1]"#, Some(r#"{"subject":"T"}"#)), None);
        assert_eq!(fold_edits(r#"{"item":{}}"#, Some("{bad")), None);
        // A null value is skipped, except scheduledIso where it means unscheduled.
        let out = fold_edits(
            r#"{"item":{"firstComment":"c","scheduledIso":"2026-11-03T09:00:00Z"}}"#,
            Some(r#"{"firstComment":null,"scheduledIso":null}"#),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"item": {"firstComment": "c", "scheduledIso": null}})
        );
    }

    #[test]
    fn patch_error_checks_attachments_strictly() {
        let at = |v: Value| patch_error(Some(&json!({ "attachments": v })));
        for ok in [
            json!([]),
            json!([{"url": "https://cdn.x/a.pdf"}]),
            json!([{"url": "https://cdn.x/a.pdf", "name": "Contract.pdf"}]),
            Value::Array(vec![json!({"url": "https://a.b/c"}); MAX_ATTACHMENTS]),
        ] {
            assert_eq!(at(ok.clone()), None, "{ok}");
        }
        for bad in [
            json!(null),
            json!("https://a.b/c"),
            json!({"url": "https://a.b/c"}),
            json!(["https://a.b/c"]),
            json!([{"url": "http://a.b/c"}]),
            json!([{"url": "ftp://a.b/c"}]),
            json!([{"url": "/relative.pdf"}]),
            json!([{"url": 5}]),
            json!([{"name": "x"}]),
            json!([{"url": "https://a.b/c", "name": 5}]),
            json!([{"url": "https://a.b/c", "name": null}]),
            json!([{"url": "https://a.b/c", "size": 1}]),
            Value::Array(vec![json!({"url": "https://a.b/c"}); MAX_ATTACHMENTS + 1]),
        ] {
            assert_eq!(at(bad.clone()), Some(PATCH_ATTACHMENTS_INVALID), "{bad}");
        }
    }

    #[tokio::test]
    async fn attachments_merge_into_the_edit_and_fold_into_the_item() {
        let raw = json!({"item": {"subject": "S", "body": "B", "channel": "smtp"}});
        let (db, _t) = db_with(&[("d1", raw)]).await;
        let list = json!([{"url": "https://cdn.x/a.pdf", "name": "a.pdf"}, {"url": "https://cdn.x/b.png"}]);
        update(&db, "d1", &json!({"attachments": list}))
            .await
            .unwrap();
        update(&db, "d1", &json!({"body": "B2"})).await.unwrap();
        let edited: Value =
            serde_json::from_str(row(&db, "d1", None).await.edited_json.as_deref().unwrap())
                .unwrap();
        assert_eq!(edited, json!({"attachments": list, "body": "B2"}));
        assert_eq!(
            update(&db, "d1", &json!({"attachments": [{"url": "http://x/y"}]}))
                .await
                .unwrap_err(),
            PATCH_ATTACHMENTS_INVALID
        );
        commit(&db, "d1").await.unwrap();
        let p: Value =
            serde_json::from_str(&row(&db, "d1", Some("committed")).await.payload_json).unwrap();
        assert_eq!(p["item"]["attachments"], list);
        // An empty list is a list: approving "no attachments" is folded too.
        let raw = json!({"item": {"body": "B", "attachments": [{"url": "https://cdn.x/old.pdf"}]}});
        let (db, _t) = db_with(&[("d2", raw)]).await;
        update(&db, "d2", &json!({"attachments": []}))
            .await
            .unwrap();
        commit(&db, "d2").await.unwrap();
        let p: Value =
            serde_json::from_str(&row(&db, "d2", Some("committed")).await.payload_json).unwrap();
        assert_eq!(p["item"]["attachments"], json!([]));
    }

    // ── source-bound social drafts (D-15) ──────────────────────────────────

    fn golden_edits() -> Vec<Value> {
        let g: Value =
            serde_json::from_str(include_str!("testdata/social-draft-golden.json")).unwrap();
        g["edit"].as_array().unwrap().clone()
    }

    /// A linked social draft `id` whose item is `item`, plus its `social_queue`
    /// row `q-<id>` holding `item.social`, as `social-queue-submit.ts` writes them.
    async fn social_db(drafts: &[(&str, &Value)]) -> (PaDb, tempfile::TempDir) {
        let payloads: Vec<(&str, Value)> = drafts
            .iter()
            .map(|(id, item)| {
                (
                    *id,
                    json!({"item": item, "meta": {"actionId": "ruby/social-post"},
                           "source": {"kind": "social_post", "queue_id": format!("q-{id}"), "slug": null}}),
                )
            })
            .collect();
        let (db, t) = db_with(&payloads).await;
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query("UPDATE pa_action_drafts SET scheduled_at = NULL")
            .execute(&pool)
            .await
            .unwrap();
        for (id, item) in drafts {
            let s = &item["social"];
            sqlx::query(
                "INSERT INTO social_queue (id, platform, account, content, media_url, status, \
                 scheduled_for, created_at, source, slug) \
                 VALUES (?, ?, ?, ?, ?, 'draft', ?, '2026-11-02T00:00:00.000Z', 'ruby-queue', ?)",
            )
            .bind(format!("q-{id}"))
            .bind(s["platform"].as_str())
            .bind(s["account"].as_str())
            .bind(s["content"].as_str())
            .bind(s["media_url"].as_str())
            .bind(s["scheduled_for"].as_str())
            .bind(format!("slug-{id}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        (db, t)
    }

    async fn queue_row(db: &PaDb, id: &str) -> Option<(String, Option<String>, Option<String>)> {
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query_as("SELECT content, media_url, scheduled_for FROM social_queue WHERE id = ?")
            .bind(id)
            .fetch_optional(&pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_social_edit_rewrites_the_source_and_the_queue_row_in_one_go() {
        for c in golden_edits() {
            let name = c["name"].as_str().unwrap();
            let (db, _t) = social_db(&[("d1", &c["before"])]).await;
            update(&db, "d1", &c["patch"])
                .await
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let r = row(&db, "d1", None).await;
            assert_eq!(r.status, "edited", "{name}");
            assert_eq!(r.edited_json, None, "{name}: edited_json is never written");
            assert_eq!(r.scheduled_at, None, "{name}: Buffer holds the schedule");
            let p: Value = serde_json::from_str(&r.payload_json).unwrap();
            assert_eq!(
                social_draft::stable(&p["item"]),
                c["stable"].as_str().unwrap(),
                "{name}: the item is what the sender's buildSocialItem builds"
            );
            assert_eq!(p["source"]["queue_id"], "q-d1");
            let after = &c["after"];
            assert_eq!(
                queue_row(&db, "q-d1").await.unwrap(),
                (
                    after["content"].as_str().unwrap().to_string(),
                    after["media_url"].as_str().map(str::to_string),
                    after["scheduled_for"].as_str().map(str::to_string),
                ),
                "{name}: the queue row equals item.social"
            );
            // Nothing to fold: approving sends exactly this payload.
            let ev = commit(&db, "d1").await.unwrap();
            assert_eq!(ev.payload_json, r.payload_json, "{name}");
        }
    }

    #[tokio::test]
    async fn a_refused_social_edit_writes_nothing_anywhere() {
        let c = &golden_edits()[0];
        let (db, _t) = social_db(&[("d1", &c["before"])]).await;
        let before = row(&db, "d1", None).await;
        let q_before = queue_row(&db, "q-d1").await;

        let e = update(&db, "d1", &json!({"subject": "x"}))
            .await
            .unwrap_err();
        assert!(
            e.starts_with("social_refused: not_editable_at_source: "),
            "{e}"
        );
        let e = update(
            &db,
            "d1",
            &json!({"media": {"kind": "single", "imageUrls": ["http://a/b.png"]}}),
        )
        .await
        .unwrap_err();
        assert!(e.starts_with("social_refused: bad_media_url: "), "{e}");

        // The queue row was changed elsewhere: refused, the draft untouched.
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query("UPDATE social_queue SET content = 'changed elsewhere' WHERE id = 'q-d1'")
            .execute(&pool)
            .await
            .unwrap();
        let e = update(&db, "d1", &json!({"body": "new"}))
            .await
            .unwrap_err();
        assert!(
            e.starts_with("social_refused: social_queue_changed: "),
            "{e}"
        );
        let r = row(&db, "d1", None).await;
        assert_eq!(
            (r.status, r.payload_json),
            (before.status.clone(), before.payload_json.clone())
        );
        assert_eq!(
            queue_row(&db, "q-d1").await.unwrap().0,
            "changed elsewhere",
            "the rolled-back transaction left the row as it was"
        );

        // The queue row is gone (cancelled): refused, the draft untouched.
        sqlx::query("DELETE FROM social_queue WHERE id = 'q-d1'")
            .execute(&pool)
            .await
            .unwrap();
        let e = update(&db, "d1", &json!({"body": "new"}))
            .await
            .unwrap_err();
        assert!(
            e.starts_with("social_refused: social_queue_missing: "),
            "{e}"
        );
        let r = row(&db, "d1", None).await;
        assert_eq!(
            (r.status, r.payload_json, r.edited_json),
            (before.status, before.payload_json, None)
        );
        assert!(q_before.is_some());
    }

    #[tokio::test]
    async fn an_x_thread_takes_no_edit_and_an_x_post_no_first_comment() {
        let g: Value =
            serde_json::from_str(include_str!("testdata/social-draft-golden.json")).unwrap();
        let built = |part: &str| {
            let c = g["build"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"].as_str().unwrap().contains(part))
                .unwrap()
                .clone();
            social_draft::build_social_item(
                &social_draft::SocialPost::from_value(&c["canon"]).unwrap(),
                c["channelId"].as_str().unwrap(),
                c["draftId"].as_str().unwrap(),
            )
        };
        let thread = built("x thread");
        let single = built("x single post");
        let (db, _t) = social_db(&[("t", &thread), ("x", &single)]).await;
        let e = update(&db, "t", &json!({"scheduledIso": null}))
            .await
            .unwrap_err();
        assert!(
            e.starts_with("social_refused: thread_not_editable: "),
            "{e}"
        );
        let e = update(&db, "x", &json!({"firstComment": "c"}))
            .await
            .unwrap_err();
        assert!(e.starts_with("social_refused: x_no_first_comment: "), "{e}");
        let e = update(&db, "x", &json!({"body": "z".repeat(281)}))
            .await
            .unwrap_err();
        assert!(e.starts_with("social_refused: over_280: "), "{e}");
        for id in ["t", "x"] {
            assert_eq!(row(&db, id, None).await.status, "awaiting");
        }
        // Missing or terminal drafts still read as they always did.
        assert_eq!(
            update(&db, "nope", &json!({"body": "x"}))
                .await
                .unwrap_err(),
            update_refusal("nope")
        );
    }

    #[tokio::test]
    async fn reject_stores_a_trimmed_capped_reason_and_list_returns_it() {
        let (db, _t) = db_with(&[
            ("d1", social_payload()),
            ("d2", social_payload()),
            ("d3", social_payload()),
        ])
        .await;
        assert_eq!(
            reject(&db, "d1", Some("  Wrong tone  "))
                .await
                .unwrap()
                .as_deref(),
            Some("Wrong tone")
        );
        assert_eq!(reject(&db, "d2", Some("   ")).await.unwrap(), None);
        let long = "x".repeat(REJECT_REASON_MAX + 50);
        let stored = reject(&db, "d3", Some(&long)).await.unwrap().unwrap();
        assert_eq!(stored.chars().count(), REJECT_REASON_MAX);
        let rows = list(&db, Some("rejected")).await.unwrap();
        let by = |id: &str| {
            rows.iter()
                .find(|r| r.id == id)
                .unwrap()
                .reject_reason
                .clone()
        };
        assert_eq!(by("d1").as_deref(), Some("Wrong tone"));
        assert_eq!(by("d2"), None);
        assert_eq!(by("d3").map(|s| s.len()), Some(REJECT_REASON_MAX));
        // camelCase on the wire.
        let wire = serde_json::to_value(&rows[0]).unwrap();
        assert!(wire.get("rejectReason").is_some(), "{wire}");
        assert_eq!(
            reject(&db, "d1", Some("again")).await.unwrap_err(),
            reject_refusal("d1")
        );
    }

    #[test]
    fn a_reasonless_reject_event_is_the_payload_it_always_was() {
        let ev = PaActionRejectedEvent {
            draft_id: "d".into(),
            reason: None,
        };
        assert_eq!(serde_json::to_value(ev).unwrap(), json!({"draftId": "d"}));
        let ev = PaActionRejectedEvent {
            draft_id: "d".into(),
            reason: Some("r".into()),
        };
        assert_eq!(
            serde_json::to_value(ev).unwrap(),
            json!({"draftId": "d", "reason": "r"})
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
