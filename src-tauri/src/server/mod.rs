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

/// What keeps a daemon active for its idle timeout: PTYs, open WebSockets,
/// recent requests (§5 row 13).
pub mod activity;
/// T1 request → principal: `PrincipalCtx`, the resolver list, sessions,
/// `/auth/*` (G-PRINCIPAL §2; WP-20 slice 3). Linux-only, like T1.
#[cfg(target_os = "linux")]
pub mod auth;
/// The T1 broker: per-principal children, the reverse proxy and the
/// open-socket registry (G-PRINCIPAL §3, topology B). Linux-only.
#[cfg(target_os = "linux")]
pub mod broker;
pub mod chat_ws;
pub mod discovery;
pub mod fs_ws;
pub mod health;
/// T1 operator: the operator root, `operator/accounts.db`, passwords and the
/// provisioning core (G-PRINCIPAL §4, §6, §7; WP-20). Linux-only, like T1.
#[cfg(target_os = "linux")]
pub mod operator;
mod pkg_cookie;
pub mod pkg_index;
pub mod pkg_static;
/// The principal-child side of topology B: the data-dir flock (I-3).
#[cfg(target_os = "linux")]
pub mod principal_child;
pub mod pty_ws;
mod reserved;
pub mod rpc;
mod rpc_claude;
mod rpc_files;
mod rpc_local;
mod rpc_shell;
pub mod shared;
pub mod static_files;
/// `ikenga-server supervise`: a minimal init that keeps detached runs alive
/// across server restarts where there is no systemd (containers). Linux-only.
pub mod supervisor;
/// Trusted proxy and client IP resolution (IKENGA_TRUSTED_PROXIES).
pub mod trusted_proxy;

/// Tauri-command ↔ daemon-RPC parity ratchet (WP-19). Test-only; reads
/// `lib.rs` and `rpc.rs` as text so it compiles in both feature sets.
/// `pub(crate)` so G-ACCESS A-1 (`access::rpc_requirements`) reuses its arm
/// lexer.
#[cfg(test)]
pub(crate) mod parity;
#[cfg(test)]
mod share_router_tests;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};

use crate::engines::EngineRegistry;
use crate::pty::PtyManager;
pub use health::health_handler;
pub use pkg_index::PkgIndex;
pub use pkg_static::PkgStaticService;
pub use static_files::SpaStaticService;

/// Request-body cap for `/api/rpc` (plans/file-editing Shape 5). axum's
/// default is 2 MB, which held browser text saves to roughly 400 KB once a
/// file's bytes were sent as a JSON number array. 16 MiB covers the editor's
/// 2 MiB edit limit even at worst-case JSON escaping. Not 64 MiB like the T1
/// broker's `MAX_RPC_BODY`: the RPC parses into `serde_json::Value`, and a
/// byte-array body inflates to ~32 B per element there. The SPA mirrors this
/// value as `RPC_BODY_LIMIT_BYTES` in `src/lib/tauri-cmd.ts`.
pub const RPC_BODY_LIMIT: usize = 16 * 1024 * 1024;

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
    /// Idle timeout in seconds before server automatically shuts down when no sessions are active.
    pub idle_timeout_secs: Option<u64>,
    /// Session-executor tier (`IKENGA_EXECUTOR_TIER` / `--executor-tier`,
    /// default `t0`). Probed first thing in `run_server`; a tier this build
    /// can't honour stops the server from starting (DEC-R9-1).
    pub executor_tier: crate::executor::ExecutorTier,
}

/// `IKENGA_BOOTSTRAP_ADMIN` + `…_PASSWORD` as captured by `main` (§7.4),
/// carried to the T1 broker, which applies it after its probe and only on an
/// empty accounts table. Debug never prints the password.
#[derive(Clone, PartialEq, Eq)]
pub struct BootstrapCredentials {
    pub username: String,
    pub password: zeroize::Zeroizing<String>,
}

impl std::fmt::Debug for BootstrapCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Serve options beyond [`ServerConfig`] (G-PRINCIPAL §2.2, §3, §7.2, §7.4,
/// §8, §9.3). Kept out of [`ServerConfig`] so the T0 config and its many
/// constructors don't change; [`run_server_with`] takes them beside it, and
/// every tier's boot reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct T1ServeOptions {
    /// `--uid-range START-END`; `None` is the default 20000-29999. Must agree
    /// with the range `accounts.db` is pinned to.
    pub uid_range: Option<String>,
    /// `--provisioning external`: never write `/etc` (reconcile refuses a
    /// missing entry instead of recreating it).
    pub provisioning_external: bool,
    /// `--principal-path`: the `PATH` principals' children get (§9.3).
    pub principal_path: Option<std::ffi::OsString>,
    /// `--insecure-cookie` (P-3, G-ACCESS R-9): drop `Secure` from the
    /// session and device cookies, for a plain-HTTP deploy. Read by every
    /// tier's boot. On T0 a tailnet peer gets a non-`Secure` device cookie
    /// without it (Round 19, DEC-R19-1, `devices::cookie_insecure`).
    pub insecure_cookie: bool,
    /// `--principal-child` (hidden): this process is a T1 principal child,
    /// launched by the broker as the principal's uid (§3). It never opens an
    /// access store (G-ACCESS R-11).
    pub principal_child: bool,
    /// `--expected-uid` (hidden, with `--principal-child`): the uid the
    /// child's probe requires it is running as.
    pub expected_uid: Option<u32>,
    /// The captured first-admin bootstrap (§7.4).
    pub bootstrap_admin: Option<BootstrapCredentials>,
    /// `--public-url` / `IKENGA_PUBLIC_URL` (G-ACCESS §3.3 rule 1): the base
    /// of pairing and invite links. Every tier.
    pub public_url: Option<String>,
    /// `--max-accounts N` (G-ACCESS P-27; T1). `None` = unlimited.
    pub max_accounts: Option<u32>,
    /// `--invite-ttl DAYS` (G-ACCESS P-15; T1). `None` = the 7-day default.
    pub invite_ttl_days: Option<u32>,
    /// `--member-invites-create-accounts` (G-ACCESS §4.4, N-11; T1). Off by
    /// default.
    pub member_invites_create_accounts: bool,
}

impl T1ServeOptions {
    /// The Part B flags as the access layer reads them (G-ACCESS §10.1).
    /// `--insecure-cookie` is WP-20's flag, reused tier-agnostically (R-9).
    pub fn access_options(&self) -> crate::access::AccessOptions {
        crate::access::AccessOptions {
            public_url: self.public_url.clone(),
            insecure_cookie: self.insecure_cookie,
            max_accounts: self.max_accounts,
            invite_ttl_days: self
                .invite_ttl_days
                .unwrap_or(crate::access::DEFAULT_INVITE_TTL_DAYS)
                .clamp(1, crate::access::MAX_INVITE_TTL_DAYS),
            member_invites_create_accounts: self.member_invites_create_accounts,
        }
    }
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
    /// Read-only index of the same `--pkgs-dir` walk, backing
    /// `pkg_kernel_status` and the skill-action listings. Every valid pkg is
    /// in it (not only the iframe-serveable ones `pkg_static` keeps); empty
    /// without `--pkgs-dir`. See `server::pkg_index`.
    pub pkg_index: Arc<PkgIndex>,
    /// The settings manager behind the `settings_*` arms, rooted at
    /// `--data-dir` (see `server::rpc_local::DaemonSettings`). `None` without
    /// a data dir or without a home; those arms then say which is missing.
    pub(crate) settings: Option<Arc<rpc_local::DaemonSettings>>,
    /// The home the per-user-file arms resolve against (the agent-ops job
    /// config, run tails and daemon lock): the router's home seam, i.e. the
    /// daemon PROCESS's home in production. `None` when it has none; those
    /// arms then answer the desktop's "home directory not found".
    /// Single-user seam (G-PRINCIPAL / WP-20), same as `settings` above.
    pub(crate) home: Option<PathBuf>,
    /// The allowlist the project filesystem arms check caller paths against
    /// (see `server::rpc_shell::PathGuard`): the process-global `fs_roots`
    /// set in production, a local one in tests.
    pub(crate) path_guard: rpc_shell::PathGuard,
    /// The actions / keybindings manager behind the `actions_*` and
    /// `keybindings_write` arms (see `server::rpc_files`): the desktop's, with
    /// no notifier, its trust record in `--data-dir`, the personal files under
    /// `home`, and project roots checked against `path_guard`. `None` without
    /// a data dir or a home; those arms then say which is missing.
    pub(crate) actions: Option<Arc<shared::actions::ActionsManager>>,
    /// The Ngwa store root the vault arms (`claude_store_*`,
    /// `claude_primitive_*`, `oba_*`; see `server::rpc_claude`) work in: the
    /// daemon PROCESS's `pkg::skill_actions::store_root()` in production, a
    /// temp dir in tests. `None` when the platform data dir cannot be
    /// resolved; those arms then answer the desktop's "cannot resolve store
    /// root". Single-user seam (G-PRINCIPAL / WP-20), same as `home`.
    pub(crate) store: Option<PathBuf>,
    /// The `secrets_*` arms' two layers (remote-access WP-21): in a T1
    /// principal child, the principal's own store under `<data>/secrets/`
    /// over the `IKENGA_SECRET_*` operator default; elsewhere the operator
    /// default alone, as before. Built once, here, because building it takes
    /// the broker's wrapping key out of the process environment before
    /// anything is spawned. See `crate::secrets_env`.
    pub(crate) secrets: Arc<crate::secrets_env::DaemonSecrets>,
    /// Channel for triggering graceful server shutdown.
    pub shutdown_tx: tokio::sync::broadcast::Sender<()>,
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

/// `Authorization: Bearer <token>` or `?token=<token>` equals the operator
/// bearer (constant time). A device token (`ikd1.…`) is never the operator
/// bearer: the daemon refuses an operator-chosen token with that prefix at
/// boot (G-ACCESS §2.4).
fn operator_bearer_ok(req: &Request, expected: &str) -> bool {
    // 1. Authorization: Bearer <TOKEN>
    if header_bearer_ok(req, expected) {
        return true;
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
                    return true;
                }
            }
        }
    }
    false
}

/// `Authorization: Bearer <token>` equals the operator bearer (constant time).
fn header_bearer_ok(req: &Request, expected: &str) -> bool {
    req.headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|header| ct_eq(header, expected))
}

fn forbidden(e: &crate::access::AccessError) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    )
        .into_response()
}

/// The T0 resolver (G-ACCESS §2.3, §2.4): the `Origin` gate, then — in
/// precedence order — a paired device's grant (`ikenga_device` cookie or
/// `Authorization: Bearer ikd1.…`), then the operator bearer. The result is
/// an [`crate::access::AccessCtx`] in the request extensions; non-RPC routes
/// are checked against their requirement here (§1.6), `/api/rpc` per
/// command in `rpc_handler`. A principal child takes caps only from
/// `X-Ikenga-Caps` on per-child-token requests (§1.7).
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Result<Response, Response> {
    use crate::access::{devices, DaemonAccess, DaemonMode, RequestMeta, StoreTier};

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
    let Some(expected) = state.config.auth_token.clone() else {
        warn!("Rejecting {} — server has no auth token", req.uri().path());
        return Err(unauthorized("Unauthorized: server has no auth token"));
    };

    let access = req
        .extensions()
        .get::<Arc<DaemonAccess>>()
        .cloned()
        .unwrap_or_else(DaemonAccess::unavailable);
    let meta = RequestMeta {
        remote_addr: trusted_proxy::client_addr(
            req.extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
            req.headers(),
        ),
        user_agent: req
            .headers()
            .get("user-agent")
            .and_then(|h| h.to_str().ok())
            .map(str::to_string),
        ..Default::default()
    }
    .with_host_from(req.headers());
    // P-3 as amended by Round 19 (DEC-R19-1): a T0 device cookie drops
    // `Secure` for a tailnet TCP peer (`ConnectInfo`, never
    // `X-Forwarded-For`) as well as under `--insecure-cookie`.
    let insecure = devices::cookie_insecure(
        StoreTier::T0,
        access.options.insecure_cookie,
        req.extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip()),
    );
    let mut set_cookie: Option<String> = None;
    let mut ctx = None;

    // DeviceGrant before OperatorBearer (§2.4). T0 only: a principal child
    // has no store, and the broker resolves devices for it.
    if access.mode == DaemonMode::T0 {
        if let Some(store) = access.store() {
            if let Some((raw, presented)) = devices::presented(req.headers()) {
                match devices::resolve(store, &access.seen, &raw, meta.remote_addr.as_deref()).await
                {
                    Ok(auth) => {
                        let upgrade = req.headers().contains_key("upgrade");
                        match devices::cookie_action(&auth, presented, upgrade) {
                            devices::CookieAction::Rotate => {
                                if let devices::DeviceAuth::Valid { row, .. } = &auth {
                                    match devices::rotate(store, &row.device_id).await {
                                        Ok(Some(tok)) => {
                                            set_cookie = Some(devices::set_cookie(&tok, insecure))
                                        }
                                        Ok(None) => {}
                                        Err(e) => warn!("device cookie rotation: {e:#}"),
                                    }
                                }
                            }
                            devices::CookieAction::Clear => {
                                set_cookie = Some(devices::clear_cookie(insecure))
                            }
                            devices::CookieAction::Keep => {}
                        }
                        match auth {
                            devices::DeviceAuth::Valid { row, .. } => {
                                ctx = Some(access.device_ctx(&row, meta.clone()).await);
                            }
                            // A dead bearer is a 401 (no fallback, §2.4); a
                            // dead cookie is cleared above but doesn't fail a
                            // request that also carries a valid operator
                            // bearer.
                            devices::DeviceAuth::Invalid(why) => {
                                if presented == devices::Presented::Bearer {
                                    warn!("device bearer refused on {} ({why})", req.uri().path());
                                    return Err(unauthorized(
                                        "Unauthorized: invalid device credential",
                                    ));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("device credential resolution failed: {e:#}");
                        return Err((
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(serde_json::json!({
                                "ok": false,
                                "error": "authentication is temporarily unavailable"
                            })),
                        )
                            .into_response());
                    }
                }
            }
        }
    }

    // App files under `/pkgs/*` (single-user server only): the browser loads
    // them itself and can't attach the bearer header, so a request that
    // authenticates with the header gets a `/pkgs`-scoped cookie, and a
    // `/pkgs` request may authenticate with that cookie (`pkg_cookie`).
    let now = pkg_cookie::now_secs();
    let mut pkgs_cookie: Option<String> = None;
    if ctx.is_none() && operator_bearer_ok(&req, &expected) {
        ctx = Some(match access.mode {
            DaemonMode::PrincipalChild => access.child_ctx(req.headers(), meta),
            DaemonMode::T0 => {
                if header_bearer_ok(&req, &expected) {
                    pkgs_cookie = Some(pkg_cookie::set_cookie(&expected, now, insecure));
                }
                access.operator_ctx(meta).await
            }
        });
    } else if ctx.is_none()
        && access.mode == DaemonMode::T0
        && pkg_cookie::is_pkgs_path(req.uri().path())
        && pkg_cookie::presented_ok(req.headers(), &expected, now)
    {
        ctx = Some(access.operator_ctx(meta).await);
    }

    let with_cookie = |mut res: Response, cookie: &Option<String>| {
        if let Some(v) = cookie
            .as_deref()
            .and_then(|v| HeaderValue::from_str(v).ok())
        {
            res.headers_mut().append("set-cookie", v);
        }
        res
    };

    let Some(ctx) = ctx else {
        warn!("Unauthorized request to {}", req.uri().path());
        return Err(with_cookie(
            unauthorized("Unauthorized: invalid or missing auth token"),
            &set_cookie,
        ));
    };

    // Non-RPC routes: their §1.6 requirement (RPCs are per command).
    if let Some(requirement) = crate::access::route_requirement(req.uri().path()) {
        if let Err(e) = crate::access::check(&ctx, requirement) {
            return Err(with_cookie(forbidden(&e), &set_cookie));
        }
    }
    req.extensions_mut().insert(ctx);
    let res = activity::track_request(next.run(req)).await;
    Ok(with_cookie(with_cookie(res, &set_cookie), &pkgs_cookie))
}

pub async fn shutdown_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    info!("POST /api/shutdown received: initiating graceful shutdown");
    let _ = state.shutdown_tx.send(());
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "status": "shutting_down"
        })),
    )
}

pub fn create_router(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
) -> Router {
    // Single-user seam (G-PRINCIPAL / WP-20): the personal settings file is
    // `<home>/.ikenga/settings.json` (and the agent-ops job files live under
    // `<home>/.agent-ops` / `<home>/.atelier`) for the daemon PROCESS's home
    // (`platform::home_dir` reads HOME / USERPROFILE), shared by every caller
    // holding the token — the same seam as `fs_home` and the Ngwa store.
    router_with_home(
        config,
        pty_manager,
        engine_registry,
        pa_db,
        shutdown_tx,
        crate::platform::home_dir(),
    )
}

/// [`create_router`] with the home made explicit, so tests never resolve (or
/// clear) the real user's `~/.ikenga/settings.json`, nor touch their
/// agent-ops files.
pub(crate) fn router_with_home(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
    home: Option<PathBuf>,
) -> Router {
    router_with(
        config,
        pty_manager,
        engine_registry,
        pa_db,
        shutdown_tx,
        home,
        rpc_shell::PathGuard::allowlist(),
    )
}

/// [`create_router`] with the daemon's access state (G-ACCESS §2.5), which
/// `run_server` opens. Attached through an `Extension` layer, not an
/// `AppState` field (G-ACCESS §10.1 X-2).
pub(crate) fn create_router_with_access(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
    access: Arc<crate::access::DaemonAccess>,
) -> Router {
    build_router(
        config,
        pty_manager,
        engine_registry,
        pa_db,
        shutdown_tx,
        crate::platform::home_dir(),
        rpc_shell::PathGuard::allowlist(),
        crate::pkg::skill_actions::store_root(),
        access,
    )
}

/// [`router_with_home`] with the path allowlist made explicit too, so tests
/// can check the project filesystem arms against a local root set instead of
/// installing the process-global one (a `OnceLock`).
pub(crate) fn router_with(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
    home: Option<PathBuf>,
    path_guard: rpc_shell::PathGuard,
) -> Router {
    build_router(
        config,
        pty_manager,
        engine_registry,
        pa_db,
        shutdown_tx,
        home,
        path_guard,
        // G-PRINCIPAL seam: the daemon process's own store.
        crate::pkg::skill_actions::store_root(),
        crate::access::DaemonAccess::unavailable(),
    )
}

/// [`router_with`] with the Ngwa store root made explicit too, so the vault
/// arms' tests work in a temp store instead of the real user's.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn router_with_store(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    home: Option<PathBuf>,
    path_guard: rpc_shell::PathGuard,
    store: Option<PathBuf>,
) -> Router {
    build_router(
        config,
        pty_manager,
        engine_registry,
        pa_db,
        None,
        home,
        path_guard,
        store,
        crate::access::DaemonAccess::unavailable(),
    )
}

#[allow(clippy::too_many_arguments)]
fn build_router(
    config: ServerConfig,
    pty_manager: Arc<PtyManager>,
    engine_registry: Arc<EngineRegistry>,
    pa_db: Option<Arc<crate::db::PaDb>>,
    shutdown_tx: Option<tokio::sync::broadcast::Sender<()>>,
    home: Option<PathBuf>,
    path_guard: rpc_shell::PathGuard,
    store: Option<PathBuf>,
    access: Arc<crate::access::DaemonAccess>,
) -> Router {
    // Whatever the allowlist covers, no caller path reaches this daemon's own
    // state: its `--data-dir` (fs_roots.json, ikenga.db, supabase.json,
    // daemon.json, …) or the per-user discovery file, both of which hold the
    // token or would widen the boundary. Attached here so every router —
    // tests included — refuses its own data dir. See `server::reserved`.
    let path_guard = path_guard.reserving(reserved::Reserved::new(
        config.data_dir.clone(),
        vec![discovery::user_temp_path()],
    ));
    let (default_tx, _) = tokio::sync::broadcast::channel(4);
    let shutdown_tx = shutdown_tx.unwrap_or(default_tx);
    let spa_service = SpaStaticService::new(&config.static_dir);
    // Walked here rather than in `run_server` so that every router — tests
    // included — gets the same view of `--pkgs-dir`. Walked ONCE: the static
    // server and the status index are built from the same list, so they can
    // never disagree about which directories are pkgs. Both log what they found.
    let pkgs = pkg_index::scan(config.pkgs_dir.as_deref());
    let pkg_static = PkgStaticService::from_packages(config.pkgs_dir.as_deref(), &pkgs);
    let pkg_index = Arc::new(PkgIndex::from_packages(&pkgs));
    let settings = match (&pa_db, &config.data_dir, &home) {
        (Some(db), Some(dir), Some(home)) => Some(Arc::new(rpc_local::DaemonSettings::new(
            db.clone(),
            dir.clone(),
            home.clone(),
        ))),
        _ => None,
    };
    let actions = match (&pa_db, &config.data_dir, &home) {
        (Some(db), Some(dir), Some(home)) => Some(Arc::new(rpc_files::daemon_actions(
            db.clone(),
            dir,
            home.clone(),
            path_guard.clone(),
        ))),
        _ => None,
    };
    let allowed_origins = config.allowed_origins.clone();
    let secrets = Arc::new(crate::secrets_env::DaemonSecrets::for_daemon(
        config.data_dir.as_deref(),
        config.executor_tier,
    ));
    let state = Arc::new(AppState {
        config,
        spa_service: spa_service.clone(),
        pty_manager,
        engine_registry,
        pa_db,
        pkg_static,
        pkg_index,
        settings,
        home,
        path_guard,
        actions,
        store,
        secrets,
        shutdown_tx,
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
        .route(
            "/api/rpc",
            post(rpc::rpc_handler).layer(axum::extract::DefaultBodyLimit::max(RPC_BODY_LIMIT)),
        )
        .route("/api/shutdown", post(shutdown_handler))
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

    // G-ACCESS §1.6: the public pairing endpoints (T0 only — a principal
    // child never pairs; the T1 broker serves its own), behind their own
    // Origin layer (A-31).
    let public_access = match access.mode {
        crate::access::DaemonMode::T0 => {
            crate::access::http::t0_public_router(allowed_origins.clone())
        }
        crate::access::DaemonMode::PrincipalChild => Router::new(),
    };

    Router::new()
        .route("/api/health", get(health::health_handler))
        .merge(protected_routes)
        .fallback(spa_fallback_handler)
        .layer(cors)
        .with_state(state)
        .merge(public_access)
        // The access state reaches `auth_middleware`, the RPC pre-hook and
        // the WS handlers through the request extensions (X-2).
        .layer(Extension(access))
}

async fn spa_fallback_handler(
    State(state): State<Arc<AppState>>,
    uri: Uri,
    headers: HeaderMap,
) -> impl IntoResponse {
    state.spa_service.handle_with(uri, &headers).await
}

pub async fn run_server(config: ServerConfig) -> anyhow::Result<()> {
    run_server_with(config, T1ServeOptions::default()).await
}

/// [`run_server`] with the [`T1ServeOptions`] (`ikenga-server` passes its
/// flags through here).
pub async fn run_server_with(config: ServerConfig, t1: T1ServeOptions) -> anyhow::Result<()> {
    // Executor tier first, before anything is created, bound or written: a
    // tier the host can't honour means this server must not start at all.
    // Refuse, don't fall back (ADR-023 / DEC-R9-1) — an operator who asked for
    // per-user isolation and quietly got a shared uid is worse off than one
    // whose server wouldn't boot.
    if config.executor_tier == crate::executor::ExecutorTier::T1 {
        return t1_boot(config, t1).await;
    }
    if t1.principal_child {
        anyhow::bail!("--principal-child is only valid with --executor-tier t1");
    }
    let executor = match crate::executor::install(config.executor_tier) {
        Ok(caps) => caps,
        Err(refusal) => {
            error!("executor tier {} refused: {refusal}", config.executor_tier);
            return Err(refusal.into());
        }
    };
    info!(
        "executor tier: {} (pty: {}, piped: {}, principal isolation: {})",
        executor.tier, executor.pty, executor.piped, executor.principal_isolation
    );
    serve_single_tenant(
        config,
        SingleTenant {
            access: t1.access_options(),
            ..SingleTenant::default()
        },
    )
    .await
}

/// What differs when the single-tenant daemon runs as a T1 principal child.
#[derive(Default)]
struct SingleTenant {
    /// The `<data>/.lock` flock, held for the process lifetime (I-3).
    #[cfg(target_os = "linux")]
    lock: Option<principal_child::DataDirLock>,
    /// A principal child: exits when the broker that launched it is gone.
    principal_child: bool,
    /// The Part B flags (G-ACCESS §10.1).
    access: crate::access::AccessOptions,
}

/// Resolves on SIGINT, SIGTERM or a message on `shutdown_rx`.
pub(crate) async fn shutdown_signal(mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            info!("Received SIGINT (Ctrl+C), shutting down daemon");
        }
        _ = terminate => {
            info!("Received SIGTERM, shutting down daemon");
        }
        _ = shutdown_rx.recv() => {
            info!("Received shutdown signal, shutting down daemon");
        }
    }
}

/// Today's single-tenant daemon: T0, and each T1 principal child.
async fn serve_single_tenant(mut config: ServerConfig, mode: SingleTenant) -> anyhow::Result<()> {
    health::init_uptime();

    // Fail closed: an operator who forgets `--auth-token` gets a generated
    // one, never an open shell. Printed below with the ready URL.
    let minted = config.auth_token.is_none();
    if minted {
        config.auth_token = Some(uuid::Uuid::new_v4().simple().to_string());
    }
    // G-ACCESS §2.4: the `ikd1.` prefix marks a device token, so an
    // operator-chosen bearer may never start with it.
    if config
        .auth_token
        .as_deref()
        .is_some_and(|t| t.starts_with(crate::access::devices::TOKEN_PREFIX))
    {
        anyhow::bail!(
            "IKENGA_AUTH_TOKEN must not start with `{}` (reserved for device tokens)",
            crate::access::devices::TOKEN_PREFIX
        );
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
        for probe in ["secrets-index.json", "pa.db"] {
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
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(4);
    // G-ACCESS §2.5: the T0 daemon opens, migrates and verifies
    // `<data-dir>/access.db`; a principal child never opens or creates one
    // (§1.7, A-32).
    let access = if mode.principal_child {
        crate::access::DaemonAccess::principal_child(mode.access.clone())
    } else {
        crate::access::DaemonAccess::boot_t0(
            config.data_dir.as_deref(),
            pa_db.clone(),
            mode.access.clone(),
        )
        .await
    };
    let router = create_router_with_access(
        config.clone(),
        pty_manager.clone(),
        engine_registry,
        pa_db,
        Some(shutdown_tx.clone()),
        access,
    );

    // Idle timeout watcher (G-02): shuts down daemon when no active sessions for idle_timeout_secs
    if let Some(idle_timeout_secs) = config.idle_timeout_secs {
        let pty_manager = pty_manager.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            let timeout = std::time::Duration::from_secs(idle_timeout_secs);
            let mut idle_since: Option<std::time::Instant> = None;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                // §5 row 13: PTYs, open WebSockets and recent requests —
                // plus (G-ACCESS §2.5, M-3) open pairing sessions and relay
                // asks, which hold an `access::KeepAlive` while pending
                // (live device sockets already count as open WebSockets).
                let holds = pty_manager.active_session_count() + crate::access::keepalive_count();
                if activity::is_active(holds, timeout) {
                    idle_since = None;
                } else {
                    let since = idle_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= timeout {
                        info!(
                            "Daemon idle for {}s (no PTY session, open WebSocket or request). \
                             Initiating auto-shutdown.",
                            idle_timeout_secs
                        );
                        let _ = shutdown_tx.send(());
                        break;
                    }
                }
            }
        });
    }

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

    // Bound before the discovery files are written, so they carry the real
    // port (a principal child binds port 0 and reports it this way, P-7).
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;

    // Write daemon discovery metadata files. They carry the bearer token, so
    // they are owner-only and the temp copy is per user (`discovery.rs`).
    let temp_meta_path = discovery::user_temp_path();
    let daemon_meta = serde_json::json!({
        "pid": std::process::id(),
        "host": config.host,
        "port": bound.port(),
        "token": token,
        "version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    if let Err(e) = discovery::write_private(&temp_meta_path, &daemon_meta) {
        warn!(
            "could not write discovery file {}: {e}",
            temp_meta_path.display()
        );
    }
    let data_dir_meta = config.data_dir.as_ref().map(|d| d.join("daemon.json"));
    if let Some(ref path) = data_dir_meta {
        if let Err(e) = discovery::write_private(path, &daemon_meta) {
            warn!("could not write discovery file {}: {e}", path.display());
        }
    }

    // A principal child whose broker died exits rather than hold the
    // principal's data-dir lock forever (the next broker could never start
    // a child for it).
    #[cfg(target_os = "linux")]
    if mode.principal_child {
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            principal_child::parent_gone().await;
            warn!("the T1 broker that launched this principal child is gone; shutting down");
            let _ = shutdown_tx.send(());
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = mode.principal_child;

    // ConnectInfo: device `last_seen_addr` and audit `remote_addr` (§3.9).
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_tx.subscribe()))
    .await?;

    info!("ikenga-server shutting down: cleaning up metadata and draining PTY sessions");
    let _ = std::fs::remove_file(&temp_meta_path);
    if let Some(ref path) = data_dir_meta {
        let _ = std::fs::remove_file(path);
    }
    pty_manager.drain_all();
    // Released last: nothing of this process touches ikenga.db any more.
    #[cfg(target_os = "linux")]
    drop(mode.lock);

    Ok(())
}

/// The T1 boot. A principal child verifies its own drop and serves as the
/// single-tenant daemon ([`principal_child_boot`]). Otherwise this is the
/// broker: the real §8 probe first, before anything binds; a failing probe is
/// the refusal (DEC-R9-1). A passing one installs the stamped executor and
/// serves the broker (`server::broker`).
#[cfg(target_os = "linux")]
async fn t1_boot(config: ServerConfig, t1: T1ServeOptions) -> anyhow::Result<()> {
    use crate::executor::Refusal;
    use operator::provision::{BootstrapAdmin, ProvisioningMode, UidRange};

    if t1.principal_child {
        return principal_child_boot(config, t1).await;
    }
    let refuse = |refusal: Refusal| -> anyhow::Error {
        error!("executor tier t1 refused: {refusal}");
        refusal.into()
    };
    let uid_range = match t1.uid_range.as_deref().map(str::parse::<UidRange>) {
        None => UidRange::DEFAULT,
        Some(Ok(range)) => range,
        Some(Err(e)) => {
            return Err(refuse(Refusal::ProbeFailed {
                check: "uid_range",
                detail: e.to_string(),
            }))
        }
    };
    let provisioning = if t1.provisioning_external {
        ProvisioningMode::External
    } else {
        ProvisioningMode::Auto
    };
    // The probe pins the uid range once its test drop passes (§8).
    let executor = operator::probe::boot(
        config.data_dir.clone(),
        uid_range,
        provisioning,
        t1.principal_path.clone(),
    )
    .await
    .map_err(refuse)?;
    let executor = Arc::new(executor);
    let caps = crate::executor::install_executor(Box::new(executor.clone())).map_err(refuse)?;
    info!(
        "executor tier: {} (pty: {}, piped: {}, principal isolation: {})",
        caps.tier, caps.pty, caps.piped, caps.principal_isolation
    );
    health::init_uptime();

    // The probe accepted --data-dir as the operator root (it resolved a
    // relative one against the cwd the same way).
    let data_dir = config
        .data_dir
        .clone()
        .ok_or_else(|| anyhow::anyhow!("--data-dir is required under t1"))?;
    let data_dir = if data_dir.is_absolute() {
        data_dir
    } else {
        std::env::current_dir()?.join(data_dir)
    };
    let root = operator::OperatorRoot::new(data_dir)?;
    if config.auth_token.is_some() {
        warn!(
            "IKENGA_AUTH_TOKEN / --auth-token is ignored under t1: the operator bearer grants \
             no principal surface (G-PRINCIPAL §2.4); principals sign in at /auth/login"
        );
    }
    let access_options = t1.access_options();
    let bootstrap = t1.bootstrap_admin.map(|b| BootstrapAdmin {
        username: b.username,
        password: b.password,
    });
    broker::serve(broker::BrokerBoot {
        config,
        executor,
        root,
        uid_range,
        provisioning,
        bootstrap,
        insecure_cookie: t1.insecure_cookie,
        access: access_options,
    })
    .await
}

/// A T1 principal child (§3 "pinned for the child"): verify the drop and
/// install that executor, take the data-dir flock **before** `PaDb` can open
/// `ikenga.db` (I-3), then serve as the single-tenant daemon on loopback
/// with the broker-issued per-child token.
#[cfg(target_os = "linux")]
async fn principal_child_boot(mut config: ServerConfig, t1: T1ServeOptions) -> anyhow::Result<()> {
    use crate::executor::Refusal;

    let refuse = |detail: String| -> anyhow::Error {
        let refusal = Refusal::ProbeFailed {
            check: "principal_child",
            detail,
        };
        error!("principal child refused: {refusal}");
        refusal.into()
    };
    let expected_uid = t1
        .expected_uid
        .ok_or_else(|| refuse("--principal-child needs --expected-uid".into()))?;
    let executor = crate::executor::t1_child::PrincipalChildExecutor::probe(expected_uid)
        .map_err(|r| refuse(r.to_string()))?;
    let caps =
        crate::executor::install_executor(Box::new(executor)).map_err(|r| refuse(r.to_string()))?;
    info!(
        "executor tier: {} principal child (pty: {}, piped: {}, principal isolation: {})",
        caps.tier, caps.pty, caps.piped, caps.principal_isolation
    );
    if config.auth_token.is_none() {
        // Never mint: only the broker's per-child token may reach us (P-7).
        return Err(refuse("no IKENGA_AUTH_TOKEN from the broker".into()));
    }
    let data_dir = config
        .data_dir
        .clone()
        .ok_or_else(|| refuse("--principal-child needs --data-dir".into()))?;
    let lock = principal_child::DataDirLock::acquire(&data_dir)
        .map_err(|e| refuse(format!("{}: {e}", data_dir.join(".lock").display())))?;
    // Loopback only, whatever was passed (P-7).
    config.host = "127.0.0.1".into();
    config.port = 0;
    serve_single_tenant(
        config,
        SingleTenant {
            lock: Some(lock),
            principal_child: true,
            access: t1.access_options(),
        },
    )
    .await
}

#[cfg(not(target_os = "linux"))]
async fn t1_boot(_config: ServerConfig, _t1: T1ServeOptions) -> anyhow::Result<()> {
    let refusal = crate::executor::Refusal::ProbeFailed {
        check: "os",
        detail: "executor tier t1 is Linux-only".into(),
    };
    error!("executor tier t1 refused: {refusal}");
    Err(refusal.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutorTier, Refusal};

    fn config(executor_tier: ExecutorTier) -> ServerConfig {
        ServerConfig {
            // Unparseable on purpose: if the executor probe ever stopped
            // running first, `run_server` would fail on the address instead —
            // with a non-`Refusal` error, so the test still catches it — and
            // it could never bind a port or write the daemon discovery file.
            host: "wp18.invalid".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
            data_dir: None,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier,
        }
    }

    /// DEC-R9-1: a tier this build can't honour stops the server from
    /// starting, with the typed refusal — it never falls back to T0.
    #[tokio::test]
    async fn refuses_to_start_on_an_unimplemented_executor_tier() {
        for tier in [ExecutorTier::T2, ExecutorTier::T3] {
            let err =
                tokio::time::timeout(std::time::Duration::from_secs(5), run_server(config(tier)))
                    .await
                    .expect("a refusal is immediate, not a running server")
                    .expect_err("an unimplemented tier must not start");
            assert_eq!(
                err.downcast_ref::<Refusal>(),
                Some(&Refusal::NotImplemented { tier }),
                "expected the typed executor refusal, got: {err:#}"
            );
        }
        assert_eq!(
            crate::executor::current().tier(),
            ExecutorTier::T0,
            "a refused tier must not be installed"
        );
    }

    /// DEC-R9-1 for T1: the real §8 probe runs first and refuses with its
    /// typed failure (unprivileged: identity; as root: the missing
    /// `--data-dir`) — never a fallback, never a bound port.
    #[tokio::test]
    async fn t1_runs_its_boot_probe_first_and_refuses_on_failure() {
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            run_server(config(ExecutorTier::T1)),
        )
        .await
        .expect("a refusal is immediate, not a running server")
        .expect_err("t1 without an operator root must not start");
        assert!(
            matches!(
                err.downcast_ref::<Refusal>(),
                Some(Refusal::ProbeFailed { .. })
            ),
            "expected the typed probe refusal, got: {err:#}"
        );
        assert_eq!(crate::executor::current().tier(), ExecutorTier::T0);

        // A bad --uid-range is refused before anything runs.
        let err = run_server_with(
            config(ExecutorTier::T1),
            T1ServeOptions {
                uid_range: Some("nope".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        #[cfg(target_os = "linux")]
        assert!(
            matches!(
                err.downcast_ref::<Refusal>(),
                Some(Refusal::ProbeFailed {
                    check: "uid_range",
                    ..
                })
            ),
            "{err:#}"
        );
        #[cfg(not(target_os = "linux"))]
        let _ = err;
    }

    #[tokio::test]
    async fn health_reports_the_executor_tier() {
        use axum::body::Body;
        use tower::ServiceExt;

        let router = create_router(
            config(ExecutorTier::T0),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
        );
        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["executor"]["tier"], "t0");
        assert_eq!(json["executor"]["principal_isolation"], false);
    }

    // ── WP-19: pkg-kernel read parity over /api/rpc ─────────────────────────

    /// A router over `pkgs_dir`, and a helper that POSTs one RPC with the
    /// bearer token and returns the decoded envelope.
    fn rpc_router(pkgs_dir: Option<PathBuf>) -> Router {
        let mut cfg = config(ExecutorTier::T0);
        cfg.pkgs_dir = pkgs_dir;
        create_router(
            cfg,
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
        )
    }

    async fn rpc(router: &Router, body: serde_json::Value) -> serde_json::Value {
        use axum::body::Body;
        use tower::ServiceExt;

        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Minimal valid pkg with one iframe route — the `pkg_static.rs` test
    /// shape — plus a `dist/` so it is also iframe-serveable.
    fn write_iframe_pkg(root: &std::path::Path, id: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","name":"T","version":"0.1.0","ikenga_api":"1",
                    "ui":{{"routes":[{{"path":"/x","kind":"iframe","source":"dist/index.html"}}],
                           "views":[{{"id":"v1","title":"V1","route":"/x"}}]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("dist").join("index.html"), "<h1>hi</h1>").unwrap();
        dir
    }

    #[tokio::test]
    async fn pkg_kernel_status_reports_the_indexed_pkgs_in_the_desktop_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = write_iframe_pkg(tmp.path(), "com.test.good");
        let router = rpc_router(Some(tmp.path().to_path_buf()));

        let res = rpc(&router, serde_json::json!({ "cmd": "pkg_kernel_status" })).await;
        assert_eq!(res["ok"], true, "{res}");
        let data = &res["data"];

        assert_eq!(
            data["api_version"],
            crate::pkg::manifest::IKENGA_API_VERSION
        );
        let row = &data["installed"][0];
        assert_eq!(row["id"], "com.test.good");
        assert_eq!(row["source"]["kind"], "local");
        assert_eq!(row["enabled"], true);
        assert_eq!(row["compatible"], true);
        assert!(row["project_id"].is_null(), "workspace scope");

        let entry = &data["registries"]["ui_routes"]["entries"][0];
        for key in ["pkg_id", "virtual_path", "path", "kind", "source"] {
            assert!(
                entry.get(key).is_some(),
                "ui_routes entry lacks `{key}`: {entry}"
            );
        }
        assert_eq!(entry["virtual_path"], "pkg://com.test.good/x");
        assert_eq!(entry["kind"], "iframe");
        assert_eq!(
            data["registries"].as_object().unwrap().len(),
            3,
            "daemon reports ui_routes, views, and activity_bar registries"
        );
        assert!(data["registries"].get("ui_routes").is_some());
        assert!(data["registries"].get("views").is_some());
        assert!(data["registries"].get("activity_bar").is_some());

        // Shape-equality with the Tauri side: `Kernel::status` goes through
        // the same `assemble_status`, so building it here from the same inputs
        // must reproduce the daemon's payload exactly.
        let pkg = crate::pkg::manifest::Package::load(&dir).unwrap();
        let ui = crate::pkg::registries::UiRoutesRegistry::new();
        let views = crate::pkg::registries::ViewsRegistry::new();
        let bar = crate::pkg::registries::ActivityBarRegistry::new();
        crate::pkg::Registry::register(&ui, &pkg).unwrap();
        crate::pkg::Registry::register(&views, &pkg).unwrap();
        crate::pkg::Registry::register(&bar, &pkg).unwrap();
        let expected = crate::pkg::assemble_status(
            vec![crate::pkg::InstalledSummary {
                id: "com.test.good".into(),
                version: "0.1.0".into(),
                ikenga_api: "1".into(),
                install_path: dir.display().to_string(),
                enabled: true,
                installed_at: row["installed_at"].as_i64().unwrap(),
                compatible: true,
                source: crate::pkg::InstallSource::Local {
                    path: dir.display().to_string(),
                },
                project_id: None,
            }],
            &[&ui, &views, &bar],
            crate::pkg::manifest::IKENGA_API_VERSION,
        );
        assert_eq!(data, &serde_json::to_value(expected).unwrap());
    }

    /// No `--pkgs-dir`: nothing installed, but `registries.ui_routes` must
    /// still be there — the FE route resolver reads `.entries` off it.
    #[tokio::test]
    async fn pkg_kernel_status_without_pkgs_dir_keeps_the_ui_routes_key() {
        let router = rpc_router(None);
        let res = rpc(&router, serde_json::json!({ "cmd": "pkg_kernel_status" })).await;
        assert_eq!(res["ok"], true, "{res}");
        assert_eq!(res["data"]["installed"], serde_json::json!([]));
        assert_eq!(
            res["data"]["registries"]["ui_routes"],
            serde_json::json!({ "count": 0, "entries": [] })
        );
    }

    /// Both spellings reach the arm (`tauri-cmd.ts` sends `pkgId`); an unknown
    /// pkg is `[]`, as on desktop — not an error.
    #[tokio::test]
    async fn list_skill_actions_accepts_both_spellings_and_unknown_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        write_iframe_pkg(tmp.path(), "com.test.good");
        let router = rpc_router(Some(tmp.path().to_path_buf()));

        for args in [
            serde_json::json!({ "pkgId": "com.test.good" }),
            serde_json::json!({ "pkg_id": "com.test.good" }),
            serde_json::json!({ "pkgId": "com.test.unknown" }),
        ] {
            let res = rpc(
                &router,
                serde_json::json!({ "cmd": "list_skill_actions", "args": args }),
            )
            .await;
            assert_eq!(res["ok"], true, "{args} → {res}");
            // The fixture requires no skills, so every case is empty — the
            // store-backed path is covered in `server::pkg_index` tests.
            assert_eq!(res["data"], serde_json::json!([]), "{args}");
        }

        let res = rpc(&router, serde_json::json!({ "cmd": "list_skill_actions" })).await;
        assert_eq!(res["ok"], false, "a missing pkgId is a caller error");

        let res = rpc(
            &router,
            serde_json::json!({ "cmd": "list_all_skill_actions" }),
        )
        .await;
        assert_eq!(res["ok"], true, "{res}");
        assert!(res["data"].is_array());
    }

    async fn get_pkgs(router: &Router, uri: &str, headers: &[(&str, &str)]) -> Response {
        use axum::body::Body;
        use tower::ServiceExt;

        let mut req = axum::http::Request::builder().method("GET").uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The `ikenga_pkgs` Set-Cookie a response carries, if any.
    fn pkgs_set_cookie(res: &Response) -> Option<String> {
        res.headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("ikenga_pkgs="))
            .map(str::to_string)
    }

    /// `name=value` from a Set-Cookie line.
    fn cookie_pair(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_string()
    }

    #[tokio::test]
    async fn pkgs_routes_take_the_scoped_cookie_minted_by_a_bearer_request() {
        let tmp = tempfile::tempdir().unwrap();
        write_iframe_pkg(tmp.path(), "com.test.good");
        let router = rpc_router(Some(tmp.path().to_path_buf()));
        let file = "/pkgs/com.test.good/index.html";

        // No credential: refused.
        let res = get_pkgs(&router, file, &[]).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // The bearer header works, and mints the `/pkgs` cookie.
        let res = get_pkgs(&router, file, &[("authorization", "Bearer tok")]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let set = pkgs_set_cookie(&res).expect("a bearer request mints the cookie");
        assert!(set.contains("; HttpOnly"), "{set}");
        assert!(set.contains("; SameSite=Strict"), "{set}");
        assert!(set.contains("; Path=/pkgs;"), "{set}");
        assert!(!set.contains("tok;"), "the cookie must not carry the bearer: {set}");
        let pair = cookie_pair(&set);
        assert!(!pair.contains("=tok"), "{pair}");

        // That cookie alone now loads app files (an ES module import, say).
        let res = get_pkgs(&router, file, &[("cookie", pair.as_str())]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let res = get_pkgs(&router, "/pkgs/com.test.good/", &[("cookie", pair.as_str())]).await;
        assert_eq!(res.status(), StatusCode::OK);

        // ...but nothing outside `/pkgs`.
        {
            use axum::body::Body;
            use tower::ServiceExt;
            let res = router
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri("/api/rpc")
                        .header("cookie", pair.as_str())
                        .header("content-type", "application/json")
                        .body(Body::from(r#"{"cmd":"pkg_kernel_status"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        }

        // A wrong, forged or raw-bearer cookie is refused.
        for bad in [
            "ikenga_pkgs=nope",
            "ikenga_pkgs=tok",
            "ikenga_session=tok",
            "ikenga_pkgs=99999999999.00",
        ] {
            let res = get_pkgs(&router, file, &[("cookie", bad)]).await;
            assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{bad}");
        }

        // A cookie minted under another bearer is refused.
        let other = format!(
            "{}={}",
            pkg_cookie::COOKIE,
            pkg_cookie::mint("another-token", pkg_cookie::now_secs())
        );
        let res = get_pkgs(&router, file, &[("cookie", other.as_str())]).await;
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // A token in the query string mints nothing.
        let res = get_pkgs(&router, &format!("{file}?token=tok"), &[]).await;
        assert!(pkgs_set_cookie(&res).is_none());
    }

    #[tokio::test]
    async fn browser_app_rpcs_answer_on_the_server() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = write_iframe_pkg(tmp.path(), "com.test.good");
        let router = rpc_router(Some(tmp.path().to_path_buf()));

        // pkg_activity_bar_set_badge: set, visible in pkg_kernel_status, clear.
        let res = rpc(
            &router,
            serde_json::json!({
                "cmd": "pkg_activity_bar_set_badge",
                "args": { "pkgId": "com.test.good", "badge": { "dot": true, "count": 3 } }
            }),
        )
        .await;
        assert_eq!(res["ok"], true, "{res}");
        let status = rpc(&router, serde_json::json!({ "cmd": "pkg_kernel_status" })).await;
        assert!(
            status["data"]["registries"]["activity_bar"]
                .to_string()
                .contains("\"count\":3"),
            "{status}"
        );
        let res = rpc(
            &router,
            serde_json::json!({
                "cmd": "pkg_activity_bar_set_badge",
                "args": { "pkgId": "com.test.good", "badge": null }
            }),
        )
        .await;
        assert_eq!(res["ok"], true, "{res}");

        // Unknown pkg, missing id and a malformed badge are errors.
        for args in [
            serde_json::json!({ "pkgId": "com.test.unknown", "badge": null }),
            serde_json::json!({ "badge": null }),
            serde_json::json!({ "pkgId": "com.test.good", "badge": { "count": "x" } }),
        ] {
            let res = rpc(
                &router,
                serde_json::json!({ "cmd": "pkg_activity_bar_set_badge", "args": args }),
            )
            .await;
            assert_eq!(res["ok"], false, "{res}");
        }

        // The server grants no elevated trust, to any pkg.
        for id in ["com.test.good", "com.test.unknown"] {
            let res = rpc(
                &router,
                serde_json::json!({
                    "cmd": "pkg_is_trusted_for_elevated",
                    "args": { "pkgId": id }
                }),
            )
            .await;
            assert_eq!(res["ok"], true, "{res}");
            assert_eq!(res["data"], false, "{res}");
        }

        // Nothing is ever parked for review.
        let res = rpc(&router, serde_json::json!({ "cmd": "pkg_trust_list_pending" })).await;
        assert_eq!(res["ok"], true, "{res}");
        assert_eq!(res["data"], serde_json::json!([]));

        // Approving stays desktop-only.
        let res = rpc(
            &router,
            serde_json::json!({ "cmd": "pkg_trust_approve", "args": { "pkgId": "com.test.good" } }),
        )
        .await;
        assert_eq!(res["ok"], false, "{res}");

        // pkg_preview_manifest reads an installed pkg's manifest.
        let res = rpc(
            &router,
            serde_json::json!({
                "cmd": "pkg_preview_manifest",
                "args": { "installPath": dir.to_str().unwrap() }
            }),
        )
        .await;
        assert_eq!(res["ok"], true, "{res}");
        assert_eq!(res["data"]["id"], "com.test.good");
    }
}
