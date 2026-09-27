//! DB-only pkg surfaces, shared by the desktop `#[tauri::command]`s and the
//! daemon's `/api/rpc` arms (WP-19 slice 6):
//!
//! * the `pkg_permission_violations` audit table (`commands::permissions_audit`,
//!   Phase 3 of `2026-05-15-runtime-acl-enforcement`) — newest-first list,
//!   optionally by pkg, and a per-pkg clear. Audit-only: clearing never alters
//!   trust state or re-grants anything.
//! * `pkg_db_diag` (`commands::pkg`) — which SQLite file the handle opened,
//!   and what `pkg_installed` holds, read straight from that file (never
//!   through the kernel), so the daemon's answer is as true as the desktop's:
//!   every field comes from the `PaDb` on both surfaces.
//!
//! Each takes the `PaDb` its caller resolved and opens the pool at the same
//! point the command always did, so both surfaces return the same value or
//! the same error.

use serde::Serialize;
use sqlx::Row;

use crate::db::PaDb;

// ─── permission violations ───────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug)]
pub struct ViolationRow {
    pub id: i64,
    pub pkg_id: String,
    pub scope_kind: String,
    pub attempted: String,
    pub declared: String,
    pub occurred_at: i64,
}

/// Default row cap — large enough for the Review dialog's "show me the
/// recent attempts" use case, small enough to keep payloads bounded when
/// the FE polls.
const DEFAULT_LIMIT: i64 = 100;
/// Hard ceiling — caps a misbehaving caller from yanking the entire table.
const MAX_LIMIT: i64 = 1000;

pub async fn violations_list(
    db: &PaDb,
    pkg_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<ViolationRow>, String> {
    let pool = db.ensure_pool().await?;
    let lim = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let rows = if let Some(id) = pkg_id {
        sqlx::query(
            "SELECT id, pkg_id, scope_kind, attempted, declared, occurred_at
             FROM pkg_permission_violations
             WHERE pkg_id = ?
             ORDER BY occurred_at DESC
             LIMIT ?",
        )
        .bind(&id)
        .bind(lim)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query(
            "SELECT id, pkg_id, scope_kind, attempted, declared, occurred_at
             FROM pkg_permission_violations
             ORDER BY occurred_at DESC
             LIMIT ?",
        )
        .bind(lim)
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| format!("query pkg_permission_violations: {e:#}"))?;

    Ok(rows
        .into_iter()
        .map(|r| ViolationRow {
            id: r.get::<i64, _>("id"),
            pkg_id: r.get::<String, _>("pkg_id"),
            scope_kind: r.get::<String, _>("scope_kind"),
            attempted: r.get::<String, _>("attempted"),
            declared: r.get::<String, _>("declared"),
            occurred_at: r.get::<i64, _>("occurred_at"),
        })
        .collect())
}

/// Delete `pkg_id`'s rows; the count deleted (`0` for an unknown pkg — the
/// desktop's answer, not an error).
pub async fn violations_clear(db: &PaDb, pkg_id: &str) -> Result<u64, String> {
    let pool = db.ensure_pool().await?;
    let result = sqlx::query("DELETE FROM pkg_permission_violations WHERE pkg_id = ?")
        .bind(pkg_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("delete pkg_permission_violations: {e:#}"))?;
    Ok(result.rows_affected())
}

// ─── pkg_db_diag ─────────────────────────────────────────────────────────────

/// Diagnostic: returns `(db_path, pkg_installed_count)` straight from the
/// kernel's PaDb handle. Used to confirm the kernel is reading the same
/// SQLite file as external tooling expects.
#[derive(Serialize, Debug)]
pub struct PkgDbDiag {
    pub db_path: String,
    pub pkg_installed_count: i64,
    pub ids: Vec<String>,
}

pub async fn db_diag(db: &PaDb) -> Result<PkgDbDiag, String> {
    let db_path = db.db_path_for_diag().display().to_string();
    let pool = db.ensure_pool().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pkg_installed")
        .fetch_one(&pool)
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM pkg_installed ORDER BY id")
        .fetch_all(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(PkgDbDiag {
        db_path,
        pkg_installed_count: count,
        ids,
    })
}
