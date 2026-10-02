//! The G-ACCESS fills for WP-20's T1 broker hooks (§10.5):
//!
//! * **R-4** — [`DeviceGrantResolver`], pushed after the session cookie in
//!   the broker's ordered `Resolvers` (§2.4 precedence: Session, then
//!   DeviceGrant; the operator bearer never resolves under T1);
//! * **R-3** — [`AccessNarrower`] (once per request / WS handshake: the
//!   effective caps with share selection and routing — §1.4 — as
//!   `X-Ikenga-Caps` + `X-Ikenga-Share-*` on the proxied request, §4.5.3,
//!   and the target child), [`AccessAuthorizer`] (`authorize_rpc`: class +
//!   caps before proxying), [`BrokerAccess`] (the broker-served `access_*`
//!   arms, as root) and [`DeviceFrames`] (the client → child WS frame hook,
//!   on the handshake's snapshot);
//! * **R-5** — [`DeviceEpochs`]: the pluggable "still valid?" check that
//!   closes a device socket when its account's `session_epoch` **or** its
//!   `grant_epoch` moves (I-8 for device sockets, §3.10);
//! * **R-11** — [`RevokeDeviceGrants`]: a forced logout revokes every grant
//!   of the principal inside its transaction (CLI and broker).
//!
//! Plus [`device_cookie_middleware`]: resolves an `ikenga_device` cookie
//! once per request for the resolver, rotates it when due (§3.9, P-32) and
//! clears a dead one (§2.4). Wired by `server::broker::serve` ([`install`]).
//!
//! Everything here runs in the broker (root) and opens only
//! `operator/accounts.db` (G-PRINCIPAL §5 row 9).

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::audit::AuditVia;
use super::caps::{self, CapSet, RoleContext, Tier};
use super::ctx::{AccessCtx, RequestMeta, Via};
use super::devices::{self, CookieAction, DeviceAuth, SeenGate};
use super::rpc::{self as access_rpc, Env, PrincipalInfo, SocketControl};
use super::sockets::Close;
use super::store::{AccessStore, StoreTier};
use super::{AccessError, AccessOptions, Code};
use crate::executor::{Principal, PrincipalId};
use crate::server::auth::{
    BoxFuture, Credential, CredentialResolver, Epochs, PrincipalCtx, Resolution,
};
use crate::server::broker::proxy::{
    AccessHandler, ClientFrame, Decision, FrameDecision, Narrower, Narrowing, Refusal,
    RpcAuthorizer, WsFrameHook, CAPS_HEADER,
};
use crate::server::broker::ws_registry::{CloseReason, StillValid, WsKey, WsRegistry};
use crate::server::operator::accounts::{self, HookFuture, SessionsRevokedHook};

/// The broker's access decision for one request or WebSocket handshake
/// (§1.4: computed **once**, never re-read from shared state). The
/// [`AccessNarrower`] stores it as the request's / socket's
/// [`Narrowing::snapshot`].
#[derive(Debug, Clone)]
pub struct BrokerCtx {
    pub access: AccessCtx,
    /// On a share: the Owner, whose child the request is routed into.
    pub owner: Option<Principal>,
}

/// The broker's shared access state.
pub struct T1Access {
    pub store: AccessStore,
    pub pool: SqlitePool,
    pub seen: SeenGate,
    pub options: AccessOptions,
    /// The broker's pairing sessions (§3.1, WP-74b): in memory only.
    pub pairing: Arc<super::pairing::Registry>,
}

impl T1Access {
    pub fn new(store: AccessStore, pool: SqlitePool, options: AccessOptions) -> Arc<Self> {
        let pairing = super::pairing::Registry::new();
        super::pairing::spawn_sweeper(&pairing, store.clone());
        Arc::new(Self {
            store,
            pool,
            seen: SeenGate::default(),
            options,
            pairing,
        })
    }

    /// The public `/access/pair/*` endpoints' state (§3.1, WP-74b).
    pub fn pairing_host(&self) -> super::http::PairingHost {
        super::http::PairingHost {
            registry: self.pairing.clone(),
            store: self.store.clone(),
            insecure_cookie: self.options.insecure_cookie,
        }
    }

    /// The tier a resolved credential carries (§1.3): a password session is
    /// `full`; a device its row's tier **as resolved for this request** —
    /// the row [`device_cookie_middleware`] read (the same read the
    /// resolver used). Without it the row is re-read and must still be
    /// unrevoked and at the resolution's `grant_epoch`, else `view` (fail
    /// closed): a tier change bumps the epoch, so a stale resolution never
    /// carries a stale (higher) tier.
    async fn tier_of(&self, ctx: &PrincipalCtx, parts: &Parts) -> Tier {
        let device_id = match &ctx.via {
            Credential::Session { .. } => return Tier::Full,
            Credential::DeviceGrant { device_id } => device_id,
            // Never produced under T1 (§2.4); grant nothing beyond view.
            Credential::OperatorBearer => return Tier::View,
        };
        if let Some(ResolvedDevice {
            auth: DeviceAuth::Valid { row, .. },
        }) = parts.extensions.get::<ResolvedDevice>()
        {
            if &row.device_id == device_id {
                return row.tier;
            }
        }
        let epoch = parts.extensions.get::<Epochs>().and_then(|e| e.grant_epoch);
        let row = match self.pool.acquire().await {
            Ok(mut conn) => devices::get(&mut conn, device_id).await.ok().flatten(),
            Err(_) => None,
        };
        match row {
            Some(r) if !r.is_revoked() && epoch.map_or(true, |e| e == r.grant_epoch) => r.tier,
            _ => Tier::View,
        }
    }

    /// `PrincipalCtx` → [`BrokerCtx`] (§2.3, built right after resolution):
    /// the tier, the share selection (§4.5.2, `share::broker_select`,
    /// WP-76), routing (§5.1, `routing::routing_ok`, WP-75) and the
    /// effective caps (§1.4). Everything W4 fills is reached from here, so
    /// W4 edits only `routing.rs` / `share.rs`.
    pub async fn access_ctx(
        &self,
        ctx: &PrincipalCtx,
        parts: &Parts,
    ) -> Result<BrokerCtx, AccessError> {
        let via = Via::from(&ctx.via);
        let tier = self.tier_of(ctx, parts).await;
        let device_id = ctx.via.device_id().map(str::to_string);
        let selected = super::share::broker_select(self, ctx, parts).await?;
        let routing_ok =
            super::routing::routing_ok(Some(&self.store), &ctx.principal.id, device_id.as_deref())
                .await;
        let (context, ceiling) = match &selected {
            Some(s) => (s.context, s.ceiling),
            None => (RoleContext::OwnWorkspace, CapSet::ALL),
        };
        let caps = if matches!(via, Via::Operator) {
            CapSet::EMPTY
        } else {
            caps::effective(context, tier, ceiling, routing_ok)
        };
        let (share, owner) = match selected {
            Some(s) => (Some(s.share), Some(s.owner)),
            None => (None, None),
        };
        let meta = RequestMeta {
            remote_addr: remote_addr(parts),
            user_agent: parts
                .headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            ..Default::default()
        }
        .with_host_from(&parts.headers);
        Ok(BrokerCtx {
            access: AccessCtx {
                principal_id: ctx.principal.id,
                admin_strength: AccessCtx::admin_strength_of(&via, tier)
                    && !matches!(via, Via::Operator),
                device_id,
                via,
                tier,
                share,
                share_headers: false,
                caps,
                meta,
            },
            owner,
        })
    }

    /// This request's [`BrokerCtx`]: the snapshot the narrower computed for
    /// it (`rpc_proxy` puts the [`Narrowing`] in the request extensions), or
    /// a fresh computation.
    pub async fn ctx_for(
        &self,
        ctx: &PrincipalCtx,
        parts: &Parts,
    ) -> Result<BrokerCtx, AccessError> {
        if let Some(b) = parts
            .extensions
            .get::<Narrowing>()
            .and_then(|n| n.snapshot::<BrokerCtx>())
        {
            return Ok(b.clone());
        }
        self.access_ctx(ctx, parts).await
    }
}

fn refusal(e: &AccessError) -> Refusal {
    Refusal {
        status: e.code.status(),
        code: e.code.as_str(),
        message: e.to_string(),
    }
}

fn remote_addr(parts: &Parts) -> Option<String> {
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
}

/// What [`device_cookie_middleware`] found, handed to the resolver through
/// the request extensions so the token is resolved once.
#[derive(Clone)]
struct ResolvedDevice {
    auth: DeviceAuth,
}

/// R-4: `DeviceGrant` resolution (§2.3 T1 column). The device row's
/// `principal_id`; the account must exist and not be disabled; the tier is
/// the row's. A present but invalid grant stops the walk (`Rejected`).
pub struct DeviceGrantResolver(pub Arc<T1Access>);

impl CredentialResolver for DeviceGrantResolver {
    fn name(&self) -> &'static str {
        "device_grant"
    }

    fn resolve<'a>(&'a self, parts: &'a Parts) -> BoxFuture<'a, anyhow::Result<Resolution>> {
        Box::pin(async move {
            let auth = match parts.extensions.get::<ResolvedDevice>() {
                Some(r) => r.auth.clone(),
                None => {
                    let Some((raw, _)) = devices::presented(&parts.headers) else {
                        return Ok(Resolution::NotPresent);
                    };
                    devices::resolve(
                        &self.0.store,
                        &self.0.seen,
                        &raw,
                        remote_addr(parts).as_deref(),
                    )
                    .await?
                }
            };
            let row = match auth {
                DeviceAuth::Valid { row, .. } => row,
                DeviceAuth::Invalid(_) => return Ok(Resolution::Rejected("device")),
            };
            let Ok(principal_id) = row.principal_id.parse::<PrincipalId>() else {
                return Ok(Resolution::Rejected("device"));
            };
            let mut conn = self.0.pool.acquire().await?;
            let Some(account) = accounts::by_id(&mut conn, principal_id).await? else {
                return Ok(Resolution::Rejected("device"));
            };
            if account.is_disabled() {
                return Ok(Resolution::Rejected("device"));
            }
            Ok(Resolution::Resolved {
                ctx: PrincipalCtx {
                    principal: account.principal(),
                    via: Credential::DeviceGrant {
                        device_id: row.device_id.clone(),
                    },
                },
                epochs: Epochs {
                    session_epoch: account.session_epoch,
                    grant_epoch: Some(row.grant_epoch),
                },
            })
        })
    }
}

/// The protected-route layer (outside `require_principal`): resolve a
/// device credential once, rotate a due cookie (the request proceeds on the
/// old secret, valid for the grace), and clear a dead cookie on the way out.
pub async fn device_cookie_middleware(
    State(t1): State<Arc<T1Access>>,
    mut req: Request,
    next: Next,
) -> Response {
    let mut set_cookie = None;
    let mut clear = false;
    // §2.4: the same pick and the same cookie decision as T0's
    // `auth_middleware` (`devices::presented` / `devices::cookie_action`).
    if let Some((raw, presented)) = devices::presented(req.headers()) {
        let addr = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip().to_string());
        match devices::resolve(&t1.store, &t1.seen, &raw, addr.as_deref()).await {
            Ok(auth) => {
                let upgrade = req.headers().contains_key(header::UPGRADE);
                match devices::cookie_action(&auth, presented, upgrade) {
                    CookieAction::Rotate => {
                        if let DeviceAuth::Valid { row, .. } = &auth {
                            match devices::rotate(&t1.store, &row.device_id).await {
                                Ok(tok) => set_cookie = tok,
                                Err(e) => tracing::warn!("device cookie rotation: {e:#}"),
                            }
                        }
                    }
                    CookieAction::Clear => clear = true,
                    CookieAction::Keep => {}
                }
                req.extensions_mut().insert(ResolvedDevice { auth });
            }
            Err(e) => {
                tracing::error!("device credential resolution failed: {e:#}");
                return crate::server::auth::json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "auth_unavailable",
                    "authentication is temporarily unavailable",
                );
            }
        }
    }
    let mut res = next.run(req).await;
    let value = match (set_cookie, clear) {
        (Some(tok), _) => Some(devices::set_cookie(&tok, t1.options.insecure_cookie)),
        (None, true) => Some(devices::clear_cookie(t1.options.insecure_cookie)),
        _ => None,
    };
    if let Some(v) = value.and_then(|v| HeaderValue::from_str(&v).ok()) {
        res.headers_mut().append(header::SET_COOKIE, v);
    }
    res
}

/// R-3 `authorize_rpc`: class + caps before anything is proxied (§1.7).
pub struct AccessAuthorizer(pub Arc<T1Access>);

impl RpcAuthorizer for AccessAuthorizer {
    fn authorize_rpc<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
        cmd: &'a str,
        _args: &'a Value,
    ) -> BoxFuture<'a, Decision> {
        Box::pin(async move {
            let deny = |e: AccessError| Decision::Deny {
                status: e.code.status(),
                code: e.code.as_str(),
                message: e.to_string(),
            };
            let b = match self.0.ctx_for(ctx, req).await {
                Ok(b) => b,
                Err(e) => return deny(e),
            };
            match super::authorize(&b.access, cmd) {
                Ok(()) => Decision::Allow,
                Err(e) => deny(e),
            }
        })
    }
}

/// The broker's [`SocketControl`]: its `ws_registry`.
struct BrokerSockets<'a>(&'a WsRegistry);

impl SocketControl for BrokerSockets<'_> {
    fn close_device(&self, device_id: &str, close: Close) -> usize {
        self.0.close_where(
            CloseReason {
                code: close.code,
                reason: close.reason,
            },
            |k| k.device_id.as_deref() == Some(device_id),
        )
    }

    fn live_sockets(&self, device_id: &str) -> usize {
        self.0
            .keys()
            .iter()
            .filter(|(_, k)| k.device_id.as_deref() == Some(device_id))
            .count()
    }
}

/// R-3 broker-side `access_*` arms, served **as root** (§9.1): never
/// proxied, never touching a path a principal names.
pub struct BrokerAccess {
    pub t1: Arc<T1Access>,
    pub ws: Arc<WsRegistry>,
}

impl AccessHandler for BrokerAccess {
    fn handle<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
        cmd: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let actx = match self.t1.ctx_for(ctx, req).await {
                Ok(b) => b.access,
                Err(e) => return Json(access_rpc::error_response(&e)).into_response(),
            };
            if let Err(e) = super::authorize(&actx, cmd) {
                return Json(access_rpc::error_response(&e)).into_response();
            }
            let principal = match self.t1.pool.acquire().await {
                Ok(mut conn) => accounts::by_id(&mut conn, ctx.principal.id)
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            };
            let sockets = BrokerSockets(&self.ws);
            let env = Env {
                tier: StoreTier::T1,
                store: Some(&self.t1.store),
                pairing: Some(&self.t1.pairing),
                sockets: &sockets,
                principal: PrincipalInfo {
                    username: principal
                        .as_ref()
                        .map(|a| a.username.clone())
                        .unwrap_or_else(|| ctx.principal.username.clone()),
                    is_admin: principal.as_ref().is_some_and(|a| a.is_admin),
                },
                public_url: self.t1.options.public_url.clone(),
                insecure_cookie: self.t1.options.insecure_cookie,
            };
            let res = match access_rpc::dispatch(&env, &actx, cmd, args).await {
                Ok(v) => crate::server::rpc::RpcResponse::success(v),
                Err(e) => access_rpc::error_response(&e),
            };
            Json(res).into_response()
        })
    }
}

/// R-3 frame hook: a frame the caps don't cover is dropped and answered
/// with the refusal control frame (§1.6); the socket stays open. The caps
/// are the handshake's snapshot (§1.4); a socket without one (no narrower
/// ran) gets none — fail closed. A change to tier, routing or membership
/// closes the socket (4403), so the snapshot is never stale.
pub struct DeviceFrames;

impl WsFrameHook for DeviceFrames {
    fn client_frame(
        &self,
        _ctx: &PrincipalCtx,
        narrowing: &Narrowing,
        path: &str,
        frame: ClientFrame<'_>,
    ) -> FrameDecision {
        let caps = narrowing
            .snapshot::<BrokerCtx>()
            .map_or(CapSet::EMPTY, |b| b.access.caps);
        let frame = match frame {
            ClientFrame::Text(t) => super::ws::Frame::Text(t),
            ClientFrame::Binary(b) => super::ws::Frame::Binary(b),
        };
        match super::ws::check_frame(caps, super::ws::Route::of(path), frame) {
            Ok(()) => FrameDecision::Pass,
            Err(missing) => FrameDecision::Reply(super::ws::refusal(missing)),
        }
    }
}

/// §1.4 / §4.5.2–§4.5.3: once per request / WS handshake, the
/// [`BrokerCtx`] — then `X-Ikenga-Caps` on every proxied request (the child
/// narrows every check to it, §1.7), the `X-Ikenga-Share-*` set and the
/// Owner as the target on a share.
pub struct AccessNarrower(pub Arc<T1Access>);

impl Narrower for AccessNarrower {
    fn narrow<'a>(
        &'a self,
        ctx: &'a PrincipalCtx,
        req: &'a Parts,
    ) -> BoxFuture<'a, Result<Narrowing, Refusal>> {
        Box::pin(async move {
            let b = self.0.access_ctx(ctx, req).await.map_err(|e| refusal(&e))?;
            let mut pairs = vec![(CAPS_HEADER, b.access.caps.to_header())];
            if let Some(share) = &b.access.share {
                pairs.extend(super::share::to_child_headers(share));
            }
            let mut headers = Vec::with_capacity(pairs.len());
            for (name, value) in pairs {
                let value = HeaderValue::from_str(&value).map_err(|_| {
                    refusal(&AccessError::new(
                        Code::Internal,
                        format!("unencodable `{name}` value"),
                    ))
                })?;
                headers.push((axum::http::HeaderName::from_static(name), value));
            }
            Ok(Narrowing {
                headers,
                target: b.owner.clone(),
                snapshot: Some(Arc::new(b)),
            })
        })
    }
}

/// R-5: an open socket stays only while its account exists, is enabled and
/// is at the socket's `session_epoch` — and, for a device socket, while the
/// device row is unrevoked and at the socket's `grant_epoch`.
pub struct DeviceEpochs {
    pub pool: SqlitePool,
}

impl StillValid for DeviceEpochs {
    fn still_valid<'a>(&'a self, key: &'a WsKey) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            let mut conn = self.pool.acquire().await?;
            let account = accounts::by_id(&mut conn, key.principal_id).await?;
            let account_ok =
                account.is_some_and(|a| !a.is_disabled() && a.session_epoch == key.session_epoch);
            if !account_ok {
                return Ok(false);
            }
            let Some(device_id) = key.device_id.as_deref() else {
                return Ok(true);
            };
            let row: Option<(i64, Option<i64>)> =
                sqlx::query_as("SELECT grant_epoch, revoked_at FROM devices WHERE device_id = ?")
                    .bind(device_id)
                    .fetch_optional(&mut *conn)
                    .await?;
            Ok(match row {
                Some((epoch, None)) => Some(epoch) == key.grant_epoch,
                _ => false,
            })
        })
    }
}

/// R-11: a forced logout (`auth.sessions_revoked`, CLI or broker) revokes
/// every device grant of the principal inside the same transaction
/// (§3.10). A store whose access set was never migrated (the broker hasn't
/// run since WP-74a) has no devices: a no-op. A newer access set is
/// refused.
#[derive(Debug, Default, Clone, Copy)]
pub struct RevokeDeviceGrants {
    pub via_cli: bool,
}

impl SessionsRevokedHook for RevokeDeviceGrants {
    fn on_sessions_revoked<'a, 'c>(
        &'a self,
        tx: &'a mut Transaction<'c, Sqlite>,
        principal_id: PrincipalId,
    ) -> HookFuture<'a>
    where
        'c: 'a,
    {
        let via = if self.via_cli {
            AuditVia::Cli
        } else {
            AuditVia::System
        };
        Box::pin(async move {
            use super::migrations::{state, SetState};
            match state(&mut **tx).await? {
                SetState::Fresh => return Ok(()),
                SetState::Current => {}
                other => anyhow::bail!("access store is not current ({other:?}); refusing"),
            }
            let meta = super::store::read_meta(&mut **tx).await?;
            let chain = super::audit::chain::Chain::new(meta.store_id);
            devices::revoke_all_for_sessions_revoked(
                &mut **tx,
                &chain,
                &principal_id.to_string(),
                via,
            )
            .await?;
            Ok(())
        })
    }
}

/// Everything `server::broker::serve` wires (R-3/R-4/R-5).
pub struct Installed {
    pub authorizer: Arc<dyn RpcAuthorizer>,
    pub access: Arc<dyn AccessHandler>,
    pub ws_frames: Arc<dyn WsFrameHook>,
    pub still_valid: Arc<dyn StillValid>,
    pub narrower: Arc<dyn Narrower>,
    pub resolver: Arc<dyn CredentialResolver>,
}

pub fn install(t1: &Arc<T1Access>, ws: Arc<WsRegistry>) -> Installed {
    Installed {
        authorizer: Arc::new(AccessAuthorizer(t1.clone())),
        access: Arc::new(BrokerAccess { t1: t1.clone(), ws }),
        ws_frames: Arc::new(DeviceFrames),
        still_valid: Arc::new(DeviceEpochs {
            pool: t1.pool.clone(),
        }),
        narrower: Arc::new(AccessNarrower(t1.clone())),
        resolver: Arc::new(DeviceGrantResolver(t1.clone())),
    }
}

/// `Code` for a broker JSON error (kept for parity with `access::http`).
pub fn code_status(code: Code) -> StatusCode {
    code.status()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::operator::{open_accounts, test_support::temp_root, Opener};

    async fn setup() -> (tempfile::TempDir, Arc<T1Access>, PrincipalId) {
        let (tmp, root) = temp_root();
        let pool = open_accounts(&root, Opener::Broker).await.unwrap();
        let store = AccessStore::attach_t1(pool.clone()).await.unwrap();
        let id = PrincipalId::new_v7();
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             shell, is_admin, session_epoch, adopted, created_at, updated_at) \
             VALUES (?, 'ada', 'ik-ada', 20001, 20001, '/h', '/bin/sh', 0, 0, 0, 0, 0)",
        )
        .bind(id.to_string())
        .execute(&pool)
        .await
        .unwrap();
        (
            tmp,
            T1Access::new(store, pool, AccessOptions::default()),
            id,
        )
    }

    async fn pair(t1: &T1Access, owner: PrincipalId, tier: Tier) -> (devices::DeviceRow, String) {
        let m = devices::mint_secret();
        let mut conn = t1.pool.acquire().await.unwrap();
        let row = devices::insert_paired(
            &mut conn,
            &owner.to_string(),
            "Pixel",
            None,
            tier,
            &m.sha256,
            None,
            None,
        )
        .await
        .unwrap();
        let tok = devices::token(&row.device_id, &m.secret);
        (row, tok)
    }

    fn parts_with(header_name: &str, value: &str) -> Parts {
        let req = axum::http::Request::builder()
            .header(header_name, value)
            .body(())
            .unwrap();
        req.into_parts().0
    }

    /// A-5 (T1): a grant resolves to its row's principal with both epochs;
    /// a disabled account or a revoked grant is rejected.
    #[tokio::test]
    async fn device_grants_resolve_with_epochs_and_respect_disable() {
        let (_tmp, t1, ada) = setup().await;
        let (row, tok) = pair(&t1, ada, Tier::Dispatch).await;
        let r = DeviceGrantResolver(t1.clone());
        let parts = parts_with("authorization", &format!("Bearer {tok}"));
        match r.resolve(&parts).await.unwrap() {
            Resolution::Resolved { ctx, epochs } => {
                assert_eq!(ctx.principal.id, ada);
                assert_eq!(ctx.via.device_id(), Some(row.device_id.as_str()));
                assert_eq!(epochs.grant_epoch, Some(0));
                let b = t1.access_ctx(&ctx, &parts).await.unwrap();
                assert_eq!(b.access.caps, Tier::Dispatch.caps());
                assert!(b.owner.is_none() && b.access.share.is_none());
            }
            other => panic!("{other:?}"),
        }
        let none = axum::http::Request::new(()).into_parts().0;
        assert!(matches!(
            r.resolve(&none).await.unwrap(),
            Resolution::NotPresent
        ));

        sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
            .bind(ada.to_string())
            .execute(&t1.pool)
            .await
            .unwrap();
        assert!(matches!(
            r.resolve(&parts).await.unwrap(),
            Resolution::Rejected(_)
        ));
    }

    /// R-5: a device socket closes when its grant epoch moves, as well as on
    /// the account's session epoch.
    #[tokio::test]
    async fn still_valid_compares_both_epochs() {
        let (_tmp, t1, ada) = setup().await;
        let (row, _) = pair(&t1, ada, Tier::Approve).await;
        let check = DeviceEpochs {
            pool: t1.pool.clone(),
        };
        let key = WsKey {
            principal_id: ada,
            session_id: String::new(),
            session_epoch: 0,
            device_id: Some(row.device_id.clone()),
            grant_epoch: Some(0),
        };
        assert!(check.still_valid(&key).await.unwrap());
        sqlx::query("UPDATE devices SET grant_epoch = 1 WHERE device_id = ?")
            .bind(&row.device_id)
            .execute(&t1.pool)
            .await
            .unwrap();
        assert!(!check.still_valid(&key).await.unwrap());
        let key = WsKey {
            grant_epoch: Some(1),
            ..key
        };
        assert!(check.still_valid(&key).await.unwrap());
        sqlx::query("UPDATE accounts SET session_epoch = 1 WHERE principal_id = ?")
            .bind(ada.to_string())
            .execute(&t1.pool)
            .await
            .unwrap();
        assert!(
            !check.still_valid(&key).await.unwrap(),
            "passwd closes device sockets"
        );
    }

    /// R-11 / A-33: forced logout revokes every grant, in its transaction.
    #[tokio::test]
    async fn forced_logout_revokes_the_grants() {
        let (_tmp, t1, ada) = setup().await;
        let (a, _) = pair(&t1, ada, Tier::View).await;
        let (b, tok_b) = pair(&t1, ada, Tier::Full).await;
        let mut conn = t1.pool.acquire().await.unwrap();
        let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
        accounts::revoke_sessions_in(
            &mut tx,
            ada,
            accounts::Actor::Cli,
            &RevokeDeviceGrants { via_cli: true },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        drop(conn);
        for id in [&a.device_id, &b.device_id] {
            let reason: Option<String> =
                sqlx::query_scalar("SELECT revoked_reason FROM devices WHERE device_id = ?")
                    .bind(id)
                    .fetch_one(&t1.pool)
                    .await
                    .unwrap();
            assert_eq!(reason.as_deref(), Some("sessions_revoked"));
        }
        let auth = devices::resolve(&t1.store, &SeenGate::default(), &tok_b, None)
            .await
            .unwrap();
        assert_eq!(auth, DeviceAuth::Invalid("revoked"));
        // The chain (broker + this CLI-shaped writer) still verifies.
        let mut conn = t1.pool.acquire().await.unwrap();
        let r = super::super::audit::chain::verify_all(&mut conn, &t1.store.meta().store_id)
            .await
            .unwrap();
        assert!(r.ok(), "{r:?}");
    }

    /// Handover lead L74-3 (broker side): no broker context reaches an
    /// `internal` arm — not the never-produced-under-T1 `OperatorBearer`
    /// (tier view, no caps), not a device whose row is gone (tier view), not
    /// a full-tier device or a password session. Only the child's
    /// `Via::ChildToken` (the broker's own marked call) does.
    #[tokio::test]
    async fn no_broker_context_reaches_an_internal_arm() {
        let (_tmp, t1, ada) = setup().await;
        let (row, tok) = pair(&t1, ada, Tier::Full).await;
        let account = {
            let mut conn = t1.pool.acquire().await.unwrap();
            accounts::by_id(&mut conn, ada).await.unwrap().unwrap()
        };
        let authz = AccessAuthorizer(t1.clone());
        let bare = axum::http::Request::new(()).into_parts().0;
        let device_parts = parts_with("authorization", &format!("Bearer {tok}"));
        let operator = PrincipalCtx {
            principal: account.principal(),
            via: Credential::OperatorBearer,
        };
        let session = PrincipalCtx {
            principal: account.principal(),
            via: Credential::Session {
                session_id: "s".into(),
            },
        };
        let device = PrincipalCtx {
            principal: account.principal(),
            via: Credential::DeviceGrant {
                device_id: row.device_id.clone(),
            },
        };
        let gone = PrincipalCtx {
            principal: account.principal(),
            via: Credential::DeviceGrant {
                device_id: "no-such-device".into(),
            },
        };
        let b = t1.access_ctx(&operator, &bare).await.unwrap();
        assert_eq!((b.access.tier, b.access.caps), (Tier::View, CapSet::EMPTY));
        let b = t1.access_ctx(&gone, &bare).await.unwrap();
        assert_eq!(b.access.tier, Tier::View, "no row: fail closed to view");
        for (ctx, parts) in [
            (&operator, &bare),
            (&session, &bare),
            (&device, &device_parts),
            (&gone, &bare),
        ] {
            for cmd in ["share_project_info", "notifications_record_access"] {
                match authz.authorize_rpc(ctx, parts, cmd, &Value::Null).await {
                    Decision::Deny { message, .. } => {
                        assert_eq!(message, "forbidden: class=internal", "{cmd} {:?}", ctx.via)
                    }
                    Decision::Allow => panic!("{cmd} allowed for {:?}", ctx.via),
                }
            }
        }
    }

    /// Handover lead L74-2 (T1): a rotation committed while resolving a
    /// cookie reaches the client on a refusal too — the new `Set-Cookie`
    /// rides the 403, so the jar never keeps a secret that is only valid
    /// for the grace.
    #[tokio::test]
    async fn a_rotated_cookie_is_set_on_a_refusal_too() {
        use tower::ServiceExt;
        let (_tmp, t1, ada) = setup().await;
        let (row, tok) = pair(&t1, ada, Tier::View).await;
        let old = devices::now_ms() - devices::ROTATE_AFTER.as_millis() as i64 - 1000;
        sqlx::query(
            "UPDATE devices SET secret_rotated_at = ?, paired_at = ?, last_seen_at = ? \
             WHERE device_id = ?",
        )
        .bind(old)
        .bind(old)
        .bind(devices::now_ms())
        .bind(&row.device_id)
        .execute(&t1.pool)
        .await
        .unwrap();
        let app = axum::Router::new()
            .route(
                "/api/rpc",
                axum::routing::post(|| async {
                    crate::server::auth::json_error(
                        StatusCode::FORBIDDEN,
                        "forbidden",
                        "forbidden: missing=dispatch",
                    )
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                t1.clone(),
                device_cookie_middleware,
            ));
        let res = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("cookie", format!("ikenga_device={tok}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let cookie = res
            .headers()
            .get(header::SET_COOKIE)
            .expect("Set-Cookie on the refusal")
            .to_str()
            .unwrap();
        let fresh = cookie
            .strip_prefix("ikenga_device=")
            .and_then(|v| v.split(';').next())
            .unwrap();
        assert_ne!(fresh, tok);
        assert!(matches!(
            devices::resolve(&t1.store, &SeenGate::default(), fresh, None)
                .await
                .unwrap(),
            DeviceAuth::Valid { .. }
        ));
    }

    #[tokio::test]
    async fn frames_and_rpcs_follow_the_device_tier() {
        let (_tmp, t1, ada) = setup().await;
        let (row, tok) = pair(&t1, ada, Tier::View).await;
        let parts = parts_with("authorization", &format!("Bearer {tok}"));
        let Resolution::Resolved { ctx, .. } = DeviceGrantResolver(t1.clone())
            .resolve(&parts)
            .await
            .unwrap()
        else {
            panic!()
        };
        let narrowing = AccessNarrower(t1.clone())
            .narrow(&ctx, &parts)
            .await
            .unwrap();
        assert_eq!(narrowing.header("x-ikenga-caps"), Some("files,sessions"));
        assert!(narrowing.target.is_none(), "own workspace: own child");
        let frames = DeviceFrames;
        assert!(matches!(
            frames.client_frame(&ctx, &narrowing, "/ws/pty/x", ClientFrame::Binary(b"ls")),
            FrameDecision::Reply(_)
        ));
        assert_eq!(
            frames.client_frame(&ctx, &narrowing, "/ws/fs", ClientFrame::Text("{}")),
            FrameDecision::Pass
        );
        let authz = AccessAuthorizer(t1.clone());
        assert!(matches!(
            authz
                .authorize_rpc(&ctx, &parts, "pty_write", &Value::Null)
                .await,
            Decision::Deny { .. }
        ));
        assert_eq!(
            authz
                .authorize_rpc(&ctx, &parts, "fs_read", &Value::Null)
                .await,
            Decision::Allow
        );
        // Operator-class arms are never proxied under T1, whatever the tier.
        sqlx::query("UPDATE devices SET tier = 'full' WHERE device_id = ?")
            .bind(&row.device_id)
            .execute(&t1.pool)
            .await
            .unwrap();
        assert!(matches!(
            authz
                .authorize_rpc(&ctx, &parts, "permission_relay_put", &Value::Null)
                .await,
            Decision::Deny { .. }
        ));
    }

    /// Review F-2: the tier is the one this request resolved — the
    /// narrowing snapshot rides with the request / socket; no shared cache.
    /// A request whose resolution predates a tier change (its
    /// `grant_epoch` is stale) never carries the new tier, and a socket keeps
    /// its handshake caps (a tier change closes it, §3.10).
    #[tokio::test]
    async fn the_tier_is_per_request_and_epoch_bound() {
        let (_tmp, t1, ada) = setup().await;
        let (row, tok) = pair(&t1, ada, Tier::Full).await;
        let parts = parts_with("authorization", &format!("Bearer {tok}"));
        let Resolution::Resolved { ctx, .. } = DeviceGrantResolver(t1.clone())
            .resolve(&parts)
            .await
            .unwrap()
        else {
            panic!()
        };
        let socket = AccessNarrower(t1.clone())
            .narrow(&ctx, &parts)
            .await
            .unwrap();
        // Downgrade: tier view, grant_epoch 0 → 1.
        sqlx::query("UPDATE devices SET tier = 'view', grant_epoch = 1 WHERE device_id = ?")
            .bind(&row.device_id)
            .execute(&t1.pool)
            .await
            .unwrap();
        let with_epoch = |e: i64| {
            let mut p = parts_with("authorization", &format!("Bearer {tok}"));
            p.extensions.insert(Epochs {
                session_epoch: 0,
                grant_epoch: Some(e),
            });
            p
        };
        let stale = t1.access_ctx(&ctx, &with_epoch(0)).await.unwrap();
        assert_eq!(stale.access.tier, Tier::View, "stale epoch: fail closed");
        let fresh = t1.access_ctx(&ctx, &with_epoch(1)).await.unwrap();
        assert_eq!(fresh.access.tier, Tier::View);
        // The open socket's snapshot is the handshake's (it is closed with
        // 4403 by the tier change, not re-read).
        assert_eq!(
            socket.snapshot::<BrokerCtx>().unwrap().access.tier,
            Tier::Full
        );
        assert_eq!(
            DeviceFrames.client_frame(&ctx, &socket, "/ws/pty/x", ClientFrame::Binary(b"ls")),
            FrameDecision::Pass
        );
        // A socket with no snapshot (no narrower ran) gets no caps.
        assert!(matches!(
            DeviceFrames.client_frame(
                &ctx,
                &Narrowing::default(),
                "/ws/pty/x",
                ClientFrame::Binary(b"ls")
            ),
            FrameDecision::Reply(_)
        ));
        // A rpc_proxy-style request reuses its snapshot, no recomputation.
        let mut p = with_epoch(1);
        p.extensions.insert(socket.clone());
        assert_eq!(t1.ctx_for(&ctx, &p).await.unwrap().access.tier, Tier::Full);
    }

    /// §4.5.2 hook site (review F-1): a share selection reaches
    /// `share::broker_select` (WP-76); until it is filled the request is
    /// refused, never served unconfined in the caller's own workspace.
    #[tokio::test]
    async fn a_share_selection_goes_through_the_share_hook() {
        let (_tmp, t1, ada) = setup().await;
        let (_, tok) = pair(&t1, ada, Tier::Full).await;
        let parts = parts_with("authorization", &format!("Bearer {tok}"));
        let Resolution::Resolved { ctx, .. } = DeviceGrantResolver(t1.clone())
            .resolve(&parts)
            .await
            .unwrap()
        else {
            panic!()
        };
        let mut share = axum::http::Request::builder()
            .uri("/ws/chat/x?share=o%2Fp")
            .body(())
            .unwrap()
            .into_parts()
            .0;
        share.extensions = parts.extensions.clone();
        let r = AccessNarrower(t1.clone()).narrow(&ctx, &share).await;
        let refusal = r.unwrap_err();
        assert!(refusal.message.contains("WP-76"), "{refusal:?}");
        assert!(matches!(
            AccessAuthorizer(t1.clone())
                .authorize_rpc(&ctx, &share, "fs_read", &Value::Null)
                .await,
            Decision::Deny { .. }
        ));
    }
}
