//! Data-health commands (Ngwa → Health → Data).
//!
//! The scan and the file-size measurement live in
//! `crate::server::shared::data_health` (the soft-FK registry, the anti-join,
//! the `OrphanReport` / `DbFileSizes` wire types and their tests), shared with
//! the daemon's RPC arms. These wrappers only pull the managed `PaDb`.

use std::sync::Arc;

use tauri::State;

use crate::commands::db::PaDb;
use crate::server::shared::data_health as shared;
pub use crate::server::shared::data_health::{measure_db_files, DbFileSizes, OrphanReport};

/// Read-only orphan audit across the Atelier/PA domain soft links. Runs on the
/// dedicated reader pool (same path as `db_query`); never writes. Returns one
/// [`OrphanReport`] per soft link that currently has dangling references.
#[tauri::command]
pub async fn data_health_scan(db: State<'_, Arc<PaDb>>) -> Result<Vec<OrphanReport>, String> {
    shared::scan(&db).await
}

/// DEC-32: report the database file sizes for Ngwa → Health → Data. Stats the
/// files only; does not touch the connection pools.
#[tauri::command]
pub async fn data_health_db_size(db: State<'_, Arc<PaDb>>) -> Result<DbFileSizes, String> {
    shared::db_size(&db)
}
