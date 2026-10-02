//! Project policies: the role × cap matrix and "Require Owner approval"
//! (G-ACCESS §4.1, §5.2) — **stub module, WP-76 fills it** (§9.2).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::rpc::Env;
use super::AccessError;

/// `access_policy_get`, `access_policy_set_cell`,
/// `access_policy_set_owner_approval` (WP-76).
pub async fn dispatch(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}
