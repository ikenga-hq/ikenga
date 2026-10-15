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
//! Core types and state live in [`crate::server::shared::app_lock`].

pub use crate::server::shared::app_lock::*;

use tauri::{AppHandle, Emitter, Manager, State};

/// Lock on idle even when no window asks: a ticker every `IDLE_TICK`.
pub fn spawn_idle_ticker(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(IDLE_TICK);
        loop {
            tick.tick().await;
            let locked_now = match app.try_state::<AppLockState>() {
                Some(state) => state.check_idle().0,
                None => false,
            };
            if locked_now {
                log::info!("[app-lock] locked after idle");
                audit_locked(&app, "idle");
                emit_changed(&app);
            }
        }
    });
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(APP_LOCK_CHANGED_EVENT, serde_json::json!({}));
}

/// G-ACCESS §6.5 / §6.9 (WP-77): append a desktop-local row — `app.*`,
/// `vault.*` — to the access audit chain through the daemon's
/// `access_audit_record_local` (the operator bearer; the desktop never opens
/// the store, P-20). **Best-effort and off the lock path:** spawned, never
/// awaited by the lock or unlock it records, and a failure (no daemon, a
/// store that is unavailable) is only logged.
pub(crate) fn record_access_audit(
    app: &AppHandle,
    kind: &'static str,
    target: impl Into<String>,
    detail: serde_json::Value,
) {
    let app = app.clone();
    let target = target.into();
    tauri::async_runtime::spawn(async move {
        let Some(daemon) =
            app.try_state::<std::sync::Arc<crate::pty::daemon_client::DaemonState>>()
        else {
            return;
        };
        let daemon = daemon.inner().clone();
        let args = serde_json::json!({ "kind": kind, "target": target, "detail": detail });
        match crate::commands::access::proxy(&app, &daemon, "access_audit_record_local", args).await
        {
            Ok(_) => {}
            // G-ACCESS §2.5: a drop while the store is unavailable is
            // logged at `warn` (review m-5) — once per window, so idle
            // locks on a machine with no daemon don't flood the log.
            Err(e) if e.starts_with("store_unavailable") => {
                if store_unavailable_warn_due((now_ms() / 1000) as i64) {
                    log::warn!(
                        "[access-audit] {kind} not recorded (and further drops for 10 min are \
                         logged at debug): {e}"
                    );
                } else {
                    log::debug!("[access-audit] {kind} not recorded: {e}");
                }
            }
            Err(e) => log::warn!("[access-audit] {kind} not recorded: {e}"),
        }
    });
}

/// The `store_unavailable` warn window for [`record_access_audit`].
const STORE_UNAVAILABLE_WARN_SECS: i64 = 600;

/// Whether a `store_unavailable` drop at `now` (unix seconds) logs at
/// `warn`: the first, then at most once per [`STORE_UNAVAILABLE_WARN_SECS`].
fn store_unavailable_warn_due(now: i64) -> bool {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(i64::MIN);
    let last = LAST.load(Ordering::Relaxed);
    if last != i64::MIN && now.saturating_sub(last) < STORE_UNAVAILABLE_WARN_SECS {
        return false;
    }
    LAST.compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
}

/// `app.locked {reason}` (best-effort, see [`record_access_audit`]).
fn audit_locked(app: &AppHandle, reason: &'static str) {
    record_access_audit(
        app,
        "app.locked",
        host_name(),
        serde_json::json!({ "reason": reason }),
    );
}

/// `app.unlocked {method}` (best-effort).
fn audit_unlocked(app: &AppHandle, method: &'static str) {
    record_access_audit(
        app,
        "app.unlocked",
        host_name(),
        serde_json::json!({ "method": method }),
    );
}

// ─── commands ───────────────────────────────────────────────────────────────

/// The current lock state and config. Also runs the idle check, so a window
/// that wakes from sleep sees the lock without waiting for the ticker.
#[tauri::command]
pub async fn app_lock_status(
    app: AppHandle,
    state: State<'_, AppLockState>,
) -> Result<AppLockStatus, String> {
    let (locked_now, status) = state.check_idle();
    if locked_now {
        audit_locked(&app, "idle");
        emit_changed(&app);
    }
    Ok(status)
}

/// Record user activity in some window. The frontend throttles this, and it
/// is ignored while locked.
#[tauri::command]
pub async fn app_lock_touch(state: State<'_, AppLockState>) -> Result<(), String> {
    state.touch();
    Ok(())
}

/// Lock now. Refused without a PIN, since nothing could unlock it.
#[tauri::command]
pub async fn app_lock_lock(
    app: AppHandle,
    state: State<'_, AppLockState>,
) -> Result<AppLockStatus, String> {
    let (changed, status) = state.lock(LockReason::Manual)?;
    if changed {
        log::info!("[app-lock] locked (Lock now)");
        audit_locked(&app, "manual");
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
    let (outcome, changed) = state.unlock(secret).await?;
    if changed {
        log::info!("[app-lock] unlocked (PIN)");
        audit_unlocked(&app, "pin");
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
    let support = biometric_support();
    let reason = if support.available {
        "biometric unlock is not wired on this build"
    } else {
        &support.reason
    };
    Ok(state.outcome(false, Some(reason.to_string())))
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
    let status = state.configure_settings(idle_enabled, idle_minutes, method)?;
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
    let status = state.set_secret(current, next).await?;
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
    let status = state.clear_secret(current).await?;
    emit_changed(&app);
    Ok(status)
}

// ─── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Review m-5: a `store_unavailable` drop warns, then at most once per
    /// window (the rest log at debug).
    #[test]
    fn store_unavailable_drops_warn_once_per_window() {
        let t = 4_000_000_000;
        assert!(store_unavailable_warn_due(t));
        assert!(!store_unavailable_warn_due(t + 1));
        assert!(!store_unavailable_warn_due(
            t + STORE_UNAVAILABLE_WARN_SECS - 1
        ));
        assert!(store_unavailable_warn_due(t + STORE_UNAVAILABLE_WARN_SECS));
    }
}
