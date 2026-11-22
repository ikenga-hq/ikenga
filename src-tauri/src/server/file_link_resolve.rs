//! `file_link_resolve`: what an outside link may carry, decided by the
//! creator's **own** child (box-files WP-15; 06-file-links-security §4).
//!
//! The T1 broker never opens, stats or canonicalizes a principal's path: it
//! is root. To create a link it calls this `internal` arm on the creator's
//! child (a broker-originated call with the internal header and no share
//! header, so the guard here is the account's own, unnarrowed one), and the
//! kernel enforces that the checks run with the account's permissions.
//!
//! [`check`] is §4.1's checks 1 to 4, plus (for WP-16's live read) check 5
//! (the fresh canonical path equals the stored one) and check 6 (a byte cap):
//!
//! 1. [`PathGuard::resolve`] succeeds: canonical, inside this account's
//!    `fs_roots`, outside the daemon's reserved state;
//! 2. `open(O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)` succeeds, the
//!    open handle is a regular file, and `/proc/self/fd/<n>` names exactly
//!    the canonical path (no swap between resolve and open). `O_NONBLOCK` is
//!    there so a FIFO planted at the path cannot hang the worker;
//! 3. no component directly under the account's home starts with `.`
//!    (`~/.ssh`, `~/.config`, `~/.claude`, `~/.ikenga` …); deeper
//!    dot-directories (`<project>/.company/`) are allowed (O-6);
//! 4. the size is at most `LINK_MAX_BYTES` (and the caller's cap);
//! 5a. the open handle's owner (`fstat`, never a second path lookup, so it
//!    can't be raced) is this process's own uid. An account can widen its own
//!    `fs_roots` to `/etc` or `/` (finding A-01), and a world-readable file
//!    it does not own, such as `/etc/passwd`, must still not be linkable.
//!    Refused as `NotLinkable`: the same answer as any other refusal.
//!
//! Linux-only in practice: outside links exist only on a T1 server. On any
//! other OS every check refuses (no `server::broker` reference anywhere).

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::rpc::RpcResponse;
use super::rpc_shell::PathGuard;
use super::AppState;
use crate::access::file_links::{self, Resolved, LINK_MAX_BYTES};
use crate::access::{AccessError, Code};

/// Why a path can't be linked. Every variant is `not_found` on the wire;
/// the message differs only where it helps the owner and reveals nothing
/// about anything outside their own allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Outside the allowlist, reserved, missing, not a regular file, not
    /// owned by this account's own uid, not UTF-8, or unopenable. One answer
    /// for all of them.
    NotLinkable,
    Folder,
    /// Under a dot-directory directly in the home (check 3).
    Hidden,
    TooLarge,
    /// Check 5: the path no longer resolves to the stored one.
    Moved,
    /// Not a Linux server.
    NoServer,
}

impl Refusal {
    pub(crate) fn to_access(self) -> AccessError {
        let msg = match self {
            Refusal::NotLinkable | Refusal::Moved => "no such file you can share by link",
            Refusal::Folder => "a folder can't be shared by an outside link; pick a file",
            Refusal::Hidden => {
                "files in hidden folders of your home (like ~/.ssh) can't be shared by link"
            }
            Refusal::TooLarge => return file_links::too_large(),
            Refusal::NoServer => file_links::NEEDS_SERVER,
        };
        AccessError::new(Code::NotFound, msg)
    }
}

/// A path that passed every check, with its open handle (WP-16 streams
/// from this handle, never by reopening the path).
#[derive(Debug)]
// `file` is read by WP-16's live read (`/internal/file-link/read`).
#[allow(dead_code)]
pub(crate) struct Linkable {
    pub canonical: PathBuf,
    pub file: std::fs::File,
    pub size: u64,
}

/// The account's home, canonical; `None` when `HOME` is unset or gone.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok())
}

/// Check 3: whether `canonical` sits under a dot-entry directly in `home`
/// (or is one).
fn under_home_dotdir(canonical: &Path, home: &Path) -> bool {
    canonical
        .strip_prefix(home)
        .ok()
        .and_then(|rest| rest.components().next())
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|first| first.starts_with('.'))
}

/// Check 2: open exactly `canonical` and confirm the handle is it.
#[cfg(target_os = "linux")]
fn open_exact(canonical: &Path) -> Result<(std::fs::File, std::fs::Metadata), Refusal> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(canonical)
        .map_err(|_| Refusal::NotLinkable)?;
    let meta = file.metadata().map_err(|_| Refusal::NotLinkable)?;
    if meta.is_dir() {
        return Err(Refusal::Folder);
    }
    if !meta.is_file() {
        return Err(Refusal::NotLinkable);
    }
    // A-01: the handle's own metadata (fstat), checked before the size so a
    // file of another uid reveals nothing, not even that it is too large.
    if !owned_by_self(&meta) {
        return Err(Refusal::NotLinkable);
    }
    let named = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| Refusal::NotLinkable)?;
    if named != canonical {
        return Err(Refusal::Moved);
    }
    Ok((file, meta))
}

/// Whether an open handle's `fstat` owner is this process's effective uid
/// (the account's own child runs as the account's uid).
#[cfg(target_os = "linux")]
fn owned_by_self(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    meta.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(target_os = "linux"))]
fn open_exact(_canonical: &Path) -> Result<(std::fs::File, std::fs::Metadata), Refusal> {
    Err(Refusal::NoServer)
}

/// §4.1 checks 1-4 on `input`; with `expected`, also check 5 (the canonical
/// path must equal it byte for byte); `max` is check 6's cap (never above
/// [`LINK_MAX_BYTES`]). Blocking: run it on the blocking pool.
pub(crate) fn check(
    guard: &PathGuard,
    home: Option<&Path>,
    input: &str,
    expected: Option<&Path>,
    max: u64,
) -> Result<Linkable, Refusal> {
    if cfg!(not(target_os = "linux")) {
        return Err(Refusal::NoServer);
    }
    guard.ready().map_err(|_| Refusal::NotLinkable)?;
    let canonical = guard.resolve(input).map_err(|_| Refusal::NotLinkable)?;
    if canonical.to_str().is_none() {
        return Err(Refusal::NotLinkable);
    }
    if expected.is_some_and(|e| e != canonical) {
        return Err(Refusal::Moved);
    }
    // Without a home there is no telling a secret from a document: refuse.
    let home = home.ok_or(Refusal::NotLinkable)?;
    if under_home_dotdir(&canonical, home) {
        return Err(Refusal::Hidden);
    }
    let (file, meta) = open_exact(&canonical)?;
    let size = meta.len();
    if size > LINK_MAX_BYTES.min(max) {
        return Err(Refusal::TooLarge);
    }
    Ok(Linkable {
        canonical,
        file,
        size,
    })
}

/// The `file_link_resolve {path}` arm → `{path, name, size, mime,
/// previewKind}` or `not_found: …`. Class `internal`: the pre-hook already
/// refused anything but the broker's own call.
pub(crate) async fn arm(state: &AppState, args: &Value) -> RpcResponse {
    let Some(path) = args.get("path").and_then(Value::as_str).map(str::to_string) else {
        return RpcResponse::error(
            AccessError::new(Code::InvalidRequest, "`path` is required").to_string(),
        );
    };
    let guard = state.path_guard.clone();
    let res = tokio::task::spawn_blocking(move || {
        let home = home_dir();
        check(&guard, home.as_deref(), &path, None, LINK_MAX_BYTES).map_err(|r| (r, path))
    })
    .await;
    match res {
        Ok(Ok(linkable)) => {
            let name = linkable
                .canonical
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let canonical = linkable.canonical.to_string_lossy().into_owned();
            RpcResponse::success(Resolved::from_parts(canonical, &name, linkable.size).to_json())
        }
        Ok(Err((refusal, path))) => {
            // The owner's own path in the owner's own log; never a token.
            tracing::debug!(?refusal, %path, "file_link_resolve refused");
            RpcResponse::error(refusal.to_access().to_string())
        }
        Err(e) => RpcResponse::error(AccessError::internal(e).to_string()),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fixture {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        outside: PathBuf,
        guard: PathGuard,
    }

    /// `<tmp>/home` is the account's home and its whole allowlist;
    /// `<tmp>/home/data` is the daemon's data dir (reserved);
    /// `<tmp>/outside.txt` is outside the allowlist.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let home = base.join("home");
        std::fs::create_dir_all(home.join("data")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join("proj/.company")).unwrap();
        std::fs::write(home.join("r.pdf"), b"%PDF-1.7 r").unwrap();
        std::fs::write(home.join("s.pdf"), b"%PDF-1.7 s").unwrap();
        std::fs::write(home.join("data/ikenga.db"), b"db").unwrap();
        std::fs::write(home.join(".ssh/id_ed25519"), b"key").unwrap();
        std::fs::write(home.join(".bashrc"), b"rc").unwrap();
        std::fs::write(home.join("proj/.company/x.md"), b"# x").unwrap();
        let outside = base.join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        let roots_file = base.join("roots.json");
        std::fs::write(
            &roots_file,
            serde_json::json!({ "roots": [home.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let guard = PathGuard::roots(Arc::new(roots)).reserving(
            crate::server::reserved::Reserved::new(Some(home.join("data")), vec![]),
        );
        Fixture {
            _tmp: tmp,
            home,
            outside,
            guard,
        }
    }

    fn run(f: &Fixture, input: &str) -> Result<Linkable, Refusal> {
        check(&f.guard, Some(&f.home), input, None, LINK_MAX_BYTES)
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn a_plain_file_in_the_home_is_linkable_and_canonical() {
        let f = fixture();
        let l = run(&f, &s(&f.home.join("r.pdf"))).unwrap();
        assert_eq!(l.canonical, f.home.join("r.pdf"));
        assert_eq!(l.size, 10);
        // A spelling with `..` that stays inside still stores the canonical path.
        let l = run(&f, &s(&f.home.join("proj/../r.pdf"))).unwrap();
        assert_eq!(l.canonical, f.home.join("r.pdf"));
    }

    /// LS-05: traversal, /proc, the daemon's own state and a folder.
    #[test]
    fn traversal_proc_reserved_and_folders_are_refused() {
        let f = fixture();
        let up = format!("{}/proj/../../outside.txt", s(&f.home));
        assert_eq!(run(&f, &up).unwrap_err(), Refusal::NotLinkable);
        assert_eq!(run(&f, &s(&f.outside)).unwrap_err(), Refusal::NotLinkable);
        assert_eq!(
            run(&f, "/proc/self/environ").unwrap_err(),
            Refusal::NotLinkable
        );
        assert_eq!(run(&f, "/etc/passwd").unwrap_err(), Refusal::NotLinkable);
        assert_eq!(
            run(&f, &s(&f.home.join("data/ikenga.db"))).unwrap_err(),
            Refusal::NotLinkable,
            "the daemon's data dir is reserved"
        );
        assert_eq!(
            run(&f, &s(&f.home.join("proj"))).unwrap_err(),
            Refusal::Folder
        );
        assert_eq!(
            run(&f, &s(&f.home.join("missing.pdf"))).unwrap_err(),
            Refusal::NotLinkable
        );
    }

    /// LS-08: dot-directories directly in the home are refused; deeper ones
    /// are not.
    #[test]
    fn home_dotdirs_are_refused_but_project_dotdirs_are_not() {
        let f = fixture();
        assert_eq!(
            run(&f, &s(&f.home.join(".ssh/id_ed25519"))).unwrap_err(),
            Refusal::Hidden
        );
        assert_eq!(
            run(&f, &s(&f.home.join(".bashrc"))).unwrap_err(),
            Refusal::Hidden
        );
        let l = run(&f, &s(&f.home.join("proj/.company/x.md"))).unwrap();
        assert_eq!(l.canonical, f.home.join("proj/.company/x.md"));
        // Without a home nothing is linkable.
        assert_eq!(
            check(
                &f.guard,
                None,
                &s(&f.home.join("r.pdf")),
                None,
                LINK_MAX_BYTES
            )
            .unwrap_err(),
            Refusal::NotLinkable
        );
    }

    #[test]
    fn symlinks_resolve_to_their_target_or_are_refused() {
        let f = fixture();
        // Inside the allowlist: the link stores the target's canonical path.
        std::os::unix::fs::symlink(f.home.join("r.pdf"), f.home.join("alias.pdf")).unwrap();
        let l = run(&f, &s(&f.home.join("alias.pdf"))).unwrap();
        assert_eq!(l.canonical, f.home.join("r.pdf"));
        // Out of it: refused.
        std::os::unix::fs::symlink("/etc/hostname", f.home.join("host.txt")).unwrap();
        assert_eq!(
            run(&f, &s(&f.home.join("host.txt"))).unwrap_err(),
            Refusal::NotLinkable
        );
        // A symlink into a home dot-dir is refused by its target.
        std::os::unix::fs::symlink(f.home.join(".ssh/id_ed25519"), f.home.join("k.txt")).unwrap();
        assert_eq!(
            run(&f, &s(&f.home.join("k.txt"))).unwrap_err(),
            Refusal::Hidden
        );
    }

    /// Check 5 (WP-16's read): the file replaced by a symlink, or moved away,
    /// no longer matches the stored path.
    #[test]
    fn a_replaced_or_moved_file_fails_the_exact_path_check() {
        let f = fixture();
        let stored = f.home.join("r.pdf");
        let again = |input: &Path| {
            check(
                &f.guard,
                Some(&f.home),
                &s(input),
                Some(&stored),
                LINK_MAX_BYTES,
            )
        };
        assert!(again(&stored).is_ok());
        std::fs::remove_file(&stored).unwrap();
        std::os::unix::fs::symlink(f.home.join("s.pdf"), &stored).unwrap();
        assert_eq!(again(&stored).unwrap_err(), Refusal::Moved);
        std::fs::remove_file(&stored).unwrap();
        assert_eq!(
            again(&stored).unwrap_err(),
            Refusal::NotLinkable,
            "moved away"
        );
        std::fs::write(&stored, b"back").unwrap();
        assert!(again(&stored).is_ok(), "moved back");
    }

    #[test]
    fn size_caps_refuse_a_file_over_the_limit() {
        let f = fixture();
        assert_eq!(
            check(&f.guard, Some(&f.home), &s(&f.home.join("r.pdf")), None, 3).unwrap_err(),
            Refusal::TooLarge
        );
        // 1 GiB + 1 byte, sparse: costs no disk.
        let big = f.home.join("big.bin");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(LINK_MAX_BYTES + 1)
            .unwrap();
        assert_eq!(run(&f, &s(&big)).unwrap_err(), Refusal::TooLarge);
        let edge = f.home.join("edge.bin");
        std::fs::File::create(&edge)
            .unwrap()
            .set_len(LINK_MAX_BYTES)
            .unwrap();
        assert_eq!(run(&f, &s(&edge)).unwrap().size, LINK_MAX_BYTES);
    }

    /// A FIFO at the path is refused without blocking (`O_NONBLOCK`).
    #[test]
    fn a_fifo_is_refused_without_hanging() {
        let f = fixture();
        let fifo = f.home.join("pipe.txt");
        let c = std::ffi::CString::new(s(&fifo)).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(run(&f, &s(&fifo)).unwrap_err(), Refusal::NotLinkable);
    }

    /// A-01: a world-readable file of another uid (root's `/etc/hostname`,
    /// `/etc/passwd`) is refused even with `/etc` added to the allowlist,
    /// exactly as a missing file is; the account's own file still passes.
    #[test]
    fn a_file_owned_by_another_uid_is_refused_even_inside_the_allowlist() {
        let f = fixture();
        let me = unsafe { libc::geteuid() };
        let roots_file = f.home.parent().unwrap().join("roots-wide.json");
        std::fs::write(
            &roots_file,
            serde_json::json!({ "roots": [s(&f.home), "/etc"] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let wide = PathGuard::roots(Arc::new(roots)).reserving(
            crate::server::reserved::Reserved::new(Some(f.home.join("data")), vec![]),
        );
        let run_wide = |p: &str| check(&wide, Some(&f.home), p, None, LINK_MAX_BYTES);
        let mut probed = 0;
        for p in ["/etc/hostname", "/etc/passwd"] {
            use std::os::unix::fs::MetadataExt;
            let Ok(meta) = std::fs::metadata(p) else {
                continue;
            };
            if !meta.is_file() || meta.uid() == me {
                continue;
            }
            probed += 1;
            assert_eq!(run_wide(p).unwrap_err(), Refusal::NotLinkable, "{p}");
            // The stored-path read (check 5) refuses it the same way.
            assert_eq!(
                check(&wide, Some(&f.home), p, Some(Path::new(p)), LINK_MAX_BYTES).unwrap_err(),
                Refusal::NotLinkable,
                "{p}"
            );
        }
        if me != 0 {
            assert!(probed > 0, "no root-owned file in /etc to probe");
        }
        // The account's own file, through the same widened guard.
        let l = run_wide(&s(&f.home.join("r.pdf"))).unwrap();
        assert_eq!(l.canonical, f.home.join("r.pdf"));
        assert!(owned_by_self(&l.file.metadata().unwrap()));
    }

    #[test]
    fn refusals_are_not_found_on_the_wire() {
        for r in [
            Refusal::NotLinkable,
            Refusal::Folder,
            Refusal::Hidden,
            Refusal::TooLarge,
            Refusal::Moved,
            Refusal::NoServer,
        ] {
            assert!(r.to_access().to_string().starts_with("not_found:"), "{r:?}");
        }
    }

    #[test]
    fn the_answer_is_classified_by_the_shared_table() {
        let f = fixture();
        let l = run(&f, &s(&f.home.join("proj/.company/x.md"))).unwrap();
        let v = Resolved::from_parts(s(&l.canonical), "x.md", l.size).to_json();
        assert_eq!(v["previewKind"], "text");
        assert_eq!(v["mime"], "text/plain; charset=utf-8");
        assert_eq!(v["size"], 3);
    }
}
