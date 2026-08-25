//! ⚠ STUB — this socket does not watch anything.
//!
//! It completes the upgrade, sends one `fs_ready` frame and then drains
//! messages until close. There is no `notify` watcher behind it, so a browser
//! client will never learn that a file changed. Kept as the placeholder the
//! real watcher bridge will land in; do not build UI that trusts it.

use std::sync::Arc;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tracing::info;

use super::AppState;

pub async fn fs_ws_handler(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_fs_socket(socket, state))
}

async fn handle_fs_socket(socket: WebSocket, _state: Arc<AppState>) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    info!("FS Watcher WebSocket client connected");

    // Says "stub", not "watching" — a client that believed the old payload
    // would sit forever waiting for change events that cannot arrive.
    let initial = json!({
        "type": "fs_ready",
        "status": "stub",
        "watching": false
    });
    let _ = ws_tx.send(Message::Text(initial.to_string())).await;

    while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Close(_) = msg {
            break;
        }
    }

    info!("FS Watcher WebSocket client disconnected");
}
