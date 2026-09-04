//! Host-side database sandbox for pkg **backend** processes (WP-23 / D-18).
//!
//! # Why this exists
//!
//! A pkg's *iframe* is properly scoped: `host.dbQuery` / `host.dbExec` in
//! `src/components/pkg/pkg-iframe-host.tsx` reject the wrong statement kind and
//! refuse any table outside the manifest's `permissions["sqlite.tables"]`.
//!
//! A pkg's *backend* — a sidecar or a long-lived MCP server — had no such
//! accessor. It is a separate OS process, so it cannot reach a Tauri command;
//! its only option was to open `ikenga.db` directly (better-sqlite3 and
//! friends), which hands it full read/write over **every** table: tasks, email,
//! finance, everything. That is not theoretical — fabricated meeting rows were
//! written through exactly that path during the meetings plan's own
//! development, one of them resurrecting a deleted recording's UUID.
//!
//! `ikenga-pkgs/packages/mcp/meetings/src/sqlite.ts` carries a pkg-side guard,
//! but a self-imposed guard is worthless once third parties publish: a hostile
//! pkg simply omits it. Enforcement has to be the host's.
//!
//! # Shape
//!
//! This module is the **pure, IO-free policy core** — the same split
//! `pkg/http_proxy.rs` uses for `host.fetch`. Every decision that can be made
//! without a pool or a socket lives here so it is unit-testable with no running
//! shell:
//!
//! - [`classify`] — statement kind, after stripping leading SQL comments so a
//!   `/* … */` prefix cannot disguise the real leading keyword.
//! - [`read_source_tables`] / [`write_target_table`] — table extraction,
//!   deliberate ports of the TS `readSourceTables` / `writeTargetTable` so the
//!   iframe path and the backend path cannot drift.
//! - [`authorize`] — the single gate both transports call.
//!
//! The IO wrapper is [`crate::iyke::pkg_db`] — the localhost HTTP endpoints a
//! backend process talks to. The frontend is expected to adopt this same core
//! rather than keep its TS copy; see "Remaining drift" below.
//!
//! # Identity
//!
//! A backend process proves which pkg it is with a per-pkg bearer token minted
//! at spawn time and handed to it through its environment
//! (`IKENGA_PKG_DB_TOKEN` + `IKENGA_PKG_DB_URL`). The token maps to a pkg id
//! and an install path; the manifest is re-read from that install path on every
//! request, so a scope narrowed on disk takes effect without a restart. The
//! token is **not** the iyke bearer token: it opens these two endpoints and
//! nothing else.
//!
//! # Fails closed
//!
//! Unknown token, unreadable/unparseable manifest, missing `capabilities.sqlite`,
//! unrecognised statement, unidentifiable table, more than one statement — every
//! one of those is a refusal, never a silent pass.
//!
//! # Remaining drift (honest limits)
//!
//! 1. **This is a sanctioned channel, not a jail.** A backend process can still
//!    `open("~/.local/share/app.ikenga/ikenga.db")` itself. Closing *that*
//!    requires OS-level process sandboxing (bubblewrap / seccomp / AppArmor on
//!    Linux, sandbox-exec on macOS) applied to pkg children, which is a separate
//!    work package and cannot be done from this module. What this buys today is
//!    a scoped path that is enforced by the host rather than by the pkg, so a
//!    published pkg can be *required* to use it and audited when it does not.
//! 2. **The frontend still runs its own TS copy.** The intended end state is a
//!    pair of thin `pkg_db_query` / `pkg_db_exec` Tauri commands over
//!    [`authorize`], with `dispatchHostCall` calling those instead of
//!    `checkSqliteTableScope`, so there is exactly one implementation. Both
//!    halves of that edit (the command registration's ACL entry and
//!    `src/components/pkg/pkg-iframe-host.tsx`) live outside this module; until
//!    then the TS helpers here are ported line-for-line and the unit tests below
//!    replay the TS test cases so drift shows up as a failure.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use regex::Regex;

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// Frozen refusal reasons. Callers surface these as `{ ok: false, reason }`;
/// keep the wire strings stable — pkg backends branch on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbScopeRefusal {
    /// Bearer token didn't match any live grant.
    UnknownToken,
    /// Grant resolved, but the manifest at its install path is missing,
    /// unreadable, or won't parse.
    ManifestUnreadable,
    /// Manifest carries no `capabilities.sqlite` block.
    CapabilityMissing,
    /// Statement kind is wrong for the verb, or is DDL / ATTACH / PRAGMA /
    /// VACUUM / anything else outside the two allowlists.
    StatementNotAllowed,
    /// More than one statement in a single request.
    MultipleStatements,
    /// No table name could be extracted from the statement.
    NoTableIdentified,
    /// A referenced table is absent from `permissions["sqlite.tables"]`.
    TableOutOfScope,
}

impl DbScopeRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            DbScopeRefusal::UnknownToken => "unknown-token",
            DbScopeRefusal::ManifestUnreadable => "manifest-unreadable",
            DbScopeRefusal::CapabilityMissing => "capability-missing",
            DbScopeRefusal::StatementNotAllowed => "statement-not-allowed",
            DbScopeRefusal::MultipleStatements => "multiple-statements",
            DbScopeRefusal::NoTableIdentified => "no-table-identified",
            DbScopeRefusal::TableOutOfScope => "table-out-of-scope",
        }
    }
}

/// A refusal plus a human-readable detail (which table, which keyword).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbScopeDenial {
    pub reason: DbScopeRefusal,
    pub detail: String,
}

impl DbScopeDenial {
    fn new(reason: DbScopeRefusal, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

/// Which verb the caller is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbMode {
    /// `SELECT` / `WITH` only — runs on the read-only reader pool.
    Read,
    /// `INSERT` / `UPDATE` / `DELETE` only.
    Write,
}

/// What [`classify`] made of a statement's leading keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Read,
    Write,
    /// DDL, ATTACH, PRAGMA, VACUUM, REPLACE, empty input — anything that is
    /// neither of the two allowlists. Always rejected.
    Rejected,
}

// ---------------------------------------------------------------------------
// Statement classification
// ---------------------------------------------------------------------------

/// Strip any run of leading line (`-- …`) and block (`/* … */`) comments plus
/// whitespace, so a comment prefix can't disguise the real leading keyword.
///
/// Mirrors the comment-stripping loop in `crate::db::is_read_only_sql`. An
/// unterminated block comment or comment-only input yields `None`, which the
/// caller treats as [`StatementKind::Rejected`].
fn strip_leading_comments(sql: &str) -> Option<&str> {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            match rest.find('\n') {
                Some(nl) => s = rest[nl + 1..].trim_start(),
                None => return None,
            }
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(end) => s = rest[end + 2..].trim_start(),
                None => return None,
            }
        } else {
            break;
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// True when `s` (already lowercased) starts with `kw` as a whole word.
/// Mirrors `crate::db::starts_with_keyword`.
fn starts_with_keyword(s: &str, kw: &str) -> bool {
    match s.strip_prefix(kw) {
        Some(rest) => rest
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_'),
        None => false,
    }
}

/// Classify a statement by its leading keyword.
///
/// The two allowlists match the iframe path exactly: reads are `SELECT`/`WITH`
/// (SQLite has no data-modifying CTEs, so a `WITH`-led statement always
/// resolves to a SELECT); writes are `INSERT`/`UPDATE`/`DELETE`. Everything
/// else — `CREATE`, `DROP`, `ALTER`, `ATTACH`, `PRAGMA`, `VACUUM`, `REPLACE`,
/// `BEGIN` — is [`StatementKind::Rejected`] outright.
pub fn classify(sql: &str) -> StatementKind {
    let Some(s) = strip_leading_comments(sql) else {
        return StatementKind::Rejected;
    };
    let lower = s.to_ascii_lowercase();
    if starts_with_keyword(&lower, "select") || starts_with_keyword(&lower, "with") {
        StatementKind::Read
    } else if starts_with_keyword(&lower, "insert")
        || starts_with_keyword(&lower, "update")
        || starts_with_keyword(&lower, "delete")
    {
        StatementKind::Write
    } else {
        StatementKind::Rejected
    }
}

/// True when `sql` carries a second statement after a top-level `;`.
///
/// Quoting-aware: a `;` inside `'…'`, `"…"`, `` `…` ``, `[…]`, a `-- …` line
/// comment or a `/* … */` block comment is not a separator. Trailing `;` plus
/// whitespace/comments is fine — a real second statement is not.
///
/// This is **stricter than the TS iframe path**, which anchors its regexes at
/// the start of the string and never looks for a second statement. That is a
/// deliberate one-way divergence: the backend path is the one an unco-operative
/// third-party pkg drives, so it tightens rather than loosens. `sqlx` binds a
/// single prepared statement and would drop the tail silently; refusing is
/// louder and leaves no room for a scope-check bypass via
/// `INSERT INTO mine …; DELETE FROM tasks`.
pub fn has_multiple_statements(sql: &str) -> bool {
    let bytes: Vec<char> = sql.chars().collect();
    let mut i = 0usize;
    let mut semi_at: Option<usize> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            '\'' | '"' | '`' => {
                let quote = c;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == quote {
                        // SQL doubles a quote to escape it.
                        if i + 1 < bytes.len() && bytes[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            '[' => {
                while i < bytes.len() && bytes[i] != ']' {
                    i += 1;
                }
                i += 1;
            }
            '-' if i + 1 < bytes.len() && bytes[i + 1] == '-' => {
                while i < bytes.len() && bytes[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < bytes.len() && bytes[i + 1] == '*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ';' => {
                semi_at = Some(i);
                break;
            }
            _ => i += 1,
        }
    }
    let Some(at) = semi_at else { return false };
    let tail: String = bytes[at + 1..].iter().collect();
    // Anything but whitespace / comments after the separator is a 2nd statement.
    strip_leading_comments(&tail).is_some()
}

// ---------------------------------------------------------------------------
// Table extraction — ports of the TS helpers
// ---------------------------------------------------------------------------

fn write_target_re() -> &'static [Regex; 3] {
    static RE: OnceLock<[Regex; 3]> = OnceLock::new();
    RE.get_or_init(|| {
        [
            Regex::new(r#"(?i)^\s*insert\s+(?:or\s+\w+\s+)?into\s+["'`\[]?(\w+)"#).unwrap(),
            Regex::new(r#"(?i)^\s*update\s+["'`\[]?(\w+)"#).unwrap(),
            Regex::new(r#"(?i)^\s*delete\s+from\s+["'`\[]?(\w+)"#).unwrap(),
        ]
    })
}

/// Target-table extraction from a single write statement.
///
/// Direct port of `writeTargetTable` in `pkg-iframe-host.tsx`: matches the
/// leading `INSERT INTO <t>` / `UPDATE <t>` / `DELETE FROM <t>`, stripping
/// optional quoting. `None` means no table could be identified, which the
/// caller treats as a refusal.
pub fn write_target_table(sql: &str) -> Option<String> {
    let sql = strip_leading_comments(sql)?;
    write_target_re()
        .iter()
        .find_map(|re| re.captures(sql))
        .map(|c| c[1].to_string())
}

fn cte_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(\w+)\s+as\s*\(").unwrap())
}

fn from_join_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)\b(?:from|join)\s+["'`\[]?(\w+)"#).unwrap())
}

/// Source-table extraction from a read statement, the read-path analogue of
/// [`write_target_table`].
///
/// Direct port of `readSourceTables` in `pkg-iframe-host.tsx`: collects every
/// table named after a `FROM`/`JOIN` keyword (stripping optional quoting) and
/// excludes CTE names introduced by a leading `WITH` (`<name> AS (…)`), which
/// resolve to inline subqueries rather than real tables. A subquery source
/// (`FROM (SELECT …)`) is skipped at its outer FROM but its inner FROM/JOIN
/// tables are still picked up by the same global scan. Returns distinct table
/// names in first-seen order; dedup is case-insensitive, the returned casing is
/// the statement's.
pub fn read_source_tables(sql: &str) -> Vec<String> {
    let mut cte_names: Vec<String> = Vec::new();
    for c in cte_re().captures_iter(sql) {
        cte_names.push(c[1].to_ascii_lowercase());
    }
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for c in from_join_re().captures_iter(sql) {
        let table = c[1].to_string();
        let key = table.to_ascii_lowercase();
        if cte_names.contains(&key) || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(table);
    }
    out
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Every table a statement touches must appear in the pkg's declared
/// `permissions["sqlite.tables"]`.
///
/// Port of `checkSqliteTableScope`: exact, case-sensitive membership — the same
/// `allowed.includes(t)` the TS runs. Case-sensitivity fails *closed*
/// (`FROM Tasks` against `["tasks"]` is refused), which is the safe direction.
pub fn check_table_scope(allowed: &[String], targets: &[String]) -> Result<(), DbScopeDenial> {
    for t in targets {
        if !allowed.iter().any(|a| a == t) {
            return Err(DbScopeDenial::new(
                DbScopeRefusal::TableOutOfScope,
                format!("table '{t}' not in the pkg's declared sqlite.tables"),
            ));
        }
    }
    Ok(())
}

/// The single gate both transports call. Returns the tables the statement was
/// cleared to touch, or the first refusal.
///
/// Order matches the iframe path's guard stack: statement kind, then multiple-
/// statement rejection (backend-only, see [`has_multiple_statements`]), then
/// table extraction, then scope. The `capabilities.sqlite` opt-in is checked by
/// the callers, which are the ones holding the manifest.
pub fn authorize(
    mode: DbMode,
    sql: &str,
    allowed: &[String],
) -> Result<Vec<String>, DbScopeDenial> {
    let kind = classify(sql);
    match (mode, kind) {
        (DbMode::Read, StatementKind::Read) | (DbMode::Write, StatementKind::Write) => {}
        (DbMode::Read, _) => {
            return Err(DbScopeDenial::new(
                DbScopeRefusal::StatementNotAllowed,
                "only SELECT/WITH read queries are allowed",
            ))
        }
        (DbMode::Write, _) => {
            return Err(DbScopeDenial::new(
                DbScopeRefusal::StatementNotAllowed,
                "only INSERT/UPDATE/DELETE write statements are allowed",
            ))
        }
    }

    if has_multiple_statements(sql) {
        return Err(DbScopeDenial::new(
            DbScopeRefusal::MultipleStatements,
            "only a single statement per request is allowed",
        ));
    }

    let targets = match mode {
        DbMode::Read => read_source_tables(sql),
        DbMode::Write => write_target_table(sql).into_iter().collect(),
    };
    if targets.is_empty() {
        return Err(DbScopeDenial::new(
            DbScopeRefusal::NoTableIdentified,
            match mode {
                DbMode::Read => "could not identify the source table(s)",
                DbMode::Write => "could not identify the target table",
            },
        ));
    }

    check_table_scope(allowed, &targets)?;
    Ok(targets)
}

/// Read a pkg's declared scope off a loaded manifest: `Some(tables)` when the
/// pkg opted into `capabilities.sqlite`, `None` when it did not (which the
/// callers turn into [`DbScopeRefusal::CapabilityMissing`]).
///
/// Only `permissions["sqlite.tables"]` counts — the deprecated
/// `supabase.tables` alias is deliberately *not* honoured here, because the
/// iframe path doesn't honour it either and the two must not drift.
pub fn declared_scope(manifest: &crate::pkg::manifest::Manifest) -> Option<Vec<String>> {
    manifest.capabilities.as_ref()?.sqlite.as_ref()?;
    Some(manifest.permissions.sqlite_tables.clone())
}

// ---------------------------------------------------------------------------
// Grants — token ⇄ pkg identity for backend processes
// ---------------------------------------------------------------------------

/// One live grant. Minted at pkg-process spawn, resolved on every request.
#[derive(Debug, Clone)]
pub struct Grant {
    pub pkg_id: String,
    /// Where to re-read `manifest.json` from on each request, so a scope
    /// narrowed on disk applies without a shell restart.
    pub install_path: PathBuf,
    token: String,
}

fn grants() -> &'static Mutex<Vec<Grant>> {
    static G: OnceLock<Mutex<Vec<Grant>>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(Vec::new()))
}

/// Base URL of the running iyke server, published once at startup so spawn
/// sites can hand it to pkg children. `None` before the bridge is up (and in
/// unit tests), in which case no env is injected and the pkg simply sees no
/// accessor — the pre-WP-23 status quo, never a widening.
fn bridge_url_cell() -> &'static OnceLock<String> {
    static U: OnceLock<String> = OnceLock::new();
    &U
}

/// Called once from `iyke::start` after the server binds.
pub fn set_bridge_url(url: &str) {
    let _ = bridge_url_cell().set(url.to_string());
}

pub fn bridge_url() -> Option<&'static str> {
    bridge_url_cell().get().map(String::as_str)
}

/// Mint (or reuse) this pkg's bearer token. One token per pkg id for the life
/// of the shell process, so a supervisor restart doesn't strand the previous
/// one; `install_path` is refreshed on every call so a dev re-mount at a new
/// path is picked up.
pub fn issue_grant(pkg_id: &str, install_path: &Path) -> String {
    let mut g = grants().lock().expect("db_scope grants poisoned");
    if let Some(existing) = g.iter_mut().find(|e| e.pkg_id == pkg_id) {
        existing.install_path = install_path.to_path_buf();
        return existing.token.clone();
    }
    let token = crate::iyke::auth::random_token_hex(32);
    g.push(Grant {
        pkg_id: pkg_id.to_string(),
        install_path: install_path.to_path_buf(),
        token: token.clone(),
    });
    token
}

/// Drop a pkg's grant — call on uninstall / dev-unregister so a token can't
/// outlive the pkg it identifies.
pub fn revoke_grant(pkg_id: &str) {
    let mut g = grants().lock().expect("db_scope grants poisoned");
    g.retain(|e| e.pkg_id != pkg_id);
}

/// Resolve a presented bearer token to its grant. Comparison is constant-time
/// over every live grant (the list is a handful of entries) so the endpoint
/// can't be probed by timing, matching `iyke::auth::require_token`.
pub fn resolve_grant(presented: &str) -> Option<Grant> {
    let g = grants().lock().expect("db_scope grants poisoned");
    let mut found: Option<Grant> = None;
    for e in g.iter() {
        if crate::iyke::auth::constant_time_eq(presented.as_bytes(), e.token.as_bytes()) {
            found = Some(e.clone());
        }
    }
    found
}

/// Layer the scoped-accessor env onto a pkg child process.
///
/// Set at every kernel spawn site (long-lived MCP servers, spawn-per-call MCP,
/// one-shot sidecars, streaming sidecars) *before* the manifest's own `env`
/// block, so an explicit manifest entry still wins — same precedence as the
/// project + settings-secret injections next to it.
///
/// No-op when the bridge URL isn't published yet; the pkg then sees no
/// accessor, which is exactly the pre-WP-23 behaviour.
pub fn inject_env(cmd: &mut tokio::process::Command, pkg_id: &str, install_path: &Path) {
    let Some(url) = bridge_url() else {
        return;
    };
    let token = issue_grant(pkg_id, install_path);
    cmd.env("IKENGA_PKG_ID", pkg_id);
    cmd.env("IKENGA_PKG_DB_URL", format!("{url}/iyke/pkg-db"));
    cmd.env("IKENGA_PKG_DB_TOKEN", token);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec![
            "meetings".to_string(),
            "meeting_transcripts".to_string(),
            "meeting_speakers".to_string(),
        ]
    }

    // -- statement classification -----------------------------------------

    #[test]
    fn classifies_reads_and_writes() {
        assert_eq!(classify("SELECT * FROM meetings"), StatementKind::Read);
        assert_eq!(
            classify("WITH r AS (SELECT 1) SELECT * FROM r"),
            StatementKind::Read
        );
        assert_eq!(
            classify("insert into meetings (id) values (1)"),
            StatementKind::Write
        );
        assert_eq!(classify("UPDATE meetings SET a=1"), StatementKind::Write);
        assert_eq!(classify("DELETE FROM meetings"), StatementKind::Write);
    }

    #[test]
    fn rejects_ddl_attach_pragma_vacuum() {
        for sql in [
            "CREATE TABLE evil (id int)",
            "DROP TABLE tasks",
            "ALTER TABLE tasks ADD COLUMN x int",
            "ATTACH DATABASE '/tmp/x.db' AS x",
            "PRAGMA table_info(tasks)",
            "VACUUM",
            "REPLACE INTO tasks VALUES (1)",
            "BEGIN",
            "",
        ] {
            assert_eq!(
                classify(sql),
                StatementKind::Rejected,
                "expected reject: {sql:?}"
            );
        }
    }

    #[test]
    fn comment_prefix_cannot_disguise_the_keyword() {
        assert_eq!(
            classify("/* SELECT */ DROP TABLE tasks"),
            StatementKind::Rejected
        );
        assert_eq!(classify("-- SELECT\nPRAGMA foo"), StatementKind::Rejected);
        assert_eq!(
            classify("/* hi */ SELECT * FROM meetings"),
            StatementKind::Read
        );
        // Unterminated block comment: no statement at all.
        assert_eq!(
            classify("/* never closed SELECT 1"),
            StatementKind::Rejected
        );
    }

    #[test]
    fn selectivity_is_word_boundary_aware() {
        // `selection` must not read as `select`.
        assert_eq!(classify("selection_of_doom()"), StatementKind::Rejected);
        assert_eq!(classify("updates_table_thing"), StatementKind::Rejected);
    }

    // -- multi-statement ---------------------------------------------------

    #[test]
    fn detects_a_second_statement() {
        assert!(has_multiple_statements(
            "INSERT INTO meetings (id) VALUES (1); DELETE FROM tasks"
        ));
        assert!(has_multiple_statements("SELECT 1;SELECT 2"));
    }

    #[test]
    fn tolerates_trailing_semicolon_and_quoted_semicolons() {
        assert!(!has_multiple_statements("SELECT * FROM meetings;"));
        assert!(!has_multiple_statements("SELECT * FROM meetings;  -- done"));
        assert!(!has_multiple_statements(
            "SELECT * FROM meetings WHERE t = 'a;b'"
        ));
        assert!(!has_multiple_statements(
            "UPDATE meetings SET title = 'x;y' WHERE id = 1"
        ));
        assert!(!has_multiple_statements("SELECT * FROM meetings /* a;b */"));
    }

    // -- table extraction (parity with the TS unit tests) ------------------

    #[test]
    fn read_source_tables_matches_the_ts_cases() {
        assert_eq!(read_source_tables("SELECT * FROM tasks"), vec!["tasks"]);
        assert_eq!(
            read_source_tables("SELECT t.id FROM tasks t JOIN sales_deals d ON d.task_id = t.id"),
            vec!["tasks", "sales_deals"]
        );
        assert_eq!(
            read_source_tables(r#"SELECT * FROM "tasks""#),
            vec!["tasks"]
        );
        assert_eq!(read_source_tables("SELECT * FROM [tasks]"), vec!["tasks"]);
        assert_eq!(
            read_source_tables("WITH recent AS (SELECT * FROM tasks) SELECT * FROM recent"),
            vec!["tasks"]
        );
        assert_eq!(
            read_source_tables("SELECT * FROM (SELECT * FROM tasks) x"),
            vec!["tasks"]
        );
        assert_eq!(
            read_source_tables("SELECT * FROM tasks a JOIN Tasks b ON a.id = b.parent_id"),
            vec!["tasks"]
        );
        assert!(read_source_tables("SELECT 1").is_empty());
    }

    #[test]
    fn write_target_table_matches_the_ts_cases() {
        assert_eq!(
            write_target_table("INSERT INTO meetings (id) VALUES (1)").as_deref(),
            Some("meetings")
        );
        assert_eq!(
            write_target_table("INSERT OR REPLACE INTO meetings (id) VALUES (1)").as_deref(),
            Some("meetings")
        );
        assert_eq!(
            write_target_table(r#"UPDATE "meetings" SET a = 1"#).as_deref(),
            Some("meetings")
        );
        assert_eq!(
            write_target_table("DELETE FROM [meetings] WHERE id = 1").as_deref(),
            Some("meetings")
        );
        assert_eq!(write_target_table("SELECT 1"), None);
    }

    // -- the gate ----------------------------------------------------------

    #[test]
    fn allows_in_scope_read_and_write() {
        assert_eq!(
            authorize(DbMode::Read, "SELECT * FROM meetings", &allowed()).unwrap(),
            vec!["meetings"]
        );
        assert_eq!(
            authorize(
                DbMode::Write,
                "INSERT INTO meetings (id) VALUES (?)",
                &allowed()
            )
            .unwrap(),
            vec!["meetings"]
        );
    }

    /// The refusal WP-23's DoD asks for, at the policy layer: a pkg scoped to
    /// the meetings tables reaching for `tasks`.
    #[test]
    fn refuses_a_read_outside_scope() {
        let err = authorize(DbMode::Read, "SELECT * FROM tasks", &allowed()).unwrap_err();
        assert_eq!(err.reason, DbScopeRefusal::TableOutOfScope);
        assert!(err.detail.contains("tasks"), "{}", err.detail);
    }

    #[test]
    fn refuses_a_write_outside_scope() {
        let err = authorize(
            DbMode::Write,
            "DELETE FROM email_messages WHERE 1=1",
            &allowed(),
        )
        .unwrap_err();
        assert_eq!(err.reason, DbScopeRefusal::TableOutOfScope);
        assert!(err.detail.contains("email_messages"), "{}", err.detail);
    }

    /// A join that smuggles one out-of-scope table alongside an allowed one is
    /// refused on the out-of-scope member, not waved through on the first.
    #[test]
    fn refuses_a_join_that_reaches_out_of_scope() {
        let err = authorize(
            DbMode::Read,
            "SELECT m.id, t.body FROM meetings m JOIN tasks t ON t.id = m.task_id",
            &allowed(),
        )
        .unwrap_err();
        assert_eq!(err.reason, DbScopeRefusal::TableOutOfScope);
        assert!(err.detail.contains("tasks"), "{}", err.detail);
    }

    #[test]
    fn refuses_a_write_on_the_read_verb_and_vice_versa() {
        assert_eq!(
            authorize(DbMode::Read, "DELETE FROM meetings", &allowed())
                .unwrap_err()
                .reason,
            DbScopeRefusal::StatementNotAllowed
        );
        assert_eq!(
            authorize(DbMode::Write, "SELECT * FROM meetings", &allowed())
                .unwrap_err()
                .reason,
            DbScopeRefusal::StatementNotAllowed
        );
    }

    #[test]
    fn refuses_ddl_on_either_verb() {
        for mode in [DbMode::Read, DbMode::Write] {
            for sql in [
                "DROP TABLE meetings",
                "ATTACH DATABASE '/tmp/x.db' AS x",
                "PRAGMA writable_schema = 1",
                "VACUUM",
            ] {
                assert_eq!(
                    authorize(mode, sql, &allowed()).unwrap_err().reason,
                    DbScopeRefusal::StatementNotAllowed,
                    "{sql:?}"
                );
            }
        }
    }

    #[test]
    fn refuses_a_trailing_statement_that_escapes_scope() {
        let err = authorize(
            DbMode::Write,
            "INSERT INTO meetings (id) VALUES (1); DELETE FROM tasks",
            &allowed(),
        )
        .unwrap_err();
        assert_eq!(err.reason, DbScopeRefusal::MultipleStatements);
    }

    #[test]
    fn refuses_a_read_with_no_identifiable_table() {
        assert_eq!(
            authorize(DbMode::Read, "SELECT 1", &allowed())
                .unwrap_err()
                .reason,
            DbScopeRefusal::NoTableIdentified
        );
    }

    /// Empty scope (the fail-closed shape a missing/unreadable manifest
    /// produces upstream) permits nothing at all.
    #[test]
    fn empty_scope_permits_nothing() {
        assert_eq!(
            authorize(DbMode::Read, "SELECT * FROM meetings", &[])
                .unwrap_err()
                .reason,
            DbScopeRefusal::TableOutOfScope
        );
    }

    /// Case mismatch fails closed rather than being normalised open — same
    /// case-sensitive `includes` the TS path runs.
    #[test]
    fn case_mismatch_fails_closed() {
        assert_eq!(
            authorize(DbMode::Read, "SELECT * FROM Meetings", &allowed())
                .unwrap_err()
                .reason,
            DbScopeRefusal::TableOutOfScope
        );
    }

    // -- grants ------------------------------------------------------------

    #[test]
    fn grants_round_trip_and_revoke() {
        let path = Path::new("/tmp/ikenga-test-pkg-grants");
        let token = issue_grant("com.ikenga.test.grants", path);
        assert_eq!(token.len(), 64, "32 random bytes, hex-encoded");
        // Same pkg re-issues the same token.
        assert_eq!(issue_grant("com.ikenga.test.grants", path), token);

        let g = resolve_grant(&token).expect("grant resolves");
        assert_eq!(g.pkg_id, "com.ikenga.test.grants");
        assert_eq!(g.install_path, path);

        assert!(resolve_grant("not-a-real-token").is_none());
        assert!(resolve_grant("").is_none());

        revoke_grant("com.ikenga.test.grants");
        assert!(
            resolve_grant(&token).is_none(),
            "revoked token must not resolve"
        );
    }
}
