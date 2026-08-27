//! File-system watcher pool. One `FsWatchManager` instance per consumer,
//! holding a `DashMap<String, WatcherEntry>` keyed by short uuid (the
//! "watcher id" the frontend sees).
//!
//! # Two consumers, one core
//!
//! The desktop app owns one manager as Tauri state and emits each change on
//! `fs://{watcher_id}` through the webview event bus. The headless daemon
//! owns one manager *per `/ws/fs` connection* and writes each change down
//! that socket. Neither of those belongs in here, so the watcher takes an
//! [`FsEventSink`] instead of an `AppHandle`: this module compiles into the
//! daemon (no `wry`, no `tauri::Wry`), and the emit-backed sink is the only
//! part gated behind `feature = "desktop"`.
//!
//! # The allowlist is re-checked per event, not per watch
//!
//! `path_allow::resolve_allowlisted` canonicalizes the watch ROOT once, at
//! watch time. That is not enough: a symlinked directory planted inside a
//! watched root makes the kernel report paths whose canonical form sits
//! outside every allowed root, and on a network socket those paths are a leak
//! of filenames and write timing to whoever holds the token. Every reported
//! path therefore goes back through `path_allow::check_allowlisted` before it
//! reaches a sink, and the canonical form is what gets emitted.
//!
//! # Debouncing, and what it costs
//!
//! Events come from `notify-debouncer-mini` (the same 250 ms debouncer
//! `pkg::file_watcher` uses), not from raw `notify`. An editor's
//! write-temp-then-rename fires a burst of events for one logical save; 1:1
//! forwarding would put every one of them on the wire.
//!
//! The debouncer collapses that burst to one event per path and **drops
//! notify's `EventKind` doing it** — `DebouncedEvent` carries only a path and
//! `Any`/`AnyContinuous`. So `ChangeKind` is reconstructed from the filesystem
//! state after the window closes; see [`classify`] for exactly how, and for
//! the one case where it can be wrong.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use dashmap::DashMap;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use serde::Serialize;
use uuid::Uuid;

/// Matches `pkg::file_watcher::DEBOUNCE_WINDOW`. Long enough to swallow an
/// editor's save burst, short enough that a hot-reload still feels immediate.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    Create,
    Modify,
    Remove,
    /// No longer produced: the debouncer discards notify's event kind, so a
    /// rename now surfaces as `Remove` on the old path and `Create` on the
    /// new one. Kept because `FileChange['kind']` in `src/lib/tauri-cmd.ts`
    /// still declares it and no consumer branches on it.
    Rename,
}

#[derive(Debug, Serialize, Clone)]
pub struct FileChange {
    pub kind: ChangeKind,
    /// Canonical absolute path — the form that passed the allowlist check,
    /// not the raw string `notify` reported.
    pub path: String,
}

/// Where a watcher's changes go. The manager knows nothing about webview
/// events or WebSockets; it hands each change to one of these.
///
/// Implementors are called from the debouncer's own thread, so `emit` must not
/// block: the desktop sink is a fire-and-forget `AppHandle::emit`, the daemon
/// sink an unbounded channel send.
pub trait FsEventSink: Send + Sync + 'static {
    fn emit(&self, watcher_id: &str, change: FileChange);
}

struct WatcherEntry {
    /// Holding the debouncer keeps the watch alive; dropping it sends the
    /// shutdown message that ends its worker thread. There is no separate
    /// forwarder thread or stop channel any more — the callback runs on the
    /// debouncer's thread and calls the sink directly.
    _debouncer: Debouncer<RecommendedWatcher>,
}

pub struct FsWatchManager {
    watchers: DashMap<String, WatcherEntry>,
}

impl FsWatchManager {
    pub fn new() -> Self {
        Self {
            watchers: DashMap::new(),
        }
    }

    /// Start watching `path`, delivering debounced, allowlist-checked changes
    /// to `sink`. Returns the watcher id.
    ///
    /// `path` is expected to have come through
    /// `path_allow::resolve_allowlisted` already — this call re-checks the
    /// paths the watcher *reports*, which is a different guarantee.
    pub fn watch_with_sink(&self, path: &Path, sink: Arc<dyn FsEventSink>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let id_for_cb = id.clone();
        let watch_start = SystemTime::now();
        let mut seen: HashSet<PathBuf> = HashSet::new();

        let mut debouncer = new_debouncer(DEBOUNCE_WINDOW, move |res: DebounceEventResult| {
            let events = match res {
                Ok(events) => events,
                Err(e) => {
                    tracing::debug!("[fs_watch] watcher {id_for_cb} error: {e}");
                    return;
                }
            };
            for ev in events {
                // Per-event allowlist re-check. See the module docs — the
                // watch root's canonicalisation does not cover the paths the
                // kernel reports underneath it.
                let canonical = match crate::path_allow::check_allowlisted(&ev.path) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::debug!(
                            "[fs_watch] dropping event for {}: {e}",
                            ev.path.display()
                        );
                        continue;
                    }
                };
                let kind = classify(&canonical, watch_start, &mut seen);
                sink.emit(
                    &id_for_cb,
                    FileChange {
                        kind,
                        path: canonical.to_string_lossy().into_owned(),
                    },
                );
            }
        })
        .context("create fs debouncer")?;

        let mode = if path.is_dir() {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        debouncer
            .watcher()
            .watch(path, mode)
            .with_context(|| format!("watch {}", path.display()))?;

        self.watchers.insert(
            id.clone(),
            WatcherEntry {
                _debouncer: debouncer,
            },
        );

        Ok(id)
    }

    pub fn unwatch(&self, id: &str) -> Result<()> {
        match self.watchers.remove(id) {
            // Dropping the entry drops the debouncer, which stops the watch.
            Some(_) => Ok(()),
            None => Err(anyhow!("unknown watcher id: {id}")),
        }
    }

    /// Live watcher count. The daemon caps how many one socket may hold.
    pub fn len(&self) -> usize {
        self.watchers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.watchers.is_empty()
    }
}

impl Default for FsWatchManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Reconstruct a [`ChangeKind`] from the filesystem state after the debounce
/// window closed.
///
/// - Path is gone → `Remove` (and it leaves `seen`, so a re-create later reads
///   as `Create` again).
/// - Path exists and this watcher has reported on it before → `Modify`.
/// - Path exists and this is its first report → `Create` if the filesystem
///   says it was born after the watch started, else `Modify`.
///
/// This is a *different* answer from the raw event stream, and in the cases
/// where they differ it is the more useful one: a file created and deleted
/// inside one window now reads `remove`, where raw notify would have reported
/// `create` for a file that no longer exists.
///
/// **Where it can be wrong:** on a filesystem with no birth time (`created()`
/// returns `Unsupported` — some older ext4 without `statx` btime, several
/// network mounts) the first report for a path always reads `Create`, so the
/// first edit to a pre-existing file looks like a creation. The consumer that
/// cares is `src/shell/artifact-wizard/scaffold.ts`, which fires on
/// `kind === 'create'`; on such a filesystem it can fire early. Everything
/// else in the frontend ignores `kind` entirely.
///
/// `symlink_metadata` rather than `metadata` so the creation of a symlink is
/// reported as a create rather than as a remove of its dangling target.
fn classify(path: &Path, watch_start: SystemTime, seen: &mut HashSet<PathBuf>) -> ChangeKind {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        seen.remove(path);
        return ChangeKind::Remove;
    };
    // `insert` returns false when the path was already present.
    if !seen.insert(path.to_path_buf()) {
        return ChangeKind::Modify;
    }
    match meta.created() {
        Ok(born) if born < watch_start => ChangeKind::Modify,
        _ => ChangeKind::Create,
    }
}

/// Desktop sink: forwards each change onto the webview event bus as
/// `fs://{watcher_id}`, which is what `fsListenWatch` subscribes to.
#[cfg(feature = "desktop")]
pub struct AppHandleSink {
    app: tauri::AppHandle,
}

#[cfg(feature = "desktop")]
impl FsEventSink for AppHandleSink {
    fn emit(&self, watcher_id: &str, change: FileChange) {
        use tauri::Emitter;
        let _ = self.app.emit(&format!("fs://{watcher_id}"), change);
    }
}

#[cfg(feature = "desktop")]
impl FsWatchManager {
    /// Desktop entry point, kept at its original name and signature so the
    /// `commands::fs` / `commands::claude_config` call sites are unchanged.
    pub fn watch(&self, app: tauri::AppHandle, path: &Path) -> Result<String> {
        self.watch_with_sink(path, Arc::new(AppHandleSink { app }))
    }
}

#[allow(dead_code)]
pub fn canonicalize_for_watch(path: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(path)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| path.to_string());
    Ok(PathBuf::from(expanded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_classifies_as_remove_and_leaves_seen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("gone.txt");
        let mut seen = HashSet::new();
        seen.insert(file.clone());

        assert_eq!(
            classify(&file, SystemTime::now(), &mut seen),
            ChangeKind::Remove
        );
        assert!(
            !seen.contains(&file),
            "a removed path must leave `seen` so a re-create reads as create"
        );
    }

    /// A file that existed before the watch started reads as `modify` on
    /// its first report — the case the wizard's `kind === 'create'` filter
    /// must not trip over.
    #[test]
    fn a_pre_existing_file_reads_as_modify_not_create() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        // Birth-time resolution can be coarse; make the gap unambiguous.
        std::thread::sleep(Duration::from_millis(20));
        let start = SystemTime::now();

        let mut seen = HashSet::new();
        if std::fs::symlink_metadata(&file).unwrap().created().is_err() {
            // No birth time on this filesystem — documented fallback, and the
            // assertion below cannot hold. See `classify`'s doc comment.
            return;
        }
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Modify);
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Modify);
    }

    #[test]
    fn a_file_born_after_the_watch_started_is_a_create() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("new.txt");
        let start = SystemTime::now();
        // Guard against coarse birth-time resolution: the file must be born
        // strictly after `start` for the "born < start" branch to be false.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&file, b"x").unwrap();

        let mut seen = HashSet::new();
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Create);
        // …and the follow-up edit is a modify, because the path is now `seen`.
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Modify);
    }

    #[test]
    fn a_removed_then_recreated_path_reads_create_again() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("flap.txt");
        let start = SystemTime::now();
        let mut seen = HashSet::new();

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Create);
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Modify);

        std::fs::remove_file(&file).unwrap();
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Remove);
        assert!(!seen.contains(&file));

        std::fs::write(&file, b"y").unwrap();
        assert_eq!(classify(&file, start, &mut seen), ChangeKind::Create);
    }

    #[test]
    fn change_kinds_serialize_lowercase_for_the_typescript_union() {
        // `FileChange['kind']` in src/lib/tauri-cmd.ts is
        // 'create' | 'modify' | 'remove' | 'rename'.
        for (kind, want) in [
            (ChangeKind::Create, "create"),
            (ChangeKind::Modify, "modify"),
            (ChangeKind::Remove, "remove"),
            (ChangeKind::Rename, "rename"),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{want}\""));
        }
    }

    #[test]
    fn a_file_change_serializes_with_the_field_names_typescript_reads() {
        let json = serde_json::to_value(FileChange {
            kind: ChangeKind::Create,
            path: "/tmp/x".into(),
        })
        .unwrap();
        assert_eq!(json["kind"], "create");
        assert_eq!(json["path"], "/tmp/x");
        assert_eq!(json.as_object().unwrap().len(), 2);
    }

    #[test]
    fn unwatch_of_an_unknown_id_is_an_error() {
        let mgr = FsWatchManager::new();
        assert!(mgr.is_empty());
        assert!(mgr.unwatch("nope").is_err());
    }
}
