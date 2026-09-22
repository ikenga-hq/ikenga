//! Widgets registry — `ui.widgets[]` project-dashboard widgets (manifest v5,
//! G-MANIFEST-V5 §2). Widgets render as iframe views inside the dashboard's
//! fixed grid; `span` (`small` | `medium` | `wide`, default `medium`) is the
//! only layout input — no drag-canvas semantics (§8 Q7).
//!
//! `route` names the iframe view, so it must match a declared `ui.routes[]`
//! path — same §2b reference rule as `ui.views[]` (one mount pipeline).

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::pkg::manifest::{Package, WidgetSpan};
use crate::pkg::registry::Registry;

use super::views::{pane_route_for, require_declared_route};

#[derive(Debug, Clone, Serialize)]
pub struct WidgetRegistryEntry {
    pub pkg_id: String,
    /// `${pkg_id}:${widget_id}`.
    pub qualified_id: String,
    pub id: String,
    pub title: String,
    /// Manifest-declared namespace path — equal to a `ui.routes[].path`.
    pub route: String,
    /// `/pkg/<id><route>` — the pane path the dashboard cell mounts.
    pub pane_route: String,
    /// Fixed-grid span (`small` | `medium` | `wide`).
    pub span: WidgetSpan,
}

#[derive(Default)]
pub struct WidgetsRegistry {
    /// Keyed by `pkg_id`; each Vec keeps the manifest's declaration order.
    entries: RwLock<HashMap<String, Vec<WidgetRegistryEntry>>>,
}

impl WidgetsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All entries — pkgs sorted by `pkg_id` (deterministic snapshots),
    /// widgets within a pkg in declaration order.
    pub fn list(&self) -> Vec<WidgetRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<WidgetRegistryEntry>)> = self
            .entries
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }
}

impl Registry for WidgetsRegistry {
    fn name(&self) -> &'static str {
        "widgets"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let block = match &pkg.manifest.ui {
            Some(b) if !b.widgets.is_empty() => b,
            _ => return Ok(()),
        };
        let pkg_id = &pkg.manifest.id;

        let mut new_entries: Vec<WidgetRegistryEntry> = Vec::with_capacity(block.widgets.len());
        let mut seen_ids: HashSet<&str> = HashSet::with_capacity(block.widgets.len());
        for w in &block.widgets {
            if !seen_ids.insert(w.id.as_str()) {
                return Err(anyhow!(
                    "`ui.widgets` of `{pkg_id}` declares duplicate id `{}`",
                    w.id
                ));
            }
            require_declared_route(block, pkg_id, "widgets", &w.id, &w.route)?;
            new_entries.push(WidgetRegistryEntry {
                pkg_id: pkg_id.clone(),
                qualified_id: format!("{pkg_id}:{}", w.id),
                id: w.id.clone(),
                title: w.title.clone(),
                route: w.route.clone(),
                pane_route: pane_route_for(pkg_id, &w.route),
                span: w.span,
            });
        }

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("widgets lock poisoned"))?;
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
            .map_err(|_| anyhow!("widgets lock poisoned"))?;
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
    use crate::pkg::manifest::{Manifest, Permissions, UiBlock, UiRoute, WidgetEntry};
    use std::path::PathBuf;

    fn pkg_with_widgets(id: &str, routes: &[&str], widgets: Vec<WidgetEntry>) -> Package {
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
                    widgets,
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
            },
            install_path: PathBuf::from("/tmp/_unused"),
        }
    }

    #[test]
    fn registers_widgets_with_span_and_pane_route() {
        let reg = WidgetsRegistry::new();
        let pkg = pkg_with_widgets(
            "com.ikenga.tasks",
            &["/badge"],
            vec![WidgetEntry {
                id: "open-tasks".into(),
                title: "Open tasks".into(),
                route: "/badge".into(),
                span: WidgetSpan::Wide,
            }],
        );
        reg.register(&pkg).unwrap();
        let entries = reg.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].qualified_id, "com.ikenga.tasks:open-tasks");
        assert_eq!(entries[0].span, WidgetSpan::Wide);
        assert_eq!(entries[0].pane_route, "/pkg/com.ikenga.tasks/badge");
        // Snapshot serializes span as the §2 lowercase enum.
        let snap = reg.snapshot();
        assert_eq!(snap["entries"][0]["span"], "wide");
    }

    #[test]
    fn unknown_route_fails_register() {
        let reg = WidgetsRegistry::new();
        let pkg = pkg_with_widgets(
            "com.ikenga.tasks",
            &["/badge"],
            vec![WidgetEntry {
                id: "nope".into(),
                title: "nope".into(),
                route: "/missing".into(),
                span: WidgetSpan::Small,
            }],
        );
        assert!(reg.register(&pkg).is_err());
        assert!(reg.list().is_empty());
    }
}
