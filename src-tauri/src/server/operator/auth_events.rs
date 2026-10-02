//! `auth_events` (G-PRINCIPAL §6.1): the append-only authentication log.
//!
//! **Every** write — broker and root CLI alike — goes through [`record`]
//! (G-ACCESS request R-2), so a second writer can't escape the chain.
//!
//! **Absorbed (G-ACCESS §6.6, WP-77).** Once the access set's
//! `0002_absorb_auth_events` has run (the broker migrates it at start),
//! `auth_events` is a view over the access audit chain with §6.1's columns,
//! the old rows are kept in `auth_events_legacy`, and [`record`] appends to
//! the chain (`kind = 'auth.<kind>'`, category `access`) through the
//! chain's §6.3 append — the same function in the broker and the root CLI.
//! Before that (a store no WP-77 broker has started on yet) it still
//! inserts into the table, and the absorption backfills those rows.
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

    /// The chained kind (G-ACCESS §6.5: the §6.1 kinds, prefixed).
    pub fn audit_kind(self) -> &'static str {
        match self {
            AuthEventKind::LoginOk => "auth.login_ok",
            AuthEventKind::LoginFail => "auth.login_fail",
            AuthEventKind::LoginThrottled => "auth.login_throttled",
            AuthEventKind::Logout => "auth.logout",
            AuthEventKind::PasswordChanged => "auth.password_changed",
            AuthEventKind::AccountCreated => "auth.account_created",
            AuthEventKind::AccountDisabled => "auth.account_disabled",
            AuthEventKind::AccountEnabled => "auth.account_enabled",
            AuthEventKind::SessionsRevoked => "auth.sessions_revoked",
            AuthEventKind::ProvisionFailed => "auth.provision_failed",
            AuthEventKind::ProbeFailed => "auth.probe_failed",
        }
    }

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
/// `auth_events`. Returns the new row id — after the absorption, the chain
/// `seq`, which is the view's `id` (G-ACCESS §6.6).
pub async fn record(tx: &mut Transaction<'_, Sqlite>, event: AuthEvent) -> sqlx::Result<i64> {
    use crate::access::audit::absorb::{shape, AuthEventsShape};
    if shape(&mut **tx).await? == AuthEventsShape::View {
        return record_chained(tx, event).await;
    }
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

/// The §6.1 `via` of a chained row: the root CLI, a password session's own
/// login / logout, or the system (failures, throttles, provisioning).
fn audit_via(event: &AuthEvent) -> crate::access::audit::AuditVia {
    use crate::access::audit::AuditVia;
    let via = event
        .detail
        .as_ref()
        .and_then(|d| d.get("via"))
        .and_then(serde_json::Value::as_str);
    match (via, event.kind) {
        (Some("cli"), _) => AuditVia::Cli,
        (_, AuthEventKind::LoginOk | AuthEventKind::Logout) => AuditVia::Session,
        _ => AuditVia::System,
    }
}

/// [`record`] after the absorption: one chained row (G-ACCESS §6.3), in the
/// caller's transaction. `username_tried` moves into `detail`, where the
/// view reads it. Authentication rows append even while the chain is
/// degraded (P-35); a process-local chain view is enough, because every
/// other writer verifies forward over these rows (§6.3 step 2, A-37).
async fn record_chained(tx: &mut Transaction<'_, Sqlite>, event: AuthEvent) -> sqlx::Result<i64> {
    use crate::access::audit::{chain::Chain, Event};
    let store_id: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
        .fetch_one(&mut **tx)
        .await?;
    let mut detail = match &event.detail {
        Some(serde_json::Value::Object(o)) => o.clone(),
        Some(other) => serde_json::Map::from_iter([("detail".to_string(), other.clone())]),
        None => serde_json::Map::new(),
    };
    if let Some(u) = &event.username_tried {
        detail.insert("username_tried".into(), serde_json::Value::from(u.clone()));
    }
    let mut ev = Event::new(event.kind.audit_kind(), audit_via(&event))
        .detail(serde_json::Value::Object(detail));
    ev.principal_id = event.principal_id.map(|id| id.to_string());
    ev.remote_addr = event.remote_addr;
    ev.user_agent = event.user_agent;
    let head = Chain::new(store_id)
        .append(&mut **tx, &ev)
        .await
        .map_err(|e| match e {
            crate::access::audit::chain::AppendError::Sql(e) => e,
            other => sqlx::Error::Protocol(other.to_string()),
        })?;
    Ok(head.seq)
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
