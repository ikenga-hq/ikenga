//! Queries registry — TanStack Query key-prefix collision detection.
//!
//! Each package may declare `queries.key_prefixes` in its manifest — the set
//! of top-level query keys it owns. The kernel rejects an install whose
//! prefixes overlap any other installed package's prefixes (exact match or
//! one is a dotted-prefix of the other), so two packages can't silently
//! invalidate each other's caches by colliding on a key like `["tasks"]`.
//!
//! State is in-memory only: a `pkg_id → prefixes` map. No DB row — boot
//! replay re-runs `register` so the map rebuilds from `pkg_installed.manifest_json`.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

pub struct QueriesRegistry {
    by_pkg: RwLock<HashMap<String, Vec<String>>>,
}

impl QueriesRegistry {
    pub fn new() -> Self {
        Self {
            by_pkg: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for QueriesRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn collides(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Dotted-prefix overlap: "tasks" vs "tasks.detail" both touch the same
    // cache root. Plain string-starts-with would also match "tasks" vs
    // "tasks2", which is a different namespace — require the dot.
    a.starts_with(&format!("{b}.")) || b.starts_with(&format!("{a}."))
}

impl Registry for QueriesRegistry {
    fn name(&self) -> &'static str {
        "queries"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let prefixes: Vec<String> = pkg
            .manifest
            .queries
            .as_ref()
            .map(|q| q.key_prefixes.clone())
            .unwrap_or_default();
        if prefixes.is_empty() {
            // Idempotency: if the package is already registered with prefixes,
            // a re-register with no prefixes (manifest edit) should drop them.
            self.unregister(&pkg.manifest.id)?;
            return Ok(());
        }
        let mut map = self
            .by_pkg
            .write()
            .map_err(|_| anyhow!("queries registry lock poisoned"))?;
        for (other_id, others) in map.iter() {
            if other_id == &pkg.manifest.id {
                continue;
            }
            for new in &prefixes {
                for existing in others {
                    if collides(new, existing) {
                        bail!(
                            "query key prefix `{new}` collides with `{existing}` from package `{other_id}`"
                        );
                    }
                }
            }
        }
        map.insert(pkg.manifest.id.clone(), prefixes);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        if let Ok(mut map) = self.by_pkg.write() {
            map.remove(pkg_id);
        }
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let map = match self.by_pkg.read() {
            Ok(g) => g,
            Err(_) => return json!({ "error": "lock poisoned" }),
        };
        let entries: Vec<Value> = map
            .iter()
            .map(|(pkg_id, prefixes)| json!({ "pkg_id": pkg_id, "key_prefixes": prefixes }))
            .collect();
        json!({
            "count": entries.len(),
            "by_pkg": *map,
            "entries": entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{Manifest, Package, Permissions, QueriesBlock};
    use std::path::PathBuf;

    fn pkg_with(id: &str, prefixes: Vec<&str>) -> Package {
        let manifest = Manifest {
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
            ui: None,
            iyke: None,
            cron: vec![],
            window: None,
            queries: Some(QueriesBlock {
                key_prefixes: prefixes.into_iter().map(String::from).collect(),
            }),
            capabilities: None,
            engine: None,
            screenshots: vec![],
            requires: vec![],
            signature: None,
        };
        Package {
            manifest,
            install_path: PathBuf::from("/tmp/_unused"),
        }
    }

    #[test]
    fn accepts_distinct_prefixes() {
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["alpha"])).unwrap();
        r.register(&pkg_with("com.b.two", vec!["beta"])).unwrap();
    }

    #[test]
    fn rejects_exact_collision() {
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
        let err = r
            .register(&pkg_with("com.b.two", vec!["tasks"]))
            .unwrap_err();
        assert!(err.to_string().contains("collides"));
    }

    #[test]
    fn rejects_dotted_prefix_collision() {
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
        let err = r
            .register(&pkg_with("com.b.two", vec!["tasks.detail"]))
            .unwrap_err();
        assert!(err.to_string().contains("collides"));
    }

    #[test]
    fn allows_distinct_with_shared_substring() {
        // "tasks" vs "tasks2" do NOT collide — collision requires dotted boundary.
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
        r.register(&pkg_with("com.b.two", vec!["tasks2"])).unwrap();
    }

    #[test]
    fn re_register_same_pkg_is_idempotent() {
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
    }

    #[test]
    fn unregister_clears_slot() {
        let r = QueriesRegistry::new();
        r.register(&pkg_with("com.a.one", vec!["tasks"])).unwrap();
        r.unregister("com.a.one").unwrap();
        // After unregister, another package can claim it.
        r.register(&pkg_with("com.b.two", vec!["tasks"])).unwrap();
    }
}
