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

    // `canonicalize` requires the path to exist. For writes to new files we
    // canonicalize the parent and re-attach the filename so the allowlist
    // check still works.
    let canonical = if abs.exists() {
        abs.canonicalize()?
    } else if let Some(parent) = abs.parent() {
        if parent.exists() {
            let parent_canon = parent.canonicalize()?;
            match abs.file_name() {
                Some(name) => parent_canon.join(name),
                None => parent_canon,
            }
        } else {
            return Err(anyhow!("path does not exist: {}", abs.display()));
        }
    } else {
        return Err(anyhow!("path has no parent: {}", abs.display()));
    };

    if !is_allowed(&canonical)? {
        return Err(anyhow!("path outside allowlist: {}", canonical.display()));
    }
    Ok(canonical)
}

fn is_allowed(path: &Path) -> Result<bool> {
    let roots = crate::fs_roots::current().ok_or_else(|| anyhow!("fs_roots not initialized"))?;
    Ok(roots.is_allowed(path))
}
