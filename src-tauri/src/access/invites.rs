//! Invites (G-ACCESS §7; T1 only): `access_invite_issue`,
//! `access_invite_revoke`, and the public `/access/invite/{inspect,accept}`
//! handlers (`access::http`). **WP-76 fills this** (W4).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::{not_implemented, RpcResult, Runtime};

pub async fn issue(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}

pub async fn revoke(_rt: &Runtime, _ctx: &AccessCtx, _args: &Value) -> RpcResult {
    Err(not_implemented("WP-76"))
}
