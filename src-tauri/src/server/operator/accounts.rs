//! The `accounts` table (G-PRINCIPAL §6.1): row type, lookups, username rules
//! and the in-transaction account mutations that do not touch the host
//! (`provision` owns the ones that do).
//!
//! Every mutation here runs inside a caller-owned transaction, which the
//! caller opens with `BEGIN IMMEDIATE` (§7.1). Rows are never deleted (I-4).

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqliteConnection, Transaction};

use super::auth_events::{self, AuthEvent, AuthEventKind};
use crate::executor::{Principal, PrincipalId};

/// One `accounts` row. `password_phc` is never serialized or debug-printed.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Account {
    pub principal_id: PrincipalId,
    pub username: String,
    #[serde(skip)]
    pub password_phc: Option<String>,
    pub unix_name: String,
    pub unix_uid: u32,
    pub unix_gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
    pub is_admin: bool,
    pub session_epoch: i64,
    pub adopted: bool,
    pub disabled_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub password_changed_at: Option<i64>,
}

impl fmt::Debug for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Account")
            .field("principal_id", &self.principal_id)
            .field("username", &self.username)
            .field(
                "password_phc",
                &self.password_phc.as_ref().map(|_| "<redacted>"),
            )
            .field("unix_name", &self.unix_name)
            .field("unix_uid", &self.unix_uid)
            .field("unix_gid", &self.unix_gid)
            .field("home", &self.home)
            .field("shell", &self.shell)
            .field("is_admin", &self.is_admin)
            .field("session_epoch", &self.session_epoch)
            .field("adopted", &self.adopted)
            .field("disabled_at", &self.disabled_at)
            .finish_non_exhaustive()
    }
}

impl Account {
    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }

    /// The fully resolved spawn identity (§1, §9.1).
    pub fn principal(&self) -> Principal {
        Principal {
            id: self.principal_id,
            username: self.username.clone(),
            unix_name: self.unix_name.clone(),
            uid: self.unix_uid,
            gid: self.unix_gid,
            home: self.home.clone(),
            shell: self.shell.clone(),
        }
    }

    fn from_row(row: &SqliteRow) -> Result<Self, sqlx::Error> {
        let decode = |col: &str, e: String| sqlx::Error::ColumnDecode {
            index: col.into(),
            source: e.into(),
        };
        let id: String = row.try_get("principal_id")?;
        let uid: i64 = row.try_get("unix_uid")?;
        let gid: i64 = row.try_get("unix_gid")?;
        Ok(Account {
            principal_id: id
                .parse()
                .map_err(|e: crate::executor::ParsePrincipalIdError| {
                    decode("principal_id", e.to_string())
                })?,
            username: row.try_get("username")?,
            password_phc: row.try_get("password_phc")?,
            unix_name: row.try_get("unix_name")?,
            unix_uid: u32::try_from(uid).map_err(|e| decode("unix_uid", e.to_string()))?,
            unix_gid: u32::try_from(gid).map_err(|e| decode("unix_gid", e.to_string()))?,
            home: PathBuf::from(row.try_get::<String, _>("home")?),
            shell: PathBuf::from(row.try_get::<String, _>("shell")?),
            is_admin: row.try_get::<i64, _>("is_admin")? != 0,
            session_epoch: row.try_get("session_epoch")?,
            adopted: row.try_get::<i64, _>("adopted")? != 0,
            disabled_at: row.try_get("disabled_at")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            password_changed_at: row.try_get("password_changed_at")?,
        })
    }
}

const SELECT: &str = "SELECT principal_id, username, password_phc, unix_name, unix_uid, unix_gid, \
     home, shell, is_admin, session_epoch, adopted, disabled_at, created_at, updated_at, \
     password_changed_at FROM accounts";

/// Case-insensitive, as the column's `COLLATE NOCASE` unique key.
pub async fn by_username(
    conn: &mut SqliteConnection,
    username: &str,
) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query(&format!("{SELECT} WHERE username = ?"))
        .bind(username)
        .fetch_optional(conn)
        .await?
        .map(|r| Account::from_row(&r))
        .transpose()
}

pub async fn by_id(
    conn: &mut SqliteConnection,
    id: PrincipalId,
) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query(&format!("{SELECT} WHERE principal_id = ?"))
        .bind(id.to_string())
        .fetch_optional(conn)
        .await?
        .map(|r| Account::from_row(&r))
        .transpose()
}

/// Every row, disabled tombstones included, oldest first.
pub async fn list(conn: &mut SqliteConnection) -> Result<Vec<Account>, sqlx::Error> {
    sqlx::query(&format!("{SELECT} ORDER BY created_at, principal_id"))
        .fetch_all(conn)
        .await?
        .iter()
        .map(Account::from_row)
        .collect()
}

pub async fn count(conn: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
        .fetch_one(conn)
        .await
}

// ─── usernames ──────────────────────────────────────────────────────────────

/// Prefix of every allocated passwd name (P-5).
pub const UNIX_NAME_PREFIX: &str = "ik-";
/// Longest login name the `accounts.username` CHECK allows.
pub const MAX_USERNAME_CHARS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsernameError {
    Empty,
    TooLong,
    /// The derived passwd name fails `^[a-z][a-z0-9-]{0,28}$` (§7.2).
    BadUnixName(String),
}

impl fmt::Display for UsernameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsernameError::Empty => f.write_str("username is empty"),
            UsernameError::TooLong => {
                write!(f, "username is longer than {MAX_USERNAME_CHARS} characters")
            }
            UsernameError::BadUnixName(name) => write!(
                f,
                "username derives the passwd name `{name}`, which is not \
                 ^[a-z][a-z0-9-]{{0,28}}$ — use ASCII letters, digits and '-', at most 26 characters"
            ),
        }
    }
}

impl std::error::Error for UsernameError {}

fn valid_unix_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 29
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `unix_name = "ik-" + lowercase(username)`, validated
/// `^[a-z][a-z0-9-]{0,28}$` (§7.2, P-5). Also enforces the username column's
/// 1..=32 length, so a name that passes here is insertable.
pub fn unix_name_for(username: &str) -> Result<String, UsernameError> {
    if username.is_empty() {
        return Err(UsernameError::Empty);
    }
    if username.chars().count() > MAX_USERNAME_CHARS {
        return Err(UsernameError::TooLong);
    }
    let name = format!("{UNIX_NAME_PREFIX}{}", username.to_lowercase());
    if !valid_unix_name(&name) {
        return Err(UsernameError::BadUnixName(name));
    }
    Ok(name)
}

// ─── in-transaction mutations ───────────────────────────────────────────────

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Where an account write came from, for `auth_events.detail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// The root CLI (`ikenga-server accounts …`).
    Cli,
    /// The broker, on behalf of the signed-in principal itself.
    Broker,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Actor::Cli => "cli",
            Actor::Broker => "broker",
        }
    }
}

pub type HookFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// Called by the forced-logout path inside its transaction (G-ACCESS R-11).
/// WP-74a fills it with "revoke every device grant of that principal"
/// (`revoked_reason = 'sessions_revoked'`, plus audit rows). An error rolls
/// the whole forced logout back.
pub trait SessionsRevokedHook: Send + Sync {
    fn on_sessions_revoked<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        principal_id: PrincipalId,
    ) -> HookFuture<'a>
    where
        'c: 'a;
}

/// The default hook: nothing beyond the epoch bump (WP-20 has no devices).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDeviceGrants;

impl SessionsRevokedHook for NoDeviceGrants {
    fn on_sessions_revoked<'a, 'c>(
        &'a self,
        _tx: &'a mut Transaction<'c, Sqlite>,
        _principal_id: PrincipalId,
    ) -> HookFuture<'a>
    where
        'c: 'a,
    {
        Box::pin(async { Ok(()) })
    }
}

/// Re-read `id` inside `tx`. A row that vanished mid-transaction is a bug
/// (rows are never deleted), so it is an error, not `None`.
async fn reload(tx: &mut Transaction<'_, Sqlite>, id: PrincipalId) -> Result<Account, sqlx::Error> {
    by_id(tx, id).await?.ok_or(sqlx::Error::RowNotFound)
}

/// Store a new password hash: sets `password_changed_at`, bumps
/// `session_epoch` (every session and open socket of the principal dies,
/// §2.2 / I-8), and writes `password_changed`. Device grants are kept
/// (G-ACCESS §3.10), so the R-11 hook is **not** called.
pub async fn set_password_in(
    tx: &mut Transaction<'_, Sqlite>,
    id: PrincipalId,
    new_phc: &str,
    actor: Actor,
) -> Result<Account, sqlx::Error> {
    let now = now_secs();
    let n = sqlx::query(
        "UPDATE accounts SET password_phc = ?, password_changed_at = ?, \
         session_epoch = session_epoch + 1, updated_at = ? WHERE principal_id = ?",
    )
    .bind(new_phc)
    .bind(now)
    .bind(now)
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let account = reload(tx, id).await?;
    auth_events::record(
        tx,
        AuthEvent::new(AuthEventKind::PasswordChanged)
            .principal(id)
            .detail(serde_json::json!({
                "via": actor.as_str(),
                "session_epoch": account.session_epoch,
            })),
    )
    .await?;
    Ok(account)
}

/// Rehash-on-login (§6.2): replace `old_phc` with `new_phc`, the same
/// password at the live argon2 parameters. Not a password change: no epoch
/// bump, no `password_changed` row. Conditional on the row still holding
/// `old_phc`, so it never undoes a concurrent `passwd`; `Ok(None)` when it
/// lost that race. (The session auth hash follows the PHC, so the
/// principal's *other* sessions sign in again once.)
pub async fn rehash_password_in(
    tx: &mut Transaction<'_, Sqlite>,
    id: PrincipalId,
    old_phc: &str,
    new_phc: &str,
) -> Result<Option<Account>, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE accounts SET password_phc = ?, updated_at = ? \
         WHERE principal_id = ? AND password_phc = ?",
    )
    .bind(new_phc)
    .bind(now_secs())
    .bind(id.to_string())
    .bind(old_phc)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Ok(None);
    }
    reload(tx, id).await.map(Some)
}

/// Forced logout: bump `session_epoch`, write `sessions_revoked`, and call the
/// R-11 hook — all inside `tx`, so a failing hook revokes nothing.
pub async fn revoke_sessions_in(
    tx: &mut Transaction<'_, Sqlite>,
    id: PrincipalId,
    actor: Actor,
    hook: &dyn SessionsRevokedHook,
) -> anyhow::Result<Account> {
    let n = sqlx::query(
        "UPDATE accounts SET session_epoch = session_epoch + 1, updated_at = ? WHERE principal_id = ?",
    )
    .bind(now_secs())
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n != 1 {
        anyhow::bail!("no account {id}");
    }
    let account = reload(tx, id).await?;
    auth_events::record(
        tx,
        AuthEvent::new(AuthEventKind::SessionsRevoked)
            .principal(id)
            .detail(serde_json::json!({
                "via": actor.as_str(),
                "session_epoch": account.session_epoch,
            })),
    )
    .await?;
    hook.on_sessions_revoked(tx, id).await?;
    Ok(account)
}

/// Set `disabled_at` and bump `session_epoch` (§7.3), writing
/// `account_disabled`. Idempotent on an already-disabled row: the original
/// `disabled_at` is kept, but the epoch is bumped again. Device rows are not
/// touched (G-ACCESS §3.10), so no hook.
pub async fn mark_disabled_in(
    tx: &mut Transaction<'_, Sqlite>,
    id: PrincipalId,
    actor: Actor,
) -> Result<Account, sqlx::Error> {
    let now = now_secs();
    let n = sqlx::query(
        "UPDATE accounts SET disabled_at = COALESCE(disabled_at, ?), \
         session_epoch = session_epoch + 1, updated_at = ? WHERE principal_id = ?",
    )
    .bind(now)
    .bind(now)
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let account = reload(tx, id).await?;
    auth_events::record(
        tx,
        AuthEvent::new(AuthEventKind::AccountDisabled)
            .principal(id)
            .detail(serde_json::json!({
                "via": actor.as_str(),
                "session_epoch": account.session_epoch,
            })),
    )
    .await?;
    Ok(account)
}

/// Clear `disabled_at`, writing `account_enabled`. The epoch is not touched:
/// the sessions it revoked stay revoked.
pub async fn mark_enabled_in(
    tx: &mut Transaction<'_, Sqlite>,
    id: PrincipalId,
    actor: Actor,
) -> Result<Account, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE accounts SET disabled_at = NULL, updated_at = ? WHERE principal_id = ?",
    )
    .bind(now_secs())
    .bind(id.to_string())
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    let account = reload(tx, id).await?;
    auth_events::record(
        tx,
        AuthEvent::new(AuthEventKind::AccountEnabled)
            .principal(id)
            .detail(serde_json::json!({ "via": actor.as_str() })),
    )
    .await?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_names_follow_p5() {
        assert_eq!(unix_name_for("ada").unwrap(), "ik-ada");
        assert_eq!(unix_name_for("Ada-Lovelace2").unwrap(), "ik-ada-lovelace2");
        assert_eq!(unix_name_for(&"a".repeat(26)).unwrap().len(), 29);
        assert!(matches!(unix_name_for(""), Err(UsernameError::Empty)));
        assert!(matches!(
            unix_name_for(&"a".repeat(33)),
            Err(UsernameError::TooLong)
        ));
        for bad in [
            "a".repeat(27),
            "ada.l".into(),
            "ada l".into(),
            "ad_a".into(),
            "émile".into(),
            "ada/..".into(),
        ] {
            assert!(
                matches!(unix_name_for(&bad), Err(UsernameError::BadUnixName(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn debug_never_prints_the_hash() {
        let account = Account {
            principal_id: PrincipalId::new_v7(),
            username: "ada".into(),
            password_phc: Some("$argon2id$secret".into()),
            unix_name: "ik-ada".into(),
            unix_uid: 20000,
            unix_gid: 20000,
            home: "/h".into(),
            shell: "/bin/sh".into(),
            is_admin: false,
            session_epoch: 0,
            adopted: false,
            disabled_at: None,
            created_at: 0,
            updated_at: 0,
            password_changed_at: None,
        };
        let dbg = format!("{account:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        let json = serde_json::to_string(&account).unwrap();
        assert!(
            !json.contains("secret") && !json.contains("password_phc"),
            "{json}"
        );
        assert_eq!(account.principal().uid, 20000);
    }
}
