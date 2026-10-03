//! Master KEK rotation for multi-user servers.
//!
//! Re-wraps each person's secret store DEK envelope under a fresh server KEK.
//! Crash-safe via journal, fd-relative I/O (safe_fs), verification before
//! committing the new KEK, secure wipe of the old KEK, and audit logging.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use zeroize::Zeroizing;

use super::safe_fs::{chown_fd, Dir};
use super::secrets_kek::{existing_stores, KekOwner, SecretsKek, HEADER, KEK_FILENAME};
use super::OperatorRoot;
use crate::secrets::principal_store::{
    rewrap_envelope_bytes, unwrap_envelope_bytes, verify_envelope_and_values, WrapKey, KEY_LEN,
};
use crate::server::principal_child::LOCK_FILE;

pub const JOURNAL_FILENAME: &str = "secrets-rotation-journal.json";
pub const NEXT_KEK_FILENAME: &str = "secrets-kek.next";
pub const OLD_KEK_FILENAME: &str = "secrets-kek.old";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RotationStage {
    Rewrapping,
    Verifying,
    Finalizing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationJournal {
    pub version: u32,
    pub started_at_ms: i64,
    pub stage: RotationStage,
    pub stores_total: usize,
    pub stores_pending: Vec<String>,
    pub stores_completed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationSummary {
    pub stores_rotated: usize,
    pub was_resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrashSimulation {
    #[default]
    None,
    DuringRewrapAfter(usize),
    BeforeVerifying,
    BeforeFinalizing,
    AfterSwapBeforeWipe,
}

/// Refuse to run if the server daemon or any principal child session is active.
pub fn check_concurrency(root: &OperatorRoot) -> anyhow::Result<()> {
    let operator_dir = root.operator_dir();
    let daemon_meta = operator_dir.join("daemon.json");
    if let Ok(text) = fs::read_to_string(&daemon_meta) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(pid) = json.get("pid").and_then(|p| p.as_i64()).filter(|p| *p > 0) {
                let pid = pid as i32;
                // SAFETY: signal 0 only checks if pid exists.
                if unsafe { libc::kill(pid, 0) } == 0 {
                    anyhow::bail!(
                        "the server daemon is running (pid {pid}, from operator/daemon.json); \
                         stop it before rotating the secrets KEK"
                    );
                }
            }
        }
    }

    let principals_dir = root.principals_dir();
    if let Ok(entries) = fs::read_dir(&principals_dir) {
        for entry in entries.flatten() {
            let p_dir = entry.path();
            if !p_dir.is_dir() {
                continue;
            }
            let data_dir = p_dir.join("data");
            let child_daemon = data_dir.join("daemon.json");
            if let Ok(text) = fs::read_to_string(&child_daemon) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(pid) = json.get("pid").and_then(|p| p.as_i64()).filter(|p| *p > 0) {
                        let pid = pid as i32;
                        if unsafe { libc::kill(pid, 0) } == 0 {
                            anyhow::bail!(
                                "a principal child session is active (pid {pid} for principal {}); \
                                 stop it before rotating the secrets KEK",
                                entry.file_name().to_string_lossy()
                            );
                        }
                    }
                }
            }
            let lock_path = data_dir.join(LOCK_FILE);
            if lock_path.exists() {
                if let Ok(file) = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&lock_path)
                {
                    // SAFETY: non-blocking test lock
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                        let err = io::Error::last_os_error();
                        if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
                            anyhow::bail!(
                                "a principal session holds the data directory lock ({}); \
                                 stop it before rotating the secrets KEK",
                                lock_path.display()
                            );
                        }
                    } else {
                        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
                    }
                }
            }
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn write_journal(operator_dir: &Path, journal: &RotationJournal) -> anyhow::Result<()> {
    let path = operator_dir.join(JOURNAL_FILENAME);
    let tmp = operator_dir.join(format!(
        ".{JOURNAL_FILENAME}.tmp-{}-{}",
        std::process::id(),
        hex::encode(&{
            let mut n = [0u8; 8];
            rand::rngs::OsRng.fill_bytes(&mut n);
            n
        })
    ));
    let bytes = serde_json::to_vec_pretty(journal)?;
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .context("open temp journal")?;
        file.write_all(&bytes).context("write temp journal")?;
        file.sync_all().context("sync temp journal")?;
    }
    fs::rename(&tmp, &path).context("rename temp journal into place")?;
    if let Ok(dir) = fs::File::open(operator_dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn load_journal(operator_dir: &Path) -> io::Result<Option<RotationJournal>> {
    let path = operator_dir.join(JOURNAL_FILENAME);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let journal: RotationJournal = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?;
    Ok(Some(journal))
}

fn mint_new_kek() -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    rand::rngs::OsRng.fill_bytes(&mut key[..]);
    key
}

fn write_kek_file(path: &Path, key: &[u8; KEY_LEN]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::other("no parent dir"))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        hex::encode(&{
            let mut n = [0u8; 8];
            rand::rngs::OsRng.fill_bytes(&mut n);
            n
        })
    ));
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        let body = Zeroizing::new(format!("{}{}\n", HEADER, hex::encode(&key[..])));
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn secure_wipe_file(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let len = file.metadata()?.len() as usize;
    if len > 0 {
        let zeroes = vec![0u8; len];
        file.write_all(&zeroes)?;
        file.sync_all()?;
    }
    fs::remove_file(path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

pub fn discover_store_principals(root: &OperatorRoot) -> io::Result<Vec<String>> {
    let stores = existing_stores(root)?;
    let mut principals = Vec::new();
    let principals_dir = root.principals_dir();
    for store_path in stores {
        if let Ok(rel) = store_path.strip_prefix(&principals_dir) {
            if let Some(p_id) = rel.components().next() {
                principals.push(p_id.as_os_str().to_string_lossy().to_string());
            }
        }
    }
    principals.sort();
    principals.dedup();
    Ok(principals)
}

fn open_secrets_dir(principals_dir: &Dir, principal_id: &str) -> io::Result<Dir> {
    let p_dir = principals_dir.open_dir(OsStr::new(principal_id))?;
    let data_dir = p_dir.open_dir(OsStr::new("data"))?;
    data_dir.open_dir(OsStr::new("secrets"))
}

fn rewrap_store(
    secrets_dir: &Dir,
    principal_id: &str,
    current_key: &WrapKey,
    next_key: &WrapKey,
) -> anyhow::Result<()> {
    let (mut env_file, env_stat) = secrets_dir
        .open_file(OsStr::new("envelope.json"))
        .context("open envelope.json")?;
    let mut body = Vec::new();
    env_file.read_to_end(&mut body).context("read envelope.json")?;
    drop(env_file);

    // If already unwrappable with next_key, it was already rewrapped before interruption.
    if unwrap_envelope_bytes(&body, next_key).is_ok() {
        return Ok(());
    }

    let rewrapped = rewrap_envelope_bytes(&body, current_key, next_key)
        .map_err(|e| anyhow::anyhow!("rewrap envelope for principal {principal_id}: {e}"))?;

    let tmp_name = format!(
        ".envelope.json.tmp-{}-{}",
        std::process::id(),
        hex::encode(&{
            let mut n = [0u8; 8];
            rand::rngs::OsRng.fill_bytes(&mut n);
            n
        })
    );
    let tmp_os = OsStr::new(&tmp_name);
    let mut tmp_file = secrets_dir
        .create_file(tmp_os, 0o600)
        .context("create temp envelope file")?;
    chown_fd(tmp_file.as_raw_fd(), env_stat.uid, env_stat.gid)
        .context("chown temp envelope file to principal uid:gid")?;
    tmp_file
        .write_all(&rewrapped)
        .context("write temp envelope file")?;
    tmp_file.sync_all().context("sync temp envelope file")?;
    drop(tmp_file);

    secrets_dir
        .rename(tmp_os, secrets_dir, OsStr::new("envelope.json"))
        .context("rename temp envelope to envelope.json")?;
    // Sync secrets dir
    unsafe { libc::fsync(secrets_dir.raw()) };
    Ok(())
}

fn verify_store(
    secrets_dir: &Dir,
    principal_id: &str,
    next_key: &WrapKey,
) -> anyhow::Result<()> {
    let (mut env_file, _) = secrets_dir
        .open_file(OsStr::new("envelope.json"))
        .context("open envelope.json")?;
    let mut env_body = Vec::new();
    env_file.read_to_end(&mut env_body).context("read envelope.json")?;
    drop(env_file);

    let values_body = match secrets_dir.open_file(OsStr::new("values.json")) {
        Ok((mut val_file, _)) => {
            let mut buf = Vec::new();
            val_file.read_to_end(&mut buf).context("read values.json")?;
            Some(buf)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(anyhow::Error::from(e).context("open values.json")),
    };

    verify_envelope_and_values(&env_body, values_body.as_deref(), next_key)
        .map_err(|e| anyhow::anyhow!("verify store for principal {principal_id}: {e}"))?;
    Ok(())
}

async fn record_rotation_audit(
    pool: &SqlitePool,
    stores_count: usize,
    via: &str,
) -> anyhow::Result<()> {
    let mut conn = match pool.acquire().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("could not acquire accounts.db connection for audit row: {e}");
            return Ok(());
        }
    };

    let has_audit_events: bool = sqlx::query_scalar(
        "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'audit_events'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap_or(false);

    if has_audit_events {
        let store_id: Option<String> =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
                .fetch_optional(&mut *conn)
                .await
                .unwrap_or(None);

        if let Some(store_id) = store_id {
            use crate::access::audit::chain::{self, Head, StoredRow};
            let head = chain::db_head(&mut *conn).await.unwrap_or(None);
            let (seq, prev) = match head {
                Some(h) => (h.seq + 1, h.hash),
                None => (1, chain::genesis(&store_id)),
            };
            let detail = serde_json::json!({
                "stores_rotated": stores_count,
                "outcome": "success",
            })
            .to_string();

            let row = StoredRow {
                seq,
                at_ms: chain::now_ms(),
                kind: "secrets.kek_rotated".to_string(),
                category: "access".to_string(),
                principal_id: None,
                device_id: None,
                via: via.to_string(),
                subject_principal_id: None,
                subject_device_id: None,
                project_key: None,
                target: None,
                remote_addr: None,
                user_agent: None,
                detail,
                prev_hash: prev.to_vec(),
                hash: Vec::new(),
            };
            let hash = row.compute_hash(&prev);

            let res = sqlx::query(
                "INSERT INTO audit_events (seq, at_ms, kind, category, principal_id, device_id, via, \
                 subject_principal_id, subject_device_id, project_key, target, remote_addr, user_agent, \
                 detail, prev_hash, hash) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(row.seq)
            .bind(row.at_ms)
            .bind(&row.kind)
            .bind(&row.category)
            .bind(&row.principal_id)
            .bind(&row.device_id)
            .bind(&row.via)
            .bind(&row.subject_principal_id)
            .bind(&row.subject_device_id)
            .bind(&row.project_key)
            .bind(&row.target)
            .bind(&row.remote_addr)
            .bind(&row.user_agent)
            .bind(&row.detail)
            .bind(&row.prev_hash)
            .bind(&hash)
            .execute(&mut *conn)
            .await;

            if res.is_ok() {
                chain::shared(&store_id).committed(Head { seq, hash });
            }
        }
    } else {
        let has_auth_events: bool = sqlx::query_scalar(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'auth_events'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap_or(false);

        if has_auth_events {
            let detail = serde_json::json!({
                "stores_rotated": stores_count,
                "outcome": "success",
                "via": via,
            })
            .to_string();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let _ = sqlx::query(
                "INSERT INTO auth_events (at, kind, detail) VALUES (?, 'secrets.kek_rotated', ?)",
            )
            .bind(now)
            .bind(detail)
            .execute(&mut *conn)
            .await;
        }
    }

    tracing::info!(
        stores_rotated = stores_count,
        via = via,
        outcome = "success",
        "secrets KEK rotation completed and recorded in audit log"
    );
    Ok(())
}

/// Execute a KEK rotation or resume an interrupted one.
pub async fn execute_or_resume(
    root: &OperatorRoot,
    owner: KekOwner,
    via: &str,
    crash_sim: CrashSimulation,
) -> anyhow::Result<RotationSummary> {
    check_concurrency(root)?;

    let operator_dir = root.operator_dir();
    let current_kek_path = SecretsKek::path(&operator_dir);
    let next_kek_path = operator_dir.join(NEXT_KEK_FILENAME);
    let old_kek_path = operator_dir.join(OLD_KEK_FILENAME);
    let journal_path = operator_dir.join(JOURNAL_FILENAME);

    let (mut journal, was_resumed) = match load_journal(&operator_dir)? {
        Some(j) => {
            tracing::info!(
                "secrets rotation: resuming interrupted rotation started at {}",
                j.started_at_ms
            );
            (j, true)
        }
        None => {
            // New rotation. Check that current KEK exists and is valid.
            let current = SecretsKek::load(&current_kek_path, owner).map_err(|e| {
                anyhow::anyhow!(
                    "cannot rotate secrets KEK: current {} is missing or invalid: {e}",
                    current_kek_path.display()
                )
            })?;
            drop(current);

            let principals = discover_store_principals(root)?;
            let new_key = mint_new_kek();
            write_kek_file(&next_kek_path, &new_key)?;

            let j = RotationJournal {
                version: 1,
                started_at_ms: now_ms(),
                stage: RotationStage::Rewrapping,
                stores_total: principals.len(),
                stores_pending: principals,
                stores_completed: Vec::new(),
            };
            write_journal(&operator_dir, &j)?;
            (j, false)
        }
    };

    let principals_dir_canon = fs::canonicalize(root.principals_dir())?;
    let principals_dir = Dir::open_no_symlinks(&principals_dir_canon)?;

    // Handle each stage
    if journal.stage == RotationStage::Rewrapping {
        let current_kek = SecretsKek::load(&current_kek_path, owner)?;
        let next_kek = SecretsKek::load(&next_kek_path, owner)?;

        while let Some(principal_id) = journal.stores_pending.first().cloned() {
            let cur_key = current_kek
                .wrap_key_for_str(&principal_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let nxt_key = next_kek
                .wrap_key_for_str(&principal_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            let secrets_dir = open_secrets_dir(&principals_dir, &principal_id)?;
            rewrap_store(&secrets_dir, &principal_id, &cur_key, &nxt_key)?;

            journal.stores_pending.remove(0);
            journal.stores_completed.push(principal_id);
            write_journal(&operator_dir, &journal)?;

            if let CrashSimulation::DuringRewrapAfter(n) = crash_sim {
                if journal.stores_completed.len() == n {
                    anyhow::bail!("simulated crash during rewrap after {n} stores");
                }
            }
        }

        journal.stage = RotationStage::Verifying;
        write_journal(&operator_dir, &journal)?;
    }

    if let CrashSimulation::BeforeVerifying = crash_sim {
        anyhow::bail!("simulated crash before verifying");
    }

    if journal.stage == RotationStage::Verifying {
        let next_kek = SecretsKek::load(&next_kek_path, owner)?;

        for principal_id in &journal.stores_completed {
            let nxt_key = next_kek
                .wrap_key_for_str(principal_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let secrets_dir = open_secrets_dir(&principals_dir, principal_id)?;
            verify_store(&secrets_dir, principal_id, &nxt_key)?;
        }

        journal.stage = RotationStage::Finalizing;
        write_journal(&operator_dir, &journal)?;
    }

    if let CrashSimulation::BeforeFinalizing = crash_sim {
        anyhow::bail!("simulated crash before finalizing");
    }

    if journal.stage == RotationStage::Finalizing {
        // Swap KEKs:
        // If secrets-kek.next still exists, secrets-kek has not been replaced yet.
        if next_kek_path.exists() {
            fs::rename(&current_kek_path, &old_kek_path).context("rename current KEK to .old")?;
            fs::rename(&next_kek_path, &current_kek_path).context("rename .next to current KEK")?;
            if let Ok(dir) = fs::File::open(&operator_dir) {
                let _ = dir.sync_all();
            }
        }

        if let CrashSimulation::AfterSwapBeforeWipe = crash_sim {
            anyhow::bail!("simulated crash after swap before wipe");
        }

        // Wipe old KEK
        if old_kek_path.exists() {
            secure_wipe_file(&old_kek_path).context("securely wipe old KEK")?;
        }

        // Audit log
        if let Ok(pool) = super::open_accounts(root, super::Opener::Cli).await {
            let _ = record_rotation_audit(&pool, journal.stores_total, via).await;
        }

        // Clean up journal
        let _ = fs::remove_file(&journal_path);
        if let Ok(dir) = fs::File::open(&operator_dir) {
            let _ = dir.sync_all();
        }
    }

    Ok(RotationSummary {
        stores_rotated: journal.stores_total,
        was_resumed,
    })
}

/// Called during server boot: resumes an interrupted rotation if a journal exists.
pub async fn resume_if_interrupted(
    root: &OperatorRoot,
    pool: &SqlitePool,
) -> anyhow::Result<Option<RotationSummary>> {
    let operator_dir = root.operator_dir();
    let journal_path = operator_dir.join(JOURNAL_FILENAME);
    if !journal_path.exists() {
        return Ok(None);
    }
    tracing::warn!("secrets rotation: found pending journal at boot; resuming rotation");
    let summary = execute_or_resume(root, KekOwner::Root, "system", CrashSimulation::None).await?;
    let _ = record_rotation_audit(pool, summary.stores_rotated, "system").await;
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::secrets::principal_store::{ENVELOPE_FILENAME, VALUES_FILENAME};
    use crate::server::operator::{open_accounts, Opener, Ownership};

    struct TestFixture {
        _tmp: tempfile::TempDir,
        root: OperatorRoot,
        pool: SqlitePool,
    }

    impl TestFixture {
        async fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = OperatorRoot::new(tmp.path().canonicalize().unwrap().join("root")).unwrap();
            root.prepare(Ownership::SkipForTests).unwrap();
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            Self {
                _tmp: tmp,
                root,
                pool,
            }
        }

        fn create_store(&self, principal_id: &str, secrets: &[(&str, &str)]) {
            let p_dir = self.root.principals_dir().join(principal_id).join("data/secrets");
            fs::create_dir_all(&p_dir).unwrap();

            let kek = SecretsKek::load(&SecretsKek::path(&self.root.operator_dir()), KekOwner::Any).unwrap();
            let key = kek.wrap_key_for_str(principal_id).unwrap();

            let store = crate::secrets::PrincipalStore::open(&self.root.principals_dir().join(principal_id).join("data"), &key).unwrap();
            use crate::secrets::store::SecretsStore;
            for (k, v) in secrets {
                store.set(k, v).unwrap();
            }
        }

        fn get_secret(&self, principal_id: &str, key_name: &str) -> Option<String> {
            let kek = SecretsKek::load(&SecretsKek::path(&self.root.operator_dir()), KekOwner::Any).unwrap();
            let key = kek.wrap_key_for_str(principal_id).unwrap();
            let store = crate::secrets::PrincipalStore::open(&self.root.principals_dir().join(principal_id).join("data"), &key).unwrap();
            use crate::secrets::store::SecretsStore;
            store.get(key_name).unwrap()
        }
    }

    #[tokio::test]
    async fn normal_rotation_and_decrypt() {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn).await.unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        fx.create_store("p-bob", &[("TOKEN", "bob-token-456")]);

        let summary = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary.stores_rotated, 2);
        assert_eq!(summary.was_resumed, false);

        assert_eq!(fx.get_secret("p-ada", "API_KEY").as_deref(), Some("ada-secret-123"));
        assert_eq!(fx.get_secret("p-bob", "TOKEN").as_deref(), Some("bob-token-456"));

        // Journal and old KEK should be cleaned up
        assert!(!fx.root.operator_dir().join(JOURNAL_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(OLD_KEK_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
    }

    #[tokio::test]
    async fn crash_mid_rotation_and_resume() {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn).await.unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        fx.create_store("p-bob", &[("TOKEN", "bob-token-456")]);

        // Simulate crash after 1 store rewrapped
        let err = execute_or_resume(
            &fx.root,
            KekOwner::Any,
            "cli",
            CrashSimulation::DuringRewrapAfter(1),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("simulated crash"));

        // Journal exists and records partial progress
        let journal = load_journal(&fx.root.operator_dir()).unwrap().unwrap();
        assert_eq!(journal.stores_completed.len(), 1);
        assert_eq!(journal.stores_pending.len(), 1);

        // Resume rotation
        let summary = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary.stores_rotated, 2);
        assert_eq!(summary.was_resumed, true);

        // Both decrypt cleanly
        assert_eq!(fx.get_secret("p-ada", "API_KEY").as_deref(), Some("ada-secret-123"));
        assert_eq!(fx.get_secret("p-bob", "TOKEN").as_deref(), Some("bob-token-456"));
    }

    #[tokio::test]
    async fn wrong_or_missing_kek_fails_loudly_without_corruption() {
        let fx = TestFixture::new().await;
        // Missing KEK
        let err = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing or invalid"));

        // Create valid KEK and stores
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn).await.unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("SECRET", "value-1")]);

        // Corrupt current KEK
        let kek_path = SecretsKek::path(&fx.root.operator_dir());
        fs::write(&kek_path, "corrupted-kek-content").unwrap();

        let err = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing or invalid"));

        // No journal or stray files created
        assert!(!fx.root.operator_dir().join(JOURNAL_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
    }

    #[tokio::test]
    async fn rotate_twice_in_a_row() {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn).await.unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("KEY", "secret-value")]);

        // First rotation
        let summary1 = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary1.stores_rotated, 1);
        assert_eq!(fx.get_secret("p-ada", "KEY").as_deref(), Some("secret-value"));

        // Second rotation immediately after
        let summary2 = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary2.stores_rotated, 1);
        assert_eq!(fx.get_secret("p-ada", "KEY").as_deref(), Some("secret-value"));
    }
}
