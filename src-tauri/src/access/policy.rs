//! The Policies matrix (G-ACCESS §4.1, §5.2): `access_policy_get`,
//! `access_policy_set_cell` (`cap ≠ secrets`, `role ≠ owner`, A-4),
//! `access_policy_set_owner_approval`. **WP-76 fills this** (W4). On T0
//! `access_policy_get` returns the defaults (§4.5.5).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::{not_implemented, RpcResult, Runtime};

pub async fn get(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn set_cell(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn set_owner_approval(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}
