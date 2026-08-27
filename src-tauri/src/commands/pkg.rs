//! Tauri commands for the package kernel. The frontend uses these to install,
//! list, uninstall, and inspect packages; the same surface is exposed to
//! external tools via the iyke bridge once the iyke routes registry lands.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result as AnyResult};
use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use tauri::{AppHandle, Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::commands::claude_store::{resolve_pkg_requires, CatalogEntryRef, PkgRequiresResult};
use crate::pkg::manifest::Package;
use crate::pkg::registries::{ActivityBarBadge, ActivityBarRegistry, SettingsRegistry};
use crate::pkg::{
    DiscoveredPkg, InstallSource, InstalledSummary, Kernel, KernelStatus, PkgHealthIssue,
};

/// State wrapper so the kernel can be stored in Tauri state behind an Arc.
/// Derives Clone so the same wrapper can be layered as an axum Extension
/// in the iyke bridge (Phase 5 of projects-first-class — `iyke_mcp_list`).
#[derive(Clone)]
pub struct KernelState(pub Arc<Kernel>);

/// Standalone handle on the settings registry so `pkg_settings_*` commands
/// can read declared schemas without going through the kernel snapshot.
pub struct PkgSettingsState(pub Arc<SettingsRegistry>);

/// Standalone handle on the activity-bar registry (WP-11) so
/// `pkg_activity_bar_set_badge` — and the mirrored `/iyke/pkg/badge-set`
/// route — can mutate a pkg's rail badge without going through the kernel
/// snapshot. Same shape as `PkgSettingsState`.
#[derive(Clone)]
pub struct ActivityBarState(pub Arc<ActivityBarRegistry>);

#[derive(Serialize)]
pub struct PkgInstallResult {
    pub installed: InstalledSummary,
    /// WP-16: the outcome of resolving the pkg manifest's `requires` via the Ọba
    /// resolver (closure installed into the store + placed into the pkg's scope).
    /// `None` when the pkg declares no `requires` (or resolution was skipped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<PkgRequiresResult>,
}

/// Install a pkg from a local directory. `scope` (Phase 2 of
/// projects-first-class) is the wire-format scope picker:
/// `"workspace"` for always-loaded, `"project:<id>"` for project-bound,
/// or null to default to the active project. `catalog` (WP-16) is the FE's
/// primitive-catalog snapshot used to resolve each `requires` dependency's
/// `(source,url)`; omit it (or pass `[]`) and only already-present deps satisfy.
#[tauri::command]
pub async fn pkg_install_from_path(
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<crate::commands::db::PaDb>>,
    install_path: String,
    scope: Option<String>,
    catalog: Option<Vec<CatalogEntryRef>>,
) -> Result<PkgInstallResult, String> {
    let path = PathBuf::from(&install_path);
    // FE / iyke / dev-mode workspace installs are all `Local` provenance.
    // Registry installs go through a separate command once the registry
    // client lands; builtins go through `install_builtins()` at boot.
    let source = InstallSource::Local { path: install_path };
    let project_id = resolve_install_scope(db.inner().clone(), scope).await?;
    // The scope string `resolve_pkg_requires` places required primitives into.
    let pkg_scope = match &project_id {
        Some(id) => format!("project:{id}"),
        None => "workspace".to_string(),
    };
    let kernel_arc = kernel.0.clone();
    let path_for_kernel = path.clone();
    let installed = tokio::task::spawn_blocking(move || {
        kernel_arc.install_from_path(&path_for_kernel, source, project_id)
    })
    .await
    .map_err(|e| format!("install join: {e}"))?
    .map_err(|e| format!("{e:#}"))?;

    // WP-16 seam: satisfy the pkg manifest's `requires` via the Ọba resolver.
    // Reads the compiled `requires` off the installed manifest (Ọba never parses
    // SKILL.md), installs the missing closure into the store, and places it into
    // the pkg's scope through the unified `place_primitive` layer.
    let requires = Package::load(&path)
        .map(|p| p.manifest.requires)
        .unwrap_or_default();
    let requires = if requires.is_empty() {
        None
    } else {
        let cat = catalog.unwrap_or_default();
        match resolve_pkg_requires(db.inner(), &requires, &cat, &pkg_scope).await {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(
                    "[pkg/wp-16] requires resolution for `{}` failed (pkg installed, deps unresolved): {e}",
                    installed.id
                );
                None
            }
        }
    };

    Ok(PkgInstallResult { installed, requires })
}

#[tauri::command]
pub fn pkg_uninstall(kernel: State<'_, KernelState>, pkg_id: String) -> Result<(), String> {
    kernel.0.uninstall(&pkg_id).map_err(|e| format!("{e:#}"))
}

/// Update an installed pkg's scope. `scope` is the same wire format as
/// `pkg_install_from_path`: `"workspace"`, `"project:<id>"`, or null
/// (defaults to active project).
#[tauri::command]
pub fn pkg_set_scope(
    kernel: State<'_, KernelState>,
    pkg_id: String,
    scope: Option<String>,
    db: State<'_, Arc<crate::commands::db::PaDb>>,
) -> Result<(), String> {
    let db_arc = db.inner().clone();
    let project_id = tauri::async_runtime::block_on(resolve_install_scope(db_arc, scope))?;
    kernel
        .0
        .set_scope(&pkg_id, project_id)
        .map_err(|e| format!("{e:#}"))
}

/// Re-export of `resolve_install_scope` for the iyke bridge handler.
/// Same parser; named differently so the call site reads as "this is
/// the bridge path, not the Tauri-command path."
pub async fn resolve_install_scope_for_iyke(
    db: Arc<crate::commands::db::PaDb>,
    scope: Option<String>,
) -> Result<Option<String>, String> {
    resolve_install_scope(db, scope).await
}

/// Parse a wire scope ("workspace" | "project:<id>" | null) into the
/// Option<String> the kernel persists. Null defaults to the active
/// project. Returns Err if the slug is malformed.
async fn resolve_install_scope(
    db: Arc<crate::commands::db::PaDb>,
    scope: Option<String>,
) -> Result<Option<String>, String> {
    match scope.as_deref() {
        Some("workspace") => Ok(None),
        Some(s) if s.starts_with("project:") => {
            let slug = &s["project:".len()..];
            if slug.is_empty() {
                return Err("empty project slug".into());
            }
            Ok(Some(slug.to_string()))
        }
        Some(other) => Err(format!("invalid scope: {other}")),
        None => {
            let pool = db.ensure_pool().await.map_err(|e| e.to_string())?;
            let id = crate::commands::projects::get_active_project_id(&pool).await?;
            Ok(Some(id))
        }
    }
}

#[tauri::command]
pub fn pkg_set_enabled(
    kernel: State<'_, KernelState>,
    pkg_id: String,
    enabled: bool,
) -> Result<(), String> {
    kernel
        .0
        .set_enabled(&pkg_id, enabled)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn pkg_kernel_status(kernel: State<'_, KernelState>) -> KernelStatus {
    kernel.0.status()
}

/// Dev-mode helper: scan a workspace directory (e.g.
/// `~/royalti-co/ikenga/pkgs`) for sibling pkgs and return manifest metadata
/// without installing anything. The FE surfaces this in the Pkg Manager so
/// the user can install workspace pkgs with one click during local dev.
///
/// If `workspace_dir` is `None`, falls back to the `IKENGA_WORKSPACE_DIR`
/// env var. Returns an empty list when neither is set or the path is missing
/// — never fails.
#[tauri::command]
pub fn pkg_discover_workspace(
    kernel: State<'_, KernelState>,
    workspace_dir: Option<String>,
) -> Vec<DiscoveredPkg> {
    let path = workspace_dir
        .or_else(|| std::env::var("IKENGA_WORKSPACE_DIR").ok())
        .map(PathBuf::from);
    match path {
        Some(p) => kernel.0.discover_workspace(&p),
        None => Vec::new(),
    }
}

/// Read a manifest at the given install path WITHOUT registering it. Used
/// by the Install panel to preview what a package declares before the user
/// commits to running the install prompt with Claude. Returns the parsed
/// manifest as JSON so the FE can render permissions, settings schema, and
/// any other declared blocks generically.
#[tauri::command]
pub fn pkg_preview_manifest(install_path: String) -> Result<serde_json::Value, String> {
    let path = PathBuf::from(install_path);
    let pkg = crate::pkg::manifest::Package::load(&path).map_err(|e| format!("{e:#}"))?;
    serde_json::to_value(&pkg.manifest).map_err(|e| format!("serialize manifest: {e}"))
}

/// Read a pkg-declared screenshot and return it as a base64 data URL the
/// webview can stuff into `<img src>`. The path must match one declared in
/// `manifest.screenshots[].path`; we resolve it against `install_path` and
/// canonicalize to defend against `../` escapes.
///
/// We go through a Tauri command (rather than enabling the asset protocol)
/// because screenshots are bounded (a handful per pkg, ~50KB each), the
/// browser caches the data URL after first render, and routing through the
/// kernel lets us enforce "must be declared in manifest" instead of
/// blanket-trusting any path under the pkgs dir.
#[tauri::command]
pub fn pkg_screenshot(
    kernel: State<'_, KernelState>,
    pkg_id: String,
    path: String,
) -> Result<String, String> {
    use base64::engine::general_purpose::STANDARD as B64;

    let installed = kernel.0.status().installed;
    let summary = installed
        .iter()
        .find(|p| p.id == pkg_id)
        .ok_or_else(|| format!("pkg {pkg_id} not installed"))?;

    let install_path = PathBuf::from(&summary.install_path);
    let pkg = crate::pkg::manifest::Package::load(&install_path)
        .map_err(|e| format!("load manifest: {e:#}"))?;

    let declared = pkg.manifest.screenshots.iter().any(|s| s.path == path);
    if !declared {
        return Err(format!("screenshot {path:?} not declared in manifest"));
    }

    let full = install_path.join(&path);
    let canon_full = full
        .canonicalize()
        .map_err(|e| format!("canonicalize screenshot: {e}"))?;
    let canon_root = install_path
        .canonicalize()
        .map_err(|e| format!("canonicalize install_path: {e}"))?;
    if !canon_full.starts_with(&canon_root) {
        return Err("screenshot path escapes install_path".into());
    }

    let bytes = std::fs::read(&canon_full).map_err(|e| format!("read screenshot: {e}"))?;
    let mime = match path.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()) {
        Some(ref e) if e == "png" => "image/png",
        Some(ref e) if e == "jpg" || e == "jpeg" => "image/jpeg",
        Some(ref e) if e == "webp" => "image/webp",
        Some(ref e) if e == "gif" => "image/gif",
        _ => "application/octet-stream",
    };
    Ok(format!("data:{mime};base64,{}", B64.encode(&bytes)))
}

/// Diagnostic: returns `(db_path, pkg_installed_count)` straight from the
/// kernel's PaDb handle. Used to confirm the kernel is reading the same
/// SQLite file as external tooling expects.
#[derive(serde::Serialize)]
pub struct PkgDbDiag {
    pub db_path: String,
    pub pkg_installed_count: i64,
    pub ids: Vec<String>,
}

#[tauri::command]
pub fn pkg_db_diag(
    db: tauri::State<'_, std::sync::Arc<crate::commands::db::PaDb>>,
) -> Result<PkgDbDiag, String> {
    let db_path = db.db_path_for_diag().display().to_string();
    let db_clone = db.inner().clone();
    let (count, ids): (i64, Vec<String>) = tauri::async_runtime::block_on(async move {
        let pool = db_clone.ensure_pool().await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pkg_installed")
            .fetch_one(&pool)
            .await
            .map_err(|e| e.to_string())?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM pkg_installed ORDER BY id")
            .fetch_all(&pool)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((count, ids))
    })?;
    Ok(PkgDbDiag {
        db_path,
        pkg_installed_count: count,
        ids,
    })
}

// ─── pkg health (install-integrity check + cleanup) ──────────────────────────
//
// Surfaces broken/orphaned `pkg_installed` records the kernel skips at boot
// (missing/unreadable/unparseable manifest, api-incompatible) plus orphaned
// child `pkg_*` rows, and offers one-click removal. The kernel is the only
// writer of `pkg_installed`, so these route straight through it.

/// Scan for broken / orphaned install records. Read-only.
#[tauri::command]
pub fn pkg_health_scan(kernel: State<'_, KernelState>) -> Result<Vec<PkgHealthIssue>, String> {
    kernel.0.health_scan().map_err(|e| format!("{e:#}"))
}

/// Remove one broken install record (its `pkg_installed` row + child rows).
#[tauri::command]
pub fn pkg_health_remove(
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Result<(), String> {
    kernel
        .0
        .purge_install_record(&pkg_id)
        .map_err(|e| format!("{e:#}"))
}

#[derive(Serialize)]
pub struct PkgHealthRemoveAllResult {
    pub removed_records: usize,
    pub removed_orphans: u64,
}

/// Remove every currently-detected broken record + orphan row.
#[tauri::command]
pub fn pkg_health_remove_all(
    kernel: State<'_, KernelState>,
) -> Result<PkgHealthRemoveAllResult, String> {
    let (removed_records, removed_orphans) =
        kernel.0.purge_all_broken().map_err(|e| format!("{e:#}"))?;
    Ok(PkgHealthRemoveAllResult {
        removed_records,
        removed_orphans,
    })
}

// ─── pkg_settings ────────────────────────────────────────────────────────────
//
// Per-package key/value settings backed by the `pkg_settings` table. Schema
// (used for default-fallback and the future Settings UI) is declared in the
// manifest under `settings.schema`. Get falls back to the schema default if
// no row exists yet — handy for first-launch reads before defaults have
// been seeded by `register()` (rare, but cheap insurance).

#[derive(serde::Serialize)]
pub struct PkgSettingsSnapshot {
    pub pkg_id: String,
    pub schema: serde_json::Value,
    pub values: serde_json::Value,
}

#[tauri::command]
pub fn pkg_settings_get(
    settings: State<'_, PkgSettingsState>,
    db: State<'_, Arc<crate::commands::db::PaDb>>,
    pkg_id: String,
) -> Result<PkgSettingsSnapshot, String> {
    let schema = settings.0.schema_for(&pkg_id);
    let db_clone = db.inner().clone();
    let pkg_for_query = pkg_id.clone();
    let stored: serde_json::Map<String, serde_json::Value> =
        tauri::async_runtime::block_on(async move {
            let pool = db_clone.ensure_pool().await?;
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT key, value_json FROM pkg_settings WHERE pkg_id = ?")
                    .bind(&pkg_for_query)
                    .fetch_all(&pool)
                    .await
                    .map_err(|e| format!("read pkg_settings: {e}"))?;
            let mut obj = serde_json::Map::new();
            for (k, vj) in rows {
                let v: serde_json::Value =
                    serde_json::from_str(&vj).unwrap_or(serde_json::Value::String(vj));
                obj.insert(k, v);
            }
            Ok::<_, String>(obj)
        })?;

    // Merge: schema defaults provide the baseline, stored rows override.
    // Lets `pkg_settings_get` return a complete shape from first-launch even
    // before the user has set anything (registry doesn't pre-seed because the
    // pkg_settings.pkg_id FK isn't satisfied until kernel persists install).
    let mut merged = serde_json::Map::new();
    if let Some(fields) = &schema {
        for f in fields {
            merged.insert(f.key.clone(), f.default.clone());
        }
    }
    for (k, v) in stored {
        merged.insert(k, v);
    }

    Ok(PkgSettingsSnapshot {
        pkg_id,
        schema: serde_json::to_value(schema).unwrap_or(serde_json::Value::Null),
        values: serde_json::Value::Object(merged),
    })
}

// ─── pkg_install_from_registry ───────────────────────────────────────────────
//
// Single-pkg installer. The TS registry-client owns signature verification of
// the registry index + dep resolution; by the time we land here the caller has
// already vetted that this `tarball` URL is the one named in the signed
// index, that `integrity` is the SRI digest npm shipped, and that `pkg_id`
// matches the manifest declared at this version.
//
// We still re-verify the tarball SHA-512 against `integrity` after download —
// that's the second leg of the trust chain (signed index → integrity → bytes).

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PkgInstallFromRegistryArgs {
    /// Direct tarball URL from `PkgVersion.tarball` (npm).
    pub tarball: String,
    /// `sha512-<base64>` SRI integrity from `PkgVersion.integrity` (npm).
    pub integrity: String,
    /// Manifest id, used as the install dir name and cross-checked against
    /// the manifest.json inside the tarball.
    pub pkg_id: String,
    /// Recorded as `InstallSource::Registry.url`. Typically equals `tarball`;
    /// passed separately so the FE can choose a more meaningful "origin" URL
    /// later (e.g. the index URL).
    pub source_url: String,
    /// The publisher's minisign public key, as named by the signed registry
    /// index for this pkg. Threaded straight into
    /// `InstallSource::Registry.publisher_key` and persisted on
    /// `pkg_installed.source_json`. The trust gate (`pkg::signature` +
    /// `pkg::trust`) verifies the manifest's `signature` against this key at
    /// install and on every boot replay; absent ⇒ the pkg installs and runs
    /// but is never trusted for elevated capabilities. The signed index does
    /// not carry per-pkg publisher keys yet (WP-06), so this is `None` today —
    /// the field is wired now so no shape change is needed when keys land.
    #[serde(default)]
    pub publisher_key: Option<String>,
}

/// Install a pkg from a registry. `scope` follows the same wire format as
/// `pkg_install_from_path`.
#[tauri::command]
pub async fn pkg_install_from_registry(
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<crate::commands::db::PaDb>>,
    args: PkgInstallFromRegistryArgs,
    scope: Option<String>,
) -> Result<PkgInstallResult, String> {
    let project_id = resolve_install_scope(db.inner().clone(), scope).await?;
    install_from_registry_inner(kernel.0.clone(), args, project_id)
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn install_from_registry_inner(
    kernel: Arc<Kernel>,
    args: PkgInstallFromRegistryArgs,
    project_id: Option<String>,
) -> AnyResult<PkgInstallResult> {
    let pkgs_dir = kernel.pkgs_dir()?;
    tokio::fs::create_dir_all(&pkgs_dir)
        .await
        .with_context(|| format!("create pkgs dir {}", pkgs_dir.display()))?;

    // Stage path is a sibling of the final install dir. Both live under
    // pkgs_dir, so a successful untar + atomic rename never crosses
    // filesystems.
    let final_dir = pkgs_dir.join(&args.pkg_id);
    let staging_dir = pkgs_dir.join(format!(".staging-{}", args.pkg_id));
    let backup_dir = pkgs_dir.join(format!(".bak-{}", args.pkg_id));

    // Clean up leftover staging/backup from a prior aborted install. We never
    // resume a partial install — start fresh every time.
    if staging_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
    }
    if backup_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&backup_dir).await;
    }

    // 1. Download tarball + verify SHA-512 against the SRI integrity.
    let tarball_path = staging_dir.with_extension("tgz");
    if let Some(parent) = tarball_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    download_and_verify(&args.tarball, &args.integrity, &tarball_path)
        .await
        .with_context(|| format!("download {}", args.tarball))?;

    // 2. Extract into staging. Strip leading `package/` (npm convention).
    //    Reject any entry whose normalized path escapes the staging root.
    let tarball_path_for_blocking = tarball_path.clone();
    let staging_dir_for_blocking = staging_dir.clone();
    tokio::task::spawn_blocking(move || -> AnyResult<()> {
        extract_tarball(&tarball_path_for_blocking, &staging_dir_for_blocking)
    })
    .await
    .map_err(|e| anyhow!("untar task join: {e}"))??;

    // 3. Cross-check the unpacked manifest's id against the requested pkg_id.
    //    Catches: typo in pkg_id, tarball/manifest mismatch from a bad publish.
    let manifest_path = staging_dir.join("manifest.json");
    let manifest_bytes = tokio::fs::read(&manifest_path)
        .await
        .with_context(|| format!("read manifest.json from {}", manifest_path.display()))?;
    let manifest_json: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse {}", manifest_path.display()))?;
    let manifest_id = manifest_json
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("manifest.json missing `id` field"))?;
    if manifest_id != args.pkg_id {
        // Clean up staging before erroring.
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        let _ = tokio::fs::remove_file(&tarball_path).await;
        return Err(anyhow!(
            "manifest id mismatch: tarball declares `{manifest_id}`, registry said `{}`",
            args.pkg_id
        ));
    }

    // 4. Atomic-ish swap: backup existing → move staging → final.
    //    Reverse on failure so the previous install isn't lost.
    if final_dir.exists() {
        tokio::fs::rename(&final_dir, &backup_dir)
            .await
            .with_context(|| {
                format!(
                    "backup existing install: {} → {}",
                    final_dir.display(),
                    backup_dir.display()
                )
            })?;
    }
    if let Err(e) = tokio::fs::rename(&staging_dir, &final_dir).await {
        // Rollback: put the backup back.
        if backup_dir.exists() {
            let _ = tokio::fs::rename(&backup_dir, &final_dir).await;
        }
        return Err(anyhow!(
            "promote staging dir to install: {} → {}: {}",
            staging_dir.display(),
            final_dir.display(),
            e
        ));
    }

    // 5. Register with the kernel. If the kernel rejects (e.g. ikenga_api
    //    incompatible, registry conflict), we already have a backup to
    //    restore; the kernel's own rollback handles the DB side.
    let final_dir_for_kernel = final_dir.clone();
    let source = InstallSource::Registry {
        url: args.source_url,
        publisher_key: args.publisher_key,
    };
    let installed = tokio::task::spawn_blocking(move || {
        kernel.install_from_path(&final_dir_for_kernel, source, project_id)
    })
    .await
    .map_err(|e| anyhow!("kernel install task join: {e}"))?;

    match installed {
        Ok(summary) => {
            // Success — drop the backup + downloaded tarball.
            let _ = tokio::fs::remove_dir_all(&backup_dir).await;
            let _ = tokio::fs::remove_file(&tarball_path).await;
            // WP-16 requires-resolution for the registry path is a follow-up
            // (the registry install flow has no catalog snapshot threaded yet).
            Ok(PkgInstallResult {
                installed: summary,
                requires: None,
            })
        }
        Err(e) => {
            // Roll the filesystem back: remove the new dir, restore the backup.
            let _ = tokio::fs::remove_dir_all(&final_dir).await;
            if backup_dir.exists() {
                let _ = tokio::fs::rename(&backup_dir, &final_dir).await;
            }
            let _ = tokio::fs::remove_file(&tarball_path).await;
            Err(anyhow!("{e:#}"))
        }
    }
}

/// Stream the tarball to disk while hashing in parallel. Constant-time-compare
/// the final digest against the SRI integrity. Removes the partial file on
/// any failure.
async fn download_and_verify(url: &str, integrity_sri: &str, dest: &Path) -> AnyResult<()> {
    let expected = parse_sri_sha512(integrity_sri)?;

    let res = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {url}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    let mut hasher = Sha512::new();
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(e) => {
                let _ = tokio::fs::remove_file(dest).await;
                return Err(anyhow!("tarball stream read: {e}"));
            }
        };
        hasher.update(&bytes);
        if let Err(e) = file.write_all(&bytes).await {
            let _ = tokio::fs::remove_file(dest).await;
            return Err(anyhow!("write tarball chunk: {e}"));
        }
    }
    file.flush().await.ok();
    drop(file);

    let actual: [u8; 64] = hasher.finalize().into();
    if !constant_time_eq(&actual, &expected) {
        let _ = tokio::fs::remove_file(dest).await;
        return Err(anyhow!(
            "tarball SHA-512 integrity mismatch — refusing to install"
        ));
    }
    Ok(())
}

/// Parse a `sha512-<base64>` SRI integrity string into a 64-byte digest.
fn parse_sri_sha512(s: &str) -> AnyResult<[u8; 64]> {
    let rest = s
        .strip_prefix("sha512-")
        .ok_or_else(|| anyhow!("integrity must start with `sha512-`: got {s}"))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(rest)
        .with_context(|| format!("base64-decode integrity digest: {rest}"))?;
    if decoded.len() != 64 {
        return Err(anyhow!(
            "integrity digest must be 64 bytes, got {}",
            decoded.len()
        ));
    }
    let mut out = [0u8; 64];
    out.copy_from_slice(&decoded);
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Untar a gzipped npm tarball into `dest_dir`. Strips the leading `package/`
/// component (npm convention). Rejects entries whose normalized path escapes
/// `dest_dir` (defends against `..` traversal and absolute paths).
fn extract_tarball(src: &Path, dest_dir: &Path) -> AnyResult<()> {
    std::fs::create_dir_all(dest_dir).with_context(|| format!("mkdir {}", dest_dir.display()))?;
    let file =
        std::fs::File::open(src).with_context(|| format!("open tarball {}", src.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_mtime(false);
    archive.set_overwrite(true);

    for entry in archive
        .entries()
        .with_context(|| format!("read entries in {}", src.display()))?
    {
        let mut entry = entry.with_context(|| "read tar entry")?;
        let entry_path = entry
            .path()
            .with_context(|| "decode tar entry path")?
            .into_owned();

        // Strip the `package/` prefix that npm pack always adds. Skip entries
        // outside that prefix (npm puts `package.json` etc. inside it; any
        // sibling top-level entry would be malformed).
        let stripped = match entry_path.strip_prefix("package") {
            Ok(p) => p,
            Err(_) => continue,
        };
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Reject `..` components and absolute paths. `Path::components()`
        // surfaces `ParentDir` explicitly; we walk and refuse if any appears.
        for comp in stripped.components() {
            use std::path::Component;
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(anyhow!(
                        "tarball entry escapes install root: {}",
                        entry_path.display()
                    ));
                }
            }
        }

        let target = dest_dir.join(stripped);
        // npm tarballs don't reliably include directory entries before the
        // files inside them, and `Entry::unpack` doesn't always mkdir parents
        // on Linux. Create them explicitly before unpacking.
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir -p parent of {}", target.display()))?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("unpack entry to {}", target.display()))?;
    }
    Ok(())
}

#[tauri::command]
pub fn pkg_settings_set(
    db: State<'_, Arc<crate::commands::db::PaDb>>,
    pkg_id: String,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let db_clone = db.inner().clone();
    let value_json = serde_json::to_string(&value).map_err(|e| format!("serialize value: {e}"))?;
    let now = chrono::Utc::now().timestamp_millis();
    tauri::async_runtime::block_on(async move {
        let pool = db_clone.ensure_pool().await?;
        sqlx::query(
            "INSERT INTO pkg_settings (pkg_id, key, value_json, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(pkg_id, key) DO UPDATE SET
               value_json = excluded.value_json,
               updated_at = excluded.updated_at",
        )
        .bind(&pkg_id)
        .bind(&key)
        .bind(&value_json)
        .bind(now)
        .execute(&pool)
        .await
        .map_err(|e| format!("upsert pkg_settings: {e}"))?;
        Ok::<_, String>(())
    })?;
    Ok(())
}

// ─── activity-bar badge (WP-11) ───────────────────────────────────────────────
//
// A pkg's own status dot/count on its activity-bar rail icon — e.g. the git
// pkg pushing dirty/ahead-behind. Set via the `host.pkg.setBadge` AppBridge
// verb (in-shell iframe path) or the `/iyke/pkg/badge-set` route (external /
// smoke-test path); both funnel into this one command. `None` clears the
// badge. Emits `pkg-badge-changed` so `usePkgActivityBarEntries` (which
// otherwise only refetches on install/uninstall/reload) picks it up without
// a poll.
#[tauri::command]
pub fn pkg_activity_bar_set_badge(
    app: AppHandle,
    activity_bar: State<'_, ActivityBarState>,
    pkg_id: String,
    badge: Option<ActivityBarBadge>,
) -> Result<(), String> {
    let found = activity_bar
        .0
        .set_badge(&pkg_id, badge.clone())
        .map_err(|e| format!("{e:#}"))?;
    if !found {
        return Err(format!("no activity-bar entry for pkg `{pkg_id}`"));
    }
    let _ = app.emit(
        "pkg-badge-changed",
        serde_json::json!({ "pkg_id": pkg_id, "badge": badge }),
    );
    Ok(())
}

// ─── skill actions (WP-13: dispatch-only lighthouse) ─────────────────────────
//
// A `kind: skill` pkg contributes actions under
// `<install_path>/<skills_dir>/<skill>/actions/*.md`. We resolve the install
// path from the kernel snapshot, load the manifest to read its `skills` dir,
// and parse each action file's YAML frontmatter. See `pkg::skill_actions`.

/// Resolve an installed pkg's on-disk root + skills dir from the kernel
/// snapshot, then discover its skill actions. Returns `[]` for unknown ids,
/// non-skill pkgs, or pkgs without a `skills` dir (never errors — a missing
/// skills dir is a normal "this pkg has no actions" case).
#[tauri::command]
pub fn list_skill_actions(
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Vec<crate::pkg::skill_actions::SkillAction> {
    let installed = kernel.0.status().installed;
    let Some(summary) = installed.iter().find(|p| p.id == pkg_id) else {
        tracing::warn!(%pkg_id, "list_skill_actions: pkg not installed");
        return Vec::new();
    };
    let install_path = PathBuf::from(&summary.install_path);
    // WP-17: pkgs no longer bundle skills — actions are discovered by following
    // the manifest's `requires` to the standalone skill primitives in the Ọba
    // store. Load the manifest to read `requires` (cheap — one small JSON).
    let pkg = match crate::pkg::manifest::Package::load(&install_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%pkg_id, error = %format!("{e:#}"), "list_skill_actions: manifest load failed");
            return Vec::new();
        }
    };
    let Some(store) = crate::commands::claude_config::store_root() else {
        return Vec::new();
    };
    crate::pkg::skill_actions::list_actions_for_pkg(&pkg_id, &pkg.manifest.requires, &store)
}

/// List skill actions across every installed pkg. Pkgs that fail to load or
/// require no skills simply contribute nothing.
#[tauri::command]
pub fn list_all_skill_actions(
    kernel: State<'_, KernelState>,
) -> Vec<crate::pkg::skill_actions::SkillAction> {
    let installed = kernel.0.status().installed;
    let Some(store) = crate::commands::claude_config::store_root() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for summary in &installed {
        let install_path = PathBuf::from(&summary.install_path);
        let Ok(pkg) = crate::pkg::manifest::Package::load(&install_path) else {
            continue;
        };
        out.extend(crate::pkg::skill_actions::list_actions_for_pkg(
            &summary.id,
            &pkg.manifest.requires,
            &store,
        ));
    }
    out
}

/// The single FE-facing gate for **elevated** host capabilities (ADR-017 /
/// trusted-pkg tier). Resolves the pkg's trust state the same way the
/// secret-injection path does (`Package::load` off disk → `trust::evaluate`
/// → `TrustState::is_trusted_for_elevated()`) and returns the boolean.
///
/// Wave-2 elevated verbs (`host.fetch` / `host.invoke`, WP-04/05) call this
/// FE-side in `dispatchHostCall` as `pkgDeclaresCapability(pkgId, '<cap>') &&
/// pkgIsTrustedForElevated(pkgId)`, then the matching Rust command **re-checks**
/// the same gate server-side (the FE check is fail-fast UX only — a hostile
/// iframe could skip it). Named-secret injection (WP-03) gates entirely inside
/// `pkg_content_html` and doesn't need this command, but it shares the exact
/// `trust::resolve_elevated_trust` helper so all three verbs agree.
///
/// Fail-closed: un-installed / un-loadable / un-evaluable → `false`.
#[tauri::command]
pub async fn pkg_is_trusted_for_elevated(
    app: tauri::AppHandle,
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<crate::commands::db::PaDb>>,
    pkg_id: String,
) -> Result<bool, String> {
    use tauri::Manager;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    let pool = db.ensure_pool().await.map_err(|e| e.to_string())?;
    Ok(crate::pkg::trust::resolve_elevated_trust(&pool, &kernel.0, &app_data, &pkg_id).await)
}
