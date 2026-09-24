//! Secrets bridge endpoints — Phase 7 of projects-first-class.
//!
//! Wraps the scoped vault primitives so the iyke CLI / `mcp-iyke` MCP
//! can manage vault entries with scope semantics. Scope defaults to the
//! active project when not specified; pass `workspace` or `pkg:<id>` to
//! target a different partition.
//!
//! These endpoints surface raw secret values — they're gated by the
//! same bearer-token middleware as the rest of `/iyke/*`. Per-call
//! user-consent on top of that lands with Phase 9 trust gating.

use std::sync::Arc;

use axum::{extract::Query, http::StatusCode, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::commands::db::PaDb;
use crate::commands::projects::get_active_project_id;
use crate::commands::secrets::{
    read_secret_scoped, scoped_delete_locked_pub, scoped_list_locked_pub, scoped_set_locked_pub,
    Scope, SecretsLock,
};

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (status, msg.into())
}

fn map_err(e: String) -> (StatusCode, String) {
    let lower = e.to_lowercase();
    if lower.contains("not found") {
        err(StatusCode::NOT_FOUND, e)
    } else if lower.contains("invalid") {
        err(StatusCode::BAD_REQUEST, e)
    } else {
        err(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

/// Parse the wire-format scope string:
///   - `"workspace"` → `Scope::Workspace`
///   - `"project:<id>"` → `Scope::Project { id }`
///   - `"pkg:<id>"` → `Scope::Pkg { id }`
///   - missing / empty → active project at call time
async fn resolve_scope(
    pool: &sqlx::SqlitePool,
    raw: Option<&str>,
) -> Result<Scope, (StatusCode, String)> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => {
            let pid = get_active_project_id(pool).await.map_err(map_err)?;
            Ok(Scope::project(pid))
        }
        Some("workspace") => Ok(Scope::Workspace),
        Some(s) if s.starts_with("project:") => {
            let id = &s["project:".len()..];
            if id.is_empty() {
                return Err(err(StatusCode::BAD_REQUEST, "project scope needs an id"));
            }
            Ok(Scope::project(id))
        }
        Some(s) if s.starts_with("pkg:") => {
            let id = &s["pkg:".len()..];
            if id.is_empty() {
                return Err(err(StatusCode::BAD_REQUEST, "pkg scope needs an id"));
            }
            Ok(Scope::pkg(id))
        }
        Some(other) => Err(err(
            StatusCode::BAD_REQUEST,
            format!("unknown scope `{other}` (expected workspace|project:<id>|pkg:<id>)"),
        )),
    }
}

fn get_lock(app: &AppHandle) -> SecretsLock {
    let s = app.state::<SecretsLock>();
    SecretsLock::from_slot(s.0.clone())
}

#[derive(Deserialize)]
pub struct GetQuery {
    #[serde(default)]
    pub scope: Option<String>,
    pub key: String,
}

pub async fn get_secret(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Query(q): Query<GetQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let scope = resolve_scope(&pool, q.scope.as_deref()).await?;
    let lock = get_lock(&app);
    let value = tokio::task::spawn_blocking({
        let app = app.clone();
        let scope = scope.clone();
        let key = q.key.clone();
        move || read_secret_scoped(&app, &lock, &scope, &key)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(map_err)?;
    Ok(Json(json!({
        "key": q.key,
        "scope": scope,
        "value": value,
    })))
}

#[derive(Deserialize)]
pub struct SetBody {
    #[serde(default)]
    pub scope: Option<String>,
    pub key: String,
    pub value: String,
}

pub async fn post_secret_set(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<SetBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.key.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "key is required"));
    }
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let scope = resolve_scope(&pool, body.scope.as_deref()).await?;
    let lock = get_lock(&app);
    tokio::task::spawn_blocking({
        let app = app.clone();
        let scope = scope.clone();
        let key = body.key.clone();
        let value = body.value.clone();
        move || scoped_set_locked_pub(&app, &lock, &scope, &key, &value)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(map_err)?;
    Ok(Json(json!({ "ok": true, "key": body.key, "scope": scope })))
}

#[derive(Deserialize)]
pub struct DeleteBody {
    #[serde(default)]
    pub scope: Option<String>,
    pub key: String,
}

pub async fn post_secret_delete(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<DeleteBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.key.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "key is required"));
    }
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let scope = resolve_scope(&pool, body.scope.as_deref()).await?;
    let lock = get_lock(&app);
    tokio::task::spawn_blocking({
        let app = app.clone();
        let scope = scope.clone();
        let key = body.key.clone();
        move || scoped_delete_locked_pub(&app, &lock, &scope, &key)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(map_err)?;
    Ok(Json(json!({ "ok": true, "key": body.key, "scope": scope })))
}

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub scope: Option<String>,
}

pub async fn get_secret_list(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let scope = resolve_scope(&pool, q.scope.as_deref()).await?;
    let lock = get_lock(&app);
    let keys = tokio::task::spawn_blocking({
        let app = app.clone();
        let scope = scope.clone();
        move || scoped_list_locked_pub(&app, &lock, &scope)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")))?
    .map_err(map_err)?;
    Ok(Json(json!({ "scope": scope, "keys": keys })))
}
