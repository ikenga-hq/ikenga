//! Embedded migrations for `operator/accounts.db` (G-PRINCIPAL §6.1, P-6).
//!
//! This is **not** `src-tauri/migrations/` — that set is `ikenga.db`'s, and
//! this store never shares a runner with it. Bookkeeping lives in
//! `_operator_migrations`, which carries a migration-**set** dimension
//! (G-ACCESS request R-1) so more than one set can share the file: WP-20's
//! rows are `set = 'accounts'`; G-ACCESS's `access` set (WP-74,
//! `src-tauri/src/access/migrations/`) records its own rows in the same table.
//!
//! Every pending batch runs inside **one `BEGIN IMMEDIATE`**, version check
//! included, so two processes racing a migration serialize on SQLite's write
//! lock instead of repeating `db.rs`'s check-then-write race (`db.rs:604-627`).

use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, Row, SqliteConnection};

/// One step of a set. Steps are applied in `version` order; versions start at
/// 1 and have no gaps.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// A named, ordered list of steps sharing `_operator_migrations`.
#[derive(Debug, Clone, Copy)]
pub struct MigrationSet {
    pub name: &'static str,
    pub steps: &'static [Migration],
}

impl MigrationSet {
    pub fn latest(&self) -> i64 {
        self.steps.last().map(|m| m.version).unwrap_or(0)
    }
}

/// WP-20's set: the `accounts` and `auth_events` tables (§6.1).
pub const ACCOUNTS: MigrationSet = MigrationSet {
    name: "accounts",
    steps: &[Migration {
        version: 1,
        name: "0001_accounts",
        sql: include_str!("accounts/0001_accounts.sql"),
    }],
};

/// The bookkeeping table (R-1). Created by whichever set runs first, inside
/// that set's transaction.
pub const CREATE_BOOKKEEPING: &str = "CREATE TABLE IF NOT EXISTS _operator_migrations (
  \"set\"     TEXT    NOT NULL,
  version     INTEGER NOT NULL,
  name        TEXT    NOT NULL,
  applied_at  INTEGER NOT NULL,
  PRIMARY KEY (\"set\", version)
)";

/// Where a store stands relative to a set this binary knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
    /// No step of the set has been applied (a new store).
    Fresh,
    /// Every known step is applied, and nothing newer.
    Current,
    /// Some known steps are missing. Only the broker migrates (§6.1).
    Behind { applied: i64, latest: i64 },
    /// The store holds a step this binary does not know: a newer binary
    /// migrated it. Nothing may write it (§6.1: "the CLI refuses to run
    /// against a schema version it doesn't match").
    Ahead { applied: i64, latest: i64 },
    /// The recorded versions are not a gap-free prefix of the set, or a
    /// recorded name differs from this binary's. Never auto-repaired.
    Inconsistent(String),
}

#[derive(Debug)]
pub enum MigrationError {
    Sql(sqlx::Error),
    /// The store does not match this binary (see [`SchemaState`]).
    Mismatch {
        set: &'static str,
        state: SchemaState,
    },
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationError::Sql(e) => write!(f, "operator store: {e}"),
            MigrationError::Mismatch { set, state } => match state {
                SchemaState::Ahead { applied, latest } => write!(
                    f,
                    "operator store migration set `{set}` is at version {applied}, newer than \
                     this binary's {latest}; refusing to touch it (run the matching ikenga-server)"
                ),
                SchemaState::Behind { applied, latest } => write!(
                    f,
                    "operator store migration set `{set}` is at version {applied}, older than \
                     this binary's {latest}; only the T1 broker migrates — start it once, then retry"
                ),
                SchemaState::Inconsistent(why) => {
                    write!(f, "operator store migration set `{set}` is inconsistent: {why}")
                }
                SchemaState::Fresh => write!(
                    f,
                    "operator store migration set `{set}` has never been initialised; create the \
                     first account with `ikenga-server accounts create <name> --admin`"
                ),
                SchemaState::Current => {
                    write!(f, "operator store migration set `{set}`: unexpected state {state:?}")
                }
            },
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<sqlx::Error> for MigrationError {
    fn from(e: sqlx::Error) -> Self {
        MigrationError::Sql(e)
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read where `conn` stands for `set`. Read-only; safe outside a transaction,
/// but [`apply`] re-reads it inside its `BEGIN IMMEDIATE` before writing.
pub async fn state(
    conn: &mut SqliteConnection,
    set: &MigrationSet,
) -> Result<SchemaState, sqlx::Error> {
    let has_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_operator_migrations'",
    )
    .fetch_one(&mut *conn)
    .await?;
    if has_table == 0 {
        return Ok(SchemaState::Fresh);
    }
    let rows = sqlx::query(
        "SELECT version, name FROM _operator_migrations WHERE \"set\" = ? ORDER BY version",
    )
    .bind(set.name)
    .fetch_all(&mut *conn)
    .await?;
    let applied: Vec<(i64, String)> = rows
        .iter()
        .map(|r| (r.get::<i64, _>(0), r.get::<String, _>(1)))
        .collect();
    Ok(classify(set, &applied))
}

fn classify(set: &MigrationSet, applied: &[(i64, String)]) -> SchemaState {
    if applied.is_empty() {
        return SchemaState::Fresh;
    }
    for (i, (version, name)) in applied.iter().enumerate() {
        let expected = i as i64 + 1;
        if *version != expected {
            return SchemaState::Inconsistent(format!(
                "expected version {expected} at position {i}, found {version}"
            ));
        }
        if let Some(step) = set.steps.get(i) {
            if step.name != name {
                return SchemaState::Inconsistent(format!(
                    "version {version} is recorded as `{name}`, this binary calls it `{}`",
                    step.name
                ));
            }
        }
    }
    let top = applied.len() as i64;
    let latest = set.latest();
    match top.cmp(&latest) {
        std::cmp::Ordering::Equal => SchemaState::Current,
        std::cmp::Ordering::Less => SchemaState::Behind {
            applied: top,
            latest,
        },
        std::cmp::Ordering::Greater => SchemaState::Ahead {
            applied: top,
            latest,
        },
    }
}

/// Which states [`apply`] may move forward from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The broker: apply every pending step (`Fresh` or `Behind` → `Current`).
    Migrate,
    /// The root CLI: never migrate an existing set (§6.1). It may only
    /// initialise a store no broker has touched yet (`Fresh` → `Current`), so
    /// `accounts create --admin` works before the first T1 boot; any other
    /// mismatch is refused.
    InitialiseOnly,
}

/// Bring `set` to [`SchemaState::Current`] under `policy`, in one
/// `BEGIN IMMEDIATE`. Returns the number of steps applied. `Ahead` and
/// `Inconsistent` are always refused, with nothing written.
pub async fn apply(
    conn: &mut SqliteConnection,
    set: &MigrationSet,
    policy: Policy,
) -> Result<usize, MigrationError> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    // The check happens under the write lock, so a concurrent migrator can
    // only be observed as "already applied", never raced.
    let current = state(&mut tx, set).await?;
    let from = match (&current, policy) {
        (SchemaState::Current, _) => return Ok(0),
        (SchemaState::Fresh, _) => 0,
        (SchemaState::Behind { applied, .. }, Policy::Migrate) => *applied,
        _ => {
            return Err(MigrationError::Mismatch {
                set: set.name,
                state: current,
            })
        }
    };
    sqlx::query(CREATE_BOOKKEEPING).execute(&mut *tx).await?;
    let mut applied = 0;
    for step in set.steps.iter().filter(|s| s.version > from) {
        sqlx::raw_sql(step.sql).execute(&mut *tx).await?;
        sqlx::query(
            "INSERT INTO _operator_migrations (\"set\", version, name, applied_at) VALUES (?, ?, ?, ?)",
        )
        .bind(set.name)
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

/// Refuse unless `set` is exactly [`SchemaState::Current`].
pub async fn require_current(
    conn: &mut SqliteConnection,
    set: &MigrationSet,
) -> Result<(), MigrationError> {
    match state(conn, set).await? {
        SchemaState::Current => Ok(()),
        other => Err(MigrationError::Mismatch {
            set: set.name,
            state: other,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem() -> SqliteConnection {
        SqliteConnection::connect("sqlite::memory:").await.unwrap()
    }

    #[test]
    fn steps_are_a_gap_free_sequence_from_one() {
        for (i, step) in ACCOUNTS.steps.iter().enumerate() {
            assert_eq!(step.version, i as i64 + 1, "{}", step.name);
            assert!(step.name.starts_with(&format!("{:04}_", step.version)));
        }
    }

    #[tokio::test]
    async fn fresh_store_migrates_once_and_records_the_set_dimension() {
        let mut conn = mem().await;
        assert_eq!(
            state(&mut conn, &ACCOUNTS).await.unwrap(),
            SchemaState::Fresh
        );
        assert_eq!(
            apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap(),
            1
        );
        assert_eq!(
            apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap(),
            0
        );
        assert_eq!(
            state(&mut conn, &ACCOUNTS).await.unwrap(),
            SchemaState::Current
        );
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as("SELECT \"set\", version, name FROM _operator_migrations")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(rows, vec![("accounts".into(), 1, "0001_accounts".into())]);
    }

    #[tokio::test]
    async fn another_set_shares_the_table_without_colliding() {
        static OTHER: MigrationSet = MigrationSet {
            name: "access",
            steps: &[Migration {
                version: 1,
                name: "0001_core",
                sql: "CREATE TABLE other_thing (x INTEGER)",
            }],
        };
        let mut conn = mem().await;
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        assert_eq!(state(&mut conn, &OTHER).await.unwrap(), SchemaState::Fresh);
        apply(&mut conn, &OTHER, Policy::Migrate).await.unwrap();
        assert_eq!(
            state(&mut conn, &OTHER).await.unwrap(),
            SchemaState::Current
        );
        assert_eq!(
            state(&mut conn, &ACCOUNTS).await.unwrap(),
            SchemaState::Current
        );
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _operator_migrations WHERE version = 1")
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(n, 2, "(set, version) is the key, not version alone");
    }

    #[tokio::test]
    async fn a_newer_store_is_refused_and_left_untouched() {
        let mut conn = mem().await;
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        sqlx::query("INSERT INTO _operator_migrations VALUES ('accounts', 2, '0002_future', 0)")
            .execute(&mut conn)
            .await
            .unwrap();
        for policy in [Policy::Migrate, Policy::InitialiseOnly] {
            let err = apply(&mut conn, &ACCOUNTS, policy).await.unwrap_err();
            assert!(
                matches!(
                    err,
                    MigrationError::Mismatch {
                        state: SchemaState::Ahead {
                            applied: 2,
                            latest: 1
                        },
                        ..
                    }
                ),
                "{err}"
            );
        }
        assert!(require_current(&mut conn, &ACCOUNTS).await.is_err());
    }

    #[test]
    fn behind_and_inconsistent_are_classified() {
        static TWO: MigrationSet = MigrationSet {
            name: "accounts",
            steps: &[
                Migration {
                    version: 1,
                    name: "0001_a",
                    sql: "",
                },
                Migration {
                    version: 2,
                    name: "0002_b",
                    sql: "",
                },
            ],
        };
        assert_eq!(
            classify(&TWO, &[(1, "0001_a".into())]),
            SchemaState::Behind {
                applied: 1,
                latest: 2
            }
        );
        assert!(matches!(
            classify(&TWO, &[(2, "0002_b".into())]),
            SchemaState::Inconsistent(_)
        ));
        assert!(matches!(
            classify(&TWO, &[(1, "0001_renamed".into())]),
            SchemaState::Inconsistent(_)
        ));
    }

    #[tokio::test]
    async fn the_cli_policy_initialises_but_never_migrates() {
        static TWO: MigrationSet = MigrationSet {
            name: "accounts",
            steps: &[
                Migration {
                    version: 1,
                    name: "0001_accounts",
                    sql: "CREATE TABLE a (x)",
                },
                Migration {
                    version: 2,
                    name: "0002_more",
                    sql: "CREATE TABLE b (x)",
                },
            ],
        };
        static ONE: MigrationSet = MigrationSet {
            name: "accounts",
            steps: &[Migration {
                version: 1,
                name: "0001_accounts",
                sql: "CREATE TABLE a (x)",
            }],
        };
        let mut conn = mem().await;
        assert_eq!(
            apply(&mut conn, &ONE, Policy::InitialiseOnly)
                .await
                .unwrap(),
            1
        );
        let err = apply(&mut conn, &TWO, Policy::InitialiseOnly)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("only the T1 broker migrates"),
            "{err}"
        );
        assert_eq!(apply(&mut conn, &TWO, Policy::Migrate).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn accounts_rows_can_never_be_deleted() {
        let mut conn = mem().await;
        apply(&mut conn, &ACCOUNTS, Policy::Migrate).await.unwrap();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             created_at, updated_at) VALUES (?, 'ada', 'ik-ada', 20000, 20000, '/h', 0, 0)",
        )
        .bind(crate::executor::PrincipalId::new_v7().to_string())
        .execute(&mut conn)
        .await
        .unwrap();
        let err = sqlx::query("DELETE FROM accounts")
            .execute(&mut conn)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("never deleted"), "{err}");
        // Review F13: nor can a tombstone's identity be rewritten.
        let other_id = crate::executor::PrincipalId::new_v7().to_string();
        for stmt in [
            "UPDATE accounts SET unix_uid = 20001".to_string(),
            "UPDATE accounts SET unix_name = 'ik-eve'".to_string(),
            format!("UPDATE accounts SET principal_id = '{other_id}'"),
        ] {
            let err = sqlx::query(&stmt).execute(&mut conn).await.unwrap_err();
            assert!(err.to_string().contains("immutable"), "{stmt}: {err}");
        }
        // A no-op write of the same value, and the mutable columns, pass.
        sqlx::query(
            "UPDATE accounts SET unix_uid = unix_uid, username = 'Ada', session_epoch = 1, \
             unix_gid = 20002",
        )
        .execute(&mut conn)
        .await
        .unwrap();
    }
}
