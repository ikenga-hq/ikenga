//! Desktop secrets substrate (ADR-022). Gated on `desktop` in `lib.rs`.
//!
//! `crypto` and `store` are compiled in both feature sets: they are mounted
//! under `crate::secrets_env` (the headless daemon needs the `SecretsStore`
//! trait for the WP-21 per-principal store) and re-exported here so every
//! `crate::secrets::{crypto, store}` / `super::{crypto, store}` path keeps
//! resolving to the one definition.
pub use crate::secrets_env::{crypto, store};
pub mod encrypted_store;
pub mod index;
pub mod keyring_store;
pub mod migrate;
pub mod unlock;

pub use encrypted_store::EncryptedStore;
pub use keyring_store::KeyringStore;
pub use store::{
    SecretMeta, SecretsStore, SharedSecretStore, SharedSecretStoreSlot, StoreError,
    StoreErrorKind, UnavailableSecretStore,
};
pub use unlock::{
    LockState, UnlockError, UnlockState, VaultMode, DEFAULT_IDLE_TIMEOUT,
    UNLOCK_ENVELOPE_FILENAME,
};
