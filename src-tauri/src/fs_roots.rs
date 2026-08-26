//! User-configurable filesystem allowlist. Backs `commands::resolve_allowlisted`.
//!
//! Roots are stored as user-input strings (preserving `~`/env vars) in
//! `app_data_dir/fs_roots.json`. At runtime we keep both the inputs (for
//! round-tripping back to the UI) and the canonicalized `PathBuf`s used for
//! `is_allowed` checks. The active `FsRoots` lives in a process-wide
//! `OnceLock` so the resolver doesn't need to thread Tauri `State` through
//! every fs command and through non-command callers like `viewer_serve`.

// `add` / `remove` / `reset` are driven by the desktop settings commands;
// the daemon only ever reads the root set that `install()` seeded.
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

/// Defaults seeded into the JSON file on first run.
///
/// Empty by design: a fresh install has no FS allowlist until the user
/// adds a root via the onboarding wizard's "Project & file roots" step or
/// Settings → Storage → File roots. The empty-state UI in
/// `routes/onboarding/roots-body.tsx` already explains the consequence.
pub const DEFAULT_ROOTS: &[&str] = &[];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRoots {
    roots: Vec<String>,
}

#[derive(Debug, Clone)]
struct Entry {
    /// The raw user-supplied string, preserved for display + persistence.
    input: String,
    /// The lexically-resolved absolute path. Used for `starts_with` checks.
    /// We don't `canonicalize()` here because the root may not exist yet
    /// (e.g. a brand-new project dir the user is about to create), and
    /// `canonicalize` errors in that case.
    resolved: PathBuf,
}

#[derive(Debug)]
pub struct FsRoots {
    file: PathBuf,
    state: RwLock<Vec<Entry>>,
}

static CURRENT: OnceLock<Arc<FsRoots>> = OnceLock::new();

/// Resolve `~/` / env vars and turn into an absolute path. Does *not* require
/// the path to exist (defaults seed paths the user may not have created yet).
fn resolve_input(input: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(input)
        .map(|c| c.into_owned())
        .map_err(|e| anyhow!("expand {input}: {e}"))?;
    let mut p = PathBuf::from(&expanded);
    if !p.is_absolute() {
        p = std::env::current_dir()
            .context("current_dir for relative root")?
            .join(p);
    }
    // Best-effort canonicalize so /private symlink prefixes on macOS (where
    // /Users canonicalizes through /System/Volumes/Data) line up with the
    // paths returned by `canonicalize` inside `resolve_allowlisted`.
    Ok(p.canonicalize().unwrap_or(p))
}

impl FsRoots {
    /// Load from disk, seeding defaults if the file is missing.
    pub fn load(file: PathBuf) -> Result<Self> {
        let entries = if file.exists() {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            let persisted: PersistedRoots =
                serde_json::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
            persisted
                .roots
                .into_iter()
                .filter_map(|s| {
                    let resolved = resolve_input(&s).ok()?;
                    Some(Entry { input: s, resolved })
                })
                .collect()
        } else {
            DEFAULT_ROOTS
                .iter()
                .filter_map(|s| {
                    let resolved = resolve_input(s).ok()?;
                    Some(Entry {
                        input: (*s).to_string(),
                        resolved,
                    })
                })
                .collect()
        };

        let roots = Self {
            file,
            state: RwLock::new(entries),
        };

        // Persist on first boot so the defaults are visible to anyone
        // poking at the on-disk file. Best-effort — a failure here just
        // means we re-seed on the next launch.
        if let Err(e) = roots.persist() {
            log::warn!("[fs_roots] initial persist failed: {e:#}");
        }

        Ok(roots)
    }

    /// Snapshot of the user-supplied strings, suitable for shipping to the
    /// frontend.
    pub fn list_inputs(&self) -> Vec<String> {
        let guard = self.state.read().expect("fs_roots state poisoned");
        guard.iter().map(|e| e.input.clone()).collect()
    }

    /// Return true if `path` (already canonicalized) is under any active root.
    pub fn is_allowed(&self, path: &Path) -> bool {
        let guard = self.state.read().expect("fs_roots state poisoned");
        guard.iter().any(|e| path.starts_with(&e.resolved))
    }

    /// Add a new root. No-op if the trimmed input is empty or already present.
    /// Returns the updated input list.
    pub fn add(&self, input: &str) -> Result<Vec<String>> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("path is empty"));
        }
        let resolved = resolve_input(trimmed)?;
        {
            let mut guard = self.state.write().expect("fs_roots state poisoned");
            if guard.iter().any(|e| e.input == trimmed) {
                return Ok(guard.iter().map(|e| e.input.clone()).collect());
            }
            guard.push(Entry {
                input: trimmed.to_string(),
                resolved,
            });
        }
        self.persist()?;
        Ok(self.list_inputs())
    }

    /// Remove a root by its user-input string (the same string the UI shows).
    /// Returns the updated input list.
    pub fn remove(&self, input: &str) -> Result<Vec<String>> {
        {
            let mut guard = self.state.write().expect("fs_roots state poisoned");
            guard.retain(|e| e.input != input);
        }
        self.persist()?;
        Ok(self.list_inputs())
    }

    /// Reset to the built-in defaults.
    pub fn reset(&self) -> Result<Vec<String>> {
        {
            let mut guard = self.state.write().expect("fs_roots state poisoned");
            guard.clear();
            for s in DEFAULT_ROOTS {
                if let Ok(resolved) = resolve_input(s) {
                    guard.push(Entry {
                        input: (*s).to_string(),
                        resolved,
                    });
                }
            }
        }
        self.persist()?;
        Ok(self.list_inputs())
    }

    fn persist(&self) -> Result<()> {
        let inputs = self.list_inputs();
        let persisted = PersistedRoots { roots: inputs };
        let json = serde_json::to_string_pretty(&persisted).context("serialize fs_roots")?;
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.file)
            .with_context(|| format!("rename {} -> {}", tmp.display(), self.file.display()))?;
        Ok(())
    }
}

/// Install a process-global handle. Returns Err if already installed.
pub fn install(roots: Arc<FsRoots>) -> Result<()> {
    CURRENT
        .set(roots)
        .map_err(|_| anyhow!("fs_roots::install called twice"))
}

/// Read the process-global handle, if one has been installed. Used by the
/// allowlist resolver and the Tauri commands. Returns `None` only during the
/// brief window before `lib.rs::run` finishes its setup — every code path that
/// reads paths runs after that, so a `None` from production code is a bug.
pub fn current() -> Option<Arc<FsRoots>> {
    CURRENT.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn seeds_defaults_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file.clone()).unwrap();
        let inputs = roots.list_inputs();
        assert_eq!(inputs.len(), DEFAULT_ROOTS.len());
        assert!(file.exists());
    }

    #[test]
    fn add_and_remove_round_trips() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file.clone()).unwrap();

        let extra = dir.path().join("project");
        std::fs::create_dir_all(&extra).unwrap();
        let extra_s = extra.to_string_lossy().to_string();

        let after_add = roots.add(&extra_s).unwrap();
        assert!(after_add.contains(&extra_s));

        let reloaded = FsRoots::load(file.clone()).unwrap();
        assert!(reloaded.list_inputs().contains(&extra_s));

        let after_remove = roots.remove(&extra_s).unwrap();
        assert!(!after_remove.contains(&extra_s));
    }

    #[test]
    fn add_dedupes() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file).unwrap();

        let extra = dir.path().join("dup");
        std::fs::create_dir_all(&extra).unwrap();
        let extra_s = extra.to_string_lossy().to_string();
        roots.add(&extra_s).unwrap();
        let second = roots.add(&extra_s).unwrap();
        assert_eq!(second.iter().filter(|s| **s == extra_s).count(), 1);
    }

    #[test]
    fn is_allowed_matches_subpaths() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file).unwrap();

        let project = dir.path().join("proj");
        std::fs::create_dir_all(project.join("sub")).unwrap();
        roots.add(&project.to_string_lossy()).unwrap();

        let canon = project.join("sub").canonicalize().unwrap();
        assert!(roots.is_allowed(&canon));

        let outside = dir.path().join("other").canonicalize().ok();
        if let Some(o) = outside {
            assert!(!roots.is_allowed(&o));
        }
    }

    #[test]
    fn reset_restores_defaults() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file).unwrap();
        let extra = dir.path().join("ephemeral");
        std::fs::create_dir_all(&extra).unwrap();
        roots.add(&extra.to_string_lossy()).unwrap();
        let after_reset = roots.reset().unwrap();
        assert_eq!(after_reset.len(), DEFAULT_ROOTS.len());
    }
}
