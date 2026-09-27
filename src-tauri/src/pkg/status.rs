//! The `pkg_kernel_status` wire shape, and the one function that builds it.
//!
//! Compiled into BOTH binaries. The desktop [`Kernel::status`] and the
//! headless daemon's `pkg_kernel_status` RPC arm (`server::pkg_index`) both
//! go through [`assemble_status`], so the two surfaces cannot drift in shape:
//! the frontend (`PkgKernelStatus` in `src/lib/tauri-cmd.ts`, and the pkg
//! route resolver reading `registries.ui_routes.entries[]`) sees the same
//! JSON whichever host answered.
//!
//! Field order and names here ARE the wire contract (serde emits struct
//! fields in declaration order). Moved verbatim out of `pkg/kernel.rs`;
//! keep them byte-identical.
//!
//! [`Kernel::status`]: crate::pkg::kernel::Kernel::status

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::registry::Registry;
use super::source::InstallSource;

/// Status returned by `pkg_kernel_status` — useful for debugging and the
/// future Settings → Packages page.
#[derive(Debug, Serialize)]
pub struct KernelStatus {
    pub installed: Vec<InstalledSummary>,
    pub registries: HashMap<String, Value>,
    pub api_version: u32,
}

#[derive(Debug, Serialize, Clone)]
pub struct InstalledSummary {
    pub id: String,
    pub version: String,
    pub ikenga_api: String,
    pub install_path: String,
    pub enabled: bool,
    pub installed_at: i64,
    pub compatible: bool,
    /// Provenance — recorded at install time, used by the UI for grouping
    /// and by the kernel to refuse uninstall of `Builtin` pkgs.
    pub source: InstallSource,
    /// Scope (Phase 2 of projects-first-class). `Some("default" | "music-2026" | …)`
    /// means the pkg loads only when that project is active; `None` is the
    /// workspace scope (always loaded). The Phase 0 bootstrap stamps existing
    /// rows with `Some("default")` so they remain visible after upgrade.
    pub project_id: Option<String>,
}

/// Build a [`KernelStatus`] from the installed set and the registries the
/// caller actually runs. Each registry contributes its `snapshot()` under its
/// stable `name()`.
///
/// Pass only registries that are live on this host: a registry that is
/// listed reads as "running, with these entries", so the daemon passing a
/// desktop-only registry it never populates would report an authoritative
/// empty set for something it does not do at all.
pub fn assemble_status(
    installed: Vec<InstalledSummary>,
    registries: &[&dyn Registry],
    api_version: u32,
) -> KernelStatus {
    let registries = registries
        .iter()
        .map(|r| (r.name().to_string(), r.snapshot()))
        .collect();
    KernelStatus {
        installed,
        registries,
        api_version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkg::manifest::{Package, IKENGA_API_VERSION};
    use crate::pkg::registries::UiRoutesRegistry;
    use serde_json::json;

    fn summary(id: &str) -> InstalledSummary {
        InstalledSummary {
            id: id.into(),
            version: "0.1.0".into(),
            ikenga_api: "1".into(),
            install_path: format!("/pkgs/{id}"),
            enabled: true,
            installed_at: 1_700_000_000_000,
            compatible: true,
            source: InstallSource::Local {
                path: format!("/pkgs/{id}"),
            },
            project_id: None,
        }
    }

    #[test]
    fn assemble_status_keys_each_registry_snapshot_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("com.test.a");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            r#"{"id":"com.test.a","name":"A","version":"0.1.0","ikenga_api":"1",
                "ui":{"routes":[{"path":"/x","kind":"iframe","source":"dist/index.html"}]}}"#,
        )
        .unwrap();
        let pkg = Package::load(&dir).unwrap();
        let ui = UiRoutesRegistry::new();
        ui.register(&pkg).unwrap();

        let status = assemble_status(vec![summary("com.test.a")], &[&ui], IKENGA_API_VERSION);
        let wire = serde_json::to_value(&status).unwrap();

        assert_eq!(wire["api_version"], IKENGA_API_VERSION);
        assert_eq!(
            wire["registries"].as_object().unwrap().len(),
            1,
            "only the registries passed in may appear"
        );
        assert_eq!(
            wire["registries"]["ui_routes"],
            json!({
                "count": 1,
                "entries": [{
                    "pkg_id": "com.test.a",
                    "virtual_path": "pkg://com.test.a/x",
                    "path": "/x",
                    "kind": "iframe",
                    "source": "dist/index.html",
                }],
            })
        );
        assert_eq!(
            wire["installed"][0]["source"],
            json!({"kind":"local","path":"/pkgs/com.test.a"})
        );
    }

    /// The installed row's wire form is the FE contract — pin the field
    /// order, not just the key set, so a reorder in this struct is caught.
    #[test]
    fn installed_summary_serializes_in_contract_order() {
        let s = serde_json::to_string(&summary("com.test.a")).unwrap();
        assert_eq!(
            s,
            r#"{"id":"com.test.a","version":"0.1.0","ikenga_api":"1","install_path":"/pkgs/com.test.a","enabled":true,"installed_at":1700000000000,"compatible":true,"source":{"kind":"local","path":"/pkgs/com.test.a"},"project_id":null}"#
        );
    }

    #[test]
    fn assemble_status_with_no_registries_is_empty_not_absent() {
        let wire = serde_json::to_value(assemble_status(Vec::new(), &[], 5)).unwrap();
        assert_eq!(
            wire,
            json!({"installed": [], "registries": {}, "api_version": 5})
        );
    }
}
