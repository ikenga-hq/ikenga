//! Desktop facade over the per-kind mute prefs (WP-40).
//!
//! The implementation — parse / apply / `muted_kinds` / `set_muted` and the
//! `notification_mutes` bookkeeping — lives in
//! [`crate::server::shared::notifications::mute`], compiled into both binaries
//! so the daemon's `notifications_mute_*` arms run the same code (WP-19
//! slice 4). This module keeps every `crate::notifications::mute::…` path the
//! desktop already uses and adds the two `AppHandle`-bound pieces: the
//! managed-state lookup and the `settings://changed` listener. The daemon has
//! no settings event to listen to, so a hand edit of `mutedKinds` there is
//! picked up by the next read (reads always go to the file) without the
//! backlog bookkeeping this listener adds.

use std::sync::Arc;

pub use crate::server::shared::notifications::mute::*;

use crate::server::shared::notifications::{publish, ChangeReason, NotificationKind};
use crate::settings::SettingsManager;

/// [`muted_kinds`] via the app's managed `SettingsManager`, for code that only
/// holds an `AppHandle` (event forwarder, iyke bridge).
pub fn muted_kinds_for_app(app: &tauri::AppHandle) -> Vec<NotificationKind> {
    use tauri::Manager;
    match app.try_state::<Arc<SettingsManager>>() {
        Some(manager) => muted_kinds(manager.inner()),
        None => Vec::new(),
    }
}

/// Hand edits to `workspace.notifications.mutedKinds` publish no command, so
/// listen to `settings://changed` and, when the muted set really changed,
/// do the same bookkeeping as the mute / unmute commands and publish
/// `mute_changed` (the FE mute-state + list queries refetch on it).
pub fn spawn_settings_listener(app: tauri::AppHandle) {
    use tauri::{Listener, Manager};
    remember_muted(&muted_kinds_for_app(&app));
    let handle = app.clone();
    let _ = app.listen("settings://changed", move |_evt| {
        let app = handle.clone();
        tauri::async_runtime::spawn(async move {
            let now = muted_kinds_for_app(&app);
            let before = {
                let Ok(mut last) = LAST_MUTED.lock() else { return };
                let before = last.clone().unwrap_or_default();
                if before == now {
                    return;
                }
                *last = Some(now.clone());
                before
            };
            let db = app
                .try_state::<Arc<crate::commands::db::PaDb>>()
                .map(|d| d.inner().clone());
            if let Some(db) = db {
                if let Ok(pool) = db.ensure_pool().await {
                    for kind in now.iter().filter(|k| !before.contains(k)) {
                        if let Err(e) = note_muted(&pool, *kind).await {
                            log::warn!(target: "ikenga::notifications", "{e}");
                        }
                    }
                    for kind in before.iter().filter(|k| !now.contains(k)) {
                        if let Err(e) = clear_muted_backlog(&pool, *kind).await {
                            log::warn!(target: "ikenga::notifications", "{e}");
                        }
                    }
                }
            }
            publish(ChangeReason::MuteChanged, None);
        });
    });
}
