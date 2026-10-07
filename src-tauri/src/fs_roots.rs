//! User-configurable filesystem allowlist. Backs `commands::resolve_allowlisted`.
//!
//! Roots are stored as user-input strings (preserving `~`/env vars) in
//! `app_data_dir/fs_roots.json`. At runtime we keep both the inputs (for
//! round-tripping back to the UI) and the canonicalized `PathBuf`s used for
//! `is_allowed` checks. The active `FsRoots` lives in a process-wide
//! `OnceLock` so the resolver doesn't need to thread Tauri `State` through
//! every fs command and through non-command callers like `viewer_serve`.

// `add` / `remove` / `reset` are driven by the desktop settings commands and
// by the daemon's `fs_roots_*` arms (`server::rpc_fs_roots`).

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
    /// True once the list has been seeded or edited (add / remove / reset).
    /// It is what tells a list nobody has set up yet from one its owner
    /// emptied on purpose: only the first is ever seeded
    /// ([`FsRoots::load_seeded`]). Files written before the flag existed
    /// read as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    initialized: bool,
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
struct State {
    entries: Vec<Entry>,
    initialized: bool,
}

#[derive(Debug)]
pub struct FsRoots {
    file: PathBuf,
    /// What [`FsRoots::reset`] restores: the seed this set was loaded with,
    /// or [`DEFAULT_ROOTS`] when it had none.
    defaults: Vec<String>,
    state: RwLock<State>,
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

fn entries_of<S: AsRef<str>>(inputs: &[S]) -> Vec<Entry> {
    inputs
        .iter()
        .filter_map(|s| {
            let resolved = resolve_input(s.as_ref()).ok()?;
            Some(Entry {
                input: s.as_ref().to_string(),
                resolved,
            })
        })
        .collect()
}

impl FsRoots {
    /// Load from disk, seeding defaults if the file is missing. The desktop's
    /// loader; the daemon always goes through [`FsRoots::load_seeded`].
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    pub fn load(file: PathBuf) -> Result<Self> {
        Self::load_seeded(file, Vec::new())
    }

    /// [`FsRoots::load`] for a set that starts from `seed` rather than
    /// [`DEFAULT_ROOTS`]: a T1 principal child seeds its principal's home
    /// (gap audit 2026-10-06 rank 1). The seed is applied when the list is
    /// empty **and** was never seeded or edited — a missing file, or one
    /// written before the `initialized` flag existed (every account on a host
    /// that predates this) — and never to a list its owner emptied on
    /// purpose. `reset` restores the seed.
    pub fn load_seeded(file: PathBuf, seed: Vec<String>) -> Result<Self> {
        let (inputs, initialized) = if file.exists() {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("read {}", file.display()))?;
            let persisted: PersistedRoots =
                serde_json::from_str(&text).with_context(|| format!("parse {}", file.display()))?;
            (persisted.roots, persisted.initialized)
        } else {
            (DEFAULT_ROOTS.iter().map(|s| s.to_string()).collect(), false)
        };
        let defaults = if seed.is_empty() {
            DEFAULT_ROOTS.iter().map(|s| s.to_string()).collect()
        } else {
            seed
        };
        let (inputs, initialized) = if !initialized && inputs.is_empty() && !defaults.is_empty() {
            tracing::info!("[fs_roots] seeding {} with {defaults:?}", file.display());
            (defaults.clone(), true)
        } else {
            (inputs, initialized)
        };

        let roots = Self {
            file,
            defaults,
            state: RwLock::new(State {
                entries: entries_of(&inputs),
                initialized,
            }),
        };

        // Persist on first boot so the defaults are visible to anyone
        // poking at the on-disk file. Best-effort — a failure here just
        // means we re-seed on the next launch.
        {
            let guard = roots.state.read().expect("fs_roots state poisoned");
            if let Err(e) = roots.persist(&guard) {
                tracing::warn!("[fs_roots] initial persist failed: {e:#}");
            }
        }

        Ok(roots)
    }

    /// Snapshot of the user-supplied strings, suitable for shipping to the
    /// frontend.
    pub fn list_inputs(&self) -> Vec<String> {
        let guard = self.state.read().expect("fs_roots state poisoned");
        guard.entries.iter().map(|e| e.input.clone()).collect()
    }

    /// Return true if `path` (already canonicalized) is under any active root.
    pub fn is_allowed(&self, path: &Path) -> bool {
        let guard = self.state.read().expect("fs_roots state poisoned");
        guard.entries.iter().any(|e| path.starts_with(&e.resolved))
    }

    /// The active root that `path` (canonical) is, or holds — `path` is that
    /// root or one of its ancestors. A subtree operation (the daemon's
    /// `fs_trash`) on such a path would move a whole root away; `is_allowed`
    /// alone cannot see that, because a root counts as inside itself.
    pub fn root_within(&self, path: &Path) -> Option<PathBuf> {
        let guard = self.state.read().expect("fs_roots state poisoned");
        guard
            .entries
            .iter()
            .find(|e| e.resolved.starts_with(path))
            .map(|e| e.resolved.clone())
    }

    /// Add a new root. No-op if the trimmed input is empty or already present.
    /// Returns the updated input list.
    pub fn add(&self, input: &str) -> Result<Vec<String>> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("path is empty"));
        }
        let resolved = resolve_input(trimmed)?;
        self.mutate(|entries| {
            if entries
                .iter()
                .any(|e| e.input == trimmed || e.resolved == resolved)
            {
                return;
            }
            entries.push(Entry {
                input: trimmed.to_string(),
                resolved,
            });
        })
    }

    /// Remove a root by its user-input string (the same string the UI shows).
    /// Returns the updated input list.
    pub fn remove(&self, input: &str) -> Result<Vec<String>> {
        self.mutate(|entries| entries.retain(|e| e.input != input))
    }

    /// Reset to this set's defaults (its seed, if it was loaded with one).
    pub fn reset(&self) -> Result<Vec<String>> {
        let defaults = entries_of(&self.defaults);
        self.mutate(|entries| *entries = defaults)
    }

    /// Apply `edit` to a copy of the entries, persist the result, and only
    /// then make it live — all under the write lock, so a failed write
    /// leaves the live set as it was, and two concurrent edits (two browser
    /// tabs, say) never interleave their writes of the one `.tmp` file.
    fn mutate(&self, edit: impl FnOnce(&mut Vec<Entry>)) -> Result<Vec<String>> {
        let mut guard = self.state.write().expect("fs_roots state poisoned");
        let mut next = State {
            entries: guard.entries.clone(),
            initialized: true,
        };
        edit(&mut next.entries);
        self.persist(&next)?;
        *guard = next;
        Ok(guard.entries.iter().map(|e| e.input.clone()).collect())
    }

    fn persist(&self, state: &State) -> Result<()> {
        let persisted = PersistedRoots {
            roots: state.entries.iter().map(|e| e.input.clone()).collect(),
            initialized: state.initialized,
        };
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

    fn initialized_flag(file: &Path) -> bool {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
        v["initialized"].as_bool().unwrap_or(false)
    }

    /// A T1 principal's first run: no file yet, so the seed (its home) is
    /// the list, and the file records that it was seeded.
    #[test]
    fn a_new_principal_is_seeded_with_its_home() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home_s = home.to_string_lossy().to_string();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load_seeded(file.clone(), vec![home_s.clone()]).unwrap();
        assert_eq!(roots.list_inputs(), vec![home_s.clone()]);
        assert!(roots.is_allowed(&home.canonicalize().unwrap().join("x")));
        assert!(initialized_flag(&file));
        // A reload with the same seed changes nothing.
        let again = FsRoots::load_seeded(file, vec![home_s.clone()]).unwrap();
        assert_eq!(again.list_inputs(), vec![home_s]);
    }

    /// Accounts that predate seeding: their file is `{"roots": []}` with no
    /// flag (what `load` wrote on the child's first boot). Never seeded, so
    /// the next boot seeds it.
    #[test]
    fn an_existing_never_seeded_empty_list_is_seeded() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        std::fs::write(&file, r#"{"roots": []}"#).unwrap();
        let roots = FsRoots::load_seeded(file.clone(), vec!["/srv/home/ada".into()]).unwrap();
        assert_eq!(roots.list_inputs(), vec!["/srv/home/ada".to_string()]);
        assert!(initialized_flag(&file));
    }

    /// A list its owner emptied on purpose stays empty across boots.
    #[test]
    fn a_deliberately_emptied_list_is_not_reseeded() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let seed = vec!["/srv/home/ada".to_string()];
        let roots = FsRoots::load_seeded(file.clone(), seed.clone()).unwrap();
        assert!(roots.remove("/srv/home/ada").unwrap().is_empty());
        drop(roots);
        let reloaded = FsRoots::load_seeded(file.clone(), seed.clone()).unwrap();
        assert!(reloaded.list_inputs().is_empty());
        // `reset` is the way back to the seed.
        assert_eq!(reloaded.reset().unwrap(), seed);
    }

    /// A list someone already filled is left alone, and a set loaded with
    /// no seed (T0, the desktop) behaves exactly as before.
    #[test]
    fn a_configured_list_and_an_unseeded_set_are_left_alone() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        std::fs::write(&file, r#"{"roots": ["/work"]}"#).unwrap();
        let roots = FsRoots::load_seeded(file, vec!["/srv/home/ada".into()]).unwrap();
        assert_eq!(roots.list_inputs(), vec!["/work".to_string()]);

        let file = dir.path().join("t0.json");
        std::fs::write(&file, r#"{"roots": []}"#).unwrap();
        let t0 = FsRoots::load(file.clone()).unwrap();
        assert!(t0.list_inputs().is_empty());
        assert!(!initialized_flag(&file), "loading alone marks nothing");
    }

    /// A write that fails leaves the live set as it was (the edit is made
    /// live only after it is on disk).
    #[test]
    fn a_failed_write_does_not_change_the_live_set() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("fs_roots.json");
        let roots = FsRoots::load(file.clone()).unwrap();
        let extra = dir.path().join("proj");
        std::fs::create_dir_all(&extra).unwrap();
        // The `.tmp` path is a directory, so the write fails.
        std::fs::create_dir_all(file.with_extension("json.tmp")).unwrap();
        assert!(roots.add(&extra.to_string_lossy()).is_err());
        assert!(roots.list_inputs().is_empty());
        assert!(!roots.is_allowed(&extra.canonicalize().unwrap()));
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
