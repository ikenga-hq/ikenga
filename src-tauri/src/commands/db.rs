//! The two SQLite `#[tauri::command]` entry points.
//!
//! Everything they stand on — the [`PaDb`] handle, the embedded migration
//! set, the read-only guard, and the row-to-JSON conversion — lives in
//! [`crate::db`], which is compiled into **both** binaries. The headless
//! daemon serves the same two commands over `/api/rpc`
//! (`server::rpc::rpc_handler`) from that shared implementation, so the two
//! surfaces cannot drift.
//!
//! [`PaDb`] is re-exported here so the ~45 `crate::commands::db::PaDb` call
//! sites across the crate resolve unchanged — the same shape as
//! `commands::path_allow` re-exporting `crate::path_allow`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Manager, State};

pub use crate::db::PaDb;

#[tauri::command]
pub async fn db_query(
    db: State<'_, Arc<PaDb>>,
    sql: String,
    params: Vec<Value>,
) -> Result<Vec<Value>, String> {
    crate::db::query_json(&db, &sql, &params).await
}

/// Returns `()` rather than [`crate::db::ExecResult`] on purpose: the typed
/// wrapper in `src/lib/tauri-cmd.ts` declares `dbExec(): Promise<void>` and no
/// caller on the Tauri path reads a result. The daemon's `db_exec` RPC arm
/// *does* return the `{rowsAffected, lastInsertId}` object, because the
/// browser transport's `sql-shim.ts` types it as `SqlQueryResult`.
#[tauri::command]
pub async fn db_exec(
    db: State<'_, Arc<PaDb>>,
    sql: String,
    params: Vec<Value>,
) -> Result<(), String> {
    crate::db::exec(&db, &sql, &params).await?;
    Ok(())
}

#[allow(dead_code)]
pub fn default_db_path<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    Ok(dir.join("ikenga.db"))
}
