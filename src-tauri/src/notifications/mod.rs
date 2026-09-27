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
//!   the desktop-only Claude Code engine (`engines::claude_code::notify`).

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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(unread_count(&pool, &[]).await.unwrap().by_kind.get("update"), Some(&2));
    }
}
