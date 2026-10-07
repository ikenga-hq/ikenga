//! Web Push (plans/pwa S2/S3, decisions W3 and W4).
//!
//! **Who stores and who sends.** One [`hub::PushHub`] per access-store
//! owner: the T0 daemon, or the T1 broker. Principal children and the
//! desktop process never hold the VAPID key or a subscription:
//!
//! * the VAPID private key stays in the T0 `--data-dir` / the T1 root-only
//!   `operator/` directory ([`vapid`]);
//! * subscriptions live in the access store ([`store`]) so a revoked device
//!   grant takes its subscriptions with it, in the same transaction;
//! * pairing, invite-accept and server-update events already start in the
//!   store owner.
//!
//! **Where events come from** ([`emit`], the one producer entry point):
//!
//! | process | sink |
//! |---|---|
//! | T0 daemon | the hub, in-process |
//! | T1 broker | the hub, in-process (pairing, invite, update) |
//! | T1 principal child | [`outbox`], which the broker drains ([`pump`]) and attributes to that child's principal |
//! | desktop | none — `emit` is a no-op |
//!
//! **Payload (W4).** `{"v":1,"k":<kind>,"r":<opaque ref>}`, padded to a
//! fixed 192 bytes ([`payload`]). No titles, bodies, tool names, paths or
//! principal ids ever reach a push service; the app fetches details after
//! the tap.

pub mod crypto;
pub mod events;
pub mod hub;
#[cfg(test)]
mod hub_tests;
pub mod outbox;
#[cfg(target_os = "linux")]
pub mod pump;
pub mod rpc;
pub mod sender;
pub mod store;
pub mod vapid;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use crate::executor::PrincipalId;

/// Every event kind a push can carry (W3). The wire spelling is
/// [`PushKind::as_str`]; the service worker's titles are fixed per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushKind {
    Permission,
    RunFinished,
    RunFailed,
    RunCancelled,
    Invite,
    Pairing,
    Update,
    /// `access_push_test`: never user-selectable, always delivered.
    Test,
}

impl PushKind {
    pub const ALL: [PushKind; 8] = [
        PushKind::Permission,
        PushKind::RunFinished,
        PushKind::RunFailed,
        PushKind::RunCancelled,
        PushKind::Invite,
        PushKind::Pairing,
        PushKind::Update,
        PushKind::Test,
    ];

    /// The kinds a subscription may select (everything but `test`).
    pub const SELECTABLE: [PushKind; 7] = [
        PushKind::Permission,
        PushKind::RunFinished,
        PushKind::RunFailed,
        PushKind::RunCancelled,
        PushKind::Invite,
        PushKind::Pairing,
        PushKind::Update,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            PushKind::Permission => "permission",
            PushKind::RunFinished => "run_finished",
            PushKind::RunFailed => "run_failed",
            PushKind::RunCancelled => "run_cancelled",
            PushKind::Invite => "invite",
            PushKind::Pairing => "pairing",
            PushKind::Update => "update",
            PushKind::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<PushKind> {
        PushKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The push service `TTL` header, in seconds: how long an undelivered
    /// message is worth keeping. A permission ask passes its own remaining
    /// life ([`PushEvent::ttl`]), capped at this.
    pub const fn ttl_secs(self) -> u32 {
        match self {
            PushKind::Permission | PushKind::Pairing => 600,
            PushKind::RunFinished | PushKind::RunFailed | PushKind::RunCancelled => 86_400,
            PushKind::Invite => 259_200,
            PushKind::Update => 604_800,
            PushKind::Test => 300,
        }
    }

    /// The `Urgency` header (RFC 8030 §5.3).
    pub const fn urgency(self) -> &'static str {
        match self {
            PushKind::Permission | PushKind::Pairing => "high",
            PushKind::Update => "low",
            _ => "normal",
        }
    }

    /// The `Topic` header: a device that comes back online gets only the
    /// latest message of each kind. RFC 8030 limits it to 32 base64url
    /// characters; every kind slug fits.
    pub fn topic(self) -> &'static str {
        self.as_str()
    }
}

/// Max length of an opaque ref.
pub const REF_MAX: usize = 64;

/// `r` is opaque, ≤64, `[A-Za-z0-9:_-]` — checked here and in the service
/// worker, so a ref can never carry text or a URL.
pub fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= REF_MAX
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
}

/// The fixed plaintext size of every push record (W4): the same length for
/// every kind, so the ciphertext size says nothing about the event.
pub const PAYLOAD_LEN: usize = 192;

/// The W4 payload: `{"v":1,"k":…,"r":…}`. [`crypto`] pads it to
/// [`PAYLOAD_LEN`]. `None` for an invalid ref.
pub fn payload(kind: PushKind, r: &str) -> Option<Vec<u8>> {
    if !valid_ref(r) {
        return None;
    }
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        v: u8,
        k: &'a str,
        r: &'a str,
    }
    let bytes = serde_json::to_vec(&Wire {
        v: 1,
        k: kind.as_str(),
        r,
    })
    .ok()?;
    // `- 1` for the RFC 8188 padding delimiter.
    (bytes.len() < PAYLOAD_LEN).then_some(bytes)
}

/// One event to push to one principal's devices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEvent {
    /// `None` = "this process's principal": the T0 owner, or (from a T1
    /// child) the child's principal, which the broker fills in.
    pub principal: Option<PrincipalId>,
    pub kind: PushKind,
    pub r: String,
    /// A shorter TTL than the kind's (a permission ask about to expire).
    pub ttl: Option<u32>,
}

impl PushEvent {
    pub fn new(principal: Option<PrincipalId>, kind: PushKind, r: impl Into<String>) -> Self {
        Self {
            principal,
            kind,
            r: r.into(),
            ttl: None,
        }
    }

    /// The TTL actually sent: the kind's, or a shorter override (never 0).
    pub fn ttl(&self) -> u32 {
        let cap = self.kind.ttl_secs();
        self.ttl.map_or(cap, |t| t.clamp(1, cap))
    }
}

/// Where [`emit`] goes in this process.
#[derive(Clone)]
enum Sink {
    Hub(Arc<hub::PushHub>),
    Outbox(Arc<outbox::Outbox>),
}

static SINK: OnceLock<Sink> = OnceLock::new();

/// Install the hub (T0 daemon, T1 broker). Once per process.
pub fn install_hub(hub: Arc<hub::PushHub>) {
    if SINK.set(Sink::Hub(hub)).is_err() {
        tracing::warn!("push: a sink was already installed; keeping the first");
    }
}

/// Install the outbox (T1 principal child). Once per process.
pub fn install_outbox(outbox: Arc<outbox::Outbox>) {
    if SINK.set(Sink::Outbox(outbox)).is_err() {
        tracing::warn!("push: a sink was already installed; keeping the first");
    }
}

/// The installed hub, if this process is a store owner.
pub fn hub() -> Option<Arc<hub::PushHub>> {
    match SINK.get() {
        Some(Sink::Hub(h)) => Some(h.clone()),
        _ => None,
    }
}

/// The installed outbox, if this process is a T1 principal child.
pub fn outbox() -> Option<Arc<outbox::Outbox>> {
    match SINK.get() {
        Some(Sink::Outbox(o)) => Some(o.clone()),
        _ => None,
    }
}

/// Whether this process has a sink at all (so a producer can skip work).
pub fn active() -> bool {
    SINK.get().is_some()
}

/// The one producer entry point. Never blocks, never fails the caller: with
/// no sink (the desktop, tests) it does nothing.
pub fn emit(event: PushEvent) {
    if !valid_ref(&event.r) {
        tracing::warn!(
            "push: dropped a {} event with an invalid ref",
            event.kind.as_str()
        );
        return;
    }
    if event.kind == PushKind::RunCancelled && !CANCELLED.first(&event.r) {
        return;
    }
    match SINK.get() {
        Some(Sink::Hub(h)) => h.emit(event),
        Some(Sink::Outbox(o)) => o.push(event.kind, &event.r),
        None => {}
    }
}

/// `run_cancelled` fires from two places (`chi_exec::cancel_run` and an
/// engine reporting `cancelled`); this keeps one per run.
struct RecentRefs {
    seen: Mutex<VecDeque<String>>,
}

impl RecentRefs {
    const CAP: usize = 256;

    const fn new() -> Self {
        Self {
            seen: Mutex::new(VecDeque::new()),
        }
    }

    /// True the first time `r` is seen (within the last [`Self::CAP`]).
    fn first(&self, r: &str) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.iter().any(|s| s == r) {
            return false;
        }
        if seen.len() >= Self::CAP {
            seen.pop_front();
        }
        seen.push_back(r.to_string());
        true
    }
}

static CANCELLED: RecentRefs = RecentRefs::new();

/// `run_cancelled` for a run id (both producer sites call this).
pub fn emit_run_cancelled(run_id: &str) {
    if !active() {
        return;
    }
    emit(PushEvent::new(
        None,
        PushKind::RunCancelled,
        format!("run:{run_id}"),
    ));
}

/// "A device is asking to pair" (`pairing::Registry` → `awaiting_host`):
/// to the code's beginner, whose admin-strength devices get it.
pub fn emit_pairing(beginner: &str, pairing_id: &str) {
    if !active() {
        return;
    }
    let Ok(principal) = beginner.parse::<PrincipalId>() else {
        return;
    };
    emit(PushEvent::new(
        Some(principal),
        PushKind::Pairing,
        format!("pair:{pairing_id}"),
    ));
}

/// The pairing hook every store owner installs on its registry.
pub fn pairing_hook() -> crate::access::pairing::OnAwaiting {
    Arc::new(|beginner: &str, pairing_id: &str| emit_pairing(beginner, pairing_id))
}

/// Daemon flags (`ikenga-server --no-push`, `--push-contact`,
/// `--push-endpoint-host`, `--push-allow-endpoint`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushOptions {
    /// `--no-push` / `IKENGA_PUSH=off`: no hub, no outbound traffic;
    /// `access_push_config` reports `enabled: false`.
    pub disabled: bool,
    /// `--push-contact` / `IKENGA_PUSH_CONTACT`: the VAPID `sub` claim.
    pub contact: Option<String>,
    /// `--push-endpoint-host <suffix>` (repeatable): more push-service hosts.
    pub endpoint_hosts: Vec<String>,
    /// `--push-allow-endpoint <origin>` (repeatable): one exact origin, http
    /// loopback included — for a local end-to-end test. Warned at boot.
    pub allow_endpoints: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_selectable_excludes_test() {
        for k in PushKind::ALL {
            assert_eq!(PushKind::parse(k.as_str()), Some(k));
            assert!(k.topic().len() <= 32);
        }
        assert!(!PushKind::SELECTABLE.contains(&PushKind::Test));
        assert_eq!(PushKind::parse("bogus"), None);
    }

    #[test]
    fn refs_are_opaque_tokens_only() {
        for ok in [
            "n:12",
            "run:0192f0a4-1b2c-7d3e-8f40-0123456789ab",
            "pair:abc_DEF-9",
            "update",
        ] {
            assert!(valid_ref(ok), "{ok}");
        }
        for bad in [
            "",
            "n 1",
            "https://x",
            "a/b",
            "é",
            &"a".repeat(65),
            "<script>",
        ] {
            assert!(!valid_ref(bad), "{bad}");
        }
    }

    /// W4: exactly `{v, k, r}`, nothing else, and short enough to pad.
    #[test]
    fn payload_is_minimal() {
        for k in PushKind::ALL {
            let p = payload(k, "n:1").unwrap();
            let v: serde_json::Value = serde_json::from_slice(&p).unwrap();
            let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
            keys.sort();
            assert_eq!(keys, vec!["k", "r", "v"]);
            assert_eq!(v["v"], 1);
            assert_eq!(v["k"], k.as_str());
        }
        assert!(payload(PushKind::Permission, "has space").is_none());
    }

    #[test]
    fn ttl_override_never_exceeds_the_kind() {
        let mut e = PushEvent::new(None, PushKind::Permission, "n:1");
        assert_eq!(e.ttl(), 600);
        e.ttl = Some(42);
        assert_eq!(e.ttl(), 42);
        e.ttl = Some(9_999);
        assert_eq!(e.ttl(), 600);
        e.ttl = Some(0);
        assert_eq!(e.ttl(), 1);
    }

    #[test]
    fn recent_refs_dedupe() {
        let r = RecentRefs::new();
        assert!(r.first("run:a"));
        assert!(!r.first("run:a"));
        assert!(r.first("run:b"));
    }
}
