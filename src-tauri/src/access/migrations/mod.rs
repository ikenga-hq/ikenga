//! The `access` migration set (G-ACCESS §8.1).
//!
//! **Not** `src-tauri/migrations/` (that is `ikenga.db`'s set). It runs on
//! `<data-dir>/access.db` under T0 and on `operator/accounts.db` under T1,
//! where it shares WP-20's `_operator_migrations` bookkeeping table (R-1:
//! `(set, version)`; WP-20's rows are `set = 'accounts'`, these are
//! `set = 'access'`). Under T1 only the broker migrates; the root CLI
//! refuses a version it doesn't know.
//!
//! The whole pending batch runs in **one `BEGIN IMMEDIATE`** (G-PRINCIPAL
//! P-6), including the Rust post-step after `0001_core`: `store_meta`
//! (`store_id`, `tier`, `created_at`, and on T0 `owner_principal_id`), the T0
//! `host` device, and the genesis `store.created` audit row (§8.1).
//!
//! The runner is platform-neutral on purpose: `server::operator` (and its
//! runner) is Linux-only, while the T0 store exists on every OS. The
//! bookkeeping DDL is byte-identical to `operator::migrations`'s.

use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, Row, SqliteConnection};

use super::audit::{chain, AuditVia, Event};
use super::store::StoreTier;
use crate::executor::PrincipalId;

/// This set's name in `_operator_migrations`.
pub const SET: &str = "access";

#[path = "0002_absorb_auth_events.rs"]
mod absorb_auth_events;

/// What a step runs.
#[derive(Debug, Clone, Copy)]
pub enum Body {
    Sql(&'static str),
    /// `0002_absorb_auth_events` (WP-77): needs the chain's hash and the
    /// tier, so it is Rust (§8.1, §8.3).
    AbsorbAuthEvents,
}

/// One step: SQL, or a Rust step.
#[derive(Debug, Clone, Copy)]
pub struct Step {
    pub version: i64,
    pub name: &'static str,
    pub body: Body,
}

/// §8.1's table.
pub const STEPS: &[Step] = &[
    Step {
        version: 1,
        name: "0001_core",
        body: Body::Sql(include_str!("0001_core.sql")),
    },
    Step {
        version: 2,
        name: "0002_absorb_auth_events",
        body: Body::AbsorbAuthEvents,
    },
    // plans/pwa S2: Web Push subscriptions (`server::push`).
    Step {
        version: 3,
        name: "0003_push",
        body: Body::Sql(include_str!("0003_push.sql")),
    },
];

pub fn latest() -> i64 {
    STEPS.last().map(|s| s.version).unwrap_or(0)
}

/// Same DDL as `server::operator::migrations::CREATE_BOOKKEEPING` (R-1).
pub const CREATE_BOOKKEEPING: &str = "CREATE TABLE IF NOT EXISTS _operator_migrations (
  \"set\"     TEXT    NOT NULL,
  version     INTEGER NOT NULL,
  name        TEXT    NOT NULL,
  applied_at  INTEGER NOT NULL,
  PRIMARY KEY (\"set\", version)
)";

/// Where a store stands for the access set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetState {
    Fresh,
    Current,
    Behind(i64),
    /// A newer binary migrated it: nothing here may write it.
    Ahead(i64),
    Inconsistent(String),
}

/// What the post-step needs to seed a new T0 store.
#[derive(Debug, Clone)]
pub struct Seed {
    pub tier: StoreTier,
    /// T0 only: the host device's display name (§2.1).
    pub host_device_name: Option<String>,
    pub host_platform: Option<String>,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Read the access set's state. Read-only.
pub async fn state(conn: &mut SqliteConnection) -> Result<SetState, sqlx::Error> {
    let has_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_operator_migrations'",
    )
    .fetch_one(&mut *conn)
    .await?;
    if has_table == 0 {
        return Ok(SetState::Fresh);
    }
    let rows = sqlx::query(
        "SELECT version, name FROM _operator_migrations WHERE \"set\" = ? ORDER BY version",
    )
    .bind(SET)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(SetState::Fresh);
    }
    for (i, r) in rows.iter().enumerate() {
        let version: i64 = r.get(0);
        let name: String = r.get(1);
        if version != i as i64 + 1 {
            return Ok(SetState::Inconsistent(format!(
                "expected version {} at position {i}, found {version}",
                i + 1
            )));
        }
        if let Some(step) = STEPS.get(i) {
            if step.name != name {
                return Ok(SetState::Inconsistent(format!(
                    "version {version} is recorded as `{name}`, this binary calls it `{}`",
                    step.name
                )));
            }
        }
    }
    let top = rows.len() as i64;
    Ok(match top.cmp(&latest()) {
        std::cmp::Ordering::Equal => SetState::Current,
        std::cmp::Ordering::Less => SetState::Behind(top),
        std::cmp::Ordering::Greater => SetState::Ahead(top),
    })
}

/// Bring the access set to current, in one `BEGIN IMMEDIATE`, seeding a
/// fresh store. Returns the number of steps applied. `Ahead` and
/// `Inconsistent` are refused with nothing written.
pub async fn migrate(conn: &mut SqliteConnection, seed: &Seed) -> anyhow::Result<usize> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let from = match state(&mut tx).await? {
        SetState::Current => return Ok(0),
        SetState::Fresh => 0,
        SetState::Behind(n) => n,
        SetState::Ahead(n) => anyhow::bail!(
            "access store migration set is at version {n}, newer than this binary's {}; \
             refusing to touch it (run the matching ikenga-server)",
            latest()
        ),
        SetState::Inconsistent(why) => {
            anyhow::bail!("access store migration set is inconsistent: {why}")
        }
    };
    sqlx::query(CREATE_BOOKKEEPING).execute(&mut *tx).await?;
    let mut applied = 0;
    for step in STEPS.iter().filter(|s| s.version > from) {
        match step.body {
            Body::Sql(sql) => {
                sqlx::raw_sql(sql).execute(&mut *tx).await?;
            }
            Body::AbsorbAuthEvents => absorb_auth_events::run(&mut tx, seed.tier).await?,
        }
        if step.version == 1 {
            post_core(&mut tx, seed).await?;
        }
        sqlx::query(
            "INSERT INTO _operator_migrations (\"set\", version, name, applied_at) VALUES (?, ?, ?, ?)",
        )
        .bind(SET)
        .bind(step.version)
        .bind(step.name)
        .bind(now_secs())
        .execute(&mut *tx)
        .await?;
        applied += 1;
    }
    tx.commit().await?;
    Ok(applied)
}

/// §8.1's Rust post-step after `0001_core`, inside the migration's
/// transaction: `store_meta`, the T0 host device, and the genesis row.
async fn post_core(conn: &mut SqliteConnection, seed: &Seed) -> anyhow::Result<()> {
    let store_id = PrincipalId::new_v7().to_string();
    let created = now_ms();
    let mut meta = vec![
        ("store_id", store_id.clone()),
        ("tier", seed.tier.as_str().to_string()),
        ("created_at", created.to_string()),
    ];
    let mut host_device: Option<String> = None;
    let mut owner: Option<String> = None;
    if seed.tier == StoreTier::T0 {
        // Minted once, never re-minted for the life of this data dir (§2.1).
        let owner_id = PrincipalId::new_v7().to_string();
        meta.push(("owner_principal_id", owner_id.clone()));
        let device_id = PrincipalId::new_v7().to_string();
        sqlx::query(
            "INSERT INTO devices (device_id, principal_id, kind, name, platform, tier, paired_at) \
             VALUES (?, ?, 'host', ?, ?, 'full', ?)",
        )
        .bind(&device_id)
        .bind(&owner_id)
        .bind(
            seed.host_device_name
                .clone()
                .unwrap_or_else(|| "this computer".into()),
        )
        .bind(&seed.host_platform)
        .bind(created)
        .execute(&mut *conn)
        .await?;
        host_device = Some(device_id);
        owner = Some(owner_id);
    }
    for (k, v) in meta {
        sqlx::query("INSERT INTO store_meta (k, v) VALUES (?, ?)")
            .bind(k)
            .bind(v)
            .execute(&mut *conn)
            .await?;
    }
    let mut ev = Event::new("store.created", AuditVia::System).detail(serde_json::json!({
        "tier": seed.tier.as_str(),
        "schema": super::caps::ACCESS_SCHEMA_VERSION,
    }));
    ev.subject_principal_id = owner;
    ev.subject_device_id = host_device;
    chain::insert_row(conn, &store_id, None, &ev).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> SqliteConnection {
        SqliteConnection::connect("sqlite::memory:").await.unwrap()
    }

    fn t0() -> Seed {
        Seed {
            tier: StoreTier::T0,
            host_device_name: Some("ned-desktop".into()),
            host_platform: Some("linux".into()),
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
    async fn t0_migrates_once_and_seeds_owner_host_and_genesis() {
        let mut conn = mem().await;
        assert_eq!(migrate(&mut conn, &t0()).await.unwrap(), 3);
        assert_eq!(migrate(&mut conn, &t0()).await.unwrap(), 0);
        assert_eq!(state(&mut conn).await.unwrap(), SetState::Current);
        let tier: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'tier'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(tier, "t0");
        let owner: String =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'owner_principal_id'")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert!(owner.parse::<PrincipalId>().is_ok());
        let hosts: Vec<(String, String, String)> =
            sqlx::query_as("SELECT principal_id, tier, name FROM devices WHERE kind = 'host'")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(hosts, vec![(owner, "full".into(), "ned-desktop".into())]);
        let store_id: String = sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        let r = chain::verify_all(&mut conn, &store_id).await.unwrap();
        assert!(r.ok());
        assert_eq!(r.rows, 1);
        // A-28: no accounts table on T0, ever.
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='accounts'",
        )
        .fetch_one(&mut conn)
        .await
        .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn t1_has_no_owner_and_no_host_device() {
        let mut conn = mem().await;
        migrate(
            &mut conn,
            &Seed {
                tier: StoreTier::T1,
                host_device_name: None,
                host_platform: None,
            },
        )
        .await
        .unwrap();
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
        assert!(owner.is_none());
    }

    #[tokio::test]
    async fn a_newer_set_is_refused_untouched() {
        let mut conn = mem().await;
        migrate(&mut conn, &t0()).await.unwrap();
        sqlx::query("INSERT INTO _operator_migrations VALUES ('access', 4, '0004_future', 0)")
            .execute(&mut conn)
            .await
            .unwrap();
        assert_eq!(state(&mut conn).await.unwrap(), SetState::Ahead(4));
        assert!(migrate(&mut conn, &t0()).await.is_err());
    }

    /// §8.2 CHECKs the code relies on.
    #[tokio::test]
    async fn ddl_checks_hold() {
        let mut conn = mem().await;
        migrate(&mut conn, &t0()).await.unwrap();
        let owner: String =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'owner_principal_id'")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        // A second host row.
        let second_host = sqlx::query(
            "INSERT INTO devices (device_id, principal_id, kind, name, tier, paired_at) \
             VALUES (?, ?, 'host', 'x', 'full', 0)",
        )
        .bind(PrincipalId::new_v7().to_string())
        .bind(&owner)
        .execute(&mut conn)
        .await;
        assert!(second_host.is_err(), "devices_one_host");
        // secrets is never an override cap (A-4's SQL half).
        let secrets = sqlx::query(
            "INSERT INTO project_role_caps VALUES ('o/p', 'operator', 'secrets', 1, 0, 'x')",
        )
        .execute(&mut conn)
        .await;
        assert!(secrets.is_err());
        // A revoked row can't keep a secret.
        let revoked_with_secret = sqlx::query(
            "INSERT INTO devices (device_id, principal_id, kind, name, tier, paired_at, \
             secret_sha256, revoked_at) VALUES (?, ?, 'paired', 'p', 'view', 0, zeroblob(32), 1)",
        )
        .bind(PrincipalId::new_v7().to_string())
        .bind(&owner)
        .execute(&mut conn)
        .await;
        assert!(revoked_with_secret.is_err());
    }
}
