use std::sync::Arc;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

use super::AppState;
use crate::pty::SpawnOpts;

/// Resolve a caller-supplied path against the user's FS allowlist — the same
/// `fs_roots` boundary `commands::fs` enforces for the desktop app, so a
/// remote client can reach exactly what a local one can and nothing more.
///
/// `resolve_allowlisted` requires the path's parent to exist, which `mkdir -p`
/// of a deep new chain does not satisfy; walk up to the nearest existing
/// ancestor, check *that* against the allowlist, then re-attach the tail.
fn resolve_path(input: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, PathBuf};

    if input.is_empty() {
        return Err("path is required".to_string());
    }
    if let Ok(p) = crate::commands::resolve_allowlisted(input) {
        return Ok(p);
    }

    let expanded = shellexpand::full(input)
        .map(|c| c.into_owned())
        .map_err(|e| format!("expand path: {e}"))?;
    let abs = {
        let p = PathBuf::from(&expanded);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()
                .map_err(|e| format!("current_dir: {e}"))?
                .join(p)
        }
    };
    // `..` would let a canonicalized-ancestor check be re-escaped by the tail
    // we re-attach, so refuse it outright rather than try to normalise it.
    if abs.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path may not contain `..`".to_string());
    }

    let mut ancestor = abs.as_path();
    let existing = loop {
        if ancestor.exists() {
            break ancestor;
        }
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return Err(format!("path outside allowlist: {}", abs.display())),
        }
    };
    let canonical_existing = existing
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", existing.display()))?;
    let tail = abs
        .strip_prefix(existing)
        .map_err(|_| "failed to resolve path".to_string())?;
    let resolved = canonical_existing.join(tail);

    let roots = crate::fs_roots::current().ok_or("fs_roots not initialized")?;
    if !roots.is_allowed(&resolved) {
        return Err(format!("path outside allowlist: {}", resolved.display()));
    }
    Ok(resolved)
}

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
            match resolve_path(path_str) {
                Ok(path) => RpcResponse::success(path.exists()),
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_read" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            match resolve_path(path_str) {
                Ok(path) => match tokio::fs::read_to_string(&path).await {
                    Ok(content) => RpcResponse::success(content),
                    Err(e) => RpcResponse::error(e.to_string()),
                },
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_write" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let content = payload.args.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            match resolve_path(path_str) {
                Ok(path) => {
                    if let Some(parent) = path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    match tokio::fs::write(&path, content).await {
                        Ok(_) => RpcResponse::success(true),
                        Err(e) => RpcResponse::error(e.to_string()),
                    }
                }
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_list" => {
            let path_str = payload.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = match resolve_path(path_str) {
                Ok(p) => p,
                Err(e) => return Json(RpcResponse::error(e)),
            };
            match std::fs::read_dir(path) {
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
            match resolve_path(path_str) {
                Ok(path) => match tokio::fs::create_dir_all(&path).await {
                    Ok(_) => RpcResponse::success(true),
                    Err(e) => RpcResponse::error(e.to_string()),
                },
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_roots_list" => {
            let roots = crate::fs_roots::current()
                .map(|r| r.list_inputs())
                .unwrap_or_default();
            RpcResponse::success(roots)
        }

        // --- Secrets & Vault Commands (G-30) ---
        //
        // Not wired to Stronghold yet. Reads are restricted to the
        // `IKENGA_SECRET_*` namespace the operator opts into: taking the key
        // as a bare env var name made this a remote `printenv` for every
        // credential in the daemon's environment.
        "secrets_get" => {
            let key = payload.args.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                RpcResponse::error("secrets_get: invalid key")
            } else {
                RpcResponse::success(std::env::var(format!("IKENGA_SECRET_{key}")).ok())
            }
        }
        "secrets_set" => {
            RpcResponse::error("secrets_set is not implemented in the headless daemon")
        }

        // --- Unknown Command Fallback ---
        other => {
            debug!("Unimplemented or pass-through RPC command: {other}");
            RpcResponse::error(format!("Command '{other}' not implemented in headless daemon"))
        }
    };

    Json(res)
}
