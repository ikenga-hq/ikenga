//! Ngwa in-shell scaffolding engine (WP-23 / locked design D-02) — the Tauri
//! command.
//!
//! The engine moved to the ungated [`crate::server::shared::pkg_scaffold`]
//! (WP-19 slice 8), shared with the daemon's `/api/rpc` arm; see that
//! module's doc. It is re-exported here, so every existing path
//! (`commands::scaffold::{PkgScaffoldParams, execute_scaffold, …}`) resolves.

use std::sync::Arc;

use tauri::State;

use super::db::PaDb;
pub use crate::server::shared::pkg_scaffold::*;

/// Tauri command entrypoint: scaffold an equipment item.
#[tauri::command]
pub async fn pkg_scaffold(
    db: State<'_, Arc<PaDb>>,
    params: PkgScaffoldParams,
) -> Result<PkgScaffoldResult, String> {
    let (folder, primary) = resolve_destination(&db, &params).await?;
    let files = execute_scaffold(&params, &folder, &primary)?;

    Ok(result(params, &folder, &primary, files))
}
