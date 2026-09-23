//! Explorer-sections registry — `ui.explorer_sections[]` contributions a
//! package adds to the Project Explorer (manifest v5, G-MANIFEST-V5 §2).
//!
//! A section is *data, never an iframe*: the FE's shared section frame fetches
//! `data_route` (a GET iyke route under `/pkg/<id>/`) and renders the returned
//! `ExplorerSectionData` rows natively. Refresh semantics per §8 Q4:
//! `pkg://lifecycle` event refetch + 60 s stale time.
//!
//! Ordering (§2): `order` is optional; the default is declaration order and
//! cross-pkg ties break by pkg id. This registry records `order` verbatim plus
//! `decl_index` (position within the pkg's block) — consumers compute the
//! effective order as `order.unwrap_or(decl_index)`, then `pkg_id`.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

#[derive(Debug, Clone, Serialize)]
pub struct ExplorerSectionRegistryEntry {
    pub pkg_id: String,
    /// `${pkg_id}:${section_id}` — the `ExplorerSectionState.id` (G-STATE §1).
    pub qualified_id: String,
    /// Pkg-local section id as declared.
    pub id: String,
    pub title: String,
    pub icon: Option<String>,
    /// Declared sort order (`order` in the manifest), or `None` for the
    /// declaration-order default.
    pub order: Option<i64>,
    /// Position of this entry within the pkg's `explorer_sections[]` block —
    /// the default `order` when none is declared.
    pub decl_index: u32,
    /// GET iyke route under `/pkg/<id>/` returning `ExplorerSectionData`.
    pub data_route: String,
}

#[derive(Default)]
pub struct ExplorerSectionsRegistry {
    /// Keyed by `pkg_id`; each Vec keeps the manifest's declaration order so
    /// `decl_index` is recoverable from position.
    entries: RwLock<HashMap<String, Vec<ExplorerSectionRegistryEntry>>>,
}

impl ExplorerSectionsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// All entries — pkgs sorted by `pkg_id` (deterministic snapshots),
    /// sections within a pkg in declaration order. Consumers apply the §2
    /// effective order (`order.unwrap_or(decl_index)`, ties by `pkg_id`).
    pub fn list(&self) -> Vec<ExplorerSectionRegistryEntry> {
        let mut by_pkg: Vec<(String, Vec<ExplorerSectionRegistryEntry>)> = self
            .entries
            .read()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        by_pkg.sort_by(|a, b| a.0.cmp(&b.0));
        by_pkg.into_iter().flat_map(|(_, v)| v).collect()
    }
}

impl Registry for ExplorerSectionsRegistry {
    fn name(&self) -> &'static str {
        "explorer_sections"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let block = match &pkg.manifest.ui {
            Some(b) if !b.explorer_sections.is_empty() => b,
            _ => return Ok(()),
        };
        let pkg_id = &pkg.manifest.id;
        // §2 pins `data_route` as "a GET iyke route under `/pkg/<id>/`" —
        // same namespace containment rule `IykeRoutesRegistry` enforces.
        let expected_prefix = format!("/pkg/{pkg_id}/");
        let bare_prefix = format!("/pkg/{pkg_id}");

        let mut new_entries: Vec<ExplorerSectionRegistryEntry> =
            Vec::with_capacity(block.explorer_sections.len());
        let mut seen_ids: HashSet<&str> = HashSet::with_capacity(block.explorer_sections.len());
        for (idx, s) in block.explorer_sections.iter().enumerate() {
            if !seen_ids.insert(s.id.as_str()) {
                return Err(anyhow!(
                    "`ui.explorer_sections` of `{pkg_id}` declares duplicate id `{}`",
                    s.id
                ));
            }
            if !s.data_route.starts_with(&expected_prefix) && s.data_route != bare_prefix {
                return Err(anyhow!(
                    "`ui.explorer_sections` entry `{}` of `{pkg_id}` declares data_route \
                     `{}` which must be a GET iyke route under `{expected_prefix}`",
                    s.id,
                    s.data_route
                ));
            }
            new_entries.push(ExplorerSectionRegistryEntry {
                pkg_id: pkg_id.clone(),
                qualified_id: format!("{pkg_id}:{}", s.id),
                id: s.id.clone(),
                title: s.title.clone(),
                icon: s.icon.clone(),
                order: s.order,
                decl_index: idx as u32,
                data_route: s.data_route.clone(),
            });
        }

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("explorer_sections lock poisoned"))?;
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
            .map_err(|_| anyhow!("explorer_sections lock poisoned"))?;
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
    use crate::pkg::manifest::{ExplorerSectionEntry, Manifest, Permissions, UiBlock};
    use std::path::PathBuf;

    fn pkg_with_sections(id: &str, sections: Vec<ExplorerSectionEntry>) -> Package {
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
                    explorer_sections: sections,
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

    fn section(id: &str, data_route: &str) -> ExplorerSectionEntry {
        ExplorerSectionEntry {
            id: id.into(),
            title: id.into(),
            icon: None,
            order: None,
            data_route: data_route.into(),
        }
    }

    #[test]
    fn registers_sections_with_qualified_ids_and_decl_index() {
        let reg = ExplorerSectionsRegistry::new();
        let pkg = pkg_with_sections(
            "com.ikenga.git",
            vec![
                section("branches", "/pkg/com.ikenga.git/sections/branches"),
                section("prs", "/pkg/com.ikenga.git/sections/prs"),
            ],
        );
        reg.register(&pkg).unwrap();

        let entries = reg.list();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].qualified_id, "com.ikenga.git:branches");
        assert_eq!(entries[0].decl_index, 0);
        assert_eq!(entries[1].decl_index, 1);
        assert_eq!(entries[0].order, None);
    }

    #[test]
    fn data_route_outside_pkg_namespace_rejected() {
        let reg = ExplorerSectionsRegistry::new();
        let pkg = pkg_with_sections(
            "com.ikenga.git",
            vec![section("evil", "/pkg/com.ikenga.other/x")],
        );
        let err = reg.register(&pkg).unwrap_err();
        assert!(err.to_string().contains("/pkg/com.ikenga.git/"), "{err}");
        assert!(reg.list().is_empty());
    }

    #[test]
    fn unregister_clears_only_that_pkg() {
        let reg = ExplorerSectionsRegistry::new();
        reg.register(&pkg_with_sections(
            "com.ikenga.git",
            vec![section("a", "/pkg/com.ikenga.git/a")],
        ))
        .unwrap();
        reg.register(&pkg_with_sections(
            "com.ikenga.mail",
            vec![section("b", "/pkg/com.ikenga.mail/b")],
        ))
        .unwrap();
        reg.unregister("com.ikenga.git").unwrap();
        let entries = reg.list();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pkg_id, "com.ikenga.mail");
    }
}
