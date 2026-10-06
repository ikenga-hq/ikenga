//! The `fs_kind` / `fs_mime` / `fs_search` / `fs_rename` bodies, shared by
//! the desktop commands (`commands::fs`) and the daemon's `/api/rpc` arms
//! (WP-19 slice 5a). [`exists`] is the daemon's copy of the desktop
//! `fs_exists` contract (the desktop command keeps its own body).
//!
//! Each takes the path resolver as an argument instead of calling
//! `resolve_allowlisted` itself: the desktop passes exactly that (so its
//! behaviour is unchanged); the daemon passes its `PathGuard`, which expands
//! and canonicalizes a caller's path the same way (`path_allow::
//! expand_absolute` + `canonical_for_check`) and checks it against the same
//! `fs_roots` set — `<data-dir>/fs_roots.json` in production. Canonicalizing
//! resolves `..` and symlinks before the check, so neither reaches outside the
//! allowlist.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Resolves a caller's path to a canonical path inside the allowlist, or
/// says why not.
pub type Resolve<'a> = &'a (dyn Fn(&str) -> Result<PathBuf, String> + Sync);

// Mirror of the JS-side IGNORED_DIRS in files-mode.tsx — folders we skip when
// `show_ignored` is false. The dot-prefix filter handles `.git`/`.next`/`.cache`
// separately via `show_hidden`.
const IGNORED_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out"];

/// The default `fs_search` cap when the caller passes no `limit`.
pub const DEFAULT_SEARCH_LIMIT: usize = 500;

#[derive(Serialize)]
pub struct FsSearchResult {
    pub matches: Vec<String>,
    pub truncated: bool,
}

/// The desktop's `FileReadResult`: the file's bytes (a JSON array of numbers) and its MIME.
#[derive(Debug, Serialize)]
pub struct FsReadResult {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// Read a whole file as bytes. Mirrors the desktop `fs_read`, so a binary file reads too
/// (the daemon's original arm used `read_to_string` and returned a bare string, which no
/// viewer could use and which failed outright on anything that was not UTF-8).
pub async fn read(resolve: Resolve<'_>, path: &str) -> Result<FsReadResult, String> {
    let resolved = resolve(path)?;
    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    let mime = mime_guess::from_path(&resolved)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    Ok(FsReadResult { bytes, mime })
}

/// Write `bytes` to `path`, creating missing parent folders. Mirrors the desktop `fs_write`.
pub async fn write(resolve: Resolve<'_>, path: &str, bytes: &[u8]) -> Result<(), String> {
    let resolved = resolve(path)?;
    if let Some(parent) = resolved.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir failed: {e}"))?;
    }
    tokio::fs::write(&resolved, bytes)
        .await
        .map_err(|e| format!("write failed: {e}"))
}

/// One row of a directory listing: the desktop's `FileEntry` (`commands::fs`), which is
/// camelCase and carries size and mtime, plus `is_dir`, the spelling the daemon's original
/// `fs_list` used. The file picker still reads it, so both keys are sent.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    #[serde(rename = "is_dir")]
    pub is_dir_legacy: bool,
    pub size: u64,
    pub modified_ms: i64,
}

/// List `dir`. Mirrors the desktop `fs_list` (unsorted; the caller sorts), with one difference
/// that only matters to a server. The desktop describes a symlink by its target. Here the target
/// is followed only when `resolve` accepts it, so a link that leaves the allowlist (or points
/// into the daemon's own state) is listed as itself: its name, but not the target's kind, size
/// or mtime. A plain entry, including the data dir's own name, is listed as on the desktop.
pub async fn list(resolve: Resolve<'_>, dir: &str) -> Result<Vec<FsEntry>, String> {
    let resolved = resolve(dir)?;
    let mut rd = tokio::fs::read_dir(&resolved)
        .await
        .map_err(|e| format!("read_dir failed: {e}"))?;
    let mut out = Vec::new();
    while let Some(entry) = rd
        .next_entry()
        .await
        .map_err(|e| format!("next_entry failed: {e}"))?
    {
        let p = entry.path();
        let Ok(link_meta) = tokio::fs::symlink_metadata(&p).await else {
            continue;
        };
        let meta = if link_meta.file_type().is_symlink() {
            match resolve(&p.to_string_lossy()) {
                Ok(real) => tokio::fs::metadata(&real).await.unwrap_or(link_meta),
                Err(_) => link_meta,
            }
        } else {
            link_meta
        };
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let is_dir = meta.is_dir();
        out.push(FsEntry {
            path: p.to_string_lossy().into_owned(),
            name: p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string(),
            is_dir,
            is_dir_legacy: is_dir,
            size: meta.len(),
            modified_ms,
        });
    }
    Ok(out)
}

/// `'file' | 'dir' | 'missing'`. `'missing'` is returned both for not-found
/// and for allowlist-rejected paths so callers can fall back uniformly.
pub async fn kind(resolve: Resolve<'_>, path: &str) -> &'static str {
    let resolved = match resolve(path) {
        Ok(p) => p,
        Err(_) => return "missing",
    };
    match tokio::fs::metadata(&resolved).await {
        Ok(m) if m.is_dir() => "dir",
        Ok(m) if m.is_file() => "file",
        Ok(_) => "missing",
        Err(_) => "missing",
    }
}

/// The desktop `fs_exists` contract: `true` only for a regular file the
/// resolver accepts. A refused path is `false` whether or not it exists, so
/// the answer outside the allowlist (or inside the daemon's own state) is a
/// constant — never an existence oracle — and a caller probing candidate
/// paths (the markdown path linkifier) gets an answer, not a rejection.
pub async fn exists(resolve: Resolve<'_>, path: &str) -> bool {
    let Ok(resolved) = resolve(path) else {
        return false;
    };
    tokio::fs::metadata(&resolved)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Extension-based MIME; the path must be allowlisted but need not exist.
pub fn mime(resolve: Resolve<'_>, path: &str) -> Result<String, String> {
    let resolved = resolve(path)?;
    Ok(mime_guess::from_path(&resolved)
        .first_or_octet_stream()
        .essence_str()
        .to_string())
}

/// Recursive basename search rooted at `root`. Case-insensitive substring
/// match. Honors the same dot-file and ignored-dir rules the JS sorter uses
/// so search results match what the user would see if they manually expanded
/// every folder. Capped at `limit` (default [`DEFAULT_SEARCH_LIMIT`]); when
/// the cap trips, `truncated` is true and the walk stops early.
///
/// The walk never leaves `root`: `DirEntry::file_type` does not follow
/// symlinks, so a symlinked directory is matched by name but not descended.
pub async fn search(
    resolve: Resolve<'_>,
    root: &str,
    query: &str,
    show_hidden: bool,
    show_ignored: bool,
    limit: Option<usize>,
) -> Result<FsSearchResult, String> {
    search_skipping(
        resolve,
        root,
        query,
        show_hidden,
        show_ignored,
        limit,
        |_| false,
    )
    .await
}

/// [`search`], leaving out every entry `skip` names: it is neither matched
/// nor, if a directory, descended. The desktop passes nothing (via
/// [`search`]); the daemon skips its own data dir and discovery file, which
/// every other daemon arm refuses (`server::reserved`).
pub async fn search_skipping(
    resolve: Resolve<'_>,
    root: &str,
    query: &str,
    show_hidden: bool,
    show_ignored: bool,
    limit: Option<usize>,
    skip: impl Fn(&std::fs::DirEntry) -> bool + Send + 'static,
) -> Result<FsSearchResult, String> {
    let resolved = resolve(root)?;
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Ok(FsSearchResult {
            matches: Vec::new(),
            truncated: false,
        });
    }
    let cap = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).max(1);

    tokio::task::spawn_blocking(move || {
        let mut matches: Vec<String> = Vec::new();
        let mut truncated = false;
        let mut stack: Vec<PathBuf> = vec![resolved];

        while let Some(dir) = stack.pop() {
            let rd = match std::fs::read_dir(&dir) {
                Ok(rd) => rd,
                // Permission denied, vanished mid-walk, etc. Skip silently —
                // search shouldn't surface every unreadable corner.
                Err(_) => continue,
            };
            for entry in rd.flatten() {
                if skip(&entry) {
                    continue;
                }
                let name = match entry.file_name().into_string() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                if !show_hidden && name.starts_with('.') {
                    continue;
                }
                let ft = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                let is_dir = ft.is_dir();
                if is_dir && !show_ignored && IGNORED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                if name.to_lowercase().contains(&needle) {
                    matches.push(entry.path().to_string_lossy().to_string());
                    if matches.len() >= cap {
                        truncated = true;
                        return FsSearchResult { matches, truncated };
                    }
                }
                if is_dir {
                    stack.push(entry.path());
                }
            }
        }
        FsSearchResult { matches, truncated }
    })
    .await
    .map_err(|e| format!("search join failed: {e}"))
}

/// Rename `from` to the basename `to_name` — in its own folder, or, when
/// `to_dir` is given, in that folder (a move; plans/file-editing F2). The
/// source, the destination folder and the resolved destination must all be
/// inside the allowlist. The destination must not already exist, and a folder
/// cannot move into itself. Returns the resolved destination.
pub async fn rename(
    resolve: Resolve<'_>,
    from: &str,
    to_name: &str,
    to_dir: Option<&str>,
) -> Result<String, String> {
    if to_name.is_empty() || to_name.contains('/') || to_name.contains('\\') {
        return Err("invalid name".to_string());
    }
    let resolved_from = resolve(from)?;
    let parent = match to_dir {
        Some(dir) => {
            let resolved_dir = resolve(dir)?;
            match tokio::fs::metadata(&resolved_dir).await {
                Ok(m) if m.is_dir() => resolved_dir,
                _ => return Err(format!("not a folder: {}", resolved_dir.display())),
            }
        }
        None => resolved_from
            .parent()
            .ok_or_else(|| "source has no parent".to_string())?
            .to_path_buf(),
    };
    let dest = parent.join(to_name);
    let resolved_dest = resolve(&dest.to_string_lossy())?;
    if resolved_dest != resolved_from && resolved_dest.starts_with(&resolved_from) {
        return Err("cannot move a folder into itself".to_string());
    }
    if tokio::fs::metadata(&resolved_dest).await.is_ok() {
        return Err(format!("destination exists: {}", resolved_dest.display()));
    }
    tokio::fs::rename(&resolved_from, &resolved_dest)
        .await
        .map_err(|e| format!("rename failed: {e}"))?;
    Ok(path_string(&resolved_dest))
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// Metadata preserved alongside a trashed file or folder.
#[derive(Debug, Serialize, Deserialize)]
pub struct TrashMetadata {
    pub original_path: String,
    pub trashed_at_ms: u64,
    pub file_name: String,
    pub is_dir: bool,
}

/// Move `canonical` into `trash_dir`, generating a unique name and sidecar JSON
/// metadata file (`<trashed_name>.meta.json`) recording original path and timestamp.
///
/// The sidecar is written **before** anything moves, so whatever lands in the
/// trash always carries its original path. A plain rename is tried first; only
/// a cross-device rename (`EXDEV`) falls back to copy + remove — any other
/// rename error (`EACCES`, `EINVAL` for a folder moved into its own subtree,
/// `EBUSY`) is reported as is, with nothing copied. The copy refuses anything
/// that is not a regular file, a directory or a symlink (a FIFO would block
/// the copy in `open()` forever), and when removing the original fails
/// partway the full copy and its sidecar stay in the trash and the error says
/// the original may be partly deleted.
///
/// The caller decides *what* may be trashed (the daemon's `fs_trash` refuses
/// allowlist roots, the data dir and its ancestors, the home): this only moves.
pub async fn trash(canonical: &Path, trash_dir: &Path) -> Result<(), String> {
    let canonical = canonical.to_path_buf();
    let trash_dir = trash_dir.to_path_buf();

    tokio::task::spawn_blocking(move || {
        trash_blocking(&canonical, &trash_dir, move_path_with_fallback)
    })
    .await
    .map_err(|e| format!("trash task join failed: {e}"))?
}

/// Why a move into the trash failed, and whether the source was touched.
#[derive(Debug)]
enum MoveError {
    /// Nothing moved: the source is as it was and none of it is in the trash.
    Untouched(String),
    /// The source was copied in full, but removing it failed partway: the full
    /// copy is in the trash, the source may be partly deleted.
    SourcePartlyRemoved(String),
}

/// [`trash`]'s body, with the mover injectable so the copy fallback can be
/// tested without a second filesystem.
fn trash_blocking(
    canonical: &Path,
    trash_dir: &Path,
    mv: impl Fn(&Path, &Path, bool) -> Result<(), MoveError>,
) -> Result<(), String> {
    // Ensure trash directory exists with mode 0700 permissions
    create_secure_trash_dir(trash_dir)?;

    let meta = std::fs::symlink_metadata(canonical)
        .map_err(|e| format!("cannot trash nonexistent path {}: {e}", canonical.display()))?;

    let is_dir = meta.is_dir();
    let orig_name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("item")
        .to_string();

    let id = uuid::Uuid::now_v7();
    let trashed_name = format!("{id}_{orig_name}");
    let dest_path = trash_dir.join(&trashed_name);
    let sidecar_path = trash_dir.join(format!("{trashed_name}.meta.json"));

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let metadata = TrashMetadata {
        original_path: canonical.to_string_lossy().into_owned(),
        trashed_at_ms: now_ms,
        file_name: orig_name,
        is_dir,
    };

    let meta_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|e| format!("serialize trash metadata: {e}"))?;

    // The sidecar first: whatever reaches the trash, even half a move, is
    // recoverable by its original path.
    write_new_file(&sidecar_path, &meta_bytes)
        .map_err(|e| format!("write sidecar metadata: {e}"))?;

    match mv(canonical, &dest_path, is_dir) {
        Ok(()) => Ok(()),
        Err(MoveError::Untouched(e)) => {
            let _ = std::fs::remove_file(&sidecar_path);
            Err(e)
        }
        Err(MoveError::SourcePartlyRemoved(e)) => Err(format!(
            "{e}; a full copy is in the trash as {trashed_name} (with its metadata), \
             but the original at {} may be partly deleted",
            canonical.display()
        )),
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

fn create_secure_trash_dir(trash_dir: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        if let Err(e) = builder.create(trash_dir) {
            if e.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(format!("create trash dir {}: {e}", trash_dir.display()));
            }
        }
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(trash_dir) {
            let mut perms = meta.permissions();
            perms.set_mode(0o700);
            let _ = std::fs::set_permissions(trash_dir, perms);
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(trash_dir)
            .map_err(|e| format!("create trash dir {}: {e}", trash_dir.display()))?;
    }
    Ok(())
}

/// A rename, falling back to copy + remove **only** when the rename failed
/// because source and trash are on different filesystems.
fn move_path_with_fallback(src: &Path, dst: &Path, is_dir: bool) -> Result<(), MoveError> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => copy_then_remove(src, dst, is_dir),
        Err(e) => Err(MoveError::Untouched(format!("move into trash failed: {e}"))),
    }
}

fn is_cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE
        e.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// The cross-device move. A copy that fails is removed, leaving the source
/// untouched; only a directory whose removal fails partway leaves both.
fn copy_then_remove(src: &Path, dst: &Path, is_dir: bool) -> Result<(), MoveError> {
    if dst.starts_with(src) {
        // A copy into its own subtree would re-copy its output until the path
        // or the disk ran out.
        return Err(MoveError::Untouched(format!(
            "cannot move {} into itself",
            src.display()
        )));
    }
    if is_dir {
        if let Err(e) = copy_dir_recursive(src, dst) {
            let _ = std::fs::remove_dir_all(dst);
            return Err(MoveError::Untouched(format!(
                "cross-device copy failed: {e}"
            )));
        }
        std::fs::remove_dir_all(src).map_err(|e| {
            MoveError::SourcePartlyRemoved(format!("remove original after copy failed: {e}"))
        })
    } else {
        if let Err(e) = copy_regular_file(src, dst) {
            let _ = std::fs::remove_file(dst);
            return Err(MoveError::Untouched(format!(
                "cross-device copy failed: {e}"
            )));
        }
        if let Err(e) = std::fs::remove_file(src) {
            // One file: the original is intact, so drop the copy.
            let _ = std::fs::remove_file(dst);
            return Err(MoveError::Untouched(format!(
                "remove original after copy failed: {e}"
            )));
        }
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ft.is_symlink() {
            #[cfg(unix)]
            {
                let link_target = std::fs::read_link(&from)?;
                std::os::unix::fs::symlink(&link_target, &to)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(&from, &to)?;
            }
        } else {
            copy_regular_file(&from, &to)?;
        }
    }
    Ok(())
}

fn not_regular(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "{} is not a regular file (a FIFO, socket or device); refusing to copy it",
            path.display()
        ),
    )
}

/// `std::fs::copy` for a regular file only. `std::fs::copy` opens its source
/// with a blocking `open()`, which on a FIFO waits for a writer forever and
/// pins the blocking thread. This refuses anything `lstat` says is not a
/// regular file, then opens non-blocking without following a symlink and
/// checks the opened descriptor again, so a file swapped for a FIFO between
/// the two cannot block either. Permissions are carried over, as
/// `std::fs::copy` does.
fn copy_regular_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if !std::fs::symlink_metadata(from)?.is_file() {
        return Err(not_regular(from));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut src = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOCTTY)
            .open(from)?;
        let meta = src.metadata()?;
        if !meta.is_file() {
            return Err(not_regular(from));
        }
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(to)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.set_permissions(meta.permissions())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(from, to).map(|_| ())
    }
}

#[cfg(all(test, unix))]
mod trash_tests {
    //! The trash mover on its own: which rename errors may fall back to a
    //! copy, what a partial copy + remove leaves, and that a FIFO never
    //! blocks. Cross-device is forced by calling `copy_then_remove` (the
    //! `EXDEV` branch) directly, so no second filesystem is needed.

    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    fn is_root() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    fn mkfifo(p: &Path) {
        let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Run `f` on a thread and fail (instead of hanging the suite) if it
    /// does not return within 10s.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the trash mover blocked (a FIFO opened for reading?)")
    }

    /// Regression: a folder renamed into its own subtree fails with EINVAL.
    /// That is not a cross-device move, so it must not fall back to a copy
    /// (which recursed into its own output until ENAMETOOLONG / ENOSPC).
    #[test]
    fn a_non_cross_device_rename_error_never_falls_back_to_a_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("tree");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/f.txt"), b"f").unwrap();
        let dst = src.join("sub/into-itself");

        let r = bounded({
            let (src, dst) = (src.clone(), dst.clone());
            move || move_path_with_fallback(&src, &dst, true)
        });
        assert!(matches!(r, Err(MoveError::Untouched(_))), "{r:?}");
        assert!(!dst.exists(), "nothing may be copied into the source");
        assert_eq!(names(&src.join("sub")), ["f.txt"]);

        // And the copy fallback itself refuses a destination inside its source.
        let r = copy_then_remove(&src, &dst, true);
        assert!(matches!(r, Err(MoveError::Untouched(_))), "{r:?}");
        assert!(!dst.exists());
    }

    /// Regression: EACCES on the rename used to fall back to copy + remove,
    /// leaving a copy in the trash with no sidecar. Now nothing is left.
    #[test]
    fn a_permission_denied_rename_leaves_nothing_in_the_trash() {
        if is_root() {
            return; // root ignores the 0555 that makes the rename fail
        }
        let tmp = tempfile::tempdir().unwrap();
        let rodir = tmp.path().join("rodir");
        let trash = tmp.path().join("trash");
        std::fs::create_dir(&rodir).unwrap();
        let f = rodir.join("f.txt");
        std::fs::write(&f, b"keep").unwrap();
        std::fs::set_permissions(&rodir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let r = trash_blocking(&f, &trash, move_path_with_fallback);
        std::fs::set_permissions(&rodir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let e = r.unwrap_err();
        assert!(e.contains("Permission denied"), "{e}");
        assert_eq!(std::fs::read(&f).unwrap(), b"keep");
        assert!(names(&trash).is_empty(), "trash: {:?}", names(&trash));
    }

    /// Regression: when removing the original fails partway, the sidecar must
    /// already be in the trash next to the full copy, and the error must say
    /// the original may be partly deleted.
    #[test]
    fn a_partial_remove_keeps_the_full_copy_and_its_sidecar() {
        if is_root() {
            return; // root can delete inside a 0555 directory
        }
        let tmp = tempfile::tempdir().unwrap();
        let xp = tmp.path().join("xp");
        let trash = tmp.path().join("trash");
        std::fs::create_dir_all(xp.join("locked")).unwrap();
        std::fs::write(xp.join("a.txt"), b"a").unwrap();
        std::fs::write(xp.join("z.txt"), b"z").unwrap();
        std::fs::write(xp.join("locked/b.txt"), b"b").unwrap();
        let locked = xp.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Force the cross-device branch.
        let r = trash_blocking(&xp, &trash, copy_then_remove);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        let e = r.unwrap_err();
        assert!(e.contains("may be partly deleted"), "{e}");
        let listed = names(&trash);
        let item = listed
            .iter()
            .find(|n| n.ends_with("_xp"))
            .unwrap_or_else(|| panic!("copy missing: {listed:?}"));
        let sidecar = listed
            .iter()
            .find(|n| n.ends_with("_xp.meta.json"))
            .unwrap_or_else(|| panic!("sidecar missing: {listed:?}"));
        let copy = trash.join(item);
        assert_eq!(std::fs::read(copy.join("a.txt")).unwrap(), b"a");
        assert_eq!(std::fs::read(copy.join("z.txt")).unwrap(), b"z");
        assert_eq!(std::fs::read(copy.join("locked/b.txt")).unwrap(), b"b");
        let meta: TrashMetadata =
            serde_json::from_slice(&std::fs::read(trash.join(sidecar)).unwrap()).unwrap();
        assert_eq!(meta.original_path, xp.to_string_lossy());
        assert!(meta.is_dir);
        assert!(meta.trashed_at_ms > 0);
    }

    /// A move that fails cleanly removes the sidecar it wrote first.
    #[test]
    fn a_clean_failure_removes_the_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let trash = tmp.path().join("trash");
        let f = tmp.path().join("f.txt");
        std::fs::write(&f, b"f").unwrap();
        let r = trash_blocking(&f, &trash, |_, _, _| {
            Err(MoveError::Untouched("nope".into()))
        });
        assert_eq!(r.unwrap_err(), "nope");
        assert!(names(&trash).is_empty(), "{:?}", names(&trash));
        assert!(f.exists());
    }

    /// Regression: the copy fallback opened a FIFO for reading and blocked
    /// in `open()` forever, pinning a blocking thread and stopping the daemon
    /// from shutting down. It must refuse it at once and leave the source.
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("xfifo");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("k.txt"), b"k").unwrap();
        mkfifo(&dir.join("pipe"));
        let dst = tmp.path().join("trash-copy");

        let r = bounded({
            let (dir, dst) = (dir.clone(), dst.clone());
            move || copy_then_remove(&dir, &dst, true)
        });
        match r {
            Err(MoveError::Untouched(e)) => assert!(e.contains("not a regular file"), "{e}"),
            other => panic!("{other:?}"),
        }
        assert!(!dst.exists(), "a failed copy is cleaned up");
        assert_eq!(names(&dir), ["k.txt", "pipe"]);

        // A FIFO trashed on its own, across devices.
        let lone = tmp.path().join("lone-pipe");
        mkfifo(&lone);
        let dst = tmp.path().join("lone-copy");
        let r = bounded({
            let (lone, dst) = (lone.clone(), dst.clone());
            move || copy_then_remove(&lone, &dst, false)
        });
        assert!(matches!(r, Err(MoveError::Untouched(_))), "{r:?}");
        assert!(!dst.exists());
        assert!(std::fs::symlink_metadata(&lone).is_ok());
    }

    /// The copy fallback carries a regular file's bytes and mode across.
    #[test]
    fn the_copy_fallback_moves_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("t");
        std::fs::create_dir_all(src.join("d")).unwrap();
        std::fs::write(src.join("d/x.sh"), b"#!/bin/sh").unwrap();
        std::fs::set_permissions(src.join("d/x.sh"), std::fs::Permissions::from_mode(0o750))
            .unwrap();
        std::os::unix::fs::symlink("d/x.sh", src.join("ln")).unwrap();
        let dst = tmp.path().join("moved");
        copy_then_remove(&src, &dst, true).unwrap();
        assert!(!src.exists());
        assert_eq!(std::fs::read(dst.join("d/x.sh")).unwrap(), b"#!/bin/sh");
        let mode = std::fs::metadata(dst.join("d/x.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
        assert_eq!(
            std::fs::read_link(dst.join("ln")).unwrap(),
            Path::new("d/x.sh")
        );
    }
}
