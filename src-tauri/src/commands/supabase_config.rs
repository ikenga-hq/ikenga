//! Supabase project config: URL + anon key.
//!
//! These two values are not secret — Supabase explicitly designs the anon key
//! for client-side use, protected by RLS rather than secrecy. Stronghold is
//! not the right home for them (boot ordering: we'd need them before the vault
//! is unlocked). Instead we keep a tiny non-secret JSON manifest at
//! `app_data_dir/supabase.json`.
//!
//! The manifest is the single source of truth: missing manifest → app boots
//! into the setup wizard; present manifest → the FE reads URL + anon key,
//! pulls `SUPABASE_SERVICE_ROLE_KEY` from Stronghold, and creates the client.
//!
//! The file logic is `crate::server::shared::supabase_config`, shared with the
//! daemon's RPC arms (which root it at `--data-dir`); these commands only
//! resolve `app_data_dir` and delegate.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};

use crate::server::shared::supabase_config as shared;
pub use crate::server::shared::supabase_config::SupabaseConfig;

fn data_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))
}

#[tauri::command]
pub async fn supabase_config_get(app: AppHandle) -> Result<Option<SupabaseConfig>, String> {
    shared::get(&data_dir(&app)?)
}

#[tauri::command]
pub async fn supabase_config_set(
    app: AppHandle,
    url: String,
    anon_key: String,
    service_role_key: Option<String>,
) -> Result<(), String> {
    shared::set(&data_dir(&app)?, url, anon_key, service_role_key)
}

#[tauri::command]
pub async fn supabase_config_clear(app: AppHandle) -> Result<(), String> {
    shared::clear(&data_dir(&app)?)
}
