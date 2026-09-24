use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMeta {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreError {
    message: String,
    committed: bool,
}

impl StoreError {
    pub fn uncommitted(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            committed: false,
        }
    }

    pub fn committed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            committed: true,
        }
    }

    pub fn is_committed(&self) -> bool {
        self.committed
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
}

impl SecretsStore for UnavailableSecretStore {
    fn get(&self, _name: &str) -> Result<Option<String>, StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
    }

    fn set(&self, _name: &str, _value: &str) -> Result<(), StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
    }

    fn delete(&self, _name: &str) -> Result<(), StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
    }

    fn replace_all(&self, _values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
    }

    fn probe(&self) -> Result<(), StoreError> {
        Err(StoreError::uncommitted(format!(
            "secret store unavailable: {}",
            self.reason
        )))
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
        assert!(store.get("workspace::TOKEN").is_err());
        assert!(store.set("workspace::TOKEN", "value").is_err());
        assert!(store.delete("workspace::TOKEN").is_err());
        assert!(store.list_meta().is_err());
        assert!(store.replace_all(&BTreeMap::new()).is_err());
        assert!(store.probe().is_err());
    }

    #[test]
    fn store_error_tracks_committed_state() {
        assert!(!StoreError::uncommitted("failed").is_committed());
        assert!(StoreError::committed("saved").is_committed());
    }
}
