//! 250 ms debounced watcher over the personal (`~/.ikenga/`) and the active
//! project's (`<root>/.ikenga/`) directories. An on-disk change to
//! `actions.json` or `keybindings.json` emits `actions://changed` with
//! `{ path, file, scope }` (G-ACTIONS §1.1); the effective model re-merges
//! on it without a restart. `settings.json` changes in the same directory are
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
}

pub fn emit_change(app: &AppHandle, path: &Path, file: FileKind, scope: SettingsScope) {
    let _ = app.emit(
        CHANGED_EVENT,
        ActionsChangeEvent {
            path: path.to_string_lossy().into_owned(),
            file,
            scope,
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
    pub fn set_targets(&self, targets: &[(PathBuf, SettingsScope)]) -> Result<(), String> {
        {
            let mut watchers = self
                .watchers
                .lock()
                .map_err(|_| "actions watcher lock poisoned")?;
            watchers.retain(|dir, (scope, _)| {
                targets
                    .iter()
                    .any(|(target, target_scope)| target == dir && target_scope == scope)
            });
        }
        let mut first_error = None;
        for (dir, scope) in targets {
            if let Err(error) = self.watch_dir(dir, *scope) {
                tracing::warn!("[actions] watch {} failed: {error}", dir.display());
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn watch_dir(&self, dir: &Path, scope: SettingsScope) -> Result<(), String> {
        {
            let watchers = self
                .watchers
                .lock()
                .map_err(|_| "actions watcher lock poisoned")?;
            if watchers.contains_key(dir) {
                return Ok(());
            }
        }
        if !dir.exists() {
            let Some(root) = dir.parent() else {
                return Ok(());
            };
            if !root.is_dir() {
                return Ok(());
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
                    emit_change(&app, &event.path, kind, scope);
                }
            })
            .map_err(|error| format!("create actions watcher: {error}"))?;
        debouncer
            .watcher()
            .watch(dir, notify::RecursiveMode::NonRecursive)
            .map_err(|error| format!("watch {}: {error}", dir.display()))?;
        let mut watchers = self
            .watchers
            .lock()
            .map_err(|_| "actions watcher lock poisoned")?;
        watchers.insert(dir.to_path_buf(), (scope, debouncer));
        Ok(())
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
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "path": "/p/.ikenga/keybindings.json",
                "file": "keybindings",
                "scope": "project"
            })
        );
    }
}
