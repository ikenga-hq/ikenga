//! Local `.ikbak` backup listing and deletion, plus the bundle-manifest types
//! both need (WP-19).
//!
//! Only list + delete are shared. They work inside ONE fixed directory — the
//! desktop's `app_local_data_dir/backups`, the daemon's `<--data-dir>/backups`
//! — and delete refuses anything that does not canonicalize inside it. Export,
//! import and the NDJSON dump/load stay in `commands/backup.rs`, desktop-only:
//! they take caller-supplied free-form paths with no containment, and restore
//! only ever applies in desktop `.setup()` followed by a relaunch.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Subdirectory of the local data dir that holds `.ikbak` bundles.
pub const BACKUPS_DIR: &str = "backups";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    Raw,
    Tokenized,
    Bundled,
}

impl Default for PathMode {
    fn default() -> Self {
        PathMode::Raw
    }
}

/// Recorded for any path that couldn't be tokenized (lives outside `$HOME`).
/// The UI surfaces these so users know the restore target may not see those
/// files. We don't fail the export over them — the user already chose
/// tokenized knowing same-machine recovery still works.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathWarning {
    pub table: String,
    pub column: String,
    pub value: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub schema_version: i64,
    pub created_at: String,
    pub hostname: String,
    pub username: String,
    pub path_mode: PathMode,
    pub home_dir: Option<String>, // export-time $HOME, present iff path_mode == tokenized
    pub has_secrets: bool,
    pub pkg_count: u32,
    #[serde(default)]
    pub path_warnings: Vec<PathWarning>,
}

#[derive(Debug, Serialize)]
pub struct BackupSummary {
    pub path: String,
    pub created_at: String,
    pub size_bytes: u64,
    pub schema_version: i64,
    pub has_secrets: bool,
    pub pkg_count: u32,
    pub path_mode: PathMode,
}

pub fn read_manifest(zip_path: &Path) -> Result<BackupManifest, String> {
    let bytes = read_zip_bytes(zip_path, "manifest.json")?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse manifest: {e}"))
}

pub fn read_zip_bytes(zip_path: &Path, name: &str) -> Result<Vec<u8>, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    let mut entry = archive.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {name}: {e}"))?;
    Ok(buf)
}

/// Every `.ikbak` in `dir`, newest first. A missing dir is an empty list (no
/// backups yet); a bundle whose manifest can't be read is still listed, with
/// zeroed metadata, so the user can see and delete it.
pub fn list(dir: &Path) -> Result<Vec<BackupSummary>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("read backups dir: {e}"))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ikbak") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        match read_manifest(&path) {
            Ok(m) => out.push(BackupSummary {
                path: path.to_string_lossy().into_owned(),
                created_at: m.created_at,
                size_bytes: size,
                schema_version: m.schema_version,
                has_secrets: m.has_secrets,
                pkg_count: m.pkg_count,
                path_mode: m.path_mode,
            }),
            Err(_) => out.push(BackupSummary {
                path: path.to_string_lossy().into_owned(),
                created_at: String::new(),
                size_bytes: size,
                schema_version: 0,
                has_secrets: false,
                pkg_count: 0,
                path_mode: PathMode::Raw,
            }),
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// Delete `path`, but only if it canonicalizes to somewhere inside `dir`.
/// Canonicalizing both sides resolves `..` and symlinks before the
/// `starts_with` check, so neither can smuggle the delete out of the dir.
pub fn delete(dir: &Path, path: &str) -> Result<(), String> {
    let target = Path::new(path);
    let canon_target = fs::canonicalize(target).map_err(|e| format!("canon target: {e}"))?;
    let canon_allowed = fs::canonicalize(dir).map_err(|e| format!("canon allowed: {e}"))?;
    if !canon_target.starts_with(&canon_allowed) {
        return Err(format!(
            "refusing to delete file outside backups dir: {}",
            target.display()
        ));
    }
    fs::remove_file(&canon_target).map_err(|e| format!("delete: {e}"))
}
