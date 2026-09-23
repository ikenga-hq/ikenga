//! Activity-bar registry — entries a package contributes to the shell's
//! left-most activity bar.
//!
//! Surfaced from `manifest.ui.views[0]` (manifest v5; during the `ui.nav`
//! alias window `Package::load` has already mapped `nav[i]` → `views[i]`,
//! so nav-only manifests land here identically — G-MANIFEST-V5 §4). The
//! frontend reads the kernel snapshot and renders one icon per pkg alongside
//! the built-in activity-bar items. Click navigates the focused pane to the
//! entry's route.
//!
//! v1 scope: one entry per pkg. Additional `ui.views[]` items beyond [0] are
//! surfaced in `nav` (mapped to the NavEntry wire shape) for the pkg sidebar
//! / Explorer Views consumers. We don't render them here.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::pkg::manifest::{NavEntry, Package};
use crate::pkg::registry::Registry;

use super::views::pane_route_for;

/// Status badge a pkg can push onto its own activity-bar icon (and, per
/// WP-11, the project switcher) — e.g. the git pkg's dirty/ahead-behind dot.
/// Not manifest-declared: it's set at runtime, after registration, via
/// `ActivityBarRegistry::set_badge`, cleared by passing `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityBarBadge {
    /// True to render a plain attention dot (e.g. "repo is dirty").
    #[serde(default)]
    pub dot: bool,
    /// Optional small integer count (e.g. ahead+behind); rendered instead of
    /// / alongside the dot when present. Kept as a string so callers aren't
    /// forced through a numeric round-trip for things like "3↑2↓".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Short human tooltip explaining the badge (e.g. "3 files changed").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tooltip: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityBarEntry {
    pub pkg_id: String,
    /// Package display name. Used as the rail label when `ui.nav[0].section`
    /// is absent, so multi-view packages are identifiable by their own name.
    pub pkg_name: String,
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub section: Option<String>,
    pub route: String,
    /// Full manifest `ui.nav` list, surfaced in the pkg-mode sidebar so the
    /// pkg's views can render with a group heading even before the iframe
    /// publishes a runtime menu.
    pub nav: Vec<NavEntry>,
    /// Runtime-set status badge; absent until a pkg calls `host.pkg.setBadge`
    /// (forwarded to `ActivityBarRegistry::set_badge`). Reset to `None` on
    /// every `register()` (fresh install / dev-reload) so a stale badge from
    /// a previous manifest version never survives a reload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<ActivityBarBadge>,
}

#[derive(Default)]
pub struct ActivityBarRegistry {
    /// Keyed by pkg_id (one entry per pkg).
    entries: RwLock<HashMap<String, ActivityBarEntry>>,
}

impl ActivityBarRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<ActivityBarEntry> {
        self.entries
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Set (or clear, with `None`) the badge on a pkg's activity-bar entry.
    /// Returns `Ok(false)` (not an error) when the pkg has no rail entry —
    /// e.g. it hasn't registered a `ui.views[0]` (or `ui.nav[0]`, via the
    /// v5 alias), or was never installed — since a pkg racing its own badge
    /// push against boot/reload is a normal transient, not a fault.
    pub fn set_badge(&self, pkg_id: &str, badge: Option<ActivityBarBadge>) -> Result<bool> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("activity_bar lock poisoned"))?;
        match entries.get_mut(pkg_id) {
            Some(entry) => {
                entry.badge = badge;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

impl Registry for ActivityBarRegistry {
    fn name(&self) -> &'static str {
        "activity_bar"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        // Read the first manifest.ui.views entry, if any. Post-alias (v5),
        // `views` is populated for nav-only manifests too, so this is the
        // "views[0], fallback nav[0]" claim from G-MANIFEST-V5 §4 — the nav
        // field itself is never read here. Pkgs without views don't appear
        // in the activity bar — they can still be launched via /pkg/<id>/
        // deep link or the Packages mode.
        let block = match &pkg.manifest.ui {
            Some(b) => b,
            None => return Ok(()),
        };
        let first = match block.views.first() {
            Some(v) => v,
            None => return Ok(()),
        };

        // Rail label: the package's own display name. `views[]` carries no
        // section grouping (that was a nav-only field), so the pkg name keeps
        // a multi-view pkg identifiable in the activity bar instead of being
        // renamed after its first view (e.g. "Git" rather than "Changes").
        let pkg_name = pkg.manifest.name.clone();
        let pkg_id = pkg.manifest.id.clone();
        // `nav` on the entry keeps its NavEntry wire shape — the pkg-mode
        // sidebar and the WP-22 pin seed read `nav[i].label`/`route`. Views
        // map onto it with `label = title`, `section = None`, and `route`
        // normalized to the pane path form the pane store navigates.
        let nav: Vec<NavEntry> = block
            .views
            .iter()
            .map(|v| NavEntry {
                id: v.id.clone(),
                label: v.title.clone(),
                icon: v.icon.clone(),
                section: None,
                route: pane_route_for(&pkg_id, &v.route),
            })
            .collect();

        let entry = ActivityBarEntry {
            pkg_id,
            pkg_name: pkg_name.clone(),
            id: first.id.clone(),
            label: pkg_name,
            icon: first.icon.clone(),
            section: None,
            route: pane_route_for(&pkg.manifest.id, &first.route),
            nav,
            badge: None,
        };

        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("activity_bar lock poisoned"))?;
        entries.insert(pkg.manifest.id.clone(), entry);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("activity_bar lock poisoned"))?;
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
    use crate::pkg::manifest::{Manifest, Permissions, UiBlock, ViewEntry};
    use std::path::PathBuf;

    /// A pkg with exactly one `ui.views` entry, so it earns an activity-bar
    /// row. `with_view = false` produces a pkg with no `ui` block at all —
    /// the "never registered a rail entry" case `set_badge` must tolerate.
    /// Note the fixture declares `views` directly: registries only ever see
    /// manifests post-`Package::load`, where the nav→views alias has already
    /// run — so the registry never reads `nav` itself.
    fn pkg_with(id: &str, with_view: bool) -> Package {
        let manifest = Manifest {
            description: None,
            _comment: None,
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            ikenga_api: "1".into(),
            kind: None,
            auth_bridge: None,
            author: None,
            targets: vec![],
            mcp: vec![],
            sidecars: vec![],
            permissions: Permissions::default(),
            migrations: None,
            settings: None,
            ui: with_view.then(|| UiBlock {
                views: vec![ViewEntry {
                    id: "open".into(),
                    title: "Git".into(),
                    icon: None,
                    route: "/".into(),
                    pin_on_install: true,
                }],
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
        };
        Package {
            manifest,
            install_path: PathBuf::from("/tmp/_unused"),
        }
    }

    fn badge_of(reg: &ActivityBarRegistry, pkg_id: &str) -> Option<ActivityBarBadge> {
        reg.list()
            .into_iter()
            .find(|e| e.pkg_id == pkg_id)
            .and_then(|e| e.badge)
    }

    #[test]
    fn set_badge_sets_then_clears() {
        let reg = ActivityBarRegistry::new();
        reg.register(&pkg_with("com.ikenga.git", true)).unwrap();

        // Fresh registration starts with no badge.
        assert_eq!(badge_of(&reg, "com.ikenga.git"), None);

        let badge = ActivityBarBadge {
            dot: true,
            count: Some(3),
            tooltip: Some("3 files changed".into()),
        };
        assert!(reg
            .set_badge("com.ikenga.git", Some(badge.clone()))
            .unwrap());
        assert_eq!(badge_of(&reg, "com.ikenga.git"), Some(badge));

        // Clearing is `Some(pkg)` + `None` badge, still a hit.
        assert!(reg.set_badge("com.ikenga.git", None).unwrap());
        assert_eq!(badge_of(&reg, "com.ikenga.git"), None);
    }

    #[test]
    fn set_badge_on_unknown_pkg_is_ok_false() {
        let reg = ActivityBarRegistry::new();

        // Never installed at all.
        assert!(!reg.set_badge("com.ikenga.nope", None).unwrap());

        // Installed, but contributed no `ui.views[0]` → no rail entry to badge.
        reg.register(&pkg_with("com.ikenga.headless", false))
            .unwrap();
        assert!(!reg
            .set_badge(
                "com.ikenga.headless",
                Some(ActivityBarBadge {
                    dot: true,
                    count: None,
                    tooltip: None,
                }),
            )
            .unwrap());
    }

    #[test]
    fn rail_label_is_pkg_name_and_route_is_pane_path() {
        // v5: `views[]` has no section grouping, so the rail label is the
        // package's own display name, not the first view's title (e.g.
        // "Git", not "Changes"). `route` is the pane path the pane store
        // navigates (`/pkg/<id><route>`).
        let reg = ActivityBarRegistry::new();
        reg.register(&pkg_with("com.ikenga.git", true)).unwrap();
        let entry = reg
            .list()
            .into_iter()
            .find(|e| e.pkg_id == "com.ikenga.git")
            .unwrap();
        assert_eq!(entry.label, "com.ikenga.git");
        assert_eq!(entry.pkg_name, "com.ikenga.git");
        assert_eq!(entry.route, "/pkg/com.ikenga.git/");
        assert_eq!(entry.nav.len(), 1);
        // `nav` keeps the NavEntry wire shape, mapped from views:
        // label = view title, route = pane path.
        assert_eq!(entry.nav[0].label, "Git");
        assert_eq!(entry.nav[0].route, "/pkg/com.ikenga.git/");
    }

    #[test]
    fn re_register_resets_the_badge() {
        let reg = ActivityBarRegistry::new();
        reg.register(&pkg_with("com.ikenga.git", true)).unwrap();
        reg.set_badge(
            "com.ikenga.git",
            Some(ActivityBarBadge {
                dot: true,
                count: None,
                tooltip: None,
            }),
        )
        .unwrap();
        assert!(badge_of(&reg, "com.ikenga.git").is_some());

        // A dev-reload / reinstall re-registers over the same key. The stale
        // badge from the previous manifest version must not survive.
        reg.register(&pkg_with("com.ikenga.git", true)).unwrap();
        assert_eq!(badge_of(&reg, "com.ikenga.git"), None);
    }
}
