//! What keeps a daemon "active" for its idle timeout (G-PRINCIPAL §5 row 13,
//! OD-13): PTY sessions (counted by `PtyManager`), **open WebSocket
//! connections** and recent authenticated requests.
//!
//! Before WP-20 the idle watcher counted PTY sessions only, so an open chat
//! or fs socket — or a browser driving the RPC surface with no terminal open
//! — could be reaped mid-use. Under T1 that watcher is what idles each
//! principal child, so the contract pins "active also counts open WS".

use std::future::Future;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static OPEN_WS: AtomicUsize = AtomicUsize::new(0);
/// Milliseconds since [`epoch`] at the last [`touch`].
static LAST_REQUEST_MS: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

/// Held for the lifetime of one upgraded WebSocket.
#[derive(Debug)]
pub struct WsOpenGuard(());

impl WsOpenGuard {
    pub fn new() -> Self {
        OPEN_WS.fetch_add(1, Ordering::SeqCst);
        touch();
        WsOpenGuard(())
    }
}

impl Default for WsOpenGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WsOpenGuard {
    fn drop(&mut self) {
        OPEN_WS.fetch_sub(1, Ordering::SeqCst);
        touch();
    }
}

/// Run a socket's handler future with a [`WsOpenGuard`] held for its whole
/// life (the `on_upgrade` callback of every `/ws/*` handler).
pub async fn track_ws<F: Future>(socket_task: F) -> F::Output {
    let _open = WsOpenGuard::new();
    socket_task.await
}

/// WebSocket connections currently open in this process.
pub fn open_ws() -> usize {
    OPEN_WS.load(Ordering::SeqCst)
}

/// Note an authenticated request.
pub fn touch() {
    LAST_REQUEST_MS.store(now_ms(), Ordering::SeqCst);
}

/// Time since the last [`touch`] (or since the process first asked).
pub fn since_last_request() -> Duration {
    let last = LAST_REQUEST_MS.load(Ordering::SeqCst);
    Duration::from_millis(now_ms().saturating_sub(last))
}

/// Whether the daemon counts as active: any PTY session, any open WS, or a
/// request within `window`.
pub fn is_active(pty_sessions: usize, window: Duration) -> bool {
    pty_sessions > 0 || open_ws() > 0 || since_last_request() < window
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_open_socket_counts_as_activity() {
        let before = open_ws();
        let guard = WsOpenGuard::new();
        assert!(open_ws() > before);
        assert!(is_active(0, Duration::ZERO), "an open WS is active");
        drop(guard);
        let out = track_ws(async { open_ws() }).await;
        assert!(out > before.saturating_sub(1));
    }

    #[test]
    fn a_recent_request_counts_and_pty_sessions_count() {
        touch();
        assert!(since_last_request() < Duration::from_secs(5));
        assert!(is_active(0, Duration::from_secs(60)));
        assert!(is_active(1, Duration::ZERO));
    }
}
