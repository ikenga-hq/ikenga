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

pub mod schema;
pub mod scope;
pub mod trust;
pub mod watch;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::AppHandle;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::db::PaDb;
use crate::settings::scope::resolve_project_scope;
use crate::settings::SettingsScope;

use self::schema::{FileKind, ValidateContext, Validation};
use self::scope::{FileState, RawRead};
use self::trust::{ActionTrust, GrantAction, KeybindingsTrust, TrustStore};
use self::watch::ActionsWatcher;

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
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustStatus {
    pub project_id: String,
    pub project_root: String,
    /// Every project action with its pin state. Empty when the project's
    /// `actions.json` is absent or invalid (see `actions_error`).
    pub actions: Vec<ActionTrust>,
    pub actions_error: Option<String>,
    pub keybindings: KeybindingsTrust,
    pub keybindings_error: Option<String>,
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
    app: AppHandle,
    db: Arc<PaDb>,
    home: PathBuf,
    trust: TrustStore,
    watcher: ActionsWatcher,
    write_lock: AsyncMutex<()>,
    trust_lock: AsyncMutex<()>,
    /// Last valid document per path — what stays in force while the file
    /// on disk is malformed (§1.1).
    last_valid: Mutex<HashMap<PathBuf, Value>>,
}

impl ActionsManager {
    pub fn new(app: AppHandle, db: Arc<PaDb>, app_data_dir: PathBuf) -> Self {
        let home = crate::platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            watcher: ActionsWatcher::new(app.clone()),
            app,
            db,
            home,
            trust: TrustStore::new(&app_data_dir),
            write_lock: AsyncMutex::new(()),
            trust_lock: AsyncMutex::new(()),
            last_valid: Mutex::new(HashMap::new()),
        }
    }

    /// Starts the watchers for the personal and the active project's
    /// `.ikenga/` directories. Called at boot and on
    /// `projects:active-changed`.
    pub async fn refresh_watch(&self) -> Result<(), String> {
        let mut targets = vec![(
            scope::ikenga_dir(&self.home),
            SettingsScope::Personal,
        )];
        let project = self.resolve_project(None).await?;
        if let Some(root) = project.root.as_ref() {
            targets.push((scope::ikenga_dir(root), SettingsScope::Project));
        }
        self.watcher.set_targets(&targets)
    }

    async fn resolve_project(&self, project_id: Option<&str>) -> Result<ResolvedProject, String> {
        let pool = self.db.ensure_pool().await?;
        let project = resolve_project_scope(&pool, project_id).await?;
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

    fn remember(&self, path: &Path, raw: &RawRead) {
        let Ok(mut cache) = self.last_valid.lock() else {
            return;
        };
        if !raw.present && raw.error.is_none() {
            cache.remove(path);
        } else if let Some(document) = scope::valid_document(raw) {
            cache.insert(path.to_path_buf(), document.clone());
        }
    }

    fn file_state(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        path: &Path,
        raw: RawRead,
    ) -> FileState {
        self.remember(path, &raw);
        let valid = scope::valid_document(&raw).is_some();
        let (document, stale) = if valid || (!raw.present && raw.error.is_none()) {
            (raw.document.clone().filter(|_| valid), false)
        } else {
            let fallback = self
                .last_valid
                .lock()
                .ok()
                .and_then(|cache| cache.get(path).cloned());
            let stale = fallback.is_some();
            (fallback, stale)
        };
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
        if written {
            if let Ok(mut cache) = self.last_valid.lock() {
                cache.insert(path.clone(), document);
            }
            watch::emit_change(&self.app, &path, kind, scope);
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

    /// Creates the file with an empty valid skeleton when absent, then opens
    /// it with the OS handler. Returns the path.
    pub async fn open_file(
        &self,
        kind: FileKind,
        scope: SettingsScope,
        project_id: Option<&str>,
    ) -> Result<String, String> {
        let _guard = self.write_lock.lock().await;
        let project = self.resolve_project(project_id).await?;
        let path = self.path_for(kind, scope, Some(&project))?;
        if !path.exists() {
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
        open_path(&path)?;
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

        let actions_read = scope::read_raw(
            &scope::project_file(root, FileKind::Actions),
            ValidateContext {
                kind: FileKind::Actions,
                scope: SettingsScope::Project,
                user_action_ids: None,
            },
        );
        let actions_error = read_problem(&actions_read);
        let actions = scope::valid_document(&actions_read)
            .map(|document| trust::action_trust(document, pins))
            .unwrap_or_default();

        let keys_read = scope::read_raw(
            &scope::project_file(root, FileKind::Keybindings),
            ValidateContext {
                kind: FileKind::Keybindings,
                scope: SettingsScope::Project,
                user_action_ids: None,
            },
        );
        let keybindings_error = read_problem(&keys_read);
        // An invalid file is not in force, so nothing of it is trusted or
        // held; report it as absent with the error alongside.
        let keybindings = trust::keybindings_trust(scope::valid_document(&keys_read), pins);

        Ok(TrustStatus {
            project_id: id.to_string(),
            project_root: root_text,
            actions,
            actions_error,
            keybindings,
            keybindings_error,
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
        let entry = record.project_mut(&status.project_id, &status.project_root);
        trust::apply_grant(
            entry,
            &status.actions,
            &status.keybindings,
            &request.actions,
            request.keybindings.as_deref(),
        )?;
        self.trust.save(&record)?;
        self.status_for(&project, &record)
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
        self.status_for(&project, &record)
    }
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

fn open_path(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("explorer.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open {}: {e}", path.display()))
}
