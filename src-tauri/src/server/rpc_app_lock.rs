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

    async fn recv_topic(
        rx: &mut tokio::sync::broadcast::Receiver<crate::server::events::Event>,
        topic: Topic,
    ) -> crate::server::events::Event {
        use std::time::Duration;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let ev = rx.recv().await.expect("channel closed waiting for event");
                if ev.topic == topic {
                    return ev;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timeout waiting for {topic:?}"))
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

        let ev1 = recv_topic(&mut rx, Topic::AppLockChanged).await;
        assert_eq!(ev1.topic, Topic::AppLockChanged);

        // Locking publishes AppLockChanged
        app_lock_lock(&state);
        let ev2 = recv_topic(&mut rx, Topic::AppLockChanged).await;
        assert_eq!(ev2.topic, Topic::AppLockChanged);

        // Unlocking publishes AppLockChanged
        app_lock_unlock(&state, &serde_json::json!({ "secret": "1234" })).await;
        let ev3 = recv_topic(&mut rx, Topic::AppLockChanged).await;
        assert_eq!(ev3.topic, Topic::AppLockChanged);
    }

    #[tokio::test]
    async fn set_secret_wrong_guesses_hit_backoff_and_lock_rate_limit() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        // Set initial PIN
        let set_res = app_lock_set_secret(
            &state,
            &serde_json::json!({
                "next": "1234"
            }),
        )
        .await;
        assert!(set_res.ok);

        // Run 100 parallel requests with wrong current PIN
        let mut tasks = Vec::new();
        for i in 0..100 {
            let state_clone = state.clone();
            tasks.push(tokio::spawn(async move {
                app_lock_set_secret(
                    &state_clone,
                    &serde_json::json!({
                        "current": format!("wrong_{i}"),
                        "next": "5678"
                    }),
                )
                .await
            }));
        }

        let mut errors = Vec::new();
        for t in tasks {
            let res = t.await.unwrap();
            assert!(!res.ok, "wrong PIN guess must be rejected");
            errors.push(res.error.unwrap());
        }

        assert_eq!(errors.len(), 100);
        let hit_backoff_or_busy = errors.iter().any(|e| {
            e.contains("Wait ")
                || e.contains("Still checking")
                || e.contains("Too many wrong entries")
        });
        assert!(
            hit_backoff_or_busy,
            "100 parallel guesses must trigger rate-limiting / busy / backoff"
        );

        let status_res = app_lock_status(&state);
        let status: AppLockStatus = serde_json::from_value(status_res.data.unwrap()).unwrap();
        assert!(
            status.attempts_left < 3 || status.retry_in_ms.is_some(),
            "attemptsLeft must decrease or retryInMs must be set after wrong guesses"
        );
    }

    #[tokio::test]
    async fn parallel_set_secret_with_right_pin_last_protects_busy_gate() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        let mut tasks = Vec::new();
        for i in 0..99 {
            let state_clone = state.clone();
            tasks.push(tokio::spawn(async move {
                app_lock_set_secret(
                    &state_clone,
                    &serde_json::json!({ "current": format!("wrong_{i}"), "next": "5678" }),
                )
                .await
            }));
        }
        let state_clone = state.clone();
        tasks.push(tokio::spawn(async move {
            app_lock_set_secret(
                &state_clone,
                &serde_json::json!({ "current": "1234", "next": "5678" }),
            )
            .await
        }));

        let mut found = 0;
        let mut busy = 0;
        for t in tasks {
            let res = t.await.unwrap();
            if res.ok {
                found += 1;
            } else if let Some(e) = res.error {
                if e.contains("Still checking") {
                    busy += 1;
                }
            }
        }

        assert_eq!(found, 0, "parallel attack must not succeed");
        assert!(busy > 0, "busy gate must reject concurrent checks");
    }

    #[tokio::test]
    async fn parallel_clear_secret_with_right_pin_last_protects_busy_gate() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        let mut tasks = Vec::new();
        for i in 0..99 {
            let state_clone = state.clone();
            tasks.push(tokio::spawn(async move {
                app_lock_clear_secret(
                    &state_clone,
                    &serde_json::json!({ "current": format!("wrong_{i}") }),
                )
                .await
            }));
        }
        let state_clone = state.clone();
        tasks.push(tokio::spawn(async move {
            app_lock_clear_secret(&state_clone, &serde_json::json!({ "current": "1234" })).await
        }));

        let mut found = 0;
        let mut busy = 0;
        for t in tasks {
            let res = t.await.unwrap();
            if res.ok {
                found += 1;
            } else if let Some(e) = res.error {
                if e.contains("Still checking") {
                    busy += 1;
                }
            }
        }

        assert_eq!(found, 0, "parallel clear_secret attack must not succeed");
        assert!(
            busy > 0,
            "busy gate must reject concurrent checks on clear_secret"
        );
        let status: AppLockStatus =
            serde_json::from_value(app_lock_status(&state).data.unwrap()).unwrap();
        assert!(
            status.secret_set,
            "PIN must remain set after parallel attack"
        );
    }

    #[tokio::test]
    async fn sequential_wrong_guesses_hit_wait_and_block_fourth_guess() {
        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        // 3 sequential wrong guesses
        for i in 1..=3 {
            let res = app_lock_set_secret(
                &state,
                &serde_json::json!({ "current": format!("bad_{i}"), "next": "9999" }),
            )
            .await;
            assert!(!res.ok);
        }

        // 4th sequential guess with RIGHT PIN must be refused with "Too many wrong entries"
        let res4 = app_lock_set_secret(
            &state,
            &serde_json::json!({ "current": "1234", "next": "9999" }),
        )
        .await;
        assert!(!res4.ok);
        let err = res4.error.unwrap();
        assert!(
            err.contains("Too many wrong entries. Try again in"),
            "expected wait error, got: {err}"
        );

        let res_clear =
            app_lock_clear_secret(&state, &serde_json::json!({ "current": "1234" })).await;
        assert!(!res_clear.ok);
        assert!(res_clear.error.unwrap().contains("Too many wrong entries"));

        let status: AppLockStatus =
            serde_json::from_value(app_lock_status(&state).data.unwrap()).unwrap();
        assert!(status.retry_in_ms.is_some(), "retryInMs must be set");
        assert!(status.secret_set);
    }

    #[tokio::test]
    async fn cancelled_pin_check_does_not_wedge_lock() {
        use std::time::Duration;

        let dir = tempdir().unwrap();
        let state = test_app_state(dir.path());

        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        // Start a PIN check and abort it mid-check
        let state_clone = state.clone();
        let handle = tokio::spawn(async move {
            app_lock_set_secret(
                &state_clone,
                &serde_json::json!({ "current": "wrong_pin", "next": "5678" }),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(1)).await;
        handle.abort();
        let _ = handle.await;

        // The aborted check runs to completion on its own task (round 6: so
        // it can't be used to start a second argon2), so the next attempt may
        // briefly hear "Still checking" — but never for good.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let res = loop {
            let res = app_lock_set_secret(
                &state,
                &serde_json::json!({ "current": "1234", "next": "5678" }),
            )
            .await;
            let busy = res
                .error
                .as_deref()
                .is_some_and(|e| e.contains("Still checking"));
            if !busy || std::time::Instant::now() > deadline {
                break res;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(
            res.ok,
            "cancelled check must not leave lock wedged: error was {:?}",
            res.error
        );
    }

    #[tokio::test]
    async fn server_side_lock_enforcement_refuses_rpc_arms_except_status_unlock_health() {
        let dir = tempdir().unwrap();
        let state = Arc::new(test_app_state(dir.path()));

        // Set PIN
        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        fn test_ctx() -> crate::access::AccessCtx {
            crate::access::AccessCtx {
                principal_id: crate::executor::PrincipalId::new_v7(),
                via: crate::access::Via::Operator,
                device_id: None,
                tier: crate::access::Tier::Full,
                share: None,
                share_headers: false,
                caps: crate::access::CapSet::ALL,
                admin_strength: true,
                is_admin: false,
                meta: crate::access::RequestMeta::default(),
            }
        }

        async fn rpc(
            state: &Arc<AppState>,
            cmd: &str,
            args: serde_json::Value,
        ) -> serde_json::Value {
            use axum::response::IntoResponse;
            let req = super::super::rpc::RpcRequest {
                cmd: cmd.to_string(),
                args,
            };
            let response = super::super::rpc::rpc_handler(
                axum::extract::State(state.clone()),
                None,
                Some(axum::Extension(test_ctx())),
                axum::Json(req),
            )
            .await
            .into_response();
            let body = axum::body::to_bytes(response.into_body(), 10 * 1024 * 1024)
                .await
                .unwrap();
            serde_json::from_slice(&body).unwrap()
        }

        // Before locking, arms are not blocked by app lock
        let status_before = rpc(&state, "app_lock_status", serde_json::json!({})).await;
        assert_eq!(status_before["ok"], true);

        // Lock the account
        let lock_res = app_lock_lock(&state);
        assert!(lock_res.ok);
        assert!(state.app_lock.is_locked());

        // While locked, random RPC arms are refused
        let pty_res = rpc(&state, "pty_spawn", serde_json::json!({})).await;
        assert_eq!(pty_res["ok"], false);
        assert!(
            pty_res["error"]
                .as_str()
                .unwrap_or("")
                .contains("Account is locked"),
            "expected 'Account is locked', got: {:?}",
            pty_res["error"]
        );

        let fs_res = rpc(&state, "fs_read", serde_json::json!({ "path": "/test" })).await;
        assert_eq!(fs_res["ok"], false);
        assert!(
            fs_res["error"]
                .as_str()
                .unwrap_or("")
                .contains("Account is locked"),
            "expected 'Account is locked', got: {:?}",
            fs_res["error"]
        );

        let list_res = rpc(&state, "fs_list", serde_json::json!({ "path": "." })).await;
        assert_eq!(list_res["ok"], false);
        assert!(
            list_res["error"]
                .as_str()
                .unwrap_or("")
                .contains("Account is locked"),
            "expected 'Account is locked', got: {:?}",
            list_res["error"]
        );

        // Exempt arms are permitted while locked: app_lock_status, server_health
        let status_res = rpc(&state, "app_lock_status", serde_json::json!({})).await;
        assert_eq!(
            status_res["ok"], true,
            "app_lock_status must be exempt while locked"
        );
        let status: AppLockStatus = serde_json::from_value(status_res["data"].clone()).unwrap();
        assert!(status.locked);

        let health_res = rpc(&state, "server_health", serde_json::json!({})).await;
        assert_ne!(
            health_res["error"].as_str().unwrap_or(""),
            "Account is locked. Unlock first.",
            "server_health must be exempt from lock gate"
        );

        // Unlock with wrong PIN is permitted (evaluates PIN check, not blocked by lock gate)
        let unlock_bad = rpc(
            &state,
            "app_lock_unlock",
            serde_json::json!({ "secret": "wrong" }),
        )
        .await;
        assert_eq!(unlock_bad["ok"], true);
        assert_eq!(unlock_bad["data"]["ok"], false);
        assert!(
            unlock_bad["data"]["error"]
                .as_str()
                .unwrap_or("")
                .contains("Wrong PIN"),
            "expected Wrong PIN error, got: {:?}",
            unlock_bad["data"]["error"]
        );

        // Unlock with right PIN succeeds and unpauses
        let unlock_ok = rpc(
            &state,
            "app_lock_unlock",
            serde_json::json!({ "secret": "1234" }),
        )
        .await;
        assert_eq!(unlock_ok["ok"], true);
        assert_eq!(unlock_ok["data"]["ok"], true);
        assert!(!state.app_lock.is_locked());

        // Now arms are no longer refused by lock
        let status_after = rpc(&state, "app_lock_status", serde_json::json!({})).await;
        assert_eq!(status_after["ok"], true);
        let status2: AppLockStatus = serde_json::from_value(status_after["data"].clone()).unwrap();
        assert!(!status2.locked);
    }

    #[tokio::test]
    async fn server_side_lock_enforcement_pauses_and_resumes_sockets() {
        use std::time::Duration;

        let dir = tempdir().unwrap();
        let state = Arc::new(test_app_state(dir.path()));

        let set_res = app_lock_set_secret(&state, &serde_json::json!({ "next": "1234" })).await;
        assert!(set_res.ok);

        let lock_res = app_lock_lock(&state);
        assert!(lock_res.ok);
        assert!(state.app_lock.is_locked());

        // Verify wait_unlocked blocks while locked
        let app_lock = state.app_lock.clone();
        let unpause_task = tokio::spawn(async move {
            app_lock.wait_unlocked().await;
            true
        });

        // Sleep briefly: unpause_task must still be waiting
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !unpause_task.is_finished(),
            "socket waiter must remain paused while locked"
        );

        // Unlock: unpause_task must complete immediately
        let unlock = state.app_lock.unlock("1234".to_string()).await.unwrap();
        assert!(unlock.1, "must unlock");

        let res = tokio::time::timeout(Duration::from_secs(1), unpause_task)
            .await
            .unwrap()
            .unwrap();
        assert!(res, "socket waiter must resume cleanly after unlock");
    }
}
