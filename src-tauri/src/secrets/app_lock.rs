//! App lock core — WP-72's state machine, shared by the desktop commands
//! (`commands/app_lock.rs`) and the headless daemon's `app_lock_*` arms
//! (remote-access WP-21; G-PRINCIPAL §5 row 16).
//!
//! Mounted as `crate::secrets_env::app_lock` so it compiles in the headless
//! build (`crate::commands` is desktop-only). The module header of
//! `commands/app_lock.rs` describes the behaviour; this file holds
//! everything that is not Tauri: the persisted config and lock record, the
//! idle / wrong-entry state machine, PIN hashing, and the private file
//! write. Each `AppLockCore` operation returns whether the state changed, so
//! the desktop can broadcast `app-lock://changed`.
//!
//! In a T1 principal child the core is rooted at the child's own
//! `<data>/app-lock.json` (owned by the principal's uid), so each principal
//! has their own PIN and lock — the "one PIN gates the whole app" problem
//! that kept these arms desktop-only. It stays what WP-72 says it is: a
//! privacy screen, not a security boundary. The daemon has no ticker; idle
//! expiry is evaluated on every `status` call, which the shell polls.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{self, Config, Variant, Version};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Lives in `app_data_dir`, beside the vault's unlock envelope. Mode 0600.
pub const CONFIG_FILENAME: &str = "app-lock.json";

pub const MIN_SECRET_CHARS: usize = 4;
pub const MAX_SECRET_CHARS: usize = 256;
pub const MIN_IDLE_MINUTES: u32 = 1;
pub const MAX_IDLE_MINUTES: u32 = 24 * 60;
pub const DEFAULT_IDLE_MINUTES: u32 = 15;
/// Wrong entries allowed before a wait. D-05's copy: "Two attempts left
/// before a 30 s wait."
pub const MAX_ATTEMPTS: u32 = 3;
/// The wait after each round of `MAX_ATTEMPTS` misses. The last step repeats.
pub const BACKOFF_STEPS_MS: [u64; 4] = [30_000, 60_000, 5 * 60_000, 15 * 60_000];

const SECRET_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const HASH_LEN: u32 = 32;
// Same argon2id cost as the vault envelope (`secrets/crypto.rs`).
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

/// Host and OS labels for [`AppLockStatus`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Platform {
    pub host: String,
    pub os: String,
}

impl Platform {
    /// The daemon's own labels: the kernel hostname and `std::env::consts`.
    pub fn headless() -> Self {
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .or_else(|| std::env::var("HOSTNAME").ok())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "this device".into());
        let os = match std::env::consts::OS {
            "linux" => "Linux",
            "macos" => "macOS",
            "windows" => "Windows",
            other => other,
        };
        Self {
            host,
            os: os.to_string(),
        }
    }
}

// ─── wire types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UnlockMethod {
    /// A PIN or passphrase, verified here against an argon2id hash.
    #[default]
    Pin,
    /// OS biometrics: Windows Hello or Touch ID. Unavailable on this build
    /// (see the module header).
    Os,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LockReason {
    Idle,
    Manual,
    Launch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiometricSupport {
    /// `windows-hello` | `touch-id` | `none`.
    pub kind: &'static str,
    /// What the UI calls it: "Windows Hello", "Touch ID", or "".
    pub label: &'static str,
    pub available: bool,
    /// Why it isn't available. Empty when it is.
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// OS hostname: the Profile tab's "OS user" row and the lock screen's meta
    /// line (`ned-desktop · locked after 15 min idle`).
    pub host: String,
    /// e.g. `Linux 6.8.0`, `Windows 10.0.22631`, `macOS 14.5.0`.
    pub os: String,
    /// Absolute path of `app-lock.json`: the recovery path shown in
    /// Profile › App lock (never on the lock screen itself).
    pub config_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub status: AppLockStatus,
}

// ─── persisted config ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecretRecord {
    version: u32,
    /// Hex.
    salt: String,
    /// Hex argon2id output.
    hash: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct AppLockConfig {
    idle_enabled: bool,
    idle_minutes: u32,
    method: UnlockMethod,
    secret: Option<SecretRecord>,
}

/// The lock itself, persisted beside the config (`"lock"` in `app-lock.json`)
/// so a relaunch can't clear it or the wrong-entry wait.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LockRecord {
    locked: bool,
    reason: Option<LockReason>,
    locked_at_ms: Option<u64>,
    failed_attempts: u32,
    retry_at_ms: Option<u64>,
    backoff_round: u32,
}

/// Read side of the file's `lock` record. `AppLockConfig` ignores the key.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LockFileIn {
    lock: LockRecord,
}

/// Write side: the config's keys, flattened, plus the `lock` record.
#[derive(Serialize)]
struct ConfigFileOut<'a> {
    #[serde(flatten)]
    config: &'a AppLockConfig,
    lock: LockRecord,
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

// ─── state ──────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct Inner {
    config: AppLockConfig,
    path: Option<PathBuf>,
    locked: bool,
    reason: Option<LockReason>,
    locked_at_ms: Option<u64>,
    last_activity_ms: u64,
    failed_attempts: u32,
    retry_at_ms: Option<u64>,
    /// How many full rounds of wrong entries have run since the last unlock.
    /// Picks the next wait from `BACKOFF_STEPS_MS`.
    backoff_round: u32,
    /// An unlock is being verified off-thread; a second one waits its turn.
    verifying: bool,
    /// What the status reports as host and OS (the desktop asks
    /// `tauri-plugin-os`; the daemon reads its own host).
    platform: Platform,
}

/// What `begin_unlock` decided before the (slow) hash check runs.
#[derive(Debug, PartialEq, Eq)]
enum UnlockGate {
    NotLocked,
    Wait(u64),
    Busy,
    /// Locked with no secret to check against (a hand-edited file). Unlocked
    /// on the spot: a lock nobody can open is worse than none.
    NoSecret,
    Check(SecretRecord),
}

impl Inner {
    fn load(&mut self, path: PathBuf, now: u64) {
        let (config, record) = match std::fs::read(&path) {
            Ok(bytes) => {
                let config = match serde_json::from_slice::<AppLockConfig>(&bytes) {
                    Ok(config) => sanitize(config),
                    Err(error) => {
                        log::warn!(
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
        // No PIN on record: nothing could unlock, so nothing restores.
        if self.config.secret.is_none() {
            return;
        }
        let longest_wait = BACKOFF_STEPS_MS[BACKOFF_STEPS_MS.len() - 1];
        self.failed_attempts = record.failed_attempts.min(MAX_ATTEMPTS - 1);
        // Capped, so a clock that jumped back can't park the lock for good.
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

    fn set_locked(&mut self, reason: LockReason, now: u64) {
        self.locked = true;
        self.reason = Some(reason);
        self.locked_at_ms = Some(now);
        self.persist();
    }

    fn set_unlocked(&mut self, now: u64) {
        self.locked = false;
        self.reason = None;
        self.locked_at_ms = None;
        self.failed_attempts = 0;
        self.retry_at_ms = None;
        self.backoff_round = 0;
        self.last_activity_ms = now;
        self.persist();
    }

    fn lock_record(&self) -> LockRecord {
        LockRecord {
            locked: self.locked,
            reason: self.reason,
            locked_at_ms: self.locked_at_ms,
            failed_attempts: self.failed_attempts,
            retry_at_ms: self.retry_at_ms,
            backoff_round: self.backoff_round,
        }
    }

    /// Write the lock record through. A failed write is logged, never fatal:
    /// the in-memory lock still holds for this run.
    fn persist(&self) {
        if self.path.is_none() {
            return;
        }
        if let Err(error) = self.save() {
            log::warn!("[app-lock] could not persist the lock state: {error}");
        }
    }

    fn touch(&mut self, now: u64) {
        if !self.locked {
            self.last_activity_ms = now;
        }
    }

    /// Lock if the idle window has passed. Returns true when this call locked.
    fn check_idle(&mut self, now: u64) -> bool {
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

    /// Lock now. `Ok(false)` if it was already locked.
    fn lock(&mut self, reason: LockReason, now: u64) -> Result<bool, String> {
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

    fn begin_unlock(&mut self, now: u64) -> UnlockGate {
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

    /// Apply a finished check. Returns the error line to show, if any.
    fn finish_unlock(&mut self, ok: bool, now: u64) -> Option<String> {
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

    fn status(&self, now: u64) -> AppLockStatus {
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
            host: self.platform.host.clone(),
            os: self.platform.os.clone(),
            config_path: self.path.as_ref().map(|p| p.display().to_string()),
        }
    }

    fn save(&self) -> Result<(), String> {
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

/// The wait after wrong-entry round `round` (0-based).
fn backoff_ms(round: u32) -> u64 {
    let index = (round as usize).min(BACKOFF_STEPS_MS.len() - 1);
    BACKOFF_STEPS_MS[index]
}

/// "30 s", "1 min", "15 min".
fn wait_label(ms: u64) -> String {
    if ms < 60_000 {
        format!("{} s", ms / 1000)
    } else {
        format!("{} min", ms / 60_000)
    }
}

/// Clamp a loaded config into range and drop combinations that can't work.
fn sanitize(mut config: AppLockConfig) -> AppLockConfig {
    config.idle_minutes = config
        .idle_minutes
        .clamp(MIN_IDLE_MINUTES, MAX_IDLE_MINUTES);
    if config.method == UnlockMethod::Os && !biometric_support().available {
        config.method = UnlockMethod::Pin;
    }
    if let Some(record) = config.secret.as_ref() {
        if !record_is_supported(record) {
            log::warn!("[app-lock] stored PIN record uses unsupported parameters; dropping it");
            config.secret = None;
        }
    }
    if config.secret.is_none() {
        config.idle_enabled = false;
    }
    config
}

/// One lock: the desktop's managed state, or a daemon's per-data-dir lock.
#[derive(Debug, Default)]
pub struct AppLockCore {
    inner: Mutex<Inner>,
}

impl AppLockCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the lock at `path` (`app-lock.json`) and load it. If idle lock
    /// is on, it starts locked.
    pub fn configure(&self, path: PathBuf, platform: Platform) {
        let now = now_ms();
        self.with(|inner| {
            inner.platform = platform;
            inner.load(path, now)
        });
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }

    /// The idle check alone. Returns true when this call locked.
    pub fn check_idle(&self) -> bool {
        self.with(|inner| inner.check_idle(now_ms()))
    }

    /// The current state, after the idle check. `.0` is true when the idle
    /// check locked just now.
    pub fn status(&self) -> (bool, AppLockStatus) {
        let now = now_ms();
        self.with(|inner| {
            let locked_now = inner.check_idle(now);
            (locked_now, inner.status(now))
        })
    }

    /// Record activity (ignored while locked).
    pub fn touch(&self) {
        let now = now_ms();
        self.with(|inner| inner.touch(now));
    }

    /// Lock now. Refused without a PIN. `.0`: whether it changed.
    pub fn lock_now(&self) -> Result<(bool, AppLockStatus), String> {
        let now = now_ms();
        self.with(|inner| {
            let changed = inner.lock(LockReason::Manual, now)?;
            Ok((changed, inner.status(now)))
        })
    }

    fn outcome(&self, ok: bool, error: Option<String>) -> UnlockOutcome {
        let status = self.with(|inner| inner.status(now_ms()));
        UnlockOutcome { ok, error, status }
    }

    /// Unlock with the PIN. A wrong entry is `Ok` with `ok: false`. `.1`:
    /// whether the state changed (unlocked).
    pub async fn unlock(&self, secret: Zeroizing<String>) -> Result<(UnlockOutcome, bool), String> {
        let now = now_ms();
        let gate = self.with(|inner| inner.begin_unlock(now));
        let record = match gate {
            UnlockGate::NotLocked => return Ok((self.outcome(true, None), false)),
            UnlockGate::Wait(ms) => {
                let secs = ms.div_ceil(1000);
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
                ))
            }
            UnlockGate::NoSecret => {
                log::warn!("[app-lock] locked with no PIN on record; unlocking");
                self.with(|inner| inner.set_unlocked(now_ms()));
                return Ok((self.outcome(true, None), true));
            }
            UnlockGate::Check(record) => record,
        };
        let verified = tokio::task::spawn_blocking(move || verify_secret(secret.as_str(), &record))
            .await
            .map_err(|e| format!("join: {e}"))
            .and_then(|result| result);
        let ok = match verified {
            Ok(ok) => ok,
            Err(error) => {
                // Leave it locked and let the next attempt run.
                self.with(|inner| inner.verifying = false);
                return Err(error);
            }
        };
        let error = self.with(|inner| inner.finish_unlock(ok, now_ms()));
        if ok {
            log::info!("[app-lock] unlocked (PIN)");
        }
        Ok((self.outcome(ok, error), ok))
    }

    /// OS biometrics: refused on every build (see `commands/app_lock.rs`).
    pub fn unlock_biometric(&self) -> UnlockOutcome {
        let support = biometric_support();
        let reason = if support.available {
            "biometric unlock is not wired on this build"
        } else {
            support.reason
        };
        self.outcome(false, Some(reason.to_string()))
    }

    /// Idle lock on/off, minutes, and unlock method.
    pub fn configure_lock(
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
            // A fresh window, so turning idle lock on never locks at once.
            inner.last_activity_ms = now;
            Ok(inner.status(now))
        })
    }

    /// Set or change the PIN. Changing one needs the current one.
    pub async fn set_secret(
        &self,
        current: Option<Zeroizing<String>>,
        next: Zeroizing<String>,
    ) -> Result<AppLockStatus, String> {
        validate_secret(next.as_str())?;
        let existing = self.with(|inner| {
            if inner.locked {
                return Err("Unlock first.".to_string());
            }
            Ok(inner.config.secret.clone())
        })?;
        let record = tokio::task::spawn_blocking(move || -> Result<SecretRecord, String> {
            if let Some(existing) = existing.as_ref() {
                let Some(current) = current.as_ref() else {
                    return Err("Enter the current PIN to change it.".into());
                };
                if !verify_secret(current.as_str(), existing)? {
                    return Err("The current PIN is wrong.".into());
                }
            }
            hash_secret(next.as_str())
        })
        .await
        .map_err(|e| format!("join: {e}"))??;
        let now = now_ms();
        self.with(|inner| {
            let previous = inner.config.clone();
            inner.config.secret = Some(record);
            if let Err(error) = inner.save() {
                inner.config = previous;
                return Err(error);
            }
            Ok(inner.status(now))
        })
    }

    /// Remove the PIN; that also turns idle lock off. `.0`: whether it
    /// changed (false when no PIN was set).
    pub async fn clear_secret(
        &self,
        current: Zeroizing<String>,
    ) -> Result<(bool, AppLockStatus), String> {
        let existing = self.with(|inner| {
            if inner.locked {
                return Err("Unlock first.".to_string());
            }
            Ok(inner.config.secret.clone())
        })?;
        let Some(existing) = existing else {
            return Ok((false, self.with(|inner| inner.status(now_ms()))));
        };
        let ok = tokio::task::spawn_blocking(move || verify_secret(current.as_str(), &existing))
            .await
            .map_err(|e| format!("join: {e}"))??;
        if !ok {
            return Err("The current PIN is wrong.".into());
        }
        let now = now_ms();
        self.with(|inner| {
            let previous = inner.config.clone();
            inner.config.secret = None;
            inner.config.idle_enabled = false;
            inner.config.method = UnlockMethod::Pin;
            if let Err(error) = inner.save() {
                inner.config = previous;
                return Err(error);
            }
            Ok((true, inner.status(now)))
        })
    }
}

// ─── platform ───────────────────────────────────────────────────────────────

/// What this build can offer. See the module header for why every arm is
/// unavailable today.
#[cfg(target_os = "windows")]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "windows-hello",
        label: "Windows Hello",
        available: false,
        reason: "Windows Hello isn't wired into this build yet. Unlock with your PIN.",
    }
}

#[cfg(target_os = "macos")]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "touch-id",
        label: "Touch ID",
        available: false,
        reason: "Touch ID isn't wired into this build yet. Unlock with your PIN.",
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn biometric_support() -> BiometricSupport {
    BiometricSupport {
        kind: "none",
        label: "",
        available: false,
        reason: "Linux has no in-app biometric prompt. Unlock with your PIN.",
    }
}

// ─── secret hashing ─────────────────────────────────────────────────────────

fn validate_secret(secret: &str) -> Result<(), String> {
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

fn record_is_supported(record: &SecretRecord) -> bool {
    record.version == SECRET_VERSION
        && record.memory_kib == ARGON2_MEMORY_KIB
        && record.iterations == ARGON2_ITERATIONS
        && record.parallelism == ARGON2_PARALLELISM
}

fn hash_secret(secret: &str) -> Result<SecretRecord, String> {
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

fn verify_secret(secret: &str, record: &SecretRecord) -> Result<bool, String> {
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

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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

/// 0600, written to a sibling temp file and renamed over the target.
fn write_private_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
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

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── tests (written, not run: DEC-50) ───────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
        // Activity while locked doesn't count.
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

        // Unlocking writes through too.
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

        // Idle lock off: starts unlocked.
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

    #[tokio::test]
    async fn the_core_round_trips_set_lock_unlock_clear() {
        let dir = tempfile::tempdir().unwrap();
        let core = AppLockCore::new();
        core.configure(dir.path().join(CONFIG_FILENAME), Platform::headless());
        let (_, status) = core.status();
        assert!(!status.locked && !status.secret_set);
        assert!(core.lock_now().is_err(), "no PIN, no lock");

        core.set_secret(None, Zeroizing::new("2468".into()))
            .await
            .unwrap();
        assert!(core
            .set_secret(None, Zeroizing::new("1357".into()))
            .await
            .unwrap_err()
            .contains("current PIN"));
        let (changed, status) = core.lock_now().unwrap();
        assert!(changed && status.locked);

        let (outcome, changed) = core.unlock(Zeroizing::new("0000".into())).await.unwrap();
        assert!(!outcome.ok && !changed);
        assert_eq!(outcome.status.attempts_left, MAX_ATTEMPTS - 1);
        let (outcome, changed) = core.unlock(Zeroizing::new("2468".into())).await.unwrap();
        assert!(outcome.ok && changed && !outcome.status.locked);

        assert!(!core.unlock_biometric().ok);
        let status = core.configure_lock(true, 5, UnlockMethod::Pin).unwrap();
        assert!(status.idle_enabled);
        let (changed, status) = core
            .clear_secret(Zeroizing::new("2468".into()))
            .await
            .unwrap();
        assert!(changed && !status.secret_set && !status.idle_enabled);

        // Persisted per data dir: a second core over the same file agrees.
        let again = AppLockCore::new();
        again.configure(dir.path().join(CONFIG_FILENAME), Platform::headless());
        assert!(!again.status().1.secret_set);
        assert!(!again.status().1.host.is_empty());
    }
}
