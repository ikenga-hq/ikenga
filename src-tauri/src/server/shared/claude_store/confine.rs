//! Where the Ngwa vault is, and how far a vault command may follow a path
//! (WP-19 slice 7).
//!
//! A [`Vault`] carries what every store / primitive / Ọba command resolves at
//! its boundary: the home the `workspace` scope and the user-tier engine dirs
//! resolve against, the store root, and the [`Reach`].
//!
//! * **The desktop** ([`Vault::desktop`]): the process home and `store_root()`,
//!   resolved exactly as the commands always resolved them, with
//!   [`Reach::Follow`]. Its caller is the user's own renderer on the same uid,
//!   so a symlink in a scope is the user's own and is followed as the OS
//!   resolves it. Every desktop code path and error string is unchanged.
//! * **The daemon** ([`Vault::daemon`]): its router home and store — the
//!   daemon PROCESS's in production. **G-PRINCIPAL single-user seam:** under
//!   topology B each principal's daemon runs as that uid with its own HOME,
//!   so each principal has its own vault; a shared multi-principal daemon
//!   would have to resolve both per principal. With [`Reach::Confined`]: the
//!   caller is a remote token holder, so a symlink planted in a scope must not
//!   turn a vault operation into a read of, a write into, or a delete in
//!   anything outside the vault or inside the daemon's own state.
//!
//! **The vault bounds** ([`Bounds`], confined reach only): the store root,
//! and for every known scope root (the home, each project root from
//! `ikenga.db`) its `.claude`, `.agents`, `.gemini` and `.codex` dirs plus its
//! `.mcp.json` and `.claude.json` — each resolved to its real location, so a
//! dotfile-managed `~/.claude` (itself a link into a dotfiles repo) is still
//! inside. A path is admitted iff it resolves under one of those AND outside
//! the daemon's data dir / discovery file (`PathGuard::check_reserved`).
//!
//! * [`Bounds::read`] — what a copy reads THROUGH: the path is followed to its
//!   real location, which must be admitted. A tree copy runs it on every
//!   entry, so one escaping link inside a skill dir refuses the whole copy.
//! * [`Bounds::node`] — what is created, replaced or deleted: the node itself
//!   is never followed (an unlink removes the link, a rename replaces it), so
//!   its PARENT is resolved and the node's location admitted.
//! * [`Bounds::settings_file`] — a merge-engine rewrite, which both reads the
//!   file through and replaces it: both of the above.
//! * [`Bounds::placement`] — a caller-named dependent link (`oba_relink_
//!   dependents` / `oba_unlink_one`): it must sit directly in one of the
//!   dependents-scan dirs (`<root>/.claude/skills`, `.agents/skills`, …) of a
//!   known scope root, i.e. be something `oba_dependents` could have listed.

use std::path::{Component, Path, PathBuf};

use crate::db::PaDb;

use super::{dependent_search_dirs, Kind};

/// A path check over a resolved path: `Ok` when it may be touched. The daemon
/// passes closures over its `PathGuard`.
pub type PathCheck<'a> = &'a (dyn Fn(&Path) -> Result<(), String> + Sync);

/// The daemon's two `PathGuard` checks, both over a RESOLVED path (canonical,
/// or a canonical ancestor plus a missing tail).
#[derive(Clone, Copy)]
pub struct DaemonChecks<'a> {
    /// The daemon's own state: its data dir and discovery file
    /// (`PathGuard::check_reserved`).
    pub reserved: PathCheck<'a>,
    /// The fs allowlist, then the reserved set (`PathGuard::check`). For the
    /// two caller-named paths that are not vault paths at all: an import
    /// source and a relink's new master.
    pub allowlisted: PathCheck<'a>,
}

/// How far a vault command follows the paths it touches (see the module doc).
#[derive(Clone, Copy)]
pub enum Reach<'a> {
    /// The desktop: paths as the OS resolves them, symlinks followed.
    Follow,
    /// The daemon: confined to the vault bounds and out of its own state.
    Confined(DaemonChecks<'a>),
}

/// The daemon's refusal when a confined command needs the scope list and there
/// is no `ikenga.db` to read it from. Same words as `server::rpc::NO_DB`.
const NO_DB: &str =
    "no database: the daemon was started without --data-dir, so there is no ikenga.db to open";

/// Home + store + reach for one vault command (see the module doc).
pub struct Vault<'a> {
    home: Option<PathBuf>,
    store: Option<PathBuf>,
    reach: Reach<'a>,
}

impl Vault<'static> {
    /// The desktop's vault: the process home, `store_root()`, symlinks
    /// followed — exactly what the Tauri commands resolved before.
    pub fn desktop() -> Self {
        Vault {
            home: crate::platform::home_dir(),
            store: super::store_root(),
            reach: Reach::Follow,
        }
    }
}

impl Vault<'static> {
    /// The desktop's reach over an explicit home and store, so a test can run
    /// the desktop body against a temp vault (never the real user's).
    #[cfg(test)]
    pub(crate) fn follow_in(home: Option<PathBuf>, store: Option<PathBuf>) -> Self {
        Vault {
            home,
            store,
            reach: Reach::Follow,
        }
    }
}

impl<'a> Vault<'a> {
    /// The daemon's vault: its router home and store, confined.
    pub fn daemon(home: Option<PathBuf>, store: Option<PathBuf>, checks: DaemonChecks<'a>) -> Self {
        Vault {
            home,
            store,
            reach: Reach::Confined(checks),
        }
    }

    /// The store root, or the desktop's own error for an unresolvable one.
    pub(crate) fn store(&self) -> Result<PathBuf, String> {
        self.store
            .clone()
            .ok_or_else(|| "cannot resolve store root".to_string())
    }

    /// The home, or the desktop's own error for a missing one.
    pub(crate) fn home(&self) -> Result<PathBuf, String> {
        self.home
            .clone()
            .ok_or_else(|| "home directory not found".to_string())
    }

    pub(crate) fn home_opt(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// The daemon's checks, `None` on the desktop.
    pub(crate) fn checks(&self) -> Option<DaemonChecks<'a>> {
        match self.reach {
            Reach::Follow => None,
            Reach::Confined(c) => Some(c),
        }
    }

    /// A caller-supplied path. The desktop `shellexpand`s it (`~` and `$VAR`),
    /// as it always has. The daemon expands only `~` / `~/…` against its home
    /// — never an env var, whose value would come back in an error string —
    /// and refuses a relative path or any `..`.
    pub(crate) fn caller_path(&self, raw: &str) -> Result<PathBuf, String> {
        match self.reach {
            Reach::Follow => super::expand(raw).map_err(|e| e.to_string()),
            Reach::Confined(_) => {
                if raw.is_empty() {
                    return Err("path is required".to_string());
                }
                let path = if raw == "~" || raw.starts_with("~/") {
                    self.home()?
                        .join(raw.trim_start_matches('~').trim_start_matches('/'))
                } else {
                    PathBuf::from(raw)
                };
                if !path.is_absolute() {
                    return Err(format!("path must be absolute: {raw}"));
                }
                if path.components().any(|c| matches!(c, Component::ParentDir)) {
                    return Err(format!("path may not contain `..`: {raw}"));
                }
                Ok(path)
            }
        }
    }

    /// The vault bounds for a confined command; `None` on the desktop (which
    /// then never touches the database for them).
    pub(crate) async fn bounds(&self, db: &PaDb) -> Option<Bounds<'a>> {
        match self.reach {
            Reach::Follow => None,
            Reach::Confined(checks) => {
                let roots = super::all_scope_roots_in(db, self.home.as_deref()).await;
                Some(Bounds::new(checks, self.store.as_deref(), &roots))
            }
        }
    }

    /// [`Self::bounds`] for a command whose desktop form takes no database
    /// (relink / unlink): the daemon still needs the scope list, and without
    /// one it refuses rather than run unconfined.
    pub(crate) async fn bounds_opt(&self, db: Option<&PaDb>) -> Result<Option<Bounds<'a>>, String> {
        match (self.reach, db) {
            (Reach::Follow, _) => Ok(None),
            (Reach::Confined(_), Some(db)) => Ok(self.bounds(db).await),
            (Reach::Confined(_), None) => Err(NO_DB.to_string()),
        }
    }
}

/// The scope dirs of a root that are inside the vault.
const SCOPE_DIRS: [&str; 4] = [".claude", ".agents", ".gemini", ".codex"];
/// The settings files directly in a scope root the merge engine rewrites.
const SCOPE_FILES: [&str; 2] = [".mcp.json", ".claude.json"];

/// The confined reach's boundary (see the module doc). Built per request:
/// the scope list is a live `ikenga.db` read and the dirs are resolved now.
pub struct Bounds<'a> {
    /// Resolved dirs a path may sit under.
    roots: Vec<PathBuf>,
    /// Resolved files a path may be.
    files: Vec<PathBuf>,
    /// Resolved dependents-scan dirs a caller-named placement must sit in.
    placements: Vec<PathBuf>,
    checks: DaemonChecks<'a>,
}

impl<'a> Bounds<'a> {
    pub(crate) fn new(
        checks: DaemonChecks<'a>,
        store: Option<&Path>,
        scope_roots: &[PathBuf],
    ) -> Self {
        let mut roots = Vec::new();
        let mut files = Vec::new();
        let mut placements = Vec::new();
        if let Some(store) = store {
            roots.extend(resolve_lenient(store).ok());
        }
        for root in scope_roots {
            for dir in SCOPE_DIRS {
                roots.extend(resolve_lenient(&root.join(dir)).ok());
            }
            for file in SCOPE_FILES {
                files.extend(node_location(&root.join(file)).ok());
            }
        }
        for kind in [Kind::Skill, Kind::Agent, Kind::Command] {
            for dir in dependent_search_dirs(scope_roots, kind) {
                placements.extend(resolve_lenient(&dir).ok());
            }
        }
        Bounds {
            roots,
            files,
            placements,
            checks,
        }
    }

    /// `resolved` (already cleared of the daemon's state) is inside the vault.
    fn admit(&self, resolved: &Path, verb: &str, p: &Path) -> Result<(), String> {
        let inside = self.roots.iter().any(|r| resolved.starts_with(r))
            || self.files.iter().any(|f| resolved == f);
        if inside {
            Ok(())
        } else {
            Err(format!(
                "refusing to {verb} {}: it resolves outside the Ngwa vault (the store, \
                 and each scope's .claude / .agents / .gemini / .codex): {}",
                p.display(),
                resolved.display()
            ))
        }
    }

    /// `p` is read through: its real location (every link followed) must be
    /// inside the vault. A path that does not resolve (dangling) is refused.
    pub(crate) fn read(&self, p: &Path) -> Result<(), String> {
        let canonical = p.canonicalize().map_err(|e| {
            format!(
                "refusing to read {}: it does not resolve ({e})",
                p.display()
            )
        })?;
        (self.checks.reserved)(&canonical)?;
        self.admit(&canonical, "read", p)
    }

    fn node_cleared(&self, p: &Path, verb: &str) -> Result<PathBuf, String> {
        node_clear_of_reserved(self.checks, p, verb)
    }

    /// `p` is created, replaced or deleted as a node (never followed): its
    /// parent's real location plus its name must be inside the vault.
    pub(crate) fn node(&self, p: &Path, verb: &str) -> Result<(), String> {
        let location = self.node_cleared(p, verb)?;
        self.admit(&location, verb, p)
    }

    /// A settings file the merge engine reads and rewrites: its node, and —
    /// when something is there — what it resolves to.
    pub(crate) fn settings_file(&self, p: &Path) -> Result<(), String> {
        self.node(p, "rewrite")?;
        if std::fs::symlink_metadata(p).is_ok() {
            self.read(p)?;
        }
        Ok(())
    }

    /// A caller-named dependent link: directly inside a dependents-scan dir of
    /// a known scope root, and out of the daemon's state.
    pub(crate) fn placement(&self, link: &Path, verb: &str) -> Result<(), String> {
        let location = self.node_cleared(link, verb)?;
        let in_scan_dir = location
            .parent()
            .is_some_and(|parent| self.placements.iter().any(|d| d == parent));
        if in_scan_dir {
            Ok(())
        } else {
            Err(format!(
                "refusing to {verb} {}: not a placement in a known scope's skills / agents / \
                 commands dir",
                link.display()
            ))
        }
    }
}

/// Where the node `p` sits, cleared of the daemon's state: its parent
/// resolved, and — unless the node is itself a link, which is only ever
/// unlinked or renamed over, never entered — the node too (so the data dir
/// itself, sitting where a primitive would, is never `remove_dir_all`'d).
pub(crate) fn node_clear_of_reserved(
    checks: DaemonChecks<'_>,
    p: &Path,
    verb: &str,
) -> Result<PathBuf, String> {
    let location =
        node_location(p).map_err(|e| format!("refusing to {verb} {}: {e}", p.display()))?;
    if let Some(parent) = location.parent() {
        (checks.reserved)(parent)?;
    }
    let is_link = std::fs::symlink_metadata(&location)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if !is_link {
        (checks.reserved)(&location)?;
    }
    Ok(location)
}

/// Where the node `p` sits, without following `p` itself: its parent
/// resolved ([`resolve_lenient`]) plus its name.
pub(crate) fn node_location(p: &Path) -> Result<PathBuf, String> {
    let name = p
        .file_name()
        .ok_or_else(|| format!("path has no file name: {}", p.display()))?;
    let parent = p
        .parent()
        .ok_or_else(|| format!("path has no parent: {}", p.display()))?;
    Ok(resolve_lenient(parent)?.join(name))
}

/// `p` resolved as far as it exists: its nearest existing ancestor (by
/// `lstat`, so a dangling link counts as existing and then fails to resolve)
/// canonicalized, the missing tail re-attached. Must be absolute and
/// `..`-free, since a re-attached tail is never canonicalized.
pub(crate) fn resolve_lenient(p: &Path) -> Result<PathBuf, String> {
    if !p.is_absolute() {
        return Err(format!("path is not absolute: {}", p.display()));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("path may not contain `..`: {}", p.display()));
    }
    let mut ancestor = p;
    while std::fs::symlink_metadata(ancestor).is_err() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| format!("no existing ancestor: {}", p.display()))?;
    }
    let canonical = ancestor
        .canonicalize()
        .map_err(|e| format!("{} does not resolve ({e})", ancestor.display()))?;
    let tail = p
        .strip_prefix(ancestor)
        .map_err(|_| format!("failed to resolve {}", p.display()))?;
    Ok(canonical.join(tail))
}
