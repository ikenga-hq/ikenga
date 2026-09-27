//! Tauri commands for artifact comments (pin-mode) — thin wrappers.
//!
//! Persistence + lifecycle live in [`crate::server::shared::comments`]
//! (WP-19 slice 4), shared with the daemon's `/api/rpc` arms; see that
//! module's doc. What stays here is desktop-only: `pin_screenshot_write`,
//! which writes under `app_data_dir`. Sink-routing is `comment_route`.
//! `Comment` is re-exported so existing paths are unchanged.

use std::sync::Arc;

use base64::Engine;
use tauri::{AppHandle, Manager, Runtime, State};
use uuid::Uuid;

use super::db::PaDb;
use crate::server::shared::comments as shared;

pub use shared::Comment;

#[tauri::command]
pub async fn comment_create(
    db: State<'_, Arc<PaDb>>,
    artifact_path: String,
    selector: String,
    text: String,
    screenshot_path: Option<String>,
    position_x: Option<f64>,
    position_y: Option<f64>,
) -> Result<Comment, String> {
    shared::create(
        &db,
        artifact_path,
        selector,
        text,
        screenshot_path,
        position_x,
        position_y,
    )
    .await
}

#[tauri::command]
pub async fn comment_get(db: State<'_, Arc<PaDb>>, id: i64) -> Result<Comment, String> {
    shared::get(&db, id).await
}

#[tauri::command]
pub async fn comment_list(
    db: State<'_, Arc<PaDb>>,
    artifact_path: Option<String>,
    include_resolved: Option<bool>,
) -> Result<Vec<Comment>, String> {
    shared::list(&db, artifact_path, include_resolved).await
}

/// Update the routing audit fields after the dispatcher has decided where the
/// pin went. Called by the routing dispatcher once the prompt is queued.
#[tauri::command]
pub async fn comment_record_routing(
    db: State<'_, Arc<PaDb>>,
    id: i64,
    sink: String,
    thread_id: Option<String>,
    opening_session_id: Option<String>,
) -> Result<Comment, String> {
    shared::record_routing(&db, id, sink, thread_id, opening_session_id).await
}

/// Set the status (stamping `acknowledged_at` / `resolved_at` on the first
/// move into `in_progress` / `resolved`).
#[tauri::command]
pub async fn comment_set_status(
    db: State<'_, Arc<PaDb>>,
    id: i64,
    status: String,
) -> Result<Comment, String> {
    shared::set_status(&db, id, status).await
}

/// Persist a base64-encoded PNG (produced by `captureToPng` on the FE) to
/// `$app_data_dir/pin-screenshots/<uuid>.png` and return the absolute path
/// for storage in `artifact_comments.screenshot_path`. The element-picker
/// captures a *cropped* PNG of the right-clicked element; this command is
/// the FE → on-disk handoff so the path can be referenced by Claude (via
/// `mcp-iyke.pin_read`) without re-decoding base64 every read.
#[tauri::command]
pub async fn pin_screenshot_write<R: Runtime>(
    app: AppHandle<R>,
    base64_png: String,
) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_png.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    // PNG magic: 89 50 4E 47 0D 0A 1A 0A. Reject anything else early so a
    // corrupt blob can't poison the screenshots dir with junk files.
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG (bad magic)".into());
    }
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .join(shared::SCREENSHOTS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let path = dir.join(format!("{}.png", Uuid::new_v4()));
    std::fs::write(&path, &bytes).map_err(|e| format!("write png: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn comment_delete(db: State<'_, Arc<PaDb>>, id: i64) -> Result<(), String> {
    shared::delete(&db, id).await
}
