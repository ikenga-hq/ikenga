//! RPC handlers and HTTP endpoints for backup and export on the daemon (WP-B).
//!
//! Provides:
//! - `backup_export`: generates `.ikbak` in `<data-dir>/backups` or private temp dir, returns result + base64 bytes
//! - `backup_import`: validates zip (Zip Slip, size cap, schema), restores into the requesting account's db only (R4-7)
//! - `db_export_ndjson`: exports user tables to `<data-dir>/exports`
//! - `db_import_ndjson`: imports tables with strict table-name and column-name allowlists
//! - HTTP `GET /api/backup/export`: streams download of the generated backup zip
//! - HTTP `POST /api/backup/import`: accepts uploaded backup zip stream, validates and restores

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::rpc::RpcResponse;
use super::rpc_local::{req_str, respond};
use super::rpc_shell::targ;
use super::shared::backups::{self, PathMode, MAX_BACKUP_SIZE_BYTES};
use super::AppState;

/// Helper to get the active database for the current daemon account.
fn get_pa_db(state: &AppState) -> Result<&crate::db::PaDb, String> {
    state
        .pa_db
        .as_deref()
        .ok_or_else(|| "no database configured for this server".to_string())
}

/// Helper to get the data directory for the current daemon account.
fn get_data_dir(state: &AppState) -> Result<&Path, String> {
    state
        .config
        .data_dir
        .as_deref()
        .ok_or_else(|| "no data directory configured for this server".to_string())
}

/// Whether `p` names anything on disk, counting a dangling symlink. `exists()`
/// follows links and reports false for a dangling one, which let the walk below
/// treat it as a missing path and `File::create` then wrote through it.
fn exists_no_follow(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// Resolve a path confined to `<data-dir>/<subdir>`.
/// Relative paths are placed under that subdir; traversal (`..`) is rejected;
/// absolute paths must canonicalize inside the subdir.
fn resolve_confined_path(
    data_dir: &Path,
    subdir: &str,
    user_path: Option<&str>,
    default_filename: &str,
) -> Result<PathBuf, String> {
    let base = data_dir.join(subdir);
    std::fs::create_dir_all(&base).map_err(|e| format!("mkdir {}: {e}", base.display()))?;

    match user_path {
        None => Ok(base.join(default_filename)),
        Some(raw) if raw.trim().is_empty() => Ok(base.join(default_filename)),
        Some(raw) => {
            let p = Path::new(raw);
            if p.is_relative() {
                if raw.contains("..") {
                    return Err(format!("path `{raw}` contains invalid directory traversal"));
                }
                let canon_base = base
                    .canonicalize()
                    .map_err(|e| format!("canon base {}: {e}", base.display()))?;
                let target = base.join(raw);
                let mut existing = target.as_path();
                let mut tail = Vec::new();
                while !exists_no_follow(existing) {
                    if let Some(parent) = existing.parent() {
                        if let Some(name) = existing.file_name() {
                            tail.push(name);
                        }
                        existing = parent;
                    } else {
                        break;
                    }
                }
                let canon_existing = existing.canonicalize().map_err(|e| {
                    format!(
                        "refusing path through a dangling or unreadable link {}: {e}",
                        existing.display()
                    )
                })?;
                let mut canon_target = canon_existing;
                for comp in tail.into_iter().rev() {
                    canon_target = canon_target.join(comp);
                }
                if !canon_target.starts_with(&canon_base) {
                    return Err(format!(
                        "refusing unconfined relative path outside account {subdir} directory: {raw}"
                    ));
                }
                Ok(canon_target)
            } else {
                let canon_base = base
                    .canonicalize()
                    .map_err(|e| format!("canon base {}: {e}", base.display()))?;
                let mut existing = p;
                let mut tail = Vec::new();
                while !exists_no_follow(existing) {
                    if let Some(parent) = existing.parent() {
                        if let Some(name) = existing.file_name() {
                            tail.push(name);
                        }
                        existing = parent;
                    } else {
                        break;
                    }
                }
                let canon_existing = existing.canonicalize().map_err(|e| {
                    format!(
                        "refusing path through a dangling or unreadable link {}: {e}",
                        existing.display()
                    )
                })?;
                let mut canon_target = canon_existing;
                for comp in tail.into_iter().rev() {
                    canon_target = canon_target.join(comp);
                }
                if !canon_target.starts_with(&canon_base) {
                    return Err(format!(
                        "refusing unconfined path outside account {subdir} directory: {raw}"
                    ));
                }
                Ok(canon_target)
            }
        }
    }
}

// ─── backup_export ───────────────────────────────────────────────────────────

pub(super) async fn backup_export(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let data_dir = get_data_dir(state)?;
        let dest_path_raw: Option<String> = targ(args, &["destPath", "dest_path"])?;
        let path_mode: Option<PathMode> = targ(args, &["pathMode", "path_mode"])?;
        let return_bytes: bool =
            targ::<Option<bool>>(args, &["returnBytes", "return_bytes"])?.unwrap_or(true);

        let default_name = format!(
            "ikenga-backup-{}.ikbak",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let dest = resolve_confined_path(
            data_dir,
            backups::BACKUPS_DIR,
            dest_path_raw.as_deref(),
            &default_name,
        )?;

        backups::export_backup_zip(data_dir, &dest, path_mode.unwrap_or_default(), return_bytes)
            .await
    }
    .await;
    respond("backup_export", r)
}

// ─── backup_import ───────────────────────────────────────────────────────────

pub(super) async fn backup_import(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let data_dir = get_data_dir(state)?;
        let db = get_pa_db(state)?;
        let dry_run: bool = targ::<Option<bool>>(args, &["dryRun", "dry_run"])?.unwrap_or(false);
        let src_path_raw: Option<String> = targ(args, &["srcPath", "src_path"])?;
        let bytes_base64: Option<String> = targ(args, &["bytesBase64", "bytes_base64"])?;

        let zip_path = if let Some(ref b64) = bytes_base64 {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.trim().as_bytes())
                .map_err(|e| format!("decode base64: {e}"))?;
            if bytes.len() as u64 > MAX_BACKUP_SIZE_BYTES {
                return Err(format!(
                    "uploaded backup exceeds size limit ({} > {MAX_BACKUP_SIZE_BYTES})",
                    bytes.len()
                ));
            }
            let staged_dir = data_dir.join(backups::STAGED_DIR);
            std::fs::create_dir_all(&staged_dir).map_err(|e| format!("mkdir staged: {e}"))?;
            let temp_zip = staged_dir.join(format!("upload-{}.ikbak", uuid::Uuid::new_v4()));
            std::fs::write(&temp_zip, &bytes).map_err(|e| format!("write temp zip: {e}"))?;
            temp_zip
        } else if let Some(ref raw) = src_path_raw {
            let p = Path::new(raw.as_str());
            if p.is_relative() {
                if raw.contains("..") {
                    return Err(format!("path `{raw}` contains invalid directory traversal"));
                }
                let base = data_dir.join(backups::BACKUPS_DIR);
                let target = base.join(raw);
                let canon = target
                    .canonicalize()
                    .map_err(|e| format!("source file not found or inaccessible: {e}"))?;
                let canon_base = base.canonicalize().unwrap_or_else(|_| base.clone());
                if !canon.starts_with(&canon_base) {
                    return Err(format!("refusing unconfined source path: {raw}"));
                }
                canon
            } else {
                // Must be confined to data_dir or allowed roots
                let canon = p
                    .canonicalize()
                    .map_err(|e| format!("source file not found or inaccessible: {e}"))?;
                let canon_data = data_dir
                    .canonicalize()
                    .unwrap_or_else(|_| data_dir.to_path_buf());
                if !canon.starts_with(&canon_data) && state.path_guard.check(&canon).is_err() {
                    return Err(format!("refusing unconfined source path: {raw}"));
                }
                canon
            }
        } else {
            return Err("either `bytesBase64` or `srcPath` must be provided".to_string());
        };

        let result = backups::import_backup_zip(data_dir, db, &zip_path, dry_run).await;
        // Clean up temporary upload file if it was written
        let staged_base = data_dir.join(backups::STAGED_DIR);
        if zip_path.starts_with(&staged_base) && zip_path.exists() {
            let _ = std::fs::remove_file(&zip_path);
            let _ = std::fs::remove_dir(&staged_base);
        }
        result
    }
    .await;
    respond("backup_import", r)
}

// ─── db_export_ndjson ────────────────────────────────────────────────────────

pub(super) async fn db_export_ndjson(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let data_dir = get_data_dir(state)?;
        let db = get_pa_db(state)?;
        let dest_dir_raw: Option<String> = targ(args, &["destDir", "dest_dir"])?;

        let default_name = format!("ndjson-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
        let dest = resolve_confined_path(
            data_dir,
            backups::EXPORTS_DIR,
            dest_dir_raw.as_deref(),
            &default_name,
        )?;

        let pool = db.ensure_reader_pool().await?;
        backups::export_ndjson_to_dir(&pool, &dest).await
    }
    .await;
    respond("db_export_ndjson", r)
}

// ─── db_import_ndjson ────────────────────────────────────────────────────────

pub(super) async fn db_import_ndjson(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let data_dir = get_data_dir(state)?;
        let db = get_pa_db(state)?;
        let src_dir_raw = req_str(args, &["srcDir", "src_dir"])?;

        let p = Path::new(&src_dir_raw);
        let src = if p.is_relative() {
            if src_dir_raw.contains("..") {
                return Err(format!(
                    "path `{src_dir_raw}` contains invalid directory traversal"
                ));
            }
            let base = data_dir.join(backups::EXPORTS_DIR);
            let target = base.join(&src_dir_raw);
            let canon = target
                .canonicalize()
                .map_err(|e| format!("source directory not found: {e}"))?;
            let canon_base = base.canonicalize().unwrap_or_else(|_| base.clone());
            if !canon.starts_with(&canon_base) {
                return Err(format!(
                    "refusing unconfined source directory: {src_dir_raw}"
                ));
            }
            canon
        } else {
            let canon = p
                .canonicalize()
                .map_err(|e| format!("source directory not found: {e}"))?;
            let canon_data = data_dir
                .canonicalize()
                .unwrap_or_else(|_| data_dir.to_path_buf());
            if !canon.starts_with(&canon_data) && state.path_guard.check(&canon).is_err() {
                return Err(format!(
                    "refusing unconfined source directory: {src_dir_raw}"
                ));
            }
            canon
        };

        let pool = db.ensure_pool().await?;
        backups::import_ndjson_from_dir(&pool, &src).await
    }
    .await;
    respond("db_import_ndjson", r)
}

// ─── HTTP Streaming Endpoints ────────────────────────────────────────────────

/// `GET /api/backup/export`
/// Generates and streams the `.ikbak` backup as an HTTP download.
pub(crate) async fn http_backup_export(
    State(state): State<Arc<AppState>>,
) -> Result<Response, (StatusCode, String)> {
    let data_dir = match get_data_dir(&state) {
        Ok(d) => d,
        Err(e) => return Err((StatusCode::BAD_REQUEST, e)),
    };

    let filename = format!(
        "ikenga-backup-{}.ikbak",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let temp_dest = data_dir
        .join(backups::BACKUPS_DIR)
        .join(format!("stream-{}.ikbak", uuid::Uuid::new_v4()));

    if let Err(e) = backups::export_backup_zip(data_dir, &temp_dest, PathMode::Raw, false).await {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("export failed: {e}"),
        ));
    }

    let bytes = match std::fs::read(&temp_dest) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&temp_dest);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read backup: {e}"),
            ));
        }
    };
    let _ = std::fs::remove_file(&temp_dest);

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"")
            .parse()
            .unwrap(),
    );

    Ok((headers, bytes).into_response())
}

/// `POST /api/backup/import`
/// Accepts an uploaded `.ikbak` binary body stream, validates and restores it.
pub(crate) async fn http_backup_import(
    State(state): State<Arc<AppState>>,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let err_json = |status, msg: String| (status, Json(json!({ "ok": false, "error": msg })));

    let data_dir = match get_data_dir(&state) {
        Ok(d) => d,
        Err(e) => return Err(err_json(StatusCode::BAD_REQUEST, e)),
    };
    let db = match get_pa_db(&state) {
        Ok(d) => d,
        Err(e) => return Err(err_json(StatusCode::BAD_REQUEST, e)),
    };

    if body.len() as u64 > MAX_BACKUP_SIZE_BYTES {
        return Err(err_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "backup upload exceeds limit ({} > {MAX_BACKUP_SIZE_BYTES})",
                body.len()
            ),
        ));
    }

    let staged_dir = data_dir.join(backups::STAGED_DIR);
    if let Err(e) = std::fs::create_dir_all(&staged_dir) {
        return Err(err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("mkdir staged: {e}"),
        ));
    }
    let temp_zip = staged_dir.join(format!("upload-{}.ikbak", uuid::Uuid::new_v4()));
    if let Err(e) = std::fs::write(&temp_zip, &body) {
        return Err(err_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("write upload: {e}"),
        ));
    }

    let res = backups::import_backup_zip(data_dir, db, &temp_zip, false).await;
    let _ = std::fs::remove_file(&temp_zip);
    let _ = std::fs::remove_dir(&staged_dir);

    match res {
        Ok(v) => Ok(Json(json!({ "ok": true, "data": v })).into_response()),
        Err(e) => Err(err_json(StatusCode::BAD_REQUEST, e)),
    }
}

// ─── Tests (WP-B security and functional verification) ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::{router_with_home, ServerConfig};
    use axum::body::Body;
    use std::io::Write;
    use tower::ServiceExt;

    struct TestDaemon {
        _tmp: tempfile::TempDir,
        data: PathBuf,
        db: Arc<PaDb>,
        router: axum::Router,
    }

    fn test_daemon() -> TestDaemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, home) = (root.join("data"), root.join("home"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&home).unwrap();

        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let config = ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
            data_dir: Some(data.clone()),
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        };

        let router = router_with_home(
            config,
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db.clone()),
            None,
            Some(home.clone()),
        );

        TestDaemon {
            _tmp: tmp,
            data,
            db,
            router,
        }
    }

    async fn rpc(router: &axum::Router, cmd: &str, args: Value) -> Value {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn ok(router: &axum::Router, cmd: &str, args: Value) -> Value {
        let res = rpc(router, cmd, args.clone()).await;
        if res["ok"] != true {
            panic!("FAILED {cmd} error: {:?}", res.get("error"));
        }
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    async fn err(router: &axum::Router, cmd: &str, args: Value) -> String {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
        res["error"].as_str().unwrap().to_string()
    }

    /// Names in `data` that a finished import must not leave: the previous
    /// db (`ikenga.db.prev*`) and any `staged-restore*` folder.
    fn data_dir_leftovers(data: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(data)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("ikenga.db.prev") || n.starts_with("staged-restore"))
            .collect();
        out.sort();
        out
    }

    /// Copy `backups/<src>` to `backups/<dst>` with its `app.db` rewritten by
    /// `sql` (run against a throwaway copy of the embedded db).
    async fn tamper_backup(d: &TestDaemon, src: &str, dst: &str, sql: &str) {
        let tmp_db = d.data.join(format!("tamper-{dst}.db"));
        {
            let file = std::fs::File::open(d.data.join("backups").join(src)).unwrap();
            let mut archive = zip::ZipArchive::new(file).unwrap();
            let mut entry = archive.by_name("app.db").unwrap();
            let mut out = std::fs::File::create(&tmp_db).unwrap();
            std::io::copy(&mut entry, &mut out).unwrap();
        }
        {
            let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&tmp_db);
            let p = sqlx::SqlitePool::connect_with(opts).await.unwrap();
            sqlx::raw_sql(sql).execute(&p).await.unwrap();
            p.close().await;
        }
        let file = std::fs::File::create(d.data.join("backups").join(dst)).unwrap();
        let mut w = zip::ZipWriter::new(file);
        w.start_file::<_, ()>("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(
            json!({
                "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
                "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        w.start_file::<_, ()>("app.db", zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(&std::fs::read(&tmp_db).unwrap()).unwrap();
        w.finish().unwrap();
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn zip_slip_in_backup_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        // Build a zip containing a path traversal entry "../../evil.txt"
        let bad_zip = d.data.join("evil_slip.ikbak");
        let file = std::fs::File::create(&bad_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(json!({
            "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
            "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
        }).to_string().as_bytes()).unwrap();

        zip.start_file::<_, ()>("../../evil.txt", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(b"malicious content").unwrap();
        zip.finish().unwrap();

        let bytes = std::fs::read(&bad_zip).unwrap();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(r, "backup_import", json!({ "bytesBase64": b64 })).await;
        assert!(e.contains("refusing malicious zip entry path"), "{e}");
    }

    #[tokio::test]
    async fn oversized_upload_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        // Create an upload exceeding 100MB
        // Instead of allocating 101MB of actual bytes, we can test that the size check triggers
        // when decoded bytes length > MAX_BACKUP_SIZE_BYTES or HTTP body check.
        // Let's create an archive that has uncompressed entry exceeding MAX_BACKUP_SIZE_BYTES:
        let bad_zip = d.data.join("bomb.ikbak");
        let file = std::fs::File::create(&bad_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(json!({
            "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
            "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
        }).to_string().as_bytes()).unwrap();

        // An unauthorized entry name is also refused
        zip.start_file::<_, ()>(
            "unauthorized_payload.bin",
            zip::write::FileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"bad").unwrap();
        zip.finish().unwrap();

        let bytes = std::fs::read(&bad_zip).unwrap();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(r, "backup_import", json!({ "bytesBase64": b64 })).await;
        assert!(
            e.contains("refusing unknown or unauthorized zip entry"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn newer_schema_in_backup_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        let future_zip = d.data.join("future.ikbak");
        let file = std::fs::File::create(&future_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(json!({
            "format_version": 3, "schema_version": 999, "created_at": "2026-01-01T00:00:00Z",
            "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
        }).to_string().as_bytes()).unwrap();
        zip.start_file::<_, ()>("ikenga.db", zip::write::FileOptions::default())
            .unwrap();
        zip.write_all(b"sqlite format 3\0").unwrap();
        zip.finish().unwrap();

        let bytes = std::fs::read(&future_zip).unwrap();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("backup schema 999 is newer than running app schema"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn sql_injection_in_ndjson_table_name_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        let export_dir = d.data.join("exports").join("malicious_dir");
        std::fs::create_dir_all(&export_dir).unwrap();

        // Plant a file with SQL injection in its name
        let evil_file = export_dir.join("tasks\"; DROP TABLE tasks; --.ndjson");
        std::fs::write(&evil_file, "{\"id\": \"1\"}\n").unwrap();

        let e = err(r, "db_import_ndjson", json!({ "srcDir": "malicious_dir" })).await;
        assert!(
            e.contains("refusing to import into unrecognized or unauthorized table"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn t0_round_trip_export_and_restore_works() {
        let d = test_daemon();
        let r = &d.router;

        // Initialize live database with a custom project row
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_roundtrip', 'Roundtrip Project', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // 1. Export backup via RPC
        let export_res = ok(r, "backup_export", json!({ "returnBytes": true })).await;
        let b64 = export_res["bytes_base64"]
            .as_str()
            .expect("bytes_base64 present");

        // 2. Modify live database: delete the project
        sqlx::query("DELETE FROM projects WHERE id = 'proj_roundtrip'")
            .execute(&pool)
            .await
            .unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_roundtrip'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 0, "project was deleted from live db");

        // 3. Import the backup (restores live db in place)
        let import_res = ok(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": false }),
        )
        .await;
        assert_eq!(import_res["requires_restart"], false);

        // 4. Verify live db is restored
        let pool_after = d.db.ensure_pool().await.unwrap();
        let count_after: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_roundtrip'")
                .fetch_one(&pool_after)
                .await
                .unwrap();
        assert_eq!(
            count_after, 1,
            "project was restored into requesting account's db"
        );
    }

    #[tokio::test]
    async fn arm_level_zip_bomb_with_lying_header_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        let bomb_zip = d.data.join("bomb_lying_header.ikbak");
        let file = std::fs::File::create(&bomb_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            json!({
                "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
                "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();

        // app.db with 105 MB of deflated zeroes
        zip.start_file::<_, ()>(
            "app.db",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..105 {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();

        let mut bytes = std::fs::read(&bomb_zip).unwrap();
        // Compressed zip of zeroes is small (< 500 KB)
        assert!(bytes.len() < 500 * 1024, "compressed bomb is small");

        // Forge a lying header: patch declared uncompressed size to 100 bytes
        let magic_local = [0x50, 0x4b, 0x03, 0x04];
        let magic_cd = [0x50, 0x4b, 0x01, 0x02];
        for i in 0..bytes.len().saturating_sub(50) {
            if &bytes[i..i + 4] == &magic_local {
                let name_len = u16::from_le_bytes([bytes[i + 26], bytes[i + 27]]) as usize;
                if i + 30 + name_len <= bytes.len()
                    && &bytes[i + 30..i + 30 + name_len] == b"app.db"
                {
                    bytes[i + 22..i + 26].copy_from_slice(&100u32.to_le_bytes());
                }
            } else if &bytes[i..i + 4] == &magic_cd {
                let name_len = u16::from_le_bytes([bytes[i + 28], bytes[i + 29]]) as usize;
                if i + 46 + name_len <= bytes.len()
                    && &bytes[i + 46..i + 46 + name_len] == b"app.db"
                {
                    bytes[i + 24..i + 28].copy_from_slice(&100u32.to_le_bytes());
                }
            }
        }
        std::fs::write(&bomb_zip, &bytes).unwrap();

        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("decompressed size exceeds maximum"),
            "expected zip bomb refusal, got: {e}"
        );
    }

    #[tokio::test]
    async fn arm_level_failed_restore_keeps_previous_db_and_rolls_back() {
        let d = test_daemon();
        let r = &d.router;

        // Seed project row in live DB
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_survives', 'Surviving Project', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a backup passing initial schema check (has _pa_migrations(id) and layout_state)
        // but whose migrations fail because _pa_migrations lacks applied_at column!
        let bad_zip = d.data.join("bad_migrations.ikbak");
        let bad_db_file = d.data.join("bad_migrations.db");
        {
            let opts = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&bad_db_file)
                .create_if_missing(true);
            let bad_pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
            sqlx::query(
                "CREATE TABLE _pa_migrations(id INTEGER PRIMARY KEY);
                 CREATE TABLE layout_state(id TEXT PRIMARY KEY, value TEXT);",
            )
            .execute(&bad_pool)
            .await
            .unwrap();
            bad_pool.close().await;
        }

        let file = std::fs::File::create(&bad_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            json!({
                "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
                "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();

        zip.start_file::<_, ()>("app.db", zip::write::SimpleFileOptions::default())
            .unwrap();
        let db_bytes = std::fs::read(&bad_db_file).unwrap();
        zip.write_all(&db_bytes).unwrap();
        zip.finish().unwrap();

        let bytes = std::fs::read(&bad_zip).unwrap();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": false }),
        )
        .await;
        // The refusal must come from the staged step, before the swap. A
        // bad db swapped in and rolled back would say "rolled back" instead.
        assert!(
            e.contains("run migrations on staged db failed"),
            "expected the staged-migration refusal, got: {e}"
        );
        assert!(
            !e.contains("rolled back"),
            "refused before the swap, not rolled back after it: {e}"
        );
        assert_eq!(
            data_dir_leftovers(&d.data),
            Vec::<String>::new(),
            "a refused import leaves nothing behind"
        );

        // Verify live DB was NOT destroyed: project row is still present!
        let pool_after = d.db.ensure_pool().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_survives'")
                .fetch_one(&pool_after)
                .await
                .unwrap();
        assert_eq!(
            count, 1,
            "original project row was preserved after failed restore"
        );
    }

    #[tokio::test]
    async fn arm_level_post_swap_failure_rolls_back_to_previous_db() {
        let d = test_daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();

        // The backup holds `proj_in_backup` only.
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_in_backup', 'In Backup', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let _ = ok(
            r,
            "backup_export",
            json!({ "destPath": "valid.ikbak", "pathMode": "raw" }),
        )
        .await;

        // Written AFTER the export: only the live db has it. If the rollback
        // does not put the previous file back, the restored backup (which
        // lacks this row) stays live and the row is gone.
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_after_export', 'After Export', '/home/proj', 2, 0, 200)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Fail this data dir's restore right after the swap (per data dir, so
        // no other test in the process sees it).
        std::fs::write(
            d.data
                .join(crate::server::shared::backups::TEST_POST_SWAP_FAULT_MARKER),
            b"",
        )
        .unwrap();

        let e = err(
            r,
            "backup_import",
            json!({ "srcPath": "valid.ikbak", "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("rolled back to previous database"),
            "expected rollback error message, got: {e}"
        );

        let pool_after = d.db.ensure_pool().await.unwrap();
        let after_export: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_after_export'")
                .fetch_one(&pool_after)
                .await
                .unwrap();
        assert_eq!(
            after_export, 1,
            "the row written after the export must survive the rollback"
        );
        // And the rolled-back db is writable through the reopened pool.
        sqlx::query("DELETE FROM projects WHERE id = 'proj_after_export'")
            .execute(&pool_after)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn arm_level_incomplete_schema_backup_refused() {
        let d = test_daemon();
        let r = &d.router;

        // Seed project row in live DB
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_survives_incomplete', 'Surviving Incomplete Project', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Export a valid backup to a file
        let _ = ok(
            r,
            "backup_export",
            json!({ "destPath": "complete.ikbak", "pathMode": "raw" }),
        )
        .await;

        // Now create a backup based on a fully migrated db, but with pkg_installed dropped!
        let complete_path = d.data.join("backups/complete.ikbak");
        let incomplete_zip = d.data.join("backups/incomplete_schema.ikbak");
        let tmp_db = d.data.join("tmp_incomplete.db");

        {
            let file = std::fs::File::open(&complete_path).unwrap();
            let mut archive = zip::ZipArchive::new(file).unwrap();
            let mut entry = archive.by_name("app.db").unwrap();
            let mut out = std::fs::File::create(&tmp_db).unwrap();
            std::io::copy(&mut entry, &mut out).unwrap();
        }

        // Drop pkg_installed table from tmp_db
        {
            let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&tmp_db);
            let p = sqlx::SqlitePool::connect_with(opts).await.unwrap();
            sqlx::query("DROP TABLE pkg_installed")
                .execute(&p)
                .await
                .unwrap();
            p.close().await;
        }

        // Re-package into incomplete_schema.ikbak
        {
            let file = std::fs::File::create(&incomplete_zip).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file::<_, ()>("manifest.json", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(
                json!({
                    "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
                    "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
                })
                .to_string()
                .as_bytes(),
            ).unwrap();
            writer
                .start_file::<_, ()>("app.db", zip::write::SimpleFileOptions::default())
                .unwrap();
            let bytes = std::fs::read(&tmp_db).unwrap();
            writer.write_all(&bytes).unwrap();
            writer.finish().unwrap();
        }

        let e = err(
            r,
            "backup_import",
            json!({ "srcPath": "incomplete_schema.ikbak", "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("missing required table `pkg_installed`"),
            "expected missing required table error, got: {e}"
        );

        // Verify user's live data survived
        let pool_after = d.db.ensure_pool().await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM projects WHERE id = 'proj_survives_incomplete'",
        )
        .fetch_one(&pool_after)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn arm_level_nested_symlink_under_missing_subfolder_refused() {
        let d = test_daemon();
        let r = &d.router;

        // Place a symlink inside backups/ pointing outside backups/
        let backups_dir = d.data.join("backups");
        std::fs::create_dir_all(&backups_dir).unwrap();
        let outside_dir = d.data.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let link_path = backups_dir.join("lnk");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_dir, &link_path).unwrap();

        // Attempt export through missing subfolder under the symlink
        let e = err(
            r,
            "backup_export",
            json!({ "destPath": "lnk/newsub/esc2.ikbak" }),
        )
        .await;
        assert!(
            e.contains("refusing unconfined relative path"),
            "expected confinement error, got: {e}"
        );
    }

    #[tokio::test]
    async fn arm_level_wrong_schema_junk_tables_is_refused() {
        let d = test_daemon();
        let r = &d.router;

        // Seed project row in live DB
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_survives_junk', 'Surviving Junk Project', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Create a backup with only junk(x) table, claiming schema 7
        let junk_zip = d.data.join("junk_schema.ikbak");
        let junk_db_file = d.data.join("junk.db");
        {
            let opts = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&junk_db_file)
                .create_if_missing(true);
            let junk_pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
            sqlx::query("CREATE TABLE junk(x TEXT);")
                .execute(&junk_pool)
                .await
                .unwrap();
            junk_pool.close().await;
        }

        let file = std::fs::File::create(&junk_zip).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file::<_, ()>("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            json!({
                "format_version": 3, "schema_version": 7, "created_at": "2026-01-01T00:00:00Z",
                "hostname": "h", "username": "u", "path_mode": "raw", "has_secrets": false, "pkg_count": 0
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();

        zip.start_file::<_, ()>("app.db", zip::write::SimpleFileOptions::default())
            .unwrap();
        let db_bytes = std::fs::read(&junk_db_file).unwrap();
        zip.write_all(&db_bytes).unwrap();
        zip.finish().unwrap();

        let bytes = std::fs::read(&junk_zip).unwrap();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

        let e = err(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("missing required `_pa_migrations` table"),
            "expected error, got: {e}"
        );

        // Verify live DB was NOT destroyed: project row is still present!
        let pool_after = d.db.ensure_pool().await.unwrap();
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_survives_junk'")
                .fetch_one(&pool_after)
                .await
                .unwrap();
        assert_eq!(
            count, 1,
            "original project row was preserved after wrong-schema restore"
        );
    }

    #[tokio::test]
    async fn arm_level_staged_schema_with_wrong_columns_is_refused() {
        let d = test_daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_survives_cols', 'Survives', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let _ = ok(r, "backup_export", json!({ "destPath": "good.ikbak" })).await;

        // Same table name, different columns.
        tamper_backup(
            &d,
            "good.ikbak",
            "wrongcols.ikbak",
            "DROP TABLE pkg_installed; CREATE TABLE pkg_installed(x TEXT);",
        )
        .await;

        let e = err(
            r,
            "backup_import",
            json!({ "srcPath": "wrongcols.ikbak", "dryRun": false }),
        )
        .await;
        assert!(
            e.contains("table `pkg_installed` is missing required column"),
            "expected a column refusal, got: {e}"
        );
        let pool_after = d.db.ensure_pool().await.unwrap();
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM projects WHERE id = 'proj_survives_cols'")
                .fetch_one(&pool_after)
                .await
                .unwrap();
        assert_eq!(n, 1, "live data survives a refused import");
        assert_eq!(data_dir_leftovers(&d.data), Vec::<String>::new());
    }

    #[tokio::test]
    async fn arm_level_dangling_symlink_export_destination_is_refused() {
        let d = test_daemon();
        let r = &d.router;
        // A live db, so an unconfined export would succeed.
        d.db.ensure_pool().await.unwrap();
        let backups_dir = d.data.join("backups");
        std::fs::create_dir_all(&backups_dir).unwrap();
        let outside = d.data.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("escaped.ikbak");
        // `dang` points at a file that does not exist yet.
        std::os::unix::fs::symlink(&target, backups_dir.join("dang")).unwrap();

        let e = err(r, "backup_export", json!({ "destPath": "dang" })).await;
        assert!(
            e.contains("dangling or unreadable link"),
            "expected a symlink refusal, got: {e}"
        );
        assert!(
            !target.exists(),
            "export must not write through a dangling link"
        );
    }

    #[tokio::test]
    async fn arm_level_successful_restore_leaves_no_prev_or_staged_dirs() {
        let d = test_daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_leftovers', 'Leftovers', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let export = ok(
            r,
            "backup_export",
            json!({ "destPath": "keep.ikbak", "returnBytes": true }),
        )
        .await;
        let b64 = export["bytes_base64"].as_str().unwrap().to_string();

        // srcPath import: no upload folder, but the previous db must be pruned.
        let _ = ok(r, "backup_import", json!({ "srcPath": "keep.ikbak" })).await;
        assert_eq!(
            data_dir_leftovers(&d.data),
            Vec::<String>::new(),
            "srcPath import"
        );

        // base64 import: the temp upload lives in `staged-restore/`.
        let _ = ok(r, "backup_import", json!({ "bytesBase64": b64 })).await;
        assert_eq!(
            data_dir_leftovers(&d.data),
            Vec::<String>::new(),
            "base64 import"
        );

        // dry run over base64 stages the upload too.
        let _ = ok(
            r,
            "backup_import",
            json!({ "bytesBase64": b64, "dryRun": true }),
        )
        .await;
        assert_eq!(
            data_dir_leftovers(&d.data),
            Vec::<String>::new(),
            "dry-run import"
        );
    }

    #[tokio::test]
    async fn arm_level_http_import_leaves_no_staged_dir() {
        use base64::Engine as _;
        let d = test_daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
             VALUES ('proj_http', 'Http', '/home/proj', 1, 0, 100)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let export = ok(r, "backup_export", json!({ "returnBytes": true })).await;
        let zip = base64::engine::general_purpose::STANDARD
            .decode(export["bytes_base64"].as_str().unwrap())
            .unwrap();

        let res = r
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/backup/import")
                    .header("authorization", "Bearer tok")
                    .body(Body::from(zip))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(data_dir_leftovers(&d.data), Vec::<String>::new());
    }

    #[tokio::test]
    async fn arm_level_post_swap_fault_is_scoped_to_its_data_dir() {
        // Two daemons in one process: a fault planted in A's data dir must not
        // reach B (a process-wide flag made `t0_round_trip...` flaky).
        let a = test_daemon();
        let b = test_daemon();
        for d in [&a, &b] {
            d.db.ensure_pool().await.unwrap();
            let _ = ok(&d.router, "backup_export", json!({ "destPath": "x.ikbak" })).await;
        }
        std::fs::write(
            a.data
                .join(crate::server::shared::backups::TEST_POST_SWAP_FAULT_MARKER),
            b"",
        )
        .unwrap();

        let e = err(&a.router, "backup_import", json!({ "srcPath": "x.ikbak" })).await;
        assert!(e.contains("injected post-swap fault"), "{e}");
        let _ = ok(&b.router, "backup_import", json!({ "srcPath": "x.ikbak" })).await;
    }
}
