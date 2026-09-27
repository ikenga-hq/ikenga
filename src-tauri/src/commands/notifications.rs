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
use crate::notifications::{self, mute, ops, producers, Notification, UnreadCount};
use crate::settings::SettingsManager;

// The bodies live in `crate::server::shared::notifications::ops` (WP-19
// slice 4), shared with the daemon's `/api/rpc` arms; these stay thin.

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
    let args = ops::ListArgs {
        unread_only,
        kinds,
        limit,
        before,
        include_muted,
    };
    ops::list(&db, || Ok(mute::muted_kinds(settings.inner())), args).await
}

/// Unread total + per-kind counts, muted kinds excluded. Backs the bell badge
/// (WP-40b) and the daily address (WP-39).
#[tauri::command]
pub async fn notifications_unread_count(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
) -> Result<UnreadCount, String> {
    let muted = mute::muted_kinds(settings.inner());
    ops::unread_count(&db, &muted).await
}

/// Mark rows read. Returns how many changed.
#[tauri::command]
pub async fn notifications_mark_read(
    db: State<'_, Arc<PaDb>>,
    ids: Vec<i64>,
) -> Result<u64, String> {
    ops::mark_read(&db, &ids).await
}

/// Mark every unread row read (optionally one kind). Muted rows included —
/// "Mark all read" means all.
#[tauri::command]
pub async fn notifications_mark_all_read(
    db: State<'_, Arc<PaDb>>,
    kind: Option<String>,
) -> Result<u64, String> {
    ops::mark_all_read(&db, kind).await
}

/// Current mute state (muted kinds + which kinds can be muted).
#[tauri::command]
pub async fn notifications_mute_state(
    settings: State<'_, Arc<SettingsManager>>,
) -> Result<mute::MuteState, String> {
    Ok(ops::mute_state(settings.inner()))
}

/// Mute one kind (writes settings.json). `permission` and `violation` are
/// refused. The mute time is recorded so un-muting can tell rows recorded
/// while muted from an older backlog.
#[tauri::command]
pub async fn notifications_mute_kind(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
    kind: String,
) -> Result<mute::MuteState, String> {
    ops::mute_kind(&db, settings.inner(), kind).await
}

/// Unmute one kind. Only rows of that kind recorded while it was muted
/// (`updated_at >=` the mute time) are marked read first, so un-muting does
/// not dump a backlog onto the badge; rows already unread before the mute
/// stay unread.
#[tauri::command]
pub async fn notifications_unmute_kind(
    db: State<'_, Arc<PaDb>>,
    settings: State<'_, Arc<SettingsManager>>,
    kind: String,
) -> Result<mute::MuteState, String> {
    ops::unmute_kind(&db, settings.inner(), kind).await
}

/// `update` producer for the two update checks that live in the webview:
/// the app updater (`src/lib/updater/updater.ts`, tauri-plugin-updater's JS
/// `check()`) and the pkg registry cross-reference (`usePkgsDerived`). The FE
/// passes only facts; copy, action and the once-per-version dedupe key are
/// built in Rust (`producers::update`) so the webview cannot mint other kinds.
/// Returns the row when one was created, `None` when that version was already
/// announced.
///
/// Every call also sweeps `update` rows whose version is now installed
/// ([`notifications::resolve_installed_updates`]), so a pkg updated in-session
/// stops reading as an open update on the next check without the updater UI
/// having to report installs.
#[tauri::command]
pub async fn notifications_record_update(
    app: tauri::AppHandle,
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
    sweep_installed_updates(&app, &pool).await;
    Ok(outcome.notification().cloned())
}

/// Installed versions the update sweep compares against: the running shell
/// and every installed pkg.
fn installed_versions(app: &tauri::AppHandle) -> (String, Vec<(String, String)>) {
    use tauri::Manager;
    let shell = app.package_info().version.to_string();
    let pkgs = app
        .try_state::<crate::commands::pkg::KernelState>()
        .map(|k| {
            k.0.list_installed()
                .into_iter()
                .map(|s| (s.id, s.version))
                .collect()
        })
        .unwrap_or_default();
    (shell, pkgs)
}

async fn sweep_installed_updates(app: &tauri::AppHandle, pool: &sqlx::SqlitePool) {
    let (shell, pkgs) = installed_versions(app);
    if let Err(e) = notifications::resolve_installed_updates(pool, Some(&shell), &pkgs).await {
        log::warn!(target: "ikenga::notifications", "update sweep failed: {e}");
    }
}

/// Boot-time update sweep: an app update relaunches into this, resolving the
/// "Ikenga <v> is available" row it answered. Call once the pkg kernel is
/// managed. Best-effort, off the setup thread.
pub fn spawn_boot_update_sweep(app: tauri::AppHandle) {
    use tauri::Manager;
    tauri::async_runtime::spawn(async move {
        let Some(db) = app.try_state::<Arc<PaDb>>().map(|d| d.inner().clone()) else {
            return;
        };
        match db.ensure_pool().await {
            Ok(pool) => sweep_installed_updates(&app, &pool).await,
            Err(e) => log::warn!(target: "ikenga::notifications", "no db pool: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::NotificationKind;

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
