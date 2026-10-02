//! The ADR-022 `SecretsStore` substrate.
//!
//! Compiled into both binaries. The desktop's backend — the OS keychain
//! (`keyring_store`) and the one-shot Stronghold migration (`migrate`) — stays
//! `desktop`-only, as ADR-022 decided. The headless daemon gets the
//! per-principal store (`principal_store`, remote-access WP-21 / ADR-023 §4),
//! which a T1 principal child opens with the wrapping key its broker derives
//! for it; `secrets_env` layers it over the `IKENGA_SECRET_*` operator default.

pub mod crypto;
pub mod encrypted_store;
pub mod hkdf;
pub mod index;
#[cfg(feature = "desktop")]
pub mod keyring_store;
#[cfg(feature = "desktop")]
pub mod migrate;
pub mod principal_store;
pub mod scope;
pub mod store;
pub mod unlock;

pub use encrypted_store::EncryptedStore;
#[cfg(feature = "desktop")]
pub use keyring_store::KeyringStore;
pub use principal_store::{PrincipalStore, WrapKey};
pub use store::{
    SecretMeta, SecretsStore, SharedSecretStore, SharedSecretStoreSlot, StoreError, StoreErrorKind,
    StoreOwner, UnavailableSecretStore,
};
pub use unlock::{
    LockState, UnlockError, UnlockState, VaultMode, DEFAULT_IDLE_TIMEOUT, UNLOCK_ENVELOPE_FILENAME,
};
