//! `access_audit_export` (G-ACCESS §6.8) — **stub module, WP-77 fills it**.
//! `destPath` is honoured only from the T0 operator bearer (P-38).

use serde_json::Value;

use crate::access::ctx::AccessCtx;
use crate::access::rpc::Env;
use crate::access::AccessError;

pub async fn dispatch(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-77"))
}
