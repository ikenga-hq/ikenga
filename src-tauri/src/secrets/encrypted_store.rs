use std::collections::BTreeMap;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};

use super::crypto;
use super::store::{SecretMeta, SecretsStore, StoreError};
use super::unlock::UnlockState;

const VALUE_PREFIX: &str = "ikenga-secret:v1:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EncryptedValue {
    version: u32,
    nonce: Vec<u8>,
    ciphertext: Vec<u8>,
}

pub struct EncryptedStore {
    inner: Arc<dyn SecretsStore>,
    unlock: Arc<UnlockState>,
}

impl EncryptedStore {
    pub fn new(inner: Arc<dyn SecretsStore>, unlock: Arc<UnlockState>) -> Self {
        Self { inner, unlock }
    }

    pub fn prepare_encryption(&self) -> Result<(), StoreError> {
        self.with_store_dek(|dek| {
            let names = self.inner.list_meta()?;
            let mut values = BTreeMap::new();
            for meta in names {
                let stored = self.inner.get(&meta.name)?.ok_or_else(|| {
                    StoreError::uncommitted(format!("indexed secret is missing: {}", meta.name))
                })?;
                let encoded = if is_encrypted(&stored) {
                    decode_value(&stored, dek, &meta.name)?;
                    stored
                } else {
                    encode_value(dek, &stored, &meta.name)?
                };
                values.insert(meta.name, encoded);
            }
            self.inner.replace_all(&values).map(|_| ())
        })
    }

    fn with_store_dek<T>(
        &self,
        f: impl FnOnce(&[u8; 32]) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.unlock.with_dek(f)
    }

    fn get_with_dek(&self, dek: &[u8; 32], name: &str) -> Result<Option<String>, StoreError> {
        let Some(stored) = self.inner.get(name)? else {
            return Ok(None);
        };
        if !is_encrypted(&stored) {
            return Ok(Some(stored));
        }
        decode_value(&stored, dek, name).map(Some)
    }
}

impl SecretsStore for EncryptedStore {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
        self.with_store_dek(|dek| self.get_with_dek(dek, name))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
        self.with_store_dek(|dek| {
            let encoded = encode_value(dek, value, name)?;
            self.inner.set(name, &encoded)
        })
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        self.with_store_dek(|_| self.inner.delete(name))
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        self.inner.list_meta()
    }

    fn export_all(&self) -> Result<BTreeMap<String, String>, StoreError> {
        self.with_store_dek(|dek| {
            let mut out = BTreeMap::new();
            for meta in self.inner.list_meta()? {
                let value = self.get_with_dek(dek, &meta.name)?.ok_or_else(|| {
                    StoreError::uncommitted(format!("indexed secret is missing: {}", meta.name))
                })?;
                out.insert(meta.name, value);
            }
            Ok(out)
        })
    }

    fn import_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        self.with_store_dek(|dek| {
            let mut encoded = BTreeMap::new();
            for (name, value) in values {
                encoded.insert(name.clone(), encode_value(dek, value, name)?);
            }
            self.inner.import_all(&encoded)
        })
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        self.with_store_dek(|dek| {
            let mut encoded = BTreeMap::new();
            for (name, value) in values {
                encoded.insert(name.clone(), encode_value(dek, value, name)?);
            }
            self.inner.replace_all(&encoded)
        })
    }

    fn probe(&self) -> Result<(), StoreError> {
        self.inner.probe()
    }

    fn backend_label(&self) -> &'static str {
        self.inner.backend_label()
    }

    fn diagnostics(&self) -> Vec<String> {
        self.inner.diagnostics()
    }
}

fn is_encoded(value: &str) -> bool {
    value.starts_with(VALUE_PREFIX)
}

fn is_encrypted(value: &str) -> bool {
    is_encoded(value)
}

fn encode_value(dek: &[u8; 32], value: &str, name: &str) -> Result<String, StoreError> {
    let encrypted = crypto::encrypt_value(dek, value.as_bytes(), name.as_bytes())
        .map_err(|_| StoreError::unknown("encrypt secret failed"))?;
    if encrypted.len() < crypto::NONCE_LEN + crypto::TAG_LEN {
        return Err(StoreError::unknown("encrypted secret is too short"));
    }
    let (nonce, ciphertext) = encrypted.split_at(crypto::NONCE_LEN);
    let body = EncryptedValue {
        version: crypto::ENVELOPE_VERSION,
        nonce: nonce.to_vec(),
        ciphertext: ciphertext.to_vec(),
    };
    let json = serde_json::to_vec(&body)
        .map_err(|_| StoreError::unknown("serialize encrypted secret failed"))?;
    Ok(format!("{VALUE_PREFIX}{}", STANDARD_NO_PAD.encode(json)))
}

fn decode_value(value: &str, dek: &[u8; 32], name: &str) -> Result<String, StoreError> {
    let encoded = value
        .strip_prefix(VALUE_PREFIX)
        .ok_or_else(|| StoreError::unknown("secret is not encrypted"))?;
    let json = STANDARD_NO_PAD
        .decode(encoded)
        .map_err(|_| StoreError::unknown("encrypted secret is not valid base64"))?;
    let body: EncryptedValue = serde_json::from_slice(&json)
        .map_err(|_| StoreError::unknown("encrypted secret has invalid metadata"))?;
    if body.version != crypto::ENVELOPE_VERSION
        || body.nonce.len() != crypto::NONCE_LEN
        || body.ciphertext.len() < crypto::TAG_LEN
    {
        return Err(StoreError::unknown("encrypted secret has invalid metadata"));
    }
    let mut combined = Vec::with_capacity(body.nonce.len() + body.ciphertext.len());
    combined.extend_from_slice(&body.nonce);
    combined.extend_from_slice(&body.ciphertext);
    let plaintext = crypto::decrypt_value(dek, &combined, name.as_bytes())
        .map_err(|_| StoreError::unknown("decrypt secret failed"))?;
    let mut plaintext = plaintext;
    let raw = std::mem::take(&mut *plaintext);
    String::from_utf8(raw).map_err(|_| StoreError::unknown("decrypted secret is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::secrets::store::{StoreError, StoreErrorKind};

    #[derive(Default)]
    struct MemoryStore {
        values: Mutex<BTreeMap<String, String>>,
    }

    impl MemoryStore {
        fn insert(&self, name: &str, value: &str) {
            self.values
                .lock()
                .unwrap()
                .insert(name.to_string(), value.to_string());
        }
    }

    impl SecretsStore for MemoryStore {
        fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
            Ok(self.values.lock().unwrap().get(name).cloned())
        }

        fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
            self.insert(name, value);
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), StoreError> {
            self.values.lock().unwrap().remove(name);
            Ok(())
        }

        fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .map(|name| SecretMeta { name })
                .collect())
        }

        fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
            *self.values.lock().unwrap() = values.clone();
            Ok(values.len())
        }

        fn probe(&self) -> Result<(), StoreError> {
            Ok(())
        }

        fn backend_label(&self) -> &'static str {
            "memory"
        }
    }

    fn encrypted_store(dir: &std::path::Path) -> (Arc<MemoryStore>, EncryptedStore) {
        let raw = Arc::new(MemoryStore::default());
        let unlock = Arc::new(UnlockState::with_path(
            dir.join(super::super::unlock::UNLOCK_ENVELOPE_FILENAME),
            Duration::from_secs(60),
        ));
        let store = EncryptedStore::new(raw.clone(), unlock);
        (raw, store)
    }

    #[test]
    fn locked_store_returns_typed_locked_error_and_unlocks_transparently() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        let unlock = store.unlock.clone();
        unlock.set_passphrase("passphrase").unwrap();
        unlock.lock().unwrap();
        raw.insert("workspace::TOKEN", "plain");
        assert_eq!(
            store.get("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Locked
        );
        unlock.unlock("passphrase").unwrap();
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("plain")
        );
        store.set("workspace::TOKEN", "new").unwrap();
        assert!(raw.values.lock().unwrap()["workspace::TOKEN"].starts_with(VALUE_PREFIX));
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("new")
        );
    }

    #[test]
    fn prepare_encryption_converts_legacy_values_and_import_is_atomic_at_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        let unlock = store.unlock.clone();
        raw.insert("workspace::TOKEN", "legacy");
        unlock.set_passphrase("passphrase").unwrap();
        store.prepare_encryption().unwrap();
        assert!(raw.values.lock().unwrap()["workspace::TOKEN"].starts_with(VALUE_PREFIX));
        let mut values = BTreeMap::new();
        values.insert("workspace::NEW".to_string(), "value".to_string());
        store.import_all(&values).unwrap();
        assert_eq!(store.export_all().unwrap().len(), 2);
    }
}
