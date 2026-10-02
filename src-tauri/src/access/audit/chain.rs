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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool};

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
///
/// `class` and `fingerprint` are the break's **evidence** (review B-1): a
/// reseal acknowledges exactly this break — the same class at the same seq
/// over the same stored bytes — never "whatever is broken at #seq". A
/// different break at the same seq (a re-forged row, a gap that now
/// swallows more rows) has different evidence and is outstanding again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub broken_at_seq: i64,
    pub reason: String,
    /// `gap`, `link`, `hash`, `genesis` (joined with `+` when one row fails
    /// several checks), `empty`, `head_missing`, `head_changed`, or
    /// `recorded` (a break only a running process saw, §6.4).
    pub class: String,
    /// What the break looks like in the stored bytes (see [`row_fingerprint`]).
    pub fingerprint: String,
}

impl Broken {
    pub fn new(
        broken_at_seq: i64,
        reason: impl Into<String>,
        class: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            broken_at_seq,
            reason: reason.into(),
            class: class.into(),
            fingerprint: fingerprint.into(),
        }
    }

    /// Whether `other` is the same break (same seq, class and evidence).
    pub fn same_as(&self, other: &Broken) -> bool {
        self.broken_at_seq == other.broken_at_seq
            && self.class == other.class
            && self.fingerprint == other.fingerprint
    }
}

/// The outcome of a full walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub rows: i64,
    pub head: Option<Head>,
    pub broken: Option<Broken>,
    /// `broken_at_seq`s of breaks a reseal acknowledged (the reseal-aware
    /// walk only; the strict walk leaves it empty).
    pub resealed: Vec<i64>,
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

/// A row's evidence (review B-1): its stored `hash` and a digest of every
/// stored column (`prev_hash` included), so any later change to the row
/// changes the fingerprint.
pub(crate) fn row_fingerprint(row: &StoredRow) -> String {
    let anchor = to32(&row.prev_hash).unwrap_or_else(|| Sha256::digest(&row.prev_hash).into());
    format!(
        "{}:{}",
        hex::encode(&row.hash),
        hex::encode(row.compute_hash(&anchor))
    )
}

/// The break at `expected` when the next surviving row is `row` (rows
/// deleted). Its evidence is that next row, so a gap that later swallows
/// more rows is a different break.
pub(crate) fn gap_break(expected: i64, row: &StoredRow) -> Broken {
    Broken::new(
        expected,
        format!(
            "seq gap: expected #{expected}, found #{} (rows deleted)",
            row.seq
        ),
        "gap",
        format!("#{}:{}", row.seq, row_fingerprint(row)),
    )
}

/// The row-level checks of §6.4 against `prev`, the hash the row should
/// link to: its link, its own hash (recomputed over the `prev_hash` it
/// stores, so a bad link doesn't hide a forged body), and row 1's kind.
/// One `Broken` per row, every failing check in its class. The strict
/// walk and the reseal-aware walk share it, so a break recorded by one is
/// recognised by the other.
pub(crate) fn inspect_row(row: &StoredRow, prev: &[u8; 32]) -> Option<Broken> {
    let mut class: Vec<&str> = Vec::new();
    let mut reason: Vec<&str> = Vec::new();
    if row.prev_hash.as_slice() != prev.as_slice() {
        class.push("link");
        reason.push("prev_hash does not link to the previous row");
    }
    let anchor = to32(&row.prev_hash).unwrap_or(*prev);
    if row.hash.as_slice() != row.compute_hash(&anchor).as_slice() {
        class.push("hash");
        reason.push("row hash does not match its contents");
    }
    if row.seq == 1 && row.kind != "store.created" {
        class.push("genesis");
        reason.push("row 1 is not store.created");
    }
    (!class.is_empty()).then(|| {
        Broken::new(
            row.seq,
            reason.join("; "),
            class.join("+"),
            row_fingerprint(row),
        )
    })
}

/// The break of a migrated store whose every row is gone (§6.2).
pub(crate) fn empty_break() -> Broken {
    Broken::new(
        1,
        "row 1 (store.created) is missing: the chain is empty",
        "empty",
        "",
    )
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
        let broken = |b: Broken| VerifyReport {
            rows: count,
            head,
            broken: Some(b),
            resealed: Vec::new(),
        };
        if row.seq != expected_seq {
            return Ok(broken(gap_break(expected_seq, &row)));
        }
        if let Some(b) = inspect_row(&row, &prev) {
            return Ok(broken(b));
        }
        prev = row.compute_hash(&prev);
        head = Some(Head {
            seq: row.seq,
            hash: prev,
        });
        expected_seq += 1;
        count += 1;
    }
    Ok(VerifyReport {
        rows: count,
        head,
        broken: None,
        resealed: Vec::new(),
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
                broken: Some(empty_break()),
                resealed: Vec::new(),
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
    // §6.5 is a closed list (review m-6): a kind outside it never reaches
    // the chain, in release builds too.
    let Some(category) = super::category_of(ev.kind) else {
        return Err(sqlx::Error::Protocol(format!(
            "audit kind `{}` is not in the closed list (G-ACCESS §6.5)",
            ev.kind
        )));
    };
    let (seq, prev) = match head {
        Some(h) => (h.seq + 1, h.hash),
        None => (1, genesis(store_id)),
    };
    let row = StoredRow {
        seq,
        at_ms,
        kind: ev.kind.to_string(),
        category: category.as_str().to_string(),
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

/// The degraded state, under one lock (review m-2): readers never see a
/// transient `None` while a re-verify recomputes it.
///
/// Invariant (review M-3): `unrecorded` non-empty ⇒ `degraded` is `Some`.
/// A break leaves `unrecorded` only once its `audit.chain_broken` row is
/// seen committed in the chain, so nothing — a refused or rolled-back
/// append, a re-verify whose walk can't see the break, a reseal of another
/// break — can drop a break this process found.
#[derive(Debug, Default)]
struct State {
    degraded: Option<Broken>,
    /// Breaks this process found whose `audit.chain_broken` row isn't known
    /// to be committed yet (a refused append rolled back, or the row was
    /// written inside a caller's transaction that may still roll back).
    unrecorded: Vec<Broken>,
    /// The newest `audit.resealed` seq step 0 has already re-verified for.
    reseal_checked: i64,
}

/// The process's view of one store's chain: the last head it committed and
/// whether it is degraded (§6.3, §6.4).
#[derive(Debug)]
pub struct Chain {
    store_id: String,
    known: Mutex<Option<Head>>,
    state: Mutex<State>,
    /// Serialises the out-of-transaction `audit.chain_broken` writes and
    /// the re-verify that follows one (review M-3): a walk never runs while
    /// a recording is in flight.
    flush_lock: tokio::sync::Mutex<()>,
    /// Set by [`Chain::attach`]: the store's pool (to persist a break found
    /// inside a refused append, review M-2) and this chain's own `Arc`.
    attached: OnceLock<(SqlitePool, Weak<Chain>)>,
}

/// The process-wide chains, by `store_id` (review m-1): every appender in
/// a process — the access arms and `operator::auth_events::record` — goes
/// through the one [`Chain`] its [`crate::access::AccessStore`] holds.
fn registry() -> &'static Mutex<HashMap<String, Weak<Chain>>> {
    static CHAINS: OnceLock<Mutex<HashMap<String, Weak<Chain>>>> = OnceLock::new();
    CHAINS.get_or_init(Default::default)
}

/// The chain this process holds for `store_id`, or a fresh view when no
/// store is open on it here (the root CLI's `accounts …`).
pub fn shared(store_id: &str) -> Arc<Chain> {
    let mut map = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = map.get(store_id).and_then(Weak::upgrade) {
        return c;
    }
    map.retain(|_, w| w.strong_count() > 0);
    Arc::new(Chain::new(store_id))
}

/// The newest `audit.resealed` row after `b.broken_at_seq`, if any: another
/// process (the T1 root CLI) may reseal while this one is degraded.
async fn newest_reseal_after(
    conn: &mut SqliteConnection,
    b: &Broken,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT max(seq) FROM audit_events WHERE seq > ? AND kind = 'audit.resealed'",
    )
    .bind(b.broken_at_seq)
    .fetch_one(&mut *conn)
    .await
}

/// Whether the chain already holds a committed (or, inside a transaction,
/// this transaction's) `audit.chain_broken` row recording exactly `b` that
/// is **in** the chain: it links to the stored hash of the row before it
/// (genesis for row 1) and verifies against it (review m-11). A row that
/// is only consistent with its own `prev_hash` — say one planted after a
/// tail truncation with the detail this process would write — doesn't
/// count, so the process records the break itself. A rolled-back row is
/// not seen either, so the break stays to be recorded again.
async fn is_recorded(
    conn: &mut SqliteConnection,
    store_id: &str,
    b: &Broken,
) -> Result<bool, sqlx::Error> {
    let detail = broken_detail(b);
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM audit_events WHERE kind = 'audit.chain_broken' AND detail = ? \
         ORDER BY seq"
    ))
    .bind(&detail)
    .fetch_all(&mut *conn)
    .await?;
    for r in &rows {
        let row = StoredRow::from_sql(r)?;
        let pred = if row.seq == 1 {
            Some(genesis(store_id))
        } else {
            sqlx::query_scalar::<_, Vec<u8>>("SELECT hash FROM audit_events WHERE seq = ?")
                .bind(row.seq - 1)
                .fetch_optional(&mut *conn)
                .await?
                .and_then(|h| to32(&h))
        };
        if pred.is_some_and(|p| inspect_row(&row, &p).is_none()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The break the process's known head shows against the DB head `db`
/// (§6.3 step 2), or `None`. A head regression is invisible to a walk from
/// genesis (§6.4 "Tail truncation"), so this is checked before every
/// append **and** before every re-verify adopts a verdict (review M-3).
///
/// * The known row is gone: `head_missing`, at the **first missing seq**
///   (`db.seq + 1` on a tail truncation — the seq the `audit.chain_broken`
///   row recording it takes), evidenced by the known head itself
///   (`#seq:hash`).
/// * The known row changed: `head_changed` at its seq.
/// * With `forward`, the rows after it must link and hash (another
///   writer's appends, P-33).
async fn known_head_break(
    conn: &mut SqliteConnection,
    store_id: &str,
    known: Option<Head>,
    db: Option<Head>,
    forward: bool,
) -> Result<Option<Broken>, sqlx::Error> {
    let Some(k) = known else {
        // Nothing known yet (a fresh process, e.g. the root CLI): the boot
        // walk is the integrity check.
        return Ok(None);
    };
    if Some(k) == db {
        return Ok(None);
    }
    let still_there = sqlx::query("SELECT hash FROM audit_events WHERE seq = ?")
        .bind(k.seq)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| r.get::<Vec<u8>, _>(0));
    Ok(match still_there {
        Some(h) if h.as_slice() == k.hash.as_slice() => {
            if forward {
                walk(conn, store_id, Some(k)).await?.broken
            } else {
                None
            }
        }
        Some(h) => Some(Broken::new(
            k.seq,
            "a row this process wrote has changed",
            "head_changed",
            format!("{}:{}", hex::encode(k.hash), hex::encode(h)),
        )),
        None => {
            let first_missing = db.map_or(1, |d| d.seq + 1).min(k.seq);
            Some(Broken::new(
                first_missing,
                format!(
                    "rows #{first_missing}..#{} this process wrote are missing (head regression)",
                    k.seq
                ),
                "head_missing",
                format!("#{}:{}", k.seq, hex::encode(k.hash)),
            ))
        }
    })
}

/// Whether `class` is a break only a running process can see (its known
/// head), never the walk from genesis.
pub(crate) fn is_head_class(class: &str) -> bool {
    matches!(class, "head_missing" | "head_changed")
}

impl Chain {
    pub fn new(store_id: impl Into<String>) -> Self {
        Self {
            store_id: store_id.into(),
            known: Mutex::new(None),
            state: Mutex::new(State::default()),
            flush_lock: tokio::sync::Mutex::new(()),
            attached: OnceLock::new(),
        }
    }

    /// Bind this chain to its store's pool and make it the process's chain
    /// for its `store_id` ([`shared`]). `AccessStore` calls it once.
    pub fn attach(self: &Arc<Self>, pool: SqlitePool) {
        let _ = self.attached.set((pool, Arc::downgrade(self)));
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.store_id.clone(), Arc::downgrade(self));
    }

    pub fn store_id(&self) -> &str {
        &self.store_id
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn degraded(&self) -> Option<Broken> {
        self.state().degraded.clone()
    }

    pub fn known_head(&self) -> Option<Head> {
        *self.known.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_known(&self, head: Option<Head>) {
        *self.known.lock().unwrap_or_else(|e| e.into_inner()) = head;
    }

    /// A break this process found: enter `degraded` (the first break stays
    /// the reported one) and queue its `audit.chain_broken` row unless the
    /// chain already holds one. A head break re-bases the known head on the
    /// DB head `db` — the break now carries the old head as its evidence —
    /// so it is found once, and a further regression after it is a new
    /// break.
    async fn note_break(
        &self,
        conn: &mut SqliteConnection,
        b: Broken,
        db: Option<Head>,
    ) -> Result<(), sqlx::Error> {
        let recorded = is_recorded(conn, &self.store_id, &b).await?;
        if !recorded {
            tracing::error!(
                "audit chain broken at #{}: {} — access changes are paused (G-ACCESS §6.4)",
                b.broken_at_seq,
                b.reason
            );
        }
        if is_head_class(&b.class) {
            self.set_known(db);
        }
        let mut st = self.state();
        if st.degraded.is_none() {
            st.degraded = Some(b.clone());
        }
        if !recorded && !st.unrecorded.iter().any(|u| u.same_as(&b)) {
            st.unrecorded.push(b);
        }
        Ok(())
    }

    /// Replace the state with a walk's verdict in one step (review m-2),
    /// never dropping a break the walk can't see yet (review M-3): one
    /// still unrecorded keeps the store `degraded`. Adopts the walk's head
    /// only when nothing is outstanding.
    fn adopt(&self, verdict: &super::verify_boot::Verdict) {
        let outstanding = verdict.outstanding.as_ref();
        let clean = {
            let mut st = self.state();
            if let Some(b) = outstanding {
                if !verdict.recorded && !st.unrecorded.iter().any(|u| u.same_as(b)) {
                    st.unrecorded.push(b.clone());
                }
            }
            st.degraded = outstanding
                .cloned()
                .or_else(|| st.unrecorded.first().cloned());
            st.degraded.is_none()
        };
        if let Some(b) = outstanding {
            tracing::error!(
                "audit chain broken at #{}{}: {} — access changes are paused (G-ACCESS §6.4)",
                b.broken_at_seq,
                if verdict.recorded {
                    " (recorded, not resealed)"
                } else {
                    ""
                },
                b.reason
            );
        }
        if clean {
            self.set_known(verdict.head);
        }
    }

    /// After the caller's `COMMIT`: remember `head` as ours (§6.3 step 4).
    /// Forgetting to call it is harmless — the next append verifies forward.
    /// Never moves the known head backwards.
    pub fn committed(&self, head: Head) {
        let mut k = self.known.lock().unwrap_or_else(|e| e.into_inner());
        if k.is_none_or(|k| head.seq > k.seq) {
            *k = Some(head);
        }
    }

    /// §6.4 at boot, on `access_audit_verify`, before an export and after a
    /// reseal: the reseal-aware full walk ([`super::verify_boot::verify`]).
    ///
    /// * A break this process found but could not record yet is written
    ///   first, so the walk sees it.
    /// * The process's known head is checked against the DB head (review
    ///   M-3): a head regression is invisible to the walk, so it is
    ///   recorded before the walk rather than lost to a clean verdict.
    /// * An outstanding break enters `degraded`; its `audit.chain_broken`
    ///   row is appended after the DB head in its own transaction unless
    ///   the chain already records it (a restart doesn't repeat the row).
    /// * Nothing outstanding and nothing unrecorded clears `degraded` and
    ///   adopts the head.
    ///
    /// `conn` must not be inside a transaction.
    pub async fn verify_boot(&self, conn: &mut SqliteConnection) -> anyhow::Result<VerifyReport> {
        let _flushing = self.flush_lock.lock().await;
        self.flush_locked(conn).await?;
        let db = db_head(conn).await?;
        if let Some(b) =
            known_head_break(conn, &self.store_id, self.known_head(), db, false).await?
        {
            self.note_break(conn, b, db).await?;
            self.flush_locked(conn).await?;
        }
        let verdict = super::verify_boot::verify(conn, &self.store_id).await?;
        self.adopt(&verdict);
        self.flush_locked(conn).await?;
        Ok(verdict.report())
    }

    /// Write every unrecorded `audit.chain_broken` row the chain doesn't
    /// already hold, chained to the DB head, in one `BEGIN IMMEDIATE`, and
    /// forget them once committed. On failure they stay unrecorded.
    /// Callers hold `flush_lock`.
    async fn flush_locked(&self, conn: &mut SqliteConnection) -> anyhow::Result<()> {
        let pending = self.state().unrecorded.clone();
        if pending.is_empty() {
            return Ok(());
        }
        use sqlx::Connection;
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
        let mut head = db_head(&mut tx).await?;
        let mut wrote = None;
        for b in &pending {
            if !is_recorded(&mut tx, &self.store_id, b).await? {
                let h = insert_row(&mut tx, &self.store_id, head, &broken_event(b)).await?;
                head = Some(h);
                wrote = Some(h);
            }
        }
        tx.commit().await?;
        if let Some(h) = wrote {
            self.committed(h);
        }
        self.state()
            .unrecorded
            .retain(|u| !pending.iter().any(|p| p.same_as(u)));
        Ok(())
    }

    /// Review M-2: a break found by an append that is then refused must
    /// not live only in memory (the T0 daemon idle-exits; a crash or a
    /// broker restart loses it, and a head regression is invisible to the
    /// next boot's walk). Persist it on a fresh pool connection once the
    /// caller's transaction is gone (`busy_timeout` covers the wait).
    fn spawn_flush(&self) {
        let Some((pool, me)) = self.attached.get() else {
            return;
        };
        let (Some(me), Ok(rt)) = (me.upgrade(), tokio::runtime::Handle::try_current()) else {
            return;
        };
        let pool = pool.clone();
        rt.spawn(async move {
            let res = async {
                // Connection first, then the lock — the order every
                // `verify_boot` caller takes them in (a one-connection pool
                // would deadlock otherwise).
                let mut conn = pool.acquire().await?;
                let _flushing = me.flush_lock.lock().await;
                me.flush_locked(&mut conn).await
            }
            .await;
            if let Err(e) = res {
                tracing::warn!("audit.chain_broken not recorded yet (stays pending): {e:#}");
            }
        });
    }

    /// Step 0 while degraded: has another process (the T1 root CLI)
    /// resealed since? Re-run the reseal-aware walk once per new
    /// `audit.resealed` row and adopt its verdict — after checking the
    /// known head, which the walk can't (review M-3). A break still
    /// unrecorded keeps the store degraded ([`adopt`](Self::adopt)).
    async fn recheck_after_reseal(
        &self,
        conn: &mut SqliteConnection,
        b: &Broken,
    ) -> Result<(), sqlx::Error> {
        let Some(at) = newest_reseal_after(conn, b).await? else {
            return Ok(());
        };
        if at <= self.state().reseal_checked {
            return Ok(());
        }
        let db = db_head(conn).await?;
        if let Some(hb) =
            known_head_break(conn, &self.store_id, self.known_head(), db, false).await?
        {
            self.note_break(conn, hb, db).await?;
        }
        let verdict = super::verify_boot::verify(conn, &self.store_id).await?;
        self.state().reseal_checked = at;
        self.adopt(&verdict);
        if self.degraded().is_none() {
            tracing::info!(
                "audit chain: the break at #{} was resealed at #{at}; access changes resume",
                b.broken_at_seq
            );
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
        // 0. Degraded: resealed by another process since?
        if let Some(b) = self.degraded() {
            self.recheck_after_reseal(conn, &b).await?;
        }
        // 1. The DB head.
        let db = db_head(conn).await?;
        // 2. Against the head this process holds — degraded or not, so a
        //    regression while degraded is a break of its own (review M-3).
        if let Some(b) = known_head_break(conn, &self.store_id, self.known_head(), db, true).await?
        {
            self.note_break(conn, b, db).await?;
        }
        if let Some(b) = self.degraded() {
            if ev.refused_when_degraded() {
                // The caller rolls back; record the break on its own.
                if !self.state().unrecorded.is_empty() {
                    self.spawn_flush();
                }
                return Err(AppendError::AuditUnavailable(b));
            }
            // Degraded but allowed: chain to the DB head (the break stays
            // visible), writing the unrecorded chain_broken rows first. They
            // stay unrecorded until seen committed — the caller may still
            // roll back — so a flush confirms them once the caller's
            // transaction is gone (or writes them again after a rollback).
            let mut head = db;
            let pending = self.state().unrecorded.clone();
            let mut wrote = false;
            for pb in &pending {
                if !is_recorded(conn, &self.store_id, pb).await? {
                    head = Some(insert_row(conn, &self.store_id, head, &broken_event(pb)).await?);
                    wrote = true;
                }
            }
            let new_head = insert_row(conn, &self.store_id, head, ev).await?;
            if wrote {
                self.spawn_flush();
            }
            return Ok(new_head);
        }
        // 3. Insert chained to the (verified) DB head.
        Ok(insert_row(conn, &self.store_id, db, ev).await?)
    }
}

fn broken_fields(b: &Broken) -> serde_json::Value {
    serde_json::json!({
        "broken_at_seq": b.broken_at_seq,
        "reason": b.reason,
        "class": b.class,
        "fingerprint": b.fingerprint,
    })
}

/// `audit.chain_broken {broken_at_seq, reason, class, fingerprint}` (§6.4;
/// the evidence fields bind a later reseal to this break, review B-1).
pub(crate) fn broken_event(b: &Broken) -> Event {
    Event::new("audit.chain_broken", super::AuditVia::System).detail(broken_fields(b))
}

/// The `detail` column [`broken_event`] stores for `b` (as `insert_row`
/// serialises it), to find a row that already records `b`.
fn broken_detail(b: &Broken) -> String {
    serde_json::to_string(&broken_event(b).detail).unwrap_or_else(|_| "{}".into())
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

    /// Review m-6: the closed kind list (§6.5) holds in release builds too —
    /// a kind outside it never reaches the chain.
    #[tokio::test]
    async fn a_kind_outside_the_closed_list_is_refused() {
        let mut conn = store().await;
        let mut ev = Event::new("store.created", AuditVia::System);
        ev.kind = "made.up";
        let err = insert_row(&mut conn, "s", None, &ev).await.unwrap_err();
        assert!(err.to_string().contains("closed list"), "{err}");
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    /// Review B-1: the strict walk and the reseal-aware walk describe a
    /// break identically, so a break the runtime recorded is recognised at
    /// boot (no second `audit.chain_broken` row).
    #[tokio::test]
    async fn both_walks_agree_on_a_breaks_evidence() {
        let mut conn = store().await;
        let chain = Chain::new("s");
        seed(&mut conn, &chain, 4).await;
        drop_triggers(&mut conn).await;
        sqlx::query(
            "UPDATE audit_events SET target = 'x', prev_hash = randomblob(32) WHERE seq = 3",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        let strict = verify_all(&mut conn, "s").await.unwrap().broken.unwrap();
        let full = super::super::verify_boot::verify(&mut conn, "s")
            .await
            .unwrap()
            .outstanding
            .unwrap();
        assert_eq!(strict, full);
        assert_eq!(strict.class, "link+hash");
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
