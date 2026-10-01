//! Account passwords (G-PRINCIPAL §6.2, OD-8): argon2id PHC strings through
//! the already-vendored `rust-argon2 =2.1.0`, plus the login verification
//! helper the broker's `POST /auth/login` (slice 3) calls.
//!
//! * **Parameters:** argon2id, v=19, m = 19 456 KiB, t = 2, p = 1, 16-byte
//!   random salt, 32-byte hash — the same costs as `commands/app_lock.rs` and
//!   `secrets/crypto.rs`. They travel in the PHC string, so raising them later
//!   is a rehash on the next successful login.
//! * **Concurrency:** hashing runs on `spawn_blocking` behind a process-wide
//!   cap of [`HASH_CONCURRENCY`], so a login flood queues instead of pinning
//!   every blocking thread at 19 MiB apiece.
//! * **Timing:** an unknown username (or an account with no password) is
//!   verified against a fixed dummy hash, so it costs the same as a wrong
//!   password.
//! * **Backoff:** failures are counted per username and per remote address;
//!   every [`MAX_ATTEMPTS`] misses start a wait from [`BACKOFF_STEPS_MS`]
//!   (`app_lock.rs:100`). Every refused-while-waiting attempt writes
//!   `login_throttled`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{Config, Variant, Version};
use rand::RngCore;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use super::accounts::{self, Account};
use super::auth_events::{self, AuthEvent, AuthEventKind};
use crate::executor::PrincipalId;

pub const ARGON2_MEMORY_KIB: u32 = 19_456;
pub const ARGON2_ITERATIONS: u32 = 2;
pub const ARGON2_PARALLELISM: u32 = 1;
pub const SALT_LEN: usize = 16;
pub const HASH_LEN: u32 = 32;
/// At most this many argon2 computations run at once in this process.
pub const HASH_CONCURRENCY: usize = 4;

/// Wrong passwords allowed per round before a wait (as `app_lock`).
pub const MAX_ATTEMPTS: u32 = 3;
/// The wait after each round of [`MAX_ATTEMPTS`] misses; the last step
/// repeats. Mirrors `commands::app_lock::BACKOFF_STEPS_MS`, which is
/// desktop-only and so can't be imported by the daemon build; a desktop test
/// pins the two together.
pub const BACKOFF_STEPS_MS: [u64; 4] = [30_000, 60_000, 5 * 60_000, 15 * 60_000];

/// Account password policy: one rule for every path into the provisioning
/// core — the CLI, the env bootstrap and G-ACCESS invite acceptance, whose
/// P-24 sets the ≥12 minimum (it calls this same `create_in`, R-8). The upper
/// bound keeps a hash request from being made arbitrarily large.
pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_CHARS: usize = 1024;

fn argon2_config() -> Config<'static> {
    Config {
        ad: &[],
        hash_length: HASH_LEN,
        lanes: ARGON2_PARALLELISM,
        mem_cost: ARGON2_MEMORY_KIB,
        secret: &[],
        time_cost: ARGON2_ITERATIONS,
        variant: Variant::Argon2id,
        version: Version::Version13,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordPolicyError {
    TooShort,
    TooLong,
}

impl std::fmt::Display for PasswordPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordPolicyError::TooShort => {
                write!(
                    f,
                    "password must be at least {MIN_PASSWORD_CHARS} characters"
                )
            }
            PasswordPolicyError::TooLong => {
                write!(
                    f,
                    "password must be at most {MAX_PASSWORD_CHARS} characters"
                )
            }
        }
    }
}

impl std::error::Error for PasswordPolicyError {}

/// The policy a **new** password must meet. Login never applies it: an
/// existing hash is whatever it is.
pub fn validate_new_password(password: &str) -> Result<(), PasswordPolicyError> {
    let n = password.chars().count();
    if n < MIN_PASSWORD_CHARS {
        return Err(PasswordPolicyError::TooShort);
    }
    if n > MAX_PASSWORD_CHARS {
        return Err(PasswordPolicyError::TooLong);
    }
    Ok(())
}

/// Hash `password` into a PHC string, on the calling thread.
pub fn hash_blocking(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    argon2::hash_encoded(password.as_bytes(), &salt, &argon2_config())
        .map_err(|e| anyhow::anyhow!("argon2 hash: {e}"))
}

/// Verify `password` against a PHC string, on the calling thread. A malformed
/// PHC string is a mismatch, never an error a caller could branch on.
pub fn verify_blocking(phc: &str, password: &str) -> bool {
    argon2::verify_encoded(phc, password.as_bytes()).unwrap_or(false)
}

/// A fixed hash at the live parameters that no password the user typed is
/// checked against meaningfully — it exists so the unknown-user path does the
/// same work as the known-user one.
fn dummy_phc() -> &'static str {
    static DUMMY: OnceLock<String> = OnceLock::new();
    DUMMY.get_or_init(|| {
        argon2::hash_encoded(
            b"ikenga-dummy-password-for-unknown-users",
            b"ikenga-dummy-salt",
            &argon2_config(),
        )
        .expect("argon2 accepts the fixed dummy parameters")
    })
}

fn hash_gate() -> &'static Arc<Semaphore> {
    static GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GATE.get_or_init(|| Arc::new(Semaphore::new(HASH_CONCURRENCY)))
}

async fn gated<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> anyhow::Result<T> {
    let _permit = hash_gate()
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| anyhow::anyhow!("hash gate closed: {e}"))?;
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| anyhow::anyhow!("argon2 task: {e}"))
}

/// [`hash_blocking`] on the blocking pool, behind the concurrency cap.
pub async fn hash(password: Zeroizing<String>) -> anyhow::Result<String> {
    gated(move || hash_blocking(&password)).await?
}

/// [`verify_blocking`] on the blocking pool, behind the concurrency cap.
/// `None` (no account, or no password set) verifies against the dummy hash
/// and is always `false`.
pub async fn verify(phc: Option<String>, password: Zeroizing<String>) -> anyhow::Result<bool> {
    gated(move || match phc {
        Some(phc) => verify_blocking(&phc, &password),
        None => {
            let _ = verify_blocking(dummy_phc(), &password);
            false
        }
    })
    .await
}

// ─── backoff ────────────────────────────────────────────────────────────────

/// Who a login attempt is against, as far as the backoff is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LoginSubject {
    /// An existing account (keyed by `principal_id`, so the case or spelling
    /// of the typed username can't split its strikes).
    Account(PrincipalId),
    /// A username with no account, lowercased. Kept per name — never one
    /// shared bucket — so the backoff itself can't tell an attacker which
    /// names exist.
    Unknown(String),
}

impl LoginSubject {
    pub fn unknown(username: &str) -> Self {
        LoginSubject::Unknown(username.to_lowercase())
    }
}

#[derive(Debug, Clone, Default)]
struct Strikes {
    /// Resolved misses in the current round.
    misses: u32,
    round: u32,
    wait_until_ms: u64,
    last_miss_ms: u64,
    /// Attempts that passed [`LoginThrottle::begin`] and haven't resolved.
    /// They count against the round, so parallel requests can't all slip
    /// through one check (each reserves a strike up front).
    in_flight: u32,
    last_touch_ms: u64,
}

impl Strikes {
    fn waiting(&self, now_ms: u64) -> bool {
        self.wait_until_ms > now_ms
    }

    /// A key quiet for longer than the longest wait — counted from its last
    /// miss or the end of its last wait, whichever is later — starts over.
    /// Serving a wait does not by itself reset the round.
    fn forget_if_quiet(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_miss_ms.max(self.wait_until_ms)) >= FORGET_AFTER_MS {
            *self = Strikes {
                in_flight: self.in_flight,
                last_touch_ms: self.last_touch_ms,
                ..Strikes::default()
            };
        }
    }

    fn quiet(&self, now_ms: u64) -> bool {
        self.in_flight == 0
            && now_ms.saturating_sub(self.last_miss_ms.max(self.wait_until_ms)) >= FORGET_AFTER_MS
    }

    fn miss(&mut self, now_ms: u64) {
        self.forget_if_quiet(now_ms);
        self.misses += 1;
        self.last_miss_ms = now_ms;
        self.last_touch_ms = now_ms;
        if self.misses >= MAX_ATTEMPTS {
            self.wait_until_ms = now_ms + step_ms(self.round);
            self.round += 1;
            self.misses = 0;
        }
    }
}

/// Cap on remembered unknown usernames and on remembered addresses (each).
/// Existing accounts are never capped: their count is the accounts table's,
/// which no attacker controls, so a flood of junk keys can't switch off
/// per-account backoff.
const MAX_TRACKED_KEYS: usize = 10_000;

/// Returned while a key's round is fully reserved by attempts still being
/// verified: retry once they resolve.
pub const IN_FLIGHT_RETRY_MS: u64 = 1_000;

/// One bounded (or unbounded) map of strikes.
#[derive(Debug)]
struct Table<K> {
    map: HashMap<K, Strikes>,
    cap: Option<usize>,
}

impl<K: std::hash::Hash + Eq + Clone> Table<K> {
    fn new(cap: Option<usize>) -> Self {
        Self {
            map: HashMap::new(),
            cap,
        }
    }

    /// The entry for `key`, making room first when the table is full: quiet
    /// keys go, then the least recently touched key that is neither waiting
    /// nor in flight, then the least recently touched one not in flight. A
    /// key with an attempt in flight is never evicted, so the table can
    /// exceed its cap by the number of concurrent attempts — never skip.
    fn entry(&mut self, key: &K, now_ms: u64) -> &mut Strikes {
        if let Some(cap) = self.cap {
            if self.map.len() >= cap && !self.map.contains_key(key) {
                self.map.retain(|_, s| !s.quiet(now_ms));
                if self.map.len() >= cap {
                    let victim = self
                        .map
                        .iter()
                        .filter(|(_, s)| s.in_flight == 0)
                        .min_by_key(|(_, s)| (s.waiting(now_ms), s.last_touch_ms))
                        .map(|(k, _)| k.clone());
                    if let Some(victim) = victim {
                        self.map.remove(&victim);
                    }
                }
            }
        }
        self.map.entry(key.clone()).or_default()
    }
}

#[derive(Debug)]
struct Tables {
    accounts: Table<PrincipalId>,
    unknown: Table<String>,
    addrs: Table<String>,
}

impl Default for Tables {
    fn default() -> Self {
        Self {
            accounts: Table::new(None),
            unknown: Table::new(Some(MAX_TRACKED_KEYS)),
            addrs: Table::new(Some(MAX_TRACKED_KEYS)),
        }
    }
}

impl Tables {
    fn get(&self, key: &Key) -> Option<&Strikes> {
        match key {
            Key::Account(id) => self.accounts.map.get(id),
            Key::Unknown(name) => self.unknown.map.get(name),
            Key::Addr(addr) => self.addrs.map.get(addr),
        }
    }

    fn get_mut(&mut self, key: &Key) -> Option<&mut Strikes> {
        match key {
            Key::Account(id) => self.accounts.map.get_mut(id),
            Key::Unknown(name) => self.unknown.map.get_mut(name),
            Key::Addr(addr) => self.addrs.map.get_mut(addr),
        }
    }

    fn entry(&mut self, key: &Key, now_ms: u64) -> &mut Strikes {
        match key {
            Key::Account(id) => self.accounts.entry(id, now_ms),
            Key::Unknown(name) => self.unknown.entry(name, now_ms),
            Key::Addr(addr) => self.addrs.entry(addr, now_ms),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    Account(PrincipalId),
    Unknown(String),
    Addr(String),
}

/// The address a backoff key is kept under: an IPv6 address counts as its
/// /64 (one host's usual allocation, so rotating within it buys nothing); an
/// IPv4 address, or anything unparsable, as itself. A `SocketAddr`'s port is
/// dropped.
fn addr_key(addr: &str) -> String {
    use std::net::{IpAddr, SocketAddr};
    let ip = addr
        .parse::<IpAddr>()
        .ok()
        .or_else(|| addr.parse::<SocketAddr>().ok().map(|s| s.ip()));
    match ip {
        Some(IpAddr::V6(v6)) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
        Some(IpAddr::V4(v4)) => v4.to_string(),
        None => addr.to_string(),
    }
}

fn keys_for(subject: &LoginSubject, addr: Option<&str>) -> Vec<Key> {
    let mut keys = vec![match subject {
        LoginSubject::Account(id) => Key::Account(*id),
        LoginSubject::Unknown(name) => Key::Unknown(name.to_lowercase()),
    }];
    if let Some(addr) = addr {
        keys.push(Key::Addr(addr_key(addr)));
    }
    keys
}

/// Failed-login backoff per account (or unknown username) and per remote
/// address (§6.2). In-memory and per process: a broker restart forgets it,
/// which costs an attacker a restart they can't cause.
///
/// Check and reservation are one step ([`begin`](Self::begin)) under one
/// lock: every admitted attempt holds a strike until it resolves, so at most
/// [`MAX_ATTEMPTS`] guesses per round reach the hash however many requests
/// arrive at once.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    tables: Mutex<Tables>,
}

fn step_ms(round: u32) -> u64 {
    BACKOFF_STEPS_MS[(round as usize).min(BACKOFF_STEPS_MS.len() - 1)]
}

const FORGET_AFTER_MS: u64 = BACKOFF_STEPS_MS[BACKOFF_STEPS_MS.len() - 1];

/// An admitted attempt's reserved strike. Resolve it with
/// [`failed`](Self::failed) or [`succeeded`](Self::succeeded); dropping it
/// unresolved (an internal error before the outcome was known) releases the
/// reservation without counting a miss.
#[must_use = "resolve the attempt with failed() or succeeded()"]
pub struct AttemptTicket<'a> {
    throttle: &'a LoginThrottle,
    keys: Vec<Key>,
    resolved: bool,
}

impl AttemptTicket<'_> {
    /// A wrong password (or any failure): count one miss on every key.
    pub fn failed(mut self, now_ms: u64) {
        self.resolved = true;
        let mut t = self.throttle.lock();
        for key in &self.keys {
            let s = t.entry(key, now_ms);
            s.in_flight = s.in_flight.saturating_sub(1);
            s.miss(now_ms);
        }
    }

    /// A correct password clears the account's strikes. The address keeps
    /// its own: one valid account must not reset a spray from the same host.
    pub fn succeeded(mut self) {
        self.resolved = true;
        let mut t = self.throttle.lock();
        for (i, key) in self.keys.iter().enumerate() {
            if let Some(s) = t.get_mut(key) {
                s.in_flight = s.in_flight.saturating_sub(1);
                if i == 0 {
                    *s = Strikes {
                        in_flight: s.in_flight,
                        last_touch_ms: s.last_touch_ms,
                        ..Strikes::default()
                    };
                }
            }
        }
    }
}

impl Drop for AttemptTicket<'_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        let mut t = self.throttle.lock();
        for key in &self.keys {
            if let Some(s) = t.get_mut(key) {
                s.in_flight = s.in_flight.saturating_sub(1);
            }
        }
    }
}

impl LoginThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tables> {
        self.tables.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Admit one attempt against `subject` from `addr`, reserving a strike on
    /// both keys, or refuse it with the milliseconds to wait. Refused when
    /// either key is serving a wait, or when its misses plus the attempts
    /// already in flight fill the round.
    pub fn begin(
        &self,
        subject: &LoginSubject,
        addr: Option<&str>,
        now_ms: u64,
    ) -> Result<AttemptTicket<'_>, u64> {
        let keys = keys_for(subject, addr);
        let mut t = self.lock();
        let mut refuse: Option<u64> = None;
        for key in &keys {
            if let Some(s) = t.get_mut(key) {
                s.forget_if_quiet(now_ms);
                let wait = if s.waiting(now_ms) {
                    Some(s.wait_until_ms - now_ms)
                } else if s.misses + s.in_flight >= MAX_ATTEMPTS {
                    Some(IN_FLIGHT_RETRY_MS)
                } else {
                    None
                };
                refuse = refuse.max(wait);
            }
        }
        if let Some(wait) = refuse {
            return Err(wait);
        }
        for key in &keys {
            let s = t.entry(key, now_ms);
            s.in_flight += 1;
            s.last_touch_ms = now_ms;
        }
        drop(t);
        Ok(AttemptTicket {
            throttle: self,
            keys,
            resolved: false,
        })
    }

    /// Milliseconds left on the longest active wait among `subject` and
    /// `addr`, or `None` when no wait runs (read-only; in-flight reservations
    /// are not reflected).
    pub fn retry_after_ms(
        &self,
        subject: &LoginSubject,
        addr: Option<&str>,
        now_ms: u64,
    ) -> Option<u64> {
        let t = self.lock();
        keys_for(subject, addr)
            .iter()
            .filter_map(|k| t.get(k))
            .filter(|s| s.waiting(now_ms))
            .map(|s| s.wait_until_ms - now_ms)
            .max()
    }

    /// Count one resolved miss against `subject` and `addr` (an attempt that
    /// was never admitted through [`begin`](Self::begin)).
    pub fn record_failure(&self, subject: &LoginSubject, addr: Option<&str>, now_ms: u64) {
        let mut t = self.lock();
        for key in keys_for(subject, addr) {
            t.entry(&key, now_ms).miss(now_ms);
        }
    }

    #[cfg(test)]
    fn tracked(&self) -> (usize, usize, usize) {
        let t = self.lock();
        (t.accounts.map.len(), t.unknown.map.len(), t.addrs.map.len())
    }
}

// ─── login ──────────────────────────────────────────────────────────────────

/// One `POST /auth/login` attempt.
pub struct LoginAttempt {
    pub username: String,
    pub password: Zeroizing<String>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
}

/// What the caller tells the client. `Failed` deliberately carries no reason:
/// unknown user, wrong password, disabled account and no-password account
/// look identical from outside (the reason is in `auth_events.detail`).
#[derive(Debug)]
pub enum LoginOutcome {
    Ok(Account),
    Failed,
    Throttled { retry_after_ms: u64 },
}

/// The login verification helper (§6.2). One per broker.
#[derive(Debug)]
pub struct LoginVerifier {
    throttle: LoginThrottle,
}

impl Default for LoginVerifier {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LoginVerifier {
    /// Builds the verifier **and computes the dummy hash now** (one argon2
    /// run, on the calling thread — construct it at broker boot), so the
    /// first unknown-user login costs exactly what every later one does.
    pub fn new() -> Self {
        let _ = dummy_phc();
        Self {
            throttle: LoginThrottle::new(),
        }
    }

    pub fn throttle(&self) -> &LoginThrottle {
        &self.throttle
    }

    /// Verify `attempt` against `accounts.db`, record the outcome in
    /// `auth_events`, and update the backoff.
    pub async fn login(
        &self,
        pool: &SqlitePool,
        attempt: LoginAttempt,
    ) -> anyhow::Result<LoginOutcome> {
        self.login_at(pool, attempt, now_ms).await
    }

    /// [`login`](Self::login) with an injectable clock (tests). The clock is
    /// read when the attempt begins and again once argon2 has finished: a
    /// verify queued behind other hashes can take seconds, and the miss must
    /// be stamped with when it actually resolved (review S1-M3).
    pub(crate) async fn login_at(
        &self,
        pool: &SqlitePool,
        attempt: LoginAttempt,
        clock: impl Fn() -> u64,
    ) -> anyhow::Result<LoginOutcome> {
        let LoginAttempt {
            username,
            password,
            remote_addr,
            user_agent,
        } = attempt;
        let addr = remote_addr.as_deref();

        // The (cheap, indexed) lookup comes first so the backoff can key an
        // existing account by its principal_id. Its cost does not depend on
        // the outcome, so it adds no timing signal.
        let account = {
            let mut conn = pool.acquire().await?;
            accounts::by_username(&mut conn, &username).await?
        };
        let subject = match &account {
            Some(a) => LoginSubject::Account(a.principal_id),
            None => LoginSubject::unknown(&username),
        };

        let ticket = match self.throttle.begin(&subject, addr, clock()) {
            Ok(ticket) => ticket,
            Err(retry_after_ms) => {
                let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
                auth_events::record(
                    &mut tx,
                    AuthEvent::new(AuthEventKind::LoginThrottled)
                        .username_tried(username.clone())
                        .remote_addr(remote_addr.clone())
                        .user_agent(user_agent.clone())
                        .detail(serde_json::json!({ "retry_after_ms": retry_after_ms })),
                )
                .await?;
                tx.commit().await?;
                return Ok(LoginOutcome::Throttled { retry_after_ms });
            }
        };

        let phc = account.as_ref().and_then(|a| a.password_phc.clone());
        let has_phc = phc.is_some();
        let verified = verify(phc, password).await?;

        let failure = match &account {
            None => Some("unknown_user"),
            Some(_) if !has_phc => Some("no_password"),
            Some(_) if !verified => Some("bad_password"),
            Some(a) if a.is_disabled() => Some("disabled"),
            Some(_) => None,
        };
        // The backoff is settled before the audit write, so a failed write
        // can't hand the strike back.
        if failure.is_none() {
            ticket.succeeded();
        } else {
            ticket.failed(clock());
        }

        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut event = AuthEvent::new(if failure.is_none() {
            AuthEventKind::LoginOk
        } else {
            AuthEventKind::LoginFail
        })
        .username_tried(username.clone())
        .remote_addr(remote_addr)
        .user_agent(user_agent);
        if let Some(a) = &account {
            event = event.principal(a.principal_id);
        }
        if let Some(reason) = failure {
            event = event.detail(serde_json::json!({ "reason": reason }));
        }
        auth_events::record(&mut tx, event).await?;
        tx.commit().await?;

        match (failure, account) {
            (None, Some(account)) => Ok(LoginOutcome::Ok(account)),
            _ => Ok(LoginOutcome::Failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phc_carries_the_contract_parameters_and_verifies() {
        let phc = hash_blocking("correct horse").unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        assert!(verify_blocking(&phc, "correct horse"));
        assert!(!verify_blocking(&phc, "correct horsf"));
        assert!(!verify_blocking("not a phc string", "correct horse"));
        // Fresh salt every time.
        assert_ne!(phc, hash_blocking("correct horse").unwrap());
    }

    #[test]
    fn dummy_hash_uses_the_live_parameters() {
        assert!(dummy_phc().starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    }

    #[tokio::test]
    async fn unknown_user_verifies_against_the_dummy_and_fails() {
        assert!(!verify(None, Zeroizing::new("anything".into()))
            .await
            .unwrap());
        let phc = hash(Zeroizing::new("pw-12345678".into())).await.unwrap();
        assert!(verify(Some(phc), Zeroizing::new("pw-12345678".into()))
            .await
            .unwrap());
    }

    #[test]
    fn new_password_policy() {
        assert_eq!(
            validate_new_password("short"),
            Err(PasswordPolicyError::TooShort)
        );
        assert_eq!(
            validate_new_password("eleven char"),
            Err(PasswordPolicyError::TooShort)
        );
        assert!(validate_new_password("twelve chars").is_ok());
        assert_eq!(
            validate_new_password(&"x".repeat(MAX_PASSWORD_CHARS + 1)),
            Err(PasswordPolicyError::TooLong)
        );
    }

    fn unknown(name: &str) -> LoginSubject {
        LoginSubject::unknown(name)
    }

    #[test]
    fn backoff_walks_the_steps_per_round_and_repeats_the_last() {
        let t = LoginThrottle::new();
        let ada = unknown("ada");
        let mut now = 1_000_000;
        for (round, step) in BACKOFF_STEPS_MS
            .iter()
            .chain(std::iter::once(&BACKOFF_STEPS_MS[3]))
            .enumerate()
        {
            for miss in 0..MAX_ATTEMPTS {
                let ticket = t
                    .begin(&ada, None, now)
                    .unwrap_or_else(|w| panic!("round {round} miss {miss}: refused for {w}"));
                ticket.failed(now);
            }
            assert_eq!(t.begin(&ada, None, now).err(), Some(*step), "round {round}");
            assert_eq!(t.retry_after_ms(&ada, None, now), Some(*step));
            now += step;
        }
    }

    #[test]
    fn backoff_is_per_subject_case_insensitively_and_per_address() {
        let t = LoginThrottle::new();
        for _ in 0..MAX_ATTEMPTS {
            t.record_failure(&unknown("Ada"), Some("10.0.0.1"), 0);
        }
        assert!(t.retry_after_ms(&unknown("ada"), None, 0).is_some());
        // Same address, another username: the address is throttled too.
        assert!(t.begin(&unknown("bob"), Some("10.0.0.1"), 0).is_err());
        // Another address, another username: free.
        t.begin(&unknown("bob"), Some("10.0.0.2"), 0)
            .unwrap()
            .succeeded();

        // A success clears the account's strikes, not the address's.
        let id = PrincipalId::new_v7();
        let acct = LoginSubject::Account(id);
        t.record_failure(&acct, Some("10.0.0.3"), 0);
        t.record_failure(&acct, Some("10.0.0.3"), 0);
        t.begin(&acct, Some("10.0.0.4"), 0).unwrap().succeeded();
        for _ in 0..MAX_ATTEMPTS - 1 {
            t.begin(&acct, Some("10.0.0.4"), 0).unwrap().failed(0);
        }
        // 10.0.0.3 still carries its two misses: one more fills its round.
        t.begin(&unknown("cy"), Some("10.0.0.3"), 0)
            .unwrap()
            .failed(0);
        assert!(t
            .retry_after_ms(&unknown("dee"), Some("10.0.0.3"), 0)
            .is_some());
    }

    #[test]
    fn strikes_are_forgotten_after_a_quiet_spell() {
        let t = LoginThrottle::new();
        let ada = unknown("ada");
        t.record_failure(&ada, None, 0);
        t.record_failure(&ada, None, 0);
        // Long after: the earlier two misses no longer count.
        let later = FORGET_AFTER_MS + 1;
        t.record_failure(&ada, None, later);
        assert_eq!(t.retry_after_ms(&ada, None, later), None);
        assert!(t.begin(&ada, None, later).is_ok());
    }

    /// Review F1: check and reservation are atomic, so a burst of parallel
    /// attempts gets at most one round's worth of guesses.
    #[test]
    fn parallel_attempts_reserve_strikes_up_front() {
        let t = LoginThrottle::new();
        let ada = LoginSubject::Account(PrincipalId::new_v7());
        let admitted: Vec<_> = (0..10)
            .filter_map(|i| t.begin(&ada, Some(&format!("10.1.0.{i}")), 0).ok())
            .collect();
        assert_eq!(admitted.len(), MAX_ATTEMPTS as usize);
        assert_eq!(
            t.begin(&ada, Some("10.1.1.1"), 0).err(),
            Some(IN_FLIGHT_RETRY_MS)
        );
        // An attempt that errors out (dropped unresolved) frees its slot
        // without a strike…
        let mut admitted = admitted;
        drop(admitted.pop());
        let again = t.begin(&ada, Some("10.1.1.1"), 0).unwrap();
        admitted.push(again);
        // …and resolving them all as misses starts the first wait.
        for ticket in admitted {
            ticket.failed(0);
        }
        assert_eq!(t.begin(&ada, None, 0).err(), Some(BACKOFF_STEPS_MS[0]));
    }

    /// Review F3: a full table never stops tracking a new key.
    #[test]
    fn a_full_table_evicts_instead_of_skipping() {
        let t = LoginThrottle::new();
        for i in 0..MAX_TRACKED_KEYS + 50 {
            t.record_failure(
                &unknown(&format!("junk{i}")),
                Some(&format!("2001:db8:{:x}::1", i)),
                1,
            );
        }
        let (_, unknown_n, addrs) = t.tracked();
        assert!(unknown_n <= MAX_TRACKED_KEYS && addrs <= MAX_TRACKED_KEYS);
        // A real account is tracked regardless of the junk…
        let ada = LoginSubject::Account(PrincipalId::new_v7());
        for i in 0..MAX_ATTEMPTS {
            t.begin(&ada, Some(&format!("2001:db8:ffff:{i}::1")), 2)
                .unwrap()
                .failed(2);
        }
        assert!(t.begin(&ada, Some("192.0.2.1"), 2).is_err());
        // …and so is a fresh unknown name and a fresh address.
        for _ in 0..MAX_ATTEMPTS {
            t.record_failure(&unknown("target"), Some("192.0.2.9"), 3);
        }
        assert!(t.retry_after_ms(&unknown("target"), None, 3).is_some());
        assert!(t
            .retry_after_ms(&unknown("x"), Some("192.0.2.9"), 3)
            .is_some());
    }

    #[test]
    fn ipv6_addresses_are_keyed_by_their_slash_64() {
        assert_eq!(addr_key("2001:db8:1:2:aaaa::1"), "2001:db8:1:2::/64");
        assert_eq!(addr_key("[2001:db8:1:2::9]:443"), "2001:db8:1:2::/64");
        assert_eq!(addr_key("10.0.0.1:5555"), "10.0.0.1");
        assert_eq!(addr_key("::ffff:10.0.0.1"), "10.0.0.1");
        assert_eq!(addr_key("unix:/run/x"), "unix:/run/x");
        let t = LoginThrottle::new();
        for i in 0..MAX_ATTEMPTS {
            t.record_failure(
                &unknown(&format!("u{i}")),
                Some(&format!("2001:db8::{i}")),
                0,
            );
        }
        assert!(t
            .retry_after_ms(&unknown("v"), Some("2001:db8::ffff"), 0)
            .is_some());
    }

    mod login {
        use super::super::*;
        use crate::server::operator::etc_files::tests::fake_etc;
        use crate::server::operator::provision::{NoReaper, Provisioner, UidRange};
        use crate::server::operator::{open_accounts, test_support, Opener};

        const PW: &str = "correct horse battery";

        fn attempt(username: &str, password: &str, addr: &str) -> LoginAttempt {
            LoginAttempt {
                username: username.into(),
                password: Zeroizing::new(password.into()),
                remote_addr: Some(addr.into()),
                user_agent: Some("test-agent".into()),
            }
        }

        async fn last_event(
            pool: &SqlitePool,
        ) -> (String, Option<String>, Option<String>, Option<String>) {
            sqlx::query_as(
                "SELECT kind, principal_id, username_tried, detail FROM auth_events ORDER BY id DESC LIMIT 1",
            )
            .fetch_one(pool)
            .await
            .unwrap()
        }

        #[tokio::test]
        async fn outcomes_are_recorded_and_indistinguishable_to_the_caller() {
            let (_root_tmp, root) = test_support::temp_root();
            let (etc_tmp, _etc) = fake_etc(true);
            let prov = Provisioner::for_tests(
                root.clone(),
                UidRange::new(3_900_000_040, 3_900_000_050).unwrap(),
                etc_tmp.path(),
            );
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            let ada = prov.create(&pool, "ada", PW, false, None).await.unwrap();
            prov.create(&pool, "bob", PW, false, None).await.unwrap();
            prov.disable(&pool, "bob", &NoReaper).await.unwrap();
            let v = LoginVerifier::new();

            let LoginOutcome::Ok(a) = v
                .login_at(&pool, attempt("ADA", PW, "10.0.0.1"), || 0)
                .await
                .unwrap()
            else {
                panic!("correct password, any case of the username");
            };
            assert_eq!(a.principal_id, ada.principal_id);
            let (kind, pid, tried, detail) = last_event(&pool).await;
            assert_eq!(
                (kind.as_str(), pid.as_deref(), detail),
                ("login_ok", Some(&*ada.principal_id.to_string()), None)
            );
            assert_eq!(tried.as_deref(), Some("ADA"));

            for (user, pw, reason) in [
                ("ada", "wrong password", "bad_password"),
                ("nobody", PW, "unknown_user"),
                ("bob", PW, "disabled"),
            ] {
                let out = v
                    .login_at(&pool, attempt(user, pw, "10.0.0.2"), || 0)
                    .await
                    .unwrap();
                assert!(matches!(out, LoginOutcome::Failed), "{user}: {out:?}");
                let (kind, pid, _, detail) = last_event(&pool).await;
                assert_eq!(kind, "login_fail");
                assert_eq!(pid.is_some(), user != "nobody");
                assert!(detail.unwrap().contains(reason));
            }
        }

        #[tokio::test]
        async fn three_misses_throttle_and_the_throttle_is_recorded() {
            let (_root_tmp, root) = test_support::temp_root();
            let (etc_tmp, _etc) = fake_etc(true);
            let prov = Provisioner::for_tests(
                root.clone(),
                UidRange::new(3_900_000_060, 3_900_000_070).unwrap(),
                etc_tmp.path(),
            );
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            prov.create(&pool, "ada", PW, false, None).await.unwrap();
            let v = LoginVerifier::new();
            for _ in 0..MAX_ATTEMPTS {
                let out = v
                    .login_at(&pool, attempt("ada", "nope nope", "10.0.0.9"), || 1_000)
                    .await
                    .unwrap();
                assert!(matches!(out, LoginOutcome::Failed));
            }
            // Even the right password is refused while the wait runs.
            let out = v
                .login_at(&pool, attempt("ada", PW, "10.0.0.8"), || 2_000)
                .await
                .unwrap();
            let LoginOutcome::Throttled { retry_after_ms } = out else {
                panic!("{out:?}");
            };
            assert_eq!(retry_after_ms, BACKOFF_STEPS_MS[0] - 1_000);
            assert_eq!(last_event(&pool).await.0, "login_throttled");
            // After the wait, it goes through.
            let later = 1_000 + BACKOFF_STEPS_MS[0];
            let out = v
                .login_at(&pool, attempt("ada", PW, "10.0.0.8"), || later)
                .await
                .unwrap();
            assert!(matches!(out, LoginOutcome::Ok(_)), "{out:?}");
        }

        /// Review S1-M3: a miss is stamped with the time verify finished, not
        /// the time the attempt began (argon2 may have queued meanwhile), so
        /// the backoff runs from when the last miss actually landed.
        #[tokio::test]
        async fn a_miss_is_stamped_after_verify_not_before() {
            let (_root_tmp, root) = test_support::temp_root();
            let (etc_tmp, _etc) = fake_etc(true);
            let prov = Provisioner::for_tests(
                root.clone(),
                UidRange::new(3_900_000_100, 3_900_000_110).unwrap(),
                etc_tmp.path(),
            );
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            prov.create(&pool, "ada", PW, false, None).await.unwrap();
            let v = LoginVerifier::new();
            // Each attempt begins at 1_000 and its verify ends at 9_000.
            for _ in 0..MAX_ATTEMPTS {
                let calls = std::sync::atomic::AtomicU32::new(0);
                let clock = || match calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => 1_000,
                    _ => 9_000,
                };
                let out = v
                    .login_at(&pool, attempt("ada", "nope nope", "10.4.0.1"), clock)
                    .await
                    .unwrap();
                assert!(matches!(out, LoginOutcome::Failed));
                assert_eq!(calls.into_inner(), 2, "begin, then after verify");
            }
            let out = v
                .login_at(&pool, attempt("ada", PW, "10.4.0.2"), || 9_000)
                .await
                .unwrap();
            let LoginOutcome::Throttled { retry_after_ms } = out else {
                panic!("{out:?}");
            };
            assert_eq!(retry_after_ms, BACKOFF_STEPS_MS[0]);
        }

        /// Review F1 end to end: a burst of wrong passwords for one account
        /// from many addresses gets at most MAX_ATTEMPTS through to argon2.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn a_concurrent_burst_reaches_verify_at_most_max_attempts_times() {
            let (_root_tmp, root) = test_support::temp_root();
            let (etc_tmp, _etc) = fake_etc(true);
            let prov = Provisioner::for_tests(
                root.clone(),
                UidRange::new(3_900_000_080, 3_900_000_090).unwrap(),
                etc_tmp.path(),
            );
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            prov.create(&pool, "ada", PW, false, None).await.unwrap();
            let v = std::sync::Arc::new(LoginVerifier::new());
            let burst = 4 * MAX_ATTEMPTS as usize;
            let tasks: Vec<_> = (0..burst)
                .map(|i| {
                    let (v, pool) = (v.clone(), pool.clone());
                    tokio::spawn(async move {
                        v.login_at(
                            &pool,
                            attempt("ada", "wrong guess", &format!("10.2.0.{i}")),
                            || 5,
                        )
                        .await
                        .unwrap()
                    })
                })
                .collect();
            let mut failed = 0;
            for t in tasks {
                match t.await.unwrap() {
                    LoginOutcome::Failed => failed += 1,
                    LoginOutcome::Throttled { .. } => {}
                    LoginOutcome::Ok(_) => panic!("wrong password accepted"),
                }
            }
            assert!(
                failed <= MAX_ATTEMPTS as usize,
                "{failed} guesses reached verify"
            );
            let verified: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM auth_events WHERE kind = 'login_fail'")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            let throttled: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM auth_events WHERE kind = 'login_throttled'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(verified as usize, failed);
            assert_eq!((verified + throttled) as usize, burst);
            // The round is spent: even the right password waits.
            assert!(matches!(
                v.login_at(&pool, attempt("ada", PW, "10.3.0.1"), || 6)
                    .await
                    .unwrap(),
                LoginOutcome::Throttled { .. }
            ));
        }
    }

    /// The daemon's copy must stay the app lock's steps (§6.2 names them).
    #[cfg(feature = "desktop")]
    #[test]
    fn backoff_steps_match_the_app_lock() {
        assert_eq!(
            BACKOFF_STEPS_MS,
            crate::commands::app_lock::BACKOFF_STEPS_MS
        );
        assert_eq!(MAX_ATTEMPTS, crate::commands::app_lock::MAX_ATTEMPTS);
    }
}
