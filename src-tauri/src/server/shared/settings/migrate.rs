use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::schema::{self, SettingsDocument};
use super::scope::{personal_path, project_path, read_document, write_document};

pub const FILE_MIGRATION_KEY: &str = "settings.migrations.file-v1";
pub const SHELL_MIGRATION_KEY: &str = "settings.migrations.shell-v17";
pub const SCREENSHOT_MIGRATION_KEY: &str = "settings.migrations.screenshot-v1";

#[derive(Debug, Default)]
pub struct MigrationReport {
    pub personal_created: bool,
    pub project_files_created: usize,
    pub migrated_keys: usize,
    pub skipped_keys: usize,
    pub already_migrated: bool,
}

pub async fn migrate_from_kv(
    pool: &sqlx::SqlitePool,
    home: &Path,
    project_roots: &HashMap<String, PathBuf>,
) -> Result<MigrationReport, String> {
    let marker: Option<String> = sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = ?")
        .bind(FILE_MIGRATION_KEY)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read settings migration marker: {e}"))?;
    let mut rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings_kv ORDER BY key ASC")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("read settings_kv for migration: {e}"))?;
    let personal_file = personal_path(home);
    if let Some((project_id, root)) = project_roots.iter().find(|(_, root)| !root.is_dir()) {
        return Err(format!(
            "project root is unavailable for migration: {project_id} {}",
            root.display()
        ));
    }
    if marker.as_deref() == Some("done") && read_document(&personal_file)?.is_some() {
        let mut pending = Vec::new();
        let mut repair = false;
        for (key, raw) in &rows {
            let Some(binding) = schema::legacy_binding(key) else {
                continue;
            };
            if schema::decode_legacy_value(&binding, key, raw).is_err() {
                pending.push(key.clone());
                continue;
            }
            if binding.target == schema::LegacyTarget::Project {
                let Some(project_id) = schema::legacy_project_id(key) else {
                    pending.push(key.clone());
                    continue;
                };
                let Some(root) = project_roots.get(project_id) else {
                    pending.push(key.clone());
                    continue;
                };
                let path = project_path(root);
                let represented = match read_document(&path) {
                    Ok(Some(document)) if raw.is_empty() => {
                        !schema::legacy_value_present(&document, key)
                    }
                    Ok(Some(document)) => schema::legacy_value_present(&document, key),
                    _ => false,
                };
                if !represented {
                    repair = true;
                }
            }
        }
        if !pending.is_empty() {
            return Err(format!(
                "settings migration pending for: {}",
                pending.join(", ")
            ));
        }
        if !repair {
            return Ok(MigrationReport {
                personal_created: false,
                already_migrated: true,
                ..MigrationReport::default()
            });
        }
    }
    rows.sort_by_key(|(key, _)| match key.as_str() {
        "agent.defaultEngineId" => 0,
        "agent.chatAdapterId" => 1,
        _ => 2,
    });

    let personal_existing = read_document(&personal_file)?;
    let mut personal = personal_existing
        .clone()
        .unwrap_or_else(SettingsDocument::default);
    let mut personal_changed = false;
    let mut report = MigrationReport {
        personal_created: personal_existing.is_none(),
        ..MigrationReport::default()
    };

    let mut pending_keys: Vec<String> = Vec::new();
    let mut project_documents: HashMap<String, (PathBuf, SettingsDocument, bool, bool)> =
        HashMap::new();
    for (project_id, root) in project_roots {
        let path = project_path(root);
        let existing = read_document(&path)?;
        let created = existing.is_none();
        let document = existing.unwrap_or_else(SettingsDocument::default);
        project_documents.insert(project_id.clone(), (path, document, created, false));
    }

    for (key, raw) in rows {
        if key == "agent.chatAdapterId"
            && pending_keys
                .iter()
                .any(|value| value == "agent.defaultEngineId")
        {
            pending_keys.push(key);
            continue;
        }
        let Some(binding) = schema::legacy_binding(&key) else {
            continue;
        };
        let project_id = match binding.target {
            schema::LegacyTarget::Personal => None,
            schema::LegacyTarget::Project => {
                let project_id = schema::legacy_project_id(&key);
                if project_id.is_none() {
                    report.skipped_keys += 1;
                    pending_keys.push(key);
                    continue;
                }
                project_id
            }
        };
        let result = if let Some(project_id) = project_id {
            let Some(entry) = project_documents.get_mut(project_id) else {
                report.skipped_keys += 1;
                pending_keys.push(key);
                continue;
            };
            let applied = apply_row(&mut entry.1, &key, &raw);
            let did_apply = matches!(applied.as_ref(), Ok(&true));
            if did_apply {
                entry.3 = true;
            }
            applied
        } else {
            apply_row(&mut personal, &key, &raw)
        };
        match result {
            Ok(true) => {
                report.migrated_keys += 1;
                if binding.target == schema::LegacyTarget::Personal {
                    personal_changed = true;
                }
            }
            Ok(false) => report.skipped_keys += 1,
            Err(_) => {
                report.skipped_keys += 1;
                pending_keys.push(key);
            }
        }
    }

    if report.personal_created || personal_changed {
        write_document(&personal_file, &personal)?;
    }

    for (_, (path, document, created, changed)) in project_documents {
        if !document.has_content() {
            if changed && !created {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
            }
            continue;
        }
        if changed || created {
            write_document(&path, &document)?;
            if created {
                report.project_files_created += 1;
            }
        }
    }
    if !pending_keys.is_empty() {
        return Err(format!(
            "settings migration pending for: {}",
            pending_keys.join(", ")
        ));
    }
    sqlx::query(
        "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(FILE_MIGRATION_KEY)
    .bind("done")
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(|e| format!("write settings migration marker: {e}"))?;
    Ok(report)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn apply_row(document: &mut SettingsDocument, key: &str, raw: &str) -> Result<bool, String> {
    if document_exists(document, key) {
        return Ok(false);
    }
    schema::apply_legacy_value(document, key, raw)
}

fn document_exists(document: &SettingsDocument, key: &str) -> bool {
    schema::legacy_value_present(document, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_file_wins_over_legacy_row() {
        let mut document = SettingsDocument::default();
        schema::apply_legacy_value(&mut document, "user.name", r#""file""#).unwrap();
        assert!(document_exists(&document, "user.name"));
    }

    #[tokio::test]
    async fn kv_rows_are_written_to_the_personal_document() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("user.name")
            .bind(r#""Ada""#)
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("appearance.theme")
            .bind(r#""C""#)
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let roots = HashMap::new();
        migrate_from_kv(&pool, home.path(), &roots).await.unwrap();
        let document = read_document(&personal_path(home.path())).unwrap().unwrap();
        assert_eq!(
            document.get_field("workspace.userName").unwrap().as_str(),
            Some("Ada")
        );
        assert_eq!(
            document.appearance.get("theme").unwrap().as_str(),
            Some("C")
        );
        std::fs::remove_file(personal_path(home.path())).unwrap();
        migrate_from_kv(&pool, home.path(), &roots).await.unwrap();
        let recovered = read_document(&personal_path(home.path())).unwrap().unwrap();
        assert_eq!(
            recovered.get_field("workspace.userName").unwrap().as_str(),
            Some("Ada")
        );
        let mut edited = recovered;
        edited.remove_field("workspace.userName").unwrap();
        write_document(&personal_path(home.path()), &edited).unwrap();
        migrate_from_kv(&pool, home.path(), &roots).await.unwrap();
        let after = read_document(&personal_path(home.path())).unwrap().unwrap();
        assert!(after.get_field("workspace.userName").is_none());
    }

    #[tokio::test]
    async fn default_engine_alias_wins_over_chat_alias() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("agent.chatAdapterId")
            .bind(r#""chat""#)
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("agent.defaultEngineId")
            .bind(r#""default""#)
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        migrate_from_kv(&pool, home.path(), &HashMap::new())
            .await
            .unwrap();
        let document = read_document(&personal_path(home.path())).unwrap().unwrap();
        assert_eq!(
            document
                .get_field("engines.defaultEngineId")
                .unwrap()
                .as_str(),
            Some("default")
        );
    }

    #[tokio::test]
    async fn migration_does_not_create_empty_project_files() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("artifact-wizard.lastAgent.project")
            .bind("")
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        let roots = HashMap::from([("project".to_string(), project.path().to_path_buf())]);
        migrate_from_kv(&pool, home.path(), &roots).await.unwrap();
        assert!(!project
            .path()
            .join(".ikenga")
            .join("settings.json")
            .exists());
    }

    #[tokio::test]
    async fn done_marker_repairs_a_missing_project_field() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind(FILE_MIGRATION_KEY)
            .bind("done")
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("artifact-wizard.lastAgent.project")
            .bind("codex")
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let project = tempfile::TempDir::new().unwrap();
        write_document(&personal_path(home.path()), &SettingsDocument::default()).unwrap();
        write_document(&project_path(project.path()), &SettingsDocument::default()).unwrap();
        let roots = HashMap::from([("project".to_string(), project.path().to_path_buf())]);
        migrate_from_kv(&pool, home.path(), &roots).await.unwrap();
        let document = read_document(&project_path(project.path()))
            .unwrap()
            .unwrap();
        assert_eq!(
            document
                .get_field("workspace.lastAgent.kind")
                .unwrap()
                .as_str(),
            Some("codex")
        );
    }

    #[tokio::test]
    async fn malformed_row_keeps_the_file_migration_open() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("user.name")
            .bind("not-json")
            .bind(1i64)
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        assert!(migrate_from_kv(&pool, home.path(), &HashMap::new())
            .await
            .is_err());
        let marker: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = ?")
                .bind(FILE_MIGRATION_KEY)
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert!(marker.is_none());
        let preserved: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = ?")
                .bind("user.name")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(preserved.as_deref(), Some("not-json"));
    }

    #[tokio::test]
    async fn migration_rejects_a_missing_project_root() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at INTEGER NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let missing = home.path().join("missing");
        let roots = HashMap::from([("project".to_string(), missing.clone())]);
        assert!(migrate_from_kv(&pool, home.path(), &roots).await.is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn malformed_legacy_row_is_not_fatal_to_other_keys() {
        let mut document = SettingsDocument::default();
        assert!(schema::apply_legacy_value(&mut document, "user.name", "not-json").is_err());
        assert!(schema::apply_legacy_value(&mut document, "appearance.theme", r#""C""#).is_ok());
        assert_eq!(
            document.appearance.get("theme").unwrap().as_str(),
            Some("C")
        );
    }
}
