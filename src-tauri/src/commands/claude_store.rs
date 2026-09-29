//! Ngwa central store (Ọba) + symlink farm — the `#[tauri::command]` wrappers.
//!
//! The store, the symlink farm, the merge engines, the registry, the resolver
//! and the installer live in the ungated `server::shared::claude_store`
//! (WP-19 slice 7), so the daemon's `/api/rpc` arms run the very same bodies.
//! Everything is re-exported here, so `crate::commands::claude_store::*` paths
//! (ngwa, pkg, scaffold, the iyke bridge, tests) keep resolving unchanged.
//!
//! Each wrapper keeps its Tauri signature — the parameter names are the
//! frontend's argument names — and calls the shared `*_in` body with
//! [`Vault::desktop`]: the process home, `store_root()`, and symlinks
//! followed as the OS resolves them, exactly as before the move.

use std::sync::Arc;

use tauri::State;

pub use crate::server::shared::claude_store::*;

use crate::commands::db::PaDb;
use crate::pkg::manifest::RequiresEntry;
use crate::server::shared::claude_store::install;

/// List the central-store catalog. Optionally filter by kind. `enabledIn` is
/// populated by probing the workspace scope plus every known project scope.
#[tauri::command]
pub async fn claude_store_list(
    db: State<'_, Arc<PaDb>>,
    kind: Option<String>,
) -> Result<Vec<ClaudeStoreEntry>, String> {
    claude_store_list_inner(db.inner(), kind).await
}

/// Import an on-disk primitive into the store as the new canonical copy.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_store_import(
    kind: String,
    name: String,
    sourcePath: String,
) -> Result<ClaudeStoreEntry, String> {
    claude_store_import_in(&Vault::desktop(), kind, name, sourcePath).await
}

/// Enable a store primitive in a scope (symlink-farm create for file-based;
/// merge-engine delegate for hook/mcp).
#[tauri::command]
pub async fn claude_primitive_enable(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<ClaudeStoreMutation, String> {
    claude_primitive_enable_in(&Vault::desktop(), &db, kind, name, scope).await
}

/// Disable a store primitive in a scope (drop the symlink for file-based;
/// merge-engine unmerge for hook/mcp).
#[tauri::command]
pub async fn claude_primitive_disable(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<(), String> {
    claude_primitive_disable_in(&Vault::desktop(), &db, kind, name, scope).await
}

/// Copy a primitive from one scope to another, leaving the source in place.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_copy(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    fromScope: String,
    toScope: String,
    overwrite: Option<bool>,
) -> Result<ClaudeStoreMutation, String> {
    claude_primitive_copy_in(
        &Vault::desktop(),
        &db,
        kind,
        name,
        fromScope,
        toScope,
        overwrite,
    )
    .await
}

/// Move a primitive from one scope to another (copy-then-remove-source).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_move(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    fromScope: String,
    toScope: String,
    overwrite: Option<bool>,
) -> Result<ClaudeStoreMutation, String> {
    claude_primitive_move_in(
        &Vault::desktop(),
        &db,
        kind,
        name,
        fromScope,
        toScope,
        overwrite,
    )
    .await
}

/// Remove a primitive from a single scope's `.claude/` (does NOT touch the
/// store canonical copy).
#[tauri::command]
pub async fn claude_primitive_remove(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    scope: String,
) -> Result<(), String> {
    claude_primitive_remove_in(&Vault::desktop(), &db, kind, name, scope).await
}

/// Enable a store primitive in a scope for a specific engine. File kinds symlink
/// / copy per engine; hook/mcp splice via the settings-embedded merge engine.
#[tauri::command]
pub async fn claude_primitive_enable_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<ClaudeStoreMutation, String> {
    claude_primitive_enable_for_in(&Vault::desktop(), &db, engine, kind, name, scope, hookFile)
        .await
}

/// Disable a store primitive in a scope for a specific engine. File kinds drop
/// the scope-local link/copy; hook/mcp unsplice via the merge engine. Store
/// untouched either way.
#[tauri::command]
pub async fn claude_primitive_disable_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<(), String> {
    claude_primitive_disable_for_in(&Vault::desktop(), &db, engine, kind, name, scope, hookFile)
        .await
}

/// Remove a primitive from a single scope for a specific engine (does NOT touch
/// the store canonical copy). For file kinds this is the scope-local delete; for
/// hook/mcp it is the scope-local unsplice (the store fragment survives).
#[tauri::command]
pub async fn claude_primitive_remove_for(
    db: State<'_, Arc<PaDb>>,
    engine: String,
    kind: String,
    name: String,
    scope: String,
    #[allow(non_snake_case)] hookFile: Option<String>,
) -> Result<(), String> {
    claude_primitive_remove_for_in(&Vault::desktop(), &db, engine, kind, name, scope, hookFile)
        .await
}

/// Copy (or move) one source primitive into N `(engine, scope)` destinations in
/// one batch (WP-24 / D-09). See `claude_primitive_copy_batch_in`.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn claude_primitive_copy_batch(
    db: State<'_, Arc<PaDb>>,
    fromEngine: String,
    kind: String,
    name: String,
    fromScope: String,
    destinations: Vec<NgwaCopyDestination>,
    #[allow(non_snake_case)] r#move: bool,
) -> Result<NgwaCopyBatchResult, String> {
    claude_primitive_copy_batch_in(
        &Vault::desktop(),
        &db,
        fromEngine,
        kind,
        name,
        fromScope,
        destinations,
        r#move,
    )
    .await
}

/// WP-04: live dependents of (kind, name)'s canonical master, for the UI's
/// dependents list. Computed fresh from disk — never a stored list.
#[tauri::command]
pub async fn oba_dependents(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<Vec<String>, String> {
    oba_dependents_in(&Vault::desktop(), &db, kind, name).await
}

/// WP-04: guarded delete of (kind, name)'s canonical master. Refuses external
/// masters and masters with live dependents; hard-deletes only a managed master
/// with zero dependents (the incident guardrail).
#[tauri::command]
pub async fn oba_safe_delete(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<SafeDeleteOutcome, String> {
    oba_safe_delete_in(&Vault::desktop(), &db, kind, name).await
}

/// WP-04: re-point dependent symlinks at a new master (relink-all), returning a
/// per-link result in request order. Used before forgetting an external master.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_relink_dependents(
    dependents: Vec<String>,
    newMaster: String,
) -> Result<Vec<RelinkRow>, String> {
    oba_relink_dependents_in(&Vault::desktop(), None, dependents, newMaster).await
}

/// WP-06: unlink one dependent placement (a symlink) by absolute path. Always
/// safe — `remove_file` never recurses into the master. Refuses a non-symlink.
#[tauri::command]
pub async fn oba_unlink_one(path: String) -> Result<bool, String> {
    oba_unlink_one_in(&Vault::desktop(), None, path).await
}

/// WP-06: drop the registry record for `(kind, name)` — provenance only.
/// Returns `true` iff a record existed.
#[tauri::command]
pub async fn oba_forget(kind: String, name: String) -> Result<bool, String> {
    oba_forget_in(&Vault::desktop(), kind, name)
}

/// WP-06: back-fill the registry with external masters discovered in the live
/// farm. Returns the number of external-master records added or updated.
#[tauri::command]
pub async fn oba_backfill_registry(db: State<'_, Arc<PaDb>>) -> Result<usize, String> {
    oba_backfill_registry_in(&Vault::desktop(), &db).await
}

// ─── Ọba installer (claude_store/install.rs) ─────────────────────────────────

/// Install a primitive from a git remote into the vault as a managed canonical.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_git(
    kind: String,
    name: String,
    url: String,
    gitRef: Option<String>,
    fromCatalog: Option<bool>,
    expectSha: Option<String>,
    expectHash: Option<String>,
) -> Result<ClaudeStoreEntry, String> {
    install::oba_install_git(kind, name, url, gitRef, fromCatalog, expectSha, expectHash).await
}

/// Install a primitive via the Claude `skills` CLI (`npx skills add <spec>`).
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_npx(
    kind: String,
    name: String,
    spec: String,
    fromCatalog: Option<bool>,
    expectSha: Option<String>,
    expectHash: Option<String>,
) -> Result<ClaudeStoreEntry, String> {
    install::oba_install_npx(kind, name, spec, fromCatalog, expectSha, expectHash).await
}

/// Install a primitive from a LOCAL path into the vault as a managed canonical
/// (WP-25). Called by the iyke bridge (`POST /iyke/oba/install-local`).
#[tauri::command]
pub async fn oba_install_local(
    kind: String,
    name: String,
    path: String,
) -> Result<ClaudeStoreEntry, String> {
    install::oba_install_local(kind, name, path).await
}

/// Install a multi-skill BUNDLE via the Claude `skills` CLI.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_install_bundle(
    name: String,
    spec: String,
    scope: Option<String>,
    fromCatalog: Option<bool>,
) -> Result<ClaudeStoreEntry, String> {
    install::oba_install_bundle(name, spec, scope, fromCatalog).await
}

/// Check whether a git/npx-installed primitive is behind its remote.
#[tauri::command]
pub async fn oba_check_update(kind: String, name: String) -> Result<UpdateStatus, String> {
    install::oba_check_update(kind, name).await
}

/// Re-fetch a managed primitive into its existing canonical in place (no relink).
/// R57 · N-C: `expectSha` / `expectHash` pin what is fetched.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_update(
    kind: String,
    name: String,
    expectSha: Option<String>,
    expectHash: Option<String>,
) -> Result<ClaudeStoreEntry, String> {
    install::oba_update(kind, name, expectSha, expectHash).await
}

/// Phase 3 — auto-update every `auto_update`-opted entry that's behind its remote.
/// R57 · Q3: `pins` are the signed catalog's pins (pinned installs follow them).
#[tauri::command]
pub async fn oba_auto_update_all(
    pins: Option<Vec<CatalogPin>>,
) -> Result<AutoUpdateSummary, String> {
    install::oba_auto_update_all(pins).await
}

/// R57 · N-B — dry-run resolve of a pasted git URL / `owner/repo` spec: fetch
/// into staging, infer the kind, report `{kind, name, sha, hash, files,
/// requires, trust}`; writes nothing to the vault.
#[tauri::command]
#[allow(non_snake_case)]
pub async fn oba_resolve_source(
    url: String,
    kind: Option<String>,
    name: Option<String>,
    gitRef: Option<String>,
) -> Result<ResolvedSource, String> {
    install::oba_resolve_source(url, kind, name, gitRef).await
}

/// Phase 3 — toggle the per-entry auto-update opt-in and persist it to
/// `registry.json`. Returns the new flag value.
#[tauri::command]
pub async fn oba_set_auto_update(
    kind: String,
    name: String,
    enabled: bool,
) -> Result<bool, String> {
    install::oba_set_auto_update_in(&Vault::desktop(), kind, name, enabled).await
}

/// Install a primitive AND its forward-dependency closure (ADR-015 §3b / WP-14).
#[tauri::command]
#[allow(non_snake_case, clippy::too_many_arguments)]
pub async fn oba_install_with_deps(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
    source: String,
    url: String,
    gitRef: Option<String>,
    fromCatalog: Option<bool>,
    catalog: Vec<CatalogEntryRef>,
    expectSha: Option<String>,
    expectHash: Option<String>,
) -> Result<InstallWithDepsResult, String> {
    install::oba_install_with_deps(
        &db,
        kind,
        name,
        source,
        url,
        gitRef,
        fromCatalog,
        catalog,
        expectSha,
        expectHash,
    )
    .await
}

/// Re-verify a primitive's `requires` at enable time: return the recorded deps
/// that are no longer present (WP-14 re-verify-at-enable).
#[tauri::command]
pub async fn oba_missing_requires(
    db: State<'_, Arc<PaDb>>,
    kind: String,
    name: String,
) -> Result<Vec<RequiresEntry>, String> {
    install::oba_missing_requires_in(&Vault::desktop(), &db, kind, name).await
}
