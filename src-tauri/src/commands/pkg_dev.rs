//! Dev-mode Tauri commands.
//!
//! These three commands are the kernel-side surface for `ikenga dev
//! <path>`: register a pkg with the running shell, drop it cleanly on
//! Ctrl-C, and trigger an explicit reload. The `manifest.json` file
//! watcher is wired automatically on register — manifest edits trip
//! reload without the CLI doing anything.
//!
//! These commands deliberately go through the same `install_from_path`
//! path as `pkg_install_from_path`, but with `InstallSource::Dev` so the
//! trust gate auto-trusts the pkg regardless of id namespace (see
//! `pkg/trust.rs::is_auto_trusted`). `Dev` sources do not persist as
//! `Dev` across reboots — the boot replay reconstructs them as plain
//! `Local`, so this can't accidentally smuggle elevated trust into a
//! production install.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use crate::pkg::{InstallSource, InstalledSummary};

use super::pkg::KernelState;

/// Install a pkg from a local directory in dev mode. Stamps
/// `source.kind = "dev"` (instead of the `Local` provenance the regular
/// `pkg_install_from_path` command uses), spawns a `manifest.json` +
/// `restart_when_changed` watcher, and auto-trusts via the dev bypass
/// in `pkg/trust.rs`.
///
/// `scope` is currently ignored (dev pkgs are always workspace-scoped —
/// they'd be a footgun to bind to a single project given the loose trust
/// gate). Reserved for symmetry with `pkg_install_from_path` should this
/// change later.
#[tauri::command]
pub async fn pkg_dev_register(
    kernel: State<'_, KernelState>,
    install_path: String,
    _scope: Option<String>,
) -> Result<InstalledSummary, String> {
    let path = PathBuf::from(&install_path);
    let source = InstallSource::Dev { path: install_path };
    let kernel_arc = kernel.0.clone();

    // Install on a blocking thread for the same reason `pkg_install_from_path`
    // does — registries call `tauri::async_runtime::block_on` internally and
    // re-entering the runtime panics.
    let kernel_for_install = Arc::clone(&kernel_arc);
    let summary = tokio::task::spawn_blocking(move || {
        crate::pkg::materialize_npm_deps(&path)?;
        kernel_for_install.install_from_path(&path, source, None)
    })
    .await
    .map_err(|e| format!("install join: {e}"))?
    .map_err(|e| format!("{e:#}"))?;

    // Pull the freshly-installed manifest off disk to collect every
    // `restart_when_changed` glob declared by `mcp[]` + `sidecars[]`. The
    // watcher needs the full pattern set so we don't fire reload only on
    // manifest.json edits. Use `summary.install_path` so the watcher
    // anchors on whatever the kernel canonicalized.
    let path_for_watcher = PathBuf::from(&summary.install_path);
    let extra_globs = collect_restart_globs(&path_for_watcher).unwrap_or_default();
    kernel_arc
        .spawn_dev_watcher(&summary.id, &path_for_watcher, extra_globs)
        .map_err(|e| format!("spawn dev watcher: {e:#}"))?;

    Ok(summary)
}

/// Drop a dev pkg cleanly. Mirrors `pkg_uninstall` but also tears down
/// the manifest watcher. Refuses to act on pkgs whose source isn't
/// `Dev` — callers should use `pkg_uninstall` for those.
#[tauri::command]
pub async fn pkg_dev_unregister(
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Result<(), String> {
    let kernel_arc = kernel.0.clone();
    let id_for_check = pkg_id.clone();
    let is_dev = kernel_arc
        .installed_summary(&id_for_check)
        .map(|s| s.source.is_dev())
        .unwrap_or(false);
    if !is_dev {
        return Err(format!(
            "pkg `{pkg_id}` is not a dev install — use pkg_uninstall instead"
        ));
    }
    kernel_arc.drop_dev_watcher(&pkg_id);
    let kernel_for_uninstall = Arc::clone(&kernel_arc);
    let id_for_uninstall = pkg_id.clone();
    tokio::task::spawn_blocking(move || kernel_for_uninstall.uninstall(&id_for_uninstall))
        .await
        .map_err(|e| format!("uninstall join: {e}"))?
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

/// Explicit reload, bypassing the file watcher. The watcher debounces 250ms
/// — when an external caller (the CLI, a test, an MCP tool) wants the
/// reload to happen *right now*, this is the surface.
#[tauri::command]
pub async fn pkg_dev_reload(
    kernel: State<'_, KernelState>,
    pkg_id: String,
) -> Result<InstalledSummary, String> {
    let kernel_arc = kernel.0.clone();
    tokio::task::spawn_blocking(move || kernel_arc.reload_pkg(&pkg_id))
        .await
        .map_err(|e| format!("reload join: {e}"))?
        .map_err(|e| format!("{e:#}"))
}

/// Read every `restart_when_changed` glob declared in the manifest's
/// `mcp[]` + `sidecars[]` blocks, returning them as a flat de-duplicated
/// list. Returns `None` on any parse failure — the caller treats this as
/// "no extra globs" and the watcher only fires on `manifest.json` edits
/// (which is still useful for the dev loop).
///
/// `pub(crate)` so the iyke bridge handler can reuse the exact same
/// extraction without re-implementing the manifest walk.
pub(crate) fn collect_restart_globs(install_path: &std::path::Path) -> Option<Vec<String>> {
    let pkg = crate::pkg::manifest::Package::load(install_path).ok()?;
    let mut out: Vec<String> = Vec::new();
    for m in &pkg.manifest.mcp {
        out.extend(m.restart_when_changed.iter().cloned());
    }
    for s in &pkg.manifest.sidecars {
        out.extend(s.restart_when_changed.iter().cloned());
    }
    out.sort();
    out.dedup();
    Some(out)
}
