//! Per-kind mute prefs (WP-40).
//!
//! # Where they live, and why
//!
//! `~/.ikenga/settings.json` → `workspace.notifications.mutedKinds`, a string
//! array, **personal-only** (a committed project file must not silence your
//! permission asks — and it could not anyway, see below). Written through the
//! WP-32 settings substrate (`SettingsManager::write_field`), read straight
//! from the personal file (`SettingsManager::personal_field`) so list / count
//! calls do not trigger a cache refresh.
//!
//! Round 31/32 left the choice between this and a `shell-store.ts` v17→v18
//! `migrateShellStore` bump to WP-40. Settings won because (a) DEC-44 makes
//! settings files the source of truth for user prefs and the D-07 design
//! points "Notification settings →" at Settings · Workspace; (b) the Rust
//! side must read mutes (bridge, event forwarder) and cannot read the
//! webview's persisted Zustand store; (c) it keeps `migrateShellStore` single-
//! writer per branch without spending 5b's one bump on a two-field slice.
//!
//! # Tolerant read
//!
//! The schema accepts any string array so a hand-edited file never fails to
//! parse over a typo. [`parse_muted`] drops unknown kinds and the two
//! unmutable ones (`permission`, `violation`) at read time; the mute command
//! refuses them at write time.

use std::sync::{Arc, OnceLock};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use super::NotificationKind;
use crate::settings::{SettingsManager, SettingsScope};

/// The settings field holding the muted kinds.
pub const MUTED_KINDS_FIELD: &str = "workspace.notifications.mutedKinds";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuteState {
    /// Currently muted kinds (mutable kinds only, deduped, stable order).
    pub muted: Vec<NotificationKind>,
    /// Kinds that CAN be muted — everything but permission and violation.
    pub mutable: Vec<NotificationKind>,
}

impl MuteState {
    pub fn from_muted(muted: Vec<NotificationKind>) -> Self {
        Self {
            muted,
            mutable: NotificationKind::ALL
                .into_iter()
                .filter(|k| k.is_mutable())
                .collect(),
        }
    }
}

/// Parse the settings value. Unknown strings and unmutable kinds are ignored;
/// the result is deduped and sorted in `NotificationKind::ALL` order.
pub fn parse_muted(value: Option<&Value>) -> Vec<NotificationKind> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let wanted: Vec<NotificationKind> = items
        .iter()
        .filter_map(Value::as_str)
        .filter_map(|raw| NotificationKind::parse(raw).ok())
        .filter(|k| k.is_mutable())
        .collect();
    NotificationKind::ALL
        .into_iter()
        .filter(|k| wanted.contains(k))
        .collect()
}

/// Apply one mute / unmute to a muted list. Refuses unmutable kinds.
pub fn apply(
    current: &[NotificationKind],
    kind: NotificationKind,
    muted: bool,
) -> Result<Vec<NotificationKind>, String> {
    if muted && !kind.is_mutable() {
        return Err(format!("{} notifications cannot be muted", kind.as_str()));
    }
    Ok(NotificationKind::ALL
        .into_iter()
        .filter(|k| {
            if *k == kind {
                muted
            } else {
                current.contains(k)
            }
        })
        .collect())
}

/// Serialize for the settings file.
pub fn to_value(kinds: &[NotificationKind]) -> Value {
    Value::Array(
        kinds
            .iter()
            .map(|k| Value::String(k.as_str().to_string()))
            .collect(),
    )
}

/// Current muted kinds. Any read failure degrades to "nothing muted" with a
/// warning — a broken settings file must not hide notifications.
pub fn muted_kinds(manager: &SettingsManager) -> Vec<NotificationKind> {
    match manager.personal_field(MUTED_KINDS_FIELD) {
        Ok(value) => parse_muted(value.as_ref()),
        Err(e) => {
            log::warn!(
                target: "ikenga::notifications",
                "could not read {MUTED_KINDS_FIELD}; treating nothing as muted: {e}"
            );
            Vec::new()
        }
    }
}

/// [`muted_kinds`] via the app's managed `SettingsManager`, for code that only
/// holds an `AppHandle` (event forwarder, iyke bridge).
pub fn muted_kinds_for_app(app: &tauri::AppHandle) -> Vec<NotificationKind> {
    use tauri::Manager;
    match app.try_state::<Arc<SettingsManager>>() {
        Some(manager) => muted_kinds(manager.inner()),
        None => Vec::new(),
    }
}

/// Serializes read-modify-write of the muted list so two quick toggles from
/// the popover cannot lose one another.
static MUTE_WRITE: OnceLock<Mutex<()>> = OnceLock::new();

/// The muted set last published, so the settings listener can tell a real
/// change from a write of the same list. `None` until first seen.
static LAST_MUTED: std::sync::Mutex<Option<Vec<NotificationKind>>> = std::sync::Mutex::new(None);

fn remember_muted(kinds: &[NotificationKind]) {
    if let Ok(mut last) = LAST_MUTED.lock() {
        *last = Some(kinds.to_vec());
    }
}

// ─── Mute timestamps (`notification_mutes`, migration 0066) ─────────────────

/// Record that `kind` became muted now. Keeps the original time if it is
/// already recorded (muting twice does not move the window).
pub async fn note_muted(pool: &sqlx::SqlitePool, kind: NotificationKind) -> Result<(), String> {
    sqlx::query("INSERT OR IGNORE INTO notification_mutes (kind, muted_at) VALUES (?, ?)")
        .bind(kind.as_str())
        .bind(chrono::Utc::now().timestamp_millis())
        .execute(pool)
        .await
        .map_err(|e| format!("notification_mutes insert: {e}"))?;
    Ok(())
}

/// Un-mute bookkeeping: mark read the unread rows of `kind` recorded WHILE it
/// was muted (`updated_at >= muted_at`) and forget the mute time. Rows that
/// were already unread before the mute stay unread. With no recorded mute
/// time, nothing is marked read. Returns rows marked read.
pub async fn clear_muted_backlog(
    pool: &sqlx::SqlitePool,
    kind: NotificationKind,
) -> Result<u64, String> {
    let muted_at: Option<i64> =
        sqlx::query_scalar("SELECT muted_at FROM notification_mutes WHERE kind = ?")
            .bind(kind.as_str())
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("notification_mutes read: {e}"))?;
    let mut changed = 0;
    if let Some(since) = muted_at {
        changed = sqlx::query(
            "UPDATE shell_notifications SET read_at = ?
             WHERE read_at IS NULL AND kind = ? AND updated_at >= ?",
        )
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(kind.as_str())
        .bind(since)
        .execute(pool)
        .await
        .map_err(|e| format!("notifications unmute backlog: {e}"))?
        .rows_affected();
    }
    sqlx::query("DELETE FROM notification_mutes WHERE kind = ?")
        .bind(kind.as_str())
        .execute(pool)
        .await
        .map_err(|e| format!("notification_mutes delete: {e}"))?;
    if changed > 0 {
        super::publish(super::ChangeReason::ReadAll, None);
    }
    Ok(changed)
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
            super::publish(super::ChangeReason::MuteChanged, None);
        });
    });
}

/// Mute or unmute one kind in `settings.json`. Returns the new state.
pub async fn set_muted(
    manager: &SettingsManager,
    kind: NotificationKind,
    muted: bool,
) -> Result<MuteState, String> {
    let _guard = MUTE_WRITE.get_or_init(|| Mutex::new(())).lock().await;
    let current = manager
        .personal_field(MUTED_KINDS_FIELD)
        .map(|v| parse_muted(v.as_ref()))?;
    let next = apply(&current, kind, muted)?;
    if next != current {
        manager
            .write_field(
                SettingsScope::Personal,
                None,
                MUTED_KINDS_FIELD,
                to_value(&next),
                false,
            )
            .await?;
    }
    // The settings watcher will see this write; remembering the new set first
    // keeps its listener from repeating the bookkeeping.
    remember_muted(&next);
    super::publish(super::ChangeReason::MuteChanged, None);
    Ok(MuteState::from_muted(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_muted_ignores_unknown_and_unmutable_kinds() {
        let v = json!(["update", "permission", "violation", "nope", 3, "update", "invite"]);
        assert_eq!(
            parse_muted(Some(&v)),
            vec![NotificationKind::Update, NotificationKind::Invite]
        );
        assert!(parse_muted(None).is_empty());
        assert!(parse_muted(Some(&json!({"update": true}))).is_empty());
    }

    #[test]
    fn apply_mutes_and_unmutes_in_stable_order() {
        let one = apply(&[], NotificationKind::Update, true).unwrap();
        let two = apply(&one, NotificationKind::RunFinished, true).unwrap();
        assert_eq!(
            two,
            vec![NotificationKind::RunFinished, NotificationKind::Update]
        );
        let back = apply(&two, NotificationKind::Update, false).unwrap();
        assert_eq!(back, vec![NotificationKind::RunFinished]);
        // Idempotent.
        assert_eq!(apply(&back, NotificationKind::RunFinished, true).unwrap(), back);
    }

    #[test]
    fn apply_refuses_to_mute_permission_or_violation() {
        assert!(apply(&[], NotificationKind::Permission, true).is_err());
        assert!(apply(&[], NotificationKind::Violation, true).is_err());
        // Unmuting one is harmless (a hand-edited file may list it).
        assert!(apply(&[], NotificationKind::Violation, false).is_ok());
    }

    #[test]
    fn mute_state_lists_the_four_mutable_kinds() {
        let s = MuteState::from_muted(vec![]);
        assert_eq!(
            s.mutable,
            vec![
                NotificationKind::RunFinished,
                NotificationKind::RunFailed,
                NotificationKind::Update,
                NotificationKind::Invite
            ]
        );
        assert_eq!(
            to_value(&[NotificationKind::Update]),
            json!(["update"])
        );
    }
}
