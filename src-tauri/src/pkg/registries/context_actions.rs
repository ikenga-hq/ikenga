//! Context-actions registry — `ui.context_actions[]` selector-scoped menu
//! contributions (manifest v5, G-MANIFEST-V5 §2).
//!
//! Action identity is `${pkg_id}:${action_id}` (§8 Q6) — namespaced like
//! explorer sections so Phase 6's `actions.json` override model can target
//! individual contributions. Phase 4 appends these entries to the WP-04
//! exported ordered menu arrays; the `when` evaluator / loader land in
//! Phase 6, so this registry is record-only: `when`/`run` payloads are
//! surfaced verbatim for the consumers to interpret.
//!
//! No `routes[]` reference check applies here: `run.view.route` may name a
//! shell route (e.g. `/settings`) or a pkg route — unlike views/panels/
//! widgets, a context action is a *navigation*, not an iframe mount claim.
//!
//! The same registry also projects `ui.command_palette[]` (G-ACTIONS §12,
//! G-70; WP-52): each entry is a package action `${pkg_id}:${id}` placed in
//! the `palette` menu, its `shortcut` a DEC-54 key request. The two lists
//! share one id space per manifest — a duplicate id across
//! `context_actions[]` and `command_palette[]` fails the pkg here (§12). The
//! palette entries ride the same `pkg_kernel_status` snapshot under
//! `command_palette` (additive: `count` / `entries` are unchanged), so the
//! frontend's effective model (`src/lib/actions/registry.ts`) reads both
//! without a new command.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::pkg::manifest::{ContextActionRun, ContextSelector, Package};
use crate::pkg::registry::Registry;

#[derive(Debug, Clone, Serialize)]
pub struct ContextActionRegistryEntry {
    pub pkg_id: String,
    /// `${pkg_id}:${action_id}` — the kernel identity (G-MANIFEST-V5 §8 Q6).
    pub qualified_id: String,
    /// Pkg-local action id as declared.
    pub id: String,
    pub label: String,
    /// Selector clause — `{kind: file|artifact|session|ngwa-item, ...}`.
    pub when: ContextSelector,
    /// Effect — `{kind: dispatch, prompt, target?}` or `{kind: view, route}`.
    pub run: ContextActionRun,
    /// DEC-54 key request (G-ACTIONS §7.1), verbatim from the manifest.
    /// `None` when the pkg made no request. WP-52's effective-keymap merge
    /// derives the request's `when` and decides whether it is granted.
    pub key: Option<String>,
}

/// One `ui.command_palette[]` entry (G-ACTIONS §12), projected verbatim.
#[derive(Debug, Clone, Serialize)]
pub struct CommandPaletteRegistryEntry {
    pub pkg_id: String,
    /// `${pkg_id}:${id}` — the same action id space as context actions.
    pub qualified_id: String,
    /// Pkg-local id as declared.
    pub id: String,
    pub label: String,
    /// DEC-54 key request (single stroke), verbatim. Its derived `when` is
    /// `!inputFocus` (§12); WP-52's merge decides whether it is granted.
    pub shortcut: Option<String>,
    /// `{kind: dispatch, prompt, target?}` or `{kind: view, route}`.
    pub action: ContextActionRun,
}

#[derive(Default)]
pub struct ContextActionsRegistry {
    /// Keyed by `pkg_id`; each Vec keeps the manifest's declaration order.
    entries: RwLock<HashMap<String, Vec<ContextActionRegistryEntry>>>,
    /// `ui.command_palette[]`, keyed by `pkg_id`, declaration order.
    palette: RwLock<HashMap<String, Vec<CommandPaletteRegistryEntry>>>,
}

impl ContextActionsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All entries — pkgs sorted by `pkg_id` (deterministic snapshots),
    /// actions within a pkg in declaration order.
    pub fn list(&self) -> Vec<ContextActionRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<ContextActionRegistryEntry>)> = self
            .entries
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }

    /// All `command_palette[]` entries — pkgs sorted by `pkg_id`, entries
    /// within a pkg in declaration order.
    pub fn list_palette(&self) -> Vec<CommandPaletteRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<CommandPaletteRegistryEntry>)> = self
            .palette
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }
}

impl Registry for ContextActionsRegistry {
    fn name(&self) -> &'static str {
        "context_actions"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let block = match &pkg.manifest.ui {
            Some(b) if !b.context_actions.is_empty() || !b.command_palette.is_empty() => b,
            _ => return Ok(()),
        };
        let pkg_id = &pkg.manifest.id;

        // One id space across both lists (G-ACTIONS §12): validate everything
        // before touching either map, so a failure leaves no partial state.
        let mut seen_ids: HashSet<&str> =
            HashSet::with_capacity(block.context_actions.len() + block.command_palette.len());
        let mut new_entries: Vec<ContextActionRegistryEntry> =
            Vec::with_capacity(block.context_actions.len());
        for a in &block.context_actions {
            if !seen_ids.insert(a.id.as_str()) {
                return Err(anyhow!(
                    "`ui.context_actions` of `{pkg_id}` declares duplicate id `{}`",
                    a.id
                ));
            }
            new_entries.push(ContextActionRegistryEntry {
                pkg_id: pkg_id.clone(),
                qualified_id: format!("{pkg_id}:{}", a.id),
                id: a.id.clone(),
                label: a.label.clone(),
                when: a.when.clone(),
                run: a.run.clone(),
                key: a.key.clone(),
            });
        }
        let mut new_palette: Vec<CommandPaletteRegistryEntry> =
            Vec::with_capacity(block.command_palette.len());
        for p in &block.command_palette {
            if !seen_ids.insert(p.id.as_str()) {
                return Err(anyhow!(
                    "`ui.command_palette` of `{pkg_id}` declares id `{}`, already used by \
                     another `ui.context_actions[]` / `ui.command_palette[]` entry",
                    p.id
                ));
            }
            new_palette.push(CommandPaletteRegistryEntry {
                pkg_id: pkg_id.clone(),
                qualified_id: format!("{pkg_id}:{}", p.id),
                id: p.id.clone(),
                label: p.label.clone(),
                shortcut: p.shortcut.clone(),
                action: p.action.clone(),
            });
        }

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("context_actions lock poisoned"))?;
        let mut palette = self
            .palette
            .write()
            .map_err(|_| anyhow!("context_actions palette lock poisoned"))?;
        // Idempotent re-register: replace the pkg's sets wholesale.
        // qualified_id is pkg-namespaced so cross-pkg collisions can't exist
        // by construction.
        if new_entries.is_empty() {
            entries.remove(pkg_id);
        } else {
            entries.insert(pkg_id.clone(), new_entries);
        }
        if new_palette.is_empty() {
            palette.remove(pkg_id);
        } else {
            palette.insert(pkg_id.clone(), new_palette);
        }
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("context_actions lock poisoned"))?;
        entries.remove(pkg_id);
        drop(entries);
        let mut palette = self
            .palette
            .write()
            .map_err(|_| anyhow!("context_actions palette lock poisoned"))?;
        palette.remove(pkg_id);
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let entries = self.list();
        let palette = self.list_palette();
        json!({
            "count": entries.len(),
            "entries": entries,
            "command_palette": { "count": palette.len(), "entries": palette },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{
        CommandPaletteEntry, ContextActionEntry, Manifest, Permissions, UiBlock,
    };
    use std::path::PathBuf;

    fn pkg_with_actions(id: &str, actions: Vec<ContextActionEntry>) -> Package {
        pkg_with_ui(id, actions, vec![])
    }

    fn pkg_with_ui(
        id: &str,
        actions: Vec<ContextActionEntry>,
        palette: Vec<CommandPaletteEntry>,
    ) -> Package {
        Package {
            manifest: Manifest {
                description: None,
                _comment: None,
                id: id.into(),
                name: id.into(),
                version: "0.1.0".into(),
                ikenga_api: "5".into(),
                kind: None,
                auth_bridge: None,
                author: None,
                targets: vec![],
                mcp: vec![],
                sidecars: vec![],
                permissions: Permissions::default(),
                migrations: None,
                settings: None,
                ui: Some(UiBlock {
                    context_actions: actions,
                    command_palette: palette,
                    ..UiBlock::default()
                }),
                iyke: None,
                cron: vec![],
                window: None,
                queries: None,
                capabilities: None,
                engine: None,
                screenshots: vec![],
                requires: vec![],
                signature: None,
                workflows: vec![],
            },
            install_path: PathBuf::from("/tmp/_unused"),
        }
    }

    #[test]
    fn registers_actions_with_namespaced_ids_and_verbatim_payloads() {
        let reg = ContextActionsRegistry::new();
        let pkg = pkg_with_actions(
            "com.ikenga.git",
            vec![ContextActionEntry {
                id: "blame".into(),
                label: "Blame in Git".into(),
                when: ContextSelector::File {
                    glob: Some("**/*.rs".into()),
                },
                run: ContextActionRun::Dispatch {
                    prompt: "Blame {{file.path}}".into(),
                    target: None,
                },
                key: None,
            }],
        );
        reg.register(&pkg).unwrap();

        let entries = reg.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].qualified_id, "com.ikenga.git:blame");
        // The snapshot serializes when/run in the §2 tagged-union wire shape.
        let snap = reg.snapshot();
        let when = &snap["entries"][0]["when"];
        assert_eq!(when["kind"], "file");
        assert_eq!(when["glob"], "**/*.rs");
        let run = &snap["entries"][0]["run"];
        assert_eq!(run["kind"], "dispatch");
        assert_eq!(run["prompt"], "Blame {{file.path}}");
    }

    #[test]
    fn ngwa_item_selector_and_view_run_round_trip() {
        let reg = ContextActionsRegistry::new();
        let pkg = pkg_with_actions(
            "com.ikenga.tasks",
            vec![ContextActionEntry {
                id: "open-item".into(),
                label: "Open item".into(),
                when: ContextSelector::NgwaItem {
                    kinds: Some(vec!["task".into()]),
                },
                run: ContextActionRun::View {
                    route: "/pkg/com.ikenga.tasks/".into(),
                },
                key: None,
            }],
        );
        reg.register(&pkg).unwrap();
        let snap = reg.snapshot();
        assert_eq!(snap["entries"][0]["when"]["kind"], "ngwa-item");
        assert_eq!(snap["entries"][0]["run"]["kind"], "view");
    }

    fn palette_entry(id: &str, shortcut: Option<&str>) -> CommandPaletteEntry {
        CommandPaletteEntry {
            id: id.into(),
            label: format!("Palette {id}"),
            shortcut: shortcut.map(Into::into),
            action: ContextActionRun::View {
                route: "/settings".into(),
            },
        }
    }

    /// G-ACTIONS §12 (WP-52 hand-off): `ui.command_palette[]` is projected
    /// under `command_palette` in the snapshot, with its `shortcut` request.
    #[test]
    fn projects_command_palette_entries_with_shortcut() {
        let reg = ContextActionsRegistry::new();
        let pkg = pkg_with_ui(
            "com.ikenga.git",
            vec![],
            vec![palette_entry("sync", Some("mod+shift+y"))],
        );
        reg.register(&pkg).unwrap();

        let palette = reg.list_palette();
        assert_eq!(palette.len(), 1);
        assert_eq!(palette[0].qualified_id, "com.ikenga.git:sync");
        let snap = reg.snapshot();
        assert_eq!(snap["count"], 0);
        assert_eq!(snap["command_palette"]["count"], 1);
        let entry = &snap["command_palette"]["entries"][0];
        assert_eq!(entry["shortcut"], "mod+shift+y");
        assert_eq!(entry["action"]["kind"], "view");

        reg.unregister("com.ikenga.git").unwrap();
        assert!(reg.list_palette().is_empty());
    }

    /// §12: ids are unique across `context_actions[]` and `command_palette[]`
    /// of one manifest; a duplicate fails `register()` and records nothing.
    #[test]
    fn duplicate_id_across_context_actions_and_palette_fails_register() {
        let reg = ContextActionsRegistry::new();
        let pkg = pkg_with_ui(
            "com.ikenga.git",
            vec![ContextActionEntry {
                id: "sync".into(),
                label: "Sync".into(),
                when: ContextSelector::Session,
                run: ContextActionRun::View {
                    route: "/settings".into(),
                },
                key: None,
            }],
            vec![palette_entry("sync", None)],
        );
        assert!(reg.register(&pkg).is_err());
        assert!(reg.list().is_empty());
        assert!(reg.list_palette().is_empty());
    }
}
