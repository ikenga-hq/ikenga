//! The local `ikenga.db` handle, its embedded migration set, and the
//! row-to-JSON conversion behind `db_query` / `db_exec`.
//!
//! # Why this is not in `commands/`
//!
//! Same reason as [`crate::path_allow`] and [`crate::secrets_env`]: the
//! headless daemon needs it and `commands/` is desktop-only, built around
//! `#[tauri::command]` and `AppHandle` (whose default type parameter IS
//! `Wry`, which a `--no-default-features` build does not link). The two
//! `#[tauri::command]` wrappers stay behind in [`crate::commands::db`],
//! which re-exports [`PaDb`] so every existing `crate::commands::db::PaDb`
//! call site resolves unchanged.
//!
//! # Migrations and multi-process access
//!
//! [`PaDb::ensure_pool`] applies [`MIGRATIONS`] on first use. The apply loop
//! is idempotent but **not** cross-process atomic — see the note on
//! [`ensure_schema`]. The daemon and the desktop app are not meant to share
//! one `ikenga.db`; `run_server` warns at startup when its `--data-dir`
//! already looks like a desktop profile.
//!
//! Original note, still true: tauri-plugin-sql exposes JS bindings directly
//! (the frontend can do `Database.load("sqlite:ikenga.db")`), but we
//! re-expose `db_query` / `db_exec` so callers that prefer the typed wrapper
//! in `tauri-cmd.ts` don't have to manage their own DB handle. The plugin's
//! `DbInstances` state type is `Mutex<HashMap<String, DbPool>>` and `DbPool`
//! is private, so we hold our own `sqlx::SqlitePool` pointed at the same file
//! the plugin manages and use `sqlx` directly.

use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use tokio::sync::Mutex;

/// SQLite handle for the local `ikenga.db`.
///
/// WP-01 (G-DB-CORE): the database is opened through **two** sqlx pools rather
/// than one shared pool. SQLite is single-writer; under a single shared pool a
/// long-running read transaction can starve a pending write (and vice-versa),
/// surfacing as `SQLITE_BUSY` / "database is locked". The fix is the standard
/// pattern for embedded SQLite under concurrency:
///
/// * **`writer`** — a pool capped at `max_connections = 1`, so every write is
///   serialized through a single connection and writes never contend with each
///   other. WAL journaling + a `busy_timeout` make any momentary contention
///   wait rather than error.
/// * **`reader`** — a multi-connection pool used only for `SELECT`s. WAL lets
///   readers run concurrently with the writer without blocking it.
///
/// Both pools open the same file with identical `SqliteConnectOptions`. WAL is
/// a persistent file-level mode, so once the writer sets it the reader inherits
/// it on connect. The writer is always initialized first (it owns schema
/// migration), guaranteeing WAL is in effect before any reader connects.
///
/// `ensure_pool()` returns the **writer** pool and keeps its original
/// signature: it is correct for both reads and writes (just serialized), so the
/// ~80 existing callers across the shell that hold a `SqlitePool` for mixed
/// read/write work — including `pool.begin()` transactions — remain correct
/// without edits. Only the hot `db_query` command is routed to the dedicated
/// reader pool for read parallelism.
pub struct PaDb {
    writer: Mutex<Option<sqlx::SqlitePool>>,
    reader: Mutex<Option<sqlx::SqlitePool>>,
    db_path: PathBuf,
}

/// How long a connection waits on a locked database before returning
/// `SQLITE_BUSY`. Load-bearing: this is what turns transient writer/reader
/// contention into a short wait instead of an error.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

impl PaDb {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            writer: Mutex::new(None),
            reader: Mutex::new(None),
            db_path,
        }
    }

    pub fn db_path_for_diag(&self) -> &PathBuf {
        &self.db_path
    }

    /// Shared connect options for both pools: WAL journaling, a busy_timeout so
    /// contention waits instead of erroring, `synchronous = NORMAL` (safe under
    /// WAL, much faster than FULL), and create-if-missing so a fresh install
    /// opens cleanly.
    fn connect_options(&self) -> SqliteConnectOptions {
        SqliteConnectOptions::new()
            .filename(&self.db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(BUSY_TIMEOUT)
            .synchronous(SqliteSynchronous::Normal)
    }

    /// Return the serialized **writer** pool, lazily building it (and applying
    /// migrations) on first use. Signature is unchanged from the pre-WP-01
    /// single-pool design — every existing caller keeps working, and writes are
    /// now serialized through a single connection.
    pub async fn ensure_pool(&self) -> Result<sqlx::SqlitePool, String> {
        let mut guard = self.writer.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        // Make sure parent dir + file exist so sqlx can open it.
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
        }
        // max_connections = 1: a single serialized writer. WAL is set on this
        // connection first, so it's the connection that flips the journal mode
        // for the whole file before any reader connects.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(self.connect_options())
            .await
            .map_err(|e| format!("sqlite writer connect: {e}"))?;
        // Apply migrations idempotently against the writer exactly once.
        // tauri-plugin-sql's migration runner only fires when JS calls
        // Database.load(), and that path has been observed to silently hang
        // (see raceTimeout in workspace.tsx). When it times out, ikenga.db stays
        // schema-less and every Rust-side query fails with "no such table".
        // Running migrations on the Rust writer makes the schema guaranteed
        // regardless of plugin behavior.
        ensure_schema(&pool).await?;
        *guard = Some(pool.clone());
        Ok(pool)
    }

    /// Return the multi-connection **reader** pool, lazily building it. Ensures
    /// the writer pool exists first so WAL is engaged and the schema is applied
    /// before any read connection opens.
    pub async fn ensure_reader_pool(&self) -> Result<sqlx::SqlitePool, String> {
        // Initialize the writer (WAL + schema) before opening readers.
        self.ensure_pool().await?;
        let mut guard = self.reader.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(self.connect_options())
            .await
            .map_err(|e| format!("sqlite reader connect: {e}"))?;
        *guard = Some(pool.clone());
        Ok(pool)
    }
}
/// Embedded migration set, kept in lockstep with `migrations/*.sql`.
///
/// Module-level rather than function-local so tests can assert against
/// `MIGRATIONS.len()` instead of restating the count. A hardcoded copy of the
/// number silently drifted once already: 0062_content_review_axis landed while
/// the test still expected 61, and nothing caught it because the Rust tests
/// were not running in CI.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "0001_init",
        include_str!("../migrations/0001_init.sql"),
    ),
    (
        2,
        "0002_viewer_recents",
        include_str!("../migrations/0002_viewer_recents.sql"),
    ),
    (
        3,
        "0003_claude_sessions",
        include_str!("../migrations/0003_claude_sessions.sql"),
    ),
    // 0004 (render_queue), 0005 (mbox_sync), 0006 (storyboards) created
    // app-specific schema that was retired with the strip-down. We keep
    // the SQL files in `migrations/` so existing dev DBs still apply
    // them in version order before 0009 cleans them up; fresh installs
    // run 0001→0003 then 0007→0009 (no app-specific tables ever exist).
    (
        4,
        "0004_render_queue",
        include_str!("../migrations/0004_render_queue.sql"),
    ),
    (
        5,
        "0005_mbox_sync",
        include_str!("../migrations/0005_mbox_sync.sql"),
    ),
    (
        6,
        "0006_storyboards",
        include_str!("../migrations/0006_storyboards.sql"),
    ),
    (
        7,
        "0007_pkg_kernel",
        include_str!("../migrations/0007_pkg_kernel.sql"),
    ),
    (
        8,
        "0008_pkg_install_source",
        include_str!("../migrations/0008_pkg_install_source.sql"),
    ),
    (
        9,
        "0009_strip_legacy",
        include_str!("../migrations/0009_strip_legacy.sql"),
    ),
    (
        10,
        "0010_activity_bar_pinning",
        include_str!("../migrations/0010_activity_bar_pinning.sql"),
    ),
    (
        11,
        "0011_chat_sessions",
        include_str!("../migrations/0011_chat_sessions.sql"),
    ),
    (
        12,
        "0012_session_fork",
        include_str!("../migrations/0012_session_fork.sql"),
    ),
    (
        13,
        "0013_settings_kv",
        include_str!("../migrations/0013_settings_kv.sql"),
    ),
    (
        14,
        "0014_browser_sessions",
        include_str!("../migrations/0014_browser_sessions.sql"),
    ),
    (
        15,
        "0015_projects",
        include_str!("../migrations/0015_projects.sql"),
    ),
    (
        16,
        "0016_iyke_memory",
        include_str!("../migrations/0016_iyke_memory.sql"),
    ),
    (
        17,
        "0017_claude_asset_preferences",
        include_str!("../migrations/0017_claude_asset_preferences.sql"),
    ),
    (
        18,
        "0018_pkg_trust_versioning",
        include_str!("../migrations/0018_pkg_trust_versioning.sql"),
    ),
    (
        19,
        "0019_artifact_pin_metadata",
        include_str!("../migrations/0019_artifact_pin_metadata.sql"),
    ),
    (
        20,
        "0020_pkg_permission_violations",
        include_str!("../migrations/0020_pkg_permission_violations.sql"),
    ),
    (
        21,
        "0021_pkg_capability_snapshots",
        include_str!("../migrations/0021_pkg_capability_snapshots.sql"),
    ),
    (
        22,
        "0022_artifact_comments",
        include_str!("../migrations/0022_artifact_comments.sql"),
    ),
    (
        23,
        "0023_studio_threads",
        include_str!("../migrations/0023_studio_threads.sql"),
    ),
    (
        24,
        "0024_rename_chat_threads_to_chat_sessions",
        include_str!("../migrations/0024_rename_chat_threads_to_chat_sessions.sql"),
    ),
    // WP-02 (G-SCHEMA): Atelier/PA domain tables, down-mapped Postgres →
    // SQLite as STRICT tables. Source of truth is royalti-pa's
    // supabase/migrations/*.sql (consolidated to one CREATE per table,
    // folding in subsequent ALTER … ADD COLUMN). Grouped by domain. These
    // are the local-store target for the Supabase → ikenga.db migration; the
    // WP-03 ETL loads rows, WP-05's validator reads the generated
    // `tables.json` (see `write_tables_manifest`).
    (
        25,
        "0025_tasks_domain",
        include_str!("../migrations/0025_tasks_domain.sql"),
    ),
    (
        26,
        "0026_mail_domain",
        include_str!("../migrations/0026_mail_domain.sql"),
    ),
    (
        27,
        "0027_outbound_domain",
        include_str!("../migrations/0027_outbound_domain.sql"),
    ),
    (
        28,
        "0028_sales_gtm_domain",
        include_str!("../migrations/0028_sales_gtm_domain.sql"),
    ),
    (
        29,
        "0029_finance_domain",
        include_str!("../migrations/0029_finance_domain.sql"),
    ),
    (
        30,
        "0030_content_product_domain",
        include_str!("../migrations/0030_content_product_domain.sql"),
    ),
    (
        31,
        "0031_work_domain",
        include_str!("../migrations/0031_work_domain.sql"),
    ),
    // WP-10a — full-domain migration schema gap-fill. 0032 repairs drift in
    // the pure-ETL tables (live columns the WP-02 down-map missed); 0033
    // recreates the latest_account_balances view; 0034–0039 add the 14
    // remaining business tables, down-mapped from LIVE Supabase
    // introspection (not the drifted in-repo migrations).
    (
        32,
        "0032_pure_etl_drift_fix",
        include_str!("../migrations/0032_pure_etl_drift_fix.sql"),
    ),
    (
        33,
        "0033_finance_views",
        include_str!("../migrations/0033_finance_views.sql"),
    ),
    (
        34,
        "0034_mail_outbound_ext",
        include_str!("../migrations/0034_mail_outbound_ext.sql"),
    ),
    (
        35,
        "0035_sales_ext",
        include_str!("../migrations/0035_sales_ext.sql"),
    ),
    (
        36,
        "0036_fundraising_ext",
        include_str!("../migrations/0036_fundraising_ext.sql"),
    ),
    (
        37,
        "0037_content_ext",
        include_str!("../migrations/0037_content_ext.sql"),
    ),
    (
        38,
        "0038_strategy_ext",
        include_str!("../migrations/0038_strategy_ext.sql"),
    ),
    (
        39,
        "0039_infra_ext",
        include_str!("../migrations/0039_infra_ext.sql"),
    ),
    // WP-10c — make the latest_account_balances view deterministic (id DESC
    // tie-break) for same-(txn_date,created_at) rows.
    (
        40,
        "0040_finance_view_deterministic",
        include_str!("../migrations/0040_finance_view_deterministic.sql"),
    ),
    // WP-10 residual: content_performance_history (empty; local write target).
    (
        41,
        "0041_content_perf_history",
        include_str!("../migrations/0041_content_perf_history.sql"),
    ),
    // WP-17b — mail thread-state table (read/unread · snooze · tags · preview).
    (
        42,
        "0042_mail_domain",
        include_str!("../migrations/0042_mail_domain.sql"),
    ),
    // WP-18b — sales domain app-layer columns (title, owner, next_action,
    // next_action_mode, win_probability) on sales_deals. Stage enum:
    // lead → qualified → proposal → negotiation → closing → won | lost.
    (
        43,
        "0043_sales_domain",
        include_str!("../migrations/0043_sales_domain.sql"),
    ),
    // WP-19b — outbound domain state tables (sequence steps · email approval
    //           queue · generic sent log · newsletter draft queue + approval).
    (
        44,
        "0044_outbound_domain",
        include_str!("../migrations/0044_outbound_domain.sql"),
    ),
    // WP-18b follow-up — map legacy sales_deals.stage values onto the 0043 enum.
    (
        45,
        "0045_sales_stage_backfill",
        include_str!("../migrations/0045_sales_stage_backfill.sql"),
    ),
    // WP-20b — Finance domain alert queue (CFO-agent writes; Finance pkg reads).
    (
        46,
        "0046_finance_domain",
        include_str!("../migrations/0046_finance_domain.sql"),
    ),
    // WP-21b — content domain tables (content_pieces, content_published,
    // content_stage_transitions). STRICT, no FK, soft TEXT links.
    // Stage enum: idea | outline | draft | review | scheduled.
    (
        47,
        "0047_content_domain",
        include_str!("../migrations/0047_content_domain.sql"),
    ),
    // 0048 — task activity/audit log (task_events). Backfilled from existing
    // tasks columns; no triggers (the splitter above can't handle them).
    (
        48,
        "0048_task_events",
        include_str!("../migrations/0048_task_events.sql"),
    ),
    // 0049 — per-task derived auto-close confidence (task_signals) driving
    // the Sweeper confidence bar. Backfill is a transparent heuristic.
    (
        49,
        "0049_task_signals",
        include_str!("../migrations/0049_task_signals.sql"),
    ),
    // 0050 — approve-gate run-then-pause durable draft queue (pa_action_drafts).
    // Producer side of the seam: host.paActionsPause writes rows; the approve-gate
    // panel reads them; commit/reject flip status + emit events for the external
    // mutation worker, which performs the real send. The shell never sends.
    (
        50,
        "0050_pa_action_drafts",
        include_str!("../migrations/0050_pa_action_drafts.sql"),
    ),
    // 0051 — mutation-worker claim + retry + delivery columns on pa_action_drafts.
    // Extends 0050: adds claimed_at/attempts/last_attempt_at/error_text/external_id/
    // delivery_status/delivery_checked_at + idx_pa_drafts_claimable. Freezes G-SCHEMA.
    (
        51,
        "0051_pa_action_drafts_send_state",
        include_str!("../migrations/0051_pa_action_drafts_send_state.sql"),
    ),
    // 0052 — social producer columns on social_queue (media_url, hashtags).
    // Source-of-truth store live producers (Buffer worker, cmo-agent) stamp;
    // the outbound pkg reads the same data from pa_action_drafts.payload_json.
    (
        52,
        "0052_social_media_hashtags",
        include_str!("../migrations/0052_social_media_hashtags.sql"),
    ),
    // 0053 — atelier wave-4 research domain (com.ikenga.research): extends
    // research_notes (next_action/target/agent_cycle_id/is_stale/word_count/
    // owner), adds the research_sources monitored-source register, and a soft
    // sales_deals.research_item_id link for the Hand-to-sales flow. No FK.
    (
        53,
        "0053_research_domain",
        include_str!("../migrations/0053_research_domain.sql"),
    ),
    // 0054 — atelier wave-4 strategy domain (com.ikenga.strategy): OKR tables
    // strategy_objectives / strategy_key_results / strategy_cycles. STRICT,
    // no FK, soft TEXT links. Board falls back to strategic_initiatives until seeded.
    (
        54,
        "0054_strategy_domain",
        include_str!("../migrations/0054_strategy_domain.sql"),
    ),
    // 0055 — pkg-browser WP-03: Managed Chrome dedicated-profile registry.
    // chrome_profiles maps friendly profile names to their on-disk --user-data-dir
    // paths. Parallel to browser_sessions (0014) for the WebKit partition model.
    (
        55,
        "0055_chrome_profiles",
        include_str!("../migrations/0055_chrome_profiles.sql"),
    ),
    // email_actions is the audit + undo log for server-side IMAP triage
    // (royalti-pa/scripts/imap-triage.ts). email_triage_cursor carries
    // resume state so a run interrupted partway through ~25k moves neither
    // redoes nor skips work.
    (
        56,
        "0056_email_actions",
        include_str!("../migrations/0056_email_actions.sql"),
    ),
    // WP-10: nullable `task_id` on iyke_todos, linking an agent's runtime
    // execution graph to the durable task board it serves.
    //
    // Renumbered 0056 → 0057 when this branch merged main, which had
    // already claimed 56 for email_actions. Migration ids are apply-order
    // keys, so two files sharing one id means the second never runs.
    (
        57,
        "0057_iyke_todo_task_link",
        include_str!("../migrations/0057_iyke_todo_task_link.sql"),
    ),
    // email_index is the headers-only substrate for mailbox triage: ~88k
    // messages across five accounts that `imap-propose.ts` clusters and
    // scores. Deliberately NOT a reshape of email_messages — that table
    // keeps bodies for the reply/draft path, which genuinely needs them.
    // email_ingest_cursor carries uidvalidity, which email_triage_cursor
    // (0056) lacks: a UIDVALIDITY reset silently repoints every cached UID
    // at a different message, so the ingest halts on a mismatch.
    (
        58,
        "0058_email_index",
        include_str!("../migrations/0058_email_index.sql"),
    ),
    // chi_cache is the local mapping layer for the Chi-first agent surface.
    // Agent records (Claude JSONL, Devin sidecar ledger, etc.) remain the
    // source of truth; this table only stores run_id → external_id mapping,
    // one-off output metadata, and terminal session links for future
    // persistence. WP-01 of plans/2026-08-08-ikenga-chi-first.
    (
        59,
        "0059_chi_cache",
        include_str!("../migrations/0059_chi_cache.sql"),
    ),
    (
        60,
        "0060_drop_chat_sessions",
        include_str!("../migrations/0060_drop_chat_sessions.sql"),
    ),
    // WP-06 follow-up: the legacy chat_messages table predates
    // 0011_chat_sessions.sql and was never referenced by the retired
    // chat surface, so it is dropped now.
    (
        61,
        "0061_drop_chat_messages",
        include_str!("../migrations/0061_drop_chat_messages.sql"),
    ),
    // WP-01 of the content-ops authoring layer: adds the approval axis
    // (status/ref/doc_*/reviewable_after) to content_pieces. `stage` stays
    // the production axis owned by com.ikenga.content and is untouched.
    (
        62,
        "0062_content_review_axis",
        include_str!("../migrations/0062_content_review_axis.sql"),
    ),
];

/// Embedded migration set, kept in lockstep with `migrations/*.sql`. Tracked
/// in a `_pa_migrations` table so each migration runs exactly once. SQL files
/// are split on `;\n` so multi-statement files execute one statement at a
/// time (sqlx's `query()` doesn't support multi-statement input).
///
/// # Not safe against a second *process* on the same file
///
/// Within one process this runs at most once: `ensure_pool` holds the
/// `writer` mutex across the whole call and the writer pool is capped at one
/// connection. Across processes there is no such guard. The sequence is
/// `SELECT id FROM _pa_migrations` → apply → `INSERT INTO _pa_migrations`,
/// with **no enclosing transaction and no `BEGIN IMMEDIATE`**, so two
/// processes can both read the same "not yet applied" set and both apply.
/// Most statements are `CREATE … IF NOT EXISTS` and the loop swallows
/// `already exists` / `duplicate column name`, but the terminal
/// `INSERT INTO _pa_migrations` is a plain `INSERT` against an
/// `id INTEGER PRIMARY KEY` — the loser gets a constraint violation and
/// `ensure_pool` fails outright. `busy_timeout` does not help: it serializes
/// individual statements, not a read-then-write sequence.
///
/// This is documented rather than fixed because the daemon and the desktop
/// app are not meant to share one `ikenga.db`. `server::run_server` warns at
/// startup when its `--data-dir` already holds a desktop profile.
async fn ensure_schema(pool: &sqlx::SqlitePool) -> Result<(), String> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS _pa_migrations (
            id INTEGER PRIMARY KEY,
            applied_at INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await
    .map_err(|e| format!("migrations table: {e}"))?;

    let applied: Vec<i64> = sqlx::query_scalar("SELECT id FROM _pa_migrations")
        .fetch_all(pool)
        .await
        .map_err(|e| format!("read applied migrations: {e}"))?;
    let applied: std::collections::HashSet<i64> = applied.into_iter().collect();

    let migrations: &[(i64, &str, &str)] = MIGRATIONS;

    for (id, name, sql) in migrations {
        if applied.contains(id) {
            continue;
        }
        for stmt in split_statements(sql) {
            if stmt.trim().is_empty() {
                continue;
            }
            // 0003 uses ALTER TABLE ADD COLUMN which errors with "duplicate
            // column name" if the column already exists. That can happen
            // when the JS plugin's migration ran before this one. Treat
            // those as already-applied and continue.
            if let Err(e) = sqlx::query(&stmt).execute(pool).await {
                let msg = e.to_string();
                if msg.contains("duplicate column name") || msg.contains("already exists") {
                    tracing::debug!("migration {name} stmt skipped: {msg}");
                    continue;
                }
                return Err(format!("migration {name} stmt failed: {msg}"));
            }
        }
        sqlx::query("INSERT INTO _pa_migrations (id, applied_at) VALUES (?, ?)")
            .bind(id)
            .bind(now_ms())
            .execute(pool)
            .await
            .map_err(|e| format!("record migration {name}: {e}"))?;
        tracing::info!("applied migration {name}");
    }

    // Post-migration bootstrap: ensure the Default project exists and
    // backfill project_id on tables added by 0015. Idempotent.
    bootstrap_default_project(pool).await?;

    Ok(())
}

/// Ensure the Default project row exists and backfill project_id columns
/// added by 0015_projects.sql. Runs after every schema apply; cheap when
/// already done (INSERT OR IGNORE + UPDATE … WHERE project_id IS NULL).
async fn bootstrap_default_project(pool: &sqlx::SqlitePool) -> Result<(), String> {
    let now = now_ms();
    sqlx::query(
        "INSERT OR IGNORE INTO projects
            (id, display_name, root_path, icon, color, description, position, is_default, created_at)
         VALUES ('default', 'Default', NULL, NULL, '#7c7c7c', NULL, 0, 1, ?)",
    )
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("seed default project: {e}"))?;

    for table in [
        "pkg_installed",
        "layout_state",
        "browser_sessions",
    ] {
        let sql = format!("UPDATE {table} SET project_id = 'default' WHERE project_id IS NULL");
        sqlx::query(&sql)
            .execute(pool)
            .await
            .map_err(|e| format!("backfill {table}.project_id: {e}"))?;
    }

    // Phase 4: promote any legacy `claudeProjectRoots` entries to first-class
    // `projects` rows. Idempotent (settings_kv-gated). Errors are logged but
    // never block boot — a bad row shouldn't lock the user out.
    //
    // Desktop-only because it lives in `commands::projects`, which is gated on
    // the `desktop` feature. The headless daemon skips it: `claudeProjectRoots`
    // is a desktop settings key written by the desktop app's own onboarding, so
    // there is nothing for the daemon to promote. If a future daemon needs it,
    // the function has to move to the headless core first — do not un-gate this
    // call, it will not compile.
    #[cfg(feature = "desktop")]
    if let Err(e) = crate::commands::projects::claude_roots_to_projects_migration_v1(pool).await {
        tracing::warn!("[claude-roots-migration] failed: {e}");
    }

    Ok(())
}

fn split_statements(sql: &str) -> Vec<String> {
    // Tiny statement splitter — strips line/block comments, then splits on `;`
    // outside of single-quoted strings. Good enough for our hand-written
    // schema files (no triggers, no embedded semicolons in literals beyond
    // the obvious cases).
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut in_string = false;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        if !in_string {
            // Strip `-- line comment` to end of line.
            if c == '-' && chars.peek() == Some(&'-') {
                for ch in chars.by_ref() {
                    if ch == '\n' {
                        break;
                    }
                }
                continue;
            }
        }
        if c == '\'' {
            in_string = !in_string;
            buf.push(c);
        } else if c == ';' && !in_string {
            out.push(buf.trim().to_string());
            buf.clear();
        } else {
            buf.push(c);
        }
    }
    if !buf.trim().is_empty() {
        out.push(buf.trim().to_string());
    }
    out
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn bind_params<'q>(
    mut q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    params: &'q [Value],
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    for p in params {
        q = match p {
            Value::Null => q.bind(Option::<String>::None),
            Value::Bool(b) => q.bind(*b),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    q.bind(i)
                } else if let Some(f) = n.as_f64() {
                    q.bind(f)
                } else {
                    q.bind(n.to_string())
                }
            }
            Value::String(s) => q.bind(s.clone()),
            Value::Array(arr) => {
                // Treat byte arrays as Vec<u8>.
                let bytes: Vec<u8> = arr
                    .iter()
                    .filter_map(|v| v.as_u64().map(|n| n as u8))
                    .collect();
                q.bind(bytes)
            }
            other => q.bind(other.to_string()),
        };
    }
    q
}

/// `s` begins with `kw` followed by a non-identifier char (or end of input).
/// Used to reject `SELECTFOO`/`WITHX` while accepting `SELECT(1)`, `WITH cte`.
fn starts_with_keyword(s: &str, kw: &str) -> bool {
    match s.strip_prefix(kw) {
        Some(rest) => rest
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_'),
        None => false,
    }
}

/// Returns `true` when `sql` is a read-only statement (`SELECT` or a `WITH`
/// CTE), ignoring leading whitespace and SQL comments. `db_query` runs on the
/// read-only reader pool and must never mutate; this guard stops a caller that
/// reaches the command directly (not via the `host.dbQuery` FE bridge, which
/// has its own allowlist) from smuggling a write/DDL through the read path.
/// SQLite has no data-modifying CTEs, so a `WITH`-led statement always resolves
/// to a SELECT.
fn is_read_only_sql(sql: &str) -> bool {
    let mut s = sql.trim_start();
    // Strip any run of leading line (`-- …`) and block (`/* … */`) comments so
    // a comment prefix can't disguise the real leading keyword.
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            match rest.find('\n') {
                Some(nl) => s = rest[nl + 1..].trim_start(),
                None => return false, // comment-only input — no statement
            }
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(end) => s = rest[end + 2..].trim_start(),
                None => return false, // unterminated block comment
            }
        } else {
            break;
        }
    }
    let lower = s.to_ascii_lowercase();
    starts_with_keyword(&lower, "select") || starts_with_keyword(&lower, "with")
}

/// Result of a `db_exec`.
///
/// **Wire shape is load-bearing.** The frontend declares this as
/// `SqlQueryResult { rowsAffected, lastInsertId }` in
/// `src/lib/transport/sql-shim.ts`; that is also exactly what
/// `@tauri-apps/plugin-sql` returns, so the browser path and the Tauri path
/// hand callers the same object. `rename_all = "camelCase"` is what makes the
/// two agree — dropping it turns both fields into `undefined` in TS with no
/// error anywhere.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub rows_affected: u64,
    pub last_insert_id: i64,
}

/// Run a read-only statement and return one JSON object per row.
///
/// Rejects anything that isn't a `SELECT`/`WITH` read *before* touching a
/// pool, then runs on the dedicated multi-connection reader pool so reads run
/// concurrently with the serialized writer (WP-01).
pub async fn query_json(db: &PaDb, sql: &str, params: &[Value]) -> Result<Vec<Value>, String> {
    use sqlx::{Column, Row, TypeInfo, ValueRef};

    // Read-only guard: `db_query` must never mutate. Reject anything that isn't
    // a SELECT/WITH read before it touches the reader pool.
    if !is_read_only_sql(sql) {
        return Err("db_query: only SELECT/WITH read statements are allowed".to_string());
    }

    let pool = db.ensure_reader_pool().await?;
    let q = bind_params(sqlx::query(sql), params);
    let rows = q
        .fetch_all(&pool)
        .await
        .map_err(|e| format!("query failed: {e}"))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut obj = Map::new();
        for (i, col) in row.columns().iter().enumerate() {
            let name = col.name().to_string();
            let raw = row.try_get_raw(i).map_err(|e| format!("get_raw: {e}"))?;
            let val = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "INT" | "BIGINT" => row
                        .try_get::<i64, _>(i)
                        .ok()
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                    "REAL" | "FLOAT" | "DOUBLE" => row
                        .try_get::<f64, _>(i)
                        .ok()
                        .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
                        .unwrap_or(Value::Null),
                    "BOOLEAN" => row
                        .try_get::<bool, _>(i)
                        .ok()
                        .map(Value::Bool)
                        .unwrap_or(Value::Null),
                    "BLOB" => row
                        .try_get::<Vec<u8>, _>(i)
                        .ok()
                        .map(|b| Value::Array(b.into_iter().map(Value::from).collect()))
                        .unwrap_or(Value::Null),
                    _ => row
                        .try_get::<String, _>(i)
                        .ok()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                }
            };
            obj.insert(name, val);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

/// Run a statement on the serialized writer pool. No read-only guard — this
/// is the write path.
pub async fn exec(db: &PaDb, sql: &str, params: &[Value]) -> Result<ExecResult, String> {
    let pool = db.ensure_pool().await?;
    let q = bind_params(sqlx::query(sql), params);
    let res = q
        .execute(&pool)
        .await
        .map_err(|e| format!("exec failed: {e}"))?;
    Ok(ExecResult {
        rows_affected: res.rows_affected(),
        last_insert_id: res.last_insert_rowid(),
    })
}
/// WP-02 (G-05): emit a generated `tables.json` schema manifest next to
/// `ikenga.db`. Introspects `sqlite_master` + `PRAGMA table_info(<table>)` and
/// writes a deterministic (sorted-key) map:
///
/// ```json
/// {
///   "<table>": {
///     "strict": true,
///     "columns": [{ "name": "...", "type": "...", "notnull": <bool>, "pk": <bool> }]
///   }
/// }
/// ```
///
/// This is the artifact the WP-05 schema-validator consumes to cross-check pkg
/// store declarations against the live schema. Internal `sqlite_*` tables and
/// the `_pa_migrations` bookkeeping table are excluded. `strict` reflects
/// whether the table was created with the `STRICT` modifier (read back from the
/// stored `CREATE TABLE` SQL — STRICT is not exposed by PRAGMA).
///
/// Determinism: `serde_json::Map` preserves insertion order, so tables are
/// inserted in `name ASC` order and columns in `cid ASC` (PRAGMA's natural
/// order). The file is pretty-printed for diffability.
#[allow(dead_code)]
pub async fn write_tables_manifest(
    pool: &sqlx::SqlitePool,
    dir: &std::path::Path,
) -> Result<PathBuf, String> {
    use sqlx::Row;

    // (name, sql) for every user table, sorted by name for stable output.
    let tables: Vec<(String, String)> = sqlx::query(
        "SELECT name, sql FROM sqlite_master
         WHERE type = 'table'
           AND name NOT LIKE 'sqlite_%'
           AND name <> '_pa_migrations'
         ORDER BY name ASC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("list tables: {e}"))?
    .into_iter()
    .map(|row| {
        let name: String = row.get("name");
        let sql: String = row.try_get("sql").unwrap_or_default();
        (name, sql)
    })
    .collect();

    let mut manifest = Map::new();
    for (table, create_sql) in &tables {
        // STRICT is a table-level modifier not surfaced by PRAGMA; detect it
        // from the stored CREATE TABLE text (case-insensitive trailing modifier).
        let strict = create_sql.to_uppercase().contains(") STRICT");

        let info = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
            .fetch_all(pool)
            .await
            .map_err(|e| format!("table_info({table}): {e}"))?;

        let mut columns = Vec::with_capacity(info.len());
        for col in info {
            // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk
            let cname: String = col.get("name");
            let ctype: String = col.try_get("type").unwrap_or_default();
            let notnull: i64 = col.try_get("notnull").unwrap_or(0);
            let pk: i64 = col.try_get("pk").unwrap_or(0);
            let mut c = Map::new();
            c.insert("name".into(), Value::String(cname));
            c.insert("type".into(), Value::String(ctype));
            c.insert("notnull".into(), Value::Bool(notnull != 0));
            c.insert("pk".into(), Value::Bool(pk != 0));
            columns.push(Value::Object(c));
        }

        let mut entry = Map::new();
        entry.insert("strict".into(), Value::Bool(strict));
        entry.insert("columns".into(), Value::Array(columns));
        manifest.insert(table.clone(), Value::Object(entry));
    }

    let json = serde_json::to_string_pretty(&Value::Object(manifest))
        .map_err(|e| format!("serialize tables.json: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = dir.join("tables.json");
    std::fs::write(&path, json).map_err(|e| format!("write {}: {e}", path.display()))?;
    tracing::info!("[wp-02] wrote schema manifest: {}", path.display());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Open an isolated PaDb on a tempdir-backed sqlite file. Returns the db
    /// plus the TempDir guard (kept alive for the test's lifetime).
    async fn fresh_db() -> (PaDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("pa.db"));
        (db, tmp)
    }

    /// Total embedded migration count. 24 shipped through WP-01; WP-02 added 7
    /// domain-schema migrations (0025–0031); WP-10a adds 8 more (0032–0039:
    /// drift fix + finance view + 14 new business tables); 0040–0051 brought it
    /// to 51 (finance/content/task domains + pa_action_drafts send-state). Keep this in
    /// lockstep with the `migrations` tuple list — it guards against a migration
    /// silently being dropped from the embedded list (a class of bug we've hit
    /// before). Was stale at 47 while 0048–0050 landed on main without a bump;
    /// 0051 (pa_action_drafts send-state, WP-12) brings it to 51; 0052
    /// (social_queue media/hashtags columns) brings it to 52; 0053/0054
    /// (research + strategy domains, atelier wave-4) bring it to 54; 0055
    /// (chrome_profiles, pkg-browser WP-03) brings it to 55 — it landed
    /// without a bump, the exact drift this guard exists to catch. 0056
    /// (email_actions + email_triage_cursor) landed without a bump too;
    /// 0057 (iyke_todo_task_link, WP-10) brings it to 57; 0058 (email_index +
    /// email_ingest_cursor, headers-first mailbox ingest) brings it to 58.
    /// Derived from the embedded set rather than restated, so adding a
    /// migration cannot silently desync this expectation.
    const MIGRATION_COUNT: i64 = MIGRATIONS.len() as i64;

    /// Schema init applies every embedded migration exactly once. The
    /// `_pa_migrations` table must end with one row per migration tuple.
    #[tokio::test]
    async fn ensure_schema_applies_all_migrations() {
        let (db, _tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _pa_migrations")
            .fetch_one(&writer)
            .await
            .expect("count migrations");
        assert_eq!(
            count, MIGRATION_COUNT,
            "expected all {MIGRATION_COUNT} embedded migrations to be recorded in _pa_migrations, got {count}"
        );

        // Idempotency: a second ensure_pool() (cached) must not double-apply.
        let writer2 = db.ensure_pool().await.expect("ensure_pool cached");
        let count2: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _pa_migrations")
            .fetch_one(&writer2)
            .await
            .expect("recount migrations");
        assert_eq!(
            count2, MIGRATION_COUNT,
            "migration count must be stable across calls"
        );
    }

    /// WP-02 (G-SCHEMA): a representative sample of the new domain tables exist
    /// with their expected key columns. Catches a migration that parsed but
    /// produced the wrong shape (e.g. a dropped column or a typo'd name).
    #[tokio::test]
    async fn wp02_domain_tables_exist_with_key_columns() {
        let (db, _tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");

        // (table, columns that MUST be present)
        let expectations: &[(&str, &[&str])] = &[
            (
                "tasks",
                &[
                    "id",
                    "title",
                    "status",
                    "initiative_id",
                    "claude_session_id",
                ],
            ),
            (
                "email_drafts",
                &["id", "subject", "body", "sequence_id", "type", "status"],
            ),
            (
                "sales_deals",
                &["id", "company", "stage", "value", "segment"],
            ),
            (
                "bank_accounts",
                &["id", "account_name", "entity", "currency"],
            ),
        ];

        for (table, cols) in expectations {
            let rows = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
                .fetch_all(&writer)
                .await
                .unwrap_or_else(|e| panic!("table_info({table}): {e}"));
            assert!(
                !rows.is_empty(),
                "expected WP-02 table `{table}` to exist after init"
            );
            let present: std::collections::HashSet<String> = {
                use sqlx::Row;
                rows.iter().map(|r| r.get::<String, _>("name")).collect()
            };
            for col in *cols {
                assert!(
                    present.contains(*col),
                    "table `{table}` is missing expected column `{col}` (got {present:?})"
                );
            }
        }
    }

    /// WP-02: STRICT is in force on the new tables. Inserting a value whose
    /// type can't be coerced to the column's declared type (here: a non-integer
    /// string into an INTEGER column) must be rejected with a datatype error.
    /// On a non-STRICT table SQLite would silently store the string.
    #[tokio::test]
    async fn wp02_strict_rejects_wrong_typed_value() {
        let (db, _tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");

        // tasks.progress_pct is INTEGER on a STRICT table. A bare non-numeric
        // string is not coercible to INTEGER → STRICT rejects it.
        let res = sqlx::query("INSERT INTO tasks (id, title, progress_pct) VALUES (?, ?, ?)")
            .bind("t-strict-1")
            .bind("strict probe")
            .bind("not-a-number")
            .execute(&writer)
            .await;

        let err = res.expect_err("STRICT table must reject a non-integer in an INTEGER column");
        let msg = err.to_string().to_lowercase();
        // SQLite phrases the STRICT rejection as "cannot store <TYPE> value in
        // <TYPE> column" (error code 3091, SQLITE_CONSTRAINT_DATATYPE).
        assert!(
            msg.contains("cannot store")
                || msg.contains("datatype")
                || msg.contains("mismatch")
                || msg.contains("strict"),
            "expected a STRICT datatype-mismatch error, got: {err}"
        );

        // Control: a well-typed insert into the same STRICT table succeeds.
        sqlx::query("INSERT INTO tasks (id, title, progress_pct) VALUES (?, ?, ?)")
            .bind("t-strict-2")
            .bind("strict probe ok")
            .bind(42i64)
            .execute(&writer)
            .await
            .expect("well-typed insert into STRICT tasks should succeed");
    }

    /// WP-02 (G-05): the tables.json emitter writes a parseable manifest next
    /// to ikenga.db that includes the new domain tables, marks them STRICT, and
    /// excludes bookkeeping/internal tables.
    #[tokio::test]
    async fn wp02_tables_manifest_emits_and_parses() {
        let (db, tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");

        let path = write_tables_manifest(&writer, tmp.path())
            .await
            .expect("write_tables_manifest");
        assert!(path.exists(), "tables.json should be written to disk");

        let raw = std::fs::read_to_string(&path).expect("read tables.json");
        let parsed: Value = serde_json::from_str(&raw).expect("tables.json must be valid JSON");
        let obj = parsed.as_object().expect("manifest is a JSON object");

        // New domain tables are present and flagged STRICT.
        for t in ["tasks", "email_drafts", "sales_deals", "bank_accounts"] {
            let entry = obj
                .get(t)
                .unwrap_or_else(|| panic!("manifest missing table `{t}`"));
            assert_eq!(
                entry.get("strict").and_then(Value::as_bool),
                Some(true),
                "table `{t}` should be reported as STRICT"
            );
            let cols = entry
                .get("columns")
                .and_then(Value::as_array)
                .unwrap_or_else(|| panic!("table `{t}` has no columns array"));
            assert!(!cols.is_empty(), "table `{t}` should report columns");
            // Each column carries name/type/notnull/pk.
            let first = cols[0].as_object().expect("column is an object");
            for k in ["name", "type", "notnull", "pk"] {
                assert!(first.contains_key(k), "column entry missing `{k}`");
            }
        }

        // Bookkeeping + internal tables are excluded.
        assert!(
            !obj.contains_key("_pa_migrations"),
            "_pa_migrations must be excluded from the manifest"
        );
        assert!(
            !obj.keys().any(|k| k.starts_with("sqlite_")),
            "internal sqlite_* tables must be excluded"
        );
    }

    /// WP-10a (G-SCHEMA-2): the 14 new business tables (0034–0039) exist STRICT
    /// with their key columns; the pure-ETL drift fix (0032) added the missing
    /// live columns; the latest_account_balances VIEW (0033) is queryable. This
    /// is the gate that the full-domain migration's local schema is complete.
    #[tokio::test]
    async fn wp10a_new_domain_schema_complete() {
        let (db, _tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");
        use sqlx::Row;

        // (table, columns that MUST be present) for a sample of the 14 new tables.
        let expectations: &[(&str, &[&str])] = &[
            (
                "outbound_sequences",
                &["id", "contact_email", "sequence_id"],
            ),
            ("sales_activities", &["id", "deal_id", "activity_type"]),
            (
                "partnership_stage_transitions",
                &["id", "partnership_id", "to_stage"],
            ),
            ("research_notes", &["id", "entity_type", "body"]),
            ("ideas_backlog", &["id", "short_id", "status", "discussion"]),
            ("review_items", &["id", "content_type", "created_by"]),
            ("task_approvals", &["id", "task_id", "requested_by"]),
            ("contacts", &["id", "email"]),
            ("claude_sessions", &["id", "session_id", "agent_type"]),
            ("video_projects", &["id", "slug", "video_provider"]),
        ];
        for (table, cols) in expectations {
            let rows = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
                .fetch_all(&writer)
                .await
                .unwrap_or_else(|e| panic!("table_info({table}): {e}"));
            assert!(!rows.is_empty(), "WP-10a table `{table}` must exist");
            let present: std::collections::HashSet<String> =
                rows.iter().map(|r| r.get::<String, _>("name")).collect();
            for col in *cols {
                assert!(
                    present.contains(*col),
                    "table `{table}` missing column `{col}` (got {present:?})"
                );
            }
        }

        // 0032 drift fix: the agent-session trio landed on a representative
        // pure-ETL table that previously lacked them.
        let ed = sqlx::query("PRAGMA table_info(\"email_drafts\")")
            .fetch_all(&writer)
            .await
            .expect("table_info(email_drafts)");
        let ed_cols: std::collections::HashSet<String> =
            ed.iter().map(|r| r.get::<String, _>("name")).collect();
        for col in ["claude_session_id", "working_dir", "delivery_external_id"] {
            assert!(
                ed_cols.contains(col),
                "0032 drift fix should have added email_drafts.{col}"
            );
        }

        // pa_actions_cursor has a COMPOSITE primary key (job_id, key).
        let pac = sqlx::query("PRAGMA table_info(\"pa_actions_cursor\")")
            .fetch_all(&writer)
            .await
            .expect("table_info(pa_actions_cursor)");
        let pk_cols: Vec<String> = pac
            .iter()
            .filter(|r| r.get::<i64, _>("pk") != 0)
            .map(|r| r.get::<String, _>("name"))
            .collect();
        assert_eq!(
            pk_cols.len(),
            2,
            "pa_actions_cursor must have a 2-column composite PK, got {pk_cols:?}"
        );

        // 0033: the latest_account_balances VIEW exists and is queryable
        // (returns 0 rows against an empty transaction_ledger — the point is it
        // parses and resolves its window function, not that it has data yet).
        let view_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM latest_account_balances")
            .fetch_one(&writer)
            .await
            .expect("latest_account_balances view must be queryable");
        assert_eq!(view_count, 0, "empty ledger → empty balances view");
    }

    /// Verify WAL is actually engaged on the writer connection — this is what
    /// makes concurrent reads-during-write safe.
    #[tokio::test]
    async fn writer_uses_wal_journal() {
        let (db, _tmp) = fresh_db().await;
        let writer = db.ensure_pool().await.expect("ensure_pool");
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&writer)
            .await
            .expect("journal_mode");
        assert_eq!(mode.to_lowercase(), "wal", "expected WAL journal mode");
    }

    /// Reader-during-write smoke: hold a stream of reads on the reader pool
    /// while a sustained series of writes runs on the writer pool, and assert
    /// **no `SQLITE_BUSY` / "database is locked"** error surfaces from either
    /// side. Under a single shared pool this is the scenario that starves and
    /// errors; with the writer/reader split + WAL + busy_timeout it must not.
    #[tokio::test]
    async fn concurrent_reads_during_writes_never_lock() {
        let (db, _tmp) = fresh_db().await;
        let db = Arc::new(db);

        // Initialize both pools (writer also applies schema).
        let writer = db.ensure_pool().await.expect("ensure_pool");
        let reader = db.ensure_reader_pool().await.expect("ensure_reader_pool");

        // A scratch table to hammer.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _wp01_smoke (id INTEGER PRIMARY KEY, v INTEGER NOT NULL)",
        )
        .execute(&writer)
        .await
        .expect("create smoke table");

        // Writer task: 200 INSERTs, several in explicit transactions to hold
        // the write lock longer (the worst case for a starving reader).
        let writer_pool = writer.clone();
        let write_task = tokio::spawn(async move {
            for i in 0..200i64 {
                if i % 10 == 0 {
                    let mut tx = writer_pool.begin().await.map_err(|e| e.to_string())?;
                    for j in 0..5i64 {
                        sqlx::query("INSERT INTO _wp01_smoke (v) VALUES (?)")
                            .bind(i * 100 + j)
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| e.to_string())?;
                    }
                    tx.commit().await.map_err(|e| e.to_string())?;
                } else {
                    sqlx::query("INSERT INTO _wp01_smoke (v) VALUES (?)")
                        .bind(i)
                        .execute(&writer_pool)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
            Ok::<(), String>(())
        });

        // Reader task: continuous COUNT(*) reads concurrent with the writes.
        let reader_pool = reader.clone();
        let read_task = tokio::spawn(async move {
            for _ in 0..400 {
                let _n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _wp01_smoke")
                    .fetch_one(&reader_pool)
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::task::yield_now().await;
            }
            Ok::<(), String>(())
        });

        let (w, r) = tokio::join!(write_task, read_task);
        let w = w.expect("writer task join");
        let r = r.expect("reader task join");

        // The key assertion: neither side saw a lock/busy error.
        for res in [&w, &r] {
            if let Err(e) = res {
                let low = e.to_lowercase();
                assert!(
                    !(low.contains("database is locked") || low.contains("sqlite_busy")),
                    "saw a lock/busy error under concurrent read/write: {e}"
                );
                panic!("unexpected db error under concurrency: {e}");
            }
        }

        // Sanity: all writes landed. 200 iterations: every 10th (20 of them)
        // is a 5-row transaction = 100 rows; the other 180 are single inserts.
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _wp01_smoke")
            .fetch_one(&reader)
            .await
            .expect("final count");
        assert_eq!(
            total, 280,
            "all writes should have committed (180 single + 20*5 tx)"
        );
    }

    /// WP-09: the Rust `db_query` read-only guard accepts SELECT/WITH reads
    /// (including leading whitespace/comment noise and mixed case) and rejects
    /// every mutating / DDL statement, so a direct caller can't push a write
    /// through the reader pool.
    #[test]
    fn db_query_guard_allows_reads_rejects_writes() {
        // Allowed: SELECT / WITH in any case, past leading whitespace + comments.
        for sql in [
            "SELECT * FROM tasks",
            "select 1",
            "  \n\t SELECT id FROM tasks",
            "WITH recent AS (SELECT * FROM tasks) SELECT * FROM recent",
            "with x as (select 1) select * from x",
            "-- a leading line comment\nSELECT * FROM tasks",
            "/* block */ SELECT * FROM tasks",
            "/* a */\n-- b\n  select 1",
            "SELECT(1)",
        ] {
            assert!(is_read_only_sql(sql), "expected read-only: {sql:?}");
        }

        // Rejected: writes, DDL, PRAGMA/ATTACH, keyword-prefixed identifiers,
        // and comment-only input.
        for sql in [
            "INSERT INTO tasks (id) VALUES ('x')",
            "UPDATE tasks SET status = 'done'",
            "DELETE FROM tasks",
            "DROP TABLE tasks",
            "CREATE TABLE t (id INTEGER)",
            "PRAGMA journal_mode",
            "ATTACH DATABASE 'x' AS y",
            "VACUUM",
            "REPLACE INTO tasks (id) VALUES ('x')",
            "SELECTFOO", // keyword-prefixed identifier, not the SELECT keyword
            "WITHOUT rowid stuff",
            "-- comment only\n",
            "/* unterminated",
            "",
        ] {
            assert!(!is_read_only_sql(sql), "expected rejected: {sql:?}");
        }
    }

    /// WP-12b: the shared implementation both the Tauri command and the
    /// daemon's `/api/rpc` arm call. Proves the write path reports real
    /// `rows_affected` / `last_insert_id` (the frontend's `SqlQueryResult`),
    /// the read path returns one JSON object per row with typed values, and
    /// the read-only guard is enforced inside `query_json` rather than only at
    /// the desktop call site.
    #[tokio::test]
    async fn query_json_and_exec_round_trip() {
        let (db, _tmp) = fresh_db().await;

        let res = exec(
            &db,
            "INSERT INTO projects (id, display_name, position, is_default, created_at) \
             VALUES (?, ?, ?, 0, ?)",
            &[
                Value::from("wp12b"),
                Value::from("WP-12b"),
                Value::from(7),
                Value::from(now_ms()),
            ],
        )
        .await
        .expect("insert");
        assert_eq!(res.rows_affected, 1);
        assert!(res.last_insert_id > 0, "expected a real rowid");

        let rows = query_json(
            &db,
            "SELECT id, display_name, position, root_path FROM projects WHERE id = ?",
            &[Value::from("wp12b")],
        )
        .await
        .expect("select");
        assert_eq!(rows.len(), 1);
        let row = rows[0].as_object().expect("row is an object");
        assert_eq!(row["id"], Value::from("wp12b"));
        assert_eq!(row["display_name"], Value::from("WP-12b"));
        assert_eq!(row["position"], Value::from(7));
        assert_eq!(row["root_path"], Value::Null, "NULL must stay JSON null");

        // The guard lives in `query_json`, so the daemon inherits it.
        let err = query_json(&db, "DELETE FROM projects", &[])
            .await
            .expect_err("write through the read path must be refused");
        assert!(err.contains("only SELECT/WITH"), "unexpected error: {err}");
    }
}
