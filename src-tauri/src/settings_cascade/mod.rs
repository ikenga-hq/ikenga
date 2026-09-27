//! 5-tier settings cascade — the `#[tauri::command]`. The merge engine lives
//! in the ungated `server::shared::settings_cascade` (WP-19 slice 5b) so the
//! daemon's `/api/rpc` arm runs the same code; re-exported here so
//! `crate::settings_cascade::*` paths keep resolving.

use std::path::Path;

pub use crate::server::shared::settings_cascade::*;

/// Tauri command to resolve effective settings cascade for a project directory.
#[tauri::command]
pub fn claude_config_resolve_cascade(
    project_dir: Option<String>,
    overlay_dir: Option<String>,
) -> CascadeResult {
    let p_path = project_dir.as_ref().map(Path::new);
    let o_path = overlay_dir.as_ref().map(Path::new);
    resolve_settings_cascade(p_path, o_path)
}
