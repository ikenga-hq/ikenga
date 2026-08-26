//! Activity-bar registry — entries a package contributes to the shell's
//! left-most activity bar.
//!
//! Surfaced from `manifest.ui.nav[0]` (the first nav entry per pkg, by
//! convention the "open this app" affordance). The frontend reads the kernel
//! snapshot and renders one icon per pkg alongside the built-in
//! activity-bar items. Click navigates the focused pane to the entry's route.
//!
//! v1 scope: one entry per pkg. Additional `ui.nav[]` items beyond [0] are
//! reserved for the in-shell pkg sidebar (Phase 2 — runtime menu protocol).
//! We don't render them here.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

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
    pub id: String,
    pub label: String,
    pub icon: Option<String>,
    pub section: Option<String>,
    pub route: String,
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
    /// e.g. it hasn't registered a `ui.nav[0]`, or was never installed —
    /// since a pkg racing its own badge push against boot/reload is a
    /// normal transient, not a fault.
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
        // Read the first manifest.ui.nav entry, if any. Pkgs without nav
        // entries don't appear in the activity bar — they can still be
        // launched via /pkg/<id>/ deep link or the Packages mode.
        let block = match &pkg.manifest.ui {
            Some(b) => b,
            None => return Ok(()),
        };
        let nav = match block.nav.first() {
            Some(n) => n,
            None => return Ok(()),
        };

        let entry = ActivityBarEntry {
            pkg_id: pkg.manifest.id.clone(),
            id: nav.id.clone(),
            label: nav.label.clone(),
            icon: nav.icon.clone(),
            section: nav.section.clone(),
            route: nav.route.clone(),
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
