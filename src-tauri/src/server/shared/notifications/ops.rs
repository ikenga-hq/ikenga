//! The `notifications_*` command bodies, shared by the desktop
//! `#[tauri::command]` wrappers (`commands::notifications`) and the daemon's
//! `/api/rpc` arms (`server::rpc_shell`), WP-19 slice 4.
//!
//! Each takes what its caller resolved — the `PaDb`, the `SettingsManager`
//! that owns `mutedKinds` — and does exactly what the desktop command did,
//! in the same order (argument checks, then the mute read, then the pool), so
//! the two surfaces return the same value or the same error.
//!
//! `notifications_record_update` is not here: its installed-version sweep
//! needs the running shell's `package_info` and the live pkg kernel, which
//! only the desktop has (see `desktop_only.toml`).

use super::mute::{self, MuteState};
use super::{ListQuery, Notification, NotificationKind, UnreadCount};
use crate::db::PaDb;
use crate::server::shared::settings::SettingsManager;

/// `kinds: Option<Vec<String>>` → parsed kinds; an unknown kind is an error.
pub fn parse_kinds(kinds: Option<Vec<String>>) -> Result<Option<Vec<NotificationKind>>, String> {
    kinds
        .map(|ks| ks.iter().map(|k| NotificationKind::parse(k)).collect())
        .transpose()
}

/// `notifications_list`'s arguments, as the command receives them.
#[derive(Debug, Default)]
pub struct ListArgs {
    pub unread_only: Option<bool>,
    pub kinds: Option<Vec<String>>,
    pub limit: Option<i64>,
    pub before: Option<i64>,
    pub include_muted: Option<bool>,
}

/// List notifications, newest first. Muted kinds are hidden unless
/// `include_muted` is true; `muted` is only consulted when they are hidden
/// (the desktop's reader never fails — it degrades to "nothing muted" — while
/// a daemon without a settings store says so rather than showing muted rows).
pub async fn list(
    db: &PaDb,
    muted: impl FnOnce() -> Result<Vec<NotificationKind>, String>,
    args: ListArgs,
) -> Result<Vec<Notification>, String> {
    let exclude = if args.include_muted.unwrap_or(false) {
        Vec::new()
    } else {
        muted()?
    };
    let query = ListQuery {
        unread_only: args.unread_only.unwrap_or(false),
        kinds: parse_kinds(args.kinds)?,
        exclude,
        limit: args.limit,
        before: args.before,
    };
    let pool = db.ensure_pool().await?;
    super::list(&pool, &query).await
}

/// Unread total + per-kind counts, `muted` excluded.
pub async fn unread_count(db: &PaDb, muted: &[NotificationKind]) -> Result<UnreadCount, String> {
    let pool = db.ensure_pool().await?;
    super::unread_count(&pool, muted).await
}

/// Mark rows read. Returns how many changed.
pub async fn mark_read(db: &PaDb, ids: &[i64]) -> Result<u64, String> {
    let pool = db.ensure_pool().await?;
    super::mark_read(&pool, ids).await
}

/// Mark every unread row read (optionally one kind). Muted rows included.
pub async fn mark_all_read(db: &PaDb, kind: Option<String>) -> Result<u64, String> {
    let kind = kind.as_deref().map(NotificationKind::parse).transpose()?;
    let pool = db.ensure_pool().await?;
    super::mark_all_read(&pool, kind).await
}

/// Current mute state (muted kinds + which kinds can be muted).
pub fn mute_state(settings: &SettingsManager) -> MuteState {
    MuteState::from_muted(mute::muted_kinds(settings))
}

/// Mute one kind (writes the personal `settings.json`), then note the mute
/// time — best-effort, as on the desktop: without it un-muting just marks
/// nothing read.
pub async fn mute_kind(
    db: &PaDb,
    settings: &SettingsManager,
    kind: String,
) -> Result<MuteState, String> {
    let kind = NotificationKind::parse(&kind)?;
    let state = mute::set_muted(settings, kind, true).await?;
    match db.ensure_pool().await {
        Ok(pool) => {
            if let Err(e) = mute::note_muted(&pool, kind).await {
                log::warn!(target: "ikenga::notifications", "{e}");
            }
        }
        Err(e) => log::warn!(target: "ikenga::notifications", "no db pool: {e}"),
    }
    Ok(state)
}

/// Unmute one kind. Rows of that kind recorded while it was muted are marked
/// read first; rows unread before the mute stay unread.
pub async fn unmute_kind(
    db: &PaDb,
    settings: &SettingsManager,
    kind: String,
) -> Result<MuteState, String> {
    let kind = NotificationKind::parse(&kind)?;
    let was_muted = mute::muted_kinds(settings).contains(&kind);
    if was_muted {
        let pool = db.ensure_pool().await?;
        mute::clear_muted_backlog(&pool, kind).await?;
    }
    mute::set_muted(settings, kind, false).await
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
}
