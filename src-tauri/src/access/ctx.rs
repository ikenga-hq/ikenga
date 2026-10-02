//! The per-request access context (G-ACCESS §2.3).
//!
//! On T1 an [`AccessCtx`] is derived right after G-PRINCIPAL's
//! `PrincipalCtx` resolves. On T0 no `Principal` exists (§2.1, review C-10):
//! the T0 resolver builds the `AccessCtx` straight from the credential, with
//! the store's synthetic owner as `principal_id`.

use crate::executor::PrincipalId;

use super::caps::{own_workspace_caps, CapSet, Role, Tier};

/// How the caller proved who they are. G-PRINCIPAL's `Credential` (§2.1)
/// lives in `server::auth`, which is Linux-only like T1; T0 runs on every
/// desktop OS, so the access side carries its own copy with the same three
/// variants plus [`Credential::ChildToken`], the broker → principal-child
/// hop (§1.7), which G-PRINCIPAL never names because it grants nothing by
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// T1 password session (`ikenga_session`).
    Session { session_id: String },
    /// A paired device (`ikenga_device` cookie or `Bearer ikd1.…`).
    DeviceGrant { device_id: String },
    /// The T0 operator bearer (header or `?token=`). Never produced on T1.
    OperatorBearer,
    /// A T1 principal child's per-child token. Grants only what the broker's
    /// `X-Ikenga-Caps` header says (§1.7: no header, no caps).
    ChildToken,
}

impl Credential {
    pub fn device_id(&self) -> Option<&str> {
        match self {
            Credential::DeviceGrant { device_id } => Some(device_id),
            _ => None,
        }
    }

    /// `AccessStatus.credential.via` / audit `via`.
    pub fn via_str(&self) -> &'static str {
        match self {
            Credential::Session { .. } => "session",
            Credential::DeviceGrant { .. } => "device",
            Credential::OperatorBearer => "operator",
            Credential::ChildToken => "operator",
        }
    }
}

#[cfg(target_os = "linux")]
impl From<&crate::server::auth::Credential> for Credential {
    fn from(c: &crate::server::auth::Credential) -> Self {
        use crate::server::auth::Credential as G;
        match c {
            G::Session { session_id } => Credential::Session {
                session_id: session_id.clone(),
            },
            G::DeviceGrant { device_id } => Credential::DeviceGrant {
                device_id: device_id.clone(),
            },
            G::OperatorBearer => Credential::OperatorBearer,
        }
    }
}

/// A share request's context (§4.5; T1 only). On the broker it comes from
/// the membership; in the child, from the narrowing-only `X-Ikenga-Share-*`
/// headers (§4.5.3). Filled in by WP-76; WP-74a only carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCtx {
    /// `<owner_principal_id>/<project_id>` (broker side; empty in the child,
    /// which only knows the project id).
    pub project_key: String,
    pub project_id: String,
    /// The member's principal (attribution and narrowing only).
    pub member_principal_id: Option<String>,
    /// The member's device, if any (attribution only).
    pub member_device_id: Option<String>,
    pub role: Role,
    /// Artifact scope: the one path, relative to the project root.
    pub artifact_path: Option<String>,
    /// `X-Ikenga-Share-Policy: owner-approval`.
    pub owner_approval: bool,
}

/// The resolved caller, as access control sees it (§2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessCtx {
    /// T1: the account's principal. T0: the store's synthetic owner. `None`
    /// only where no principal can be named without trusting a header: a T0
    /// daemon with no access store (no `--data-dir`), and a T1 child, which
    /// never authorizes on `X-Ikenga-Principal` (G-PRINCIPAL §3).
    pub principal_id: Option<PrincipalId>,
    pub via: Credential,
    /// DeviceGrant → that device; T0 OperatorBearer → the host device;
    /// Session / child → `None`.
    pub device_id: Option<String>,
    pub tier: Tier,
    pub share: Option<ShareCtx>,
    /// Effective caps (§1.4), computed once per request / WS handshake.
    pub caps: CapSet,
    /// P-26: Session, OperatorBearer, or a `full` device.
    pub admin_strength: bool,
    /// The device grant's revocation epoch at resolution (socket registry).
    pub grant_epoch: Option<i64>,
}

impl AccessCtx {
    /// The T0 operator bearer: the host itself, always `full` (§1.3).
    pub fn operator(owner: Option<PrincipalId>, host_device: Option<String>) -> Self {
        AccessCtx {
            principal_id: owner,
            via: Credential::OperatorBearer,
            device_id: host_device,
            tier: Tier::Full,
            share: None,
            caps: CapSet::ALL,
            admin_strength: true,
            grant_epoch: None,
        }
    }

    /// A device grant in its principal's own workspace (T0, or T1 without a
    /// share). Routing (§5.1) is WP-75's: until it lands, routing is
    /// `any_approve`, the default, so `routing_ok` is true.
    pub fn device(principal: PrincipalId, device_id: String, tier: Tier, grant_epoch: i64) -> Self {
        AccessCtx {
            principal_id: Some(principal),
            via: Credential::DeviceGrant {
                device_id: device_id.clone(),
            },
            device_id: Some(device_id),
            tier,
            share: None,
            caps: own_workspace_caps(tier, true),
            admin_strength: tier == Tier::Full,
            grant_epoch: Some(grant_epoch),
        }
    }

    /// A T1 password session (`full`, §1.3).
    pub fn session(principal: PrincipalId, session_id: String) -> Self {
        AccessCtx {
            principal_id: Some(principal),
            via: Credential::Session { session_id },
            device_id: None,
            tier: Tier::Full,
            share: None,
            caps: CapSet::ALL,
            admin_strength: true,
            grant_epoch: None,
        }
    }

    /// A T1 principal child's request: caps **only** from `X-Ikenga-Caps`
    /// (absent or unparseable → none), never from the token itself (§1.7).
    pub fn child(caps: Option<CapSet>, share: Option<ShareCtx>) -> Self {
        AccessCtx {
            principal_id: None,
            via: Credential::ChildToken,
            device_id: None,
            tier: Tier::Full,
            share,
            caps: caps.unwrap_or(CapSet::EMPTY),
            admin_strength: false,
            grant_epoch: None,
        }
    }

    pub fn is_operator(&self) -> bool {
        self.via == Credential::OperatorBearer
    }
}

/// The T0 display identity (§2.1): the OS user, for UI copy and audit
/// `target` text only. Never a principal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostIdentity {
    pub username: String,
}

impl HostIdentity {
    pub fn from_env() -> Self {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        HostIdentity { username }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::Cap;

    #[test]
    fn admin_strength_is_p26() {
        let p = PrincipalId::new_v7();
        assert!(AccessCtx::operator(Some(p), None).admin_strength);
        assert!(AccessCtx::session(p, "s".into()).admin_strength);
        assert!(AccessCtx::device(p, "d".into(), Tier::Full, 0).admin_strength);
        for tier in [Tier::View, Tier::Dispatch, Tier::Approve] {
            assert!(!AccessCtx::device(p, "d".into(), tier, 0).admin_strength);
        }
        assert!(!AccessCtx::child(Some(CapSet::ALL), None).admin_strength);
    }

    #[test]
    fn a_child_without_the_caps_header_holds_nothing() {
        assert_eq!(AccessCtx::child(None, None).caps, CapSet::EMPTY);
        let view = AccessCtx::device(PrincipalId::new_v7(), "d".into(), Tier::View, 3);
        assert!(!view.caps.contains(Cap::Dispatch));
        assert_eq!(view.grant_epoch, Some(3));
    }
}
