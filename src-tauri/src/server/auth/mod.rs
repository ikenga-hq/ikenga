//! T1 request → principal (G-PRINCIPAL §2): the one resolution point.
//!
//! Every authenticated T1 request resolves to **exactly one**
//! [`PrincipalCtx`] before it reaches a handler (§2.1); handlers never see a
//! raw credential. Resolution walks an **ordered list** of
//! [`CredentialResolver`]s — the session cookie first — so WP-74 can append a
//! `DeviceGrant` resolver without restructuring (G-ACCESS R-4). Under T1 the
//! operator bearer and `?token=` grant nothing (§2.3, §2.4, I-6): no resolver
//! reads them.
//!
//! Module map (G-ACCESS R-6 keeps these apart from proxying, child launch and
//! provisioning):
//!
//! * this file — [`PrincipalCtx`], [`Credential`], the resolver list, the
//!   `require_principal` middleware, the `Origin` gate and the unauthenticated
//!   route extension point ([`PublicRoutes`]);
//! * [`backend`] — the `axum-login` backend over `operator/accounts.db`, the
//!   `tower-sessions` SQLite store in `operator/sessions.db`, the cookie, and
//!   the session-cookie resolver;
//! * [`routes`] — `/auth/login`, `/auth/logout`, `/auth/me`, `/auth/password`.

pub mod backend;
pub mod routes;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{request::Parts, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};

use crate::executor::Principal;

/// The resolved caller (§2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalCtx {
    pub principal: Principal,
    pub via: Credential,
}

/// How the caller proved who they are (§2.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// WP-20: the browser session cookie.
    Session { session_id: String },
    /// Reserved for WP-74; must resolve to a `principal_id`.
    DeviceGrant { device_id: String },
    /// T0 only (§2.4); never produced under T1.
    OperatorBearer,
}

impl Credential {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Credential::Session { session_id } => Some(session_id),
            _ => None,
        }
    }

    pub fn device_id(&self) -> Option<&str> {
        match self {
            Credential::DeviceGrant { device_id } => Some(device_id),
            _ => None,
        }
    }
}

/// The revocation epochs a credential was resolved under, captured with the
/// [`PrincipalCtx`]: the open-socket registry keys on them (§2.2, G-ACCESS
/// R-5), and a socket is closed once either moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Epochs {
    /// `accounts.session_epoch` at resolution.
    pub session_epoch: i64,
    /// A credential-specific revocation epoch (WP-74's device grants); `None`
    /// for a session cookie.
    pub grant_epoch: Option<i64>,
}

/// What one resolver made of a request.
#[derive(Debug)]
pub enum Resolution {
    Resolved {
        ctx: PrincipalCtx,
        epochs: Epochs,
    },
    /// This resolver's credential is absent: ask the next one.
    NotPresent,
    /// The credential is present but invalid. Stops the walk: a bad device
    /// grant must not fall through to some weaker credential.
    Rejected(&'static str),
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One way to obtain a [`PrincipalCtx`] (§2.1: every later credential kind
/// *adds a way to obtain* one; none bypasses it).
pub trait CredentialResolver: Send + Sync {
    /// Stable name, for logs.
    fn name(&self) -> &'static str;
    fn resolve<'a>(&'a self, parts: &'a Parts) -> BoxFuture<'a, anyhow::Result<Resolution>>;
}

/// The ordered resolver list (R-4). The session cookie is first; WP-74
/// [`push`](Self::push)es its device-grant resolver after it.
#[derive(Clone, Default)]
pub struct Resolvers(Vec<Arc<dyn CredentialResolver>>);

impl Resolvers {
    pub fn new(first: Arc<dyn CredentialResolver>) -> Self {
        Self(vec![first])
    }

    pub fn push(&mut self, resolver: Arc<dyn CredentialResolver>) -> &mut Self {
        self.0.push(resolver);
        self
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|r| r.name()).collect()
    }

    /// Walk the list in order; the first resolver that finds its credential
    /// decides.
    pub async fn resolve(&self, parts: &Parts) -> anyhow::Result<Resolution> {
        for resolver in &self.0 {
            match resolver.resolve(parts).await? {
                Resolution::NotPresent => continue,
                decided => return Ok(decided),
            }
        }
        Ok(Resolution::NotPresent)
    }
}

pub(crate) fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "ok": false, "error": message, "code": code })),
    )
        .into_response()
}

/// The 401 every unresolved request gets. It never says which credentials
/// were tried or why one failed.
pub(crate) fn unauthenticated() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "Unauthorized: log in at /auth/login",
    )
}

/// Middleware: resolve a [`PrincipalCtx`] (and its [`Epochs`]) into the
/// request extensions, or answer 401. Everything behind it — RPC, WebSocket,
/// pkg routes, `/auth/{logout,me,password}` — sees the context and nothing
/// else (I-6).
pub async fn require_principal(
    State(resolvers): State<Resolvers>,
    req: Request,
    next: Next,
) -> Response {
    let (mut parts, body) = req.into_parts();
    match resolvers.resolve(&parts).await {
        Ok(Resolution::Resolved { ctx, epochs }) => {
            parts.extensions.insert(ctx);
            parts.extensions.insert(epochs);
            super::activity::touch();
            next.run(Request::from_parts(parts, body)).await
        }
        Ok(Resolution::NotPresent) | Ok(Resolution::Rejected(_)) => unauthenticated(),
        Err(e) => {
            tracing::error!("credential resolution failed: {e:#}");
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "authentication is temporarily unavailable",
            )
        }
    }
}

/// Compare two secrets without leaking their common prefix through timing.
pub(crate) fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The `Origin` gate (`server::origin_permitted`, unchanged): a browser
/// attaches `Origin` to every WebSocket handshake and non-simple fetch and
/// can't forge it, so an `Origin` that is neither ours nor explicitly allowed
/// means another site is driving the request. A missing `Origin` is a
/// non-browser client (curl, a cookie jar) and is allowed.
pub fn origin_ok(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) else {
        return true;
    };
    if allowed_origins.iter().any(|allowed| ct_eq(allowed, origin)) {
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

/// Whether the gate applies: every state-changing method, and every
/// WebSocket handshake (a GET that upgrades).
pub fn is_gated(method: &Method, headers: &HeaderMap) -> bool {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    let upgrade = headers
        .get("upgrade")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    !safe || upgrade
}

/// Middleware: the `Origin` gate on every state-changing route and WS
/// handshake (§2.2 CSRF), the extension routes included.
pub async fn origin_gate(
    State(allowed_origins): State<Arc<Vec<String>>>,
    req: Request,
    next: Next,
) -> Response {
    if is_gated(req.method(), req.headers()) && !origin_ok(req.headers(), &allowed_origins) {
        tracing::warn!(
            "cross-origin {} {} rejected (origin: {:?})",
            req.method(),
            req.uri().path(),
            req.headers().get("origin")
        );
        return json_error(
            StatusCode::FORBIDDEN,
            "cross_origin",
            "Forbidden: cross-origin request",
        );
    }
    next.run(req).await
}

/// The unauthenticated routes beyond `/api/health`, `/auth/login` and the
/// SPA (G-PRINCIPAL §2.2 as amended by Round 16 §14.1): the pairing and
/// invite endpoints WP-74 and WP-76 add. They are mounted outside
/// [`require_principal`] but inside the session layer and the `Origin` gate,
/// under the two reserved prefixes only — nothing else can be made public
/// through here. Their own throttling is theirs.
#[derive(Default)]
pub struct PublicRoutes {
    pairing: Option<Router>,
    invites: Option<Router>,
}

impl PublicRoutes {
    pub const PAIRING_PREFIX: &'static str = "/access/pair";
    pub const INVITE_PREFIX: &'static str = "/access/invite";

    /// `/access/pair/{hello,confirm,status}` (T0 and T1).
    pub fn pairing(mut self, routes: Router) -> Self {
        self.pairing = Some(routes);
        self
    }

    /// `/access/invite/{inspect,accept}` (T1).
    pub fn invites(mut self, routes: Router) -> Self {
        self.invites = Some(routes);
        self
    }

    pub(crate) fn into_router(self) -> Router {
        let mut router = Router::new();
        if let Some(pairing) = self.pairing {
            router = router.nest(Self::PAIRING_PREFIX, pairing);
        }
        if let Some(invites) = self.invites {
            router = router.nest(Self::INVITE_PREFIX, invites);
        }
        router
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("host", HeaderValue::from_str(host).unwrap());
        if let Some(o) = origin {
            h.insert("origin", HeaderValue::from_str(o).unwrap());
        }
        h
    }

    #[test]
    fn origin_gate_allows_same_origin_listed_and_absent_only() {
        let allowed = vec!["http://localhost:5173".to_string()];
        assert!(origin_ok(&headers(None, "ik:4000"), &allowed));
        assert!(origin_ok(
            &headers(Some("https://ik:4000"), "ik:4000"),
            &allowed
        ));
        assert!(origin_ok(
            &headers(Some("http://localhost:5173"), "ik:4000"),
            &allowed
        ));
        assert!(!origin_ok(
            &headers(Some("https://evil.example"), "ik:4000"),
            &allowed
        ));
        assert!(!origin_ok(&headers(Some("null"), "ik:4000"), &allowed));
    }

    #[test]
    fn the_gate_covers_writes_and_ws_handshakes_not_plain_reads() {
        let plain = HeaderMap::new();
        assert!(!is_gated(&Method::GET, &plain));
        assert!(!is_gated(&Method::HEAD, &plain));
        for m in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(is_gated(&m, &plain), "{m}");
        }
        let mut ws = HeaderMap::new();
        ws.insert("upgrade", HeaderValue::from_static("WebSocket"));
        assert!(is_gated(&Method::GET, &ws));
    }

    struct Fixed(&'static str, fn() -> Resolution);
    impl CredentialResolver for Fixed {
        fn name(&self) -> &'static str {
            self.0
        }
        fn resolve<'a>(&'a self, _parts: &'a Parts) -> BoxFuture<'a, anyhow::Result<Resolution>> {
            let r = (self.1)();
            Box::pin(async move { Ok(r) })
        }
    }

    fn ctx(device: &str) -> Resolution {
        Resolution::Resolved {
            ctx: PrincipalCtx {
                principal: crate::executor::Principal {
                    id: crate::executor::PrincipalId::new_v7(),
                    username: "ada".into(),
                    unix_name: "ik-ada".into(),
                    uid: 20_000,
                    gid: 20_000,
                    home: "/h".into(),
                    shell: "/bin/sh".into(),
                },
                via: Credential::DeviceGrant {
                    device_id: device.into(),
                },
            },
            epochs: Epochs {
                session_epoch: 0,
                grant_epoch: Some(1),
            },
        }
    }

    /// R-4: resolvers run in order; an absent credential falls through, a
    /// present-but-bad one stops the walk.
    #[tokio::test]
    async fn resolvers_walk_in_order_and_a_rejection_stops_the_walk() {
        let parts = axum::http::Request::new(()).into_parts().0;
        let mut list = Resolvers::new(Arc::new(Fixed("session", || Resolution::NotPresent)));
        list.push(Arc::new(Fixed("device", || ctx("d1"))));
        assert_eq!(list.names(), ["session", "device"]);
        match list.resolve(&parts).await.unwrap() {
            Resolution::Resolved { ctx, .. } => assert_eq!(ctx.via.device_id(), Some("d1")),
            other => panic!("{other:?}"),
        }

        let mut list = Resolvers::new(Arc::new(Fixed("session", || Resolution::Rejected("bad"))));
        list.push(Arc::new(Fixed("device", || ctx("d1"))));
        assert!(matches!(
            list.resolve(&parts).await.unwrap(),
            Resolution::Rejected("bad")
        ));

        let list = Resolvers::default();
        assert!(matches!(
            list.resolve(&parts).await.unwrap(),
            Resolution::NotPresent
        ));
    }
}
