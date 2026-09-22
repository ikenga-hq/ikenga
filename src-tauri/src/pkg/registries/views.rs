//! Views registry — `ui.views[]` entry points a package contributes to the
//! shell (manifest v5, G-MANIFEST-V5 §2).
//!
//! Views are the pkg's *entry-point list*: the Explorer **Views** section
//! renders them, `views[0]` is the rail claim the activity bar/pins consume,
//! and `pin_on_install` is honoured once at first install via
//! `activityPinsAdd`. The mount table stays `ui.routes[]` — a `ViewEntry`
//! carries no source/kind of its own.
//!
//! Reference rule (§2b): every `views[].route` must equal a path declared in
//! the same manifest's `ui.routes[]`. Validated here at `register()` and the
//! pkg fails loudly on a miss — same class as the sidecar-name prefix rule.
//! Alias note: `ui.nav[]` entries arrive already normalized to namespace
//! paths by `Package::load` (the alias maps `/pkg/<id>/x` → `/x`), so they
//! satisfy this check when the nav entry pointed at a declared route.
//!
//! Ordering: entries are stored per-pkg in manifest-declaration order —
//! `views[0]` must mean the pkg's first declared view (the rail claim), so
//! the snapshot preserves it rather than re-sorting.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::pkg::manifest::{Package, UiBlock};
use crate::pkg::registry::Registry;

/// One registered view. `qualified_id` (`${pkg_id}:${view_id}`) is the
/// namespaced identity convention shared with explorer sections / context
/// actions (G-MANIFEST-V5 §8 Q6).
#[derive(Debug, Clone, Serialize)]
pub struct ViewRegistryEntry {
    pub pkg_id: String,
    /// Package display name — rail-label fallback for consumers.
    pub pkg_name: String,
    /// `${pkg_id}:${view_id}` — globally unique, collision-free by
    /// construction (the pkg prefix namespaces the local id).
    pub qualified_id: String,
    /// The pkg-local view id as declared in the manifest.
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    /// Manifest-declared namespace path — equal to a `ui.routes[].path`.
    pub route: String,
    /// `/pkg/<id><route>` — the pane path the pane store and pins navigate.
    pub pane_route: String,
    pub pin_on_install: bool,
}

#[derive(Default)]
pub struct ViewsRegistry {
    /// Keyed by `pkg_id`; each Vec keeps the manifest's declaration order so
    /// `entries[0]` for a pkg is always its declared `views[0]` rail claim.
    entries: RwLock<HashMap<String, Vec<ViewRegistryEntry>>>,
}

/// Shared §2b check: `route` must equal one of the pkg's declared
/// `ui.routes[].path` values. Used by every registry whose manifest field
/// names an iframe-mountable route (views, companion_panels, widgets).
pub(crate) fn require_declared_route(
    block: &UiBlock,
    pkg_id: &str,
    field: &str,
    entry_id: &str,
    route: &str,
) -> Result<()> {
    let declared: HashSet<&str> = block.routes.iter().map(|r| r.path.as_str()).collect();
    if !declared.contains(route) {
        return Err(anyhow!(
            "`ui.{field}` entry `{entry_id}` of `{pkg_id}` declares route `{route}` \
             which is not in `ui.routes[]` (G-MANIFEST-V5 §2b — declare the path in \
             `ui.routes[]` first)"
        ));
    }
    Ok(())
}

/// `/pkg/<id><route>` — the pane path for a declared namespace route.
pub(crate) fn pane_route_for(pkg_id: &str, route: &str) -> String {
    format!("/pkg/{pkg_id}{route}")
}

impl ViewsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All entries — pkgs sorted by `pkg_id` (deterministic snapshots), views
    /// within a pkg in declaration order so `views[0]` semantics survive.
    pub fn list(&self) -> Vec<ViewRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<ViewRegistryEntry>)> = self
            .entries
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }
}

impl Registry for ViewsRegistry {
    fn name(&self) -> &'static str {
        "views"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let block = match &pkg.manifest.ui {
            Some(b) if !b.views.is_empty() => b,
            _ => return Ok(()),
        };
        let pkg_id = &pkg.manifest.id;

        // Build + validate first so a bad manifest applies nothing (atomic).
        let mut new_entries: Vec<ViewRegistryEntry> = Vec::with_capacity(block.views.len());
        let mut seen_ids: HashSet<&str> = HashSet::with_capacity(block.views.len());
        for v in &block.views {
            if !seen_ids.insert(v.id.as_str()) {
                return Err(anyhow!(
                    "`ui.views` of `{pkg_id}` declares duplicate id `{}`",
                    v.id
                ));
            }
            require_declared_route(block, pkg_id, "views", &v.id, &v.route)?;
            new_entries.push(ViewRegistryEntry {
                pkg_id: pkg_id.clone(),
                pkg_name: pkg.manifest.name.clone(),
                qualified_id: format!("{pkg_id}:{}", v.id),
                id: v.id.clone(),
                title: v.title.clone(),
                icon: v.icon.clone(),
                route: v.route.clone(),
                pane_route: pane_route_for(pkg_id, &v.route),
                pin_on_install: v.pin_on_install,
            });
        }

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("views lock poisoned"))?;
        // Idempotent re-register: replace the pkg's set wholesale so a removed
        // view in a new manifest version doesn't linger. qualified_id is
        // pkg-namespaced so cross-pkg collisions can't exist by construction.
        entries.insert(pkg_id.clone(), new_entries);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("views lock poisoned"))?;
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
    use crate::pkg::manifest::{Manifest, Permissions, UiBlock, UiRoute, ViewEntry};
    use std::path::PathBuf;

    fn pkg_with_views(id: &str, routes: &[&str], views: Vec<ViewEntry>) -> Package {
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
                    views,
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

    fn view(id: &str, route: &str, pin_on_install: bool) -> ViewEntry {
        ViewEntry {
            id: id.into(),
            title: id.into(),
            icon: None,
            route: route.into(),
            pin_on_install,
        }
    }

    #[test]
    fn registers_views_and_surfaces_snapshot_in_declaration_order() {
        let reg = ViewsRegistry::new();
        // Declaration order is NOT qualified-id order — 'changes' sorts after
        // 'branches' alphabetically, so this proves declaration order wins.
        let pkg = pkg_with_views(
            "com.ikenga.git",
            &["/", "/history", "/branches"],
            vec![
                view("git.changes", "/", true),
                view("git.history", "/history", false),
                view("git.branches", "/branches", false),
            ],
        );
        reg.register(&pkg).unwrap();

        let entries = reg.list();
        assert_eq!(entries.len(), 3);
        let first = &entries[0];
        assert_eq!(first.qualified_id, "com.ikenga.git:git.changes");
        assert_eq!(first.route, "/");
        assert_eq!(first.pane_route, "/pkg/com.ikenga.git/");
        assert!(first.pin_on_install);
        assert!(!entries[1].pin_on_install);

        let snap = reg.snapshot();
        assert_eq!(snap["count"], 3);
        assert_eq!(snap["entries"][0]["id"], "git.changes");
    }

    #[test]
    fn unknown_route_fails_register_loudly() {
        let reg = ViewsRegistry::new();
        let pkg = pkg_with_views(
            "com.ikenga.git",
            &["/"],
            vec![view("git.nope", "/not-a-route", false)],
        );
        let err = reg.register(&pkg).unwrap_err();
        assert!(
            err.to_string().contains("not in `ui.routes[]`"),
            "error must name the §2b violation: {err}"
        );
        // Atomic: the bad pkg left nothing behind.
        assert!(reg.list().is_empty());
    }

    #[test]
    fn duplicate_view_ids_rejected() {
        let reg = ViewsRegistry::new();
        let pkg = pkg_with_views(
            "com.ikenga.git",
            &["/"],
            vec![view("same", "/", false), view("same", "/", false)],
        );
        assert!(reg.register(&pkg).is_err());
    }

    #[test]
    fn re_register_replaces_and_unregister_clears() {
        let reg = ViewsRegistry::new();
        reg.register(&pkg_with_views(
            "com.ikenga.git",
            &["/", "/history"],
            vec![view("a", "/", false), view("b", "/history", false)],
        ))
        .unwrap();
        // New manifest drops view b.
        reg.register(&pkg_with_views(
            "com.ikenga.git",
            &["/", "/history"],
            vec![view("a", "/", false)],
        ))
        .unwrap();
        assert_eq!(reg.list().len(), 1);

        reg.unregister("com.ikenga.git").unwrap();
        assert!(reg.list().is_empty());
        // Unregister is a no-op on absent pkgs.
        reg.unregister("com.ikenga.never").unwrap();
    }
}
