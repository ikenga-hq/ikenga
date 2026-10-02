//! Per-principal routing preference (G-ACCESS §5.1): `access_routing_get` /
//! `access_routing_set` over `routing_prefs`. **WP-75 fills this** (W4).
//!
//! Until then routing is the default, `any_approve`, so `routing_ok` is
//! true for every context (`AccessCtx::device`).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::{not_implemented, RpcResult, Runtime};

pub async fn get(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-75"))
}

pub async fn set(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-75"))
}
