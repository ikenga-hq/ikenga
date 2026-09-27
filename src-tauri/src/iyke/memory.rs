//! Memory + coordination bridge endpoints (Phase 1 of
//! projects-first-class). Implements `pkgs/mcp-iyke/DESIGN.md` §4-6.
//!
//! Wire scope is a single string: "workspace" | "pkg:<id>" | "project:<id>"
//! | "seat:<project>/<name>" (G-SEATS §3, DEC-68).
//! `scope` omitted on a write resolves to the active project at request
//! time (DESIGN.md §1 amendment for the projects-first-class plan). A
//! `seat:` scope is never expanded from a bare name: callers pass the full
//! address a seat view reports.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::{extract::Query, http::StatusCode, Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::commands::db::PaDb;
use crate::commands::projects::get_active_project_id;

/// Wake handle for the timer firing loop. When a new timer is scheduled
/// or an existing one cancelled, the request handler calls `notify_one`
/// so the loop re-reads the next-pending row without waiting for its
/// 30s poll fallback.
#[derive(Clone, Default)]
pub struct TimerScheduler {
    notify: Arc<Notify>,
}

impl TimerScheduler {
    pub fn new() -> Self {
        Self::default()
    }
    fn wake(&self) {
        self.notify.notify_one();
    }
    async fn wait(&self) {
        self.notify.notified().await;
    }
}

const MAX_SCRATCHPAD_BYTES: usize = 1_000_000;
const MAX_KV_VALUE_BYTES: usize = 64_000;
const NAME_PATTERN_MAX: usize = 120;
const KV_KEY_MAX: usize = 200;
const TITLE_MAX: usize = 300;
const BODY_MAX: usize = 100_000;
const LOCK_DEFAULT_TTL_MS: i64 = 60_000;
const LOCK_MAX_TTL_MS: i64 = 600_000;
const LOCK_WAIT_MAX_MS: u64 = 30_000;
const SCRATCHPAD_WATCH_MAX_MS: u64 = 60_000;

fn scratchpad_version() -> &'static AtomicI64 {
    static VERSION: AtomicI64 = AtomicI64::new(0);
    &VERSION
}

fn observe_scratchpad_version(version: i64) {
    let _ = scratchpad_version().fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        (version > current).then_some(version)
    });
}

fn next_scratchpad_version() -> i64 {
    let now = now_ms();
    scratchpad_version()
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some((current + 1).max(now))
        })
        .unwrap_or(0)
        .max(now)
}

fn scratchpad_notifiers() -> &'static Mutex<HashMap<String, Arc<Notify>>> {
    static NOTIFIERS: OnceLock<Mutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    NOTIFIERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scratchpad_tombstones() -> &'static Mutex<HashMap<String, i64>> {
    static TOMBSTONES: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    TOMBSTONES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn scratchpad_key(scope: &str, name: &str) -> String {
    format!("{scope}\0{name}")
}

fn scratchpad_notifier(scope: &str, name: &str) -> Arc<Notify> {
    let key = scratchpad_key(scope, name);
    let mut notifiers = scratchpad_notifiers()
        .lock()
        .expect("scratchpad notifier lock");
    notifiers.entry(key).or_default().clone()
}

pub(crate) fn scratchpad_changed(scope: &str, name: &str, version: i64, deleted: bool) {
    observe_scratchpad_version(version);
    let key = scratchpad_key(scope, name);
    if let Ok(mut tombstones) = scratchpad_tombstones().lock() {
        if deleted {
            tombstones.insert(key, version);
        } else {
            tombstones.remove(&key);
        }
    }
    scratchpad_notifier(scope, name).notify_waiters();
}

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (status, msg.into())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn validate_name(name: &str) -> Result<(), (StatusCode, String)> {
    if name.is_empty() || name.len() > NAME_PATTERN_MAX {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("name length out of range: {}", name.len()),
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphanumeric()) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "name must start with [a-zA-Z0-9]".to_string(),
        ));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "name has invalid chars".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_scope(scope: &str) -> Result<(), (StatusCode, String)> {
    if scope == "workspace" {
        return Ok(());
    }
    if let Some(rest) = scope.strip_prefix("pkg:") {
        if rest.is_empty() || rest.len() > 128 {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "invalid pkg scope".to_string(),
            ));
        }
        return Ok(());
    }
    if let Some(rest) = scope.strip_prefix("project:") {
        if rest.is_empty() || rest.len() > 64 {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "invalid project scope".to_string(),
            ));
        }
        return Ok(());
    }
    // G-SEATS §3.1/§3.2: `seat:<project>/<name>`. Grammar only — `<project>`
    // stays opaque here, like the `project:` arm (§3.3). The project segment
    // follows `projects.rs::validate_slug` (shared copy, agreement-tested in
    // `seats.rs`); the name follows the seat grammar, which admits no `/`.
    if let Some(rest) = scope.strip_prefix("seat:") {
        let (project, name) = rest
            .split_once('/')
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, "invalid seat scope"))?;
        if super::seats::validate_project_slug(project).is_err()
            || super::seats::validate_seat_name(name).is_err()
        {
            return Err(err(StatusCode::BAD_REQUEST, "invalid seat scope"));
        }
        return Ok(());
    }
    Err(err(
        StatusCode::BAD_REQUEST,
        format!("invalid scope: {scope}"),
    ))
}

async fn resolve_scope(
    pool: &SqlitePool,
    scope: Option<String>,
) -> Result<String, (StatusCode, String)> {
    let s = match scope {
        Some(s) => s,
        None => {
            let active = get_active_project_id(pool)
                .await
                .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
            return Ok(format!("project:{active}"));
        }
    };
    validate_scope(&s)?;
    Ok(s)
}

// ═══════════════════════════════════════════════════════════════════════
// Scratchpads
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct ScratchpadWriteBody {
    pub scope: Option<String>,
    pub name: String,
    pub body: String,
}

#[derive(Deserialize)]
pub struct ScratchpadAppendBody {
    pub scope: Option<String>,
    pub name: String,
    pub body: String,
    #[serde(default = "default_true")]
    pub with_separator: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub struct ScopeNameQuery {
    pub scope: Option<String>,
    pub name: String,
}

#[derive(Deserialize)]
pub struct ScopeOnlyQuery {
    pub scope: Option<String>,
}

#[derive(Deserialize)]
pub struct ScratchpadWatchQuery {
    pub scope: Option<String>,
    pub name: String,
    #[serde(default)]
    pub since: i64,
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Serialize)]
struct ScratchpadInfo {
    id: String,
    name: String,
    updated_at: i64,
    preview: String,
}

pub async fn post_scratchpad_write(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<ScratchpadWriteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.body.len() > MAX_SCRATCHPAD_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body > 1 MB".to_string(),
        ));
    }
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    validate_name(&body.name)?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let now = now_ms();
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO iyke_scratchpads (id, scope, name, body, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(scope, name) DO UPDATE SET
             body = excluded.body,
             updated_at = MAX(iyke_scratchpads.updated_at + 1, excluded.updated_at)",
    )
    .bind(&id)
    .bind(&scope)
    .bind(&body.name)
    .bind(&body.body)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratchpad write: {e}"),
        )
    })?;
    let (final_id, updated_at): (String, i64) =
        sqlx::query_as("SELECT id, updated_at FROM iyke_scratchpads WHERE scope = ? AND name = ?")
            .bind(&scope)
            .bind(&body.name)
            .fetch_one(&pool)
            .await
            .map_err(|e| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scratchpad lookup: {e}"),
                )
            })?;
    scratchpad_changed(&scope, &body.name, updated_at, false);
    let _ = app.emit(
        "iyke://scratchpad-changed",
        json!({ "scope": scope, "name": body.name, "action": "write", "updated_at": updated_at }),
    );
    Ok(Json(
        json!({ "id": final_id, "scope": scope, "updated_at": updated_at }),
    ))
}

pub async fn post_scratchpad_append(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<ScratchpadAppendBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    validate_name(&body.name)?;
    let scope = resolve_scope(&pool, body.scope).await?;

    if body.body.len() > MAX_SCRATCHPAD_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body > 1 MB".to_string(),
        ));
    }
    let now = now_ms();
    let separator = if body.with_separator {
        format!("\n\n---\n_{}_\n\n", chrono_like(now))
    } else {
        String::new()
    };
    let append_body = format!("{separator}{}", body.body);
    let id = Uuid::new_v4().to_string();
    let result: Option<(String, i64)> = sqlx::query_as(
        "INSERT INTO iyke_scratchpads (id, scope, name, body, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(scope, name) DO UPDATE SET
             body = iyke_scratchpads.body || ?,
             updated_at = MAX(iyke_scratchpads.updated_at + 1, excluded.updated_at)
         WHERE length(CAST(iyke_scratchpads.body AS BLOB)) + ? <= ?
         RETURNING id, updated_at",
    )
    .bind(&id)
    .bind(&scope)
    .bind(&body.name)
    .bind(&body.body)
    .bind(now)
    .bind(now)
    .bind(&append_body)
    .bind(append_body.len() as i64)
    .bind(MAX_SCRATCHPAD_BYTES as i64)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratchpad append: {e}"),
        )
    })?;
    let Some((final_id, updated_at)) = result else {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "combined body > 1 MB".to_string(),
        ));
    };
    scratchpad_changed(&scope, &body.name, updated_at, false);
    let _ = app.emit(
        "iyke://scratchpad-changed",
        json!({ "scope": scope, "name": body.name, "action": "append", "updated_at": updated_at }),
    );
    Ok(Json(
        json!({ "id": final_id, "scope": scope, "updated_at": updated_at }),
    ))
}

fn chrono_like(unix_ms: i64) -> String {
    // Minimal ISO-8601 UTC without pulling chrono.
    let secs = unix_ms / 1000;
    let days = secs / 86_400;
    let _rem = secs % 86_400;
    // Cheap approximation: epoch-relative; use the full ms in the body for now.
    format!("ts={unix_ms} (~{days}d since epoch)")
}

pub async fn get_scratchpad_read(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<ScopeNameQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    validate_name(&q.name)?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let row: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT id, body, updated_at FROM iyke_scratchpads WHERE scope = ? AND name = ?",
    )
    .bind(&scope)
    .bind(&q.name)
    .fetch_optional(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratchpad read: {e}"),
        )
    })?;
    match row {
        Some((id, body, updated_at)) => Ok(Json(json!({
            "id": id, "scope": scope, "name": q.name, "body": body, "updated_at": updated_at
        }))),
        None => Err(err(
            StatusCode::NOT_FOUND,
            "scratchpad not found".to_string(),
        )),
    }
}

pub async fn get_scratchpad_watch(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<ScratchpadWatchQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    validate_name(&q.name)?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let notifier = scratchpad_notifier(&scope, &q.name);
    let read_change = || async {
        sqlx::query_as::<_, (String, String, i64)>(
            "SELECT id, body, updated_at FROM iyke_scratchpads WHERE scope = ? AND name = ? AND updated_at > ?",
        )
        .bind(&scope)
        .bind(&q.name)
        .bind(q.since)
        .fetch_optional(&pool)
        .await
    };
    let notified = notifier.notified();
    tokio::pin!(notified);
    let mut row = read_change().await.map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratchpad watch: {e}"),
        )
    })?;
    let deleted_version = scratchpad_tombstones()
        .lock()
        .ok()
        .and_then(|tombstones| tombstones.get(&scratchpad_key(&scope, &q.name)).copied())
        .filter(|version| *version > q.since);
    if row.is_none() && deleted_version.is_none() {
        if let Some(wait_ms) = q.wait_ms.filter(|wait_ms| *wait_ms > 0) {
            let _ = tokio::time::timeout(
                Duration::from_millis(wait_ms.min(SCRATCHPAD_WATCH_MAX_MS)),
                &mut notified,
            )
            .await;
            row = read_change().await.map_err(|e| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("scratchpad watch: {e}"),
                )
            })?;
        }
    }
    if let Some((id, body, updated_at)) = row {
        return Ok(Json(json!({
            "updated": true,
            "deleted": false,
            "id": id,
            "scope": scope,
            "name": q.name,
            "body": body,
            "updated_at": updated_at
        })));
    }
    let deleted_version = scratchpad_tombstones()
        .lock()
        .ok()
        .and_then(|tombstones| tombstones.get(&scratchpad_key(&scope, &q.name)).copied())
        .filter(|version| *version > q.since);
    match deleted_version {
        Some(updated_at) => Ok(Json(json!({
            "updated": true,
            "deleted": true,
            "scope": scope,
            "name": q.name,
            "updated_at": updated_at
        }))),
        None => Ok(Json(json!({ "updated": false }))),
    }
}

pub async fn get_scratchpad_list(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<ScopeOnlyQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let rows = sqlx::query(
        "SELECT id, name, body, updated_at FROM iyke_scratchpads WHERE scope = ? ORDER BY updated_at DESC",
    )
    .bind(&scope)
    .fetch_all(&pool)
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("scratchpad list: {e}")))?;
    let scratchpads: Vec<ScratchpadInfo> = rows
        .iter()
        .map(|r| {
            let body: String = r.get("body");
            let preview: String = body.chars().take(200).collect();
            ScratchpadInfo {
                id: r.get("id"),
                name: r.get("name"),
                updated_at: r.get("updated_at"),
                preview,
            }
        })
        .collect();
    Ok(Json(json!({ "scope": scope, "scratchpads": scratchpads })))
}

#[derive(Deserialize)]
pub struct ScratchpadDeleteBody {
    pub scope: Option<String>,
    pub name: String,
}

pub async fn post_scratchpad_delete(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<ScratchpadDeleteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    sqlx::query("DELETE FROM iyke_scratchpads WHERE scope = ? AND name = ?")
        .bind(&scope)
        .bind(&body.name)
        .execute(&pool)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("scratchpad delete: {e}"),
            )
        })?;
    let updated_at = next_scratchpad_version();
    scratchpad_changed(&scope, &body.name, updated_at, true);
    let _ = app.emit(
        "iyke://scratchpad-changed",
        json!({ "scope": scope, "name": body.name, "action": "delete", "updated_at": updated_at }),
    );
    Ok(Json(json!({ "ok": true })))
}

// ═══════════════════════════════════════════════════════════════════════
// KV
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct KvSetBody {
    pub scope: Option<String>,
    pub key: String,
    pub value: Value,
}

fn validate_kv_key(key: &str) -> Result<(), (StatusCode, String)> {
    if key.is_empty() || key.len() > KV_KEY_MAX {
        return Err(err(StatusCode::BAD_REQUEST, "kv key length".to_string()));
    }
    for c in key.chars() {
        if !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ':' || c == '/' || c == '-')
        {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "kv key invalid char".to_string(),
            ));
        }
    }
    Ok(())
}

pub async fn post_kv_set(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<KvSetBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    validate_kv_key(&body.key)?;
    let serialized = serde_json::to_string(&body.value)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("value not JSON: {e}")))?;
    if serialized.len() > MAX_KV_VALUE_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            "value > 64 KB".to_string(),
        ));
    }
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let now = now_ms();
    sqlx::query(
        "INSERT INTO iyke_kv (scope, key, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(scope, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(&scope)
    .bind(&body.key)
    .bind(&serialized)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("kv set: {e}")))?;
    Ok(Json(json!({ "updated_at": now })))
}

#[derive(Deserialize)]
pub struct KvGetQuery {
    pub scope: Option<String>,
    pub key: String,
}

pub async fn get_kv_get(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<KvGetQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    validate_kv_key(&q.key)?;
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let row: Option<(String, i64)> =
        sqlx::query_as("SELECT value, updated_at FROM iyke_kv WHERE scope = ? AND key = ?")
            .bind(&scope)
            .bind(&q.key)
            .fetch_optional(&pool)
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("kv get: {e}")))?;
    match row {
        Some((s, updated_at)) => {
            let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
            Ok(Json(
                json!({ "key": q.key, "value": v, "updated_at": updated_at }),
            ))
        }
        None => Ok(Json(
            json!({ "key": q.key, "value": Value::Null, "updated_at": Value::Null }),
        )),
    }
}

#[derive(Deserialize)]
pub struct KvDeleteBody {
    pub scope: Option<String>,
    pub key: String,
}

pub async fn post_kv_delete(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<KvDeleteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    validate_kv_key(&body.key)?;
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    sqlx::query("DELETE FROM iyke_kv WHERE scope = ? AND key = ?")
        .bind(&scope)
        .bind(&body.key)
        .execute(&pool)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("kv delete: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct KvListQuery {
    pub scope: Option<String>,
    pub prefix: Option<String>,
}

pub async fn get_kv_list(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<KvListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let rows = if let Some(prefix) = q.prefix.filter(|s| !s.is_empty()) {
        let like = format!("{prefix}%");
        sqlx::query(
            "SELECT key, value, updated_at FROM iyke_kv WHERE scope = ? AND key LIKE ? ORDER BY key",
        )
        .bind(&scope)
        .bind(&like)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query("SELECT key, value, updated_at FROM iyke_kv WHERE scope = ? ORDER BY key")
            .bind(&scope)
            .fetch_all(&pool)
            .await
    }
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("kv list: {e}")))?;
    let entries: Vec<Value> = rows
        .iter()
        .map(|r| {
            let s: String = r.get("value");
            let v: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
            json!({ "key": r.get::<String, _>("key"), "value": v, "updated_at": r.get::<i64, _>("updated_at") })
        })
        .collect();
    Ok(Json(json!({ "scope": scope, "entries": entries })))
}

// ═══════════════════════════════════════════════════════════════════════
// Locks
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct LockAcquireBody {
    pub scope: Option<String>,
    pub resource: String,
    pub holder: String,
    pub ttl_ms: Option<i64>,
    pub wait_ms: Option<u64>,
}

pub async fn post_lock_acquire(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<LockAcquireBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ttl = body
        .ttl_ms
        .unwrap_or(LOCK_DEFAULT_TTL_MS)
        .clamp(1000, LOCK_MAX_TTL_MS);
    let wait = body.wait_ms.unwrap_or(0).min(LOCK_WAIT_MAX_MS);
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let start = std::time::Instant::now();
    loop {
        let now = now_ms();
        let res = sqlx::query(
            "INSERT INTO iyke_locks (scope, resource, holder, acquired_at, expires_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(scope, resource) DO UPDATE SET
               holder = excluded.holder,
               acquired_at = excluded.acquired_at,
               expires_at = excluded.expires_at
             WHERE iyke_locks.expires_at < ?",
        )
        .bind(&scope)
        .bind(&body.resource)
        .bind(&body.holder)
        .bind(now)
        .bind(now + ttl)
        .bind(now)
        .execute(&pool)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("lock acquire: {e}"),
            )
        })?;
        if res.rows_affected() > 0 {
            return Ok(Json(json!({
                "acquired": true,
                "scope": scope,
                "resource": body.resource,
                "expires_at": now + ttl,
            })));
        }
        let elapsed = start.elapsed().as_millis() as u64;
        if elapsed >= wait {
            let row: Option<(String, i64)> = sqlx::query_as(
                "SELECT holder, expires_at FROM iyke_locks WHERE scope = ? AND resource = ?",
            )
            .bind(&scope)
            .bind(&body.resource)
            .fetch_optional(&pool)
            .await
            .map_err(|e| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("lock status: {e}"),
                )
            })?;
            return Ok(Json(json!({
                "acquired": false,
                "held_by": row.as_ref().map(|(h, _)| h.clone()),
                "expires_at": row.map(|(_, e)| e),
            })));
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

#[derive(Deserialize)]
pub struct LockStatusQuery {
    pub scope: Option<String>,
    pub resource: String,
}

pub async fn get_lock_status(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<LockStatusQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let row: Option<(String, i64)> = sqlx::query_as(
        "SELECT holder, expires_at FROM iyke_locks WHERE scope = ? AND resource = ? AND expires_at > ?",
    )
    .bind(&scope)
    .bind(&q.resource)
    .bind(now_ms())
    .fetch_optional(&pool)
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("lock status: {e}")))?;
    Ok(Json(json!({
        "held": row.is_some(),
        "holder": row.as_ref().map(|(h, _)| h.clone()),
        "expires_at": row.map(|(_, e)| e),
    })))
}

#[derive(Deserialize)]
pub struct LockReleaseBody {
    pub scope: Option<String>,
    pub resource: String,
    pub holder: String,
}

pub async fn post_lock_release(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<LockReleaseBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let res = sqlx::query("DELETE FROM iyke_locks WHERE scope = ? AND resource = ? AND holder = ?")
        .bind(&scope)
        .bind(&body.resource)
        .bind(&body.holder)
        .execute(&pool)
        .await
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("lock release: {e}"),
            )
        })?;
    Ok(Json(json!({ "released": res.rows_affected() > 0 })))
}

#[derive(Deserialize)]
pub struct LockRenewBody {
    pub scope: Option<String>,
    pub resource: String,
    pub holder: String,
    pub ttl_ms: Option<i64>,
}

pub async fn post_lock_renew(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<LockRenewBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let ttl = body
        .ttl_ms
        .unwrap_or(LOCK_DEFAULT_TTL_MS)
        .clamp(1000, LOCK_MAX_TTL_MS);
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let now = now_ms();
    let res = sqlx::query(
        "UPDATE iyke_locks SET expires_at = ?
         WHERE scope = ? AND resource = ? AND holder = ? AND expires_at > ?",
    )
    .bind(now + ttl)
    .bind(&scope)
    .bind(&body.resource)
    .bind(&body.holder)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("lock renew: {e}"),
        )
    })?;
    Ok(Json(
        json!({ "renewed": res.rows_affected() > 0, "expires_at": now + ttl }),
    ))
}

// ═══════════════════════════════════════════════════════════════════════
// Agents
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct AgentRegisterBody {
    pub id: Option<String>,
    pub name: String,
    pub model: Option<String>,
    pub metadata: Option<Value>,
}

pub async fn post_agent_register(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<AgentRegisterBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let id = body.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let now = now_ms();
    let meta = body
        .metadata
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".to_string());
    sqlx::query(
        "INSERT INTO iyke_agents (id, name, model, metadata, registered_at, last_seen_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             model = excluded.model,
             metadata = excluded.metadata,
             last_seen_at = excluded.last_seen_at",
    )
    .bind(&id)
    .bind(&body.name)
    .bind(&body.model)
    .bind(&meta)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("agent register: {e}"),
        )
    })?;
    Ok(Json(json!({ "id": id, "registered_at": now })))
}

// ═══════════════════════════════════════════════════════════════════════
// Todos
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
pub struct TodoCreateBody {
    pub scope: Option<String>,
    pub title: String,
    pub body: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub blocker_id: Option<String>,
}

pub async fn post_todo_create(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<TodoCreateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    if body.title.is_empty() || body.title.chars().count() > TITLE_MAX {
        return Err(err(StatusCode::BAD_REQUEST, "title length".to_string()));
    }
    if let Some(b) = &body.body {
        if b.len() > BODY_MAX {
            return Err(err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body > 100 KB".to_string(),
            ));
        }
    }
    let id = Uuid::new_v4().to_string();
    let now = now_ms();
    let tags_json = serde_json::to_string(&body.tags).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "INSERT INTO iyke_todos (id, scope, title, body, status, tags, blocker_id, assignee, created_at, updated_at)
         VALUES (?, ?, ?, ?, 'open', ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&scope)
    .bind(&body.title)
    .bind(&body.body)
    .bind(&tags_json)
    .bind(&body.blocker_id)
    .bind(&body.assignee)
    .bind(now)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("todo create: {e}")))?;
    Ok(Json(json!({ "id": id, "scope": scope, "created_at": now })))
}

#[derive(Deserialize)]
pub struct TodoListQuery {
    pub scope: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub assignee: Option<String>,
}

pub async fn get_todo_list(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<TodoListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let mut sql = String::from(
        "SELECT id, scope, title, body, status, tags, blocker_id, assignee, created_at, updated_at, completed_at
         FROM iyke_todos WHERE scope = ?",
    );
    if q.status.is_some() {
        sql.push_str(" AND status = ?");
    }
    if q.assignee.is_some() {
        sql.push_str(" AND assignee = ?");
    }
    sql.push_str(" ORDER BY created_at DESC");
    let mut qb = sqlx::query(&sql).bind(&scope);
    if let Some(s) = &q.status {
        qb = qb.bind(s);
    }
    if let Some(a) = &q.assignee {
        qb = qb.bind(a);
    }
    let rows = qb
        .fetch_all(&pool)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("todo list: {e}")))?;
    let mut todos: Vec<Value> = Vec::new();
    for r in rows.iter() {
        let tags_raw: String = r.get("tags");
        let tags: Vec<String> = serde_json::from_str(&tags_raw).unwrap_or_default();
        if let Some(filter) = &q.tag {
            if !tags.iter().any(|t| t == filter) {
                continue;
            }
        }
        todos.push(json!({
            "id": r.get::<String, _>("id"),
            "scope": r.get::<String, _>("scope"),
            "title": r.get::<String, _>("title"),
            "body": r.get::<Option<String>, _>("body"),
            "status": r.get::<String, _>("status"),
            "tags": tags,
            "blocker_id": r.get::<Option<String>, _>("blocker_id"),
            "assignee": r.get::<Option<String>, _>("assignee"),
            "created_at": r.get::<i64, _>("created_at"),
            "updated_at": r.get::<i64, _>("updated_at"),
            "completed_at": r.get::<Option<i64>, _>("completed_at"),
        }));
    }
    Ok(Json(json!({ "scope": scope, "todos": todos })))
}

// ─── WP-09: agent inbox ──────────────────────────────────────────────────────
//
// `fire_due_timer` has always written into `iyke_agent_inbox`, and the sweeper
// has always pruned it — but nothing could ever read it. A scheduled timer
// fired into a table no client could see, which made `/iyke/timer/schedule`
// effectively a no-op for agents. These two routes close that loop.
//
// Ack deletes rather than marking (decision D-4): the table has no `acked_at`
// column, adding one would mean a migration for a queue whose rows the sweeper
// already discards, and delete-on-ack is what a mailbox cursor actually wants.

#[derive(Deserialize)]
pub struct AgentInboxQuery {
    pub agent_id: String,
    /// Exclusive cursor — return only entries created after this timestamp.
    /// Feed `next_since` from the previous response back in.
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn get_agent_inbox(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<AgentInboxQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if q.agent_id.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "agent_id must not be empty"));
    }
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let since = q.since.unwrap_or(0);
    let rows = sqlx::query(
        "SELECT id, agent_id, kind, payload, created_at
         FROM iyke_agent_inbox
         WHERE agent_id = ? AND created_at > ?
         ORDER BY created_at ASC, id ASC
         LIMIT ?",
    )
    .bind(&q.agent_id)
    .bind(since)
    .bind(limit)
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("agent inbox: {e}"),
        )
    })?;

    let mut entries: Vec<Value> = Vec::new();
    let mut next_since = since;
    for r in rows.iter() {
        let created_at: i64 = r.get("created_at");
        next_since = next_since.max(created_at);
        let payload_raw: String = r.get("payload");
        entries.push(json!({
            "id": r.get::<String, _>("id"),
            "agent_id": r.get::<String, _>("agent_id"),
            "kind": r.get::<String, _>("kind"),
            // Hand back parsed JSON when it parses, else the raw string — a
            // malformed payload should still be visible, not swallowed.
            "payload": serde_json::from_str::<Value>(&payload_raw)
                .unwrap_or(Value::String(payload_raw)),
            "created_at": created_at,
        }));
    }
    Ok(Json(json!({
        "agent_id": q.agent_id,
        "entries": entries,
        "next_since": next_since,
    })))
}

#[derive(Deserialize)]
pub struct AgentInboxAckBody {
    pub ids: Vec<String>,
}

pub async fn post_agent_inbox_ack(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<AgentInboxAckBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.ids.is_empty() {
        return Ok(Json(json!({ "ok": true, "deleted": 0 })));
    }
    if body.ids.len() > 500 {
        return Err(err(StatusCode::BAD_REQUEST, "at most 500 ids per ack"));
    }
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let placeholders = vec!["?"; body.ids.len()].join(",");
    let sql = format!("DELETE FROM iyke_agent_inbox WHERE id IN ({placeholders})");
    let mut qb = sqlx::query(&sql);
    for id in &body.ids {
        qb = qb.bind(id);
    }
    let result = qb
        .execute(&pool)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("inbox ack: {e}")))?;
    Ok(Json(
        json!({ "ok": true, "deleted": result.rows_affected() }),
    ))
}

#[derive(Deserialize)]
pub struct TodoIdBody {
    pub id: String,
}

pub async fn post_todo_complete(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<TodoIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let now = now_ms();
    let res = sqlx::query(
        "UPDATE iyke_todos SET status = 'done', completed_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(now)
    .bind(now)
    .bind(&body.id)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("todo complete: {e}"),
        )
    })?;
    if res.rows_affected() == 0 {
        return Err(err(StatusCode::NOT_FOUND, "todo not found".to_string()));
    }
    Ok(Json(json!({ "id": body.id, "completed_at": now })))
}

#[derive(Deserialize)]
pub struct TodoUpdateBody {
    pub id: String,
    pub status: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub assignee: Option<String>,
    pub blocker_id: Option<String>,
}

pub async fn post_todo_update(
    Extension(db): Extension<Arc<PaDb>>,
    Json(body): Json<TodoUpdateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut sets: Vec<String> = Vec::new();
    if body.status.is_some() {
        sets.push("status = ?".to_string());
    }
    if body.title.is_some() {
        sets.push("title = ?".to_string());
    }
    if body.body.is_some() {
        sets.push("body = ?".to_string());
    }
    if body.assignee.is_some() {
        sets.push("assignee = ?".to_string());
    }
    if body.blocker_id.is_some() {
        sets.push("blocker_id = ?".to_string());
    }
    if sets.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "no fields to update".to_string(),
        ));
    }
    sets.push("updated_at = ?".to_string());
    let sql = format!("UPDATE iyke_todos SET {} WHERE id = ?", sets.join(", "));
    let mut qb = sqlx::query(&sql);
    if let Some(s) = &body.status {
        qb = qb.bind(s);
    }
    if let Some(t) = &body.title {
        qb = qb.bind(t);
    }
    if let Some(b) = &body.body {
        qb = qb.bind(b);
    }
    if let Some(a) = &body.assignee {
        qb = qb.bind(a);
    }
    if let Some(bk) = &body.blocker_id {
        qb = qb.bind(bk);
    }
    let now = now_ms();
    qb = qb.bind(now).bind(&body.id);
    let res = qb.execute(&pool).await.map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("todo update: {e}"),
        )
    })?;
    if res.rows_affected() == 0 {
        return Err(err(StatusCode::NOT_FOUND, "todo not found".to_string()));
    }
    Ok(Json(json!({ "id": body.id, "updated_at": now })))
}

// ═══════════════════════════════════════════════════════════════════════
// Timers
// ═══════════════════════════════════════════════════════════════════════

const TIMER_TITLE_MAX: usize = 300;
const TIMER_BODY_MAX: usize = 4_000;
const TIMER_MAX_DELAY_MS: i64 = 365 * 24 * 60 * 60 * 1000; // 1 year
const TIMER_INBOX_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Deserialize)]
pub struct TimerScheduleBody {
    pub scope: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub agent_id: Option<String>,
    /// Absolute epoch-ms wall-clock fire time. Mutually exclusive with delay_ms.
    pub fire_at: Option<i64>,
    /// Relative delay in milliseconds from "now". Mutually exclusive with fire_at.
    pub delay_ms: Option<i64>,
}

pub async fn post_timer_schedule(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(sched): Extension<TimerScheduler>,
    Json(body): Json<TimerScheduleBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.title.is_empty() || body.title.chars().count() > TIMER_TITLE_MAX {
        return Err(err(StatusCode::BAD_REQUEST, "title length".to_string()));
    }
    if let Some(b) = &body.body {
        if b.len() > TIMER_BODY_MAX {
            return Err(err(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body > 4 KB".to_string(),
            ));
        }
    }
    // `iyke_timers.agent_id` has an FK to `iyke_agents(id)`. Without this check
    // an unregistered agent gets a raw `(code: 787) FOREIGN KEY constraint
    // failed` back, which says nothing about what to do. Now that WP-09 makes
    // timers actually deliver (they fire into the agent inbox), this is the
    // first thing an orchestrator hits when it schedules before registering.
    if let Some(agent_id) = &body.agent_id {
        let pool = db
            .ensure_pool()
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let known: Option<(String,)> = sqlx::query_as("SELECT id FROM iyke_agents WHERE id = ?")
            .bind(agent_id)
            .fetch_optional(&pool)
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("timer agent: {e}")))?;
        if known.is_none() {
            return Err(err(
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown agent_id {agent_id:?} — POST /iyke/agent/register with \
                     {{\"id\":{agent_id:?},\"name\":…}} first"
                ),
            ));
        }
    }
    let now = now_ms();
    let fire_at = match (body.fire_at, body.delay_ms) {
        (Some(_), Some(_)) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "pass either fire_at or delay_ms, not both".to_string(),
            ));
        }
        (Some(f), None) => f,
        (None, Some(d)) => {
            if d < 0 {
                return Err(err(StatusCode::BAD_REQUEST, "delay_ms < 0".to_string()));
            }
            now.saturating_add(d)
        }
        (None, None) => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "must pass fire_at or delay_ms".to_string(),
            ));
        }
    };
    if fire_at - now > TIMER_MAX_DELAY_MS {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "fire_at > 1 year out".to_string(),
        ));
    }
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, body.scope).await?;
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO iyke_timers (id, scope, fire_at, agent_id, title, body, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
    )
    .bind(&id)
    .bind(&scope)
    .bind(fire_at)
    .bind(&body.agent_id)
    .bind(&body.title)
    .bind(&body.body)
    .bind(now)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timer schedule: {e}"),
        )
    })?;

    // Wake the firing loop so it re-reads "next pending" and doesn't wait
    // out a stale sleep tied to a later timer.
    sched.wake();

    Ok(Json(json!({
        "id": id,
        "scope": scope,
        "fire_at": fire_at,
        "agent_id": body.agent_id,
    })))
}

#[derive(Deserialize)]
pub struct TimerCancelBody {
    pub id: String,
}

pub async fn post_timer_cancel(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(sched): Extension<TimerScheduler>,
    Json(body): Json<TimerCancelBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let res = sqlx::query(
        "UPDATE iyke_timers SET status = 'cancelled' WHERE id = ? AND status = 'pending'",
    )
    .bind(&body.id)
    .execute(&pool)
    .await
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timer cancel: {e}"),
        )
    })?;
    sched.wake();
    Ok(Json(json!({ "cancelled": res.rows_affected() > 0 })))
}

#[derive(Deserialize)]
pub struct TimerListQuery {
    pub scope: Option<String>,
    pub status: Option<String>,
}

pub async fn get_timer_list(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<TimerListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let scope = resolve_scope(&pool, q.scope).await?;
    let rows = if let Some(s) = q.status.filter(|s| !s.is_empty()) {
        sqlx::query(
            "SELECT id, scope, fire_at, agent_id, title, body, status, created_at, fired_at
             FROM iyke_timers WHERE scope = ? AND status = ? ORDER BY fire_at ASC",
        )
        .bind(&scope)
        .bind(&s)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query(
            "SELECT id, scope, fire_at, agent_id, title, body, status, created_at, fired_at
             FROM iyke_timers WHERE scope = ? ORDER BY fire_at ASC",
        )
        .bind(&scope)
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("timer list: {e}"),
        )
    })?;
    let timers: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "scope": r.get::<String, _>("scope"),
                "fire_at": r.get::<i64, _>("fire_at"),
                "agent_id": r.get::<Option<String>, _>("agent_id"),
                "title": r.get::<String, _>("title"),
                "body": r.get::<Option<String>, _>("body"),
                "status": r.get::<String, _>("status"),
                "created_at": r.get::<i64, _>("created_at"),
                "fired_at": r.get::<Option<i64>, _>("fired_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "scope": scope, "timers": timers })))
}

/// One firing-loop iteration. Returns the fired timer's id when one
/// actually fired, otherwise None. Public for tests.
pub async fn fire_due_timer(pool: &SqlitePool, app: Option<&AppHandle>) -> Option<String> {
    let now = now_ms();
    // SELECT first, then UPDATE the row to 'fired'. We use a
    // "claim" via UPDATE..RETURNING semantics by checking rows_affected
    // on a conditional update — that way concurrent loop iterations
    // (shouldn't happen, but cheap to be safe) can't double-fire.
    let row = sqlx::query_as::<_, (String, String, Option<String>, String, Option<String>)>(
        "SELECT id, scope, agent_id, title, body
         FROM iyke_timers
         WHERE status = 'pending' AND fire_at <= ?
         ORDER BY fire_at ASC LIMIT 1",
    )
    .bind(now)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let (id, scope, agent_id, title, body) = row;

    let updated = sqlx::query(
        "UPDATE iyke_timers SET status = 'fired', fired_at = ?
         WHERE id = ? AND status = 'pending'",
    )
    .bind(now)
    .bind(&id)
    .execute(pool)
    .await
    .ok()?;
    if updated.rows_affected() == 0 {
        // Lost the race; let the next iteration try again.
        return None;
    }

    // Deliver to the agent inbox if attributed. Source of "synthetic
    // events delivered on the agent's next tool call".
    if let Some(aid) = &agent_id {
        let payload = json!({
            "timer_id": id,
            "scope": scope,
            "title": title,
            "body": body,
            "fire_at": now,
        });
        let _ = sqlx::query(
            "INSERT INTO iyke_agent_inbox (id, agent_id, kind, payload, created_at)
             VALUES (?, ?, 'timer-fired', ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(aid)
        .bind(payload.to_string())
        .bind(now)
        .execute(pool)
        .await;
    }

    // Emit Tauri event so the FE can fire OS notifications via
    // tauri-plugin-notification (mirrors acp-notify-bridge.ts).
    if let Some(handle) = app {
        let _ = handle.emit(
            "iyke://timer-fired",
            json!({
                "id": id,
                "scope": scope,
                "title": title,
                "body": body,
                "agent_id": agent_id,
                "fired_at": now,
            }),
        );
    }

    Some(id)
}

/// 24-hour TTL sweeper for the agent inbox. Called once per firing-loop
/// iteration after a timer fires; cheap enough to inline.
async fn sweep_inbox(pool: &SqlitePool) {
    let cutoff = now_ms() - TIMER_INBOX_TTL_MS;
    let _ = sqlx::query("DELETE FROM iyke_agent_inbox WHERE created_at < ?")
        .bind(cutoff)
        .execute(pool)
        .await;
}

pub fn spawn_timer_fire_loop(db: Arc<PaDb>, sched: TimerScheduler, app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let pool = match db.ensure_pool().await {
                Ok(p) => p,
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Find the next pending timer.
            let next: Option<(String, i64)> = sqlx::query_as(
                "SELECT id, fire_at FROM iyke_timers
                 WHERE status = 'pending' ORDER BY fire_at ASC LIMIT 1",
            )
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten();

            match next {
                None => {
                    // No pending timers: wait for a scheduler wake or a
                    // long fallback poll. The fallback covers the case
                    // where wake signals are lost (shouldn't happen but
                    // cheap insurance).
                    tokio::select! {
                        _ = sched.wait() => continue,
                        _ = tokio::time::sleep(Duration::from_secs(30)) => continue,
                    }
                }
                Some((_id, fire_at)) => {
                    let wait_ms = (fire_at - now_ms()).max(0) as u64;
                    if wait_ms > 0 {
                        // A new schedule/cancel can shift the next fire
                        // time — wake out of the sleep on notify.
                        tokio::select! {
                            _ = sched.wait() => continue,
                            _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
                        }
                    }
                    let _ = fire_due_timer(&pool, Some(&app_handle)).await;
                    sweep_inbox(&pool).await;
                }
            }
        }
    });
}

// ═══════════════════════════════════════════════════════════════════════
// Locks sweeper (spawned once from lib.rs)
// ═══════════════════════════════════════════════════════════════════════

pub fn spawn_lock_sweeper(db: Arc<PaDb>) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            interval.tick().await;
            let pool = match db.ensure_pool().await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let _ = sqlx::query("DELETE FROM iyke_locks WHERE expires_at < ?")
                .bind(now_ms())
                .execute(&pool)
                .await;
        }
    });
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn fresh_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // Minimal schema slice from 0016_iyke_memory.sql — only the tables
        // the timer fire path touches.
        for stmt in [
            "CREATE TABLE iyke_agents (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                model TEXT,
                metadata TEXT NOT NULL DEFAULT '{}',
                registered_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL
            )",
            "CREATE TABLE iyke_timers (
                id TEXT PRIMARY KEY,
                scope TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                agent_id TEXT,
                title TEXT NOT NULL,
                body TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at INTEGER NOT NULL,
                fired_at INTEGER
            )",
            "CREATE TABLE iyke_agent_inbox (
                id TEXT PRIMARY KEY,
                agent_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn insert_pending_timer(
        pool: &SqlitePool,
        id: &str,
        scope: &str,
        fire_at: i64,
        agent_id: Option<&str>,
        title: &str,
    ) {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO iyke_timers
             (id, scope, fire_at, agent_id, title, body, status, created_at)
             VALUES (?, ?, ?, ?, ?, NULL, 'pending', ?)",
        )
        .bind(id)
        .bind(scope)
        .bind(fire_at)
        .bind(agent_id)
        .bind(title)
        .bind(now)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn fire_due_timer_marks_fired_and_returns_id() {
        let pool = fresh_pool().await;
        let past = now_ms() - 1000;
        insert_pending_timer(&pool, "t1", "project:default", past, None, "test").await;

        let fired = fire_due_timer(&pool, None).await;
        assert_eq!(fired.as_deref(), Some("t1"));

        let row: (String, Option<i64>) =
            sqlx::query_as("SELECT status, fired_at FROM iyke_timers WHERE id = 't1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, "fired");
        assert!(row.1.is_some());
    }

    #[tokio::test]
    async fn fire_due_timer_returns_none_for_future_only() {
        let pool = fresh_pool().await;
        let future = now_ms() + 60_000;
        insert_pending_timer(&pool, "t2", "project:default", future, None, "future").await;

        let fired = fire_due_timer(&pool, None).await;
        assert!(fired.is_none());

        let status: String = sqlx::query_scalar("SELECT status FROM iyke_timers WHERE id = 't2'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn fire_due_timer_writes_agent_inbox_when_attributed() {
        let pool = fresh_pool().await;
        // Agent row required by FK on iyke_agent_inbox in the real
        // schema. The minimal test schema drops the FK constraint, but
        // we still write a registered row so the inbox payload is
        // realistic.
        sqlx::query(
            "INSERT INTO iyke_agents (id, name, model, metadata, registered_at, last_seen_at)
             VALUES ('a1', 'tester', NULL, '{}', 0, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let past = now_ms() - 100;
        insert_pending_timer(&pool, "t3", "project:default", past, Some("a1"), "ping").await;

        let fired = fire_due_timer(&pool, None).await;
        assert_eq!(fired.as_deref(), Some("t3"));

        let (count, kind): (i64, String) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(MAX(kind), '') FROM iyke_agent_inbox WHERE agent_id = 'a1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(kind, "timer-fired");
    }

    #[tokio::test]
    async fn fire_due_timer_does_not_double_fire() {
        let pool = fresh_pool().await;
        let past = now_ms() - 100;
        insert_pending_timer(&pool, "t4", "project:default", past, None, "once").await;

        let first = fire_due_timer(&pool, None).await;
        let second = fire_due_timer(&pool, None).await;
        assert_eq!(first.as_deref(), Some("t4"));
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn scratchpad_watch_returns_only_newer_content() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(PaDb::new(dir.path().join("watch.db")));
        let pool = db.ensure_pool().await.unwrap();
        sqlx::query(
            "INSERT INTO iyke_scratchpads (id, scope, name, body, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("scratch-1")
        .bind("project:test")
        .bind("handoff")
        .bind("ready")
        .bind(100_i64)
        .bind(200_i64)
        .execute(&pool)
        .await
        .unwrap();

        let Json(updated) = get_scratchpad_watch(
            Extension(db.clone()),
            Query(ScratchpadWatchQuery {
                scope: Some("project:test".into()),
                name: "handoff".into(),
                since: 199,
                wait_ms: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(updated.get("updated").and_then(Value::as_bool), Some(true));
        assert_eq!(updated.get("body").and_then(Value::as_str), Some("ready"));

        let Json(unchanged) = get_scratchpad_watch(
            Extension(db),
            Query(ScratchpadWatchQuery {
                scope: Some("project:test".into()),
                name: "handoff".into(),
                since: 200,
                wait_ms: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(unchanged, json!({ "updated": false }));
    }

    #[test]
    fn scratchpad_tombstone_is_versioned_and_cleared_by_write() {
        let scope = "project:tombstone-test";
        let name = "handoff";
        let version = next_scratchpad_version();
        scratchpad_changed(scope, name, version, true);
        let key = scratchpad_key(scope, name);
        assert_eq!(
            scratchpad_tombstones().lock().unwrap().get(&key).copied(),
            Some(version)
        );
        scratchpad_changed(scope, name, version + 1, false);
        assert!(!scratchpad_tombstones().lock().unwrap().contains_key(&key));
    }

    #[test]
    fn scope_validation_round_trip() {
        assert!(validate_scope("workspace").is_ok());
        assert!(validate_scope("project:default").is_ok());
        assert!(validate_scope("pkg:com.ikenga.iyke").is_ok());
        assert!(validate_scope("nope").is_err());
        assert!(validate_scope("project:").is_err());
    }

    /// G-SEATS §3.1: `seat:<project>/<name>` — accepted forms and every
    /// rejection the grammar names (no project, extra `/`, empty segment,
    /// uppercase, trailing slash), plus the bounds of both segments.
    #[test]
    fn seat_scope_grammar_accepts_and_rejects() {
        for ok in [
            "seat:royalti-co/lead",
            "seat:default/review",
            "seat:a/b",
            "seat:x_y-z/nightly-2",
            "seat:0/9",
        ] {
            assert!(validate_scope(ok).is_ok(), "should accept {ok:?}");
        }
        let long_project = format!("seat:{}/lead", "a".repeat(64));
        assert!(validate_scope(&long_project).is_ok());
        let long_name = format!("seat:default/{}", "a".repeat(32));
        assert!(validate_scope(&long_name).is_ok());

        for bad in [
            "seat:",
            "seat:lead",             // no project
            "seat:/lead",            // empty project
            "seat:default/",         // trailing slash / empty name
            "seat:default/lead/",    // trailing slash
            "seat:default/a/b",      // extra '/'
            "seat:a/b/c",            // extra '/'
            "seat:Default/lead",     // uppercase project
            "seat:default/Lead",     // uppercase name
            "seat:-bad/lead",        // project must start [a-z0-9]
            "seat:default/-lead",    // name must start [a-z0-9]
            "seat:default/lead-",    // name must end [a-z0-9]
            "seat:default/le_ad",    // '_' not in the seat charset
            "seat:default/le.ad",    // '.' not in the seat charset
            "seat:default/le ad",
            "seat:de.fault/lead",    // '.' not in the project charset
            "seat",
            "seats:default/lead",
        ] {
            assert!(validate_scope(bad).is_err(), "should reject {bad:?}");
        }
        let too_long_project = format!("seat:{}/lead", "a".repeat(65));
        assert!(validate_scope(&too_long_project).is_err());
        let too_long_name = format!("seat:default/{}", "a".repeat(33));
        assert!(validate_scope(&too_long_name).is_err());
    }
}
