//! Project CRUD commands. Backed by the `projects` + `project_settings`
//! tables (migration 0015). The `default` project row is bootstrapped by
//! `db::bootstrap_default_project` so the active-project id always
//! resolves to something even on a fresh boot.
//!
//! The active project id lives in `settings_kv` at key
//! `shell.activeProjectId`. `project_set_active` writes that row and
//! emits a Tauri event `projects:active-changed` so the frontend can
//! invalidate `project-scoped` TanStack queries.
//!
//! Slug validation: `^[a-z0-9][a-z0-9_-]{0,63}$`. The reserved id
//! `default` cannot be archived (refused at command level).
//!
//! Shared helpers (`list_projects`, `create_project`, ...) take an
//! `&sqlx::SqlitePool` so the iyke bridge can reuse the same logic
//! without round-tripping through Tauri.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter, State};

use super::db::PaDb;

pub const ACTIVE_PROJECT_KEY: &str = "shell.activeProjectId";
pub const DEFAULT_PROJECT_ID: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub display_name: String,
    pub root_path: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub description: Option<String>,
    pub position: i64,
    pub is_default: bool,
    pub created_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectPatch {
    pub display_name: Option<String>,
    pub root_path: Option<Option<String>>,
    pub icon: Option<Option<String>>,
    pub color: Option<Option<String>>,
    pub description: Option<Option<String>>,
    pub position: Option<i64>,
}

impl Default for ProjectPatch {
    fn default() -> Self {
        Self {
            display_name: None,
            root_path: None,
            icon: None,
            color: None,
            description: None,
            position: None,
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn validate_slug(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 {
        return Err(format!("invalid project id length: {} (1..=64)", id.len()));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!(
            "invalid project id {id:?}: must start with [a-z0-9]"
        ));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(format!(
                "invalid project id {id:?}: only [a-z0-9_-] allowed after first char"
            ));
        }
    }
    Ok(())
}

fn row_to_project(row: &sqlx::sqlite::SqliteRow) -> Project {
    Project {
        id: row.get("id"),
        display_name: row.get("display_name"),
        root_path: row.get("root_path"),
        icon: row.get("icon"),
        color: row.get("color"),
        description: row.get("description"),
        position: row.get("position"),
        is_default: row.get::<i64, _>("is_default") != 0,
        created_at: row.get("created_at"),
        archived_at: row.get("archived_at"),
    }
}

// ── shared helpers (used by Tauri commands + the iyke bridge) ────────────

pub async fn list_projects(
    pool: &SqlitePool,
    include_archived: bool,
) -> Result<Vec<Project>, String> {
    let sql = if include_archived {
        "SELECT id, display_name, root_path, icon, color, description, position, is_default, created_at, archived_at
         FROM projects
         ORDER BY position ASC, created_at ASC"
    } else {
        "SELECT id, display_name, root_path, icon, color, description, position, is_default, created_at, archived_at
         FROM projects
         WHERE archived_at IS NULL
         ORDER BY position ASC, created_at ASC"
    };
    let rows = sqlx::query(sql)
        .fetch_all(pool)
        .await
        .map_err(|e| format!("list projects: {e}"))?;
    Ok(rows.iter().map(row_to_project).collect())
}

pub async fn get_project(pool: &SqlitePool, id: &str) -> Result<Option<Project>, String> {
    let row = sqlx::query(
        "SELECT id, display_name, root_path, icon, color, description, position, is_default, created_at, archived_at
         FROM projects WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("get project {id}: {e}"))?;
    Ok(row.as_ref().map(row_to_project))
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateArgs {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub async fn create_project(pool: &SqlitePool, args: CreateArgs) -> Result<Project, String> {
    validate_slug(&args.id)?;
    let display = args.display_name.trim();
    if display.is_empty() || display.chars().count() > 120 {
        return Err(format!(
            "invalid display_name length: {} (1..=120)",
            display.chars().count()
        ));
    }

    let next_position: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM projects")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("next position: {e}"))?;

    let now = now_ms();
    sqlx::query(
        "INSERT INTO projects
            (id, display_name, root_path, icon, color, description, position, is_default, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(&args.id)
    .bind(display)
    .bind(&args.root_path)
    .bind(&args.icon)
    .bind(&args.color)
    .bind(&args.description)
    .bind(next_position)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE") {
            format!("project id already exists: {}", args.id)
        } else {
            format!("create project: {e}")
        }
    })?;

    get_project(pool, &args.id)
        .await?
        .ok_or_else(|| "create_project: row vanished after insert".to_string())
}

pub async fn update_project(
    pool: &SqlitePool,
    id: &str,
    patch: ProjectPatch,
) -> Result<Project, String> {
    let mut sets: Vec<String> = Vec::new();
    let mut display_buf: Option<String> = None;
    let mut root_buf: Option<Option<String>> = None;
    let mut icon_buf: Option<Option<String>> = None;
    let mut color_buf: Option<Option<String>> = None;
    let mut desc_buf: Option<Option<String>> = None;
    let mut pos_buf: Option<i64> = None;

    if let Some(name) = patch.display_name {
        let trimmed = name.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 120 {
            return Err(format!(
                "invalid display_name length: {} (1..=120)",
                trimmed.chars().count()
            ));
        }
        display_buf = Some(trimmed.to_string());
        sets.push("display_name = ?".into());
    }
    if let Some(v) = patch.root_path {
        root_buf = Some(v);
        sets.push("root_path = ?".into());
    }
    if let Some(v) = patch.icon {
        icon_buf = Some(v);
        sets.push("icon = ?".into());
    }
    if let Some(v) = patch.color {
        color_buf = Some(v);
        sets.push("color = ?".into());
    }
    if let Some(v) = patch.description {
        desc_buf = Some(v);
        sets.push("description = ?".into());
    }
    if let Some(v) = patch.position {
        pos_buf = Some(v);
        sets.push("position = ?".into());
    }

    if sets.is_empty() {
        // no-op patch — return current row.
        return get_project(pool, id)
            .await?
            .ok_or_else(|| format!("project not found: {id}"));
    }

    let sql = format!("UPDATE projects SET {} WHERE id = ?", sets.join(", "));
    let mut q = sqlx::query(&sql);
    if let Some(v) = display_buf.as_ref() {
        q = q.bind(v);
    }
    if let Some(v) = root_buf.as_ref() {
        q = q.bind(v);
    }
    if let Some(v) = icon_buf.as_ref() {
        q = q.bind(v);
    }
    if let Some(v) = color_buf.as_ref() {
        q = q.bind(v);
    }
    if let Some(v) = desc_buf.as_ref() {
        q = q.bind(v);
    }
    if let Some(v) = pos_buf {
        q = q.bind(v);
    }
    q = q.bind(id);

    let res = q
        .execute(pool)
        .await
        .map_err(|e| format!("update project {id}: {e}"))?;
    if res.rows_affected() == 0 {
        return Err(format!("project not found: {id}"));
    }
    get_project(pool, id)
        .await?
        .ok_or_else(|| format!("project not found after update: {id}"))
}

pub async fn archive_project(pool: &SqlitePool, id: &str) -> Result<(), String> {
    if id == DEFAULT_PROJECT_ID {
        return Err("cannot archive the Default project".into());
    }
    let res = sqlx::query("UPDATE projects SET archived_at = ? WHERE id = ?")
        .bind(now_ms())
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| format!("archive project {id}: {e}"))?;
    if res.rows_affected() == 0 {
        return Err(format!("project not found: {id}"));
    }
    Ok(())
}

pub async fn get_active_project_id(pool: &SqlitePool) -> Result<String, String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings_kv WHERE key = ?")
        .bind(ACTIVE_PROJECT_KEY)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read active project id: {e}"))?;
    Ok(row
        .map(|(v,)| v)
        .unwrap_or_else(|| DEFAULT_PROJECT_ID.to_string()))
}

/// Phase 5: resolve `(IKENGA_PROJECT_ID, IKENGA_PROJECT_ROOT)` for an MCP
/// child spawn. Workspace-scoped pkgs pass `None` and pick up the current
/// active project; project-scoped pkgs pass their own project id. Either
/// component may be `None` (missing project row, no root_path) — callers
/// skip the env var rather than injecting an empty string.
pub async fn resolve_project_env_ctx(
    pool: &SqlitePool,
    pkg_project_id: Option<&str>,
) -> (Option<String>, Option<String>) {
    let effective_id = match pkg_project_id {
        Some(id) => id.to_string(),
        None => match get_active_project_id(pool).await {
            Ok(id) => id,
            Err(_) => return (None, None),
        },
    };
    let root = match get_project(pool, &effective_id).await {
        Ok(Some(p)) => p.root_path,
        _ => None,
    };
    (Some(effective_id), root)
}

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

pub async fn set_active_project_id(pool: &SqlitePool, id: &str) -> Result<(), String> {
    // Validate the project exists and isn't archived.
    let p = get_project(pool, id)
        .await?
        .ok_or_else(|| format!("project not found: {id}"))?;
    if p.archived_at.is_some() {
        return Err(format!("project is archived: {id}"));
    }
    sqlx::query(
        "INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(ACTIVE_PROJECT_KEY)
    .bind(id)
    .bind(now_ms())
    .execute(pool)
    .await
    .map_err(|e| format!("set active project id: {e}"))?;
    Ok(())
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

// ── project_inventory: count skills/commands/mcp servers under a root ───
//
// Used by Settings → Projects (badge "12 skills, 4 commands, 2 MCP servers")
// and by the artifact creation wizard (skill checklist source — D10 in the
// plan). Pure filesystem read, no DB access. Safe to call without a root_path
// — returns zeroed counts.
//
// What counts:
// - skills:   `<root>/.claude/skills/*.md` plus directories containing a
//             `SKILL.md` (Anthropic skill folder format).
// - commands: `<root>/.claude/commands/*.md` (top-level only; nested files
//             aren't first-class slash-commands in claude code).
// - mcp:      `mcpServers` keys in `<root>/.mcp.json` (claude code's
//             project-MCP convention). Falls back to 0 if the file is
//             missing or unparseable.
//
// Empty `root_path` → all zeros. Missing `.claude/` → zeros for skills/cmds
// but `.mcp.json` is still consulted (it lives at project root, not under
// `.claude/`).

#[derive(Debug, Clone, Serialize)]
pub struct ProjectInventory {
    pub root_path: Option<String>,
    pub has_claude_dir: bool,
    pub skills: usize,
    pub commands: usize,
    pub mcp: usize,
}

fn count_skills(claude_dir: &std::path::Path) -> usize {
    let skills_dir = claude_dir.join("skills");
    let read = match std::fs::read_dir(&skills_dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut n = 0usize;
    for entry in read.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_file() {
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                n += 1;
            }
        } else if file_type.is_dir() {
            // Anthropic skill folder: contains SKILL.md
            if path.join("SKILL.md").is_file() {
                n += 1;
            }
        }
    }
    n
}

fn count_commands(claude_dir: &std::path::Path) -> usize {
    let cmds_dir = claude_dir.join("commands");
    let read = match std::fs::read_dir(&cmds_dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    read.flatten()
        .filter(|e| {
            e.file_type().map(|t| t.is_file()).unwrap_or(false)
                && e.path().extension().and_then(|s| s.to_str()) == Some("md")
        })
        .count()
}

fn count_mcp_servers(root: &std::path::Path) -> usize {
    let mcp_file = root.join(".mcp.json");
    let raw = match std::fs::read_to_string(&mcp_file) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let v: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return 0,
    };
    v.get("mcpServers")
        .and_then(|s| s.as_object())
        .map(|o| o.len())
        .unwrap_or(0)
}

// ── project_scaffold_claude: mkdir <root>/.claude/{skills,commands} + stub
//
// Used by Settings → Projects "Initialise new" (A3). Creates the directories
// claude code expects to find when it walks a project, plus a minimal
// CLAUDE.md stub. Idempotent — leaves existing files alone, only adds what's
// missing. The user can run `claude init` later (or hand-edit) to flesh it
// out; this gets them past the "no project context" gate.

fn write_if_missing(path: &std::path::Path, contents: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
}

#[tauri::command]
pub fn project_scaffold_claude(root_path: String) -> Result<(), String> {
    let root = std::path::Path::new(&root_path);
    if !root.is_dir() {
        return Err(format!("root_path is not a directory: {root_path}"));
    }
    let claude_dir = root.join(".claude");
    std::fs::create_dir_all(claude_dir.join("skills"))
        .map_err(|e| format!("mkdir .claude/skills: {e}"))?;
    std::fs::create_dir_all(claude_dir.join("commands"))
        .map_err(|e| format!("mkdir .claude/commands: {e}"))?;

    let claude_md = root.join("CLAUDE.md");
    let project_name = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Project");
    write_if_missing(
        &claude_md,
        &format!(
            "# CLAUDE.md\n\nProject-level guidance for Claude Code in `{project_name}`.\n\n## What this is\n\nDescribe the project here.\n\n## Common commands\n\nList build / test / lint commands.\n\n## Conventions\n\n- (add project-specific rules here)\n"
        ),
    )?;
    Ok(())
}

// ── project_skills_list: enumerate skills (project-scoped + user-global) ─
//
// Used by the artifact creation wizard's step-3 skill checklist (D10 in
// the plan). Walks `<root>/.claude/skills/` and (if `include_user_global`)
// `~/.claude/skills/`, parsing the `name` and `description` frontmatter
// fields from each skill's `SKILL.md` (skill-folder format) or the file
// itself (single-file format).
//
// The `source` tag distinguishes "project" from "user" so the UI can
// disclose which scope a skill comes from — important because a user can
// install the same skill at both scopes and the project version wins.
//
// Frontmatter parser is intentionally tiny: looks for a leading `---`
// block, scans `key: value` lines, stops at the closing `---`. No YAML
// dependency; skill frontmatter in practice is flat scalars.

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSkill {
    pub slug: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// "project" — under `<root>/.claude/skills/`.
    /// "user"    — under `~/.claude/skills/`.
    pub source: String,
}

fn parse_frontmatter_min(raw: &str) -> (Option<String>, Option<String>) {
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return (name, desc);
    }
    // Skip the first --- line.
    let after_open = match trimmed.find('\n') {
        Some(i) => &trimmed[i + 1..],
        None => return (name, desc),
    };
    for line in after_open.lines() {
        let line_trimmed = line.trim_end();
        if line_trimmed.starts_with("---") {
            break;
        }
        if let Some((k, v)) = line_trimmed.split_once(':') {
            let key = k.trim();
            let mut val = v.trim().to_string();
            // Strip surrounding quotes if any.
            if (val.starts_with('"') && val.ends_with('"') && val.len() >= 2)
                || (val.starts_with('\'') && val.ends_with('\'') && val.len() >= 2)
            {
                val = val[1..val.len() - 1].to_string();
            }
            match key {
                "name" => {
                    if name.is_none() && !val.is_empty() {
                        name = Some(val);
                    }
                }
                "description" => {
                    if desc.is_none() && !val.is_empty() {
                        desc = Some(val);
                    }
                }
                _ => {}
            }
        }
    }
    (name, desc)
}

fn list_skills_in_dir(skills_dir: &std::path::Path, source: &str) -> Vec<ProjectSkill> {
    let mut out: Vec<ProjectSkill> = Vec::new();
    let read = match std::fs::read_dir(skills_dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (slug, body_path): (String, Option<std::path::PathBuf>) = if file_type.is_file() {
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if stem.is_empty() {
                continue;
            }
            (stem, Some(path.clone()))
        } else if file_type.is_dir() {
            let skill_md = path.join("SKILL.md");
            if !skill_md.is_file() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if dir_name.is_empty() {
                continue;
            }
            (dir_name, Some(skill_md))
        } else {
            continue;
        };
        let (name, description) = match body_path.as_ref() {
            Some(p) => match std::fs::read_to_string(p) {
                Ok(raw) => parse_frontmatter_min(&raw),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        out.push(ProjectSkill {
            slug,
            name,
            description,
            source: source.to_string(),
        });
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

#[tauri::command]
pub fn project_skills_list(
    root_path: Option<String>,
    include_user_global: bool,
) -> Result<Vec<ProjectSkill>, String> {
    let mut out: Vec<ProjectSkill> = Vec::new();
    let mut seen_slugs: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(root_str) = root_path.as_ref() {
        let root = std::path::Path::new(root_str);
        let skills_dir = root.join(".claude").join("skills");
        for s in list_skills_in_dir(&skills_dir, "project") {
            seen_slugs.insert(s.slug.clone());
            out.push(s);
        }
    }
    if include_user_global {
        // $HOME is unset on Windows; route through the platform resolver.
        if let Some(home) = crate::platform::home_dir() {
            let global_skills = home.join(".claude").join("skills");
            for s in list_skills_in_dir(&global_skills, "user") {
                if seen_slugs.contains(&s.slug) {
                    // Project scope already provides this slug — skip the
                    // user-global duplicate so the UI doesn't show two
                    // checkboxes that mean the same thing.
                    continue;
                }
                out.push(s);
            }
        }
    }
    Ok(out)
}

#[tauri::command]
pub fn project_inventory(root_path: Option<String>) -> Result<ProjectInventory, String> {
    let Some(root_str) = root_path.as_ref() else {
        return Ok(ProjectInventory {
            root_path: None,
            has_claude_dir: false,
            skills: 0,
            commands: 0,
            mcp: 0,
        });
    };
    let root = std::path::Path::new(root_str);
    let claude_dir = root.join(".claude");
    let has_claude_dir = claude_dir.is_dir();
    let (skills, commands) = if has_claude_dir {
        (count_skills(&claude_dir), count_commands(&claude_dir))
    } else {
        (0, 0)
    };
    let mcp = if root.is_dir() {
        count_mcp_servers(root)
    } else {
        0
    };
    Ok(ProjectInventory {
        root_path: Some(root_str.clone()),
        has_claude_dir,
        skills,
        commands,
        mcp,
    })
}

#[tauri::command]
pub async fn project_get_active(db: State<'_, Arc<PaDb>>) -> Result<Project, String> {
    let pool = db.ensure_pool().await?;
    let id = get_active_project_id(&pool).await?;
    // If the active id points at an archived/missing row, fall back to default.
    if let Some(p) = get_project(&pool, &id).await? {
        if p.archived_at.is_none() {
            return Ok(p);
        }
    }
    get_project(&pool, DEFAULT_PROJECT_ID)
        .await?
        .ok_or_else(|| "Default project missing — db bootstrap failed".to_string())
}

// ── project_artifacts_walk: enumerate HTML artifacts under a project root ─
//
// Used by the artifact-grid sidebar / Studio home / drafts heuristic. Walks
// `<root>` recursively, opens each `.html`, regex-extracts the
// `<script id="ikenga-manifest">…</script>` body, and surfaces the parsed
// preview as an `ArtifactRow`. Files without a manifest are still returned
// (kind/name unset) so the user sees what's there — the consumer decides
// whether to filter them out.
//
// Bounded to keep the call cheap on big repos:
//   - skips common churn dirs (.git, node_modules, target, dist, build,
//     .cache, .next, .tanstack)
//   - skips dotfile-dirs by default (.claude/ is configuration, not
//     artifacts; .well-known etc. won't have .html artifacts either)
//   - caps walk at MAX_WALK_ENTRIES files visited
//   - caps each parsed file at MAX_PARSE_BYTES (don't slurp giant HTML
//     just to fail to find a manifest)
//
// Pure filesystem read, no DB access. Safe to call without a root_path —
// returns an empty list.

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactRow {
    pub path: String,
    /// Manifest `name` field, or basename-without-extension when the
    /// manifest is missing/unparseable.
    pub name: String,
    /// Archetype slug from `manifest.notes.kind`. None when the file has
    /// no manifest, the manifest has no notes, or the kind is missing.
    pub kind: Option<String>,
    /// Manifest `version` field. Used for the drafts heuristic
    /// (no version or version starting with `0.` → draft).
    pub version: Option<String>,
    pub starred: bool,
    /// Whether the file actually had an `ikenga-manifest` script tag
    /// parseable into JSON. Consumers can hide non-manifest .html if they
    /// want a stricter view.
    pub has_manifest: bool,
    /// File mtime in ms since epoch. Used for recency sort.
    pub modified_at: i64,
    pub size_bytes: u64,
}

const MAX_WALK_ENTRIES: usize = 5_000;
const MAX_PARSE_BYTES: u64 = 2 * 1024 * 1024;
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".cache",
    ".next",
    ".tanstack",
    ".turbo",
    ".svelte-kit",
    ".vercel",
    ".direnv",
    ".venv",
    "venv",
    "__pycache__",
];

#[tauri::command]
pub fn project_artifacts_walk(root_path: Option<String>) -> Result<Vec<ArtifactRow>, String> {
    let Some(root_str) = root_path.as_ref() else {
        return Ok(Vec::new());
    };
    let root = std::path::Path::new(root_str);
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut out: Vec<ArtifactRow> = Vec::new();
    let mut visited: usize = 0;
    walk_dir(root, &mut out, &mut visited);
    Ok(out)
}

fn walk_dir(dir: &std::path::Path, out: &mut Vec<ArtifactRow>, visited: &mut usize) {
    if *visited >= MAX_WALK_ENTRIES {
        return;
    }
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for entry in read.flatten() {
        if *visited >= MAX_WALK_ENTRIES {
            return;
        }
        *visited += 1;
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            if SKIP_DIR_NAMES.contains(&name) {
                continue;
            }
            // Skip dotfile-dirs except where the user might keep artifacts
            // (currently none — .claude/ holds skills/commands, not html).
            if name.starts_with('.') {
                continue;
            }
            walk_dir(&path, out, visited);
        } else if file_type.is_file() && has_html_extension(name) {
            if let Some(row) = parse_artifact_row(&path) {
                out.push(row);
            }
        }
    }
}

fn has_html_extension(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

fn parse_artifact_row(path: &std::path::Path) -> Option<ArtifactRow> {
    let meta = std::fs::metadata(path).ok()?;
    let size_bytes = meta.len();
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let path_str = path.to_str()?.to_string();
    let basename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("(untitled)")
        .to_string();

    // Read only up to MAX_PARSE_BYTES — anything bigger almost certainly
    // isn't a hand-authored artifact and we don't want to slurp it.
    let bytes_to_read = std::cmp::min(size_bytes, MAX_PARSE_BYTES) as usize;
    let raw = match read_bounded(path, bytes_to_read) {
        Ok(s) => s,
        Err(_) => {
            return Some(ArtifactRow {
                path: path_str,
                name: basename,
                kind: None,
                version: None,
                starred: false,
                has_manifest: false,
                modified_at,
                size_bytes,
            });
        }
    };

    let manifest = extract_manifest_json(&raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok());
    let (name, kind, version, starred, has_manifest) = match manifest {
        Some(v) => (
            v.get("name")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| basename.clone()),
            v.get("notes")
                .and_then(|n| n.get("kind"))
                .and_then(|k| k.as_str())
                .map(str::to_string),
            v.get("version")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            v.get("notes")
                .and_then(|n| n.get("starred"))
                .and_then(|s| s.as_bool())
                .unwrap_or(false),
            true,
        ),
        None => (basename, None, None, false, false),
    };

    Some(ArtifactRow {
        path: path_str,
        name,
        kind,
        version,
        starred,
        has_manifest,
        modified_at,
        size_bytes,
    })
}

fn read_bounded(path: &std::path::Path, max_bytes: usize) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::with_capacity(max_bytes.min(64 * 1024));
    f.by_ref().take(max_bytes as u64).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Mirrors `src/lib/artifact/manifest-from-file.ts::extractManifestJson`.
/// Returns the trimmed body of the first `<script id="ikenga-manifest">`
/// tag, or None when missing/empty.
fn extract_manifest_json(html: &str) -> Option<String> {
    use regex::Regex;
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(?is)<script\s+[^>]*?\bid\s*=\s*["']ikenga-manifest["'][^>]*?>(.*?)</script>"#,
        )
        .expect("manifest regex compiles")
    });
    let caps = re.captures(html)?;
    let body = caps.get(1)?.as_str().trim();
    if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn slug_validation_accepts_valid() {
        for ok in &["default", "music-2026", "a", "x_y_z", "abc123"] {
            validate_slug(ok).expect(ok);
        }
    }

    #[test]
    fn slug_validation_rejects_invalid() {
        for bad in &["", "-bad", "_bad", "Bad", "with space", "with.dot", "with!"] {
            assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
        }
        let long = "a".repeat(65);
        assert!(validate_slug(&long).is_err());
    }

    async fn fresh_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // Minimal schema needed by these tests.
        sqlx::query(
            "CREATE TABLE settings_kv (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(include_str!("../../migrations/0015_projects.sql"))
            .execute(&pool)
            .await
            .ok(); // sqlx::query runs one stmt; the file has many. Apply each.
                   // The file uses ALTER TABLE on tables that don't exist in this minimal
                   // schema — apply only the CREATE statements.
        for stmt in [
            "CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                root_path TEXT,
                icon TEXT,
                color TEXT,
                description TEXT,
                position INTEGER NOT NULL DEFAULT 0,
                is_default INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                archived_at INTEGER
            )",
            "CREATE UNIQUE INDEX IF NOT EXISTS projects_default ON projects (is_default) WHERE is_default = 1",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        // Seed default.
        sqlx::query(
            "INSERT INTO projects (id, display_name, color, position, is_default, created_at)
             VALUES ('default', 'Default', '#7c7c7c', 0, 1, 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn create_list_archive_round_trip() {
        let pool = fresh_pool().await;

        let created = create_project(
            &pool,
            CreateArgs {
                id: "music-2026".into(),
                display_name: "Music 2026".into(),
                root_path: Some("/tmp/music".into()),
                icon: None,
                color: Some("#4f8cff".into()),
                description: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(created.id, "music-2026");
        assert_eq!(created.display_name, "Music 2026");
        assert!(!created.is_default);

        let listed = list_projects(&pool, false).await.unwrap();
        assert_eq!(listed.len(), 2, "default + music-2026");

        archive_project(&pool, "music-2026").await.unwrap();
        let listed_active = list_projects(&pool, false).await.unwrap();
        assert_eq!(listed_active.len(), 1);
        let listed_all = list_projects(&pool, true).await.unwrap();
        assert_eq!(listed_all.len(), 2);
    }

    #[tokio::test]
    async fn cannot_archive_default() {
        let pool = fresh_pool().await;
        let err = archive_project(&pool, "default").await.unwrap_err();
        assert!(err.contains("Default"));
    }

    #[tokio::test]
    async fn set_active_round_trip() {
        let pool = fresh_pool().await;
        create_project(
            &pool,
            CreateArgs {
                id: "music".into(),
                display_name: "Music".into(),
                root_path: None,
                icon: None,
                color: None,
                description: None,
            },
        )
        .await
        .unwrap();
        // Initially the default is the active id.
        assert_eq!(get_active_project_id(&pool).await.unwrap(), "default");
        set_active_project_id(&pool, "music").await.unwrap();
        assert_eq!(get_active_project_id(&pool).await.unwrap(), "music");
    }

    #[tokio::test]
    async fn duplicate_id_rejected() {
        let pool = fresh_pool().await;
        let err = create_project(
            &pool,
            CreateArgs {
                id: "default".into(),
                display_name: "Should fail".into(),
                root_path: None,
                icon: None,
                color: None,
                description: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("already exists"));
    }
}
