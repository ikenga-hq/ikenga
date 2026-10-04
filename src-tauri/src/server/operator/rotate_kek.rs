//! Master KEK rotation for multi-user servers.
//!
//! Re-wraps each person's secret store DEK envelope under a fresh server KEK.
//! Crash-safe via journal, fd-relative I/O (safe_fs), verification before
//! committing the new KEK, secure wipe of the old KEK, and audit logging.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::Context;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use zeroize::Zeroizing;

use super::safe_fs::{chown_fd, Dir};
use super::secrets_kek::{existing_stores, KekOwner, SecretsKek, HEADER};
use super::OperatorRoot;
use crate::access::audit::AuditVia;
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
    DuringSwap,
    AfterSwapBeforeWipe,
}

/// Refuse to run if the server daemon or any principal child session is active.
pub fn check_concurrency(root: &OperatorRoot) -> anyhow::Result<()> {
    check_concurrency_inner(root, false)
}

/// `at_boot`: called by the broker itself before it serves. Every
/// `daemon.json` on disk is then left over from an earlier run, and its pid
/// may since belong to an unrelated process (or, in a container, to this
/// very broker), so only the data-directory locks are checked.
fn check_concurrency_inner(root: &OperatorRoot, at_boot: bool) -> anyhow::Result<()> {
    let operator_dir = root.operator_dir();
    let daemon_meta = operator_dir.join("daemon.json");
    if let Some(text) = (!at_boot)
        .then(|| fs::read_to_string(&daemon_meta).ok())
        .flatten()
    {
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
            if let Some(text) = (!at_boot)
                .then(|| fs::read_to_string(&child_daemon).ok())
                .flatten()
            {
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
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
                    {
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
    let journal: RotationJournal = serde_json::from_slice(&bytes).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })?;
    Ok(Some(journal))
}

fn mint_new_kek() -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    rand::rngs::OsRng.fill_bytes(&mut key[..]);
    key
}

fn write_kek_file(path: &Path, key: &[u8; KEY_LEN]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent dir"))?;
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
    env_file
        .read_to_end(&mut body)
        .context("read envelope.json")?;
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
    let written = (|| -> anyhow::Result<()> {
        chown_fd(tmp_file.as_raw_fd(), env_stat.uid, env_stat.gid)
            .context("chown temp envelope file to principal uid:gid")?;
        tmp_file
            .write_all(&rewrapped)
            .context("write temp envelope file")?;
        tmp_file.sync_all().context("sync temp envelope file")?;
        Ok(())
    })();
    drop(tmp_file);
    let renamed = written.and_then(|()| {
        secrets_dir
            .rename(tmp_os, secrets_dir, OsStr::new("envelope.json"))
            .context("rename temp envelope to envelope.json")
    });
    if let Err(e) = renamed {
        // Don't leave a stray temp envelope beside the store.
        let _ = secrets_dir.unlink(tmp_os);
        return Err(e);
    }
    // Sync secrets dir
    unsafe { libc::fsync(secrets_dir.raw()) };
    Ok(())
}

fn verify_store(secrets_dir: &Dir, principal_id: &str, next_key: &WrapKey) -> anyhow::Result<()> {
    let (mut env_file, _) = secrets_dir
        .open_file(OsStr::new("envelope.json"))
        .context("open envelope.json")?;
    let mut env_body = Vec::new();
    env_file
        .read_to_end(&mut env_body)
        .context("read envelope.json")?;
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

/// Append a `secrets.kek_rotated` row to the audit chain in `accounts.db`.
/// Best effort: the rotation has already committed, so a failure here is
/// logged, never returned.
async fn record_rotation_audit(pool: &SqlitePool, stores_count: usize, via: AuditVia) {
    use crate::access::audit::{chain, Event};
    use sqlx::Connection;

    let result: anyhow::Result<bool> = async {
        let mut conn = pool.acquire().await?;
        let has_audit_events: bool = sqlx::query_scalar(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'audit_events'",
        )
        .fetch_one(&mut *conn)
        .await?;
        if !has_audit_events {
            return Ok(false);
        }
        let store_id: Option<String> =
            sqlx::query_scalar("SELECT v FROM store_meta WHERE k = 'store_id'")
                .fetch_optional(&mut *conn)
                .await?;
        let Some(store_id) = store_id else {
            return Ok(false);
        };
        let ev = Event::new("secrets.kek_rotated", via).detail(serde_json::json!({
            "stores_rotated": stores_count,
            "outcome": "success",
        }));
        let chain = chain::shared(&store_id);
        let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
        let head = chain
            .append(&mut tx, &ev)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.commit().await?;
        chain.committed(head);
        Ok(true)
    }
    .await;

    match result {
        Ok(true) => tracing::info!(
            stores_rotated = stores_count,
            via = via.as_str(),
            "secrets KEK rotation completed and recorded in the audit log"
        ),
        Ok(false) => tracing::warn!(
            stores_rotated = stores_count,
            via = via.as_str(),
            "secrets KEK rotation completed; accounts.db has no audit chain yet, so it is \
             recorded in this log only"
        ),
        Err(e) => tracing::warn!(
            stores_rotated = stores_count,
            via = via.as_str(),
            "secrets KEK rotation completed, but writing its audit row failed: {e:#}"
        ),
    }
}

/// Re-wrap one principal's store from `from` to `to`. Idempotent: a store
/// already under `to` is left alone.
fn rewrap_one(
    principals_dir: &Dir,
    principal_id: &str,
    from: &SecretsKek,
    to: &SecretsKek,
) -> anyhow::Result<()> {
    let from_key = from
        .wrap_key_for_str(principal_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let to_key = to
        .wrap_key_for_str(principal_id)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let secrets_dir = open_secrets_dir(principals_dir, principal_id)
        .with_context(|| format!("open the secret store of principal {principal_id}"))?;
    rewrap_store(&secrets_dir, principal_id, &from_key, &to_key)
}

/// A re-wrap failed: put every store already moved back under the current
/// KEK, then drop the journal and the unused next KEK, so the server is
/// exactly as it was. If a store can't be moved back, the journal stays and
/// a rerun resumes the rotation instead.
fn roll_back(
    operator_dir: &Path,
    principals_dir: &Dir,
    journal: &RotationJournal,
    current: &SecretsKek,
    next: &SecretsKek,
    next_kek_path: &Path,
    cause: anyhow::Error,
) -> anyhow::Error {
    for principal_id in journal.stores_completed.iter().rev() {
        if let Err(e) = rewrap_one(principals_dir, principal_id, next, current) {
            return cause.context(format!(
                "secrets KEK rotation failed, and moving principal {principal_id} back to the \
                 current KEK failed too ({e:#}). The rotation journal is kept: fix the cause and \
                 rerun `ikenga-server secrets rotate-kek` to finish the rotation"
            ));
        }
    }
    // Journal first: a journal without its next KEK could not resume.
    let journal_path = operator_dir.join(JOURNAL_FILENAME);
    if let Err(e) = fs::remove_file(&journal_path) {
        if e.kind() != io::ErrorKind::NotFound {
            return cause.context(format!(
                "secrets KEK rotation failed and every store was moved back, but removing {} \
                 failed ({e}); rerun `ikenga-server secrets rotate-kek` to finish",
                journal_path.display()
            ));
        }
    }
    if let Ok(dir) = fs::File::open(operator_dir) {
        let _ = dir.sync_all();
    }
    if let Err(e) = secure_wipe_file(next_kek_path) {
        tracing::warn!(
            "secrets KEK rotation rolled back, but wiping the unused {} failed: {e}",
            next_kek_path.display()
        );
    }
    cause.context(format!(
        "secrets KEK rotation failed and was rolled back ({} store(s) moved back); the current \
         KEK and every store are unchanged",
        journal.stores_completed.len()
    ))
}

/// Execute a KEK rotation or resume an interrupted one.
pub async fn execute_or_resume(
    root: &OperatorRoot,
    owner: KekOwner,
    via: &str,
    crash_sim: CrashSimulation,
) -> anyhow::Result<RotationSummary> {
    let via = if via == "system" {
        AuditVia::System
    } else {
        AuditVia::Cli
    };
    run(root, owner, via, crash_sim, None, false).await
}

async fn run(
    root: &OperatorRoot,
    owner: KekOwner,
    via: AuditVia,
    crash_sim: CrashSimulation,
    pool: Option<&SqlitePool>,
    at_boot: bool,
) -> anyhow::Result<RotationSummary> {
    if owner == KekOwner::Root && unsafe { libc::geteuid() != 0 } {
        anyhow::bail!("`ikenga-server secrets rotate-kek` must be run as root");
    }

    check_concurrency_inner(root, at_boot)?;

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

    if journal.stage == RotationStage::Rewrapping {
        let current_kek = SecretsKek::load(&current_kek_path, owner)?;
        let next_kek = SecretsKek::load(&next_kek_path, owner)?;

        while let Some(principal_id) = journal.stores_pending.first().cloned() {
            if let Err(e) = rewrap_one(&principals_dir, &principal_id, &current_kek, &next_kek) {
                return Err(roll_back(
                    &operator_dir,
                    &principals_dir,
                    &journal,
                    &current_kek,
                    &next_kek,
                    &next_kek_path,
                    e,
                ));
            }

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
        // Swap the KEKs. While `secrets-kek.next` exists the swap hasn't
        // finished. Each step is checked on its own so a crash between the
        // two renames (current already moved to `.old`) still resumes.
        if next_kek_path.exists() {
            if current_kek_path.exists() {
                fs::rename(&current_kek_path, &old_kek_path)
                    .context("rename current KEK to .old")?;
            }
            if let CrashSimulation::DuringSwap = crash_sim {
                anyhow::bail!("simulated crash during swap");
            }
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

        // Audit log: through the caller's pool (the broker at boot), else
        // the CLI's own connection to an existing accounts.db.
        match pool {
            Some(pool) => record_rotation_audit(pool, journal.stores_total, via).await,
            None => match super::open_accounts(root, super::Opener::CliExisting).await {
                Ok(pool) => {
                    record_rotation_audit(&pool, journal.stores_total, via).await;
                    pool.close().await;
                }
                Err(e) => tracing::warn!(
                    "secrets KEK rotation completed, but accounts.db could not be opened for \
                     its audit row: {e:#}"
                ),
            },
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
    resume_if_interrupted_as(root, pool, KekOwner::Root).await
}

async fn resume_if_interrupted_as(
    root: &OperatorRoot,
    pool: &SqlitePool,
    owner: KekOwner,
) -> anyhow::Result<Option<RotationSummary>> {
    let journal_path = root.operator_dir().join(JOURNAL_FILENAME);
    if !journal_path.exists() {
        return Ok(None);
    }
    tracing::warn!("secrets rotation: found pending journal at boot; resuming rotation");
    // `run` records the audit row itself, through `pool`.
    let summary = run(
        root,
        owner,
        AuditVia::System,
        CrashSimulation::None,
        Some(pool),
        true,
    )
    .await?;
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::principal_store::{ENVELOPE_FILENAME, VALUES_FILENAME};
    use crate::server::operator::{open_accounts, Opener, Ownership};
    use std::collections::BTreeMap;

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
            let p_dir = self
                .root
                .principals_dir()
                .join(principal_id)
                .join("data/secrets");
            fs::create_dir_all(&p_dir).unwrap();

            let kek = SecretsKek::load(&SecretsKek::path(&self.root.operator_dir()), KekOwner::Any)
                .unwrap();
            let key = kek.wrap_key_for_str(principal_id).unwrap();

            let store = crate::secrets::PrincipalStore::open(
                &self.root.principals_dir().join(principal_id).join("data"),
                &key,
            )
            .unwrap();
            use crate::secrets::store::SecretsStore;
            for (k, v) in secrets {
                store.set(k, v).unwrap();
            }
        }

        fn get_secret(&self, principal_id: &str, key_name: &str) -> Option<String> {
            let kek = SecretsKek::load(&SecretsKek::path(&self.root.operator_dir()), KekOwner::Any)
                .unwrap();
            let key = kek.wrap_key_for_str(principal_id).unwrap();
            let store = crate::secrets::PrincipalStore::open(
                &self.root.principals_dir().join(principal_id).join("data"),
                &key,
            )
            .unwrap();
            use crate::secrets::store::SecretsStore;
            store.get(key_name).unwrap()
        }
    }

    #[tokio::test]
    async fn normal_rotation_and_decrypt() {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn)
            .await
            .unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        fx.create_store("p-bob", &[("TOKEN", "bob-token-456")]);

        let summary = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary.stores_rotated, 2);
        assert_eq!(summary.was_resumed, false);

        assert_eq!(
            fx.get_secret("p-ada", "API_KEY").as_deref(),
            Some("ada-secret-123")
        );
        assert_eq!(
            fx.get_secret("p-bob", "TOKEN").as_deref(),
            Some("bob-token-456")
        );

        // Journal and old KEK should be cleaned up
        assert!(!fx.root.operator_dir().join(JOURNAL_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(OLD_KEK_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
    }

    #[tokio::test]
    async fn crash_mid_rotation_and_resume() {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn)
            .await
            .unwrap();
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
        assert_eq!(
            fx.get_secret("p-ada", "API_KEY").as_deref(),
            Some("ada-secret-123")
        );
        assert_eq!(
            fx.get_secret("p-bob", "TOKEN").as_deref(),
            Some("bob-token-456")
        );
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
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn)
            .await
            .unwrap();
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
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn)
            .await
            .unwrap();
        drop(conn);

        fx.create_store("p-ada", &[("KEY", "secret-value")]);

        // First rotation
        let summary1 = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary1.stores_rotated, 1);
        assert_eq!(
            fx.get_secret("p-ada", "KEY").as_deref(),
            Some("secret-value")
        );

        // Second rotation immediately after
        let summary2 = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert_eq!(summary2.stores_rotated, 1);
        assert_eq!(
            fx.get_secret("p-ada", "KEY").as_deref(),
            Some("secret-value")
        );
    }

    fn kek_bytes(fx: &TestFixture) -> Vec<u8> {
        fs::read(SecretsKek::path(&fx.root.operator_dir())).unwrap()
    }

    async fn fixture_with_kek() -> TestFixture {
        let fx = TestFixture::new().await;
        let mut conn = fx.pool.acquire().await.unwrap();
        let _ = SecretsKek::load_or_create(&fx.root, KekOwner::Any, &mut conn)
            .await
            .unwrap();
        drop(conn);
        fx
    }

    #[tokio::test]
    async fn each_rotation_replaces_the_kek() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("KEY", "secret-value")]);
        let before = kek_bytes(&fx);
        execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        let after_one = kek_bytes(&fx);
        execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        let after_two = kek_bytes(&fx);
        assert_ne!(before, after_one);
        assert_ne!(after_one, after_two);
        assert_eq!(
            fx.get_secret("p-ada", "KEY").as_deref(),
            Some("secret-value")
        );
    }

    #[tokio::test]
    async fn wrong_kek_rolls_back_and_leaves_stores_untouched() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("SECRET", "value-1")]);
        let kek_path = SecretsKek::path(&fx.root.operator_dir());
        let original = kek_bytes(&fx);
        let envelope = fx
            .root
            .principals_dir()
            .join("p-ada/data/secrets/envelope.json");
        let envelope_before = fs::read(&envelope).unwrap();

        // A well-formed KEK that is not the one the stores were wrapped under.
        write_kek_file(&kek_path, &[7u8; KEY_LEN]).unwrap();
        let err = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("rolled back"), "{err:#}");

        assert!(!fx.root.operator_dir().join(JOURNAL_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
        assert_eq!(fs::read(&envelope).unwrap(), envelope_before);

        // Put the right KEK back: the store still opens.
        fs::write(&kek_path, original).unwrap();
        assert_eq!(fx.get_secret("p-ada", "SECRET").as_deref(), Some("value-1"));
    }

    #[tokio::test]
    async fn failure_mid_rotation_moves_finished_stores_back() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        fx.create_store("p-bob", &[("TOKEN", "bob-token-456")]);
        let before = kek_bytes(&fx);
        // p-ada is re-wrapped first; p-bob's envelope then fails to parse.
        let bob_envelope = fx
            .root
            .principals_dir()
            .join("p-bob/data/secrets/envelope.json");
        fs::write(&bob_envelope, b"{}").unwrap();

        let err = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("rolled back (1 store"),
            "{err:#}"
        );

        assert_eq!(kek_bytes(&fx), before);
        assert!(!fx.root.operator_dir().join(JOURNAL_FILENAME).exists());
        assert!(!fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
        assert_eq!(
            fx.get_secret("p-ada", "API_KEY").as_deref(),
            Some("ada-secret-123")
        );
    }

    #[tokio::test]
    async fn crash_between_the_kek_renames_resumes() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);

        let err = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::DuringSwap)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("simulated crash"));
        let op = fx.root.operator_dir();
        assert!(!SecretsKek::path(&op).exists());
        assert!(op.join(NEXT_KEK_FILENAME).exists());
        assert!(op.join(OLD_KEK_FILENAME).exists());

        let summary = execute_or_resume(&fx.root, KekOwner::Any, "cli", CrashSimulation::None)
            .await
            .unwrap();
        assert!(summary.was_resumed);
        assert_eq!(
            fx.get_secret("p-ada", "API_KEY").as_deref(),
            Some("ada-secret-123")
        );
        assert!(!op.join(JOURNAL_FILENAME).exists());
        assert!(!op.join(NEXT_KEK_FILENAME).exists());
        assert!(!op.join(OLD_KEK_FILENAME).exists());
    }

    #[tokio::test]
    async fn the_old_kek_stays_until_the_rewrap_verifies() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        let before = kek_bytes(&fx);

        execute_or_resume(
            &fx.root,
            KekOwner::Any,
            "cli",
            CrashSimulation::BeforeFinalizing,
        )
        .await
        .unwrap_err();
        // Verified but not swapped: the current KEK is still the old one.
        assert_eq!(kek_bytes(&fx), before);
        assert!(fx.root.operator_dir().join(NEXT_KEK_FILENAME).exists());
    }

    #[tokio::test]
    async fn boot_resume_records_one_audit_row() {
        let fx = fixture_with_kek().await;
        fx.create_store("p-ada", &[("API_KEY", "ada-secret-123")]);
        execute_or_resume(
            &fx.root,
            KekOwner::Any,
            "cli",
            CrashSimulation::BeforeFinalizing,
        )
        .await
        .unwrap_err();

        let summary = resume_if_interrupted_as(&fx.root, &fx.pool, KekOwner::Any)
            .await
            .unwrap()
            .expect("a pending journal resumes");
        assert!(summary.was_resumed);
        assert_eq!(
            fx.get_secret("p-ada", "API_KEY").as_deref(),
            Some("ada-secret-123")
        );
        assert!(resume_if_interrupted_as(&fx.root, &fx.pool, KekOwner::Any)
            .await
            .unwrap()
            .is_none());

        let mut conn = fx.pool.acquire().await.unwrap();
        let has_chain: bool = sqlx::query_scalar(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'audit_events'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        if has_chain {
            let rows: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit_events WHERE kind = 'secrets.kek_rotated'",
            )
            .fetch_one(&mut *conn)
            .await
            .unwrap();
            assert!(rows <= 1, "boot resume wrote {rows} audit rows");
        }
    }
}
