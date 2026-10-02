//! Multi-engine ACP chat WebSocket stream.
//!
//! Accepts client prompts over `/ws/chat/:id`, looks up the requested engine in
//! `AppState::engine_registry`, and streams ACP `session/update` JSON-RPC
//! notifications as deltas arrive.
//!
//! The turn runs on its own task rather than inline in the socket's read loop.
//! That is load-bearing: `session/cancel` arrives on the same socket, so a turn
//! awaited inline would block the very read that is supposed to interrupt it —
//! cancel would only ever be processed after the turn it was meant to stop.

use std::sync::Arc;

use agent_client_protocol::schema::SessionUpdate;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use super::AppState;
use crate::access::ctx::AccessCtx;
use crate::access::sockets::Registration;
use crate::access::ws::{check_frame, refusal_frame, Frame, FrameVerdict, WsRoute};
use crate::engines::EngineHandle;

/// Engine used when the client doesn't name one.
const DEFAULT_ENGINE: &str = "antigravity-cli";

type WsSink = Arc<TokioMutex<SplitSink<WebSocket, Message>>>;

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatClientMessage {
    Prompt {
        prompt: String,
        engine: Option<String>,
        cwd: Option<String>,
        model: Option<String>,
    },
    Cancel,
}

/// The handshake check (shared{sessions}) ran in `auth_middleware`
/// (G-ACCESS §1.6); `Prompt` / `Cancel` need {dispatch} and are checked per
/// frame, and the socket is registered for revocation (§3.10).
pub async fn chat_ws_handler(
    State(state): State<Arc<AppState>>,
    access: Option<Extension<Arc<crate::access::Runtime>>>,
    ctx: Option<Extension<AccessCtx>>,
    Path(thread_id): Path<String>,
    ws: WebSocketUpgrade,
) -> axum::response::Response {
    let (Some(Extension(access)), Some(Extension(ctx))) = (access, ctx) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let registration = access.registry.register(super::pty_ws::socket_key(&ctx));
    ws.on_upgrade(move |socket| {
        super::activity::track_ws(handle_chat_socket(
            socket,
            state,
            thread_id,
            ctx,
            registration,
        ))
    })
}

/// One `session/update` envelope.
fn update_event(thread_id: &str, update: serde_json::Value) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": { "thread_id": thread_id, "update": update }
    })
    .to_string()
}

fn status_event(thread_id: &str, status: &str, stop_reason: Option<&str>) -> String {
    let mut update = serde_json::json!({ "type": "status", "status": status });
    if let Some(reason) = stop_reason {
        update["stop_reason"] = serde_json::Value::String(reason.to_string());
    }
    update_event(thread_id, update)
}

/// Acknowledges a `cancel` regardless of whether a turn was running, so the
/// client can distinguish "cancel received" from "socket wedged".
fn cancel_ack_event(thread_id: &str) -> String {
    update_event(thread_id, serde_json::json!({ "type": "cancel_ack" }))
}

fn error_event(thread_id: &str, message: String) -> String {
    update_event(
        thread_id,
        serde_json::json!({
            "session_id": thread_id,
            "type": "error",
            "error": { "message": message }
        }),
    )
}

/// ACP's own snake_case wire form. Deriving it from serde rather than
/// `format!("{:?}")` keeps this in step with the protocol — `{:?}` yields
/// `EndTurn`/`endturn`, which is not a value any ACP client accepts, and
/// `StopReason` is `#[non_exhaustive]` so a hand-written map would silently
/// mis-render a variant added upstream.
fn stop_reason_wire(reason: agent_client_protocol::schema::StopReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "end_turn".to_string())
}

async fn send(ws_tx: &WsSink, payload: String) {
    let _ = ws_tx.lock().await.send(Message::Text(payload)).await;
}

/// Run a single turn to completion and emit its terminal status.
///
/// Spawned, never awaited inline — see the module docs.
async fn run_turn(
    state: Arc<AppState>,
    ws_tx: WsSink,
    thread_id: String,
    engine_name: String,
    prompt: String,
    cwd: Option<String>,
    model: Option<String>,
) {
    send(&ws_tx, status_event(&thread_id, "running", None)).await;

    let Some(engine_handle) = state.engine_registry.get(&engine_name).await else {
        send(
            &ws_tx,
            error_event(&thread_id, format!("Unknown engine: {engine_name}")),
        )
        .await;
        send(&ws_tx, status_event(&thread_id, "idle", Some("refusal"))).await;
        return;
    };

    match engine_handle {
        EngineHandle::Antigravity(engine) => {
            if let Some(c) = &cwd {
                engine.register_session(thread_id.clone(), c.clone()).await;
            }

            let (tx_chan, mut rx_chan) = tokio::sync::mpsc::unbounded_channel::<SessionUpdate>();

            // `on_update` owns the sender, so it MUST be scoped inside the
            // prompt future. The drain below ends only when every sender has
            // dropped; a callback that outlives the `join!` leaves the channel
            // open forever and hangs the socket after the CLI has exited.
            let prompt_fut = {
                let thread_id = thread_id.clone();
                let prompt = prompt.clone();
                let model = model.clone();
                let engine = engine.clone();
                async move {
                    let on_update = move |update: SessionUpdate| {
                        let _ = tx_chan.send(update);
                    };
                    let cb: &(dyn Fn(SessionUpdate) + Send + Sync) = &on_update;
                    engine
                        .run_prompt(&thread_id, &prompt, model.as_deref(), Some(cb))
                        .await
                }
            };

            let drain = {
                let ws_tx = ws_tx.clone();
                let thread_id = thread_id.clone();
                async move {
                    while let Some(update) = rx_chan.recv().await {
                        send(
                            &ws_tx,
                            update_event(
                                &thread_id,
                                serde_json::to_value(&update).unwrap_or(serde_json::Value::Null),
                            ),
                        )
                        .await;
                    }
                }
            };

            let (prompt_res, ()) = tokio::join!(prompt_fut, drain);

            let stop_reason = match prompt_res {
                Ok(resp) => stop_reason_wire(resp.stop_reason),
                Err(e) => {
                    // The engine's failure is the only explanation the user
                    // gets; reporting a bare `end_turn` here presents a failed
                    // turn as a successful empty one.
                    tracing::warn!(thread = %thread_id, "antigravity prompt failed: {e}");
                    send(&ws_tx, error_event(&thread_id, e)).await;
                    "refusal".to_string()
                }
            };

            send(&ws_tx, status_event(&thread_id, "idle", Some(&stop_reason))).await;
        }
        // Only reachable in the desktop build; headless `EngineHandle` has the
        // single Antigravity variant, so this arm would be an unreachable
        // pattern there.
        #[cfg(feature = "desktop")]
        _ => {
            // Remaining adapters have no headless driver yet. Say so rather
            // than echoing the prompt back as though it were a reply.
            tracing::warn!(
                engine = %engine_name,
                "engine has no headless daemon driver; refusing the turn"
            );
            send(
                &ws_tx,
                error_event(
                    &thread_id,
                    format!(
                        "Engine '{engine_name}' is registered but not yet drivable from the \
                         headless daemon. Only '{DEFAULT_ENGINE}' is wired today."
                    ),
                ),
            )
            .await;
            send(&ws_tx, status_event(&thread_id, "idle", Some("refusal"))).await;
        }
    }
}

/// Ask `engine_name`'s adapter to cancel the in-flight turn on `thread_id`.
async fn cancel_turn(state: &AppState, engine_name: &str, thread_id: &str) {
    if let Some(EngineHandle::Antigravity(engine)) = state.engine_registry.get(engine_name).await {
        let _ = engine.handle_cancel(thread_id.to_string()).await;
    }
}

async fn handle_chat_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    thread_id: String,
    ctx: AccessCtx,
    mut registration: Registration,
) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(TokioMutex::new(ws_tx));
    if let Some(close) = registration.revoked() {
        let _ = ws_tx
            .lock()
            .await
            .send(super::pty_ws::close_message(close))
            .await;
        return;
    }

    info!("Chat WebSocket connected for thread: {thread_id}");

    send(&ws_tx, status_event(&thread_id, "idle", None)).await;

    // The engine driving the current turn, and its task handle. Tracking the
    // engine (rather than assuming the default) means cancel reaches whichever
    // adapter actually started the turn.
    let mut in_flight: Option<(tokio::task::JoinHandle<()>, String)> = None;

    loop {
        let msg = tokio::select! {
            // A revoke (4401) or tier change (4403) closes the socket; a
            // running turn is cancelled below like any dropped socket.
            closed = &mut registration.closed => {
                if let Ok(close) = closed {
                    let _ = ws_tx.lock().await.send(super::pty_ws::close_message(close)).await;
                }
                break;
            }
            msg = ws_rx.next() => match msg {
                Some(Ok(msg)) => msg,
                _ => break,
            },
        };
        match msg {
            Message::Text(text) => {
                // G-ACCESS §1.6: `Prompt` / `Cancel` need {dispatch}.
                if let FrameVerdict::Refuse(missing) =
                    check_frame(ctx.caps, WsRoute::Chat, Frame::Text(&text))
                {
                    let _ = ws_tx
                        .lock()
                        .await
                        .send(Message::Text(refusal_frame(missing)))
                        .await;
                    continue;
                }
                let Ok(client_msg) = serde_json::from_str::<ChatClientMessage>(&text) else {
                    continue;
                };
                match client_msg {
                    ChatClientMessage::Prompt {
                        prompt,
                        engine,
                        cwd,
                        model,
                    } => {
                        // G-ACCESS §4.5.4 share hooks (WP-76): the client's
                        // `cwd` is forced under the share root, and a
                        // share-originated run gets no vault env (N-10).
                        let cwd = match ctx.share.as_ref() {
                            None => cwd,
                            Some(share) => {
                                match crate::access::share::chat_cwd(share, cwd.as_deref())
                                    .and_then(|c| crate::access::share::run_env(share).map(|_| c))
                                {
                                    Ok(c) => c,
                                    Err(e) => {
                                        send(&ws_tx, error_event(&thread_id, e)).await;
                                        continue;
                                    }
                                }
                            }
                        };
                        crate::access::audit::on_client_frame(
                            &ctx,
                            crate::access::audit::FrameRoute::Chat,
                            &thread_id,
                        );
                        // Reap a finished turn so a completed one never looks
                        // in-flight.
                        if in_flight.as_ref().is_some_and(|(h, _)| h.is_finished()) {
                            in_flight = None;
                        }
                        if in_flight.is_some() {
                            send(
                                &ws_tx,
                                error_event(
                                    &thread_id,
                                    "A turn is already running on this thread; cancel it first."
                                        .to_string(),
                                ),
                            )
                            .await;
                            continue;
                        }

                        let engine_name = engine.unwrap_or_else(|| DEFAULT_ENGINE.to_string());
                        info!("Running prompt on engine {engine_name} for thread {thread_id}");

                        let handle = tokio::spawn(run_turn(
                            state.clone(),
                            ws_tx.clone(),
                            thread_id.clone(),
                            engine_name.clone(),
                            prompt,
                            cwd,
                            model,
                        ));
                        in_flight = Some((handle, engine_name));
                    }
                    ChatClientMessage::Cancel => {
                        info!("Cancelled chat turn for thread {thread_id}");
                        // Reap first. A finished-but-unreaped handle is not a
                        // running turn, and treating it as one made cancel a
                        // silent no-op: the client got no reply at all.
                        if in_flight.as_ref().is_some_and(|(h, _)| h.is_finished()) {
                            in_flight = None;
                        }

                        // The ack always goes out, so the client can tell
                        // "received and acted on" from "socket is wedged".
                        send(&ws_tx, cancel_ack_event(&thread_id)).await;

                        match in_flight.as_ref() {
                            // A live turn owns its own terminal status —
                            // emitting one here too would double-report the
                            // end of the same turn.
                            Some((_, engine_name)) => {
                                cancel_turn(&state, engine_name, &thread_id).await;
                            }
                            // Nothing running, so nothing else will ever emit
                            // a terminal status for this thread.
                            None => {
                                cancel_turn(&state, DEFAULT_ENGINE, &thread_id).await;
                                send(&ws_tx, status_event(&thread_id, "idle", Some("cancelled")))
                                    .await;
                            }
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    // A dropped socket must not leave a CLI child running against a thread
    // nobody is listening to.
    if let Some((handle, engine_name)) = in_flight {
        if !handle.is_finished() {
            cancel_turn(&state, &engine_name, &thread_id).await;
            handle.abort();
        }
    }

    info!("Chat WebSocket disconnected for thread: {thread_id}");
}
