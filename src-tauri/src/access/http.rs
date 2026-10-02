//! The public `/access/*` HTTP endpoints (G-ACCESS §1.6, §3.7, §7.3).
//!
//! * `POST /access/pair/hello`, `POST /access/pair/confirm`,
//!   `GET /access/pair/status` — T0 and T1. **WP-74b** fills the handlers
//!   (SPAKE2, the fingerprint, throttling); until then every pairing
//!   endpoint answers the uniform `404 pair_failed` body (§3.7: every
//!   failure looks the same), i.e. pairing is off.
//! * `POST /access/invite/inspect`, `POST /access/invite/accept` — **T1
//!   only**; **WP-76** fills them.
//!
//! These routes are mounted outside the credential middleware, so this
//! module wraps them in its own `Origin` layer (A-31): every state-changing
//! method from a foreign `Origin` is refused. On T1 the broker's gate runs
//! as well; the two agree.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

/// `{ok:false, error:<code>, message}` with the matching status (§9.1).
pub fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "ok": false, "error": code, "message": message })),
    )
        .into_response()
}

fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// The same rule as `server::origin_permitted` / `server::auth::origin_ok`
/// (platform-neutral copy: the latter is Linux-only). A missing `Origin` is
/// a non-browser client and is allowed.
pub fn origin_ok(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) else {
        return true;
    };
    if allowed_origins.iter().any(|a| ct_eq(a, origin)) {
        return true;
    }
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    origin
        .split_once("://")
        .map(|(_, o)| o == host)
        .unwrap_or(false)
}

async fn origin_layer(
    State(allowed): State<Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response {
    let state_changing = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if state_changing && !origin_ok(req.headers(), &allowed) {
        tracing::warn!(
            "cross-origin {} {} rejected (origin: {:?})",
            req.method(),
            req.uri().path(),
            req.headers().get("origin")
        );
        return error(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Forbidden: cross-origin request",
        );
    }
    next.run(req).await
}

/// WP-74b replaces these bodies.
async fn pair_failed() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "pair_failed",
        "That code didn't work. Ask for a new one on the computer.",
    )
}

/// WP-76 replaces these bodies.
async fn invite_gone() -> Response {
    error(
        StatusCode::GONE,
        "gone",
        "This invite link is not valid (invites are not available yet).",
    )
}

/// `/access/pair/{hello,confirm,status}`, relative to the prefix (the T1
/// broker nests it under `PublicRoutes::PAIRING_PREFIX`).
pub fn pairing_routes(allowed_origins: Vec<String>) -> Router {
    Router::new()
        .route("/hello", post(pair_failed))
        .route("/confirm", post(pair_failed))
        .route("/status", get(pair_failed))
        .layer(middleware::from_fn_with_state(
            Arc::new(allowed_origins),
            origin_layer,
        ))
}

/// `/access/invite/{inspect,accept}` (T1 only), relative to the prefix.
pub fn invite_routes(allowed_origins: Vec<String>) -> Router {
    Router::new()
        .route("/inspect", post(invite_gone))
        .route("/accept", post(invite_gone))
        .layer(middleware::from_fn_with_state(
            Arc::new(allowed_origins),
            origin_layer,
        ))
}

pub const PAIRING_PREFIX: &str = "/access/pair";
pub const INVITE_PREFIX: &str = "/access/invite";

/// The T0 daemon's public access router: pairing only (invites are T1).
pub fn t0_public_router(allowed_origins: Vec<String>) -> Router {
    Router::new().nest(PAIRING_PREFIX, pairing_routes(allowed_origins))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    async fn call(router: &Router, method: &str, path: &str, origin: Option<&str>) -> StatusCode {
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "ik:4000");
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    /// A-31 (T0 half): the pairing endpoints exist, are public, and refuse
    /// a foreign Origin on every state-changing method; invites are absent
    /// on T0.
    #[tokio::test]
    async fn pairing_is_public_origin_checked_and_invites_are_t1_only() {
        let r = t0_public_router(vec![]);
        assert_eq!(
            call(&r, "POST", "/access/pair/hello", None).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &r,
                "POST",
                "/access/pair/hello",
                Some("https://evil.example")
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&r, "POST", "/access/pair/confirm", Some("http://ik:4000")).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(
                &r,
                "GET",
                "/access/pair/status",
                Some("https://evil.example")
            )
            .await,
            StatusCode::NOT_FOUND,
            "a GET is not state-changing"
        );
        assert_eq!(
            call(&r, "POST", "/access/invite/accept", None).await,
            StatusCode::NOT_FOUND
        );
        let t1 = Router::new().nest(INVITE_PREFIX, invite_routes(vec![]));
        assert_eq!(
            call(
                &t1,
                "POST",
                "/access/invite/accept",
                Some("https://evil.example")
            )
            .await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&t1, "POST", "/access/invite/inspect", None).await,
            StatusCode::GONE
        );
    }
}
