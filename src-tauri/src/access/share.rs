//! Child-side share confinement (G-ACCESS §4.5.4). **WP-76 fills this**
//! (W4); WP-74a creates the hook sites and wires them:
//!
//! | hook | called from |
//! |---|---|
//! | [`narrow_state`] | the `rpc_handler` pre-hook (a cloned `AppState` whose `path_guard` is `PathGuard::narrowed_to(…)`) |
//! | [`actions_dispatch`] | the pre-hook, for share-mode `actions_*` / `keybindings_write` |
//! | [`filter`] | the `rpc_handler` post-hook (list filtering, comment ownership, Reviewer cost removal) |
//! | [`chat_cwd`], [`run_env`] | `/ws/chat` `Prompt` |
//! | [`fs_watch_root`] | `/ws/fs` `watch` |
//! | [`share_project_info`] | the `internal` arm of that name |
//!
//! **Fail closed until filled:** every hook passes own-workspace requests
//! untouched and refuses share requests with `internal: not implemented
//! (WP-76)`. No share request reaches an arm unconfined.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use super::ctx::{AccessCtx, ShareCtx};
use super::{not_implemented, RpcResult};
use crate::server::AppState;

/// The `actions_*` arms whose manager captured its own guard at router
/// construction (§4.5.4 table, review M-7).
pub const ACTIONS_ARMS: &[&str] = &[
    "actions_read_files",
    "actions_trust_status",
    "actions_write",
    "keybindings_write",
];

/// For a share request, the `AppState` the arm runs against (path guard
/// narrowed to the project root or the one artifact). `Ok(None)`: run on
/// the router's own state.
pub fn narrow_state(
    ctx: &AccessCtx,
    _cmd: &str,
    _state: &Arc<AppState>,
) -> Result<Option<Arc<AppState>>, String> {
    match ctx.share {
        None => Ok(None),
        Some(_) => Err(not_implemented("WP-76")),
    }
}

/// A share-mode `actions_*` call, on a per-request actions manager over the
/// narrowed guard.
pub async fn actions_dispatch(
    _state: &Arc<AppState>,
    _share: &ShareCtx,
    _cmd: &str,
    _args: &Value,
) -> RpcResult {
    Err(not_implemented("WP-76"))
}

/// Post-filter a successful share-mode result in place (§4.5.4 list
/// table). Own-workspace results pass untouched.
pub fn filter(ctx: &AccessCtx, _cmd: &str, _data: &mut Value) -> Result<(), String> {
    match ctx.share {
        None => Ok(()),
        Some(_) => Err(not_implemented("WP-76")),
    }
}

/// `/ws/chat` `Prompt { cwd }` under a share: forced under the share root,
/// or refused.
pub fn chat_cwd(_share: &ShareCtx, _cwd: Option<&str>) -> Result<Option<String>, String> {
    Err(not_implemented("WP-76"))
}

/// The engine env of a share-originated run: no Ikenga vault injection
/// (§4.5.1, N-10). Returns the env keys to clear.
pub fn run_env(_share: &ShareCtx) -> Result<Vec<String>, String> {
    Err(not_implemented("WP-76"))
}

/// `/ws/fs` watch root under a share: confined to the project root (or the
/// artifact), or refused.
pub fn fs_watch_root(_share: &ShareCtx, _root: &Path) -> Result<PathBuf, String> {
    Err(not_implemented("WP-76"))
}

/// The `share_project_info {projectId} → {root, name}` internal arm
/// (§4.5.4).
pub async fn share_project_info(_state: &AppState, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

/// The `notifications_record_access {kind:'invite', title, body}` internal
/// arm (§7.3, the first `invite` producer).
pub async fn notifications_record_access(
    _state: &AppState,
    _ctx: &AccessCtx,
    _args: &Value,
) -> RpcResult {
    Err(not_implemented("WP-76"))
}
