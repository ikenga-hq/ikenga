//! Multi-engine ACP chat WebSocket stream.
//!
//! Accepts client prompts over `/ws/chat/:id`, looks up the requested engine in
//! `AppState::engine_registry`, and streams ACP `session/update` JSON-RPC notifications
//! as deltas arrive.

use std::sync::Arc;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tracing::info;

use super::AppState;

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
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_chat_socket(socket, state, thread_id))
}

async fn handle_chat_socket(socket: WebSocket, state: Arc<AppState>, thread_id: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    info!("Chat WebSocket connected for thread: {thread_id}");

    // Welcome message
    let welcome = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "thread_id": thread_id,
            "update": {
                "session_id": thread_id,
                "type": "agent_message_delta",
                "delta": {
                    "type": "text",
                    "text": ""
                }
            }
        }
    });
    let _ = ws_tx.send(Message::Text(welcome.to_string())).await;

    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(client_msg) = serde_json::from_str::<ChatClientMessage>(&text) {
                    match client_msg {
                        ChatClientMessage::Prompt { prompt, engine, cwd, model } => {
                            let engine_name = engine.unwrap_or_else(|| "antigravity-cli".to_string());
                            info!("Running prompt on engine {engine_name} for thread {thread_id}");

                            // Send turn start notification
                            let start_event = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": {
                                    "thread_id": thread_id,
                                    "update": {
                                        "type": "status",
                                        "status": "running"
                                    }
                                }
                            });
                            let _ = ws_tx.send(Message::Text(start_event.to_string())).await;

                            if let Some(engine_handle) = state.engine_registry.get(&engine_name).await {
                                match engine_handle {
                                    crate::engines::EngineHandle::Antigravity(antigravity_engine) => {
                                        if let Some(c) = &cwd {
                                            antigravity_engine.register_session(thread_id.clone(), c.clone()).await;
                                        }
                                        let (tx_chan, mut rx_chan) = tokio::sync::mpsc::unbounded_channel();
                                        let thread_id_clone = thread_id.clone();

                                         let on_update = move |update: agent_client_protocol::schema::SessionUpdate| {
                                             let _ = tx_chan.send(update);
                                         };
                                         let cb: &(dyn Fn(agent_client_protocol::schema::SessionUpdate) + Send + Sync) = &on_update;
                                         let prompt_fut = antigravity_engine.run_prompt(
                                             None,
                                             &thread_id,
                                             &prompt,
                                             model.as_deref(),
                                             Some(cb),
                                         );

                                        let (prompt_res, _) = tokio::join!(
                                            prompt_fut,
                                            async {
                                                while let Some(update) = rx_chan.recv().await {
                                                    let event = serde_json::json!({
                                                        "jsonrpc": "2.0",
                                                        "method": "session/update",
                                                        "params": {
                                                            "thread_id": thread_id_clone,
                                                            "update": update
                                                        }
                                                    });
                                                    let _ = ws_tx.send(Message::Text(event.to_string())).await;
                                                }
                                            }
                                        );

                                        let stop_reason = match prompt_res {
                                            Ok(resp) => format!("{:?}", resp.stop_reason).to_lowercase(),
                                            Err(e) => {
                                                tracing::warn!("Antigravity prompt error: {e}");
                                                "end_turn".to_string()
                                            }
                                        };

                                        let stop_event = serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "method": "session/update",
                                            "params": {
                                                "thread_id": thread_id,
                                                "update": {
                                                    "type": "status",
                                                    "status": "idle",
                                                    "stop_reason": stop_reason
                                                }
                                            }
                                        });
                                        let _ = ws_tx.send(Message::Text(stop_event.to_string())).await;
                                    }
                                    _ => {
                                        let response_event = serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "method": "session/update",
                                            "params": {
                                                "thread_id": thread_id,
                                                "update": {
                                                    "session_id": thread_id,
                                                    "type": "agent_message_delta",
                                                    "delta": {
                                                        "type": "text",
                                                        "text": format!("Received prompt for {engine_name}: {prompt}")
                                                    }
                                                }
                                            }
                                        });
                                        let _ = ws_tx.send(Message::Text(response_event.to_string())).await;

                                        let stop_event = serde_json::json!({
                                            "jsonrpc": "2.0",
                                            "method": "session/update",
                                            "params": {
                                                "thread_id": thread_id,
                                                "update": {
                                                    "type": "status",
                                                    "status": "idle",
                                                    "stop_reason": "end_turn"
                                                }
                                            }
                                        });
                                        let _ = ws_tx.send(Message::Text(stop_event.to_string())).await;
                                    }
                                }
                            } else {
                                let response_event = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "session/update",
                                    "params": {
                                        "thread_id": thread_id,
                                        "update": {
                                            "session_id": thread_id,
                                            "type": "agent_message_delta",
                                            "delta": {
                                                "type": "text",
                                                "text": format!("Unknown engine {engine_name}")
                                            }
                                        }
                                    }
                                });
                                let _ = ws_tx.send(Message::Text(response_event.to_string())).await;
                            }
                        }
                        ChatClientMessage::Cancel => {
                            info!("Cancelled chat turn for thread {thread_id}");
                            if let Some(engine_handle) = state.engine_registry.get("antigravity-cli").await {
                                if let crate::engines::EngineHandle::Antigravity(engine) = engine_handle {
                                    let _ = engine.handle_cancel(thread_id.clone()).await;
                                }
                            }
                            let cancel_event = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "session/update",
                                "params": {
                                    "thread_id": thread_id,
                                    "update": {
                                        "type": "status",
                                        "status": "cancelled"
                                    }
                                }
                            });
                            let _ = ws_tx.send(Message::Text(cancel_event.to_string())).await;
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    info!("Chat WebSocket disconnected for thread: {thread_id}");
}

