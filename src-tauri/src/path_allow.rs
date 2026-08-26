//! Path resolution against the user's filesystem allowlist.
//!
//! Lives outside `commands/` because the headless daemon needs it and
//! `commands/` is desktop-only — every module in there is built around
//! `#[tauri::command]` and an `AppHandle`, which do not exist in a build
//! without the webview runtime. This function needs neither: it is pure path
//! math plus a read of the process-global root set.
//!
//! `commands` re-exports it, so the ~300 existing call sites are unchanged.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Resolve `~/...` and env vars, then enforce the user-configurable allowlist
/// (see `crate::fs_roots`). Returns the canonical absolute path.
///
/// The active root set lives in a process-global `OnceLock` set by
/// `lib.rs::run` during `.setup()` (and by `server::run_server` in the
/// daemon), so this function does not need to thread `tauri::State` through
/// every fs command + the viewer.
pub fn resolve_allowlisted(input: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(input)
        .map(|c| c.into_owned())
        .map_err(|e| anyhow!("shellexpand failed: {e}"))?;
    let path = PathBuf::from(&expanded);
    let abs = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    check_allowlisted(&abs)
}

/// Enforce the allowlist on a path the *operating system* handed us — a
/// `notify` event path, say — rather than one the user typed.
///
/// Deliberately does no shell expansion. `resolve_allowlisted` starts from
/// user input, so `~` and `$VAR` in it are meant to expand; a path that came
/// back from the filesystem is already literal, and a file genuinely named
/// `$HOME` would expand into a check about an entirely different file.
///
/// This exists because `resolve_allowlisted` canonicalizes a watch ROOT
/// exactly once, at watch time, and nothing re-checked the individual paths a
/// watcher then reported. A symlinked directory planted inside a watched root
/// can make the kernel report paths whose canonical form sits outside every
/// allowed root. On the desktop that only ever reached the same user who owns
/// the files; over a network socket it leaks filenames and write timing to
/// whoever holds the token. `fs_watch` calls this on every event path.
pub fn check_allowlisted(path: &Path) -> Result<PathBuf> {
    let canonical = canonical_for_check(path)?;
    if !is_allowed(&canonical)? {
        return Err(anyhow!("path outside allowlist: {}", canonical.display()));
    }
    Ok(canonical)
}

/// The canonicalisation half of [`check_allowlisted`], split out so it can be
/// tested without touching the process-global root set (a `OnceLock`, so a
/// test that installed one would fix it for the whole test binary).
///
/// `canonicalize` requires the path to exist. For writes to new files — and
/// for the `remove` events a watcher reports after the file is already gone —
/// we canonicalize the parent and re-attach the filename so the allowlist
/// check still works. Note that this resolves symlinks: that is the entire
/// point, since a symlink is how an in-root path reaches out-of-root content.
fn canonical_for_check(abs: &Path) -> Result<PathBuf> {
    if abs.exists() {
        return Ok(abs.canonicalize()?);
    }
    let Some(parent) = abs.parent() else {
        return Err(anyhow!("path has no parent: {}", abs.display()));
    };
    if !parent.exists() {
        return Err(anyhow!("path does not exist: {}", abs.display()));
    }
    let parent_canon = parent.canonicalize()?;
    Ok(match abs.file_name() {
        Some(name) => parent_canon.join(name),
        None => parent_canon,
    })
}

fn is_allowed(path: &Path) -> Result<bool> {
    let roots = crate::fs_roots::current().ok_or_else(|| anyhow!("fs_roots not initialized"))?;
    Ok(roots.is_allowed(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_roots::FsRoots;

    /// The composition that closes the watcher leak: canonicalising an event
    /// path resolves a symlink planted inside the watched root, and the
    /// resulting path then fails `is_allowed`.
    ///
    /// Driven against a locally-built `FsRoots` rather than `check_allowlisted`
    /// itself because the active root set is a process-global `OnceLock` —
    /// installing one here would pin it for every other test in the binary.
    #[cfg(unix)]
    #[test]
    fn canonicalising_an_event_path_defeats_an_in_root_symlink() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("watched");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("mine.txt"), b"ok").unwrap();
        std::fs::write(outside.join("secret.txt"), b"no").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

        let roots = FsRoots::load(tmp.path().join("fs_roots.json")).unwrap();
        roots.add(&root.to_string_lossy()).unwrap();

        // In-root file: canonical form stays under the root and is allowed.
        let mine = canonical_for_check(&root.join("mine.txt")).unwrap();
        assert!(roots.is_allowed(&mine), "in-root file must stay allowed");

        // Reached through the symlink: canonical form lands outside.
        let leaked = canonical_for_check(&root.join("escape/secret.txt")).unwrap();
        assert!(
            leaked.starts_with(outside.canonicalize().unwrap()),
            "canonicalize must resolve the symlink, got {}",
            leaked.display()
        );
        assert!(
            !roots.is_allowed(&leaked),
            "out-of-root target must be refused, got {}",
            leaked.display()
        );
    }

    /// A `remove` event names a path that no longer exists; the parent
    /// fallback has to keep working or every deletion would be dropped.
    #[test]
    fn a_deleted_path_still_canonicalises_via_its_parent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let gone = tmp.path().join("gone.txt");
        let canon = canonical_for_check(&gone).unwrap();
        assert_eq!(canon.file_name().unwrap(), "gone.txt");
        assert!(canon.starts_with(tmp.path().canonicalize().unwrap()));
    }

    #[test]
    fn a_path_whose_parent_is_missing_is_an_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("no-such-dir/child.txt");
        assert!(canonical_for_check(&nested).is_err());
    }
}
