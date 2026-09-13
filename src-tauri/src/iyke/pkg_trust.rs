//! Localhost HTTP relay for the per-folder Studio project-access trust gate
//! (WP-04).
//!
//! A sidecar is a separate OS process: it cannot `invoke()` a Tauri command.
//! This route is the transport that lets `com.ikenga.studio`'s project sidecar
//! reach the *same* decision the iframe reaches through
//! `commands::pkg_studio::request_project_access` — canonicalize, return `true`
//! with no prompt on an existing grant row, otherwise pop the native dialog.
//!
//! # Auth
//!
//! Mounted on the **unauthed** router next to `/iyke/pkg-db/*`, with its own
//! credential check, for exactly the same reason: the global iyke bearer token
//! grants terminals, secrets and the whole control surface. The caller instead
//! presents its per-pkg `IKENGA_PKG_DB_TOKEN`, minted at spawn by
//! `pkg::db_scope::inject_env` and injected into the child's environment — the
//! one credential the forgeable side (iframe, MCP tool arguments, iyke pkg
//! route) never sees.
//!
//! This endpoint is **single-pkg by design**: the grant rows it can produce are
//! keyed to [`crate::pkg::trust::STUDIO_PKG_ID`], so any other pkg's token is
//! refused with 403 rather than silently writing a Studio-scoped grant.
//!
//! # Wire
//!
//! ```text
//! POST /iyke/pkg-trust/project-access
//!      Authorization: Bearer <IKENGA_PKG_DB_TOKEN>   (or X-Ikenga-Pkg-Token)
//!      { "path": "<abs path>" }
//!   -> 200 { "ok": true,  "granted": true|false }
//!   -> 401 { "ok": false, "reason": "unknown-token",   "error": … }
//!   -> 403 { "ok": false, "reason": "pkg-not-allowed", "error": … }
//!   -> 500 { "ok": false, "reason": "trust-failed",    "error": … }
//! ```
//!
//! The request asserts nothing — its only field is a path. It can ask; only the
//! shell decides, and only `pkg::trust::record_studio_project_grant` writes.
//!
//! Design note: `plans/studio/verify/2026-09-12-wp32-live/wp04/design-note.md`.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use tauri::AppHandle;

use crate::commands::db::PaDb;
use crate::pkg::db_scope;
use crate::pkg::trust::STUDIO_PKG_ID;

#[derive(Debug, Deserialize)]
pub struct PkgTrustReq {
    pub path: PathBuf,
}

/// Refusal envelope. The pkg side maps any `ok:false` to `trust-unreachable`,
/// so `reason` is for operators, not for caller control flow.
fn deny(status: StatusCode, reason: &str, error: impl Into<String>) -> axum::response::Response {
    (
        status,
        Json(json!({ "ok": false, "reason": reason, "error": error.into() })),
    )
        .into_response()
}

/// Resolve the caller to a pkg id, refusing anything that is not Studio.
///
/// Fails closed at every step: no token, an unknown token, or a token belonging
/// to some other installed pkg all end here rather than reaching the dialog or
/// the grant store.
pub(crate) fn authenticate(
    headers: &HeaderMap,
) -> Result<String, (StatusCode, &'static str, String)> {
    let token = super::pkg_db::presented_token(headers).ok_or((
        StatusCode::UNAUTHORIZED,
        "unknown-token",
        "missing pkg token".to_string(),
    ))?;

    let grant = db_scope::resolve_grant(&token).ok_or((
        StatusCode::UNAUTHORIZED,
        "unknown-token",
        "token does not identify an installed pkg".to_string(),
    ))?;

    if grant.pkg_id != STUDIO_PKG_ID {
        return Err((
            StatusCode::FORBIDDEN,
            "pkg-not-allowed",
            format!("this endpoint is reserved for `{STUDIO_PKG_ID}`"),
        ));
    }

    Ok(grant.pkg_id)
}

pub async fn post_pkg_trust_project_access(
    Extension(app): Extension<AppHandle>,
    Extension(db): Extension<Arc<PaDb>>,
    headers: HeaderMap,
    Json(req): Json<PkgTrustReq>,
) -> axum::response::Response {
    let pkg_id = match authenticate(&headers) {
        Ok(id) => id,
        Err((status, reason, detail)) => {
            log::warn!("[pkg-trust] REFUSED reason={reason} detail={detail}");
            return deny(status, reason, detail);
        }
    };

    log::debug!(
        "[pkg-trust] pkg=`{pkg_id}` project-access path={}",
        req.path.display()
    );

    match crate::commands::pkg_studio::request_project_access(&app, &db, req.path).await {
        Ok(res) => Json(json!({ "ok": true, "granted": res.granted })).into_response(),
        Err(e) => deny(StatusCode::INTERNAL_SERVER_ERROR, "trust-failed", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::AUTHORIZATION, HeaderValue};

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h
    }

    /// No credential at all: refused before any dialog or db work.
    #[test]
    fn no_token_is_unauthorized() {
        let err = authenticate(&HeaderMap::new()).unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1, "unknown-token");
    }

    /// A token the shell never minted.
    #[test]
    fn an_unknown_token_is_unauthorized() {
        let err = authenticate(&bearer("00000000000000000000000000000000")).unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1, "unknown-token");
    }

    /// A *valid* per-pkg token belonging to some other installed pkg. This is
    /// the interesting case: the credential is real, but the endpoint is
    /// single-pkg, so it is 403 rather than a Studio-scoped grant written on
    /// another pkg's behalf.
    #[test]
    fn a_token_for_another_pkg_is_forbidden() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = db_scope::issue_grant("com.ikenga.test.not-studio", dir.path());
        let err = authenticate(&bearer(&token)).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "pkg-not-allowed");
        assert!(err.2.contains(STUDIO_PKG_ID), "{}", err.2);
    }

    /// The well-formed case: Studio's own token authenticates, so the handler
    /// calls through to `pkg_studio::request_project_access`.
    #[test]
    fn the_studio_token_authenticates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = db_scope::issue_grant(STUDIO_PKG_ID, dir.path());
        assert_eq!(authenticate(&bearer(&token)).unwrap(), STUDIO_PKG_ID);
    }

    /// The fallback header the pkg-db accessor also accepts works here too.
    #[test]
    fn the_x_header_fallback_also_authenticates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = db_scope::issue_grant(STUDIO_PKG_ID, dir.path());
        let mut h = HeaderMap::new();
        h.insert("x-ikenga-pkg-token", HeaderValue::from_str(&token).unwrap());
        assert_eq!(authenticate(&h).unwrap(), STUDIO_PKG_ID);
    }

    /// Request body shape matches the pkg client: a single `path` field, and no
    /// field by which a caller could assert a grant.
    #[test]
    fn the_request_body_is_just_a_path() {
        let req: PkgTrustReq = serde_json::from_str(r#"{"path":"/tmp/proj"}"#).unwrap();
        assert_eq!(req.path, PathBuf::from("/tmp/proj"));
        let req: PkgTrustReq =
            serde_json::from_str(r#"{"path":"/tmp/proj","granted":true}"#).unwrap();
        assert_eq!(req.path, PathBuf::from("/tmp/proj"));
    }
}
