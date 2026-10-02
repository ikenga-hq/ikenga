//! `access_audit_export` (§6.8; `destPath` only from the T0 operator bearer,
//! P-38) — WP-77.

use serde_json::Value;

use crate::access::ctx::AccessCtx;
use crate::access::{not_implemented, RpcResult, Runtime};

pub async fn export(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-77"))
}
