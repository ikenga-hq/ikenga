//! Broker tests: `/auth/*`, the cookie, the `Origin` gate, I-6 under T1, the
//! proxy and its R-3 hooks, and I-8 socket revocation — all against a fake
//! child launcher that serves in-process, so they run unprivileged. The real
//! launcher (setuid children, I-7, I-8 against `accounts passwd`) is covered
//! by the root tests in `server/tests/broker_t1.rs`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use super::children::{tests::FakeProcess, ChildLauncher, LaunchedChild};
use super::proxy::{Decision, RpcAuthorizer};
use super::*;
use crate::executor::{Principal, PrincipalId};
use crate::server::auth::BoxFuture;
use crate::server::operator::password::hash_blocking;
use crate::server::operator::test_support;

const PASSWORD: &str = "correct horse battery";

/// What one fake child saw.
#[derive(Debug, Clone)]
struct Seen {
    principal: PrincipalId,
    token: String,
    requests: Arc<Mutex<Vec<(String, HeaderMap, String)>>>,
}

/// Serves a fake child per launch on 127.0.0.1:0: `/api/rpc` and `/pkgs/*`
/// `/__viewer/*` echo what they received; `/ws/*` echoes frames.
#[derive(Default)]
struct FakeLauncher {
    launches: AtomicUsize,
    seen: Mutex<Vec<Seen>>,
    /// How long a launch takes (a slow child start, for handshake races).
    delay_ms: AtomicU64,
    /// What each child answers to `server_open_terminals` (WP-P9).
    open_terminals: Arc<AtomicU64>,
    /// How long a child takes to answer `server_open_terminals`.
    open_delay_ms: Arc<AtomicU64>,
}

impl FakeLauncher {
    fn seen_for(&self, id: PrincipalId) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|s| s.principal == id)
            .cloned()
            .expect("a child was launched for this principal")
    }
}

async fn echo_http(
    seen: Arc<Mutex<Vec<(String, HeaderMap, String)>>>,
    req: Request<Body>,
) -> impl IntoResponse {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body).into_owned();
    seen.lock()
        .unwrap()
        .push((parts.uri.to_string(), parts.headers.clone(), body.clone()));
    Json(json!({ "ok": true, "data": { "path": parts.uri.path(), "body": body } }))
}

impl ChildLauncher for FakeLauncher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        Box::pin(async move {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let delay = self.delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let requests = Arc::new(Mutex::new(Vec::new()));
            let (r1, r2, r3) = (requests.clone(), requests.clone(), requests.clone());
            let (open, open_delay) = (self.open_terminals.clone(), self.open_delay_ms.clone());
            let app = Router::new()
                .route(
                    "/api/rpc",
                    post(move |req: Request<Body>| {
                        let (r1, open, open_delay) = (r1.clone(), open.clone(), open_delay.clone());
                        async move {
                            let (parts, body) = req.into_parts();
                            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
                            let asks_open = serde_json::from_slice::<Value>(&body)
                                .is_ok_and(|v| v["cmd"] == "server_open_terminals");
                            if !asks_open {
                                return echo_http(r1, Request::from_parts(parts, Body::from(body)))
                                    .await
                                    .into_response();
                            }
                            r1.lock().unwrap().push((
                                parts.uri.to_string(),
                                parts.headers.clone(),
                                String::from_utf8_lossy(&body).into_owned(),
                            ));
                            let delay = open_delay.load(Ordering::SeqCst);
                            if delay > 0 {
                                tokio::time::sleep(Duration::from_millis(delay)).await;
                            }
                            Json(json!({
                                "ok": true,
                                "data": { "open": open.load(Ordering::SeqCst) }
                            }))
                            .into_response()
                        }
                    }),
                )
                .route(
                    "/pkgs/*rest",
                    get(move |req: Request<Body>| echo_http(r2.clone(), req)),
                )
                .route(
                    "/__viewer/*rest",
                    any(move |req: Request<Body>| echo_http(r3.clone(), req)),
                )
                .route(
                    "/ws/*rest",
                    any(|ws: WebSocketUpgrade| async move {
                        ws.on_upgrade(|mut socket| async move {
                            while let Some(Ok(msg)) = socket.recv().await {
                                if matches!(msg, Message::Close(_)) {
                                    break;
                                }
                                if socket.send(msg).await.is_err() {
                                    break;
                                }
                            }
                        })
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            self.seen.lock().unwrap().push(Seen {
                principal: principal.id,
                token: token.to_string(),
                requests,
            });
            Ok(LaunchedChild {
                addr,
                process: Box::new(FakeProcess::default()),
            })
        })
    }
}

struct Harness {
    _tmp: tempfile::TempDir,
    pool: SqlitePool,
    state: Arc<BrokerState>,
    launcher: Arc<FakeLauncher>,
    app: Router,
}

async fn insert_account(pool: &SqlitePool, username: &str, uid: u32, admin: bool) -> PrincipalId {
    let id = PrincipalId::new_v7();
    let phc = tokio::task::spawn_blocking(|| hash_blocking(PASSWORD))
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "INSERT INTO accounts (principal_id, username, password_phc, unix_name, unix_uid, \
         unix_gid, home, is_admin, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 0)",
    )
    .bind(id.to_string())
    .bind(username)
    .bind(phc)
    .bind(format!("ik-{username}"))
    .bind(i64::from(uid))
    .bind(i64::from(uid))
    .bind(format!("/srv/{username}"))
    .bind(i64::from(admin))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn harness_with(insecure_cookie: bool, hooks: impl FnOnce(&mut BrokerHooks)) -> Harness {
    harness_state(insecure_cookie, |state, _| hooks(&mut state.hooks)).await
}

/// [`harness_with`], with the whole state (and the operator root) in reach.
async fn harness_state(
    insecure_cookie: bool,
    set: impl FnOnce(&mut BrokerState, &crate::server::operator::OperatorRoot),
) -> Harness {
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let launcher = Arc::new(FakeLauncher::default());
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let mut state = BrokerState::new(pool.clone(), verifier, launcher.clone()).unwrap();
    set(&mut state, &root);
    let state = Arc::new(state);
    let store = backend::open_session_store(&root).await.unwrap();
    let app = router(
        state.clone(),
        store,
        &tmp.path().join("no-spa"),
        vec![],
        insecure_cookie,
        BrokerExtensions::default(),
    );
    Harness {
        _tmp: tmp,
        pool,
        state,
        launcher,
        app,
    }
}

async fn harness() -> Harness {
    harness_with(false, |_| {}).await
}

const HOST: &str = "ik.test:4000";

fn request(method: &str, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, json)
}

/// `name=value` of the session cookie set by `headers`, if any.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("ikenga_session="))
        .map(|v| v.split(';').next().unwrap().to_string())
}

async fn login(app: &Router, username: &str, password: &str) -> (StatusCode, HeaderMap) {
    let (status, headers, _) = send(
        app,
        request("POST", "/auth/login")
            .header("content-type", "application/json")
            .header("origin", format!("https://{HOST}"))
            .body(Body::from(
                json!({ "username": username, "password": password }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    (status, headers)
}

async fn login_cookie(app: &Router, username: &str) -> String {
    let (status, headers) = login(app, username, PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    session_cookie(&headers).expect("login sets the session cookie")
}

async fn me(app: &Router, cookie: &str) -> (StatusCode, Value) {
    let (s, _, j) = send(
        app,
        request("GET", "/auth/me")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, j)
}

async fn rpc(app: &Router, cookie: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = request("POST", "/api/rpc").header("content-type", "application/json");
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let (s, _, j) = send(app, req.body(Body::from(body.to_string())).unwrap()).await;
    (s, j)
}

// ─── /auth/* and the cookie ────────────────────────────────────────────────

#[tokio::test]
async fn login_sets_the_contract_cookie_and_me_reports_the_principal() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, true).await;

    let (status, headers) = login(&h.app, "ada", PASSWORD).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let raw = headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .find(|v| v.starts_with("ikenga_session="))
        .unwrap();
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Secure"] {
        assert!(raw.contains(attr), "{attr} missing from {raw}");
    }
    // OnInactivity(24h): the cookie carries an expiry.
    assert!(
        raw.contains("Max-Age=86400") || raw.contains("Expires="),
        "{raw}"
    );

    let cookie = session_cookie(&headers).unwrap();
    let (status, body) = me(&h.app, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "principal_id": id.to_string(), "username": "ada", "is_admin": true })
    );
}

#[tokio::test]
async fn insecure_cookie_drops_only_secure() {
    let h = harness_with(true, |_| {}).await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let (_, headers) = login(&h.app, "ada", PASSWORD).await;
    let raw = headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(!raw.contains("Secure"), "{raw}");
    assert!(
        raw.contains("HttpOnly") && raw.contains("SameSite=Strict"),
        "{raw}"
    );
}

/// Round 19, DEC-R19-1 relaxes `Secure` on the T0 daemon only: a T1 login
/// from a tailnet TCP peer still gets a `Secure` session cookie.
#[tokio::test]
async fn a_tailnet_peer_still_gets_a_secure_session_cookie() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    for peer in ["100.101.102.103", "fd7a:115c:a1e0::7", "::ffff:100.64.0.9"] {
        let mut req = request("POST", "/auth/login")
            .header("content-type", "application/json")
            .header("origin", format!("https://{HOST}"))
            .body(Body::from(
                json!({ "username": "ada", "password": PASSWORD }).to_string(),
            ))
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo(SocketAddr::new(
                peer.parse().unwrap(),
                51000,
            )));
        let (status, headers, _) = send(&h.app, req).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{peer}");
        let raw = headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .find(|v| v.starts_with("ikenga_session="))
            .unwrap();
        assert!(raw.contains("Secure"), "{peer}: {raw}");
    }
}

#[tokio::test]
async fn a_failed_login_says_nothing_and_sets_no_session() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    for (user, pw) in [("ada", "wrong password!"), ("nobody", PASSWORD)] {
        let (status, headers) = login(&h.app, user, pw).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{user}");
        assert_eq!(session_cookie(&headers), None);
    }
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM auth_events ORDER BY id")
        .fetch_all(&h.pool)
        .await
        .unwrap();
    assert_eq!(kinds, ["login_fail", "login_fail"]);
}

/// P-4: every login is a new session id, even over an existing session.
#[tokio::test]
async fn login_cycles_the_session_id() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let first = login_cookie(&h.app, "ada").await;
    let (status, headers, _) = send(
        &h.app,
        request("POST", "/auth/login")
            .header("content-type", "application/json")
            .header("cookie", &first)
            .body(Body::from(
                json!({ "username": "ada", "password": PASSWORD }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let second = session_cookie(&headers).unwrap();
    assert_ne!(first, second);
    assert_eq!(me(&h.app, &first).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&h.app, &second).await.0, StatusCode::OK);
}

#[tokio::test]
async fn logout_ends_that_session_only() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let a = login_cookie(&h.app, "ada").await;
    let b = login_cookie(&h.app, "ada").await;
    let (status, _, _) = send(
        &h.app,
        request("POST", "/auth/logout")
            .header("cookie", &a)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me(&h.app, &a).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&h.app, &b).await.0, StatusCode::OK);
    let logouts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM auth_events WHERE kind = 'logout'")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(logouts, 1);
}

#[tokio::test]
async fn password_change_checks_current_revokes_other_sessions_and_keeps_the_caller() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, false).await;
    let caller = login_cookie(&h.app, "ada").await;
    let other = login_cookie(&h.app, "ada").await;
    let change = |cookie: String, current: &'static str, new: &'static str| {
        let app = h.app.clone();
        async move {
            send(
                &app,
                request("POST", "/auth/password")
                    .header("content-type", "application/json")
                    .header("cookie", cookie)
                    .body(Body::from(
                        json!({ "current": current, "new": new }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
        }
    };

    let (status, _, body) =
        change(caller.clone(), "not the password", "a brand new password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    let (status, _, body) = change(caller.clone(), PASSWORD, "short").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "password_too_short");

    let (status, headers, _) = change(caller.clone(), PASSWORD, "a brand new password").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let renewed = session_cookie(&headers).expect("the caller gets a fresh session id");
    assert_ne!(renewed, caller);
    assert_eq!(me(&h.app, &renewed).await.0, StatusCode::OK);
    assert_eq!(me(&h.app, &other).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(me(&h.app, &caller).await.0, StatusCode::UNAUTHORIZED);

    let (epoch, kinds): (i64, Vec<String>) = (
        sqlx::query_scalar("SELECT session_epoch FROM accounts WHERE principal_id = ?")
            .bind(id.to_string())
            .fetch_one(&h.pool)
            .await
            .unwrap(),
        sqlx::query_scalar("SELECT kind FROM auth_events ORDER BY id")
            .fetch_all(&h.pool)
            .await
            .unwrap(),
    );
    assert_eq!(epoch, 1);
    assert!(kinds.contains(&"password_changed".to_string()), "{kinds:?}");
    // The new password logs in; the old doesn't.
    assert_eq!(
        login(&h.app, "ada", PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&h.app, "ada", "a brand new password").await.0,
        StatusCode::NO_CONTENT
    );
}

/// §2.2 "staying valid": a CLI-side epoch bump or disable invalidates every
/// session on its next request (the row is re-read per request).
#[tokio::test]
async fn an_epoch_bump_or_a_disable_invalidates_sessions_on_the_next_request() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::OK);
    sqlx::query("UPDATE accounts SET session_epoch = session_epoch + 1 WHERE principal_id = ?")
        .bind(id.to_string())
        .execute(&h.pool)
        .await
        .unwrap();
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::UNAUTHORIZED);

    let cookie = login_cookie(&h.app, "ada").await;
    sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
        .bind(id.to_string())
        .execute(&h.pool)
        .await
        .unwrap();
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        rpc(&h.app, Some(&cookie), json!({"cmd": "x"})).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&h.app, "ada", PASSWORD).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_rehash_on_login_moves_old_params_to_the_live_ones() {
    let h = harness().await;
    let id = PrincipalId::new_v7();
    let old = argon2::hash_encoded(
        PASSWORD.as_bytes(),
        b"0123456789abcdef",
        &argon2::Config {
            variant: argon2::Variant::Argon2id,
            version: argon2::Version::Version13,
            mem_cost: 8_192,
            time_cost: 1,
            lanes: 1,
            secret: &[],
            ad: &[],
            hash_length: 32,
        },
    )
    .unwrap();
    sqlx::query(
        "INSERT INTO accounts (principal_id, username, password_phc, unix_name, unix_uid, \
         unix_gid, home, created_at, updated_at) VALUES (?, 'ada', ?, 'ik-ada', 20001, 20001, \
         '/srv/ada', 0, 0)",
    )
    .bind(id.to_string())
    .bind(&old)
    .execute(&h.pool)
    .await
    .unwrap();
    let cookie = login_cookie(&h.app, "ada").await;
    let (phc, epoch): (String, i64) =
        sqlx::query_as("SELECT password_phc, session_epoch FROM accounts WHERE principal_id = ?")
            .bind(id.to_string())
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_ne!(phc, old);
    assert!(
        !crate::server::operator::password::needs_rehash(&phc),
        "{phc}"
    );
    assert_eq!(epoch, 0, "a rehash is not a password change");
    // The session logged in with the rehashed account stays valid.
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::OK);
}

// ─── I-6, the Origin gate ──────────────────────────────────────────────────

/// I-6 under T1: no request reaches an RPC / WS / pkg handler without a
/// `PrincipalCtx`. `?token=` and a bearer grant nothing — and no child is
/// ever launched for them.
#[tokio::test]
async fn i6_token_bearer_and_no_cookie_are_all_401() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let routes = [
        ("POST", "/api/rpc"),
        ("GET", "/ws/pty/abc"),
        ("GET", "/ws/chat/t1"),
        ("GET", "/ws/fs"),
        ("GET", "/ws/events"),
        ("GET", "/pkgs/com.x/index.html"),
        ("GET", "/pkgs/com.x/"),
        ("POST", "/api/shutdown"),
        ("GET", "/auth/me"),
        ("POST", "/auth/logout"),
        ("POST", "/auth/password"),
    ];
    for (method, path) in routes {
        for (label, uri, bearer) in [
            ("none", path.to_string(), None),
            ("?token=", format!("{path}?token=anything"), None),
            ("bearer", path.to_string(), Some("Bearer anything")),
        ] {
            let mut req = request(method, &uri)
                .header("content-type", "application/json")
                .header("upgrade", "websocket")
                .header("connection", "upgrade")
                .header("sec-websocket-version", "13")
                .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
            if let Some(b) = bearer {
                req = req.header("authorization", b);
            }
            let (status, _, _) = send(
                &h.app,
                req.body(Body::from(r#"{"cmd":"pty_list"}"#)).unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri} ({label})");
        }
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
    // The unauthenticated surface still answers.
    let (status, _, body) = send(
        &h.app,
        request("GET", "/api/health").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn the_origin_gate_covers_login_rpc_and_ws_handshakes() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let evil = "https://evil.example";

    let (status, _, _) = send(
        &h.app,
        request("POST", "/auth/login")
            .header("origin", evil)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username":"ada","password":PASSWORD}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    for (method, path) in [
        ("POST", "/api/rpc"),
        ("POST", "/auth/logout"),
        ("POST", "/auth/password"),
    ] {
        let (status, _, _) = send(
            &h.app,
            request(method, path)
                .header("origin", evil)
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"cmd":"pty_list"}"#))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    let (status, _, _) = send(
        &h.app,
        request("GET", "/ws/pty/abc")
            .header("origin", evil)
            .header("cookie", &cookie)
            .header("upgrade", "websocket")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A plain read isn't gated, and same-origin writes pass.
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::OK);
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
    let (status, _, _) = send(
        &h.app,
        request("POST", "/api/rpc")
            .header("origin", format!("http://{HOST}"))
            .header("cookie", &cookie)
            .body(Body::from(r#"{"cmd":"pty_list"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ─── the proxy and R-3 ─────────────────────────────────────────────────────

#[tokio::test]
async fn rpc_reaches_only_the_callers_child_with_its_token_and_no_client_headers() {
    let h = harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, false).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let ada_cookie = login_cookie(&h.app, "ada").await;
    let bob_cookie = login_cookie(&h.app, "bob").await;

    let (status, _, body) = send(
        &h.app,
        request("POST", "/api/rpc?token=leak")
            .header("cookie", &ada_cookie)
            .header("content-type", "application/json")
            .header("authorization", "Bearer guessed")
            .header("x-ikenga-principal", bob.to_string())
            .header("x-ikenga-caps", "everything")
            .body(Body::from(json!({"cmd":"pty_list","args":{}}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["path"], "/api/rpc");

    let seen = h.launcher.seen_for(ada);
    let reqs = seen.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    let (uri, headers, sent) = &reqs[0];
    assert_eq!(uri, "/api/rpc", "no ?token= forwarded");
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {}", seen.token)
    );
    assert_eq!(headers.get("x-ikenga-principal").unwrap(), &ada.to_string());
    assert!(headers.get("x-ikenga-caps").is_none());
    assert!(
        headers.get("cookie").is_none(),
        "the session cookie stays at the broker"
    );
    assert_eq!(
        serde_json::from_str::<Value>(sent).unwrap()["cmd"],
        "pty_list"
    );

    // Bob gets his own child; Ada's saw nothing of his.
    assert_eq!(
        rpc(&h.app, Some(&bob_cookie), json!({"cmd":"pty_list"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 2);
    assert_eq!(h.launcher.seen_for(ada).requests.lock().unwrap().len(), 1);
    assert_eq!(h.launcher.seen_for(bob).requests.lock().unwrap().len(), 1);
    assert_ne!(
        h.launcher.seen_for(ada).token,
        h.launcher.seen_for(bob).token
    );

    // pkgs, path + query preserved.
    let (status, _, body) = send(
        &h.app,
        request("GET", "/pkgs/com.x/assets/app.js?v=2")
            .header("cookie", &ada_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["path"], "/pkgs/com.x/assets/app.js");
    let (uri, _, _) = h.launcher.seen_for(ada).requests.lock().unwrap()[1].clone();
    assert_eq!(uri, "/pkgs/com.x/assets/app.js?v=2");
}

#[tokio::test]
async fn access_commands_never_reach_a_child_and_bad_bodies_fail_closed() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (status, body) = rpc(&h.app, Some(&cookie), json!({"cmd":"access_list_devices"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
    for bad in [json!([1, 2]), json!({"args": {}}), json!({"cmd": 7})] {
        assert_eq!(
            rpc(&h.app, Some(&cookie), bad).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
}

struct DenyFsWrite(Arc<Mutex<Vec<(String, String, Value)>>>);
impl RpcAuthorizer for DenyFsWrite {
    fn authorize_rpc<'a>(
        &'a self,
        ctx: &'a crate::server::auth::PrincipalCtx,
        _req: &'a axum::http::request::Parts,
        cmd: &'a str,
        args: &'a Value,
    ) -> BoxFuture<'a, Decision> {
        self.0.lock().unwrap().push((
            ctx.principal.username.clone(),
            cmd.to_string(),
            args.clone(),
        ));
        let deny = cmd == "fs_write";
        Box::pin(async move {
            if deny {
                Decision::Deny {
                    status: StatusCode::FORBIDDEN,
                    code: "forbidden",
                    message: "no writes".into(),
                }
            } else {
                Decision::Allow
            }
        })
    }
}

/// R-3: the authorizer sees `{cmd, args}` with the resolved principal and
/// decides before anything is forwarded.
#[tokio::test]
async fn the_rpc_authorizer_hook_sees_cmd_and_args_and_can_deny() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let hook_calls = calls.clone();
    let h = harness_with(false, move |hooks| {
        hooks.authorizer = Arc::new(DenyFsWrite(hook_calls));
    })
    .await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (status, body) = rpc(
        &h.app,
        Some(&cookie),
        json!({"cmd":"fs_write","args":{"path":"/x"}}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
    assert_eq!(
        rpc(&h.app, Some(&cookie), json!({"cmd":"fs_read"})).await.0,
        StatusCode::OK
    );
    let calls = calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            ("ada".into(), "fs_write".into(), json!({"path":"/x"})),
            ("ada".into(), "fs_read".into(), Value::Null),
        ]
    );
}

// ─── WebSockets and I-8 ────────────────────────────────────────────────────

/// A real listener for the broker router (WS upgrades need one).
async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    addr
}

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(
    addr: SocketAddr,
    path: &str,
    cookie: &str,
) -> Result<Client, tungstenite::Error> {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    req.headers_mut()
        .insert("origin", format!("http://{addr}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

/// The next close frame's code, within `within`.
async fn close_code(ws: &mut Client, within: Duration) -> Option<u16> {
    tokio::time::timeout(within, async {
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(tungstenite::Message::Close(frame)) => return frame.map(|f| u16::from(f.code)),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn http_post(addr: SocketAddr, path: &str, cookie: &str, body: Value) -> reqwest::StatusCode {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{addr}{path}"))
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_proxied_socket_relays_frames_and_is_registered() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    let mut ws = ws_connect(addr, "/ws/pty/abc?spawn=true", &cookie)
        .await
        .unwrap();
    ws.send(tungstenite::Message::Text("hello".into()))
        .await
        .unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(echoed, tungstenite::Message::Text("hello".into()));
    ws.send(tungstenite::Message::Binary(vec![1, 2, 3]))
        .await
        .unwrap();
    let echoed = ws.next().await.unwrap().unwrap();
    assert_eq!(echoed, tungstenite::Message::Binary(vec![1, 2, 3]));
    assert_eq!(h.state.ws.open_for(id), 1);
    ws.close(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(h.state.ws.open_for(id), 0);

    // No cookie: the handshake is refused before any upgrade.
    assert!(ws_connect(addr, "/ws/pty/abc", "ikenga_session=forged")
        .await
        .is_err());
}

/// I-8, broker-side changes: logout closes that session's sockets, and a
/// password change closes every old-epoch socket — immediately, with 4401.
#[tokio::test]
async fn i8_logout_and_password_change_close_sockets_with_4401() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let a = login_cookie(&h.app, "ada").await;
    let b = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    let mut ws_a = ws_connect(addr, "/ws/chat/t1", &a).await.unwrap();
    let mut ws_b = ws_connect(addr, "/ws/fs", &b).await.unwrap();

    assert_eq!(http_post(addr, "/auth/logout", &a, json!({})).await, 204);
    assert_eq!(
        close_code(&mut ws_a, Duration::from_secs(1)).await,
        Some(4401)
    );

    // ws_b is still alive.
    ws_b.send(tungstenite::Message::Text("ping".into()))
        .await
        .unwrap();
    assert_eq!(
        ws_b.next().await.unwrap().unwrap(),
        tungstenite::Message::Text("ping".into())
    );
    assert_eq!(
        http_post(
            addr,
            "/auth/password",
            &b,
            json!({"current": PASSWORD, "new": "another long password"})
        )
        .await,
        204
    );
    assert_eq!(
        close_code(&mut ws_b, Duration::from_secs(1)).await,
        Some(4401)
    );
}

/// I-8 for writes the broker didn't make (the CLI's): the ≤2 s re-check
/// loop, driven by `PRAGMA data_version`, closes the socket with 4401.
#[tokio::test]
async fn i8_an_external_epoch_bump_closes_sockets_within_two_seconds() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    let mut ws = ws_connect(addr, "/ws/pty/abc", &cookie).await.unwrap();

    let file: (i64, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    let conn = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&file.2)
            .read_only(true),
    )
    .await
    .unwrap();
    let (stop, _) = tokio::sync::broadcast::channel(1);
    tokio::spawn(ws_registry::recheck_loop(
        h.state.ws.clone(),
        conn,
        h.state.hooks.still_valid.clone(),
        ws_registry::RECHECK_INTERVAL,
        stop.subscribe(),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;

    // "accounts passwd" from another process: a separate connection's write.
    let mut cli = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new().filename(&file.2),
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    sqlx::query("UPDATE accounts SET session_epoch = session_epoch + 1 WHERE principal_id = ?")
        .bind(id.to_string())
        .execute(&mut cli)
        .await
        .unwrap();
    assert_eq!(
        close_code(&mut ws, Duration::from_secs(2)).await,
        Some(4401)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let _ = stop.send(());
}

/// Start the ≤2 s re-check loop against the harness's accounts.db, as
/// `serve()` does. Send on the returned channel to stop it.
async fn spawn_recheck(h: &Harness) -> (tokio::sync::broadcast::Sender<()>, String) {
    let file: (i64, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    let conn = SqliteConnection::connect_with(
        &sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&file.2)
            .read_only(true),
    )
    .await
    .unwrap();
    let (stop, _) = tokio::sync::broadcast::channel(1);
    tokio::spawn(ws_registry::recheck_loop(
        h.state.ws.clone(),
        conn,
        h.state.hooks.still_valid.clone(),
        ws_registry::RECHECK_INTERVAL,
        stop.subscribe(),
    ));
    (stop, file.2)
}

/// A handshake that was in progress when its credential was revoked: it is
/// either refused (401) or opened and then closed with 4401 — never left
/// open.
async fn assert_revoked_handshake(
    handshake: tokio::task::JoinHandle<Result<Client, tungstenite::Error>>,
) {
    match handshake.await.unwrap() {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 401),
        Ok(mut ws) => assert_eq!(
            close_code(&mut ws, Duration::from_secs(3)).await,
            Some(4401),
            "a socket that raced its revocation stayed open"
        ),
        Err(e) => panic!("unexpected handshake error: {e}"),
    }
}

/// S3-2: a CLI epoch bump that lands while the child is still being
/// launched for the handshake (the re-check pass sees an empty registry
/// unless the socket registered first).
#[tokio::test]
async fn i8_an_external_epoch_bump_inside_the_handshake_window_is_not_missed() {
    let h = harness().await;
    let id = insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    let (stop, db) = spawn_recheck(&h).await;
    h.launcher.delay_ms.store(1_500, Ordering::SeqCst);

    let handshake = tokio::spawn(async move { ws_connect(addr, "/ws/pty/abc", &cookie).await });
    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut cli =
        SqliteConnection::connect_with(&sqlx::sqlite::SqliteConnectOptions::new().filename(&db))
            .await
            .unwrap();
    sqlx::query("UPDATE accounts SET session_epoch = session_epoch + 1 WHERE principal_id = ?")
        .bind(id.to_string())
        .execute(&mut cli)
        .await
        .unwrap();
    assert_revoked_handshake(handshake).await;
    let _ = stop.send(());
}

/// S3-2: a logout of the session while its socket's child is launching.
#[tokio::test]
async fn i8_a_logout_inside_the_handshake_window_is_not_missed() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    h.launcher.delay_ms.store(1_000, Ordering::SeqCst);

    let c = cookie.clone();
    let handshake = tokio::spawn(async move { ws_connect(addr, "/ws/fs", &c).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        http_post(addr, "/auth/logout", &cookie, json!({})).await,
        204
    );
    assert_revoked_handshake(handshake).await;
}

/// S3-1: a request that loaded its session before a logout and finishes
/// after it must not write the session back (always_save + an upsert would
/// resurrect the logged-out id).
#[tokio::test]
async fn a_request_in_flight_across_logout_does_not_resurrect_the_session() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(2);
    let body = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    }));
    let req = request("POST", "/api/rpc")
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();
    let app = h.app.clone();
    let in_flight = tokio::spawn(async move { app.oneshot(req).await.unwrap() });
    tx.send(Ok(r#"{"cmd":"echo","#.into())).await.unwrap();
    // Past the session layer, holding a loaded session, reading its body.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (status, _, _) = send(
        &h.app,
        request("POST", "/auth/logout")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(me(&h.app, &cookie).await.0, StatusCode::UNAUTHORIZED);

    tx.send(Ok(r#""args":{}}"#.into())).await.unwrap();
    drop(tx);
    let res = in_flight.await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "the request itself completes");
    assert_eq!(
        me(&h.app, &cookie).await.0,
        StatusCode::UNAUTHORIZED,
        "the logged-out session came back"
    );
}

/// S3-5: a path the child would resolve to another route is refused before
/// any child is launched.
#[tokio::test]
async fn dot_segment_paths_are_refused_not_forwarded() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    for path in ["/pkgs/../api/rpc", "/pkgs/%2e%2e/api/rpc", "/pkgs/x/.%2E/y"] {
        let (status, _, _) = send(
            &h.app,
            request("GET", path)
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let addr = serve(h.app.clone()).await;
    match ws_connect(addr, "/ws/pty/%2e%2e", &cookie).await {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 400),
        other => panic!("expected a 400, got {:?}", other.map(|_| ())),
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
}

struct DropBinary;
impl super::proxy::WsFrameHook for DropBinary {
    fn client_frame(
        &self,
        _ctx: &crate::server::auth::PrincipalCtx,
        _narrowing: &super::proxy::Narrowing,
        _path: &str,
        frame: super::proxy::ClientFrame<'_>,
    ) -> super::proxy::FrameDecision {
        match frame {
            super::proxy::ClientFrame::Binary(_) => super::proxy::FrameDecision::Drop,
            super::proxy::ClientFrame::Text(_) => super::proxy::FrameDecision::Pass,
        }
    }
}

/// S3-6: binary frames (raw PTY stdin) go through the frame hook too.
#[tokio::test]
async fn the_frame_hook_sees_binary_frames_too() {
    let h = harness_with(false, |hooks| hooks.ws_frames = Arc::new(DropBinary)).await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let addr = serve(h.app.clone()).await;
    let mut ws = ws_connect(addr, "/ws/pty/abc", &cookie).await.unwrap();
    ws.send(tungstenite::Message::Binary(b"rm -rf ~\n".to_vec()))
        .await
        .unwrap();
    ws.send(tungstenite::Message::Text("after".into()))
        .await
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        next,
        tungstenite::Message::Text("after".into()),
        "the binary frame was dropped by the hook, not forwarded"
    );
}

/// G-ACCESS §4.5.3 / A-29: with a narrowing hook installed, the child sees
/// the broker's `X-Ikenga-Caps` — never the client's — and the share
/// selector `?share=` is not forwarded (§4.5.2 step 6).
#[tokio::test]
async fn the_child_sees_the_brokers_caps_header_not_the_clients() {
    struct Fixed;
    impl super::proxy::Narrower for Fixed {
        fn narrow<'a>(
            &'a self,
            _ctx: &'a crate::server::auth::PrincipalCtx,
            _req: &'a axum::http::request::Parts,
        ) -> crate::server::auth::BoxFuture<
            'a,
            Result<super::proxy::Narrowing, super::proxy::Refusal>,
        > {
            Box::pin(async {
                Ok(super::proxy::Narrowing {
                    headers: vec![(
                        axum::http::HeaderName::from_static(super::proxy::CAPS_HEADER),
                        axum::http::HeaderValue::from_static("files,sessions"),
                    )],
                    ..Default::default()
                })
            })
        }
    }
    let h = harness_with(false, |hooks| hooks.narrower = Arc::new(Fixed)).await;
    let ada = insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (status, _, _) = send(
        &h.app,
        request("POST", "/api/rpc")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .header(
                "x-ikenga-caps",
                "files,sessions,dispatch,approve,install,settings,secrets",
            )
            .body(Body::from(json!({"cmd":"pty_list","args":{}}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(
        &h.app,
        request("GET", "/pkgs/studio/index.html?share=o%2Fp&v=1")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reqs = h.launcher.seen_for(ada).requests.lock().unwrap().clone();
    let (_, headers, _) = &reqs[0];
    let caps: Vec<_> = headers.get_all("x-ikenga-caps").iter().collect();
    assert_eq!(caps, ["files,sessions"]);
    let (uri, headers, _) = &reqs[1];
    assert_eq!(uri, "/pkgs/studio/index.html?v=1");
    assert_eq!(headers.get("x-ikenga-caps").unwrap(), "files,sessions");
}

/// A narrowing refusal (e.g. an unknown share, G-ACCESS §4.5.2) answers the
/// client; nothing is proxied and no child is launched.
#[tokio::test]
async fn a_narrowing_refusal_proxies_nothing() {
    struct Refuse;
    impl super::proxy::Narrower for Refuse {
        fn narrow<'a>(
            &'a self,
            _ctx: &'a crate::server::auth::PrincipalCtx,
            _req: &'a axum::http::request::Parts,
        ) -> crate::server::auth::BoxFuture<
            'a,
            Result<super::proxy::Narrowing, super::proxy::Refusal>,
        > {
            Box::pin(async {
                Err(super::proxy::Refusal {
                    status: StatusCode::NOT_FOUND,
                    code: "not_found",
                    message: "no such project".into(),
                })
            })
        }
    }
    let h = harness_with(false, |hooks| hooks.narrower = Arc::new(Refuse)).await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (status, _, _) = send(
        &h.app,
        request("POST", "/api/rpc")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body(Body::from(json!({"cmd":"pty_list","args":{}}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let addr = serve(h.app.clone()).await;
    match ws_connect(addr, "/ws/pty/abc", &cookie).await {
        Err(tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 404),
        other => panic!("expected a 404, got {:?}", other.map(|_| ())),
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
}

// ─── an admin's edit of another principal's folder list ────────────────────

/// Gap audit 2026-10-06 rank 1: `fs_roots_*` naming another principal is an
/// admin's call. The broker routes it into the TARGET's child with the
/// argument removed and only `files, settings` granted; the caller's own
/// child sees nothing.
#[tokio::test]
async fn an_admin_edits_another_principals_folder_list_in_their_child() {
    let h = harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, true).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let cookie = login_cookie(&h.app, "ada").await;

    for (cmd, named) in [
        ("fs_roots_add", json!("bob")),
        ("fs_roots_list", json!(bob.to_string())),
    ] {
        let (status, body) = rpc(
            &h.app,
            Some(&cookie),
            json!({"cmd": cmd, "args": {"path": "/srv/bob/work", "principal": named}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let reqs = h.launcher.seen_for(bob).requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    let (_, headers, sent) = &reqs[0];
    let sent: Value = serde_json::from_str(sent).unwrap();
    assert_eq!(
        sent,
        json!({"cmd": "fs_roots_add", "args": {"path": "/srv/bob/work"}})
    );
    assert_eq!(headers.get("x-ikenga-caps").unwrap(), "files,settings");
    assert_eq!(headers.get("x-ikenga-principal").unwrap(), &bob.to_string());
    assert!(
        h.launcher
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.principal != ada),
        "the admin's own child is never involved"
    );
}

/// A non-admin naming someone else is refused before anything is looked up
/// or launched — whether or not the name exists — and so is an admin
/// demoted after signing in (the flag is read per call, not from the
/// session).
#[tokio::test]
async fn a_non_admin_cannot_edit_another_principals_folder_list() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, true).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let bob_cookie = login_cookie(&h.app, "bob").await;
    for named in ["ada", "nobody-here"] {
        let (status, body) = rpc(
            &h.app,
            Some(&bob_cookie),
            json!({"cmd": "fs_roots_reset", "args": {"principal": named}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], false);
        assert!(
            body["error"].as_str().unwrap().starts_with("forbidden:"),
            "{body}"
        );
    }

    let ada_cookie = login_cookie(&h.app, "ada").await;
    sqlx::query("UPDATE accounts SET is_admin = 0 WHERE username = 'ada'")
        .execute(&h.pool)
        .await
        .unwrap();
    let (_, body) = rpc(
        &h.app,
        Some(&ada_cookie),
        json!({"cmd": "fs_roots_add", "args": {"path": "/", "principal": bob.to_string()}}),
    )
    .await;
    assert!(
        body["error"].as_str().unwrap().starts_with("forbidden:"),
        "{body}"
    );
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
}

/// Naming oneself is the own-scope call (to one's own child, argument
/// removed); an unknown target is `not_found` for an admin; and other
/// commands carrying a `principal` argument pass through untouched.
#[tokio::test]
async fn naming_oneself_unknown_targets_and_other_commands() {
    let h = harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, true).await;
    let cookie = login_cookie(&h.app, "ada").await;

    let (_, body) = rpc(
        &h.app,
        Some(&cookie),
        json!({"cmd": "fs_roots_list", "args": {"principal": "nobody-here"}}),
    )
    .await;
    assert_eq!(
        body["error"], "not_found: no active account `nobody-here`",
        "{body}"
    );
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);

    let (status, _) = rpc(
        &h.app,
        Some(&cookie),
        json!({"cmd": "fs_roots_list", "args": {"principal": "ADA"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rpc(
        &h.app,
        Some(&cookie),
        json!({"cmd": "pty_list", "args": {"principal": "bob"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let reqs = h.launcher.seen_for(ada).requests.lock().unwrap().clone();
    let bodies: Vec<Value> = reqs
        .iter()
        .map(|(_, _, b)| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(
        bodies,
        vec![
            json!({"cmd": "fs_roots_list", "args": {}}),
            json!({"cmd": "pty_list", "args": {"principal": "bob"}}),
        ]
    );
}

// ─── in-app updates (WP-P9) ────────────────────────────────────────────────

/// A broker whose update controller reads `<tmp>/update-state` and writes
/// `<operator>/update-request.json`, as `serve()` wires it.
async fn update_harness() -> (Harness, std::path::PathBuf, std::path::PathBuf) {
    use crate::server::update::{UpdateCtl, REQUEST_FILE};
    let mut paths = None;
    let h = harness_state(false, |state, root| {
        let state_dir = root.operator_dir().parent().unwrap().join("update-state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let request = root.operator_dir().join(REQUEST_FILE);
        state.update = Some(Arc::new(UpdateCtl::with_version(
            state_dir.clone(),
            request.clone(),
            "0.20.0",
        )));
        paths = Some((state_dir, request));
    })
    .await;
    let (state_dir, request) = paths.unwrap();
    std::fs::write(
        state_dir.join("available.json"),
        json!({
            "schema": "ikenga-update-available/1", "checked_at": "2026-10-06T00:00:00Z",
            "channel": "stable", "installed": "0.20.0", "latest": "0.21.0",
            "min_upgrade_from": null, "blocked": false, "blocked_reason": null,
            "notes_url": "https://github.com/ikenga-hq/ikenga/releases/tag/v0.21.0",
            "published_at": "2026-10-05T12:00:00Z", "last_error": null
        })
        .to_string(),
    )
    .unwrap();
    (h, state_dir, request)
}

async fn update_get(app: &Router, cookie: &str) -> (StatusCode, Value) {
    let (s, _, j) = send(
        app,
        request("GET", "/api/server/update")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    (s, j)
}

async fn update_apply(app: &Router, cookie: &str, origin: &str, ack: u64) -> (StatusCode, Value) {
    let (s, _, j) = send(
        app,
        request("POST", "/api/server/update/apply")
            .header("cookie", cookie)
            .header("origin", origin)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"version": "0.21.0", "acknowledged_open_terminals": ack}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    (s, j)
}

fn same_origin() -> String {
    format!("https://{HOST}")
}

#[tokio::test]
async fn update_routes_refuse_a_member_without_leaking_versions() {
    let (h, _, request_path) = update_harness().await;
    insert_account(&h.pool, "bob", 20_002, false).await;
    let cookie = login_cookie(&h.app, "bob").await;
    let (s, body) = update_get(&h.app, &cookie).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert!(!body.to_string().contains("0.21.0") && !body.to_string().contains("0.20.0"));
    let (s, body) = update_apply(&h.app, &cookie, &same_origin(), 0).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!body.to_string().contains("0.21.0"));
    assert!(!request_path.exists());
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_admin_sees_the_update_and_terminals_summed_across_children() {
    let (h, _, request_path) = update_harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, true).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let ada_cookie = login_cookie(&h.app, "ada").await;
    let bob_cookie = login_cookie(&h.app, "bob").await;

    // No child running: nothing to ask, nothing launched.
    let (s, body) = update_get(&h.app, &ada_cookie).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["available"]["version"], "0.21.0");
    assert_eq!(body["data"]["open_terminals"], 0);
    assert_eq!(body["data"]["open_terminals_partial"], false);
    assert_eq!(
        h.launcher.launches.load(Ordering::SeqCst),
        0,
        "GET never launches"
    );

    // Two children running, three terminals each.
    h.launcher.open_terminals.store(3, Ordering::SeqCst);
    for c in [&ada_cookie, &bob_cookie] {
        assert_eq!(
            rpc(&h.app, Some(c), json!({"cmd":"pty_list"})).await.0,
            StatusCode::OK
        );
    }
    let (s, body) = update_get(&h.app, &ada_cookie).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["data"]["open_terminals"], 6);
    assert_eq!(body["data"]["open_terminals_partial"], false);

    // The broker's internal call: the per-child token, the principal, the
    // internal marker, no caps header.
    let seen = h.launcher.seen_for(bob);
    let reqs = seen.requests.lock().unwrap().clone();
    let (_, headers, sent) = reqs
        .iter()
        .find(|(_, _, b)| b.contains("server_open_terminals"))
        .expect("bob's child was asked")
        .clone();
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {}", seen.token)
    );
    assert_eq!(headers.get("x-ikenga-principal").unwrap(), &bob.to_string());
    assert_eq!(
        headers.get(crate::access::INTERNAL_CALL_HEADER).unwrap(),
        "1"
    );
    assert!(headers.get("x-ikenga-caps").is_none());
    assert!(sent.contains("server_open_terminals"));

    // Acknowledging fewer terminals than are open re-prompts with the count.
    let (s, body) = update_apply(&h.app, &ada_cookie, &same_origin(), 2).await;
    assert_eq!(s, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "terminals_open");
    assert_eq!(body["open_terminals"], 6);
    assert!(!request_path.exists());

    let (s, body) = update_apply(&h.app, &ada_cookie, &same_origin(), 6).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{body}");
    let doc: Value = serde_json::from_slice(&std::fs::read(&request_path).unwrap()).unwrap();
    assert_eq!(doc["version"], "0.21.0");
    assert_eq!(doc["requested_by"], "ada");
    assert_eq!(doc["acknowledged_open_terminals"], 6);

    // Nothing under /api/server ever reached a child.
    for id in [ada, bob] {
        for (uri, _, _) in h.launcher.seen_for(id).requests.lock().unwrap().iter() {
            assert!(!uri.starts_with("/api/server"), "{uri} was forwarded");
        }
    }
}

#[tokio::test]
async fn a_slow_child_makes_the_count_partial() {
    let (h, _, _) = update_harness().await;
    insert_account(&h.pool, "ada", 20_001, true).await;
    let cookie = login_cookie(&h.app, "ada").await;
    assert_eq!(
        rpc(&h.app, Some(&cookie), json!({"cmd":"pty_list"}))
            .await
            .0,
        StatusCode::OK
    );
    h.launcher.open_terminals.store(4, Ordering::SeqCst);
    h.launcher.open_delay_ms.store(2_500, Ordering::SeqCst);
    let (s, body) = update_get(&h.app, &cookie).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["data"]["open_terminals"], 0);
    assert_eq!(body["data"]["open_terminals_partial"], true);
}

#[tokio::test]
async fn a_demoted_or_disabled_admin_is_refused_on_the_next_request() {
    let (h, _, request_path) = update_harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, true).await;
    let cookie = login_cookie(&h.app, "ada").await;
    assert_eq!(update_get(&h.app, &cookie).await.0, StatusCode::OK);

    sqlx::query("UPDATE accounts SET is_admin = 0 WHERE principal_id = ?")
        .bind(ada.to_string())
        .execute(&h.pool)
        .await
        .unwrap();
    assert_eq!(update_get(&h.app, &cookie).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        update_apply(&h.app, &cookie, &same_origin(), 0).await.0,
        StatusCode::FORBIDDEN
    );

    // Re-promoted, then disabled without a session-epoch bump: the fresh
    // read alone still refuses.
    sqlx::query("UPDATE accounts SET is_admin = 1, disabled_at = 1 WHERE principal_id = ?")
        .bind(ada.to_string())
        .execute(&h.pool)
        .await
        .unwrap();
    let (s, _) = update_apply(&h.app, &cookie, &same_origin(), 0).await;
    assert!(
        matches!(s, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
        "{s}"
    );
    assert!(!request_path.exists());
}

#[tokio::test]
async fn a_cross_origin_apply_is_refused_and_writes_nothing() {
    let (h, _, request_path) = update_harness().await;
    insert_account(&h.pool, "ada", 20_001, true).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (s, _) = update_apply(&h.app, &cookie, "https://evil.example", 0).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(!request_path.exists());
}

#[tokio::test]
async fn without_an_update_controller_the_admin_gets_unsupported() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, true).await;
    let cookie = login_cookie(&h.app, "ada").await;
    let (s, body) = update_get(&h.app, &cookie).await;
    assert_eq!(
        (s, body["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("unsupported"))
    );
}

#[tokio::test]
async fn running_endpoints_never_launches() {
    let h = harness().await;
    insert_account(&h.pool, "ada", 20_001, false).await;
    let (eps, partial) = h.state.children.running_endpoints();
    assert!(eps.is_empty() && !partial);
    let cookie = login_cookie(&h.app, "ada").await;
    assert_eq!(
        rpc(&h.app, Some(&cookie), json!({"cmd":"pty_list"}))
            .await
            .0,
        StatusCode::OK
    );
    let (eps, partial) = h.state.children.running_endpoints();
    assert_eq!((eps.len(), partial), (1, false));
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 1);
}

// ─── /__viewer/* (capability-token previews) ──────────────────────────────

/// What a principal's child mints: `<uuid, simple form>_<random hex>`.
fn viewer_token_for(id: PrincipalId) -> String {
    format!("{}_{}", id.as_uuid().simple(), "ab".repeat(24))
}

#[test]
fn viewer_token_principal_parses_the_simple_form_only() {
    let id = PrincipalId::new_v7();
    let tok = viewer_token_for(id);
    assert_eq!(proxy::viewer_token_principal(&tok), Some(id));
    // Hyphenated, no separator, empty random part, junk, a v4 uuid.
    assert_eq!(proxy::viewer_token_principal(&format!("{id}_ab")), None);
    assert_eq!(
        proxy::viewer_token_principal(&id.as_uuid().simple().to_string()),
        None
    );
    assert_eq!(
        proxy::viewer_token_principal(&format!("{}_", id.as_uuid().simple())),
        None
    );
    assert_eq!(proxy::viewer_token_principal("deadbeef_ab"), None);
    assert_eq!(
        proxy::viewer_token_principal(&format!("{}_ab", uuid::Uuid::new_v4().simple())),
        None
    );
}

/// The mount lives in the child, and a sandboxed page sends no session
/// cookie: the token's principal prefix routes the request, to a RUNNING
/// child only, and with no cookie it never launches one.
#[tokio::test]
async fn viewer_token_routes_to_the_running_child_without_a_cookie_and_never_launches() {
    let h = harness().await;
    let ada = insert_account(&h.pool, "ada", 20_001, false).await;
    let bob = insert_account(&h.pool, "bob", 20_002, false).await;
    let (ada_tok, bob_tok) = (viewer_token_for(ada), viewer_token_for(bob));

    // No child running: nothing to launch, nothing to serve.
    for tok in [&ada_tok, &bob_tok] {
        let (status, _, _) = send(
            &h.app,
            request("GET", &format!("/__viewer/{tok}/index.html"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);

    // Malformed / hyphenated / unknown-principal prefixes: 404, no launch.
    for tok in [
        format!("{ada}_ab"),
        "nope".to_string(),
        viewer_token_for(PrincipalId::new_v7()),
    ] {
        let (status, _, _) = send(
            &h.app,
            request("GET", &format!("/__viewer/{tok}/x"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{tok}");
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);

    // Ada's child comes up through her authenticated session...
    let ada_cookie = login_cookie(&h.app, "ada").await;
    assert_eq!(
        rpc(&h.app, Some(&ada_cookie), json!({"cmd":"pty_list"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 1);

    // ...and her token now reaches it with NO cookie, as her child's bearer.
    let (status, _, body) = send(
        &h.app,
        request("GET", &format!("/__viewer/{ada_tok}/sub/a%2520b.css?x=1"))
            .header("range", "bytes=0-9")
            .header("cookie", "ikenga_session=junk")
            .header("origin", "null")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["path"],
        format!("/__viewer/{ada_tok}/sub/a%2520b.css")
    );
    let seen = h.launcher.seen_for(ada);
    let (uri, headers, _) = seen.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(uri, format!("/__viewer/{ada_tok}/sub/a%2520b.css?x=1"));
    assert_eq!(
        headers.get("authorization").unwrap(),
        &format!("Bearer {}", seen.token)
    );
    assert_eq!(headers.get("x-ikenga-principal").unwrap(), &ada.to_string());
    assert!(headers.get("cookie").is_none() && headers.get("origin").is_none());
    assert_eq!(headers.get("range").unwrap(), "bytes=0-9");
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 1);

    // Bob's token while only Ada's child runs: still no launch.
    let (status, _, _) = send(
        &h.app,
        request("GET", &format!("/__viewer/{bob_tok}/x"))
            .header("cookie", &ada_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 1);

    // With both children up, a token reaches ITS principal's child whatever
    // session cookie rides along, and never the other's.
    let bob_cookie = login_cookie(&h.app, "bob").await;
    assert_eq!(
        rpc(&h.app, Some(&bob_cookie), json!({"cmd":"pty_list"}))
            .await
            .0,
        StatusCode::OK
    );
    let ada_before = h.launcher.seen_for(ada).requests.lock().unwrap().len();
    let (status, _, _) = send(
        &h.app,
        request("GET", &format!("/__viewer/{bob_tok}/x"))
            .header("cookie", &ada_cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        h.launcher.seen_for(ada).requests.lock().unwrap().len(),
        ada_before,
        "bob's token must not reach ada's child"
    );
    let bob_seen = h.launcher.seen_for(bob);
    let (uri, _, _) = bob_seen.requests.lock().unwrap().last().unwrap().clone();
    assert_eq!(uri, format!("/__viewer/{bob_tok}/x"));

    // Dot segments and non-read methods are refused at the broker.
    for path in [
        format!("/__viewer/{ada_tok}/../api/rpc"),
        format!("/__viewer/{ada_tok}/%2e%2e/api/rpc"),
    ] {
        let (status, _, _) = send(
            &h.app,
            request("GET", &path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }
    let (status, _, _) = send(
        &h.app,
        request("POST", &format!("/__viewer/{ada_tok}/x"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

/// `/ws/events` under T1 (principal isolation through the proxy).
mod events;
