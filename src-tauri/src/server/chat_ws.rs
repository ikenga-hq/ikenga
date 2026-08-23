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

async fn handle_chat_socket(socket: WebSocket, _state: Arc<AppState>, thread_id: String) {
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
                        ChatClientMessage::Prompt { prompt, engine, cwd: _, model: _ } => {
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

                            // Echo response text block
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
                        ChatClientMessage::Cancel => {
                            info!("Cancelled chat turn for thread {thread_id}");
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
