//! Secrets commands and the runtime/durable env-vault files.
//!
//! # Env-vault files are plaintext while unlocked (by design)
//!
//! Sidecars and the headless mutation-worker daemon read secrets from two
//! dotenv files, not from the vault (ADR-022 R6b): the volatile runtime file
//! (`runtime_env_vault_path`) and the **durable** file (`durable_env_path`,
//! `%LOCALAPPDATA%\ikenga-actions\env` on Windows). Both hold every
//! resolvable secret **in plaintext** whenever they are published. With a
//! WP-34 passphrase configured, that is only ever true while the vault is
//! unlocked:
//!
//! - publication needs the DEK (a locked vault fails the read, which
//!   invalidates both files instead of publishing);
//! - explicit lock, idle expiry and app exit overwrite both files with a
//!   deny body and leave a durable deny marker, so nothing plaintext survives
//!   a lock or a clean shutdown;
//! - a publication racing a lock re-checks the lock generation under the
//!   env-publish mutex and aborts (see `dump_to_runtime_file_locked`).
//!
//! What remains, deliberately: while unlocked the durable file is plaintext
//! on disk (per-user ACL / chmod 600 is its only protection), and a crash or
//! power loss while unlocked skips the exit wipe and leaves it until the next
//! launch invalidates it. The trade-off is that a passphrase-protected vault
//! gives the daemon no secrets after the shell exits. With no passphrase
//! configured the WP-33 contract is unchanged: the durable file persists
//! across shell exits.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, State};
use zeroize::Zeroizing;

use crate::secrets::{
    index::{
        validate_key, validate_legacy_name, validate_scope_id, SecretIndex, INDEX_FILENAME,
    },
    EncryptedStore, KeyringStore, LockState, SecretsStore, SharedSecretStore,
    SharedSecretStoreSlot, StoreError, UnavailableSecretStore, UnlockState,
    UNLOCK_ENVELOPE_FILENAME,
};

const MANIFEST_KEY: &[u8] = b"__manifest";
const MANIFEST_V2_KEY: &[u8] = b"__manifest_v2";
const ENV_VAULT_PENDING_FILENAME: &str = "env-vault.pending.json";
const ENV_VAULT_DENIED_FILENAME: &str = "env-vault.denied";
const ENV_VAULT_DENIED_BODY: &[u8] = b"# IKENGA SECRETS DENIED\n";

#[derive(Clone)]
pub struct SecretsLock {
    pub store: SharedSecretStoreSlot,
    pub unlock: Arc<UnlockState>,
    data_dir: Arc<Mutex<Option<PathBuf>>>,
}

impl SecretsLock {
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(None)),
            unlock: Arc::new(UnlockState::new()),
            data_dir: Arc::new(Mutex::new(None)),
        }
    }

    pub fn from_slot(store: SharedSecretStoreSlot) -> Self {
        Self {
            store,
            unlock: Arc::new(UnlockState::new()),
            data_dir: Arc::new(Mutex::new(None)),
        }
    }

    pub fn configure_data_dir(&self, data_dir: &Path) -> Result<(), String> {
        self.unlock
            .configure_path(data_dir.join(UNLOCK_ENVELOPE_FILENAME))
            .map_err(|error| error.to_string())?;
        let mut guard = self
            .data_dir
            .lock()
            .map_err(|error| format!("secrets data directory lock poisoned: {error}"))?;
        match guard.as_ref() {
            Some(existing) if existing != data_dir => {
                Err("secrets data directory is already configured".to_string())
            }
            _ => {
                *guard = Some(data_dir.to_path_buf());
                Ok(())
            }
        }
    }

    pub fn state(&self) -> LockState {
        self.unlock.state()
    }

    pub fn expire_if_idle(&self) -> bool {
        self.unlock.expire_if_idle().unwrap_or(false)
    }

    pub fn replace_store(&self, store: SharedSecretStore) -> Result<(), String> {
        let mut guard = self
            .store
            .lock()
            .map_err(|error| format!("secrets lock poisoned: {error}"))?;
        *guard = Some(store);
        Ok(())
    }

    pub fn mark_unavailable(&self, reason: impl Into<String>) -> Result<(), String> {
        self.unlock.lock().map_err(|error| error.to_string())?;
        self.replace_store(Arc::new(UnavailableSecretStore::new(reason)))
    }

    pub fn probe(&self, app: &AppHandle) -> Result<(), String> {
        let state = self.store.clone();
        with_store(app, &state, self.unlock.as_ref(), |store| store.probe())
            .map_err(|error| error.to_string())
    }

    pub fn prepare_encryption(&self, app: &AppHandle) -> Result<(), String> {
        let state = self.store.clone();
        with_store(app, &state, self.unlock.as_ref(), |store| {
            store.prepare_encryption()
        })
        .map_err(|error| error.to_string())
    }
}

impl Default for SecretsLock {
    fn default() -> Self {
        Self::new()
    }
}

fn ensure_store<R: Runtime>(
    app: &AppHandle<R>,
    slot: &mut Option<SharedSecretStore>,
    unlock: &UnlockState,
) -> Result<(), StoreError> {
    if slot.is_some() {
        return Ok(());
    }
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| StoreError::uncommitted(format!("app_data_dir: {error}")))?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|error| StoreError::uncommitted(format!("mkdir: {error}")))?;
    unlock
        .configure_path(data_dir.join(UNLOCK_ENVELOPE_FILENAME))
        .map_err(|error| StoreError::unavailable(error.to_string()))?;
    let keyring = Arc::new(KeyringStore::new(data_dir.join(INDEX_FILENAME))?);
    keyring.persist_index()?;
    let store = Arc::new(EncryptedStore::new(keyring, Arc::new(unlock.clone())));
    *slot = Some(store);
    Ok(())
}

fn with_store<R: Runtime, F, T>(
    app: &AppHandle<R>,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
    f: F,
) -> Result<T, StoreError>
where
    F: FnOnce(&dyn SecretsStore) -> Result<T, StoreError>,
{
    let mut guard = state
        .lock()
        .map_err(|error| StoreError::uncommitted(format!("secrets lock poisoned: {error}")))?;
    ensure_store(app, &mut guard, unlock)?;
    let store = guard.as_ref().expect("ensure_store populated the slot");
    f(store.as_ref())
}

fn finish_mutation<R: Runtime>(
    app: &AppHandle<R>,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
    result: Result<(), StoreError>,
) -> Result<(), String> {
    match result {
        Ok(()) => dump_to_runtime_file_locked(app, state, unlock).map(|_| ()),
        Err(error) if error.is_committed() => {
            let env_result = dump_to_runtime_file_locked(app, state, unlock);
            match env_result {
                Ok(_) => Err(error.to_string()),
                Err(env_error) => Err(format!("{error}; env-vault update failed: {env_error}")),
            }
        }
        Err(error) => Err(error.to_string()),
    }
}

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

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Workspace => Ok(()),
            Self::Project { id } | Self::Pkg { id } => validate_scope_id(id),
        }
    }
}

pub fn checked_vault_key(scope: &Scope, key: &str) -> Result<String, String> {
    scope.validate()?;
    validate_key(key)?;
    Ok(vault_key(scope, key))
}

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
///
/// Deliberately the permissive WP-33 split — `project::<id>::<key>` is split
/// at the first `::` after the prefix, and the id and key only need to be
/// non-empty — so every scoped name an earlier build could write (e.g.
/// `project::other::a::b`, or an id outside today's charset) still
/// classifies under its scope. The strict WP-34 charset rules apply only to
/// new writes (`checked_vault_key`, `scoped_set_locked`).
pub fn parse_scoped(fqk: &str) -> Option<(Scope, String)> {
    if let Some(rest) = fqk.strip_prefix("workspace::") {
        if rest.is_empty() {
            return None;
        }
        return Some((Scope::Workspace, rest.to_string()));
    }
    if let Some(rest) = fqk.strip_prefix("project::") {
        let (id, key) = rest.split_once("::")?;
        if id.is_empty() || key.is_empty() {
            return None;
        }
        return Some((Scope::project(id), key.to_string()));
    }
    if let Some(rest) = fqk.strip_prefix("pkg::") {
        let (id, key) = rest.split_once("::")?;
        if id.is_empty() || key.is_empty() {
            return None;
        }
        return Some((Scope::pkg(id), key.to_string()));
    }
    None
}

/// `true` when `name` starts with a scope prefix, whether or not the rest
/// classifies under [`parse_scoped`].
fn has_scope_prefix(name: &str) -> bool {
    ["workspace::", "project::", "pkg::"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// Full vault name of an EXISTING scoped entry, for read / delete / list.
/// Permissive (WP-33) rules so legacy entries stay reachable, with one hard
/// requirement: the name must classify back to exactly `(scope, key)`, so a
/// scope id or key containing `::` can never address another scope's entry.
fn existing_scoped_name(scope: &Scope, key: &str) -> Result<String, String> {
    validate_legacy_name(key)?;
    let name = vault_key(scope, key);
    validate_legacy_name(&name)?;
    match parse_scoped(&name) {
        Some((parsed_scope, parsed_key)) if &parsed_scope == scope && parsed_key == key => {
            Ok(name)
        }
        _ => Err("invalid scoped secret name".into()),
    }
}

/// Permissive scope check for reading or listing existing entries.
fn validate_existing_scope(scope: &Scope) -> Result<(), String> {
    existing_scoped_name(scope, "_").map(|_| ())
}

/// Bare (unscoped) address of an EXISTING entry, for the unscoped read and
/// delete commands: any legacy name that does not classify as scoped —
/// exactly the names `secrets_list_keys` shows. That includes a
/// scope-prefixed legacy name that no longer parses (e.g. `project::onlyid`),
/// whose only handle this is. A name that does classify as scoped is refused
/// so the unscoped commands cannot reach into a scope.
fn validate_existing_bare_name(name: &str) -> Result<(), String> {
    validate_legacy_name(name)?;
    if parse_scoped(name).is_some() {
        return Err("secret key contains a scope delimiter".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn secrets_get(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
) -> Result<Option<String>, String> {
    if key.as_bytes() == MANIFEST_KEY || key.as_bytes() == MANIFEST_V2_KEY {
        return Ok(None);
    }
    // Reads address existing entries, which may carry a pre-WP-34 name.
    if validate_existing_bare_name(&key).is_err() {
        return Err("invalid key".into());
    }
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    tokio::task::spawn_blocking(move || {
        with_store(&app, &state, unlock.as_ref(), |store| store.get(&key))
    })
    .await
    .map_err(|error| format!("join: {error}"))?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn secrets_set(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
    value: String,
) -> Result<(), String> {
    if validate_key(&key).is_err() {
        return Err("invalid key".into());
    }
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || {
        let result = with_store(&app, &state, unlock.as_ref(), |store| {
            store.set(&key, &value)
        });
        finish_mutation(&app_for_dump, &state, unlock.as_ref(), result)
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_delete(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    key: String,
) -> Result<(), String> {
    // Deletes address existing entries, which may carry a pre-WP-34 name
    // (including a scope-prefixed one that no longer classifies).
    if validate_existing_bare_name(&key).is_err() {
        return Err("invalid key".into());
    }
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || {
        let result = with_store(&app, &state, unlock.as_ref(), |store| store.delete(&key));
        finish_mutation(&app_for_dump, &state, unlock.as_ref(), result)
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_list_keys(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<Vec<String>, String> {
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    tokio::task::spawn_blocking(move || {
        with_store(&app, &state, unlock.as_ref(), |store| {
            Ok(store
                .list_meta()?
                .into_iter()
                .map(|meta| meta.name)
                .filter(|name| parse_scoped(name).is_none())
                .collect())
        })
    })
    .await
    .map_err(|error| format!("join: {error}"))?
    .map_err(|error| error.to_string())
}

/// Names only, straight from `secrets-index.json` — never touches the
/// Stronghold store, so it works whether or not the vault is unlocked. Added
/// for WP-43's restore wizard, which needs to say which current keys a
/// restore's vault merge would touch without asking the user to unlock the
/// vault just to preview that list. No value is ever read or returned.
#[tauri::command]
pub async fn secrets_index_names(app: AppHandle) -> Result<Vec<String>, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("app_data_dir: {error}"))?;
    let index = SecretIndex::load(data_dir.join(INDEX_FILENAME))?;
    Ok(index.names())
}

pub use crate::secrets_env::VaultStatus;

#[tauri::command]
pub async fn secrets_vault_status(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<VaultStatus, String> {
    if lock.expire_if_idle() {
        invalidate_env_vaults(&app).map_err(|error| {
            format!("secrets idle-locked but env-vault invalidation failed: {error}")
        })?;
    }
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let lock_state = lock.state();
    tokio::task::spawn_blocking(move || {
        let backend = crate::secrets::keyring_store::backend_label().to_string();
        let probe = with_store(&app, &state, unlock.as_ref(), |store| {
            store.probe()?;
            Ok((store.backend_label().to_string(), store.diagnostics()))
        });
        match probe {
            Ok((keychain_backend, diagnostics)) => Ok(VaultStatus {
                available: true,
                keychain_backend,
                error: if diagnostics.is_empty() {
                    None
                } else {
                    Some(diagnostics.join("; "))
                },
                mode: crate::secrets_env::MODE_KEYCHAIN.to_string(),
                writable: !lock_state.locked,
                locked: lock_state.locked,
                configured: lock_state.configured,
                idle_timeout_secs: lock_state.idle_timeout_secs,
                last_activity_unix_ms: lock_state.last_activity_unix_ms,
            }),
            Err(error) => Ok(VaultStatus {
                available: false,
                keychain_backend: backend,
                error: Some(error.to_string()),
                mode: crate::secrets_env::MODE_KEYCHAIN.to_string(),
                writable: false,
                locked: lock_state.locked,
                configured: lock_state.configured,
                idle_timeout_secs: lock_state.idle_timeout_secs,
                last_activity_unix_ms: lock_state.last_activity_unix_ms,
            }),
        }
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
pub async fn secrets_set_passphrase(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    passphrase: String,
    current_passphrase: Option<String>,
    old_passphrase: Option<String>,
) -> Result<LockState, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("app_data_dir: {error}"))?;
    lock.configure_data_dir(&data_dir)?;
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_work = app.clone();
    let passphrase = Zeroizing::new(passphrase);
    let current_passphrase = current_passphrase.or(old_passphrase).map(Zeroizing::new);
    tokio::task::spawn_blocking(move || {
        // One store-slot critical section covers set/rotate and the
        // encryption of pre-existing plaintext values, so no other secrets
        // call interleaves between "envelope exists" and "every value is
        // encrypted". `prepare_encryption` is best-effort per entry; if it
        // still fails (the store cannot be listed), the envelope stays but
        // the DEK is dropped so the backend is locked, matching the error the
        // FE sees; plaintext left behind is encrypted on the next unlock.
        let outcome = with_store(&app_for_work, &state, unlock.as_ref(), |store| {
            store.probe()?;
            // Latch "configured" if the store already holds ciphertext, so a
            // missing envelope can never lead to a fresh DEK over it.
            store.detect_configuration()?;
            if let Err(error) = unlock.set_or_rotate(
                passphrase.as_str(),
                current_passphrase.as_ref().map(|value| value.as_str()),
            ) {
                return Ok(Err(error.to_string()));
            }
            if let Err(error) = store.prepare_encryption() {
                return Ok(Err(relock_after_failure(
                    &app_for_work,
                    unlock.as_ref(),
                    error.to_string(),
                )));
            }
            Ok(Ok(()))
        })
        .map_err(|error| error.to_string())?;
        outcome?;
        dump_to_runtime_file_locked(&app_for_work, &state, unlock.as_ref())
            .map_err(|error| error.to_string())?;
        Ok(unlock.state())
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_unlock(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    passphrase: String,
) -> Result<LockState, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("app_data_dir: {error}"))?;
    lock.configure_data_dir(&data_dir)?;
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_work = app.clone();
    let passphrase = Zeroizing::new(passphrase);
    tokio::task::spawn_blocking(move || {
        let outcome = with_store(&app_for_work, &state, unlock.as_ref(), |store| {
            if let Err(error) = unlock.unlock(passphrase.as_str()) {
                return Ok(Err(error.to_string()));
            }
            // Best-effort per entry (orphans and undecodable values are
            // skipped), so this only fails when the store cannot be walked at
            // all. The DEK is already in memory by then: drop it again so the
            // backend is locked exactly as the FE's failed unlock implies.
            if let Err(error) = store.prepare_encryption() {
                return Ok(Err(relock_after_failure(
                    &app_for_work,
                    unlock.as_ref(),
                    error.to_string(),
                )));
            }
            Ok(Ok(()))
        })
        .map_err(|error| error.to_string())?;
        outcome?;
        dump_to_runtime_file_locked(&app_for_work, &state, unlock.as_ref())
            .map_err(|error| error.to_string())?;
        Ok(unlock.state())
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

/// After a failed post-unlock step: drop the DEK again and, if one was held,
/// invalidate the env-vault files (a re-unlock of an already-unlocked vault
/// may have published them). Returns the error to report.
fn relock_after_failure<R: Runtime>(
    app: &AppHandle<R>,
    unlock: &UnlockState,
    error: String,
) -> String {
    let relock = unlock
        .lock()
        .map_err(|lock_error| lock_error.to_string())
        .and_then(|was_unlocked| {
            if was_unlocked {
                invalidate_env_vaults(app)
            } else {
                Ok(())
            }
        });
    match relock {
        Ok(()) => error,
        Err(relock_error) => format!("{error}; re-lock after failure failed: {relock_error}"),
    }
}

#[tauri::command]
pub async fn secrets_lock(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<LockState, String> {
    if !lock.unlock.is_configured() {
        // No passphrase: there is nothing to lock, and the env-vault files
        // keep their WP-33 contract (invalidating them would only starve the
        // daemon until the next mutation).
        return Ok(lock.state());
    }
    lock.unlock.lock().map_err(|error| error.to_string())?;
    invalidate_env_vaults(&app)
        .map_err(|error| format!("secrets locked but env-vault invalidation failed: {error}"))?;
    Ok(lock.state())
}

#[tauri::command]
pub async fn secrets_lock_state(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
) -> Result<LockState, String> {
    if lock.expire_if_idle() {
        invalidate_env_vaults(&app).map_err(|error| {
            format!("secrets idle-locked but env-vault invalidation failed: {error}")
        })?;
    }
    Ok(lock.state())
}

#[allow(dead_code)]
pub fn read_secret(
    app: &AppHandle,
    lock: &SecretsLock,
    key: &str,
) -> Result<Option<String>, String> {
    if key.as_bytes() == MANIFEST_KEY || key.as_bytes() == MANIFEST_V2_KEY {
        return Ok(None);
    }
    with_store(app, &lock.store, lock.unlock.as_ref(), |store| {
        store.get(key)
    })
    .map_err(|error| error.to_string())
}

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
    // Reads address existing entries (possibly pre-WP-34 names, including
    // WP-33 scoped names outside today's charset).
    let scoped = existing_scoped_name(scope, key).map_err(|_| "invalid key".to_string())?;
    // The legacy unscoped fallback only for a name the bare commands can
    // address, so it never reads another scope's entry.
    let bare_fallback = validate_existing_bare_name(key).is_ok();
    with_store(app, &lock.store, lock.unlock.as_ref(), |store| {
        if let Some(value) = store.get(&scoped)? {
            return Ok(Some(value));
        }
        if !bare_fallback {
            return Ok(None);
        }
        if let Some(value) = store.get(key)? {
            log::warn!(
                "vault: legacy unscoped key `{key}` read (deprecation: migrate to `{scoped}`)"
            );
            return Ok(Some(value));
        }
        Ok(None)
    })
    .map_err(|error| error.to_string())
}

pub fn scoped_set_locked_pub(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let result = scoped_set_locked(app, &lock.store, lock.unlock.as_ref(), scope, key, value);
    finish_mutation(app, &lock.store, lock.unlock.as_ref(), result)
}

pub fn scoped_delete_locked_pub(
    app: &AppHandle,
    lock: &SecretsLock,
    scope: &Scope,
    key: &str,
) -> Result<(), String> {
    let result = scoped_delete_locked(app, &lock.store, lock.unlock.as_ref(), scope, key);
    finish_mutation(app, &lock.store, lock.unlock.as_ref(), result)
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

pub fn valid_vault_glob(glob: &str) -> bool {
    !glob.is_empty()
        && glob.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'*' | b'?')
        })
}

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
    if validate_key(key).is_err() {
        return Err("invalid secret key".into());
    }
    if validate_scope_id(pkg_id).is_err() {
        return Err("invalid pkg id".into());
    }
    if validate_scope_id(active_project_id).is_err() {
        return Err("invalid active project id".into());
    }
    for raw in declared {
        let pattern_glob = raw
            .strip_prefix("scope=workspace:")
            .or_else(|| raw.strip_prefix("scope=project:"))
            .unwrap_or(raw);
        if !valid_vault_glob(pattern_glob) {
            return Err("invalid vault key pattern".into());
        }
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
    validate_existing_scope(scope)?;
    scoped_list_locked(app, &lock.store, lock.unlock.as_ref(), scope)
        .map_err(|error| error.to_string())
}

fn scoped_set_locked(
    app: &AppHandle,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
    scope: &Scope,
    key: &str,
    value: &str,
) -> Result<(), StoreError> {
    if validate_key(key).is_err() {
        return Err(StoreError::invalid("invalid key"));
    }
    scope.validate().map_err(StoreError::invalid)?;
    with_store(app, state, unlock, |store| {
        store.set(&vault_key(scope, key), value)
    })
}

fn scoped_delete_locked(
    app: &AppHandle,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
    scope: &Scope,
    key: &str,
) -> Result<(), StoreError> {
    // Deletes address existing entries (possibly pre-WP-34 names, including
    // WP-33 scoped names such as `project::other::a::b`, whose key is `a::b`).
    let name = existing_scoped_name(scope, key).map_err(StoreError::invalid)?;
    with_store(app, state, unlock, |store| store.delete(&name))
}

fn scoped_list_locked(
    app: &AppHandle,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
    scope: &Scope,
) -> Result<Vec<String>, StoreError> {
    validate_existing_scope(scope).map_err(StoreError::invalid)?;
    with_store(app, state, unlock, |store| {
        let mut out = Vec::new();
        for meta in store.list_meta()? {
            if let Some((candidate_scope, key)) = parse_scoped(&meta.name) {
                if &candidate_scope == scope {
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
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    tokio::task::spawn_blocking(move || {
        let transient = SecretsLock {
            store: state,
            unlock,
            data_dir: Arc::new(Mutex::new(None)),
        };
        read_secret_scoped(&app, &transient, &scope, &key)
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_set_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
    key: String,
    value: String,
) -> Result<(), String> {
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || {
        let result = scoped_set_locked(&app, &state, unlock.as_ref(), &scope, &key, &value);
        finish_mutation(&app_for_dump, &state, unlock.as_ref(), result)
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_delete_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
    key: String,
) -> Result<(), String> {
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    let app_for_dump = app.clone();
    tokio::task::spawn_blocking(move || {
        let result = scoped_delete_locked(&app, &state, unlock.as_ref(), &scope, &key);
        finish_mutation(&app_for_dump, &state, unlock.as_ref(), result)
    })
    .await
    .map_err(|error| format!("join: {error}"))?
}

#[tauri::command]
pub async fn secrets_list_keys_scoped(
    app: AppHandle,
    lock: State<'_, SecretsLock>,
    scope: Scope,
) -> Result<Vec<String>, String> {
    let state = lock.store.clone();
    let unlock = lock.unlock.clone();
    tokio::task::spawn_blocking(move || scoped_list_locked(&app, &state, unlock.as_ref(), &scope))
        .await
        .map_err(|error| format!("join: {error}"))?
        .map_err(|error| error.to_string())
}

// ─── Backup helpers (phase 2) ────────────────────────────────────────────────

pub fn dump_all_kvs<R: Runtime>(
    app: &AppHandle<R>,
    lock: &SecretsLock,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    with_store(app, &lock.store, lock.unlock.as_ref(), |store| {
        store.export_all()
    })
    .map_err(|error| error.to_string())
}

pub fn bulk_set<R: Runtime>(
    app: &AppHandle<R>,
    lock: &SecretsLock,
    kvs: &std::collections::BTreeMap<String, String>,
) -> Result<usize, String> {
    let filtered: std::collections::BTreeMap<String, String> = kvs
        .iter()
        .map(|(name, value)| {
            // A restore re-imports names an earlier build may have written.
            validate_legacy_name(name).map_err(|error| StoreError::invalid(error))?;
            Ok((name.clone(), value.clone()))
        })
        .collect::<Result<_, StoreError>>()
        .map_err(|error| error.to_string())?;
    with_store(app, &lock.store, lock.unlock.as_ref(), |store| {
        store.import_all(&filtered)
    })
    .map_err(|error| error.to_string())
}

// ─── Runtime env-vault file (for the actions sidecar) ────────────────────────

/// Path to the runtime env-vault file. macOS uses `$TMPDIR`, Windows uses
/// `%LOCALAPPDATA%` (see `windows_local_appdata_base`), others use
/// `$XDG_RUNTIME_DIR` (with a `/tmp` fallback). chmod 600 on unix.
pub fn runtime_env_vault_path() -> PathBuf {
    let base: PathBuf = if cfg!(windows) {
        windows_secrets_base()
    } else if cfg!(target_os = "macos") {
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
/// if neither env var is set; `%LOCALAPPDATA%` on Windows. chmod 600 on
/// unix. With no passphrase configured it is NOT cleaned up on app quit —
/// the file's whole purpose is to survive the shell being closed. With a
/// WP-34 passphrase configured it is plaintext only while unlocked and is
/// overwritten on lock, idle expiry and exit (see the module docs).
pub fn durable_env_path() -> PathBuf {
    let base: PathBuf = if cfg!(windows) {
        windows_secrets_base()
    } else if cfg!(target_os = "macos") {
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

/// Windows base dir for both env files. Without this branch the unix
/// fallbacks resolved `/tmp` to `C:\tmp`, a folder any local user can read
/// (and Authenticated Users can modify), and chmod 0600 is a no-op there.
/// `%LOCALAPPDATA%` sits in the user profile, which is ACL'd to that user
/// by default. Local rather than Roaming (`%APPDATA%`) on purpose: Roaming
/// is synced to domain profile servers, and these files hold plaintext
/// secrets that must stay on this machine.
fn windows_secrets_base() -> PathBuf {
    windows_local_appdata_base(
        std::env::var_os("LOCALAPPDATA"),
        crate::platform::home_dir(),
    )
    // No profile hint at all (never on a real user session): the Win32
    // temp dir is still per-user, unlike `C:\tmp`.
    .unwrap_or_else(std::env::temp_dir)
}

/// Pure resolution for `windows_secrets_base`, split out so the Linux CI
/// can test it: `%LOCALAPPDATA%`, else `<home>\AppData\Local`.
fn windows_local_appdata_base(
    local_appdata: Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    local_appdata
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| home.map(|h| h.join("AppData").join("Local")))
}

/// Where builds before the Windows branch above wrote both env files on
/// Windows (`C:\tmp\ikenga-actions`). Resolved exactly as the old code did.
#[cfg(windows)]
fn legacy_windows_tmp_dir() -> PathBuf {
    PathBuf::from("/tmp").join("ikenga-actions")
}

/// True for a symlink or, on Windows, any reparse point (junctions, mount
/// points). The legacy cleanup never follows or deletes through these.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_link_or_reparse_point(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Best-effort removal of the legacy env copies in `dir` (and `dir` itself
/// if that leaves it empty). Returns how many files were removed.
///
/// `C:\tmp` is writable by every authenticated user, so another local user
/// could swap `ikenga-actions` (or a file in it) for a junction/symlink
/// into this user's profile. Everything is checked with `symlink_metadata`
/// first: links and reparse points are skipped, and only regular files are
/// removed. Logs carry paths only, never file contents.
#[cfg_attr(not(windows), allow(dead_code))]
fn remove_legacy_env_copies(dir: &std::path::Path) -> usize {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if is_link_or_reparse_point(&meta) => {
            tracing::warn!(
                "vault: legacy env dir {} is a symlink or reparse point; skipping cleanup",
                dir.display()
            );
            return 0;
        }
        Ok(meta) if meta.is_dir() => {}
        // Absent (the common case) or not a directory: nothing to do.
        _ => return 0,
    }
    let mut removed = 0;
    for name in ["env-vault", "env"] {
        let path = dir.join(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if is_link_or_reparse_point(&meta) {
            tracing::warn!(
                "vault: legacy env file {} is a symlink or reparse point; not removing",
                path.display()
            );
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(
                "vault: could not remove legacy env file {}: {e}",
                path.display()
            ),
        }
    }
    // `remove_dir` refuses non-empty dirs, so anything else in there stays.
    let _ = std::fs::remove_dir(dir);
    removed
}

fn cleanup_legacy_windows_tmp_copies() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir = legacy_windows_tmp_dir();
            let removed = remove_legacy_env_copies(&dir);
            if removed > 0 {
                tracing::info!(
                    "vault: removed {removed} legacy env file(s) from {}",
                    dir.display()
                );
            }
        });
    }
}

fn shell_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' | '$' => {
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

fn dump_to_runtime_file_locked<R: Runtime>(
    app: &AppHandle<R>,
    state: &SharedSecretStoreSlot,
    unlock: &UnlockState,
) -> Result<PathBuf, String> {
    cleanup_legacy_windows_tmp_copies();
    let pending = env_vault_pending_path(app)?;
    if pending.exists() {
        invalidate_env_vault_outputs(app)?;
    }
    let active_pid = resolve_active_project_blocking(app);
    // Captured before any value is read; re-checked under the publish mutex.
    let generation = unlock.generation();
    let body = match with_store(app, state, unlock, |store| {
        store.probe()?;
        let names = env_vault_names(
            store.list_meta()?.into_iter().map(|meta| meta.name),
            &active_pid,
        );

        let mut body = String::from("# Auto-generated by ikenga-desktop. Do not edit.\n");
        for name in &names {
            if !safe_env_name(name) {
                return Err(StoreError::uncommitted(format!(
                    "env-vault cannot safely serialize secret name `{name}`"
                )));
            }
            let candidates: [String; 3] = [
                format!("project::{active_pid}::{name}"),
                format!("workspace::{name}"),
                name.clone(),
            ];
            let mut value: Option<String> = None;
            for candidate in &candidates {
                if let Some(found) = store.get(candidate)? {
                    if !safe_env_value(&found) {
                        return Err(StoreError::uncommitted(format!(
                            "env-vault cannot safely serialize secret `{name}`"
                        )));
                    }
                    value = Some(found);
                    break;
                }
            }
            let Some(value) = value else { continue };
            body.push_str(name);
            body.push('=');
            body.push_str(&shell_escape(&value));
            body.push('\n');
        }

        Ok(body)
    }) {
        Ok(body) => body,
        Err(error) => {
            let invalidation = invalidate_env_vault_outputs(app);
            return match invalidation {
                Ok(()) => Err(format!("env-vault publication blocked: {error}")),
                Err(invalidation) => Err(format!(
                    "env-vault publication blocked: {error}; output invalidation failed: {invalidation}"
                )),
            };
        }
    };
    // Re-check under the env-publish mutex: a lock or idle expiry that
    // landed while values were being read must win. Every lock path advances
    // the generation before it invalidates, and invalidation takes this same
    // mutex, so either the change is visible here (abort) or the
    // invalidation runs after this publish and overwrites it.
    let _publish = env_publish_guard();
    match stale_publication_action(generation, unlock.generation(), unlock.state().locked) {
        StalePublication::Publish => {}
        StalePublication::Invalidate => {
            let invalidation = invalidate_env_vault_outputs_unguarded(app);
            return Err(match invalidation {
                Ok(()) => {
                    "env-vault publication aborted: secrets locked during publication".into()
                }
                Err(invalidation) => format!(
                    "env-vault publication aborted: secrets locked during publication; output invalidation failed: {invalidation}"
                ),
            });
        }
        StalePublication::Drop => {
            // Locked and re-unlocked while this body was being read: the
            // unlock publishes (or invalidates) on its own, and its dump read
            // the store after this caller's mutation. Invalidating here would
            // wipe that newer publication and starve the daemon, so the stale
            // body is simply dropped and the files are left alone.
            log::debug!("[secrets] stale env-vault publication dropped (vault re-unlocked)");
            return Ok(runtime_env_vault_path());
        }
    }
    clear_env_deny_state(app)?;
    publish_env_vaults_unguarded(app, &body)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StalePublication {
    /// Nothing changed since the values were read.
    Publish,
    /// The vault is locked now: overwrite both files with the deny body.
    Invalidate,
    /// The generation moved but the vault is unlocked again: a newer
    /// publication owns the files; drop this body without touching them.
    Drop,
}

/// Decide what a publication does with the body it read at
/// `captured_generation`, given the lock state under the publish mutex.
fn stale_publication_action(
    captured_generation: u64,
    current_generation: u64,
    locked: bool,
) -> StalePublication {
    if locked {
        StalePublication::Invalidate
    } else if captured_generation != current_generation {
        StalePublication::Drop
    } else {
        StalePublication::Publish
    }
}

/// The env-var names an env-vault publication for `active_pid` resolves,
/// from the store's entry names: bare names, workspace keys and the active
/// project's keys. Skipped with a name-only warning, instead of failing the
/// whole publication for every consumer: a name with a scope prefix that
/// still does not classify (it is not a bare name), and any name that is not
/// a valid environment variable name (e.g. a WP-33 key such as `a::b` or
/// `My Token`).
fn env_vault_names(
    entries: impl IntoIterator<Item = String>,
    active_pid: &str,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for name in entries {
        let key = match parse_scoped(&name) {
            None if has_scope_prefix(&name) => {
                log::warn!("[secrets] env-vault skipped `{name}`: its scope prefix does not parse");
                continue;
            }
            None => name.clone(),
            Some((Scope::Workspace, key)) => key,
            Some((Scope::Project { id }, key)) if id == active_pid => key,
            Some((Scope::Project { .. }, _)) | Some((Scope::Pkg { .. }, _)) => continue,
        };
        if !safe_env_name(&key) {
            log::warn!(
                "[secrets] env-vault skipped `{name}`: not a valid environment variable name"
            );
            continue;
        }
        names.insert(key);
    }
    names
}

/// Serialises env-vault publication against env-vault invalidation so a
/// lock can never be overtaken by a publish that read values before it.
static ENV_PUBLISH_LOCK: Mutex<()> = Mutex::new(());

fn env_publish_guard() -> std::sync::MutexGuard<'static, ()> {
    // The guarded data is `()`; a panic elsewhere leaves nothing to repair.
    ENV_PUBLISH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn safe_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '.'))
    {
        return false;
    }
    let line = format!("{name}=value\n");
    let mut parsed = dotenvy::from_read_iter(line.as_bytes());
    let Some(first_value) = parsed.next() else {
        return false;
    };
    let Ok((parsed_name, _)) = first_value else {
        return false;
    };
    parsed_name == name && parsed.next().is_none()
}

fn safe_env_value(value: &str) -> bool {
    if value.contains('\0') {
        return false;
    }
    let line = format!("IKENGA_VALUE={}\n", shell_escape(value));
    let mut parsed = dotenvy::from_read_iter(line.as_bytes());
    let Some(first_value) = parsed.next() else {
        return false;
    };
    let Ok((name, parsed_value)) = first_value else {
        return false;
    };
    name == "IKENGA_VALUE" && parsed_value == value && parsed.next().is_none()
}

fn env_vault_pending_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(ENV_VAULT_PENDING_FILENAME))
        .map_err(|error| format!("app_data_dir: {error}"))
}

fn env_vault_denied_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|dir| dir.join(ENV_VAULT_DENIED_FILENAME))
        .map_err(|error| format!("app_data_dir: {error}"))
}

fn ensure_private_env_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut directories = parent
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect::<Vec<_>>();
    directories.reverse();
    for directory in directories {
        match std::fs::symlink_metadata(directory) {
            Ok(metadata) => {
                if is_link_or_reparse_point(&metadata) {
                    return Err(format!(
                        "refusing linked env-vault directory {}",
                        directory.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Err(format!(
                        "env-vault parent is not a directory: {}",
                        directory.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(directory).map_err(|error| {
                    format!(
                        "create env-vault directory {}: {error}",
                        directory.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "inspect env-vault directory {}: {error}",
                    directory.display()
                ));
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| format!("protect env-vault directory {}: {error}", parent.display()),
        )?;
    }
    Ok(())
}

/// Caller must hold `env_publish_guard()`.
fn publish_env_vaults_unguarded<R: Runtime>(
    app: &AppHandle<R>,
    body: &str,
) -> Result<PathBuf, String> {
    let runtime = runtime_env_vault_path();
    let durable = durable_env_path();
    let pending = env_vault_pending_path(app)?;
    let denied = env_vault_denied_path(app)?;
    if denied.exists() {
        return Err("env-vault publication is denied by durable state".into());
    }
    let result = (|| {
        ensure_private_env_parent(&runtime)?;
        ensure_private_env_parent(&durable)?;
        crate::secrets::index::write_atomic(&pending, b"pending")?;
        crate::secrets::index::write_atomic(&runtime, body.as_bytes())?;
        crate::secrets::index::write_atomic(&durable, body.as_bytes())?;
        remove_env_file(&pending)
    })();
    match result {
        Ok(()) => Ok(runtime),
        Err(error) => {
            let invalidation = invalidate_env_vault_outputs_unguarded(app);
            match invalidation {
                Ok(()) => Err(format!("env-vault publication failed: {error}")),
                Err(invalidation) => Err(format!(
                    "env-vault publication failed: {error}; output invalidation failed: {invalidation}"
                )),
            }
        }
    }
}

fn invalidate_env_vault_outputs<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let _publish = env_publish_guard();
    invalidate_env_vault_outputs_unguarded(app)
}

/// Caller must hold `env_publish_guard()`.
fn invalidate_env_vault_outputs_unguarded<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    invalidate_env_paths(
        &runtime_env_vault_path(),
        &durable_env_path(),
        &env_vault_denied_path(app)?,
        &env_vault_pending_path(app)?,
    )
}

fn invalidate_env_paths(
    runtime: &Path,
    durable: &Path,
    denied: &Path,
    pending: &Path,
) -> Result<(), String> {
    let mut failures = Vec::new();
    if let Err(error) = crate::secrets::index::write_atomic(denied, b"denied") {
        failures.push(format!("write env-vault deny state: {error}"));
    }
    for path in [runtime, durable] {
        if let Err(error) = ensure_private_env_parent(path) {
            failures.push(error);
            continue;
        }
        if let Err(error) = crate::secrets::index::write_atomic(path, ENV_VAULT_DENIED_BODY) {
            failures.push(format!("overwrite {}: {error}", path.display()));
        }
    }
    if let Err(error) = remove_env_file(pending) {
        failures.push(error);
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn clear_env_deny_state<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    remove_env_file(&env_vault_denied_path(app)?)
}

fn remove_env_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

pub fn invalidate_env_vaults<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    invalidate_env_vault_outputs(app)
}

pub fn dump_to_runtime_file<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let lock: State<'_, SecretsLock> = app
        .try_state::<SecretsLock>()
        .ok_or_else(|| "SecretsLock state not registered".to_string())?;
    dump_to_runtime_file_locked(app, &lock.store, lock.unlock.as_ref())
}

/// App exit hook for a passphrase-protected vault: drops (zeroizes) the
/// in-memory DEK and, when a passphrase is configured, overwrites both
/// env-vault files — including the durable one the daemon reads — with the
/// deny body and leaves the durable deny marker, exactly as an explicit lock
/// does. The next launch publishes again only after an unlock. With no
/// passphrase configured this is a no-op and the WP-33 durable-file contract
/// holds. Best-effort: failures are logged, never block shutdown.
pub fn wipe_env_vaults_on_exit<R: Runtime>(app: &AppHandle<R>) {
    let Some(lock) = app.try_state::<SecretsLock>() else {
        return;
    };
    let configured = lock.unlock.is_configured();
    if let Err(error) = lock.unlock.lock() {
        log::warn!("[secrets] could not drop the unlock key on exit: {error}");
    }
    if !configured {
        return;
    }
    if let Err(error) = invalidate_env_vaults(app) {
        log::warn!("[secrets] env-vault invalidation on exit failed: {error}");
    }
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
        assert_eq!(parse_scoped("workspace::"), None);
        assert_eq!(parse_scoped("project::::KEY"), None);
    }

    #[test]
    fn parse_scoped_classifies_wp33_legacy_scoped_names() {
        // Split at the first `::` after the prefix, exactly as WP-33 did.
        assert_eq!(
            parse_scoped("project::other::a::b"),
            Some((Scope::project("other"), "a::b".to_string()))
        );
        assert_eq!(
            parse_scoped("pkg::com.x.y::a::b"),
            Some((Scope::pkg("com.x.y"), "a::b".to_string()))
        );
        assert_eq!(
            parse_scoped("workspace::a::b"),
            Some((Scope::Workspace, "a::b".to_string()))
        );
        // Ids and keys outside today's strict charset still classify.
        assert_eq!(
            parse_scoped("project::My Project::My Token"),
            Some((Scope::project("My Project"), "My Token".to_string()))
        );
    }

    #[test]
    fn legacy_scoped_entries_resolve_to_their_stored_name_for_delete() {
        // `secrets_delete_scoped` / `secrets_get_scoped` address exactly the
        // stored legacy name ...
        assert_eq!(
            existing_scoped_name(&Scope::project("other"), "a::b").unwrap(),
            "project::other::a::b"
        );
        assert_eq!(
            existing_scoped_name(&Scope::project("My Project"), "My Token").unwrap(),
            "project::My Project::My Token"
        );
        assert_eq!(
            existing_scoped_name(&Scope::Workspace, "a::b").unwrap(),
            "workspace::a::b"
        );
        // ... and never one that classifies under a different scope.
        assert!(existing_scoped_name(&Scope::project("a::b"), "c").is_err());
        assert!(existing_scoped_name(&Scope::project("x:"), "y").is_err());
        assert!(existing_scoped_name(&Scope::project(""), "k").is_err());
        assert!(existing_scoped_name(&Scope::Workspace, "").is_err());
        assert!(existing_scoped_name(&Scope::Workspace, "__manifest").is_err());
        assert!(validate_existing_scope(&Scope::project("My Project")).is_ok());
        assert!(validate_existing_scope(&Scope::project("a::b")).is_err());
    }

    #[test]
    fn unscoped_commands_address_exactly_the_names_they_list() {
        // Bare legacy names, and scope-prefixed names that no longer
        // classify (their only handle), are addressable ...
        assert!(validate_existing_bare_name("LEGACY_KEY").is_ok());
        assert!(validate_existing_bare_name("My Token").is_ok());
        assert!(validate_existing_bare_name("foo::bar").is_ok());
        assert!(validate_existing_bare_name("project::onlyid").is_ok());
        assert!(validate_existing_bare_name("workspace::").is_ok());
        // ... a classifiable scoped name is not.
        assert!(validate_existing_bare_name("workspace::KEY").is_err());
        assert!(validate_existing_bare_name("project::other::a::b").is_err());
        assert!(validate_existing_bare_name("pkg::com.x.y::KEY").is_err());
        assert!(validate_existing_bare_name("__manifest").is_err());
        assert!(validate_existing_bare_name("").is_err());
    }

    #[test]
    fn env_vault_names_skip_unclassifiable_and_unsafe_names() {
        let entries = [
            "LEGACY",
            "workspace::WS",
            "project::alpha::P",
            "project::beta::Q",
            "pkg::com.x.y::R",
            // Scope prefix that does not classify: skipped, not treated bare.
            "project::onlyid",
            // WP-33 scoped names whose key is not an env name: skipped
            // instead of failing publication for every consumer.
            "project::alpha::a::b",
            "workspace::My Token",
            "My Token",
        ]
        .into_iter()
        .map(String::from);
        let names = env_vault_names(entries, "alpha");
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            vec!["LEGACY".to_string(), "P".to_string(), "WS".to_string()]
        );
    }

    #[test]
    fn stale_publication_invalidates_only_when_locked() {
        assert_eq!(stale_publication_action(3, 3, false), StalePublication::Publish);
        // Re-unlocked since the body was read: a newer dump owns the files.
        assert_eq!(stale_publication_action(3, 4, false), StalePublication::Drop);
        assert_eq!(stale_publication_action(3, 4, true), StalePublication::Invalidate);
        assert_eq!(stale_publication_action(3, 3, true), StalePublication::Invalidate);
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
        // Component-wise so the `\` separator on Windows still matches.
        assert!(
            p.ends_with(std::path::Path::new("ikenga-actions").join("env")),
            "durable_env_path should end with ikenga-actions/env, got: {s}"
        );
        // Must NOT be inside a volatile tmp/runtime dir.
        assert!(
            !s.contains("/run/user/") && !s.starts_with("/tmp"),
            "durable_env_path must not point to a volatile runtime dir, got: {s}"
        );
    }

    #[test]
    fn windows_base_prefers_local_appdata_then_home() {
        let local = PathBuf::from("C:/Users/me/AppData/Local");
        assert_eq!(
            windows_local_appdata_base(
                Some(local.clone().into_os_string()),
                Some(PathBuf::from("C:/Users/other"))
            ),
            Some(local)
        );
        // Empty LOCALAPPDATA falls through to the profile dir.
        let home = PathBuf::from("C:/Users/me");
        assert_eq!(
            windows_local_appdata_base(Some("".into()), Some(home.clone())),
            Some(home.join("AppData").join("Local"))
        );
        // Nothing to go on: no silent `/tmp` default.
        assert_eq!(windows_local_appdata_base(None, None), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_env_paths_are_not_under_tmp() {
        let legacy = legacy_windows_tmp_dir();
        for p in [runtime_env_vault_path(), durable_env_path()] {
            assert!(
                !p.starts_with(&legacy) && !p.starts_with("/tmp") && !p.starts_with("C:\\tmp"),
                "Windows env path must not be under C:\\tmp, got: {}",
                p.display()
            );
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            assert!(runtime_env_vault_path().starts_with(PathBuf::from(&local)));
            assert!(durable_env_path().starts_with(PathBuf::from(&local)));
        }
    }

    #[test]
    fn remove_legacy_env_copies_deletes_files_and_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ikenga-actions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("env-vault"), "K=\"v\"\n").unwrap();
        std::fs::write(dir.join("env"), "K=\"v\"\n").unwrap();
        assert_eq!(remove_legacy_env_copies(&dir), 2);
        assert!(!dir.exists());
        // Idempotent, and a dir holding anything else is left in place.
        assert_eq!(remove_legacy_env_copies(&dir), 0);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("unrelated"), "x").unwrap();
        assert_eq!(remove_legacy_env_copies(&dir), 0);
        assert!(dir.join("unrelated").exists());
        // A same-named directory is not a regular file and is left alone.
        std::fs::create_dir_all(dir.join("env")).unwrap();
        assert_eq!(remove_legacy_env_copies(&dir), 0);
        assert!(dir.join("env").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn remove_legacy_env_copies_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        // Stand-in for a file in the victim's profile.
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("env"), "SECRET=\"v\"\n").unwrap();
        std::fs::write(victim.join("env-vault"), "SECRET=\"v\"\n").unwrap();

        // Symlinked files inside a real legacy dir: the links stay, and
        // their targets are untouched.
        let dir = tmp.path().join("ikenga-actions");
        std::fs::create_dir_all(&dir).unwrap();
        symlink(victim.join("env"), dir.join("env")).unwrap();
        std::fs::write(dir.join("env-vault"), "K=\"v\"\n").unwrap();
        assert_eq!(remove_legacy_env_copies(&dir), 1);
        assert!(victim.join("env").exists());
        assert!(std::fs::symlink_metadata(dir.join("env")).is_ok());
        assert!(!dir.join("env-vault").exists());

        // The legacy dir itself swapped for a symlink: nothing is removed.
        let linked_dir = tmp.path().join("ikenga-actions-link");
        symlink(&victim, &linked_dir).unwrap();
        assert_eq!(remove_legacy_env_copies(&linked_dir), 0);
        assert!(victim.join("env").exists());
        assert!(victim.join("env-vault").exists());
        assert!(std::fs::symlink_metadata(&linked_dir).is_ok());
    }

    #[test]
    fn env_serialization_rejects_injection_characters() {
        assert!(safe_env_name("OPENAI_API_KEY"));
        assert!(!safe_env_name("OPENAI=API_KEY"));
        assert!(!safe_env_name("OPENAI\nAPI_KEY"));
        assert!(safe_env_name("OPENAI.API_KEY"));
        assert!(safe_env_value("plain-value"));
        assert!(safe_env_value("line\nbreak"));
        assert!(safe_env_value("carriage\rreturn"));
        assert!(safe_env_value("token=override"));
        assert!(safe_env_value("bell\u{0007}"));
    }

    #[cfg(unix)]
    #[test]
    fn env_vault_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("linked");
        symlink(&target, &link).unwrap();
        let error = ensure_private_env_parent(&link.join("env-vault")).unwrap_err();
        assert!(error.contains("linked env-vault directory"));
        assert!(!target.join("env-vault").exists());
    }

    #[test]
    fn env_invalidation_overwrites_plaintext_and_sets_deny_state() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        let durable = dir.path().join("durable");
        let denied = dir.path().join("denied");
        let pending = dir.path().join("pending");
        std::fs::write(&runtime, "TOKEN=\"secret\"\n").unwrap();
        std::fs::write(&durable, "TOKEN=\"secret\"\n").unwrap();
        std::fs::write(&pending, b"pending").unwrap();

        invalidate_env_paths(&runtime, &durable, &denied, &pending).unwrap();
        assert_eq!(std::fs::read(&runtime).unwrap(), ENV_VAULT_DENIED_BODY);
        assert_eq!(std::fs::read(&durable).unwrap(), ENV_VAULT_DENIED_BODY);
        assert!(denied.exists());
        assert!(!pending.exists());
    }

    #[test]
    fn env_serialization_escapes_quoted_shell_characters() {
        assert_eq!(shell_escape("a\"b\\c$d`e"), "\"a\\\"b\\\\c\\$d`e\"");
        let value = "a=b$c`d\\e\"f\ng";
        let line = format!("K={}\n", shell_escape(value));
        let parsed = dotenvy::from_read_iter(line.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(parsed, ("K".to_string(), value.to_string()));
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
