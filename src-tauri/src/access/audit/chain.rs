//! The audit hash chain (G-ACCESS §6.2–§6.4).
//!
//! * **Hash** (§6.2, P-13): SHA-256 over a fixed binary framing of the row
//!   **as stored** — a verifier never re-serializes JSON.
//! * **Genesis**: `prev_hash_1 = SHA-256("ikenga-audit-genesis-v1" ‖ 0x00 ‖
//!   store_id)`; row 1 is always `store.created`.
//! * **Append** (§6.3): inside the caller's `BEGIN IMMEDIATE`. The DB head is
//!   compared with the head this process last committed; under T1 two
//!   processes write (broker + root CLI), so a mismatch is verified
//!   *forward* and adopted when it holds (P-33). A known row that is missing
//!   or changed, or a newer row that doesn't verify, degrades the chain.
//! * **Verify** (§6.4): a full walk from genesis — every hash recomputed,
//!   every link and `seq` checked. A failure enters `degraded`: access
//!   changes are refused with `audit_unavailable`; authentication,
//!   dispatch, decisions and their appends continue (P-35).

use std::sync::Mutex;

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::Event;

const DOMAIN: &[u8] = b"ikenga-audit-v1";
const GENESIS_DOMAIN: &[u8] = b"ikenga-audit-genesis-v1";

/// A chain position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    pub seq: i64,
    pub hash: [u8; 32],
}

/// Where a verification failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub broken_at_seq: i64,
    pub reason: String,
}

/// The outcome of a full walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub rows: i64,
    pub head: Option<Head>,
    pub broken: Option<Broken>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.broken.is_none()
    }
}

/// Why an append was refused.
#[derive(Debug)]
pub enum AppendError {
    /// The chain is degraded and the event is an access change (§6.4).
    AuditUnavailable(Broken),
    Sql(sqlx::Error),
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::AuditUnavailable(b) => write!(
                f,
                "audit_unavailable: the audit chain is broken at #{} ({}); access changes are \
                 paused until an operator reseals it",
                b.broken_at_seq, b.reason
            ),
            AppendError::Sql(e) => write!(f, "internal: audit append: {e}"),
        }
    }
}

impl std::error::Error for AppendError {}

impl From<sqlx::Error> for AppendError {
    fn from(e: sqlx::Error) -> Self {
        AppendError::Sql(e)
    }
}

/// `F(NULL) = 0xFF ; F(s) = 0x01 ‖ be32(len) ‖ utf8(s)` (§6.2).
fn field(h: &mut Sha256, v: Option<&str>) {
    match v {
        None => h.update([0xFF]),
        Some(s) => {
            h.update([0x01]);
            h.update((s.len() as u32).to_be_bytes());
            h.update(s.as_bytes());
        }
    }
}

/// One stored row, exactly as the columns hold it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    pub seq: i64,
    pub at_ms: i64,
    pub kind: String,
    pub category: String,
    pub principal_id: Option<String>,
    pub device_id: Option<String>,
    pub via: String,
    pub subject_principal_id: Option<String>,
    pub subject_device_id: Option<String>,
    pub project_key: Option<String>,
    pub target: Option<String>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
    pub detail: String,
    pub prev_hash: Vec<u8>,
    pub hash: Vec<u8>,
}

impl StoredRow {
    /// `hash_n` over this row with `prev` as `prev_hash_n` (§6.2).
    pub fn compute_hash(&self, prev: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(DOMAIN);
        h.update([0x00]);
        h.update(prev);
        h.update(self.seq.to_be_bytes());
        h.update(self.at_ms.to_be_bytes());
        field(&mut h, Some(&self.kind));
        field(&mut h, Some(&self.category));
        field(&mut h, self.principal_id.as_deref());
        field(&mut h, self.device_id.as_deref());
        field(&mut h, Some(&self.via));
        field(&mut h, self.subject_principal_id.as_deref());
        field(&mut h, self.subject_device_id.as_deref());
        field(&mut h, self.project_key.as_deref());
        field(&mut h, self.target.as_deref());
        field(&mut h, self.remote_addr.as_deref());
        field(&mut h, self.user_agent.as_deref());
        field(&mut h, Some(&self.detail));
        h.finalize().into()
    }

    pub(crate) fn from_sql(r: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            seq: r.try_get("seq")?,
            at_ms: r.try_get("at_ms")?,
            kind: r.try_get("kind")?,
            category: r.try_get("category")?,
            principal_id: r.try_get("principal_id")?,
            device_id: r.try_get("device_id")?,
            via: r.try_get("via")?,
            subject_principal_id: r.try_get("subject_principal_id")?,
            subject_device_id: r.try_get("subject_device_id")?,
            project_key: r.try_get("project_key")?,
            target: r.try_get("target")?,
            remote_addr: r.try_get("remote_addr")?,
            user_agent: r.try_get("user_agent")?,
            detail: r.try_get("detail")?,
            prev_hash: r.try_get("prev_hash")?,
            hash: r.try_get("hash")?,
        })
    }
}

/// `prev_hash_1` (§6.2).
pub fn genesis(store_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(GENESIS_DOMAIN);
    h.update([0x00]);
    h.update(store_id.as_bytes());
    h.finalize().into()
}

pub(crate) const COLUMNS: &str = "seq, at_ms, kind, category, principal_id, device_id, via, \
    subject_principal_id, subject_device_id, project_key, target, remote_addr, user_agent, \
    detail, prev_hash, hash";

pub(crate) fn to32(v: &[u8]) -> Option<[u8; 32]> {
    v.try_into().ok()
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The newest row's position, or `None` on an empty table.
pub async fn db_head(conn: &mut SqliteConnection) -> Result<Option<Head>, sqlx::Error> {
    let row = sqlx::query("SELECT seq, hash FROM audit_events ORDER BY seq DESC LIMIT 1")
        .fetch_optional(&mut *conn)
        .await?;
    Ok(row.and_then(|r| {
        let seq: i64 = r.get(0);
        let hash: Vec<u8> = r.get(1);
        to32(&hash).map(|hash| Head { seq, hash })
    }))
}

/// Walk rows with `seq > after.seq` (or from genesis when `after` is
/// `None`) and check `seq` contiguity, every link and every hash.
async fn walk(
    conn: &mut SqliteConnection,
    store_id: &str,
    after: Option<Head>,
) -> Result<VerifyReport, sqlx::Error> {
    let (mut prev, mut expected_seq) = match after {
        Some(h) => (h.hash, h.seq + 1),
        None => (genesis(store_id), 1),
    };
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM audit_events WHERE seq >= ? ORDER BY seq"
    ))
    .bind(expected_seq)
    .fetch_all(&mut *conn)
    .await?;
    let mut count = 0i64;
    let mut head = after;
    for r in &rows {
        let row = StoredRow::from_sql(r)?;
        let broken = |reason: String| VerifyReport {
            rows: count,
            head,
            broken: Some(Broken {
                broken_at_seq: expected_seq,
                reason,
            }),
        };
        if row.seq != expected_seq {
            return Ok(broken(format!(
                "seq gap: expected #{expected_seq}, found #{}",
                row.seq
            )));
        }
        if row.prev_hash.as_slice() != prev.as_slice() {
            return Ok(broken("prev_hash does not link to the previous row".into()));
        }
        let computed = row.compute_hash(&prev);
        if row.hash.as_slice() != computed.as_slice() {
            return Ok(broken("row hash does not match its contents".into()));
        }
        if after.is_none() && row.seq == 1 && row.kind != "store.created" {
            return Ok(broken("row 1 is not store.created".into()));
        }
        prev = computed;
        head = Some(Head {
            seq: row.seq,
            hash: computed,
        });
        expected_seq += 1;
        count += 1;
    }
    Ok(VerifyReport {
        rows: count,
        head,
        broken: None,
    })
}

/// A full verification walk from genesis (§6.4).
///
/// An empty `audit_events` table fails at `#1` whenever `store_meta` is
/// populated: the migration writes `store_meta` and the `store.created`
/// genesis row in one transaction (§6.2 "row 1 is always store.created"),
/// so a migrated store with no rows had them all deleted — and a fresh
/// process has no known head to catch that otherwise.
pub async fn verify_all(
    conn: &mut SqliteConnection,
    store_id: &str,
) -> Result<VerifyReport, sqlx::Error> {
    let report = walk(conn, store_id, None).await?;
    if report.rows == 0 && report.broken.is_none() {
        let migrated: i64 = sqlx::query_scalar("SELECT count(*) FROM store_meta")
            .fetch_one(&mut *conn)
            .await?;
        if migrated > 0 {
            return Ok(VerifyReport {
                rows: 0,
                head: None,
                broken: Some(Broken {
                    broken_at_seq: 1,
                    reason: "row 1 (store.created) is missing: the chain is empty".into(),
                }),
            });
        }
    }
    Ok(report)
}

/// Insert `ev` as the row after `head` (or as row 1 on `None`). Low level:
/// no head checks. Returns the new head.
pub async fn insert_row(
    conn: &mut SqliteConnection,
    store_id: &str,
    head: Option<Head>,
    ev: &Event,
) -> Result<Head, sqlx::Error> {
    insert_row_at(conn, store_id, head, ev, now_ms()).await
}

/// [`insert_row`] with an explicit `at_ms` — for the §6.6 backfill, whose
/// rows keep their original time (`at * 1000`).
pub async fn insert_row_at(
    conn: &mut SqliteConnection,
    store_id: &str,
    head: Option<Head>,
    ev: &Event,
    at_ms: i64,
) -> Result<Head, sqlx::Error> {
    let (seq, prev) = match head {
        Some(h) => (h.seq + 1, h.hash),
        None => (1, genesis(store_id)),
    };
    let row = StoredRow {
        seq,
        at_ms,
        kind: ev.kind.to_string(),
        category: ev.category().as_str().to_string(),
        principal_id: ev.principal_id.clone(),
        device_id: ev.device_id.clone(),
        via: ev.via.as_str().to_string(),
        subject_principal_id: ev.subject_principal_id.clone(),
        subject_device_id: ev.subject_device_id.clone(),
        project_key: ev.project_key.clone(),
        target: ev.target.clone(),
        remote_addr: ev.remote_addr.clone(),
        user_agent: ev.user_agent.clone(),
        // Compact JSON of the typed detail, hashed exactly as stored.
        detail: serde_json::to_string(&ev.detail).unwrap_or_else(|_| "{}".into()),
        prev_hash: prev.to_vec(),
        hash: Vec::new(),
    };
    let hash = row.compute_hash(&prev);
    sqlx::query(&format!(
        "INSERT INTO audit_events ({COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(row.seq)
    .bind(row.at_ms)
    .bind(&row.kind)
    .bind(&row.category)
    .bind(&row.principal_id)
    .bind(&row.device_id)
    .bind(&row.via)
    .bind(&row.subject_principal_id)
    .bind(&row.subject_device_id)
    .bind(&row.project_key)
    .bind(&row.target)
    .bind(&row.remote_addr)
    .bind(&row.user_agent)
    .bind(&row.detail)
    .bind(&row.prev_hash)
    .bind(hash.as_slice())
    .execute(&mut *conn)
    .await?;
    Ok(Head { seq, hash })
}

/// The process's view of one store's chain: the last head it committed and
/// whether it is degraded (§6.3, §6.4).
#[derive(Debug)]
pub struct Chain {
    store_id: String,
    known: Mutex<Option<Head>>,
    degraded: Mutex<Option<Broken>>,
    /// A break found mid-append whose `audit.chain_broken` row could not be
    /// written yet (its append was refused and rolled back).
    pending_broken_row: Mutex<Option<Broken>>,
}

/// The newest `audit.resealed` row after `b.broken_at_seq` that acknowledges
/// it (WP-77): another process (the T1 root CLI) may reseal while this one
/// is degraded.
async fn resealed_after(
    conn: &mut SqliteConnection,
    b: &Broken,
) -> Result<Option<Head>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT seq, hash FROM audit_events WHERE seq > ? AND kind = 'audit.resealed' \
         AND json_extract(detail, '$.broken_at_seq') = ? ORDER BY seq DESC LIMIT 1",
    )
    .bind(b.broken_at_seq)
    .bind(b.broken_at_seq)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.and_then(|r| {
        let seq: i64 = r.get(0);
        let hash: Vec<u8> = r.get(1);
        to32(&hash).map(|hash| Head { seq, hash })
    }))
}

impl Chain {
    pub fn new(store_id: impl Into<String>) -> Self {
        Self {
            store_id: store_id.into(),
            known: Mutex::new(None),
            degraded: Mutex::new(None),
            pending_broken_row: Mutex::new(None),
        }
    }

    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    pub fn degraded(&self) -> Option<Broken> {
        self.degraded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn known_head(&self) -> Option<Head> {
        *self.known.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_degraded(&self, b: Broken) {
        tracing::error!(
            "audit chain broken at #{}: {} — access changes are paused (G-ACCESS §6.4)",
            b.broken_at_seq,
            b.reason
        );
        let mut d = self.degraded.lock().unwrap_or_else(|e| e.into_inner());
        if d.is_none() {
            *d = Some(b.clone());
            *self
                .pending_broken_row
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(b);
        }
    }

    /// Enter `degraded` for a break the chain already records (an
    /// unacknowledged `audit.chain_broken` row): no second row.
    fn set_degraded_recorded(&self, b: Broken) {
        tracing::error!(
            "audit chain broken at #{} (recorded, not resealed): {} — access changes are paused \
             (G-ACCESS §6.4)",
            b.broken_at_seq,
            b.reason
        );
        let mut d = self.degraded.lock().unwrap_or_else(|e| e.into_inner());
        if d.is_none() {
            *d = Some(b);
        }
    }

    /// After the caller's `COMMIT`: remember `head` as ours (§6.3 step 4).
    /// Forgetting to call it is harmless — the next append verifies forward.
    pub fn committed(&self, head: Head) {
        *self.known.lock().unwrap_or_else(|e| e.into_inner()) = Some(head);
    }

    /// Clear `degraded` (reseal, WP-77) — the break stays in the chain.
    pub fn clear_degraded(&self) {
        *self.degraded.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .pending_broken_row
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// §6.4 at boot, on `access_audit_verify`, before an export and after a
    /// reseal: the reseal-aware full walk ([`super::verify_boot::verify`]).
    ///
    /// * A break this process found but could not record yet is written
    ///   first, so the walk sees it.
    /// * An outstanding break enters `degraded`; its `audit.chain_broken`
    ///   row is appended after the DB head in its own transaction unless
    ///   the chain already records it (a restart doesn't repeat the row).
    /// * No outstanding break clears `degraded` and adopts the head.
    ///
    /// `conn` must not be inside a transaction.
    pub async fn verify_boot(&self, conn: &mut SqliteConnection) -> anyhow::Result<VerifyReport> {
        self.flush_broken_row(conn).await?;
        let verdict = super::verify_boot::verify(conn, &self.store_id).await?;
        // The walk is the source of truth: recompute the state from it.
        self.clear_degraded();
        match &verdict.outstanding {
            None => {
                *self.known.lock().unwrap_or_else(|e| e.into_inner()) = verdict.head;
            }
            Some(b) if verdict.recorded => self.set_degraded_recorded(b.clone()),
            Some(b) => {
                self.set_degraded(b.clone());
                self.flush_broken_row(conn).await?;
            }
        }
        Ok(verdict.report())
    }

    /// Write a pending `audit.chain_broken` row, chained to the DB head.
    async fn flush_broken_row(&self, conn: &mut SqliteConnection) -> anyhow::Result<()> {
        let pending = self
            .pending_broken_row
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(b) = pending {
            use sqlx::Connection;
            let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
            let head = db_head(&mut tx).await?;
            let ev = broken_event(&b);
            let new_head = insert_row(&mut tx, &self.store_id, head, &ev).await?;
            tx.commit().await?;
            self.committed(new_head);
        }
        Ok(())
    }

    /// §6.3: append `ev` inside the caller's transaction (`conn` is the
    /// transaction's connection). Returns the new head; call
    /// [`committed`](Self::committed) with it after `COMMIT`.
    pub async fn append(
        &self,
        conn: &mut SqliteConnection,
        ev: &Event,
    ) -> Result<Head, AppendError> {
        // 0. Degraded: has another process (the T1 root CLI) resealed the
        //    break since? Then resume from its `audit.resealed` row, which
        //    step 2 verifies forward like any foreign append.
        if let Some(b) = self.degraded() {
            if let Some(h) = resealed_after(conn, &b).await? {
                tracing::info!(
                    "audit chain: the break at #{} was resealed at #{}; access changes resume",
                    b.broken_at_seq,
                    h.seq
                );
                self.clear_degraded();
                self.committed(h);
            }
        }
        // 1. The DB head.
        let db = db_head(conn).await?;
        // 2. Against the head this process holds.
        let known = self.known_head();
        if self.degraded().is_none() && known != db {
            match known {
                // Nothing known yet (a fresh process, e.g. the root CLI):
                // adopt the DB head; the boot walk is the integrity check.
                None => {}
                Some(k) => {
                    let still_there = sqlx::query("SELECT hash FROM audit_events WHERE seq = ?")
                        .bind(k.seq)
                        .fetch_optional(&mut *conn)
                        .await?
                        .map(|r| r.get::<Vec<u8>, _>(0));
                    match still_there {
                        Some(h) if h.as_slice() == k.hash.as_slice() => {
                            // §6.3 step 2: every newer row must link and hash.
                            let report = walk(conn, &self.store_id, Some(k)).await?;
                            if let Some(b) = report.broken {
                                self.set_degraded(b);
                            }
                        }
                        Some(_) => self.set_degraded(Broken {
                            broken_at_seq: k.seq,
                            reason: "a row this process wrote has changed".into(),
                        }),
                        None => self.set_degraded(Broken {
                            broken_at_seq: k.seq,
                            reason: "a row this process wrote is missing (head regression)".into(),
                        }),
                    }
                }
            }
        }
        if let Some(b) = self.degraded() {
            if ev.refused_when_degraded() {
                return Err(AppendError::AuditUnavailable(b));
            }
            // Degraded but allowed: chain to the DB head (the break stays
            // visible), writing the pending chain_broken row first.
            let mut head = db;
            let pending = self
                .pending_broken_row
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(pb) = pending {
                head = Some(insert_row(conn, &self.store_id, head, &broken_event(&pb)).await?);
            }
            return Ok(insert_row(conn, &self.store_id, head, ev).await?);
        }
        // 3. Insert chained to the (verified) DB head.
        Ok(insert_row(conn, &self.store_id, db, ev).await?)
    }
}

/// `audit.chain_broken {broken_at_seq, reason}` (§6.4).
fn broken_event(b: &Broken) -> Event {
    Event::new("audit.chain_broken", super::AuditVia::System)
        .detail(serde_json::json!({ "broken_at_seq": b.broken_at_seq, "reason": b.reason }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::audit::AuditVia;
    use sqlx::Connection;

    async fn store() -> SqliteConnection {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_core.sql"))
            .execute(&mut conn)
            .await
            .unwrap();
        conn
    }

    async fn seed(conn: &mut SqliteConnection, chain: &Chain, n: usize) {
        let mut tx = conn.begin().await.unwrap();
        let h = chain
            .append(&mut tx, &Event::new("store.created", AuditVia::System))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        chain.committed(h);
        for i in 0..n {
            let mut tx = conn.begin().await.unwrap();
            let ev = Event::new("device.tier_changed", AuditVia::Operator)
                .target(format!("device {i}"))
                .detail(serde_json::json!({"from": "view", "to": "dispatch"}));
            let h = chain.append(&mut tx, &ev).await.unwrap();
            tx.commit().await.unwrap();
            chain.committed(h);
        }
    }

    /// A-16: N appends verify; the head is the last row.
    #[tokio::test]
    async fn appends_verify_from_genesis() {
        let mut conn = store().await;
        let chain = Chain::new("01890a5d-ac96-774b-bcce-b302099a8057");
        seed(&mut conn, &chain, 20).await;
        let r = verify_all(&mut conn, chain.store_id()).await.unwrap();
        assert!(r.ok(), "{r:?}");
        assert_eq!(r.rows, 21);
        assert_eq!(r.head, chain.known_head());
        // Another store id never verifies these rows (genesis binding).
        let other = verify_all(&mut conn, "another-store").await.unwrap();
        assert_eq!(other.broken.unwrap().broken_at_seq, 1);
    }

    /// A-15: UPDATE and DELETE abort.
    #[tokio::test]
    async fn audit_rows_are_append_only() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 2).await;
        let upd = sqlx::query("UPDATE audit_events SET target = 'x' WHERE seq = 2")
            .execute(&mut conn)
            .await;
        assert!(upd.unwrap_err().to_string().contains("append-only"));
        let del = sqlx::query("DELETE FROM audit_events WHERE seq = 3")
            .execute(&mut conn)
            .await;
        assert!(del.unwrap_err().to_string().contains("append-only"));
    }

    /// Rebuild the table without its triggers, to play the attacker.
    async fn drop_triggers(conn: &mut SqliteConnection) {
        sqlx::raw_sql("DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete;")
            .execute(&mut *conn)
            .await
            .unwrap();
    }

    /// A-16: flipping any byte of any row, or deleting a middle row, fails
    /// verification at that seq.
    #[tokio::test]
    async fn tampering_fails_at_the_tampered_seq() {
        for victim in 1..=5i64 {
            for column in ["target", "detail", "at_ms", "kind", "hash"] {
                let mut conn = store().await;
                let chain = Chain::new("s");
                seed(&mut conn, &chain, 4).await;
                drop_triggers(&mut conn).await;
                let sql = match column {
                    "at_ms" => {
                        "UPDATE audit_events SET at_ms = at_ms + 1 WHERE seq = ?".to_string()
                    }
                    "hash" => {
                        "UPDATE audit_events SET hash = randomblob(32) WHERE seq = ?".to_string()
                    }
                    c => format!(
                        "UPDATE audit_events SET {c} = coalesce({c}, '') || 'x' WHERE seq = ?"
                    ),
                };
                sqlx::query(&sql)
                    .bind(victim)
                    .execute(&mut conn)
                    .await
                    .unwrap();
                let r = verify_all(&mut conn, "s").await.unwrap();
                let at = r
                    .broken
                    .unwrap_or_else(|| panic!("{column} on #{victim} went undetected"))
                    .broken_at_seq;
                // A forged `hash` shows at the row itself (its hash no longer
                // matches) — every other column too.
                assert_eq!(at, victim, "{column} on #{victim}");
            }
        }
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 4).await;
        drop_triggers(&mut conn).await;
        sqlx::query("DELETE FROM audit_events WHERE seq = 3")
            .execute(&mut conn)
            .await
            .unwrap();
        let r = verify_all(&mut conn, "s").await.unwrap();
        assert_eq!(r.broken.unwrap().broken_at_seq, 3);
    }

    /// §6.2 / review F-3: a migrated store (store_meta populated) whose
    /// every row was deleted fails at #1; an unmigrated, empty one is clean.
    #[tokio::test]
    async fn an_emptied_chain_fails_at_row_one() {
        let mut conn = store().await;
        let r = verify_all(&mut conn, "s").await.unwrap();
        assert!(r.ok(), "no store_meta yet: nothing to verify ({r:?})");
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 2).await;
        sqlx::query("INSERT INTO store_meta (k, v) VALUES ('store_id', 's')")
            .execute(&mut conn)
            .await
            .unwrap();
        drop_triggers(&mut conn).await;
        sqlx::query("DELETE FROM audit_events")
            .execute(&mut conn)
            .await
            .unwrap();
        let r = verify_all(&mut conn, "s").await.unwrap();
        let b = r.broken.expect("an emptied chain must not verify");
        assert_eq!(b.broken_at_seq, 1);
        assert_eq!(r.rows, 0);
    }

    /// A-16 (running process) / A-37 shape: a known head that vanished
    /// degrades; a foreign append that links is adopted.
    #[tokio::test]
    async fn head_regression_degrades_and_forward_appends_are_adopted() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 2).await;

        // Another writer (the root CLI) appends: forward verify adopts it.
        let cli = Chain::new("s");
        let mut tx = conn.begin().await.unwrap();
        cli.append(&mut tx, &Event::new("auth.sessions_revoked", AuditVia::Cli))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        let h = chain
            .append(&mut tx, &Event::new("device.revoked", AuditVia::Operator))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        chain.committed(h);
        assert!(chain.degraded().is_none());
        assert_eq!(h.seq, 5);

        // Truncate the tail the process knows about.
        drop_triggers(&mut conn).await;
        sqlx::query("DELETE FROM audit_events WHERE seq = 5")
            .execute(&mut conn)
            .await
            .unwrap();
        let mut tx = conn.begin().await.unwrap();
        let refused = chain
            .append(
                &mut tx,
                &Event::new("device.tier_changed", AuditVia::Operator),
            )
            .await;
        assert!(matches!(refused, Err(AppendError::AuditUnavailable(_))));
        drop(tx);
        assert_eq!(chain.degraded().unwrap().broken_at_seq, 5);

        // Authentication keeps appending after the DB head (P-35), with the
        // chain_broken row first.
        let mut tx = conn.begin().await.unwrap();
        let h = chain
            .append(&mut tx, &Event::new("auth.login_ok", AuditVia::Session))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(h.seq, 6);
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM audit_events WHERE seq >= 5")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        assert_eq!(kinds, ["audit.chain_broken", "auth.login_ok"]);
    }

    #[tokio::test]
    async fn boot_verify_degrades_and_records_the_break() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 3).await;
        drop_triggers(&mut conn).await;
        sqlx::query("UPDATE audit_events SET target = 'forged' WHERE seq = 2")
            .execute(&mut conn)
            .await
            .unwrap();
        let fresh = Chain::new("s");
        let r = fresh.verify_boot(&mut conn).await.unwrap();
        assert_eq!(r.broken.as_ref().unwrap().broken_at_seq, 2);
        assert_eq!(fresh.degraded().unwrap().broken_at_seq, 2);
        let last: String =
            sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(last, "audit.chain_broken");
    }

    #[test]
    fn framing_distinguishes_null_from_empty() {
        let mut a = Sha256::new();
        field(&mut a, None);
        let mut b = Sha256::new();
        field(&mut b, Some(""));
        assert_ne!(a.finalize(), b.finalize());
    }
}
