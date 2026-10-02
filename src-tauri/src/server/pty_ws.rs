use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tracing::{error, info, warn};

use super::AppState;
use crate::access::ctx::AccessCtx;
use crate::access::sockets::{Registration, SocketKey};
use crate::access::ws::{check_frame, refusal_frame, Frame, FrameVerdict, WsRoute};
use crate::pty::SpawnOpts;

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

/// The handshake's own class/caps check (owner{sessions}, `?spawn=true`
/// {dispatch}) ran in `auth_middleware` (G-ACCESS §1.6); frames are checked
/// here, and the socket is registered so a revoke or tier change closes it
/// (§3.10).
pub async fn pty_ws_handler(
    State(state): State<Arc<AppState>>,
    access: Option<Extension<Arc<crate::access::Runtime>>>,
    ctx: Option<Extension<AccessCtx>>,
    Path(id): Path<String>,
    Query(query): Query<PtyQuery>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    let (Some(Extension(access)), Some(Extension(ctx))) = (access, ctx) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let registration = access.registry.register(socket_key(&ctx));
    ws.on_upgrade(move |socket| {
        super::activity::track_ws(handle_pty_socket(
            socket,
            state,
            id,
            query,
            ctx,
            registration,
        ))
    })
}

/// What a socket is registered under (G-ACCESS §3.10).
pub(crate) fn socket_key(ctx: &AccessCtx) -> SocketKey {
    SocketKey {
        principal_id: ctx.principal_id.map(|p| p.to_string()),
        device_id: ctx.device_id.clone(),
        grant_epoch: ctx.grant_epoch,
    }
}

/// The close frame for a registry close (4401 / 4403).
pub(crate) fn close_message(close: crate::access::sockets::Close) -> Message {
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

async fn handle_pty_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    id: String,
    query: PtyQuery,
    ctx: AccessCtx,
    mut registration: Registration,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    // Revoked between the handshake and here: born closed.
    if let Some(close) = registration.revoked() {
        let _ = ws_tx.send(close_message(close)).await;
        return;
    }

    // The path segment may be a pty id, a terminal id, or a label. Resolve it
    // once: `write` / `resize` / `kill` take the pty id only, so holding on to
    // the unresolved segment silently drops every keystroke.
    let pty_id = match state.pty_manager.resolve_id(&id) {
        Ok(resolved) => resolved,
        Err(e) if query.spawn => {
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

    let snap = match state.pty_manager.attach_begin(&pty_id) {
        Some(snap) => snap,
        None => {
            error!("Failed to begin attach for PTY {pty_id}");
            let _ = ws_tx
                .send(control("ikenga.gone", serde_json::json!({ "id": id })))
                .await;
            return;
        }
    };

    // Watchdog, mirroring `commands::pty::pty_attach_begin`. While the gate is
    // installed NOTHING is delivered to any consumer — desktop sink included —
    // so a client that stalls mid-handshake (a slow socket blocking the
    // snapshot write) would freeze the terminal for everyone until the hold cap
    // overflows. `attach_arm` is token-checked, so a late fire is a no-op.
    {
        let watchdog = state.pty_manager.clone();
        let watched_id = pty_id.clone();
        let token = snap.token;
        tokio::spawn(async move {
            tokio::time::sleep(crate::pty::ATTACH_GATE_TIMEOUT).await;
            if watchdog.attach_arm(&watched_id, token) {
                warn!(pty = %watched_id, "ws attach never armed; watchdog released the gate");
            }
        });
    }

    let mut pty_rx = match state.pty_manager.subscribe(&pty_id) {
        Ok(rx) => rx,
        Err(err) => {
            error!("Failed to subscribe to PTY {pty_id}: {err}");
            state.pty_manager.attach_arm(&pty_id, snap.token);
            let _ = ws_tx
                .send(control(
                    "ikenga.error",
                    serde_json::json!({ "message": err.to_string() }),
                ))
                .await;
            return;
        }
    };

    // Announce the snapshot's absolute end offset BEFORE the bytes.
    //
    // Without it a reconnecting client cannot tell which part of the replayed
    // scrollback it has already painted, and re-appends the whole buffer on
    // every reconnect. With it the client keeps its own cursor into the
    // stream and emits only the genuinely new tail.
    let snapshot_len = snap.data.len();
    if ws_tx
        .send(control(
            "ikenga.snapshot",
            serde_json::json!({ "end_offset": snap.end_offset, "len": snapshot_len }),
        ))
        .await
        .is_err()
    {
        state.pty_manager.attach_arm(&pty_id, snap.token);
        return;
    }

    if snapshot_len > 0 {
        if let Err(e) = ws_tx.send(Message::Binary(snap.data)).await {
            error!("Failed to send scrollback snapshot: {e}");
            state.pty_manager.attach_arm(&pty_id, snap.token);
            return;
        }
    }

    // Release gate: held bytes (if any) flush to broadcast_tx and land on pty_rx.
    state.pty_manager.attach_arm(&pty_id, snap.token);

    let pty_manager = state.pty_manager.clone();
    let session_id = pty_id.clone();
    let id_for_send = pty_id.clone();

    // Task 1: Pump PTY output -> WebSocket binary frames, and announce exit.
    //
    // Exit is awaited explicitly rather than inferred from the broadcast
    // channel closing. An exited session is RETAINED (with its scrollback) for
    // EXITED_RETENTION, so its sender is not dropped for another ten minutes —
    // a client keyed on `Closed` would spend that whole window reconnecting to
    // a shell that already finished. A bare socket close is likewise
    // indistinguishable from a dropped link, which is the ambiguity this frame
    // exists to remove.
    let exit_manager = state.pty_manager.clone();
    let exit_watch_id = pty_id.clone();
    // Refusal frames from the input pump (G-ACCESS §1.6) share this writer.
    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let mut send_task = tokio::spawn(async move {
        let exit_fut = exit_manager.wait_for_exit(&exit_watch_id);
        tokio::pin!(exit_fut);

        loop {
            tokio::select! {
                // Bias toward draining output: on exit there is usually a
                // final chunk still in flight, and losing the last line of a
                // command is the most visible way to get this wrong.
                biased;

                // A revoke (4401) or tier change (4403) closes the socket;
                // the PTY lives on (G-ACCESS §3.10). First, so a flood of
                // output can't starve it.
                closed = &mut registration.closed => {
                    if let Ok(close) = closed {
                        let _ = ws_tx.send(close_message(close)).await;
                    }
                    break;
                }

                recv = pty_rx.recv() => match recv {
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

                Some(ctl) = ctl_rx.recv() => {
                    if ws_tx.send(ctl).await.is_err() {
                        break;
                    }
                }

                code = &mut exit_fut => {
                    // Flush whatever the shell emitted on its way out before
                    // announcing the exit.
                    while let Ok(bytes) = pty_rx.try_recv() {
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
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            // G-ACCESS §1.6: every PTY input frame needs {dispatch}; a
            // refused one is dropped and answered, the socket stays open.
            let frame = match &msg {
                Message::Binary(b) => Some(Frame::Binary(b)),
                Message::Text(t) => Some(Frame::Text(t)),
                _ => None,
            };
            if let Some(frame) = frame {
                if let FrameVerdict::Refuse(missing) = check_frame(ctx.caps, WsRoute::Pty, frame) {
                    let _ = ctl_tx.send(Message::Text(refusal_frame(missing)));
                    continue;
                }
                crate::access::audit::on_client_frame(
                    &ctx,
                    crate::access::audit::FrameRoute::Pty,
                    &session_id,
                );
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
                Message::Close(_) => {
                    break;
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
