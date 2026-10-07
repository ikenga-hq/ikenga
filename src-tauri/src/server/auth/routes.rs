//! `/auth/*` (G-PRINCIPAL §2.2).
//!
//! | Route | Auth | Does |
//! |---|---|---|
//! | `POST /auth/login {username, password}` | none (Origin-gated) | §6.2 verifier; rehash if the argon2 params moved; cycle the session id (P-4); `204` + `Set-Cookie` |
//! | `POST /auth/logout` | session | flush the session; close **that session's** sockets (I-8) |
//! | `GET /auth/me` | session | `{principal_id, username, is_admin}` |
//! | `POST /auth/password {current, new}` | session | verify `current` (throttled); set `new` (bumps the epoch); close the principal's old-epoch sockets **immediately**; keep the caller signed in on a fresh session id |
//!
//! A failed login says nothing about why (unknown user, wrong password,
//! disabled): the reason is in `auth_events.detail` only.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use axum_login::AuthSession;
use serde::Deserialize;
use tower_sessions::Session;
use zeroize::Zeroizing;

use super::backend::{AccountsBackend, BrokerUser};
use super::{json_error, PrincipalCtx};
use crate::server::broker::BrokerState;
use crate::server::operator::accounts::{self, Account, Actor};
use crate::server::operator::auth_events::{self, AuthEvent, AuthEventKind};
use crate::server::operator::password::{
    self, LoginAttempt, LoginOutcome, LoginSubject, PasswordPolicyError,
};

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct PasswordBody {
    pub current: String,
    pub new: String,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn remote_addr(conn: &Option<ConnectInfo<SocketAddr>>, headers: &HeaderMap) -> Option<String> {
    crate::server::trusted_proxy::client_addr_from_conn(conn, headers)
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.chars().take(256).collect())
}

fn internal(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("{what}: {e}");
    json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "internal error",
    )
}

fn throttled(retry_after_ms: u64) -> Response {
    let mut res = json_error(
        StatusCode::TOO_MANY_REQUESTS,
        "throttled",
        "too many attempts; try again later",
    );
    let secs = retry_after_ms.div_ceil(1000).max(1);
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res
}

/// §6.2 rehash-on-login. Best effort: a failure leaves the old (still
/// valid) hash and is logged.
async fn rehash_if_needed(
    state: &BrokerState,
    account: Account,
    password: Zeroizing<String>,
) -> Account {
    let Some(old) = account.password_phc.clone() else {
        return account;
    };
    if !password::needs_rehash(&old) {
        return account;
    }
    let rehashed = async {
        let new = password::hash(password).await?;
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated =
            accounts::rehash_password_in(&mut tx, account.principal_id, &old, &new).await?;
        tx.commit().await?;
        anyhow::Ok(updated)
    }
    .await;
    match rehashed {
        Ok(Some(updated)) => {
            tracing::info!(
                "rehashed the password of {} at the live argon2 parameters",
                account.principal_id
            );
            updated
        }
        Ok(None) => account,
        Err(e) => {
            tracing::warn!("rehash for {} failed: {e:#}", account.principal_id);
            account
        }
    }
}

/// `POST /auth/login`.
pub async fn login(
    State(state): State<Arc<BrokerState>>,
    mut auth: AuthSession<AccountsBackend>,
    session: Session,
    conn: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<LoginBody>,
) -> Response {
    let password = Zeroizing::new(body.password);
    let attempt = LoginAttempt {
        username: body.username,
        password: password.clone(),
        remote_addr: remote_addr(&conn, &headers),
        user_agent: user_agent(&headers),
    };
    let account = match state.verifier.login(&state.pool, attempt).await {
        Ok(LoginOutcome::Ok(account)) => account,
        Ok(LoginOutcome::Failed) => {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "wrong username or password",
            )
        }
        Ok(LoginOutcome::Throttled { retry_after_ms }) => return throttled(retry_after_ms),
        Err(e) => return internal("login", format!("{e:#}")),
    };
    let account = rehash_if_needed(&state, account, password).await;
    // P-4: a fresh id on every login (axum-login only cycles an anonymous
    // session; this also covers re-login over an existing one).
    if let Err(e) = session.cycle_id().await {
        return internal("cycle session id", e);
    }
    if let Err(e) = auth.login(&BrokerUser::new(account)).await {
        return internal("session login", e);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /auth/logout`.
pub async fn logout(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    mut auth: AuthSession<AccountsBackend>,
    conn: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = auth.logout().await {
        return internal("session logout", e);
    }
    // I-8: this session's sockets, now — not on their next frame.
    if let Some(session_id) = ctx.via.session_id() {
        state.ws.close_session(session_id);
    }
    let recorded = async {
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        // plans/pwa S2: this session's push subscriptions end with it.
        if let Some(session_id) = ctx.via.session_id() {
            crate::server::push::store::delete_for_session(&mut tx, session_id).await?;
        }
        auth_events::record(
            &mut tx,
            AuthEvent::new(AuthEventKind::Logout)
                .principal(ctx.principal.id)
                .remote_addr(remote_addr(&conn, &headers))
                .user_agent(user_agent(&headers)),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = recorded {
        tracing::warn!("could not record logout: {e}");
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /auth/me`.
pub async fn me(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
) -> Response {
    let account = async {
        let mut conn = state.pool.acquire().await?;
        accounts::by_id(&mut conn, ctx.principal.id).await
    }
    .await;
    match account {
        Ok(Some(a)) => Json(serde_json::json!({
            "principal_id": a.principal_id,
            "username": a.username,
            "is_admin": a.is_admin,
        }))
        .into_response(),
        Ok(None) => super::unauthenticated(),
        Err(e) => internal("auth/me", e),
    }
}

/// `POST /auth/password`.
pub async fn change_password(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    mut auth: AuthSession<AccountsBackend>,
    session: Session,
    conn: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<PasswordBody>,
) -> Response {
    let id = ctx.principal.id;
    let addr = remote_addr(&conn, &headers);
    let current = Zeroizing::new(body.current);
    let new = Zeroizing::new(body.new);

    // Same backoff as login: a stolen session must not become a password
    // oracle.
    let subject = LoginSubject::Account(id);
    let now = now_ms();
    let ticket = match state
        .verifier
        .throttle()
        .begin(&subject, addr.as_deref(), now)
    {
        Ok(t) => t,
        Err(wait) => return throttled(wait),
    };
    let account = async {
        let mut conn = state.pool.acquire().await?;
        accounts::by_id(&mut conn, id).await
    }
    .await;
    let account = match account {
        Ok(Some(a)) if !a.is_disabled() => a,
        Ok(_) => return super::unauthenticated(),
        Err(e) => return internal("auth/password", e),
    };
    let verified = match password::verify(account.password_phc.clone(), current).await {
        Ok(v) => v,
        Err(e) => return internal("auth/password verify", format!("{e:#}")),
    };
    if !verified {
        ticket.failed(now);
        let recorded = async {
            let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
            auth_events::record(
                &mut tx,
                AuthEvent::new(AuthEventKind::LoginFail)
                    .principal(id)
                    .username_tried(account.username.clone())
                    .remote_addr(addr.clone())
                    .user_agent(user_agent(&headers))
                    .detail(serde_json::json!({
                        "via": "password_change",
                        "reason": "bad_password",
                    })),
            )
            .await?;
            tx.commit().await
        }
        .await;
        if let Err(e) = recorded {
            tracing::warn!("could not record a failed password change: {e}");
        }
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "the current password is wrong",
        );
    }
    ticket.succeeded();
    if let Err(e) = password::validate_new_password(&new) {
        let code = match e {
            PasswordPolicyError::TooShort => "password_too_short",
            PasswordPolicyError::TooLong => "password_too_long",
        };
        return json_error(StatusCode::BAD_REQUEST, code, &e.to_string());
    }
    let phc = match password::hash(new).await {
        Ok(phc) => phc,
        Err(e) => return internal("auth/password hash", format!("{e:#}")),
    };
    let updated = async {
        let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
        let updated = accounts::set_password_in(&mut tx, id, &phc, Actor::Broker).await?;
        tx.commit().await?;
        Ok::<_, sqlx::Error>(updated)
    }
    .await;
    let updated = match updated {
        Ok(a) => a,
        Err(e) => return internal("auth/password write", e),
    };
    // I-8, broker-side: immediately, not on the next re-check.
    state.ws.close_stale_epoch(id, updated.session_epoch);
    // The caller proved the old password: keep them signed in, on a new id.
    if let Err(e) = session.cycle_id().await {
        return internal("cycle session id", e);
    }
    if let Err(e) = auth.login(&BrokerUser::new(updated)).await {
        return internal("session login", e);
    }
    StatusCode::NO_CONTENT.into_response()
}
