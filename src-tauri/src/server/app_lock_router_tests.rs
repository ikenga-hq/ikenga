//! WP-C (R4-6a) end to end on a real T0 router served on 127.0.0.1:0: a
//! locked account is enforced on every surface outside `/api/rpc` too.
//!
//! - pty: input sent while locked is dropped (never run after the unlock), a
//!   `?spawn=true` is refused, an attach gets no scrollback until the unlock,
//!   and an open socket pauses and is re-sent a snapshot on unlock;
//! - chat: a prompt sent while locked never runs;
//! - events: control frames are answered while locked (nothing parks the
//!   loop) and a socket opened behind the lock screen sees the unlock;
//! - fs: no events and no new watches while locked, and a new socket is
//!   refused;
//! - viewer and backup: refused while locked, including a mount made before.
//!
//! Same house pattern as `events_ws_tests`: a literal `ServerConfig`, temp
//! data dir and allowlist root, real WebSockets.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;
use tower::ServiceExt;

use crate::server::shared::app_lock::LOCKED_ERROR;

const TOKEN: &str = "app-lock-router-test-token";
const PIN: &str = "1234";

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Daemon {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    router: Router,
    addr: SocketAddr,
}

async fn daemon() -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let roots_file = root.join("roots.json");
    std::fs::write(
        &roots_file,
        json!({ "roots": [work.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let roots = Arc::new(crate::fs_roots::FsRoots::load(roots_file).unwrap());
    let config = crate::server::ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: root.join("no-spa-here"),
        pkgs_dir: None,
        data_dir: Some(root.join("data")),
        auth_token: Some(TOKEN.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: crate::executor::ExecutorTier::T0,
    };
    let router = crate::server::build_router(
        config,
        Arc::new(crate::pty::PtyManager::new()),
        Arc::new(crate::engines::EngineRegistry::new()),
        None,
        None,
        None,
        crate::server::rpc_shell::PathGuard::roots(roots),
        None,
        crate::access::DaemonAccess::unavailable(),
        None,
        crate::server::UpdateSource::Default,
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
        root: work,
        router,
        addr,
    }
}

impl Daemon {
    async fn rpc(&self, cmd: &str, args: Value) -> Value {
        let res = self
            .router
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
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(&self, uri: &str, bearer: bool) -> (StatusCode, String) {
        let mut b = Request::builder().method("GET").uri(uri);
        if bearer {
            b = b.header("authorization", format!("Bearer {TOKEN}"));
        }
        let res = self
            .router
            .clone()
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn set_pin(&self) {
        let res = self
            .rpc("app_lock_set_secret", json!({ "next": PIN }))
            .await;
        assert_eq!(res["ok"], true, "{res}");
    }

    async fn lock(&self) {
        let res = self.rpc("app_lock_lock", json!({})).await;
        assert_eq!(res["ok"], true, "{res}");
        assert_eq!(res["data"]["locked"], true, "{res}");
    }

    async fn unlock(&self) {
        let res = self.rpc("app_lock_unlock", json!({ "secret": PIN })).await;
        assert_eq!(res["data"]["ok"], true, "{res}");
        assert_eq!(res["data"]["status"]["locked"], false, "{res}");
    }

    async fn ws(&self, path: &str) -> Client {
        self.try_ws(path).await.unwrap()
    }

    async fn try_ws(&self, path: &str) -> Result<Client, tungstenite::Error> {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{}{path}", self.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {TOKEN}").parse().unwrap());
        tokio_tungstenite::connect_async(req)
            .await
            .map(|(ws, _)| ws)
    }
}

#[derive(Debug)]
enum Frame {
    Text(Value),
    Binary(Vec<u8>),
}

/// Every frame `ws` delivers within `within`.
async fn frames(ws: &mut Client, within: Duration) -> Vec<Frame> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(within, async {
        while let Some(Ok(msg)) = ws.next().await {
            match msg {
                tungstenite::Message::Text(t) => out.push(Frame::Text(
                    serde_json::from_str(&t).unwrap_or(Value::String(t)),
                )),
                tungstenite::Message::Binary(b) => out.push(Frame::Binary(b)),
                tungstenite::Message::Close(_) => break,
                _ => {}
            }
        }
    })
    .await;
    out
}

/// Frames until one satisfies `pred` (returned with everything before it),
/// or panics after `within`.
async fn until(
    ws: &mut Client,
    within: Duration,
    what: &str,
    pred: impl Fn(&Frame) -> bool,
) -> Vec<Frame> {
    let mut out = Vec::new();
    let found = tokio::time::timeout(within, async {
        while let Some(Ok(msg)) = ws.next().await {
            let frame = match msg {
                tungstenite::Message::Text(t) => {
                    Frame::Text(serde_json::from_str(&t).unwrap_or(Value::String(t)))
                }
                tungstenite::Message::Binary(b) => Frame::Binary(b),
                tungstenite::Message::Close(_) => return false,
                _ => continue,
            };
            let hit = pred(&frame);
            out.push(frame);
            if hit {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(found, "no {what} within {within:?}; got {out:?}");
    out
}

fn is_type(f: &Frame, ty: &str) -> bool {
    matches!(f, Frame::Text(v) if v["type"] == ty)
}

fn output(frames: &[Frame]) -> String {
    frames
        .iter()
        .filter_map(|f| match f {
            Frame::Binary(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        })
        .collect()
}

fn has_output(f: &Frame, needle: &str) -> bool {
    matches!(f, Frame::Binary(b) if String::from_utf8_lossy(b).contains(needle))
}

async fn wait_for_file(path: &Path, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    path.exists()
}

/// A fresh shell on `/ws/pty/<id>?spawn=true`, attached (its first snapshot
/// read) and answering commands.
#[cfg(unix)]
async fn shell(d: &Daemon, id: &str) -> Client {
    let mut ws = d.ws(&format!("/ws/pty/{id}?spawn=true")).await;
    until(&mut ws, Duration::from_secs(10), "first snapshot", |f| {
        is_type(f, "ikenga.snapshot")
    })
    .await;
    ws.send(tungstenite::Message::Binary(
        b"echo READY_$((40+2))\r".to_vec(),
    ))
    .await
    .unwrap();
    until(&mut ws, Duration::from_secs(15), "shell ready", |f| {
        has_output(f, "READY_42")
    })
    .await;
    ws
}

#[cfg(unix)]
async fn kill(mut ws: Client) {
    let _ = ws
        .send(tungstenite::Message::Text(
            json!({ "type": "kill" }).to_string(),
        ))
        .await;
    let _ = ws.close(None).await;
}

/// Round-5 new_blocking 1: keys typed on an open pty socket while the account
/// is locked are dropped — they never run, not during the lock and not after
/// the unlock — and the socket still works once unlocked.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pty_input_sent_while_locked_never_runs_after_unlock() {
    let d = daemon().await;
    d.set_pin().await;
    let mut ws = shell(&d, "lock-input").await;

    d.lock().await;
    until(&mut ws, Duration::from_secs(5), "ikenga.locked", |f| {
        is_type(f, "ikenga.locked")
    })
    .await;
    let typed = d.root.join("typed.txt");
    let written = d.root.join("written.txt");
    ws.send(tungstenite::Message::Binary(
        format!("echo x > {}\r", typed.display()).into_bytes(),
    ))
    .await
    .unwrap();
    ws.send(tungstenite::Message::Text(
        json!({ "type": "write", "data": format!("echo x > {}\r", written.display()) }).to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!typed.exists() && !written.exists(), "ran while locked");

    d.unlock().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !typed.exists(),
        "a key typed while locked ran after the unlock"
    );
    assert!(
        !written.exists(),
        "a write frame sent while locked ran after the unlock"
    );

    let after = d.root.join("after.txt");
    ws.send(tungstenite::Message::Binary(
        format!("echo x > {}\r", after.display()).into_bytes(),
    ))
    .await
    .unwrap();
    assert!(
        wait_for_file(&after, Duration::from_secs(10)).await,
        "input after the unlock must run"
    );
    kill(ws).await;
}

/// Round-5 new_blocking 3: `?spawn=true` while locked starts nothing, then or
/// after the unlock.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pty_spawn_is_refused_while_locked() {
    let d = daemon().await;
    d.set_pin().await;
    d.lock().await;

    let mut ws = d.ws("/ws/pty/lock-spawned?spawn=true").await;
    let got = until(
        &mut ws,
        Duration::from_secs(5),
        "a refusal",
        |f| matches!(f, Frame::Text(v) if v["type"] == "ikenga.error"),
    )
    .await;
    let Some(Frame::Text(refusal)) = got.last() else {
        unreachable!()
    };
    assert_eq!(refusal["code"], "locked", "{refusal}");
    assert_eq!(refusal["message"], LOCKED_ERROR);

    d.unlock().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let list = d.rpc("pty_terminal_list", json!({})).await;
    assert_eq!(list["ok"], true, "{list}");
    assert!(
        !list["data"].to_string().contains("lock-spawned"),
        "a shell was spawned for a locked request: {list}"
    );
}

/// Round-5 new_blocking 4: attaching while locked delivers no scrollback;
/// the unlock delivers the snapshot.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pty_attach_while_locked_gets_no_scrollback_until_unlock() {
    let d = daemon().await;
    d.set_pin().await;
    let mut first = shell(&d, "lock-attach").await;
    first
        .send(tungstenite::Message::Binary(
            b"echo BEFORE_LOCK_$((6*7))\r".to_vec(),
        ))
        .await
        .unwrap();
    until(&mut first, Duration::from_secs(10), "the marker", |f| {
        has_output(f, "BEFORE_LOCK_42")
    })
    .await;
    let _ = first.close(None).await;

    d.lock().await;
    let mut ws = d.ws("/ws/pty/lock-attach").await;
    let locked = frames(&mut ws, Duration::from_millis(800)).await;
    assert!(
        locked.iter().any(|f| is_type(f, "ikenga.locked")),
        "{locked:?}"
    );
    assert!(
        !locked
            .iter()
            .any(|f| is_type(f, "ikenga.snapshot") || matches!(f, Frame::Binary(_))),
        "scrollback delivered while locked: {locked:?}"
    );

    d.unlock().await;
    let got = until(&mut ws, Duration::from_secs(5), "the scrollback", |f| {
        has_output(f, "BEFORE_LOCK_42")
    })
    .await;
    assert!(got.iter().any(|f| is_type(f, "ikenga.snapshot")), "{got:?}");
    kill(ws).await;
}

/// Round-5 item 7: an open pty socket really pauses. A shell printing every
/// 100 ms delivers nothing while locked, and the unlock re-sends a snapshot
/// (a clean repaint) before live output resumes.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pty_open_socket_pauses_while_locked_and_resends_a_snapshot() {
    let d = daemon().await;
    d.set_pin().await;
    let mut ws = shell(&d, "lock-pause").await;
    ws.send(tungstenite::Message::Binary(
        b"while :; do echo TICK_$((1+1)); sleep 0.1; done\r".to_vec(),
    ))
    .await
    .unwrap();
    until(&mut ws, Duration::from_secs(10), "ticks", |f| {
        has_output(f, "TICK_2")
    })
    .await;

    d.lock().await;
    until(&mut ws, Duration::from_secs(5), "ikenga.locked", |f| {
        is_type(f, "ikenga.locked")
    })
    .await;
    let paused = frames(&mut ws, Duration::from_millis(800)).await;
    assert!(
        !paused.iter().any(|f| matches!(f, Frame::Binary(_))),
        "output delivered while locked: {}",
        output(&paused)
    );

    d.unlock().await;
    let resumed = until(&mut ws, Duration::from_secs(5), "a snapshot", |f| {
        is_type(f, "ikenga.snapshot")
    })
    .await;
    assert!(
        !resumed.iter().any(|f| matches!(f, Frame::Binary(_))),
        "the snapshot comes first: {resumed:?}"
    );
    until(&mut ws, Duration::from_secs(5), "live ticks", |f| {
        has_output(f, "TICK_2")
    })
    .await;
    let _ = ws.send(tungstenite::Message::Binary(vec![3])).await;
    kill(ws).await;
}

/// Round-5 new_blocking 1 (chat): a prompt sent while locked never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_prompt_sent_while_locked_never_runs_after_unlock() {
    let d = daemon().await;
    d.set_pin().await;
    let mut ws = d.ws("/ws/chat/lock-thread").await;
    let status = |f: &Frame, s: &str| {
        matches!(f, Frame::Text(v) if v["params"]["update"]["type"] == "status"
            && v["params"]["update"]["status"] == s)
    };
    until(&mut ws, Duration::from_secs(5), "idle", |f| {
        status(f, "idle")
    })
    .await;

    d.lock().await;
    ws.send(tungstenite::Message::Text(
        json!({ "type": "prompt", "prompt": "while locked" }).to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    d.unlock().await;
    let after = frames(&mut ws, Duration::from_millis(1500)).await;
    assert!(
        !after.iter().any(|f| status(f, "running")),
        "a prompt sent while locked ran after the unlock: {after:?}"
    );

    ws.send(tungstenite::Message::Text(
        json!({ "type": "prompt", "prompt": "after unlock" }).to_string(),
    ))
    .await
    .unwrap();
    until(&mut ws, Duration::from_secs(5), "running", |f| {
        status(f, "running")
    })
    .await;
}

/// The events socket never parks on the lock: a ping is answered while
/// locked, and a socket opened behind the lock screen can subscribe and sees
/// the unlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_socket_answers_while_locked_and_sees_the_unlock() {
    let d = daemon().await;
    d.set_pin().await;
    let subscribe = json!({ "type": "subscribe", "events": ["app-lock://changed"] }).to_string();

    let mut before = d.ws("/ws/events").await;
    until(&mut before, Duration::from_secs(5), "ready", |f| {
        is_type(f, "ready")
    })
    .await;
    before
        .send(tungstenite::Message::Text(subscribe.clone()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    d.lock().await;
    let is_change = |f: &Frame| matches!(f, Frame::Text(v) if v["event"] == "app-lock://changed");
    until(
        &mut before,
        Duration::from_secs(5),
        "the lock event",
        is_change,
    )
    .await;

    before
        .send(tungstenite::Message::Text(
            json!({ "type": "ping", "id": 7 }).to_string(),
        ))
        .await
        .unwrap();
    until(
        &mut before,
        Duration::from_secs(2),
        "pong while locked",
        |f| matches!(f, Frame::Text(v) if v["type"] == "pong" && v["id"] == 7),
    )
    .await;

    let mut during = d.ws("/ws/events").await;
    until(&mut during, Duration::from_secs(5), "ready", |f| {
        is_type(f, "ready")
    })
    .await;
    during
        .send(tungstenite::Message::Text(subscribe))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    d.unlock().await;
    until(
        &mut during,
        Duration::from_secs(5),
        "the unlock event",
        is_change,
    )
    .await;
    until(
        &mut before,
        Duration::from_secs(5),
        "the unlock event",
        is_change,
    )
    .await;
}

/// Round-5 new_blocking 5 (fs): while locked a new watch is refused (not
/// deferred) and a new socket is refused; after the unlock watches work
/// again. That a watch made before the lock streams nothing while locked is
/// `fs_ws::tests::a_locked_account_emits_no_change` (events need the
/// process-global allowlist, which a router test must not install) and the
/// live T0 run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fs_new_watches_and_sockets_are_refused_while_locked() {
    let d = daemon().await;
    d.set_pin().await;
    let mut ws = d.ws("/ws/fs").await;
    until(&mut ws, Duration::from_secs(5), "fs_ready", |f| {
        is_type(f, "fs_ready")
    })
    .await;
    let watch = |req: &str| {
        tungstenite::Message::Text(
            json!({ "type": "watch", "reqId": req, "path": d.root.to_string_lossy() }).to_string(),
        )
    };
    ws.send(watch("r1")).await.unwrap();
    until(&mut ws, Duration::from_secs(5), "watched", |f| {
        is_type(f, "watched")
    })
    .await;

    d.lock().await;
    ws.send(watch("r2")).await.unwrap();
    let refused = until(&mut ws, Duration::from_secs(5), "an error", |f| {
        is_type(f, "error")
    })
    .await;
    let Some(Frame::Text(err)) = refused.last() else {
        unreachable!()
    };
    assert_eq!(err["reqId"], "r2");
    assert_eq!(err["message"], LOCKED_ERROR);
    assert!(!refused.iter().any(|f| is_type(f, "watched")));

    match d.try_ws("/ws/fs").await {
        Err(tungstenite::Error::Http(res)) => assert_eq!(res.status().as_u16(), 423),
        Err(e) => panic!("expected a 423, got {e}"),
        Ok(_) => panic!("a new /ws/fs opened while locked"),
    }

    d.unlock().await;
    ws.send(watch("r3")).await.unwrap();
    until(
        &mut ws,
        Duration::from_secs(5),
        "watched after unlock",
        |f| matches!(f, Frame::Text(v) if v["type"] == "watched" && v["reqId"] == "r3"),
    )
    .await;
    let mut again = d.ws("/ws/fs").await;
    until(&mut again, Duration::from_secs(5), "fs_ready", |f| {
        is_type(f, "fs_ready")
    })
    .await;
}

/// Round-5 new_blocking 5 (viewer), and the whole-account backup export: a
/// mount made before the lock serves nothing while locked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewer_and_backup_are_refused_while_locked() {
    let d = daemon().await;
    d.set_pin().await;
    let site = d.root.join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("secret.txt"), "TOP_SECRET_CONTENTS").unwrap();
    let res = d
        .rpc(
            "viewer_serve",
            json!({
                "rootDir": site.to_string_lossy(),
                "filePath": site.join("secret.txt").to_string_lossy(),
            }),
        )
        .await;
    assert_eq!(res["ok"], true, "{res}");
    let token = res["data"]["token"].as_str().unwrap().to_string();
    let uri = format!("/__viewer/{token}/secret.txt");
    let (status, body) = d.get(&uri, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("TOP_SECRET_CONTENTS"));

    d.lock().await;
    for (uri, bearer) in [
        (uri.as_str(), false),
        (uri.as_str(), true),
        (&format!("/__viewer/{token}/") as &str, false),
        ("/api/backup/export", true),
    ] {
        let (status, body) = d.get(uri, bearer).await;
        assert_eq!(status, StatusCode::LOCKED, "{uri}: {body}");
        assert!(!body.contains("TOP_SECRET_CONTENTS"), "{uri}");
    }

    d.unlock().await;
    let (status, body) = d.get(&uri, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The gate is a default-deny list: only the named paths are exempt.
#[test]
fn the_lock_gates_every_protected_path_but_the_named_ones() {
    use crate::server::app_lock_gates_path as gated;
    for path in [
        "/__viewer/abc/x.txt",
        "/__viewer/abc",
        "/ws/fs",
        "/api/backup/export",
        "/api/backup/import",
        "/api/something-new",
    ] {
        assert!(gated(path), "{path}");
    }
    for path in [
        "/api/rpc",
        "/ws/pty/abc",
        "/ws/chat/abc",
        "/ws/events",
        "/pkgs/com.x/index.js",
        "/api/shutdown",
        "/api/server/update",
        "/api/server/update/apply",
    ] {
        assert!(!gated(path), "{path}");
    }
}

/// Round-5 non-blocking: the RPC exemptions are named exactly.
#[test]
fn rpc_lock_exemptions_are_exact() {
    use crate::server::rpc::is_app_lock_exempt_cmd as exempt;
    for cmd in ["app_lock_status", "app_lock_unlock", "server_health"] {
        assert!(exempt(cmd), "{cmd}");
    }
    for cmd in [
        "auth",
        "auth_status",
        "authorize",
        "app_lock_lock",
        "health",
        "app_lock_set_secret",
    ] {
        assert!(!exempt(cmd), "{cmd}");
    }
}
