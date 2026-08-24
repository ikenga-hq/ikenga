//! Ọba Phase 2 (WP-07/08/09) — install from git / npx + update.
//!
//! Fetches a primitive's canonical master from a remote into the vault and keeps
//! it fresh. Design locked Round 4 (`plans/oba-registry`):
//!   - **Canonical layout is IN-PLACE, UNIFORM** — an installed master lives at
//!     the same `store/<kind>s/<name>` a copy-vault import uses. `oba_update`
//!     re-fetches into that same path (atomic swap) so existing symlinks resolve
//!     to the refreshed files with **no relink**.
//!   - **Install = ADD-TO-VAULT ONLY.** These commands create the canonical and
//!     record provenance; per-scope placement stays the separate
//!     `claude_primitive_enable` step. Nothing here touches a `.claude/` farm.
//!   - **`source:"git"`** = `git clone`; **`source:"npx"`** = `npx skills add <gh-spec>`.
//!   - **Public-only auth (v1)** — plain clone / npx, no credential handling.
//!     A private repo fails with a clear error.
//!   - **File-based primitives only** (skill/agent/command). hook/mcp are
//!     JSON-fragments owned by the merge engine, not vault masters — rejected.
//!
//! The fetch always lands in a disposable staging dir first; only an atomic swap
//! promotes it into the vault, so a failed/interrupted fetch never leaves a
//! half-written canonical and never mutates `registry.json`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::registry;
use super::{
    atomic_copy_dir, atomic_copy_file, read_description, store_path_for, validate_name,
    ClaudeStoreEntry, ClaudeStoreMutation, Kind, ProvenanceSource, RegistryProvenance,
};
use crate::commands::claude_config::{mtime_ms, store_root};

use std::sync::Arc;

use tauri::State;

use super::resolve::{
    collect_satisfied, resolve_install_loop_core, resolve_requires_core, PrimitiveRef, RequiresGraph,
};
use crate::commands::db::PaDb;
use crate::pkg::manifest::RequiresEntry;

/// Result of an update check — current (recorded) vs latest (remote) version.
/// Mirrors `UpdateStatus` in `tauri-cmd.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateStatus {
    /// Recorded resolved version (git SHA / npm spec SHA); `None` if never resolved.
    pub current: Option<String>,
    /// Latest resolved version at the remote.
    pub latest: Option<String>,
    /// `true` when `current != latest` (an update is available).
    pub behind: bool,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// A disposable staging path under the system temp dir.
fn staging_dir(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("oba-install-{tag}-{nonce}"))
}

/// Normalize a `git`/`owner/repo`/`github:` spec to an https clone URL.
fn gh_url(spec: &str) -> String {
    if spec.starts_with("http://") || spec.starts_with("https://") || spec.starts_with("git@") {
        spec.to_string()
    } else {
        format!("https://github.com/{}", spec.trim_start_matches("github:"))
    }
}

fn ensure_file_based(kind: Kind) -> Result<(), String> {
    if !kind.is_file_based() {
        return Err(format!(
            "kind {} is a JSON-fragment primitive (merge-engine owned); not installable as a vault master",
            kind.as_str()
        ));
    }
    Ok(())
}

// ─── git / npx invocation (shell out; public-only) ───────────────────────────

fn run(cmd: &str, args: &[&str], cwd: Option<&Path>) -> Result<std::process::Output, String> {
    let mut c = Command::new(cmd);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("`{cmd}` not found on PATH — install it to use {cmd}-sourced primitives")
        } else {
            format!("{cmd}: {e}")
        }
    })
}

/// `git clone --depth 1 [--branch <ref>] <url> <dest>`. Public-only (no creds).
fn git_clone(url: &str, ref_: Option<&str>, dest: &Path) -> Result<(), String> {
    let dest_s = dest.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["clone", "--depth", "1"];
    if let Some(r) = ref_ {
        args.push("--branch");
        args.push(r);
    }
    args.push(url);
    args.push(&dest_s);
    let out = run("git", &args, None)?;
    if !out.status.success() {
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The cloned tree's HEAD commit SHA.
fn git_head_sha(dir: &Path) -> Result<String, String> {
    let dir_s = dir.to_string_lossy().to_string();
    let out = run("git", &["-C", &dir_s, "rev-parse", "HEAD"], None)?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Resolve a remote ref to its SHA WITHOUT cloning (`git ls-remote <url> <ref>`).
/// `ref` defaults to `HEAD`. Returns the first column (the SHA).
fn git_ls_remote_sha(url: &str, ref_: Option<&str>) -> Result<String, String> {
    let r = ref_.unwrap_or("HEAD");
    let out = run("git", &["ls-remote", url, r], None)?;
    if !out.status.success() {
        return Err(format!(
            "git ls-remote failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let first = stdout
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    if first.is_empty() {
        return Err(format!("git ls-remote returned no ref for {url} {r}"));
    }
    Ok(first.to_string())
}

/// `npx --yes skills add <spec>` with HOME + cwd pinned to `staging`, so whatever
/// the Claude `skills` CLI writes lands under our isolated tree (never the user's
/// real `~/.claude`). We adopt the written skill from there.
fn npx_skills_add(spec: &str, staging: &Path) -> Result<(), String> {
    let mut c = Command::new("npx");
    c.args(["--yes", "skills", "add", spec])
        .current_dir(staging)
        .env("HOME", staging);
    let out = c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "`npx` not found on PATH — install Node.js to use npx-sourced primitives".to_string()
        } else {
            format!("npx: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!(
            "npx skills add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// `npx --yes skills add <spec> --skill '*'` — installs ALL member skills a
/// bundle source ships (the `*` / `--all` selector). HOME + cwd pinned to
/// `staging` exactly like `npx_skills_add`, so every member lands under our
/// isolated `.agents/skills/<member>/` tree. This is the bundle install's ONLY
/// network/impure edge; the bundle core takes it as an injected fn so the
/// materialization + members + registry logic stays pure/tempdir-testable.
fn npx_skills_add_all(spec: &str, staging: &Path) -> Result<(), String> {
    let mut c = Command::new("npx");
    c.args(["--yes", "skills", "add", spec, "--skill", "*"])
        .current_dir(staging)
        .env("HOME", staging);
    let out = c.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "`npx` not found on PATH — install Node.js to use npx-sourced primitives".to_string()
        } else {
            format!("npx: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!(
            "npx skills add --skill '*' failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

// ─── locate the primitive inside a fetched tree ──────────────────────────────

/// Find the primitive content inside a freshly-cloned tree. Bounded, predictable
/// search (no deep guessing):
///   skill   → `<root>/SKILL.md` (root IS the skill) | `<root>/skills/<name>` | `<root>/<name>`
///   agent   → `<root>/<name>.md` | `<root>/agents/<name>.md`
///   command → `<root>/<name>.md` | `<root>/commands/<name>.md`
fn locate_in_clone(root: &Path, kind: Kind, name: &str) -> Result<PathBuf, String> {
    match kind {
        Kind::Skill => {
            if root.join("SKILL.md").is_file() {
                return Ok(root.to_path_buf());
            }
            for cand in [root.join("skills").join(name), root.join(name)] {
                if cand.join("SKILL.md").is_file() {
                    return Ok(cand);
                }
            }
            Err(format!(
                "no SKILL.md found in clone for skill {name:?} (looked at root, skills/{name}, {name})"
            ))
        }
        Kind::Agent | Kind::Command => {
            let leaf = format!("{name}.md");
            let mut cands = vec![root.join(&leaf)];
            if let Some(sub) = kind.dir_name() {
                cands.push(root.join(sub).join(&leaf));
            }
            for cand in cands {
                if cand.is_file() {
                    return Ok(cand);
                }
            }
            Err(format!(
                "no {name}.md found in clone for {} {name:?}",
                kind.as_str()
            ))
        }
        Kind::Bundle => {
            Err("bundle locate-in-clone not yet implemented (WP-19); a bundle ships member skills".to_string())
        }
        Kind::Hook | Kind::Mcp => {
            Err("hook/mcp are JSON-fragment primitives; install via the merge engine".to_string())
        }
    }
}

/// Find the skill the `skills` CLI wrote under the staging dir — prefer the
/// expected `name`, else the single skill it produced. The `skills` CLI writes
/// to a CWD-relative `.agents/skills/<name>` (its universal cross-tool layout);
/// the other candidates are defensive fallbacks for layout changes.
fn locate_installed_skill(staging: &Path, name: &str) -> Result<PathBuf, String> {
    for skills_dir in [
        staging.join(".agents").join("skills"),
        staging.join(".claude").join("skills"),
        staging.join("skills"),
    ] {
        let named = skills_dir.join(name);
        if named.join("SKILL.md").is_file() {
            return Ok(named);
        }
        let mut found: Option<PathBuf> = None;
        if let Ok(rd) = std::fs::read_dir(&skills_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.join("SKILL.md").is_file() {
                    if found.is_some() {
                        return Err(
                            "npx skills add produced multiple skills; expected exactly one"
                                .to_string(),
                        );
                    }
                    found = Some(p);
                }
            }
        }
        if let Some(p) = found {
            return Ok(p);
        }
    }
    Err(format!(
        "npx skills add produced no skill for {name:?} under the staging dir"
    ))
}

/// Collect ALL member skills the `skills` CLI wrote under the staging dir — the
/// multi-skill sibling of `locate_installed_skill`. Walks the same three
/// candidate roots (`.agents/skills`, `.claude/skills`, `skills`), returns
/// every immediate child dir containing a `SKILL.md` as `(leaf_name, path)`,
/// sorted by leaf name (so the derived `members` list is deterministic). The
/// FIRST root that yields any skills wins (the CLI writes to exactly one); we
/// don't merge across roots. Errors if no root produced any skill.
fn collect_installed_skills(staging: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    for skills_dir in [
        staging.join(".agents").join("skills"),
        staging.join(".claude").join("skills"),
        staging.join("skills"),
    ] {
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&skills_dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("SKILL.md").is_file() {
                    let leaf = e.file_name().to_string_lossy().to_string();
                    found.push((leaf, p));
                }
            }
        }
        if !found.is_empty() {
            found.sort_by(|a, b| a.0.cmp(&b.0));
            return Ok(found);
        }
    }
    Err("npx skills add --skill '*' produced no member skills under the staging dir".to_string())
}

/// Copy the located primitive into the vault canonical at `store/<kind>s/<name>`,
/// atomically (the existing `atomic_copy_*` swap-over-existing-dst), and return
/// the canonical store path.
fn adopt_into_store(
    store: &Path,
    kind: Kind,
    name: &str,
    located: &Path,
) -> Result<PathBuf, String> {
    let dest = store_path_for(store, kind, name)?;
    if !dest.starts_with(store) {
        return Err(format!("install dest outside store: {}", dest.display()));
    }
    if kind.is_dir_primitive() {
        atomic_copy_dir(located, &dest)?;
    } else {
        atomic_copy_file(located, &dest)?;
    }
    Ok(dest)
}

// ─── fetch + adopt (shared by install + update) ──────────────────────────────

/// Fetch from the remote into staging, locate the primitive, atomically swap it
/// into the vault canonical, and clean up staging. Returns `(canonical, version)`.
/// The vault is mutated only by the final atomic swap, so a failure anywhere
/// before it leaves the prior canonical (if any) untouched.
fn fetch_and_adopt(
    store: &Path,
    kind: Kind,
    name: &str,
    source: ProvenanceSource,
    url: &str,
    ref_: Option<&str>,
) -> Result<(PathBuf, Option<String>, Vec<RequiresEntry>), String> {
    match source {
        ProvenanceSource::Git | ProvenanceSource::Catalog => {
            let staging = staging_dir("git");
            let res = (|| {
                git_clone(url, ref_, &staging)?;
                let sha = git_head_sha(&staging)?;
                let located = locate_in_clone(&staging, kind, name)?;
                // WP-14: the compiled `requires` live in the published
                // `manifest.json` — at the package/clone root for an `ikenga-pkgs`
                // skill (skills/<name>/ layout), or beside SKILL.md for a
                // skill-at-root. Read the clone root first, fall back to the
                // located primitive dir.
                let mut requires = read_manifest_requires(&staging);
                if requires.is_empty() {
                    requires = read_manifest_requires(&located);
                }
                // Don't carry the clone's `.git` into the vault canonical.
                let _ = std::fs::remove_dir_all(located.join(".git"));
                let dest = adopt_into_store(store, kind, name, &located)?;
                Ok::<_, String>((dest, Some(sha), requires))
            })();
            let _ = std::fs::remove_dir_all(&staging);
            res
        }
        ProvenanceSource::Npx => {
            if kind != Kind::Skill {
                return Err(
                    "npx (skills CLI) installs skills only; use git for agents/commands"
                        .to_string(),
                );
            }
            let staging = staging_dir("npx");
            std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir staging: {e}"))?;
            let res = (|| {
                npx_skills_add(url, &staging)?;
                let located = locate_installed_skill(&staging, name)?;
                // The skills CLI writes only the skill dir (no package manifest),
                // so `requires` is best-effort here — read the skill dir itself.
                let requires = read_manifest_requires(&located);
                // Best-effort version: resolve the source repo's HEAD SHA.
                let sha = git_ls_remote_sha(&gh_url(url), None).ok();
                let dest = adopt_into_store(store, kind, name, &located)?;
                Ok::<_, String>((dest, sha, requires))
            })();
            let _ = std::fs::remove_dir_all(&staging);
            res
        }
        ProvenanceSource::Local => {
            // WP-25: install from a LOCAL path — staging = COPY from the dir the
            // caller pointed us at (the dir containing the primitive content, e.g.
            // `ikenga-pkgs/packages/skills/mail/skills/mail`). `url` carries that
            // absolute source path. No remote → no version (`None`). The copy is
            // adopted through the SAME atomic swap as git/npx, so a partial copy
            // never exposes a half-written canonical. Copy-only: the source tree
            // is never moved, symlinked, or mutated (the v2b data-loss guard).
            let src_root = Path::new(url);
            if !src_root.exists() {
                return Err(format!("local source path does not exist: {url}"));
            }
            if !src_root.is_dir() {
                return Err(format!(
                    "local source must be a directory (the dir containing the primitive content): {url}"
                ));
            }
            let located = locate_in_local(src_root, kind, name)?;
            // Compiled `requires`: read the package root's `manifest.json` first
            // (the `ikenga-pkgs` skills/<name>/ publish layout puts it at the
            // package root, two levels above SKILL.md), then fall back to the
            // located primitive dir — mirrors the git path's two-step lookup.
            let mut requires = read_manifest_requires_walk_up(&located, src_root);
            if requires.is_empty() {
                requires = read_manifest_requires(&located);
            }
            let dest = adopt_into_store(store, kind, name, &located)?;
            // No remote ⇒ no resolved version.
            Ok((dest, None, requires))
        }
    }
}

/// Find the primitive content inside a LOCAL source directory — the copy-staging
/// sibling of `locate_in_clone`. Same bounded search (no deep guessing):
///   skill   → `<root>/SKILL.md` (root IS the skill) | `<root>/skills/<name>` | `<root>/<name>`
///   agent   → `<root>/<name>.md` | `<root>/agents/<name>.md`
///   command → `<root>/<name>.md` | `<root>/commands/<name>.md`
/// `locate_in_clone` operates on a freshly-cloned tree; this operates on the
/// caller-supplied local dir (never mutated — `adopt_into_store` copies out of it).
fn locate_in_local(root: &Path, kind: Kind, name: &str) -> Result<PathBuf, String> {
    match kind {
        Kind::Skill => {
            if root.join("SKILL.md").is_file() {
                return Ok(root.to_path_buf());
            }
            for cand in [root.join("skills").join(name), root.join(name)] {
                if cand.join("SKILL.md").is_file() {
                    return Ok(cand);
                }
            }
            Err(format!(
                "no SKILL.md found in local source for skill {name:?} (looked at root, skills/{name}, {name})"
            ))
        }
        Kind::Agent | Kind::Command => {
            let leaf = format!("{name}.md");
            let mut cands = vec![root.join(&leaf)];
            if let Some(sub) = kind.dir_name() {
                cands.push(root.join(sub).join(&leaf));
            }
            for cand in cands {
                if cand.is_file() {
                    return Ok(cand);
                }
            }
            Err(format!(
                "no {name}.md found in local source for {} {name:?}",
                kind.as_str()
            ))
        }
        Kind::Bundle => {
            Err("bundle local-install not supported (a bundle ships member skills via the bundle path)".to_string())
        }
        Kind::Hook | Kind::Mcp => {
            Err("hook/mcp are JSON-fragment primitives; install via the merge engine".to_string())
        }
    }
}

/// Read the compiled `requires` from the nearest `manifest.json` walking UP from
/// the located primitive dir toward (and including) the source root. The
/// `ikenga-pkgs` skills publish layout is `packages/skills/<pkg>/skills/<name>/`
/// with the `manifest.json` (carrying `requires`) at the package root — two
/// levels above SKILL.md — so a plain "read beside SKILL.md" misses it. We probe
/// the located dir, then each ancestor up to `boundary` (inclusive). Returns the
/// first non-empty `requires` found; empty if none on the path.
fn read_manifest_requires_walk_up(located: &Path, boundary: &Path) -> Vec<RequiresEntry> {
    let mut cur = Some(located);
    while let Some(dir) = cur {
        let req = read_manifest_requires(dir);
        if !req.is_empty() {
            return req;
        }
        if dir == boundary {
            break;
        }
        cur = dir.parent();
    }
    Vec::new()
}

fn build_entry(
    kind: Kind,
    name: &str,
    dest: &Path,
    requires: Vec<RequiresEntry>,
    prov: RegistryProvenance,
) -> ClaudeStoreEntry {
    ClaudeStoreEntry {
        kind: kind.as_str().to_string(),
        name: name.to_string(),
        store_path: dest.to_string_lossy().to_string(),
        description: read_description(dest, kind),
        modified_ms: mtime_ms(dest),
        enabled_in: Vec::new(),
        // WP-12/WP-14: the compiled `requires` are read from the fetched
        // primitive's `manifest.json` (the publish-time lift writes them there)
        // and recorded here so `claude_store_list` + the resolver see them.
        requires,
        // WP-18: leaf installs record no bundle members; the bundle installer
        // (WP-19) is the only site that populates this.
        members: Vec::new(),
        provenance: prov,
    }
}

// ─── core (pure: store is a parameter, tempdir-testable) ──────────────────────

fn install_core(
    store: &Path,
    kind: Kind,
    name: &str,
    source: ProvenanceSource,
    url: &str,
    ref_: Option<&str>,
    from_catalog: bool,
) -> Result<ClaudeStoreEntry, String> {
    ensure_file_based(kind)?;
    validate_name(name)?;
    let (dest, version, requires) = fetch_and_adopt(store, kind, name, source, url, ref_)?;
    let now = now_iso();
    let prov = RegistryProvenance {
        source,
        url: Some(url.to_string()),
        r#ref: ref_.map(|s| s.to_string()),
        version,
        canonical_path: dest.to_string_lossy().to_string(),
        managed: true,
        installed_at: Some(now.clone()),
        updated_at: Some(now),
        // Phase 3: record the catalog discovery origin orthogonally to the
        // resolved fetch mechanism (`source`). Curated-catalog installs opt into
        // auto-update; plain git/npx installs do not (catalog ON, manual OFF).
        from_catalog,
        auto_update: from_catalog,
    };
    let entry = build_entry(kind, name, &dest, requires, prov);
    let mut rf = registry::load(store);
    registry::upsert_record(&mut rf, entry.clone());
    registry::save(store, &rf)?;
    Ok(entry)
}

fn check_update_core(store: &Path, kind: Kind, name: &str) -> Result<UpdateStatus, String> {
    let rf = registry::load(store);
    let entry = rf
        .entries
        .iter()
        .find(|e| e.kind == kind.as_str() && e.name == name)
        .ok_or_else(|| format!("{} {name:?} not in registry", kind.as_str()))?;
    let prov = &entry.provenance;
    let url = prov
        .url
        .as_deref()
        .ok_or_else(|| "entry has no source url; nothing to check".to_string())?;
    let latest = match prov.source {
        ProvenanceSource::Git | ProvenanceSource::Catalog => {
            git_ls_remote_sha(url, prov.r#ref.as_deref())?
        }
        ProvenanceSource::Npx => git_ls_remote_sha(&gh_url(url), None)?,
        ProvenanceSource::Local => return Err("local entries have no remote to check".to_string()),
    };
    Ok(UpdateStatus {
        current: prov.version.clone(),
        behind: prov.version.as_deref() != Some(latest.as_str()),
        latest: Some(latest),
    })
}

fn update_core(store: &Path, kind: Kind, name: &str) -> Result<ClaudeStoreEntry, String> {
    // WP-19: a bundle re-fetches whole (re-run skills-add --skill '*', re-assemble,
    // atomic-swap, re-derive members) — it does NOT flow through the single-skill
    // fetch_and_adopt path (which rejects Bundle). Dispatch before that.
    if kind == Kind::Bundle {
        return update_bundle_core(store, name, &|spec, staging| {
            npx_skills_add_all(spec, staging)
        });
    }
    let rf = registry::load(store);
    let existing = rf
        .entries
        .iter()
        .find(|e| e.kind == kind.as_str() && e.name == name)
        .cloned()
        .ok_or_else(|| {
            format!(
                "{} {name:?} not in registry; install it first",
                kind.as_str()
            )
        })?;
    let prov = existing.provenance.clone();
    if !prov.managed {
        return Err(format!(
            "{} {name:?} is an external master (kept in place) — update its source upstream, not here",
            kind.as_str()
        ));
    }
    // WP-25: a local-source install has no remote to re-fetch from — `update`
    // (and `check_update`) refuse it. Re-installing from the local path is the
    // explicit `oba_install_local` call, not an `update`. This also keeps the
    // auto-update batch a no-op for local entries (their `auto_update` is always
    // off, but guard the manual path too).
    if prov.source == ProvenanceSource::Local {
        return Err(format!(
            "{} {name:?} is a local-source install (no remote) — re-run the local install to refresh, not update",
            kind.as_str()
        ));
    }
    let url = prov
        .url
        .clone()
        .ok_or_else(|| "entry has no source url; cannot update".to_string())?;
    let (dest, version, requires) =
        fetch_and_adopt(store, kind, name, prov.source, &url, prov.r#ref.as_deref())?;
    let new_prov = RegistryProvenance {
        source: prov.source,
        url: prov.url.clone(),
        r#ref: prov.r#ref.clone(),
        version,
        canonical_path: dest.to_string_lossy().to_string(),
        managed: true,
        installed_at: prov.installed_at.clone(),
        updated_at: Some(now_iso()),
        // Preserve the Phase-3 discovery origin + auto-update opt-in across a
        // re-fetch — an update doesn't change where the primitive came from or
        // the user's auto-update preference.
        from_catalog: prov.from_catalog,
        auto_update: prov.auto_update,
    };
    let entry = build_entry(kind, name, &dest, requires, new_prov);
    let mut rf2 = registry::load(store);
    registry::upsert_record(&mut rf2, entry.clone());
    registry::save(store, &rf2)?;
    Ok(entry)
}

// ─── WP-19 — multi-skill BUNDLE install (store population + registry only) ────
//
// A bundle ships N member skills. The Claude `skills` CLI installs them all in
// one shot with `skills add <spec> --skill '*'`. We materialize them into the
// vault canonical `store/bundles/<name>/<member>/SKILL.md` (one dir per member)
// and record ONE registry entry: `kind=bundle`, `members=[sorted leaf names]`,
// provenance `source:npx`, `url:<spec>`. The members list is DERIVED from what
// the CLI actually produced — never hand-passed.
//
// PLACEMENT (symlinking members into a scope's .agents/skills) is WP-21;
// RESOLUTION (a pkg's `requires:{kind:bundle}` expanding to members) is WP-20.
// Neither is done here — this is store + registry only.
//
// The network `skills add --skill '*'` step is the ONLY impure edge and is
// INJECTED as a closure (`fetch_fn`) so the materialization core is pure and
// tempdir-testable without any network: a test supplies a fetch_fn that writes
// a fake `.agents/skills/<member>/SKILL.md` tree into the staging dir.

/// Build the single bundle registry entry. Mirrors `build_entry` but populates
/// `members` (sorted member leaf names) instead of leaving it empty, and never
/// carries `requires` (a bundle has no compiled deps of its own — its members
/// carry theirs; resolution of member requires is WP-20).
fn build_bundle_entry(
    name: &str,
    dest: &Path,
    members: Vec<String>,
    prov: RegistryProvenance,
) -> ClaudeStoreEntry {
    ClaudeStoreEntry {
        kind: Kind::Bundle.as_str().to_string(),
        name: name.to_string(),
        store_path: dest.to_string_lossy().to_string(),
        // A bundle dir holds member skill subdirs, NOT a top-level SKILL.md, so a
        // bundle has no description of its own (its members carry theirs). Be
        // explicit rather than relying on read_description silently finding none.
        description: None,
        modified_ms: mtime_ms(dest),
        enabled_in: Vec::new(),
        requires: Vec::new(),
        members,
        provenance: prov,
    }
}

/// Fetch + assemble + atomic-swap the bundle canonical, returning
/// `(canonical_path, sorted_member_leaves, version)`. The vault is mutated only
/// by the final `atomic_copy_dir` swap, so a failed fetch/assemble leaves the
/// prior canonical (if any) untouched. `fetch_fn` is the injected skills-add
/// edge: it must populate `staging` with the member skills' `.agents/skills/...`
/// tree (real impl: `npx_skills_add_all`).
fn fetch_and_adopt_bundle(
    store: &Path,
    name: &str,
    spec: &str,
    fetch_fn: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<(PathBuf, Vec<String>, Option<String>), String> {
    let dest = store_path_for(store, Kind::Bundle, name)?;
    if !dest.starts_with(store) {
        return Err(format!("install dest outside store: {}", dest.display()));
    }
    let staging = staging_dir("bundle");
    std::fs::create_dir_all(&staging).map_err(|e| format!("mkdir staging: {e}"))?;
    // Hoisted out of the closure so it is cleaned up UNCONDITIONALLY below — an
    // error mid-assembly must not leak the temp dir in /tmp.
    let assembled = staging_dir("bundle-assembled");
    let res = (|| {
        fetch_fn(spec, &staging)?;
        let members = collect_installed_skills(&staging)?;
        // Assemble the whole bundle dir under a disposable temp, then swap the
        // assembled tree into the canonical in ONE atomic_copy_dir (handles
        // swap-over-existing-dst + rollback). This makes update a true
        // whole-bundle replace: a member dropped upstream simply isn't in the
        // freshly-assembled tree.
        let _ = std::fs::remove_dir_all(&assembled);
        std::fs::create_dir_all(&assembled).map_err(|e| format!("mkdir assembled: {e}"))?;
        let mut leaves: Vec<String> = Vec::with_capacity(members.len());
        for (leaf, member_path) in &members {
            atomic_copy_dir(member_path, &assembled.join(leaf))?;
            leaves.push(leaf.clone());
        }
        // Best-effort source version: resolve the spec repo's HEAD SHA. NOTE:
        // this is a SECOND, best-effort network touch separate from the injected
        // `fetch_fn` (it `.ok()`s to None offline, incl. in tests) — the skills-add
        // fetch is the only edge that must be injected for the core to be testable.
        let sha = git_ls_remote_sha(&gh_url(spec), None).ok();
        atomic_copy_dir(&assembled, &dest)?;
        // `leaves` is already sorted (collect_installed_skills returns sorted),
        // so members[] is the sorted member-leaf set — no re-sort needed.
        Ok::<_, String>((dest.clone(), leaves, sha))
    })();
    let _ = std::fs::remove_dir_all(&assembled);
    let _ = std::fs::remove_dir_all(&staging);
    res
}

/// Pure bundle install/update core (store is a parameter; the skills-add edge is
/// injected). Installs ALL member skills of `spec` into
/// `store/bundles/<name>/` and writes ONE registry record. Idempotent:
/// re-running re-fetches, re-assembles, atomically swaps the bundle dir, and
/// re-derives members — so this same fn serves both install and update.
fn install_bundle_core(
    store: &Path,
    name: &str,
    spec: &str,
    from_catalog: bool,
    fetch_fn: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<ClaudeStoreEntry, String> {
    validate_name(name)?;
    // Preserve the prior provenance timestamps + auto-update opt-in across a
    // re-install (update semantics): if a record already exists, keep its
    // installed_at + auto_update; otherwise this is a fresh install.
    let prior = registry::load(store)
        .entries
        .into_iter()
        .find(|e| e.kind == Kind::Bundle.as_str() && e.name == name);

    let (dest, members, version) = fetch_and_adopt_bundle(store, name, spec, fetch_fn)?;
    let now = now_iso();
    let prov = RegistryProvenance {
        source: ProvenanceSource::Npx,
        url: Some(spec.to_string()),
        r#ref: None,
        version,
        canonical_path: dest.to_string_lossy().to_string(),
        managed: true,
        installed_at: prior
            .as_ref()
            .and_then(|p| p.provenance.installed_at.clone())
            .or_else(|| Some(now.clone())),
        updated_at: Some(now),
        from_catalog: prior.as_ref().map_or(from_catalog, |p| p.provenance.from_catalog),
        auto_update: prior
            .as_ref()
            .map_or(from_catalog, |p| p.provenance.auto_update),
    };
    let entry = build_bundle_entry(name, &dest, members, prov);
    let mut rf = registry::load(store);
    registry::upsert_record(&mut rf, entry.clone());
    registry::save(store, &rf)?;
    Ok(entry)
}

/// Whole-bundle atomic re-fetch (update). Reads the existing bundle record for
/// its `spec` (provenance url) and re-runs the install flow, which swaps the
/// bundle dir in place and re-derives members. Mirrors `update_core`'s
/// managed-check; the `fetch_fn` injection keeps it tempdir-testable.
fn update_bundle_core(
    store: &Path,
    name: &str,
    fetch_fn: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<ClaudeStoreEntry, String> {
    let existing = registry::load(store)
        .entries
        .into_iter()
        .find(|e| e.kind == Kind::Bundle.as_str() && e.name == name)
        .ok_or_else(|| format!("bundle {name:?} not in registry; install it first"))?;
    if !existing.provenance.managed {
        return Err(format!(
            "bundle {name:?} is an external master (kept in place) — update its source upstream, not here"
        ));
    }
    let spec = existing
        .provenance
        .url
        .clone()
        .ok_or_else(|| "bundle entry has no source spec; cannot update".to_string())?;
    install_bundle_core(store, name, &spec, existing.provenance.from_catalog, fetch_fn)
}

// ─── Phase 3 — auto-update trust policy (catalog ON, local/dev/manual OFF) ────

/// Per-entry outcome of a batch auto-update run. Mirrors `AutoUpdateRow` in
/// `tauri-cmd.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoUpdateRow {
    pub kind: String,
    pub name: String,
    /// `"updated"` (a stale entry was re-fetched), `"current"` (already at the
    /// latest), or `"error"` (the check/update failed — see `error`).
    pub status: String,
    /// Resolved version after a successful update; `None` on current/error.
    pub version: Option<String>,
    /// Populated only when `status == "error"`.
    pub error: Option<String>,
}

/// Summary of a batch auto-update run over every `auto_update`-opted entry.
/// Mirrors `AutoUpdateSummary` in `tauri-cmd.ts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoUpdateSummary {
    /// Names that were re-fetched (were behind).
    pub updated: Vec<AutoUpdateRow>,
    /// Names that were already current (no fetch).
    pub current: Vec<AutoUpdateRow>,
    /// Names whose check/update errored — the batch continues past each.
    pub errored: Vec<AutoUpdateRow>,
}

/// Run auto-updates across every registry entry with `auto_update == true`:
/// check each against its remote and re-fetch the stale ones. A per-entry error
/// is recorded and the batch continues (never aborts). Pure-core (store is a
/// parameter), tempdir-testable.
fn auto_update_all_core(store: &Path) -> AutoUpdateSummary {
    let rf = registry::load(store);
    // Snapshot the (kind, name) of opted-in entries up front; each update reloads
    // + rewrites registry.json, so we don't iterate a live-mutating list.
    let targets: Vec<(String, String)> = rf
        .entries
        .iter()
        .filter(|e| e.provenance.auto_update)
        .map(|e| (e.kind.clone(), e.name.clone()))
        .collect();

    let mut summary = AutoUpdateSummary {
        updated: Vec::new(),
        current: Vec::new(),
        errored: Vec::new(),
    };

    for (kind_s, name) in targets {
        let k = match Kind::parse(&kind_s) {
            Ok(k) => k,
            Err(e) => {
                summary.errored.push(AutoUpdateRow {
                    kind: kind_s,
                    name,
                    status: "error".into(),
                    version: None,
                    error: Some(e),
                });
                continue;
            }
        };
        match check_update_core(store, k, &name) {
            Ok(st) if st.behind => match update_core(store, k, &name) {
                Ok(entry) => {
                    tracing::info!(kind = %kind_s, name = %name, "oba auto-update: refreshed stale catalog entry");
                    summary.updated.push(AutoUpdateRow {
                        kind: kind_s,
                        name,
                        status: "updated".into(),
                        version: entry.provenance.version,
                        error: None,
                    });
                }
                Err(e) => {
                    tracing::warn!(kind = %kind_s, name = %name, error = %e, "oba auto-update: update failed");
                    summary.errored.push(AutoUpdateRow {
                        kind: kind_s,
                        name,
                        status: "error".into(),
                        version: None,
                        error: Some(e),
                    });
                }
            },
            Ok(st) => summary.current.push(AutoUpdateRow {
                kind: kind_s,
                name,
                status: "current".into(),
                version: st.current,
                error: None,
            }),
            Err(e) => {
                tracing::warn!(kind = %kind_s, name = %name, error = %e, "oba auto-update: check failed");
                summary.errored.push(AutoUpdateRow {
                    kind: kind_s,
                    name,
                    status: "error".into(),
                    version: None,
                    error: Some(e),
                });
            }
        }
    }
    summary
}

/// Set the `auto_update` flag on an installed managed entry and persist it to
/// `registry.json`. Returns the new flag value. Errors if the entry is absent.
fn set_auto_update_core(
    store: &Path,
    kind: Kind,
    name: &str,
    enabled: bool,
) -> Result<bool, String> {
    let mut rf = registry::load(store);
    let entry = rf
        .entries
        .iter_mut()
        .find(|e| e.kind == kind.as_str() && e.name == name)
        .ok_or_else(|| format!("{} {name:?} not in registry", kind.as_str()))?;
    entry.provenance.auto_update = enabled;
    registry::save(store, &rf)?;
    Ok(enabled)
}

// ─── Tauri commands (thin: resolve the real store root, delegate to core) ─────

/// Install a primitive from a git remote into the vault as a managed canonical.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_git(
    kind: String,
    name: String,
    url: String,
    gitRef: Option<String>,
    fromCatalog: Option<bool>,
) -> Result<ClaudeStoreEntry, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    install_core(
        &store,
        k,
        &name,
        ProvenanceSource::Git,
        &url,
        gitRef.as_deref(),
        fromCatalog.unwrap_or(false),
    )
}

/// Install a primitive via the Claude `skills` CLI (`npx skills add <spec>`).
/// `fromCatalog` records that the install was discovered through the recommended
/// catalog (Phase 3) — set by the catalog Install path, omitted/false for a
/// direct npx install.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_npx(
    kind: String,
    name: String,
    spec: String,
    fromCatalog: Option<bool>,
) -> Result<ClaudeStoreEntry, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    install_core(
        &store,
        k,
        &name,
        ProvenanceSource::Npx,
        &spec,
        None,
        fromCatalog.unwrap_or(false),
    )
}

/// Install a primitive from a LOCAL path into the vault as a managed canonical
/// (WP-25). Staging is a COPY of the dir at `path` (the dir containing the
/// primitive content, e.g. a skill's `SKILL.md` dir); provenance is recorded as
/// `local` with NO version (there is no remote to resolve a SHA from) and
/// `auto_update` OFF (catalog ON, local/manual OFF — there is nothing to update
/// from). Copy-only: the source tree at `path` is never moved, symlinked, or
/// mutated. The store `name` is the canonical store dir (`store/skills/<name>`),
/// independent of the skill's own frontmatter name — so a local dir whose
/// SKILL.md is named `mail` can be installed as `skill-mail` to satisfy a pkg's
/// `requires:{kind:"skill",name:"skill-mail"}` edge.
#[tauri::command]
pub async fn oba_install_local(
    kind: String,
    name: String,
    path: String,
) -> Result<ClaudeStoreEntry, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    // Local installs are never catalog-discovered → from_catalog=false →
    // auto_update=false (the `install_core` provenance rule).
    install_core(
        &store,
        k,
        &name,
        ProvenanceSource::Local,
        &path,
        None,
        false,
    )
}

/// Install a multi-skill BUNDLE via the Claude `skills` CLI
/// (`npx skills add <spec> --skill '*'`). Materializes ALL member skills into
/// the vault canonical `store/bundles/<name>/<member>/` and writes ONE registry
/// record (`kind:bundle`, `members:[sorted leaves]`, `source:npx`). Idempotent:
/// re-running this re-fetches + atomically swaps the bundle dir + re-derives
/// members (so it doubles as the update path). `scope` is accepted for forward
/// compatibility with WP-21 placement but is unused here (store population only).
/// `fromCatalog` records catalog discovery (Phase 3).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_bundle(
    name: String,
    spec: String,
    scope: Option<String>,
    fromCatalog: Option<bool>,
) -> Result<ClaudeStoreEntry, String> {
    // WP-19 is store + registry only; placement (the `scope`) is WP-21.
    let _ = scope;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    install_bundle_core(
        &store,
        &name,
        &spec,
        fromCatalog.unwrap_or(false),
        &|spec, staging| npx_skills_add_all(spec, staging),
    )
}

/// Check whether a git/npx-installed primitive is behind its remote.
#[tauri::command]
pub async fn oba_check_update(kind: String, name: String) -> Result<UpdateStatus, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    check_update_core(&store, k, &name)
}

/// Re-fetch a managed primitive into its existing canonical in place (no relink).
#[tauri::command]
pub async fn oba_update(kind: String, name: String) -> Result<ClaudeStoreEntry, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    update_core(&store, k, &name)
}

/// Phase 3 — auto-update every `auto_update`-opted entry that's behind its
/// remote (FE-driven: called on the Ọba/catalog surface mount). Per-entry errors
/// are collected, never abort the batch. Returns a summary of updated / current /
/// errored entries.
#[tauri::command]
pub async fn oba_auto_update_all() -> Result<AutoUpdateSummary, String> {
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    Ok(auto_update_all_core(&store))
}

/// Phase 3 — toggle the per-entry auto-update opt-in and persist it to
/// `registry.json`. Returns the new flag value.
#[tauri::command]
pub async fn oba_set_auto_update(
    kind: String,
    name: String,
    enabled: bool,
) -> Result<bool, String> {
    let k = Kind::parse(&kind)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    set_auto_update_core(&store, k, &name, enabled)
}

// ─── WP-14 — forward-dependency resolution at install (ADR-015 §3b) ───────────
//
// `oba_install_with_deps` is the resolver-wired install: it fetches the target,
// reads the target's compiled `requires` (WP-12 lift, embedded in the fetched
// `manifest.json`), then installs the missing dependency closure transactionally
// in topological order via `resolve::resolve_install_loop_core`, rolling back
// every install (including the target) if any dependency fetch fails.
//
// The discovery catalog (`primitives.json`) carries no `requires`, so the FE
// passes a catalog SNAPSHOT (name → source+url); each fetched dependency's own
// `manifest.json` reveals its transitive `requires`, growing the graph across
// iterations (the install-then-reresolve loop). The real depth today is 1
// (skill-pa → skill-core), so it converges immediately, but the loop handles
// arbitrary acyclic depth and defends against cycles.

/// A catalog row the FE hands the installer so a dependency `(kind,name)` can be
/// resolved to a fetchable `(source,url)`. Mirrors the subset of
/// `PrimitiveCatalogEntry` (`primitives.ts`) the resolver needs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CatalogEntryRef {
    pub kind: String,
    pub name: String,
    /// `"git"` | `"npx"` — the fetch mechanism (mirrors the catalog `source`).
    pub source: String,
    /// git remote URL | npx/skills spec.
    pub url: String,
    /// Member skills a `bundle` catalog row carries (WP-18). Derived at publish
    /// into the catalog (later WP); the bundle installer (WP-19) places these.
    /// `#[serde(default)]` so a non-bundle / pre-WP-18 catalog row (no `members`)
    /// still deserializes. Mirrors `members?` on the TS `PrimitiveCatalogEntry`.
    #[serde(default)]
    #[allow(dead_code)]
    pub members: Vec<String>,
}

/// Result of a resolver-driven install: the target plus the dependency closure
/// that was auto-installed (topological enable order) and the deps that were
/// already present (surfaced for the WP-15 consent UX; never reinstalled).
#[derive(Debug, Clone, Serialize)]
pub struct InstallWithDepsResult {
    pub target: ClaudeStoreEntry,
    /// The auto-installed dependency closure, deepest-first (enable order).
    pub installed: Vec<ClaudeStoreEntry>,
    /// Deps already satisfied (store ∪ external) — listed, not installed.
    #[serde(rename = "alreadySatisfied")]
    pub already_satisfied: Vec<PrimitiveRef>,
}

/// Map a wire fetch-source string to the provenance enum (target + deps come in
/// as `"git"`/`"npx"`; `"catalog"` is tolerated and dispatches like git).
fn parse_source(s: &str) -> Result<ProvenanceSource, String> {
    match s {
        "git" => Ok(ProvenanceSource::Git),
        "npx" => Ok(ProvenanceSource::Npx),
        "catalog" => Ok(ProvenanceSource::Git),
        "local" => Ok(ProvenanceSource::Local),
        other => Err(format!("install source must be git|npx|local, got {other:?}")),
    }
}

/// Read a fetched primitive's compiled `requires` from its `manifest.json`
/// (the WP-12 lift writes it there). Absent/unreadable/array-less ⇒ no deps.
/// Ọba reads ONLY this compiled field — never `SKILL.md` (ADR-015 §3a).
fn read_manifest_requires(dir: &Path) -> Vec<RequiresEntry> {
    #[derive(Deserialize)]
    struct ManifestRequires {
        #[serde(default)]
        requires: Vec<RequiresEntry>,
    }
    let path = dir.join("manifest.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<ManifestRequires>(&raw)
        .map(|m| m.requires)
        .unwrap_or_default()
}

/// Roll back one fetched primitive: vault-delete its canonical + drop its
/// registry record. Best-effort (a rollback failure must not mask the original
/// error). Mirrors the removal half of `oba_safe_delete`.
fn rollback_install_kn(store: &Path, kind: Kind, name: &str) {
    if let Ok(canon) = store_path_for(store, kind, name) {
        if canon.is_dir() {
            let _ = std::fs::remove_dir_all(&canon);
        } else if canon.exists() {
            let _ = std::fs::remove_file(&canon);
        }
    }
    let mut rf = registry::load(store);
    rf.entries
        .retain(|e| !(e.kind == kind.as_str() && e.name == name));
    let _ = registry::save(store, &rf);
}

fn rollback_install(store: &Path, item: &RequiresEntry) {
    if let Ok(k) = Kind::parse(&item.kind) {
        rollback_install_kn(store, k, &item.name);
    }
}

/// Fetch + adopt one dependency named by `item`, resolving its `(source,url)`
/// from the catalog snapshot. Returns the dep's OWN revealed `requires` (read
/// from its fetched manifest) so the install loop can grow the graph for
/// transitive deps. Deps inherit catalog provenance (`from_catalog: true`).
///
/// WP-20: dispatches by `item.kind` — `"bundle"` routes to
/// `install_bundle_core`; everything else routes to the existing
/// `install_core` (single-skill/agent/command). A bundle's registry entry
/// carries no `requires` of its own (members carry theirs; member deps are
/// resolved at placement time by WP-21), so we return `Vec::new()` for bundles.
fn install_dep(
    store: &Path,
    catalog: &[CatalogEntryRef],
    item: &RequiresEntry,
) -> Result<Vec<RequiresEntry>, String> {
    // Production: inject the real npx skills-add edge for the bundle branch.
    install_dep_with(store, catalog, item, &|spec, staging| {
        npx_skills_add_all(spec, staging)
    })
}

/// `install_dep` with the bundle skills-add edge INJECTED, so the bundle
/// dispatch (absent-bundle install) is unit-testable offline. The single-skill
/// path fetches from a local `file://` git repo and needs no injection, so only
/// the bundle edge is parameterized.
fn install_dep_with(
    store: &Path,
    catalog: &[CatalogEntryRef],
    item: &RequiresEntry,
    bundle_fetch: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> Result<Vec<RequiresEntry>, String> {
    // WP-20: bundle dispatch — a requires:{kind:"bundle"} edge installs via
    // the bundle path (install_bundle_core), NOT the single-skill install_core
    // which would reject Kind::Bundle.
    if item.kind == "bundle" {
        let cat = catalog
            .iter()
            .find(|c| c.kind == item.kind && c.name == item.name)
            .ok_or_else(|| {
                format!(
                    "bundle dependency {} is not in the catalog snapshot; cannot resolve its source",
                    item.name
                )
            })?;
        install_bundle_core(store, &item.name, &cat.url, true, bundle_fetch)?;
        // A bundle carries no requires of its own (members do); no graph growth.
        return Ok(Vec::new());
    }

    let k = Kind::parse(&item.kind)?;
    let cat = catalog
        .iter()
        .find(|c| c.kind == item.kind && c.name == item.name)
        .ok_or_else(|| {
            format!(
                "dependency {}:{} is not in the catalog snapshot; cannot resolve its source",
                item.kind, item.name
            )
        })?;
    let src = parse_source(&cat.source)?;
    let entry = install_core(store, k, &item.name, src, &cat.url, None, true)?;
    Ok(entry.requires)
}

/// The transactional resolver-driven install core (pure over `store` +
/// `scope_roots`, so it is tempdir-testable with the real `install_core`).
/// Installs the target, then its missing dependency closure in topological
/// order; on any dependency failure rolls back the closure AND the target.
///
/// NOTE on ordering: the target is fetched FIRST — we must read its
/// freshly-fetched `manifest.json` to learn its `requires` (the discovery
/// catalog carries none). For a vault add this is immaterial (placement/enable
/// is a separate step) and fully reversible — a closure failure rolls the
/// target back too. The returned `installed` list is in topological ENABLE
/// order (deepest dep first); the caller enables the closure, then the target.
/// Install the missing forward-dependency closure of an arbitrary `requires`
/// list into the store, transactionally. Shared by `oba_install_with_deps` (a
/// primitive's own deps, WP-14) and the pkg-kernel → Ọba seam (a pkg manifest's
/// `requires`, WP-16). Pure over `store` + `scope_roots` (tempdir-testable with
/// the real `install_core` against local `file://` repos).
///
/// `satisfied` (store ∪ external masters) is computed once and read-only across
/// the loop — a just-fetched node must NOT be marked satisfied or its revealed
/// children would be pruned (the loop tracks fetched-ness separately). On any
/// dependency failure the loop rolls back the closure in reverse and the error
/// propagates (the caller rolls back anything it installed BEFORE calling this,
/// e.g. `install_with_deps_core`'s target).
///
/// Returns `(installed-closure in topological/enable order, already-satisfied
/// deps)`.
fn install_requires_closure_core(
    store: &Path,
    scope_roots: &[PathBuf],
    requires: &[RequiresEntry],
    catalog: &[CatalogEntryRef],
) -> Result<(Vec<ClaudeStoreEntry>, Vec<PrimitiveRef>), String> {
    let satisfied = collect_satisfied(store, scope_roots);

    // Deps already present up front (consent UX / WP-15) — best-effort.
    let already_satisfied = resolve_requires_core(requires, &RequiresGraph::new(), &satisfied)
        .map(|p| p.already_satisfied)
        .unwrap_or_default();

    // Install the missing closure transactionally. install_one reveals each dep's
    // own requires (growing the graph); a failure rolls back the closure in reverse.
    let mut graph = RequiresGraph::new();
    let closure = resolve_install_loop_core(
        requires,
        &satisfied,
        &mut graph,
        |item| install_dep(store, catalog, item),
        |item| rollback_install(store, item),
    )
    .map_err(|e| e.to_string())?;

    // Map the topologically-ordered closure refs back to their store entries.
    let rf = registry::load(store);
    let installed: Vec<ClaudeStoreEntry> = closure
        .iter()
        .filter_map(|r| {
            rf.entries
                .iter()
                .find(|e| e.kind == r.kind && e.name == r.name)
                .cloned()
        })
        .collect();

    Ok((installed, already_satisfied))
}

#[allow(clippy::too_many_arguments)]
fn install_with_deps_core(
    store: &Path,
    scope_roots: &[PathBuf],
    kind: Kind,
    name: &str,
    source: ProvenanceSource,
    url: &str,
    ref_: Option<&str>,
    from_catalog: bool,
    catalog: &[CatalogEntryRef],
) -> Result<InstallWithDepsResult, String> {
    // 1. Fetch the target; its fetched manifest reveals its compiled `requires`.
    let target_entry = install_core(store, kind, name, source, url, ref_, from_catalog)?;
    let target_requires = target_entry.requires.clone();

    // 2. Install the missing closure; on failure roll the TARGET back too.
    match install_requires_closure_core(store, scope_roots, &target_requires, catalog) {
        Ok((installed, already_satisfied)) => Ok(InstallWithDepsResult {
            target: target_entry,
            installed,
            already_satisfied,
        }),
        Err(e) => {
            rollback_install_kn(store, kind, name);
            Err(e)
        }
    }
}

/// Re-verify, at enable time, which of `(kind,name)`'s recorded `requires` are
/// NOT currently present (a dep may have been removed since install). The FE
/// offers to re-fetch the returned set (WP-14 "re-verify-at-enable"). Empty ⇒
/// the closure is intact.
fn missing_requires_core(
    store: &Path,
    scope_roots: &[PathBuf],
    kind: Kind,
    name: &str,
) -> Vec<RequiresEntry> {
    let rf = registry::load(store);
    let Some(entry) = rf
        .entries
        .iter()
        .find(|e| e.kind == kind.as_str() && e.name == name)
    else {
        return Vec::new();
    };
    let satisfied = collect_satisfied(store, scope_roots);
    entry
        .requires
        .iter()
        .filter(|r| {
            !satisfied.contains(&PrimitiveRef {
                kind: r.kind.clone(),
                name: r.name.clone(),
            })
        })
        .cloned()
        .collect()
}

/// Install a primitive AND its forward-dependency closure (ADR-015 §3b / WP-14).
/// The FE passes a catalog snapshot so each dependency resolves to a fetchable
/// source; the missing closure auto-installs transactionally (rolled back with
/// the target on any failure). Returns the target + the installed closure
/// (enable order) + already-satisfied deps (consent UX).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_with_deps(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    source: String,
    url: String,
    gitRef: Option<String>,
    fromCatalog: Option<bool>,
    catalog: Vec<CatalogEntryRef>,
) -> Result<InstallWithDepsResult, String> {
    let k = Kind::parse(&kind)?;
    let src = parse_source(&source)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let scope_roots = super::all_scope_roots(&db).await;
    install_with_deps_core(
        &store,
        &scope_roots,
        k,
        &name,
        src,
        &url,
        gitRef.as_deref(),
        fromCatalog.unwrap_or(false),
        &catalog,
    )
}

/// Re-verify a primitive's `requires` at enable time: return the recorded deps
/// that are no longer present (the FE offers to re-fetch them). WP-14
/// re-verify-at-enable.
#[tauri::command]
pub async fn oba_missing_requires(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<Vec<RequiresEntry>, String> {
    let k = Kind::parse(&kind)?;
    validate_name(&name)?;
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let scope_roots = super::all_scope_roots(&db).await;
    Ok(missing_requires_core(&store, &scope_roots, k, &name))
}

// ─── WP-16: pkg-kernel → Ọba seam ────────────────────────────────────────────
// When an Ikenga pkg manifest declares `requires:[skill …]`, the pkg-install
// flow calls the Ọba resolver to satisfy the closure (ADR-015 §3c). The seam
// runs the SAME resolver-driven closure installer as `oba_install_with_deps`
// (a primitive's own deps) — one resolver, two trigger points. It then PLACES
// the freshly-installed members into the pkg's scope through `super::place_primitive`
// (the single materialization layer — WP-16 unify), the same fn the Ọba enable
// Tauri path (`claude_primitive_enable`) routes through. This freezes `G-RESOLVER`:
// forward resolution dispatches end-to-end via one placement layer.

/// Outcome of resolving an Ikenga pkg's manifest `requires` at install (WP-16).
#[derive(Debug, Serialize)]
pub struct PkgRequiresResult {
    /// Required primitives newly fetched into the store (topological enable order).
    pub installed: Vec<ClaudeStoreEntry>,
    /// Required primitives already present (store ∪ external) — no reinstall.
    pub already_satisfied: Vec<PrimitiveRef>,
    /// Scope placements created for the freshly-installed members (the unified
    /// `place_primitive` layer). Best-effort — a placement failure leaves the
    /// vault master intact (enable later) and is logged, not fatal.
    pub placed: Vec<ClaudeStoreMutation>,
}

/// WP-16 seam. Given a pkg's compiled manifest `requires`, install the missing
/// forward-dependency closure into the store (resolving each dep's `(source,url)`
/// from the `catalog` snapshot) and place the freshly-installed members into the
/// pkg's `scope` via the unified placement layer. Dedups vs store ∪ external.
/// Empty `requires` ⇒ a no-op (returns empty result). Used by `pkg_install_*`.
pub async fn resolve_pkg_requires(
    db: &Arc<PaDb>,
    requires: &[RequiresEntry],
    catalog: &[CatalogEntryRef],
    scope: &str,
) -> Result<PkgRequiresResult, String> {
    if requires.is_empty() {
        return Ok(PkgRequiresResult {
            installed: Vec::new(),
            already_satisfied: Vec::new(),
            placed: Vec::new(),
        });
    }
    let store = store_root().ok_or_else(|| "cannot resolve store root".to_string())?;
    let scope_roots = super::all_scope_roots(db).await;
    let (installed, already_satisfied) =
        install_requires_closure_core(&store, &scope_roots, requires, catalog)?;

    // Materialize the freshly-installed closure into the pkg's scope through the
    // ONE placement fn `claude_primitive_enable` also routes through (WP-16 unify).
    // Claude layout today; the per-engine adapters' physical delegation to this fn
    // (codex/gemini) is the deferred merge (freeze pending live-verify).
    let mut placed = Vec::new();
    match super::resolve_scope_claude(db, scope).await {
        Ok(scope_claude) => {
            for e in &installed {
                let Ok(k) = Kind::parse(&e.kind) else { continue };
                match super::place_primitive(&store, &scope_claude, scope, k, &e.name) {
                    Ok(m) => placed.push(m),
                    Err(err) => tracing::warn!(
                        "[oba/wp-16] place {}:{} into {scope} failed (vault master kept): {err}",
                        e.kind,
                        e.name
                    ),
                }
            }
        }
        Err(err) => tracing::warn!(
            "[oba/wp-16] resolve scope {scope:?} for requires placement failed (vault masters kept): {err}"
        ),
    }

    Ok(PkgRequiresResult {
        installed,
        already_satisfied,
        placed,
    })
}

// ─── Tests (git path: offline, deterministic against local file:// repos) ─────

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // ── WP-14 — resolver-driven install (real install_core + rollback) ──────────

    /// A source repo holding a skill-at-root + a `manifest.json` carrying
    /// `requires` (the published-artifact shape the resolver reads).
    fn make_skill_repo_with_requires(
        base: &Path,
        dir: &str,
        skill_name: &str,
        requires_json: &str,
    ) -> String {
        let repo = base.join(dir);
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q"], &repo);
        git(&["config", "user.email", "t@t"], &repo);
        git(&["config", "user.name", "t"], &repo);
        std::fs::write(
            repo.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: d\n---\nbody"),
        )
        .unwrap();
        std::fs::write(
            repo.join("manifest.json"),
            format!("{{\"requires\":{requires_json}}}"),
        )
        .unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-q", "-m", "v1"], &repo);
        format!("file://{}", repo.display())
    }

    #[test]
    fn install_with_deps_auto_installs_the_closure() {
        let base = unique_tmp("wd_closure");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // target requires child; child requires nothing.
        let target_url = make_skill_repo_with_requires(
            &base,
            "parent",
            "parent",
            r#"[{"kind":"skill","name":"child"}]"#,
        );
        let child_url = make_skill_repo_with_requires(&base, "child", "child", "[]");

        let catalog = vec![CatalogEntryRef {
            kind: "skill".into(),
            name: "child".into(),
            source: "git".into(),
            url: child_url,
            ..Default::default()
        }];

        let res = install_with_deps_core(
            &store,
            &[],
            Kind::Skill,
            "parent",
            ProvenanceSource::Git,
            &target_url,
            None,
            true,
            &catalog,
        )
        .expect("install with deps ok");

        assert_eq!(res.target.name, "parent");
        // the child was auto-installed (closure)
        assert_eq!(res.installed.len(), 1);
        assert_eq!(res.installed[0].name, "child");
        // both canonicals on disk
        assert!(store_path_for(&store, Kind::Skill, "parent")
            .unwrap()
            .join("SKILL.md")
            .is_file());
        assert!(store_path_for(&store, Kind::Skill, "child")
            .unwrap()
            .join("SKILL.md")
            .is_file());
        // both recorded; target carries its compiled requires
        let rf = registry::load(&store);
        let parent = rf.entries.iter().find(|e| e.name == "parent").unwrap();
        assert_eq!(parent.requires.len(), 1);
        assert_eq!(parent.requires[0].name, "child");
        assert!(rf.entries.iter().any(|e| e.name == "child"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_with_deps_skips_already_satisfied_dep() {
        let base = unique_tmp("wd_satisfied");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // pre-install the child directly so it is already satisfied
        let child_url = make_skill_repo_with_requires(&base, "child", "child", "[]");
        install_core(
            &store,
            Kind::Skill,
            "child",
            ProvenanceSource::Git,
            &child_url,
            None,
            false,
        )
        .unwrap();

        let target_url = make_skill_repo_with_requires(
            &base,
            "parent",
            "parent",
            r#"[{"kind":"skill","name":"child"}]"#,
        );
        let catalog = vec![CatalogEntryRef {
            kind: "skill".into(),
            name: "child".into(),
            source: "git".into(),
            url: child_url,
            ..Default::default()
        }];

        let res = install_with_deps_core(
            &store,
            &[],
            Kind::Skill,
            "parent",
            ProvenanceSource::Git,
            &target_url,
            None,
            true,
            &catalog,
        )
        .unwrap();
        // child already present → not reinstalled, surfaced as already-satisfied
        assert!(res.installed.is_empty(), "no reinstall of satisfied dep");
        assert!(res.already_satisfied.iter().any(|p| p.name == "child"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_with_deps_rolls_back_target_on_dep_failure() {
        let base = unique_tmp("wd_rollback");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let target_url = make_skill_repo_with_requires(
            &base,
            "parent",
            "parent",
            r#"[{"kind":"skill","name":"child"}]"#,
        );
        // child repo has NO SKILL.md → install_core fails to locate → dep failure
        let bad = base.join("badchild");
        std::fs::create_dir_all(&bad).unwrap();
        git(&["init", "-q"], &bad);
        git(&["config", "user.email", "t@t"], &bad);
        git(&["config", "user.name", "t"], &bad);
        std::fs::write(bad.join("README.md"), "nothing").unwrap();
        git(&["add", "-A"], &bad);
        git(&["commit", "-q", "-m", "x"], &bad);
        let catalog = vec![CatalogEntryRef {
            kind: "skill".into(),
            name: "child".into(),
            source: "git".into(),
            url: format!("file://{}", bad.display()),
            ..Default::default()
        }];

        let err = install_with_deps_core(
            &store,
            &[],
            Kind::Skill,
            "parent",
            ProvenanceSource::Git,
            &target_url,
            None,
            true,
            &catalog,
        )
        .unwrap_err();
        assert!(
            err.contains("failed installing dependency") || err.contains("no SKILL.md"),
            "dep failure surfaced: {err}"
        );
        // target rolled back: no canonical, registry empty (full rollback)
        assert!(!store_path_for(&store, Kind::Skill, "parent")
            .unwrap()
            .exists());
        assert!(
            registry::load(&store).entries.is_empty(),
            "full rollback leaves no records"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    // ── WP-16 — pkg-kernel → Ọba seam (closure install over a `requires` list) ──

    #[test]
    fn pkg_requires_closure_installs_over_a_requires_list() {
        // The seam (`resolve_pkg_requires`) drives `install_requires_closure_core`
        // directly over a pkg manifest's `requires` — there is NO target primitive
        // to fetch first (the pkg is not a store primitive). Transitive closure
        // (a → b) must install both, deepest-first.
        let base = unique_tmp("pkg_req_closure");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let a_url =
            make_skill_repo_with_requires(&base, "a", "a", r#"[{"kind":"skill","name":"b"}]"#);
        let b_url = make_skill_repo_with_requires(&base, "b", "b", "[]");
        let catalog = vec![
            CatalogEntryRef {
                kind: "skill".into(),
                name: "a".into(),
                source: "git".into(),
                url: a_url,
                ..Default::default()
            },
            CatalogEntryRef {
                kind: "skill".into(),
                name: "b".into(),
                source: "git".into(),
                url: b_url,
                ..Default::default()
            },
        ];
        // pkg requires only `a`; `a` transitively requires `b`.
        let pkg_requires = vec![RequiresEntry {
            kind: "skill".into(),
            name: "a".into(),
            source: None,
            r#ref: None,
        }];

        let (installed, already_satisfied) =
            install_requires_closure_core(&store, &[], &pkg_requires, &catalog)
                .expect("closure install ok");

        // Both a and b installed; topological (enable) order = deepest first (b, a).
        let names: Vec<&str> = installed.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["b", "a"], "topological enable order");
        assert!(already_satisfied.is_empty());
        assert!(store_path_for(&store, Kind::Skill, "a")
            .unwrap()
            .join("SKILL.md")
            .is_file());
        assert!(store_path_for(&store, Kind::Skill, "b")
            .unwrap()
            .join("SKILL.md")
            .is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn pkg_requires_closure_dedups_already_present() {
        // A required primitive already in the store is surfaced as already-satisfied
        // and NOT reinstalled — the no-double-install guarantee for the pkg seam.
        let base = unique_tmp("pkg_req_dedup");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let dep_url = make_skill_repo_with_requires(&base, "dep", "dep", "[]");
        install_core(
            &store,
            Kind::Skill,
            "dep",
            ProvenanceSource::Git,
            &dep_url,
            None,
            false,
        )
        .unwrap();
        let catalog = vec![CatalogEntryRef {
            kind: "skill".into(),
            name: "dep".into(),
            source: "git".into(),
            url: dep_url,
            ..Default::default()
        }];
        let pkg_requires = vec![RequiresEntry {
            kind: "skill".into(),
            name: "dep".into(),
            source: None,
            r#ref: None,
        }];

        let (installed, already_satisfied) =
            install_requires_closure_core(&store, &[], &pkg_requires, &catalog)
                .expect("closure ok");
        assert!(installed.is_empty(), "satisfied dep not reinstalled");
        assert!(already_satisfied.iter().any(|p| p.name == "dep"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_core_records_compiled_requires_from_manifest() {
        let base = unique_tmp("wd_reqrecord");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let url = make_skill_repo_with_requires(
            &base,
            "solo",
            "solo",
            r#"[{"kind":"skill","name":"skill-core"}]"#,
        );
        let entry = install_core(
            &store,
            Kind::Skill,
            "solo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .unwrap();
        assert_eq!(entry.requires.len(), 1);
        assert_eq!(entry.requires[0].name, "skill-core");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_requires_core_reports_removed_dep() {
        let base = unique_tmp("wd_missing");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let url = make_skill_repo_with_requires(
            &base,
            "solo",
            "solo",
            r#"[{"kind":"skill","name":"child"}]"#,
        );
        install_core(
            &store,
            Kind::Skill,
            "solo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .unwrap();
        // child never installed → reported missing
        let missing = missing_requires_core(&store, &[], Kind::Skill, "solo");
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "child");
        std::fs::remove_dir_all(&base).ok();
    }

    fn unique_tmp(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("oba_install_test_{tag}_{nonce}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn git(args: &[&str], cwd: &Path) {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Create a bare-ish source git repo holding one skill named `name`, return
    /// its `file://` URL.
    fn make_skill_repo(base: &Path, name: &str, body: &str) -> (PathBuf, String) {
        let repo = base.join("src-repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q"], &repo);
        git(&["config", "user.email", "t@t"], &repo);
        git(&["config", "user.name", "t"], &repo);
        // Skill-at-root layout: SKILL.md + a helper at the repo root.
        std::fs::write(
            repo.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\nbody"),
        )
        .unwrap();
        std::fs::write(repo.join("helper.py"), "print(1)\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-q", "-m", "v1"], &repo);
        let url = format!("file://{}", repo.display());
        (repo, url)
    }

    #[test]
    fn install_git_creates_canonical_and_records_provenance() {
        let base = unique_tmp("install_git");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (_repo, url) = make_skill_repo(&base, "demo", "a demo skill");

        let entry = install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .expect("install ok");

        // canonical landed at store/skills/demo with the content (no .git)
        let canon = store_path_for(&store, Kind::Skill, "demo").unwrap();
        assert!(canon.join("SKILL.md").is_file());
        assert!(canon.join("helper.py").is_file());
        assert!(
            !canon.join(".git").exists(),
            ".git must not be copied into the vault"
        );

        // provenance recorded: git source, resolved SHA, managed
        assert_eq!(entry.provenance.source, ProvenanceSource::Git);
        assert!(entry.provenance.managed);
        let sha = entry.provenance.version.clone().expect("sha");
        assert_eq!(sha.len(), 40, "git SHA");

        // and it persisted to registry.json
        let rf = registry::load(&store);
        let rec = rf
            .entries
            .iter()
            .find(|e| e.name == "demo")
            .expect("recorded");
        assert_eq!(rec.provenance.version.as_deref(), Some(sha.as_str()));
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(unix)]
    #[test]
    fn check_update_and_update_swap_in_place_without_relink() {
        let base = unique_tmp("update_git");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (repo, url) = make_skill_repo(&base, "demo", "v1 desc");

        install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .unwrap();
        let canon = store_path_for(&store, Kind::Skill, "demo").unwrap();

        // a live dependent symlink into the canonical (the placement)
        let scope = base.join("proj").join(".claude").join("skills");
        std::fs::create_dir_all(&scope).unwrap();
        let link = scope.join("demo");
        std::os::unix::fs::symlink(&canon, &link).unwrap();
        assert!(link.join("SKILL.md").is_file(), "link resolves pre-update");

        // up to date
        let st = check_update_core(&store, Kind::Skill, "demo").unwrap();
        assert!(!st.behind, "freshly installed = not behind");

        // advance the source with a new commit (new file)
        std::fs::write(repo.join("NEW.md"), "added\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-q", "-m", "v2"], &repo);

        let st2 = check_update_core(&store, Kind::Skill, "demo").unwrap();
        assert!(st2.behind, "source advanced → behind");
        assert_ne!(st2.current, st2.latest);

        let updated = update_core(&store, Kind::Skill, "demo").unwrap();
        assert_eq!(
            updated.provenance.version, st2.latest,
            "version bumped to latest"
        );
        // refreshed content is visible THROUGH the unchanged symlink (no relink)
        assert!(
            link.join("NEW.md").is_file(),
            "symlink resolves to refreshed files"
        );
        // not behind anymore
        let st3 = check_update_core(&store, Kind::Skill, "demo").unwrap();
        assert!(!st3.behind);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn update_refuses_external_master() {
        let base = unique_tmp("update_external");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // record an external (managed:false) master
        let mut rf = registry::load(&store);
        registry::upsert_external(&mut rf, "skill", "ext", "/elsewhere/ext");
        registry::save(&store, &rf).unwrap();

        let err = update_core(&store, Kind::Skill, "ext").unwrap_err();
        assert!(
            err.contains("external master"),
            "must refuse external: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn locate_installed_skill_finds_agents_skills_layout() {
        // The `skills` CLI writes to a CWD-relative `.agents/skills/<name>`.
        let base = unique_tmp("locate_agents");
        let dir = base.join(".agents").join("skills").join("groundwork");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: groundwork\n---\nx").unwrap();
        let found = locate_installed_skill(&base, "groundwork").expect("found in .agents/skills");
        assert_eq!(found, dir);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_rejects_json_fragment_kinds() {
        let base = unique_tmp("reject_hook");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let err = install_core(
            &store,
            Kind::Hook,
            "x",
            ProvenanceSource::Git,
            "u",
            None,
            false,
        )
        .unwrap_err();
        assert!(
            err.contains("JSON-fragment"),
            "hook install must be rejected: {err}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_git_missing_skill_leaves_no_canonical_no_record() {
        let base = unique_tmp("install_missing");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // a repo with NO SKILL.md → locate fails after clone
        let repo = base.join("empty-repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q"], &repo);
        git(&["config", "user.email", "t@t"], &repo);
        git(&["config", "user.name", "t"], &repo);
        std::fs::write(repo.join("README.md"), "nothing here\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-q", "-m", "x"], &repo);
        let url = format!("file://{}", repo.display());

        let err = install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("no SKILL.md"), "should fail to locate: {err}");
        // no canonical, no registry mutation
        assert!(!store_path_for(&store, Kind::Skill, "demo")
            .unwrap()
            .exists());
        assert!(registry::load(&store).entries.is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    // ─── Phase 3 — catalog discovery origin + auto-update trust policy ────────

    #[test]
    fn catalog_install_records_from_catalog_and_auto_update_on() {
        let base = unique_tmp("catalog_install");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (_repo, url) = make_skill_repo(&base, "demo", "a demo skill");

        // a catalog-discovered install (from_catalog = true) still resolves via git
        let entry = install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            true,
        )
        .expect("install ok");

        // dispatch axis unchanged: the resolved mechanism stays Git
        assert_eq!(entry.provenance.source, ProvenanceSource::Git);
        // discovery origin recorded orthogonally
        assert!(entry.provenance.from_catalog, "catalog discovery recorded");
        // curated-catalog installs opt into auto-update
        assert!(entry.provenance.auto_update, "catalog → auto_update on");

        // persisted to registry.json (survives a reload)
        let rec = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "demo")
            .expect("recorded");
        assert!(rec.provenance.from_catalog);
        assert!(rec.provenance.auto_update);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn plain_git_install_is_not_catalog_discovered_and_auto_update_off() {
        let base = unique_tmp("plain_install");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (_repo, url) = make_skill_repo(&base, "demo", "a demo skill");

        // a direct (non-catalog) git install
        let entry = install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .expect("install ok");

        assert!(
            !entry.provenance.from_catalog,
            "direct install is NOT catalog-discovered"
        );
        assert!(
            !entry.provenance.auto_update,
            "manual install → auto_update off (catalog ON, manual OFF)"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn update_preserves_from_catalog_and_auto_update() {
        let base = unique_tmp("update_preserves");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (repo, url) = make_skill_repo(&base, "demo", "v1");

        install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            true,
        )
        .unwrap();

        // advance the source
        std::fs::write(repo.join("NEW.md"), "added\n").unwrap();
        git(&["add", "-A"], &repo);
        git(&["commit", "-q", "-m", "v2"], &repo);

        let updated = update_core(&store, Kind::Skill, "demo").unwrap();
        assert!(
            updated.provenance.from_catalog,
            "update preserves catalog origin"
        );
        assert!(
            updated.provenance.auto_update,
            "update preserves auto_update opt-in"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn set_auto_update_round_trips_through_registry() {
        let base = unique_tmp("set_auto");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let (_repo, url) = make_skill_repo(&base, "demo", "a demo skill");

        // plain install → auto_update off
        install_core(
            &store,
            Kind::Skill,
            "demo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .unwrap();
        let before = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "demo")
            .unwrap();
        assert!(!before.provenance.auto_update);

        // flip it on — persists
        assert!(set_auto_update_core(&store, Kind::Skill, "demo", true).unwrap());
        let on = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "demo")
            .unwrap();
        assert!(on.provenance.auto_update, "persisted on");

        // flip it back off — persists
        assert!(!set_auto_update_core(&store, Kind::Skill, "demo", false).unwrap());
        let off = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "demo")
            .unwrap();
        assert!(!off.provenance.auto_update, "persisted off");

        // absent entry errors
        let err = set_auto_update_core(&store, Kind::Skill, "ghost", true).unwrap_err();
        assert!(err.contains("not in registry"), "absent → error: {err}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn auto_update_all_updates_only_stale_auto_update_entries_and_survives_errors() {
        let base = unique_tmp("auto_all");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // (1) a catalog (auto_update) skill that WILL go stale
        let (stale_repo, stale_url) = {
            let repo = base.join("stale-repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&["init", "-q"], &repo);
            git(&["config", "user.email", "t@t"], &repo);
            git(&["config", "user.name", "t"], &repo);
            std::fs::write(repo.join("SKILL.md"), "---\nname: stale\n---\nv1").unwrap();
            git(&["add", "-A"], &repo);
            git(&["commit", "-q", "-m", "v1"], &repo);
            let url = format!("file://{}", repo.display());
            (repo, url)
        };
        install_core(
            &store,
            Kind::Skill,
            "stale",
            ProvenanceSource::Git,
            &stale_url,
            None,
            true, // catalog → auto_update on
        )
        .unwrap();

        // (2) a catalog (auto_update) skill that stays CURRENT
        let (_cur_repo, cur_url) = make_skill_repo(&base, "current", "stays current");
        install_core(
            &store,
            Kind::Skill,
            "current",
            ProvenanceSource::Git,
            &cur_url,
            None,
            true,
        )
        .unwrap();

        // (3) a plain (auto_update OFF) skill that is ALSO stale — must be skipped
        let (manual_repo, manual_url) = {
            let repo = base.join("manual-repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&["init", "-q"], &repo);
            git(&["config", "user.email", "t@t"], &repo);
            git(&["config", "user.name", "t"], &repo);
            std::fs::write(repo.join("SKILL.md"), "---\nname: manual\n---\nv1").unwrap();
            git(&["add", "-A"], &repo);
            git(&["commit", "-q", "-m", "v1"], &repo);
            let url = format!("file://{}", repo.display());
            (repo, url)
        };
        install_core(
            &store,
            Kind::Skill,
            "manual",
            ProvenanceSource::Git,
            &manual_url,
            None,
            false, // manual → auto_update off
        )
        .unwrap();

        // (4) an auto_update entry whose remote is GONE — check errors, batch survives
        {
            let mut rf = registry::load(&store);
            let mut prov = RegistryProvenance::local(
                store.join("skills/broken").to_string_lossy().to_string(),
            );
            prov.source = ProvenanceSource::Git;
            prov.url = Some(format!("file://{}/does-not-exist", base.display()));
            prov.managed = true;
            prov.auto_update = true;
            prov.from_catalog = true;
            registry::upsert_record(
                &mut rf,
                ClaudeStoreEntry {
                    kind: "skill".into(),
                    name: "broken".into(),
                    store_path: store.join("skills/broken").to_string_lossy().to_string(),
                    description: None,
                    modified_ms: 0,
                    enabled_in: vec![],
                    requires: vec![],
                    members: vec![],
                    provenance: prov,
                },
            );
            registry::save(&store, &rf).unwrap();
        }

        // now make `stale` actually behind
        std::fs::write(stale_repo.join("NEW.md"), "added\n").unwrap();
        git(&["add", "-A"], &stale_repo);
        git(&["commit", "-q", "-m", "v2"], &stale_repo);
        // (advance manual's source too — to prove it's skipped by policy, not by being current)
        std::fs::write(manual_repo.join("NEW.md"), "added\n").unwrap();
        git(&["add", "-A"], &manual_repo);
        git(&["commit", "-q", "-m", "v2"], &manual_repo);

        let summary = auto_update_all_core(&store);

        // exactly `stale` was updated
        assert_eq!(
            summary.updated.len(),
            1,
            "only the stale auto entry updates"
        );
        assert_eq!(summary.updated[0].name, "stale");
        // `current` reported current
        assert!(summary.current.iter().any(|r| r.name == "current"));
        // `broken` errored but did NOT abort the batch
        assert!(summary.errored.iter().any(|r| r.name == "broken"));
        // `manual` (auto_update off) was never touched — not in any bucket
        let all_names: Vec<&str> = summary
            .updated
            .iter()
            .chain(summary.current.iter())
            .chain(summary.errored.iter())
            .map(|r| r.name.as_str())
            .collect();
        assert!(
            !all_names.contains(&"manual"),
            "auto_update=false entry must be skipped entirely"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn legacy_registry_without_phase3_fields_deserializes_with_defaults() {
        let base = unique_tmp("legacy_serde");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        // a registry.json written before Phase 3 — no fromCatalog / autoUpdate keys
        let raw = format!(
            r#"{{"schemaVersion":1,"entries":[{{"kind":"skill","name":"x","storePath":"{p}","description":null,"modifiedMs":0,"enabledIn":[],"source":"git","url":"u","ref":null,"version":"sha","canonicalPath":"{p}","managed":true,"installedAt":"t","updatedAt":"t"}}]}}"#,
            p = store.join("skills/x").to_string_lossy()
        );
        std::fs::write(registry::registry_path(&store), raw).unwrap();
        let rf = registry::load(&store);
        assert_eq!(rf.entries.len(), 1);
        // the new fields default to false (back-compat)
        assert!(!rf.entries[0].provenance.from_catalog);
        assert!(!rf.entries[0].provenance.auto_update);
        // and the pre-existing fields still bind
        assert_eq!(rf.entries[0].provenance.source, ProvenanceSource::Git);
        assert_eq!(rf.entries[0].provenance.version.as_deref(), Some("sha"));
        std::fs::remove_dir_all(&base).ok();
    }

    // ─── WP-25 — local-source install (copy-staging, provenance `local`) ──────

    /// Build a local source dir holding a skill-at-root: `<dir>/SKILL.md` (+ a
    /// helper file). Returns the dir path. No git, no manifest — the simplest
    /// local skill the installer copies.
    fn make_local_skill_dir(base: &Path, dir: &str, skill_name: &str) -> PathBuf {
        let d = base.join(dir);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: a local skill\n---\nbody"),
        )
        .unwrap();
        std::fs::write(d.join("helper.py"), "print(1)\n").unwrap();
        d
    }

    #[test]
    fn install_local_creates_canonical_and_records_provenance_local() {
        let base = unique_tmp("install_local");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = make_local_skill_dir(&base, "src-mail", "mail");

        // install as `skill-mail` (store name differs from frontmatter `mail`)
        let entry = install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .expect("local install ok");

        // canonical landed at store/skills/skill-mail with the content (copied)
        let canon = store_path_for(&store, Kind::Skill, "skill-mail").unwrap();
        assert!(canon.ends_with("skills/skill-mail"));
        assert!(canon.join("SKILL.md").is_file());
        assert!(canon.join("helper.py").is_file());

        // provenance: local source, NO version (no remote), managed, path recorded
        assert_eq!(entry.provenance.source, ProvenanceSource::Local);
        assert!(entry.provenance.managed);
        assert!(
            entry.provenance.version.is_none(),
            "local install resolves no version (no remote)"
        );
        assert_eq!(entry.provenance.url.as_deref(), Some(src.to_string_lossy().as_ref()));
        // auto_update OFF for local (catalog ON, local/manual OFF)
        assert!(!entry.provenance.from_catalog);
        assert!(
            !entry.provenance.auto_update,
            "local install → auto_update off"
        );

        // the SOURCE tree is untouched (copy-only staging — the v2b data-loss guard)
        assert!(src.join("SKILL.md").is_file(), "source not moved");
        assert!(src.join("helper.py").is_file());

        // persisted to registry.json
        let rec = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "skill-mail")
            .expect("recorded");
        assert_eq!(rec.provenance.source, ProvenanceSource::Local);
        assert!(rec.provenance.version.is_none());
        assert!(!rec.provenance.auto_update);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_local_reads_requires_from_package_root_manifest() {
        // Mirror the ikenga-pkgs publish layout: package root has manifest.json
        // (carrying `requires`) and skills/<name>/SKILL.md two levels below. The
        // local install points at the SKILL.md dir; the requires walk-up finds the
        // manifest only when it lives on the path between located and boundary.
        // Here boundary == located (the SKILL.md dir), and we place the manifest
        // beside SKILL.md so it is read — proving the walk-up reads the located dir.
        let base = unique_tmp("install_local_requires");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = make_local_skill_dir(&base, "src-with-req", "mail");
        std::fs::write(
            src.join("manifest.json"),
            r#"{"requires":[{"kind":"skill","name":"skill-core"}]}"#,
        )
        .unwrap();

        let entry = install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .expect("local install ok");

        assert_eq!(entry.requires.len(), 1, "requires read from local manifest");
        assert_eq!(entry.requires[0].name, "skill-core");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_local_finds_skill_in_skills_subdir() {
        // Point the local install at a package root (NOT the SKILL.md dir): the
        // skill lives at `<root>/skills/<name>/`. `locate_in_local` must find it
        // via the `skills/<name>` candidate, and the requires walk-up reads the
        // package-root manifest.json.
        let base = unique_tmp("install_local_subdir");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let pkg_root = base.join("pkg-mail");
        let skill = pkg_root.join("skills").join("skill-mail");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: mail\n---\nbody").unwrap();
        std::fs::write(
            pkg_root.join("manifest.json"),
            r#"{"requires":[{"kind":"skill","name":"skill-core"}]}"#,
        )
        .unwrap();

        let entry = install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &pkg_root.to_string_lossy(),
            None,
            false,
        )
        .expect("local install from package root ok");

        let canon = store_path_for(&store, Kind::Skill, "skill-mail").unwrap();
        assert!(canon.join("SKILL.md").is_file());
        assert_eq!(entry.requires.len(), 1, "requires read from package-root manifest");
        assert_eq!(entry.requires[0].name, "skill-core");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_local_missing_path_leaves_no_canonical_no_record() {
        let base = unique_tmp("install_local_missing");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        let err = install_core(
            &store,
            Kind::Skill,
            "ghost",
            ProvenanceSource::Local,
            &base.join("does-not-exist").to_string_lossy(),
            None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("does not exist"), "clear error: {err}");
        assert!(!store_path_for(&store, Kind::Skill, "ghost")
            .unwrap()
            .exists());
        assert!(registry::load(&store).entries.is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_local_no_skill_md_errors() {
        let base = unique_tmp("install_local_noskill");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = base.join("empty-src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("README.md"), "nothing\n").unwrap();

        let err = install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .unwrap_err();
        assert!(err.contains("no SKILL.md"), "should fail to locate: {err}");
        assert!(registry::load(&store).entries.is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn check_update_and_update_refuse_local_entries() {
        let base = unique_tmp("local_no_update");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = make_local_skill_dir(&base, "src-local", "mail");
        install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .unwrap();

        let cerr = check_update_core(&store, Kind::Skill, "skill-mail").unwrap_err();
        assert!(cerr.contains("no remote"), "check refuses local: {cerr}");
        let uerr = update_core(&store, Kind::Skill, "skill-mail").unwrap_err();
        assert!(
            uerr.contains("local-source"),
            "update refuses local: {uerr}"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn parse_source_accepts_local() {
        assert_eq!(parse_source("local").unwrap(), ProvenanceSource::Local);
        assert_eq!(parse_source("git").unwrap(), ProvenanceSource::Git);
        assert!(parse_source("bogus").unwrap_err().contains("git|npx|local"));
    }

    #[test]
    fn install_local_round_trips_reinstall_idempotent() {
        // Re-installing from the same local path overwrites the canonical
        // atomically and leaves exactly one registry record (idempotent refresh).
        let base = unique_tmp("local_round_trip");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let src = make_local_skill_dir(&base, "src-rt", "mail");

        install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .unwrap();
        // advance the local source (new file) + reinstall
        std::fs::write(src.join("NEW.md"), "added\n").unwrap();
        let entry2 = install_core(
            &store,
            Kind::Skill,
            "skill-mail",
            ProvenanceSource::Local,
            &src.to_string_lossy(),
            None,
            false,
        )
        .unwrap();

        // the refreshed file is in the canonical; still exactly one record
        let canon = store_path_for(&store, Kind::Skill, "skill-mail").unwrap();
        assert!(canon.join("NEW.md").is_file(), "reinstall picks up new file");
        assert_eq!(entry2.provenance.source, ProvenanceSource::Local);
        let recs: Vec<_> = registry::load(&store)
            .entries
            .into_iter()
            .filter(|e| e.name == "skill-mail")
            .collect();
        assert_eq!(recs.len(), 1, "reinstall replaces, not appends");
        std::fs::remove_dir_all(&base).ok();
    }

    // ─── WP-19 — multi-skill BUNDLE install (store + registry only) ───────────

    /// Populate a staging dir the way `skills add --skill '*'` would: each
    /// `member` becomes `.agents/skills/<member>/SKILL.md`. This is exactly what
    /// the injected `fetch_fn` writes — so the bundle core is exercised with NO
    /// network and NO npx.
    fn make_bundle_staging(staging: &Path, members: &[&str]) -> Result<(), String> {
        let root = staging.join(".agents").join("skills");
        for m in members {
            let d = root.join(m);
            std::fs::create_dir_all(&d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
            std::fs::write(
                d.join("SKILL.md"),
                format!("---\nname: {m}\ndescription: member {m}\n---\nbody"),
            )
            .map_err(|e| format!("write SKILL.md: {e}"))?;
        }
        Ok(())
    }

    #[test]
    fn bundle_install_materializes_all_members_and_records_one_entry() {
        let base = unique_tmp("bundle_install");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // injected skills-add edge: writes 3 member skills, no network.
        let fetch = |_spec: &str, staging: &Path| make_bundle_staging(staging, &["gamma", "alpha", "beta"]);

        let entry = install_bundle_core(
            &store,
            "atelier",
            "ikenga-hq/atelier-bundle",
            false,
            &fetch,
        )
        .expect("bundle install ok");

        // ONE registry record, kind=bundle, members=[sorted leaves]
        assert_eq!(entry.kind, "bundle");
        assert_eq!(entry.name, "atelier");
        assert_eq!(
            entry.members,
            vec![
                "alpha".to_string(),
                "beta".to_string(),
                "gamma".to_string()
            ],
            "members are the sorted produced leaves"
        );
        assert_eq!(entry.provenance.source, ProvenanceSource::Npx);
        assert_eq!(
            entry.provenance.url.as_deref(),
            Some("ikenga-hq/atelier-bundle")
        );
        assert!(entry.provenance.managed);

        // store/bundles/atelier/<member>/SKILL.md for ALL N members
        let bundle_dir = store_path_for(&store, Kind::Bundle, "atelier").unwrap();
        assert!(bundle_dir.ends_with("bundles/atelier"));
        for m in ["alpha", "beta", "gamma"] {
            assert!(
                bundle_dir.join(m).join("SKILL.md").is_file(),
                "member {m} placed in the bundle dir"
            );
        }

        // persisted to registry.json as exactly one bundle record
        let rf = registry::load(&store);
        let recs: Vec<_> = rf.entries.iter().filter(|e| e.kind == "bundle").collect();
        assert_eq!(recs.len(), 1, "exactly one bundle record");
        assert_eq!(
            recs[0].members,
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            "registry round-trip preserves the exact sorted member set"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn bundle_update_swaps_in_place_and_rederives_members() {
        let base = unique_tmp("bundle_update");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // v1: alpha + beta + gamma
        let fetch_v1 =
            |_s: &str, staging: &Path| make_bundle_staging(staging, &["alpha", "beta", "gamma"]);
        install_bundle_core(&store, "kit", "owner/kit", true, &fetch_v1)
            .expect("install v1 ok");

        let bundle_dir = store_path_for(&store, Kind::Bundle, "kit").unwrap();
        assert!(bundle_dir.join("gamma").join("SKILL.md").is_file());
        // catalog install → auto_update on
        let v1 = registry::load(&store)
            .entries
            .into_iter()
            .find(|e| e.name == "kit")
            .unwrap();
        assert!(v1.provenance.from_catalog);
        assert!(v1.provenance.auto_update);

        // v2 upstream: gamma dropped, delta added (alpha + beta + delta)
        let fetch_v2 =
            |_s: &str, staging: &Path| make_bundle_staging(staging, &["alpha", "beta", "delta"]);
        let updated = update_bundle_core(&store, "kit", &fetch_v2).expect("update ok");

        // members re-derived: delta present, gamma gone
        assert_eq!(
            updated.members,
            vec![
                "alpha".to_string(),
                "beta".to_string(),
                "delta".to_string()
            ],
            "member set re-derived on whole-bundle re-fetch"
        );
        // on disk: the bundle dir was atomically swapped — delta exists, gamma removed
        assert!(bundle_dir.join("delta").join("SKILL.md").is_file());
        assert!(
            !bundle_dir.join("gamma").exists(),
            "dropped upstream member is gone after the atomic swap"
        );
        // still exactly one registry record; preserved auto_update opt-in
        let recs: Vec<_> = registry::load(&store)
            .entries
            .into_iter()
            .filter(|e| e.kind == "bundle")
            .collect();
        assert_eq!(recs.len(), 1, "update replaces, not appends");
        assert_eq!(
            recs[0].members,
            vec!["alpha".to_string(), "beta".to_string(), "delta".to_string()],
            "post-update members re-derived + persisted (gamma dropped, delta added)"
        );
        assert!(recs[0].provenance.auto_update, "auto_update preserved across update");
        assert!(recs[0].provenance.from_catalog, "from_catalog preserved across update");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn bundle_install_does_not_regress_single_skill_install() {
        // A single-skill git install run ALONGSIDE a bundle install: the
        // single-skill path stays a plain skill canonical (no members), and the
        // bundle path stays a bundle (with members). Neither leaks into the other.
        let base = unique_tmp("bundle_no_regress");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // single-skill install (unchanged path)
        let (_repo, url) = make_skill_repo(&base, "solo", "a solo skill");
        let skill = install_core(
            &store,
            Kind::Skill,
            "solo",
            ProvenanceSource::Git,
            &url,
            None,
            false,
        )
        .expect("single-skill install ok");
        assert_eq!(skill.kind, "skill");
        assert!(skill.members.is_empty(), "single skill records no members");
        assert!(store_path_for(&store, Kind::Skill, "solo")
            .unwrap()
            .join("SKILL.md")
            .is_file());

        // bundle install (new path)
        let fetch = |_s: &str, staging: &Path| make_bundle_staging(staging, &["one", "two"]);
        let bundle = install_bundle_core(&store, "pack", "owner/pack", false, &fetch)
            .expect("bundle install ok");
        assert_eq!(bundle.kind, "bundle");
        assert_eq!(bundle.members, vec!["one".to_string(), "two".to_string()]);

        // both records coexist; the single-skill record is untouched + still has no members
        let rf = registry::load(&store);
        let solo = rf.entries.iter().find(|e| e.name == "solo").unwrap();
        assert_eq!(solo.kind, "skill");
        assert!(solo.members.is_empty(), "single-skill record unchanged");
        let pack = rf.entries.iter().find(|e| e.name == "pack").unwrap();
        assert_eq!(pack.kind, "bundle");
        assert_eq!(pack.members.len(), 2);

        // single-skill canonical lives under skills/, bundle under bundles/
        assert!(store.join("skills").join("solo").join("SKILL.md").is_file());
        assert!(store
            .join("bundles")
            .join("pack")
            .join("one")
            .join("SKILL.md")
            .is_file());
        std::fs::remove_dir_all(&base).ok();
    }

    // ─── WP-20 — resolver bundle-expansion (install_dep bundle dispatch) ──────
    //
    // Tests that:
    //   (a) requires:{kind:bundle} resolves → the closure includes the bundle,
    //       install dispatches to the bundle path, members surfaced from catalog.
    //   (b) An already-present bundle is deduped (not re-installed).
    //   (c) Single-skill resolution behavior is UNCHANGED (regression guard).

    #[test]
    fn already_present_bundle_is_deduped_not_reinstalled() {
        // DoD (b): a bundle whose registry record already exists is seen as
        // satisfied by collect_satisfied and pruned from the closure — NOT
        // re-installed. (The fresh-dispatch path — DoD (a) — is proven by
        // `install_dep_dispatches_absent_bundle_to_bundle_path` below, which
        // drives `install_dep_with` against an ABSENT bundle.)
        let base = unique_tmp("wp20_dedup_bundle");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // Pre-seed the bundle (simulates what install_dep would have done on a
        // first install).
        let fetch =
            |_s: &str, staging: &Path| make_bundle_staging(staging, &["alpha", "beta"]);
        install_bundle_core(&store, "studio-archetypes", "owner/studio-arc", false, &fetch)
            .expect("pre-seed bundle");

        // The bundle record is now in the registry → collect_satisfied will include it.
        let requires = vec![RequiresEntry {
            kind: "bundle".into(),
            name: "studio-archetypes".into(),
            source: None,
            r#ref: None,
        }];
        // catalog has the bundle entry (members list mirrors what was installed)
        let catalog = vec![CatalogEntryRef {
            kind: "bundle".into(),
            name: "studio-archetypes".into(),
            source: "npx".into(),
            url: "owner/studio-arc".into(),
            members: vec!["alpha".into(), "beta".into()],
        }];

        let (installed, already_satisfied) =
            install_requires_closure_core(&store, &[], &requires, &catalog)
                .expect("closure ok for already-present bundle");

        // Bundle already present → deduped, NOT re-installed.
        assert!(
            installed.is_empty(),
            "already-present bundle must not be re-installed; installed={installed:?}"
        );
        assert!(
            already_satisfied.iter().any(|p| p.kind == "bundle" && p.name == "studio-archetypes"),
            "already-present bundle surfaced in already_satisfied"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn install_dep_dispatches_absent_bundle_to_bundle_path() {
        // DoD (a): for an ABSENT requires:{kind:bundle}, install_dep must DISPATCH
        // to the bundle path (install_bundle_core), pass the CATALOG url as the
        // skills-add spec (not item.name), and materialize the members. Driven via
        // the injectable `install_dep_with` so the dispatch runs offline.
        let base = unique_tmp("wp20_bundle_dispatch");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        let item = RequiresEntry {
            kind: "bundle".into(),
            name: "studio-archetypes".into(),
            source: None,
            r#ref: None,
        };
        let catalog = vec![CatalogEntryRef {
            kind: "bundle".into(),
            name: "studio-archetypes".into(),
            source: "npx".into(),
            url: "ikenga-hq/studio-archetypes".into(),
            members: vec!["alpha".into(), "beta".into()],
        }];

        // Fake skills-add edge: asserts the spec passed in is the CATALOG url
        // (proving the dispatch reads cat.url, not item.name), then writes 2 members.
        let fetch = |spec: &str, staging: &Path| {
            assert_eq!(
                spec, "ikenga-hq/studio-archetypes",
                "bundle dispatch must pass the catalog url as the skills-add spec"
            );
            make_bundle_staging(staging, &["beta", "alpha"])
        };

        let revealed =
            install_dep_with(&store, &catalog, &item, &fetch).expect("bundle dispatch ok");
        assert!(revealed.is_empty(), "a bundle reveals no requires of its own");

        // Members materialized under store/bundles/<name>/<member>/.
        let bundle_dir = store_path_for(&store, Kind::Bundle, "studio-archetypes").unwrap();
        assert!(bundle_dir.join("alpha").join("SKILL.md").is_file());
        assert!(bundle_dir.join("beta").join("SKILL.md").is_file());

        // Exactly one bundle registry record, members = sorted leaves.
        let recs: Vec<_> = registry::load(&store)
            .entries
            .into_iter()
            .filter(|e| e.kind == "bundle")
            .collect();
        assert_eq!(recs.len(), 1, "one bundle record after dispatch");
        assert_eq!(recs[0].name, "studio-archetypes");
        assert_eq!(
            recs[0].members,
            vec!["alpha".to_string(), "beta".to_string()],
            "dispatch materialized + recorded the sorted member set"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn bundle_requires_alongside_skill_requires_resolves_correctly() {
        // A pkg requires BOTH a skill (via git) and a bundle (pre-seeded).
        // The skill is absent → installed; the bundle is present → deduped.
        let base = unique_tmp("wp20_mix");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        // Pre-seed the bundle.
        let fetch = |_s: &str, staging: &Path| make_bundle_staging(staging, &["m1", "m2"]);
        install_bundle_core(&store, "my-bundle", "owner/my-bundle", false, &fetch)
            .expect("pre-seed bundle");

        // The skill dep comes from a git repo.
        let skill_url =
            make_skill_repo_with_requires(&base, "dep-skill", "dep-skill", "[]");

        let requires = vec![
            RequiresEntry {
                kind: "bundle".into(),
                name: "my-bundle".into(),
                source: None,
                r#ref: None,
            },
            RequiresEntry {
                kind: "skill".into(),
                name: "dep-skill".into(),
                source: None,
                r#ref: None,
            },
        ];
        let catalog = vec![
            CatalogEntryRef {
                kind: "bundle".into(),
                name: "my-bundle".into(),
                source: "npx".into(),
                url: "owner/my-bundle".into(),
                members: vec!["m1".into(), "m2".into()],
            },
            CatalogEntryRef {
                kind: "skill".into(),
                name: "dep-skill".into(),
                source: "git".into(),
                url: skill_url,
                ..Default::default()
            },
        ];

        let (installed, already_satisfied) =
            install_requires_closure_core(&store, &[], &requires, &catalog)
                .expect("closure ok");

        // skill installed (absent), bundle deduped (present)
        assert_eq!(installed.len(), 1, "only the skill dep is new");
        assert_eq!(installed[0].name, "dep-skill");
        assert!(
            already_satisfied.iter().any(|p| p.kind == "bundle" && p.name == "my-bundle"),
            "bundle deduped"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn single_skill_resolution_unchanged_after_wp20() {
        // Regression: the WP-20 bundle dispatch must not affect the existing
        // single-skill closure install path. Exercises the exact same path as
        // `pkg_requires_closure_installs_over_a_requires_list` — if that test
        // still passes this is belt-and-suspenders.
        let base = unique_tmp("wp20_skill_regress");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();

        let skill_url = make_skill_repo_with_requires(&base, "sk", "sk", "[]");
        let catalog = vec![CatalogEntryRef {
            kind: "skill".into(),
            name: "sk".into(),
            source: "git".into(),
            url: skill_url,
            ..Default::default()
        }];
        let requires = vec![RequiresEntry {
            kind: "skill".into(),
            name: "sk".into(),
            source: None,
            r#ref: None,
        }];

        let (installed, already_satisfied) =
            install_requires_closure_core(&store, &[], &requires, &catalog)
                .expect("single-skill closure ok after WP-20");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "sk");
        assert!(already_satisfied.is_empty());
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn bundle_collect_errors_when_staging_has_no_skills() {
        // The injected fetch produced nothing → install fails, no canonical, no record.
        let base = unique_tmp("bundle_empty");
        let store = base.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let fetch = |_s: &str, _staging: &Path| Ok::<(), String>(()); // writes nothing
        let err = install_bundle_core(&store, "empty", "owner/empty", false, &fetch).unwrap_err();
        assert!(
            err.contains("no member skills"),
            "empty staging surfaces a clear error: {err}"
        );
        assert!(!store_path_for(&store, Kind::Bundle, "empty")
            .unwrap()
            .exists());
        assert!(
            registry::load(&store)
                .entries
                .iter()
                .all(|e| e.kind != "bundle"),
            "no bundle record written on failure"
        );
        std::fs::remove_dir_all(&base).ok();
    }
}
