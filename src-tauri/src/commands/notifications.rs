//! `notifications_*` Tauri commands (WP-40).
//!
//! Thin wrappers over `crate::notifications`. Reads apply the per-kind mute
//! from settings.json (`workspace.notifications.mutedKinds`) unless the caller
//! asks for muted rows explicitly. Every command here is listed in
//! `permissions/app-commands.toml` (acl-parity).
//!
//! The FE contract is `src/lib/tauri-cmd.ts` (`notifications*` wrappers) and
//! `src/lib/queries/notifications.ts` (query options + mutations).

use std::sync::Arc;

use tauri::State;

use crate::commands::db::PaDb;
use crate::notifications::{
    self, mute, producers, ListQuery, Notification, NotificationKind, UnreadCount,
};
use crate::settings::SettingsManager;

fn parse_kinds(kinds: Option<Vec<String>>) -> Result<Option<Vec<NotificationKind>>, String> {
    kinds
        .map(|ks| ks.iter().map(|k| NotificationKind::parse(k)).collect())
        .transpose()
}

/// List notifications, newest first. Muted kinds are hidden unless
/// `includeMuted` is true.
#[tauri::command]
pub async fn notifications_list(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
    unread_only: Option<bool>,
    kinds: Option<Vec<String>>,
    limit: Option<i64>,
    before: Option<i64>,
    include_muted: Option<bool>,
) -> Result<Vec<Notification>, String> {
    let exclude = if include_muted.unwrap_or(false) {
        Vec::new()
    } else {
        mute::muted_kinds(settings.inner())
    };
    let query = ListQuery {
        unread_only: unread_only.unwrap_or(false),
        kinds: parse_kinds(kinds)?,
        exclude,
        limit,
        before,
    };
    let pool = db.ensure_pool().await?;
    notifications::list(&pool, &query).await
}

/// Unread total + per-kind counts, muted kinds excluded. Backs the bell badge
/// (WP-40b) and the daily address (WP-39).
#[tauri::command]
pub async fn notifications_unread_count(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
) -> Result<UnreadCount, String> {
    let muted = mute::muted_kinds(settings.inner());
    let pool = db.ensure_pool().await?;
    notifications::unread_count(&pool, &muted).await
}

/// Mark rows read. Returns how many changed.
#[tauri::command]
pub async fn notifications_mark_read(
    db: State<'_, Arc<PaDb>>,
    ids: Vec<i64>,
) -> Result<u64, String> {
    let pool = db.ensure_pool().await?;
    notifications::mark_read(&pool, &ids).await
}

/// Mark every unread row read (optionally one kind). Muted rows included —
/// "Mark all read" means all.
#[tauri::command]
pub async fn notifications_mark_all_read(
    db: State<'_, Arc<PaDb>>,
    kind: Option<String>,
) -> Result<u64, String> {
    let kind = kind.as_deref().map(NotificationKind::parse).transpose()?;
    let pool = db.ensure_pool().await?;
    notifications::mark_all_read(&pool, kind).await
}

/// Current mute state (muted kinds + which kinds can be muted).
#[tauri::command]
pub async fn notifications_mute_state(
    settings: State<'_, Arc<SettingsManager>>,
) -> Result<mute::MuteState, String> {
    Ok(mute::MuteState::from_muted(mute::muted_kinds(
        settings.inner(),
    )))
}

/// Mute one kind (writes settings.json). `permission` and `violation` are
/// refused.
#[tauri::command]
pub async fn notifications_mute_kind(
    settings: State<'_, Arc<SettingsManager>>,
    kind: String,
) -> Result<mute::MuteState, String> {
    let kind = NotificationKind::parse(&kind)?;
    mute::set_muted(settings.inner(), kind, true).await
}

/// Unmute one kind. Rows of that kind recorded while it was muted are marked
/// read first, so un-muting does not dump a backlog onto the badge.
#[tauri::command]
pub async fn notifications_unmute_kind(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
    kind: String,
) -> Result<mute::MuteState, String> {
    let kind = NotificationKind::parse(&kind)?;
    let was_muted = mute::muted_kinds(settings.inner()).contains(&kind);
    if was_muted {
        let pool = db.ensure_pool().await?;
        notifications::mark_all_read(&pool, Some(kind)).await?;
    }
    mute::set_muted(settings.inner(), kind, false).await
}

/// `update` producer for the two update checks that live in the webview:
/// the app updater (`src/lib/updater/updater.ts`, tauri-plugin-updater's JS
/// `check()`) and the pkg registry cross-reference (`usePkgsDerived`). The FE
/// passes only facts; copy, action and the once-per-version dedupe key are
/// built in Rust (`producers::update`) so the webview cannot mint other kinds.
/// Returns the row when one was created, `None` when that version was already
/// announced.
#[tauri::command]
pub async fn notifications_record_update(
    db: State<'_, Arc<PaDb>>,
    source: String,
    version: String,
    pkg_id: Option<String>,
    pkg_name: Option<String>,
) -> Result<Option<Notification>, String> {
    let source = producers::UpdateSource::parse(&source)?;
    let new = producers::update(source, &version, pkg_id.as_deref(), pkg_name.as_deref())?;
    let pool = db.ensure_pool().await?;
    let outcome = notifications::record(&pool, new).await?;
    Ok(outcome.notification().cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kinds_accepts_known_and_rejects_unknown() {
        assert_eq!(parse_kinds(None).unwrap(), None);
        assert_eq!(
            parse_kinds(Some(vec!["update".into(), "run_failed".into()])).unwrap(),
            Some(vec![NotificationKind::Update, NotificationKind::RunFailed])
        );
        assert!(parse_kinds(Some(vec!["toast".into()])).is_err());
    }

    /// The command-level `update` path: what the FE's two update checks call.
    #[tokio::test]
    async fn record_update_is_once_per_version() {
        let tmp = tempfile::tempdir().unwrap();
        let db = PaDb::new(tmp.path().join("ikenga.db"));
        let pool = db.ensure_pool().await.unwrap();
        let mk = || producers::update(producers::UpdateSource::Shell, "0.9.1", None, None).unwrap();
        let first = notifications::record(&pool, mk()).await.unwrap();
        assert!(first.notification().is_some());
        let again = notifications::record(&pool, mk()).await.unwrap();
        assert!(again.notification().is_none());
        let c = notifications::unread_count(&pool, &[]).await.unwrap();
        assert_eq!(c.by_kind.get("update"), Some(&1));
        // Muting `update` hides it from the unread count.
        let c = notifications::unread_count(&pool, &[NotificationKind::Update])
            .await
            .unwrap();
        assert_eq!(c.total, 0);
    }
}
