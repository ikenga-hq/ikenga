//! Data-health: read-only orphan detection for the Atelier/PA domain tables.
//!
//! The `0025`–`0054` migrations are the SQLite down-map of the old `royalti-pa`
//! Supabase schema. They declare **zero** `FOREIGN KEY` clauses — every
//! cross-domain relationship is a plain `TEXT` "soft link" resolved at query
//! time (see the migration-header comments, e.g. `0025_tasks_domain.sql:18`,
//! `0028_sales_gtm_domain.sql:12`, `0053_research_domain.sql:9`). Nothing in the
//! schema (no `REFERENCES`, no CHECK, no trigger) stops a parent row from being
//! deleted out from under a child that points at it, so a `sales_deals` row can
//! silently strand a dangling `research_item_id` and nothing errors or warns.
//!
//! This module mirrors the established `pkg/kernel.rs::scan_health` +
//! `pkg-health-panel.tsx` precedent — a read-only scan that *surfaces* orphaned
//! soft-linked rows and leaves the fix to a human. It never auto-deletes: these
//! are real business records (a stray deal, a task) the user may want to repair,
//! not install metadata. Decision doc: `plans/atelier-parity/07-fk-orphan-audit.md`
//! (Option B).
//!
//! **The `SOFT_LINKS` table below is hand-maintained.** There is no schema-level
//! way to derive "this `TEXT` column is a soft FK" automatically, so every future
//! soft-FK migration must add its `(child_table, child_column, parent_table)`
//! triple here or the new dangling reference won't be caught.

use std::sync::Arc;

use serde::Serialize;
use sqlx::Row;
use tauri::State;

use crate::commands::db::PaDb;

/// A hand-maintained soft foreign-key: `child_table.child_column` conceptually
/// references `parent_table.id`, but no DB constraint enforces it. Derived by
/// reading the migration-header soft-link comments across `0025`–`0054` and
/// mapping each documented cross-domain / self-reference `..._id TEXT` column to
/// the table it was meant to point at. The parent column is always `id` (every
/// domain table uses `id TEXT PRIMARY KEY`).
struct SoftLink {
    child_table: &'static str,
    child_column: &'static str,
    parent_table: &'static str,
}

/// The soft-FK registry. One entry per documented cross-domain / self-reference
/// link whose parent table exists in the schema. Grouped by the migration that
/// declares the child column; the header comment of each cited file documents
/// the dropped FK. Polymorphic discriminator columns (e.g.
/// `outbound_sent_log.source_id`, `research_notes.next_action_target`) are
/// deliberately excluded — a single anti-join can't validate a column that may
/// point at several different tables.
const SOFT_LINKS: &[SoftLink] = &[
    // ── tasks domain (0025_tasks_domain.sql) ──────────────────────────────
    // Header line 18 names `tasks.initiative_id → strategic_initiatives`
    // explicitly. parent_task_id / blocked_by_task_id are self-references
    // (0025:54-55). risk_id → risk_register is inferred: risk_register (0030)
    // is the only risk table in the down-map.
    SoftLink { child_table: "tasks", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "tasks", child_column: "parent_task_id", parent_table: "tasks" },
    SoftLink { child_table: "tasks", child_column: "blocked_by_task_id", parent_table: "tasks" },
    SoftLink { child_table: "tasks", child_column: "risk_id", parent_table: "risk_register" },
    SoftLink { child_table: "delegations", child_column: "task_id", parent_table: "tasks" },
    SoftLink { child_table: "notifications", child_column: "task_id", parent_table: "tasks" },
    // ── sales / GTM domain (0028_sales_gtm_domain.sql) ────────────────────
    // Header line 12 names `initiative_id → strategic_initiatives` and
    // `deal_id → sales_deals` explicitly. fundraising_activities /
    // fundraising_outreach.deal_id point at fundraising_deals (same file,
    // fundraising sub-tables, deal_id NOT NULL).
    SoftLink { child_table: "sales_deals", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "sales_stage_transitions", child_column: "deal_id", parent_table: "sales_deals" },
    SoftLink { child_table: "sales_forecasts", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "partnership_deals", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "fundraising_activities", child_column: "deal_id", parent_table: "fundraising_deals" },
    SoftLink { child_table: "fundraising_outreach", child_column: "deal_id", parent_table: "fundraising_deals" },
    // ── content / product domain (0030_content_product_domain.sql) ────────
    // Header line 12 "FK REFERENCES dropped"; each `initiative_id` targets
    // strategic_initiatives (created in this same file). adr_id →
    // architecture_decisions (0038), related_risk_id → risk_register,
    // mitigation_task_id → tasks, partnership_id → partnership_deals (0028).
    SoftLink { child_table: "risk_register", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "risk_register", child_column: "mitigation_task_id", parent_table: "tasks" },
    SoftLink { child_table: "product_features", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "product_features", child_column: "partnership_id", parent_table: "partnership_deals" },
    SoftLink { child_table: "content_calendar", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "tech_debt_items", child_column: "initiative_id", parent_table: "strategic_initiatives" },
    SoftLink { child_table: "tech_debt_items", child_column: "adr_id", parent_table: "architecture_decisions" },
    SoftLink { child_table: "tech_debt_items", child_column: "related_risk_id", parent_table: "risk_register" },
    // ── outbound domain (0044_outbound_domain.sql) ────────────────────────
    // Lines 35 / 69 annotate both sequence_id columns "soft link to
    // email_sequences.id" (email_sequences created in 0026).
    SoftLink { child_table: "outbound_sequence_steps", child_column: "sequence_id", parent_table: "email_sequences" },
    SoftLink { child_table: "outbound_email_approvals", child_column: "sequence_id", parent_table: "email_sequences" },
    // ── content domain (0047_content_domain.sql) ──────────────────────────
    // Lines 27 / 33 / 44 annotate the calendar_id / piece_id soft links.
    SoftLink { child_table: "content_pieces", child_column: "calendar_id", parent_table: "content_calendar" },
    SoftLink { child_table: "content_published", child_column: "piece_id", parent_table: "content_pieces" },
    SoftLink { child_table: "content_stage_transitions", child_column: "piece_id", parent_table: "content_pieces" },
    // ── research domain (0053_research_domain.sql) ────────────────────────
    // Line 51 adds `sales_deals.research_item_id` — line 49 annotates it a
    // soft link to the seeding research note (research_notes, 0037).
    SoftLink { child_table: "sales_deals", child_column: "research_item_id", parent_table: "research_notes" },
    // ── strategy domain (0054_strategy_domain.sql) ────────────────────────
    // Lines 24 / 34 annotate cycle_id → strategy_cycles and objective_id →
    // strategy_objectives soft links.
    SoftLink { child_table: "strategy_objectives", child_column: "cycle_id", parent_table: "strategy_cycles" },
    SoftLink { child_table: "strategy_key_results", child_column: "objective_id", parent_table: "strategy_objectives" },
];

/// One orphan finding: a soft-linked child column with dangling references. The
/// cross-boundary contract (Rust serde ↔ the `OrphanReport` TS type in
/// `tauri-cmd.ts`) — keep both in lockstep.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct OrphanReport {
    /// The child table holding the dangling references.
    pub table: String,
    /// The soft-FK column whose value has no matching parent.
    pub column: String,
    /// The table the column conceptually references (`parent_table.id`).
    pub parent_table: String,
    /// How many child rows have a non-null value absent from the parent.
    pub orphan_count: i64,
    /// Up to `SAMPLE_CAP` child-row ids for the "open the record" affordance.
    pub sample_ids: Vec<String>,
}

/// Cap on `sample_ids` per report — enough to jump to a few records without
/// bloating the payload when a column has thousands of orphans.
const SAMPLE_CAP: i64 = 5;

/// Scan every soft link in [`SOFT_LINKS`], returning a report for each that has
/// one or more dangling references. Read-only — a per-link anti-join, the
/// standard substitute for `PRAGMA foreign_key_check` when FKs were never
/// declared. Reports with `orphan_count == 0` are omitted (a clean link is not
/// a finding); the FE renders a green check for links absent from the result.
///
/// The unit-tested core behind the `data_health_scan` command.
pub(crate) async fn scan_orphans(pool: &sqlx::SqlitePool) -> Result<Vec<OrphanReport>, String> {
    let mut reports = Vec::new();

    for link in SOFT_LINKS {
        // Anti-join: child rows whose non-null FK value has no matching parent.
        // Table/column names come only from the compile-time const list, never
        // from user input, so string interpolation here is not an injection
        // surface. Bare `id` on the parent is uniform across the down-map.
        let count_sql = format!(
            "SELECT COUNT(*) AS n FROM {child} \
             WHERE {col} IS NOT NULL \
               AND {col} NOT IN (SELECT id FROM {parent})",
            child = link.child_table,
            col = link.child_column,
            parent = link.parent_table,
        );
        let row = sqlx::query(&count_sql)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("orphan count {}.{}: {e}", link.child_table, link.child_column))?;
        let orphan_count: i64 = row.try_get("n").map_err(|e| format!("read count: {e}"))?;

        if orphan_count == 0 {
            continue;
        }

        let sample_sql = format!(
            "SELECT id FROM {child} \
             WHERE {col} IS NOT NULL \
               AND {col} NOT IN (SELECT id FROM {parent}) \
             LIMIT {cap}",
            child = link.child_table,
            col = link.child_column,
            parent = link.parent_table,
            cap = SAMPLE_CAP,
        );
        let sample_rows = sqlx::query(&sample_sql)
            .fetch_all(pool)
            .await
            .map_err(|e| format!("orphan sample {}.{}: {e}", link.child_table, link.child_column))?;
        let sample_ids: Vec<String> = sample_rows
            .iter()
            .filter_map(|r| r.try_get::<String, _>("id").ok())
            .collect();

        reports.push(OrphanReport {
            table: link.child_table.to_string(),
            column: link.child_column.to_string(),
            parent_table: link.parent_table.to_string(),
            orphan_count,
            sample_ids,
        });
    }

    Ok(reports)
}

/// Read-only orphan audit across the Atelier/PA domain soft links. Runs on the
/// dedicated reader pool (same path as `db_query`); never writes. Returns one
/// [`OrphanReport`] per soft link that currently has dangling references.
#[tauri::command]
pub async fn data_health_scan(db: State<'_, Arc<PaDb>>) -> Result<Vec<OrphanReport>, String> {
    let pool = db.ensure_reader_pool().await?;
    scan_orphans(&pool).await
}

/// On-disk byte sizes of the shell database and its SQLite sidecar files
/// (DEC-32, WP-16a). `None` means the file is absent — not zero. The WAL and
/// shared-memory files only exist while a connection has the database open in
/// WAL mode, so `None` for them is normal.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DbFileSizes {
    /// Absolute path of the main database file that was measured.
    pub db_path: String,
    pub db_bytes: Option<u64>,
    pub wal_bytes: Option<u64>,
    pub shm_bytes: Option<u64>,
}

/// `<db_path>` + `suffix`, e.g. `pa.db` → `pa.db-wal` (SQLite's naming).
fn sibling_with_suffix(db_path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut s = db_path.as_os_str().to_owned();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

/// Byte size of one file via `metadata` only. Absent → `Ok(None)`. Any other
/// failure (permission, not a regular file) is an error, never a silent `None`.
fn file_size(path: &std::path::Path) -> Result<Option<u64>, String> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => Ok(Some(m.len())),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
    }
}

/// Measure the database and its `-wal` / `-shm` siblings. **Read-only**: it
/// only calls `metadata` and never opens any of the files, so it cannot
/// create, lock, truncate or checkpoint anything.
pub fn measure_db_files(db_path: &std::path::Path) -> Result<DbFileSizes, String> {
    Ok(DbFileSizes {
        db_path: db_path.to_string_lossy().into_owned(),
        db_bytes: file_size(db_path)?,
        wal_bytes: file_size(&sibling_with_suffix(db_path, "-wal"))?,
        shm_bytes: file_size(&sibling_with_suffix(db_path, "-shm"))?,
    })
}

/// DEC-32: report the database file sizes for Ngwa → Health → Data. Stats the
/// files only; does not touch the connection pools.
#[tauri::command]
pub async fn data_health_db_size(db: State<'_, Arc<PaDb>>) -> Result<DbFileSizes, String> {
    measure_db_files(db.db_path_for_diag())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open an isolated PaDb on a tempdir-backed sqlite file (all migrations
    /// applied), returning the reader pool plus the TempDir guard (kept alive
    /// for the test's lifetime). Mirrors `commands::db`'s `fresh_db` helper.
    async fn fresh_pool() -> (sqlx::SqlitePool, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("pa.db"));
        let pool = db.ensure_reader_pool().await.expect("ensure_reader_pool");
        (pool, tmp)
    }

    /// A known-good parent+child pair produces zero orphans; a dangling child
    /// is caught exactly once with its id sampled. Exercises the real
    /// `sales_deals.research_item_id → research_notes` soft link (0053).
    #[tokio::test]
    async fn detects_exactly_the_dangling_child() {
        let (pool, _tmp) = fresh_pool().await;

        // Parent research note + a good deal that points at it.
        sqlx::query(
            "INSERT INTO research_notes (id, entity_type, entity_name, title, body) \
             VALUES ('rn-1', 'market', 'Good Note', 'Good Note', 'body text')",
        )
        .execute(&pool)
        .await
        .expect("insert research note");
        sqlx::query("INSERT INTO sales_deals (id, company, research_item_id) VALUES ('deal-good', 'Acme', 'rn-1')")
            .execute(&pool)
            .await
            .expect("insert good deal");
        // Dangling deal: research_item_id points at a note that doesn't exist.
        sqlx::query("INSERT INTO sales_deals (id, company, research_item_id) VALUES ('deal-orphan', 'Beta', 'rn-missing')")
            .execute(&pool)
            .await
            .expect("insert orphan deal");
        // A deal with NULL research_item_id must NOT be flagged.
        sqlx::query("INSERT INTO sales_deals (id, company) VALUES ('deal-null', 'Gamma')")
            .execute(&pool)
            .await
            .expect("insert null deal");

        let reports = scan_orphans(&pool).await.expect("scan");
        let deal_report = reports
            .iter()
            .find(|r| r.table == "sales_deals" && r.column == "research_item_id")
            .expect("sales_deals.research_item_id should be reported as having an orphan");

        assert_eq!(deal_report.orphan_count, 1, "exactly one dangling deal");
        assert_eq!(deal_report.parent_table, "research_notes");
        assert_eq!(deal_report.sample_ids, vec!["deal-orphan".to_string()]);
    }

    /// Empty tables (fresh schema, no rows) yield no reports at all — the scan
    /// is silent on a clean database, not noisy.
    #[tokio::test]
    async fn empty_tables_report_no_orphans() {
        let (pool, _tmp) = fresh_pool().await;
        let reports = scan_orphans(&pool).await.expect("scan");
        assert!(
            reports.is_empty(),
            "a fresh empty db must produce zero orphan reports; got {reports:?}"
        );
    }

    /// A well-formed graph (child points at a real parent) produces zero
    /// reports even when rows exist — proves the anti-join isn't false-positive
    /// on valid links. Uses a self-reference (tasks.parent_task_id → tasks).
    #[tokio::test]
    async fn valid_self_reference_is_not_flagged() {
        let (pool, _tmp) = fresh_pool().await;

        sqlx::query("INSERT INTO tasks (id, title) VALUES ('t-root', 'Root')")
            .execute(&pool)
            .await
            .expect("insert root task");
        sqlx::query("INSERT INTO tasks (id, title, parent_task_id) VALUES ('t-child', 'Child', 't-root')")
            .execute(&pool)
            .await
            .expect("insert child task");

        let reports = scan_orphans(&pool).await.expect("scan");
        assert!(
            !reports.iter().any(|r| r.table == "tasks" && r.column == "parent_task_id"),
            "a task pointing at a real parent must not be flagged; got {reports:?}"
        );
    }

    /// DEC-32: sizes come from the files on disk, an absent sibling is `None`
    /// (never 0), and measuring changes nothing — contents and modified time
    /// are identical afterwards and no sibling file is created.
    #[test]
    fn measure_db_files_reports_sizes_and_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("pa.db");
        std::fs::write(&db, b"0123456789").expect("write db");
        std::fs::write(tmp.path().join("pa.db-wal"), b"abc").expect("write wal");
        let before_mtime = std::fs::metadata(&db).unwrap().modified().unwrap();

        let sizes = measure_db_files(&db).expect("measure");
        assert_eq!(sizes.db_bytes, Some(10));
        assert_eq!(sizes.wal_bytes, Some(3));
        assert_eq!(sizes.shm_bytes, None, "absent -shm must be None, not 0");
        assert_eq!(sizes.db_path, db.to_string_lossy());

        assert_eq!(std::fs::read(&db).unwrap(), b"0123456789");
        assert_eq!(
            std::fs::metadata(&db).unwrap().modified().unwrap(),
            before_mtime
        );
        assert!(
            !tmp.path().join("pa.db-shm").exists(),
            "measuring must not create -shm"
        );
        let mut names: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["pa.db".to_string(), "pa.db-wal".to_string()]);
    }

    /// A database that does not exist yet measures as all-`None` and is not
    /// created by the measurement.
    #[test]
    fn measure_db_files_absent_db_is_none_and_not_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("missing.db");
        let sizes = measure_db_files(&db).expect("measure");
        assert_eq!(
            (sizes.db_bytes, sizes.wal_bytes, sizes.shm_bytes),
            (None, None, None)
        );
        assert!(!db.exists());
    }

    /// A directory where the database file should be is an error, not a size.
    #[test]
    fn measure_db_files_non_file_is_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = tmp.path().join("pa.db");
        std::fs::create_dir(&db).expect("mkdir");
        assert!(measure_db_files(&db).is_err());
    }
}
