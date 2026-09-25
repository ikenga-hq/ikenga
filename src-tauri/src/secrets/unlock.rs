//! Passphrase unlock state for the secrets vault (WP-34, DEC-47).
//!
//! The passphrase is **optional**. With no envelope on disk the vault is
//! "unconfigured": `LockState.locked` is `false` and `EncryptedStore` passes
//! every call straight through to the keychain store, exactly as WP-33 did.
//! Only once an envelope exists (`configured`) does a missing in-memory DEK
//! mean `Locked`.
//!
//! "Configured" is **sticky**. It latches the first time an envelope is seen,
//! a DEK is held, or `EncryptedStore` meets a stored value in the encrypted
//! format (`mark_configured`). An envelope that disappears afterwards —
//! deleted by a same-user process (the DEC-47 threat), a partial cleanup or a
//! sync client — does not turn the vault back into a passthrough: the mode
//! becomes [`VaultMode::EnvelopeMissing`], any held DEK is dropped as on an
//! idle expiry (so the env-vault files are invalidated), value access fails
//! closed, lock and exit-wipe stay active, and `set_passphrase` refuses to
//! mint a fresh DEK over ciphertext only the lost envelope can open.
//!
//! Every time the DEK leaves memory (explicit lock or idle expiry) the lock
//! `generation` advances. Env-vault publication captures the generation
//! before it reads values and re-checks it under the publish mutex before
//! writing plaintext files, so a lock that lands mid-publish can never be
//! overtaken by a stale publish. Idle expiries that happen as a side effect
//! of another call (`state()`, `with_dek`) are remembered until
//! `expire_if_idle` reports them, so the env-vault invalidation that follows
//! an idle lock is never skipped.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::crypto::{self, CryptoError, WrappedDekEnvelope, DEK_LEN, MAX_ENVELOPE_BYTES};
use super::index::write_atomic;

pub const UNLOCK_ENVELOPE_FILENAME: &str = "secrets-unlock.json";
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockState {
    pub configured: bool,
    pub locked: bool,
    pub idle_timeout_secs: u64,
    pub last_activity_unix_ms: Option<u64>,
}

impl LockState {
    pub fn is_unlocked(&self) -> bool {
        self.configured && !self.locked
    }
}

/// Whether the vault carries a passphrase, derived from the envelope on disk
/// and the sticky configured latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultMode {
    /// No passphrase has ever been seen: WP-33 passthrough.
    Unconfigured,
    /// Envelope present: values are encrypted, `Locked` without the DEK.
    Configured,
    /// A passphrase was configured (latched) but the envelope is gone. Fails
    /// closed: never passthrough, never a fresh envelope over ciphertext.
    EnvelopeMissing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockError {
    Locked,
    NotConfigured,
    AlreadyConfigured,
    /// A passphrase was configured but `secrets-unlock.json` is gone. The
    /// stored ciphertext can only be opened by restoring that file.
    EnvelopeMissing,
    CurrentPassphraseRequired,
    InvalidPassphrase,
    WrongPassphrase,
    InvalidEnvelope(String),
    Crypto(CryptoError),
    Io(String),
    Poisoned,
}

impl std::fmt::Display for UnlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked => f.write_str("secrets are locked"),
            Self::NotConfigured => f.write_str("secrets passphrase is not configured"),
            Self::AlreadyConfigured => f.write_str("secrets passphrase is already configured"),
            Self::EnvelopeMissing => f.write_str(
                "secrets are passphrase-protected but the unlock envelope (secrets-unlock.json) is missing; restore it to regain access",
            ),
            Self::CurrentPassphraseRequired => {
                f.write_str("the current passphrase is required to change it")
            }
            Self::InvalidPassphrase => f.write_str("passphrase must not be empty"),
            Self::WrongPassphrase => f.write_str("wrong passphrase"),
            Self::InvalidEnvelope(message) => {
                write!(f, "invalid secrets unlock envelope: {message}")
            }
            Self::Crypto(error) => write!(f, "secrets crypto failure: {error}"),
            Self::Io(message) => write!(f, "secrets unlock storage failure: {message}"),
            Self::Poisoned => f.write_str("secrets unlock state is poisoned"),
        }
    }
}

impl std::error::Error for UnlockError {}

impl From<CryptoError> for UnlockError {
    fn from(error: CryptoError) -> Self {
        match error {
            CryptoError::InvalidPassphrase => Self::InvalidPassphrase,
            CryptoError::WrongPassphrase => Self::WrongPassphrase,
            other => Self::Crypto(other),
        }
    }
}

#[derive(Clone)]
pub struct UnlockState {
    path: Arc<Mutex<Option<PathBuf>>>,
    inner: Arc<Mutex<UnlockInner>>,
    idle_timeout: Duration,
}

struct UnlockInner {
    dek: Option<Zeroizing<[u8; DEK_LEN]>>,
    last_activity: Option<Instant>,
    last_activity_unix_ms: Option<u64>,
    /// Advances every time the DEK leaves memory (lock or idle expiry).
    generation: u64,
    /// An idle expiry happened (possibly inside `state()` / `with_dek`) and
    /// `expire_if_idle` has not reported it yet.
    expired_unobserved: bool,
    /// Sticky "a passphrase is configured" latch. Set once an envelope has
    /// been seen, a DEK has been held, or ciphertext was met in the store;
    /// never cleared for the life of the process.
    configured_latched: bool,
}

impl UnlockState {
    pub fn new() -> Self {
        Self::with_idle_timeout(DEFAULT_IDLE_TIMEOUT)
    }

    pub fn with_idle_timeout(idle_timeout: Duration) -> Self {
        Self {
            path: Arc::new(Mutex::new(None)),
            inner: Arc::new(Mutex::new(UnlockInner {
                dek: None,
                last_activity: None,
                last_activity_unix_ms: None,
                generation: 0,
                expired_unobserved: false,
                configured_latched: false,
            })),
            idle_timeout,
        }
    }

    pub fn with_path(path: impl Into<PathBuf>, idle_timeout: Duration) -> Self {
        let state = Self::with_idle_timeout(idle_timeout);
        *state.path.lock().expect("new unlock path mutex") = Some(path.into());
        state
    }

    pub fn configure_path(&self, path: impl Into<PathBuf>) -> Result<(), UnlockError> {
        let path = path.into();
        {
            let mut guard = self.path.lock().map_err(|_| UnlockError::Poisoned)?;
            match guard.as_ref() {
                Some(existing) if existing != &path => {
                    return Err(UnlockError::Io(
                        "secrets unlock path is already configured".to_string(),
                    ));
                }
                _ => *guard = Some(path),
            }
        }
        // Latch "configured" now (boot) if the envelope is present, so its
        // later disappearance can never make the vault a passthrough. Taken
        // after the path guard is released: lock order is `inner` → `path`.
        let _ = self.mode();
        Ok(())
    }

    pub fn envelope_path(&self) -> Result<PathBuf, UnlockError> {
        self.path
            .lock()
            .map_err(|_| UnlockError::Poisoned)?
            .clone()
            .ok_or(UnlockError::NotConfigured)
    }

    /// `true` while a passphrase is (or, sticky, was) configured — including
    /// [`VaultMode::EnvelopeMissing`]. Lock and exit-wipe key off this, so
    /// they stay active when the envelope disappears. Fails closed (`true`)
    /// on a poisoned state mutex.
    pub fn is_configured(&self) -> bool {
        self.mode() != VaultMode::Unconfigured
    }

    /// Current vault mode; latches "configured" whenever the envelope is seen.
    pub fn mode(&self) -> VaultMode {
        match self.inner.lock() {
            Ok(mut inner) => self.mode_locked(&mut inner),
            // Poisoned: never report a passthrough.
            Err(_) => VaultMode::EnvelopeMissing,
        }
    }

    /// Record at-rest evidence that a passphrase was configured (e.g.
    /// `EncryptedStore` met a value in the encrypted format). Sticky for the
    /// life of the process.
    pub fn mark_configured(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.configured_latched = true;
            self.reconcile_envelope_locked(&mut inner);
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.state().is_unlocked()
    }

    pub fn state(&self) -> LockState {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => {
                // Fail closed: a poisoned state never reads as a passthrough.
                return LockState {
                    configured: true,
                    locked: true,
                    idle_timeout_secs: self.idle_timeout.as_secs(),
                    last_activity_unix_ms: None,
                };
            }
        };
        self.expire_locked(&mut inner);
        self.reconcile_envelope_locked(&mut inner);
        let mode = self.mode_locked(&mut inner);
        let configured = mode != VaultMode::Unconfigured;
        LockState {
            configured,
            // No passphrase configured means there is nothing to unlock: the
            // vault behaves exactly as the plain keychain store (WP-33). A
            // configured vault whose envelope is missing always reads locked.
            locked: configured && (inner.dek.is_none() || mode == VaultMode::EnvelopeMissing),
            idle_timeout_secs: self.idle_timeout.as_secs(),
            last_activity_unix_ms: inner.last_activity_unix_ms,
        }
    }

    pub fn set_passphrase(&self, passphrase: &str) -> Result<(), UnlockError> {
        if passphrase.is_empty() {
            return Err(UnlockError::InvalidPassphrase);
        }
        let path = self.envelope_path()?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        if path.exists() {
            inner.configured_latched = true;
            return Err(UnlockError::AlreadyConfigured);
        }
        if inner.configured_latched || inner.dek.is_some() {
            // Configured before, envelope now gone: a fresh envelope would
            // mint a new DEK and orphan every value encrypted under the old
            // one (and discard a DEK still held in memory).
            inner.configured_latched = true;
            return Err(UnlockError::EnvelopeMissing);
        }
        let (envelope, dek) = crypto::create_envelope(passphrase)?;
        persist_envelope(&path, &envelope)?;
        inner.dek = Some(dek);
        inner.configured_latched = true;
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn rotate(&self, old_passphrase: &str, new_passphrase: &str) -> Result<(), UnlockError> {
        if new_passphrase.is_empty() {
            return Err(UnlockError::InvalidPassphrase);
        }
        let path = self.envelope_path()?;
        let envelope = self.read_configured_envelope(&path)?;
        let dek = crypto::unwrap_dek(old_passphrase, &envelope)?;
        let next = crypto::wrap_dek(new_passphrase, &dek)?;
        persist_envelope(&path, &next)?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        inner.dek = Some(dek);
        inner.configured_latched = true;
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn set_or_rotate(
        &self,
        passphrase: &str,
        current_passphrase: Option<&str>,
    ) -> Result<(), UnlockError> {
        match self.mode() {
            // First set: there is no existing passphrase to prove.
            VaultMode::Unconfigured => return self.set_passphrase(passphrase),
            VaultMode::EnvelopeMissing => return Err(UnlockError::EnvelopeMissing),
            VaultMode::Configured => {}
        }
        // Changing an existing passphrase always proves the current one, even
        // while unlocked: an unattended unlocked session must not be enough
        // to re-wrap the DEK under a passphrase someone else chose.
        match current_passphrase {
            Some(current) => self.rotate(current, passphrase),
            None => Err(UnlockError::CurrentPassphraseRequired),
        }
    }

    pub fn unlock(&self, passphrase: &str) -> Result<(), UnlockError> {
        let path = self.envelope_path()?;
        let envelope = self.read_configured_envelope(&path)?;
        let dek = crypto::unwrap_dek(passphrase, &envelope)?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        inner.dek = Some(dek);
        inner.configured_latched = true;
        self.touch_locked(&mut inner);
        Ok(())
    }

    /// `read_envelope`, reporting a missing file on a latched vault as the
    /// typed `EnvelopeMissing` rather than `NotConfigured`.
    fn read_configured_envelope(&self, path: &Path) -> Result<WrappedDekEnvelope, UnlockError> {
        match read_envelope(path) {
            Err(UnlockError::NotConfigured) if self.is_configured() => {
                Err(UnlockError::EnvelopeMissing)
            }
            other => other,
        }
    }

    pub fn lock(&self) -> Result<bool, UnlockError> {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        let was_unlocked = inner.dek.take().is_some();
        inner.last_activity = None;
        inner.last_activity_unix_ms = None;
        if was_unlocked {
            inner.generation = inner.generation.wrapping_add(1);
        }
        Ok(was_unlocked)
    }

    /// Expire the DEK if the idle timeout has passed. Returns `true` when an
    /// idle expiry happened since the last call — including one triggered as
    /// a side effect of `state()` or `with_dek` — so the caller's env-vault
    /// invalidation is never skipped.
    pub fn expire_if_idle(&self) -> Result<bool, UnlockError> {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        self.expire_locked(&mut inner);
        // Doubles as the idle loop's once-a-second check for a vanished
        // envelope: a held DEK is dropped and reported like an idle expiry,
        // so the env-vault files get invalidated.
        self.reconcile_envelope_locked(&mut inner);
        Ok(std::mem::take(&mut inner.expired_unobserved))
    }

    /// Counter that advances whenever the DEK leaves memory. Env-vault
    /// publication compares it before and after reading values to detect a
    /// lock that raced it.
    pub fn generation(&self) -> u64 {
        self.inner
            .lock()
            .map(|inner| inner.generation)
            // Poisoned: `state()` then reports locked, which aborts any
            // publication that re-checks it (fails closed).
            .unwrap_or(u64::MAX)
    }

    pub fn with_dek<T, E>(&self, f: impl FnOnce(&[u8; DEK_LEN]) -> Result<T, E>) -> Result<T, E>
    where
        E: From<UnlockError>,
    {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        self.expire_locked(&mut inner);
        self.reconcile_envelope_locked(&mut inner);
        if self.mode_locked(&mut inner) == VaultMode::EnvelopeMissing {
            return Err(UnlockError::EnvelopeMissing.into());
        }
        let dek = inner.dek.as_ref().ok_or(UnlockError::Locked)?;
        let value = f(&*dek)?;
        self.touch_locked(&mut inner);
        Ok(value)
    }

    fn touch_locked(&self, inner: &mut UnlockInner) {
        inner.last_activity = Some(Instant::now());
        inner.last_activity_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as u64);
    }

    /// Whether the envelope file is on disk. Takes (and releases) only the
    /// path mutex, so it is safe to call while holding `inner`.
    fn envelope_present(&self) -> bool {
        // The guarded value is a plain `Option<PathBuf>`; a panic elsewhere
        // cannot leave it half-written, so a poisoned guard is still read.
        let path = self
            .path
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        path.is_some_and(|path| path.is_file())
    }

    fn mode_locked(&self, inner: &mut UnlockInner) -> VaultMode {
        if self.envelope_present() {
            inner.configured_latched = true;
            return VaultMode::Configured;
        }
        if inner.configured_latched || inner.dek.is_some() {
            inner.configured_latched = true;
            return VaultMode::EnvelopeMissing;
        }
        VaultMode::Unconfigured
    }

    /// A held DEK whose envelope has vanished is dropped (zeroized) exactly
    /// like an idle expiry: the generation advances and the expiry is
    /// reported to `expire_if_idle`, so env-vault files get invalidated.
    fn reconcile_envelope_locked(&self, inner: &mut UnlockInner) {
        if inner.dek.is_none() || self.mode_locked(inner) != VaultMode::EnvelopeMissing {
            return;
        }
        log::warn!("[secrets] unlock envelope disappeared while unlocked; locking the vault");
        inner.dek = None;
        inner.expired_unobserved = true;
        inner.generation = inner.generation.wrapping_add(1);
        inner.last_activity = None;
        inner.last_activity_unix_ms = None;
    }

    fn expire_locked(&self, inner: &mut UnlockInner) {
        let Some(last_activity) = inner.last_activity else {
            return;
        };
        if last_activity.elapsed() < self.idle_timeout {
            return;
        }
        if inner.dek.take().is_some() {
            inner.expired_unobserved = true;
            inner.generation = inner.generation.wrapping_add(1);
        }
        inner.last_activity = None;
        inner.last_activity_unix_ms = None;
    }
}

impl Default for UnlockState {
    fn default() -> Self {
        Self::new()
    }
}

fn reject_linked_parent(path: &Path) -> Result<(), UnlockError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for ancestor in parent
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|error| UnlockError::Io(format!("inspect unlock parent: {error}")))?;
        if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(UnlockError::Io(
                "secrets unlock envelope has a linked parent".to_string(),
            ));
        }
    }
    Ok(())
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn read_envelope(path: &Path) -> Result<WrappedDekEnvelope, UnlockError> {
    reject_linked_parent(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(UnlockError::NotConfigured);
        }
        Err(error) => {
            return Err(UnlockError::Io(format!("inspect unlock envelope: {error}")));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(UnlockError::InvalidEnvelope("symlink".to_string()));
    }
    if !metadata.is_file() {
        return Err(UnlockError::InvalidEnvelope("not a file".to_string()));
    }
    let body = fs::read(path)
        .map_err(|error| UnlockError::Io(format!("read unlock envelope: {error}")))?;
    if body.len() > MAX_ENVELOPE_BYTES {
        return Err(UnlockError::InvalidEnvelope("size".to_string()));
    }
    WrappedDekEnvelope::from_slice(&body).map_err(|error| match error {
        CryptoError::UnsupportedVersion(version) => {
            UnlockError::InvalidEnvelope(format!("unsupported version {version}"))
        }
        CryptoError::UnsupportedKdf(kdf) => {
            UnlockError::InvalidEnvelope(format!("unsupported KDF {kdf}"))
        }
        other => UnlockError::InvalidEnvelope(other.to_string()),
    })
}

fn persist_envelope(path: &Path, envelope: &WrappedDekEnvelope) -> Result<(), UnlockError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(UnlockError::Io("unlock envelope is a symlink".to_string()));
        }
    }
    let body = envelope.to_json().map_err(UnlockError::Crypto)?;
    write_atomic(path, &body).map_err(UnlockError::Io)?;
    let persisted = read_envelope(path)?;
    if persisted != *envelope {
        return Err(UnlockError::Io(
            "unlock envelope verification failed after write".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(dir: &Path, timeout: Duration) -> UnlockState {
        UnlockState::with_path(dir.join(UNLOCK_ENVELOPE_FILENAME), timeout)
    }

    #[test]
    fn set_unlock_lock_and_rotation_preserve_the_dek() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        assert!(!unlock.is_configured());
        unlock.set_passphrase("first").unwrap();
        assert!(unlock.is_configured());
        assert!(unlock.is_unlocked());
        unlock.lock().unwrap();
        assert!(unlock.is_unlocked() == false);
        unlock.unlock("first").unwrap();
        assert!(unlock.is_unlocked());
        unlock.rotate("first", "second").unwrap();
        unlock.lock().unwrap();
        assert_eq!(
            unlock.unlock("first").unwrap_err(),
            UnlockError::WrongPassphrase
        );
        unlock.unlock("second").unwrap();
        assert!(unlock.is_unlocked());
    }

    #[test]
    fn idle_timeout_zeroizes_and_locks_the_dek() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_millis(1));
        unlock.set_passphrase("passphrase").unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(unlock.expire_if_idle().unwrap());
        assert!(!unlock.is_unlocked());
        assert!(unlock.with_dek(|_| Ok::<(), UnlockError>(())).is_err());
    }

    #[test]
    fn wrong_passphrase_does_not_unlock_or_replace_state() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        unlock.set_passphrase("right").unwrap();
        unlock.lock().unwrap();
        assert_eq!(
            unlock.unlock("wrong").unwrap_err(),
            UnlockError::WrongPassphrase
        );
        assert!(!unlock.is_unlocked());
        unlock.unlock("right").unwrap();
        assert!(unlock.is_unlocked());
    }

    #[test]
    fn state_reports_activity_and_configured_flag() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        let initial = unlock.state();
        assert!(!initial.configured);
        // No passphrase set: nothing to unlock (behaves exactly as WP-33).
        assert!(!initial.locked);
        unlock.set_passphrase("passphrase").unwrap();
        let active = unlock.state();
        assert!(active.configured);
        assert!(!active.locked);
        assert!(active.last_activity_unix_ms.is_some());
        unlock.lock().unwrap();
        let locked = unlock.state();
        assert!(locked.locked);
        assert!(locked.last_activity_unix_ms.is_none());
    }

    #[test]
    fn changing_a_configured_passphrase_requires_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        // First set needs no current passphrase.
        unlock.set_or_rotate("first", None).unwrap();
        assert!(unlock.is_configured());
        // Unlocked, but still refused without the current passphrase.
        assert!(unlock.is_unlocked());
        assert_eq!(
            unlock.set_or_rotate("second", None).unwrap_err(),
            UnlockError::CurrentPassphraseRequired
        );
        assert_eq!(
            unlock.set_or_rotate("second", Some("wrong")).unwrap_err(),
            UnlockError::WrongPassphrase
        );
        unlock.set_or_rotate("second", Some("first")).unwrap();
        unlock.lock().unwrap();
        assert_eq!(
            unlock.unlock("first").unwrap_err(),
            UnlockError::WrongPassphrase
        );
        unlock.unlock("second").unwrap();
    }

    #[test]
    fn generation_advances_on_lock_and_idle_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_millis(1));
        unlock.set_passphrase("passphrase").unwrap();
        let before = unlock.generation();
        unlock.lock().unwrap();
        assert_ne!(unlock.generation(), before);

        unlock.unlock("passphrase").unwrap();
        let before = unlock.generation();
        std::thread::sleep(Duration::from_millis(5));
        // The expiry happens as a side effect of `state()` ...
        assert!(unlock.state().locked);
        assert_ne!(unlock.generation(), before);
        // ... and is still reported to the idle loop exactly once.
        assert!(unlock.expire_if_idle().unwrap());
        assert!(!unlock.expire_if_idle().unwrap());
    }

    #[test]
    fn configured_is_sticky_when_the_envelope_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        unlock.set_passphrase("passphrase").unwrap();
        let before = unlock.generation();
        std::fs::remove_file(dir.path().join(UNLOCK_ENVELOPE_FILENAME)).unwrap();

        assert_eq!(unlock.mode(), VaultMode::EnvelopeMissing);
        assert!(unlock.is_configured());
        // The held DEK is dropped like an idle expiry and reported to the
        // idle loop, so the env-vault files get invalidated.
        assert!(unlock.expire_if_idle().unwrap());
        assert_ne!(unlock.generation(), before);
        let locked = unlock.state();
        assert!(locked.configured);
        assert!(locked.locked);
        assert_eq!(
            unlock
                .with_dek(|_| Ok::<(), UnlockError>(()))
                .unwrap_err(),
            UnlockError::EnvelopeMissing
        );

        // Never a fresh envelope over the lost one.
        assert_eq!(
            unlock.set_passphrase("fresh").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert_eq!(
            unlock.set_or_rotate("fresh", None).unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert_eq!(
            unlock.unlock("passphrase").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert!(!dir.path().join(UNLOCK_ENVELOPE_FILENAME).exists());
    }

    #[test]
    fn envelope_present_at_boot_latches_configured() {
        let dir = tempfile::tempdir().unwrap();
        state(dir.path(), Duration::from_secs(60))
            .set_passphrase("passphrase")
            .unwrap();
        // A new process configures its path at boot while the envelope exists.
        let booted = UnlockState::with_idle_timeout(Duration::from_secs(60));
        booted
            .configure_path(dir.path().join(UNLOCK_ENVELOPE_FILENAME))
            .unwrap();
        std::fs::remove_file(dir.path().join(UNLOCK_ENVELOPE_FILENAME)).unwrap();
        assert_eq!(booted.mode(), VaultMode::EnvelopeMissing);
        assert!(booted.state().locked);
        assert_eq!(
            booted.set_passphrase("fresh").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
    }

    #[test]
    fn mark_configured_latches_without_an_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let unlock = state(dir.path(), Duration::from_secs(60));
        assert_eq!(unlock.mode(), VaultMode::Unconfigured);
        assert!(!unlock.state().locked);
        unlock.mark_configured();
        assert_eq!(unlock.mode(), VaultMode::EnvelopeMissing);
        assert!(unlock.state().locked);
        assert_eq!(
            unlock.set_passphrase("fresh").unwrap_err(),
            UnlockError::EnvelopeMissing
        );
        assert!(!dir.path().join(UNLOCK_ENVELOPE_FILENAME).exists());
    }
}
