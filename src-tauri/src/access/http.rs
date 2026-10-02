//! HTTP surface of G-ACCESS (§1.6, §2.4, §3.8).
//!
//! * The **public** `/access/pair/{hello,confirm,status}` (T0 and T1) and
//!   `/access/invite/{inspect,accept}` (T1 only) routes, behind their own
//!   `Origin` layer: `origin_permitted` otherwise runs only inside
//!   `auth_middleware`, and these routes sit outside it (A-31). The handler
//!   bodies are stubs: pairing is WP-74b's, invites WP-76's.
//! * T0 credential resolution for `server::auth_middleware`: precedence
//!   DeviceGrant (cookie `ikenga_device`, or `Bearer ikd1.…`) then the
//!   operator bearer (header or `?token=`) (§2.4). A present but invalid
//!   device cookie is cleared (`Max-Age=0`) without failing a request that
//!   carries another valid credential.
//! * The device cookie's `Set-Cookie` forms (§3.8).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use super::audit::chain::now_ms;
use super::ctx::AccessCtx;
use super::devices::{self, Presented, Resolved, COOKIE_MAX_AGE_SECS, DEVICE_COOKIE, TOKEN_PREFIX};
use super::{child_ctx, Mode, Runtime};

/// Constant-time string compare.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The `Origin` rule `server::origin_permitted` applies: absent (a
/// non-browser client) passes; otherwise it must be listed or same-origin
/// with the `Host` we were reached on.
pub fn origin_ok(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) else {
        return true;
    };
    if allowed.iter().any(|a| ct_eq(a, origin)) {
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

/// Every state-changing method, and every WebSocket handshake.
pub fn is_state_changing(method: &Method, headers: &HeaderMap) -> bool {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let upgrade = headers
        .get("upgrade")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    !safe || upgrade
}

/// `{ok:false, error:<code>, message}` with the matching status (§9.1).
pub fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "ok": false, "error": code, "message": message })),
    )
        .into_response()
}

async fn origin_layer(
    State(allowed): State<Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response {
    if is_state_changing(req.method(), req.headers()) && !origin_ok(req.headers(), &allowed) {
        tracing::warn!(
            "cross-origin {} {} rejected (origin: {:?})",
            req.method(),
            req.uri().path(),
            req.headers().get("origin")
        );
        return json_error(StatusCode::FORBIDDEN, "forbidden", "cross-origin request");
    }
    next.run(req).await
}

async fn pairing_stub() -> Response {
    json_error(
        StatusCode::NOT_IMPLEMENTED,
        "internal",
        "not implemented (WP-74b)",
    )
}

async fn invite_stub() -> Response {
    json_error(
        StatusCode::NOT_IMPLEMENTED,
        "internal",
        "not implemented (WP-76)",
    )
}

/// `/hello`, `/confirm`, `/status`, to be nested under `/access/pair`
/// (`server::auth::PublicRoutes::PAIRING_PREFIX` on T1). Throttling (§3.7)
/// is WP-74b's, with the handlers.
pub fn pairing_routes(rt: &Runtime) -> Router {
    Router::new()
        .route("/hello", post(pairing_stub))
        .route("/confirm", post(pairing_stub))
        .route("/status", get(pairing_stub))
        .layer(middleware::from_fn_with_state(
            Arc::new(rt.options.allowed_origins.clone()),
            origin_layer,
        ))
}

/// `/inspect`, `/accept`, to be nested under `/access/invite` (T1 only).
pub fn invite_routes(rt: &Runtime) -> Router {
    Router::new()
        .route("/inspect", post(invite_stub))
        .route("/accept", post(invite_stub))
        .layer(middleware::from_fn_with_state(
            Arc::new(rt.options.allowed_origins.clone()),
            origin_layer,
        ))
}

/// The T0 daemon's public access routes: pairing only (invites are T1).
/// A principal child serves none (the broker does).
pub fn t0_public_router(rt: &Runtime) -> Router {
    match rt.mode {
        Mode::Daemon => Router::new().nest("/access/pair", pairing_routes(rt)),
        _ => Router::new(),
    }
}

/// `ikenga_device=<token>; HttpOnly; SameSite=Strict; Path=/;
/// Max-Age=34560000; Secure` (§3.8; `Secure` off with `--insecure-cookie`).
pub fn set_device_cookie(token: &str, insecure: bool) -> Option<HeaderValue> {
    let secure = if insecure { "" } else { "; Secure" };
    HeaderValue::from_str(&format!(
        "{DEVICE_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE_SECS}{secure}"
    ))
    .ok()
}

/// Clears a dead device cookie. No `Secure`: a clearing cookie carries no
/// secret, and it must also land on a plain-HTTP perimeter.
pub fn clear_device_cookie() -> HeaderValue {
    HeaderValue::from_static("ikenga_device=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

/// The `ikenga_device` cookie value, if any.
pub fn device_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == DEVICE_COOKIE)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The device credential a request presents, and how (§2.4) — the one
/// rule both tiers use: the `ikenga_device` cookie when present (a browser
/// sends it on every same-origin request and WS handshake), else
/// `Authorization: Bearer ikd1.…` (non-browser clients). When a cookie is
/// present it alone decides the device credential.
pub fn presented_device_token(headers: &HeaderMap) -> Option<(String, Presented)> {
    if let Some(cookie) = device_cookie(headers) {
        return Some((cookie, Presented::Cookie));
    }
    bearer(headers)
        .filter(|t| t.starts_with(TOKEN_PREFIX))
        .map(|t| (t.to_string(), Presented::Bearer))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
}

fn query_token(query: Option<&str>) -> Option<String> {
    query?.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == "token").then(|| {
            percent_encoding::percent_decode_str(v)
                .decode_utf8_lossy()
                .into_owned()
        })
    })
}

/// What T0 resolution decided.
#[derive(Debug)]
pub enum T0Auth {
    Ok {
        ctx: AccessCtx,
        /// A rotated device cookie, or a clear of a dead one.
        set_cookie: Option<HeaderValue>,
    },
    Denied {
        clear_cookie: bool,
    },
}

/// Resolve a T0 daemon (or T1 child) request (§2.3, §2.4).
pub async fn resolve_t0(
    rt: &Runtime,
    headers: &HeaderMap,
    query: Option<&str>,
    expected_token: &str,
    remote_addr: Option<&str>,
) -> T0Auth {
    // A principal child: only the broker's per-child token, in the header,
    // and caps only from `X-Ikenga-Caps` (§1.7).
    if rt.mode == Mode::PrincipalChild {
        return match bearer(headers) {
            Some(b) if !expected_token.is_empty() && ct_eq(b, expected_token) => T0Auth::Ok {
                ctx: child_ctx(headers),
                set_cookie: None,
            },
            _ => T0Auth::Denied {
                clear_cookie: false,
            },
        };
    }

    let header_bearer = bearer(headers);
    // §2.4: the device credential (one rule on T0 and T1,
    // [`presented_device_token`]: the cookie decides when present, else a
    // `Bearer ikd1.…`).
    let mut clear_cookie = false;
    match presented_device_token(headers) {
        Some((cookie, Presented::Cookie)) => {
            match resolve_device(rt, &cookie, Presented::Cookie, remote_addr).await {
                Some((ctx, rotated)) => {
                    let set_cookie =
                        rotated.and_then(|t| set_device_cookie(&t, rt.options.insecure_cookie));
                    return T0Auth::Ok { ctx, set_cookie };
                }
                // A dead cookie is cleared, and doesn't fail a request that
                // carries the operator bearer.
                None => clear_cookie = true,
            }
        }
        // Never rotated. Present but invalid stops here: it can't also be
        // the operator bearer.
        Some((tok, Presented::Bearer)) => {
            return match resolve_device(rt, &tok, Presented::Bearer, remote_addr).await {
                Some((ctx, _)) => T0Auth::Ok {
                    ctx,
                    set_cookie: None,
                },
                None => T0Auth::Denied {
                    clear_cookie: false,
                },
            };
        }
        None => {}
    }

    let operator = !expected_token.is_empty()
        && (header_bearer.is_some_and(|b| ct_eq(b, expected_token))
            || query_token(query).is_some_and(|t| ct_eq(&t, expected_token)));
    if operator {
        return T0Auth::Ok {
            ctx: rt.operator_ctx(),
            set_cookie: clear_cookie.then(clear_device_cookie),
        };
    }
    T0Auth::Denied { clear_cookie }
}

/// A device token → its context (and a rotated token for a cookie).
async fn resolve_device(
    rt: &Runtime,
    token: &str,
    presented: Presented,
    remote_addr: Option<&str>,
) -> Option<(AccessCtx, Option<String>)> {
    let store = rt.store.as_ref()?;
    let now = now_ms();
    let (row, used_prev) = match devices::resolve(store, token, now).await {
        Ok(Resolved::Ok { row, used_prev }) => (row, used_prev),
        Ok(Resolved::Refused(why)) => {
            tracing::info!("device credential refused ({why:?})");
            return None;
        }
        Err(e) => {
            tracing::error!("device credential resolution failed: {e:#}");
            return None;
        }
    };
    let principal = row.principal()?;
    devices::touch(store, &row.device_id, remote_addr, now).await;
    let rotated = match devices::rotate_if_due(store, &row, used_prev, presented, now).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("device cookie rotation failed: {e:#}");
            None
        }
    };
    Some((
        AccessCtx::device(principal, row.device_id.clone(), row.tier, row.grant_epoch),
        rotated,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::Tier;
    use crate::access::store::test_support;
    use crate::access::Runtime;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        m
    }

    #[test]
    fn cookies_parse_and_serialize() {
        let m = h(&[("cookie", "a=1; ikenga_device=ikd1.x.y; ikenga_session=s")]);
        assert_eq!(device_cookie(&m).as_deref(), Some("ikd1.x.y"));
        assert_eq!(device_cookie(&h(&[("cookie", "ikenga_device=")])), None);
        let c = set_device_cookie("ikd1.a.b", false).unwrap();
        let c = c.to_str().unwrap();
        for part in [
            "HttpOnly",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=34560000",
            "Secure",
        ] {
            assert!(c.contains(part), "{c}");
        }
        assert!(!set_device_cookie("t", true)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Secure"));
        assert!(clear_device_cookie()
            .to_str()
            .unwrap()
            .contains("Max-Age=0"));
    }

    async fn rt() -> (tempfile::TempDir, Runtime, String) {
        let (dir, store) = test_support::t0().await;
        let owner = store.owner.unwrap().to_string();
        let mut tx = store.begin().await.unwrap();
        let (_, token) =
            devices::issue_paired_in(&mut tx, &owner, "phone", None, Tier::View, None, None)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        let mut rt = Runtime::none();
        rt.store = Some(store);
        (dir, rt, token)
    }

    /// §2.4 precedence and the clear-on-invalid rule.
    #[tokio::test]
    async fn device_then_operator_and_invalid_cookies_are_cleared() {
        let (_d, rt, token) = rt().await;
        // A device cookie resolves to the device, at its tier.
        let m = h(&[("cookie", &format!("ikenga_device={token}"))]);
        match resolve_t0(&rt, &m, None, "op", None).await {
            T0Auth::Ok { ctx, set_cookie } => {
                assert_eq!(ctx.tier, Tier::View);
                assert!(!ctx.admin_strength);
                assert!(set_cookie.is_none());
            }
            other => panic!("{other:?}"),
        }
        // A bearer device token too.
        let m = h(&[("authorization", &format!("Bearer {token}"))]);
        assert!(matches!(
            resolve_t0(&rt, &m, None, "op", None).await,
            T0Auth::Ok { .. }
        ));
        // A dead cookie with a valid operator bearer: operator, cookie cleared.
        let dead = format!("ikenga_device={}x", &token[..token.len() - 1]);
        let m = h(&[("cookie", &dead), ("authorization", "Bearer op")]);
        match resolve_t0(&rt, &m, None, "op", None).await {
            T0Auth::Ok { ctx, set_cookie } => {
                assert!(ctx.is_operator());
                assert_eq!(set_cookie, Some(clear_device_cookie()));
            }
            other => panic!("{other:?}"),
        }
        // A dead cookie alone: denied, cleared.
        let m = h(&[("cookie", &dead)]);
        assert!(matches!(
            resolve_t0(&rt, &m, None, "op", None).await,
            T0Auth::Denied { clear_cookie: true }
        ));
        // A bad bearer device token never falls through to `?token=`.
        let m = h(&[("authorization", "Bearer ikd1.bogus")]);
        assert!(matches!(
            resolve_t0(&rt, &m, Some("token=op"), "op", None).await,
            T0Auth::Denied { .. }
        ));
        // The operator bearer by query (WS handshakes).
        assert!(matches!(
            resolve_t0(&rt, &HeaderMap::new(), Some("x=1&token=op"), "op", None).await,
            T0Auth::Ok { .. }
        ));
        // No store: a device token resolves to nothing.
        let none = Runtime::none();
        let m = h(&[("cookie", &format!("ikenga_device={token}"))]);
        assert!(matches!(
            resolve_t0(&none, &m, None, "op", None).await,
            T0Auth::Denied { .. }
        ));
    }

    /// Review finding 4: one §2.4 order on both tiers — the cookie decides
    /// when present, else `Bearer ikd1.…`.
    #[tokio::test]
    async fn the_cookie_decides_the_device_credential_on_both_tiers() {
        let (_d, rt, cookie_tok) = rt().await;
        let store = rt.store.clone().unwrap();
        let owner = store.owner.unwrap().to_string();
        let mut tx = store.begin().await.unwrap();
        let (bearer_dev, bearer_tok) =
            devices::issue_paired_in(&mut tx, &owner, "cli", None, Tier::Full, None, None)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        let both = h(&[
            ("cookie", &format!("ikenga_device={cookie_tok}")),
            ("authorization", &format!("Bearer {bearer_tok}")),
        ]);
        assert_eq!(
            presented_device_token(&both),
            Some((cookie_tok.clone(), Presented::Cookie))
        );
        match resolve_t0(&rt, &both, None, "op", None).await {
            T0Auth::Ok { ctx, .. } => {
                assert_eq!(
                    ctx.tier,
                    Tier::View,
                    "the cookie's device, not the bearer's"
                );
                assert_ne!(
                    ctx.device_id.as_deref(),
                    Some(bearer_dev.device_id.as_str())
                );
            }
            other => panic!("{other:?}"),
        }
        // A dead cookie isn't skipped for the bearer device (T1 rejects too).
        let dead = h(&[
            ("cookie", "ikenga_device=ikd1.dead"),
            ("authorization", &format!("Bearer {bearer_tok}")),
        ]);
        assert!(matches!(
            resolve_t0(&rt, &dead, None, "op", None).await,
            T0Auth::Denied { clear_cookie: true }
        ));
        let only_bearer = h(&[("authorization", &format!("Bearer {bearer_tok}"))]);
        assert_eq!(
            presented_device_token(&only_bearer),
            Some((bearer_tok.clone(), Presented::Bearer))
        );
        assert_eq!(
            presented_device_token(&h(&[("authorization", "Bearer op")])),
            None
        );
    }

    #[tokio::test]
    async fn a_child_accepts_only_the_per_child_token_header() {
        let child = Runtime::for_daemon(None, true, crate::access::AccessOptions::default()).await;
        let m = h(&[("authorization", "Bearer kid"), ("x-ikenga-caps", "files")]);
        match resolve_t0(&child, &m, None, "kid", None).await {
            T0Auth::Ok { ctx, .. } => {
                assert_eq!(ctx.via, crate::access::ctx::Credential::ChildToken);
                assert_eq!(ctx.caps.names(), ["files"]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            resolve_t0(&child, &HeaderMap::new(), Some("token=kid"), "kid", None).await,
            T0Auth::Denied { .. }
        ));
    }
}
