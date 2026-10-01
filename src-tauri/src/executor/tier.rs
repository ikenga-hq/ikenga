//! Executor tiers (ADR-023 §1) and the boot capability probe.

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

/// Isolation tier a host runs sessions under. See ADR-023 for the full table;
/// in short:
///
/// * **T0** — in-process, current behaviour. Single trusted user.
/// * **T1** — per-user Unix uid, per-user data dir / credential store.
/// * **T2** — container per session (Docker socket / K8s / Firecracker / E2B).
/// * **T3** — firejail per session plus network policy.
///
/// Wire form (config, `/api/health`) is lowercase `t0`..`t3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorTier {
    T0,
    T1,
    T2,
    T3,
}

impl ExecutorTier {
    pub const ALL: [ExecutorTier; 4] = [
        ExecutorTier::T0,
        ExecutorTier::T1,
        ExecutorTier::T2,
        ExecutorTier::T3,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ExecutorTier::T0 => "t0",
            ExecutorTier::T1 => "t1",
            ExecutorTier::T2 => "t2",
            ExecutorTier::T3 => "t3",
        }
    }
}

impl Default for ExecutorTier {
    fn default() -> Self {
        ExecutorTier::T0
    }
}

impl fmt::Display for ExecutorTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An `IKENGA_EXECUTOR_TIER` / `--executor-tier` value that names no tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTierError(pub String);

impl fmt::Display for ParseTierError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown executor tier `{}` (expected one of t0, t1, t2, t3)",
            self.0
        )
    }
}

impl std::error::Error for ParseTierError {}

impl FromStr for ExecutorTier {
    type Err = ParseTierError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ExecutorTier::ALL
            .into_iter()
            .find(|t| t.as_str().eq_ignore_ascii_case(s.trim()))
            .ok_or_else(|| ParseTierError(s.to_string()))
    }
}

/// What the host can honour at the resolved tier. Serialized onto
/// `/api/health` so a client (or an operator with `curl`) can see which
/// isolation the sessions it opens actually get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub tier: ExecutorTier,
    /// Can spawn PTY children (terminals).
    pub pty: bool,
    /// Can spawn piped children (engine CLIs).
    pub piped: bool,
    /// Whether `SpawnSpec::principal` is honoured. `false` at T0: every child
    /// runs as the host process's own user.
    pub principal_isolation: bool,
}

/// A §8 boot-probe result as `/api/health` carries it: `ok` and when, nothing
/// else — the endpoint is unauthenticated, so the full report goes to the log
/// and `operator/probe.json` (G-PRINCIPAL §8 "Results").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProbeStamp {
    pub ok: bool,
    /// Unix seconds.
    pub at: u64,
}

/// Why a tier cannot be used. The daemon treats any refusal as fatal at boot
/// (DEC-R9-1): refuse, don't fall back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// This build has no executor for `tier`.
    NotImplemented { tier: ExecutorTier },
    /// A different tier is already live in this process.
    AlreadyInstalled {
        installed: ExecutorTier,
        requested: ExecutorTier,
    },
    /// A boot-probe check failed (G-PRINCIPAL §8). `check` is a stable name
    /// (`os`, `identity`, `capabilities`, `operator_root`, `uid_range`,
    /// `probe_uid`, `test_drop`, `reconcile`, `setup`); `detail` is for the
    /// operator's log, never for an unauthenticated client.
    ProbeFailed { check: &'static str, detail: String },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotImplemented {
                tier: ExecutorTier::T1,
            } => f.write_str(
                "executor tier t1 passed its boot probe, but this build has no T1 broker yet \
                 (WP-20 slice 3), so it can't serve; refusing to start rather than serving a \
                 single-tenant daemon as root. `ikenga-server probe --executor-tier t1` \
                 reports whether this host can run T1",
            ),
            Refusal::NotImplemented { tier } => write!(
                f,
                "executor tier {tier} is not implemented on this build (only t0 and t1 are); \
                 refusing to start rather than falling back to a weaker tier"
            ),
            Refusal::AlreadyInstalled {
                installed,
                requested,
            } => write!(
                f,
                "executor tier {installed} is already installed in this process; \
                 refusing to switch to {requested}"
            ),
            Refusal::ProbeFailed { check, detail } => write!(
                f,
                "t1 boot probe failed at `{check}`: {detail}; refusing to start rather than \
                 falling back to a weaker tier"
            ),
        }
    }
}

impl std::error::Error for Refusal {}

/// Boot capability probe for the tiers that need no operator context.
///
/// T0 always passes. T2/T3 are refused with [`Refusal::NotImplemented`].
/// T1's probe (G-PRINCIPAL §8) is real and needs the operator root, the uid
/// range and `accounts.db`: it runs as `server::operator::probe`, whose
/// host-side steps live in [`super::t1_probe`] and whose passing result is
/// the only way a [`super::t1::T1Executor`] reports isolation. Called without
/// that context, T1 is refused with [`Refusal::ProbeFailed`] (`setup`) — a
/// tier is never *claimed* on a host that hasn't proven it.
pub fn probe(tier: ExecutorTier) -> Result<Capabilities, Refusal> {
    match tier {
        ExecutorTier::T0 => Ok(Capabilities {
            tier,
            pty: true,
            piped: true,
            principal_isolation: false,
        }),
        ExecutorTier::T1 => Err(Refusal::ProbeFailed {
            check: "setup",
            detail: "the t1 probe needs the operator root (--data-dir) and uid range; it runs \
                     from the T1 boot and `ikenga-server probe --executor-tier t1`"
                .into(),
        }),
        ExecutorTier::T2 | ExecutorTier::T3 => Err(Refusal::NotImplemented { tier }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_tier_case_insensitively() {
        for tier in ExecutorTier::ALL {
            assert_eq!(tier.as_str().parse::<ExecutorTier>(), Ok(tier));
            assert_eq!(
                tier.as_str().to_uppercase().parse::<ExecutorTier>(),
                Ok(tier)
            );
        }
        assert_eq!(" t0 ".parse::<ExecutorTier>(), Ok(ExecutorTier::T0));
    }

    #[test]
    fn rejects_unknown_tiers() {
        for bad in ["", "t4", "0", "tier0", "in-process"] {
            assert_eq!(
                bad.parse::<ExecutorTier>(),
                Err(ParseTierError(bad.to_string()))
            );
        }
    }

    #[test]
    fn default_tier_is_t0() {
        assert_eq!(ExecutorTier::default(), ExecutorTier::T0);
    }

    #[test]
    fn probe_passes_t0() {
        let caps = probe(ExecutorTier::T0).expect("t0 always passes");
        assert_eq!(caps.tier, ExecutorTier::T0);
        assert!(caps.pty && caps.piped);
        assert!(!caps.principal_isolation, "t0 runs everything as the host user");
    }

    #[test]
    fn probe_without_operator_context_refuses_t1() {
        let refusal = probe(ExecutorTier::T1).expect_err("needs the operator root");
        assert!(
            matches!(refusal, Refusal::ProbeFailed { check: "setup", .. }),
            "{refusal}"
        );
        assert!(refusal.to_string().contains("t1 boot probe failed"));
    }

    #[test]
    fn probe_refuses_t2_and_t3_with_a_typed_error() {
        for tier in [ExecutorTier::T2, ExecutorTier::T3] {
            let refusal = probe(tier).expect_err("not implemented on this build");
            assert_eq!(refusal, Refusal::NotImplemented { tier });
            let msg = refusal.to_string();
            assert!(msg.contains(tier.as_str()), "{msg}");
            assert!(msg.contains("not implemented"), "{msg}");
        }
    }

    #[test]
    fn serializes_lowercase_on_the_wire() {
        let caps = probe(ExecutorTier::T0).unwrap();
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json["tier"], "t0");
        assert_eq!(json["principal_isolation"], false);
    }
}
