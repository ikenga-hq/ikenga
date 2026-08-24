//! `/claude` config browser backend.
//!
//! Scans the on-disk Claude Code config (agents/skills/commands/hooks) across
//! a user-managed list of project roots plus the personal `~/.claude` dir.
//! There is no `claude` CLI subcommand to list these, so we read the FS
//! directly. Read-only — never writes.
//!
//! Paths scanned for each project root `<P>`:
//!   - `<P>/.claude/agents/*.md`           — agent definitions
//!   - `<P>/.claude/skills/<name>/SKILL.md`
//!   - `<P>/.claude/commands/*.md`         — slash commands
//!   - `<P>/.claude/settings.local.json`   — hook config (one of the keys)
//!
//! Personal:
//!   - `~/.claude/agents/*.md`
//!   - `~/.claude/skills/<name>/SKILL.md`
//!   - `~/.claude/commands/*.md`
//!   - `~/.claude/settings.json`
//!
//! These paths are not in the global Tauri allowlist (`is_allowed`) but the
//! commands here are read-only and constrained to the structure above, so
//! they call `std::fs` directly with input paths normalized through
//! `shellexpand`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::commands::engine_layout::{
    engine_layouts, ConfigFormat, EngineId, EngineLayout, KindStatus, PrimitiveKind, ScopeTier,
};
use crate::fs_watch::FsWatchManager;

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    Personal,
}

/// Additive cross-system tag carried by every scan entry (WP-17).
///
/// Holds the three frozen entry-extension fields (`system` / `format` /
/// `status`) the Phase-2 cross-system scan adds. It is `#[serde(flatten)]`-ed
/// into every entry struct and has a **hand-rolled `Serialize`** so that
/// **Claude entries serialize byte-for-byte unchanged**: when `system ==
/// Claude` *and* `status == Active` the tag emits **zero** fields (the pre-WP-17
/// wire), so a Claude-only scan is identical to before. Gemini/Codex entries
/// emit all three camelCase fields (`system` / `format` / `status`) with
/// kebab-case enum values, matching the frozen contract WP-19 mirrors in TS.
///
/// The fields map 1:1 onto the frozen `EngineLayout` enums (reused, not
/// re-declared): `EngineId` (wire `"claude"|"gemini"|"codex"`, serde default
/// claude), `ConfigFormat` (`"md-yaml"|"toml"|"json-embedded"`), `KindStatus`
/// (`"active"|"deprecated"`, serde default active). Entry structs are
/// Serialize-only, so no `Deserialize` is needed on the Rust side; the serde
/// defaults documented here are the contract WP-19's TS deserializer honors.
#[derive(Debug, Clone, Copy)]
pub struct SystemTag {
    pub system: EngineId,
    pub format: ConfigFormat,
    pub status: KindStatus,
}

impl SystemTag {
    /// The legacy Claude default for a given kind's format — used so the
    /// shipping single-engine (Claude) scan keeps emitting the same bytes
    /// (i.e. NO `system`/`format`/`status` keys at all).
    fn claude(format: ConfigFormat) -> Self {
        SystemTag {
            system: EngineId::Claude,
            format,
            status: KindStatus::Active,
        }
    }
}

impl Serialize for SystemTag {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        // Byte-compat fast path: a Claude/Active entry is the pre-WP-17 shape —
        // emit nothing so existing Claude scan output is unchanged.
        if self.system == EngineId::Claude && self.status == KindStatus::Active {
            return serializer.serialize_map(Some(0))?.end();
        }
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("system", &self.system)?;
        map.serialize_entry("format", &self.format)?;
        map.serialize_entry("status", &self.status)?;
        map.end()
    }
}

/// Symlink / central-store metadata for an on-disk primitive (file or dir).
///
/// Additive, back-compatible scanner enrichment for the Ngwa store layer
/// (WP-02/03). For file-based primitives (agents, commands → `.md`; skills →
/// skill dir) these are computed from an `lstat` on the primitive path. For
/// JSON-fragment primitives that have no standalone symlinkable file (hooks,
/// MCP servers — they live as keys inside a settings JSON) the fields default
/// to `is_symlink = false`, `link_target = None`, `in_store = false`.
///
/// `in_store` is true when the *resolved* target (the symlink destination, or
/// the path itself when not a link) lives under the Ngwa central store root
/// (`<app_data_dir>/store/`). The store root is resolved with the same
/// platform conventions Tauri uses for `app_data_dir` (mirrors
/// `vault_key.rs`); WP-02 owns the canonical store layout under it.
#[derive(Debug, Clone, Default)]
pub struct LinkMeta {
    pub is_symlink: bool,
    pub link_target: Option<String>,
    pub in_store: bool,
    /// WP-03: whether the link resolves to an existing target. `false` only for
    /// a dangling symlink; `true` for a resolving symlink or a non-symlink
    /// present path. Drives the linked-vs-orphaned classification downstream.
    pub target_exists: bool,
}

#[derive(Debug, Serialize)]
pub struct AgentEntry {
    pub name: String,
    pub scope: Scope,
    /// Project root this entry belongs to (`null` for personal).
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    pub path: String,
    /// File mtime (epoch ms).
    #[serde(rename = "modifiedMs")]
    pub modified_ms: i64,
    pub description: Option<String>,
    pub model: Option<String>,
    /// All frontmatter rendered as JSON for the detail-view grid.
    pub frontmatter: serde_json::Value,
    /// Markdown body (everything after the closing `---`).
    pub body: String,
    /// True if a project entry of the same name overrides this personal one.
    #[serde(rename = "overriddenBy")]
    pub overridden_by: Option<String>,
    /// Whether the on-disk primitive file is a symlink.
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    /// Resolved symlink target path (`null` when not a symlink).
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
    /// Whether the resolved target lives inside the Ngwa central store.
    #[serde(rename = "inStore")]
    pub in_store: bool,
    /// WP-03: whether a symlink resolves to an existing target. `false` = a
    /// dangling link (orphaned); `true` = linked — a valid master, *regardless*
    /// of `in_store`. Always `true` for a non-symlink present entry.
    #[serde(rename = "targetExists")]
    pub target_exists: bool,
    /// Cross-system tag (`system`/`format`/`status`). Omitted for Claude.
    #[serde(flatten)]
    pub tag: SystemTag,
}

#[derive(Debug, Serialize)]
pub struct SkillEntry {
    pub name: String,
    pub scope: Scope,
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    pub path: String,
    /// Skill dir (parent of SKILL.md). Used to enumerate supporting files.
    #[serde(rename = "dirPath")]
    pub dir_path: String,
    #[serde(rename = "modifiedMs")]
    pub modified_ms: i64,
    pub description: Option<String>,
    pub frontmatter: serde_json::Value,
    pub body: String,
    /// Sibling files in the skill dir (excluding SKILL.md).
    #[serde(rename = "supportingFiles")]
    pub supporting_files: Vec<SupportingFile>,
    #[serde(rename = "overriddenBy")]
    pub overridden_by: Option<String>,
    /// Whether the skill dir is a symlink (skills are symlinked dir-wise).
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    /// Resolved symlink target dir (`null` when not a symlink).
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
    /// Whether the resolved target dir lives inside the Ngwa central store.
    #[serde(rename = "inStore")]
    pub in_store: bool,
    /// WP-03: whether a symlink resolves to an existing target. `false` = a
    /// dangling link (orphaned); `true` = linked — a valid master, *regardless*
    /// of `in_store`. Always `true` for a non-symlink present entry.
    #[serde(rename = "targetExists")]
    pub target_exists: bool,
    /// Cross-system tag (`system`/`format`/`status`). Omitted for Claude.
    #[serde(flatten)]
    pub tag: SystemTag,
}

#[derive(Debug, Serialize)]
pub struct SupportingFile {
    pub name: String,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct CommandEntry {
    pub name: String,
    pub scope: Scope,
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    pub path: String,
    #[serde(rename = "modifiedMs")]
    pub modified_ms: i64,
    pub description: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "argumentHint")]
    pub argument_hint: Option<String>,
    pub frontmatter: serde_json::Value,
    pub body: String,
    #[serde(rename = "overriddenBy")]
    pub overridden_by: Option<String>,
    /// Whether the on-disk command file is a symlink.
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    /// Resolved symlink target path (`null` when not a symlink).
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
    /// Whether the resolved target lives inside the Ngwa central store.
    #[serde(rename = "inStore")]
    pub in_store: bool,
    /// WP-03: whether a symlink resolves to an existing target. `false` = a
    /// dangling link (orphaned); `true` = linked — a valid master, *regardless*
    /// of `in_store`. Always `true` for a non-symlink present entry.
    #[serde(rename = "targetExists")]
    pub target_exists: bool,
    /// Cross-system tag (`system`/`format`/`status`). Omitted for Claude.
    #[serde(flatten)]
    pub tag: SystemTag,
}

#[derive(Debug, Serialize)]
pub struct McpEntry {
    /// Server name (key under `mcpServers`).
    pub name: String,
    pub scope: Scope,
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    /// Source file (e.g. `.claude/mcp.json`).
    pub path: String,
    /// Transport: `stdio`, `http`, or `sse`.
    pub transport: String,
    /// stdio: program; http/sse: null.
    pub command: Option<String>,
    /// stdio args.
    pub args: Vec<String>,
    /// Env-var keys declared (values stripped — they may be secrets).
    #[serde(rename = "envKeys")]
    pub env_keys: Vec<String>,
    /// http/sse: server URL.
    pub url: Option<String>,
    /// http/sse: header names declared.
    #[serde(rename = "headerKeys")]
    pub header_keys: Vec<String>,
    /// Raw entry JSON for the detail preview.
    pub raw: serde_json::Value,
    /// Always `false` — MCP servers are JSON keys, not symlinkable files.
    /// Present for a uniform entry contract; the store layer toggles MCP
    /// servers via JSON merge/unmerge, not the symlink farm.
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    /// Always `null` for MCP servers (no standalone file).
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
    /// Always `false` for MCP servers.
    #[serde(rename = "inStore")]
    pub in_store: bool,
    /// WP-03: whether a symlink resolves to an existing target. `false` = a
    /// dangling link (orphaned); `true` = linked — a valid master, *regardless*
    /// of `in_store`. Always `true` for a non-symlink present entry.
    #[serde(rename = "targetExists")]
    pub target_exists: bool,
    /// Cross-system tag (`system`/`format`/`status`). Omitted for Claude.
    #[serde(flatten)]
    pub tag: SystemTag,
}

#[derive(Debug, Serialize)]
pub struct HookEntry {
    /// Hook event name: `SessionStart`, `Stop`, `PreCompact`, `PreToolUse`, ...
    pub event: String,
    /// Hook type: `command`, `prompt`, etc.
    #[serde(rename = "type")]
    pub kind: String,
    /// Display name — basename of the command path or the inline matcher.
    pub name: String,
    pub scope: Scope,
    #[serde(rename = "projectRoot")]
    pub project_root: Option<String>,
    /// Path to the settings file the hook is declared in.
    #[serde(rename = "settingsPath")]
    pub settings_path: String,
    /// Resolved absolute path to the script (only when `kind == "command"`).
    #[serde(rename = "commandPath")]
    pub command_path: Option<String>,
    /// Raw command string as written in settings.json (unresolved).
    #[serde(rename = "commandRaw")]
    pub command_raw: Option<String>,
    /// Raw entry as JSON for the JSON config preview.
    pub raw: serde_json::Value,
    /// Always `false` — hooks are JSON entries in settings, not symlinkable
    /// files. Present for a uniform entry contract; the store layer toggles
    /// hooks via JSON merge/unmerge, not the symlink farm.
    #[serde(rename = "isSymlink")]
    pub is_symlink: bool,
    /// Always `null` for hooks (no standalone primitive file).
    #[serde(rename = "linkTarget")]
    pub link_target: Option<String>,
    /// Always `false` for hooks.
    #[serde(rename = "inStore")]
    pub in_store: bool,
    /// WP-03: whether a symlink resolves to an existing target. `false` = a
    /// dangling link (orphaned); `true` = linked — a valid master, *regardless*
    /// of `in_store`. Always `true` for a non-symlink present entry.
    #[serde(rename = "targetExists")]
    pub target_exists: bool,
    /// Cross-system tag (`system`/`format`/`status`). Omitted for Claude.
    #[serde(flatten)]
    pub tag: SystemTag,
}

#[derive(Debug, Serialize)]
pub struct ClaudeConfig {
    pub agents: Vec<AgentEntry>,
    pub skills: Vec<SkillEntry>,
    pub commands: Vec<CommandEntry>,
    pub hooks: Vec<HookEntry>,
    pub mcps: Vec<McpEntry>,
    /// Scan errors per root (path → message). Surfaced in the UI footer.
    pub errors: Vec<ScanError>,
}

#[derive(Debug, Serialize)]
pub struct ScanError {
    pub path: String,
    pub message: String,
}

// ─── Public commands ────────────────────────────────────────────────────────

/// Scan all project roots + personal dir, return a single config tree.
#[tauri::command]
pub async fn claude_config_load(
    #[allow(non_snake_case)] projectRoots: Vec<String>,
) -> Result<ClaudeConfig, String> {
    let project_roots = projectRoots;
    tokio::task::spawn_blocking(move || scan_all(project_roots))
        .await
        .map_err(|e| format!("join failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// Watch every `.claude/` dir in the supplied roots + the personal `~/.claude/`.
/// One watcher per dir, all emitting on the same event channel
/// `claude-config:changed`. Returns the list of watcher ids so the frontend
/// can release them on unmount via `claude_config_unwatch`.
#[tauri::command]
pub async fn claude_config_watch(
    app: AppHandle,
    manager: State<'_, Arc<FsWatchManager>>,
    #[allow(non_snake_case)] projectRoots: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    for root in &projectRoots {
        if let Ok(p) = expand(root) {
            let claude_dir = p.join(".claude");
            if claude_dir.is_dir() {
                targets.push(claude_dir);
            }
        }
    }
    if let Some(home) = home_dir() {
        let personal = home.join(".claude");
        if personal.is_dir() {
            targets.push(personal);
        }
    }
    for t in targets {
        match manager.watch(app.clone(), &t) {
            Ok(id) => ids.push(id),
            Err(e) => log::warn!("claude_config_watch failed for {}: {e}", t.display()),
        }
    }
    Ok(ids)
}

#[tauri::command]
pub async fn claude_config_unwatch(
    manager: State<'_, Arc<FsWatchManager>>,
    #[allow(non_snake_case)] watcherIds: Vec<String>,
) -> Result<(), String> {
    for id in watcherIds {
        let _ = manager.unwatch(&id);
    }
    Ok(())
}

/// Read an arbitrary file under a `.claude/` dir (e.g. a hook script body or a
/// skill supporting file). Restricted to paths whose canonical form sits under
/// either a `.claude/` segment or `~/.claude/`. Read-only.
#[tauri::command]
pub async fn claude_config_read_file(path: String) -> Result<String, String> {
    let resolved = expand(&path).map_err(|e| e.to_string())?;
    if !is_under_claude_dir(&resolved) {
        return Err(format!(
            "path not under a .claude/ dir: {}",
            resolved.display()
        ));
    }
    tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|e| format!("read failed: {e}"))
}

// ─── Scanning ──────────────────────────────────────────────────────────────

fn scan_all(project_roots: Vec<String>) -> Result<ClaudeConfig> {
    let mut agents: Vec<AgentEntry> = Vec::new();
    let mut skills: Vec<SkillEntry> = Vec::new();
    let mut commands: Vec<CommandEntry> = Vec::new();
    let mut hooks: Vec<HookEntry> = Vec::new();
    let mut mcps: Vec<McpEntry> = Vec::new();
    let mut errors: Vec<ScanError> = Vec::new();

    // Resolve the Ngwa central store root once for the whole scan; threaded
    // into the file-based scanners so each primitive can report `in_store`.
    let store = store_root();
    let store_ref = store.as_deref();

    let mut acc = ScanAcc {
        agents: &mut agents,
        skills: &mut skills,
        commands: &mut commands,
        hooks: &mut hooks,
        mcps: &mut mcps,
        errors: &mut errors,
    };

    // ── Engine-parametric scan loop ──────────────────────────────────────
    //
    // Driven by the frozen `EngineLayout` descriptor (G-ADAPTER). Each engine
    // owns a per-system reader that reads primitives at the engine's declared
    // location/format and stamps entries with the engine's `SystemTag`
    // (`system`/`format`/`status`). The Claude reader is the shipping Phase-1
    // path verbatim — its tag serializes to nothing, so a Claude-only scan is
    // byte-identical to before. WP-18 fills in the Codex arm below.
    for layout in engine_layouts() {
        match layout.engine {
            EngineId::Claude => scan_claude(&project_roots, store_ref, &mut acc),
            EngineId::Gemini => scan_gemini(&layout, &project_roots, store_ref, &mut acc),
            EngineId::Antigravity => scan_gemini(&layout, &project_roots, store_ref, &mut acc),
            // WP-18: the Codex reader — TOML agents/mcp/(inline)hooks +
            // JSON hooks.json + cross-tool `.agents/skills/` + deprecated
            // `prompts/`. Missing files/dirs yield zero entries (NOT an error).
            EngineId::Codex => scan_codex(&layout, &project_roots, store_ref, &mut acc),
        }
    }

    // Override resolution: any project entry with the same name wins; mark the
    // personal twin with `overridden_by`.
    mark_overrides_agents(&mut agents);
    mark_overrides_skills(&mut skills);
    mark_overrides_commands(&mut commands);

    // Sort each list by name asc.
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    hooks.sort_by(|a, b| a.event.cmp(&b.event).then_with(|| a.name.cmp(&b.name)));
    mcps.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(ClaudeConfig {
        agents,
        skills,
        commands,
        hooks,
        mcps,
        errors,
    })
}

/// Mutable accumulator threaded through every per-engine reader. Bundling the
/// five entry lists + the error list keeps the per-engine reader signatures
/// flat as more engines land (WP-18 Codex).
struct ScanAcc<'a> {
    agents: &'a mut Vec<AgentEntry>,
    skills: &'a mut Vec<SkillEntry>,
    commands: &'a mut Vec<CommandEntry>,
    hooks: &'a mut Vec<HookEntry>,
    mcps: &'a mut Vec<McpEntry>,
    errors: &'a mut Vec<ScanError>,
}

/// Claude reader — the shipping Phase-1 scan, unchanged. Walks the
/// user-supplied project roots' `.claude/` dirs + the personal `~/.claude/`.
/// Entries carry the Claude `SystemTag`, which serializes to nothing, so this
/// path's output is byte-identical to the pre-WP-17 single-engine scan.
fn scan_claude(project_roots: &[String], store: Option<&Path>, acc: &mut ScanAcc<'_>) {
    // Project scope.
    for raw in project_roots {
        let root = match expand(raw) {
            Ok(p) => p,
            Err(e) => {
                acc.errors.push(ScanError {
                    path: raw.clone(),
                    message: format!("expand: {e}"),
                });
                continue;
            }
        };
        let claude = root.join(".claude");
        if !claude.is_dir() {
            // Silent skip — many project roots may not have a .claude/ dir.
            continue;
        }
        scan_project(
            &root,
            &claude,
            store,
            acc.agents,
            acc.skills,
            acc.commands,
            acc.hooks,
            acc.mcps,
            acc.errors,
        );
    }

    // Personal scope.
    if let Some(home) = home_dir() {
        let personal = home.join(".claude");
        if personal.is_dir() {
            scan_personal(
                &personal,
                store,
                acc.agents,
                acc.skills,
                acc.commands,
                acc.hooks,
                acc.mcps,
                acc.errors,
            );
        }
    }
}

fn scan_project(
    root: &Path,
    claude: &Path,
    store: Option<&Path>,
    agents: &mut Vec<AgentEntry>,
    skills: &mut Vec<SkillEntry>,
    commands: &mut Vec<CommandEntry>,
    hooks: &mut Vec<HookEntry>,
    mcps: &mut Vec<McpEntry>,
    errors: &mut Vec<ScanError>,
) {
    let root_str = root.to_string_lossy().to_string();
    let md_tag = SystemTag::claude(ConfigFormat::MdYaml);
    scan_md_dir(
        &claude.join("agents"),
        Scope::Project,
        Some(&root_str),
        store,
        md_tag,
        |entry| agents.push(agent_from_md(entry)),
        errors,
    );
    scan_skills_dir(
        &claude.join("skills"),
        Scope::Project,
        Some(&root_str),
        store,
        md_tag,
        skills,
        errors,
    );
    scan_md_dir(
        &claude.join("commands"),
        Scope::Project,
        Some(&root_str),
        store,
        md_tag,
        |entry| commands.push(command_from_md(entry)),
        errors,
    );

    // Hooks + inline mcpServers: read both settings.local.json and settings.json.
    for f in ["settings.local.json", "settings.json"] {
        let p = claude.join(f);
        if p.is_file() {
            match parse_hooks_file(&p, Scope::Project, Some(&root_str)) {
                Ok(mut h) => hooks.append(&mut h),
                Err(e) => errors.push(ScanError {
                    path: p.to_string_lossy().to_string(),
                    message: format!("hooks parse: {e}"),
                }),
            }
            match parse_mcp_from_settings(&p, Scope::Project, Some(&root_str)) {
                Ok(mut m) => mcps.append(&mut m),
                Err(e) => errors.push(ScanError {
                    path: p.to_string_lossy().to_string(),
                    message: format!("mcp(settings) parse: {e}"),
                }),
            }
        }
    }

    // Standalone .claude/mcp.json (canonical project MCP file).
    let mcp_json = claude.join("mcp.json");
    if mcp_json.is_file() {
        match parse_mcp_file(&mcp_json, Scope::Project, Some(&root_str)) {
            Ok(mut m) => mcps.append(&mut m),
            Err(e) => errors.push(ScanError {
                path: mcp_json.to_string_lossy().to_string(),
                message: format!("mcp parse: {e}"),
            }),
        }
    }
}

fn scan_personal(
    personal: &Path,
    store: Option<&Path>,
    agents: &mut Vec<AgentEntry>,
    skills: &mut Vec<SkillEntry>,
    commands: &mut Vec<CommandEntry>,
    hooks: &mut Vec<HookEntry>,
    mcps: &mut Vec<McpEntry>,
    errors: &mut Vec<ScanError>,
) {
    let md_tag = SystemTag::claude(ConfigFormat::MdYaml);
    scan_md_dir(
        &personal.join("agents"),
        Scope::Personal,
        None,
        store,
        md_tag,
        |entry| agents.push(agent_from_md(entry)),
        errors,
    );
    scan_skills_dir(
        &personal.join("skills"),
        Scope::Personal,
        None,
        store,
        md_tag,
        skills,
        errors,
    );
    scan_md_dir(
        &personal.join("commands"),
        Scope::Personal,
        None,
        store,
        md_tag,
        |entry| commands.push(command_from_md(entry)),
        errors,
    );
    let p = personal.join("settings.json");
    if p.is_file() {
        match parse_hooks_file(&p, Scope::Personal, None) {
            Ok(mut h) => hooks.append(&mut h),
            Err(e) => errors.push(ScanError {
                path: p.to_string_lossy().to_string(),
                message: format!("hooks parse: {e}"),
            }),
        }
        match parse_mcp_from_settings(&p, Scope::Personal, None) {
            Ok(mut m) => mcps.append(&mut m),
            Err(e) => errors.push(ScanError {
                path: p.to_string_lossy().to_string(),
                message: format!("mcp(settings) parse: {e}"),
            }),
        }
    }
    // Personal-level top-level mcp.json or ~/.claude.json `mcpServers`.
    let mcp_json = personal.join("mcp.json");
    if mcp_json.is_file() {
        match parse_mcp_file(&mcp_json, Scope::Personal, None) {
            Ok(mut m) => mcps.append(&mut m),
            Err(e) => errors.push(ScanError {
                path: mcp_json.to_string_lossy().to_string(),
                message: format!("mcp parse: {e}"),
            }),
        }
    }
    // Top-level `~/.claude.json` (Claude Code's user-level config). Only the
    // root-level `mcpServers` key — per-project keys inside `projects` are
    // user toggles, not server defs.
    if let Some(home) = home_dir() {
        let user_json = home.join(".claude.json");
        if user_json.is_file() {
            match parse_mcp_from_settings(&user_json, Scope::Personal, None) {
                Ok(mut m) => mcps.append(&mut m),
                Err(e) => errors.push(ScanError {
                    path: user_json.to_string_lossy().to_string(),
                    message: format!("mcp(~/.claude.json) parse: {e}"),
                }),
            }
        }
    }
}

// ─── Gemini reader (WP-17) ──────────────────────────────────────────────────
//
// Reads `~/.gemini` (user) + each `<root>/.gemini` (project) per the frozen
// `EngineLayout`. READ-ONLY — never writes. Missing files/dirs yield zero
// entries (NOT an error). Scope mapping reuses the existing `Scope` enum: the
// Gemini `ScopeDef` whose tier is `User` maps to `Scope::Personal`, `Project`
// to `Scope::Project` (the frozen entry-extension contract: "Scope reuses the
// existing entry scope representation").

/// Map a frozen `EngineLayout` scope tier onto the existing entry `Scope`.
fn scope_for_tier(tier: ScopeTier) -> Scope {
    match tier {
        ScopeTier::User => Scope::Personal,
        ScopeTier::Project => Scope::Project,
    }
}

fn scan_gemini(
    layout: &EngineLayout,
    project_roots: &[String],
    store: Option<&Path>,
    acc: &mut ScanAcc<'_>,
) {
    let Some(home) = home_dir() else {
        return;
    };
    // Per-engine path confinement: every read this reader performs must sit
    // under one of Gemini's declared roots (see `engine_roots`).
    let roots = engine_roots(layout, project_roots);
    let confined = |p: &Path| path_under_any(p, &roots);

    // Scope values derived from the frozen layout's tiers (not hardcoded), so
    // the entry `Scope` always reflects the descriptor's user/project mapping.
    let user_scope = layout
        .scopes
        .iter()
        .find(|s| s.tier == ScopeTier::User)
        .map(|s| scope_for_tier(s.tier))
        .unwrap_or(Scope::Personal);
    let project_scope = layout
        .scopes
        .iter()
        .find(|s| s.tier == ScopeTier::Project)
        .map(|s| scope_for_tier(s.tier))
        .unwrap_or(Scope::Project);

    // ── User scope (`~/.gemini` + cross-tool `~/.agents`) ────────────────
    let user = home.join(".gemini");
    let agents_md = SystemTag {
        system: EngineId::Gemini,
        format: ConfigFormat::MdYaml,
        status: KindStatus::Active,
    };
    let cmd_toml = SystemTag {
        system: EngineId::Gemini,
        format: ConfigFormat::Toml,
        status: KindStatus::Active,
    };
    let json_embedded = SystemTag {
        system: EngineId::Gemini,
        format: ConfigFormat::JsonEmbedded,
        status: KindStatus::Active,
    };

    // skills: `~/.gemini/skills/` AND cross-tool `~/.agents/skills/`.
    for dir in [user.join("skills"), home.join(".agents/skills")] {
        if confined(&dir) {
            scan_skills_dir(
                &dir, user_scope, None, store, agents_md, acc.skills, acc.errors,
            );
        }
    }
    // agents: `~/.gemini/agents/*.md`.
    let agents_dir = user.join("agents");
    if confined(&agents_dir) {
        scan_md_dir(
            &agents_dir,
            user_scope,
            None,
            store,
            agents_md,
            |e| acc.agents.push(agent_from_md(e)),
            acc.errors,
        );
    }
    // commands: `~/.gemini/commands/**/*.toml` (subdir-namespaced).
    let commands_dir = user.join("commands");
    if confined(&commands_dir) {
        scan_gemini_commands(&commands_dir, user_scope, None, store, cmd_toml, acc);
    }
    // hooks + mcp: keys inside `~/.gemini/settings.json`.
    let settings = user.join("settings.json");
    if settings.is_file() && confined(&settings) {
        gemini_settings(&settings, user_scope, None, json_embedded, acc);
    }

    // ── Project scope (`<root>/.gemini` + cross-tool `<root>/.agents`) ────
    for raw in project_roots {
        let root = match expand(raw) {
            Ok(p) => p,
            Err(_) => continue, // Claude reader already surfaced expand errors.
        };
        let root_str = root.to_string_lossy().to_string();
        let gem = root.join(".gemini");

        // skills: project `<root>/.agents/skills/`.
        let skills_dir = root.join(".agents/skills");
        if confined(&skills_dir) {
            scan_skills_dir(
                &skills_dir,
                project_scope,
                Some(&root_str),
                store,
                agents_md,
                acc.skills,
                acc.errors,
            );
        }
        // agents: `<root>/.gemini/agents/*.md`.
        let agents_dir = gem.join("agents");
        if confined(&agents_dir) {
            scan_md_dir(
                &agents_dir,
                project_scope,
                Some(&root_str),
                store,
                agents_md,
                |e| acc.agents.push(agent_from_md(e)),
                acc.errors,
            );
        }
        // commands: `<root>/.gemini/commands/**/*.toml`.
        let commands_dir = gem.join("commands");
        if confined(&commands_dir) {
            scan_gemini_commands(
                &commands_dir,
                project_scope,
                Some(&root_str),
                store,
                cmd_toml,
                acc,
            );
        }
        // hooks + mcp: `<root>/.gemini/settings.json`.
        let settings = gem.join("settings.json");
        if settings.is_file() && confined(&settings) {
            gemini_settings(
                &settings,
                project_scope,
                Some(&root_str),
                json_embedded,
                acc,
            );
        }
    }

    // Cross-system override resolution within Gemini (same-name project beats
    // personal) is handled by the shared `mark_overrides_*` pass in `scan_all`,
    // which keys on name only. v2a does NOT resolve overrides across systems —
    // a Gemini `planner` and a Claude `planner` stay distinct rows by design
    // (the `{system,kind,scope,name}` identity D-08 surfaces).
    let _ = scope_for_tier; // documented mapping; used by the reader inline.
}

/// Read hooks + mcpServers from a Gemini `settings.json` (both are keys inside
/// the same JSON file). Reuses the shared JSON parsers, stamping a Gemini tag.
fn gemini_settings(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
    tag: SystemTag,
    acc: &mut ScanAcc<'_>,
) {
    match parse_hooks_file_tagged(path, scope, project_root, tag) {
        Ok(mut h) => acc.hooks.append(&mut h),
        Err(e) => acc.errors.push(ScanError {
            path: path.to_string_lossy().to_string(),
            message: format!("gemini hooks parse: {e}"),
        }),
    }
    // `mcpServers` key inside the settings file — same shape as Claude's
    // settings-embedded MCP; reuse the JSON extractor with a Gemini tag.
    match read_settings_mcp_tagged(path, scope, project_root, tag) {
        Ok(mut m) => acc.mcps.append(&mut m),
        Err(e) => acc.errors.push(ScanError {
            path: path.to_string_lossy().to_string(),
            message: format!("gemini mcp(settings) parse: {e}"),
        }),
    }
}

/// Tagged variant of [`parse_mcp_from_settings`] — reads the top-level
/// `mcpServers` key from any JSON settings file and stamps the supplied tag.
fn read_settings_mcp_tagged(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
    tag: SystemTag,
) -> Result<Vec<McpEntry>> {
    let raw = std::fs::read_to_string(path).context("read settings file")?;
    let json: serde_json::Value = serde_json::from_str(&raw).context("parse settings json")?;
    let servers = match json.get("mcpServers").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };
    Ok(extract_mcp_servers(servers, path, scope, project_root, tag))
}

/// Walk a Gemini `commands/` tree (`**/*.toml`, subdir-namespaced). A command
/// in `commands/git/commit.toml` is named `git:commit`. Gemini command TOMLs
/// carry a `prompt` (the command body) and an optional `description`. The
/// `toml` crate is not a direct dependency here, and v2a is read-only, so we
/// extract just those two top-level string keys with a small line scanner —
/// enough to display the command in Ngwa without a full TOML parser.
fn scan_gemini_commands(
    dir: &Path,
    scope: Scope,
    project_root: Option<&str>,
    store: Option<&Path>,
    tag: SystemTag,
    acc: &mut ScanAcc<'_>,
) {
    if !dir.is_dir() {
        return;
    }
    walk_gemini_commands(dir, dir, scope, project_root, store, tag, acc);
}

fn walk_gemini_commands(
    base: &Path,
    dir: &Path,
    scope: Scope,
    project_root: Option<&str>,
    store: Option<&Path>,
    tag: SystemTag,
    acc: &mut ScanAcc<'_>,
) {
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            acc.errors.push(ScanError {
                path: dir.to_string_lossy().to_string(),
                message: format!("read_dir: {e}"),
            });
            return;
        }
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Recurse — namespace segments come from subdir names.
            walk_gemini_commands(base, &p, scope, project_root, store, tag, acc);
            continue;
        }
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        // Namespaced name: relative path from `base`, minus `.toml`, with
        // path separators → `:` (e.g. `git/commit.toml` → `git:commit`).
        let rel = p.strip_prefix(base).unwrap_or(&p);
        let name = rel
            .with_extension("")
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(":");
        if name.is_empty() || name.starts_with('_') {
            continue;
        }
        let raw = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                acc.errors.push(ScanError {
                    path: p.to_string_lossy().to_string(),
                    message: format!("read toml: {e}"),
                });
                continue;
            }
        };
        let (description, body) = parse_gemini_command_toml(&raw);
        // Gemini commands are standalone files (Mechanism::File) — surface
        // symlink/store metadata the same way agents/commands do.
        let meta = link_meta(&p, store);
        let mut frontmatter = serde_json::Map::new();
        if let Some(d) = &description {
            frontmatter.insert("description".into(), serde_json::Value::String(d.clone()));
        }
        acc.commands.push(CommandEntry {
            name,
            scope,
            project_root: project_root.map(|s| s.to_string()),
            path: p.to_string_lossy().to_string(),
            modified_ms: mtime_ms(&p),
            description,
            model: None,
            argument_hint: None,
            frontmatter: serde_json::Value::Object(frontmatter),
            body,
            overridden_by: None,
            is_symlink: meta.is_symlink,
            link_target: meta.link_target,
            in_store: meta.in_store,
            target_exists: meta.target_exists,
            tag,
        });
    }
}

/// Extract `description` + `prompt` (the body) from a Gemini command TOML.
/// Minimal: handles top-level `key = "..."` (single/multi-line basic strings)
/// and `key = '''...'''` / `"""..."""` triple-quoted blocks. Returns
/// `(description, prompt_body)`.
fn parse_gemini_command_toml(raw: &str) -> (Option<String>, String) {
    let mut description: Option<String> = None;
    let mut prompt = String::new();
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        for key in ["description", "prompt"] {
            let prefix = format!("{key}");
            if let Some(rest) = trimmed.strip_prefix(&prefix) {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let val_start = rest.trim_start();
                    // Triple-quoted block?
                    let triple = if val_start.starts_with("\"\"\"") {
                        Some("\"\"\"")
                    } else if val_start.starts_with("'''") {
                        Some("'''")
                    } else {
                        None
                    };
                    let value = if let Some(delim) = triple {
                        let mut acc = String::new();
                        let after = &val_start[delim.len()..];
                        if let Some(end) = after.find(delim) {
                            acc.push_str(&after[..end]);
                        } else {
                            acc.push_str(after);
                            // consume following lines until the closing delim
                            for l in lines.by_ref() {
                                if let Some(end) = l.find(delim) {
                                    if !acc.is_empty() {
                                        acc.push('\n');
                                    }
                                    acc.push_str(&l[..end]);
                                    break;
                                }
                                if !acc.is_empty() {
                                    acc.push('\n');
                                }
                                acc.push_str(l);
                            }
                        }
                        acc.trim().to_string()
                    } else {
                        unquote_toml_basic(val_start)
                    };
                    match key {
                        "description" => description = Some(value),
                        "prompt" => prompt = value,
                        _ => {}
                    }
                }
            }
        }
    }
    (description, prompt)
}

/// Strip a single-line TOML basic/literal string (`"..."` or `'...'`),
/// dropping an inline `# comment` tail when unquoted. Best-effort for display.
fn unquote_toml_basic(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.len() >= 2) || (s.starts_with('\'') && s.len() >= 2) {
        let q = s.as_bytes()[0] as char;
        if let Some(end) = s[1..].find(q) {
            return s[1..1 + end].to_string();
        }
    }
    // Unquoted scalar — strip a trailing comment.
    s.split('#').next().unwrap_or(s).trim().to_string()
}

// ─── Codex reader (WP-18) ────────────────────────────────────────────────────
//
// Reads `~/.codex` (user) + cross-tool `~/.agents` (user) + each
// `<root>/.codex` / `<root>/.agents` (project) per the frozen `EngineLayout`
// (G-ADAPTER) Codex cell. READ-ONLY — never writes. Missing files/dirs yield
// zero entries (NOT an error). Scope mapping reuses `scope_for_tier` exactly
// like the Gemini reader.
//
// Per-kind layout (frozen matrix / Round-9):
//   - skills:   cross-tool `~/.agents/skills/` (+ project `<root>/.agents/skills/`).
//               Codex NEVER uses `.codex/skills/`. format md-yaml.
//               (Reuses `scan_skills_dir`.)
//   - agents:   `~/.codex/agents/*.toml` — TOML. format toml.
//   - commands: `~/.codex/prompts/*.md` — DEPRECATED, format md-yaml.
//               (Reuses the md reader; tagged status=Deprecated.)
//   - hooks:    `~/.codex/hooks.json` (JSON, reuse `parse_hooks_file_tagged`)
//               AND inline `[hooks]` tables in `~/.codex/config.toml` (TOML).
//   - mcp:      `[mcp_servers.*]` tables in `~/.codex/config.toml` — TOML.
//
// The project-scope env-plumbing for `.agents/skills/` (`IKENGA_CODEX_PROJECT_ROOT`)
// is DEFERRED — we read the already-resolved project roots directly, identical
// to the Gemini reader.
fn scan_codex(
    layout: &EngineLayout,
    project_roots: &[String],
    store: Option<&Path>,
    acc: &mut ScanAcc<'_>,
) {
    let Some(home) = home_dir() else {
        return;
    };
    // Per-engine path confinement — every read must sit under one of Codex's
    // declared roots (`engine_roots` confines `.codex` + cross-tool `.agents`).
    let roots = engine_roots(layout, project_roots);
    let confined = |p: &Path| path_under_any(p, &roots);

    // Scope values derived from the frozen layout's tiers (not hardcoded).
    let user_scope = layout
        .scopes
        .iter()
        .find(|s| s.tier == ScopeTier::User)
        .map(|s| scope_for_tier(s.tier))
        .unwrap_or(Scope::Personal);
    let project_scope = layout
        .scopes
        .iter()
        .find(|s| s.tier == ScopeTier::Project)
        .map(|s| scope_for_tier(s.tier))
        .unwrap_or(Scope::Project);

    let skill_md = SystemTag {
        system: EngineId::Codex,
        format: ConfigFormat::MdYaml,
        status: KindStatus::Active,
    };
    let agent_toml = SystemTag {
        system: EngineId::Codex,
        format: ConfigFormat::Toml,
        status: KindStatus::Active,
    };
    // Codex commands live in `prompts/` and are DEPRECATED (superseded by
    // skills). format md-yaml, status deprecated.
    let prompt_md_deprecated = SystemTag {
        system: EngineId::Codex,
        format: ConfigFormat::MdYaml,
        status: KindStatus::Deprecated,
    };
    let hooks_json = SystemTag {
        system: EngineId::Codex,
        format: ConfigFormat::JsonEmbedded,
        status: KindStatus::Active,
    };
    let toml_tag = SystemTag {
        system: EngineId::Codex,
        format: ConfigFormat::Toml,
        status: KindStatus::Active,
    };

    // ── User scope (`~/.codex` + cross-tool `~/.agents`) ─────────────────
    let codex = home.join(".codex");

    // skills: cross-tool `~/.agents/skills/` ONLY (never `.codex/skills/`).
    let user_skills = home.join(".agents/skills");
    if confined(&user_skills) {
        scan_skills_dir(
            &user_skills,
            user_scope,
            None,
            store,
            skill_md,
            acc.skills,
            acc.errors,
        );
    }
    // agents: `~/.codex/agents/*.toml`.
    let agents_dir = codex.join("agents");
    if confined(&agents_dir) {
        scan_codex_agents(&agents_dir, user_scope, None, store, agent_toml, acc);
    }
    // commands: `~/.codex/prompts/*.md` (DEPRECATED).
    let prompts_dir = codex.join("prompts");
    if confined(&prompts_dir) {
        scan_md_dir(
            &prompts_dir,
            user_scope,
            None,
            store,
            prompt_md_deprecated,
            |e| acc.commands.push(command_from_md(e)),
            acc.errors,
        );
    }
    // hooks: `~/.codex/hooks.json` (JSON).
    let hooks_file = codex.join("hooks.json");
    if hooks_file.is_file() && confined(&hooks_file) {
        match parse_hooks_file_tagged(&hooks_file, user_scope, None, hooks_json) {
            Ok(mut h) => acc.hooks.append(&mut h),
            Err(e) => acc.errors.push(ScanError {
                path: hooks_file.to_string_lossy().to_string(),
                message: format!("codex hooks.json parse: {e}"),
            }),
        }
    }
    // hooks (inline `[hooks]`) + mcp (`[mcp_servers.*]`): `~/.codex/config.toml`.
    let config = codex.join("config.toml");
    if config.is_file() && confined(&config) {
        codex_config_toml(&config, user_scope, None, toml_tag, acc);
    }

    // ── Project scope (`<root>/.codex` + cross-tool `<root>/.agents`) ─────
    // Trusted-repo project roots are read directly (env-plumbing deferred).
    for raw in project_roots {
        let root = match expand(raw) {
            Ok(p) => p,
            Err(_) => continue, // Claude reader already surfaced expand errors.
        };
        let root_str = root.to_string_lossy().to_string();
        let cx = root.join(".codex");

        // skills: project `<root>/.agents/skills/`.
        let skills_dir = root.join(".agents/skills");
        if confined(&skills_dir) {
            scan_skills_dir(
                &skills_dir,
                project_scope,
                Some(&root_str),
                store,
                skill_md,
                acc.skills,
                acc.errors,
            );
        }
        // agents: `<root>/.codex/agents/*.toml`.
        let agents_dir = cx.join("agents");
        if confined(&agents_dir) {
            scan_codex_agents(
                &agents_dir,
                project_scope,
                Some(&root_str),
                store,
                agent_toml,
                acc,
            );
        }
        // commands: `<root>/.codex/prompts/*.md` (DEPRECATED).
        let prompts_dir = cx.join("prompts");
        if confined(&prompts_dir) {
            scan_md_dir(
                &prompts_dir,
                project_scope,
                Some(&root_str),
                store,
                prompt_md_deprecated,
                |e| acc.commands.push(command_from_md(e)),
                acc.errors,
            );
        }
        // hooks: `<root>/.codex/hooks.json`.
        let hooks_file = cx.join("hooks.json");
        if hooks_file.is_file() && confined(&hooks_file) {
            match parse_hooks_file_tagged(&hooks_file, project_scope, Some(&root_str), hooks_json) {
                Ok(mut h) => acc.hooks.append(&mut h),
                Err(e) => acc.errors.push(ScanError {
                    path: hooks_file.to_string_lossy().to_string(),
                    message: format!("codex hooks.json parse: {e}"),
                }),
            }
        }
        // hooks (inline) + mcp: `<root>/.codex/config.toml`.
        let config = cx.join("config.toml");
        if config.is_file() && confined(&config) {
            codex_config_toml(&config, project_scope, Some(&root_str), toml_tag, acc);
        }
    }
}

/// Scan a Codex agents dir (`*.toml`). Each file is a standalone TOML agent
/// (`Mechanism::File`). We parse `name` / `description` for display and treat
/// `body` / `system_prompt` (if present) as the rendered body. Surfaces
/// symlink/store metadata the same way the md agents do.
fn scan_codex_agents(
    dir: &Path,
    scope: Scope,
    project_root: Option<&str>,
    store: Option<&Path>,
    tag: SystemTag,
    acc: &mut ScanAcc<'_>,
) {
    if !dir.is_dir() {
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            acc.errors.push(ScanError {
                path: dir.to_string_lossy().to_string(),
                message: format!("read_dir: {e}"),
            });
            return;
        }
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        let file_name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if file_name.starts_with('_') {
            continue;
        }
        let raw = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                acc.errors.push(ScanError {
                    path: p.to_string_lossy().to_string(),
                    message: format!("read toml: {e}"),
                });
                continue;
            }
        };
        let value: toml::Value = match toml::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                acc.errors.push(ScanError {
                    path: p.to_string_lossy().to_string(),
                    message: format!("toml parse: {e}"),
                });
                continue;
            }
        };
        // Display name: explicit `name` key wins, else the file stem.
        let name = toml_str(&value, "name").unwrap_or(file_name);
        let description = toml_str(&value, "description");
        let model = toml_str(&value, "model");
        // Body: `system_prompt` (Codex's prompt field) or `body`, if present.
        let body = toml_str(&value, "system_prompt")
            .or_else(|| toml_str(&value, "body"))
            .unwrap_or_default();
        let frontmatter = toml_value_to_json(&value);
        let meta = link_meta(&p, store);
        acc.agents.push(AgentEntry {
            name,
            scope,
            project_root: project_root.map(|s| s.to_string()),
            path: p.to_string_lossy().to_string(),
            modified_ms: mtime_ms(&p),
            description,
            model,
            frontmatter,
            body,
            overridden_by: None,
            is_symlink: meta.is_symlink,
            link_target: meta.link_target,
            in_store: meta.in_store,
            target_exists: meta.target_exists,
            tag,
        });
    }
}

/// Read inline `[hooks]` + `[mcp_servers.*]` tables from a Codex `config.toml`.
/// Both live in the same file (TOML); hooks are tagged `format = toml` (this is
/// an inline-TOML hook source, distinct from the standalone `hooks.json`),
/// mcp servers `format = toml`.
fn codex_config_toml(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
    tag: SystemTag,
    acc: &mut ScanAcc<'_>,
) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            acc.errors.push(ScanError {
                path: path.to_string_lossy().to_string(),
                message: format!("read config.toml: {e}"),
            });
            return;
        }
    };
    let value: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            acc.errors.push(ScanError {
                path: path.to_string_lossy().to_string(),
                message: format!("config.toml parse: {e}"),
            });
            return;
        }
    };

    // ── mcp servers: `[mcp_servers.<id>]` ────────────────────────────────
    if let Some(servers) = value.get("mcp_servers").and_then(|v| v.as_table()) {
        let path_str = path.to_string_lossy().to_string();
        for (name, server) in servers {
            let command = toml_str(server, "command");
            let url = toml_str(server, "url");
            // Codex stdio servers carry a `command`; the `type` key is rare.
            let transport = if let Some(t) = toml_str(server, "type") {
                t
            } else if url.is_some() {
                "http".to_string()
            } else if command.is_some() {
                "stdio".to_string()
            } else {
                "unknown".to_string()
            };
            let args = server
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            // Codex `env` literal table + `env_vars` forwarded-key list — both
            // declare env-var *keys* (values stripped, same as the JSON path).
            let mut env_keys: Vec<String> = server
                .get("env")
                .and_then(|v| v.as_table())
                .map(|t| t.keys().cloned().collect())
                .unwrap_or_default();
            if let Some(forwarded) = server.get("env_vars").and_then(|v| v.as_array()) {
                for k in forwarded.iter().filter_map(|x| x.as_str()) {
                    if !env_keys.iter().any(|e| e == k) {
                        env_keys.push(k.to_string());
                    }
                }
            }
            acc.mcps.push(McpEntry {
                name: name.clone(),
                scope,
                project_root: project_root.map(|s| s.to_string()),
                path: path_str.clone(),
                transport,
                command,
                args,
                env_keys,
                url,
                header_keys: Vec::new(),
                raw: toml_value_to_json(server),
                is_symlink: false,
                link_target: None,
                in_store: false,
                target_exists: true,
                tag,
            });
        }
    }

    // ── hooks: inline `[hooks]` table ────────────────────────────────────
    // Codex's inline hooks are tagged `format = toml` to distinguish them from
    // the standalone `hooks.json` source. Shape mirrors the JSON hooks:
    // `[hooks]` is an event→entries map; each entry carries a `type` + a
    // `command` (or an inline matcher). We surface one HookEntry per hook,
    // walking whatever nested array/table shape the event value holds.
    if let Some(hooks) = value.get("hooks").and_then(|v| v.as_table()) {
        let settings_path = path.to_string_lossy().to_string();
        for (event, value) in hooks {
            // An event maps to either an array of hook entries or a single
            // inline table — normalize to a slice of entries.
            let entries: Vec<&toml::Value> = match value {
                toml::Value::Array(a) => a.iter().collect(),
                toml::Value::Table(_) => vec![value],
                _ => continue,
            };
            for h in entries {
                // Mirror the JSON hooks grouping: an entry may itself nest a
                // `hooks = [...]` array (matcher-group shape) or be a flat
                // `{ type, command }` table.
                let inner: Vec<&toml::Value> = match h.get("hooks").and_then(|v| v.as_array()) {
                    Some(a) => a.iter().collect(),
                    None => vec![h],
                };
                for entry in inner {
                    let kind = toml_str(entry, "type").unwrap_or_else(|| "command".to_string());
                    let command_raw = toml_str(entry, "command");
                    let command_path = command_raw.as_ref().and_then(|c| {
                        resolve_command_path(c, project_root)
                            .map(|p| p.to_string_lossy().to_string())
                    });
                    let json_raw = toml_value_to_json(entry);
                    let name = derive_hook_name(&command_raw, &json_raw);
                    acc.hooks.push(HookEntry {
                        event: event.clone(),
                        kind,
                        name,
                        scope,
                        project_root: project_root.map(|s| s.to_string()),
                        settings_path: settings_path.clone(),
                        command_path,
                        command_raw,
                        raw: json_raw,
                        is_symlink: false,
                        link_target: None,
                        in_store: false,
                        target_exists: true,
                        tag,
                    });
                }
            }
        }
    }
}

/// Read a top-level (or table-relative) string key from a `toml::Value`.
fn toml_str(v: &toml::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

/// Convert a `toml::Value` into a `serde_json::Value` for the detail-view grid
/// and raw previews (mirrors what the md frontmatter / JSON readers store).
fn toml_value_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in t {
                obj.insert(k.clone(), toml_value_to_json(val));
            }
            serde_json::Value::Object(obj)
        }
    }
}

/// Internal struct passed to the closures so we don't re-stat / re-parse
/// inside the agent/command builders.
struct MdEntry {
    name: String,
    scope: Scope,
    project_root: Option<String>,
    path: PathBuf,
    modified_ms: i64,
    frontmatter: serde_json::Value,
    body: String,
    link_meta: LinkMeta,
    tag: SystemTag,
}

fn scan_md_dir<F: FnMut(MdEntry)>(
    dir: &Path,
    scope: Scope,
    project_root: Option<&str>,
    store: Option<&Path>,
    tag: SystemTag,
    mut push: F,
    errors: &mut Vec<ScanError>,
) {
    if !dir.is_dir() {
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            errors.push(ScanError {
                path: dir.to_string_lossy().to_string(),
                message: format!("read_dir: {e}"),
            });
            return;
        }
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Only `.md`; skip `_archived-*` rendered hidden/dot-files.
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        if name.starts_with('_') {
            continue;
        }
        match parse_md(&p) {
            Ok((fm, body)) => push(MdEntry {
                name,
                scope,
                project_root: project_root.map(|s| s.to_string()),
                modified_ms: mtime_ms(&p),
                link_meta: link_meta(&p, store),
                path: p.clone(),
                frontmatter: fm,
                body,
                tag,
            }),
            Err(e) => errors.push(ScanError {
                path: p.to_string_lossy().to_string(),
                message: format!("parse: {e}"),
            }),
        }
    }
}

fn scan_skills_dir(
    dir: &Path,
    scope: Scope,
    project_root: Option<&str>,
    store: Option<&Path>,
    tag: SystemTag,
    skills: &mut Vec<SkillEntry>,
    errors: &mut Vec<ScanError>,
) {
    if !dir.is_dir() {
        return;
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) => {
            errors.push(ScanError {
                path: dir.to_string_lossy().to_string(),
                message: format!("read_dir: {e}"),
            });
            return;
        }
    };
    for entry in read_dir.flatten() {
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
        match parse_md(&skill_md) {
            Ok((fm, body)) => {
                let description = string_field(&fm, "description");
                let supporting_files = list_supporting_files(&skill_dir);
                // Skills are symlinked dir-wise: inspect the skill dir, not
                // SKILL.md (the file inside a symlinked dir is not itself a
                // link).
                let meta = link_meta(&skill_dir, store);
                skills.push(SkillEntry {
                    name,
                    scope,
                    project_root: project_root.map(|s| s.to_string()),
                    path: skill_md.to_string_lossy().to_string(),
                    dir_path: skill_dir.to_string_lossy().to_string(),
                    modified_ms: mtime_ms(&skill_md),
                    description,
                    frontmatter: fm,
                    body,
                    supporting_files,
                    overridden_by: None,
                    is_symlink: meta.is_symlink,
                    link_target: meta.link_target,
                    in_store: meta.in_store,
                    target_exists: meta.target_exists,
                    tag,
                });
            }
            Err(e) => errors.push(ScanError {
                path: skill_md.to_string_lossy().to_string(),
                message: format!("parse: {e}"),
            }),
        }
    }
}

fn list_supporting_files(skill_dir: &Path) -> Vec<SupportingFile> {
    let mut out = Vec::new();
    let read_dir = match std::fs::read_dir(skill_dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for entry in read_dir.flatten() {
        let p = entry.path();
        let name = match p.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if name == "SKILL.md" || name.starts_with('.') {
            continue;
        }
        if p.is_file() {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            out.push(SupportingFile {
                name,
                path: p.to_string_lossy().to_string(),
                size,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn agent_from_md(e: MdEntry) -> AgentEntry {
    AgentEntry {
        name: e.name,
        scope: e.scope,
        project_root: e.project_root,
        path: e.path.to_string_lossy().to_string(),
        modified_ms: e.modified_ms,
        description: string_field(&e.frontmatter, "description"),
        model: string_field(&e.frontmatter, "model"),
        frontmatter: e.frontmatter,
        body: e.body,
        overridden_by: None,
        is_symlink: e.link_meta.is_symlink,
        link_target: e.link_meta.link_target,
        in_store: e.link_meta.in_store,
        target_exists: e.link_meta.target_exists,
        tag: e.tag,
    }
}

fn command_from_md(e: MdEntry) -> CommandEntry {
    CommandEntry {
        name: e.name,
        scope: e.scope,
        project_root: e.project_root,
        path: e.path.to_string_lossy().to_string(),
        modified_ms: e.modified_ms,
        description: string_field(&e.frontmatter, "description"),
        model: string_field(&e.frontmatter, "model"),
        argument_hint: string_field(&e.frontmatter, "argument-hint"),
        frontmatter: e.frontmatter,
        body: e.body,
        overridden_by: None,
        is_symlink: e.link_meta.is_symlink,
        link_target: e.link_meta.link_target,
        in_store: e.link_meta.in_store,
        target_exists: e.link_meta.target_exists,
        tag: e.tag,
    }
}

// ─── Hook parsing ───────────────────────────────────────────────────────────

pub(crate) fn parse_hooks_file(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
) -> Result<Vec<HookEntry>> {
    // Claude wrapper — preserves the `pub(crate)` signature `discovery.rs`
    // depends on. Claude hooks carry the Claude tag (which serializes to
    // nothing, keeping the shipping wire byte-identical).
    parse_hooks_file_tagged(
        path,
        scope,
        project_root,
        SystemTag::claude(ConfigFormat::JsonEmbedded),
    )
}

/// Engine-parametric core of [`parse_hooks_file`]. The `hooks` key inside a
/// JSON settings file is laid out identically across Claude & Gemini
/// (`{ Event: [ { matcher?, hooks: [ {type, command, ...} ] } ] }`), so the
/// only difference is the `SystemTag` stamped on each emitted entry.
fn parse_hooks_file_tagged(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
    tag: SystemTag,
) -> Result<Vec<HookEntry>> {
    let raw = std::fs::read_to_string(path).context("read settings file")?;
    let json: serde_json::Value = serde_json::from_str(&raw).context("parse settings json")?;
    let hooks_obj = match json.get("hooks") {
        Some(serde_json::Value::Object(m)) => m,
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    let settings_path_str = path.to_string_lossy().to_string();
    for (event, value) in hooks_obj {
        // Settings format: hooks: { Event: [ { matcher?, hooks: [ {type, command, ...} ] } ] }
        let groups = match value.as_array() {
            Some(g) => g,
            None => continue,
        };
        for group in groups {
            let inner = match group.get("hooks").and_then(|v| v.as_array()) {
                Some(a) => a,
                None => continue,
            };
            for h in inner {
                let kind = h
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("command")
                    .to_string();
                let command_raw = h
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let command_path = command_raw.as_ref().and_then(|c| {
                    let resolved = resolve_command_path(c, project_root);
                    resolved.map(|p| p.to_string_lossy().to_string())
                });
                let name = derive_hook_name(&command_raw, h);
                out.push(HookEntry {
                    event: event.clone(),
                    kind,
                    name,
                    scope,
                    project_root: project_root.map(|s| s.to_string()),
                    settings_path: settings_path_str.clone(),
                    command_path,
                    command_raw,
                    raw: h.clone(),
                    // Hooks are JSON entries in settings, not symlinkable files.
                    is_symlink: false,
                    link_target: None,
                    in_store: false,
                    target_exists: true,
                    tag,
                });
            }
        }
    }
    Ok(out)
}

fn derive_hook_name(command_raw: &Option<String>, raw: &serde_json::Value) -> String {
    if let Some(c) = command_raw {
        // Take the first whitespace-separated token, basename.
        let first = c.split_whitespace().next().unwrap_or(c);
        if let Some(name) = std::path::Path::new(first)
            .file_name()
            .and_then(|s| s.to_str())
        {
            return name.to_string();
        }
        return first.to_string();
    }
    raw.get("matcher")
        .and_then(|v| v.as_str())
        .unwrap_or("hook")
        .to_string()
}

fn resolve_command_path(raw: &str, project_root: Option<&str>) -> Option<PathBuf> {
    let first = raw.split_whitespace().next().unwrap_or(raw);
    if first.is_empty() {
        return None;
    }
    if let Ok(p) = expand(first) {
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
        if let Some(root) = project_root {
            if let Ok(rp) = expand(root) {
                let candidate = rp.join(first);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

// ─── MCP parsing ────────────────────────────────────────────────────────────

/// Parse a standalone `mcp.json` file (top level is `{ mcpServers: { ... } }`).
pub(crate) fn parse_mcp_file(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
) -> Result<Vec<McpEntry>> {
    let raw = std::fs::read_to_string(path).context("read mcp file")?;
    let json: serde_json::Value = serde_json::from_str(&raw).context("parse mcp json")?;
    let servers = json.get("mcpServers").or_else(|| json.get("servers"));
    Ok(servers
        .and_then(|v| v.as_object())
        .map(|m| {
            extract_mcp_servers(
                m,
                path,
                scope,
                project_root,
                SystemTag::claude(ConfigFormat::JsonEmbedded),
            )
        })
        .unwrap_or_default())
}

/// Parse `mcpServers` if it appears inside an arbitrary settings JSON file
/// (settings.local.json, settings.json, ~/.claude.json — anywhere it shows up
/// at the top level).
pub(crate) fn parse_mcp_from_settings(
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
) -> Result<Vec<McpEntry>> {
    let raw = std::fs::read_to_string(path).context("read settings file")?;
    let json: serde_json::Value = serde_json::from_str(&raw).context("parse settings json")?;
    let servers = match json.get("mcpServers").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Ok(Vec::new()),
    };
    Ok(extract_mcp_servers(
        servers,
        path,
        scope,
        project_root,
        SystemTag::claude(ConfigFormat::JsonEmbedded),
    ))
}

fn extract_mcp_servers(
    servers: &serde_json::Map<String, serde_json::Value>,
    path: &Path,
    scope: Scope,
    project_root: Option<&str>,
    tag: SystemTag,
) -> Vec<McpEntry> {
    let path_str = path.to_string_lossy().to_string();
    let mut out = Vec::with_capacity(servers.len());
    for (name, raw) in servers {
        let kind = raw.get("type").and_then(|v| v.as_str()).map(str::to_string);
        let url = raw.get("url").and_then(|v| v.as_str()).map(str::to_string);
        let command = raw
            .get("command")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let transport = if let Some(t) = kind {
            t
        } else if url.is_some() {
            "http".to_string()
        } else if command.is_some() {
            "stdio".to_string()
        } else {
            "unknown".to_string()
        };
        let args = raw
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let env_keys = raw
            .get("env")
            .and_then(|v| v.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let header_keys = raw
            .get("headers")
            .and_then(|v| v.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        out.push(McpEntry {
            name: name.clone(),
            scope,
            project_root: project_root.map(|s| s.to_string()),
            path: path_str.clone(),
            transport,
            command,
            args,
            env_keys,
            url,
            header_keys,
            raw: raw.clone(),
            // MCP servers are JSON keys, not symlinkable files.
            is_symlink: false,
            link_target: None,
            in_store: false,
            target_exists: true,
            tag,
        });
    }
    out
}

// ─── Markdown / frontmatter parsing ─────────────────────────────────────────

pub(crate) fn parse_md(path: &Path) -> Result<(serde_json::Value, String)> {
    let raw = std::fs::read_to_string(path).context("read md")?;
    Ok(split_frontmatter(&raw))
}

/// Split a markdown file at the leading `---` frontmatter block. Returns
/// (frontmatter as JSON object, body string). If no frontmatter is present,
/// returns an empty object and the full file as body.
pub fn split_frontmatter(raw: &str) -> (serde_json::Value, String) {
    // Treat `---\n` or `---\r\n` as the opener; require it at the very start.
    let trimmed = raw.trim_start_matches('\u{FEFF}');
    if !trimmed.starts_with("---") {
        return (
            serde_json::Value::Object(Default::default()),
            raw.to_string(),
        );
    }
    // Find the line break after the opening fence.
    let after_open = match trimmed.find('\n') {
        Some(i) => &trimmed[i + 1..],
        None => {
            return (
                serde_json::Value::Object(Default::default()),
                raw.to_string(),
            )
        }
    };
    // Find the closing `---` on its own line.
    let mut close_idx: Option<usize> = None;
    let bytes = after_open.as_bytes();
    let mut line_start = 0usize;
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' || i == bytes.len() - 1 {
            let end = if bytes[i] == b'\n' { i } else { i + 1 };
            let line = &after_open[line_start..end].trim_end_matches('\r');
            if *line == "---" {
                close_idx = Some(line_start);
                break;
            }
            line_start = i + 1;
        }
    }
    let close = match close_idx {
        Some(c) => c,
        None => {
            return (
                serde_json::Value::Object(Default::default()),
                raw.to_string(),
            )
        }
    };
    let yaml = &after_open[..close];
    // Body starts after the closing `---` line.
    let after_close = &after_open[close..];
    let body = match after_close.find('\n') {
        Some(i) => after_close[i + 1..].to_string(),
        None => String::new(),
    };
    let fm_json = match serde_yaml::from_str::<serde_yaml::Value>(yaml) {
        Ok(v) => yaml_to_json(v),
        Err(_) => serde_json::Value::Object(Default::default()),
    };
    let fm_json = match fm_json {
        serde_json::Value::Object(_) => fm_json,
        _ => serde_json::Value::Object(Default::default()),
    };
    (fm_json, body)
}

fn yaml_to_json(v: serde_yaml::Value) -> serde_json::Value {
    match v {
        serde_yaml::Value::Null => serde_json::Value::Null,
        serde_yaml::Value::Bool(b) => serde_json::Value::Bool(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(serde_json::Number::from(i))
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            } else {
                serde_json::Value::Null
            }
        }
        serde_yaml::Value::String(s) => serde_json::Value::String(s),
        serde_yaml::Value::Sequence(seq) => {
            serde_json::Value::Array(seq.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                let key = match k {
                    serde_yaml::Value::String(s) => s,
                    other => serde_yaml::to_string(&other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                obj.insert(key, yaml_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        serde_yaml::Value::Tagged(t) => yaml_to_json(t.value),
    }
}

pub(crate) fn string_field(fm: &serde_json::Value, key: &str) -> Option<String> {
    fm.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

// ─── Override resolution ────────────────────────────────────────────────────

fn mark_overrides_agents(items: &mut [AgentEntry]) {
    let project_paths: std::collections::HashMap<String, String> = items
        .iter()
        .filter(|i| i.scope == Scope::Project)
        .map(|i| (i.name.clone(), i.path.clone()))
        .collect();
    for it in items.iter_mut() {
        if it.scope == Scope::Personal {
            if let Some(p) = project_paths.get(&it.name) {
                it.overridden_by = Some(p.clone());
            }
        }
    }
}
fn mark_overrides_skills(items: &mut [SkillEntry]) {
    let project_paths: std::collections::HashMap<String, String> = items
        .iter()
        .filter(|i| i.scope == Scope::Project)
        .map(|i| (i.name.clone(), i.path.clone()))
        .collect();
    for it in items.iter_mut() {
        if it.scope == Scope::Personal {
            if let Some(p) = project_paths.get(&it.name) {
                it.overridden_by = Some(p.clone());
            }
        }
    }
}
fn mark_overrides_commands(items: &mut [CommandEntry]) {
    let project_paths: std::collections::HashMap<String, String> = items
        .iter()
        .filter(|i| i.scope == Scope::Project)
        .map(|i| (i.name.clone(), i.path.clone()))
        .collect();
    for it in items.iter_mut() {
        if it.scope == Scope::Personal {
            if let Some(p) = project_paths.get(&it.name) {
                it.overridden_by = Some(p.clone());
            }
        }
    }
}

// ─── Path helpers ───────────────────────────────────────────────────────────

pub(crate) fn expand(input: &str) -> Result<PathBuf> {
    let s = shellexpand::full(input)
        .map(|c| c.into_owned())
        .map_err(|e| anyhow!("shellexpand: {e}"))?;
    Ok(PathBuf::from(s))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub(crate) fn mtime_ms(p: &Path) -> i64 {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn is_under_claude_dir(p: &Path) -> bool {
    // Walk components: must contain a `.claude` segment somewhere.
    p.components().any(|c| match c {
        std::path::Component::Normal(s) => s == std::ffi::OsStr::new(".claude"),
        _ => false,
    })
}

// ─── Per-engine read confinement (WP-17) ─────────────────────────────────────
//
// The shipping `is_under_claude_dir` is the Claude-specific case of a more
// general invariant: a read path must sit under one of the *active engine's*
// declared roots. WP-17 generalizes it so the Gemini reader (and WP-18's Codex
// reader) can only ever read inside the dirs the frozen `EngineLayout` declares
// for that engine — a path computed outside those roots is rejected/skipped.
// Claude's behavior is preserved exactly: its roots resolve to `~/.claude` +
// each `<root>/.claude`, which `is_under_claude_dir` already covers.

/// Resolve the concrete root dirs an engine may read under, from its frozen
/// `EngineLayout` scopes' `root_source` templates plus the supplied project
/// roots. User-tier roots resolve against `$HOME` (`~/.claude` → `<home>/
/// .claude`, `~/.gemini` → `<home>/.gemini`, …); project-tier roots resolve to
/// `<project_root>/<engine-dotdir>` for every supplied project root. The
/// cross-tool `.agents/` skills path is added for any engine that declares a
/// `.agents/skills/` skill location (Gemini, Codex) so that shared-skills reads
/// stay confined.
fn engine_roots(layout: &EngineLayout, project_roots: &[String]) -> Vec<PathBuf> {
    let home = home_dir();
    let mut roots: Vec<PathBuf> = Vec::new();

    // The engine's dotdir basename, derived from its user scope root_source
    // (e.g. `~/.gemini` → `.gemini`). Used to build project-scope roots.
    let dotdir: Option<String> = layout
        .scopes
        .iter()
        .find(|s| s.tier == ScopeTier::User)
        .and_then(|s| Path::new(s.root_source).file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string());

    for sc in &layout.scopes {
        match sc.tier {
            ScopeTier::User => {
                // `~/<dotdir>` → `<home>/<dotdir>`.
                if let (Some(home), Some(name)) = (&home, Path::new(sc.root_source).file_name()) {
                    roots.push(home.join(name));
                }
            }
            ScopeTier::Project => {
                if let Some(dd) = &dotdir {
                    for raw in project_roots {
                        if let Ok(root) = expand(raw) {
                            roots.push(root.join(dd));
                        }
                    }
                }
            }
        }
    }

    // Cross-tool `.agents/` skills path (Gemini + Codex): user `~/.agents` and
    // project `<root>/.agents`. Only added when the engine declares a skill
    // location under `.agents/`.
    let uses_agents_skills = layout
        .kinds
        .get(&PrimitiveKind::Skill)
        .map(|k| k.location.contains("/.agents/"))
        .unwrap_or(false);
    if uses_agents_skills {
        if let Some(home) = &home {
            roots.push(home.join(".agents"));
        }
        for raw in project_roots {
            if let Ok(root) = expand(raw) {
                roots.push(root.join(".agents"));
            }
        }
    }

    roots
}

/// True iff `p` sits under (or equals) one of `roots`. Lexical check after
/// `expand`; both sides are compared on their `components()` so a `..`-free
/// computed scan path is reliably contained. Used to reject any read the
/// per-engine reader might compute outside its declared roots.
fn path_under_any(p: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| p == r || p.starts_with(r))
}

/// Path-confinement guard for the store + symlink-farm mutations (WP-02). A
/// mutation target is permitted iff it sits under some `.claude/` dir OR under
/// the Ngwa central store root. This widens `is_under_claude_dir` (which only
/// permits `.claude/`) so store-side writes (`claude_store_import` copying a
/// canonical primitive into `<app_data_dir>/store/`) pass the same check that
/// scope-side symlink creates do. Read-only callers keep using
/// `is_under_claude_dir` directly so their semantics are unchanged.
pub(crate) fn is_under_claude_or_store(p: &Path) -> bool {
    if is_under_claude_dir(p) {
        return true;
    }
    match store_root() {
        Some(store) => is_in_store(p, &store),
        None => false,
    }
}

// ─── Symlink / central-store metadata ───────────────────────────────────────

/// Resolve the Ngwa central store root (`<app_data_dir>/store/`) using the same
/// platform conventions Tauri uses for `app_data_dir`. Mirrors the resolver in
/// `vault_key.rs` so it works without an `AppHandle` (the scanner runs on a
/// blocking thread with no handle). Returns `None` if the platform's data dir
/// can't be resolved.
///
/// WP-02 owns the canonical layout *under* this root (e.g.
/// `store/{agents,skills,commands}/`); WP-01 only needs the root to decide
/// `in_store`.
pub(crate) fn store_root() -> Option<PathBuf> {
    const BUNDLE_ID: &str = "app.ikenga";
    let dir: PathBuf = if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(BUNDLE_ID)
    } else if cfg!(target_os = "windows") {
        let appdata = std::env::var_os("APPDATA")?;
        PathBuf::from(appdata).join(BUNDLE_ID)
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(xdg).join(BUNDLE_ID)
    } else {
        let home = std::env::var_os("HOME")?;
        PathBuf::from(home).join(".local/share").join(BUNDLE_ID)
    };
    Some(dir.join("store"))
}

/// True when `target` is the store root itself or sits underneath it.
/// Both sides are normalized via `canonicalize` when possible so symlinked or
/// `..`-laden paths still compare correctly; falls back to a lexical
/// `starts_with` when canonicalization fails (e.g. the store dir doesn't exist
/// yet on a fresh install).
pub(crate) fn is_in_store(target: &Path, store: &Path) -> bool {
    let canon_target = target.canonicalize();
    let canon_store = store.canonicalize();
    match (canon_target, canon_store) {
        (Ok(t), Ok(s)) => t.starts_with(&s),
        _ => target.starts_with(store),
    }
}

/// Compute symlink + store metadata for a file-based primitive path (an agent
/// or command `.md`, or a skill dir). Uses `symlink_metadata` (lstat) so the
/// link itself is inspected, not its target. `store` is the memoized store
/// root for this scan (computed once in `scan_all`).
fn link_meta(path: &Path, store: Option<&Path>) -> LinkMeta {
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);

    // The path used for the `in_store` check: the resolved target when the
    // primitive is a symlink, else the path itself (a primitive can live
    // directly inside the store without being a link — e.g. a store-owned
    // entry surfaced in its own catalog scope).
    // `target_exists`: a symlink that canonicalizes is resolving (links to a
    // real master → *linked*); one that fails is dangling (→ orphaned). A
    // non-symlink path we are scanning exists by definition.
    let (link_target, resolved, target_exists) = if is_symlink {
        match std::fs::canonicalize(path) {
            Ok(t) => (Some(t.to_string_lossy().to_string()), t, true),
            // Dangling symlink: report the raw read_link target, treat the
            // link's own location for the store check, flag the missing target.
            Err(_) => {
                let raw = std::fs::read_link(path)
                    .ok()
                    .map(|t| t.to_string_lossy().to_string());
                (raw, path.to_path_buf(), false)
            }
        }
    } else {
        (None, path.to_path_buf(), true)
    };

    let in_store = store.map(|s| is_in_store(&resolved, s)).unwrap_or(false);

    LinkMeta {
        is_symlink,
        link_target,
        in_store,
        target_exists,
    }
}

/// WP-03: resolve a path to its canonical target if (and only if) it exists,
/// following symlinks. Returns `None` for a dangling link or a missing path.
/// The WP-04 dependents scan uses this to test whether a scope symlink resolves
/// *into* a given master directory.
pub(crate) fn resolve_symlink_target(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok()
}

/// WP-04 — the incident guardrail's core: find a master's **live dependents**,
/// computed fresh from the filesystem (never a stored list, which can drift).
/// A dependent is a symlink directly inside one of `search_dirs` whose resolved
/// target IS the master or sits inside it. The master is canonicalized once;
/// only links that actually resolve count (a dangling link is not a dependent).
///
/// `search_dirs` are the scope×engine placement directories the caller wants to
/// check (e.g. every `.claude/skills`, `~/.agents/skills`, `~/.codex/...` across
/// workspace + projects). Keeping the dir set a parameter makes this pure and
/// testable against tempdir HOMEs.
pub(crate) fn scan_live_dependents(master: &Path, search_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(master_canon) = std::fs::canonicalize(master) else {
        // master itself missing → nothing resolves into it
        return Vec::new();
    };
    let mut out = Vec::new();
    for dir in search_dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
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
            if let Some(t) = resolve_symlink_target(&p) {
                if t == master_canon || t.starts_with(&master_canon) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// ─── Phase 4 — 4-tier discovery + pin CRUD (new surface) ────────────────────
//
// These commands sit alongside the legacy `claude_config_*` ones. The FE
// migrates incrementally — old `/claude` route keeps the legacy ones; new
// "Claude Config Browser" UI consumes the layered tree.

use crate::claude::discovery::{self, AssetPin, AssetTree};
use crate::commands::db::PaDb;
use crate::commands::projects::get_active_project_id;

pub(crate) fn validate_pin_scope(scope: &str) -> Result<(), String> {
    if scope == "workspace" {
        return Ok(());
    }
    if let Some(id) = scope.strip_prefix("project:") {
        if id.is_empty() || id.len() > 64 {
            return Err(format!("invalid project id length in scope {scope:?}"));
        }
        let mut chars = id.chars();
        let first = chars.next().unwrap();
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(format!("invalid project id in scope {scope:?}"));
        }
        for c in chars {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
                return Err(format!("invalid project id in scope {scope:?}"));
            }
        }
        return Ok(());
    }
    Err(format!(
        "scope must be 'workspace' or 'project:<id>', got {scope:?}"
    ))
}

fn validate_pin_tier(tier: &str) -> Result<(), String> {
    match tier {
        "personal" | "workspace_pkg" | "project" | "project_pkg" => Ok(()),
        _ => Err(format!(
            "preferred_tier must be one of personal|workspace_pkg|project|project_pkg, got {tier:?}"
        )),
    }
}

fn validate_pin_kind(kind: &str) -> Result<(), String> {
    match kind {
        "skill" | "agent" | "command" | "hook" | "mcp" => Ok(()),
        _ => Err(format!(
            "asset_kind must be one of skill|agent|command|hook|mcp, got {kind:?}"
        )),
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Run the 4-tier layered discovery.
///
/// `projectId` defaults to the currently-active project. The returned
/// `AssetTree` lists *all* sources for each asset name; consumers apply
/// `resolve_preferred` (or the equivalent FE helper) to pick the active one.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_assets_discover(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    projectId: Option<String>,
) -> Result<AssetTree, String> {
    let pool = db.ensure_pool().await?;
    let active = match projectId {
        Some(id) => id,
        None => get_active_project_id(&pool).await?,
    };
    discovery::discover(&active, &pool, &app).await
}

/// Insert / update a pin row. `preferredSource` is nullable for the personal
/// tier (there's only one personal source); for pkg tiers callers should pass
/// the pkg id so the pin can disambiguate between multiple pkgs declaring the
/// same asset name.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_pin(
    db: State<'_, Arc<PaDb>>,
    scope: String,
    assetKind: String,
    assetName: String,
    preferredTier: String,
    preferredSource: Option<String>,
) -> Result<(), String> {
    validate_pin_scope(&scope)?;
    validate_pin_kind(&assetKind)?;
    validate_pin_tier(&preferredTier)?;
    if assetName.is_empty() || assetName.len() > 256 {
        return Err(format!(
            "invalid asset_name length: {} (1..=256)",
            assetName.len()
        ));
    }
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "INSERT INTO claude_asset_preferences
            (scope, asset_kind, asset_name, preferred_tier, preferred_source, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(scope, asset_kind, asset_name) DO UPDATE SET
            preferred_tier   = excluded.preferred_tier,
            preferred_source = excluded.preferred_source,
            updated_at       = excluded.updated_at",
    )
    .bind(&scope)
    .bind(&assetKind)
    .bind(&assetName)
    .bind(&preferredTier)
    .bind(&preferredSource)
    .bind(now_ms())
    .execute(&pool)
    .await
    .map_err(|e| format!("upsert pin: {e}"))?;
    Ok(())
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_unpin(
    db: State<'_, Arc<PaDb>>,
    scope: String,
    assetKind: String,
    assetName: String,
) -> Result<(), String> {
    validate_pin_scope(&scope)?;
    validate_pin_kind(&assetKind)?;
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "DELETE FROM claude_asset_preferences
         WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
    )
    .bind(&scope)
    .bind(&assetKind)
    .bind(&assetName)
    .execute(&pool)
    .await
    .map_err(|e| format!("delete pin: {e}"))?;
    Ok(())
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_list_pins(
    db: State<'_, Arc<PaDb>>,
    scope: String,
) -> Result<Vec<AssetPin>, String> {
    validate_pin_scope(&scope)?;
    let pool = db.ensure_pool().await?;
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT scope, asset_kind, asset_name, preferred_tier, preferred_source, updated_at
         FROM claude_asset_preferences
         WHERE scope = ?
         ORDER BY asset_kind, asset_name",
    )
    .bind(&scope)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("list pins: {e}"))?;
    let out = rows
        .into_iter()
        .map(|r| AssetPin {
            scope: r.get("scope"),
            asset_kind: r.get("asset_kind"),
            asset_name: r.get("asset_name"),
            preferred_tier: r.get("preferred_tier"),
            preferred_source: r.get("preferred_source"),
            updated_at: r.get("updated_at"),
        })
        .collect();
    Ok(out)
}

// ─── Pin coexistence with the store/symlink layer (WP-04) ───────────────────
//
// The store mutations (`claude_store.rs`: disable / move / remove) make a
// primitive no longer resolvable at a given scope. A `claude_asset_preferences`
// pin row keys to a primitive by exactly `(scope, asset_kind, asset_name)` — the
// same triple those mutations receive — so when a mutation strands a primitive
// these helpers re-point or clear the matching pin so no dangling pin survives.
//
// These are pool-level helpers (no `State`/command wrapper) so the store layer,
// which has already resolved its pool, can call them inline. They are the only
// *write* surface this file exposes over the pin table beyond the existing
// `claude_asset_pin` / `_unpin` commands; the read-only config-scanning commands
// are untouched.

/// Clear the pin for `(scope, kind, name)` if one exists. Idempotent — a missing
/// row is a no-op. Called when `disable` / `remove` makes the primitive
/// unresolvable at `scope` (the pin would otherwise point at a gone primitive).
pub(crate) async fn clear_pin_for(
    pool: &sqlx::SqlitePool,
    scope: &str,
    kind: &str,
    name: &str,
) -> Result<(), String> {
    sqlx::query(
        "DELETE FROM claude_asset_preferences
         WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
    )
    .bind(scope)
    .bind(kind)
    .bind(name)
    .execute(pool)
    .await
    .map_err(|e| format!("clear pin: {e}"))?;
    Ok(())
}

/// Re-point a pin from `from_scope` to `to_scope` for `(kind, name)`. Called on
/// `move`, where the primitive leaves `from_scope` and lands in `to_scope`:
/// rather than orphan the pin we carry it to the new location.
///
/// If no pin exists at `from_scope` this is a no-op. If a pin already exists at
/// `to_scope` (the destination was independently pinned) we keep the
/// destination's pin and drop the source pin — re-pointing must never clobber an
/// existing destination preference, and either way no dangling pin remains.
pub(crate) async fn repoint_pin(
    pool: &sqlx::SqlitePool,
    from_scope: &str,
    to_scope: &str,
    kind: &str,
    name: &str,
) -> Result<(), String> {
    use sqlx::Row;
    // Fetch the source pin (if any).
    let row = sqlx::query(
        "SELECT preferred_tier, preferred_source
         FROM claude_asset_preferences
         WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
    )
    .bind(from_scope)
    .bind(kind)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("repoint pin (read source): {e}"))?;
    let Some(row) = row else {
        // No source pin → nothing to carry.
        return Ok(());
    };
    let preferred_tier: String = row.get("preferred_tier");
    let preferred_source: Option<String> = row.get("preferred_source");

    // Upsert onto the destination scope unless it already has a pin (in which
    // case the destination's own preference wins). `INSERT OR IGNORE` leaves an
    // existing destination row intact; the source row is then deleted regardless.
    sqlx::query(
        "INSERT OR IGNORE INTO claude_asset_preferences
            (scope, asset_kind, asset_name, preferred_tier, preferred_source, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(to_scope)
    .bind(kind)
    .bind(name)
    .bind(&preferred_tier)
    .bind(&preferred_source)
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(|e| format!("repoint pin (write dest): {e}"))?;

    // Drop the now-stranded source pin.
    clear_pin_for(pool, from_scope, kind, name).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_simple() {
        let raw = "---\nname: x\nmodel: opus\n---\nbody here\n";
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm["name"], "x");
        assert_eq!(fm["model"], "opus");
        assert_eq!(body, "body here\n");
    }

    #[test]
    fn frontmatter_with_list() {
        let raw = "---\nname: x\nallowed-tools:\n  - Task\n  - Read\n---\nhello\n";
        let (fm, body) = split_frontmatter(raw);
        assert_eq!(fm["allowed-tools"][0], "Task");
        assert_eq!(fm["allowed-tools"][1], "Read");
        assert_eq!(body, "hello\n");
    }

    #[test]
    fn no_frontmatter() {
        let raw = "# just markdown\n";
        let (fm, body) = split_frontmatter(raw);
        assert!(fm.is_object());
        assert_eq!(body, raw);
    }

    #[test]
    fn under_claude_dir() {
        assert!(is_under_claude_dir(Path::new("/x/y/.claude/agents/a.md")));
        assert!(is_under_claude_dir(Path::new(
            "/home/u/.claude/settings.json"
        )));
        assert!(!is_under_claude_dir(Path::new("/x/y/agents/a.md")));
    }

    #[test]
    fn pin_scope_validation() {
        assert!(validate_pin_scope("workspace").is_ok());
        assert!(validate_pin_scope("project:music-2026").is_ok());
        assert!(validate_pin_scope("project:default").is_ok());
        assert!(validate_pin_scope("personal").is_err());
        assert!(validate_pin_scope("project:").is_err());
        assert!(validate_pin_scope("project:BadCase").is_err());
        assert!(validate_pin_scope("workspace:x").is_err());
    }

    #[test]
    fn pin_tier_validation() {
        for t in &["personal", "workspace_pkg", "project", "project_pkg"] {
            assert!(validate_pin_tier(t).is_ok());
        }
        assert!(validate_pin_tier("workspace").is_err());
        assert!(validate_pin_tier("Personal").is_err());
    }

    #[test]
    fn pin_kind_validation() {
        for k in &["skill", "agent", "command", "hook", "mcp"] {
            assert!(validate_pin_kind(k).is_ok());
        }
        assert!(validate_pin_kind("Skill").is_err());
        assert!(validate_pin_kind("hooks").is_err());
    }

    // ── Symlink / store metadata ──────────────────────────────────────────

    fn unique_tmp(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("ngwa_wp01_{tag}_{nonce}"));
        p
    }

    #[test]
    fn link_meta_plain_file_is_not_symlink() {
        let base = unique_tmp("plain");
        std::fs::create_dir_all(&base).unwrap();
        let f = base.join("agent.md");
        std::fs::write(&f, "---\nname: a\n---\nbody").unwrap();

        let meta = link_meta(&f, None);
        assert!(!meta.is_symlink);
        assert!(meta.link_target.is_none());
        assert!(!meta.in_store);

        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn link_meta_symlink_reports_target_and_in_store() {
        // Lay out a fake store + a project .claude/agents symlink pointing in.
        let root = unique_tmp("store");
        let store = root.join("store");
        let store_agents = store.join("agents");
        std::fs::create_dir_all(&store_agents).unwrap();
        let target = store_agents.join("shared.md");
        std::fs::write(&target, "---\nname: shared\n---\nbody").unwrap();

        let proj_agents = root.join("proj/.claude/agents");
        std::fs::create_dir_all(&proj_agents).unwrap();
        let link = proj_agents.join("shared.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let meta = link_meta(&link, Some(&store));
        assert!(meta.is_symlink, "the .claude entry is a symlink");
        let lt = meta.link_target.expect("link target present");
        assert!(lt.ends_with("shared.md"), "target resolves: {lt}");
        assert!(meta.in_store, "resolved target lives under the store root");

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn link_meta_symlink_outside_store_is_not_in_store() {
        let root = unique_tmp("outside");
        let store = root.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let target = elsewhere.join("local.md");
        std::fs::write(&target, "x").unwrap();
        let link = root.join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let meta = link_meta(&link, Some(&store));
        assert!(meta.is_symlink);
        assert!(meta.link_target.is_some());
        assert!(!meta.in_store, "target is outside the store root");
        // WP-03 req #6: an out-of-store symlink that RESOLVES reads as linked —
        // target_exists must be true even though in_store is false. This is the
        // signal the FE uses to classify `linked` (not `orphaned`).
        assert!(
            meta.target_exists,
            "resolving out-of-store symlink → linked, not orphaned"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn link_meta_dangling_symlink_target_does_not_exist() {
        // A symlink whose target is missing is the ONLY orphaned case.
        let root = unique_tmp("dangling");
        std::fs::create_dir_all(&root).unwrap();
        let link = root.join("broken.md");
        std::os::unix::fs::symlink(root.join("nope.md"), &link).unwrap();

        let meta = link_meta(&link, None);
        assert!(meta.is_symlink);
        assert!(!meta.target_exists, "dangling link → orphaned");
        // we still surface the raw (broken) target for display
        assert!(meta.link_target.is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn link_meta_plain_file_target_exists() {
        let base = unique_tmp("plain_exists");
        std::fs::create_dir_all(&base).unwrap();
        let f = base.join("agent.md");
        std::fs::write(&f, "x").unwrap();
        let meta = link_meta(&f, None);
        assert!(!meta.is_symlink);
        assert!(meta.target_exists, "a present non-symlink exists");
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn resolve_symlink_target_some_when_resolving_none_when_dangling() {
        let root = unique_tmp("resolve");
        std::fs::create_dir_all(&root).unwrap();
        let real = root.join("master");
        std::fs::create_dir_all(&real).unwrap();
        let ok_link = root.join("ok");
        std::os::unix::fs::symlink(&real, &ok_link).unwrap();
        let bad_link = root.join("bad");
        std::os::unix::fs::symlink(root.join("missing"), &bad_link).unwrap();

        let resolved = resolve_symlink_target(&ok_link).expect("resolving link → Some");
        assert_eq!(
            std::fs::canonicalize(&real).unwrap(),
            resolved,
            "resolves into the master dir"
        );
        assert!(
            resolve_symlink_target(&bad_link).is_none(),
            "dangling link → None"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn store_root_resolves_to_store_subdir() {
        // Whatever the platform resolves to, the leaf must be `store`.
        if let Some(s) = store_root() {
            assert_eq!(s.file_name().and_then(|n| n.to_str()), Some("store"));
        }
    }

    // ── WP-04 pin coexistence with the store mutations ─────────────────────

    /// In-memory pool seeded with the real pin-table migration.
    async fn pin_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory db");
        sqlx::query(include_str!(
            "../../migrations/0017_claude_asset_preferences.sql"
        ))
        .execute(&pool)
        .await
        .expect("create pin table");
        pool
    }

    /// Insert a pin row directly (mirrors what `claude_asset_pin` would write).
    async fn seed_pin(pool: &sqlx::SqlitePool, scope: &str, kind: &str, name: &str, tier: &str) {
        sqlx::query(
            "INSERT INTO claude_asset_preferences
                (scope, asset_kind, asset_name, preferred_tier, preferred_source, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(scope)
        .bind(kind)
        .bind(name)
        .bind(tier)
        .bind(Option::<String>::None)
        .bind(now_ms())
        .execute(pool)
        .await
        .expect("seed pin");
    }

    async fn pin_count(pool: &sqlx::SqlitePool, scope: &str, kind: &str, name: &str) -> i64 {
        use sqlx::Row;
        sqlx::query(
            "SELECT COUNT(*) AS n FROM claude_asset_preferences
             WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
        )
        .bind(scope)
        .bind(kind)
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("count")
        .get::<i64, _>("n")
    }

    /// disable / remove: a pinned primitive that goes unresolvable at its scope
    /// must leave NO dangling pin row.
    #[tokio::test]
    async fn disable_clears_dangling_pin() {
        let pool = pin_pool().await;
        seed_pin(&pool, "project:demo", "agent", "router", "personal").await;
        assert_eq!(pin_count(&pool, "project:demo", "agent", "router").await, 1);

        // The exact call the disable/remove command makes after the symlink/
        // merge mutation succeeds.
        clear_pin_for(&pool, "project:demo", "agent", "router")
            .await
            .expect("clear");

        assert_eq!(
            pin_count(&pool, "project:demo", "agent", "router").await,
            0,
            "pin must be gone after the primitive is disabled at that scope"
        );
        // Idempotent: a second clear (or a clear with no row) is a no-op.
        clear_pin_for(&pool, "project:demo", "agent", "router")
            .await
            .expect("clear is idempotent");
    }

    /// disable must only touch the pin for the mutated scope; a pin for the same
    /// asset in a different scope survives.
    #[tokio::test]
    async fn clear_pin_is_scope_local() {
        let pool = pin_pool().await;
        seed_pin(&pool, "workspace", "skill", "lint", "personal").await;
        seed_pin(&pool, "project:demo", "skill", "lint", "personal").await;

        clear_pin_for(&pool, "project:demo", "skill", "lint")
            .await
            .expect("clear");

        assert_eq!(pin_count(&pool, "project:demo", "skill", "lint").await, 0);
        assert_eq!(
            pin_count(&pool, "workspace", "skill", "lint").await,
            1,
            "the workspace pin for the same asset name must be untouched"
        );
    }

    /// move: a pinned primitive carried from one scope to another must re-point
    /// its pin to the destination — gone at the source, present at the dest, no
    /// dangling pin.
    #[tokio::test]
    async fn move_repoints_pin_to_destination() {
        let pool = pin_pool().await;
        seed_pin(&pool, "project:from", "command", "deploy", "project").await;

        repoint_pin(&pool, "project:from", "project:to", "command", "deploy")
            .await
            .expect("repoint");

        assert_eq!(
            pin_count(&pool, "project:from", "command", "deploy").await,
            0,
            "source pin must be gone after move"
        );
        assert_eq!(
            pin_count(&pool, "project:to", "command", "deploy").await,
            1,
            "pin must follow the primitive to the destination scope"
        );
        // The carried tier is preserved.
        use sqlx::Row;
        let tier: String = sqlx::query(
            "SELECT preferred_tier FROM claude_asset_preferences
             WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
        )
        .bind("project:to")
        .bind("command")
        .bind("deploy")
        .fetch_one(&pool)
        .await
        .expect("read dest")
        .get("preferred_tier");
        assert_eq!(tier, "project");
    }

    /// move into a scope that already has its own pin for the same asset keeps
    /// the destination's existing preference and still clears the source — never
    /// a dangling source pin, never a clobbered destination pin.
    #[tokio::test]
    async fn move_does_not_clobber_existing_dest_pin() {
        let pool = pin_pool().await;
        seed_pin(&pool, "project:from", "command", "deploy", "project").await;
        seed_pin(&pool, "project:to", "command", "deploy", "personal").await;

        repoint_pin(&pool, "project:from", "project:to", "command", "deploy")
            .await
            .expect("repoint");

        assert_eq!(
            pin_count(&pool, "project:from", "command", "deploy").await,
            0
        );
        use sqlx::Row;
        let tier: String = sqlx::query(
            "SELECT preferred_tier FROM claude_asset_preferences
             WHERE scope = ? AND asset_kind = ? AND asset_name = ?",
        )
        .bind("project:to")
        .bind("command")
        .bind("deploy")
        .fetch_one(&pool)
        .await
        .expect("read dest")
        .get("preferred_tier");
        assert_eq!(
            tier, "personal",
            "destination's own pin wins over the carried source pin"
        );
    }

    /// repoint with no source pin is a harmless no-op (covers move of an
    /// unpinned primitive).
    #[tokio::test]
    async fn repoint_no_source_pin_is_noop() {
        let pool = pin_pool().await;
        repoint_pin(&pool, "project:from", "project:to", "agent", "ghost")
            .await
            .expect("repoint no-op");
        assert_eq!(pin_count(&pool, "project:to", "agent", "ghost").await, 0);
    }

    // ── WP-17 cross-system scan (engine-parametric + Gemini reader) ─────────
    //
    // These tests mutate the process-global `HOME` to isolate the scan to a
    // tempdir, so they serialize on a module-local lock (mirrors the
    // `engine_adapters::test_util::HomeGuard` pattern, which is `pub(super)`
    // and not reachable from here).

    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
    }
    impl HomeGuard {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let previous = std::env::var_os("HOME");
            std::env::set_var("HOME", tmp.path());
            Self {
                previous,
                _tmp: tmp,
            }
        }
        fn home(&self) -> PathBuf {
            self._tmp.path().to_path_buf()
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Lay out a `~/.gemini/`-style user tree (no project roots) and assert the
    /// scan emits Gemini-tagged entries with the correct kind / format / scope.
    #[test]
    fn gemini_user_scope_scan_yields_tagged_entries() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        write(
            &h.join(".gemini/skills/lint/SKILL.md"),
            "---\nname: lint\ndescription: Lints things\n---\nbody\n",
        );
        write(
            &h.join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: Shared skill\n---\nx\n",
        );
        write(
            &h.join(".gemini/agents/router.md"),
            "---\nname: router\ndescription: Routes\n---\nb\n",
        );
        write(
            &h.join(".gemini/commands/git/commit.toml"),
            "description = \"Commit helper\"\nprompt = \"Write a commit message\"\n",
        );
        write(
            &h.join(".gemini/settings.json"),
            r#"{
                "hooks": {
                    "PreToolUse": [
                        { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/bin/echo hi" } ] }
                    ]
                },
                "mcpServers": {
                    "fs": { "command": "node", "args": ["server.js"], "env": { "TOKEN": "x" } }
                }
            }"#,
        );

        let cfg = scan_all(vec![]).expect("scan");

        let gem_skills: Vec<_> = cfg
            .skills
            .iter()
            .filter(|s| s.tag.system == EngineId::Gemini)
            .collect();
        let skill_names: Vec<&str> = gem_skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            skill_names.contains(&"lint") && skill_names.contains(&"shared"),
            "both ~/.gemini/skills and ~/.agents/skills surface: {skill_names:?}"
        );
        for s in &gem_skills {
            assert_eq!(s.tag.format, ConfigFormat::MdYaml);
            assert_eq!(s.scope, Scope::Personal);
            assert_eq!(s.tag.status, KindStatus::Active);
        }

        let router = cfg
            .agents
            .iter()
            .find(|a| a.name == "router" && a.tag.system == EngineId::Gemini)
            .expect("gemini agent router");
        assert_eq!(router.tag.format, ConfigFormat::MdYaml);
        assert_eq!(router.scope, Scope::Personal);

        let commit = cfg
            .commands
            .iter()
            .find(|c| c.tag.system == EngineId::Gemini)
            .expect("gemini command");
        assert_eq!(commit.name, "git:commit", "subdir-namespaced command name");
        assert_eq!(commit.tag.format, ConfigFormat::Toml);
        assert_eq!(commit.description.as_deref(), Some("Commit helper"));
        assert!(commit.body.contains("Write a commit message"));

        let hook = cfg
            .hooks
            .iter()
            .find(|hk| hk.tag.system == EngineId::Gemini)
            .expect("gemini hook");
        assert_eq!(hook.event, "PreToolUse");
        assert_eq!(hook.tag.format, ConfigFormat::JsonEmbedded);

        let mcp = cfg
            .mcps
            .iter()
            .find(|m| m.tag.system == EngineId::Gemini)
            .expect("gemini mcp");
        assert_eq!(mcp.name, "fs");
        assert_eq!(mcp.tag.format, ConfigFormat::JsonEmbedded);
        assert_eq!(mcp.command.as_deref(), Some("node"));
        assert_eq!(mcp.env_keys, vec!["TOKEN".to_string()]);
    }

    /// A Gemini project tree (`<root>/.gemini` + `<root>/.agents/skills`) is
    /// scoped Project and tagged Gemini.
    #[test]
    fn gemini_project_scope_scan() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path();

        write(
            &root.join(".gemini/agents/proj-agent.md"),
            "---\nname: proj-agent\n---\nbody\n",
        );
        write(
            &root.join(".agents/skills/proj-skill/SKILL.md"),
            "---\nname: proj-skill\n---\nx\n",
        );

        let cfg = scan_all(vec![root.to_string_lossy().to_string()]).expect("scan");
        let a = cfg
            .agents
            .iter()
            .find(|a| a.name == "proj-agent")
            .expect("project agent");
        assert_eq!(a.tag.system, EngineId::Gemini);
        assert_eq!(a.scope, Scope::Project);
        assert_eq!(
            a.project_root.as_deref(),
            Some(root.to_string_lossy().as_ref())
        );

        let s = cfg
            .skills
            .iter()
            .find(|s| s.name == "proj-skill")
            .expect("project skill");
        assert_eq!(s.tag.system, EngineId::Gemini);
        assert_eq!(s.scope, Scope::Project);
        drop(home);
    }

    /// The shipping Claude scan output must be **byte-unchanged** by WP-17:
    /// a Claude-only fixture serializes with NO `system`/`format`/`status`
    /// keys anywhere (the pre-WP-17 wire).
    #[test]
    fn claude_scan_output_is_byte_unchanged() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        write(
            &h.join(".claude/agents/router.md"),
            "---\nname: router\ndescription: Routes\nmodel: opus\n---\nbody\n",
        );
        write(
            &h.join(".claude/skills/lint/SKILL.md"),
            "---\nname: lint\ndescription: Lints\n---\nx\n",
        );
        write(
            &h.join(".claude/commands/deploy.md"),
            "---\ndescription: Deploy\n---\ncmd body\n",
        );
        write(
            &h.join(".claude/settings.json"),
            r#"{
                "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "/bin/true" } ] } ] },
                "mcpServers": { "fs": { "command": "node" } }
            }"#,
        );

        let cfg = scan_all(vec![]).expect("scan");
        let json = serde_json::to_value(&cfg).expect("serialize");

        fn assert_no_new_keys(v: &serde_json::Value) {
            match v {
                serde_json::Value::Object(m) => {
                    for k in ["system", "format", "status"] {
                        assert!(
                            !m.contains_key(k),
                            "Claude entry must omit `{k}` to stay byte-unchanged"
                        );
                    }
                    for sub in m.values() {
                        assert_no_new_keys(sub);
                    }
                }
                serde_json::Value::Array(a) => a.iter().for_each(assert_no_new_keys),
                _ => {}
            }
        }
        assert_no_new_keys(&json);

        assert!(cfg.agents.iter().any(|a| a.name == "router"));
        assert!(cfg.skills.iter().any(|s| s.name == "lint"));
        assert!(cfg.commands.iter().any(|c| c.name == "deploy"));
        assert!(!cfg.hooks.is_empty());
        assert!(cfg.mcps.iter().any(|m| m.name == "fs"));
    }

    /// Per-engine confinement: a path outside the active engine's declared
    /// roots is rejected by `path_under_any`, and the Gemini reader never
    /// reads outside `~/.gemini` / `~/.agents` / `<root>/.gemini`.
    #[test]
    fn engine_confinement_rejects_outside_paths() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        let gemini = crate::commands::engine_layout::engine_layout_by_id(EngineId::Gemini).unwrap();
        let proj = tempfile::tempdir().unwrap();
        let roots = engine_roots(&gemini, &[proj.path().to_string_lossy().to_string()]);

        assert!(path_under_any(&h.join(".gemini/agents/x.md"), &roots));
        assert!(path_under_any(&h.join(".agents/skills/x"), &roots));
        assert!(path_under_any(
            &proj.path().join(".gemini/settings.json"),
            &roots
        ));
        assert!(!path_under_any(&h.join(".claude/agents/x.md"), &roots));
        assert!(!path_under_any(Path::new("/etc/passwd"), &roots));
        assert!(!path_under_any(&h.join("Documents/secret.md"), &roots));

        // End-to-end: a stray `~/.gemini`-adjacent file outside the scanned
        // kind dirs is never surfaced.
        write(
            &h.join(".gemini/NOT_A_KIND/rogue.md"),
            "---\nname: rogue\n---\nx\n",
        );
        let cfg = scan_all(vec![]).expect("scan");
        assert!(
            !cfg.agents.iter().any(|a| a.name == "rogue"),
            "files outside declared kind dirs are not scanned"
        );
    }

    /// `SystemTag` serialization contract: Claude → no fields; Gemini → all
    /// three camelCase fields with kebab-case enum values.
    #[test]
    fn system_tag_serialization_shape() {
        let claude = SystemTag::claude(ConfigFormat::MdYaml);
        let j = serde_json::to_value(claude).unwrap();
        assert_eq!(j, serde_json::json!({}), "Claude tag emits nothing");

        let gem = SystemTag {
            system: EngineId::Gemini,
            format: ConfigFormat::Toml,
            status: KindStatus::Active,
        };
        let j = serde_json::to_value(gem).unwrap();
        assert_eq!(j["system"], "gemini");
        assert_eq!(j["format"], "toml");
        assert_eq!(j["status"], "active");
    }

    // ── WP-18 Codex reader ─────────────────────────────────────────────────

    /// Lay out a `~/.codex/` user tree (+ cross-tool `~/.agents/skills`) and
    /// assert the scan emits Codex-tagged entries with the right kind / format
    /// / scope — including a DEPRECATED prompt, a TOML agent, an inline-TOML
    /// hook, a `hooks.json` hook, and a `config.toml [mcp_servers]` server.
    #[test]
    fn codex_user_scope_scan_yields_tagged_entries() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        // skills: cross-tool `~/.agents/skills/` only.
        write(
            &h.join(".agents/skills/lint/SKILL.md"),
            "---\nname: lint\ndescription: Lints things\n---\nbody\n",
        );
        // agents: `~/.codex/agents/*.toml`.
        write(
            &h.join(".codex/agents/router.toml"),
            "name = \"router\"\ndescription = \"Routes requests\"\nmodel = \"gpt-5\"\nsystem_prompt = \"You route.\"\n",
        );
        // commands: `~/.codex/prompts/*.md` (DEPRECATED).
        write(
            &h.join(".codex/prompts/deploy.md"),
            "---\ndescription: Deploy helper\n---\nDeploy the thing\n",
        );
        // hooks: `~/.codex/hooks.json` (JSON).
        write(
            &h.join(".codex/hooks.json"),
            r#"{
                "hooks": {
                    "Stop": [ { "hooks": [ { "type": "command", "command": "/bin/echo done" } ] } ]
                }
            }"#,
        );
        // hooks (inline) + mcp: `~/.codex/config.toml`.
        write(
            &h.join(".codex/config.toml"),
            "[mcp_servers.fs]\ncommand = \"node\"\nargs = [\"server.js\"]\nenv = { TOKEN = \"x\" }\nenv_vars = [\"OPENAI_API_KEY\"]\n\n\
             [[hooks.PreToolUse]]\ntype = \"command\"\ncommand = \"/bin/echo pre\"\n",
        );

        let cfg = scan_all(vec![]).expect("scan");

        // skill (md-yaml, cross-tool, personal)
        let skill = cfg
            .skills
            .iter()
            .find(|s| s.name == "lint" && s.tag.system == EngineId::Codex)
            .expect("codex skill lint");
        assert_eq!(skill.tag.format, ConfigFormat::MdYaml);
        assert_eq!(skill.scope, Scope::Personal);
        assert_eq!(skill.tag.status, KindStatus::Active);

        // (b) TOML agent
        let agent = cfg
            .agents
            .iter()
            .find(|a| a.name == "router" && a.tag.system == EngineId::Codex)
            .expect("codex toml agent");
        assert_eq!(agent.tag.format, ConfigFormat::Toml);
        assert_eq!(agent.scope, Scope::Personal);
        assert_eq!(agent.description.as_deref(), Some("Routes requests"));
        assert_eq!(agent.model.as_deref(), Some("gpt-5"));
        assert!(agent.body.contains("You route."));

        // (a) DEPRECATED prompt command
        let cmd = cfg
            .commands
            .iter()
            .find(|c| c.name == "deploy" && c.tag.system == EngineId::Codex)
            .expect("codex deprecated prompt command");
        assert_eq!(cmd.tag.format, ConfigFormat::MdYaml);
        assert_eq!(
            cmd.tag.status,
            KindStatus::Deprecated,
            "Codex prompts are surfaced as deprecated commands"
        );
        assert_eq!(cmd.scope, Scope::Personal);

        // (c) MCP server from config.toml [mcp_servers]
        let mcp = cfg
            .mcps
            .iter()
            .find(|m| m.name == "fs" && m.tag.system == EngineId::Codex)
            .expect("codex mcp server");
        assert_eq!(mcp.tag.format, ConfigFormat::Toml);
        assert_eq!(mcp.command.as_deref(), Some("node"));
        assert_eq!(mcp.transport, "stdio");
        assert!(mcp.env_keys.contains(&"TOKEN".to_string()));
        assert!(
            mcp.env_keys.contains(&"OPENAI_API_KEY".to_string()),
            "env_vars forwarded keys are surfaced too: {:?}",
            mcp.env_keys
        );

        // hooks: BOTH the JSON `hooks.json` Stop hook AND the inline-TOML
        // PreToolUse hook are surfaced, tagged Codex.
        let codex_hooks: Vec<_> = cfg
            .hooks
            .iter()
            .filter(|hk| hk.tag.system == EngineId::Codex)
            .collect();
        let stop = codex_hooks
            .iter()
            .find(|hk| hk.event == "Stop")
            .expect("hooks.json Stop hook");
        assert_eq!(
            stop.tag.format,
            ConfigFormat::JsonEmbedded,
            "hooks.json hooks are json-embedded"
        );
        let pre = codex_hooks
            .iter()
            .find(|hk| hk.event == "PreToolUse")
            .expect("inline config.toml PreToolUse hook");
        assert_eq!(
            pre.tag.format,
            ConfigFormat::Toml,
            "inline config.toml hooks are tagged toml"
        );
        assert_eq!(pre.command_raw.as_deref(), Some("/bin/echo pre"));
    }

    /// A Codex project tree (`<root>/.codex` + `<root>/.agents/skills`) is
    /// scoped Project and tagged Codex.
    #[test]
    fn codex_project_scope_scan() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let proj = tempfile::tempdir().unwrap();
        let root = proj.path();

        write(
            &root.join(".codex/agents/proj-agent.toml"),
            "name = \"proj-agent\"\ndescription = \"Project agent\"\n",
        );
        write(
            &root.join(".agents/skills/proj-skill/SKILL.md"),
            "---\nname: proj-skill\n---\nx\n",
        );

        let cfg = scan_all(vec![root.to_string_lossy().to_string()]).expect("scan");
        let a = cfg
            .agents
            .iter()
            .find(|a| a.name == "proj-agent")
            .expect("project agent");
        assert_eq!(a.tag.system, EngineId::Codex);
        assert_eq!(a.scope, Scope::Project);
        assert_eq!(a.tag.format, ConfigFormat::Toml);
        assert_eq!(
            a.project_root.as_deref(),
            Some(root.to_string_lossy().as_ref())
        );

        // The cross-tool `.agents/skills/` path is shared by Gemini AND Codex,
        // so `proj-skill` surfaces once per engine (distinct rows by D-08) —
        // filter to the Codex row.
        let s = cfg
            .skills
            .iter()
            .find(|s| s.name == "proj-skill" && s.tag.system == EngineId::Codex)
            .expect("codex project skill");
        assert_eq!(s.scope, Scope::Project);
        drop(home);
    }

    /// Codex never reads `.codex/skills/` — a skill placed there must NOT
    /// surface. Missing `~/.codex` entirely yields zero Codex entries (no
    /// error).
    #[test]
    fn codex_ignores_codex_skills_dir_and_missing_tree() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        // A stray skill under `.codex/skills/` — Codex must ignore it.
        write(
            &h.join(".codex/skills/rogue/SKILL.md"),
            "---\nname: rogue\n---\nx\n",
        );

        let cfg = scan_all(vec![]).expect("scan");
        assert!(
            !cfg.skills
                .iter()
                .any(|s| s.tag.system == EngineId::Codex && s.name == "rogue"),
            "Codex never reads `.codex/skills/`"
        );
        // No other Codex entries either (the rest of the tree is absent).
        assert!(!cfg.agents.iter().any(|a| a.tag.system == EngineId::Codex));
        assert!(!cfg.mcps.iter().any(|m| m.tag.system == EngineId::Codex));
        assert!(cfg.errors.is_empty(), "missing files are not errors");
    }

    /// Adding the Codex reader must NOT change Gemini OR Claude scan output for
    /// a mixed fixture: the Claude entries stay byte-unchanged (no
    /// system/format/status keys) and the Gemini entries keep their Gemini tag.
    #[test]
    fn codex_addition_leaves_gemini_and_claude_unchanged() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        // Claude fixture.
        write(
            &h.join(".claude/agents/router.md"),
            "---\nname: router\ndescription: Routes\n---\nbody\n",
        );
        write(
            &h.join(".claude/settings.json"),
            r#"{ "mcpServers": { "fs": { "command": "node" } } }"#,
        );
        // Gemini fixture.
        write(
            &h.join(".gemini/agents/gm-agent.md"),
            "---\nname: gm-agent\n---\nb\n",
        );
        // Codex fixture (present so the Codex arm actually runs).
        write(
            &h.join(".codex/agents/cx-agent.toml"),
            "name = \"cx-agent\"\n",
        );

        let cfg = scan_all(vec![]).expect("scan");
        let json = serde_json::to_value(&cfg).expect("serialize");

        // Claude entries: no new keys anywhere on the Claude rows.
        let claude_agent = cfg
            .agents
            .iter()
            .find(|a| a.name == "router" && a.tag.system == EngineId::Claude)
            .expect("claude agent");
        let cj = serde_json::to_value(claude_agent).unwrap();
        for k in ["system", "format", "status"] {
            assert!(
                !cj.as_object().unwrap().contains_key(k),
                "Claude agent must omit `{k}`"
            );
        }
        assert!(cfg
            .mcps
            .iter()
            .any(|m| m.name == "fs" && m.tag.system == EngineId::Claude));

        // Gemini entry: still Gemini-tagged.
        let gm = cfg
            .agents
            .iter()
            .find(|a| a.name == "gm-agent")
            .expect("gemini agent");
        assert_eq!(gm.tag.system, EngineId::Gemini);
        assert_eq!(gm.tag.format, ConfigFormat::MdYaml);

        // Codex entry present (proves the arm ran without disturbing the others).
        assert!(cfg
            .agents
            .iter()
            .any(|a| a.name == "cx-agent" && a.tag.system == EngineId::Codex));

        // The serialized JSON contains all three systems' rows.
        let s = json.to_string();
        assert!(s.contains("\"system\":\"gemini\""));
        assert!(s.contains("\"system\":\"codex\""));
    }

    #[test]
    fn gemini_command_toml_parse() {
        let (desc, body) = parse_gemini_command_toml(
            "description = \"A cmd\"\nprompt = \"\"\"\nLine one\nLine two\n\"\"\"\n",
        );
        assert_eq!(desc.as_deref(), Some("A cmd"));
        assert_eq!(body, "Line one\nLine two");
    }
}
