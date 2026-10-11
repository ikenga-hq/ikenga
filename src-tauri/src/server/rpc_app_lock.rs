//! RPC handlers for the `app_lock_*` family (WP-C).
//!
//! Exposes app lock operations over `/api/rpc`. State and PIN records are stored
//! per principal in `<data-dir>/app-lock.json`. When an account's lock state or config
//! changes, `app-lock://changed` is broadcast over `/ws/events` to all connected sessions
//! for that account.

use serde::Deserialize;
use serde_json::Value;

use super::events::Topic;
use super::rpc::RpcResponse;
use super::shared::app_lock::{LockReason, UnlockMethod};
use super::AppState;

#[derive(Deserialize)]
struct UnlockArgs {
    secret: String,
}

#[derive(Deserialize)]
struct ConfigureArgs {
    #[serde(alias = "idleEnabled")]
    idle_enabled: bool,
    #[serde(alias = "idleMinutes")]
    idle_minutes: u32,
    #[serde(default)]
    method: UnlockMethod,
}

#[derive(Deserialize)]
struct SetSecretArgs {
    #[serde(default)]
    current: Option<String>,
    next: String,
}

#[derive(Deserialize)]
struct ClearSecretArgs {
    current: String,
}

pub fn app_lock_status(state: &AppState) -> RpcResponse {
    let (locked_now, status) = state.app_lock.check_idle();
    if locked_now {
        tracing::info!("[app-lock] locked after idle");
        state
            .events
            .publish(Topic::AppLockChanged, &serde_json::json!({}));
    }
    RpcResponse::success(status)
}

pub fn app_lock_touch(state: &AppState) -> RpcResponse {
    state.app_lock.touch();
    RpcResponse::success(serde_json::json!({}))
}

pub fn app_lock_lock(state: &AppState) -> RpcResponse {
    match state.app_lock.lock(LockReason::Manual) {
        Ok((changed, status)) => {
            if changed {
                tracing::info!("[app-lock] locked (Lock now)");
                state
                    .events
                    .publish(Topic::AppLockChanged, &serde_json::json!({}));
            }
            RpcResponse::success(status)
        }
        Err(e) => RpcResponse::error(e),
    }
}

pub async fn app_lock_unlock(state: &AppState, args: &Value) -> RpcResponse {
    let args: UnlockArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return RpcResponse::error(format!("invalid arguments: {e}")),
    };
    match state.app_lock.unlock(args.secret).await {
        Ok((outcome, changed)) => {
            if changed {
                tracing::info!("[app-lock] unlocked (PIN)");
                state
                    .events
                    .publish(Topic::AppLockChanged, &serde_json::json!({}));
            }
            RpcResponse::success(outcome)
        }
        Err(e) => RpcResponse::error(e),
    }
}

pub fn app_lock_configure(state: &AppState, args: &Value) -> RpcResponse {
    let args: ConfigureArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return RpcResponse::error(format!("invalid arguments: {e}")),
    };
    match state
        .app_lock
        .configure_settings(args.idle_enabled, args.idle_minutes, args.method)
    {
        Ok(status) => {
            state
                .events
                .publish(Topic::AppLockChanged, &serde_json::json!({}));
            RpcResponse::success(status)
        }
        Err(e) => RpcResponse::error(e),
    }
}

pub async fn app_lock_set_secret(state: &AppState, args: &Value) -> RpcResponse {
    let args: SetSecretArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return RpcResponse::error(format!("invalid arguments: {e}")),
    };
    match state.app_lock.set_secret(args.current, args.next).await {
        Ok(status) => {
            state
                .events
                .publish(Topic::AppLockChanged, &serde_json::json!({}));
            RpcResponse::success(status)
        }
        Err(e) => RpcResponse::error(e),
    }
}

pub async fn app_lock_clear_secret(state: &AppState, args: &Value) -> RpcResponse {
    let args: ClearSecretArgs = match serde_json::from_value(args.clone()) {
        Ok(a) => a,
        Err(e) => return RpcResponse::error(format!("invalid arguments: {e}")),
    };
    match state.app_lock.clear_secret(args.current).await {
        Ok(status) => {
            state
                .events
                .publish(Topic::AppLockChanged, &serde_json::json!({}));
            RpcResponse::success(status)
        }
        Err(e) => RpcResponse::error(e),
    }
}

#[cfg(test)]
mod tests {
    use super::super::shared::app_lock::{self, AppLockStatus, UnlockOutcome};
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn test_app_state(dir: &std::path::Path) -> AppState {
        let app_lock = Arc::new(app_lock::AppLockState::new());
        app_lock.configure(dir.join(app_lock::CONFIG_FILENAME));

        let config = super::super::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: dir.to_path_buf(),
            pkgs_dir: None,
            data_dir: Some(dir.to_path_buf()),
            auth_token: Some("test-token".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: crate::executor::ExecutorTier::T0,
        };
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(4);
        let events = super::super::events::EventBus::new();

        AppState {
            config,
            spa_service: super::super::SpaStaticService::new(dir),
            pty_manager: Arc::new(crate::pty::PtyManager::new()),
            engine_registry: Arc::new(crate::engines::EngineRegistry::new()),
            pa_db: None,
            pkg_static: super::super::PkgStaticService::from_packages(None, &[]),
            pkg_index: Arc::new(super::super::PkgIndex::from_packages(&[])),
            settings: None,
            home: None,
            path_guard: super::super::rpc_shell::PathGuard::allowlist(),
            actions: None,
            store: None,
            secrets: Arc::new(crate::secrets_env::DaemonSecrets::for_daemon(
                Some(dir),
                crate::executor::ExecutorTier::T0,
            )),
            chi: Arc::new(super::super::rpc_local::DaemonChi::host()),
            events,
            term_hooks: super::super::term_hooks::TermHooks::new(
                Some(dir),
                super::super::events::EventBus::new(),
            ),
            update: None,
            shutdown_tx,
            viewer: Arc::new(super::super::viewer::ViewerService::new()),
            sidecars: Arc::new(super::super::shared::pkg_helpers::DaemonSidecarManager::new()),
            app_lock,
        }
    }

    #[tokio::test]
    async fn cross_account_isolation_locks_signed_in_account_only() {
        let alice_dir = tempdir().unwrap();
        let bob_dir = tempdir().unwrap();

        let alice_state = test_app_state(alice_dir.path());
        let bob_state = test_app_state(bob_dir.path());

        // Alice sets a PIN
        let set_res = app_lock_set_secret(
            &alice_state,
            &serde_json::json!({
                "next": "1234"
            }),
        )
        .await;
        assert!(set_res.ok, "Alice PIN set failed: {:?}", set_res.error);

        // Bob's status remains unset and unlocked
        let bob_status_res = app_lock_status(&bob_state);
        let bob_status: AppLockStatus =
            serde_json::from_value(bob_status_res.data.unwrap()).unwrap();
        assert!(!bob_status.secret_set, "Bob should not have secretSet");
        assert!(!bob_status.locked, "Bob should not be locked");

        // Alice locks
        let lock_res = app_lock_lock(&alice_state);
        assert!(lock_res.ok, "Alice lock failed: {:?}", lock_res.error);
        let alice_status: AppLockStatus = serde_json::from_value(lock_res.data.unwrap()).unwrap();
        assert!(alice_status.locked, "Alice must be locked");

        // Bob is completely unaffected (still unlocked)
        let bob_status_res2 = app_lock_status(&bob_state);
        let bob_status2: AppLockStatus =
            serde_json::from_value(bob_status_res2.data.unwrap()).unwrap();
        assert!(
            !bob_status2.locked,
            "Bob must remain unlocked when Alice locks"
        );

        // Bob sets his own PIN and locks
        let bob_set_res = app_lock_set_secret(
            &bob_state,
            &serde_json::json!({
                "next": "5678"
            }),
        )
        .await;
        assert!(bob_set_res.ok);
        let bob_lock_res = app_lock_lock(&bob_state);
        assert!(bob_lock_res.ok);

        // Alice unlocks with her PIN — Bob remains locked
        let alice_unlock_res = app_lock_unlock(
            &alice_state,
            &serde_json::json!({
                "secret": "1234"
            }),
        )
        .await;
        assert!(alice_unlock_res.ok);
        let alice_outcome: UnlockOutcome =
            serde_json::from_value(alice_unlock_res.data.unwrap()).unwrap();
        assert!(alice_outcome.ok);
        assert!(!alice_outcome.status.locked);

        // Bob is still locked
        let bob_check = app_lock_status(&bob_state);
        let bob_check_status: AppLockStatus =
            serde_json::from_value(bob_check.data.unwrap()).unwrap();
        assert!(
            bob_check_status.locked,
            "Bob must remain locked when Alice unlocks"
        );
    }

    #[tokio::test]
    async fn rate_limiting_backoff_enforces_wrong_entry_wait() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        app_lock_set_secret(
            &state,
            &serde_json::json!({
                "next": "1234"
            }),
        )
        .await;
        app_lock_lock(&state);

        // Attempt 1: wrong PIN
        let r1 = app_lock_unlock(&state, &serde_json::json!({ "secret": "wrong1" })).await;
        let o1: UnlockOutcome = serde_json::from_value(r1.data.unwrap()).unwrap();
        assert!(!o1.ok);
        assert_eq!(o1.status.attempts_left, 2);
        assert!(o1.error.as_deref().unwrap().contains("Two attempts left"));

        // Attempt 2: wrong PIN
        let r2 = app_lock_unlock(&state, &serde_json::json!({ "secret": "wrong2" })).await;
        let o2: UnlockOutcome = serde_json::from_value(r2.data.unwrap()).unwrap();
        assert!(!o2.ok);
        assert_eq!(o2.status.attempts_left, 1);
        assert!(o2.error.as_deref().unwrap().contains("One attempt left"));

        // Attempt 3: wrong PIN -> enters 30s wait
        let r3 = app_lock_unlock(&state, &serde_json::json!({ "secret": "wrong3" })).await;
        let o3: UnlockOutcome = serde_json::from_value(r3.data.unwrap()).unwrap();
        assert!(!o3.ok);
        assert!(o3.status.retry_in_ms.is_some());
        assert!(o3.error.as_deref().unwrap().contains("Wait 30 s"));

        // Attempt 4: during wait window, even correct PIN is rejected by wait gate
        let r4 = app_lock_unlock(&state, &serde_json::json!({ "secret": "1234" })).await;
        let o4: UnlockOutcome = serde_json::from_value(r4.data.unwrap()).unwrap();
        assert!(
            !o4.ok,
            "Must reject during wait window even with correct PIN"
        );
        assert!(o4
            .error
            .as_deref()
            .unwrap()
            .contains("Too many wrong entries. Try again in"));
    }

    #[tokio::test]
    async fn event_bus_broadcasts_on_app_lock_changes() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());
        let mut rx = state.events.subscribe();

        // Setting secret publishes AppLockChanged
        app_lock_set_secret(
            &state,
            &serde_json::json!({
                "next": "1234"
            }),
        )
        .await;

        let ev1 = rx.recv().await.unwrap();
        assert_eq!(ev1.topic, Topic::AppLockChanged);

        // Locking publishes AppLockChanged
        app_lock_lock(&state);
        let ev2 = rx.recv().await.unwrap();
        assert_eq!(ev2.topic, Topic::AppLockChanged);

        // Unlocking publishes AppLockChanged
        app_lock_unlock(&state, &serde_json::json!({ "secret": "1234" })).await;
        let ev3 = rx.recv().await.unwrap();
        assert_eq!(ev3.topic, Topic::AppLockChanged);
    }
}
