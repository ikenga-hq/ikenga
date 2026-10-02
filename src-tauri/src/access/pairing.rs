//! Device pairing (G-ACCESS §3.1–§3.8, DEC-77): the in-memory pairing
//! registry, the `access_pair_*` arms, and the bodies of the public
//! `/access/pair/{hello,confirm,status}` endpoints (`access::http`).
//!
//! ```text
//! host: access_pair_begin ──► open ──hello(msgA)──► exchanged ──confirm(ok)──► awaiting_host ──decide(allow)──► allowed ─► delivered
//!                               │                       │                          │            └─decide(deny)──► denied
//!                               │                       └─confirm(bad) / 2nd hello─► burned
//!                               ├─ 10 min from begin (any non-final state) ──────► expired
//!                               └─ access_pair_cancel / host starts a new code ──► cancelled
//! ```
//!
//! * Sessions live **in memory only** (T0 daemon / T1 broker). The code is
//!   never written to disk, logged or audited (A-12); only audit rows
//!   persist. Each open session holds an [`super::KeepAlive`], so the
//!   desktop's daemon never idles out mid-window (§2.5, M-3).
//! * **Codes** (§3.2, P-5/P-6): Crockford base32 minus `I L O U`, 6 symbols
//!   shown `XXX-XXX`. Symbol 1 is the public **slot**; symbols 2–6 are 25
//!   secret bits. ≤8 open sessions per host, 1 per beginner.
//! * **One exchange per session** (§3.7, A-8): a second `hello` on a slot
//!   past `open`, or a bad `device_confirm`, burns it. Every failure answers
//!   the same `404 pair_failed`.
//! * **Throttle** (§3.7, A-11): 5 failures per address per 10 min →
//!   `429 throttled`, escalating by [`PAIR_BACKOFF_STEPS_MS`]; 30 per host
//!   per hour → every slot paused 15 min, open sessions burned.
//! * **No credential exists before `decide(allow)`** (A-9): the device row
//!   and its secret are minted there, in one transaction with
//!   `pair.allowed`; the token waits in memory for the first `status` poll
//!   (cookie, or the body for `?client=cli`), then the session ends (`410`).
//!
//! All times are unix milliseconds; the registry's clock is injectable
//! (A-10).

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use rand::Rng;
use serde_json::{json, Value};
use sqlx::Connection;
use zeroize::Zeroize;

use super::audit::{AuditVia, Event};
use super::caps::Tier;
use super::ctx::AccessCtx;
use super::devices::{self, DeviceView};
use super::rpc::Env;
use super::spake::{self, Keys};
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code, KeepAlive};

/// The code alphabet (P-5): Crockford base32 without `I L O U`.
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
pub const CODE_LEN: usize = 6;
/// Sessions expire 10 minutes after begin, in any non-final state (A-10).
pub const PAIR_TTL_MS: i64 = 10 * 60 * 1000;
/// A final session is kept this long past its expiry, so a late status poll
/// still reads its outcome; then it is purged.
pub const RETAIN_FINAL_MS: i64 = 10 * 60 * 1000;
/// An allowed (or deciding) session outlives the 10-minute begin TTL by
/// this much, so a grant decided at 9:59 still reaches the device's poll
/// (every 1.5 s). A grant still undelivered after it is revoked (review m1).
pub const DELIVERY_GRACE_MS: i64 = 2 * 60 * 1000;
/// §3.1 host-wide cap.
pub const MAX_OPEN_PER_HOST: usize = 8;
/// §3.7: copies `commands/app_lock.rs` `BACKOFF_STEPS_MS` (30 s, 1 min,
/// 5 min, 15 min) — copied, not imported: `commands` is desktop-gated.
pub const PAIR_BACKOFF_STEPS_MS: [i64; 4] = [30_000, 60_000, 300_000, 900_000];
pub const ADDR_FAILURES: usize = 5;
pub const ADDR_WINDOW_MS: i64 = 10 * 60 * 1000;
pub const HOST_FAILURES: usize = 30;
pub const HOST_WINDOW_MS: i64 = 60 * 60 * 1000;
pub const HOST_PAUSE_MS: i64 = 15 * 60 * 1000;
/// An address's backoff steps reset after this long without a trip.
pub const STRIKE_DECAY_MS: i64 = 60 * 60 * 1000;
pub const DEVICE_NAME_MAX: usize = 64;
pub const PLATFORM_MAX: usize = 32;
/// The device's status-poll proof (§3.5 `poll_key`).
pub const POLL_HEADER: &str = "x-ikenga-pair-poll";
/// The uniform failure copy (§3.7: every failure looks the same).
pub const PAIR_FAILED_MESSAGE: &str = "That code didn't work. Ask for a new one on the computer.";

fn symbol_index(c: u8) -> Option<usize> {
    ALPHABET.iter().position(|&a| a == c)
}

/// §3.2 input normalization: accept lowercase, map `O`→`0` and `I`/`L`→`1`,
/// strip `-` and whitespace. `None` unless exactly 6 alphabet symbols remain.
pub fn normalize_code(input: &str) -> Option<String> {
    let mut out = String::with_capacity(CODE_LEN);
    for ch in input.chars() {
        if ch == '-' || ch.is_whitespace() {
            continue;
        }
        let c = match ch.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || symbol_index(c as u8).is_none() {
            return None;
        }
        out.push(c);
    }
    (out.len() == CODE_LEN).then_some(out)
}

/// `XXX-XXX` (D-05's `K7P-42Q` shape).
pub fn display_code(normalized: &str) -> String {
    if normalized.len() == CODE_LEN {
        format!("{}-{}", &normalized[..3], &normalized[3..])
    } else {
        normalized.to_string()
    }
}

/// A single slot symbol from a device (`hello`), normalized like a code.
fn normalize_slot(input: &str) -> Option<usize> {
    let mut chars = input.trim().chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let c = match ch.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    };
    c.is_ascii().then(|| symbol_index(c as u8)).flatten()
}

/// Unicode `General_Category = Cf` (format) characters: bidi embeddings,
/// overrides and isolates, zero-width characters, the BOM, invisible
/// operators, tags. They render invisibly or reorder text, so a name
/// carrying them could spoof the pair-confirm "Device" row (review m4).
fn is_format_char(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

/// `deviceName` from the client: control characters become spaces, format
/// characters (bidi controls, zero-widths — [`is_format_char`]) are dropped,
/// whitespace collapsed, ≤64 chars (§3.6); empty → "Device".
pub fn sanitize_name(raw: &str, max: usize) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !is_format_char(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clamped: String = cleaned.chars().take(max).collect();
    clamped.trim().to_string()
}

/// The session states (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Exchanged,
    AwaitingHost,
    /// The beginner's `decide` is committing (guards a double decide).
    Deciding,
    /// Decided `allow`; the credential waits for the first status poll.
    Allowed,
    /// The credential was handed over; a later status call gets `410`.
    Delivered,
    Denied,
    Burned,
    Expired,
    Cancelled,
}

impl State {
    pub const fn as_str(self) -> &'static str {
        match self {
            State::Open => "open",
            State::Exchanged => "exchanged",
            State::AwaitingHost | State::Deciding => "awaiting_host",
            State::Allowed => "allowed",
            State::Delivered => "delivered",
            State::Denied => "denied",
            State::Burned => "burned",
            State::Expired => "expired",
            State::Cancelled => "cancelled",
        }
    }

    pub const fn is_final(self) -> bool {
        matches!(
            self,
            State::Delivered | State::Denied | State::Burned | State::Expired | State::Cancelled
        )
    }
}

/// What the device told us at `hello` (shown on `pair-confirm`, §3.6).
#[derive(Debug, Clone)]
struct Hello {
    remote_addr: String,
    device_name: String,
    platform: Option<String>,
    asked_at: i64,
    fingerprint: [&'static str; 4],
}

struct Delivery {
    device_id: String,
    tier: Tier,
    token: String,
}

impl Drop for Delivery {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

struct Session {
    id: String,
    slot: usize,
    /// The normalized code. Memory only; zeroized on drop.
    code: String,
    beginner: String,
    beginner_device: Option<String>,
    began_at: i64,
    expires_at: i64,
    state: State,
    hello: Option<Hello>,
    keys: Option<Keys>,
    delivery: Option<Delivery>,
    /// The beginner has seen this final session (cancel / a new code); it
    /// leaves `access_pair_pending`.
    acknowledged: bool,
    /// Burned by the host-wide pause (§3.7): `access_pair_pending` reports
    /// it as `paused` while the pause lasts (review m3).
    paused: bool,
    keepalive: Option<KeepAlive>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.code.zeroize();
    }
}

impl Session {
    fn finish(&mut self, state: State) {
        debug_assert!(state.is_final());
        self.state = state;
        self.keepalive = None;
        self.delivery = None;
    }
}

/// A `pair.failed` (or other system) audit row the registry asks its async
/// owner to append (§3.7: every failure is audited). Never carries a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditIntent {
    pub reason: &'static str,
    pub pairing_id: Option<String>,
    pub beginner: Option<String>,
    pub remote_addr: Option<String>,
}

#[derive(Default)]
struct AddrState {
    failures: VecDeque<i64>,
    strikes: usize,
    until: i64,
    last_trip: i64,
}

#[derive(Default)]
struct Throttle {
    addrs: HashMap<String, AddrState>,
    host_failures: VecDeque<i64>,
    host_paused_until: i64,
}

/// A grant minted at `decide(allow)` that never reached the device; its
/// async owner revokes the row ([`flush_audits`], review m1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undelivered {
    pub device_id: String,
    pub pairing_id: String,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    throttle: Throttle,
    audits: Vec<AuditIntent>,
    undelivered: Vec<Undelivered>,
}

/// A public-endpoint refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    /// The uniform `404 pair_failed`.
    PairFailed,
    /// `429 throttled` with `retry_after_ms`.
    Throttled { retry_after_ms: i64 },
    /// `410 gone`: the credential was already delivered.
    Gone,
}

/// `access_pair_begin`'s ticket (before the URL is attached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub pairing_id: String,
    /// Normalized (`K7P42Q`).
    pub code: String,
    pub expires_at: i64,
    /// The beginner's sessions this begin cancelled (audited
    /// `pair.cancelled {reason: replaced}`).
    pub replaced: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloOk {
    pub pairing_id: String,
    pub msg_b: Vec<u8>,
    pub host_confirm: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusOut {
    /// Any non-delivering state (`awaiting_host`, `denied`, `burned`, …).
    State(&'static str),
    Allowed {
        device_id: String,
        tier: Tier,
        token: String,
    },
}

/// What `decide` needs from the session.
#[derive(Debug, Clone)]
pub struct DecideInfo {
    pub pairing_id: String,
    pub beginner: String,
    pub beginner_device: Option<String>,
    pub device_name: String,
    pub platform: Option<String>,
    pub remote_addr: String,
}

type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// `access::pairing::Registry` (§3.1): every open pairing session of this
/// T0 daemon or T1 broker.
pub struct Registry {
    inner: Mutex<Inner>,
    clock: Clock,
}

impl Default for Registry {
    fn default() -> Self {
        Self::with_clock(Arc::new(devices::now_ms))
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("pairing::Registry")
    }
}

impl Registry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn with_clock(clock: Clock) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            clock,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn now(&self) -> i64 {
        (self.clock)()
    }

    /// Open (non-final) sessions — what keeps the daemon alive.
    pub fn open_count(&self) -> usize {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        inner
            .sessions
            .values()
            .filter(|s| !s.state.is_final())
            .count()
    }

    /// Drain the audit rows the registry produced (the async owner appends
    /// them: [`flush_audits`]).
    pub fn take_audits(&self) -> Vec<AuditIntent> {
        std::mem::take(&mut self.lock().audits)
    }

    /// Expire due sessions and purge old final ones (the sweeper's tick).
    pub fn tick(&self) {
        let now = self.now();
        sweep(&mut self.lock(), now);
    }

    /// `access_pair_begin` (§3.1): a fresh code on a free slot. Cancels the
    /// beginner's previous non-final session ("New code · the old one is
    /// dead") and drops their unacknowledged final ones from pending.
    pub fn begin(
        &self,
        beginner: &str,
        beginner_device: Option<&str>,
    ) -> Result<Ticket, AccessError> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        if inner.throttle.host_paused_until > now {
            return Err(paused(inner.throttle.host_paused_until - now));
        }
        let mut replaced = Vec::new();
        for s in inner.sessions.values_mut() {
            if s.beginner != beginner {
                continue;
            }
            // A request being decided, or allowed and waiting for its first
            // poll, is no longer an open code: it finishes on its own
            // (review m1 — cancelling it would orphan the minted grant).
            if matches!(s.state, State::Deciding | State::Allowed) {
                continue;
            }
            if !s.state.is_final() {
                s.finish(State::Cancelled);
                replaced.push(s.id.clone());
            }
            s.acknowledged = true;
        }
        let held: Vec<usize> = inner
            .sessions
            .values()
            .filter(|s| !s.state.is_final())
            .map(|s| s.slot)
            .collect();
        if held.len() >= MAX_OPEN_PER_HOST {
            return Err(AccessError::new(
                Code::Conflict,
                "too many open pairing codes on this host — cancel one first",
            ));
        }
        let free: Vec<usize> = (0..ALPHABET.len()).filter(|i| !held.contains(i)).collect();
        let mut rng = rand::rngs::OsRng;
        let slot = free[rng.gen_range(0..free.len())];
        let mut code = String::with_capacity(CODE_LEN);
        code.push(ALPHABET[slot] as char);
        for _ in 1..CODE_LEN {
            code.push(ALPHABET[rng.gen_range(0..ALPHABET.len())] as char);
        }
        let id = devices::new_id();
        let ticket = Ticket {
            pairing_id: id.clone(),
            code: code.clone(),
            expires_at: now + PAIR_TTL_MS,
            replaced,
        };
        inner.sessions.insert(
            id.clone(),
            Session {
                id,
                slot,
                code,
                beginner: beginner.to_string(),
                beginner_device: beginner_device.map(str::to_string),
                began_at: now,
                expires_at: now + PAIR_TTL_MS,
                state: State::Open,
                hello: None,
                keys: None,
                delivery: None,
                acknowledged: false,
                paused: false,
                keepalive: Some(KeepAlive::hold()),
            },
        );
        Ok(ticket)
    }

    /// Undo a `begin` whose `pair.started` row could not be appended.
    pub fn abort_begin(&self, pairing_id: &str) {
        self.lock().sessions.remove(pairing_id);
    }

    /// `access_pair_cancel`: the beginner only. A non-final session becomes
    /// `cancelled` (`Ok(true)`, audited by the caller); a final one is just
    /// acknowledged (`Ok(false)`).
    pub fn cancel(&self, pairing_id: &str, beginner: &str) -> Result<bool, AccessError> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        let s = inner
            .sessions
            .get_mut(pairing_id)
            .filter(|s| s.beginner == beginner)
            .ok_or_else(not_found)?;
        s.acknowledged = true;
        if s.state.is_final() || s.state == State::Allowed {
            return Ok(false);
        }
        if s.state == State::Deciding {
            return Err(AccessError::new(
                Code::Conflict,
                "this request is being decided",
            ));
        }
        s.finish(State::Cancelled);
        Ok(true)
    }

    /// `access_pair_pending` (§3.6): the beginner's requests awaiting the
    /// host, and burned codes the host hasn't acknowledged (§3.7).
    pub fn pending(&self, beginner: &str) -> Vec<Value> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        let paused_for = (inner.throttle.host_paused_until - now).max(0);
        let mut rows: Vec<(&Session, i64)> = inner
            .sessions
            .values()
            .filter(|s| s.beginner == beginner && !s.acknowledged)
            .filter(|s| {
                matches!(
                    s.state,
                    State::AwaitingHost | State::Deciding | State::Burned
                )
            })
            .map(|s| (s, s.hello.as_ref().map_or(s.began_at, |h| h.asked_at)))
            .collect();
        rows.sort_by_key(|(_, at)| *at);
        rows.into_iter()
            .map(|(s, asked_at)| {
                let hello = s.hello.as_ref();
                json!({
                    "pairingId": s.id,
                    "deviceName": hello.map_or("", |h| h.device_name.as_str()),
                    "platform": hello.and_then(|h| h.platform.clone()),
                    "remoteAddr": hello.map_or("", |h| h.remote_addr.as_str()),
                    "askedAt": asked_at,
                    "code": display_code(&s.code),
                    "fingerprint": hello.map_or(["", "", "", ""], |h| h.fingerprint),
                    // §3.7: a code burned by the host-wide pause reads
                    // `paused` (with the wait) while the pause lasts.
                    "state": match s.state {
                        State::Burned if s.paused && paused_for > 0 => "paused",
                        State::Burned => "burned",
                        _ => "awaiting_host",
                    },
                    "retryAfterMs": (s.paused && paused_for > 0).then_some(paused_for),
                })
            })
            .collect()
    }

    /// `access_pair_decide` step 1: `awaiting_host` → `deciding`, for the
    /// beginner only.
    pub fn begin_decide(
        &self,
        pairing_id: &str,
        beginner: &str,
    ) -> Result<DecideInfo, AccessError> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        let s = inner
            .sessions
            .get_mut(pairing_id)
            .filter(|s| s.beginner == beginner)
            .ok_or_else(not_found)?;
        match s.state {
            State::AwaitingHost => {}
            State::Expired => {
                return Err(AccessError::new(
                    Code::Expired,
                    "this pairing request expired",
                ))
            }
            State::Deciding => {
                return Err(AccessError::new(
                    Code::Conflict,
                    "this request is being decided",
                ))
            }
            _ => {
                return Err(AccessError::new(
                    Code::Gone,
                    "this pairing request is no longer waiting",
                ))
            }
        }
        let hello = s.hello.clone().ok_or_else(not_found)?;
        s.state = State::Deciding;
        Ok(DecideInfo {
            pairing_id: s.id.clone(),
            beginner: s.beginner.clone(),
            beginner_device: s.beginner_device.clone(),
            device_name: hello.device_name,
            platform: hello.platform,
            remote_addr: hello.remote_addr,
        })
    }

    /// `access_pair_decide` step 2 (allow committed): hold the credential
    /// for the first status poll (until [`DELIVERY_GRACE_MS`] past the begin
    /// TTL, [`sweep`]). Only
    /// from `deciding`; `false` when the session is gone (the caller then
    /// revokes the minted row — never a grant nobody holds, review m1).
    #[must_use]
    pub fn finish_allow(
        &self,
        pairing_id: &str,
        device_id: &str,
        tier: Tier,
        token: String,
    ) -> bool {
        let mut inner = self.lock();
        let Some(s) = inner
            .sessions
            .get_mut(pairing_id)
            .filter(|s| s.state == State::Deciding)
        else {
            let mut token = token;
            token.zeroize();
            return false;
        };
        s.state = State::Allowed;
        s.acknowledged = true;
        s.delivery = Some(Delivery {
            device_id: device_id.to_string(),
            tier,
            token,
        });
        true
    }

    /// `access_pair_decide` step 2 (deny committed). Only from `deciding`.
    pub fn finish_deny(&self, pairing_id: &str) {
        let mut inner = self.lock();
        if let Some(s) = inner
            .sessions
            .get_mut(pairing_id)
            .filter(|s| s.state == State::Deciding)
        {
            s.acknowledged = true;
            s.finish(State::Denied);
        }
    }

    /// The decide transaction failed: back to `awaiting_host`.
    pub fn revert_decide(&self, pairing_id: &str) {
        let mut inner = self.lock();
        if let Some(s) = inner
            .sessions
            .get_mut(pairing_id)
            .filter(|s| s.state == State::Deciding)
        {
            s.state = State::AwaitingHost;
        }
    }

    /// `POST /access/pair/hello` (§3.4): run the host side of SPAKE2 for the
    /// session on `slot`.
    #[allow(clippy::too_many_arguments)]
    pub fn hello(
        &self,
        slot: &str,
        msg_a: &str,
        device_name: &str,
        platform: Option<&str>,
        remote_addr: &str,
        store_id: &str,
    ) -> Result<HelloOk, Fail> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        throttle_check(&inner, remote_addr, now)?;
        let Some(slot) = normalize_slot(slot) else {
            return Err(failure(&mut inner, remote_addr, now, "wrong_code", None));
        };
        let found = inner
            .sessions
            .values()
            .find(|s| s.slot == slot && !s.state.is_final())
            .map(|s| s.id.clone());
        let Some(id) = found else {
            // A free slot: the same body as every other failure. If the slot
            // last belonged to a session that expired, say so in the audit.
            let expired = inner
                .sessions
                .values()
                .filter(|s| s.slot == slot && s.state == State::Expired)
                .max_by_key(|s| s.began_at)
                .map(|s| s.id.clone());
            let reason = if expired.is_some() {
                "expired"
            } else {
                "wrong_code"
            };
            return Err(failure(&mut inner, remote_addr, now, reason, expired));
        };
        let state = inner.sessions[&id].state;
        match state {
            State::Open => {}
            State::Exchanged | State::AwaitingHost => {
                // §3.7 "failed or burned exchanges per address": an
                // exchange that never confirmed is charged to the address
                // that made it, as well as to this second sender (review
                // M2 — else an attacker who hellos and walks away to test
                // a guess offline is never counted).
                let exchanger = inner
                    .sessions
                    .get(&id)
                    .filter(|s| s.state == State::Exchanged)
                    .and_then(|s| s.hello.as_ref())
                    .map(|h| h.remote_addr.clone());
                if let Some(s) = inner.sessions.get_mut(&id) {
                    s.finish(State::Burned);
                }
                if let Some(addr) = exchanger {
                    count_failure(&mut inner, &addr, now);
                }
                return Err(failure(
                    &mut inner,
                    remote_addr,
                    now,
                    "second_hello",
                    Some(id),
                ));
            }
            // Past the host's decision: refuse, but never burn a grant that
            // was already issued.
            _ => {
                return Err(failure(
                    &mut inner,
                    remote_addr,
                    now,
                    "second_hello",
                    Some(id),
                ))
            }
        }
        let reply = spake::unb64(msg_a).and_then(|m| {
            let code = inner.sessions[&id].code.clone();
            let r = spake::host_reply(&code, store_id, &id, &m).ok();
            let mut code = code;
            code.zeroize();
            r
        });
        let Some(reply) = reply else {
            if let Some(s) = inner.sessions.get_mut(&id) {
                s.finish(State::Burned);
            }
            return Err(failure(
                &mut inner,
                remote_addr,
                now,
                "wrong_code",
                Some(id),
            ));
        };
        let host_confirm = reply.keys.host_confirm();
        let name = sanitize_name(device_name, DEVICE_NAME_MAX);
        let platform = platform
            .map(|p| sanitize_name(p, PLATFORM_MAX))
            .filter(|p| !p.is_empty());
        let s = inner.sessions.get_mut(&id).expect("session present");
        s.state = State::Exchanged;
        s.hello = Some(Hello {
            remote_addr: remote_addr.to_string(),
            device_name: if name.is_empty() {
                "Device".to_string()
            } else {
                name
            },
            platform,
            asked_at: now,
            fingerprint: reply.keys.fingerprint(),
        });
        s.keys = Some(reply.keys);
        Ok(HelloOk {
            pairing_id: id,
            msg_b: reply.msg_b,
            host_confirm,
        })
    }

    /// `POST /access/pair/confirm` (§3.5): the device's key confirmation. A
    /// bad one burns the session.
    pub fn confirm(
        &self,
        pairing_id: &str,
        device_confirm: &str,
        remote_addr: &str,
    ) -> Result<(), Fail> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        throttle_check(&inner, remote_addr, now)?;
        let Some(state) = inner.sessions.get(pairing_id).map(|s| s.state) else {
            return Err(failure(&mut inner, remote_addr, now, "wrong_code", None));
        };
        match state {
            State::Exchanged => {}
            State::Expired => {
                return Err(failure(
                    &mut inner,
                    remote_addr,
                    now,
                    "expired",
                    Some(pairing_id.to_string()),
                ))
            }
            // A replay against a session already confirmed, decided or dead:
            // refused, counted, not burned.
            _ => {
                count_failure(&mut inner, remote_addr, now);
                return Err(Fail::PairFailed);
            }
        }
        let ok = spake::unb64(device_confirm).is_some_and(|presented| {
            inner.sessions[pairing_id]
                .keys
                .as_ref()
                .is_some_and(|k| k.verify_device_confirm(&presented))
        });
        if !ok {
            if let Some(s) = inner.sessions.get_mut(pairing_id) {
                s.finish(State::Burned);
            }
            return Err(failure(
                &mut inner,
                remote_addr,
                now,
                "wrong_code",
                Some(pairing_id.to_string()),
            ));
        }
        if let Some(s) = inner.sessions.get_mut(pairing_id) {
            s.state = State::AwaitingHost;
        }
        Ok(())
    }

    /// `GET /access/pair/status` (§3.8): the device's poll, proven with
    /// `poll_key`. The first poll after `allow` takes the credential.
    pub fn status(
        &self,
        pairing_id: &str,
        poll_key: &str,
        remote_addr: &str,
    ) -> Result<StatusOut, Fail> {
        let now = self.now();
        let mut inner = self.lock();
        sweep(&mut inner, now);
        // A proven poll is no guess: it is exempt from the throttle, so a
        // device whose request was allowed still collects its credential
        // during a pause (review m3). Only unproven polls are throttled and
        // counted.
        let proven = spake::unb64(poll_key).is_some_and(|presented| {
            inner
                .sessions
                .get(pairing_id)
                .and_then(|s| s.keys.as_ref())
                .is_some_and(|k| k.verify_poll_key(&presented))
        });
        if !proven {
            throttle_check(&inner, remote_addr, now)?;
            count_failure(&mut inner, remote_addr, now);
            return Err(Fail::PairFailed);
        }
        let s = inner.sessions.get_mut(pairing_id).expect("proven above");
        match s.state {
            State::Delivered => Err(Fail::Gone),
            State::Allowed => {
                let Some(mut d) = s.delivery.take() else {
                    return Err(Fail::Gone);
                };
                let out = StatusOut::Allowed {
                    device_id: d.device_id.clone(),
                    tier: d.tier,
                    token: std::mem::take(&mut d.token),
                };
                s.finish(State::Delivered);
                Ok(out)
            }
            other => Ok(StatusOut::State(other.as_str())),
        }
    }

    #[cfg(test)]
    fn state_of(&self, pairing_id: &str) -> Option<State> {
        self.lock().sessions.get(pairing_id).map(|s| s.state)
    }
}

fn not_found() -> AccessError {
    AccessError::new(Code::NotFound, "no such pairing request")
}

fn paused(retry_after_ms: i64) -> AccessError {
    AccessError::new(
        Code::Throttled,
        format!(
            "Too many wrong codes — pairing paused for 15 min. retry_after_ms={retry_after_ms}"
        ),
    )
}

/// Expire due sessions (§3.1, any non-final state, 10 min from begin) and
/// purge final ones past their retention.
fn sweep(inner: &mut Inner, now: i64) {
    let mut audits = Vec::new();
    let mut abandoned = Vec::new();
    for s in inner.sessions.values_mut() {
        if s.state.is_final() {
            continue;
        }
        // `deciding` / `allowed` get the delivery grace past the begin TTL,
        // so a grant decided at 9:59 still reaches the device (review m1).
        let due = if matches!(s.state, State::Deciding | State::Allowed) {
            s.expires_at + DELIVERY_GRACE_MS
        } else {
            s.expires_at
        };
        if now < due {
            continue;
        }
        if let Some(d) = s.delivery.as_ref() {
            inner.undelivered.push(Undelivered {
                device_id: d.device_id.clone(),
                pairing_id: s.id.clone(),
            });
        }
        if s.state == State::Exchanged {
            // An exchange that never confirmed is a burned exchange for the
            // address that made it (§3.7, review M2).
            if let Some(h) = s.hello.as_ref() {
                abandoned.push(h.remote_addr.clone());
            }
        }
        s.finish(State::Expired);
        audits.push(AuditIntent {
            reason: "expired",
            pairing_id: Some(s.id.clone()),
            beginner: Some(s.beginner.clone()),
            remote_addr: s.hello.as_ref().map(|h| h.remote_addr.clone()),
        });
    }
    inner.audits.extend(audits);
    for addr in abandoned {
        count_failure(inner, &addr, now);
    }
    inner
        .sessions
        .retain(|_, s| !(s.state.is_final() && now >= s.expires_at + RETAIN_FINAL_MS));
    let t = &mut inner.throttle;
    t.addrs.retain(|_, a| {
        while a
            .failures
            .front()
            .is_some_and(|&at| now - at > ADDR_WINDOW_MS)
        {
            a.failures.pop_front();
        }
        a.until > now || !a.failures.is_empty() || now - a.last_trip < STRIKE_DECAY_MS
    });
    while t
        .host_failures
        .front()
        .is_some_and(|&at| now - at > HOST_WINDOW_MS)
    {
        t.host_failures.pop_front();
    }
}

/// The per-address throttle key (review m8): an IPv4 address as is; an
/// IPv6 address by its /64, since one client can rotate freely inside it.
/// Anything else (`unknown`) as is.
pub fn throttle_key(addr: &str) -> String {
    match addr.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(v6)) if v6.to_ipv4_mapped().is_none() => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        Ok(std::net::IpAddr::V6(v6)) => v6
            .to_ipv4_mapped()
            .map_or(addr.to_string(), |v4| v4.to_string()),
        _ => addr.to_string(),
    }
}

fn throttle_check(inner: &Inner, addr: &str, now: i64) -> Result<(), Fail> {
    let t = &inner.throttle;
    let mut until = t.host_paused_until;
    if let Some(a) = t.addrs.get(&throttle_key(addr)) {
        until = until.max(a.until);
    }
    if until > now {
        return Err(Fail::Throttled {
            retry_after_ms: until - now,
        });
    }
    Ok(())
}

/// Count one failure against the address and the host (§3.7). Trips are
/// audited once (`pair.failed {reason: throttled}`), not per request.
fn count_failure(inner: &mut Inner, addr: &str, now: i64) {
    let mut trips: Vec<AuditIntent> = Vec::new();
    {
        let a = inner.throttle.addrs.entry(throttle_key(addr)).or_default();
        if now - a.last_trip > STRIKE_DECAY_MS {
            a.strikes = 0;
        }
        a.failures.push_back(now);
        while a
            .failures
            .front()
            .is_some_and(|&at| now - at > ADDR_WINDOW_MS)
        {
            a.failures.pop_front();
        }
        if a.failures.len() >= ADDR_FAILURES {
            let step = PAIR_BACKOFF_STEPS_MS[a.strikes.min(PAIR_BACKOFF_STEPS_MS.len() - 1)];
            a.until = now + step;
            a.strikes += 1;
            a.last_trip = now;
            a.failures.clear();
            trips.push(AuditIntent {
                reason: "throttled",
                pairing_id: None,
                beginner: None,
                remote_addr: Some(addr.to_string()),
            });
        }
    }
    let t = &mut inner.throttle;
    t.host_failures.push_back(now);
    if t.host_failures.len() >= HOST_FAILURES {
        t.host_failures.clear();
        t.host_paused_until = now + HOST_PAUSE_MS;
        for s in inner.sessions.values_mut() {
            if !s.state.is_final() && s.state != State::Allowed && s.state != State::Deciding {
                s.finish(State::Burned);
                s.acknowledged = false;
                s.paused = true;
            }
        }
        trips.push(AuditIntent {
            reason: "throttled",
            pairing_id: None,
            beginner: None,
            remote_addr: None,
        });
    }
    inner.audits.extend(trips);
}

/// [`count_failure`] plus the `pair.failed {reason}` row; returns the
/// uniform refusal.
fn failure(
    inner: &mut Inner,
    addr: &str,
    now: i64,
    reason: &'static str,
    pairing_id: Option<String>,
) -> Fail {
    let beginner = pairing_id
        .as_ref()
        .and_then(|id| inner.sessions.get(id))
        .map(|s| s.beginner.clone());
    inner.audits.push(AuditIntent {
        reason,
        pairing_id,
        beginner,
        remote_addr: Some(addr.to_string()),
    });
    count_failure(inner, addr, now);
    Fail::PairFailed
}

/// Append the registry's queued `pair.failed` rows (not access changes, so
/// a degraded chain still takes them, §6.4). Best-effort: a failed append is
/// logged, never surfaced to the device.
pub async fn flush_audits(registry: &Registry, store: &AccessStore) {
    let undelivered = std::mem::take(&mut registry.lock().undelivered);
    for u in undelivered {
        if let Err(e) = devices::revoke_undelivered(store, &u.device_id, &u.pairing_id).await {
            tracing::warn!("pairing: revoke an undelivered grant: {e:#}");
        }
    }
    let intents = registry.take_audits();
    if intents.is_empty() {
        return;
    }
    let Ok(mut conn) = store.pool().acquire().await else {
        tracing::warn!("pairing: audit flush could not acquire a connection");
        return;
    };
    for intent in intents {
        let mut ev = Event::new("pair.failed", AuditVia::System).detail(json!({
            "reason": intent.reason,
            "pairing_id": intent.pairing_id,
        }));
        ev.remote_addr = intent.remote_addr;
        if let Some(b) = intent.beginner {
            ev = ev.subject_principal(b);
        }
        let res = async {
            let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
            let head = store.chain().append(&mut tx, &ev).await?;
            tx.commit().await?;
            store.chain().committed(head);
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = res {
            tracing::warn!("pairing: pair.failed audit append: {e:#}");
        }
    }
}

/// Expire codes on time (so their keep-alives drop and the daemon can idle
/// out) and flush the audit queue. Ends when the registry is dropped. A
/// no-op outside a Tokio runtime.
pub fn spawn_sweeper(registry: &Arc<Registry>, store: AccessStore) {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let weak: Weak<Registry> = Arc::downgrade(registry);
    handle.spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tick.tick().await;
            let Some(registry) = weak.upgrade() else {
                break;
            };
            registry.tick();
            flush_audits(&registry, &store).await;
        }
    });
}

/// §3.3: the QR / link base, first match wins — `--public-url`; the
/// desktop's `publicBase` (operator bearer only); the request's own `Host`
/// when it is not loopback. `None` when no other device can reach us.
pub fn pair_url(
    public_url: Option<&str>,
    ctx: &AccessCtx,
    public_base: Option<&str>,
) -> Option<String> {
    let base = public_url
        .map(str::to_string)
        .or_else(|| {
            public_base
                .filter(|_| ctx.is_operator())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.meta
                .host
                .as_deref()
                .filter(|h| !is_loopback_host(h))
                .map(|h| format!("{}://{h}", ctx.meta.scheme.as_deref().unwrap_or("http")))
        })?;
    let base = base.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return None;
    }
    Some(format!("{base}/remote/pair"))
}

/// `localhost`, `127.0.0.0/8`, `[::1]` (with or without a port).
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("").to_string()
    } else {
        h.rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(h.clone(), |(n, _)| n.to_string())
    };
    name == "localhost"
        || name.ends_with(".localhost")
        || name == "::1"
        || name
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, AccessError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, format!("`{key}` is required")))
}

fn audit_err(e: super::audit::chain::AppendError) -> AccessError {
    match e {
        super::audit::chain::AppendError::AuditUnavailable(b) => AccessError::new(
            Code::AuditUnavailable,
            format!(
                "the audit chain is broken at #{} — access changes are paused",
                b.broken_at_seq
            ),
        ),
        super::audit::chain::AppendError::Sql(e) => AccessError::internal(e),
    }
}

/// Append one access-change row in its own transaction.
async fn append_one(store: &AccessStore, ev: &Event) -> Result<(), AccessError> {
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let head = store.chain().append(&mut tx, ev).await.map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(())
}

fn refuse_if_degraded(store: &AccessStore) -> Result<(), AccessError> {
    match store.chain().degraded() {
        Some(b) => Err(AccessError::new(
            Code::AuditUnavailable,
            format!(
                "the audit chain is broken at #{} — access changes are paused",
                b.broken_at_seq
            ),
        )),
        None => Ok(()),
    }
}

/// The four `access_pair_*` arms (§9.1).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    let store = env.store.ok_or_else(AccessError::store_unavailable)?;
    let registry = env.pairing.ok_or_else(AccessError::store_unavailable)?;
    let beginner = ctx.principal_id.to_string();
    let out = match cmd {
        "access_pair_begin" => begin(env, store, registry, ctx, args).await,
        "access_pair_cancel" => {
            // §3.1: begin, decide, cancel and pending are admin_strength.
            if !ctx.admin_strength {
                return Err(AccessError::forbidden_admin());
            }
            let id = str_arg(args, "pairingId")?;
            if registry.cancel(id, &beginner)? {
                append_one(
                    store,
                    &Event::by("pair.cancelled", ctx)
                        .subject_principal(beginner.clone())
                        .detail(json!({ "pairing_id": id, "reason": "user" })),
                )
                .await?;
            }
            Ok(json!({}))
        }
        "access_pair_pending" => {
            if !ctx.admin_strength {
                return Err(AccessError::forbidden_admin());
            }
            Ok(Value::Array(registry.pending(&beginner)))
        }
        "access_pair_decide" => decide(store, registry, ctx, args).await,
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    };
    flush_audits(registry, store).await;
    out
}

async fn begin(
    env: &Env<'_>,
    store: &AccessStore,
    registry: &Registry,
    ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    refuse_if_degraded(store)?;
    let beginner = ctx.principal_id.to_string();
    let ticket = registry.begin(&beginner, ctx.device_id.as_deref())?;
    let started = Event::by("pair.started", ctx)
        .subject_principal(beginner.clone())
        .detail(json!({ "pairing_id": ticket.pairing_id, "expires_at": ticket.expires_at }));
    if let Err(e) = append_one(store, &started).await {
        registry.abort_begin(&ticket.pairing_id);
        return Err(e);
    }
    for old in &ticket.replaced {
        let ev = Event::by("pair.cancelled", ctx)
            .subject_principal(beginner.clone())
            .detail(json!({ "pairing_id": old, "reason": "replaced" }));
        if let Err(e) = append_one(store, &ev).await {
            tracing::warn!("pairing: pair.cancelled audit append: {e}");
        }
    }
    let public_base = args.get("publicBase").and_then(Value::as_str);
    let url = pair_url(env.public_url.as_deref(), ctx, public_base);
    // `&h=` pins idB's store id on the device for a scanned code (review
    // m5): the hello reply's `storeId` is then not trusted for the binding.
    let qr = url
        .as_ref()
        .map(|u| format!("{u}#c={}&h={}", ticket.code, store.meta().store_id));
    Ok(json!({
        "pairingId": ticket.pairing_id,
        "code": display_code(&ticket.code),
        "expiresAt": ticket.expires_at,
        "pairUrl": url,
        "qrPayload": qr,
        // Whether the device cookie a device opening `pairUrl` gets carries
        // `Secure`: a browser drops it over plain HTTP off loopback, so the
        // sheet warns when `pairUrl` is `http://` and this is true (review
        // M1). False under `--insecure-cookie`, and on T0 when `pairUrl`'s
        // host is a tailnet address — that device's peer address is then a
        // tailnet one, which gets a non-`Secure` cookie (Round 19,
        // DEC-R19-1). The T1 broker is unchanged.
        "cookieSecure": !(env.insecure_cookie
            || (env.tier == StoreTier::T0
                && url.as_deref().is_some_and(devices::is_tailnet_url))),
    }))
}

async fn decide(
    store: &AccessStore,
    registry: &Registry,
    ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    let id = str_arg(args, "pairingId")?;
    let decision = str_arg(args, "decision")?;
    let tier = match args.get("tier").and_then(Value::as_str) {
        None => Tier::Dispatch,
        // P-9: `full` is set only later, from the Devices table.
        Some(t) => match Tier::parse(t) {
            Some(t) if t != Tier::Full => t,
            _ => {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "tier must be view|dispatch|approve",
                ))
            }
        },
    };
    if !matches!(decision, "allow" | "deny") {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "decision must be allow|deny",
        ));
    }
    refuse_if_degraded(store)?;
    let info = registry.begin_decide(id, &ctx.principal_id.to_string())?;
    let res = if decision == "deny" {
        let ev = Event::by("pair.denied", ctx)
            .subject_principal(info.beginner.clone())
            .target(info.device_name.clone())
            .detail(json!({ "pairing_id": info.pairing_id, "device_addr": info.remote_addr }));
        append_one(store, &ev).await.map(|()| {
            registry.finish_deny(id);
            json!({})
        })
    } else {
        match allow(store, ctx, &info, tier).await {
            Ok((view, token)) => {
                if registry.finish_allow(id, &view.device_id, tier, token) {
                    Ok(json!({ "device": view }))
                } else {
                    // The session ended while the row was being minted: the
                    // token can never be delivered, so the row must not stay
                    // a live grant (review m1).
                    if let Err(e) =
                        devices::revoke_undelivered(store, &view.device_id, &info.pairing_id).await
                    {
                        tracing::warn!("pairing: revoke an undelivered grant: {e:#}");
                    }
                    return Err(AccessError::new(
                        Code::Gone,
                        "this pairing request is no longer waiting",
                    ));
                }
            }
            Err(e) => Err(e),
        }
    };
    if res.is_err() {
        registry.revert_decide(id);
    }
    res
}

/// Mint the device (A-9: the first credential exists here) and audit
/// `pair.allowed`, in one transaction (A-18).
async fn allow(
    store: &AccessStore,
    ctx: &AccessCtx,
    info: &DecideInfo,
    tier: Tier,
) -> Result<(DeviceView, String), AccessError> {
    let minted = devices::mint_secret();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let row = devices::insert_paired(
        &mut tx,
        &info.beginner,
        &info.device_name,
        info.platform.as_deref(),
        tier,
        &minted.sha256,
        info.beginner_device.as_deref(),
        Some(&info.pairing_id),
    )
    .await
    .map_err(AccessError::internal)?;
    let ev = Event::by("pair.allowed", ctx)
        .subject_principal(info.beginner.clone())
        .subject_device(row.device_id.clone())
        .target(row.name.clone())
        .detail(json!({
            "tier": tier.as_str(),
            "pairing_id": info.pairing_id,
            "device_addr": info.remote_addr,
        }));
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    let token = devices::token(&row.device_id, &minted.secret);
    Ok((row.view(0, false), token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::CapSet;
    use crate::access::ctx::{RequestMeta, Via};
    use crate::access::devices::tests::operator_ctx;
    use crate::access::rpc::{Env, PrincipalInfo};
    use crate::access::sockets;
    use crate::access::store::StoreTier;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn manual_clock() -> (Arc<AtomicI64>, Registry) {
        let t = Arc::new(AtomicI64::new(1_700_000_000_000));
        let c = t.clone();
        (
            t,
            Registry::with_clock(Arc::new(move || c.load(Ordering::SeqCst))),
        )
    }

    /// The device side of one run: hello + confirm with the code it typed.
    struct Device {
        pairing_id: String,
        keys: Keys,
    }

    fn device_hello(
        reg: &Registry,
        typed: &str,
        store_id: &str,
        addr: &str,
    ) -> Result<(Device, HelloOk), Fail> {
        let norm = normalize_code(typed).unwrap();
        let (a, msg_a) = spake::device_start_with_rng(&norm, store_id, rand::rngs::OsRng);
        let ok = reg.hello(
            &norm[..1],
            &spake::b64(&msg_a),
            "Pixel 9 · Chrome",
            Some("android"),
            addr,
            store_id,
        )?;
        let key = a.finish(&ok.msg_b).unwrap();
        let keys = Keys::derive(&key, &ok.pairing_id, &msg_a, &ok.msg_b);
        Ok((
            Device {
                pairing_id: ok.pairing_id.clone(),
                keys,
            },
            ok,
        ))
    }

    #[test]
    fn codes_normalize_and_display() {
        assert_eq!(normalize_code("k7p-42q").as_deref(), Some("K7P42Q"));
        assert_eq!(normalize_code(" o1l iab ").as_deref(), Some("0111AB"));
        assert_eq!(normalize_code("K7P-42"), None);
        assert_eq!(normalize_code("K7P-42QQ"), None);
        assert_eq!(normalize_code("K7U-42Q"), None, "U is not in the alphabet");
        assert_eq!(display_code("K7P42Q"), "K7P-42Q");
        assert_eq!(normalize_slot("k"), Some(symbol_index(b'K').unwrap()));
        assert_eq!(normalize_slot("o"), Some(0));
        assert_eq!(normalize_slot("KK"), None);
        assert_eq!(sanitize_name("  Pixel\u{0}\n 9  ", 64), "Pixel 9");
        // Review m4: bidi controls and zero-widths are dropped, not shown.
        assert_eq!(
            sanitize_name("Pixel\u{202E}9\u{200B} \u{2066}Chrome\u{2069}\u{FEFF}", 64),
            "Pixel9 Chrome"
        );
        assert_eq!(sanitize_name("\u{202E}\u{200F}", 64), "");
        assert_eq!(sanitize_name(&"x".repeat(100), 64).len(), 64);
    }

    #[test]
    fn begin_allocates_distinct_slots_and_one_session_per_beginner() {
        let (_, reg) = manual_clock();
        let before = super::super::keepalive_count();
        let a = reg.begin("p-a", None).unwrap();
        assert_eq!(a.code.len(), 6);
        assert!(a.code.bytes().all(|b| symbol_index(b).is_some()));
        // A new code by the same beginner cancels the old one.
        let a2 = reg.begin("p-a", None).unwrap();
        assert_eq!(a2.replaced, vec![a.pairing_id.clone()]);
        assert_eq!(reg.state_of(&a.pairing_id), Some(State::Cancelled));
        assert_eq!(reg.open_count(), 1);
        // Up to 8 open sessions host-wide, each on its own slot.
        let mut slots = vec![a2.code.as_bytes()[0]];
        for i in 0..7 {
            let t = reg.begin(&format!("p-{i}"), None).unwrap();
            assert!(!slots.contains(&t.code.as_bytes()[0]));
            slots.push(t.code.as_bytes()[0]);
        }
        assert_eq!(reg.open_count(), 8);
        assert!(super::super::keepalive_count() >= before + 8);
        assert_eq!(reg.begin("p-9", None).unwrap_err().code, Code::Conflict);
    }

    /// A-8: one exchange per session — a second hello burns it; a bad
    /// confirm burns it; a free slot answers like every other failure.
    #[test]
    fn one_exchange_per_session() {
        let (_, reg) = manual_clock();
        let store_id = "store-1";
        let t = reg.begin("owner", None).unwrap();
        let (dev, _) = device_hello(&reg, &t.code, store_id, "10.0.0.2").unwrap();
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Exchanged));
        // Second hello on the same slot → burned.
        assert_eq!(
            device_hello(&reg, &t.code, store_id, "10.0.0.3").err(),
            Some(Fail::PairFailed)
        );
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Burned));
        // The original device's confirm now fails too.
        assert_eq!(
            reg.confirm(
                &dev.pairing_id,
                &spake::b64(&dev.keys.device_confirm()),
                "10.0.0.2"
            ),
            Err(Fail::PairFailed)
        );
        let pending = reg.pending("owner");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["state"], "burned");

        // A wrong secret part: the exchange "succeeds", the confirm burns.
        let t = reg.begin("owner", None).unwrap();
        let mut wrong = t.code.clone().into_bytes();
        wrong[5] = if wrong[5] == b'Z' { b'Y' } else { b'Z' };
        let (dev, ok) = device_hello(
            &reg,
            std::str::from_utf8(&wrong).unwrap(),
            store_id,
            "10.0.0.4",
        )
        .unwrap();
        assert_ne!(
            ok.host_confirm,
            dev.keys.host_confirm(),
            "the device detects it"
        );
        assert_eq!(
            reg.confirm(
                &dev.pairing_id,
                &spake::b64(&dev.keys.device_confirm()),
                "10.0.0.4"
            ),
            Err(Fail::PairFailed)
        );
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Burned));

        // A free slot: the same refusal.
        let free = (0..32)
            .map(|i| ALPHABET[i] as char)
            .find(|c| {
                reg.lock()
                    .sessions
                    .values()
                    .all(|s| s.state.is_final() || ALPHABET[s.slot] as char != *c)
            })
            .unwrap();
        let (_, msg_a) = spake::device_start_with_rng("K7P42Q", store_id, rand::rngs::OsRng);
        assert_eq!(
            reg.hello(
                &free.to_string(),
                &spake::b64(&msg_a),
                "x",
                None,
                "10.0.0.5",
                store_id
            ),
            Err(Fail::PairFailed)
        );
        let reasons: Vec<_> = reg.take_audits().iter().map(|a| a.reason).collect();
        assert_eq!(reasons, ["second_hello", "wrong_code", "wrong_code"]);
        // Review M2: the abandoned exchange was charged to 10.0.0.2, the
        // address that made it (plus its refused late confirm), as well as
        // to the second sender.
        let inner = reg.lock();
        assert_eq!(inner.throttle.addrs["10.0.0.2"].failures.len(), 2);
        assert_eq!(inner.throttle.addrs["10.0.0.3"].failures.len(), 1);
    }

    /// Review M2 (§3.7 "failed or burned exchanges per address"): an
    /// address that makes exchanges and never confirms them — testing a
    /// guess offline against `hostConfirm` — is throttled after 5.
    #[test]
    fn abandoned_exchanges_count_against_the_exchanging_address() {
        let (clock, reg) = manual_clock();
        let attacker = "203.0.113.7";
        for i in 0..ADDR_FAILURES {
            let t = reg.begin(&format!("p-{i}"), None).unwrap();
            device_hello(&reg, &t.code, "s", attacker).unwrap();
            assert_eq!(reg.state_of(&t.pairing_id), Some(State::Exchanged));
        }
        // Never confirmed: they expire, each one a burned exchange.
        clock.fetch_add(PAIR_TTL_MS, Ordering::SeqCst);
        reg.tick();
        let t = reg.begin("p-next", None).unwrap();
        assert_eq!(
            device_hello(&reg, &t.code, "s", attacker).err(),
            Some(Fail::Throttled {
                retry_after_ms: PAIR_BACKOFF_STEPS_MS[0]
            })
        );
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Open), "untouched");
        // Another address still pairs.
        assert!(device_hello(&reg, &t.code, "s", "198.51.100.2").is_ok());
    }

    /// Review m8: IPv6 clients are throttled per /64.
    #[test]
    fn ipv6_is_throttled_per_slash_64() {
        assert_eq!(throttle_key("10.0.0.1"), "10.0.0.1");
        assert_eq!(throttle_key("unknown"), "unknown");
        assert_eq!(throttle_key("2001:db8:1:2:aaaa::1"), "2001:db8:1:2::/64");
        assert_eq!(throttle_key("2001:db8:1:2:ffff::9"), "2001:db8:1:2::/64");
        assert_eq!(throttle_key("::ffff:10.0.0.1"), "10.0.0.1");
        let (_, reg) = manual_clock();
        let (_, msg_a) = spake::device_start_with_rng("K7P42Q", "s", rand::rngs::OsRng);
        let m = spake::b64(&msg_a);
        for i in 0..ADDR_FAILURES {
            let addr = format!("2001:db8:1:2::{:x}", i + 1);
            assert_eq!(
                reg.hello("!", &m, "x", None, &addr, "s"),
                Err(Fail::PairFailed)
            );
        }
        assert!(matches!(
            reg.hello("!", &m, "x", None, "2001:db8:1:2::ffff", "s"),
            Err(Fail::Throttled { .. })
        ));
    }

    /// Review m1: a request being decided, or allowed and not yet
    /// delivered, survives a new begin by the same beginner and the begin
    /// TTL (by the delivery grace); `finish_*` only apply from `deciding`.
    #[test]
    fn undelivered_grants_are_neither_cancelled_nor_resurrected() {
        let (clock, reg) = manual_clock();
        let awaiting = |beginner: &str, addr: &str| {
            let t = reg.begin(beginner, None).unwrap();
            let (dev, _) = device_hello(&reg, &t.code, "s", addr).unwrap();
            reg.confirm(
                &dev.pairing_id,
                &spake::b64(&dev.keys.device_confirm()),
                addr,
            )
            .unwrap();
            (t, dev)
        };
        // Deciding, then the beginner opens a new code: not cancelled.
        let (t, dev) = awaiting("owner", "10.0.0.2");
        reg.begin_decide(&t.pairing_id, "owner").unwrap();
        let t2 = reg.begin("owner", None).unwrap();
        assert!(t2.replaced.is_empty());
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Deciding));
        assert!(reg.finish_allow(&t.pairing_id, "dev-1", Tier::Dispatch, "ikd1.a".into()));
        // Allowed, a third begin: still deliverable.
        let t3 = reg.begin("owner", None).unwrap();
        assert_eq!(t3.replaced, vec![t2.pairing_id.clone()]);
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Allowed));
        // Past the begin TTL, inside the grace: the poll still collects it.
        clock.fetch_add(PAIR_TTL_MS + DELIVERY_GRACE_MS / 2, Ordering::SeqCst);
        let poll = spake::b64(dev.keys.poll_key());
        assert!(matches!(
            reg.status(&t.pairing_id, &poll, "10.0.0.2"),
            Ok(StatusOut::Allowed { .. })
        ));

        // A cancelled session is never resurrected by a late finish_allow.
        let (t, _) = awaiting("other", "10.0.0.3");
        reg.begin_decide(&t.pairing_id, "other").unwrap();
        reg.revert_decide(&t.pairing_id);
        assert!(reg.cancel(&t.pairing_id, "other").unwrap());
        assert!(!reg.finish_allow(&t.pairing_id, "dev-2", Tier::View, "ikd1.b".into()));
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Cancelled));
        reg.finish_deny(&t.pairing_id);
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Cancelled));

        // Allowed and never polled: after the grace it expires, and the
        // minted row is queued for revocation.
        let (t, _) = awaiting("third", "10.0.0.4");
        reg.begin_decide(&t.pairing_id, "third").unwrap();
        assert!(reg.finish_allow(&t.pairing_id, "dev-3", Tier::View, "ikd1.c".into()));
        clock.fetch_add(PAIR_TTL_MS + DELIVERY_GRACE_MS, Ordering::SeqCst);
        reg.tick();
        assert_eq!(reg.state_of(&t.pairing_id), Some(State::Expired));
        assert_eq!(
            reg.lock().undelivered,
            vec![Undelivered {
                device_id: "dev-3".into(),
                pairing_id: t.pairing_id.clone()
            }]
        );
    }

    /// Review m3: during the host-wide pause, pending reports the burned
    /// codes as `paused` with the wait, and a proven status poll is exempt
    /// from the throttle (it is no guess).
    #[test]
    fn the_host_pause_is_visible_and_proven_polls_are_exempt() {
        let (_, reg) = manual_clock();
        // An allowed request waiting for its poll, and an open code.
        let t = reg.begin("owner", None).unwrap();
        let (dev, _) = device_hello(&reg, &t.code, "s", "10.0.0.2").unwrap();
        reg.confirm(
            &dev.pairing_id,
            &spake::b64(&dev.keys.device_confirm()),
            "10.0.0.2",
        )
        .unwrap();
        reg.begin_decide(&t.pairing_id, "owner").unwrap();
        assert!(reg.finish_allow(&t.pairing_id, "dev-1", Tier::View, "ikd1.a".into()));
        let open = reg.begin("other", None).unwrap();
        let (_, msg_a) = spake::device_start_with_rng("K7P42Q", "s", rand::rngs::OsRng);
        let m = spake::b64(&msg_a);
        for i in 0..HOST_FAILURES {
            let _ = reg.hello("!", &m, "x", None, &format!("10.1.{i}.1"), "s");
        }
        let pending = reg.pending("other");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["pairingId"], open.pairing_id.as_str());
        assert_eq!(pending[0]["state"], "paused");
        assert_eq!(pending[0]["retryAfterMs"], HOST_PAUSE_MS);
        // The allowed device still collects its credential, even from a
        // throttled address; an unproven poll is refused.
        assert!(matches!(
            reg.status(&t.pairing_id, "AAAA", "10.0.0.2"),
            Err(Fail::Throttled { .. })
        ));
        assert!(matches!(
            reg.status(&t.pairing_id, &spake::b64(dev.keys.poll_key()), "10.0.0.2"),
            Ok(StatusOut::Allowed { .. })
        ));
    }

    /// A-10: sessions expire 10 minutes after begin, in any state.
    #[test]
    fn sessions_expire_ten_minutes_after_begin() {
        let (clock, reg) = manual_clock();
        let store_id = "s";
        let open = reg.begin("a", None).unwrap();
        let waiting = reg.begin("b", None).unwrap();
        let (dev, _) = device_hello(&reg, &waiting.code, store_id, "1.1.1.1").unwrap();
        reg.confirm(
            &dev.pairing_id,
            &spake::b64(&dev.keys.device_confirm()),
            "1.1.1.1",
        )
        .unwrap();
        assert_eq!(reg.state_of(&waiting.pairing_id), Some(State::AwaitingHost));
        clock.fetch_add(PAIR_TTL_MS - 1, Ordering::SeqCst);
        assert_eq!(reg.open_count(), 2);
        clock.fetch_add(1, Ordering::SeqCst);
        assert_eq!(reg.open_count(), 0);
        assert_eq!(reg.state_of(&open.pairing_id), Some(State::Expired));
        assert_eq!(reg.state_of(&waiting.pairing_id), Some(State::Expired));
        let poll = spake::b64(dev.keys.poll_key());
        assert_eq!(
            reg.status(&dev.pairing_id, &poll, "1.1.1.1"),
            Ok(StatusOut::State("expired"))
        );
        assert_eq!(
            reg.begin_decide(&waiting.pairing_id, "b").unwrap_err().code,
            Code::Expired
        );
        let audits = reg.take_audits();
        assert_eq!(audits.iter().filter(|a| a.reason == "expired").count(), 2);
        // Purged after the retention window.
        clock.fetch_add(RETAIN_FINAL_MS, Ordering::SeqCst);
        reg.tick();
        assert_eq!(reg.state_of(&open.pairing_id), None);
    }

    /// A-11: 5 failures per address per 10 min → 429, escalating; 30 per
    /// host per hour → every slot paused and open sessions burned.
    #[test]
    fn throttles_per_address_and_per_host() {
        let (clock, reg) = manual_clock();
        let (_, msg_a) = spake::device_start_with_rng("K7P42Q", "s", rand::rngs::OsRng);
        let m = spake::b64(&msg_a);
        let hello = |addr: &str| reg.hello("!", &m, "x", None, addr, "s");
        for _ in 0..ADDR_FAILURES {
            assert_eq!(hello("9.9.9.9"), Err(Fail::PairFailed));
        }
        assert_eq!(
            hello("9.9.9.9"),
            Err(Fail::Throttled {
                retry_after_ms: PAIR_BACKOFF_STEPS_MS[0]
            })
        );
        assert_eq!(hello("9.9.9.8"), Err(Fail::PairFailed), "per address");
        clock.fetch_add(PAIR_BACKOFF_STEPS_MS[0], Ordering::SeqCst);
        for _ in 0..ADDR_FAILURES {
            assert_eq!(hello("9.9.9.9"), Err(Fail::PairFailed));
        }
        assert_eq!(
            hello("9.9.9.9"),
            Err(Fail::Throttled {
                retry_after_ms: PAIR_BACKOFF_STEPS_MS[1]
            }),
            "escalates"
        );

        // Host-wide: 30 failures in an hour from many addresses.
        let (_, reg) = manual_clock();
        let open = reg.begin("owner", None).unwrap();
        for i in 0..HOST_FAILURES {
            let _ = reg.hello("!", &m, "x", None, &format!("10.1.{i}.1"), "s");
        }
        assert_eq!(reg.state_of(&open.pairing_id), Some(State::Burned));
        let e = reg.begin("owner", None).unwrap_err();
        assert_eq!(e.code, Code::Throttled);
        assert!(e.message.contains("pairing paused for 15 min"));
        assert!(matches!(
            reg.hello("!", &m, "x", None, "10.9.9.9", "s"),
            Err(Fail::Throttled { .. })
        ));
        let throttled = reg
            .take_audits()
            .iter()
            .filter(|a| a.reason == "throttled")
            .count();
        assert!(throttled >= 1);
    }

    fn env<'a>(
        store: &'a AccessStore,
        reg: &'a Registry,
        sockets: &'a sockets::Registry,
    ) -> Env<'a> {
        Env {
            tier: StoreTier::T0,
            store: Some(store),
            pairing: Some(reg),
            sockets,
            principal: PrincipalInfo {
                username: "ned".into(),
                is_admin: false,
            },
            public_url: None,
            insecure_cookie: false,
        }
    }

    async fn table_text(store: &AccessStore) -> String {
        let mut conn = store.pool().acquire().await.unwrap();
        let mut all = String::new();
        for table in ["devices", "audit_events", "store_meta"] {
            let rows = sqlx::query(&format!("SELECT * FROM {table}"))
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            for r in rows {
                use sqlx::{Row, ValueRef};
                for i in 0..r.len() {
                    let raw = r.try_get_raw(i).unwrap();
                    if raw.is_null() {
                        continue;
                    }
                    if let Ok(s) = r.try_get::<String, _>(i) {
                        all.push_str(&s);
                    } else if let Ok(b) = r.try_get::<Vec<u8>, _>(i) {
                        all.push_str(&hex::encode(&b));
                        all.push_str(&String::from_utf8_lossy(&b));
                    }
                    all.push('\n');
                }
            }
        }
        all
    }

    /// A-9 / A-12 / the full flow: begin → hello → confirm → pending (same
    /// words) → allow → status delivers once (410 after); nothing holds a
    /// credential before allow; deny leaves no device row; no code or token
    /// appears in any column.
    #[tokio::test]
    async fn full_pairing_flow_allow_and_deny() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::default();
        let socks = sockets::Registry::new();
        let op = operator_ctx(&store);
        let e = env(&store, &reg, &socks);
        let store_id = store.meta().store_id.clone();

        let t = dispatch(&e, &op, "access_pair_begin", &json!({}))
            .await
            .unwrap();
        assert_eq!(t["pairUrl"], Value::Null, "loopback-only: no QR");
        let code = t["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 7);
        let pid = t["pairingId"].as_str().unwrap().to_string();

        let (dev, ok) = device_hello(&reg, &code, &store_id, "100.64.0.9").unwrap();
        assert_eq!(ok.host_confirm, dev.keys.host_confirm());
        reg.confirm(&pid, &spake::b64(&dev.keys.device_confirm()), "100.64.0.9")
            .unwrap();
        let poll = spake::b64(dev.keys.poll_key());
        assert_eq!(
            reg.status(&pid, &poll, "100.64.0.9"),
            Ok(StatusOut::State("awaiting_host"))
        );
        assert_eq!(
            reg.status(&pid, "AAAA", "100.64.0.9"),
            Err(Fail::PairFailed)
        );

        let pending = dispatch(&e, &op, "access_pair_pending", &json!({}))
            .await
            .unwrap();
        let p = &pending[0];
        assert_eq!(p["state"], "awaiting_host");
        assert_eq!(p["code"], code);
        assert_eq!(p["remoteAddr"], "100.64.0.9");
        assert_eq!(p["deviceName"], "Pixel 9 · Chrome");
        let words: Vec<String> = p["fingerprint"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_string())
            .collect();
        assert_eq!(words, dev.keys.fingerprint().map(str::to_string));

        // A-9: no credential before allow.
        let mut conn = store.pool().acquire().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE kind = 'paired'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(n, 0);
        drop(conn);

        let err = dispatch(
            &e,
            &op,
            "access_pair_decide",
            &json!({"pairingId": pid, "decision": "allow", "tier": "full"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest, "P-9: no full at pair time");
        let v = dispatch(
            &e,
            &op,
            "access_pair_decide",
            &json!({"pairingId": pid, "decision": "allow"}),
        )
        .await
        .unwrap();
        assert_eq!(v["device"]["tier"], "dispatch");
        let StatusOut::Allowed {
            device_id,
            tier,
            token,
        } = reg.status(&pid, &poll, "100.64.0.9").unwrap()
        else {
            panic!("allowed")
        };
        assert_eq!(tier, Tier::Dispatch);
        assert_eq!(reg.status(&pid, &poll, "100.64.0.9"), Err(Fail::Gone));
        let gate = devices::SeenGate::default();
        match devices::resolve(&store, &gate, &token, Some("100.64.0.9"))
            .await
            .unwrap()
        {
            devices::DeviceAuth::Valid { row, .. } => {
                assert_eq!(row.device_id, device_id);
                assert_eq!(row.principal_id, op.principal_id.to_string());
                assert_eq!(row.name, "Pixel 9 · Chrome");
            }
            other => panic!("{other:?}"),
        }

        // Deny: no device row.
        let t = dispatch(&e, &op, "access_pair_begin", &json!({}))
            .await
            .unwrap();
        let pid2 = t["pairingId"].as_str().unwrap().to_string();
        let code2 = t["code"].as_str().unwrap().to_string();
        let (dev2, _) = device_hello(&reg, &code2, &store_id, "100.64.0.10").unwrap();
        reg.confirm(
            &pid2,
            &spake::b64(&dev2.keys.device_confirm()),
            "100.64.0.10",
        )
        .unwrap();
        dispatch(
            &e,
            &op,
            "access_pair_decide",
            &json!({"pairingId": pid2, "decision": "deny"}),
        )
        .await
        .unwrap();
        assert_eq!(
            reg.status(&pid2, &spake::b64(dev2.keys.poll_key()), "100.64.0.10"),
            Ok(StatusOut::State("denied"))
        );
        let mut conn = store.pool().acquire().await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM devices WHERE kind = 'paired'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(n, 1, "deny left no row");
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        assert_eq!(
            kinds,
            [
                "store.created",
                "pair.started",
                "pair.allowed",
                "pair.started",
                "pair.denied"
            ]
        );

        // A-12: no code, token or secret anywhere in the store. A code is
        // matched as a whole token (no alphanumeric neighbour), so an
        // all-digit code can't collide with a timestamp or an id (review m7).
        let text = table_text(&store).await;
        let secret = token.rsplit('.').next().unwrap();
        for needle in [code.replace('-', ""), code.clone(), code2.replace('-', "")] {
            assert!(
                !contains_token(&text, &needle),
                "{needle} leaked into the store"
            );
        }
        for needle in [token.clone(), secret.to_string()] {
            assert!(!text.contains(&needle), "{needle} leaked into the store");
        }
    }

    /// `needle` in `hay` with no ASCII alphanumeric on either side.
    fn contains_token(hay: &str, needle: &str) -> bool {
        hay.match_indices(needle).any(|(i, _)| {
            let before = hay[..i].chars().next_back();
            let after = hay[i + needle.len()..].chars().next();
            !before.is_some_and(|c| c.is_ascii_alphanumeric())
                && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        })
    }

    #[test]
    fn the_leak_scan_matches_whole_tokens() {
        assert!(contains_token("x \"K7P42Q\" y", "K7P42Q"));
        assert!(contains_token("K7P-42Q", "K7P-42Q"));
        assert!(!contains_token("1700000123456", "000123"));
        assert!(!contains_token("abK7P42Qcd", "K7P42Q"));
    }

    /// Review m1: an allowed grant never collected within the delivery
    /// grace is revoked in place (no live grant nobody holds), audited
    /// `device.revoked {reason: undelivered}`.
    #[tokio::test]
    async fn an_undelivered_grant_is_revoked() {
        let store = AccessStore::memory_t0().await;
        let (clock, reg) = manual_clock();
        let socks = sockets::Registry::new();
        let op = operator_ctx(&store);
        let e = env(&store, &reg, &socks);
        let store_id = store.meta().store_id.clone();
        let t = dispatch(&e, &op, "access_pair_begin", &json!({}))
            .await
            .unwrap();
        let pid = t["pairingId"].as_str().unwrap().to_string();
        let (dev, _) =
            device_hello(&reg, t["code"].as_str().unwrap(), &store_id, "10.0.0.2").unwrap();
        reg.confirm(&pid, &spake::b64(&dev.keys.device_confirm()), "10.0.0.2")
            .unwrap();
        let v = dispatch(
            &e,
            &op,
            "access_pair_decide",
            &json!({"pairingId": pid, "decision": "allow"}),
        )
        .await
        .unwrap();
        let device_id = v["device"]["deviceId"].as_str().unwrap().to_string();
        clock.fetch_add(PAIR_TTL_MS + DELIVERY_GRACE_MS, Ordering::SeqCst);
        reg.tick();
        flush_audits(&reg, &store).await;
        let mut conn = store.pool().acquire().await.unwrap();
        let (revoked_at, reason): (Option<i64>, Option<String>) =
            sqlx::query_as("SELECT revoked_at, revoked_reason FROM devices WHERE device_id = ?")
                .bind(&device_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert!(revoked_at.is_some());
        assert_eq!(reason.as_deref(), Some("user"));
        let detail: String = sqlx::query_scalar(
            "SELECT detail FROM audit_events WHERE kind = 'device.revoked' ORDER BY seq DESC",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert!(detail.contains("undelivered"), "{detail}");
    }

    #[tokio::test]
    async fn only_admin_strength_pairs_and_only_the_beginner_decides() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::default();
        let socks = sockets::Registry::new();
        let e = env(&store, &reg, &socks);
        let (row, _) = devices::tests::pair(&store, Tier::Approve).await;
        let phone = AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            via: Via::Device {
                device_id: row.device_id.clone(),
            },
            device_id: Some(row.device_id.clone()),
            tier: Tier::Approve,
            share: None,
            share_headers: false,
            caps: Tier::Approve.caps(),
            admin_strength: false,
            meta: RequestMeta::default(),
        };
        for cmd in [
            "access_pair_begin",
            "access_pair_pending",
            "access_pair_cancel",
            "access_pair_decide",
        ] {
            assert_eq!(
                dispatch(
                    &e,
                    &phone,
                    cmd,
                    &json!({"pairingId": "x", "decision": "deny"})
                )
                .await
                .unwrap_err()
                .code,
                Code::Forbidden,
                "{cmd}"
            );
        }
        let op = operator_ctx(&store);
        let t = dispatch(&e, &op, "access_pair_begin", &json!({}))
            .await
            .unwrap();
        let mut other = op.clone();
        other.principal_id = crate::executor::PrincipalId::new_v7();
        assert_eq!(
            dispatch(
                &e,
                &other,
                "access_pair_decide",
                &json!({"pairingId": t["pairingId"], "decision": "allow"})
            )
            .await
            .unwrap_err()
            .code,
            Code::NotFound
        );
        assert_eq!(
            dispatch(
                &e,
                &other,
                "access_pair_cancel",
                &json!({"pairingId": t["pairingId"]})
            )
            .await
            .unwrap_err()
            .code,
            Code::NotFound
        );
        dispatch(
            &e,
            &op,
            "access_pair_cancel",
            &json!({"pairingId": t["pairingId"]}),
        )
        .await
        .unwrap();
    }

    #[test]
    fn pair_url_follows_the_three_rules() {
        let mut ctx = AccessCtx {
            principal_id: crate::executor::PrincipalId::new_v7(),
            via: Via::Operator,
            device_id: None,
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::ALL,
            admin_strength: true,
            meta: RequestMeta {
                host: Some("127.0.0.1:4000".into()),
                ..Default::default()
            },
        };
        assert_eq!(pair_url(None, &ctx, None), None);
        assert_eq!(
            pair_url(None, &ctx, Some("http://100.64.0.1:4000/")).as_deref(),
            Some("http://100.64.0.1:4000/remote/pair")
        );
        assert_eq!(
            pair_url(Some("https://ik.example"), &ctx, Some("http://x:1")).as_deref(),
            Some("https://ik.example/remote/pair"),
            "--public-url wins"
        );
        ctx.via = Via::Device {
            device_id: "d".into(),
        };
        assert_eq!(
            pair_url(None, &ctx, Some("http://100.64.0.1:4000")),
            None,
            "publicBase only from the operator bearer"
        );
        ctx.meta.host = Some("ned-desktop.tail1.ts.net:4000".into());
        assert_eq!(
            pair_url(None, &ctx, None).as_deref(),
            Some("http://ned-desktop.tail1.ts.net:4000/remote/pair")
        );
        for h in ["localhost:4000", "127.0.0.1", "[::1]:4000", "app.localhost"] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in ["100.64.0.1:4000", "192.168.1.4", "[fd7a::1]:4000"] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }
}
