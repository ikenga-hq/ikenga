//! Secrets via the `tauri_plugin_stronghold::stronghold::Stronghold` wrapper
//! (a thin shim over `iota_stronghold`). Snapshot lives at
//! `app_data_dir/secrets.stronghold`. The vault key is bootstrapped from a
//! file at `app_data_dir/.vault-key` — see `crate::vault_key`. No password
//! prompt; no argon2.
//!
//! The `tauri-plugin-stronghold` plugin itself is intentionally NOT
//! registered in `lib.rs`. The FE never invokes plugin-direct commands; all
//! vault access goes through the `secrets_*` commands here, serialized by
//! `SecretsLock`. Registering the plugin would expose handlers that bypass
//! our lock and race on the same snapshot file (each `Stronghold` instance
//! owns its own per-instance RwLocks, so two instances on one snapshot path
//! corrupt writes via `commit_with_keyprovider`).
//!
//! Implementation notes:
//!   - We always open Stronghold with the key from the vault-key file.
//!   - All values are stored as UTF-8 bytes under a single client (`pa`).
//!   - We maintain a parallel `__manifest` entry containing a JSON array of
//!     known key names, so the UI can list keys without scanning the whole
//!     vault. `Store::keys()` is not stable across plugin versions; the
//!     manifest is portable.
//!   - The `secrets_dump_to_runtime_file` helper writes all key/value pairs
//!     to an OS-runtime file (`$XDG_RUNTIME_DIR/ikenga-actions/env-vault` or
//!     `$TMPDIR/ikenga-actions/env-vault` on macOS) so sidecar processes can
//!     read them via the existing dotenv loader. The runtime file is chmod
//!     0600 and cleaned up on app quit.
//!   - A second **durable** copy is written to
//!     `$XDG_CONFIG_HOME/ikenga-actions/env` (Linux) or
//!     `~/Library/Application Support/ikenga-actions/env` (macOS) on every
//!     `secrets_set` / `secrets_delete`. This file survives shell restarts so
//!     the mutation-worker daemon can send overnight with the shell closed.
//!     It is NOT cleaned up on quit — that is by design.
//!   - `read_secret` is a sync helper exposed for capability resolvers (e.g.
//!     pkg_content's Supabase capability) that need vault values during
//!     command handling without round-tripping through the public
//!     `secrets_get` Tauri command.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_stronghold::stronghold::Stronghold;

use crate::vault_key;

const CLIENT_NAME: &[u8] = b"pa";
const MANIFEST_KEY: &[u8] = b"__manifest";
/// Phase 7 — scope-tagged manifest. Stored alongside `__manifest` so legacy
/// readers (anything tracking unscoped names) keep working during the
/// deprecation window. Values are fully-qualified scoped names —
/// `workspace::KEY`, `project::<id>::KEY`, `pkg::<id>::KEY`.
const MANIFEST_V2_KEY: &[u8] = b"__manifest_v2";

/// App-managed cache + serialization lock for Stronghold. The wrapped
/// `Stronghold` instance is opened lazily on first use (or eagerly during
/// `setup`) and reused across every `secrets_*` command — `load_snapshot`
/// runs once per app lifetime, `commit_with_keyprovider` runs only on writes.
///
/// `std::sync::Mutex` is correct here because all blocking work happens
/// inside `tokio::task::spawn_blocking`, never across `.await`. Two parallel
/// instances on the same snapshot file would corrupt writes (each has its
/// own per-instance RwLocks); this single cached instance + serialized
/// access is the canonical pattern.
pub struct SecretsLock(pub Arc<Mutex<Option<Stronghold>>>);

impl SecretsLock {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }
}

impl Default for SecretsLock {
    fn default() -> Self {
        Self::new()
    }
}

fn snapshot_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
    Ok(dir.join("secrets.stronghold"))
}

/// Lazily open the cached Stronghold and load (or create) our client. After
/// first call the in-memory `Stronghold` is reused for the app's lifetime.
fn ensure_open<R: Runtime>(
    app: &AppHandle<R>,
    slot: &mut Option<Stronghold>,
) -> Result<(), String> {
    if slot.is_some() {
        return Ok(());
    }
    let path = snapshot_path(app)?;
    let pw = vault_key::fetch_or_create().map_err(|e| format!("vault key: {e}"))?;
    let stronghold = Stronghold::new(&path, pw).map_err(|e| format!("stronghold open: {e}"))?;
    // After `Stronghold::new` runs `load_snapshot` (if the file exists), the
    // client still needs to be brought into memory. Try `get_client` first
    // (no-op if the in-memory map already has it — shouldn't on a fresh
    // instance, but cheap), then `load_client` (snapshot → memory), and only
    // `create_client` when the snapshot has no such client at all.
    if stronghold.get_client(CLIENT_NAME).is_err() && stronghold.load_client(CLIENT_NAME).is_err() {
        stronghold
            .create_client(CLIENT_NAME)
            .map_err(|e| format!("stronghold create_client: {e}"))?;
    }
    *slot = Some(stronghold);
    Ok(())
}

/// Run a closure under the cached Stronghold. `commit` controls whether to
/// persist after the closure runs — reads pass `false`, writes pass `true`.
/// Lock-poisoning short-circuits with an error rather than panicking.
fn with_stronghold<R: Runtime, F, T>(
    app: &AppHandle<R>,
    state: &Arc<Mutex<Option<Stronghold>>>,
    commit: bool,
    f: F,
) -> Result<T, String>
where
    F: FnOnce(&Stronghold) -> Result<T, String>,
{
    let mut guard = state
        .lock()
        .map_err(|e| format!("secrets lock poisoned: {e}"))?;
    ensure_open(app, &mut guard)?;
    let sh = guard.as_ref().expect("ensure_open populated the slot");
    let result = f(sh)?;
    if commit {
        if let Err(e) = sh.save() {
            log::warn!("stronghold save failed: {e}");
        }
    }
    Ok(result)
}

// ─── Phase 7: vault scope partitioning ──────────────────────────────────
//
// Vault keys are namespaced by Scope to keep secrets from leaking across
// projects / pkgs. The on-disk Stronghold remains one snapshot — the
// namespacing is at the key-name level. Existing un-namespaced keys
// continue to work via the legacy fallback path on read.

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scope {
    /// Workspace-level — intentionally cross-project (rare, e.g. shared
    /// connector tokens).
    Workspace,
    /// Project-level — defaults for new secrets are this scope with the
    /// active project's id.
    Project { id: String },
    /// Pkg-level — pkg-supplied capability resolvers default here, with
    /// the pkg's own id.
    Pkg { id: String },
}

impl Scope {
    pub fn project(id: impl Into<String>) -> Self {
        Self::Project { id: id.into() }
    }
    pub fn pkg(id: impl Into<String>) -> Self {
        Self::Pkg { id: id.into() }
    }
}

/// Fully-qualify a key under a scope. Used by every scoped read/write
/// against Stronghold. The result is the literal key stored in the vault.
pub fn vault_key(scope: &Scope, key: &str) -> String {
    match scope {
        Scope::Workspace => format!("workspace::{key}"),
        Scope::Project { id } => format!("project::{id}::{key}"),
        Scope::Pkg { id } => format!("pkg::{id}::{key}"),
    }
}

/// Parse a fully-qualified vault entry back into `(scope, key)`. Returns
/// `None` for legacy unscoped entries (no `::` prefix matching a known
/// scope). Used by the Settings UI and the dump-resolver to walk the
/// namespace without re-parsing strings repeatedly.
pub fn parse_scoped(fqk: &str) -> Option<(Scope, String)> {
    if let Some(rest) = fqk.strip_prefix("workspace::") {
        return Some((Scope::Workspace, rest.to_string()));
    }
    if let Some(rest) = fqk.strip_prefix("project::") {
        // project::<id>::<key>
        if let Some((id, key)) = rest.split_once("::") {
            if !id.is_empty() && !key.is_empty() {
                return Some((Scope::project(id), key.to_string()));
            }
        }
        return None;
    }
    if let Some(rest) = fqk.strip_prefix("pkg::") {
        if let Some((id, key)) = rest.split_once("::") {
            if !id.is_empty() && !key.is_empty() {
                return Some((Scope::pkg(id), key.to_string()));
            }
        }
        return None;
    }
    None
}

fn read_manifest(stronghold: &Stronghold) -> Result<BTreeSet<String>, String> {
    let client = stronghold
        .get_client(CLIENT_NAME)
        .map_err(|e| format!("get_client: {e}"))?;
    let store = client.store();
    match store.get(MANIFEST_KEY) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes).map_err(|e| format!("manifest utf8: {e}"))?;
            let v: Vec<String> =
                serde_json::from_str(&s).map_err(|e| format!("manifest json: {e}"))?;
            Ok(v.into_iter().collect())
        }
        Ok(None) => Ok(BTreeSet::new()),
        Err(e) => Err(format!("manifest get: {e}")),
    }
}

fn write_manifest(stronghold: &Stronghold, keys: &BTreeSet<String>) -> Result<(), String> {
    let client = stronghold
        .get_client(CLIENT_NAME)
        .map_err(|e| format!("get_client: {e}"))?;
    let store = client.store();
    let v: Vec<&String> = keys.iter().collect();
    let json = serde_json::to_string(&v).map_err(|e| format!("manifest serialize: {e}"))?;
    store
        .insert(MANIFEST_KEY.to_vec(), json.into_bytes(), None)
        .map_err(|e| format!("manifest insert: {e}"))?;
    Ok(())
}

/// Read the v2 (scoped) manifest. Returns fully-qualified names.
fn read_manifest_v2(stronghold: &Stronghold) -> Result<BTreeSet<String>, String> {
    let client = stronghold
        .get_client(CLIENT_NAME)
        .map_err(|e| format!("get_client: {e}"))?;
    let store = client.store();
    match store.get(MANIFEST_V2_KEY) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes).map_err(|e| format!("manifest_v2 utf8: {e}"))?;
            let v: Vec<String> =
                serde_json::from_str(&s).map_err(|e| format!("manifest_v2 json: {e}"))?;
            Ok(v.into_iter().collect())
        }
        Ok(None) => Ok(BTreeSet::new()),
        Err(e) => Err(format!("manifest_v2 get: {e}")),
    }
}

fn write_manifest_v2(stronghold: &Stronghold, keys: &BTreeSet<String>) -> Result<(), String> {
    let client = stronghold
        .get_client(CLIENT_NAME)
        .map_err(|e| format!("get_client: {e}"))?;
    let store = client.store();
    let v: Vec<&String> = keys.iter().collect();
    let json = serde_json::to_string(&v).map_err(|e| format!("manifest_v2 serialize: {e}"))?;
    store
        .insert(MANIFEST_V2_KEY.to_vec(), json.into_bytes(), None)
        .map_err(|e| format!("manifest_v2 insert: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn secrets_get(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
) -> Result<Option<String>, String> {
    if key.as_bytes() == MANIFEST_KEY {
        return Ok(None);
    }
    let state = lock.0.clone();
    tokio::task::spawn_blocking(move || {
        with_stronghold(&app, &state, false, |sh| {
            let client = sh
                .get_client(CLIENT_NAME)
                .map_err(|e| format!("get_client: {e}"))?;
            let store = client.store();
            match store.get(key.as_bytes()) {
                Ok(Some(bytes)) => {
                    let s = String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))?;
                    Ok(Some(s))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(format!("store get: {e}")),
            }
        })
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_set(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
    value: String,
) -> Result<(), String> {
    if key.as_bytes() == MANIFEST_KEY || key.is_empty() {
        return Err("invalid key".into());
    }
    let state = lock.0.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        with_stronghold(&app, &state, true, |sh| {
            let client = sh
                .get_client(CLIENT_NAME)
                .map_err(|e| format!("get_client: {e}"))?;
            let store = client.store();
            store
                .insert(key.as_bytes().to_vec(), value.into_bytes(), None)
                .map_err(|e| format!("store insert: {e}"))?;
            let mut manifest = read_manifest(sh)?;
            manifest.insert(key);
            write_manifest(sh, &manifest)?;
            Ok(())
        })?;
        // Re-dump on the same blocking thread so we don't bounce work back
        // through the runtime just to enter another spawn_blocking.
        let _ = dump_to_runtime_file_locked(&app_for_dump, &state);
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_delete(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
) -> Result<(), String> {
    if key.as_bytes() == MANIFEST_KEY {
        return Err("invalid key".into());
    }
    let state = lock.0.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        with_stronghold(&app, &state, true, |sh| {
            let client = sh
                .get_client(CLIENT_NAME)
                .map_err(|e| format!("get_client: {e}"))?;
            let store = client.store();
            store
                .delete(key.as_bytes())
                .map_err(|e| format!("store delete: {e}"))?;
            let mut manifest = read_manifest(sh)?;
            manifest.remove(&key);
            write_manifest(sh, &manifest)?;
            Ok(())
        })?;
        let _ = dump_to_runtime_file_locked(&app_for_dump, &state);
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_list_keys(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<Vec<String>, String> {
    let state = lock.0.clone();
    tokio::task::spawn_blocking(move || {
        with_stronghold(&app, &state, false, |sh| {
            let manifest = read_manifest(sh)?;
            Ok(manifest.into_iter().collect())
        })
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

/// Wire shape lives in `crate::secrets_env` so the desktop app and the
/// headless daemon cannot drift: both builds serialize the same five fields,
/// and the frontend normalizes one shape. Desktop reports
/// `mode: "stronghold", writable: true`; the daemon reports
/// `mode: "env", writable: false`.
pub use crate::secrets_env::VaultStatus;

#[tauri::command]
pub async fn secrets_vault_status(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<VaultStatus, String> {
    let state = lock.0.clone();
    tokio::task::spawn_blocking(move || {
        let backend = vault_key::keychain_backend().to_string();
        match vault_key::fetch_or_create() {
            Ok(_) => {
                // Probe an actual open + manifest read so we surface
                // post-keychain-open failures (snapshot corruption, etc.) too.
                match with_stronghold(&app, &state, false, |sh| read_manifest(sh)) {
                    Ok(_) => Ok(VaultStatus {
                        available: true,
                        keychain_backend: backend,
                        error: None,
                        mode: crate::secrets_env::MODE_STRONGHOLD.to_string(),
                        writable: true,
                    }),
                    Err(e) => Ok(VaultStatus {
                        available: false,
                        keychain_backend: backend,
                        error: Some(e),
                        mode: crate::secrets_env::MODE_STRONGHOLD.to_string(),
                        writable: true,
                    }),
                }
            }
            Err(e) => Ok(VaultStatus {
                available: false,
                keychain_backend: backend,
                error: Some(e.to_string()),
                mode: crate::secrets_env::MODE_STRONGHOLD.to_string(),
                writable: true,
            }),
        }
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

// ─── Internal read helper (for capability resolvers) ────────────────────────

/// Read a single key from the vault. Used by capability resolvers (e.g.
/// pkg_content's Supabase capability) that need to consult secrets without
/// going through the public `secrets_get` Tauri command.
///
/// Synchronous: takes the `SecretsLock` directly and runs against the cached
/// Stronghold. Caller is already inside an async command but vault reads off
/// the cached instance are sub-millisecond, so blocking the runtime briefly
/// is acceptable. Wrap in `spawn_blocking` if calling on a hot path.
#[allow(dead_code)]
pub fn read_secret(
    app: &AppHandle,
    lock: &SecretsLock,
    key: &str,
) -> Result<Option<String>, String> {
    if key.as_bytes() == MANIFEST_KEY {
        return Ok(None);
    }
    with_stronghold(app, &lock.0, false, |sh| {
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        match store.get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let s = String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))?;
                Ok(Some(s))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("store get: {e}")),
        }
    })
}

// ─── F-9: settings-secret env injection ─────────────────────────────────────

/// Resolve the env-var secrets a pkg declares in its `settings` schema.
///
/// For each `type:"secret"` settings field carrying an `env` name, read the
/// secret value from Stronghold under the pkg's OWN scope (`Scope::pkg`, with
/// the legacy-unscoped fallback that `read_secret_scoped` already provides)
/// and return `(ENV_NAME, value)` pairs for the caller to set on a spawning
/// child (`cmd.env(name, value)`).
///
/// Scoped strictly to the pkg's own manifest — its manifest is its
/// entitlement, so no `permissions.vault.keys` glob check is involved. A
/// secret absent from the vault is skipped silently (the process-env fallback
/// still applies). Synchronous + `block_on`-free: it reads off the cached
/// Stronghold, matching `read_secret`'s contract, so it is safe to call from
/// both the sync (`spawn_streaming_child_sync`) and async
/// (`spawn_and_handshake`) spawn paths without touching the Tokio runtime.
pub fn resolve_settings_secret_env(
    app: &AppHandle,
    pkg_id: &str,
    settings_fields: &[crate::pkg::manifest::SettingsField],
) -> Vec<(String, String)> {
    let Some(lock) = app.try_state::<SecretsLock>() else {
        return Vec::new();
    };
    let scope = Scope::pkg(pkg_id);
    let mut out = Vec::new();
    for field in settings_fields {
        if field.field_type != "secret" {
            continue;
        }
        let Some(env_name) = field.env.as_deref() else {
            continue;
        };
        if env_name.is_empty() {
            continue;
        }
        match read_secret_scoped(app, lock.inner(), &scope, env_name) {
            Ok(Some(value)) => out.push((env_name.to_string(), value)),
            Ok(None) => {}
            Err(e) => log::warn!(
                "[secrets] settings-secret env `{env_name}` for pkg `{pkg_id}` failed to resolve: {e}"
            ),
        }
    }
    out
}

// ─── Scope-aware variants (Phase 7) ──────────────────────────────────────────

/// Scoped read with legacy fallback. Resolution order:
///   1. `vault_key(scope, key)` — the new partitioned key.
///   2. The literal `key` — legacy unscoped value, deprecation-warned on
///      first hit per process.
/// Returns `None` only when neither exists.
pub fn read_secret_scoped(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
    key: &str,
) -> Result<Option<String>, String> {
    if key.as_bytes() == MANIFEST_KEY || key.as_bytes() == MANIFEST_V2_KEY {
        return Ok(None);
    }
    with_stronghold(app, &lock.0, false, |sh| {
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        let scoped = vault_key(scope, key);
        match store.get(scoped.as_bytes()) {
            Ok(Some(bytes)) => {
                let s = String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))?;
                return Ok(Some(s));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("scoped get: {e}")),
        }
        match store.get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let s = String::from_utf8(bytes).map_err(|e| format!("utf8: {e}"))?;
                log::warn!(
                    "vault: legacy unscoped key `{key}` read (deprecation: migrate to `{scoped}`)"
                );
                Ok(Some(s))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("legacy get: {e}")),
        }
    })
}

pub fn scoped_set_locked_pub(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
    key: &str,
    value: &str,
) -> Result<(), String> {
    scoped_set_locked(app, &lock.0, scope, key, value)?;
    let _ = dump_to_runtime_file_locked(app, &lock.0);
    Ok(())
}

pub fn scoped_delete_locked_pub(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
    key: &str,
) -> Result<(), String> {
    scoped_delete_locked(app, &lock.0, scope, key)?;
    let _ = dump_to_runtime_file_locked(app, &lock.0);
    Ok(())
}

// ─── Manifest vault.keys glob parsing (Phase 7) ─────────────────────────
//
// Pkgs declare which secrets they're allowed to read via
// `permissions.vault.keys` — a list of key-name globs. Phase 7 extends
// the syntax: a bare entry binds to the pkg's own scope; an explicit
// `scope=<workspace|project>:` prefix declares a cross-scope grant.
//
// Examples:
//   "MY_API_KEY"                  → matches  pkg::<this-pkg>::MY_API_KEY
//   "scope=workspace:SHARED_KEY"  → matches  workspace::SHARED_KEY
//   "scope=project:STRIPE_KEY"    → matches  project::<active>::STRIPE_KEY
//
// `*` and `?` are glob wildcards. We use a small in-house matcher rather
// than pulling in a crate.

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum VaultKeyPatternScope {
    Pkg,       // default — bind to the requesting pkg's own scope
    Workspace, // cross-scope: read from workspace
    Project,   // cross-scope: read from the active project
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VaultKeyPattern {
    pub scope: VaultKeyPatternScope,
    pub glob: String, // bare key glob (no scope prefix)
}

#[allow(dead_code)]
pub fn parse_vault_key_pattern(pattern: &str) -> VaultKeyPattern {
    if let Some(rest) = pattern.strip_prefix("scope=workspace:") {
        VaultKeyPattern {
            scope: VaultKeyPatternScope::Workspace,
            glob: rest.to_string(),
        }
    } else if let Some(rest) = pattern.strip_prefix("scope=project:") {
        VaultKeyPattern {
            scope: VaultKeyPatternScope::Project,
            glob: rest.to_string(),
        }
    } else {
        VaultKeyPattern {
            scope: VaultKeyPatternScope::Pkg,
            glob: pattern.to_string(),
        }
    }
}

/// Minimal glob match — supports `*` (any sequence) and `?` (one char).
/// `[]` character classes and `**` are intentionally out of scope; the
/// vault key namespace is flat and short.
pub fn glob_match(glob: &str, name: &str) -> bool {
    let gb = glob.as_bytes();
    let nb = name.as_bytes();
    fn rec(g: &[u8], n: &[u8]) -> bool {
        if g.is_empty() {
            return n.is_empty();
        }
        match g[0] {
            b'*' => {
                // Greedy: try matching zero or more chars.
                let rest = &g[1..];
                let mut i = 0;
                loop {
                    if rec(rest, &n[i..]) {
                        return true;
                    }
                    if i == n.len() {
                        return false;
                    }
                    i += 1;
                }
            }
            b'?' => !n.is_empty() && rec(&g[1..], &n[1..]),
            c => !n.is_empty() && n[0] == c && rec(&g[1..], &n[1..]),
        }
    }
    rec(gb, nb)
}

/// Read a secret on behalf of a pkg, enforcing its declared
/// `permissions.vault.keys` globs. Returns `Err` when no declared
/// pattern matches `key` in any allowed scope. Returns `Ok(None)` when
/// a pattern matched but the value is absent in the vault.
///
/// `active_project_id` is the resolved project to use for project-scoped
/// patterns. Pass the pkg's own project_id when the pkg is project-scoped,
/// otherwise the currently-active project.
#[allow(dead_code)]
pub fn read_secret_for_pkg(
    app: &AppHandle,
    lock: &SecretsLock,
    pkg_id: &str,
    declared: &[String],
    key: &str,
    active_project_id: &str,
) -> Result<Option<String>, String> {
    // Walk every declared pattern; the first whose glob matches `key`
    // gets to do the read against its resolved scope.
    for raw in declared {
        let pat = parse_vault_key_pattern(raw);
        if !glob_match(&pat.glob, key) {
            continue;
        }
        let scope = match pat.scope {
            VaultKeyPatternScope::Pkg => Scope::pkg(pkg_id),
            VaultKeyPatternScope::Workspace => Scope::Workspace,
            VaultKeyPatternScope::Project => Scope::project(active_project_id),
        };
        return read_secret_scoped(app, lock, &scope, key);
    }
    Err(format!(
        "pkg `{pkg_id}` not permitted to read vault key `{key}` (no matching vault.keys pattern)"
    ))
}

pub fn scoped_list_locked_pub(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
) -> Result<Vec<String>, String> {
    scoped_list_locked(app, &lock.0, scope)
}

fn scoped_set_locked(
    app: &AppHandle,
    state: &Arc<Mutex<Option<Stronghold>>>,
    scope: &Scope,
    key: &str,
    value: &str,
) -> Result<(), String> {
    if key.as_bytes() == MANIFEST_KEY || key.as_bytes() == MANIFEST_V2_KEY || key.is_empty() {
        return Err("invalid key".into());
    }
    with_stronghold(app, state, true, |sh| {
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        let scoped = vault_key(scope, key);
        store
            .insert(scoped.as_bytes().to_vec(), value.as_bytes().to_vec(), None)
            .map_err(|e| format!("store insert: {e}"))?;
        let mut m2 = read_manifest_v2(sh)?;
        m2.insert(scoped);
        write_manifest_v2(sh, &m2)?;
        Ok(())
    })
}

fn scoped_delete_locked(
    app: &AppHandle,
    state: &Arc<Mutex<Option<Stronghold>>>,
    scope: &Scope,
    key: &str,
) -> Result<(), String> {
    if key.as_bytes() == MANIFEST_KEY || key.as_bytes() == MANIFEST_V2_KEY {
        return Err("invalid key".into());
    }
    with_stronghold(app, state, true, |sh| {
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        let scoped = vault_key(scope, key);
        store
            .delete(scoped.as_bytes())
            .map_err(|e| format!("store delete: {e}"))?;
        let mut m2 = read_manifest_v2(sh)?;
        m2.remove(&scoped);
        write_manifest_v2(sh, &m2)?;
        Ok(())
    })
}

fn scoped_list_locked(
    app: &AppHandle,
    state: &Arc<Mutex<Option<Stronghold>>>,
    scope: &Scope,
) -> Result<Vec<String>, String> {
    with_stronghold(app, state, false, |sh| {
        let m2 = read_manifest_v2(sh)?;
        let mut out: Vec<String> = Vec::new();
        for fqk in m2 {
            if let Some((sc, key)) = parse_scoped(&fqk) {
                if &sc == scope {
                    out.push(key);
                }
            }
        }
        out.sort();
        Ok(out)
    })
}

#[tauri::command]
pub async fn secrets_get_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
    key: String,
) -> Result<Option<String>, String> {
    let state = lock.0.clone();
    tokio::task::spawn_blocking(move || {
        // SecretsLock isn't State<>-shaped here; reconstruct a transient
        // one so we can reuse the public read_secret_scoped helper.
        let l = SecretsLock(state);
        read_secret_scoped(&app, &l, &scope, &key)
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_set_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
    key: String,
    value: String,
) -> Result<(), String> {
    let state = lock.0.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        scoped_set_locked(&app, &state, &scope, &key, &value)?;
        let _ = dump_to_runtime_file_locked(&app_for_dump, &state);
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_delete_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
    key: String,
) -> Result<(), String> {
    let state = lock.0.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        scoped_delete_locked(&app, &state, &scope, &key)?;
        let _ = dump_to_runtime_file_locked(&app_for_dump, &state);
        Ok(())
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_list_keys_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
) -> Result<Vec<String>, String> {
    let state = lock.0.clone();
    tokio::task::spawn_blocking(move || scoped_list_locked(&app, &state, &scope))
        .await
        .map_err(|e| format!("join: {e}"))?
}

// ─── Backup helpers (phase 2) ────────────────────────────────────────────────

/// Enumerate the entire vault as `key → value` pairs. Used by the backup
/// exporter; the result is age-encrypted before leaving the process. Skips
/// the internal `__manifest` key.
pub fn dump_all_kvs<R: Runtime>(
    app: &AppHandle<R>,
    lock: &SecretsLock,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    with_stronghold(app, &lock.0, false, |sh| {
        let manifest = read_manifest(sh)?;
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        let mut out = std::collections::BTreeMap::new();
        for k in &manifest {
            if k.as_bytes() == MANIFEST_KEY {
                continue;
            }
            match store.get(k.as_bytes()) {
                Ok(Some(b)) => match String::from_utf8(b) {
                    Ok(v) => {
                        out.insert(k.clone(), v);
                    }
                    Err(_) => log::warn!("secret {k} is non-utf8, skipping in backup"),
                },
                Ok(None) => {}
                Err(e) => log::warn!("secret {k} read failed: {e}"),
            }
        }
        Ok(out)
    })
}

/// Apply a full set of `key → value` pairs to the vault. Used by the backup
/// importer's boot-time apply step. Each key is upserted; the manifest is
/// updated once at the end. Existing values for keys that aren't in `kvs`
/// are left in place — this is a merge, not a replacement, so a partial or
/// secret-less restore can't accidentally wipe a user's vault.
pub fn bulk_set<R: Runtime>(
    app: &AppHandle<R>,
    lock: &SecretsLock,
    kvs: &std::collections::BTreeMap<String, String>,
) -> Result<usize, String> {
    if kvs.is_empty() {
        return Ok(0);
    }
    with_stronghold(app, &lock.0, true, |sh| {
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();
        let mut manifest = read_manifest(sh)?;
        for (k, v) in kvs {
            if k.as_bytes() == MANIFEST_KEY || k.is_empty() {
                continue;
            }
            store
                .insert(k.as_bytes().to_vec(), v.as_bytes().to_vec(), None)
                .map_err(|e| format!("insert {k}: {e}"))?;
            manifest.insert(k.clone());
        }
        write_manifest(sh, &manifest)?;
        Ok(kvs.len())
    })
}

// ─── Runtime env-vault file (for the actions sidecar) ────────────────────────

/// Path to the runtime env-vault file. macOS uses `$TMPDIR`, others use
/// `$XDG_RUNTIME_DIR` (with a `/tmp` fallback). chmod 600.
pub fn runtime_env_vault_path() -> PathBuf {
    let base: PathBuf = if cfg!(target_os = "macos") {
        std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    };
    base.join("ikenga-actions").join("env-vault")
}

/// Path to the **durable** env file written alongside the volatile runtime
/// vault. The mutation-worker daemon reads this path when the shell is not
/// running (overnight sends). Uses `$XDG_CONFIG_HOME` on Linux, or
/// `~/Library/Application Support` on macOS, falling back to `~/.config`
/// if neither env var is set. chmod 600. NOT cleaned up on app quit — the
/// file's whole purpose is to survive the shell being closed.
pub fn durable_env_path() -> PathBuf {
    let base: PathBuf = if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".config"))
                    .unwrap_or_else(|| PathBuf::from("/tmp"))
            })
    };
    base.join("ikenga-actions").join("env")
}

fn shell_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' | '$' | '`' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Resolve the active project id without forcing every caller to await
/// the projects table. Uses `tauri::async_runtime::block_on` since we're
/// invoked from sync contexts (`std::thread::spawn` at boot, `spawn_blocking`
/// for command-driven re-dumps). Falls back to `"default"` on any failure
/// so the dump can still proceed with workspace + legacy values.
fn resolve_active_project_blocking<R: Runtime>(app: &AppHandle<R>) -> String {
    use crate::commands::db::PaDb;
    use crate::commands::projects::get_active_project_id;

    let Some(db) = app.try_state::<std::sync::Arc<PaDb>>() else {
        return "default".to_string();
    };
    let db = db.inner().clone();
    tauri::async_runtime::block_on(async move {
        match db.ensure_pool().await {
            Ok(pool) => get_active_project_id(&pool)
                .await
                .unwrap_or_else(|_| "default".to_string()),
            Err(_) => "default".to_string(),
        }
    })
}

/// Dump vault key/values into the runtime env-vault file using the
/// shared cached Stronghold. The dumped file is what sidecars + per-call
/// MCP children pick up via dotenv — so the file contains *resolved*
/// values for the active project, not scope-prefixed key names.
///
/// Resolution cascade per name:
///   1. `project::<active_id>::NAME` — active project's value.
///   2. `workspace::NAME` — cross-project fallback.
///   3. legacy unscoped `NAME` — pre-Phase-7 entries.
///
/// Pkg-scoped values (`pkg::<id>::*`) are intentionally NOT dumped —
/// those resolve at command-handling time inside the kernel where we
/// know the caller's pkg id, not in the shared sidecar env file.
fn dump_to_runtime_file_locked<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<Mutex<Option<Stronghold>>>,
) -> Result<PathBuf, String> {
    let active_pid = resolve_active_project_blocking(app);
    with_stronghold(app, state, false, |sh| {
        let m_legacy = read_manifest(sh)?;
        let m_v2 = read_manifest_v2(sh)?;
        let client = sh
            .get_client(CLIENT_NAME)
            .map_err(|e| format!("get_client: {e}"))?;
        let store = client.store();

        // Collect every visible key NAME from the union of:
        //   - legacy (unscoped) manifest
        //   - v2 entries whose scope is Workspace or Project(active)
        let mut names: BTreeSet<String> = BTreeSet::new();
        for k in &m_legacy {
            names.insert(k.clone());
        }
        for fqk in &m_v2 {
            if let Some((scope, key)) = parse_scoped(fqk) {
                match &scope {
                    Scope::Workspace => {
                        names.insert(key);
                    }
                    Scope::Project { id } if id == &active_pid => {
                        names.insert(key);
                    }
                    _ => {}
                }
            }
        }

        let mut body = String::from("# Auto-generated by ikenga-desktop. Do not edit.\n");
        for name in &names {
            // Cascade: project → workspace → legacy. First hit wins.
            let candidates: [String; 3] = [
                format!("project::{active_pid}::{name}"),
                format!("workspace::{name}"),
                name.clone(),
            ];
            let mut value: Option<String> = None;
            for fqk in &candidates {
                match store.get(fqk.as_bytes()) {
                    Ok(Some(b)) => match String::from_utf8(b) {
                        Ok(v) => {
                            value = Some(v);
                            break;
                        }
                        Err(_) => continue,
                    },
                    _ => continue,
                }
            }
            let Some(val) = value else { continue };
            body.push_str(name);
            body.push('=');
            body.push_str(&shell_escape(&val));
            body.push('\n');
        }

        let path = runtime_env_vault_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir runtime: {e}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
        }
        std::fs::write(&path, &body).map_err(|e| format!("write runtime: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("chmod runtime: {e}"))?;
        }

        // Also write the durable copy at ~/.config/ikenga-actions/env so the
        // mutation-worker daemon can read credentials while the shell is closed
        // (overnight sends). Failure is non-fatal — the volatile runtime vault
        // is still the primary path; log and continue.
        let durable = durable_env_path();
        if let Some(parent) = durable.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("vault: could not create durable env dir {}: {e}", parent.display());
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
                }
            }
        }
        match std::fs::write(&durable, &body) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Err(e) = std::fs::set_permissions(
                        &durable,
                        std::fs::Permissions::from_mode(0o600),
                    ) {
                        tracing::warn!("vault: chmod durable env {}: {e}", durable.display());
                    }
                }
            }
            Err(e) => {
                tracing::warn!("vault: could not write durable env {}: {e}", durable.display());
            }
        }

        Ok(path)
    })
}

/// Public entry point used from the Tauri `setup` hook. Pulls the shared
/// cache out of app state. Setup runs synchronously before the runtime
/// starts handling commands, so this is fine to call directly without
/// `spawn_blocking`.
pub fn dump_to_runtime_file<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let lock: State<'_, SecretsLock> = app
        .try_state::<SecretsLock>()
        .ok_or_else(|| "SecretsLock state not registered".to_string())?;
    dump_to_runtime_file_locked(app, &lock.0)
}

/// Best-effort cleanup of the runtime env-vault file. Called from the app
/// quit hook.
pub fn cleanup_runtime_file() {
    let path = runtime_env_vault_path();
    let _ = std::fs::remove_file(path);
}

// ─── Tests (Phase 7) ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_key_formats_each_scope() {
        assert_eq!(vault_key(&Scope::Workspace, "K"), "workspace::K");
        assert_eq!(
            vault_key(&Scope::project("alpha"), "K"),
            "project::alpha::K"
        );
        assert_eq!(vault_key(&Scope::pkg("com.x.y"), "K"), "pkg::com.x.y::K");
    }

    #[test]
    fn parse_scoped_roundtrips_each_variant() {
        assert_eq!(
            parse_scoped("workspace::ABC"),
            Some((Scope::Workspace, "ABC".to_string()))
        );
        assert_eq!(
            parse_scoped("project::alpha::ABC"),
            Some((Scope::project("alpha"), "ABC".to_string()))
        );
        assert_eq!(
            parse_scoped("pkg::com.x.y::ABC"),
            Some((Scope::pkg("com.x.y"), "ABC".to_string()))
        );
    }

    #[test]
    fn parse_scoped_returns_none_for_legacy_or_malformed() {
        assert_eq!(parse_scoped("LEGACY_KEY"), None);
        assert_eq!(parse_scoped("project::"), None);
        assert_eq!(parse_scoped("project::onlyid"), None);
        assert_eq!(parse_scoped("pkg::"), None);
    }

    #[test]
    fn vault_key_pattern_parses_explicit_scopes() {
        let p = parse_vault_key_pattern("MY_KEY");
        assert_eq!(p.scope, VaultKeyPatternScope::Pkg);
        assert_eq!(p.glob, "MY_KEY");

        let p = parse_vault_key_pattern("scope=workspace:SHARED");
        assert_eq!(p.scope, VaultKeyPatternScope::Workspace);
        assert_eq!(p.glob, "SHARED");

        let p = parse_vault_key_pattern("scope=project:STRIPE_KEY");
        assert_eq!(p.scope, VaultKeyPatternScope::Project);
        assert_eq!(p.glob, "STRIPE_KEY");
    }

    #[test]
    fn glob_match_basic_wildcards() {
        assert!(glob_match("FOO_*", "FOO_BAR"));
        assert!(glob_match("FOO_*", "FOO_"));
        assert!(!glob_match("FOO_*", "BAR_FOO"));
        assert!(glob_match("?_BAR", "X_BAR"));
        assert!(!glob_match("?_BAR", "XX_BAR"));
        assert!(glob_match("EXACT", "EXACT"));
        assert!(!glob_match("EXACT", "EXAC"));
    }

    #[test]
    fn glob_match_stars_in_middle() {
        assert!(glob_match("A*Z", "AZ"));
        assert!(glob_match("A*Z", "AmiddleZ"));
        assert!(!glob_match("A*Z", "B"));
    }

    #[test]
    fn durable_env_path_ends_with_expected_suffix() {
        let p = durable_env_path();
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("ikenga-actions/env"),
            "durable_env_path should end with ikenga-actions/env, got: {s}"
        );
        // Must NOT be inside a volatile tmp/runtime dir.
        assert!(
            !s.contains("/run/user/") && !s.starts_with("/tmp"),
            "durable_env_path must not point to a volatile runtime dir, got: {s}"
        );
    }

    #[test]
    fn durable_env_path_differs_from_runtime_vault() {
        let durable = durable_env_path();
        let runtime = runtime_env_vault_path();
        assert_ne!(
            durable, runtime,
            "durable and runtime paths must be distinct"
        );
    }
}
