//! The master KEK for every principal's secret store (remote-access WP-21,
//! founder decision DEC-R18-1: a server-held KEK envelope).
//!
//! `operator/secrets-kek` — 32 random bytes, hex, `root:root 0600`, inside
//! the `0700` root-only `operator/` (G-PRINCIPAL §4). Created the first time
//! a T1 broker launches a principal child, and from then on only read. The
//! broker derives one wrapping key per principal from it
//! ([`WrapKey::derive`], HKDF-SHA256 with the principal id as info) and hands
//! a child **only its own** derived key (`server::broker::children`). The
//! KEK itself never leaves this process: it is not in any child's
//! environment, argv or files, and no principal uid can open the file.
//!
//! Losing the file loses every principal's stored secrets (the operator
//! defaults are unaffected). Back it up with `operator/`. Rotation is a
//! rewrap of each principal's envelope
//! ([`crate::secrets::PrincipalStore::rewrap`]); the operator command that
//! drives it is deferred.
//!
//! # A lost KEK is never silently re-minted
//!
//! A missing file is only "first use" when nothing was ever sealed under a
//! KEK. [`SecretsKek::load_or_create`] refuses to mint when either
//!
//! * `operator_meta` (in `accounts.db`) holds [`KEK_MARKER`], written the
//!   moment a KEK is created, or
//! * any `principals/<id>/data/secrets/envelope.json` exists
//!   ([`existing_stores`]),
//!
//! because a fresh KEK would leave every existing store undecryptable with
//! no error until each principal looked. The launch fails instead, with
//! [`KekLost`]'s "restore operator/secrets-kek from backup" message.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use rand::RngCore;
use sqlx::SqliteConnection;
use zeroize::Zeroizing;

use super::safe_fs::Dir;
use super::OperatorRoot;
use crate::executor::PrincipalId;
use crate::secrets::principal_store::{WrapKey, KEY_LEN};

/// Under `operator/`.
pub const KEK_FILENAME: &str = "secrets-kek";
const HEADER: &str = "ikenga-secrets-kek:v1:";

/// The `operator_meta` key recording that a KEK was created (value: the
/// creation time, unix seconds). Never deleted by Ikenga: an operator who
/// has truly given up on every existing store removes it by hand, together
/// with those stores (see [`KekLost`]).
pub const KEK_MARKER: &str = "secrets_kek_created";

/// `operator/secrets-kek` is missing but a KEK existed: refusing to mint a
/// replacement that would orphan every principal store sealed under it.
#[derive(Debug)]
pub struct KekLost {
    pub path: PathBuf,
    /// What says a KEK existed: the marker, and/or the stores found.
    pub evidence: Vec<String>,
}

impl std::fmt::Display for KekLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is missing, but a secrets KEK was created before ({}). Refusing to mint a new \
             one: every principal's secret store is sealed under the old KEK and a new one \
             would make them all undecryptable. Restore operator/secrets-kek from backup \
             (root:root 0600). Only if every principal store is truly unrecoverable: move \
             each principals/<id>/data/secrets/ aside and delete the operator_meta row \
             '{KEK_MARKER}' in operator/accounts.db; the next launch then mints a new KEK.",
            self.path.display(),
            self.evidence.join("; ")
        )
    }
}

impl std::error::Error for KekLost {}

/// Every `principals/<id>/data/secrets/envelope.json` under `root`, as root.
///
/// Walked fd-relative ([`super::safe_fs`]) without following a symlink at
/// any level: the trees below `principals/<id>/` are the principals'. A
/// symlink or non-directory where a directory belongs is not a store and is
/// skipped; any other error (`EACCES`, `EIO`) is returned, so the caller
/// fails closed rather than minting over a store it could not see.
pub fn existing_stores(root: &OperatorRoot) -> io::Result<Vec<PathBuf>> {
    let principals = root.principals_dir();
    let canonical = match fs::canonicalize(&principals) {
        Ok(p) => p,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let dir = Dir::open_no_symlinks(&canonical)?;
    let not_a_store = |e: &io::Error| {
        e.kind() == io::ErrorKind::NotFound
            || matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR))
    };
    let mut found = Vec::new();
    for id in dir.entries()? {
        let mut cur = match dir.open_dir(&id) {
            Ok(d) => d,
            Err(e) if not_a_store(&e) => continue,
            Err(e) => return Err(io::Error::new(e.kind(), format!("principals/{id:?}: {e}"))),
        };
        let mut reached = true;
        for name in ["data", "secrets"] {
            match cur.open_dir(OsStr::new(name)) {
                Ok(d) => cur = d,
                Err(e) if not_a_store(&e) => {
                    reached = false;
                    break;
                }
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("principals/{id:?}/…/{name}: {e}"),
                    ))
                }
            }
        }
        if reached && cur.try_stat_at(OsStr::new("envelope.json"))?.is_some() {
            found.push(principals.join(&id).join("data/secrets/envelope.json"));
        }
    }
    Ok(found)
}

async fn marker(meta: &mut SqliteConnection) -> io::Result<Option<String>> {
    sqlx::query_scalar("SELECT value FROM operator_meta WHERE key = ?")
        .bind(KEK_MARKER)
        .fetch_optional(meta)
        .await
        .map_err(|e| io::Error::other(format!("operator_meta: {e}")))
}

/// The master KEK, held by the broker. Never `Debug`-printed, never cloned
/// out of this type.
pub struct SecretsKek {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for SecretsKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretsKek(<redacted>)")
    }
}

/// Whether the file must be root-owned. Production always enforces; unit
/// tests that run unprivileged skip only the owner check (the mode is still
/// checked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KekOwner {
    Root,
    #[cfg(test)]
    Any,
}

impl SecretsKek {
    pub fn path(operator_dir: &Path) -> PathBuf {
        operator_dir.join(KEK_FILENAME)
    }

    /// Read `operator/secrets-kek`, creating it on first use. Refuses (never
    /// regenerates) a file that is a symlink, not regular, readable by group
    /// or other, not root-owned, or malformed: a silently re-minted KEK
    /// would orphan every principal's store.
    ///
    /// A **missing** file is minted only when no KEK ever existed: neither
    /// [`KEK_MARKER`] in `operator_meta` (`meta`, a connection to
    /// `operator/accounts.db`) nor any principal store ([`existing_stores`]).
    /// Otherwise this logs loudly and fails with [`KekLost`] (kind
    /// `NotFound`), and the caller fails that principal's launch. A new KEK
    /// is recorded under [`KEK_MARKER`].
    pub async fn load_or_create(
        root: &OperatorRoot,
        owner: KekOwner,
        meta: &mut SqliteConnection,
    ) -> io::Result<Self> {
        let operator_dir = root.operator_dir();
        let path = Self::path(&operator_dir);
        match Self::load(&path, owner) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            other => return other,
        }
        let mut evidence = Vec::new();
        if let Some(created) = marker(meta).await? {
            evidence.push(format!(
                "operator_meta '{KEK_MARKER}' = {created:?} in operator/accounts.db"
            ));
        }
        let stores = existing_stores(root).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{} is missing and the principal stores could not be checked, so no new \
                     KEK was minted: {e}",
                    path.display()
                ),
            )
        })?;
        if !stores.is_empty() {
            let shown: Vec<String> = stores
                .iter()
                .take(3)
                .map(|p| p.display().to_string())
                .collect();
            let more = stores.len().saturating_sub(shown.len());
            evidence.push(format!(
                "{} principal store(s): {}{}",
                stores.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ));
        }
        if !evidence.is_empty() {
            // A concurrent first launch may have just linked it in.
            match Self::load(&path, owner) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                other => return other,
            }
            let lost = KekLost { path, evidence };
            tracing::error!("secrets: {lost}");
            return Err(io::Error::new(io::ErrorKind::NotFound, lost));
        }
        let kek = Self::create(&operator_dir, owner)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let recorded = sqlx::query(
            "INSERT INTO operator_meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO NOTHING",
        )
        .bind(KEK_MARKER)
        .bind(now.to_string())
        .execute(meta)
        .await;
        if let Err(e) = recorded {
            // The KEK is in place and every later launch reads it; the
            // envelope scan still guards a later loss once a store exists.
            tracing::error!(
                "secrets: created {} but could not record '{KEK_MARKER}' in operator_meta: {e}",
                Self::path(&operator_dir).display()
            );
        }
        Ok(kek)
    }

    /// Mint a new KEK file. Only [`Self::load_or_create`] calls this, after
    /// establishing that no KEK ever existed.
    fn create(operator_dir: &Path, owner: KekOwner) -> io::Result<Self> {
        let path = Self::path(operator_dir);
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        rand::rngs::OsRng.fill_bytes(&mut key[..]);
        // Written whole to a private temp file, then linked into place: the
        // link fails if another launch won the race, and a crash never leaves
        // a short KEK file behind.
        let tmp = operator_dir.join(format!(
            ".{KEK_FILENAME}.tmp-{}-{}",
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
            let body = Zeroizing::new(format!("{HEADER}{}\n", hex::encode(&key[..])));
            let written = file
                .write_all(body.as_bytes())
                .and_then(|()| file.sync_all());
            if let Err(e) = written {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        }
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                if let Ok(dir) = fs::File::open(operator_dir) {
                    let _ = dir.sync_all();
                }
                tracing::warn!(
                    "secrets: created the operator KEK at {} — back it up with operator/; \
                     losing it loses every principal's own secrets",
                    path.display()
                );
                Ok(Self { key })
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Self::load(&path, owner),
            Err(e) => Err(e),
        }
    }

    fn load(path: &Path, owner: KekOwner) -> io::Result<Self> {
        let invalid = |why: String| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {why}", path.display()),
            )
        };
        let mut file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(invalid("not a regular file".into()));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(invalid(format!(
                "mode {:o} is open to group or other (expected 0600)",
                meta.mode() & 0o7777
            )));
        }
        if owner == KekOwner::Root && (meta.uid() != 0 || meta.gid() != 0) {
            return Err(invalid(format!(
                "owned by {}:{}, not root:root",
                meta.uid(),
                meta.gid()
            )));
        }
        let mut text = Zeroizing::new(String::new());
        (&mut file).take(256).read_to_string(&mut text)?;
        let hex = text
            .trim_end()
            .strip_prefix(HEADER)
            .ok_or_else(|| invalid("unrecognised format".into()))?;
        let bytes = Zeroizing::new(hex::decode(hex).map_err(|_| invalid("not hex".into()))?);
        if bytes.len() != KEY_LEN {
            return Err(invalid(format!("not {KEY_LEN} bytes")));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&bytes);
        Ok(Self { key })
    }

    /// The wrapping key for one principal's store — the only key material a
    /// child ever receives.
    pub fn wrap_key_for(&self, principal: PrincipalId) -> WrapKey {
        WrapKey::derive(&self.key, &principal.to_string())
            .expect("a PrincipalId's text form is a valid store principal")
    }

    /// The hex form a test can search a child's environment for.
    #[cfg(test)]
    pub(crate) fn hex_for_tests(&self) -> String {
        hex::encode(&self.key[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use crate::server::operator::{open_accounts, Opener, Ownership};
    use sqlx::SqlitePool;

    /// An operator root (owners unchecked) with a real `accounts.db`, whose
    /// `operator_meta` the KEK marker lives in.
    struct Fixture {
        _tmp: tempfile::TempDir,
        root: OperatorRoot,
        pool: SqlitePool,
    }

    impl Fixture {
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

        fn kek_path(&self) -> PathBuf {
            SecretsKek::path(&self.root.operator_dir())
        }

        async fn load_or_create(&self) -> io::Result<SecretsKek> {
            let mut conn = self.pool.acquire().await.unwrap();
            SecretsKek::load_or_create(&self.root, KekOwner::Any, &mut conn).await
        }

        async fn marker(&self) -> Option<String> {
            let mut conn = self.pool.acquire().await.unwrap();
            marker(&mut conn).await.unwrap()
        }

        /// A principal's `data/secrets/envelope.json`, as `PrincipalStore`
        /// would leave it (content is irrelevant to the scan).
        fn plant_store(&self, id: PrincipalId) -> PathBuf {
            let secrets = self.root.principal_data(id).join("secrets");
            fs::create_dir_all(&secrets).unwrap();
            let envelope = secrets.join("envelope.json");
            fs::write(&envelope, "{}").unwrap();
            envelope
        }
    }

    #[tokio::test]
    async fn created_once_0600_recorded_then_reloaded_identically() {
        let fx = Fixture::new().await;
        assert_eq!(fx.marker().await, None);
        let first = fx.load_or_create().await.unwrap();
        let path = fx.kek_path();
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.mode() & 0o7777, 0o600);
        let created = fx.marker().await.expect("KEK creation is recorded");
        assert!(created.parse::<u64>().unwrap() > 0, "{created}");
        let second = fx.load_or_create().await.unwrap();
        assert_eq!(*first.key, *second.key, "never re-minted");
        assert_eq!(fx.marker().await, Some(created), "recorded once");
        // No temp file left behind.
        let stray: Vec<_> = fs::read_dir(fx.root.operator_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(KEK_FILENAME) && n != KEK_FILENAME)
            .collect();
        assert!(stray.is_empty(), "{stray:?}");
        assert!(!format!("{first:?}").contains(&first.hex_for_tests()));
    }

    /// R3 (WP-21 review): a KEK that existed and is now missing (lost or a
    /// partial restore) is never silently replaced. Each piece of evidence
    /// alone is enough, and nothing is written on the refusal path.
    #[tokio::test]
    async fn a_lost_kek_is_refused_not_reminted() {
        // (1) The operator_meta marker alone (no store yet).
        let fx = Fixture::new().await;
        fx.load_or_create().await.unwrap();
        fs::remove_file(fx.kek_path()).unwrap();
        let err = fx.load_or_create().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("Restore operator/secrets-kek from backup"),
            "{msg}"
        );
        assert!(msg.contains(KEK_MARKER), "{msg}");
        assert!(err.get_ref().unwrap().is::<KekLost>());
        assert!(!fx.kek_path().exists(), "nothing minted");

        // (2) A principal store alone (marker lost with a partial restore of
        // accounts.db, or written by a build that predates the marker).
        let fx = Fixture::new().await;
        let envelope = fx.plant_store(PrincipalId::new_v7());
        let err = fx.load_or_create().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
        let msg = err.to_string();
        assert!(msg.contains(&envelope.display().to_string()), "{msg}");
        assert!(
            msg.contains("Restore operator/secrets-kek from backup"),
            "{msg}"
        );
        assert!(!fx.kek_path().exists(), "nothing minted");
        assert_eq!(fx.marker().await, None, "nothing recorded");

        // Restoring the backup is the fix: the same KEK loads again.
        let fx = Fixture::new().await;
        let kek = fx.load_or_create().await.unwrap();
        let backup = fs::read(fx.kek_path()).unwrap();
        fx.plant_store(PrincipalId::new_v7());
        fs::remove_file(fx.kek_path()).unwrap();
        assert!(fx.load_or_create().await.is_err());
        fs::write(fx.kek_path(), &backup).unwrap();
        fs::set_permissions(fx.kek_path(), fs::Permissions::from_mode(0o600)).unwrap();
        let restored = fx.load_or_create().await.unwrap();
        assert_eq!(*kek.key, *restored.key);
    }

    /// The scan finds only real `data/secrets/envelope.json` files and never
    /// follows a principal-planted symlink; with neither evidence, a first
    /// launch beside other principals' (store-less) trees mints normally.
    #[tokio::test]
    async fn the_store_scan_skips_symlinks_and_storeless_principals() {
        let fx = Fixture::new().await;
        // Principals with no store yet: a bare data dir, a data dir with an
        // empty secrets dir.
        fs::create_dir_all(fx.root.principal_data(PrincipalId::new_v7())).unwrap();
        fs::create_dir_all(
            fx.root
                .principal_data(PrincipalId::new_v7())
                .join("secrets"),
        )
        .unwrap();
        // A symlinked `secrets/` pointing at a real store elsewhere.
        let elsewhere = fx._tmp.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("envelope.json"), "{}").unwrap();
        let sneaky = fx.root.principal_data(PrincipalId::new_v7());
        fs::create_dir_all(&sneaky).unwrap();
        std::os::unix::fs::symlink(&elsewhere, sneaky.join("secrets")).unwrap();
        assert_eq!(existing_stores(&fx.root).unwrap(), Vec::<PathBuf>::new());
        fx.load_or_create().await.unwrap();

        let real = fx.plant_store(PrincipalId::new_v7());
        assert_eq!(existing_stores(&fx.root).unwrap(), vec![real]);
    }

    #[tokio::test]
    async fn an_unsafe_or_malformed_kek_is_refused_never_regenerated() {
        let fx = Fixture::new().await;
        let path = fx.kek_path();
        let kek = fx.load_or_create().await.unwrap();
        let body = fs::read(&path).unwrap();

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let err = fx.load_or_create().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        fs::write(&path, "ikenga-secrets-kek:v1:abcd\n").unwrap();
        assert!(fx.load_or_create().await.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"ikenga-secrets-kek:v1:abcd\n");

        fs::remove_file(&path).unwrap();
        let elsewhere = fx.root.operator_dir().join("elsewhere");
        fs::write(&elsewhere, &body).unwrap();
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(fx.load_or_create().await.is_err());

        // Unprivileged, a root-owner check always fails.
        if unsafe { libc::geteuid() } != 0 {
            fs::remove_file(&path).unwrap();
            fs::rename(&elsewhere, &path).unwrap();
            let mut conn = fx.pool.acquire().await.unwrap();
            assert!(
                SecretsKek::load_or_create(&fx.root, KekOwner::Root, &mut conn)
                    .await
                    .is_err()
            );
        }
        drop(kek);
    }

    #[tokio::test]
    async fn each_principal_gets_a_distinct_key_that_is_not_the_kek() {
        let fx = Fixture::new().await;
        let kek = fx.load_or_create().await.unwrap();
        let (a, b) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let ka = kek.wrap_key_for(a);
        let kb = kek.wrap_key_for(b);
        assert_eq!(ka.principal(), a.to_string());
        let (ea, eb) = (ka.to_env_value(), kb.to_env_value());
        assert_ne!(ea.split(':').nth(2), eb.split(':').nth(2));
        assert!(!ea.contains(&kek.hex_for_tests()));
        assert_eq!(
            kek.wrap_key_for(a).to_env_value().as_str(),
            ea.as_str(),
            "deterministic: the same principal reopens its store after a broker restart"
        );
    }

    /// Real uids. `#[ignore]`d; the `t1-root` CI job runs them as root with
    /// only the T1 capabilities, from a world-traversable `/opt/t1` copy of
    /// this test binary (`--ignored --test-threads=1 t1_root`).
    mod t1_root {
        use super::*;
        use std::ffi::OsString;
        use std::os::unix::fs::MetadataExt;

        use crate::executor::t1::tests::t1_root::{home_for, require_root};
        use crate::executor::t1::tests::{config, piped, principal};
        use crate::executor::t1::T1Executor;
        use crate::executor::{Principal, SpawnSpec};
        use crate::secrets::principal_store::{PrincipalStore, WRAP_KEY_ENV};
        use crate::secrets::SecretsStore;
        use crate::server::broker::children::T1Launcher;
        use crate::server::operator::{OperatorRoot, Ownership};

        const ENTRY: &str =
            "server::operator::secrets_kek::tests::t1_root::t1_root_secrets_child_entry";
        const OWN_DATA: &str = "IKENGA_T1ROOT_SECRETS_DATA";
        const OTHER_DATA: &str = "IKENGA_T1ROOT_SECRETS_OTHER";
        const KEK_PATH: &str = "IKENGA_T1ROOT_SECRETS_KEK";

        /// This process's environment as Rust sees it, `NAME=value\0`-joined.
        fn env_hex() -> String {
            let mut out = Vec::new();
            for (k, v) in std::env::vars_os() {
                out.extend_from_slice(k.as_encoded_bytes());
                out.push(b'=');
                out.extend_from_slice(v.as_encoded_bytes());
                out.push(0);
            }
            hex::encode(out)
        }

        /// The child side, run as a principal's uid through the real T1
        /// executor with the broker's real `host_env`. The hand-off was
        /// captured before `main` (`.init_array`), so by the time this test
        /// body runs the key must already be out of the environment and the
        /// process non-dumpable. Prints its environment; exits non-zero on
        /// the first failed check, naming it on stderr.
        #[test]
        #[ignore = "t1-root (secrets child entry)"]
        fn t1_root_secrets_child_entry() {
            let Some(own) = std::env::var_os(OWN_DATA) else {
                return;
            };
            let fail = |code: i32, why: String| -> ! {
                eprintln!("secrets child: {why}");
                std::process::exit(code)
            };
            // L21-5: captured at exec, before the test harness (or, in the
            // daemon, the Tokio runtime) started a thread.
            if std::env::var_os(WRAP_KEY_ENV).is_some() {
                fail(
                    19,
                    "the hand-off was still in the environment after exec".into(),
                );
            }
            // SAFETY: PR_GET_DUMPABLE reads one flag.
            if unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } != 0 {
                fail(
                    18,
                    "the child is still dumpable after taking its key".into(),
                );
            }
            // Non-dumpable: `/proc/<pid>/environ` is root's, so no process of
            // this uid (this one included) can read the block the key was in.
            match fs::read("/proc/self/environ") {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                other => fail(
                    20,
                    format!("/proc/self/environ readable by the principal: {other:?}"),
                ),
            }
            println!("ENV:{}", env_hex());
            let key = match WrapKey::take_from_env() {
                Some(Ok(key)) => key,
                Some(Err(e)) => fail(10, e),
                None => fail(11, format!("no {WRAP_KEY_ENV}")),
            };
            if std::env::var_os(WRAP_KEY_ENV).is_some() {
                fail(12, "the wrapping key is still in the environment".into());
            }
            if WrapKey::take_from_env().is_some() {
                fail(21, "the hand-off was handed over twice".into());
            }
            // The master KEK is out of reach (operator/ is root 0700).
            let kek = std::env::var_os(KEK_PATH).unwrap();
            if fs::File::open(&kek).is_ok() {
                fail(13, "a principal opened the operator KEK".into());
            }
            let store = PrincipalStore::open(Path::new(&own), &key)
                .unwrap_or_else(|e| fail(14, format!("open own store: {e}")));
            store
                .set("WHO", key.principal())
                .unwrap_or_else(|e| fail(15, format!("set: {e}")));
            if store.get("WHO").ok().flatten().as_deref() != Some(key.principal()) {
                fail(16, "own value did not read back".into());
            }
            // Another principal's store is closed to this uid by the kernel.
            let other = PathBuf::from(std::env::var_os(OTHER_DATA).unwrap());
            for path in [
                other.join("secrets"),
                other.join("secrets/envelope.json"),
                other.join("secrets/values.json"),
            ] {
                if fs::read(&path).is_ok() || fs::read_dir(&path).is_ok() {
                    fail(17, format!("read {}", path.display()));
                }
            }
            std::process::exit(0);
        }

        fn data_for(tmp: &Path, name: &str, uid: u32) -> PathBuf {
            let dir = tmp.join(name);
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::lchown(&dir, Some(uid), Some(uid)).unwrap();
            dir
        }

        async fn run_child(
            exec: &T1Executor,
            p: &Principal,
            kek: &SecretsKek,
            own: &Path,
            other: &Path,
            kek_path: &Path,
        ) -> String {
            let mut spec = SpawnSpec::new(std::env::current_exe().unwrap());
            spec.args(
                [
                    "--exact",
                    ENTRY,
                    "--ignored",
                    "--test-threads=1",
                    "--nocapture",
                    "-q",
                ]
                .map(OsString::from),
            )
            .principal(Some(p.clone()))
            .env(OWN_DATA, own)
            .env(OTHER_DATA, other)
            .env(KEK_PATH, kek_path)
            // The deny floor drops this from a spec: only `host_env` passes it.
            .env(WRAP_KEY_ENV, "v1:smuggled:00");
            let host = T1Launcher::host_env("per-child-token", &kek.wrap_key_for(p.id));
            let out = exec
                .spawn_piped_with_host_env(spec, piped(), &host)
                .unwrap()
                .wait_with_output()
                .await
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(
                out.status.success(),
                "secrets child {} failed ({:?}): {}\n{stdout}",
                p.uid,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            let hex = stdout
                .lines()
                .find_map(|l| l.strip_prefix("ENV:"))
                .unwrap_or_else(|| panic!("no ENV: line: {stdout}"));
            String::from_utf8_lossy(&hex::decode(hex).unwrap()).into_owned()
        }

        /// WP-21: two principals' stores are isolated by uid and by key; the
        /// master KEK is root 0600 and never in a child's environment; the
        /// wrapping key is out of the child's environment before `main` and
        /// the child is non-dumpable (L21-5).
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_principal_stores_are_isolated_and_the_kek_stays_root() {
            require_root();
            let (ua, ub) = (28_550, 28_551);
            let (_ta, home_a) = home_for(ua);
            let (_tb, home_b) = home_for(ub);
            let (a, b) = (principal(ua, &home_a), principal(ub, &home_b));

            let tmp = tempfile::tempdir().unwrap();
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            let pool = crate::server::operator::open_accounts(
                &root,
                crate::server::operator::Opener::Broker,
            )
            .await
            .unwrap();
            let mut meta = pool.acquire().await.unwrap();
            let kek = SecretsKek::load_or_create(&root, KekOwner::Root, &mut meta)
                .await
                .unwrap();
            let kek_path = SecretsKek::path(&root.operator_dir());
            let meta = fs::symlink_metadata(&kek_path).unwrap();
            assert!(meta.is_file());
            assert_eq!(
                (meta.uid(), meta.gid(), meta.mode() & 0o7777),
                (0, 0, 0o600),
                "operator/secrets-kek is root:root 0600"
            );
            let kek_hex = kek.hex_for_tests();

            let data_a = data_for(tmp.path(), "a", ua);
            let data_b = data_for(tmp.path(), "b", ub);
            let exec = T1Executor::new(config());
            for (p, own, other) in [(&a, &data_a, &data_b), (&b, &data_b, &data_a)] {
                // That the broker handed over exactly this principal's key is
                // proven below: the store the child created with it opens
                // with `wrap_key_for(p.id)` (and a smuggled spec value would
                // have failed the child's take, exit 10).
                let env = run_child(&exec, p, &kek, own, other, &kek_path).await;
                let own_key = kek.wrap_key_for(p.id).to_env_value();
                assert!(!env.contains("v1:smuggled"), "a spec can't set the key");
                assert!(!env.contains(&kek_hex), "never the master KEK");
                let other_id = if p.id == a.id { b.id } else { a.id };
                let other_key = kek.wrap_key_for(other_id).to_env_value();
                let other_hex = other_key.rsplit(':').next().unwrap();
                assert!(!env.contains(other_hex), "never another principal's key");
                let own_hex = own_key.rsplit(':').next().unwrap();
                assert!(!env.contains(own_hex), "taken out at exec");
            }

            // §4 / I-9: each store is its uid's, 0700 / 0600.
            for (data, uid) in [(&data_a, ua), (&data_b, ub)] {
                for (path, mode) in [
                    (data.join("secrets"), 0o700),
                    (data.join("secrets/envelope.json"), 0o600),
                    (data.join("secrets/values.json"), 0o600),
                ] {
                    let m = fs::symlink_metadata(&path).unwrap();
                    assert_eq!(
                        (m.uid(), m.mode() & 0o7777),
                        (uid, mode),
                        "{}",
                        path.display()
                    );
                }
            }

            // Key isolation, even for root holding the files: Bob's key never
            // opens Ada's store, and each opens with its own.
            let err = PrincipalStore::open(&data_a, &kek.wrap_key_for(b.id)).unwrap_err();
            assert!(err.is_invalid(), "{err}");
            let ada = PrincipalStore::open(&data_a, &kek.wrap_key_for(a.id)).unwrap();
            assert_eq!(ada.get("WHO").unwrap(), Some(a.id.to_string()));
            let bob = PrincipalStore::open(&data_b, &kek.wrap_key_for(b.id)).unwrap();
            assert_eq!(bob.get("WHO").unwrap(), Some(b.id.to_string()));
        }
    }
}
