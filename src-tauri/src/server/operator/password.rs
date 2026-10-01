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

/// Account password policy (not fixed by the contract; NIST 800-63B's
/// minimum, and a bound so a hash request can't be made arbitrarily large).
pub const MIN_PASSWORD_CHARS: usize = 8;
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ThrottleKey {
    Username(String),
    Addr(String),
}

#[derive(Debug, Clone, Default)]
struct Strikes {
    misses: u32,
    round: u32,
    wait_until_ms: u64,
    last_miss_ms: u64,
}

/// Cap on remembered keys; the oldest idle ones are dropped first.
const MAX_TRACKED_KEYS: usize = 10_000;

/// Failed-login backoff per username and per remote address (§6.2). In-memory
/// and per process: a broker restart forgets it, which costs an attacker a
/// restart they can't cause.
#[derive(Debug, Default)]
pub struct LoginThrottle {
    keys: Mutex<HashMap<ThrottleKey, Strikes>>,
}

fn step_ms(round: u32) -> u64 {
    BACKOFF_STEPS_MS[(round as usize).min(BACKOFF_STEPS_MS.len() - 1)]
}

const FORGET_AFTER_MS: u64 = BACKOFF_STEPS_MS[BACKOFF_STEPS_MS.len() - 1];

fn keys_for(username: &str, addr: Option<&str>) -> Vec<ThrottleKey> {
    let mut keys = vec![ThrottleKey::Username(username.to_lowercase())];
    if let Some(addr) = addr {
        keys.push(ThrottleKey::Addr(addr.to_string()));
    }
    keys
}

impl LoginThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Milliseconds left on the longest active wait among `username` and
    /// `addr`, or `None` when an attempt may proceed.
    pub fn retry_after_ms(&self, username: &str, addr: Option<&str>, now_ms: u64) -> Option<u64> {
        let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        keys_for(username, addr)
            .iter()
            .filter_map(|k| keys.get(k))
            .filter(|s| s.wait_until_ms > now_ms)
            .map(|s| s.wait_until_ms - now_ms)
            .max()
    }

    /// Count one wrong password against both keys.
    pub fn record_failure(&self, username: &str, addr: Option<&str>, now_ms: u64) {
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        if keys.len() >= MAX_TRACKED_KEYS {
            keys.retain(|_, s| {
                now_ms.saturating_sub(s.last_miss_ms.max(s.wait_until_ms)) < FORGET_AFTER_MS
            });
        }
        for key in keys_for(username, addr) {
            if keys.len() >= MAX_TRACKED_KEYS && !keys.contains_key(&key) {
                continue;
            }
            let s = keys.entry(key).or_default();
            // A key quiet for longer than the longest wait — counted from its
            // last miss or the end of its last wait, whichever is later —
            // starts over. Serving a wait does not by itself reset the round.
            if now_ms.saturating_sub(s.last_miss_ms.max(s.wait_until_ms)) >= FORGET_AFTER_MS {
                *s = Strikes::default();
            }
            s.misses += 1;
            s.last_miss_ms = now_ms;
            if s.misses >= MAX_ATTEMPTS {
                s.wait_until_ms = now_ms + step_ms(s.round);
                s.round += 1;
                s.misses = 0;
            }
        }
    }

    /// A correct password clears the username's strikes. The address keeps
    /// its own: one valid account must not reset a spray from the same host.
    pub fn record_success(&self, username: &str) {
        let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
        keys.remove(&ThrottleKey::Username(username.to_lowercase()));
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
#[derive(Debug, Default)]
pub struct LoginVerifier {
    throttle: LoginThrottle,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl LoginVerifier {
    pub fn new() -> Self {
        Self::default()
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
        self.login_at(pool, attempt, now_ms()).await
    }

    pub(crate) async fn login_at(
        &self,
        pool: &SqlitePool,
        attempt: LoginAttempt,
        now_ms: u64,
    ) -> anyhow::Result<LoginOutcome> {
        let LoginAttempt {
            username,
            password,
            remote_addr,
            user_agent,
        } = attempt;
        let addr_key = remote_addr.clone();
        let addr = addr_key.as_deref();

        if let Some(retry_after_ms) = self.throttle.retry_after_ms(&username, addr, now_ms) {
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

        let account = {
            let mut conn = pool.acquire().await?;
            accounts::by_username(&mut conn, &username).await?
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
            (None, Some(account)) => {
                self.throttle.record_success(&username);
                Ok(LoginOutcome::Ok(account))
            }
            _ => {
                self.throttle.record_failure(&username, addr, now_ms);
                Ok(LoginOutcome::Failed)
            }
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
        assert!(validate_new_password("eight ch").is_ok());
        assert_eq!(
            validate_new_password(&"x".repeat(MAX_PASSWORD_CHARS + 1)),
            Err(PasswordPolicyError::TooLong)
        );
    }

    #[test]
    fn backoff_walks_the_steps_per_round_and_repeats_the_last() {
        let t = LoginThrottle::new();
        let mut now = 1_000_000;
        for (round, step) in BACKOFF_STEPS_MS
            .iter()
            .chain(std::iter::once(&BACKOFF_STEPS_MS[3]))
            .enumerate()
        {
            for miss in 0..MAX_ATTEMPTS {
                assert_eq!(
                    t.retry_after_ms("ada", None, now),
                    None,
                    "round {round} miss {miss}"
                );
                t.record_failure("ada", None, now);
            }
            assert_eq!(
                t.retry_after_ms("ada", None, now),
                Some(*step),
                "round {round}"
            );
            now += step;
        }
    }

    #[test]
    fn backoff_is_per_username_case_insensitively_and_per_address() {
        let t = LoginThrottle::new();
        for _ in 0..MAX_ATTEMPTS {
            t.record_failure("Ada", Some("10.0.0.1"), 0);
        }
        assert!(t.retry_after_ms("ada", None, 0).is_some());
        // Same address, another username: the address is throttled too.
        assert!(t.retry_after_ms("bob", Some("10.0.0.1"), 0).is_some());
        // Another address, another username: free.
        assert_eq!(t.retry_after_ms("bob", Some("10.0.0.2"), 0), None);
        // Success clears the username, not the address.
        t.record_success("ADA");
        assert_eq!(t.retry_after_ms("ada", None, 0), None);
        assert!(t.retry_after_ms("ada", Some("10.0.0.1"), 0).is_some());
    }

    #[test]
    fn strikes_are_forgotten_after_a_quiet_spell() {
        let t = LoginThrottle::new();
        t.record_failure("ada", None, 0);
        t.record_failure("ada", None, 0);
        // Long after: the earlier two misses no longer count.
        let later = FORGET_AFTER_MS + 1;
        t.record_failure("ada", None, later);
        assert_eq!(t.retry_after_ms("ada", None, later), None);
    }

    mod login {
        use super::super::*;
        use crate::server::operator::etc_files::tests::fake_etc;
        use crate::server::operator::provision::{Provisioner, ReaperPendingT1Executor, UidRange};
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
            prov.disable(&pool, "bob", &ReaperPendingT1Executor)
                .await
                .unwrap();
            let v = LoginVerifier::new();

            let LoginOutcome::Ok(a) = v
                .login_at(&pool, attempt("ADA", PW, "10.0.0.1"), 0)
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
                    .login_at(&pool, attempt(user, pw, "10.0.0.2"), 0)
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
                    .login_at(&pool, attempt("ada", "nope nope", "10.0.0.9"), 1_000)
                    .await
                    .unwrap();
                assert!(matches!(out, LoginOutcome::Failed));
            }
            // Even the right password is refused while the wait runs.
            let out = v
                .login_at(&pool, attempt("ada", PW, "10.0.0.8"), 2_000)
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
                .login_at(&pool, attempt("ada", PW, "10.0.0.8"), later)
                .await
                .unwrap();
            assert!(matches!(out, LoginOutcome::Ok(_)), "{out:?}");
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
