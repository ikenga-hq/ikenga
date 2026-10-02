//! `access_audit_list` and `access_audit_verify` (§6.7, §9.1) — WP-77.

use serde_json::Value;

use crate::access::ctx::AccessCtx;
use crate::access::{not_implemented, RpcResult, Runtime};

pub async fn list(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-77"))
}

pub async fn verify(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-77"))
}

pub async fn record_local(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-77"))
}
