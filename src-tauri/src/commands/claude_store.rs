//! Ngwa central store (Ọba) + symlink farm — WP-02.
//!
//! Implements the file-based half of the G-CONTRACT store layer (see the
//! frozen TS signatures in `src/lib/tauri-cmd.ts`). The store is a central
//! catalog of canonical primitives at `<app_data_dir>/store/{agents,skills,
//! commands}/<name>`; "enabling" a primitive in a scope creates a **symlink**
//! in that scope's `.claude/<kind>s/` dir pointing into the store. Disabling
//! drops only the link, leaving the canonical store copy intact. Copy/move
//! relocate the resolved primitive across scopes; remove deletes the
//! scope-local entry (link or real file) without touching the store.
//!
//! Three primitive shapes share the same store/farm machinery:
//!   - **agent**   → `<kind-dir>/<name>.md`        (single file)
//!   - **command** → `<kind-dir>/<name>.md`        (single file)
//!   - **skill**   → `<kind-dir>/<name>/`          (directory, holds SKILL.md)
//!
//! `hook` and `mcp` are JSON-fragment primitives toggled by a separate merge
//! engine (WP-03, `claude_store/merge.rs`). They are intentionally *not*
//! implemented here — every mutation routes them through a single, clearly
//! marked `ORCHESTRATOR-WIRE` match arm that errors for now and is the wiring
//! point for WP-03's merge API at integration.
//!
//! ## Atomicity + path-confinement
//! Writes that materialize a primitive (import copy, copy/move across scopes)
//! go through a temp path + atomic rename so an interrupted write never leaves
//! a half-written canonical file. Every mutation validates that its on-disk
//! target sits under a `.claude/` dir or the store root
//! (`claude_config::is_under_claude_or_store`) before touching the FS.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::claude_config::{
    clear_pin_for, expand, is_in_store, is_under_claude_or_store, mtime_ms, parse_md, repoint_pin,
    store_root, string_field, validate_pin_scope,
};
use crate::commands::db::PaDb;
use crate::commands::engine_layout::{
    engine_layout_by_id, ConfigFormat, EngineId, KindLayout, Mechanism, PrimitiveKind,
};
use crate::commands::projects::get_project;
// `requires` is declared on the pkg manifest (pkg::manifest) and carried,
// compiled, on the store registry entry. Reuse the one canonical struct so the
// manifest and registry shapes can never drift (ADR-015 §3a / WP-11).
use crate::pkg::manifest::RequiresEntry;

/// WP-03 merge engine — JSON-fragment (hook/mcp) splice into Claude Code's
/// settings files. `claude_store/merge.rs` (file sits in the same-named subdir
/// next to this file). We call its pure, synchronous API; the async
/// `project:<id>` → `root_path` resolution stays here (`resolve_scope_root`).
mod merge;

/// WP-22 TOML merge engine — the Codex sibling of `merge.rs`'s JSON path, kept
/// **disjoint** from it. Splices `config.toml` `[mcp_servers.<name>]` / inline
/// `[hooks]` blocks format-preservingly. `merge.rs`'s engine-aware dispatch
/// (`enable_mcp_for` / `enable_hook_for`) routes to it by `ConfigFormat::Toml`.
mod toml_merge;

/// WP-02 Ọba registry index I/O — `store/registry.json` load/save/back-fill +
/// the provenance overlay `claude_store_list` applies. Provenance is stored;
/// dependents are computed live (WP-04), so the index is non-fatal if lost.
mod registry;

/// Ọba Phase 2 (WP-07/08/09) — install-from-git / install-from-npx + check-update
/// / update. Fetches a managed canonical into the vault (`store/<kind>s/<name>`,
/// in-place uniform) and records provenance; placement stays the separate
/// `claude_primitive_enable` step. Reuses this module's `atomic_copy_*` as the
/// atomic swap and the `registry` I/O above.
mod install;

pub use install::{
    oba_auto_update_all, oba_check_update, oba_install_bundle, oba_install_git, oba_install_local,
    oba_install_npx, oba_install_with_deps, oba_missing_requires, oba_set_auto_update, oba_update,
    resolve_pkg_requires, CatalogEntryRef, PkgRequiresResult,
};

/// Forward-dependency resolver core (Ọba Phase 4, ADR-015 §3b / WP-13). Pure
/// closure/topo/cycle over an injected `requires` graph + satisfied set; the
/// disk helper unions the store registry with external masters. Wired into
/// install/enable in WP-14.
mod resolve;

// ─── Wire types (mirror the frozen G-CONTRACT) ───────────────────────────────

/// Origin of a primitive's canonical master. Frozen part of `G-SCHEMA` (Ọba
/// registry). Mirrors the `source` union on `ClaudeStoreEntry` in `tauri-cmd.ts`.
/// Defaults to `Local` so a pre-registry entry deserializes as a plain copy-vault
/// entry (back-compat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProvenanceSource {
    /// A plain copy-vault entry — today's default; no remote.
    #[default]
    Local,
    /// Cloned from a git remote (`url` + resolved `version` = commit SHA).
    Git,
    /// Installed via npx/npm (`url` = spec, `version` = resolved version).
    Npx,
    /// Deprecated discovery-origin variant. **Never constructed** — catalog
    /// discovery is now recorded orthogonally via `RegistryProvenance.from_catalog`
    /// so the resolved fetch mechanism (`Git` vs `Npx`) stays the dispatch axis
    /// (an npx-backed catalog entry must `npx skills add`, not `git clone`). Kept
    /// in the enum for wire/schema back-compat: an old `registry.json` that wrote
    /// `"source":"catalog"` still deserializes. Treated like `Git` on the
    /// fetch/check dispatch paths.
    Catalog,
}

fn default_true() -> bool {
    true
}

/// Provenance for a store entry: where its canonical master came from + whether
/// the shell owns its lifecycle. The new information a copy-vault lacks; the
/// freeze gate `G-SCHEMA` (see `plans/oba-registry/drafts/registry-schema.md`).
///
/// **Dependents are deliberately NOT stored here** — they are computed live from
/// the filesystem by the scanner (a stored list can drift; the symlink graph is
/// truth), so the safe-delete guard works even if `registry.json` is lost.
///
/// Flattened onto `ClaudeStoreEntry` so the wire JSON carries the fields inline.
/// Every field has a serde default, so an entry serialized before the registry
/// existed deserializes as a synthesized `local`, shell-managed entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryProvenance {
    /// Origin of the canonical master.
    #[serde(default)]
    pub source: ProvenanceSource,
    /// git remote URL | npm spec | catalog id; `None` for local.
    #[serde(default)]
    pub url: Option<String>,
    /// Requested git tag/branch (resolves to `version`); `None` otherwise.
    #[serde(rename = "ref", default)]
    pub r#ref: Option<String>,
    /// Resolved: git commit SHA | npm version; `None` for local.
    #[serde(default)]
    pub version: Option<String>,
    /// Absolute path to the real master — may be in-vault OR external/in-place.
    /// Empty only on a legacy entry deserialized without provenance; the registry
    /// load normalizes empty → the store path (WP-02).
    #[serde(rename = "canonicalPath", default)]
    pub canonical_path: String,
    /// `true` = master lives in the Ọba vault and the shell owns its lifecycle
    /// (deletable). `false` = external master kept in place — NEVER
    /// `remove_dir_all`'d by the safe-delete guard.
    #[serde(default = "default_true")]
    pub managed: bool,
    #[serde(rename = "installedAt", default)]
    pub installed_at: Option<String>,
    #[serde(rename = "updatedAt", default)]
    pub updated_at: Option<String>,
    /// `true` = the primitive was discovered through the recommended Ọba catalog
    /// (Phase 3). Orthogonal to `source`, which records the resolved fetch
    /// mechanism (`git`/`npx`). A direct (non-catalog) git/npx install leaves this
    /// `false`. `#[serde(default)]` → an entry serialized before Phase 3
    /// deserializes as `false` (back-compat).
    #[serde(rename = "fromCatalog", default)]
    pub from_catalog: bool,
    /// `true` = the shell may auto-update this master on the catalog surface
    /// mount. Curated-catalog installs set this `true`; plain git/npx/local
    /// installs leave it `false` (mirrors Claude Code marketplaces: catalog ON,
    /// local/dev/manual OFF). `#[serde(default)]` → pre-Phase-3 entries read
    /// `false` (back-compat).
    #[serde(rename = "autoUpdate", default)]
    pub auto_update: bool,
}

impl RegistryProvenance {
    /// Synthesized provenance for a plain copy-vault entry (today's default): the
    /// master is the store copy, shell-managed, no remote. Used at every
    /// in-code construction site until WP-02's registry load supplies real
    /// provenance from `store/registry.json`.
    pub fn local(canonical_path: String) -> Self {
        Self {
            source: ProvenanceSource::Local,
            url: None,
            r#ref: None,
            version: None,
            canonical_path,
            managed: true,
            installed_at: None,
            updated_at: None,
            from_catalog: false,
            auto_update: false,
        }
    }
}

/// A single catalog entry in the central store (Ọba). Mirrors
/// `ClaudeStoreEntry` in `tauri-cmd.ts`. Carries `RegistryProvenance` flattened
/// inline (`G-SCHEMA`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeStoreEntry {
    pub kind: String,
    pub name: String,
    #[serde(rename = "storePath")]
    pub store_path: String,
    pub description: Option<String>,
    #[serde(rename = "modifiedMs")]
    pub modified_ms: i64,
    #[serde(rename = "enabledIn")]
    pub enabled_in: Vec<String>,
    /// Forward dependencies (ADR-015 §3 / WP-11). The compiled `requires` list —
    /// each entry names a standalone primitive this one depends on; the resolver
    /// (WP-13/14) installs the closure. A SIBLING of `provenance` on purpose:
    /// provenance is strictly *origin* (where the master came from), `requires`
    /// is *what it needs* — two separate graphs (the reverse-dependents graph is
    /// also kept out of provenance, computed live by the scanner). Empty by
    /// default (`#[serde(default)]`) so a pre-Phase-4 entry deserializes with no
    /// deps. Mirrors `requires?` on the `ClaudeStoreEntry` TS interface.
    #[serde(default)]
    pub requires: Vec<RequiresEntry>,
    /// Member skills shipped by a `bundle` (WP-18). A bundle is a directory
    /// primitive (`store/bundles/<name>/`) holding N member skill subdirs; this
    /// list names the members the install placed. A SIBLING of `requires` with
    /// identical serde attrs: empty by default (`#[serde(default)]`) so every
    /// non-bundle entry (and any pre-WP-18 bundle JSON without `members`) still
    /// deserializes unchanged. Populated by the bundle installer (WP-19); WP-18
    /// only freezes the field + back-compat. Mirrors `members?` on the
    /// `ClaudeStoreEntry` TS interface.
    #[serde(default)]
    pub members: Vec<String>,
    /// Provenance (`G-SCHEMA`), flattened inline on the wire. Defaults to a
    /// synthesized `local` entry when absent (back-compat).
    #[serde(flatten)]
    pub provenance: RegistryProvenance,
}

/// On-disk shape of the registry index `store/registry.json` (`G-SCHEMA`). The
/// single JSON sidecar that turns the copy-vault into an index. Load/save +
/// back-fill land in WP-02 (`claude_store/registry.rs`); the shape is frozen
/// here so consumers can bind to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistryFile {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub entries: Vec<ClaudeStoreEntry>,
}

impl Default for RegistryFile {
    fn default() -> Self {
        Self {
            // v2 (WP-11, ADR-015): additive `requires` on each entry. The bump is
            // back-compatible — the loader does not gate on the value, and every
            // new field is `#[serde(default)]`, so a v1 `registry.json` (no
            // `requires`) still deserializes unchanged. A freshly-created registry
            // is written at the current version. WP-18 did NOT bump the version:
            // the new `members` field is likewise additive + `#[serde(default)]`,
            // so a v2 registry without `members` still deserializes unchanged.
            schema_version: 2,
            entries: Vec::new(),
        }
    }
}

/// Result of a symlink-farm mutation. Mirrors `ClaudeStoreMutation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaudeStoreMutation {
    pub kind: String,
    pub name: String,
    pub scope: String,
    pub path: String,
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
}

// ─── Kind classification ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Agent,
    Skill,
    Command,
    /// JSON-fragment primitives handled by the WP-03 merge engine.
    Hook,
    Mcp,
    /// A package shipping N member skills (WP-18). Canonical lives at a DIRECTORY
    /// `store/bundles/<name>/` holding member skill subdirs. A bundle is INVISIBLE
    /// to engines — only its member skills get placed (WP-21) — so it is never
    /// directly enabled/symlinked into a scope. Install/resolve/placement land in
    /// WP-19/20/21; WP-18 freezes the schema (enum, kind-string, store path,
    /// `members[]`) only.
    Bundle,
}

impl Kind {
    fn parse(s: &str) -> Result<Kind, String> {
        match s {
            "agent" => Ok(Kind::Agent),
            "skill" => Ok(Kind::Skill),
            "command" => Ok(Kind::Command),
            "hook" => Ok(Kind::Hook),
            "mcp" => Ok(Kind::Mcp),
            "bundle" => Ok(Kind::Bundle),
            _ => Err(format!(
                "kind must be one of skill|agent|command|hook|mcp|bundle, got {s:?}"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Skill => "skill",
            Kind::Command => "command",
            Kind::Hook => "hook",
            Kind::Mcp => "mcp",
            Kind::Bundle => "bundle",
        }
    }

    /// The `<kind>s/` subdir name shared by the store layout and the `.claude/`
    /// farm layout (e.g. `agents`, `skills`, `commands`).
    fn dir_name(self) -> Option<&'static str> {
        match self {
            Kind::Agent => Some("agents"),
            Kind::Skill => Some("skills"),
            Kind::Command => Some("commands"),
            Kind::Bundle => Some("bundles"),
            Kind::Hook | Kind::Mcp => None,
        }
    }

    /// File-based primitives flow through the symlink farm; JSON-fragment
    /// primitives (hook/mcp) flow through the WP-03 merge engine. A `bundle` is
    /// NOT file-based in this sense — it is never symlinked into a scope as
    /// itself; only its member skills are placed (WP-21). So it routes away from
    /// the symlink farm and the per-engine transcode machinery via this `false`.
    fn is_file_based(self) -> bool {
        matches!(self, Kind::Agent | Kind::Skill | Kind::Command)
    }

    /// A directory primitive's canonical store copy is a directory (not a single
    /// `.md` file): a `skill` (`store/skills/<name>/`) or a `bundle`
    /// (`store/bundles/<name>/`, holding member skill subdirs — WP-18).
    /// Agents/commands are single `.md` files.
    fn is_dir_primitive(self) -> bool {
        matches!(self, Kind::Skill | Kind::Bundle)
    }
}

/// Validate a primitive `name` — no path separators, no `..`, no leading dot,
/// bounded length. Names map directly onto on-disk path segments so this is a
/// load-bearing confinement check, not cosmetic.
fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 256 {
        return Err(format!("invalid name length: {} (1..=256)", name.len()));
    }
    if name.starts_with('.') {
        return Err(format!("name must not start with a dot: {name:?}"));
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!("name must not contain path separators: {name:?}"));
    }
    if name == "." || name == ".." || name.split(['/', '\\']).any(|seg| seg == "..") {
        return Err(format!("name must not traverse parents: {name:?}"));
    }
    Ok(())
}

// ─── Pure path math (testable without env / DB) ───────────────────────────────

/// On-disk path of a primitive's canonical copy inside the store root.
/// `agent`/`command` → `<store>/<kind>s/<name>.md`; `skill` →
/// `<store>/skills/<name>` (a directory).
fn store_path_for(store: &Path, kind: Kind, name: &str) -> Result<PathBuf, String> {
    let dir = kind
        .dir_name()
        .ok_or_else(|| format!("kind {} is not file-based", kind.as_str()))?;
    let leaf = if kind.is_dir_primitive() {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    Ok(store.join(dir).join(leaf))
}

/// On-disk path of a primitive inside a scope's `.claude/` farm dir.
/// `scope_claude` is the `<scope>/.claude` directory.
fn scope_path_for(scope_claude: &Path, kind: Kind, name: &str) -> Result<PathBuf, String> {
    // A bundle is invisible to engines: it is never placed into a scope as
    // itself — only its member skills are placed (WP-21). So there is no
    // legitimate `<scope>/.claude/bundles/<name>` path. Error descriptively
    // rather than hand back a bogus location.
    if kind == Kind::Bundle {
        return Err(
            "bundles place their members, not the bundle itself — see WP-21".to_string(),
        );
    }
    let dir = kind
        .dir_name()
        .ok_or_else(|| format!("kind {} is not file-based", kind.as_str()))?;
    let leaf = if kind.is_dir_primitive() {
        name.to_string()
    } else {
        format!("{name}.md")
    };
    Ok(scope_claude.join(dir).join(leaf))
}

// ─── Atomic FS primitives ─────────────────────────────────────────────────────

/// Atomically copy a file: write to a sibling temp path, fsync, then rename
/// over the destination. A crash mid-copy leaves only the temp file (cleaned
/// up on the next run by being uniquely named), never a half-written dest.
fn atomic_copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = temp_sibling(dst);
    let bytes = std::fs::read(src).map_err(|e| format!("read {}: {e}", src.display()))?;
    write_then_sync(&tmp, &bytes)?;
    std::fs::rename(&tmp, dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {} -> {}: {e}", tmp.display(), dst.display())
    })
}

/// Atomically copy a directory tree (skills). Materialize the whole tree under
/// a sibling temp dir, then a single `rename` swaps it into place — so a crash
/// mid-copy never exposes a partial skill dir at the destination.
fn atomic_copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = temp_sibling(dst);
    // Clear any stale temp from a prior interrupted run.
    let _ = std::fs::remove_dir_all(&tmp);
    copy_dir_recursive(src, &tmp).map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        e
    })?;
    // If a dest already exists, swap it out of the way then drop it — keeps the
    // window where neither exists as small as a rename.
    if dst.exists() {
        let old = temp_sibling(dst);
        std::fs::rename(dst, &old).map_err(|e| format!("stage-out {}: {e}", dst.display()))?;
        let res = std::fs::rename(&tmp, dst);
        match res {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&old);
                Ok(())
            }
            Err(e) => {
                // Roll the original back.
                let _ = std::fs::rename(&old, dst);
                let _ = std::fs::remove_dir_all(&tmp);
                Err(format!(
                    "rename {} -> {}: {e}",
                    tmp.display(),
                    dst.display()
                ))
            }
        }
    } else {
        std::fs::rename(&tmp, dst).map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp);
            format!("rename {} -> {}: {e}", tmp.display(), dst.display())
        })
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    let rd = std::fs::read_dir(src).map_err(|e| format!("read_dir {}: {e}", src.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("dir entry: {e}"))?;
        let ft = entry.file_type().map_err(|e| format!("file_type: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            // Resolve symlinks into real bytes so the store copy is canonical.
            let bytes =
                std::fs::read(&from).map_err(|e| format!("read {}: {e}", from.display()))?;
            write_then_sync(&to, &bytes)?;
        }
    }
    Ok(())
}

fn write_then_sync(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut f =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    f.sync_all()
        .map_err(|e| format!("fsync {}: {e}", path.display()))?;
    Ok(())
}

/// A uniquely-named sibling temp path for atomic rename staging.
fn temp_sibling(dst: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "tmp".to_string());
    let parent = dst.parent().map(Path::to_path_buf).unwrap_or_default();
    parent.join(format!(".{name}.ngwa-tmp.{nonce}"))
}

/// Remove a scope-local primitive (file, dir, or symlink). For a symlink we
/// only drop the link — `remove_file`/`remove_dir_all` both delete the link
/// node itself, never following into the store target. Idempotent on absence.
fn remove_primitive(path: &Path, kind: Kind) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("lstat {}: {e}", path.display())),
    };
    if meta.file_type().is_symlink() {
        // A symlink node is removed with remove_file on every platform,
        // including when it points at a directory — this never recurses into
        // the store target.
        std::fs::remove_file(path).map_err(|e| format!("unlink {}: {e}", path.display()))
    } else if kind.is_dir_primitive() {
        std::fs::remove_dir_all(path).map_err(|e| format!("rmdir {}: {e}", path.display()))
    } else {
        std::fs::remove_file(path).map_err(|e| format!("rm {}: {e}", path.display()))
    }
}

// ─── WP-04 — dependent-aware safe delete (the incident guardrail) ─────────────

/// The pure safe-delete decision. No filesystem access — the three inputs that
/// determine whether a delete is safe, so the policy is unit-tested directly.
/// This is the rule that makes the `groundwork` incident impossible to repeat:
/// a real master is never `remove_dir_all`'d when it is external or has live
/// dependents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteVerdict {
    /// `path` is a symlink — unlinking one placement is always safe (`remove_file`
    /// never follows into the master).
    UnlinkPlacement,
    /// `path` is a vault-managed master with zero live dependents — safe to
    /// hard-delete.
    HardDelete,
    /// `path` is an external master (`managed:false`) — kept in place; the shell
    /// never deletes it. Refuse.
    RefuseExternal,
    /// `path` is a master with live dependents — refuse and offer relink.
    RefuseHasDependents,
}

/// Decide the verdict from the three load-bearing facts. Order matters: a
/// symlink is always just a placement; only a *real* master can be refused.
pub(crate) fn delete_verdict(
    is_symlink: bool,
    managed: bool,
    dependent_count: usize,
) -> DeleteVerdict {
    if is_symlink {
        return DeleteVerdict::UnlinkPlacement;
    }
    if !managed {
        return DeleteVerdict::RefuseExternal;
    }
    if dependent_count > 0 {
        return DeleteVerdict::RefuseHasDependents;
    }
    DeleteVerdict::HardDelete
}

/// Outcome of a guarded delete, surfaced to the FE so it can render the inline
/// safe-delete guard (D-01). On a refusal, `dependents` carries the live
/// dependent paths the relink chooser offers to re-point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SafeDeleteOutcome {
    /// `unlinked` | `deleted` | `refused_external` | `refused_dependents`.
    pub verdict: String,
    pub removed: bool,
    pub dependents: Vec<String>,
    pub message: String,
}

/// Perform a guarded delete of `path` (a placement symlink OR a real master).
/// `managed` + `dependents` come from the registry + the live dependents scan.
/// NEVER reaches `remove_dir_all` on a real master that is external or has
/// dependents — the verdict gates that. Idempotent on a missing path.
pub(crate) fn guarded_delete(
    path: &Path,
    kind: Kind,
    managed: bool,
    dependents: &[PathBuf],
) -> Result<SafeDeleteOutcome, String> {
    let is_symlink = match std::fs::symlink_metadata(path) {
        Ok(m) => m.file_type().is_symlink(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SafeDeleteOutcome {
                verdict: "deleted".into(),
                removed: false,
                dependents: Vec::new(),
                message: "already absent".into(),
            });
        }
        Err(e) => return Err(format!("lstat {}: {e}", path.display())),
    };
    match delete_verdict(is_symlink, managed, dependents.len()) {
        DeleteVerdict::UnlinkPlacement => {
            remove_primitive(path, kind)?;
            Ok(SafeDeleteOutcome {
                verdict: "unlinked".into(),
                removed: true,
                dependents: Vec::new(),
                message: "removed one placement; master untouched".into(),
            })
        }
        DeleteVerdict::HardDelete => {
            remove_primitive(path, kind)?;
            Ok(SafeDeleteOutcome {
                verdict: "deleted".into(),
                removed: true,
                dependents: Vec::new(),
                message: "deleted vault-managed master (no dependents)".into(),
            })
        }
        DeleteVerdict::RefuseExternal => Ok(SafeDeleteOutcome {
            verdict: "refused_external".into(),
            removed: false,
            dependents: dependents.iter().map(|p| p.display().to_string()).collect(),
            message: "external master kept in place — never deleted here".into(),
        }),
        DeleteVerdict::RefuseHasDependents => Ok(SafeDeleteOutcome {
            verdict: "refused_dependents".into(),
            removed: false,
            dependents: dependents.iter().map(|p| p.display().to_string()).collect(),
            message: format!(
                "{} live dependent(s) resolve into this master — relink them first",
                dependents.len()
            ),
        }),
    }
}

/// Atomically re-point a dependent symlink at a new master (temp link + rename
/// over the existing link). Used by relink-all before forgetting an external
/// master. Refuses to touch a non-symlink (we never overwrite real files).
pub(crate) fn relink_one(link: &Path, new_target: &Path) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(link).map_err(|e| format!("lstat {}: {e}", link.display()))?;
    if !meta.file_type().is_symlink() {
        return Err(format!(
            "refusing to relink {}: not a symlink (would overwrite a real path)",
            link.display()
        ));
    }
    make_symlink(new_target, link)
}

/// Create a symlink at `link` pointing to `target`, replacing any existing
/// node atomically (build the link at a temp path, then rename over).
fn make_symlink(target: &Path, link: &Path) -> Result<(), String> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let tmp = temp_sibling(link);
    let _ = std::fs::remove_file(&tmp);
    symlink_impl(target, &tmp)?;
    std::fs::rename(&tmp, link).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename link {} -> {}: {e}", tmp.display(), link.display())
    })
}

#[cfg(unix)]
fn symlink_impl(target: &Path, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("symlink {} -> {}: {e}", link.display(), target.display()))
}

#[cfg(windows)]
fn symlink_impl(target: &Path, link: &Path) -> Result<(), String> {
    // On Windows, file vs dir symlinks use different syscalls.
    let res = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    res.map_err(|e| format!("symlink {} -> {}: {e}", link.display(), target.display()))
}

// ─── Description extraction ───────────────────────────────────────────────────

/// Lift `description` from a primitive's frontmatter. For agents/commands the
/// `.md` is the file itself; for skills it's `<dir>/SKILL.md`.
fn read_description(store_entry: &Path, kind: Kind) -> Option<String> {
    let md = if kind.is_dir_primitive() {
        store_entry.join("SKILL.md")
    } else {
        store_entry.to_path_buf()
    };
    let (fm, _body) = parse_md(&md).ok()?;
    string_field(&fm, "description")
}

// ─── Catalog enumeration ──────────────────────────────────────────────────────

/// Walk the store catalog for a single kind, building one entry per canonical
/// primitive. `enabled_in` is left empty here — the command layer fills it by
/// probing each known scope's farm (see `claude_store_list`).
fn list_store_kind(store: &Path, kind: Kind) -> Vec<ClaudeStoreEntry> {
    let Some(dir) = kind.dir_name() else {
        return Vec::new();
    };
    let kind_dir = store.join(dir);
    let Ok(rd) = std::fs::read_dir(&kind_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        let (name, canonical) = if kind.is_dir_primitive() {
            if !ft.is_dir() {
                continue;
            }
            (
                entry.file_name().to_string_lossy().to_string(),
                path.clone(),
            )
        } else {
            if ft.is_dir() {
                continue;
            }
            let fname = entry.file_name().to_string_lossy().to_string();
            let Some(stem) = fname.strip_suffix(".md") else {
                continue;
            };
            (stem.to_string(), path.clone())
        };
        if name.starts_with('.') {
            continue;
        }
        let store_path = canonical.to_string_lossy().to_string();
        out.push(ClaudeStoreEntry {
            kind: kind.as_str().to_string(),
            name,
            store_path: store_path.clone(),
            description: read_description(&canonical, kind),
            modified_ms: mtime_ms(&canonical),
            enabled_in: Vec::new(),
            // Synthesized empty; WP-02's registry load overlays the recorded
            // `requires` (and real provenance) from store/registry.json.
            requires: Vec::new(),
            // No bundle members on a file-walked entry (only the bundle installer
            // records members — WP-19). Empty for all current file-based kinds.
            members: Vec::new(),
            // Synthesized local provenance; WP-02's registry load overlays real
            // provenance from store/registry.json where present.
            provenance: RegistryProvenance::local(store_path),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// True iff `name` of `kind` is currently enabled (symlinked) in the scope
/// whose `.claude` dir is `scope_claude`, AND the link resolves into the store.
fn is_enabled_in(scope_claude: &Path, store: &Path, kind: Kind, name: &str) -> bool {
    let Ok(p) = scope_path_for(scope_claude, kind, name) else {
        return false;
    };
    // Must exist as a symlink pointing into the store.
    let Ok(meta) = std::fs::symlink_metadata(&p) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    match std::fs::canonicalize(&p) {
        Ok(resolved) => is_in_store(&resolved, store),
        Err(_) => false,
    }
}

// ─── Scope resolution (command layer) ─────────────────────────────────────────

/// Resolve a `ClaudeStoreScope` string to that scope's `.claude` directory.
/// `workspace` → `~/.claude`; `project:<id>` → `<project.root_path>/.claude`.
async fn resolve_scope_claude(db: &Arc<PaDb>, scope: &str) -> Result<PathBuf, String> {
    validate_pin_scope(scope)?;
    if scope == "workspace" {
        let home = std::env::var_os("HOME").ok_or_else(|| "HOME not set".to_string())?;
        return Ok(PathBuf::from(home).join(".claude"));
    }
    let id = scope
        .strip_prefix("project:")
        .ok_or_else(|| format!("unexpected scope {scope:?}"))?;
    let pool = db.ensure_pool().await?;
    let project = get_project(&pool, id)
        .await?
        .ok_or_else(|| format!("no project with id {id:?}"))?;
    let root = project
        .root_path
        .ok_or_else(|| format!("project {id:?} has no root_path"))?;
    let root = expand(&root).map_err(|e| e.to_string())?;
    Ok(root.join(".claude"))
}

/// Resolve a `ClaudeStoreScope` string to the `project_root` the WP-03 merge
/// engine expects: `None` for `workspace` (merge then targets `~/.claude/...`
/// and `~/.claude.json`), or `Some(<project.root_path>)` for `project:<id>`.
///
/// This is the JSON-fragment sibling of [`resolve_scope_claude`]: same scope
/// grammar (`validate_pin_scope`), same DB lookup (`get_project`), same
/// `~`-expansion — it just stops one level up (the project root itself, the
/// parent of `.claude`) because `merge` owns the `.claude` / `.mcp.json` /
/// `.claude.json` suffix per scope+kind. We do **not** invent a second
/// resolver: this funnels through the identical `get_project` path
/// `resolve_scope_claude` uses, just returning the root instead of `root/.claude`.
async fn resolve_scope_root(db: &Arc<PaDb>, scope: &str) -> Result<Option<PathBuf>, String> {
    validate_pin_scope(scope)?;
    if scope == "workspace" {
        return Ok(None);
    }
    let id = scope
        .strip_prefix("project:")
        .ok_or_else(|| format!("unexpected scope {scope:?}"))?;
    let pool = db.ensure_pool().await?;
    let project = get_project(&pool, id)
        .await?
        .ok_or_else(|| format!("no project with id {id:?}"))?;
    let root = project
        .root_path
        .ok_or_else(|| format!("project {id:?} has no root_path"))?;
    let root = expand(&root).map_err(|e| e.to_string())?;
    Ok(Some(root))
}

// ─── JSON-fragment (hook/mcp) store fragments ─────────────────────────────────
//
// The store keeps hook/mcp primitives as JSON fragments alongside the
// file-based ones, under `<store>/{hooks,mcp}/<name>.json`. "Enabling" a
// fragment in a scope splices its block into that scope's settings file via
// the WP-03 merge engine; it never symlinks (JSON primitives aren't files in a
// farm). The on-disk fragment is the catalog entry — scope toggles read it but
// never delete it (deleting the catalog entry is a separate store op).

/// Hook fragment schema (`<store>/hooks/<name>.json`). Carries everything
/// `merge::enable_hook` needs beyond the scope:
///
/// ```json
/// {
///   "event": "PreToolUse",        // the hooks.<event> key the block lands at
///   "file": "shared",             // "shared" → settings.json | "local" → settings.local.json
///   "block": [                    // value placed at hooks.<event> (Claude Code's
///     { "matcher": "Bash",        //   per-event array of matcher groups)
///       "hooks": [ { "type": "command", "command": "echo hi" } ] }
///   ]
/// }
/// ```
///
/// `event` and `block` are required; `file` defaults to `shared` when absent.
#[derive(Debug, Clone, Deserialize)]
struct HookFragment {
    event: String,
    #[serde(default)]
    file: HookFileTag,
    block: serde_json::Value,
}

/// Target settings file for a hook fragment. Mirrors `merge::HookFile`;
/// kept as a local wire enum so the fragment JSON uses lowercase tags.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HookFileTag {
    #[default]
    Shared,
    Local,
}

impl HookFileTag {
    fn to_merge(self) -> merge::HookFile {
        match self {
            HookFileTag::Shared => merge::HookFile::Shared,
            HookFileTag::Local => merge::HookFile::Local,
        }
    }
}

/// On-disk path of a hook/mcp fragment in the store: `<store>/<kind-dir>/<name>.json`.
/// `kind-dir` is `hooks` for [`Kind::Hook`] and `mcp` for [`Kind::Mcp`].
fn fragment_path(store: &Path, kind: Kind, name: &str) -> Result<PathBuf, String> {
    let dir = match kind {
        Kind::Hook => "hooks",
        Kind::Mcp => "mcp",
        other => return Err(format!("kind {} has no JSON fragment", other.as_str())),
    };
    Ok(store.join(dir).join(format!("{name}.json")))
}

/// Read + parse a hook fragment from the store.
fn read_hook_fragment(store: &Path, name: &str) -> Result<HookFragment, String> {
    let path = fragment_path(store, Kind::Hook, name)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("read hook fragment {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse hook fragment {}: {e}", path.display()))
}

/// Read the MCP `server_def` from the store. For MCP the fragment file content
/// **is** the `server_def` value (the object placed at `mcpServers.<name>`).
fn read_mcp_fragment(store: &Path, name: &str) -> Result<serde_json::Value, String> {
    let path = fragment_path(store, Kind::Mcp, name)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("read mcp fragment {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse mcp fragment {}: {e}", path.display()))
}

/// The settings file a hook/mcp enable/disable actually touches, for the
/// returned `ClaudeStoreMutation.path`. Mirrors `merge`'s own path resolution
/// (kept in lockstep): hooks land in `<root|~>/.claude/settings{,.local}.json`;
/// MCP lands in `<root>/.mcp.json` (project) or `~/.claude.json` (workspace).
/// `project_root.is_some()` ⇔ a `project:<id>` scope; `None` ⇔ `workspace`.
fn fragment_target_path(
    kind: Kind,
    project_root: Option<&Path>,
    hook_file: Option<HookFileTag>,
) -> Result<PathBuf, String> {
    let home = || {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME not set".to_string())
    };
    match kind {
        Kind::Hook => {
            let leaf = match hook_file.unwrap_or_default() {
                HookFileTag::Shared => "settings.json",
                HookFileTag::Local => "settings.local.json",
            };
            let claude = match project_root {
                Some(root) => root.join(".claude"),
                None => home()?.join(".claude"),
            };
            Ok(claude.join(leaf))
        }
        Kind::Mcp => match project_root {
            Some(root) => Ok(root.join(".mcp.json")),
            None => Ok(home()?.join(".claude.json")),
        },
        other => Err(format!("kind {} has no settings target", other.as_str())),
    }
}

/// Splice a hook/mcp fragment's block into `scope`'s settings file via the
/// merge engine (enable-in-scope). Returns the settings file touched (for the
/// `ClaudeStoreMutation.path`). Shared by enable / copy / move (the dest leg).
fn enable_fragment_in_scope(
    store: &Path,
    kind: Kind,
    name: &str,
    scope: &str,
    project_root: Option<&Path>,
) -> Result<PathBuf, String> {
    match kind {
        Kind::Hook => {
            let frag = read_hook_fragment(store, name)?;
            merge::enable_hook(
                scope,
                project_root,
                frag.file.to_merge(),
                &frag.event,
                frag.block,
            )
            .map_err(|e| e.to_string())?;
            fragment_target_path(Kind::Hook, project_root, Some(frag.file))
        }
        Kind::Mcp => {
            let server_def = read_mcp_fragment(store, name)?;
            merge::enable_mcp(scope, project_root, name, server_def).map_err(|e| e.to_string())?;
            fragment_target_path(Kind::Mcp, project_root, None)
        }
        other => Err(format!("kind {} is not a JSON fragment", other.as_str())),
    }
}

/// Remove a hook/mcp fragment's spliced block from `scope`'s settings file via
/// the merge engine (disable-in-scope). For hooks the event/file are read from
/// the store fragment. Shared by disable / move (the source leg).
fn disable_fragment_in_scope(
    store: &Path,
    kind: Kind,
    name: &str,
    scope: &str,
    project_root: Option<&Path>,
) -> Result<(), String> {
    match kind {
        Kind::Hook => {
            let frag = read_hook_fragment(store, name)?;
            merge::disable_hook(scope, project_root, frag.file.to_merge(), &frag.event)
                .map_err(|e| e.to_string())
        }
        Kind::Mcp => merge::disable_mcp(scope, project_root, name).map_err(|e| e.to_string()),
        other => Err(format!("kind {} is not a JSON fragment", other.as_str())),
    }
}

/// Engine-aware sibling of [`enable_fragment_in_scope`] — reads the store
/// hook/mcp fragment and splices it for an ARBITRARY engine via WP-22's
/// engine-aware merge dispatch (`merge::enable_hook_for` / `enable_mcp_for`),
/// which routes JSON vs TOML by the frozen layout and runs the Gemini strict-key
/// guard before any write. The Phase-1 path above stays Claude-pinned; this is
/// the one the unified `*_for` commands call so a single command name handles
/// both file-kinds (WP-23) and settings-embedded (WP-22). Returns the settings
/// file touched (for `ClaudeStoreMutation.path`). For hooks `hook_file` overrides
/// the fragment's own `file` tag (the FE passes it explicitly per the TS
/// contract); the fragment's tag is the fallback default.
fn enable_fragment_in_scope_for(
    engine: EngineId,
    store: &Path,
    kind: Kind,
    name: &str,
    scope: &str,
    project_root: Option<&Path>,
    hook_file: merge::HookFile,
) -> Result<PathBuf, String> {
    match kind {
        Kind::Hook => {
            let frag = read_hook_fragment(store, name)?;
            merge::enable_hook_for(
                engine,
                scope,
                project_root,
                hook_file,
                &frag.event,
                frag.block,
            )
            .map_err(|e| e.to_string())?;
            merge::resolve_hook_target(engine, scope, project_root, hook_file)
                .map_err(|e| e.to_string())
        }
        Kind::Mcp => {
            let server_def = read_mcp_fragment(store, name)?;
            merge::enable_mcp_for(engine, scope, project_root, name, server_def)
                .map_err(|e| e.to_string())?;
            merge::resolve_mcp_target(engine, scope, project_root).map_err(|e| e.to_string())
        }
        other => Err(format!("kind {} is not a JSON fragment", other.as_str())),
    }
}

/// Engine-aware sibling of [`disable_fragment_in_scope`] — unsplices a hook/mcp
/// block for an arbitrary engine via WP-22's `merge::disable_hook_for` /
/// `disable_mcp_for`. For hooks the event comes from the store fragment; the
/// `hook_file` selects the target settings file (JSON engines) and is ignored by
/// Codex's single TOML/JSON target.
fn disable_fragment_in_scope_for(
    engine: EngineId,
    store: &Path,
    kind: Kind,
    name: &str,
    scope: &str,
    project_root: Option<&Path>,
    hook_file: merge::HookFile,
) -> Result<(), String> {
    match kind {
        Kind::Hook => {
            let frag = read_hook_fragment(store, name)?;
            merge::disable_hook_for(engine, scope, project_root, hook_file, &frag.event)
                .map_err(|e| e.to_string())
        }
        Kind::Mcp => {
            merge::disable_mcp_for(engine, scope, project_root, name).map_err(|e| e.to_string())
        }
        other => Err(format!("kind {} is not a JSON fragment", other.as_str())),
    }
}

/// Map the wire `hookFile` string (`"shared"` | `"local"`) to `merge::HookFile`.
/// Defaults to `Shared` (matches the TS default).
fn parse_hook_file(s: &str) -> merge::HookFile {
    match s {
        "local" => merge::HookFile::Local,
        // "shared" or anything else → shared (the TS default).
        _ => merge::HookFile::Shared,
    }
}

// ─── Core mutation logic (pure; takes resolved roots) ─────────────────────────

/// `claude_store_import` core: copy an on-disk primitive into the store as the
/// new canonical source. Atomic. Returns the resulting catalog entry.
fn import_core(
    store: &Path,
    kind: Kind,
    name: &str,
    source: &Path,
) -> Result<ClaudeStoreEntry, String> {
    // WP-18 stub: a bundle IS a dir primitive, so the generic `atomic_copy_dir`
    // below would naively copy the source dir into `store/bundles/<name>` —
    // wrong. A bundle install places its MEMBER skills, not the bundle dir
    // (WP-19). Not reachable today (the public `claude_store_import` guards
    // `!is_file_based` first), but guard here so a future caller can't inherit
    // the silent-copy footgun.
    if kind == Kind::Bundle {
        return Err(
            "bundle import not yet implemented (WP-19); a bundle places member skills, not the bundle dir"
                .to_string(),
        );
    }
    let dest = store_path_for(store, kind, name)?;
    // Confinement: the import dest must sit inside the store root we were
    // handed. `store_path_for` builds it from `store` + validated name, so the
    // only way this fails is a caller passing a bogus store root — reject it
    // rather than write outside the store. (We check against `store` directly,
    // not the global guard, because import is the one mutation that writes
    // *into* the store rather than a `.claude/` farm.)
    if !dest.starts_with(store) {
        return Err(format!("import dest outside store: {}", dest.display()));
    }
    if !source.exists() {
        return Err(format!("source does not exist: {}", source.display()));
    }
    if kind.is_dir_primitive() {
        if !source.is_dir() {
            return Err(format!("skill source is not a dir: {}", source.display()));
        }
        atomic_copy_dir(source, &dest)?;
    } else {
        if !source.is_file() {
            return Err(format!("source is not a file: {}", source.display()));
        }
        atomic_copy_file(source, &dest)?;
    }
    let store_path = dest.to_string_lossy().to_string();
    Ok(ClaudeStoreEntry {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        store_path: store_path.clone(),
        description: read_description(&dest, kind),
        modified_ms: mtime_ms(&dest),
        enabled_in: Vec::new(),
        // A fresh local import declares no forward deps; the publish-time lift
        // (WP-12) is what populates `requires` for catalog/git/npx entries.
        requires: Vec::new(),
        // A leaf import has no bundle members; only the bundle installer (WP-19)
        // records placed members.
        members: Vec::new(),
        // A fresh import is a shell-managed local copy in the vault.
        provenance: RegistryProvenance::local(store_path),
    })
}

/// **The single primitive-placement layer (WP-16 unify).** Creates the scope-farm
/// symlink `<scope>/.claude/<kind>s/<name>` → the store canonical copy. Idempotent.
/// Both the Ọba enable Tauri path (`claude_primitive_enable` → here) and the
/// pkg-kernel → Ọba `requires` seam (`install::resolve_pkg_requires` → here) route
/// through this one fn, so there is ONE materialization implementation (replacing
/// the former split between the Ọba symlink-farm and the per-engine adapters'
/// `install_*`). The per-engine adapters keep their layout knowledge and will
/// physically delegate to this fn (codex `.agents/skills/`, gemini) — that
/// adapter↔enable merge's freeze is pending live-verify; the claude path is
/// unified here now. Formerly `enable_core`.
fn place_primitive(
    store: &Path,
    scope_claude: &Path,
    scope: &str,
    kind: Kind,
    name: &str,
) -> Result<ClaudeStoreMutation, String> {
    let target = store_path_for(store, kind, name)?;
    if !target.exists() {
        return Err(format!(
            "store has no {} named {name:?} (expected {})",
            kind.as_str(),
            target.display()
        ));
    }
    let link = scope_path_for(scope_claude, kind, name)?;
    if !is_under_claude_or_store(&link) {
        return Err(format!(
            "enable target outside .claude/store: {}",
            link.display()
        ));
    }
    // Idempotent: if already a store-backed link, no-op.
    if is_enabled_in(scope_claude, store, kind, name) {
        return Ok(ClaudeStoreMutation {
            kind: kind.as_str().to_string(),
            name: name.to_string(),
            scope: scope.to_string(),
            path: link.to_string_lossy().to_string(),
            link_target: Some(target.to_string_lossy().to_string()),
        });
    }
    make_symlink(&target, &link)?;
    Ok(ClaudeStoreMutation {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        scope: scope.to_string(),
        path: link.to_string_lossy().to_string(),
        link_target: Some(target.to_string_lossy().to_string()),
    })
}

/// `claude_primitive_disable` core: drop only the scope-local link. The store
/// canonical copy is untouched. Idempotent.
fn disable_core(scope_claude: &Path, kind: Kind, name: &str) -> Result<(), String> {
    let link = scope_path_for(scope_claude, kind, name)?;
    if !is_under_claude_or_store(&link) {
        return Err(format!(
            "disable target outside .claude/store: {}",
            link.display()
        ));
    }
    remove_primitive(&link, kind)
}

// ─── WP-21 — bundle placement / removal (members, not the bundle) ─────────────

/// The member-skill leaf names of a bundle, registry-first with a disk fallback.
/// The registry record (`members[]`, recorded by the WP-19 installer) is the
/// authoritative installed set; if it is absent/empty (e.g. a hand-assembled
/// vault dir or a pre-WP-19 entry) we fall back to enumerating the canonical
/// bundle dir's subdirectories. Returns leaves sorted for deterministic order.
fn bundle_member_leaves(store: &Path, name: &str) -> Result<Vec<String>, String> {
    // Prefer the registry record's members[].
    let rf = registry::load(store);
    if let Some(e) = rf
        .entries
        .iter()
        .find(|e| e.kind == Kind::Bundle.as_str() && e.name == name)
    {
        if !e.members.is_empty() {
            let mut m = e.members.clone();
            m.sort();
            return Ok(m);
        }
    }
    // Fallback: enumerate the canonical bundle dir's member subdirs.
    let canonical = store_path_for(store, Kind::Bundle, name)?;
    let rd = match std::fs::read_dir(&canonical) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "store has no bundle named {name:?} (expected {})",
                canonical.display()
            ));
        }
        Err(e) => return Err(format!("read_dir {}: {e}", canonical.display())),
    };
    let mut leaves = Vec::new();
    for entry in rd.flatten() {
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            leaves.push(entry.file_name().to_string_lossy().to_string());
        } else {
            // A bundle member must be a directory (it holds SKILL.md). A stray
            // file here means a malformed bundle — surface it rather than
            // silently dropping it (a silent drop could leave a member's scope
            // link uncleaned). Not fatal: skip it.
            tracing::warn!(
                bundle = %name,
                entry = ?entry.file_name(),
                "bundle canonical contains a non-directory member entry — skipping (malformed bundle)"
            );
        }
    }
    leaves.sort();
    Ok(leaves)
}

/// **WP-21 bundle placement.** A bundle is invisible to engines — enabling one
/// in a scope places each of its MEMBER skills as a single cross-tool skill
/// symlink (`<scope>/.claude/skills/<member>` → `store/bundles/<bundle>/<member>`),
/// the same scope alias a single skill uses, so every engine reads it.
///
/// FIRST-WINS on a member-leaf collision: if the member's scope link already
/// exists and points somewhere OTHER than this bundle's member canonical
/// (another bundle/skill placed a same-named leaf), the collider is SKIPPED with
/// a `tracing::warn!` — never clobbered, never an error for the whole enable.
/// A link that already points into THIS bundle's member canonical is idempotent.
/// Returns a synthetic bundle-level mutation summarizing the placement; per-member
/// links are side effects under the one cross-tool skill path.
fn place_bundle(
    store: &Path,
    scope_claude: &Path,
    scope: &str,
    name: &str,
) -> Result<ClaudeStoreMutation, String> {
    let canonical = store_path_for(store, Kind::Bundle, name)?;
    if !canonical.exists() {
        return Err(format!(
            "store has no bundle named {name:?} (expected {})",
            canonical.display()
        ));
    }
    let members = bundle_member_leaves(store, name)?;
    for member in &members {
        // Guard each member leaf name (registry/disk-sourced) before it becomes
        // a path segment — same confinement the single-skill path enforces.
        validate_name(member)?;
        let target = canonical.join(member);
        let link = scope_path_for(scope_claude, Kind::Skill, member)?;
        if !is_under_claude_or_store(&link) {
            return Err(format!(
                "bundle member enable target outside .claude/store: {}",
                link.display()
            ));
        }
        // FIRST-WINS collision check — MUST precede make_symlink (which renames
        // over unconditionally). Inspect the existing node, if any.
        if let Ok(meta) = std::fs::symlink_metadata(&link) {
            // Already points into THIS bundle's member canonical → idempotent.
            let owned = meta.file_type().is_symlink()
                && std::fs::canonicalize(&link)
                    .ok()
                    .zip(std::fs::canonicalize(&target).ok())
                    .map(|(l, t)| l == t)
                    .unwrap_or(false);
            if owned {
                continue;
            }
            // Owned by someone else (another bundle/skill, or a real file) →
            // first-wins: keep theirs, skip ours, warn, keep going.
            tracing::warn!(
                bundle = %name,
                member = %member,
                link = %link.display(),
                "bundle member leaf collides with an existing placement; first-wins — skipping this member, keeping the existing one"
            );
            continue;
        }
        make_symlink(&target, &link)?;
    }
    Ok(ClaudeStoreMutation {
        kind: Kind::Bundle.as_str().to_string(),
        name: name.to_string(),
        scope: scope.to_string(),
        path: canonical.to_string_lossy().to_string(),
        link_target: None,
    })
}

/// **WP-21 bundle removal.** Disabling a bundle drops every member skill link
/// THIS bundle placed in the scope — and ONLY those. A member leaf whose scope
/// link resolves into a DIFFERENT bundle/skill canonical (a first-wins collision
/// winner from another owner) is left untouched. Idempotent on missing links.
fn disable_bundle(store: &Path, scope_claude: &Path, name: &str) -> Result<(), String> {
    let canonical = store_path_for(store, Kind::Bundle, name)?;
    // Idempotent on a missing canonical: a bundle with no canonical backs no
    // scope links, so there is nothing to remove (mirrors place_bundle's check).
    if !canonical.exists() {
        return Ok(());
    }
    // canonicalize the bundle root once so ownership checks survive symlinked
    // store roots; fall back to the lexical path if it can't be resolved.
    let canon_root = std::fs::canonicalize(&canonical).unwrap_or_else(|_| canonical.clone());
    // Drive the sweep from the ON-DISK member subdirs (the truth of what this
    // canonical actually backs), NOT the registry `members[]` — so disable
    // provably removes every link this bundle backs even if the registry list is
    // stale/subset.
    let rd = match std::fs::read_dir(&canonical) {
        Ok(rd) => rd,
        Err(_) => return Ok(()), // vanished mid-op → nothing to remove
    };
    for entry in rd.flatten() {
        if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let member = entry.file_name().to_string_lossy().to_string();
        // member is a disk-sourced path segment — confine it before path use.
        if validate_name(&member).is_err() {
            continue;
        }
        let link = scope_path_for(scope_claude, Kind::Skill, &member)?;
        if !is_under_claude_or_store(&link) {
            return Err(format!(
                "bundle member disable target outside .claude/store: {}",
                link.display()
            ));
        }
        // Only remove a link this bundle owns: a symlink resolving into this
        // bundle's canonical (store/bundles/<bundle>/...). A foreign leaf
        // (another bundle/skill) or a real file is left alone. Absent → idempotent.
        let meta = match std::fs::symlink_metadata(&link) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let owned = std::fs::canonicalize(&link)
            .map(|resolved| resolved.starts_with(&canon_root))
            .unwrap_or(false);
        if owned {
            remove_primitive(&link, Kind::Skill)?;
        }
    }
    Ok(())
}

/// **WP-21 bundle-aware dependents scan.** A bundle's "dependents" are the scope
/// links of its MEMBER skills (the bundle itself is never placed, so
/// `dependent_search_dirs(Kind::Bundle)` is empty — see that arm). Scans each
/// member subdir under the bundle canonical against the skill search dirs and
/// unions the results. Shared by `oba_safe_delete` and `oba_dependents` so a
/// bundle with any live member link refuses a hard-delete (the incident
/// guardrail). Dedupes so a link counted under multiple members appears once.
fn bundle_member_dependents(
    _store: &Path,
    bundle_canonical: &Path,
    _name: &str,
    scope_roots: &[PathBuf],
) -> Vec<PathBuf> {
    // DATA-LOSS GUARDRAIL: scan the WHOLE bundle canonical, NOT per registry
    // member leaf. `scan_live_dependents` matches any link whose target == or
    // starts_with the canonical, so scanning the bundle ROOT catches EVERY scope
    // link resolving anywhere into store/bundles/<name>/ — independent of the
    // registry `members[]` list. This must be drift-proof: `guarded_delete`
    // `remove_dir_all`s the whole canonical tree, so a stale/subset `members[]`
    // must not be able to hide a live dependent and turn a refuse into a wipe.
    let skill_dirs = dependent_search_dirs(scope_roots, Kind::Skill);
    crate::commands::claude_config::scan_live_dependents(bundle_canonical, &skill_dirs)
}

/// `claude_primitive_remove` core: delete the scope-local entry (link or real
/// file/dir). Does NOT touch the store. On a store-backed symlink this is
/// identical to disable; on a real primitive it deletes the actual file.
fn remove_core(scope_claude: &Path, kind: Kind, name: &str) -> Result<(), String> {
    let p = scope_path_for(scope_claude, kind, name)?;
    if !is_under_claude_or_store(&p) {
        return Err(format!(
            "remove target outside .claude/store: {}",
            p.display()
        ));
    }
    remove_primitive(&p, kind)
}

/// `claude_primitive_copy` core: copy the resolved primitive from one scope's
/// farm into another scope's farm, leaving the source in place. Atomic at the
/// destination. The copy is the resolved (real) primitive — symlinks are
/// followed so the destination owns a real file/dir, not a dangling link.
fn copy_core(
    from_claude: &Path,
    to_claude: &Path,
    from_scope: &str,
    to_scope: &str,
    kind: Kind,
    name: &str,
) -> Result<ClaudeStoreMutation, String> {
    let _ = from_scope;
    let src = scope_path_for(from_claude, kind, name)?;
    let dst = scope_path_for(to_claude, kind, name)?;
    if !is_under_claude_or_store(&src) {
        return Err(format!(
            "copy source outside .claude/store: {}",
            src.display()
        ));
    }
    if !is_under_claude_or_store(&dst) {
        return Err(format!(
            "copy dest outside .claude/store: {}",
            dst.display()
        ));
    }
    if !src.exists() {
        return Err(format!("copy source missing: {}", src.display()));
    }
    // Resolve through any symlink so the destination is a real, standalone
    // copy (not a link that would dangle if the source scope is later cleaned).
    let resolved = std::fs::canonicalize(&src)
        .map_err(|e| format!("resolve source {}: {e}", src.display()))?;
    if kind.is_dir_primitive() {
        atomic_copy_dir(&resolved, &dst)?;
    } else {
        atomic_copy_file(&resolved, &dst)?;
    }
    Ok(ClaudeStoreMutation {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        scope: to_scope.to_string(),
        path: dst.to_string_lossy().to_string(),
        link_target: None,
    })
}

/// `claude_primitive_move` core: copy-then-remove-source. The destination
/// write is atomic; the source removal only runs once the destination is in
/// place, so a crash between the two leaves the source intact (move degrades
/// to copy, never to loss).
fn move_core(
    from_claude: &Path,
    to_claude: &Path,
    from_scope: &str,
    to_scope: &str,
    kind: Kind,
    name: &str,
) -> Result<ClaudeStoreMutation, String> {
    let mutation = copy_core(from_claude, to_claude, from_scope, to_scope, kind, name)?;
    // Source removal is scope-local; never touches the store.
    let src = scope_path_for(from_claude, kind, name)?;
    remove_primitive(&src, kind)?;
    Ok(mutation)
}

// ─── WP-23: per-engine file-kind write paths (skill/agent/command) ────────────
//
// The Phase-1 farm above (`place_primitive`/`disable_core`/`remove_core`) is
// Claude-specific: it hardcodes `<scope>/.claude/<kind>s/<name>` via
// `scope_path_for` and confines through `is_under_claude_or_store` (a `.claude`
// segment check). WP-23 generalizes the file-based half per engine by threading
// the frozen `EngineLayout` location/mechanism cell:
//
//   - **Claude** — UNCHANGED. `engine == Claude` routes straight back through
//     the Phase-1 `*_core` functions, so Claude's on-disk writes are
//     byte-for-byte identical to Phase 1 (asserted in the tests).
//   - **Gemini** — skills `{root}/.agents/skills/{name}/` (SymlinkDir),
//     agents `{user_root}/.gemini/agents/{name}.md` (SymlinkDir),
//     commands `{user_root}/.gemini/commands/{name}.toml` (File → copy-on-enable).
//   - **Codex** — skills `{root}/.agents/skills/{name}/` (SymlinkDir),
//     agents `{user_root}/.codex/agents/{name}.toml` (File → copy-on-enable),
//     commands `{user_root}/.codex/prompts/{name}.md` (File, deprecated).
//
// `Mechanism::SymlinkDir` cells symlink into the store (same as Phase 1);
// `Mechanism::File` cells copy the resolved store primitive on enable (a symlink
// would dangle for a single standalone file the engine reads in place). Disable
// / remove drop the scope-local node either way; the store copy is untouched.
//
// **Confinement is per-engine**: a resolved write target must sit under one of
// the engine's declared roots (its dotdir under the scope root, plus the
// cross-tool `.agents/` for skill cells) — the generalization of the Claude-only
// `.claude` check. A target computed outside those roots is refused before any
// FS touch. Every write is atomic (symlink temp+rename / atomic copy).

/// Map a wire engine id (`"claude"` / `"gemini"` / `"codex"`) to the typed
/// `EngineId`. Defaults are NOT applied here — the command layer defaults a
/// missing arg to `"claude"` so the Phase-1 path stays the zero-config default.
fn parse_engine(s: &str) -> Result<EngineId, String> {
    match s {
        "claude" => Ok(EngineId::Claude),
        "gemini" => Ok(EngineId::Gemini),
        "codex" => Ok(EngineId::Codex),
        "antigravity" | "antigravity-cli" => Ok(EngineId::Antigravity),
        _ => Err(format!("engine must be claude|gemini|codex|antigravity, got {s:?}")),
    }
}

/// The `PrimitiveKind` for a file-based `Kind`. Errors for hook/mcp (those route
/// through the settings-embedded merge engine, never the file paths).
fn primitive_kind_for(kind: Kind) -> Result<PrimitiveKind, String> {
    match kind {
        Kind::Skill => Ok(PrimitiveKind::Skill),
        Kind::Agent => Ok(PrimitiveKind::Agent),
        Kind::Command => Ok(PrimitiveKind::Command),
        Kind::Hook | Kind::Mcp => Err(format!(
            "kind {} is settings-embedded, not file-based",
            kind.as_str()
        )),
        // A bundle never enters the per-engine file-primitive machinery: it has
        // no single placed file/dir of its own — its member skills are placed
        // (WP-21). Error here so a bundle can't accidentally be transcoded or
        // farmed as if it were a leaf primitive.
        Kind::Bundle => Err(
            "bundle is not a file-based engine primitive; bundles place their members (WP-21)"
                .to_string(),
        ),
    }
}

/// Look up the frozen layout cell for `(engine, kind)`.
fn file_layout_cell(engine: EngineId, kind: Kind) -> Result<KindLayout, String> {
    let pk = primitive_kind_for(kind)?;
    let layout =
        engine_layout_by_id(engine).ok_or_else(|| format!("no layout for engine {engine:?}"))?;
    layout
        .kinds
        .get(&pk)
        .cloned()
        .ok_or_else(|| format!("engine {engine:?} has no {pk:?} cell"))
}

/// The engine's dotdir basename (`.claude` / `.gemini` / `.codex`).
fn engine_dotdir_name(engine: EngineId) -> &'static str {
    match engine {
        EngineId::Claude => ".claude",
        EngineId::Gemini => ".gemini",
        EngineId::Codex => ".codex",
        EngineId::Antigravity => ".gemini/antigravity-cli",
    }
}

/// Resolve the user-tier dotdir for an engine (`<home>/.claude` / `.gemini` /
/// `.codex`) — the home of `{user_root}` location templates. `user_home` is the
/// resolved user home dir (threaded in by the caller, NOT read from the process
/// env here — so the pure core never depends on a global `HOME` the test suite
/// would race on).
fn engine_user_dotdir(engine: EngineId, user_home: &Path) -> PathBuf {
    user_home.join(engine_dotdir_name(engine))
}

/// Resolve a file-based primitive's concrete on-disk path for `(engine, kind,
/// name)` in a scope whose root is `scope_root` (the directory that holds the
/// engine dotdir — `~` for workspace, `<project.root_path>` for project scope).
///
/// This reads the frozen `EngineLayout` `location` template and substitutes the
/// three placeholders the file cells use:
///   - `{root}`      → `scope_root` (scope-relative dotdir parent).
///   - `{user_root}` → `~` (user-tier; these cells are user-scoped on disk).
///   - `{name}`      → the validated primitive name.
///
/// The Gemini `commands/**/{name}.toml` namespace wildcard collapses to a flat
/// `commands/{name}.toml` for writes (Ngwa writes the un-namespaced leaf; reads
/// still walk the `**` tree). For dir primitives (skills) a trailing-slash
/// template yields a directory path.
fn engine_file_path(
    engine: EngineId,
    cell: &KindLayout,
    scope_root: &Path,
    user_home: &Path,
    kind: Kind,
    name: &str,
) -> Result<PathBuf, String> {
    // Resolve the template body up to the engine-relative dotdir, then append
    // the kind subdir + leaf so the wildcard / trailing-slash quirks don't leak.
    // We don't string-substitute the raw template (its `**` and trailing `/`
    // are not real path segments) — we read it only to pick the dotdir + subdir.
    let leaf = if kind.is_dir_primitive() {
        name.to_string()
    } else {
        // File extension is engine-declared by the cell location suffix.
        let ext = if cell.location.ends_with(".toml") {
            "toml"
        } else {
            // Claude/Gemini agents + Claude/Codex commands are `.md`.
            "md"
        };
        format!("{name}.{ext}")
    };

    let pk = primitive_kind_for(kind)?;
    let path = match (engine, pk) {
        // ── Claude: scope-relative `.claude/<kind>s/<leaf>` ──────────────────
        (EngineId::Claude, PrimitiveKind::Skill) => {
            scope_root.join(".claude").join("skills").join(&leaf)
        }
        (EngineId::Claude, PrimitiveKind::Agent) => {
            scope_root.join(".claude").join("agents").join(&leaf)
        }
        (EngineId::Claude, PrimitiveKind::Command) => {
            scope_root.join(".claude").join("commands").join(&leaf)
        }
        // ── Gemini ───────────────────────────────────────────────────────────
        // Skills ride the cross-tool `.agents/skills/` alias (scope-relative).
        (EngineId::Gemini, PrimitiveKind::Skill) => {
            scope_root.join(".agents").join("skills").join(&leaf)
        }
        // Agents + commands are user-tier under `~/.gemini`.
        (EngineId::Gemini, PrimitiveKind::Agent) => engine_user_dotdir(engine, user_home)
            .join("agents")
            .join(&leaf),
        (EngineId::Gemini, PrimitiveKind::Command) => engine_user_dotdir(engine, user_home)
            .join("commands")
            .join(&leaf),
        // ── Antigravity ──────────────────────────────────────────────────────
        (EngineId::Antigravity, PrimitiveKind::Skill) => {
            scope_root.join(".agents").join("skills").join(&leaf)
        }
        (EngineId::Antigravity, PrimitiveKind::Agent) => engine_user_dotdir(engine, user_home)
            .join("agents")
            .join(&leaf),
        (EngineId::Antigravity, PrimitiveKind::Command) => engine_user_dotdir(engine, user_home)
            .join("commands")
            .join(&leaf),
        // ── Codex ──────────────────────────────────────────────────────────
        (EngineId::Codex, PrimitiveKind::Skill) => {
            scope_root.join(".agents").join("skills").join(&leaf)
        }
        (EngineId::Codex, PrimitiveKind::Agent) => engine_user_dotdir(engine, user_home)
            .join("agents")
            .join(&leaf),
        // Deprecated, but still writable (read/migration parity).
        (EngineId::Codex, PrimitiveKind::Command) => engine_user_dotdir(engine, user_home)
            .join("prompts")
            .join(&leaf),
        (_, other) => return Err(format!("engine {engine:?} has no file path for {other:?}")),
    };
    Ok(path)
}

/// Per-engine write confinement: a resolved file target must sit under one of
/// the engine's declared roots — its dotdir (under the scope root for
/// scope-relative cells, `~/<dotdir>` for user-tier cells) OR the cross-tool
/// `.agents/` dir (for skill cells). This is the per-engine generalization of
/// Phase-1's Claude-only `is_under_claude_or_store` check.
fn is_under_engine_root(engine: EngineId, scope_root: &Path, user_home: &Path, p: &Path) -> bool {
    // Build the permitted roots for this engine + scope.
    let mut roots: Vec<PathBuf> = Vec::new();
    // Scope-relative dotdir (Claude `.claude`, and the project-scope dotdirs).
    roots.push(scope_root.join(engine_dotdir_name(engine)));
    // User-tier dotdir (Gemini/Codex agents + commands live here).
    roots.push(engine_user_dotdir(engine, user_home));
    // Cross-tool `.agents/` (skills): scope-relative + user home.
    roots.push(scope_root.join(".agents"));
    roots.push(user_home.join(".agents"));
    roots.iter().any(|r| p == r || p.starts_with(r))
}

/// `enable` for an arbitrary engine. Claude routes back through the Phase-1
/// `place_primitive` (byte-identical). For other engines: `SymlinkDir` cells symlink
/// into the store; `File` cells copy the resolved store primitive on enable.
/// Idempotent. Atomic. Per-engine path-confined.
fn enable_for_core(
    engine: EngineId,
    store: &Path,
    scope_root: &Path,
    user_home: &Path,
    scope: &str,
    kind: Kind,
    name: &str,
) -> Result<ClaudeStoreMutation, String> {
    if engine == EngineId::Claude {
        // Phase-1 path: confinement + write are byte-unchanged.
        let scope_claude = scope_root.join(".claude");
        return place_primitive(store, &scope_claude, scope, kind, name);
    }
    let cell = file_layout_cell(engine, kind)?;
    let target_src = store_path_for(store, kind, name)?;
    if !target_src.exists() {
        return Err(format!(
            "store has no {} named {name:?} (expected {})",
            kind.as_str(),
            target_src.display()
        ));
    }
    let dest = engine_file_path(engine, &cell, scope_root, user_home, kind, name)?;
    if !is_under_engine_root(engine, scope_root, user_home, &dest) {
        return Err(format!(
            "enable target outside {engine:?} roots: {}",
            dest.display()
        ));
    }
    let link_target = match cell.mechanism {
        Mechanism::SymlinkDir => {
            // Idempotent: an existing store-backed link is a no-op.
            if !is_engine_enabled(&dest, store) {
                make_symlink(&target_src, &dest)?;
            }
            Some(target_src.to_string_lossy().to_string())
        }
        Mechanism::File => {
            // Copy-on-enable: a single standalone file the engine reads in place.
            // We resolve the store copy into real bytes at the destination.
            if kind.is_dir_primitive() {
                atomic_copy_dir(&target_src, &dest)?;
            } else {
                atomic_copy_file(&target_src, &dest)?;
            }
            None
        }
        Mechanism::SettingsKey => {
            return Err(format!(
                "kind {} for {engine:?} is settings-embedded, not file-based",
                kind.as_str()
            ))
        }
    };
    Ok(ClaudeStoreMutation {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        scope: scope.to_string(),
        path: dest.to_string_lossy().to_string(),
        link_target,
    })
}

/// True iff `dest` is a symlink resolving into `store` (engine-agnostic enabled
/// probe for SymlinkDir cells; the per-engine sibling of `is_enabled_in`).
fn is_engine_enabled(dest: &Path, store: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dest) else {
        return false;
    };
    if !meta.file_type().is_symlink() {
        return false;
    }
    match std::fs::canonicalize(dest) {
        Ok(resolved) => is_in_store(&resolved, store),
        Err(_) => false,
    }
}

/// `disable` for an arbitrary engine — drop only the scope-local node (link or
/// copied file/dir). The store copy is untouched. Idempotent. Claude routes back
/// through Phase-1 `disable_core`.
fn disable_for_core(
    engine: EngineId,
    scope_root: &Path,
    user_home: &Path,
    kind: Kind,
    name: &str,
) -> Result<(), String> {
    if engine == EngineId::Claude {
        let scope_claude = scope_root.join(".claude");
        return disable_core(&scope_claude, kind, name);
    }
    let cell = file_layout_cell(engine, kind)?;
    let dest = engine_file_path(engine, &cell, scope_root, user_home, kind, name)?;
    if !is_under_engine_root(engine, scope_root, user_home, &dest) {
        return Err(format!(
            "disable target outside {engine:?} roots: {}",
            dest.display()
        ));
    }
    remove_primitive(&dest, kind)
}

/// `remove` for an arbitrary engine — delete the scope-local entry (link or real
/// file/dir). Does NOT touch the store. Claude routes back through Phase-1
/// `remove_core`. (For file-kind primitives disable and remove are the same FS
/// op — drop the scope-local node — but kept distinct to mirror the contract and
/// the Phase-1 surface.)
fn remove_for_core(
    engine: EngineId,
    scope_root: &Path,
    user_home: &Path,
    kind: Kind,
    name: &str,
) -> Result<(), String> {
    if engine == EngineId::Claude {
        let scope_claude = scope_root.join(".claude");
        return remove_core(&scope_claude, kind, name);
    }
    let cell = file_layout_cell(engine, kind)?;
    let dest = engine_file_path(engine, &cell, scope_root, user_home, kind, name)?;
    if !is_under_engine_root(engine, scope_root, user_home, &dest) {
        return Err(format!(
            "remove target outside {engine:?} roots: {}",
            dest.display()
        ));
    }
    remove_primitive(&dest, kind)
}

// ─── WP-24: cross-engine transcode copy/move (D-09 batch) ─────────────────────
//
// Resolve `(fromEngine, kind, toEngine)` → a transcode plan per the
// directionality contract in `plans/cockpit/06-cross-engine-transcode.md`, then
// materialize the destination with the existing forward transcoder
// (`md_to_codex_toml` / `md_to_gemini_command_toml`) for MD→TOML pairs, or a
// direct file/dir copy for same-format pairs. Every blocked direction (TOML→MD)
// returns `StoreError::TranscodeUnsupported` BEFORE any disk write.
//
// The unit of work is the SOURCE: we read the resolved scope-local primitive in
// `fromEngine`/`fromScope`, then write one destination per requested
// `(toEngine, toScope)`. Each destination write is atomic (temp+rename, reusing
// the WP-23 file-write path). A batch reports a per-row result so a partial
// failure (one bad destination) never rolls back the rows that succeeded.

use merge::StoreError;

/// The resolved transcode plan for one `(fromEngine, kind, toEngine)` triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscodePlan {
    /// Same on-disk format end-to-end → direct file/dir copy (no transform).
    /// (Claude↔Gemini MD agents, any skill dir, Claude→Claude, …)
    DirectCopy,
    /// MD → Codex TOML agent: `md_to_codex_toml`.
    MdToCodexToml,
    /// MD command → Gemini slash-command TOML: `md_to_gemini_command_toml`.
    MdToGeminiCommandToml,
}

/// Map a typed `EngineId` to its stable wire id (matches `parse_engine`).
fn engine_wire_id(engine: EngineId) -> &'static str {
    match engine {
        EngineId::Claude => "claude",
        EngineId::Gemini => "gemini",
        EngineId::Codex => "codex",
        EngineId::Antigravity => "antigravity-cli",
    }
}

/// The on-disk `ConfigFormat` of a file-based `(engine, kind)` cell — the basis
/// for the same-format vs transcode decision. Reads the frozen `EngineLayout`
/// cell so the matrix stays in lockstep with G-ADAPTER. Errors for hook/mcp
/// (settings-embedded — out of v2b cross-engine scope) and for cells the engine
/// doesn't have.
fn cell_format(engine: EngineId, kind: Kind) -> Result<ConfigFormat, StoreError> {
    let cell =
        file_layout_cell(engine, kind).map_err(|message| StoreError::Unsupported { message })?;
    Ok(cell.format)
}

/// Resolve `(fromEngine, kind, toEngine)` → a [`TranscodePlan`] per the
/// directionality matrix in `06-cross-engine-transcode.md`. Returns
/// `StoreError::TranscodeUnsupported` for the blocked TOML→MD directions and
/// `StoreError::Unsupported` for kinds that are out of v2b cross-engine scope
/// (hook/mcp — settings-embedded, not a portable primitive).
fn resolve_transcode_plan(
    from_engine: EngineId,
    to_engine: EngineId,
    kind: Kind,
) -> Result<TranscodePlan, StoreError> {
    // hooks / MCP are settings-embedded — explicitly OUT of v2b cross-engine
    // scope (06 §matrix: "not a portable primitive"). Never offer a copy.
    if !kind.is_file_based() {
        return Err(StoreError::Unsupported {
            message: format!(
                "{} is settings-embedded; cross-engine copy is out of v2b scope",
                kind.as_str()
            ),
        });
    }

    let from_fmt = cell_format(from_engine, kind)?;
    let to_fmt = cell_format(to_engine, kind)?;

    use ConfigFormat::*;
    match (from_fmt, to_fmt) {
        // Same on-disk format end-to-end → verbatim copy. Covers Claude↔Gemini
        // MD agents, every skill (MD dir under `.agents/skills`), Claude→Claude,
        // and any TOML→TOML pair.
        (MdYaml, MdYaml) | (Toml, Toml) => Ok(TranscodePlan::DirectCopy),
        // MD → TOML: forward transcode. Which entry point depends on the kind:
        //   - agent  → `md_to_codex_toml`  (Codex agent: body → system_prompt)
        //   - command→ `md_to_gemini_command_toml` (Gemini command: body → prompt)
        (MdYaml, Toml) => match kind {
            Kind::Agent => Ok(TranscodePlan::MdToCodexToml),
            Kind::Command => Ok(TranscodePlan::MdToGeminiCommandToml),
            // A skill is always MD on both sides (no TOML skill cell exists), so
            // this arm is unreachable for skills; guard it anyway.
            Kind::Skill => Err(StoreError::Unsupported {
                message: "skills are MD on every engine; no MD→TOML skill transcode".to_string(),
            }),
            // hook/mcp and bundle are all excluded by the `!is_file_based`
            // guard at the top of this fn, so this arm is unreachable for them.
            Kind::Hook | Kind::Mcp | Kind::Bundle => {
                unreachable!("file-based guard above excludes hook/mcp/bundle")
            }
        },
        // TOML → MD: BLOCKED. No reverse transcoder exists (06 §matrix). Refused
        // before any write so a blocked destination never leaves a partial file.
        (Toml, MdYaml) => Err(StoreError::TranscodeUnsupported {
            from: engine_wire_id(from_engine).to_string(),
            to: engine_wire_id(to_engine).to_string(),
            reason: format!(
                "no reverse transcoder for {} {} (TOML→Markdown); see 06-cross-engine-transcode.md",
                engine_wire_id(from_engine),
                kind.as_str()
            ),
        }),
        // JSON-embedded should never reach here (file-based guard above).
        (JsonEmbedded, _) | (_, JsonEmbedded) => Err(StoreError::Unsupported {
            message: "settings-embedded format has no portable cross-engine copy".to_string(),
        }),
    }
}

/// Read the resolved (real, symlink-followed) source primitive's UTF-8 body for
/// a transcode. Only single-file primitives (agent/command) are transcoded;
/// skills are dir primitives and always take the same-format direct-copy path.
fn read_source_md(src_resolved: &Path) -> Result<String, StoreError> {
    std::fs::read_to_string(src_resolved).map_err(|e| StoreError::Io {
        path: src_resolved.to_string_lossy().to_string(),
        message: format!("read source for transcode: {e}"),
    })
}

/// Execute one cross-engine destination write per a resolved [`TranscodePlan`].
/// `src_resolved` is the real (symlink-followed) source path in the from-scope;
/// `dest` is the computed, already-confinement-checked destination path. The
/// write is atomic (temp+rename) for both copy and transcode. Returns the bytes-
/// landed `ClaudeStoreMutation` (`link_target: None` — a cross-engine copy always
/// materializes a real file/dir, never a link).
#[allow(clippy::too_many_arguments)]
fn write_transcoded_dest(
    plan: TranscodePlan,
    kind: Kind,
    name: &str,
    src_resolved: &Path,
    dest: &Path,
    to_scope: &str,
) -> Result<ClaudeStoreMutation, StoreError> {
    // Same-format → independent copy; cross-format → transcode. (Symlink-based
    // dedup was reverted after a data-loss incident — see the primitive-registry
    // plan; symlinks return there with provenance + dependent-aware deletes.)
    match plan {
        TranscodePlan::DirectCopy => {
            if kind.is_dir_primitive() {
                atomic_copy_dir(src_resolved, dest).map_err(|message| StoreError::Io {
                    path: dest.to_string_lossy().to_string(),
                    message,
                })?;
            } else {
                atomic_copy_file(src_resolved, dest).map_err(|message| StoreError::Io {
                    path: dest.to_string_lossy().to_string(),
                    message,
                })?;
            }
        }
        TranscodePlan::MdToCodexToml | TranscodePlan::MdToGeminiCommandToml => {
            let md = read_source_md(src_resolved)?;
            let toml = match plan {
                TranscodePlan::MdToCodexToml => {
                    crate::pkg::engine_adapters::transcoder::md_to_codex_toml(&md)
                }
                TranscodePlan::MdToGeminiCommandToml => {
                    crate::pkg::engine_adapters::transcoder::md_to_gemini_command_toml(&md)
                }
                TranscodePlan::DirectCopy => unreachable!(),
            }
            .map_err(|e| StoreError::UnrepresentableValue {
                path: dest.to_string_lossy().to_string(),
                message: format!("transcode failed: {e}"),
            })?;
            // Atomic: write the transcoded bytes to a temp sibling, fsync, rename.
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| StoreError::Io {
                    path: parent.to_string_lossy().to_string(),
                    message: format!("mkdir: {e}"),
                })?;
            }
            let tmp = temp_sibling(dest);
            write_then_sync(&tmp, toml.as_bytes()).map_err(|message| StoreError::Io {
                path: tmp.to_string_lossy().to_string(),
                message,
            })?;
            std::fs::rename(&tmp, dest).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                StoreError::Io {
                    path: dest.to_string_lossy().to_string(),
                    message: format!("rename {} -> {}: {e}", tmp.display(), dest.display()),
                }
            })?;
        }
    }
    Ok(ClaudeStoreMutation {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        scope: to_scope.to_string(),
        path: dest.to_string_lossy().to_string(),
        link_target: None,
    })
}

/// The pure core of one batch row: resolve the plan, compute + confine the
/// destination path, then write atomically. Reads `from`'s resolved source once
/// per row (callers may hoist the read; kept per-row here for blocked-pair
/// safety — a blocked plan returns BEFORE the source is even read or any path is
/// touched). `from_resolved` is the already-resolved (symlink-followed) source
/// path; `to_scope_root`/`to_user_home` locate the destination per the WP-23
/// file-path math. Never partial-writes: a blocked or unsupported plan errors
/// before any FS mutation.
#[allow(clippy::too_many_arguments)]
fn copy_cross_engine_row(
    from_engine: EngineId,
    to_engine: EngineId,
    kind: Kind,
    name: &str,
    from_resolved: &Path,
    to_scope: &str,
    to_scope_root: &Path,
    to_user_home: &Path,
) -> Result<ClaudeStoreMutation, StoreError> {
    // 1. Resolve the plan FIRST — a blocked (TOML→MD) or out-of-scope direction
    //    errors here, before any destination path is computed or touched.
    let plan = resolve_transcode_plan(from_engine, to_engine, kind)?;

    // 2. Compute the destination cell + path for the TARGET engine. For a
    //    transcode the on-disk leaf extension follows the target engine's cell
    //    (`.toml`), which `engine_file_path` already derives from the cell.
    let cell =
        file_layout_cell(to_engine, kind).map_err(|message| StoreError::Unsupported { message })?;
    let dest = engine_file_path(to_engine, &cell, to_scope_root, to_user_home, kind, name)
        .map_err(|message| StoreError::Unsupported { message })?;

    // 3. Per-engine confinement on the TARGET — same guard WP-23 enforces.
    if !is_under_engine_root(to_engine, to_scope_root, to_user_home, &dest) {
        return Err(StoreError::Unsupported {
            message: format!(
                "cross-engine copy target outside {to_engine:?} roots: {}",
                dest.display()
            ),
        });
    }

    // 4. Materialize the destination atomically.
    write_transcoded_dest(plan, kind, name, from_resolved, &dest, to_scope)
}

/// The user home dir (`$HOME`). Resolved at the command boundary and threaded
/// into the pure file-path core so the core itself never touches the env.
fn user_home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME not set".to_string())
}

/// Resolve a `ClaudeStoreScope` string to the scope ROOT (the parent of the
/// engine dotdir): `~` for `workspace`, `<project.root_path>` for `project:<id>`.
/// The per-engine sibling of [`resolve_scope_claude`] (which appends `.claude`);
/// the WP-23 file paths append the engine-specific dotdir themselves.
async fn resolve_scope_root_dir(db: &Arc<PaDb>, scope: &str) -> Result<PathBuf, String> {
    validate_pin_scope(scope)?;
    if scope == "workspace" {
        let home = std::env::var_os("HOME").ok_or_else(|| "HOME not set".to_string())?;
        return Ok(PathBuf::from(home));
    }
    let id = scope
        .strip_prefix("project:")
        .ok_or_else(|| format!("unexpected scope {scope:?}"))?;
    let pool = db.ensure_pool().await?;
    let project = get_project(&pool, id)
        .await?
        .ok_or_else(|| format!("no project with id {id:?}"))?;
    let root = project
        .root_path
        .ok_or_else(|| format!("project {id:?} has no root_path"))?;
    expand(&root).map_err(|e| e.to_string())
}

// ─── Tauri command surface ────────────────────────────────────────────────────

/// List the central-store catalog. Optionally filter by kind. `enabledIn` is
/// populated by probing the workspace scope plus every known project scope.
#[tauri::command]
pub async fn claude_store_list(
    db: State<'_, Arc<PaDb>>,
    kind: Option<String>,
) -> Result<Vec<ClaudeStoreEntry>, String> {
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let kinds: Vec<Kind> = match kind {
        Some(k) => vec![Kind::parse(&k)?],
        None => vec![Kind::Agent, Kind::Skill, Kind::Command],
    };

    // Build the set of scopes to probe for `enabledIn`: workspace + every
    // project with a root_path.
    let mut scopes: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(claude) = resolve_scope_claude(&db, "workspace").await {
        scopes.push(("workspace".to_string(), claude));
    }
    let pool = db.ensure_pool().await?;
    if let Ok(projects) = crate::commands::projects::list_projects(&pool, false).await {
        for p in projects {
            if let Some(root) = p.root_path.as_deref() {
                if let Ok(expanded) = expand(root) {
                    scopes.push((format!("project:{}", p.id), expanded.join(".claude")));
                }
            }
        }
    }

    // Overlay stored provenance from store/registry.json onto the scanned
    // entries. A missing/corrupt index degrades to empty (load never fails), so
    // every entry simply keeps its synthesized-`local` provenance.
    let prov_map = registry::provenance_map(&registry::load(&store));

    let mut out = Vec::new();
    for k in kinds {
        if !k.is_file_based() {
            // hook/mcp catalogs are owned by the WP-03 merge engine; bundle
            // listing is deferred to a later WP (bundles surface via their
            // member skills). Either way, skip here.
            continue;
        }
        for mut entry in list_store_kind(&store, k) {
            entry.enabled_in = scopes
                .iter()
                .filter(|(_, claude)| is_enabled_in(claude, &store, k, &entry.name))
                .map(|(scope, _)| scope.clone())
                .collect();
            registry::overlay_provenance(&mut entry, &prov_map);
            out.push(entry);
        }
    }
    Ok(out)
}

/// Import an on-disk primitive into the store as the new canonical copy.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_store_import(
    kind: String,
    name: String,
    sourcePath: String,
) -> Result<ClaudeStoreEntry, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        return Err(format!(
            "claude_store_import: kind {} is JSON-fragment; import via the merge engine",
            k.as_str()
        ));
    }
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let source = expand(&sourcePath).map_err(|e| e.to_string())?;
    import_core(&store, k, &name, &source)
}

/// Enable a store primitive in a scope (symlink-farm create for file-based;
/// merge-engine delegate for hook/mcp).
#[tauri::command]
pub async fn claude_primitive_enable(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<ClaudeStoreMutation, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    // WP-21: a bundle is invisible to engines — enabling one places each of its
    // MEMBER skills as a single cross-tool skill symlink (first-wins on a leaf
    // collision). It is never symlinked as itself, so it routes here, away from
    // both the file-based symlink farm and the merge engine.
    if k == Kind::Bundle {
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let scope_claude = resolve_scope_claude(&db, &scope).await?;
        return place_bundle(&store, &scope_claude, &scope, &name);
    }
    if !k.is_file_based() {
        // hook/mcp: read the store fragment and splice it into `scope`'s
        // settings file via the WP-03 merge engine. No symlink → link_target None.
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let root = resolve_scope_root(&db, &scope).await?;
        let target = enable_fragment_in_scope(&store, k, &name, &scope, root.as_deref())?;
        return Ok(ClaudeStoreMutation {
            kind: k.as_str().to_string(),
            name,
            scope,
            path: target.to_string_lossy().to_string(),
            link_target: None,
        });
    }
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let scope_claude = resolve_scope_claude(&db, &scope).await?;
    place_primitive(&store, &scope_claude, &scope, k, &name)
}

/// Disable a store primitive in a scope (drop the symlink for file-based;
/// merge-engine unmerge for hook/mcp).
#[tauri::command]
pub async fn claude_primitive_disable(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<(), String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    // WP-21: disabling a bundle removes every member skill link THIS bundle
    // placed in the scope (and only those — a first-wins collision winner from
    // another owner is left intact).
    if k == Kind::Bundle {
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let scope_claude = resolve_scope_claude(&db, &scope).await?;
        disable_bundle(&store, &scope_claude, &name)?;
        // Clear any pin keyed on the bundle name so no dangling pin remains.
        let pool = db.ensure_pool().await?;
        clear_pin_for(&pool, &scope, k.as_str(), &name).await?;
        return Ok(());
    }
    if !k.is_file_based() {
        // hook/mcp: remove the spliced block from `scope`'s settings file via
        // the merge engine. For hooks the event/file come from the fragment.
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let root = resolve_scope_root(&db, &scope).await?;
        disable_fragment_in_scope(&store, k, &name, &scope, root.as_deref())?;
    } else {
        let scope_claude = resolve_scope_claude(&db, &scope).await?;
        disable_core(&scope_claude, k, &name)?;
    }
    // WP-04: the primitive is no longer resolvable at `scope`; clear any pin
    // that pointed at it so no dangling pin remains.
    let pool = db.ensure_pool().await?;
    clear_pin_for(&pool, &scope, k.as_str(), &name).await?;
    Ok(())
}

/// Copy a primitive from one scope to another, leaving the source in place.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_copy(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    fromScope: String,
    toScope: String,
) -> Result<ClaudeStoreMutation, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        // hook/mcp copy = enable-in-dest; fromScope is left untouched. The store
        // fragment is the single source of truth for the block we splice.
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let dest_root = resolve_scope_root(&db, &toScope).await?;
        let target = enable_fragment_in_scope(&store, k, &name, &toScope, dest_root.as_deref())?;
        return Ok(ClaudeStoreMutation {
            kind: k.as_str().to_string(),
            name,
            scope: toScope,
            path: target.to_string_lossy().to_string(),
            link_target: None,
        });
    }
    let from_claude = resolve_scope_claude(&db, &fromScope).await?;
    let to_claude = resolve_scope_claude(&db, &toScope).await?;
    copy_core(&from_claude, &to_claude, &fromScope, &toScope, k, &name)
}

/// Move a primitive from one scope to another (copy-then-remove-source).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_move(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    fromScope: String,
    toScope: String,
) -> Result<ClaudeStoreMutation, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        // hook/mcp move = enable-in-dest THEN disable-in-source. Ordered so a
        // crash between the two legs degrades to "present in both" (a copy),
        // never to loss — mirrors the file-based move's copy-then-remove safety.
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let dest_root = resolve_scope_root(&db, &toScope).await?;
        let src_root = resolve_scope_root(&db, &fromScope).await?;
        let target = enable_fragment_in_scope(&store, k, &name, &toScope, dest_root.as_deref())?;
        disable_fragment_in_scope(&store, k, &name, &fromScope, src_root.as_deref())?;
        // WP-04: carry any pin from the source scope to the destination.
        let pool = db.ensure_pool().await?;
        repoint_pin(&pool, &fromScope, &toScope, k.as_str(), &name).await?;
        return Ok(ClaudeStoreMutation {
            kind: k.as_str().to_string(),
            name,
            scope: toScope,
            path: target.to_string_lossy().to_string(),
            link_target: None,
        });
    }
    let from_claude = resolve_scope_claude(&db, &fromScope).await?;
    let to_claude = resolve_scope_claude(&db, &toScope).await?;
    let mutation = move_core(&from_claude, &to_claude, &fromScope, &toScope, k, &name)?;
    // WP-04: the primitive left `fromScope` and now lives in `toScope`; carry
    // any pin across rather than orphan it at the now-empty source scope.
    let pool = db.ensure_pool().await?;
    repoint_pin(&pool, &fromScope, &toScope, k.as_str(), &name).await?;
    Ok(mutation)
}

/// Remove a primitive from a single scope's `.claude/` (does NOT touch the
/// store canonical copy).
#[tauri::command]
pub async fn claude_primitive_remove(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<(), String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        // hook/mcp scope-local remove = disable in that scope. Per the contract
        // `remove(scope)` is scope-local, so we splice the block OUT of `scope`'s
        // settings but leave the store fragment in the catalog (deleting the
        // catalog entry is a separate store op).
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let root = resolve_scope_root(&db, &scope).await?;
        disable_fragment_in_scope(&store, k, &name, &scope, root.as_deref())?;
    } else {
        let scope_claude = resolve_scope_claude(&db, &scope).await?;
        remove_core(&scope_claude, k, &name)?;
    }
    // WP-04: the scope-local primitive is gone; clear any pin pointing at it.
    let pool = db.ensure_pool().await?;
    clear_pin_for(&pool, &scope, k.as_str(), &name).await?;
    Ok(())
}

// ─── WP-23: per-engine unified-dispatch Tauri commands ────────────────────────
//
// `*_for` mirror the frozen TS write signatures (`claudePrimitiveEnableFor` /
// `DisableFor`); `engine` defaults to `"claude"` at the TS layer so the Phase-1
// zero-config path is unchanged. ONE command name handles BOTH families, keyed
// on `kind`:
//   - **file kinds** (skill / agent / command) → WP-23's per-engine symlink /
//     copy paths (`enable_for_core` / `disable_for_core` / `remove_for_core`).
//   - **settings-embedded** (hook / mcp) → WP-22's engine-aware merge dispatch
//     (`merge::enable_hook_for` / `enable_mcp_for` / …) via the store fragment.
// This unified routing is the contract WP-22 froze (`tauri-cmd.ts` documents
// `claude_primitive_enable_for` taking a `hookFile` arg and routing hook/mcp).
// `hookFile` is consumed only by the hook branch (JSON engines); file kinds and
// mcp ignore it. **Coordination note (drift §2):** WP-22's TS doc framed these
// as the *settings-embedded* commands; WP-23 widens the handler to also serve
// file kinds so the FE has one command per verb — flagged for WP-26.

/// Enable a store primitive in a scope for a specific engine. File kinds symlink
/// / copy per engine; hook/mcp splice via the settings-embedded merge engine.
#[tauri::command]
pub async fn claude_primitive_enable_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<ClaudeStoreMutation, String> {
    let e = parse_engine(&engine)?;
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    if !k.is_file_based() {
        // hook/mcp → WP-22 engine-aware merge dispatch (read store fragment,
        // splice for `engine`). No symlink → link_target None.
        let hf = parse_hook_file(hookFile.as_deref().unwrap_or("shared"));
        let project_root = resolve_scope_root(&db, &scope).await?;
        let target =
            enable_fragment_in_scope_for(e, &store, k, &name, &scope, project_root.as_deref(), hf)?;
        return Ok(ClaudeStoreMutation {
            kind: k.as_str().to_string(),
            name,
            scope,
            path: target.to_string_lossy().to_string(),
            link_target: None,
        });
    }
    let scope_root = resolve_scope_root_dir(&db, &scope).await?;
    let user_home = user_home_dir()?;
    enable_for_core(e, &store, &scope_root, &user_home, &scope, k, &name)
}

/// Disable a store primitive in a scope for a specific engine. File kinds drop
/// the scope-local link/copy; hook/mcp unsplice via the merge engine. Store
/// untouched either way.
#[tauri::command]
pub async fn claude_primitive_disable_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<(), String> {
    let e = parse_engine(&engine)?;
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        let hf = parse_hook_file(hookFile.as_deref().unwrap_or("shared"));
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let project_root = resolve_scope_root(&db, &scope).await?;
        disable_fragment_in_scope_for(e, &store, k, &name, &scope, project_root.as_deref(), hf)?;
    } else {
        let scope_root = resolve_scope_root_dir(&db, &scope).await?;
        let user_home = user_home_dir()?;
        disable_for_core(e, &scope_root, &user_home, k, &name)?;
    }
    // WP-04: clear any pin that pointed at the now-removed scope-local primitive.
    let pool = db.ensure_pool().await?;
    clear_pin_for(&pool, &scope, k.as_str(), &name).await?;
    Ok(())
}

/// Remove a primitive from a single scope for a specific engine (does NOT touch
/// the store canonical copy). For file kinds this is the scope-local delete; for
/// hook/mcp it is the scope-local unsplice (the store fragment survives).
#[tauri::command]
pub async fn claude_primitive_remove_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<(), String> {
    let e = parse_engine(&engine)?;
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    if !k.is_file_based() {
        let hf = parse_hook_file(hookFile.as_deref().unwrap_or("shared"));
        let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
        let project_root = resolve_scope_root(&db, &scope).await?;
        disable_fragment_in_scope_for(e, &store, k, &name, &scope, project_root.as_deref(), hf)?;
    } else {
        let scope_root = resolve_scope_root_dir(&db, &scope).await?;
        let user_home = user_home_dir()?;
        remove_for_core(e, &scope_root, &user_home, k, &name)?;
    }
    let pool = db.ensure_pool().await?;
    clear_pin_for(&pool, &scope, k.as_str(), &name).await?;
    Ok(())
}

// ─── WP-24: cross-engine transcode copy batch (D-09) — Tauri command ──────────
//
// `claude_primitive_copy_batch` copies (or moves) a single source primitive into
// N `(engine, scope)` destinations in one call, returning a per-row result in
// REQUEST ORDER. Mirrors the frozen FE wire shape (`NgwaCopyBatchResult` /
// `NgwaCopyRowResult` in `tauri-cmd.ts`): each row is `{ engine, scope, mode }`
// plus either `ok: true, mutation` or `ok: false, error`. The `mode` echoed back
// is the RESOLVED transcode relationship (we re-derive it from the matrix — the
// FE-supplied `mode` on the request is advisory and not trusted for the write).

/// One requested destination in a batch copy. Mirrors `NgwaCopyDestination`.
/// `mode` on the REQUEST is advisory (FE preview cue); the backend re-resolves
/// the authoritative plan from `(fromEngine, kind, engine)`.
#[derive(Debug, Clone, Deserialize)]
pub struct NgwaCopyDestination {
    pub engine: String,
    pub scope: String,
    /// Advisory mode the FE computed; ignored for the write, echoed (resolved).
    #[serde(default)]
    #[allow(dead_code)]
    pub mode: Option<String>,
}

/// The resolved transcode relationship echoed on each row. Serializes to
/// `NgwaTranscodeMode` (`same | transcode | blocked`).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum CopyMode {
    Same,
    Transcode,
    Blocked,
}

impl CopyMode {
    fn from_plan(plan: TranscodePlan) -> CopyMode {
        match plan {
            TranscodePlan::DirectCopy => CopyMode::Same,
            TranscodePlan::MdToCodexToml | TranscodePlan::MdToGeminiCommandToml => {
                CopyMode::Transcode
            }
        }
    }
}

/// Per-row outcome. Serializes to `NgwaCopyRowResult` — the row identity
/// (`engine`/`scope`/`mode`) is flattened alongside the `ok` discriminant so the
/// wire shape matches the FE union exactly.
#[derive(Debug, Clone, Serialize)]
struct NgwaCopyRowResult {
    engine: String,
    scope: String,
    mode: CopyMode,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mutation: Option<ClaudeStoreMutation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<StoreError>,
}

/// Batch result. Serializes to `NgwaCopyBatchResult { rows }`.
#[derive(Debug, Clone, Serialize)]
pub struct NgwaCopyBatchResult {
    rows: Vec<NgwaCopyRowResult>,
}

/// Map a wire scope string to the resolved scope ROOT (parent of the engine
/// dotdir), reusing the WP-23 resolver. Workspace → `~`; project:<id> → project
/// root_path.
async fn resolve_dest_scope_root(db: &Arc<PaDb>, scope: &str) -> Result<PathBuf, String> {
    resolve_scope_root_dir(db, scope).await
}

/// Copy (or move) one source primitive into N `(engine, scope)` destinations in
/// one batch. Per the directionality contract (`06-cross-engine-transcode.md`):
/// same-format destinations copy verbatim; MD→TOML destinations transcode via the
/// existing forward transcoder; TOML→MD destinations are blocked with a typed
/// `TranscodeUnsupported` error BEFORE any write. Each destination writes
/// atomically and reports its own row; a partial failure never rolls back the
/// rows that succeeded. For `move`, the source is removed only AFTER the batch,
/// and only if at least one destination succeeded (a total failure degrades to a
/// no-op, never to loss).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_copy_batch(
    db: State<'_, Arc<PaDb>>,
    fromEngine: String,
    kind: String,
    name: String,
    fromScope: String,
    destinations: Vec<NgwaCopyDestination>,
    #[allow(non_snake_case)] r#move: bool,
) -> Result<NgwaCopyBatchResult, String> {
    let from_engine = parse_engine(&fromEngine)?;
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;

    // Resolve the SOURCE once: its real (symlink-followed) on-disk path in the
    // from-engine's from-scope. A missing source fails the whole batch up front
    // (nothing to copy) rather than per-row.
    let from_scope_root = resolve_scope_root_dir(&db, &fromScope).await?;
    let user_home = user_home_dir()?;
    let from_cell = file_layout_cell(from_engine, k)?;
    let src = engine_file_path(
        from_engine,
        &from_cell,
        &from_scope_root,
        &user_home,
        k,
        &name,
    )?;
    if !src.exists() {
        return Err(format!(
            "cross-engine copy source missing: {} ({fromEngine} {} {name:?} in {fromScope})",
            src.display(),
            k.as_str()
        ));
    }
    let from_resolved = std::fs::canonicalize(&src)
        .map_err(|e| format!("resolve source {}: {e}", src.display()))?;

    let mut rows: Vec<NgwaCopyRowResult> = Vec::with_capacity(destinations.len());
    let mut any_ok = false;

    for dest in &destinations {
        // Resolve the target engine + scope root per row; a bad engine/scope is a
        // per-row error (the batch keeps going for the other destinations).
        let to_engine = match parse_engine(&dest.engine) {
            Ok(e) => e,
            Err(message) => {
                rows.push(NgwaCopyRowResult {
                    engine: dest.engine.clone(),
                    scope: dest.scope.clone(),
                    mode: CopyMode::Blocked,
                    ok: false,
                    mutation: None,
                    error: Some(StoreError::Unsupported { message }),
                });
                continue;
            }
        };

        // Resolve the plan first so a blocked direction echoes mode=blocked and
        // never touches disk.
        let plan = match resolve_transcode_plan(from_engine, to_engine, k) {
            Ok(p) => p,
            Err(err) => {
                rows.push(NgwaCopyRowResult {
                    engine: dest.engine.clone(),
                    scope: dest.scope.clone(),
                    mode: CopyMode::Blocked,
                    ok: false,
                    mutation: None,
                    error: Some(err),
                });
                continue;
            }
        };
        let mode = CopyMode::from_plan(plan);

        let to_scope_root = match resolve_dest_scope_root(&db, &dest.scope).await {
            Ok(r) => r,
            Err(message) => {
                rows.push(NgwaCopyRowResult {
                    engine: dest.engine.clone(),
                    scope: dest.scope.clone(),
                    mode,
                    ok: false,
                    mutation: None,
                    error: Some(StoreError::Unsupported { message }),
                });
                continue;
            }
        };

        match copy_cross_engine_row(
            from_engine,
            to_engine,
            k,
            &name,
            &from_resolved,
            &dest.scope,
            &to_scope_root,
            &user_home,
        ) {
            Ok(mutation) => {
                any_ok = true;
                rows.push(NgwaCopyRowResult {
                    engine: dest.engine.clone(),
                    scope: dest.scope.clone(),
                    mode,
                    ok: true,
                    mutation: Some(mutation),
                    error: None,
                });
            }
            Err(err) => rows.push(NgwaCopyRowResult {
                engine: dest.engine.clone(),
                scope: dest.scope.clone(),
                mode,
                ok: false,
                mutation: None,
                error: Some(err),
            }),
        }
    }

    // Move semantics: drop the source only after the batch, and only if at least
    // one destination landed — so a total failure degrades to a no-op (copy
    // safety: never lose the source to a failed move).
    if r#move && any_ok {
        let _ = remove_for_core(from_engine, &from_scope_root, &user_home, k, &name);
        let pool = db.ensure_pool().await?;
        clear_pin_for(&pool, &fromScope, k.as_str(), &name).await?;
    }

    Ok(NgwaCopyBatchResult { rows })
}

// ─── WP-04 — registry-aware safe-delete command layer ─────────────────────────

/// Per-link result of a relink-all (`oba_relink_dependents`). Mirrors
/// `ObaRelinkRow` in `tauri-cmd.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelinkRow {
    pub link: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Candidate dirs (across scope roots × engines) where a dependent symlink for
/// `kind` could live. Pure path math mirroring the `EngineLayout` location
/// templates (`engine_layout.rs`); `scan_live_dependents` then checks which
/// entries actually resolve into the master. Non-existent dirs are harmless —
/// the scan skips them.
fn dependent_search_dirs(scope_roots: &[PathBuf], kind: Kind) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for root in scope_roots {
        match kind {
            Kind::Skill => {
                dirs.push(root.join(".claude/skills"));
                dirs.push(root.join(".agents/skills")); // cross-tool (Gemini + Codex)
                dirs.push(root.join(".gemini/skills"));
            }
            Kind::Agent => {
                dirs.push(root.join(".claude/agents"));
                dirs.push(root.join(".gemini/agents"));
                dirs.push(root.join(".codex/agents"));
            }
            Kind::Command => {
                dirs.push(root.join(".claude/commands"));
                dirs.push(root.join(".gemini/commands"));
                dirs.push(root.join(".codex/prompts"));
            }
            // hook/mcp are merge-based — no symlink placements to depend on a master.
            Kind::Hook | Kind::Mcp => {}
            // A bundle is never placed into a scope as itself — only its member
            // skills are (WP-21) — so there are no bundle placements that could
            // depend on the bundle master. The members' own placements are
            // tracked under the Skill arm.
            Kind::Bundle => {}
        }
    }
    dirs
}

/// Every scope root the dependents scan should sweep: the workspace root, each
/// project root, and the user home (for user-tier `~/.claude`, `~/.agents`, …).
async fn all_scope_roots(db: &Arc<PaDb>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(r) = resolve_scope_root_dir(db, "workspace").await {
        roots.push(r);
    }
    if let Ok(pool) = db.ensure_pool().await {
        if let Ok(projects) = crate::commands::projects::list_projects(&pool, false).await {
            for p in projects {
                if let Some(root) = p.root_path.as_deref() {
                    if let Ok(x) = expand(root) {
                        roots.push(x);
                    }
                }
            }
        }
    }
    if let Ok(home) = user_home_dir() {
        roots.push(home);
    }
    roots
}

/// Resolve the canonical master path + `managed` for (kind, name): the registry
/// record if present, else a synthesized `local` entry at the store path.
fn resolve_canonical(store: &Path, kind: Kind, name: &str) -> (PathBuf, bool) {
    let map = registry::provenance_map(&registry::load(store));
    if let Some(p) = map.get(&(kind.as_str().to_string(), name.to_string())) {
        return (PathBuf::from(&p.canonical_path), p.managed);
    }
    let canonical = store_path_for(store, kind, name).unwrap_or_default();
    (canonical, true) // a plain vault entry is managed
}

/// WP-04: live dependents of (kind, name)'s canonical master, for the UI's
/// dependents list. Computed fresh from disk — never a stored list.
#[tauri::command]
pub async fn oba_dependents(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<Vec<String>, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let (canonical, _managed) = resolve_canonical(&store, k, &name);
    let roots = all_scope_roots(&db).await;
    // WP-21: a bundle is never placed as itself, so its dependents are the scope
    // links of its MEMBER skills (scanned per member subdir). All other kinds use
    // the canonical + the kind's own search dirs.
    let deps = if k == Kind::Bundle {
        bundle_member_dependents(&store, &canonical, &name, &roots)
    } else {
        let dirs = dependent_search_dirs(&roots, k);
        crate::commands::claude_config::scan_live_dependents(&canonical, &dirs)
    };
    Ok(deps.into_iter().map(|p| p.display().to_string()).collect())
}

/// WP-04: guarded delete of (kind, name)'s canonical master. Refuses external
/// masters and masters with live dependents; hard-deletes only a managed master
/// with zero dependents. NEVER `remove_dir_all`s out from under dependents (the
/// incident guardrail). On a successful hard-delete, drops the registry record.
#[tauri::command]
pub async fn oba_safe_delete(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<SafeDeleteOutcome, String> {
    tracing::info!("[oba_safe_delete] enter kind={kind} name={name}");
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    tracing::info!("[oba_safe_delete] store={}", store.display());
    let (canonical, managed) = resolve_canonical(&store, k, &name);
    tracing::info!(
        "[oba_safe_delete] canonical={} managed={managed}",
        canonical.display()
    );
    let roots = all_scope_roots(&db).await;
    tracing::info!("[oba_safe_delete] scope_roots={}", roots.len());
    // WP-21: a bundle's dependents are its MEMBER skill scope links (the bundle
    // itself is never placed). `dependent_search_dirs(Kind::Bundle)` is empty, so
    // we MUST scan per member subdir — otherwise a managed bundle always reads
    // zero dependents and gets blind-`remove_dir_all`'d out from under live
    // member links. That is the incident guardrail for bundles.
    let deps = if k == Kind::Bundle {
        bundle_member_dependents(&store, &canonical, &name, &roots)
    } else {
        let dirs = dependent_search_dirs(&roots, k);
        tracing::info!("[oba_safe_delete] dependent dirs={}", dirs.len());
        crate::commands::claude_config::scan_live_dependents(&canonical, &dirs)
    };
    tracing::info!("[oba_safe_delete] live dependents={}", deps.len());
    let outcome = guarded_delete(&canonical, k, managed, &deps)?;
    tracing::info!("[oba_safe_delete] verdict={}", outcome.verdict);
    if outcome.removed {
        let mut rf = registry::load(&store);
        rf.entries
            .retain(|e| !(e.kind == k.as_str() && e.name == name));
        let _ = registry::save(&store, &rf);
    }
    Ok(outcome)
}

/// WP-04: re-point dependent symlinks at a new master (relink-all), returning a
/// per-link result in request order. Used before forgetting an external master.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_relink_dependents(
    dependents: Vec<String>,
    newMaster: String,
) -> Result<Vec<RelinkRow>, String> {
    let target = expand(&newMaster).map_err(|e| e.to_string())?;
    if !target.exists() {
        return Err(format!("new master does not exist: {newMaster}"));
    }
    Ok(dependents
        .iter()
        .map(|d| {
            let link = PathBuf::from(d);
            match relink_one(&link, &target) {
                Ok(()) => RelinkRow {
                    link: d.clone(),
                    ok: true,
                    error: None,
                },
                Err(e) => RelinkRow {
                    link: d.clone(),
                    ok: false,
                    error: Some(e),
                },
            }
        })
        .collect())
}

/// Unlink a single dependent placement by absolute path: `remove_file` iff it
/// is a symlink, refuse otherwise (we never delete a real file/dir this way —
/// that path is the safe-delete guard's job). Idempotent on a missing path.
pub(crate) fn unlink_placement(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            std::fs::remove_file(path)
                .map_err(|e| format!("remove_file {}: {e}", path.display()))?;
            Ok(true)
        }
        Ok(_) => Err(format!(
            "refusing to unlink {}: not a symlink (use safe-delete for a real master)",
            path.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("lstat {}: {e}", path.display())),
    }
}

/// WP-06: unlink one dependent placement (a symlink) by absolute path. Always
/// safe — `remove_file` never recurses into the master. Refuses a non-symlink.
#[tauri::command]
pub async fn oba_unlink_one(path: String) -> Result<bool, String> {
    let p = expand(&path).map_err(|e| e.to_string())?;
    unlink_placement(&p)
}

/// WP-06: drop the registry record for `(kind, name)` — provenance only. Touches
/// no files: the master + every symlink stay on disk exactly as they are. Backs
/// the D-01 "Forget from registry" choice. Returns `true` iff a record existed.
#[tauri::command]
pub async fn oba_forget(kind: String, name: String) -> Result<bool, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let mut rf = registry::load(&store);
    let removed = registry::forget(&mut rf, k.as_str(), &name);
    if removed {
        registry::save(&store, &rf)?;
    }
    Ok(removed)
}

/// Discover EXTERNAL masters by walking the live farm: for every dependent
/// symlink across the scope×engine placement dirs, resolve its target; if the
/// target exists and sits OUTSIDE the vault `store`, that target is a real
/// external master kept in place (the `groundwork` shape). Returns one
/// `(kind, name, canonical)` per distinct external master. Pure + tempdir-
/// testable: `scope_roots` + `store` are parameters. Dependents are NOT
/// returned/stored — they stay live-computed at delete time (`G-SCHEMA`).
pub(crate) fn scan_external_masters(
    store: &Path,
    scope_roots: &[PathBuf],
) -> Vec<(Kind, String, PathBuf)> {
    let store_canon = std::fs::canonicalize(store).unwrap_or_else(|_| store.to_path_buf());
    // Keyed (kind, name) so duplicate dependents collapse to one master record.
    let mut found: std::collections::BTreeMap<(String, String), PathBuf> =
        std::collections::BTreeMap::new();
    for kind in [Kind::Skill, Kind::Agent, Kind::Command] {
        for dir in dependent_search_dirs(scope_roots, kind) {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let is_link = std::fs::symlink_metadata(&p)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
                if !is_link {
                    continue;
                }
                let Some(target) = crate::commands::claude_config::resolve_symlink_target(&p)
                else {
                    continue; // dangling — not a master
                };
                if target.starts_with(&store_canon) {
                    continue; // vault-managed; not external
                }
                // Name from the link's own basename (kind-aware), matching the
                // farm's naming. Skip anything that fails the confinement check.
                let fname = e.file_name().to_string_lossy().to_string();
                let name = if kind.is_dir_primitive() {
                    fname
                } else {
                    match fname.strip_suffix(".md") {
                        Some(stem) => stem.to_string(),
                        None => continue,
                    }
                };
                if validate_name(&name).is_err() {
                    continue;
                }
                found
                    .entry((kind.as_str().to_string(), name))
                    .or_insert(target);
            }
        }
    }
    found
        .into_iter()
        .filter_map(|((k, n), c)| Kind::parse(&k).ok().map(|k| (k, n, c)))
        .collect()
}

/// WP-06: back-fill the registry with external masters discovered in the live
/// farm (`managed:false`, real `canonicalPath`). Makes `oba_safe_delete`
/// resolve a real external canonical (so it can hit `refused_external` /
/// `refused_dependents`) instead of a nonexistent vault path. Returns the number
/// of external-master records added or updated.
#[tauri::command]
pub async fn oba_backfill_registry(db: State<'_, Arc<PaDb>>) -> Result<usize, String> {
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let roots = all_scope_roots(&db).await;
    let externals = scan_external_masters(&store, &roots);
    tracing::info!(
        "[oba_backfill_registry] external masters found={}",
        externals.len()
    );
    let mut rf = registry::load(&store);
    let mut n = 0usize;
    for (kind, name, canonical) in externals {
        if registry::upsert_external(&mut rf, kind.as_str(), &name, &canonical.to_string_lossy()) {
            n += 1;
        }
    }
    if n > 0 {
        registry::save(&store, &rf)?;
    }
    tracing::info!("[oba_backfill_registry] recorded={n}");
    Ok(n)
}

// ─── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ─── G-SCHEMA (WP-01) — registry provenance contract ─────────────────────

    #[test]
    fn provenance_back_compat_deserializes_legacy_entry() {
        // A ClaudeStoreEntry serialized BEFORE the registry existed (no
        // provenance keys) must deserialize as a synthesized local, managed
        // entry — not fail. This is the back-compat half of G-SCHEMA's DoD.
        let legacy = r#"{
            "kind":"skill","name":"groundwork","storePath":"/s/skills/groundwork",
            "description":null,"modifiedMs":0,"enabledIn":[]
        }"#;
        let e: ClaudeStoreEntry = serde_json::from_str(legacy).unwrap();
        assert_eq!(e.provenance.source, ProvenanceSource::Local);
        assert!(e.provenance.managed, "legacy entries default to managed");
        assert!(e.provenance.url.is_none());
        assert!(e.provenance.r#ref.is_none());
        // canonical_path is empty on legacy data; WP-02's load normalizes it.
        assert_eq!(e.provenance.canonical_path, "");
    }

    #[test]
    fn provenance_serializes_with_camelcase_inline_keys() {
        // Field names + flatten must match the tauri-cmd.ts mirror exactly.
        let e = ClaudeStoreEntry {
            kind: "skill".into(),
            name: "huashu".into(),
            store_path: "/s/skills/huashu".into(),
            description: None,
            modified_ms: 0,
            enabled_in: vec![],
            requires: vec![],
            members: vec![],
            provenance: RegistryProvenance {
                source: ProvenanceSource::Git,
                url: Some("github:obra/huashu".into()),
                r#ref: Some("v2.1.0".into()),
                version: Some("abc123".into()),
                canonical_path: "/s/skills/huashu".into(),
                managed: true,
                installed_at: None,
                updated_at: None,
                from_catalog: false,
                auto_update: false,
            },
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        // flattened inline (not nested under "provenance")
        assert_eq!(v["source"], "git");
        assert_eq!(v["ref"], "v2.1.0");
        assert_eq!(v["canonicalPath"], "/s/skills/huashu");
        assert_eq!(v["managed"], true);
        assert!(
            v.get("provenance").is_none(),
            "provenance must be flattened"
        );
        // round-trips
        let back: ClaudeStoreEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn registry_file_defaults_to_v2_empty() {
        // WP-11 bumped the current registry schema to v2 (additive `requires`).
        let rf = RegistryFile::default();
        assert_eq!(rf.schema_version, 2);
        assert!(rf.entries.is_empty());
        let v: serde_json::Value = serde_json::to_value(&rf).unwrap();
        assert_eq!(v["schemaVersion"], 2);
    }

    #[test]
    fn entry_requires_round_trips_and_defaults_empty() {
        // An entry WITH requires serde-round-trips; the `requires` field rides
        // as a sibling array, NOT under the flattened provenance.
        let e = ClaudeStoreEntry {
            kind: "skill".into(),
            name: "studio".into(),
            store_path: "/s/skills/studio".into(),
            description: None,
            modified_ms: 0,
            enabled_in: vec![],
            requires: vec![
                RequiresEntry {
                    kind: "skill".into(),
                    name: "skill-core".into(),
                    source: Some(crate::pkg::manifest::RequireSource::Npx),
                    r#ref: None,
                },
            ],
            members: vec![],
            provenance: RegistryProvenance::local("/s/skills/studio".into()),
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["requires"][0]["name"], "skill-core");
        assert_eq!(v["requires"][0]["source"], "npx");
        // requires is a sibling, not nested under provenance (which is flattened)
        assert!(v.get("provenance").is_none());
        let back: ClaudeStoreEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);

        // A legacy entry JSON without `requires` deserializes with an empty Vec.
        let legacy = r#"{"kind":"skill","name":"x","storePath":"/s/skills/x",
            "description":null,"modifiedMs":0,"enabledIn":[]}"#;
        let le: ClaudeStoreEntry = serde_json::from_str(legacy).unwrap();
        assert!(le.requires.is_empty());
    }

    #[test]
    fn bundle_entry_round_trips_with_members() {
        // WP-18 (G-BUNDLE) test (a): a bundle registry/store entry with a
        // populated `members[]` serde-round-trips; `members` rides as a SIBLING
        // array (not nested under the flattened provenance, same as `requires`).
        let e = ClaudeStoreEntry {
            kind: "bundle".into(),
            name: "studio-archetypes".into(),
            store_path: "/s/bundles/studio-archetypes".into(),
            description: Some("9 studio archetype skills".into()),
            modified_ms: 0,
            enabled_in: vec![],
            requires: vec![],
            members: vec!["archetype-beat".into(), "archetype-mix".into()],
            provenance: RegistryProvenance::local("/s/bundles/studio-archetypes".into()),
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "bundle");
        assert_eq!(v["members"][0], "archetype-beat");
        assert_eq!(v["members"][1], "archetype-mix");
        // sibling of the flattened provenance, not nested under it
        assert!(v.get("provenance").is_none());
        let back: ClaudeStoreEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);

        // WP-18 test (b): a legacy entry JSON WITHOUT `members` (the pre-WP-18
        // shape — every non-bundle entry on disk today) still parses, with
        // `members` defaulting to [] via `#[serde(default)]`. Back-compat proof.
        let legacy = r#"{"kind":"skill","name":"x","storePath":"/s/skills/x",
            "description":null,"modifiedMs":0,"enabledIn":[],"requires":[]}"#;
        let le: ClaudeStoreEntry = serde_json::from_str(legacy).unwrap();
        assert!(le.members.is_empty(), "absent members defaults to []");
    }

    // ─── WP-04 — dependent-aware safe delete (incident guardrail) ─────────────

    #[test]
    fn delete_verdict_policy_table() {
        use DeleteVerdict::*;
        // a symlink is always just a placement, whatever else is true
        assert_eq!(delete_verdict(true, true, 0), UnlinkPlacement);
        assert_eq!(delete_verdict(true, false, 9), UnlinkPlacement);
        // a real external master is never hard-deleted
        assert_eq!(delete_verdict(false, false, 0), RefuseExternal);
        assert_eq!(delete_verdict(false, false, 3), RefuseExternal);
        // a real managed master with dependents is refused (relink first)
        assert_eq!(delete_verdict(false, true, 1), RefuseHasDependents);
        // only a managed master with zero dependents may be hard-deleted
        assert_eq!(delete_verdict(false, true, 0), HardDelete);
    }

    /// Lay out a real master dir + `n` symlinks (across fake scope dirs) that
    /// resolve into it. Returns (root, master, search_dirs, link_paths).
    fn dependents_fixture(tag: &str, n: usize) -> (PathBuf, PathBuf, Vec<PathBuf>, Vec<PathBuf>) {
        let root = unique_tmp(tag);
        let master = root.join("ikenga/.claude/skills/groundwork");
        std::fs::create_dir_all(&master).unwrap();
        std::fs::write(master.join("SKILL.md"), "---\nname: groundwork\n---\n").unwrap();
        let mut search_dirs = Vec::new();
        let mut links = Vec::new();
        for i in 0..n {
            let dir = root.join(format!("scope{i}/skills"));
            std::fs::create_dir_all(&dir).unwrap();
            let link = dir.join("groundwork");
            symlink_impl(&master, &link).unwrap();
            search_dirs.push(dir);
            links.push(link);
        }
        (root, master, search_dirs, links)
    }

    #[test]
    fn incident_regression_external_master_with_dependents_is_never_wiped() {
        // The exact shape of the data-loss incident: an EXTERNAL master
        // (managed:false) with 3 live dependent symlinks. Deleting it must
        // REFUSE — no remove_dir_all — and leave the master + every symlink
        // intact. This is the test that must pass before any UI wires to delete.
        let (root, master, search_dirs, links) = dependents_fixture("incident", 3);

        let deps = crate::commands::claude_config::scan_live_dependents(&master, &search_dirs);
        assert_eq!(deps.len(), 3, "all 3 symlinks resolve into the master");

        let outcome = guarded_delete(&master, Kind::Skill, /*managed=*/ false, &deps).unwrap();
        assert_eq!(outcome.verdict, "refused_external");
        assert!(!outcome.removed);
        assert_eq!(outcome.dependents.len(), 3);

        // The master and every dependent must STILL be on disk + resolving.
        assert!(master.join("SKILL.md").exists(), "master must survive");
        for l in &links {
            assert!(
                std::fs::canonicalize(l).is_ok(),
                "dependent {} must still resolve",
                l.display()
            );
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn guarded_delete_managed_master_with_dependents_refuses_relink() {
        let (root, master, search_dirs, _links) = dependents_fixture("managed_deps", 2);
        let deps = crate::commands::claude_config::scan_live_dependents(&master, &search_dirs);
        let outcome = guarded_delete(&master, Kind::Skill, true, &deps).unwrap();
        assert_eq!(outcome.verdict, "refused_dependents");
        assert!(!outcome.removed);
        assert!(master.exists(), "master survives a refused delete");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn guarded_delete_unlinks_a_placement_symlink_leaving_master() {
        let (root, master, _sd, links) = dependents_fixture("unlink", 1);
        let link = &links[0];
        // deleting the SYMLINK (a placement) is always safe regardless of managed
        let outcome = guarded_delete(link, Kind::Skill, false, &[]).unwrap();
        assert_eq!(outcome.verdict, "unlinked");
        assert!(outcome.removed);
        assert!(
            std::fs::symlink_metadata(link).is_err(),
            "placement removed"
        );
        assert!(master.join("SKILL.md").exists(), "master untouched");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn guarded_delete_hard_deletes_managed_master_with_no_dependents() {
        let (root, master, _sd, _l) = dependents_fixture("hard", 0);
        let outcome = guarded_delete(&master, Kind::Skill, true, &[]).unwrap();
        assert_eq!(outcome.verdict, "deleted");
        assert!(outcome.removed);
        assert!(!master.exists(), "managed master with no deps is removed");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dependent_search_dirs_cover_cross_engine_placements() {
        let root = PathBuf::from("/w");
        // skills: claude + cross-tool .agents + gemini
        let s = dependent_search_dirs(&[root.clone()], Kind::Skill);
        assert!(s.contains(&root.join(".claude/skills")));
        assert!(s.contains(&root.join(".agents/skills")));
        // agents: per-engine
        let a = dependent_search_dirs(&[root.clone()], Kind::Agent);
        assert!(a.contains(&root.join(".claude/agents")));
        assert!(a.contains(&root.join(".codex/agents")));
        // merge-based kinds have no symlink placements
        assert!(dependent_search_dirs(&[root], Kind::Mcp).is_empty());
    }

    #[test]
    fn relink_one_repoints_atomically_and_refuses_real_paths() {
        let root = unique_tmp("relink");
        let master_a = root.join("a");
        let master_b = root.join("b");
        std::fs::create_dir_all(&master_a).unwrap();
        std::fs::create_dir_all(&master_b).unwrap();
        let link = root.join("scope/groundwork");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink_impl(&master_a, &link).unwrap();

        relink_one(&link, &master_b).unwrap();
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&master_b).unwrap(),
            "link now resolves to the new master"
        );
        assert!(master_a.exists(), "old master untouched by relink");

        // never overwrite a real path
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        assert!(relink_one(&real, &master_b).is_err(), "refuses non-symlink");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unlink_placement_removes_symlink_refuses_real_idempotent_on_missing() {
        let (root, master, _sd, links) = dependents_fixture("unlink_one", 1);
        let link = &links[0];
        // symlink → removed, master untouched
        assert!(unlink_placement(link).unwrap());
        assert!(std::fs::symlink_metadata(link).is_err(), "placement gone");
        assert!(master.join("SKILL.md").exists(), "master untouched");
        // missing → idempotent (false, not error)
        assert!(!unlink_placement(link).unwrap());
        // real dir → refused (never delete a real master this way)
        assert!(unlink_placement(&master).is_err(), "refuses non-symlink");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_external_masters_finds_external_skips_vault() {
        // root/ has: an EXTERNAL master (outside the vault store) with 2 symlink
        // dependents in fake scope dirs, AND a vault store with its own master +
        // a symlink resolving INTO the vault. Only the external master is
        // discovered; the vault-internal symlink is skipped.
        let root = unique_tmp("scan_ext");
        let store = root.join("store");
        std::fs::create_dir_all(store.join("skills")).unwrap();

        // External master kept in place (the groundwork shape)
        let ext_master = root.join("repo/.claude/skills/groundwork");
        std::fs::create_dir_all(&ext_master).unwrap();
        std::fs::write(ext_master.join("SKILL.md"), "---\nname: groundwork\n---\n").unwrap();

        // Vault-managed master (lives under store/)
        let vault_master = store.join("skills/release-status");
        std::fs::create_dir_all(&vault_master).unwrap();
        std::fs::write(vault_master.join("SKILL.md"), "x").unwrap();

        // Two scope roots, each with a .claude/skills holding both a dependent of
        // the external master and a dependent of the vault master.
        let mut scope_roots = Vec::new();
        for i in 0..2 {
            let sr = root.join(format!("scope{i}"));
            let skills = sr.join(".claude/skills");
            std::fs::create_dir_all(&skills).unwrap();
            symlink_impl(&ext_master, &skills.join("groundwork")).unwrap();
            symlink_impl(&vault_master, &skills.join("release-status")).unwrap();
            scope_roots.push(sr);
        }

        let found = scan_external_masters(&store, &scope_roots);
        assert_eq!(found.len(), 1, "only the external master is discovered");
        let (kind, name, canonical) = &found[0];
        assert_eq!(*kind, Kind::Skill);
        assert_eq!(name, "groundwork");
        assert_eq!(
            std::fs::canonicalize(canonical).unwrap(),
            std::fs::canonicalize(&ext_master).unwrap(),
            "canonical is the real external master"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    fn unique_tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("ngwa_wp02_{tag}_{nonce}_{:p}", &nonce as *const _));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Build a (store, scope_claude) pair under a fresh tmp dir. The scope
    /// `.claude` dir sits under a `.claude` segment so the confinement guard
    /// accepts it without needing the real env-derived store root.
    fn fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = unique_tmp(tag);
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let scope = base.join("proj").join(".claude");
        std::fs::create_dir_all(&scope).unwrap();
        (base, store, scope)
    }

    fn seed_agent(store: &Path, name: &str, desc: &str) {
        let p = store_path_for(store, Kind::Agent, name).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody"),
        )
        .unwrap();
    }

    fn seed_skill(store: &Path, name: &str, desc: &str) {
        let dir = store_path_for(store, Kind::Skill, name).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody"),
        )
        .unwrap();
        std::fs::write(dir.join("helper.py"), "print(1)\n").unwrap();
    }

    /// Seed a bundle canonical at `store/bundles/<name>/` holding one member skill
    /// subdir per leaf (each a real dir with its own SKILL.md). WP-21 fixtures use
    /// the disk fallback in `bundle_member_leaves` — no registry write needed.
    fn seed_bundle(store: &Path, name: &str, members: &[&str]) {
        let dir = store_path_for(store, Kind::Bundle, name).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        for m in members {
            let md = dir.join(m);
            std::fs::create_dir_all(&md).unwrap();
            std::fs::write(
                md.join("SKILL.md"),
                format!("---\nname: {m}\ndescription: member of {name}\n---\nbody"),
            )
            .unwrap();
        }
    }

    // ── name validation ──────────────────────────────────────────────────

    #[test]
    fn name_validation_rejects_traversal_and_separators() {
        assert!(validate_name("good-name").is_ok());
        assert!(validate_name("with_underscore").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("../escape").is_err());
        assert!(validate_name(&"x".repeat(257)).is_err());
    }

    #[test]
    fn kind_parse_and_file_based() {
        assert!(Kind::parse("agent").unwrap().is_file_based());
        assert!(Kind::parse("skill").unwrap().is_file_based());
        assert!(Kind::parse("command").unwrap().is_file_based());
        assert!(!Kind::parse("hook").unwrap().is_file_based());
        assert!(!Kind::parse("mcp").unwrap().is_file_based());
        assert!(Kind::parse("nope").is_err());
    }

    #[test]
    fn bundle_kind_schema_is_frozen() {
        // WP-18 (G-BUNDLE): the Kind::Bundle schema — kind-string, dir-name,
        // dir-primitive-ness, and the store/scope path math — is frozen here.
        let b = Kind::parse("bundle").expect("bundle parses");
        assert_eq!(b, Kind::Bundle);
        assert_eq!(b.as_str(), "bundle");
        // round-trips through the kind-string
        assert_eq!(Kind::parse(b.as_str()).unwrap(), b);
        // directory primitive: canonical lives at store/bundles/<name>/
        assert_eq!(b.dir_name(), Some("bundles"));
        assert!(b.is_dir_primitive(), "a bundle's store copy is a directory");
        // NOT file-based: never enters the symlink farm / per-engine transcode.
        assert!(!b.is_file_based());

        // store_path_for routes to <store>/bundles/<name> (a directory leaf,
        // no `.md` suffix because it's a dir primitive).
        let store = PathBuf::from("/s");
        let sp = store_path_for(&store, Kind::Bundle, "studio-archetypes").unwrap();
        assert_eq!(sp, PathBuf::from("/s/bundles/studio-archetypes"));

        // scope_path_for refuses a bundle: bundles place their members, not the
        // bundle itself (WP-21) — a descriptive Err, never a bogus path.
        let scope = PathBuf::from("/proj/.claude");
        let err = scope_path_for(&scope, Kind::Bundle, "studio-archetypes").unwrap_err();
        assert!(err.contains("WP-21"), "descriptive scope-path Err: {err}");

        // primitive_kind_for refuses a bundle (it's not a file-based engine leaf).
        let pk_err = primitive_kind_for(Kind::Bundle).unwrap_err();
        assert!(pk_err.contains("WP-21"), "descriptive primitive-kind Err: {pk_err}");

        // a bundle contributes no dependent search dirs (members are placed, not
        // the bundle).
        let root = PathBuf::from("/r");
        assert!(dependent_search_dirs(&[root], Kind::Bundle).is_empty());
    }

    // ── WP-21: bundle placement / collision / disable / safe-delete ──────────

    /// (a) Enabling a bundle places ALL members as scope skill symlinks, each
    /// resolving to `store/bundles/<name>/<member>`, reachable through the link.
    #[test]
    fn place_bundle_places_all_members() {
        let (base, store, scope) = fixture("bundle_enable");
        seed_bundle(&store, "atelier", &["archetype-beat", "archetype-mix"]);

        let m = place_bundle(&store, &scope, "workspace", "atelier").unwrap();
        // Synthetic bundle-level mutation: kind=bundle, path=canonical, no link.
        // (Intentional divergence from other kinds where `path` is the scope link
        // — a bundle has no single scope link; its members are the placed links.)
        assert_eq!(m.kind, "bundle");
        assert_eq!(m.name, "atelier");
        assert!(m.link_target.is_none());
        assert_eq!(
            m.path,
            store_path_for(&store, Kind::Bundle, "atelier")
                .unwrap()
                .to_string_lossy()
                .to_string(),
            "bundle mutation.path is the canonical bundle dir (documented divergence)"
        );

        for member in ["archetype-beat", "archetype-mix"] {
            let link = scope_path_for(&scope, Kind::Skill, member).unwrap();
            let meta = std::fs::symlink_metadata(&link).unwrap();
            assert!(meta.file_type().is_symlink(), "{member} placed as a symlink");
            // Resolves into the bundle member canonical.
            let want = store_path_for(&store, Kind::Bundle, "atelier")
                .unwrap()
                .join(member);
            assert_eq!(
                std::fs::canonicalize(&link).unwrap(),
                std::fs::canonicalize(&want).unwrap(),
                "{member} link resolves to store/bundles/atelier/{member}"
            );
            // SKILL.md reachable through the link.
            assert!(link.join("SKILL.md").exists());
        }
        // Idempotent re-enable is a no-op (no clobber, no error).
        place_bundle(&store, &scope, "workspace", "atelier").unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    /// (b) A member-leaf collision is first-wins: a pre-existing foreign link is
    /// left untouched, the colliding member is skipped, and the rest still place.
    #[test]
    fn member_collision_first_wins_no_clobber() {
        let (base, store, scope) = fixture("bundle_collision");
        seed_bundle(&store, "atelier", &["shared-leaf", "unique-leaf"]);
        // A different canonical that an existing placement already points at.
        let foreign = store.join("other-canonical");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("SKILL.md"), "---\nname: shared-leaf\n---\n").unwrap();
        // Pre-plant the foreign link at the colliding member's scope path.
        let collide_link = scope_path_for(&scope, Kind::Skill, "shared-leaf").unwrap();
        std::fs::create_dir_all(collide_link.parent().unwrap()).unwrap();
        symlink_impl(&foreign, &collide_link).unwrap();

        // Enable still succeeds.
        place_bundle(&store, &scope, "workspace", "atelier").unwrap();

        // First-wins: the colliding link STILL points at the foreign canonical
        // (NOT clobbered to the bundle member).
        assert_eq!(
            std::fs::canonicalize(&collide_link).unwrap(),
            std::fs::canonicalize(&foreign).unwrap(),
            "colliding leaf kept the first (foreign) owner — no clobber"
        );
        // The non-colliding member WAS placed into the bundle canonical.
        let ok_link = scope_path_for(&scope, Kind::Skill, "unique-leaf").unwrap();
        let want = store_path_for(&store, Kind::Bundle, "atelier")
            .unwrap()
            .join("unique-leaf");
        assert_eq!(
            std::fs::canonicalize(&ok_link).unwrap(),
            std::fs::canonicalize(&want).unwrap(),
            "the rest of the bundle still placed"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// (c) Disabling a bundle removes the member links it owns — and ONLY those.
    /// A foreign leaf (first-wins winner from another owner) survives untouched.
    #[test]
    fn disable_bundle_removes_own_links_only() {
        let (base, store, scope) = fixture("bundle_disable");
        seed_bundle(&store, "atelier", &["mine-a", "mine-b", "foreign-leaf"]);
        // Place the whole bundle.
        place_bundle(&store, &scope, "workspace", "atelier").unwrap();
        // Now stage a foreign owner for `foreign-leaf`: point it elsewhere so the
        // bundle does NOT own that link anymore.
        let foreign = store.join("foreign-canonical");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("SKILL.md"), "---\nname: foreign-leaf\n---\n").unwrap();
        let foreign_link = scope_path_for(&scope, Kind::Skill, "foreign-leaf").unwrap();
        make_symlink(&foreign, &foreign_link).unwrap();

        disable_bundle(&store, &scope, "atelier").unwrap();

        // The bundle's own member links are gone.
        for member in ["mine-a", "mine-b"] {
            let link = scope_path_for(&scope, Kind::Skill, member).unwrap();
            assert!(
                std::fs::symlink_metadata(&link).is_err(),
                "{member} link removed by disable"
            );
        }
        // The foreign link (now pointing elsewhere) is UNTOUCHED.
        assert_eq!(
            std::fs::canonicalize(&foreign_link).unwrap(),
            std::fs::canonicalize(&foreign).unwrap(),
            "foreign-owned leaf survives — disable removes only this bundle's links"
        );
        // The store canonical (bundle dir + members) is intact.
        let canonical = store_path_for(&store, Kind::Bundle, "atelier").unwrap();
        assert!(canonical.join("mine-a").join("SKILL.md").exists());
        assert!(canonical.join("mine-b").join("SKILL.md").exists());
        // Idempotent.
        disable_bundle(&store, &scope, "atelier").unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    /// (d) INCIDENT-REGRESSION: safe-deleting a bundle whose member is referenced
    /// by a live scope link must REFUSE (no blind remove_dir_all) and leave both
    /// the bundle canonical and the dependent link intact. This is the bundle
    /// analogue of the groundwork data-loss incident.
    #[test]
    fn bundle_safe_delete_dependent_aware_incident_regression() {
        let (base, store, scope) = fixture("bundle_safe_delete");
        seed_bundle(&store, "atelier", &["archetype-beat", "archetype-mix"]);
        // Place the bundle → its member links are live dependents into the
        // bundle's member subdirs.
        place_bundle(&store, &scope, "workspace", "atelier").unwrap();
        let canonical = store_path_for(&store, Kind::Bundle, "atelier").unwrap();

        // The scope root is the parent of `.claude`. `dependent_search_dirs` joins
        // `.claude/skills` etc., so feed the dir holding the scope `.claude`.
        let scope_root = scope.parent().unwrap().to_path_buf();
        let deps = bundle_member_dependents(&store, &canonical, "atelier", &[scope_root]);
        assert_eq!(deps.len(), 2, "both member links resolve into the bundle");
        // Load-bearing: the CORRECT two links were found (not spurious paths) —
        // proves the whole-canonical scan attributes dependents to this bundle.
        assert!(
            deps.iter().any(|p| p.ends_with("skills/archetype-beat")),
            "archetype-beat member link found as a dependent: {deps:?}"
        );
        assert!(
            deps.iter().any(|p| p.ends_with("skills/archetype-mix")),
            "archetype-mix member link found as a dependent: {deps:?}"
        );

        // Managed bundle WITH dependents → refuse, never wipe.
        let outcome = guarded_delete(&canonical, Kind::Bundle, /*managed=*/ true, &deps).unwrap();
        assert_eq!(outcome.verdict, "refused_dependents");
        assert!(!outcome.removed);

        // Bundle canonical + members STILL on disk.
        assert!(canonical.join("archetype-beat").join("SKILL.md").exists());
        assert!(canonical.join("archetype-mix").join("SKILL.md").exists());
        // Dependent links STILL resolve.
        for member in ["archetype-beat", "archetype-mix"] {
            let link = scope_path_for(&scope, Kind::Skill, member).unwrap();
            assert!(
                std::fs::canonicalize(&link).is_ok(),
                "{member} dependent link must survive the refusal"
            );
        }

        // An EXTERNAL bundle (managed:false) is likewise never wiped, even with
        // zero dependents — same external-master guard as single skills.
        let ext_outcome =
            guarded_delete(&canonical, Kind::Bundle, /*managed=*/ false, &[]).unwrap();
        assert_eq!(ext_outcome.verdict, "refused_external");
        assert!(!ext_outcome.removed);
        assert!(canonical.exists(), "external bundle canonical kept in place");

        // With zero dependents AND managed, a hard-delete is allowed (no live
        // member links remain): disable first, then delete proceeds.
        disable_bundle(&store, &scope, "atelier").unwrap();
        let deps_after = bundle_member_dependents(&store, &canonical, "atelier", &[scope.parent().unwrap().to_path_buf()]);
        assert!(deps_after.is_empty(), "no dependents after disable");
        let del = guarded_delete(&canonical, Kind::Bundle, /*managed=*/ true, &deps_after).unwrap();
        assert_eq!(del.verdict, "deleted");
        assert!(del.removed);
        assert!(!canonical.exists(), "managed bundle with no deps is hard-deleted");
        std::fs::remove_dir_all(&base).ok();
    }

    /// (e) Single-skill enable/disable/delete is UNCHANGED by the bundle work:
    /// placing a bundle does not disturb an independent same-scope skill, and the
    /// skill's own disable still drops only its own link.
    #[test]
    fn single_skill_unchanged_by_bundle_paths() {
        let (base, store, scope) = fixture("skill_unchanged");
        seed_skill(&store, "solo", "a standalone skill");
        seed_bundle(&store, "atelier", &["member-x"]);

        // Place the standalone skill, then the bundle.
        place_primitive(&store, &scope, "workspace", Kind::Skill, "solo").unwrap();
        place_bundle(&store, &scope, "workspace", "atelier").unwrap();

        // The standalone skill link is byte-for-byte the single-skill path and
        // resolves to store/skills/solo (NOT a bundle member).
        let solo_link = scope_path_for(&scope, Kind::Skill, "solo").unwrap();
        let solo_store = store_path_for(&store, Kind::Skill, "solo").unwrap();
        assert_eq!(
            std::fs::canonicalize(&solo_link).unwrap(),
            std::fs::canonicalize(&solo_store).unwrap(),
            "standalone skill still resolves to store/skills/solo"
        );

        // Disabling the bundle must NOT touch the standalone skill.
        disable_bundle(&store, &scope, "atelier").unwrap();
        assert_eq!(
            std::fs::canonicalize(&solo_link).unwrap(),
            std::fs::canonicalize(&solo_store).unwrap(),
            "bundle disable leaves the standalone skill intact"
        );
        // The single-skill disable path still drops only its own link.
        disable_core(&scope, Kind::Skill, "solo").unwrap();
        assert!(std::fs::symlink_metadata(&solo_link).is_err());
        assert!(solo_store.join("SKILL.md").exists(), "store skill untouched");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── enable creates a symlink resolving into the store ────────────────

    #[test]
    fn enable_creates_symlink_into_store() {
        let (base, store, scope) = fixture("enable");
        seed_agent(&store, "shared", "an agent");

        let m = place_primitive(&store, &scope, "project:p", Kind::Agent, "shared").unwrap();
        let link = PathBuf::from(&m.path);
        let lmeta = std::fs::symlink_metadata(&link).unwrap();
        assert!(lmeta.file_type().is_symlink(), "farm entry is a symlink");

        // Resolves to the store canonical copy.
        let resolved = std::fs::canonicalize(&link).unwrap();
        let expected =
            std::fs::canonicalize(store_path_for(&store, Kind::Agent, "shared").unwrap()).unwrap();
        assert_eq!(resolved, expected, "symlink resolves into the store");
        assert!(
            is_in_store(&resolved, &store),
            "resolved target is in store"
        );
        // link_target is the (unresolved) store path the symlink points at.
        let lt = m.link_target.expect("link_target populated on enable");
        assert!(
            lt.ends_with("shared.md"),
            "link target points at store copy: {lt}"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn enable_is_idempotent() {
        let (base, store, scope) = fixture("idem");
        seed_agent(&store, "a", "x");
        let m1 = place_primitive(&store, &scope, "workspace", Kind::Agent, "a").unwrap();
        let m2 = place_primitive(&store, &scope, "workspace", Kind::Agent, "a").unwrap();
        assert_eq!(m1.path, m2.path);
        assert!(is_enabled_in(&scope, &store, Kind::Agent, "a"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn enable_skill_dir_symlink() {
        let (base, store, scope) = fixture("skill_enable");
        seed_skill(&store, "myskill", "a skill");
        let m = place_primitive(&store, &scope, "workspace", Kind::Skill, "myskill").unwrap();
        let link = PathBuf::from(&m.path);
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        // SKILL.md is reachable through the link.
        assert!(link.join("SKILL.md").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn enable_missing_store_entry_errors() {
        let (base, store, scope) = fixture("missing");
        let err = place_primitive(&store, &scope, "workspace", Kind::Agent, "ghost").unwrap_err();
        assert!(err.contains("store has no"), "got: {err}");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── disable drops only the link; store intact ────────────────────────

    #[test]
    fn disable_drops_link_store_intact() {
        let (base, store, scope) = fixture("disable");
        seed_agent(&store, "a", "x");
        place_primitive(&store, &scope, "workspace", Kind::Agent, "a").unwrap();
        let store_file = store_path_for(&store, Kind::Agent, "a").unwrap();
        assert!(store_file.exists());

        disable_core(&scope, Kind::Agent, "a").unwrap();
        let link = scope_path_for(&scope, Kind::Agent, "a").unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err(), "link removed");
        assert!(store_file.exists(), "store canonical copy untouched");

        // Idempotent — disabling again is fine.
        disable_core(&scope, Kind::Agent, "a").unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn disable_skill_drops_link_not_store_tree() {
        let (base, store, scope) = fixture("disable_skill");
        seed_skill(&store, "s", "x");
        place_primitive(&store, &scope, "workspace", Kind::Skill, "s").unwrap();
        disable_core(&scope, Kind::Skill, "s").unwrap();
        let link = scope_path_for(&scope, Kind::Skill, "s").unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        // Store skill dir + its files survive.
        let store_dir = store_path_for(&store, Kind::Skill, "s").unwrap();
        assert!(store_dir.join("SKILL.md").exists());
        assert!(store_dir.join("helper.py").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    // ── copy / move preserve store + source integrity ────────────────────

    #[test]
    fn copy_materializes_real_file_in_dest_keeps_source() {
        let (base, store, from) = fixture("copy");
        let to = base.join("proj2").join(".claude");
        std::fs::create_dir_all(&to).unwrap();
        seed_agent(&store, "a", "x");
        place_primitive(&store, &from, "workspace", Kind::Agent, "a").unwrap();

        let m = copy_core(&from, &to, "workspace", "project:p2", Kind::Agent, "a").unwrap();
        // Source link still present.
        let src_link = scope_path_for(&from, Kind::Agent, "a").unwrap();
        assert!(std::fs::symlink_metadata(&src_link)
            .unwrap()
            .file_type()
            .is_symlink());
        // Dest is a REAL file (not a symlink), resolving the source link.
        let dst = PathBuf::from(&m.path);
        let dmeta = std::fs::symlink_metadata(&dst).unwrap();
        assert!(!dmeta.file_type().is_symlink(), "copy dest is a real file");
        assert!(dst.is_file());
        assert_eq!(m.link_target, None);
        // Store canonical copy intact.
        assert!(store_path_for(&store, Kind::Agent, "a").unwrap().exists());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn copy_skill_dir_deep() {
        let (base, store, from) = fixture("copy_skill");
        let to = base.join("proj2").join(".claude");
        std::fs::create_dir_all(&to).unwrap();
        seed_skill(&store, "s", "x");
        place_primitive(&store, &from, "workspace", Kind::Skill, "s").unwrap();
        let m = copy_core(&from, &to, "workspace", "project:p2", Kind::Skill, "s").unwrap();
        let dst = PathBuf::from(&m.path);
        assert!(!std::fs::symlink_metadata(&dst)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(dst.join("SKILL.md").is_file());
        assert!(dst.join("helper.py").is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn move_copies_then_removes_source() {
        let (base, store, from) = fixture("move");
        let to = base.join("proj2").join(".claude");
        std::fs::create_dir_all(&to).unwrap();
        seed_agent(&store, "a", "x");
        // Make the source a REAL file in the farm (not a link) so move clearly
        // relocates it.
        let src = scope_path_for(&from, Kind::Agent, "a").unwrap();
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "---\nname: a\n---\nbody").unwrap();

        let m = move_core(&from, &to, "workspace", "project:p2", Kind::Agent, "a").unwrap();
        assert!(std::fs::symlink_metadata(&src).is_err(), "source removed");
        assert!(PathBuf::from(&m.path).is_file(), "dest present");
        // Store untouched (move is scope-local).
        assert!(store_path_for(&store, Kind::Agent, "a").unwrap().exists());
        std::fs::remove_dir_all(&base).ok();
    }

    // ── path confinement ─────────────────────────────────────────────────

    #[test]
    fn mutations_reject_outside_claude_or_store() {
        let (base, store, _scope) = fixture("confine");
        seed_agent(&store, "a", "x");
        // A scope dir with NO `.claude` segment and not under the store.
        let rogue = base.join("not-claude");
        std::fs::create_dir_all(&rogue).unwrap();

        // enable into a rogue scope dir is rejected.
        let e1 = place_primitive(&store, &rogue, "workspace", Kind::Agent, "a").unwrap_err();
        assert!(e1.contains("outside .claude/store"), "enable: {e1}");

        // disable on a rogue scope dir rejected.
        let e2 = disable_core(&rogue, Kind::Agent, "a").unwrap_err();
        assert!(e2.contains("outside .claude/store"), "disable: {e2}");

        // remove on a rogue scope dir rejected.
        let e3 = remove_core(&rogue, Kind::Agent, "a").unwrap_err();
        assert!(e3.contains("outside .claude/store"), "remove: {e3}");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn copy_rejects_rogue_dest() {
        let (base, store, from) = fixture("confine_copy");
        seed_agent(&store, "a", "x");
        place_primitive(&store, &from, "workspace", Kind::Agent, "a").unwrap();
        let rogue = base.join("rogue-dest");
        std::fs::create_dir_all(&rogue).unwrap();
        let e = copy_core(&from, &rogue, "workspace", "x", Kind::Agent, "a").unwrap_err();
        assert!(e.contains("outside .claude/store"), "copy dest: {e}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn import_into_store_is_confined_and_atomic_layout() {
        let (base, store, _scope) = fixture("import");
        // Source primitive outside the store.
        let srcdir = base.join("incoming");
        std::fs::create_dir_all(&srcdir).unwrap();
        let src = srcdir.join("a.md");
        std::fs::write(&src, "---\nname: a\ndescription: imported\n---\nbody").unwrap();

        let entry = import_core(&store, Kind::Agent, "a", &src).unwrap();
        assert_eq!(entry.kind, "agent");
        assert_eq!(entry.name, "a");
        assert_eq!(entry.description.as_deref(), Some("imported"));
        let dest = store_path_for(&store, Kind::Agent, "a").unwrap();
        assert!(dest.is_file());
        // No leftover temp files in the store kind dir.
        let kind_dir = store.join("agents");
        let leftovers: Vec<_> = std::fs::read_dir(&kind_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("ngwa-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp files leak into store");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── atomicity: interrupted write proof ───────────────────────────────

    /// Simulate an interrupted atomic copy: stage the temp file but never
    /// rename it over the destination. Proves the destination is never left in
    /// a half-written state — it simply does not exist, and the canonical
    /// store copy (when overwriting) is untouched until the final rename.
    #[test]
    fn interrupted_write_leaves_dest_absent_not_partial() {
        let (base, store, _scope) = fixture("atomic");
        seed_agent(&store, "a", "original");
        let dest = store_path_for(&store, Kind::Agent, "a").unwrap();
        let original = std::fs::read_to_string(&dest).unwrap();

        // Manually perform the temp-write half of atomic_copy_file WITHOUT the
        // rename, mimicking a crash between write and rename.
        let tmp = temp_sibling(&dest);
        write_then_sync(&tmp, b"---\nname: a\ndescription: HALF\n---\npartial").unwrap();

        // The destination still holds the ORIGINAL, fully-valid content — the
        // partial write is isolated in the temp file.
        let after = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(after, original, "dest unchanged until rename commits");
        assert!(tmp.exists(), "partial content quarantined in temp file");
        // The temp file is uniquely named (won't be mistaken for a catalog entry).
        assert!(tmp
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("ngwa-tmp"));

        // Now complete the rename — the dest flips atomically to the new content.
        std::fs::rename(&tmp, &dest).unwrap();
        let final_content = std::fs::read_to_string(&dest).unwrap();
        assert!(
            final_content.contains("partial"),
            "rename commits new content"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn atomic_copy_file_overwrites_cleanly() {
        let (base, store, _scope) = fixture("overwrite");
        let kind_dir = store.join("agents");
        std::fs::create_dir_all(&kind_dir).unwrap();
        let dst = kind_dir.join("a.md");
        std::fs::write(&dst, "old").unwrap();
        let src = base.join("new.md");
        std::fs::write(&src, "new-content").unwrap();
        atomic_copy_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "new-content");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── catalog listing ──────────────────────────────────────────────────

    #[test]
    fn list_store_kind_reports_entries_with_description() {
        let (base, store, _scope) = fixture("list");
        seed_agent(&store, "alpha", "first");
        seed_agent(&store, "beta", "second");
        seed_skill(&store, "gamma", "skilled");

        let agents = list_store_kind(&store, Kind::Agent);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].name, "alpha");
        assert_eq!(agents[0].description.as_deref(), Some("first"));
        assert_eq!(agents[1].name, "beta");

        let skills = list_store_kind(&store, Kind::Skill);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "gamma");
        assert_eq!(skills[0].description.as_deref(), Some("skilled"));

        // hook/mcp have no store catalog here.
        assert!(list_store_kind(&store, Kind::Hook).is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn enabled_in_detection() {
        let (base, store, scope) = fixture("enabledin");
        seed_agent(&store, "a", "x");
        assert!(!is_enabled_in(&scope, &store, Kind::Agent, "a"));
        place_primitive(&store, &scope, "workspace", Kind::Agent, "a").unwrap();
        assert!(is_enabled_in(&scope, &store, Kind::Agent, "a"));
        disable_core(&scope, Kind::Agent, "a").unwrap();
        assert!(!is_enabled_in(&scope, &store, Kind::Agent, "a"));
        std::fs::remove_dir_all(&base).ok();
    }

    // ── JSON-fragment (hook/mcp) wiring: store-fragment → merge engine ────
    //
    // These exercise the integration seam this WP owns: read a store fragment,
    // splice/unsplice it via the merge engine, and (for the dest path) report
    // the right settings file. They use a project scope with an explicit root
    // so no HOME override is needed for the project legs; the move test threads
    // both a project src and project dest.

    use serde_json::{json, Value};

    /// Write a hook fragment into the store catalog.
    fn seed_hook_fragment(store: &Path, name: &str, event: &str, file: &str, block: Value) {
        let p = fragment_path(store, Kind::Hook, name).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let frag = json!({ "event": event, "file": file, "block": block });
        std::fs::write(&p, serde_json::to_string_pretty(&frag).unwrap()).unwrap();
    }

    /// Write an MCP fragment — its content IS the server_def.
    fn seed_mcp_fragment(store: &Path, name: &str, server_def: Value) {
        let p = fragment_path(store, Kind::Mcp, name).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(&server_def).unwrap()).unwrap();
    }

    /// MCP enable splices ONLY `mcpServers.<name>` into `<root>/.mcp.json`,
    /// preserving every unrelated key; disable removes only that block.
    #[test]
    fn mcp_fragment_enable_disable_splices_one_key() {
        let base = unique_tmp("mcp_frag");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // Project root (parent of .claude) — the merge engine writes .mcp.json here.
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        // Seed an existing .mcp.json with an unrelated server we must preserve.
        let mcp_json = proj.join(".mcp.json");
        std::fs::write(
            &mcp_json,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "exa": { "type": "stdio", "command": "exa-mcp" } }
            }))
            .unwrap(),
        )
        .unwrap();

        let def = json!({ "type": "stdio", "command": "royalti-mcp", "args": ["--stdio"] });
        seed_mcp_fragment(&store, "royalti", def.clone());

        // enable-in-scope → block spliced, exa preserved, target is .mcp.json.
        let target =
            enable_fragment_in_scope(&store, Kind::Mcp, "royalti", "project:p", Some(&proj))
                .unwrap();
        assert_eq!(target, mcp_json, "MCP target is <root>/.mcp.json");
        let after: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        assert_eq!(after.pointer("/mcpServers/royalti").unwrap(), &def);
        assert!(
            after.pointer("/mcpServers/exa").is_some(),
            "unrelated server preserved"
        );

        // disable-in-scope → only the royalti block removed; exa survives.
        disable_fragment_in_scope(&store, Kind::Mcp, "royalti", "project:p", Some(&proj)).unwrap();
        let after2: Value = serde_json::from_slice(&std::fs::read(&mcp_json).unwrap()).unwrap();
        assert!(
            after2.pointer("/mcpServers/royalti").is_none(),
            "royalti removed"
        );
        assert!(after2.pointer("/mcpServers/exa").is_some(), "exa untouched");

        std::fs::remove_dir_all(&base).ok();
    }

    /// Hook enable lands the fragment's block at `hooks.<event>` in the right
    /// settings file (shared vs local driven by the fragment's `file` tag).
    #[test]
    fn hook_fragment_enable_lands_block_in_right_file() {
        let base = unique_tmp("hook_frag");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        let block = json!([
            { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo hi" } ] }
        ]);
        // file: "local" → settings.local.json.
        seed_hook_fragment(&store, "guard", "PreToolUse", "local", block.clone());

        let target =
            enable_fragment_in_scope(&store, Kind::Hook, "guard", "project:p", Some(&proj))
                .unwrap();
        let expected = proj.join(".claude").join("settings.local.json");
        assert_eq!(target, expected, "hook 'local' → settings.local.json");

        let after: Value = serde_json::from_slice(&std::fs::read(&expected).unwrap()).unwrap();
        assert_eq!(
            after.pointer("/hooks/PreToolUse").unwrap(),
            &block,
            "block landed at hooks.PreToolUse"
        );
        // settings.json (shared) was NOT created — the fragment targeted local.
        assert!(
            !proj.join(".claude").join("settings.json").exists(),
            "shared file untouched"
        );

        disable_fragment_in_scope(&store, Kind::Hook, "guard", "project:p", Some(&proj)).unwrap();
        let after2: Value = serde_json::from_slice(&std::fs::read(&expected).unwrap()).unwrap();
        assert!(
            after2.pointer("/hooks/PreToolUse").is_none(),
            "block removed on disable"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// move = enable-in-dest + disable-in-source: present in dest, absent in
    /// source. Both legs use explicit project roots.
    #[test]
    fn mcp_fragment_move_present_in_dest_absent_in_source() {
        let base = unique_tmp("mcp_move");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = base.join("src");
        let dst = base.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();

        let def = json!({ "type": "stdio", "command": "royalti-mcp" });
        seed_mcp_fragment(&store, "royalti", def.clone());

        // Pre-enable in source so move has something to disable there.
        enable_fragment_in_scope(&store, Kind::Mcp, "royalti", "project:s", Some(&src)).unwrap();
        assert!(src.join(".mcp.json").exists());

        // Now the move legs (mirrors the command arm's ordering).
        enable_fragment_in_scope(&store, Kind::Mcp, "royalti", "project:d", Some(&dst)).unwrap();
        disable_fragment_in_scope(&store, Kind::Mcp, "royalti", "project:s", Some(&src)).unwrap();

        let in_dst: Value =
            serde_json::from_slice(&std::fs::read(dst.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            in_dst.pointer("/mcpServers/royalti").unwrap(),
            &def,
            "present in dest after move"
        );
        let in_src: Value =
            serde_json::from_slice(&std::fs::read(src.join(".mcp.json")).unwrap()).unwrap();
        assert!(
            in_src.pointer("/mcpServers/royalti").is_none(),
            "absent in source after move"
        );

        // Store fragment is NOT deleted by scope-local ops.
        assert!(fragment_path(&store, Kind::Mcp, "royalti")
            .unwrap()
            .exists());

        std::fs::remove_dir_all(&base).ok();
    }

    /// Fragment schema: `file` defaults to `shared` when omitted; `event` +
    /// `block` are carried through.
    #[test]
    fn hook_fragment_file_defaults_to_shared() {
        let base = unique_tmp("hook_default");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let p = fragment_path(&store, Kind::Hook, "h").unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // No "file" key.
        std::fs::write(&p, r#"{ "event": "Stop", "block": [] }"#).unwrap();
        let frag = read_hook_fragment(&store, "h").unwrap();
        assert_eq!(frag.event, "Stop");
        assert!(matches!(frag.file, HookFileTag::Shared));
        let proj = base.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let target =
            enable_fragment_in_scope(&store, Kind::Hook, "h", "project:p", Some(&proj)).unwrap();
        assert_eq!(target, proj.join(".claude").join("settings.json"));
        std::fs::remove_dir_all(&base).ok();
    }

    // ── WP-08 end-to-end: project scope resolves by DB id, not basename ──
    //
    // Regression guard for the scope-grammar fix. The FE now emits
    // `project:<id>` (the DB project slug) rather than `project:<basename>`.
    // This drives the real resolver (`resolve_scope_claude`/`_root`) against a
    // seeded projects DB whose slug deliberately differs from the root-dir
    // basename, then a project-scoped `enable` through the resolved path, and
    // asserts: (a) the slug string resolves into the project's `.claude` and
    // the symlink lands there resolving into the store; (b) the OLD basename
    // string errors `no project with id …` — exactly the bug the FE fix
    // avoids. Backend resolver code is unchanged; this locks the contract the
    // corrected FE now satisfies so neither side can silently drift again.
    #[tokio::test]
    async fn project_scope_resolves_by_db_id_not_basename() {
        use crate::commands::db::PaDb;
        use crate::commands::projects::{create_project, CreateArgs};

        let base = unique_tmp("scopeid");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // Root basename ("ngwa-smoke-proj") deliberately != project slug.
        let proj_root = base.join("ngwa-smoke-proj");
        std::fs::create_dir_all(proj_root.join(".claude")).unwrap();
        seed_skill(&store, "smoke-skill", "store skill for the e2e scope test");

        let db = Arc::new(PaDb::new(base.join("pa.db")));
        let pool = db.ensure_pool().await.unwrap();
        create_project(
            &pool,
            CreateArgs {
                id: "smoke-alias".into(),
                display_name: "Smoke Alias".into(),
                root_path: Some(proj_root.to_string_lossy().into_owned()),
                icon: None,
                color: None,
                description: None,
            },
        )
        .await
        .unwrap();

        // (a) the string the fixed FE emits resolves to the project's .claude.
        let resolved = resolve_scope_claude(&db, "project:smoke-alias")
            .await
            .unwrap();
        assert_eq!(resolved, proj_root.join(".claude"));
        let root = resolve_scope_root(&db, "project:smoke-alias")
            .await
            .unwrap();
        assert_eq!(root, Some(proj_root.clone()));

        // (b) the OLD basename string passes scope-format validation but has no
        // matching project row — the precise failure the bug produced live.
        let err = resolve_scope_claude(&db, "project:ngwa-smoke-proj")
            .await
            .unwrap_err();
        assert!(
            err.contains("no project"),
            "expected no-project error, got: {err}"
        );

        // end-to-end: enable into the resolved project scope; symlink lands
        // under the project's .claude and resolves into the store.
        let m = place_primitive(
            &store,
            &resolved,
            "project:smoke-alias",
            Kind::Skill,
            "smoke-skill",
        )
        .unwrap();
        let link = PathBuf::from(&m.path);
        assert!(
            link.starts_with(proj_root.join(".claude")),
            "link outside project: {link:?}"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "expected a symlink at {link:?}"
        );
        assert!(
            std::fs::canonicalize(&link)
                .unwrap()
                .starts_with(std::fs::canonicalize(&store).unwrap()),
            "symlink does not resolve into the store"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    // ── WP-23: per-engine file-kind writes (enable/disable/remove) ───────────
    //
    // Per-engine enable/disable/remove on FIXTURE trees only. The pure core
    // functions take an explicit `user_home` (the resolved `~`), so these tests
    // pass a tempdir for it and NEVER mutate the process-global `HOME` — no
    // races with the dev's real `~/.gemini` / `~/.codex`, and no cross-test
    // contention with the `merge.rs` HOME-mutating suite. The Claude path is
    // asserted byte-identical to the Phase-1 `place_primitive`.

    fn seed_command(store: &Path, name: &str, desc: &str) {
        let p = store_path_for(store, Kind::Command, name).unwrap();
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {desc}\n---\nbody"),
        )
        .unwrap();
    }

    // ── Claude: byte-unchanged from Phase 1 ──────────────────────────────────

    /// The engine-aware enable for Claude must produce a result identical to the
    /// Phase-1 `place_primitive`, and the on-disk symlink must be byte-identical
    /// (same link node, same store target). This is the DoD "Claude file-writes
    /// byte-unchanged from Phase 1" assertion.
    #[test]
    fn claude_engine_aware_enable_byte_identical_to_phase1() {
        // Two independent fixtures, identical seeds: one driven by the Phase-1
        // `place_primitive`, one by the WP-23 `enable_for_core(Claude, …)`.
        let (base1, store1, scope1) = fixture("cl_phase1");
        let (base2, store2, _scope2) = fixture("cl_wp23");
        seed_agent(&store1, "shared", "an agent");
        seed_agent(&store2, "shared", "an agent");

        // Phase-1 takes the `<scope>/.claude` dir directly.
        let m_phase1 = place_primitive(&store1, &scope1, "workspace", Kind::Agent, "shared").unwrap();
        // WP-23 takes the scope ROOT (parent of `.claude`) and appends `.claude`.
        // (Claude ignores user_home — it routes straight back to Phase-1.)
        let scope2_root = base2.join("proj");
        let m_wp23 = enable_for_core(
            EngineId::Claude,
            &store2,
            &scope2_root,
            &base2,
            "workspace",
            Kind::Agent,
            "shared",
        )
        .unwrap();

        // Same mutation shape (kind/name/scope + link_target relative suffix).
        assert_eq!(m_phase1.kind, m_wp23.kind);
        assert_eq!(m_phase1.name, m_wp23.name);
        assert_eq!(m_phase1.scope, m_wp23.scope);
        assert!(m_phase1.path.ends_with(".claude/agents/shared.md"));
        assert!(m_wp23.path.ends_with(".claude/agents/shared.md"));

        // Both produced a symlink resolving into their store.
        for (link, store) in [(&m_phase1.path, &store1), (&m_wp23.path, &store2)] {
            let lp = PathBuf::from(link);
            assert!(
                std::fs::symlink_metadata(&lp)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "Claude enable produces a symlink (Phase-1 mechanism)"
            );
            let resolved = std::fs::canonicalize(&lp).unwrap();
            assert!(is_in_store(&resolved, store), "resolves into the store");
        }

        std::fs::remove_dir_all(&base1).ok();
        std::fs::remove_dir_all(&base2).ok();
    }

    /// Claude disable + remove through the engine-aware path behave exactly like
    /// Phase-1 (drop the link, store untouched).
    #[test]
    fn claude_engine_aware_disable_remove_match_phase1() {
        let (base, store, _scope) = fixture("cl_dis");
        let scope_root = base.join("proj");
        seed_skill(&store, "s", "x");
        enable_for_core(
            EngineId::Claude,
            &store,
            &scope_root,
            &base,
            "workspace",
            Kind::Skill,
            "s",
        )
        .unwrap();
        let link = scope_root.join(".claude").join("skills").join("s");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());

        disable_for_core(EngineId::Claude, &scope_root, &base, Kind::Skill, "s").unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err(), "link dropped");
        // Store dir + files survive.
        let sdir = store_path_for(&store, Kind::Skill, "s").unwrap();
        assert!(sdir.join("SKILL.md").exists());

        // remove on an absent node is idempotent.
        remove_for_core(EngineId::Claude, &scope_root, &base, Kind::Skill, "s").unwrap();
        std::fs::remove_dir_all(&base).ok();
    }

    // ── Gemini: skills symlink, agents/commands copy-on-enable ───────────────

    /// A WP-23 fixture: `(base, store, scope_root, user_home)`. `scope_root` is
    /// the parent of the scope dotdir (`<base>/proj`); `user_home` is an explicit
    /// `~` tempdir (`<base>/home`) — never the process env.
    fn engine_fixture(tag: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let base = unique_tmp(tag);
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let scope_root = base.join("proj");
        std::fs::create_dir_all(&scope_root).unwrap();
        let user_home = base.join("home");
        std::fs::create_dir_all(&user_home).unwrap();
        (base, store, scope_root, user_home)
    }

    #[test]
    fn gemini_skill_symlinks_into_store_via_agents_alias() {
        let (base, store, scope_root, home) = engine_fixture("gm_skill");
        seed_skill(&store, "myskill", "a skill");

        let m = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Skill,
            "myskill",
        )
        .unwrap();
        let link = PathBuf::from(&m.path);
        assert!(
            link.starts_with(scope_root.join(".agents").join("skills")),
            "lands under .agents/skills: {link:?}"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "skill is a symlink"
        );
        assert!(
            link.join("SKILL.md").exists(),
            "SKILL.md reachable through link"
        );
        assert!(m.link_target.is_some(), "symlink cell reports link_target");

        // disable drops the link; store survives.
        disable_for_core(EngineId::Gemini, &scope_root, &home, Kind::Skill, "myskill").unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(store_path_for(&store, Kind::Skill, "myskill")
            .unwrap()
            .join("SKILL.md")
            .exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn gemini_agent_symlinks_to_user_dotdir() {
        let (base, store, scope_root, home) = engine_fixture("gm_agent");
        seed_agent(&store, "planner", "a planner");

        // Gemini agents are user-tier `~/.gemini/agents/<name>.md`, SymlinkDir
        // per the frozen layout (a symlink to the single .md store copy).
        let m = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Agent,
            "planner",
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert_eq!(dest, home.join(".gemini").join("agents").join("planner.md"));
        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "Gemini agent is SymlinkDir per the layout"
        );
        // Resolves to the store copy.
        let src = store_path_for(&store, Kind::Agent, "planner").unwrap();
        assert_eq!(
            std::fs::canonicalize(&dest).unwrap(),
            std::fs::canonicalize(&src).unwrap()
        );
        assert!(m.link_target.is_some(), "symlink cell reports link_target");

        // remove drops the link; store untouched.
        remove_for_core(EngineId::Gemini, &scope_root, &home, Kind::Agent, "planner").unwrap();
        assert!(std::fs::symlink_metadata(&dest).is_err());
        assert!(src.exists(), "store copy untouched by scope-local remove");

        std::fs::remove_dir_all(&base).ok();
    }

    /// Codex agents ARE copy-on-enable (`Mechanism::File`, `.toml` leaf) — the
    /// real exercise of the copy fallback for a single-file primitive.
    #[test]
    fn codex_agent_copies_on_enable_real_file() {
        let (base, store, scope_root, home) = engine_fixture("cx_agent");
        seed_agent(&store, "planner", "a planner");

        let m = enable_for_core(
            EngineId::Codex,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Agent,
            "planner",
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert_eq!(
            dest,
            home.join(".codex").join("agents").join("planner.toml")
        );
        let meta = std::fs::symlink_metadata(&dest).unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "File cell copies a REAL file (not a link)"
        );
        assert!(dest.is_file());
        assert_eq!(m.link_target, None, "copy cell has no link_target");
        // Content matches the store copy byte-for-byte (no transcode in WP-23).
        let src = store_path_for(&store, Kind::Agent, "planner").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), std::fs::read(&src).unwrap());

        remove_for_core(EngineId::Codex, &scope_root, &home, Kind::Agent, "planner").unwrap();
        assert!(std::fs::symlink_metadata(&dest).is_err());
        assert!(src.exists(), "store copy untouched");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn gemini_command_copies_as_toml_leaf() {
        let (base, store, scope_root, home) = engine_fixture("gm_cmd");
        seed_command(&store, "ship", "ship it");

        let m = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Command,
            "ship",
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        // `commands/**/{name}.toml` collapses to a flat `commands/ship.toml`.
        assert_eq!(
            dest,
            home.join(".gemini").join("commands").join("ship.toml")
        );
        assert!(dest.is_file());
        assert_eq!(m.link_target, None);
        std::fs::remove_dir_all(&base).ok();
    }

    // ── Codex: skills symlink, agents copy as .toml leaf ─────────────────────

    #[test]
    fn codex_skill_symlinks_and_agent_copies() {
        let (base, store, scope_root, home) = engine_fixture("cx");
        seed_skill(&store, "sk", "a skill");
        seed_agent(&store, "ag", "an agent");

        // Skill → symlink under cross-tool `.agents/skills`.
        let ms = enable_for_core(
            EngineId::Codex,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Skill,
            "sk",
        )
        .unwrap();
        let slink = PathBuf::from(&ms.path);
        assert!(slink.starts_with(scope_root.join(".agents").join("skills")));
        assert!(std::fs::symlink_metadata(&slink)
            .unwrap()
            .file_type()
            .is_symlink());

        // Agent → copied `.toml` leaf under `~/.codex/agents`.
        let ma = enable_for_core(
            EngineId::Codex,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Agent,
            "ag",
        )
        .unwrap();
        let adest = PathBuf::from(&ma.path);
        assert_eq!(adest, home.join(".codex").join("agents").join("ag.toml"));
        assert!(!std::fs::symlink_metadata(&adest)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(adest.is_file());
        assert_eq!(ma.link_target, None);

        std::fs::remove_dir_all(&base).ok();
    }

    // ── enable is idempotent for symlink cells across engines ────────────────

    #[test]
    fn gemini_enable_symlink_is_idempotent() {
        let (base, store, scope_root, home) = engine_fixture("gm_idem");
        seed_skill(&store, "s", "x");
        let m1 = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Skill,
            "s",
        )
        .unwrap();
        let m2 = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Skill,
            "s",
        )
        .unwrap();
        assert_eq!(m1.path, m2.path);
        assert!(is_engine_enabled(&PathBuf::from(&m1.path), &store));
        std::fs::remove_dir_all(&base).ok();
    }

    // ── missing store entry errors ───────────────────────────────────────────

    #[test]
    fn enable_for_missing_store_entry_errors() {
        let (base, store, scope_root, home) = engine_fixture("missing_for");
        let err = enable_for_core(
            EngineId::Gemini,
            &store,
            &scope_root,
            &home,
            "workspace",
            Kind::Agent,
            "ghost",
        )
        .unwrap_err();
        assert!(err.contains("store has no"), "got: {err}");
        std::fs::remove_dir_all(&base).ok();
    }

    // ── per-engine path confinement ──────────────────────────────────────────

    /// A resolved write target outside the engine's declared roots is refused —
    /// the per-engine generalization of Phase-1's `.claude`-only guard.
    #[test]
    fn enable_for_confinement_rejects_out_of_root_target() {
        let (base, _store, scope_root, home) = engine_fixture("confine");

        // A computed path under neither the scope `.gemini`/`.agents`, the
        // user-tier `~/.gemini`, nor `~/.agents` must be rejected.
        let rogue = base.join("rogue").join("s");
        assert!(
            !is_under_engine_root(EngineId::Gemini, &scope_root, &home, &rogue),
            "a path under no engine root must not pass confinement"
        );
        // Legitimate targets pass: scope-relative `.agents/skills` (skill cell)…
        let ok_skill = scope_root.join(".agents").join("skills").join("s");
        assert!(is_under_engine_root(
            EngineId::Gemini,
            &scope_root,
            &home,
            &ok_skill
        ));
        // …and user-tier `~/.gemini/agents` (agent cell).
        let ok_agent = home.join(".gemini").join("agents").join("a.md");
        assert!(is_under_engine_root(
            EngineId::Gemini,
            &scope_root,
            &home,
            &ok_agent
        ));

        std::fs::remove_dir_all(&base).ok();
    }

    // ── engine + kind parsing guards ─────────────────────────────────────────

    #[test]
    fn parse_engine_and_file_layout_cell() {
        assert_eq!(parse_engine("claude").unwrap(), EngineId::Claude);
        assert_eq!(parse_engine("gemini").unwrap(), EngineId::Gemini);
        assert_eq!(parse_engine("codex").unwrap(), EngineId::Codex);
        assert!(parse_engine("nope").is_err());

        // file_layout_cell errors for settings-embedded kinds.
        assert!(file_layout_cell(EngineId::Gemini, Kind::Hook).is_err());
        assert!(file_layout_cell(EngineId::Gemini, Kind::Mcp).is_err());
        // and resolves for file kinds with the right mechanism.
        assert_eq!(
            file_layout_cell(EngineId::Gemini, Kind::Skill)
                .unwrap()
                .mechanism,
            Mechanism::SymlinkDir
        );
        assert_eq!(
            file_layout_cell(EngineId::Gemini, Kind::Command)
                .unwrap()
                .mechanism,
            Mechanism::File
        );
        assert_eq!(
            file_layout_cell(EngineId::Codex, Kind::Agent)
                .unwrap()
                .mechanism,
            Mechanism::File
        );
    }

    // ── WP-24: cross-engine transcode copy/move (D-09) ───────────────────────
    //
    // All on FIXTURE trees with an explicit `user_home` tempdir (never the
    // process env). `copy_cross_engine_row` is the pure core the
    // `claude_primitive_copy_batch` command delegates to per destination, so the
    // round-trip + blocked-pair + batch behaviour are all exercised here without
    // a Tauri State / DB harness.

    /// Write a single-file MD primitive (agent/command frontmatter + body) at an
    /// arbitrary path and return it. Used as the "resolved source" for a copy.
    fn seed_md_source(dir: &Path, leaf: &str, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(leaf);
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: a {name}\n---\n\n{body}\n"),
        )
        .unwrap();
        p
    }

    /// matrix: every cell of `resolve_transcode_plan` matches `06`.
    #[test]
    fn transcode_matrix_matches_contract() {
        use EngineId::*;
        use Kind::*;
        // Same-format direct copies.
        assert_eq!(
            resolve_transcode_plan(Claude, Gemini, Agent).unwrap(),
            TranscodePlan::DirectCopy
        );
        assert_eq!(
            resolve_transcode_plan(Gemini, Claude, Agent).unwrap(),
            TranscodePlan::DirectCopy
        );
        // Skills are MD everywhere → direct dir copy in every pair.
        for (a, b) in [(Claude, Gemini), (Gemini, Codex), (Codex, Claude)] {
            assert_eq!(
                resolve_transcode_plan(a, b, Skill).unwrap(),
                TranscodePlan::DirectCopy,
                "skill {a:?}→{b:?} is a direct dir copy"
            );
        }
        // MD agent → Codex TOML agent (from Claude AND Gemini).
        assert_eq!(
            resolve_transcode_plan(Claude, Codex, Agent).unwrap(),
            TranscodePlan::MdToCodexToml
        );
        assert_eq!(
            resolve_transcode_plan(Gemini, Codex, Agent).unwrap(),
            TranscodePlan::MdToCodexToml
        );
        // Claude MD command → Gemini TOML command.
        assert_eq!(
            resolve_transcode_plan(Claude, Gemini, Command).unwrap(),
            TranscodePlan::MdToGeminiCommandToml
        );
        // BLOCKED: Codex TOML agent → Claude/Gemini MD agent.
        for to in [Claude, Gemini] {
            let err = resolve_transcode_plan(Codex, to, Agent).unwrap_err();
            assert!(
                matches!(err, StoreError::TranscodeUnsupported { .. }),
                "Codex→{to:?} agent must be TranscodeUnsupported, got {err:?}"
            );
        }
        // BLOCKED: Gemini TOML command → Claude/Codex MD command.
        for to in [Claude, Codex] {
            let err = resolve_transcode_plan(Gemini, to, Command).unwrap_err();
            assert!(
                matches!(err, StoreError::TranscodeUnsupported { .. }),
                "Gemini→{to:?} command must be TranscodeUnsupported, got {err:?}"
            );
        }
        // hooks / MCP are out of scope (settings-embedded, not portable).
        assert!(matches!(
            resolve_transcode_plan(Claude, Gemini, Hook).unwrap_err(),
            StoreError::Unsupported { .. }
        ));
        assert!(matches!(
            resolve_transcode_plan(Claude, Codex, Mcp).unwrap_err(),
            StoreError::Unsupported { .. }
        ));
    }

    /// ALLOWED #1 — Claude agent (MD) → Gemini agent (MD): direct copy, lands a
    /// real file under `~/.gemini/agents/<name>.md` byte-identical to the source.
    #[test]
    fn xeng_claude_agent_to_gemini_agent_direct_copy() {
        let (base, _store, scope_root, home) = engine_fixture("xe_cl_gm_agent");
        let src = seed_md_source(&base.join("src"), "planner.md", "planner", "Plan it.");
        let original = std::fs::read_to_string(&src).unwrap();

        let m = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Gemini,
            Kind::Agent,
            "planner",
            &src,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert_eq!(dest, home.join(".gemini").join("agents").join("planner.md"));
        assert_eq!(
            std::fs::read_to_string(&dest).unwrap(),
            original,
            "byte-identical copy"
        );
        assert!(
            m.link_target.is_none(),
            "cross-engine copy materializes a real file"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// ALLOWED #2 — skill (MD dir) → skill: direct dir copy, SKILL.md + helpers
    /// land under the target engine's `.agents/skills/<name>/`.
    #[test]
    fn xeng_skill_to_skill_direct_dir_copy() {
        let (base, store, scope_root, home) = engine_fixture("xe_skill");
        // Seed a real skill dir in the store, resolve its path as the source.
        seed_skill(&store, "huashu", "design");
        let src = store_path_for(&store, Kind::Skill, "huashu").unwrap();

        let m = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Codex,
            Kind::Skill,
            "huashu",
            &src,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert!(dest.starts_with(scope_root.join(".agents").join("skills")));
        assert!(dest.join("SKILL.md").exists(), "SKILL.md copied");
        assert!(dest.join("helper.py").exists(), "supporting file copied");
        assert!(!dest.is_symlink(), "real dir, not a link");
        std::fs::remove_dir_all(&base).ok();
    }

    /// ALLOWED #3 — Claude agent (MD) → Codex agent (TOML): `md_to_codex_toml`.
    /// The transcoded file lands at `~/.codex/agents/<name>.toml` and contains the
    /// transcoder's output (`system_prompt` from the body).
    #[test]
    fn xeng_claude_agent_to_codex_toml() {
        let (base, _store, scope_root, home) = engine_fixture("xe_cl_cx_agent");
        let src = seed_md_source(
            &base.join("src"),
            "planner.md",
            "planner",
            "You are a planner.",
        );

        let m = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Codex,
            Kind::Agent,
            "planner",
            &src,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert_eq!(
            dest,
            home.join(".codex").join("agents").join("planner.toml")
        );
        let toml = std::fs::read_to_string(&dest).unwrap();
        assert!(toml.contains("name = \"planner\""), "frontmatter carried");
        assert!(
            toml.contains("system_prompt = \"\"\""),
            "body → system_prompt"
        );
        assert!(toml.contains("You are a planner."));
        std::fs::remove_dir_all(&base).ok();
    }

    /// ALLOWED #4 — Claude command (MD) → Gemini command (TOML):
    /// `md_to_gemini_command_toml`. Body → `prompt`, lands `.toml`.
    #[test]
    fn xeng_claude_command_to_gemini_toml() {
        let (base, _store, scope_root, home) = engine_fixture("xe_cl_gm_cmd");
        let src = seed_md_source(&base.join("src"), "ship.md", "ship", "Ship the release.");

        let m = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Gemini,
            Kind::Command,
            "ship",
            &src,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap();
        let dest = PathBuf::from(&m.path);
        assert_eq!(
            dest,
            home.join(".gemini").join("commands").join("ship.toml")
        );
        let toml = std::fs::read_to_string(&dest).unwrap();
        assert!(
            toml.contains("prompt = \"\"\""),
            "body → prompt (Gemini command)"
        );
        assert!(toml.contains("Ship the release."));
        std::fs::remove_dir_all(&base).ok();
    }

    /// BLOCKED — Codex agent (TOML) → Claude agent (MD): returns
    /// `TranscodeUnsupported` BEFORE any write. Assert NO FS change at the dest.
    #[test]
    fn xeng_codex_agent_to_claude_blocked_no_write() {
        let (base, _store, scope_root, home) = engine_fixture("xe_cx_cl_block");
        // A real TOML source (what a Codex agent looks like on disk).
        let src_dir = base.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("planner.toml");
        std::fs::write(
            &src,
            "name = \"planner\"\nsystem_prompt = \"\"\"\nx\"\"\"\n",
        )
        .unwrap();

        // The destination Claude would write to, were it not blocked.
        let would_be_dest = scope_root.join(".claude").join("agents").join("planner.md");
        assert!(!would_be_dest.exists());

        let err = copy_cross_engine_row(
            EngineId::Codex,
            EngineId::Claude,
            Kind::Agent,
            "planner",
            &src,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::TranscodeUnsupported { .. }),
            "blocked pair returns TranscodeUnsupported, got {err:?}"
        );
        // Pre-write assertion: NOTHING was written at the destination, and the
        // `.claude` farm dir was never even created.
        assert!(
            !would_be_dest.exists(),
            "blocked pair never writes the dest file"
        );
        assert!(
            !scope_root.join(".claude").join("agents").exists(),
            "blocked pair never creates the dest dir"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    /// BATCH-shape — drive `copy_cross_engine_row` over a mixed set of
    /// destinations (the per-row loop `claude_primitive_copy_batch` runs): two
    /// allowed (one same-format, one transcode) + one deliberately blocked. Assert
    /// per-row outcomes in request order, and that the blocked row left no file.
    #[test]
    fn xeng_batch_mixed_per_row_results() {
        let (base, _store, scope_root, home) = engine_fixture("xe_batch");
        let src = seed_md_source(&base.join("src"), "planner.md", "planner", "Plan.");

        // Request order: [Gemini agent (same), Codex agent (transcode)].
        // Both ALLOWED from a Claude MD agent source.
        let r_same = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Gemini,
            Kind::Agent,
            "planner",
            &src,
            "workspace",
            &scope_root,
            &home,
        );
        let r_transcode = copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Codex,
            Kind::Agent,
            "planner",
            &src,
            "workspace",
            &scope_root,
            &home,
        );
        assert!(r_same.is_ok(), "same-format row ok");
        assert!(r_transcode.is_ok(), "transcode row ok");
        assert_eq!(
            CopyMode::from_plan(TranscodePlan::DirectCopy) as u8,
            CopyMode::Same as u8
        );

        // A deliberately blocked destination (Codex TOML → Claude MD), using a
        // TOML source — fails before any write.
        let toml_src = base.join("src").join("p.toml");
        std::fs::write(&toml_src, "name = \"p\"\n").unwrap();
        let r_blocked = copy_cross_engine_row(
            EngineId::Codex,
            EngineId::Claude,
            Kind::Agent,
            "p",
            &toml_src,
            "workspace",
            &scope_root,
            &home,
        );
        assert!(
            matches!(r_blocked, Err(StoreError::TranscodeUnsupported { .. })),
            "blocked row errors typed"
        );
        // The two ok rows landed real files; the blocked one wrote nothing.
        assert!(home
            .join(".gemini")
            .join("agents")
            .join("planner.md")
            .exists());
        assert!(home
            .join(".codex")
            .join("agents")
            .join("planner.toml")
            .exists());
        assert!(!scope_root
            .join(".claude")
            .join("agents")
            .join("p.md")
            .exists());

        std::fs::remove_dir_all(&base).ok();
    }

    /// The transcoded TOML for an agent is byte-equal to calling the transcoder
    /// directly — proving WP-24 does NOT alter the transcoder's output.
    #[test]
    fn xeng_transcode_output_equals_transcoder_directly() {
        let (base, _store, scope_root, home) = engine_fixture("xe_byteq");
        let md = "---\nname: planner\ndescription: a planner\n---\n\nYou are a planner.\n";
        let src = base.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let src_file = src.join("planner.md");
        std::fs::write(&src_file, md).unwrap();

        copy_cross_engine_row(
            EngineId::Claude,
            EngineId::Codex,
            Kind::Agent,
            "planner",
            &src_file,
            "workspace",
            &scope_root,
            &home,
        )
        .unwrap();
        let landed =
            std::fs::read_to_string(home.join(".codex").join("agents").join("planner.toml"))
                .unwrap();
        let direct = crate::pkg::engine_adapters::transcoder::md_to_codex_toml(md).unwrap();
        assert_eq!(
            landed, direct,
            "WP-24 writes the transcoder's bytes verbatim"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
