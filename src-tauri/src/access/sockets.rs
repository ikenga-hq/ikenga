//! The T0 open-socket registry (G-ACCESS §3.10): every `/ws/pty`,
//! `/ws/chat` and `/ws/fs` socket the T0 daemon accepts registers
//! `(principal_id, device_id, grant_epoch)` and selects on a close signal,
//! so a revoke closes the device's sockets **immediately** with `4401`
//! (`device_revoked`) and a tier change with `4403` (`caps_changed`).
//!
//! T1 uses WP-20's broker registry (`server::broker::ws_registry`, extended
//! by R-5) instead; this one lives in the daemon process only. Closing a
//! socket never kills the PTY or run behind it (G-PRINCIPAL §2.2).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// Why a socket is told to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    pub code: u16,
    pub reason: &'static str,
}

impl Close {
    pub const DEVICE_REVOKED: Close = Close {
        code: super::devices::CLOSE_REVOKED,
        reason: "device_revoked",
    };
    pub const CAPS_CHANGED: Close = Close {
        code: super::devices::CLOSE_CAPS_CHANGED,
        reason: "caps_changed",
    };
}

#[derive(Debug, Clone)]
struct Entry {
    device_id: Option<String>,
    close: Option<Arc<Mutex<Option<oneshot::Sender<Close>>>>>,
}

/// Every open socket of this daemon.
#[derive(Default)]
pub struct Registry {
    entries: Mutex<HashMap<u64, Entry>>,
    next: AtomicU64,
}

/// One registered socket; dropping it unregisters.
pub struct SocketGuard {
    id: u64,
    registry: Arc<Registry>,
    pub closed: oneshot::Receiver<Close>,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        self.registry.lock().remove(&self.id);
    }
}

impl Registry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn register(self: &Arc<Self>, device_id: Option<String>) -> SocketGuard {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.lock().insert(
            id,
            Entry {
                device_id,
                close: Some(Arc::new(Mutex::new(Some(tx)))),
            },
        );
        SocketGuard {
            id,
            registry: self.clone(),
            closed: rx,
        }
    }

    /// Close every open socket of `device_id`; returns how many.
    pub fn close_device(&self, device_id: &str, why: Close) -> usize {
        let mut closed = 0;
        for e in self.lock().values() {
            if e.device_id.as_deref() == Some(device_id) {
                if let Some(slot) = &e.close {
                    if let Some(tx) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        let _ = tx.send(why.clone());
                        closed += 1;
                    }
                }
            }
        }
        closed
    }

    /// `DeviceView.liveSockets` (D-05 "Live sessions").
    pub fn count_for_device(&self, device_id: &str) -> usize {
        self.lock()
            .values()
            .filter(|e| e.device_id.as_deref() == Some(device_id))
            .count()
    }

    /// Open sockets authenticated by a paired device (not the host).
    pub fn device_sockets(&self) -> usize {
        self.lock()
            .values()
            .filter(|e| e.device_id.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A-6 / A-7 (registry half): a revoke closes exactly that device's
    /// sockets with 4401; a tier change with 4403.
    #[tokio::test]
    async fn close_device_signals_only_that_device() {
        let reg = Registry::new();
        let mut a1 = reg.register(Some("a".into()));
        let mut a2 = reg.register(Some("a".into()));
        let mut b = reg.register(Some("b".into()));
        let mut host = reg.register(None);
        assert_eq!(reg.count_for_device("a"), 2);
        assert_eq!(reg.device_sockets(), 3);
        assert_eq!(reg.close_device("a", Close::DEVICE_REVOKED), 2);
        assert_eq!(a1.closed.try_recv().unwrap().code, 4401);
        assert_eq!(a2.closed.try_recv().unwrap().reason, "device_revoked");
        assert!(b.closed.try_recv().is_err());
        assert!(host.closed.try_recv().is_err());
        assert_eq!(
            reg.close_device("a", Close::DEVICE_REVOKED),
            0,
            "already told"
        );
        assert_eq!(reg.close_device("b", Close::CAPS_CHANGED), 1);
        assert_eq!(b.closed.try_recv().unwrap().code, 4403);
        drop(a1);
        drop(a2);
        assert_eq!(reg.count_for_device("a"), 0);
    }
}
