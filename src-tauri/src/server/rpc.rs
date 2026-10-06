use axum::extract::State;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::debug;

use super::rpc_claude;
use super::rpc_files;
use super::rpc_local;
use super::rpc_shell;
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
            match resolve_path(&state, path_str) {
                Ok(path) => RpcResponse::success(path.exists()),
                Err(e) => RpcResponse::error(e),
            }
        }
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

        // --- Chi reads, agent-ops files, identity (WP-19 slice 3) ---
        //
        // Also bodies in `server::rpc_local`, over `server::shared::{chi,
        // agent_ops, identity}` — the cores the desktop commands call. The chi
        // reads need `--data-dir`; the agent-ops arms resolve the router's home
        // (single-user seam, G-PRINCIPAL / WP-20). Everything that spawns
        // (`chi_run` / `chi_resume` / `chi_cancel`, `agent_ops_run_now`) stays
        // desktop-only.
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
        // notifications also needs the settings home. No events are emitted
        // (no event channel here). The project filesystem arms stay inside the
        // fs allowlist and the project root. Left desktop-only:
        // `notifications_record_update` (its sweep needs the shell version +
        // pkg kernel), `comment_route` (spawns chi). `pin_screenshot_write`
        // joined in slice 8 (below).
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
        // must pass the fs allowlist. Left allowlisted: the git / npx
        // installers and updaters (they spawn, WP-18b) and `oba_install_local`
        // (an unconfined read source) — see `desktop_only.toml`.
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

        // --- fs family + actions / keybindings / trust (WP-19 slice 5a) ---
        //
        // Bodies in `server::rpc_files`, over `server::shared::{fs, actions}`
        // — the cores the desktop commands call. Every caller path goes
        // through the fs allowlist (`PathGuard`, the `fs_read` / `fs_write`
        // boundary); a project's `.ikenga/` files are reached only when its
        // root is inside it. No `actions://changed` is emitted (no event
        // channel). Writes never touch the trust record, which lives in
        // `--data-dir`. Left allowlisted: `fs_trash` (OS trash outside the
        // allowlist), `fs_roots_*` (would let the token holder redefine the
        // boundary), `fs_watch` / `fs_unwatch` (`/ws/fs` covers them),
        // `actions_open_file` (spawns the OS opener).
        "fs_read" => rpc_files::fs_read(&state, &payload.args).await,
        "fs_write" => rpc_files::fs_write(&state, &payload.args).await,
        "fs_trash" => rpc_files::fs_trash(&state, &payload.args).await,
        "fs_list" => rpc_files::fs_list(&state, &payload.args).await,
        "fs_kind" => rpc_files::fs_kind(&state, &payload.args).await,
        "fs_mime" => rpc_files::fs_mime(&state, &payload.args),
        "fs_search" => rpc_files::fs_search(&state, &payload.args).await,
        "fs_rename" => rpc_files::fs_rename(&state, &payload.args).await,
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
        // allowlist and refused inside the daemon's own state). No
        // `pa-action-*` events (no event channel). Commit / retry wake the
        // mutation worker with the hardcoded `mutation:send-worker` only —
        // `agent_ops_run_now` itself stays allowlisted.
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
        | "permission_decide"
        | "permission_relay_put"
        | "permission_relay_take"
        | "permission_relay_resolve"
        | "notifications_record_access"
        | "share_project_info") => {
            crate::access::rpc::serve_daemon(access.as_deref(), ctx.as_ref(), cmd, &payload.args)
                .await
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
