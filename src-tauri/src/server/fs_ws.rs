//! `/ws/fs` — file-system watchers for a browser client.
//!
//! The desktop app delivers watcher events over Tauri's event bus, which does
//! not exist in a browser session, so this socket is the delivery channel for
//! `fsWatch` / `fsListenWatch` / `fsUnwatch` when the frontend is talking to
//! the daemon. `src/lib/transport/fs-socket.ts` is the client half; the two
//! are matched by hand, so a field renamed here must be renamed there.
//!
//! # Wire protocol
//!
//! Client → server (JSON text):
//! ```text
//! {"type":"watch",  "reqId":"<opaque>", "path":"<path>"}
//! {"type":"unwatch","watcherId":"<id>"}
//! ```
//! Server → client (JSON text):
//! ```text
//! {"type":"fs_ready","status":"watching","watching":true}
//! {"type":"watched","reqId":"<opaque>","watcherId":"<id>","path":"<canonical>"}
//! {"type":"error","reqId":"<opaque>|null","message":"…"}
//! {"type":"change","watcherId":"<id>","kind":"create|modify|remove","path":"<canonical>"}
//! ```
//!
//! # Lifetime
//!
//! The `FsWatchManager` is **per connection**, not shared in `AppState`. It
//! lives on this task's stack, so when the socket closes — cleanly, by client
//! disconnect, or because the pump died — it drops, and dropping it drops
//! every `Debouncer` it holds, which stops every watcher thread. A watcher
//! cannot outlive the client that asked for it, and a reconnecting client
//! re-issues its watches (which is what the TS client does).
//!
//! # Security
//!
//! `watch` resolves the requested path through the same allowlist the desktop
//! `fs_watch` command uses (via the router's `PathGuard`), and `fs_watch` then re-checks **every reported
//! event path** before it reaches this sink — see `crate::fs_watch`. Without
//! that second check a symlink inside a watched root would put out-of-allowlist
//! filenames and write timing on the wire.
//!
//! The daemon's own state is refused on top of the allowlist (see
//! `server::reserved`): a watch on the data dir, anything inside it, or the
//! discovery file is refused, and a watch on an ancestor of them drops every
//! event whose path is inside them — so neither the names nor the write
//! timing of `fs_roots.json`, `daemon.json`, `ikenga.db`, … reach the client.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::pty_ws::{close_message, closed, SocketAccess};
use super::rpc_shell::PathGuard;
use super::AppState;
use crate::access::{AccessCtx, DaemonAccess};
use crate::fs_watch::{FileChange, FsEventSink, FsWatchManager};

/// Cap on live watchers for one socket. Each one is an OS watch plus a
/// debouncer thread, and nothing in the UI needs more than a handful; the cap
/// exists so a buggy client in a reconnect loop cannot exhaust inotify
/// watches for the whole machine.
const MAX_WATCHERS_PER_CONNECTION: usize = 64;

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FsControlMessage {
    #[serde(rename_all = "camelCase")]
    Watch {
        /// Echoed back on the reply so the client can settle the right
        /// promise. Optional: a client that fires and forgets still works.
        #[serde(default)]
        req_id: Option<String>,
        path: String,
    },
    #[serde(rename_all = "camelCase")]
    Unwatch { watcher_id: String },
}

/// Ships one watcher's changes down one client's socket.
struct SocketSink {
    tx: mpsc::UnboundedSender<Message>,
    /// Drops events inside the daemon's own state. `None` only in unit tests
    /// of the frame shape.
    guard: Option<PathGuard>,
}

impl FsEventSink for SocketSink {
    fn emit(&self, watcher_id: &str, change: FileChange) {
        // `change.path` is canonical (`fs_watch` re-checks every event path
        // through `check_allowlisted`), so the reserved compare is exact.
        if self
            .guard
            .as_ref()
            .is_some_and(|g| g.is_reserved(std::path::Path::new(&change.path)))
        {
            return;
        }
        // Unbounded because this runs on the debouncer's thread, which must
        // not block; the pump task drains it. Bounded by the debounce window
        // in practice — one frame per changed path per 250 ms.
        let _ = self.tx.send(Message::Text(
            json!({
                "type": "change",
                "watcherId": watcher_id,
                "kind": change.kind,
                "path": change.path,
            })
            .to_string(),
        ));
    }
}

fn error_frame(req_id: Option<&str>, message: &str) -> Message {
    Message::Text(
        json!({
            "type": "error",
            "reqId": req_id,
            "message": message,
        })
        .to_string(),
    )
}

pub async fn fs_ws_handler(
    State(state): State<Arc<AppState>>,
    access: Option<Extension<Arc<DaemonAccess>>>,
    ctx: Option<Extension<AccessCtx>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // G-ACCESS §1.6: `files` to attach (checked at the handshake); watch
    // roots are confined under a share (`access::share::fs_watch_root`).
    let guard = SocketAccess::new(access.map(|Extension(a)| a), ctx.map(|Extension(c)| c));
    ws.on_upgrade(move |socket| super::activity::track_ws(handle_fs_socket(socket, state, guard)))
}

async fn handle_fs_socket(socket: WebSocket, state: Arc<AppState>, mut guard: SocketAccess) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let closed_fut = closed(guard.take_closed());
    tokio::pin!(closed_fut);
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    info!("FS watcher WebSocket client connected");

    // Single writer. Sinks fire from debouncer threads and the control loop
    // replies from this task; both go through the channel so nothing has to
    // share the split sink.
    let pump = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_tx.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Owned by this task — see the module docs on lifetime.
    let manager = FsWatchManager::new();
    let sink: Arc<dyn FsEventSink> = Arc::new(SocketSink {
        tx: out_tx.clone(),
        guard: Some(state.path_guard.clone()),
    });

    let _ = out_tx.send(Message::Text(
        json!({ "type": "fs_ready", "status": "watching", "watching": true }).to_string(),
    ));

    loop {
        let msg = tokio::select! {
            // G-ACCESS §3.10: revoked (4401) / caps changed (4403).
            close = &mut closed_fut => {
                let _ = out_tx.send(close_message(&close));
                break;
            }
            next = ws_rx.next() => match next {
                Some(Ok(msg)) => msg,
                _ => break,
            },
        };
        match msg {
            Message::Close(_) => break,
            Message::Text(text) => handle_control(
                &text,
                &state.path_guard,
                guard.ctx.as_ref(),
                &manager,
                &sink,
                &out_tx,
            ),
            // Binary/ping/pong carry nothing this socket understands. Axum
            // answers pings itself.
            _ => {}
        }
    }

    // Explicit for the reader: this is what stops every watcher this client
    // opened. `drop(out_tx)` then ends the pump once it has drained.
    drop(manager);
    drop(out_tx);
    let _ = pump.await;
    info!("FS watcher WebSocket client disconnected");
}

fn handle_control(
    raw: &str,
    guard: &PathGuard,
    ctx: Option<&AccessCtx>,
    manager: &FsWatchManager,
    sink: &Arc<dyn FsEventSink>,
    out: &mpsc::UnboundedSender<Message>,
) {
    let parsed: FsControlMessage = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            debug!("[fs_ws] undecodable control frame: {e}");
            let _ = out.send(error_frame(None, &format!("bad fs control frame: {e}")));
            return;
        }
    };

    match parsed {
        FsControlMessage::Watch { req_id, path } => {
            if manager.len() >= MAX_WATCHERS_PER_CONNECTION {
                let _ = out.send(error_frame(
                    req_id.as_deref(),
                    &format!(
                        "too many watchers on this connection (limit {MAX_WATCHERS_PER_CONNECTION}); \
                         unwatch something first"
                    ),
                ));
                return;
            }
            // G-ACCESS §4.5.4 share hook (WP-76 fills it): under a share a
            // watch root is confined to the project (or the one artifact).
            if let Some(Err(e)) = ctx.map(|c| crate::access::share::fs_watch_root(c, &path)) {
                let _ = out.send(error_frame(req_id.as_deref(), &e.to_string()));
                return;
            }
            // The router's guard: the process-global allowlist exactly as
            // `resolve_allowlisted` applies it, then the daemon's own state —
            // the allowlist may cover the data dir, a watch inside it may not.
            let resolved = match guard.resolve(&path) {
                Ok(p) => p,
                Err(e) => {
                    let _ = out.send(error_frame(req_id.as_deref(), &e));
                    return;
                }
            };
            match manager.watch_with_sink(&resolved, sink.clone()) {
                Ok(id) => {
                    let _ = out.send(Message::Text(
                        json!({
                            "type": "watched",
                            "reqId": req_id,
                            "watcherId": id,
                            "path": resolved.to_string_lossy(),
                        })
                        .to_string(),
                    ));
                }
                Err(e) => {
                    let _ = out.send(error_frame(
                        req_id.as_deref(),
                        &format!("watch failed: {e:#}"),
                    ));
                }
            }
        }
        FsControlMessage::Unwatch { watcher_id } => {
            // A client that unwatches an id twice (reconnect races) is not an
            // error worth a frame; the desired end state is reached either way.
            if let Err(e) = manager.unwatch(&watcher_id) {
                debug!("[fs_ws] unwatch: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_frames_decode_the_camelcase_the_client_sends() {
        let msg: FsControlMessage =
            serde_json::from_str(r#"{"type":"watch","reqId":"r1","path":"~/x"}"#).unwrap();
        match msg {
            FsControlMessage::Watch { req_id, path } => {
                assert_eq!(req_id.as_deref(), Some("r1"));
                assert_eq!(path, "~/x");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn req_id_is_optional() {
        let msg: FsControlMessage =
            serde_json::from_str(r#"{"type":"watch","path":"/tmp"}"#).unwrap();
        match msg {
            FsControlMessage::Watch { req_id, .. } => assert!(req_id.is_none()),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn unwatch_frames_decode_watcher_id() {
        let msg: FsControlMessage =
            serde_json::from_str(r#"{"type":"unwatch","watcherId":"abc"}"#).unwrap();
        match msg {
            FsControlMessage::Unwatch { watcher_id } => assert_eq!(watcher_id, "abc"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_frame_type_is_rejected_not_silently_ignored() {
        assert!(serde_json::from_str::<FsControlMessage>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<FsControlMessage>(r#"{"path":"/tmp"}"#).is_err());
    }

    /// The exact field names + casing `fs-socket.ts` destructures.
    #[test]
    fn a_change_frame_carries_the_shape_the_client_reads() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = SocketSink { tx, guard: None };
        sink.emit(
            "w-1",
            FileChange {
                kind: crate::fs_watch::ChangeKind::Modify,
                path: "/tmp/a.txt".into(),
            },
        );
        let Message::Text(raw) = rx.try_recv().unwrap() else {
            panic!("expected a text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["type"], "change");
        assert_eq!(v["watcherId"], "w-1");
        assert_eq!(v["kind"], "modify");
        assert_eq!(v["path"], "/tmp/a.txt");
        assert_eq!(v.as_object().unwrap().len(), 4);
    }

    #[test]
    fn an_error_frame_echoes_the_request_id() {
        let Message::Text(raw) = error_frame(Some("r7"), "nope") else {
            panic!("expected a text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["reqId"], "r7");
        assert_eq!(v["message"], "nope");

        let Message::Text(raw) = error_frame(None, "nope") else {
            panic!("expected a text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(v["reqId"].is_null());
    }

    /// End to end through a real watcher on a real directory: a write lands
    /// in the debouncer callback, which runs the per-event allowlist check.
    ///
    /// This test process installs no root set (`fs_roots` is a process-global
    /// `OnceLock`, so a test that installed one would pin it for the whole
    /// binary), which makes the expected outcome **nothing on the wire** —
    /// the fail-closed direction, and the half of the guard worth pinning
    /// down here. `path_allow::tests` covers the symlink-escape half against
    /// a locally-built `FsRoots`.
    #[test]
    fn with_no_allowlist_installed_no_event_reaches_the_wire() {
        if crate::fs_roots::current().is_some() {
            // Another test installed a root set; the premise no longer holds.
            return;
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink: Arc<dyn FsEventSink> = Arc::new(SocketSink { tx, guard: None });

        let manager = FsWatchManager::new();
        let id = manager.watch_with_sink(&dir, sink).unwrap();
        assert_eq!(manager.len(), 1);

        std::fs::write(dir.join("hello.txt"), b"hi").unwrap();
        // Comfortably past the 250 ms debounce window.
        std::thread::sleep(std::time::Duration::from_millis(1200));

        let mut frames = Vec::new();
        while let Ok(Message::Text(raw)) = rx.try_recv() {
            frames.push(raw);
        }
        assert!(
            frames.is_empty(),
            "with no fs_roots installed the per-event check must drop everything, got {frames:?}"
        );

        manager.unwatch(&id).unwrap();
        assert_eq!(manager.len(), 0);
    }

    /// A guard whose allowlist is `root/` and whose data dir is `root/data/`
    /// — the allowlist covers the data dir, which must still be refused.
    fn reserving_guard() -> (tempfile::TempDir, std::path::PathBuf, PathGuard) {
        let tmp = tempfile::TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join("data")).unwrap();
        let roots_file = base.join("roots.json");
        std::fs::write(
            &roots_file,
            json!({ "roots": [root.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        let guard = PathGuard::roots(Arc::new(roots)).reserving(
            crate::server::reserved::Reserved::new(Some(root.join("data")), vec![]),
        );
        (tmp, root, guard)
    }

    fn frames(rx: &mut mpsc::UnboundedReceiver<Message>) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(Message::Text(raw)) = rx.try_recv() {
            out.push(serde_json::from_str(&raw).unwrap());
        }
        out
    }

    #[test]
    fn a_watch_inside_the_data_dir_is_refused() {
        let (_tmp, root, guard) = reserving_guard();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink: Arc<dyn FsEventSink> = Arc::new(SocketSink {
            tx: tx.clone(),
            guard: Some(guard.clone()),
        });
        let manager = FsWatchManager::new();
        for path in [
            root.join("data"),
            root.join("data/fs_roots.json"),
            root.join("data/sub"),
        ] {
            let frame = json!({ "type": "watch", "reqId": "r", "path": path }).to_string();
            handle_control(&frame, &guard, None, &manager, &sink, &tx);
            let got = frames(&mut rx);
            assert_eq!(got.len(), 1, "{got:?}");
            assert_eq!(got[0]["type"], "error", "{}: {got:?}", path.display());
            let message = got[0]["message"].as_str().unwrap();
            assert!(
                message.contains(crate::server::reserved::INSIDE_DATA_DIR),
                "{message}"
            );
        }
        assert_eq!(manager.len(), 0, "no watcher may be opened");

        // The root around it is still watchable.
        let frame = json!({ "type": "watch", "reqId": "ok", "path": root }).to_string();
        handle_control(&frame, &guard, None, &manager, &sink, &tx);
        let got = frames(&mut rx);
        assert_eq!(got[0]["type"], "watched", "{got:?}");
        assert_eq!(manager.len(), 1);
    }

    /// A watch on an ancestor of the data dir must not relay the names or
    /// timing of its files.
    #[test]
    fn events_inside_the_data_dir_never_reach_the_wire() {
        let (_tmp, root, guard) = reserving_guard();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = SocketSink {
            tx,
            guard: Some(guard),
        };
        for inside in ["data", "data/fs_roots.json", "data/backups/new.db"] {
            sink.emit(
                "w",
                FileChange {
                    kind: crate::fs_watch::ChangeKind::Modify,
                    path: root.join(inside).to_string_lossy().into_owned(),
                },
            );
        }
        assert!(frames(&mut rx).is_empty());
        sink.emit(
            "w",
            FileChange {
                kind: crate::fs_watch::ChangeKind::Create,
                path: root.join("data-notes.txt").to_string_lossy().into_owned(),
            },
        );
        let got = frames(&mut rx);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(
            got[0]["path"],
            root.join("data-notes.txt").to_string_lossy().as_ref()
        );
    }
}
