//! `GET /iyke/notifications` — read-only bridge view of the WP-40
//! notifications table (the D-07 footer's `iyke notifications list --unread`).
//!
//! Query: `?unread=true&kind=update,run_failed&limit=50&includeMuted=true`.
//! Response: `{ items, unread: { total, byKind }, mutedKinds }` — muted kinds
//! are hidden from `items` and `unread` unless `includeMuted=true`.
//!
//! Marking read and muting are deliberately NOT on the bridge: they are the
//! user's attention state, changed from the notification centre (the same
//! stance `iyke::permissions_audit` takes on clearing violations).

use std::sync::Arc;

use axum::{extract::Query, http::StatusCode, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::commands::db::PaDb;
use crate::notifications::{self, mute, ListQuery, NotificationKind};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsParams {
    pub unread: Option<bool>,
    /// Comma-separated kinds.
    pub kind: Option<String>,
    pub limit: Option<i64>,
    pub include_muted: Option<bool>,
}

fn parse_kind_filter(raw: Option<&str>) -> Result<Option<Vec<NotificationKind>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(NotificationKind::parse)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub async fn get_notifications(
    Extension(app): Extension<AppHandle>,
    Extension(db): Extension<Arc<PaDb>>,
    Query(params): Query<NotificationsParams>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let kinds =
        parse_kind_filter(params.kind.as_deref()).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let muted = mute::muted_kinds_for_app(&app);
    let exclude = if params.include_muted.unwrap_or(false) {
        Vec::new()
    } else {
        muted.clone()
    };
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let items = notifications::list(
        &pool,
        &ListQuery {
            unread_only: params.unread.unwrap_or(false),
            kinds,
            exclude: exclude.clone(),
            limit: params.limit,
            before: None,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let unread = notifications::unread_count(&pool, &exclude)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(json!({
        "items": items,
        "unread": unread,
        "mutedKinds": muted,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_filter_parses_a_comma_list() {
        assert_eq!(parse_kind_filter(None).unwrap(), None);
        assert_eq!(parse_kind_filter(Some("  ")).unwrap(), None);
        assert_eq!(
            parse_kind_filter(Some("update, run_failed")).unwrap(),
            Some(vec![NotificationKind::Update, NotificationKind::RunFailed])
        );
        assert!(parse_kind_filter(Some("update,nope")).is_err());
    }

    #[test]
    fn params_use_camel_case() {
        let p: NotificationsParams =
            serde_json::from_value(json!({ "unread": true, "includeMuted": true })).unwrap();
        assert_eq!(p.unread, Some(true));
        assert_eq!(p.include_muted, Some(true));
    }
}
