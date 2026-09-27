//! Tauri commands for user-level activity-bar pinning — thin wrappers.
//!
//! Sections (user-created groups of pins) and pins (artifacts, routes,
//! files, external URLs, pkg-owned routes). The types, validation and
//! queries live in [`crate::server::shared::activity_bar`] (WP-19 slice 4),
//! shared with the daemon's `/api/rpc` arms; see that module's doc for the
//! domain. The types are re-exported so existing paths are unchanged.

use std::sync::Arc;

use tauri::State;

use super::db::PaDb;
use crate::server::shared::activity_bar as shared;

pub use shared::{Pin, Section};

// ─── Sections ──────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn activity_sections_list(db: State<'_, Arc<PaDb>>) -> Result<Vec<Section>, String> {
    shared::sections_list(&db).await
}

#[tauri::command]
pub async fn activity_sections_create(
    db: State<'_, Arc<PaDb>>,
    id: String,
    label: String,
    icon_lucide: Option<String>,
    icon_emoji: Option<String>,
) -> Result<Section, String> {
    shared::sections_create(&db, id, label, icon_lucide, icon_emoji).await
}

#[tauri::command]
pub async fn activity_sections_update(
    db: State<'_, Arc<PaDb>>,
    id: String,
    label: Option<String>,
    icon_lucide: Option<Option<String>>,
    icon_emoji: Option<Option<String>>,
) -> Result<Section, String> {
    shared::sections_update(&db, id, label, icon_lucide, icon_emoji).await
}

#[tauri::command]
pub async fn activity_sections_remove(db: State<'_, Arc<PaDb>>, id: String) -> Result<(), String> {
    shared::sections_remove(&db, id).await
}

// ─── Pins ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn activity_pins_list(db: State<'_, Arc<PaDb>>) -> Result<Vec<Pin>, String> {
    shared::pins_list(&db).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn activity_pins_add(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    target: String,
    label: String,
    icon_lucide: Option<String>,
    icon_emoji: Option<String>,
    section_id: Option<String>,
    manifest_id: Option<String>,
) -> Result<Pin, String> {
    shared::pins_add(
        &db,
        kind,
        target,
        label,
        icon_lucide,
        icon_emoji,
        section_id,
        manifest_id,
    )
    .await
}

/// Look up a pinned artifact by its manifest id (the `ikenga://artifact/<id>`
/// resolver). Does NOT bump `last_opened_at` — see `activity_pins_touch_open`.
#[tauri::command]
pub async fn activity_pins_resolve_artifact(
    db: State<'_, Arc<PaDb>>,
    manifest_id: String,
) -> Result<Option<Pin>, String> {
    shared::pins_resolve_artifact(&db, &manifest_id).await
}

/// Stamp `last_opened_at` to "now" for a pin.
#[tauri::command]
pub async fn activity_pins_touch_open(
    db: State<'_, Arc<PaDb>>,
    pin_id: String,
) -> Result<(), String> {
    shared::pins_touch_open(&db, &pin_id).await
}

#[tauri::command]
pub async fn activity_pins_remove(db: State<'_, Arc<PaDb>>, id: String) -> Result<(), String> {
    shared::pins_remove(&db, id).await
}

#[tauri::command]
pub async fn activity_pins_reorder(
    db: State<'_, Arc<PaDb>>,
    ordered_ids: Vec<String>,
    section_id: String,
) -> Result<(), String> {
    shared::pins_reorder(&db, ordered_ids, section_id).await
}
