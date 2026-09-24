use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::Row;

use super::schema::SettingsDocument;

pub const ACTIVE_PROJECT_KEY: &str = "shell.activeProjectId";
pub const DEFAULT_PROJECT_ID: &str = "default";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SettingsScope {
    Personal,
    Project,
}

impl SettingsScope {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "personal" => Ok(Self::Personal),
            "project" => Ok(Self::Project),
            _ => Err(format!("unknown settings scope: {value}")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProjectScope {
    pub id: String,
    pub root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SettingsPaths {
    pub personal: PathBuf,
    pub project: Option<PathBuf>,
    pub project_id: Option<String>,
    pub project_root: Option<PathBuf>,
}

pub fn personal_path(home: &Path) -> PathBuf {
    home.join(".ikenga").join("settings.json")
}

pub fn project_path(root: &Path) -> PathBuf {
    root.join(".ikenga").join("settings.json")
}

pub fn normalize_project_root(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("project root is empty".to_string());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(format!("project root is not absolute: {trimmed}"));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("project root contains parent traversal: {trimmed}"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if normalized.parent().is_none() {
        return Err(format!("project root is a filesystem root: {trimmed}"));
    }
    match std::fs::canonicalize(&normalized) {
        Ok(canonical) => {
            if canonical.parent().is_none() {
                return Err(format!("project root is a filesystem root: {trimmed}"));
            }
            if !canonical.is_dir() {
                return Err(format!("project root is not a directory: {trimmed}"));
            }
            Ok(canonical)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("project root does not exist: {trimmed}"))
        }
        Err(error) => Err(format!("cannot resolve project root {trimmed}: {error}")),
    }
}

pub async fn resolve_project_scope(
    pool: &sqlx::SqlitePool,
    project_id: Option<&str>,
) -> Result<ProjectScope, String> {
    let requested = project_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let id = match requested.as_deref() {
        Some(id) => id.to_string(),
        None => sqlx::query_scalar::<_, String>("SELECT value FROM settings_kv WHERE key = ?")
            .bind(ACTIVE_PROJECT_KEY)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("read active project: {e}"))?
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PROJECT_ID.to_string()),
    };
    let row = sqlx::query("SELECT id, root_path, archived_at FROM projects WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read project {id}: {e}"))?;
    let Some(row) = row else {
        if requested.is_some() {
            return Err(format!("project not found: {id}"));
        }
        return Ok(ProjectScope {
            id: DEFAULT_PROJECT_ID.to_string(),
            root: None,
        });
    };
    let actual_id: String = row
        .try_get("id")
        .map_err(|e| format!("read project id: {e}"))?;
    let archived_at: Option<i64> = row
        .try_get("archived_at")
        .map_err(|e| format!("read project archive state: {e}"))?;
    if archived_at.is_some() {
        if requested.is_some() {
            return Err(format!("project is archived: {id}"));
        }
        return Ok(ProjectScope {
            id: DEFAULT_PROJECT_ID.to_string(),
            root: None,
        });
    }
    let root: Option<String> = row
        .try_get("root_path")
        .map_err(|e| format!("read project root: {e}"))?;
    let root = root.filter(|value| !value.trim().is_empty());
    if requested.is_some() {
        if let Some(value) = root.as_ref() {
            if !Path::new(value.trim()).is_dir() {
                return Err(format!("project root is unavailable: {id}"));
            }
        }
    }
    let root = match root {
        Some(value) if Path::new(value.trim()).is_dir() => normalize_project_root(&value).ok(),
        _ => None,
    };
    if let Some(root) = root.as_ref() {
        if project_root_is_duplicate(pool, &actual_id, root).await? {
            if requested.is_some() {
                return Err(format!(
                    "project root is shared by another active project: {}",
                    root.display()
                ));
            }
            return Ok(ProjectScope {
                id: DEFAULT_PROJECT_ID.to_string(),
                root: None,
            });
        }
    }
    Ok(ProjectScope {
        id: actual_id,
        root,
    })
}

async fn project_root_is_duplicate(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    root: &Path,
) -> Result<bool, String> {
    let rows = sqlx::query("SELECT id, root_path, archived_at FROM projects WHERE id != ?")
        .bind(project_id)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("list project roots for settings: {e}"))?;
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
        let other_root: Option<String> = row
            .try_get("root_path")
            .map_err(|e| format!("read project root: {e}"))?;
        let Some(other_root) = other_root.filter(|value| !value.trim().is_empty()) else {
            continue;
        };
        if normalize_project_root(&other_root).ok().as_deref() == Some(root) {
            tracing::warn!(
                "[settings] project {id} shares settings root {}",
                root.display()
            );
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn resolve_paths(
    pool: &sqlx::SqlitePool,
    home: &Path,
    scope: SettingsScope,
    project_id: Option<&str>,
) -> Result<SettingsPaths, String> {
    let project_scope = if scope == SettingsScope::Project || project_id.is_some() {
        Some(resolve_project_scope(pool, project_id).await?)
    } else {
        None
    };
    let project_root = project_scope.as_ref().and_then(|value| value.root.clone());
    let project = project_root.as_ref().map(|root| project_path(root));
    Ok(SettingsPaths {
        personal: personal_path(home),
        project,
        project_id: project_scope.map(|value| value.id),
        project_root,
    })
}

pub fn read_document(path: &Path) -> Result<Option<SettingsDocument>, String> {
    recover_orphan_backup(path)?;
    match std::fs::read(path) {
        Ok(bytes) => SettingsDocument::parse(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

pub fn write_document(path: &Path, document: &SettingsDocument) -> Result<(), String> {
    let bytes = document.to_bytes()?;
    write_bytes_atomic(path, &bytes)
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    recover_orphan_backup(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("settings path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("settings path has no file name: {}", path.display()))?;
    let temp = parent.join(format!(
        ".{name}.{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    ));
    let write_result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|e| format!("create {}: {e}", temp.display()))?;
        std::io::Write::write_all(&mut file, bytes)
            .map_err(|e| format!("write {}: {e}", temp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0o600);
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|e| format!("set permissions {}: {e}", temp.display()))?;
        }
        file.sync_all()
            .map_err(|e| format!("sync {}: {e}", temp.display()))?;
        replace_path(&temp, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result
}

fn recovery_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    Some(parent.join(format!(".{name}.recovery")))
}

fn recover_orphan_backup(path: &Path) -> Result<(), String> {
    let Some(backup) = recovery_path(path) else {
        return Ok(());
    };
    if !backup.exists() {
        return Ok(());
    }
    if path.exists() {
        let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        SettingsDocument::parse(&bytes)
            .map_err(|e| format!("settings {} is invalid: {e}", path.display()))?;
        std::fs::remove_file(&backup)
            .map_err(|e| format!("remove recovery {}: {e}", backup.display()))?;
        return Ok(());
    }
    let bytes =
        std::fs::read(&backup).map_err(|e| format!("read recovery {}: {e}", backup.display()))?;
    SettingsDocument::parse(&bytes)
        .map_err(|e| format!("recovery {} is invalid: {e}", backup.display()))?;
    std::fs::rename(&backup, path)
        .map_err(|e| format!("recover {} to {}: {e}", backup.display(), path.display()))
}

fn replace_path(temp: &Path, path: &Path) -> Result<(), String> {
    recover_orphan_backup(path)?;
    match std::fs::rename(temp, path) {
        Ok(()) => Ok(()),
        Err(first_error) if path.exists() => {
            let backup = recovery_path(path)
                .ok_or_else(|| format!("settings path has no recovery name: {}", path.display()))?;
            std::fs::rename(path, &backup).map_err(|backup_error| {
                format!(
                    "replace {}: {first_error}; backup: {backup_error}",
                    path.display()
                )
            })?;
            match std::fs::rename(temp, path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(backup);
                    Ok(())
                }
                Err(second_error) => {
                    let _ = std::fs::rename(&backup, path);
                    Err(format!(
                        "replace {}: {second_error}; restored previous file",
                        path.display()
                    ))
                }
            }
        }
        Err(error) => Err(format!("replace {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_use_ikenga_dot_directory() {
        let home = Path::new("home");
        let root = Path::new("project");
        assert_eq!(personal_path(home), Path::new("home/.ikenga/settings.json"));
        assert_eq!(
            project_path(root),
            Path::new("project/.ikenga/settings.json")
        );
    }

    #[test]
    fn document_write_round_trips_through_atomic_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(".ikenga").join("settings.json");
        let mut document = SettingsDocument::default();
        document
            .set_field("appearance.theme", serde_json::Value::String("C".into()))
            .unwrap();
        write_document(&path, &document).unwrap();
        let read = read_document(&path).unwrap().unwrap();
        assert_eq!(read.appearance.get("theme").unwrap().as_str(), Some("C"));
    }

    #[test]
    fn project_roots_reject_relative_and_parent_traversal() {
        assert!(normalize_project_root("relative/project").is_err());
        assert!(normalize_project_root("/tmp/../project").is_err());
    }

    #[test]
    fn project_roots_reject_missing_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        assert!(normalize_project_root(temp.path().join("missing").to_str().unwrap()).is_err());
    }

    #[test]
    fn orphaned_recovery_file_is_restored_on_read() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(".ikenga").join("settings.json");
        write_document(&path, &SettingsDocument::default()).unwrap();
        let backup = recovery_path(&path).unwrap();
        std::fs::rename(&path, &backup).unwrap();
        assert!(read_document(&path).unwrap().is_some());
    }

    #[test]
    fn invalid_recovery_file_is_not_consumed() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join(".ikenga").join("settings.json");
        let backup = recovery_path(&path).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&backup, b"not-json").unwrap();
        assert!(read_document(&path).is_err());
        assert!(backup.exists());
    }

    #[test]
    fn scope_parser_rejects_unknown_scope() {
        assert_eq!(
            SettingsScope::parse("personal").unwrap(),
            SettingsScope::Personal
        );
        assert_eq!(
            SettingsScope::parse("project").unwrap(),
            SettingsScope::Project
        );
        assert!(SettingsScope::parse("workspace").is_err());
    }
}
