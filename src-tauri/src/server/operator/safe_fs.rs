//! fd-relative filesystem primitives for walks that root makes over trees a
//! principal (or an adopted T0 user) controls — `accounts adopt-t0` (review
//! S4-1).
//!
//! Path-based syscalls follow symlinks in every component but the last (and
//! `open`/`chmod`/`read_dir` follow the last one too), so a uid that can
//! rename entries in a tree root is walking can swap a component for a
//! symlink between root's `stat` and root's write, and aim that write at
//! `/etc`. Every operation here instead works on a held directory fd and one
//! name inside it:
//!
//! * directories are opened `O_DIRECTORY | O_NOFOLLOW`, so a symlink in their
//!   place fails (`ELOOP`/`ENOTDIR`) instead of being followed;
//! * every other entry is opened `O_PATH | O_NOFOLLOW` (a symlink yields the
//!   link itself), checked with `fstat` **on that fd**, and changed through
//!   the same fd (`fchownat(AT_EMPTY_PATH)`, `chmod` of
//!   `/proc/self/fd/<n>`), so what is changed is exactly the inode that was
//!   checked;
//! * a regular file is read only from an `O_NOFOLLOW | O_NONBLOCK` fd whose
//!   `fstat` says regular file.
//!
//! Callers check `st_nlink` on those fds: a file with more than one link
//! may also be named outside the tree, and changing it would change that
//! other name too.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

/// What an entry is (`lstat` semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File,
    Symlink,
    /// Sockets, FIFOs, devices.
    Other,
}

/// The parts of `struct stat` the walks use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stat {
    pub kind: Kind,
    /// Permission bits, set-id bits included (`st_mode & 0o7777`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
}

impl Stat {
    fn from_raw(st: &libc::stat) -> Self {
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFREG => Kind::File,
            libc::S_IFLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        Self {
            kind,
            mode: st.st_mode & 0o7777,
            uid: st.st_uid,
            gid: st.st_gid,
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
            #[allow(clippy::unnecessary_cast)] // u32 on some targets
            nlink: st.st_nlink as u64,
        }
    }

    /// The same inode as `other` (an `fstatat` of the name, then an
    /// `fstat` of the fd opened from it).
    pub fn same_inode(&self, other: &Stat) -> bool {
        (self.dev, self.ino) == (other.dev, other.ino)
    }
}

fn cstr(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: a NUL byte in a path", name.to_string_lossy()),
        )
    })
}

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// `fstat(fd)`. Works on `O_PATH` fds.
pub(crate) fn fstat(fd: RawFd) -> io::Result<Stat> {
    // SAFETY: all-zero is a valid `stat`; the kernel fills it.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid out-pointer.
    cvt(unsafe { libc::fstat(fd, &mut st) })?;
    Ok(Stat::from_raw(&st))
}

/// Change the owner of the inode `fd` refers to — never following a
/// symlink: on an `O_PATH | O_NOFOLLOW` fd of a link, the link itself.
pub(crate) fn chown_fd(fd: RawFd, uid: u32, gid: u32) -> io::Result<()> {
    // SAFETY: an empty, NUL-terminated path with AT_EMPTY_PATH targets `fd`.
    cvt(unsafe {
        libc::fchownat(
            fd,
            c"".as_ptr(),
            uid,
            gid,
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
        )
    })
    .map(drop)
}

/// Change the mode of the (non-symlink) inode `fd` refers to. `fchmod`
/// refuses `O_PATH` fds, so this goes through `/proc/self/fd/<fd>`, which
/// resolves to exactly that inode (what glibc's
/// `fchmodat(AT_SYMLINK_NOFOLLOW)` does too). A symlink is refused: its
/// mode means nothing, and chmod on its magic link would follow it.
pub(crate) fn chmod_fd(fd: RawFd, mode: u32) -> io::Result<()> {
    if fstat(fd)?.kind == Kind::Symlink {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to chmod a symlink",
        ));
    }
    let path = CString::new(format!("/proc/self/fd/{fd}")).expect("no NUL");
    // SAFETY: a valid C string.
    cvt(unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) })
        .map(drop)
        .map_err(|e| io::Error::new(e.kind(), format!("chmod through /proc/self/fd: {e}")))
}

/// A held directory. Every method resolves exactly one name inside it and
/// never follows a symlink in that name (except [`Dir::read_link`], which
/// reads one).
#[derive(Debug)]
pub(crate) struct Dir {
    fd: OwnedFd,
}

const DIR_FLAGS: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

impl Dir {
    fn openat_raw(dirfd: RawFd, name: &OsStr, flags: libc::c_int) -> io::Result<OwnedFd> {
        let c = cstr(name)?;
        // SAFETY: a valid C string; the fd is checked below.
        let fd = cvt(unsafe { libc::openat(dirfd, c.as_ptr(), flags) })?;
        // SAFETY: a fresh fd we own.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Open the absolute `path` one component at a time, refusing a symlink
    /// at **any** component. `path` must already be canonical (a canonical
    /// path has no symlinks, so a symlink found now was put there since).
    pub fn open_no_symlinks(path: &Path) -> io::Result<Dir> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: not an absolute path", path.display()),
            ));
        }
        let mut dir = Dir {
            fd: Self::openat_raw(
                libc::AT_FDCWD,
                OsStr::new("/"),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )?,
        };
        for comp in path.components() {
            match comp {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(name) => {
                    dir = dir.open_dir(name).map_err(|e| {
                        io::Error::new(
                            e.kind(),
                            format!(
                                "{}: {e} (a component is not a directory, or is a symlink)",
                                path.display()
                            ),
                        )
                    })?
                }
                Component::ParentDir | Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{}: not a canonical path", path.display()),
                    ))
                }
            }
        }
        Ok(dir)
    }

    pub fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// The directory `name`; a symlink there is an error.
    pub fn open_dir(&self, name: &OsStr) -> io::Result<Dir> {
        Ok(Dir {
            fd: Self::openat_raw(self.raw(), name, DIR_FLAGS)?,
        })
    }

    pub fn stat(&self) -> io::Result<Stat> {
        fstat(self.raw())
    }

    /// `fstatat(name, AT_SYMLINK_NOFOLLOW)`.
    pub fn stat_at(&self, name: &OsStr) -> io::Result<Stat> {
        let c = cstr(name)?;
        // SAFETY: all-zero is a valid `stat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid C string and out-pointer.
        cvt(unsafe { libc::fstatat(self.raw(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
        Ok(Stat::from_raw(&st))
    }

    /// [`Dir::stat_at`], `None` when there is no such entry.
    pub fn try_stat_at(&self, name: &OsStr) -> io::Result<Option<Stat>> {
        match self.stat_at(name) {
            Ok(st) => Ok(Some(st)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The names in this directory (no `.`/`..`), sorted.
    pub fn entries(&self) -> io::Result<Vec<OsString>> {
        // fdopendir takes ownership of the fd it is given: give it a dup.
        // SAFETY: plain fcntl on a valid fd.
        let dup = cvt(unsafe { libc::fcntl(self.raw(), libc::F_DUPFD_CLOEXEC, 0) })?;
        // SAFETY: `dup` is a valid directory fd we own; on success the DIR
        // owns it.
        let dirp = unsafe { libc::fdopendir(dup) };
        if dirp.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: still ours on failure.
            unsafe { libc::close(dup) };
            return Err(e);
        }
        // The dup shares the file offset with `self.fd`: start over.
        // SAFETY: a valid DIR.
        unsafe { libc::rewinddir(dirp) };
        let mut names = Vec::new();
        let result = loop {
            // SAFETY: resetting errno so a NULL from readdir can be told
            // apart (end of directory vs. error).
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: a valid DIR; the entry is valid until the next call.
            let ent = unsafe { libc::readdir(dirp) };
            if ent.is_null() {
                let e = io::Error::last_os_error();
                break match e.raw_os_error() {
                    Some(0) | None => Ok(()),
                    _ => Err(e),
                };
            }
            // SAFETY: d_name is NUL-terminated.
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsStr::from_bytes(name).to_os_string());
            }
        };
        // SAFETY: a valid DIR, closed once (this also closes `dup`).
        unsafe { libc::closedir(dirp) };
        result?;
        names.sort();
        Ok(names)
    }

    /// `name` for reading, never following a symlink and never blocking on
    /// a FIFO, with the `fstat` of the opened fd.
    pub fn open_file(&self, name: &OsStr) -> io::Result<(File, Stat)> {
        let fd = Self::openat_raw(
            self.raw(),
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC,
        )?;
        let st = fstat(fd.as_raw_fd())?;
        Ok((File::from(fd), st))
    }

    /// An `O_PATH | O_NOFOLLOW` fd of `name` (the link itself when `name`
    /// is a symlink), with its `fstat`.
    pub fn open_path(&self, name: &OsStr) -> io::Result<(OwnedFd, Stat)> {
        let fd = Self::openat_raw(
            self.raw(),
            name,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        let st = fstat(fd.as_raw_fd())?;
        Ok((fd, st))
    }

    /// Create the new regular file `name` (never an existing one, never
    /// through a symlink) for writing.
    pub fn create_file(&self, name: &OsStr, mode: u32) -> io::Result<File> {
        let c = cstr(name)?;
        // SAFETY: a valid C string.
        let fd = cvt(unsafe {
            libc::openat(
                self.raw(),
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        })?;
        // SAFETY: a fresh fd we own.
        Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// `mkdirat(name)`, then the exact `mode` (the umask is not applied).
    pub fn mkdir(&self, name: &OsStr, mode: u32) -> io::Result<Dir> {
        let c = cstr(name)?;
        // SAFETY: a valid C string.
        cvt(unsafe { libc::mkdirat(self.raw(), c.as_ptr(), mode as libc::mode_t) })?;
        let dir = self.open_dir(name)?;
        dir.chmod(mode)?;
        Ok(dir)
    }

    pub fn symlink(&self, target: &Path, name: &OsStr) -> io::Result<()> {
        let (t, c) = (cstr(target.as_os_str())?, cstr(name)?);
        // SAFETY: valid C strings.
        cvt(unsafe { libc::symlinkat(t.as_ptr(), self.raw(), c.as_ptr()) }).map(drop)
    }

    /// The target of the symlink `name`.
    pub fn read_link(&self, name: &OsStr) -> io::Result<PathBuf> {
        let c = cstr(name)?;
        let mut buf = vec![0u8; 256];
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe {
                libc::readlinkat(self.raw(), c.as_ptr(), buf.as_mut_ptr().cast(), buf.len())
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let n = n as usize;
            if n < buf.len() {
                buf.truncate(n);
                return Ok(PathBuf::from(OsString::from_vec(buf)));
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    /// `renameat(self/name, to/to_name)`. Never follows `name` (rename
    /// moves a symlink, not its target).
    pub fn rename(&self, name: &OsStr, to: &Dir, to_name: &OsStr) -> io::Result<()> {
        let (a, b) = (cstr(name)?, cstr(to_name)?);
        // SAFETY: valid C strings.
        cvt(unsafe { libc::renameat(self.raw(), a.as_ptr(), to.raw(), b.as_ptr()) }).map(drop)
    }

    pub fn unlink(&self, name: &OsStr) -> io::Result<()> {
        let c = cstr(name)?;
        // SAFETY: a valid C string.
        cvt(unsafe { libc::unlinkat(self.raw(), c.as_ptr(), 0) }).map(drop)
    }

    pub fn rmdir(&self, name: &OsStr) -> io::Result<()> {
        let c = cstr(name)?;
        // SAFETY: a valid C string.
        cvt(unsafe { libc::unlinkat(self.raw(), c.as_ptr(), libc::AT_REMOVEDIR) }).map(drop)
    }

    /// `fchmod` of this directory.
    pub fn chmod(&self, mode: u32) -> io::Result<()> {
        // SAFETY: a valid fd.
        cvt(unsafe { libc::fchmod(self.raw(), mode as libc::mode_t) }).map(drop)
    }

    /// `fchown` of this directory.
    pub fn chown(&self, uid: u32, gid: u32) -> io::Result<()> {
        // SAFETY: a valid fd.
        cvt(unsafe { libc::fchown(self.raw(), uid, gid) }).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn open_no_symlinks_refuses_a_symlink_at_any_component() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        fs::create_dir_all(base.join("a/b")).unwrap();
        std::os::unix::fs::symlink(base.join("a"), base.join("link")).unwrap();
        assert!(Dir::open_no_symlinks(&base.join("a/b")).is_ok());
        assert!(Dir::open_no_symlinks(&base.join("link/b")).is_err());
        assert!(Dir::open_no_symlinks(&base.join("link")).is_err());
        assert!(Dir::open_no_symlinks(Path::new("relative")).is_err());
        assert!(Dir::open_no_symlinks(&base.join("a/../a")).is_err());
    }

    #[test]
    fn entries_stat_and_o_path_changes_never_follow_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let tree = base.join("tree");
        fs::create_dir(&tree).unwrap();
        let outside = base.join("outside");
        fs::write(&outside, "keep").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("link")).unwrap();
        std::os::unix::fs::symlink(&base, tree.join("dirlink")).unwrap();
        fs::write(tree.join("f"), "x").unwrap();

        let dir = Dir::open_no_symlinks(&tree).unwrap();
        assert_eq!(dir.entries().unwrap(), ["dirlink", "f", "link"]);
        assert_eq!(dir.entries().unwrap().len(), 3, "re-readable");
        assert_eq!(dir.stat_at(OsStr::new("link")).unwrap().kind, Kind::Symlink);
        assert!(dir.open_dir(OsStr::new("dirlink")).is_err());
        assert!(dir.open_file(OsStr::new("link")).is_err());
        let (fd, st) = dir.open_path(OsStr::new("link")).unwrap();
        assert_eq!(st.kind, Kind::Symlink);
        assert!(chmod_fd(fd.as_raw_fd(), 0o600).is_err());
        assert_eq!(
            dir.read_link(OsStr::new("link")).unwrap(),
            outside.as_path()
        );
        // chown through the O_PATH fd of the link touches the link only.
        let me = (unsafe { libc::geteuid() }, unsafe { libc::getegid() });
        chown_fd(fd.as_raw_fd(), me.0, me.1).unwrap();
        assert_eq!(fs::metadata(&outside).unwrap().mode() & 0o777, 0o644);

        let (fd, st) = dir.open_path(OsStr::new("f")).unwrap();
        assert_eq!((st.kind, st.nlink), (Kind::File, 1));
        chmod_fd(fd.as_raw_fd(), 0o600).unwrap();
        assert_eq!(fs::metadata(tree.join("f")).unwrap().mode() & 0o777, 0o600);
        let (mut file, st) = dir.open_file(OsStr::new("f")).unwrap();
        assert_eq!(st.kind, Kind::File);
        let mut s = String::new();
        file.read_to_string(&mut s).unwrap();
        assert_eq!(s, "x");

        let sub = dir.mkdir(OsStr::new("sub"), 0o700).unwrap();
        assert_eq!(sub.stat().unwrap().mode, 0o700);
        dir.rename(OsStr::new("f"), &sub, OsStr::new("g")).unwrap();
        assert!(tree.join("sub/g").exists());
        sub.symlink(Path::new("/nowhere"), OsStr::new("l")).unwrap();
        sub.unlink(OsStr::new("l")).unwrap();
        let mut w = sub.create_file(OsStr::new("new"), 0o600).unwrap();
        std::io::Write::write_all(&mut w, b"n").unwrap();
        assert!(sub.create_file(OsStr::new("new"), 0o600).is_err(), "O_EXCL");
        assert!(dir.try_stat_at(OsStr::new("nope")).unwrap().is_none());
        sub.unlink(OsStr::new("g")).unwrap();
        sub.unlink(OsStr::new("new")).unwrap();
        dir.rmdir(OsStr::new("sub")).unwrap();
    }
}
