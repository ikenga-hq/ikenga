//! Tauri commands for unified artifact-studio chat threads — thin wrappers.
//!
//! One thread per folder; messages carry their scope chip. The types and
//! queries live in [`crate::server::shared::studio_threads`] (WP-19 slice 4),
//! shared with the daemon's `/api/rpc` arms; see that module's doc. The types
//! are re-exported so existing paths are unchanged.

use std::sync::Arc;

use tauri::State;

use super::db::PaDb;
use crate::server::shared::studio_threads as shared;

pub use shared::{StudioMessage, StudioThread};

/// Idempotent: returns the existing thread for this folder or creates one.
#[tauri::command]
pub async fn studio_thread_get_or_create(
    db: State<'_, Arc<PaDb>>,
    folder_path: String,
) -> Result<StudioThread, String> {
    shared::thread_get_or_create(&db, folder_path).await
}

#[tauri::command]
pub async fn studio_thread_get(
    db: State<'_, Arc<PaDb>>,
    id: String,
) -> Result<StudioThread, String> {
    shared::thread_get(&db, id).await
}

/// Recently active threads, most recent `last_message_at` first.
#[tauri::command]
pub async fn studio_thread_list_recent(
    db: State<'_, Arc<PaDb>>,
    limit: Option<i64>,
) -> Result<Vec<StudioThread>, String> {
    shared::thread_list_recent(&db, limit).await
}

/// Append a message to a thread; bumps the parent's `last_message_at`.
#[tauri::command]
pub async fn studio_message_append(
    db: State<'_, Arc<PaDb>>,
    thread_id: String,
    role: String,
    content_md: String,
    scope_chip_json: Option<String>,
) -> Result<StudioMessage, String> {
    shared::message_append(&db, thread_id, role, content_md, scope_chip_json).await
}

/// Messages in a thread, oldest first.
#[tauri::command]
pub async fn studio_message_list(
    db: State<'_, Arc<PaDb>>,
    thread_id: String,
    limit: Option<i64>,
    before_created_at: Option<i64>,
) -> Result<Vec<StudioMessage>, String> {
    shared::message_list(&db, thread_id, limit, before_created_at).await
}

/// Delete a thread and all its messages (ON DELETE CASCADE).
#[tauri::command]
pub async fn studio_thread_delete(db: State<'_, Arc<PaDb>>, id: String) -> Result<(), String> {
    shared::thread_delete(&db, id).await
}
