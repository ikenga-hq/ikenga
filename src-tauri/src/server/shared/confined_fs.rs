//! How far a scaffold or workspace-scan body may follow the paths it touches
//! (WP-19 slice 8): the write-side sibling of `projects::FsReach` for bodies
//! that create a tree of arbitrary depth, not four fixed nodes.
//!
//! * [`Reach::Follow`] is the desktop, unchanged: `std::fs::create_dir_all`,
//!   `std::fs::write` and `Path::exists`, symlinks followed as the OS resolves
//!   them. Its caller is the user's own renderer on the same uid.
//! * [`Reach::Confined`] is the daemon, whose caller is a remote token holder.
//!   The daemon arm has already resolved the base it hands over to its
//!   canonical form and checked it against its `PathGuard` (fs allowlist +
//!   its own `--data-dir` / discovery file). Then, for every node it creates
//!   or writes:
//!   * the path must be absolute with no `..`;
//!   * no component of it that exists may be a symlink, live or dangling
//!     (`lstat` on each ancestor) — so nothing below the canonical base can
//!     redirect a write, and the node itself is never followed;
//!   * the path goes through the guard again (`check`), which also compares
//!     the data dir by inode, so a bind mount inside the base is refused;
//!   * a directory is created one component at a time with `create_dir`
//!     (which does not follow a link planted at the name), and a file is
//!     written to an exclusively created sibling temp file (`create_new`) that
//!     is then `rename`d over the target — a rename replaces a link rather
//!     than writing through it.
//!
//!   Reads ([`Reach::admits`]) admit a path only when it canonicalizes to a
//!   location the guard admits; anything else reads as absent.
//!
//! Every refusal is an `io::Error` (`PermissionDenied`), so a body that wraps
//! IO errors in its own words (`"write SKILL.md: {e}"`) reports it in the same
//! place and shape as any other write failure — and the desktop's error
//! strings, which only ever see real IO errors, are unchanged.

use std::io::{self, Write as _};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

/// A path check: `Ok` when the path may be touched. The daemon passes its
/// `PathGuard::check_maybe_missing` (allowlist + reserved).
pub type PathCheck<'a> = &'a (dyn Fn(&Path) -> Result<(), String> + Sync);

/// How far a body may follow the paths it touches (see the module doc).
#[derive(Clone, Copy)]
pub enum Reach<'a> {
    /// The desktop: paths as given, symlinks followed.
    Follow,
    /// The daemon: no symlink below the canonical base, every node checked.
    Confined(PathCheck<'a>),
}

/// The words every confined symlink refusal starts with.
pub const SYMLINK_REFUSAL: &str = "refusing to follow a symlink";

/// Monotonic suffix so two concurrent confined writes never share a temp name.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn refused(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg)
}

impl<'a> Reach<'a> {
    /// Is something at `path`? The desktop asks `Path::exists` (a dangling
    /// link reads as absent, and a write then follows it); the daemon asks
    /// `lstat`, so a link of any kind is present — and is then skipped or
    /// refused, never written through.
    pub fn exists(&self, path: &Path) -> bool {
        match self {
            Reach::Follow => path.exists(),
            Reach::Confined(_) => std::fs::symlink_metadata(path).is_ok(),
        }
    }

    /// May `path` be read? Always on the desktop; on the daemon only when it
    /// canonicalizes (exists, links resolved) to a location the guard admits.
    pub fn admits(&self, path: &Path) -> bool {
        match self {
            Reach::Follow => true,
            Reach::Confined(check) => path
                .canonicalize()
                .map(|c| check(&c).is_ok())
                .unwrap_or(false),
        }
    }

    /// `std::fs::create_dir_all`, confined on the daemon.
    pub fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        match self {
            Reach::Follow => std::fs::create_dir_all(path),
            Reach::Confined(check) => confined_create_dir_all(*check, path),
        }
    }

    /// `std::fs::write`, confined on the daemon.
    pub fn write(&self, path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
        match self {
            Reach::Follow => std::fs::write(path, contents),
            Reach::Confined(check) => confined_write(*check, path, contents.as_ref()),
        }
    }
}

/// Refuse a symlink at `path` itself (`lstat`; a missing path is fine).
fn refuse_link(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(refused(format!("{SYMLINK_REFUSAL}: {}", path.display())))
        }
        _ => Ok(()),
    }
}

/// The confined preconditions for creating or writing `path` (module doc).
fn vet(check: PathCheck<'_>, path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(refused(format!("path is not absolute: {}", path.display())));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(refused(format!(
            "path may not contain `..`: {}",
            path.display()
        )));
    }
    for ancestor in path.ancestors() {
        refuse_link(ancestor)?;
    }
    check(path).map_err(refused)
}

fn confined_create_dir_all(check: PathCheck<'_>, path: &Path) -> io::Result<()> {
    vet(check, path)?;
    // The missing tail, outermost first. `lstat`, not `exists`: a dangling
    // link is present (and `vet` has already refused any link).
    let mut missing = Vec::new();
    let mut cur = path;
    while std::fs::symlink_metadata(cur).is_err() {
        missing.push(cur);
        cur = match cur.parent() {
            Some(p) => p,
            None => break,
        };
    }
    for dir in missing.into_iter().rev() {
        match std::fs::create_dir(dir) {
            Ok(()) => {}
            // Raced into existence: acceptable only as a real directory.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => refuse_link(dir)?,
            Err(e) => return Err(e),
        }
    }
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(refused(format!("{SYMLINK_REFUSAL}: {}", path.display())));
    }
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("not a directory: {}", path.display()),
        ));
    }
    Ok(())
}

fn confined_write(check: PathCheck<'_>, path: &Path, contents: &[u8]) -> io::Result<()> {
    vet(check, path)?;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(refused(format!("not a file path: {}", path.display())));
    };
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(
        ".{}.ikenga-tmp-{}-{seq}",
        name.to_string_lossy(),
        std::process::id()
    ));
    // Exclusive create: a link (or anything else) planted at the temp name
    // fails this rather than being followed, and is left alone.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    file.write_all(contents).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })?;
    drop(file);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow_under(base: &Path) -> impl Fn(&Path) -> Result<(), String> + Sync + '_ {
        move |p: &Path| {
            if p.starts_with(base) {
                Ok(())
            } else {
                Err(format!("path outside allowlist: {}", p.display()))
            }
        }
    }

    #[test]
    fn follow_is_plain_std_fs() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("a/b");
        Reach::Follow.create_dir_all(&d).unwrap();
        Reach::Follow.write(&d.join("f"), "x").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "x");
        assert!(Reach::Follow.exists(&d.join("f")));
        assert!(Reach::Follow.admits(Path::new("/nowhere")));
    }

    #[test]
    fn confined_creates_and_overwrites_inside_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let check = allow_under(&base);
        let reach = Reach::Confined(&check);
        let d = base.join("a/b/c");
        reach.create_dir_all(&d).unwrap();
        reach.write(&d.join("f"), "one").unwrap();
        reach.write(&d.join("f"), "two").unwrap();
        assert_eq!(std::fs::read_to_string(d.join("f")).unwrap(), "two");
        let leftovers: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left behind");
        let e = reach.write(&base.join("../x"), "x").unwrap_err();
        assert!(e.to_string().contains("`..`"), "{e}");
        let e = reach.write(Path::new("rel/x"), "x").unwrap_err();
        assert!(e.to_string().contains("not absolute"), "{e}");
    }

    #[test]
    #[cfg(unix)]
    fn confined_refuses_live_and_dangling_links_anywhere_on_the_path() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (base, outside) = (root.join("base"), root.join("outside"));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // Everything under `root` is admitted, so only the link rule refuses.
        let check = allow_under(&root);
        let reach = Reach::Confined(&check);

        symlink(&outside, base.join("live")).unwrap();
        symlink(outside.join("gone"), base.join("dangling")).unwrap();
        symlink(outside.join("f"), base.join("file-link")).unwrap();
        for bad in [
            base.join("live/sub"),
            base.join("live"),
            base.join("dangling/sub"),
            base.join("dangling"),
        ] {
            let e = reach.create_dir_all(&bad).unwrap_err();
            assert!(e.to_string().contains(SYMLINK_REFUSAL), "{bad:?}: {e}");
        }
        for bad in [
            base.join("live/f"),
            base.join("dangling"),
            base.join("file-link"),
        ] {
            let e = reach.write(&bad, "x").unwrap_err();
            assert!(e.to_string().contains(SYMLINK_REFUSAL), "{bad:?}: {e}");
        }
        assert!(!outside.join("sub").exists());
        assert!(!outside.join("gone").exists());
        assert!(!outside.join("f").exists());
        assert!(reach.exists(&base.join("dangling")), "lstat sees the link");
        assert!(!Reach::Follow.exists(&base.join("dangling")));
        assert!(!reach.admits(&base.join("dangling")));
    }

    #[test]
    fn confined_consults_the_guard_for_every_node() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let ok = root.join("ok");
        let check = allow_under(&ok);
        let reach = Reach::Confined(&check);
        let e = reach.create_dir_all(&root.join("no/sub")).unwrap_err();
        assert!(e.to_string().contains("outside allowlist"), "{e}");
        assert!(!root.join("no").exists());
        std::fs::create_dir_all(root.join("ok")).unwrap();
        assert!(reach.admits(&root.join("ok")));
        assert!(!reach.admits(&root));
    }
}
