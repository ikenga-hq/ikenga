use std::sync::Arc;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::pty::SpawnOpts;
use super::AppState;

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PtyControlMessage {
    Resize { rows: u16, cols: u16 },
    Write { data: String },
    Kill,
}

#[derive(Deserialize, Debug)]
pub struct PtySpawnPayload {
    pub terminal_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub cmd: Option<Vec<String>>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
}

#[derive(Serialize)]
pub struct PtySpawnResponse {
    pub ok: bool,
    pub pty_id: String,
    pub terminal_id: String,
}

pub async fn pty_ws_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_pty_socket(socket, state, id))
}

async fn handle_pty_socket(socket: WebSocket, state: Arc<AppState>, id: String) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Check if session exists or spawn a default one if needed
    let session_res = state.pty_manager.subscribe_session(&id);
    let (snapshot, mut pty_rx) = match session_res {
        Ok(res) => res,
        Err(e) => {
            warn!("PTY session not found for {id}: {e}. Attempting auto-spawn.");
            let default_shell = if cfg!(windows) {
                vec!["powershell.exe".to_string()]
            } else {
                vec!["/bin/bash".to_string()]
            };
            let spawn_res = state.pty_manager.spawn_headless(SpawnOpts {
                terminal_id: Some(id.clone()),
                title: Some("Terminal".to_string()),
                cwd: ".".to_string(),
                cmd: default_shell,
                env: std::collections::HashMap::new(),
                rows: 24,
                cols: 80,
            }).await;

            match spawn_res {
                Ok(new_id) => {
                    match state.pty_manager.subscribe_session(&new_id) {
                        Ok(res) => res,
                        Err(err) => {
                            error!("Failed to subscribe to auto-spawned PTY {new_id}: {err}");
                            let _ = ws_tx.send(Message::Text(format!("Error: {err}"))).await;
                            return;
                        }
                    }
                }
                Err(err) => {
                    error!("Failed to auto-spawn PTY for {id}: {err}");
                    let _ = ws_tx.send(Message::Text(format!("Error: {err}"))).await;
                    return;
                }
            }
        }
    };

    // Replay initial scrollback snapshot as binary frame
    if !snapshot.is_empty() {
        if let Err(e) = ws_tx.send(Message::Binary(snapshot)).await {
            error!("Failed to send scrollback snapshot: {e}");
            return;
        }
    }

    let pty_manager = state.pty_manager.clone();
    let session_id = id.clone();

    // Task 1: Pump PTY output -> WebSocket binary frames
    let mut send_task = tokio::spawn(async move {
        while let Ok(bytes) = pty_rx.recv().await {
            if ws_tx.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
    });

    // Task 2: Pump WebSocket input -> PTY stdin / control commands
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
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
