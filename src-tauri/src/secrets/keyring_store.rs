//! Persistent secret storage backed by the platform credential store.
//!
//! The earlier WP-33 Linux attempt treated the keyring as one interchangeable
//! backend. That was wrong: on Linux, Secret Service can be unavailable while
//! keyutils still answers, and a keyutils hit is not proof of authoritative
//! persistence. Secret Service is therefore the authoritative backend. Keyutils
//! is a read-only fallback for a read that cannot reach Secret Service; it is
//! never written or deleted by this module. Every mutation, probe, import, and
//! migration uses Secret Service directly and fails closed when it is absent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::index::{item_name, pending_path, validate_legacy_name, IndexPending, SecretIndex};
use super::store::{SecretMeta, SecretsStore, StoreError};

const SERVICE: &str = "ikenga";
const USER: &str = "secret";
const KEYUTILS_DISABLED_FILENAME: &str = "keyutils-cache.disabled";
const MIGRATION_ARTIFACTS: &[&str] = &[
    "secrets.stronghold",
    "secrets.stronghold.bak",
    "secrets.stronghold.migrated",
    "secrets-migration.json",
    "secrets-migration.rollback.json",
    "secrets-unlock.json",
    KEYUTILS_DISABLED_FILENAME,
];

#[derive(Debug, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct BackendError {
    message: String,
    cache_fallback_allowed: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl BackendError {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cache_fallback_allowed: true,
        }
    }

    fn rejected(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cache_fallback_allowed: false,
        }
    }

    fn allows_cache_fallback(&self) -> bool {
        self.cache_fallback_allowed
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

type BackendResult<T> = Result<T, BackendError>;

fn backend_store_error(error: BackendError) -> StoreError {
    if error.allows_cache_fallback() {
        StoreError::unavailable(error.to_string())
    } else {
        StoreError::unknown(error.to_string())
    }
}

trait CredentialProvider: Send + Sync {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>>;
    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()>;
    fn delete(&self, item: &str) -> BackendResult<()>;
}

#[allow(dead_code)]
trait CredentialReader: Send + Sync {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>>;
}

trait SecretBackend: Send + Sync {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>>;
    fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>>;
    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()>;
    fn delete(&self, item: &str) -> BackendResult<()>;
    fn label(&self) -> &'static str;
    fn diagnostic(&self) -> Option<String> {
        None
    }
}

fn keyring_error(error: keyring::Error) -> BackendError {
    match error {
        keyring::Error::NoEntry => BackendError::rejected("keychain entry not found"),
        keyring::Error::BadEncoding(_) => BackendError::rejected("keychain value is not UTF-8"),
        keyring::Error::TooLong(name, limit) => {
            BackendError::rejected(format!("keychain attribute {name} exceeds limit {limit}"))
        }
        keyring::Error::Invalid(name, reason) => {
            BackendError::rejected(format!("keychain rejected attribute {name}: {reason}"))
        }
        keyring::Error::NoStorageAccess(_) => {
            BackendError::unavailable("platform keychain storage is unavailable or locked")
        }
        keyring::Error::PlatformFailure(_) => {
            BackendError::unavailable("platform keychain operation failed")
        }
        keyring::Error::Ambiguous(_) => {
            BackendError::rejected("platform keychain returned multiple matching entries")
        }
        _ => BackendError::rejected("unknown keychain error"),
    }
}

fn get_entry(entry: &keyring::Entry) -> BackendResult<Option<Vec<u8>>> {
    match entry.get_secret() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(keyring_error(error)),
    }
}

fn set_entry(entry: &keyring::Entry, value: &[u8]) -> BackendResult<()> {
    entry.set_secret(value).map_err(keyring_error)
}

fn delete_entry(entry: &keyring::Entry) -> BackendResult<()> {
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(keyring_error(error)),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn native_entry(item: &str) -> BackendResult<keyring::Entry> {
    #[cfg(target_os = "windows")]
    {
        return keyring::Entry::new_with_target(item, SERVICE, USER).map_err(keyring_error);
    }
    #[cfg(target_os = "macos")]
    {
        return keyring::Entry::new(SERVICE, item).map_err(keyring_error);
    }
    #[allow(unreachable_code)]
    Err(BackendError::rejected("unsupported platform keychain"))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeProvider;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CredentialProvider for NativeProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        get_entry(&native_entry(item)?)
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        set_entry(&native_entry(item)?, value)
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        delete_entry(&native_entry(item)?)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeSecretBackend;

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretBackend for NativeSecretBackend {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        NativeProvider.get(item)
    }

    fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        NativeProvider.get(item)
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        NativeProvider.set(item, value)
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        NativeProvider.delete(item)
    }

    fn label(&self) -> &'static str {
        native_backend_label()
    }
}

#[cfg(target_os = "linux")]
struct KeyutilsProvider;

#[cfg(target_os = "linux")]
impl CredentialReader for KeyutilsProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        let credential =
            keyring::keyutils::KeyutilsCredential::new_with_target(Some(item), SERVICE, USER)
                .map_err(keyring_error)?;
        get_entry(&keyring::Entry::new_with_credential(Box::new(credential)))
    }
}

#[cfg(target_os = "linux")]
struct SecretServiceProvider;

#[cfg(target_os = "linux")]
impl CredentialProvider for SecretServiceProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        let credential =
            keyring::secret_service::SsCredential::new_with_target(Some("default"), SERVICE, item)
                .map_err(keyring_error)?;
        get_entry(&keyring::Entry::new_with_credential(Box::new(credential)))
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        let credential =
            keyring::secret_service::SsCredential::new_with_target(Some("default"), SERVICE, item)
                .map_err(keyring_error)?;
        set_entry(
            &keyring::Entry::new_with_credential(Box::new(credential)),
            value,
        )
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        let credential =
            keyring::secret_service::SsCredential::new_with_target(Some("default"), SERVICE, item)
                .map_err(keyring_error)?;
        delete_entry(&keyring::Entry::new_with_credential(Box::new(credential)))
    }
}

#[allow(dead_code)]
struct LinuxSecretBackend {
    secret_service: Arc<dyn CredentialProvider>,
    keyutils_cache: Arc<dyn CredentialReader>,
    cache_disabled: AtomicBool,
    cache_disabled_path: Option<PathBuf>,
    diagnostic: Mutex<Option<String>>,
}

#[allow(dead_code)]
impl LinuxSecretBackend {
    fn new(
        secret_service: Arc<dyn CredentialProvider>,
        keyutils_cache: Arc<dyn CredentialReader>,
        cache_disabled_path: Option<PathBuf>,
    ) -> Self {
        let cache_disabled = cache_disabled_path
            .as_ref()
            .map(|path| path.exists())
            .unwrap_or(false);
        Self {
            secret_service,
            keyutils_cache,
            cache_disabled: AtomicBool::new(cache_disabled),
            cache_disabled_path,
            diagnostic: Mutex::new(if cache_disabled {
                Some("keyutils cache disabled by durable marker".to_string())
            } else {
                None
            }),
        }
    }

    fn cache_is_disabled(&self) -> bool {
        self.cache_disabled.load(Ordering::Acquire)
            || self
                .cache_disabled_path
                .as_ref()
                .map(|path| path.exists())
                .unwrap_or(false)
    }
}

impl SecretBackend for LinuxSecretBackend {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        match self.secret_service.get(item) {
            Ok(value) => Ok(value),
            Err(secret_service_error) => {
                if !secret_service_error.allows_cache_fallback() {
                    return Err(secret_service_error);
                }
                if self.cache_is_disabled() {
                    return Err(BackendError::rejected(format!(
                        "Secret Service read failed: {secret_service_error}; keyutils cache is disabled"
                    )));
                }
                match self.keyutils_cache.get(item) {
                    Ok(Some(value)) => Ok(Some(value)),
                    Ok(None) => Err(BackendError::unavailable(format!(
                        "Secret Service read failed and keyutils cache has no entry: {secret_service_error}"
                    ))),
                    Err(cache_error) => Err(BackendError::unavailable(format!(
                        "Secret Service read failed: {secret_service_error}; keyutils cache read failed: {cache_error}"
                    ))),
                }
            }
        }
    }

    fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        self.secret_service.get(item)
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        self.secret_service.set(item, value).map_err(|error| {
            BackendError::rejected(format!("Secret Service write failed: {error}"))
        })
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        self.secret_service.delete(item).map_err(|error| {
            BackendError::rejected(format!("Secret Service delete failed: {error}"))
        })
    }

    fn label(&self) -> &'static str {
        "Secret Service with read-only Linux keyutils fallback"
    }

    fn diagnostic(&self) -> Option<String> {
        self.diagnostic.lock().ok().and_then(|value| value.clone())
    }
}

#[cfg(target_os = "linux")]
fn platform_backend(index_path: &Path) -> Arc<dyn SecretBackend> {
    let cache_disabled_path = index_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(KEYUTILS_DISABLED_FILENAME);
    Arc::new(LinuxSecretBackend::new(
        Arc::new(SecretServiceProvider),
        Arc::new(KeyutilsProvider),
        Some(cache_disabled_path),
    ))
}

#[cfg(target_os = "macos")]
fn platform_backend(_index_path: &Path) -> Arc<dyn SecretBackend> {
    Arc::new(NativeSecretBackend)
}

#[cfg(target_os = "windows")]
fn platform_backend(_index_path: &Path) -> Arc<dyn SecretBackend> {
    Arc::new(NativeSecretBackend)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_backend(_index_path: &Path) -> Arc<dyn SecretBackend> {
    Arc::new(UnsupportedBackend)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
struct UnsupportedBackend;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl SecretBackend for UnsupportedBackend {
    fn get(&self, _item: &str) -> BackendResult<Option<Vec<u8>>> {
        Err(BackendError::rejected("unsupported platform keychain"))
    }

    fn get_authoritative(&self, _item: &str) -> BackendResult<Option<Vec<u8>>> {
        Err(BackendError::rejected("unsupported platform keychain"))
    }

    fn set(&self, _item: &str, _value: &[u8]) -> BackendResult<()> {
        Err(BackendError::rejected("unsupported platform keychain"))
    }

    fn delete(&self, _item: &str) -> BackendResult<()> {
        Err(BackendError::rejected("unsupported platform keychain"))
    }

    fn label(&self) -> &'static str {
        "unsupported"
    }
}

fn native_backend_label() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "Secret Service with read-only Linux keyutils fallback"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS Keychain"
    }
    #[cfg(target_os = "windows")]
    {
        "Windows Credential Manager"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "unsupported"
    }
}

pub fn backend_label() -> &'static str {
    native_backend_label()
}

pub struct KeyringStore {
    backend: Arc<dyn SecretBackend>,
    index: Mutex<SecretIndex>,
    pending_path: PathBuf,
}

impl KeyringStore {
    pub fn new(index_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = index_path.as_ref().to_path_buf();
        Self::with_backend_config(&path, platform_backend(&path), false)
    }

    pub(crate) fn new_for_migration(index_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = index_path.as_ref().to_path_buf();
        Self::with_backend_config(&path, platform_backend(&path), true)
    }

    pub fn persist_index(&self) -> Result<(), StoreError> {
        self.lock_index()?.save().map_err(StoreError::uncommitted)
    }

    #[cfg(test)]
    fn with_backend(
        index_path: impl AsRef<Path>,
        backend: Arc<dyn SecretBackend>,
    ) -> Result<Self, StoreError> {
        Self::with_backend_config(index_path, backend, false)
    }

    fn with_backend_config(
        index_path: impl AsRef<Path>,
        backend: Arc<dyn SecretBackend>,
        allow_missing_index: bool,
    ) -> Result<Self, StoreError> {
        let path = index_path.as_ref().to_path_buf();
        let artifacts = migration_artifacts_present(&path);
        let blocking_artifacts = migration_artifacts_present_except_legacy(&path);
        if !path.exists()
            && ((!allow_missing_index && artifacts) || (allow_missing_index && blocking_artifacts))
        {
            return Err(StoreError::uncommitted(format!(
                "secrets index is missing while migration artifacts exist: {}",
                path.display()
            )));
        }
        let pending = pending_path(&path);
        let mut index = SecretIndex::load(&path).map_err(StoreError::uncommitted)?;
        recover_pending(&mut index, backend.as_ref(), &pending)?;
        Ok(Self {
            backend,
            index: Mutex::new(index),
            pending_path: pending,
        })
    }

    fn lock_index(&self) -> Result<std::sync::MutexGuard<'_, SecretIndex>, StoreError> {
        self.index.lock().map_err(|error| {
            StoreError::uncommitted(format!("secrets index lock poisoned: {error}"))
        })
    }

    fn authoritative_snapshot(
        &self,
        index: &SecretIndex,
    ) -> Result<BTreeMap<String, Option<Vec<u8>>>, StoreError> {
        let mut out = BTreeMap::new();
        for name in index.names() {
            let item = item_name(&name).map_err(StoreError::uncommitted)?;
            let value = self.backend.get_authoritative(&item).map_err(|error| {
                StoreError::uncommitted(format!(
                    "authoritative keychain read failed for `{name}`: {error}"
                ))
            })?;
            if value.is_none() {
                return Err(StoreError::uncommitted(format!(
                    "indexed secret is missing from authoritative storage: {name}"
                )));
            }
            out.insert(name, value);
        }
        Ok(out)
    }

    fn write_pending(
        &self,
        upserts: &BTreeSet<String>,
        deletes: &BTreeSet<String>,
    ) -> Result<(), StoreError> {
        let pending = IndexPending {
            upserts: upserts.clone(),
            deletes: deletes.clone(),
        };
        pending
            .save(&self.pending_path)
            .map_err(StoreError::uncommitted)
    }

    fn restore_snapshot(
        &self,
        previous: &BTreeMap<String, Option<Vec<u8>>>,
        affected: &BTreeSet<String>,
    ) -> Result<(), StoreError> {
        let mut failure = None;
        for name in affected {
            if previous.contains_key(name) {
                continue;
            }
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.delete(&item) {
                failure = Some(format!("delete rollback item `{name}`: {error}"));
                break;
            }
        }
        if failure.is_none() {
            for (name, value) in previous {
                let item = item_name(name).map_err(StoreError::uncommitted)?;
                let result = match value {
                    Some(value) => self
                        .backend
                        .set(&item, value)
                        .map_err(|error| error.to_string()),
                    None => self
                        .backend
                        .delete(&item)
                        .map_err(|error| error.to_string()),
                };
                if let Err(error) = result {
                    failure = Some(format!("restore rollback item `{name}`: {error}"));
                    break;
                }
            }
        }
        match failure {
            Some(error) => Err(StoreError::committed(format!(
                "keychain rollback failed: {error}"
            ))),
            None => Ok(()),
        }
    }

    fn verify_values(&self, values: &BTreeMap<String, String>) -> Result<(), StoreError> {
        for (name, expected) in values {
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            match self.backend.get_authoritative(&item) {
                Ok(Some(actual)) if actual == expected.as_bytes() => {}
                Ok(_) => {
                    return Err(StoreError::committed(format!(
                        "authoritative keychain verification failed for `{name}`"
                    )))
                }
                Err(error) => {
                    return Err(StoreError::committed(format!(
                        "authoritative keychain verification failed for `{name}`: {error}"
                    )))
                }
            }
        }
        Ok(())
    }
}

fn migration_artifacts_present(index_path: &Path) -> bool {
    let parent = index_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    MIGRATION_ARTIFACTS
        .iter()
        .any(|artifact| parent.join(artifact).exists())
}

fn migration_artifacts_present_except_legacy(index_path: &Path) -> bool {
    let parent = index_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    MIGRATION_ARTIFACTS
        .iter()
        .filter(|artifact| **artifact != "secrets.stronghold")
        .any(|artifact| parent.join(artifact).exists())
}

fn recover_pending(
    index: &mut SecretIndex,
    backend: &dyn SecretBackend,
    path: &Path,
) -> Result<(), StoreError> {
    let pending = IndexPending::load(path).map_err(StoreError::uncommitted)?;
    if pending.upserts.is_empty() && pending.deletes.is_empty() {
        return Ok(());
    }
    let mut next = index.clone();
    let mut changed = false;
    for name in &pending.deletes {
        let item = item_name(name).map_err(StoreError::uncommitted)?;
        let authoritative = backend
            .get_authoritative(&item)
            .map_err(backend_store_error)?;
        if authoritative.is_none() {
            next.remove(name).map_err(StoreError::uncommitted)?;
            changed = true;
        }
    }
    for name in &pending.upserts {
        let item = item_name(name).map_err(StoreError::uncommitted)?;
        if backend
            .get_authoritative(&item)
            .map_err(backend_store_error)?
            .is_some()
        {
            next.insert(name).map_err(StoreError::uncommitted)?;
            changed = true;
        }
    }
    if changed {
        next.save().map_err(StoreError::committed)?;
        *index = next;
    }
    IndexPending::clear(path).map_err(StoreError::committed)
}

impl SecretsStore for KeyringStore {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
        let item = item_name(name).map_err(StoreError::uncommitted)?;
        let value = self.backend.get(&item).map_err(backend_store_error)?;
        value
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|_| StoreError::uncommitted("keychain value is not UTF-8".to_string()))
            })
            .transpose()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
        let mut index = self.lock_index()?;
        let item = item_name(name).map_err(StoreError::uncommitted)?;
        let previous_value_bytes = self.backend.get_authoritative(&item).map_err(|error| {
            StoreError::uncommitted(format!("read previous keychain value: {error}"))
        })?;
        let upserts = BTreeSet::from([name.to_string()]);
        self.write_pending(&upserts, &BTreeSet::new())?;
        if let Err(error) = self.backend.set(&item, value.as_bytes()) {
            let _ = IndexPending::clear(&self.pending_path);
            return Err(StoreError::uncommitted(format!(
                "write keychain value: {error}"
            )));
        }
        let mut next = index.clone();
        next.insert(name).map_err(StoreError::committed)?;
        if let Err(error) = next.save() {
            let affected = BTreeSet::from([name.to_string()]);
            let mut snapshot = BTreeMap::new();
            snapshot.insert(name.to_string(), previous_value_bytes);
            let rollback = self.restore_snapshot(&snapshot, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(StoreError::uncommitted(error)),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        *index = next;
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        let mut index = self.lock_index()?;
        let item = item_name(name).map_err(StoreError::uncommitted)?;
        let previous_value_bytes = self.backend.get_authoritative(&item).map_err(|error| {
            StoreError::uncommitted(format!("read previous keychain value: {error}"))
        })?;
        self.write_pending(&BTreeSet::new(), &BTreeSet::from([name.to_string()]))?;
        if let Err(error) = self.backend.delete(&item) {
            let _ = IndexPending::clear(&self.pending_path);
            return Err(StoreError::uncommitted(format!(
                "delete keychain value: {error}"
            )));
        }
        let mut next = index.clone();
        next.remove(name).map_err(StoreError::committed)?;
        if let Err(error) = next.save() {
            let affected = BTreeSet::from([name.to_string()]);
            let mut snapshot = BTreeMap::new();
            snapshot.insert(name.to_string(), previous_value_bytes);
            let rollback = self.restore_snapshot(&snapshot, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(StoreError::uncommitted(error)),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        *index = next;
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        let index = self.lock_index()?;
        Ok(index
            .names()
            .into_iter()
            .map(|name| SecretMeta { name })
            .collect())
    }

    fn export_all(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let names = self.lock_index()?.names();
        let mut out = BTreeMap::new();
        for name in names {
            match self.get(&name)? {
                Some(value) => {
                    out.insert(name, value);
                }
                None => {
                    return Err(StoreError::uncommitted(format!(
                        "indexed secret is missing: {name}"
                    )))
                }
            }
        }
        Ok(out)
    }

    fn import_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        if values.is_empty() {
            return Ok(0);
        }
        let mut index = self.lock_index()?;
        for name in values.keys() {
            validate_legacy_name(name).map_err(StoreError::uncommitted)?;
        }
        let mut previous = self.authoritative_snapshot(&index)?;
        for name in values.keys() {
            if previous.contains_key(name) {
                continue;
            }
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            let value = self.backend.get_authoritative(&item).map_err(|error| {
                StoreError::uncommitted(format!(
                    "read previous keychain value for `{name}`: {error}"
                ))
            })?;
            previous.insert(name.clone(), value);
        }
        let affected: BTreeSet<String> = values.keys().cloned().collect();
        self.write_pending(&affected, &BTreeSet::new())?;
        for (name, value) in values {
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.set(&item, value.as_bytes()) {
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(StoreError::uncommitted(format!(
                        "write keychain during import: {error}"
                    ))),
                    Err(rollback) => Err(StoreError::committed(format!(
                        "write keychain during import: {error}; {rollback}"
                    ))),
                };
            }
        }
        if let Err(error) = self.verify_values(values) {
            let rollback = self.restore_snapshot(&previous, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        let mut next = index.clone();
        for name in values.keys() {
            next.insert(name).map_err(StoreError::committed)?;
        }
        if let Err(error) = next.save() {
            let rollback = self.restore_snapshot(&previous, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(StoreError::uncommitted(error)),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        *index = next;
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)?;
        Ok(values.len())
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        let mut index = self.lock_index()?;
        for name in values.keys() {
            validate_legacy_name(name).map_err(StoreError::uncommitted)?;
        }
        let previous = self.authoritative_snapshot(&index)?;
        let target: BTreeSet<String> = values.keys().cloned().collect();
        let deletes: BTreeSet<String> = previous
            .keys()
            .filter(|name| !target.contains(*name))
            .cloned()
            .collect();
        let affected: BTreeSet<String> = previous.keys().chain(target.iter()).cloned().collect();
        self.write_pending(&target, &deletes)?;
        for name in &deletes {
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.delete(&item) {
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(StoreError::uncommitted(format!(
                        "delete keychain during replacement: {error}"
                    ))),
                    Err(rollback) => Err(StoreError::committed(format!(
                        "delete keychain during replacement: {error}; {rollback}"
                    ))),
                };
            }
        }
        for (name, value) in values {
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.set(&item, value.as_bytes()) {
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(StoreError::uncommitted(format!(
                        "write keychain during replacement: {error}"
                    ))),
                    Err(rollback) => Err(StoreError::committed(format!(
                        "write keychain during replacement: {error}; {rollback}"
                    ))),
                };
            }
        }
        if let Err(error) = self.verify_values(values) {
            let rollback = self.restore_snapshot(&previous, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        for name in &deletes {
            let item = item_name(name).map_err(StoreError::uncommitted)?;
            if self
                .backend
                .get_authoritative(&item)
                .map_err(|error| StoreError::committed(error.to_string()))?
                .is_some()
            {
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(StoreError::committed(format!(
                        "authoritative keychain still contains removed secret `{name}`"
                    ))),
                    Err(rollback) => Err(StoreError::committed(format!(
                        "authoritative keychain still contains removed secret `{name}`; {rollback}"
                    ))),
                };
            }
        }
        let mut next = index.clone();
        for name in index.names() {
            if !target.contains(&name) {
                next.remove(&name).map_err(StoreError::committed)?;
            }
        }
        for name in &target {
            next.insert(name).map_err(StoreError::committed)?;
        }
        if let Err(error) = next.save() {
            let rollback = self.restore_snapshot(&previous, &affected);
            let _ = IndexPending::clear(&self.pending_path);
            return match rollback {
                Ok(()) => Err(StoreError::uncommitted(error)),
                Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
            };
        }
        *index = next;
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)?;
        Ok(values.len())
    }

    fn probe(&self) -> Result<(), StoreError> {
        let token = uuid::Uuid::new_v4();
        let item = format!("ikenga:__probe::{}", token.simple());
        self.backend
            .set(&item, token.as_bytes())
            .map_err(backend_store_error)?;
        let read = self
            .backend
            .get_authoritative(&item)
            .map_err(backend_store_error);
        let delete = self.backend.delete(&item).map_err(backend_store_error);
        if let Err(error) = delete {
            return Err(error);
        }
        let read = read?;
        let expected: Vec<u8> = token.as_bytes().to_vec();
        if !read.is_some_and(|value| value == expected) {
            return Err(StoreError::uncommitted(
                "authoritative keychain probe read mismatch",
            ));
        }
        self.lock_index()?.save().map_err(StoreError::uncommitted)
    }

    fn prepare_encryption(&self) -> Result<(), StoreError> {
        // The keychain layer stores the bytes it is handed; value encryption
        // belongs to `EncryptedStore`, which wraps this store.
        Ok(())
    }

    fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.backend.diagnostic().into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeProvider {
        values: Mutex<BTreeMap<String, Vec<u8>>>,
        fail_reads: Mutex<bool>,
        reject_reads: Mutex<bool>,
        fail_writes: Mutex<bool>,
        fail_deletes: Mutex<bool>,
    }

    impl FakeProvider {
        fn insert(&self, item: &str, value: &[u8]) {
            self.values
                .lock()
                .unwrap()
                .insert(item.to_string(), value.to_vec());
        }

        fn get_item(&self, item: &str) -> Option<Vec<u8>> {
            self.values.lock().unwrap().get(item).cloned()
        }
    }

    impl CredentialProvider for FakeProvider {
        fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            if *self.reject_reads.lock().unwrap() {
                return Err(BackendError::rejected("read rejected"));
            }
            if *self.fail_reads.lock().unwrap() {
                return Err(BackendError::unavailable("read unavailable"));
            }
            Ok(self.get_item(item))
        }

        fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
            if *self.fail_writes.lock().unwrap() {
                return Err(BackendError::rejected("write unavailable"));
            }
            self.insert(item, value);
            Ok(())
        }

        fn delete(&self, item: &str) -> BackendResult<()> {
            if *self.fail_deletes.lock().unwrap() {
                return Err(BackendError::rejected("delete unavailable"));
            }
            self.values.lock().unwrap().remove(item);
            Ok(())
        }
    }

    impl CredentialReader for FakeProvider {
        fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            <Self as CredentialProvider>::get(self, item)
        }
    }

    struct FakeBackend {
        provider: Arc<FakeProvider>,
    }

    impl SecretBackend for FakeBackend {
        fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            CredentialProvider::get(&*self.provider, item)
        }

        fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            CredentialProvider::get(&*self.provider, item)
        }

        fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
            self.provider.set(item, value)
        }

        fn delete(&self, item: &str) -> BackendResult<()> {
            self.provider.delete(item)
        }

        fn label(&self) -> &'static str {
            "fake"
        }
    }

    fn linux_backend(
        secret_service: Arc<FakeProvider>,
        keyutils_cache: Arc<FakeProvider>,
        path: &Path,
    ) -> LinuxSecretBackend {
        LinuxSecretBackend::new(
            secret_service,
            keyutils_cache,
            Some(path.join(KEYUTILS_DISABLED_FILENAME)),
        )
    }

    #[test]
    fn store_round_trip_keeps_values_out_of_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        let store = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();

        store.set("workspace::TOKEN", "dummy-value").unwrap();
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("dummy-value")
        );
        let index = fs::read_to_string(path).unwrap();
        assert!(index.contains("workspace::TOKEN"));
        assert!(!index.contains("dummy-value"));
        assert_eq!(
            provider.get_item("ikenga:workspace::TOKEN"),
            Some(b"dummy-value".to_vec())
        );

        store.delete("workspace::TOKEN").unwrap();
        assert_eq!(store.get("workspace::TOKEN").unwrap(), None);
        assert!(store.list_meta().unwrap().is_empty());
    }

    #[test]
    fn store_uses_interior_mutability_for_index_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        let store = Arc::new(
            KeyringStore::with_backend(
                &path,
                Arc::new(FakeBackend {
                    provider: provider.clone(),
                }),
            )
            .unwrap(),
        );
        let writer = Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            writer.set("workspace::THREAD", "value").unwrap();
        });
        handle.join().unwrap();
        assert_eq!(
            store.get("workspace::THREAD").unwrap().as_deref(),
            Some("value")
        );
    }

    #[test]
    fn missing_index_is_fresh_only_without_migration_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        let fresh = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        assert!(fresh.list_meta().unwrap().is_empty());

        fs::write(dir.path().join("secrets.stronghold.bak"), b"backup").unwrap();
        assert!(KeyringStore::with_backend(&path, Arc::new(FakeBackend { provider }),).is_err());
        assert!(KeyringStore::with_backend_config(
            &path,
            Arc::new(FakeBackend {
                provider: Arc::new(FakeProvider::default()),
            }),
            true,
        )
        .is_err());
        fs::remove_file(dir.path().join("secrets.stronghold.bak")).unwrap();
        fs::write(dir.path().join("secrets.stronghold"), b"legacy").unwrap();
        assert!(KeyringStore::with_backend_config(
            &path,
            Arc::new(FakeBackend {
                provider: Arc::new(FakeProvider::default()),
            }),
            true,
        )
        .is_ok());
        fs::remove_file(dir.path().join("secrets.stronghold")).unwrap();
        fs::write(dir.path().join("secrets-unlock.json"), b"envelope").unwrap();
        assert!(KeyringStore::with_backend_config(
            &path,
            Arc::new(FakeBackend {
                provider: Arc::new(FakeProvider::default()),
            }),
            true,
        )
        .is_err());
    }

    #[test]
    fn pending_upsert_recovers_only_when_authoritative_value_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let pending = pending_path(&path);
        let provider = Arc::new(FakeProvider::default());
        provider.insert("ikenga:workspace::PRESENT", b"value");
        let mut intent = IndexPending::default();
        intent.upserts.insert("workspace::PRESENT".to_string());
        intent.upserts.insert("workspace::MISSING".to_string());
        intent.save(&pending).unwrap();

        let store = KeyringStore::with_backend(&path, Arc::new(FakeBackend { provider })).unwrap();
        assert_eq!(
            store.list_meta().unwrap(),
            vec![SecretMeta {
                name: "workspace::PRESENT".to_string()
            }]
        );
        assert!(!pending.exists());
    }

    #[test]
    fn pending_delete_recovers_after_authoritative_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let pending = pending_path(&path);
        fs::write(&path, br#"["workspace::DELETED"]"#).unwrap();
        let mut intent = IndexPending::default();
        intent.deletes.insert("workspace::DELETED".to_string());
        intent.save(&pending).unwrap();
        let provider = Arc::new(FakeProvider::default());

        let store = KeyringStore::with_backend(&path, Arc::new(FakeBackend { provider })).unwrap();
        assert!(store.list_meta().unwrap().is_empty());
        assert!(!pending.exists());
    }

    #[test]
    fn export_fails_when_indexed_value_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        let store = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        store.set("workspace::MISSING", "value").unwrap();
        provider
            .values
            .lock()
            .unwrap()
            .remove("ikenga:workspace::MISSING");

        let error = store.export_all().unwrap_err().to_string();
        assert!(error.contains("indexed secret is missing"));
    }

    #[test]
    fn replace_all_reconciles_deleted_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        let store = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        store.set("workspace::KEEP", "old").unwrap();
        store.set("workspace::REMOVE", "old").unwrap();
        let mut replacement = BTreeMap::new();
        replacement.insert("workspace::KEEP".to_string(), "new".to_string());
        replacement.insert("workspace::ADD".to_string(), "added".to_string());

        store.replace_all(&replacement).unwrap();
        assert_eq!(store.export_all().unwrap(), replacement);
        assert_eq!(provider.get_item("ikenga:workspace::REMOVE"), None);
    }

    #[test]
    fn legacy_name_with_a_space_loads_reads_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        // An index written before WP-34 tightened the charset for new names.
        fs::write(&path, br#"["My Token"]"#).unwrap();
        let provider = Arc::new(FakeProvider::default());
        provider.insert("ikenga:My Token", b"legacy-value");
        let store = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        assert_eq!(
            store.get("My Token").unwrap().as_deref(),
            Some("legacy-value")
        );
        let mut replacement = BTreeMap::new();
        replacement.insert("My Token".to_string(), "rewritten".to_string());
        store.replace_all(&replacement).unwrap();
        assert_eq!(store.export_all().unwrap(), replacement);
        assert_eq!(
            provider.get_item("ikenga:My Token"),
            Some(b"rewritten".to_vec())
        );
    }

    #[test]
    fn failed_import_rolls_back_every_item() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let provider = Arc::new(FakeProvider::default());
        provider.insert("ikenga:workspace::EXISTING", b"before");
        let store = KeyringStore::with_backend(
            &path,
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        *provider.fail_writes.lock().unwrap() = true;

        let mut values = BTreeMap::new();
        values.insert("workspace::EXISTING".to_string(), "after".to_string());
        values.insert("workspace::NEW".to_string(), "new".to_string());
        assert!(store.import_all(&values).is_err());
        assert_eq!(
            provider.get_item("ikenga:workspace::EXISTING"),
            Some(b"before".to_vec())
        );
        assert_eq!(provider.get_item("ikenga:workspace::NEW"), None);
        assert!(store.list_meta().unwrap().is_empty());
    }

    #[test]
    fn linux_backend_secret_service_absence_does_not_invalidate_read_only_cache() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"stale");
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        assert_eq!(backend.get("ikenga:workspace::TOKEN").unwrap(), None);
        assert_eq!(
            keyutils_cache.get_item("ikenga:workspace::TOKEN"),
            Some(b"stale".to_vec())
        );
    }

    #[test]
    fn linux_backend_cache_failures_do_not_affect_authoritative_absence() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"stale");
        *keyutils_cache.fail_deletes.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        assert_eq!(backend.get("ikenga:workspace::TOKEN").unwrap(), None);
        assert!(backend.diagnostic().is_none());
        assert!(!dir.path().join(KEYUTILS_DISABLED_FILENAME).exists());
        assert_eq!(
            keyutils_cache.get_item("ikenga:workspace::TOKEN"),
            Some(b"stale".to_vec())
        );
    }

    #[test]
    fn linux_backend_preexisting_disable_marker_keeps_cache_unused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(KEYUTILS_DISABLED_FILENAME), b"disabled").unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        backend
            .set("ikenga:workspace::TOKEN", b"persisted")
            .unwrap();
        assert!(backend.diagnostic().is_some());
        assert_eq!(keyutils_cache.get_item("ikenga:workspace::TOKEN"), None);
    }

    #[test]
    fn linux_backend_failed_secret_service_write_never_reports_cache_success() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"cached-old");
        *secret_service.fail_writes.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        let error = backend
            .set("ikenga:workspace::TOKEN", b"new-value")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Secret Service write failed"));
        assert_eq!(
            keyutils_cache.get_item("ikenga:workspace::TOKEN"),
            Some(b"cached-old".to_vec())
        );
    }

    #[test]
    fn linux_backend_successful_write_never_writes_keyutils() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        let backend = linux_backend(secret_service.clone(), keyutils_cache.clone(), dir.path());
        *keyutils_cache.fail_writes.lock().unwrap() = true;

        backend
            .set("ikenga:workspace::TOKEN", b"persisted-value")
            .unwrap();
        assert!(backend.diagnostic().is_none());
        assert_eq!(
            secret_service.get_item("ikenga:workspace::TOKEN"),
            Some(b"persisted-value".to_vec())
        );
        assert_eq!(keyutils_cache.get_item("ikenga:workspace::TOKEN"), None);
    }

    #[test]
    fn backup_restore_and_probe_survive_cache_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());
        let store = KeyringStore::with_backend(&path, Arc::new(backend)).unwrap();
        *keyutils_cache.fail_writes.lock().unwrap() = true;
        let mut values = BTreeMap::new();
        values.insert("workspace::TOKEN".to_string(), "persisted".to_string());

        assert_eq!(store.import_all(&values).unwrap(), 1);
        store.probe().unwrap();
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("persisted")
        );
        assert!(store.diagnostics().is_empty());
    }

    #[test]
    fn linux_backend_successful_delete_never_writes_keyutils() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        secret_service.insert("ikenga:workspace::TOKEN", b"persisted");
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"cached");
        *keyutils_cache.fail_deletes.lock().unwrap() = true;
        let backend = linux_backend(secret_service.clone(), keyutils_cache, dir.path());

        backend.delete("ikenga:workspace::TOKEN").unwrap();
        assert_eq!(secret_service.get_item("ikenga:workspace::TOKEN"), None);
        assert!(backend.diagnostic().is_none());
    }

    #[test]
    fn keyring_store_successful_secret_service_write_survives_cache_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());
        let store = KeyringStore::with_backend(&path, Arc::new(backend)).unwrap();
        *keyutils_cache.fail_writes.lock().unwrap() = true;

        store.set("workspace::TOKEN", "persisted").unwrap();
        assert_eq!(store.list_meta().unwrap().len(), 1);
        assert!(store.diagnostics().is_empty());
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("persisted")
        );
    }

    #[test]
    fn linux_backend_cache_write_failure_during_read_disables_cache() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        secret_service.insert("ikenga:workspace::TOKEN", b"persisted");
        *keyutils_cache.fail_writes.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache, dir.path());

        assert_eq!(
            backend.get("ikenga:workspace::TOKEN").unwrap(),
            Some(b"persisted".to_vec())
        );
        assert!(backend.diagnostic().is_none());
        assert!(!dir.path().join(KEYUTILS_DISABLED_FILENAME).exists());
    }

    #[test]
    fn keyring_probe_does_not_write_index_when_authoritative_backend_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets-index.json");
        fs::write(&path, b"[]").unwrap();
        let secret_service = Arc::new(FakeProvider::default());
        *secret_service.fail_writes.lock().unwrap() = true;
        let backend = linux_backend(
            secret_service,
            Arc::new(FakeProvider::default()),
            dir.path(),
        );
        let store = KeyringStore::with_backend(&path, Arc::new(backend)).unwrap();
        assert!(store.probe().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"[]");
    }

    #[test]
    fn linux_backend_authoritative_unavailable_blocks_mutations_and_probe() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"cached");
        *secret_service.fail_reads.lock().unwrap() = true;
        *secret_service.fail_writes.lock().unwrap() = true;
        *secret_service.fail_deletes.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        assert!(backend.set("ikenga:workspace::TOKEN", b"new").is_err());
        assert!(backend.delete("ikenga:workspace::TOKEN").is_err());
        let probe = backend.set("ikenga:__probe::token", b"probe");
        assert!(probe.is_err());
        assert_eq!(
            keyutils_cache.get_item("ikenga:workspace::TOKEN"),
            Some(b"cached".to_vec())
        );
    }

    #[test]
    fn linux_backend_does_not_use_cache_for_non_unavailability_error() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"cached");
        *secret_service.reject_reads.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache.clone(), dir.path());

        let error = backend
            .get("ikenga:workspace::TOKEN")
            .unwrap_err()
            .to_string();
        assert!(error.contains("read rejected"));
        assert_eq!(
            keyutils_cache.get_item("ikenga:workspace::TOKEN"),
            Some(b"cached".to_vec())
        );
    }

    #[test]
    fn linux_backend_uses_keyutils_only_after_secret_service_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        keyutils_cache.insert("ikenga:workspace::TOKEN", b"cached");
        *secret_service.fail_reads.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache, dir.path());

        assert_eq!(
            backend.get("ikenga:workspace::TOKEN").unwrap(),
            Some(b"cached".to_vec())
        );
    }

    #[test]
    fn linux_backend_fails_closed_when_secret_service_and_cache_are_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let keyutils_cache = Arc::new(FakeProvider::default());
        let secret_service = Arc::new(FakeProvider::default());
        *secret_service.fail_reads.lock().unwrap() = true;
        *keyutils_cache.fail_reads.lock().unwrap() = true;
        let backend = linux_backend(secret_service, keyutils_cache, dir.path());

        let error = backend
            .get("ikenga:workspace::TOKEN")
            .unwrap_err()
            .to_string();
        assert!(error.contains("Secret Service read failed"));
        assert!(error.contains("keyutils cache read failed"));
    }
}
