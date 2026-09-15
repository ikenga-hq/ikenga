//! 4-tier layered Claude-config discovery (Phase 4 of projects-first-class).
//!
//! Replaces the legacy 2-tier model (Project / Personal) with:
//!
//!   Tier 1 — Personal       (`~/.claude/{agents,skills,commands}/*` excluding
//!                            subdirs that belong to an installed pkg id).
//!   Tier 2 — Workspace pkg  (Kernel.list_installed() rows with project_id=None.
//!                            Files live at `~/.claude/<kind>/<pkg.id>/...`).
//!   Tier 3 — Active project (`<project.root_path>/.claude/...`).
//!   Tier 4 — Project pkg    (Kernel.list_installed() rows whose project_id
//!                            matches the active project).
//!
//! Default conflict resolution: lower tier wins (Personal beats WorkspacePkg
//! beats Project beats ProjectPkg). A `claude_asset_preferences` row (the
//! "pin") can override the default per (scope, kind, name).
//!
//! This module runs alongside the legacy `commands/claude_config.rs` scan —
//! the legacy Tauri commands stay registered so the existing /claude UI
//! keeps working. New callers go through `claude_assets_discover`.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

use crate::commands::claude_config::{
    parse_hooks_file, parse_mcp_file, parse_mcp_from_settings, HookEntry, McpEntry,
    Scope as LegacyScope,
};
use crate::commands::projects::get_project;

// ─── Public types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Personal,
    WorkspacePkg,
    Project,
    ProjectPkg,
}

impl Tier {
    /// Lower-tier-wins ordering for conflict resolution.
    pub fn precedence(self) -> u8 {
        match self {
            Tier::Personal => 0,
            Tier::WorkspacePkg => 1,
            Tier::Project => 2,
            Tier::ProjectPkg => 3,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "personal" => Some(Tier::Personal),
            "workspace_pkg" => Some(Tier::WorkspacePkg),
            "project" => Some(Tier::Project),
            "project_pkg" => Some(Tier::ProjectPkg),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Personal => "personal",
            Tier::WorkspacePkg => "workspace_pkg",
            Tier::Project => "project",
            Tier::ProjectPkg => "project_pkg",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Skill,
    Agent,
    Command,
    Hook,
    Mcp,
}

impl AssetKind {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(AssetKind::Skill),
            "agent" => Some(AssetKind::Agent),
            "command" => Some(AssetKind::Command),
            "hook" => Some(AssetKind::Hook),
            "mcp" => Some(AssetKind::Mcp),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Skill => "skill",
            AssetKind::Agent => "agent",
            AssetKind::Command => "command",
            AssetKind::Hook => "hook",
            AssetKind::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetSource {
    pub tier: Tier,
    /// pkg id, "personal", or "project:<id>".
    pub provider: String,
    /// Resolved file path (or sentinel for MCP entries — same as the legacy
    /// path field, e.g. the settings file the server was declared in).
    pub path: String,
    pub name: String,
    pub kind: AssetKind,
}

#[derive(Debug, Default, Serialize)]
pub struct AssetTree {
    pub skills: HashMap<String, Vec<AssetSource>>,
    pub agents: HashMap<String, Vec<AssetSource>>,
    pub commands: HashMap<String, Vec<AssetSource>>,
    pub hooks: HashMap<String, Vec<AssetSource>>,
    pub mcps: HashMap<String, Vec<AssetSource>>,
}

/// `(asset_kind, asset_name) -> (preferred_tier, preferred_source)`.
pub type PinMap = HashMap<(AssetKind, String), (Tier, Option<String>)>;

// ─── Discovery entry point ─────────────────────────────────────────────────

/// Run the 4-tier discovery walk. `active_project_id` selects which project
/// rows populate tier 3 + 4; pass `"default"` (or the result of
/// `get_active_project_id`) for the active session.
///
/// Best-effort: bad files are silently skipped (the legacy scan surfaces them
/// in `errors`, we don't have a place for those here yet). FE/MCP callers
/// that need diagnostics should also call `claude_config_load`.
pub async fn discover(
    active_project_id: &str,
    pool: &SqlitePool,
    app: &AppHandle,
) -> Result<AssetTree, String> {
    // Snapshot installed pkgs up front so we can split them by tier and use
    // the id set as a filter against personal-tier files.
    let kernel: tauri::State<crate::commands::pkg::KernelState> = app.state();
    let installed = kernel.0.list_installed();
    let installed_ids: HashSet<String> = installed.iter().map(|s| s.id.clone()).collect();

    let mut tree = AssetTree::default();

    // ── Tier 1: Personal ──────────────────────────────────────────────────
    if let Some(home) = home_dir() {
        let personal = home.join(".claude");
        if personal.is_dir() {
            scan_personal_files(&personal, &installed_ids, &mut tree);
            // Settings.json hooks + mcpServers (top-level only).
            let p = personal.join("settings.json");
            if p.is_file() {
                push_hooks(
                    &p,
                    LegacyScope::Personal,
                    None,
                    Tier::Personal,
                    "personal",
                    &mut tree,
                );
                push_mcps_from_settings(
                    &p,
                    LegacyScope::Personal,
                    None,
                    Tier::Personal,
                    "personal",
                    &mut tree,
                );
            }
            // Personal mcp.json
            let mcp_json = personal.join("mcp.json");
            if mcp_json.is_file() {
                push_mcps_from_mcp_file(
                    &mcp_json,
                    LegacyScope::Personal,
                    None,
                    Tier::Personal,
                    "personal",
                    &mut tree,
                );
            }
            // Top-level ~/.claude.json (Claude Code user-level config) — only
            // root-level mcpServers.
            let user_json = home.join(".claude.json");
            if user_json.is_file() {
                push_mcps_from_settings(
                    &user_json,
                    LegacyScope::Personal,
                    None,
                    Tier::Personal,
                    "personal",
                    &mut tree,
                );
            }
        }
    }

    // ── Tier 2 + 4: Pkg contributions ────────────────────────────────────
    for summary in &installed {
        let tier = match &summary.project_id {
            None => Tier::WorkspacePkg,
            Some(pid) if pid == active_project_id => Tier::ProjectPkg,
            Some(_) => continue, // inactive project pkg — invisible this session
        };
        scan_pkg_contributions(&summary.id, tier, &mut tree);
        // Manifest mcp[] entries — load manifest and surface each server.
        scan_pkg_mcps(summary, tier, &mut tree);
    }

    // ── Tier 3: Active project ───────────────────────────────────────────
    if let Some(project) = get_project(pool, active_project_id).await.ok().flatten() {
        if let Some(root) = project.root_path.as_deref() {
            if let Ok(root_path) = expand(root) {
                let claude = root_path.join(".claude");
                if claude.is_dir() {
                    let provider = format!("project:{}", project.id);
                    scan_project_files(&claude, &project.id, &provider, &mut tree);

                    for f in ["settings.local.json", "settings.json"] {
                        let p = claude.join(f);
                        if p.is_file() {
                            push_hooks(
                                &p,
                                LegacyScope::Project,
                                Some(&root_path.to_string_lossy()),
                                Tier::Project,
                                &provider,
                                &mut tree,
                            );
                            push_mcps_from_settings(
                                &p,
                                LegacyScope::Project,
                                Some(&root_path.to_string_lossy()),
                                Tier::Project,
                                &provider,
                                &mut tree,
                            );
                        }
                    }
                    let mcp_json = claude.join("mcp.json");
                    if mcp_json.is_file() {
                        push_mcps_from_mcp_file(
                            &mcp_json,
                            LegacyScope::Project,
                            Some(&root_path.to_string_lossy()),
                            Tier::Project,
                            &provider,
                            &mut tree,
                        );
                    }
                }
            }
        }
    }

    // Sort each entry's source vec by tier precedence for stable ordering.
    for sources in tree
        .skills
        .values_mut()
        .chain(tree.agents.values_mut())
        .chain(tree.commands.values_mut())
        .chain(tree.hooks.values_mut())
        .chain(tree.mcps.values_mut())
    {
        sources.sort_by_key(|s| s.tier.precedence());
    }

    Ok(tree)
}

// ─── Tier 1 — personal scanning ────────────────────────────────────────────

/// Walk `~/.claude/{agents,skills,commands}/`. Subdirs whose name matches an
/// installed pkg id are skipped (those are tier 2/4 contributions).
fn scan_personal_files(personal: &Path, installed_ids: &HashSet<String>, tree: &mut AssetTree) {
    // Agents: `~/.claude/agents/*.md` (top-level only — subdirs belong to pkgs).
    scan_kind_personal(
        &personal.join("agents"),
        AssetKind::Agent,
        installed_ids,
        tree,
    );
    // Commands: same shape.
    scan_kind_personal(
        &personal.join("commands"),
        AssetKind::Command,
        installed_ids,
        tree,
    );
    // Skills: `~/.claude/skills/<name>/SKILL.md`. Skill `<name>` matching an
    // installed pkg id is a pkg-provided skill.
    let skills_dir = personal.join("skills");
    if skills_dir.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&skills_dir) {
            for entry in rd.flatten() {
                let skill_dir = entry.path();
                if !skill_dir.is_dir() {
                    continue;
                }
                let name = match skill_dir.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                if name.starts_with('_') || name.starts_with('.') {
                    continue;
                }
                if installed_ids.contains(&name) {
                    continue;
                }
                let skill_md = skill_dir.join("SKILL.md");
                if !skill_md.is_file() {
                    continue;
                }
                push_source(
                    &mut tree.skills,
                    AssetSource {
                        tier: Tier::Personal,
                        provider: "personal".into(),
                        path: skill_md.to_string_lossy().to_string(),
                        name: name.clone(),
                        kind: AssetKind::Skill,
                    },
                );
            }
        }
    }
}

fn scan_kind_personal(
    dir: &Path,
    kind: AssetKind,
    installed_ids: &HashSet<String>,
    tree: &mut AssetTree,
) {
    if !dir.is_dir() {
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        // Skip pkg subdirs (their `<pkg-id>/...` files surface via tier 2/4).
        if p.is_dir() {
            continue;
        }
        if !p.is_file() {
            continue;
        }
        let name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if name.starts_with('_') {
            continue;
        }
        // Defensive: a flat file whose stem matches a pkg id is still
        // personal (it can't have been installed by a pkg because the
        // installer writes to a subdir). No-op here, but reading the rule
        // out loud: only directories named after a pkg id are excluded.
        let _ = installed_ids;
        let map = match kind {
            AssetKind::Agent => &mut tree.agents,
            AssetKind::Command => &mut tree.commands,
            _ => unreachable!("scan_kind_personal called with non-md kind"),
        };
        push_source(
            map,
            AssetSource {
                tier: Tier::Personal,
                provider: "personal".into(),
                path: p.to_string_lossy().to_string(),
                name,
                kind,
            },
        );
    }
}

// ─── Tier 2 + 4 — pkg contributions ────────────────────────────────────────

/// Walk `~/.claude/<kind>/<pkg_id>/...` for one installed pkg.
fn scan_pkg_contributions(pkg_id: &str, tier: Tier, tree: &mut AssetTree) {
    let Some(home) = home_dir() else { return };
    let personal = home.join(".claude");
    for (subdir, kind, map) in [
        ("agents", AssetKind::Agent, "agents"),
        ("commands", AssetKind::Command, "commands"),
    ] {
        let _ = map;
        let pkg_dir = personal.join(subdir).join(pkg_id);
        if !pkg_dir.is_dir() {
            continue;
        }
        scan_md_recursive(&pkg_dir, pkg_id, tier, kind, tree);
    }
    // Skills: `~/.claude/skills/<pkg_id>/<skill_name>/SKILL.md` (one level).
    let skills_root = personal.join("skills").join(pkg_id);
    if skills_root.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&skills_root) {
            for entry in rd.flatten() {
                let skill_dir = entry.path();
                if !skill_dir.is_dir() {
                    // pkg could ship a flat SKILL.md too — handle that case
                    // by treating the pkg dir itself as the skill.
                    continue;
                }
                let name = match skill_dir.file_name().and_then(|s| s.to_str()) {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let skill_md = skill_dir.join("SKILL.md");
                if !skill_md.is_file() {
                    continue;
                }
                push_source(
                    &mut tree.skills,
                    AssetSource {
                        tier,
                        provider: pkg_id.to_string(),
                        path: skill_md.to_string_lossy().to_string(),
                        name,
                        kind: AssetKind::Skill,
                    },
                );
            }
            // Also handle the case where the pkg-id dir itself is the skill
            // (single-skill pkg with a flat SKILL.md).
            let flat = skills_root.join("SKILL.md");
            if flat.is_file() {
                push_source(
                    &mut tree.skills,
                    AssetSource {
                        tier,
                        provider: pkg_id.to_string(),
                        path: flat.to_string_lossy().to_string(),
                        name: pkg_id.to_string(),
                        kind: AssetKind::Skill,
                    },
                );
            }
        }
    }
}

fn scan_md_recursive(dir: &Path, pkg_id: &str, tier: Tier, kind: AssetKind, tree: &mut AssetTree) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        if p.is_dir() {
            scan_md_recursive(&p, pkg_id, tier, kind, tree);
            continue;
        }
        if !p.is_file() {
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if name.starts_with('_') {
            continue;
        }
        let map = match kind {
            AssetKind::Agent => &mut tree.agents,
            AssetKind::Command => &mut tree.commands,
            _ => unreachable!("scan_md_recursive expects agent or command"),
        };
        push_source(
            map,
            AssetSource {
                tier,
                provider: pkg_id.to_string(),
                path: p.to_string_lossy().to_string(),
                name,
                kind,
            },
        );
    }
}

/// Surface the pkg's manifest `mcp[]` entries as tier-tagged MCP sources.
fn scan_pkg_mcps(summary: &crate::pkg::kernel::InstalledSummary, tier: Tier, tree: &mut AssetTree) {
    let manifest_path = Path::new(&summary.install_path).join("manifest.json");
    let raw = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let manifest: crate::pkg::manifest::Manifest = match serde_json::from_str(&raw) {
        Ok(m) => m,
        Err(_) => return,
    };
    let path_str = manifest_path.to_string_lossy().to_string();
    for server in &manifest.mcp {
        push_source(
            &mut tree.mcps,
            AssetSource {
                tier,
                provider: summary.id.clone(),
                path: path_str.clone(),
                name: server.name.clone(),
                kind: AssetKind::Mcp,
            },
        );
    }
}

// ─── Tier 3 — project-rooted files ─────────────────────────────────────────

fn scan_project_files(claude: &Path, project_id: &str, provider: &str, tree: &mut AssetTree) {
    let _ = project_id;
    // Agents.
    if let Ok(rd) = std::fs::read_dir(claude.join("agents")) {
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let name = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if name.starts_with('_') {
                continue;
            }
            push_source(
                &mut tree.agents,
                AssetSource {
                    tier: Tier::Project,
                    provider: provider.to_string(),
                    path: p.to_string_lossy().to_string(),
                    name,
                    kind: AssetKind::Agent,
                },
            );
        }
    }
    // Commands.
    if let Ok(rd) = std::fs::read_dir(claude.join("commands")) {
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if p.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let name = match p.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if name.starts_with('_') {
                continue;
            }
            push_source(
                &mut tree.commands,
                AssetSource {
                    tier: Tier::Project,
                    provider: provider.to_string(),
                    path: p.to_string_lossy().to_string(),
                    name,
                    kind: AssetKind::Command,
                },
            );
        }
    }
    // Skills.
    let skills_dir = claude.join("skills");
    if let Ok(rd) = std::fs::read_dir(&skills_dir) {
        for entry in rd.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            let name = match skill_dir.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            let skill_md = skill_dir.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            push_source(
                &mut tree.skills,
                AssetSource {
                    tier: Tier::Project,
                    provider: provider.to_string(),
                    path: skill_md.to_string_lossy().to_string(),
                    name,
                    kind: AssetKind::Skill,
                },
            );
        }
    }
}

// ─── Hook + MCP helpers (wrap legacy parsers) ──────────────────────────────

fn push_hooks(
    path: &Path,
    scope: LegacyScope,
    project_root: Option<&str>,
    tier: Tier,
    provider: &str,
    tree: &mut AssetTree,
) {
    let Ok(entries) = parse_hooks_file(path, scope, project_root) else {
        return;
    };
    for h in entries {
        // Synthesize a stable name: "<event>/<name>".
        let name = format!("{}/{}", h.event, hook_name(&h));
        push_source(
            &mut tree.hooks,
            AssetSource {
                tier,
                provider: provider.to_string(),
                path: hook_path(&h),
                name,
                kind: AssetKind::Hook,
            },
        );
    }
}

fn hook_name(h: &HookEntry) -> &str {
    h.name.as_str()
}

fn hook_path(h: &HookEntry) -> String {
    h.command_path
        .clone()
        .unwrap_or_else(|| h.settings_path.clone())
}

fn push_mcps_from_settings(
    path: &Path,
    scope: LegacyScope,
    project_root: Option<&str>,
    tier: Tier,
    provider: &str,
    tree: &mut AssetTree,
) {
    let Ok(entries) = parse_mcp_from_settings(path, scope, project_root) else {
        return;
    };
    push_mcps_inner(entries, tier, provider, tree);
}

fn push_mcps_from_mcp_file(
    path: &Path,
    scope: LegacyScope,
    project_root: Option<&str>,
    tier: Tier,
    provider: &str,
    tree: &mut AssetTree,
) {
    let Ok(entries) = parse_mcp_file(path, scope, project_root) else {
        return;
    };
    push_mcps_inner(entries, tier, provider, tree);
}

fn push_mcps_inner(entries: Vec<McpEntry>, tier: Tier, provider: &str, tree: &mut AssetTree) {
    for m in entries {
        push_source(
            &mut tree.mcps,
            AssetSource {
                tier,
                provider: provider.to_string(),
                path: m.path,
                name: m.name,
                kind: AssetKind::Mcp,
            },
        );
    }
}

// ─── Generic helpers ───────────────────────────────────────────────────────

fn push_source(map: &mut HashMap<String, Vec<AssetSource>>, src: AssetSource) {
    map.entry(src.name.clone()).or_default().push(src);
}

fn home_dir() -> Option<PathBuf> {
    // Raw $HOME is unset on Windows; route through the shared resolver.
    crate::platform::home_dir()
}

fn expand(input: &str) -> Result<PathBuf, String> {
    shellexpand::full(input)
        .map(|c| PathBuf::from(c.into_owned()))
        .map_err(|e| format!("shellexpand: {e}"))
}

// ─── Pin resolution ────────────────────────────────────────────────────────

/// Pick the preferred source for an asset given the current pin map.
///
/// Default behaviour: lowest tier wins (precedence asc).
/// Pin behaviour: pin matches a source iff its `tier` is `preferred_tier`
/// AND (preferred_source is None OR source.provider == preferred_source).
pub fn resolve_preferred<'a>(
    name: &str,
    sources: &'a [AssetSource],
    pins: &PinMap,
) -> &'a AssetSource {
    debug_assert!(!sources.is_empty(), "resolve_preferred on empty sources");
    if sources.len() == 1 {
        return &sources[0];
    }
    let kind = sources[0].kind;
    if let Some((tier, provider)) = pins.get(&(kind, name.to_string())) {
        if let Some(s) = sources
            .iter()
            .find(|s| s.tier == *tier && provider.as_deref().map_or(true, |p| s.provider == p))
        {
            return s;
        }
    }
    sources
        .iter()
        .min_by_key(|s| s.tier.precedence())
        .expect("non-empty by debug_assert")
}

// ─── Pin storage helpers ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetPin {
    pub scope: String,
    pub asset_kind: String,
    pub asset_name: String,
    pub preferred_tier: String,
    pub preferred_source: Option<String>,
    pub updated_at: i64,
}

/// Load every pin for a scope, indexed by `(kind, name)` so callers can look
/// up directly.
pub async fn load_pins(pool: &SqlitePool, scope: &str) -> Result<PinMap, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT asset_kind, asset_name, preferred_tier, preferred_source
         FROM claude_asset_preferences
         WHERE scope = ?",
    )
    .bind(scope)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load pins: {e}"))?;
    let mut out: PinMap = HashMap::new();
    for r in rows {
        let kind_str: String = r.get("asset_kind");
        let name: String = r.get("asset_name");
        let tier_str: String = r.get("preferred_tier");
        let source: Option<String> = r.get("preferred_source");
        let (Some(kind), Some(tier)) = (AssetKind::from_str(&kind_str), Tier::from_str(&tier_str))
        else {
            continue;
        };
        out.insert((kind, name), (tier, source));
    }
    Ok(out)
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_precedence_order() {
        assert!(Tier::Personal.precedence() < Tier::WorkspacePkg.precedence());
        assert!(Tier::WorkspacePkg.precedence() < Tier::Project.precedence());
        assert!(Tier::Project.precedence() < Tier::ProjectPkg.precedence());
    }

    #[test]
    fn resolve_default_is_lowest_tier() {
        let sources = vec![
            AssetSource {
                tier: Tier::ProjectPkg,
                provider: "p".into(),
                path: "/p".into(),
                name: "x".into(),
                kind: AssetKind::Skill,
            },
            AssetSource {
                tier: Tier::Personal,
                provider: "personal".into(),
                path: "/h".into(),
                name: "x".into(),
                kind: AssetKind::Skill,
            },
        ];
        let pins = PinMap::new();
        let picked = resolve_preferred("x", &sources, &pins);
        assert_eq!(picked.tier, Tier::Personal);
    }

    #[test]
    fn pin_overrides_default() {
        let sources = vec![
            AssetSource {
                tier: Tier::Personal,
                provider: "personal".into(),
                path: "/h".into(),
                name: "x".into(),
                kind: AssetKind::Skill,
            },
            AssetSource {
                tier: Tier::Project,
                provider: "project:music".into(),
                path: "/m".into(),
                name: "x".into(),
                kind: AssetKind::Skill,
            },
        ];
        let mut pins = PinMap::new();
        pins.insert((AssetKind::Skill, "x".into()), (Tier::Project, None));
        let picked = resolve_preferred("x", &sources, &pins);
        assert_eq!(picked.tier, Tier::Project);
    }

    #[test]
    fn pin_with_provider_matches_exact() {
        let sources = vec![
            AssetSource {
                tier: Tier::WorkspacePkg,
                provider: "com.example.a".into(),
                path: "/a".into(),
                name: "x".into(),
                kind: AssetKind::Agent,
            },
            AssetSource {
                tier: Tier::WorkspacePkg,
                provider: "com.example.b".into(),
                path: "/b".into(),
                name: "x".into(),
                kind: AssetKind::Agent,
            },
        ];
        let mut pins = PinMap::new();
        pins.insert(
            (AssetKind::Agent, "x".into()),
            (Tier::WorkspacePkg, Some("com.example.b".into())),
        );
        let picked = resolve_preferred("x", &sources, &pins);
        assert_eq!(picked.provider, "com.example.b");
    }

    #[test]
    fn pin_with_missing_target_falls_back_to_default() {
        let sources = vec![
            AssetSource {
                tier: Tier::Personal,
                provider: "personal".into(),
                path: "/h".into(),
                name: "x".into(),
                kind: AssetKind::Command,
            },
            AssetSource {
                tier: Tier::WorkspacePkg,
                provider: "com.x".into(),
                path: "/p".into(),
                name: "x".into(),
                kind: AssetKind::Command,
            },
        ];
        let mut pins = PinMap::new();
        // Pin says ProjectPkg but no such source exists.
        pins.insert((AssetKind::Command, "x".into()), (Tier::ProjectPkg, None));
        let picked = resolve_preferred("x", &sources, &pins);
        assert_eq!(picked.tier, Tier::Personal);
    }
}
