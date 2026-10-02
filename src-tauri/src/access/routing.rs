//! Per-principal permission-routing preference (G-ACCESS §5.1) — **stub
//! module, WP-75 fills it** (§9.2). WP-74a registers `access_routing_get` /
//! `access_routing_set` and creates the `routing_prefs` table (§8.2), so
//! WP-75 adds no arm and no migration.
//!
//! `routing_ok(ctx)` feeds §1.4's `approve` term. Until WP-75 lands every
//! principal is at the default `any_approve`, for which `routing_ok` is
//! true.

use serde_json::Value;

use super::ctx::AccessCtx;
use super::rpc::Env;
use super::AccessError;

/// §5.1 under the default preference (`any_approve`): always true. WP-75
/// replaces callers' use with a lookup of the principal's `routing_prefs`.
pub const fn routing_ok_default() -> bool {
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
