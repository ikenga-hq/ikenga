//! The per-request access context (G-ACCESS §2.3).
//!
//! On T1 an [`AccessCtx`] is derived right after G-PRINCIPAL's
//! `PrincipalCtx` resolves (`access::t1`). On T0 no G-PRINCIPAL `Principal`
//! or `PrincipalCtx` exists at all (§2.1, review C-10): the T0 resolver
//! (`server::auth_middleware`) builds the `AccessCtx` directly from the
//! credential, with the synthetic owner's id.
//!
//! `via` is [`Via`], a platform-neutral mirror of G-PRINCIPAL's
//! `Credential` kinds: `server::auth::Credential` is Linux-only (T1 is
//! Linux-only) while T0 runs everywhere, so this module cannot name it. On
//! Linux, `From<&Credential>` converts.

use super::caps::{Cap, CapSet, Role, Tier};
use crate::executor::PrincipalId;

/// How the caller proved who they are (§2.3's credential column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// T1 password session (cookie `ikenga_session`, WP-20).
    Session { session_id: String },
    /// A paired device (§3.8).
    Device { device_id: String },
    /// The T0 operator bearer (header or `?token=`). Never under T1.
    Operator,
    /// A T1 principal child's per-child token on a call the **broker itself**
    /// makes (marked with [`super::INTERNAL_CALL_HEADER`]): the only context
    /// that reaches an `internal` arm (§9.1). Caps come only from
    /// `X-Ikenga-Caps` (§1.7).
    ChildToken,
    /// A T1 principal child's per-child token on a principal's request the
    /// broker **relayed** (every proxied request): caps only from
    /// `X-Ikenga-Caps`, and never an `internal` arm — so a relay that set no
    /// caps (a broker without the narrower) is a non-granting context, not
    /// the broker's own one.
    Relayed,
}

impl Via {
    /// The wire / audit spelling (`AccessStatus.credential.via`, §6.1 `via`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Via::Session { .. } => "session",
            Via::Device { .. } => "device",
            Via::Operator => "operator",
            Via::ChildToken | Via::Relayed => "system",
        }
    }
}

#[cfg(target_os = "linux")]
impl From<&crate::server::auth::Credential> for Via {
    fn from(c: &crate::server::auth::Credential) -> Self {
        use crate::server::auth::Credential;
        match c {
            Credential::Session { session_id } => Via::Session {
                session_id: session_id.clone(),
            },
            Credential::DeviceGrant { device_id } => Via::Device {
                device_id: device_id.clone(),
            },
            Credential::OperatorBearer => Via::Operator,
        }
    }
}

/// A share selection (§4.5), T1 only. WP-74a defines it and parses the
/// broker → child headers; WP-76 builds it broker-side and fills the
/// confinement hooks (`access::share`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareCtx {
    /// `<owner_principal_id>/<project_id>`.
    pub project_key: String,
    pub project_id: String,
    /// The member (attribution and narrowing only; §4.5.3).
    pub member_principal_id: Option<String>,
    pub member_device_id: Option<String>,
    pub role: Option<Role>,
    /// Artifact scope: the one relative path `files` is confined to.
    pub artifact_path: Option<String>,
    /// `X-Ikenga-Share-Policy: owner-approval` (§5.4).
    pub owner_approval: bool,
}

/// Request metadata kept for audit rows (§6.1 `remote_addr`, `user_agent`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestMeta {
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
    /// The `Host` the request arrived on (§3.3 rule 3: the pairing QR's
    /// base when no `--public-url` is set and the host isn't loopback).
    pub host: Option<String>,
    /// `http` / `https`, from the request's `Origin` when it has one.
    pub scheme: Option<String>,
    /// §5.1 / A-23 (WP-75): the principal's routing preference withheld an
    /// `approve` the role and tier would otherwise grant — set where the
    /// effective caps are computed ([`routing_withheld_approve`]), so a
    /// refusal is `routing_refused` only when routing caused it (review
    /// WP75-R5). `false` where caps arrive precomputed (a T1 child).
    pub routing_withheld_approve: bool,
}

/// Whether routing (and only routing) removed `approve` from
/// `caps::effective(context, tier, ceiling, routing_ok)`.
pub fn routing_withheld_approve(
    context: super::caps::RoleContext,
    tier: Tier,
    ceiling: CapSet,
    routing_ok: bool,
) -> bool {
    !routing_ok && super::caps::effective(context, tier, ceiling, true).contains(Cap::Approve)
}

impl RequestMeta {
    /// `host` + `scheme` from a request's headers (WP-74b, §3.3).
    pub fn with_host_from(mut self, headers: &axum::http::HeaderMap) -> Self {
        let get = |n: &str| {
            headers
                .get(n)
                .and_then(|h| h.to_str().ok())
                .map(str::to_string)
        };
        self.host = get("host");
        self.scheme = get("origin")
            .and_then(|o| o.split_once("://").map(|(s, _)| s.to_ascii_lowercase()))
            .filter(|s| s == "http" || s == "https");
        self
    }
}

/// G-ACCESS's one derived, per-request struct (§2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessCtx {
    /// T1: `PrincipalCtx.principal.id`; T0: the synthetic owner id. On a T1
    /// child it is the broker's `X-Ikenga-Principal` (attribution only, never
    /// authorization — G-PRINCIPAL §3).
    pub principal_id: PrincipalId,
    pub via: Via,
    /// DeviceGrant → that device; T0 OperatorBearer → the host device;
    /// Session → `None`.
    pub device_id: Option<String>,
    pub tier: Tier,
    pub share: Option<ShareCtx>,
    /// Whether the request carried **any** `X-Ikenga-Share-*` header (T1
    /// child only; always `false` elsewhere). `internal` arms are refused
    /// when it is set, even if the headers didn't parse into a [`ShareCtx`]
    /// (§4.5.3: "no `X-Ikenga-Share-*` header" at all).
    pub share_headers: bool,
    /// Effective caps (§1.4), computed once per request / WS handshake.
    pub caps: CapSet,
    /// `via ∈ {Session, OperatorBearer} || tier == Full` (P-26).
    pub admin_strength: bool,
    pub meta: RequestMeta,
}

impl AccessCtx {
    /// P-26.
    pub fn admin_strength_of(via: &Via, tier: Tier) -> bool {
        matches!(via, Via::Session { .. } | Via::Operator) || tier == Tier::Full
    }

    pub fn is_operator(&self) -> bool {
        matches!(self.via, Via::Operator)
    }

    pub fn has(&self, cap: Cap) -> bool {
        self.caps.contains(cap)
    }

    /// The `credential` block of `AccessStatus` (§9.1).
    pub fn credential_json(&self) -> serde_json::Value {
        serde_json::json!({
            "via": match self.via {
                Via::ChildToken | Via::Relayed => "operator",
                _ => self.via.as_str(),
            },
            "deviceId": self.device_id,
            "tier": self.tier.as_str(),
        })
    }
}

/// The T0-only display identity (§2.1): the OS user, for UI copy and audit
/// `target` text only. Never a G-PRINCIPAL `Principal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostIdentity {
    pub username: String,
    pub hostname: String,
}

impl HostIdentity {
    pub fn detect() -> Self {
        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "owner".into());
        let hostname = hostname().unwrap_or_else(|| "this computer".into());
        Self { username, hostname }
    }

    /// The host device row's name (§2.1): the OS hostname, clamped to the
    /// `devices.name` CHECK (1..=64 chars).
    pub fn device_name(&self) -> String {
        let name: String = self.hostname.chars().take(64).collect();
        if name.trim().is_empty() {
            "this computer".into()
        } else {
            name
        }
    }
}

fn hostname() -> Option<String> {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    #[cfg(unix)]
    {
        if let Ok(v) = std::fs::read_to_string("/etc/hostname") {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
        if let Ok(out) = std::process::Command::new("hostname").output() {
            let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_strength_is_p26() {
        assert!(AccessCtx::admin_strength_of(&Via::Operator, Tier::View));
        assert!(AccessCtx::admin_strength_of(
            &Via::Session {
                session_id: "s".into()
            },
            Tier::Full
        ));
        let device = Via::Device {
            device_id: "d".into(),
        };
        assert!(AccessCtx::admin_strength_of(&device, Tier::Full));
        for tier in [Tier::View, Tier::Dispatch, Tier::Approve] {
            assert!(!AccessCtx::admin_strength_of(&device, tier), "{tier}");
        }
        assert!(!AccessCtx::admin_strength_of(
            &Via::ChildToken,
            Tier::Approve
        ));
    }

    #[test]
    fn host_device_name_fits_the_check() {
        let id = HostIdentity {
            username: "u".into(),
            hostname: "x".repeat(100),
        };
        assert_eq!(id.device_name().chars().count(), 64);
        let blank = HostIdentity {
            username: "u".into(),
            hostname: "  ".into(),
        };
        assert_eq!(blank.device_name(), "this computer");
    }
}
