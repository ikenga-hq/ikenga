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
pub mod pkg_static;
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

use crate::engines::EngineRegistry;
use crate::pty::PtyManager;
pub use health::health_handler;
pub use pkg_static::PkgStaticService;
pub use static_files::SpaStaticService;

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
    /// Handle on `<data_dir>/ikenga.db`, backing the `db_query` / `db_exec`
    /// RPC arms. `None` when the operator gave no `--data-dir`: there is no
    /// sane default path for a daemon, and silently picking one would create
    /// an empty database that looks authoritative. Those two arms then return
    /// an error naming the missing flag, which is what the frontend's
    /// `sql-shim` already degrades on.
    pub pa_db: Option<Arc<crate::db::PaDb>>,
    /// Read-only view of `--pkgs-dir`, backing `GET /pkgs/:id/*`. Built once
    /// at router construction; empty when no `--pkgs-dir` was given, in which
    /// case the route exists but 404s. See `server::pkg_static`.
    pub pkg_static: PkgStaticService,
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
    pa_db: Option<Arc<crate::db::PaDb>>,
) -> Router {
    let spa_service = SpaStaticService::new(&config.static_dir);
    // Walked here rather than in `run_server` so that every router — tests
    // included — gets the same view of `--pkgs-dir`. Logs the ids it found.
    let pkg_static = PkgStaticService::discover(config.pkgs_dir.as_deref());
    let allowed_origins = config.allowed_origins.clone();
    let state = Arc::new(AppState {
        config,
        spa_service: spa_service.clone(),
        pty_manager,
        engine_registry,
        pa_db,
        pkg_static,
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
        // Installed pkg bundles, read-only. Inside the protected group on
        // purpose: pkg content is code, and the bearer token is the whole
        // trust boundary. No second per-mount token is minted.
        // All three forms are needed. `/*path` does NOT match an empty tail,
        // so without the explicit `/pkgs/:id/` route a trailing slash falls
        // through to the SPA fallback — which is OUTSIDE this auth layer, so
        // `<iframe src="/pkgs/x/">` silently rendered the shell's index.html
        // with no token at all. Verified against a live daemon; keep all three.
        .route("/pkgs/:id", get(pkg_static::pkg_static_root_handler))
        .route("/pkgs/:id/", get(pkg_static::pkg_static_root_handler))
        .route("/pkgs/:id/*path", get(pkg_static::pkg_static_file_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    Router::new()
        .route("/api/health", get(health::health_handler))
        .merge(protected_routes)
        .fallback(spa_fallback_handler)
        .layer(cors)
        .with_state(state)
}

async fn spa_fallback_handler(State(state): State<Arc<AppState>>, uri: Uri) -> impl IntoResponse {
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

    let mut pa_db: Option<Arc<crate::db::PaDb>> = None;
    // `/api/rpc`'s fs_* commands resolve every path through the same
    // allowlist the desktop app enforces. Without this the resolver has no
    // roots installed and refuses all paths — which is the safe direction,
    // but not the useful one. `--data-dir` is also where `ikenga.db` lives.
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

        // Sharing one `ikenga.db` with a running desktop app is not supported:
        // `db::ensure_schema` reads the applied-migration set and writes the
        // bookkeeping row without an enclosing transaction, so two processes
        // racing a fresh migration can leave one of them failing on the
        // `_pa_migrations` primary key. These two files only ever exist in a
        // desktop profile, so their presence is the cheap tell.
        for probe in ["secrets.stronghold", "pa.db"] {
            if data_dir.join(probe).exists() {
                warn!(
                    "--data-dir {} already contains a desktop profile ({probe}). \
                     ikenga.db is not safe to share with a running desktop app — \
                     migrations are applied without a cross-process lock. Point \
                     the daemon at its own directory.",
                    data_dir.display()
                );
            }
        }

        // Opened lazily: `PaDb::new` only records the path. The pools (and the
        // migration apply) happen on the first `db_query` / `db_exec`, so a
        // daemon nobody queries never touches the file.
        pa_db = Some(Arc::new(crate::db::PaDb::new(data_dir.join("ikenga.db"))));
    } else {
        warn!(
            "no --data-dir: fs_* RPC commands will reject every path and \
             db_query/db_exec have no database to open"
        );
    }

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let pty_manager = Arc::new(PtyManager::new());
    let engine_registry = Arc::new(EngineRegistry::new());
    {
        let antigravity_engine =
            Arc::new(crate::engines::antigravity_acp::AntigravityEngine::new());
        let antigravity_handle = crate::engines::EngineHandle::Antigravity(antigravity_engine);
        engine_registry
            .insert("antigravity", antigravity_handle.clone())
            .await;
        engine_registry
            .insert("antigravity-cli", antigravity_handle)
            .await;
    }
    let token = config.auth_token.clone().unwrap_or_default();
    let router = create_router(config.clone(), pty_manager, engine_registry, pa_db);

    info!(
        "ikenga-server listening on http://{} (static assets: {})",
        addr,
        config.static_dir.display()
    );
    // The opening link carries the bearer token, and that token grants a shell.
    // Print it ONLY when the daemon minted an ephemeral one for this run and is
    // therefore attached to somebody's terminal — a minted token dies with the
    // process, so the console is the only place it can come from.
    //
    // A configured token must never be logged. Under systemd this call goes to
    // journald, where it persists, is readable by anyone in `systemd-journal`,
    // and outlives every rotation of the credential itself.
    if minted {
        info!("no --auth-token given; minted an ephemeral one for this run");
        info!("open: http://{addr}/?token={token}");
    } else {
        info!("open: http://{addr}/?token=<IKENGA_AUTH_TOKEN>");
        info!("token is the configured one; read it from the env file, not from this log");
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
