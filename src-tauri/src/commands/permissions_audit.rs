//! Phase 3 of `2026-05-15-runtime-acl-enforcement` — read/clear surface for
//! the `pkg_permission_violations` audit table that Phase 2 writes into.
//!
//! Two commands:
//!   - `pkg_permission_violations_list` — newest-first rows, optionally
//!     filtered by pkg_id. Backs Settings → Pkgs Violations badge + the
//!     Review dialog table.
//!   - `pkg_permission_violations_clear` — deletes the named pkg's rows.
//!     Audit-only — does not alter trust state or re-grant anything.
//!
//! Both go through the same `db: State<Arc<PaDb>>` shape as `commands::trust`.
//! WP-19 slice 6: the SQL lives in `server::shared::pkg_db`, which the headless
//! daemon serves too; these are thin delegates.

use std::sync::Arc;

use tauri::State;

use crate::commands::db::PaDb;
use crate::server::shared::pkg_db;

pub use crate::server::shared::pkg_db::ViolationRow;

#[tauri::command]
pub async fn pkg_permission_violations_list(
    db: State<'_, Arc<PaDb>>,
    pkg_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<ViolationRow>, String> {
    pkg_db::violations_list(&db, pkg_id, limit).await
}

#[tauri::command]
pub async fn pkg_permission_violations_clear(
    db: State<'_, Arc<PaDb>>,
    pkg_id: String,
) -> Result<u64, String> {
    pkg_db::violations_clear(&db, &pkg_id).await
}

#[cfg(test)]
mod tests {
    use crate::pkg::permissions_check::{record_violation, ShellExecuteDenied};
    use sqlx::Row;

    /// Bring up an in-memory pool with the violations table, write a few
    /// rows via the Phase 2 writer, and verify the read shape + ordering.
    async fn fresh_pool() -> sqlx::SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory db");
        sqlx::query(include_str!(
            "../../migrations/0020_pkg_permission_violations.sql"
        ))
        .execute(&pool)
        .await
        .expect("create table");
        pool
    }

    #[tokio::test]
    async fn list_returns_newest_first() {
        let pool = fresh_pool().await;
        for cmd in ["a", "b", "c"] {
            record_violation(
                &pool,
                "shell.execute",
                &ShellExecuteDenied {
                    pkg_id: "p".into(),
                    command: cmd.into(),
                    declared: "x".into(),
                },
            )
            .await
            .expect("record");
            // 1 ms gap so occurred_at orders deterministically without
            // relying on insert order.
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        let rows = sqlx::query(
            "SELECT id, pkg_id, scope_kind, attempted, declared, occurred_at
             FROM pkg_permission_violations
             ORDER BY occurred_at DESC LIMIT 100",
        )
        .fetch_all(&pool)
        .await
        .expect("read back");
        let attempted: Vec<String> = rows
            .into_iter()
            .map(|r| r.get::<String, _>("attempted"))
            .collect();
        assert_eq!(attempted, vec!["c", "b", "a"]);
    }

    #[tokio::test]
    async fn clear_only_deletes_named_pkg() {
        let pool = fresh_pool().await;
        for pkg in ["p1", "p2", "p1"] {
            record_violation(
                &pool,
                "shell.execute",
                &ShellExecuteDenied {
                    pkg_id: pkg.into(),
                    command: "x".into(),
                    declared: "y".into(),
                },
            )
            .await
            .expect("record");
        }
        let deleted = sqlx::query("DELETE FROM pkg_permission_violations WHERE pkg_id = 'p1'")
            .execute(&pool)
            .await
            .expect("delete")
            .rows_affected();
        assert_eq!(deleted, 2);
        let remaining: i64 =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pkg_permission_violations")
                .fetch_one(&pool)
                .await
                .expect("count");
        assert_eq!(remaining, 1);
    }
}
