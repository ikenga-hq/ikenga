//! Open-socket registries and revocation immediacy (G-ACCESS §3.10, P-12).
//!
//! * **T0:** [`Registry`], built here and held by the daemon's
//!   [`super::Runtime`]. The `pty_ws`, `chat_ws` and `fs_ws` handlers
//!   register `(principal_id, device_id, grant_epoch)` and select on the
//!   registration's close signal.
//! * **T1:** WP-20's broker registry (`server/broker/ws_registry.rs`),
//!   extended by R-5. It implements [`SocketControl`] there.
//!
//! Close codes: **4401** credential dead (`device_revoked`,
//! `epoch_changed`), **4403** caps changed (`caps_changed`). Closing a
//! socket never kills its PTY or run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

/// P-12: the credential is dead.
pub const CLOSE_REVOKED: u16 = 4401;
/// P-12: the caps changed; reconnect.
pub const CLOSE_CAPS_CHANGED: u16 = 4403;

/// Why a socket is being closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Close {
    pub code: u16,
    pub reason: &'static str,
}

impl Close {
    pub const DEVICE_REVOKED: Close = Close {
        code: CLOSE_REVOKED,
        reason: "device_revoked",
    };
    pub const CAPS_CHANGED: Close = Close {
        code: CLOSE_CAPS_CHANGED,
        reason: "caps_changed",
    };
    pub const EPOCH_CHANGED: Close = Close {
        code: CLOSE_REVOKED,
        reason: "epoch_changed",
    };
}

/// What the access arms need from whichever registry the tier runs: close
/// a device's sockets, and count them (`DeviceView.liveSockets`).
pub trait SocketControl: Send + Sync {
    /// `device_id` was revoked: close every open socket of it with 4401
    /// (`device_revoked`); a racing handshake is born closed.
    fn device_revoked(&self, device_id: &str) -> usize;
    /// `device_id`'s tier changed (its `grant_epoch` is now `new_epoch`):
    /// close its older sockets with 4403 (`caps_changed`).
    fn device_caps_changed(&self, device_id: &str, new_epoch: i64) -> usize;
    /// Open sockets of `device_id`.
    fn live_for_device(&self, device_id: &str) -> usize;
}

/// What a T0 socket was opened with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketKey {
    pub principal_id: Option<String>,
    /// The paired device, or the host device for the operator bearer.
    pub device_id: Option<String>,
    pub grant_epoch: Option<i64>,
}

struct Entry {
    key: SocketKey,
    close: Option<oneshot::Sender<Close>>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<u64, Entry>,
    /// Devices revoked by this process: a socket registered after the revoke
    /// (its handshake raced it) is born closed.
    revoked: HashSet<String>,
    /// Per device: the lowest `grant_epoch` still valid after a tier change.
    epoch_floor: HashMap<String, i64>,
}

fn born_closed(inner: &Inner, key: &SocketKey) -> Option<Close> {
    let device = key.device_id.as_deref()?;
    if inner.revoked.contains(device) {
        return Some(Close::DEVICE_REVOKED);
    }
    match (inner.epoch_floor.get(device), key.grant_epoch) {
        (Some(floor), Some(epoch)) if epoch < *floor => Some(Close::CAPS_CHANGED),
        _ => None,
    }
}

/// The T0 daemon's open-socket registry.
#[derive(Default)]
pub struct Registry {
    inner: Mutex<Inner>,
    next: AtomicU64,
}

/// One registered socket. Dropping it unregisters; `closed` resolves when
/// the registry decides the socket must close.
pub struct Registration {
    id: u64,
    registry: Arc<Registry>,
    pub closed: oneshot::Receiver<Close>,
}

impl Registration {
    /// Already told to close (born closed, or closed meanwhile)?
    pub fn revoked(&mut self) -> Option<Close> {
        self.closed.try_recv().ok()
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.registry.lock().entries.remove(&self.id);
    }
}

impl Registry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn register(self: &Arc<Self>, key: SocketKey) -> Registration {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        let close = match born_closed(&inner, &key) {
            Some(c) => {
                let _ = tx.send(c);
                None
            }
            None => Some(tx),
        };
        inner.entries.insert(id, Entry { key, close });
        drop(inner);
        Registration {
            id,
            registry: self.clone(),
            closed: rx,
        }
    }

    /// Open sockets (the idle watcher counts them, §2.5).
    pub fn len(&self) -> usize {
        self.lock()
            .entries
            .values()
            .filter(|e| e.close.is_some())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Open sockets of paired devices (not the host's own).
    pub fn device_sockets(&self, host_device: Option<&str>) -> usize {
        self.lock()
            .entries
            .values()
            .filter(|e| {
                e.close.is_some()
                    && e.key.device_id.is_some()
                    && e.key.device_id.as_deref() != host_device
            })
            .count()
    }

    /// Record that `device_id` is revoked and close its sockets.
    pub fn revoke_device(&self, device_id: &str) -> usize {
        let mut inner = self.lock();
        inner.revoked.insert(device_id.to_string());
        Self::close_in(
            &mut inner,
            |k| k.device_id.as_deref() == Some(device_id),
            Close::DEVICE_REVOKED,
        )
    }

    /// A tier change: every socket of `device_id` below `new_epoch` closes
    /// with 4403, and a racing handshake of the old epoch is born closed.
    pub fn caps_changed(&self, device_id: &str, new_epoch: i64) -> usize {
        let mut inner = self.lock();
        let floor = inner
            .epoch_floor
            .entry(device_id.to_string())
            .or_insert(new_epoch);
        *floor = (*floor).max(new_epoch);
        Self::close_in(
            &mut inner,
            |k| k.device_id.as_deref() == Some(device_id) && k.grant_epoch.is_none_or_lt(new_epoch),
            Close::CAPS_CHANGED,
        )
    }

    fn close_in(inner: &mut Inner, pred: impl Fn(&SocketKey) -> bool, close: Close) -> usize {
        let mut n = 0;
        for e in inner.entries.values_mut() {
            if e.close.is_some() && pred(&e.key) {
                if let Some(tx) = e.close.take() {
                    let _ = tx.send(close);
                    n += 1;
                }
            }
        }
        n
    }
}

trait EpochBelow {
    fn is_none_or_lt(self, epoch: i64) -> bool;
}

impl EpochBelow for Option<i64> {
    fn is_none_or_lt(self, epoch: i64) -> bool {
        self.map_or(true, |e| e < epoch)
    }
}

impl SocketControl for Registry {
    fn device_revoked(&self, device_id: &str) -> usize {
        self.revoke_device(device_id)
    }

    fn device_caps_changed(&self, device_id: &str, new_epoch: i64) -> usize {
        self.caps_changed(device_id, new_epoch)
    }

    fn live_for_device(&self, device_id: &str) -> usize {
        self.lock()
            .entries
            .values()
            .filter(|e| e.close.is_some() && e.key.device_id.as_deref() == Some(device_id))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(device: &str, epoch: i64) -> SocketKey {
        SocketKey {
            principal_id: Some("p".into()),
            device_id: Some(device.into()),
            grant_epoch: Some(epoch),
        }
    }

    /// A-6 (in-process half): revoke closes every socket of that device with
    /// 4401 at once, others stay; a racing handshake is born closed.
    #[test]
    fn revoke_closes_that_devices_sockets_immediately() {
        let reg = Registry::new();
        let mut a1 = reg.register(key("a", 0));
        let mut a2 = reg.register(key("a", 0));
        let mut b = reg.register(key("b", 0));
        assert_eq!(reg.live_for_device("a"), 2);
        assert_eq!(reg.revoke_device("a"), 2);
        assert_eq!(a1.revoked(), Some(Close::DEVICE_REVOKED));
        assert_eq!(a2.revoked().map(|c| c.code), Some(4401));
        assert_eq!(b.revoked(), None);
        assert_eq!(reg.live_for_device("a"), 0);
        let mut late = reg.register(key("a", 0));
        assert_eq!(late.revoked(), Some(Close::DEVICE_REVOKED));
        assert_eq!(reg.len(), 1);
    }

    /// A-7 (in-process half): a tier change closes the device's sockets with
    /// 4403; a reconnect at the new epoch stays open.
    #[test]
    fn a_tier_change_closes_with_4403_and_the_new_epoch_reconnects() {
        let reg = Registry::new();
        let mut old = reg.register(key("a", 0));
        assert_eq!(reg.caps_changed("a", 1), 1);
        assert_eq!(old.revoked(), Some(Close::CAPS_CHANGED));
        let mut racing = reg.register(key("a", 0));
        assert_eq!(racing.revoked().map(|c| c.code), Some(4403));
        let mut fresh = reg.register(key("a", 1));
        assert_eq!(fresh.revoked(), None);
        drop(fresh);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn operator_sockets_are_not_device_sockets() {
        let reg = Registry::new();
        let _host = reg.register(key("host", 0));
        let _op = reg.register(SocketKey {
            principal_id: None,
            device_id: None,
            grant_epoch: None,
        });
        let _phone = reg.register(key("phone", 0));
        assert_eq!(reg.len(), 3);
        assert_eq!(reg.device_sockets(Some("host")), 1);
    }
}
