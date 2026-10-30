//! Project bridge endpoints — mirrors `commands::projects` over HTTP so
//! the `iyke` CLI and `mcp-iyke` MCP server can manage projects without
//! going through a Tauri command.
//!
//! Logic is delegated to the shared helpers in `commands::projects`
//! (`list_projects`, `create_project`, ...) so wire validation lives
//! in one place. The handlers here only deal with HTTP shape.

use std::sync::Arc;

use axum::{extract::Query, http::StatusCode, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::commands::db::PaDb;
use crate::commands::projects::{
    archive_project_announcing, create_project_announcing, get_active_project_id, get_project,
    list_projects, set_active_project_id, update_project_announcing, CreateArgs, Project,
    ProjectPatch,
};

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (status, msg.into())
}

fn map_err(e: String) -> (StatusCode, String) {
    // Heuristic mapping — keep it cheap.
    let lower = e.to_lowercase();
    if lower.contains("not found") {
        err(StatusCode::NOT_FOUND, e)
    } else if lower.contains("invalid")
        || lower.contains("cannot archive")
        || lower.contains("already exists")
    {
        err(StatusCode::BAD_REQUEST, e)
    } else {
        err(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}

// ── GET /iyke/project/list ───────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_archived: Option<bool>,
}

pub async fn get_project_list(
    Extension(db): Extension<Arc<PaDb>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let projects = list_projects(&pool, q.include_archived.unwrap_or(false))
        .await
        .map_err(map_err)?;
    Ok(Json(json!({ "projects": projects })))
}

// ── POST /iyke/project/create ───────────────────────────────────────────

pub async fn post_project_create(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<CreateArgs>,
) -> Result<Json<Project>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    create_project_announcing(&pool, body, &app)
        .await
        .map(Json)
        .map_err(map_err)
}

// ── POST /iyke/project/update ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateBody {
    pub id: String,
    #[serde(default)]
    pub patch: ProjectPatch,
}

pub async fn post_project_update(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<UpdateBody>,
) -> Result<Json<Project>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    update_project_announcing(&pool, &body.id, body.patch, &app)
        .await
        .map(Json)
        .map_err(map_err)
}

// ── POST /iyke/project/archive ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct IdBody {
    pub id: String,
}

pub async fn post_project_archive(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    archive_project_announcing(&pool, &body.id, &app)
        .await
        .map_err(map_err)?;
    Ok(Json(json!({ "ok": true, "id": body.id })))
}

// ── POST /iyke/project/set-active ───────────────────────────────────────

pub async fn post_project_set_active(
    Extension(db): Extension<Arc<PaDb>>,
    Extension(app): Extension<AppHandle>,
    Json(body): Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    set_active_project_id(&pool, &body.id)
        .await
        .map_err(map_err)?;
    // Broadcast: single app-wide active project, so every window invalidates
    // its cache (research 03). TODO(multi-window): per-window project binding
    // (Flavor B) will need `emit_to` the bound window.
    let _ = app.emit("projects:active-changed", json!({ "id": body.id }));
    Ok(Json(json!({ "ok": true, "id": body.id })))
}

// ── GET /iyke/project/active ────────────────────────────────────────────

pub async fn get_project_active(
    Extension(db): Extension<Arc<PaDb>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pool = db.ensure_pool().await.map_err(map_err)?;
    let id = get_active_project_id(&pool).await.map_err(map_err)?;
    let p = get_project(&pool, &id).await.map_err(map_err)?;
    let p = match p {
        Some(p) if p.archived_at.is_none() => p,
        _ => get_project(&pool, "default")
            .await
            .map_err(map_err)?
            .ok_or_else(|| err(StatusCode::INTERNAL_SERVER_ERROR, "Default project missing"))?,
    };
    Ok(Json(json!({ "project": p })))
}
