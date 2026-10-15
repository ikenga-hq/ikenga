//! App lock core — shared between desktop `#[tauri::command]`s and daemon RPC arms.
//!
//! A privacy screen over every shell window: locks on idle and manual "Lock now",
//! and unlocks with PIN or passphrase. Argon2id hashing, wrong-entry rate limiting
//! with exponential backoff, and persisted lock state in `app-lock.json`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::{self, Config, Variant, Version};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use zeroize::Zeroizing;

/// Broadcast to every webview/browser session when the lock state or its config changes.
pub const APP_LOCK_CHANGED_EVENT: &str = "app-lock://changed";
/// What a locked account answers to anything but status, unlock and health.
pub const LOCKED_ERROR: &str = "Account is locked. Unlock first.";
/// Lives in `data_dir`. Mode 0600.
pub const CONFIG_FILENAME: &str = "app-lock.json";

pub const MIN_SECRET_CHARS: usize = 4;
pub const MAX_SECRET_CHARS: usize = 256;
pub const MIN_IDLE_MINUTES: u32 = 1;
pub const MAX_IDLE_MINUTES: u32 = 24 * 60;
pub const DEFAULT_IDLE_MINUTES: u32 = 15;
/// Wrong entries allowed before a wait.
pub const MAX_ATTEMPTS: u32 = 3;
/// The wait after each round of `MAX_ATTEMPTS` misses. The last step repeats.
pub const BACKOFF_STEPS_MS: [u64; 4] = [30_000, 60_000, 5 * 60_000, 15 * 60_000];
pub const IDLE_TICK: Duration = Duration::from_secs(10);

const SECRET_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const HASH_LEN: u32 = 32;
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

// ─── wire types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UnlockMethod {
    /// A PIN or passphrase, verified against an argon2id hash.
    #[default]
    Pin,
    /// OS biometrics: Windows Hello or Touch ID. Unavailable on this build.
    Os,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockReason {
    Idle,
    Manual,
    Launch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BiometricSupport {
    /// `windows-hello` | `touch-id` | `none`.
    pub kind: std::borrow::Cow<'static, str>,
    /// What the UI calls it: "Windows Hello", "Touch ID", or "".
    pub label: std::borrow::Cow<'static, str>,
    pub available: bool,
    /// Why it isn't available. Empty when it is.
    pub reason: std::borrow::Cow<'static, str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLockStatus {
    pub locked: bool,
    pub reason: Option<LockReason>,
    pub locked_at_ms: Option<u64>,
    pub idle_enabled: bool,
    pub idle_minutes: u32,
    pub method: UnlockMethod,
    /// A PIN or passphrase is set. Without one, nothing can lock.
    pub secret_set: bool,
    pub biometric: BiometricSupport,
    /// Milliseconds left in the wrong-entry wait, if one is running.
    pub retry_in_ms: Option<u64>,
    pub attempts_left: u32,
    /// OS hostname.
    pub host: String,
    /// e.g. `Linux 6.8.0`, `Windows 10.0.22631`, `macOS 14.5.0`.
    pub os: String,
    /// Absolute path of `app-lock.json`.
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub status: AppLockStatus,
}

// ─── persisted config ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRecord {
    pub version: u32,
    pub salt: String,
    pub hash: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppLockConfig {
    pub idle_enabled: bool,
    pub idle_minutes: u32,
    pub method: UnlockMethod,
    pub secret: Option<SecretRecord>,
}

impl Default for AppLockConfig {
    fn default() -> Self {
        Self {
            idle_enabled: false,
            idle_minutes: DEFAULT_IDLE_MINUTES,
            method: UnlockMethod::Pin,
            secret: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LockRecord {
    pub locked: bool,
    pub reason: Option<LockReason>,
    pub locked_at_ms: Option<u64>,
    pub failed_attempts: u32,
    pub retry_at_ms: Option<u64>,
    pub backoff_round: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LockFileIn {
    lock: LockRecord,
}

#[derive(Serialize)]
struct ConfigFileOut<'a> {
    #[serde(flatten)]
    config: &'a AppLockConfig,
    lock: LockRecord,
}

// ─── state ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Inner {
    pub config: AppLockConfig,
    pub path: Option<PathBuf>,
    pub locked: bool,
    pub reason: Option<LockReason>,
    pub locked_at_ms: Option<u64>,
    pub last_activity_ms: u64,
    pub failed_attempts: u32,
    pub retry_at_ms: Option<u64>,
    pub backoff_round: u32,
    pub verifying: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnlockGate {
    NotLocked,
    Wait(u64),
    Busy,
    NoSecret,
    Check(SecretRecord),
}

impl Inner {
    pub fn load(&mut self, path: PathBuf, now: u64) {
        let (config, record) = match std::fs::read(&path) {
            Ok(bytes) => {
                let config = match serde_json::from_slice::<AppLockConfig>(&bytes) {
                    Ok(config) => sanitize(config),
                    Err(error) => {
                        tracing::warn!(
                            "[app-lock] {} is unreadable, using defaults: {error}",
                            path.display()
                        );
                        AppLockConfig::default()
                    }
                };
                let record = serde_json::from_slice::<LockFileIn>(&bytes)
                    .map(|file| file.lock)
                    .unwrap_or_default();
                (config, record)
            }
            Err(_) => (AppLockConfig::default(), LockRecord::default()),
        };
        self.config = config;
        self.path = Some(path);
        self.last_activity_ms = now;
        if self.config.secret.is_none() {
            return;
        }
        let longest_wait = BACKOFF_STEPS_MS[BACKOFF_STEPS_MS.len() - 1];
        self.failed_attempts = record.failed_attempts.min(MAX_ATTEMPTS - 1);
        self.retry_at_ms = record.retry_at_ms.map(|at| at.min(now + longest_wait));
        self.backoff_round = record.backoff_round;
        if record.locked {
            self.locked = true;
            self.reason = Some(record.reason.unwrap_or(LockReason::Launch));
            self.locked_at_ms = Some(record.locked_at_ms.unwrap_or(now));
        } else if self.config.idle_enabled {
            self.set_locked(LockReason::Launch, now);
        }
    }

    pub fn set_locked(&mut self, reason: LockReason, now: u64) {
        self.locked = true;
        self.reason = Some(reason);
        self.locked_at_ms = Some(now);
        self.persist();
    }

    pub fn set_unlocked(&mut self, now: u64) {
        self.locked = false;
        self.reason = None;
        self.locked_at_ms = None;
        self.failed_attempts = 0;
        self.retry_at_ms = None;
        self.backoff_round = 0;
        self.last_activity_ms = now;
        self.persist();
    }

    pub fn lock_record(&self) -> LockRecord {
        LockRecord {
            locked: self.locked,
            reason: self.reason,
            locked_at_ms: self.locked_at_ms,
            failed_attempts: self.failed_attempts,
            retry_at_ms: self.retry_at_ms,
            backoff_round: self.backoff_round,
        }
    }

    pub fn persist(&self) {
        if self.path.is_none() {
            return;
        }
        if let Err(error) = self.save() {
            tracing::warn!("[app-lock] could not persist the lock state: {error}");
        }
    }

    pub fn touch(&mut self, now: u64) {
        if !self.locked {
            self.last_activity_ms = now;
        }
    }

    pub fn check_idle(&mut self, now: u64) -> bool {
        if self.locked || !self.config.idle_enabled || self.config.secret.is_none() {
            return false;
        }
        let window = u64::from(self.config.idle_minutes) * 60_000;
        if now.saturating_sub(self.last_activity_ms) >= window {
            self.set_locked(LockReason::Idle, now);
            return true;
        }
        false
    }

    pub fn lock(&mut self, reason: LockReason, now: u64) -> Result<bool, String> {
        if self.config.secret.is_none() {
            return Err(
                "Set a PIN or passphrase first — otherwise nothing could unlock it.".into(),
            );
        }
        if self.locked {
            return Ok(false);
        }
        self.set_locked(reason, now);
        Ok(true)
    }

    pub fn begin_check(&mut self, now: u64) -> Result<Option<SecretRecord>, String> {
        if self.locked {
            return Err("Unlock first.".into());
        }
        if let Some(at) = self.retry_at_ms {
            if at > now {
                let secs = (at - now + 999) / 1000;
                return Err(format!("Too many wrong entries. Try again in {secs} s."));
            }
            self.retry_at_ms = None;
        }
        if self.verifying {
            return Err("Still checking the last entry.".into());
        }
        self.verifying = true;
        Ok(self.config.secret.clone())
    }

    pub fn finish_check(&mut self, ok: bool, now: u64) -> Option<String> {
        self.verifying = false;
        if ok {
            self.failed_attempts = 0;
            self.retry_at_ms = None;
            self.backoff_round = 0;
            self.persist();
            return None;
        }
        self.failed_attempts += 1;
        if self.failed_attempts >= MAX_ATTEMPTS {
            let wait = backoff_ms(self.backoff_round);
            self.failed_attempts = 0;
            self.backoff_round = self.backoff_round.saturating_add(1);
            self.retry_at_ms = Some(now + wait);
            self.persist();
            return Some(format!(
                "Wrong PIN. Wait {} before trying again.",
                wait_label(wait)
            ));
        }
        self.persist();
        let left = MAX_ATTEMPTS - self.failed_attempts;
        Some(format!(
            "Wrong PIN. {} before a {} wait.",
            match left {
                1 => "One attempt left".to_string(),
                2 => "Two attempts left".to_string(),
                n => format!("{n} attempts left"),
            },
            wait_label(backoff_ms(self.backoff_round))
        ))
    }

    pub fn begin_unlock(&mut self, now: u64) -> UnlockGate {
        if !self.locked {
            return UnlockGate::NotLocked;
        }
        if let Some(at) = self.retry_at_ms {
            if at > now {
                return UnlockGate::Wait(at - now);
            }
            self.retry_at_ms = None;
        }
        if self.verifying {
            return UnlockGate::Busy;
        }
        match self.config.secret.clone() {
            Some(record) => {
                self.verifying = true;
                UnlockGate::Check(record)
            }
            None => UnlockGate::NoSecret,
        }
    }

    pub fn finish_unlock(&mut self, ok: bool, now: u64) -> Option<String> {
        self.verifying = false;
        if ok {
            self.set_unlocked(now);
            return None;
        }
        self.failed_attempts += 1;
        if self.failed_attempts >= MAX_ATTEMPTS {
            let wait = backoff_ms(self.backoff_round);
            self.failed_attempts = 0;
            self.backoff_round = self.backoff_round.saturating_add(1);
            self.retry_at_ms = Some(now + wait);
            self.persist();
            return Some(format!(
                "Wrong PIN. Wait {} before trying again.",
                wait_label(wait)
            ));
        }
        self.persist();
        let left = MAX_ATTEMPTS - self.failed_attempts;
        Some(format!(
            "Wrong PIN. {} before a {} wait.",
            match left {
                1 => "One attempt left".to_string(),
                2 => "Two attempts left".to_string(),
                n => format!("{n} attempts left"),
            },
            wait_label(backoff_ms(self.backoff_round))
        ))
    }

    pub fn status(&self, now: u64) -> AppLockStatus {
        AppLockStatus {
            locked: self.locked,
            reason: self.reason,
            locked_at_ms: self.locked_at_ms,
            idle_enabled: self.config.idle_enabled,
            idle_minutes: self.config.idle_minutes,
            method: self.config.method,
            secret_set: self.config.secret.is_some(),
            biometric: biometric_support(),
            retry_in_ms: self.retry_at_ms.filter(|at| *at > now).map(|at| at - now),
            attempts_left: MAX_ATTEMPTS.saturating_sub(self.failed_attempts),
            host: host_name(),
            os: os_label(),
            config_path: self.path.as_ref().map(|p| p.display().to_string()),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.as_ref() else {
            return Err("app lock is not configured yet (no data directory)".into());
        };
        let file = ConfigFileOut {
            config: &self.config,
            lock: self.lock_record(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|e| format!("serialize: {e}"))?;
        write_private_atomic(path, &json).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

pub fn backoff_ms(round: u32) -> u64 {
    let index = (round as usize).min(BACKOFF_STEPS_MS.len() - 1);
    BACKOFF_STEPS_MS[index]
}

pub fn wait_label(ms: u64) -> String {
    if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else {
        format!("{} min", ms / 60_000)
    }
}

pub fn sanitize(mut config: AppLockConfig) -> AppLockConfig {
    config.idle_minutes = config
        .idle_minutes
        .clamp(MIN_IDLE_MINUTES, MAX_IDLE_MINUTES);
    if config.method == UnlockMethod::Os && !biometric_support().available {
        config.method = UnlockMethod::Pin;
    }
    if let Some(record) = config.secret.as_ref() {
        if !record_is_supported(record) {
            tracing::warn!("[app-lock] stored PIN record uses unsupported parameters; dropping it");
            config.secret = None;
        }
    }
    if config.secret.is_none() {
        config.idle_enabled = false;
    }
    config
}

/// Shared lock state.
///
/// Cheap to share: the state lives behind an `Arc`, so a PIN check can run on
/// its own task (see [`AppLockState::unlock`]) and finish even when the
/// request that started it is dropped.
#[derive(Debug, Default)]
pub struct AppLockState {
    core: Arc<Core>,
}

#[derive(Debug)]
struct Core {
    inner: Mutex<Inner>,
    lock_tx: watch::Sender<bool>,
    _rx: watch::Receiver<bool>,
    /// argon2 runs in flight right now, and the most there have ever been at
    /// once. The busy flag allows one; the tests assert it.
    checks_running: AtomicUsize,
    checks_peak: AtomicUsize,
}

impl Default for Core {
    fn default() -> Self {
        let (lock_tx, _rx) = watch::channel(false);
        Self {
            inner: Mutex::new(Inner::default()),
            lock_tx,
            _rx,
            checks_running: AtomicUsize::new(0),
            checks_peak: AtomicUsize::new(0),
        }
    }
}

impl Core {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_locked = guard.locked;
        let res = f(&mut *guard);
        let is_locked = guard.locked;
        if was_locked != is_locked {
            let _ = self.lock_tx.send(is_locked);
        }
        res
    }

    /// Run one argon2 job on the blocking pool, counted.
    async fn argon2<T: Send + 'static>(
        &self,
        job: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let now = self.checks_running.fetch_add(1, Ordering::SeqCst) + 1;
        self.checks_peak.fetch_max(now, Ordering::SeqCst);
        let res = tokio::task::spawn_blocking(job)
            .await
            .map_err(|e| format!("join: {e}"))
            .and_then(|r| r);
        self.checks_running.fetch_sub(1, Ordering::SeqCst);
        res
    }
}

/// Run a PIN check to completion on its own task.
///
/// The busy flag (`Inner::verifying`) is set by `begin_*` and cleared only by
/// this task, in `finish_*` (or on an error), so it stays set for exactly as
/// long as the argon2 run. A dropped request (client gone, request aborted)
/// detaches from the task rather than cancelling it: the check still
/// finishes, still counts as an attempt, and no second check can start while
/// the first one's 19 MiB argon2 is running. Clearing the flag on drop
/// instead let every aborted request start another argon2 (617 MB after 30
/// aborts) and could clear another caller's flag.
async fn run_owned<T: Send + 'static>(
    core: &Arc<Core>,
    job: impl FnOnce(Arc<Core>) -> futures_util::future::BoxFuture<'static, Result<T, String>>,
) -> Result<T, String> {
    let task = tokio::spawn(job(Arc::clone(core)));
    task.await.map_err(|e| format!("join: {e}"))?
}

impl AppLockState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn configure(&self, path: PathBuf) {
        let now = now_ms();
        self.with(|inner| inner.load(path, now));
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        self.core.with(f)
    }

    pub fn is_locked(&self) -> bool {
        self.with(|inner| inner.locked)
    }

    pub fn subscribe_lock(&self) -> watch::Receiver<bool> {
        self.core.lock_tx.subscribe()
    }

    pub async fn wait_unlocked(&self) {
        let mut rx = self.subscribe_lock();
        while *rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// The most argon2 runs this state has ever had in flight at once.
    pub fn peak_checks(&self) -> usize {
        self.core.checks_peak.load(Ordering::SeqCst)
    }

    /// argon2 runs in flight right now.
    pub fn running_checks(&self) -> usize {
        self.core.checks_running.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> AppLockStatus {
        let now = now_ms();
        self.with(|inner| inner.status(now))
    }

    /// Check whether idle timeout was exceeded. Returns (locked_now, status).
    pub fn check_idle(&self) -> (bool, AppLockStatus) {
        let now = now_ms();
        self.with(|inner| {
            let locked_now = inner.check_idle(now);
            (locked_now, inner.status(now))
        })
    }

    pub fn touch(&self) {
        let now = now_ms();
        self.with(|inner| inner.touch(now));
    }

    /// Lock now. Returns (changed, status).
    pub fn lock(&self, reason: LockReason) -> Result<(bool, AppLockStatus), String> {
        let now = now_ms();
        self.with(|inner| {
            let changed = inner.lock(reason, now)?;
            Ok((changed, inner.status(now)))
        })
    }

    /// Unlock with PIN / passphrase.
    /// Returns (outcome, changed). `changed` is true if the lock unlocked.
    pub async fn unlock(&self, secret: String) -> Result<(UnlockOutcome, bool), String> {
        let secret = Zeroizing::new(secret);
        let now = now_ms();
        let gate = self.with(|inner| inner.begin_unlock(now));
        let record = match gate {
            UnlockGate::NotLocked => {
                return Ok((self.outcome(true, None), false));
            }
            UnlockGate::Wait(ms) => {
                let secs = (ms + 999) / 1000;
                return Ok((
                    self.outcome(
                        false,
                        Some(format!("Too many wrong entries. Try again in {secs} s.")),
                    ),
                    false,
                ));
            }
            UnlockGate::Busy => {
                return Ok((
                    self.outcome(false, Some("Still checking the last entry.".into())),
                    false,
                ));
            }
            UnlockGate::NoSecret => {
                tracing::warn!("[app-lock] locked with no PIN on record; unlocking");
                self.with(|inner| inner.set_unlocked(now_ms()));
                return Ok((self.outcome(true, None), true));
            }
            UnlockGate::Check(record) => record,
        };

        let (ok, error) = run_owned(&self.core, move |core| {
            Box::pin(async move {
                let verified = core
                    .argon2(move || verify_secret(secret.as_str(), &record))
                    .await;
                match verified {
                    Ok(ok) => Ok((ok, core.with(|inner| inner.finish_unlock(ok, now_ms())))),
                    Err(error) => {
                        core.with(|inner| inner.verifying = false);
                        Err(error)
                    }
                }
            })
        })
        .await?;
        Ok((self.outcome(ok, error), ok))
    }

    /// Configure idle lock and method.
    pub fn configure_settings(
        &self,
        idle_enabled: bool,
        idle_minutes: u32,
        method: UnlockMethod,
    ) -> Result<AppLockStatus, String> {
        if !(MIN_IDLE_MINUTES..=MAX_IDLE_MINUTES).contains(&idle_minutes) {
            return Err(format!(
                "Idle minutes must be between {MIN_IDLE_MINUTES} and {MAX_IDLE_MINUTES}."
            ));
        }
        if method == UnlockMethod::Os && !biometric_support().available {
            return Err(biometric_support().reason.to_string());
        }
        let now = now_ms();
        self.with(|inner| {
            if inner.locked {
                return Err("Unlock first.".to_string());
            }
            if idle_enabled && inner.config.secret.is_none() {
                return Err("Set a PIN or passphrase before turning on idle lock.".to_string());
            }
            let previous = inner.config.clone();
            inner.config.idle_enabled = idle_enabled;
            inner.config.idle_minutes = idle_minutes;
            inner.config.method = method;
            if let Err(error) = inner.save() {
                inner.config = previous;
                return Err(error);
            }
            inner.last_activity_ms = now;
            Ok(inner.status(now))
        })
    }

    /// Set or change the PIN or passphrase.
    ///
    /// The busy flag stays set through both argon2 runs (checking the current
    /// PIN, hashing the new one), so one call never has two in flight.
    pub async fn set_secret(
        &self,
        current: Option<String>,
        next: String,
    ) -> Result<AppLockStatus, String> {
        let current = current.map(Zeroizing::new);
        let next = Zeroizing::new(next);
        validate_secret(next.as_str())?;
        let now = now_ms();
        let existing = self.with(|inner| inner.begin_check(now))?;
        if existing.is_some() && current.is_none() {
            self.with(|inner| inner.verifying = false);
            return Err("Enter the current PIN to change it.".into());
        }
        run_owned(&self.core, move |core| {
            Box::pin(async move {
                let verified = match existing {
                    Some(existing) => {
                        let current = current.unwrap_or_default();
                        core.argon2(move || verify_secret(current.as_str(), &existing))
                            .await
                    }
                    None => Ok(true),
                };
                let verified = match verified {
                    Ok(ok) => ok,
                    Err(error) => {
                        core.with(|inner| inner.verifying = false);
                        return Err(error);
                    }
                };
                if !verified {
                    let error = core.with(|inner| inner.finish_check(false, now_ms()));
                    return Err(error.unwrap_or_else(|| "The current PIN is wrong.".into()));
                }
                let hashed = core.argon2(move || hash_secret(next.as_str())).await;
                core.with(|inner| {
                    inner.finish_check(true, now_ms());
                    let record = hashed?;
                    let previous = inner.config.clone();
                    inner.config.secret = Some(record);
                    if let Err(error) = inner.save() {
                        inner.config = previous;
                        return Err(error);
                    }
                    Ok(inner.status(now_ms()))
                })
            })
        })
        .await
    }

    /// Remove the PIN. Also turns idle lock off.
    pub async fn clear_secret(&self, current: String) -> Result<AppLockStatus, String> {
        let current = Zeroizing::new(current);
        let now = now_ms();
        let existing = self.with(|inner| inner.begin_check(now))?;
        let Some(existing) = existing else {
            self.with(|inner| inner.verifying = false);
            return Ok(self.with(|inner| inner.status(now_ms())));
        };
        run_owned(&self.core, move |core| {
            Box::pin(async move {
                let verified = core
                    .argon2(move || verify_secret(current.as_str(), &existing))
                    .await;
                let ok = match verified {
                    Ok(ok) => ok,
                    Err(error) => {
                        core.with(|inner| inner.verifying = false);
                        return Err(error);
                    }
                };
                core.with(|inner| {
                    let error = inner.finish_check(ok, now_ms());
                    if !ok {
                        return Err(error.unwrap_or_else(|| "The current PIN is wrong.".into()));
                    }
                    let previous = inner.config.clone();
                    inner.config.secret = None;
                    inner.config.idle_enabled = false;
                    inner.config.method = UnlockMethod::Pin;
                    if let Err(error) = inner.save() {
                        inner.config = previous;
                        return Err(error);
                    }
                    Ok(inner.status(now_ms()))
                })
            })
        })
        .await
    }

    pub fn outcome(&self, ok: bool, error: Option<String>) -> UnlockOutcome {
        let status = self.with(|inner| inner.status(now_ms()));
        UnlockOutcome { ok, error, status }
    }
}

// ─── platform ───────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "windows-hello".into(),
        label: "Windows Hello".into(),
        available: false,
        reason: "Windows Hello isn't wired into this build yet. Unlock with your PIN.".into(),
    }
}

#[cfg(target_os = "macos")]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "touch-id".into(),
        label: "Touch ID".into(),
        available: false,
        reason: "Touch ID isn't wired into this build yet. Unlock with your PIN.".into(),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "none".into(),
        label: "".into(),
        available: false,
        reason: "Linux has no in-app biometric prompt. Unlock with your PIN.".into(),
    }
}

pub fn host_name() -> String {
    let name = tauri_plugin_os::hostname();
    if name.trim().is_empty() {
        "this device".into()
    } else {
        name
    }
}

pub fn os_label() -> String {
    let name = match tauri_plugin_os::platform() {
        "windows" => "Windows",
        "macos" => "macOS",
        "linux" => "Linux",
        other => other,
    };
    let version = tauri_plugin_os::version().to_string();
    if version.is_empty() || version.eq_ignore_ascii_case("unknown") {
        name.to_string()
    } else {
        format!("{name} {version}")
    }
}

// ─── secret hashing ─────────────────────────────────────────────────────────

pub fn validate_secret(secret: &str) -> Result<(), String> {
    let chars = secret.chars().count();
    if chars < MIN_SECRET_CHARS {
        return Err(format!("Use at least {MIN_SECRET_CHARS} characters."));
    }
    if chars > MAX_SECRET_CHARS {
        return Err(format!("Use at most {MAX_SECRET_CHARS} characters."));
    }
    if secret.trim().is_empty() {
        return Err("A PIN can't be only spaces.".into());
    }
    Ok(())
}

fn argon2_config() -> Config<'static> {
    Config {
        ad: &[],
        hash_length: HASH_LEN,
        lanes: ARGON2_PARALLELISM,
        mem_cost: ARGON2_MEMORY_KIB,
        secret: &[],
        time_cost: ARGON2_ITERATIONS,
        variant: Variant::Argon2id,
        version: Version::Version13,
    }
}

pub fn record_is_supported(record: &SecretRecord) -> bool {
    record.version == SECRET_VERSION
        && record.memory_kib == ARGON2_MEMORY_KIB
        && record.iterations == ARGON2_ITERATIONS
        && record.parallelism == ARGON2_PARALLELISM
}

pub fn hash_secret(secret: &str) -> Result<SecretRecord, String> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let hash = argon2::hash_raw(secret.as_bytes(), &salt, &argon2_config())
        .map_err(|e| format!("hash: {e}"))?;
    Ok(SecretRecord {
        version: SECRET_VERSION,
        salt: hex::encode(salt),
        hash: hex::encode(hash),
        memory_kib: ARGON2_MEMORY_KIB,
        iterations: ARGON2_ITERATIONS,
        parallelism: ARGON2_PARALLELISM,
    })
}

pub fn verify_secret(secret: &str, record: &SecretRecord) -> Result<bool, String> {
    if !record_is_supported(record) {
        return Err("the stored PIN uses parameters this build doesn't know".into());
    }
    let salt = hex::decode(&record.salt).map_err(|e| format!("salt: {e}"))?;
    let expected = hex::decode(&record.hash).map_err(|e| format!("hash: {e}"))?;
    let actual = Zeroizing::new(
        argon2::hash_raw(secret.as_bytes(), &salt, &argon2_config())
            .map_err(|e| format!("hash: {e}"))?,
    );
    Ok(constant_time_eq(&actual, &expected))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ─── io ─────────────────────────────────────────────────────────────────────

pub fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn inner_with_secret(dir: &Path, idle_enabled: bool) -> Inner {
        let mut inner = Inner {
            path: Some(dir.join(CONFIG_FILENAME)),
            ..Inner::default()
        };
        inner.config.secret = Some(hash_secret("1234").unwrap());
        inner.config.idle_enabled = idle_enabled;
        inner.config.idle_minutes = 15;
        inner
    }

    #[test]
    fn hash_and_verify_round_trip() {
        let record = hash_secret("correct horse").unwrap();
        assert!(verify_secret("correct horse", &record).unwrap());
        assert!(!verify_secret("wrong horse", &record).unwrap());
        assert_eq!(record.salt.len(), SALT_LEN * 2);
    }

    #[test]
    fn secrets_are_validated() {
        assert!(validate_secret("123").is_err());
        assert!(validate_secret("    ").is_err());
        assert!(validate_secret("1234").is_ok());
        assert!(validate_secret(&"x".repeat(MAX_SECRET_CHARS + 1)).is_err());
    }

    #[test]
    fn cannot_lock_without_a_secret() {
        let mut inner = Inner::default();
        assert!(inner.lock(LockReason::Manual, 1_000).is_err());
        assert!(!inner.locked);
    }

    #[test]
    fn lock_now_locks_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), false);
        assert_eq!(inner.lock(LockReason::Manual, 1_000), Ok(true));
        assert_eq!(inner.lock(LockReason::Manual, 2_000), Ok(false));
        assert_eq!(inner.reason, Some(LockReason::Manual));
        assert_eq!(inner.locked_at_ms, Some(1_000));
    }

    #[test]
    fn idle_locks_after_the_window_and_touch_resets_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), true);
        inner.last_activity_ms = 0;
        let window = 15 * 60_000;
        assert!(!inner.check_idle(window - 1));
        inner.touch(window - 1);
        assert!(!inner.check_idle(window + 10));
        assert!(inner.check_idle(2 * window));
        assert_eq!(inner.reason, Some(LockReason::Idle));
        let locked_at = inner.last_activity_ms;
        inner.touch(3 * window);
        assert_eq!(inner.last_activity_ms, locked_at);
    }

    #[test]
    fn idle_does_nothing_when_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), false);
        assert!(!inner.check_idle(u64::MAX / 2));
    }

    #[test]
    fn wrong_entries_count_down_then_wait() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), false);
        inner.lock(LockReason::Manual, 0).unwrap();

        assert!(matches!(inner.begin_unlock(10), UnlockGate::Check(_)));
        assert!(inner
            .finish_unlock(false, 10)
            .unwrap()
            .contains("Two attempts left"));
        assert!(matches!(inner.begin_unlock(20), UnlockGate::Check(_)));
        assert!(inner
            .finish_unlock(false, 20)
            .unwrap()
            .contains("One attempt left"));
        assert!(matches!(inner.begin_unlock(30), UnlockGate::Check(_)));
        assert!(inner
            .finish_unlock(false, 30)
            .unwrap()
            .contains("Wait 30 s"));

        let first = BACKOFF_STEPS_MS[0];
        assert_eq!(inner.begin_unlock(40), UnlockGate::Wait(first - 10));
        assert!(matches!(
            inner.begin_unlock(30 + first),
            UnlockGate::Check(_)
        ));
        assert_eq!(inner.finish_unlock(true, 30 + first), None);
        assert!(!inner.locked);
        assert_eq!(inner.failed_attempts, 0);
        assert_eq!(inner.backoff_round, 0);
    }

    #[test]
    fn the_wait_grows_each_round() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), false);
        inner.lock(LockReason::Manual, 0).unwrap();
        let mut now = 0;
        for (round, step) in [30_000u64, 60_000, 300_000, 900_000, 900_000]
            .iter()
            .enumerate()
        {
            let mut last = None;
            for _ in 0..MAX_ATTEMPTS {
                assert!(
                    matches!(inner.begin_unlock(now), UnlockGate::Check(_)),
                    "round {round}"
                );
                last = inner.finish_unlock(false, now);
            }
            assert_eq!(inner.retry_at_ms, Some(now + step), "round {round}");
            assert!(last.unwrap().starts_with("Wrong PIN. Wait"));
            now += step;
        }
        assert!(matches!(inner.begin_unlock(now), UnlockGate::Check(_)));
        inner.finish_unlock(true, now);
        assert_eq!(inner.backoff_round, 0);
    }

    #[test]
    fn a_manual_lock_survives_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILENAME);
        let mut inner = inner_with_secret(dir.path(), false);
        inner.save().unwrap();
        inner.lock(LockReason::Manual, 1_000).unwrap();

        let mut relaunched = Inner::default();
        relaunched.load(path.clone(), 9_000);
        assert!(
            relaunched.locked,
            "idle lock is off, but the manual lock was persisted"
        );
        assert_eq!(relaunched.reason, Some(LockReason::Manual));
        assert_eq!(relaunched.locked_at_ms, Some(1_000));

        assert!(matches!(
            relaunched.begin_unlock(9_500),
            UnlockGate::Check(_)
        ));
        relaunched.finish_unlock(true, 9_500);
        let mut again = Inner::default();
        again.load(path, 10_000);
        assert!(!again.locked);
    }

    #[test]
    fn the_wrong_entry_wait_survives_a_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILENAME);
        let mut inner = inner_with_secret(dir.path(), false);
        inner.save().unwrap();
        inner.lock(LockReason::Manual, 0).unwrap();
        for _ in 0..MAX_ATTEMPTS {
            assert!(matches!(inner.begin_unlock(100), UnlockGate::Check(_)));
            inner.finish_unlock(false, 100);
        }

        let mut relaunched = Inner::default();
        relaunched.load(path, 200);
        assert!(relaunched.locked);
        assert_eq!(relaunched.backoff_round, 1);
        assert_eq!(
            relaunched.begin_unlock(200),
            UnlockGate::Wait(100 + BACKOFF_STEPS_MS[0] - 200)
        );
    }

    #[test]
    fn a_second_check_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut inner = inner_with_secret(dir.path(), false);
        inner.lock(LockReason::Manual, 0).unwrap();
        assert!(matches!(inner.begin_unlock(1), UnlockGate::Check(_)));
        assert_eq!(inner.begin_unlock(2), UnlockGate::Busy);
    }

    #[test]
    fn launch_locks_when_idle_lock_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILENAME);
        let seeded = inner_with_secret(dir.path(), true);
        seeded.save().unwrap();

        let mut inner = Inner::default();
        inner.load(path.clone(), 5_000);
        assert!(inner.locked);
        assert_eq!(inner.reason, Some(LockReason::Launch));

        let mut off = inner_with_secret(dir.path(), false);
        off.path = Some(path.clone());
        off.save().unwrap();
        let mut inner = Inner::default();
        inner.load(path, 5_000);
        assert!(!inner.locked);
    }

    #[test]
    fn sanitize_drops_impossible_combinations() {
        let config = sanitize(AppLockConfig {
            idle_enabled: true,
            idle_minutes: 0,
            method: UnlockMethod::Os,
            secret: None,
        });
        assert!(!config.idle_enabled, "no secret, no idle lock");
        assert_eq!(config.idle_minutes, MIN_IDLE_MINUTES);
        assert_eq!(
            config.method,
            UnlockMethod::Pin,
            "biometrics are unavailable on this build"
        );
    }

    #[test]
    fn status_never_carries_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let inner = inner_with_secret(dir.path(), true);
        let json = serde_json::to_string(&inner.status(0)).unwrap();
        let hash = &inner.config.secret.as_ref().unwrap().hash;
        assert!(!json.contains(hash.as_str()));
        assert!(json.contains("\"secretSet\":true"));
    }

    async fn locked_state(dir: &Path) -> AppLockState {
        let state = AppLockState::new();
        state.configure(dir.join(CONFIG_FILENAME));
        state.set_secret(None, "1234".into()).await.unwrap();
        state.lock(LockReason::Manual).unwrap();
        state
    }

    /// R4-6a / round-5 item 2: an aborted unlock must not let the next one
    /// start a second argon2 while the first is still running. 30 aborts in a
    /// row keep at most one check in flight, and every aborted check still
    /// finishes and counts as an attempt.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn aborted_unlocks_never_run_two_checks_at_once_and_still_count() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(locked_state(dir.path()).await);
        let peak_before = state.peak_checks();
        assert_eq!(peak_before, 1, "set_secret ran one argon2 at a time");

        for i in 0..30 {
            let s = Arc::clone(&state);
            let h = tokio::spawn(async move { s.unlock(format!("wrong{i}")).await });
            tokio::time::sleep(Duration::from_millis(3)).await;
            h.abort();
            let _ = h.await;
        }
        // Let whatever is still running finish.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while state.running_checks() > 0 || state.with(|i| i.verifying) {
            assert!(std::time::Instant::now() < deadline, "checks never settled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            state.peak_checks(),
            1,
            "an aborted unlock let a second argon2 start while the first ran"
        );
        let status = state.status();
        assert!(status.locked);
        assert!(
            status.attempts_left < MAX_ATTEMPTS || status.retry_in_ms.is_some(),
            "aborted checks still count as attempts: {status:?}"
        );
    }

    /// The busy flag belongs to the check that set it: while one check runs,
    /// every other caller is told "Still checking" and none of them clears
    /// the flag, so no extra check slips in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_busy_flag_is_never_cleared_by_another_caller() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppLockState::new());
        state.configure(dir.path().join(CONFIG_FILENAME));
        state.set_secret(None, "1234".into()).await.unwrap();

        // A long call: verify the current PIN, then hash the new one.
        let s = Arc::clone(&state);
        let long =
            tokio::spawn(async move { s.set_secret(Some("1234".into()), "5678".into()).await });
        while !state.with(|i| i.verifying) {
            tokio::task::yield_now().await;
        }
        let mut busy = 0;
        while !long.is_finished() {
            let s = Arc::clone(&state);
            let other = tokio::spawn(async move { s.clear_secret("0000".into()).await });
            tokio::time::sleep(Duration::from_millis(1)).await;
            other.abort();
            match other.await {
                Ok(Err(e)) if e.contains("Still checking") => busy += 1,
                Ok(Err(e)) if e.contains("Wrong PIN") => panic!("a second check ran: {e}"),
                _ => {}
            }
        }
        assert!(long.await.unwrap().is_ok());
        assert!(busy > 0, "no caller was ever told the check was busy");
        assert_eq!(state.peak_checks(), 1);
        assert_eq!(
            state.status().attempts_left,
            MAX_ATTEMPTS,
            "no wrong check ran"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let inner = inner_with_secret(dir.path(), false);
        inner.save().unwrap();
        let mode = std::fs::metadata(dir.path().join(CONFIG_FILENAME))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
