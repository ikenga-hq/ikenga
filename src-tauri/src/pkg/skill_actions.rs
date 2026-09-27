//! Skill-action discovery (WP-13: dispatch-only lighthouse).
//!
//! Reads an installed `kind: skill` pkg's
//! `<pkg_root>/<skills_dir>/<skill>/actions/*.md` files, parses the YAML
//! frontmatter between the leading `---` fences, and returns a flat list of
//! [`SkillAction`]s. The chrome renders each action as a button in the pkg's
//! pane header; for this MVP only `ux_mode: confirm` actions dispatch (the rest
//! render as disabled placeholders).
//!
//! The frontmatter shape is the Rust half of the `ActionFrontmatter` contract
//! published in `@ikenga/contract` (`contract/src/action-frontmatter.ts`). The
//! enum value lists below (ux modes, domains, capabilities, setup modes) MUST be
//! kept in lockstep with that file — same convention as `manifest.rs` ↔
//! `manifest.ts`.
//!
//! Validation is deliberately split: a file that is malformed or violates the
//! two load-bearing enums (`ux_mode`, `domain`) is skipped with a
//! `tracing::warn!` rather than aborting discovery for the whole pkg. A missing
//! `ux_mode` invalidates the file (no silent default). Softer drift — an unknown
//! top-level key, an out-of-set capability/trigger/setup value — is warned about
//! but the file is kept (forward-compat, mirroring the manifest loader).
//!
//! Uses `serde_yaml` — already a direct dependency (frontmatter parsing for
//! `.claude/agents`/`skills`/`commands`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pkg::manifest::RequiresEntry;

/// Resolve the Ngwa central store root (`<app_data_dir>/store/`) using the same
/// platform conventions Tauri uses for `app_data_dir` — env only, no
/// `AppHandle`. Returns `None` if the platform's data dir can't be resolved.
///
/// Canonical home of what `commands::claude_config::store_root` returns (that
/// one delegates here). It lives in this ungated module so the headless daemon
/// resolves the same `store/skills/<name>` edges as the desktop.
///
/// In the daemon this resolves from the DAEMON PROCESS's env (HOME /
/// XDG_DATA_HOME / APPDATA): one store for every caller. That is a
/// single-user seam, same as the daemon's `fs_home`; per-principal store
/// resolution is WP-20 (G-PRINCIPAL).
pub fn store_root() -> Option<PathBuf> {
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

// ---- Contract enums (lockstep with contract/src/action-frontmatter.ts) ------
//
// Keep these arrays byte-for-byte aligned with the corresponding `z.enum([...])`
// in `contract/src/action-frontmatter.ts`. A drift here silently rejects (ux
// modes / domains) or under-warns (capabilities / setup modes) real skills.

/// `UxModeEnum` — exactly five presentation modes.
const UX_MODES: &[&str] = &["confirm", "silent", "form", "streaming", "approve"];

/// `DomainEnum` — the eight stateful domains plus `skill-core`.
const DOMAINS: &[&str] = &[
    "tasks",
    "mail",
    "outbound",
    "sales",
    "finance",
    "content",
    "research",
    "strategy",
    "skill-core",
];

/// `CapabilityEnum` — coarse capability grants.
const CAPABILITIES: &[&str] = &[
    "sqlite", "mcp", "sidecar", "network", "fs", "secrets", "chat",
];

/// `Trigger` discriminant kinds.
const TRIGGER_KINDS: &[&str] = &["manual", "schedule", "webhook", "event"];

/// `SetupModeEnum`.
const SETUP_MODES: &[&str] = &["ai_infer", "interview"];

/// Every top-level key the `ActionFrontmatter` object schema declares. Any other
/// top-level key is unknown → warned (forward-compat).
const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "name",
    "description",
    "domain",
    "ux_mode",
    "inputs_schema",
    "run",
    "triggers",
    "depends_on",
    "requires_capabilities",
    "setup",
];

/// A single skill action, flattened for the TS renderer (camelCase over the
/// Tauri bridge). Mirrors the `SkillAction` interface in `src/types/pkg.ts`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAction {
    pub pkg_id: String,
    /// The skill directory name (e.g. "pa").
    pub skill: String,
    /// The action verb — frontmatter `verb`/`name`, else the file stem.
    pub verb: String,
    /// Display name — frontmatter `name`, else the verb.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// One of: confirm | streaming | approve | (other). Only `confirm`
    /// dispatches in this MVP; everything else renders disabled.
    pub ux_mode: String,
    /// The `run.kind` (e.g. "chat_prompt").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_kind: Option<String>,
    /// The `run.prompt` template, if present — what gets seeded into chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_template: Option<String>,
    /// The `inputs_schema` block re-serialized as a JSON string for the FE.
    /// (skill-pa authors it as `inputs_schema`, not `inputs.schema`.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inputs_schema_json: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// How the action can be invoked (`manual` / `schedule` / `webhook` /
    /// `event`). Empty ⇒ manual-only (the contract default).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<SkillTrigger>,
    /// Coarse capability grants this action declares (`CapabilityEnum`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub requires_capabilities: Vec<String>,
    /// The setup lifecycle block — present ONLY on the well-known `setup` action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<SkillSetup>,
}

/// A flattened trigger for the FE. Mirrors the `Trigger` discriminated union in
/// `contract/src/action-frontmatter.ts`; only the fields relevant to `kind`
/// are populated.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillTrigger {
    pub kind: String,
    /// schedule: 5-field crontab expression.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// schedule: optional human label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// webhook: stable path segment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// event: internal event name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// The setup lifecycle spec for the FE. Mirrors `SetupSpec` in the contract.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSetup {
    pub mode: String,
    pub template_version: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub infer_sources: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interview_questions: Vec<String>,
}

// ---- Frontmatter deserialization shapes -----------------------------------

// Serde does not `deny_unknown_fields` here: unknown top-level keys are captured
// separately (via a second parse into a `serde_yaml::Mapping`) and warned about,
// so forward-compat manifests are not rejected. See `parse_action_file`.
#[derive(Debug, Deserialize)]
struct ActionFrontmatter {
    name: Option<String>,
    description: Option<String>,
    domain: Option<String>,
    ux_mode: Option<String>,
    run: Option<RunBlock>,
    /// skill-pa authors this as a top-level `inputs_schema:` mapping.
    inputs_schema: Option<serde_yaml::Value>,
    #[serde(default)]
    triggers: Vec<TriggerBlock>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    requires_capabilities: Vec<String>,
    setup: Option<SetupBlock>,
}

#[derive(Debug, Deserialize)]
struct RunBlock {
    kind: Option<String>,
    // chat_prompt
    prompt: Option<String>,
    // sidecar
    sidecar_id: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    // mcp_tool
    server_id: Option<String>,
    tool: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TriggerBlock {
    kind: String,
    cron: Option<String>,
    label: Option<String>,
    path: Option<String>,
    event: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetupBlock {
    mode: Option<String>,
    template_version: Option<u32>,
    #[serde(default)]
    infer_sources: Vec<String>,
    #[serde(default)]
    interview_questions: Vec<String>,
}

/// Extract the YAML frontmatter block (between the leading `---` fences) from a
/// markdown file body. Returns `None` if the file does not start with a `---`
/// fence or the closing fence is missing.
fn extract_frontmatter(body: &str) -> Option<&str> {
    let trimmed = body.trim_start_matches('\u{feff}');
    // First line must be the opening fence.
    let first = trimmed.lines().next()?;
    if first.trim() != "---" {
        return None;
    }
    // Frontmatter starts right after the opening fence line.
    let after_open = trimmed.find('\n')? + 1;
    let rest = &trimmed[after_open..];
    // Line scan for the closing "---" fence; return everything before it.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).trim() == "---" {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    None
}

/// Parse a single action `.md` file into a [`SkillAction`].
/// Returns `None` (and logs a warning) on any parse failure.
fn parse_action_file(pkg_id: &str, skill: &str, path: &Path) -> Option<SkillAction> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(?path, error = %e, "skill_actions: failed to read action file");
            return None;
        }
    };

    let fm_src = match extract_frontmatter(&body) {
        Some(s) => s,
        None => {
            tracing::warn!(?path, "skill_actions: no YAML frontmatter fence; skipping");
            return None;
        }
    };

    let fm: ActionFrontmatter = match serde_yaml::from_str(fm_src) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(?path, error = %e, "skill_actions: frontmatter parse failed; skipping");
            return None;
        }
    };

    // Forward-compat: warn (do not reject) on any unknown top-level key. Re-parse
    // into a generic mapping — `ActionFrontmatter` does not `deny_unknown_fields`
    // so extras are silently dropped by the typed parse above; this surfaces them.
    if let Ok(serde_yaml::Value::Mapping(map)) =
        serde_yaml::from_str::<serde_yaml::Value>(fm_src)
    {
        for key in map.keys() {
            if let Some(k) = key.as_str() {
                if !KNOWN_TOP_LEVEL_KEYS.contains(&k) {
                    tracing::warn!(?path, key = %k, "skill_actions: unknown top-level frontmatter key (ignored)");
                }
            }
        }
    }

    // The contract has no `verb` — the stable action id IS `name` (kebab-case).
    // Fall back to the file stem only when `name` is absent.
    let name = fm.name.clone().unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("action")
            .to_string()
    });
    let verb = name.clone();

    // ux_mode: REQUIRED and must be one of the five contract modes. A missing or
    // out-of-set ux_mode invalidates the file (skip with warn) — no silent default.
    let ux_mode = match fm.ux_mode.as_deref() {
        Some(m) if UX_MODES.contains(&m) => m.to_string(),
        Some(bad) => {
            tracing::warn!(?path, ux_mode = %bad, "skill_actions: unknown ux_mode; skipping");
            return None;
        }
        None => {
            tracing::warn!(?path, "skill_actions: missing ux_mode; skipping");
            return None;
        }
    };

    // domain: the contract requires it, but we stay lenient about *absence* (a
    // domain-less file still lists, carrying `None`). A domain that is *present
    // but out-of-set* is a hard error (skip with warn) — same gate as ux_mode.
    let domain = match fm.domain.as_deref() {
        Some(d) if DOMAINS.contains(&d) => Some(d.to_string()),
        Some(bad) => {
            tracing::warn!(?path, domain = %bad, "skill_actions: unknown domain; skipping");
            return None;
        }
        None => None,
    };

    // run binding: validate the discriminant + its required sub-fields. Soft —
    // a malformed run block warns but does not invalidate the whole action.
    let (run_kind, prompt_template) = match &fm.run {
        Some(r) => {
            match r.kind.as_deref() {
                Some("chat_prompt") => {
                    if r.prompt.as_deref().map(str::trim).unwrap_or("").is_empty() {
                        tracing::warn!(?path, "skill_actions: chat_prompt run has no prompt");
                    }
                }
                Some("sidecar") => {
                    if r.sidecar_id.is_none() {
                        tracing::warn!(?path, "skill_actions: sidecar run missing sidecar_id");
                    }
                    // `args` defaults to [] — nothing to validate.
                    let _ = &r.args;
                }
                Some("mcp_tool") => {
                    if r.server_id.is_none() || r.tool.is_none() {
                        tracing::warn!(?path, "skill_actions: mcp_tool run missing server_id/tool");
                    }
                }
                Some(other) => {
                    tracing::warn!(?path, run_kind = %other, "skill_actions: unknown run.kind");
                }
                None => {
                    tracing::warn!(?path, "skill_actions: run block missing kind");
                }
            }
            (r.kind.clone(), r.prompt.clone().map(|p| p.trim_end().to_string()))
        }
        None => (None, None),
    };

    // requires_capabilities: warn on any value outside `CapabilityEnum`
    // (forward-compat — keep the file).
    for cap in &fm.requires_capabilities {
        if !CAPABILITIES.contains(&cap.as_str()) {
            tracing::warn!(?path, capability = %cap, "skill_actions: unknown capability (kept)");
        }
    }

    // triggers: validate kind + kind-specific required field. Warn-only (kept).
    let mut triggers = Vec::with_capacity(fm.triggers.len());
    for t in &fm.triggers {
        if !TRIGGER_KINDS.contains(&t.kind.as_str()) {
            tracing::warn!(?path, kind = %t.kind, "skill_actions: unknown trigger kind (kept)");
        }
        match t.kind.as_str() {
            "schedule" if t.cron.is_none() => {
                tracing::warn!(?path, "skill_actions: schedule trigger missing cron");
            }
            "webhook" if t.path.is_none() => {
                tracing::warn!(?path, "skill_actions: webhook trigger missing path");
            }
            "event" if t.event.is_none() => {
                tracing::warn!(?path, "skill_actions: event trigger missing event");
            }
            _ => {}
        }
        triggers.push(SkillTrigger {
            kind: t.kind.clone(),
            cron: t.cron.clone(),
            label: t.label.clone(),
            path: t.path.clone(),
            event: t.event.clone(),
        });
    }

    // setup: only valid on the well-known `setup` action (contract superRefine).
    // Warn-only; a malformed/misplaced setup block is dropped, not fatal.
    let setup = match &fm.setup {
        Some(s) => {
            if name != "setup" {
                tracing::warn!(?path, "skill_actions: `setup` block only valid on the setup action (dropped)");
            }
            if let Some(m) = s.mode.as_deref() {
                if !SETUP_MODES.contains(&m) {
                    tracing::warn!(?path, mode = %m, "skill_actions: unknown setup mode");
                }
            }
            match (s.mode.clone(), s.template_version) {
                (Some(mode), Some(template_version)) if name == "setup" => Some(SkillSetup {
                    mode,
                    template_version,
                    infer_sources: s.infer_sources.clone(),
                    interview_questions: s.interview_questions.clone(),
                }),
                _ => {
                    if name == "setup" {
                        tracing::warn!(?path, "skill_actions: setup block missing mode/template_version (dropped)");
                    }
                    None
                }
            }
        }
        None => {
            if name == "setup" {
                tracing::warn!(?path, "skill_actions: setup action missing its setup block");
            }
            None
        }
    };

    let inputs_schema_json = fm
        .inputs_schema
        .and_then(|v| serde_json::to_string(&v).ok());

    Some(SkillAction {
        pkg_id: pkg_id.to_string(),
        skill: skill.to_string(),
        verb,
        name,
        description: fm.description,
        domain,
        ux_mode,
        run_kind,
        prompt_template,
        inputs_schema_json,
        depends_on: fm.depends_on,
        triggers,
        requires_capabilities: fm.requires_capabilities,
        setup,
    })
}

/// Scan a resolved pkg root for skill actions.
///
/// `skills_dir` is the manifest's `skills` field (a relative dir like "skills").
/// Layout scanned: `<pkg_root>/<skills_dir>/<skill>/actions/*.md`.
/// Symlinked roots/skill dirs are followed by `std::fs` automatically (we use
/// `metadata()`, not `symlink_metadata()`), so dev-mounted (symlinked) pkgs
/// resolve transparently.
#[allow(dead_code)]
pub fn discover_actions(pkg_id: &str, pkg_root: &Path, skills_dir: &str) -> Vec<SkillAction> {
    let mut out = Vec::new();
    let skills_root = pkg_root.join(skills_dir);

    let skill_entries = match std::fs::read_dir(&skills_root) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(?skills_root, error = %e, "skill_actions: skills dir unreadable");
            return out;
        }
    };

    for skill_entry in skill_entries.flatten() {
        let skill_path = skill_entry.path();
        // Follow symlinks via metadata() (not symlink_metadata()).
        if !skill_path.metadata().map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        let skill_name = match skill_path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        out.extend(scan_skill_actions_dir(pkg_id, &skill_name, &skill_path));
    }

    // Deterministic order: by skill then verb.
    out.sort_by(|a, b| a.skill.cmp(&b.skill).then(a.verb.cmp(&b.verb)));
    out
}

/// Scan ONE skill directory's `actions/*.md` files into [`SkillAction`]s.
/// `skill_dir` is the skill's own directory (containing `actions/`); for a
/// store-resolved standalone primitive (WP-17) this is `store/skills/<name>`.
/// A skill without an `actions/` dir contributes nothing (not an error).
fn scan_skill_actions_dir(pkg_id: &str, skill_name: &str, skill_dir: &Path) -> Vec<SkillAction> {
    let mut out = Vec::new();
    let action_entries = match std::fs::read_dir(skill_dir.join("actions")) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for action_entry in action_entries.flatten() {
        let action_path = action_entry.path();
        if action_path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        // Skip non-action docs like actions/README.md.
        if action_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.eq_ignore_ascii_case("README.md"))
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(action) = parse_action_file(pkg_id, skill_name, &action_path) {
            out.push(action);
        }
    }
    out
}

/// Discover a pkg's skill actions by following its manifest `requires` to the
/// standalone skill primitives in the Ọba store (WP-17 — pkgs no longer bundle
/// skills; a `requires:[{kind:"skill",name}]` edge points at `store/skills/<name>`,
/// the canonical the Ọba resolver/boot-seeding installed). Returns an empty Vec
/// for a pkg that requires no skills or whose required skills aren't installed.
///
/// WP-20: also handles `requires:[{kind:"bundle",name}]` — for each member dir
/// under `store/bundles/<name>/<member>/` the member's `actions/*.md` files are
/// scanned (reusing `scan_skill_actions_dir` per member; the reported `skill`
/// name is the member leaf). A bundle whose canonical dir is absent contributes
/// nothing (not an error — same lenient contract as a missing skill dir).
pub fn list_actions_for_pkg(
    pkg_id: &str,
    requires: &[RequiresEntry],
    store: &Path,
) -> Vec<SkillAction> {
    let mut out = Vec::new();
    for req in requires {
        match req.kind.as_str() {
            "skill" => {
                let skill_dir = store.join("skills").join(&req.name);
                if !skill_dir.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                    continue;
                }
                out.extend(scan_skill_actions_dir(pkg_id, &req.name, &skill_dir));
            }
            "bundle" => {
                // WP-20: expand the bundle → its member skill subdirs.
                // Layout: store/bundles/<bundle-name>/<member>/actions/*.md
                let bundle_dir = store.join("bundles").join(&req.name);
                if !bundle_dir.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                    continue;
                }
                let rd = match std::fs::read_dir(&bundle_dir) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(
                            ?bundle_dir,
                            error = %e,
                            "list_actions_for_pkg: bundle dir unreadable"
                        );
                        continue;
                    }
                };
                let mut members: Vec<(String, std::path::PathBuf)> = rd
                    .flatten()
                    .filter_map(|entry| {
                        let p = entry.path();
                        if p.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                            let name =
                                p.file_name().and_then(|s| s.to_str()).map(|s| s.to_string())?;
                            // skip dot-files / staging artifacts
                            if name.starts_with('.') {
                                return None;
                            }
                            Some((name, p))
                        } else {
                            None
                        }
                    })
                    .collect();
                // Deterministic order: member leaf name
                members.sort_by(|a, b| a.0.cmp(&b.0));
                for (member_name, member_path) in members {
                    out.extend(scan_skill_actions_dir(pkg_id, &member_name, &member_path));
                }
            }
            _ => {
                // Other kinds (agent, command, hook, mcp) carry no skill actions.
                continue;
            }
        }
    }
    out.sort_by(|a, b| a.skill.cmp(&b.skill).then(a.verb.cmp(&b.verb)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_frontmatter_block() {
        let body = "---\nname: send\nux_mode: confirm\n---\n\n# body\n";
        let fm = extract_frontmatter(body).expect("frontmatter");
        assert!(fm.contains("name: send"));
        assert!(fm.contains("ux_mode: confirm"));
        assert!(!fm.contains("# body"));
    }

    #[test]
    fn no_frontmatter_returns_none() {
        assert!(extract_frontmatter("# just a heading\n").is_none());
    }

    #[test]
    fn parses_confirm_action_with_run_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let actions = dir.path().join("pa").join("actions");
        std::fs::create_dir_all(&actions).unwrap();
        let f = actions.join("send.md");
        std::fs::write(
            &f,
            "---\nname: send\ndescription: Dispatch approved items.\ndomain: tasks\nux_mode: confirm\nrun:\n  kind: chat_prompt\n  prompt: |\n    # PA Send Queue\n    Do the thing.\ndepends_on:\n  - skill-core\n---\n\n# action: send\n",
        )
        .unwrap();

        let actions = discover_actions("com.ikenga.skill-pa", dir.path(), ".");
        assert_eq!(actions.len(), 1);
        let a = &actions[0];
        assert_eq!(a.verb, "send");
        assert_eq!(a.ux_mode, "confirm");
        assert_eq!(a.run_kind.as_deref(), Some("chat_prompt"));
        assert!(a
            .prompt_template
            .as_deref()
            .unwrap()
            .contains("PA Send Queue"));
        assert_eq!(a.depends_on, vec!["skill-core".to_string()]);
    }

    #[test]
    fn skips_readme_and_unparseable() {
        let dir = tempfile::tempdir().unwrap();
        let actions = dir.path().join("pa").join("actions");
        std::fs::create_dir_all(&actions).unwrap();
        std::fs::write(actions.join("README.md"), "# readme, no frontmatter\n").unwrap();
        std::fs::write(actions.join("broken.md"), "no frontmatter here\n").unwrap();
        std::fs::write(
            actions.join("ok.md"),
            "---\nname: ok\nux_mode: streaming\n---\nbody\n",
        )
        .unwrap();

        let found = discover_actions("p", dir.path(), ".");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].verb, "ok");
    }

    // ── WP-20: list_actions_for_pkg — bundle + skill requires ─────────────────

    /// Build a minimal store fixture: a `store/bundles/<bundle>/<member>/actions/*.md`
    /// tree (for bundle requires) and/or `store/skills/<name>/actions/*.md` (for
    /// skill requires). Returns the store root path.
    fn make_store_fixture(
        base: &std::path::Path,
        // (bundle_name, [(member_name, [(action_verb, ux_mode)])])
        bundles: &[(&str, &[(&str, &[(&str, &str)])])],
        // (skill_name, [(action_verb, ux_mode)])
        skills: &[(&str, &[(&str, &str)])],
    ) -> std::path::PathBuf {
        let store = base.join("store");
        for (bundle_name, members) in bundles {
            for (member_name, actions) in *members {
                let action_dir = store
                    .join("bundles")
                    .join(bundle_name)
                    .join(member_name)
                    .join("actions");
                std::fs::create_dir_all(&action_dir).unwrap();
                for (verb, ux_mode) in *actions {
                    std::fs::write(
                        action_dir.join(format!("{verb}.md")),
                        format!("---\nname: {verb}\nux_mode: {ux_mode}\n---\nbody\n"),
                    )
                    .unwrap();
                }
            }
        }
        for (skill_name, actions) in skills {
            let action_dir = store.join("skills").join(skill_name).join("actions");
            std::fs::create_dir_all(&action_dir).unwrap();
            for (verb, ux_mode) in *actions {
                std::fs::write(
                    action_dir.join(format!("{verb}.md")),
                    format!("---\nname: {verb}\nux_mode: {ux_mode}\n---\nbody\n"),
                )
                .unwrap();
            }
        }
        store
    }

    fn req(kind: &str, name: &str) -> RequiresEntry {
        RequiresEntry {
            kind: kind.into(),
            name: name.into(),
            source: None,
            r#ref: None,
        }
    }

    #[test]
    fn list_actions_for_pkg_bundle_surfaces_all_member_actions() {
        // (c) A requires:{kind:bundle} pkg returns the UNION of all members' actions.
        let base = tempfile::tempdir().unwrap();
        let store = make_store_fixture(
            base.path(),
            &[(
                "atelier",
                &[
                    ("pa", &[("send", "confirm"), ("review", "streaming")]),
                    ("outbound", &[("publish", "confirm")]),
                ],
            )],
            &[],
        );

        let requires = vec![req("bundle", "atelier")];
        let mut actions = list_actions_for_pkg("com.ikenga.studio", &requires, &store);
        actions.sort_by(|a, b| a.skill.cmp(&b.skill).then(a.verb.cmp(&b.verb)));

        // All 3 actions from the 2 members surfaced.
        assert_eq!(actions.len(), 3, "union of all bundle members' actions");
        // Skills reported as the member leaf names.
        let skills: Vec<&str> = actions.iter().map(|a| a.skill.as_str()).collect();
        assert!(skills.contains(&"pa"));
        assert!(skills.contains(&"outbound"));
        // Verbs match what was written.
        let verbs: Vec<&str> = actions.iter().map(|a| a.verb.as_str()).collect();
        assert!(verbs.contains(&"send"));
        assert!(verbs.contains(&"review"));
        assert!(verbs.contains(&"publish"));
        // ATTRIBUTION is load-bearing: each verb is reported under its OWN member,
        // not lumped onto the first member's name. A bug that mis-attributes the
        // `skill` field would pass the contains-checks above but fail here.
        assert!(actions.iter().any(|a| a.skill == "pa" && a.verb == "send"));
        assert!(actions.iter().any(|a| a.skill == "pa" && a.verb == "review"));
        assert!(actions.iter().any(|a| a.skill == "outbound" && a.verb == "publish"));
    }

    #[test]
    fn list_actions_for_pkg_skill_requires_unchanged_after_wp20() {
        // (d) A requires:{kind:skill} pkg returns the skill's actions — regression.
        let base = tempfile::tempdir().unwrap();
        let store = make_store_fixture(
            base.path(),
            &[],
            &[("groundwork", &[("init", "confirm"), ("status", "streaming")])],
        );

        let requires = vec![req("skill", "groundwork")];
        let actions = list_actions_for_pkg("com.ikenga.x", &requires, &store);
        assert_eq!(actions.len(), 2, "two actions from the skill");
        let verbs: Vec<&str> = actions.iter().map(|a| a.verb.as_str()).collect();
        assert!(verbs.contains(&"init"));
        assert!(verbs.contains(&"status"));
    }

    #[test]
    fn list_actions_for_pkg_mixed_skill_and_bundle() {
        // A pkg with BOTH a skill and a bundle require: union of actions from both.
        let base = tempfile::tempdir().unwrap();
        let store = make_store_fixture(
            base.path(),
            &[("my-bundle", &[("m1", &[("action-a", "confirm")])])],
            &[("my-skill", &[("action-b", "streaming")])],
        );

        let requires = vec![req("bundle", "my-bundle"), req("skill", "my-skill")];
        let mut actions = list_actions_for_pkg("com.ikenga.mixed", &requires, &store);
        actions.sort_by(|a, b| a.verb.cmp(&b.verb));

        assert_eq!(actions.len(), 2);
        let verbs: Vec<&str> = actions.iter().map(|a| a.verb.as_str()).collect();
        assert!(verbs.contains(&"action-a"));
        assert!(verbs.contains(&"action-b"));
    }

    #[test]
    fn list_actions_for_pkg_absent_bundle_contributes_nothing() {
        // A requires:{kind:bundle} where the bundle dir is absent → empty (not an error).
        let base = tempfile::tempdir().unwrap();
        let store = base.path().join("store");
        std::fs::create_dir_all(&store).unwrap();

        let requires = vec![req("bundle", "ghost-bundle")];
        let actions = list_actions_for_pkg("com.ikenga.x", &requires, &store);
        assert!(actions.is_empty(), "absent bundle dir → empty, not error");
    }

    #[test]
    fn list_actions_for_pkg_bundle_member_order_is_deterministic() {
        // Actions from bundle members are returned in (skill-leaf, verb) order regardless
        // of filesystem iteration order.
        let base = tempfile::tempdir().unwrap();
        let store = make_store_fixture(
            base.path(),
            &[(
                "sorted-bundle",
                &[
                    ("zzz", &[("z-verb", "streaming")]),
                    ("aaa", &[("a-verb", "streaming")]),
                    ("mmm", &[("m-verb", "streaming")]),
                ],
            )],
            &[],
        );

        let requires = vec![req("bundle", "sorted-bundle")];
        let actions = list_actions_for_pkg("com.ikenga.x", &requires, &store);
        let skills: Vec<&str> = actions.iter().map(|a| a.skill.as_str()).collect();
        assert_eq!(skills, vec!["aaa", "mmm", "zzz"], "sorted by member leaf name");
    }

    // ── WP-07: strict contract binding ────────────────────────────────────────

    /// Write one action file and parse it directly. Returns None on skip.
    fn parse_one(frontmatter: &str) -> Option<SkillAction> {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.md");
        std::fs::write(&f, format!("---\n{frontmatter}\n---\nbody\n")).unwrap();
        parse_action_file("com.ikenga.x", "s", &f)
    }

    #[test]
    fn missing_ux_mode_is_invalid() {
        // R-01 behavior change: no silent default to streaming.
        assert!(parse_one("name: x\ndomain: mail").is_none());
    }

    #[test]
    fn unknown_ux_mode_is_invalid() {
        assert!(parse_one("name: x\nux_mode: modal").is_none());
    }

    #[test]
    fn unknown_domain_is_invalid() {
        assert!(parse_one("name: x\nux_mode: confirm\ndomain: marketing").is_none());
    }

    #[test]
    fn absent_domain_is_tolerated() {
        let a = parse_one("name: x\nux_mode: confirm").expect("domain-less file still lists");
        assert_eq!(a.domain, None);
    }

    #[test]
    fn parses_triggers_capabilities_and_setup() {
        let a = parse_one(
            "name: setup\ndescription: d\ndomain: skill-core\nux_mode: streaming\n\
             run:\n  kind: chat_prompt\n  prompt: |\n    hi\n\
             triggers:\n  - kind: manual\n  - kind: schedule\n    cron: \"30 */4 * * *\"\n    label: Sweep\n\
             requires_capabilities:\n  - fs\n  - chat\n\
             setup:\n  mode: interview\n  template_version: 2\n  interview_questions:\n    - q1\n    - q2",
        )
        .expect("valid setup action");
        assert_eq!(a.triggers.len(), 2);
        assert_eq!(a.triggers[0].kind, "manual");
        assert_eq!(a.triggers[1].kind, "schedule");
        assert_eq!(a.triggers[1].cron.as_deref(), Some("30 */4 * * *"));
        assert_eq!(a.triggers[1].label.as_deref(), Some("Sweep"));
        assert_eq!(a.requires_capabilities, vec!["fs", "chat"]);
        let setup = a.setup.expect("setup block surfaced");
        assert_eq!(setup.mode, "interview");
        assert_eq!(setup.template_version, 2);
        assert_eq!(setup.interview_questions, vec!["q1", "q2"]);
    }

    #[test]
    fn unknown_top_level_key_warns_but_keeps_file() {
        // Forward-compat: a not-yet-known key doesn't reject the action.
        let a = parse_one("name: x\nux_mode: confirm\ndomain: mail\nfuture_field: 1")
            .expect("unknown key is warned, not fatal");
        assert_eq!(a.name, "x");
    }

    /// R-01 conformance sweep: parse EVERY real action `.md` under the sibling
    /// `ikenga-pkgs` skills tree with the new strict rules. Files without a
    /// frontmatter fence (prose-only groundwork/contribute skill docs) are
    /// legitimately skipped; every file that HAS a fence MUST parse to a valid
    /// `SkillAction`. A single skip here would mean the strict loader bricks an
    /// installed skill.
    #[test]
    fn conformance_all_installed_action_files_parse_valid() {
        let skills_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("ikenga-pkgs")
            .join("packages")
            .join("skills");
        if !skills_root.is_dir() {
            eprintln!(
                "conformance: skills tree not found at {} — skipping (run in the monorepo worktree to exercise)",
                skills_root.display()
            );
            return;
        }

        let mut structured = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for domain in std::fs::read_dir(&skills_root).unwrap().flatten() {
            let skills_dir = domain.path().join("skills");
            if !skills_dir.is_dir() {
                continue;
            }
            for skill in std::fs::read_dir(&skills_dir).unwrap().flatten() {
                let skill_name = skill.file_name().to_string_lossy().to_string();
                let actions_dir = skill.path().join("actions");
                if !actions_dir.is_dir() {
                    continue;
                }
                for entry in std::fs::read_dir(&actions_dir).unwrap().flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("md") {
                        continue;
                    }
                    if p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| n.eq_ignore_ascii_case("README.md"))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let body = std::fs::read_to_string(&p).unwrap();
                    // Only files that actually open a frontmatter fence are
                    // structured actions subject to the schema.
                    if extract_frontmatter(&body).is_none() {
                        continue;
                    }
                    structured += 1;
                    if parse_action_file("com.ikenga.conformance", &skill_name, &p).is_none() {
                        failures.push(p.display().to_string());
                    }
                }
            }
        }

        assert!(
            structured >= 32,
            "expected >= 32 structured action files, found {structured}"
        );
        assert!(
            failures.is_empty(),
            "installed action files failed the strict parse:\n{}",
            failures.join("\n")
        );
    }
}
