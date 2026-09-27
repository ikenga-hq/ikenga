//! Value-encryption wrapper over the keychain store (WP-34, DEC-47).
//!
//! Three modes, decided per call from [`UnlockState::mode`]:
//!
//! - **Unconfigured** (no passphrase ever seen): a pure passthrough to the
//!   inner store — no encryption, never `Locked`. This is the WP-33
//!   behaviour the WP-34 DoD requires. Before the first passthrough the
//!   store is scanned once for values in the encrypted format; finding one
//!   (or meeting one later) latches "configured", because it proves a
//!   passphrase existed and its envelope went missing.
//! - **Configured**: every value is AES-256-GCM encrypted with the DEK before
//!   it reaches the keychain; without the DEK in memory, value access returns
//!   a typed `Locked` error. Setting the first passphrase (and every unlock)
//!   runs `prepare_encryption`, which encrypts any plaintext value left from
//!   the unconfigured era — best-effort, so one orphaned or undecodable
//!   entry never blocks an unlock. A plaintext value met on a configured
//!   read is encrypted in place (migrate-on-read) so it is never served from
//!   the keychain as plaintext indefinitely.
//! - **EnvelopeMissing** (configured, envelope gone): every value operation
//!   fails closed with `Unavailable`. Nothing is passed through, nothing new
//!   is written under a key that can no longer be recovered.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::crypto;
use super::store::{SecretMeta, SecretsStore, StoreError};
use super::unlock::{UnlockError, UnlockState, VaultMode};

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
    /// The one-time scan for encrypted values (see `mode`) has completed.
    evidence_scanned: AtomicBool,
}

impl EncryptedStore {
    pub fn new(inner: Arc<dyn SecretsStore>, unlock: Arc<UnlockState>) -> Self {
        Self {
            inner,
            unlock,
            evidence_scanned: AtomicBool::new(false),
        }
    }

    /// Vault mode for this call. While the unlock state says `Unconfigured`
    /// the inner store is scanned once for values in the encrypted format:
    /// one is proof that a passphrase was configured and its envelope went
    /// missing, so "configured" is latched instead of passing through.
    /// Unreadable entries are skipped (name-only warning); a failure to list
    /// the store fails the call and the scan is retried next time.
    fn mode(&self) -> Result<VaultMode, StoreError> {
        let mode = self.unlock.mode();
        if mode != VaultMode::Unconfigured || self.evidence_scanned.load(Ordering::Acquire) {
            return Ok(mode);
        }
        for meta in self.inner.list_meta()? {
            match self.inner.get(&meta.name) {
                Ok(Some(stored)) => {
                    let stored = Zeroizing::new(stored);
                    if is_encrypted(&stored) {
                        self.unlock.mark_configured();
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => log::warn!(
                    "[secrets] could not inspect `{}` for encrypted values ({})",
                    meta.name,
                    error.code()
                ),
            }
        }
        self.evidence_scanned.store(true, Ordering::Release);
        Ok(self.unlock.mode())
    }

    /// A value in the encrypted format met while unconfigured: latch
    /// "configured" and refuse it (never serve ciphertext as a value).
    fn encrypted_without_envelope(&self, name: &str) -> StoreError {
        self.unlock.mark_configured();
        log::warn!("[secrets] `{name}` is encrypted but no passphrase envelope exists");
        envelope_missing()
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
        if is_encrypted(&stored) {
            return decode_value(&stored, dek, name).map(Some);
        }
        // Plaintext on a configured vault: left from before the passphrase
        // was set, or written by a call that raced the first set. Encrypt it
        // in place now. A failed write-back is logged and retried by the
        // `prepare_encryption` that runs on every unlock.
        let encoded = encode_value(dek, &stored, name)?;
        if let Err(error) = self.inner.set(name, &encoded) {
            log::warn!(
                "[secrets] could not encrypt plaintext value `{name}` on read: {error}"
            );
        }
        Ok(Some(stored))
    }
}

/// Typed fail-closed error for a configured vault whose envelope is gone.
fn envelope_missing() -> StoreError {
    UnlockError::EnvelopeMissing.into()
}

impl SecretsStore for EncryptedStore {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => match self.inner.get(name)? {
                Some(value) if is_encrypted(&value) => Err(self.encrypted_without_envelope(name)),
                other => Ok(other),
            },
            VaultMode::EnvelopeMissing => Err(envelope_missing()),
            VaultMode::Configured => self.with_store_dek(|dek| self.get_with_dek(dek, name)),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => self.inner.set(name, value),
            VaultMode::EnvelopeMissing => Err(envelope_missing()),
            VaultMode::Configured => self.with_store_dek(|dek| {
                let encoded = encode_value(dek, value, name)?;
                self.inner.set(name, &encoded)
            }),
        }
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => self.inner.delete(name),
            VaultMode::EnvelopeMissing => Err(envelope_missing()),
            VaultMode::Configured => self.with_store_dek(|_| self.inner.delete(name)),
        }
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        self.inner.list_meta()
    }

    fn export_all(&self) -> Result<BTreeMap<String, String>, StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => {
                let values = self.inner.export_all()?;
                if let Some(name) = values
                    .iter()
                    .find(|(_, value)| is_encrypted(value))
                    .map(|(name, _)| name)
                {
                    return Err(self.encrypted_without_envelope(name));
                }
                return Ok(values);
            }
            VaultMode::EnvelopeMissing => return Err(envelope_missing()),
            VaultMode::Configured => {}
        }
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
        match self.mode()? {
            VaultMode::Unconfigured => return self.inner.import_all(values),
            VaultMode::EnvelopeMissing => return Err(envelope_missing()),
            VaultMode::Configured => {}
        }
        self.with_store_dek(|dek| {
            let mut encoded = BTreeMap::new();
            for (name, value) in values {
                encoded.insert(name.clone(), encode_value(dek, value, name)?);
            }
            self.inner.import_all(&encoded)
        })
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => return self.inner.replace_all(values),
            VaultMode::EnvelopeMissing => return Err(envelope_missing()),
            VaultMode::Configured => {}
        }
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

    /// Encrypt every plaintext value in the inner store. A no-op while no
    /// passphrase is configured; `Locked` when configured without the DEK;
    /// `Unavailable` when the envelope is missing.
    ///
    /// Best-effort per entry, because it runs on every unlock after the DEK
    /// is already in memory: an index entry whose keychain item is gone, a
    /// read that fails, or an encrypted value that does not open with the
    /// current DEK is skipped with a name-only warning and left exactly as it
    /// is (never deleted), and every other plaintext value is still
    /// encrypted. Each value is rewritten on its own (`set`), not through
    /// `replace_all`/`import_all`, whose whole-index snapshot would fail on
    /// the very orphan being skipped. Only a failure to list the store is an
    /// error. Already-encrypted values are not rewritten, so routine unlocks
    /// do not churn the keychain.
    fn prepare_encryption(&self) -> Result<(), StoreError> {
        match self.mode()? {
            VaultMode::Unconfigured => return Ok(()),
            VaultMode::EnvelopeMissing => return Err(envelope_missing()),
            VaultMode::Configured => {}
        }
        self.with_store_dek(|dek| {
            let mut encrypted = 0usize;
            let mut skipped = 0usize;
            for meta in self.inner.list_meta()? {
                let name = meta.name;
                let stored = match self.inner.get(&name) {
                    Ok(Some(stored)) => Zeroizing::new(stored),
                    Ok(None) => {
                        log::warn!(
                            "[secrets] encryption pass skipped `{name}`: indexed but missing from the keychain"
                        );
                        skipped += 1;
                        continue;
                    }
                    Err(error) => {
                        log::warn!(
                            "[secrets] encryption pass skipped `{name}`: keychain read failed ({})",
                            error.code()
                        );
                        skipped += 1;
                        continue;
                    }
                };
                if is_encrypted(&stored) {
                    match decode_value(&stored, dek, &name) {
                        Ok(plaintext) => drop(Zeroizing::new(plaintext)),
                        Err(_) => {
                            log::warn!(
                                "[secrets] encryption pass skipped `{name}`: stored value does not decrypt with the current key"
                            );
                            skipped += 1;
                        }
                    }
                    continue;
                }
                let encoded = match encode_value(dek, &stored, &name) {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        log::warn!(
                            "[secrets] encryption pass skipped `{name}`: encrypt failed ({})",
                            error.code()
                        );
                        skipped += 1;
                        continue;
                    }
                };
                match self.inner.set(&name, &encoded) {
                    Ok(()) => encrypted += 1,
                    Err(error) => {
                        log::warn!(
                            "[secrets] encryption pass skipped `{name}`: keychain write failed ({})",
                            error.code()
                        );
                        skipped += 1;
                    }
                }
            }
            if encrypted > 0 || skipped > 0 {
                log::info!(
                    "[secrets] encryption pass: {encrypted} value(s) encrypted, {skipped} skipped"
                );
            }
            Ok(())
        })
    }

    fn detect_configuration(&self) -> Result<bool, StoreError> {
        Ok(self.mode()? != VaultMode::Unconfigured)
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
        /// Indexed names whose keychain item is gone (listed, `get` = None).
        orphans: Mutex<std::collections::BTreeSet<String>>,
    }

    impl MemoryStore {
        fn insert(&self, name: &str, value: &str) {
            self.values
                .lock()
                .unwrap()
                .insert(name.to_string(), value.to_string());
        }

        fn add_orphan(&self, name: &str) {
            self.orphans.lock().unwrap().insert(name.to_string());
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
            let mut names: std::collections::BTreeSet<String> =
                self.values.lock().unwrap().keys().cloned().collect();
            names.extend(self.orphans.lock().unwrap().iter().cloned());
            Ok(names.into_iter().map(|name| SecretMeta { name }).collect())
        }

        fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
            *self.values.lock().unwrap() = values.clone();
            Ok(values.len())
        }

        fn probe(&self) -> Result<(), StoreError> {
            Ok(())
        }

        fn prepare_encryption(&self) -> Result<(), StoreError> {
            Ok(())
        }

        fn detect_configuration(&self) -> Result<bool, StoreError> {
            Ok(false)
        }

        fn backend_label(&self) -> &'static str {
            "memory"
        }
    }

    fn envelope_path(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join(super::super::unlock::UNLOCK_ENVELOPE_FILENAME)
    }

    fn encrypted_store(dir: &std::path::Path) -> (Arc<MemoryStore>, EncryptedStore) {
        let raw = Arc::new(MemoryStore::default());
        let store = store_over(raw.clone(), dir);
        (raw, store)
    }

    /// A fresh `UnlockState` + `EncryptedStore` over an existing raw store —
    /// what a new process sees after a restart.
    fn store_over(raw: Arc<MemoryStore>, dir: &std::path::Path) -> EncryptedStore {
        let unlock = Arc::new(UnlockState::with_path(
            envelope_path(dir),
            Duration::from_secs(60),
        ));
        EncryptedStore::new(raw, unlock)
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
    fn no_passphrase_is_a_passthrough_that_never_returns_locked() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        assert!(!store.unlock.is_configured());
        assert!(!store.unlock.state().locked);

        store.set("workspace::TOKEN", "plain").unwrap();
        // Stored exactly as WP-33 stored it: no envelope, no encryption.
        assert_eq!(raw.values.lock().unwrap()["workspace::TOKEN"], "plain");
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("plain")
        );
        assert_eq!(store.get("workspace::MISSING").unwrap(), None);

        let mut values = BTreeMap::new();
        values.insert("workspace::OTHER".to_string(), "other".to_string());
        store.import_all(&values).unwrap();
        assert_eq!(store.export_all().unwrap().len(), 2);
        store.prepare_encryption().unwrap();
        assert_eq!(raw.values.lock().unwrap()["workspace::OTHER"], "other");

        store.delete("workspace::TOKEN").unwrap();
        assert_eq!(store.get("workspace::TOKEN").unwrap(), None);
        store.replace_all(&BTreeMap::new()).unwrap();
        assert!(store.list_meta().unwrap().is_empty());
    }

    #[test]
    fn first_passphrase_encrypts_pre_existing_plaintext_through_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        store.set("workspace::TOKEN", "legacy").unwrap();
        store.set("My Token", "spaced").unwrap();
        assert_eq!(raw.values.lock().unwrap()["workspace::TOKEN"], "legacy");

        store.unlock.set_or_rotate("passphrase", None).unwrap();
        // Callers hold `&dyn SecretsStore`; the override must be reached
        // through the trait object, not an inherent method.
        let as_trait: &dyn SecretsStore = &store;
        as_trait.prepare_encryption().unwrap();
        {
            let stored = raw.values.lock().unwrap();
            assert!(stored["workspace::TOKEN"].starts_with(VALUE_PREFIX));
            assert!(stored["My Token"].starts_with(VALUE_PREFIX));
        }
        assert_eq!(
            store.get("workspace::TOKEN").unwrap().as_deref(),
            Some("legacy")
        );
        assert_eq!(store.get("My Token").unwrap().as_deref(), Some("spaced"));

        store.unlock.lock().unwrap();
        assert_eq!(
            store.get("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Locked
        );
    }

    #[test]
    fn plaintext_met_on_a_configured_read_is_encrypted_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        store.unlock.set_passphrase("passphrase").unwrap();
        // A plaintext write that raced the first set.
        raw.insert("workspace::LATE", "late");
        assert_eq!(
            store.get("workspace::LATE").unwrap().as_deref(),
            Some("late")
        );
        assert!(raw.values.lock().unwrap()["workspace::LATE"].starts_with(VALUE_PREFIX));
        assert_eq!(
            store.get("workspace::LATE").unwrap().as_deref(),
            Some("late")
        );
    }

    #[test]
    fn ciphertext_is_never_served_when_the_envelope_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        store.unlock.set_passphrase("passphrase").unwrap();
        store.set("workspace::TOKEN", "secret").unwrap();
        std::fs::remove_file(dir.path().join(super::super::unlock::UNLOCK_ENVELOPE_FILENAME))
            .unwrap();
        assert!(raw.values.lock().unwrap()["workspace::TOKEN"].starts_with(VALUE_PREFIX));
        assert_eq!(
            store.get("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.export_all().unwrap_err().kind(),
            StoreErrorKind::Unavailable
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

    #[test]
    fn envelope_deleted_after_configure_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        store.unlock.set_passphrase("passphrase").unwrap();
        store.set("workspace::TOKEN", "secret").unwrap();
        std::fs::remove_file(envelope_path(dir.path())).unwrap();

        // Still configured (sticky), and reported locked even though a DEK
        // was held when the envelope vanished.
        assert!(store.unlock.is_configured());
        let state = store.unlock.state();
        assert!(state.configured);
        assert!(state.locked);

        // Writes are refused, never stored as plaintext.
        assert_eq!(
            store.set("workspace::NEW", "value").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert!(!raw.values.lock().unwrap().contains_key("workspace::NEW"));

        // Reads never pass through: neither the ciphertext nor a plaintext
        // value planted after the envelope went missing is served.
        raw.insert("workspace::PLANTED", "planted");
        assert_eq!(
            store.get("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.get("workspace::PLANTED").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.delete("workspace::TOKEN").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert_eq!(
            store.prepare_encryption().unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );

        // No fresh envelope (and fresh DEK) over the existing ciphertext.
        assert_eq!(
            store.unlock.set_passphrase("fresh").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert_eq!(
            store.unlock.set_or_rotate("fresh", None).unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert_eq!(
            store.unlock.unlock("passphrase").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert!(!envelope_path(dir.path()).exists());
        assert!(raw.values.lock().unwrap()["workspace::TOKEN"].starts_with(VALUE_PREFIX));

        // Lock stays meaningful.
        store.unlock.lock().unwrap();
        assert!(store.unlock.state().locked);
    }

    #[test]
    fn ciphertext_without_envelope_latches_configured_in_a_fresh_process() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, first) = encrypted_store(dir.path());
        first.unlock.set_passphrase("passphrase").unwrap();
        first.set("workspace::TOKEN", "secret").unwrap();
        std::fs::remove_file(envelope_path(dir.path())).unwrap();

        // Restart: the new process has never seen the envelope. Its first
        // operation is a write, which must not go plaintext.
        let second = store_over(raw.clone(), dir.path());
        assert_eq!(
            second.set("workspace::NEW", "value").unwrap_err().kind(),
            StoreErrorKind::Unavailable
        );
        assert!(!raw.values.lock().unwrap().contains_key("workspace::NEW"));
        assert!(second.unlock.is_configured());
        assert!(second.unlock.state().locked);
        assert_eq!(
            second.unlock.set_passphrase("fresh").unwrap_err(),
            UnlockError::EnvelopeMissing
        );

        // Same through the explicit detection the set-passphrase command
        // runs before a first set.
        let third = store_over(raw, dir.path());
        let as_trait: &dyn SecretsStore = &third;
        assert!(as_trait.detect_configuration().unwrap());
        assert_eq!(
            third.unlock.set_or_rotate("fresh", None).unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert!(!envelope_path(dir.path()).exists());
    }

    #[test]
    fn detect_configuration_is_false_for_a_plain_vault() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        raw.insert("workspace::TOKEN", "plain");
        assert!(!store.detect_configuration().unwrap());
        store.unlock.set_or_rotate("passphrase", None).unwrap();
        assert!(store.detect_configuration().unwrap());
    }

    #[test]
    fn prepare_encryption_skips_orphans_and_undecodable_values() {
        let dir = tempfile::tempdir().unwrap();
        let (raw, store) = encrypted_store(dir.path());
        raw.insert("workspace::GOOD", "plain");
        // Index entry whose keychain item was deleted in Credential Manager.
        raw.add_orphan("workspace::ORPHAN");
        store.unlock.set_passphrase("passphrase").unwrap();
        // A value in the encrypted format that does not open with this DEK.
        let undecodable = format!("{VALUE_PREFIX}not-base64!");
        raw.insert("workspace::BAD", &undecodable);
        store.unlock.lock().unwrap();
        store.unlock.unlock("passphrase").unwrap();

        let as_trait: &dyn SecretsStore = &store;
        as_trait.prepare_encryption().unwrap();

        {
            let stored = raw.values.lock().unwrap();
            assert!(stored["workspace::GOOD"].starts_with(VALUE_PREFIX));
            // Skipped entries are left exactly as they were, never dropped.
            assert_eq!(stored["workspace::BAD"], undecodable);
        }
        assert!(store
            .list_meta()
            .unwrap()
            .iter()
            .any(|meta| meta.name == "workspace::ORPHAN"));
        assert_eq!(
            store.get("workspace::GOOD").unwrap().as_deref(),
            Some("plain")
        );
        assert!(store.unlock.is_unlocked());
    }
}
