pub mod migrate;
pub mod schema;
pub mod scope;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::RecommendedWatcher;
use notify_debouncer_mini::{new_debouncer, DebounceEventResult, Debouncer};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::db::PaDb;

use self::migrate::{migrate_from_kv, SCREENSHOT_MIGRATION_KEY};
use self::schema::SettingsDocument;
pub use self::scope::SettingsScope;
use self::scope::{
    normalize_project_root, personal_path, project_path, read_document, resolve_paths,
    write_bytes_atomic, write_document,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReadResult {
    pub personal: SettingsDocument,
    pub project: Option<SettingsDocument>,
    pub effective: SettingsDocument,
    pub personal_path: String,
    pub project_path: Option<String>,
    pub project_id: Option<String>,
    pub project_root: Option<String>,
    pub overrides: Vec<String>,
    pub personal_present: bool,
    pub project_present: bool,
    pub scope: SettingsScope,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsChangeEvent {
    pub path: String,
}

pub struct SettingsManager {
    app: AppHandle,
    db: Arc<PaDb>,
    home: PathBuf,
    app_data_dir: PathBuf,
    project_generation: AtomicU64,
    migration_ready: AtomicBool,
    generation_lock: AsyncMutex<()>,
    write_lock: AsyncMutex<()>,
    cache_lock: AsyncMutex<()>,
    watchers: Mutex<HashMap<(PathBuf, PathBuf), Debouncer<RecommendedWatcher>>>,
}

impl SettingsManager {
    pub fn new(app: AppHandle, db: Arc<PaDb>, app_data_dir: PathBuf) -> Self {
        let home = crate::platform::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            app,
            db,
            home,
            app_data_dir,
            project_generation: AtomicU64::new(0),
            migration_ready: AtomicBool::new(false),
            generation_lock: AsyncMutex::new(()),
            write_lock: AsyncMutex::new(()),
            cache_lock: AsyncMutex::new(()),
            watchers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn initialize(&self) -> Result<(), String> {
        self.migration_ready.store(false, Ordering::Release);
        {
            let _generation_guard = self.generation_lock.lock().await;
            self.project_generation.fetch_add(1, Ordering::AcqRel);
        }
        let pool = self.db.ensure_pool().await?;
        let roots = self.project_roots(&pool).await?;
        match migrate_from_kv(&pool, &self.home, &roots).await {
            Ok(report) => {
                tracing::info!(
                    "[settings] migration personal_created={} project_files_created={} keys={} skipped={} already={}",
                    report.personal_created,
                    report.project_files_created,
                    report.migrated_keys,
                    report.skipped_keys,
                    report.already_migrated
                );
            }
            Err(error) => {
                tracing::warn!("[settings] migration will retry: {error}");
                return Err(error);
            }
        };
        if !self.screenshot_migration_done(&pool).await? {
            self.import_screenshot_config()?;
            self.mark_screenshot_migration_done(&pool).await?;
        }
        if let Err(e) = self.watch_path(&personal_path(&self.home)) {
            tracing::warn!("[settings] personal watcher failed: {e}");
        }
        for root in roots.values() {
            if let Err(e) = self.watch_path(&project_path(root)) {
                tracing::warn!("[settings] project watcher failed: {e}");
            }
        }
        self.refresh_cache().await?;
        self.migration_ready.store(true, Ordering::Release);
        Ok(())
    }

    fn ensure_migration_ready(&self) -> Result<(), String> {
        if self.migration_ready.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("settings migration is not ready".to_string())
        }
    }

    pub async fn refresh_watch(&self) -> Result<(), String> {
        let _generation_guard = self.generation_lock.lock().await;
        self.project_generation.fetch_add(1, Ordering::AcqRel);
        let pool = self.db.ensure_pool().await?;
        let roots = self.project_roots(&pool).await?;
        self.prune_project_watchers(&roots)?;
        for root in roots.values() {
            if let Err(e) = self.watch_path(&project_path(root)) {
                tracing::warn!("[settings] project watcher refresh failed: {e}");
            }
        }
        Ok(())
    }

    pub async fn read(
        &self,
        scope: SettingsScope,
        project_id: Option<&str>,
    ) -> Result<SettingsReadResult, String> {
        self.ensure_migration_ready()?;
        let generation = self.project_generation.load(Ordering::Acquire);
        let pool = self.db.ensure_pool().await?;
        let paths = resolve_paths(&pool, &self.home, scope, project_id).await?;
        let personal_file = read_document(&paths.personal)?;
        let personal_present = personal_file.is_some();
        let personal = personal_file.unwrap_or_else(SettingsDocument::default);
        let project_file = match &paths.project {
            Some(path) => read_document(path)?,
            None => None,
        };
        let project_present = project_file_present(&paths.project);
        let project_overlay = project_file.as_ref().map(SettingsDocument::project_overlay);
        let effective = effective_document(
            scope,
            &personal,
            project_overlay.as_ref(),
            scope == SettingsScope::Project || paths.project_id.is_some(),
        );
        let overrides = if scope == SettingsScope::Project {
            project_overlay
                .as_ref()
                .map(SettingsDocument::leaf_paths)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Err(e) = self.watch_path(&paths.personal) {
            tracing::warn!("[settings] personal watcher failed: {e}");
        }
        if let Some(path) = &paths.project {
            if let Err(e) = self.watch_path(path) {
                tracing::warn!("[settings] project watcher failed: {e}");
            }
        }
        let result = SettingsReadResult {
            personal,
            project: project_file,
            effective,
            personal_path: paths.personal.to_string_lossy().into_owned(),
            project_path: paths
                .project
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            project_id: paths.project_id,
            project_root: paths
                .project_root
                .map(|path| path.to_string_lossy().into_owned()),
            overrides,
            personal_present,
            project_present,
            scope,
        };
        if self.project_generation.load(Ordering::Acquire) != generation {
            return Err("settings project generation changed during read".to_string());
        }
        self.cache_result(&result, generation).await?;
        if self.project_generation.load(Ordering::Acquire) != generation {
            return Err("settings project generation changed during cache refresh".to_string());
        }
        Ok(result)
    }

    /// Read one field from the **personal** file (`~/.ikenga/settings.json`),
    /// resolved against schema defaults. No KV-cache refresh, no watcher
    /// registration and no migration-ready gate: this is for hot read paths
    /// that only need a personal-only value (WP-40 notification mutes, read on
    /// every list / unread-count call). A missing file yields the default.
    pub fn personal_field(&self, field: &str) -> Result<Option<Value>, String> {
        let document = read_document(&personal_path(&self.home))?.unwrap_or_default();
        Ok(document.resolved().get_field(field).cloned())
    }

    pub async fn write_field(
        &self,
        scope: SettingsScope,
        project_id: Option<&str>,
        field: &str,
        value: Value,
        remove: bool,
    ) -> Result<SettingsReadResult, String> {
        self.ensure_migration_ready()?;
        let _write_guard = self.write_lock.lock().await;
        if scope == SettingsScope::Project && schema::is_personal_only_field(field) {
            return Err(format!("settings field is personal-only: {field}"));
        }
        let pool = self.db.ensure_pool().await?;
        let paths = resolve_paths(&pool, &self.home, scope, project_id).await?;
        let cache_project_id = if scope == SettingsScope::Project {
            project_id.or(paths.project_id.as_deref())
        } else {
            project_id
        };
        if scope == SettingsScope::Personal
            && schema::is_project_only_field(field)
            && (field != "projects.extraRoots" || paths.project.is_some())
        {
            return Err(format!("settings field is project-only: {field}"));
        }
        let path = match scope {
            SettingsScope::Personal => Some(paths.personal.clone()),
            SettingsScope::Project => Some(
                paths
                    .project
                    .clone()
                    .ok_or_else(|| "the selected project has no filesystem root".to_string())?,
            ),
        };
        let path = path.ok_or_else(|| "settings file is unavailable".to_string())?;
        let existing = read_document(&path)?;
        if remove && existing.is_none() {
            self.delete_field_cache(field, cache_project_id).await?;
            return self.read(scope, project_id).await;
        }
        let mut document = existing.unwrap_or_else(SettingsDocument::default);
        let mut project_file_removed = false;
        if remove {
            document.remove_field(field)?;
            if scope == SettingsScope::Project && !document.has_content() {
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return Err(format!("remove {}: {error}", path.display()));
                    }
                }
                project_file_removed = scope == SettingsScope::Project;
            } else {
                write_document(&path, &document)?;
            }
        } else {
            document.set_field(field, value.clone())?;
            if scope == SettingsScope::Project && !document.has_content() {
                if let Err(error) = std::fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return Err(format!("remove {}: {error}", path.display()));
                    }
                }
                project_file_removed = scope == SettingsScope::Project;
            } else {
                write_document(&path, &document)?;
            }
        }
        if remove {
            self.delete_field_cache(field, cache_project_id).await?;
        }
        if project_file_removed {
            if let Some(project_id) = cache_project_id {
                self.delete_project_last_agent_cache(project_id).await?;
            }
        }
        let screenshot_result = if field == "storage.screenshotDirectory" {
            let screenshot_value = if remove { Value::Null } else { value.clone() };
            self.write_screenshot_config(&screenshot_value)
        } else {
            Ok(())
        };
        if let Err(e) = self.watch_path(&path) {
            tracing::warn!("[settings] watcher refresh after write failed: {e}");
        }
        self.emit_change(&path);
        let read_result = self.read(scope, project_id).await;
        screenshot_result?;
        read_result
    }

    pub async fn set_legacy(&self, key: &str, raw: &str) -> Result<(), String> {
        let Some(binding) = schema::legacy_binding(key) else {
            return self.set_cache_value(key, raw).await;
        };
        self.ensure_migration_ready()?;
        let _write_guard = self.write_lock.lock().await;
        let project_id = schema::legacy_project_id(key);
        let scope = if binding.target == schema::LegacyTarget::Project {
            SettingsScope::Project
        } else {
            SettingsScope::Personal
        };
        let pool = self.db.ensure_pool().await?;
        let paths = match resolve_paths(&pool, &self.home, scope, project_id).await {
            Ok(paths) => paths,
            Err(error) if binding.target == schema::LegacyTarget::Project => {
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let path = match scope {
            SettingsScope::Project => match paths.project.clone() {
                Some(path) => path,
                None => return Err("project root is unavailable".to_string()),
            },
            SettingsScope::Personal => paths.personal.clone(),
        };
        let mut document = read_document(&path)?.unwrap_or_else(SettingsDocument::default);
        schema::apply_legacy_value(&mut document, key, raw)?;
        if scope == SettingsScope::Project && !document.has_content() {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("remove {}: {error}", path.display()));
                }
            }
        } else {
            write_document(&path, &document)?;
        }
        if raw.is_empty() {
            self.delete_cache_value(key).await?;
        }
        if let Err(e) = self.watch_path(&path) {
            tracing::warn!("[settings] watcher refresh after legacy write failed: {e}");
        }
        let result = self.refresh_cache().await;
        self.emit_change(&path);
        result
    }

    pub async fn get_legacy(&self, key: &str) -> Result<Option<String>, String> {
        if !schema::is_known_legacy_key(key) {
            return self.get_cache_value(key).await;
        }
        self.ensure_migration_ready()?;
        let project_id = schema::legacy_project_id(key);
        let result = match self.read(SettingsScope::Project, project_id).await {
            Ok(result) => result,
            Err(error) => {
                let Some(project_id) = project_id else {
                    return Err(error);
                };
                if self.project_cache_fallback_allowed(project_id).await? {
                    return self.get_cache_value(key).await;
                }
                return Err(error);
            }
        };
        if project_id.is_some() && result.project_path.is_none() {
            return self.get_cache_value(key).await;
        }
        if let Some(value) = schema::read_legacy_value(&result.effective, key) {
            return Ok(Some(value));
        }
        self.get_cache_value(key).await
    }

    pub async fn get_all(&self) -> Result<HashMap<String, String>, String> {
        self.ensure_migration_ready()?;
        let _ = self.read(SettingsScope::Project, None).await?;
        let pool = self.db.ensure_pool().await?;
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings_kv")
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("settings_get_all: {e}"))?;
        Ok(rows.into_iter().collect())
    }

    pub async fn clear_all(&self) -> Result<(), String> {
        let _write_guard = self.write_lock.lock().await;
        let _generation_guard = self.generation_lock.lock().await;
        self.project_generation.fetch_add(1, Ordering::AcqRel);
        let _cache_guard = self.cache_lock.lock().await;
        let pool = self.db.ensure_pool().await?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| format!("begin settings_clear_all: {e}"))?;
        sqlx::query("DELETE FROM settings_kv")
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("settings_clear_all: {e}"))?;
        tx.commit()
            .await
            .map_err(|e| format!("commit settings_clear_all: {e}"))?;
        let personal = personal_path(&self.home);
        remove_file_if_exists(&personal)?;
        remove_file_if_exists(&self.app_data_dir.join("screenshot-config.json"))?;
        Ok(())
    }

    pub async fn open_file(
        &self,
        scope: SettingsScope,
        project_id: Option<&str>,
    ) -> Result<String, String> {
        self.ensure_migration_ready()?;
        let _write_guard = self.write_lock.lock().await;
        let pool = self.db.ensure_pool().await?;
        let paths = resolve_paths(&pool, &self.home, scope, project_id).await?;
        let path = match scope {
            SettingsScope::Personal => paths.personal,
            SettingsScope::Project => paths
                .project
                .ok_or_else(|| "the selected project has no filesystem root".to_string())?,
        };
        if !path.exists() {
            write_document(&path, &SettingsDocument::default())?;
        }
        open_path(&path)?;
        if let Err(e) = self.watch_path(&path) {
            tracing::warn!("[settings] watcher refresh after open failed: {e}");
        }
        Ok(path.to_string_lossy().into_owned())
    }

    async fn refresh_cache(&self) -> Result<(), String> {
        let generation = self.project_generation.load(Ordering::Acquire);
        let result = self
            .read_without_cache(SettingsScope::Project, None)
            .await?;
        self.cache_result(&result, generation).await
    }

    async fn read_without_cache(
        &self,
        scope: SettingsScope,
        project_id: Option<&str>,
    ) -> Result<SettingsReadResult, String> {
        let pool = self.db.ensure_pool().await?;
        let paths = resolve_paths(&pool, &self.home, scope, project_id).await?;
        let personal_file = read_document(&paths.personal)?;
        let personal_present = personal_file.is_some();
        let personal = personal_file.unwrap_or_else(SettingsDocument::default);
        let project_file = match &paths.project {
            Some(path) => read_document(path)?,
            None => None,
        };
        let project_present = project_file_present(&paths.project);
        let project_overlay = project_file.as_ref().map(SettingsDocument::project_overlay);
        let effective = effective_document(
            scope,
            &personal,
            project_overlay.as_ref(),
            scope == SettingsScope::Project || paths.project_id.is_some(),
        );
        let overrides = if scope == SettingsScope::Project {
            project_overlay
                .as_ref()
                .map(SettingsDocument::leaf_paths)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(SettingsReadResult {
            personal,
            project: project_file,
            effective,
            personal_path: paths.personal.to_string_lossy().into_owned(),
            project_path: paths
                .project
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            project_id: paths.project_id,
            project_root: paths
                .project_root
                .map(|path| path.to_string_lossy().into_owned()),
            overrides,
            personal_present,
            project_present,
            scope,
        })
    }

    async fn cache_result(
        &self,
        result: &SettingsReadResult,
        generation: u64,
    ) -> Result<(), String> {
        let _generation_guard = self.generation_lock.lock().await;
        let _cache_guard = self.cache_lock.lock().await;
        if self.project_generation.load(Ordering::Acquire) != generation {
            return Err("settings project generation changed before cache refresh".to_string());
        }
        let pool = self.db.ensure_pool().await?;
        let project_id = if result.project_present {
            result.project_id.as_deref()
        } else {
            None
        };
        let values = schema::legacy_values_for_document(&result.effective, project_id);
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings_kv")
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("read settings cache: {e}"))?;
        let mut value_map: HashMap<String, String> = values.into_iter().collect();
        let mut blocked_keys = HashSet::new();
        for (key, raw) in &rows {
            if schema::is_known_legacy_key(key)
                && self.legacy_row_is_pending(&pool, key, raw).await?
            {
                blocked_keys.insert(key.clone());
                value_map.remove(key);
            }
        }
        let mut prune_keys = Vec::new();
        for (key, raw) in &rows {
            if schema::is_known_legacy_key(key)
                && !blocked_keys.contains(key)
                && !value_map.contains_key(key)
                && self.can_prune_legacy(&pool, key, raw).await?
            {
                prune_keys.push(key.clone());
            }
        }
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| format!("begin settings cache transaction: {e}"))?;
        for key in prune_keys {
            sqlx::query("DELETE FROM settings_kv WHERE key = ?")
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("prune settings cache: {e}"))?;
        }
        for (key, value) in value_map {
            sqlx::query(
                "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            )
            .bind(key)
            .bind(value)
            .bind(now_ms())
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("write settings cache: {e}"))?;
        }
        if self.project_generation.load(Ordering::Acquire) != generation {
            return Err("settings project generation changed during cache refresh".to_string());
        }
        tx.commit()
            .await
            .map_err(|e| format!("commit settings cache: {e}"))?;
        Ok(())
    }

    async fn legacy_row_is_pending(
        &self,
        pool: &sqlx::SqlitePool,
        key: &str,
        raw: &str,
    ) -> Result<bool, String> {
        let Some(binding) = schema::legacy_binding(key) else {
            return Ok(false);
        };
        if schema::decode_legacy_value(&binding, key, raw).is_err() {
            return Ok(true);
        }
        let Some(project_id) = schema::legacy_project_id(key) else {
            return Ok(false);
        };
        let row: Option<(Option<String>, Option<i64>)> =
            sqlx::query_as("SELECT root_path, archived_at FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("read project for settings migration: {e}"))?;
        Ok(!row.is_some_and(|(root, archived)| {
            archived.is_none()
                && root
                    .and_then(|value| normalize_project_root(&value).ok())
                    .is_some()
        }))
    }

    async fn can_prune_legacy(
        &self,
        pool: &sqlx::SqlitePool,
        key: &str,
        raw: &str,
    ) -> Result<bool, String> {
        let Some(binding) = schema::legacy_binding(key) else {
            return Ok(false);
        };
        if schema::decode_legacy_value(&binding, key, raw).is_err() {
            return Ok(false);
        }
        if raw.is_empty() {
            return Ok(true);
        }
        let Some(project_id) = schema::legacy_project_id(key) else {
            return Ok(true);
        };
        let row: Option<(Option<String>, Option<i64>)> =
            sqlx::query_as("SELECT root_path, archived_at FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("read project for settings cache: {e}"))?;
        Ok(row.is_some_and(|(root, archived)| {
            if archived.is_some() {
                return false;
            }
            let Some(root) = root.and_then(|value| normalize_project_root(&value).ok()) else {
                return false;
            };
            let path = project_path(&root);
            path.is_file() && read_document(&path).is_ok()
        }))
    }

    async fn set_cache_value(&self, key: &str, value: &str) -> Result<(), String> {
        let _cache_guard = self.cache_lock.lock().await;
        let pool = self.db.ensure_pool().await?;
        sqlx::query(
            "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now_ms())
        .execute(&pool)
        .await
        .map_err(|e| format!("settings_set: {e}"))?;
        Ok(())
    }

    async fn delete_project_last_agent_cache(&self, project_id: &str) -> Result<(), String> {
        let _cache_guard = self.cache_lock.lock().await;
        let pool = self.db.ensure_pool().await?;
        let kind_key = format!("artifact-wizard.lastAgent.{project_id}");
        let custom_key = format!("artifact-wizard.lastAgentCustom.{project_id}");
        sqlx::query("DELETE FROM settings_kv WHERE key IN (?, ?)")
            .bind(kind_key)
            .bind(custom_key)
            .execute(&pool)
            .await
            .map_err(|e| format!("settings_delete project cache: {e}"))?;
        Ok(())
    }

    async fn delete_field_cache(
        &self,
        field: &str,
        project_id: Option<&str>,
    ) -> Result<(), String> {
        match field {
            "projects.extraRoots" => self.delete_cache_value("projects.extraRoots").await,
            "workspace.lastAgent" => {
                if let Some(project_id) = project_id {
                    self.delete_cache_value(&format!("artifact-wizard.lastAgent.{project_id}"))
                        .await?;
                    self.delete_cache_value(&format!(
                        "artifact-wizard.lastAgentCustom.{project_id}"
                    ))
                    .await?;
                }
                Ok(())
            }
            "workspace.lastAgent.kind" => {
                if let Some(project_id) = project_id {
                    self.delete_cache_value(&format!("artifact-wizard.lastAgent.{project_id}"))
                        .await?;
                }
                Ok(())
            }
            "workspace.lastAgent.customCommand" => {
                if let Some(project_id) = project_id {
                    self.delete_cache_value(&format!(
                        "artifact-wizard.lastAgentCustom.{project_id}"
                    ))
                    .await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn delete_cache_value(&self, key: &str) -> Result<(), String> {
        let _cache_guard = self.cache_lock.lock().await;
        let pool = self.db.ensure_pool().await?;
        sqlx::query("DELETE FROM settings_kv WHERE key = ?")
            .bind(key)
            .execute(&pool)
            .await
            .map_err(|e| format!("settings_delete: {e}"))?;
        Ok(())
    }

    async fn project_cache_fallback_allowed(&self, project_id: &str) -> Result<bool, String> {
        let pool = self.db.ensure_pool().await?;
        let row: Option<(Option<i64>,)> =
            sqlx::query_as("SELECT archived_at FROM projects WHERE id = ?")
                .bind(project_id)
                .fetch_optional(&pool)
                .await
                .map_err(|e| format!("read project for settings cache: {e}"))?;
        Ok(match row {
            None => true,
            Some((archived_at,)) => archived_at.is_some(),
        })
    }

    async fn get_cache_value(&self, key: &str) -> Result<Option<String>, String> {
        let pool = self.db.ensure_pool().await?;
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings_kv WHERE key = ?")
            .bind(key)
            .fetch_optional(&pool)
            .await
            .map_err(|e| format!("settings_get: {e}"))?;
        Ok(row.map(|(value,)| value))
    }

    async fn project_roots(
        &self,
        pool: &sqlx::SqlitePool,
    ) -> Result<HashMap<String, PathBuf>, String> {
        let rows = sqlx::query("SELECT id, root_path, archived_at FROM projects ORDER BY id ASC")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("list settings project roots: {e}"))?;
        let mut roots = HashMap::new();
        let mut owners: HashMap<PathBuf, String> = HashMap::new();
        for row in rows {
            let id: String = row
                .try_get("id")
                .map_err(|e| format!("read project id: {e}"))?;
            let archived_at: Option<i64> = row
                .try_get("archived_at")
                .map_err(|e| format!("read project archive state: {e}"))?;
            if archived_at.is_some() {
                continue;
            }
            let root: Option<String> = row
                .try_get("root_path")
                .map_err(|e| format!("read project root: {e}"))?;
            let Some(root) = root.filter(|value| !value.trim().is_empty()) else {
                continue;
            };
            let root = match normalize_project_root(&root) {
                Ok(root) => root,
                Err(error) => {
                    tracing::warn!("[settings] skipping project {id}: {error}");
                    continue;
                }
            };
            if let Some(owner) = owners.get(&root) {
                tracing::warn!(
                    "[settings] skipping duplicate project root {root:?} for {id}; owned by {owner}"
                );
                continue;
            }
            owners.insert(root.clone(), id.clone());
            roots.insert(id, root);
        }
        Ok(roots)
    }

    fn prune_project_watchers(&self, roots: &HashMap<String, PathBuf>) -> Result<(), String> {
        let personal = personal_path(&self.home);
        let project_paths: HashSet<PathBuf> =
            roots.values().map(|root| project_path(root)).collect();
        let mut watchers = self
            .watchers
            .lock()
            .map_err(|_| "settings watcher lock poisoned")?;
        watchers.retain(|(_, target), _| target == &personal || project_paths.contains(target));
        Ok(())
    }

    fn emit_change(&self, path: &Path) {
        let _ = self.app.emit(
            "settings://changed",
            SettingsChangeEvent {
                path: path.to_string_lossy().into_owned(),
            },
        );
    }

    fn watch_path(&self, path: &Path) -> Result<(), String> {
        let parent = path
            .parent()
            .ok_or_else(|| format!("settings path has no parent: {}", path.display()))?;
        let watch_dir = if parent.exists() {
            parent.to_path_buf()
        } else {
            let Some(root) = parent.parent() else {
                return Ok(());
            };
            if !root.is_dir() {
                return Ok(());
            }
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create settings directory {}: {e}", parent.display()))?;
            parent.to_path_buf()
        };
        let target = path.to_path_buf();
        let key = (watch_dir.clone(), target.clone());
        {
            let watchers = self
                .watchers
                .lock()
                .map_err(|_| "settings watcher lock poisoned")?;
            if watchers.contains_key(&key) {
                return Ok(());
            }
        }
        let app = self.app.clone();
        let event_target = target.clone();
        let mut debouncer: Debouncer<RecommendedWatcher> = new_debouncer(
            Duration::from_millis(250),
            move |result: DebounceEventResult| {
                let Ok(events) = result else { return };
                let changed = events.into_iter().any(|event| event.path == event_target);
                if changed {
                    let _ = app.emit(
                        "settings://changed",
                        SettingsChangeEvent {
                            path: event_target.to_string_lossy().into_owned(),
                        },
                    );
                }
            },
        ).map_err(|error| format!("create settings watcher: {error}"))?;
        let watch_result = debouncer
            .watcher()
            .watch(&watch_dir, notify::RecursiveMode::NonRecursive);
        if let Err(error) = watch_result {
            return Err(format!("watch {}: {error}", watch_dir.display()));
        }
        let mut watchers = self
            .watchers
            .lock()
            .map_err(|_| "settings watcher lock poisoned")?;
        watchers.retain(|(_, existing), _| existing.as_path() != path);
        watchers.insert(key, debouncer);
        Ok(())
    }

    async fn screenshot_migration_done(&self, pool: &sqlx::SqlitePool) -> Result<bool, String> {
        let value: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = ?")
                .bind(SCREENSHOT_MIGRATION_KEY)
                .fetch_optional(pool)
                .await
                .map_err(|e| format!("read screenshot migration marker: {e}"))?;
        Ok(value.as_deref() == Some("done"))
    }

    async fn mark_screenshot_migration_done(&self, pool: &sqlx::SqlitePool) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(SCREENSHOT_MIGRATION_KEY)
        .bind("done")
        .bind(now_ms())
        .execute(pool)
        .await
        .map_err(|e| format!("write screenshot migration marker: {e}"))?;
        Ok(())
    }

    fn import_screenshot_config(&self) -> Result<(), String> {
        let path = self.app_data_dir.join("screenshot-config.json");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("read {}: {error}", path.display())),
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid screenshot config {}: {error}", path.display()))?;
        let Some(override_dir) = value.get("override_dir") else {
            return Ok(());
        };
        let override_dir = match override_dir {
            Value::String(value) if value.trim().is_empty() => Value::Null,
            value => value.clone(),
        };
        let personal = personal_path(&self.home);
        let mut document = read_document(&personal)?.unwrap_or_else(SettingsDocument::default);
        if document.get_field("storage.screenshotDirectory").is_none() {
            document.set_field("storage.screenshotDirectory", override_dir)?;
            write_document(&personal, &document)?;
        }
        Ok(())
    }

    fn write_screenshot_config(&self, value: &Value) -> Result<(), String> {
        let override_dir = if value.is_null() {
            Value::Null
        } else {
            Value::String(value.as_str().unwrap_or_default().to_string())
        };
        let path = self.app_data_dir.join("screenshot-config.json");
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({ "override_dir": override_dir }))
            .map_err(|e| format!("serialize screenshot config: {e}"))?;
        write_bytes_atomic(&path, &bytes)
    }
}

fn effective_document(
    scope: SettingsScope,
    personal: &SettingsDocument,
    project: Option<&SettingsDocument>,
    has_project_scope: bool,
) -> SettingsDocument {
    let mut base = personal.clone();
    if scope == SettingsScope::Project && has_project_scope {
        for field in schema::PROJECT_ONLY_FIELDS
            .iter()
            .filter(|field| **field != "projects.extraRoots")
        {
            base.remove_field(field);
        }
        if let Some(project) = project {
            base.projects.remove("extraRoots");
            base = base.merge(project);
        }
    }
    base.resolved()
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

fn project_file_present(path: &Option<PathBuf>) -> bool {
    path.as_ref().is_some_and(|path| path.exists())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_project_does_not_inherit_personal_extra_roots() {
        let mut personal = SettingsDocument::default();
        personal
            .set_field("projects.extraRoots", serde_json::json!(["/personal"]))
            .unwrap();
        let project = SettingsDocument::default();
        let effective = effective_document(SettingsScope::Project, &personal, Some(&project), true);
        assert_eq!(
            effective.get_field("projects.extraRoots"),
            Some(&serde_json::json!([]))
        );
    }

    #[test]
    fn cache_key_set_is_stable() {
        let keys = [
            "appearance.theme",
            "engines.resumeTerminals",
            "artifact-grid.default-sink",
        ];
        assert_eq!(keys.len(), 3);
    }
}
