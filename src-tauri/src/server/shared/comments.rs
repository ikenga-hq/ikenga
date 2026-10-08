//! Store for artifact comments (pin-mode).
//!
//! Backs the artifact-grid pane's pin overlay. A pin is a `{ artifact_path,
//! selector, text, screenshot_path, status, position }` record anchored to a
//! CSS selector inside a rendered artifact. Lifecycle: `open` →
//! `in_progress` (set by the agent via `mcp-iyke.pin_acknowledge`) → `resolved`
//! (set by user manually or by the agent via `mcp-iyke.pin_resolve`).
//!
//! Sink-routing (terminal claude vs side-pane Chat) is handled in a sibling
//! module — this one is pure persistence + lifecycle.
//!
//! Schema lives in migration 0022.
//!
//! **Where this lives.** The AppHandle-free bodies of the `comment_*` commands
//! (all but `comment_route`), shared by the desktop `#[tauri::command]`
//! wrappers (`commands::comments`, thin delegates) and the daemon's `/api/rpc`
//! arms (`server::rpc_shell`), WP-19 slice 4. Each takes the `PaDb` its caller
//! resolved and opens the pool at the same point the command always did, so
//! both surfaces return the same value or the same error.
//!
//! `pin_screenshot_write`'s body is [`write_screenshot`] (WP-19 slice 8): the
//! desktop passes its `app_data_dir` and [`ShotLimit::Desktop`], the daemon
//! its `--data-dir` and [`ShotLimit::Daemon`].

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::db::PaDb;

/// Leaf under the data dir where `pin_screenshot_write` puts element
/// screenshots (`$app_data_dir/pin-screenshots/<uuid>.png` on the desktop).
pub const SCREENSHOTS_DIR: &str = "pin-screenshots";

/// The daemon's cap on one decoded pin screenshot. The desktop has none (its
/// caller is the user's own renderer over Tauri IPC); the daemon's caller is a
/// remote token holder, so one call must not be able to fill its disk.
/// `/api/rpc`'s body limit is `server::RPC_BODY_LIMIT` (16 MiB, raised from
/// axum's 2 MiB default for editor saves), so this cap — not the body limit —
/// is what bounds a screenshot. An element
/// crop (`captureToPng` of one picked element) is tens to hundreds of KB.
pub const DAEMON_MAX_SCREENSHOT_BYTES: usize = 2 * 1024 * 1024;

/// How a pin screenshot write is bounded.
#[derive(Clone, Copy, Debug)]
pub enum ShotLimit {
    /// The desktop: no size cap, `fs::write`, the path as joined —
    /// byte-identical to the command before the move.
    Desktop,
    /// The daemon: at most `max_bytes` decoded (refused before decoding when
    /// the base64 alone is too long), the file created exclusively, and the
    /// canonical path returned — the form `comment_create` checks and stores.
    Daemon { max_bytes: usize },
}

/// `pin_screenshot_write`'s body: decode an FE-supplied base64 PNG and write
/// it to `<data_dir>/pin-screenshots/<uuid>.png` under a server-minted name.
/// No caller-supplied path is involved. Returns the file's path.
pub fn write_screenshot(
    data_dir: &Path,
    base64_png: &str,
    limit: ShotLimit,
) -> Result<String, String> {
    let bytes = decode_screenshot(base64_png, limit)?;
    store_screenshot(data_dir, &bytes, limit)
}

/// The first half of [`write_screenshot`]: the size cap (daemon), base64
/// decode and PNG magic check. Errors are the desktop's words.
pub fn decode_screenshot(base64_png: &str, limit: ShotLimit) -> Result<Vec<u8>, String> {
    let too_large = |max: usize| format!("screenshot too large: the daemon's cap is {max} bytes");
    if let ShotLimit::Daemon { max_bytes } = limit {
        // Padded STANDARD base64 of n bytes is exactly 4 * ceil(n / 3) chars.
        if base64_png.len() > max_bytes.div_ceil(3) * 4 {
            return Err(too_large(max_bytes));
        }
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64_png.as_bytes())
        .map_err(|e| format!("base64 decode: {e}"))?;
    if let ShotLimit::Daemon { max_bytes } = limit {
        if bytes.len() > max_bytes {
            return Err(too_large(max_bytes));
        }
    }
    // PNG magic: 89 50 4E 47 0D 0A 1A 0A. Reject anything else early so a
    // corrupt blob can't poison the screenshots dir with junk files.
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG (bad magic)".into());
    }
    Ok(bytes)
}

/// The second half of [`write_screenshot`]: write decoded `bytes` under a
/// fresh UUID in `<data_dir>/pin-screenshots/`.
pub fn store_screenshot(data_dir: &Path, bytes: &[u8], limit: ShotLimit) -> Result<String, String> {
    let dir = data_dir.join(SCREENSHOTS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    let name = format!("{}.png", uuid::Uuid::new_v4());
    match limit {
        ShotLimit::Desktop => {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).map_err(|e| format!("write png: {e}"))?;
            Ok(path.to_string_lossy().into_owned())
        }
        ShotLimit::Daemon { .. } => {
            use std::io::Write as _;
            let dir = dir.canonicalize().map_err(|e| format!("mkdir: {e}"))?;
            let path = dir.join(name);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("write png: {e}"))?;
            if let Err(e) = file.write_all(&bytes) {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(format!("write png: {e}"));
            }
            Ok(path.to_string_lossy().into_owned())
        }
    }
}

const VALID_STATUSES: &[&str] = &["open", "in_progress", "resolved", "stale"];
const VALID_SINKS: &[&str] = &["terminal", "sidepane", "both", "clipboard", "chi"];

fn validate_status(s: &str) -> Result<(), String> {
    if VALID_STATUSES.contains(&s) {
        Ok(())
    } else {
        Err(format!(
            "invalid status '{s}' (expected one of: {})",
            VALID_STATUSES.join(", ")
        ))
    }
}

fn validate_sink(s: &str) -> Result<(), String> {
    if VALID_SINKS.contains(&s) {
        Ok(())
    } else {
        Err(format!(
            "invalid sink '{s}' (expected one of: {})",
            VALID_SINKS.join(", ")
        ))
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: i64,
    #[serde(rename = "artifactPath")]
    pub artifact_path: String,
    pub selector: String,
    pub text: String,
    #[serde(rename = "screenshotPath")]
    pub screenshot_path: Option<String>,
    /// `open` | `in_progress` | `resolved` | `stale`.
    pub status: String,
    #[serde(rename = "positionX")]
    pub position_x: Option<f64>,
    #[serde(rename = "positionY")]
    pub position_y: Option<f64>,
    #[serde(rename = "threadId")]
    pub thread_id: Option<String>,
    #[serde(rename = "openingSessionId")]
    pub opening_session_id: Option<String>,
    /// `terminal` | `sidepane` | `both` — audit field, null until routed.
    pub sink: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "acknowledgedAt")]
    pub acknowledged_at: Option<i64>,
    #[serde(rename = "resolvedAt")]
    pub resolved_at: Option<i64>,
    /// G-ACCESS §4.5.4 (WP-76, column from 0070): who wrote it. `None` for
    /// the Owner's own comments (and every row from before 0070); a share
    /// member's comment carries the member's `principal_id`, so a non-Owner
    /// may change only their own rows.
    #[serde(rename = "authorPrincipalId")]
    pub author_principal_id: Option<String>,
}

type CommentRow = (
    i64,            // id
    String,         // artifact_path
    String,         // selector
    String,         // text
    Option<String>, // screenshot_path
    String,         // status
    Option<f64>,    // position_x
    Option<f64>,    // position_y
    Option<String>, // thread_id
    Option<String>, // opening_session_id
    Option<String>, // sink
    i64,            // created_at
    Option<i64>,    // acknowledged_at
    Option<i64>,    // resolved_at
    Option<String>, // author_principal_id
);

const COMMENT_COLUMNS: &str = "id, artifact_path, selector, text, screenshot_path, \
    status, position_x, position_y, thread_id, opening_session_id, sink, \
    created_at, acknowledged_at, resolved_at, author_principal_id";

fn row_to_comment(row: CommentRow) -> Comment {
    Comment {
        id: row.0,
        artifact_path: row.1,
        selector: row.2,
        text: row.3,
        screenshot_path: row.4,
        status: row.5,
        position_x: row.6,
        position_y: row.7,
        thread_id: row.8,
        opening_session_id: row.9,
        sink: row.10,
        created_at: row.11,
        acknowledged_at: row.12,
        resolved_at: row.13,
        author_principal_id: row.14,
    }
}

pub async fn create(
    db: &PaDb,
    artifact_path: String,
    selector: String,
    text: String,
    screenshot_path: Option<String>,
    position_x: Option<f64>,
    position_y: Option<f64>,
) -> Result<Comment, String> {
    create_as(
        db,
        None,
        artifact_path,
        selector,
        text,
        screenshot_path,
        position_x,
        position_y,
    )
    .await
}

/// [`create`] with an author (G-ACCESS §4.5.4, WP-76): a share member's
/// comment is stamped with the member's `principal_id`, taken from the
/// broker's `X-Ikenga-Share-Principal` — never from the request body.
#[allow(clippy::too_many_arguments)]
pub async fn create_as(
    db: &PaDb,
    author_principal_id: Option<String>,
    artifact_path: String,
    selector: String,
    text: String,
    screenshot_path: Option<String>,
    position_x: Option<f64>,
    position_y: Option<f64>,
) -> Result<Comment, String> {
    if artifact_path.trim().is_empty() {
        return Err("artifact_path cannot be empty".into());
    }
    if selector.trim().is_empty() {
        return Err("selector cannot be empty".into());
    }
    if text.trim().is_empty() {
        return Err("text cannot be empty".into());
    }
    let pool = db.ensure_pool().await?;
    let created_at = now_millis();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO artifact_comments (artifact_path, selector, text, screenshot_path, \
         status, position_x, position_y, created_at, author_principal_id) \
         VALUES (?, ?, ?, ?, 'open', ?, ?, ?, ?) RETURNING id",
    )
    .bind(&artifact_path)
    .bind(&selector)
    .bind(&text)
    .bind(&screenshot_path)
    .bind(position_x)
    .bind(position_y)
    .bind(created_at)
    .bind(&author_principal_id)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("insert comment: {e}"))?;

    let row: CommentRow = sqlx::query_as(&format!(
        "SELECT {COMMENT_COLUMNS} FROM artifact_comments WHERE id = ?"
    ))
    .bind(id)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("read created comment: {e}"))?;
    Ok(row_to_comment(row))
}

/// A comment's `artifact_path` and `author_principal_id` (G-ACCESS §4.5.4:
/// the share pre-hook's ownership and confinement check). `None` when the
/// row doesn't exist.
pub async fn path_and_author(
    db: &PaDb,
    id: i64,
) -> Result<Option<(String, Option<String>)>, String> {
    let pool = db.ensure_pool().await?;
    sqlx::query_as("SELECT artifact_path, author_principal_id FROM artifact_comments WHERE id = ?")
        .bind(id)
        .fetch_optional(&pool)
        .await
        .map_err(|e| format!("read comment: {e}"))
}

pub async fn get(db: &PaDb, id: i64) -> Result<Comment, String> {
    let pool = db.ensure_pool().await?;
    let row: CommentRow = sqlx::query_as(&format!(
        "SELECT {COMMENT_COLUMNS} FROM artifact_comments WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("read comment: {e}"))?
    .ok_or_else(|| format!("comment {id} not found"))?;
    Ok(row_to_comment(row))
}

pub async fn list(
    db: &PaDb,
    artifact_path: Option<String>,
    include_resolved: Option<bool>,
) -> Result<Vec<Comment>, String> {
    let pool = db.ensure_pool().await?;
    let include = include_resolved.unwrap_or(false);

    let rows: Vec<CommentRow> = match (artifact_path.as_deref(), include) {
        (Some(path), true) => {
            sqlx::query_as(&format!(
                "SELECT {COMMENT_COLUMNS} FROM artifact_comments \
             WHERE artifact_path = ? ORDER BY created_at ASC"
            ))
            .bind(path)
            .fetch_all(&pool)
            .await
        }
        (Some(path), false) => {
            sqlx::query_as(&format!(
                "SELECT {COMMENT_COLUMNS} FROM artifact_comments \
             WHERE artifact_path = ? AND status != 'resolved' \
             ORDER BY created_at ASC"
            ))
            .bind(path)
            .fetch_all(&pool)
            .await
        }
        (None, true) => {
            sqlx::query_as(&format!(
                "SELECT {COMMENT_COLUMNS} FROM artifact_comments \
             ORDER BY status, created_at DESC"
            ))
            .fetch_all(&pool)
            .await
        }
        (None, false) => {
            sqlx::query_as(&format!(
                "SELECT {COMMENT_COLUMNS} FROM artifact_comments \
             WHERE status != 'resolved' ORDER BY status, created_at DESC"
            ))
            .fetch_all(&pool)
            .await
        }
    }
    .map_err(|e| format!("list comments: {e}"))?;

    Ok(rows.into_iter().map(row_to_comment).collect())
}

/// Update the routing audit fields after the dispatcher has decided where the
/// pin went. Called by the routing dispatcher once the prompt is queued.
pub async fn record_routing(
    db: &PaDb,
    id: i64,
    sink: String,
    thread_id: Option<String>,
    opening_session_id: Option<String>,
) -> Result<Comment, String> {
    validate_sink(&sink)?;
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "UPDATE artifact_comments \
         SET sink = ?, thread_id = COALESCE(?, thread_id), \
             opening_session_id = COALESCE(?, opening_session_id) \
         WHERE id = ?",
    )
    .bind(&sink)
    .bind(&thread_id)
    .bind(&opening_session_id)
    .bind(id)
    .execute(&pool)
    .await
    .map_err(|e| format!("record routing: {e}"))?;
    get(db, id).await
}

/// Set the status. The agent uses this via mcp-iyke when transitioning to
/// `in_progress` (and stamps `acknowledged_at` on first transition) or
/// `resolved` (and stamps `resolved_at`).
pub async fn set_status(db: &PaDb, id: i64, status: String) -> Result<Comment, String> {
    validate_status(&status)?;
    let pool = db.ensure_pool().await?;
    let now = now_millis();

    // We stamp the transition timestamps only on the first move into each
    // state; later re-transitions (e.g. reopen → in_progress) leave the
    // earliest stamp in place for the audit trail.
    let sql = match status.as_str() {
        "in_progress" => {
            "UPDATE artifact_comments \
             SET status = 'in_progress', \
                 acknowledged_at = COALESCE(acknowledged_at, ?) \
             WHERE id = ?"
        }
        "resolved" => {
            "UPDATE artifact_comments \
             SET status = 'resolved', \
                 resolved_at = COALESCE(resolved_at, ?) \
             WHERE id = ?"
        }
        _ => "UPDATE artifact_comments SET status = ?2 WHERE id = ?1",
    };

    if status == "in_progress" || status == "resolved" {
        sqlx::query(sql)
            .bind(now)
            .bind(id)
            .execute(&pool)
            .await
            .map_err(|e| format!("set status: {e}"))?;
    } else {
        sqlx::query("UPDATE artifact_comments SET status = ? WHERE id = ?")
            .bind(&status)
            .bind(id)
            .execute(&pool)
            .await
            .map_err(|e| format!("set status: {e}"))?;
    }

    get(db, id).await
}

pub async fn delete(db: &PaDb, id: i64) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    sqlx::query("DELETE FROM artifact_comments WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await
        .map_err(|e| format!("delete comment: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_status() {
        assert!(validate_status("queued").is_err());
        assert!(validate_status("open").is_ok());
        assert!(validate_status("in_progress").is_ok());
        assert!(validate_status("resolved").is_ok());
        assert!(validate_status("stale").is_ok());
    }

    #[test]
    fn rejects_unknown_sink() {
        assert!(validate_sink("kafka").is_err());
        assert!(validate_sink("terminal").is_ok());
        assert!(validate_sink("sidepane").is_ok());
        assert!(validate_sink("both").is_ok());
        assert!(validate_sink("clipboard").is_ok());
        assert!(validate_sink("chi").is_ok());
    }
}
