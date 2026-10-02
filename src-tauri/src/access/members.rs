//! Members and roles (G-ACCESS §4, §9.1): `access_members_list`,
//! `access_member_set_role`, `access_member_remove`,
//! `access_member_restore`, `access_shares_list`. **WP-76 fills this** (W4).
//! On T0 these answer `requires_t1` (§4.5.5).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::{not_implemented, RpcResult, Runtime};

pub async fn list(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn set_role(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn remove(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn restore(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn shares_list(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}
