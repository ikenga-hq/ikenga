//! The access store (G-ACCESS §2.5, P-18): devices, memberships, policies,
//! invites and the audit chain.
//!
//! | | T0 | T1 |
//! |---|---|---|
//! | File | `<data-dir>/access.db` (+wal/shm), 0600 | `operator/accounts.db`, beside `accounts` |
//! | Opener | the **daemon only**, never a principal child | the **broker only** |
//! | Migrations | the `access` set | WP-20's `accounts` set, then `access` |
//!
//! Unreachable from RPC on T0: `fs_*` refuses `--data-dir` (`server::
//! reserved`) and `db_query`/`db_exec` open only `ikenga.db` — which is why
//! the chain does **not** live in `ikenga.db` (§2.5).
//!
//! A-32: desktop code never calls [`AccessStore::open_t0`]; the desktop's
//! `access_*` commands proxy to the daemon (P-20).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqliteConnection, SqlitePool};

use super::audit::chain::{Chain, VerifyReport};
use super::ctx::HostIdentity;
use super::migrations::{self, Seed};
use crate::executor::PrincipalId;

/// The T0 file name inside `--data-dir`.
pub const T0_FILE: &str = "access.db";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// `store_meta.tier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreTier {
    T0,
    T1,
}

impl StoreTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            StoreTier::T0 => "t0",
            StoreTier::T1 => "t1",
        }
    }
}

/// `store_meta`, read once after migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreMeta {
    pub store_id: String,
    pub tier: StoreTier,
    /// T0: the synthetic owner (§2.1). `None` on T1.
    pub owner_principal_id: Option<PrincipalId>,
    /// T0: the one `kind = 'host'` device row.
    pub host_device_id: Option<String>,
}

/// An open, migrated, verified access store.
#[derive(Clone)]
pub struct AccessStore {
    pool: SqlitePool,
    meta: StoreMeta,
    chain: Arc<Chain>,
}

impl std::fmt::Debug for AccessStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessStore")
            .field("meta", &self.meta)
            .finish()
    }
}

impl AccessStore {
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn meta(&self) -> &StoreMeta {
        &self.meta
    }

    pub fn chain(&self) -> &Arc<Chain> {
        &self.chain
    }

    /// `'ok' | 'degraded'` for `AccessStatus.store`.
    pub fn status(&self) -> (&'static str, Option<i64>) {
        match self.chain.degraded() {
            None => ("ok", None),
            Some(b) => ("degraded", Some(b.broken_at_seq)),
        }
    }

    /// The T0 path for a data dir.
    pub fn t0_path(data_dir: &Path) -> PathBuf {
        data_dir.join(T0_FILE)
    }

    /// Open (creating, 0600) `<data-dir>/access.db`, migrate the access set,
    /// read `store_meta`, and verify the chain (§6.4 "at every start").
    /// Called only by the T0 daemon's `run_server` — never by a principal
    /// child (A-32) and never by the desktop (P-20).
    pub async fn open_t0(data_dir: &Path, host: &HostIdentity) -> anyhow::Result<Self> {
        let path = Self::t0_path(data_dir);
        create_private(&path)?;
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(false)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        let seed = Seed {
            tier: StoreTier::T0,
            host_device_name: Some(host.device_name()),
            host_platform: Some(std::env::consts::OS.to_string()),
        };
        Self::finish_open(pool, StoreTier::T0, &seed).await
    }

    /// Attach the access set to the broker's `accounts.db` pool (T1): run
    /// the access migrations **after** WP-20's `accounts` set (R-1), then
    /// verify the chain. Only the broker calls this.
    pub async fn attach_t1(pool: SqlitePool) -> anyhow::Result<Self> {
        let seed = Seed {
            tier: StoreTier::T1,
            host_device_name: None,
            host_platform: None,
        };
        Self::finish_open(pool, StoreTier::T1, &seed).await
    }

    /// An in-memory T0 store for tests.
    #[cfg(test)]
    pub async fn memory_t0() -> Self {
        let options = SqliteConnectOptions::new()
            .filename(":memory:")
            .shared_cache(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .unwrap();
        let seed = Seed {
            tier: StoreTier::T0,
            host_device_name: Some("test-host".into()),
            host_platform: Some("test".into()),
        };
        Self::finish_open(pool, StoreTier::T0, &seed).await.unwrap()
    }

    /// `ikenga-server audit …` (WP-77): open an **existing** store file as
    /// the root CLI (T1 `operator/accounts.db`, or any store named with
    /// `--file`). Never creates or migrates — the access set must be exactly
    /// current (§8.1: the CLI refuses a version it doesn't know; only the
    /// broker / daemon migrates). Its chain starts with no known head, like
    /// any second writer (§6.3 step 2). `read_only` (`audit verify`, review
    /// m-8) opens it `SQLITE_OPEN_READONLY`.
    pub async fn open_cli(path: &Path, read_only: bool) -> anyhow::Result<Self> {
        if !path.is_file() {
            anyhow::bail!("no access store at {}", path.display());
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .read_only(read_only)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;
        let mut conn = pool.acquire().await?;
        match migrations::state(&mut conn).await? {
            migrations::SetState::Current => {}
            migrations::SetState::Fresh => anyhow::bail!(
                "{} has no access store (the server has never started on it)",
                path.display()
            ),
            other => anyhow::bail!(
                "{}: the access store is {other:?} for this binary; start the matching server \
                 once (only it migrates), then retry",
                path.display()
            ),
        }
        let meta = read_meta(&mut conn).await?;
        drop(conn);
        let chain = Arc::new(Chain::new(meta.store_id.clone()));
        chain.attach(pool.clone());
        Ok(Self { pool, meta, chain })
    }

    /// The same store with a fresh chain view: a second writer in tests.
    #[cfg(test)]
    pub fn clone_with_fresh_chain(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            meta: self.meta.clone(),
            chain: Arc::new(Chain::new(self.meta.store_id.clone())),
        }
    }

    async fn finish_open(pool: SqlitePool, tier: StoreTier, seed: &Seed) -> anyhow::Result<Self> {
        let mut conn = pool.acquire().await?;
        migrations::migrate(&mut conn, seed).await?;
        let meta = read_meta(&mut conn).await?;
        if meta.tier != tier {
            anyhow::bail!(
                "access store is a {} store, opened as {} (refusing)",
                meta.tier.as_str(),
                tier.as_str()
            );
        }
        let chain = Arc::new(Chain::new(meta.store_id.clone()));
        chain.attach(pool.clone());
        let report: VerifyReport = chain.verify_boot(&mut conn).await?;
        if report.ok() {
            tracing::info!(
                "access store ({}): audit chain verified, {} rows",
                tier.as_str(),
                report.rows
            );
        }
        drop(conn);
        Ok(Self { pool, meta, chain })
    }
}

/// Read `store_meta` (+ the T0 host device id).
pub async fn read_meta(conn: &mut SqliteConnection) -> anyhow::Result<StoreMeta> {
    let rows = sqlx::query("SELECT k, v FROM store_meta")
        .fetch_all(&mut *conn)
        .await?;
    let get = |key: &str| {
        rows.iter()
            .find(|r| r.get::<String, _>(0) == key)
            .map(|r| r.get::<String, _>(1))
    };
    let store_id = get("store_id").ok_or_else(|| anyhow::anyhow!("store_meta has no store_id"))?;
    let tier = match get("tier").as_deref() {
        Some("t0") => StoreTier::T0,
        Some("t1") => StoreTier::T1,
        other => anyhow::bail!("store_meta.tier is {other:?}"),
    };
    let owner_principal_id = match get("owner_principal_id") {
        Some(s) => Some(
            s.parse::<PrincipalId>()
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
        None => None,
    };
    let host_device_id: Option<String> =
        sqlx::query_scalar("SELECT device_id FROM devices WHERE kind = 'host'")
            .fetch_optional(&mut *conn)
            .await?;
    Ok(StoreMeta {
        store_id,
        tier,
        owner_principal_id,
        host_device_id,
    })
}

/// Create the file owner-only before SQLite first opens it (and tighten an
/// existing one).
fn create_private(path: &Path) -> anyhow::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> HostIdentity {
        HostIdentity {
            username: "ned".into(),
            hostname: "ned-desktop".into(),
        }
    }

    /// A-28: the owner id is stable across restarts; no accounts table.
    #[tokio::test]
    async fn t0_store_is_private_stable_and_account_free() {
        let tmp = tempfile::tempdir().unwrap();
        let a = AccessStore::open_t0(tmp.path(), &host()).await.unwrap();
        let owner = a.meta().owner_principal_id.unwrap();
        let host_dev = a.meta().host_device_id.clone().unwrap();
        a.pool().close().await;
        let b = AccessStore::open_t0(tmp.path(), &host()).await.unwrap();
        assert_eq!(b.meta().owner_principal_id, Some(owner));
        assert_eq!(b.meta().host_device_id.as_deref(), Some(host_dev.as_str()));
        assert_eq!(b.status(), ("ok", None));
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='accounts'",
        )
        .fetch_one(b.pool())
        .await
        .unwrap();
        assert_eq!(n, 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(AccessStore::t0_path(tmp.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn a_t1_store_is_not_opened_as_t0() {
        let tmp = tempfile::tempdir().unwrap();
        let path = AccessStore::t0_path(tmp.path());
        create_private(&path).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        AccessStore::attach_t1(pool.clone()).await.unwrap();
        pool.close().await;
        assert!(AccessStore::open_t0(tmp.path(), &host()).await.is_err());
    }
}
