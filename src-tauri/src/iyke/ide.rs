//! IDE MCP server & lock file manager (WP-12).
//!
//! Manages `~/.claude/ide/<port>.lock` (and overlay `ide/<port>.lock`) so the `claude` CLI
//! automatically discovers Ikenga as its IDE host. Exposes `openFile`, `getSelections`,
//! and `getDiagnostics` IPC handlers.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

static ACTIVE_LOCK: OnceLock<Arc<Mutex<Option<IdeLockInfo>>>> = OnceLock::new();

fn get_lock_store() -> &'static Arc<Mutex<Option<IdeLockInfo>>> {
    ACTIVE_LOCK.get_or_init(|| Arc::new(Mutex::new(None)))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeLockInfo {
    pub port: u16,
    #[serde(rename = "authToken")]
    pub auth_token: String,
    pub pid: u32,
    pub lock_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFileParams {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

/// Writes `ide/<port>.lock` in the specified base directory (e.g., overlay dir or `~/.claude`).
pub fn write_ide_lock_file(
    base_dir: &Path,
    port: u16,
    auth_token: &str,
) -> std::io::Result<IdeLockInfo> {
    let ide_dir = base_dir.join("ide");
    fs::create_dir_all(&ide_dir)?;

    let lock_path = ide_dir.join(format!("{}.lock", port));
    let lock_info = IdeLockInfo {
        port,
        auth_token: auth_token.to_string(),
        pid: std::process::id(),
        lock_path: lock_path.to_string_lossy().to_string(),
    };

    let content = serde_json::to_string_pretty(&lock_info)?;
    fs::write(&lock_path, content)?;

    if let Ok(mut store) = get_lock_store().lock() {
        *store = Some(lock_info.clone());
    }

    Ok(lock_info)
}

/// Post route handler: POST /iyke/ide/open_file
pub async fn post_ide_open_file(
    Extension(app): Extension<AppHandle>,
    Json(params): Json<OpenFileParams>,
) -> impl IntoResponse {
    let _ = app.emit("ide://open_file", &params);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "opened": params.file_path
        })),
    )
}

/// Get route handler: GET /iyke/ide/lock
pub async fn get_ide_lock_status() -> impl IntoResponse {
    let lock = get_lock_store()
        .lock()
        .ok()
        .and_then(|guard| guard.clone());

    (StatusCode::OK, Json(lock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_write_ide_lock_file() {
        let tmp = TempDir::new().expect("tempdir");
        let lock_info = write_ide_lock_file(tmp.path(), 12345, "secret-token-777")
            .expect("write lock file");

        assert_eq!(lock_info.port, 12345);
        assert_eq!(lock_info.auth_token, "secret-token-777");

        let lock_file_path = PathBuf::from(&lock_info.lock_path);
        assert!(lock_file_path.is_file());

        let raw = fs::read_to_string(lock_file_path).expect("read lock file");
        assert!(raw.contains("secret-token-777"));
        assert!(raw.contains("12345"));
    }
}
