//! Members, roles and "Shared with you" (G-ACCESS §4, §4.5.2) — **stub
//! module, WP-76 fills it** (§9.2). The tables (`shared_projects`,
//! `project_members`, `project_role_caps`) exist from WP-74a's `0001_core`.

use serde_json::Value;

use super::ctx::AccessCtx;
use super::rpc::Env;
use super::AccessError;

/// `access_members_list`, `access_member_set_role`, `access_member_remove`,
/// `access_member_restore`, `access_shares_list` (WP-76).
pub async fn dispatch(
    _env: &Env<'_>,
    _ctx: &AccessCtx,
    _cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-76"))
}
