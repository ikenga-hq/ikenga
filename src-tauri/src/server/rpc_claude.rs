//! `/api/rpc` bodies for the Claude config / session / detection reads served
//! in WP-19 slice 5b: the `/claude` config browser (`claude_config_load`,
//! `claude_config_read_file`, `claude_config_resolve_cascade`), the asset
//! pins, the on-disk session logs (`claude_list_sessions`,
//! `claude_read_jsonl`, `claude_session_list`), the wizard's project /
//! config listers, `engine_layout` and `terminal_detect_shells`.
//!
//! Same house pattern as `rpc_local` / `rpc_shell`: the arm *names* stay in
//! `rpc.rs`'s dispatch `match` (the parity ratchet reads them there); every
//! body calls the core the desktop `#[tauri::command]` calls
//! (`server::shared::{claude_config, claude_sessions, settings_cascade,
//! agent_config, agent_projects, engine_layout, shell_detect}`) and returns
//! the same serialized type. Arguments are decoded with `rpc_shell::targ`
//! from the camelCase key `tauri-cmd.ts` sends or the snake_case spelling.
//!
//! **Whose home (G-PRINCIPAL / WP-20).** Every read of `~/.claude`,
//! `~/.claude/projects`, `~/.gemini`, `~/.codex` and the Ngwa store is the
//! router home's — the daemon PROCESS's home in production (`platform::
//! home_dir`), one for every token holder. That is correct under G-PRINCIPAL
//! #310's topology B, where each principal's daemon runs as that uid with its
//! own HOME (T1); a shared multi-principal daemon would have to resolve all of
//! it per principal. The Ngwa store root (`pkg::skill_actions::store_root`)
//! is the same seam, resolved from the process env.
//!
//! **Caller paths.** Every path a caller supplies — a project root, a config
//! file, a cascade project / overlay dir, a wizard root — must be absolute
//! (or `~/…`, resolved against the router home), `..`-free, and canonicalize
//! inside the daemon's fs allowlist (`<data-dir>/fs_roots.json`, the `fs_*`
//! arms' boundary; see [`confine`]). `claude_config_read_file` additionally
//! admits the router home's `~/.claude` and the Ngwa store, which is where
//! that command's files live. Session logs are read only from inside
//! `~/.claude/projects`: a session id is never a path, and a log that
//! canonicalizes outside that dir is refused (`claude_sessions::read_session`).
//!
//! **The Ngwa vault (WP-19 slice 7).** The store / primitive / Ọba registry
//! arms (`claude_store_*`, `claude_primitive_*`, `oba_*`) run the desktop's
//! bodies (`server::shared::claude_store`) against the router home and store,
//! confined to the vault — see the section comment above those arms.
//!
//! **No events, nothing spawned.** The desktop's `claude-config:changed`
//! watchers (`claude_config_watch` / `_unwatch`) stay desktop-only: there is
//! no event channel here. Nothing in this module starts a process; on Windows
//! `terminal_detect_shells` would (`wsl.exe -l -q`), so there it refuses.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::rpc::RpcResponse;
use super::rpc_local::{pa_db, respond};
use super::rpc_shell::targ;
use super::shared::claude_config::{self, ClaudeConfig, ScanError};
use super::shared::claude_sessions::{self, projects_root_in};
use super::shared::claude_store::{self, DaemonChecks, Vault};
use super::shared::projects::FsReach;
use super::shared::{agent_config, agent_projects, engine_layout, settings_cascade, shell_detect};
use super::AppState;

/// The desktop's own error for "no home to resolve `~/.claude` against".
const NO_HOME: &str = "HOME unset";

// ─── caller paths ────────────────────────────────────────────────────────────

/// A caller-supplied path as the daemon will consider it: `~` / `~/…`
/// resolved against the router home, otherwise absolute as given. Relative
/// paths (which would resolve against the daemon's cwd) and any `..` are
/// refused outright, so a canonical-ancestor check below cannot be re-escaped.
fn caller_path(state: &AppState, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("path is required".to_string());
    }
    let path = if raw == "~" || raw.starts_with("~/") {
        let home = state.home.as_deref().ok_or(NO_HOME)?;
        home.join(raw.trim_start_matches('~').trim_start_matches('/'))
    } else {
        PathBuf::from(raw)
    };
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {raw}"));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path may not contain `..`: {raw}"));
    }
    Ok(path)
}

/// Confine `path` to the fs allowlist. `Some(canonical)` when it exists and
/// canonicalizes inside; `None` when it does not exist but its nearest
/// existing ancestor is inside (so there is nothing under it to read, and the
/// desktop's answer — nothing found — is the honest one); an error otherwise.
fn confine(state: &AppState, path: &Path) -> Result<Option<PathBuf>, String> {
    if let Ok(canonical) = path.canonicalize() {
        state.path_guard.check(&canonical)?;
        return Ok(Some(canonical));
    }
    let mut ancestor = path;
    loop {
        match ancestor.parent() {
            Some(parent) => ancestor = parent,
            None => return Err(format!("path outside allowlist: {}", path.display())),
        }
        if let Ok(canonical) = ancestor.canonicalize() {
            state.path_guard.check(&canonical)?;
            return Ok(None);
        }
    }
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// `<router home>/.claude/projects`, or the desktop's "HOME unset". G-PRINCIPAL
/// single-user seam: the daemon process's home, one for every token holder
/// (correct under topology B / T1, where each principal runs its own daemon).
fn projects_root(state: &AppState) -> Result<PathBuf, String> {
    state
        .home
        .as_deref()
        .map(projects_root_in)
        .ok_or_else(|| NO_HOME.to_string())
}

// ─── Claude config browser ───────────────────────────────────────────────────

/// The desktop's scan over the router home, with each project root confined
/// first. A root outside the allowlist is not scanned; it is reported in the
/// result's `errors[]` (the per-root error list the desktop already surfaces
/// in the UI footer), so one stale root does not blank the whole page. A root
/// that does not exist is skipped silently, as the desktop's scan skips a
/// root with no `.claude/`. Admitted roots are scanned by canonical path.
pub(super) async fn claude_config_load(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_roots: Vec<String> = targ(args, &["projectRoots", "project_roots"])?;
        let mut admitted = Vec::new();
        let mut refused = Vec::new();
        for raw in project_roots {
            match caller_path(state, &raw).and_then(|p| confine(state, &p)) {
                Ok(Some(canonical)) => admitted.push(lossy(&canonical)),
                Ok(None) => {}
                Err(message) => refused.push(ScanError { path: raw, message }),
            }
        }
        let home = state.home.clone();
        // G-PRINCIPAL seam: the process-resolved Ngwa store (see module doc).
        let store = crate::pkg::skill_actions::store_root();
        let mut config: ClaudeConfig = tokio::task::spawn_blocking(move || {
            claude_config::scan_all_in(admitted, home.as_deref(), store.as_deref())
        })
        .await
        .map_err(|e| format!("join failed: {e}"))?
        .map_err(|e| e.to_string())?;
        config.errors.extend(refused);
        Ok(config)
    }
    .await;
    respond("claude_config_load", r)
}

/// The desktop reads any path with a `.claude` segment. The daemon keeps that
/// rule on the path as given and additionally requires the file to
/// canonicalize inside the fs allowlist, the router home's `~/.claude`, or
/// the Ngwa store (a skill's supporting file is often a symlink into it).
pub(super) async fn claude_config_read_file(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let raw: String = targ(args, &["path"])?;
        let path = caller_path(state, &raw)?;
        if !claude_config::is_under_claude_dir(&path) {
            return Err(format!("path not under a .claude/ dir: {}", path.display()));
        }
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("read failed: {e}"))?;
        // Whichever base admits it, never the daemon's own state.
        state.path_guard.check_reserved(&canonical)?;
        let under = |base: Option<PathBuf>| {
            base.and_then(|b| b.canonicalize().ok())
                .is_some_and(|b| canonical.starts_with(b))
        };
        let admitted = state.path_guard.check(&canonical).is_ok()
            || under(state.home.as_ref().map(|h| h.join(".claude")))
            // G-PRINCIPAL seam: the process-resolved Ngwa store.
            || under(crate::pkg::skill_actions::store_root());
        if !admitted {
            return Err(format!(
                "path outside the fs allowlist, ~/.claude and the Ngwa store: {}",
                canonical.display()
            ));
        }
        tokio::fs::read_to_string(&canonical)
            .await
            .map_err(|e| format!("read failed: {e}"))
    }
    .await;
    respond("claude_config_read_file", r)
}

/// The desktop's 5-tier merge. The user tier is the router home's
/// `~/.claude/settings.json`; the managed tier is `/etc/claude/settings.json`,
/// as on the desktop. The caller's project and overlay dirs are confined; one
/// that does not exist contributes no tier, which is what the desktop finds.
pub(super) fn claude_config_resolve_cascade(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let project_dir: Option<String> = targ(args, &["projectDir", "project_dir"])?;
        let overlay_dir: Option<String> = targ(args, &["overlayDir", "overlay_dir"])?;
        let confine_dir = |raw: Option<String>| -> Result<Option<PathBuf>, String> {
            match raw {
                Some(raw) => confine(state, &caller_path(state, &raw)?),
                None => Ok(None),
            }
        };
        let project_dir = confine_dir(project_dir)?;
        let overlay_dir = confine_dir(overlay_dir)?;
        let user = state
            .home
            .as_ref()
            .map(|h| h.join(".claude").join("settings.json"));
        Ok(settings_cascade::resolve_settings_cascade_in(
            Path::new(settings_cascade::MANAGED_SETTINGS),
            user.as_deref(),
            project_dir.as_deref(),
            overlay_dir.as_deref(),
        ))
    })();
    respond("claude_config_resolve_cascade", r)
}

// ─── Asset pins ──────────────────────────────────────────────────────────────
//
// Rows in the daemon's `--data-dir` `ikenga.db` (`claude_asset_preferences`),
// through the same validate-then-write cores as the desktop.

pub(super) async fn claude_asset_pin(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope: String = targ(args, &["scope"])?;
        let asset_kind: String = targ(args, &["assetKind", "asset_kind"])?;
        let asset_name: String = targ(args, &["assetName", "asset_name"])?;
        let preferred_tier: String = targ(args, &["preferredTier", "preferred_tier"])?;
        let preferred_source: Option<String> =
            targ(args, &["preferredSource", "preferred_source"])?;
        claude_config::asset_pin(
            pa_db(state)?,
            scope,
            asset_kind,
            asset_name,
            preferred_tier,
            preferred_source,
        )
        .await
    }
    .await;
    respond("claude_asset_pin", r)
}

pub(super) async fn claude_asset_unpin(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope: String = targ(args, &["scope"])?;
        let asset_kind: String = targ(args, &["assetKind", "asset_kind"])?;
        let asset_name: String = targ(args, &["assetName", "asset_name"])?;
        claude_config::asset_unpin(pa_db(state)?, scope, asset_kind, asset_name).await
    }
    .await;
    respond("claude_asset_unpin", r)
}

pub(super) async fn claude_asset_list_pins(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let scope: String = targ(args, &["scope"])?;
        claude_config::asset_list_pins(pa_db(state)?, scope).await
    }
    .await;
    respond("claude_asset_list_pins", r)
}

// ─── Session logs ────────────────────────────────────────────────────────────

/// The desktop's two-phase listing over the router home's
/// `~/.claude/projects`, confined to it. `projectDir` only ever selects a
/// slug dir by name (it is never joined onto the root).
pub(super) async fn claude_list_sessions(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_dir: Option<String> = targ(args, &["projectDir", "project_dir"])?;
        let limit: Option<usize> = targ(args, &["limit"])?;
        let root = projects_root(state)?;
        tokio::task::spawn_blocking(move || {
            claude_sessions::list_sessions(&root, project_dir.as_deref(), limit, FsReach::Confined)
        })
        .await
        .map_err(|e| format!("join failed: {e}"))?
    }
    .await;
    respond("claude_list_sessions", r)
}

/// One transcript, by session id, from inside `~/.claude/projects` only.
pub(super) async fn claude_read_jsonl(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let session_id: String = targ(args, &["sessionId", "session_id"])?;
        let root = projects_root(state)?;
        tokio::task::spawn_blocking(move || claude_sessions::read_session(&root, &session_id))
            .await
            .map_err(|e| format!("join failed: {e}"))?
    }
    .await;
    respond("claude_read_jsonl", r)
}

/// The session browser's enumeration. Without a home the desktop's scan finds
/// no `~/.claude/projects` and answers `[]`; so does this.
pub(super) async fn claude_session_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_slug: Option<String> = targ(args, &["projectSlug", "project_slug"])?;
        let Some(root) = state.home.as_deref().map(projects_root_in) else {
            return Ok(Vec::new());
        };
        tokio::task::spawn_blocking(move || {
            claude_sessions::enumerate_sessions(&root, project_slug.as_deref(), FsReach::Confined)
        })
        .await
        .map_err(|e| format!("join failed: {e}"))
    }
    .await;
    respond("claude_session_list", r)
}

// ─── Wizard detection / layout ───────────────────────────────────────────────

/// Counts under the caller's `rootPath` (confined) plus the router home's
/// global counts. `root_path` echoes the caller's spelling, as the desktop's
/// does; the counting happens under the canonical root.
pub(super) fn detect_agent_config(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let agent_id: String = targ(args, &["agentId", "agent_id"])?;
        let root_path: String = targ(args, &["rootPath", "root_path"])?;
        let path = caller_path(state, &root_path)?;
        let walk = confine(state, &path)?.unwrap_or(path);
        let mut inv = agent_config::build_inventory_in(&agent_id, &walk, state.home.as_deref());
        inv.root_path = root_path;
        Ok(inv)
    })();
    respond("detect_agent_config", r)
}

/// No caller path: the router home's `~/.claude/projects` (+ WSL on Windows).
/// No home = no projects, as on the desktop.
pub(super) async fn list_claude_projects(state: &AppState) -> RpcResponse {
    let home = state.home.clone();
    let r = tokio::task::spawn_blocking(move || {
        agent_projects::list_claude_projects_in(home.as_deref())
    })
    .await
    .map_err(|e| format!("join failed: {e}"));
    respond("list_claude_projects", r)
}

/// `~/.gemini/antigravity/brain` / `~/.codex/sessions` / `~/.claude/projects`
/// under the router home (G-PRINCIPAL single-user seam, see the module doc).
pub(super) async fn list_agent_projects(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let agent_id: String = targ(args, &["agentId", "agent_id"])?;
        let home = state.home.clone();
        tokio::task::spawn_blocking(move || {
            agent_projects::list_agent_projects_in(&agent_id, home.as_deref())
        })
        .await
        .map_err(|e| format!("join failed: {e}"))
    }
    .await;
    respond("list_agent_projects", r)
}

/// The frozen per-engine descriptor — static data, no args.
pub(super) fn engine_layout() -> RpcResponse {
    RpcResponse::success(engine_layout::engine_layouts())
}

/// The host's shells, detected on the daemon's host. On Unix this is fixed
/// path probes plus `$SHELL` (the daemon process's). On Windows detection
/// runs `wsl.exe -l -q`, a spawn outside the session executor, so it refuses.
pub(super) fn terminal_detect_shells() -> RpcResponse {
    #[cfg(not(windows))]
    {
        RpcResponse::success(shell_detect::detect_shells())
    }
    #[cfg(windows)]
    {
        let _ = shell_detect::detect_shells;
        RpcResponse::error(
            "terminal_detect_shells: not served on a Windows daemon — WSL distro \
             detection runs `wsl.exe -l -q`, which is not routed through the session \
             executor (WP-18b)",
        )
    }
}

// ─── The Ngwa vault: store, primitives, Ọba registry (WP-19 slice 7) ─────────
//
// The bodies are `server::shared::claude_store`'s `*_in` functions — the ones
// the desktop's `#[tauri::command]`s call — run against a daemon `Vault`:
//
// * **Whose vault (G-PRINCIPAL / WP-20).** The router home (the `workspace`
//   scope, the user-tier engine dirs) and `state.store` (the daemon PROCESS's
//   `store_root()` in production): single-user seam — under topology B each
//   principal's daemon has its own HOME and so its own vault. The merge
//   engine's user-tier settings files (`~/.claude.json`, `~/.codex/…`) resolve
//   against the process home, which is the router home in production.
// * **Confined** (`claude_store::confine`): the desktop trusts symlinks because
//   its caller is the user's own renderer on the same uid; a token holder is
//   not. Every copy source (and every entry of a copied skill dir) must
//   resolve inside the vault — the store, or a known scope's `.claude` /
//   `.agents` / `.gemini` / `.codex` — and outside the daemon's data dir;
//   every node created, replaced or deleted must sit there too; a relink /
//   unlink names only placements in a known scope's dependents-scan dirs; an
//   import source and a relink's new master must pass the fs allowlist
//   (`PathGuard`). Deletes stay `lstat`-first: a link is unlinked, never
//   followed. Caller paths expand only `~`, never `$VAR`.
// * **Needs `--data-dir`.** Each arm whose desktop command takes the
//   database, plus relink / unlink (whose confinement needs the scope list),
//   refuses without one. `claude_store_import`, `oba_forget` and
//   `oba_set_auto_update` touch only the store.
// * **Not served:** the git / npx installers, `oba_update`,
//   `oba_check_update`, `oba_auto_update_all` (they spawn; WP-18b) and
//   `oba_install_local` (an unconfined, symlink-following read source with no
//   FE caller) — see `desktop_only.toml`.

/// Bind `$v` to the daemon's [`Vault`] for `$state`: its router home and
/// store, confined by its `PathGuard` (the checks borrow `$state`).
macro_rules! daemon_vault {
    ($state:expr, $v:ident) => {
        let reserved = |p: &Path| $state.path_guard.check_reserved(p);
        let allowlisted = |p: &Path| $state.path_guard.check(p);
        let $v = Vault::daemon(
            $state.home.clone(),
            $state.store.clone(),
            DaemonChecks {
                reserved: &reserved,
                allowlisted: &allowlisted,
            },
        );
    };
}

pub(super) async fn claude_store_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: Option<String> = targ(args, &["kind"])?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_store_list_in(&v, db, kind).await
    }
    .await;
    respond("claude_store_list", r)
}

pub(super) async fn claude_store_import(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: String = targ(args, &["kind"])?;
        let name: String = targ(args, &["name"])?;
        let source_path: String = targ(args, &["sourcePath", "source_path"])?;
        daemon_vault!(state, v);
        claude_store::claude_store_import_in(&v, kind, name, source_path).await
    }
    .await;
    respond("claude_store_import", r)
}

pub(super) async fn claude_primitive_enable(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name, scope) = kind_name_scope(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_enable_in(&v, db, kind, name, scope).await
    }
    .await;
    respond("claude_primitive_enable", r)
}

pub(super) async fn claude_primitive_disable(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name, scope) = kind_name_scope(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_disable_in(&v, db, kind, name, scope).await
    }
    .await;
    respond("claude_primitive_disable", r)
}

pub(super) async fn claude_primitive_remove(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name, scope) = kind_name_scope(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_remove_in(&v, db, kind, name, scope).await
    }
    .await;
    respond("claude_primitive_remove", r)
}

pub(super) async fn claude_primitive_copy(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name, from, to, overwrite) = copy_args(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_copy_in(&v, db, kind, name, from, to, overwrite).await
    }
    .await;
    respond("claude_primitive_copy", r)
}

pub(super) async fn claude_primitive_move(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name, from, to, overwrite) = copy_args(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_move_in(&v, db, kind, name, from, to, overwrite).await
    }
    .await;
    respond("claude_primitive_move", r)
}

pub(super) async fn claude_primitive_enable_for(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (engine, kind, name, scope, hook_file) = engine_args(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_enable_for_in(&v, db, engine, kind, name, scope, hook_file)
            .await
    }
    .await;
    respond("claude_primitive_enable_for", r)
}

pub(super) async fn claude_primitive_disable_for(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (engine, kind, name, scope, hook_file) = engine_args(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_disable_for_in(&v, db, engine, kind, name, scope, hook_file)
            .await
    }
    .await;
    respond("claude_primitive_disable_for", r)
}

pub(super) async fn claude_primitive_remove_for(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (engine, kind, name, scope, hook_file) = engine_args(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_remove_for_in(&v, db, engine, kind, name, scope, hook_file)
            .await
    }
    .await;
    respond("claude_primitive_remove_for", r)
}

pub(super) async fn claude_primitive_copy_batch(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let from_engine: String = targ(args, &["fromEngine", "from_engine"])?;
        let kind: String = targ(args, &["kind"])?;
        let name: String = targ(args, &["name"])?;
        let from_scope: String = targ(args, &["fromScope", "from_scope"])?;
        let destinations: Vec<claude_store::NgwaCopyDestination> = targ(args, &["destinations"])?;
        let is_move: bool = targ(args, &["move"])?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::claude_primitive_copy_batch_in(
            &v,
            db,
            from_engine,
            kind,
            name,
            from_scope,
            destinations,
            is_move,
        )
        .await
    }
    .await;
    respond("claude_primitive_copy_batch", r)
}

pub(super) async fn oba_dependents(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name) = kind_name(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::oba_dependents_in(&v, db, kind, name).await
    }
    .await;
    respond("oba_dependents", r)
}

pub(super) async fn oba_safe_delete(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name) = kind_name(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::oba_safe_delete_in(&v, db, kind, name).await
    }
    .await;
    respond("oba_safe_delete", r)
}

pub(super) async fn oba_relink_dependents(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let dependents: Vec<String> = targ(args, &["dependents"])?;
        let new_master: String = targ(args, &["newMaster", "new_master"])?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::oba_relink_dependents_in(&v, Some(db), dependents, new_master).await
    }
    .await;
    respond("oba_relink_dependents", r)
}

pub(super) async fn oba_unlink_one(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::oba_unlink_one_in(&v, Some(db), path).await
    }
    .await;
    respond("oba_unlink_one", r)
}

pub(super) fn oba_forget(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let (kind, name) = kind_name(args)?;
        daemon_vault!(state, v);
        claude_store::oba_forget_in(&v, kind, name)
    })();
    respond("oba_forget", r)
}

pub(super) async fn oba_backfill_registry(state: &AppState) -> RpcResponse {
    let r = async {
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::oba_backfill_registry_in(&v, db).await
    }
    .await;
    respond("oba_backfill_registry", r)
}

pub(super) async fn oba_missing_requires(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name) = kind_name(args)?;
        let db = pa_db(state)?;
        daemon_vault!(state, v);
        claude_store::install::oba_missing_requires_in(&v, db, kind, name).await
    }
    .await;
    respond("oba_missing_requires", r)
}

pub(super) async fn oba_set_auto_update(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let (kind, name) = kind_name(args)?;
        let enabled: bool = targ(args, &["enabled"])?;
        daemon_vault!(state, v);
        claude_store::install::oba_set_auto_update_in(&v, kind, name, enabled).await
    }
    .await;
    respond("oba_set_auto_update", r)
}

fn kind_name(args: &Value) -> Result<(String, String), String> {
    Ok((targ(args, &["kind"])?, targ(args, &["name"])?))
}

fn kind_name_scope(args: &Value) -> Result<(String, String, String), String> {
    let (kind, name) = kind_name(args)?;
    Ok((kind, name, targ(args, &["scope"])?))
}

type CopyArgs = (String, String, String, String, Option<bool>);

fn copy_args(args: &Value) -> Result<CopyArgs, String> {
    let (kind, name) = kind_name(args)?;
    Ok((
        kind,
        name,
        targ(args, &["fromScope", "from_scope"])?,
        targ(args, &["toScope", "to_scope"])?,
        targ(args, &["overwrite"])?,
    ))
}

type EngineArgs = (String, String, String, String, Option<String>);

fn engine_args(args: &Value) -> Result<EngineArgs, String> {
    let engine: String = targ(args, &["engine"])?;
    let (kind, name, scope) = kind_name_scope(args)?;
    Ok((
        engine,
        kind,
        name,
        scope,
        targ(args, &["hookFile", "hook_file"])?,
    ))
}

/// Router tests for the vault arms above (a file of their own: the fixture is
/// a whole temp vault — home, store, data dir with a project row).
#[cfg(test)]
#[path = "rpc_claude_vault_tests.rs"]
mod vault_tests;

#[cfg(test)]
mod tests {
    //! House pattern (see `rpc_shell`'s tests): a literal `ServerConfig` →
    //! `router_with` → `oneshot` POST `/api/rpc` with the bearer token. The
    //! router home is a temp dir holding a fixture `~/.claude` tree and the fs
    //! allowlist is a local root set, so nothing here reads the real user's
    //! `~/.claude` or installs the process-global `fs_roots`. Every happy path
    //! is compared against the shared core called directly on the same
    //! inputs — the JSON is the desktop's by construction.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use crate::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::rpc_shell::PathGuard;
    use crate::server::shared::claude_sessions::{self, jsonl_reader};
    use crate::server::shared::projects::FsReach;
    use crate::server::shared::{
        agent_config, agent_projects, chi, claude_config, engine_layout, settings_cascade,
    };
    use crate::server::{router_with, ServerConfig};

    const S1: &str = "11111111-1111-4111-8111-111111111111";
    const S2: &str = "22222222-2222-4222-8222-222222222222";
    const EVIL_FILE: &str = "33333333-3333-4333-8333-333333333333";
    const EVIL_DIR: &str = "44444444-4444-4444-8444-444444444444";

    fn config(data_dir: Option<PathBuf>) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
            data_dir,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn session_log(id: &str, cwd: &str, ts: &str, text: &str) -> String {
        [
            json!({
                "type": "user", "sessionId": id, "cwd": cwd, "timestamp": format!("{ts}:00Z"),
                "uuid": "u1",
                "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
            }),
            json!({
                "type": "assistant", "sessionId": id, "cwd": cwd, "timestamp": format!("{ts}:05Z"),
                "uuid": "a1",
                "message": {
                    "model": "claude-opus", "role": "assistant",
                    "content": [{ "type": "text", "text": "hi" }],
                },
            }),
            json!({ "type": "ai-title", "aiTitle": format!("title {id}") }),
        ]
        .iter()
        .map(|v| format!("{v}\n"))
        .collect()
    }

    /// A daemon with `--data-dir`, a fixture home, and an fs allowlist of
    /// exactly `allowed/`. `outside/` is a sibling the allowlist does not
    /// cover, holding a would-be secret session log and config file.
    struct Daemon {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        allowed: PathBuf,
        outside: PathBuf,
        db: Arc<PaDb>,
        router: Router,
    }

    impl Daemon {
        fn projects(&self) -> PathBuf {
            self.home.join(".claude/projects")
        }
    }

    fn daemon_with(with_home: bool, with_data: bool) -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, home) = (root.join("data"), root.join("home"));
        let (allowed, outside) = (root.join("allowed"), root.join("outside"));
        for d in [&data, &home, &allowed, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }

        // ~/.claude: settings, an agent, a skill, two session logs.
        let claude = home.join(".claude");
        write(
            &claude.join("settings.json"),
            r#"{"model":"opus","theme":"dark"}"#,
        );
        write(
            &claude.join("agents/helper.md"),
            "---\nname: helper\ndescription: home agent\n---\nbody\n",
        );
        write(
            &claude.join("skills/tidy/SKILL.md"),
            "---\nname: tidy\ndescription: a skill\n---\nsteps\n",
        );
        let projects = claude.join("projects");
        write(
            &projects.join(format!("-proj-one/{S1}.jsonl")),
            &session_log(S1, "/proj/one", "2026-09-01T10:00", "first question"),
        );
        write(
            &projects.join(format!("-proj-two/{S2}.jsonl")),
            &session_log(S2, "/proj/two", "2026-09-02T10:00", "second question"),
        );

        // An allowlisted project with its own .claude.
        let proj = allowed.join("proj");
        write(
            &proj.join(".claude/settings.json"),
            r#"{"theme":"light","permissions":{"allow":["Read"]}}"#,
        );
        write(
            &proj.join(".claude/agents/local.md"),
            "---\nname: local\ndescription: project agent\n---\nproj body\n",
        );
        write(&proj.join("notes.txt"), "not config");

        // Outside the allowlist: a project, and would-be secrets.
        write(
            &outside.join("proj/.claude/agents/secret.md"),
            "---\nname: secret\n---\nx\n",
        );
        write(&outside.join(".claude/secret.md"), "top secret");
        write(
            &outside.join("secret.jsonl"),
            &session_log(EVIL_FILE, "/secret", "2026-09-03T10:00", "exfiltrate me"),
        );
        write(
            &outside.join(format!("dir/{EVIL_DIR}.jsonl")),
            &session_log(EVIL_DIR, "/secret", "2026-09-04T10:00", "exfiltrate me too"),
        );

        let roots_file = root.join("fs_roots.json");
        std::fs::write(
            &roots_file,
            json!({ "roots": [allowed.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with(
            config(with_data.then(|| data.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            with_data.then(|| db.clone()),
            None,
            with_home.then(|| home.clone()),
            PathGuard::roots(Arc::new(roots)),
        );
        Daemon {
            _tmp: tmp,
            home,
            allowed,
            outside,
            db,
            router,
        }
    }

    fn daemon() -> Daemon {
        daemon_with(true, true)
    }

    async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
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

    async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    async fn err(router: &Router, cmd: &str, args: Value) -> String {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
        res["error"].as_str().unwrap().to_string()
    }

    fn wire<T: serde::Serialize>(v: T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Plant the two escapes: a session log symlinked out of
    /// `~/.claude/projects`, and a slug dir symlinked out of it.
    #[cfg(unix)]
    fn plant_session_escapes(d: &Daemon) {
        let evil = d.projects().join("-evil");
        std::fs::create_dir_all(&evil).unwrap();
        std::os::unix::fs::symlink(
            d.outside.join("secret.jsonl"),
            evil.join(format!("{EVIL_FILE}.jsonl")),
        )
        .unwrap();
        std::os::unix::fs::symlink(d.outside.join("dir"), d.projects().join("-linked")).unwrap();
    }

    fn sorted_ids(v: &Value, key: &str) -> Vec<String> {
        let mut ids: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[key].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    }

    // ── session logs ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn session_arms_serve_the_shared_core_over_the_router_home() {
        let d = daemon();
        let r = &d.router;
        let root = d.projects();

        // claude_list_sessions: both spellings, filter + limit.
        let all = ok(r, "claude_list_sessions", json!({})).await;
        let expect =
            wire(claude_sessions::list_sessions(&root, None, None, FsReach::Follow).unwrap());
        assert_eq!(all, expect);
        assert_eq!(sorted_ids(&all, "sessionId"), vec![S1, S2]);
        // Newest first, camelCase wire.
        assert_eq!(all[0]["sessionId"], S2);
        assert_eq!(all[0]["projectDir"], "/proj/two");
        assert_eq!(all[0]["messageCount"], 2);
        for args in [
            json!({ "projectDir": "/proj/one" }),
            json!({ "project_dir": "/proj/one" }),
        ] {
            let one = ok(r, "claude_list_sessions", args).await;
            assert_eq!(sorted_ids(&one, "sessionId"), vec![S1]);
        }
        let limited = ok(r, "claude_list_sessions", json!({ "limit": 1 })).await;
        assert_eq!(limited.as_array().unwrap().len(), 1);

        // claude_read_jsonl: both spellings, the desktop's reader.
        let path = root.join(format!("-proj-one/{S1}.jsonl"));
        let expect = wire(jsonl_reader::read_jsonl(&path).unwrap());
        assert!(expect.as_array().unwrap().len() > 1);
        for args in [json!({ "sessionId": S1 }), json!({ "session_id": S1 })] {
            assert_eq!(ok(r, "claude_read_jsonl", args).await, expect);
        }
        let e = err(r, "claude_read_jsonl", json!({ "sessionId": EVIL_FILE })).await;
        assert!(e.contains("not found on disk"), "{e}");

        // claude_session_list: both spellings.
        let browser = ok(r, "claude_session_list", json!({})).await;
        assert_eq!(
            browser,
            wire(claude_sessions::enumerate_sessions(
                &root,
                None,
                FsReach::Follow
            ))
        );
        assert_eq!(sorted_ids(&browser, "session_id"), vec![S1, S2]);
        for args in [
            json!({ "projectSlug": "-proj-two" }),
            json!({ "project_slug": "-proj-two" }),
        ] {
            let two = ok(r, "claude_session_list", args).await;
            assert_eq!(sorted_ids(&two, "session_id"), vec![S2]);
            assert_eq!(two[0]["title"], format!("title {S2}"));
        }
    }

    /// A session id is never a path, and a log that canonicalizes outside
    /// `~/.claude/projects` is neither read nor listed.
    #[tokio::test]
    async fn session_arms_refuse_paths_and_symlink_escapes() {
        let d = daemon();
        let r = &d.router;

        let outside_log = s(&d.outside.join("secret"));
        for bad in [
            "../../../outside/secret",
            "..",
            "a/b",
            outside_log.as_str(),
            "",
            "x\\y",
        ] {
            let e = err(r, "claude_read_jsonl", json!({ "sessionId": bad })).await;
            assert!(e.contains("invalid session id"), "{bad:?}: {e}");
        }

        #[cfg(unix)]
        {
            plant_session_escapes(&d);
            let root = d.projects();
            for id in [EVIL_FILE, EVIL_DIR] {
                let e = err(r, "claude_read_jsonl", json!({ "sessionId": id })).await;
                assert!(e.contains("resolves outside"), "{id}: {e}");
            }
            // The unconfined (desktop) walk would list both escapes; the
            // daemon's lists neither.
            let follow =
                claude_sessions::list_sessions(&root, None, None, FsReach::Follow).unwrap();
            assert_eq!(follow.len(), 4, "the fixture's escapes are real");
            let listed = ok(r, "claude_list_sessions", json!({})).await;
            assert_eq!(sorted_ids(&listed, "sessionId"), vec![S1, S2]);
            let browsed = ok(r, "claude_session_list", json!({})).await;
            assert_eq!(sorted_ids(&browsed, "session_id"), vec![S1, S2]);
            assert_eq!(
                claude_sessions::enumerate_sessions(&root, None, FsReach::Follow).len(),
                4
            );
            let merged = ok(r, "chi_list", json!({})).await;
            assert!(!merged.to_string().contains("exfiltrate"), "{merged}");
        }
    }

    /// `chi_list` now merges the JSONL sessions exactly as the desktop does
    /// (`chi::list_merged`), from the router home, confined.
    #[tokio::test]
    async fn chi_list_merges_claude_sessions_like_the_desktop() {
        let d = daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        // A cache row that IS session S1 (external_id), and one codex row.
        for (run_id, engine, external, seen) in [
            ("r-claude", "claude-code", Some(S1), "2020-01-01T00:00:00Z"),
            ("r-codex", "codex", None, "2026-09-05T00:00:00Z"),
        ] {
            sqlx::query(
                "INSERT INTO chi_cache (
                    run_id, engine_id, external_id, status, output_truncated, owner,
                    started_at, last_seen_at
                ) VALUES (?, ?, ?, 'done', 0, 'cli', ?, ?)",
            )
            .bind(run_id)
            .bind(engine)
            .bind(external)
            .bind(seen)
            .bind(seen)
            .execute(&pool)
            .await
            .unwrap();
        }

        let root = d.projects();
        let merged = ok(r, "chi_list", json!({})).await;
        let expect = wire(
            chi::list_merged(&d.db, None, None, Some(&root), FsReach::Follow)
                .await
                .unwrap(),
        );
        assert_eq!(merged, expect);
        let ids: Vec<&str> = merged
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["run_id"].as_str().unwrap())
            .collect();
        // S1 refreshed the cache row (no duplicate); S2 was added.
        assert_eq!(ids, vec!["r-codex", S2, "r-claude"]);
        assert_eq!(merged[2]["last_seen_at"], "2026-09-01T10:00:05Z");
        assert_eq!(merged[1]["engine_id"], "claude-code");
        assert_eq!(merged[1]["status"], "done");

        // An engine filter other than claude-code merges nothing.
        let codex = ok(r, "chi_list", json!({ "engineId": "codex" })).await;
        assert_eq!(sorted_ids(&codex, "run_id"), vec!["r-codex"]);
        // The limit still applies after the merge.
        let one = ok(r, "chi_list", json!({ "limit": 1 })).await;
        assert_eq!(sorted_ids(&one, "run_id"), vec!["r-codex"]);
    }

    // ── config browser ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn claude_config_load_scans_home_and_confines_project_roots() {
        let d = daemon();
        let r = &d.router;
        let proj = d.allowed.join("proj");

        let expect = wire(
            claude_config::scan_all_in(
                vec![s(&proj)],
                Some(&d.home),
                crate::pkg::skill_actions::store_root().as_deref(),
            )
            .unwrap(),
        );
        for key in ["projectRoots", "project_roots"] {
            let got = ok(r, "claude_config_load", json!({ key: [s(&proj)] })).await;
            assert_eq!(got, expect);
        }
        let names: Vec<&str> = expect["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"helper") && names.contains(&"local"),
            "{names:?}"
        );

        // Outside the allowlist / relative / `..`: not scanned, reported in
        // errors[]; a missing root under the allowlist is skipped silently.
        let outside = s(&d.outside.join("proj"));
        let dotdot = format!("{}/../outside/proj", s(&d.allowed));
        let missing = s(&d.allowed.join("nope"));
        let got = ok(
            r,
            "claude_config_load",
            json!({ "projectRoots": [s(&proj), outside, "relative/proj", dotdot, missing] }),
        )
        .await;
        let messages: Vec<String> = got["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["message"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].contains("outside allowlist"), "{messages:?}");
        assert!(messages[1].contains("must be absolute"), "{messages:?}");
        assert!(messages[2].contains("`..`"), "{messages:?}");
        let mut expect = expect.clone();
        expect["errors"] = got["errors"].clone();
        assert_eq!(got, expect, "everything but errors[] is the confined scan");
        assert!(!got["agents"].to_string().contains("secret"));
    }

    #[tokio::test]
    async fn claude_config_read_file_stays_inside_its_three_roots() {
        let d = daemon();
        let r = &d.router;

        let agent = d.home.join(".claude/agents/helper.md");
        let body = std::fs::read_to_string(&agent).unwrap();
        assert_eq!(
            ok(r, "claude_config_read_file", json!({ "path": s(&agent) })).await,
            body
        );
        assert_eq!(
            ok(
                r,
                "claude_config_read_file",
                json!({ "path": "~/.claude/agents/helper.md" })
            )
            .await,
            body
        );
        let local = d.allowed.join("proj/.claude/agents/local.md");
        assert_eq!(
            ok(r, "claude_config_read_file", json!({ "path": s(&local) })).await,
            std::fs::read_to_string(&local).unwrap()
        );

        let e = err(
            r,
            "claude_config_read_file",
            json!({ "path": s(&d.outside.join(".claude/secret.md")) }),
        )
        .await;
        assert!(e.contains("outside the fs allowlist"), "{e}");
        let e = err(
            r,
            "claude_config_read_file",
            json!({ "path": s(&d.allowed.join("proj/notes.txt")) }),
        )
        .await;
        assert!(e.contains("not under a .claude/ dir"), "{e}");
        let climb = format!(
            "{}/proj/.claude/../../../outside/.claude/secret.md",
            s(&d.allowed)
        );
        let e = err(r, "claude_config_read_file", json!({ "path": climb })).await;
        assert!(e.contains("`..`"), "{e}");
        let e = err(
            r,
            "claude_config_read_file",
            json!({ "path": ".claude/x.md" }),
        )
        .await;
        assert!(e.contains("must be absolute"), "{e}");

        #[cfg(unix)]
        {
            let link = d.allowed.join("proj/.claude/agents/link.md");
            std::os::unix::fs::symlink(d.outside.join(".claude/secret.md"), &link).unwrap();
            let e = err(r, "claude_config_read_file", json!({ "path": s(&link) })).await;
            assert!(e.contains("outside the fs allowlist"), "{e}");
        }
    }

    #[tokio::test]
    async fn resolve_cascade_merges_home_and_confined_dirs() {
        let d = daemon();
        let r = &d.router;
        let proj = d.allowed.join("proj");

        let expect = wire(settings_cascade::resolve_settings_cascade_in(
            Path::new(settings_cascade::MANAGED_SETTINGS),
            Some(&d.home.join(".claude/settings.json")),
            Some(&proj),
            None,
        ));
        for args in [
            json!({ "projectDir": s(&proj) }),
            json!({ "project_dir": s(&proj), "overlay_dir": null }),
        ] {
            assert_eq!(ok(r, "claude_config_resolve_cascade", args).await, expect);
        }
        // Project beats user on `theme`; the user tier's `model` survives.
        assert_eq!(expect["merged"]["theme"], "light");
        assert_eq!(expect["merged"]["model"], "opus");

        // No project dir: the user tier alone.
        let user_only = ok(r, "claude_config_resolve_cascade", json!({})).await;
        assert_eq!(user_only["merged"]["theme"], "dark");

        // A missing dir inside the allowlist contributes nothing.
        let missing = ok(
            r,
            "claude_config_resolve_cascade",
            json!({ "overlayDir": s(&d.allowed.join("no-overlay")) }),
        )
        .await;
        assert_eq!(missing, user_only);

        for key in ["projectDir", "overlayDir"] {
            let e = err(
                r,
                "claude_config_resolve_cascade",
                json!({ key: s(&d.outside.join("proj")) }),
            )
            .await;
            assert!(e.contains("outside allowlist"), "{key}: {e}");
        }
    }

    // ── asset pins ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn asset_pins_round_trip_in_both_spellings() {
        let d = daemon();
        let r = &d.router;
        ok(
            r,
            "claude_asset_pin",
            json!({
                "scope": "workspace", "assetKind": "skill", "assetName": "tidy",
                "preferredTier": "personal", "preferredSource": null,
            }),
        )
        .await;
        ok(
            r,
            "claude_asset_pin",
            json!({
                "scope": "workspace", "asset_kind": "agent", "asset_name": "helper",
                "preferred_tier": "workspace_pkg", "preferred_source": "com.x.pkg",
            }),
        )
        .await;
        let listed = ok(r, "claude_asset_list_pins", json!({ "scope": "workspace" })).await;
        let expect = wire(
            claude_config::asset_list_pins(&d.db, "workspace".into())
                .await
                .unwrap(),
        );
        assert_eq!(listed, expect);
        assert_eq!(listed.as_array().unwrap().len(), 2);
        assert_eq!(listed[0]["asset_kind"], "agent");
        assert_eq!(listed[0]["preferred_source"], "com.x.pkg");

        ok(
            r,
            "claude_asset_unpin",
            json!({ "scope": "workspace", "assetKind": "agent", "assetName": "helper" }),
        )
        .await;
        let listed = ok(r, "claude_asset_list_pins", json!({ "scope": "workspace" })).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);

        // The desktop's validation, verbatim.
        let e = err(r, "claude_asset_list_pins", json!({ "scope": "global" })).await;
        assert!(
            e.contains("scope must be 'workspace' or 'project:<id>'"),
            "{e}"
        );
        let e = err(
            r,
            "claude_asset_pin",
            json!({
                "scope": "workspace", "assetKind": "widget", "assetName": "x",
                "preferredTier": "personal",
            }),
        )
        .await;
        assert!(e.contains("asset_kind must be one of"), "{e}");
    }

    // ── wizard detection / layout ───────────────────────────────────────────

    #[tokio::test]
    async fn detection_arms_match_the_shared_cores() {
        let d = daemon();
        let r = &d.router;
        let proj = d.allowed.join("proj");

        let mut expect = agent_config::build_inventory_in("claude-code", &proj, Some(&d.home));
        expect.root_path = s(&proj);
        let expect = wire(expect);
        for args in [
            json!({ "agentId": "claude-code", "rootPath": s(&proj) }),
            json!({ "agent_id": "claude-code", "root_path": s(&proj) }),
        ] {
            assert_eq!(ok(r, "detect_agent_config", args).await, expect);
        }
        assert_eq!(expect["agent_count"], 1);
        assert_eq!(
            expect["project_count"], 2,
            "home's ~/.claude/projects slugs"
        );

        let e = err(
            r,
            "detect_agent_config",
            json!({ "agentId": "claude-code", "rootPath": s(&d.outside.join("proj")) }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");
        let e = err(
            r,
            "detect_agent_config",
            json!({ "agentId": "claude-code", "rootPath": "proj" }),
        )
        .await;
        assert!(e.contains("must be absolute"), "{e}");

        let projects = ok(r, "list_claude_projects", json!({})).await;
        assert_eq!(
            projects,
            wire(agent_projects::list_claude_projects_in(Some(&d.home)))
        );
        assert_eq!(
            sorted_ids(&projects, "slug"),
            vec!["-proj-one", "-proj-two"]
        );
        for (args, id) in [
            (json!({ "agentId": "codex" }), "codex"),
            (json!({ "agent_id": "claude" }), "claude"),
        ] {
            assert_eq!(
                ok(r, "list_agent_projects", args).await,
                wire(agent_projects::list_agent_projects_in(id, Some(&d.home)))
            );
        }

        assert_eq!(
            ok(r, "engine_layout", json!({})).await,
            wire(engine_layout::engine_layouts())
        );
        #[cfg(not(windows))]
        assert_eq!(
            ok(r, "terminal_detect_shells", json!({})).await,
            wire(crate::server::shared::shell_detect::detect_shells())
        );
    }

    // ── what is missing ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn arms_name_what_is_missing() {
        // No home: the desktop's "HOME unset" where it errors, its empty
        // answer where it has one.
        let d = daemon_with(false, true);
        let r = &d.router;
        for (cmd, args) in [
            ("claude_list_sessions", json!({})),
            ("claude_read_jsonl", json!({ "sessionId": S1 })),
        ] {
            let e = err(r, cmd, args).await;
            assert!(e.contains("HOME unset"), "{cmd}: {e}");
        }
        assert_eq!(ok(r, "claude_session_list", json!({})).await, json!([]));
        assert_eq!(ok(r, "list_claude_projects", json!({})).await, json!([]));
        // The cache rows alone: the desktop's chi_list with no HOME.
        assert_eq!(ok(r, "chi_list", json!({})).await, json!([]));

        // No --data-dir: the pins name the flag.
        let d = daemon_with(true, false);
        for (cmd, args) in [
            ("claude_asset_list_pins", json!({ "scope": "workspace" })),
            (
                "claude_asset_unpin",
                json!({ "scope": "workspace", "assetKind": "skill", "assetName": "x" }),
            ),
        ] {
            let e = err(&d.router, cmd, args).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
        }
    }
}
