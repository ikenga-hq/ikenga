//! Child-side share confinement (G-ACCESS §4.5.3–§4.5.4) — **stub module,
//! WP-76 fills it**. WP-74a creates every hook site (§9.2, review M-7):
//!
//! * [`prehook`] — the `rpc_handler` pre-hook for a share request: a cloned
//!   `AppState` whose `path_guard` is narrowed, or a reroute of a
//!   share-mode `actions_*` arm to [`actions_dispatch`];
//! * [`filter`] — the post-hook's list filtering / cost stripping;
//! * [`chat_cwd`] — `/ws/chat` `Prompt { cwd }` confinement;
//! * [`run_env`] — no vault env in share-originated runs (§4.5.1);
//! * [`fs_watch_root`] — `/ws/fs` watch-root confinement;
//! * [`project_info`] — the `share_project_info` internal arm;
//! * [`record_access`] — the `notifications_record_access` internal arm.
//!
//! [`from_child_headers`] (WP-74a) parses the broker → child narrowing
//! headers (§4.5.3), read only on per-child-token requests.

use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::Value;

use super::caps::Role;
use super::ctx::{AccessCtx, ShareCtx};
use super::{AccessError, PreHook};

/// `X-Ikenga-Share-*` → [`ShareCtx`] (§4.5.3). `None` when the request
/// carries no `X-Ikenga-Share-Project`. Narrowing only — never a grant.
pub fn from_child_headers(headers: &HeaderMap) -> Option<ShareCtx> {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let project_id = h("x-ikenga-share-project")?;
    let member = h("x-ikenga-share-principal");
    let device = h("x-ikenga-share-device").filter(|d| d != "-");
    Some(ShareCtx {
        project_key: format!(
            "{}/{}",
            h("x-ikenga-principal").unwrap_or_default(),
            project_id
        ),
        project_id,
        member_principal_id: member,
        member_device_id: device,
        role: h("x-ikenga-share-role").and_then(|r| Role::parse(&r)),
        artifact_path: h("x-ikenga-share-artifact"),
        owner_approval: h("x-ikenga-share-policy").as_deref() == Some("owner-approval"),
    })
}

/// The share-mode pre-hook (WP-76). Until it lands a share request is
/// refused rather than served unconfined.
pub async fn prehook(
    _state: &Arc<crate::server::AppState>,
    _ctx: &AccessCtx,
    _share: &ShareCtx,
    _cmd: &str,
    _args: &Value,
) -> PreHook {
    PreHook::Answered(super::rpc::error_response(&AccessError::not_implemented(
        "WP-76",
    )))
}

/// The share-mode post-hook filter (WP-76). No-op until then (the
/// pre-hook refuses every share request meanwhile).
pub fn filter(_ctx: &AccessCtx, _share: &ShareCtx, _cmd: &str, _data: &mut Value) {}

/// Share-mode `actions_*` dispatch on a narrowed guard (WP-76).
pub async fn actions_dispatch(
    _ctx: &AccessCtx,
    _share: &ShareCtx,
    _cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}

/// `/ws/chat` `Prompt { cwd }` under a share: force it under the share root
/// or refuse (WP-76). Own-workspace requests pass `cwd` through unchanged.
pub fn chat_cwd(ctx: &AccessCtx, cwd: Option<String>) -> Result<Option<String>, AccessError> {
    match ctx.share {
        None => Ok(cwd),
        Some(_) => Err(AccessError::not_implemented("WP-76")),
    }
}

/// Share-originated runs get no Ikenga vault env (§4.5.1, WP-76). Returns
/// whether the run may receive vault injection.
pub fn run_env(ctx: &AccessCtx) -> bool {
    ctx.share.is_none()
}

/// `/ws/fs` watch roots under a share (WP-76). Own workspace: unchanged.
pub fn fs_watch_root(ctx: &AccessCtx, root: &str) -> Result<(), AccessError> {
    match ctx.share {
        None => {
            let _ = root;
            Ok(())
        }
        Some(_) => Err(AccessError::not_implemented("WP-76")),
    }
}

/// `share_project_info {projectId}` → `{root, name}` (internal, WP-76).
pub fn project_info(_ctx: &AccessCtx, _args: &Value) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}

/// `notifications_record_access {kind:'invite', title, body}` (internal,
/// WP-76).
pub fn record_access(_ctx: &AccessCtx, _args: &Value) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_headers_parse_and_absent_means_none() {
        assert!(from_child_headers(&HeaderMap::new()).is_none());
        let mut h = HeaderMap::new();
        for (k, v) in [
            ("x-ikenga-principal", "owner"),
            ("x-ikenga-share-project", "royalti-co"),
            ("x-ikenga-share-principal", "ada"),
            ("x-ikenga-share-device", "-"),
            ("x-ikenga-share-role", "reviewer"),
            ("x-ikenga-share-policy", "owner-approval"),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let s = from_child_headers(&h).unwrap();
        assert_eq!(s.project_key, "owner/royalti-co");
        assert_eq!(s.member_device_id, None);
        assert_eq!(s.role, Some(Role::Reviewer));
        assert!(s.owner_approval);
        assert!(s.artifact_path.is_none());
    }
}
