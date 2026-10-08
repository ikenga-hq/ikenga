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
//!                          ├─ /api/server/update{,/apply}   (answered here, admins only; never proxied)
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
pub mod fs_roots_admin;
pub mod proxy;
pub mod ws_registry;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
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
    AccessHandler, AccessNotFound, AllowAll, Narrower, NoNarrowing, PassFrames, RpcAuthorizer,
    WsFrameHook,
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

/// The R-3 / R-5 hook points, with their WP-20 defaults. WP-74a swaps them
/// in [`serve`] (`access::t1::install`).
#[derive(Clone)]
pub struct BrokerHooks {
    pub authorizer: Arc<dyn RpcAuthorizer>,
    pub access: Arc<dyn AccessHandler>,
    pub ws_frames: Arc<dyn WsFrameHook>,
    pub still_valid: Arc<dyn StillValid>,
    /// G-ACCESS §1.4 / §4.5.2–§4.5.3: each proxied request's / socket's
    /// narrowing (`X-Ikenga-Caps`, share selection and headers, target
    /// child), decided once.
    pub narrower: Arc<dyn Narrower>,
}

impl BrokerHooks {
    /// Allow every RPC, no `access_*` arms, pass every frame, close a socket
    /// once its account is disabled or its `session_epoch` moved, and set no
    /// narrowing headers.
    pub fn defaults(pool: SqlitePool) -> Self {
        Self {
            authorizer: Arc::new(AllowAll),
            access: Arc::new(AccessNotFound),
            ws_frames: Arc::new(PassFrames),
            still_valid: Arc::new(AccountEpochs { pool }),
            narrower: Arc::new(NoNarrowing),
        }
    }

    /// WP-74a's fills (G-ACCESS R-3, R-5).
    pub fn access(installed: &crate::access::t1::Installed) -> Self {
        Self {
            authorizer: installed.authorizer.clone(),
            access: installed.access.clone(),
            ws_frames: installed.ws_frames.clone(),
            still_valid: installed.still_valid.clone(),
            narrower: installed.narrower.clone(),
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
    /// In-app updates (WP-P9; `server::update`). `None` in unit tests that
    /// don't set it, which answers `unsupported`.
    pub update: Option<Arc<crate::server::update::UpdateCtl>>,
    /// The access layer, for the update routes' admin-strength check and
    /// audit row. `None` in unit tests: then only a password session counts.
    pub access_t1: Option<Arc<crate::access::t1::T1Access>>,
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
            update: None,
            access_t1: None,
        })
    }
}

/// A layer applied around the protected routes, outside credential
/// resolution (WP-74a: the device-cookie resolve / rotate / clear layer).
pub type ProtectedLayer = Box<dyn FnOnce(Router) -> Router + Send>;

/// What WP-74 / WP-76 add without restructuring: more credential resolvers
/// (appended after the session cookie, R-4), the public pairing/invite
/// routes (Round 16 §14.1), and a layer around the protected routes.
pub struct BrokerExtensions {
    pub resolvers: Resolvers,
    pub public: PublicRoutes,
    pub protected_layer: Option<ProtectedLayer>,
}

impl Default for BrokerExtensions {
    fn default() -> Self {
        Self {
            resolvers: Resolvers::new(Arc::new(SessionCookieResolver)),
            public: PublicRoutes::default(),
            protected_layer: None,
        }
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

    let protected = Router::new()
        .route("/api/rpc", post(proxy::rpc_proxy))
        .route("/ws/*rest", get(proxy::ws_proxy))
        .route("/pkgs/*rest", get(proxy::pkgs_proxy))
        .route("/auth/logout", post(auth_mod::routes::logout))
        .route("/auth/me", get(auth_mod::routes::me))
        .route("/auth/password", post(auth_mod::routes::change_password))
        // In-app updates (WP-P9): the broker answers these itself, after a
        // fresh admin check; they never reach a principal's child.
        .route(
            "/api/server/update",
            get(crate::server::update::broker_status),
        )
        .route(
            "/api/server/update/apply",
            post(crate::server::update::broker_apply),
        )
        // `/api/shutdown` and anything else under /api: no such route here,
        // and an unauthenticated caller can't even learn that.
        .route("/api/*rest", any(api_not_found))
        .route_layer(middleware::from_fn_with_state(
            extensions.resolvers,
            auth_mod::require_principal,
        ))
        .with_state(state.clone());
    let protected = match extensions.protected_layer {
        Some(layer) => layer(protected),
        None => protected,
    };

    let public = Router::new()
        .route("/api/health", get(health::health_handler))
        .route("/auth/login", post(auth_mod::routes::login))
        .with_state(state.clone())
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

    let viewer_routes = Router::new()
        .route("/__viewer/*rest", any(proxy::viewer_proxy))
        .with_state(state.clone());

    Router::new()
        .merge(public)
        .merge(viewer_routes)
        .merge(protected)
        // With the request headers, so the broker gzips like the T0 daemon
        // and refuses a non-`/sw.js` service-worker install the same way.
        .fallback(move |uri: Uri, headers: HeaderMap| {
            let spa = spa.clone();
            async move { spa.handle_with(uri, &headers).await }
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
    /// `--account-secrets-dir`: where the per-account secrets files live
    /// (`None` = `/etc/ikenga/secrets`). Root's to set; see
    /// [`children::T1Launcher::account_secrets_dir`].
    pub account_secrets_dir: Option<PathBuf>,
    /// The Part B flags (G-ACCESS §10.1): `--public-url`, `--max-accounts`,
    /// `--invite-ttl`, `--member-invites-create-accounts`.
    pub access: crate::access::AccessOptions,
    /// Web Push flags (plans/pwa S2): the broker owns the hub under T1.
    pub push: crate::server::push::PushOptions,
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
        account_secrets_dir,
        access: access_options,
        push: push_options,
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

    // G-ACCESS §8.1 / R-1: the access set runs after `accounts`, in the same
    // `_operator_migrations` table; then the chain is verified (§6.4 "at
    // every start"). Only the broker migrates it.
    let access_store = crate::access::AccessStore::attach_t1(pool.clone()).await?;
    let max_accounts = access_options.max_accounts;
    // plans/pwa S2: the broker owns push under T1 — the VAPID key in the
    // root-only `operator/push/`, subscriptions in `accounts.db`. Children
    // never see either; they queue events the pump below drains.
    let push_hub = crate::server::push::hub::boot(
        access_store.clone(),
        &crate::server::push::vapid::path_in(&root.operator_dir()),
        &push_options,
        access_options.public_url.as_deref(),
    );
    let access_t1 = crate::access::t1::T1Access::new(access_store, pool.clone(), access_options);
    // G-ACCESS P-27 / §4.4: `--max-accounts` caps every creation path — the
    // root CLI and the env bootstrap included, which never see this flag —
    // so the serving broker pins it (or its absence) for them (WP76-R6).
    Provisioner::pin_max_accounts(&pool, max_accounts).await?;

    // Resume any interrupted secrets KEK rotation before serving.
    if let Err(e) = crate::server::operator::rotate_kek::resume_if_interrupted(&root, &pool).await {
        tracing::error!("secrets KEK rotation auto-resume failed at boot: {e}");
        return Err(e);
    }

    // §7.4: honoured after the probe, only on an empty accounts table.
    if let Some(bootstrap) = bootstrap {
        let prov = Provisioner::new(root.clone(), uid_range, provisioning, Actor::Broker)
            .with_max_accounts(max_accounts);
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
        account_secrets_dir: crate::executor::t1_account_env::dir_or_default(
            account_secrets_dir.as_deref(),
        ),
    });
    let mut broker_state = BrokerState::new(pool.clone(), verifier, launcher)?;
    // G-ACCESS R-3 / R-4 / R-5: the device-grant resolver, authorize_rpc, the
    // broker-served `access_*` arms, the WS frame hook, the narrowing hook and
    // the two-epoch socket check.
    let installed = crate::access::t1::install(&access_t1, broker_state.ws.clone());
    broker_state.hooks = BrokerHooks::access(&installed);
    // WP-P9: root's update files, and the request path under operator/
    // (root 0700: no principal can see or write it).
    broker_state.update = Some(Arc::new(crate::server::update::UpdateCtl::new(
        crate::server::update::state_dir(),
        root.operator_dir()
            .join(crate::server::update::REQUEST_FILE),
    )));
    broker_state.access_t1 = Some(access_t1.clone());
    let state = Arc::new(broker_state);
    if let Some(hub) = push_hub {
        crate::server::push::install_hub(hub);
        crate::server::push::pump::spawn(state.children.clone());
    }
    // Daemon asks, gap 2: a child's held-hook decisions are chained here, by
    // the store's one writer (`access::audit::child`).
    crate::access::audit::child::spawn(state.children.clone(), access_t1.store.clone());
    // G-ACCESS §4.5 / §7 (WP-76): the broker's handle on owner children
    // (share validation, the `invite` notification, member socket closes),
    // the membership expiry sweeper, and the invite-accept host (the §7.2
    // provisioning core, R-8).
    let invite_host = crate::access::share::install_t1(
        &access_t1,
        Arc::new(proxy::ShareChildCalls::new(&state)),
        Provisioner::new(root.clone(), uid_range, provisioning, Actor::Broker)
            .with_max_accounts(max_accounts),
    );
    let mut extensions = BrokerExtensions::default();
    extensions.resolvers.push(installed.resolver.clone());
    extensions.public = PublicRoutes::default()
        .pairing(crate::access::http::pairing_routes_for(
            access_t1.pairing_host(),
            config.allowed_origins.clone(),
        ))
        .invites(crate::access::http::invite_routes_for(
            invite_host,
            config.allowed_origins.clone(),
        ));
    {
        let t1 = access_t1.clone();
        extensions.protected_layer = Some(Box::new(move |r: Router| {
            r.layer(middleware::from_fn_with_state(
                t1,
                crate::access::t1::device_cookie_middleware,
            ))
        }));
    }
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
