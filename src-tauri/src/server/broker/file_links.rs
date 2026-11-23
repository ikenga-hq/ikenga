//! The public outside-link routes, `/s/*` (box-files WP-16; the design is
//! `plans/2026-11-20-box-files-sharing/06-file-links-security.md` in the
//! workspace meta-repo, cited below as "§N").
//!
//! | Method + path | Answer |
//! |---|---|
//! | `GET, HEAD /s/:token` | the page, the passcode form, or "locked" (§5.1) |
//! | `POST /s/:token` | passcode submit → `303` + the unlock cookie (§2.4) |
//! | `GET, HEAD /s/:token/view` | inline bytes, preview kinds only (§5.1) |
//! | `GET, HEAD /s/:token/download` | attachment bytes, every kind |
//! | anything else under `/s` | the uniform "not available" page (§6.1) |
//!
//! "Anything else" includes `OPTIONS`: [`front`] sits outside the CORS
//! layer and the `Origin` gate and sends every method but `GET`, `HEAD`
//! and `POST` straight to the handler, so no preflight 200 is ever answered
//! here (LS-15, A-03). It also puts the `/s` headers on every `/s` answer,
//! the `Origin` gate's 403 included.
//!
//! Unauthenticated and served by the root broker for people with **no
//! account**. The routes are always registered (otherwise `/s/…` would fall
//! to the SPA and answer 200 with the app's index.html); the
//! `--file-links` setting is checked in the handler on every request, and
//! off, everything is the uniform 404.
//!
//! What the broker does and does not do here:
//!
//! * It never reads the app's session or device cookie, never calls the auth
//!   backend, and sets no cookie but `ikenga_link`. The routes are mounted
//!   **outside** the session layer (06 O-8: so a signed-in visitor's session
//!   can never be refreshed or rotated by a `/s/*` response), inside the
//!   `Origin` gate (a cross-origin passcode `POST` is refused, LS-13).
//! * It never touches a principal's path itself (it is root). The bytes come
//!   from the **owner's own child** over the child-only
//!   `/internal/file-link/read` (`server::file_link_read`), with the
//!   per-child bearer and the internal-call header. No header of the
//!   outsider's request is forwarded and the token never leaves this module.
//! * It takes only the body and the numeric `Content-Length` from the child
//!   and sets every response header itself from the root-owned row (§1.5),
//!   and it cuts the stream at `min(Content-Length, cap)`.
//! * The page is rendered from the row alone and never launches a child; a
//!   byte request launches one only once the link is proven live, behind a
//!   box-wide cold-launch semaphore (§1.3).
//! * Throttles (§6.2) are in memory, keyed by the client address (IPv4, or
//!   IPv6 /64) as `server::trusted_proxy` resolves it.
//! * Logs name the `link_id` or nothing: never the URI, the token, the
//!   cookie or the passcode (§6.3).
//!
//! Opens (WP-18, §8): a page view, a `/view` or a `/download` the child
//! answered 200 is noted off the request path ([`note_open`]): a coalesced
//! `link.opened` / `link.downloaded` audit row (one per link, visitor
//! address, mode and hour) that bumps `open_count` / `download_count` and
//! `last_opened_at`, and, for the first byte request only, a conditional
//! `first_opened_at` UPDATE whose single winner notifies the owner. A lock
//! is noted as `link.locked`. All of it is best effort and never delays or
//! fails the response. A `HEAD` answers like the `GET` but is never an open
//! (A-04): a link scanner's probe counts, audits and notifies nothing.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use base64::Engine;
use futures_util::{Stream, StreamExt};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::children::ChildEndpoint;
use super::md_preview;
use super::proxy::PRINCIPAL_HEADER;
use super::BrokerState;
use crate::access::audit::{self, AuditVia, Event};
use crate::access::file_links::{self as links, PreviewKind, LINK_MAX_BYTES};
use crate::access::pairing::throttle_key;
use crate::access::share::{broker_host, now_ms, OwnerCalls};
use crate::access::AccessStore;
use crate::executor::{Principal, PrincipalId};
use crate::server::operator::accounts;
use crate::server::shared::download;
use crate::server::trusted_proxy;

type HmacSha256 = Hmac<Sha256>;

/// The unlock cookie's name (§2.4).
pub const COOKIE_NAME: &str = "ikenga_link";
/// How long one unlock lasts (§2.4).
pub const UNLOCK_TTL_MS: i64 = 60 * 60 * 1000;
const UNLOCK_DOMAIN: &[u8] = b"ikenga-link-unlock\0";
/// `link_id` (36) ‖ `exp_ms` (8, big-endian) ‖ HMAC-SHA256 (32).
const UNLOCK_RAW_LEN: usize = 36 + 8 + 32;

const MINUTE_MS: i64 = 60 * 1000;
/// Requests to `/s/*` per address per minute (token bucket, §6.2).
pub const REQUESTS_PER_MINUTE: f64 = 120.0;
/// Not-available outcomes per address per window (§6.2).
pub const MISS_MAX: usize = 30;
pub const MISS_WINDOW_MS: i64 = 10 * MINUTE_MS;
/// Wrong passcodes per (address, link) per window (§2.3).
pub const PASS_ADDR_MAX: usize = 5;
pub const PASS_ADDR_WINDOW_MS: i64 = 10 * MINUTE_MS;
/// Wrong passcodes per link per window, then the link locks (§2.3).
pub const PASS_LINK_MAX: i64 = 20;
pub const PASS_LINK_WINDOW_MS: i64 = 24 * 60 * MINUTE_MS;
pub const LOCK_MS: i64 = 60 * MINUTE_MS;
/// Concurrent byte streams per link, and on the whole box (§6.2).
pub const STREAMS_PER_LINK: usize = 4;
pub const STREAMS_TOTAL: usize = 32;
/// Cold child launches in flight on the whole box (§1.3).
pub const COLD_LAUNCHES: usize = 2;
/// Passcode verifications in flight (§2.2): its own gate, not the login's.
pub const PASSCODE_VERIFICATIONS: usize = 1;
/// The largest passcode form body read.
const MAX_FORM_BYTES: usize = 4 * 1024;
/// In-memory maps are swept past this many keys (the invite throttle's rule).
const SWEEP_AT: usize = 4096;

/// The only `mime` values a row can hold (the `file_links.mime` CHECK).
const STORED_MIMES: &[&str] = &[
    "application/pdf",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "text/plain; charset=utf-8",
    links::OCTET_STREAM,
];

// ─── state ──────────────────────────────────────────────────────────────────

/// How the broker was started, for the link routes.
pub struct LinkConfig {
    /// `--file-links` / `IKENGA_FILE_LINKS` (§7.1).
    pub enabled: bool,
    /// `--public-url`: only its host is shown on the page.
    pub public_url: Option<String>,
    /// `--insecure-cookie`: drop `Secure` from the unlock cookie.
    pub insecure_cookie: bool,
    /// The access store holding `file_links` (T1: `operator/accounts.db`).
    pub store: Option<AccessStore>,
}

/// Checks a passcode against its stored PHC. Blocking (Argon2id).
pub type PasscodeVerifier = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// A sliding-window counter per key (the `InviteThrottle` shape, with its
/// own limit).
struct WindowCounter {
    max: usize,
    window_ms: i64,
    hits: Mutex<HashMap<String, Vec<i64>>>,
}

impl WindowCounter {
    fn new(max: usize, window_ms: i64) -> Self {
        Self {
            max,
            window_ms,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Whether `key` has used its budget.
    fn over(&self, key: &str, now: i64) -> bool {
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let Some(v) = map.get_mut(key) else {
            return false;
        };
        v.retain(|t| now - t < self.window_ms);
        v.len() >= self.max
    }

    fn hit(&self, key: &str, now: i64) {
        let mut map = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let v = map.entry(key.to_string()).or_default();
        v.retain(|t| now - t < self.window_ms);
        v.push(now);
        if map.len() > SWEEP_AT {
            let w = self.window_ms;
            map.retain(|_, v| v.last().is_some_and(|t| now - t < w));
        }
    }
}

/// A token bucket per key: `capacity` requests, refilled evenly per minute.
struct Buckets {
    capacity: f64,
    map: Mutex<HashMap<String, (f64, i64)>>,
}

impl Buckets {
    fn new(capacity: f64) -> Self {
        Self {
            capacity,
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token; `false` when the bucket is empty.
    fn take(&self, key: &str, now: i64) -> bool {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        let cap = self.capacity;
        let (tokens, at) = map.entry(key.to_string()).or_insert((cap, now));
        let elapsed = (now - *at).max(0) as f64;
        *tokens = (*tokens + elapsed * cap / MINUTE_MS as f64).min(cap);
        *at = now;
        let ok = *tokens >= 1.0;
        if ok {
            *tokens -= 1.0;
        }
        if map.len() > SWEEP_AT {
            // A bucket untouched for a minute is full again: forget it.
            map.retain(|_, (_, at)| now - *at < MINUTE_MS);
        }
        ok
    }
}

/// The link routes' state: the setting, the row store, the boot-time unlock
/// key, the throttles and the semaphores (§2, §6.2).
pub struct LinkGate {
    enabled: bool,
    store: Option<AccessStore>,
    public_host: Option<String>,
    insecure_cookie: bool,
    /// K (§2.4): 32 random bytes per broker boot, in memory only. A restart
    /// asks for the passcode again.
    unlock_key: [u8; 32],
    verify: PasscodeVerifier,
    requests: Buckets,
    misses: WindowCounter,
    pass_fails: WindowCounter,
    link_streams: Mutex<HashMap<String, Arc<Semaphore>>>,
    streams: Arc<Semaphore>,
    cold: Arc<Semaphore>,
    passcode_gate: Arc<Semaphore>,
    /// How the broker reaches an owner's child for the first-open
    /// notification (§8.3). `None`: the installed broker host's.
    calls: Option<Arc<dyn OwnerCalls>>,
    /// Passcode verifications started (LS-11 counts them).
    pub(crate) verifications: AtomicUsize,
}

impl LinkGate {
    /// The default before broker boot configures it: every `/s/*` answer is
    /// the uniform 404.
    pub fn disabled() -> Self {
        Self::new(LinkConfig {
            enabled: false,
            public_url: None,
            insecure_cookie: false,
            store: None,
        })
    }

    pub fn new(cfg: LinkConfig) -> Self {
        let mut unlock_key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut unlock_key);
        let public_host = cfg
            .public_url
            .as_deref()
            .and_then(|u| url::Url::parse(u.trim()).ok())
            .and_then(|u| u.host_str().map(str::to_string));
        Self {
            enabled: cfg.enabled,
            store: cfg.store,
            public_host,
            insecure_cookie: cfg.insecure_cookie,
            unlock_key,
            verify: Arc::new(|phc: &str, passcode: &str| {
                crate::server::operator::password::verify_blocking(phc, passcode)
            }),
            requests: Buckets::new(REQUESTS_PER_MINUTE),
            misses: WindowCounter::new(MISS_MAX, MISS_WINDOW_MS),
            pass_fails: WindowCounter::new(PASS_ADDR_MAX, PASS_ADDR_WINDOW_MS),
            link_streams: Mutex::new(HashMap::new()),
            streams: Arc::new(Semaphore::new(STREAMS_TOTAL)),
            cold: Arc::new(Semaphore::new(COLD_LAUNCHES)),
            passcode_gate: Arc::new(Semaphore::new(PASSCODE_VERIFICATIONS)),
            calls: None,
            verifications: AtomicUsize::new(0),
        }
    }

    /// Swap the passcode check (tests: a cheap stand-in for Argon2id).
    pub fn with_verifier(mut self, verify: PasscodeVerifier) -> Self {
        self.verify = verify;
        self
    }

    /// Reach owner children through `calls` for notifications (tests).
    pub fn with_calls(mut self, calls: Arc<dyn OwnerCalls>) -> Self {
        self.calls = Some(calls);
        self
    }

    /// Note an open of a live link, spawned and best effort (§8).
    fn spawn_open(&self, live: &Live, mode: OpenMode, visitor: &Visitor, now: i64) {
        let Some(store) = self.live_store().cloned() else {
            return;
        };
        let calls = self
            .calls
            .clone()
            .or_else(|| broker_host().map(|h| h.calls.clone()));
        let note = OpenNote {
            link_id: live.row.link_id.clone(),
            owner: live.owner.id,
            owner_text: live.row.owner.clone(),
            name: live.row.name.clone(),
            mode,
            visitor: visitor.clone(),
            now,
        };
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(note_open(store, calls, note));
        }
    }

    /// Note that a link just locked (§8.1 `link.locked`), spawned.
    fn spawn_locked(&self, live: &Live, visitor: &Visitor, until: i64) {
        let Some(store) = self.live_store().cloned() else {
            return;
        };
        let ev = outsider_event("link.locked", &live.row, visitor)
            .detail(serde_json::json!({ "name": live.row.name, "until": until }));
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                if let Err(e) = audit::record(&store, &ev).await {
                    tracing::warn!("audit link.locked not recorded: {e}");
                }
            });
        }
    }

    /// The store, only while the setting is on.
    fn live_store(&self) -> Option<&AccessStore> {
        if self.enabled {
            self.store.as_ref()
        } else {
            None
        }
    }

    /// A stream slot for one link (at most [`STREAMS_PER_LINK`]).
    fn link_stream(&self, link_id: &str) -> Option<OwnedSemaphorePermit> {
        let sem = {
            let mut map = self.link_streams.lock().unwrap_or_else(|e| e.into_inner());
            if map.len() > SWEEP_AT {
                map.retain(|_, s| s.available_permits() < STREAMS_PER_LINK);
            }
            map.entry(link_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(STREAMS_PER_LINK)))
                .clone()
        };
        sem.try_acquire_owned().ok()
    }

    /// The unlock cookie's value for `link_id` until `exp_ms` (§2.4).
    fn mint_unlock(&self, link_id: &str, exp_ms: i64) -> Option<String> {
        if link_id.len() != 36 {
            return None;
        }
        let mut mac = HmacSha256::new_from_slice(&self.unlock_key).ok()?;
        mac.update(UNLOCK_DOMAIN);
        mac.update(link_id.as_bytes());
        mac.update(&exp_ms.to_be_bytes());
        let tag = mac.finalize().into_bytes();
        let mut raw = Vec::with_capacity(UNLOCK_RAW_LEN);
        raw.extend_from_slice(link_id.as_bytes());
        raw.extend_from_slice(&exp_ms.to_be_bytes());
        raw.extend_from_slice(&tag);
        Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw))
    }

    /// Whether one value is a valid unlock for `link_id` at `now`:
    /// constant-time MAC check, bound to the link and to its expiry.
    fn unlock_value_ok(&self, value: &str, link_id: &str, now: i64) -> bool {
        let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value) else {
            return false;
        };
        if raw.len() != UNLOCK_RAW_LEN {
            return false;
        }
        let (id, rest) = raw.split_at(36);
        let (exp, tag) = rest.split_at(8);
        let Ok(exp_bytes) = <[u8; 8]>::try_from(exp) else {
            return false;
        };
        let exp_ms = i64::from_be_bytes(exp_bytes);
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.unlock_key) else {
            return false;
        };
        mac.update(UNLOCK_DOMAIN);
        mac.update(id);
        mac.update(exp);
        mac.verify_slice(tag).is_ok() && id == link_id.as_bytes() && exp_ms > now
    }

    /// Whether the request carries a valid unlock cookie for `link_id`.
    fn unlocked(&self, headers: &HeaderMap, link_id: &str, now: i64) -> bool {
        cookie_values(headers).any(|v| self.unlock_value_ok(&v, link_id, now))
    }

    fn set_cookie(&self, token: &str, value: &str) -> String {
        format!(
            "{COOKIE_NAME}={value}; Path=/s/{token}; Max-Age={}; HttpOnly; SameSite=Lax{}",
            UNLOCK_TTL_MS / 1000,
            if self.insecure_cookie { "" } else { "; Secure" }
        )
    }
}

/// Every `ikenga_link` value in the request's `Cookie` headers (several
/// can arrive: one per link path).
fn cookie_values(headers: &HeaderMap) -> impl Iterator<Item = String> + '_ {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| {
            pair.trim()
                .strip_prefix(COOKIE_NAME)
                .and_then(|r| r.strip_prefix('='))
                .map(str::to_string)
        })
}

// ─── opens: audit, counters, first-open notification (WP-18, §8) ─────────────

/// Who is asking: the throttle key (IPv4, or the IPv6 /64), the address as
/// the trusted proxy resolved it, and the user agent.
#[derive(Clone, Debug)]
struct Visitor {
    key: String,
    addr: String,
    agent: Option<String>,
}

/// §8.1: the user agent is kept to 256 characters.
const AGENT_MAX_CHARS: usize = 256;

fn visitor_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(AGENT_MAX_CHARS).collect())
        .filter(|s: &String| !s.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenMode {
    Page,
    View,
    Download,
}

impl OpenMode {
    fn as_str(self) -> &'static str {
        match self {
            OpenMode::Page => "page",
            OpenMode::View => "view",
            OpenMode::Download => "download",
        }
    }

    fn kind(self) -> &'static str {
        match self {
            OpenMode::Download => "link.downloaded",
            _ => "link.opened",
        }
    }
}

struct OpenNote {
    link_id: String,
    owner: PrincipalId,
    owner_text: String,
    name: String,
    mode: OpenMode,
    visitor: Visitor,
    now: i64,
}

/// An outsider's audit row: no actor (`via = system`), the owner as subject
/// so the owner sees it, the link as target, the visitor's address and agent.
/// Never the token, passcode, cookie or path (§8.1).
fn outsider_event(kind: &'static str, row: &LinkRow, visitor: &Visitor) -> Event {
    let mut ev = Event::new(kind, AuditVia::System)
        .subject_principal(row.owner.clone())
        .target(row.link_id.clone());
    ev.remote_addr = Some(visitor.addr.clone());
    ev.user_agent = visitor.agent.clone();
    ev
}

/// `UPDATE … WHERE first_opened_at IS NULL`: `true` for the one caller whose
/// update changed the row, however many race (§8.2).
async fn claim_first_open(pool: &SqlitePool, link_id: &str, now: i64) -> bool {
    match sqlx::query(
        "UPDATE file_links SET first_opened_at = ?1 WHERE link_id = ?2 AND first_opened_at IS NULL",
    )
    .bind(now)
    .bind(link_id)
    .execute(pool)
    .await
    {
        Ok(r) => r.rows_affected() == 1,
        Err(e) => {
            tracing::warn!("file link first-open update failed: {e}");
            false
        }
    }
}

/// §8.3: a notification on the owner's child, then a push by id.
async fn notify_first_open(calls: &dyn OwnerCalls, owner: PrincipalId, name: &str) {
    match calls
        .call(
            owner,
            "notifications_record_access",
            serde_json::json!({
                "kind": "link",
                "title": "Someone opened your link",
                "body": name,
            }),
        )
        .await
    {
        Ok(v) => {
            if let Some(id) = v.get("id").and_then(serde_json::Value::as_i64) {
                crate::server::push::emit(crate::server::push::PushEvent::new(
                    Some(owner),
                    crate::server::push::PushKind::Invite,
                    format!("n:{id}"),
                ));
            }
        }
        Err(e) => tracing::warn!("file link opened, but the owner's notification failed: {e}"),
    }
}

/// Everything an open does, in order: the first-open claim and notification
/// (byte requests only: a page view is not a first open, §8.2), then the
/// coalesced audit row and, when it was written, the counters (§8.1).
async fn note_open(store: AccessStore, calls: Option<Arc<dyn OwnerCalls>>, n: OpenNote) {
    if n.mode != OpenMode::Page && claim_first_open(store.pool(), &n.link_id, n.now).await {
        if let Some(calls) = &calls {
            notify_first_open(calls.as_ref(), n.owner, &n.name).await;
        }
    }
    let window = format!("{}\u{0}{}\u{0}{}", n.link_id, n.visitor.key, n.mode.as_str());
    if !audit::link_window_opens(window, n.now) {
        return;
    }
    let row = LinkRow::stub(&n.link_id, &n.owner_text);
    let detail = if n.mode == OpenMode::Download {
        serde_json::json!({ "name": n.name })
    } else {
        serde_json::json!({ "name": n.name, "mode": n.mode.as_str() })
    };
    let ev = outsider_event(n.mode.kind(), &row, &n.visitor).detail(detail);
    if let Err(e) = audit::record(&store, &ev).await {
        tracing::warn!("audit {} not recorded: {e}", n.mode.kind());
        return;
    }
    let sql = if n.mode == OpenMode::Download {
        "UPDATE file_links SET download_count = download_count + 1, last_opened_at = ?1 WHERE link_id = ?2"
    } else {
        "UPDATE file_links SET open_count = open_count + 1, last_opened_at = ?1 WHERE link_id = ?2"
    };
    if let Err(e) = sqlx::query(sql)
        .bind(n.now)
        .bind(&n.link_id)
        .execute(store.pool())
        .await
    {
        tracing::warn!("file link counter update failed: {e}");
    }
}

// ─── the row ────────────────────────────────────────────────────────────────

/// What a request needs of one `file_links` row. Never the token hash.
struct LinkRow {
    link_id: String,
    owner: String,
    path: String,
    name: String,
    preview_kind: String,
    mime: String,
    size_at_create: i64,
    expires_at: Option<i64>,
    passcode_phc: Option<String>,
    locked_until: Option<i64>,
    revoked_at: Option<i64>,
}

async fn fetch_row(pool: &SqlitePool, token: &str) -> Option<LinkRow> {
    let hash = links::token_hash(token);
    let row = sqlx::query(
        "SELECT link_id, owner_principal_id, path, name, preview_kind, mime, size_at_create, \
           expires_at, passcode_phc, passcode_locked_until, revoked_at \
         FROM file_links WHERE token_sha256 = ?",
    )
    .bind(hash.as_slice())
    .fetch_optional(pool)
    .await;
    let row = match row {
        Ok(r) => r?,
        Err(e) => {
            tracing::warn!("file link lookup failed: {e}");
            return None;
        }
    };
    Some(LinkRow {
        link_id: row.try_get("link_id").ok()?,
        owner: row.try_get("owner_principal_id").ok()?,
        path: row.try_get("path").ok()?,
        name: row.try_get("name").ok()?,
        preview_kind: row.try_get("preview_kind").ok()?,
        mime: row.try_get("mime").ok()?,
        size_at_create: row.try_get("size_at_create").ok()?,
        expires_at: row.try_get("expires_at").ok()?,
        passcode_phc: row.try_get("passcode_phc").ok()?,
        locked_until: row.try_get("passcode_locked_until").ok()?,
        revoked_at: row.try_get("revoked_at").ok()?,
    })
}

impl LinkRow {
    /// Just the ids an audit row needs.
    fn stub(link_id: &str, owner: &str) -> Self {
        Self {
            link_id: link_id.to_string(),
            owner: owner.to_string(),
            path: String::new(),
            name: String::new(),
            preview_kind: String::new(),
            mime: String::new(),
            size_at_create: 0,
            expires_at: None,
            passcode_phc: None,
            locked_until: None,
            revoked_at: None,
        }
    }
}

/// A link that is live right now, with its owner's spawn identity.
struct Live {
    row: LinkRow,
    owner: Principal,
}

/// §1.3's order after the shape check and the throttle: row by hash → not
/// revoked, not expired → setting on → owner exists and is enabled. Every
/// failure is `None` (the uniform answer).
async fn live_link(state: &BrokerState, gate: &LinkGate, token: &str, now: i64) -> Option<Live> {
    let store = gate.live_store()?;
    let row = fetch_row(store.pool(), token).await?;
    if row.revoked_at.is_some() || row.expires_at.is_some_and(|e| e <= now) {
        return None;
    }
    let id: PrincipalId = row.owner.parse().ok()?;
    let mut conn = state.pool.acquire().await.ok()?;
    let account = accounts::by_id(&mut conn, id).await.ok()??;
    if account.is_disabled() {
        return None;
    }
    Some(Live {
        owner: account.principal(),
        row,
    })
}

/// A wrong passcode against the row (§2.3): a rolling 24 h count; at
/// [`PASS_LINK_MAX`] the link locks for [`LOCK_MS`] and the count resets.
/// `true` when this attempt locked it.
async fn record_wrong_passcode(pool: &SqlitePool, link_id: &str, now: i64) -> bool {
    let failures: Option<i64> = match sqlx::query_scalar(
        "UPDATE file_links SET \
           passcode_failures = CASE WHEN passcode_window_start IS NULL \
               OR ?1 - passcode_window_start >= ?2 THEN 1 ELSE passcode_failures + 1 END, \
           passcode_window_start = CASE WHEN passcode_window_start IS NULL \
               OR ?1 - passcode_window_start >= ?2 THEN ?1 ELSE passcode_window_start END \
         WHERE link_id = ?3 RETURNING passcode_failures",
    )
    .bind(now)
    .bind(PASS_LINK_WINDOW_MS)
    .bind(link_id)
    .fetch_optional(pool)
    .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(link_id = %link_id, "recording a wrong passcode failed: {e}");
            None
        }
    };
    if failures.unwrap_or(0) < PASS_LINK_MAX {
        return false;
    }
    if let Err(e) = sqlx::query(
        "UPDATE file_links SET passcode_locked_until = ?, passcode_failures = 0, \
           passcode_window_start = NULL WHERE link_id = ?",
    )
    .bind(now + LOCK_MS)
    .bind(link_id)
    .execute(pool)
    .await
    {
        tracing::warn!(link_id = %link_id, "locking a file link failed: {e}");
        return false;
    }
    tracing::warn!(link_id = %link_id, "file link locked after too many wrong passcodes");
    true
}

// ─── routes ─────────────────────────────────────────────────────────────────

/// Whether `path` is one of the link routes' paths.
fn is_link_path(path: &str) -> bool {
    path == "/s" || path.starts_with("/s/")
}

/// The outermost middleware of [`super::router`], for `/s` paths only (any
/// other request passes untouched). LS-15 / A-03:
///
/// * a method other than `GET`, `HEAD` and `POST` (`OPTIONS`, `PUT`,
///   `DELETE` …) goes straight to [`handle`], past the CORS layer (which
///   would answer a preflight `OPTIONS` 200) and the `Origin` gate: the
///   answer is the uniform 404, counted as a miss like any other;
/// * every `/s` answer, whoever made it (the `Origin` gate's 403 for a
///   cross-origin `POST` too), leaves with `Referrer-Policy`,
///   `X-Robots-Tag`, `X-Content-Type-Options` and, unless the answer set its
///   own, `Cache-Control: no-store`.
pub async fn front(
    State(state): State<Arc<BrokerState>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    req: Request,
    next: Next,
) -> Response {
    if !is_link_path(req.uri().path()) {
        return next.run(req).await;
    }
    let mut res = if matches!(*req.method(), Method::GET | Method::HEAD | Method::POST) {
        next.run(req).await
    } else {
        handle(State(state), conn, req).await
    };
    let h = res.headers_mut();
    common_headers(h);
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, hv("no-store"));
    }
    res
}

/// The `/s` routes. Merged by [`super::router`] outside the session layer,
/// inside the `Origin` gate; [`front`] wraps the whole router.
pub fn routes(state: Arc<BrokerState>) -> Router {
    Router::new()
        .route("/s", any(handle))
        .route("/s/", any(handle))
        .route("/s/*rest", any(handle))
        .with_state(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Page,
    View,
    Download,
}

/// `/s/<token>[/view|/download]` → the token and what is asked; `None` for
/// anything else, a malformed token included (§2.1's shape check).
fn parse_route(path: &str) -> Option<(&str, Action)> {
    let rest = path.strip_prefix("/s/")?;
    let mut parts = rest.split('/');
    let token = parts.next()?;
    let action = match (parts.next(), parts.next()) {
        (None, _) => Action::Page,
        (Some("view"), None) => Action::View,
        (Some("download"), None) => Action::Download,
        _ => return None,
    };
    links::looks_like_token(token).then_some((token, action))
}

enum Outcome {
    /// The uniform 404; counts against the address (§6.2).
    NotAvailable,
    Done(Response),
}

async fn handle(
    State(state): State<Arc<BrokerState>>,
    conn: Option<ConnectInfo<SocketAddr>>,
    req: Request,
) -> Response {
    let gate = state.file_links.clone();
    let now = now_ms();
    let addr = trusted_proxy::addr_of_conn(&conn, req.headers());
    let key = throttle_key(&addr);
    let visitor = Visitor {
        key: key.clone(),
        addr,
        agent: visitor_agent(req.headers()),
    };
    // Throttled addresses get 429 for every /s/* request, valid links too,
    // so the 429 is not an oracle (§6.2).
    if !gate.requests.take(&key, now) || gate.misses.over(&key, now) {
        return slow_down();
    }
    let (parts, body) = req.into_parts();
    let read = matches!(parts.method, Method::GET | Method::HEAD);
    // A-04: a HEAD (a link scanner's, say) gets the same status and headers
    // as a GET but is not an open: no counter, no first open, no audit row,
    // no notification.
    let count = parts.method != Method::HEAD;
    let outcome = match parse_route(parts.uri.path()) {
        Some((token, Action::Page)) if read => {
            page_get(&state, &gate, token, &parts.headers, &visitor, count, now).await
        }
        Some((token, Action::Page)) if parts.method == Method::POST => {
            passcode_post(&state, &gate, token, &visitor, body, now).await
        }
        Some((token, action @ (Action::View | Action::Download))) if read => {
            let inline = action == Action::View;
            bytes(
                &state,
                &gate,
                token,
                &parts.headers,
                &visitor,
                inline,
                count,
                now,
            )
            .await
        }
        _ => Outcome::NotAvailable,
    };
    match outcome {
        Outcome::NotAvailable => {
            gate.misses.hit(&key, now);
            not_available()
        }
        Outcome::Done(res) => res,
    }
}

/// `GET|HEAD /s/:token`: rendered from the row alone; never launches a
/// child. `count`: whether this is an open (false for HEAD).
async fn page_get(
    state: &BrokerState,
    gate: &LinkGate,
    token: &str,
    headers: &HeaderMap,
    visitor: &Visitor,
    count: bool,
    now: i64,
) -> Outcome {
    let Some(live) = live_link(state, gate, token, now).await else {
        return Outcome::NotAvailable;
    };
    let row = &live.row;
    if row.passcode_phc.is_some() && !gate.unlocked(headers, &row.link_id, now) {
        if row.locked_until.is_some_and(|l| l > now) {
            return Outcome::Done(locked());
        }
        return Outcome::Done(passcode_form(gate, token, None, StatusCode::OK));
    }
    // A page view is audited (link.opened, mode "page", coalesced) but is NOT
    // a first open (§8.2: an unfurler fetches only the HTML).
    if count {
        gate.spawn_open(&live, OpenMode::Page, visitor, now);
    }
    Outcome::Done(file_page(gate, token, row))
}

/// `POST /s/:token`: the passcode (§2.2-§2.4).
async fn passcode_post(
    state: &BrokerState,
    gate: &LinkGate,
    token: &str,
    visitor: &Visitor,
    body: Body,
    now: i64,
) -> Outcome {
    let Some(live) = live_link(state, gate, token, now).await else {
        return Outcome::NotAvailable;
    };
    let row = &live.row;
    let Some(phc) = row.passcode_phc.clone() else {
        return Outcome::Done(see_other(token, None));
    };
    if row.locked_until.is_some_and(|l| l > now) {
        return Outcome::Done(locked());
    }
    // Before Argon2: a throttled address costs no CPU (§2.3).
    let pair = format!("{}|{}", visitor.key, row.link_id);
    if gate.pass_fails.over(&pair, now) {
        return Outcome::Done(slow_down());
    }
    let Ok(form) = axum::body::to_bytes(body, MAX_FORM_BYTES).await else {
        return Outcome::Done(passcode_form(
            gate,
            token,
            Some("Enter the passcode."),
            StatusCode::BAD_REQUEST,
        ));
    };
    let given = url::form_urlencoded::parse(&form)
        .find(|(k, _)| k == "passcode")
        .map(|(_, v)| v.into_owned())
        .filter(|p| !p.is_empty() && p.chars().count() <= links::PASSCODE_MAX_CHARS);
    let Some(given) = given else {
        return Outcome::Done(passcode_form(
            gate,
            token,
            Some("Enter the passcode."),
            StatusCode::BAD_REQUEST,
        ));
    };
    let Ok(permit) = gate.passcode_gate.clone().try_acquire_owned() else {
        return Outcome::Done(slow_down());
    };
    gate.verifications.fetch_add(1, Ordering::SeqCst);
    let verify = gate.verify.clone();
    let right = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify(&phc, &given)
    })
    .await
    .unwrap_or(false);
    if right {
        let Some(value) = gate.mint_unlock(&row.link_id, now + UNLOCK_TTL_MS) else {
            return Outcome::NotAvailable;
        };
        return Outcome::Done(see_other(token, Some(gate.set_cookie(token, &value))));
    }
    gate.pass_fails.hit(&pair, now);
    tracing::info!(link_id = %row.link_id, "wrong passcode for a file link");
    let Some(store) = gate.live_store() else {
        return Outcome::NotAvailable;
    };
    if record_wrong_passcode(store.pool(), &row.link_id, now).await {
        gate.spawn_locked(&live, visitor, now + LOCK_MS);
        return Outcome::Done(locked());
    }
    Outcome::Done(passcode_form(
        gate,
        token,
        Some("That passcode isn't right."),
        StatusCode::OK,
    ))
}

/// Why the owner's child gave no bytes.
enum ReadFail {
    /// The cold-launch semaphore is full: 503.
    Busy,
    /// Anything else: the uniform 404.
    Gone,
}

/// The broker's own request to the owner's child (§1.2): bearer, principal
/// header for its logs, the internal-call marker. Nothing of the outsider's.
async fn read_request(
    state: &BrokerState,
    owner: &Principal,
    endpoint: &ChildEndpoint,
    path: &str,
    max: u64,
) -> Result<reqwest::Response, reqwest::Error> {
    let url = format!(
        "http://{}{}?path={}&max={max}",
        endpoint.addr,
        crate::server::file_link_read::READ_PATH,
        percent_encoding::utf8_percent_encode(path, percent_encoding::NON_ALPHANUMERIC),
    );
    state
        .http
        .get(url)
        .bearer_auth(&*endpoint.token)
        .header(PRINCIPAL_HEADER, owner.id.to_string())
        .header(crate::access::INTERNAL_CALL_HEADER, "1")
        .send()
        .await
}

/// Reach the owner's child (running, or launched behind the cold-launch
/// semaphore, §1.3) and ask it for the bytes; retry once after
/// `invalidate` on a connect error, as `account_locked` does.
async fn reach_and_read(
    state: &BrokerState,
    gate: &LinkGate,
    owner: &Principal,
    path: &str,
    max: u64,
) -> Result<reqwest::Response, ReadFail> {
    let mut cold: Option<OwnedSemaphorePermit> = None;
    for attempt in 0..2 {
        let endpoint = match state.children.running_endpoint(owner.id).await {
            Some(e) => e,
            None => {
                if cold.is_none() {
                    cold = Some(
                        gate.cold
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| ReadFail::Busy)?,
                    );
                }
                match state.children.endpoint(owner).await {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!("file link: the owner's workspace did not start: {e:#}");
                        return Err(ReadFail::Gone);
                    }
                }
            }
        };
        match read_request(state, owner, &endpoint, path, max).await {
            Ok(res) => return Ok(res),
            Err(e) if e.is_connect() && attempt == 0 => {
                state.children.invalidate(owner.id, &endpoint).await;
            }
            Err(e) => {
                // Without the URL: it carries the owner's path.
                tracing::warn!(
                    "file link read on the owner's child failed: {}",
                    e.without_url()
                );
                return Err(ReadFail::Gone);
            }
        }
    }
    Err(ReadFail::Gone)
}

/// `GET|HEAD /s/:token/{view,download}`. `count`: whether this is an open
/// (false for HEAD, A-04).
#[allow(clippy::too_many_arguments)]
async fn bytes(
    state: &BrokerState,
    gate: &LinkGate,
    token: &str,
    headers: &HeaderMap,
    visitor: &Visitor,
    inline: bool,
    count: bool,
    now: i64,
) -> Outcome {
    let Some(live) = live_link(state, gate, token, now).await else {
        return Outcome::NotAvailable;
    };
    let row = &live.row;
    if row.passcode_phc.is_some() && !gate.unlocked(headers, &row.link_id, now) {
        return Outcome::NotAvailable;
    }
    let Some(kind) = PreviewKind::parse(&row.preview_kind) else {
        return Outcome::NotAvailable;
    };
    let (limit, content_type) = if inline {
        match (kind.preview_limit(), inline_type(&row.mime)) {
            (Some(limit), Some(ct)) => (limit, ct),
            _ => return Outcome::NotAvailable,
        }
    } else {
        (LINK_MAX_BYTES, stored_type(&row.mime))
    };
    let Some(link_permit) = gate.link_stream(&row.link_id) else {
        return Outcome::Done(slow_down());
    };
    let Ok(box_permit) = gate.streams.clone().try_acquire_owned() else {
        return Outcome::Done(busy());
    };
    let res = match reach_and_read(state, gate, &live.owner, &row.path, limit).await {
        Ok(res) => res,
        Err(ReadFail::Busy) => return Outcome::Done(busy()),
        Err(ReadFail::Gone) => return Outcome::NotAvailable,
    };
    if res.status() != reqwest::StatusCode::OK {
        return Outcome::NotAvailable;
    }
    // Only the numeric length is taken from the child, and only within the
    // cap: every header below is the broker's own (§1.5).
    let Some(len) = res.content_length().filter(|l| *l <= limit) else {
        return Outcome::NotAvailable;
    };
    // A byte request the child answered 200 is an open (§8.2): audit,
    // counters and the once-only first-open notification, off the request
    // path. A HEAD is not (A-04).
    if count {
        gate.spawn_open(
            &live,
            if inline {
                OpenMode::View
            } else {
                OpenMode::Download
            },
            visitor,
            now,
        );
    }
    let body = Body::from_stream(cap_stream(
        Box::pin(res.bytes_stream()),
        len,
        (link_permit, box_permit),
    ));
    let name = links::clean_name(&row.name);
    // Markdown is shown as a document, not as its source (06 §5.1 amended).
    if inline && md_preview::is_markdown(&name) {
        let mut raw: Vec<u8> = Vec::with_capacity(len as usize);
        let mut stream = std::pin::pin!(body.into_data_stream());
        while let Some(Ok(chunk)) = stream.next().await {
            raw.extend_from_slice(&chunk);
        }
        // A read that broke part-way is not shown as a short document.
        if raw.len() as u64 != len {
            return Outcome::NotAvailable;
        }
        let html = tokio::task::spawn_blocking(move || md_preview::render(&String::from_utf8_lossy(&raw)))
            .await
            .unwrap_or_default();
        return Outcome::Done(markdown_response(&name, html));
    }
    Outcome::Done(bytes_response(content_type, &name, inline, len, body))
}

/// The `Content-Type` for `/view`: a stored preview type on the passive
/// inline list, or nothing (HTML and SVG can't be stored, and couldn't pass
/// here if they were).
fn inline_type(mime: &str) -> Option<&'static str> {
    let stored = STORED_MIMES.iter().copied().find(|m| *m == mime)?;
    let essence = stored.split(';').next().unwrap_or("").trim();
    (stored != links::OCTET_STREAM && download::INLINE_MIMES.contains(&essence)).then_some(stored)
}

/// The `Content-Type` for `/download`: the stored type, or octet-stream.
fn stored_type(mime: &str) -> &'static str {
    STORED_MIMES
        .iter()
        .copied()
        .find(|m| *m == mime)
        .unwrap_or(links::OCTET_STREAM)
}

/// The child's body, ended at `limit` bytes whatever it sends (§1.5). `hold`
/// (the stream permits) lives as long as the stream.
pub(crate) fn cap_stream<S, E, H>(
    inner: S,
    limit: u64,
    hold: H,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
    H: Send + 'static,
{
    futures_util::stream::unfold((inner, 0u64, hold), move |(mut inner, sent, hold)| async move {
        if sent >= limit {
            return None;
        }
        match inner.next().await {
            None => None,
            Some(Ok(mut chunk)) => {
                let room = limit - sent;
                if chunk.len() as u64 > room {
                    chunk.truncate(room as usize);
                }
                let n = chunk.len() as u64;
                Some((Ok(chunk), (inner, sent + n, hold)))
            }
            Some(Err(e)) => Some((
                Err(std::io::Error::other(e.to_string())),
                (inner, limit, hold),
            )),
        }
    })
}

// ─── responses ──────────────────────────────────────────────────────────────

fn hv(s: &'static str) -> HeaderValue {
    HeaderValue::from_static(s)
}

/// The headers on every `/s/*` response (§5.2, §6.3).
fn common_headers(h: &mut HeaderMap) {
    h.insert(header::X_CONTENT_TYPE_OPTIONS, hv("nosniff"));
    h.insert(header::REFERRER_POLICY, hv("no-referrer"));
    h.insert("x-robots-tag", hv("noindex, nofollow, noarchive"));
}

/// `sha256` of [`STYLE`] as the page CSP names it.
fn style_hash() -> String {
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(STYLE.as_bytes()))
}

/// The page CSP (§5.2): no `script-src`, so no script runs.
fn page_csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        format!(
            "default-src 'none'; style-src 'sha256-{}'; img-src 'self'; object-src 'self'; \
             frame-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
            style_hash()
        )
    })
}

fn page_response(status: StatusCode, html: String) -> Response {
    let mut res = (status, html).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, hv("text/html; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(page_csp()) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::X_FRAME_OPTIONS, hv("DENY"));
    h.insert(header::CACHE_CONTROL, hv("no-store"));
    h.insert("cross-origin-opener-policy", hv("same-origin"));
    common_headers(h);
    res
}

/// The uniform "not available" answer (§6.1): the same status, bytes and
/// headers for every reason.
pub(crate) fn not_available() -> Response {
    static HTML: OnceLock<String> = OnceLock::new();
    let html = HTML.get_or_init(|| {
        render(
            "Link not available",
            "<h1>This link isn&#39;t available</h1><p>It may have expired, been turned off, \
             or the file may have moved. Ask the person who sent it for a new one.</p>",
        )
    });
    page_response(StatusCode::NOT_FOUND, html.clone())
}

fn slow_down() -> Response {
    page_response(
        StatusCode::TOO_MANY_REQUESTS,
        render(
            "Slow down",
            "<h1>Too many tries</h1><p>Please wait a few minutes, then try again.</p>",
        ),
    )
}

fn busy() -> Response {
    page_response(
        StatusCode::SERVICE_UNAVAILABLE,
        render(
            "Busy",
            "<h1>Busy right now</h1><p>Try again in a minute.</p>",
        ),
    )
}

fn locked() -> Response {
    page_response(
        StatusCode::FORBIDDEN,
        render(
            "Link locked",
            "<h1>This link is locked for a while</h1><p>Too many wrong passcodes were \
             tried. Try again later, or ask the person who sent it.</p>",
        ),
    )
}

/// `303` back to the page, with the unlock cookie when there is one.
fn see_other(token: &str, cookie: Option<String>) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&format!("/s/{token}")) {
        h.insert(header::LOCATION, v);
    }
    if let Some(c) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        h.insert(header::SET_COOKIE, c);
    }
    h.insert(header::CACHE_CONTROL, hv("no-store"));
    common_headers(h);
    res
}

fn passcode_form(
    gate: &LinkGate,
    token: &str,
    error: Option<&str>,
    status: StatusCode,
) -> Response {
    let error = error
        .map(|e| format!("<p class=\"error\" role=\"alert\">{}</p>", escape(e)))
        .unwrap_or_default();
    let inner = format!(
        "<h1>This file needs a passcode</h1>{from}{error}\
         <form method=\"post\" action=\"/s/{t}\">\
         <label for=\"passcode\">Passcode</label>\
         <input id=\"passcode\" name=\"passcode\" type=\"password\" autocomplete=\"off\" \
         required maxlength=\"{max}\" autofocus>\
         <button type=\"submit\">Open</button></form>",
        from = shared_from(gate),
        t = escape(token),
        max = links::PASSCODE_MAX_CHARS,
    );
    page_response(
        status,
        render_with_referrer("Passcode needed", &inner, "same-origin"),
    )
}

fn shared_from(gate: &LinkGate) -> String {
    gate.public_host
        .as_deref()
        .map(|h| format!("<p class=\"meta\">Shared from {}.</p>", escape(h)))
        .unwrap_or_default()
}

/// The file variant (§5.1): name, size, origin and expiry, the preview, the
/// Download link. Never the owner, the path or the link id.
fn file_page(gate: &LinkGate, token: &str, row: &LinkRow) -> Response {
    let clean = links::clean_name(&row.name);
    let name = escape(&clean);
    let t = escape(token);
    let expiry = match row.expires_at {
        Some(ms) => format!("Expires {} (UTC).", human_date(ms)),
        None => "Does not expire.".to_string(),
    };
    let from = gate
        .public_host
        .as_deref()
        .map(|h| format!(" Shared from {}.", escape(h)))
        .unwrap_or_default();
    let download = format!("<a class=\"button\" href=\"/s/{t}/download\" download>Download</a>");
    let preview = match PreviewKind::parse(&row.preview_kind) {
        Some(PreviewKind::Pdf) => format!(
            "<div class=\"preview\"><object data=\"/s/{t}/view\" type=\"application/pdf\">\
             <p>This browser can&#39;t show the PDF here. {download}</p></object></div>"
        ),
        Some(PreviewKind::Image) => {
            format!("<div class=\"preview\"><img src=\"/s/{t}/view\" alt=\"{name}\"></div>")
        }
        // Text types: shown as text in a sandboxed frame. Markdown is rendered
        // by [md_preview] on /view, never as the file's own HTML (§5.1).
        Some(PreviewKind::Text) => format!(
            "<div class=\"preview\"><iframe sandbox src=\"/s/{t}/view\" \
             title=\"{name}\"></iframe></div>"
        ),
        Some(PreviewKind::None) | None => String::new(),
    };
    let inner = format!(
        "<h1>{name}</h1><p class=\"meta\">{size}.{from} {expiry}</p>{preview}<p>{download}</p>",
        size = human_size(row.size_at_create),
    );
    page_response(StatusCode::OK, render(&clean, &inner))
}

fn bytes_response(
    content_type: &'static str,
    name: &str,
    inline: bool,
    len: u64,
    body: Body,
) -> Response {
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, hv(content_type));
    let disposition = HeaderValue::from_str(&download::content_disposition(name, inline))
        .unwrap_or_else(|_| hv("attachment; filename=\"download\""));
    h.insert(header::CONTENT_DISPOSITION, disposition);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    // The sandbox directive stays on every type, PDFs included, until 06
    // O-1 is settled by the live browser check (the conservative choice).
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        hv("default-src 'none'; frame-ancestors 'self'; sandbox"),
    );
    h.insert("cross-origin-resource-policy", hv("same-origin"));
    h.insert(header::CACHE_CONTROL, hv("private, no-store"));
    common_headers(h);
    res
}

/// The rendered markdown document for `/view`: HTML, but only the HTML
/// [md_preview] writes, under its own no-script CSP and the sandbox directive.
fn markdown_response(name: &str, html: String) -> Response {
    let len = html.len() as u64;
    let mut res = html.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, hv("text/html; charset=utf-8"));
    let disposition = HeaderValue::from_str(&download::content_disposition(name, true))
        .unwrap_or_else(|_| hv("attachment; filename=\"download\""));
    h.insert(header::CONTENT_DISPOSITION, disposition);
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    if let Ok(v) = HeaderValue::from_str(md_preview::csp()) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert("cross-origin-resource-policy", hv("same-origin"));
    h.insert(header::CACHE_CONTROL, hv("private, no-store"));
    common_headers(h);
    res
}

// ─── the template ───────────────────────────────────────────────────────────

/// The page's one inline style; the CSP allows exactly its hash.
const STYLE: &str = "body{margin:0;font:16px/1.5 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;\
background:#f6f4f0;color:#1d1b18}main{max-width:44rem;margin:0 auto;padding:2rem 1.25rem}\
h1{font-size:1.25rem;margin:0 0 .25rem;overflow-wrap:anywhere}p{margin:.5rem 0}\
.meta{color:#5d574e;font-size:.9rem}.error{color:#9b2c1f}\
.preview{margin:1.25rem 0;background:#fff;border:1px solid #ddd6cb;border-radius:8px;overflow:hidden}\
.preview img{display:block;max-width:100%;height:auto;margin:0 auto}\
.preview object,.preview iframe{display:block;width:100%;height:75vh;border:0}\
.button,button{display:inline-block;padding:.6rem 1rem;border-radius:6px;border:0;background:#3b2f22;\
color:#fff;text-decoration:none;font:inherit;cursor:pointer}\
label{display:block;margin-top:1rem}input{font:inherit;padding:.55rem .7rem;border:1px solid #bdb3a4;\
border-radius:6px;width:100%;box-sizing:border-box;margin:.5rem 0 1rem}\
@media (prefers-color-scheme:dark){body{background:#181613;color:#ece7df}.meta{color:#a89f92}\
.preview{background:#222;border-color:#3a352e}input{background:#222;color:#ece7df;border-color:#4a443b}}";

/// The whole document around `inner` (already escaped). No script, no app
/// asset, no manifest, no service worker (§5.3).
fn render(title: &str, inner: &str) -> String {
    render_with_referrer(title, inner, "no-referrer")
}

/// [`render`] with the page's own `<meta name="referrer">`. The passcode form
/// needs `same-origin`: under `no-referrer` a browser sends `Origin: null`
/// on the form's POST navigation (Fetch spec; Chromium and Firefox both do),
/// even to its own origin, and the `Origin` gate rightly refuses `null`. The
/// meta tag wins over the `Referrer-Policy` header for the document's own
/// requests, Caddy's deferred header included. A `Referer` then only ever
/// goes to the box itself, and the form posts to the URL it was served at, so
/// it carries nothing the request line does not.
fn render_with_referrer(title: &str, inner: &str, referrer: &'static str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <meta name=\"referrer\" content=\"{referrer}\">\
         <title>{}</title><style>{STYLE}</style></head><body><main>{inner}</main></body></html>",
        escape(title)
    )
}

/// HTML-escape text and attribute values.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn human_size(bytes: i64) -> String {
    let n = bytes.max(0) as f64;
    if n < 1024.0 {
        return format!("{} bytes", bytes.max(0));
    }
    let (v, unit) = if n < 1024.0 * 1024.0 {
        (n / 1024.0, "KB")
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        (n / (1024.0 * 1024.0), "MB")
    } else {
        (n / (1024.0 * 1024.0 * 1024.0), "GB")
    };
    format!("{v:.1} {unit}")
}

/// `ms` since the epoch → `(year, month, day)` in UTC (Hinnant's
/// civil-from-days).
fn ymd(ms: i64) -> (i64, u32, u32) {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn human_date(ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let (y, m, d) = ymd(ms);
    format!("{d} {} {y}", MONTHS[(m as usize).saturating_sub(1).min(11)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_csp_names_exactly_the_templates_style() {
        let html = render("t", "<p>x</p>");
        let start = html.find("<style>").unwrap() + "<style>".len();
        let end = html.find("</style>").unwrap();
        let style = &html[start..end];
        let hash =
            base64::engine::general_purpose::STANDARD.encode(Sha256::digest(style.as_bytes()));
        assert!(
            page_csp().contains(&format!("style-src 'sha256-{hash}'")),
            "{}",
            page_csp()
        );
        assert!(!page_csp().contains("script-src"));
        assert!(page_csp().contains("default-src 'none'"));
        assert!(page_csp().contains("frame-ancestors 'none'"));
        assert_eq!(html.matches("<style>").count(), 1);
        assert!(!html.contains("<script"));
        assert!(!html.contains("style=\""));
    }

    #[test]
    fn routes_parse_only_well_formed_tokens() {
        let t = "A".repeat(43);
        assert_eq!(parse_route(&format!("/s/{t}")), Some((t.as_str(), Action::Page)));
        assert_eq!(
            parse_route(&format!("/s/{t}/view")),
            Some((t.as_str(), Action::View))
        );
        assert_eq!(
            parse_route(&format!("/s/{t}/download")),
            Some((t.as_str(), Action::Download))
        );
        for bad in [
            "/s".to_string(),
            "/s/".to_string(),
            format!("/s/{t}/"),
            format!("/s/{t}/view/x"),
            format!("/s/{t}/other"),
            format!("/s/{}", "A".repeat(42)),
            format!("/s/{}", "A".repeat(44)),
            format!("/s/{}%41", "A".repeat(40)),
            "/s/x/y".to_string(),
        ] {
            assert_eq!(parse_route(&bad), None, "{bad}");
        }
    }

    #[test]
    fn escaping_covers_markup_and_quotes() {
        assert_eq!(
            escape("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn sizes_and_dates_read_plainly() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1023), "1023 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(25 * 1024 * 1024), "25.0 MB");
        assert_eq!(ymd(0), (1970, 1, 1));
        assert_eq!(ymd(951_782_400_000), (2000, 2, 29));
        assert_eq!(ymd(951_868_800_000), (2000, 3, 1));
        assert_eq!(human_date(951_782_400_000), "29 February 2000");
    }

    /// LS-12 at the unit level: a valid cookie passes; a tampered MAC,
    /// another link's cookie, an expired one, a spliced expiry and another
    /// boot's key do not.
    #[test]
    fn unlock_cookies_are_bound_to_link_expiry_and_boot() {
        let gate = LinkGate::disabled();
        let id = "01890a5d-ac96-774b-bcce-b302099a8057";
        let other = "01890a5d-ac96-774b-bcce-b302099a8058";
        let v = gate.mint_unlock(id, 2_000).unwrap();
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        assert!(gate.unlock_value_ok(&v, id, 1_000));
        assert!(!gate.unlock_value_ok(&v, id, 2_000), "expired");
        assert!(!gate.unlock_value_ok(&v, other, 1_000), "another link");
        let mut raw = b64.decode(&v).unwrap();
        *raw.last_mut().unwrap() ^= 1;
        assert!(!gate.unlock_value_ok(&b64.encode(&raw), id, 1_000), "tampered MAC");
        let mut raw = b64.decode(&v).unwrap();
        raw[36..44].copy_from_slice(&9_999i64.to_be_bytes());
        assert!(!gate.unlock_value_ok(&b64.encode(&raw), id, 1_000), "extended expiry");
        assert!(!LinkGate::disabled().unlock_value_ok(&v, id, 1_000), "another boot");
        assert!(!gate.unlock_value_ok("not base64!", id, 1_000));
        let c = gate.set_cookie("TOKEN", &v);
        assert!(
            c.starts_with(&format!(
                "{COOKIE_NAME}={v}; Path=/s/TOKEN; Max-Age=3600; HttpOnly; SameSite=Lax"
            )),
            "{c}"
        );
        assert!(c.ends_with("; Secure"));
    }

    #[test]
    fn cookie_values_are_read_from_every_cookie_header() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, hv("ikenga_session=s; ikenga_link=a"));
        h.append(header::COOKIE, hv("ikenga_link=b; other=1; ikenga_linkx=c"));
        let v: Vec<String> = cookie_values(&h).collect();
        assert_eq!(v, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn the_request_bucket_refills_over_a_minute() {
        let b = Buckets::new(3.0);
        assert!(b.take("k", 0) && b.take("k", 0) && b.take("k", 0));
        assert!(!b.take("k", 0));
        assert!(b.take("other", 0), "per key");
        assert!(
            b.take("k", MINUTE_MS / 3 + 1),
            "one token back after a third of a minute"
        );
        assert!(!b.take("k", MINUTE_MS / 3 + 1));
    }

    #[test]
    fn window_counters_forget_old_hits() {
        let w = WindowCounter::new(2, 100);
        assert!(!w.over("k", 0));
        w.hit("k", 0);
        w.hit("k", 10);
        assert!(w.over("k", 50));
        assert!(!w.over("k", 101), "the first hit aged out");
        assert!(!w.over("other", 50));
    }

    /// LS-10 at the unit level: whatever the child sends, the stream ends at
    /// the limit.
    #[tokio::test]
    async fn the_byte_stream_is_cut_at_the_limit() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from_static(b"0123456789")),
            Ok(Bytes::from_static(b"abcdefghij")),
            Ok(Bytes::from_static(b"never sent")),
        ];
        let out: Vec<_> = cap_stream(futures_util::stream::iter(chunks), 15, ())
            .collect()
            .await;
        let bytes: Vec<u8> = out.into_iter().flat_map(|c| c.unwrap().to_vec()).collect();
        assert_eq!(bytes, b"0123456789abcde");
    }

    #[test]
    fn view_types_are_passive_and_download_types_are_stored_ones() {
        assert_eq!(inline_type("application/pdf"), Some("application/pdf"));
        assert_eq!(
            inline_type("text/plain; charset=utf-8"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(inline_type(links::OCTET_STREAM), None);
        assert_eq!(inline_type("text/html"), None);
        assert_eq!(inline_type("image/svg+xml"), None);
        assert_eq!(stored_type("text/html"), links::OCTET_STREAM);
        assert_eq!(stored_type("image/png"), "image/png");
    }

    /// LS-04 (static half): this module never opens, stats or canonicalizes
    /// a path; the bytes come only from the owner's child.
    #[test]
    fn the_broker_side_has_no_filesystem_access() {
        let src = include_str!("file_links.rs");
        let head = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for needle in [
            "std::fs",
            "tokio::fs",
            "File::open",
            "OpenOptions",
            "canonicalize",
            "read_link",
            "symlink_metadata",
        ] {
            assert!(!head.contains(needle), "{needle}");
        }
    }

    /// LS-15 (provision half): the generated Caddyfile keeps no access log,
    /// so a `/s/<token>` URI is never written down by the proxy.
    #[test]
    fn the_caddyfile_has_no_log_directive() {
        let sh = include_str!("../../../../scripts/server/provision.sh");
        let start = sh
            .find("conf+=\"$PUBLIC_HOST {")
            .expect("the Caddyfile site block");
        let block = &sh[start..];
        let end = block.find("\n\"\n").expect("its closing quote");
        let block = &block[..end];
        assert!(block.contains("reverse_proxy"), "{block}");
        for line in block.lines() {
            let l = line.trim();
            assert!(
                !(l == "log" || l.starts_with("log ") || l.starts_with("log{")),
                "Caddy access log would record /s/ tokens: {line}"
            );
        }
        assert!(block.contains("Referrer-Policy no-referrer"));
    }
}
