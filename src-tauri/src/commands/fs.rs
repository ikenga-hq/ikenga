//! Filesystem commands. All path inputs go through `resolve_allowlisted` —
//! reads/writes/lists outside `~/royalti-co`, `~/.claude/projects`, and
//! `~/.company` are rejected.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::resolve_allowlisted;
use crate::fs_watch::FsWatchManager;
use crate::server::shared::fs as shared_fs;

pub use crate::server::shared::fs::FsSearchResult;

/// The desktop's resolver for the shared fs cores: `resolve_allowlisted`
/// against the process-global root set, error text unchanged.
fn allowlisted(path: &str) -> Result<std::path::PathBuf, String> {
    resolve_allowlisted(path).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct FileReadResult {
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
    pub size: u64,
    #[serde(rename = "modifiedMs")]
    pub modified_ms: i64,
}

#[tauri::command]
pub async fn fs_read(path: String) -> Result<FileReadResult, String> {
    let resolved = resolve_allowlisted(&path).map_err(|e| e.to_string())?;
    let bytes = tokio::fs::read(&resolved)
        .await
        .map_err(|e| format!("read failed: {e}"))?;
    let mime = mime_guess::from_path(&resolved)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    Ok(FileReadResult { bytes, mime })
}

/// Cheap existence check. Returns true if the path resolves to a regular file
/// (not a directory). Used by the markdown FilePathPill click handler to walk
/// monorepo subprojects when a relative path doesn't resolve against the
/// thread's cwd. Allowlisted same as the other fs commands.
#[tauri::command]
pub async fn fs_exists(path: String) -> Result<bool, String> {
    let resolved = match resolve_allowlisted(&path) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    Ok(tokio::fs::metadata(&resolved)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false))
}

/// Cheap kind discriminator. Returns `'file' | 'dir' | 'missing'` for an
/// allowlisted path. Used by the unified artifact-studio route resolver to
/// pick density (folder → grid, file → loupe) without doing a parent
/// `read_dir`. `'missing'` is returned both for not-found and for
/// allowlist-rejected paths so callers can fall back uniformly.
#[tauri::command]
pub async fn fs_kind(path: String) -> Result<&'static str, String> {
    Ok(shared_fs::kind(&allowlisted, &path).await)
}

/// Cheap MIME lookup that doesn't read file contents. Used by the artifact
/// viewer's auto-router as a fallback when the JS-side extension table doesn't
/// recognize a file. The path must be allowlisted but does not need to exist —
/// `mime_guess` only inspects the extension.
#[tauri::command]
pub async fn fs_mime(path: String) -> Result<String, String> {
    shared_fs::mime(&allowlisted, &path)
}

/// Recursive mkdir. Idempotent: succeeds if `path` already exists as a
/// directory. Used by the artifact wizard so the file-watcher's target
/// folder is guaranteed to exist before the first event lands. The
/// allowlist check walks up to the nearest existing ancestor — creating
/// a deep sub-folder under an already-allowed root is fine.
#[tauri::command]
pub async fn fs_mkdir(path: String) -> Result<(), String> {
    let resolved = resolve_allowlisted(&path).map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&resolved)
        .await
        .map_err(|e| format!("mkdir failed: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn fs_write(path: String, bytes: Vec<u8>) -> Result<(), String> {
    let resolved = resolve_allowlisted(&path).map_err(|e| e.to_string())?;
    if let Some(parent) = resolved.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("mkdir failed: {e}"))?;
    }
    tokio::fs::write(&resolved, &bytes)
        .await
        .map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn fs_list(dir: String, glob: Option<String>) -> Result<Vec<FileEntry>, String> {
    let resolved = resolve_allowlisted(&dir).map_err(|e| e.to_string())?;

    // Glob mode: anchor pattern under the resolved dir.
    if let Some(pattern) = glob {
        let pattern_str = format!("{}/{}", resolved.display(), pattern);
        let entries = ::glob::glob(&pattern_str)
            .map_err(|e| format!("invalid glob: {e}"))?
            .filter_map(|res| res.ok())
            .filter_map(|p| entry_for(&p).ok())
            .collect();
        return Ok(entries);
    }

    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(&resolved)
        .await
        .map_err(|e| format!("read_dir failed: {e}"))?;
    while let Some(entry) = rd
        .next_entry()
        .await
        .map_err(|e| format!("next_entry failed: {e}"))?
    {
        let p = entry.path();
        if let Ok(fe) = entry_for(&p) {
            out.push(fe);
        }
    }
    Ok(out)
}

fn entry_for(p: &std::path::Path) -> Result<FileEntry, std::io::Error> {
    let meta = std::fs::metadata(p)?;
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    Ok(FileEntry {
        path: p.to_string_lossy().to_string(),
        name,
        is_dir: meta.is_dir(),
        size: meta.len(),
        modified_ms,
    })
}

/// Recursive basename search rooted at `root`. Case-insensitive substring
/// match. Honors the same dot-file and ignored-dir rules the JS sorter uses
/// so search results match what the user would see if they manually expanded
/// every folder. Capped at `limit` (default 500); when the cap trips,
/// `truncated` is true and the walk stops early.
#[tauri::command]
pub async fn fs_search(
    root: String,
    query: String,
    #[allow(non_snake_case)] showHidden: bool,
    #[allow(non_snake_case)] showIgnored: bool,
    limit: Option<usize>,
) -> Result<FsSearchResult, String> {
    shared_fs::search(&allowlisted, &root, &query, showHidden, showIgnored, limit).await
}

/// Move `path` to the OS trash. Reversible — file can be restored from the
/// system trash UI. Allowlisted same as the other fs commands.
#[tauri::command]
pub async fn fs_trash(path: String) -> Result<(), String> {
    let resolved = resolve_allowlisted(&path).map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || trash::delete(&resolved))
        .await
        .map_err(|e| format!("trash join failed: {e}"))?
        .map_err(|e| format!("trash failed: {e}"))?;
    Ok(())
}

/// Rename `from` to the basename `to_name` — in its own folder, or in `to_dir`
/// when given (a move). Every end must be inside the allowlist and the
/// destination must not already exist.
#[tauri::command]
pub async fn fs_rename(
    from: String,
    to_name: String,
    to_dir: Option<String>,
) -> Result<String, String> {
    shared_fs::rename(&allowlisted, &from, &to_name, to_dir.as_deref()).await
}

#[tauri::command]
pub async fn fs_watch(
    app: AppHandle,
    manager: State<'_, Arc<FsWatchManager>>,
    path: String,
) -> Result<String, String> {
    let resolved = resolve_allowlisted(&path).map_err(|e| e.to_string())?;
    manager
        .watch(app, &resolved)
        .map_err(|e| format!("watch failed: {e}"))
}

#[tauri::command]
pub async fn fs_unwatch(
    manager: State<'_, Arc<FsWatchManager>>,
    #[allow(non_snake_case)] watcherId: String,
) -> Result<(), String> {
    manager
        .unwatch(&watcherId)
        .map_err(|e| format!("unwatch failed: {e}"))
}
