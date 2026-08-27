use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
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
    if let Ok(p) = crate::path_allow::resolve_allowlisted(input) {
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

/// Returned when the daemon was started without `--data-dir`, so there is no
/// `ikenga.db` to open. Says which flag is missing rather than "unknown
/// command", which would read as unimplemented.
const NO_DB: &str =
    "no database: the daemon was started without --data-dir, so there is no ikenga.db to open";

/// Pull `(sql, params)` out of an RPC payload, accepting **both** spellings the
/// frontend uses.
///
/// This is not defensive coding, it is a real fork in the frontend:
///
/// * `src/lib/tauri-cmd.ts` (`dbQuery` / `dbExec`) sends `{sql, params}` — the
///   Tauri command's own argument names. Used by viewer recents, the home
///   widgets, and the pkg-iframe `host.dbQuery` / `host.dbExec` bridge.
/// * `src/lib/transport/sql-shim.ts` (`SqlDbWebProxy`) sends `{query, values}`
///   — the `@tauri-apps/plugin-sql` argument names, because it stands in for
///   that package. Used by `layout-state`, `sql-db`, and the terminal
///   `session-store`.
///
/// Both reach this one command name. Honouring only one spelling would leave
/// the other half of the app looking like an empty database rather than an
/// error — so accept both, and keep accepting both.
fn db_args(args: &Value) -> Result<(String, Vec<Value>), String> {
    let sql = args
        .get("sql")
        .or_else(|| args.get("query"))
        .and_then(|v| v.as_str())
        .ok_or("`sql` (or `query`) is required")?
        .to_string();
    let params = args
        .get("params")
        .or_else(|| args.get("values"))
        .map(|v| match v {
            Value::Array(a) => Ok(a.clone()),
            Value::Null => Ok(Vec::new()),
            _ => Err("`params` (or `values`) must be an array".to_string()),
        })
        .transpose()?
        .unwrap_or_default();
    Ok((sql, params))
}

/// The `kind` discriminant of the frontend's `VaultScope`
/// (`src/lib/tauri-cmd.ts`), whose wire shape is
/// `{ kind: "workspace" } | { kind: "project", id } | { kind: "pkg", id }`.
///
/// The daemon only distinguishes workspace from everything else, because its
/// secret namespace is flat — see `crate::secrets_env`.
enum ScopeKind {
    Workspace,
    Other(String),
}

fn scope_kind(args: &Value) -> Result<ScopeKind, String> {
    let kind = args
        .get("scope")
        .and_then(|s| s.get("kind"))
        .and_then(|k| k.as_str())
        .ok_or("scope is required, e.g. {\"kind\":\"workspace\"}")?;
    Ok(match kind {
        "workspace" => ScopeKind::Workspace,
        other => ScopeKind::Other(other.to_string()),
    })
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
            let terminal_id = payload
                .args
                .get("terminal_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let title = payload
                .args
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let cwd = payload
                .args
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();
            let cmd: Vec<String> = payload
                .args
                .get("cmd")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_else(|| {
                    if cfg!(windows) {
                        vec!["powershell.exe".to_string()]
                    } else {
                        vec!["/bin/bash".to_string()]
                    }
                });
            let rows = payload
                .args
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(24) as u16;
            let cols = payload
                .args
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(80) as u16;
            // The caller's env was accepted and then dropped on the floor.
            let env: std::collections::HashMap<String, String> = payload
                .args
                .get("env")
                .and_then(|v| v.as_object())
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();

            match state
                .pty_manager
                .spawn_headless(SpawnOpts {
                    terminal_id,
                    title,
                    cwd,
                    cmd,
                    env,
                    rows,
                    cols,
                })
                .await
            {
                Ok(pty_id) => RpcResponse::success(serde_json::json!({ "pty_id": pty_id })),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_write" => {
            let id = payload
                .args
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let data = payload
                .args
                .get("data")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match state.pty_manager.write(id, data.as_bytes()) {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_resize" => {
            let id = payload
                .args
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let rows = payload
                .args
                .get("rows")
                .and_then(|v| v.as_u64())
                .unwrap_or(24) as u16;
            let cols = payload
                .args
                .get("cols")
                .and_then(|v| v.as_u64())
                .unwrap_or(80) as u16;
            match state.pty_manager.resize(id, rows, cols) {
                Ok(_) => RpcResponse::success(true),
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "pty_kill" => {
            let id = payload
                .args
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
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
            let id = payload
                .args
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            RpcResponse::success(state.pty_manager.foreground(id))
        }
        "pty_foreground_snapshot" => RpcResponse::success(state.pty_manager.foreground_snapshot()),

        // --- FS Commands ---
        "fs_exists" => {
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match resolve_path(path_str) {
                Ok(path) => RpcResponse::success(path.exists()),
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_read" => {
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match resolve_path(path_str) {
                Ok(path) => match tokio::fs::read_to_string(&path).await {
                    Ok(content) => RpcResponse::success(content),
                    Err(e) => RpcResponse::error(e.to_string()),
                },
                Err(e) => RpcResponse::error(e),
            }
        }
        "fs_write" => {
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let content = payload
                .args
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
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
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let path = match resolve_path(path_str) {
                Ok(p) => p,
                Err(e) => return Json(RpcResponse::error(e)),
            };
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let items: Vec<serde_json::Value> = entries
                        .filter_map(|e| e.ok())
                        .map(|entry| {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                            serde_json::json!({
                                "name": name,
                                "is_dir": is_dir,
                                "path": entry.path().to_string_lossy().into_owned(),
                            })
                        })
                        .collect();
                    RpcResponse::success(items)
                }
                Err(e) => RpcResponse::error(e.to_string()),
            }
        }
        "fs_mkdir" => {
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
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
        // The browser has no `@tauri-apps/api/path`, so `homeDir()` resolves
        // here. Without it the shim silently returns the literal string "~",
        // which then gets joined into paths and handed to `fs_read` — a
        // failure that looks like a missing file rather than a missing RPC.
        "fs_home" => match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            Ok(home) if !home.is_empty() => RpcResponse::success(home),
            _ => RpcResponse::error("fs_home: no HOME/USERPROFILE in the daemon environment"),
        },
        // --- SQLite (WP-12b / G-41, ikenga#100) ---
        //
        // Backed by `crate::db`, the same module the desktop `#[tauri::command]`
        // wrappers call, so the read-only guard and the row-to-JSON conversion
        // are literally the same code on both surfaces.
        "db_query" => match state.pa_db.as_deref() {
            Some(db) => {
                let (sql, params) = match db_args(&payload.args) {
                    Ok(v) => v,
                    Err(e) => return Json(RpcResponse::error(format!("db_query: {e}"))),
                };
                match crate::db::query_json(db, &sql, &params).await {
                    Ok(rows) => RpcResponse::success(rows),
                    Err(e) => RpcResponse::error(e),
                }
            }
            None => RpcResponse::error(NO_DB),
        },
        "db_exec" => match state.pa_db.as_deref() {
            Some(db) => {
                let (sql, params) = match db_args(&payload.args) {
                    Ok(v) => v,
                    Err(e) => return Json(RpcResponse::error(format!("db_exec: {e}"))),
                };
                match crate::db::exec(db, &sql, &params).await {
                    Ok(res) => RpcResponse::success(res),
                    Err(e) => RpcResponse::error(e),
                }
            }
            None => RpcResponse::error(NO_DB),
        },

        // --- Pkg iframe mount (WP-12b / W4) ---
        //
        // `<PkgIframeHost>` calls this on mount and puts `html` in the
        // iframe's `srcdoc`. `supabase` / `secrets` are omitted — there is no
        // vault here — and `buildHostContext` already treats both as optional,
        // which is why a capability-free pkg needs none of the vault work.
        // The refusals for pkgs that DO require them live in `mint_html`.
        "pkg_content_html" => {
            // `pkgId` is what `tauri-cmd.ts` sends (Tauri does the camel →
            // snake conversion on the desktop side; nothing does it here).
            // `pkg_id` is accepted too so a curl'd probe or a future caller
            // using the Rust spelling isn't silently told the pkg is unknown.
            let pkg_id = payload
                .args
                .get("pkgId")
                .or_else(|| payload.args.get("pkg_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if pkg_id.is_empty() {
                return Json(RpcResponse::error("pkg_content_html: `pkgId` is required"));
            }
            // The manifest route's `source`, e.g. `dist/index.html`. Defaulted
            // rather than required: every pkg-pattern template emits exactly
            // that, and a missing `source` should mount the entry document,
            // not fail.
            let source = payload
                .args
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("index.html");
            match state.pkg_static.mint_html(pkg_id, source) {
                Ok(handle) => RpcResponse::success(handle),
                Err(e) => RpcResponse::error(e),
            }
        }
        // Nothing to revoke: the daemon mints no per-mount credential (the
        // bearer token is the whole boundary — see `server::pkg_static`). This
        // is a deliberate success, not the unknown-command fallthrough: the
        // frontend calls it on every iframe unmount and swallows the result
        // with `.catch(() => {})`, so a refusal here would be an invisible
        // error on a path that is working exactly as designed.
        "pkg_content_revoke" => RpcResponse::success(true),

        // --- Secrets & Vault Commands (G-30) ---
        //
        // There is no vault in the daemon and that is decided, not pending:
        // every server-side reader of a secret is desktop-gated, so an
        // encrypted store here would exist only to be read back out over the
        // bearer-token boundary. The daemon serves the flat, operator-opted-in
        // `IKENGA_SECRET_*` namespace instead. Rationale, the PTY denylist
        // interaction, and the operator runbook all live in
        // `crate::secrets_env` — read that before changing anything below.
        "secrets_get" => {
            let key = payload
                .args
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match crate::secrets_env::get(key) {
                Ok(value) => RpcResponse::success(value),
                Err(e) => RpcResponse::error(format!("secrets_get: {e}")),
            }
        }
        "secrets_list_keys" => RpcResponse::success(crate::secrets_env::list_keys()),
        // Without this arm Settings → API Keys / Integrations / Secrets and
        // every connector probe are dead in a browser session: they all gate
        // on `available`, and the unknown-command fallthrough throws.
        "secrets_vault_status" => RpcResponse::success(crate::secrets_env::status()),

        // Scoped reads: the env namespace is flat, which IS workspace scope.
        // Project and pkg partitions exist only in the desktop vault, so they
        // get a refusal that says which, rather than an empty list that reads
        // like "you have no secrets there".
        "secrets_get_scoped" => match scope_kind(&payload.args) {
            Ok(ScopeKind::Workspace) => {
                let key = payload
                    .args
                    .get("key")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                match crate::secrets_env::get(key) {
                    Ok(value) => RpcResponse::success(value),
                    Err(e) => RpcResponse::error(format!("secrets_get_scoped: {e}")),
                }
            }
            Ok(ScopeKind::Other(kind)) => RpcResponse::error(format!(
                "secrets_get_scoped: scope {kind:?} is not servable — {}",
                crate::secrets_env::SCOPE_REFUSAL
            )),
            Err(e) => RpcResponse::error(format!("secrets_get_scoped: {e}")),
        },
        "secrets_list_keys_scoped" => match scope_kind(&payload.args) {
            Ok(ScopeKind::Workspace) => RpcResponse::success(crate::secrets_env::list_keys()),
            Ok(ScopeKind::Other(kind)) => RpcResponse::error(format!(
                "secrets_list_keys_scoped: scope {kind:?} is not servable — {}",
                crate::secrets_env::SCOPE_REFUSAL
            )),
            Err(e) => RpcResponse::error(format!("secrets_list_keys_scoped: {e}")),
        },

        // Writes. Explicit refusal arms, NOT the unknown-command fallthrough:
        // the fallthrough reads as "unfinished, someone will get to it", and
        // the next person to read it would implement the thing this decision
        // rejects. `secrets_env::WRITE_REFUSAL` is the operator runbook.
        cmd @ ("secrets_set" | "secrets_delete" | "secrets_set_scoped"
        | "secrets_delete_scoped") => RpcResponse::error(format!(
            "{cmd} {}",
            crate::secrets_env::WRITE_REFUSAL
        )),

        // --- Unknown Command Fallback ---
        other => {
            debug!("Unimplemented or pass-through RPC command: {other}");
            RpcResponse::error(format!(
                "Command '{other}' not implemented in headless daemon"
            ))
        }
    };

    Json(res)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The two frontend spellings are a real fork, not a hypothetical: half
    /// the app sends `{sql, params}` (`tauri-cmd.ts`) and half sends
    /// `{query, values}` (`transport/sql-shim.ts`). Both must land.
    #[test]
    fn db_args_accepts_both_frontend_spellings() {
        let tauri_cmd = json!({ "sql": "SELECT 1", "params": [1, "x"] });
        let sql_shim = json!({ "query": "SELECT 1", "values": [1, "x"] });

        let a = db_args(&tauri_cmd).expect("tauri-cmd.ts spelling");
        let b = db_args(&sql_shim).expect("sql-shim.ts spelling");
        assert_eq!(a.0, "SELECT 1");
        assert_eq!(a, b, "both spellings must decode identically");
    }

    /// Omitted bind lists are normal (`dbQuery(sql)` with no params) and must
    /// not be an error; an explicit `null` is the same thing.
    #[test]
    fn db_args_defaults_missing_params_to_empty() {
        let (sql, params) = db_args(&json!({ "sql": "SELECT 1" })).expect("no params");
        assert_eq!(sql, "SELECT 1");
        assert!(params.is_empty());

        let (_, params) =
            db_args(&json!({ "query": "SELECT 1", "values": null })).expect("null values");
        assert!(params.is_empty());
    }

    #[test]
    fn db_args_rejects_missing_sql_and_non_array_params() {
        assert!(db_args(&json!({ "params": [] })).is_err());
        assert!(db_args(&json!({ "sql": "SELECT 1", "params": "nope" })).is_err());
    }

    /// `db_exec`'s success payload is destructured in TS as
    /// `SqlQueryResult { rowsAffected, lastInsertId }`. snake_case here would
    /// read as `undefined` on both fields with no error anywhere.
    #[test]
    fn exec_result_serializes_camel_case() {
        let wire = serde_json::to_value(crate::db::ExecResult {
            rows_affected: 3,
            last_insert_id: 42,
        })
        .expect("serialize");
        assert_eq!(wire, json!({ "rowsAffected": 3, "lastInsertId": 42 }));
    }
}
