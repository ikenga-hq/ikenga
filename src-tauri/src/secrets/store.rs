use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::unlock::UnlockError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMeta {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreErrorKind {
    Locked,
    Unavailable,
    Invalid,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    kind: StoreErrorKind,
    message: String,
    committed: bool,
}

impl StoreError {
    pub fn uncommitted(message: impl Into<String>) -> Self {
        Self::with_kind(StoreErrorKind::Unknown, message, false)
    }

    pub fn committed(message: impl Into<String>) -> Self {
        Self::with_kind(StoreErrorKind::Unknown, message, true)
    }

    pub fn locked() -> Self {
        Self::with_kind(StoreErrorKind::Locked, "secret store is locked", false)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::with_kind(StoreErrorKind::Unavailable, message, false)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::with_kind(StoreErrorKind::Invalid, message, false)
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self::uncommitted(message)
    }

    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    pub fn code(&self) -> &'static str {
        match self.kind {
            StoreErrorKind::Locked => "locked",
            StoreErrorKind::Unavailable => "unavailable",
            StoreErrorKind::Invalid => "invalid",
            StoreErrorKind::Unknown => "unknown",
        }
    }

    pub fn is_locked(&self) -> bool {
        self.kind == StoreErrorKind::Locked
    }

    pub fn is_unavailable(&self) -> bool {
        self.kind == StoreErrorKind::Unavailable
    }

    pub fn is_invalid(&self) -> bool {
        self.kind == StoreErrorKind::Invalid
    }

    pub fn is_unknown(&self) -> bool {
        self.kind == StoreErrorKind::Unknown
    }

    pub fn is_committed(&self) -> bool {
        self.committed
    }

    fn with_kind(kind: StoreErrorKind, message: impl Into<String>, committed: bool) -> Self {
        Self {
            kind,
            message: message.into(),
            committed,
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

impl From<StoreError> for String {
    fn from(error: StoreError) -> Self {
        error.message
    }
}

impl From<UnlockError> for StoreError {
    fn from(error: UnlockError) -> Self {
        match error {
            UnlockError::Locked | UnlockError::NotConfigured => Self::locked(),
            UnlockError::WrongPassphrase => Self::unknown("wrong passphrase"),
            UnlockError::InvalidPassphrase => Self::unavailable("passphrase is invalid"),
            other => Self::unknown(other.to_string()),
        }
    }
}

pub trait SecretsStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError>;

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError>;

    fn delete(&self, name: &str) -> Result<(), StoreError>;

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError>;

    fn export_all(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let mut out = BTreeMap::new();
        for meta in self.list_meta()? {
            match self.get(&meta.name)? {
                Some(value) => {
                    out.insert(meta.name, value);
                }
                None => {
                    return Err(StoreError::uncommitted(format!(
                        "indexed secret is missing: {}",
                        meta.name
                    )))
                }
            }
        }
        Ok(out)
    }

    fn import_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        for (name, value) in values {
            self.set(name, value)?;
        }
        Ok(values.len())
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError>;

    fn probe(&self) -> Result<(), StoreError>;

    /// Bring every stored value up to the store's at-rest format. For
    /// `EncryptedStore` with a configured passphrase this encrypts any
    /// plaintext value left from before the passphrase was set; plain stores
    /// have nothing to do. Deliberately has no default body: callers reach it
    /// through `&dyn SecretsStore`, so a wrapper that forgot to override it
    /// would silently skip the migration.
    fn prepare_encryption(&self) -> Result<(), StoreError>;

    fn backend_label(&self) -> &'static str;

    fn diagnostics(&self) -> Vec<String> {
        Vec::new()
    }
}

pub type SharedSecretStore = Arc<dyn SecretsStore>;
pub type SharedSecretStoreSlot = Arc<Mutex<Option<SharedSecretStore>>>;

pub struct UnavailableSecretStore {
    reason: String,
}

impl UnavailableSecretStore {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn error(&self) -> StoreError {
        StoreError::unavailable(format!("secret store unavailable: {}", self.reason))
    }
}

impl SecretsStore for UnavailableSecretStore {
    fn get(&self, _name: &str) -> Result<Option<String>, StoreError> {
        Err(self.error())
    }

    fn set(&self, _name: &str, _value: &str) -> Result<(), StoreError> {
        Err(self.error())
    }

    fn delete(&self, _name: &str) -> Result<(), StoreError> {
        Err(self.error())
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        Err(self.error())
    }

    fn replace_all(&self, _values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        Err(self.error())
    }

    fn probe(&self) -> Result<(), StoreError> {
        Err(self.error())
    }

    fn prepare_encryption(&self) -> Result<(), StoreError> {
        Err(self.error())
    }

    fn backend_label(&self) -> &'static str {
        "unavailable"
    }

    fn diagnostics(&self) -> Vec<String> {
        vec![self.reason.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_store_fails_all_access_paths() {
        let store = UnavailableSecretStore::new("migration failed");
        assert_eq!(
            store.get("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.set("workspace::TOKEN", "value").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.delete("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.list_meta().unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.replace_all(&BTreeMap::new()).unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.probe().unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
    }

    #[test]
    fn store_error_tracks_committed_state() {
        assert!(!StoreError::uncommitted("failed").is_committed());
        assert!(StoreError::committed("saved").is_committed());
    }
}
