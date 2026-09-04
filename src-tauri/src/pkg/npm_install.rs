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

/// Dependency specs npm cannot resolve. All of them mean "resolved locally by
/// the workspace or the build", so the package's shipped artifacts either
/// bundle them or do not need them at run time.
const LOCAL_PROTOCOLS: [&str; 4] = ["workspace:", "link:", "file:", "portal:"];

/// Runtime dependencies with locally-resolved specs removed.
fn sanitize_dependencies(
    package_json: &PackageJson,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    let Some(deps) = package_json.dependencies.as_ref() else {
        return out;
    };
    for (name, spec) in deps {
        let is_local = spec
            .as_str()
            .map(|v| LOCAL_PROTOCOLS.iter().any(|p| v.starts_with(p)))
            .unwrap_or(false);
        if is_local {
            log::info!("dropping locally-resolved dependency `{name}` ({spec}) before npm install");
            continue;
        }
        out.insert(name.clone(), spec.clone());
    }
    out
}

/// Holds the pkg's real package.json while npm runs against a synthesized one.
///
/// `restore` is called on every path — success, npm failure, and spawn failure
/// — so the pkg directory is never left carrying our synthesized manifest.
struct ManifestBackup {
    original: Vec<u8>,
}

impl ManifestBackup {
    fn restore(self, install_path: &Path) {
        let target = install_path.join("package.json");
        if let Err(e) = std::fs::write(&target, &self.original) {
            // Loud: the pkg's own manifest is now our synthesized one, which
            // would mislead anyone reading it and drop its scripts/metadata.
            log::error!(
                "FAILED to restore original package.json at {}: {e}. \
                 The file currently holds the sanitized manifest used for npm install.",
                target.display()
            );
        }
    }
}

/// Write a minimal package.json containing only npm-resolvable runtime deps,
/// returning the original bytes so the caller can put them back.
fn write_sanitized_package_json(
    install_path: &Path,
    deps: &serde_json::Map<String, serde_json::Value>,
) -> Result<ManifestBackup> {
    let target = install_path.join("package.json");
    let original = std::fs::read(&target).context("read package.json")?;

    let synthesized = serde_json::json!({
        "name": "ikenga-pkg-npm-materialization",
        "version": "0.0.0",
        "private": true,
        "dependencies": deps,
    });
    std::fs::write(
        &target,
        serde_json::to_vec_pretty(&synthesized).context("serialize sanitized package.json")?,
    )
    .context("write sanitized package.json")?;

    Ok(ManifestBackup { original })
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

    // A pkg mounted with `ikenga dev` points at its source directory inside the
    // pnpm workspace, where `node_modules` is already a pnpm symlink farm. npm
    // run there follows those symlinks into `.pnpm/…` and tries to `prepare` a
    // dependency pnpm never prepared, failing on a script a published tarball
    // does not ship:
    //
    //     npm error command sh -c npm run build && husky
    //     Cannot find module '…/ext-apps/scripts/generate-schemas.ts'
    //
    // If node_modules is already present, the dependencies are materialized —
    // pnpm did it — and there is nothing for npm to add. Skipping is both the
    // correct behaviour and the only way a workspace-linked pkg can be
    // dev-mounted. A registry-installed pkg never ships node_modules (npm pack
    // always excludes it), so the real install path is unaffected.
    if install_path.join("node_modules").exists() {
        log::info!(
            "node_modules already present in {}; skipping npm materialization",
            install_path.display()
        );
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

    // ── Why a sanitized manifest, and not just `--omit=dev` ────────────────
    //
    // npm parses the ENTIRE package.json — including devDependencies — before
    // it prunes anything, so `--omit=dev` does not save us from a spec npm
    // cannot understand. A pkg developed in this monorepo carries pnpm
    // workspace links (`"@ikenga/contract": "workspace:*"`), and npm rejects
    // the whole install with:
    //
    //     EUNSUPPORTEDPROTOCOL — Unsupported URL Type "workspace:": workspace:*
    //
    // Because materialization failure aborts registration, a single workspace
    // link made the pkg impossible to mount at all — observed on
    // com.ikenga.meetings, which could not be opened until its long-lived MCP
    // entry was removed. Verified empirically that neither `--omit=dev` nor
    // passing explicit `name@spec` arguments avoids the parse.
    //
    // So npm runs against a manifest we synthesize: runtime dependencies only,
    // with unresolvable local protocols dropped. Those dropped deps are always
    // either bundled into the pkg's shipped artifacts or resolved by the
    // workspace at build time, so they are not needed in `node_modules`. The
    // pkg's real package.json is never modified.
    let sanitized = sanitize_dependencies(&package_json);
    if sanitized.is_empty() {
        log::info!(
            "no npm-resolvable dependencies for {} after dropping local specs; skipping",
            install_path.display()
        );
        return Ok(());
    }

    let manifest_backup = write_sanitized_package_json(install_path, &sanitized)
        .context("write sanitized package.json for npm")?;

    let output = Command::new(&npm)
        .args(["install", "--omit=dev", "--no-audit", "--no-fund"])
        .current_dir(install_path)
        .env("PATH", search_path)
        .output();

    // Restore the pkg's own package.json before inspecting the result, so a
    // failure can never leave the pkg with our synthesized one on disk.
    manifest_backup.restore(install_path);

    let output = output.context("spawn npm install")?;

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

    #[test]
    fn existing_node_modules_skips_materialization() {
        // The dev-mount case: pnpm has already linked everything, and running
        // npm inside its symlink farm fails on a dependency's prepare script.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "com.test.pkg", "name": "T", "version": "0.1.0",
                "ikenga_api": "3", "kind": "app",
                "mcp": [{ "name": "m", "command": "node", "args": ["x.js"],
                          "lifecycle": "long-lived" }]
            })).unwrap(),
        )
        .expect("manifest");
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"name":"t","version":"1.0.0","dependencies":{"ajv":"^8"}}"#,
        )
        .expect("package.json");
        std::fs::create_dir(dir.path().join("node_modules")).expect("node_modules");

        // Must return Ok without invoking npm at all. If npm ran here it would
        // need network and would take seconds; this returns immediately.
        materialize_npm_deps(dir.path()).expect("skip, not error");
    }


    /// A pnpm workspace link in a pkg's package.json made `npm install` fail
    /// with EUNSUPPORTEDPROTOCOL, and because materialization failure aborts
    /// registration, the pkg could not be mounted at all. `--omit=dev` does not
    /// help: npm parses the whole manifest before pruning. These pin the
    /// sanitizer that keeps such specs away from npm.
    #[test]
    fn drops_locally_resolved_specs() {
        let pj: PackageJson = serde_json::from_value(serde_json::json!({
            "dependencies": {
                "ajv": "^8.17.1",
                "@ikenga/contract": "workspace:*",
                "@ikenga/tokens": "link:../tokens",
                "some-tarball": "file:../x.tgz",
                "portalled": "portal:../y"
            }
        }))
        .expect("parse");

        let out = sanitize_dependencies(&pj);
        assert_eq!(out.len(), 1, "only the registry spec survives: {out:?}");
        assert_eq!(out.get("ajv").and_then(|v| v.as_str()), Some("^8.17.1"));
        for dropped in ["@ikenga/contract", "@ikenga/tokens", "some-tarball", "portalled"] {
            assert!(!out.contains_key(dropped), "{dropped} should have been dropped");
        }
    }

    #[test]
    fn keeps_ordinary_registry_and_range_specs() {
        let pj: PackageJson = serde_json::from_value(serde_json::json!({
            "dependencies": {
                "a": "1.2.3",
                "b": "^2.0.0",
                "c": "~3.1.0",
                "d": "*",
                "e": "npm:aliased@^1"
            }
        }))
        .expect("parse");
        assert_eq!(sanitize_dependencies(&pj).len(), 5);
    }

    #[test]
    fn empty_when_every_dep_is_local() {
        // This is the skip case: nothing left for npm to do, so materialization
        // must return early rather than run npm against an empty manifest.
        let pj: PackageJson = serde_json::from_value(serde_json::json!({
            "dependencies": { "@ikenga/contract": "workspace:*" }
        }))
        .expect("parse");
        assert!(sanitize_dependencies(&pj).is_empty());
    }

    #[test]
    fn sanitized_manifest_is_written_then_restored_byte_for_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("package.json");
        let original = br#"{"name":"real","version":"9.9.9","scripts":{"build":"x"}}"#;
        std::fs::write(&target, original).expect("seed");

        let mut deps = serde_json::Map::new();
        deps.insert("ajv".into(), serde_json::json!("^8"));

        let backup = write_sanitized_package_json(dir.path(), &deps).expect("write");
        let during = std::fs::read_to_string(&target).expect("read during");
        assert!(during.contains("ajv"), "npm must see the sanitized deps");
        assert!(!during.contains("\"build\""), "synthesized manifest carries no scripts");

        backup.restore(dir.path());
        assert_eq!(
            std::fs::read(&target).expect("read after"),
            original,
            "the pkg's own package.json must come back byte-for-byte"
        );
    }

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
