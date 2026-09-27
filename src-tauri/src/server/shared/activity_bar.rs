//! Store for user-level activity-bar pinning.
//!
//! Two domains:
//!
//!   * **Sections** — free-form user-created groups of pins. The id is a
//!     slug (lowercase, kebab-cased) chosen at create time; the label is
//!     the display string. Reserved ids (`system`, `settings`) are
//!     host-owned and rejected here.
//!
//!   * **Pins** — user pins of artifacts, routes, files, external URLs, or
//!     pkg-owned routes. A pin may belong to a section (by id) or sit
//!     section-less (`section_id = NULL`). Reorder is per-section: passing
//!     the empty string as `section_id` means "no section".
//!
//! Both tables live in `ikenga.db` (migration 0010). Mutations go through the
//! shared `PaDb` pool; the schema migration is also embedded in
//! `commands/db.rs::ensure_schema` so fresh installs and dev pools both
//! see the schema.
//!
//! **Where this lives.** The AppHandle-free bodies of the `activity_pins_*` /
//! `activity_sections_*` commands, shared by the desktop `#[tauri::command]`
//! wrappers (`commands::activity_bar`, thin delegates) and the daemon's
//! `/api/rpc` arms (`server::rpc_shell`), WP-19 slice 4. Each takes the `PaDb`
//! its caller resolved and opens the pool at the same point the command always
//! did, so both surfaces return the same value or the same error.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::PaDb;

const RESERVED_SECTION_IDS: &[&str] = &["system", "settings"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub label: String,
    #[serde(rename = "iconLucide")]
    pub icon_lucide: Option<String>,
    #[serde(rename = "iconEmoji")]
    pub icon_emoji: Option<String>,
    #[serde(rename = "sortOrder")]
    pub sort_order: i64,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pin {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub label: String,
    #[serde(rename = "iconLucide")]
    pub icon_lucide: Option<String>,
    #[serde(rename = "iconEmoji")]
    pub icon_emoji: Option<String>,
    #[serde(rename = "sectionId")]
    pub section_id: Option<String>,
    #[serde(rename = "sortOrder")]
    pub sort_order: i64,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// Stable artifact id from the manifest's `<script id="ikenga-manifest">`.
    /// Lookup key for `ikenga://artifact/<id>` resolution. Unique among pins
    /// that have it; non-artifact pins leave this NULL.
    #[serde(rename = "manifestId")]
    pub manifest_id: Option<String>,
    /// ISO-8601 UTC timestamp of the most recent open via the resolver.
    /// Set by `activity_pins_touch_open`; null until first open.
    #[serde(rename = "lastOpenedAt")]
    pub last_opened_at: Option<String>,
}

/// Tuple type the row queries fetch_as into. Pulled out as an alias because
/// 11 positional fields in a turbofish on every call site is unreadable.
type PinRow = (
    String,         // id
    String,         // kind
    String,         // target
    String,         // label
    Option<String>, // icon_lucide
    Option<String>, // icon_emoji
    Option<String>, // section_id
    i64,            // sort_order
    String,         // created_at
    Option<String>, // manifest_id
    Option<String>, // last_opened_at
);

const PIN_COLUMNS: &str = "id, kind, target, label, icon_lucide, icon_emoji, \
    section_id, sort_order, created_at, manifest_id, last_opened_at";

fn row_to_pin(row: PinRow) -> Pin {
    Pin {
        id: row.0,
        kind: row.1,
        target: row.2,
        label: row.3,
        icon_lucide: row.4,
        icon_emoji: row.5,
        section_id: row.6,
        sort_order: row.7,
        created_at: row.8,
        manifest_id: row.9,
        last_opened_at: row.10,
    }
}

const VALID_PIN_KINDS: &[&str] = &["artifact", "route", "file", "external", "pkg-route"];

fn validate_pin_kind(kind: &str) -> Result<(), String> {
    if VALID_PIN_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(format!(
            "invalid pin kind '{kind}' (expected one of: {})",
            VALID_PIN_KINDS.join(", ")
        ))
    }
}

fn validate_section_id(id: &str) -> Result<(), String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err("section id cannot be empty".into());
    }
    if RESERVED_SECTION_IDS.contains(&trimmed) {
        return Err(format!("'{trimmed}' is a reserved section id"));
    }
    // Sections accept lowercase letters, digits, '-', '_'. Tighter than the
    // free-form label; ids are used in URLs / iyke routes elsewhere so we
    // keep them shell-safe.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(format!(
            "section id '{trimmed}' must be lowercase ascii [a-z0-9_-]"
        ));
    }
    Ok(())
}

// ─── Sections ──────────────────────────────────────────────────────────────

pub async fn sections_list(db: &PaDb) -> Result<Vec<Section>, String> {
    let pool = db.ensure_pool().await?;
    let rows = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, i64, String)>(
        "SELECT id, label, icon_lucide, icon_emoji, sort_order, created_at \
         FROM activity_bar_sections \
         ORDER BY sort_order ASC, created_at ASC",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("query sections: {e}"))?;
    Ok(rows
        .into_iter()
        .map(
            |(id, label, icon_lucide, icon_emoji, sort_order, created_at)| Section {
                id,
                label,
                icon_lucide,
                icon_emoji,
                sort_order,
                created_at,
            },
        )
        .collect())
}

pub async fn sections_create(
    db: &PaDb,
    id: String,
    label: String,
    icon_lucide: Option<String>,
    icon_emoji: Option<String>,
) -> Result<Section, String> {
    validate_section_id(&id)?;
    let label_trim = label.trim();
    if label_trim.is_empty() {
        return Err("label cannot be empty".into());
    }

    let pool = db.ensure_pool().await?;
    // Place new section at the end.
    let next_order: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM activity_bar_sections")
            .fetch_one(&pool)
            .await
            .map_err(|e| format!("next sort_order: {e}"))?;
    sqlx::query(
        "INSERT INTO activity_bar_sections (id, label, icon_lucide, icon_emoji, sort_order) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(label_trim)
    .bind(icon_lucide.as_deref())
    .bind(icon_emoji.as_deref())
    .bind(next_order)
    .execute(&pool)
    .await
    .map_err(|e| format!("insert section: {e}"))?;

    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, i64, String)>(
        "SELECT id, label, icon_lucide, icon_emoji, sort_order, created_at \
         FROM activity_bar_sections WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("read back section: {e}"))?;
    Ok(Section {
        id: row.0,
        label: row.1,
        icon_lucide: row.2,
        icon_emoji: row.3,
        sort_order: row.4,
        created_at: row.5,
    })
}

pub async fn sections_update(
    db: &PaDb,
    id: String,
    label: Option<String>,
    icon_lucide: Option<Option<String>>,
    icon_emoji: Option<Option<String>>,
) -> Result<Section, String> {
    validate_section_id(&id)?;
    let pool = db.ensure_pool().await?;

    if let Some(l) = &label {
        let trimmed = l.trim();
        if trimmed.is_empty() {
            return Err("label cannot be empty".into());
        }
        sqlx::query("UPDATE activity_bar_sections SET label = ? WHERE id = ?")
            .bind(trimmed)
            .bind(&id)
            .execute(&pool)
            .await
            .map_err(|e| format!("update label: {e}"))?;
    }
    if let Some(v) = icon_lucide {
        sqlx::query("UPDATE activity_bar_sections SET icon_lucide = ? WHERE id = ?")
            .bind(v.as_deref())
            .bind(&id)
            .execute(&pool)
            .await
            .map_err(|e| format!("update icon_lucide: {e}"))?;
    }
    if let Some(v) = icon_emoji {
        sqlx::query("UPDATE activity_bar_sections SET icon_emoji = ? WHERE id = ?")
            .bind(v.as_deref())
            .bind(&id)
            .execute(&pool)
            .await
            .map_err(|e| format!("update icon_emoji: {e}"))?;
    }

    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, i64, String)>(
        "SELECT id, label, icon_lucide, icon_emoji, sort_order, created_at \
         FROM activity_bar_sections WHERE id = ?",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("read back section: {e}"))?;
    Ok(Section {
        id: row.0,
        label: row.1,
        icon_lucide: row.2,
        icon_emoji: row.3,
        sort_order: row.4,
        created_at: row.5,
    })
}

pub async fn sections_remove(db: &PaDb, id: String) -> Result<(), String> {
    validate_section_id(&id)?;
    let pool = db.ensure_pool().await?;
    // ON DELETE SET NULL on activity_bar_pins.section_id re-parents pins.
    sqlx::query("DELETE FROM activity_bar_sections WHERE id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .map_err(|e| format!("delete section: {e}"))?;
    Ok(())
}

// ─── Pins ──────────────────────────────────────────────────────────────────

pub async fn pins_list(db: &PaDb) -> Result<Vec<Pin>, String> {
    let pool = db.ensure_pool().await?;
    let rows = sqlx::query_as::<_, PinRow>(&format!(
        "SELECT {PIN_COLUMNS} \
         FROM activity_bar_pins \
         ORDER BY section_id IS NULL, section_id ASC, sort_order ASC, created_at ASC",
    ))
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("query pins: {e}"))?;
    Ok(rows.into_iter().map(row_to_pin).collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn pins_add(
    db: &PaDb,
    kind: String,
    target: String,
    label: String,
    icon_lucide: Option<String>,
    icon_emoji: Option<String>,
    section_id: Option<String>,
    manifest_id: Option<String>,
) -> Result<Pin, String> {
    validate_pin_kind(&kind)?;
    if target.trim().is_empty() {
        return Err("target cannot be empty".into());
    }
    let label_trim = label.trim();
    if label_trim.is_empty() {
        return Err("label cannot be empty".into());
    }
    if let Some(sid) = &section_id {
        validate_section_id(sid)?;
    }
    let manifest_id_trim: Option<String> = manifest_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    if let Some(mid) = &manifest_id_trim {
        // Same shell-safe charset rule as section ids — manifest_ids land in
        // ikenga://artifact/<id> URIs and the artifact-format spec already
        // restricts them to /^[a-z0-9-]+$/. Re-validating here keeps callers
        // honest if they bypass the FE.
        if !mid
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!(
                "manifest_id '{mid}' must match /^[a-z0-9-]+$/ (per artifact format v0)"
            ));
        }
    }

    let pool = db.ensure_pool().await?;

    // Reject pins that point at sections that don't exist. This matches the
    // FE flow (host prompts user to create a section first).
    if let Some(sid) = &section_id {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM activity_bar_sections WHERE id = ?")
                .bind(sid)
                .fetch_optional(&pool)
                .await
                .map_err(|e| format!("section lookup: {e}"))?;
        if exists.is_none() {
            return Err(format!("section '{sid}' does not exist"));
        }
    }

    // Pre-check manifest_id collision so the error message is friendlier than
    // the raw "UNIQUE constraint failed: …" SQLite throws. The unique index
    // is the source of truth either way.
    if let Some(mid) = &manifest_id_trim {
        let collision: Option<String> =
            sqlx::query_scalar("SELECT id FROM activity_bar_pins WHERE manifest_id = ?")
                .bind(mid)
                .fetch_optional(&pool)
                .await
                .map_err(|e| format!("manifest_id lookup: {e}"))?;
        if let Some(existing) = collision {
            return Err(format!(
                "manifest_id '{mid}' is already pinned (pin id {existing})"
            ));
        }
    }

    // Place new pin at the end of its section.
    let next_order: i64 = match &section_id {
        Some(sid) => sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM activity_bar_pins WHERE section_id = ?",
        )
        .bind(sid)
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("next sort_order: {e}"))?,
        None => sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM activity_bar_pins WHERE section_id IS NULL",
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("next sort_order: {e}"))?,
    };

    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO activity_bar_pins \
         (id, kind, target, label, icon_lucide, icon_emoji, section_id, sort_order, manifest_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&kind)
    .bind(target.trim())
    .bind(label_trim)
    .bind(icon_lucide.as_deref())
    .bind(icon_emoji.as_deref())
    .bind(section_id.as_deref())
    .bind(next_order)
    .bind(manifest_id_trim.as_deref())
    .execute(&pool)
    .await
    .map_err(|e| format!("insert pin: {e}"))?;

    let row = sqlx::query_as::<_, PinRow>(&format!(
        "SELECT {PIN_COLUMNS} FROM activity_bar_pins WHERE id = ?",
    ))
    .bind(&id)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("read back pin: {e}"))?;
    Ok(row_to_pin(row))
}

/// Look up a pinned artifact by its manifest id. Returns None if no pin
/// claims this id. Used by the `ikenga://artifact/<id>` URI resolver in
/// `src/lib/panes/pane-address-resolver.ts` to find the on-disk path before
/// mounting the artifact pane.
///
/// This does NOT update `last_opened_at` — call `activity_pins_touch_open`
/// separately once the pane actually mounts. Splitting the two means a
/// pre-flight resolve (e.g. for URL-bar autocomplete) doesn't bump the
/// recency timestamp.
pub async fn pins_resolve_artifact(db: &PaDb, manifest_id: &str) -> Result<Option<Pin>, String> {
    let pool = db.ensure_pool().await?;
    resolve_artifact_inner(&pool, manifest_id).await
}

async fn resolve_artifact_inner(
    pool: &sqlx::SqlitePool,
    manifest_id: &str,
) -> Result<Option<Pin>, String> {
    let mid = manifest_id.trim();
    if mid.is_empty() {
        return Err("manifest_id cannot be empty".into());
    }
    let row = sqlx::query_as::<_, PinRow>(&format!(
        "SELECT {PIN_COLUMNS} FROM activity_bar_pins WHERE manifest_id = ?",
    ))
    .bind(mid)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("resolve artifact: {e}"))?;
    Ok(row.map(row_to_pin))
}

/// Stamp `last_opened_at` to "now" for a pin. Called by the resolver right
/// after a successful artifact open so the activity bar can sort
/// recently-used pins to the top later.
pub async fn pins_touch_open(db: &PaDb, pin_id: &str) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    touch_open_inner(&pool, pin_id).await
}

async fn touch_open_inner(pool: &sqlx::SqlitePool, pin_id: &str) -> Result<(), String> {
    let id = pin_id.trim();
    if id.is_empty() {
        return Err("pin_id cannot be empty".into());
    }
    let result = sqlx::query("UPDATE activity_bar_pins SET last_opened_at = ? WHERE id = ?")
        // SQLite's `datetime('now')` returns "YYYY-MM-DD HH:MM:SS" without a
        // timezone marker. Format on the Rust side so the column always holds
        // an ISO-8601-with-Z string the FE can parse straight into Date.
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("touch open: {e}"))?;
    if result.rows_affected() == 0 {
        return Err(format!("no pin with id '{id}'"));
    }
    Ok(())
}

pub async fn pins_remove(db: &PaDb, id: String) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    sqlx::query("DELETE FROM activity_bar_pins WHERE id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .map_err(|e| format!("delete pin: {e}"))?;
    Ok(())
}

pub async fn pins_reorder(
    db: &PaDb,
    ordered_ids: Vec<String>,
    section_id: String,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    // section_id "" sentinel means "no section" (NULL in SQL).
    let target_section: Option<&str> = if section_id.is_empty() {
        None
    } else {
        validate_section_id(&section_id)?;
        Some(section_id.as_str())
    };

    let mut tx = pool.begin().await.map_err(|e| format!("begin tx: {e}"))?;
    for (idx, pin_id) in ordered_ids.iter().enumerate() {
        let res = match target_section {
            Some(sid) => {
                sqlx::query(
                    "UPDATE activity_bar_pins SET sort_order = ?, section_id = ? \
                     WHERE id = ?",
                )
                .bind(idx as i64)
                .bind(sid)
                .bind(pin_id)
                .execute(&mut *tx)
                .await
            }
            None => {
                sqlx::query(
                    "UPDATE activity_bar_pins SET sort_order = ?, section_id = NULL \
                     WHERE id = ?",
                )
                .bind(idx as i64)
                .bind(pin_id)
                .execute(&mut *tx)
                .await
            }
        };
        res.map_err(|e| format!("reorder pin {pin_id}: {e}"))?;
    }
    tx.commit()
        .await
        .map_err(|e| format!("commit reorder: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_reserved_section_ids() {
        assert!(validate_section_id("system").is_err());
        assert!(validate_section_id("settings").is_err());
    }

    #[test]
    fn rejects_empty_section_id() {
        assert!(validate_section_id("").is_err());
        assert!(validate_section_id("   ").is_err());
    }

    #[test]
    fn rejects_invalid_chars() {
        assert!(validate_section_id("Finance").is_err());
        assert!(validate_section_id("my section").is_err());
        assert!(validate_section_id("foo/bar").is_err());
    }

    #[test]
    fn accepts_valid_section_ids() {
        assert!(validate_section_id("finance").is_ok());
        assert!(validate_section_id("my-section").is_ok());
        assert!(validate_section_id("custom_2").is_ok());
    }

    #[test]
    fn validates_pin_kinds() {
        assert!(validate_pin_kind("artifact").is_ok());
        assert!(validate_pin_kind("route").is_ok());
        assert!(validate_pin_kind("file").is_ok());
        assert!(validate_pin_kind("external").is_ok());
        assert!(validate_pin_kind("pkg-route").is_ok());
        assert!(validate_pin_kind("bogus").is_err());
        assert!(validate_pin_kind("").is_err());
    }

    // ─── DB-backed tests for the artifact-pin plumbing (Phase 2) ───────────

    use crate::db::PaDb;

    /// Spin up an isolated PaDb on a tempdir-backed sqlite file. ensure_pool
    /// runs all migrations end-to-end, so each test exercises the real
    /// 0010 + 0019 schema (no hand-rolled CREATE TABLE).
    async fn fresh_db() -> (PaDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("pa.db"));
        // Touch the pool so migrations apply before the test body asserts.
        db.ensure_pool().await.expect("ensure_pool");
        (db, tmp)
    }

    /// Insert a pin via the public command path so we exercise the real
    /// validation + insert code, not a hand-rolled INSERT.
    async fn insert_artifact_pin(
        pool: &sqlx::SqlitePool,
        target: &str,
        label: &str,
        manifest_id: Option<&str>,
    ) -> Pin {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO activity_bar_pins \
             (id, kind, target, label, sort_order, manifest_id) \
             VALUES (?, 'artifact', ?, ?, 0, ?)",
        )
        .bind(&id)
        .bind(target)
        .bind(label)
        .bind(manifest_id)
        .execute(pool)
        .await
        .expect("insert pin");
        let row = sqlx::query_as::<_, PinRow>(&format!(
            "SELECT {PIN_COLUMNS} FROM activity_bar_pins WHERE id = ?",
        ))
        .bind(&id)
        .fetch_one(pool)
        .await
        .expect("read back");
        row_to_pin(row)
    }

    #[tokio::test]
    async fn migration_0019_adds_columns() {
        // Sanity: the columns are queryable. If 0019 didn't run, the SELECT
        // explodes with "no such column". This catches a missing entry in
        // ensure_schema's migration list, which is a class of bug we've hit
        // before with new migrations.
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query("SELECT manifest_id, last_opened_at FROM activity_bar_pins LIMIT 1")
            .execute(&pool)
            .await
            .expect("manifest_id + last_opened_at columns must exist after 0019");
    }

    #[tokio::test]
    async fn resolve_artifact_returns_pin_for_known_id() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        let pin = insert_artifact_pin(&pool, "/tmp/cfo.html", "CFO Daily", Some("cfo-daily")).await;
        let resolved = resolve_artifact_inner(&pool, "cfo-daily")
            .await
            .unwrap()
            .expect("pin found");
        assert_eq!(resolved.id, pin.id);
        assert_eq!(resolved.target, "/tmp/cfo.html");
        assert_eq!(resolved.manifest_id.as_deref(), Some("cfo-daily"));
    }

    #[tokio::test]
    async fn resolve_artifact_returns_none_for_unknown_id() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        insert_artifact_pin(&pool, "/tmp/x.html", "X", Some("known")).await;
        let resolved = resolve_artifact_inner(&pool, "absent").await.unwrap();
        assert!(resolved.is_none());
    }

    #[tokio::test]
    async fn resolve_artifact_rejects_empty_id() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        assert!(resolve_artifact_inner(&pool, "").await.is_err());
        assert!(resolve_artifact_inner(&pool, "   ").await.is_err());
    }

    #[tokio::test]
    async fn touch_open_stamps_iso8601() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        let pin = insert_artifact_pin(&pool, "/tmp/x.html", "X", Some("x")).await;
        assert!(pin.last_opened_at.is_none(), "starts unset");

        touch_open_inner(&pool, &pin.id).await.unwrap();

        let after = resolve_artifact_inner(&pool, "x")
            .await
            .unwrap()
            .expect("still resolves");
        let stamp = after
            .last_opened_at
            .expect("stamped after touch_open_inner");
        // RFC3339 starts with a 4-digit year + dash. Crude but enough to catch
        // a regression to "datetime('now')" output (which would lack the T).
        assert!(
            stamp.contains('T') && stamp.len() >= 20,
            "expected RFC3339 timestamp, got {stamp:?}",
        );
    }

    #[tokio::test]
    async fn touch_open_errors_on_missing_pin() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        let err = touch_open_inner(&pool, "nope").await.unwrap_err();
        assert!(err.contains("nope"), "error names the missing pin: {err}");
    }

    #[tokio::test]
    async fn touch_open_rejects_empty_id() {
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        assert!(touch_open_inner(&pool, "").await.is_err());
    }

    #[tokio::test]
    async fn unique_index_blocks_duplicate_manifest_ids() {
        // Two pins fighting for the same artifact id would make ikenga://
        // resolution non-deterministic. Migration 0019 declares a partial
        // unique index; this confirms it actually fires at insert time.
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        insert_artifact_pin(&pool, "/tmp/a.html", "A", Some("dup")).await;

        let err = sqlx::query(
            "INSERT INTO activity_bar_pins \
             (id, kind, target, label, sort_order, manifest_id) \
             VALUES ('xxx', 'artifact', '/tmp/b.html', 'B', 0, 'dup')",
        )
        .execute(&pool)
        .await
        .expect_err("unique index should block this");
        assert!(
            format!("{err:?}").to_lowercase().contains("unique"),
            "expected UNIQUE-constraint error, got {err:?}",
        );
    }

    #[tokio::test]
    async fn null_manifest_ids_dont_collide() {
        // Partial index: WHERE manifest_id IS NOT NULL. So multiple pins
        // with NULL manifest_id (the common case for non-artifact pins or
        // one-shot artifacts) must coexist freely.
        let (db, _tmp) = fresh_db().await;
        let pool = db.ensure_pool().await.unwrap();
        insert_artifact_pin(&pool, "/a", "A", None).await;
        insert_artifact_pin(&pool, "/b", "B", None).await;
        insert_artifact_pin(&pool, "/c", "C", None).await;
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM activity_bar_pins WHERE manifest_id IS NULL")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 3);
    }
}
