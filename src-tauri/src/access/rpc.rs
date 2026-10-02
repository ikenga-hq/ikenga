//! The G-ACCESS arm group (§9.1, §9.2): every `access_*` command,
//! `permission_decide`, the `permission_relay_*` arms and the two
//! `internal` arms. `server::rpc` delegates here from one arm group just
//! before its unknown-command fallback; the T1 broker's R-3 `access_*`
//! intercept calls [`dispatch`] with no `AppState`.
//!
//! Serving rules:
//! * **T1 child:** every `access_*` → `served_by_broker` (§1.7).
//! * **T0 daemon without a store:** every `access_*` but `access_status` →
//!   `store_unavailable` (§2.5).
//! * Arms later WPs own dispatch to their stub modules, which answer
//!   `internal: not implemented (WP-NN)`.

use serde_json::{json, Value};

use super::caps::Tier;
use super::ctx::AccessCtx;
use super::{audit, devices, invites, members, policy, routing, share, Mode, RpcResult, Runtime};
use crate::server::shared::notifications::routing as ask_routing;
use crate::server::AppState;

/// Every command of this group, for the tests and the parity ratchet.
pub const COMMANDS: &[&str] = &[
    "access_status",
    "access_devices_list",
    "access_device_set_tier",
    "access_device_revoke",
    "access_pair_begin",
    "access_pair_cancel",
    "access_pair_pending",
    "access_pair_decide",
    "access_routing_get",
    "access_routing_set",
    "access_members_list",
    "access_member_set_role",
    "access_member_remove",
    "access_member_restore",
    "access_policy_get",
    "access_policy_set_cell",
    "access_policy_set_owner_approval",
    "access_invite_issue",
    "access_invite_revoke",
    "access_shares_list",
    "access_audit_list",
    "access_audit_verify",
    "access_audit_export",
    "access_audit_record_local",
    "access_audit_reseal",
    "permission_decide",
    "permission_relay_put",
    "permission_relay_take",
    "permission_relay_resolve",
    "notifications_record_access",
    "share_project_info",
];

/// The non-Tauri verbs of this group (`parity.rs` `DAEMON_ONLY_VERBS`).
pub const DAEMON_ONLY: &[&str] = &[
    "notifications_record_access",
    "share_project_info",
    "permission_relay_put",
    "permission_relay_take",
    "permission_relay_resolve",
];

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("invalid_request: `{key}` is required"))
}

/// Serve one command of this group. `state` is the daemon's / child's
/// `AppState` (the broker has none). Authorization by class has already
/// run (`access::authorize_rpc`); per-command rules run here.
pub async fn dispatch(
    rt: &Runtime,
    state: Option<&AppState>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> RpcResult {
    if cmd.starts_with("access_") {
        if rt.mode == Mode::PrincipalChild {
            return Err("served_by_broker: access commands are served by the T1 broker".into());
        }
        if rt.store.is_none() && cmd != "access_status" {
            return Err(
                "store_unavailable: this daemon has no access store (no --data-dir)".into(),
            );
        }
    }
    match cmd {
        "access_status" => status(rt, ctx).await,
        "access_devices_list" => devices_list(rt, ctx).await,
        "access_device_set_tier" => {
            let store = rt.store.as_ref().expect("checked above");
            let device_id = arg_str(args, "deviceId")?;
            let tier = Tier::parse(arg_str(args, "tier")?)
                .ok_or("invalid_request: `tier` must be view|dispatch|approve|full")?;
            let row = devices::set_tier(store, ctx, device_id, tier).await?;
            rt.sockets
                .device_caps_changed(&row.device_id, row.grant_epoch);
            Ok(json!(row.view(
                rt.sockets.live_for_device(&row.device_id),
                is_this(ctx, &row.device_id)
            )))
        }
        "access_device_revoke" => {
            let store = rt.store.as_ref().expect("checked above");
            let device_id = arg_str(args, "deviceId")?;
            let row = devices::revoke(store, ctx, device_id).await?;
            // §3.10: every open socket of that device closes at once (4401).
            rt.sockets.device_revoked(&row.device_id);
            Ok(json!({}))
        }
        // WP-74b: pairing (§3.1–§3.8).
        "access_pair_begin"
        | "access_pair_cancel"
        | "access_pair_pending"
        | "access_pair_decide" => Err(super::not_implemented("WP-74b")),
        // WP-75.
        "access_routing_get" => routing::get(rt, ctx, args).await,
        "access_routing_set" => routing::set(rt, ctx, args).await,
        "permission_decide" => {
            ask_routing::decide(
                state.and_then(|s| s.pa_db.as_deref()),
                &ask_routing::NoResolvers,
                Some(ctx),
                args,
            )
            .await
        }
        "permission_relay_put" => ask_routing::relay_put(args).await,
        "permission_relay_take" => ask_routing::relay_take(args).await,
        "permission_relay_resolve" => ask_routing::relay_resolve(args).await,
        // WP-76.
        "access_members_list" => members::list(rt, ctx, args).await,
        "access_member_set_role" => members::set_role(rt, ctx, args).await,
        "access_member_remove" => members::remove(rt, ctx, args).await,
        "access_member_restore" => members::restore(rt, ctx, args).await,
        "access_shares_list" => members::shares_list(rt, ctx, args).await,
        "access_policy_get" => policy::get(rt, ctx, args).await,
        "access_policy_set_cell" => policy::set_cell(rt, ctx, args).await,
        "access_policy_set_owner_approval" => policy::set_owner_approval(rt, ctx, args).await,
        "access_invite_issue" => invites::issue(rt, ctx, args).await,
        "access_invite_revoke" => invites::revoke(rt, ctx, args).await,
        "notifications_record_access" | "share_project_info" => match state {
            None => Err("not_found: no such command here".into()),
            Some(s) if cmd == "share_project_info" => share::share_project_info(s, ctx, args).await,
            Some(s) => share::notifications_record_access(s, ctx, args).await,
        },
        // WP-77.
        "access_audit_list" => audit::list::list(rt, ctx, args).await,
        "access_audit_verify" => audit::list::verify(rt, ctx, args).await,
        "access_audit_export" => audit::export::export(rt, ctx, args).await,
        "access_audit_record_local" => audit::list::record_local(rt, ctx, args).await,
        "access_audit_reseal" => audit::reseal::reseal(rt, ctx, args).await,
        other => Err(format!("not_found: no access command `{other}`")),
    }
}

fn is_this(ctx: &AccessCtx, device_id: &str) -> bool {
    ctx.device_id.as_deref() == Some(device_id)
}

/// `access_status` (§9.1): any authenticated caller.
async fn status(rt: &Runtime, ctx: &AccessCtx) -> RpcResult {
    let (store_state, broken) = match &rt.store {
        None => ("none", None),
        Some(s) => match s.degraded() {
            Some(b) => ("degraded", Some(b.at_seq)),
            None => ("ok", None),
        },
    };
    let tier = if rt.mode == Mode::Broker { "t1" } else { "t0" };
    let principal_id = ctx.principal_id.map(|p| p.to_string()).unwrap_or_default();
    let (username, is_admin) = match (&rt.directory, rt.mode) {
        (Some(dir), Mode::Broker) => dir.lookup(&principal_id).await.unwrap_or_default(),
        _ => (rt.host.username.clone(), false),
    };
    let share = ctx.share.as_ref().map(|s| {
        let scope = if s.artifact_path.is_some() {
            "artifact"
        } else {
            "project"
        };
        json!({
            "projectKey": s.project_key,
            "projectName": s.project_id,
            "ownerUsername": "",
            "role": s.role.as_str(),
            "scope": scope,
            "artifactPath": s.artifact_path,
        })
    });
    let mut out = json!({
        "tier": tier,
        "store": store_state,
        "principal": { "principalId": principal_id, "username": username, "isAdmin": is_admin },
        "credential": {
            "via": ctx.via.via_str(),
            "deviceId": ctx.device_id,
            "tier": ctx.tier.as_str(),
        },
        "caps": ctx.caps.names(),
        "adminStrength": ctx.admin_strength,
        "publicUrl": rt.options.public_url,
        "sharingEnabled": tier == "t1",
        "share": share,
    });
    if let Some(seq) = broken {
        out["brokenAtSeq"] = json!(seq);
    }
    Ok(out)
}

/// `access_devices_list` (§9.1): the caller's own principal's devices;
/// `admin_strength` sees them all, anything else only its own row.
async fn devices_list(rt: &Runtime, ctx: &AccessCtx) -> RpcResult {
    let store = rt.store.as_ref().expect("checked by dispatch");
    let Some(principal) = ctx.principal_id else {
        return Ok(json!([]));
    };
    let mut conn = store
        .pool
        .acquire()
        .await
        .map_err(|e| format!("internal: {e}"))?;
    let rows = devices::list_for(&mut conn, &principal.to_string())
        .await
        .map_err(|e| format!("internal: {e}"))?;
    let views: Vec<_> = rows
        .iter()
        .filter(|r| ctx.admin_strength || is_this(ctx, &r.device_id))
        .map(|r| {
            r.view(
                rt.sockets.live_for_device(&r.device_id),
                is_this(ctx, &r.device_id),
            )
        })
        .collect();
    Ok(json!(views))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::store::test_support;
    use crate::access::{AccessOptions, Runtime};
    use std::sync::Arc;

    fn rt_with(store: Arc<crate::access::store::AccessStore>) -> Runtime {
        let mut rt = Runtime::none();
        rt.store = Some(store);
        rt
    }

    #[test]
    fn every_command_is_mapped_as_its_section_says() {
        use crate::access::caps::ArmClass;
        use crate::access::rpc_requirements::requirement;
        for cmd in COMMANDS {
            let class = requirement(cmd).class;
            let expected = if cmd.starts_with("access_") {
                ArmClass::Access
            } else if cmd.starts_with("permission_relay_") {
                ArmClass::Operator
            } else if *cmd == "permission_decide" {
                ArmClass::Shared
            } else {
                ArmClass::Internal
            };
            assert_eq!(class, expected, "{cmd}");
        }
    }

    #[tokio::test]
    async fn a_child_answers_served_by_broker_and_no_store_answers_unavailable() {
        let child = Runtime::for_daemon(None, true, AccessOptions::default()).await;
        let ctx = AccessCtx::child(None, None);
        for cmd in COMMANDS.iter().filter(|c| c.starts_with("access_")) {
            let e = dispatch(&child, None, &ctx, cmd, &json!({}))
                .await
                .unwrap_err();
            assert!(e.starts_with("served_by_broker"), "{cmd}: {e}");
        }
        let none = Runtime::none();
        let op = none.operator_ctx();
        for cmd in COMMANDS
            .iter()
            .filter(|c| c.starts_with("access_") && **c != "access_status")
        {
            let e = dispatch(&none, None, &op, cmd, &json!({}))
                .await
                .unwrap_err();
            assert!(e.starts_with("store_unavailable"), "{cmd}: {e}");
        }
        let st = dispatch(&none, None, &op, "access_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(st["store"], "none");
        assert_eq!(st["tier"], "t0");
    }

    #[tokio::test]
    async fn stubs_name_their_wp() {
        let (_d, store) = test_support::t0().await;
        let rt = rt_with(store);
        let op = rt.operator_ctx();
        for (cmd, wp) in [
            ("access_pair_begin", "WP-74b"),
            ("access_routing_get", "WP-75"),
            ("access_members_list", "WP-76"),
            ("access_invite_issue", "WP-76"),
            ("access_policy_get", "WP-76"),
            ("access_audit_list", "WP-77"),
            ("access_audit_reseal", "WP-77"),
            ("permission_decide", "WP-75"),
        ] {
            let e = dispatch(&rt, None, &op, cmd, &json!({})).await.unwrap_err();
            assert_eq!(e, format!("internal: not implemented ({wp})"), "{cmd}");
        }
        let e = dispatch(&rt, None, &op, "permission_relay_put", &json!({}))
            .await
            .unwrap_err();
        assert!(e.starts_with("invalid_request"), "{e}");
    }

    #[tokio::test]
    async fn status_and_devices_for_the_operator_and_a_phone() {
        let (_d, store) = test_support::t0().await;
        let owner = store.owner.unwrap();
        let mut tx = store.begin().await.unwrap();
        let (phone, _) = devices::issue_paired_in(
            &mut tx,
            &owner.to_string(),
            "Pixel 9 · Chrome",
            None,
            Tier::Dispatch,
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rt = rt_with(store.clone());
        let op = rt.operator_ctx();

        let st = dispatch(&rt, None, &op, "access_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(st["store"], "ok");
        assert_eq!(st["principal"]["principalId"], owner.to_string());
        assert_eq!(st["credential"]["via"], "operator");
        assert_eq!(st["credential"]["tier"], "full");
        assert_eq!(st["adminStrength"], true);
        assert_eq!(st["sharingEnabled"], false);
        assert_eq!(st["caps"].as_array().unwrap().len(), 7);

        let list = dispatch(&rt, None, &op, "access_devices_list", &json!({}))
            .await
            .unwrap();
        let list = list.as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["kind"], "host");
        assert_eq!(list[0]["thisDevice"], true);
        assert_eq!(list[1]["deviceId"], phone.device_id);
        assert_eq!(list[1]["tier"], "dispatch");

        // The phone itself (below full) sees only its own row.
        let me = AccessCtx::device(owner, phone.device_id.clone(), Tier::Dispatch, 0);
        let list = dispatch(&rt, None, &me, "access_devices_list", &json!({}))
            .await
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["thisDevice"], true);
        let st = dispatch(&rt, None, &me, "access_status", &json!({}))
            .await
            .unwrap();
        assert_eq!(st["credential"]["via"], "device");
        assert_eq!(st["caps"], json!(["files", "sessions", "dispatch"]));
    }

    /// A-6 / A-7 at the arm level: revoke closes the device's open sockets
    /// with 4401 in-process; a tier change closes them with 4403.
    #[tokio::test]
    async fn revoke_and_tier_arms_close_the_devices_sockets() {
        use crate::access::sockets::{Close, SocketControl, SocketKey};
        let (_d, store) = test_support::t0().await;
        let owner = store.owner.unwrap();
        let mut tx = store.begin().await.unwrap();
        let (phone, _) = devices::issue_paired_in(
            &mut tx,
            &owner.to_string(),
            "p",
            None,
            Tier::Approve,
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let rt = rt_with(store);
        let op = rt.operator_ctx();
        let key = SocketKey {
            principal_id: Some(owner.to_string()),
            device_id: Some(phone.device_id.clone()),
            grant_epoch: Some(phone.grant_epoch),
        };
        let mut sock = rt.registry.register(key.clone());
        let out = dispatch(
            &rt,
            None,
            &op,
            "access_device_set_tier",
            &json!({"deviceId": phone.device_id, "tier": "view"}),
        )
        .await
        .unwrap();
        assert_eq!(out["tier"], "view");
        assert_eq!(sock.revoked(), Some(Close::CAPS_CHANGED));

        let mut sock = rt.registry.register(SocketKey {
            grant_epoch: Some(phone.grant_epoch + 1),
            ..key
        });
        assert_eq!(rt.registry.live_for_device(&phone.device_id), 1);
        dispatch(
            &rt,
            None,
            &op,
            "access_device_revoke",
            &json!({"deviceId": phone.device_id}),
        )
        .await
        .unwrap();
        assert_eq!(sock.revoked(), Some(Close::DEVICE_REVOKED));
        assert_eq!(rt.registry.live_for_device(&phone.device_id), 0);
    }
}
