//! Notification aggregation (WP-40, Phase 5b, D-07 `notifications`).
//!
//! One SQLite table (`notifications`, migration `0066`) that every
//! user-attention event lands in, so the notification centre (WP-40b), the
//! daily address (WP-39) and the toast bridge read one list instead of each
//! listening to a different transient event.
//!
//! # Shape
//!
//! * [`record`] is the only writer of new rows. Producers call it from the
//!   existing emit sites with a [`NewNotification`] built by one of the pure
//!   builders in [`producers`], so each producer's copy and dedupe key is
//!   unit-testable without a running app.
//! * Reads ([`list`], [`unread_count`]) and read-state writes ([`mark_read`],
//!   [`mark_all_read`], [`mark_read_by_key`]) back the `notifications_*` Tauri
//!   commands (`commands::notifications`) and `GET /iyke/notifications`.
//! * Every change is published on a process-wide broadcast channel
//!   ([`subscribe`]). [`spawn_event_forwarder`] relays it to the webview as
//!   the `notifications://changed` Tauri event. Producers therefore only need
//!   a `SqlitePool` — `pkg::permissions_check::record_violation` has no
//!   `AppHandle` and does not need one.
//!
//! # Mute
//!
//! Per-kind mute lives in `~/.ikenga/settings.json` at
//! `workspace.notifications.mutedKinds` (see [`mute`]), not in this table and
//! not in `shell-store.ts`. Muted kinds are still **recorded** and filtered at
//! read time: list / unread count hide them, and the forwarded event carries
//! `muted: true` so the toast bridge stays quiet. `permission` and `violation`
//! cannot be muted (D-07: "Permission and violation cannot be muted; every
//! other kind can").
//!
//! # Kinds without a producer
//!
//! `invite` is a real kind (schema, parse, mute, tests) but has **no producer
//! yet**: it assumes D-05's people surface, which does not exist in the shell.
//! Nothing fakes one.

pub mod mute;
pub mod producers;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use tokio::sync::broadcast;

/// Tauri event the forwarder emits on every change.
pub const EVENT_NAME: &str = "notifications://changed";

/// Rows older than this that are already read are pruned on insert.
const READ_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Hard cap on table size, pruned on insert (oldest first).
const MAX_ROWS: i64 = 1000;
/// Default and maximum page size for [`list`].
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

// ─── Kinds ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// A Chi asked to read, write or run something.
    Permission,
    /// A Chi run ended cleanly.
    RunFinished,
    /// A Chi run ended non-zero / with an error.
    RunFailed,
    /// A shell or package release is available.
    Update,
    /// A package was refused something.
    Violation,
    /// Someone was given access, or an invite is about to expire. No producer
    /// yet (D-05 people surface does not exist).
    Invite,
}

impl NotificationKind {
    pub const ALL: [NotificationKind; 6] = [
        NotificationKind::Permission,
        NotificationKind::RunFinished,
        NotificationKind::RunFailed,
        NotificationKind::Update,
        NotificationKind::Violation,
        NotificationKind::Invite,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotificationKind::Permission => "permission",
            NotificationKind::RunFinished => "run_finished",
            NotificationKind::RunFailed => "run_failed",
            NotificationKind::Update => "update",
            NotificationKind::Violation => "violation",
            NotificationKind::Invite => "invite",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        Self::ALL
            .into_iter()
            .find(|k| k.as_str() == raw)
            .ok_or_else(|| format!("unknown notification kind: {raw}"))
    }

    /// D-07: permission and violation cannot be muted; every other kind can.
    pub fn is_mutable(self) -> bool {
        !matches!(self, NotificationKind::Permission | NotificationKind::Violation)
    }
}

// ─── Rows ───────────────────────────────────────────────────────────────────

/// How a repeat of the same `dedupe_key` is folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coalesce {
    /// Always insert a new row (the key, if any, is informational).
    Never,
    /// Drop the repeat if ANY row with the key exists, read or not. For
    /// one-shot facts: "0.9.1 is available", "permission request X".
    Once,
    /// Fold into the newest UNREAD row with the key (count + 1, copy and
    /// action replaced, `updated_at` bumped); insert a fresh row when every
    /// earlier one has been read. For repeating facts: violations, runs.
    WhileUnread,
}

/// What a producer hands to [`record`].
#[derive(Clone, Debug, PartialEq)]
pub struct NewNotification {
    pub kind: NotificationKind,
    pub title: String,
    pub body: Option<String>,
    pub action: Option<Value>,
    pub source: String,
    pub dedupe_key: Option<String>,
    pub coalesce: Coalesce,
}

/// A stored row, as the FE / bridge see it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: i64,
    pub kind: NotificationKind,
    pub title: String,
    pub body: Option<String>,
    pub action: Option<Value>,
    pub source: String,
    pub dedupe_key: Option<String>,
    pub count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub read_at: Option<i64>,
}

/// Result of [`record`].
#[derive(Clone, Debug, PartialEq)]
pub enum RecordOutcome {
    Inserted(Notification),
    Coalesced(Notification),
    /// `Coalesce::Once` and a row with the key already exists.
    Suppressed,
}

impl RecordOutcome {
    pub fn notification(&self) -> Option<&Notification> {
        match self {
            RecordOutcome::Inserted(n) | RecordOutcome::Coalesced(n) => Some(n),
            RecordOutcome::Suppressed => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadCount {
    pub total: i64,
    /// Unread per kind, muted kinds excluded. Keys are `NotificationKind::as_str`.
    pub by_kind: BTreeMap<String, i64>,
}

/// Filter for [`list`].
#[derive(Clone, Debug, Default)]
pub struct ListQuery {
    pub unread_only: bool,
    /// Only these kinds (None = all).
    pub kinds: Option<Vec<NotificationKind>>,
    /// Never these kinds — the muted set.
    pub exclude: Vec<NotificationKind>,
    /// Page size, clamped to `1..=MAX_LIMIT`.
    pub limit: Option<i64>,
    /// Cursor: only rows with `updated_at` strictly below this.
    pub before: Option<i64>,
}

// ─── Events ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeReason {
    Created,
    Coalesced,
    Read,
    ReadAll,
    MuteChanged,
}

/// Payload of `notifications://changed`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationEvent {
    pub reason: ChangeReason,
    /// The row, for `created` / `coalesced`.
    pub notification: Option<Notification>,
    /// True when the row's kind is muted. Set by the forwarder (which can see
    /// settings); always false on the in-process channel.
    pub muted: bool,
}

static EVENTS: OnceLock<broadcast::Sender<NotificationEvent>> = OnceLock::new();

fn events() -> &'static broadcast::Sender<NotificationEvent> {
    EVENTS.get_or_init(|| broadcast::channel(256).0)
}

/// Subscribe to every change. Lagging receivers skip ahead; the event is an
/// invalidation hint, the table is the truth.
pub fn subscribe() -> broadcast::Receiver<NotificationEvent> {
    events().subscribe()
}

pub(crate) fn publish(reason: ChangeReason, notification: Option<Notification>) {
    // No receivers (tests, early boot) is not an error.
    let _ = events().send(NotificationEvent {
        reason,
        notification,
        muted: false,
    });
}

/// Relay the broadcast channel to the webview as `notifications://changed`,
/// stamping `muted` from settings so the toast bridge can stay quiet for a
/// muted kind without re-reading settings per event.
pub fn spawn_event_forwarder(app: tauri::AppHandle) {
    use tauri::Emitter;
    let mut rx = subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(mut event) => {
                    if let Some(n) = &event.notification {
                        event.muted = mute::muted_kinds_for_app(&app).contains(&n.kind);
                    }
                    let _ = app.emit(EVENT_NAME, &event);
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!(
                        target: "ikenga::notifications",
                        "event forwarder lagged {skipped} events; FE will refetch on the next one"
                    );
                    // Still tell the FE something changed.
                    let _ = app.emit(
                        EVENT_NAME,
                        &NotificationEvent {
                            reason: ChangeReason::ReadAll,
                            notification: None,
                            muted: false,
                        },
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ─── Store ──────────────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

const SELECT_COLUMNS: &str = "id, kind, title, body, action, source, dedupe_key, count, \
                              created_at, updated_at, read_at";

fn row_to_notification(row: &sqlx::sqlite::SqliteRow) -> Result<Notification, String> {
    let kind_raw: String = row.try_get("kind").map_err(|e| format!("kind: {e}"))?;
    let action_raw: Option<String> = row.try_get("action").map_err(|e| format!("action: {e}"))?;
    Ok(Notification {
        id: row.try_get("id").map_err(|e| format!("id: {e}"))?,
        kind: NotificationKind::parse(&kind_raw)?,
        title: row.try_get("title").map_err(|e| format!("title: {e}"))?,
        body: row.try_get("body").map_err(|e| format!("body: {e}"))?,
        // A malformed action is dropped, not fatal: the row still shows.
        action: action_raw.and_then(|s| serde_json::from_str(&s).ok()),
        source: row.try_get("source").map_err(|e| format!("source: {e}"))?,
        dedupe_key: row
            .try_get("dedupe_key")
            .map_err(|e| format!("dedupe_key: {e}"))?,
        count: row.try_get("count").map_err(|e| format!("count: {e}"))?,
        created_at: row
            .try_get("created_at")
            .map_err(|e| format!("created_at: {e}"))?,
        updated_at: row
            .try_get("updated_at")
            .map_err(|e| format!("updated_at: {e}"))?,
        read_at: row.try_get("read_at").map_err(|e| format!("read_at: {e}"))?,
    })
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Record one notification. The single writer of new rows.
///
/// Runs in one transaction on the (single-connection) writer pool, so the
/// dedupe lookup and the insert/update cannot interleave with another
/// producer in this process. Publishes `created` / `coalesced` on success.
pub async fn record(
    pool: &sqlx::SqlitePool,
    new: NewNotification,
) -> Result<RecordOutcome, String> {
    let now = now_ms();
    let action_json = new.action.as_ref().map(Value::to_string);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("notifications begin: {e}"))?;

    let outcome_id: Option<(i64, bool)> = match (&new.dedupe_key, new.coalesce) {
        (Some(key), Coalesce::Once) => {
            let exists: Option<i64> =
                sqlx::query_scalar("SELECT id FROM notifications WHERE dedupe_key = ? LIMIT 1")
                    .bind(key)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| format!("notifications dedupe lookup: {e}"))?;
            if exists.is_some() {
                tx.rollback().await.ok();
                return Ok(RecordOutcome::Suppressed);
            }
            None
        }
        (Some(key), Coalesce::WhileUnread) => {
            let unread: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM notifications
                 WHERE dedupe_key = ? AND read_at IS NULL
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| format!("notifications coalesce lookup: {e}"))?;
            match unread {
                Some(id) => {
                    sqlx::query(
                        "UPDATE notifications
                         SET title = ?, body = ?, action = ?, source = ?,
                             count = count + 1, updated_at = ?
                         WHERE id = ?",
                    )
                    .bind(&new.title)
                    .bind(&new.body)
                    .bind(&action_json)
                    .bind(&new.source)
                    .bind(now)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| format!("notifications coalesce: {e}"))?;
                    Some((id, true))
                }
                None => None,
            }
        }
        _ => None,
    };

    let (id, coalesced) = match outcome_id {
        Some(found) => found,
        None => {
            let res = sqlx::query(
                "INSERT INTO notifications
                   (kind, title, body, action, source, dedupe_key, count, created_at, updated_at, read_at)
                 VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?, NULL)",
            )
            .bind(new.kind.as_str())
            .bind(&new.title)
            .bind(&new.body)
            .bind(&action_json)
            .bind(&new.source)
            .bind(&new.dedupe_key)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("notifications insert: {e}"))?;
            (res.last_insert_rowid(), false)
        }
    };

    prune(&mut tx, now).await?;

    let row = sqlx::query(&format!(
        "SELECT {SELECT_COLUMNS} FROM notifications WHERE id = ?"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| format!("notifications read back: {e}"))?;
    let stored = row_to_notification(&row)?;

    tx.commit()
        .await
        .map_err(|e| format!("notifications commit: {e}"))?;

    if coalesced {
        publish(ChangeReason::Coalesced, Some(stored.clone()));
        Ok(RecordOutcome::Coalesced(stored))
    } else {
        publish(ChangeReason::Created, Some(stored.clone()));
        Ok(RecordOutcome::Inserted(stored))
    }
}

/// Best-effort wrapper for emit sites: a failed notification write must never
/// break the thing that produced it (a permission ask, a run, a denial).
pub async fn record_best_effort(pool: &sqlx::SqlitePool, new: NewNotification) {
    let kind = new.kind;
    if let Err(e) = record(pool, new).await {
        log::warn!(
            target: "ikenga::notifications",
            "could not record {} notification: {e}",
            kind.as_str()
        );
    }
}

/// Same as [`record_best_effort`] for call sites that hold a `PaDb`.
pub async fn record_with_db(db: &crate::commands::db::PaDb, new: NewNotification) {
    match db.ensure_pool().await {
        Ok(pool) => record_best_effort(&pool, new).await,
        Err(e) => log::warn!(target: "ikenga::notifications", "no db pool: {e}"),
    }
}

/// Retention: read rows older than 30 days go, then the table is capped at
/// `MAX_ROWS` newest. Unread rows are only ever dropped by the cap.
async fn prune(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    now: i64,
) -> Result<(), String> {
    sqlx::query("DELETE FROM notifications WHERE read_at IS NOT NULL AND updated_at < ?")
        .bind(now - READ_RETENTION_MS)
        .execute(&mut **tx)
        .await
        .map_err(|e| format!("notifications prune (age): {e}"))?;
    sqlx::query(
        "DELETE FROM notifications WHERE id IN (
           SELECT id FROM notifications
           ORDER BY updated_at DESC, id DESC
           LIMIT -1 OFFSET ?
         )",
    )
    .bind(MAX_ROWS)
    .execute(&mut **tx)
    .await
    .map_err(|e| format!("notifications prune (cap): {e}"))?;
    Ok(())
}

/// Newest first (by latest occurrence).
pub async fn list(pool: &sqlx::SqlitePool, q: &ListQuery) -> Result<Vec<Notification>, String> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM notifications WHERE 1 = 1");
    let mut binds: Vec<String> = Vec::new();
    if q.unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    if let Some(kinds) = &q.kinds {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
        binds.extend(kinds.iter().map(|k| k.as_str().to_string()));
    }
    if !q.exclude.is_empty() {
        sql.push_str(&format!(" AND kind NOT IN ({})", placeholders(q.exclude.len())));
        binds.extend(q.exclude.iter().map(|k| k.as_str().to_string()));
    }
    if q.before.is_some() {
        sql.push_str(" AND updated_at < ?");
    }
    sql.push_str(" ORDER BY updated_at DESC, id DESC LIMIT ?");

    let mut query = sqlx::query(&sql);
    for b in &binds {
        query = query.bind(b);
    }
    if let Some(before) = q.before {
        query = query.bind(before);
    }
    query = query.bind(q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT));

    let rows = query
        .fetch_all(pool)
        .await
        .map_err(|e| format!("notifications list: {e}"))?;
    rows.iter().map(row_to_notification).collect()
}

/// Unread count, muted kinds excluded.
pub async fn unread_count(
    pool: &sqlx::SqlitePool,
    exclude: &[NotificationKind],
) -> Result<UnreadCount, String> {
    let rows = sqlx::query(
        "SELECT kind, COUNT(*) AS n FROM notifications WHERE read_at IS NULL GROUP BY kind",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("notifications unread count: {e}"))?;
    let mut out = UnreadCount::default();
    for row in rows {
        let raw: String = row.try_get("kind").map_err(|e| format!("kind: {e}"))?;
        let n: i64 = row.try_get("n").map_err(|e| format!("n: {e}"))?;
        // Unknown kinds (a newer build's rows after a downgrade) are skipped.
        let Ok(kind) = NotificationKind::parse(&raw) else {
            continue;
        };
        if exclude.contains(&kind) {
            continue;
        }
        out.total += n;
        out.by_kind.insert(kind.as_str().to_string(), n);
    }
    Ok(out)
}

/// Mark the given rows read. Returns rows changed (already-read rows keep
/// their original `read_at`).
pub async fn mark_read(pool: &sqlx::SqlitePool, ids: &[i64]) -> Result<u64, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let sql = format!(
        "UPDATE notifications SET read_at = ? WHERE read_at IS NULL AND id IN ({})",
        placeholders(ids.len())
    );
    let mut query = sqlx::query(&sql).bind(now_ms());
    for id in ids {
        query = query.bind(id);
    }
    let changed = query
        .execute(pool)
        .await
        .map_err(|e| format!("notifications mark read: {e}"))?
        .rows_affected();
    if changed > 0 {
        publish(ChangeReason::Read, None);
    }
    Ok(changed)
}

/// Mark every unread row read, optionally only one kind.
pub async fn mark_all_read(
    pool: &sqlx::SqlitePool,
    kind: Option<NotificationKind>,
) -> Result<u64, String> {
    let now = now_ms();
    let res = match kind {
        Some(k) => {
            sqlx::query("UPDATE notifications SET read_at = ? WHERE read_at IS NULL AND kind = ?")
                .bind(now)
                .bind(k.as_str())
                .execute(pool)
                .await
        }
        None => {
            sqlx::query("UPDATE notifications SET read_at = ? WHERE read_at IS NULL")
                .bind(now)
                .execute(pool)
                .await
        }
    }
    .map_err(|e| format!("notifications mark all read: {e}"))?;
    let changed = res.rows_affected();
    if changed > 0 {
        publish(ChangeReason::ReadAll, None);
    }
    Ok(changed)
}

/// Mark the rows carrying `dedupe_key` read. Used when the thing a row asks
/// about is resolved elsewhere (a permission answered in its dialog, a gate
/// timing out) so the centre does not keep offering a dead Allow / Deny.
pub async fn mark_read_by_key(pool: &sqlx::SqlitePool, dedupe_key: &str) -> Result<u64, String> {
    let changed = sqlx::query(
        "UPDATE notifications SET read_at = ? WHERE read_at IS NULL AND dedupe_key = ?",
    )
    .bind(now_ms())
    .bind(dedupe_key)
    .execute(pool)
    .await
    .map_err(|e| format!("notifications mark read by key: {e}"))?
    .rows_affected();
    if changed > 0 {
        publish(ChangeReason::Read, None);
    }
    Ok(changed)
}

/// Best-effort [`mark_read_by_key`] for emit sites holding a `PaDb`.
pub async fn resolve_key_with_db(db: &crate::commands::db::PaDb, dedupe_key: &str) {
    let result = match db.ensure_pool().await {
        Ok(pool) => mark_read_by_key(&pool, dedupe_key).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        log::warn!(target: "ikenga::notifications", "could not resolve {dedupe_key}: {e}");
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::db::PaDb;
    use serde_json::json;

    async fn fresh_pool() -> (sqlx::SqlitePool, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("ikenga.db"));
        let pool = db.ensure_pool().await.expect("pool");
        (pool, tmp)
    }

    fn note(kind: NotificationKind, key: Option<&str>, coalesce: Coalesce) -> NewNotification {
        NewNotification {
            kind,
            title: format!("{} title", kind.as_str()),
            body: Some("body".into()),
            action: Some(json!({ "kind": "open.test" })),
            source: "test".into(),
            dedupe_key: key.map(str::to_string),
            coalesce,
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_name() {
        for kind in NotificationKind::ALL {
            assert_eq!(NotificationKind::parse(kind.as_str()).unwrap(), kind);
            let wire = serde_json::to_value(kind).unwrap();
            assert_eq!(wire, json!(kind.as_str()));
        }
        assert!(NotificationKind::parse("toast").is_err());
    }

    #[test]
    fn permission_and_violation_are_the_only_unmutable_kinds() {
        let unmutable: Vec<_> = NotificationKind::ALL
            .into_iter()
            .filter(|k| !k.is_mutable())
            .collect();
        assert_eq!(
            unmutable,
            vec![NotificationKind::Permission, NotificationKind::Violation]
        );
    }

    #[tokio::test]
    async fn migration_0066_creates_the_notifications_table() {
        let (pool, _tmp) = fresh_pool().await;
        let cols: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('notifications')")
                .fetch_all(&pool)
                .await
                .unwrap();
        for c in [
            "id", "kind", "title", "body", "action", "source", "dedupe_key", "count",
            "created_at", "updated_at", "read_at",
        ] {
            assert!(cols.iter().any(|x| x == c), "missing column {c}: {cols:?}");
        }
    }

    #[tokio::test]
    async fn record_inserts_and_list_returns_newest_first() {
        let (pool, _tmp) = fresh_pool().await;
        let a = record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        let b = record(&pool, note(NotificationKind::RunFailed, None, Coalesce::Never))
            .await
            .unwrap();
        assert!(matches!(a, RecordOutcome::Inserted(_)));
        let rows = list(&pool, &ListQuery::default()).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, b.notification().unwrap().id);
        assert_eq!(rows[0].action, Some(json!({ "kind": "open.test" })));
        assert_eq!(rows[0].count, 1);
        assert!(rows[0].read_at.is_none());
    }

    #[tokio::test]
    async fn unread_count_counts_per_kind_and_drops_read_rows() {
        let (pool, _tmp) = fresh_pool().await;
        let first = record(&pool, note(NotificationKind::Permission, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Permission, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();

        let c = unread_count(&pool, &[]).await.unwrap();
        assert_eq!(c.total, 3);
        assert_eq!(c.by_kind.get("permission"), Some(&2));
        assert_eq!(c.by_kind.get("update"), Some(&1));

        let changed = mark_read(&pool, &[first.notification().unwrap().id])
            .await
            .unwrap();
        assert_eq!(changed, 1);
        // Re-marking is a no-op.
        assert_eq!(
            mark_read(&pool, &[first.notification().unwrap().id]).await.unwrap(),
            0
        );
        let c = unread_count(&pool, &[]).await.unwrap();
        assert_eq!(c.total, 2);

        assert_eq!(mark_all_read(&pool, None).await.unwrap(), 2);
        assert_eq!(unread_count(&pool, &[]).await.unwrap().total, 0);
    }

    #[tokio::test]
    async fn mute_is_respected_by_list_and_unread_count() {
        let (pool, _tmp) = fresh_pool().await;
        record(&pool, note(NotificationKind::RunFinished, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Permission, None, Coalesce::Never))
            .await
            .unwrap();

        let muted = vec![NotificationKind::RunFinished, NotificationKind::Update];
        let c = unread_count(&pool, &muted).await.unwrap();
        assert_eq!(c.total, 1);
        assert_eq!(c.by_kind.keys().collect::<Vec<_>>(), vec!["permission"]);

        let rows = list(
            &pool,
            &ListQuery {
                exclude: muted.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, NotificationKind::Permission);

        // Muted rows are still recorded — un-muting shows them again.
        assert_eq!(list(&pool, &ListQuery::default()).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn list_filters_unread_kind_and_cursor() {
        let (pool, _tmp) = fresh_pool().await;
        let r1 = record(&pool, note(NotificationKind::Violation, None, Coalesce::Never))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
        record(&pool, note(NotificationKind::Violation, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();
        mark_read(&pool, &[r1.notification().unwrap().id]).await.unwrap();

        let unread_violations = list(
            &pool,
            &ListQuery {
                unread_only: true,
                kinds: Some(vec![NotificationKind::Violation]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(unread_violations.len(), 1);

        let older = list(
            &pool,
            &ListQuery {
                before: Some(r1.notification().unwrap().updated_at + 1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(older.iter().all(|n| n.updated_at <= r1.notification().unwrap().updated_at));
        assert!(older.iter().any(|n| n.id == r1.notification().unwrap().id));

        let none = list(
            &pool,
            &ListQuery {
                kinds: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn coalesce_once_drops_a_repeat_even_after_it_is_read() {
        let (pool, _tmp) = fresh_pool().await;
        let key = Some("update:shell:0.9.1");
        let first = record(&pool, note(NotificationKind::Update, key, Coalesce::Once))
            .await
            .unwrap();
        assert!(matches!(first, RecordOutcome::Inserted(_)));
        assert_eq!(
            record(&pool, note(NotificationKind::Update, key, Coalesce::Once))
                .await
                .unwrap(),
            RecordOutcome::Suppressed
        );
        mark_all_read(&pool, None).await.unwrap();
        assert_eq!(
            record(&pool, note(NotificationKind::Update, key, Coalesce::Once))
                .await
                .unwrap(),
            RecordOutcome::Suppressed
        );
        assert_eq!(list(&pool, &ListQuery::default()).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn coalesce_while_unread_folds_then_starts_a_new_row_once_read() {
        let (pool, _tmp) = fresh_pool().await;
        let key = Some("violation:p:shell.execute:ffmpeg");
        let a = record(&pool, note(NotificationKind::Violation, key, Coalesce::WhileUnread))
            .await
            .unwrap();
        let b = record(&pool, note(NotificationKind::Violation, key, Coalesce::WhileUnread))
            .await
            .unwrap();
        let b = match b {
            RecordOutcome::Coalesced(n) => n,
            other => panic!("expected coalesced, got {other:?}"),
        };
        assert_eq!(b.id, a.notification().unwrap().id);
        assert_eq!(b.count, 2);
        assert!(b.updated_at >= b.created_at);
        assert_eq!(unread_count(&pool, &[]).await.unwrap().total, 1);

        mark_read(&pool, &[b.id]).await.unwrap();
        let c = record(&pool, note(NotificationKind::Violation, key, Coalesce::WhileUnread))
            .await
            .unwrap();
        assert!(matches!(c, RecordOutcome::Inserted(_)));
        assert_ne!(c.notification().unwrap().id, b.id);
        assert_eq!(c.notification().unwrap().count, 1);
    }

    #[tokio::test]
    async fn mark_read_by_key_resolves_only_that_key() {
        let (pool, _tmp) = fresh_pool().await;
        record(&pool, note(NotificationKind::Permission, Some("permission:hook:1"), Coalesce::Once))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::Permission, Some("permission:hook:2"), Coalesce::Once))
            .await
            .unwrap();
        assert_eq!(mark_read_by_key(&pool, "permission:hook:1").await.unwrap(), 1);
        let unread = list(
            &pool,
            &ListQuery {
                unread_only: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].dedupe_key.as_deref(), Some("permission:hook:2"));
    }

    #[tokio::test]
    async fn mark_all_read_can_target_one_kind() {
        let (pool, _tmp) = fresh_pool().await;
        record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();
        record(&pool, note(NotificationKind::RunFinished, None, Coalesce::Never))
            .await
            .unwrap();
        assert_eq!(
            mark_all_read(&pool, Some(NotificationKind::Update)).await.unwrap(),
            1
        );
        let c = unread_count(&pool, &[]).await.unwrap();
        assert_eq!(c.by_kind.get("run_finished"), Some(&1));
        assert!(c.by_kind.get("update").is_none());
    }

    #[tokio::test]
    async fn prune_drops_old_read_rows_and_keeps_unread() {
        let (pool, _tmp) = fresh_pool().await;
        let old = now_ms() - READ_RETENTION_MS - 1000;
        for read in [true, false] {
            sqlx::query(
                "INSERT INTO notifications (kind, title, source, count, created_at, updated_at, read_at)
                 VALUES ('update', 'old', 'test', 1, ?, ?, ?)",
            )
            .bind(old)
            .bind(old)
            .bind(if read { Some(old) } else { None })
            .execute(&pool)
            .await
            .unwrap();
        }
        record(&pool, note(NotificationKind::Update, None, Coalesce::Never))
            .await
            .unwrap();
        let rows = list(&pool, &ListQuery::default()).await.unwrap();
        assert_eq!(rows.len(), 2, "old read row pruned, old unread row kept");
        assert!(rows.iter().all(|n| n.title != "old" || n.read_at.is_none()));
    }

    #[tokio::test]
    async fn record_publishes_a_created_event() {
        let mut rx = subscribe();
        let (pool, _tmp) = fresh_pool().await;
        let out = record(&pool, note(NotificationKind::RunFinished, Some("evt-test-unique"), Coalesce::Once))
            .await
            .unwrap();
        let id = out.notification().unwrap().id;
        // Other tests publish on the same process-wide channel; find ours.
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => panic!("channel closed"),
            };
            if ev.reason == ChangeReason::Created
                && ev.notification.as_ref().and_then(|n| n.dedupe_key.as_deref())
                    == Some("evt-test-unique")
            {
                assert_eq!(ev.notification.unwrap().id, id);
                assert!(!ev.muted);
                break;
            }
        }
    }

    #[tokio::test]
    async fn invite_kind_is_stored_and_mutable_but_has_no_producer() {
        // `invite` has no emit site (D-05 people surface does not exist). The
        // kind still has to store, count and mute like any other so the day a
        // producer lands it needs no schema or command change.
        let (pool, _tmp) = fresh_pool().await;
        record(&pool, note(NotificationKind::Invite, None, Coalesce::Never))
            .await
            .unwrap();
        assert!(NotificationKind::Invite.is_mutable());
        assert_eq!(
            unread_count(&pool, &[]).await.unwrap().by_kind.get("invite"),
            Some(&1)
        );
        assert_eq!(
            unread_count(&pool, &[NotificationKind::Invite])
                .await
                .unwrap()
                .total,
            0
        );
    }
}
