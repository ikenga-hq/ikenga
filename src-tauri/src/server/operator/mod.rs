//! The T1 operator (G-PRINCIPAL §4, §6, §7): the root-owned operator root,
//! the local-accounts store `operator/accounts.db`, password hashing and the
//! provisioning core. Linux-only, like T1 itself.
//!
//! Under T1, `--data-dir` names the **operator root** (P-1):
//!
//! ```text
//! <root>/                     root:root 0755
//! ├── operator/               root:root 0700   accounts.db (0600), sessions.db, daemon.json, probe.json
//! └── principals/             root:root 0711   traverse, no listing
//!     └── <principal_id>/     uid:gid   0700
//!         ├── home/           uid:gid   0700   the passwd home
//!         └── data/           uid:gid   0700   the principal child's --data-dir
//!             └── tmp/        uid:gid   0700   TMPDIR (§9.3)
//! ```
//!
//! Module map (G-ACCESS R-6 keeps account provisioning apart from credential
//! resolution — `server::auth` — and proxying and child launch —
//! `server::broker`):
//!
//! * [`migrations`] — the embedded `accounts` set and the `(set, version)`
//!   bookkeeping table (R-1).
//! * [`auth_events`] — the one `auth_events` writer (R-2).
//! * [`accounts`] — row type, lookups, epoch bumps and the R-11 hook.
//! * [`password`] — argon2id hashing, the login verifier and its backoff.
//! * [`provision`] — §7.2 create (`create_in`, R-8), §7.3 disable / enable /
//!   passwd, the uid allocator and the `/etc` backends.
//! * [`cli`] — `ikenga-server accounts …`.
//! * [`probe`] — the §8 boot probe's operator side (steps 5 and 7, the
//!   probe-uid precondition, `probe.json`) and `ikenga-server probe`; the
//!   host side (steps 2–4, 6) is `executor::t1_probe`.
//! * [`reaper`] — the §7.3 uid-wide kill through the T1 executor.
//! * [`adopt_t0`] — `accounts adopt-t0`, the §11.2 T0 → T1 migration (and
//!   G-ACCESS R-10's archiving of the T0 access store).
//! * [`safe_fs`] — fd-relative, never-follow-a-symlink walks for root over
//!   trees another uid controls (used by [`adopt_t0`]).

pub mod accounts;
pub mod adopt_t0;
pub mod auth_events;
pub mod cli;
mod etc_files;
pub mod migrations;
pub mod password;
pub mod probe;
pub mod provision;
pub mod reaper;
mod safe_fs;
mod sys;

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;

use crate::executor::PrincipalId;

/// Files that only a T0 daemon writes at the top of its `--data-dir`. Any of
/// them at an operator root means the directory is (or was) a T0 install.
///
/// `daemon.json` is among them: the T0 daemon writes `<data_dir>/daemon.json`,
/// while the T1 broker writes `operator/daemon.json` (§4, `server::broker`),
/// so a second T1 boot never trips over its own discovery file.
const T0_MARKERS: &[&str] = &[
    "ikenga.db",
    "ikenga.db-wal",
    "fs_roots.json",
    "supabase.json",
    "daemon.json",
    "access.db",
];

/// Whether the §4 owners are enforced. Production is always [`Enforce`];
/// unit tests that run unprivileged build paths with the other variant, which
/// only exists under `cfg(test)`.
///
/// [`Enforce`]: Ownership::Enforce
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Operator dirs must be root-owned; principal dirs are chowned to the uid.
    Enforce,
    /// Modes are still applied and checked; owners are neither set nor checked.
    #[cfg(test)]
    SkipForTests,
}

/// Why an operator root can't be used.
#[derive(Debug)]
pub enum LayoutError {
    /// I-10: a T1 boot (or the accounts CLI) on a T0-shaped data dir.
    T0Layout {
        root: PathBuf,
        marker: &'static str,
    },
    Unsafe {
        path: PathBuf,
        why: String,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::T0Layout { root, marker } => write!(
                f,
                "{} is a T0 data dir (it holds {marker}); T1 refuses it rather than migrating it \
                 (G-PRINCIPAL I-10). Point --data-dir at a separate, empty directory for the \
                 operator root, then migrate this install into a principal with `ikenga-server \
                 accounts adopt-t0 --data-dir <operator-root> --from {} --home <the T0 \
                 daemon's home> <username>` (stop the T0 daemon first).",
                root.display(),
                root.display()
            ),
            LayoutError::Unsafe { path, why } => {
                write!(f, "refusing operator path {}: {why}", path.display())
            }
            LayoutError::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for LayoutError {}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> LayoutError + '_ {
    move |source| LayoutError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The `--data-dir` of a T1 server: paths and the §4 layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorRoot {
    root: PathBuf,
}

impl OperatorRoot {
    /// `root` must be absolute: every path derived from it lands in `accounts`
    /// rows and passwd entries.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, LayoutError> {
        let root = root.into();
        if !root.is_absolute() {
            return Err(LayoutError::Unsafe {
                path: root,
                why: "the operator root must be an absolute path".into(),
            });
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn operator_dir(&self) -> PathBuf {
        self.root.join("operator")
    }
    pub fn principals_dir(&self) -> PathBuf {
        self.root.join("principals")
    }
    pub fn accounts_db(&self) -> PathBuf {
        self.operator_dir().join("accounts.db")
    }
    pub fn sessions_db(&self) -> PathBuf {
        self.operator_dir().join("sessions.db")
    }
    pub fn probe_json(&self) -> PathBuf {
        self.operator_dir().join("probe.json")
    }
    pub fn principal_dir(&self, id: PrincipalId) -> PathBuf {
        self.principals_dir().join(id.to_string())
    }
    pub fn principal_home(&self, id: PrincipalId) -> PathBuf {
        self.principal_dir(id).join("home")
    }
    pub fn principal_data(&self, id: PrincipalId) -> PathBuf {
        self.principal_dir(id).join("data")
    }

    /// I-10: refuse a T0-shaped dir. Stricter than "`ikenga.db` present and
    /// `operator/` absent": any T0 marker at the root refuses, with or without
    /// `operator/`, because a T1 root never holds them (§4) and a mixed root
    /// would otherwise stop looking T0-shaped the moment `operator/` appeared.
    pub fn refuse_t0_layout(&self) -> Result<(), LayoutError> {
        for marker in T0_MARKERS {
            let path = self.root.join(marker);
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Err(LayoutError::T0Layout {
                        root: self.root.clone(),
                        marker,
                    })
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(&path)(e)),
            }
        }
        Ok(())
    }

    /// The three operator dirs with their §4 modes.
    fn operator_dirs(&self) -> [(PathBuf, u32); 3] {
        [
            (self.root.clone(), 0o755),
            (self.operator_dir(), 0o700),
            (self.principals_dir(), 0o711),
        ]
    }

    /// [`prepare`](Self::prepare) without writing anything (the read-only
    /// `ikenga-server probe`, §8 step 5): refuse a T0 layout, check every
    /// operator dir that exists, and return the ones that don't yet (the boot
    /// would create them).
    pub fn check_layout(&self, ownership: Ownership) -> Result<Vec<PathBuf>, LayoutError> {
        self.refuse_t0_layout()?;
        let mut missing = Vec::new();
        for (path, _) in self.operator_dirs() {
            match fs::symlink_metadata(&path) {
                Ok(meta) => check_existing_dir(&path, &meta, ownership)?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push(path),
                Err(e) => return Err(io_err(&path)(e)),
            }
        }
        Ok(missing)
    }

    /// Create or verify `<root>` (0755), `operator/` (0700) and `principals/`
    /// (0711) per §4 / §8 step 5: none may be a symlink or group/other
    /// writable, and under [`Ownership::Enforce`] each must be root-owned.
    /// Refuses a T0 layout first (I-10).
    pub fn prepare(&self, ownership: Ownership) -> Result<(), LayoutError> {
        self.refuse_t0_layout()?;
        for (path, mode) in self.operator_dirs() {
            ensure_dir(&path, mode, ownership)?;
        }
        Ok(())
    }
}

/// §8 step 5 for one existing operator dir: not a symlink, a directory, not
/// group/world writable, and (enforced) root-owned.
fn check_existing_dir(
    path: &Path,
    meta: &fs::Metadata,
    ownership: Ownership,
) -> Result<(), LayoutError> {
    let unsafe_because = |why: String| {
        Err(LayoutError::Unsafe {
            path: path.into(),
            why,
        })
    };
    if meta.file_type().is_symlink() {
        return unsafe_because("is a symlink".into());
    }
    if !meta.is_dir() {
        return unsafe_because("is not a directory".into());
    }
    if meta.mode() & 0o022 != 0 {
        return unsafe_because(format!(
            "is group- or world-writable (mode {:o})",
            meta.mode() & 0o7777
        ));
    }
    if ownership == Ownership::Enforce && (meta.uid() != 0 || meta.gid() != 0) {
        return unsafe_because(format!(
            "is owned by {}:{}, not root:root",
            meta.uid(),
            meta.gid()
        ));
    }
    Ok(())
}

fn ensure_dir(path: &Path, mode: u32, ownership: Ownership) -> Result<(), LayoutError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => check_existing_dir(path, &meta, ownership)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(mode)
                .create(path)
                .map_err(io_err(path))?;
        }
        Err(e) => return Err(io_err(path)(e)),
    }
    // Exact mode, whatever the umask did.
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_err(path))
}

/// How long a writer waits on `accounts.db`'s lock before `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Who is opening the store, which decides whether it may migrate (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opener {
    /// The T1 broker: the only migrator.
    Broker,
    /// The root CLI's `create`: refuses any schema it doesn't match, except
    /// that it may initialise a brand-new store (see
    /// [`migrations::Policy::InitialiseOnly`]) so the first admin can be
    /// created before the first T1 boot (§7.4).
    Cli,
    /// Every other CLI command: the store must already exist and be exactly
    /// current. Creates nothing (a mistyped `--data-dir` grows no store).
    CliExisting,
}

/// Open (creating if needed) `operator/accounts.db` with WAL, a busy timeout
/// and foreign keys on, and bring the `accounts` set to current as `opener`
/// is allowed to. The file is created `0600` before SQLite first opens it.
/// Call [`OperatorRoot::prepare`] first.
pub async fn open_accounts(root: &OperatorRoot, opener: Opener) -> anyhow::Result<SqlitePool> {
    let path = root.accounts_db();
    fs::OpenOptions::new()
        .create(opener != Opener::CliExisting)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(false)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    {
        let mut conn = pool.acquire().await?;
        let checked = match opener {
            Opener::Broker => migrations::apply(
                &mut conn,
                &migrations::ACCOUNTS,
                migrations::Policy::Migrate,
            )
            .await
            .map(drop),
            Opener::Cli => migrations::apply(
                &mut conn,
                &migrations::ACCOUNTS,
                migrations::Policy::InitialiseOnly,
            )
            .await
            .map(drop),
            Opener::CliExisting => {
                migrations::require_current(&mut conn, &migrations::ACCOUNTS).await
            }
        };
        if let Err(e) = checked {
            drop(conn);
            pool.close().await;
            return Err(e.into());
        }
    }
    Ok(pool)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A fresh operator root in a temp dir, prepared without owner checks.
    pub(crate) fn temp_root() -> (tempfile::TempDir, OperatorRoot) {
        let tmp = tempfile::tempdir().unwrap();
        let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
        root.prepare(Ownership::SkipForTests).unwrap();
        (tmp, root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_paths_follow_section_4() {
        let root = OperatorRoot::new("/opt/ikenga/data").unwrap();
        let id: PrincipalId = "01890a5d-ac96-774b-bcce-b302099a8057".parse().unwrap();
        assert_eq!(
            root.accounts_db(),
            Path::new("/opt/ikenga/data/operator/accounts.db")
        );
        assert_eq!(
            root.sessions_db(),
            Path::new("/opt/ikenga/data/operator/sessions.db")
        );
        assert_eq!(
            root.probe_json(),
            Path::new("/opt/ikenga/data/operator/probe.json")
        );
        assert_eq!(
            root.principal_home(id),
            Path::new("/opt/ikenga/data/principals/01890a5d-ac96-774b-bcce-b302099a8057/home")
        );
        assert_eq!(
            root.principal_data(id),
            Path::new("/opt/ikenga/data/principals/01890a5d-ac96-774b-bcce-b302099a8057/data")
        );
        assert!(OperatorRoot::new("relative/root").is_err());
    }

    #[test]
    fn prepare_creates_the_operator_dirs_with_their_modes() {
        let (_tmp, root) = test_support::temp_root();
        for (path, mode) in [
            (root.root().to_path_buf(), 0o755),
            (root.operator_dir(), 0o700),
            (root.principals_dir(), 0o711),
        ] {
            let meta = fs::symlink_metadata(&path).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.mode() & 0o7777, mode, "{}", path.display());
        }
        // Idempotent.
        root.prepare(Ownership::SkipForTests).unwrap();
    }

    /// I-10: a T0 data dir is refused, never adopted — and refusing writes
    /// nothing into it.
    #[test]
    fn t0_shaped_data_dir_is_refused() {
        for marker in T0_MARKERS {
            let tmp = tempfile::tempdir().unwrap();
            fs::write(tmp.path().join(marker), b"").unwrap();
            let root = OperatorRoot::new(tmp.path()).unwrap();
            let err = root.prepare(Ownership::SkipForTests).unwrap_err();
            assert!(
                matches!(err, LayoutError::T0Layout { .. }),
                "{marker}: {err}"
            );
            assert!(err.to_string().contains("accounts adopt-t0"), "{err}");
            assert!(
                !root.operator_dir().exists(),
                "refusal must not create operator/"
            );
        }
        // Mixed: operator/ present does not make a T0 marker acceptable.
        let (_tmp, root) = test_support::temp_root();
        fs::write(root.root().join("ikenga.db"), b"").unwrap();
        assert!(matches!(
            root.prepare(Ownership::SkipForTests),
            Err(LayoutError::T0Layout {
                marker: "ikenga.db",
                ..
            })
        ));
    }

    #[test]
    fn unsafe_operator_dirs_are_refused() {
        let (_tmp, root) = test_support::temp_root();
        fs::set_permissions(root.operator_dir(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            root.prepare(Ownership::SkipForTests),
            Err(LayoutError::Unsafe { .. })
        ));

        let tmp = tempfile::tempdir().unwrap();
        let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
        fs::create_dir(root.root()).unwrap();
        fs::set_permissions(root.root(), fs::Permissions::from_mode(0o755)).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.operator_dir()).unwrap();
        let err = root.prepare(Ownership::SkipForTests).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");
    }

    /// The read-only probe's step 5: checks what exists, creates nothing.
    #[test]
    fn check_layout_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
        let missing = root.check_layout(Ownership::SkipForTests).unwrap();
        assert_eq!(missing.len(), 3);
        assert!(!root.root().exists());

        root.prepare(Ownership::SkipForTests).unwrap();
        assert!(root
            .check_layout(Ownership::SkipForTests)
            .unwrap()
            .is_empty());
        fs::set_permissions(root.principals_dir(), fs::Permissions::from_mode(0o773)).unwrap();
        assert!(matches!(
            root.check_layout(Ownership::SkipForTests),
            Err(LayoutError::Unsafe { .. })
        ));
        fs::set_permissions(root.principals_dir(), fs::Permissions::from_mode(0o711)).unwrap();
        fs::write(root.root().join("ikenga.db"), b"").unwrap();
        assert!(matches!(
            root.check_layout(Ownership::SkipForTests),
            Err(LayoutError::T0Layout { .. })
        ));
    }

    /// Review F10: read-only commands neither create nor initialise a store.
    #[tokio::test]
    async fn the_cli_existing_opener_creates_nothing() {
        let (_tmp, root) = test_support::temp_root();
        assert!(open_accounts(&root, Opener::CliExisting).await.is_err());
        assert!(!root.accounts_db().exists());
        // An empty (never initialised) file is refused too, and left empty.
        fs::write(root.accounts_db(), b"").unwrap();
        let err = open_accounts(&root, Opener::CliExisting).await.unwrap_err();
        assert!(err.to_string().contains("never been initialised"), "{err}");
        open_accounts(&root, Opener::Cli)
            .await
            .unwrap()
            .close()
            .await;
        open_accounts(&root, Opener::CliExisting)
            .await
            .unwrap()
            .close()
            .await;
    }

    #[tokio::test]
    async fn accounts_db_is_created_0600_and_migrated() {
        let (_tmp, root) = test_support::temp_root();
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let meta = fs::metadata(root.accounts_db()).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600);
        let mut conn = pool.acquire().await.unwrap();
        migrations::require_current(&mut conn, &migrations::ACCOUNTS)
            .await
            .unwrap();
        drop(conn);
        pool.close().await;
        // Re-opening as either opener is a no-op on a current store.
        open_accounts(&root, Opener::Broker)
            .await
            .unwrap()
            .close()
            .await;
        open_accounts(&root, Opener::Cli)
            .await
            .unwrap()
            .close()
            .await;
    }
}
