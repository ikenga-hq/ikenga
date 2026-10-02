//! Audit read surface (G-ACCESS §6.7) — **stub module, WP-77 fills it**:
//! `access_audit_list`, `access_audit_verify` and the desktop-local
//! `access_audit_record_local` (operator bearer only, §6.5).

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

/// `access_audit_verify` (WP-77; the walk itself is `chain::verify_all`).
pub async fn verify(_env: &Env<'_>, _ctx: &AccessCtx) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-77"))
}

/// `access_audit_record_local` (WP-77).
pub async fn record_local(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-77"))
}
