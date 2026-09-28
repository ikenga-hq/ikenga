//! Read-only pkg manifest helpers that need no live kernel (WP-19 slice 8):
//! the bodies of `pkg_preview_manifest` and of the workspace scan behind
//! `pkg_discover_workspace`.
//!
//! * [`preview_manifest`] is `Package::load(install_path)` rendered to JSON,
//!   registering nothing. The desktop command (`commands::pkg`) calls it on
//!   the path as given; the daemon arm (`server::rpc_files`) first resolves
//!   the path through its `PathGuard` and admits `manifest.json` only when its
//!   canonical location passes the guard too.
//! * [`discover`] is the scan `Kernel::discover_workspace` always ran. The one
//!   thing it needed from the kernel is the set of installed ids (for the
//!   `installed` flag), so that is now a parameter: the desktop kernel passes
//!   its live `installed` map, the daemon its `--pkgs-dir` index — the same set
//!   its `pkg_kernel_status` reports as installed, so the two answers agree.
//!   Under [`Reach::Confined`] a child directory or `manifest.json` that does
//!   not canonicalize to somewhere the guard admits reads as absent, as a
//!   `FsReach::Confined` read does.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;

pub use super::confined_fs::Reach;
use crate::pkg::manifest::Package;

/// One entry returned by `Kernel::discover_workspace` — a manifest dir found
/// in a workspace path. `valid=false` means the dir had a manifest.json but
/// it failed to parse; `error` carries the reason.
#[derive(Debug, Serialize, Clone)]
pub struct DiscoveredPkg {
    pub id: String,
    pub name: String,
    pub version: String,
    pub install_path: String,
    pub valid: bool,
    pub error: Option<String>,
    pub installed: bool,
    pub compatible: bool,
}

/// `pkg_preview_manifest`'s body: the manifest at `install_path`, parsed and
/// validated exactly as an install would, as JSON. Errors are the desktop's.
pub fn preview_manifest(install_path: &Path) -> Result<serde_json::Value, String> {
    let pkg = Package::load(install_path).map_err(|e| format!("{e:#}"))?;
    serde_json::to_value(&pkg.manifest).map_err(|e| format!("serialize manifest: {e}"))
}

/// Discover (but do NOT install) packages under a workspace directory: one
/// entry per direct child directory that contains a `manifest.json`; entries
/// that fail to parse are reported as `valid=false` with the error so the FE
/// can show a useful warning rather than silently dropping them. Read-only.
/// A missing or unreadable dir is an empty list, never an error.
pub fn discover(
    workspace_dir: &Path,
    installed_ids: &HashSet<String>,
    reach: Reach<'_>,
) -> Vec<DiscoveredPkg> {
    let mut out = Vec::new();
    if !workspace_dir.is_dir() {
        return out;
    }
    let entries = match std::fs::read_dir(workspace_dir) {
        Ok(e) => e,
        Err(err) => {
            log::warn!(
                "[pkg_kernel] discover_workspace: read_dir({}) failed: {err}",
                workspace_dir.display()
            );
            return out;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !reach.admits(&path) || !path.is_dir() {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        if !manifest_path.exists() || !reach.admits(&manifest_path) {
            continue;
        }
        match Package::load(&path) {
            Ok(pkg) => out.push(DiscoveredPkg {
                id: pkg.manifest.id.clone(),
                name: pkg.manifest.name.clone(),
                version: pkg.manifest.version.clone(),
                install_path: path.display().to_string(),
                valid: true,
                error: None,
                installed: installed_ids.contains(&pkg.manifest.id),
                compatible: pkg.is_compatible(),
            }),
            Err(e) => out.push(DiscoveredPkg {
                id: String::new(),
                name: String::new(),
                version: String::new(),
                install_path: path.display().to_string(),
                valid: false,
                error: Some(format!("{e:#}")),
                installed: false,
                compatible: false,
            }),
        }
    }
    out
}
