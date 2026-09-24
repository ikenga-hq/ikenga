use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::settings::{SettingsManager, SettingsReadResult, SettingsScope};

#[tauri::command]
pub async fn settings_get(
    manager: State<'_, Arc<SettingsManager>>,
    key: String,
) -> Result<Option<String>, String> {
    manager.get_legacy(&key).await
}

#[tauri::command]
pub async fn settings_set(
    manager: State<'_, Arc<SettingsManager>>,
    key: String,
    value: String,
) -> Result<(), String> {
    manager.set_legacy(&key, &value).await
}

#[tauri::command]
pub async fn settings_get_all(
    manager: State<'_, Arc<SettingsManager>>,
) -> Result<HashMap<String, String>, String> {
    manager.get_all().await
}

#[tauri::command]
pub async fn settings_clear_all(manager: State<'_, Arc<SettingsManager>>) -> Result<(), String> {
    manager.clear_all().await
}

#[tauri::command]
pub async fn settings_read_file(
    manager: State<'_, Arc<SettingsManager>>,
    scope: Option<String>,
    project_id: Option<String>,
) -> Result<SettingsReadResult, String> {
    let scope = SettingsScope::parse(scope.as_deref().unwrap_or("project"))?;
    manager.read(scope, project_id.as_deref()).await
}

#[tauri::command]
pub async fn settings_write_field(
    manager: State<'_, Arc<SettingsManager>>,
    scope: String,
    field: String,
    value: Value,
    project_id: Option<String>,
    remove: Option<bool>,
) -> Result<SettingsReadResult, String> {
    let scope = SettingsScope::parse(&scope)?;
    manager
        .write_field(
            scope,
            project_id.as_deref(),
            &field,
            value,
            remove.unwrap_or(false),
        )
        .await
}

#[tauri::command]
pub async fn settings_open_file(
    manager: State<'_, Arc<SettingsManager>>,
    scope: String,
    project_id: Option<String>,
) -> Result<String, String> {
    let scope = SettingsScope::parse(&scope)?;
    manager.open_file(scope, project_id.as_deref()).await
}
