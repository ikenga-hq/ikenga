//! `engine_layout` Tauri command. The descriptor itself — `EngineLayout` and
//! the frozen per-engine matrix — lives in the ungated
//! `server::shared::engine_layout` (WP-19 slice 5b), so the daemon's
//! `/api/rpc` arm serves the very same data. Re-exported here so every
//! `crate::commands::engine_layout::*` path keeps resolving.

pub use crate::server::shared::engine_layout::*;

/// Tauri command — return the frozen layout descriptor for all engines so the
/// FE can fetch it live (avoids TS/Rust drift). Read-only; takes no args.
#[tauri::command]
pub fn engine_layout() -> Vec<EngineLayout> {
    engine_layouts()
}
