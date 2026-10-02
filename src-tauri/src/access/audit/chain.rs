//! The audit hash chain (G-ACCESS §6.1–§6.4, DEC-80, P-13).
//!
//! `audit_events` is append-only (triggers, A-15) and every row is chained:
//!
//! ```text
//! hash_n = SHA-256( "ikenga-audit-v1" ‖ 0x00 ‖ prev_hash_n ‖ be64(seq) ‖ be64(at_ms)
//!                 ‖ F(kind) ‖ F(category) ‖ F(principal_id) ‖ F(device_id) ‖ F(via)
//!                 ‖ F(subject_principal_id) ‖ F(subject_device_id) ‖ F(project_key)
//!                 ‖ F(target) ‖ F(remote_addr) ‖ F(user_agent) ‖ F(detail) )
//! F(NULL) = 0xFF ;  F(s) = 0x01 ‖ be32(len(utf8(s))) ‖ utf8(s)
//! prev_hash_1 = SHA-256("ikenga-audit-genesis-v1" ‖ 0x00 ‖ store_id)
//! ```
//!
//! The stored bytes are what gets hashed: `detail` is hashed as stored, never
//! re-serialized.
//!
//! [`append`] runs inside the caller's `BEGIN IMMEDIATE` transaction (§6.3),
//! so an access change and its audit row commit together (A-18). [`verify`]
//! walks from the genesis and fails closed (§6.4): a broken chain puts the
//! [`Chain`] in `degraded`, where access-changing appends are refused with
//! `audit_unavailable` while authentication, dispatch and decision events
//! continue (P-35).
//!
//! WP-77 owns what is built on top: list, export, reseal, the
//! `audit.chain_broken` row for breaks found mid-append by an access change,
//! and the `auth_events` absorption.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

/// The fixed list of §6.5 kinds → category. Anything else is refused.
const KINDS: &[(&str, &str)] = &[
    ("pair.started", "pairing"),
    ("pair.failed", "pairing"),
    ("pair.denied", "pairing"),
    ("pair.allowed", "pairing"),
    ("pair.cancelled", "pairing"),
    ("device.tier_changed", "pairing"),
    ("device.revoked", "pairing"),
    ("device.expired", "pairing"),
    ("routing.changed", "pairing"),
    ("member.added", "people"),
    ("member.role_changed", "people"),
    ("member.removed", "people"),
    ("member.restored", "people"),
    ("member.expired", "people"),
    ("invite.issued", "people"),
    ("invite.revoked", "people"),
    ("invite.accepted", "people"),
    ("policy.changed", "people"),
    ("policy.owner_approval_changed", "people"),
    ("ownership.offered", "people"),
    ("ownership.accepted", "people"),
    ("permission.decided", "permission"),
    ("permission.refused", "permission"),
    ("dispatch.sent", "dispatch"),
    ("auth.login_ok", "access"),
    ("auth.login_fail", "access"),
    ("auth.login_throttled", "access"),
    ("auth.logout", "access"),
    ("auth.password_changed", "access"),
    ("auth.account_created", "access"),
    ("auth.account_disabled", "access"),
    ("auth.account_enabled", "access"),
    ("auth.sessions_revoked", "access"),
    ("auth.provision_failed", "access"),
    ("auth.probe_failed", "access"),
    ("share.artifact_viewed", "access"),
    ("app.locked", "access"),
    ("app.unlocked", "access"),
    ("vault.locked", "access"),
    ("vault.unlocked", "access"),
    ("store.created", "access"),
    ("audit.verified", "access"),
    ("audit.exported", "access"),
    ("audit.chain_broken", "access"),
    ("audit.resealed", "access"),
];

/// §6.5: the category a kind is filed under; `None` for an unknown kind.
pub fn category_for(kind: &str) -> Option<&'static str> {
    KINDS.iter().find(|(k, _)| *k == kind).map(|(_, c)| *c)
}

/// §6.4: which appends continue in `degraded` mode. Everything else is an
/// access change and is refused.
pub fn continues_when_degraded(kind: &str) -> bool {
    [
        "auth.",
        "dispatch.",
        "permission.",
        "app.",
        "vault.",
        "audit.",
    ]
    .iter()
    .any(|p| kind.starts_with(p))
        || kind == "share.artifact_viewed"
}

/// `via` column values (§6.1).
pub const VIAS: &[&str] = &["session", "device", "operator", "cli", "system"];

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One event to append. `detail` is compact JSON of a typed value; it never
/// carries secrets, tokens, codes or passwords (§6.2, A-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub at_ms: i64,
    pub kind: String,
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
}

impl Event {
    pub fn new(kind: &str, via: &str) -> Self {
        Event {
            at_ms: now_ms(),
            kind: kind.to_string(),
            principal_id: None,
            device_id: None,
            via: via.to_string(),
            subject_principal_id: None,
            subject_device_id: None,
            project_key: None,
            target: None,
            remote_addr: None,
            user_agent: None,
            detail: "{}".to_string(),
        }
    }

    pub fn actor(mut self, principal_id: Option<String>, device_id: Option<String>) -> Self {
        self.principal_id = principal_id;
        self.device_id = device_id;
        self
    }

    pub fn subject(mut self, principal_id: Option<String>, device_id: Option<String>) -> Self {
        self.subject_principal_id = principal_id;
        self.subject_device_id = device_id;
        self
    }

    pub fn target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn detail(mut self, detail: &serde_json::Value) -> Self {
        // `to_string` on a Value is compact, and Value maps are ordered
        // (no `preserve_order`), so the stored text is deterministic.
        self.detail = detail.to_string();
        self
    }
}

/// A stored row, as the chain sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    pub seq: i64,
    pub event: Event,
    pub category: String,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
}

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

/// §6.2: the row hash over the fixed binary framing.
pub fn row_hash(prev: &[u8; 32], seq: i64, ev: &Event, category: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ikenga-audit-v1\0");
    h.update(prev);
    h.update(seq.to_be_bytes());
    h.update(ev.at_ms.to_be_bytes());
    field(&mut h, Some(&ev.kind));
    field(&mut h, Some(category));
    field(&mut h, ev.principal_id.as_deref());
    field(&mut h, ev.device_id.as_deref());
    field(&mut h, Some(&ev.via));
    field(&mut h, ev.subject_principal_id.as_deref());
    field(&mut h, ev.subject_device_id.as_deref());
    field(&mut h, ev.project_key.as_deref());
    field(&mut h, ev.target.as_deref());
    field(&mut h, ev.remote_addr.as_deref());
    field(&mut h, ev.user_agent.as_deref());
    field(&mut h, Some(&ev.detail));
    h.finalize().into()
}

/// §6.2: `prev_hash` of row 1.
pub fn genesis(store_id: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"ikenga-audit-genesis-v1\0");
    h.update(store_id.as_bytes());
    h.finalize().into()
}

/// A chain head: the newest row's `seq` and `hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Head {
    pub seq: i64,
    pub hash: [u8; 32],
}

/// Where verification failed (§6.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub at_seq: i64,
    pub reason: String,
}

/// A successful walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub rows: i64,
    pub head: Option<Head>,
}

#[derive(Debug)]
pub enum AppendError {
    /// The chain is degraded and this event is an access change (§6.4).
    AuditUnavailable(Broken),
    /// Not a §6.5 kind, or a `via` outside §6.1.
    InvalidEvent(String),
    Sql(sqlx::Error),
}

impl std::fmt::Display for AppendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppendError::AuditUnavailable(b) => write!(
                f,
                "audit_unavailable: the audit chain is broken at #{} — access changes are paused",
                b.at_seq
            ),
            AppendError::InvalidEvent(why) => write!(f, "internal: audit event refused: {why}"),
            AppendError::Sql(e) => write!(f, "internal: audit append failed: {e}"),
        }
    }
}

impl std::error::Error for AppendError {}

impl From<sqlx::Error> for AppendError {
    fn from(e: sqlx::Error) -> Self {
        AppendError::Sql(e)
    }
}

#[derive(Debug, Default)]
struct ChainState {
    /// The newest row this process has verified or written.
    head: Option<Head>,
    degraded: Option<Broken>,
}

/// The in-process view of one store's chain (§6.3 step 2's "in-memory head").
#[derive(Debug)]
pub struct Chain {
    store_id: String,
    state: Mutex<ChainState>,
}

impl Chain {
    pub fn new(store_id: impl Into<String>) -> Self {
        Chain {
            store_id: store_id.into(),
            state: Mutex::new(ChainState::default()),
        }
    }

    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ChainState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn head(&self) -> Option<Head> {
        self.lock().head
    }

    /// `Some` while the chain is degraded (§6.4).
    pub fn degraded(&self) -> Option<Broken> {
        self.lock().degraded.clone()
    }

    /// §6.3 step 4: after the transaction that appended `head` committed.
    /// Never moves the head backwards.
    pub fn committed(&self, head: Head) {
        let mut s = self.lock();
        if s.head.map_or(true, |h| h.seq < head.seq) {
            s.head = Some(head);
        }
    }

    fn set_degraded(&self, broken: Broken) {
        let mut s = self.lock();
        if s.degraded.is_none() {
            tracing::error!(
                "audit chain broken at #{}: {} — access changes are paused",
                broken.at_seq,
                broken.reason
            );
            s.degraded = Some(broken);
        }
    }

    /// The boot walk (§6.4: every start of the T0 daemon or T1 broker). On
    /// success the verified head becomes the in-memory head. On failure the
    /// chain is degraded and `audit.chain_broken` is appended after the DB
    /// head, in its own transaction.
    pub async fn boot_verify(&self, conn: &mut SqliteConnection) -> Result<Verified, Broken> {
        match verify(conn, &self.store_id).await {
            Ok(v) => {
                if let Some(h) = v.head {
                    self.committed(h);
                }
                Ok(v)
            }
            Err(broken) => {
                self.set_degraded(broken.clone());
                let ev = Event::new("audit.chain_broken", "system").detail(&serde_json::json!({
                    "broken_at_seq": broken.at_seq,
                    "reason": broken.reason,
                }));
                let res = async {
                    let mut tx = sqlx::Connection::begin(&mut *conn).await?;
                    append(&mut tx, self, ev).await?;
                    tx.commit().await?;
                    Ok::<_, AppendError>(())
                }
                .await;
                if let Err(e) = res {
                    tracing::error!("could not record audit.chain_broken: {e}");
                }
                Err(broken)
            }
        }
    }
}

async fn db_head(conn: &mut SqliteConnection) -> Result<Option<Head>, sqlx::Error> {
    let row = sqlx::query("SELECT seq, hash FROM audit_events ORDER BY seq DESC LIMIT 1")
        .fetch_optional(&mut *conn)
        .await?;
    Ok(match row {
        Some(r) => Some(Head {
            seq: r.get(0),
            hash: to32(r.get::<Vec<u8>, _>(1))?,
        }),
        None => None,
    })
}

fn to32(v: Vec<u8>) -> Result<[u8; 32], sqlx::Error> {
    v.try_into()
        .map_err(|_| sqlx::Error::Decode("audit hash is not 32 bytes".into()))
}

const SELECT_ROWS: &str = "SELECT seq, at_ms, kind, category, principal_id, device_id, via, \
     subject_principal_id, subject_device_id, project_key, target, remote_addr, user_agent, \
     detail, prev_hash, hash FROM audit_events";

fn decode(r: &sqlx::sqlite::SqliteRow) -> Result<StoredRow, sqlx::Error> {
    Ok(StoredRow {
        seq: r.try_get(0)?,
        event: Event {
            at_ms: r.try_get(1)?,
            kind: r.try_get(2)?,
            principal_id: r.try_get(4)?,
            device_id: r.try_get(5)?,
            via: r.try_get(6)?,
            subject_principal_id: r.try_get(7)?,
            subject_device_id: r.try_get(8)?,
            project_key: r.try_get(9)?,
            target: r.try_get(10)?,
            remote_addr: r.try_get(11)?,
            user_agent: r.try_get(12)?,
            detail: r.try_get(13)?,
        },
        category: r.try_get(3)?,
        prev_hash: to32(r.try_get(14)?)?,
        hash: to32(r.try_get(15)?)?,
    })
}

/// Rows with `seq > after`, in order.
pub async fn rows_after(
    conn: &mut SqliteConnection,
    after: i64,
) -> Result<Vec<StoredRow>, sqlx::Error> {
    let rows = sqlx::query(&format!("{SELECT_ROWS} WHERE seq > ? ORDER BY seq"))
        .bind(after)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter().map(decode).collect()
}

/// Check `rows` link from `prev` at `expect_seq`, recomputing each hash.
fn walk(rows: &[StoredRow], mut prev: [u8; 32], mut expect_seq: i64) -> Result<(), Broken> {
    for row in rows {
        let broken = |reason: &str| Broken {
            at_seq: row.seq,
            reason: reason.to_string(),
        };
        if row.seq != expect_seq {
            return Err(Broken {
                at_seq: expect_seq,
                reason: format!("row #{expect_seq} is missing (found #{})", row.seq),
            });
        }
        if row.prev_hash != prev {
            return Err(broken("prev_hash does not link to the previous row"));
        }
        if category_for(&row.event.kind) != Some(row.category.as_str()) {
            return Err(broken("kind/category is not a §6.5 pair"));
        }
        if row_hash(&prev, row.seq, &row.event, &row.category) != row.hash {
            return Err(broken("hash does not match the row"));
        }
        prev = row.hash;
        expect_seq += 1;
    }
    Ok(())
}

/// §6.4: the full walk from the genesis. Row 1 must be `store.created`.
pub async fn verify(conn: &mut SqliteConnection, store_id: &str) -> Result<Verified, Broken> {
    let rows = rows_after(conn, 0).await.map_err(|e| Broken {
        at_seq: 0,
        reason: format!("unreadable: {e}"),
    })?;
    match rows.first() {
        None => {
            return Err(Broken {
                at_seq: 1,
                reason: "the chain is empty (no store.created row)".into(),
            })
        }
        Some(first) if first.event.kind != "store.created" => {
            return Err(Broken {
                at_seq: first.seq,
                reason: "row 1 is not store.created".into(),
            })
        }
        _ => {}
    }
    walk(&rows, genesis(store_id), 1)?;
    let last = rows.last().expect("non-empty");
    Ok(Verified {
        rows: rows.len() as i64,
        head: Some(Head {
            seq: last.seq,
            hash: last.hash,
        }),
    })
}

/// §6.3 step 2: the DB head moved past the head this process knows (another
/// writer — the T1 root CLI — appended). Adopt it only if the known row is
/// still there unchanged and everything after it verifies.
async fn verify_forward(conn: &mut SqliteConnection, known: Head) -> Result<(), Broken> {
    let row = sqlx::query("SELECT hash FROM audit_events WHERE seq = ?")
        .bind(known.seq)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| Broken {
            at_seq: known.seq,
            reason: format!("unreadable: {e}"),
        })?;
    match row {
        None => {
            return Err(Broken {
                at_seq: known.seq,
                reason: "a row this process verified is missing (head regression)".into(),
            })
        }
        Some(r) => {
            let hash: Vec<u8> = r.get(0);
            if hash.as_slice() != known.hash.as_slice() {
                return Err(Broken {
                    at_seq: known.seq,
                    reason: "a row this process verified has changed".into(),
                });
            }
        }
    }
    let rows = rows_after(conn, known.seq).await.map_err(|e| Broken {
        at_seq: known.seq + 1,
        reason: format!("unreadable: {e}"),
    })?;
    walk(&rows, known.hash, known.seq + 1)
}

/// §6.3: append `ev` inside the caller's transaction (`conn` is that
/// transaction). Returns the new head; call [`Chain::committed`] with it
/// once the transaction commits. If the append fails, the caller's whole
/// transaction must roll back (A-18).
pub async fn append(
    conn: &mut SqliteConnection,
    chain: &Chain,
    ev: Event,
) -> Result<Head, AppendError> {
    let category = category_for(&ev.kind)
        .ok_or_else(|| AppendError::InvalidEvent(format!("unknown kind {:?}", ev.kind)))?;
    if !VIAS.contains(&ev.via.as_str()) {
        return Err(AppendError::InvalidEvent(format!(
            "unknown via {:?}",
            ev.via
        )));
    }

    // 1. The DB head.
    let db = db_head(conn).await?;
    // 2. Compare with what this process knows.
    let (known, degraded) = {
        let s = chain.lock();
        (s.head, s.degraded.clone())
    };
    let broken = match (degraded, known) {
        (Some(b), _) => Some(b),
        (None, Some(k)) if Some(k) == db => None,
        (None, Some(k)) => match verify_forward(conn, k).await {
            Ok(()) => None,
            Err(b) => {
                chain.set_degraded(b.clone());
                Some(b)
            }
        },
        // Nothing known yet in this process (a CLI write): walk it all.
        (None, None) => match db {
            None => None,
            Some(_) => match verify(conn, &chain.store_id).await {
                Ok(_) => None,
                Err(b) => {
                    chain.set_degraded(b.clone());
                    Some(b)
                }
            },
        },
    };
    if let Some(b) = broken {
        if !continues_when_degraded(&ev.kind) {
            return Err(AppendError::AuditUnavailable(b));
        }
        // Degraded: chain to the DB head as it stands, so the rows from the
        // break onward stay internally valid and the break stays visible.
    }

    // 3. Chain to the DB head.
    let (seq, prev) = match db {
        Some(h) => (h.seq + 1, h.hash),
        None => (1, genesis(&chain.store_id)),
    };
    if seq == 1 && ev.kind != "store.created" {
        return Err(AppendError::InvalidEvent(
            "row 1 must be store.created".into(),
        ));
    }
    let hash = row_hash(&prev, seq, &ev, category);
    sqlx::query(
        "INSERT INTO audit_events (seq, at_ms, kind, category, principal_id, device_id, via, \
         subject_principal_id, subject_device_id, project_key, target, remote_addr, user_agent, \
         detail, prev_hash, hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(seq)
    .bind(ev.at_ms)
    .bind(&ev.kind)
    .bind(category)
    .bind(&ev.principal_id)
    .bind(&ev.device_id)
    .bind(&ev.via)
    .bind(&ev.subject_principal_id)
    .bind(&ev.subject_device_id)
    .bind(&ev.project_key)
    .bind(&ev.target)
    .bind(&ev.remote_addr)
    .bind(&ev.user_agent)
    .bind(&ev.detail)
    .bind(prev.as_slice())
    .bind(hash.as_slice())
    .execute(&mut *conn)
    .await?;
    Ok(Head { seq, hash })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    async fn store() -> (SqliteConnection, Chain) {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/0001_core.sql"))
            .execute(&mut conn)
            .await
            .unwrap();
        let chain = Chain::new("0190a000-0000-7000-8000-000000000001");
        let mut tx = conn.begin().await.unwrap();
        let h = append(&mut tx, &chain, Event::new("store.created", "system"))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        chain.committed(h);
        (conn, chain)
    }

    async fn push(
        conn: &mut SqliteConnection,
        chain: &Chain,
        kind: &str,
    ) -> Result<Head, AppendError> {
        let mut tx = conn.begin().await.unwrap();
        let ev = Event::new(kind, "operator")
            .actor(Some("p".into()), Some("d".into()))
            .target("Pixel 9 · Chrome")
            .detail(&serde_json::json!({"tier": "view"}));
        let h = append(&mut tx, chain, ev).await?;
        tx.commit().await.unwrap();
        chain.committed(h);
        Ok(h)
    }

    #[test]
    fn every_kind_has_a_d05_category_and_unknowns_are_refused() {
        for (kind, cat) in KINDS {
            assert!(
                ["permission", "dispatch", "access", "pairing", "people"].contains(cat),
                "{kind}"
            );
        }
        assert_eq!(category_for("pair.allowed"), Some("pairing"));
        assert_eq!(category_for("nope"), None);
        assert!(continues_when_degraded("auth.login_ok"));
        assert!(continues_when_degraded("permission.decided"));
        assert!(continues_when_degraded("share.artifact_viewed"));
        assert!(!continues_when_degraded("device.revoked"));
        assert!(!continues_when_degraded("pair.started"));
        assert!(!continues_when_degraded("invite.issued"));
    }

    /// P-13: the framing is fixed; NULL and empty are distinct.
    #[test]
    fn the_hash_framing_separates_null_empty_and_fields() {
        let prev = genesis("s");
        let a = Event::new("pair.started", "operator");
        let mut b = a.clone();
        b.target = Some(String::new());
        assert_ne!(
            row_hash(&prev, 2, &a, "pairing"),
            row_hash(&prev, 2, &b, "pairing")
        );
        let mut c = a.clone();
        c.principal_id = Some("x".into());
        let mut d = a.clone();
        d.device_id = Some("x".into());
        assert_ne!(
            row_hash(&prev, 2, &c, "pairing"),
            row_hash(&prev, 2, &d, "pairing")
        );
        assert_ne!(
            row_hash(&prev, 2, &a, "pairing"),
            row_hash(&prev, 3, &a, "pairing")
        );
        assert_ne!(genesis("a"), genesis("b"));
    }

    /// A-16: the chain verifies after N appends.
    #[tokio::test]
    async fn the_chain_verifies_after_many_appends() {
        let (mut conn, chain) = store().await;
        for i in 0..50 {
            let kind = if i % 2 == 0 {
                "pair.started"
            } else {
                "device.revoked"
            };
            push(&mut conn, &chain, kind).await.unwrap();
        }
        let v = verify(&mut conn, chain.store_id()).await.unwrap();
        assert_eq!(v.rows, 51);
        assert_eq!(v.head, chain.head());
        assert!(
            verify(&mut conn, "another-store").await.is_err(),
            "genesis binds store_id"
        );
    }

    /// A-15: UPDATE and DELETE abort.
    #[tokio::test]
    async fn audit_rows_are_append_only() {
        let (mut conn, chain) = store().await;
        push(&mut conn, &chain, "pair.started").await.unwrap();
        for stmt in [
            "UPDATE audit_events SET target = 'x' WHERE seq = 2",
            "DELETE FROM audit_events WHERE seq = 2",
            "DELETE FROM audit_events",
        ] {
            let err = sqlx::query(stmt).execute(&mut conn).await.unwrap_err();
            assert!(err.to_string().contains("append-only"), "{stmt}: {err}");
        }
    }

    async fn drop_triggers(conn: &mut SqliteConnection) {
        sqlx::raw_sql("DROP TRIGGER audit_events_no_update; DROP TRIGGER audit_events_no_delete;")
            .execute(&mut *conn)
            .await
            .unwrap();
    }

    /// A-16: flipping any byte of any row, or deleting a middle row, fails
    /// verification at that seq.
    #[tokio::test]
    async fn tampering_is_found_at_the_tampered_seq() {
        for (stmt, at) in [
            (
                "UPDATE audit_events SET target = 'Pixel 8' WHERE seq = 4",
                4,
            ),
            (
                "UPDATE audit_events SET detail = '{\"tier\":\"full\"}' WHERE seq = 3",
                3,
            ),
            ("UPDATE audit_events SET at_ms = at_ms + 1 WHERE seq = 5", 5),
            (
                "UPDATE audit_events SET principal_id = NULL WHERE seq = 2",
                2,
            ),
            (
                "UPDATE audit_events SET category = 'people' WHERE seq = 2",
                2,
            ),
            ("DELETE FROM audit_events WHERE seq = 3", 3),
            (
                "UPDATE audit_events SET hash = zeroblob(32) WHERE seq = 5",
                5,
            ),
        ] {
            let (mut conn, chain) = store().await;
            for _ in 0..5 {
                push(&mut conn, &chain, "pair.started").await.unwrap();
            }
            drop_triggers(&mut conn).await;
            sqlx::query(stmt).execute(&mut conn).await.unwrap();
            let b = verify(&mut conn, chain.store_id()).await.unwrap_err();
            assert_eq!(b.at_seq, at, "{stmt}: {}", b.reason);
        }
    }

    /// A-16: a head regression (the newest known row deleted) is detected
    /// by the running process, and access changes then stop (§6.4).
    #[tokio::test]
    async fn a_head_regression_degrades_the_running_process() {
        let (mut conn, chain) = store().await;
        for _ in 0..3 {
            push(&mut conn, &chain, "pair.started").await.unwrap();
        }
        assert_eq!(chain.head().unwrap().seq, 4);
        drop_triggers(&mut conn).await;
        sqlx::query("DELETE FROM audit_events WHERE seq = 4")
            .execute(&mut conn)
            .await
            .unwrap();
        // The walk alone can't see tail truncation (N-9) …
        assert!(verify(&mut conn, chain.store_id()).await.is_ok());
        // … but the process that knew #4 does.
        let err = push(&mut conn, &chain, "device.revoked").await.unwrap_err();
        assert!(
            matches!(err, AppendError::AuditUnavailable(ref b) if b.at_seq == 4),
            "{err}"
        );
        assert!(chain.degraded().is_some());
        // Authentication and decisions continue, chained to the DB head.
        let h = push(&mut conn, &chain, "auth.login_ok").await.unwrap();
        assert_eq!(h.seq, 4);
        let err = push(&mut conn, &chain, "pair.started").await.unwrap_err();
        assert!(matches!(err, AppendError::AuditUnavailable(_)));
    }

    /// §6.3 step 2 / P-33: another writer's valid rows are adopted.
    #[tokio::test]
    async fn another_writers_valid_rows_are_adopted() {
        let (mut conn, chain) = store().await;
        push(&mut conn, &chain, "pair.started").await.unwrap();
        // "The root CLI": a second Chain over the same store.
        let cli = Chain::new(chain.store_id());
        push(&mut conn, &cli, "auth.sessions_revoked")
            .await
            .unwrap();
        push(&mut conn, &cli, "device.revoked").await.unwrap();
        let h = push(&mut conn, &chain, "device.tier_changed")
            .await
            .unwrap();
        assert_eq!(h.seq, 5);
        assert!(chain.degraded().is_none());
        assert!(verify(&mut conn, chain.store_id()).await.is_ok());
    }

    /// A-18 at the chain level: a rolled-back transaction leaves no row and
    /// does not move the head.
    #[tokio::test]
    async fn a_rolled_back_append_leaves_nothing() {
        let (mut conn, chain) = store().await;
        let before = chain.head();
        {
            let mut tx = conn.begin().await.unwrap();
            append(&mut tx, &chain, Event::new("pair.started", "operator"))
                .await
                .unwrap();
            tx.rollback().await.unwrap();
        }
        assert_eq!(chain.head(), before);
        let v = verify(&mut conn, chain.store_id()).await.unwrap();
        assert_eq!(v.rows, 1);
        assert!(matches!(
            push(&mut conn, &chain, "no.such.kind").await,
            Err(AppendError::InvalidEvent(_))
        ));
    }

    #[tokio::test]
    async fn boot_verify_degrades_and_records_the_break() {
        let (mut conn, chain) = store().await;
        push(&mut conn, &chain, "pair.started").await.unwrap();
        drop_triggers(&mut conn).await;
        sqlx::query("UPDATE audit_events SET target = 'x' WHERE seq = 2")
            .execute(&mut conn)
            .await
            .unwrap();
        let fresh = Chain::new(chain.store_id());
        let b = fresh.boot_verify(&mut conn).await.unwrap_err();
        assert_eq!(b.at_seq, 2);
        assert_eq!(fresh.degraded(), Some(b));
        let last: String =
            sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(last, "audit.chain_broken");
    }
}
