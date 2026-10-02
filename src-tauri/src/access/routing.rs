//! Per-principal permission-routing preference (G-ACCESS §5.1) — **stub
//! module, WP-75 fills it** (§9.2). WP-74a registers `access_routing_get` /
//! `access_routing_set`, creates the `routing_prefs` table (§8.2) and calls
//! [`routing_ok`] everywhere effective caps are computed, so WP-75 adds no
//! arm, no migration and no call site: it fills the bodies in this file.
//!
//! `routing_ok(ctx)` feeds §1.4's `approve` term. Until WP-75 lands every
//! principal is at the default `any_approve`, for which `routing_ok` is
//! true.

use serde_json::Value;

use super::ctx::AccessCtx;
use super::rpc::Env;
use super::store::AccessStore;
use super::AccessError;
use crate::executor::PrincipalId;

/// §5.1 `routing_ok(ctx)`: may this credential answer the principal's
/// permission asks? `false` removes `approve` from the effective caps
/// (§1.4), which gates `permission_decide`, the `pa_actions_*` mutations and
/// `can_decide` on rows.
///
/// Called **once per request / WS handshake**, wherever effective caps are
/// computed: T0 `DaemonAccess::{operator_ctx, device_ctx}` and T1
/// `T1Access::access_ctx` (own workspace and shares alike). `device_id` is
/// the credential's device: the host device for the T0 operator bearer, the
/// paired device for a grant, `None` for a T1 password session (which never
/// satisfies `this_device`). `store` is `None` when the access store is
/// unavailable.
///
/// WP-75 fills it with the `routing_prefs` lookup. It must **fail closed**
/// (`false`) on a store error. Until then: the default `any_approve`, true.
pub async fn routing_ok(
    store: Option<&AccessStore>,
    principal_id: &PrincipalId,
    device_id: Option<&str>,
) -> bool {
    let _ = (store, principal_id, device_id);
    true
}

/// `access_routing_get` / `access_routing_set` (WP-75).
pub async fn dispatch(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-75"))
}
