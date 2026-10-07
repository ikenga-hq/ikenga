//! The `fs_roots_*` arms (gap audit 2026-10-06 rank 1): the folders this
//! daemon lets its caller open — the allowlist every served `fs_*`, project
//! and actions arm checks (`PathGuard`).
//!
//! **Whose list.** The process's own, `<data-dir>/fs_roots.json`:
//!
//! * **T1** — this handler runs in a principal child, as that principal's
//!   uid (topology B: every account has its own Unix user, and `adopt` refuses
//!   a uid another account already holds). A root therefore exposes only
//!   what that uid can read; the OS bounds it, so a principal may list any
//!   directory it can reach, and nothing here confines it to its home. The
//!   child seeds its home on first launch (`FsRoots::load_seeded`). An admin
//!   edits *another* principal's list by naming them (`principal`); the
//!   broker checks that and routes the call into the target's child
//!   (`server::broker::fs_roots_admin`) after removing the argument, so it
//!   never reaches this arm.
//! * **T0** — one owner. The list is theirs (no seed, as before); a
//!   `principal` argument is refused, since there is no one else.
//!
//! Who may call: `owner[files, settings]` (`access::rpc_requirements`) — the
//! owner's password session, the T0 operator bearer, or a `full` device;
//! never a share member, whose requests are confined to the shared project.
//!
//! **Validation.** A root to add must be an absolute path that exists and is
//! a directory; it is stored canonicalized (symlinks resolved, so the stored
//! root is what the checks compare against), and the daemon's own state
//! (`--data-dir`, the discovery file) is refused as a root. No `~` / `$VAR`
//! expansion: the browser cannot see the daemon's environment, so the path
//! the caller typed is the path that is checked.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::rpc::RpcResponse;
use super::AppState;

/// No allowlist was installed: the daemon has no `--data-dir`.
const NO_LIST: &str =
    "Not available on this server: it was started without --data-dir, so it keeps no folder list";

/// A `principal` argument that reached a daemon arm: on T0 there is no one
/// else, and under T1 the broker consumes it before forwarding.
const NO_OTHER_PRINCIPAL: &str = "Not available on this server: it has one person, so there is \
     no one else's folder list to change";

fn refuse_principal(args: &Value) -> Option<RpcResponse> {
    args.get("principal")
        .filter(|v| !v.is_null())
        .map(|_| RpcResponse::error(NO_OTHER_PRINCIPAL))
}

fn path_arg(args: &Value) -> Result<&str, String> {
    args.get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "`path` is required".to_string())
}

/// An absolute, existing directory outside the daemon's own state,
/// canonicalized.
fn validated_dir(state: &AppState, raw: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(format!(
            "{raw} is not an absolute path: give the folder's full path"
        ));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("{raw} does not exist or can't be read ({e})"))?;
    if !canonical.is_dir() {
        return Err(format!("{raw} is not a folder"));
    }
    state.path_guard.check_reserved(&canonical)?;
    Ok(canonical)
}

/// `fs_roots_list` → `string[]`. With no allowlist installed the list is
/// empty, which is also what the checks enforce (every path refused);
/// `reauth-store.ts` probes a token with this arm, so it stays a success.
pub(super) fn fs_roots_list(state: &AppState, args: &Value) -> RpcResponse {
    if let Some(refused) = refuse_principal(args) {
        return refused;
    }
    RpcResponse::success(
        state
            .path_guard
            .allowlist_roots()
            .map(|r| r.list_inputs())
            .unwrap_or_default(),
    )
}

/// `fs_roots_add {path}` → the updated list.
pub(super) fn fs_roots_add(state: &AppState, args: &Value) -> RpcResponse {
    if let Some(refused) = refuse_principal(args) {
        return refused;
    }
    let Some(roots) = state.path_guard.allowlist_roots() else {
        return RpcResponse::error(NO_LIST);
    };
    let dir = match path_arg(args).and_then(|raw| validated_dir(state, raw)) {
        Ok(d) => d,
        Err(e) => return RpcResponse::error(format!("fs_roots_add: {e}")),
    };
    match roots.add(&dir.to_string_lossy()) {
        Ok(list) => RpcResponse::success(list),
        Err(e) => RpcResponse::error(format!("fs_roots_add: {e:#}")),
    }
}

/// `fs_roots_remove {path}` → the updated list. `path` is an entry as
/// `fs_roots_list` shows it, or any spelling that canonicalizes to one.
pub(super) fn fs_roots_remove(state: &AppState, args: &Value) -> RpcResponse {
    if let Some(refused) = refuse_principal(args) {
        return refused;
    }
    let Some(roots) = state.path_guard.allowlist_roots() else {
        return RpcResponse::error(NO_LIST);
    };
    let raw = match path_arg(args) {
        Ok(r) => r,
        Err(e) => return RpcResponse::error(format!("fs_roots_remove: {e}")),
    };
    let listed = roots.list_inputs();
    let entry = listed
        .iter()
        .find(|e| e.as_str() == raw)
        .cloned()
        .or_else(|| {
            let want = Path::new(raw).canonicalize().ok()?;
            listed
                .iter()
                .find(|e| Path::new(e.as_str()).canonicalize().ok().as_ref() == Some(&want))
                .cloned()
        });
    let Some(entry) = entry else {
        return RpcResponse::error(format!("fs_roots_remove: {raw} is not in the folder list"));
    };
    match roots.remove(&entry) {
        Ok(list) => RpcResponse::success(list),
        Err(e) => RpcResponse::error(format!("fs_roots_remove: {e:#}")),
    }
}

/// `fs_roots_reset` → the list it was seeded with (a T1 principal's home;
/// empty on T0).
pub(super) fn fs_roots_reset(state: &AppState, args: &Value) -> RpcResponse {
    if let Some(refused) = refuse_principal(args) {
        return refused;
    }
    let Some(roots) = state.path_guard.allowlist_roots() else {
        return RpcResponse::error(NO_LIST);
    };
    match roots.reset() {
        Ok(list) => RpcResponse::success(list),
        Err(e) => RpcResponse::error(format!("fs_roots_reset: {e:#}")),
    }
}
