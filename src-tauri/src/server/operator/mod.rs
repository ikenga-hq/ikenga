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
//! resolution, proxying and child launch, which land in later slices):
//!
//! * [`migrations`] — the embedded `accounts` set and the `(set, version)`
//!   bookkeeping table (R-1).
//! * [`auth_events`] — the one `auth_events` writer (R-2).
//! * [`accounts`] — row type, lookups, epoch bumps and the R-11 hook.
//! * [`password`] — argon2id hashing, the login verifier and its backoff.
//! * [`provision`] — §7.2 create (`create_in`, R-8), §7.3 disable / enable /
//!   passwd, the uid allocator and the `/etc` backends.
//! * [`cli`] — `ikenga-server accounts …`.

pub mod accounts;
pub mod auth_events;
pub mod cli;
mod etc_files;
pub mod migrations;
pub mod password;
pub mod provision;
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
                 (G-PRINCIPAL I-10). Stop the T0 daemon and migrate it with \
                 `ikenga-server accounts adopt-t0 --from {} …` into a fresh operator root, \
                 or point --data-dir at an empty directory",
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

    /// Create or verify `<root>` (0755), `operator/` (0700) and `principals/`
    /// (0711) per §4 / §8 step 5: none may be a symlink or group/other
    /// writable, and under [`Ownership::Enforce`] each must be root-owned.
    /// Refuses a T0 layout first (I-10).
    pub fn prepare(&self, ownership: Ownership) -> Result<(), LayoutError> {
        self.refuse_t0_layout()?;
        for (path, mode) in [
            (self.root.clone(), 0o755),
            (self.operator_dir(), 0o700),
            (self.principals_dir(), 0o711),
        ] {
            ensure_dir(&path, mode, ownership)?;
        }
        Ok(())
    }
}

fn ensure_dir(path: &Path, mode: u32, ownership: Ownership) -> Result<(), LayoutError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(LayoutError::Unsafe {
                    path: path.into(),
                    why: "is a symlink".into(),
                });
            }
            if !meta.is_dir() {
                return Err(LayoutError::Unsafe {
                    path: path.into(),
                    why: "is not a directory".into(),
                });
            }
            if meta.mode() & 0o022 != 0 {
                return Err(LayoutError::Unsafe {
                    path: path.into(),
                    why: format!(
                        "is group- or world-writable (mode {:o})",
                        meta.mode() & 0o7777
                    ),
                });
            }
            if ownership == Ownership::Enforce && (meta.uid() != 0 || meta.gid() != 0) {
                return Err(LayoutError::Unsafe {
                    path: path.into(),
                    why: format!("is owned by {}:{}, not root:root", meta.uid(), meta.gid()),
                });
            }
        }
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
    /// The root CLI: refuses any schema it doesn't match, except that it may
    /// initialise a brand-new store (see [`migrations::Policy::InitialiseOnly`]).
    Cli,
}

/// Open (creating if needed) `operator/accounts.db` with WAL, a busy timeout
/// and foreign keys on, and bring the `accounts` set to current as `opener`
/// is allowed to. The file is created `0600` before SQLite first opens it.
/// Call [`OperatorRoot::prepare`] first.
pub async fn open_accounts(root: &OperatorRoot, opener: Opener) -> anyhow::Result<SqlitePool> {
    let path = root.accounts_db();
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(BUSY_TIMEOUT)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await?;
    let policy = match opener {
        Opener::Broker => migrations::Policy::Migrate,
        Opener::Cli => migrations::Policy::InitialiseOnly,
    };
    {
        let mut conn = pool.acquire().await?;
        migrations::apply(&mut conn, &migrations::ACCOUNTS, policy).await?;
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
            assert!(err.to_string().contains("adopt-t0"), "{err}");
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
