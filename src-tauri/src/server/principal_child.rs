//! The principal-child side of topology B (G-PRINCIPAL §3, §11.1 I-3).
//!
//! A T1 broker launches `ikenga-server --executor-tier t1 --principal-child`
//! once per principal, as that principal's uid, with `--data-dir
//! <root>/principals/<id>/data`. Before the child serves anything it:
//!
//! 1. verifies it is already dropped ([`executor::t1_child`]) and installs
//!    that executor — the boot refuses otherwise (DEC-R9-1);
//! 2. takes an exclusive `flock` on `<data>/.lock` **before** `PaDb` can open
//!    `ikenga.db` ([`DataDirLock`]): a second child for the same principal
//!    fails here and exits, so no two processes ever open the same
//!    `ikenga.db` (I-3, §5 row 9);
//! 3. watches its parent: a broker that dies without stopping its children
//!    would otherwise leave this one holding the lock, and the next broker's
//!    child for this principal could never start.
//!
//! It never opens an access store (G-ACCESS R-11, §1.7): `access_*` RPCs are
//! served by the broker, which never forwards them here.
//!
//! [`executor::t1_child`]: crate::executor::t1_child

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `<data>/.lock` (§4).
pub const LOCK_FILE: &str = ".lock";

/// An exclusive `flock` on `<data>/.lock`, held until drop (in production:
/// for the process lifetime). `flock` locks belong to the open file
/// description, so a second [`DataDirLock::acquire`] fails even inside the
/// same process.
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
    path: PathBuf,
}

impl DataDirLock {
    /// Take the lock without waiting. `WouldBlock` means another process (or
    /// another acquisition) holds it.
    pub fn acquire(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        // SAFETY: a valid fd owned by `file`.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let e = io::Error::last_os_error();
            return Err(if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "{} is held by another process: a principal's ikenga.db has exactly \
                         one opener (G-PRINCIPAL I-3)",
                        path.display()
                    ),
                )
            } else {
                e
            });
        }
        Ok(Self { _file: file, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolve when this process's parent changes (the broker died and the child
/// was re-parented). Polled: `PR_SET_PDEATHSIG` would have to be set in the
/// broker's `pre_exec`, after the uid change, and the kernel clears it on
/// credential changes anyway.
pub async fn parent_gone() {
    // SAFETY: no preconditions.
    let original = unsafe { libc::getppid() };
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        // SAFETY: as above.
        if unsafe { libc::getppid() } != original {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I-3: while one holder has the lock, a second acquisition — the second
    /// child for the same principal — fails, and it succeeds again once the
    /// first is gone.
    #[test]
    fn i3_a_second_flock_on_the_data_dir_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let first = DataDirLock::acquire(tmp.path()).expect("first child takes the lock");
        assert_eq!(first.path(), tmp.path().join(".lock"));
        let err = DataDirLock::acquire(tmp.path()).expect_err("a second child must not");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        assert!(err.to_string().contains("I-3"), "{err}");
        drop(first);
        DataDirLock::acquire(tmp.path()).expect("free again after the holder exits");
    }

    /// I-3 across processes: a separate process holding the flock blocks us.
    #[test]
    fn i3_a_lock_held_by_another_process_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join(".lock");
        std::fs::write(&lock, b"").unwrap();
        // `flock(1)` from util-linux, when present; skip otherwise.
        let Ok(mut holder) = std::process::Command::new("flock")
            .arg("-x")
            .arg(&lock)
            .arg("-c")
            .arg("sleep 5")
            .spawn()
        else {
            return;
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut blocked = false;
        while std::time::Instant::now() < deadline {
            match DataDirLock::acquire(tmp.path()) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    blocked = true;
                    break;
                }
                Ok(held) => drop(held),
                Err(e) => panic!("{e}"),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(blocked, "the other process's flock was never observed");
    }

    #[test]
    fn the_lock_file_is_never_followed_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("elsewhere");
        std::fs::write(&target, b"x").unwrap();
        std::os::unix::fs::symlink(&target, tmp.path().join(".lock")).unwrap();
        assert!(DataDirLock::acquire(tmp.path()).is_err());
    }
}
