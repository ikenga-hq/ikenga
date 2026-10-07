//! `/ws/events` end to end on a T0 router: the handshake's auth and origin
//! gate, the `ready` frame, subscribe / unsubscribe, and every wired producer
//! publishing the desktop's event name and payload on a real mutation.
//!
//! Same house pattern as the arm tests: a literal `ServerConfig`, a router
//! whose home and data dir are temp dirs (never the real `~/.ikenga`), served
//! on 127.0.0.1:0 so the socket is a real WebSocket.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use crate::db::PaDb;
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::{router_with_home, ServerConfig};

const TOKEN: &str = "events-test-token";

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn config(data_dir: PathBuf) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir: Some(data_dir),
        auth_token: Some(TOKEN.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

struct Daemon {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    db: Arc<PaDb>,
    router: Router,
    addr: SocketAddr,
}

async fn daemon() -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home) = (root.join("data"), root.join("home"));
    for d in [&data, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let router = router_with_home(
        config(data),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db.clone()),
        None,
        Some(home.clone()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = router.clone();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            served.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Daemon {
        _tmp: tmp,
        home,
        db,
        router,
        addr,
    }
}

async fn connect(addr: SocketAddr) -> Client {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/ws/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws
}

/// The next text frame as JSON, or `None` if nothing arrives within `within`.
async fn next_json(ws: &mut Client, within: Duration) -> Option<Value> {
    tokio::time::timeout(within, async {
        while let Some(Ok(msg)) = ws.next().await {
            if let tungstenite::Message::Text(t) = msg {
                return Some(serde_json::from_str(&t).unwrap());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// Skip frames until an `event` frame named `name` whose payload matches.
async fn expect_event(ws: &mut Client, name: &str, matches: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = next_json(ws, left)
            .await
            .unwrap_or_else(|| panic!("no `{name}` event within 5 s"));
        if frame["type"] == "event" && frame["event"] == name && matches(&frame["payload"]) {
            return frame["payload"].clone();
        }
    }
}

async fn ready_and_subscribe(ws: &mut Client, events: &[&str]) -> Value {
    let ready = next_json(ws, Duration::from_secs(5))
        .await
        .expect("a ready frame");
    assert_eq!(ready["type"], "ready", "{ready}");
    ws.send(tungstenite::Message::Text(
        json!({ "type": "subscribe", "events": events }).to_string(),
    ))
    .await
    .unwrap();
    // The subscribe has no reply; let the server apply it before a mutation.
    tokio::time::sleep(Duration::from_millis(100)).await;
    ready
}

async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
    let res = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ok"], true, "{cmd}: {v}");
    v["data"].clone()
}

#[tokio::test]
async fn the_handshake_refuses_a_missing_credential_and_a_foreign_origin() {
    let d = daemon().await;
    let get = |auth: Option<&str>, origin: Option<&str>| {
        let mut req = Request::builder().method("GET").uri("/ws/events");
        if let Some(a) = auth {
            req = req.header("authorization", a);
        }
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        d.router.clone().oneshot(req.body(Body::empty()).unwrap())
    };
    assert_eq!(
        get(None, None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(Some("Bearer wrong"), None).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(
            Some(&format!("Bearer {TOKEN}")),
            Some("https://evil.example")
        )
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED,
        "WebSockets skip CORS; the Origin gate is the defence"
    );

    // And over a real socket: no upgrade without the token.
    use tungstenite::client::IntoClientRequest;
    let req = format!("ws://{}/ws/events", d.addr)
        .into_client_request()
        .unwrap();
    assert!(tokio_tungstenite::connect_async(req).await.is_err());
}

#[tokio::test]
async fn ready_names_every_live_topic_and_never_an_unproduced_one() {
    let d = daemon().await;
    let mut ws = connect(d.addr).await;
    let ready = ready_and_subscribe(&mut ws, &[]).await;
    let events: Vec<&str> = ready["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    for name in [
        "settings://changed",
        "projects:active-changed",
        "actions://changed",
        "notifications://changed",
        "pa-action-paused",
        "pa-action-committed",
        "pa-action-retried",
        "pa-action-rejected",
        "seats://changed",
    ] {
        assert!(events.contains(&name), "{name} missing from {ready}");
    }
    for name in ["hooks://event", "statusline://snapshot"] {
        assert!(!events.contains(&name), "{name} has no daemon producer");
    }
}

/// Each producer the daemon has publishes the desktop's event name and
/// payload when its arm mutates state.
#[tokio::test]
async fn every_wired_producer_emits_on_mutation() {
    let d = daemon().await;
    let r = &d.router;
    let mut ws = connect(d.addr).await;
    ready_and_subscribe(
        &mut ws,
        &[
            "settings://changed",
            "projects:active-changed",
            "actions://changed",
            "notifications://changed",
            "pa-action-paused",
            "pa-action-committed",
            "pa-action-rejected",
            "seats://changed",
        ],
    )
    .await;

    // settings://changed { path }
    rpc(
        r,
        "settings_write_field",
        json!({ "scope": "personal", "field": "appearance.theme", "value": "B",
                "remove": false, "projectId": null }),
    )
    .await;
    let personal = d.home.join(".ikenga/settings.json");
    expect_event(&mut ws, "settings://changed", |p| {
        p == &json!({ "path": personal.to_string_lossy() })
    })
    .await;

    // projects:active-changed { id }
    rpc(
        r,
        "project_create",
        json!({ "id": "p1", "displayName": "P1" }),
    )
    .await;
    rpc(r, "project_set_active", json!({ "id": "p1" })).await;
    expect_event(&mut ws, "projects:active-changed", |p| {
        p == &json!({ "id": "p1" })
    })
    .await;

    // actions://changed { path, file, scope } — no `reason` for a write.
    let doc = json!({ "version": 1, "actions": [
        { "id": "ask", "name": "Ask", "scope": "personal",
          "run": { "kind": "chi", "target": "new", "prompt": "x" } }
    ]});
    rpc(
        r,
        "actions_write",
        json!({ "scope": "personal", "document": doc }),
    )
    .await;
    let actions = d.home.join(".ikenga/actions.json");
    expect_event(&mut ws, "actions://changed", |p| {
        p == &json!({ "path": actions.to_string_lossy(), "file": "actions", "scope": "personal" })
    })
    .await;

    // pa-action-paused / -committed / -rejected, the desktop's structs.
    let draft = |id: &str| json!({ "id": id, "channel": "email", "payload": { "subject": "s", "body": "b" } });
    rpc(
        r,
        "pa_actions_pause",
        json!({ "batchId": "b1", "actionId": "mail.send", "drafts": [draft("d1"), draft("d2")] }),
    )
    .await;
    expect_event(&mut ws, "pa-action-paused", |p| {
        p == &json!({ "batchId": "b1", "count": 2 })
    })
    .await;
    rpc(r, "pa_actions_commit", json!({ "draftId": "d1" })).await;
    let committed = expect_event(&mut ws, "pa-action-committed", |p| p["draftId"] == "d1").await;
    assert_eq!(committed["channel"], "email");
    rpc(r, "pa_actions_reject", json!({ "draftId": "d2" })).await;
    expect_event(&mut ws, "pa-action-rejected", |p| {
        p == &json!({ "draftId": "d2" })
    })
    .await;

    // notifications://changed: a mute change, then a created row of the
    // muted kind carries `muted: true` (stamped from this daemon's settings,
    // as the desktop forwarder does). The notification channel is
    // process-wide, so match our own row by title.
    rpc(
        r,
        "notifications_mute_kind",
        json!({ "kind": "run_finished" }),
    )
    .await;
    expect_event(&mut ws, "notifications://changed", |p| {
        p["reason"] == "mute_changed"
    })
    .await;
    let pool = d.db.ensure_pool().await.unwrap();
    crate::server::shared::notifications::record(
        &pool,
        crate::server::shared::notifications::NewNotification {
            kind: crate::server::shared::notifications::NotificationKind::RunFinished,
            title: "events-ws producer test".into(),
            body: None,
            action: None,
            source: "test".into(),
            dedupe_key: None,
            coalesce: crate::server::shared::notifications::Coalesce::Never,
        },
    )
    .await
    .unwrap();
    let created = expect_event(&mut ws, "notifications://changed", |p| {
        p["reason"] == "created" && p["notification"]["title"] == "events-ws producer test"
    })
    .await;
    assert_eq!(created["muted"], true, "{created}");

    // seats://changed { project_id, seat_id, kinds } (snake_case, as the desktop emits) from a seat command.
    let seat = rpc(
        r,
        "seats_create",
        json!({
            "req": { "projectId": "p1", "name": "lead", "engineId": "claude-code",
                     "start": { "kind": "empty" } },
            "actor": { "client": "ui" },
        }),
    )
    .await;
    let seat_id = seat["seat"]["id"].as_str().unwrap_or_default().to_string();
    let changed = expect_event(&mut ws, "seats://changed", |p| p["project_id"] == "p1").await;
    assert_eq!(changed["seat_id"], seat_id.as_str(), "{changed}");
    assert!(changed["kinds"].as_array().is_some_and(|k| !k.is_empty()));
}

/// Only subscribed topics are delivered, and an unsubscribe stops them.
#[tokio::test]
async fn subscribe_and_unsubscribe_scope_what_is_delivered() {
    let d = daemon().await;
    let r = &d.router;
    let mut ws = connect(d.addr).await;
    ready_and_subscribe(&mut ws, &["projects:active-changed"]).await;

    rpc(
        r,
        "settings_write_field",
        json!({ "scope": "personal", "field": "appearance.theme", "value": "C",
                "remove": false, "projectId": null }),
    )
    .await;
    rpc(
        r,
        "project_create",
        json!({ "id": "p1", "displayName": "P1" }),
    )
    .await;
    rpc(r, "project_set_active", json!({ "id": "p1" })).await;
    // The first frame is the project switch: the settings write before it
    // was never sent to a socket that did not subscribe to it.
    let first = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(first["event"], "projects:active-changed", "{first}");

    ws.send(tungstenite::Message::Text(
        json!({ "type": "unsubscribe", "events": ["projects:active-changed"] }).to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    rpc(r, "project_set_active", json!({ "id": "p1" })).await;
    assert!(
        next_json(&mut ws, Duration::from_millis(500))
            .await
            .is_none(),
        "nothing after the unsubscribe"
    );
}
