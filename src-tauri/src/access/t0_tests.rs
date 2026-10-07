//! T0 daemon integration tests for G-ACCESS (WP-74a): the `auth_middleware`
//! resolver (§2.3/§2.4), the RPC pre-hook (§1.6), the public route set
//! (A-31) and revoke immediacy on an open socket (A-6).

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use super::caps::Tier;
use super::devices::tests::pair;
use super::{AccessStore, DaemonAccess};
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::{create_router_with_access, ServerConfig};

const OP: &str = "operator-token-for-tests";

fn config() -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir: None,
        auth_token: Some(OP.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

async fn daemon() -> (Router, Arc<DaemonAccess>) {
    let access = DaemonAccess::with_store(AccessStore::memory_t0().await);
    let router = create_router_with_access(
        config(),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        access.clone(),
    );
    (router, access)
}

async fn rpc(
    router: &Router,
    auth: Option<(&str, String)>,
    cmd: &str,
    args: Value,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("content-type", "application/json");
    if let Some((name, value)) = auth {
        req = req.header(name, value);
    }
    let res = router
        .clone()
        .oneshot(
            req.body(Body::from(json!({"cmd": cmd, "args": args}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let cookie = res
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().to_string());
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
        cookie,
    )
}

fn bearer(t: &str) -> Option<(&'static str, String)> {
    Some(("authorization", format!("Bearer {t}")))
}

#[tokio::test]
async fn the_operator_bearer_is_the_synthetic_owner_on_the_host_device() {
    let (router, access) = daemon().await;
    let (status, body, _) = rpc(&router, bearer(OP), "access_status", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["credential"]["via"], "operator");
    assert_eq!(data["credential"]["tier"], "full");
    assert_eq!(
        data["credential"]["deviceId"],
        access
            .store()
            .unwrap()
            .meta()
            .host_device_id
            .clone()
            .unwrap()
    );
    assert_eq!(data["principal"]["principalId"], access.owner().to_string());
    assert_eq!(data["adminStrength"], true);
}

/// §1.6 at the RPC head: a `view` device reads, but can't act.
#[tokio::test]
async fn a_view_device_is_narrowed_by_the_rpc_prehook() {
    let (router, access) = daemon().await;
    let (_, tok) = pair(access.store().unwrap(), Tier::View).await;
    let (status, body, _) = rpc(&router, bearer(&tok), "access_status", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["credential"]["via"], "device");
    assert_eq!(body["data"]["credential"]["tier"], "view");
    assert_eq!(body["data"]["adminStrength"], false);

    let (_, body, _) = rpc(
        &router,
        bearer(&tok),
        "pty_write",
        json!({"id": "x", "data": "ls"}),
    )
    .await;
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "forbidden: missing=dispatch");
    let (_, body, _) = rpc(&router, bearer(&tok), "permission_relay_take", json!({})).await;
    assert_eq!(body["error"], "forbidden: class=operator");
    // A read goes through to the arm (which then answers on its own terms).
    let (_, body, _) = rpc(&router, bearer(&tok), "fs_exists", json!({"path": "/nope"})).await;
    assert!(
        !body["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("forbidden"),
        "{body}"
    );
}

/// §2.4: a bad device bearer is a 401 (no fallback); a dead cookie is
/// cleared but does not fail a request with a valid operator bearer.
#[tokio::test]
async fn bad_device_credentials() {
    let (router, access) = daemon().await;
    let (row, _tok) = pair(access.store().unwrap(), Tier::Dispatch).await;
    let forged = super::devices::token(&row.device_id, &super::devices::mint_secret().secret);
    let (status, _, _) = rpc(&router, bearer(&forged), "access_status", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {OP}"))
        .header("cookie", format!("ikenga_device={forged}"))
        .body(Body::from(
            json!({"cmd": "access_status", "args": {}}).to_string(),
        ))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(
        cookie.starts_with("ikenga_device=;") && cookie.contains("Max-Age=0"),
        "{cookie}"
    );

    // The same cookie alone: 401, and still cleared.
    let req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("cookie", format!("ikenga_device={forged}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"cmd": "access_status"}).to_string()))
        .unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().get("set-cookie").is_some());
}

/// §1.6 non-RPC routes: `/api/shutdown` is operator-only, even for a full
/// device.
#[tokio::test]
async fn shutdown_is_operator_only() {
    let (router, access) = daemon().await;
    let (_, tok) = pair(access.store().unwrap(), Tier::Full).await;
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/shutdown")
                .header("authorization", format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

/// A-31 (T0): the unauthenticated surface is `/api/health`, the SPA and the
/// three pairing endpoints; every protected route answers 401 without a
/// credential; the invite endpoints don't exist on T0.
#[tokio::test]
async fn the_public_route_set_is_exactly_health_spa_and_pairing() {
    let (router, _) = daemon().await;
    let call = |method: &'static str, path: &'static str| {
        let router = router.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(call("GET", "/api/health").await, StatusCode::OK);
    for (m, p) in [
        ("POST", "/access/pair/hello"),
        ("POST", "/access/pair/confirm"),
        ("GET", "/access/pair/status"),
    ] {
        assert_eq!(
            call(m, p).await,
            StatusCode::NOT_FOUND,
            "{p}: pairing is off until WP-74b"
        );
    }
    for (m, p) in [
        ("POST", "/api/rpc"),
        ("POST", "/api/shutdown"),
        ("GET", "/ws/fs"),
        ("GET", "/ws/events"),
        ("GET", "/ws/pty/x"),
        ("GET", "/ws/chat/x"),
        ("GET", "/pkgs/x/index.html"),
    ] {
        assert_eq!(call(m, p).await, StatusCode::UNAUTHORIZED, "{p}");
    }
    // Not an access route on T0: it falls through to the SPA fallback (no
    // invite handler answers `gone`, which only the T1 broker mounts).
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/access/invite/accept")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::GONE);
}

/// Review m7: the T0 pairing endpoints reach the daemon's registry through
/// the full router (`t0_pairing_host`: `Arc<DaemonAccess>` →
/// `PairingHost`): begin over `/api/rpc` with the operator bearer, then a
/// device's hello and confirm through `create_router` answer 200, and the
/// request shows in `access_pair_pending`.
#[tokio::test]
async fn t0_pairing_runs_through_the_full_router() {
    let (router, access) = daemon().await;
    let store_id = access.store().unwrap().meta().store_id.clone();
    let (status, body, _) = rpc(&router, bearer(OP), "access_pair_begin", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let code = super::pairing::normalize_code(body["data"]["code"].as_str().unwrap()).unwrap();
    assert_eq!(body["data"]["cookieSecure"], true);
    let post = |path: &'static str, v: Value| {
        let router = router.clone();
        async move {
            let res = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json")
                        .body(Body::from(v.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = res.status();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null),
            )
        }
    };
    let (dev, msg_a) = super::spake::device_start_with_rng(&code, &store_id, rand::rngs::OsRng);
    let (status, v) = post(
        "/access/pair/hello",
        json!({"slot": &code[..1], "msgA": super::spake::b64(&msg_a), "deviceName": "Pixel 9 · Chrome"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["storeId"], store_id);
    let pid = v["pairingId"].as_str().unwrap().to_string();
    let msg_b = super::spake::unb64(v["msgB"].as_str().unwrap()).unwrap();
    let keys = super::spake::Keys::derive(&dev.finish(&msg_b).unwrap(), &pid, &msg_a, &msg_b);
    let (status, v) = post(
        "/access/pair/confirm",
        json!({"pairingId": pid, "deviceConfirm": super::spake::b64(&keys.device_confirm())}),
    )
    .await;
    assert_eq!(
        (status, v["state"].as_str()),
        (StatusCode::OK, Some("awaiting_host"))
    );
    let (_, body, _) = rpc(&router, bearer(OP), "access_pair_pending", json!({})).await;
    assert_eq!(body["data"][0]["pairingId"], pid.as_str());
    assert_eq!(body["data"][0]["state"], "awaiting_host");
}

/// A-6 / A-7 (T0, end to end): a device's open socket closes with 4403 on a
/// tier change and 4401 on revoke, immediately.
#[tokio::test]
async fn revoke_and_tier_change_close_an_open_device_socket() {
    let (router, access) = daemon().await;
    let (row, tok) = pair(access.store().unwrap(), Tier::Dispatch).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });

    async fn open(
        addr: std::net::SocketAddr,
        tok: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/ws/fs").into_client_request().unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {tok}").parse().unwrap());
        let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        ws
    }

    async fn close_code(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> u16 {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("closed within 5 s")
                .expect("a frame")
                .expect("ok frame");
            if let tungstenite::Message::Close(Some(f)) = msg {
                return f.code.into();
            }
        }
    }

    let http = format!("http://{addr}/api/rpc");
    let post = |cmd: &'static str, args: Value| {
        let http = http.clone();
        async move {
            reqwest::Client::new()
                .post(http)
                .bearer_auth(OP)
                .header("content-type", "application/json")
                .body(json!({"cmd": cmd, "args": args}).to_string())
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
        }
    };

    let mut ws = open(addr, &tok).await;
    // Wait until the socket is registered (fs_ready is sent after upgrade).
    let _ = ws.next().await;
    let list: Value =
        serde_json::from_slice(&post("access_devices_list", json!({})).await).unwrap();
    let phone = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["deviceId"] == row.device_id)
        .unwrap()
        .clone();
    assert_eq!(phone["liveSockets"], 1);

    post(
        "access_device_set_tier",
        json!({"deviceId": row.device_id, "tier": "approve"}),
    )
    .await;
    assert_eq!(close_code(&mut ws).await, 4403);

    let mut ws = open(addr, &tok).await;
    let _ = ws.next().await;
    post("access_device_revoke", json!({"deviceId": row.device_id})).await;
    assert_eq!(close_code(&mut ws).await, 4401);
    let _ = ws.send(tungstenite::Message::Close(None)).await;

    // The next request with the revoked grant: 401.
    let res = reqwest::Client::new()
        .post(format!("http://{addr}/api/rpc"))
        .bearer_auth(&tok)
        .header("content-type", "application/json")
        .body(json!({"cmd": "access_status"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 401);
}

/// §1.7 / A-32: a principal child's router — caps only from `X-Ikenga-Caps`
/// on the per-child token, `access_*` → `served_by_broker`, no pairing
/// routes, and no `access.db` anywhere.
#[tokio::test]
async fn a_principal_child_takes_caps_only_from_the_broker_header() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config();
    cfg.data_dir = Some(tmp.path().to_path_buf());
    let access = DaemonAccess::principal_child(Default::default());
    let router = create_router_with_access(
        cfg,
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        access,
    );
    let call_as = |broker_call: bool, caps: Option<&'static str>, cmd: &'static str| {
        let router = router.clone();
        async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", format!("Bearer {OP}"))
                .header("content-type", "application/json");
            if let Some(c) = caps {
                req = req.header("x-ikenga-caps", c);
            }
            if broker_call {
                req = req.header(super::INTERNAL_CALL_HEADER, "1");
            }
            let res = router
                .oneshot(
                    req.body(Body::from(json!({"cmd": cmd, "args": {}}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            serde_json::from_slice::<Value>(&body).unwrap()
        }
    };
    let call = |caps: Option<&'static str>, cmd: &'static str| call_as(false, caps, cmd);
    // No header: no caps.
    assert_eq!(
        call(None, "pty_write").await["error"],
        "forbidden: missing=dispatch"
    );
    assert_eq!(
        call(Some("files,sessions"), "pty_write").await["error"],
        "forbidden: missing=dispatch"
    );
    let ok = call(Some("files,sessions,dispatch"), "pty_write").await;
    assert!(
        !ok["error"]
            .as_str()
            .unwrap_or_default()
            .starts_with("forbidden"),
        "{ok}"
    );
    assert!(call(Some("files"), "access_status").await["error"]
        .as_str()
        .unwrap()
        .starts_with("served_by_broker:"));
    // `internal` arms: only on the broker's own call (the marker, no caps
    // header, no share header) — never on a relayed request (L74-3).
    // (WP-76: the arm is filled, so the broker's call reaches its body.)
    assert!(call_as(true, None, "share_project_info").await["error"]
        .as_str()
        .unwrap()
        .starts_with("invalid_request:"));
    for caps in [
        None,
        Some(""),
        Some("files,sessions,dispatch,approve,manage"),
    ] {
        for cmd in ["share_project_info", "notifications_record_access"] {
            assert_eq!(call(caps, cmd).await["error"], "forbidden: class=internal");
        }
        assert_eq!(
            call_as(true, caps.or(Some("")), "share_project_info").await["error"],
            "forbidden: class=internal"
        );
    }
    assert!(!tmp.path().join("access.db").exists());
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/access/pair/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // No pairing routes on a child: the request falls to the SPA fallback,
    // never the `pair_failed` handler.
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("pair_failed"));
}

/// A-28 / A-32 (code structure): T0 access code never builds a G-PRINCIPAL
/// `Principal` or `PrincipalCtx`, and desktop code never opens the store.
#[test]
fn t0_code_builds_no_principal_and_the_desktop_never_opens_the_store() {
    for (name, src) in [
        ("access/ctx.rs", include_str!("ctx.rs")),
        ("access/devices.rs", include_str!("devices.rs")),
        ("access/rpc.rs", include_str!("rpc.rs")),
        ("access/store.rs", include_str!("store.rs")),
        ("access/mod.rs", include_str!("mod.rs")),
    ] {
        for forbidden in ["Principal {", "PrincipalCtx {"] {
            let hit = src
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .any(|l| l.contains(forbidden) && !l.contains("struct"));
            assert!(!hit, "{name} constructs `{forbidden}`");
        }
    }
    let desktop = include_str!("../commands/access.rs");
    assert!(
        !desktop.contains("AccessStore::"),
        "commands/access.rs must proxy (P-20)"
    );
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_open(
    addr: std::net::SocketAddr,
    path_and_query: &str,
    tok: &str,
) -> Result<Ws, String> {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}{path_and_query}")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {tok}").parse().unwrap());
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

/// Every text frame until the server closes (or 5 s pass).
async fn text_frames(ws: &mut Ws) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(Some(Ok(msg))) =
        tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await
    {
        match msg {
            tungstenite::Message::Text(t) => out.push(serde_json::from_str(&t).unwrap()),
            tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }
    out
}

/// Handover lead L74-1: `?spawn=` is decided once, inside
/// `pty_ws_handler`, from the very `Query<PtyQuery>` (percent-decoded) parse
/// that drives the spawn, against the socket's caps — there is no second,
/// raw-query decision to disagree with it. So every encoded spelling of
/// `spawn=true` from a `view` device is seen as a spawn request **and
/// refused** (the refusal frame, then `ikenga.gone`, no shell); a spelling
/// that isn't `spawn` is a plain attach; a duplicate is a 400 at the
/// handshake (fail closed); and an encoded route segment is not the PTY
/// route at all (axum matches the raw path, as `route_requirement` reads it).
#[tokio::test]
async fn encoded_spawn_forms_never_spawn_without_dispatch() {
    let access = DaemonAccess::with_store(AccessStore::memory_t0().await);
    let pty = Arc::new(PtyManager::new());
    let router = create_router_with_access(
        config(),
        pty.clone(),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        access.clone(),
    );
    let (_, tok) = pair(access.store().unwrap(), Tier::View).await;
    let addr = serve(router).await;
    let refusal = json!({"type": "error", "code": "forbidden", "missing": ["dispatch"]});

    for q in [
        "spawn=true",
        "sp%61wn=true",
        "spawn=tru%65",
        "%73%70%61%77%6E=%74%72%75%65",
        "spawn=true&cols=80",
        "token=x&sp%61wn=true",
    ] {
        let mut ws = ws_open(addr, &format!("/ws/pty/l74-{}?{q}", q.len()), &tok)
            .await
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        let frames = text_frames(&mut ws).await;
        assert_eq!(frames.first(), Some(&refusal), "{q}: {frames:?}");
        assert_eq!(
            frames.get(1).map(|f| &f["type"]),
            Some(&json!("ikenga.gone")),
            "{q}"
        );
    }
    // Not `spawn`: a plain attach, told the terminal is gone.
    for q in ["spawnx=true", "spawn%20=true", "spawn=false"] {
        let mut ws = ws_open(addr, &format!("/ws/pty/plain?{q}"), &tok)
            .await
            .unwrap_or_else(|e| panic!("{q}: {e}"));
        let frames = text_frames(&mut ws).await;
        assert_eq!(frames.len(), 1, "{q}: {frames:?}");
        assert_eq!(frames[0]["type"], "ikenga.gone", "{q}");
    }
    // Duplicates and unparseable values: the handshake fails.
    for q in [
        "spawn=false&spawn=true",
        "spawn=true&sp%61wn=true",
        "spawn=yes",
    ] {
        let err = ws_open(addr, &format!("/ws/pty/dup?{q}"), &tok)
            .await
            .err()
            .unwrap_or_else(|| panic!("{q} upgraded"));
        assert!(err.contains("400"), "{q}: {err}");
    }
    // An encoded route segment never reaches the PTY handler.
    for path in [
        "/ws/p%74y/x?spawn=true",
        "/ws/%70ty/x?spawn=true",
        "/ws//pty/x?spawn=true",
    ] {
        assert!(ws_open(addr, path, &tok).await.is_err(), "{path} upgraded");
    }
    assert!(pty.list_terminals().is_empty(), "no shell was spawned");
}

/// Handover lead L74-2 (T0): a cookie rotation committed while resolving
/// reaches the client on a refusal too — a route-class 403 and an RPC
/// refusal both carry the new `Set-Cookie`.
#[tokio::test]
async fn a_rotated_cookie_is_set_on_a_refusal_too() {
    let (router, access) = daemon().await;
    let store = access.store().unwrap();
    let (row, mut tok) = pair(store, Tier::View).await;
    let age = || async {
        let old = super::devices::now_ms() - super::devices::ROTATE_AFTER.as_millis() as i64 - 1000;
        sqlx::query(
            "UPDATE devices SET secret_rotated_at = ?, paired_at = ?, last_seen_at = ? \
             WHERE device_id = ?",
        )
        .bind(old)
        .bind(old)
        .bind(super::devices::now_ms())
        .bind(&row.device_id)
        .execute(store.pool())
        .await
        .unwrap();
    };
    let fresh_from = |res: &axum::response::Response| {
        res.headers()
            .get("set-cookie")
            .expect("Set-Cookie on the refusal")
            .to_str()
            .unwrap()
            .strip_prefix("ikenga_device=")
            .and_then(|v| v.split(';').next())
            .map(str::to_string)
            .filter(|v| !v.is_empty())
            .expect("a rotated (not cleared) cookie")
    };

    age().await;
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/shutdown")
                .header("cookie", format!("ikenga_device={tok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let fresh = fresh_from(&res);
    assert_ne!(fresh, tok);
    tok = fresh;

    age().await;
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("cookie", format!("ikenga_device={tok}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"cmd": "pty_write", "args": {"id": "x", "data": "ls"}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let fresh = fresh_from(&res);
    assert_ne!(fresh, tok);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"], "forbidden: missing=dispatch");
}

/// Round 19, DEC-R19-1 (T0, full router): the `auth_middleware` device
/// cookie — cleared or rotated — omits `Secure` only when the TCP peer
/// (`ConnectInfo`) is a tailnet address. A LAN peer keeps it, and so does a
/// LAN peer claiming a tailnet `X-Forwarded-For`.
#[tokio::test]
async fn t0_device_cookies_drop_secure_only_for_a_tailnet_peer() {
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    let (router, access) = daemon().await;
    let store = access.store().unwrap();
    let (row, tok) = pair(store, Tier::View).await;
    let forged = super::devices::token(&row.device_id, &super::devices::mint_secret().secret);
    let send = |cookie: String, peer: &'static str, xff: &'static str| {
        let router = router.clone();
        async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {OP}"))
                .header("cookie", format!("ikenga_device={cookie}"))
                .header("x-forwarded-for", xff)
                .body(Body::from(
                    json!({"cmd": "access_status", "args": {}}).to_string(),
                ))
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(peer.parse().unwrap(), 51000)));
            let res = router.oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            res.headers()
                .get("set-cookie")
                .expect("a Set-Cookie")
                .to_str()
                .unwrap()
                .to_string()
        }
    };

    // Clear (a dead cookie beside the operator bearer).
    for (peer, xff, secure) in [
        ("100.101.102.103", "192.168.1.9", false),
        ("fd7a:115c:a1e0::7", "192.168.1.9", false),
        ("::ffff:100.64.0.9", "192.168.1.9", false),
        ("192.168.1.9", "100.101.102.103", true),
        ("100.63.255.255", "100.101.102.103", true),
        ("127.0.0.1", "100.101.102.103", true),
    ] {
        let cookie = send(forged.clone(), peer, xff).await;
        assert!(cookie.contains("Max-Age=0"), "{cookie}");
        assert_eq!(
            cookie.contains("Secure"),
            secure,
            "clear, peer={peer}: {cookie}"
        );
    }

    // Rotate (a cookie older than 30 days).
    let age = || async {
        let old = super::devices::now_ms() - super::devices::ROTATE_AFTER.as_millis() as i64 - 1000;
        sqlx::query(
            "UPDATE devices SET secret_rotated_at = ?, paired_at = ?, last_seen_at = ? \
             WHERE device_id = ?",
        )
        .bind(old)
        .bind(old)
        .bind(super::devices::now_ms())
        .bind(&row.device_id)
        .execute(store.pool())
        .await
        .unwrap();
    };
    let mut tok = tok;
    for (peer, secure) in [("100.101.102.103", false), ("192.168.1.9", true)] {
        age().await;
        let cookie = send(tok.clone(), peer, "100.101.102.103").await;
        let fresh = cookie
            .strip_prefix("ikenga_device=")
            .and_then(|v| v.split(';').next())
            .filter(|v| !v.is_empty())
            .expect("a rotated (not cleared) cookie")
            .to_string();
        assert_ne!(fresh, tok);
        assert_eq!(
            cookie.contains("Secure"),
            secure,
            "rotate, peer={peer}: {cookie}"
        );
        tok = fresh;
    }
}

/// Round 19, DEC-R19-1: `access_pair_begin`'s `cookieSecure` predicts the
/// cookie a device opening `pairUrl` gets — false for a tailnet host on
/// T0, true for a LAN host.
#[tokio::test]
async fn pair_begin_reports_cookie_secure_by_the_pair_url_host() {
    let (router, _access) = daemon().await;
    for (base, secure) in [
        ("http://100.101.102.103:4000", false),
        ("http://[fd7a:115c:a1e0::7]:4000", false),
        ("http://ned-desktop.tail1a2b.ts.net:4000", false),
        ("http://192.168.1.9:4000", true),
        ("http://100.128.0.1:4000", true),
    ] {
        let (status, body, _) = rpc(
            &router,
            bearer(OP),
            "access_pair_begin",
            json!({"publicBase": base}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["pairUrl"],
            format!("{base}/remote/pair"),
            "{body}"
        );
        assert_eq!(body["data"]["cookieSecure"], secure, "{base}");
    }
}
