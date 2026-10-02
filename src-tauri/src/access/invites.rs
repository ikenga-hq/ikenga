//! Invites (G-ACCESS §7, T1 only) — **stub module, WP-76 fills it** (§9.2):
//! the issue / revoke arms here, the public `/access/invite/{inspect,accept}`
//! handlers in `access::http`. The `invites` table exists from `0001_core`;
//! `--invite-ttl`, `--max-accounts` and `--member-invites-create-accounts`
//! are already parsed (`AccessOptions`).

use serde_json::Value;

use super::ctx::AccessCtx;
use super::rpc::Env;
use super::AccessError;

/// `access_invite_issue`, `access_invite_revoke` (WP-76).
pub async fn dispatch(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}
