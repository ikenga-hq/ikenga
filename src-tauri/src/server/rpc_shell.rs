//! `/api/rpc` bodies for the shell-state commands served in WP-19 slice 4:
//! notifications, projects, activity-bar pins / sections, artifact comments
//! and studio threads.
//!
//! Same house pattern as `rpc_local`: the arm *names* stay in `rpc.rs`'s
//! dispatch `match` (the parity ratchet reads them there) and delegate here;
//! every body calls the core the desktop `#[tauri::command]` calls
//! (`server::shared::{notifications, projects, activity_bar, comments,
//! studio_threads}`), over the daemon's `--data-dir` `ikenga.db`, and returns
//! the same serialized type — so the JSON shapes are the desktop's by
//! construction.
//!
//! **Arguments.** Each argument is decoded by [`targ`] into the Rust type the
//! desktop command declares, from the camelCase key `tauri-cmd.ts` sends (on
//! the desktop Tauri renames `snake_case` parameters to camelCase keys) or
//! the snake_case spelling, with Tauri's own rules: absent and `null` are the
//! same thing for an `Option`, so `Option<Option<T>>` "clear" semantics are
//! exactly the desktop's (`null` = leave unchanged).
//!
//! **No events.** The desktop emits `projects:active-changed` and relays
//! notification changes as `notifications://changed`. The daemon has no event
//! channel — the web transport's `listen()` is a no-op — so these arms change
//! the same rows and simply emit nothing; the browser refetches on its own
//! schedule.
//!
//! **Paths.** Caller-controlled paths never reach past the daemon's fs
//! allowlist (`<data-dir>/fs_roots.json`, the `fs_*` arms' boundary) or out
//! of the project root they name: see [`project_root`] and [`scaffold_root`].
//! The strings that are only stored as keys — a comment's `artifactPath`, a
//! studio thread's `folderPath` — are never opened by any daemon arm; the one
//! stored path something later opens (a comment's `screenshotPath`, which
//! `comment_route` hands to an agent) is confined to the data dir's
//! `pin-screenshots/`.
//!
//! **Single-user seams (G-PRINCIPAL / WP-20).** Everything here is keyed by
//! the daemon's `--data-dir` and its process home, not by a principal: one
//! project list, one active project, one set of pins, one `mutedKinds` in
//! `<home>/.ikenga/settings.json`. That is correct under G-PRINCIPAL #310's
//! topology B (each principal runs its own daemon with its own data dir); a
//! shared multi-principal daemon would have to key all of it by principal.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::rpc::RpcResponse;
use super::rpc_local::{arg, data_dir, pa_db, respond, settings};
use super::shared::notifications::{mute, ops as notif};
use super::shared::projects::{self, CreateArgs, FsReach, ProjectInventory, ProjectPatch};
use super::shared::{activity_bar, comments, studio_threads};
use super::AppState;

// ─── arguments ───────────────────────────────────────────────────────────────

/// Decode one command argument the way Tauri does: into the type the desktop
/// command declares, from the first of `names` present with a non-null
/// value, or from `null` when none is (which an `Option` takes as `None` and
/// anything else refuses — reported as "required").
pub(super) fn targ<T: DeserializeOwned>(args: &Value, names: &[&str]) -> Result<T, String> {
    match arg(args, names) {
        Some(v) => {
            serde_json::from_value(v.clone()).map_err(|e| format!("invalid `{}`: {e}", names[0]))
        }
        None => {
            serde_json::from_value(Value::Null).map_err(|_| format!("`{}` is required", names[0]))
        }
    }
}

// ─── path boundary ───────────────────────────────────────────────────────────

/// The allowlist the project filesystem arms check a canonical path against.
///
/// In production it is the process-global `crate::fs_roots` set the daemon
/// installs from `<data-dir>/fs_roots.json` at boot — the same boundary the
/// `fs_*` arms enforce, so these arms reach nothing a token holder could not
/// already `fs_read`. Tests pass a local root set, because the global is a
/// `OnceLock` that would pin one set for the whole test binary.
#[derive(Clone)]
pub(crate) enum PathGuard {
    Allowlist,
    #[cfg(test)]
    Roots(std::sync::Arc<crate::fs_roots::FsRoots>),
}

impl PathGuard {
    pub(super) fn check(&self, canonical: &Path) -> Result<(), String> {
        let allowed = match self {
            PathGuard::Allowlist => crate::fs_roots::current()
                .ok_or("fs allowlist not initialized (the daemon needs --data-dir)")?
                .is_allowed(canonical),
            #[cfg(test)]
            PathGuard::Roots(roots) => roots.is_allowed(canonical),
        };
        if allowed {
            Ok(())
        } else {
            Err(format!("path outside allowlist: {}", canonical.display()))
        }
    }
}

/// An absolute, existing directory inside the allowlist, canonicalized.
fn allowlisted_dir(state: &AppState, raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!("root_path must be absolute: {raw}"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("root_path is not a directory: {raw} ({e})"))?;
    if !canonical.is_dir() {
        return Err(format!("root_path is not a directory: {raw}"));
    }
    state.path_guard.check(&canonical)?;
    Ok(canonical)
}

/// What a read arm may do with a caller's `rootPath`.
enum Root {
    /// It names an active project's root, which exists and is allowlisted.
    /// Carries the canonical path, which is what gets walked — so a symlink
    /// in the caller's spelling cannot be swapped between check and use.
    Walk(String),
    /// It is, verbatim, an active project's recorded root, but that directory
    /// no longer exists: the desktop reads it as empty, and so does this.
    Missing,
}

/// The read arms (`project_inventory`, `project_skills_list`,
/// `project_artifacts_walk`) only look inside the root of an **active
/// project** in the daemon's `ikenga.db`, and only when that root is inside
/// the fs allowlist. A path that is neither — `/`, `~/.ssh`, a sibling of a
/// project — is refused, not walked; inside the root, the shared helpers run
/// with [`FsReach::Confined`] so a symlink planted there reads as absent.
async fn project_root(state: &AppState, raw: &str) -> Result<Root, String> {
    let pool = pa_db(state)?.ensure_pool().await?;
    let active = projects::list_projects(&pool, false).await?;
    let roots: Vec<&str> = active
        .iter()
        .filter_map(|p| p.root_path.as_deref())
        .filter(|r| !r.trim().is_empty())
        .collect();
    let not_a_project = || format!("root_path is not the root of an active project: {raw}");
    if !Path::new(raw).is_absolute() {
        return Err(format!("root_path must be absolute: {raw}"));
    }
    let Ok(canonical) = Path::new(raw).canonicalize() else {
        return if roots.contains(&raw) {
            Ok(Root::Missing)
        } else {
            Err(not_a_project())
        };
    };
    let registered = roots
        .iter()
        .any(|r| Path::new(r).canonicalize().ok().as_deref() == Some(canonical.as_path()));
    if !registered {
        return Err(not_a_project());
    }
    if !canonical.is_dir() {
        return Ok(Root::Missing);
    }
    state.path_guard.check(&canonical)?;
    Ok(Root::Walk(canonical.to_string_lossy().into_owned()))
}

/// `project_scaffold_claude`'s boundary. Its only caller scaffolds a folder
/// the user just picked, BEFORE the project row exists, so this cannot demand
/// a registered root: the folder must be inside the fs allowlist (where the
/// token holder can already `fs_mkdir` / `fs_write`), and the shared helper
/// then refuses to create or write through a symlink.
fn scaffold_root(state: &AppState, raw: &str) -> Result<String, String> {
    allowlisted_dir(state, raw).map(|c| c.to_string_lossy().into_owned())
}

/// A project `root_path` the daemon would store: `None` / blank pass through
/// (the desktop stores them as given); anything else must be an allowlisted
/// directory. Without this, `project_create` + `settings_write_field` (scope
/// `project`, which writes `<root>/.ikenga/settings.json`) would be a write
/// anywhere on disk.
fn check_stored_root(state: &AppState, root: Option<&str>) -> Result<(), String> {
    match root {
        Some(r) if !r.trim().is_empty() => allowlisted_dir(state, r.trim()).map(|_| ()),
        _ => Ok(()),
    }
}

// ─── Notifications ───────────────────────────────────────────────────────────
//
// `mutedKinds` lives in the personal settings file under the daemon's home
// (G-PRINCIPAL seam, see the module doc). Reads that hide muted rows need
// that settings store and say so when it is missing, rather than returning
// muted rows as if nothing were muted.

pub(super) async fn notifications_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let list_args = notif::ListArgs {
            unread_only: targ(args, &["unreadOnly", "unread_only"])?,
            kinds: targ(args, &["kinds"])?,
            limit: targ(args, &["limit"])?,
            before: targ(args, &["before"])?,
            include_muted: targ(args, &["includeMuted", "include_muted"])?,
        };
        let db = pa_db(state)?;
        let muted = if list_args.include_muted.unwrap_or(false) {
            Ok(Vec::new())
        } else {
            settings(state).await.map(mute::muted_kinds)
        };
        notif::list(db, move || muted, list_args).await
    }
    .await;
    respond("notifications_list", r)
}

pub(super) async fn notifications_unread_count(state: &AppState) -> RpcResponse {
    let r = async {
        let db = pa_db(state)?;
        let muted = mute::muted_kinds(settings(state).await?);
        notif::unread_count(db, &muted).await
    }
    .await;
    respond("notifications_unread_count", r)
}

pub(super) async fn notifications_mark_read(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let ids: Vec<i64> = targ(args, &["ids"])?;
        notif::mark_read(pa_db(state)?, &ids).await
    }
    .await;
    respond("notifications_mark_read", r)
}

pub(super) async fn notifications_mark_all_read(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: Option<String> = targ(args, &["kind"])?;
        notif::mark_all_read(pa_db(state)?, kind).await
    }
    .await;
    respond("notifications_mark_all_read", r)
}

pub(super) async fn notifications_mute_state(state: &AppState) -> RpcResponse {
    let r = async { Ok(notif::mute_state(settings(state).await?)) }.await;
    respond("notifications_mute_state", r)
}

pub(super) async fn notifications_mute_kind(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: String = targ(args, &["kind"])?;
        let db = pa_db(state)?;
        notif::mute_kind(db, settings(state).await?, kind).await
    }
    .await;
    respond("notifications_mute_kind", r)
}

pub(super) async fn notifications_unmute_kind(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: String = targ(args, &["kind"])?;
        let db = pa_db(state)?;
        notif::unmute_kind(db, settings(state).await?, kind).await
    }
    .await;
    respond("notifications_unmute_kind", r)
}

// ─── Projects ────────────────────────────────────────────────────────────────

pub(super) async fn project_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let include_archived: Option<bool> = targ(args, &["includeArchived", "include_archived"])?;
        let pool = pa_db(state)?.ensure_pool().await?;
        projects::list_projects(&pool, include_archived.unwrap_or(false)).await
    }
    .await;
    respond("project_list", r)
}

pub(super) async fn project_get_active(state: &AppState) -> RpcResponse {
    let r = async {
        let pool = pa_db(state)?.ensure_pool().await?;
        projects::get_active_project(&pool).await
    }
    .await;
    respond("project_get_active", r)
}

pub(super) async fn project_create(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let create = CreateArgs {
            id: targ(args, &["id"])?,
            display_name: targ(args, &["displayName", "display_name"])?,
            root_path: targ(args, &["rootPath", "root_path"])?,
            icon: targ(args, &["icon"])?,
            color: targ(args, &["color"])?,
            description: targ(args, &["description"])?,
        };
        let pool = pa_db(state)?.ensure_pool().await?;
        check_stored_root(state, create.root_path.as_deref())?;
        projects::create_project(&pool, create).await
    }
    .await;
    respond("project_create", r)
}

/// `patch` is decoded as the desktop's `ProjectPatch` — snake_case fields,
/// because Tauri renames only the top-level parameter names, not the fields
/// of a struct argument. `tauri-cmd.ts` sends it that way.
pub(super) async fn project_update(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        let patch: ProjectPatch = targ(args, &["patch"])?;
        let pool = pa_db(state)?.ensure_pool().await?;
        if let Some(root) = &patch.root_path {
            check_stored_root(state, root.as_deref())?;
        }
        projects::update_project(&pool, &id, patch).await
    }
    .await;
    respond("project_update", r)
}

pub(super) async fn project_archive(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        let pool = pa_db(state)?.ensure_pool().await?;
        projects::archive_project(&pool, &id).await
    }
    .await;
    respond("project_archive", r)
}

/// The desktop also emits `projects:active-changed`; the daemon has no event
/// channel (see the module doc), so it only writes the row.
pub(super) async fn project_set_active(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        let pool = pa_db(state)?.ensure_pool().await?;
        projects::set_active_project_id(&pool, &id).await
    }
    .await;
    respond("project_set_active", r)
}

pub(super) async fn project_inventory(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root_path: Option<String> = targ(args, &["rootPath", "root_path"])?;
        let Some(raw) = root_path else {
            // No root: the desktop's all-zero answer, which reads nothing.
            return projects::inventory(None, FsReach::Confined);
        };
        match project_root(state, &raw).await? {
            // Counted under the canonical root; `root_path` echoes the
            // caller's spelling, as the desktop's does (the FE keys by it).
            Root::Walk(root) => {
                projects::inventory(Some(root), FsReach::Confined).map(|inv| ProjectInventory {
                    root_path: Some(raw),
                    ..inv
                })
            }
            Root::Missing => Ok(ProjectInventory {
                root_path: Some(raw),
                has_claude_dir: false,
                skills: 0,
                commands: 0,
                mcp: 0,
            }),
        }
    }
    .await;
    respond("project_inventory", r)
}

/// The user-global half reads `<home>/.claude/skills` for the router's home —
/// the daemon PROCESS's home (G-PRINCIPAL seam) — confined to that dir.
pub(super) async fn project_skills_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root_path: Option<String> = targ(args, &["rootPath", "root_path"])?;
        let include_user_global: bool = targ(args, &["includeUserGlobal", "include_user_global"])?;
        let root_path = match root_path {
            Some(raw) => match project_root(state, &raw).await? {
                Root::Walk(root) => Some(root),
                Root::Missing => None,
            },
            None => None,
        };
        projects::skills_list(
            root_path,
            include_user_global,
            state.home.as_deref(),
            FsReach::Confined,
        )
    }
    .await;
    respond("project_skills_list", r)
}

pub(super) async fn project_artifacts_walk(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let root_path: Option<String> = targ(args, &["rootPath", "root_path"])?;
        let Some(raw) = root_path else {
            return Ok(Vec::new());
        };
        match project_root(state, &raw).await? {
            Root::Walk(root) => projects::artifacts_walk(Some(root)),
            Root::Missing => Ok(Vec::new()),
        }
    }
    .await;
    respond("project_artifacts_walk", r)
}

pub(super) fn project_scaffold_claude(state: &AppState, args: &Value) -> RpcResponse {
    let r = (|| {
        let root_path: String = targ(args, &["rootPath", "root_path"])?;
        // The fs allowlist is installed from `--data-dir`; without one there
        // is no boundary to check against, so say which flag is missing.
        data_dir(state, super::rpc::NO_DB)?;
        if !Path::new(&root_path).is_dir() {
            return Err(format!("root_path is not a directory: {root_path}"));
        }
        let root = scaffold_root(state, &root_path)?;
        projects::scaffold_claude(&root, FsReach::Confined)
    })();
    respond("project_scaffold_claude", r)
}

// ─── Activity-bar pins / sections ────────────────────────────────────────────

pub(super) async fn activity_sections_list(state: &AppState) -> RpcResponse {
    let r = async { activity_bar::sections_list(pa_db(state)?).await }.await;
    respond("activity_sections_list", r)
}

pub(super) async fn activity_sections_create(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        let label: String = targ(args, &["label"])?;
        let icon_lucide: Option<String> = targ(args, &["iconLucide", "icon_lucide"])?;
        let icon_emoji: Option<String> = targ(args, &["iconEmoji", "icon_emoji"])?;
        activity_bar::sections_create(pa_db(state)?, id, label, icon_lucide, icon_emoji).await
    }
    .await;
    respond("activity_sections_create", r)
}

pub(super) async fn activity_sections_update(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        let label: Option<String> = targ(args, &["label"])?;
        let icon_lucide: Option<Option<String>> = targ(args, &["iconLucide", "icon_lucide"])?;
        let icon_emoji: Option<Option<String>> = targ(args, &["iconEmoji", "icon_emoji"])?;
        activity_bar::sections_update(pa_db(state)?, id, label, icon_lucide, icon_emoji).await
    }
    .await;
    respond("activity_sections_update", r)
}

pub(super) async fn activity_sections_remove(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        activity_bar::sections_remove(pa_db(state)?, id).await
    }
    .await;
    respond("activity_sections_remove", r)
}

pub(super) async fn activity_pins_list(state: &AppState) -> RpcResponse {
    let r = async { activity_bar::pins_list(pa_db(state)?).await }.await;
    respond("activity_pins_list", r)
}

/// A pin's `target` is a stored string the FE interprets (a route, a URL, a
/// path it opens through the fs arms, which have their own boundary); no
/// daemon arm opens it.
pub(super) async fn activity_pins_add(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let kind: String = targ(args, &["kind"])?;
        let target: String = targ(args, &["target"])?;
        let label: String = targ(args, &["label"])?;
        let icon_lucide: Option<String> = targ(args, &["iconLucide", "icon_lucide"])?;
        let icon_emoji: Option<String> = targ(args, &["iconEmoji", "icon_emoji"])?;
        let section_id: Option<String> = targ(args, &["sectionId", "section_id"])?;
        let manifest_id: Option<String> = targ(args, &["manifestId", "manifest_id"])?;
        activity_bar::pins_add(
            pa_db(state)?,
            kind,
            target,
            label,
            icon_lucide,
            icon_emoji,
            section_id,
            manifest_id,
        )
        .await
    }
    .await;
    respond("activity_pins_add", r)
}

pub(super) async fn activity_pins_resolve_artifact(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let manifest_id: String = targ(args, &["manifestId", "manifest_id"])?;
        activity_bar::pins_resolve_artifact(pa_db(state)?, &manifest_id).await
    }
    .await;
    respond("activity_pins_resolve_artifact", r)
}

pub(super) async fn activity_pins_touch_open(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pin_id: String = targ(args, &["pinId", "pin_id"])?;
        activity_bar::pins_touch_open(pa_db(state)?, &pin_id).await
    }
    .await;
    respond("activity_pins_touch_open", r)
}

pub(super) async fn activity_pins_remove(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        activity_bar::pins_remove(pa_db(state)?, id).await
    }
    .await;
    respond("activity_pins_remove", r)
}

pub(super) async fn activity_pins_reorder(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let ordered_ids: Vec<String> = targ(args, &["orderedIds", "ordered_ids"])?;
        let section_id: String = targ(args, &["sectionId", "section_id"])?;
        activity_bar::pins_reorder(pa_db(state)?, ordered_ids, section_id).await
    }
    .await;
    respond("activity_pins_reorder", r)
}

// ─── Comments ────────────────────────────────────────────────────────────────

/// A comment's `screenshotPath` is the one stored path something later opens:
/// `comment_route` (desktop-only) hands it to an agent as "Screenshot: …".
/// The desktop's comes from `pin_screenshot_write`, under
/// `<app_data_dir>/pin-screenshots/`; the daemon serves no screenshot write,
/// so it accepts only an existing file directly in `<data-dir>/pin-screenshots/`
/// (stored canonical), or none. Blank passes through as the desktop stores it.
fn check_screenshot_path(state: &AppState, path: Option<String>) -> Result<Option<String>, String> {
    let Some(raw) = path else { return Ok(None) };
    if raw.trim().is_empty() {
        return Ok(Some(raw));
    }
    let dir = data_dir(state, super::rpc::NO_DB)?.join(comments::SCREENSHOTS_DIR);
    let refuse = || {
        format!(
            "screenshotPath must be a file in {} (the daemon does not serve pin_screenshot_write)",
            dir.display()
        )
    };
    let canonical_dir = dir.canonicalize().map_err(|_| refuse())?;
    let canonical = Path::new(&raw).canonicalize().map_err(|_| refuse())?;
    if canonical.parent() != Some(canonical_dir.as_path()) || !canonical.is_file() {
        return Err(refuse());
    }
    Ok(Some(canonical.to_string_lossy().into_owned()))
}

pub(super) async fn comment_create(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let artifact_path: String = targ(args, &["artifactPath", "artifact_path"])?;
        let selector: String = targ(args, &["selector"])?;
        let text: String = targ(args, &["text"])?;
        let screenshot_path: Option<String> = targ(args, &["screenshotPath", "screenshot_path"])?;
        let position_x: Option<f64> = targ(args, &["positionX", "position_x"])?;
        let position_y: Option<f64> = targ(args, &["positionY", "position_y"])?;
        let db = pa_db(state)?;
        let screenshot_path = check_screenshot_path(state, screenshot_path)?;
        comments::create(
            db,
            artifact_path,
            selector,
            text,
            screenshot_path,
            position_x,
            position_y,
        )
        .await
    }
    .await;
    respond("comment_create", r)
}

pub(super) async fn comment_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: i64 = targ(args, &["id"])?;
        comments::get(pa_db(state)?, id).await
    }
    .await;
    respond("comment_get", r)
}

pub(super) async fn comment_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let artifact_path: Option<String> = targ(args, &["artifactPath", "artifact_path"])?;
        let include_resolved: Option<bool> = targ(args, &["includeResolved", "include_resolved"])?;
        comments::list(pa_db(state)?, artifact_path, include_resolved).await
    }
    .await;
    respond("comment_list", r)
}

pub(super) async fn comment_record_routing(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: i64 = targ(args, &["id"])?;
        let sink: String = targ(args, &["sink"])?;
        let thread_id: Option<String> = targ(args, &["threadId", "thread_id"])?;
        let opening_session_id: Option<String> =
            targ(args, &["openingSessionId", "opening_session_id"])?;
        comments::record_routing(pa_db(state)?, id, sink, thread_id, opening_session_id).await
    }
    .await;
    respond("comment_record_routing", r)
}

pub(super) async fn comment_set_status(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: i64 = targ(args, &["id"])?;
        let status: String = targ(args, &["status"])?;
        comments::set_status(pa_db(state)?, id, status).await
    }
    .await;
    respond("comment_set_status", r)
}

pub(super) async fn comment_delete(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: i64 = targ(args, &["id"])?;
        comments::delete(pa_db(state)?, id).await
    }
    .await;
    respond("comment_delete", r)
}

// ─── Studio threads ──────────────────────────────────────────────────────────

pub(super) async fn studio_thread_get_or_create(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let folder_path: String = targ(args, &["folderPath", "folder_path"])?;
        studio_threads::thread_get_or_create(pa_db(state)?, folder_path).await
    }
    .await;
    respond("studio_thread_get_or_create", r)
}

pub(super) async fn studio_thread_get(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        studio_threads::thread_get(pa_db(state)?, id).await
    }
    .await;
    respond("studio_thread_get", r)
}

pub(super) async fn studio_thread_list_recent(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let limit: Option<i64> = targ(args, &["limit"])?;
        studio_threads::thread_list_recent(pa_db(state)?, limit).await
    }
    .await;
    respond("studio_thread_list_recent", r)
}

pub(super) async fn studio_thread_delete(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: String = targ(args, &["id"])?;
        studio_threads::thread_delete(pa_db(state)?, id).await
    }
    .await;
    respond("studio_thread_delete", r)
}

pub(super) async fn studio_message_append(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let thread_id: String = targ(args, &["threadId", "thread_id"])?;
        let role: String = targ(args, &["role"])?;
        let content_md: String = targ(args, &["contentMd", "content_md"])?;
        let scope_chip_json: Option<String> = targ(args, &["scopeChipJson", "scope_chip_json"])?;
        studio_threads::message_append(pa_db(state)?, thread_id, role, content_md, scope_chip_json)
            .await
    }
    .await;
    respond("studio_message_append", r)
}

pub(super) async fn studio_message_list(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let thread_id: String = targ(args, &["threadId", "thread_id"])?;
        let limit: Option<i64> = targ(args, &["limit"])?;
        let before_created_at: Option<i64> = targ(args, &["beforeCreatedAt", "before_created_at"])?;
        studio_threads::message_list(pa_db(state)?, thread_id, limit, before_created_at).await
    }
    .await;
    respond("studio_message_list", r)
}

#[cfg(test)]
mod tests {
    //! House pattern (see `rpc_local`'s tests): a literal `ServerConfig` →
    //! the router → `oneshot` POST `/api/rpc` with the bearer token. The home
    //! and the fs allowlist are pinned to temp dirs (`router_with`), so
    //! nothing here touches the real `~/.ikenga` or installs the
    //! process-global `fs_roots`.

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
    use crate::server::shared::notifications::{
        self, Coalesce, ListQuery, NewNotification, NotificationKind,
    };
    use crate::server::shared::projects::{self, FsReach};
    use crate::server::shared::{activity_bar, comments, studio_threads};
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
        let db = Arc::new(PaDb::new(data.join("ikenga.db")));
        let router = router_with(
            config(Some(data.clone())),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            Some(db.clone()),
            None,
            with_home.then(|| home.clone()),
            PathGuard::Roots(Arc::new(roots)),
        );
        Daemon {
            _tmp: tmp,
            data,
            home,
            allowed,
            outside,
            db,
            router,
        }
    }

    fn daemon() -> Daemon {
        daemon_with_home(true)
    }

    /// No `--data-dir` (so no `PaDb`, no settings, no allowlist).
    fn bare() -> Router {
        router_with(
            config(None),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
            None,
            PathGuard::Allowlist,
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

    fn wire<T: serde::Serialize>(v: T) -> Value {
        serde_json::to_value(v).unwrap()
    }

    // ── no data dir ─────────────────────────────────────────────────────────

    /// Every arm in the slice reads or writes `ikenga.db`, so without
    /// `--data-dir` each names the flag — never an empty answer. (The two
    /// root-less project fs reads are the exception that proves it: they are
    /// the desktop's constant answer and read nothing.)
    #[tokio::test]
    async fn every_arm_without_data_dir_names_the_flag() {
        let r = bare();
        for (cmd, args) in [
            ("notifications_list", json!({})),
            ("notifications_list", json!({ "includeMuted": true })),
            ("notifications_unread_count", json!({})),
            ("notifications_mark_read", json!({ "ids": [1] })),
            ("notifications_mark_all_read", json!({})),
            ("notifications_mute_state", json!({})),
            ("notifications_mute_kind", json!({ "kind": "update" })),
            ("notifications_unmute_kind", json!({ "kind": "update" })),
            ("project_list", json!({})),
            ("project_get_active", json!({})),
            ("project_create", json!({ "id": "p", "displayName": "P" })),
            ("project_update", json!({ "id": "p", "patch": {} })),
            ("project_archive", json!({ "id": "p" })),
            ("project_set_active", json!({ "id": "p" })),
            ("project_inventory", json!({ "rootPath": "/tmp" })),
            (
                "project_skills_list",
                json!({ "rootPath": "/tmp", "includeUserGlobal": false }),
            ),
            ("project_artifacts_walk", json!({ "rootPath": "/tmp" })),
            ("project_scaffold_claude", json!({ "rootPath": "/tmp" })),
            ("activity_sections_list", json!({})),
            (
                "activity_sections_create",
                json!({ "id": "s", "label": "S" }),
            ),
            ("activity_sections_update", json!({ "id": "s" })),
            ("activity_sections_remove", json!({ "id": "s" })),
            ("activity_pins_list", json!({})),
            (
                "activity_pins_add",
                json!({ "kind": "route", "target": "/x", "label": "X" }),
            ),
            (
                "activity_pins_resolve_artifact",
                json!({ "manifestId": "m" }),
            ),
            ("activity_pins_touch_open", json!({ "pinId": "p" })),
            ("activity_pins_remove", json!({ "id": "p" })),
            (
                "activity_pins_reorder",
                json!({ "orderedIds": [], "sectionId": "" }),
            ),
            (
                "comment_create",
                json!({ "artifactPath": "a", "selector": "s", "text": "t" }),
            ),
            ("comment_get", json!({ "id": 1 })),
            ("comment_list", json!({})),
            (
                "comment_record_routing",
                json!({ "id": 1, "sink": "terminal" }),
            ),
            ("comment_set_status", json!({ "id": 1, "status": "open" })),
            ("comment_delete", json!({ "id": 1 })),
            ("studio_thread_get_or_create", json!({ "folderPath": "/f" })),
            ("studio_thread_get", json!({ "id": "t" })),
            ("studio_thread_list_recent", json!({})),
            ("studio_thread_delete", json!({ "id": "t" })),
            (
                "studio_message_append",
                json!({ "threadId": "t", "role": "user", "contentMd": "hi" }),
            ),
            ("studio_message_list", json!({ "threadId": "t" })),
        ] {
            let e = err(&r, cmd, args).await;
            assert!(e.contains("--data-dir"), "{cmd}: {e}");
            assert!(e.starts_with(&format!("{cmd}: ")), "{cmd}: {e}");
        }
        // Root-less: the desktop's constant answers, nothing read.
        let inv = ok(&r, "project_inventory", json!({ "rootPath": null })).await;
        assert_eq!(
            inv,
            wire(projects::inventory(None, FsReach::Follow).unwrap())
        );
        let walk = ok(&r, "project_artifacts_walk", json!({})).await;
        assert_eq!(walk, json!([]));
    }

    // ── Notifications ───────────────────────────────────────────────────────

    fn note(kind: NotificationKind, title: &str) -> NewNotification {
        NewNotification {
            kind,
            title: title.into(),
            body: Some("body".into()),
            action: None,
            source: "test".into(),
            dedupe_key: None,
            coalesce: Coalesce::Never,
        }
    }

    #[tokio::test]
    async fn notifications_list_count_and_read_state_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        for (kind, title) in [
            (NotificationKind::Update, "u1"),
            (NotificationKind::RunFinished, "r1"),
            (NotificationKind::RunFailed, "f1"),
        ] {
            notifications::record(&pool, note(kind, title))
                .await
                .unwrap();
        }

        // Shape parity: the same serialized rows the core returns.
        let listed = ok(r, "notifications_list", json!({})).await;
        let direct = notifications::list(&pool, &ListQuery::default())
            .await
            .unwrap();
        assert_eq!(listed, wire(&direct));
        assert_eq!(listed.as_array().unwrap().len(), 3);

        let count = ok(r, "notifications_unread_count", json!({})).await;
        assert_eq!(
            count,
            wire(notifications::unread_count(&pool, &[]).await.unwrap())
        );
        assert_eq!(count["total"], 3);

        // Both spellings of the filters.
        let camel = ok(
            r,
            "notifications_list",
            json!({ "kinds": ["update"], "unreadOnly": true, "limit": 10 }),
        )
        .await;
        let snake = ok(
            r,
            "notifications_list",
            json!({ "kinds": ["update"], "unread_only": true }),
        )
        .await;
        assert_eq!(camel, snake);
        assert_eq!(camel.as_array().unwrap().len(), 1);
        assert_eq!(camel[0]["title"], "u1");

        // Unknown kind: the desktop's parse error.
        let e = err(r, "notifications_list", json!({ "kinds": ["toast"] })).await;
        assert!(
            e.contains(&NotificationKind::parse("toast").unwrap_err()),
            "{e}"
        );
        let e = err(r, "notifications_mark_all_read", json!({ "kind": "toast" })).await;
        assert!(e.contains("toast"), "{e}");
        // Wrong type is a caller error, as Tauri would refuse it.
        let e = err(r, "notifications_mark_read", json!({ "ids": "1" })).await;
        assert!(e.contains("invalid `ids`"), "{e}");
        let e = err(r, "notifications_mark_read", json!({})).await;
        assert!(e.contains("`ids` is required"), "{e}");

        let first = direct.iter().find(|n| n.title == "u1").unwrap().id;
        assert_eq!(
            ok(
                r,
                "notifications_mark_read",
                json!({ "ids": [first, 99999] })
            )
            .await,
            1
        );
        assert_eq!(
            ok(
                r,
                "notifications_mark_all_read",
                json!({ "kind": "run_failed" })
            )
            .await,
            1
        );
        assert_eq!(
            ok(r, "notifications_unread_count", json!({})).await["total"],
            1
        );
        assert_eq!(
            ok(r, "notifications_mark_all_read", json!({ "kind": null })).await,
            1
        );
        assert_eq!(
            ok(r, "notifications_unread_count", json!({})).await["total"],
            0
        );
    }

    #[tokio::test]
    async fn notifications_mute_round_trips_through_the_daemon_home() {
        let d = daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();
        notifications::record(&pool, note(NotificationKind::Update, "u"))
            .await
            .unwrap();
        notifications::record(&pool, note(NotificationKind::RunFinished, "r"))
            .await
            .unwrap();

        let state = ok(r, "notifications_mute_state", json!({})).await;
        assert_eq!(state["muted"], json!([]));
        assert!(!state["mutable"]
            .as_array()
            .unwrap()
            .contains(&json!("permission")));

        let state = ok(r, "notifications_mute_kind", json!({ "kind": "update" })).await;
        assert_eq!(state["muted"], json!(["update"]));
        assert_eq!(
            state,
            wire(notifications::mute::MuteState::from_muted(vec![
                NotificationKind::Update
            ]))
        );
        // Written to the personal settings file under the router's home.
        let personal: Value = serde_json::from_str(
            &std::fs::read_to_string(d.home.join(".ikenga").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            personal["workspace"]["notifications"]["mutedKinds"],
            json!(["update"])
        );

        // Muted rows are hidden unless asked for, in both spellings.
        let visible = ok(r, "notifications_list", json!({})).await;
        assert_eq!(visible.as_array().unwrap().len(), 1);
        assert_eq!(visible[0]["kind"], "run_finished");
        let all = ok(r, "notifications_list", json!({ "includeMuted": true })).await;
        assert_eq!(all.as_array().unwrap().len(), 2);
        let all = ok(r, "notifications_list", json!({ "include_muted": true })).await;
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(
            ok(r, "notifications_unread_count", json!({})).await["total"],
            1
        );

        // Unmutable kinds are refused with the desktop's error.
        let e = err(
            r,
            "notifications_mute_kind",
            json!({ "kind": "permission" }),
        )
        .await;
        assert!(
            e.contains("permission notifications cannot be muted"),
            "{e}"
        );
        let e = err(r, "notifications_mute_kind", json!({ "kind": "toast" })).await;
        assert!(e.contains("toast"), "{e}");

        let state = ok(r, "notifications_unmute_kind", json!({ "kind": "update" })).await;
        assert_eq!(state["muted"], json!([]));
        assert_eq!(
            ok(r, "notifications_list", json!({}))
                .await
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    /// With a data dir but no home there is no personal settings file, so the
    /// reads that hide muted rows say so; asking for muted rows too needs no
    /// settings and works.
    #[tokio::test]
    async fn notifications_without_a_home_name_what_is_missing() {
        let d = daemon_with_home(false);
        let r = &d.router;
        for cmd in [
            "notifications_list",
            "notifications_unread_count",
            "notifications_mute_state",
        ] {
            let e = err(r, cmd, json!({})).await;
            assert!(e.contains("HOME"), "{cmd}: {e}");
        }
        let e = err(r, "notifications_mute_kind", json!({ "kind": "update" })).await;
        assert!(e.contains("HOME"), "{e}");
        assert_eq!(
            ok(r, "notifications_list", json!({ "includeMuted": true })).await,
            json!([])
        );
        assert_eq!(ok(r, "notifications_mark_all_read", json!({})).await, 0);
    }

    // ── Projects (store) ────────────────────────────────────────────────────

    #[tokio::test]
    async fn projects_crud_keeps_the_desktop_semantics() {
        let d = daemon();
        let r = &d.router;
        let pool = d.db.ensure_pool().await.unwrap();

        // Fresh db: the bootstrapped Default project, active.
        let list = ok(r, "project_list", json!({})).await;
        assert_eq!(
            list,
            wire(projects::list_projects(&pool, false).await.unwrap())
        );
        assert_eq!(list.as_array().unwrap().len(), 1);
        let active = ok(r, "project_get_active", json!({})).await;
        assert_eq!(active["id"], "default");
        assert_eq!(
            active,
            wire(projects::get_active_project(&pool).await.unwrap())
        );

        // Both spellings of displayName / rootPath.
        let proj = d.allowed.join("music");
        std::fs::create_dir_all(&proj).unwrap();
        let created = ok(
            r,
            "project_create",
            json!({ "id": "music", "displayName": "Music", "rootPath": proj.to_string_lossy(), "color": null }),
        )
        .await;
        assert_eq!(created["display_name"], "Music");
        assert_eq!(created["root_path"], proj.to_string_lossy().as_ref());
        let created = ok(
            r,
            "project_create",
            json!({ "id": "films", "display_name": "Films" }),
        )
        .await;
        assert_eq!(created["root_path"], Value::Null);

        // The desktop's refusals.
        let e = err(
            r,
            "project_create",
            json!({ "id": "music", "displayName": "Again" }),
        )
        .await;
        assert!(e.contains("project id already exists: music"), "{e}");
        let e = err(
            r,
            "project_create",
            json!({ "id": "Bad Id", "displayName": "x" }),
        )
        .await;
        assert!(e.contains("invalid project id"), "{e}");
        let e = err(r, "project_create", json!({ "id": "x" })).await;
        assert!(e.contains("`displayName` is required"), "{e}");
        let e = err(r, "project_archive", json!({ "id": "default" })).await;
        assert!(e.contains("cannot archive the Default project"), "{e}");
        for cmd in ["project_archive", "project_set_active"] {
            let e = err(r, cmd, json!({ "id": "nope" })).await;
            assert!(e.contains("project not found: nope"), "{cmd}: {e}");
        }
        let e = err(
            r,
            "project_update",
            json!({ "id": "nope", "patch": { "icon": "x" } }),
        )
        .await;
        assert!(e.contains("project not found: nope"), "{e}");

        // `patch` fields are snake_case (Tauri renames only parameter names).
        let updated = ok(
            r,
            "project_update",
            json!({ "id": "films", "patch": { "display_name": "Films!", "position": 7 } }),
        )
        .await;
        assert_eq!(updated["display_name"], "Films!");
        assert_eq!(updated["position"], 7);
        assert_eq!(
            updated,
            wire(
                projects::get_project(&pool, "films")
                    .await
                    .unwrap()
                    .unwrap()
            )
        );

        // set_active → get_active; archive → falls back to Default.
        assert_eq!(
            ok(r, "project_set_active", json!({ "id": "films" })).await,
            Value::Null
        );
        assert_eq!(ok(r, "project_get_active", json!({})).await["id"], "films");
        assert_eq!(
            ok(r, "project_archive", json!({ "id": "films" })).await,
            Value::Null
        );
        assert_eq!(
            ok(r, "project_get_active", json!({})).await["id"],
            "default"
        );
        let e = err(r, "project_set_active", json!({ "id": "films" })).await;
        assert!(e.contains("project is archived: films"), "{e}");
        assert_eq!(
            ok(r, "project_list", json!({}))
                .await
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ok(r, "project_list", json!({ "include_archived": true }))
                .await
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    /// A stored root is later written through (`settings_write_field`
    /// scope `project` writes `<root>/.ikenga/settings.json`), so the daemon
    /// only stores one inside the fs allowlist.
    #[tokio::test]
    async fn project_roots_outside_the_allowlist_are_refused() {
        let d = daemon();
        let r = &d.router;
        let inside = d.allowed.join("p");
        std::fs::create_dir_all(&inside).unwrap();
        let escape = format!("{}/../outside", d.allowed.display());
        for bad in [
            d.outside.to_string_lossy().into_owned(),
            "/".to_string(),
            escape,
            "relative/dir".to_string(),
            d.allowed.join("missing").to_string_lossy().into_owned(),
        ] {
            let e = err(
                r,
                "project_create",
                json!({ "id": "p", "displayName": "P", "rootPath": bad }),
            )
            .await;
            assert!(
                e.contains("outside allowlist")
                    || e.contains("absolute")
                    || e.contains("not a directory"),
                "{bad}: {e}"
            );
        }
        ok(
            r,
            "project_create",
            json!({ "id": "p", "displayName": "P", "rootPath": inside.to_string_lossy() }),
        )
        .await;
        let e = err(
            r,
            "project_update",
            json!({ "id": "p", "patch": { "root_path": d.outside.to_string_lossy() } }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");
        // Blank / null roots pass through as the desktop stores them.
        ok(
            r,
            "project_create",
            json!({ "id": "q", "displayName": "Q", "rootPath": "" }),
        )
        .await;
        ok(
            r,
            "project_update",
            json!({ "id": "p", "patch": { "root_path": null } }),
        )
        .await;
    }

    // ── Projects (filesystem) ───────────────────────────────────────────────

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// `allowed/proj` registered as a project, with real skills, commands,
    /// an MCP file and an artifact.
    async fn project_fixture(d: &Daemon) -> PathBuf {
        let proj = d.allowed.join("proj");
        write(
            &proj.join(".claude/skills/alpha.md"),
            "---\nname: Alpha\ndescription: first\n---\n",
        );
        write(
            &proj.join(".claude/skills/beta/SKILL.md"),
            "---\nname: Beta\n---\n",
        );
        write(&proj.join(".claude/commands/go.md"), "go");
        write(&proj.join(".mcp.json"), r#"{"mcpServers":{"a":{},"b":{}}}"#);
        write(
            &proj.join("site/index.html"),
            r#"<script id="ikenga-manifest">{"name":"Deck","version":"1.0.0","notes":{"kind":"deck"}}</script>"#,
        );
        ok(
            &d.router,
            "project_create",
            json!({ "id": "proj", "displayName": "Proj", "rootPath": proj.to_string_lossy() }),
        )
        .await;
        proj
    }

    #[tokio::test]
    async fn project_fs_reads_match_the_desktop_inside_a_registered_root() {
        let d = daemon();
        let r = &d.router;
        let proj = project_fixture(&d).await;
        let root = proj.to_string_lossy().into_owned();
        write(
            &d.home.join(".claude/skills/gamma.md"),
            "---\nname: Gamma\n---\n",
        );
        write(
            &d.home.join(".claude/skills/alpha.md"),
            "---\nname: Shadowed\n---\n",
        );

        let inv = ok(r, "project_inventory", json!({ "rootPath": root })).await;
        assert_eq!(
            inv,
            wire(projects::inventory(Some(root.clone()), FsReach::Follow).unwrap())
        );
        assert_eq!(
            (
                inv["skills"].as_u64(),
                inv["commands"].as_u64(),
                inv["mcp"].as_u64()
            ),
            (Some(2), Some(1), Some(2))
        );
        assert_eq!(
            ok(r, "project_inventory", json!({ "root_path": root })).await,
            inv
        );

        let skills = ok(
            r,
            "project_skills_list",
            json!({ "rootPath": root, "includeUserGlobal": true }),
        )
        .await;
        let direct =
            projects::skills_list(Some(root.clone()), true, Some(&d.home), FsReach::Follow)
                .unwrap();
        assert_eq!(skills, wire(&direct));
        let slugs: Vec<&str> = skills
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["slug"].as_str().unwrap())
            .collect();
        assert_eq!(
            slugs,
            ["alpha", "beta", "gamma"],
            "project alpha wins over the user copy"
        );
        let snake = ok(
            r,
            "project_skills_list",
            json!({ "root_path": root, "include_user_global": false }),
        )
        .await;
        assert_eq!(snake.as_array().unwrap().len(), 2);
        let e = err(r, "project_skills_list", json!({ "rootPath": root })).await;
        assert!(e.contains("`includeUserGlobal` is required"), "{e}");

        let walk = ok(r, "project_artifacts_walk", json!({ "rootPath": root })).await;
        assert_eq!(
            walk,
            wire(projects::artifacts_walk(Some(root.clone())).unwrap())
        );
        assert_eq!(walk[0]["name"], "Deck");
        assert_eq!(walk[0]["kind"], "deck");
    }

    /// The read arms walk only an active project's root inside the allowlist.
    #[tokio::test]
    async fn project_fs_reads_refuse_paths_that_are_not_a_project_root() {
        let d = daemon();
        let r = &d.router;
        let proj = project_fixture(&d).await;
        write(
            &d.outside.join(".claude/skills/secret.md"),
            "---\nname: Secret\n---\n",
        );
        // Registered but outside the allowlist: a row the daemon did not write
        // (the arms refuse it at use, not only at registration).
        let pool = d.db.ensure_pool().await.unwrap();
        sqlx::query("INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) VALUES ('out', 'Out', ?, 9, 0, 0)")
            .bind(d.outside.to_string_lossy().as_ref())
            .execute(&pool)
            .await
            .unwrap();

        let dotdot = format!("{}/../../outside", proj.display());
        for (cmd, extra) in [
            ("project_inventory", json!({})),
            ("project_skills_list", json!({ "includeUserGlobal": false })),
            ("project_artifacts_walk", json!({})),
        ] {
            for (bad, want) in [
                ("/".to_string(), "not the root of an active project"),
                (
                    d.allowed.to_string_lossy().into_owned(),
                    "not the root of an active project",
                ),
                (
                    proj.join(".claude").to_string_lossy().into_owned(),
                    "not the root of an active project",
                ),
                (dotdot.clone(), "outside allowlist"),
                ("proj".to_string(), "absolute"),
                (
                    d.outside.to_string_lossy().into_owned(),
                    "outside allowlist",
                ),
            ] {
                let mut args = extra.clone();
                args["rootPath"] = json!(bad);
                let e = err(r, cmd, args).await;
                assert!(e.contains(want), "{cmd} {bad}: {e}");
            }
        }

        // Archived projects are not active.
        ok(r, "project_archive", json!({ "id": "proj" })).await;
        let e = err(
            r,
            "project_inventory",
            json!({ "rootPath": proj.to_string_lossy() }),
        )
        .await;
        assert!(e.contains("not the root of an active project"), "{e}");
    }

    /// A symlink planted inside a project is not a way out of it: the daemon
    /// reads it as absent where the desktop (unconfined) follows it.
    #[tokio::test]
    #[cfg(unix)]
    async fn project_fs_reads_do_not_follow_symlinks_out_of_the_root() {
        use std::os::unix::fs::symlink;
        let d = daemon();
        let r = &d.router;
        let proj = project_fixture(&d).await;
        let root = proj.to_string_lossy().into_owned();
        write(
            &d.outside.join("evil/SKILL.md"),
            "---\nname: Stolen\ndescription: secret\n---\n",
        );
        write(
            &d.outside.join("mcp.json"),
            r#"{"mcpServers":{"x":{},"y":{},"z":{}}}"#,
        );
        write(&d.outside.join("html/leak.html"), "<p>leak</p>");
        symlink(d.outside.join("evil"), proj.join(".claude/skills/evil")).unwrap();
        std::fs::remove_file(proj.join(".mcp.json")).unwrap();
        symlink(d.outside.join("mcp.json"), proj.join(".mcp.json")).unwrap();
        symlink(d.outside.join("html"), proj.join("site/linked")).unwrap();

        // The desktop follows; that is the difference being pinned.
        let desktop = projects::inventory(Some(root.clone()), FsReach::Follow).unwrap();
        assert_eq!(desktop.mcp, 3);
        assert_eq!(
            desktop.skills, 2,
            "a symlinked dir is not a DirEntry dir either"
        );

        let inv = ok(r, "project_inventory", json!({ "rootPath": root })).await;
        assert_eq!(inv["mcp"], 0, "{inv}");
        assert_eq!(inv["skills"], 2, "{inv}");

        // A real dir whose SKILL.md links out.
        std::fs::create_dir_all(proj.join(".claude/skills/sneaky")).unwrap();
        symlink(
            d.outside.join("evil/SKILL.md"),
            proj.join(".claude/skills/sneaky/SKILL.md"),
        )
        .unwrap();
        let followed =
            projects::skills_list(Some(root.clone()), false, None, FsReach::Follow).unwrap();
        assert!(followed.iter().any(|s| s.name.as_deref() == Some("Stolen")));
        let skills = ok(
            r,
            "project_skills_list",
            json!({ "rootPath": root, "includeUserGlobal": false }),
        )
        .await;
        assert!(!skills.to_string().contains("Stolen"), "{skills}");
        assert!(!skills.to_string().contains("secret"), "{skills}");
        assert_eq!(
            ok(r, "project_inventory", json!({ "rootPath": root })).await["skills"],
            2
        );

        // `.claude` itself linked out: nothing under it counts or lists.
        write(
            &d.outside.join("dot-claude/skills/leaked.md"),
            "---\nname: Leaked\n---\n",
        );
        std::fs::rename(proj.join(".claude"), proj.join("claude-real")).unwrap();
        symlink(d.outside.join("dot-claude"), proj.join(".claude")).unwrap();
        let followed =
            projects::skills_list(Some(root.clone()), false, None, FsReach::Follow).unwrap();
        assert!(
            followed.iter().any(|s| s.slug == "leaked"),
            "the desktop follows it"
        );
        let inv = ok(r, "project_inventory", json!({ "rootPath": root })).await;
        assert_eq!(inv["has_claude_dir"], false, "{inv}");
        assert_eq!(inv["skills"], 0, "{inv}");
        let skills = ok(
            r,
            "project_skills_list",
            json!({ "rootPath": root, "includeUserGlobal": false }),
        )
        .await;
        assert_eq!(skills, json!([]), "{skills}");

        let walk = ok(r, "project_artifacts_walk", json!({ "rootPath": root })).await;
        assert!(!walk.to_string().contains("leak"), "{walk}");
    }

    /// A project row whose root has since been deleted reads as empty, as on
    /// the desktop — not as a refusal, and not as an error about the path.
    #[tokio::test]
    async fn a_registered_root_that_vanished_reads_as_empty() {
        let d = daemon();
        let r = &d.router;
        let proj = project_fixture(&d).await;
        let root = proj.to_string_lossy().into_owned();
        std::fs::remove_dir_all(&proj).unwrap();
        let inv = ok(r, "project_inventory", json!({ "rootPath": root })).await;
        assert_eq!(
            inv,
            wire(projects::inventory(Some(root.clone()), FsReach::Follow).unwrap())
        );
        assert_eq!(
            ok(r, "project_artifacts_walk", json!({ "rootPath": root })).await,
            json!([])
        );
        assert_eq!(
            ok(
                r,
                "project_skills_list",
                json!({ "rootPath": root, "includeUserGlobal": false })
            )
            .await,
            json!([])
        );
    }

    #[tokio::test]
    async fn project_scaffold_stays_inside_the_allowlist() {
        let d = daemon();
        let r = &d.router;
        let picked = d.allowed.join("new-thing");
        std::fs::create_dir_all(&picked).unwrap();
        assert_eq!(
            ok(
                r,
                "project_scaffold_claude",
                json!({ "rootPath": picked.to_string_lossy() })
            )
            .await,
            Value::Null
        );
        assert!(picked.join(".claude/skills").is_dir());
        assert!(picked.join(".claude/commands").is_dir());
        let md = std::fs::read_to_string(picked.join("CLAUDE.md")).unwrap();
        assert!(md.contains("`new-thing`"), "{md}");
        // Idempotent; existing CLAUDE.md untouched.
        std::fs::write(picked.join("CLAUDE.md"), "mine").unwrap();
        ok(
            r,
            "project_scaffold_claude",
            json!({ "root_path": picked.to_string_lossy() }),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(picked.join("CLAUDE.md")).unwrap(),
            "mine"
        );

        let e = err(
            r,
            "project_scaffold_claude",
            json!({ "rootPath": d.outside.to_string_lossy() }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(!d.outside.join(".claude").exists());
        let escape = format!("{}/../outside", d.allowed.display());
        let e = err(r, "project_scaffold_claude", json!({ "rootPath": escape })).await;
        assert!(e.contains("outside allowlist"), "{e}");
        let e = err(
            r,
            "project_scaffold_claude",
            json!({ "rootPath": d.allowed.join("nope").to_string_lossy() }),
        )
        .await;
        assert!(e.contains("root_path is not a directory"), "{e}");
        let e = err(r, "project_scaffold_claude", json!({})).await;
        assert!(e.contains("`rootPath` is required"), "{e}");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn project_scaffold_refuses_to_write_through_a_symlink() {
        use std::os::unix::fs::symlink;
        let d = daemon();
        let r = &d.router;
        let a = d.allowed.join("a");
        std::fs::create_dir_all(&a).unwrap();
        symlink(&d.outside, a.join(".claude")).unwrap();
        let e = err(
            r,
            "project_scaffold_claude",
            json!({ "rootPath": a.to_string_lossy() }),
        )
        .await;
        assert!(e.contains("refusing to follow a symlink"), "{e}");
        assert!(!d.outside.join("skills").exists());

        // A dangling CLAUDE.md link would otherwise create its target.
        let b = d.allowed.join("b");
        std::fs::create_dir_all(&b).unwrap();
        symlink(d.outside.join("planted.md"), b.join("CLAUDE.md")).unwrap();
        let e = err(
            r,
            "project_scaffold_claude",
            json!({ "rootPath": b.to_string_lossy() }),
        )
        .await;
        assert!(e.contains("refusing to follow a symlink"), "{e}");
        assert!(!d.outside.join("planted.md").exists());
        assert!(
            !b.join(".claude").exists(),
            "nothing created before the refusal"
        );
    }

    // ── Activity-bar pins / sections ────────────────────────────────────────

    #[tokio::test]
    async fn activity_sections_and_pins_round_trip_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let db = &d.db;

        let s = ok(
            r,
            "activity_sections_create",
            json!({ "id": "work", "label": " Work ", "iconLucide": "briefcase" }),
        )
        .await;
        assert_eq!(s["label"], "Work");
        assert_eq!(s["iconLucide"], "briefcase");
        ok(
            r,
            "activity_sections_create",
            json!({ "id": "play", "label": "Play", "icon_emoji": "🎮" }),
        )
        .await;
        let e = err(
            r,
            "activity_sections_create",
            json!({ "id": "system", "label": "x" }),
        )
        .await;
        assert!(e.contains("'system' is a reserved section id"), "{e}");

        // `null` = leave unchanged, exactly as Tauri decodes Option<Option<_>>.
        let s = ok(
            r,
            "activity_sections_update",
            json!({ "id": "work", "iconLucide": null, "label": "Job" }),
        )
        .await;
        assert_eq!(s["iconLucide"], "briefcase");
        assert_eq!(s["label"], "Job");
        let s = ok(
            r,
            "activity_sections_update",
            json!({ "id": "work", "icon_lucide": "star" }),
        )
        .await;
        assert_eq!(s["iconLucide"], "star");
        let e = err(
            r,
            "activity_sections_update",
            json!({ "id": "ghost", "label": "x" }),
        )
        .await;
        assert!(e.contains("read back section"), "{e}");

        let sections = ok(r, "activity_sections_list", json!({})).await;
        assert_eq!(
            sections,
            wire(activity_bar::sections_list(db).await.unwrap())
        );

        let p1 = ok(
            r,
            "activity_pins_add",
            json!({ "kind": "artifact", "target": "/a.html", "label": "A", "sectionId": "work", "manifestId": "deck-a" }),
        )
        .await;
        let p2 = ok(
            r,
            "activity_pins_add",
            json!({ "kind": "route", "target": "/b", "label": "B", "section_id": "work" }),
        )
        .await;
        let e = err(
            r,
            "activity_pins_add",
            json!({ "kind": "bogus", "target": "t", "label": "l" }),
        )
        .await;
        assert!(e.contains("invalid pin kind 'bogus'"), "{e}");
        let e = err(
            r,
            "activity_pins_add",
            json!({ "kind": "route", "target": "t", "label": "l", "sectionId": "nope" }),
        )
        .await;
        assert!(e.contains("section 'nope' does not exist"), "{e}");
        let e = err(
            r,
            "activity_pins_add",
            json!({ "kind": "artifact", "target": "t", "label": "l", "manifestId": "deck-a" }),
        )
        .await;
        assert!(e.contains("already pinned"), "{e}");

        let resolved = ok(
            r,
            "activity_pins_resolve_artifact",
            json!({ "manifestId": "deck-a" }),
        )
        .await;
        assert_eq!(resolved["id"], p1["id"]);
        assert_eq!(
            ok(
                r,
                "activity_pins_resolve_artifact",
                json!({ "manifest_id": "absent" })
            )
            .await,
            Value::Null
        );

        ok(r, "activity_pins_touch_open", json!({ "pinId": p1["id"] })).await;
        let e = err(r, "activity_pins_touch_open", json!({ "pin_id": "nope" })).await;
        assert!(e.contains("no pin with id 'nope'"), "{e}");

        ok(
            r,
            "activity_pins_reorder",
            json!({ "orderedIds": [p2["id"], p1["id"]], "sectionId": "work" }),
        )
        .await;
        ok(
            r,
            "activity_pins_reorder",
            json!({ "ordered_ids": [p1["id"]], "section_id": "" }),
        )
        .await;
        let pins = ok(r, "activity_pins_list", json!({})).await;
        assert_eq!(pins, wire(activity_bar::pins_list(db).await.unwrap()));
        assert_eq!(pins[0]["id"], p2["id"]);
        assert_eq!(pins[1]["sectionId"], Value::Null, "moved section-less");
        assert!(pins[1]["lastOpenedAt"].is_string());

        ok(r, "activity_pins_remove", json!({ "id": p2["id"] })).await;
        ok(r, "activity_sections_remove", json!({ "id": "work" })).await;
        assert_eq!(
            ok(r, "activity_pins_list", json!({}))
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            ok(r, "activity_sections_list", json!({}))
                .await
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    // ── Comments ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn comments_round_trip_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let c = ok(
            r,
            "comment_create",
            json!({ "artifactPath": "/x/a.html", "selector": "#h1", "text": "fix", "positionX": 1.5, "positionY": null }),
        )
        .await;
        assert_eq!(c["status"], "open");
        assert_eq!(c["positionX"], 1.5);
        let id = c["id"].as_i64().unwrap();
        ok(
            r,
            "comment_create",
            json!({ "artifact_path": "/x/b.html", "selector": "p", "text": "more" }),
        )
        .await;
        let e = err(
            r,
            "comment_create",
            json!({ "artifactPath": " ", "selector": "p", "text": "t" }),
        )
        .await;
        assert!(e.contains("artifact_path cannot be empty"), "{e}");

        assert_eq!(
            ok(r, "comment_get", json!({ "id": id })).await,
            wire(comments::get(&d.db, id).await.unwrap())
        );
        let e = err(r, "comment_get", json!({ "id": 999 })).await;
        assert!(e.contains("comment 999 not found"), "{e}");
        let e = err(r, "comment_get", json!({ "id": "1" })).await;
        assert!(e.contains("invalid `id`"), "{e}");

        let c = ok(
            r,
            "comment_set_status",
            json!({ "id": id, "status": "in_progress" }),
        )
        .await;
        assert!(c["acknowledgedAt"].is_i64());
        let e = err(
            r,
            "comment_set_status",
            json!({ "id": id, "status": "queued" }),
        )
        .await;
        assert!(e.contains("invalid status 'queued'"), "{e}");
        let c = ok(
            r,
            "comment_record_routing",
            json!({ "id": id, "sink": "terminal", "threadId": "t1", "opening_session_id": "s1" }),
        )
        .await;
        assert_eq!(
            (
                c["sink"].as_str(),
                c["threadId"].as_str(),
                c["openingSessionId"].as_str()
            ),
            (Some("terminal"), Some("t1"), Some("s1"))
        );
        let e = err(
            r,
            "comment_record_routing",
            json!({ "id": id, "sink": "kafka" }),
        )
        .await;
        assert!(e.contains("invalid sink 'kafka'"), "{e}");

        ok(
            r,
            "comment_set_status",
            json!({ "id": id, "status": "resolved" }),
        )
        .await;
        let open = ok(r, "comment_list", json!({})).await;
        assert_eq!(open, wire(comments::list(&d.db, None, None).await.unwrap()));
        assert_eq!(open.as_array().unwrap().len(), 1);
        let one = ok(
            r,
            "comment_list",
            json!({ "artifactPath": "/x/a.html", "includeResolved": true }),
        )
        .await;
        assert_eq!(one.as_array().unwrap().len(), 1);
        let one = ok(
            r,
            "comment_list",
            json!({ "artifact_path": "/x/a.html", "include_resolved": false }),
        )
        .await;
        assert_eq!(one, json!([]));

        ok(r, "comment_delete", json!({ "id": id })).await;
        let e = err(r, "comment_get", json!({ "id": id })).await;
        assert!(e.contains("not found"), "{e}");
    }

    /// `comment_route` later hands `screenshotPath` to an agent, so the
    /// daemon only stores one that is a file in `<data-dir>/pin-screenshots/`.
    #[tokio::test]
    async fn comment_screenshot_paths_stay_in_the_data_dir() {
        let d = daemon();
        let r = &d.router;
        let shots = d.data.join(comments::SCREENSHOTS_DIR);
        write(&shots.join("ok.png"), "png");
        write(&d.outside.join("secret.png"), "png");
        let base = json!({ "artifactPath": "a", "selector": "s", "text": "t" });
        let with = |p: String| {
            let mut a = base.clone();
            a["screenshotPath"] = json!(p);
            a
        };
        for bad in [
            d.outside.join("secret.png").to_string_lossy().into_owned(),
            d.data.join("ikenga.db").to_string_lossy().into_owned(),
            format!("{}/../ikenga.db", shots.display()),
            shots.join("missing.png").to_string_lossy().into_owned(),
            shots.to_string_lossy().into_owned(),
            "/etc/passwd".to_string(),
        ] {
            let e = err(r, "comment_create", with(bad.clone())).await;
            assert!(e.contains("screenshotPath must be a file in"), "{bad}: {e}");
        }
        let c = ok(
            r,
            "comment_create",
            with(shots.join("ok.png").to_string_lossy().into_owned()),
        )
        .await;
        assert_eq!(
            c["screenshotPath"],
            shots
                .canonicalize()
                .unwrap()
                .join("ok.png")
                .to_string_lossy()
                .as_ref()
        );
        let c = ok(r, "comment_create", base.clone()).await;
        assert_eq!(c["screenshotPath"], Value::Null);
    }

    // ── Studio threads ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn studio_threads_round_trip_in_the_desktop_shape() {
        let d = daemon();
        let r = &d.router;
        let t = ok(
            r,
            "studio_thread_get_or_create",
            json!({ "folderPath": " /work/deck " }),
        )
        .await;
        assert_eq!(t["folderPath"], "/work/deck");
        let again = ok(
            r,
            "studio_thread_get_or_create",
            json!({ "folder_path": "/work/deck" }),
        )
        .await;
        assert_eq!(again, t, "idempotent per folder");
        let e = err(
            r,
            "studio_thread_get_or_create",
            json!({ "folderPath": "  " }),
        )
        .await;
        assert!(e.contains("folder_path cannot be empty"), "{e}");
        let tid = t["id"].as_str().unwrap().to_string();

        assert_eq!(
            ok(r, "studio_thread_get", json!({ "id": tid })).await,
            wire(
                studio_threads::thread_get(&d.db, tid.clone())
                    .await
                    .unwrap()
            )
        );
        let e = err(r, "studio_thread_get", json!({ "id": "nope" })).await;
        assert!(e.contains("thread nope not found"), "{e}");

        let m = ok(
            r,
            "studio_message_append",
            json!({ "threadId": tid, "role": "user", "contentMd": "hi", "scopeChipJson": "{\"k\":1}" }),
        )
        .await;
        assert_eq!(m["scopeChipJson"], "{\"k\":1}");
        ok(
            r,
            "studio_message_append",
            json!({ "thread_id": tid, "role": "claude", "content_md": "yo" }),
        )
        .await;
        let e = err(
            r,
            "studio_message_append",
            json!({ "threadId": tid, "role": "system", "contentMd": "x" }),
        )
        .await;
        assert!(e.contains("invalid role 'system'"), "{e}");
        let e = err(
            r,
            "studio_message_append",
            json!({ "threadId": tid, "role": "user", "contentMd": "" }),
        )
        .await;
        assert!(e.contains("content_md cannot be empty"), "{e}");

        let msgs = ok(r, "studio_message_list", json!({ "threadId": tid })).await;
        assert_eq!(
            msgs,
            wire(
                studio_threads::message_list(&d.db, tid.clone(), None, None)
                    .await
                    .unwrap()
            )
        );
        assert_eq!(msgs.as_array().unwrap().len(), 2);
        let limited = ok(
            r,
            "studio_message_list",
            json!({ "thread_id": tid, "limit": 1, "before_created_at": null }),
        )
        .await;
        assert_eq!(limited.as_array().unwrap().len(), 1);

        let recent = ok(r, "studio_thread_list_recent", json!({ "limit": 5 })).await;
        assert_eq!(
            recent,
            wire(
                studio_threads::thread_list_recent(&d.db, Some(5))
                    .await
                    .unwrap()
            )
        );
        assert_eq!(recent[0]["id"], tid.as_str());

        ok(r, "studio_thread_delete", json!({ "id": tid })).await;
        assert_eq!(
            ok(r, "studio_thread_list_recent", json!({})).await,
            json!([])
        );
        assert_eq!(
            ok(r, "studio_message_list", json!({ "threadId": tid })).await,
            json!([])
        );
    }
}
