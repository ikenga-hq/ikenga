use std::sync::Arc;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use super::AppState;
use crate::pty::SpawnOpts;

#[derive(Deserialize, Debug)]
pub struct RpcRequest {
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Serialize)]
pub struct RpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RpcResponse {
    pub fn success(data: impl Serialize) -> Self {
        Self {
            ok: true,
            data: serde_json::to_value(data).ok(),
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

pub async fn rpc_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<RpcRequest>,
) -> impl IntoResponse {
    debug!("RPC request: cmd={}", payload.cmd);

    let res = match payload.cmd.as_str() {
        // --- PTY Commands ---
        "pty_spawn" => {
            let terminal_id = payload.args.get("terminal_id").and_then(|v| v.as_str()).map(str::to_string);
            let title = payload.args.get("title").and_then(|v| v.as_str()).map(str::to_string);
            let cwd = payload.args.get("cwd").and_then(|v| v.as_str()).unwrap_or(".").to_string();
            let cmd: Vec<String> = payload.args.get("cmd")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
                .unwrap_or_else(|| {
                    if cfg!(windows) {
                        vec!["powershell.exe".to_string()]
                    } else {
                        vec!["/bin/bash".to_string()]
                    }
                });
            let rows = payload.args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            let cols = payload.args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;

            match state.pty_manager.spawn_headless(SpawnOpts {
                terminal_id,
                title,
                cwd,
                cmd,
                env: std::collections::HashMap::new(),
                rows,
                cols,
            }).await {
                Ok(pty_id) => RpcResponse::success(serde_json::json!({ "pty_id": pty_id })),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_write" => {
            let id = payload.args.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let data = payload.args.get("data").and_then(|v| v.as_str()).unwrap_or_default();
            match state.pty_manager.write(id, data.as_bytes()) {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_resize" => {
            let id = payload.args.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let rows = payload.args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            let cols = payload.args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            match state.pty_manager.resize(id, rows, cols) {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_kill" => {
            let id = payload.args.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            match state.pty_manager.kill(id) {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_list" | "pty_terminal_list" => {
            let terminals = state.pty_manager.list_terminals();
            RpcResponse::success(terminals)
        }
        "pty_foreground" => {
            let id = payload.args.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            RpcResponse::success(state.pty_manager.foreground(id))
        }
        "pty_foreground_snapshot" => {
            RpcResponse::success(state.pty_manager.foreground_snapshot())
        }

        // --- FS Commands ---
        "fs_exists" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let exists = std::path::Path::new(path_str).exists();
            RpcResponse::success(exists)
        }
        "fs_read" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            match tokio::fs::read_to_string(path_str).await {
                Ok(content) => RpcResponse::success(content),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "fs_write" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let content = payload.args.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            if let Some(parent) = std::path::Path::new(path_str).parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::write(path_str, content).await {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "fs_list" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match std::fs::read_dir(path_str) {
                Ok(entries) => {
                    let items: Vec<serde_json::Value> = entries.filter_map(|e| e.ok()).map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        serde_json::json!({
                            "name": name,
                            "is_dir": is_dir,
                            "path": entry.path().to_string_lossy().into_owned(),
                        })
                    }).collect();
                    RpcResponse::success(items)
                }
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "fs_mkdir" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            match tokio::fs::create_dir_all(path_str).await {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "fs_roots_list" => {
            let roots = vec![std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".to_string())];
            RpcResponse::success(roots)
        }

        // --- Secrets & Vault Commands (G-30) ---
        "secrets_get" => {
            let key = payload.args.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let env_val = std::env::var(key).or_else(|_| std::env::var(format!("IKENGA_SECRET_{key}"))).ok();
            RpcResponse::success(env_val)
        }
        "secrets_set" => {
            RpcResponse::success(true)
        }

        // --- Unknown Command Fallback ---
        other => {
            debug!("Unimplemented or pass-through RPC command: {other}");
            RpcResponse::error(format!("Command '{other}' not implemented in headless daemon"))
        }
    };

    Json(res)
}
