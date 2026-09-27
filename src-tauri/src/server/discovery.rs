//! Daemon discovery files: `{pid, host, port, token, version}`.
//!
//! `run_server` writes one next to its data dir and one in a per-user temp
//! location; the desktop app's `pty::daemon_client::init_daemon` reads them to
//! find a daemon it can reuse. Both files carry the bearer token, and that
//! token grants a shell as the daemon's user, so:
//!
//! * they are written owner-only (0600 on Unix) through a fresh temp file and
//!   a rename, so a pre-existing file never keeps looser permissions and a
//!   planted symlink is replaced rather than followed;
//! * the temp-dir copy is per user. `/tmp` is shared on Linux, and a fixed
//!   name there let one user's daemon clobber (or another user pre-plant) the
//!   file every user's app reads;
//! * readers refuse a file that another user owns, or a symlink
//!   (`is_trusted`), so a planted file can't point the app at a daemon
//!   somebody else controls; a file we own with loose bits is tightened to
//!   owner-only on read (`tighten`).
//!
//! On Windows `temp_dir()` is already per user (`%LOCALAPPDATA%\Temp`) and
//! inherits a per-user ACL, so no mode bits are set there.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The per-user temp-dir discovery file.
///
/// Unix: `$XDG_RUNTIME_DIR/ikenga-daemon.json` when that is set (a 0700
/// per-user dir), else `<temp>/ikenga-daemon-<uid>.json`. Windows: the
/// already-per-user `<temp>/ikenga-daemon.json`.
pub fn user_temp_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(dir).join("ikenga-daemon.json");
        }
        // Safety: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("ikenga-daemon-{uid}.json"))
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir().join("ikenga-daemon.json")
    }
}

/// Write `contents` to `path`, readable and writable by the owner only.
///
/// Goes through a uniquely named sibling created with `create_new` (O_EXCL, so
/// it can't be a pre-planted symlink) and mode 0600 from the first byte, then
/// renames over `path`. A rename replaces whatever sits at `path` — including a
/// symlink — instead of writing through it.
pub fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "daemon.json".into());
    let tmp = dir.join(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Whether a discovery file may be trusted: on Unix it must be a regular file
/// (not a symlink) owned by the current effective user. Ownership is what
/// stops a planted file; loose mode bits are a confidentiality problem that
/// [`tighten`] fixes, not a reason to distrust our own file — daemons from
/// before this module wrote 0644, and refusing those would strand an old
/// daemon the app then can't shut down. Always true for a regular file on
/// other platforms (see the module docs).
pub fn is_trusted(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // symlink_metadata: a symlink is never trusted, whoever it points at.
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        // Safety: geteuid has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        meta.file_type().is_file() && meta.uid() == euid
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Drop group/other permission bits from a discovery file we own. Returns
/// true if it had any. No-op off Unix.
pub fn tighten(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        let mode = meta.permissions().mode();
        if mode & 0o077 == 0 {
            return false;
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o700)).is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_private_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        write_private(&path, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
        assert!(is_trusted(&path));
    }

    #[cfg(unix)]
    #[test]
    fn written_file_is_0600_even_over_a_world_readable_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_private(&path, "new").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(is_trusted(&path));
    }

    #[cfg(unix)]
    #[test]
    fn a_legacy_0644_file_we_own_is_trusted_and_tightened_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.json");
        std::fs::write(&path, "legacy").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            is_trusted(&path),
            "our own legacy file must stay readable to retire its daemon"
        );

        assert!(tighten(&path));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!tighten(&path), "already owner-only");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_replaces_a_planted_symlink_instead_of_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "untouched").unwrap();
        let path = dir.path().join("daemon.json");
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        assert!(!is_trusted(&path), "a symlink must not be trusted");

        write_private(&path, "daemon").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_file());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "daemon");
    }

    #[cfg(unix)]
    #[test]
    fn user_temp_path_is_per_user() {
        let p = user_temp_path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let in_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|d| !d.is_empty());
        if in_runtime_dir {
            assert_eq!(name, "ikenga-daemon.json");
        } else {
            let uid = unsafe { libc::geteuid() };
            assert_eq!(name, format!("ikenga-daemon-{uid}.json"));
        }
    }
}
