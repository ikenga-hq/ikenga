//! The `access` migration set (G-ACCESS §8.1).
//!
//! Not `src-tauri/migrations/` (that is `ikenga.db`'s). The set runs on the
//! T0 `<data-dir>/access.db` and on the T1 `operator/accounts.db`, beside
//! WP-20's `accounts` set, and records each step as `("access", version,
//! name, applied_at)` in `_operator_migrations` (R-1). On T0 this runner
//! creates that table itself; the DDL is the same as WP-20's.
//!
//! The whole pending batch runs in **one `BEGIN IMMEDIATE`** (G-PRINCIPAL
//! P-6), version check included. After `0001_core`, a Rust post-step writes
//! `store_meta` (`store_id`, `tier`, `created_at`, and on T0
//! `owner_principal_id`), the T0 `host` device and the genesis
//! `store.created` row, in the same transaction.
//!
//! WP-77 appends `0002_absorb_auth_events` (a Rust step) in Wave 5. A store
//! holding a step this binary doesn't know is refused, never written.

use sqlx::{Connection, Row, SqliteConnection};

use super::audit::chain::{self, now_ms, Chain, Event};
use crate::executor::PrincipalId;

/// The set's name in `_operator_migrations`.
pub const SET: &str = "access";

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const STEPS: &[Migration] = &[Migration {
    version: 1,
    name: "0001_core",
    sql: include_str!("0001_core.sql"),
}];

/// Same table, same columns as `server::operator::migrations`
/// (`CREATE … IF NOT EXISTS`, so whichever set runs first creates it).
const CREATE_BOOKKEEPING: &str = "CREATE TABLE IF NOT EXISTS _operator_migrations (
  \"set\"     TEXT    NOT NULL,
  version     INTEGER NOT NULL,
  name        TEXT    NOT NULL,
  applied_at  INTEGER NOT NULL,
  PRIMARY KEY (\"set\", version)
)";

/// Which store is being migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreTier {
    /// `<data-dir>/access.db`: mints the synthetic owner and the host device.
    T0 { host_name: String, platform: String },
    /// `operator/accounts.db`: no owner, no host device (accounts are
    /// principals there).
    T1,
}

impl StoreTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            StoreTier::T0 { .. } => "t0",
            StoreTier::T1 => "t1",
        }
    }
}

#[derive(Debug)]
pub enum MigrateError {
    Sql(sqlx::Error),
    /// The store holds steps this binary doesn't know, or an inconsistent
    /// record. Nothing was written.
    Mismatch(String),
    Audit(chain::AppendError),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrateError::Sql(e) => write!(f, "access store: {e}"),
            MigrateError::Mismatch(why) => write!(f, "access store: {why}"),
            MigrateError::Audit(e) => write!(f, "access store: {e}"),
        }
    }
}

impl std::error::Error for MigrateError {}

impl From<sqlx::Error> for MigrateError {
    fn from(e: sqlx::Error) -> Self {
        MigrateError::Sql(e)
    }
}

/// Bring the `access` set current, in one `BEGIN IMMEDIATE`. Returns the
/// number of steps applied (0 when already current).
pub async fn migrate(conn: &mut SqliteConnection, tier: &StoreTier) -> Result<usize, MigrateError> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query(CREATE_BOOKKEEPING).execute(&mut *tx).await?;
    let rows = sqlx::query(
        "SELECT version, name FROM _operator_migrations WHERE \"set\" = ? ORDER BY version",
    )
    .bind(SET)
    .fetch_all(&mut *tx)
    .await?;
    let applied: Vec<(i64, String)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    for (i, (version, name)) in applied.iter().enumerate() {
        let expected = i as i64 + 1;
        if *version != expected {
            return Err(MigrateError::Mismatch(format!(
                "migration set `access` is inconsistent: expected version {expected}, found {version}"
            )));
        }
        match STEPS.get(i) {
            Some(step) if step.name != name => {
                return Err(MigrateError::Mismatch(format!(
                    "migration set `access` records version {version} as `{name}`, this binary \
                     calls it `{}`",
                    step.name
                )))
            }
            None => {
                return Err(MigrateError::Mismatch(format!(
                    "migration set `access` is at version {}, newer than this binary's {}; \
                     refusing to touch it",
                    applied.len(),
                    STEPS.len()
                )))
            }
            _ => {}
        }
    }
    let from = applied.len() as i64;
    let mut n = 0;
    for step in STEPS.iter().filter(|s| s.version > from) {
        sqlx::raw_sql(step.sql).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO _operator_migrations (\"set\", version, name, applied_at) VALUES (?, ?, ?, ?)",
        )
        .bind(SET)
        .bind(step.version)
        .bind(step.name)
        .bind(now_ms() / 1000)
        .execute(&mut *tx)
        .await?;
        if step.version == 1 {
            post_core(&mut tx, tier).await?;
        }
        n += 1;
    }
    tx.commit().await?;
    Ok(n)
}

/// The Rust post-step of `0001_core` (§8.1): store identity, the T0 synthetic
/// owner and host device, and the genesis row.
async fn post_core(conn: &mut SqliteConnection, tier: &StoreTier) -> Result<(), MigrateError> {
    let now = now_ms();
    let store_id = uuid::Uuid::now_v7().to_string();
    for (k, v) in [
        ("store_id", store_id.clone()),
        ("tier", tier.as_str().to_string()),
        ("created_at", now.to_string()),
    ] {
        sqlx::query("INSERT INTO store_meta (k, v) VALUES (?, ?)")
            .bind(k)
            .bind(v)
            .execute(&mut *conn)
            .await?;
    }
    let mut owner = None;
    if let StoreTier::T0 {
        host_name,
        platform,
    } = tier
    {
        // §2.1: minted once, for the life of this data dir. Never an
        // `accounts` row (there is no such table here).
        let owner_id = PrincipalId::new_v7().to_string();
        sqlx::query("INSERT INTO store_meta (k, v) VALUES ('owner_principal_id', ?)")
            .bind(&owner_id)
            .execute(&mut *conn)
            .await?;
        sqlx::query(
            "INSERT INTO devices (device_id, principal_id, kind, name, platform, tier, paired_at) \
             VALUES (?, ?, 'host', ?, ?, 'full', ?)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&owner_id)
        .bind(super::devices::sanitize_name(host_name))
        .bind(platform)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        owner = Some(owner_id);
    }
    let chain = Chain::new(store_id);
    chain::append(
        conn,
        &chain,
        Event::new("store.created", "system")
            .actor(owner, None)
            .detail(&serde_json::json!({ "tier": tier.as_str() })),
    )
    .await
    .map_err(MigrateError::Audit)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> SqliteConnection {
        SqliteConnection::connect("sqlite::memory:").await.unwrap()
    }

    fn t0() -> StoreTier {
        StoreTier::T0 {
            host_name: "ned-desktop".into(),
            platform: "linux".into(),
        }
    }

    #[test]
    fn steps_are_gap_free_from_one() {
        for (i, s) in STEPS.iter().enumerate() {
            assert_eq!(s.version, i as i64 + 1);
            assert!(s.name.starts_with(&format!("{:04}_", s.version)));
        }
    }

    #[tokio::test]
    async fn a_fresh_t0_store_gets_identity_owner_host_and_genesis_once() {
        let mut conn = mem().await;
        assert_eq!(migrate(&mut conn, &t0()).await.unwrap(), 1);
        assert_eq!(migrate(&mut conn, &t0()).await.unwrap(), 0);
        let owner: String =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'owner_principal_id'")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert!(owner.parse::<PrincipalId>().is_ok());
        let (kind, tier, principal): (String, String, String) =
            sqlx::query_as("SELECT kind, tier, principal_id FROM devices")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!((kind.as_str(), tier.as_str()), ("host", "full"));
        assert_eq!(principal, owner);
        let store_id: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let v = chain::verify(&mut conn, &store_id).await.unwrap();
        assert_eq!(v.rows, 1);
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as("SELECT \"set\", version, name FROM _operator_migrations")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(rows, vec![("access".into(), 1, "0001_core".into())]);
    }

    /// A-28: the T0 store has no `accounts` table.
    #[tokio::test]
    async fn the_t0_store_has_no_accounts_table() {
        let mut conn = mem().await;
        migrate(&mut conn, &t0()).await.unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'accounts'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn a_t1_store_has_no_owner_and_no_host() {
        let mut conn = mem().await;
        migrate(&mut conn, &StoreTier::T1).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let owner: Option<String> =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'owner_principal_id'")
                .fetch_optional(&mut conn)
                .await
                .unwrap();
        assert_eq!(owner, None);
    }

    #[tokio::test]
    async fn a_newer_store_is_refused_untouched() {
        let mut conn = mem().await;
        migrate(&mut conn, &t0()).await.unwrap();
        sqlx::query("INSERT INTO _operator_migrations VALUES ('access', 2, '0002_future', 0)")
            .execute(&mut conn)
            .await
            .unwrap();
        let err = migrate(&mut conn, &t0()).await.unwrap_err();
        assert!(err.to_string().contains("newer"), "{err}");
    }

    /// §4.1 / A-4 (the SQL half): the overrides table can't hold `secrets`;
    /// the device CHECKs hold.
    #[tokio::test]
    async fn ddl_checks_hold() {
        let mut conn = mem().await;
        migrate(&mut conn, &StoreTier::T1).await.unwrap();
        let err = sqlx::query(
            "INSERT INTO project_role_caps VALUES ('o/p', 'operator', 'secrets', 1, 0, 'x')",
        )
        .execute(&mut conn)
        .await
        .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
        // A host device must be full and secret-less; only one host.
        let err = sqlx::query(
            "INSERT INTO devices (device_id, principal_id, kind, name, tier, paired_at) \
             VALUES (?, ?, 'host', 'h', 'view', 0)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(PrincipalId::new_v7().to_string())
        .execute(&mut conn)
        .await
        .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
    }
}
