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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockError {
    Locked,
    NotConfigured,
    AlreadyConfigured,
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
        let mut guard = self.path.lock().map_err(|_| UnlockError::Poisoned)?;
        match guard.as_ref() {
            Some(existing) if existing != &path => Err(UnlockError::Io(
                "secrets unlock path is already configured".to_string(),
            )),
            _ => {
                *guard = Some(path);
                Ok(())
            }
        }
    }

    pub fn envelope_path(&self) -> Result<PathBuf, UnlockError> {
        self.path
            .lock()
            .map_err(|_| UnlockError::Poisoned)?
            .clone()
            .ok_or(UnlockError::NotConfigured)
    }

    pub fn is_configured(&self) -> bool {
        self.path
            .lock()
            .ok()
            .and_then(|path| path.as_ref().map(|path| path.is_file()))
            .unwrap_or(false)
    }

    pub fn is_unlocked(&self) -> bool {
        self.state().is_unlocked()
    }

    pub fn state(&self) -> LockState {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(_) => {
                return LockState {
                    configured: self.is_configured(),
                    locked: true,
                    idle_timeout_secs: self.idle_timeout.as_secs(),
                    last_activity_unix_ms: None,
                }
            }
        };
        self.expire_locked(&mut inner);
        LockState {
            configured: self.is_configured(),
            locked: inner.dek.is_none(),
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
            return Err(UnlockError::AlreadyConfigured);
        }
        let (envelope, dek) = crypto::create_envelope(passphrase)?;
        persist_envelope(&path, &envelope)?;
        inner.dek = Some(dek);
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn rotate(&self, old_passphrase: &str, new_passphrase: &str) -> Result<(), UnlockError> {
        if new_passphrase.is_empty() {
            return Err(UnlockError::InvalidPassphrase);
        }
        let path = self.envelope_path()?;
        let envelope = read_envelope(&path)?;
        let dek = crypto::unwrap_dek(old_passphrase, &envelope)?;
        let next = crypto::wrap_dek(new_passphrase, &dek)?;
        persist_envelope(&path, &next)?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        inner.dek = Some(dek);
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn set_or_rotate(
        &self,
        passphrase: &str,
        current_passphrase: Option<&str>,
    ) -> Result<(), UnlockError> {
        if !self.is_configured() {
            return self.set_passphrase(passphrase);
        }
        if let Some(current) = current_passphrase {
            return self.rotate(current, passphrase);
        }
        let path = self.envelope_path()?;
        let dek = {
            let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
            self.expire_locked(&mut inner);
            inner
                .dek
                .as_ref()
                .map(|dek| Zeroizing::new(**dek))
                .ok_or(UnlockError::Locked)?
        };
        let envelope = crypto::wrap_dek(passphrase, &dek)?;
        persist_envelope(&path, &envelope)?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        inner.dek = Some(dek);
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn unlock(&self, passphrase: &str) -> Result<(), UnlockError> {
        let path = self.envelope_path()?;
        let envelope = read_envelope(&path)?;
        let dek = crypto::unwrap_dek(passphrase, &envelope)?;
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        inner.dek = Some(dek);
        self.touch_locked(&mut inner);
        Ok(())
    }

    pub fn lock(&self) -> Result<bool, UnlockError> {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        let was_unlocked = inner.dek.take().is_some();
        inner.last_activity = None;
        inner.last_activity_unix_ms = None;
        Ok(was_unlocked)
    }

    pub fn expire_if_idle(&self) -> Result<bool, UnlockError> {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        let was_unlocked = inner.dek.is_some();
        self.expire_locked(&mut inner);
        Ok(was_unlocked && inner.dek.is_none())
    }

    pub fn with_dek<T, E>(&self, f: impl FnOnce(&[u8; DEK_LEN]) -> Result<T, E>) -> Result<T, E>
    where
        E: From<UnlockError>,
    {
        let mut inner = self.inner.lock().map_err(|_| UnlockError::Poisoned)?;
        self.expire_locked(&mut inner);
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

    fn expire_locked(&self, inner: &mut UnlockInner) {
        let Some(last_activity) = inner.last_activity else {
            return;
        };
        if last_activity.elapsed() < self.idle_timeout {
            return;
        }
        inner.dek.take();
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
        assert!(initial.locked);
        unlock.set_passphrase("passphrase").unwrap();
        let active = unlock.state();
        assert!(active.configured);
        assert!(!active.locked);
        assert!(active.last_activity_unix_ms.is_some());
        unlock.lock().unwrap();
        assert!(unlock.state().last_activity_unix_ms.is_none());
    }
}
