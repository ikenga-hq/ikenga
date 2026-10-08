//! `server::term_hooks` end to end on a real T0 router: the endpoint's
//! credential (missing, wrong, another terminal's, another principal's),
//! the events it publishes with the desktop's shapes, the permission gate's
//! decision (single-use, the timeout default, a dropped hook) and the
//! `pty_spawn` wiring with a real PTY.

use std::cell::Cell;
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

use super::*;
use crate::db::PaDb;
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::{router_with_home, ServerConfig};

thread_local! {
    /// Per-test hold for a parked gate (read by [`TermHooks::new`]).
    pub(super) static TEST_HOLD: Cell<Option<Duration>> = const { Cell::new(None) };
}

const TOKEN: &str = "term-hooks-test-token";
const PORT: u16 = 45871;

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Daemon {
    _tmp: tempfile::TempDir,
    data: PathBuf,
    router: Router,
    addr: SocketAddr,
}

fn config(data_dir: Option<PathBuf>, port: u16) -> ServerConfig {
    ServerConfig {
        host: "0.0.0.0".into(),
        port,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir,
        auth_token: Some(TOKEN.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

async fn daemon_with(with_data: bool, port: u16) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home) = (root.join("data"), root.join("home"));
    for d in [&data, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let router = router_with_home(
        config(with_data.then(|| data.clone()), port),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db),
        None,
        Some(home),
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
        data,
        router,
        addr,
    }
}

async fn daemon() -> Daemon {
    daemon_with(true, PORT).await
}

async fn rpc(r: &Router, cmd: &str, args: Value) -> Value {
    let res = r
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
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

async fn ok(r: &Router, cmd: &str, args: Value) -> Value {
    let res = rpc(r, cmd, args.clone()).await;
    assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
    res.get("data").cloned().unwrap_or(Value::Null)
}

/// The settings path the frontend would put in argv for `term`.
fn settings_path(d: &Daemon, term: &str) -> String {
    d.data
        .join(DIR_NAME)
        .join(terminal_file_name(term))
        .to_string_lossy()
        .into_owned()
}

/// `pty_spawn` with a short-lived real PTY, which wires `term`.
async fn wire_via_rpc(d: &Daemon, term: &str, cmd: &[&str]) -> (String, Value) {
    let res = rpc(
        &d.router,
        "pty_spawn",
        json!({
            "terminalId": term,
            "cwd": "/",
            "cmd": cmd,
            "rows": 24,
            "cols": 80,
            "settingsPath": settings_path(d, term),
        }),
    )
    .await;
    (secret_of(d, term), res)
}

/// The secret in `term`'s header file.
fn secret_of(d: &Daemon, term: &str) -> String {
    let hdr = d.data.join(DIR_NAME).join(header_file_name(term));
    std::fs::read_to_string(hdr)
        .unwrap()
        .trim()
        .strip_prefix("Authorization: Bearer ")
        .unwrap()
        .to_string()
}

async fn post(
    r: &Router,
    path: &str,
    term: Option<&str>,
    secret: Option<&str>,
    body: Value,
) -> (StatusCode, Vec<u8>) {
    let uri = match term {
        Some(t) => format!("{path}?terminal={t}"),
        None => path.to_string(),
    };
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(s) = secret {
        req = req.header("authorization", format!("Bearer {s}"));
    }
    let res = r
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

async fn post_json(
    r: &Router,
    path: &str,
    term: &str,
    secret: &str,
    body: Value,
) -> (StatusCode, Value) {
    let (status, bytes) = post(r, path, Some(term), Some(secret), body).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn connect(addr: SocketAddr) -> Client {
    connect_as(addr, &[]).await
}

/// [`connect`] with the headers a broker relays to a principal child.
async fn connect_as(addr: SocketAddr, headers: &[(&'static str, String)]) -> Client {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/ws/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
    for (k, v) in headers {
        req.headers_mut().insert(*k, v.parse().unwrap());
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let ready = next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
    assert_eq!(ready["type"], "ready", "{ready}");
    ws.send(tungstenite::Message::Text(
        json!({
            "type": "subscribe",
            "events": ["hooks://event", "hooks://decision", "statusline://snapshot"],
        })
        .to_string(),
    ))
    .await
    .unwrap();
    // The subscribe has no reply; let the server apply it.
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws
}

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

/// The payload of the next `name` event.
async fn event(ws: &mut Client, name: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = next_json(ws, left)
            .await
            .unwrap_or_else(|| panic!("no `{name}` event within 5 s"));
        if frame["type"] == "event" && frame["event"] == name {
            return frame["payload"].clone();
        }
    }
}

/// Assert no `name` event arrives for a moment.
async fn no_event(ws: &mut Client, name: &str) {
    while let Some(frame) = next_json(ws, Duration::from_millis(400)).await {
        assert!(
            !(frame["type"] == "event" && frame["event"] == name),
            "unexpected {name}: {frame}"
        );
    }
}

fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A shell that stays alive for `secs`, so the terminal's hooks stay live.
fn sleeper(secs: u32) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), format!("sleep {secs}")]
}

async fn live(d: &Daemon, term: &str, secs: u32) -> String {
    let cmd = sleeper(secs);
    let cmd: Vec<&str> = cmd.iter().map(String::as_str).collect();
    let (secret, res) = wire_via_rpc(d, term, &cmd).await;
    assert_eq!(res["ok"], true, "{res}");
    secret
}

// ─── the settings files ──────────────────────────────────────────────────────

#[tokio::test]
async fn spawn_writes_private_files_that_keep_the_secret_out_of_every_command() {
    let d = daemon().await;
    let secret = live(&d, "t-files", 30).await;
    assert_eq!(secret.len(), 64, "256 bits, hex");

    let dir = d.data.join(DIR_NAME);
    let settings = dir.join(terminal_file_name("t-files"));
    let header = dir.join(header_file_name("t-files"));
    assert_eq!(mode(&dir), 0o700, "the directory is closed to other users");
    assert_eq!(mode(&settings), 0o600);
    assert_eq!(mode(&header), 0o600);

    let doc = std::fs::read_to_string(&settings).unwrap();
    assert!(
        !doc.contains(&secret),
        "the settings file must not hold the secret"
    );
    // The wildcard bind is reached on loopback, and every command is
    // attributed to this terminal and reads the header file.
    assert!(doc.contains("http://127.0.0.1:45871/term-hooks/event?terminal=t-files"));
    assert!(doc.contains("http://127.0.0.1:45871/term-hooks/statusline?terminal=t-files"));
    assert_eq!(
        doc.matches("curl ").count(),
        doc.matches(&format!("@{}", header.display())).count()
    );
    let parsed: Value = serde_json::from_str(&doc).unwrap();
    assert_eq!(parsed["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"], 40);
}

#[tokio::test]
async fn info_names_the_dir_or_says_why_not() {
    let d = daemon().await;
    let info = ok(&d.router, "term_hooks_info", json!({})).await;
    assert_eq!(
        info["settingsDir"],
        d.data.join(DIR_NAME).to_string_lossy().as_ref()
    );
    assert!(info["reason"].is_null());

    // No data dir: nowhere to keep the files.
    let none = daemon_with(false, PORT).await;
    let info = ok(&none.router, "term_hooks_info", json!({})).await;
    assert!(info["settingsDir"].is_null());
    assert!(
        info["reason"]
            .as_str()
            .unwrap()
            .starts_with("Not available on this server"),
        "{info}"
    );

    // A port the server has not bound yet.
    let unbound = daemon_with(true, 0).await;
    let info = ok(&unbound.router, "term_hooks_info", json!({})).await;
    assert!(info["settingsDir"].is_null(), "{info}");
}

#[tokio::test]
async fn pty_spawn_refuses_a_settings_path_it_would_not_have_chosen() {
    let d = daemon().await;
    for (term, path, why) in [
        ("t-a", "/etc/cron.d/evil".to_string(), "not this terminal's"),
        (
            "t-a",
            settings_path(&d, "t-b"),
            "another terminal's file name",
        ),
        (
            "../up",
            d.data
                .join(DIR_NAME)
                .join("claude-hooks-../up.json")
                .to_string_lossy()
                .into_owned(),
            "a traversing terminal id",
        ),
    ] {
        let res = rpc(
            &d.router,
            "pty_spawn",
            json!({ "terminalId": term, "cwd": "/", "cmd": sleeper(1), "settingsPath": path }),
        )
        .await;
        assert_eq!(res["ok"], false, "{why}: {res}");
        assert!(
            res["error"].as_str().unwrap().starts_with("pty_spawn: "),
            "{why}: {res}"
        );
    }
    assert!(
        !d.data.join(DIR_NAME).exists()
            || std::fs::read_dir(d.data.join(DIR_NAME)).unwrap().count() == 0,
        "nothing was written for a refused path"
    );
    // Without a data dir the spawn fails with the reason, not a dead claude.
    let none = daemon_with(false, PORT).await;
    let res = rpc(
        &none.router,
        "pty_spawn",
        json!({ "terminalId": "t", "cwd": "/", "cmd": sleeper(1), "settingsPath": "/x/claude-hooks-t.json" }),
    )
    .await;
    assert_eq!(res["ok"], false);
    assert!(res["error"]
        .as_str()
        .unwrap()
        .contains("Not available on this server"));
}

// ─── the credential ──────────────────────────────────────────────────────────

#[tokio::test]
async fn the_endpoint_refuses_every_wrong_credential_alike() {
    let d = daemon().await;
    let a = live(&d, "t-a", 30).await;
    let b = live(&d, "t-b", 30).await;
    let body = json!({ "hook_event_name": "Stop" });

    // No header, no terminal, wrong secret, the daemon's own token, an
    // unknown terminal, and ANOTHER terminal's secret: all one 401.
    let cases: Vec<(&str, Option<&str>, Option<&str>)> = vec![
        ("no header", Some("t-a"), None),
        ("no terminal", None, Some(&a)),
        ("wrong secret", Some("t-a"), Some("0".repeat(64).leak())),
        ("the daemon token", Some("t-a"), Some(TOKEN)),
        ("unknown terminal", Some("t-zzz"), Some(&a)),
        ("another terminal's secret", Some("t-a"), Some(&b)),
    ];
    let mut bodies = Vec::new();
    for (why, term, secret) in cases {
        for path in [EVENT_PATH, STATUSLINE_PATH] {
            let (status, bytes) = post(&d.router, path, term, secret, body.clone()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{why} {path}");
            bodies.push(bytes);
        }
    }
    assert!(
        bodies.windows(2).all(|w| w[0] == w[1]),
        "the refusals must not tell the caller why"
    );

    let (status, _) = post_json(&d.router, EVENT_PATH, "t-a", &a, body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post_json(&d.router, EVENT_PATH, "t-b", &b, body).await;
    assert_eq!(status, StatusCode::OK);
}

/// Two principals' children are two routers: a secret is known only to the
/// one that minted it, even when both have a terminal with the same id.
#[tokio::test]
async fn another_principals_secret_opens_nothing_and_its_events_stay_home() {
    let alice = daemon().await;
    let bob = daemon().await;
    let a = live(&alice, "t-1", 30).await;
    let b = live(&bob, "t-1", 30).await;
    assert_ne!(a, b);

    let (status, _) = post_json(
        &bob.router,
        STATUSLINE_PATH,
        "t-1",
        &a,
        json!({ "model": { "id": "x" } }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "alice's secret on bob's endpoint"
    );
    let (status, _) = post_json(&alice.router, STATUSLINE_PATH, "t-1", &b, json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "bob's secret on alice's endpoint"
    );

    // Each one's own statusline reaches only its own bus.
    let mut alice_ws = connect(alice.addr).await;
    let mut bob_ws = connect(bob.addr).await;
    let (status, _) = post_json(
        &alice.router,
        STATUSLINE_PATH,
        "t-1",
        &a,
        json!({ "cost": { "total_cost_usd": 1.5 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = event(&mut alice_ws, "statusline://snapshot").await;
    assert_eq!(got["cost"]["total_cost_usd"], 1.5);
    no_event(&mut bob_ws, "statusline://snapshot").await;
    assert_eq!(
        ok(&bob.router, "term_hooks_statusline_snapshot", json!({})).await,
        json!({}),
        "bob's HUD has nothing of alice's"
    );
}

#[tokio::test]
async fn a_secret_dies_with_its_terminal_and_a_respawn_keeps_its_own() {
    let d = daemon().await;
    let old = live(&d, "t-x", 1).await;
    let (status, _) = post_json(&d.router, EVENT_PATH, "t-x", &old, json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // The PTY exits after a second: the secret stops working, the files go.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, _) = post_json(&d.router, EVENT_PATH, "t-x", &old, json!({})).await;
        if status == StatusCode::UNAUTHORIZED {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "secret outlived its PTY"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!d
        .data
        .join(DIR_NAME)
        .join(terminal_file_name("t-x"))
        .exists());
    assert!(!d.data.join(DIR_NAME).join(header_file_name("t-x")).exists());

    // Respawned under the same id: the first PTY's exit must not revoke the
    // second's newer secret.
    let first = live(&d, "t-y", 1).await;
    let second = live(&d, "t-y", 30).await;
    assert_ne!(first, second);
    let (status, _) = post_json(&d.router, EVENT_PATH, "t-y", &first, json!({})).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the old secret was replaced"
    );
    tokio::time::sleep(Duration::from_millis(1800)).await; // first PTY exits
    let (status, _) = post_json(&d.router, EVENT_PATH, "t-y", &second, json!({})).await;
    assert_eq!(status, StatusCode::OK, "the live terminal kept its hooks");
    assert!(d
        .data
        .join(DIR_NAME)
        .join(terminal_file_name("t-y"))
        .exists());
}

// ─── statusline ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_statusline_posts_a_snapshot_the_hud_can_read_back() {
    let d = daemon().await;
    let secret = live(&d, "t-hud", 30).await;
    let mut ws = connect(d.addr).await;

    // The body claims another terminal; the authenticated one wins.
    let (status, bytes) = post(
        &d.router,
        STATUSLINE_PATH,
        Some("t-hud"),
        Some(&secret),
        json!({
            "ikenga_terminal_id": "someone-else",
            "session_id": "s1",
            "model": { "id": "claude-x", "display_name": "Claude X" },
            "cost": { "total_cost_usd": 0.42 },
            "context_window": { "used_percentage": 12.5 },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.is_empty(), "stdout is the status text claude draws");

    let got = event(&mut ws, "statusline://snapshot").await;
    assert_eq!(got["ikenga_terminal_id"], "t-hud");
    assert_eq!(got["model"]["display_name"], "Claude X");
    assert_eq!(got["cost"]["total_cost_usd"], 0.42);
    assert_eq!(got["context_window"]["used_percentage"], 12.5);

    let snaps = ok(&d.router, "term_hooks_statusline_snapshot", json!({})).await;
    assert_eq!(snaps["t-hud"]["session_id"], "s1");

    let (status, _) = post(
        &d.router,
        STATUSLINE_PATH,
        Some("t-hud"),
        Some(&secret),
        json!([1]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a snapshot is an object");
}

// ─── hook events ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn only_the_inbox_lifecycle_is_published_and_without_tool_output() {
    let d = daemon().await;
    let secret = live(&d, "t-ev", 30).await;
    let mut ws = connect(d.addr).await;

    // Not the inbox's: accepted, answered, never published.
    for name in ["SessionStart", "PreCompact", "PreToolUse"] {
        let (status, body) = post_json(
            &d.router,
            EVENT_PATH,
            "t-ev",
            &secret,
            json!({ "hook_event_name": name, "tool_name": "Bash" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "continue": true }), "{name}");
    }
    no_event(&mut ws, "hooks://event").await;

    // A native Claude prompt, in the desktop's shape.
    post_json(
        &d.router,
        EVENT_PATH,
        "t-ev",
        &secret,
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "tool_name": "Bash",
            "tool_input": { "command": "ls" },
            "prompt": "Run ls?",
            "transcript_path": "/not/forwarded",
        }),
    )
    .await;
    let p = event(&mut ws, "hooks://event").await;
    assert_eq!(p["ikenga_terminal_id"], "t-ev");
    assert_eq!(p["hook_event_name"], "PermissionRequest");
    assert_eq!(p["tool_name"], "Bash");
    assert_eq!(p["tool_input"], json!({ "command": "ls" }));
    assert_eq!(p["prompt"], "Run ls?");
    assert!(p.get("request_id").is_none() && p.get("held").is_none());
    assert!(p.get("transcript_path").is_none());

    // What resolves it: the tool's output and a prompt's text stay behind.
    post_json(
        &d.router,
        EVENT_PATH,
        "t-ev",
        &secret,
        json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "ls" },
            "tool_use_id": "tu1",
            "tool_output": { "stdout": "SECRET FILE LISTING" },
            "tool_response": { "stdout": "SECRET FILE LISTING" },
        }),
    )
    .await;
    let p = event(&mut ws, "hooks://event").await;
    assert_eq!(p["hook_event_name"], "PostToolUse");
    assert_eq!(p["tool_use_id"], "tu1");
    assert!(!p.to_string().contains("SECRET FILE LISTING"), "{p}");

    post_json(
        &d.router,
        EVENT_PATH,
        "t-ev",
        &secret,
        json!({ "hook_event_name": "UserPromptSubmit", "prompt": "my private prompt" }),
    )
    .await;
    let p = event(&mut ws, "hooks://event").await;
    assert_eq!(p["hook_event_name"], "UserPromptSubmit");
    assert!(p["prompt"].is_null(), "{p}");
}

// ─── the permission gate ─────────────────────────────────────────────────────

async fn hold_on(d: &Daemon, term: &str) {
    ok(
        &d.router,
        "settings_set",
        json!({ "key": format!("permissions.hold_terminal_{term}"), "value": "true" }),
    )
    .await;
}

/// POST a `PreToolUse` and return the join handle of its (parked) response.
fn gated_call(
    d: &Daemon,
    term: &str,
    secret: &str,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let (router, term, secret) = (d.router.clone(), term.to_string(), secret.to_string());
    tokio::spawn(async move {
        post_json(
            &router,
            EVENT_PATH,
            &term,
            &secret,
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "command": "rm -rf /tmp/x" },
                "tool_use_id": "tu-gate",
            }),
        )
        .await
    })
}

#[tokio::test]
async fn an_approved_gate_allows_once_and_a_replayed_answer_takes_nothing() {
    let d = daemon().await;
    let secret = live(&d, "t-gate", 30).await;
    hold_on(&d, "t-gate").await;
    let mut ws = connect(d.addr).await;

    let call = gated_call(&d, "t-gate", &secret);
    let held = event(&mut ws, "hooks://event").await;
    assert_eq!(held["hook_event_name"], "PreToolUse");
    assert_eq!(held["held"], true);
    assert_eq!(held["ikenga_terminal_id"], "t-gate");
    assert_eq!(held["tool_input"], json!({ "command": "rm -rf /tmp/x" }));
    let request_id = held["request_id"].as_str().unwrap().to_string();
    assert!(request_id.starts_with("perm-") && request_id.len() > 20);

    let res = ok(
        &d.router,
        "term_hooks_decide",
        json!({ "requestId": request_id, "decision": "approved" }),
    )
    .await;
    assert_eq!(res, json!({ "recorded": true, "gated": true }));
    let decided = event(&mut ws, "hooks://decision").await;
    assert_eq!(
        decided,
        json!({ "requestId": request_id, "decision": "approved" })
    );

    let (status, body) = call.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(body["request_id"], request_id);
    assert_eq!(body["gated"], true);
    assert_eq!(body["continue"], true);

    // Single use: the same id again takes nothing, whichever way it points.
    for decision in ["approved", "denied"] {
        let res = ok(
            &d.router,
            "term_hooks_decide",
            json!({ "requestId": request_id, "decision": decision }),
        )
        .await;
        assert_eq!(
            res,
            json!({ "recorded": true, "gated": false }),
            "{decision}"
        );
    }
    no_event(&mut ws, "hooks://decision").await;
}

#[tokio::test]
async fn a_denied_gate_blocks_the_tool_not_the_session() {
    let d = daemon().await;
    let secret = live(&d, "t-deny", 30).await;
    hold_on(&d, "t-deny").await;
    let mut ws = connect(d.addr).await;

    let call = gated_call(&d, "t-deny", &secret);
    let held = event(&mut ws, "hooks://event").await;
    ok(
        &d.router,
        "term_hooks_decide",
        json!({ "requestId": held["request_id"], "decision": "denied" }),
    )
    .await;
    let (_, body) = call.await.unwrap();
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        body["continue"], true,
        "continue:false would end the session"
    );
}

#[tokio::test]
async fn an_unanswered_gate_times_out_to_the_desktops_deny() {
    TEST_HOLD.with(|h| h.set(Some(Duration::from_millis(600))));
    let d = daemon().await;
    TEST_HOLD.with(|h| h.set(None));
    let secret = live(&d, "t-late", 30).await;
    hold_on(&d, "t-late").await;
    let mut ws = connect(d.addr).await;

    let call = gated_call(&d, "t-late", &secret);
    let held = event(&mut ws, "hooks://event").await;
    let request_id = held["request_id"].as_str().unwrap().to_string();

    let (status, body) = call.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        body["hookSpecificOutput"]["permissionDecisionReason"],
        "Denied in the Ikenga permission inbox (or the request timed out)."
    );
    // The inbox is told it is over, as the desktop's synthetic timeout does.
    let decided = event(&mut ws, "hooks://decision").await;
    assert_eq!(
        decided,
        json!({ "requestId": request_id, "decision": "denied" })
    );

    // And a late "approve" cannot resurrect it.
    let res = ok(
        &d.router,
        "term_hooks_decide",
        json!({ "requestId": request_id, "decision": "approved" }),
    )
    .await;
    assert_eq!(res["gated"], false);
}

#[tokio::test]
async fn a_hook_that_hangs_up_is_denied_and_cannot_be_answered() {
    let d = daemon().await;
    let secret = live(&d, "t-hup", 30).await;
    hold_on(&d, "t-hup").await;
    let mut ws = connect(d.addr).await;

    let call = gated_call(&d, "t-hup", &secret);
    let held = event(&mut ws, "hooks://event").await;
    let request_id = held["request_id"].as_str().unwrap().to_string();
    call.abort(); // curl gave up / claude died: the response future is dropped
    let _ = call.await;

    let decided = event(&mut ws, "hooks://decision").await;
    assert_eq!(decided["decision"], "denied");
    let res = ok(
        &d.router,
        "term_hooks_decide",
        json!({ "requestId": request_id, "decision": "approved" }),
    )
    .await;
    assert_eq!(res["gated"], false, "nothing is parked any more");
}

#[tokio::test]
async fn a_terminal_that_dies_with_an_ask_parked_denies_it() {
    let d = daemon().await;
    let cmd = sleeper(60);
    let res = rpc(
        &d.router,
        "pty_spawn",
        json!({
            "terminalId": "t-die", "cwd": "/", "cmd": cmd,
            "settingsPath": settings_path(&d, "t-die"),
        }),
    )
    .await;
    let pty_id = res["data"]["pty_id"].as_str().unwrap().to_string();
    let secret = secret_of(&d, "t-die");
    hold_on(&d, "t-die").await;
    let mut ws = connect(d.addr).await;

    let call = gated_call(&d, "t-die", &secret);
    let held = event(&mut ws, "hooks://event").await;
    ok(&d.router, "pty_kill", json!({ "id": pty_id })).await;

    let decided = event(&mut ws, "hooks://decision").await;
    assert_eq!(
        decided,
        json!({ "requestId": held["request_id"], "decision": "denied" })
    );
    let (_, body) = call.await.unwrap();
    assert_eq!(body["hookSpecificOutput"]["permissionDecision"], "deny");
}

#[tokio::test]
async fn the_hold_is_opt_in_per_terminal() {
    let d = daemon().await;
    let on = live(&d, "t-on", 30).await;
    let off = live(&d, "t-off", 30).await;
    hold_on(&d, "t-on").await;

    // The terminal without the setting is never parked.
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(3),
        post_json(
            &d.router,
            EVENT_PATH,
            "t-off",
            &off,
            json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash" }),
        ),
    )
    .await
    .expect("an ungated PreToolUse answers at once");
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "continue": true }));

    // And one terminal's setting does not gate another.
    let call = gated_call(&d, "t-on", &on);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!call.is_finished(), "the opted-in terminal is parked");
    call.abort();
}

#[tokio::test]
async fn the_decision_arm_is_strict_about_what_it_accepts() {
    let d = daemon().await;
    for (args, why) in [
        (
            json!({ "requestId": "perm-x", "decision": "allow" }),
            "decision spelling",
        ),
        (json!({ "requestId": "perm-x" }), "no decision"),
        (json!({ "decision": "approved" }), "no request id"),
        (
            json!({ "requestId": "", "decision": "approved" }),
            "empty request id",
        ),
    ] {
        let res = rpc(&d.router, "term_hooks_decide", args).await;
        assert_eq!(res["ok"], false, "{why}: {res}");
    }
    // An id nothing is parked under is a no-op, never an invented decision.
    let mut ws = connect(d.addr).await;
    let res = ok(
        &d.router,
        "term_hooks_decide",
        json!({ "requestId": "perm-nothing", "decision": "approved" }),
    )
    .await;
    assert_eq!(res["gated"], false);
    no_event(&mut ws, "hooks://decision").await;
}

#[test]
fn the_arms_carry_the_requirements_the_gate_depends_on() {
    use crate::access::rpc_requirements::requirement;
    use crate::access::{ArmClass, Cap};
    let decide = requirement("term_hooks_decide");
    assert_eq!(
        decide.class,
        ArmClass::Owner,
        "never reachable through a share"
    );
    assert!(
        decide.caps.contains(Cap::Approve),
        "routing withholds approve"
    );
    for arm in ["term_hooks_info", "term_hooks_statusline_snapshot"] {
        let r = requirement(arm);
        assert_eq!(r.class, ArmClass::Owner, "{arm}");
        assert!(r.caps.contains(Cap::Sessions), "{arm}");
    }
}

// ─── the ask's notification row and audit trail (daemon asks) ───────────────
//
// A held gate is the desktop's `permission` row, answerable through the shared
// decide core, resolved by every way it can end, and audited where its tier
// audits. T0 audits into its access store; a T1 principal child queues for the
// broker, whose `accept` is run here against a real store.

use crate::access::audit::child as audit_child;
use crate::access::{AccessStore, DaemonAccess};
use crate::executor::PrincipalId;
use crate::server::shared::notifications as notes;

const OWN_CAPS: &str = "files,sessions,dispatch,approve,install,settings,secrets";

/// One caller of a daemon: the T0 operator bearer, or what a T1 broker relays
/// to a principal child (the per-child token, `X-Ikenga-Principal`, caps and,
/// for a share member, the `X-Ikenga-Share-*` set).
#[derive(Clone)]
struct Caller {
    router: Router,
    headers: Vec<(&'static str, String)>,
}

impl Caller {
    async fn raw(&self, cmd: &str, args: Value) -> Value {
        let mut req = Request::builder()
            .method("POST")
            .uri("/api/rpc")
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json");
        for (k, v) in &self.headers {
            req = req.header(*k, v);
        }
        let res = self
            .router
            .clone()
            .oneshot(
                req.body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn ok(&self, cmd: &str, args: Value) -> Value {
        let res = self.raw(cmd, args.clone()).await;
        assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    /// The error text of a refused call.
    async fn err(&self, cmd: &str, args: Value) -> String {
        let res = self.raw(cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should be refused: {res}");
        res["error"].as_str().unwrap_or_default().to_string()
    }

    /// Every `permission` row this caller can see.
    async fn permission_rows(&self) -> Vec<Value> {
        self.ok("notifications_list", json!({ "kinds": ["permission"] }))
            .await
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

struct Asks {
    d: Daemon,
    db: Arc<PaDb>,
    /// The T0 daemon's access store (`None` for a principal child).
    store: Option<AccessStore>,
    /// A principal child's audit queue (what its broker would drain).
    outbox: Option<Arc<audit_child::Outbox>>,
    operator: Caller,
}

async fn asks_on(
    access: Arc<DaemonAccess>,
    store: Option<AccessStore>,
    outbox: Option<Arc<audit_child::Outbox>>,
    path_guard: Option<crate::server::rpc_shell::PathGuard>,
) -> Asks {
    crate::server::hook_asks::TEST_OUTBOX.with(|t| *t.borrow_mut() = outbox.clone());
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home) = (root.join("data"), root.join("home"));
    for d in [&data, &home] {
        std::fs::create_dir_all(d).unwrap();
    }
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    db.ensure_pool().await.unwrap();
    let router = crate::server::build_router(
        config(Some(data.clone()), PORT),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db.clone()),
        None,
        Some(home),
        path_guard.unwrap_or_else(crate::server::rpc_shell::PathGuard::allowlist),
        None,
        access,
        None,
        crate::server::UpdateSource::Default,
    );
    crate::server::hook_asks::TEST_OUTBOX.with(|t| *t.borrow_mut() = None);
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
    let d = Daemon {
        _tmp: tmp,
        data,
        router: router.clone(),
        addr,
    };
    Asks {
        operator: Caller {
            router,
            headers: vec![],
        },
        d,
        db,
        store,
        outbox,
    }
}

/// A T0 daemon with an access store, so decisions have a chain to land in.
async fn t0() -> Asks {
    let store = AccessStore::memory_t0().await;
    asks_on(
        DaemonAccess::with_store(store.clone()),
        Some(store),
        None,
        None,
    )
    .await
}

/// A T1 principal child (no store of its own), and the principal it serves.
async fn child() -> (Asks, PrincipalId) {
    child_with(None).await
}

async fn child_with(
    path_guard: Option<crate::server::rpc_shell::PathGuard>,
) -> (Asks, PrincipalId) {
    child_on(Arc::new(audit_child::Outbox::new()), path_guard).await
}

async fn child_on(
    outbox: Arc<audit_child::Outbox>,
    path_guard: Option<crate::server::rpc_shell::PathGuard>,
) -> (Asks, PrincipalId) {
    let a = asks_on(
        DaemonAccess::principal_child(Default::default()),
        None,
        Some(outbox),
        path_guard,
    )
    .await;
    (a, PrincipalId::new_v7())
}

impl Asks {
    /// The principal's own request as the broker relays it.
    fn principal(&self, id: PrincipalId) -> Caller {
        Caller {
            router: self.d.router.clone(),
            headers: vec![
                ("x-ikenga-principal", id.to_string()),
                ("x-ikenga-caps", OWN_CAPS.into()),
            ],
        }
    }

    /// A share member's request, relayed into the owner's child.
    fn member(&self, owner: PrincipalId, member: PrincipalId, project: &str) -> Caller {
        Caller {
            router: self.d.router.clone(),
            headers: vec![
                ("x-ikenga-principal", owner.to_string()),
                ("x-ikenga-caps", "files,sessions,dispatch,approve".into()),
                ("x-ikenga-share-project", project.into()),
                ("x-ikenga-share-principal", member.to_string()),
                ("x-ikenga-share-device", "-".into()),
                ("x-ikenga-share-role", "operator".into()),
            ],
        }
    }

    /// `audit_events` rows of the T0 store: `(kind, principal, via, target, detail)`.
    async fn audit(&self) -> Vec<(String, Option<String>, String, Option<String>, Value)> {
        let rows: Vec<(String, Option<String>, String, Option<String>, String)> = sqlx::query_as(
            "SELECT kind, principal_id, via, target, detail FROM audit_events \
                 WHERE category = 'permission' ORDER BY seq",
        )
        .fetch_all(self.store.as_ref().expect("a T0 store").pool())
        .await
        .unwrap();
        rows.into_iter()
            .map(|(k, p, v, t, d)| (k, p, v, t, serde_json::from_str(&d).unwrap()))
            .collect()
    }
}

/// Poll until `f` yields a value (5 s).
async fn until<T, Fut: std::future::Future<Output = Option<T>>>(mut f: impl FnMut() -> Fut) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition not met within 5 s"
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// A gate that is parked, with its request id and a live bus socket.
struct Parked {
    request_id: String,
    call: tokio::task::JoinHandle<(StatusCode, Value)>,
    /// Held open so the bus keeps a subscriber for the ask's whole life.
    _ws: Client,
}

async fn park_on(
    a: &Asks,
    caller: &Caller,
    term: &str,
    secs: u32,
    tool: &str,
    cwd: Option<&str>,
) -> Parked {
    park_with(
        a,
        caller,
        term,
        secs,
        tool,
        json!({ "command": "rm -rf /tmp/x" }),
        cwd,
    )
    .await
}

async fn park_with(
    a: &Asks,
    caller: &Caller,
    term: &str,
    secs: u32,
    tool: &str,
    input: Value,
    cwd: Option<&str>,
) -> Parked {
    // The terminal is spawned as `caller` (a child only takes what its broker
    // relays), then its hook secret is read from the header file.
    let cmd = sleeper(secs);
    let spawned = caller
        .ok(
            "pty_spawn",
            json!({
                "terminalId": term,
                "cwd": "/",
                "cmd": cmd,
                "rows": 24,
                "cols": 80,
                "settingsPath": settings_path(&a.d, term),
            }),
        )
        .await;
    assert!(spawned["pty_id"].is_string(), "{spawned}");
    let secret = secret_of(&a.d, term);
    caller
        .ok(
            "settings_set",
            json!({ "key": format!("permissions.hold_terminal_{term}"), "value": "true" }),
        )
        .await;
    let mut ws = connect_as(a.d.addr, &caller.headers).await;
    let (router, term_s, secret_s, tool_s, cwd_s) = (
        a.d.router.clone(),
        term.to_string(),
        secret,
        tool.to_string(),
        cwd.map(str::to_string),
    );
    let call = tokio::spawn(async move {
        post_json(
            &router,
            EVENT_PATH,
            &term_s,
            &secret_s,
            json!({
                "hook_event_name": "PreToolUse",
                "tool_name": tool_s,
                "tool_input": input,
                "tool_use_id": "tu-gate",
                "cwd": cwd_s,
            }),
        )
        .await
    });
    let held = event(&mut ws, "hooks://event").await;
    Parked {
        request_id: held["request_id"].as_str().unwrap().to_string(),
        call,
        _ws: ws,
    }
}

/// The open row for `request_id` as `caller` sees it.
async fn row_of(caller: &Caller, request_id: &str) -> Value {
    let key = format!("permission:hook:{request_id}");
    until(|| {
        let key = key.clone();
        async move {
            caller
                .permission_rows()
                .await
                .into_iter()
                .find(|r| r["dedupeKey"] == key)
        }
    })
    .await
}

async fn row_resolved(caller: &Caller, request_id: &str) -> Value {
    let key = format!("permission:hook:{request_id}");
    until(|| {
        let key = key.clone();
        async move {
            caller
                .permission_rows()
                .await
                .into_iter()
                .find(|r| r["dedupeKey"] == key && !r["resolvedAt"].is_null())
        }
    })
    .await
}

fn decision_of(body: &Value) -> &str {
    body["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
}

#[tokio::test]
async fn a_held_gate_records_the_desktops_row_and_announces_it() {
    let a = t0().await;
    let mut changes = notes::subscribe();
    let p = park_on(&a, &a.operator, "t-row", 30, "Bash", Some("/")).await;

    let row = row_of(&a.operator, &p.request_id).await;
    assert_eq!(row["kind"], "permission");
    assert_eq!(row["title"], "Claude wants to use Bash");
    assert_eq!(row["source"], "iyke.hooks", "the desktop's own source");
    assert_eq!(row["action"]["kind"], "permission.decide");
    assert_eq!(row["action"]["via"], "hooks");
    assert_eq!(row["action"]["requestId"], p.request_id);
    assert_eq!(row["action"]["terminalId"], "t-row");
    assert!(row["resolvedAt"].is_null(), "pending while held");
    assert_eq!(
        row["can_decide"], true,
        "the read model offers Allow / Deny"
    );
    // Bash is shell exec: sensitive (§5.3 rule 1), so a member could not
    // answer it under owner-approval; here the Owner can.
    assert_eq!(row["action"]["routing"]["sensitive"], 1);
    let expires = row["action"]["expiresAtMs"].as_i64().unwrap();
    let now = chrono::Utc::now().timestamp_millis();
    assert!(
        expires > now && expires <= now + 31_000,
        "{expires} vs {now}"
    );

    // Exactly one row for the one ask, and the bell's count includes it.
    assert_eq!(a.operator.permission_rows().await.len(), 1);
    let count = a.operator.ok("notifications_unread_count", json!({})).await;
    assert_eq!(count["pendingPermissions"], 1);

    // `notifications://changed` carried it on the process channel (the event
    // bus relays that to /ws/events), and Web Push's producer turns that very
    // event into a permission push whose TTL is the hold.
    let created = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), changes.recv())
            .await
            .expect("a created event")
            .unwrap();
        if ev
            .notification
            .as_ref()
            .and_then(|n| n.dedupe_key.as_deref())
            == Some(&format!("permission:hook:{}", p.request_id))
        {
            break ev;
        }
    };
    let push = crate::server::push::events::from_event(&created, now).expect("a permission push");
    assert_eq!(push.kind, crate::server::push::PushKind::Permission);
    assert_eq!(push.r, format!("n:{}", row["id"]));
    assert!((1..=30).contains(&push.ttl()), "ttl {}", push.ttl());

    p.call.abort();
}

#[tokio::test]
async fn the_row_reaches_a_browsers_event_socket() {
    let a = t0().await;
    let mut ws = {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{}/ws/events", a.d.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
        next_json(&mut ws, Duration::from_secs(5)).await.unwrap();
        ws.send(tungstenite::Message::Text(
            json!({
                "type": "subscribe",
                "events": ["notifications://changed", "hooks://event"],
            })
            .to_string(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        ws
    };
    let secret = live(&a.d, "t-ws", 30).await;
    a.operator
        .ok(
            "settings_set",
            json!({ "key": "permissions.hold_terminal_t-ws", "value": "true" }),
        )
        .await;
    let call = gated_call(&a.d, "t-ws", &secret);
    let held = event(&mut ws, "hooks://event").await;
    let key = format!("permission:hook:{}", held["request_id"].as_str().unwrap());
    // The created row arrives as a `notifications://changed` frame.
    let created = loop {
        let p = event(&mut ws, "notifications://changed").await;
        if p["notification"]["dedupeKey"] == key {
            break p;
        }
    };
    assert_eq!(created["reason"], "created");
    assert_eq!(created["notification"]["kind"], "permission");
    call.abort();
}

#[tokio::test]
async fn answering_through_the_row_resolves_the_held_hook_exactly_once() {
    let a = t0().await;
    let p = park_on(&a, &a.operator, "t-bell", 30, "Bash", Some("/")).await;
    let row = row_of(&a.operator, &p.request_id).await;
    let id = row["id"].as_i64().unwrap();

    let res = a
        .operator
        .ok(
            "permission_decide",
            json!({ "notificationId": id, "decision": "allow_once" }),
        )
        .await;
    assert_eq!(res, json!({ "resolved": true }));

    let (status, body) = p.call.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(decision_of(&body), "allow", "the SAME held request");
    assert_eq!(body["request_id"], p.request_id);

    // The row flipped, attributed to the operator on this host.
    let done = row_resolved(&a.operator, &p.request_id).await;
    assert!(done["can_decide"] == false || done["can_decide"].is_null());
    let (by, via): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT decided_by, decided_via FROM shell_notifications WHERE id = ?")
            .bind(id)
            .fetch_one(&a.db.ensure_pool().await.unwrap())
            .await
            .unwrap();
    assert_eq!(via.as_deref(), Some("operator"));
    assert!(by.is_some());

    // Single use, through every door: the row (conflict), the legacy arm
    // (gated:false), and a deny after an allow.
    for decision in ["allow_once", "deny"] {
        let e = a
            .operator
            .err(
                "permission_decide",
                json!({ "notificationId": id, "decision": decision }),
            )
            .await;
        assert!(e.contains("conflict"), "{e}");
    }
    for decision in ["approved", "denied"] {
        let r = a
            .operator
            .ok(
                "term_hooks_decide",
                json!({ "requestId": p.request_id, "decision": decision }),
            )
            .await;
        assert_eq!(r["gated"], false, "{decision}");
    }

    // One decision, one chain row, with the facts the desktop records.
    let audit = a.audit().await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    let (kind, principal, via, target, detail) = &audit[0];
    assert_eq!(kind, "permission.decided");
    assert_eq!(via, "operator");
    assert!(principal.is_some());
    assert_eq!(target.as_deref(), Some("Claude wants to use Bash"));
    assert_eq!(detail["decision"], "allow_once");
    assert_eq!(detail["sensitive"], 1);
}

#[tokio::test]
async fn a_deny_through_the_row_blocks_the_tool_and_is_audited() {
    let a = t0().await;
    let p = park_on(&a, &a.operator, "t-nope", 30, "Bash", Some("/")).await;
    let id = row_of(&a.operator, &p.request_id).await["id"]
        .as_i64()
        .unwrap();
    a.operator
        .ok(
            "permission_decide",
            json!({ "notificationId": id, "decision": "deny" }),
        )
        .await;
    let (_, body) = p.call.await.unwrap();
    assert_eq!(decision_of(&body), "deny");
    assert_eq!(body["continue"], true, "the session goes on");
    let audit = a.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].4["decision"], "deny");
    // The hook gate has no "always for this project" (§5.5).
    let p2 = park_on(&a, &a.operator, "t-nope2", 30, "Bash", Some("/")).await;
    let id2 = row_of(&a.operator, &p2.request_id).await["id"]
        .as_i64()
        .unwrap();
    let e = a
        .operator
        .err(
            "permission_decide",
            json!({ "notificationId": id2, "decision": "allow_always_project" }),
        )
        .await;
    assert!(e.contains("invalid_request"), "{e}");
    assert!(
        !p2.call.is_finished(),
        "a refused decision leaves the ask held"
    );
    p2.call.abort();
}

#[tokio::test]
async fn the_legacy_arm_answers_through_the_row_when_there_is_one() {
    let a = t0().await;
    let p = park_on(&a, &a.operator, "t-arm", 30, "Bash", Some("/")).await;
    row_of(&a.operator, &p.request_id).await;
    let r = a
        .operator
        .ok(
            "term_hooks_decide",
            json!({ "requestId": p.request_id, "decision": "approved" }),
        )
        .await;
    assert_eq!(r, json!({ "recorded": true, "gated": true }));
    let (_, body) = p.call.await.unwrap();
    assert_eq!(decision_of(&body), "allow");
    let done = row_resolved(&a.operator, &p.request_id).await;
    assert!(!done["resolvedAt"].is_null());
    let audit = a.audit().await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0].0, "permission.decided");
    assert_eq!(audit[0].4["decision"], "allow_once");
}

#[tokio::test]
async fn a_timeout_resolves_the_row_denies_and_is_audited() {
    TEST_HOLD.with(|h| h.set(Some(Duration::from_millis(700))));
    let a = t0().await;
    TEST_HOLD.with(|h| h.set(None));
    let p = park_on(&a, &a.operator, "t-slow", 30, "Bash", Some("/")).await;
    let id = row_of(&a.operator, &p.request_id).await["id"]
        .as_i64()
        .unwrap();

    let (_, body) = p.call.await.unwrap();
    assert_eq!(decision_of(&body), "deny");
    row_resolved(&a.operator, &p.request_id).await;

    // A late answer, through either door, takes nothing.
    let e = a
        .operator
        .err(
            "permission_decide",
            json!({ "notificationId": id, "decision": "allow_once" }),
        )
        .await;
    assert!(e.contains("conflict"), "{e}");
    let r = a
        .operator
        .ok(
            "term_hooks_decide",
            json!({ "requestId": p.request_id, "decision": "approved" }),
        )
        .await;
    assert_eq!(r["gated"], false);

    // The deny the gate gave on its own is on the chain, as the system's.
    let audit = until(|| async {
        let rows = a.audit().await;
        (!rows.is_empty()).then_some(rows)
    })
    .await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    let (kind, principal, via, target, detail) = &audit[0];
    assert_eq!(kind, "permission.decided");
    assert_eq!(via, "system");
    assert!(principal.is_some());
    assert_eq!(target.as_deref(), Some("Claude wants to use Bash"));
    assert_eq!(detail["decision"], "deny");
    assert_eq!(detail["outcome"], "timed_out");
}

#[tokio::test]
async fn a_terminal_that_dies_resolves_its_row_and_is_audited() {
    let a = t0().await;
    // A 2 s shell: the PTY exits with the ask still parked.
    let p = park_on(&a, &a.operator, "t-gone", 2, "Bash", Some("/")).await;
    row_of(&a.operator, &p.request_id).await;
    let row = row_resolved(&a.operator, &p.request_id).await;
    assert!(!row["resolvedAt"].is_null());
    // Nothing is left looking pending.
    assert_eq!(
        a.operator.ok("notifications_unread_count", json!({})).await["pendingPermissions"],
        0
    );
    let audit = until(|| async {
        let rows = a.audit().await;
        (!rows.is_empty()).then_some(rows)
    })
    .await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0].0, "permission.refused");
    assert_eq!(audit[0].4["reason"], "hook_disconnected");
    assert_eq!(audit[0].4["outcome"], "terminal_ended");
    let e = a
        .operator
        .ok(
            "term_hooks_decide",
            json!({ "requestId": p.request_id, "decision": "approved" }),
        )
        .await;
    assert_eq!(e["gated"], false);
}

#[tokio::test]
async fn a_hook_that_hangs_up_resolves_its_row_and_is_audited() {
    let a = t0().await;
    let p = park_on(&a, &a.operator, "t-hang", 30, "Bash", Some("/")).await;
    row_of(&a.operator, &p.request_id).await;
    p.call.abort();
    let _ = p.call.await;
    row_resolved(&a.operator, &p.request_id).await;
    let audit = until(|| async {
        let rows = a.audit().await;
        (!rows.is_empty()).then_some(rows)
    })
    .await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert_eq!(audit[0].0, "permission.refused");
    assert_eq!(audit[0].4["outcome"], "hook_disconnected");
}

/// An ask that ends before its row is even written must not leave the row
/// pending: the end waits for the write, then flips it.
#[tokio::test]
async fn an_ask_that_ends_instantly_never_leaves_a_pending_row() {
    let a = t0().await;
    let secret = live(&a.d, "t-fast", 30).await;
    a.operator
        .ok(
            "settings_set",
            json!({ "key": "permissions.hold_terminal_t-fast", "value": "true" }),
        )
        .await;
    let mut ws = connect(a.d.addr).await;
    for _ in 0..6 {
        let call = gated_call(&a.d, "t-fast", &secret);
        let held = event(&mut ws, "hooks://event").await;
        // Answered through the legacy arm the moment it is parked — possibly
        // before the row exists, so the arm cannot find it to claim.
        a.operator
            .raw(
                "term_hooks_decide",
                json!({ "requestId": held["request_id"], "decision": "approved" }),
            )
            .await;
        let _ = call.await;
    }
    until(|| async {
        let c = a.operator.ok("notifications_unread_count", json!({})).await;
        (c["pendingPermissions"] == 0).then_some(())
    })
    .await;
    let rows = a.operator.permission_rows().await;
    assert!(rows.iter().all(|r| !r["resolvedAt"].is_null()), "{rows:?}");
}

/// A daemon that crashed with an ask held leaves a row nothing can answer.
#[tokio::test]
async fn a_row_left_open_by_an_earlier_run_is_closed_at_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().canonicalize().unwrap().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let pool = db.ensure_pool().await.unwrap();
    let stale = notes::hook_ask::permission_from_hook_gate(
        Some("Bash"),
        None,
        Some("t-old"),
        None,
        "perm-from-a-dead-run",
    );
    notes::record(&pool, stale).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _router = crate::server::build_router(
        config(Some(data.clone()), PORT),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db.clone()),
        None,
        None,
        crate::server::rpc_shell::PathGuard::allowlist(),
        None,
        DaemonAccess::unavailable(),
        None,
        crate::server::UpdateSource::Default,
    );
    until(|| async {
        let open: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM shell_notifications WHERE kind = 'permission' AND resolved_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        (open == 0).then_some(())
    })
    .await;
}

// ─── principals ──────────────────────────────────────────────────────────────

/// Two principals' children are two databases and two held tables: B sees
/// nothing of A's ask, B's `permission_decide` on A's row id lands on B's OWN
/// row of that number, and B's `term_hooks_decide` on A's request id takes
/// nothing — A's hook stays held until A answers it.
#[tokio::test]
async fn principal_b_cannot_see_or_answer_principal_as_ask() {
    let (alice, alice_id) = child().await;
    let (bob, bob_id) = child().await;
    let (a, b) = (alice.principal(alice_id), bob.principal(bob_id));
    let pa = park_on(&alice, &a, "t-same", 30, "Bash", Some("/")).await;
    let pb = park_on(&bob, &b, "t-same", 30, "Bash", Some("/")).await;
    let row_a = row_of(&a, &pa.request_id).await;
    let row_b = row_of(&b, &pb.request_id).await;
    assert_eq!(
        row_a["id"], row_b["id"],
        "both are row 1 of their own database: an id is no capability"
    );

    // Neither list holds the other's row.
    assert!(b
        .permission_rows()
        .await
        .iter()
        .all(|r| r["action"]["requestId"] != pa.request_id));
    assert!(a
        .permission_rows()
        .await
        .iter()
        .all(|r| r["action"]["requestId"] != pb.request_id));

    // B names A's request id: nothing is held under it in B's child.
    let r = b
        .ok(
            "term_hooks_decide",
            json!({ "requestId": pa.request_id, "decision": "approved" }),
        )
        .await;
    assert_eq!(r["gated"], false);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pa.call.is_finished(), "A's hook is still held");
    assert!(!pb.call.is_finished(), "and B's");

    // B's decide on "row 1" answers B's own ask, never A's.
    b.ok(
        "permission_decide",
        json!({ "notificationId": row_a["id"], "decision": "allow_once" }),
    )
    .await;
    let (_, body_b) = pb.call.await.unwrap();
    assert_eq!(decision_of(&body_b), "allow");
    assert!(!pa.call.is_finished(), "still A's to answer");

    // A answers its own.
    a.ok(
        "permission_decide",
        json!({ "notificationId": row_a["id"], "decision": "deny" }),
    )
    .await;
    let (_, body_a) = pa.call.await.unwrap();
    assert_eq!(decision_of(&body_a), "deny");
}

/// A share member sees and answers only asks of the shared project, and what
/// the member decides is attributed to the member.
#[tokio::test]
async fn a_share_member_answers_only_the_shared_projects_asks() {
    // The shared projects live under one allowlisted root.
    let root = tempfile::tempdir().unwrap();
    let work = root.path().canonicalize().unwrap().join("work");
    let (site, other) = (work.join("site"), work.join("other"));
    for dir in [&site, &other] {
        std::fs::create_dir_all(dir.join("src")).unwrap();
    }
    let roots = crate::fs_roots::FsRoots::load_seeded(
        root.path().join("fs_roots.json"),
        vec![work.to_string_lossy().into_owned()],
    )
    .unwrap();
    let guard = crate::server::rpc_shell::PathGuard::roots(Arc::new(roots));
    let (a, owner) = child_with(Some(guard)).await;
    let owner_c = a.principal(owner);
    let pool = a.db.ensure_pool().await.unwrap();
    for (id, dir) in [("site", &site), ("other", &other)] {
        sqlx::query(
            "INSERT INTO projects (id, display_name, root_path, created_at) VALUES (?, ?, ?, 0)",
        )
        .bind(id)
        .bind(id)
        .bind(dir.to_string_lossy().as_ref())
        .execute(&pool)
        .await
        .unwrap();
    }

    let in_site = site.join("src").to_string_lossy().into_owned();
    let in_other = other.join("src").to_string_lossy().into_owned();
    // Reads inside their own project: not sensitive (§5.3), so a member may
    // answer them under any policy.
    let read = |dir: &std::path::Path| json!({ "file_path": dir.join("src/a.rs") });
    let p_site = park_with(
        &a,
        &owner_c,
        "t-site",
        30,
        "Read",
        read(&site),
        Some(&in_site),
    )
    .await;
    let p_other = park_with(
        &a,
        &owner_c,
        "t-other",
        30,
        "Read",
        read(&other),
        Some(&in_other),
    )
    .await;
    let row_site = row_of(&owner_c, &p_site.request_id).await;
    let row_other = row_of(&owner_c, &p_other.request_id).await;
    assert_eq!(row_site["action"]["routing"]["projectId"], "site");
    assert_eq!(row_other["action"]["routing"]["projectId"], "other");

    let member_id = PrincipalId::new_v7();
    let member = a.member(owner, member_id, "site");
    // The member's list is the shared project's asks, no others.
    let seen = member.permission_rows().await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["action"]["requestId"], p_site.request_id);
    // The other project's ask does not exist for the member.
    let e = member
        .err(
            "permission_decide",
            json!({ "notificationId": row_other["id"], "decision": "allow_once" }),
        )
        .await;
    assert!(e.contains("not_found"), "{e}");
    assert!(!p_other.call.is_finished(), "left held");

    // The shared project's is theirs to answer.
    member
        .ok(
            "permission_decide",
            json!({ "notificationId": row_site["id"], "decision": "allow_once" }),
        )
        .await;
    let (_, body) = p_site.call.await.unwrap();
    assert_eq!(decision_of(&body), "allow");
    // What the member decided is the member's, on the note the broker will
    // chain — not the Owner's.
    let notes_out = a
        .outbox
        .as_ref()
        .unwrap()
        .take(Duration::from_millis(200))
        .await
        .notes;
    let note = notes_out
        .iter()
        .find(|n| n.kind == Some(audit_child::NoteKind::Decided))
        .unwrap_or_else(|| panic!("no decided note in {notes_out:?}"));
    assert_eq!(
        note.by.as_ref().and_then(|b| b.principal_id.clone()),
        Some(member_id.to_string())
    );
    assert_eq!(
        note.project_key.as_deref(),
        Some(format!("{owner}/site").as_str())
    );
    // And the legacy arm is the Owner's alone: a member is refused outright.
    let e = member
        .err(
            "term_hooks_decide",
            json!({ "requestId": p_other.request_id, "decision": "approved" }),
        )
        .await;
    assert!(e.contains("owner"), "{e}");
    assert!(!p_other.call.is_finished());
    p_other.call.abort();
}

/// Under T1 the chain row is written by the broker from the child's note: run
/// the child's real queue through `accept` against a real store, and check it
/// is the same row T0 writes for the same decision.
#[tokio::test]
async fn a_child_decision_reaches_the_chain_through_the_brokers_accept() {
    let tool = format!("Probe{}", uuid::Uuid::new_v4().simple());
    let (a, owner) = child().await;
    let outbox = a.outbox.clone().unwrap();
    let c = a.principal(owner);
    let p = park_on(&a, &c, "t-aud", 30, &tool, Some("/")).await;
    let id = row_of(&c, &p.request_id).await["id"].as_i64().unwrap();
    c.ok(
        "permission_decide",
        json!({ "notificationId": id, "decision": "allow_once" }),
    )
    .await;
    let _ = p.call.await.unwrap();

    let title = format!("Claude wants to use {tool}");
    let note = until(|| {
        let outbox = outbox.clone();
        let title = title.clone();
        async move {
            outbox
                .take(Duration::from_millis(50))
                .await
                .notes
                .into_iter()
                .find(|n| n.target.as_deref() == Some(title.as_str()))
        }
    })
    .await;
    assert_eq!(note.kind, Some(audit_child::NoteKind::Decided));
    assert_eq!(note.decision.as_deref(), Some("allow_once"));
    assert_eq!(
        note.by.as_ref().and_then(|b| b.principal_id.clone()),
        Some(owner.to_string()),
        "the credential the broker handed the child"
    );

    // The broker's side: validate, attribute to this child's principal, append.
    let store = AccessStore::memory_t0().await;
    let ev = audit_child::accept(&store, owner, &note)
        .await
        .expect("accepted");
    crate::server::shared::notifications::routing::append_audit(&store, &ev).await;
    let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT kind, principal_id, detail FROM audit_events WHERE category = 'permission'",
    )
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "permission.decided");
    assert_eq!(rows[0].1.as_deref(), Some(owner.to_string().as_str()));
    let detail: Value = serde_json::from_str(&rows[0].2).unwrap();

    // The same decision on T0 writes the same facts.
    let t = t0().await;
    let p0 = park_on(&t, &t.operator, "t-aud0", 30, &tool, Some("/")).await;
    let id0 = row_of(&t.operator, &p0.request_id).await["id"]
        .as_i64()
        .unwrap();
    t.operator
        .ok(
            "permission_decide",
            json!({ "notificationId": id0, "decision": "allow_once" }),
        )
        .await;
    let _ = p0.call.await.unwrap();
    assert_eq!(t.audit().await[0].4, detail, "one shape on both tiers");
}

#[tokio::test]
async fn a_child_timeout_is_a_note_for_the_broker_not_a_claimed_row() {
    TEST_HOLD.with(|h| h.set(Some(Duration::from_millis(500))));
    let (a, owner) = child().await;
    TEST_HOLD.with(|h| h.set(None));
    let outbox = a.outbox.clone().unwrap();
    let tool = format!("Slow{}", uuid::Uuid::new_v4().simple());
    let c = a.principal(owner);
    let p = park_on(&a, &c, "t-cto", 30, &tool, Some("/")).await;
    let (_, body) = p.call.await.unwrap();
    assert_eq!(decision_of(&body), "deny");
    row_resolved(&c, &p.request_id).await;

    let title = format!("Claude wants to use {tool}");
    let note = until(|| {
        let outbox = outbox.clone();
        let title = title.clone();
        async move {
            outbox
                .take(Duration::from_millis(50))
                .await
                .notes
                .into_iter()
                .find(|n| n.target.as_deref() == Some(title.as_str()))
        }
    })
    .await;
    assert_eq!(note.outcome.as_deref(), Some("timed_out"));
    assert_eq!(note.decision.as_deref(), Some("deny"));
    assert!(note.by.is_none(), "nobody decided it");
    let store = AccessStore::memory_t0().await;
    let ev = audit_child::accept(&store, owner, &note).await.unwrap();
    assert_eq!(ev.principal_id.as_deref(), Some(owner.to_string().as_str()));
    assert_eq!(ev.detail["outcome"], "timed_out");
}

#[tokio::test]
async fn a_full_held_table_denies_unparked_and_still_records_the_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
    db.ensure_pool().await.unwrap();
    let outbox = Arc::new(audit_child::Outbox::new());
    let asks =
        crate::server::hook_asks::HookAsks::with_outbox(db, PrincipalId::new_v7(), outbox.clone());
    asks.refused_unparked(
        Some("Bash"),
        Some(&json!({"command": "ls"})),
        "t-full",
        Some("/"),
    );
    let notes_out = until(|| {
        let outbox = outbox.clone();
        async move {
            let b = outbox.take(Duration::from_millis(50)).await;
            (!b.notes.is_empty()).then_some(b.notes)
        }
    })
    .await;
    assert_eq!(notes_out.len(), 1);
    assert_eq!(notes_out[0].outcome.as_deref(), Some("held_table_full"));
    assert_eq!(notes_out[0].decision.as_deref(), Some("deny"));
    assert_eq!(notes_out[0].kind, Some(audit_child::NoteKind::Decided));
}

/// The route the broker long-polls is `internal`: a principal's own relayed
/// request is refused, and only the broker's call (the internal-call header,
/// no caps) is served what the child queued.
#[tokio::test]
async fn the_brokers_audit_poll_is_internal_and_serves_the_childs_queue() {
    // The route reads the process-global outbox a principal child installs at
    // boot; only this test points a gate at it.
    audit_child::install_outbox(Arc::new(audit_child::Outbox::new()));
    let outbox = audit_child::outbox().unwrap();
    let (a, owner) = child_on(outbox, None).await;
    let c = a.principal(owner);
    let p = park_on(&a, &c, "t-poll", 30, "Bash", Some("/")).await;
    let id = row_of(&c, &p.request_id).await["id"].as_i64().unwrap();
    c.ok(
        "permission_decide",
        json!({ "notificationId": id, "decision": "deny" }),
    )
    .await;
    let _ = p.call.await;

    let get = |headers: &[(&str, &str)]| {
        let mut req = Request::builder()
            .method("GET")
            .uri(format!("{}?waitMs=0", audit_child::EVENTS_PATH))
            .header("authorization", format!("Bearer {TOKEN}"));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let router = a.d.router.clone();
        async move {
            let res = router
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, bytes)
        }
    };
    // A principal's request, relayed with its caps: not the broker.
    let owner_id = owner.to_string();
    let (status, _) = get(&[
        ("x-ikenga-principal", owner_id.as_str()),
        ("x-ikenga-caps", OWN_CAPS),
    ])
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Nor a bare bearer with neither.
    let (status, _) = get(&[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The broker's own call.
    let (status, bytes) = get(&[
        ("x-ikenga-principal", owner_id.as_str()),
        ("x-ikenga-internal-call", "1"),
    ])
    .await;
    assert_eq!(status, StatusCode::OK);
    let batch: Value = serde_json::from_slice(&bytes).unwrap();
    let notes = batch["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "{batch}");
    assert_eq!(notes[0]["kind"], "permission.decided");
    assert_eq!(notes[0]["decision"], "deny");
    assert_eq!(batch["dropped"], 0);
}
