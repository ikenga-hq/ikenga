//! The capability vocabulary (G-ACCESS §1, DEC-76) — **the one source of
//! truth**. `src/lib/access/caps.gen.ts` is rendered from this file by
//! [`super::caps_ts`] and a golden test keeps the two in step (§1.5, A-2).
//!
//! Frozen at G-ACCESS Round 60: W4/W5 WPs only **read** this file. Any change
//! is a G-ACCESS erratum made by the orchestrator (§1.5 "Owner").
//!
//! * Seven boolean rows ([`Cap`]); `secrets` is additionally *never* for a
//!   non-Owner role, which is a property of the role, not a third level
//!   (P-1).
//! * Device tiers ([`Tier`]) are named, ordered subsets of the rows (§1.3).
//! * Four roles ([`Role`]) with D-05's default matrix (§4.1).
//! * Every RPC arm has a [`Requirement`]: a class ([`ArmClass`]) and a
//!   conjunction of caps (§1.6). The table itself lives in
//!   [`super::rpc_requirements`] (append-only for arm-adding WPs).
//! * Effective permission = role ∩ tier ∩ share ceiling, with `approve`
//!   only if routing allows (§1.4) — [`effective`].

use std::fmt;

/// Bumped by an erratum that changes the vocabulary's shape (§1.5).
pub const ACCESS_SCHEMA_VERSION: u32 = 1;

/// One policy row (§1.1). `#[repr(u8)]`: the discriminant is the bit index
/// in a [`CapSet`].
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

/// Every row, in the canonical (D-05 `POLROWS`) order.
pub const CAPS: [Cap; 7] = [
    Cap::Files,
    Cap::Sessions,
    Cap::Dispatch,
    Cap::Approve,
    Cap::Install,
    Cap::Settings,
    Cap::Secrets,
];

impl Cap {
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
        CAPS.into_iter().find(|c| c.as_str() == s)
    }

    pub const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    /// D-05 `POLROWS` copy (UI labels; unchanged, §1.1).
    pub const fn label(self) -> (&'static str, &'static str) {
        match self {
            Cap::Files => ("View files", "The project tree and file contents"),
            Cap::Sessions => (
                "View sessions",
                "Live and past Chi transcripts, cost, tool feed",
            ),
            Cap::Dispatch => (
                "Dispatch to Chi",
                "Send an instruction to a session or start a run",
            ),
            Cap::Approve => (
                "Approve permissions",
                "Answer Chi\u{2019}s asks on your behalf",
            ),
            Cap::Install => ("Install Ngwa", "Add or update packages in this project"),
            Cap::Settings => ("Edit settings", "Change project-scoped configuration"),
            Cap::Secrets => (
                "Read secrets",
                "Never grantable \u{2014} the vault stays on the host",
            ),
        }
    }
}

impl fmt::Display for Cap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of caps as a `u8` bitset (§1.2: role ∩ tier is a plain
/// intersection).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CapSet(u8);

impl CapSet {
    pub const EMPTY: CapSet = CapSet(0);
    pub const ALL: CapSet = CapSet(0b0111_1111);

    /// `const` so the requirement tables can be built at compile time.
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

    pub const fn with(self, cap: Cap) -> CapSet {
        CapSet(self.0 | cap.bit())
    }

    pub const fn without(self, cap: Cap) -> CapSet {
        CapSet(self.0 & !cap.bit())
    }

    pub const fn intersect(self, other: CapSet) -> CapSet {
        CapSet(self.0 & other.0)
    }

    pub const fn union(self, other: CapSet) -> CapSet {
        CapSet(self.0 | other.0)
    }

    pub const fn is_subset_of(self, other: CapSet) -> bool {
        self.0 & !other.0 == 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The caps of `need` this set lacks (§1.6 rule 3: conjunctions).
    pub const fn missing(self, need: CapSet) -> CapSet {
        CapSet(need.0 & !self.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Cap> {
        CAPS.into_iter().filter(move |c| self.contains(*c))
    }

    pub fn names(self) -> Vec<&'static str> {
        self.iter().map(Cap::as_str).collect()
    }

    /// The `X-Ikenga-Caps` wire form (§4.5.3): comma-separated, canonical
    /// order, empty string for none.
    pub fn to_header(self) -> String {
        self.names().join(",")
    }

    /// Parse `X-Ikenga-Caps`. Unknown tokens are dropped: a caps header can
    /// only **narrow** (§1.7), so anything unrecognised grants nothing.
    pub fn parse_header(raw: &str) -> CapSet {
        raw.split(',')
            .filter_map(|t| Cap::parse(t.trim()))
            .fold(CapSet::EMPTY, CapSet::with)
    }
}

impl fmt::Debug for CapSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.names()).finish()
    }
}

impl FromIterator<Cap> for CapSet {
    fn from_iter<I: IntoIterator<Item = Cap>>(iter: I) -> Self {
        iter.into_iter().fold(CapSet::EMPTY, CapSet::with)
    }
}

/// A device tier (§1.3). Ordered: `view < dispatch < approve < full`, each a
/// superset of the one before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    View,
    Dispatch,
    Approve,
    Full,
}

pub const TIERS: [Tier; 4] = [Tier::View, Tier::Dispatch, Tier::Approve, Tier::Full];

impl Tier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::View => "view",
            Tier::Dispatch => "dispatch",
            Tier::Approve => "approve",
            Tier::Full => "full",
        }
    }

    pub fn parse(s: &str) -> Option<Tier> {
        TIERS.into_iter().find(|t| t.as_str() == s)
    }

    pub const fn caps(self) -> CapSet {
        match self {
            Tier::View => CapSet::of(TIER_VIEW),
            Tier::Dispatch => CapSet::of(TIER_DISPATCH),
            Tier::Approve => CapSet::of(TIER_APPROVE),
            Tier::Full => CapSet::ALL,
        }
    }

    /// D-05 `CAPS` copy.
    pub const fn label(self) -> (&'static str, &'static str) {
        match self {
            Tier::View => (
                "View only",
                "Read files, artifacts and session transcripts. Cannot type at Chi.",
            ),
            Tier::Dispatch => (
                "View + dispatch",
                "Everything above, plus sending instructions to a session.",
            ),
            Tier::Approve => (
                "Dispatch + approve",
                "Everything above, plus answering Chi\u{2019}s permission asks.",
            ),
            Tier::Full => (
                "Full",
                "Everything this machine can do, including settings and packages.",
            ),
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

/// §1.3 as a table (rendered into `TIER_CAPS`).
pub const TIER_CAPS: [(Tier, &[Cap]); 4] = [
    (Tier::View, TIER_VIEW),
    (Tier::Dispatch, TIER_DISPATCH),
    (Tier::Approve, TIER_APPROVE),
    (Tier::Full, &CAPS),
];

/// A project role (§4, DEC-78).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Owner,
    Operator,
    Reviewer,
    Guest,
}

pub const ROLES: [Role; 4] = [Role::Owner, Role::Operator, Role::Reviewer, Role::Guest];

impl Role {
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Operator => "operator",
            Role::Reviewer => "reviewer",
            Role::Guest => "guest",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        ROLES.into_iter().find(|r| r.as_str() == s)
    }

    /// §4.1 defaults (before per-project overrides).
    pub const fn default_caps(self) -> CapSet {
        match self {
            Role::Owner => CapSet::ALL,
            Role::Operator => CapSet::of(ROLE_OPERATOR),
            Role::Reviewer => CapSet::of(ROLE_REVIEWER),
            Role::Guest => CapSet::EMPTY,
        }
    }

    /// Caps this role can never hold, whatever an override says (§4.1).
    pub const fn never(self) -> CapSet {
        match self {
            Role::Owner => CapSet::EMPTY,
            _ => CapSet::of(&[Cap::Secrets]),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const ROLE_OPERATOR: &[Cap] = &[Cap::Files, Cap::Sessions, Cap::Dispatch, Cap::Approve];
const ROLE_REVIEWER: &[Cap] = &[Cap::Files, Cap::Sessions];

/// §4.1's default matrix (D-05 `POL`).
pub const ROLE_DEFAULT_CAPS: [(Role, &[Cap]); 4] = [
    (Role::Owner, &CAPS),
    (Role::Operator, ROLE_OPERATOR),
    (Role::Reviewer, ROLE_REVIEWER),
    (Role::Guest, &[]),
];

/// `secrets` is never grantable to a non-Owner (§4.1, A-4).
pub const NEVER_GRANTABLE: [(Role, &[Cap]); 3] = [
    (Role::Operator, &[Cap::Secrets]),
    (Role::Reviewer, &[Cap::Secrets]),
    (Role::Guest, &[Cap::Secrets]),
];

/// An RPC arm's class (§1.6 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArmClass {
    /// Reachable through a share when the caps hold; the child narrows it.
    Shared,
    /// Own workspace only; a share request gets `forbidden`.
    Owner,
    /// Only the T0 operator bearer; the T1 broker never proxies it.
    Operator,
    /// G-ACCESS's own arms: per-command rules (§9.1), served by the access
    /// module, never by a T1 child.
    Access,
    /// Broker → child only, on the per-child token, with no share header.
    Internal,
}

pub const ARM_CLASSES: [ArmClass; 5] = [
    ArmClass::Shared,
    ArmClass::Owner,
    ArmClass::Operator,
    ArmClass::Access,
    ArmClass::Internal,
];

impl ArmClass {
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

impl fmt::Display for ArmClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one RPC arm needs (§1.6): a class and a conjunction of caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub class: ArmClass,
    pub caps: CapSet,
}

impl Requirement {
    pub const fn shared(caps: &[Cap]) -> Requirement {
        Requirement {
            class: ArmClass::Shared,
            caps: CapSet::of(caps),
        }
    }

    pub const fn owner(caps: &[Cap]) -> Requirement {
        Requirement {
            class: ArmClass::Owner,
            caps: CapSet::of(caps),
        }
    }

    pub const fn operator() -> Requirement {
        Requirement {
            class: ArmClass::Operator,
            caps: CapSet::EMPTY,
        }
    }

    pub const fn access() -> Requirement {
        Requirement {
            class: ArmClass::Access,
            caps: CapSet::EMPTY,
        }
    }

    pub const fn internal() -> Requirement {
        Requirement {
            class: ArmClass::Internal,
            caps: CapSet::EMPTY,
        }
    }

    /// §1.6 rule 2: an unmapped command is `{caps: all 7, class: owner}`.
    pub const UNMAPPED: Requirement = Requirement {
        class: ArmClass::Owner,
        caps: CapSet::ALL,
    };
}

/// The inputs of §1.4's formula for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleContext {
    /// The principal's own workspace (always the case under T0): the Owner,
    /// all seven.
    OwnWorkspace,
    /// A membership in someone else's project (T1 only, §4.5).
    Share {
        role: Role,
        /// The project's override-applied matrix row for `role` (§4.1);
        /// WP-76 computes it from `project_role_caps`.
        row: CapSet,
        /// `scope_kind = 'artifact'` adds `{files}` (the artifact grant,
        /// §1.4 / P-37).
        artifact_scope: bool,
    },
}

/// `role_caps(ctx.principal, ctx.context)` (§1.4).
pub fn role_caps(context: RoleContext) -> CapSet {
    match context {
        RoleContext::OwnWorkspace => Role::Owner.default_caps(),
        RoleContext::Share {
            role,
            row,
            artifact_scope,
        } => {
            let row = row.intersect(CapSet::ALL.intersect(CapSet(!role.never().bits())));
            if artifact_scope {
                row.with(Cap::Files)
            } else {
                row
            }
        }
    }
}

/// §1.4: `role_caps ∩ tier_caps ∩ share_ceiling`, with `approve` kept only
/// if `routing_ok` (§5.1). `share_ceiling` is all seven unless the request
/// is a share.
pub fn effective(
    context: RoleContext,
    tier: Tier,
    share_ceiling: CapSet,
    routing_ok: bool,
) -> CapSet {
    let caps = role_caps(context)
        .intersect(tier.caps())
        .intersect(share_ceiling);
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
        for w in TIERS.windows(2) {
            assert!(w[0] < w[1]);
            assert!(
                w[0].caps().is_subset_of(w[1].caps()),
                "{:?} ⊆ {:?}",
                w[0],
                w[1]
            );
            assert_ne!(w[0].caps(), w[1].caps());
        }
        assert_eq!(Tier::Full.caps(), CapSet::ALL);
        for (tier, caps) in TIER_CAPS {
            assert_eq!(tier.caps(), CapSet::of(caps), "{tier}");
        }
        assert!(
            !Tier::Dispatch.caps().contains(Cap::Approve),
            "approve ∉ dispatch tier"
        );
    }

    #[test]
    fn role_matrix_matches_d05_and_secrets_is_owner_only() {
        for (role, caps) in ROLE_DEFAULT_CAPS {
            assert_eq!(role.default_caps(), CapSet::of(caps), "{role}");
        }
        for (role, never) in NEVER_GRANTABLE {
            assert_eq!(role.never(), CapSet::of(never));
            assert!(!role.default_caps().contains(Cap::Secrets), "{role}");
        }
        assert_eq!(Role::Owner.default_caps(), CapSet::ALL);
    }

    #[test]
    fn names_round_trip() {
        for cap in CAPS {
            assert_eq!(Cap::parse(cap.as_str()), Some(cap));
        }
        for tier in TIERS {
            assert_eq!(Tier::parse(tier.as_str()), Some(tier));
        }
        for role in ROLES {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Cap::parse("Files"), None, "lowercase `code` only");
    }

    #[test]
    fn the_caps_header_only_narrows() {
        let caps = CapSet::of(&[Cap::Files, Cap::Dispatch]);
        assert_eq!(caps.to_header(), "files,dispatch");
        assert_eq!(CapSet::parse_header("files, dispatch"), caps);
        assert_eq!(CapSet::parse_header("everything,root,*"), CapSet::EMPTY);
        assert_eq!(CapSet::parse_header(""), CapSet::EMPTY);
        assert_eq!(CapSet::parse_header(&CapSet::ALL.to_header()), CapSet::ALL);
    }

    #[test]
    fn missing_is_the_unmet_conjunction() {
        let have = Tier::Dispatch.caps();
        let need = CapSet::of(&[Cap::Files, Cap::Approve, Cap::Settings]);
        assert_eq!(have.missing(need).names(), ["approve", "settings"]);
        assert!(have.missing(CapSet::of(&[Cap::Files])).is_empty());
    }

    /// A-3: `effective ⊆ role_caps ∩ tier_caps` (the artifact grant included
    /// in `role_caps`), for every role × scope × tier × override row ×
    /// routing combination; `secrets` never reaches a non-Owner.
    #[test]
    fn effective_is_bounded_by_role_and_tier_everywhere() {
        let mut contexts = vec![RoleContext::OwnWorkspace];
        for role in ROLES {
            for row in 0u8..=0x7f {
                for artifact_scope in [false, true] {
                    contexts.push(RoleContext::Share {
                        role,
                        row: CapSet(row),
                        artifact_scope,
                    });
                }
            }
        }
        for context in contexts {
            for tier in TIERS {
                for ceiling in [CapSet::ALL, CapSet::of(&[Cap::Files]), CapSet::EMPTY] {
                    for routing_ok in [true, false] {
                        let eff = effective(context, tier, ceiling, routing_ok);
                        let bound = role_caps(context).intersect(tier.caps());
                        assert!(eff.is_subset_of(bound), "{context:?} {tier} {eff:?}");
                        assert!(eff.is_subset_of(ceiling));
                        if !routing_ok {
                            assert!(!eff.contains(Cap::Approve));
                        }
                        // Owner is never a member role; for the other three
                        // `secrets` never survives, whatever the row says.
                        if let RoleContext::Share { role, .. } = context {
                            if role != Role::Owner {
                                assert!(!eff.contains(Cap::Secrets), "{role} {eff:?}");
                                assert!(!role_caps(context).contains(Cap::Secrets));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_artifact_grant_gives_a_guest_files() {
        let guest = RoleContext::Share {
            role: Role::Guest,
            row: Role::Guest.default_caps(),
            artifact_scope: true,
        };
        assert_eq!(role_caps(guest), CapSet::of(&[Cap::Files]));
        assert_eq!(
            effective(guest, Tier::View, CapSet::ALL, true),
            CapSet::of(&[Cap::Files])
        );
    }
}
