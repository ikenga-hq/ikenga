//! The §9.1 arm surface, registered skeleton-first (§9.2).
//!
//! * The **T0 daemon** serves every arm here in-process ([`serve_daemon`],
//!   called from the one arm group at the end of `server::rpc`'s dispatch).
//! * The **T1 broker** intercepts `access_*` and serves it itself, as root
//!   ([`dispatch`] via `access::t1`); it never proxies them.
//! * A **T1 child** answers every `access_*` arm with `served_by_broker`.
//!
//! Arms later waves own dispatch to their stub modules, which answer
//! `internal: not implemented (WP-NN)` until filled; the relay arms answer
//! `invalid_request` until WP-75 (§9.1).

use serde_json::{json, Value};

use super::caps::Tier;
use super::ctx::AccessCtx;
use super::devices;
use super::sockets::Close;
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code, DaemonAccess, DaemonMode};
use crate::server::rpc::RpcResponse;

/// Close / count a device's open sockets: the T0 [`super::sockets::Registry`]
/// or the T1 broker's `ws_registry`.
pub trait SocketControl: Send + Sync {
    fn close_device(&self, device_id: &str, close: Close) -> usize;
    fn live_sockets(&self, device_id: &str) -> usize;
}

impl SocketControl for super::sockets::Registry {
    fn close_device(&self, device_id: &str, close: Close) -> usize {
        super::sockets::Registry::close_device(self, device_id, close)
    }

    fn live_sockets(&self, device_id: &str) -> usize {
        self.count_for_device(device_id)
    }
}

/// The caller's display identity for `AccessStatus.principal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalInfo {
    pub username: String,
    pub is_admin: bool,
}

/// Where an arm is being served.
pub struct Env<'a> {
    pub tier: StoreTier,
    pub store: Option<&'a AccessStore>,
    /// The pairing sessions (§3.1): the T0 daemon's or the T1 broker's;
    /// `None` without a store (pairing is off).
    pub pairing: Option<&'a super::pairing::Registry>,
    pub sockets: &'a dyn SocketControl,
    pub principal: PrincipalInfo,
    pub public_url: Option<String>,
    /// `--insecure-cookie` (§3.8): whether the device cookie drops `Secure`.
    pub insecure_cookie: bool,
}

/// `RpcResponse` for an [`AccessError`] (`"<code>: <message>"`).
pub fn error_response(e: &AccessError) -> RpcResponse {
    RpcResponse::error(e.to_string())
}

fn to_response(r: Result<Value, AccessError>) -> RpcResponse {
    match r {
        Ok(v) => RpcResponse::success(v),
        Err(e) => error_response(&e),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, AccessError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, format!("`{key}` is required")))
}

/// The T0 daemon / T1 child entry (the `server::rpc` arm group). The arm's
/// class and caps were already checked by the `rpc_handler` pre-hook.
pub async fn serve_daemon(
    access: Option<&DaemonAccess>,
    ctx: Option<&AccessCtx>,
    cmd: &str,
    args: &Value,
) -> RpcResponse {
    let Some(ctx) = ctx else {
        return error_response(&AccessError::new(
            Code::Unauthenticated,
            "no access context",
        ));
    };
    let Some(access) = access else {
        return error_response(&AccessError::store_unavailable());
    };
    if access.mode == DaemonMode::PrincipalChild && cmd.starts_with("access_") {
        return error_response(&AccessError::new(
            Code::ServedByBroker,
            "access arms are served by the T1 broker",
        ));
    }
    let env = Env {
        tier: StoreTier::T0,
        store: access.store(),
        pairing: access.store().map(|_| access.pairing.as_ref()),
        sockets: access.sockets.as_ref(),
        principal: PrincipalInfo {
            username: access.host.username.clone(),
            is_admin: false,
        },
        public_url: access.options.public_url.clone(),
        insecure_cookie: access.options.insecure_cookie,
    };
    to_response(dispatch(&env, ctx, cmd, args).await)
}

/// Every §9.1 arm.
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    match cmd {
        "access_status" => Ok(status(env, ctx)),
        "access_devices_list" => devices_list(env, ctx).await,
        "access_device_set_tier" => device_set_tier(env, ctx, args).await,
        "access_device_revoke" => device_revoke(env, ctx, args).await,
        // Pairing (§3, WP-74b).
        "access_pair_begin"
        | "access_pair_cancel"
        | "access_pair_pending"
        | "access_pair_decide" => super::pairing::dispatch(env, ctx, cmd, args).await,
        // Routing preference — WP-75.
        "access_routing_get" | "access_routing_set" => {
            super::routing::dispatch(env, ctx, cmd, args).await
        }
        // The decide core and the T0 ask relay — WP-75
        // (`server::shared::notifications::routing`).
        "permission_decide" => {
            crate::server::shared::notifications::routing::decide_rpc(ctx, args).await
        }
        "permission_relay_put" | "permission_relay_take" | "permission_relay_resolve" => {
            crate::server::shared::notifications::routing::relay_rpc(ctx, cmd, args).await
        }
        // Members, policies, invites, shares — WP-76.
        "access_members_list"
        | "access_member_set_role"
        | "access_member_remove"
        | "access_member_restore"
        | "access_shares_list" => super::members::dispatch(env, ctx, cmd, args).await,
        "access_policy_get" | "access_policy_set_cell" | "access_policy_set_owner_approval" => {
            super::policy::dispatch(env, ctx, cmd, args).await
        }
        "access_invite_issue" | "access_invite_revoke" => {
            super::invites::dispatch(env, ctx, cmd, args).await
        }
        "notifications_record_access" => super::share::record_access(ctx, args),
        "share_project_info" => super::share::project_info(ctx, args),
        // Audit — WP-77.
        "access_audit_list" => super::audit::list::dispatch(env, ctx, args).await,
        "access_audit_verify" => super::audit::list::verify(env, ctx).await,
        "access_audit_export" => super::audit::export::dispatch(env, ctx, args).await,
        "access_audit_record_local" => super::audit::list::record_local(env, ctx, args).await,
        "access_audit_reseal" => super::audit::reseal::dispatch(env, ctx, args).await,
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}

fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

/// `access_status` (§9.1): any authenticated caller.
pub fn status(env: &Env<'_>, ctx: &AccessCtx) -> Value {
    let (store, broken) = match env.store {
        None => ("none", None),
        Some(s) => s.status(),
    };
    let mut v = json!({
        "tier": env.tier.as_str(),
        "store": store,
        "principal": {
            "principalId": ctx.principal_id.to_string(),
            "username": env.principal.username,
            "isAdmin": env.principal.is_admin,
        },
        "credential": ctx.credential_json(),
        "caps": ctx.caps.names(),
        "adminStrength": ctx.admin_strength,
        "publicUrl": env.public_url,
        // §4.5.5 / Round 16: sharing is on exactly under T1.
        "sharingEnabled": env.tier == StoreTier::T1,
        "share": ctx.share.as_ref().map(|s| json!({
            "projectKey": s.project_key,
            "projectName": s.project_id,
            "ownerUsername": Value::Null,
            "role": s.role.map(|r| r.as_str()),
            "scope": if s.artifact_path.is_some() { "artifact" } else { "project" },
            "artifactPath": s.artifact_path,
        })),
    });
    if let Some(seq) = broken {
        v["brokenAtSeq"] = json!(seq);
    }
    v
}

/// `access_devices_list` (§9.1): the caller's principal's live devices;
/// below `admin_strength`, only the caller's own row.
async fn devices_list(env: &Env<'_>, ctx: &AccessCtx) -> Result<Value, AccessError> {
    let store = store(env)?;
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let rows = devices::list_active(&mut conn, &ctx.principal_id.to_string())
        .await
        .map_err(AccessError::internal)?;
    let views: Vec<_> = rows
        .iter()
        .filter(|d| ctx.admin_strength || ctx.device_id.as_deref() == Some(&d.device_id))
        .map(|d| {
            let this = ctx.device_id.as_deref() == Some(&d.device_id);
            d.view(env.sockets.live_sockets(&d.device_id), this)
        })
        .collect();
    serde_json::to_value(views).map_err(AccessError::internal)
}

async fn device_set_tier(
    env: &Env<'_>,
    ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    let store = store(env)?;
    let device_id = str_arg(args, "deviceId")?;
    let tier = Tier::parse(str_arg(args, "tier")?).ok_or_else(|| {
        AccessError::new(
            Code::InvalidRequest,
            "tier must be view|dispatch|approve|full",
        )
    })?;
    let (row, from) = devices::set_tier(store, ctx, device_id, tier).await?;
    if from != row.tier {
        // §3.10: the device reconnects at once with its new caps.
        env.sockets.close_device(device_id, Close::CAPS_CHANGED);
    }
    let this = ctx.device_id.as_deref() == Some(device_id);
    serde_json::to_value(row.view(env.sockets.live_sockets(device_id), this))
        .map_err(AccessError::internal)
}

async fn device_revoke(env: &Env<'_>, ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    let store = store(env)?;
    let device_id = str_arg(args, "deviceId")?;
    devices::revoke(store, ctx, device_id).await?;
    // §3.10: every open socket of the device closes at once (4401).
    env.sockets.close_device(device_id, Close::DEVICE_REVOKED);
    Ok(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::CapSet;
    use crate::access::ctx::{RequestMeta, Via};
    use crate::access::devices::tests::{operator_ctx, pair};
    use crate::access::sockets::Registry;

    fn env<'a>(store: &'a AccessStore, sockets: &'a Registry) -> Env<'a> {
        Env {
            tier: StoreTier::T0,
            store: Some(store),
            pairing: None,
            sockets,
            principal: PrincipalInfo {
                username: "ned".into(),
                is_admin: false,
            },
            public_url: None,
            insecure_cookie: false,
        }
    }

    #[tokio::test]
    async fn status_reports_t0_operator_full() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let ctx = operator_ctx(&store);
        let v = dispatch(&env(&store, &reg), &ctx, "access_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["tier"], "t0");
        assert_eq!(v["store"], "ok");
        assert_eq!(v["credential"]["via"], "operator");
        assert_eq!(v["credential"]["tier"], "full");
        assert_eq!(v["caps"].as_array().unwrap().len(), 7);
        assert_eq!(v["sharingEnabled"], false);
        assert_eq!(v["share"], Value::Null);
        assert_eq!(
            v["principal"]["principalId"],
            store.meta().owner_principal_id.unwrap().to_string()
        );
    }

    /// A-6 / A-7 (in-process): revoke closes the device's sockets 4401, a
    /// tier change 4403; the list shows live sockets and `thisDevice`.
    #[tokio::test]
    async fn revoke_and_tier_change_close_sockets() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let (row, _) = pair(&store, Tier::Dispatch).await;
        let mut sock = reg.register(Some(row.device_id.clone()));
        let op = operator_ctx(&store);
        let e = env(&store, &reg);

        let list = dispatch(&e, &op, "access_devices_list", &json!({}))
            .await
            .unwrap();
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["kind"], "host");
        assert_eq!(list[0]["thisDevice"], true);
        assert_eq!(list[1]["liveSockets"], 1);

        let v = dispatch(
            &e,
            &op,
            "access_device_set_tier",
            &json!({"deviceId": row.device_id, "tier": "approve"}),
        )
        .await
        .unwrap();
        assert_eq!(v["tier"], "approve");
        assert_eq!(sock.closed.try_recv().unwrap().code, 4403);

        let mut sock = reg.register(Some(row.device_id.clone()));
        dispatch(
            &e,
            &op,
            "access_device_revoke",
            &json!({"deviceId": row.device_id}),
        )
        .await
        .unwrap();
        let close = sock.closed.try_recv().unwrap();
        assert_eq!((close.code, close.reason), (4401, "device_revoked"));
    }

    #[tokio::test]
    async fn a_low_tier_device_sees_only_itself_and_cannot_pair() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let (row, _) = pair(&store, Tier::View).await;
        let phone = AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            via: Via::Device {
                device_id: row.device_id.clone(),
            },
            device_id: Some(row.device_id.clone()),
            tier: Tier::View,
            share: None,
            share_headers: false,
            caps: CapSet::of(&[crate::access::Cap::Files, crate::access::Cap::Sessions]),
            admin_strength: false,
            meta: RequestMeta::default(),
        };
        let e = env(&store, &reg);
        let list = dispatch(&e, &phone, "access_devices_list", &json!({}))
            .await
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["thisDevice"], true);
        let err = dispatch(
            &e,
            &phone,
            "access_device_set_tier",
            &json!({"deviceId": row.device_id, "tier": "full"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::Forbidden);
    }

    #[tokio::test]
    async fn stubs_answer_with_their_owning_wp() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let op = operator_ctx(&store);
        let e = env(&store, &reg);
        for (cmd, wp) in [
            ("access_audit_list", "WP-77"),
            ("access_audit_reseal", "WP-77"),
        ] {
            let err = dispatch(&e, &op, cmd, &json!({})).await.unwrap_err();
            assert_eq!(err.code, Code::Internal, "{cmd}");
            assert!(err.message.contains(wp), "{cmd}: {err}");
        }
        // WP-76: on T0 (one principal) the member, invite and share arms
        // answer `requires_t1`, and the policy matrix is the defaults
        // (§4.5.5).
        for cmd in [
            "access_members_list",
            "access_member_set_role",
            "access_invite_issue",
            "access_invite_revoke",
            "access_shares_list",
            "access_policy_set_cell",
        ] {
            let err = dispatch(&e, &op, cmd, &json!({})).await.unwrap_err();
            assert_eq!(err.code, Code::RequiresT1, "{cmd}");
        }
        let policy = dispatch(&e, &op, "access_policy_get", &json!({"projectId": "p"}))
            .await
            .unwrap();
        assert_eq!(policy["matrix"]["reviewer"]["secrets"], "never");
    }

    #[tokio::test]
    async fn no_store_means_store_unavailable() {
        let reg = Registry::new();
        let access = DaemonAccess::unavailable();
        let ctx = access.operator_ctx(RequestMeta::default()).await;
        let e = Env {
            tier: StoreTier::T0,
            store: None,
            pairing: None,
            sockets: &*reg,
            principal: PrincipalInfo {
                username: "u".into(),
                is_admin: false,
            },
            public_url: None,
            insecure_cookie: false,
        };
        let s = dispatch(&e, &ctx, "access_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(s["store"], "none");
        for cmd in ["access_devices_list", "access_pair_begin"] {
            assert_eq!(
                dispatch(&e, &ctx, cmd, &json!({})).await.unwrap_err().code,
                Code::StoreUnavailable,
                "{cmd}"
            );
        }
    }

    #[tokio::test]
    async fn a_principal_child_answers_served_by_broker() {
        let child = DaemonAccess::principal_child(Default::default());
        let ctx = child.child_ctx(&Default::default(), RequestMeta::default());
        let r = serve_daemon(Some(&child), Some(&ctx), "access_status", &json!({})).await;
        assert!(r.error.unwrap().starts_with("served_by_broker:"));
    }
}
