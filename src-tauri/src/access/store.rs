//! The access store (G-ACCESS §2.5, P-18).
//!
//! | | T0 | T1 |
//! |---|---|---|
//! | file | `<data-dir>/access.db` (+wal/shm), mode 0600 | `operator/accounts.db`, beside `accounts` |
//! | opener | the daemon process only — never a principal child (§1.7, A-32), never the desktop (P-20) | the broker only |
//!
//! Opening migrates the `access` set (§8.1) and walks the audit chain from
//! the genesis (§6.4). A failed walk does not stop the process: the store
//! comes up `degraded`, and every access-changing operation is then refused
//! with `audit_unavailable` (P-35).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::audit::chain::{Broken, Chain};
use super::migrations::{self, StoreTier};
use crate::executor::PrincipalId;

/// The T0 store's file name inside `--data-dir`.
pub const T0_FILE: &str = "access.db";

/// An open access store.
pub struct AccessStore {
    pub pool: SqlitePool,
    pub store_id: String,
    /// `"t0"` or `"t1"`, as recorded in `store_meta`.
    pub tier: &'static str,
    /// T0: the synthetic owner (§2.1). T1: `None`.
    pub owner: Option<PrincipalId>,
    /// T0: the `kind = 'host'` device every operator-bearer request is
    /// attributed to. T1: `None`.
    pub host_device_id: Option<String>,
    pub chain: Chain,
    /// §3.9: `last_seen_*` is written at most once per 60 s per device.
    last_seen: Mutex<HashMap<String, Instant>>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for AccessStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessStore")
            .field("store_id", &self.store_id)
            .field("tier", &self.tier)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// How often `last_seen_at` may be written per device (§3.9).
pub const LAST_SEEN_EVERY: Duration = Duration::from_secs(60);

/// The OS hostname, for the T0 host device's name (§2.1).
pub fn host_name() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: `buf` is valid for `buf.len()` bytes; gethostname writes a
        // NUL-terminated name (truncated if longer) and returns 0 on success.
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        if rc == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "this computer".to_string())
}

/// Create the store file owner-only before SQLite does (it would use the
/// umask), as WP-20 does for `sessions.db`.
fn create_private(path: &Path) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        opts.open(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        opts.open(path)?;
    }
    Ok(())
}

impl AccessStore {
    /// T0: open (creating) `<data_dir>/access.db`. Only the daemon calls
    /// this, and never as a principal child (`access::Runtime::for_daemon`).
    pub async fn open_t0(data_dir: &Path) -> anyhow::Result<Arc<AccessStore>> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(T0_FILE);
        create_private(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
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
        let tier = StoreTier::T0 {
            host_name: host_name(),
            platform: std::env::consts::OS.to_string(),
        };
        let store = Self::open_pool(pool, tier, Some(path)).await?;
        Ok(store)
    }

    /// T1: the broker's `operator/accounts.db` pool, after WP-20's
    /// `accounts` set (R-1).
    pub async fn open_t1(pool: SqlitePool) -> anyhow::Result<Arc<AccessStore>> {
        Self::open_pool(pool, StoreTier::T1, None).await
    }

    /// Migrate, load the identity, walk the chain.
    pub async fn open_pool(
        pool: SqlitePool,
        tier: StoreTier,
        path: Option<PathBuf>,
    ) -> anyhow::Result<Arc<AccessStore>> {
        let mut conn = pool.acquire().await?;
        migrations::migrate(&mut conn, &tier).await?;
        let meta: HashMap<String, String> = sqlx::query_as("SELECT k, v FROM store_meta")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
        let store_id = meta
            .get("store_id")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("access store has no store_id"))?;
        let recorded = meta.get("tier").map(String::as_str).unwrap_or_default();
        if recorded != tier.as_str() {
            anyhow::bail!(
                "access store {} was created for tier {recorded:?}, not {}; refusing it",
                path.as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                tier.as_str()
            );
        }
        let owner = match meta.get("owner_principal_id") {
            Some(s) => Some(
                s.parse::<PrincipalId>()
                    .map_err(|e| anyhow::anyhow!("access store owner id: {e}"))?,
            ),
            None => None,
        };
        if matches!(tier, StoreTier::T0 { .. }) && owner.is_none() {
            anyhow::bail!("T0 access store has no owner_principal_id");
        }
        let host_device_id: Option<String> =
            sqlx::query_scalar("SELECT device_id FROM devices WHERE kind = 'host'")
                .fetch_optional(&mut *conn)
                .await?;
        let chain = Chain::new(store_id.clone());
        match chain.boot_verify(&mut conn).await {
            Ok(v) => tracing::info!(
                "access store {}: audit chain verified ({} rows)",
                store_id,
                v.rows
            ),
            Err(b) => tracing::error!(
                "access store {}: audit chain broken at #{} ({}) — access changes refused until \
                 an operator reseals it",
                store_id,
                b.at_seq,
                b.reason
            ),
        }
        drop(conn);
        Ok(Arc::new(AccessStore {
            pool,
            store_id,
            tier: if matches!(tier, StoreTier::T0 { .. }) {
                "t0"
            } else {
                "t1"
            },
            owner,
            host_device_id,
            chain,
            last_seen: Mutex::new(HashMap::new()),
            path,
        }))
    }

    /// An access-changing write: `BEGIN IMMEDIATE` (§6.3). The caller
    /// appends its audit row through [`super::audit::append`] on the same
    /// transaction, commits, then calls `self.chain.committed(head)`.
    pub async fn begin(&self) -> sqlx::Result<Transaction<'static, Sqlite>> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    /// `Some` while the audit chain is broken (§6.4).
    pub fn degraded(&self) -> Option<Broken> {
        self.chain.degraded()
    }

    /// The §3.9 in-memory gate: true at most once per [`LAST_SEEN_EVERY`]
    /// per device.
    pub fn last_seen_due(&self, device_id: &str, now: Instant) -> bool {
        let mut seen = self.last_seen.lock().unwrap_or_else(|e| e.into_inner());
        match seen.get(device_id) {
            Some(at) if now.duration_since(*at) < LAST_SEEN_EVERY => false,
            _ => {
                seen.insert(device_id.to_string(), now);
                true
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A T0 store in a temp dir (each pool connection must see one file).
    pub async fn t0() -> (tempfile::TempDir, Arc<AccessStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = AccessStore::open_t0(dir.path()).await.unwrap();
        (dir, store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A-28: the owner id is stable across restarts (minted once per data
    /// dir); the file is owner-only.
    #[tokio::test]
    async fn the_owner_is_minted_once_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let a = AccessStore::open_t0(dir.path()).await.unwrap();
        let owner = a.owner.unwrap();
        let host = a.host_device_id.clone().unwrap();
        let store_id = a.store_id.clone();
        a.pool.close().await;
        drop(a);
        let b = AccessStore::open_t0(dir.path()).await.unwrap();
        assert_eq!(b.owner, Some(owner));
        assert_eq!(b.host_device_id.as_deref(), Some(host.as_str()));
        assert_eq!(b.store_id, store_id);
        assert!(b.degraded().is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(T0_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn a_t0_store_is_not_opened_as_t1() {
        let (_dir, store) = test_support::t0().await;
        let err = AccessStore::open_t1(store.pool.clone()).await.unwrap_err();
        assert!(err.to_string().contains("tier"), "{err}");
    }

    #[test]
    fn last_seen_is_gated_per_device() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (_dir, store) = rt.block_on(test_support::t0());
        let now = Instant::now();
        assert!(store.last_seen_due("a", now));
        assert!(!store.last_seen_due("a", now + Duration::from_secs(30)));
        assert!(store.last_seen_due("b", now));
        assert!(store.last_seen_due("a", now + Duration::from_secs(61)));
    }
}
