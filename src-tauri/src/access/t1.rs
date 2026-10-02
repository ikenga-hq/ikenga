//! The G-ACCESS fills for WP-20's T1 broker hooks (§10.5):
//!
//! * **R-4** — [`DeviceGrantResolver`], pushed after the session cookie in
//!   the broker's ordered `Resolvers` (§2.4 precedence: Session, then
//!   DeviceGrant; the operator bearer never resolves under T1);
//! * **R-3** — [`AccessAuthorizer`] (`authorize_rpc`: class + caps before
//!   proxying), [`BrokerAccess`] (the broker-served `access_*` arms, as
//!   root), [`DeviceFrames`] (the client → child WS frame hook) and
//!   [`CapsFor`] (`X-Ikenga-Caps` on every proxied request, §4.5.3);
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use sqlx::{Sqlite, SqlitePool, Transaction};

use super::audit::AuditVia;
use super::caps::{self, CapSet, Tier};
use super::ctx::{AccessCtx, RequestMeta, Via};
use super::devices::{self, DeviceAuth, Presented, SeenGate};
use super::rpc::{self as access_rpc, Env, PrincipalInfo, SocketControl};
use super::sockets::Close;
use super::store::{AccessStore, StoreTier};
use super::{AccessOptions, Code};
use crate::executor::PrincipalId;
use crate::server::auth::{
    BoxFuture, Credential, CredentialResolver, Epochs, PrincipalCtx, Resolution,
};
use crate::server::broker::proxy::{
    AccessHandler, CapsHeader, ClientFrame, Decision, FrameDecision, RpcAuthorizer, WsFrameHook,
};
use crate::server::broker::ws_registry::{CloseReason, StillValid, WsKey, WsRegistry};
use crate::server::operator::accounts::{self, HookFuture, SessionsRevokedHook};

/// device id → tier, as last resolved. The frame hook and the caps header
/// are synchronous, so they read the tier the resolver saw at this
/// request's / socket's resolution. A tier change closes the device's
/// sockets (4403) and updates the entry, so a socket never runs on a stale
/// tier.
#[derive(Default)]
pub struct TierCache(Mutex<HashMap<String, Tier>>);

impl TierCache {
    pub fn set(&self, device_id: &str, tier: Option<Tier>) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match tier {
            Some(t) => {
                m.insert(device_id.to_string(), t);
            }
            None => {
                m.remove(device_id);
            }
        }
    }

    pub fn get(&self, device_id: &str) -> Option<Tier> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device_id)
            .copied()
    }
}

/// The broker's shared access state.
pub struct T1Access {
    pub store: AccessStore,
    pub pool: SqlitePool,
    pub tiers: TierCache,
    pub seen: SeenGate,
    pub options: AccessOptions,
}

impl T1Access {
    pub fn new(store: AccessStore, pool: SqlitePool, options: AccessOptions) -> Arc<Self> {
        Arc::new(Self {
            store,
            pool,
            tiers: TierCache::default(),
            seen: SeenGate::default(),
            options,
        })
    }

    /// The tier a resolved credential carries (§1.3): a password session is
    /// `full`; a device its row's tier (fail closed to `view` if unknown).
    pub fn tier_of(&self, ctx: &PrincipalCtx) -> Tier {
        match &ctx.via {
            Credential::Session { .. } => Tier::Full,
            Credential::DeviceGrant { device_id } => {
                self.tiers.get(device_id).unwrap_or(Tier::View)
            }
            // Never produced under T1 (§2.4); grant nothing beyond view.
            Credential::OperatorBearer => Tier::View,
        }
    }

    /// The effective caps of an own-workspace request (§1.4). Shares are
    /// WP-76's; T1 own workspace means role = Owner, so the tier decides.
    pub fn caps_of(&self, ctx: &PrincipalCtx) -> CapSet {
        if matches!(ctx.via, Credential::OperatorBearer) {
            return CapSet::EMPTY;
        }
        caps::effective(
            caps::RoleContext::OwnWorkspace,
            self.tier_of(ctx),
            CapSet::ALL,
            super::routing::routing_ok_default(),
        )
    }

    /// `PrincipalCtx` → [`AccessCtx`] (§2.3, built right after resolution).
    pub fn access_ctx(&self, ctx: &PrincipalCtx, parts: Option<&Parts>) -> AccessCtx {
        let via = Via::from(&ctx.via);
        let tier = self.tier_of(ctx);
        let meta = RequestMeta {
            remote_addr: parts.and_then(remote_addr),
            user_agent: parts.and_then(|p| {
                p.headers
                    .get(header::USER_AGENT)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            }),
        };
        AccessCtx {
            principal_id: ctx.principal.id,
            admin_strength: AccessCtx::admin_strength_of(&via, tier)
                && !matches!(via, Via::Operator),
            device_id: ctx.via.device_id().map(str::to_string),
            via,
            tier,
            share: None,
            caps: self.caps_of(ctx),
            meta,
        }
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
                    let raw = devices::bearer_from(&parts.headers)
                        .or_else(|| devices::cookie_from(&parts.headers));
                    let Some(raw) = raw else {
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
            self.0.tiers.set(&row.device_id, Some(row.tier));
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
    let cookie = devices::cookie_from(req.headers());
    let bearer = devices::bearer_from(req.headers());
    let (raw, presented) = match (bearer, cookie) {
        (Some(b), _) => (Some(b), Presented::Bearer),
        (None, Some(c)) => (Some(c), Presented::Cookie),
        (None, None) => (None, Presented::Cookie),
    };
    let mut set_cookie = None;
    let mut clear = false;
    if let Some(raw) = raw {
        let addr = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip().to_string());
        match devices::resolve(&t1.store, &t1.seen, &raw, addr.as_deref()).await {
            Ok(auth) => {
                match &auth {
                    DeviceAuth::Valid { row, rotation_due } => {
                        // WS handshakes never rotate (§3.9).
                        let upgrade = req.headers().contains_key(header::UPGRADE);
                        if *rotation_due && presented == Presented::Cookie && !upgrade {
                            match devices::rotate(&t1.store, &row.device_id).await {
                                Ok(tok) => set_cookie = tok,
                                Err(e) => tracing::warn!("device cookie rotation: {e:#}"),
                            }
                        }
                    }
                    DeviceAuth::Invalid(_) => clear = presented == Presented::Cookie,
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
            let actx = self.0.access_ctx(ctx, Some(req));
            match super::authorize(&actx, cmd) {
                Ok(()) => Decision::Allow,
                Err(e) => Decision::Deny {
                    status: e.code.status(),
                    code: e.code.as_str(),
                    message: e.to_string(),
                },
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
            let actx = self.t1.access_ctx(ctx, Some(req));
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
            let tiers = &self.t1.tiers;
            let refresh = move |id: &str, tier: Option<Tier>| tiers.set(id, tier);
            let sockets = BrokerSockets(&self.ws);
            let env = Env {
                tier: StoreTier::T1,
                store: Some(&self.t1.store),
                sockets: &sockets,
                principal: PrincipalInfo {
                    username: principal
                        .as_ref()
                        .map(|a| a.username.clone())
                        .unwrap_or_else(|| ctx.principal.username.clone()),
                    is_admin: principal.as_ref().is_some_and(|a| a.is_admin),
                },
                public_url: self.t1.options.public_url.clone(),
                on_device_changed: Some(&refresh),
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
/// with the refusal control frame (§1.6); the socket stays open.
pub struct DeviceFrames(pub Arc<T1Access>);

impl WsFrameHook for DeviceFrames {
    fn client_frame(
        &self,
        ctx: &PrincipalCtx,
        path: &str,
        frame: ClientFrame<'_>,
    ) -> FrameDecision {
        let caps = self.0.caps_of(ctx);
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

/// §4.5.3: `X-Ikenga-Caps` on every proxied request — the child narrows
/// every check to it (§1.7).
pub struct CapsFor(pub Arc<T1Access>);

impl CapsHeader for CapsFor {
    fn caps_header(&self, ctx: &PrincipalCtx) -> Option<String> {
        Some(self.0.caps_of(ctx).to_header())
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
    pub caps: Arc<dyn CapsHeader>,
    pub resolver: Arc<dyn CredentialResolver>,
}

pub fn install(t1: &Arc<T1Access>, ws: Arc<WsRegistry>) -> Installed {
    Installed {
        authorizer: Arc::new(AccessAuthorizer(t1.clone())),
        access: Arc::new(BrokerAccess { t1: t1.clone(), ws }),
        ws_frames: Arc::new(DeviceFrames(t1.clone())),
        still_valid: Arc::new(DeviceEpochs {
            pool: t1.pool.clone(),
        }),
        caps: Arc::new(CapsFor(t1.clone())),
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
                assert_eq!(t1.caps_of(&ctx), Tier::Dispatch.caps());
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
        let frames = DeviceFrames(t1.clone());
        assert!(matches!(
            frames.client_frame(&ctx, "/ws/pty/x", ClientFrame::Binary(b"ls")),
            FrameDecision::Reply(_)
        ));
        assert_eq!(
            frames.client_frame(&ctx, "/ws/fs", ClientFrame::Text("{}")),
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
        assert_eq!(
            CapsFor(t1.clone()).caps_header(&ctx).as_deref(),
            Some("files,sessions")
        );
        // Operator-class arms are never proxied under T1.
        t1.tiers.set(&row.device_id, Some(Tier::Full));
        assert!(matches!(
            authz
                .authorize_rpc(&ctx, &parts, "permission_relay_put", &Value::Null)
                .await,
            Decision::Deny { .. }
        ));
    }
}
