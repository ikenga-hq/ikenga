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
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://{addr}/ws/events")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
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
