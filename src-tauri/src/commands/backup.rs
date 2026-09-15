//! Backup / restore.
//!
//! Phase 1 — SQLite snapshot (`pa.db` via `VACUUM INTO`) + `manifest.json`.
//! Phase 2 — adds:
//!   - `secrets.age`         age-passphrase-encrypted JSON of vault kv pairs
//!   - `installed-pkgs.json` informational list of installed pkgs (the actual
//!                            row data already rides along inside pa.db)
//! Phase 3 — adds path_mode picker:
//!   - `raw`        absolute paths preserved (default — same-machine recovery)
//!   - `tokenized`  rewrite `$HOME/...` → `${IKENGA_HOME}/...` in 8 explicit
//!                  path columns; reversed at boot against the new user's
//!                  $HOME. Paths outside $HOME are left raw and recorded in
//!                  `path_warnings` for the UI to surface.
//!   - `bundled`    not yet implemented (returns an error if requested).
//!
//! Pkg restore stays list-only (the user re-installs from registry by hand).
//!
//! ## Restore is stage-and-swap-on-boot, not live
//!
//! `backup_import` validates the bundle and writes:
//!
//! ```text
//! <app_data_dir>/staged-restore/pa.db.new
//! <app_data_dir>/staged-restore/secrets-pending.json   (only if has_secrets and passphrase ok)
//! <app_data_dir>/staged-restore/RESTORE_PENDING        (last write — atomic-ish marker)
//! ```
//!
//! `lib.rs::setup()` calls `apply_staged_restore_if_present` BEFORE any pool
//! opens. SQLite is swapped in-place; secrets-pending.json is left for the
//! Stronghold-backed apply that runs once setup wires `SecretsLock` (the
//! lazy-open inside `bulk_set` handles initialization). On restart-after-
//! restart the file is gone, so the apply is single-shot.
//!
//! ## Why decrypt at import-time, write plaintext to staged-restore
//!
//! Two options for moving secrets across the boot boundary:
//!   (a) carry the encrypted blob through, decrypt on next boot — but we'd
//!       need to persist the passphrase, and persisting passphrases is
//!       exactly what we were trying to avoid by using age in the first
//!       place.
//!   (b) decrypt during `backup_import`, drop a chmod-0600 plaintext JSON
//!       in the user's app-data dir, apply on next boot. Window of exposure
//!       is one app launch; file is on the user's own machine.
//!
//! We pick (b). The marker is the last write so a crash mid-import never
//! leaves a half-applied state.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tauri::{AppHandle, Manager, State};

use crate::commands::db::PaDb;
use crate::commands::secrets::{self, SecretsLock};

const BACKUP_FORMAT_VERSION: u32 = 3;
const BACKUP_SCHEMA_VERSION: i64 = 7;

const MARKER_NAME: &str = "RESTORE_PENDING";
const STAGED_DIR: &str = "staged-restore";
const STAGED_DB_NAME: &str = "ikenga.db.new";
const STAGED_SECRETS_NAME: &str = "secrets-pending.json";
const STAGED_PATH_REWRITE_NAME: &str = "path-rewrite.json";
const DB_NAME: &str = "ikenga.db";

/// `${IKENGA_HOME}` is the export-time placeholder for the originating
/// machine's `$HOME`. The tokenized export rewrites `$HOME/...` paths to
/// this prefix; the boot-time reverse-apply rewrites it back to whatever
/// `$HOME` resolves to on the restoring machine.
const HOME_TOKEN: &str = "${IKENGA_HOME}";

/// Path-bearing columns that get rewritten in tokenized mode. The optional
/// `where_clause` filters rows (used for `pkg_permissions_granted` where
/// only fs.* scopes are paths). Tables that don't exist on older snapshots
/// are tolerated — the UPDATE just no-ops.
struct PathColumn {
    table: &'static str,
    column: &'static str,
    where_clause: Option<&'static str>,
}

const PATH_COLUMNS: &[PathColumn] = &[
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

// ─── manifest + result types ──────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PkgEntry {
    pub id: String,
    pub version: String,
    pub enabled: bool,
}

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

impl BackupManifest {
    fn new(
        path_mode: PathMode,
        home_dir: Option<String>,
        has_secrets: bool,
        pkg_count: u32,
        path_warnings: Vec<PathWarning>,
    ) -> Self {
        Self {
            format_version: BACKUP_FORMAT_VERSION,
            schema_version: BACKUP_SCHEMA_VERSION,
            created_at: now_iso(),
            hostname: hostname_or_unknown(),
            username: std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
            path_mode,
            home_dir,
            has_secrets,
            pkg_count,
            path_warnings,
        }
    }
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

#[derive(Debug, Serialize)]
pub struct ExportResult {
    pub path: String,
    pub size_bytes: u64,
    pub secrets_count: u32,
    pub pkg_count: u32,
    pub path_warnings_count: u32,
}

#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub manifest: BackupManifest,
    pub size_bytes: u64,
    pub schema_action: SchemaAction,
    pub pkgs: Vec<PkgEntry>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SchemaAction {
    Match,
    Forward { from: i64, to: i64 },
    NewerThanApp { backup: i64, app: i64 },
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub staged_at: String,
    pub requires_restart: bool,
    pub secrets_staged: bool,
}

// ─── public commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn backup_export<R: tauri::Runtime>(
    app: AppHandle<R>,
    lock: State<'_, SecretsLock>,
    dest_path: String,
    include_secrets: bool,
    passphrase: Option<String>,
    path_mode: Option<PathMode>,
) -> Result<ExportResult, String> {
    let path_mode = path_mode.unwrap_or_default();
    if path_mode == PathMode::Bundled {
        return Err(
            "bundled path mode is not yet implemented (phase 4). Use raw or tokenized.".into(),
        );
    }
    if include_secrets && passphrase.as_deref().unwrap_or("").is_empty() {
        return Err("a passphrase is required to include secrets".into());
    }

    let dest = PathBuf::from(&dest_path);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir dest parent: {e}"))?;
    }
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    let db_path = data_dir.join(DB_NAME);
    if !db_path.exists() {
        return Err(format!("no database at {}", db_path.display()));
    }

    // SQLite snapshot via VACUUM INTO — the only safe way to copy a live db.
    let snapshot = data_dir.join(format!("{DB_NAME}.export.tmp"));
    if snapshot.exists() {
        let _ = fs::remove_file(&snapshot);
    }
    vacuum_into(&db_path, &snapshot).await?;

    // Read installed pkgs from the freshly snapshotted db so the json matches
    // exactly what's inside app.db (no torn read against the live pool).
    let pkgs = read_installed_pkgs(&snapshot).await.unwrap_or_else(|e| {
        log::warn!("read_installed_pkgs failed (continuing): {e}");
        Vec::new()
    });

    // Tokenize paths in the snapshot, in place. Done after pkg-list read so
    // the json reflects original install_path values (the rewritten ones go
    // into the bundled pa.db). Warnings are appended to the manifest.
    let home_dir_str = current_home_dir();
    let (export_home, warnings) = match path_mode {
        PathMode::Raw => (None, Vec::new()),
        PathMode::Tokenized => {
            let home = home_dir_str
                .clone()
                .ok_or("tokenized path mode requires $HOME to be set")?;
            let warnings = tokenize_snapshot_paths(&snapshot, &home).await?;
            (Some(home), warnings)
        }
        PathMode::Bundled => unreachable!(),
    };

    // Optional secrets: enumerate, JSON-encode, age-encrypt with passphrase.
    let mut secrets_blob: Option<Vec<u8>> = None;
    let mut secrets_count: u32 = 0;
    if include_secrets {
        let kvs = secrets::dump_all_kvs(&app, &lock)?;
        secrets_count = kvs.len() as u32;
        let plaintext = serde_json::to_vec(&serde_json::json!({ "kvs": kvs }))
            .map_err(|e| format!("serialize secrets: {e}"))?;
        let pp = passphrase.clone().unwrap_or_default();
        secrets_blob = Some(age_encrypt(&plaintext, &pp)?);
    }

    let warnings_count = warnings.len() as u32;
    let manifest = BackupManifest::new(
        path_mode,
        export_home,
        secrets_blob.is_some(),
        pkgs.len() as u32,
        warnings,
    );
    let manifest_json =
        serde_json::to_vec_pretty(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;
    let pkgs_json = serde_json::to_vec_pretty(&pkgs).map_err(|e| format!("serialize pkgs: {e}"))?;

    // Build the zip.
    let file = fs::File::create(&dest).map_err(|e| format!("create dest: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts_deflate: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let opts_stored: zip::write::SimpleFileOptions =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("manifest.json", opts_deflate)
        .map_err(|e| format!("zip start manifest: {e}"))?;
    zip.write_all(&manifest_json)
        .map_err(|e| format!("zip write manifest: {e}"))?;

    zip.start_file("app.db", opts_stored)
        .map_err(|e| format!("zip start app.db: {e}"))?;
    let db_bytes = fs::read(&snapshot).map_err(|e| format!("read snapshot: {e}"))?;
    zip.write_all(&db_bytes)
        .map_err(|e| format!("zip write app.db: {e}"))?;

    zip.start_file("installed-pkgs.json", opts_deflate)
        .map_err(|e| format!("zip start pkgs: {e}"))?;
    zip.write_all(&pkgs_json)
        .map_err(|e| format!("zip write pkgs: {e}"))?;

    if let Some(blob) = secrets_blob.as_ref() {
        zip.start_file("secrets.age", opts_stored)
            .map_err(|e| format!("zip start secrets: {e}"))?;
        zip.write_all(blob)
            .map_err(|e| format!("zip write secrets: {e}"))?;
    }

    zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    let _ = fs::remove_file(&snapshot);

    let size = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    Ok(ExportResult {
        path: dest.to_string_lossy().into_owned(),
        size_bytes: size,
        secrets_count,
        pkg_count: pkgs.len() as u32,
        path_warnings_count: warnings_count,
    })
}

#[tauri::command]
pub async fn backup_import<R: tauri::Runtime>(
    app: AppHandle<R>,
    src_path: String,
    dry_run: bool,
    passphrase: Option<String>,
) -> Result<serde_json::Value, String> {
    let src = PathBuf::from(&src_path);
    if !src.exists() {
        return Err(format!("no such file: {}", src.display()));
    }
    let size = fs::metadata(&src).map(|m| m.len()).unwrap_or(0);

    let manifest = read_manifest(&src)?;
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(format!(
            "unsupported backup format_version {} (this app supports up to {})",
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

    let pkgs = read_pkgs_from_zip(&src).unwrap_or_default();

    if dry_run {
        let preview = ImportPreview {
            manifest,
            size_bytes: size,
            schema_action,
            pkgs,
        };
        return Ok(serde_json::to_value(preview).unwrap());
    }

    if let SchemaAction::NewerThanApp { backup, app } = &schema_action {
        return Err(format!(
            "backup schema {} is newer than running app schema {} — upgrade the app first",
            backup, app
        ));
    }

    // Stage pa.db.
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    let staged_dir = data_dir.join(STAGED_DIR);
    fs::create_dir_all(&staged_dir).map_err(|e| format!("mkdir staged: {e}"))?;
    let staged_db = staged_dir.join(STAGED_DB_NAME);
    extract_app_db(&src, &staged_db)?;

    // Stage path-rewrite plan if the bundle is tokenized. We can't apply it
    // pre-pool (requires async sqlx) so leave a tiny json next to pa.db.new
    // and let `apply_staged_path_rewrites` finish the job after the pool is
    // up. If we can't resolve the current $HOME we refuse the restore — a
    // tokenized bundle is unusable without a target home.
    if matches!(manifest.path_mode, PathMode::Tokenized) {
        let target_home = current_home_dir().ok_or(
            "this bundle is tokenized but $HOME is not set on the running app — \
             cannot determine where to restore paths to",
        )?;
        let rewrite = serde_json::json!({
            "from_token": HOME_TOKEN,
            "to_home": target_home,
        });
        fs::write(
            staged_dir.join(STAGED_PATH_REWRITE_NAME),
            serde_json::to_vec_pretty(&rewrite).unwrap(),
        )
        .map_err(|e| format!("write path-rewrite plan: {e}"))?;
    }

    // Stage secrets if present + passphrase ok.
    let mut secrets_staged = false;
    if manifest.has_secrets {
        let pp = passphrase.unwrap_or_default();
        if pp.is_empty() {
            // Don't fail the whole restore — let the user re-run with a
            // passphrase if they want secrets back.
            log::warn!("backup has secrets but no passphrase provided — skipping");
        } else {
            let cipher = read_zip_bytes(&src, "secrets.age")
                .map_err(|e| format!("read secrets.age: {e}"))?;
            let plaintext = age_decrypt(&cipher, &pp)
                .map_err(|e| format!("decrypt secrets (wrong passphrase?): {e}"))?;
            // Validate JSON shape before writing to disk.
            let _: serde_json::Value = serde_json::from_slice(&plaintext)
                .map_err(|e| format!("parse decrypted secrets: {e}"))?;
            let staged_secrets = staged_dir.join(STAGED_SECRETS_NAME);
            write_chmod_600(&staged_secrets, &plaintext)?;
            secrets_staged = true;
        }
    }

    // Marker is the last write — boot keys off its presence.
    let marker = staged_dir.join(MARKER_NAME);
    fs::write(&marker, marker_payload(&manifest, &src)?)
        .map_err(|e| format!("write marker: {e}"))?;

    Ok(serde_json::to_value(ImportResult {
        staged_at: staged_db.to_string_lossy().into_owned(),
        requires_restart: true,
        secrets_staged,
    })
    .unwrap())
}

#[tauri::command]
pub async fn backup_list<R: tauri::Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<BackupSummary>, String> {
    let dir = local_backups_dir(&app)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| format!("read backups dir: {e}"))? {
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

#[tauri::command]
pub async fn backup_delete<R: tauri::Runtime>(
    app: AppHandle<R>,
    path: String,
) -> Result<(), String> {
    let target = PathBuf::from(&path);
    let allowed = local_backups_dir(&app)?;
    let canon_target = fs::canonicalize(&target).map_err(|e| format!("canon target: {e}"))?;
    let canon_allowed = fs::canonicalize(&allowed).map_err(|e| format!("canon allowed: {e}"))?;
    if !canon_target.starts_with(&canon_allowed) {
        return Err(format!(
            "refusing to delete file outside backups dir: {}",
            target.display()
        ));
    }
    fs::remove_file(&canon_target).map_err(|e| format!("delete: {e}"))
}

// ─── one-time db-file rename (pa.db → ikenga.db) ─────────────────────────────

/// Legacy local-store filenames, retired in favor of `ikenga.db` /
/// `ikenga-terminal.sqlite`. Renamed once on boot if the new name is absent
/// but the legacy file exists. See [`migrate_legacy_db_names`].
const LEGACY_DB_NAME: &str = "pa.db";
const LEGACY_TERMINAL_DB_NAME: &str = "pa.sqlite";
const TERMINAL_DB_NAME: &str = "ikenga-terminal.sqlite";

/// WAL-safe, idempotent, atomic-on-failure rename of a single SQLite store
/// from `legacy_name` to `new_name` inside `data_dir`.
///
/// A naïve `fs::rename` of just the main file would drop any frames still
/// sitting in the `-wal` sidecar, so we first open the legacy file and run
/// `PRAGMA wal_checkpoint(TRUNCATE)` to fold the WAL back into the main db and
/// empty the sidecar, then drop the handle before touching the filesystem.
///
/// Safety contract:
/// * **Idempotent** — no-op if `new_name` already exists or `legacy_name` is
///   absent (so it runs harmlessly on every boot after the first).
/// * **Atomic-on-failure** — if the checkpoint or the main-file rename fails,
///   the legacy file is left exactly where it was and a warning is logged; the
///   caller then opens the store under its old name as a fallback so a botched
///   migration can never brick the store. (We only rename the `-wal`/`-shm`
///   siblings *after* the main rename has already succeeded, so a partial
///   sidecar rename never orphans a live db.)
/// Returns the filename the store should actually be opened under after the
/// attempt: `new_name` if the migration succeeded (or had already happened),
/// or `legacy_name` if the legacy file is still present because the migration
/// was skipped or failed. The caller threads this into the pool path so a
/// botched migration opens the (intact) legacy file rather than creating a
/// fresh empty db under the new name.
async fn migrate_one_db_name<'a>(
    data_dir: &Path,
    legacy_name: &'a str,
    new_name: &'a str,
) -> &'a str {
    let new_path = data_dir.join(new_name);
    let legacy_path = data_dir.join(legacy_name);

    // Idempotent: nothing to do if already migrated or there's no legacy file.
    // If the new file is present, open the new name. Otherwise (no legacy file
    // either) this is a fresh install — also open the new name.
    if new_path.exists() {
        return new_name;
    }
    if !legacy_path.exists() {
        return new_name;
    }

    // Step 1 — fold the WAL into the main file, then close the handle. We open
    // with create_if_missing(false) so we never resurrect a deleted db, and in
    // WAL mode so the checkpoint applies. Any failure here leaves the legacy
    // file untouched (we abort before renaming).
    {
        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&legacy_path)
            .create_if_missing(false)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = match sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    "[migrate] open legacy {legacy_name} for checkpoint failed ({e}); \
                     leaving in place, will open under old name"
                );
                return legacy_name;
            }
        };
        if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await
        {
            tracing::warn!(
                "[migrate] wal_checkpoint on {legacy_name} failed ({e}); \
                 leaving in place, will open under old name"
            );
            pool.close().await;
            return legacy_name;
        }
        // Drop the handle so the OS releases the file lock before we rename.
        pool.close().await;
    }

    // Step 2 — rename the main file. If this fails, the legacy file is still
    // intact and the caller falls back to the old name.
    if let Err(e) = fs::rename(&legacy_path, &new_path) {
        tracing::warn!(
            "[migrate] rename {legacy_name} → {new_name} failed ({e}); \
             leaving in place, will open under old name"
        );
        return legacy_name;
    }

    // Step 3 — rename any now-(near-)empty -wal/-shm siblings so they don't
    // orphan next to the renamed main file. Best-effort: the sidecars are
    // empty after TRUNCATE and SQLite re-derives them on next open, so a
    // failure here is cosmetic and never data-bearing.
    for ext in ["-wal", "-shm", "-journal"] {
        let from = data_dir.join(format!("{legacy_name}{ext}"));
        if from.exists() {
            let to = data_dir.join(format!("{new_name}{ext}"));
            if let Err(e) = fs::rename(&from, &to) {
                tracing::warn!("[migrate] rename sidecar {legacy_name}{ext} failed ({e})");
            }
        }
    }

    tracing::info!("[migrate] renamed local store {legacy_name} → {new_name}");
    new_name
}

/// Boot-time, pre-pool migration of the two local SQLite stores from their
/// legacy `royalti-pa`-era names (`pa.db`, `pa.sqlite`) to the Ikenga names
/// (`ikenga.db`, `ikenga-terminal.sqlite`). Must run BEFORE
/// `apply_staged_restore_if_present` (which swaps `ikenga.db`) and before any
/// pool opens. Idempotent + atomic-on-failure per [`migrate_one_db_name`].
///
/// Returns the filename the **main** store (`ikenga.db`) should be opened
/// under — normally `ikenga.db`, but `pa.db` if that migration was skipped or
/// failed and the legacy file is still the source of truth. The caller threads
/// this into the `PaDb` path so a botched migration opens the intact legacy
/// file rather than creating a fresh empty db. The terminal store has no
/// Rust-side reader to redirect (the FE plugin opens it by connection string),
/// so its return is not surfaced — a failed terminal migration just means the
/// FE re-derives an empty terminal-sessions store, which is acceptable.
pub async fn migrate_legacy_db_names(data_dir: &Path) -> &'static str {
    let main = migrate_one_db_name(data_dir, LEGACY_DB_NAME, DB_NAME).await;
    let _terminal =
        migrate_one_db_name(data_dir, LEGACY_TERMINAL_DB_NAME, TERMINAL_DB_NAME).await;
    main
}

// ─── boot-time apply (called from lib.rs setup) ───────────────────────────────

/// Pre-pool stage: swap pa.db if a staged restore is present. Called BEFORE
/// any SQLite pool opens. Returns Ok(true) if applied this boot. The
/// secrets-pending.json (if any) is left in place; `apply_staged_secrets`
/// finishes that part once `SecretsLock` is registered.
pub fn apply_staged_restore_if_present(data_dir: &Path) -> Result<bool, String> {
    let staged_dir = data_dir.join(STAGED_DIR);
    let marker = staged_dir.join(MARKER_NAME);
    if !marker.exists() {
        return Ok(false);
    }
    let staged_db = staged_dir.join(STAGED_DB_NAME);
    if !staged_db.exists() {
        let _ = fs::remove_file(&marker);
        return Err("RESTORE_PENDING marker present but pa.db.new missing — cleared".into());
    }
    let live_db = data_dir.join(DB_NAME);
    for ext in ["-wal", "-shm", "-journal"] {
        let aux = data_dir.join(format!("{DB_NAME}{ext}"));
        let _ = fs::remove_file(&aux);
    }
    fs::rename(&staged_db, &live_db).map_err(|e| format!("swap pa.db: {e}"))?;
    // Marker stays until ALL deferred apply steps complete (secrets +
    // path rewrites). Each post-step removes its own staging file and
    // checks if the marker should be cleared. With nothing else staged,
    // clear it now.
    let secrets_pending = staged_dir.join(STAGED_SECRETS_NAME).exists();
    let rewrite_pending = staged_dir.join(STAGED_PATH_REWRITE_NAME).exists();
    if !secrets_pending && !rewrite_pending {
        let _ = fs::remove_file(&marker);
    }
    log::info!("[backup] swapped pa.db ← {}", live_db.display());
    Ok(true)
}

/// Post-Stronghold stage: if a `secrets-pending.json` is staged, replay it
/// into the vault then delete both it and the marker. Idempotent — safe to
/// call on every boot. Errors are surfaced to the log; we never fail boot
/// over a botched secrets restore.
pub fn apply_staged_secrets<R: tauri::Runtime>(app: &AppHandle<R>) {
    let data_dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[backup] app_data_dir for staged secrets: {e}");
            return;
        }
    };
    let staged_dir = data_dir.join(STAGED_DIR);
    let secrets_file = staged_dir.join(STAGED_SECRETS_NAME);
    if !secrets_file.exists() {
        return;
    }
    let lock: State<'_, SecretsLock> = match app.try_state::<SecretsLock>() {
        Some(s) => s,
        None => {
            log::error!("[backup] SecretsLock missing during staged-secrets apply");
            return;
        }
    };
    match load_pending_kvs(&secrets_file) {
        Ok(kvs) => match secrets::bulk_set(app, &lock, &kvs) {
            Ok(n) => {
                log::info!("[backup] restored {n} secrets from staged file");
                let _ = fs::remove_file(&secrets_file);
                let _ = fs::remove_file(staged_dir.join(MARKER_NAME));
            }
            Err(e) => log::error!("[backup] bulk_set failed: {e}"),
        },
        Err(e) => log::error!("[backup] load staged secrets: {e}"),
    }
}

// ─── path tokenize / detokenize ──────────────────────────────────────────────

/// Walk `PATH_COLUMNS` in the snapshot, rewriting `<home>/...` → `${IKENGA_HOME}/...`
/// in place. Returns a list of `PathWarning`s for paths outside `<home>`
/// (left untouched). `home` is canonical (no trailing slash).
async fn tokenize_snapshot_paths(snapshot: &Path, home: &str) -> Result<Vec<PathWarning>, String> {
    let home = home.trim_end_matches('/').to_string();
    let url = format!("sqlite://{}?mode=rwc", snapshot.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open snapshot for tokenize: {e}"))?;

    let mut warnings = Vec::new();
    for col in PATH_COLUMNS {
        let where_filter = col.where_clause.unwrap_or("1=1");
        // Find candidates: non-null, non-empty values that don't already
        // start with the token (idempotent in case a tokenized snapshot is
        // re-tokenized by mistake).
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
                // Table missing on older snapshots — skip silently.
                let m = e.to_string();
                if m.contains("no such table") {
                    log::debug!("tokenize: skipping missing table {}", col.table);
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

/// Apply a staged path-rewrite plan against the live `pa.db`. Called from
/// setup AFTER `apply_staged_restore_if_present`, AFTER the PaDb pool is
/// available. Spins up a small async runtime via tauri's runtime helper.
/// Idempotent — once the file is consumed it's deleted, and the rewrite
/// itself is a no-op on already-rewritten values.
pub fn apply_staged_path_rewrites<R: tauri::Runtime>(app: &AppHandle<R>) {
    let data_dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(e) => {
            log::error!("[backup] app_data_dir for path rewrite: {e}");
            return;
        }
    };
    let staged_dir = data_dir.join(STAGED_DIR);
    let plan_path = staged_dir.join(STAGED_PATH_REWRITE_NAME);
    if !plan_path.exists() {
        return;
    }
    let plan: serde_json::Value = match fs::read(&plan_path).map(|b| serde_json::from_slice(&b)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            log::error!("[backup] parse path-rewrite plan: {e}");
            let _ = fs::remove_file(&plan_path);
            return;
        }
        Err(e) => {
            log::error!("[backup] read path-rewrite plan: {e}");
            return;
        }
    };
    let from_token = plan
        .get("from_token")
        .and_then(|v| v.as_str())
        .unwrap_or(HOME_TOKEN)
        .to_string();
    let to_home = match plan.get("to_home").and_then(|v| v.as_str()) {
        Some(s) => s.trim_end_matches('/').to_string(),
        None => {
            log::error!("[backup] path-rewrite plan missing to_home");
            return;
        }
    };

    let live_db = data_dir.join(DB_NAME);
    let from_token_log = from_token.clone();
    let to_home_log = to_home.clone();
    let result: Result<usize, String> = tauri::async_runtime::block_on(async move {
        let url = format!("sqlite://{}?mode=rwc", live_db.display());
        let pool = sqlx::SqlitePool::connect(&url)
            .await
            .map_err(|e| format!("open live db: {e}"))?;
        let mut total: usize = 0;
        for col in PATH_COLUMNS {
            let where_filter = col.where_clause.unwrap_or("1=1");
            // REPLACE only at the start of the string, since SQLite's REPLACE
            // is global — guard with a substr prefix match.
            let upd_sql = format!(
                "UPDATE \"{t}\" SET \"{c}\" = ? || substr(\"{c}\", {n}) \
                 WHERE substr(\"{c}\", 1, {n}-1) = ? AND ({w})",
                t = col.table,
                c = col.column,
                n = from_token.len() + 1,
                w = where_filter,
            );
            match sqlx::query(&upd_sql)
                .bind(&to_home)
                .bind(&from_token)
                .execute(&pool)
                .await
            {
                Ok(r) => total += r.rows_affected() as usize,
                Err(e) => {
                    let m = e.to_string();
                    if !m.contains("no such table") {
                        log::warn!("path rewrite {}.{}: {e}", col.table, col.column);
                    }
                }
            }
        }
        pool.close().await;
        Ok(total)
    });
    match result {
        Ok(n) => {
            log::info!("[backup] rewrote {n} path-bearing rows ({from_token_log} → {to_home_log})");
            let _ = fs::remove_file(&plan_path);
            let _ = fs::remove_file(staged_dir.join(MARKER_NAME));
        }
        Err(e) => log::error!("[backup] path rewrite failed: {e}"),
    }
}

fn strip_home_prefix<'a>(value: &'a str, home: &str) -> Option<&'a str> {
    let bytes = value.as_bytes();
    let hb = home.as_bytes();
    if bytes.len() < hb.len() || &bytes[..hb.len()] != hb {
        return None;
    }
    // Match `home` itself, or `home/...` — not `home_other_dir`.
    match bytes.get(hb.len()) {
        None => Some(""),
        Some(&b'/') => Some(&value[hb.len()..]),
        _ => None,
    }
}

fn current_home_dir() -> Option<String> {
    // $HOME is unset on Windows; go through the platform resolver and
    // convert back to the String this callsite's callers expect.
    crate::platform::home_dir().map(|p| p.to_string_lossy().into_owned())
}

// ─── NDJSON text-export + loader (WP-06, G-03 / decision 7) ─────────────────
//
// Deterministic per-table NDJSON export: one JSON object per row, keys in
// column-definition order, rows in PRIMARY KEY (or rowid) order. Output lives
// in a caller-supplied directory as `<table>.ndjson`.
//
// Properties that make this git-versionable:
//   • Tables emitted in alphabetical order — same as `tables.json`.
//   • Rows ordered by PK → a single-row edit produces a single-line diff.
//   • Keys in PRAGMA table_info (cid) order → stable across re-exports.
//   • Pretty-deterministic JSON serialization (no float round-trip drift for
//     TEXT/INTEGER/NULL columns; REAL columns mirror sqlx's f64 serialization).
//
// `VACUUM INTO` stays as the fast binary backup (backup_export). This export
// is the diffable / committable artifact.

#[derive(Debug, Serialize)]
pub struct NdjsonTableSummary {
    pub table: String,
    pub rows: u64,
}

#[derive(Debug, Serialize)]
pub struct NdjsonExportResult {
    pub dir: String,
    pub table_count: usize,
    pub row_count: u64,
    pub tables: Vec<NdjsonTableSummary>,
}

#[derive(Debug, Serialize)]
pub struct NdjsonLoadResult {
    pub table_count: usize,
    pub row_count: u64,
}

/// Per-column metadata from `PRAGMA table_info`.
struct ColMeta {
    name: String,
    /// 0 = not part of PK; ≥1 = 1-indexed ordinal position in the PK.
    pk: i64,
}

/// Convert the value at column index `i` in a SQLite row to a serde_json Value.
/// Mirrors the type-mapping in `db_query` (db.rs) so export → import → export
/// round-trips identically.
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

/// Bind a slice of `serde_json::Value` onto a sqlx query (mirrors `bind_params`
/// in db.rs but lives here so WP-06 has no cross-module dep on that helper).
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

/// Inner logic of [`db_export_ndjson`] — takes a bare pool so tests can call
/// it directly without constructing a Tauri `State`.
async fn export_ndjson_to_dir(
    pool: &sqlx::SqlitePool,
    dest: &Path,
) -> Result<NdjsonExportResult, String> {
    use sqlx::Row;

    fs::create_dir_all(dest).map_err(|e| format!("mkdir dest_dir: {e}"))?;

    // All user tables, name-sorted for stable output order.
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
        // PRAGMA table_info: cid, name, type, notnull, dflt_value, pk
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

        // PK columns sorted by pk ordinal (1-indexed); fall back to rowid.
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

        // Explicit column list preserves schema-definition order in the output.
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

/// Export every user table in `pa.db` as a separate `<table>.ndjson` file in
/// `dest_dir`. Tables are emitted in alphabetical order; rows in PK / rowid
/// order. One JSON object per line; keys in column-definition order.
///
/// `VACUUM INTO` (backup_export) stays as the fast binary snapshot — this
/// command is the git-versionable, single-row-diffable text artifact.
#[tauri::command]
pub async fn db_export_ndjson(
    db: State<'_, Arc<PaDb>>,
    dest_dir: String,
) -> Result<NdjsonExportResult, String> {
    let pool = db.ensure_reader_pool().await?;
    export_ndjson_to_dir(&pool, Path::new(&dest_dir)).await
}

/// Inner logic of [`db_import_ndjson`] — takes a bare pool so tests can call
/// it directly without constructing a Tauri `State`.
async fn import_ndjson_from_dir(
    pool: &sqlx::SqlitePool,
    src: &Path,
) -> Result<NdjsonLoadResult, String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(src)
        .map_err(|e| format!("read_dir {}: {e}", src.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("ndjson"))
        .collect();
    entries.sort(); // alphabetical = same as export order

    let mut total_rows: u64 = 0;
    let table_count = entries.len();

    for path in &entries {
        let table = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("bad ndjson filename: {}", path.display()))?
            .to_string();

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

/// Load NDJSON files produced by `db_export_ndjson` into the current pa.db.
/// Each `<table>.ndjson` file is read in alphabetical order; each line is
/// parsed as a JSON object and upserted via `INSERT OR REPLACE`.
///
/// Idempotent: re-running replaces rows with identical primary keys.
/// Intended for the round-trip test: export → fresh db → import → re-export.
#[tauri::command]
pub async fn db_import_ndjson(
    db: State<'_, Arc<PaDb>>,
    src_dir: String,
) -> Result<NdjsonLoadResult, String> {
    let src = PathBuf::from(&src_dir);
    let pool = db.ensure_pool().await?;
    import_ndjson_from_dir(&pool, &src).await
}

// ─── helpers ──────────────────────────────────────────────────────────────────

async fn vacuum_into(src: &Path, dest: &Path) -> Result<(), String> {
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

async fn read_installed_pkgs(db_path: &Path) -> Result<Vec<PkgEntry>, String> {
    let url = format!("sqlite://{}?mode=ro", db_path.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .map_err(|e| format!("open snapshot: {e}"))?;
    // Tolerate missing table (older db that hasn't run 0007 yet).
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
            log::warn!("pkg_installed query failed: {e}");
            Ok(Vec::new())
        }
    }
}

fn read_manifest(zip_path: &Path) -> Result<BackupManifest, String> {
    let bytes = read_zip_bytes(zip_path, "manifest.json")?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse manifest: {e}"))
}

fn read_pkgs_from_zip(zip_path: &Path) -> Result<Vec<PkgEntry>, String> {
    match read_zip_bytes(zip_path, "installed-pkgs.json") {
        Ok(bytes) => {
            serde_json::from_slice::<Vec<PkgEntry>>(&bytes).map_err(|e| format!("parse pkgs: {e}"))
        }
        Err(_) => Ok(Vec::new()), // tolerate older bundles without the file
    }
}

fn read_zip_bytes(zip_path: &Path, name: &str) -> Result<Vec<u8>, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("read zip: {e}"))?;
    let mut entry = archive.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("read {name}: {e}"))?;
    Ok(buf)
}

fn extract_app_db(zip_path: &Path, dest: &Path) -> Result<(), String> {
    // The db member has always been stored as `app.db` (see `start_file`
    // above), independent of the on-disk filename, so this is already
    // version-agnostic for archives produced by this code. Back-compat
    // fallback for any archive that named the member after the live file:
    // try `app.db`, then the legacy `pa.db`, then the current `ikenga.db`.
    // Whatever is found is staged as the current STAGED_DB_NAME
    // (`ikenga.db.new`), so a legacy archive restores onto `ikenga.db`.
    let bytes = read_zip_bytes(zip_path, "app.db")
        .or_else(|_| read_zip_bytes(zip_path, "pa.db"))
        .or_else(|_| read_zip_bytes(zip_path, "ikenga.db"))
        .map_err(|_| "archive missing db member (app.db / pa.db / ikenga.db)".to_string())?;
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    fs::write(dest, &bytes).map_err(|e| format!("write staged db: {e}"))
}

fn marker_payload(m: &BackupManifest, src: &Path) -> Result<Vec<u8>, String> {
    let out = serde_json::json!({
        "source": src.to_string_lossy(),
        "manifest": m,
        "staged_at": now_iso(),
    });
    Ok(serde_json::to_vec_pretty(&out).unwrap())
}

fn local_backups_dir<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("app_local_data_dir: {e}"))?
        .join("backups");
    Ok(dir)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn hostname_or_unknown() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

fn load_pending_kvs(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let raw = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|e| format!("parse staged secrets: {e}"))?;
    let kvs = v
        .get("kvs")
        .and_then(|x| x.as_object())
        .ok_or("staged secrets json missing `kvs` object")?;
    let mut out = BTreeMap::new();
    for (k, v) in kvs.iter() {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    Ok(out)
}

fn write_chmod_600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir staged parent: {e}"))?;
    }
    fs::write(path, bytes).map_err(|e| format!("write staged secrets: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod staged secrets: {e}"))?;
    }
    Ok(())
}

// ─── age passphrase encryption ────────────────────────────────────────────────

fn age_encrypt(plaintext: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    use age::secrecy::SecretString;
    use std::io::Write as _;

    let encryptor =
        age::Encryptor::with_user_passphrase(SecretString::from(passphrase.to_string()));
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|e| format!("age wrap: {e}"))?;
    writer
        .write_all(plaintext)
        .map_err(|e| format!("age write: {e}"))?;
    writer.finish().map_err(|e| format!("age finish: {e}"))?;
    Ok(out)
}

fn age_decrypt(ciphertext: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    use age::secrecy::SecretString;
    use std::io::Read as _;

    let decryptor = match age::Decryptor::new(ciphertext).map_err(|e| format!("age open: {e}"))? {
        age::Decryptor::Passphrase(d) => d,
        age::Decryptor::Recipients(_) => {
            return Err("backup is not passphrase-encrypted".into());
        }
    };
    let mut reader = decryptor
        .decrypt(&SecretString::from(passphrase.to_string()), None)
        .map_err(|e| format!("age decrypt: {e}"))?;
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .map_err(|e| format!("age read: {e}"))?;
    Ok(out)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::db::PaDb;
    use std::collections::BTreeMap;

    async fn fresh_db() -> (PaDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("pa.db"));
        (db, tmp)
    }

    fn read_export_dir(dir: &Path) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for entry in fs::read_dir(dir).expect("read export dir") {
            let p = entry.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) == Some("ndjson") {
                let name = p.file_name().unwrap().to_str().unwrap().to_string();
                let content = fs::read_to_string(&p).expect("read ndjson");
                out.insert(name, content);
            }
        }
        out
    }

    /// WP-06 DoD: export → import into fresh db → re-export is BYTE-IDENTICAL;
    /// a single-row edit in the original db yields exactly one changed line in
    /// the affected table's .ndjson (i.e. a git diff is a single hunk).
    #[tokio::test]
    async fn wp06_ndjson_round_trip_byte_identical() {
        // ── db1: apply schema + insert two known tasks ────────────────────────
        let (db1, _tmp1) = fresh_db().await;
        let pool1 = db1.ensure_pool().await.expect("db1 ensure_pool");

        sqlx::query("INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)")
            .bind("tsk_rt_001")
            .bind("Round-trip task alpha")
            .bind("pending")
            .bind("medium")
            .execute(&pool1)
            .await
            .expect("insert task 1");

        sqlx::query("INSERT INTO tasks (id, title, status, priority) VALUES (?, ?, ?, ?)")
            .bind("tsk_rt_002")
            .bind("Round-trip task beta")
            .bind("done")
            .bind("high")
            .execute(&pool1)
            .await
            .expect("insert task 2");

        // ── export1 from db1 ──────────────────────────────────────────────────
        let exp1 = tempfile::tempdir().expect("exp1 dir");
        export_ndjson_to_dir(&pool1, exp1.path())
            .await
            .expect("export1");

        // ── db2: fresh schema + import from export1 ───────────────────────────
        let (db2, _tmp2) = fresh_db().await;
        let pool2 = db2.ensure_pool().await.expect("db2 ensure_pool");
        import_ndjson_from_dir(&pool2, exp1.path())
            .await
            .expect("import into db2");

        // ── export2 from db2 ──────────────────────────────────────────────────
        let exp2 = tempfile::tempdir().expect("exp2 dir");
        export_ndjson_to_dir(&pool2, exp2.path())
            .await
            .expect("export2");

        // ── assert BYTE-IDENTICAL ─────────────────────────────────────────────
        let snap1 = read_export_dir(exp1.path());
        let snap2 = read_export_dir(exp2.path());
        assert_eq!(
            snap1.len(),
            snap2.len(),
            "both exports must have the same number of .ndjson files"
        );
        for (name, c1) in &snap1 {
            let c2 = snap2
                .get(name)
                .unwrap_or_else(|| panic!("{name} missing from re-export"));
            assert_eq!(
                c1, c2,
                "re-export of {name} must be byte-identical to first export"
            );
        }

        // ── single-row edit → exactly one line differs ────────────────────────
        sqlx::query("UPDATE tasks SET status = 'in_progress' WHERE id = 'tsk_rt_001'")
            .execute(&pool1)
            .await
            .expect("update task");

        let exp3 = tempfile::tempdir().expect("exp3 dir");
        export_ndjson_to_dir(&pool1, exp3.path())
            .await
            .expect("export3");

        let snap3 = read_export_dir(exp3.path());
        let tasks1 = snap1.get("tasks.ndjson").expect("tasks.ndjson in snap1");
        let tasks3 = snap3.get("tasks.ndjson").expect("tasks.ndjson in snap3");

        let lines1: Vec<&str> = tasks1.lines().collect();
        let lines3: Vec<&str> = tasks3.lines().collect();
        assert_eq!(
            lines1.len(),
            lines3.len(),
            "a row-update must not change the line count"
        );
        let diff_count = lines1
            .iter()
            .zip(lines3.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            diff_count, 1,
            "exactly one line must differ after a single-row edit (got {diff_count})"
        );
    }
}
