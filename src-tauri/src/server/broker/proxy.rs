//! The broker's reverse proxy (G-PRINCIPAL §3 topology B; G-ACCESS R-3,
//! R-6 `<broker-proxy>`): a resolved principal's `/api/rpc` and `/pkgs/*`
//! (HTTP) and `/ws/*` (WebSocket) go to **that principal's** child and no
//! other — the child is chosen from the [`PrincipalCtx`], never from
//! anything the client sent.
//!
//! On the way through:
//!
//! * every client `X-Ikenga-*` header is stripped, as are `Cookie` (the
//!   broker's session cookie never reaches a child), `Authorization` and
//!   `Origin` (the broker has already applied the gate; the child sees a
//!   loopback request with its own per-child bearer);
//! * `X-Ikenga-Principal: <principal_id>` is added **for logging only** —
//!   the child never authorizes on it (§3);
//! * R-3 hooks: an `/api/rpc` body is parsed into `{cmd, args}` and
//!   [`RpcAuthorizer::authorize_rpc`] decides before anything is forwarded
//!   (default: allow); a `cmd` starting `access_` never reaches a child — the
//!   broker's [`AccessHandler`] answers it (default: `not_found`); and every
//!   client→child WebSocket data frame — **text and binary**, since the PTY
//!   socket takes binary frames as raw stdin — passes [`WsFrameHook`]
//!   (default: pass);
//! * a forwarded path is the path the broker routed and authorized: one with
//!   a dot-segment (raw or percent-encoded) or a backslash is refused, since
//!   the child's URL parser would resolve it to another route.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{request::Parts, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::children::ChildEndpoint;
use super::ws_registry::WsKey;
use super::BrokerState;
use crate::executor::Principal;
use crate::server::auth::{json_error, BoxFuture, Epochs, PrincipalCtx};

/// The largest `/api/rpc` body the broker parses (file writes ride RPC).
pub const MAX_RPC_BODY: usize = 64 * 1024 * 1024;

/// The R-3 verdict on one RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

/// R-3: called with the parsed `{cmd, args}` before an RPC is proxied.
pub trait RpcAuthorizer: Send + Sync {
    fn authorize_rpc<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
        cmd: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Decision>;
}

/// The default: every resolved principal may call every arm of its own
/// child (topology B makes that safe by construction).
#[derive(Debug, Default, Clone, Copy)]
pub struct AllowAll;

impl RpcAuthorizer for AllowAll {
    fn authorize_rpc<'a>(
        &'a self,
        _ctx: &'a PrincipalCtx,
        _req: &'a Parts,
        _cmd: &'a str,
        _args: &'a Value,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async { Decision::Allow })
    }
}

/// R-3: the broker-side `access_*` arms.
pub trait AccessHandler: Send + Sync {
    fn handle<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
        cmd: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Response>;
}

/// The default until WP-74: no `access_*` arm exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct AccessNotFound;

impl AccessHandler for AccessNotFound {
    fn handle<'a>(
        &'a self,
        _ctx: &'a PrincipalCtx,
        _req: &'a Parts,
        cmd: &'a str,
        _args: &'a Value,
    ) -> BoxFuture<'a, Response> {
        let msg = format!("not_found: no access command `{cmd}`");
        Box::pin(async move { json_error(StatusCode::NOT_FOUND, "not_found", &msg) })
    }
}

/// What to do with one client→child data frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDecision {
    Pass,
    /// Swallow this frame; keep the socket.
    Drop,
    /// Swallow this frame and answer the client with this text control
    /// frame (G-ACCESS §1.6's `{type:"error",code:"forbidden",…}` refusal);
    /// keep the socket.
    Reply(String),
    /// Close the socket with this code and reason.
    Close {
        code: u16,
        reason: String,
    },
}

/// One client→child data frame, as the hook sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientFrame<'a> {
    Text(&'a str),
    /// `pty_ws` writes a binary frame straight to the PTY's stdin, so a
    /// read-only / share policy (WP-74) **must** gate these as well as text.
    Binary(&'a [u8]),
}

/// R-3: the client→child WebSocket frame hook. It sees every text **and**
/// binary frame (R-3 names a text-frame hook; binary frames are PTY input
/// too, so a hook that saw only text could be bypassed). Ping, pong and
/// close are control frames and pass.
///
/// `narrowing` is the socket's [`Narrowing`], decided once at the handshake
/// (G-ACCESS §1.4: effective caps are per handshake, never re-read per
/// frame from shared state).
pub trait WsFrameHook: Send + Sync {
    fn client_frame(
        &self,
        ctx: &PrincipalCtx,
        narrowing: &Narrowing,
        path: &str,
        frame: ClientFrame<'_>,
    ) -> FrameDecision;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PassFrames;

impl WsFrameHook for PassFrames {
    fn client_frame(
        &self,
        _ctx: &PrincipalCtx,
        _narrowing: &Narrowing,
        _path: &str,
        _frame: ClientFrame<'_>,
    ) -> FrameDecision {
        FrameDecision::Pass
    }
}

/// G-ACCESS §1.4 / §4.5.2–§4.5.3: what one proxied request or WebSocket is
/// narrowed to, decided **once** — per request, or per WebSocket handshake —
/// by the [`Narrower`] hook before anything is proxied.
#[derive(Clone, Default)]
pub struct Narrowing {
    /// Set on the upstream request after every client `X-Ikenga-*` header
    /// was stripped: `X-Ikenga-Caps` on every request, plus the
    /// `X-Ikenga-Share-*` set on a share (§4.5.3).
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// The child to proxy to. `None` = the caller's own; a share routes into
    /// the Owner's child (§4.5.1 — `access::share::broker_select`, WP-76).
    /// `X-Ikenga-Principal` names the target.
    pub target: Option<Principal>,
    /// The hook's own per-request / per-socket snapshot (WP-74a: the
    /// broker's `AccessCtx`). [`rpc_proxy`] puts the whole `Narrowing` in
    /// the request extensions before [`RpcAuthorizer`] / [`AccessHandler`]
    /// run, and [`WsFrameHook`] receives it per frame, so neither recomputes
    /// it nor reads a shared cache.
    pub snapshot: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for Narrowing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Narrowing")
            .field("headers", &self.headers)
            .field("target", &self.target.as_ref().map(|p| p.id))
            .field("snapshot", &self.snapshot.is_some())
            .finish()
    }
}

impl Narrowing {
    /// A header this narrowing sets, as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.as_str() == name)
            .and_then(|(_, v)| v.to_str().ok())
    }

    /// The hook's snapshot, if it is a `T`.
    pub fn snapshot<T: std::any::Any + Send + Sync>(&self) -> Option<&T> {
        self.snapshot.as_deref().and_then(|s| s.downcast_ref::<T>())
    }
}

/// Why a request was refused before it was proxied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

/// G-ACCESS §4.5.2–§4.5.3: decide a request's / socket's [`Narrowing`]
/// (effective caps, share selection, target child). A refusal answers the
/// client and nothing is proxied (e.g. a share that doesn't exist: `404`).
pub trait Narrower: Send + Sync {
    fn narrow<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
    ) -> BoxFuture<'a, Result<Narrowing, Refusal>>;
}

/// WP-20's default: no narrowing headers, the caller's own child.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoNarrowing;

impl Narrower for NoNarrowing {
    fn narrow<'a>(
        &'a self,
        _ctx: &'a PrincipalCtx,
        _req: &'a Parts,
    ) -> BoxFuture<'a, Result<Narrowing, Refusal>> {
        Box::pin(async { Ok(Narrowing::default()) })
    }
}

fn refused(r: Refusal) -> Response {
    json_error(r.status, r.code, &r.message)
}

/// The narrowing header (§4.5.3). Set only by the broker; a client's copy is
/// stripped with every other `x-ikenga-*` header.
pub const CAPS_HEADER: &str = "x-ikenga-caps";

/// The header the broker adds for the child's logs.
pub const PRINCIPAL_HEADER: &str = "x-ikenga-principal";

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// The header names a `Connection:` value lists: hop-by-hop for this one
/// connection (RFC 9110 §7.6.1), so never forwarded.
fn connection_listed(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The client headers a child may see.
pub fn upstream_headers(client: &HeaderMap) -> HeaderMap {
    let listed = connection_listed(client);
    let mut out = HeaderMap::new();
    for (name, value) in client {
        let n = name.as_str();
        if HOP_BY_HOP.contains(&n)
            || listed.iter().any(|l| l == n)
            || n.starts_with("x-ikenga-")
            || n.starts_with("sec-websocket-")
            || matches!(
                n,
                "host" | "content-length" | "cookie" | "authorization" | "origin"
            )
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// The child response headers a client may see.
fn downstream_headers(child: &HeaderMap) -> HeaderMap {
    let listed = connection_listed(child);
    let mut out = HeaderMap::new();
    for (name, value) in child {
        let n = name.as_str();
        if HOP_BY_HOP.contains(&n) || n == "set-cookie" || listed.iter().any(|l| l == n) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Drop any `token=` from a query: under T1 it grants nothing, and it must
/// not be forwarded as if it were the child's credential. The key is
/// compared percent-decoded (`%74oken=` is the same parameter to the child).
pub fn strip_token_param(query: Option<&str>) -> Option<String> {
    strip_params(query, &["token"])
}

/// Drop every `keys` parameter (compared percent-decoded) from a query.
fn strip_params(query: Option<&str>, keys: &[&str]) -> Option<String> {
    let kept: Vec<&str> = query?
        .split('&')
        .filter(|p| {
            let key = p.split('=').next().unwrap_or_default().replace('+', " ");
            let key = percent_encoding::percent_decode_str(&key).decode_utf8_lossy();
            !p.is_empty() && !keys.contains(&key.as_ref())
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

/// Whether `path` reaches the child as the very route the broker matched:
/// no segment that is, once percent-decoded, `.` or `..` (the child's URL
/// parser resolves those, so `/pkgs/../api/rpc` would arrive as
/// `/api/rpc`), and no backslash, raw or encoded (a URL parser treats it as
/// `/` for http). A path that fails is refused, never normalised.
pub fn is_routable_path(path: &str) -> bool {
    path.starts_with('/')
        && path.split('/').all(|seg| {
            let decoded = percent_encoding::percent_decode_str(seg).collect::<Vec<u8>>();
            decoded != b"." && decoded != b".." && !decoded.contains(&b'\\')
        })
}

/// The path and query to forward — without `token=` and without the share
/// selector `share=` (G-ACCESS §4.5.2 step 6: the broker strips it; the
/// child learns the share only from `X-Ikenga-Share-*`) — or `None` if the
/// path isn't [routable](is_routable_path).
fn path_and_query(uri: &axum::http::Uri) -> Option<String> {
    let path = uri.path();
    if !is_routable_path(path) {
        return None;
    }
    Some(match strip_params(uri.query(), &["token", "share"]) {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    })
}

fn bad_path() -> Response {
    json_error(
        StatusCode::BAD_REQUEST,
        "bad_request",
        "path segments `.`/`..` and backslashes are not allowed",
    )
}

fn child_unavailable(e: &anyhow::Error) -> Response {
    tracing::error!("principal child unavailable: {e:#}");
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "child_unavailable",
        "your workspace process could not be started; try again shortly",
    )
}

/// Forward one HTTP request to the principal's child. A refused connection
/// (the child exited between requests) relaunches it and retries once — the
/// request never reached the old child, so the retry can't double it.
async fn forward(
    state: &BrokerState,
    narrowing: &Narrowing,
    ctx: &PrincipalCtx,
    method: Method,
    path_and_query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Response {
    let mut headers = upstream_headers(headers);
    for (name, value) in &narrowing.headers {
        headers.insert(name.clone(), value.clone());
    }
    let target = narrowing.target.as_ref().unwrap_or(&ctx.principal);
    let principal = target.id.to_string();
    for attempt in 0..2 {
        let endpoint = match state.children.endpoint(target).await {
            Ok(e) => e,
            Err(e) => return child_unavailable(&e),
        };
        let sent = state
            .http
            .request(
                method.clone(),
                format!("http://{}{path_and_query}", endpoint.addr),
            )
            .headers(headers.clone())
            .bearer_auth(&*endpoint.token)
            .header(PRINCIPAL_HEADER, &principal)
            .body(body.clone())
            .send()
            .await;
        match sent {
            Ok(resp) => {
                let mut out = Response::builder().status(resp.status());
                if let Some(h) = out.headers_mut() {
                    *h = downstream_headers(resp.headers());
                }
                return out
                    .body(Body::from_stream(resp.bytes_stream()))
                    .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response());
            }
            Err(e) if e.is_connect() && attempt == 0 => {
                state.children.invalidate(target.id, &endpoint).await;
            }
            Err(e) => {
                tracing::warn!("proxy to principal child {}: {e}", target.id);
                return json_error(
                    StatusCode::BAD_GATEWAY,
                    "bad_gateway",
                    "your workspace process did not answer",
                );
            }
        }
    }
    json_error(
        StatusCode::BAD_GATEWAY,
        "bad_gateway",
        "your workspace process did not answer",
    )
}

/// `POST /api/rpc` → R-3 → the principal's child.
pub async fn rpc_proxy(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    req: Request,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_RPC_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "request body too large",
            )
        }
    };
    // Fail closed: what the broker can't parse it can't authorize.
    let Ok(Value::Object(mut payload)) = serde_json::from_slice::<Value>(&bytes) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "expected a JSON object {cmd, args}",
        );
    };
    let Some(Value::String(cmd)) = payload.remove("cmd") else {
        return json_error(StatusCode::BAD_REQUEST, "bad_request", "missing `cmd`");
    };
    let args = payload.remove("args").unwrap_or(Value::Null);

    // Decided once; the authorizer and the access handler read it back from
    // the request extensions.
    let narrowing = match state.hooks.narrower.narrow(&ctx, &parts).await {
        Ok(n) => n,
        Err(r) => return refused(r),
    };
    parts.extensions.insert(narrowing.clone());

    if cmd.starts_with("access_") {
        return state.hooks.access.handle(&ctx, &parts, &cmd, &args).await;
    }
    match state
        .hooks
        .authorizer
        .authorize_rpc(&ctx, &parts, &cmd, &args)
        .await
    {
        Decision::Allow => {}
        Decision::Deny {
            status,
            code,
            message,
        } => return json_error(status, code, &message),
    }
    forward(
        &state,
        &narrowing,
        &ctx,
        Method::POST,
        "/api/rpc",
        &parts.headers,
        bytes,
    )
    .await
}

/// `GET /pkgs/*` → the principal's child (its read-only pkg server).
pub async fn pkgs_proxy(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    req: Request,
) -> Response {
    let (parts, _body) = req.into_parts();
    if !matches!(parts.method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(pq) = path_and_query(&parts.uri) else {
        return bad_path();
    };
    let narrowing = match state.hooks.narrower.narrow(&ctx, &parts).await {
        Ok(n) => n,
        Err(r) => return refused(r),
    };
    forward(
        &state,
        &narrowing,
        &ctx,
        parts.method.clone(),
        &pq,
        &parts.headers,
        Bytes::new(),
    )
    .await
}

/// How long the broker waits for a child to accept a WebSocket handshake.
pub const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

type Upstream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect_upstream(
    endpoint: &ChildEndpoint,
    target: &Principal,
    path_and_query: &str,
    narrowing: &Narrowing,
) -> Result<Upstream, tungstenite::Error> {
    let mut request = format!("ws://{}{path_and_query}", endpoint.addr).into_client_request()?;
    let h = request.headers_mut();
    h.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {}", endpoint.token))
            .map_err(|e| tungstenite::Error::HttpFormat(e.into()))?,
    );
    h.insert(
        HeaderName::from_static(PRINCIPAL_HEADER),
        HeaderValue::from_str(&target.id.to_string())
            .map_err(|e| tungstenite::Error::HttpFormat(e.into()))?,
    );
    for (name, value) in &narrowing.headers {
        h.insert(name.clone(), value.clone());
    }
    let (ws, _) = tokio::time::timeout(
        UPSTREAM_CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| {
        tungstenite::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the principal child did not complete the WebSocket handshake",
        ))
    })??;
    Ok(ws)
}

fn is_refused(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused)
}

/// `GET /ws/*` (upgrade) → the principal's child. The upstream socket is
/// opened **before** the client's upgrade completes, so a child that can't
/// be reached is an HTTP error, not an instantly-closed socket.
///
/// I-8 across the handshake: the socket is registered **first**, before a
/// child is launched or reached (that can take up to `READY_TIMEOUT`), so a
/// logout, password change or epoch bump that lands meanwhile reaches it —
/// and one that landed between resolution and registration is caught by
/// [`WsRegistry::register`](super::ws_registry::WsRegistry::register)
/// itself. Then the [`StillValid`](super::ws_registry::StillValid) check
/// runs once, for CLI writes whose re-check pass already went by.
pub async fn ws_proxy(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    Extension(epochs): Extension<Epochs>,
    req: Request,
) -> Response {
    let (mut parts, _body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let uri = parts.uri.clone();
    let Some(pq) = path_and_query(&uri) else {
        return bad_path();
    };
    let key = WsKey::from_ctx(&ctx, epochs);
    let mut registration = state.ws.register(key.clone());
    match state.hooks.still_valid.still_valid(&key).await {
        Ok(true) => {}
        Ok(false) => return revoked_handshake(),
        Err(e) => {
            tracing::warn!("ws handshake re-check for {}: {e:#}", ctx.principal.id);
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "could not confirm the session; try again",
            );
        }
    }
    // G-ACCESS §1.4: the socket's caps (and share selection) are decided
    // once, here; the frame hook reads this snapshot for the socket's life.
    let narrowing = match state.hooks.narrower.narrow(&ctx, &parts).await {
        Ok(n) => n,
        Err(r) => return refused(r),
    };
    let target = narrowing
        .target
        .clone()
        .unwrap_or_else(|| ctx.principal.clone());
    let mut upstream = None;
    for attempt in 0..2 {
        let endpoint = match state.children.endpoint(&target).await {
            Ok(e) => e,
            Err(e) => return child_unavailable(&e),
        };
        match connect_upstream(&endpoint, &target, &pq, &narrowing).await {
            Ok(up) => {
                upstream = Some(up);
                break;
            }
            Err(e) if is_refused(&e) && attempt == 0 => {
                state.children.invalidate(target.id, &endpoint).await;
            }
            Err(tungstenite::Error::Http(resp)) => {
                let status =
                    StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
                return status.into_response();
            }
            Err(e) => {
                tracing::warn!("ws proxy to principal child {}: {e}", target.id);
                return StatusCode::BAD_GATEWAY.into_response();
            }
        }
    }
    let Some(mut upstream) = upstream else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    // Revoked while the child was launched or reached: refuse the upgrade.
    if registration.revoked().is_some() {
        let _ = upstream.close(None).await;
        return revoked_handshake();
    }
    let path = uri.path().to_string();
    ws.on_upgrade(move |client| async move {
        pump(state, ctx, narrowing, path, client, upstream, registration).await;
    })
}

fn revoked_handshake() -> Response {
    json_error(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "the session was revoked",
    )
}

fn to_upstream(msg: Message) -> tungstenite::Message {
    match msg {
        Message::Text(t) => tungstenite::Message::Text(t),
        Message::Binary(b) => tungstenite::Message::Binary(b),
        Message::Ping(p) => tungstenite::Message::Ping(p),
        Message::Pong(p) => tungstenite::Message::Pong(p),
        Message::Close(f) => {
            tungstenite::Message::Close(f.map(|f| tungstenite::protocol::CloseFrame {
                code: f.code.into(),
                reason: f.reason,
            }))
        }
    }
}

fn to_client(msg: tungstenite::Message) -> Option<Message> {
    Some(match msg {
        tungstenite::Message::Text(t) => Message::Text(t),
        tungstenite::Message::Binary(b) => Message::Binary(b),
        tungstenite::Message::Ping(p) => Message::Ping(p),
        tungstenite::Message::Pong(p) => Message::Pong(p),
        tungstenite::Message::Close(f) => Message::Close(f.map(|f| CloseFrame {
            code: f.code.into(),
            reason: f.reason,
        })),
        tungstenite::Message::Frame(_) => return None,
    })
}

fn close_frame(code: u16, reason: impl Into<String>) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into().into(),
    }))
}

/// Shuttle frames both ways until either side closes or the registry says
/// the credential was revoked (then: close `4401` to the client, close the
/// child's socket — the PTY or run behind it lives on).
async fn pump(
    state: Arc<BrokerState>,
    ctx: PrincipalCtx,
    narrowing: Narrowing,
    path: String,
    client: WebSocket,
    upstream: Upstream,
    mut registration: super::ws_registry::WsRegistration,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();
    loop {
        tokio::select! {
            reason = &mut registration.closed => {
                if let Ok(reason) = reason {
                    let _ = client_tx.send(close_frame(reason.code, reason.reason)).await;
                }
                let _ = up_tx.send(tungstenite::Message::Close(None)).await;
                break;
            }
            msg = client_rx.next() => match msg {
                Some(Ok(Message::Close(frame))) => {
                    let _ = up_tx.send(to_upstream(Message::Close(frame))).await;
                    break;
                }
                Some(Ok(msg)) => {
                    let decision = match &msg {
                        Message::Text(t) => {
                            state.hooks.ws_frames.client_frame(&ctx, &narrowing, &path, ClientFrame::Text(t))
                        }
                        Message::Binary(b) => {
                            state.hooks.ws_frames.client_frame(&ctx, &narrowing, &path, ClientFrame::Binary(b))
                        }
                        // Ping / pong: control frames, not input.
                        _ => FrameDecision::Pass,
                    };
                    match decision {
                        FrameDecision::Pass => {
                            if up_tx.send(to_upstream(msg)).await.is_err() {
                                let _ = client_tx.send(close_frame(1011, "upstream gone")).await;
                                break;
                            }
                        }
                        FrameDecision::Drop => {}
                        FrameDecision::Reply(text) => {
                            if client_tx.send(Message::Text(text)).await.is_err() {
                                let _ = up_tx.send(tungstenite::Message::Close(None)).await;
                                break;
                            }
                        }
                        FrameDecision::Close { code, reason } => {
                            let _ = client_tx.send(close_frame(code, reason)).await;
                            let _ = up_tx.send(tungstenite::Message::Close(None)).await;
                            break;
                        }
                    }
                }
                Some(Err(_)) | None => {
                    let _ = up_tx.send(tungstenite::Message::Close(None)).await;
                    break;
                }
            },
            msg = up_rx.next() => match msg {
                Some(Ok(m)) => {
                    let closing = matches!(m, tungstenite::Message::Close(_));
                    if let Some(m) = to_client(m) {
                        if client_tx.send(m).await.is_err() {
                            let _ = up_tx.send(tungstenite::Message::Close(None)).await;
                            break;
                        }
                    }
                    if closing {
                        break;
                    }
                }
                Some(Err(_)) | None => {
                    let _ = client_tx.send(close_frame(1011, "upstream gone")).await;
                    break;
                }
            },
        }
    }
    let _ = client_tx.close().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_x_ikenga_headers_cookies_and_credentials_never_reach_a_child() {
        let mut h = HeaderMap::new();
        for (k, v) in [
            ("x-ikenga-principal", "someone-else"),
            ("x-ikenga-caps", "all"),
            ("X-Ikenga-Share-Owner", "x"),
            ("cookie", "ikenga_session=abc"),
            ("authorization", "Bearer guessed"),
            ("origin", "https://ik.example"),
            ("host", "ik.example"),
            ("connection", "keep-alive"),
            ("content-type", "application/json"),
            ("accept-language", "en"),
        ] {
            h.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        let up = upstream_headers(&h);
        let names: Vec<&str> = up.keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["content-type", "accept-language"]);
    }

    #[test]
    fn token_params_are_dropped_from_forwarded_queries() {
        assert_eq!(strip_token_param(None), None);
        assert_eq!(strip_token_param(Some("token=abc")), None);
        assert_eq!(
            strip_token_param(Some("spawn=true&token=abc&cols=80")).as_deref(),
            Some("spawn=true&cols=80")
        );
        assert_eq!(
            strip_token_param(Some("tokens=1")).as_deref(),
            Some("tokens=1")
        );
        // S3-5: an encoded key is the same parameter to the child.
        assert_eq!(
            strip_token_param(Some("%74oken=abc&a=1&%54OKEN=x")).as_deref(),
            Some("a=1&%54OKEN=x")
        );
        // G-ACCESS §4.5.2 step 6: the share selector is stripped too.
        assert_eq!(
            strip_params(Some("share=o%2Fp&cols=80&%73hare=x"), &["token", "share"]).as_deref(),
            Some("cols=80")
        );
    }

    /// S3-5: the child must see the route the broker matched.
    #[test]
    fn dot_segments_and_backslashes_are_not_routable() {
        for ok in [
            "/pkgs/studio/index.html",
            "/ws/pty/abc",
            "/pkgs/a/..b/c.js",
            "/pkgs/a/.hidden",
            "/pkgs/a//b",
        ] {
            assert!(is_routable_path(ok), "{ok}");
        }
        for bad in [
            "/pkgs/../api/rpc",
            "/pkgs/./x",
            "/pkgs/%2e%2e/api/rpc",
            "/pkgs/.%2E/api/rpc",
            "/pkgs/%2e/x",
            "/pkgs/a/..",
            "/pkgs/..%5capi/rpc",
            "/pkgs/a\\b",
            "pkgs/x",
        ] {
            assert!(!is_routable_path(bad), "{bad}");
        }
    }

    /// S3-8: a header named in `Connection:` is hop-by-hop for that hop.
    #[test]
    fn headers_named_by_connection_are_not_forwarded() {
        let mut h = HeaderMap::new();
        h.insert("connection", "close, X-Foo".parse().unwrap());
        h.insert("x-foo", "1".parse().unwrap());
        h.insert("x-bar", "2".parse().unwrap());
        let up = upstream_headers(&h);
        let names: Vec<&str> = up.keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["x-bar"]);
        let down = downstream_headers(&h);
        let names: Vec<&str> = down.keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["x-bar"]);
    }
}
