//! Sidecars registry — first concrete `Registry` implementation.
//!
//! Tracks the binaries each package contributes. Stores `(pkg_id, name) →
//! resolved absolute path` so future spawn helpers can look up the right
//! binary without going through the static `externalBin` list. The host's
//! built-in sidecars (`pa-mbox`, `pa-actions`, `pa-storyboard`, etc.) stay
//! on the existing `app.shell().sidecar(NAME)` path for now — this registry
//! only governs *package* sidecars.
//!
//! Validation done at register time:
//!   - target triple in manifest matches the host (Linux x86_64 today)
//!   - `{target}` placeholder in the bin path is expanded
//!   - resolved binary exists and is under the package install dir
//!   - sidecar name uniqueness (the manifest enforces the prefix; this
//!     registry catches in-prefix collisions across two packages that use
//!     the same id by accident)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

/// One entry per (package, sidecar-name) pair.
#[derive(Debug, Clone)]
pub struct SidecarEntry {
    pub pkg_id: String,
    pub name: String,
    pub bin_path: PathBuf,
}

#[derive(Default)]
pub struct SidecarsRegistry {
    /// Keyed by sidecar name (which is globally unique by manifest contract:
    /// `pa-{pkg-slug}-{sub}`). Lookups by name are the common case for spawn
    /// helpers; reverse lookup by pkg_id is done with a linear scan during
    /// uninstall (rare).
    entries: RwLock<HashMap<String, SidecarEntry>>,
}

impl SidecarsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a sidecar's absolute binary path by name. Returns None if the
    /// name doesn't belong to any installed package.
    pub fn resolve(&self, name: &str) -> Option<SidecarEntry> {
        self.entries.read().ok()?.get(name).cloned()
    }
}

impl Registry for SidecarsRegistry {
    fn name(&self) -> &'static str {
        "sidecars"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        if pkg.manifest.sidecars.is_empty() {
            return Ok(());
        }

        // Target gate: a package can ship multiple targets, but at install
        // time at least one must match this host. Skill packs (`targets:
        // []`) bypass this check.
        let host_target = host_target_triple();
        if !pkg.manifest.targets.is_empty() && !pkg.manifest.targets.contains(&host_target) {
            return Err(anyhow!(
                "package `{}` ships targets {:?}, host is `{}`",
                pkg.manifest.id,
                pkg.manifest.targets,
                host_target
            ));
        }

        let mut new_entries: Vec<SidecarEntry> = Vec::with_capacity(pkg.manifest.sidecars.len());
        for spec in &pkg.manifest.sidecars {
            let bin_rel = spec.bin.replace("{target}", &host_target);
            let bin_path = pkg.resolve_relative(&bin_rel).with_context(|| {
                format!(
                    "sidecar `{}` of `{}`: bin `{}`",
                    spec.name, pkg.manifest.id, bin_rel
                )
            })?;
            if !bin_path.is_file() {
                return Err(anyhow!(
                    "sidecar `{}` of `{}`: bin `{}` not found",
                    spec.name,
                    pkg.manifest.id,
                    bin_path.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(&bin_path) {
                    let mut perms = metadata.permissions();
                    if perms.mode() & 0o111 == 0 {
                        perms.set_mode(perms.mode() | 0o755);
                        let _ = std::fs::set_permissions(&bin_path, perms);
                    }
                }
            }
            new_entries.push(SidecarEntry {
                pkg_id: pkg.manifest.id.clone(),
                name: spec.name.clone(),
                bin_path,
            });
        }

        // Atomic apply: collision check + insert under one write lock.
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("sidecars registry lock poisoned"))?;
        for entry in &new_entries {
            if let Some(existing) = entries.get(&entry.name) {
                if existing.pkg_id != entry.pkg_id {
                    return Err(anyhow!(
                        "sidecar name `{}` already registered by `{}`",
                        entry.name,
                        existing.pkg_id
                    ));
                }
                // Same pkg re-registering (boot replay): treat as idempotent.
            }
        }
        for entry in new_entries {
            entries.insert(entry.name.clone(), entry);
        }
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let mut entries = self
            .entries
            .write()
            .map_err(|_| anyhow!("sidecars registry lock poisoned"))?;
        entries.retain(|_, e| e.pkg_id != pkg_id);
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let entries = match self.entries.read() {
            Ok(g) => g,
            Err(_) => return json!({ "error": "lock poisoned" }),
        };
        let list: Vec<_> = entries
            .values()
            .map(|e| {
                json!({
                    "pkg_id": e.pkg_id,
                    "name": e.name,
                    "bin_path": e.bin_path.display().to_string(),
                })
            })
            .collect();
        json!({ "count": list.len(), "entries": list })
    }
}

/// Host's rust target triple. Hard-coded per-OS arms cover what the host
/// supports today; wrong arch hits the catch-all and surfaces as an install
/// error rather than silently accepting an incompatible binary.
fn host_target_triple() -> String {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu".into()
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu".into()
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin".into()
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin".into()
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc".into()
    } else {
        "unknown".into()
    }
}
