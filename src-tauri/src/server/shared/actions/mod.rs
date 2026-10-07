//! Actions / keybindings file layer (WP-50; G-ACTIONS §1, §2, §6, §8).
//!
//! Personal (`~/.ikenga/`) and project (`<project-root>/.ikenga/`)
//! `actions.json` + `keybindings.json`: read and validate against G-ACTIONS,
//! write atomically with `.recovery` on the G-SETTINGS substrate, refuse
//! symlinks / reparse points, watch with a 250 ms debounce and emit
//! `actions://changed`, and keep the user-side project-trust record
//! (DEC-55 per-action run pins, DEC-65 per-project keybindings pin).
//!
//! This layer hands both scopes to the frontend in load order (personal,
//! then project — G-ACTIONS §2.1); the merge (project over personal, held
//! project rules dropped) is WP-52's, and trust enforcement is WP-52's /
//! WP-53's. `settings.json` is not touched.
//!
//! # Why this lives under `server::shared` (WP-19 slice 5a)
//!
//! It is the desktop's actions manager, moved here unchanged because
//! `lib.rs` declares `crate::actions` desktop-only and the daemon's
//! `actions_*` / `keybindings_write` RPC arms need the same code (same
//! pattern as `server::shared::settings`). `crate::actions` is now a thin
//! desktop facade that re-exports everything below and adds the two
//! desktop-only pieces: the `AppHandle` constructor, whose notifier emits
//! `actions://changed`, and the OS opener behind `actions_open_file`.
//!
//! What varies between the two surfaces is injected, never branched on:
//! - an optional [`watch::ActionsNotifier`] — the desktop's emits
//!   `actions://changed`; the daemon's publishes on its event bus
//!   (`server::events`) for trust changes and starts no watcher
//!   ([`ActionsManager::without_file_watcher`]) — its arms announce a
//!   written file themselves;
//! - where the trust record lives — `<app_data_dir>` on the desktop,
//!   `<data-dir>` on the daemon; always user-side, never under a project;
//! - an optional [`RootGuard`] — the daemon's refuses a project whose root is
//!   outside its fs allowlist; the desktop passes none.
//!
//! **Trust invariant (both surfaces).** Nothing in this manager's write path
//! touches the trust record: `write` validates and writes one file and
//! returns; only `trust_grant` / `trust_revoke` change pins, and a grant pins
//! only the hashes of the documents in force *now* (refused whole when the
//! caller's hash is stale). So a written project file — from the webview or
//! over the daemon's `/api/rpc` — is held / re-asks until it is granted.

pub mod schema;
pub mod scope;
pub mod trust;
pub mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use crate::db::PaDb;
use crate::server::shared::settings::scope::resolve_project_scope;
use crate::server::shared::settings::SettingsScope;

use self::schema::{FileKind, ValidateContext, Validation};
use self::scope::{FileState, RawRead};
use self::trust::{ActionTrust, GrantAction, KeybindingsTrust, TrustStore};
use self::watch::{ActionsNotifier, ActionsWatcher, ChangeReason};

/// Checks a resolved project root before anything under it is read or
/// written. The daemon's refuses a root outside its fs allowlist; the
/// desktop has none (its project roots are the signed-in user's own).
pub type RootGuard = Arc<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// Both files of one scope.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeFiles {
    pub scope: SettingsScope,
    pub actions: FileState,
    pub keybindings: FileState,
}

/// `actions_read_files`: the user layers in load order (§2.1).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsFilesResult {
    pub personal: ScopeFiles,
    /// `None` when the active (or requested) project has no filesystem root.
    pub project: Option<ScopeFiles>,
    pub project_id: Option<String>,
    pub project_root: Option<String>,
    /// DEC-65: the project keybindings file's trust. `held()` ⇔ every rule
    /// in `project.keybindings` is dropped before the effective keymap.
    pub project_keybindings_trust: Option<KeybindingsTrust>,
    /// A trust-record read failure (the record is then treated as empty:
    /// nothing is trusted).
    pub trust_error: Option<String>,
}

/// `actions_write` / `keybindings_write`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionsWriteResult {
    /// `false` when validation refused the document; the file is untouched.
    pub written: bool,
    pub kind: FileKind,
    pub scope: SettingsScope,
    pub path: String,
    pub validation: Validation,
}

/// `actions_trust_status`.
///
/// Computed from the same **in-force** documents `actions_read_files`
/// serves: while a project file on disk is malformed, the last valid
/// document read this session stays in force (§1.1) and is what the status
/// describes (`*_stale: true`, the problem in `*_error`).
///
/// **Fail closed:** an action id missing from `actions` is untrusted. A
/// consumer (WP-53's run gate) must refuse a gated run whose id it cannot
/// find here, never treat "not listed" as "not gated".
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStatus {
    pub project_id: String,
    pub project_root: String,
    /// Every project action in force with its pin state. Empty when the
    /// project's `actions.json` is absent, or malformed with no last valid
    /// document this session (see `actions_error`).
    pub actions: Vec<ActionTrust>,
    /// A problem with the file on disk (invalid, linked, I/O).
    pub actions_error: Option<String>,
    /// `actions` describes the last valid document, not the file on disk.
    pub actions_stale: bool,
    pub keybindings: KeybindingsTrust,
    pub keybindings_error: Option<String>,
    /// `keybindings` describes the last valid document, not the file on disk.
    pub keybindings_stale: bool,
}

/// `actions_trust_grant` request. Each hash is the one the user was shown;
/// the grant is refused whole if a file changed since.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustGrantRequest {
    #[serde(default)]
    pub actions: Vec<GrantAction>,
    #[serde(default)]
    pub keybindings: Option<String>,
}

/// `actions_trust_revoke` request. Both absent revokes everything the
/// project has.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustRevokeRequest {
    #[serde(default)]
    pub action_ids: Option<Vec<String>>,
    #[serde(default)]
    pub keybindings: Option<bool>,
}

struct ResolvedProject {
    id: String,
    root: Option<PathBuf>,
}

pub struct ActionsManager {
    /// The desktop's emits `actions://changed`; the daemon's publishes on
    /// its event bus. `None` (tests): no emits.
    notifier: Option<ActionsNotifier>,
    db: Arc<PaDb>,
    home: PathBuf,
    trust: TrustStore,
    /// `None` when `notifier` is (a watcher exists only to emit), and on the
    /// daemon ([`ActionsManager::without_file_watcher`]).
    watcher: Option<ActionsWatcher>,
    root_guard: Option<RootGuard>,
    write_lock: AsyncMutex<()>,
    trust_lock: AsyncMutex<()>,
    /// Serializes whole watcher refreshes (boot and `projects:active-changed`
    /// may overlap), so the last resolved project is the one watched.
    refresh_lock: AsyncMutex<()>,
    /// Last valid document per path — what stays in force while the file
    /// on disk is malformed (§1.1).
    last_valid: LastValid,
}

/// The §1.1 stale fallback: the last valid document read (or written) this
/// session per path. `actions_read_files` and the trust status both resolve
/// the document in force through it, so they cannot disagree.
#[derive(Default)]
struct LastValid(Mutex<HashMap<PathBuf, Value>>);

impl LastValid {
    fn remember(&self, path: &Path, raw: &RawRead) {
        let Ok(mut cache) = self.0.lock() else {
            return;
        };
        if !raw.present && raw.error.is_none() {
            cache.remove(path);
        } else if let Some(document) = scope::valid_document(raw) {
            cache.insert(path.to_path_buf(), document.clone());
        }
    }

    fn insert(&self, path: &Path, document: Value) {
        if let Ok(mut cache) = self.0.lock() {
            cache.insert(path.to_path_buf(), document);
        }
    }

    /// Remembers `raw`, then returns the document in force for `path`: the
    /// file when valid, nothing when absent, else the last valid document
    /// (`stale: true`), or nothing if there was none this session.
    fn in_force(&self, path: &Path, raw: &RawRead) -> (Option<Value>, bool) {
        self.remember(path, raw);
        if let Some(document) = scope::valid_document(raw) {
            return (Some(document.clone()), false);
        }
        if !raw.present && raw.error.is_none() {
            return (None, false);
        }
        let fallback = self
            .0
            .lock()
            .ok()
            .and_then(|cache| cache.get(path).cloned());
        let stale = fallback.is_some();
        (fallback, stale)
    }
}

impl ActionsManager {
    /// `trust_dir` holds `actions-trust.json` (`app_data_dir` on the
    /// desktop, `--data-dir` on the daemon); `home` holds the personal
    /// `.ikenga/` files. The desktop constructor is
    /// `crate::actions::ActionsManager::new`.
    pub fn with_notifier(
        notifier: Option<ActionsNotifier>,
        db: Arc<PaDb>,
        trust_dir: &Path,
        home: PathBuf,
    ) -> Self {
        Self {
            watcher: notifier.clone().map(ActionsWatcher::new),
            notifier,
            db,
            home,
            trust: TrustStore::new(trust_dir),
            root_guard: None,
            write_lock: AsyncMutex::new(()),
            trust_lock: AsyncMutex::new(()),
            refresh_lock: AsyncMutex::new(()),
            last_valid: LastValid::default(),
        }
    }

    /// Never start a file watcher, whatever the notifier: the daemon's
    /// manager tells only about trust changes, and its arms announce a
    /// written file themselves (`server::rpc_files`).
    pub fn without_file_watcher(mut self) -> Self {
        self.watcher = None;
        self
    }

    /// Every resolved project root is checked by `guard` before anything
    /// under it is read or written (the daemon's fs-allowlist boundary).
    pub fn with_root_guard(mut self, guard: RootGuard) -> Self {
        self.root_guard = Some(guard);
        self
    }

    /// Starts the watchers for the personal and the active project's
    /// `.ikenga/` directories. Called at boot and on
    /// `projects:active-changed`. A no-op without a watcher (no notifier, or
    /// the daemon).
    pub async fn refresh_watch(&self) -> Result<(), String> {
        let Some(watcher) = self.watcher.as_ref() else {
            return Ok(());
        };
        let _guard = self.refresh_lock.lock().await;
        let mut targets = vec![(
            scope::ikenga_dir(&self.home),
            SettingsScope::Personal,
        )];
        let project = self.resolve_project(None).await?;
        if let Some(root) = project.root.as_ref() {
            targets.push((scope::ikenga_dir(root), SettingsScope::Project));
        }
        watcher.set_targets(&targets)
    }

    async fn resolve_project(&self, project_id: Option<&str>) -> Result<ResolvedProject, String> {
        let pool = self.db.ensure_pool().await?;
        let project = resolve_project_scope(&pool, project_id).await?;
        if let (Some(guard), Some(root)) = (self.root_guard.as_ref(), project.root.as_deref()) {
            guard(root)?;
        }
        Ok(ResolvedProject {
            id: project.id,
            root: project.root,
        })
    }

    fn path_for(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        project: Option<&ResolvedProject>,
    ) -> Result<PathBuf, String> {
        match scope {
            SettingsScope::Personal => Ok(scope::personal_file(&self.home, kind)),
            SettingsScope::Project => project
                .and_then(|project| project.root.as_ref())
                .map(|root| scope::project_file(root, kind))
                .ok_or_else(|| "the selected project has no filesystem root".to_string()),
        }
    }

    fn file_state(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        path: &Path,
        raw: RawRead,
    ) -> FileState {
        let (document, stale) = self.last_valid.in_force(path, &raw);
        FileState {
            kind,
            scope,
            path: path.to_string_lossy().into_owned(),
            present: raw.present,
            document,
            stale,
            validation: raw.validation,
            error: raw.error,
        }
    }

    /// Reads the four files; keybindings are validated knowing the user
    /// action ids both `actions.json` files define.
    pub async fn read_files(&self, project_id: Option<&str>) -> Result<ActionsFilesResult, String> {
        let project = self.resolve_project(project_id).await?;
        let personal_actions_path = scope::personal_file(&self.home, FileKind::Actions);
        let personal_keys_path = scope::personal_file(&self.home, FileKind::Keybindings);
        let project_paths = project.root.as_ref().map(|root| {
            (
                scope::project_file(root, FileKind::Actions),
                scope::project_file(root, FileKind::Keybindings),
            )
        });

        let actions_ctx = |scope| ValidateContext {
            kind: FileKind::Actions,
            scope,
            user_action_ids: None,
        };
        let personal_actions = scope::read_raw(&personal_actions_path, actions_ctx(SettingsScope::Personal));
        let project_actions = project_paths
            .as_ref()
            .map(|(actions, _)| scope::read_raw(actions, actions_ctx(SettingsScope::Project)));
        let ids = scope::collect_action_ids(
            scope::valid_document(&personal_actions)
                .into_iter()
                .chain(project_actions.as_ref().and_then(scope::valid_document)),
        );
        let keys_ctx = |scope| ValidateContext {
            kind: FileKind::Keybindings,
            scope,
            user_action_ids: Some(&ids),
        };
        let personal_keys = scope::read_raw(&personal_keys_path, keys_ctx(SettingsScope::Personal));
        let project_keys = project_paths
            .as_ref()
            .map(|(_, keys)| scope::read_raw(keys, keys_ctx(SettingsScope::Project)));

        let personal = ScopeFiles {
            scope: SettingsScope::Personal,
            actions: self.file_state(
                FileKind::Actions,
                SettingsScope::Personal,
                &personal_actions_path,
                personal_actions,
            ),
            keybindings: self.file_state(
                FileKind::Keybindings,
                SettingsScope::Personal,
                &personal_keys_path,
                personal_keys,
            ),
        };
        let project_files = match (project_paths, project_actions, project_keys) {
            (Some((actions_path, keys_path)), Some(actions), Some(keys)) => Some(ScopeFiles {
                scope: SettingsScope::Project,
                actions: self.file_state(
                    FileKind::Actions,
                    SettingsScope::Project,
                    &actions_path,
                    actions,
                ),
                keybindings: self.file_state(
                    FileKind::Keybindings,
                    SettingsScope::Project,
                    &keys_path,
                    keys,
                ),
            }),
            _ => None,
        };

        let (project_keybindings_trust, trust_error) = match (&project_files, &project.root) {
            (Some(files), Some(root)) => {
                let (record, trust_error) = match self.trust.load() {
                    Ok(record) => (record, None),
                    Err(error) => (Default::default(), Some(error)),
                };
                let root = root.to_string_lossy();
                let pins = record.project(&project.id, &root);
                let document = files.keybindings.document.as_ref();
                (Some(trust::keybindings_trust(document, pins)), trust_error)
            }
            _ => (None, None),
        };

        Ok(ActionsFilesResult {
            personal,
            project: project_files,
            project_root: project
                .root
                .as_ref()
                .map(|root| root.to_string_lossy().into_owned()),
            project_id: Some(project.id),
            project_keybindings_trust,
            trust_error,
        })
    }

    /// Validates and atomically writes one whole document. A document with
    /// any validation error is refused whole (`written: false`) and the file
    /// is left untouched. Writing never changes trust: a written project
    /// file is held / re-asks until `trust_grant` pins it.
    pub async fn write(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        project_id: Option<&str>,
        document: Value,
    ) -> Result<ActionsWriteResult, String> {
        let _guard = self.write_lock.lock().await;
        let project = self.resolve_project(project_id).await?;
        let path = self.path_for(kind, scope, Some(&project))?;
        let ids = match kind {
            FileKind::Actions => None,
            FileKind::Keybindings => Some(self.known_action_ids(&project)),
        };
        let ctx = ValidateContext {
            kind,
            scope,
            user_action_ids: ids.as_ref(),
        };
        let validation = scope::write_validated(&path, ctx, &document)?;
        let written = validation.is_ok();
        // No emit here: the directory watcher sees the atomic rename and
        // emits `actions://changed` once (a manual emit doubled it).
        if written {
            self.last_valid.insert(&path, document);
        }
        Ok(ActionsWriteResult {
            written,
            kind,
            scope,
            path: path.to_string_lossy().into_owned(),
            validation,
        })
    }

    /// User action ids the personal and project `actions.json` define (for
    /// `W_UNKNOWN_COMMAND` on a keybindings write).
    fn known_action_ids(&self, project: &ResolvedProject) -> HashSet<String> {
        let mut reads = vec![scope::read_raw(
            &scope::personal_file(&self.home, FileKind::Actions),
            ValidateContext {
                kind: FileKind::Actions,
                scope: SettingsScope::Personal,
                user_action_ids: None,
            },
        )];
        if let Some(root) = project.root.as_ref() {
            reads.push(scope::read_raw(
                &scope::project_file(root, FileKind::Actions),
                ValidateContext {
                    kind: FileKind::Actions,
                    scope: SettingsScope::Project,
                    user_action_ids: None,
                },
            ));
        }
        scope::collect_action_ids(reads.iter().filter_map(scope::valid_document))
    }

    /// Creates the file with an empty valid skeleton when absent, then hands
    /// it to `open` (the desktop's OS handler, `crate::actions::open_path`;
    /// the daemon serves no opener). Returns the path. A symlinked /
    /// reparse-point file or `.ikenga/` directory is refused before anything
    /// is written or opened: the OS opener follows links, so a project's
    /// `.ikenga/actions.json` pointing at a `.command` / `.app` would run it.
    pub async fn open_file_with(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        project_id: Option<&str>,
        open: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<String, String> {
        let _guard = self.write_lock.lock().await;
        let project = self.resolve_project(project_id).await?;
        let path = self.path_for(kind, scope, Some(&project))?;
        let refuse = |error: String| format!("refusing to open {}: {error}", path.display());
        if !scope::present_unlinked(&path).map_err(refuse)? {
            let ctx = ValidateContext {
                kind,
                scope,
                user_action_ids: None,
            };
            let validation = scope::write_validated(&path, ctx, &kind.skeleton())?;
            if !validation.is_ok() {
                return Err(validation.summary());
            }
        }
        // Re-checked right before the hand-off: the file is the project's to
        // replace at any time.
        scope::present_unlinked(&path).map_err(refuse)?;
        open(&path)?;
        Ok(path.to_string_lossy().into_owned())
    }

    // --- trust (DEC-55, DEC-65) -------------------------------------------

    fn project_with_root(project: &ResolvedProject) -> Result<(&str, String), String> {
        let root = project
            .root
            .as_ref()
            .ok_or_else(|| "the selected project has no filesystem root".to_string())?;
        Ok((project.id.as_str(), root.to_string_lossy().into_owned()))
    }

    fn status_for(
        &self,
        project: &ResolvedProject,
        record: &trust::TrustFile,
    ) -> Result<TrustStatus, String> {
        let (id, root_text) = Self::project_with_root(project)?;
        let root = project.root.as_ref().expect("checked by project_with_root");
        let pins = record.project(id, &root_text);

        // The same in-force documents `read_files` serves: a malformed file
        // keeps its last valid document in force (§1.1), and so its trust.
        let actions_path = scope::project_file(root, FileKind::Actions);
        let actions_read = scope::read_raw(
            &actions_path,
            ValidateContext {
                kind: FileKind::Actions,
                scope: SettingsScope::Project,
                user_action_ids: None,
            },
        );
        let actions_error = read_problem(&actions_read);
        let (actions_doc, actions_stale) = self.last_valid.in_force(&actions_path, &actions_read);
        let actions = actions_doc
            .as_ref()
            .map(|document| trust::action_trust(document, pins))
            .unwrap_or_default();

        let keys_path = scope::project_file(root, FileKind::Keybindings);
        let keys_read = scope::read_raw(
            &keys_path,
            ValidateContext {
                kind: FileKind::Keybindings,
                scope: SettingsScope::Project,
                user_action_ids: None,
            },
        );
        let keybindings_error = read_problem(&keys_read);
        let (keys_doc, keybindings_stale) = self.last_valid.in_force(&keys_path, &keys_read);
        let keybindings = trust::keybindings_trust(keys_doc.as_ref(), pins);

        Ok(TrustStatus {
            project_id: id.to_string(),
            project_root: root_text,
            actions,
            actions_error,
            actions_stale,
            keybindings,
            keybindings_error,
            keybindings_stale,
        })
    }

    pub async fn trust_status(&self, project_id: Option<&str>) -> Result<TrustStatus, String> {
        let project = self.resolve_project(project_id).await?;
        let _guard = self.trust_lock.lock().await;
        let record = self.trust.load()?;
        self.status_for(&project, &record)
    }

    pub async fn trust_grant(
        &self,
        project_id: Option<&str>,
        request: TrustGrantRequest,
    ) -> Result<TrustStatus, String> {
        let project = self.resolve_project(project_id).await?;
        let _guard = self.trust_lock.lock().await;
        let mut record = self.trust.load()?;
        let status = self.status_for(&project, &record)?;
        if !request.actions.is_empty() {
            if let Some(error) = status.actions_error.as_ref() {
                return Err(format!("the project actions.json is not valid: {error}"));
            }
        }
        if request.keybindings.is_some() {
            if let Some(error) = status.keybindings_error.as_ref() {
                return Err(format!("the project keybindings.json is not valid: {error}"));
            }
        }
        // Pins for actions the file no longer defines are pruned only
        // against a valid `actions.json` on disk: a momentarily malformed
        // (or stale) one must not wipe every action pin on a keybindings-only
        // grant.
        let actions_valid = status.actions_error.is_none() && !status.actions_stale;
        let entry = record.project_mut(&status.project_id, &status.project_root);
        trust::apply_grant(
            entry,
            &status.actions,
            actions_valid,
            &status.keybindings,
            &request.actions,
            request.keybindings.as_deref(),
        )?;
        self.trust.save(&record)?;
        let files = trust_files(!request.actions.is_empty(), request.keybindings.is_some());
        self.emit_trust_change(&project, &files);
        self.status_for(&project, &record)
    }

    /// DEC-55 / DEC-65: a grant or revoke changes which project rules and
    /// runs are in force without touching either file, so the watcher never
    /// sees it; tell the client to re-read.
    /// No notifier → nothing to tell. The daemon's publishes on its event
    /// bus (`/ws/events`).
    fn emit_trust_change(&self, project: &ResolvedProject, files: &[FileKind]) {
        let Some(notifier) = self.notifier.as_ref() else {
            return;
        };
        let Some(root) = project.root.as_ref() else {
            return;
        };
        for kind in files {
            watch::emit_change(
                notifier,
                &scope::project_file(root, *kind),
                *kind,
                SettingsScope::Project,
                Some(ChangeReason::Trust),
            );
        }
    }

    pub async fn trust_revoke(
        &self,
        project_id: Option<&str>,
        request: TrustRevokeRequest,
    ) -> Result<TrustStatus, String> {
        let project = self.resolve_project(project_id).await?;
        let _guard = self.trust_lock.lock().await;
        let mut record = self.trust.load()?;
        let (id, _) = Self::project_with_root(&project)?;
        let everything = request.action_ids.is_none() && request.keybindings.is_none();
        let files = if everything {
            trust_files(true, true)
        } else {
            trust_files(
                request.action_ids.as_ref().is_some_and(|ids| !ids.is_empty()),
                request.keybindings == Some(true),
            )
        };
        if everything {
            record.projects.remove(id);
        } else if let Some(entry) = record.projects.get_mut(id) {
            for action_id in request.action_ids.iter().flatten() {
                entry.actions.remove(action_id);
            }
            if request.keybindings == Some(true) {
                entry.keybindings = None;
            }
        }
        self.trust.save(&record)?;
        self.emit_trust_change(&project, &files);
        self.status_for(&project, &record)
    }
}

/// The files a trust change touched, for `actions://changed`.
fn trust_files(actions: bool, keybindings: bool) -> Vec<FileKind> {
    let mut files = Vec::new();
    if actions {
        files.push(FileKind::Actions);
    }
    if keybindings {
        files.push(FileKind::Keybindings);
    }
    files
}

fn read_problem(read: &RawRead) -> Option<String> {
    if let Some(error) = read.error.as_ref() {
        return Some(error.clone());
    }
    if !read.validation.is_ok() {
        return Some(read.validation.summary());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::trust::TrustState;

    fn keys_ctx() -> ValidateContext<'static> {
        ValidateContext {
            kind: FileKind::Keybindings,
            scope: SettingsScope::Project,
            user_action_ids: None,
        }
    }

    /// §1.1 + DEC-65: while the project keybindings file is malformed, its
    /// last valid rules stay in force, so the trust status must describe
    /// them (held, untrusted) — not report "absent", which would let a
    /// consumer treat the in-force rules as having nothing to hold.
    #[test]
    fn a_malformed_file_keeps_its_last_valid_document_and_trust_in_force() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = scope::project_file(temp.path(), FileKind::Keybindings);
        let good = serde_json::json!({ "version": 1, "bindings": [ { "key": "mod+shift+r", "command": "refresh-pulse" } ] });
        assert!(scope::write_validated(&path, keys_ctx(), &good).unwrap().is_ok());
        let cache = LastValid::default();

        let (document, stale) = cache.in_force(&path, &scope::read_raw(&path, keys_ctx()));
        assert!(!stale);
        let valid_hash = trust::keybindings_trust(document.as_ref(), None).hash;

        std::fs::write(&path, b"{ \"version\": 1, ").unwrap();
        let raw = scope::read_raw(&path, keys_ctx());
        assert!(read_problem(&raw).is_some());
        let (document, stale) = cache.in_force(&path, &raw);
        assert!(stale);
        let status = trust::keybindings_trust(document.as_ref(), None);
        assert_eq!(status.state, TrustState::Untrusted);
        assert!(status.held());
        assert_eq!(status.hash, valid_hash);

        // Deleting the file clears the fallback: absent is empty.
        std::fs::remove_file(&path).unwrap();
        let (document, stale) = cache.in_force(&path, &scope::read_raw(&path, keys_ctx()));
        assert!(document.is_none());
        assert!(!stale);
    }

    #[test]
    fn a_malformed_file_with_no_prior_valid_read_has_nothing_in_force() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = scope::project_file(temp.path(), FileKind::Keybindings);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let (document, stale) =
            LastValid::default().in_force(&path, &scope::read_raw(&path, keys_ctx()));
        assert!(document.is_none());
        assert!(!stale);
    }

    #[test]
    fn trust_changes_name_only_the_files_they_touch() {
        assert_eq!(trust_files(false, true), vec![FileKind::Keybindings]);
        assert_eq!(trust_files(true, true), vec![FileKind::Actions, FileKind::Keybindings]);
        assert!(trust_files(false, false).is_empty());
    }
}
