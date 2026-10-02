//! The audit log (G-ACCESS §6, DEC-80): append-only, hash-chained from day
//! one, in the access store, every row carrying `principal_id` and
//! `device_id`.
//!
//! WP-74a owns the chain core ([`chain`]: the §6.2 hash, the §6.3 append
//! protocol and the §6.4 fail-closed verify) so W3's device events are
//! chained from the first row. WP-77 (W5) fills the stub modules — list,
//! export, the `auth_events` absorption and reseal — and the dispatch-frame
//! hook ([`on_client_frame`]).

pub mod absorb;
pub mod chain;
pub mod export;
pub mod list;
pub mod reseal;

use serde_json::Value;

/// The §6.1 `category` column (D-05 `AUDIT_KINDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Permission,
    Dispatch,
    Access,
    Pairing,
    People,
}

impl Category {
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Permission => "permission",
            Category::Dispatch => "dispatch",
            Category::Access => "access",
            Category::Pairing => "pairing",
            Category::People => "people",
        }
    }
}

/// The §6.1 `via` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditVia {
    Session,
    Device,
    Operator,
    Cli,
    System,
}

impl AuditVia {
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditVia::Session => "session",
            AuditVia::Device => "device",
            AuditVia::Operator => "operator",
            AuditVia::Cli => "cli",
            AuditVia::System => "system",
        }
    }

    pub fn of(via: &super::ctx::Via) -> AuditVia {
        use super::ctx::Via;
        match via {
            Via::Session { .. } => AuditVia::Session,
            Via::Device { .. } => AuditVia::Device,
            Via::Operator => AuditVia::Operator,
            Via::ChildToken | Via::Relayed => AuditVia::System,
        }
    }
}

/// The closed `kind` list (§6.5) → its category, and whether the kind is an
/// **access change** that a degraded chain refuses (§6.4, P-35).
const KINDS: &[(&str, Category, bool)] = &[
    // pairing
    ("pair.started", Category::Pairing, true),
    ("pair.failed", Category::Pairing, false),
    ("pair.denied", Category::Pairing, true),
    ("pair.allowed", Category::Pairing, true),
    ("pair.cancelled", Category::Pairing, true),
    ("device.tier_changed", Category::Pairing, true),
    ("device.revoked", Category::Pairing, true),
    ("device.expired", Category::Pairing, false),
    ("routing.changed", Category::Pairing, true),
    // people
    ("member.added", Category::People, true),
    ("member.role_changed", Category::People, true),
    ("member.removed", Category::People, true),
    ("member.restored", Category::People, true),
    ("member.expired", Category::People, false),
    ("invite.issued", Category::People, true),
    ("invite.revoked", Category::People, true),
    ("invite.accepted", Category::People, true),
    ("policy.changed", Category::People, true),
    ("policy.owner_approval_changed", Category::People, true),
    ("ownership.offered", Category::People, true),
    ("ownership.accepted", Category::People, true),
    // permission
    ("permission.decided", Category::Permission, false),
    ("permission.refused", Category::Permission, false),
    // dispatch
    ("dispatch.sent", Category::Dispatch, false),
    // access
    ("auth.login_ok", Category::Access, false),
    ("auth.login_fail", Category::Access, false),
    ("auth.login_throttled", Category::Access, false),
    ("auth.logout", Category::Access, false),
    ("auth.password_changed", Category::Access, false),
    ("auth.account_created", Category::Access, false),
    ("auth.account_disabled", Category::Access, false),
    ("auth.account_enabled", Category::Access, false),
    ("auth.sessions_revoked", Category::Access, false),
    ("auth.provision_failed", Category::Access, false),
    ("auth.probe_failed", Category::Access, false),
    ("share.artifact_viewed", Category::Access, false),
    ("app.locked", Category::Access, false),
    ("app.unlocked", Category::Access, false),
    ("vault.locked", Category::Access, false),
    ("vault.unlocked", Category::Access, false),
    ("store.created", Category::Access, false),
    ("audit.verified", Category::Access, false),
    ("audit.exported", Category::Access, false),
    ("audit.chain_broken", Category::Access, false),
    ("audit.resealed", Category::Access, false),
];

/// `kind`'s category, or `None` for a kind outside the closed list.
pub fn category_of(kind: &str) -> Option<Category> {
    KINDS.iter().find(|(k, ..)| *k == kind).map(|(_, c, _)| *c)
}

/// Whether a degraded chain refuses `kind` (§6.4).
pub fn is_access_change(kind: &str) -> bool {
    KINDS.iter().any(|(k, _, gated)| *k == kind && *gated)
}

/// One row to append (§6.1 minus `seq`, `at_ms`, `prev_hash`, `hash`, which
/// the chain assigns).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: &'static str,
    pub principal_id: Option<String>,
    pub device_id: Option<String>,
    pub via: AuditVia,
    pub subject_principal_id: Option<String>,
    pub subject_device_id: Option<String>,
    pub project_key: Option<String>,
    pub target: Option<String>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
    /// Never secrets, tokens, codes, passwords or full tool inputs (§6.2).
    pub detail: Value,
    /// Append even when the chain is degraded. Set for revocations the
    /// system makes on the authentication path (a forced logout's device
    /// grants, §3.10 / R-11): P-35 keeps authentication — and killing
    /// credentials — running while access *grants* are paused.
    pub continue_when_degraded: bool,
}

impl Event {
    pub fn new(kind: &'static str, via: AuditVia) -> Self {
        debug_assert!(
            category_of(kind).is_some(),
            "audit kind {kind} is not in §6.5"
        );
        Self {
            kind,
            principal_id: None,
            device_id: None,
            via,
            subject_principal_id: None,
            subject_device_id: None,
            project_key: None,
            target: None,
            remote_addr: None,
            user_agent: None,
            detail: Value::Object(Default::default()),
            continue_when_degraded: false,
        }
    }

    /// Actor columns from a request's [`super::ctx::AccessCtx`].
    pub fn by(kind: &'static str, ctx: &super::ctx::AccessCtx) -> Self {
        let mut e = Self::new(kind, AuditVia::of(&ctx.via));
        e.principal_id = Some(ctx.principal_id.to_string());
        e.device_id = ctx.device_id.clone();
        e.remote_addr = ctx.meta.remote_addr.clone();
        e.user_agent = ctx.meta.user_agent.clone();
        if let super::ctx::Via::Session { session_id } = &ctx.via {
            e.detail = serde_json::json!({ "session_ref": session_ref(session_id) });
        }
        e
    }

    pub fn subject_device(mut self, id: impl Into<String>) -> Self {
        self.subject_device_id = Some(id.into());
        self
    }

    pub fn subject_principal(mut self, id: impl Into<String>) -> Self {
        self.subject_principal_id = Some(id.into());
        self
    }

    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = Some(t.into());
        self
    }

    /// Merge `fields` into `detail` (an object).
    pub fn detail(mut self, fields: Value) -> Self {
        if let (Value::Object(base), Value::Object(add)) = (&mut self.detail, fields) {
            base.extend(add);
        }
        self
    }

    pub fn continue_when_degraded(mut self) -> Self {
        self.continue_when_degraded = true;
        self
    }

    pub fn category(&self) -> Category {
        category_of(self.kind).unwrap_or(Category::Access)
    }

    /// Whether a degraded chain refuses this append.
    pub fn refused_when_degraded(&self) -> bool {
        is_access_change(self.kind) && !self.continue_when_degraded
    }
}

/// P-34: the first 8 hex chars of SHA-256(session_id) — never the raw id.
pub fn session_ref(session_id: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(session_id.as_bytes()))[..8].to_string()
}

/// WP-77 stub: the dispatch-audit hook on a client WS frame (`dispatch.sent`,
/// P-22). WP-74a wires the call sites in `pty_ws` / `chat_ws`; WP-77 fills
/// the body. A no-op until then.
pub fn on_client_frame(_ctx: &super::ctx::AccessCtx, _route: &str, _is_dispatch: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_unique_and_categorised() {
        let mut seen = std::collections::BTreeSet::new();
        for (k, ..) in KINDS {
            assert!(seen.insert(*k), "duplicate kind {k}");
        }
        assert_eq!(category_of("device.revoked"), Some(Category::Pairing));
        assert_eq!(category_of("auth.login_ok"), Some(Category::Access));
        assert_eq!(category_of("nope"), None);
        assert!(is_access_change("device.tier_changed"));
        assert!(!is_access_change("permission.decided"));
        assert!(!is_access_change("auth.sessions_revoked"));
    }

    #[test]
    fn session_ref_is_a_hash_prefix_not_the_id() {
        let r = session_ref("secret-session-id");
        assert_eq!(r.len(), 8);
        assert!(!"secret-session-id".contains(&r));
    }

    #[test]
    fn system_revocations_continue_when_degraded() {
        let e = Event::new("device.revoked", AuditVia::System);
        assert!(e.refused_when_degraded());
        assert!(!e.continue_when_degraded().refused_when_degraded());
    }
}
