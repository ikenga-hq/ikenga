//! Permission routing core (G-ACCESS §5.3–§5.7) — **stub module, WP-75
//! fills it** (§9.2, review M-1). It lives here, in the notification core
//! both binaries compile, so the daemon's `permission_decide` arm, the
//! desktop's in-process `permission_decide` command and the `rpc_handler`
//! post-hook share one copy:
//!
//! * [`classify`] — the sensitive-ask classifier (P-29);
//! * [`decide`] — the decide core, dispatching on the row's `dedupe_key`
//!   prefix through registered resolvers ([`AskResolvers`]);
//! * [`annotate`] — `can_decide` / `waiting_on` on `notifications_list`
//!   rows (the one `annotate`, called by `access::postfilter`);
//! * the T0 desktop → daemon ask relay (§5.5 (a), N-8 decided (a)+(c),
//!   DEC-83): [`relay_rpc`] behind `permission_relay_{put,take,resolve}`.

use serde_json::Value;

use crate::access::{AccessCtx, AccessError, Code};

/// `classify(tool_name, tool_input, project_root)` (§5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Sensitivity {
    pub sensitive: bool,
    pub secret_material: bool,
}

/// WP-75. Until then nothing is classified sensitive — and nothing can be
/// decided remotely either (the decide core is a stub too).
pub fn classify(_tool_name: &str, _tool_input: &Value, _project_root: Option<&str>) -> Sensitivity {
    Sensitivity::default()
}

/// The resolver table `decide` dispatches through (§5.5): the desktop
/// registers the hook and ACP resolvers, the daemon only the relay one.
pub trait AskResolvers: Send + Sync {}

/// `permission_decide`'s core (WP-75).
pub fn decide(
    _ctx: &AccessCtx,
    _notification_id: i64,
    _decision: &str,
) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-75"))
}

/// The desktop's in-process `permission_decide` (§5.5, review C-05): the
/// operator deciding its own hook / ACP asks against the desktop's
/// `ikenga.db` (WP-75).
pub fn decide_local(_notification_id: i64, _decision: &str) -> Result<Value, AccessError> {
    Err(AccessError::not_implemented("WP-75"))
}

/// The `permission_decide` RPC arm (WP-75).
pub fn decide_rpc(ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    let id = args
        .get("notificationId")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let decision = args.get("decision").and_then(Value::as_str).unwrap_or("");
    decide(ctx, id, decision)
}

/// `permission_relay_put` / `_take` / `_resolve` (operator only). Until
/// WP-75 builds the relay the skeleton answers `invalid_request` (§9.1).
pub fn relay_rpc(_ctx: &AccessCtx, cmd: &str, _args: &Value) -> Result<Value, AccessError> {
    Err(AccessError::new(
        Code::InvalidRequest,
        format!("{cmd}: the ask relay is not built yet (WP-75)"),
    ))
}

/// Annotate `permission` rows with `can_decide` / `waiting_on` for this
/// request (§5.7). WP-75 fills it; a no-op until then.
pub fn annotate(_ctx: &AccessCtx, _rows: &mut Value) {}
