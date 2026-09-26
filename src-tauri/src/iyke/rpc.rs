//! Generic RPC pending-registry for FE round-trip handlers.
//!
//! Iyke's read endpoints that need fresh data from the webview (DOM
//! snapshot, TanStack query-cache dump, wait-for-predicate) follow the
//! same pattern as `commands::screenshot::capture`: emit a Tauri event
//! with a `request_id`, await a `oneshot` keyed by that id, the FE calls
//! a Tauri command that resolves the oneshot. This module owns the
//! HashMap+Mutex bookkeeping so each handler is just a thin wrapper.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

pub type Pending<T> = Arc<Mutex<HashMap<String, oneshot::Sender<T>>>>;

pub fn new_pending<T>() -> Pending<T> {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Emit `event` with a payload that carries `request_id`, then await the
/// matching `oneshot` up to `timeout`. The FE side calls the corresponding
/// `*_done` Tauri command which calls [`resolve`] to fulfill the oneshot.
pub async fn request<T, P>(
    app: &AppHandle,
    pending: &Pending<T>,
    event: &str,
    timeout: Duration,
    build_payload: impl FnOnce(&str) -> P,
) -> Result<T>
where
    P: Serialize + Clone,
{
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<T>();
    {
        let mut map = pending.lock().await;
        map.insert(request_id.clone(), tx);
    }

    let payload = build_payload(&request_id);
    if let Err(e) = app.emit(event, &payload) {
        pending.lock().await.remove(&request_id);
        return Err(anyhow!("emit {event}: {e}"));
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(anyhow!("{event} sender dropped")),
        Err(_) => {
            pending.lock().await.remove(&request_id);
            Err(anyhow!("{event} timed out after {}ms", timeout.as_millis()))
        }
    }
}

/// Same as [`request`], but targets one window (`emit_to`) instead of
/// broadcasting to every window (`emit`). WP-62 review (S7): the actions/keys
/// write round trips have exactly one real listener —
/// `use-iyke-shell-sync.ts`, mounted once inside the main window's
/// `<Workspace />` — so a broadcast needlessly wakes every detached window's
/// webview too.
pub async fn request_to<T, P>(
    app: &AppHandle,
    pending: &Pending<T>,
    window_label: &str,
    event: &str,
    timeout: Duration,
    build_payload: impl FnOnce(&str) -> P,
) -> Result<T>
where
    P: Serialize + Clone,
{
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<T>();
    {
        let mut map = pending.lock().await;
        map.insert(request_id.clone(), tx);
    }

    let payload = build_payload(&request_id);
    if let Err(e) = app.emit_to(window_label, event, &payload) {
        pending.lock().await.remove(&request_id);
        return Err(anyhow!("emit_to {window_label} {event}: {e}"));
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err(anyhow!("{event} sender dropped")),
        Err(_) => {
            pending.lock().await.remove(&request_id);
            Err(anyhow!("{event} timed out after {}ms", timeout.as_millis()))
        }
    }
}

/// Resolve the oneshot for a given request_id. Called by the FE-callback
/// Tauri commands. Returns an error if no pending entry matches.
pub async fn resolve<T>(pending: &Pending<T>, request_id: &str, value: T) -> Result<()> {
    let mut map = pending.lock().await;
    let Some(tx) = map.remove(request_id) else {
        return Err(anyhow!("no pending entry for {request_id}"));
    };
    let _ = tx.send(value);
    Ok(())
}
