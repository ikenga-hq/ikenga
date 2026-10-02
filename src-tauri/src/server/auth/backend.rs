//! The T1 session stack (G-PRINCIPAL §2.2, §6.3): an `axum-login` 0.16
//! backend over `operator/accounts.db`, sessions in `tower-sessions` 0.13's
//! SQLite store at `operator/sessions.db`, and the session-cookie
//! [`CredentialResolver`] that turns a logged-in session into a
//! [`PrincipalCtx`].
//!
//! **Staying valid.** [`BrokerUser::session_auth_hash`] is derived from
//! `(password_phc, session_epoch)`. `axum-login` compares the hash stored in
//! the session with the one the backend computes on every request, so a
//! password change, a disable (which also makes [`AccountsBackend::get_user`]
//! answer `None`) or a forced logout invalidates every session of the
//! principal on its next request — the CLI's writes included, because the
//! account row is re-read per request (§7.1).
//!
//! **Cookie** (§2.2, P-3, P-4): `ikenga_session`, `HttpOnly`,
//! `SameSite=Strict`, `Path=/`, `Secure` unless `--insecure-cookie`,
//! `OnInactivity(24h)` (saved on every request so the window slides). The
//! login route cycles the session id.
//!
//! **A save never recreates a session** ([`BrokerSessionStore`]). Saving on
//! every request means a request that loaded its session *before* a logout
//! (or a password change's id cycle) writes it back *after* — and the sqlx
//! store's `save` is an upsert, which would bring the deleted id back to
//! life. Here `save` only updates a row that still exists; only `create`
//! (a login) inserts.

use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use axum::http::request::Parts;
use axum_login::{AuthSession, AuthUser, AuthnBackend};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use tower_sessions::cookie::{time, SameSite};
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store::{self, ExpiredDeletion, SessionStore};
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::SqliteStore;

use super::{BoxFuture, Credential, CredentialResolver, Epochs, PrincipalCtx, Resolution};
use crate::executor::PrincipalId;
use crate::server::operator::accounts::{self, Account};
use crate::server::operator::password::{LoginAttempt, LoginOutcome, LoginVerifier};
use crate::server::operator::OperatorRoot;

/// P-3.
pub const SESSION_COOKIE: &str = "ikenga_session";
/// P-4.
pub const SESSION_INACTIVITY: Duration = Duration::from_secs(24 * 60 * 60);

/// `session_auth_hash` (§2.2): `sha256("ikenga-session-v1" ‖ phc ‖ epoch)`.
/// The PHC string is itself salted, so the hash reveals nothing about the
/// password; it only has to change whenever the PHC or the epoch does.
pub fn session_auth_hash(password_phc: Option<&str>, session_epoch: i64) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"ikenga-session-v1\0");
    match password_phc {
        Some(phc) => {
            h.update([1u8]);
            h.update(phc.as_bytes());
        }
        None => h.update([0u8]),
    }
    h.update(b"\0");
    h.update(session_epoch.to_le_bytes());
    h.finalize().to_vec()
}

/// The `axum-login` user: one active account.
#[derive(Clone)]
pub struct BrokerUser {
    pub account: Account,
    auth_hash: Vec<u8>,
}

impl std::fmt::Debug for BrokerUser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Account`'s Debug already redacts the PHC; the hash is skipped.
        f.debug_struct("BrokerUser")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl BrokerUser {
    pub fn new(account: Account) -> Self {
        let auth_hash = session_auth_hash(account.password_phc.as_deref(), account.session_epoch);
        Self { account, auth_hash }
    }
}

impl AuthUser for BrokerUser {
    type Id = PrincipalId;

    fn id(&self) -> PrincipalId {
        self.account.principal_id
    }

    fn session_auth_hash(&self) -> &[u8] {
        &self.auth_hash
    }
}

/// A backend failure, as `axum-login` needs a `std::error::Error`.
#[derive(Debug)]
pub struct BackendError(pub anyhow::Error);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for BackendError {}

/// The `axum-login` backend over `operator/accounts.db`.
#[derive(Clone)]
pub struct AccountsBackend {
    pub pool: SqlitePool,
    pub verifier: Arc<LoginVerifier>,
}

#[axum::async_trait]
impl AuthnBackend for AccountsBackend {
    type User = BrokerUser;
    type Credentials = LoginAttempt;
    type Error = BackendError;

    /// The §6.2 verifier (throttle, dummy hash, `auth_events`). The login
    /// route calls the verifier itself to tell a throttle from a failure;
    /// this is the trait's form of the same check.
    async fn authenticate(&self, creds: LoginAttempt) -> Result<Option<BrokerUser>, BackendError> {
        match self
            .verifier
            .login(&self.pool, creds)
            .await
            .map_err(BackendError)?
        {
            LoginOutcome::Ok(account) => Ok(Some(BrokerUser::new(account))),
            LoginOutcome::Failed | LoginOutcome::Throttled { .. } => Ok(None),
        }
    }

    /// Re-read on every request. A disabled account has no user, so its
    /// sessions stop resolving at once whatever the epoch says.
    async fn get_user(&self, id: &PrincipalId) -> Result<Option<BrokerUser>, BackendError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| BackendError(e.into()))?;
        let account = accounts::by_id(&mut conn, *id)
            .await
            .map_err(|e| BackendError(e.into()))?;
        Ok(account
            .filter(|a| !a.is_disabled() && a.password_phc.is_some())
            .map(BrokerUser::new))
    }
}

/// The session-cookie resolver (first in the R-4 list).
#[derive(Debug, Default, Clone, Copy)]
pub struct SessionCookieResolver;

impl CredentialResolver for SessionCookieResolver {
    fn name(&self) -> &'static str {
        "session_cookie"
    }

    fn resolve<'a>(&'a self, parts: &'a Parts) -> BoxFuture<'a, anyhow::Result<Resolution>> {
        Box::pin(async move {
            // Inserted by the AuthManagerLayer, which has already loaded the
            // session and checked its auth hash against the live account.
            let Some(auth) = parts.extensions.get::<AuthSession<AccountsBackend>>() else {
                return Ok(Resolution::NotPresent);
            };
            let Some(user) = auth.user.as_ref() else {
                return Ok(Resolution::NotPresent);
            };
            let Some(session_id) = parts
                .extensions
                .get::<tower_sessions::Session>()
                .and_then(|s| s.id())
            else {
                return Ok(Resolution::NotPresent);
            };
            Ok(Resolution::Resolved {
                ctx: PrincipalCtx {
                    principal: user.account.principal(),
                    via: Credential::Session {
                        session_id: session_id.to_string(),
                    },
                },
                epochs: Epochs {
                    session_epoch: user.account.session_epoch,
                    grant_epoch: None,
                },
            })
        })
    }
}

/// `operator/sessions.db`: the sqlx store, except that [`save`] is
/// UPDATE-only, so a request still in flight across a logout can't
/// resurrect the session it loaded (see the module docs).
///
/// [`save`]: SessionStore::save
#[derive(Debug, Clone)]
pub struct BrokerSessionStore {
    inner: SqliteStore,
    pool: SqlitePool,
}

/// The sqlx store's table (its default name; never configured otherwise).
const SESSIONS_TABLE: &str = "tower_sessions";

#[axum::async_trait]
impl SessionStore for BrokerSessionStore {
    /// A new session (login, or an id cycle): the only path that inserts.
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.inner.create(record).await
    }

    /// Slide an **existing** session's data and expiry. A row that is gone
    /// (logged out, cycled, expired and swept) stays gone: the request that
    /// still holds it finishes, but its id never authenticates again.
    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let data =
            rmp_serde::to_vec(record).map_err(|e| session_store::Error::Encode(e.to_string()))?;
        sqlx::query(&format!(
            "UPDATE {SESSIONS_TABLE} SET data = ?, expiry_date = ? WHERE id = ?"
        ))
        .bind(data)
        .bind(record.expiry_date)
        .bind(record.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        Ok(())
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        self.inner.load(session_id).await
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        self.inner.delete(session_id).await
    }
}

#[axum::async_trait]
impl ExpiredDeletion for BrokerSessionStore {
    async fn delete_expired(&self) -> session_store::Result<()> {
        self.inner.delete_expired().await
    }
}

/// Open (creating `0600` if needed) `operator/sessions.db` and run the
/// store's own migration (`tower_sessions (id, data, expiry_date)`).
/// Separate from `accounts.db` so per-request session writes never contend
/// with account writes (P-6).
pub async fn open_session_store(root: &OperatorRoot) -> anyhow::Result<BrokerSessionStore> {
    let path = root.sessions_db();
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    let inner = SqliteStore::new(pool.clone());
    inner.migrate().await?;
    Ok(BrokerSessionStore { inner, pool })
}

/// The cookie and expiry policy (§2.2, P-3, P-4).
pub fn session_layer(
    store: BrokerSessionStore,
    insecure_cookie: bool,
) -> SessionManagerLayer<BrokerSessionStore> {
    SessionManagerLayer::new(store)
        .with_name(SESSION_COOKIE)
        .with_http_only(true)
        .with_same_site(SameSite::Strict)
        .with_path("/")
        .with_secure(!insecure_cookie)
        .with_expiry(Expiry::OnInactivity(time::Duration::seconds(
            SESSION_INACTIVITY.as_secs() as i64,
        )))
        // Save on every authenticated request, so the inactivity window
        // slides; only `sessions.db` is written, and only rows that still
        // exist ([`BrokerSessionStore::save`]).
        .with_always_save(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_auth_hash_moves_with_the_phc_and_the_epoch() {
        let a = session_auth_hash(Some("$argon2id$a"), 0);
        assert_eq!(a, session_auth_hash(Some("$argon2id$a"), 0));
        assert_ne!(a, session_auth_hash(Some("$argon2id$a"), 1));
        assert_ne!(a, session_auth_hash(Some("$argon2id$b"), 0));
        assert_ne!(a, session_auth_hash(None, 0));
        assert_eq!(a.len(), 32);
    }

    #[tokio::test]
    async fn the_session_store_is_owner_only() {
        let (_tmp, root) = crate::server::operator::test_support::temp_root();
        let _store = open_session_store(&root).await.unwrap();
        let mode = fs::metadata(root.sessions_db())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// S3-1: a request that loaded its session before a logout writes it
    /// back after — that write must not bring the session back.
    #[tokio::test]
    async fn a_save_after_a_delete_never_resurrects_the_session() {
        use tower_sessions::Session;
        let (_tmp, root) = crate::server::operator::test_support::temp_root();
        let store = Arc::new(open_session_store(&root).await.unwrap());

        // Login: a fresh session is created.
        let login = Session::new(None, store.clone(), None);
        login.insert("user", "ada").await.unwrap();
        login.save().await.unwrap();
        let id = login.id().unwrap();

        // A slow request loads it…
        let in_flight = Session::new(Some(id), store.clone(), None);
        assert_eq!(
            in_flight.get::<String>("user").await.unwrap().as_deref(),
            Some("ada")
        );
        // …the user logs out on another request…
        let logout = Session::new(Some(id), store.clone(), None);
        logout.flush().await.unwrap();
        assert!(store.load(&id).await.unwrap().is_none());
        // …and the slow request finishes and saves (always_save).
        in_flight.save().await.unwrap();
        assert!(
            store.load(&id).await.unwrap().is_none(),
            "the logged-out session came back"
        );

        // An existing session still slides.
        let live = Session::new(None, store.clone(), None);
        live.insert("user", "bob").await.unwrap();
        live.save().await.unwrap();
        let live_id = live.id().unwrap();
        let again = Session::new(Some(live_id), store.clone(), None);
        again.insert("n", 2).await.unwrap();
        again.save().await.unwrap();
        let rec = store.load(&live_id).await.unwrap().unwrap();
        assert_eq!(rec.data.get("n"), Some(&serde_json::json!(2)));
    }
}
