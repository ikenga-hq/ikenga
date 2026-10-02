//! Secret-name scopes: `workspace::<key>`, `project::<id>::<key>`,
//! `pkg::<id>::<key>`, and the legacy bare names.
//!
//! Extracted from `commands/secrets.rs` (WP-21, core extraction only) so the
//! headless daemon's per-principal store addresses names exactly as the
//! desktop keychain does; `commands::secrets` re-exports every public item,
//! so desktop call sites are unchanged.

use serde::{Deserialize, Serialize};

use super::index::{validate_key, validate_legacy_name, validate_scope_id};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Workspace-level — intentionally cross-project (rare, e.g. shared
    /// connector tokens).
    Workspace,
    /// Project-level — defaults for new secrets are this scope with the
    /// active project's id.
    Project { id: String },
    /// Pkg-level — pkg-supplied capability resolvers default here, with
    /// the pkg's own id.
    Pkg { id: String },
}

impl Scope {
    pub fn project(id: impl Into<String>) -> Self {
        Self::Project { id: id.into() }
    }

    pub fn pkg(id: impl Into<String>) -> Self {
        Self::Pkg { id: id.into() }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Workspace => Ok(()),
            Self::Project { id } | Self::Pkg { id } => validate_scope_id(id),
        }
    }
}

pub fn checked_vault_key(scope: &Scope, key: &str) -> Result<String, String> {
    scope.validate()?;
    validate_key(key)?;
    Ok(vault_key(scope, key))
}

pub fn vault_key(scope: &Scope, key: &str) -> String {
    match scope {
        Scope::Workspace => format!("workspace::{key}"),
        Scope::Project { id } => format!("project::{id}::{key}"),
        Scope::Pkg { id } => format!("pkg::{id}::{key}"),
    }
}

/// Parse a fully-qualified vault entry back into `(scope, key)`. Returns
/// `None` for legacy unscoped entries (no `::` prefix matching a known
/// scope). Used by the Settings UI and the dump-resolver to walk the
/// namespace without re-parsing strings repeatedly.
///
/// Deliberately the permissive WP-33 split — `project::<id>::<key>` is split
/// at the first `::` after the prefix, and the id and key only need to be
/// non-empty — so every scoped name an earlier build could write (e.g.
/// `project::other::a::b`, or an id outside today's charset) still
/// classifies under its scope. The strict WP-34 charset rules apply only to
/// new writes (`checked_vault_key`, `scoped_set_locked`).
pub fn parse_scoped(fqk: &str) -> Option<(Scope, String)> {
    if let Some(rest) = fqk.strip_prefix("workspace::") {
        if rest.is_empty() {
            return None;
        }
        return Some((Scope::Workspace, rest.to_string()));
    }
    if let Some(rest) = fqk.strip_prefix("project::") {
        let (id, key) = rest.split_once("::")?;
        if id.is_empty() || key.is_empty() {
            return None;
        }
        return Some((Scope::project(id), key.to_string()));
    }
    if let Some(rest) = fqk.strip_prefix("pkg::") {
        let (id, key) = rest.split_once("::")?;
        if id.is_empty() || key.is_empty() {
            return None;
        }
        return Some((Scope::pkg(id), key.to_string()));
    }
    None
}

/// `true` when `name` starts with a scope prefix, whether or not the rest
/// classifies under [`parse_scoped`].
#[cfg_attr(not(feature = "desktop"), allow(dead_code))] // commands::secrets only
pub(crate) fn has_scope_prefix(name: &str) -> bool {
    ["workspace::", "project::", "pkg::"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Full vault name of an EXISTING scoped entry, for read / delete / list.
/// Permissive (WP-33) rules so legacy entries stay reachable, with one hard
/// requirement: the name must classify back to exactly `(scope, key)`, so a
/// scope id or key containing `::` can never address another scope's entry.
pub(crate) fn existing_scoped_name(scope: &Scope, key: &str) -> Result<String, String> {
    validate_legacy_name(key)?;
    let name = vault_key(scope, key);
    validate_legacy_name(&name)?;
    match parse_scoped(&name) {
        Some((parsed_scope, parsed_key)) if &parsed_scope == scope && parsed_key == key => Ok(name),
        _ => Err("invalid scoped secret name".into()),
    }
}

/// Permissive scope check for reading or listing existing entries.
pub(crate) fn validate_existing_scope(scope: &Scope) -> Result<(), String> {
    existing_scoped_name(scope, "_").map(|_| ())
}

/// Bare (unscoped) address of an EXISTING entry, for the unscoped read and
/// delete commands: any legacy name that does not classify as scoped —
/// exactly the names `secrets_list_keys` shows. That includes a
/// scope-prefixed legacy name that no longer parses (e.g. `project::onlyid`),
/// whose only handle this is. A name that does classify as scoped is refused
/// so the unscoped commands cannot reach into a scope.
pub(crate) fn validate_existing_bare_name(name: &str) -> Result<(), String> {
    validate_legacy_name(name)?;
    if parse_scoped(name).is_some() {
        return Err("secret key contains a scope delimiter".into());
    }
    Ok(())
}
