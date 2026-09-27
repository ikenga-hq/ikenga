//! Tauri command surface for the multi-window substrate (plans/multi-window
//! WP-03). Thin wrappers over `window::WindowRegistry`. Mirrored by the typed
//! wrappers in `src/lib/tauri-cmd.ts`.

use tauri::{AppHandle, State};

use crate::window::descriptor::WindowDescriptor;
use crate::window::registry::WindowRegistry;

/// Spawn a labeled window from a descriptor. Returns the window label.
#[tauri::command]
pub fn window_spawn(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
    descriptor: WindowDescriptor,
) -> Result<String, String> {
    registry.spawn(&app, descriptor).map_err(|e| e.to_string())
}

/// Close a spawned window by label (`main` is refused).
#[tauri::command]
pub fn window_close(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
    label: String,
) -> Result<(), String> {
    registry.close(&app, &label).map_err(|e| e.to_string())
}

/// List descriptors of all currently-spawned windows. Reconciles against the
/// OS first so a window whose `Destroyed` event was missed can't linger as a
/// permanent ghost.
#[tauri::command]
pub fn window_list(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
) -> Vec<WindowDescriptor> {
    registry.list_live(&app)
}

/// WP-69 (G-SEATS §4.4, DEC-69d): *Pop out* joins Window 2. Picks Window 2
/// (pin P-7: the most recently focused live non-`main` window, excluding
/// `Workspace` windows bound to a project other than `project_id`) and adds
/// `surface_id` to it as a tab. Returns that window's label, or `null` when
/// there is no Window 2 — the caller then spawns one with `window_spawn`.
/// The pick and the add are one call, so the chosen window can't close in
/// between. The FE focuses the returned label itself (`WebviewWindow` lookup
/// by label, then `setFocus`).
#[tauri::command]
pub fn window_join_surface(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
    surface_id: String,
    project_id: Option<String>,
) -> Result<Option<String>, String> {
    registry
        .join_surface(&app, &surface_id, project_id.as_deref())
        .map_err(|e| e.to_string())
}

/// WP-69: take `surface_id` out of the detached window `label` — *Move back
/// to main window* (`move_back: true`), or the primary's *Open in pane* /
/// "Bring it back" reclaiming one tab of a multi-surface window. The window
/// closes when it held nothing else. Returns its `surface_set` after.
#[tauri::command]
pub fn window_remove_surface(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
    label: String,
    surface_id: String,
    move_back: Option<bool>,
) -> Result<Vec<String>, String> {
    registry
        .remove_surface(&app, &label, &surface_id, move_back.unwrap_or(false))
        .map_err(|e| e.to_string())
}
