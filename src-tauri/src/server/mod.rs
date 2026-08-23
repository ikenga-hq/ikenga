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
use axum::http::{StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tower_http::cors::{Any, CorsLayer};
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
    pub auth_token: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub config: ServerConfig,
    pub spa_service: SpaStaticService,
    pub pty_manager: Arc<PtyManager>,
    pub engine_registry: Arc<EngineRegistry>,
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, Response> {
    if let Some(ref expected_token) = state.config.auth_token {
        // 1. Check Authorization header: Bearer <TOKEN>
        if let Some(auth_header) = req.headers().get("authorization").and_then(|h| h.to_str().ok()) {
            if auth_header.starts_with("Bearer ") {
                let token = &auth_header[7..];
                if token == expected_token {
                    return Ok(next.run(req).await);
                }
            }
        }

        // 2. Check query param: ?token=<TOKEN>
        if let Some(query) = req.uri().query() {
            for param in query.split('&') {
                if let Some((k, v)) = param.split_once('=') {
                    if k == "token" && v == expected_token {
                        return Ok(next.run(req).await);
                    }
                }
            }
        }

        warn!("Unauthorized request to {}", req.uri().path());
        let error_body = Json(serde_json::json!({
            "ok": false,
            "error": "Unauthorized: invalid or missing auth token"
        }));
        return Err((StatusCode::UNAUTHORIZED, error_body).into_response());
    }

    Ok(next.run(req).await)
}

pub fn create_router(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
) -> Router {
    let spa_service = SpaStaticService::new(&config.static_dir);
    let state = Arc::new(AppState {
        config,
        spa_service: spa_service.clone(),
        pty_manager,
        engine_registry,
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

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

pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    health::init_uptime();
    
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let pty_manager = Arc::new(PtyManager::new());
    let engine_registry = Arc::new(EngineRegistry::new());
    let router = create_router(config.clone(), pty_manager, engine_registry);

    info!(
        "ikenga-server listening on http://{} (static assets: {}, auth: {})",
        addr,
        config.static_dir.display(),
        if config.auth_token.is_some() { "enabled" } else { "disabled" }
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;

    Ok(())
}
