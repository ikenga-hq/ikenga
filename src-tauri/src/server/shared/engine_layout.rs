//! Per-engine data descriptor — the frozen `G-ADAPTER` contract.
//!
//! `EngineLayout` records, for each AI-coding engine and each primitive kind,
//! WHERE its config lives, in WHAT format, and HOW it is stored. It is a pure
//! **data descriptor**, NOT an extension of the write-only `EngineAdapter`
//! trait (`crate::pkg::engine_adapter`). Layout (where/format/scope) is
//! deliberately separated from operation (how to write).
//!
//! This is the single source of truth that the Phase-2 scanner (WP-17/18) and
//! the TS bridge (WP-19) consume. **v2a is read-only** — this module only
//! defines the descriptor and populates it; it does not scan or mutate
//! anything.
//!
//! Path harvest sources (read, never modified by this WP):
//!   - Claude: the shipping Phase-1 scanner + store (`.claude/skills/`,
//!     `.mcp.json` / `~/.claude.json` for MCP, `settings.json#hooks`).
//!   - Gemini: `crate::pkg::engine_adapters::gemini` (`~/.gemini/...`).
//!   - Codex:  `crate::pkg::engine_adapters::codex` (`~/.codex/...`,
//!     cross-tool `.agents/skills/`).
//!
//! The matrix encoded here is the verified Round-9 research in
//! `plans/cockpit/04-discussion.md`. See that doc for the corrected seam
//! (all three engines have hooks + project scope; three serialization
//! formats; MCP/hooks are keys *inside* the settings file in Gemini/Codex;
//! skills share the cross-tool `.agents/skills/` path).
//!
//! Lives under `server::shared` (WP-19 slice 5b) so the daemon's
//! `engine_layout` arm and the config scanner (`shared::claude_config`)
//! compile in the headless build; `commands::engine_layout` re-exports it and
//! keeps the `#[tauri::command]`.

use std::collections::BTreeMap;

use serde::Serialize;

/// Stable engine identifier. Mirrors (a subset of) the chat-layer
/// `ChatEngineId` but scoped to the three engines whose config layout is
/// frozen in v2a.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineId {
    Claude,
    Gemini,
    Codex,
    Antigravity,
}

/// Which tier a scope lives at — a user-global root vs. a per-project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeTier {
    User,
    Project,
}

/// The five config primitive kinds Ngwa manages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrimitiveKind {
    Skill,
    Agent,
    Command,
    Hook,
    Mcp,
}

/// On-disk serialization format for a primitive.
///   - `MdYaml`        — Markdown body with a YAML frontmatter block
///                       (Claude/Gemini agents, Claude commands).
///   - `Toml`          — a TOML file or TOML table (Gemini commands,
///                       Codex agents, Codex MCP/hooks).
///   - `JsonEmbedded`  — a key inside a JSON settings file
///                       (Claude/Gemini MCP + hooks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigFormat {
    MdYaml,
    Toml,
    JsonEmbedded,
}

/// How a primitive is stored / toggled on disk.
///   - `SymlinkDir`  — a directory symlinked into the engine's dotdir
///                     (the symlink farm — skills, Claude/Gemini agents).
///   - `File`        — a standalone file (Gemini/Codex commands, Codex agents).
///   - `SettingsKey` — a key merged into a shared settings file
///                     (MCP + hooks for all engines except Claude's
///                     standalone `.mcp.json`, which is still modeled as a
///                     `SettingsKey` since it's a JSON-object key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mechanism {
    SymlinkDir,
    File,
    SettingsKey,
}

/// Whether a (engine, kind) cell is a live primitive or a deprecated one
/// kept only for read/migration (Codex `prompts/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KindStatus {
    Active,
    Deprecated,
}

/// One scope an engine supports — a root whose primitives Ngwa scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeDef {
    /// Stable scope id, referenced by `KindLayout::scopes`.
    pub id: &'static str,
    /// Human label for the sidebar facet.
    pub label: &'static str,
    /// User-global vs. per-project.
    pub tier: ScopeTier,
    /// Where the scope root resolves from — a path template / source note,
    /// e.g. `~/.claude` or `<root>/.gemini`.
    pub root_source: &'static str,
}

/// Per (engine, kind) layout cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindLayout {
    /// `true` if this engine has this primitive kind at all. Always `true`
    /// in the v2a matrix, but kept so future engines can omit a kind.
    pub exists: bool,
    /// Live vs. deprecated (Codex commands are deprecated → skills).
    pub status: KindStatus,
    /// Which scope ids (a subset of `EngineLayout::scopes`) this kind
    /// supports.
    pub scopes: Vec<&'static str>,
    /// On-disk serialization format.
    pub format: ConfigFormat,
    /// How it's stored / toggled.
    pub mechanism: Mechanism,
    /// Path template. `{user_root}` / `{root}` resolve to the scope root;
    /// `{settings_file}` to the engine's settings file; `{name}` to the
    /// primitive name. For `SettingsKey` cells the template carries a
    /// `#path.to.key` fragment, e.g. `{settings_file}#mcpServers.{name}`.
    pub location: &'static str,
    /// Strict-key validation on the backing settings file. Gemini
    /// `settings.json` is strict (`additionalProperties:false`, fail-fast
    /// ≥v0.22.4) → `true`. v2b uses this as a write-guard hook; **v2a
    /// ignores it.**
    pub strict_keys: bool,
}

/// The full layout descriptor for one engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineLayout {
    pub engine: EngineId,
    /// Display name (e.g. "Claude Code").
    pub display: &'static str,
    /// Short badge shown on interleaved rows (`CL` / `GM` / `CX`).
    pub badge: &'static str,
    /// Per-engine tint token name (Dusk Wood theme var, no leading `--`).
    pub tint: &'static str,
    pub scopes: Vec<ScopeDef>,
    /// Per-kind layout, keyed (and ordered) by `PrimitiveKind`.
    pub kinds: BTreeMap<PrimitiveKind, KindLayout>,
}

// ── Shared scope-id literals (kept as consts so the cells and the scope
//    table can't drift) ──────────────────────────────────────────────────
const SC_USER: &str = "user";
const SC_PROJECT: &str = "project";

fn user_project_scopes(user_root: &'static str, project_root: &'static str) -> Vec<ScopeDef> {
    vec![
        ScopeDef {
            id: SC_USER,
            label: "User",
            tier: ScopeTier::User,
            root_source: user_root,
        },
        ScopeDef {
            id: SC_PROJECT,
            label: "Project",
            tier: ScopeTier::Project,
            root_source: project_root,
        },
    ]
}

/// Convenience: a cell present in both user + project scope.
fn both_scopes() -> Vec<&'static str> {
    vec![SC_USER, SC_PROJECT]
}

// ── Claude (the shipping Phase-1 engine) ─────────────────────────────────
fn claude_layout() -> EngineLayout {
    let mut kinds = BTreeMap::new();

    kinds.insert(
        PrimitiveKind::Skill,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            location: "{root}/.claude/skills/{name}/",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Agent,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            location: "{root}/.claude/agents/{name}.md",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Command,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::File,
            location: "{root}/.claude/commands/{name}.md",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Hook,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            // settings.json / settings.local.json
            location: "{settings_file}#hooks",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Mcp,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            // project `.mcp.json#mcpServers` / user `~/.claude.json#mcpServers`
            location: "{mcp_file}#mcpServers.{name}",
            strict_keys: false,
        },
    );

    EngineLayout {
        engine: EngineId::Claude,
        display: "Claude Code",
        badge: "CL",
        tint: "agent",
        scopes: user_project_scopes("~/.claude", "<root>/.claude"),
        kinds,
    }
}

// ── Gemini ───────────────────────────────────────────────────────────────
fn gemini_layout() -> EngineLayout {
    let mut kinds = BTreeMap::new();

    kinds.insert(
        PrimitiveKind::Skill,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            // cross-tool `.agents/skills/` alias + `~/.gemini/skills/`
            location: "{root}/.agents/skills/{name}/",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Agent,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            location: "{user_root}/.gemini/agents/{name}.md",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Command,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::Toml,
            mechanism: Mechanism::File,
            // subdir-namespaced under commands/
            location: "{user_root}/.gemini/commands/**/{name}.toml",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Hook,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            location: "{settings_file}#hooks",
            // settings.json strict-key validation, but hooks block itself
            // is a recognized key; the strict flag is engine-wide on the
            // settings file (consumed by the MCP cell's write-guard).
            strict_keys: true,
        },
    );
    kinds.insert(
        PrimitiveKind::Mcp,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            // ~/.gemini/settings.json#mcpServers (additionalProperties:false)
            location: "{user_root}/.gemini/settings.json#mcpServers.{name}",
            strict_keys: true,
        },
    );

    EngineLayout {
        engine: EngineId::Gemini,
        display: "Gemini CLI",
        badge: "GM",
        tint: "systemic",
        scopes: user_project_scopes("~/.gemini", "<root>/.gemini"),
        kinds,
    }
}

// ── Antigravity ───────────────────────────────────────────────────────────
fn antigravity_layout() -> EngineLayout {
    let mut kinds = BTreeMap::new();

    kinds.insert(
        PrimitiveKind::Skill,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            location: "{root}/.agents/skills/{name}/",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Agent,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            location: "{user_root}/agents/{name}.md",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Command,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::Toml,
            mechanism: Mechanism::File,
            location: "{user_root}/commands/**/{name}.toml",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Hook,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            location: "{settings_file}#hooks",
            strict_keys: true,
        },
    );
    kinds.insert(
        PrimitiveKind::Mcp,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::JsonEmbedded,
            mechanism: Mechanism::SettingsKey,
            location: "{user_root}/../config/mcp_config.json#mcpServers.{name}",
            strict_keys: true,
        },
    );

    EngineLayout {
        engine: EngineId::Antigravity,
        display: "Antigravity CLI",
        badge: "AG",
        tint: "antigravity",
        scopes: user_project_scopes("~/.gemini/antigravity-cli", "<root>/.gemini"),
        kinds,
    }
}

// ── Codex ─────────────────────────────────────────────────────────────────
fn codex_layout() -> EngineLayout {
    let mut kinds = BTreeMap::new();

    kinds.insert(
        PrimitiveKind::Skill,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::SymlinkDir,
            // cross-tool `.agents/skills/` ONLY — Codex never uses
            // `.codex/skills/`.
            location: "{root}/.agents/skills/{name}/",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Agent,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::Toml,
            mechanism: Mechanism::File,
            // MD→TOML transcode
            location: "{user_root}/.codex/agents/{name}.toml",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Command,
        KindLayout {
            exists: true,
            // superseded by skills
            status: KindStatus::Deprecated,
            scopes: both_scopes(),
            format: ConfigFormat::MdYaml,
            mechanism: Mechanism::File,
            location: "{user_root}/.codex/prompts/{name}.md",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Hook,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::Toml,
            mechanism: Mechanism::SettingsKey,
            // `~/.codex/hooks.json` OR inline `config.toml#hooks`
            location: "{user_root}/.codex/hooks.json",
            strict_keys: false,
        },
    );
    kinds.insert(
        PrimitiveKind::Mcp,
        KindLayout {
            exists: true,
            status: KindStatus::Active,
            scopes: both_scopes(),
            format: ConfigFormat::Toml,
            mechanism: Mechanism::SettingsKey,
            // lenient top-level
            location: "{user_root}/.codex/config.toml#mcp_servers.{name}",
            strict_keys: false,
        },
    );

    EngineLayout {
        engine: EngineId::Codex,
        display: "Codex CLI",
        badge: "CX",
        tint: "achievement",
        // project `.codex` is trusted-repo-only; modeled in the contract,
        // env-plumbing deferred to the Codex phase (Round 9).
        scopes: user_project_scopes("~/.codex", "<repo>/.codex (trusted)"),
        kinds,
    }
}

/// All engine layouts, in display order (`Claude`, `Gemini`, `Codex`).
/// This is the frozen `G-ADAPTER` data the scanner + TS bridge consume.
pub fn engine_layouts() -> Vec<EngineLayout> {
    vec![
        claude_layout(),
        gemini_layout(),
        codex_layout(),
        antigravity_layout(),
    ]
}

/// Look up a single engine's layout by id.
pub fn engine_layout_by_id(id: EngineId) -> Option<EngineLayout> {
    engine_layouts().into_iter().find(|e| e.engine == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(id: EngineId) -> EngineLayout {
        engine_layouts()
            .into_iter()
            .find(|e| e.engine == id)
            .unwrap_or_else(|| panic!("missing engine {id:?}"))
    }

    fn cell(layout: &EngineLayout, kind: PrimitiveKind) -> &KindLayout {
        layout
            .kinds
            .get(&kind)
            .unwrap_or_else(|| panic!("missing {kind:?} for {:?}", layout.engine))
    }

    #[test]
    fn all_four_engines_present() {
        let all = engine_layouts();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].engine, EngineId::Claude);
        assert_eq!(all[1].engine, EngineId::Gemini);
        assert_eq!(all[2].engine, EngineId::Codex);
        assert_eq!(all[3].engine, EngineId::Antigravity);
        // by_id agrees
        for e in [
            EngineId::Claude,
            EngineId::Gemini,
            EngineId::Codex,
            EngineId::Antigravity,
        ] {
            assert_eq!(engine_layout_by_id(e).unwrap().engine, e);
        }
    }

    #[test]
    fn every_engine_kind_cell_exists() {
        let kinds = [
            PrimitiveKind::Skill,
            PrimitiveKind::Agent,
            PrimitiveKind::Command,
            PrimitiveKind::Hook,
            PrimitiveKind::Mcp,
        ];
        for layout in engine_layouts() {
            assert_eq!(layout.kinds.len(), 5, "{:?}", layout.engine);
            for k in kinds {
                let c = cell(&layout, k);
                assert!(c.exists, "{:?}/{k:?} should exist", layout.engine);
                // every kind supports both scopes in the v2a matrix
                assert!(
                    c.scopes.contains(&"user") && c.scopes.contains(&"project"),
                    "{:?}/{k:?} scope set {:?}",
                    layout.engine,
                    c.scopes
                );
            }
        }
    }

    #[test]
    fn matrix_format_and_mechanism() {
        // Claude
        let cl = find(EngineId::Claude);
        for k in [PrimitiveKind::Skill, PrimitiveKind::Agent] {
            assert_eq!(cell(&cl, k).mechanism, Mechanism::SymlinkDir);
            assert_eq!(cell(&cl, k).format, ConfigFormat::MdYaml);
        }
        assert_eq!(cell(&cl, PrimitiveKind::Command).mechanism, Mechanism::File);
        assert_eq!(
            cell(&cl, PrimitiveKind::Command).format,
            ConfigFormat::MdYaml
        );
        for k in [PrimitiveKind::Hook, PrimitiveKind::Mcp] {
            assert_eq!(cell(&cl, k).mechanism, Mechanism::SettingsKey);
            assert_eq!(cell(&cl, k).format, ConfigFormat::JsonEmbedded);
        }

        // Gemini
        let gm = find(EngineId::Gemini);
        assert_eq!(
            cell(&gm, PrimitiveKind::Skill).mechanism,
            Mechanism::SymlinkDir
        );
        assert_eq!(
            cell(&gm, PrimitiveKind::Agent).mechanism,
            Mechanism::SymlinkDir
        );
        assert_eq!(cell(&gm, PrimitiveKind::Agent).format, ConfigFormat::MdYaml);
        assert_eq!(cell(&gm, PrimitiveKind::Command).format, ConfigFormat::Toml);
        assert_eq!(cell(&gm, PrimitiveKind::Command).mechanism, Mechanism::File);
        assert_eq!(
            cell(&gm, PrimitiveKind::Hook).format,
            ConfigFormat::JsonEmbedded
        );
        assert_eq!(
            cell(&gm, PrimitiveKind::Mcp).format,
            ConfigFormat::JsonEmbedded
        );

        // Codex
        let cx = find(EngineId::Codex);
        assert_eq!(
            cell(&cx, PrimitiveKind::Skill).mechanism,
            Mechanism::SymlinkDir
        );
        assert_eq!(cell(&cx, PrimitiveKind::Agent).format, ConfigFormat::Toml);
        assert_eq!(cell(&cx, PrimitiveKind::Mcp).format, ConfigFormat::Toml);
        assert_eq!(
            cell(&cx, PrimitiveKind::Mcp).mechanism,
            Mechanism::SettingsKey
        );
    }

    #[test]
    fn gemini_settings_strict_keys() {
        let gm = find(EngineId::Gemini);
        assert!(
            cell(&gm, PrimitiveKind::Mcp).strict_keys,
            "Gemini settings.json MCP must be strict-keyed"
        );
        // Codex config.toml is lenient at top level.
        let cx = find(EngineId::Codex);
        assert!(!cell(&cx, PrimitiveKind::Mcp).strict_keys);
        // Claude MCP is lenient per the brief.
        let cl = find(EngineId::Claude);
        assert!(!cell(&cl, PrimitiveKind::Mcp).strict_keys);
    }

    #[test]
    fn codex_command_is_deprecated() {
        let cx = find(EngineId::Codex);
        assert_eq!(
            cell(&cx, PrimitiveKind::Command).status,
            KindStatus::Deprecated
        );
        // every other cell is Active
        for layout in engine_layouts() {
            for (kind, c) in &layout.kinds {
                if layout.engine == EngineId::Codex && *kind == PrimitiveKind::Command {
                    continue;
                }
                assert_eq!(c.status, KindStatus::Active, "{:?}/{kind:?}", layout.engine);
            }
        }
    }

    #[test]
    fn claude_cells_reproduce_shipping_phase1_paths() {
        // Guards against a Phase-1 regression: the Claude templates must match
        // the live scanner/store paths.
        let cl = find(EngineId::Claude);
        assert_eq!(
            cell(&cl, PrimitiveKind::Skill).location,
            "{root}/.claude/skills/{name}/"
        );
        assert_eq!(
            cell(&cl, PrimitiveKind::Agent).location,
            "{root}/.claude/agents/{name}.md"
        );
        assert_eq!(
            cell(&cl, PrimitiveKind::Command).location,
            "{root}/.claude/commands/{name}.md"
        );
        assert_eq!(
            cell(&cl, PrimitiveKind::Hook).location,
            "{settings_file}#hooks"
        );
        // MCP rides the standalone `.mcp.json` (project) / `~/.claude.json`
        // (user) — the `{mcp_file}` placeholder, NOT `{settings_file}`.
        let mcp = cell(&cl, PrimitiveKind::Mcp).location;
        assert_eq!(mcp, "{mcp_file}#mcpServers.{name}");
    }

    #[test]
    fn serializes_to_json() {
        // The descriptor must be Serialize so it can cross the Tauri / TS
        // boundary. Smoke the enum rename + key shapes.
        let json = serde_json::to_string(&engine_layouts()).unwrap();
        assert!(json.contains("\"engine\":\"claude\""));
        assert!(json.contains("\"engine\":\"gemini\""));
        assert!(json.contains("\"engine\":\"codex\""));
        assert!(json.contains("\"engine\":\"antigravity\""));
        assert!(json.contains("\"mechanism\":\"symlink-dir\""));
        assert!(json.contains("\"format\":\"json-embedded\""));
        assert!(json.contains("\"format\":\"md-yaml\""));
        assert!(json.contains("\"status\":\"deprecated\""));
        assert!(json.contains("\"strictKeys\":true"));
    }
}
