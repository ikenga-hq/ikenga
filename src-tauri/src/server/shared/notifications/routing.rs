//! Remote permission routing core (G-ACCESS §5.3–§5.5, review M-1), shared
//! by both binaries. **WP-75 fills this** (W4); WP-74a creates it so the
//! rpc post-hook can call the one [`annotate`]:
//!
//! * [`classify`] — the §5.3 sensitivity classifier (pure, table-tested by
//!   WP-75, A-22);
//! * [`decide`] — the §5.4 decide core behind `permission_decide`, which
//!   dispatches on the row's `dedupe_key` prefix through an
//!   [`AskResolvers`] table;
//! * [`annotate`] — the read-model post-hook: `can_decide` / `waiting_on`
//!   on each `permission` row of `notifications_list` (§5.7);
//! * the T0 desktop → daemon ask relay arms (`permission_relay_*`, §5.5 (a),
//!   N-8 decided (a)+(c)).
//!
//! Until WP-75 lands: `classify` fails closed (everything is secret
//! material), `decide` and the relay arms refuse, `annotate` leaves rows as
//! they are.

use std::path::Path;

use serde_json::Value;

use crate::access::ctx::AccessCtx;

/// §5.3: `sensitive` (rules 1–3) and `secret_material` (rule 3 alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sensitivity {
    pub sensitive: bool,
    pub secret_material: bool,
}

impl Sensitivity {
    /// `shell_notifications.sensitive` (0 = no, 1 = sensitive, 2 = secret
    /// material; migration 0070).
    pub fn column(self) -> i64 {
        if self.secret_material {
            2
        } else if self.sensitive {
            1
        } else {
            0
        }
    }
}

/// §5.3 classifier. Stub: fails closed (the strictest answer) until WP-75.
pub fn classify(
    _tool_name: &str,
    _tool_input: &Value,
    _project_root: Option<&Path>,
) -> Sensitivity {
    Sensitivity {
        sensitive: true,
        secret_material: true,
    }
}

/// Who resolves an ask of a given `dedupe_key` prefix (§5.5). The desktop
/// registers the hook and ACP resolvers; the daemon only the relay one.
pub trait AskResolvers: Send + Sync {}

/// The daemon's resolver table until WP-75: none.
pub struct NoResolvers;

impl AskResolvers for NoResolvers {}

/// `permission_decide { notificationId, decision }` (§5.4, §5.5). Stub.
pub async fn decide(
    _db: Option<&crate::db::PaDb>,
    _resolvers: &dyn AskResolvers,
    _ctx: Option<&AccessCtx>,
    _args: &Value,
) -> Result<Value, String> {
    Err("internal: not implemented (WP-75)".to_string())
}

/// §5.7 read model, called by the rpc post-hook for every successful arm.
/// Stub: leaves the result untouched.
pub fn annotate(_ctx: &AccessCtx, _cmd: &str, _data: &mut Value) {}

/// `permission_relay_put` (operator bearer only). Refused until WP-75.
pub async fn relay_put(_args: &Value) -> Result<Value, String> {
    Err("invalid_request: the permission relay is not built yet (WP-75)".to_string())
}

/// `permission_relay_take` (operator bearer only). Refused until WP-75.
pub async fn relay_take(_args: &Value) -> Result<Value, String> {
    Err("invalid_request: the permission relay is not built yet (WP-75)".to_string())
}

/// `permission_relay_resolve` (operator bearer only). Refused until WP-75.
pub async fn relay_resolve(_args: &Value) -> Result<Value, String> {
    Err("invalid_request: the permission relay is not built yet (WP-75)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_classifier_fails_closed() {
        let s = classify("Read", &serde_json::json!({"file_path": "README.md"}), None);
        assert_eq!(s.column(), 2);
    }
}
