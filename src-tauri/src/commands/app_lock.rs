//! App lock — WP-72 (D-05 `locked`, `designs/people.html?state=locked`).
//!
//! A privacy screen over every shell window: it locks on idle and on
//! **Lock now**, and it unlocks with the configured method. Sessions, runs
//! and the daemon keep going underneath. It is **not** the vault lock
//! (`commands/secrets.rs` owns that) and it is **not** a security boundary:
//! anyone with the OS account can delete `app-lock.json` and relaunch. It
//! exists for the walk-up case: an unattended desktop, or a shell that is also
//! reachable remotely (Round 45: the case for app lock rests on remote and
//! multi-device use, not IDE parity, since VS Code and JetBrains have none).
//!
//! Rust owns the lock state, not the webview, so:
//! - a webview reload (⌘R, a crash, HMR) can't unlock it;
//! - every window (main and `detached-*`) shows the same state;
//! - the idle clock counts activity from any window. The frontend reports
//!   activity with `app_lock_touch`, throttled, and the ticker below locks
//!   once the configured minutes pass with none.
//!
//! When idle lock is on and a PIN is set, the app also starts locked
//! (`LockReason::Launch`). Without that, quit and relaunch would get past the
//! lock in three seconds.
//!
//! ## Per-OS biometric path — the WP-72 decision
//!
//! **This build unlocks with a PIN or passphrase on every OS. OS biometrics
//! are reported as unavailable, with the reason.** The per-OS paths below
//! each need a crate that isn't a direct dependency today. WP-72 runs under
//! DEC-50 (no build, no cargo) and may not add crates, so nothing could prove
//! a new FFI binding compiles or prompts. The shape is kept so a follow-up
//! only fills in `biometric_support()` and `verify_biometric()`:
//!
//! - **Windows: Windows Hello.** Use
//!   `Windows.Security.Credentials.UI.UserConsentVerifier`
//!   (`CheckAvailabilityAsync`, then `RequestVerificationAsync`). It needs the
//!   `windows` crate with the `Security_Credentials_UI` feature. `windows` is
//!   only in the tree transitively, through webview2-com, without that
//!   feature. On Windows the consent dialog is system-modal and owned by the
//!   OS, so the lock overlay must not re-grab focus while it is up (below).
//! - **macOS: Touch ID.** Use `LAContext.evaluatePolicy(
//!   .deviceOwnerAuthenticationWithBiometrics)` through
//!   `objc2-local-authentication` and `block2`. `objc2 0.5` is already a
//!   direct dependency; the other two aren't. LAContext is preferred over a
//!   Keychain item with a biometric ACL because that needs signing
//!   entitlements, and these builds are unsigned (README).
//! - **Linux: PIN only, for good.** No desktop has an in-app biometric
//!   prompt. fprintd is D-Bus/PAM only (research P7-53..55).
//! - **Rejected:**
//!   - `tauri-plugin-biometric` is mobile-only, by a Cargo `cfg` gate;
//!   - `tauri-plugin-biometry` (community) uses WebAuthn `hmac-secret` on
//!     Windows, which needs Windows 11 and WebAuthn API 8 or later, and
//!     Keychain on macOS, which needs signing (P7-48).
//!
//! ## Focus (the 1Password bug, research P7-29)
//!
//! 1Password's own lock window takes focus from the OS Touch ID sheet and
//! blocks it. The rule here is that **the shell never calls `set_focus` for
//! the lock**. Locking does not raise, show or focus any window. The
//! overlay (`src/shell/people/app-lock-overlay.tsx`) only moves DOM focus
//! back to its own field when the window regains focus on its own, and it
//! keeps focus from leaving the overlay while it is up. An OS prompt
//! (Windows Hello, Touch ID, a password manager) can always take and keep
//! focus.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::{self, Config, Variant, Version};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::Zeroizing;

/// Broadcast to every webview when the lock state or its config changes.
/// The payload is deliberately empty: `pkg-*` child webviews (partner sites)
/// hold `core:default` and could listen. They can't call
/// `app_lock_status` (no `allow-app-commands`), so the event carries nothing
/// worth reading. Shell windows refetch the status on it.
pub const APP_LOCK_CHANGED_EVENT: &str = "app-lock://changed";
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
pub const BACKOFF_MS: u64 = 30_000;
const IDLE_TICK: Duration = Duration::from_secs(10);

const SECRET_VERSION: u32 = 1;
const SALT_LEN: usize = 16;
const HASH_LEN: u32 = 32;
// Same argon2id cost as the vault envelope (`secrets/crypto.rs`).
const ARGON2_MEMORY_KIB: u32 = 19_456;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_PARALLELISM: u32 = 1;

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
    /// Absolute path of `app-lock.json`: the recovery path shown on the lock
    /// screen.
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
    /// An unlock is being verified off-thread; a second one waits its turn.
    verifying: bool,
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
        self.config = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<AppLockConfig>(&bytes) {
                Ok(config) => sanitize(config),
                Err(error) => {
                    log::warn!("[app-lock] {} is unreadable, using defaults: {error}", path.display());
                    AppLockConfig::default()
                }
            },
            Err(_) => AppLockConfig::default(),
        };
        self.path = Some(path);
        self.last_activity_ms = now;
        if self.config.idle_enabled && self.config.secret.is_some() {
            self.set_locked(LockReason::Launch, now);
        }
    }

    fn set_locked(&mut self, reason: LockReason, now: u64) {
        self.locked = true;
        self.reason = Some(reason);
        self.locked_at_ms = Some(now);
    }

    fn set_unlocked(&mut self, now: u64) {
        self.locked = false;
        self.reason = None;
        self.locked_at_ms = None;
        self.failed_attempts = 0;
        self.retry_at_ms = None;
        self.last_activity_ms = now;
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
            return Err("Set a PIN or passphrase first — otherwise nothing could unlock it.".into());
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
            self.failed_attempts = 0;
            self.retry_at_ms = Some(now + BACKOFF_MS);
            return Some(format!("Wrong PIN. Wait {} s before trying again.", BACKOFF_MS / 1000));
        }
        let left = MAX_ATTEMPTS - self.failed_attempts;
        Some(format!(
            "Wrong PIN. {} before a {} s wait.",
            match left {
                1 => "One attempt left".to_string(),
                2 => "Two attempts left".to_string(),
                n => format!("{n} attempts left"),
            },
            BACKOFF_MS / 1000
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
            host: host_name(),
            os: os_label(),
            config_path: self.path.as_ref().map(|p| p.display().to_string()),
        }
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.as_ref() else {
            return Err("app lock is not configured yet (no data directory)".into());
        };
        let json = serde_json::to_vec_pretty(&self.config).map_err(|e| format!("serialize: {e}"))?;
        write_private_atomic(path, &json).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

/// Clamp a loaded config into range and drop combinations that can't work.
fn sanitize(mut config: AppLockConfig) -> AppLockConfig {
    config.idle_minutes = config.idle_minutes.clamp(MIN_IDLE_MINUTES, MAX_IDLE_MINUTES);
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

/// Managed Tauri state. One per app; every window reads the same lock.
#[derive(Debug, Default)]
pub struct AppLockState {
    inner: Mutex<Inner>,
}

impl AppLockState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the state at `app_data_dir/app-lock.json` and load it. Called once
    /// from `lib.rs` setup. If idle lock is on, the app starts locked.
    pub fn configure(&self, path: PathBuf) {
        let now = now_ms();
        self.with(|inner| inner.load(path, now));
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut guard = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut *guard)
    }
}

/// Lock on idle even when no window asks: a ticker every `IDLE_TICK`.
pub fn spawn_idle_ticker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(IDLE_TICK);
        loop {
            tick.tick().await;
            let locked_now = match app.try_state::<AppLockState>() {
                Some(state) => state.with(|inner| inner.check_idle(now_ms())),
                None => false,
            };
            if locked_now {
                log::info!("[app-lock] locked after idle");
                emit_changed(&app);
            }
        }
    });
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(APP_LOCK_CHANGED_EVENT, serde_json::json!({}));
}

// ─── commands ───────────────────────────────────────────────────────────────

/// The current lock state and config. Also runs the idle check, so a window
/// that wakes from sleep sees the lock without waiting for the ticker.
#[tauri::command]
pub async fn app_lock_status(
    app: AppHandle,
    state: State<'_, AppLockState>,
) -> Result<AppLockStatus, String> {
    let now = now_ms();
    let (locked_now, status) = state.with(|inner| {
        let locked_now = inner.check_idle(now);
        (locked_now, inner.status(now))
    });
    if locked_now {
        emit_changed(&app);
    }
    Ok(status)
}

/// Record user activity in some window. The frontend throttles this, and it
/// is ignored while locked.
#[tauri::command]
pub async fn app_lock_touch(state: State<'_, AppLockState>) -> Result<(), String> {
    let now = now_ms();
    state.with(|inner| inner.touch(now));
    Ok(())
}

/// Lock now. Refused without a PIN, since nothing could unlock it.
#[tauri::command]
pub async fn app_lock_lock(
    app: AppHandle,
    state: State<'_, AppLockState>,
) -> Result<AppLockStatus, String> {
    let now = now_ms();
    let (changed, status) = state.with(|inner| {
        let changed = inner.lock(LockReason::Manual, now)?;
        Ok::<_, String>((changed, inner.status(now)))
    })?;
    if changed {
        log::info!("[app-lock] locked (Lock now)");
        emit_changed(&app);
    }
    Ok(status)
}

/// Unlock with the PIN or passphrase. A wrong entry is not an `Err`: it
/// returns `ok: false` with the line D-05 shows under the field.
#[tauri::command]
pub async fn app_lock_unlock(
    app: AppHandle,
    state: State<'_, AppLockState>,
    secret: String,
) -> Result<UnlockOutcome, String> {
    let secret = Zeroizing::new(secret);
    let now = now_ms();
    let gate = state.with(|inner| inner.begin_unlock(now));
    let record = match gate {
        UnlockGate::NotLocked => {
            return Ok(outcome(&state, true, None));
        }
        UnlockGate::Wait(ms) => {
            let secs = (ms + 999) / 1000;
            return Ok(outcome(&state, false, Some(format!("Too many wrong entries. Try again in {secs} s."))));
        }
        UnlockGate::Busy => {
            return Ok(outcome(&state, false, Some("Still checking the last entry.".into())));
        }
        UnlockGate::NoSecret => {
            log::warn!("[app-lock] locked with no PIN on record; unlocking");
            state.with(|inner| inner.set_unlocked(now_ms()));
            emit_changed(&app);
            return Ok(outcome(&state, true, None));
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
            state.with(|inner| inner.verifying = false);
            return Err(error);
        }
    };
    let error = state.with(|inner| inner.finish_unlock(ok, now_ms()));
    if ok {
        log::info!("[app-lock] unlocked (PIN)");
        emit_changed(&app);
    }
    Ok(outcome(&state, ok, error))
}

/// Unlock with OS biometrics. Always refused on this build (see the module
/// header); kept so the frontend and the ACL already carry the path the
/// follow-up fills in.
#[tauri::command]
pub async fn app_lock_unlock_biometric(
    state: State<'_, AppLockState>,
) -> Result<UnlockOutcome, String> {
    let support = biometric_support();
    let reason = if support.available {
        "biometric unlock is not wired on this build"
    } else {
        support.reason
    };
    Ok(outcome(&state, false, Some(reason.to_string())))
}

/// Idle lock on/off, minutes, and unlock method.
#[tauri::command]
pub async fn app_lock_configure(
    app: AppHandle,
    state: State<'_, AppLockState>,
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
    let status = state.with(|inner| {
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
    })?;
    emit_changed(&app);
    Ok(status)
}

/// Set or change the PIN or passphrase. Changing one needs the current one.
#[tauri::command]
pub async fn app_lock_set_secret(
    app: AppHandle,
    state: State<'_, AppLockState>,
    current: Option<String>,
    next: String,
) -> Result<AppLockStatus, String> {
    let current = current.map(Zeroizing::new);
    let next = Zeroizing::new(next);
    validate_secret(next.as_str())?;
    let existing = state.with(|inner| {
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
    let status = state.with(|inner| {
        let previous = inner.config.clone();
        inner.config.secret = Some(record);
        if let Err(error) = inner.save() {
            inner.config = previous;
            return Err(error);
        }
        Ok(inner.status(now))
    })?;
    emit_changed(&app);
    Ok(status)
}

/// Remove the PIN. That also turns idle lock off, since nothing could unlock.
#[tauri::command]
pub async fn app_lock_clear_secret(
    app: AppHandle,
    state: State<'_, AppLockState>,
    current: String,
) -> Result<AppLockStatus, String> {
    let current = Zeroizing::new(current);
    let existing = state.with(|inner| {
        if inner.locked {
            return Err("Unlock first.".to_string());
        }
        Ok(inner.config.secret.clone())
    })?;
    let Some(existing) = existing else {
        return Ok(state.with(|inner| inner.status(now_ms())));
    };
    let ok = tokio::task::spawn_blocking(move || verify_secret(current.as_str(), &existing))
        .await
        .map_err(|e| format!("join: {e}"))??;
    if !ok {
        return Err("The current PIN is wrong.".into());
    }
    let now = now_ms();
    let status = state.with(|inner| {
        let previous = inner.config.clone();
        inner.config.secret = None;
        inner.config.idle_enabled = false;
        inner.config.method = UnlockMethod::Pin;
        if let Err(error) = inner.save() {
            inner.config = previous;
            return Err(error);
        }
        Ok(inner.status(now))
    })?;
    emit_changed(&app);
    Ok(status)
}

fn outcome(state: &AppLockState, ok: bool, error: Option<String>) -> UnlockOutcome {
    let status = state.with(|inner| inner.status(now_ms()));
    UnlockOutcome { ok, error, status }
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

fn host_name() -> String {
    let name = tauri_plugin_os::hostname();
    if name.trim().is_empty() {
        "this device".into()
    } else {
        name
    }
}

fn os_label() -> String {
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
        assert!(inner.finish_unlock(false, 10).unwrap().contains("Two attempts left"));
        assert!(matches!(inner.begin_unlock(20), UnlockGate::Check(_)));
        assert!(inner.finish_unlock(false, 20).unwrap().contains("One attempt left"));
        assert!(matches!(inner.begin_unlock(30), UnlockGate::Check(_)));
        assert!(inner.finish_unlock(false, 30).unwrap().contains("Wait 30 s"));

        assert_eq!(inner.begin_unlock(40), UnlockGate::Wait(BACKOFF_MS - 10));
        assert!(matches!(inner.begin_unlock(30 + BACKOFF_MS), UnlockGate::Check(_)));
        assert_eq!(inner.finish_unlock(true, 30 + BACKOFF_MS), None);
        assert!(!inner.locked);
        assert_eq!(inner.failed_attempts, 0);
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
        assert_eq!(config.method, UnlockMethod::Pin, "biometrics are unavailable on this build");
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
}
