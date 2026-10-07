//! An admin's edit of another principal's folder list (gap audit 2026-10-06
//! rank 1, user decision 2026-10-07).
//!
//! The `fs_roots_*` arms serve the caller's **own** list, in its own child
//! (`server::rpc_fs_roots`). Naming someone else — `principal: <username or
//! principal id>` — is decided here, in the broker, before anything is
//! proxied:
//!
//! * the caller must be an admin account (`accounts.is_admin`, read now, not
//!   from the session) holding a credential of admin strength (P-26: a
//!   password session or a `full` device) — the same pair the broker's
//!   admin-only `access_*` arms require; anyone else gets `forbidden`,
//!   before the target is even looked up, so a refusal says nothing about
//!   who exists;
//! * the call is then routed into the **target's** child (launched if it is
//!   not running, which seeds a never-seeded list first) with `principal`
//!   removed and only `files, settings` granted — exactly the arm's own
//!   requirement — so the child serves it as that principal's own request:
//!   the path is validated and canonicalized as the target's uid, and the
//!   list written is the target's `fs_roots.json`.
//!
//! Naming oneself is the plain own-scope call. Every other command passes
//! through untouched. The R-3 authorizer has already run on the caller's
//! own caps, so a share member never gets here (the arms are `owner` class).

use axum::body::Bytes;
use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use super::proxy::{Narrowing, CAPS_HEADER};
use super::BrokerState;
use crate::access::caps::{Cap, CapSet};
use crate::server::auth::{Credential, PrincipalCtx};
use crate::server::operator::accounts;
use crate::server::rpc::RpcResponse;

/// The commands an admin may aim at another principal.
const CMDS: [&str; 4] = [
    "fs_roots_list",
    "fs_roots_add",
    "fs_roots_remove",
    "fs_roots_reset",
];

/// The argument naming the target principal.
pub const TARGET_ARG: &str = "principal";

fn refuse(msg: &str) -> Response {
    Json(RpcResponse::error(msg)).into_response()
}

/// P-26 for this request: a password session, or a device whose resolved
/// tier is `full` (the narrower's snapshot; none means not proven — fail
/// closed).
fn admin_strength(ctx: &PrincipalCtx, narrowing: &Narrowing) -> bool {
    if let Some(b) = narrowing.snapshot::<crate::access::t1::BrokerCtx>() {
        return b.access.admin_strength;
    }
    matches!(ctx.via, Credential::Session { .. })
}

/// `cmd` + `args` → where the request goes: unchanged for anything but a
/// `fs_roots_*` call that names a principal; for one that does, the
/// caller's own child (naming oneself) or, for an admin, the target's —
/// with the argument removed from the forwarded body. `Err` is the answer
/// to send instead.
pub async fn route(
    state: &BrokerState,
    ctx: &PrincipalCtx,
    narrowing: Narrowing,
    cmd: &str,
    args: &Value,
    body: Bytes,
) -> Result<(Narrowing, Bytes), Response> {
    if !CMDS.contains(&cmd) {
        return Ok((narrowing, body));
    }
    let Some(named) = args.get(TARGET_ARG).filter(|v| !v.is_null()) else {
        return Ok((narrowing, body));
    };
    let Some(named) = named.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(refuse(
            "invalid_request: `principal` must be a username or a principal id",
        ));
    };
    let mut stripped = args.clone();
    if let Some(map) = stripped.as_object_mut() {
        map.remove(TARGET_ARG);
    }
    let body = Bytes::from(serde_json::json!({ "cmd": cmd, "args": stripped }).to_string());

    let is_self = named == ctx.principal.id.to_string()
        || named.eq_ignore_ascii_case(&ctx.principal.username);
    if is_self {
        return Ok((narrowing, body));
    }

    let mut conn = match state.pool.acquire().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("fs_roots admin route: accounts.db: {e}");
            return Err(refuse(
                "internal: the account store failed; see the server log",
            ));
        }
    };
    let caller_admin = matches!(
        accounts::by_id(&mut conn, ctx.principal.id).await,
        Ok(Some(a)) if a.is_admin && !a.is_disabled()
    );
    if !caller_admin || !admin_strength(ctx, &narrowing) || narrowing.target.is_some() {
        return Err(refuse(
            "forbidden: only an admin, signed in with a password or a full-access device, \
             can change another person's folders",
        ));
    }
    let target = match named.parse() {
        Ok(id) => accounts::by_id(&mut conn, id).await,
        Err(_) => accounts::by_username(&mut conn, named).await,
    };
    let target = match target {
        Ok(Some(a)) if !a.is_disabled() => a,
        Ok(_) => return Err(refuse(&format!("not_found: no active account `{named}`"))),
        Err(e) => {
            tracing::error!("fs_roots admin route: accounts.db: {e}");
            return Err(refuse(
                "internal: the account store failed; see the server log",
            ));
        }
    };
    if target.principal_id == ctx.principal.id {
        return Ok((narrowing, body));
    }
    tracing::info!(
        actor = %ctx.principal.id,
        target = %target.principal_id,
        cmd,
        "admin acts on another principal's fs allowlist"
    );
    let caps = CapSet::of(&[Cap::Files, Cap::Settings]).to_header();
    let caps = HeaderValue::from_str(&caps).map_err(|_| refuse("internal: unencodable caps"))?;
    Ok((
        Narrowing {
            headers: vec![(HeaderName::from_static(CAPS_HEADER), caps)],
            target: Some(target.principal()),
            snapshot: narrowing.snapshot,
        },
        body,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::access::caps::Tier;
    use crate::access::ctx::{AccessCtx, RequestMeta, Via};
    use crate::executor::{Principal, PrincipalId};

    fn ctx(via: Credential) -> PrincipalCtx {
        PrincipalCtx {
            principal: Principal {
                id: PrincipalId::new_v7(),
                username: "ada".into(),
                unix_name: "ik-ada".into(),
                uid: 20_001,
                gid: 20_001,
                home: "/srv/ada".into(),
                shell: "/bin/sh".into(),
            },
            via,
        }
    }

    fn snapshot(device_tier: Tier) -> Narrowing {
        let via = Via::Device {
            device_id: "d1".into(),
        };
        let access = AccessCtx {
            principal_id: PrincipalId::new_v7(),
            admin_strength: AccessCtx::admin_strength_of(&via, device_tier),
            via,
            device_id: Some("d1".into()),
            tier: device_tier,
            share: None,
            share_headers: false,
            caps: device_tier.caps(),
            meta: RequestMeta::default(),
        };
        Narrowing {
            snapshot: Some(Arc::new(crate::access::t1::BrokerCtx {
                access,
                owner: None,
                socket: 0,
            })),
            ..Default::default()
        }
    }

    /// P-26: a password session, or a `full` device by the narrower's
    /// snapshot. A device with no snapshot has proven nothing.
    #[test]
    fn admin_strength_is_a_session_or_a_full_device() {
        let session = ctx(Credential::Session {
            session_id: "s".into(),
        });
        assert!(admin_strength(&session, &Narrowing::default()));
        let device = ctx(Credential::DeviceGrant {
            device_id: "d1".into(),
        });
        assert!(!admin_strength(&device, &Narrowing::default()));
        assert!(admin_strength(&device, &snapshot(Tier::Full)));
        for tier in [Tier::View, Tier::Dispatch, Tier::Approve] {
            assert!(!admin_strength(&device, &snapshot(tier)), "{tier}");
        }
    }
}
