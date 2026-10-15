use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tracing::{error, info, warn};

use super::AppState;
use crate::access::ws::{self as access_ws, Frame, Route};
use crate::access::{AccessCtx, DaemonAccess};
use crate::pty::SpawnOpts;
use crate::server::shared::app_lock::LOCKED_ERROR;

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PtyControlMessage {
    Resize { rows: u16, cols: u16 },
    Write { data: String },
    Kill,
}

#[derive(Deserialize, Debug, Default)]
pub struct PtyQuery {
    /// Create the session if the id resolves to nothing.
    ///
    /// Opt-in, because reconnect uses the same URL. Auto-spawning on every
    /// attach means a client reconnecting to a shell the user exited silently
    /// gets a **brand new shell** presented as the same session. Only the
    /// first attach of a terminal passes this.
    #[serde(default)]
    pub spawn: bool,
}

pub async fn pty_ws_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PtyQuery>,
    access: Option<Extension<Arc<DaemonAccess>>>,
    ctx: Option<Extension<AccessCtx>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // G-ACCESS §1.6: attaching needs `sessions` (checked at the handshake by
    // `auth_middleware`); input frames and `?spawn=true` need `dispatch`.
    let guard = SocketAccess::new(access.map(|Extension(a)| a), ctx.map(|Extension(c)| c))
        .with_target(format!("pty · {id}"));
    ws.on_upgrade(move |socket| {
        super::activity::track_ws(handle_pty_socket(socket, state, id, query, guard))
    })
}

/// The access side of one PTY socket (G-ACCESS §1.6, §3.10): the caps it
/// was opened with and its registration in the T0 socket registry, so a
/// revoke (4401) or tier change (4403) closes it at once.
pub(crate) struct SocketAccess {
    pub(crate) ctx: Option<AccessCtx>,
    pub(crate) registration: Option<crate::access::sockets::SocketGuard>,
    /// The store `dispatch.sent` is written to (G-ACCESS §6.5, WP-77).
    access: Option<Arc<DaemonAccess>>,
    /// The P-22 coalescing key and the row's `target`.
    socket_id: u64,
    target: String,
}

impl SocketAccess {
    pub(crate) fn new(access: Option<Arc<DaemonAccess>>, ctx: Option<AccessCtx>) -> Self {
        let registration = match (&access, &ctx) {
            (Some(a), Some(c)) => Some(a.sockets.register(c.device_id.clone().filter(|d| {
                // Only paired devices are revocable; the host row isn't.
                a.store()
                    .and_then(|s| s.meta().host_device_id.as_deref())
                    .map_or(true, |host| host != d)
            }))),
            _ => None,
        };
        Self {
            ctx,
            registration,
            access,
            socket_id: crate::access::audit::next_socket_id(),
            target: String::new(),
        }
    }

    /// What `dispatch.sent` names as this socket's target (§6.5).
    pub(crate) fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    /// `Ok` to deliver, or the refusal control frame to answer with. No
    /// context (a router built without the auth layer) refuses.
    pub(crate) fn check(&self, route: Route, frame: Frame<'_>) -> Result<(), String> {
        let caps = self
            .ctx
            .as_ref()
            .map(|c| c.caps)
            .unwrap_or(crate::access::CapSet::EMPTY);
        match access_ws::check_frame(caps, route, frame) {
            Ok(()) => {
                if let Some(ctx) = &self.ctx {
                    crate::access::audit::on_client_frame(
                        self.access.as_deref().and_then(DaemonAccess::store),
                        ctx,
                        self.socket_id,
                        &self.target,
                        route,
                        frame,
                    );
                }
                Ok(())
            }
            Err(missing) => Err(access_ws::refusal(missing)),
        }
    }

    pub(crate) fn may(&self, cap: crate::access::Cap) -> bool {
        self.ctx.as_ref().is_some_and(|c| c.caps.contains(cap))
    }

    /// The close signal (never fires when unregistered).
    pub(crate) fn take_closed(
        &mut self,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::access::sockets::Close>> {
        self.registration.as_mut().map(|r| {
            let (_tx, rx) = tokio::sync::oneshot::channel();
            std::mem::replace(&mut r.closed, rx)
        })
    }
}

/// Resolves when the registry says close (or never, without one).
pub(crate) async fn closed(
    rx: Option<tokio::sync::oneshot::Receiver<crate::access::sockets::Close>>,
) -> crate::access::sockets::Close {
    match rx {
        Some(rx) => match rx.await {
            Ok(close) => close,
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

pub(crate) fn close_message(close: &crate::access::sockets::Close) -> Message {
    Message::Close(Some(CloseFrame {
        code: close.code,
        reason: close.reason.into(),
    }))
}

/// Control frames are JSON text; terminal output is always binary. The client
/// distinguishes them by frame type, so a control frame can never be painted
/// into the terminal as if it were output.
fn control(kind: &str, extra: serde_json::Value) -> Message {
    let mut obj = serde_json::json!({ "type": kind });
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            obj[k] = v.clone();
        }
    }
    Message::Text(obj.to_string())
}

/// Sent to a pty socket while its account is locked (WP-C, R4-6a): output is
/// paused and input is dropped until an unlock, which re-sends a snapshot.
fn locked_frame() -> Message {
    control(
        "ikenga.locked",
        serde_json::json!({ "message": LOCKED_ERROR }),
    )
}

/// What [`send_snapshot`] did.
enum Snapshot {
    /// Sent; live output follows on this receiver.
    Sent(tokio::sync::broadcast::Receiver<Vec<u8>>),
    /// The account locked before anything went out. Nothing was sent.
    Locked,
    /// The session or the socket is gone; the caller stops.
    Gone,
}

/// Attach: announce the scrollback snapshot's absolute end offset, send the
/// bytes, then release the attach gate so live output follows.
///
/// Used for the first attach and again after every unlock, so a socket that
/// was paused while locked repaints from a clean snapshot instead of from
/// whatever survived in the broadcast channel. Nothing is sent while the
/// account is locked.
async fn send_snapshot(
    state: &AppState,
    pty_id: &str,
    label: &str,
    ws_tx: &mut futures_util::stream::SplitSink<WebSocket, Message>,
) -> Snapshot {
    if state.app_lock.is_locked() {
        return Snapshot::Locked;
    }
    let Some(snap) = state.pty_manager.attach_begin(pty_id) else {
        error!("Failed to begin attach for PTY {pty_id}");
        let _ = ws_tx
            .send(control("ikenga.gone", serde_json::json!({ "id": label })))
            .await;
        return Snapshot::Gone;
    };

    // Watchdog, mirroring `commands::pty::pty_attach_begin`. While the gate is
    // installed NOTHING is delivered to any consumer — desktop sink included —
    // so a client that stalls mid-handshake (a slow socket blocking the
    // snapshot write) would freeze the terminal for everyone until the hold cap
    // overflows. `attach_arm` is token-checked, so a late fire is a no-op.
    {
        let watchdog = state.pty_manager.clone();
        let watched_id = pty_id.to_string();
        let token = snap.token;
        tokio::spawn(async move {
            tokio::time::sleep(crate::pty::ATTACH_GATE_TIMEOUT).await;
            if watchdog.attach_arm(&watched_id, token) {
                warn!(pty = %watched_id, "ws attach never armed; watchdog released the gate");
            }
        });
    }

    let pty_rx = match state.pty_manager.subscribe(pty_id) {
        Ok(rx) => rx,
        Err(err) => {
            error!("Failed to subscribe to PTY {pty_id}: {err}");
            state.pty_manager.attach_arm(pty_id, snap.token);
            let _ = ws_tx
                .send(control(
                    "ikenga.error",
                    serde_json::json!({ "message": err.to_string() }),
                ))
                .await;
            return Snapshot::Gone;
        }
    };

    // The lock may have engaged while the attach began: send nothing.
    if state.app_lock.is_locked() {
        state.pty_manager.attach_arm(pty_id, snap.token);
        return Snapshot::Locked;
    }

    // Announce the snapshot's absolute end offset BEFORE the bytes.
    //
    // Without it a reconnecting client cannot tell which part of the replayed
    // scrollback it has already painted, and re-appends the whole buffer on
    // every reconnect. With it the client keeps its own cursor into the
    // stream and emits only the genuinely new tail — which is also what lets
    // an unlock re-send a snapshot on a socket that stayed open.
    let snapshot_len = snap.data.len();
    if ws_tx
        .send(control(
            "ikenga.snapshot",
            serde_json::json!({ "end_offset": snap.end_offset, "len": snapshot_len }),
        ))
        .await
        .is_err()
    {
        state.pty_manager.attach_arm(pty_id, snap.token);
        return Snapshot::Gone;
    }

    if snapshot_len > 0 {
        if let Err(e) = ws_tx.send(Message::Binary(snap.data)).await {
            error!("Failed to send scrollback snapshot: {e}");
            state.pty_manager.attach_arm(pty_id, snap.token);
            return Snapshot::Gone;
        }
    }

    // Release gate: held bytes (if any) flush to broadcast_tx and land on pty_rx.
    state.pty_manager.attach_arm(pty_id, snap.token);
    Snapshot::Sent(pty_rx)
}

async fn handle_pty_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    id: String,
    mut query: PtyQuery,
    mut guard: SocketAccess,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // `?spawn=true` creates a shell: `dispatch` (G-ACCESS §1.6). Without it
    // the attach proceeds as a plain (non-spawning) attach and the client is
    // told why.
    if query.spawn && !guard.may(crate::access::Cap::Dispatch) {
        query.spawn = false;
        let _ = ws_tx
            .send(Message::Text(access_ws::refusal(
                crate::access::CapSet::of(&[crate::access::Cap::Dispatch]),
            )))
            .await;
    }
    let closed_rx = guard.take_closed();
    let (refuse_tx, mut refuse_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // The path segment may be a pty id, a terminal id, or a label. Resolve it
    // once: `write` / `resize` / `kill` take the pty id only, so holding on to
    // the unresolved segment silently drops every keystroke.
    let pty_id = match state.pty_manager.resolve_id(&id) {
        Ok(resolved) => resolved,
        Err(e) if query.spawn => {
            // WP-C (R4-6a): a locked account starts nothing. Refused, not
            // deferred — a shell asked for behind the lock screen must not
            // appear when someone later unlocks.
            if state.app_lock.is_locked() {
                info!("PTY {id}: spawn refused, the account is locked");
                let _ = ws_tx
                    .send(control(
                        "ikenga.error",
                        serde_json::json!({ "message": LOCKED_ERROR, "code": "locked" }),
                    ))
                    .await;
                return;
            }
            warn!("PTY session not found for {id}: {e}. Auto-spawning (spawn=1).");
            let default_shell = if cfg!(windows) {
                vec!["powershell.exe".to_string()]
            } else {
                vec!["/bin/bash".to_string()]
            };
            match state
                .pty_manager
                .spawn_headless(SpawnOpts {
                    terminal_id: Some(id.clone()),
                    title: Some("Terminal".to_string()),
                    cwd: ".".to_string(),
                    cmd: default_shell,
                    env: std::collections::HashMap::new(),
                    rows: 24,
                    cols: 80,
                })
                .await
            {
                Ok(new_id) => new_id,
                Err(err) => {
                    error!("Failed to auto-spawn PTY for {id}: {err}");
                    let _ = ws_tx
                        .send(control(
                            "ikenga.error",
                            serde_json::json!({ "message": err.to_string() }),
                        ))
                        .await;
                    return;
                }
            }
        }
        Err(e) => {
            // No session and the client didn't ask for one: tell it the
            // terminal is gone so it stops reconnecting, rather than
            // manufacturing a replacement shell behind the user's back.
            info!("PTY session not found for {id}: {e}. Reporting gone (spawn not requested).");
            let _ = ws_tx
                .send(control("ikenga.gone", serde_json::json!({ "id": id })))
                .await;
            return;
        }
    };

    let pty_manager = state.pty_manager.clone();
    let session_id = pty_id.clone();
    let id_for_send = pty_id.clone();
    let label = id.clone();
    let send_state = state.clone();

    // Task 1: Pump PTY output -> WebSocket binary frames, and announce exit.
    //
    // Exit is awaited explicitly rather than inferred from the broadcast
    // channel closing. An exited session is RETAINED (with its scrollback) for
    // EXITED_RETENTION, so its sender is not dropped for another ten minutes —
    // a client keyed on `Closed` would spend that whole window reconnecting to
    // a shell that already finished. A bare socket close is likewise
    // indistinguishable from a dropped link, which is the ambiguity this frame
    // exists to remove.
    //
    // WP-C (R4-6a): while the account is locked this task sends nothing but
    // one `ikenga.locked` frame — no snapshot (an attach made while locked
    // gets no scrollback), no output, no exit. It drops its subscription, and
    // the unlock re-sends a snapshot, so a pane paused past the broadcast
    // capacity repaints cleanly instead of resuming mid-stream.
    let mut send_task = tokio::spawn(async move {
        let state = send_state;
        let exit_fut = state.pty_manager.wait_for_exit(&id_for_send);
        tokio::pin!(exit_fut);
        let closed_fut = closed(closed_rx);
        tokio::pin!(closed_fut);
        let mut lock_rx = state.app_lock.subscribe_lock();
        // `None` until a snapshot has gone out, and again while locked.
        let mut pty_rx: Option<tokio::sync::broadcast::Receiver<Vec<u8>>> = None;

        loop {
            if *lock_rx.borrow_and_update() {
                pty_rx = None;
                if ws_tx.send(locked_frame()).await.is_err() {
                    break;
                }
                // Revocation stays live while paused (§3.10).
                loop {
                    tokio::select! {
                        close = &mut closed_fut => {
                            let _ = ws_tx.send(close_message(&close)).await;
                            return;
                        }
                        changed = lock_rx.changed() => {
                            if changed.is_err() || !*lock_rx.borrow_and_update() {
                                break;
                            }
                        }
                        // Answers to input refused before the lock: dropped.
                        Some(_) = refuse_rx.recv() => {}
                    }
                }
                continue;
            }

            if pty_rx.is_none() {
                match send_snapshot(&state, &id_for_send, &label, &mut ws_tx).await {
                    Snapshot::Sent(rx) => pty_rx = Some(rx),
                    Snapshot::Locked => continue,
                    Snapshot::Gone => break,
                }
            }
            let Some(rx) = pty_rx.as_mut() else { break };

            tokio::select! {
                // Bias toward draining output: on exit there is usually a
                // final chunk still in flight, and losing the last line of a
                // command is the most visible way to get this wrong.
                biased;

                // G-ACCESS §3.10: revoked (4401) or caps changed (4403) —
                // close the socket; the PTY lives on.
                close = &mut closed_fut => {
                    let _ = ws_tx.send(close_message(&close)).await;
                    break;
                }

                // Locked: the top of the loop pauses.
                Ok(_) = lock_rx.changed() => {}

                // A refused input frame's control-frame answer.
                Some(text) = refuse_rx.recv() => {
                    if ws_tx.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }

                recv = rx.recv() => match recv {
                    Ok(bytes) => {
                        if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                            break;
                        }
                    }
                    // A burst past the channel's capacity means we dropped
                    // frames, not that the terminal ended — skipping ahead
                    // beats freezing the pane for the rest of the session.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("PTY {id_for_send}: websocket lagged, dropped {n} chunks");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },

                code = &mut exit_fut => {
                    // Flush whatever the shell emitted on its way out before
                    // announcing the exit.
                    while let Ok(bytes) = rx.try_recv() {
                        if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                            return;
                        }
                    }
                    let _ = ws_tx
                        .send(control(
                            "ikenga.exit",
                            serde_json::json!({ "id": id_for_send, "code": code }),
                        ))
                        .await;
                    break;
                }
            }
        }
    });

    // Task 2: Pump WebSocket input -> PTY stdin / control commands
    let recv_app_lock = state.app_lock.clone();
    let mut recv_task = tokio::spawn(async move {
        // Held for the socket's life: dropping it unregisters (§3.10).
        let guard = guard;
        let mut dropped_while_locked: u64 = 0;
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Close(_) = msg {
                break;
            }
            // WP-C (R4-6a): input that arrives while the account is locked is
            // dropped, never queued — nothing typed behind the lock screen
            // runs when someone later unlocks. This loop never waits on the
            // lock, so a frame is judged when it arrives, not after an unlock.
            if recv_app_lock.is_locked() {
                dropped_while_locked += 1;
                if dropped_while_locked.is_power_of_two() {
                    info!("PTY {session_id}: dropped {dropped_while_locked} input frame(s) while locked");
                }
                continue;
            }
            // G-ACCESS §1.6: every PTY input frame needs `dispatch`; a
            // refused one is dropped and answered, the socket stays open.
            let verdict = match &msg {
                Message::Binary(b) => guard.check(Route::Pty, Frame::Binary(b)),
                Message::Text(t) => guard.check(Route::Pty, Frame::Text(t)),
                _ => Ok(()),
            };
            if let Err(refusal) = verdict {
                let _ = refuse_tx.send(refusal);
                continue;
            }
            match msg {
                Message::Binary(bytes) => {
                    let _ = pty_manager.write(&session_id, &bytes);
                }
                Message::Text(text) => {
                    // Try parsing JSON control message (e.g. resize)
                    if let Ok(ctrl) = serde_json::from_str::<PtyControlMessage>(&text) {
                        match ctrl {
                            PtyControlMessage::Resize { rows, cols } => {
                                let _ = pty_manager.resize(&session_id, rows, cols);
                            }
                            PtyControlMessage::Write { data } => {
                                let _ = pty_manager.write(&session_id, data.as_bytes());
                            }
                            PtyControlMessage::Kill => {
                                let _ = pty_manager.kill(&session_id);
                                break;
                            }
                        }
                    } else {
                        // Raw text fallback
                        let _ = pty_manager.write(&session_id, text.as_bytes());
                    }
                }
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }

    info!("PTY WebSocket client disconnected for {id}");
}
