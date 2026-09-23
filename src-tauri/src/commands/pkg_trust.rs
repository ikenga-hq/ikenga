//! Trust-review modal — Tauri command surface (2026-05-15).
//!
//! Distinct from `commands/trust.rs` (Phase 9 sensitive-perms gating at
//! MCP call-time). This module handles the boot-time capability-diff
//! review: lists pkgs whose declared `capabilities` + `permissions`
//! changed across an upgrade, and approves / rejects them.
//!
//! Approve → write new snapshot, `kernel.resume_after_review` runs the
//! registry replay so sidecars / MCPs come up.
//! Reject → delegate to `kernel.uninstall`.

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::commands::db::PaDb;
use crate::commands::pkg::KernelState;
use crate::pkg::cap_snapshot;
use crate::pkg::kernel::InstalledSummary;
use crate::pkg::manifest::Package;

#[derive(Debug, Clone, Serialize)]
pub struct TrustReview {
    pub pkg_id: String,
    pub manifest_version: String,
    /// Normalized JSON of the previously-approved capabilities +
    /// permissions blocks (`cap_snapshot::normalize` form).
    pub old_capabilities: String,
    /// Normalized JSON of the current on-disk manifest's capabilities +
    /// permissions blocks.
    pub new_capabilities: String,
    /// When the prior snapshot was approved (unix millis). The "diff
    /// detected" timestamp is implicit — it's whenever this command is
    /// called — so we expose the prior approval instead, which is more
    /// useful for the user ("you approved this on 2026-05-12").
    pub prior_approved_at_ms: i64,
}

/// List pkgs whose normalized capabilities + permissions differ from
/// their last-approved snapshot. Pkgs without a snapshot, or pkgs whose
/// snapshot matches, are omitted. Sorted by `pkg_id` for stable UI.
#[tauri::command]
pub async fn pkg_trust_list_pending(
    db: State<'_, Arc<PaDb>>,
    kernel: State<'_, KernelState>,
) -> Result<Vec<TrustReview>, String> {
    let pool = db.ensure_pool().await?;
    let installed = kernel.0.list_installed();
    pending_trust_reviews(&pool, &installed).await
}

/// The scan behind [`pkg_trust_list_pending`], factored so callers without
/// Tauri `State` (the `GET /iyke/ngwa/snapshot` route via
/// `commands::ngwa::ngwa_snapshot_inner`) run the identical logic.
pub(crate) async fn pending_trust_reviews(
    pool: &sqlx::SqlitePool,
    installed: &[InstalledSummary],
) -> Result<Vec<TrustReview>, String> {
    let mut out: Vec<TrustReview> = Vec::new();
    for s in installed {
        let pkg = match Package::load(Path::new(&s.install_path)) {
            Ok(p) => p,
            Err(e) => {
                log::warn!(
                    "[pkg_trust] list_pending: reload manifest for `{}` failed: {e:#} — skipping",
                    s.id
                );
                continue;
            }
        };
        let current_norm = cap_snapshot::normalize(&pkg.manifest);
        let snap = match cap_snapshot::fetch(&pool, &s.id).await {
            Ok(snap) => snap,
            Err(e) => {
                log::warn!(
                    "[pkg_trust] list_pending: fetch snapshot for `{}` failed: {e:#} — skipping",
                    s.id
                );
                continue;
            }
        };
        let Some(snap) = snap else {
            // No snapshot stored — first boot for this pkg likely hasn't
            // happened (or the implicit write failed). Either way, not
            // pending review yet; skip.
            continue;
        };
        if !cap_snapshot::capabilities_changed(&snap.manifest_capabilities_json, &current_norm) {
            continue;
        }
        out.push(TrustReview {
            pkg_id: s.id.clone(),
            manifest_version: pkg.manifest.version.clone(),
            old_capabilities: snap.manifest_capabilities_json,
            new_capabilities: current_norm,
            prior_approved_at_ms: snap.approved_at,
        });
    }
    out.sort_by(|a, b| a.pkg_id.cmp(&b.pkg_id));
    Ok(out)
}

/// Approve the current manifest's capability set: write a new explicit
/// snapshot and re-register the pkg with all kernel registries (which
/// starts its sidecars / MCPs).
#[tauri::command]
pub async fn pkg_trust_approve(
    db: State<'_, Arc<PaDb>>,
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Result<(), String> {
    let installed = kernel
        .0
        .installed_summary(&pkg_id)
        .ok_or_else(|| format!("pkg `{pkg_id}` not installed"))?;
    let pkg = Package::load(Path::new(&installed.install_path))
        .map_err(|e| format!("reload manifest: {e:#}"))?;
    let pool = db.ensure_pool().await?;
    let snapshot_json = cap_snapshot::normalize(&pkg.manifest);
    cap_snapshot::write_explicit(&pool, &pkg_id, &snapshot_json)
        .await
        .map_err(|e| format!("write snapshot: {e:#}"))?;
    kernel
        .0
        .resume_after_review(&pkg_id)
        .map_err(|e| format!("resume_after_review: {e:#}"))?;
    Ok(())
}

/// Reject the diff: uninstall the pkg. Delegates to the existing
/// uninstall path so registry teardown, DB cleanup, and FK cascades all
/// match the normal uninstall behavior.
#[tauri::command]
pub async fn pkg_trust_reject(
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Result<(), String> {
    kernel
        .0
        .uninstall(&pkg_id)
        .map_err(|e| format!("uninstall: {e:#}"))
}
