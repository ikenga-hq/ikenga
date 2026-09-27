//! Store for unified artifact-studio chat threads.
//!
//! One thread per folder (D3 in
//! `plans/shell/2026-05-16-artifact-studio-unified.md`). The "scope chip"
//! (folder · artifact · element · compare) travels with each message in
//! `scope_chip_json` so the thread re-scopes contextually without forking.
//!
//! Schema lives in migration 0023.
//!
//! Phase 0 surface — read/write primitives only. Wire-up to the actual
//! engine (claude session, scope-aware prompt prefix, fs_watch / hot-reload)
//! happens in Phase 2.
//!
//! **Where this lives.** The AppHandle-free bodies of the `studio_thread_*` /
//! `studio_message_*` commands, shared by the desktop `#[tauri::command]`
//! wrappers (`commands::studio_threads`, thin delegates) and the daemon's
//! `/api/rpc` arms (`server::rpc_shell`), WP-19 slice 4. Each takes the `PaDb`
//! its caller resolved and opens the pool at the same point the command always
//! did, so both surfaces return the same value or the same error.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::PaDb;

const VALID_ROLES: &[&str] = &["user", "claude", "tool"];

fn validate_role(role: &str) -> Result<(), String> {
    if VALID_ROLES.contains(&role) {
        Ok(())
    } else {
        Err(format!(
            "invalid role '{role}' (expected one of: {})",
            VALID_ROLES.join(", ")
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
pub struct StudioThread {
    pub id: String,
    #[serde(rename = "folderPath")]
    pub folder_path: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(rename = "lastMessageAt")]
    pub last_message_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioMessage {
    pub id: i64,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    /// `user` | `claude` | `tool`.
    pub role: String,
    #[serde(rename = "contentMd")]
    pub content_md: String,
    /// Raw JSON describing the scope chip the message was sent under.
    /// Renderer parses; nullable for tool turns that inherit the parent's chip.
    #[serde(rename = "scopeChipJson")]
    pub scope_chip_json: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

type ThreadRow = (String, String, i64, i64);
type MessageRow = (i64, String, String, String, Option<String>, i64);

const THREAD_COLUMNS: &str = "id, folder_path, created_at, last_message_at";
const MESSAGE_COLUMNS: &str = "id, thread_id, role, content_md, scope_chip_json, created_at";

fn row_to_thread(row: ThreadRow) -> StudioThread {
    StudioThread {
        id: row.0,
        folder_path: row.1,
        created_at: row.2,
        last_message_at: row.3,
    }
}

fn row_to_message(row: MessageRow) -> StudioMessage {
    StudioMessage {
        id: row.0,
        thread_id: row.1,
        role: row.2,
        content_md: row.3,
        scope_chip_json: row.4,
        created_at: row.5,
    }
}

/// Idempotent: returns the existing thread for this folder or creates a new
/// one. The FE calls this on Studio pane mount; the thread row outlives the
/// pane so reopening the same folder resumes the conversation.
pub async fn thread_get_or_create(db: &PaDb, folder_path: String) -> Result<StudioThread, String> {
    let trimmed = folder_path.trim();
    if trimmed.is_empty() {
        return Err("folder_path cannot be empty".into());
    }
    let pool = db.ensure_pool().await?;

    let existing: Option<ThreadRow> = sqlx::query_as(&format!(
        "SELECT {THREAD_COLUMNS} FROM studio_threads WHERE folder_path = ?"
    ))
    .bind(trimmed)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("lookup thread: {e}"))?;

    if let Some(row) = existing {
        return Ok(row_to_thread(row));
    }

    let id = Uuid::new_v4().to_string();
    let now = now_millis();
    sqlx::query(
        "INSERT INTO studio_threads (id, folder_path, created_at, last_message_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(trimmed)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| format!("insert thread: {e}"))?;

    Ok(StudioThread {
        id,
        folder_path: trimmed.to_string(),
        created_at: now,
        last_message_at: now,
    })
}

pub async fn thread_get(db: &PaDb, id: String) -> Result<StudioThread, String> {
    let pool = db.ensure_pool().await?;
    let row: ThreadRow = sqlx::query_as(&format!(
        "SELECT {THREAD_COLUMNS} FROM studio_threads WHERE id = ?"
    ))
    .bind(&id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("read thread: {e}"))?
    .ok_or_else(|| format!("thread {id} not found"))?;
    Ok(row_to_thread(row))
}

/// List recently active threads (most recent `last_message_at` first). Used
/// by the Studio's "recent boards" / nav-restore surface.
pub async fn thread_list_recent(
    db: &PaDb,
    limit: Option<i64>,
) -> Result<Vec<StudioThread>, String> {
    let pool = db.ensure_pool().await?;
    let lim = limit.unwrap_or(50).clamp(1, 500);
    let rows: Vec<ThreadRow> = sqlx::query_as(&format!(
        "SELECT {THREAD_COLUMNS} FROM studio_threads \
         ORDER BY last_message_at DESC LIMIT ?"
    ))
    .bind(lim)
    .fetch_all(&pool)
    .await
    .map_err(|e| format!("list threads: {e}"))?;
    Ok(rows.into_iter().map(row_to_thread).collect())
}

/// Append a message to a thread. Bumps `last_message_at` on the parent.
/// Caller is responsible for shaping `content_md` (markdown for user/claude,
/// JSON or text for `tool`). `scope_chip_json` is opaque to the backend
/// beyond being stored verbatim — the FE renders it.
pub async fn message_append(
    db: &PaDb,
    thread_id: String,
    role: String,
    content_md: String,
    scope_chip_json: Option<String>,
) -> Result<StudioMessage, String> {
    validate_role(&role)?;
    if content_md.is_empty() {
        return Err("content_md cannot be empty".into());
    }
    let pool = db.ensure_pool().await?;
    let now = now_millis();

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO studio_messages (thread_id, role, content_md, scope_chip_json, created_at) \
         VALUES (?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&thread_id)
    .bind(&role)
    .bind(&content_md)
    .bind(&scope_chip_json)
    .bind(now)
    .fetch_one(&pool)
    .await
    .map_err(|e| format!("insert message: {e}"))?;

    sqlx::query("UPDATE studio_threads SET last_message_at = ? WHERE id = ?")
        .bind(now)
        .bind(&thread_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("bump thread: {e}"))?;

    Ok(StudioMessage {
        id,
        thread_id,
        role,
        content_md,
        scope_chip_json,
        created_at: now,
    })
}

/// List messages in a thread. Ordered chronologically (oldest first). `limit`
/// caps the tail — pass `None` for the full thread (small threads are common).
/// `before_created_at` is for paging older messages when the user scrolls up.
pub async fn message_list(
    db: &PaDb,
    thread_id: String,
    limit: Option<i64>,
    before_created_at: Option<i64>,
) -> Result<Vec<StudioMessage>, String> {
    let pool = db.ensure_pool().await?;
    let lim = limit.unwrap_or(500).clamp(1, 2000);

    let rows: Vec<MessageRow> = if let Some(before) = before_created_at {
        sqlx::query_as(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM studio_messages \
             WHERE thread_id = ? AND created_at < ? \
             ORDER BY created_at ASC LIMIT ?"
        ))
        .bind(&thread_id)
        .bind(before)
        .bind(lim)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query_as(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM studio_messages \
             WHERE thread_id = ? ORDER BY created_at ASC LIMIT ?"
        ))
        .bind(&thread_id)
        .bind(lim)
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| format!("list messages: {e}"))?;

    Ok(rows.into_iter().map(row_to_message).collect())
}

/// Delete a thread and all its messages (ON DELETE CASCADE handles the
/// children). The FE is expected to confirm before calling — there is no
/// undo. Mainly used for cleaning up stale boards from the recent list.
pub async fn thread_delete(db: &PaDb, id: String) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    sqlx::query("DELETE FROM studio_threads WHERE id = ?")
        .bind(&id)
        .execute(&pool)
        .await
        .map_err(|e| format!("delete thread: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_role() {
        assert!(validate_role("system").is_err());
        assert!(validate_role("user").is_ok());
        assert!(validate_role("claude").is_ok());
        assert!(validate_role("tool").is_ok());
    }
}
