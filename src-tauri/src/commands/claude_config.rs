//! `/claude` config browser — the `#[tauri::command]` wrappers.
//!
//! The scanner, the path helpers and the asset-pin cores live in the ungated
//! `server::shared::claude_config` (WP-19 slice 5b), so the daemon's
//! `/api/rpc` arms run the very same code. Everything is re-exported here, so
//! `crate::commands::claude_config::*` paths (the store, ngwa, discovery and
//! iyke callers) keep resolving unchanged. What stays in this file is what
//! needs Tauri: the `State`/`AppHandle` wrappers, the fs watchers (which emit
//! on a Tauri event channel), and the 4-tier discovery (which reads the live
//! pkg kernel).

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, State};

pub use crate::server::shared::claude_config::*;

use crate::fs_watch::FsWatchManager;

// ─── Public commands ────────────────────────────────────────────────────────

/// Scan all project roots + personal dir, return a single config tree.
#[tauri::command]
pub async fn claude_config_load(
    #[allow(non_snake_case)] projectRoots: Vec<String>,
) -> Result<ClaudeConfig, String> {
    let project_roots = projectRoots;
    tokio::task::spawn_blocking(move || scan_all(project_roots))
        .await
        .map_err(|e| format!("join failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// Watch every `.claude/` dir in the supplied roots + the personal `~/.claude/`.
/// One watcher per dir, all emitting on the same event channel
/// `claude-config:changed`. Returns the list of watcher ids so the frontend
/// can release them on unmount via `claude_config_unwatch`.
#[tauri::command]
pub async fn claude_config_watch(
    app: AppHandle,
    manager: State<'_, Arc<FsWatchManager>>,
    #[allow(non_snake_case)] projectRoots: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    for root in &projectRoots {
        if let Ok(p) = expand(root) {
            let claude_dir = p.join(".claude");
            if claude_dir.is_dir() {
                targets.push(claude_dir);
            }
        }
    }
    if let Some(home) = home_dir() {
        let personal = home.join(".claude");
        if personal.is_dir() {
            targets.push(personal);
        }
    }
    for t in targets {
        match manager.watch(app.clone(), &t) {
            Ok(id) => ids.push(id),
            Err(e) => log::warn!("claude_config_watch failed for {}: {e}", t.display()),
        }
    }
    Ok(ids)
}

#[tauri::command]
pub async fn claude_config_unwatch(
    manager: State<'_, Arc<FsWatchManager>>,
    #[allow(non_snake_case)] watcherIds: Vec<String>,
) -> Result<(), String> {
    for id in watcherIds {
        let _ = manager.unwatch(&id);
    }
    Ok(())
}

/// Read an arbitrary file under a `.claude/` dir (e.g. a hook script body or a
/// skill supporting file). Restricted to paths whose canonical form sits under
/// either a `.claude/` segment or `~/.claude/`. Read-only.
#[tauri::command]
pub async fn claude_config_read_file(path: String) -> Result<String, String> {
    let resolved = expand(&path).map_err(|e| e.to_string())?;
    if !is_under_claude_dir(&resolved) {
        return Err(format!(
            "path not under a .claude/ dir: {}",
            resolved.display()
        ));
    }
    tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|e| format!("read failed: {e}"))
}

// ─── Phase 4 — 4-tier discovery + pin CRUD (new surface) ────────────────────
//
// These commands sit alongside the legacy `claude_config_*` ones. The FE
// migrates incrementally — old `/claude` route keeps the legacy ones; new
// "Claude Config Browser" UI consumes the layered tree.

use crate::claude::discovery::{self, AssetTree};
use crate::commands::db::PaDb;
use crate::commands::projects::get_active_project_id;

/// Run the 4-tier layered discovery.
///
/// `projectId` defaults to the currently-active project. The returned
/// `AssetTree` lists *all* sources for each asset name; consumers apply
/// `resolve_preferred` (or the equivalent FE helper) to pick the active one.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_assets_discover(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    projectId: Option<String>,
) -> Result<AssetTree, String> {
    let pool = db.ensure_pool().await?;
    let active = match projectId {
        Some(id) => id,
        None => get_active_project_id(&pool).await?,
    };
    discovery::discover(&active, &pool, &app).await
}

/// Insert / update a pin row. `preferredSource` is nullable for the personal
/// tier (there's only one personal source); for pkg tiers callers should pass
/// the pkg id so the pin can disambiguate between multiple pkgs declaring the
/// same asset name.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_pin(
    db: State<'_, Arc<PaDb>>,
    scope: String,
    assetKind: String,
    assetName: String,
    preferredTier: String,
    preferredSource: Option<String>,
) -> Result<(), String> {
    asset_pin(
        &db,
        scope,
        assetKind,
        assetName,
        preferredTier,
        preferredSource,
    )
    .await
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_unpin(
    db: State<'_, Arc<PaDb>>,
    scope: String,
    assetKind: String,
    assetName: String,
) -> Result<(), String> {
    asset_unpin(&db, scope, assetKind, assetName).await
}

#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_asset_list_pins(
    db: State<'_, Arc<PaDb>>,
    scope: String,
) -> Result<Vec<AssetPin>, String> {
    asset_list_pins(&db, scope).await
}
