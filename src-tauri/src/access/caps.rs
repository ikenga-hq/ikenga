//! The capability vocabulary (G-ACCESS §1, DEC-76): the one source of truth.
//!
//! Seven boolean rows (`files` … `secrets`), four device tiers that are named
//! subsets of them, four roles with their D-05 default matrix, and the arm
//! classes of §1.6. `src/lib/access/caps.gen.ts` is rendered from this file
//! by `caps_ts.rs` (§1.5); a golden test keeps the two in step.
//!
//! **Frozen.** After the G-ACCESS freeze (shell-ux Round 60) this file
//! changes only by an erratum recorded in `04`. W4/W5 WPs read it.

use std::fmt;

/// `caps.gen.ts`'s `ACCESS_SCHEMA_VERSION`.
pub const ACCESS_SCHEMA_VERSION: u32 = 1;

/// One policy row (§1.1). The discriminant is the bit index in [`CapSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Cap {
    Files = 0,
    Sessions = 1,
    Dispatch = 2,
    Approve = 3,
    Install = 4,
    Settings = 5,
    Secrets = 6,
}

impl Cap {
    /// All seven, in the canonical (D-05 `POLROWS`) order.
    pub const ALL: [Cap; 7] = [
        Cap::Files,
        Cap::Sessions,
        Cap::Dispatch,
        Cap::Approve,
        Cap::Install,
        Cap::Settings,
        Cap::Secrets,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Cap::Files => "files",
            Cap::Sessions => "sessions",
            Cap::Dispatch => "dispatch",
            Cap::Approve => "approve",
            Cap::Install => "install",
            Cap::Settings => "settings",
            Cap::Secrets => "secrets",
        }
    }

    pub fn parse(s: &str) -> Option<Cap> {
        Cap::ALL.into_iter().find(|c| c.as_str() == s)
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl fmt::Display for Cap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of [`Cap`]s as a `u8` bitset (§1.2: role ∩ tier is a plain set
/// intersection).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapSet(u8);

impl CapSet {
    pub const EMPTY: CapSet = CapSet(0);
    pub const ALL: CapSet = CapSet(0b0111_1111);

    pub const fn of(caps: &[Cap]) -> CapSet {
        let mut bits = 0u8;
        let mut i = 0;
        while i < caps.len() {
            bits |= caps[i].bit();
            i += 1;
        }
        CapSet(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, cap: Cap) -> bool {
        self.0 & cap.bit() != 0
    }

    /// Every cap of `other` is in `self`.
    pub const fn contains_all(self, other: CapSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersect(self, other: CapSet) -> CapSet {
        CapSet(self.0 & other.0)
    }

    pub const fn union(self, other: CapSet) -> CapSet {
        CapSet(self.0 | other.0)
    }

    pub const fn without(self, cap: Cap) -> CapSet {
        CapSet(self.0 & !cap.bit())
    }

    pub const fn with(self, cap: Cap) -> CapSet {
        CapSet(self.0 | cap.bit())
    }

    /// The caps of `required` that `self` lacks.
    pub const fn missing(self, required: CapSet) -> CapSet {
        CapSet(required.0 & !self.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Cap> {
        Cap::ALL.into_iter().filter(move |c| self.contains(*c))
    }

    pub fn names(self) -> Vec<&'static str> {
        self.iter().map(Cap::as_str).collect()
    }

    /// `X-Ikenga-Caps` wire form (§4.5.3): comma-separated, canonical order.
    pub fn to_header(self) -> String {
        self.names().join(",")
    }

    /// Parse `X-Ikenga-Caps`. Fail closed: an unknown token makes the whole
    /// header worthless (`None`), and the caller then grants nothing.
    pub fn parse_header(s: &str) -> Option<CapSet> {
        let mut set = CapSet::EMPTY;
        for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            set = set.with(Cap::parse(tok)?);
        }
        Some(set)
    }
}

impl fmt::Debug for CapSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CapSet{:?}", self.names())
    }
}

/// A device grant's tier (§1.3). Ordered: each is a superset of the one
/// before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    View,
    Dispatch,
    Approve,
    Full,
}

impl Tier {
    pub const ALL: [Tier; 4] = [Tier::View, Tier::Dispatch, Tier::Approve, Tier::Full];

    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::View => "view",
            Tier::Dispatch => "dispatch",
            Tier::Approve => "approve",
            Tier::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.as_str() == s)
    }

    pub const fn caps(self) -> CapSet {
        match self {
            Tier::View => CapSet::of(TIER_VIEW),
            Tier::Dispatch => CapSet::of(TIER_DISPATCH),
            Tier::Approve => CapSet::of(TIER_APPROVE),
            Tier::Full => CapSet::ALL,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const TIER_VIEW: &[Cap] = &[Cap::Files, Cap::Sessions];
const TIER_DISPATCH: &[Cap] = &[Cap::Files, Cap::Sessions, Cap::Dispatch];
const TIER_APPROVE: &[Cap] = &[Cap::Files, Cap::Sessions, Cap::Dispatch, Cap::Approve];

/// §1.3: the tier → caps table, as `caps.gen.ts` exports it.
pub const TIER_CAPS: [(Tier, &[Cap]); 4] = [
    (Tier::View, TIER_VIEW),
    (Tier::Dispatch, TIER_DISPATCH),
    (Tier::Approve, TIER_APPROVE),
    (Tier::Full, &Cap::ALL),
];

/// A project role (§4.1, DEC-78).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Owner,
    Operator,
    Reviewer,
    Guest,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Owner, Role::Operator, Role::Reviewer, Role::Guest];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Operator => "operator",
            Role::Reviewer => "reviewer",
            Role::Guest => "guest",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| r.as_str() == s)
    }

    /// The §4.1 default matrix row.
    pub const fn default_caps(self) -> CapSet {
        match self {
            Role::Owner => CapSet::ALL,
            Role::Operator => CapSet::of(ROLE_OPERATOR),
            Role::Reviewer => CapSet::of(ROLE_REVIEWER),
            Role::Guest => CapSet::EMPTY,
        }
    }

    /// Caps this role can never hold, whatever an override says (§4.1:
    /// `secrets` for every non-Owner).
    pub const fn never_grantable(self) -> CapSet {
        match self {
            Role::Owner => CapSet::EMPTY,
            _ => CapSet::of(&[Cap::Secrets]),
        }
    }
}

const ROLE_OPERATOR: &[Cap] = &[Cap::Files, Cap::Sessions, Cap::Dispatch, Cap::Approve];
const ROLE_REVIEWER: &[Cap] = &[Cap::Files, Cap::Sessions];

/// §4.1 default matrix, as `caps.gen.ts` exports it.
pub const ROLE_DEFAULT_CAPS: [(Role, &[Cap]); 4] = [
    (Role::Owner, &Cap::ALL),
    (Role::Operator, ROLE_OPERATOR),
    (Role::Reviewer, ROLE_REVIEWER),
    (Role::Guest, &[]),
];

/// §4.1: `secrets` is never grantable to a non-Owner.
pub const NEVER_GRANTABLE: [(Role, &[Cap]); 3] = [
    (Role::Operator, &[Cap::Secrets]),
    (Role::Reviewer, &[Cap::Secrets]),
    (Role::Guest, &[Cap::Secrets]),
];

/// D-05 `POLROWS` copy (§1.1), `{label, sub}` per cap. UI copy only.
pub const CAP_LABELS: [(Cap, &str, &str); 7] = [
    (
        Cap::Files,
        "View files",
        "The project tree and file contents",
    ),
    (
        Cap::Sessions,
        "View sessions",
        "Live and past Chi transcripts, cost, tool feed",
    ),
    (
        Cap::Dispatch,
        "Dispatch to Chi",
        "Send an instruction to a session or start a run",
    ),
    (
        Cap::Approve,
        "Approve permissions",
        "Answer Chi\u{2019}s asks on your behalf",
    ),
    (
        Cap::Install,
        "Install Ngwa",
        "Add or update packages in this project",
    ),
    (
        Cap::Settings,
        "Edit settings",
        "Change project-scoped configuration",
    ),
    (
        Cap::Secrets,
        "Read secrets",
        "Never grantable \u{2014} the vault stays on the host",
    ),
];

/// D-05 `CAPS` copy (§1.3), `{label, long}` per tier. UI copy only.
pub const TIER_LABELS: [(Tier, &str, &str); 4] = [
    (
        Tier::View,
        "View only",
        "Read files, artifacts and session transcripts. Cannot type at Chi.",
    ),
    (
        Tier::Dispatch,
        "View + dispatch",
        "Everything above, plus sending instructions to a session.",
    ),
    (
        Tier::Approve,
        "Dispatch + approve",
        "Everything above, plus answering Chi\u{2019}s permission asks.",
    ),
    (
        Tier::Full,
        "Full",
        "Everything this machine can do, including settings and packages.",
    ),
];

/// An RPC arm's class (§1.6 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmClass {
    /// Reachable through a share when the caps hold; narrowed to the project.
    Shared,
    /// Own workspace only; a share request is `forbidden` (`class=owner`).
    Owner,
    /// Only the T0 operator bearer; the T1 broker never proxies it.
    Operator,
    /// G-ACCESS's own arms: per-command rules (§9.1), served by the access
    /// module (T0 daemon / T1 broker), never by a T1 child.
    Access,
    /// Broker → child only, per-child token, no share header (§4.5.3).
    Internal,
}

impl ArmClass {
    pub const ALL: [ArmClass; 5] = [
        ArmClass::Shared,
        ArmClass::Owner,
        ArmClass::Operator,
        ArmClass::Access,
        ArmClass::Internal,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ArmClass::Shared => "shared",
            ArmClass::Owner => "owner",
            ArmClass::Operator => "operator",
            ArmClass::Access => "access",
            ArmClass::Internal => "internal",
        }
    }
}

/// What an arm (or route) needs: a class and a conjunction of caps (§1.6
/// rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub caps: CapSet,
    pub class: ArmClass,
}

impl Requirement {
    pub const fn shared(caps: &[Cap]) -> Self {
        Requirement {
            caps: CapSet::of(caps),
            class: ArmClass::Shared,
        }
    }

    pub const fn owner(caps: &[Cap]) -> Self {
        Requirement {
            caps: CapSet::of(caps),
            class: ArmClass::Owner,
        }
    }

    /// The T0 operator bearer only. It always holds all seven, so the caps
    /// are all seven too: a UI reading the table never offers the control
    /// to anything less.
    pub const fn operator() -> Self {
        Requirement {
            caps: CapSet::ALL,
            class: ArmClass::Operator,
        }
    }

    pub const fn access() -> Self {
        Requirement {
            caps: CapSet::EMPTY,
            class: ArmClass::Access,
        }
    }

    pub const fn internal() -> Self {
        Requirement {
            caps: CapSet::EMPTY,
            class: ArmClass::Internal,
        }
    }

    /// §1.6 rule 2: what an unmapped command needs at runtime.
    pub const UNMAPPED: Requirement = Requirement {
        caps: CapSet::ALL,
        class: ArmClass::Owner,
    };
}

/// `effective(ctx)` for the own-workspace case (§1.4): the Owner's role caps
/// are all seven, so effective = tier ∩ routing. Shares (role ∩ tier ∩
/// ceiling) are WP-76's (`access::share`).
pub fn own_workspace_caps(tier: Tier, routing_ok: bool) -> CapSet {
    let caps = Role::Owner.default_caps().intersect(tier.caps());
    if routing_ok {
        caps
    } else {
        caps.without(Cap::Approve)
    }
}

/// `role_caps` of a share membership (§1.4): the matrix row (with the
/// project's overrides already applied by the caller), minus what the role
/// can never hold, plus `files` for an artifact-scope membership.
pub fn member_role_caps(role: Role, matrix_row: CapSet, artifact_scope: bool) -> CapSet {
    let row = matrix_row.intersect(CapSet::ALL.missing(role.never_grantable()));
    if artifact_scope {
        row.with(Cap::Files)
    } else {
        row
    }
}

/// The full §1.4 formula: role ∩ tier ∩ share ceiling, `approve` only when
/// routing allows it. `role_caps` comes from [`member_role_caps`] (or all
/// seven for the Owner).
pub fn effective(role_caps: CapSet, tier: Tier, share_ceiling: CapSet, routing_ok: bool) -> CapSet {
    let caps = role_caps.intersect(tier.caps()).intersect(share_ceiling);
    if routing_ok {
        caps
    } else {
        caps.without(Cap::Approve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_are_ordered_supersets() {
        for w in Tier::ALL.windows(2) {
            assert!(w[0] < w[1]);
            assert!(w[1].caps().contains_all(w[0].caps()), "{:?}", w);
            assert_ne!(w[1].caps(), w[0].caps());
        }
        assert_eq!(Tier::Full.caps(), CapSet::ALL);
        assert!(!Tier::Dispatch.caps().contains(Cap::Approve));
        for (tier, caps) in TIER_CAPS {
            assert_eq!(tier.caps(), CapSet::of(caps));
        }
    }

    #[test]
    fn the_default_matrix_is_d05_pol() {
        for (role, caps) in ROLE_DEFAULT_CAPS {
            assert_eq!(role.default_caps(), CapSet::of(caps));
        }
        assert_eq!(Role::Owner.default_caps(), CapSet::ALL);
        for (role, never) in NEVER_GRANTABLE {
            assert_eq!(role.never_grantable(), CapSet::of(never));
            assert!(!role.default_caps().contains(Cap::Secrets));
        }
    }

    #[test]
    fn header_round_trips_and_fails_closed() {
        for bits in 0..=CapSet::ALL.bits() {
            let set = CapSet(bits);
            assert_eq!(CapSet::parse_header(&set.to_header()), Some(set));
        }
        assert_eq!(CapSet::parse_header(""), Some(CapSet::EMPTY));
        assert_eq!(
            CapSet::parse_header("files, sessions"),
            Some(Tier::View.caps())
        );
        assert_eq!(CapSet::parse_header("files,root"), None);
        assert_eq!(CapSet::parse_header("FILES"), None);
    }

    /// A-3: effective ⊆ role_caps ∩ tier_caps (with the artifact-grant
    /// term) for every role × scope × tier × override row × routing.
    #[test]
    fn effective_is_bounded_by_role_and_tier_for_every_combination() {
        for role in Role::ALL {
            for artifact_scope in [false, true] {
                for tier in Tier::ALL {
                    for row_bits in 0..=CapSet::ALL.bits() {
                        let row = if role == Role::Owner {
                            CapSet::ALL
                        } else {
                            CapSet(row_bits)
                        };
                        let role_caps = member_role_caps(role, row, artifact_scope);
                        for ceiling_bits in [0, CapSet::ALL.bits(), Tier::View.caps().bits()] {
                            for routing_ok in [false, true] {
                                let eff =
                                    effective(role_caps, tier, CapSet(ceiling_bits), routing_ok);
                                assert!(
                                    role_caps.intersect(tier.caps()).contains_all(eff),
                                    "{role:?} {tier:?} {row:?} → {eff:?}"
                                );
                                if role != Role::Owner {
                                    assert!(!eff.contains(Cap::Secrets), "A-4 for {role:?}");
                                }
                                if !routing_ok {
                                    assert!(!eff.contains(Cap::Approve));
                                }
                            }
                        }
                    }
                }
            }
        }
        // The artifact grant: a Guest with artifact scope holds `files`.
        assert!(member_role_caps(Role::Guest, CapSet::EMPTY, true).contains(Cap::Files));
        assert!(!member_role_caps(Role::Guest, CapSet::EMPTY, false).contains(Cap::Files));
    }

    #[test]
    fn own_workspace_is_the_tier_with_routing_removing_approve() {
        assert_eq!(own_workspace_caps(Tier::Full, true), CapSet::ALL);
        assert_eq!(
            own_workspace_caps(Tier::Approve, false),
            Tier::Dispatch.caps()
        );
    }
}
