//! Headless HTTP/WebSocket host for the shell.
//!
//! Serves the built SPA, a JSON-RPC-ish `/api/rpc` surface, and binary PTY
//! WebSockets so the same frontend bundle can run in a browser against a
//! remote machine instead of inside the Tauri webview.
//!
//! **Security posture.** Every request this daemon can serve is capable of
//! running code or touching the user's files, so the auth token is *not*
//! optional: `run_server` mints one when the operator doesn't supply it and
//! prints it with the ready URL. Cross-origin access is denied by default —
//! WebSockets are exempt from CORS, so the `Origin` header is checked
//! explicitly on every protected route rather than left to the browser.

pub mod chat_ws;
pub mod fs_ws;
pub mod health;
pub mod pty_ws;
pub mod rpc;
pub mod static_files;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{info, warn};

pub use health::health_handler;
pub use static_files::SpaStaticService;
use crate::engines::EngineRegistry;
use crate::pty::PtyManager;

#[derive(Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub static_dir: PathBuf,
    pub pkgs_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    /// Bearer token required on every protected route. `None` here only
    /// means "the operator didn't pick one" — `run_server` mints one before
    /// the listener binds, so the running server always has a token.
    pub auth_token: Option<String>,
    /// Extra origins permitted to call the API cross-site (e.g. a Vite dev
    /// server). Empty means same-origin only.
    pub allowed_origins: Vec<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub spa_service: SpaStaticService,
    pub pty_manager: Arc<PtyManager>,
    pub engine_registry: Arc<EngineRegistry>,
}

/// Compare two secrets without leaking their common prefix through timing.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn unauthorized(reason: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "ok": false, "error": reason })),
    )
        .into_response()
}

/// Reject cross-site requests. Browsers attach `Origin` to every WebSocket
/// handshake and to non-simple fetches, and they refuse to let a page forge
/// it — so an `Origin` that isn't ours means another site is driving us.
/// A missing `Origin` is a non-browser client (curl, the CLI) and is allowed;
/// those can't be steered by a page the user happens to be visiting.
fn origin_permitted(req: &Request, state: &AppState) -> bool {
    let Some(origin) = req.headers().get("origin").and_then(|h| h.to_str().ok()) else {
        return true;
    };
    if state
        .config
        .allowed_origins
        .iter()
        .any(|allowed| ct_eq(allowed, origin))
    {
        return true;
    }
    // Same-origin: the Origin's host:port matches the Host we were reached on.
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    origin
        .split_once("://")
        .map(|(_, o)| o == host)
        .unwrap_or(false)
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    if !origin_permitted(&req, &state) {
        warn!(
            "Cross-origin request to {} rejected (origin: {:?})",
            req.uri().path(),
            req.headers().get("origin")
        );
        return Err(unauthorized("Forbidden: cross-origin request"));
    }

    // `run_server` guarantees this is populated; a `None` here means the
    // router was built directly (tests) and we still refuse to serve.
    let Some(ref expected) = state.config.auth_token else {
        warn!("Rejecting {} — server has no auth token", req.uri().path());
        return Err(unauthorized("Unauthorized: server has no auth token"));
    };

    // 1. Authorization: Bearer <TOKEN>
    if let Some(header) = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        if ct_eq(header, expected) {
            return Ok(next.run(req).await);
        }
    }

    // 2. ?token=<TOKEN> — the only way to authenticate a WebSocket handshake
    //    from a browser, which cannot set request headers on `new WebSocket`.
    if let Some(query) = req.uri().query() {
        for param in query.split('&') {
            if let Some((k, v)) = param.split_once('=') {
                if k != "token" {
                    continue;
                }
                let decoded = percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned();
                if ct_eq(&decoded, expected) {
                    return Ok(next.run(req).await);
                }
            }
        }
    }

    warn!("Unauthorized request to {}", req.uri().path());
    Err(unauthorized("Unauthorized: invalid or missing auth token"))
}

pub fn create_router(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
) -> Router {
    let spa_service = SpaStaticService::new(&config.static_dir);
    let allowed_origins = config.allowed_origins.clone();
    let state = Arc::new(AppState {
        config,
        spa_service: spa_service.clone(),
        pty_manager,
        engine_registry,
    });

    // Same-origin needs no CORS headers at all; anything else has to be named
    // explicitly. `Any` here would have let every page on the internet call
    // the RPC surface.
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    // Protected API and WebSocket endpoints
    let protected_routes = Router::new()
        .route("/api/rpc", post(rpc::rpc_handler))
        .route("/ws/pty/:id", get(pty_ws::pty_ws_handler))
        .route("/ws/chat/:id", get(chat_ws::chat_ws_handler))
        .route("/ws/fs", get(fs_ws::fs_ws_handler))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

    Router::new()
        .route("/api/health", get(health::health_handler))
        .merge(protected_routes)
        .fallback(spa_fallback_handler)
        .layer(cors)
        .with_state(state)
}

async fn spa_fallback_handler(
    State(state): State<Arc<AppState>>,
    uri: Uri,
) -> impl IntoResponse {
    state.spa_service.handle(uri).await
}

pub async fn run_server(mut config: ServerConfig) -> anyhow::Result<()> {
    health::init_uptime();

    // Fail closed: an operator who forgets `--auth-token` gets a generated
    // one, never an open shell. Printed below with the ready URL.
    let minted = config.auth_token.is_none();
    if minted {
        config.auth_token = Some(uuid::Uuid::new_v4().simple().to_string());
    }

    // `/api/rpc`'s fs_* commands resolve every path through the same
    // allowlist the desktop app enforces. Without this the resolver has no
    // roots installed and refuses all paths — which is the safe direction,
    // but not the useful one.
    if let Some(ref data_dir) = config.data_dir {
        std::fs::create_dir_all(data_dir)?;
        match crate::fs_roots::FsRoots::load(data_dir.join("fs_roots.json")) {
            Ok(roots) => {
                if let Err(e) = crate::fs_roots::install(Arc::new(roots)) {
                    warn!("fs_roots install failed: {e:#}");
                }
            }
            Err(e) => warn!("fs_roots load failed: {e:#}"),
        }
    } else {
        warn!("no --data-dir: fs_* RPC commands will reject every path");
    }

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let pty_manager = Arc::new(PtyManager::new());
    let engine_registry = Arc::new(EngineRegistry::new());
    let token = config.auth_token.clone().unwrap_or_default();
    let router = create_router(config.clone(), pty_manager, engine_registry);

    info!(
        "ikenga-server listening on http://{} (static assets: {})",
        addr,
        config.static_dir.display()
    );
    if minted {
        info!("no --auth-token given; minted one for this run");
    }
    info!("open: http://{addr}/?token={token}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
