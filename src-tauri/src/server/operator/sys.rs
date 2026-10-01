//! Thin libc wrappers for the operator (T1, Linux-only): identity, NSS
//! lookups and the shadow-file lock. Nothing here allocates between fork and
//! exec — none of it is on a spawn path.

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// The effective uid of this process.
pub(crate) fn geteuid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// A passwd entry as the provisioning core needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PasswdInfo {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
    pub shell: PathBuf,
}

/// `getpw*_r` / `getgr*_r` report "no such entry" either as a zero return
/// with a NULL result or, on some NSS backends, as one of these errnos
/// (`getpwnam_r(3)`, NOTES).
fn is_not_found(rc: i32) -> bool {
    matches!(rc, libc::ENOENT | libc::ESRCH | libc::EBADF | libc::EPERM)
}

const INITIAL_BUF: usize = 1024;
const MAX_BUF: usize = 1 << 20;

/// Run a reentrant NSS lookup, growing its scratch buffer on `ERANGE`.
fn with_buffer<T>(
    mut lookup: impl FnMut(&mut Vec<libc::c_char>) -> Result<Option<T>, i32>,
) -> io::Result<Option<T>> {
    let mut buf: Vec<libc::c_char> = vec![0; INITIAL_BUF];
    loop {
        match lookup(&mut buf) {
            Ok(found) => return Ok(found),
            Err(libc::ERANGE) if buf.len() < MAX_BUF => {
                let len = buf.len() * 2;
                buf.resize(len, 0);
            }
            Err(rc) if is_not_found(rc) => return Ok(None),
            Err(rc) => return Err(io::Error::from_raw_os_error(rc)),
        }
    }
}

/// SAFETY: `pw` must point at a passwd struct filled by a successful
/// `getpw*_r` whose string buffer is still alive.
unsafe fn passwd_info(pw: &libc::passwd) -> PasswdInfo {
    let s = |p: *const libc::c_char| {
        if p.is_null() {
            Vec::new()
        } else {
            CStr::from_ptr(p).to_bytes().to_vec()
        }
    };
    PasswdInfo {
        name: String::from_utf8_lossy(&s(pw.pw_name)).into_owned(),
        uid: pw.pw_uid,
        gid: pw.pw_gid,
        home: PathBuf::from(std::ffi::OsStr::from_bytes(&s(pw.pw_dir))),
        shell: PathBuf::from(std::ffi::OsStr::from_bytes(&s(pw.pw_shell))),
    }
}

/// `getpwuid_r`: the passwd entry holding `uid`, if any.
pub(crate) fn user_by_uid(uid: u32) -> io::Result<Option<PasswdInfo>> {
    with_buffer(|buf| {
        // SAFETY: all-zero is a valid `passwd` (null pointers, zero ids).
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call; `buf.len()` is its size.
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc != 0 {
            return Err(rc);
        }
        // SAFETY: on success `result` is either null or `&pw`, backed by `buf`.
        Ok((!result.is_null()).then(|| unsafe { passwd_info(&pw) }))
    })
}

/// `getpwnam_r`: the passwd entry named `name`, if any.
pub(crate) fn user_by_name(name: &str) -> io::Result<Option<PasswdInfo>> {
    let cname = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    with_buffer(|buf| {
        // SAFETY: as in `user_by_uid`.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: as in `user_by_uid`; `cname` is NUL-terminated.
        let rc = unsafe {
            libc::getpwnam_r(
                cname.as_ptr(),
                &mut pw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        // SAFETY: as in `user_by_uid`.
        Ok((!result.is_null()).then(|| unsafe { passwd_info(&pw) }))
    })
}

/// `getgrgid_r`: does a group hold `gid`?
pub(crate) fn group_gid_exists(gid: u32) -> io::Result<bool> {
    with_buffer(|buf| {
        // SAFETY: all-zero is a valid `group`.
        let mut gr: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call.
        let rc =
            unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc != 0 {
            return Err(rc);
        }
        Ok((!result.is_null()).then_some(()))
    })
    .map(|found| found.is_some())
}

/// `getgrnam_r`: does a group named `name` exist?
pub(crate) fn group_name_exists(name: &str) -> io::Result<bool> {
    let cname = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    with_buffer(|buf| {
        // SAFETY: all-zero is a valid `group`.
        let mut gr: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call; `cname` is NUL-terminated.
        let rc = unsafe {
            libc::getgrnam_r(
                cname.as_ptr(),
                &mut gr,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        Ok((!result.is_null()).then_some(()))
    })
    .map(|found| found.is_some())
}

extern "C" {
    // shadow.h (glibc; musl ships them too). Not bound by the `libc` crate.
    fn lckpwdf() -> libc::c_int;
    fn ulckpwdf() -> libc::c_int;
}

/// Holds the system shadow-file lock (`lckpwdf(3)`) until dropped.
pub(crate) struct PwdLock(());

impl PwdLock {
    /// Take `lckpwdf()`. glibc waits up to 15 s for a competing holder
    /// (`useradd`, `passwd`, another daemon) before failing.
    pub(crate) fn acquire() -> io::Result<Self> {
        // SAFETY: no preconditions; the lock is released in Drop.
        if unsafe { lckpwdf() } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PwdLock(()))
    }
}

impl Drop for PwdLock {
    fn drop(&mut self) {
        // SAFETY: we hold the lock taken in `acquire`.
        unsafe {
            ulckpwdf();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_resolves_and_an_unused_uid_does_not() {
        let root = user_by_uid(0).unwrap().expect("uid 0 has a passwd entry");
        assert_eq!(root.uid, 0);
        assert_eq!(user_by_name(&root.name).unwrap().unwrap().uid, 0);
        assert!(group_gid_exists(0).unwrap());
        // Far outside any distro or operator range.
        assert_eq!(user_by_uid(3_999_999_000).unwrap(), None);
        assert!(!group_gid_exists(3_999_999_000).unwrap());
        assert_eq!(user_by_name("ik-no-such-user-wp20").unwrap(), None);
        assert!(!group_name_exists("ik-no-such-group-wp20").unwrap());
    }
}
