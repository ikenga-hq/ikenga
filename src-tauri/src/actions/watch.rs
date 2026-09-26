//! 250 ms debounced watcher over the personal (`~/.ikenga/`) and the active
//! project's (`<root>/.ikenga/`) directories. An on-disk change to
//! `actions.json` or `keybindings.json` emits `actions://changed` with
//! `{ path, file, scope }` (G-ACTIONS §1.1); the effective model re-merges
//! on it without a restart. The watcher is the only emitter for file
//! writes (a successful `actions_write` is seen here ~250 ms later); the
//! manager emits directly only for trust grants / revokes (`reason:
//! "trust"`), which change what is in force without touching either file. `settings.json` changes in the same directory are
//! the settings watcher's and are ignored here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use notify::RecommendedWatcher;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::settings::SettingsScope;

use super::schema::FileKind;

pub const CHANGED_EVENT: &str = "actions://changed";
const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActionsChangeEvent {
    pub path: String,
    pub file: FileKind,
    pub scope: SettingsScope,
    /// Absent for an on-disk change; `trust` when a grant / revoke changed
    /// what of the file is in force (DEC-55 / DEC-65).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<ChangeReason>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeReason {
    Trust,
}

pub fn emit_change(
    app: &AppHandle,
    path: &Path,
    file: FileKind,
    scope: SettingsScope,
    reason: Option<ChangeReason>,
) {
    let _ = app.emit(
        CHANGED_EVENT,
        ActionsChangeEvent {
            path: path.to_string_lossy().into_owned(),
            file,
            scope,
            reason,
        },
    );
}

/// The watched file an event path names, if any. Temp files
/// (`.actions.json.<n>.tmp`) and `.recovery` files are not it; the final
/// rename onto `actions.json` is. The watch is non-recursive on one
/// directory, so only the file name is compared (FSEvents may report the
/// directory under its canonical, symlink-resolved path).
pub fn watched_kind(path: &Path) -> Option<FileKind> {
    FileKind::from_file_name(path.file_name()?.to_str()?)
}

/// One watcher per watched `.ikenga/` directory.
pub struct ActionsWatcher {
    app: AppHandle,
    watchers: Mutex<HashMap<PathBuf, (SettingsScope, Debouncer<RecommendedWatcher>)>>,
}

impl ActionsWatcher {
    pub fn new(app: AppHandle) -> Self {
        Self {
            app,
            watchers: Mutex::new(HashMap::new()),
        }
    }

    /// Makes the watched set exactly `targets` (`.ikenga/` dir, scope):
    /// drops watchers no longer wanted, adds new ones. A directory whose
    /// parent (home or project root) exists is created so a first save is
    /// seen, as the settings watcher does for the same directory.
    ///
    /// The whole update — drop, then add — runs under one lock span, so two
    /// overlapping refreshes (boot and an early `projects:active-changed`)
    /// cannot interleave; the caller also serializes whole refreshes
    /// (`ActionsManager::refresh_watch`) so the last resolved project wins.
    pub fn set_targets(&self, targets: &[(PathBuf, SettingsScope)]) -> Result<(), String> {
        let mut watchers = self
            .watchers
            .lock()
            .map_err(|_| "actions watcher lock poisoned")?;
        watchers.retain(|dir, (scope, _)| {
            targets
                .iter()
                .any(|(target, target_scope)| target == dir && target_scope == scope)
        });
        let mut first_error = None;
        for (dir, scope) in targets {
            if watchers.contains_key(dir) {
                continue;
            }
            match self.watch_dir(dir, *scope) {
                Ok(Some(debouncer)) => {
                    watchers.insert(dir.clone(), (*scope, debouncer));
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!("[actions] watch {} failed: {error}", dir.display());
                    first_error.get_or_insert(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Creates the debounced watcher for one directory; the caller holds the
    /// map lock and inserts it. `None` when the root itself is missing.
    fn watch_dir(
        &self,
        dir: &Path,
        scope: SettingsScope,
    ) -> Result<Option<Debouncer<RecommendedWatcher>>, String> {
        if !dir.exists() {
            let Some(root) = dir.parent() else {
                return Ok(None);
            };
            if !root.is_dir() {
                return Ok(None);
            }
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("create actions directory {}: {e}", dir.display()))?;
        }
        let meta = std::fs::symlink_metadata(dir)
            .map_err(|e| format!("inspect {}: {e}", dir.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("refusing to watch linked directory {}", dir.display()));
        }
        let app = self.app.clone();
        let mut debouncer: Debouncer<RecommendedWatcher> =
            new_debouncer(DEBOUNCE, move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                let mut seen: Vec<FileKind> = Vec::new();
                for event in events {
                    let Some(kind) = watched_kind(&event.path) else {
                        continue;
                    };
                    if seen.contains(&kind) {
                        continue;
                    }
                    seen.push(kind);
                    emit_change(&app, &event.path, kind, scope, None);
                }
            })
            .map_err(|error| format!("create actions watcher: {error}"))?;
        debouncer
            .watcher()
            .watch(dir, notify::RecursiveMode::NonRecursive)
            .map_err(|error| format!("watch {}: {error}", dir.display()))?;
        Ok(Some(debouncer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_files_count() {
        let dir = Path::new("/home/u/.ikenga");
        assert_eq!(watched_kind(&dir.join("actions.json")), Some(FileKind::Actions));
        assert_eq!(
            watched_kind(&dir.join("keybindings.json")),
            Some(FileKind::Keybindings)
        );
        assert_eq!(watched_kind(&dir.join("settings.json")), None);
        assert_eq!(watched_kind(&dir.join(".actions.json.123.tmp")), None);
        assert_eq!(watched_kind(&dir.join(".actions.json.recovery")), None);
        assert_eq!(watched_kind(dir), None);
    }

    #[test]
    fn change_event_serializes_as_the_contract_shape() {
        let event = ActionsChangeEvent {
            path: "/p/.ikenga/keybindings.json".into(),
            file: FileKind::Keybindings,
            scope: SettingsScope::Project,
            reason: None,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "path": "/p/.ikenga/keybindings.json",
                "file": "keybindings",
                "scope": "project"
            })
        );
        let trust = ActionsChangeEvent {
            reason: Some(ChangeReason::Trust),
            ..event
        };
        assert_eq!(serde_json::to_value(&trust).unwrap()["reason"], "trust");
    }
}
