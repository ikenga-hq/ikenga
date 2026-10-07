//! The pure half of `pkg::trust`: the [`TrustState`] wire shape and the
//! sensitive-perms summary of a manifest.
//!
//! Compiled into BOTH binaries. `pkg::trust` (evaluation, grants, the
//! signature check) stays desktop-only — the daemon has no trust store — but
//! the Ngwa snapshot join (`server::shared::ngwa`) names these types in its
//! inputs, and that join is shared by the desktop command and the daemon arm.
//! Moved verbatim out of `pkg/trust.rs`, which re-exports everything here, so
//! every `crate::pkg::trust::…` path is unchanged.

use serde::Serialize;

use crate::pkg::manifest::Permissions;

/// Plain-English summary of one declared permission set, for the trust
/// dialog and the `iyke_pkg_trust_*` MCP tools. Lists only the entries
/// that triggered the trust requirement, not the full perms block.
#[derive(Debug, Clone, Serialize)]
pub struct PermsSummary {
    pub shell_execute: Vec<String>,
    pub fs_write_outside_sandbox: Vec<String>,
    pub net: Vec<String>,
    pub vault_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsApprovalReason {
    /// First time the user has seen this pkg.
    Never,
    /// Pkg was previously trusted but the sensitive-perms snapshot
    /// changed. `added` / `removed` are the diff against the prior grant.
    PermissionsChanged {
        prior_version: String,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// Existing grant explicitly revoked by the user.
    Revoked,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TrustState {
    /// Provenance-trusted, no user row required; never expires. Reached by:
    /// - `source.kind = "builtin"` AND id starts with `com.ikenga.`,
    /// - `source.kind = "dev"` (the `ikenga dev` bypass), OR
    /// - a `registry` source whose manifest `signature` minisign-verifies
    ///   against the install's `publisher_key` (WP-02 / `pkg::signature`).
    ///
    /// This is the only state that grants `is_trusted_for_elevated()` — the
    /// cryptographic / provenance anchor that elevated host caps
    /// (host.fetch / secrets / invoke) consult. A signed-registry pkg that
    /// reaches `AutoTrusted` is still subject to the sensitive-perms snapshot
    /// for `shell.execute` / unsandboxed `fs.write` (those follow the
    /// builtin's ceiling, i.e. covered by this same auto-trust).
    AutoTrusted,
    /// Pkg declares no sensitive perms — auto-granted on install with no
    /// prompt. Same surface as a user-approved row from the FE's POV.
    AutoGranted,
    /// User explicitly approved this version's sensitive perms.
    Granted { version: String, granted_at_ms: i64 },
    /// Approval needed. Caller must surface the prompt.
    NeedsApproval {
        reason: NeedsApprovalReason,
        current_version: String,
    },
}

impl TrustState {
    /// True when this state allows MCP tools/call to proceed.
    pub fn is_allowed(&self) -> bool {
        matches!(
            self,
            TrustState::AutoTrusted | TrustState::AutoGranted | TrustState::Granted { .. }
        )
    }

    /// The SINGLE gate for *elevated* host capabilities (host.fetch,
    /// secrets/named-secret injection, host.invoke — consumed by WP-03/04/05).
    ///
    /// Returns `true` ONLY for `AutoTrusted`, i.e. a pkg that earned trust by
    /// **provenance**: builtin-in-namespace, a `ikenga dev` mount, or a
    /// registry pkg whose manifest signature cryptographically verified
    /// (`pkg::signature::verify_manifest_signature(...).is_valid()`, wired in
    /// `pkg::trust::evaluate()`).
    ///
    /// It deliberately returns `false` for `AutoGranted` and `Granted`. Those
    /// states mean the pkg either declares no sensitive perms (`AutoGranted`)
    /// or a *user clicked approve* on its sensitive-perms snapshot
    /// (`Granted`). A user approving `shell.execute` / `fs.write` is **not**
    /// the same as provenance trust: approving a community pkg's declared
    /// perms lets its MCP tools run, but it does NOT hand that pkg the
    /// cryptographic anchor that elevated host caps require. Elevated caps are
    /// gated on *who published this and did the bytes verify*, not on *did the
    /// user consent to the declared perms*. The two are intentionally
    /// orthogonal: `is_allowed()` (run at all) vs `is_trusted_for_elevated()`
    /// (reach the privileged host surface).
    pub fn is_trusted_for_elevated(&self) -> bool {
        matches!(self, TrustState::AutoTrusted)
    }

    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            TrustState::AutoTrusted => "auto_trusted",
            TrustState::AutoGranted => "auto_granted",
            TrustState::Granted { .. } => "granted",
            TrustState::NeedsApproval { .. } => "needs_approval",
        }
    }
}

/// True when this raw `fs.write` glob resolves to a path outside the pkg's
/// `$pkg_data` sandbox. `$pkg_data` and `$pkg_install` are sandbox-aligned
/// (kernel-managed); anything else (`$home`, absolute paths) is sensitive.
pub(crate) fn fs_write_is_sensitive(raw: &str) -> bool {
    !raw.starts_with("$pkg_data") && !raw.starts_with("$pkg_install")
}

/// Compute the sensitive-perms summary for a manifest. Only the fields that
/// actually triggered the trust requirement get populated; everything else
/// is empty.
pub fn summarize_sensitive(perms: &Permissions) -> PermsSummary {
    let fs_write_outside_sandbox = perms
        .fs_write
        .iter()
        .filter(|raw| fs_write_is_sensitive(raw))
        .cloned()
        .collect::<Vec<_>>();
    PermsSummary {
        shell_execute: perms.shell_execute.clone(),
        fs_write_outside_sandbox,
        net: perms.net.clone(),
        vault_keys: perms.vault_keys.clone(),
    }
}
