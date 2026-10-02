//! End-to-end checks of the T0 daemon router with an access store
//! (G-ACCESS §14): A-31 (the unauthenticated route set and the `/access/*`
//! `Origin` layer), route and arm enforcement for a paired device, and A-6 /
//! A-7 (revoke and tier change close a live socket with 4401 / 4403).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use super::caps::Tier;
use super::devices;
use super::store::test_support;
use super::Runtime;
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::{create_router, create_router_with_access, ServerConfig};

const TOKEN: &str = "operator-token";

fn config() -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir: None,
        auth_token: Some(TOKEN.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

async fn daemon() -> (tempfile::TempDir, Arc<Runtime>, axum::Router) {
    let (dir, store) = test_support::t0().await;
    let mut rt = Runtime::none();
    rt.store = Some(store);
    let rt = Arc::new(rt);
    let router = create_router_with_access(
        config(),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        rt.clone(),
    );
    (dir, rt, router)
}

async fn pair(rt: &Runtime, tier: Tier) -> (devices::DeviceRow, String) {
    let store = rt.store.as_ref().unwrap();
    let mut tx = store.begin().await.unwrap();
    let out = devices::issue_paired_in(
        &mut tx,
        &store.owner.unwrap().to_string(),
        "Pixel 9 · Chrome",
        None,
        tier,
        None,
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    out
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn rpc_req(cmd: &str, args: Value, cookie: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("content-type", "application/json");
    if let Some(c) = cookie {
        b = b.header("cookie", format!("ikenga_device={c}"));
    }
    b.body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
        .unwrap()
}

/// A-31: without a credential only `/api/health`, the SPA and the pairing
/// endpoints answer; the invite endpoints are T1-only; every state-changing
/// public `/access/*` route rejects a foreign `Origin`.
#[tokio::test]
async fn the_unauthenticated_route_set_is_exactly_the_contracts() {
    let router = create_router(
        config(),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
    );
    let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
    let post = |uri: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    };
    for req in [
        post("/api/rpc"),
        post("/api/shutdown"),
        get("/ws/pty/x"),
        get("/ws/chat/x"),
        get("/ws/fs"),
        get("/pkgs/x"),
        get("/pkgs/x/"),
        get("/pkgs/x/index.html"),
    ] {
        let uri = req.uri().clone();
        let (status, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    assert_eq!(send(&router, get("/api/health")).await.0, StatusCode::OK);
    // Pairing: public (stubbed until WP-74b), origin-gated on writes.
    for req in [
        post("/access/pair/hello"),
        post("/access/pair/confirm"),
        get("/access/pair/status"),
    ] {
        let uri = req.uri().clone();
        assert_eq!(
            send(&router, req).await.0,
            StatusCode::NOT_IMPLEMENTED,
            "{uri}"
        );
    }
    for uri in ["/access/pair/hello", "/access/pair/confirm"] {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "ik:4000")
            .header("origin", "https://evil.example")
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(body["error"], "forbidden");
        let same = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "ik:4000")
            .header("origin", "http://ik:4000")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&router, same).await.0, StatusCode::NOT_IMPLEMENTED);
    }
    // T0 serves no invite endpoint: it falls through to the SPA, which has
    // no such file here.
    let (status, _) = send(&router, post("/access/invite/accept")).await;
    assert_ne!(status, StatusCode::NOT_IMPLEMENTED);
}

/// §1.6 / §1.7 for a paired View device: routes and arms by class and caps,
/// spoofed caps headers ignored (A-29), the operator-only shutdown refused.
#[tokio::test]
async fn a_view_device_reads_but_cannot_act() {
    let (_d, rt, router) = daemon().await;
    let (_, token) = pair(&rt, Tier::View).await;

    let (status, body) = send(&router, rpc_req("access_status", json!({}), Some(&token))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["credential"]["via"], "device");
    assert_eq!(body["data"]["caps"], json!(["files", "sessions"]));

    for (cmd, missing) in [
        ("fs_write", "forbidden: missing=dispatch"),
        ("pty_spawn", "forbidden: missing=dispatch"),
        ("secrets_get", "forbidden: missing=secrets"),
    ] {
        let (_, body) = send(&router, rpc_req(cmd, json!({}), Some(&token))).await;
        assert_eq!(body["ok"], false, "{cmd}");
        assert_eq!(body["error"], missing, "{cmd}");
    }
    // A client-sent caps header grants nothing.
    let mut req = rpc_req("fs_write", json!({}), Some(&token));
    req.headers_mut()
        .insert("x-ikenga-caps", "files,sessions,dispatch".parse().unwrap());
    let (_, body) = send(&router, req).await;
    assert_eq!(body["error"], "forbidden: missing=dispatch");
    // Operator class.
    let req = Request::builder()
        .method("POST")
        .uri("/api/shutdown")
        .header("cookie", format!("ikenga_device={token}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&router, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden: class=operator");
    // A view device can't manage devices (P-26).
    let host = rt.store.as_ref().unwrap().host_device_id.clone().unwrap();
    let (_, body) = send(
        &router,
        rpc_req(
            "access_device_set_tier",
            json!({"deviceId": host, "tier": "full"}),
            Some(&token),
        ),
    )
    .await;
    assert!(
        body["error"].as_str().unwrap().starts_with("forbidden"),
        "{body}"
    );
}

/// Review finding 1 on T0: an encoded `spawn` is still a spawn.
#[tokio::test]
async fn an_encoded_spawn_still_needs_dispatch() {
    let (_d, rt, router) = daemon().await;
    let (_, token) = pair(&rt, Tier::View).await;
    for q in ["sp%61wn=true", "spawn=tru%65", "spawn=false&spawn=true"] {
        let req = Request::builder()
            .uri(format!("/ws/pty/new?{q}"))
            .header("cookie", format!("ikenga_device={token}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{q}");
        assert_eq!(body["error"], "forbidden: missing=dispatch", "{q}");
    }
}

/// Review finding 2: a cookie rotated while resolving a request reaches the
/// browser even when the request is then refused.
#[tokio::test]
async fn a_rotated_cookie_survives_a_refusal() {
    let (_d, rt, router) = daemon().await;
    let (phone, token) = pair(&rt, Tier::View).await;
    let old = crate::access::audit::chain::now_ms() - devices::ROTATE_AFTER_MS - 1000;
    sqlx::query("UPDATE devices SET secret_rotated_at = ?, paired_at = ? WHERE device_id = ?")
        .bind(old)
        .bind(old)
        .bind(&phone.device_id)
        .execute(&rt.store.as_ref().unwrap().pool)
        .await
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/api/shutdown")
        .header("cookie", format!("ikenga_device={token}"))
        .body(Body::empty())
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let cookie = res
        .headers()
        .get("set-cookie")
        .expect("the rotated cookie")
        .to_str()
        .unwrap();
    let fresh = cookie
        .strip_prefix("ikenga_device=")
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert_ne!(fresh, token);
    // The new secret works (the old one only for the grace window).
    let (status, body) = send(&router, rpc_req("access_status", json!({}), Some(fresh))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["credential"]["deviceId"],
        json!(phone.device_id)
    );
}

/// The operator bearer keeps working exactly as before (every existing
/// daemon test goes through it).
#[tokio::test]
async fn the_operator_bearer_is_the_host_at_full() {
    let (_d, rt, router) = daemon().await;
    let mut req = rpc_req("access_status", json!({}), None);
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (_, body) = send(&router, req).await;
    assert_eq!(body["data"]["credential"]["via"], "operator");
    assert_eq!(
        body["data"]["credential"]["deviceId"],
        json!(rt.store.as_ref().unwrap().host_device_id)
    );
    assert_eq!(body["data"]["store"], "ok");
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn serve(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}

async fn connect(addr: std::net::SocketAddr, path: &str, token: &str) -> Ws {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    req.headers_mut()
        .insert("cookie", format!("ikenga_device={token}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

/// The next close frame's code (skipping data frames), within 2 s.
async fn close_code(ws: &mut Ws) -> u16 {
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws.next().await {
            if let Ok(tungstenite::Message::Close(Some(f))) = msg {
                return u16::from(f.code);
            }
        }
        panic!("socket ended without a close frame");
    })
    .await
    .expect("closed within 2 s")
}

/// A-6: revoke closes every socket of that device with 4401, immediately
/// (in-process). A-7: a tier change closes with 4403 and the reconnect sees
/// the new caps.
#[tokio::test]
async fn revoke_and_tier_change_close_live_sockets() {
    let (_d, rt, router) = daemon().await;
    let (phone, token) = pair(&rt, Tier::Dispatch).await;
    let addr = serve(router).await;
    let op = rt.operator_ctx();

    // A-7.
    let mut chat = connect(addr, "/ws/chat/t1", &token).await;
    let mut fs = connect(addr, "/ws/fs", &token).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    super::rpc::dispatch(
        &rt,
        None,
        &op,
        "access_device_set_tier",
        &json!({"deviceId": phone.device_id, "tier": "view"}),
    )
    .await
    .unwrap();
    assert_eq!(close_code(&mut chat).await, 4403);
    assert_eq!(close_code(&mut fs).await, 4403);

    // The reconnect runs at View: a prompt is refused, the socket stays.
    let mut chat = connect(addr, "/ws/chat/t1", &token).await;
    chat.send(tungstenite::Message::Text(
        json!({"type": "prompt", "prompt": "rm -rf /"}).to_string(),
    ))
    .await
    .unwrap();
    let refusal = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(Ok(tungstenite::Message::Text(t))) = chat.next().await {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "error" {
                    return v;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        refusal,
        json!({"type":"error","code":"forbidden","missing":["dispatch"]})
    );

    // A-6 (a PTY socket, as the contract's test names, plus fs and chat).
    let (phone, token) = pair(&rt, Tier::Dispatch).await;
    let mut pty = connect(addr, "/ws/pty/e2e-a6?spawn=true", &token).await;
    let mut chat = connect(addr, "/ws/chat/t1", &token).await;
    let mut fs = connect(addr, "/ws/fs", &token).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    super::rpc::dispatch(
        &rt,
        None,
        &op,
        "access_device_revoke",
        &json!({"deviceId": phone.device_id}),
    )
    .await
    .unwrap();
    assert_eq!(close_code(&mut pty).await, 4401);
    assert_eq!(close_code(&mut chat).await, 4401);
    assert_eq!(close_code(&mut fs).await, 4401);
    // And the next request is refused, its cookie cleared.
    let res = tokio_tungstenite::connect_async({
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/ws/fs").into_client_request().unwrap();
        req.headers_mut()
            .insert("cookie", format!("ikenga_device={token}").parse().unwrap());
        req
    })
    .await;
    match res {
        Err(tungstenite::Error::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 401);
            let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
            assert!(cookie.contains("Max-Age=0"), "{cookie}");
        }
        other => panic!("expected a 401, got {other:?}"),
    }
}
