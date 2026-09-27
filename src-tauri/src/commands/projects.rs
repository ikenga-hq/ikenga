//! Project `#[tauri::command]`s — thin wrappers.
//!
//! The store and filesystem helpers live in [`crate::server::shared::projects`]
//! (WP-19 slice 4), shared with the daemon's `/api/rpc` arms, and are
//! re-exported here so every `crate::commands::projects::…` path (the iyke
//! bridge, the engines, `claude_store`, …) is unchanged. What stays here is
//! desktop-only: the commands themselves (`project_set_active` emits
//! `projects:active-changed`) and the one-time `claudeProjectRoots`
//! migration, which reads a desktop settings key.

use std::sync::Arc;

use sqlx::SqlitePool;
use tauri::{AppHandle, Emitter, State};

use super::db::PaDb;

pub use crate::server::shared::projects::*;

// ─── One-time migration: claudeProjectRoots → projects ────────────────────
//
// Pre-Phase-0-of-projects-first-class, the /claude config browser tracked a
// flat `claudeProjectRoots: string[]` in shell-store (mirrored to
// `settings_kv["claude.projectRoots"]` as a JSON array — see
// `src/lib/shell/shell-store.ts`, `KV_CLAUDE_ROOTS`). Phase 4 promotes those
// roots to first-class `projects` rows so the layered discovery has
// somewhere durable to look up tier-3 file roots.
//
// The migration is one-shot, gated on `settings_kv["migrations.claude_roots_to_projects.v1"]`.
// It reads `settings_kv["claude.projectRoots"]` as a JSON array, slugifies
// each entry's basename, and `create_project`s any root that doesn't already
// have a matching project. Errors are logged, not propagated — the boot path
// should never fail because of this.

const CLAUDE_ROOTS_KEY: &str = "claude.projectRoots";
const ROOTS_MIGRATION_KEY: &str = "migrations.claude_roots_to_projects.v1";

/// One-time migration to populate `projects` from the FE-store `claudeProjectRoots`.
/// Idempotent — gated on `settings_kv[ROOTS_MIGRATION_KEY] = "done"`.
///
/// Best-effort: any error short-circuits the helper and is returned to the
/// caller; `bootstrap_default_project`'s call site logs and ignores it so a
/// stray bad row never blocks boot.
pub async fn claude_roots_to_projects_migration_v1(pool: &SqlitePool) -> Result<(), String> {
    // Gate.
    let already: Option<(String,)> = sqlx::query_as("SELECT value FROM settings_kv WHERE key = ?")
        .bind(ROOTS_MIGRATION_KEY)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read migration gate: {e}"))?;
    if matches!(already, Some((ref v,)) if v == "done") {
        return Ok(());
    }

    // Read roots blob. May be absent (fresh install) — that's fine, mark done.
    let roots_row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings_kv WHERE key = ?")
            .bind(CLAUDE_ROOTS_KEY)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("read claude roots: {e}"))?;
    let roots: Vec<String> = match roots_row {
        Some((blob,)) => serde_json::from_str(&blob)
            .map_err(|e| format!("parse claude.projectRoots blob: {e}"))?,
        None => Vec::new(),
    };

    // For each root, dedupe by matching root_path on existing projects.
    let existing_paths: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, root_path FROM projects")
            .fetch_all(pool)
            .await
            .map_err(|e| format!("list existing projects: {e}"))?;
    let existing_set: std::collections::HashSet<String> = existing_paths
        .iter()
        .filter_map(|(_, p)| p.clone())
        .collect();
    let used_ids: std::collections::HashSet<String> =
        existing_paths.iter().map(|(id, _)| id.clone()).collect();

    let now = now_ms();
    let mut next_pos: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM projects")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("next position: {e}"))?;

    let mut taken: std::collections::HashSet<String> = used_ids;
    for raw in roots {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if existing_set.contains(trimmed) {
            continue;
        }
        // Slug = slugified basename. Falls back to "project-N" if empty.
        let basename = std::path::Path::new(trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(trimmed);
        let mut slug = slugify(basename);
        if slug.is_empty() {
            slug = format!("project-{next_pos}");
        }
        // Append a numeric suffix if collision.
        let mut candidate = slug.clone();
        let mut n = 1;
        while taken.contains(&candidate) {
            n += 1;
            candidate = format!("{slug}-{n}");
        }
        let display = basename.to_string();
        let display = if display.chars().count() > 120 {
            display.chars().take(120).collect()
        } else {
            display
        };

        let res = sqlx::query(
            "INSERT INTO projects
                (id, display_name, root_path, icon, color, description, position, is_default, created_at)
             VALUES (?, ?, ?, NULL, NULL, NULL, ?, 0, ?)",
        )
        .bind(&candidate)
        .bind(&display)
        .bind(trimmed)
        .bind(next_pos)
        .bind(now)
        .execute(pool)
        .await;
        match res {
            Ok(_) => {
                log::info!(
                    "[claude-roots-migration] created project {candidate:?} for root {trimmed:?}"
                );
                taken.insert(candidate);
                next_pos += 1;
            }
            Err(e) => {
                // Don't propagate — one bad row mustn't block the migration.
                log::warn!("[claude-roots-migration] skip {trimmed:?}: insert failed: {e}");
            }
        }
    }

    sqlx::query(
        "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(ROOTS_MIGRATION_KEY)
    .bind("done")
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| format!("mark migration done: {e}"))?;
    Ok(())
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.chars() {
        let ch = c.to_ascii_lowercase();
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
        } else if ch == '_' || ch == '-' {
            out.push(ch);
            prev_dash = false;
        } else if ch.is_whitespace() || ch == '/' || ch == '.' {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            // Drop other punctuation.
        }
    }
    // Trim leading non-alphanumeric characters so the slug satisfies the
    // `^[a-z0-9]` rule enforced by `validate_slug`.
    let trimmed = out
        .trim_start_matches(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .trim_end_matches('-')
        .to_string();
    if trimmed.len() > 64 {
        trimmed.chars().take(64).collect()
    } else {
        trimmed
    }
}

// ── Tauri commands ───────────────────────────────────────────────────────

#[tauri::command]
pub async fn project_create(
    db: State<'_, Arc<PaDb>>,
    id: String,
    display_name: String,
    root_path: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    description: Option<String>,
) -> Result<Project, String> {
    let pool = db.ensure_pool().await?;
    create_project(
        &pool,
        CreateArgs {
            id,
            display_name,
            root_path,
            icon,
            color,
            description,
        },
    )
    .await
}

#[tauri::command]
pub async fn project_update(
    db: State<'_, Arc<PaDb>>,
    id: String,
    patch: ProjectPatch,
) -> Result<Project, String> {
    let pool = db.ensure_pool().await?;
    update_project(&pool, &id, patch).await
}

#[tauri::command]
pub async fn project_list(
    db: State<'_, Arc<PaDb>>,
    include_archived: Option<bool>,
) -> Result<Vec<Project>, String> {
    let pool = db.ensure_pool().await?;
    list_projects(&pool, include_archived.unwrap_or(false)).await
}

#[tauri::command]
pub async fn project_archive(db: State<'_, Arc<PaDb>>, id: String) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    archive_project(&pool, &id).await
}

#[tauri::command]
pub async fn project_set_active(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    id: String,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    set_active_project_id(&pool, &id).await?;
    // Broadcast: today there is ONE app-wide active project, so every window
    // must invalidate its query cache (research 03 — broadcast is correct for
    // the single-project model). TODO(multi-window): per-window project binding
    // (Flavor B) will need `emit_to` the bound window instead.
    let _ = app.emit("projects:active-changed", serde_json::json!({ "id": id }));
    Ok(())
}

#[tauri::command]
pub fn project_scaffold_claude(root_path: String) -> Result<(), String> {
    scaffold_claude(&root_path, FsReach::Follow)
}

#[tauri::command]
pub fn project_skills_list(
    root_path: Option<String>,
    include_user_global: bool,
) -> Result<Vec<ProjectSkill>, String> {
    // $HOME is unset on Windows; route through the platform resolver.
    let home = crate::platform::home_dir();
    skills_list(
        root_path,
        include_user_global,
        home.as_deref(),
        FsReach::Follow,
    )
}

#[tauri::command]
pub fn project_inventory(root_path: Option<String>) -> Result<ProjectInventory, String> {
    inventory(root_path, FsReach::Follow)
}

#[tauri::command]
pub async fn project_get_active(db: State<'_, Arc<PaDb>>) -> Result<Project, String> {
    let pool = db.ensure_pool().await?;
    get_active_project(&pool).await
}

#[tauri::command]
pub fn project_artifacts_walk(root_path: Option<String>) -> Result<Vec<ArtifactRow>, String> {
    artifacts_walk(root_path)
}
