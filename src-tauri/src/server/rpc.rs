use axum::extract::State;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;

use super::hook_asks;
use super::rpc_claude;
use super::rpc_exec;
use super::rpc_files;
use super::rpc_fs_roots;
use super::rpc_local;
use super::rpc_seats;
use super::rpc_shell;
use super::term_hooks;
use super::AppState;
use crate::pty::SpawnOpts;

/// Resolve a caller-supplied path against the user's FS allowlist — the same
/// `fs_roots` boundary `commands::fs` enforces for the desktop app, so a
/// remote client can reach exactly what a local one can and nothing more —
/// and refuse the daemon's own state (`--data-dir`, the discovery file)
/// whatever that allowlist covers (see `server::reserved`).
///
/// `resolve_allowlisted` requires the path's parent to exist, which `mkdir -p`
/// of a deep new chain does not satisfy, so this goes through
/// `PathGuard::resolve_deep`: walk up to the nearest existing ancestor, check
/// *that*, then re-attach the tail. One choke point with the other fs arms.
fn resolve_path(state: &AppState, input: &str) -> Result<std::path::PathBuf, String> {
    state.path_guard.resolve_deep(input)
}

/// Returned when the daemon was started without `--data-dir`, so there is no
/// `ikenga.db` to open. Says which flag is missing rather than "unknown
/// command", which would read as unimplemented.
pub(super) const NO_DB: &str =
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

/// The frontend's `VaultScope` (`src/lib/tauri-cmd.ts`), whose wire shape is
/// `{ kind: "workspace" } | { kind: "project", id } | { kind: "pkg", id }` —
/// the desktop's `Scope`, decoded the same way (`crate::secrets::scope`).
///
/// Whether a project or pkg scope is servable is the secrets layer's call
/// (`crate::secrets_env::DaemonSecrets`): a T1 principal store serves all
/// three, the T0 operator-default namespace only `workspace`.
pub(super) fn scope_kind(args: &Value) -> Result<crate::secrets::scope::Scope, String> {
    let scope = args
        .get("scope")
        .ok_or("scope is required, e.g. {\"kind\":\"workspace\"}")?;
    serde_json::from_value(scope.clone()).map_err(|e| format!("invalid scope: {e}"))
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
    /// A typed rejection's own fields, for a command whose desktop `invoke`
    /// rejects with an object rather than a string (the seats' `SeatError`,
    /// `{code, message, details?}`). The web transport assigns them onto the
    /// `Error` it throws, so the frontend reads the same rejection on both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_data: Option<Value>,
}

impl RpcResponse {
    pub fn success(data: impl Serialize) -> Self {
        Self {
            ok: true,
            data: serde_json::to_value(data).ok(),
            error: None,
            error_data: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self::error_with_data(msg, None)
    }

    /// [`RpcResponse::error`] carrying the typed rejection (see `error_data`).
    pub fn error_with_data(msg: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
            error_data: data,
        }
    }
}

pub async fn rpc_handler(
    State(state): State<Arc<AppState>>,
    access: Option<Extension<Arc<crate::access::DaemonAccess>>>,
    ctx: Option<Extension<crate::access::AccessCtx>>,
    Json(payload): Json<RpcRequest>,
) -> impl IntoResponse {
    debug!("RPC request: cmd={}", payload.cmd);

    // G-ACCESS §9.2 pre-hook: class + caps for this command (§1.6), and in
    // share mode a narrowed `AppState` (WP-76). One call, before any arm.
    let access = access.map(|Extension(a)| a);
    let ctx = ctx.map(|Extension(c)| c);
    let state =
        match crate::access::rpc_prehook(&state, ctx.as_ref(), &payload.cmd, &payload.args).await {
            crate::access::PreHook::Proceed => state,
            crate::access::PreHook::Narrowed(narrowed) => narrowed,
            crate::access::PreHook::Answered(res) => return Json(res),
        };

    let res = match payload.cmd.as_str() {
        // --- PTY Commands ---
        "pty_spawn" => {
            // `terminalId` is what `tauri-cmd.ts` sends (Tauri does the camel -> snake conversion
            // on the desktop; nothing does it here). `terminal_id` is kept for older callers.
            // Reading only the snake spelling dropped the id, so a browser terminal got a random
            // one and could not be found again after a reload.
            let terminal_id = payload
                .args
                .get("terminalId")
                .or_else(|| payload.args.get("terminal_id"))
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

            // The per-terminal claude hook settings (`server::term_hooks`):
            // the frontend already put `--settings <path>` in `cmd`, so the
            // file has to exist before the child execs. A path the daemon
            // cannot honestly serve fails the spawn with the reason, rather
            // than starting a claude that dies on a missing settings file.
            let settings_path = payload
                .args
                .get("settingsPath")
                .or_else(|| payload.args.get("settings_path"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let grant = match (&settings_path, &terminal_id) {
                (Some(path), Some(term_id)) => {
                    match state.term_hooks.wire(&state.config, term_id, path) {
                        Ok(grant) => Some(grant),
                        Err(e) => {
                            return Json(crate::access::postfilter(
                                ctx.as_ref(),
                                &payload.cmd,
                                RpcResponse::error(format!("pty_spawn: {e}")),
                            ))
                        }
                    }
                }
                _ => None,
            };

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
                Ok(pty_id) => {
                    if let Some(grant) = grant {
                        // The secret dies with the terminal.
                        let hooks = state.term_hooks.clone();
                        let pty = state.pty_manager.clone();
                        let id = pty_id.clone();
                        tokio::spawn(async move {
                            pty.wait_for_exit(&id).await;
                            hooks.revoke(&grant);
                        });
                    }
                    RpcResponse::success(serde_json::json!({ "pty_id": pty_id }))
                }
                Err(e) => {
                    if let Some(grant) = grant {
                        state.term_hooks.revoke(&grant);
                    }
                    RpcResponse::error(e.to_string())
                }
            }
        }
        // Claude terminal hooks for the browser (gap audit rank 11): where the
        // per-terminal settings live (or why there are none), the HUD's
        // snapshots, and the permission inbox's answer to a held gate. Not
        // Tauri commands — the desktop reaches these through the iyke bridge.
        "term_hooks_info" => term_hooks::info_arm(&state),
        "term_hooks_statusline_snapshot" => term_hooks::snapshot_arm(&state),
        "term_hooks_decide" => {
            hook_asks::term_hooks_decide(&state, access.as_deref(), ctx.as_ref(), &payload.args)
                .await
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
        // Refusals fold into `false`, as on the desktop (`rpc_files`).
        "fs_exists" => rpc_files::fs_exists(&state, &payload.args).await,
        "fs_mkdir" => {
            let path_str = payload
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            match resolve_path(&state, path_str) {
                Ok(path) => match tokio::fs::create_dir_all(&path).await {
                    Ok(_) => RpcResponse::success(true),
                    Err(e) => RpcResponse::error(e.to_string()),
                },
                Err(e) => RpcResponse::error(e),
            }
        }
        // The caller's own folder list (gap audit 2026-10-06 rank 1): under
        // T1 this child's principal's, seeded with its home; on T0 the one
        // owner's. Validation, scoping and the admin route in
        // `server::rpc_fs_roots`.
        "fs_roots_list" => rpc_fs_roots::fs_roots_list(&state, &payload.args),
        "fs_roots_add" => rpc_fs_roots::fs_roots_add(&state, &payload.args),
        "fs_roots_remove" => rpc_fs_roots::fs_roots_remove(&state, &payload.args),
        "fs_roots_reset" => rpc_fs_roots::fs_roots_reset(&state, &payload.args),
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

        // --- Pkg kernel read parity (WP-19) ---
        //
        // Same `assemble_status` the desktop `Kernel::status` calls, over the
        // daemon's read-only `--pkgs-dir` index, with ONLY the registry the
        // daemon runs (`ui_routes`). `registries.ui_routes` is always present,
        // even empty — the FE pkg route resolver reads `.entries` off it.
        // No args. See `server::pkg_index` for how each row is filled.
        "pkg_kernel_status" => RpcResponse::success(state.pkg_index.status()),
        // Resolved against the daemon's own index. An unknown id returns `[]`,
        // not an error — mirroring the desktop command, which returns
        // `Vec::new()` for a pkg that isn't installed.
        //
        // `store_root()` resolves from the DAEMON PROCESS's env (HOME /
        // XDG_DATA_HOME): one Ngwa store for every caller. A single-user seam
        // for WP-20 (G-PRINCIPAL), same as `fs_home`.
        "list_skill_actions" => {
            // `pkgId` is what `tauri-cmd.ts` sends (no camel → snake
            // conversion here); `pkg_id` too, as for `pkg_content_html`.
            let pkg_id = payload
                .args
                .get("pkgId")
                .or_else(|| payload.args.get("pkg_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if pkg_id.is_empty() {
                return Json(RpcResponse::error(
                    "list_skill_actions: `pkgId` is required",
                ));
            }
            let store = crate::pkg::skill_actions::store_root();
            RpcResponse::success(state.pkg_index.skill_actions(pkg_id, store.as_deref()))
        }
        "list_all_skill_actions" => {
            let store = crate::pkg::skill_actions::store_root();
            RpcResponse::success(state.pkg_index.all_skill_actions(store.as_deref()))
        }
        "pkg_activity_bar_set_badge" => {
            let pkg_id = payload
                .args
                .get("pkgId")
                .or_else(|| payload.args.get("pkg_id"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if pkg_id.is_empty() {
                return Json(RpcResponse::error("pkg_activity_bar_set_badge: `pkgId` is required"));
            }
            // Absent or null clears the badge; anything else must parse.
            let badge: Option<crate::pkg::registries::ActivityBarBadge> =
                match payload.args.get("badge").filter(|v| !v.is_null()) {
                    None => None,
                    Some(v) => match serde_json::from_value(v.clone()) {
                        Ok(b) => Some(b),
                        Err(e) => {
                            return Json(RpcResponse::error(format!(
                                "pkg_activity_bar_set_badge: invalid `badge`: {e}"
                            )))
                        }
                    },
                };
            // Kept in memory and reported through `pkg_kernel_status`'s
            // `activity_bar` registry; the server has no desktop event to emit.
            match state.pkg_index.set_badge(pkg_id, badge) {
                Ok(true) => RpcResponse::success(()),
                Ok(false) => RpcResponse::error(format!("no activity-bar entry for pkg `{pkg_id}`")),
                Err(e) => RpcResponse::error(format!("{e:#}")),
            }
        }
        // The server has no install-time trust gate, so nothing is ever
        // parked for a capability review.
        "pkg_trust_list_pending" => RpcResponse::success(Vec::<serde_json::Value>::new()),
        // No trust store, so no pkg's trust can be evaluated. An empty list
        // would read as "nothing to trust"; the refusal names why, and the
        // frontend renders it as "not available on this server".
        "pkg_trust_list" => RpcResponse::error(format!(
            "pkg_trust_list: {}",
            rpc_claude::TRUST_NOT_SERVED
        )),
        // What the daemon can see: `--pkgs-dir` entries that failed to load,
        // are api-incompatible, or a registry rejected — plus one
        // `records_unavailable` row saying install-record health is not
        // checked here, so the answer is never a bare `[]` that reads as
        // healthy. Removal (`pkg_health_remove*`) stays desktop-only.
        "pkg_health_scan" => RpcResponse::success(state.pkg_index.health_scan()),
        // Elevated trust (`host.fetch`, `host.invoke`) is granted on the
        // desktop only, and the server runs neither, so the answer here is
        // always no: the app reports the capability as unavailable instead
        // of failing on an unknown command.
        "pkg_is_trusted_for_elevated" => RpcResponse::success(false),

        // --- Secrets & Vault Commands (G-30; per-principal store, WP-21) ---
        //
        // Two layers: a T1 principal child's own store (`<data>/secrets/`,
        // sealed under a key its broker derived for it alone) over the
        // operator-opted-in `IKENGA_SECRET_*` default. A T0 daemon has only
        // the default: read-only, workspace scope only, as before. Layer
        // order, the fail-closed rule, the PTY denylist interaction and the
        // operator runbook live in `crate::secrets_env` — read that before
        // changing anything below. Bodies in `rpc_local`.
        "secrets_get" => rpc_local::secrets_get(&state, &payload.args),
        "secrets_list_keys" => rpc_local::secrets_list_keys(&state),
        // Without this arm Settings → API Keys / Integrations / Secrets and
        // every connector probe are dead in a browser session: they all gate
        // on `available`, and the unknown-command fallthrough throws.
        "secrets_vault_status" => RpcResponse::success(state.secrets.status()),
        "secrets_get_scoped" => rpc_local::secrets_get_scoped(&state, &payload.args),
        "secrets_list_keys_scoped" => rpc_local::secrets_list_keys_scoped(&state, &payload.args),
        // Writes land in the principal's own store. Without one (T0) each is
        // an explicit refusal, NOT the unknown-command fallthrough, which
        // would read as "unfinished": `secrets_env::WRITE_REFUSAL` is the
        // operator runbook.
        cmd @ ("secrets_set"
        | "secrets_delete"
        | "secrets_set_scoped"
        | "secrets_delete_scoped") => rpc_local::secrets_write(&state, cmd, &payload.args),
        // Desktop: the names in `secrets-index.json`. Daemon: every name of
        // both layers — same `string[]` shape, never a value.
        "secrets_index_names" => rpc_local::secrets_index_names(&state),
        // The operator default's names alone (review WP76-RV1): how the
        // browser tells "your override" from a bare key of your own. A
        // browser-only verb — the desktop keychain has no default layer.
        "secrets_default_names" => rpc_local::secrets_default_names(&state),
        // No passphrase layer (DEC-R18-1: the key is server-held, so
        // background work reads secrets while the user is signed out). T1:
        // `configured: true, locked: false`, `secrets_lock` answers that
        // state, setting / unlocking a passphrase is refused with why. T0:
        // the unknown-command error, byte-identical to before WP-21, so
        // Settings → Secrets stays read-only there.
        cmd @ ("secrets_lock_state"
        | "secrets_lock"
        | "secrets_set_passphrase"
        | "secrets_unlock") => rpc_local::secrets_lock_family(&state, cmd),

        // --- Local state (WP-19 slice 2) ---
        //
        // Bodies live in `server::rpc_local`, over the same cores the desktop
        // commands call (`server::shared::*`, `pkg::settings_values`), rooted
        // at `--data-dir`. Each errors, naming the flag, without one.
        "supabase_config_get" => rpc_local::supabase_config_get(&state),
        "supabase_config_set" => rpc_local::supabase_config_set(&state, &payload.args),
        "supabase_config_clear" => rpc_local::supabase_config_clear(&state),
        "settings_get" => rpc_local::settings_get(&state, &payload.args).await,
        "settings_set" => rpc_local::settings_set(&state, &payload.args).await,
        "settings_get_all" => rpc_local::settings_get_all(&state).await,
        "settings_clear_all" => rpc_local::settings_clear_all(&state).await,
        "settings_read_file" => rpc_local::settings_read_file(&state, &payload.args).await,
        "settings_write_field" => rpc_local::settings_write_field(&state, &payload.args).await,
        "data_health_scan" => rpc_local::data_health_scan(&state).await,
        "data_health_db_size" => rpc_local::data_health_db_size(&state),
        "backup_list" => rpc_local::backup_list(&state),
        "backup_delete" => rpc_local::backup_delete(&state, &payload.args),
        "pkg_settings_get" => rpc_local::pkg_settings_get(&state, &payload.args).await,

        // --- Chi runs, agent-ops files, identity (WP-19 slice 3, WP-P10) ---
        //
        // Also bodies in `server::rpc_local`, over `server::shared::{chi,
        // chi_exec, agent_ops, identity}` — the cores the desktop commands
        // call. Every chi arm needs `--data-dir`. The write arms (WP-P10)
        // spawn and signal through `executor::current()`, so under T1 — where
        // this handler runs in the signed-in principal's child — a run
        // executes as that principal, against that principal's own ikenga.db
        // and chi-cache. The agent-ops arms resolve the router's home
        // (single-user seam, G-PRINCIPAL / WP-20); `agent_ops_run_now` is
        // served below with the executor-routed arms.
        "chi_run" => rpc_local::chi_run(&state, &payload.args).await,
        "chi_resume" => rpc_local::chi_resume(&state, &payload.args).await,
        "chi_cancel" => rpc_local::chi_cancel(&state, &payload.args).await,
        "chi_status" => rpc_local::chi_status(&state, &payload.args).await,
        "chi_list" => rpc_local::chi_list(&state, &payload.args).await,
        "agent_ops_list_jobs" => rpc_local::agent_ops_list_jobs(&state).await,
        "agent_ops_tail_run" => rpc_local::agent_ops_tail_run(&state, &payload.args).await,
        "agent_ops_upsert_job" => rpc_local::agent_ops_upsert_job(&state, &payload.args).await,
        "agent_ops_delete_job" => rpc_local::agent_ops_delete_job(&state, &payload.args).await,
        "agent_ops_set_enabled" => rpc_local::agent_ops_set_enabled(&state, &payload.args).await,
        "os_username" => rpc_local::os_username(),

        // --- Shell state: notifications, projects, pins, comments, studio
        //     threads (WP-19 slice 4) ---
        //
        // Bodies in `server::rpc_shell`, over `server::shared::{notifications,
        // projects, activity_bar, comments, studio_threads}` — the cores the
        // desktop commands call. All need `--data-dir`; the mute half of
        // notifications also needs the settings home. Events go out on the
        // daemon's bus (`server::events`, `/ws/events`). The project filesystem arms stay inside the
        // fs allowlist and the project root. Left desktop-only:
        // `notifications_record_update` (its sweep needs the shell version +
        // pkg kernel). `pin_screenshot_write` joined in slice 8, and
        // `comment_route` with the executor-routed arms (both below).
        "notifications_list" => rpc_shell::notifications_list(&state, &payload.args).await,
        "notifications_unread_count" => rpc_shell::notifications_unread_count(&state).await,
        "notifications_mark_read" => {
            rpc_shell::notifications_mark_read(&state, &payload.args).await
        }
        "notifications_mark_all_read" => {
            rpc_shell::notifications_mark_all_read(&state, &payload.args).await
        }
        "notifications_mute_state" => rpc_shell::notifications_mute_state(&state).await,
        "notifications_mute_kind" => {
            rpc_shell::notifications_mute_kind(&state, &payload.args).await
        }
        "notifications_unmute_kind" => {
            rpc_shell::notifications_unmute_kind(&state, &payload.args).await
        }
        "project_list" => rpc_shell::project_list(&state, &payload.args).await,
        "project_get_active" => rpc_shell::project_get_active(&state).await,
        "project_create" => rpc_shell::project_create(&state, &payload.args).await,
        "project_update" => rpc_shell::project_update(&state, &payload.args).await,
        "project_archive" => rpc_shell::project_archive(&state, &payload.args).await,
        "project_set_active" => rpc_shell::project_set_active(&state, &payload.args).await,
        "project_inventory" => rpc_shell::project_inventory(&state, &payload.args).await,
        "project_skills_list" => rpc_shell::project_skills_list(&state, &payload.args).await,
        "project_artifacts_walk" => rpc_shell::project_artifacts_walk(&state, &payload.args).await,
        "project_scaffold_claude" => rpc_shell::project_scaffold_claude(&state, &payload.args),
        "activity_sections_list" => rpc_shell::activity_sections_list(&state).await,
        "activity_sections_create" => {
            rpc_shell::activity_sections_create(&state, &payload.args).await
        }
        "activity_sections_update" => {
            rpc_shell::activity_sections_update(&state, &payload.args).await
        }
        "activity_sections_remove" => {
            rpc_shell::activity_sections_remove(&state, &payload.args).await
        }
        "activity_pins_list" => rpc_shell::activity_pins_list(&state).await,
        "activity_pins_add" => rpc_shell::activity_pins_add(&state, &payload.args).await,
        "activity_pins_resolve_artifact" => {
            rpc_shell::activity_pins_resolve_artifact(&state, &payload.args).await
        }
        "activity_pins_touch_open" => {
            rpc_shell::activity_pins_touch_open(&state, &payload.args).await
        }
        "activity_pins_remove" => rpc_shell::activity_pins_remove(&state, &payload.args).await,
        "activity_pins_reorder" => rpc_shell::activity_pins_reorder(&state, &payload.args).await,
        "comment_create" => rpc_shell::comment_create(&state, &payload.args).await,
        "comment_get" => rpc_shell::comment_get(&state, &payload.args).await,
        "comment_list" => rpc_shell::comment_list(&state, &payload.args).await,
        "comment_record_routing" => rpc_shell::comment_record_routing(&state, &payload.args).await,
        "comment_set_status" => rpc_shell::comment_set_status(&state, &payload.args).await,
        "comment_delete" => rpc_shell::comment_delete(&state, &payload.args).await,
        "studio_thread_get_or_create" => {
            rpc_shell::studio_thread_get_or_create(&state, &payload.args).await
        }
        "studio_thread_get" => rpc_shell::studio_thread_get(&state, &payload.args).await,
        "studio_thread_list_recent" => {
            rpc_shell::studio_thread_list_recent(&state, &payload.args).await
        }
        "studio_thread_delete" => rpc_shell::studio_thread_delete(&state, &payload.args).await,
        "studio_message_append" => rpc_shell::studio_message_append(&state, &payload.args).await,
        "studio_message_list" => rpc_shell::studio_message_list(&state, &payload.args).await,

        // --- Claude config / assets / sessions + detection (WP-19 slice 5b) ---
        //
        // Bodies in `server::rpc_claude`, over `server::shared::{claude_config,
        // claude_sessions, settings_cascade, agent_config, agent_projects,
        // engine_layout, shell_detect}` — the cores the desktop commands call.
        // `~/.claude`, `~/.claude/projects` and the Ngwa store are the router
        // home's (single-user seam, G-PRINCIPAL / WP-20); caller paths are
        // confined to the fs allowlist, session logs to `~/.claude/projects`.
        // Nothing spawns and nothing is watched (no event channel): the
        // `claude_config_watch` pair, discovery, the store, the primitives and
        // the probing detectors stay in `desktop_only.toml`.
        "claude_config_load" => rpc_claude::claude_config_load(&state, &payload.args).await,
        "claude_config_read_file" => {
            rpc_claude::claude_config_read_file(&state, &payload.args).await
        }
        "claude_config_resolve_cascade" => {
            rpc_claude::claude_config_resolve_cascade(&state, &payload.args)
        }
        "claude_asset_pin" => rpc_claude::claude_asset_pin(&state, &payload.args).await,
        "claude_asset_unpin" => rpc_claude::claude_asset_unpin(&state, &payload.args).await,
        "claude_asset_list_pins" => rpc_claude::claude_asset_list_pins(&state, &payload.args).await,
        "claude_list_sessions" => rpc_claude::claude_list_sessions(&state, &payload.args).await,
        "claude_read_jsonl" => rpc_claude::claude_read_jsonl(&state, &payload.args).await,
        "claude_session_list" => rpc_claude::claude_session_list(&state, &payload.args).await,
        "detect_agent" => rpc_claude::detect_agent(&payload.args).await,
        "detect_agents" => rpc_claude::detect_agents().await,
        "detect_agent_config" => rpc_claude::detect_agent_config(&state, &payload.args),
        "list_claude_projects" => rpc_claude::list_claude_projects(&state).await,
        "list_agent_projects" => rpc_claude::list_agent_projects(&state, &payload.args).await,
        "engine_layout" => rpc_claude::engine_layout(),
        "terminal_detect_shells" => rpc_claude::terminal_detect_shells(),

        // --- Ngwa vault: store, primitives, Ọba registry (WP-19 slice 7) ---
        //
        // Bodies in `server::rpc_claude`, over `server::shared::claude_store`
        // — the `*_in` bodies the desktop commands call — against the router
        // home and store (G-PRINCIPAL single-user seam: under topology B each
        // principal's daemon has its own HOME, so its own vault). Confined:
        // no symlink planted in a scope can turn a copy, write or delete into
        // one outside the vault or inside `--data-dir`; relink / unlink name
        // only placements in a known scope; an import source and a new master
        // must pass the fs allowlist. The git / npx installers and updaters
        // (WP-18b part c) spawn through `executor::current()` — as the
        // signed-in account under T1 — under `claude_store::remote`'s policy
        // (https-only public sources, scrubbed env, deadlines, vetted trees).
        // Left allowlisted: `oba_install_local` (an unconfined read source) —
        // see `desktop_only.toml`.
        "claude_store_list" => rpc_claude::claude_store_list(&state, &payload.args).await,
        "claude_store_import" => rpc_claude::claude_store_import(&state, &payload.args).await,
        "claude_primitive_enable" => {
            rpc_claude::claude_primitive_enable(&state, &payload.args).await
        }
        "claude_primitive_enable_for" => {
            rpc_claude::claude_primitive_enable_for(&state, &payload.args).await
        }
        "claude_primitive_disable" => {
            rpc_claude::claude_primitive_disable(&state, &payload.args).await
        }
        "claude_primitive_disable_for" => {
            rpc_claude::claude_primitive_disable_for(&state, &payload.args).await
        }
        "claude_primitive_remove" => {
            rpc_claude::claude_primitive_remove(&state, &payload.args).await
        }
        "claude_primitive_remove_for" => {
            rpc_claude::claude_primitive_remove_for(&state, &payload.args).await
        }
        "claude_primitive_copy" => rpc_claude::claude_primitive_copy(&state, &payload.args).await,
        "claude_primitive_move" => rpc_claude::claude_primitive_move(&state, &payload.args).await,
        "claude_primitive_copy_batch" => {
            rpc_claude::claude_primitive_copy_batch(&state, &payload.args).await
        }
        "oba_backfill_registry" => rpc_claude::oba_backfill_registry(&state).await,
        "oba_dependents" => rpc_claude::oba_dependents(&state, &payload.args).await,
        "oba_forget" => rpc_claude::oba_forget(&state, &payload.args),
        "oba_missing_requires" => rpc_claude::oba_missing_requires(&state, &payload.args).await,
        "oba_safe_delete" => rpc_claude::oba_safe_delete(&state, &payload.args).await,
        "oba_set_auto_update" => rpc_claude::oba_set_auto_update(&state, &payload.args).await,
        "oba_relink_dependents" => rpc_claude::oba_relink_dependents(&state, &payload.args).await,
        "oba_unlink_one" => rpc_claude::oba_unlink_one(&state, &payload.args).await,
        "oba_install_git" => rpc_claude::oba_install_git(&state, &payload.args).await,
        "oba_install_npx" => rpc_claude::oba_install_npx(&state, &payload.args).await,
        "oba_install_bundle" => rpc_claude::oba_install_bundle(&state, &payload.args).await,
        "oba_install_with_deps" => rpc_claude::oba_install_with_deps(&state, &payload.args).await,
        "oba_resolve_source" => rpc_claude::oba_resolve_source(&state, &payload.args).await,
        "oba_check_update" => rpc_claude::oba_check_update(&state, &payload.args).await,
        "oba_update" => rpc_claude::oba_update(&state, &payload.args).await,
        "oba_auto_update_all" => rpc_claude::oba_auto_update_all(&state, &payload.args).await,

        // --- Ngwa snapshot (WP-19) ---
        //
        // The desktop's own join over the projects, `--pkgs-dir` index,
        // config scan and Ọba store this router can see, read as its
        // principal (router home, router store). Pkg runtime, engine-asset
        // placements, trust and transcript usage are reported unavailable in
        // `sources`, never as an empty or zeroed set. Body in `rpc_claude`.
        "ngwa_snapshot" => rpc_claude::ngwa_snapshot(&state).await,

        // --- fs family + actions / keybindings / trust (WP-19 slice 5a) ---
        //
        // Bodies in `server::rpc_files`, over `server::shared::{fs, actions}`
        // — the cores the desktop commands call. Every caller path goes
        // through the fs allowlist (`PathGuard`, the `fs_read` / `fs_write`
        // boundary); a project's `.ikenga/` files are reached only when its
        // root is inside it. `actions://changed` goes out on the event bus
        // (`server::events`). Writes never touch the trust record, which lives in
        // `--data-dir`. Left allowlisted: `fs_trash` (OS trash outside the
        // allowlist), `fs_watch` / `fs_unwatch` (`/ws/fs` covers them),
        // `actions_open_file` (spawns the OS opener).
        "fs_read" => rpc_files::fs_read(&state, &payload.args).await,
        "fs_write" => rpc_files::fs_write(&state, &payload.args).await,
        "fs_trash" => rpc_files::fs_trash(&state, &payload.args).await,
        "fs_list" => rpc_files::fs_list(&state, &payload.args).await,
        "fs_kind" => rpc_files::fs_kind(&state, &payload.args).await,
        "fs_mime" => rpc_files::fs_mime(&state, &payload.args),
        "fs_search" => rpc_files::fs_search(&state, &payload.args).await,
        "fs_rename" => rpc_files::fs_rename(&state, &payload.args).await,
        "viewer_serve" => {
            let pid = ctx.as_ref().map(|c| *c.principal_id.as_uuid());
            rpc_files::viewer_serve(&state, &payload.args, pid).await
        }
        "viewer_stop" => rpc_files::viewer_stop(&state, &payload.args).await,
        "actions_read_files" => rpc_files::actions_read_files(&state, &payload.args).await,
        "actions_write" => rpc_files::actions_write(&state, &payload.args).await,
        "keybindings_write" => rpc_files::keybindings_write(&state, &payload.args).await,
        "actions_trust_status" => rpc_files::actions_trust_status(&state, &payload.args).await,
        "actions_trust_grant" => rpc_files::actions_trust_grant(&state, &payload.args).await,
        "actions_trust_revoke" => rpc_files::actions_trust_revoke(&state, &payload.args).await,

        // --- Approve gate, atelier files, `{{branch}}`, pkg DB audit /
        //     diagnostics (WP-19 slice 6) ---
        //
        // Bodies in `server::rpc_local` (over `server::shared::{pa_actions,
        // pkg_db}`, the daemon's `ikenga.db`) and `server::rpc_files` (over
        // `server::shared::{atelier, git}`, every caller root through the fs
        // allowlist and refused inside the daemon's own state). `pa-action-*`
        // go out on the event bus (`server::events`). Commit / retry wake the
        // mutation worker with the hardcoded `mutation:send-worker` only —
        // `agent_ops_run_now` is its own arm (below), confined to the
        // principal's own jobs.
        "pa_actions_pause" => rpc_local::pa_actions_pause(&state, &payload.args).await,
        "pa_actions_list" => rpc_local::pa_actions_list(&state, &payload.args).await,
        "pa_actions_update" => rpc_local::pa_actions_update(&state, &payload.args).await,
        "pa_actions_commit" => rpc_local::pa_actions_commit(&state, &payload.args).await,
        "pa_actions_retry" => rpc_local::pa_actions_retry(&state, &payload.args).await,
        "pa_actions_reject" => rpc_local::pa_actions_reject(&state, &payload.args).await,
        "pkg_permission_violations_list" => {
            rpc_local::pkg_permission_violations_list(&state, &payload.args).await
        }
        "pkg_permission_violations_clear" => {
            rpc_local::pkg_permission_violations_clear(&state, &payload.args).await
        }
        "pkg_db_diag" => rpc_local::pkg_db_diag(&state).await,
        "atelier_file_read" => rpc_files::atelier_file_read(&state, &payload.args),
        "atelier_file_write" => rpc_files::atelier_file_write(&state, &payload.args),
        "action_git_branch" => rpc_files::action_git_branch(&state, &payload.args).await,
        "git_status" => rpc_files::git_status(&state, &payload.args).await,

        // --- Pin screenshots, agent-config scaffold, pkg manifest / workspace
        //     / scaffold helpers (WP-19 slice 8) ---
        //
        // Bodies in `server::rpc_shell` (over `server::shared::{comments,
        // agent_scaffold}`) and `server::rpc_files` (over `server::shared::
        // {pkg_workspace, pkg_scaffold}`). `pin_screenshot_write` writes only
        // under the daemon's own `<data-dir>/pin-screenshots/` with a minted
        // name, capped in size; every caller path in the rest goes through
        // the fs allowlist and is refused inside the daemon's state, and every
        // write goes through `shared::confined_fs` (no symlink followed, live
        // or dangling). None needs the live pkg kernel; nothing spawns, no
        // events.
        "pin_screenshot_write" => rpc_shell::pin_screenshot_write(&state, &payload.args),
        "scaffold_agent_config" => rpc_shell::scaffold_agent_config(&state, &payload.args),
        "pkg_preview_manifest" => rpc_files::pkg_preview_manifest(&state, &payload.args),
        "pkg_discover_workspace" => rpc_files::pkg_discover_workspace(&state, &payload.args),
        "pkg_scaffold" => rpc_files::pkg_scaffold(&state, &payload.args).await,

        // --- Chi seats (gap audit 2026-10-06 rank 7) ---
        //
        // The desktop's seat store (`shared::seats`) over `--data-dir`'s
        // ikenga.db; resume / fill / the §4.5 queue start their runs through
        // `chi_exec` like `chi_run`. Bodies and the daemon's world in
        // `server/rpc_seats.rs`.
        "seats_list" => rpc_seats::seats_list(&state, &payload.args).await,
        "seats_get" => rpc_seats::seats_get(&state, &payload.args).await,
        "seats_engines" => rpc_seats::seats_engines(&state).await,
        "seats_resolve" => rpc_seats::seats_resolve(&state, &payload.args).await,
        "seats_create" => rpc_seats::seats_create(&state, &payload.args).await,
        "seats_move" => rpc_seats::seats_move(&state, &payload.args).await,
        "seats_resume" => rpc_seats::seats_resume(&state, &payload.args).await,
        "seats_fill" => rpc_seats::seats_fill(&state, &payload.args).await,
        "seats_queue" => rpc_seats::seats_queue(&state, &payload.args).await,
        "seats_clear" => rpc_seats::seats_clear(&state, &payload.args).await,
        "seats_rename" => rpc_seats::seats_rename(&state, &payload.args).await,
        "seats_remove" => rpc_seats::seats_remove(&state, &payload.args).await,
        "seats_release" => rpc_seats::seats_release(&state, &payload.args).await,

        // --- Executor-routed arms + the pkg-settings write (gap audit
        //     2026-10-06 ranks 21, 23, 20-partial) ---
        //
        // Bodies in `server::rpc_exec`, over the cores the desktop commands
        // call (`server::shared::{sidecar_call, action_exec, comment_route,
        // agent_ops}`, `pkg::settings_values`). Every spawn goes through
        // `executor::current()`: under T1 this handler runs in the signed-in
        // principal's child, so a sidecar, action or pin run executes as that
        // principal, against that principal's home, ikenga.db and PTYs. A
        // sidecar resolves only from the `--pkgs-dir` index and only inside
        // its pkg; an action or pin cwd only inside the fs allowlist; run-now
        // only for the principal's own job through the principal's own
        // agent-ops daemon; a setting only for a declared key. The kernel
        // verbs (`pkg_set_enabled`, `pkg_uninstall`, `pkg_supervisor_restart`)
        // stay in `desktop_only.toml`: the daemon has no kernel to run them.
        "pkg_sidecar_call" => rpc_exec::pkg_sidecar_call(&state, &payload.args).await,
        "action_exec" => rpc_exec::action_exec(&state, &payload.args).await,
        "comment_route" => rpc_exec::comment_route(&state, &payload.args).await,
        "agent_ops_run_now" => rpc_exec::agent_ops_run_now(&state, &payload.args).await,
        "pkg_settings_set" => rpc_exec::pkg_settings_set(&state, &payload.args).await,

        // `permission_decide` on a daemon (T0 or a T1 child): the decide core
        // with the hook resolver beside the relay's, audited where this tier
        // audits (`server::hook_asks`). Every other §9.1 arm follows.
        "permission_decide" => {
            hook_asks::permission_decide(&state, access.as_deref(), ctx.as_ref(), &payload.args)
                .await
        }

        // --- G-ACCESS §9.1 (WP-74a, skeleton-first §9.2) ---
        //
        // Every access arm, the permission decide core, the T0 ask relay and
        // the two broker → child `internal` arms. Bodies in `crate::access::
        // rpc`; the arms later waves own answer `internal: not implemented
        // (WP-NN)` (the relay: `invalid_request`) until filled. Class and
        // caps were already checked by the pre-hook above. A principal child
        // answers `access_*` with `served_by_broker`.
        cmd @ ("access_status"
        | "access_devices_list"
        | "access_device_set_tier"
        | "access_device_revoke"
        | "access_pair_begin"
        | "access_pair_cancel"
        | "access_pair_pending"
        | "access_pair_decide"
        | "access_routing_get"
        | "access_routing_set"
        | "access_members_list"
        | "access_member_set_role"
        | "access_member_remove"
        | "access_member_restore"
        | "access_policy_get"
        | "access_policy_set_cell"
        | "access_policy_set_owner_approval"
        | "access_invite_issue"
        | "access_invite_revoke"
        | "access_shares_list"
        | "access_audit_list"
        | "access_audit_verify"
        | "access_audit_export"
        | "access_audit_record_local"
        | "access_audit_reseal"
        | "access_push_config"
        | "access_push_subscribe"
        | "access_push_update"
        | "access_push_unsubscribe"
        | "access_push_list"
        | "access_push_test"
        | "server_health"
        | "permission_relay_put"
        | "permission_relay_take"
        | "permission_relay_resolve"
        | "notifications_record_access"
        | "share_project_info") => {
            crate::access::rpc::serve_daemon(access.as_deref(), ctx.as_ref(), cmd, &payload.args)
                .await
        }

        // --- WP-P9: in-app updates ---
        //
        // `internal` (only the T1 broker's own call reaches it): how many
        // terminals a restart of this process would end. Served by T0 and
        // principal children alike; nothing else.
        //
        // The admin Server card (`server::host_health`) asks the same arm for
        // `claude_procs`: how many processes of this child's own uid are a
        // `claude` (a count — never a name, argument or another uid's).
        "server_open_terminals" => {
            let claude_procs = tokio::task::spawn_blocking(|| {
                super::host_health::count_own_processes("claude")
            })
            .await
            .ok()
            .flatten();
            RpcResponse::success(serde_json::json!({
                "open": state.pty_manager.active_session_count(),
                "claude_procs": claude_procs,
            }))
        }

        // --- Unknown Command Fallback ---
        other => {
            debug!("Unimplemented or pass-through RPC command: {other}");
            RpcResponse::error(format!(
                "Command '{other}' not implemented in headless daemon"
            ))
        }
    };

    // G-ACCESS §9.2 post-hook: share filtering (WP-76) and the permission
    // rows' `can_decide` / `waiting_on` (WP-75).
    Json(crate::access::postfilter(ctx.as_ref(), &payload.cmd, res))
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
