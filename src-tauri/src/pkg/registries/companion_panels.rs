//! Companion-panels registry — `ui.companion_panels[]` state panels a package
//! contributes to the Chi-mode Companion (manifest v5, G-MANIFEST-V5 §2).
//!
//! Panels mount as iframe views in the Companion slot. `route` names the
//! iframe view — the spec's "Pane route rendered in the panel slot" — so it
//! must match a declared `ui.routes[]` path (same §2b reference rule as
//! `ui.views[]`; one mount pipeline, no second source-of-truth for
//! source/kind).
//!
//! ADR-021 conformance: panels render STATE only — never model prose, chat
//! bubbles, or scrollback. The manifest flag is recorded here; the checklist
//! enforcement lives in the consuming surface (WP-06/WP-31).
//!
//! `session_scoped` (§8 Q5): when true, the shell threads the read-only
//! `panelScopeSessionId` (the selected Companion session tab) through the
//! AppBridge hostContext — the field exists on `RoyaltiSuiteContext` and is
//! only populated for scoped panels.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

use super::views::{pane_route_for, require_declared_route};

#[derive(Debug, Clone, Serialize)]
pub struct CompanionPanelRegistryEntry {
    pub pkg_id: String,
    /// `${pkg_id}:${panel_id}`.
    pub qualified_id: String,
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    /// Manifest-declared namespace path — equal to a `ui.routes[].path`.
    pub route: String,
    /// `/pkg/<id><route>` — the pane path the panel slot mounts.
    pub pane_route: String,
    /// When true, the AppBridge hostContext carries `panelScopeSessionId`
    /// (read-only).
    pub session_scoped: bool,
}

#[derive(Default)]
pub struct CompanionPanelsRegistry {
    /// Keyed by `pkg_id`; each Vec keeps the manifest's declaration order.
    entries: RwLock<HashMap<String, Vec<CompanionPanelRegistryEntry>>>,
}

impl CompanionPanelsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All entries — pkgs sorted by `pkg_id` (deterministic snapshots),
    /// panels within a pkg in declaration order.
    pub fn list(&self) -> Vec<CompanionPanelRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<CompanionPanelRegistryEntry>)> = self
            .entries
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }
}

impl Registry for CompanionPanelsRegistry {
    fn name(&self) -> &'static str {
        "companion_panels"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let block = match &pkg.manifest.ui {
            Some(b) if !b.companion_panels.is_empty() => b,
            _ => return Ok(()),
        };
        let pkg_id = &pkg.manifest.id;

        let mut new_entries: Vec<CompanionPanelRegistryEntry> =
            Vec::with_capacity(block.companion_panels.len());
        let mut seen_ids: HashSet<&str> = HashSet::with_capacity(block.companion_panels.len());
        for p in &block.companion_panels {
            if !seen_ids.insert(p.id.as_str()) {
                return Err(anyhow!(
                    "`ui.companion_panels` of `{pkg_id}` declares duplicate id `{}`",
                    p.id
                ));
            }
            require_declared_route(block, pkg_id, "companion_panels", &p.id, &p.route)?;
            new_entries.push(CompanionPanelRegistryEntry {
                pkg_id: pkg_id.clone(),
                qualified_id: format!("{pkg_id}:{}", p.id),
                id: p.id.clone(),
                title: p.title.clone(),
                icon: p.icon.clone(),
                route: p.route.clone(),
                pane_route: pane_route_for(pkg_id, &p.route),
                session_scoped: p.session_scoped,
            });
        }

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("companion_panels lock poisoned"))?;
        // Idempotent re-register: replace the pkg's set wholesale.
        // qualified_id is pkg-namespaced so cross-pkg collisions can't exist
        // by construction.
        entries.insert(pkg_id.clone(), new_entries);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("companion_panels lock poisoned"))?;
        entries.remove(pkg_id);
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let entries = self.list();
        json!({ "count": entries.len(), "entries": entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{CompanionPanelEntry, Manifest, Permissions, UiBlock, UiRoute};
    use std::path::PathBuf;

    fn pkg_with_panels(id: &str, routes: &[&str], panels: Vec<CompanionPanelEntry>) -> Package {
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
                    routes: routes
                        .iter()
                        .map(|p| UiRoute {
                            path: (*p).to_string(),
                            kind: "iframe".into(),
                            source: "dist/index.html".into(),
                            partition: None,
                        })
                        .collect(),
                    companion_panels: panels,
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

    fn panel(id: &str, route: &str, session_scoped: bool) -> CompanionPanelEntry {
        CompanionPanelEntry {
            id: id.into(),
            title: id.into(),
            icon: None,
            route: route.into(),
            session_scoped,
        }
    }

    #[test]
    fn registers_panels_and_surfaces_session_scope() {
        let reg = CompanionPanelsRegistry::new();
        let pkg = pkg_with_panels(
            "com.ikenga.agentops",
            &["/status"],
            vec![panel("job-status", "/status", true)],
        );
        reg.register(&pkg).unwrap();
        let entries = reg.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].qualified_id, "com.ikenga.agentops:job-status");
        assert_eq!(entries[0].pane_route, "/pkg/com.ikenga.agentops/status");
        assert!(entries[0].session_scoped);
    }

    #[test]
    fn unknown_route_fails_register() {
        let reg = CompanionPanelsRegistry::new();
        let pkg = pkg_with_panels(
            "com.ikenga.agentops",
            &["/status"],
            vec![panel("nope", "/elsewhere", false)],
        );
        assert!(reg.register(&pkg).is_err());
        assert!(reg.list().is_empty());
    }
}
