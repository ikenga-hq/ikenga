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
//! The lock itself is persisted too: `app-lock.json` carries a `lock` record
//! (locked, reason, wrong-entry count, wait deadline, backoff round), written
//! whenever the app locks, unlocks or takes a wrong entry. So quitting or
//! crashing while locked relaunches locked, and a relaunch doesn't reset the
//! wrong-entry wait. When idle lock is on and a PIN is set, the app also
//! starts locked (`LockReason::Launch`) even if it was unlocked at quit.
//! Without that, quit and relaunch would get past the idle lock.
//!
//! Wrong entries: after every `MAX_ATTEMPTS` misses the wait grows through
//! `BACKOFF_STEPS_MS` (30 s, 1 min, 5 min, then 15 min each round) and only
//! resets on a successful unlock.
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
//!
//! ## Where the code lives (remote-access WP-21)
//!
//! The state machine, the persisted config, PIN hashing and the private file
//! write are in `src/secrets/app_lock.rs`, mounted as
//! `crate::secrets_env::app_lock` so the headless daemon serves the same
//! `app_lock_*` arms per principal. This file keeps only the Tauri side: the
//! managed state, the idle ticker, the `app-lock://changed` broadcast, and
//! the `tauri-plugin-os` host / OS labels.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager, State};
use zeroize::Zeroizing;

pub use crate::secrets_env::app_lock::{
    biometric_support, AppLockStatus, BiometricSupport, LockReason, UnlockMethod, UnlockOutcome,
    BACKOFF_STEPS_MS, CONFIG_FILENAME, DEFAULT_IDLE_MINUTES, MAX_ATTEMPTS, MAX_IDLE_MINUTES,
    MAX_SECRET_CHARS, MIN_IDLE_MINUTES, MIN_SECRET_CHARS,
};
use crate::secrets_env::app_lock::{AppLockCore, Platform};

/// Broadcast to every webview when the lock state or its config changes.
/// The payload is deliberately empty: `pkg-*` child webviews (partner sites)
/// hold `core:default` and could listen. They can't call
/// `app_lock_status` (no `allow-app-commands`), so the event carries nothing
/// worth reading. Shell windows refetch the status on it.
pub const APP_LOCK_CHANGED_EVENT: &str = "app-lock://changed";
const IDLE_TICK: Duration = Duration::from_secs(10);

/// Managed Tauri state. One per app; every window reads the same lock.
#[derive(Debug, Default)]
pub struct AppLockState {
    core: AppLockCore,
}

impl AppLockState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Point the state at `app_data_dir/app-lock.json` and load it. Called once
    /// from `lib.rs` setup. If idle lock is on, the app starts locked.
    pub fn configure(&self, path: std::path::PathBuf) {
        self.core.configure(
            path,
            Platform {
                host: host_name(),
                os: os_label(),
            },
        );
    }
}

/// Lock on idle even when no window asks: a ticker every `IDLE_TICK`.
pub fn spawn_idle_ticker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(IDLE_TICK);
        loop {
            tick.tick().await;
            let locked_now = match app.try_state::<AppLockState>() {
                Some(state) => state.core.check_idle(),
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
    let (locked_now, status) = state.core.status();
    if locked_now {
        emit_changed(&app);
    }
    Ok(status)
}

/// Record user activity in some window. The frontend throttles this, and it
/// is ignored while locked.
#[tauri::command]
pub async fn app_lock_touch(state: State<'_, AppLockState>) -> Result<(), String> {
    state.core.touch();
    Ok(())
}

/// Lock now. Refused without a PIN, since nothing could unlock it.
#[tauri::command]
pub async fn app_lock_lock(
    app: AppHandle,
    state: State<'_, AppLockState>,
) -> Result<AppLockStatus, String> {
    let (changed, status) = state.core.lock_now()?;
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
    let (outcome, changed) = state.core.unlock(Zeroizing::new(secret)).await?;
    if changed {
        emit_changed(&app);
    }
    Ok(outcome)
}

/// Unlock with OS biometrics. Always refused on this build (see the module
/// header); kept so the frontend and the ACL already carry the path the
/// follow-up fills in.
#[tauri::command]
pub async fn app_lock_unlock_biometric(
    state: State<'_, AppLockState>,
) -> Result<UnlockOutcome, String> {
    Ok(state.core.unlock_biometric())
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
    let status = state
        .core
        .configure_lock(idle_enabled, idle_minutes, method)?;
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
    let status = state
        .core
        .set_secret(current.map(Zeroizing::new), Zeroizing::new(next))
        .await?;
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
    let (changed, status) = state.core.clear_secret(Zeroizing::new(current)).await?;
    if changed {
        emit_changed(&app);
    }
    Ok(status)
}

// ─── platform ───────────────────────────────────────────────────────────────

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
