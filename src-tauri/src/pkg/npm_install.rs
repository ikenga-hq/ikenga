//! Materialize npm dependencies for an unpacked package directory.
//!
//! Mirrors the `ikenga` CLI's `materializeNpmDeps` so long-lived MCP packages
//! that ship a `package.json` with `dependencies` are ready to run before the
//! sidecar supervisor first spawns them. Without this, MCP servers that
//! `require()` a dep exit with `ERR_MODULE_NOT_FOUND` on first boot and the
//! supervisor parks them (ikenga#150).
//!
//! Only packages that declare at least one `mcp[].lifecycle = "long-lived"`
//! are processed — per-call MCPs and pure UI pkgs don't need a `node_modules`.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::pkg::manifest::Manifest;
use crate::runtime::augmented_path;

/// Minimal `package.json` shape — we only need to read `dependencies`.
#[derive(Debug, Deserialize)]
struct PackageJson {
    dependencies: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Run `npm install --omit=dev` (or `bun install --production` as fallback) in
/// `install_path` if the manifest declares any long-lived MCP servers and a
/// `package.json` with non-empty `dependencies` is present.
///
/// `install_path` is the final pkg directory on disk. The function is
/// synchronous so it can run inside `tokio::task::spawn_blocking` without
/// holding an async runtime worker for the duration.
pub fn materialize_npm_deps(install_path: &Path) -> Result<()> {
    // Gate: only packages with long-lived MCP servers pay the npm cost. Pure
    // UI pkgs, per-call MCPs, and sidecar-only pkgs don't need node_modules.
    let manifest_path = install_path.join("manifest.json");
    if !manifest_path.exists() {
        return Ok(());
    }
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).context("read manifest.json")?
    ).context("parse manifest.json")?;
    if !manifest.mcp.iter().any(|s| s.is_long_lived()) {
        return Ok(());
    }

    let package_json_path = install_path.join("package.json");
    if !package_json_path.exists() {
        return Ok(());
    }
    let package_json: PackageJson = serde_json::from_slice(
        &std::fs::read(&package_json_path).context("read package.json")?
    ).context("parse package.json")?;
    let deps = package_json.dependencies.as_ref().map(|m| m.len()).unwrap_or(0);
    if deps == 0 {
        return Ok(());
    }

    // Resolve `npm` against the same augmented PATH used for agent CLIs — the
    // GUI process on some desktops (Linux .desktop, macOS .app) does not
    // inherit the user's shell PATH, so npm may be installed but invisible.
    let search_path = augmented_path();
    let npm = which::which_in("npm", Some(search_path), install_path)
        .or_else(|_| which::which_in("npm", Some(search_path), std::env::current_dir().unwrap_or_default()))
        .map_err(|e| anyhow!("npm not found on augmented PATH: {e}"))?;

    log::info!(
        "materializing {deps} npm dependenc(y/ies) for {} with npm {}",
        install_path.display(),
        npm.display()
    );

    let output = Command::new(&npm)
        .args(["install", "--omit=dev", "--no-audit", "--no-fund"])
        .current_dir(install_path)
        .env("PATH", search_path)
        .output()
        .context("spawn npm install")?;

    if output.status.success() {
        log::info!("npm install succeeded in {}", install_path.display());
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    log::warn!(
        "npm install failed in {} (exit {}): {stderr}",
        install_path.display(),
        output.status
    );

    // Fallback: try bun install --production if bun is on the augmented PATH.
    if let Ok(bun) = which::which_in("bun", Some(search_path), install_path)
        .or_else(|_| which::which_in("bun", Some(search_path), std::env::current_dir().unwrap_or_default()))
    {
        log::info!("falling back to bun install for {}", install_path.display());
        let output = Command::new(&bun)
            .args(["install", "--production"])
            .current_dir(install_path)
            .env("PATH", search_path)
            .output()
            .context("spawn bun install")?;

        if output.status.success() {
            log::info!("bun install succeeded in {}", install_path.display());
            return Ok(());
        }

        let bun_err = String::from_utf8_lossy(&output.stderr);
        log::warn!(
            "bun install failed in {} (exit {}): {bun_err}",
            install_path.display(),
            output.status
        );
    }

    Err(anyhow!("npm dependency materialization failed: {stderr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_packages_without_long_lived_mcp() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("manifest.json"), r#"{"id":"test","name":"Test","version":"1.0.0","ikenga_api":"1","mcp":[{"name":"x","command":"node","args":["index.js"],"lifecycle":"per-call"}]}"#).unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"dependencies":{"left-pad":"1.0.0"}}"#).unwrap();

        // Should return Ok without doing anything (npm is not run).
        assert!(materialize_npm_deps(dir.path()).is_ok());
    }

    #[test]
    fn skips_when_dependencies_is_empty() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(dir.path().join("manifest.json"), r#"{"id":"test","name":"Test","version":"1.0.0","ikenga_api":"1","mcp":[{"name":"x","command":"node","args":["index.js"],"lifecycle":"long-lived"}]}"#).unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"dependencies":{}}"#).unwrap();

        assert!(materialize_npm_deps(dir.path()).is_ok());
    }

    /// Run an actual `npm install` for a long-lived MCP pkg and assert
    /// `node_modules` is produced. This is the shell-side fix for ikenga#150.
    #[test]
    #[ignore] // slow / network-dependent; run explicitly with `--ignored`
    fn materializes_dependencies_for_long_lived_mcp() {
        let dir = tempfile::tempdir().unwrap();

        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{"id":"test","name":"Test","version":"1.0.0","ikenga_api":"1","mcp":[{"name":"x","command":"node","args":["index.js"],"lifecycle":"long-lived"}]}"#,
        )
        .unwrap();
        // Use a tiny, stable package that npm can resolve quickly.
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"is-odd":"3.0.1"}}"#,
        )
        .unwrap();

        materialize_npm_deps(dir.path()).expect("materialize should succeed");

        assert!(dir.path().join("node_modules/is-odd").is_dir());
    }
}
