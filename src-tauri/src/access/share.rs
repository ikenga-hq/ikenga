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
//! * [`record_access`] — the `notifications_record_access` internal arm;
//! * `broker_select` (Linux) — the **broker's** share selection (§4.5.2):
//!   membership lookup, the role context and ceiling for §1.4, and the
//!   Owner whose child the request is routed into. `access::t1` calls it
//!   once per request / WS handshake and turns the result into the
//!   `X-Ikenga-Share-*` headers ([`to_child_headers`]) and the proxy target.
//!
//! [`from_child_headers`] / [`to_child_headers`] (WP-74a) are the two ends
//! of the broker → child narrowing headers (§4.5.3); the child reads them
//! only on per-child-token requests.

use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::Value;

use super::caps::Role;
#[cfg(target_os = "linux")]
use super::caps::{CapSet, RoleContext};
use super::ctx::{AccessCtx, ShareCtx};
use super::{AccessError, PreHook};

/// Whether `headers` carry any `X-Ikenga-Share-*` header (§4.5.3: an
/// `internal` arm is accepted only when none is present).
pub fn any_share_header(headers: &HeaderMap) -> bool {
    headers
        .keys()
        .any(|k| k.as_str().starts_with("x-ikenga-share-"))
}

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

/// [`ShareCtx`] → the `X-Ikenga-Share-*` headers the broker sets (§4.5.3);
/// the inverse of [`from_child_headers`]. `X-Ikenga-Principal` (the Owner,
/// the proxy target) is set by the proxy itself.
pub fn to_child_headers(share: &ShareCtx) -> Vec<(&'static str, String)> {
    let mut out = vec![("x-ikenga-share-project", share.project_id.clone())];
    if let Some(m) = &share.member_principal_id {
        out.push(("x-ikenga-share-principal", m.clone()));
    }
    out.push((
        "x-ikenga-share-device",
        share.member_device_id.clone().unwrap_or_else(|| "-".into()),
    ));
    if let Some(r) = share.role {
        out.push(("x-ikenga-share-role", r.as_str().to_string()));
    }
    if let Some(a) = &share.artifact_path {
        out.push(("x-ikenga-share-artifact", a.clone()));
    }
    if share.owner_approval {
        out.push(("x-ikenga-share-policy", "owner-approval".into()));
    }
    out
}

/// The share a client request selects (§4.5.2): `X-Ikenga-Share:
/// <owner>/<project>` on HTTP, `?share=<owner>/<project>` on a WebSocket
/// (it only selects; the credential authenticates). `None` = own workspace.
pub fn selected(parts: &axum::http::request::Parts) -> Option<String> {
    if let Some(v) = parts
        .headers
        .get("x-ikenga-share")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return Some(v.to_string());
    }
    parts.uri.query().and_then(|q| {
        q.split('&').find_map(|p| {
            let (k, v) = p.split_once('=')?;
            let k = percent_encoding::percent_decode_str(k).decode_utf8_lossy();
            (k == "share" && !v.is_empty()).then(|| {
                percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned()
            })
        })
    })
}

/// A share the broker selected for one request (§4.5.2 steps 2–3).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct BrokerShare {
    /// What the child is told (§4.5.3) — and the broker's `AccessCtx.share`.
    pub share: ShareCtx,
    /// `role_caps` for §1.4: the membership's role, the project's
    /// override-applied row and the artifact grant.
    pub context: RoleContext,
    /// §1.4 `share_ceiling` (all seven unless the share narrows further).
    pub ceiling: CapSet,
    /// The project's Owner: the request is routed into **their** child.
    pub owner: crate::executor::Principal,
}

/// The broker's share selection (§4.5.2, **WP-76 fills it**): resolve the
/// membership the request [selects](selected) for `ctx`'s principal, or
/// `Ok(None)` for an own-workspace request. A selection that names no
/// active membership must be `not_found` (no existence oracle). Called once
/// per request / WS handshake by `access::t1::T1Access::access_ctx`.
///
/// Until WP-76 lands, a share selection is refused rather than served
/// unconfined in the caller's own workspace.
#[cfg(target_os = "linux")]
pub async fn broker_select(
    t1: &super::t1::T1Access,
    ctx: &crate::server::auth::PrincipalCtx,
    parts: &axum::http::request::Parts,
) -> Result<Option<BrokerShare>, AccessError> {
    let _ = (t1, ctx);
    match selected(parts) {
        None => Ok(None),
        Some(_) => Err(AccessError::not_implemented("WP-76")),
    }
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

    /// The broker's headers parse back into the same share (§4.5.3).
    #[test]
    fn to_child_headers_round_trips() {
        let share = ShareCtx {
            project_key: "owner/royalti-co".into(),
            project_id: "royalti-co".into(),
            member_principal_id: Some("ada".into()),
            member_device_id: None,
            role: Some(Role::Guest),
            artifact_path: Some("docs/brief.md".into()),
            owner_approval: false,
        };
        let mut h = HeaderMap::new();
        h.insert("x-ikenga-principal", "owner".parse().unwrap());
        for (k, v) in to_child_headers(&share) {
            h.insert(k, v.parse().unwrap());
        }
        assert_eq!(from_child_headers(&h), Some(share));
        assert!(any_share_header(&h));
    }

    #[test]
    fn a_share_is_selected_by_header_or_query() {
        let parts = |uri: &str, header: Option<&str>| {
            let mut b = axum::http::Request::builder().uri(uri);
            if let Some(v) = header {
                b = b.header("x-ikenga-share", v);
            }
            b.body(()).unwrap().into_parts().0
        };
        assert_eq!(selected(&parts("/api/rpc", None)), None);
        assert_eq!(
            selected(&parts("/api/rpc", Some("o/p"))).as_deref(),
            Some("o/p")
        );
        assert_eq!(
            selected(&parts("/ws/chat/x?cols=1&share=o%2Fp", None)).as_deref(),
            Some("o/p")
        );
        assert_eq!(selected(&parts("/ws/chat/x?share=", None)), None);
    }
}
