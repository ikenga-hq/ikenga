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

use super::index::{
    item_name_for_service, pending_path, validate_legacy_name, write_atomic, IndexPending,
    SecretIndex,
};
use super::store::{ReplaceOutcome, SecretMeta, SecretsStore, StoreError};

/// Live list of names that failed migration and still need re-entry. Names
/// only, JSON array, next to the index. Owned by [`KeyringStore`]; the
/// migration marker keeps the immutable record of what was quarantined at
/// migration time.
pub(crate) const QUARANTINE_FILENAME: &str = "secrets-quarantine.json";

/// Why one entry failed its authoritative read-back. Logged by class only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureClass {
    BackendWriteError,
    BackendReadError,
    ReadBackNone,
    LengthMismatch,
    ByteMismatch,
}

impl FailureClass {
    fn label(self) -> &'static str {
        match self {
            Self::BackendWriteError => "backend write error",
            Self::BackendReadError => "backend read error",
            Self::ReadBackNone => "read-back returned nothing",
            Self::LengthMismatch => "read-back length mismatch",
            Self::ByteMismatch => "read-back byte mismatch",
        }
    }
}

/// Class plus lengths for a read-back that returned bytes. Pure, so tests
/// can pin it without touching a keychain.
fn classify_mismatch(expected: &[u8], actual: &[u8]) -> Option<(FailureClass, Option<usize>)> {
    if expected == actual {
        return None;
    }
    if expected.len() != actual.len() {
        return Some((FailureClass::LengthMismatch, first_diff(expected, actual)));
    }
    Some((FailureClass::ByteMismatch, first_diff(expected, actual)))
}

fn first_diff(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .or_else(|| {
            if a.len() != b.len() {
                Some(a.len().min(b.len()))
            } else {
                None
            }
        })
}

/// Production keychain service. Also the item prefix (`index::ITEM_PREFIX`).
pub(crate) const SERVICE: &str = "ikenga";
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

/// Keeps the failure class of a Secret Service write/delete error. A locked
/// or unreachable keychain (`NoStorageAccess`, `PlatformFailure`) is
/// `unavailable` and must fail closed; only an item-specific rejection stays
/// `rejected`, which is the one class migration may quarantine.
fn reclassify_write_error(operation: &str, error: BackendError) -> BackendError {
    if error.allows_cache_fallback() {
        BackendError::unavailable(format!("Secret Service {operation} failed: {error}"))
    } else {
        BackendError::rejected(format!("Secret Service {operation} failed: {error}"))
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
fn native_entry(service: &str, item: &str) -> BackendResult<keyring::Entry> {
    #[cfg(target_os = "windows")]
    {
        return keyring::Entry::new_with_target(item, service, USER).map_err(keyring_error);
    }
    #[cfg(target_os = "macos")]
    {
        return keyring::Entry::new(service, item).map_err(keyring_error);
    }
    #[allow(unreachable_code)]
    Err(BackendError::rejected("unsupported platform keychain"))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeProvider {
    service: String,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl CredentialProvider for NativeProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        get_entry(&native_entry(&self.service, item)?)
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        set_entry(&native_entry(&self.service, item)?, value)
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        delete_entry(&native_entry(&self.service, item)?)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NativeSecretBackend {
    provider: NativeProvider,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl SecretBackend for NativeSecretBackend {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        self.provider.get(item)
    }

    fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        self.provider.get(item)
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        self.provider.set(item, value)
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        self.provider.delete(item)
    }

    fn label(&self) -> &'static str {
        native_backend_label()
    }
}

#[cfg(target_os = "linux")]
struct KeyutilsProvider {
    service: String,
}

#[cfg(target_os = "linux")]
impl CredentialReader for KeyutilsProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        let credential =
            keyring::keyutils::KeyutilsCredential::new_with_target(Some(item), &self.service, USER)
                .map_err(keyring_error)?;
        get_entry(&keyring::Entry::new_with_credential(Box::new(credential)))
    }
}

#[cfg(target_os = "linux")]
struct SecretServiceProvider {
    service: String,
}

#[cfg(target_os = "linux")]
impl CredentialProvider for SecretServiceProvider {
    fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
        let credential = keyring::secret_service::SsCredential::new_with_target(
            Some("default"),
            &self.service,
            item,
        )
        .map_err(keyring_error)?;
        get_entry(&keyring::Entry::new_with_credential(Box::new(credential)))
    }

    fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
        let credential = keyring::secret_service::SsCredential::new_with_target(
            Some("default"),
            &self.service,
            item,
        )
        .map_err(keyring_error)?;
        set_entry(
            &keyring::Entry::new_with_credential(Box::new(credential)),
            value,
        )
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        let credential = keyring::secret_service::SsCredential::new_with_target(
            Some("default"),
            &self.service,
            item,
        )
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
        self.secret_service
            .set(item, value)
            .map_err(|error| reclassify_write_error("write", error))
    }

    fn delete(&self, item: &str) -> BackendResult<()> {
        self.secret_service
            .delete(item)
            .map_err(|error| reclassify_write_error("delete", error))
    }

    fn label(&self) -> &'static str {
        "Secret Service with read-only Linux keyutils fallback"
    }

    fn diagnostic(&self) -> Option<String> {
        self.diagnostic.lock().ok().and_then(|value| value.clone())
    }
}

#[cfg(target_os = "linux")]
fn platform_backend(index_path: &Path, service: &str) -> Arc<dyn SecretBackend> {
    let cache_disabled_path = index_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(KEYUTILS_DISABLED_FILENAME);
    Arc::new(LinuxSecretBackend::new(
        Arc::new(SecretServiceProvider {
            service: service.to_string(),
        }),
        Arc::new(KeyutilsProvider {
            service: service.to_string(),
        }),
        Some(cache_disabled_path),
    ))
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn platform_backend(_index_path: &Path, service: &str) -> Arc<dyn SecretBackend> {
    Arc::new(NativeSecretBackend {
        provider: NativeProvider {
            service: service.to_string(),
        },
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_backend(_index_path: &Path, _service: &str) -> Arc<dyn SecretBackend> {
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
    /// Keychain service and item prefix: [`SERVICE`] in production. The
    /// `*_with_service` constructors exist so a rehearsal against copied app
    /// data can never address the production namespace.
    service: String,
    quarantine: Mutex<BTreeSet<String>>,
    quarantine_path: PathBuf,
}

impl KeyringStore {
    pub fn new(index_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::new_with_service(index_path, SERVICE)
    }

    /// [`KeyringStore::new`] under an explicit keychain service.
    pub(crate) fn new_with_service(
        index_path: impl AsRef<Path>,
        service: &str,
    ) -> Result<Self, StoreError> {
        let path = index_path.as_ref().to_path_buf();
        let service = validate_service(service)?;
        Self::with_backend_config_in(&path, platform_backend(&path, service), false, service)
    }

    /// Store for the one-time Stronghold migration: a missing index is
    /// tolerated while the legacy snapshot is the only migration artifact.
    pub(crate) fn new_for_migration_with_service(
        index_path: impl AsRef<Path>,
        service: &str,
    ) -> Result<Self, StoreError> {
        let path = index_path.as_ref().to_path_buf();
        let service = validate_service(service)?;
        Self::with_backend_config_in(&path, platform_backend(&path, service), true, service)
    }

    fn item(&self, name: &str) -> Result<String, String> {
        item_name_for_service(&self.service, name)
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

    #[cfg(test)]
    fn with_backend_config(
        index_path: impl AsRef<Path>,
        backend: Arc<dyn SecretBackend>,
        allow_missing_index: bool,
    ) -> Result<Self, StoreError> {
        Self::with_backend_config_in(index_path, backend, allow_missing_index, SERVICE)
    }

    fn with_backend_config_in(
        index_path: impl AsRef<Path>,
        backend: Arc<dyn SecretBackend>,
        allow_missing_index: bool,
        service: &str,
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
        let quarantine_path = path.with_file_name(QUARANTINE_FILENAME);
        let mut quarantine = load_quarantine(&quarantine_path, &path)?;
        recover_pending(&mut index, backend.as_ref(), &pending, service, &quarantine)?;
        // A name in the index is never quarantined, whatever the file says.
        let indexed: BTreeSet<String> = index.names().into_iter().collect();
        quarantine.retain(|name| !indexed.contains(name));
        Ok(Self {
            backend,
            index: Mutex::new(index),
            pending_path: pending,
            service: service.to_string(),
            quarantine: Mutex::new(quarantine),
            quarantine_path,
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
            let item = self.item(&name).map_err(StoreError::uncommitted)?;
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
            let item = self.item(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.delete(&item) {
                failure = Some(format!("delete rollback item `{name}`: {error}"));
                break;
            }
        }
        if failure.is_none() {
            for (name, value) in previous {
                let item = self.item(name).map_err(StoreError::uncommitted)?;
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

    /// Per-entry authoritative read-back. Returns the entries that failed,
    /// with their class (logged by name, class and lengths only). A backend
    /// that is unavailable (not merely rejecting one item) is a hard error:
    /// it says nothing about a single entry.
    fn verify_each(
        &self,
        values: &BTreeMap<String, String>,
        skip: &BTreeSet<String>,
    ) -> Result<Vec<(String, FailureClass)>, StoreError> {
        let mut failed = Vec::new();
        for (name, expected) in values {
            if skip.contains(name) {
                continue;
            }
            let item = self.item(name).map_err(StoreError::uncommitted)?;
            match self.backend.get_authoritative(&item) {
                Ok(Some(actual)) => {
                    if let Some((class, at)) = classify_mismatch(expected.as_bytes(), &actual) {
                        log::warn!(
                            "[secrets] verification failed for `{name}`: {} (expected_len={}, actual_len={}, first_diff_index={})",
                            class.label(),
                            expected.len(),
                            actual.len(),
                            at.map_or_else(|| "none".to_string(), |i| i.to_string()),
                        );
                        failed.push((name.clone(), class));
                    }
                }
                Ok(None) => {
                    log::warn!(
                        "[secrets] verification failed for `{name}`: {} (expected_len={})",
                        FailureClass::ReadBackNone.label(),
                        expected.len()
                    );
                    failed.push((name.clone(), FailureClass::ReadBackNone));
                }
                Err(error) if error.allows_cache_fallback() => {
                    return Err(StoreError::committed(format!(
                        "authoritative keychain verification failed for `{name}`: {error}"
                    )))
                }
                Err(error) => {
                    log::warn!(
                        "[secrets] verification failed for `{name}`: {} ({error})",
                        FailureClass::BackendReadError.label()
                    );
                    failed.push((name.clone(), FailureClass::BackendReadError));
                }
            }
        }
        Ok(failed)
    }

    fn verify_values(&self, values: &BTreeMap<String, String>) -> Result<(), StoreError> {
        match self.verify_each(values, &BTreeSet::new())?.first() {
            None => Ok(()),
            Some((name, _)) => Err(StoreError::committed(format!(
                "authoritative keychain verification failed for `{name}`"
            ))),
        }
    }

    fn lock_quarantine(&self) -> Result<std::sync::MutexGuard<'_, BTreeSet<String>>, StoreError> {
        self.quarantine.lock().map_err(|error| {
            StoreError::uncommitted(format!("secrets quarantine lock poisoned: {error}"))
        })
    }

    fn save_quarantine(&self, names: &BTreeSet<String>) -> Result<(), StoreError> {
        save_quarantine(&self.quarantine_path, names).map_err(StoreError::committed)
    }
}

/// Upper bound on the live quarantine list. A list larger than this is not
/// a migration result; it fails closed rather than being loaded.
pub(crate) const MAX_QUARANTINED_NAMES: usize = 1024;

/// Loads the live quarantine list. A garbage or oversized file fails closed.
/// When a migration marker exists the list is intersected with the marker's
/// quarantined names (a quarantine can only come from a migration, so any
/// other name is not legitimate); the caller then drops indexed names.
fn load_quarantine(path: &Path, index_path: &Path) -> Result<BTreeSet<String>, StoreError> {
    let names: BTreeSet<String> = match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Vec<String>>(&bytes)
            .map(|names| names.into_iter().collect())
            .map_err(|error| {
                StoreError::uncommitted(format!("secrets quarantine list is unreadable: {error}"))
            })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => {
            return Err(StoreError::uncommitted(format!(
                "secrets quarantine list is unreadable: {error}"
            )))
        }
    };
    if names.len() > MAX_QUARANTINED_NAMES {
        return Err(StoreError::uncommitted(format!(
            "secrets quarantine list is implausibly large ({} names)",
            names.len()
        )));
    }
    let dir = index_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(match super::migrate::marker_quarantined_names(dir) {
        Some(allowed) => names.intersection(&allowed).cloned().collect(),
        None => names,
    })
}

fn save_quarantine(path: &Path, names: &BTreeSet<String>) -> Result<(), String> {
    if names.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove secrets quarantine list: {error}")),
        };
    }
    let mut body = serde_json::to_vec_pretty(&names.iter().collect::<Vec<_>>())
        .map_err(|error| format!("serialize secrets quarantine list: {error}"))?;
    body.push(b'\n');
    write_atomic(path, &body)
}

fn validate_service(service: &str) -> Result<&str, StoreError> {
    if service.is_empty() || service.contains(':') || service.contains('\0') {
        return Err(StoreError::invalid("keychain service name is invalid"));
    }
    Ok(service)
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
    service: &str,
    quarantined: &BTreeSet<String>,
) -> Result<(), StoreError> {
    let pending = IndexPending::load(path).map_err(StoreError::uncommitted)?;
    if pending.upserts.is_empty() && pending.deletes.is_empty() {
        return Ok(());
    }
    let mut next = index.clone();
    let mut changed = false;
    for name in &pending.deletes {
        let item = item_name_for_service(service, name).map_err(StoreError::uncommitted)?;
        let authoritative = backend
            .get_authoritative(&item)
            .map_err(backend_store_error)?;
        if authoritative.is_none() {
            next.remove(name).map_err(StoreError::uncommitted)?;
            changed = true;
        }
    }
    for name in &pending.upserts {
        // The pending record carries no value, so a recovered item cannot be
        // checked against what was meant to be written. A quarantined name
        // therefore stays out of the index (and in quarantine) until it is
        // entered again and verified by `set`.
        if quarantined.contains(name) {
            continue;
        }
        let item = item_name_for_service(service, name).map_err(StoreError::uncommitted)?;
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
        if self.lock_quarantine()?.contains(name) {
            return Err(StoreError::quarantined(name));
        }
        let item = self.item(name).map_err(StoreError::uncommitted)?;
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
        let item = self.item(name).map_err(StoreError::uncommitted)?;
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
        // A quarantined name only leaves quarantine through a write whose
        // authoritative read-back matches what was written.
        let was_quarantined = self.lock_quarantine()?.contains(name);
        if was_quarantined {
            let verified = matches!(
                self.backend.get_authoritative(&item),
                Ok(Some(actual)) if actual == value.as_bytes()
            );
            if !verified {
                log::warn!(
                    "[secrets] re-entry of quarantined `{name}` did not verify; it stays quarantined"
                );
                let affected = BTreeSet::from([name.to_string()]);
                let mut snapshot = BTreeMap::new();
                snapshot.insert(name.to_string(), previous_value_bytes);
                let rollback = self.restore_snapshot(&snapshot, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(StoreError::uncommitted(
                        "keychain write could not be verified; the secret is still quarantined",
                    )),
                    Err(rollback) => Err(StoreError::committed(format!(
                        "keychain write could not be verified; {rollback}"
                    ))),
                };
            }
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
        if was_quarantined {
            let mut quarantine = self.lock_quarantine()?;
            let mut remaining = quarantine.clone();
            remaining.remove(name);
            self.save_quarantine(&remaining)?;
            *quarantine = remaining;
        }
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        let mut index = self.lock_index()?;
        let item = self.item(name).map_err(StoreError::uncommitted)?;
        if self.lock_quarantine()?.contains(name) {
            // Not in the index: drop any leftover keychain item (best
            // effort, it is unreferenced) and leave quarantine.
            if let Err(error) = self.backend.delete(&item) {
                log::warn!(
                    "[secrets] could not remove leftover item for quarantined `{name}`: {error}"
                );
            }
            // Defensive: a name that is somehow also indexed leaves both.
            if index.names().iter().any(|indexed| indexed == name) {
                let mut next = index.clone();
                next.remove(name).map_err(StoreError::committed)?;
                next.save().map_err(StoreError::committed)?;
                *index = next;
            }
            let mut quarantine = self.lock_quarantine()?;
            let mut remaining = quarantine.clone();
            remaining.remove(name);
            self.save_quarantine(&remaining)?;
            *quarantine = remaining;
            return Ok(());
        }
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
        let quarantined = self.lock_quarantine()?.clone();
        let mut out = BTreeMap::new();
        for name in names {
            if quarantined.contains(&name) {
                // Needs re-entry: not exportable, but not a reason to fail
                // the whole store.
                log::warn!("[secrets] export skipped `{name}`: needs re-entry");
                continue;
            }
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
            let item = self.item(name).map_err(StoreError::uncommitted)?;
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
            let item = self.item(name).map_err(StoreError::uncommitted)?;
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
        // Every imported value was just verified against the keychain, so a
        // name that was quarantined is no longer: it must not stay both
        // indexed and quarantined.
        {
            let mut quarantine = self.lock_quarantine()?;
            if values.keys().any(|name| quarantine.contains(name)) {
                let mut remaining = quarantine.clone();
                for name in values.keys() {
                    remaining.remove(name);
                }
                self.save_quarantine(&remaining)?;
                *quarantine = remaining;
            }
        }
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)?;
        Ok(values.len())
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        self.replace_all_inner(values, false)
            .map(|outcome| outcome.count)
    }

    fn replace_all_quarantining(
        &self,
        values: &BTreeMap<String, String>,
    ) -> Result<ReplaceOutcome, StoreError> {
        self.replace_all_inner(values, true)
    }

    fn quarantined_names(&self) -> Vec<String> {
        self.quarantine
            .lock()
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn probe(&self) -> Result<(), StoreError> {
        let token = uuid::Uuid::new_v4();
        let item = self
            .item(&format!("__probe::{}", token.simple()))
            .map_err(StoreError::uncommitted)?;
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

    fn detect_configuration(&self) -> Result<bool, StoreError> {
        // The encrypted value format is `EncryptedStore`'s; it scans for it.
        Ok(false)
    }

    fn backend_label(&self) -> &'static str {
        self.backend.label()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.backend.diagnostic().into_iter().collect()
    }
}

impl KeyringStore {
    fn replace_all_inner(
        &self,
        values: &BTreeMap<String, String>,
        tolerate_entry_failures: bool,
    ) -> Result<ReplaceOutcome, StoreError> {
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
            let item = self.item(name).map_err(StoreError::uncommitted)?;
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
        // Entries that fail on their own (rejected write, or a read-back that
        // is missing or different) are quarantined when the caller allows it;
        // anything else, or too many of them, rolls the whole thing back.
        let mut failed: BTreeMap<String, FailureClass> = BTreeMap::new();
        for (name, value) in values {
            let item = self.item(name).map_err(StoreError::uncommitted)?;
            if let Err(error) = self.backend.set(&item, value.as_bytes()) {
                if tolerate_entry_failures && !error.allows_cache_fallback() {
                    log::warn!(
                        "[secrets] write failed for `{name}`: {} ({error})",
                        FailureClass::BackendWriteError.label()
                    );
                    failed.insert(name.clone(), FailureClass::BackendWriteError);
                    continue;
                }
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
        let skip: BTreeSet<String> = failed.keys().cloned().collect();
        let verification = self.verify_each(values, &skip);
        let verified_failures = match verification {
            Ok(list) => list,
            Err(error) => {
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
                };
            }
        };
        for (name, class) in verified_failures {
            failed.insert(name, class);
        }
        if !failed.is_empty() {
            // Fail closed unless this is the tolerant path AND the failures
            // are a minority of the entries.
            let too_many = failed.len().saturating_mul(2) > values.len();
            if !tolerate_entry_failures || too_many {
                let name = failed.keys().next().cloned().unwrap_or_default();
                let error = StoreError::committed(format!(
                    "authoritative keychain verification failed for `{name}`"
                ));
                let rollback = self.restore_snapshot(&previous, &affected);
                let _ = IndexPending::clear(&self.pending_path);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(StoreError::committed(format!("{error}; {rollback}"))),
                };
            }
            // Remove only the failed entries' items. They are not indexed, so
            // a leftover cannot be read through the store; warn and go on.
            for name in failed.keys() {
                let item = self.item(name).map_err(StoreError::uncommitted)?;
                if let Err(error) = self.backend.delete(&item) {
                    log::warn!("[secrets] could not remove failed item for `{name}`: {error}");
                }
            }
        }
        for name in &deletes {
            let item = self.item(name).map_err(StoreError::uncommitted)?;
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
            if !target.contains(&name) || failed.contains_key(&name) {
                next.remove(&name).map_err(StoreError::committed)?;
            }
        }
        for name in &target {
            if !failed.contains_key(name) {
                next.insert(name).map_err(StoreError::committed)?;
            }
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
        // The live quarantine is exactly this run's failures (a full
        // replacement also resets any earlier list).
        let quarantined: BTreeSet<String> = failed.keys().cloned().collect();
        {
            let mut live = self.lock_quarantine()?;
            if *live != quarantined {
                self.save_quarantine(&quarantined)?;
                *live = quarantined.clone();
            }
        }
        IndexPending::clear(&self.pending_path).map_err(StoreError::committed)?;
        Ok(ReplaceOutcome {
            count: values.len() - quarantined.len(),
            quarantined: quarantined.into_iter().collect(),
        })
    }
}

/// Test-only fault-injecting backend, shared with `migrate`'s tests: an
/// in-memory keychain where chosen entries read back as missing, wrong or
/// rejected, or where the whole backend is unreachable.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    #[derive(Default)]
    pub(crate) struct Faults {
        /// Writes succeed but the read-back finds nothing.
        pub drop_names: BTreeSet<String>,
        /// Read-back returns the right length with one byte changed.
        pub corrupt_names: BTreeSet<String>,
        /// Read-back returns one byte too many.
        pub grow_names: BTreeSet<String>,
        /// The read-back call for this item is rejected (confined to it).
        pub reject_read_names: BTreeSet<String>,
        /// Every read and write says the keychain is unreachable.
        pub unreachable: bool,
        /// Item deletes fail (rollback cannot complete).
        pub fail_deletes: bool,
    }

    #[derive(Default)]
    pub(crate) struct FaultBackend {
        pub values: Mutex<BTreeMap<String, Vec<u8>>>,
        pub faults: Mutex<Faults>,
    }

    impl FaultBackend {
        fn hit(set: &BTreeSet<String>, item: &str) -> bool {
            set.iter().any(|name| item.ends_with(name.as_str()))
        }

        pub(crate) fn item_count(&self) -> usize {
            self.values.lock().unwrap().len()
        }
    }

    impl SecretBackend for Arc<FaultBackend> {
        fn get(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            self.get_authoritative(item)
        }

        fn get_authoritative(&self, item: &str) -> BackendResult<Option<Vec<u8>>> {
            let faults = self.faults.lock().unwrap();
            if faults.unreachable {
                return Err(BackendError::unavailable("keychain unreachable"));
            }
            if FaultBackend::hit(&faults.reject_read_names, item) {
                return Err(BackendError::rejected("read rejected"));
            }
            let stored = self.values.lock().unwrap().get(item).cloned();
            let Some(mut bytes) = stored else {
                return Ok(None);
            };
            if FaultBackend::hit(&faults.drop_names, item) {
                return Ok(None);
            }
            if FaultBackend::hit(&faults.corrupt_names, item) && !bytes.is_empty() {
                let last = bytes.len() - 1;
                bytes[last] ^= 0x01;
            }
            if FaultBackend::hit(&faults.grow_names, item) {
                bytes.push(b'x');
            }
            Ok(Some(bytes))
        }

        fn set(&self, item: &str, value: &[u8]) -> BackendResult<()> {
            if self.faults.lock().unwrap().unreachable {
                return Err(BackendError::unavailable("keychain unreachable"));
            }
            self.values
                .lock()
                .unwrap()
                .insert(item.to_string(), value.to_vec());
            Ok(())
        }

        fn delete(&self, item: &str) -> BackendResult<()> {
            if self.faults.lock().unwrap().fail_deletes {
                return Err(BackendError::rejected("delete rejected"));
            }
            self.values.lock().unwrap().remove(item);
            Ok(())
        }

        fn label(&self) -> &'static str {
            "fault-memory"
        }
    }

    /// A store in migration mode (missing index tolerated) over `backend`.
    pub(crate) fn migration_store(index_path: &Path, backend: Arc<FaultBackend>) -> KeyringStore {
        KeyringStore::with_backend_config_in(index_path, Arc::new(backend), true, SERVICE).unwrap()
    }

    /// A normal (post-migration) store over the same backend, as a later
    /// start of the app builds it.
    pub(crate) fn reopened_store(index_path: &Path, backend: Arc<FaultBackend>) -> KeyringStore {
        KeyringStore::with_backend_config_in(index_path, Arc::new(backend), false, SERVICE).unwrap()
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
        written: Mutex<Vec<String>>,
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
            self.written.lock().unwrap().push(item.to_string());
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
    fn service_prefixes_every_item_and_default_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let provider = Arc::new(FakeProvider::default());
        let rehearsal = KeyringStore::with_backend_config_in(
            dir.path().join("rehearsal").join("secrets-index.json"),
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
            false,
            "ikenga-rehearsal-5a",
        )
        .unwrap();
        rehearsal.set("workspace::TOKEN", "value").unwrap();
        rehearsal.probe().unwrap();
        let written = provider.written.lock().unwrap().clone();
        assert_eq!(written.len(), 2);
        assert!(written
            .iter()
            .all(|item| item.starts_with("ikenga-rehearsal-5a:")));
        assert!(written[1].starts_with("ikenga-rehearsal-5a:__probe::"));

        let default = KeyringStore::with_backend(
            dir.path().join("secrets-index.json"),
            Arc::new(FakeBackend {
                provider: provider.clone(),
            }),
        )
        .unwrap();
        default.set("workspace::TOKEN", "other").unwrap();
        default.probe().unwrap();
        let written = provider.written.lock().unwrap().clone();
        assert_eq!(written[2], "ikenga:workspace::TOKEN");
        assert!(written[3].starts_with("ikenga:__probe::"));
        assert_eq!(
            rehearsal.get("workspace::TOKEN").unwrap().as_deref(),
            Some("value")
        );

        for invalid in ["", "a:b", "nul\0"] {
            assert!(KeyringStore::new_with_service(dir.path().join("x.json"), invalid).is_err());
        }
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

    // ── per-entry quarantine ────────────────────────────────────────────

    fn sample_values() -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        for (name, value) in [
            ("workspace::A", "alpha"),
            ("workspace::B", "bravo"),
            ("workspace::C", "charlie"),
            ("workspace::D", "delta"),
            ("workspace::E", "echo"),
        ] {
            values.insert(name.to_string(), value.to_string());
        }
        values
    }

    fn fault_store(dir: &Path) -> (KeyringStore, Arc<testing::FaultBackend>, PathBuf) {
        let backend = Arc::new(testing::FaultBackend::default());
        let index = dir.join("secrets-index.json");
        let store = testing::migration_store(&index, backend.clone());
        (store, backend, index)
    }

    #[test]
    fn classify_mismatch_reports_class_and_first_diff_only() {
        assert_eq!(classify_mismatch(b"abc", b"abc"), None);
        assert_eq!(
            classify_mismatch(b"abc", b"abd"),
            Some((FailureClass::ByteMismatch, Some(2)))
        );
        assert_eq!(
            classify_mismatch(b"abc", b"abcd"),
            Some((FailureClass::LengthMismatch, Some(3)))
        );
        assert_eq!(
            classify_mismatch(b"abc", b"xb"),
            Some((FailureClass::LengthMismatch, Some(0)))
        );
    }

    #[test]
    fn one_failing_entry_is_quarantined_and_the_rest_migrate() {
        for fault in ["drop", "corrupt", "grow", "reject"] {
            let dir = tempfile::tempdir().unwrap();
            let (store, backend, _) = fault_store(dir.path());
            {
                let mut faults = backend.faults.lock().unwrap();
                let set = match fault {
                    "drop" => &mut faults.drop_names,
                    "corrupt" => &mut faults.corrupt_names,
                    "grow" => &mut faults.grow_names,
                    _ => &mut faults.reject_read_names,
                };
                set.insert("workspace::C".to_string());
            }
            let values = sample_values();
            let outcome = store.replace_all_quarantining(&values).unwrap();
            assert_eq!(outcome.count, 4, "{fault}");
            assert_eq!(
                outcome.quarantined,
                vec!["workspace::C".to_string()],
                "{fault}"
            );
            assert_eq!(store.quarantined_names(), outcome.quarantined);
            let listed: Vec<String> = store
                .list_meta()
                .unwrap()
                .into_iter()
                .map(|meta| meta.name)
                .collect();
            assert!(!listed.contains(&"workspace::C".to_string()));
            assert_eq!(listed.len(), 4);
            // The failed item is gone from the keychain: only the four good ones remain.
            assert_eq!(backend.item_count(), 4, "{fault}");
            let err = store.get("workspace::C").unwrap_err();
            assert!(err.is_quarantined(), "{fault}");
            assert_eq!(err.code(), "quarantined");
            let mut expected = values.clone();
            expected.remove("workspace::C");
            assert_eq!(store.export_all().unwrap(), expected, "{fault}");
        }
    }

    #[test]
    fn strict_replace_all_still_fails_on_one_bad_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        backend
            .faults
            .lock()
            .unwrap()
            .drop_names
            .insert("workspace::C".to_string());
        let error = store.replace_all(&sample_values()).unwrap_err();
        assert!(error.to_string().contains("verification failed"));
        assert_eq!(backend.item_count(), 0, "everything rolled back");
        assert!(store.quarantined_names().is_empty());
        let _ = index;
    }

    #[test]
    fn majority_or_all_failing_fails_closed() {
        for failing in [
            &["workspace::A", "workspace::B", "workspace::C"][..],
            &[
                "workspace::A",
                "workspace::B",
                "workspace::C",
                "workspace::D",
                "workspace::E",
            ][..],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (store, backend, _) = fault_store(dir.path());
            for name in failing {
                backend
                    .faults
                    .lock()
                    .unwrap()
                    .drop_names
                    .insert(name.to_string());
            }
            let error = store
                .replace_all_quarantining(&sample_values())
                .unwrap_err();
            assert!(error.to_string().contains("verification failed"));
            assert_eq!(backend.item_count(), 0);
            assert!(store.list_meta().unwrap().is_empty());
            assert!(store.quarantined_names().is_empty());
        }
    }

    #[test]
    fn exactly_half_failing_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, _) = fault_store(dir.path());
        {
            let mut faults = backend.faults.lock().unwrap();
            faults.drop_names.insert("workspace::A".to_string());
            faults.drop_names.insert("workspace::B".to_string());
        }
        let mut values = sample_values();
        values.remove("workspace::E");
        let outcome = store.replace_all_quarantining(&values).unwrap();
        assert_eq!(outcome.quarantined.len(), 2);
        assert_eq!(outcome.count, 2);
    }

    #[test]
    fn unreachable_backend_is_never_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, _) = fault_store(dir.path());
        backend.faults.lock().unwrap().unreachable = true;
        assert!(store.replace_all_quarantining(&sample_values()).is_err());
        assert!(store.quarantined_names().is_empty());
    }

    #[test]
    fn failed_rollback_during_guard_trip_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, _) = fault_store(dir.path());
        {
            let mut faults = backend.faults.lock().unwrap();
            for name in ["workspace::A", "workspace::B", "workspace::C"] {
                faults.drop_names.insert(name.to_string());
            }
            faults.fail_deletes = true;
        }
        let error = store
            .replace_all_quarantining(&sample_values())
            .unwrap_err();
        assert!(error.is_committed());
        assert!(error.to_string().contains("rollback failed"));
    }

    #[test]
    fn quarantine_survives_restart_and_set_with_verified_readback_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        backend
            .faults
            .lock()
            .unwrap()
            .drop_names
            .insert("workspace::C".to_string());
        store.replace_all_quarantining(&sample_values()).unwrap();
        drop(store);

        let reopened = testing::reopened_store(&index, backend.clone());
        assert_eq!(
            reopened.quarantined_names(),
            vec!["workspace::C".to_string()]
        );
        assert!(reopened.get("workspace::C").unwrap_err().is_quarantined());

        // Still faulty: the re-entry cannot be verified, so it stays quarantined
        // and nothing is left behind.
        assert!(reopened.set("workspace::C", "fresh").is_err());
        assert_eq!(
            reopened.quarantined_names(),
            vec!["workspace::C".to_string()]
        );
        assert!(!reopened
            .list_meta()
            .unwrap()
            .iter()
            .any(|meta| meta.name == "workspace::C"));

        backend.faults.lock().unwrap().drop_names.clear();
        reopened.set("workspace::C", "fresh").unwrap();
        assert!(reopened.quarantined_names().is_empty());
        assert_eq!(
            reopened.get("workspace::C").unwrap().as_deref(),
            Some("fresh")
        );
        assert!(!index.with_file_name(QUARANTINE_FILENAME).exists());
        let again = testing::reopened_store(&index, backend);
        assert!(again.quarantined_names().is_empty());
        assert_eq!(again.list_meta().unwrap().len(), 5);
    }

    #[test]
    fn delete_clears_a_quarantined_name() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        backend
            .faults
            .lock()
            .unwrap()
            .drop_names
            .insert("workspace::C".to_string());
        store.replace_all_quarantining(&sample_values()).unwrap();
        store.delete("workspace::C").unwrap();
        assert!(store.quarantined_names().is_empty());
        assert_eq!(store.get("workspace::C").unwrap(), None);
        assert!(!index.with_file_name(QUARANTINE_FILENAME).exists());
        assert_eq!(store.list_meta().unwrap().len(), 4);
    }

    #[test]
    fn plain_replace_all_resets_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, _) = fault_store(dir.path());
        backend
            .faults
            .lock()
            .unwrap()
            .drop_names
            .insert("workspace::C".to_string());
        store.replace_all_quarantining(&sample_values()).unwrap();
        store.replace_all(&BTreeMap::new()).unwrap();
        assert!(store.quarantined_names().is_empty());
    }

    // ── verifier round 1: quarantine/index overlap, validation, classes ──

    fn quarantine_c(dir: &Path) -> (KeyringStore, Arc<testing::FaultBackend>, PathBuf) {
        let (store, backend, index) = fault_store(dir);
        backend
            .faults
            .lock()
            .unwrap()
            .drop_names
            .insert("workspace::C".to_string());
        store.replace_all_quarantining(&sample_values()).unwrap();
        (store, backend, index)
    }

    #[test]
    fn import_all_of_a_quarantined_name_clears_quarantine_and_export_works() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = quarantine_c(dir.path());
        backend.faults.lock().unwrap().drop_names.clear();
        let mut values = BTreeMap::new();
        values.insert("workspace::C".to_string(), "restored".to_string());
        assert_eq!(store.import_all(&values).unwrap(), 1);
        assert!(store.quarantined_names().is_empty());
        assert!(!index.with_file_name(QUARANTINE_FILENAME).exists());
        assert_eq!(
            store.get("workspace::C").unwrap().as_deref(),
            Some("restored")
        );
        let exported = store.export_all().unwrap();
        assert_eq!(exported.len(), 5);
        assert_eq!(exported["workspace::C"], "restored");
    }

    #[test]
    fn import_all_that_fails_verification_keeps_the_name_quarantined_and_unindexed() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _backend, _) = quarantine_c(dir.path());
        // The fault is still active, so the read-back of C is missing.
        let mut values = BTreeMap::new();
        values.insert("workspace::C".to_string(), "restored".to_string());
        assert!(store.import_all(&values).is_err());
        assert_eq!(store.quarantined_names(), vec!["workspace::C".to_string()]);
        assert!(!store
            .list_meta()
            .unwrap()
            .iter()
            .any(|meta| meta.name == "workspace::C"));
        assert_eq!(store.export_all().unwrap().len(), 4);
    }

    #[test]
    fn recovering_a_pending_upsert_never_indexes_a_quarantined_name() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = quarantine_c(dir.path());
        drop(store);
        backend.faults.lock().unwrap().drop_names.clear();
        // Crash shape: the item was written and a pending upsert recorded,
        // but the verified index/quarantine update never ran.
        let item = item_name_for_service(SERVICE, "workspace::C").unwrap();
        backend
            .values
            .lock()
            .unwrap()
            .insert(item, b"unverified".to_vec());
        IndexPending {
            upserts: BTreeSet::from(["workspace::C".to_string(), "workspace::A".to_string()]),
            deletes: BTreeSet::new(),
        }
        .save(&pending_path(&index))
        .unwrap();
        let reopened = testing::reopened_store(&index, backend.clone());
        assert_eq!(
            reopened.quarantined_names(),
            vec!["workspace::C".to_string()]
        );
        assert!(!reopened
            .list_meta()
            .unwrap()
            .iter()
            .any(|meta| meta.name == "workspace::C"));
        assert!(reopened.get("workspace::C").unwrap_err().is_quarantined());
        assert_eq!(reopened.export_all().unwrap().len(), 4);
        assert!(!pending_path(&index).exists());
    }

    #[test]
    fn recovering_a_pending_upsert_still_indexes_a_present_unquarantined_name() {
        let dir = tempfile::tempdir().unwrap();
        let backend = Arc::new(testing::FaultBackend::default());
        let index = dir.path().join("secrets-index.json");
        let item = item_name_for_service(SERVICE, "workspace::N").unwrap();
        backend
            .values
            .lock()
            .unwrap()
            .insert(item, b"value".to_vec());
        IndexPending {
            upserts: BTreeSet::from(["workspace::N".to_string()]),
            deletes: BTreeSet::new(),
        }
        .save(&pending_path(&index))
        .unwrap();
        // An index must exist for a non-migration store; start it empty.
        SecretIndex::load(&index).unwrap().save().unwrap();
        let store = testing::reopened_store(&index, backend);
        assert_eq!(store.list_meta().unwrap().len(), 1);
    }

    #[test]
    fn export_all_skips_a_name_that_is_both_indexed_and_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let (store, _backend, _) = fault_store(dir.path());
        store.replace_all(&sample_values()).unwrap();
        store
            .quarantine
            .lock()
            .unwrap()
            .insert("workspace::C".to_string());
        let exported = store.export_all().unwrap();
        assert_eq!(exported.len(), 4);
        assert!(!exported.contains_key("workspace::C"));
    }

    #[test]
    fn delete_of_a_name_both_quarantined_and_indexed_removes_both() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        store.replace_all(&sample_values()).unwrap();
        store
            .quarantine
            .lock()
            .unwrap()
            .insert("workspace::C".to_string());
        store.delete("workspace::C").unwrap();
        assert!(store.quarantined_names().is_empty());
        assert!(!store
            .list_meta()
            .unwrap()
            .iter()
            .any(|meta| meta.name == "workspace::C"));
        assert_eq!(store.export_all().unwrap().len(), 4);
        // And the index file agrees after a restart.
        let reopened = testing::reopened_store(&index, backend);
        assert_eq!(reopened.list_meta().unwrap().len(), 4);
    }

    #[test]
    fn write_error_classes_survive_the_linux_backend_mapping() {
        // Locked or unreachable keychain stays `unavailable` (fail closed);
        // only an item-specific rejection stays `rejected` (quarantinable).
        let locked = reclassify_write_error(
            "write",
            keyring_error(keyring::Error::NoStorageAccess("locked".into())),
        );
        assert!(locked.allows_cache_fallback());
        let platform = reclassify_write_error(
            "write",
            keyring_error(keyring::Error::PlatformFailure("down".into())),
        );
        assert!(platform.allows_cache_fallback());
        let unavailable = reclassify_write_error("delete", BackendError::unavailable("x"));
        assert!(unavailable.allows_cache_fallback());
        let invalid = reclassify_write_error(
            "write",
            keyring_error(keyring::Error::Invalid("attr".into(), "bad".into())),
        );
        assert!(!invalid.allows_cache_fallback());
        let ambiguous = reclassify_write_error(
            "write",
            keyring_error(keyring::Error::Ambiguous(Vec::new())),
        );
        assert!(!ambiguous.allows_cache_fallback());
    }

    #[test]
    fn quarantine_file_drops_indexed_names_at_load() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        store.replace_all(&sample_values()).unwrap();
        drop(store);
        fs::write(
            index.with_file_name(QUARANTINE_FILENAME),
            br#"["workspace::A","workspace::ZZ"]"#,
        )
        .unwrap();
        let reopened = testing::reopened_store(&index, backend);
        // A is indexed so it is never quarantined; ZZ has no marker to back it
        // up but is not indexed, so it is kept (no marker = migration still
        // in flight).
        assert_eq!(
            reopened.quarantined_names(),
            vec!["workspace::ZZ".to_string()]
        );
        assert!(reopened.get("workspace::A").unwrap().is_some());
        assert_eq!(reopened.export_all().unwrap().len(), 5);
    }

    #[test]
    fn quarantine_file_is_intersected_with_the_marker_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = quarantine_c(dir.path());
        drop(store);
        let sha = "0".repeat(64);
        fs::write(
            dir.path().join("secrets-migration.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1,
                "source_sha256": sha,
                "backup_sha256": sha,
                "index_sha256": sha,
                "count": 4,
                "quarantined": ["workspace::C"],
            }))
            .unwrap(),
        )
        .unwrap();
        // Tampered live file: the real name plus invented ones.
        fs::write(
            index.with_file_name(QUARANTINE_FILENAME),
            br#"["workspace::C","workspace::INVENTED","workspace::A"]"#,
        )
        .unwrap();
        let reopened = testing::reopened_store(&index, backend);
        assert_eq!(
            reopened.quarantined_names(),
            vec!["workspace::C".to_string()]
        );
        assert_eq!(reopened.export_all().unwrap().len(), 4);
    }

    #[test]
    fn oversized_or_garbage_quarantine_file_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (store, backend, index) = fault_store(dir.path());
        store.replace_all(&sample_values()).unwrap();
        drop(store);
        let path = index.with_file_name(QUARANTINE_FILENAME);
        let names: Vec<String> = (0..=MAX_QUARANTINED_NAMES)
            .map(|n| format!("workspace::N{n}"))
            .collect();
        fs::write(&path, serde_json::to_vec(&names).unwrap()).unwrap();
        assert!(KeyringStore::with_backend_config_in(
            &index,
            Arc::new(backend.clone()),
            false,
            SERVICE
        )
        .is_err());
        fs::write(&path, b"not json").unwrap();
        assert!(
            KeyringStore::with_backend_config_in(&index, Arc::new(backend), false, SERVICE)
                .is_err()
        );
    }

    // ── real-backend shape probe (founder-run) ──────────────────────────

    /// Synthetic values of the shapes a real secret can take. Never real data.
    fn probe_shapes() -> Vec<(&'static str, String)> {
        fn pem(line_end: &str, trailing: bool, target: usize) -> String {
            let mut out = format!("-----BEGIN PRIVATE KEY-----{line_end}");
            let mut n = 0u32;
            while out.len() < target {
                let line: String = (0..64)
                    .map(|i| {
                        let v = (n.wrapping_mul(31).wrapping_add(i * 7)) % 64;
                        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                            [v as usize] as char
                    })
                    .collect();
                out.push_str(&line);
                out.push_str(line_end);
                n += 1;
            }
            out.push_str("-----END PRIVATE KEY-----");
            if trailing {
                out.push_str(line_end);
            }
            out
        }
        fn entropy(len: usize) -> String {
            let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
            (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
                        [(x % 64) as usize] as char
                })
                .collect()
        }
        vec![
            ("pem_1_7k_lf", pem("\n", false, 1700)),
            ("pem_1_7k_crlf", pem("\r\n", false, 1700)),
            ("pem_1_7k_trailing_newline", pem("\n", true, 1700)),
            ("escaped_backslash_n", pem("\\n", false, 1700)),
            ("base64_high_entropy_1_7k", entropy(1700)),
            ("size_4k", entropy(4 * 1024)),
            ("size_16k", entropy(16 * 1024)),
            (
                "unicode",
                "k\u{00e9}y-\u{4e2d}\u{6587}-\u{1f511}-\u{0645}".repeat(60),
            ),
            ("short", "short-value".to_string()),
        ]
    }

    /// Run by the founder to find the shape the real keychain mishandles:
    /// `IKENGA_QUARANTINE_PROBE=1 cargo test --lib secrets::keyring_store::tests::real_keychain_shape_probe -- --ignored --nocapture`.
    /// Uses only the isolated service `ikenga-quarantine-probe` (never the
    /// production one) and sweeps its items afterwards. Prints name, length
    /// and class per shape, never a value.
    #[test]
    #[ignore = "touches the real OS keychain under the isolated service ikenga-quarantine-probe"]
    fn real_keychain_shape_probe() {
        const PROBE_SERVICE: &str = "ikenga-quarantine-probe";
        if std::env::var_os("IKENGA_QUARANTINE_PROBE").is_none() {
            eprintln!("set IKENGA_QUARANTINE_PROBE=1 to run the keychain shape probe");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let store =
            KeyringStore::new_with_service(dir.path().join("probe-index.json"), PROBE_SERVICE)
                .expect("open the probe keychain service");
        let shapes = probe_shapes();
        let mut items = Vec::new();

        println!(
            "probe service: {PROBE_SERVICE} backend: {}",
            store.backend_label()
        );
        for (shape, value) in &shapes {
            let name = format!("workspace::PROBE_{shape}");
            let item = store.item(&name).unwrap();
            items.push(item.clone());
            let class = match store.backend.set(&item, value.as_bytes()) {
                Err(error) => format!("write error ({})", error.allows_cache_fallback()),
                Ok(()) => match store.backend.get_authoritative(&item) {
                    Err(error) => {
                        format!("read error (unavailable={})", error.allows_cache_fallback())
                    }
                    Ok(None) => FailureClass::ReadBackNone.label().to_string(),
                    Ok(Some(actual)) => match classify_mismatch(value.as_bytes(), &actual) {
                        None => "ok".to_string(),
                        Some((class, at)) => format!(
                            "{} (actual_len={}, first_diff_index={:?})",
                            class.label(),
                            actual.len(),
                            at
                        ),
                    },
                },
            };
            println!("shape={shape} len={} class={class}", value.len());
        }

        // The same shapes through the migration code path, all at once.
        let batch: BTreeMap<String, String> = shapes
            .iter()
            .map(|(shape, value)| (format!("workspace::PROBE_{shape}"), value.clone()))
            .collect();
        match store.replace_all_quarantining(&batch) {
            Ok(outcome) => println!(
                "batch: migrated={} quarantined={:?}",
                outcome.count, outcome.quarantined
            ),
            Err(error) => println!("batch: failed closed: {error}"),
        }

        // Sweep: nothing may stay behind under the probe service.
        let _ = store.replace_all(&BTreeMap::new());
        for item in &items {
            let _ = store.backend.delete(item);
        }
        let left = items
            .iter()
            .filter(|item| matches!(store.backend.get_authoritative(item), Ok(Some(_))))
            .count();
        println!("sweep: items left under {PROBE_SERVICE} = {left}");
    }
}
