//! Package kernel.
//!
//! Owns the registry list and drives install / uninstall / boot. Every
//! lifecycle operation runs under a per-package lock (held in a `Mutex<()>`
//! inside the kernel) so install/uninstall can never interleave for the same
//! package id.
//!
//! The kernel is the *only* place that touches `pkg_installed` and the only
//! place that calls `Registry::register/unregister`. Other code that wants to
//! "see what's registered" reads through the kernel's snapshot API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use tauri::AppHandle;

use crate::commands::db::PaDb;

use super::cap_snapshot;
use super::file_watcher::{self, WatcherHandle};
use super::manifest::{Package, IKENGA_API_MIN_SUPPORTED, IKENGA_API_VERSION};
use super::registry::Registry;
use super::source::InstallSource;

/// Status returned by `pkg_kernel_status` — useful for debugging and the
/// future Settings → Packages page.
#[derive(Debug, Serialize)]
pub struct KernelStatus {
    pub installed: Vec<InstalledSummary>,
    pub registries: HashMap<String, Value>,
    pub api_version: u32,
}

/// One entry returned by `Kernel::discover_workspace` — a manifest dir found
/// in a workspace path. `valid=false` means the dir had a manifest.json but
/// it failed to parse; `error` carries the reason.
#[derive(Debug, Serialize, Clone)]
pub struct DiscoveredPkg {
    pub id: String,
    pub name: String,
    pub version: String,
    pub install_path: String,
    pub valid: bool,
    pub error: Option<String>,
    pub installed: bool,
    pub compatible: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct InstalledSummary {
    pub id: String,
    pub version: String,
    pub ikenga_api: String,
    pub install_path: String,
    pub enabled: bool,
    pub installed_at: i64,
    pub compatible: bool,
    /// Provenance — recorded at install time, used by the UI for grouping
    /// and by the kernel to refuse uninstall of `Builtin` pkgs.
    pub source: InstallSource,
    /// Scope (Phase 2 of projects-first-class). `Some("default" | "music-2026" | …)`
    /// means the pkg loads only when that project is active; `None` is the
    /// workspace scope (always loaded). The Phase 0 bootstrap stamps existing
    /// rows with `Some("default")` so they remain visible after upgrade.
    pub project_id: Option<String>,
}

/// Child tables carrying a `pkg_id` referencing `pkg_installed(id)`. Three
/// declare `ON DELETE CASCADE` (migration 0007); `pkg_capability_snapshots`
/// (0021) does NOT — deleting a parent leaves its snapshot orphaned, which is
/// exactly the drift the health check surfaces. Listed once so purge is
/// explicit and robust regardless of the connection's `foreign_keys` pragma.
/// `pkg_permission_violations` is deliberately absent — it is an audit log,
/// not parent-owned state.
const PKG_CHILD_TABLES: &[&str] = &[
    "pkg_capability_snapshots",
    "pkg_settings",
    "pkg_permissions_granted",
    "pkg_migrations",
];

/// One broken or orphaned install record surfaced by [`scan_health`]. The
/// cross-boundary contract (Rust serde ↔ the `PkgHealthIssue` TS type in
/// `tauri-cmd.ts`); keep both in lockstep.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct PkgHealthIssue {
    /// pkg id — or the orphan row's `pkg_id` for `OrphanRow`.
    pub id: String,
    /// `install_path` from `pkg_installed`; empty for table-only orphans.
    pub install_path: String,
    pub enabled: bool,
    pub issue: HealthIssueKind,
    /// Human-readable reason, safe to show in the UI / CLI.
    pub detail: String,
}

/// Why an install record is unhealthy. Serializes as a tagged union
/// (`{ "kind": "manifest_missing" }`, `{ "kind": "api_incompatible", "ikenga_api": "9" }`, …).
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealthIssueKind {
    /// `install_path` has no `manifest.json`.
    ManifestMissing,
    /// `manifest.json` exists but can't be read (IO/permission error).
    ManifestUnreadable,
    /// `manifest.json` exists but failed to parse / validate.
    ManifestUnparseable,
    /// Loads fine but `ikenga_api` is outside the supported window.
    ApiIncompatible { ikenga_api: String },
    /// A child `pkg_*` row with no parent in `pkg_installed`.
    OrphanRow { table: String },
}

/// Scan every `pkg_installed` row (enabled **and** disabled — boot only loads
/// enabled ones, so disabled-but-broken rows would otherwise never surface)
/// plus the child `pkg_*` tables, returning every broken/orphaned record.
/// Read-only — never mutates. The unit-tested core behind `Kernel::health_scan`,
/// the `pkg_health_*` commands, and `ikenga doctor`.
pub(crate) async fn scan_health(pool: &sqlx::SqlitePool) -> Result<Vec<PkgHealthIssue>> {
    let mut issues = Vec::new();

    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, install_path, enabled FROM pkg_installed")
            .fetch_all(pool)
            .await
            .map_err(|e| anyhow!("read pkg_installed: {e}"))?;

    for (id, install_path, enabled_raw) in rows {
        let enabled = enabled_raw != 0;
        let manifest_path = Path::new(&install_path).join("manifest.json");
        if !manifest_path.exists() {
            issues.push(PkgHealthIssue {
                id,
                detail: format!("no manifest.json at {install_path}"),
                install_path,
                enabled,
                issue: HealthIssueKind::ManifestMissing,
            });
            continue;
        }
        if let Err(e) = std::fs::read_to_string(&manifest_path) {
            issues.push(PkgHealthIssue {
                id,
                detail: format!("manifest.json unreadable: {e}"),
                install_path,
                enabled,
                issue: HealthIssueKind::ManifestUnreadable,
            });
            continue;
        }
        match Package::load(Path::new(&install_path)) {
            Ok(pkg) => {
                if !pkg.is_compatible() {
                    let api = pkg.manifest.ikenga_api.clone();
                    issues.push(PkgHealthIssue {
                        id,
                        detail: format!(
                            "ikenga_api={api} outside supported window {}..={}",
                            IKENGA_API_MIN_SUPPORTED, IKENGA_API_VERSION
                        ),
                        install_path,
                        enabled,
                        issue: HealthIssueKind::ApiIncompatible { ikenga_api: api },
                    });
                }
            }
            Err(e) => {
                issues.push(PkgHealthIssue {
                    id,
                    detail: format!("manifest.json failed to load: {e:#}"),
                    install_path,
                    enabled,
                    issue: HealthIssueKind::ManifestUnparseable,
                });
            }
        }
    }

    // Orphan child rows: a `pkg_id` with no parent in `pkg_installed`.
    for table in PKG_CHILD_TABLES {
        let sql =
            format!("SELECT pkg_id FROM {table} WHERE pkg_id NOT IN (SELECT id FROM pkg_installed)");
        let orphans: Vec<(String,)> = sqlx::query_as(&sql)
            .fetch_all(pool)
            .await
            .map_err(|e| anyhow!("scan orphans in {table}: {e}"))?;
        for (pkg_id,) in orphans {
            issues.push(PkgHealthIssue {
                id: pkg_id,
                install_path: String::new(),
                enabled: false,
                issue: HealthIssueKind::OrphanRow {
                    table: (*table).to_string(),
                },
                detail: format!("orphaned row in {table} (no parent pkg_installed)"),
            });
        }
    }

    Ok(issues)
}

/// Delete a `pkg_installed` row and every child `pkg_*` row for it, in one
/// transaction. Children are deleted explicitly so this is correct regardless
/// of the connection's `foreign_keys` pragma — and it clears
/// `pkg_capability_snapshots`, which has no cascade FK.
pub(crate) async fn purge_record(pool: &sqlx::SqlitePool, id: &str) -> Result<()> {
    let mut tx = pool.begin().await.map_err(|e| anyhow!("begin txn: {e}"))?;
    for table in PKG_CHILD_TABLES {
        sqlx::query(&format!("DELETE FROM {table} WHERE pkg_id = ?"))
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow!("delete {table} for {id}: {e}"))?;
    }
    sqlx::query("DELETE FROM pkg_installed WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("delete pkg_installed for {id}: {e}"))?;
    tx.commit().await.map_err(|e| anyhow!("commit txn: {e}"))?;
    Ok(())
}

/// Delete every child `pkg_*` row whose `pkg_id` has no parent in
/// `pkg_installed`. Returns the number of rows removed.
pub(crate) async fn purge_orphans(pool: &sqlx::SqlitePool) -> Result<u64> {
    let mut total = 0u64;
    let mut tx = pool.begin().await.map_err(|e| anyhow!("begin txn: {e}"))?;
    for table in PKG_CHILD_TABLES {
        let sql =
            format!("DELETE FROM {table} WHERE pkg_id NOT IN (SELECT id FROM pkg_installed)");
        let r = sqlx::query(&sql)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow!("purge orphans in {table}: {e}"))?;
        total += r.rows_affected();
    }
    tx.commit().await.map_err(|e| anyhow!("commit txn: {e}"))?;
    Ok(total)
}

pub struct Kernel {
    /// Registries are registered once at construction and never mutate after.
    registries: Vec<Arc<dyn Registry>>,

    /// Per-package locks to serialize install/uninstall on the same id.
    /// Stored as a single map behind one outer lock — contention is fine
    /// because lifecycle ops are rare and short.
    pkg_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,

    /// In-memory mirror of `pkg_installed` (read-side cache so listing
    /// doesn't hit SQLite on every call). Writes go to SQLite first.
    installed: RwLock<HashMap<String, InstalledSummary>>,

    /// Tauri app handle, needed to call `add_capability` and (eventually)
    /// resolve `app_data_dir`.
    app: AppHandle,

    /// Shared SQLite handle. The kernel writes to `pkg_installed` /
    /// `pkg_settings` / `pkg_permissions_granted` here; FK cascades on
    /// uninstall clean up children automatically.
    db: Arc<PaDb>,

    /// Phase 2 (projects-first-class): which pkgs are currently
    /// "registered with runtime registries" — i.e. their sidecars are
    /// running, their UI routes are mounted, their MCP/cron/etc.
    /// contributions are live. Distinct from `installed`, which is the
    /// durable set; `live ⊆ installed`. The reconciler diffs `live`
    /// against the target set and registers/unregisters to converge.
    live: RwLock<std::collections::HashSet<String>>,

    /// Dev-mode (2026-05-18): per-pkg file watchers spawned by
    /// `pkg_dev_register`. The handle holds the underlying notify
    /// debouncer; dropping it tears down the watcher worker. Keyed by
    /// pkg id so `pkg_dev_unregister` can drop precisely the one it owns.
    dev_watchers: RwLock<HashMap<String, WatcherHandle>>,

    /// Cached install-health snapshot, refreshed by `boot()` and every
    /// `health_scan()`. Read by the `pkg_health_scan` command so the UI can
    /// show the last result without re-hitting SQLite.
    health: RwLock<Vec<PkgHealthIssue>>,
}

/// Walk the registries in reverse order calling `unregister`. Per the
/// `Registry` trait contract, `unregister` must be a no-op on absent
/// pkgs — so a failure here is logged but never aborts the sequence.
/// Used by `Kernel::reload_pkg` (and called directly by tests).
fn replay_unregisters(registries: &[Arc<dyn Registry>], pkg_id: &str) {
    for reg in registries.iter().rev() {
        if let Err(e) = reg.unregister(pkg_id) {
            log::warn!(
                "[pkg_kernel] unregister `{}` for `{pkg_id}` failed (continuing): {e}",
                reg.name()
            );
        }
    }
}

/// Walk the registries forward calling `register(pkg)`. On the first
/// failure, walk the already-applied registries in reverse calling
/// `unregister` to roll back, then return the error. Returns the list of
/// registry names applied on success.
///
/// Pure function over the slice + package — no `&Kernel` or `AppHandle`
/// touched, which makes it directly testable with `MockRegistry`.
fn replay_registers(registries: &[Arc<dyn Registry>], pkg: &Package) -> Result<Vec<String>> {
    let mut applied: Vec<String> = Vec::new();
    for reg in registries {
        if let Err(e) = reg.register(pkg) {
            let pkg_id = &pkg.manifest.id;
            log::error!(
                "[pkg_kernel] register `{}` failed for `{pkg_id}`: {e}",
                reg.name()
            );
            // Roll back only the registries we managed to apply for this pkg.
            for name in applied.iter().rev() {
                if let Some(r) = registries.iter().find(|r| r.name() == name) {
                    if let Err(re) = r.unregister(pkg_id) {
                        log::warn!(
                            "[pkg_kernel] rollback `{name}` for `{pkg_id}` failed (continuing): {re}"
                        );
                    }
                }
            }
            return Err(e);
        }
        applied.push(reg.name().to_string());
    }
    Ok(applied)
}

impl Kernel {
    pub fn new(app: AppHandle, db: Arc<PaDb>, registries: Vec<Arc<dyn Registry>>) -> Self {
        Self {
            registries,
            pkg_locks: Mutex::new(HashMap::new()),
            installed: RwLock::new(HashMap::new()),
            app,
            db,
            live: RwLock::new(std::collections::HashSet::new()),
            dev_watchers: RwLock::new(HashMap::new()),
            health: RwLock::new(Vec::new()),
        }
    }

    /// Where unpacked packages live. `~/.local/share/ikenga/pkgs/<id>/`
    /// on Linux; the host-equivalent on mac/win.
    pub fn pkgs_dir(&self) -> Result<PathBuf> {
        let base = self
            .app
            .path()
            .app_data_dir()
            .map_err(|e| anyhow!("app_data_dir: {e}"))?;
        Ok(base.join("pkgs"))
    }

    /// Hook for the future install-from-archive path. For now packages are
    /// expected to already exist on disk at `install_path`. The caller must
    /// declare provenance via `source` so the kernel can stamp the
    /// `pkg_installed.source_json` row — this is what later distinguishes
    /// shell-bundled builtins from registry / sideloaded pkgs.
    /// Install a pkg at `install_path` with the given provenance + scope.
    /// `project_id = None` means workspace scope (always loaded);
    /// `Some("default" | other slug)` binds the pkg to that project so it
    /// only loads when the project is active. The kernel persists the
    /// scope on `pkg_installed.project_id` but does *not* perform
    /// reconciliation here — caller is responsible for kicking
    /// `reconcile_for_project` after install if the scope differs from the
    /// active project.
    pub fn install_from_path(
        &self,
        install_path: &Path,
        source: InstallSource,
        project_id: Option<String>,
    ) -> Result<InstalledSummary> {
        let pkg = Package::load(install_path)
            .with_context(|| format!("load manifest at {}", install_path.display()))?;
        let pkg_id = pkg.manifest.id.clone();
        let lock = self.lock_for(&pkg_id);
        // Recover from poison: the `()` payload carries no state, so a prior
        // panic while holding the lock can't have left anything inconsistent.
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        if !pkg.is_compatible() {
            return Err(anyhow!(
                "package `{pkg_id}` declares ikenga_api={}, host supports [{IKENGA_API_MIN_SUPPORTED}, {IKENGA_API_VERSION}]",
                pkg.manifest.ikenga_api
            ));
        }

        // Reject if already installed at a different path. Re-installing the
        // same path is treated as boot replay (idempotent register, no DB
        // write) — useful for dev-loop where the same dir gets re-poked.
        // Preserve a stronger pre-existing source: if the row is already
        // marked Builtin, never downgrade it to Local on a path-equal replay
        // (e.g. workspace dev pointing at the same dir as a builtin).
        let existing = self
            .installed
            .read()
            .ok()
            .and_then(|g| g.get(&pkg_id).cloned());
        let effective_source = if let Some(existing) = &existing {
            if existing.install_path != pkg.install_path.display().to_string() {
                return Err(anyhow!(
                    "package `{pkg_id}` already installed from {} — uninstall first",
                    existing.install_path
                ));
            }
            if existing.source.is_builtin() && !source.is_builtin() {
                existing.source.clone()
            } else {
                source
            }
        } else {
            source
        };

        // A same-path row already exists → this is a re-install / in-place
        // upgrade (registry update or dev re-poke), not a first install.
        // Several registries (ui_routes, sidecars, mcp) aren't idempotent on
        // `register`, so unregister the prior version first. unregister is a
        // no-op on absent ids per the Registry trait, so the forward loop
        // below still behaves like a clean install. On a fresh install
        // `existing` is None and this is skipped.
        let is_reinstall = existing.is_some();
        if is_reinstall {
            for reg in self.registries.iter().rev() {
                if let Err(e) = reg.unregister(&pkg_id) {
                    log::warn!(
                        "[pkg_kernel] pre-reinstall unregister `{}` for `{pkg_id}` failed (continuing): {e}",
                        reg.name()
                    );
                }
            }
        }

        let installed_at = chrono::Utc::now().timestamp_millis();
        let summary = InstalledSummary {
            id: pkg_id.clone(),
            version: pkg.manifest.version.clone(),
            ikenga_api: pkg.manifest.ikenga_api.clone(),
            install_path: pkg.install_path.display().to_string(),
            enabled: true,
            installed_at,
            compatible: true,
            source: effective_source,
            project_id,
        };

        // Persist the parent `pkg_installed` row BEFORE running registries.
        // Several registries (permissions, settings, migrations) write child
        // rows with `FOREIGN KEY(pkg_id) REFERENCES pkg_installed(id)`, so
        // they need the parent committed first. If the parent write fails,
        // bail before touching any registry.
        let manifest_json =
            serde_json::to_string(&pkg.manifest).map_err(|e| anyhow!("serialize manifest: {e}"))?;
        self.persist_install(&summary, &manifest_json)?;

        // Register against every registry in order. On any failure, walk
        // back over what succeeded, then drop the orphan parent row so the
        // user can retry cleanly.
        let mut applied: Vec<&str> = Vec::new();
        for reg in &self.registries {
            if let Err(e) = reg.register(&pkg) {
                log::error!(
                    "[pkg_kernel] register `{}` failed for `{pkg_id}`: {e}",
                    reg.name()
                );
                self.rollback(&pkg_id, &applied);
                if let Err(de) = self.delete_install_row(&pkg_id) {
                    log::warn!(
                        "[pkg_kernel] post-rollback delete `{pkg_id}` failed (continuing): {de:#}"
                    );
                }
                return Err(e);
            }
            applied.push(reg.name());
        }

        self.installed
            .write()
            .map_err(|_| anyhow!("installed lock poisoned"))?
            .insert(pkg_id.clone(), summary.clone());

        // Trust-review modal (2026-05-15): the install itself is implicit
        // consent for the manifest's current capabilities + permissions.
        // Subsequent upgrades that change the normalized blob will trip
        // the boot-time diff and surface in the review modal. Best-effort:
        // a write failure here just means the first re-boot will see a
        // missing snapshot and re-stamp implicitly, which is the same
        // semantics; we log and continue.
        let snapshot_json = cap_snapshot::normalize(&pkg.manifest);
        let db_for_snap = self.db.clone();
        let id_for_snap = pkg_id.clone();
        if let Err(e) = tauri::async_runtime::block_on(async move {
            let pool = db_for_snap.ensure_pool().await.map_err(|e| anyhow!(e))?;
            cap_snapshot::write_implicit(&pool, &id_for_snap, &snapshot_json).await
        }) {
            log::warn!(
                "[pkg_kernel] cap-snapshot write_implicit for `{pkg_id}` failed (continuing): {e:#}"
            );
        }

        log::info!(
            "[pkg_kernel] installed `{pkg_id}` v{} ({} registries)",
            pkg.manifest.version,
            applied.len()
        );

        // In-place upgrade/reinstall: tell the FE to remount any mounted
        // iframe/webview so it picks up the new bundle without a shell
        // restart (mirrors the dev-loop `pkg-reloaded` path). Harmless if the
        // pkg isn't currently mounted — the host filters by pkg_id.
        if is_reinstall {
            let mut live = self.live.write().unwrap_or_else(|e| e.into_inner());
            live.insert(pkg_id.clone());
            if let Err(e) = self.app.emit(
                "pkg-reloaded",
                serde_json::json!({ "pkg_id": pkg_id, "version": pkg.manifest.version }),
            ) {
                log::warn!("[pkg_kernel] emit pkg-reloaded for `{pkg_id}` failed: {e}");
            }
        }

        Ok(summary)
    }

    fn delete_install_row(&self, pkg_id: &str) -> Result<()> {
        let db = self.db.clone();
        let id_owned = pkg_id.to_string();
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query("DELETE FROM pkg_installed WHERE id = ?")
                .bind(&id_owned)
                .execute(&pool)
                .await
                .map_err(|e| anyhow!("delete pkg_installed: {e}"))?;
            Ok::<_, anyhow::Error>(())
        })
    }

    fn persist_install(&self, s: &InstalledSummary, manifest_json: &str) -> Result<()> {
        let db = self.db.clone();
        let source_json = serde_json::to_string(&s.source)
            .map_err(|e| anyhow!("serialize install source: {e}"))?;
        let row = (
            s.id.clone(),
            s.version.clone(),
            s.ikenga_api.clone(),
            manifest_json.to_string(),
            s.install_path.clone(),
            s.installed_at,
            source_json,
            s.project_id.clone(),
        );
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query(
                "INSERT OR REPLACE INTO pkg_installed
                 (id, version, ikenga_api, manifest_json, install_path, installed_at, enabled, source_json, project_id)
                 VALUES (?, ?, ?, ?, ?, ?, 1, ?, ?)",
            )
            .bind(&row.0)
            .bind(&row.1)
            .bind(&row.2)
            .bind(&row.3)
            .bind(&row.4)
            .bind(row.5)
            .bind(&row.6)
            .bind(&row.7)
            .execute(&pool)
            .await
            .map_err(|e| anyhow!("insert pkg_installed: {e}"))?;
            Ok::<_, anyhow::Error>(())
        })
    }

    /// Phase 2: update the scope of an already-installed pkg. `None` means
    /// workspace; `Some(slug)` rebinds it to that project. Returns Err if
    /// the pkg isn't installed. The caller should run a reconcile after
    /// updating to start/stop sidecars affected by the change.
    pub fn set_scope(&self, pkg_id: &str, project_id: Option<String>) -> Result<()> {
        let lock = self.lock_for(pkg_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let exists = self
            .installed
            .read()
            .map(|g| g.contains_key(pkg_id))
            .unwrap_or(false);
        if !exists {
            return Err(anyhow!("pkg not installed: {pkg_id}"));
        }
        let db = self.db.clone();
        let id_owned = pkg_id.to_string();
        let scope_owned = project_id.clone();
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query("UPDATE pkg_installed SET project_id = ? WHERE id = ?")
                .bind(&scope_owned)
                .bind(&id_owned)
                .execute(&pool)
                .await
                .map_err(|e| anyhow!("update project_id: {e}"))?;
            Ok::<_, anyhow::Error>(())
        })?;
        if let Ok(mut g) = self.installed.write() {
            if let Some(existing) = g.get_mut(pkg_id) {
                existing.project_id = project_id;
            }
        }
        Ok(())
    }

    /// Whether `pkg_id` is currently visible under `active_project_id`.
    /// Workspace-scoped pkgs (project_id None) are always visible.
    /// Returns false for unknown pkg ids.
    #[allow(dead_code)]
pub fn is_visible_under(&self, pkg_id: &str, active_project_id: &str) -> bool {
        self.installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).map(|s| s.project_id.clone()))
            .map(|scope| match scope {
                None => true,
                Some(p) => p == active_project_id,
            })
            .unwrap_or(false)
    }

    /// Reconcile a pre-existing row's source. Used by `install_builtins()` to
    /// stamp `Builtin` on rows whose ids match the bundled set but were
    /// installed before the source column existed.
    fn reconcile_source(&self, pkg_id: &str, source: &InstallSource) -> Result<()> {
        let db = self.db.clone();
        let id_owned = pkg_id.to_string();
        let source_json =
            serde_json::to_string(source).map_err(|e| anyhow!("serialize install source: {e}"))?;
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query("UPDATE pkg_installed SET source_json = ? WHERE id = ?")
                .bind(&source_json)
                .bind(&id_owned)
                .execute(&pool)
                .await
                .map_err(|e| anyhow!("update source_json: {e}"))?;
            Ok::<_, anyhow::Error>(())
        })?;
        if let Ok(mut g) = self.installed.write() {
            if let Some(existing) = g.get_mut(pkg_id) {
                existing.source = source.clone();
            }
        }
        Ok(())
    }

    /// Uninstall: walk registries in reverse, drop the row, mark disabled.
    /// Tauri ACL grants are NOT actually revoked — `add_capability` has no
    /// counterpart. The kernel-side allowlists in each registry stop spawning
    /// the package's binaries / accepting its iyke routes immediately; the
    /// OS-level ACL revocation requires a restart.
    pub fn uninstall(&self, pkg_id: &str) -> Result<()> {
        let lock = self.lock_for(pkg_id);
        // Recover from poison: the `()` payload carries no state, so a prior
        // panic while holding the lock can't have left anything inconsistent.
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // Refuse to uninstall shell-bundled builtins. Enforced here (not just
        // in the UI) so CLI / iyke / future remote callers also can't strip
        // them — they'd just get auto-reinstalled on next boot anyway.
        let is_builtin = self
            .installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).map(|s| s.source.is_builtin()))
            .unwrap_or(false);
        if is_builtin {
            return Err(anyhow!(
                "package `{pkg_id}` is shipped with the shell and cannot be uninstalled (disable it instead)"
            ));
        }
        for reg in self.registries.iter().rev() {
            if let Err(e) = reg.unregister(pkg_id) {
                log::warn!(
                    "[pkg_kernel] unregister `{}` failed for `{pkg_id}` (continuing): {e}",
                    reg.name()
                );
            }
        }
        // FK cascades drop pkg_settings / pkg_migrations / pkg_permissions_granted.
        if let Err(e) = self.delete_install_row(pkg_id) {
            log::warn!("[pkg_kernel] DB delete for `{pkg_id}` failed (continuing): {e:#}");
        }
        self.installed
            .write()
            .map_err(|_| anyhow!("installed lock poisoned"))?
            .remove(pkg_id);
        log::info!("[pkg_kernel] uninstalled `{pkg_id}` (restart for full ACL revocation)");
        Ok(())
    }

    /// Live enable/disable. Disable walks registries in reverse so spawning
    /// stops immediately, but keeps the row + manifest_json + child rows
    /// (settings/permissions/migrations) so re-enabling is loss-free.
    pub fn set_enabled(&self, pkg_id: &str, enabled: bool) -> Result<()> {
        let lock = self.lock_for(pkg_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let current = {
            let g = self
                .installed
                .read()
                .map_err(|_| anyhow!("installed lock poisoned"))?;
            g.get(pkg_id).cloned()
        };
        let Some(mut summary) = current else {
            return Err(anyhow!("pkg not installed: {pkg_id}"));
        };
        if summary.enabled == enabled {
            return Ok(());
        }
        if enabled {
            let pkg = Package::load(Path::new(&summary.install_path))
                .with_context(|| format!("load `{pkg_id}` from {}", summary.install_path))?;
            if !pkg.is_compatible() {
                return Err(anyhow!(
                    "pkg `{pkg_id}` ikenga_api={} outside support window",
                    pkg.manifest.ikenga_api
                ));
            }
            let mut applied: Vec<&str> = Vec::new();
            for reg in &self.registries {
                if let Err(e) = reg.register(&pkg) {
                    self.rollback(pkg_id, &applied);
                    return Err(e);
                }
                applied.push(reg.name());
            }
        } else {
            for reg in self.registries.iter().rev() {
                if let Err(e) = reg.unregister(pkg_id) {
                    log::warn!(
                        "[pkg_kernel] unregister `{}` failed for `{pkg_id}` (continuing): {e}",
                        reg.name()
                    );
                }
            }
        }
        self.update_enabled_row(pkg_id, enabled)?;
        summary.enabled = enabled;
        self.installed
            .write()
            .map_err(|_| anyhow!("installed lock poisoned"))?
            .insert(pkg_id.to_string(), summary);
        log::info!(
            "[pkg_kernel] {} `{pkg_id}`",
            if enabled { "enabled" } else { "disabled" }
        );
        Ok(())
    }

    fn update_enabled_row(&self, pkg_id: &str, enabled: bool) -> Result<()> {
        let db = self.db.clone();
        let id_owned = pkg_id.to_string();
        let val: i64 = if enabled { 1 } else { 0 };
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            sqlx::query("UPDATE pkg_installed SET enabled = ? WHERE id = ?")
                .bind(val)
                .bind(&id_owned)
                .execute(&pool)
                .await
                .map_err(|e| anyhow!("update enabled: {e}"))?;
            Ok::<_, anyhow::Error>(())
        })
    }

    /// Discover (but do NOT install) packages under a workspace directory.
    /// Used in dev mode to surface sibling pkgs from a monorepo-style
    /// workspace (e.g. `royalti-co/ikenga/pkgs/*`) in the Pkg Manager UI so
    /// the user can opt-in to installing them with `pkg_install_from_path`.
    ///
    /// Read-only: never mutates `pkg_installed` or any registry. Returns one
    /// entry per direct child directory that contains a parseable
    /// `manifest.json`; entries that fail to parse are reported as
    /// `valid=false` with the error so the FE can show a useful warning
    /// rather than silently dropping them.
    pub fn discover_workspace(&self, workspace_dir: &Path) -> Vec<DiscoveredPkg> {
        let mut out = Vec::new();
        if !workspace_dir.is_dir() {
            return out;
        }
        let entries = match std::fs::read_dir(workspace_dir) {
            Ok(e) => e,
            Err(err) => {
                log::warn!(
                    "[pkg_kernel] discover_workspace: read_dir({}) failed: {err}",
                    workspace_dir.display()
                );
                return out;
            }
        };
        let installed_ids: std::collections::HashSet<String> = self
            .installed
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            match super::manifest::Package::load(&path) {
                Ok(pkg) => out.push(DiscoveredPkg {
                    id: pkg.manifest.id.clone(),
                    name: pkg.manifest.name.clone(),
                    version: pkg.manifest.version.clone(),
                    install_path: path.display().to_string(),
                    valid: true,
                    error: None,
                    installed: installed_ids.contains(&pkg.manifest.id),
                    compatible: pkg.is_compatible(),
                }),
                Err(e) => out.push(DiscoveredPkg {
                    id: String::new(),
                    name: String::new(),
                    version: String::new(),
                    install_path: path.display().to_string(),
                    valid: false,
                    error: Some(format!("{e:#}")),
                    installed: false,
                    compatible: false,
                }),
            }
        }
        out
    }

    /// Auto-install built-in packages bundled with the app on first boot.
    ///
    /// The desktop app ships a small set of "meta-packages" (today: just
    /// `com.ikenga.iyke`) under `<resource_dir>/builtin-pkgs/`. Each one is
    /// installed exactly like a user package — same manifest contract, same
    /// kernel lifecycle — but the kernel is responsible for ensuring at least
    /// the iyke skill is present on a fresh machine so any Claude session
    /// (in-app or terminal) can drive the desktop UI from day one.
    ///
    /// Idempotent: skips any built-in already in `pkg_installed`. Failures
    /// log a warning and continue — they're not fatal because the rest of
    /// the kernel works without these packages.
    pub fn install_builtins(&self, resource_dir: &Path) -> Result<()> {
        let builtins_dir = resource_dir.join("builtin-pkgs");
        if !builtins_dir.is_dir() {
            log::info!(
                "[pkg_kernel] no builtin-pkgs/ at {} — skipping auto-install",
                builtins_dir.display()
            );
            return Ok(());
        }
        let entries = std::fs::read_dir(&builtins_dir)
            .with_context(|| format!("read {}", builtins_dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            // Cheap pre-read of the id so we can skip already-installed
            // built-ins without going through the full Package::load path.
            let id_opt = std::fs::read_to_string(&manifest_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("id").and_then(|s| s.as_str().map(String::from)));
            let id = match id_opt {
                Some(id) => id,
                None => {
                    log::warn!(
                        "[pkg_kernel] builtin at {} has no id — skipping",
                        path.display()
                    );
                    continue;
                }
            };
            let already_installed = self
                .installed
                .read()
                .map(|g| g.contains_key(&id))
                .unwrap_or(false);
            if already_installed {
                // Backfill: if the row predates the source column or was
                // (somehow) installed as Local, restamp it as Builtin so
                // the uninstall guard and UI grouping behave correctly.
                let needs_restamp = self
                    .installed
                    .read()
                    .ok()
                    .and_then(|g| g.get(&id).map(|s| !s.source.is_builtin()))
                    .unwrap_or(false);
                if needs_restamp {
                    if let Err(e) = self.reconcile_source(&id, &InstallSource::Builtin) {
                        log::warn!(
                            "[pkg_kernel] could not stamp `{id}` as Builtin (continuing): {e:#}"
                        );
                    } else {
                        log::info!("[pkg_kernel] reconciled `{id}` source → builtin");
                    }
                } else {
                    log::debug!("[pkg_kernel] builtin `{id}` already installed — skipping");
                }
                continue;
            }
            // Builtins are always workspace-scoped — they're shell-shipped
            // and ought to load regardless of which project is active.
            match self.install_from_path(&path, InstallSource::Builtin, None) {
                Ok(s) => log::info!(
                    "[pkg_kernel] auto-installed builtin `{}` v{}",
                    s.id,
                    s.version
                ),
                Err(e) => log::warn!(
                    "[pkg_kernel] auto-install builtin at {} failed (continuing): {e:#}",
                    path.display()
                ),
            }
        }
        Ok(())
    }

    /// Discover pkgs that exist on disk under `pkgs_dir()` but aren't yet
    /// tracked in `pkg_installed`. Used to pick up CLI-installed pkgs the
    /// user dropped in while the shell was offline — same pattern as
    /// `install_builtins`, but scans the runtime data dir and records the
    /// source as `Local` (the CLI doesn't currently write provenance to
    /// disk; a future `.source.json` sidecar would let this stamp
    /// `Registry { url }` instead).
    ///
    /// Idempotent — re-running on an already-tracked install path is a no-op.
    /// Failures on individual entries log and continue.
    pub fn install_from_pkgs_dir(&self) -> Result<()> {
        let dir = self.pkgs_dir()?;
        if !dir.is_dir() {
            return Ok(());
        }
        let entries = std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip the installer's own staging/backup directories.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with('.') {
                    continue;
                }
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.exists() {
                continue;
            }
            // Cheap pre-read of the id; skip if already-installed (the
            // path-equal replay inside install_from_path is also idempotent,
            // but checking here avoids re-reading the full manifest).
            let id_opt = std::fs::read_to_string(&manifest_path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.get("id").and_then(|s| s.as_str().map(String::from)));
            let id = match id_opt {
                Some(id) => id,
                None => {
                    log::warn!(
                        "[pkg_kernel] pkgs-dir entry at {} has no id — skipping",
                        path.display()
                    );
                    continue;
                }
            };
            let already_installed = self
                .installed
                .read()
                .map(|g| g.contains_key(&id))
                .unwrap_or(false);
            if already_installed {
                log::debug!("[pkg_kernel] pkgs-dir entry `{id}` already tracked — skipping");
                continue;
            }
            let path_str = path.display().to_string();
            // CLI sideloads default to workspace scope — the shell may not
            // know which project the user intended. They can move it via
            // Settings → Packages or `iyke_pkg_install_scope_set`.
            match self.install_from_path(&path, InstallSource::Local { path: path_str }, None) {
                Ok(s) => log::info!(
                    "[pkg_kernel] discovered pkgs-dir pkg `{}` v{} (CLI install)",
                    s.id,
                    s.version
                ),
                Err(e) => log::warn!(
                    "[pkg_kernel] register pkgs-dir entry at {} failed (continuing): {e:#}",
                    path.display()
                ),
            }
        }
        Ok(())
    }

    /// Boot-time replay: read every enabled `pkg_installed` row, reconstruct
    /// the Package from disk, and replay register against every registry. A
    /// package whose `install_path` is missing or whose manifest no longer
    /// loads gets logged and skipped — the row stays so the user can decide
    /// to repair or uninstall via the UI.
    pub fn boot(&self) -> Result<()> {
        let db = self.db.clone();
        let (rows, total_rows): (
            Vec<(String, String, i64, Option<String>, Option<String>)>,
            i64,
        ) = tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            // Diagnostic: total row count regardless of `enabled`. Distinguishes
            // "wrong DB file" / "missing rows" from "all rows disabled".
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pkg_installed")
                .fetch_one(&pool)
                .await
                .map_err(|e| anyhow!("count pkg_installed: {e}"))?;
            let r: Vec<(String, String, i64, Option<String>, Option<String>)> = sqlx::query_as(
                "SELECT id, install_path, installed_at, source_json, project_id
                 FROM pkg_installed WHERE enabled = 1",
            )
            .fetch_all(&pool)
            .await
            .map_err(|e| anyhow!("read pkg_installed: {e}"))?;
            Ok::<_, anyhow::Error>((r, total))
        })?;
        log::info!(
            "[pkg_kernel] boot: pkg_installed total_rows={total_rows} enabled_rows={}",
            rows.len()
        );

        let mut replayed = 0usize;
        let mut skipped = 0usize;
        let mut parked_for_review = 0usize;
        for (id, install_path, installed_at, source_raw, project_id) in rows {
            match Package::load(Path::new(&install_path)) {
                Ok(pkg) => {
                    if !pkg.is_compatible() {
                        log::warn!(
                            "[pkg_kernel] boot: `{id}` ikenga_api={} outside support window — skipping",
                            pkg.manifest.ikenga_api
                        );
                        skipped += 1;
                        continue;
                    }

                    // Trust-review modal (2026-05-15): diff the current
                    // manifest's normalized capabilities + permissions
                    // against the stored snapshot. No snapshot → first
                    // boot for this pkg, treat as implicit approval and
                    // proceed. Matching snapshot → proceed. Mismatched
                    // snapshot → record the pkg as "installed but pending
                    // review" (in `self.installed` so the FE can list it,
                    // but skip the registry replay so sidecars / MCPs
                    // never start). The user resolves via the
                    // `pkg_trust_*` commands.
                    let current_norm = cap_snapshot::normalize(&pkg.manifest);
                    let db_for_snap = self.db.clone();
                    let id_for_snap = id.clone();
                    let norm_for_check = current_norm.clone();
                    let snap_result = tauri::async_runtime::block_on(async move {
                        let pool = db_for_snap.ensure_pool().await.map_err(|e| anyhow!(e))?;
                        cap_snapshot::fetch(&pool, &id_for_snap).await
                    });
                    let needs_review = match snap_result {
                        Ok(Some(snap)) => cap_snapshot::capabilities_changed(
                            &snap.manifest_capabilities_json,
                            &norm_for_check,
                        ),
                        Ok(None) => {
                            // No snapshot — write implicit and proceed.
                            let db = self.db.clone();
                            let id_w = id.clone();
                            let norm_w = current_norm.clone();
                            if let Err(e) = tauri::async_runtime::block_on(async move {
                                let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
                                cap_snapshot::write_implicit(&pool, &id_w, &norm_w).await
                            }) {
                                log::warn!(
                                    "[pkg_kernel] boot: cap-snapshot implicit write for `{id}` failed (continuing): {e:#}"
                                );
                            }
                            false
                        }
                        Err(e) => {
                            log::warn!(
                                "[pkg_kernel] boot: cap-snapshot fetch for `{id}` failed (proceeding): {e:#}"
                            );
                            false
                        }
                    };

                    let source =
                        InstallSource::parse_or_local(source_raw.as_deref(), &install_path);
                    let summary = InstalledSummary {
                        id: id.clone(),
                        version: pkg.manifest.version.clone(),
                        ikenga_api: pkg.manifest.ikenga_api.clone(),
                        install_path: install_path.clone(),
                        enabled: true,
                        installed_at,
                        compatible: true,
                        source,
                        project_id,
                    };

                    if needs_review {
                        // Park: insert into `installed` so the FE / list
                        // commands see the pkg, but skip the registry
                        // replay. The pkg won't enter `live` either, so
                        // the project reconciler won't accidentally
                        // resurrect it.
                        if let Ok(mut g) = self.installed.write() {
                            g.insert(id.clone(), summary);
                        }
                        log::info!(
                            "[pkg_kernel] boot: `{id}` parked pending capability review — \
                             not registering until user approves"
                        );
                        parked_for_review += 1;
                        continue;
                    }

                    let mut applied: Vec<&str> = Vec::new();
                    let mut failed = false;
                    for reg in &self.registries {
                        if let Err(e) = reg.register(&pkg) {
                            log::error!(
                                "[pkg_kernel] boot: register `{}` failed for `{id}`: {e}",
                                reg.name()
                            );
                            self.rollback(&id, &applied);
                            failed = true;
                            break;
                        }
                        applied.push(reg.name());
                    }
                    if failed {
                        skipped += 1;
                        continue;
                    }
                    if let Ok(mut g) = self.installed.write() {
                        g.insert(id.clone(), summary);
                    }
                    replayed += 1;
                }
                Err(e) => {
                    log::warn!(
                        "[pkg_kernel] boot: load `{id}` from `{install_path}` failed (skipping): {e:#}"
                    );
                    skipped += 1;
                }
            }
        }
        log::info!(
            "[pkg_kernel] boot — {} registries, replayed {replayed}, skipped {skipped}, parked_for_review {parked_for_review}",
            self.registries.len()
        );

        // Stamp the install-health snapshot so the UI / CLI can report broken
        // and orphaned rows. Boot-time skips (above) only count enabled rows;
        // this scan covers disabled rows + orphan child tables too.
        match self.health_scan() {
            Ok(issues) if !issues.is_empty() => {
                let broken = issues
                    .iter()
                    .filter(|i| !matches!(i.issue, HealthIssueKind::OrphanRow { .. }))
                    .count();
                let orphans = issues.len() - broken;
                log::warn!(
                    "[pkg_kernel] health: {broken} broken install record(s) + {orphans} orphaned pkg_* row(s) — surface for cleanup (pkg_health_scan / `ikenga doctor`)"
                );
            }
            Ok(_) => {}
            Err(e) => log::warn!("[pkg_kernel] health scan at boot failed (continuing): {e:#}"),
        }

        Ok(())
    }

    /// Read-only scan for broken / orphaned install records. Refreshes the
    /// cached `health` snapshot and returns the issues. Backed by the
    /// module-level [`scan_health`] (the unit-tested core).
    pub fn health_scan(&self) -> Result<Vec<PkgHealthIssue>> {
        let db = self.db.clone();
        let issues = tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            scan_health(&pool).await
        })?;
        if let Ok(mut g) = self.health.write() {
            g.clone_from(&issues);
        }
        Ok(issues)
    }

    /// Delete a broken install record — its `pkg_installed` row + every child
    /// `pkg_*` row — and drop it from the in-memory maps. Serialized against
    /// install/uninstall via the per-pkg lock. Never touches the filesystem or
    /// registries: a broken row was never registered, so there is nothing to
    /// unregister.
    pub fn purge_install_record(&self, pkg_id: &str) -> Result<()> {
        let lock = self.lock_for(pkg_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let db = self.db.clone();
        let id = pkg_id.to_string();
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            purge_record(&pool, &id).await
        })?;
        if let Ok(mut g) = self.installed.write() {
            g.remove(pkg_id);
        }
        let mut g = self.live.write().unwrap_or_else(|e| e.into_inner());
        g.remove(pkg_id);
        Ok(())
    }

    /// Delete every orphaned child `pkg_*` row (no parent in `pkg_installed`).
    /// Returns the count removed.
    pub fn purge_orphan_rows(&self) -> Result<u64> {
        let db = self.db.clone();
        tauri::async_runtime::block_on(async move {
            let pool = db.ensure_pool().await.map_err(|e| anyhow!(e))?;
            purge_orphans(&pool).await
        })
    }

    /// Remove every currently-detected broken install record + orphan row.
    /// Returns `(records_removed, orphan_rows_removed)` and refreshes the cache.
    pub fn purge_all_broken(&self) -> Result<(usize, u64)> {
        let issues = self.health_scan()?;
        let mut broken_ids: Vec<String> = issues
            .iter()
            .filter(|i| !matches!(i.issue, HealthIssueKind::OrphanRow { .. }))
            .map(|i| i.id.clone())
            .collect();
        broken_ids.sort();
        broken_ids.dedup();
        let records = broken_ids.len();
        for id in &broken_ids {
            self.purge_install_record(id)?;
        }
        let orphans = self.purge_orphan_rows()?;
        let _ = self.health_scan();
        Ok((records, orphans))
    }

    /// Trust-review modal (2026-05-15) — run the registry replay for a pkg
    /// that was parked by `boot()` because its capability snapshot
    /// differed. Called after the user approves the diff via the modal.
    /// Idempotent if the pkg is already registered: the registries
    /// themselves enforce single-register semantics on their side.
    pub fn resume_after_review(&self, pkg_id: &str) -> Result<()> {
        let lock = self.lock_for(pkg_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        let install_path = self
            .installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).map(|s| s.install_path.clone()))
            .ok_or_else(|| anyhow!("pkg `{pkg_id}` not installed"))?;
        let pkg = Package::load(Path::new(&install_path))
            .with_context(|| format!("reload manifest for `{pkg_id}`"))?;

        let mut applied: Vec<&str> = Vec::new();
        for reg in &self.registries {
            if let Err(e) = reg.register(&pkg) {
                log::error!(
                    "[pkg_kernel] resume_after_review: register `{}` failed for `{pkg_id}`: {e}",
                    reg.name()
                );
                self.rollback(pkg_id, &applied);
                return Err(e);
            }
            applied.push(reg.name());
        }
        let mut g = self.live.write().unwrap_or_else(|e| e.into_inner());
        g.insert(pkg_id.to_string());
        log::info!(
            "[pkg_kernel] resume_after_review: `{pkg_id}` re-registered ({} registries)",
            applied.len()
        );
        Ok(())
    }

    /// Dev-mode (2026-05-18): atomically reload a pkg in place. Re-reads
    /// `manifest.json`, walks every registry's `unregister(pkg_id)`, then
    /// walks them forward calling `register(&pkg)`. On any register
    /// failure, rolls back via the existing `rollback` helper.
    ///
    /// Used by the `manifest.json` file watcher spawned in
    /// `pkg_dev_register` — manifest edits trip a 250ms-debounced reload.
    /// Safe for non-dev pkgs too (used by the iyke `pkg_dev_reload`
    /// command for explicit triggers), but the watcher is only spawned
    /// for `source.is_dev()`.
    ///
    /// Emits a `pkg-reloaded` Tauri event on success with the new version
    /// + the list of registries that re-registered. The FE iframe / webview
    /// hosts listen for this event to remount.
    ///
    /// Failure modes:
    /// - Pkg not installed → return error, no state change.
    /// - Manifest invalid or fails compatibility → return error, no
    ///   state change. Previous registrations remain intact because we
    ///   only call `unregister` after the new manifest validates.
    /// - One registry's `register` fails → roll back applied registries,
    ///   return error. The pkg ends up unregistered everywhere; the
    ///   caller can retry once the user fixes the manifest.
    pub fn reload_pkg(&self, pkg_id: &str) -> Result<InstalledSummary> {
        let lock = self.lock_for(pkg_id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

        // Validate before unregistering so a typo'd manifest can't leave the
        // pkg torn down with no recovery path.
        let install_path = self
            .installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).map(|s| s.install_path.clone()))
            .ok_or_else(|| anyhow!("pkg `{pkg_id}` not installed"))?;
        let pkg = Package::load(Path::new(&install_path))
            .with_context(|| format!("reload manifest for `{pkg_id}`"))?;
        if !pkg.is_compatible() {
            return Err(anyhow!(
                "pkg `{pkg_id}` ikenga_api={} outside support window [{IKENGA_API_MIN_SUPPORTED}, {IKENGA_API_VERSION}]",
                pkg.manifest.ikenga_api
            ));
        }

        // Unregister in reverse order (Registry trait says unregister is a
        // no-op on absent pkgs) then re-register forward with rollback. Both
        // halves are extracted as pure free functions so the sequence can be
        // exercised in tests without an AppHandle / SQLite / DB harness.
        replay_unregisters(&self.registries, pkg_id);
        // Reap leak fix (Finding D): the SidecarsRegistry `unregister` above only
        // drops path metadata — the StreamingSidecarManager still owns the
        // previous long-running child, which would be reused (stale binary) on
        // the next RPC send. `re-register` re-adds metadata but never respawns.
        // Evict the live child here so the next `pkg_sidecar_rpc_send` spawns the
        // freshly rebuilt binary. Synchronous (DashMap) — safe under the
        // spawn_blocking context reload_pkg runs in.
        let reaped = crate::commands::pkg_sidecar_stream::shutdown_pkg_sidecars(pkg_id);
        if reaped > 0 {
            log::info!("[pkg_kernel] reload `{pkg_id}`: reaped {reaped} stale streaming sidecar(s)");
        }
        let applied_names = match replay_registers(&self.registries, &pkg) {
            Ok(names) => names,
            Err(e) => {
                let mut live = self.live.write().unwrap_or_else(|e| e.into_inner());
                live.remove(pkg_id);
                return Err(e);
            }
        };

        // Refresh the installed snapshot — version + ikenga_api may have
        // changed in the new manifest. Source + project_id are preserved
        // from the pre-reload row.
        let updated = {
            let mut g = self
                .installed
                .write()
                .map_err(|_| anyhow!("installed lock poisoned"))?;
            let prev = g
                .get(pkg_id)
                .cloned()
                .ok_or_else(|| anyhow!("pkg `{pkg_id}` vanished mid-reload"))?;
            let updated = InstalledSummary {
                id: pkg_id.to_string(),
                version: pkg.manifest.version.clone(),
                ikenga_api: pkg.manifest.ikenga_api.clone(),
                install_path: prev.install_path.clone(),
                enabled: prev.enabled,
                installed_at: prev.installed_at,
                compatible: true,
                source: prev.source,
                project_id: prev.project_id,
            };
            g.insert(pkg_id.to_string(), updated.clone());
            updated
        };
        let mut live = self.live.write().unwrap_or_else(|e| e.into_inner());
        live.insert(pkg_id.to_string());

        // Best-effort event emission for the FE. A failure here means the
        // iframe/webview won't auto-remount, but the reload itself
        // succeeded — surface as a log line, not an error to the caller.
        if let Err(e) = self.app.emit(
            "pkg-reloaded",
            serde_json::json!({
                "pkg_id": pkg_id,
                "version": pkg.manifest.version,
                "registries": applied_names,
            }),
        ) {
            log::warn!("[pkg_kernel] reload: emit pkg-reloaded for `{pkg_id}` failed: {e}");
        }

        log::info!(
            "[pkg_kernel] reloaded `{pkg_id}` v{} ({} registries)",
            pkg.manifest.version,
            applied_names.len()
        );
        Ok(updated)
    }

    /// Dev-mode: install a `WatcherHandle` keyed by `pkg_id`. Replaces any
    /// existing watcher for the same id, which means dropping the previous
    /// handle and tearing down its worker. Idempotent re-registration is
    /// fine — `pkg_dev_register` may be called more than once during a
    /// CLI iterate.
    pub fn set_dev_watcher(&self, pkg_id: &str, handle: WatcherHandle) {
        if let Ok(mut g) = self.dev_watchers.write() {
            g.insert(pkg_id.to_string(), handle);
        }
    }

    /// Drop a dev-mode watcher (e.g. on `pkg_dev_unregister`). No-op if the
    /// pkg never had one.
    pub fn drop_dev_watcher(&self, pkg_id: &str) {
        if let Ok(mut g) = self.dev_watchers.write() {
            g.remove(pkg_id);
        }
    }

    /// Spawn a dev-mode file watcher for an already-installed dev pkg.
    /// Returns Ok(()) even when the pkg has no `restart_when_changed`
    /// globs — `manifest.json` is always watched. The watcher's on_change
    /// callback invokes `reload_pkg` (debounced 250ms by the underlying
    /// notify-debouncer-mini).
    pub fn spawn_dev_watcher(
        self: &Arc<Self>,
        pkg_id: &str,
        install_path: &Path,
        extra_globs: Vec<String>,
    ) -> Result<()> {
        let kernel_for_cb = Arc::clone(self);
        let id_for_cb = pkg_id.to_string();
        let handle = file_watcher::spawn_dev(install_path.to_path_buf(), extra_globs, move || {
            let kernel = Arc::clone(&kernel_for_cb);
            let id = id_for_cb.clone();
            // Run the reload on a blocking thread — registries call
            // `tauri::async_runtime::block_on` internally for DB writes,
            // and reentering the runtime panics.
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = kernel.reload_pkg(&id) {
                    log::warn!("[pkg_kernel] dev watcher: reload `{id}` failed: {e:#}");
                }
            });
        })
        .context("spawn dev file watcher")?;
        self.set_dev_watcher(pkg_id, handle);
        Ok(())
    }

    /// Look up an installed package by id and return its on-disk install
    /// path. Used by `pkg_mcp_call` to resolve relative paths in the
    /// manifest's mcp server `args` (working dir for the spawned child).
    pub fn installed_path(&self, pkg_id: &str) -> Option<PathBuf> {
        self.installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).map(|s| PathBuf::from(&s.install_path)))
    }

    /// Snapshot of the installed map. Useful for the reconciler and other
    /// callers that want full info, not just the kernel status payload.
    pub fn list_installed(&self) -> Vec<InstalledSummary> {
        self.installed
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Look up a single installed summary by id without cloning the whole
    /// map. Phase 5 of projects-first-class uses this to find an MCP child's
    /// own project scope (workspace or `project:<id>`) before spawning, so
    /// it can inject the matching `IKENGA_PROJECT_ID` env.
    pub fn installed_summary(&self, pkg_id: &str) -> Option<InstalledSummary> {
        self.installed
            .read()
            .ok()
            .and_then(|g| g.get(pkg_id).cloned())
    }

    /// Phase 2 reconciler. For each installed pkg, ensure its registry
    /// contribution + sidecar match its scope vs the active project:
    /// - workspace (project_id None) → always live.
    /// - project_id Some(p) where p == active → live.
    /// - otherwise → parked (unregistered from runtime registries).
    ///
    /// "Live" pkgs missing from runtime registries get registered.
    /// "Parked" pkgs present in runtime registries get unregistered.
    /// Idempotent: re-running with the same active is a no-op.
    ///
    /// We track which pkgs are currently "live" in `live` and compare on
    /// each reconcile to compute the delta.
    pub fn reconcile_for_project(&self, active_project_id: &str) -> Result<()> {
        // This function drives registries that call `block_on` internally, so
        // it must never run inside an async task on a tokio worker (issue #130:
        // "Cannot start a runtime from within a runtime"). Callers hop it onto
        // `spawn_blocking` or a plain thread. `tokio::task::try_id()` is `Some`
        // only when polled as a task — it is `None` on blocking-pool threads,
        // so `spawn_blocking` passes and an async caller trips this in dev.
        debug_assert!(
            tokio::task::try_id().is_none(),
            "reconcile_for_project called from inside an async task — use spawn_blocking (issue #130)"
        );
        let installed = self.list_installed();
        let want_live: std::collections::HashSet<String> = installed
            .iter()
            .filter(|s| match &s.project_id {
                None => true,
                Some(p) => p == active_project_id,
            })
            .map(|s| s.id.clone())
            .collect();

        let mut live_guard = self.live.write().unwrap_or_else(|e| e.into_inner());
        let prev_live: std::collections::HashSet<String> = live_guard.clone();

        // Park anything live → not in target set.
        for pkg_id in prev_live
            .difference(&want_live)
            .cloned()
            .collect::<Vec<_>>()
        {
            log::info!("[pkg_kernel] reconcile: parking `{pkg_id}` (scope mismatch)");
            for reg in self.registries.iter().rev() {
                if let Err(e) = reg.unregister(&pkg_id) {
                    log::warn!(
                        "[pkg_kernel] reconcile: unregister `{}` for `{pkg_id}` failed: {e}",
                        reg.name()
                    );
                }
            }
            live_guard.remove(&pkg_id);
        }

        // Resume anything in target set → not yet live.
        for pkg_id in want_live
            .difference(&prev_live)
            .cloned()
            .collect::<Vec<_>>()
        {
            let install_path = installed
                .iter()
                .find(|s| s.id == pkg_id)
                .map(|s| s.install_path.clone());
            let Some(install_path) = install_path else {
                continue;
            };
            match Package::load(Path::new(&install_path)) {
                Ok(pkg) => {
                    log::info!("[pkg_kernel] reconcile: resuming `{pkg_id}`");
                    let mut applied: Vec<&str> = Vec::new();
                    let mut failed = false;
                    for reg in &self.registries {
                        if let Err(e) = reg.register(&pkg) {
                            log::warn!(
                                "[pkg_kernel] reconcile: register `{}` for `{pkg_id}` failed: {e}",
                                reg.name()
                            );
                            // Roll back what we managed to apply for this pkg.
                            for name in applied.iter().rev() {
                                if let Some(r) = self.registries.iter().find(|r| r.name() == *name)
                                {
                                    let _ = r.unregister(&pkg_id);
                                }
                            }
                            failed = true;
                            break;
                        }
                        applied.push(reg.name());
                    }
                    if !failed {
                        live_guard.insert(pkg_id);
                    }
                }
                Err(e) => {
                    log::warn!(
                        "[pkg_kernel] reconcile: load `{pkg_id}` at {install_path} failed: {e:#}"
                    );
                }
            }
        }
        Ok(())
    }

    /// Mark every installed pkg as live. Used by `boot()` (which has
    /// already registered everything) so the reconciler knows the
    /// current truth at startup. Called from lib.rs setup after
    /// `kernel.boot()` returns.
    pub fn mark_all_live(&self) {
        if let Ok(g) = self.installed.read() {
            let mut live = self.live.write().unwrap_or_else(|e| e.into_inner());
            live.clear();
            for id in g.keys() {
                live.insert(id.clone());
            }
        }
    }

    pub fn status(&self) -> KernelStatus {
        let installed = self
            .installed
            .read()
            .map(|g| g.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let registries = self
            .registries
            .iter()
            .map(|r| (r.name().to_string(), r.snapshot()))
            .collect();
        KernelStatus {
            installed,
            registries,
            api_version: IKENGA_API_VERSION,
        }
    }

    fn rollback(&self, pkg_id: &str, applied: &[&str]) {
        for name in applied.iter().rev() {
            if let Some(reg) = self.registries.iter().find(|r| r.name() == *name) {
                if let Err(e) = reg.unregister(pkg_id) {
                    log::warn!("[pkg_kernel] rollback `{name}` for `{pkg_id}` failed: {e}");
                }
            }
        }
    }

    /// Returns the Arc for this package's lifecycle lock. Caller is
    /// expected to immediately `.lock()` it and hold the guard for the
    /// duration of the lifecycle op. Returning the Arc (not the guard)
    /// avoids lifetime gymnastics — the Arc owns the Mutex.
    fn lock_for(&self, pkg_id: &str) -> Arc<Mutex<()>> {
        let mut map = self.pkg_locks.lock().expect("pkg_locks poisoned");
        map.entry(pkg_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

// Use `tauri::Manager` for `app.path()` and `tauri::Emitter` for `app.emit()`.
use tauri::{Emitter, Manager};

#[cfg(test)]
mod tests {
    //! Tests for the registry replay helpers used by `Kernel::reload_pkg`.
    //! The Kernel itself depends on an `AppHandle` + SQLite pool which need
    //! the tauri test runtime; the helpers are pure functions over a slice
    //! of `Arc<dyn Registry>` + a `Package`, so we test them directly.

    use super::*;
    use crate::pkg::manifest::Manifest;
    use serde_json::Value;
    use std::sync::Mutex as StdMutex;

    /// Records every register/unregister call. Optionally fails on register
    /// to exercise the rollback path.
    struct MockRegistry {
        name: &'static str,
        fail_register: bool,
        events: Arc<StdMutex<Vec<String>>>,
    }

    impl Registry for MockRegistry {
        fn name(&self) -> &'static str {
            self.name
        }
        fn register(&self, pkg: &Package) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("register:{}:{}", self.name, pkg.manifest.id));
            if self.fail_register {
                return Err(anyhow!("mock register failure in {}", self.name));
            }
            Ok(())
        }
        fn unregister(&self, pkg_id: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("unregister:{}:{pkg_id}", self.name));
            Ok(())
        }
        fn snapshot(&self) -> Value {
            Value::Null
        }
    }

    fn mock(
        name: &'static str,
        fail_register: bool,
        events: Arc<StdMutex<Vec<String>>>,
    ) -> Arc<dyn Registry> {
        Arc::new(MockRegistry {
            name,
            fail_register,
            events,
        })
    }

    fn fixture_pkg(id: &str) -> Package {
        Package {
            manifest: Manifest {
                id: id.into(),
                name: "T".into(),
                version: "1.0.0".into(),
                ikenga_api: "1".into(),
                kind: None,
                author: None,
                targets: vec![],
                mcp: vec![],
                sidecars: vec![],
                permissions: Default::default(),
                migrations: None,
                settings: None,
                ui: None,
                iyke: None,
                cron: vec![],
                window: None,
                queries: None,
                capabilities: None,
                engine: None,
                screenshots: vec![],
                requires: vec![],
                signature: None,
            },
            install_path: PathBuf::from("/tmp"),
        }
    }

    /// WP-04 regression: reproduces the live-install drift this feature was
    /// built for — a `pkg_installed` row at a vanished path + an orphaned
    /// `pkg_capability_snapshots` row (no FK, so a parent delete leaves it).
    /// Asserts detection, targeted purge, orphan purge, and that valid
    /// neighbours survive. Runs against the free functions (no AppHandle).
    #[test]
    fn health_scan_detects_and_purge_cleans_broken_and_orphans() {
        use sqlx::sqlite::SqlitePoolOptions;
        tauri::async_runtime::block_on(async {
            let pool = SqlitePoolOptions::new()
                .max_connections(1) // single shared in-memory db
                .connect("sqlite::memory:")
                .await
                .expect("open in-memory sqlite");

            // Minimal schema mirroring migrations 0007 + 0021 (columns we touch).
            for ddl in [
                "CREATE TABLE pkg_installed (id TEXT PRIMARY KEY, version TEXT, ikenga_api TEXT, manifest_json TEXT, install_path TEXT NOT NULL, installed_at INTEGER, enabled INTEGER NOT NULL DEFAULT 1, signature TEXT, source_json TEXT, project_id TEXT)",
                "CREATE TABLE pkg_capability_snapshots (pkg_id TEXT PRIMARY KEY, manifest_capabilities_json TEXT NOT NULL, approved_at INTEGER NOT NULL, approved_by_implicit INTEGER NOT NULL DEFAULT 0)",
                "CREATE TABLE pkg_settings (pkg_id TEXT, key TEXT, value_json TEXT, updated_at INTEGER, PRIMARY KEY (pkg_id, key))",
                "CREATE TABLE pkg_permissions_granted (pkg_id TEXT, scope TEXT, granted_at INTEGER, PRIMARY KEY (pkg_id, scope))",
                "CREATE TABLE pkg_migrations (pkg_id TEXT, version TEXT, applied_at INTEGER, PRIMARY KEY (pkg_id, version))",
            ] {
                sqlx::query(ddl).execute(&pool).await.expect("create table");
            }

            // Two install rows (both at vanished paths → ManifestMissing); a child
            // setting for the one we'll purge; an orphan snapshot (no parent) plus a
            // snapshot that DOES have a parent (must survive).
            let ins = "INSERT INTO pkg_installed (id, version, ikenga_api, manifest_json, install_path, installed_at, enabled) VALUES (?,?,?,?,?,?,?)";
            sqlx::query(ins).bind("com.test.broken").bind("0.1.0").bind("1").bind("{}").bind("/nonexistent/broken").bind(0).bind(1).execute(&pool).await.unwrap();
            sqlx::query(ins).bind("com.test.keeper").bind("0.1.0").bind("1").bind("{}").bind("/nonexistent/keeper").bind(0).bind(1).execute(&pool).await.unwrap();
            sqlx::query("INSERT INTO pkg_settings (pkg_id, key, value_json, updated_at) VALUES (?,?,?,?)")
                .bind("com.test.broken").bind("k").bind("\"v\"").bind(0).execute(&pool).await.unwrap();
            let snap = "INSERT INTO pkg_capability_snapshots (pkg_id, manifest_capabilities_json, approved_at) VALUES (?,?,?)";
            sqlx::query(snap).bind("com.test.ghost").bind("{}").bind(0).execute(&pool).await.unwrap(); // orphan
            sqlx::query(snap).bind("com.test.keeper").bind("{}").bind(0).execute(&pool).await.unwrap(); // has parent

            // ── scan ──
            let issues = scan_health(&pool).await.expect("scan");
            assert!(
                issues.iter().any(|i| i.id == "com.test.broken" && i.issue == HealthIssueKind::ManifestMissing),
                "broken row should be detected as ManifestMissing; got {issues:?}"
            );
            assert!(
                issues.iter().any(|i| i.id == "com.test.ghost"
                    && i.issue == HealthIssueKind::OrphanRow { table: "pkg_capability_snapshots".to_string() }),
                "ghost snapshot should be detected as an OrphanRow; got {issues:?}"
            );
            assert!(
                !issues.iter().any(|i| i.id == "com.test.keeper" && matches!(i.issue, HealthIssueKind::OrphanRow { .. })),
                "keeper snapshot has a parent — must not be flagged orphan"
            );

            // ── purge the broken record (targeted) ──
            purge_record(&pool, "com.test.broken").await.expect("purge_record");
            let installed_ids: Vec<String> =
                sqlx::query_scalar("SELECT id FROM pkg_installed ORDER BY id").fetch_all(&pool).await.unwrap();
            assert_eq!(installed_ids, vec!["com.test.keeper".to_string()], "only keeper remains");
            let settings_left: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM pkg_settings").fetch_one(&pool).await.unwrap();
            assert_eq!(settings_left, 0, "broken's child setting cascaded away");

            // ── purge orphans ──
            let removed = purge_orphans(&pool).await.expect("purge_orphans");
            assert_eq!(removed, 1, "exactly the ghost snapshot removed");
            let snap_ids: Vec<String> =
                sqlx::query_scalar("SELECT pkg_id FROM pkg_capability_snapshots ORDER BY pkg_id").fetch_all(&pool).await.unwrap();
            assert_eq!(snap_ids, vec!["com.test.keeper".to_string()], "keeper snapshot survives; ghost gone");
        });
    }

    #[test]
    fn replay_unregisters_walks_reverse_order() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", false, events.clone()),
            mock("b", false, events.clone()),
            mock("c", false, events.clone()),
        ];
        replay_unregisters(&regs, "com.test.x");
        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "unregister:c:com.test.x".to_string(),
                "unregister:b:com.test.x".to_string(),
                "unregister:a:com.test.x".to_string(),
            ]
        );
    }

    #[test]
    fn replay_unregisters_continues_through_errors() {
        // A registry that fails unregister must not block the others. Per the
        // Registry trait contract, unregister is best-effort.
        struct FailingUnregister(&'static str, Arc<StdMutex<Vec<String>>>);
        impl Registry for FailingUnregister {
            fn name(&self) -> &'static str {
                self.0
            }
            fn register(&self, _pkg: &Package) -> Result<()> {
                Ok(())
            }
            fn unregister(&self, pkg_id: &str) -> Result<()> {
                self.1
                    .lock()
                    .unwrap()
                    .push(format!("unregister:{}:{pkg_id}", self.0));
                Err(anyhow!("nope"))
            }
            fn snapshot(&self) -> Value {
                Value::Null
            }
        }
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", false, events.clone()),
            Arc::new(FailingUnregister("b", events.clone())),
            mock("c", false, events.clone()),
        ];
        replay_unregisters(&regs, "com.test.x");
        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "unregister:c:com.test.x".to_string(),
                "unregister:b:com.test.x".to_string(),
                "unregister:a:com.test.x".to_string(),
            ]
        );
    }

    #[test]
    fn replay_registers_happy_path_calls_each_forward_no_unregister() {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", false, events.clone()),
            mock("b", false, events.clone()),
            mock("c", false, events.clone()),
        ];
        let applied = replay_registers(&regs, &fixture_pkg("com.test.x")).expect("happy path");
        assert_eq!(applied, vec!["a", "b", "c"]);
        // Only register calls; no unregisters fired since nothing failed.
        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "register:a:com.test.x".to_string(),
                "register:b:com.test.x".to_string(),
                "register:c:com.test.x".to_string(),
            ]
        );
    }

    #[test]
    fn replay_registers_rollback_on_middle_failure() {
        // Registry `b` fails register → `a` should be rolled back via
        // unregister, `c` should never be touched.
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", false, events.clone()),
            mock("b", true, events.clone()),
            mock("c", false, events.clone()),
        ];
        let err = replay_registers(&regs, &fixture_pkg("com.test.x"))
            .expect_err("expected register failure");
        assert!(err.to_string().contains("mock register failure in b"));

        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "register:a:com.test.x".to_string(),   // applied
                "register:b:com.test.x".to_string(),   // failed
                "unregister:a:com.test.x".to_string(), // rollback (no c register attempted)
            ]
        );
    }

    #[test]
    fn replay_registers_rollback_on_first_failure_does_not_unregister() {
        // First registry fails → nothing was applied, nothing to roll back.
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", true, events.clone()),
            mock("b", false, events.clone()),
        ];
        let _err = replay_registers(&regs, &fixture_pkg("com.test.x"))
            .expect_err("expected register failure");
        let log = events.lock().unwrap().clone();
        assert_eq!(log, vec!["register:a:com.test.x".to_string()]);
    }

    #[test]
    fn replay_registers_rollback_walks_applied_reverse() {
        // First two succeed, third fails → unregister fires for b then a, in
        // that order. Confirms the rollback walks applied in reverse.
        let events = Arc::new(StdMutex::new(Vec::new()));
        let regs: Vec<Arc<dyn Registry>> = vec![
            mock("a", false, events.clone()),
            mock("b", false, events.clone()),
            mock("c", true, events.clone()),
        ];
        let _err = replay_registers(&regs, &fixture_pkg("com.test.x"))
            .expect_err("expected register failure");
        let log = events.lock().unwrap().clone();
        assert_eq!(
            log,
            vec![
                "register:a:com.test.x".to_string(),
                "register:b:com.test.x".to_string(),
                "register:c:com.test.x".to_string(),
                "unregister:b:com.test.x".to_string(),
                "unregister:a:com.test.x".to_string(),
            ]
        );
    }
}
