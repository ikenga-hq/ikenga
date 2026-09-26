//! WP-50: the actions / keybindings file layer and the project-trust record
//! (G-ACTIONS §1, §8.3). Typed client: `src/lib/actions/client.ts`.

use std::sync::Arc;

use serde_json::Value;
use tauri::State;

use crate::actions::schema::FileKind;
use crate::actions::{
    ActionsFilesResult, ActionsManager, ActionsWriteResult, TrustGrantRequest,
    TrustRevokeRequest, TrustStatus,
};
use crate::settings::SettingsScope;

/// Both scopes' `actions.json` + `keybindings.json`, validated, in load
/// order, plus the project keybindings trust (DEC-65).
#[tauri::command]
pub async fn actions_read_files(
    manager: State<'_, Arc<ActionsManager>>,
    project_id: Option<String>,
) -> Result<ActionsFilesResult, String> {
    manager.read_files(project_id.as_deref()).await
}

/// Validates and atomically writes a whole `actions.json`.
#[tauri::command]
pub async fn actions_write(
    manager: State<'_, Arc<ActionsManager>>,
    scope: String,
    document: Value,
    project_id: Option<String>,
) -> Result<ActionsWriteResult, String> {
    let scope = SettingsScope::parse(&scope)?;
    manager
        .write(FileKind::Actions, scope, project_id.as_deref(), document)
        .await
}

/// Validates and atomically writes a whole `keybindings.json`. A project
/// `scope: "os"` rule is refused with `E_OS_LAYER` (DEC-60).
#[tauri::command]
pub async fn keybindings_write(
    manager: State<'_, Arc<ActionsManager>>,
    scope: String,
    document: Value,
    project_id: Option<String>,
) -> Result<ActionsWriteResult, String> {
    let scope = SettingsScope::parse(&scope)?;
    manager
        .write(FileKind::Keybindings, scope, project_id.as_deref(), document)
        .await
}

/// Creates the file if absent (empty, valid) and opens it with the OS.
#[tauri::command]
pub async fn actions_open_file(
    manager: State<'_, Arc<ActionsManager>>,
    file: String,
    scope: String,
    project_id: Option<String>,
) -> Result<String, String> {
    let kind = FileKind::parse(&file)?;
    let scope = SettingsScope::parse(&scope)?;
    manager.open_file(kind, scope, project_id.as_deref()).await
}

#[tauri::command]
pub async fn actions_trust_status(
    manager: State<'_, Arc<ActionsManager>>,
    project_id: Option<String>,
) -> Result<TrustStatus, String> {
    manager.trust_status(project_id.as_deref()).await
}

#[tauri::command]
pub async fn actions_trust_grant(
    manager: State<'_, Arc<ActionsManager>>,
    request: TrustGrantRequest,
    project_id: Option<String>,
) -> Result<TrustStatus, String> {
    manager.trust_grant(project_id.as_deref(), request).await
}

#[tauri::command]
pub async fn actions_trust_revoke(
    manager: State<'_, Arc<ActionsManager>>,
    request: Option<TrustRevokeRequest>,
    project_id: Option<String>,
) -> Result<TrustStatus, String> {
    manager
        .trust_revoke(project_id.as_deref(), request.unwrap_or_default())
        .await
}
