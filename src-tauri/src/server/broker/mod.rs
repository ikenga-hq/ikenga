//! The T1 broker (G-PRINCIPAL §3, topology B): the root process that owns
//! auth, accounts, the probe, the static SPA and a reverse proxy, and
//! lazily runs one unprivileged `ikenga-server` child per principal.
//!
//! ```text
//!  browser ──cookie──▶ broker (root)                         principal child (uid)
//!                      ├─ /api/health, /auth/login, SPA       (no auth)
//!                      ├─ /access/pair/*, /access/invite/*    (extension point, R16 §14.1)
//!                      └─ require_principal ─▶ PrincipalCtx
//!                          ├─ /auth/{logout,me,password}
//!                          ├─ /api/rpc ─ R-3 authorize / access_* ─▶ 127.0.0.1:<port> /api/rpc
//!                          ├─ /pkgs/*  ──────────────────────────▶ 127.0.0.1:<port> /pkgs/*
//!                          └─ /ws/*    ─ ws_registry ───────────▶ 127.0.0.1:<port> /ws/*
//! ```
//!
//! Module map (G-ACCESS R-6): [`children`] launches and reaps children,
//! [`proxy`] forwards (with the R-3 hooks), [`ws_registry`] tracks and
//! revokes open sockets. Credential resolution is `server::auth`; account
//! provisioning is `server::operator`.
//!
//! Under T1 the operator bearer grants nothing (§2.4, I-6): the broker mints
//! none, accepts none, and `/api/shutdown` doesn't exist here.

pub mod children;
pub mod proxy;
pub mod ws_registry;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, Uri};
use axum::middleware;
use axum::response::Response;
use axum::routing::{any, get, post};
use axum::Router;
use axum_login::AuthManagerLayerBuilder;
use sqlx::{Connection, SqliteConnection, SqlitePool};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_sessions::ExpiredDeletion;

use self::children::{ChildLauncher, Children, T1Launcher};
use self::proxy::{
    AccessHandler, AccessNotFound, AllowAll, PassFrames, RpcAuthorizer, WsFrameHook,
};
use self::ws_registry::{AccountEpochs, StillValid, WsRegistry};
use super::auth::backend::{self, AccountsBackend, BrokerSessionStore, SessionCookieResolver};
use super::auth::{self as auth_mod, json_error, PublicRoutes, Resolvers};
use super::operator::accounts::{self, Actor};
use super::operator::password::LoginVerifier;
use super::operator::provision::{BootstrapAdmin, Provisioner, ProvisioningMode, UidRange};
use super::operator::{open_accounts, Opener, OperatorRoot};
use super::{discovery, health, static_files::SpaStaticService, ServerConfig};
use crate::executor::t1::T1Executor;

/// The R-3 / R-5 hook points, with their WP-20 defaults. WP-74 swaps them.
#[derive(Clone)]
pub struct BrokerHooks {
    pub authorizer: Arc<dyn RpcAuthorizer>,
    pub access: Arc<dyn AccessHandler>,
    pub ws_frames: Arc<dyn WsFrameHook>,
    pub still_valid: Arc<dyn StillValid>,
}

impl BrokerHooks {
    /// Allow every RPC, no `access_*` arms, pass every frame, and close a
    /// socket once its account is disabled or its `session_epoch` moved.
    pub fn defaults(pool: SqlitePool) -> Self {
        Self {
            authorizer: Arc::new(AllowAll),
            access: Arc::new(AccessNotFound),
            ws_frames: Arc::new(PassFrames),
            still_valid: Arc::new(AccountEpochs { pool }),
        }
    }
}

/// Everything a broker handler needs.
pub struct BrokerState {
    pub pool: SqlitePool,
    pub verifier: Arc<LoginVerifier>,
    pub children: Arc<Children>,
    pub ws: Arc<WsRegistry>,
    pub http: reqwest::Client,
    pub hooks: BrokerHooks,
}

impl BrokerState {
    pub fn new(
        pool: SqlitePool,
        verifier: Arc<LoginVerifier>,
        launcher: Arc<dyn ChildLauncher>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            // Loopback only: never through an operator's HTTP(S)_PROXY.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self {
            hooks: BrokerHooks::defaults(pool.clone()),
            pool,
            verifier,
            children: Arc::new(Children::new(launcher)),
            ws: WsRegistry::new(),
            http,
        })
    }
}

/// What WP-74 / WP-76 add without restructuring: more credential resolvers
/// (appended after the session cookie, R-4) and the public pairing/invite
/// routes (Round 16 §14.1).
pub struct BrokerExtensions {
    pub resolvers: Resolvers,
    pub public: PublicRoutes,
    /// G-ACCESS §3.9: device-cookie rotation (and clearing a dead cookie)
    /// on authenticated requests. `None` without an access store.
    pub device_cookies: Option<Arc<backend::DeviceCookieRotation>>,
}

impl Default for BrokerExtensions {
    fn default() -> Self {
        Self {
            resolvers: Resolvers::new(Arc::new(SessionCookieResolver)),
            public: PublicRoutes::default(),
            device_cookies: None,
        }
    }
}

/// `AccessStatus.principal` under T1: username and `is_admin` from
/// `accounts` (G-ACCESS §9.1).
struct AccountsDirectory {
    pool: SqlitePool,
}

impl crate::access::PrincipalDirectory for AccountsDirectory {
    fn lookup<'a>(
        &'a self,
        principal_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<(String, bool)>> + Send + 'a>>
    {
        Box::pin(async move {
            let id = principal_id.parse().ok()?;
            let mut conn = self.pool.acquire().await.ok()?;
            let account = accounts::by_id(&mut conn, id).await.ok()??;
            Some((account.username, account.is_admin))
        })
    }
}

async fn api_not_found() -> Response {
    json_error(StatusCode::NOT_FOUND, "not_found", "no such route")
}

/// The broker's router. Unauthenticated: `/api/health`, `/auth/login`, the
/// SPA fallback and the extension routes. Everything else resolves a
/// [`auth_mod::PrincipalCtx`] first (I-6). The `Origin` gate covers every
/// state-changing route and WebSocket handshake.
pub fn router(
    state: Arc<BrokerState>,
    store: BrokerSessionStore,
    static_dir: &std::path::Path,
    allowed_origins: Vec<String>,
    insecure_cookie: bool,
    extensions: BrokerExtensions,
) -> Router {
    let auth_layer = AuthManagerLayerBuilder::new(
        AccountsBackend {
            pool: state.pool.clone(),
            verifier: state.verifier.clone(),
        },
        backend::session_layer(store, insecure_cookie),
    )
    .build();

    let mut protected = Router::new()
        .route("/api/rpc", post(proxy::rpc_proxy))
        .route("/ws/*rest", get(proxy::ws_proxy))
        .route("/pkgs/*rest", get(proxy::pkgs_proxy))
        .route("/auth/logout", post(auth_mod::routes::logout))
        .route("/auth/me", get(auth_mod::routes::me))
        .route("/auth/password", post(auth_mod::routes::change_password))
        // `/api/shutdown` and anything else under /api: no such route here,
        // and an unauthenticated caller can't even learn that.
        .route("/api/*rest", any(api_not_found));
    // Inner to `require_principal`: runs once the caller is resolved.
    if let Some(rotation) = extensions.device_cookies {
        protected = protected.route_layer(middleware::from_fn_with_state(
            rotation,
            backend::rotate_device_cookie,
        ));
    }
    let protected = protected
        .route_layer(middleware::from_fn_with_state(
            extensions.resolvers,
            auth_mod::require_principal,
        ))
        .with_state(state.clone());

    let public = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/auth/login", post(auth_mod::routes::login))
        .with_state(state)
        .merge(extensions.public.into_router());

    let spa = SpaStaticService::new(static_dir);
    let origins: Vec<HeaderValue> = allowed_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    Router::new()
        .merge(public)
        .merge(protected)
        .fallback(move |uri: Uri| {
            let spa = spa.clone();
            async move { spa.handle(uri).await }
        })
        .layer(auth_layer)
        .layer(middleware::from_fn_with_state(
            Arc::new(allowed_origins),
            auth_mod::origin_gate,
        ))
        .layer(cors)
}

/// What `t1_boot` hands the broker once its §8 probe has passed and the
/// stamped executor is installed.
pub struct BrokerBoot {
    pub config: ServerConfig,
    pub executor: Arc<T1Executor>,
    pub root: OperatorRoot,
    pub uid_range: UidRange,
    pub provisioning: ProvisioningMode,
    pub bootstrap: Option<BootstrapAdmin>,
    pub insecure_cookie: bool,
    /// The Part B flags (G-ACCESS §10.1).
    pub access: crate::access::AccessOptions,
}

/// Refuse to exec a binary a principal could have replaced (I-9: nothing a
/// principal owns is ever executed by root — nor by root on its behalf).
fn child_exe() -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let exe = std::env::current_exe()?;
    let meta = std::fs::metadata(&exe)?;
    if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        anyhow::bail!(
            "{} must be root-owned and not group/world-writable to launch principal children \
             (owner {}, mode {:o})",
            exe.display(),
            meta.uid(),
            meta.mode() & 0o7777
        );
    }
    Ok(exe)
}

/// Run the broker until shutdown.
pub async fn serve(boot: BrokerBoot) -> anyhow::Result<()> {
    let BrokerBoot {
        config,
        executor,
        root,
        uid_range,
        provisioning,
        bootstrap,
        insecure_cookie,
        access,
    } = boot;

    // Only the broker migrates (§6.1); the probe already did, so this is a
    // no-op check here.
    let pool = open_accounts(&root, Opener::Broker).await?;
    {
        let mut conn = pool.acquire().await?;
        let pinned = Provisioner::stored_uid_range(&mut conn).await?;
        if pinned.as_deref() != Some(uid_range.to_string().as_str()) {
            anyhow::bail!(
                "accounts.db is pinned to uid range {pinned:?}, not {uid_range} (the probe pins \
                 it; refusing to serve against a store it didn't check)"
            );
        }
    }

    // §7.4: honoured after the probe, only on an empty accounts table.
    if let Some(bootstrap) = bootstrap {
        let prov = Provisioner::new(root.clone(), uid_range, provisioning, Actor::Broker);
        if let Err(e) = prov.bootstrap_admin(&pool, &bootstrap).await {
            tracing::error!("IKENGA_BOOTSTRAP_ADMIN could not be applied: {e}");
        }
    }

    // The dummy hash is one argon2 run: off the runtime threads.
    let verifier = Arc::new(tokio::task::spawn_blocking(LoginVerifier::new).await?);
    let store = backend::open_session_store(&root).await?;
    {
        let store = store.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60 * 60));
            loop {
                tick.tick().await;
                if let Err(e) = store.delete_expired().await {
                    tracing::warn!("sessions.db: deleting expired sessions: {e}");
                }
            }
        });
    }

    let idle_timeout = config
        .idle_timeout_secs
        .map(Duration::from_secs)
        .unwrap_or(children::DEFAULT_IDLE_TIMEOUT);
    let launcher = Arc::new(T1Launcher {
        executor,
        exe: child_exe()?,
        root: root.clone(),
        pkgs_dir: config.pkgs_dir.clone(),
        idle_timeout,
    });
    let mut state = BrokerState::new(pool.clone(), verifier, launcher)?;

    // G-ACCESS (WP-74a): the access set migrates after `accounts` in the
    // same `_operator_migrations` table (R-1), and the audit chain is walked
    // (§6.4; a broken chain comes up degraded, refusing access changes).
    // Then the R-3 / R-4 / R-5 hooks get their access bodies.
    let access_store = crate::access::store::AccessStore::open_t1(pool.clone()).await?;
    let tiers = Arc::new(crate::access::devices::TierCache::default());
    let access_rt = Arc::new(crate::access::Runtime::for_broker(
        access_store.clone(),
        state.ws.clone(),
        Arc::new(AccountsDirectory { pool: pool.clone() }),
        access,
    ));
    state.hooks = BrokerHooks {
        authorizer: Arc::new(proxy::AccessAuthorizer {
            tiers: tiers.clone(),
        }),
        access: Arc::new(proxy::BrokerAccess {
            rt: access_rt.clone(),
            tiers: tiers.clone(),
        }),
        ws_frames: Arc::new(proxy::AccessFrames {
            tiers: tiers.clone(),
        }),
        still_valid: Arc::new(ws_registry::AccessStillValid { pool: pool.clone() }),
    };
    let mut extensions = BrokerExtensions::default();
    extensions
        .resolvers
        .push(Arc::new(backend::DeviceGrantResolver {
            store: access_store.clone(),
            tiers,
        }));
    extensions.public = PublicRoutes::default()
        .pairing(crate::access::http::pairing_routes(&access_rt))
        .invites(crate::access::http::invite_routes(&access_rt));
    extensions.device_cookies = Some(Arc::new(backend::DeviceCookieRotation {
        store: access_store,
        insecure_cookie,
    }));
    let state = Arc::new(state);
    let app = router(
        state.clone(),
        store,
        &config.static_dir,
        config.allowed_origins.clone(),
        insecure_cookie,
        extensions,
    );

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(4);

    // §2.2: the ≤2 s re-check, on its own connection (data_version is
    // per-connection and only sees *other* connections' commits).
    let recheck_conn = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(root.accounts_db())
            .read_only(true),
    )
    .await?;
    tokio::spawn(ws_registry::recheck_loop(
        state.ws.clone(),
        recheck_conn,
        state.hooks.still_valid.clone(),
        ws_registry::RECHECK_INTERVAL,
        shutdown_tx.subscribe(),
    ));

    // §7.3: a disabled principal's child is stopped. (OD-13 idle reap is
    // the child's own watcher, which counts PTYs; `running()` forgets the
    // children that exited.)
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                for id in state.children.running().await {
                    let disabled = async {
                        let mut conn = state.pool.acquire().await?;
                        accounts::by_id(&mut conn, id).await
                    }
                    .await
                    .map(|a| a.map_or(true, |a| a.is_disabled()));
                    if matches!(disabled, Ok(true)) && state.children.stop(id).await {
                        tracing::info!("stopped the child of disabled principal {id}");
                    }
                }
            }
        });
    }

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(
        "ikenga-server (t1 broker) listening on http://{bound} — sign in at /auth/login \
         (cookie {}secure)",
        if insecure_cookie { "IN" } else { "" }
    );

    // §4: the broker's discovery file is operator/daemon.json — never
    // <root>/daemon.json, which is a T0 marker the next boot would refuse.
    let meta_path = root.operator_dir().join("daemon.json");
    let meta = serde_json::json!({
        "pid": std::process::id(),
        "host": config.host,
        "port": bound.port(),
        "tier": "t1",
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    if let Err(e) = discovery::write_private(&meta_path, &meta) {
        tracing::warn!("could not write {}: {e}", meta_path.display());
    }

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(super::shutdown_signal(shutdown_tx.subscribe()))
    .await?;

    tracing::info!("t1 broker shutting down: stopping principal children");
    let _ = shutdown_tx.send(());
    state.children.stop_all().await;
    let _ = std::fs::remove_file(&meta_path);
    pool.close().await;
    Ok(())
}

#[cfg(test)]
mod tests;
