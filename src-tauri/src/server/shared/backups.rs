//! Local `.ikbak` backup listing, deletion, export, import, and NDJSON
//! migration tools shared between desktop and daemon (WP-19 / WP-B).

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Subdirectory of the local data dir that holds `.ikbak` bundles.
pub const BACKUPS_DIR: &str = "backups";
pub const EXPORTS_DIR: &str = "exports";
pub const BACKUP_FORMAT_VERSION: u32 = 3;
pub const BACKUP_SCHEMA_VERSION: i64 = 7;
pub const MAX_BACKUP_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100 MB
pub const STAGED_DIR: &str = "staged-restore";
pub const STAGED_DB_NAME: &str = "ikenga.db.new";
pub const LIVE_DB_NAME: &str = "ikenga.db";
pub const HOME_TOKEN: &str = "${IKENGA_HOME}";

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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathWarning {
    pub table: String,
    pub column: String,
    pub value: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgEntry {
    pub id: String,
    pub version: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub format_version: u32,
    pub schema_version: i64,
    pub created_at: String,
    pub hostname: String,
    pub username: String,
    pub path_mode: PathMode,
    pub home_dir: Option<String>,
    pub has_secrets: bool,
    pub pkg_count: u32,
    #[serde(default)]
    pub path_warnings: Vec<PathWarning>,
}

impl BackupManifest {
    pub fn new(
        path_mode: PathMode,
        home_dir: Option<String>,
        has_secrets: bool,
        pkg_count: u32,
        path_warnings: Vec<PathWarning>,
    ) -> Self {
        Self {
            format_version: BACKUP_FORMAT_VERSION,
            schema_version: BACKUP_SCHEMA_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            hostname: std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("HOST"))
                .unwrap_or_else(|_| "unknown".into()),
            username: std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
            path_mode,
            home_dir,
            has_secrets,
            pkg_count,
            path_warnings,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSummary {
    pub path: String,
    pub created_at: String,
    pub size_bytes: u64,
    pub schema_version: i64,
    pub has_secrets: bool,
    pub pkg_count: u32,
    pub path_mode: PathMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub path: String,
    pub size_bytes: u64,
    pub secrets_count: u32,
    pub pkg_count: u32,
    pub path_warnings_count: u32,
    #[serde(alias = "bytesBase64", skip_serializing_if = "Option::is_none")]
    pub bytes_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SchemaAction {
    Match,
    Forward { from: i64, to: i64 },
    NewerThanApp { backup: i64, app: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPreview {
    pub manifest: BackupManifest,
    pub size_bytes: u64,
    pub schema_action: SchemaAction,
    pub pkgs: Vec<PkgEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub staged_at: String,
    pub requires_restart: bool,
    pub secrets_staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NdjsonTableSummary {
    pub table: String,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NdjsonExportResult {
    pub dir: String,
    pub table_count: usize,
    pub row_count: u64,
    pub tables: Vec<NdjsonTableSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NdjsonLoadResult {
    pub table_count: usize,
    pub row_count: u64,
}

pub struct PathColumn {
    pub table: &'static str,
    pub column: &'static str,
    pub where_clause: Option<&'static str>,
}

pub const PATH_COLUMNS: &[PathColumn] = &[
    PathColumn {
        table: "viewer_recents",
        column: "path",
        where_clause: None,
    },
    PathColumn {
        table: "render_jobs",
        column: "output_path",
        where_clause: None,
    },
    PathColumn {
        table: "storyboards",
        column: "r1_still_path",
        where_clause: None,
    },
    PathColumn {
        table: "storyboards",
        column: "r2_still_path",
        where_clause: None,
    },
    PathColumn {
        table: "pkg_installed",
        column: "install_path",
        where_clause: None,
    },
    PathColumn {
        table: "pkg_permissions_granted",
        column: "scope_value",
        where_clause: Some("scope_kind IN ('fs.read', 'fs.write')"),
    },
];

pub fn read_manifest(zip_path: &Path) -> Result<BackupManifest, String> {
    let bytes = read_zip_bytes_limited(zip_path, "manifest.json", 1024 * 1024)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse manifest: {e}"))
}

pub fn read_zip_bytes(zip_path: &Path, name: &str) -> Result<Vec<u8>, String> {
    read_zip_bytes_limited(zip_path, name, MAX_BACKUP_SIZE_BYTES)
}

pub fn read_zip_bytes_limited(zip_path: &Path, name: &str, limit: u64) -> Result<Vec<u8>, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    let mut entry = archive.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut buf = Vec::new();
    let mut limited = (&mut entry).take(limit + 1);
    let n = limited
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {name}: {e}"))?;
    if n as u64 > limit {
        return Err(format!(
            "zip entry `{name}` decompressed size exceeds maximum allowed ({limit} bytes)"
        ));
    }
    Ok(buf)
}

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

// ─── SQLite helpers ──────────────────────────────────────────────────────────

pub async fn vacuum_into(src: &Path, dest: &Path) -> Result<(), String> {
    let url = format!("sqlite://{}?mode=ro", src.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open source db: {e}"))?;
    let stmt = format!(
        "VACUUM INTO '{}'",
        dest.display().to_string().replace('\'', "''")
    );
    sqlx::query(&stmt)
        .execute(&pool)
        .await
        .map_err(|e| format!("VACUUM INTO failed: {e}"))?;
    pool.close().await;
    Ok(())
}

pub async fn read_installed_pkgs(db_path: &Path) -> Result<Vec<PkgEntry>, String> {
    let url = format!("sqlite://{}?mode=ro", db_path.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open snapshot: {e}"))?;
    let rows: Result<Vec<(String, String, i64)>, _> =
        sqlx::query_as("SELECT id, version, enabled FROM pkg_installed ORDER BY id")
            .fetch_all(&pool)
            .await;
    pool.close().await;
    match rows {
        Ok(rows) => Ok(rows
            .into_iter()
            .map(|(id, version, enabled)| PkgEntry {
                id,
                version,
                enabled: enabled != 0,
            })
            .collect()),
        Err(e) => {
            tracing::warn!("pkg_installed query failed: {e}");
            Ok(Vec::new())
        }
    }
}

pub fn read_pkgs_from_zip(zip_path: &Path) -> Result<Vec<PkgEntry>, String> {
    match read_zip_bytes_limited(zip_path, "installed-pkgs.json", 10 * 1024 * 1024) {
        Ok(bytes) => {
            serde_json::from_slice::<Vec<PkgEntry>>(&bytes).map_err(|e| format!("parse pkgs: {e}"))
        }
        Err(_) => Ok(Vec::new()),
    }
}

pub fn strip_home_prefix<'a>(value: &'a str, home: &str) -> Option<&'a str> {
    let bytes = value.as_bytes();
    let hb = home.as_bytes();
    if bytes.len() < hb.len() || &bytes[..hb.len()] != hb {
        return None;
    }
    match bytes.get(hb.len()) {
        None => Some(""),
        Some(&b'/') => Some(&value[hb.len()..]),
        _ => None,
    }
}

pub fn current_home_dir() -> Option<String> {
    crate::platform::home_dir().map(|p| p.to_string_lossy().into_owned())
}

pub async fn tokenize_snapshot_paths(
    snapshot: &Path,
    home: &str,
) -> Result<Vec<PathWarning>, String> {
    let home = home.trim_end_matches('/').to_string();
    let url = format!("sqlite://{}?mode=rwc", snapshot.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open snapshot for tokenize: {e}"))?;

    let mut warnings = Vec::new();
    for col in PATH_COLUMNS {
        let where_filter = col.where_clause.unwrap_or("1=1");
        let select_sql = format!(
            "SELECT rowid, \"{c}\" FROM \"{t}\" \
             WHERE \"{c}\" IS NOT NULL AND \"{c}\" <> '' \
             AND substr(\"{c}\", 1, {n}) != ? AND ({w})",
            c = col.column,
            t = col.table,
            n = HOME_TOKEN.len(),
            w = where_filter,
        );
        let rows: Result<Vec<(i64, String)>, _> = sqlx::query_as(&select_sql)
            .bind(HOME_TOKEN)
            .fetch_all(&pool)
            .await;
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                let m = e.to_string();
                if m.contains("no such table") {
                    continue;
                }
                pool.close().await;
                return Err(format!("tokenize select {}.{}: {e}", col.table, col.column));
            }
        };

        for (rowid, value) in rows {
            if let Some(rest) = strip_home_prefix(&value, &home) {
                let new_value = format!("{HOME_TOKEN}{rest}");
                let upd_sql = format!(
                    "UPDATE \"{t}\" SET \"{c}\" = ? WHERE rowid = ?",
                    t = col.table,
                    c = col.column,
                );
                sqlx::query(&upd_sql)
                    .bind(&new_value)
                    .bind(rowid)
                    .execute(&pool)
                    .await
                    .map_err(|e| {
                        format!(
                            "tokenize update {}.{} rowid={rowid}: {e}",
                            col.table, col.column
                        )
                    })?;
            } else {
                warnings.push(PathWarning {
                    table: col.table.into(),
                    column: col.column.into(),
                    value,
                    reason: "outside $HOME".into(),
                });
            }
        }
    }
    pool.close().await;
    Ok(warnings)
}

pub async fn apply_path_rewrites_to_db(
    db_path: &Path,
    from_token: &str,
    to_home: &str,
) -> Result<usize, String> {
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open db for path rewrite: {e}"))?;
    let mut total: usize = 0;
    for col in PATH_COLUMNS {
        let where_filter = col.where_clause.unwrap_or("1=1");
        let upd_sql = format!(
            "UPDATE \"{t}\" SET \"{c}\" = ? || substr(\"{c}\", {n}) \
             WHERE substr(\"{c}\", 1, {n}-1) = ? AND ({w})",
            t = col.table,
            c = col.column,
            n = from_token.len() + 1,
            w = where_filter,
        );
        match sqlx::query(&upd_sql)
            .bind(to_home)
            .bind(from_token)
            .execute(&pool)
            .await
        {
            Ok(r) => total += r.rows_affected() as usize,
            Err(e) => {
                let m = e.to_string();
                if !m.contains("no such table") {
                    tracing::warn!("path rewrite {}.{}: {e}", col.table, col.column);
                }
            }
        }
    }
    pool.close().await;
    Ok(total)
}

// ─── Export & Import Archive ─────────────────────────────────────────────────

pub async fn export_backup_zip(
    data_dir: &Path,
    dest: &Path,
    path_mode: PathMode,
    return_bytes: bool,
) -> Result<ExportResult, String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir dest parent: {e}"))?;
    }
    let db_path = data_dir.join(LIVE_DB_NAME);
    let legacy_db_path = data_dir.join("pa.db");
    let active_db = if db_path.exists() {
        db_path
    } else if legacy_db_path.exists() {
        legacy_db_path
    } else {
        return Err(format!("no database found at {}", db_path.display()));
    };

    let active_db_size = fs::metadata(&active_db).map(|m| m.len()).unwrap_or(0);
    if active_db_size > MAX_BACKUP_SIZE_BYTES {
        return Err(format!(
            "database size ({active_db_size} bytes) exceeds maximum backup limit ({MAX_BACKUP_SIZE_BYTES} bytes)"
        ));
    }

    let tmp_id = uuid::Uuid::new_v4().simple();
    let snapshot = data_dir.join(format!("{LIVE_DB_NAME}.export-{tmp_id}.tmp"));
    struct TmpFileGuard(PathBuf);
    impl Drop for TmpFileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let _snapshot_guard = TmpFileGuard(snapshot.clone());

    vacuum_into(&active_db, &snapshot).await?;

    let pkgs = read_installed_pkgs(&snapshot).await.unwrap_or_default();
    let home_dir_str = current_home_dir();
    let (export_home, warnings) = match path_mode {
        PathMode::Raw => (None, Vec::new()),
        PathMode::Tokenized => {
            let home = home_dir_str.clone().ok_or("tokenized mode requires HOME")?;
            let warnings = tokenize_snapshot_paths(&snapshot, &home).await?;
            (Some(home), warnings)
        }
        PathMode::Bundled => {
            return Err("bundled path mode is not implemented".into());
        }
    };

    let warnings_count = warnings.len() as u32;
    let manifest = BackupManifest::new(path_mode, export_home, false, pkgs.len() as u32, warnings);
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;
    let pkgs_json = serde_json::to_vec_pretty(&pkgs).map_err(|e| format!("serialize pkgs: {e}"))?;
    let db_bytes = fs::read(&snapshot).map_err(|e| format!("read snapshot: {e}"))?;

    let file = fs::File::create(dest).map_err(|e| format!("create zip: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts_deflate = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let opts_stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("manifest.json", opts_deflate)
        .map_err(|e| e.to_string())?;
    zip.write_all(&manifest_json).map_err(|e| e.to_string())?;

    zip.start_file("app.db", opts_stored)
        .map_err(|e| e.to_string())?;
    zip.write_all(&db_bytes).map_err(|e| e.to_string())?;

    zip.start_file("installed-pkgs.json", opts_deflate)
        .map_err(|e| e.to_string())?;
    zip.write_all(&pkgs_json).map_err(|e| e.to_string())?;

    zip.finish().map_err(|e| format!("finish zip: {e}"))?;

    let size_bytes = fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
    let bytes_base64 = if return_bytes {
        let zip_bytes = fs::read(dest).map_err(|e| format!("read created zip: {e}"))?;
        use base64::Engine as _;
        Some(base64::engine::general_purpose::STANDARD.encode(&zip_bytes))
    } else {
        None
    };

    Ok(ExportResult {
        path: dest.to_string_lossy().into_owned(),
        size_bytes,
        secrets_count: 0,
        pkg_count: pkgs.len() as u32,
        path_warnings_count: warnings_count,
        bytes_base64,
    })
}

pub fn validate_backup_zip(
    zip_path: &Path,
) -> Result<(BackupManifest, SchemaAction, Vec<PkgEntry>), String> {
    let metadata = fs::metadata(zip_path).map_err(|e| format!("metadata: {e}"))?;
    if metadata.len() > MAX_BACKUP_SIZE_BYTES {
        return Err(format!(
            "backup file size ({} bytes) exceeds maximum allowed ({} bytes)",
            metadata.len(),
            MAX_BACKUP_SIZE_BYTES
        ));
    }

    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;

    let mut total_uncompressed: u64 = 0;
    let allowed_entries = [
        "manifest.json",
        "app.db",
        "pa.db",
        "ikenga.db",
        "installed-pkgs.json",
        "secrets.age",
    ];
    let mut seen_entries = std::collections::HashSet::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("zip entry {i}: {e}"))?;
        let name = entry.name().to_string();

        // 1. Duplicate entry check
        if !seen_entries.insert(name.clone()) {
            return Err(format!("refusing duplicate zip entry `{name}`"));
        }

        // 2. Zip Slip check
        if name.contains("..") || name.starts_with('/') || name.starts_with('\\') {
            return Err(format!("refusing malicious zip entry path `{name}`"));
        }

        // 3. Allowlist of valid backup entry names
        if !allowed_entries.contains(&name.as_str()) {
            return Err(format!(
                "refusing unknown or unauthorized zip entry `{name}`"
            ));
        }

        // 4. Zip bomb check - declared size
        let declared_size = entry.size();
        if declared_size > MAX_BACKUP_SIZE_BYTES {
            return Err(format!(
                "zip entry `{name}` uncompressed size ({declared_size}) exceeds maximum"
            ));
        }

        // 5. Zip bomb check - stream with take(MAX+1) to verify actual decompressed bytes
        let mut limited = (&mut entry).take(MAX_BACKUP_SIZE_BYTES + 1);
        let actual_size = std::io::copy(&mut limited, &mut std::io::sink())
            .map_err(|e| format!("verify entry `{name}`: {e}"))?;
        if actual_size > MAX_BACKUP_SIZE_BYTES {
            return Err(format!(
                "zip entry `{name}` decompressed size exceeds maximum ({MAX_BACKUP_SIZE_BYTES} bytes)"
            ));
        }

        total_uncompressed += actual_size;
        if total_uncompressed > 2 * MAX_BACKUP_SIZE_BYTES {
            return Err("total uncompressed backup size exceeds maximum allowed".to_string());
        }
    }

    let manifest = read_manifest(zip_path)?;
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(format!(
            "unsupported backup format_version {} (supported up to {})",
            manifest.format_version, BACKUP_FORMAT_VERSION
        ));
    }

    let schema_action = match manifest.schema_version.cmp(&BACKUP_SCHEMA_VERSION) {
        std::cmp::Ordering::Equal => SchemaAction::Match,
        std::cmp::Ordering::Less => SchemaAction::Forward {
            from: manifest.schema_version,
            to: BACKUP_SCHEMA_VERSION,
        },
        std::cmp::Ordering::Greater => SchemaAction::NewerThanApp {
            backup: manifest.schema_version,
            app: BACKUP_SCHEMA_VERSION,
        },
    };

    let pkgs = read_pkgs_from_zip(zip_path).unwrap_or_default();
    Ok((manifest, schema_action, pkgs))
}

pub fn extract_app_db(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;

    let db_name = if archive.by_name("app.db").is_ok() {
        "app.db"
    } else if archive.by_name("pa.db").is_ok() {
        "pa.db"
    } else if archive.by_name("ikenga.db").is_ok() {
        "ikenga.db"
    } else {
        return Err("archive missing db member (app.db / pa.db / ikenga.db)".to_string());
    };

    let mut entry = archive
        .by_name(db_name)
        .map_err(|e| format!("{db_name}: {e}"))?;
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    let mut out = fs::File::create(dest).map_err(|e| format!("create staged db: {e}"))?;
    let mut limited = (&mut entry).take(MAX_BACKUP_SIZE_BYTES + 1);
    let copied =
        std::io::copy(&mut limited, &mut out).map_err(|e| format!("write staged db: {e}"))?;
    if copied > MAX_BACKUP_SIZE_BYTES {
        let _ = fs::remove_file(dest);
        return Err(format!(
            "zip entry `{db_name}` decompressed size exceeds maximum allowed ({MAX_BACKUP_SIZE_BYTES} bytes)"
        ));
    }
    Ok(())
}

async fn validate_staged_schema(
    pool: &sqlx::SqlitePool,
    _schema_version: i64,
) -> Result<(), String> {
    // 1. Check _pa_migrations table exists
    let migrations_table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='_pa_migrations'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("query sqlite_master: {e}"))?;

    if migrations_table_exists.is_none() {
        return Err("staged database is missing required `_pa_migrations` table".to_string());
    }

    // Check column 'id' in _pa_migrations
    let col_rows: Vec<sqlx::sqlite::SqliteRow> = sqlx::query("PRAGMA table_info(_pa_migrations)")
        .fetch_all(pool)
        .await
        .map_err(|e| format!("PRAGMA table_info(_pa_migrations): {e}"))?;
    let has_id_col = col_rows.iter().any(|r| {
        use sqlx::Row;
        r.get::<String, _>("name") == "id"
    });
    if !has_id_col {
        return Err("staged database `_pa_migrations` table is missing `id` column".to_string());
    }

    // 2. Check required core table layout_state (persistent across all schema versions)
    let layout_table_exists: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='layout_state'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("query sqlite_master: {e}"))?;

    if layout_table_exists.is_none() {
        return Err("staged database is missing required core table `layout_state`".to_string());
    }

    Ok(())
}

pub async fn import_backup_zip(
    data_dir: &Path,
    db: &crate::db::PaDb,
    zip_path: &Path,
    dry_run: bool,
) -> Result<serde_json::Value, String> {
    static RESTORE_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _lock = RESTORE_MUTEX.lock().await;

    let (manifest, schema_action, pkgs) = validate_backup_zip(zip_path)?;
    let size_bytes = fs::metadata(zip_path).map(|m| m.len()).unwrap_or(0);

    if dry_run {
        let preview = ImportPreview {
            manifest,
            size_bytes,
            schema_action,
            pkgs,
        };
        return Ok(serde_json::to_value(preview).map_err(|e| e.to_string())?);
    }

    if let SchemaAction::NewerThanApp { backup, app } = schema_action {
        return Err(format!(
            "backup schema {backup} is newer than running app schema {app} — upgrade the app first"
        ));
    }

    let run_id = uuid::Uuid::new_v4().simple();
    let staged_dir = data_dir.join(format!("{STAGED_DIR}-{run_id}"));
    fs::create_dir_all(&staged_dir).map_err(|e| format!("mkdir staged: {e}"))?;
    let staged_db = staged_dir.join(STAGED_DB_NAME);

    struct StagedDirGuard(PathBuf);
    impl Drop for StagedDirGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _staged_guard = StagedDirGuard(staged_dir.clone());

    extract_app_db(zip_path, &staged_db)?;

    // 1. Verify SQLite integrity & schema of staged db
    {
        let url = format!("sqlite://{}?mode=ro", staged_db.display());
        let test_pool = sqlx::SqlitePool::connect(&url)
            .await
            .map_err(|e| format!("open staged db: {e}"))?;
        let check: Result<String, _> = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&test_pool)
            .await;
        match check {
            Ok(ref s) if s == "ok" => {}
            Ok(other) => {
                test_pool.close().await;
                return Err(format!("staged db integrity check failed: {other}"));
            }
            Err(e) => {
                test_pool.close().await;
                return Err(format!("staged db probe failed: {e}"));
            }
        }

        // Schema validation (Blocker 3)
        if let Err(e) = validate_staged_schema(&test_pool, manifest.schema_version).await {
            test_pool.close().await;
            return Err(e);
        }
        test_pool.close().await;
    }

    // 2. Run migrations on staged db copy first (Blocker 2)
    {
        let staged_pa_db = crate::db::PaDb::new(staged_db.clone());
        let _staged_writer = staged_pa_db
            .ensure_pool()
            .await
            .map_err(|e| format!("run migrations on staged db failed: {e}"))?;
        staged_pa_db.close().await;
    }

    // 3. Atomic swap with rollback (Blocker 2)
    let live_db = data_dir.join(LIVE_DB_NAME);
    let prev_db = data_dir.join(format!("{LIVE_DB_NAME}.prev"));

    // Close live pool
    db.close().await;

    // Remove old .prev files if present
    let _ = fs::remove_file(&prev_db);
    for ext in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(data_dir.join(format!("{LIVE_DB_NAME}.prev{ext}")));
    }

    // Move live_db -> prev_db
    if live_db.exists() {
        fs::rename(&live_db, &prev_db)
            .map_err(|e| format!("backup live db to .prev failed: {e}"))?;
    }
    for ext in ["-wal", "-shm", "-journal"] {
        let aux = data_dir.join(format!("{LIVE_DB_NAME}{ext}"));
        let prev_aux = data_dir.join(format!("{LIVE_DB_NAME}.prev{ext}"));
        if aux.exists() {
            let _ = fs::rename(&aux, &prev_aux);
        }
    }

    // Swap staged -> live
    if let Err(e) = fs::rename(&staged_db, &live_db) {
        // Roll back immediately
        if prev_db.exists() {
            let _ = fs::rename(&prev_db, &live_db);
            for ext in ["-wal", "-shm", "-journal"] {
                let prev_aux = data_dir.join(format!("{LIVE_DB_NAME}.prev{ext}"));
                let aux = data_dir.join(format!("{LIVE_DB_NAME}{ext}"));
                if prev_aux.exists() {
                    let _ = fs::rename(&prev_aux, &aux);
                }
            }
        }
        let _ = db.ensure_pool().await;
        return Err(format!("swap db: {e}"));
    }

    // Post-swap: path rewrites and ensure live pool
    let post_swap_res = async {
        if matches!(manifest.path_mode, PathMode::Tokenized) {
            if let Some(home) = current_home_dir() {
                apply_path_rewrites_to_db(&live_db, HOME_TOKEN, &home).await?;
            }
        }
        db.ensure_pool()
            .await
            .map_err(|e| format!("reopen live pool: {e}"))?;
        Ok::<(), String>(())
    }
    .await;

    if let Err(e) = post_swap_res {
        // Roll back on any error after the swap!
        db.close().await;
        let _ = fs::remove_file(&live_db);
        for ext in ["-wal", "-shm", "-journal"] {
            let _ = fs::remove_file(data_dir.join(format!("{LIVE_DB_NAME}{ext}")));
        }
        if prev_db.exists() {
            let _ = fs::rename(&prev_db, &live_db);
            for ext in ["-wal", "-shm", "-journal"] {
                let prev_aux = data_dir.join(format!("{LIVE_DB_NAME}.prev{ext}"));
                let aux = data_dir.join(format!("{LIVE_DB_NAME}{ext}"));
                if prev_aux.exists() {
                    let _ = fs::rename(&prev_aux, &aux);
                }
            }
        }
        let _ = db.ensure_pool().await;
        return Err(format!(
            "restore failed post-swap, rolled back to previous database: {e}"
        ));
    }

    let res = ImportResult {
        staged_at: live_db.to_string_lossy().into_owned(),
        requires_restart: false,
        secrets_staged: false,
    };
    Ok(serde_json::to_value(res).map_err(|e| e.to_string())?)
}

// ─── NDJSON export and import ────────────────────────────────────────────────

fn sqlite_col_to_json(row: &sqlx::sqlite::SqliteRow, i: usize) -> Result<Value, String> {
    use sqlx::{Row, TypeInfo, ValueRef};
    let raw = row
        .try_get_raw(i)
        .map_err(|e| format!("get_raw col {i}: {e}"))?;
    if raw.is_null() {
        return Ok(Value::Null);
    }
    Ok(match raw.type_info().name() {
        "INTEGER" | "INT" | "BIGINT" => row
            .try_get::<i64, _>(i)
            .ok()
            .map(Value::from)
            .unwrap_or(Value::Null),
        "REAL" | "FLOAT" | "DOUBLE" => row
            .try_get::<f64, _>(i)
            .ok()
            .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
            .unwrap_or(Value::Null),
        "BOOLEAN" => row
            .try_get::<bool, _>(i)
            .ok()
            .map(Value::Bool)
            .unwrap_or(Value::Null),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(i)
            .ok()
            .map(|b| Value::Array(b.into_iter().map(Value::from).collect()))
            .unwrap_or(Value::Null),
        _ => row
            .try_get::<String, _>(i)
            .ok()
            .map(Value::String)
            .unwrap_or(Value::Null),
    })
}

fn bind_ndjson_params<'q>(
    mut q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    params: &'q [Value],
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    for p in params {
        q = match p {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else if let Some(f) = n.as_f64() {
                    q.bind(f)
                } else {
                    q.bind(n.to_string())
                }
            }
            Value::String(s) => q.bind(s.clone()),
            Value::Array(arr) => {
                let bytes: Vec<u8> = arr
                    .iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect();
                q.bind(bytes)
            }
            other => q.bind(other.to_string()),
        };
    }
    q
}

struct ColMeta {
    name: String,
    pk: i64,
}

pub async fn export_ndjson_to_dir(
    pool: &sqlx::SqlitePool,
    dest: &Path,
) -> Result<NdjsonExportResult, String> {
    use sqlx::Row;

    fs::create_dir_all(dest).map_err(|e| format!("mkdir dest_dir: {e}"))?;

    let table_names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite_%' \
           AND name <> '_pa_migrations' \
         ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list tables: {e}"))?;

    let mut tables_out = Vec::new();
    let mut total_rows: u64 = 0;

    for table in &table_names {
        let col_rows: Vec<sqlx::sqlite::SqliteRow> =
            sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
                .fetch_all(pool)
                .await
                .map_err(|e| format!("PRAGMA table_info({table}): {e}"))?;

        let cols: Vec<ColMeta> = col_rows
            .iter()
            .map(|r| ColMeta {
                name: r.get("name"),
                pk: r.get::<i64, _>("pk"),
            })
            .collect();

        let mut pk_cols: Vec<&ColMeta> = cols.iter().filter(|c| c.pk > 0).collect();
        pk_cols.sort_by_key(|c| c.pk);
        let order_by = if pk_cols.is_empty() {
            "rowid".to_string()
        } else {
            pk_cols
                .iter()
                .map(|c| format!("\"{}\"", c.name))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let col_select = cols
            .iter()
            .map(|c| format!("\"{}\"", c.name))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT {col_select} FROM \"{table}\" ORDER BY {order_by}");

        let rows: Vec<sqlx::sqlite::SqliteRow> = sqlx::query(&sql)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("query {table}: {e}"))?;

        let row_count = rows.len() as u64;
        let mut ndjson = String::new();

        for row in &rows {
            let mut obj = Map::new();
            for (i, col) in cols.iter().enumerate() {
                let val = sqlite_col_to_json(row, i)?;
                obj.insert(col.name.clone(), val);
            }
            let line = serde_json::to_string(&Value::Object(obj))
                .map_err(|e| format!("serialize row in {table}: {e}"))?;
            ndjson.push_str(&line);
            ndjson.push('\n');
        }

        let out_path = dest.join(format!("{table}.ndjson"));
        fs::write(&out_path, ndjson.as_bytes())
            .map_err(|e| format!("write {}: {e}", out_path.display()))?;

        tracing::info!(
            "[db_export_ndjson] {}: {} rows → {}",
            table,
            row_count,
            out_path.display()
        );
        total_rows += row_count;
        tables_out.push(NdjsonTableSummary {
            table: table.clone(),
            rows: row_count,
        });
    }

    Ok(NdjsonExportResult {
        dir: dest.to_string_lossy().into_owned(),
        table_count: table_names.len(),
        row_count: total_rows,
        tables: tables_out,
    })
}

pub async fn import_ndjson_from_dir(
    pool: &sqlx::SqlitePool,
    src: &Path,
) -> Result<NdjsonLoadResult, String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(src)
        .map_err(|e| format!("read_dir {}: {e}", src.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ndjson"))
        .collect();
    entries.sort();

    // Query valid user tables from sqlite_master
    let valid_tables: std::collections::HashSet<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite_%' \
           AND name <> '_pa_migrations' \
         ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list tables: {e}"))?
    .into_iter()
    .collect();

    let mut total_rows: u64 = 0;
    let table_count = entries.len();

    for path in &entries {
        let table = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad ndjson filename: {}", path.display()))?
            .to_string();

        // 1. Validate table name: must be ASCII alphanumeric/underscore and exist in valid_tables
        if !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || !valid_tables.contains(&table)
        {
            return Err(format!(
                "refusing to import into unrecognized or unauthorized table `{table}` from file {}",
                path.display()
            ));
        }

        // 2. Validate columns for this table from pragma_table_info
        let col_rows: Vec<sqlx::sqlite::SqliteRow> =
            sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
                .fetch_all(pool)
                .await
                .map_err(|e| format!("PRAGMA table_info({table}): {e}"))?;
        let valid_cols: std::collections::HashSet<String> = col_rows
            .into_iter()
            .map(|r| sqlx::Row::get::<String, _>(&r, "name"))
            .collect();

        let content =
            fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;

        let mut rows_loaded: u64 = 0;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let obj: serde_json::Map<String, Value> =
                serde_json::from_str(line).map_err(|e| format!("parse line in {table}: {e}"))?;
            if obj.is_empty() {
                continue;
            }

            let cols: Vec<&String> = obj.keys().collect();
            for c in &cols {
                if !valid_cols.contains(*c) {
                    return Err(format!(
                        "refusing insert: column `{c}` does not exist in table `{table}`"
                    ));
                }
            }

            let col_list = cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let placeholders = cols.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            let sql =
                format!("INSERT OR REPLACE INTO \"{table}\" ({col_list}) VALUES ({placeholders})");

            let params: Vec<Value> = obj.values().cloned().collect();
            let q = bind_ndjson_params(sqlx::query(&sql), &params);
            q.execute(pool)
                .await
                .map_err(|e| format!("insert into {table}: {e}"))?;
            rows_loaded += 1;
        }

        total_rows += rows_loaded;
        tracing::info!("[db_import_ndjson] {table}: {rows_loaded} rows loaded");
    }

    Ok(NdjsonLoadResult {
        table_count,
        row_count: total_rows,
    })
}
