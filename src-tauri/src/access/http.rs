//! The public `/access/*` HTTP endpoints (G-ACCESS §1.6, §3.7, §7.3).
//!
//! * `POST /access/pair/hello`, `POST /access/pair/confirm`,
//!   `GET /access/pair/status` — T0 and T1, filled by **WP-74b**
//!   ([`super::pairing`]: SPAKE2, key confirmation, throttling). Without a
//!   [`PairingHost`] (no access store) every pairing endpoint answers the
//!   uniform `404 pair_failed` body (§3.7: every failure looks the same),
//!   i.e. pairing is off.
//!
//!   Wire (all binary values base64url, no padding):
//!   - `hello {slot, msgA, deviceName, platform?}` →
//!     `{ok, pairingId, msgB, hostConfirm, storeId}` (`storeId` completes
//!     a typed code's `idB`, §3.4; a scanned QR pins it with `#h=`);
//!   - `confirm {pairingId, deviceConfirm}` → `{ok, state:"awaiting_host"}`;
//!   - `status ?id=<pairingId>[&client=cli]` with `X-Ikenga-Pair-Poll:
//!     <poll_key>` → `{ok, state}`; on the first poll after `allow`,
//!     `{ok, state:"allowed", device_id, tier}` plus the `ikenga_device`
//!     cookie (§3.8), or `token` in the body for `?client=cli`; after that,
//!     `410 gone`. A throttled address gets `429 throttled` with
//!     `retry_after_ms`.
//! * `POST /access/invite/inspect`, `POST /access/invite/accept` — **T1
//!   only**; **WP-76** fills them.
//!
//! These routes are mounted outside the credential middleware, so this
//! module wraps them in its own `Origin` layer (A-31): every state-changing
//! method from a foreign `Origin` is refused. On T1 the broker's gate runs
//! as well; the two agree.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde_json::{json, Value};

use super::pairing::{self, Fail, Registry, StatusOut};
use super::spake;
use super::store::{AccessStore, StoreTier};

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

/// What the public pairing endpoints serve against: the T0 daemon's or the
/// T1 broker's registry and store.
#[derive(Clone)]
pub struct PairingHost {
    pub registry: Arc<Registry>,
    pub store: AccessStore,
    /// Which tier serves these endpoints: on T0 a tailnet peer gets a
    /// non-`Secure` device cookie (Round 19, DEC-R19-1); T1 never does.
    pub tier: StoreTier,
    /// `--insecure-cookie`: drop `Secure` from the device cookie (§3.8).
    pub insecure_cookie: bool,
}

fn pair_failed() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "pair_failed",
        pairing::PAIR_FAILED_MESSAGE,
    )
}

fn fail(f: Fail) -> Response {
    match f {
        Fail::PairFailed => pair_failed(),
        Fail::Gone => error(StatusCode::GONE, "gone", "This pairing session has ended."),
        Fail::Throttled { retry_after_ms } => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "ok": false,
                "error": "throttled",
                "message": "Too many wrong codes from this device — wait a moment and try again.",
                "retry_after_ms": retry_after_ms,
            })),
        )
            .into_response(),
    }
}

/// The TCP peer's IP: the throttle key (bucketed per IPv6 /64 in
/// [`pairing::throttle_key`]) and pair-confirm's "Address" row. Behind a
/// reverse proxy (Caddy, Tailscale Serve) every client shares the proxy's
/// address; `X-Forwarded-For` is deliberately not trusted (no trusted-proxy
/// configuration exists yet), review m8.
fn addr_of(conn: &Option<ConnectInfo<SocketAddr>>) -> String {
    conn.as_ref()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn body_json(body: &Bytes) -> Option<Value> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
}

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

async fn hello(
    host: Option<Extension<PairingHost>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    body: Bytes,
) -> Response {
    let Some(Extension(host)) = host else {
        return pair_failed();
    };
    let addr = addr_of(&conn);
    let req = body_json(&body).unwrap_or(Value::Null);
    let res = host.registry.hello(
        field(&req, "slot").unwrap_or(""),
        field(&req, "msgA").unwrap_or(""),
        field(&req, "deviceName").unwrap_or(""),
        field(&req, "platform"),
        &addr,
        &host.store.meta().store_id,
    );
    pairing::flush_audits(&host.registry, &host.store).await;
    match res {
        Ok(ok) => Json(json!({
            "ok": true,
            "pairingId": ok.pairing_id,
            "msgB": spake::b64(&ok.msg_b),
            "hostConfirm": spake::b64(&ok.host_confirm),
            // idB's suffix (§3.4): a typed code needs it to finish SPAKE2.
            // It travels on the same untrusted channel, so it gives the
            // device no independent host binding (a MITM substitutes it
            // consistently; without the code that still gets it nothing).
            // A scanned QR pins it (`#h=`) and the device ignores this.
            "storeId": host.store.meta().store_id,
        }))
        .into_response(),
        Err(f) => fail(f),
    }
}

async fn confirm(
    host: Option<Extension<PairingHost>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    body: Bytes,
) -> Response {
    let Some(Extension(host)) = host else {
        return pair_failed();
    };
    let addr = addr_of(&conn);
    let req = body_json(&body).unwrap_or(Value::Null);
    let res = host.registry.confirm(
        field(&req, "pairingId").unwrap_or(""),
        field(&req, "deviceConfirm").unwrap_or(""),
        &addr,
    );
    pairing::flush_audits(&host.registry, &host.store).await;
    match res {
        Ok(()) => Json(json!({ "ok": true, "state": "awaiting_host" })).into_response(),
        Err(f) => fail(f),
    }
}

async fn status(
    host: Option<Extension<PairingHost>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(Extension(host)) = host else {
        return pair_failed();
    };
    let addr = addr_of(&conn);
    let poll = headers
        .get(pairing::POLL_HEADER)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let res = host
        .registry
        .status(q.get("id").map(String::as_str).unwrap_or(""), poll, &addr);
    pairing::flush_audits(&host.registry, &host.store).await;
    let out = match res {
        Ok(out) => out,
        Err(f) => return fail(f),
    };
    let mut res = match out {
        StatusOut::State(state) => Json(json!({ "ok": true, "state": state })).into_response(),
        StatusOut::Allowed {
            device_id,
            tier,
            token,
        } => {
            // P-10: a browser gets the credential only as an HttpOnly
            // cookie; `?client=cli` (iyke, curl) gets it in the body.
            if q.get("client").map(String::as_str) == Some("cli") {
                Json(json!({
                    "ok": true,
                    "state": "allowed",
                    "device_id": device_id,
                    "tier": tier.as_str(),
                    "token": token,
                }))
                .into_response()
            } else {
                let mut r = Json(json!({
                    "ok": true,
                    "state": "allowed",
                    "device_id": device_id,
                    "tier": tier.as_str(),
                }))
                .into_response();
                // DEC-R19-1: the TCP peer decides, never `X-Forwarded-For`.
                let insecure = super::devices::cookie_insecure(
                    host.tier,
                    host.insecure_cookie,
                    conn.as_ref().map(|c| c.0.ip()),
                );
                match HeaderValue::from_str(&super::devices::set_cookie(&token, insecure)) {
                    Ok(v) => {
                        r.headers_mut().append(header::SET_COOKIE, v);
                    }
                    Err(_) => {
                        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal", "cookie")
                    }
                }
                r
            }
        }
    };
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// WP-76 replaces these bodies.
async fn invite_gone() -> Response {
    error(
        StatusCode::GONE,
        "gone",
        "This invite link is not valid (invites are not available yet).",
    )
}

/// `/access/pair/{hello,confirm,status}`, relative to the prefix. The
/// handlers serve against a [`PairingHost`] request extension; without one
/// (no store) every endpoint answers `404 pair_failed`.
pub fn pairing_routes(allowed_origins: Vec<String>) -> Router {
    Router::new()
        .route("/hello", post(hello))
        .route("/confirm", post(confirm))
        .route("/status", get(status))
        .layer(middleware::from_fn_with_state(
            Arc::new(allowed_origins),
            origin_layer,
        ))
}

/// The T1 broker's pairing routes (nested under
/// `PublicRoutes::PAIRING_PREFIX`), serving against its registry.
pub fn pairing_routes_for(host: PairingHost, allowed_origins: Vec<String>) -> Router {
    pairing_routes(allowed_origins).layer(Extension(host))
}

/// T0: the daemon's access state reaches the router as an
/// `Extension<Arc<DaemonAccess>>` (outer layer, `server::create_router`);
/// hand its [`PairingHost`] to the pairing handlers.
async fn t0_pairing_host(mut req: Request, next: Next) -> Response {
    let host = req
        .extensions()
        .get::<Arc<super::DaemonAccess>>()
        .and_then(|a| a.pairing_host());
    if let Some(host) = host {
        req.extensions_mut().insert(host);
    }
    next.run(req).await
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
    Router::new().nest(
        PAIRING_PREFIX,
        pairing_routes(allowed_origins).layer(middleware::from_fn(t0_pairing_host)),
    )
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

    async fn send(
        router: &Router,
        req: axum::http::Request<Body>,
    ) -> (StatusCode, HeaderMap, Value) {
        let res = router.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    fn post_json(path: &str, body: Value) -> axum::http::Request<Body> {
        axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "ik:4000")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// Pairs one device over `host`'s registry and returns the `Set-Cookie`
    /// the browser status poll from TCP peer `peer` receives (`ConnectInfo`,
    /// as `into_make_service_with_connect_info` attaches it), with a
    /// spoofed `X-Forwarded-For` naming the other kind of address.
    async fn delivered_cookie(host: PairingHost, peer: &str, xff: &str) -> String {
        let registry = host.registry.clone();
        let store = host.store.clone();
        let r = Router::new().nest(PAIRING_PREFIX, pairing_routes_for(host, vec![]));
        let owner = store.meta().owner_principal_id.unwrap().to_string();
        let ticket = registry.begin(&owner, None).unwrap();
        let store_id = store.meta().store_id.clone();
        let (dev, msg_a) = spake::device_start_with_rng(&ticket.code, &store_id, rand::rngs::OsRng);
        let ok = registry
            .hello(
                &ticket.code[..1],
                &spake::b64(&msg_a),
                "Pixel 9 · Chrome",
                None,
                peer,
                &store_id,
            )
            .unwrap();
        let keys = spake::Keys::derive(
            &dev.finish(&ok.msg_b).unwrap(),
            &ok.pairing_id,
            &msg_a,
            &ok.msg_b,
        );
        registry
            .confirm(&ok.pairing_id, &spake::b64(&keys.device_confirm()), peer)
            .unwrap();
        let info = registry.begin_decide(&ok.pairing_id, &owner).unwrap();
        assert!(registry.finish_allow(
            &info.pairing_id,
            "dev-1",
            super::super::Tier::View,
            "ikd1.tok".into(),
        ));
        let mut req = axum::http::Request::builder()
            .uri(format!("/access/pair/status?id={}", ok.pairing_id))
            .header("host", "ik:4000")
            .header("x-forwarded-for", xff)
            .header(pairing::POLL_HEADER, spake::b64(keys.poll_key()))
            .body(Body::empty())
            .unwrap();
        let addr = SocketAddr::new(peer.parse().unwrap(), 51000);
        req.extensions_mut().insert(ConnectInfo(addr));
        let (st, h, v) = send(&r, req).await;
        assert_eq!((st, v["state"].as_str()), (StatusCode::OK, Some("allowed")));
        h.get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string()
    }

    /// Round 19, DEC-R19-1: the T0 pairing status drops `Secure` for a
    /// tailnet TCP peer only — never on the strength of `X-Forwarded-For` —
    /// and the T1 broker keeps it for every peer.
    #[tokio::test]
    async fn the_delivered_cookie_drops_secure_only_for_a_t0_tailnet_peer() {
        let store = AccessStore::memory_t0().await;
        let host = |tier| PairingHost {
            registry: Registry::new(),
            store: store.clone(),
            tier,
            insecure_cookie: false,
        };
        for (tier, peer, xff, secure) in [
            (StoreTier::T0, "100.101.102.103", "192.168.1.9", false),
            (StoreTier::T0, "fd7a:115c:a1e0::7", "192.168.1.9", false),
            (StoreTier::T0, "::ffff:100.64.0.9", "192.168.1.9", false),
            (StoreTier::T0, "192.168.1.9", "100.101.102.103", true),
            (StoreTier::T0, "100.128.0.1", "100.101.102.103", true),
            (StoreTier::T1, "100.101.102.103", "192.168.1.9", true),
            (StoreTier::T1, "fd7a:115c:a1e0::7", "192.168.1.9", true),
            (StoreTier::T1, "192.168.1.9", "100.101.102.103", true),
        ] {
            let cookie = delivered_cookie(host(tier), peer, xff).await;
            assert!(cookie.starts_with("ikenga_device=ikd1.tok;"), "{cookie}");
            assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
            assert_eq!(
                cookie.contains("Secure"),
                secure,
                "{tier:?} peer={peer}: {cookie}"
            );
        }
    }

    /// The endpoints end to end over the router: hello → confirm → (allow)
    /// → status sets the HttpOnly device cookie once, then `410`.
    #[tokio::test]
    async fn the_pairing_endpoints_deliver_a_cookie_once() {
        let store = AccessStore::memory_t0().await;
        let registry = Registry::new();
        let host = PairingHost {
            registry: registry.clone(),
            store: store.clone(),
            tier: StoreTier::T0,
            insecure_cookie: true,
        };
        let r = Router::new().nest(PAIRING_PREFIX, pairing_routes_for(host, vec![]));
        let ticket = registry
            .begin(&store.meta().owner_principal_id.unwrap().to_string(), None)
            .unwrap();
        let (dev, msg_a) =
            spake::device_start_with_rng(&ticket.code, &store.meta().store_id, rand::rngs::OsRng);
        let (st, _, v) = send(
            &r,
            post_json(
                "/access/pair/hello",
                json!({"slot": &ticket.code[..1], "msgA": spake::b64(&msg_a), "deviceName": "Pixel 9 · Chrome"}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["storeId"], store.meta().store_id);
        let pid = v["pairingId"].as_str().unwrap().to_string();
        let msg_b = spake::unb64(v["msgB"].as_str().unwrap()).unwrap();
        let key = dev.finish(&msg_b).unwrap();
        let keys = spake::Keys::derive(&key, &pid, &msg_a, &msg_b);
        assert_eq!(
            spake::unb64(v["hostConfirm"].as_str().unwrap()).unwrap(),
            keys.host_confirm()
        );
        let (st, _, v) = send(
            &r,
            post_json(
                "/access/pair/confirm",
                json!({"pairingId": pid, "deviceConfirm": spake::b64(&keys.device_confirm())}),
            ),
        )
        .await;
        assert_eq!(
            (st, v["state"].as_str()),
            (StatusCode::OK, Some("awaiting_host"))
        );

        let poll = |cli: bool| {
            axum::http::Request::builder()
                .uri(format!(
                    "/access/pair/status?id={pid}{}",
                    if cli { "&client=cli" } else { "" }
                ))
                .header("host", "ik:4000")
                .header(pairing::POLL_HEADER, spake::b64(keys.poll_key()))
                .body(Body::empty())
                .unwrap()
        };
        let (st, _, v) = send(&r, poll(false)).await;
        assert_eq!(
            (st, v["state"].as_str()),
            (StatusCode::OK, Some("awaiting_host"))
        );

        let info = registry
            .begin_decide(&pid, &store.meta().owner_principal_id.unwrap().to_string())
            .unwrap();
        assert!(registry.finish_allow(
            &info.pairing_id,
            "dev-1",
            super::super::Tier::View,
            "ikd1.tok".into(),
        ));
        let (st, h, v) = send(&r, poll(false)).await;
        assert_eq!((st, v["state"].as_str()), (StatusCode::OK, Some("allowed")));
        assert_eq!(v["tier"], "view");
        assert!(v.get("token").is_none(), "P-10: never in a browser body");
        let cookie = h.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(cookie.starts_with("ikenga_device=ikd1.tok;"));
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        assert!(!cookie.contains("Secure"), "--insecure-cookie");
        let (st, _, v) = send(&r, poll(true)).await;
        assert_eq!((st, v["error"].as_str()), (StatusCode::GONE, Some("gone")));

        // Malformed bodies get the uniform refusal.
        let (st, _, v) = send(&r, post_json("/access/pair/hello", json!({}))).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("pair_failed"))
        );
    }
}
