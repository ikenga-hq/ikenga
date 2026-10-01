//! `auth_events` (G-PRINCIPAL §6.1): the append-only authentication log.
//!
//! **Every** write — broker and root CLI alike — goes through [`record`]
//! (G-ACCESS request R-2). WP-77 absorbs this table into the access audit
//! chain (G-ACCESS §6.6) by re-pointing this one function at
//! `access::audit::append`; a second writer would escape the chain.
//!
//! `detail` is JSON and never carries a password, hash or token.

use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Sqlite, Transaction};

use crate::executor::PrincipalId;

/// The closed `kind` set of §6.1. Mirrors the table's `CHECK`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthEventKind {
    LoginOk,
    LoginFail,
    LoginThrottled,
    Logout,
    PasswordChanged,
    AccountCreated,
    AccountDisabled,
    AccountEnabled,
    SessionsRevoked,
    ProvisionFailed,
    ProbeFailed,
}

impl AuthEventKind {
    pub const ALL: [AuthEventKind; 11] = [
        AuthEventKind::LoginOk,
        AuthEventKind::LoginFail,
        AuthEventKind::LoginThrottled,
        AuthEventKind::Logout,
        AuthEventKind::PasswordChanged,
        AuthEventKind::AccountCreated,
        AuthEventKind::AccountDisabled,
        AuthEventKind::AccountEnabled,
        AuthEventKind::SessionsRevoked,
        AuthEventKind::ProvisionFailed,
        AuthEventKind::ProbeFailed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AuthEventKind::LoginOk => "login_ok",
            AuthEventKind::LoginFail => "login_fail",
            AuthEventKind::LoginThrottled => "login_throttled",
            AuthEventKind::Logout => "logout",
            AuthEventKind::PasswordChanged => "password_changed",
            AuthEventKind::AccountCreated => "account_created",
            AuthEventKind::AccountDisabled => "account_disabled",
            AuthEventKind::AccountEnabled => "account_enabled",
            AuthEventKind::SessionsRevoked => "sessions_revoked",
            AuthEventKind::ProvisionFailed => "provision_failed",
            AuthEventKind::ProbeFailed => "probe_failed",
        }
    }
}

/// One row to append. Build with [`AuthEvent::new`] and the `with_*` setters.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthEvent {
    pub kind: AuthEventKind,
    pub principal_id: Option<PrincipalId>,
    pub username_tried: Option<String>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
    pub detail: Option<serde_json::Value>,
}

impl AuthEvent {
    pub fn new(kind: AuthEventKind) -> Self {
        Self {
            kind,
            principal_id: None,
            username_tried: None,
            remote_addr: None,
            user_agent: None,
            detail: None,
        }
    }

    pub fn principal(mut self, id: PrincipalId) -> Self {
        self.principal_id = Some(id);
        self
    }

    pub fn username_tried(mut self, username: impl Into<String>) -> Self {
        self.username_tried = Some(username.into());
        self
    }

    pub fn remote_addr(mut self, addr: Option<String>) -> Self {
        self.remote_addr = addr;
        self
    }

    pub fn user_agent(mut self, ua: Option<String>) -> Self {
        self.user_agent = ua;
        self
    }

    pub fn detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Append `event` inside the caller's transaction (R-2). The only writer of
/// `auth_events`. Returns the new row id.
pub async fn record(tx: &mut Transaction<'_, Sqlite>, event: AuthEvent) -> sqlx::Result<i64> {
    let detail = event.detail.as_ref().map(|d| d.to_string());
    let result = sqlx::query(
        "INSERT INTO auth_events (at, principal_id, username_tried, kind, remote_addr, user_agent, detail) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(now_secs())
    .bind(event.principal_id.map(|id| id.to_string()))
    .bind(event.username_tried)
    .bind(event.kind.as_str())
    .bind(event.remote_addr)
    .bind(event.user_agent)
    .bind(detail)
    .execute(&mut **tx)
    .await?;
    Ok(result.last_insert_rowid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::operator::migrations::{apply, Policy, ACCOUNTS};
    use sqlx::{Connection, SqliteConnection};

    #[tokio::test]
    async fn every_kind_satisfies_the_table_check_and_unknown_ones_do_not() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        for kind in AuthEventKind::ALL {
            record(
                &mut tx,
                AuthEvent::new(kind)
                    .username_tried("ada")
                    .detail(serde_json::json!({ "via": "test" })),
            )
            .await
            .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        }
        let bad = sqlx::query("INSERT INTO auth_events (at, kind) VALUES (0, 'login_maybe')")
            .execute(&mut *tx)
            .await;
        assert!(bad.is_err(), "kind CHECK must reject unknown kinds");
        tx.commit().await.unwrap();
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM auth_events ORDER BY id")
            .fetch_all(&mut conn)
            .await
            .unwrap();
        assert_eq!(
            kinds,
            AuthEventKind::ALL.map(|k| k.as_str().to_string()).to_vec()
        );
    }

    #[tokio::test]
    async fn a_rolled_back_transaction_records_nothing() {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        {
            let mut tx = conn.begin().await.unwrap();
            record(&mut tx, AuthEvent::new(AuthEventKind::LoginOk))
                .await
                .unwrap();
            // dropped without commit
        }
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_events")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
