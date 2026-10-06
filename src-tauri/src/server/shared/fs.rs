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

use serde::Serialize;

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

/// Rename `from` to a sibling with the new basename. Both the source and the
/// resolved destination must be inside the allowlist. The destination must
/// not already exist. Returns the resolved destination.
pub async fn rename(resolve: Resolve<'_>, from: &str, to_name: &str) -> Result<String, String> {
    if to_name.is_empty() || to_name.contains('/') || to_name.contains('\\') {
        return Err("invalid name".to_string());
    }
    let resolved_from = resolve(from)?;
    let parent = resolved_from
        .parent()
        .ok_or_else(|| "source has no parent".to_string())?;
    let dest = parent.join(to_name);
    let resolved_dest = resolve(&dest.to_string_lossy())?;
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
