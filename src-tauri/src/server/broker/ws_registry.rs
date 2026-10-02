//! The open-socket registry (G-PRINCIPAL §2.2, I-8; G-ACCESS R-5).
//!
//! A PTY/chat/fs WebSocket authenticates once, at its handshake, so a
//! request-time check alone would let an open socket outlive a revocation.
//! Every socket the broker proxies is registered here under the credential it
//! was opened with — `(principal_id, session_id, session_epoch, device_id?,
//! grant_epoch?)` — and is closed with code **4401** when that credential
//! stops being valid:
//!
//! * **immediately** for changes the broker makes itself: `POST
//!   /auth/logout` closes that session's sockets; `POST /auth/password`
//!   closes the principal's sockets of the old epoch;
//! * **within 2 s** for writes made elsewhere (the root CLI's `accounts
//!   passwd | disable | revoke-sessions`): [`recheck_loop`] polls `PRAGMA
//!   data_version` on its own `accounts.db` connection — it changes whenever
//!   another connection commits — and only then asks a pluggable
//!   [`StillValid`] check about every open socket.
//!
//! **A socket registered after its revocation** (the handshake raced it) is
//! still caught: the registry remembers recently logged-out session ids and
//! each principal's epoch floor, and [`WsRegistry::register`] checks both
//! atomically with inserting the socket — so a socket registered after the
//! broker-side close is born closed. For writes made elsewhere, the proxy
//! runs the [`StillValid`] check once *after* registering: a commit before
//! that check is seen by it, one after it is seen by the loop's next pass.
//!
//! The default check ([`AccountEpochs`]) closes a socket once its account is
//! gone or disabled or its `session_epoch` moved. WP-74 plugs in a check that
//! also compares `grant_epoch`, so a device socket closes on **either** epoch
//! moving (R-5). Closing a socket never kills the PTY or run behind it; the
//! owner reattaches after logging in again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::{Connection, SqliteConnection, SqlitePool};
use tokio::sync::oneshot;

use crate::executor::PrincipalId;
use crate::server::auth::{BoxFuture, Epochs, PrincipalCtx};
use crate::server::operator::accounts;

/// The close code for a revoked credential (§2.2).
pub const CLOSE_REVOKED: u16 = 4401;

/// The re-check bound for writes the broker didn't make (§2.2: "at least
/// every 2 s").
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(1);

/// How long a logged-out session id is remembered for a racing handshake.
/// The proxy registers a socket first thing in its handler, so the window
/// it covers is resolution → handler (no I/O); this is generous.
pub const LOGGED_OUT_TTL: Duration = Duration::from_secs(10 * 60);

/// What a socket was authenticated with, captured at its handshake (R-5).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WsKey {
    pub principal_id: PrincipalId,
    /// The session cookie's id; empty for a credential with no session.
    pub session_id: String,
    /// `accounts.session_epoch` at the handshake.
    pub session_epoch: i64,
    /// WP-74: the paired device, when the credential was a device grant.
    pub device_id: Option<String>,
    /// WP-74: the grant's own revocation epoch.
    pub grant_epoch: Option<i64>,
}

impl WsKey {
    pub fn from_ctx(ctx: &PrincipalCtx, epochs: Epochs) -> Self {
        Self {
            principal_id: ctx.principal.id,
            session_id: ctx.via.session_id().unwrap_or_default().to_string(),
            session_epoch: epochs.session_epoch,
            device_id: ctx.via.device_id().map(str::to_string),
            grant_epoch: epochs.grant_epoch,
        }
    }
}

/// Why a socket is being closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReason {
    pub code: u16,
    pub reason: &'static str,
}

impl CloseReason {
    pub const REVOKED: CloseReason = CloseReason {
        code: CLOSE_REVOKED,
        reason: "credential revoked",
    };
    pub const LOGGED_OUT: CloseReason = CloseReason {
        code: CLOSE_REVOKED,
        reason: "logged out",
    };
    /// G-ACCESS §3.10: the device grant was revoked.
    pub const DEVICE_REVOKED: CloseReason = CloseReason {
        code: CLOSE_REVOKED,
        reason: "device_revoked",
    };
    /// G-ACCESS §3.10 / P-12: the device's tier changed; reconnect.
    pub const CAPS_CHANGED: CloseReason = CloseReason {
        code: crate::access::sockets::CLOSE_CAPS_CHANGED,
        reason: "caps_changed",
    };
}

struct Entry {
    key: WsKey,
    close: Option<oneshot::Sender<CloseReason>>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<u64, Entry>,
    /// Session ids logged out within [`LOGGED_OUT_TTL`].
    logged_out: HashMap<String, Instant>,
    /// Per principal: the lowest `session_epoch` still valid, as last set by
    /// a broker-side epoch bump.
    epoch_floor: HashMap<PrincipalId, i64>,
}

/// Whether a broker-side revocation already covers `key`.
fn revoked(
    logged_out: &HashMap<String, Instant>,
    epoch_floor: &HashMap<PrincipalId, i64>,
    key: &WsKey,
) -> Option<CloseReason> {
    if !key.session_id.is_empty() && logged_out.contains_key(&key.session_id) {
        return Some(CloseReason::LOGGED_OUT);
    }
    match epoch_floor.get(&key.principal_id) {
        Some(floor) if key.session_epoch < *floor => Some(CloseReason::REVOKED),
        _ => None,
    }
}

/// Every proxied socket that is open right now.
#[derive(Default)]
pub struct WsRegistry {
    inner: Mutex<Inner>,
    next: AtomicU64,
}

/// One registered socket. Dropping it unregisters; `closed` resolves when
/// the registry decides the socket must close.
pub struct WsRegistration {
    id: u64,
    registry: Arc<WsRegistry>,
    pub closed: oneshot::Receiver<CloseReason>,
}

impl WsRegistration {
    /// Whether the registry has already told this socket to close.
    pub fn revoked(&mut self) -> Option<CloseReason> {
        self.closed.try_recv().ok()
    }
}

impl Drop for WsRegistration {
    fn drop(&mut self) {
        self.registry.lock().entries.remove(&self.id);
    }
}

impl WsRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Register a socket under the credential it resolved with. If that
    /// credential was already revoked broker-side (a logout of its session,
    /// or an epoch bump past it), the registration is born closed.
    pub fn register(self: &Arc<Self>, key: WsKey) -> WsRegistration {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        let close = match revoked(&inner.logged_out, &inner.epoch_floor, &key) {
            Some(reason) => {
                let _ = tx.send(reason);
                None
            }
            None => Some(tx),
        };
        inner.entries.insert(id, Entry { key, close });
        drop(inner);
        WsRegistration {
            id,
            registry: self.clone(),
            closed: rx,
        }
    }

    /// Open sockets (not yet told to close).
    pub fn len(&self) -> usize {
        self.lock()
            .entries
            .values()
            .filter(|e| e.close.is_some())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Open sockets of one principal (the idle reaper counts them, OD-13).
    pub fn open_for(&self, principal: PrincipalId) -> usize {
        self.lock()
            .entries
            .values()
            .filter(|e| e.close.is_some() && e.key.principal_id == principal)
            .count()
    }

    /// The keys of every open socket, for a validity pass.
    pub fn keys(&self) -> Vec<(u64, WsKey)> {
        self.lock()
            .entries
            .iter()
            .filter(|(_, e)| e.close.is_some())
            .map(|(id, e)| (*id, e.key.clone()))
            .collect()
    }

    fn close_ids(&self, ids: &[u64], reason: &CloseReason) -> usize {
        let mut inner = self.lock();
        let mut closed = 0;
        for id in ids {
            if let Some(tx) = inner.entries.get_mut(id).and_then(|e| e.close.take()) {
                let _ = tx.send(reason.clone());
                closed += 1;
            }
        }
        closed
    }

    /// Close every open socket whose key matches.
    pub fn close_where(&self, reason: CloseReason, pred: impl Fn(&WsKey) -> bool) -> usize {
        let ids: Vec<u64> = self
            .keys()
            .into_iter()
            .filter(|(_, k)| pred(k))
            .map(|(id, _)| id)
            .collect();
        self.close_ids(&ids, &reason)
    }

    /// `POST /auth/logout`: that one session's sockets — and any socket
    /// whose handshake resolved before the logout but registers after it.
    pub fn close_session(&self, session_id: &str) -> usize {
        if session_id.is_empty() {
            return 0;
        }
        self.close_matching(CloseReason::LOGGED_OUT, |inner| {
            let now = Instant::now();
            inner
                .logged_out
                .retain(|_, at| now.duration_since(*at) < LOGGED_OUT_TTL);
            inner.logged_out.insert(session_id.to_string(), now);
        })
    }

    /// A broker-side epoch bump: every socket of `principal` opened under an
    /// older `session_epoch` (§2.2: "carrying the old epoch"), and any
    /// older-epoch socket that registers later. Epochs only grow, so a
    /// floor (never lowered) is exact — and a late call with a stale
    /// `current_epoch` can't close sockets of a newer one.
    pub fn close_stale_epoch(&self, principal: PrincipalId, current_epoch: i64) -> usize {
        self.close_matching(CloseReason::REVOKED, |inner| {
            let floor = inner.epoch_floor.entry(principal).or_insert(current_epoch);
            *floor = (*floor).max(current_epoch);
        })
    }

    /// Record a revocation and close every open socket it covers, under one
    /// lock, so no registration slips between the two.
    fn close_matching(&self, reason: CloseReason, record: impl FnOnce(&mut Inner)) -> usize {
        let mut inner = self.lock();
        record(&mut inner);
        let Inner {
            entries,
            logged_out,
            epoch_floor,
        } = &mut *inner;
        let mut closed = 0;
        for e in entries.values_mut() {
            if e.close.is_some() && revoked(logged_out, epoch_floor, &e.key).is_some() {
                if let Some(tx) = e.close.take() {
                    let _ = tx.send(reason.clone());
                    closed += 1;
                }
            }
        }
        closed
    }

    /// Ask `check` about every open socket and close the invalid ones. A
    /// failed check is logged and retried on the next pass rather than
    /// closing sockets on a transient read error.
    pub async fn recheck(&self, check: &dyn StillValid) -> usize {
        let mut stale = Vec::new();
        for (id, key) in self.keys() {
            match check.still_valid(&key).await {
                Ok(true) => {}
                Ok(false) => stale.push(id),
                Err(e) => tracing::warn!("ws re-check for {}: {e:#}", key.principal_id),
            }
        }
        let closed = self.close_ids(&stale, &CloseReason::REVOKED);
        if closed > 0 {
            tracing::info!("closed {closed} WebSocket(s) whose credential was revoked (4401)");
        }
        closed
    }
}

/// "Is this socket's credential still valid?" (R-5's pluggable check).
pub trait StillValid: Send + Sync {
    fn still_valid<'a>(&'a self, key: &'a WsKey) -> BoxFuture<'a, anyhow::Result<bool>>;
}

/// The default check: the account exists, is enabled, and is still at the
/// socket's `session_epoch`. `grant_epoch` is WP-74's to compare.
#[derive(Clone)]
pub struct AccountEpochs {
    pub pool: SqlitePool,
}

impl StillValid for AccountEpochs {
    fn still_valid<'a>(&'a self, key: &'a WsKey) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let mut conn = self.pool.acquire().await?;
            let account = accounts::by_id(&mut conn, key.principal_id).await?;
            Ok(account.is_some_and(|a| !a.is_disabled() && a.session_epoch == key.session_epoch))
        })
    }
}

/// G-ACCESS R-5: [`AccountEpochs`] plus the device grant. A device socket
/// closes once its account's `session_epoch` moved (passwd, disable, forced
/// logout — I-8 for device sockets, P-31) **or** its grant was revoked or
/// re-tiered (`grant_epoch` moved). Also covers root-CLI writes within 2 s
/// through [`recheck_loop`] (A-33).
#[derive(Clone)]
pub struct AccessStillValid {
    pub pool: SqlitePool,
}

impl StillValid for AccessStillValid {
    fn still_valid<'a>(&'a self, key: &'a WsKey) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let account_ok = AccountEpochs {
                pool: self.pool.clone(),
            }
            .still_valid(key)
            .await?;
            if !account_ok {
                return Ok(false);
            }
            let Some(device_id) = key.device_id.as_deref() else {
                return Ok(true);
            };
            let row: Option<(i64, Option<i64>)> =
                sqlx::query_as("SELECT grant_epoch, revoked_at FROM devices WHERE device_id = ?")
                    .bind(device_id)
                    .fetch_optional(&self.pool)
                    .await?;
            Ok(matches!(
                row,
                Some((epoch, None)) if Some(epoch) == key.grant_epoch
            ))
        })
    }
}

/// The access arms close device sockets through this (G-ACCESS §3.10):
/// in-process, so a broker-side revoke or tier change is immediate.
impl crate::access::sockets::SocketControl for WsRegistry {
    fn device_revoked(&self, device_id: &str) -> usize {
        self.close_where(CloseReason::DEVICE_REVOKED, |k| {
            k.device_id.as_deref() == Some(device_id)
        })
    }

    fn device_caps_changed(&self, device_id: &str, new_epoch: i64) -> usize {
        self.close_where(CloseReason::CAPS_CHANGED, |k| {
            k.device_id.as_deref() == Some(device_id)
                && k.grant_epoch.map_or(true, |e| e < new_epoch)
        })
    }

    fn live_for_device(&self, device_id: &str) -> usize {
        self.keys()
            .iter()
            .filter(|(_, k)| k.device_id.as_deref() == Some(device_id))
            .count()
    }
}

/// `PRAGMA data_version` on `conn`: it changes whenever **another**
/// connection commits to the database (SQLite docs), which is every writer
/// the broker doesn't own — the root CLI — and its own pool's writes too.
pub async fn data_version(conn: &mut SqliteConnection) -> sqlx::Result<i64> {
    sqlx::query_scalar("PRAGMA data_version")
        .fetch_one(conn)
        .await
}

/// The ≤2 s re-check loop. `conn` must be a dedicated connection to
/// `accounts.db` (never one from the pool the broker writes through, or its
/// `data_version` would not see those writes). Runs until `shutdown` fires.
pub async fn recheck_loop(
    registry: Arc<WsRegistry>,
    mut conn: SqliteConnection,
    check: Arc<dyn StillValid>,
    interval: Duration,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    let mut last = data_version(&mut conn).await.ok();
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = shutdown.recv() => break,
        }
        let now = match data_version(&mut conn).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!("ws re-check: PRAGMA data_version: {e}");
                None
            }
        };
        // Unchanged → nothing committed since the last pass. An unreadable
        // version re-checks (fail towards closing, not towards trusting).
        if now.is_some() && now == last {
            continue;
        }
        last = now;
        if !registry.is_empty() {
            registry.recheck(check.as_ref()).await;
        }
    }
    let _ = conn.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn key(p: PrincipalId, session: &str, epoch: i64) -> WsKey {
        WsKey {
            principal_id: p,
            session_id: session.into(),
            session_epoch: epoch,
            device_id: None,
            grant_epoch: None,
        }
    }

    #[test]
    fn registration_counts_and_unregisters_on_drop() {
        let reg = WsRegistry::new();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let r1 = reg.register(key(a, "s1", 0));
        let r2 = reg.register(key(a, "s2", 0));
        let _r3 = reg.register(key(b, "s3", 0));
        assert_eq!(reg.len(), 3);
        assert_eq!(reg.open_for(a), 2);
        drop(r1);
        assert_eq!(reg.open_for(a), 1);
        drop(r2);
        assert_eq!(reg.open_for(a), 0);
        assert_eq!(reg.open_for(b), 1);
    }

    #[tokio::test]
    async fn logout_closes_only_that_session() {
        let reg = WsRegistry::new();
        let a = PrincipalId::new_v7();
        let mut s1 = reg.register(key(a, "s1", 0));
        let mut s2 = reg.register(key(a, "s2", 0));
        assert_eq!(reg.close_session("s1"), 1);
        assert_eq!(s1.closed.try_recv().unwrap(), CloseReason::LOGGED_OUT);
        assert!(s2.closed.try_recv().is_err());
        assert_eq!(reg.close_session(""), 0, "an empty id names no session");
        assert_eq!(reg.len(), 1);
    }

    #[tokio::test]
    async fn an_epoch_bump_closes_the_old_epoch_of_that_principal_only() {
        let reg = WsRegistry::new();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let mut old = reg.register(key(a, "s1", 3));
        let mut new = reg.register(key(a, "s2", 4));
        let mut other = reg.register(key(b, "s3", 3));
        assert_eq!(reg.close_stale_epoch(a, 4), 1);
        let reason = old.closed.try_recv().unwrap();
        assert_eq!(reason.code, CLOSE_REVOKED);
        assert!(new.closed.try_recv().is_err());
        assert!(other.closed.try_recv().is_err());
        // Idempotent: a closed socket isn't closed twice.
        assert_eq!(reg.close_stale_epoch(a, 4), 0);
    }

    /// S3-2: a handshake that resolved before a broker-side revocation but
    /// registers after it is born closed.
    #[tokio::test]
    async fn a_socket_registered_after_its_revocation_is_born_closed() {
        let reg = WsRegistry::new();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        assert_eq!(reg.close_session("s1"), 0);
        let mut late = reg.register(key(a, "s1", 0));
        assert_eq!(late.revoked(), Some(CloseReason::LOGGED_OUT));
        let mut other_session = reg.register(key(a, "s2", 0));
        assert_eq!(other_session.revoked(), None);

        assert_eq!(reg.close_stale_epoch(a, 5), 1, "s2 was at epoch 0");
        let mut stale = reg.register(key(a, "s3", 4));
        assert_eq!(stale.revoked(), Some(CloseReason::REVOKED));
        let mut current = reg.register(key(a, "s4", 5));
        assert_eq!(current.revoked(), None);
        let mut other = reg.register(key(b, "s5", 0));
        assert_eq!(other.revoked(), None, "another principal's floor");

        // A late call with an older epoch never lowers the floor or closes
        // a newer socket.
        assert_eq!(reg.close_stale_epoch(a, 3), 0);
        assert_eq!(current.revoked(), None);
        assert_eq!(reg.len(), 2);
    }

    struct Revoke(PrincipalId, AtomicBool);
    impl StillValid for Revoke {
        fn still_valid<'a>(&'a self, key: &'a WsKey) -> BoxFuture<'a, anyhow::Result<bool>> {
            Box::pin(async move {
                if key.device_id.as_deref() == Some("broken") {
                    self.1.store(true, Ordering::SeqCst);
                    anyhow::bail!("transient");
                }
                Ok(key.principal_id != self.0)
            })
        }
    }

    /// R-5: the pluggable check decides; a failing check closes nothing.
    #[tokio::test]
    async fn recheck_closes_what_the_check_rejects_and_tolerates_errors() {
        let reg = WsRegistry::new();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let mut ra = reg.register(key(a, "s1", 0));
        let mut rb = reg.register(key(b, "s2", 0));
        let mut broken = reg.register(WsKey {
            device_id: Some("broken".into()),
            grant_epoch: Some(7),
            ..key(b, "", 0)
        });
        let check = Revoke(a, AtomicBool::new(false));
        assert_eq!(reg.recheck(&check).await, 1);
        assert_eq!(ra.closed.try_recv().unwrap().code, 4401);
        assert!(rb.closed.try_recv().is_err());
        assert!(broken.closed.try_recv().is_err());
        assert!(check.1.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn the_default_check_follows_epoch_and_disable() {
        let (_tmp, root) = crate::server::operator::test_support::temp_root();
        let pool =
            crate::server::operator::open_accounts(&root, crate::server::operator::Opener::Broker)
                .await
                .unwrap();
        let id = PrincipalId::new_v7();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             session_epoch, created_at, updated_at) VALUES (?, 'ada', 'ik-ada', 20000, 20000, \
             '/h', 2, 0, 0)",
        )
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        let check = AccountEpochs { pool: pool.clone() };
        assert!(check.still_valid(&key(id, "s", 2)).await.unwrap());
        assert!(!check.still_valid(&key(id, "s", 1)).await.unwrap());
        assert!(!check
            .still_valid(&key(PrincipalId::new_v7(), "s", 0))
            .await
            .unwrap());
        sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
            .bind(id.to_string())
            .execute(&pool)
            .await
            .unwrap();
        assert!(!check.still_valid(&key(id, "s", 2)).await.unwrap());
    }

    /// A-33 (over R-5 / R-11): a device socket closes when its account's
    /// epoch moves (passwd, disable) or its grant is revoked or re-tiered;
    /// a forced logout revokes every grant of the principal, audited, in the
    /// logout's own transaction, and the CLI's write closes the socket
    /// within 2 s through the re-check loop.
    #[tokio::test]
    async fn device_sockets_follow_both_epochs_and_forced_logout_revokes_grants() {
        use crate::access::caps::Tier;
        use crate::server::operator::accounts::{self, Actor, DeviceGrantsRevoked};
        let (_tmp, root) = crate::server::operator::test_support::temp_root();
        let pool =
            crate::server::operator::open_accounts(&root, crate::server::operator::Opener::Broker)
                .await
                .unwrap();
        let store = crate::access::store::AccessStore::open_t1(pool.clone())
            .await
            .unwrap();
        let id = PrincipalId::new_v7();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             password_phc, created_at, updated_at) VALUES (?, 'ada', 'ik-ada', 20000, 20000, \
             '/h', 'phc', 0, 0)",
        )
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        let mut tx = store.begin().await.unwrap();
        let (device, _token) = crate::access::devices::issue_paired_in(
            &mut tx,
            &id.to_string(),
            "phone",
            None,
            Tier::Approve,
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let device_key = WsKey {
            principal_id: id,
            session_id: String::new(),
            session_epoch: 0,
            device_id: Some(device.device_id.clone()),
            grant_epoch: Some(device.grant_epoch),
        };
        let check = AccessStillValid { pool: pool.clone() };
        assert!(check.still_valid(&device_key).await.unwrap());

        // A session epoch bump (passwd) closes the device socket, keeps the
        // grant: the device reconnects at the new epoch.
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let acct = accounts::set_password_in(&mut tx, id, "phc2", Actor::Cli)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert!(!check.still_valid(&device_key).await.unwrap());
        let reconnect = WsKey {
            session_epoch: acct.session_epoch,
            ..device_key.clone()
        };
        assert!(check.still_valid(&reconnect).await.unwrap());

        // A tier change (grant_epoch) closes it too.
        sqlx::query("UPDATE devices SET grant_epoch = grant_epoch + 1 WHERE device_id = ?")
            .bind(&device.device_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(!check.still_valid(&reconnect).await.unwrap());
        let reconnect = WsKey {
            grant_epoch: Some(device.grant_epoch + 1),
            ..reconnect
        };
        assert!(check.still_valid(&reconnect).await.unwrap());

        // Forced logout from "the CLI" (its own connection): grants
        // revoked + audited; the loop closes the socket within 2 s.
        let reg = WsRegistry::new();
        let mut socket = reg.register(reconnect.clone());
        let conn = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(root.accounts_db()),
        )
        .await
        .unwrap();
        let (stop, _) = tokio::sync::broadcast::channel(1);
        let task = tokio::spawn(recheck_loop(
            reg.clone(),
            conn,
            Arc::new(check.clone()),
            Duration::from_millis(200),
            stop.subscribe(),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut cli = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(root.accounts_db()),
        )
        .await
        .unwrap();
        let mut tx = cli.begin_with("BEGIN IMMEDIATE").await.unwrap();
        accounts::revoke_sessions_in(
            &mut tx,
            id,
            Actor::Cli,
            &DeviceGrantsRevoked { via_cli: true },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(2), &mut socket.closed)
            .await
            .expect("closed within 2 s")
            .unwrap();
        assert_eq!(reason.code, CLOSE_REVOKED);
        stop.send(()).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;

        let (reason, revoked): (Option<String>, Option<i64>) =
            sqlx::query_as("SELECT revoked_reason, revoked_at FROM devices WHERE device_id = ?")
                .bind(&device.device_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(reason.as_deref(), Some("sessions_revoked"));
        assert!(revoked.is_some());
        let kinds: Vec<(String, String)> =
            sqlx::query_as("SELECT kind, via FROM audit_events ORDER BY seq")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            kinds,
            vec![
                ("store.created".to_string(), "system".to_string()),
                ("device.revoked".to_string(), "cli".to_string())
            ]
        );
        // The broker's chain view adopts the CLI's row (§6.3 step 2).
        let mut conn = pool.acquire().await.unwrap();
        assert!(crate::access::audit::verify(&mut conn, &store.store_id)
            .await
            .is_ok());
    }

    /// The loop wakes on another connection's commit (the CLI's), closes the
    /// stale socket well inside the 2 s bound, and stops on shutdown.
    #[tokio::test]
    async fn the_loop_closes_on_an_external_epoch_bump_within_two_seconds() {
        let (_tmp, root) = crate::server::operator::test_support::temp_root();
        let pool =
            crate::server::operator::open_accounts(&root, crate::server::operator::Opener::Broker)
                .await
                .unwrap();
        let id = PrincipalId::new_v7();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             created_at, updated_at) VALUES (?, 'ada', 'ik-ada', 20000, 20000, '/h', 0, 0)",
        )
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        let reg = WsRegistry::new();
        let mut socket = reg.register(key(id, "s1", 0));
        let conn = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(root.accounts_db()),
        )
        .await
        .unwrap();
        let (stop, _) = tokio::sync::broadcast::channel(1);
        let task = tokio::spawn(recheck_loop(
            reg.clone(),
            conn,
            Arc::new(AccountEpochs { pool: pool.clone() }),
            Duration::from_millis(200),
            stop.subscribe(),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(socket.closed.try_recv().is_err(), "nothing changed yet");

        // "The CLI": a separate connection bumps the epoch.
        let mut cli = SqliteConnection::connect_with(
            &sqlx::sqlite::SqliteConnectOptions::new().filename(root.accounts_db()),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE accounts SET session_epoch = session_epoch + 1")
            .execute(&mut cli)
            .await
            .unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(2), &mut socket.closed)
            .await
            .expect("closed within 2 s")
            .unwrap();
        assert_eq!(reason.code, CLOSE_REVOKED);
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
}
