//! Notification aggregation (WP-40) — desktop facade.
//!
//! The store, the kinds, list / count / read-state / resolution and the
//! process-wide change channel live in [`crate::server::shared::notifications`]
//! (see its module doc for the shape), compiled into both binaries so the
//! daemon's `notifications_*` RPC arms run the same code (WP-19 slice 4).
//! Everything is re-exported here, so every `crate::notifications::…` path is
//! unchanged. What stays desktop-only:
//!
//! * [`spawn_event_forwarder`] — relays the change channel to the webview as
//!   the `notifications://changed` Tauri event;
//! * [`mute`]'s `AppHandle`-bound helpers (see that module);
//! * [`producers`] — the per-source copy / dedupe-key builders, which lean on
//!   the desktop-only Claude Code engine (`engines::claude_code::notify`);
//! * permission routing's desktop half (G-ACCESS §5.5, WP-75): the hook / ACP
//!   resolvers, the host's routing check, and the desktop → daemon ask relay
//!   ([`record_permission`], [`relay_resolved`]).

pub mod mute;
pub mod producers;

pub use crate::server::shared::notifications::*;

use tokio::sync::broadcast;

/// Relay the broadcast channel to the webview as `notifications://changed`,
/// stamping `muted` from settings so the toast bridge can stay quiet for a
/// muted kind without re-reading settings per event.
pub fn spawn_event_forwarder(app: tauri::AppHandle) {
    use tauri::Emitter;
    // Hand edits of the muted list publish `mute_changed` too.
    mute::spawn_settings_listener(app.clone());
    // G-ACCESS §5.5 (WP-75): the in-process decide core and the ask relay.
    routing_desktop::install(&app);
    let mut rx = subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(mut event) => {
                    if let Some(n) = &event.notification {
                        event.muted = mute::muted_kinds_for_app(&app).contains(&n.kind);
                    }
                    let _ = app.emit(EVENT_NAME, &event);
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    log::warn!(
                        target: "ikenga::notifications",
                        "event forwarder lagged {skipped} events; FE will refetch on the next one"
                    );
                    // Still tell the FE something changed.
                    let _ = app.emit(
                        EVENT_NAME,
                        &NotificationEvent {
                            reason: ChangeReason::ReadAll,
                            notification: None,
                            muted: false,
                        },
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub use routing_desktop::{host_side, record_permission, relay_resolved};

/// The desktop half of permission routing (G-ACCESS §5.5, WP-75; DEC-83):
///
/// * the resolvers `permission_decide` dispatches through in this process —
///   the held hook gate (`iyke::hooks`) and the Claude Code ACP round-trip;
/// * [`HostSide`](crate::server::shared::notifications::routing::HostSide):
///   the host's routing preference (§5.1) and `access_audit_record_local`,
///   both through the local daemon (the access store's one opener, P-20);
/// * the T0 desktop → daemon ask relay (§5.5 (a)): every answerable ask is
///   mirrored to the daemon (`permission_relay_put`), a long-poll task takes
///   remote decisions (`permission_relay_take`) and applies them here, and an
///   ask the desktop resolves itself closes its mirror
///   (`permission_relay_resolve`).
mod routing_desktop {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::Duration;

    use futures_util::future::BoxFuture;
    use serde_json::{json, Value};
    use tauri::{AppHandle, Manager};

    use crate::access::{AccessError, Code};
    use crate::commands::db::PaDb;
    use crate::pty::daemon_client::DaemonState;
    use crate::server::shared::notifications::routing::{
        self as core, AskKey, AskResolvers, Attribution, DecidedBy, Decision, HostRouting,
        HostSide, LocalRouting,
    };

    use super::producers::AskFacts;
    use super::{NewNotification, Notification};

    /// Desktop keys mirrored to the daemon and still open.
    fn pending() -> &'static Mutex<HashSet<String>> {
        static P: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        P.get_or_init(Default::default)
    }

    fn wake() -> &'static tokio::sync::Notify {
        static W: OnceLock<tokio::sync::Notify> = OnceLock::new();
        W.get_or_init(tokio::sync::Notify::new)
    }

    /// The owner principal / host device the daemon reported last.
    fn identity() -> &'static Mutex<(Option<String>, Option<String>)> {
        static I: OnceLock<Mutex<(Option<String>, Option<String>)>> = OnceLock::new();
        I.get_or_init(Default::default)
    }

    async fn daemon_rpc(app: &AppHandle, cmd: &str, args: Value) -> Result<Value, String> {
        let Some(daemon) = app.try_state::<Arc<DaemonState>>() else {
            return Err("store_unavailable: no daemon state yet".into());
        };
        let daemon = daemon.inner().clone();
        crate::commands::access::proxy(app, &daemon, cmd, args).await
    }

    fn pa_db(app: &AppHandle) -> Option<Arc<PaDb>> {
        app.try_state::<Arc<PaDb>>().map(|d| d.inner().clone())
    }

    struct Resolvers {
        app: AppHandle,
    }

    impl AskResolvers for Resolvers {
        fn resolve<'a>(
            &'a self,
            key: &'a AskKey,
            decision: Decision,
            _by: &'a DecidedBy,
        ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
            let gone = || {
                AccessError::new(
                    Code::Conflict,
                    "the ask timed out before the decision reached it",
                )
            };
            match key {
                AskKey::Hook { request_id } => Some(Box::pin(async move {
                    if crate::iyke::hooks::resolve_held(&self.app, request_id, decision.allows()) {
                        Ok(())
                    } else {
                        Err(gone())
                    }
                })),
                AskKey::Acp { request_id, .. } => Some(Box::pin(async move {
                    let Some(engine) = self
                        .app
                        .try_state::<crate::engines::claude_code::server::ClaudeCodeEngineState>(
                    ) else {
                        return Err(gone());
                    };
                    if engine.answer_permission(request_id, decision).await {
                        Ok(())
                    } else {
                        Err(gone())
                    }
                })),
                _ => None,
            }
        }
    }

    struct Host {
        app: AppHandle,
    }

    impl HostSide for Host {
        /// One `access_status` gives all three: the operator's effective caps
        /// already apply the routing preference (§1.4), so `approve` in them
        /// is `routing_ok` for the host device.
        fn host_routing(&self) -> BoxFuture<'_, Result<HostRouting, AccessError>> {
            Box::pin(async move {
                let st = daemon_rpc(&self.app, "access_status", json!({})).await;
                let answered = st.is_ok();
                let routing = routing_from_status(st, || store_on_disk(&self.app))?;
                if answered {
                    if let Ok(mut id) = identity().lock() {
                        *id = (routing.owner.clone(), routing.host_device.clone());
                    }
                }
                Ok(routing)
            })
        }

        fn audit_local(
            &self,
            kind: &'static str,
            target: String,
            detail: Value,
        ) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                let args = json!({ "kind": kind, "target": target, "detail": detail });
                if let Err(e) = daemon_rpc(&self.app, "access_audit_record_local", args).await {
                    // Dropped when the store is unavailable (§2.5); the arm
                    // body is WP-77's.
                    log::debug!(target: "ikenga::notifications", "audit {kind} not recorded: {e}");
                }
            })
        }
    }

    /// Whether the daemon's access store exists in this profile
    /// (`<app data>/daemon/access.db`, where `init_daemon` points
    /// `--data-dir`). Existence only; never opened here (P-20).
    fn store_on_disk(app: &AppHandle) -> bool {
        match app.path().app_data_dir() {
            Ok(dir) => dir
                .join("daemon")
                .join(crate::access::store::T0_FILE)
                .exists(),
            // Can't tell: assume it may exist (fail closed).
            Err(_) => true,
        }
    }

    /// [`Host::host_routing`]'s decision from the daemon's `access_status`
    /// reply (pure, so the fail-closed table is testable without an app).
    pub(super) fn routing_from_status(
        status: Result<Value, String>,
        store_on_disk: impl Fn() -> bool,
    ) -> Result<HostRouting, AccessError> {
        match status {
            Ok(st) => {
                let owner = st["principal"]["principalId"].as_str().map(str::to_string);
                let host = st["credential"]["deviceId"].as_str().map(str::to_string);
                // A daemon with no store admits only if no store exists to
                // hold a preference (as below).
                let no_store = st["store"].as_str() == Some("none") && !store_on_disk();
                let approve = st["caps"]
                    .as_array()
                    .is_some_and(|c| c.iter().any(|v| v.as_str() == Some("approve")));
                Ok(HostRouting {
                    routing_ok: no_store || approve,
                    owner,
                    host_device: host,
                })
            }
            // The daemon can't be asked. Only when no access store was ever
            // created (no daemon binary, the ephemeral fallback, a profile
            // that never ran one) can no preference exist, so the default
            // holds. A store on disk may hold `this_device` naming another
            // device, and its daemon being down doesn't lift it: fail closed
            // (§5.1, §1.4; review WP75-R3). The desktop never opens the store
            // itself (P-20) — it only checks that it exists.
            Err(e) if e.starts_with("store_unavailable") && !store_on_disk() => Ok(HostRouting {
                routing_ok: true,
                ..Default::default()
            }),
            Err(e) => Err(AccessError::new(
                Code::RoutingRefused,
                format!("couldn't read who may answer asks: {e}"),
            )),
        }
    }

    /// Install the in-process routing runtime and start the relay task.
    pub fn install(app: &AppHandle) {
        let Some(db) = pa_db(app) else {
            log::warn!(target: "ikenga::notifications", "no PaDb: permission routing is off");
            return;
        };
        let booted_at = chrono::Utc::now().timestamp_millis();
        core::install_local(LocalRouting {
            db: db.clone(),
            resolvers: Arc::new(Resolvers { app: app.clone() }),
            host: Arc::new(Host { app: app.clone() }),
        });
        {
            let app = app.clone();
            tauri::async_runtime::spawn(async move { sweep_orphans(app, db, booted_at).await });
        }
        let app = app.clone();
        tauri::async_runtime::spawn(async move { relay_loop(app).await });
    }

    /// The host side (§5.1) without the installed runtime: the same
    /// fail-closed routing read [`Host`] gives `permission_decide`. What
    /// `/iyke/hooks/decision`'s no-row fallback consults even when routing
    /// never installed (no `PaDb`; review WP75-RV1).
    pub fn host_side(app: &AppHandle) -> impl HostSide {
        Host { app: app.clone() }
    }

    /// Review WP75-RV2: close the hook / ACP asks the last desktop run left
    /// open and report each to the daemon (`permission_relay_resolve
    /// {outcome:'cancelled'}`), so a remote decision delivered just before a
    /// crash, never applied, is retracted from the daemon's chain. The daemon
    /// may not be up yet at boot: each report retries for about a minute.
    async fn sweep_orphans(app: AppHandle, db: Arc<PaDb>, booted_at: i64) {
        let Ok(pool) = db.ensure_pool().await else {
            return;
        };
        let keys = core::sweep_orphaned_asks(&pool, booted_at).await;
        for key in keys {
            let args = json!({ "key": key, "outcome": "cancelled" });
            let mut delay = Duration::from_secs(1);
            for attempt in 0..6 {
                match daemon_rpc(&app, "permission_relay_resolve", args.clone()).await {
                    Ok(_) => break,
                    Err(e) => {
                        log::debug!(
                            target: "ikenga::notifications",
                            "orphaned ask {key}: report attempt {attempt}: {e}"
                        );
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_secs(20));
                    }
                }
            }
        }
    }

    /// Long-poll `permission_relay_take` while any mirrored ask is open
    /// (and only then: pending relay asks are what keep the daemon alive).
    async fn relay_loop(app: AppHandle) {
        loop {
            let notified = wake().notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let open = pending().lock().map(|p| !p.is_empty()).unwrap_or(false);
            if !open {
                notified.await;
                continue;
            }
            let wait = core::RELAY_MAX_WAIT_MS;
            match daemon_rpc(&app, "permission_relay_take", json!({ "waitMs": wait })).await {
                Ok(v) => {
                    for d in v["decisions"].as_array().cloned().unwrap_or_default() {
                        apply(&app, &d).await;
                    }
                }
                Err(e) => {
                    log::debug!(target: "ikenga::notifications", "relay take: {e}");
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
            }
        }
    }

    async fn apply(app: &AppHandle, d: &Value) {
        let key = d["key"].as_str().unwrap_or_default().to_string();
        if let Ok(mut p) = pending().lock() {
            p.remove(&key);
        }
        let (Some(local), Some(db)) = (core::local(), pa_db(app)) else {
            return;
        };
        let Ok(pool) = db.ensure_pool().await else {
            return;
        };
        // The daemon names the decider (`decidedBy`); the cached owner is
        // only a fallback for an older daemon (review WP75-R4).
        let owner = identity().lock().ok().and_then(|i| i.0.clone());
        let outcome = match core::apply_relayed(&pool, local.resolvers.as_ref(), owner, d).await {
            Ok(true) => return,
            // The ask was already over here (timed out, or answered on
            // the host first): the remote decision never took effect.
            Ok(false) => "timed_out",
            Err(e) => {
                log::warn!(target: "ikenga::notifications", "relay decision for {key}: {e}");
                "cancelled"
            }
        };
        // Tell the daemon, so it retracts the decision it audited (review
        // WP75-R2: the chain never claims a decision that didn't happen).
        let args = json!({ "key": key, "outcome": outcome });
        if let Err(e) = daemon_rpc(app, "permission_relay_resolve", args).await {
            log::debug!(target: "ikenga::notifications", "relay resolve {key}: {e}");
        }
    }

    /// Record a `permission` row with its attribution (§5.7) and, for an
    /// answerable ask (`relay_expires_at_ms`), mirror it to the daemon so a
    /// paired device can answer it (§5.5 (a)). Best effort: a failed write
    /// never breaks the ask that produced it.
    pub async fn record_permission(
        app: &AppHandle,
        new: NewNotification,
        facts: AskFacts,
        relay_expires_at_ms: Option<i64>,
    ) -> Option<Notification> {
        let db = pa_db(app)?;
        let pool = match db.ensure_pool().await {
            Ok(p) => p,
            Err(e) => {
                log::warn!(target: "ikenga::notifications", "no db pool: {e}");
                return None;
            }
        };
        let project = match facts.cwd.as_deref() {
            Some(cwd) => core::project_for_path(&pool, cwd).await,
            None => None,
        };
        let attribution = super::producers::attribution(&facts, project.as_ref());
        let row = match core::record_ask(&pool, new, &attribution).await {
            Ok(r) => r?,
            Err(e) => {
                log::warn!(target: "ikenga::notifications", "could not record permission ask: {e}");
                return None;
            }
        };
        if let Some(expires) = relay_expires_at_ms {
            relay_put(app, &row, &attribution, expires).await;
        }
        Some(row)
    }

    async fn relay_put(app: &AppHandle, row: &Notification, a: &Attribution, expires: i64) {
        let Some(key) = row.dedupe_key.clone() else {
            return;
        };
        if !matches!(
            AskKey::parse(&key),
            AskKey::Hook { .. } | AskKey::Acp { .. }
        ) {
            return;
        }
        if let Ok(mut p) = pending().lock() {
            p.insert(key.clone());
        }
        let args = json!({
            "key": key,
            "title": row.title,
            "body": row.body,
            "projectId": a.project_id,
            "sensitive": a.sensitivity.level(),
            "requestedBy": a.requested_by,
            "expiresAtMs": expires,
        });
        match daemon_rpc(app, "permission_relay_put", args).await {
            Ok(_) => wake().notify_one(),
            Err(e) => {
                if let Ok(mut p) = pending().lock() {
                    p.remove(&key);
                }
                log::debug!(target: "ikenga::notifications", "ask stays desktop-only ({key}): {e}");
            }
        }
    }

    /// The desktop resolved or timed out `key` itself: close its mirror
    /// (`decided_on_host` | `timed_out` | `cancelled`). No-op for an ask
    /// that was never mirrored, or was decided remotely.
    pub fn relay_resolved(app: &AppHandle, key: &str, outcome: &'static str) {
        let was = pending().lock().map(|mut p| p.remove(key)).unwrap_or(false);
        if !was {
            return;
        }
        let app = app.clone();
        let key = key.to_string();
        tauri::async_runtime::spawn(async move {
            let args = json!({ "key": key, "outcome": outcome });
            if let Err(e) = daemon_rpc(&app, "permission_relay_resolve", args).await {
                log::debug!(target: "ikenga::notifications", "relay resolve {key}: {e}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review WP75-RV1 / WP75-R3: the host's routing read fails closed — a
    /// daemon that can't be asked admits only when no access store exists
    /// to hold a preference; `approve` in the operator's caps is the
    /// answer otherwise. `/iyke/hooks/decision`'s no-row fallback reads it
    /// whether or not the routing runtime installed.
    #[test]
    fn host_routing_fails_closed() {
        use crate::access::Code;
        use routing_desktop::routing_from_status;
        use serde_json::json;
        let st = |caps: &[&str], store: &str| {
            Ok(json!({
                "store": store,
                "caps": caps,
                "principal": {"principalId": "owner"},
                "credential": {"deviceId": "host"},
            }))
        };
        let ok = routing_from_status(st(&["approve"], "t0"), || true).unwrap();
        assert!(ok.routing_ok);
        assert_eq!(ok.owner.as_deref(), Some("owner"));
        assert_eq!(ok.host_device.as_deref(), Some("host"));
        assert!(
            !routing_from_status(st(&["files"], "t0"), || true)
                .unwrap()
                .routing_ok
        );
        // A store-less daemon admits only while no store is on disk.
        assert!(
            routing_from_status(st(&[], "none"), || false)
                .unwrap()
                .routing_ok
        );
        assert!(
            !routing_from_status(st(&[], "none"), || true)
                .unwrap()
                .routing_ok
        );
        // No daemon at all.
        let down = || Err::<serde_json::Value, _>("store_unavailable: no daemon".to_string());
        assert!(routing_from_status(down(), || false).unwrap().routing_ok);
        assert_eq!(
            routing_from_status(down(), || true).unwrap_err().code,
            Code::RoutingRefused
        );
        assert_eq!(
            routing_from_status(Err("connection refused".into()), || false)
                .unwrap_err()
                .code,
            Code::RoutingRefused
        );
    }

    #[tokio::test]
    async fn resolve_installed_updates_resolves_only_installed_versions() {
        use super::producers::{update, UpdateSource};
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = crate::db::PaDb::new(tmp.path().join("ikenga.db"));
        let pool = db.ensure_pool().await.expect("pool");
        for n in [
            update(UpdateSource::Shell, "0.13.0", None, None).unwrap(),
            update(UpdateSource::Shell, "0.14.0", None, None).unwrap(),
            update(UpdateSource::Pkg, "1.2.0", Some("com.ikenga.tasks"), None).unwrap(),
            update(UpdateSource::Pkg, "2.0.0", Some("com.ikenga.iyke"), None).unwrap(),
        ] {
            record(&pool, n).await.unwrap();
        }
        let installed = vec![
            ("com.ikenga.tasks".to_string(), "1.2.0".to_string()),
            ("com.ikenga.iyke".to_string(), "1.9.9".to_string()),
        ];
        let n = resolve_installed_updates(&pool, Some("0.13.0"), &installed)
            .await
            .unwrap();
        assert_eq!(n, 2);
        let open: Vec<String> = list(&pool, &ListQuery::default())
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.resolved_at.is_none())
            .filter_map(|r| r.dedupe_key)
            .collect();
        assert_eq!(open.len(), 2);
        assert!(open.contains(&"update:shell:0.14.0".to_string()));
        assert!(open.contains(&"update:pkg:com.ikenga.iyke@2.0.0".to_string()));
        assert_eq!(
            unread_count(&pool, &[])
                .await
                .unwrap()
                .by_kind
                .get("update"),
            Some(&2)
        );
    }
}
