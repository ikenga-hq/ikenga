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
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex as TokioMutex;
use tracing::info;

use super::pty_ws::{close_message, closed, SocketAccess};
use super::AppState;
use crate::access::ws::{Frame, Route};
use crate::access::{AccessCtx, DaemonAccess};
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

pub async fn chat_ws_handler(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    access: Option<Extension<Arc<DaemonAccess>>>,
    ctx: Option<Extension<AccessCtx>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // G-ACCESS §1.6: attaching needs `sessions` (checked at the handshake);
    // `Prompt` and `Cancel` need `dispatch`.
    let guard = SocketAccess::new(access.map(|Extension(a)| a), ctx.map(|Extension(c)| c));
    ws.on_upgrade(move |socket| {
        super::activity::track_ws(handle_chat_socket(socket, state, thread_id, guard))
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
    mut guard: SocketAccess,
) {
    let (ws_tx, mut ws_rx) = socket.split();
    let ws_tx: WsSink = Arc::new(TokioMutex::new(ws_tx));
    let closed_fut = closed(guard.take_closed());
    tokio::pin!(closed_fut);

    info!("Chat WebSocket connected for thread: {thread_id}");

    send(&ws_tx, status_event(&thread_id, "idle", None)).await;

    // The engine driving the current turn, and its task handle. Tracking the
    // engine (rather than assuming the default) means cancel reaches whichever
    // adapter actually started the turn.
    let mut in_flight: Option<(tokio::task::JoinHandle<()>, String)> = None;

    loop {
        let msg = tokio::select! {
            // G-ACCESS §3.10: revoked (4401) / caps changed (4403). The turn
            // in flight is cancelled below, as on any disconnect.
            close = &mut closed_fut => {
                let _ = ws_tx.lock().await.send(close_message(&close)).await;
                break;
            }
            next = ws_rx.next() => match next {
                Some(Ok(msg)) => msg,
                _ => break,
            },
        };
        match msg {
            Message::Text(text) => {
                // G-ACCESS §1.6: `Prompt` / `Cancel` need `dispatch`; a
                // refused frame is dropped and answered, the socket stays.
                if let Err(refusal) = guard.check(Route::Chat, Frame::Text(&text)) {
                    let _ = ws_tx.lock().await.send(Message::Text(refusal)).await;
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
                        // G-ACCESS §4.5.4 share hooks (WP-76 fills them): a
                        // share's `cwd` is forced under the share root, and a
                        // share-originated run gets no vault env (§4.5.1).
                        let cwd = match guard
                            .ctx
                            .as_ref()
                            .map(|c| crate::access::share::chat_cwd(c, cwd.clone()))
                        {
                            None => cwd,
                            Some(Ok(cwd)) => cwd,
                            Some(Err(e)) => {
                                send(&ws_tx, error_event(&thread_id, e.to_string())).await;
                                continue;
                            }
                        };
                        if guard
                            .ctx
                            .as_ref()
                            .is_some_and(|c| !crate::access::share::run_env(c))
                        {
                            // The daemon's engines inject no Ikenga vault env
                            // today; WP-76 strips it here once one does.
                            tracing::debug!("share-originated run: no vault env");
                        }
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
