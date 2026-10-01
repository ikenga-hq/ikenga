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
//! The default check ([`AccountEpochs`]) closes a socket once its account is
//! gone or disabled or its `session_epoch` moved. WP-74 plugs in a check that
//! also compares `grant_epoch`, so a device socket closes on **either** epoch
//! moving (R-5). Closing a socket never kills the PTY or run behind it; the
//! owner reattaches after logging in again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
}

struct Entry {
    key: WsKey,
    close: Option<oneshot::Sender<CloseReason>>,
}

/// Every proxied socket that is open right now.
#[derive(Default)]
pub struct WsRegistry {
    entries: Mutex<HashMap<u64, Entry>>,
    next: AtomicU64,
}

/// One registered socket. Dropping it unregisters; `closed` resolves when
/// the registry decides the socket must close.
pub struct WsRegistration {
    id: u64,
    registry: Arc<WsRegistry>,
    pub closed: oneshot::Receiver<CloseReason>,
}

impl Drop for WsRegistration {
    fn drop(&mut self) {
        self.registry.lock().remove(&self.id);
    }
}

impl WsRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn register(self: &Arc<Self>, key: WsKey) -> WsRegistration {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.lock().insert(
            id,
            Entry {
                key,
                close: Some(tx),
            },
        );
        WsRegistration {
            id,
            registry: self.clone(),
            closed: rx,
        }
    }

    /// Open sockets (not yet told to close).
    pub fn len(&self) -> usize {
        self.lock().values().filter(|e| e.close.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Open sockets of one principal (the idle reaper counts them, OD-13).
    pub fn open_for(&self, principal: PrincipalId) -> usize {
        self.lock()
            .values()
            .filter(|e| e.close.is_some() && e.key.principal_id == principal)
            .count()
    }

    /// The keys of every open socket, for a validity pass.
    pub fn keys(&self) -> Vec<(u64, WsKey)> {
        self.lock()
            .iter()
            .filter(|(_, e)| e.close.is_some())
            .map(|(id, e)| (*id, e.key.clone()))
            .collect()
    }

    fn close_ids(&self, ids: &[u64], reason: &CloseReason) -> usize {
        let mut entries = self.lock();
        let mut closed = 0;
        for id in ids {
            if let Some(tx) = entries.get_mut(id).and_then(|e| e.close.take()) {
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

    /// `POST /auth/logout`: that one session's sockets.
    pub fn close_session(&self, session_id: &str) -> usize {
        if session_id.is_empty() {
            return 0;
        }
        self.close_where(CloseReason::LOGGED_OUT, |k| k.session_id == session_id)
    }

    /// A broker-side epoch bump: every socket of `principal` opened under any
    /// other `session_epoch` (§2.2: "carrying the old epoch").
    pub fn close_stale_epoch(&self, principal: PrincipalId, current_epoch: i64) -> usize {
        self.close_where(CloseReason::REVOKED, |k| {
            k.principal_id == principal && k.session_epoch != current_epoch
        })
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
