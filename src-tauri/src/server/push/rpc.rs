//! The `access_push_*` arms (plans/pwa S2 §7). Named `access_*` so they
//! ride the existing routing: the T0 daemon serves them in
//! `access::rpc::dispatch`, the T1 broker intercepts and serves them as
//! root, a T1 child answers `served_by_broker`.
//!
//! A subscription binds to the caller's own principal and credential; a
//! broker → child token (`ChildToken` / `Relayed`) can't subscribe. Nothing
//! here ever returns `p256dh`, `auth` or a full endpoint.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde_json::{json, Value};

use super::hub::PushHub;
use super::store::{self, NewSub, SubVia};
use super::PushKind;
use crate::access::ctx::{AccessCtx, Via};
use crate::access::rpc::Env;
use crate::access::store::StoreTier;
use crate::access::{AccessError, Code};

pub const ARMS: [&str; 6] = [
    "access_push_config",
    "access_push_subscribe",
    "access_push_update",
    "access_push_unsubscribe",
    "access_push_list",
    "access_push_test",
];

const LABEL_MAX: usize = 64;
const USER_AGENT_MAX: usize = 256;

/// The served entry (`access::rpc::dispatch`).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    let hub = super::hub();
    dispatch_with(hub.as_deref(), env, ctx, cmd, args).await
}

fn internal(e: impl std::fmt::Display) -> AccessError {
    AccessError::internal(e)
}

fn invalid(msg: &str) -> AccessError {
    AccessError::new(Code::InvalidRequest, msg)
}

/// Who is subscribing, as the store records it.
struct Binding {
    via: SubVia,
    device_id: Option<String>,
    session_id: Option<String>,
}

fn binding(ctx: &AccessCtx) -> Result<Binding, AccessError> {
    match &ctx.via {
        Via::Device { device_id } => Ok(Binding {
            via: SubVia::Device,
            device_id: Some(device_id.clone()),
            session_id: None,
        }),
        Via::Session { session_id } => Ok(Binding {
            via: SubVia::Session,
            device_id: None,
            session_id: Some(session_id.clone()),
        }),
        Via::Operator => Ok(Binding {
            via: SubVia::Operator,
            // The T0 host device (the operator bearer's device).
            device_id: ctx.device_id.clone(),
            session_id: None,
        }),
        Via::ChildToken | Via::Relayed => Err(AccessError::new(
            Code::Forbidden,
            "push subscriptions belong to a principal's own credential",
        )),
    }
}

/// The kinds this credential may receive.
fn entitled_kinds(env: &Env<'_>, ctx: &AccessCtx) -> Vec<PushKind> {
    let admin_ok = env.tier == StoreTier::T0 || env.principal.is_admin;
    store::kinds_for(ctx.tier, ctx.admin_strength, admin_ok)
}

fn parse_kinds(args: &Value, allowed: &[PushKind]) -> Result<Option<Vec<PushKind>>, AccessError> {
    let Some(raw) = args.get("kinds") else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let arr = raw
        .as_array()
        .ok_or_else(|| invalid("`kinds` must be an array"))?;
    let mut out = Vec::new();
    for v in arr {
        let k = v
            .as_str()
            .and_then(PushKind::parse)
            .ok_or_else(|| invalid("unknown kind in `kinds`"))?;
        // Silently narrowed to what this credential may get.
        if allowed.contains(&k) && !out.contains(&k) {
            out.push(k);
        }
    }
    Ok(Some(out))
}

fn b64(raw: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(raw.trim().trim_end_matches('='))
        .ok()
}

fn hub_on<'a>(hub: Option<&'a PushHub>) -> Result<&'a PushHub, AccessError> {
    hub.filter(|h| h.enabled()).ok_or_else(|| {
        AccessError::new(Code::Conflict, "push notifications are off on this server")
    })
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

pub async fn dispatch_with(
    hub: Option<&PushHub>,
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    let b = binding(ctx)?;
    let principal = ctx.principal_id.to_string();
    match cmd {
        "access_push_config" => Ok(match hub {
            Some(h) if h.enabled() => {
                let v = h.vapid().expect("enabled hub has a key");
                json!({
                    "enabled": true,
                    "publicKey": v.public_key_b64(),
                    "keyId": v.key_id(),
                    "kinds": entitled_kinds(env, ctx).iter().map(|k| k.as_str()).collect::<Vec<_>>(),
                })
            }
            Some(h) => json!({
                "enabled": false,
                "reason": h.disabled_reason().unwrap_or("push notifications are off on this server"),
            }),
            None => json!({
                "enabled": false,
                "reason": "push notifications are off on this server",
            }),
        }),
        "access_push_subscribe" => {
            let h = hub_on(hub)?;
            let endpoint =
                str_arg(args, "endpoint").ok_or_else(|| invalid("`endpoint` is required"))?;
            let (_, origin) = h.policy().check(endpoint).map_err(invalid)?;
            let keys = args
                .get("keys")
                .ok_or_else(|| invalid("`keys` is required"))?;
            let p256dh = keys
                .get("p256dh")
                .and_then(Value::as_str)
                .and_then(b64)
                .filter(|k| super::crypto::valid_ua_public(k))
                .ok_or_else(|| invalid("`keys.p256dh` must be a P-256 public key"))?;
            let auth = keys
                .get("auth")
                .and_then(Value::as_str)
                .and_then(b64)
                .filter(|a| a.len() == super::crypto::AUTH_LEN)
                .ok_or_else(|| invalid("`keys.auth` must be 16 bytes"))?;
            let allowed = entitled_kinds(env, ctx);
            // `pushsubscriptionchange` (the service worker): the browser
            // rotated the endpoint; carry the old row's kinds and label over.
            let replaced = match str_arg(args, "replaces") {
                Some(old) if old != endpoint => store::list(h.pool(), &principal)
                    .await
                    .map_err(internal)?
                    .into_iter()
                    .find(|s| s.endpoint == old),
                _ => None,
            };
            let kinds = match parse_kinds(args, &allowed)? {
                Some(k) => k,
                None => match &replaced {
                    Some(r) => r
                        .kinds
                        .iter()
                        .copied()
                        .filter(|k| allowed.contains(k))
                        .collect(),
                    None => allowed,
                },
            };
            let label = str_arg(args, "label")
                .map(|l| crate::access::pairing::sanitize_name(l, LABEL_MAX))
                .filter(|l| !l.is_empty())
                .or_else(|| replaced.as_ref().and_then(|r| r.label.clone()));
            let session_epoch = match (&b.via, env.tier) {
                (SubVia::Session, StoreTier::T1) => {
                    store::account_state(h.pool(), StoreTier::T1, &principal)
                        .await
                        .map_err(internal)?
                        .map(|a| a.session_epoch)
                }
                _ => None,
            };
            let new = NewSub {
                principal_id: principal,
                device_id: b.device_id,
                via: b.via,
                session_epoch,
                session_ref: b.session_id.as_deref().map(store::session_ref),
                endpoint: endpoint.to_string(),
                endpoint_origin: origin,
                p256dh,
                auth,
                vapid_key_id: h.vapid().expect("enabled").key_id(),
                kinds,
                label,
                user_agent: ctx
                    .meta
                    .user_agent
                    .as_deref()
                    .map(|u| u.chars().take(USER_AGENT_MAX).collect()),
            };
            let sub_id = store::upsert(h.pool(), &new).await.map_err(internal)?;
            if let Some(old) = replaced {
                store::delete_own(h.pool(), &new.principal_id, Some(&old.sub_id), None)
                    .await
                    .map_err(internal)?;
            }
            Ok(json!({ "subId": sub_id }))
        }
        "access_push_update" => {
            let h = hub_on(hub)?;
            let sub_id = str_arg(args, "subId").ok_or_else(|| invalid("`subId` is required"))?;
            let kinds = parse_kinds(args, &entitled_kinds(env, ctx))?
                .ok_or_else(|| invalid("`kinds` is required"))?;
            if !store::set_kinds(h.pool(), &principal, sub_id, &kinds)
                .await
                .map_err(internal)?
            {
                return Err(AccessError::new(Code::NotFound, "no such subscription"));
            }
            Ok(json!({ "kinds": kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>() }))
        }
        "access_push_unsubscribe" => {
            // Works with push off too: a client can always clean up.
            let Some(h) = hub else {
                return Ok(json!({ "removed": 0 }));
            };
            let sub_id = str_arg(args, "subId");
            let endpoint = str_arg(args, "endpoint");
            if sub_id.is_none() && endpoint.is_none() {
                return Err(invalid("`subId` or `endpoint` is required"));
            }
            let n = store::delete_own(h.pool(), &principal, sub_id, endpoint)
                .await
                .map_err(internal)?;
            Ok(json!({ "removed": n }))
        }
        "access_push_list" => {
            let Some(h) = hub else {
                return Ok(json!([]));
            };
            let my_ref = b.session_id.as_deref().map(store::session_ref);
            let rows = store::list(h.pool(), &principal).await.map_err(internal)?;
            Ok(Value::Array(
                rows.iter()
                    .map(|s| {
                        let this = match s.via {
                            SubVia::Device => b.via == SubVia::Device && s.device_id == b.device_id,
                            SubVia::Session => my_ref.is_some() && s.session_ref == my_ref,
                            SubVia::Operator => false,
                        };
                        s.view(this)
                    })
                    .collect(),
            ))
        }
        "access_push_test" => {
            let h = hub_on(hub)?;
            let sub_id = str_arg(args, "subId").ok_or_else(|| invalid("`subId` is required"))?;
            let owned = store::get(h.pool(), sub_id)
                .await
                .map_err(internal)?
                .is_some_and(|s| s.principal_id == principal);
            if !owned {
                return Err(AccessError::new(Code::NotFound, "no such subscription"));
            }
            if !h.send_test(ctx.principal_id, sub_id) {
                return Err(AccessError::new(
                    Code::Throttled,
                    "a test was just sent; try again in a few seconds",
                ));
            }
            Ok(json!({ "queued": true }))
        }
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}
