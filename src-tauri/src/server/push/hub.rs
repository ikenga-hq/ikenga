//! [`PushHub`]: one per access-store owner (the T0 daemon, the T1 broker).
//!
//! [`PushHub::emit`] never blocks: it queues the event (bounded) and a
//! worker delivers it — at most [`IN_FLIGHT`] sends at once — to every
//! subscription of the event's principal that is **entitled right now**
//! ([`super::store::entitled`]: device unrevoked at its current tier,
//! routing preference, account enabled and at its session epoch, admin for
//! updates), re-checking each endpoint against the allowlist before it goes.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tokio::sync::{mpsc, Semaphore};

use super::sender::{self, Backoff, EndpointPolicy, Outcome, PushRequest, PushTransport};
use super::store::{self, Candidate};
use super::vapid::Vapid;
use super::{PushEvent, PushKind, PAYLOAD_LEN};
use crate::access::store::{AccessStore, StoreTier};
use crate::executor::PrincipalId;

/// Concurrent sends.
pub const IN_FLIGHT: usize = 4;
/// Queued events beyond this are dropped (the notification centre stays the
/// source of truth).
pub const QUEUE: usize = 256;
/// Per-principal budget.
pub const PER_MINUTE: usize = 30;
pub const PER_HOUR: usize = 300;
/// `access_push_test`, per subscription.
pub const TEST_EVERY: Duration = Duration::from_secs(10);
/// How often a registered [`UpdateSource`] is asked.
pub const UPDATE_POLL: Duration = Duration::from_secs(15 * 60);
const LAST_UPDATE_KEY: &str = "last_update_version";

/// Something that knows whether a newer server release is available. The
/// in-app server update feature (`feat/remote-update`) implements it and
/// registers with [`PushHub::set_update_source`]; nothing on `main` does
/// yet. The version never leaves the server (W4).
pub trait UpdateSource: Send + Sync {
    fn available_version(&self) -> Option<String>;
}

/// What a hub is built from.
pub struct HubConfig {
    pub store: AccessStore,
    /// `None` = push is off (`--no-push`, or the key file was refused).
    pub vapid: Option<Arc<Vapid>>,
    /// Why push is off, for `access_push_config`.
    pub disabled_reason: Option<String>,
    pub policy: EndpointPolicy,
    pub transport: Arc<dyn PushTransport>,
    pub backoff: Backoff,
}

pub struct PushHub {
    pool: SqlitePool,
    tier: StoreTier,
    owner: Option<PrincipalId>,
    vapid: Option<Arc<Vapid>>,
    disabled_reason: Option<String>,
    policy: EndpointPolicy,
    transport: Arc<dyn PushTransport>,
    backoff: Backoff,
    tx: mpsc::Sender<(PushEvent, Option<String>)>,
    sends: Arc<Semaphore>,
    budget: Mutex<HashMap<PrincipalId, VecDeque<Instant>>>,
    tests: Mutex<HashMap<String, Instant>>,
}

impl std::fmt::Debug for PushHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushHub")
            .field("tier", &self.tier)
            .field("enabled", &self.enabled())
            .finish()
    }
}

impl PushHub {
    /// Build the hub and start its delivery worker (needs a Tokio runtime).
    pub fn new(cfg: HubConfig) -> Arc<Self> {
        let (tx, mut rx) = mpsc::channel::<(PushEvent, Option<String>)>(QUEUE);
        let hub = Arc::new(PushHub {
            pool: cfg.store.pool().clone(),
            tier: cfg.store.meta().tier,
            owner: cfg.store.meta().owner_principal_id,
            vapid: cfg.vapid,
            disabled_reason: cfg.disabled_reason,
            policy: cfg.policy,
            transport: cfg.transport,
            backoff: cfg.backoff,
            tx,
            sends: Arc::new(Semaphore::new(IN_FLIGHT)),
            budget: Mutex::new(HashMap::new()),
            tests: Mutex::new(HashMap::new()),
        });
        let weak = Arc::downgrade(&hub);
        tokio::spawn(async move {
            while let Some((event, only)) = rx.recv().await {
                let Some(hub) = weak.upgrade() else { break };
                tokio::spawn(async move {
                    hub.deliver(&event, only.as_deref()).await;
                });
            }
        });
        hub
    }

    pub fn enabled(&self) -> bool {
        self.vapid.is_some()
    }

    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }

    pub fn vapid(&self) -> Option<&Arc<Vapid>> {
        self.vapid.as_ref()
    }

    pub fn policy(&self) -> &EndpointPolicy {
        &self.policy
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn tier(&self) -> StoreTier {
        self.tier
    }

    /// Queue one event (never blocks; dropped when push is off or the queue
    /// is full).
    pub fn emit(&self, event: PushEvent) {
        if !self.enabled() {
            return;
        }
        if self.tx.try_send((event, None)).is_err() {
            tracing::warn!("push: queue full; dropped an event");
        }
    }

    /// `access_push_test`: one `test` push to one of the caller's own
    /// subscriptions, at most every [`TEST_EVERY`]. `false` = throttled.
    pub fn send_test(&self, principal: PrincipalId, sub_id: &str) -> bool {
        {
            let mut tests = self.tests.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            tests.retain(|_, t| now.duration_since(*t) < TEST_EVERY);
            if tests.contains_key(sub_id) {
                return false;
            }
            tests.insert(sub_id.to_string(), now);
        }
        let ev = PushEvent::new(Some(principal), PushKind::Test, "test");
        let _ = self.tx.try_send((ev, Some(sub_id.to_string())));
        true
    }

    /// Spend one unit of the principal's send budget.
    fn spend(&self, principal: PrincipalId) -> bool {
        let now = Instant::now();
        let mut budget = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        let q = budget.entry(principal).or_default();
        while q
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(3600))
        {
            q.pop_front();
        }
        let last_minute = q
            .iter()
            .filter(|t| now.duration_since(**t) <= Duration::from_secs(60))
            .count();
        if q.len() >= PER_HOUR || last_minute >= PER_MINUTE {
            return false;
        }
        q.push_back(now);
        true
    }

    /// The subscriptions `event` goes to right now.
    pub async fn targets(
        &self,
        principal: PrincipalId,
        kind: PushKind,
    ) -> Result<Vec<Candidate>, sqlx::Error> {
        let Some(vapid) = &self.vapid else {
            return Ok(Vec::new());
        };
        let pid = principal.to_string();
        let candidates = store::candidates(&self.pool, &pid).await?;
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let account = store::account_state(&self.pool, self.tier, &pid).await?;
        let pref = {
            let mut conn = self.pool.acquire().await?;
            crate::access::routing::read_pref(&mut conn, &pid).await?
        };
        let key_id = vapid.key_id();
        Ok(candidates
            .into_iter()
            .filter(|c| store::entitled(c, kind, self.tier, account.as_ref(), &pref, &key_id))
            .collect())
    }

    /// Deliver one event now (the worker's body; tests call it directly).
    /// Returns each target's outcome.
    pub async fn deliver(&self, event: &PushEvent, only: Option<&str>) -> Vec<(String, Outcome)> {
        let Some(vapid) = self.vapid.clone() else {
            return Vec::new();
        };
        let Some(principal) = event.principal.or(self.owner) else {
            return Vec::new();
        };
        let Some(payload) = super::payload(event.kind, &event.r) else {
            return Vec::new();
        };
        if !self.spend(principal) {
            tracing::warn!(
                "push: send budget spent; dropped a {} push",
                event.kind.as_str()
            );
            return Vec::new();
        }
        let targets = match self.targets(principal, event.kind).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("push: reading subscriptions failed: {e}");
                return Vec::new();
            }
        };
        let now_secs = store::now_ms() / 1000;
        let mut jobs = Vec::new();
        for c in targets {
            let sub = c.sub;
            if only.is_some_and(|id| id != sub.sub_id) {
                continue;
            }
            let host = sub.endpoint_host();
            // Re-checked at send: the allowlist may have narrowed since.
            let origin = match self.policy.check(&sub.endpoint) {
                Ok((_, origin)) => origin,
                Err(why) => {
                    tracing::warn!("push: skipped {} ({host}): {why}", sub.short_id());
                    continue;
                }
            };
            let body = match super::crypto::encrypt(&sub.p256dh, &sub.auth, &payload, PAYLOAD_LEN) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("push: {} ({host}): {e}", sub.short_id());
                    let _ = store::mark_failure(&self.pool, &sub.sub_id, 0).await;
                    continue;
                }
            };
            let authorization = match vapid.authorization(&origin, now_secs) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("push: {e}");
                    return Vec::new();
                }
            };
            let req = PushRequest {
                endpoint: sub.endpoint.clone(),
                authorization,
                ttl: event.ttl(),
                urgency: event.kind.urgency(),
                topic: event.kind.topic(),
                body,
            };
            jobs.push(async move {
                let _permit = self.sends.clone().acquire_owned().await;
                let outcome =
                    sender::send_with_retries(self.transport.as_ref(), &req, &self.backoff).await;
                let id = sub.sub_id.clone();
                let r = match outcome {
                    Outcome::Delivered(s) => store::mark_success(&self.pool, &id, s).await,
                    Outcome::Gone(s) => {
                        tracing::info!("push: {} ({host}) is gone ({s}); removed", sub.short_id());
                        store::delete_by_id(&self.pool, &id).await.map(|_| ())
                    }
                    Outcome::Failed(s) => {
                        tracing::warn!("push: {} ({host}) failed: {s}", sub.short_id());
                        store::mark_failure(&self.pool, &id, s).await
                    }
                };
                if let Err(e) = r {
                    tracing::warn!("push: recording an outcome failed: {e}");
                }
                (id, outcome)
            });
        }
        let out = futures_util::future::join_all(jobs).await;
        if let Err(e) = store::prune(&self.pool, store::now_ms()).await {
            tracing::warn!("push: prune failed: {e}");
        }
        out
    }

    /// The admins an `update` push goes to: T0's owner; T1's enabled
    /// `is_admin` accounts.
    pub async fn admins(&self) -> Result<Vec<PrincipalId>, sqlx::Error> {
        match self.tier {
            StoreTier::T0 => Ok(self.owner.into_iter().collect()),
            StoreTier::T1 => {
                let ids: Vec<String> = sqlx::query_scalar(
                    "SELECT principal_id FROM accounts WHERE is_admin = 1 AND disabled_at IS NULL",
                )
                .fetch_all(&self.pool)
                .await?;
                Ok(ids.iter().filter_map(|s| s.parse().ok()).collect())
            }
        }
    }

    /// A server update is available: push `update` to every admin, once per
    /// version (remembered in `push_meta`, so a restart doesn't repeat it).
    /// Returns the principals it was queued for (empty = already announced).
    pub async fn announce_update(&self, version: &str) -> Vec<PrincipalId> {
        if !self.enabled() {
            return Vec::new();
        }
        match store::meta_get(&self.pool, LAST_UPDATE_KEY).await {
            Ok(Some(v)) if v == version => return Vec::new(),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("push: reading the announced update failed: {e}");
                return Vec::new();
            }
        }
        if let Err(e) = store::meta_set(&self.pool, LAST_UPDATE_KEY, version).await {
            tracing::warn!("push: recording the announced update failed: {e}");
            return Vec::new();
        }
        let admins = self.admins().await.unwrap_or_default();
        for id in &admins {
            // The payload never carries the version (W4).
            self.emit(PushEvent::new(Some(*id), PushKind::Update, "update"));
        }
        admins
    }

    /// Register the update signal and poll it every [`UPDATE_POLL`] (the
    /// first check is immediate). One line from the update feature:
    /// `push::hub().map(|h| h.set_update_source(Arc::new(ctl)))`.
    pub fn set_update_source(self: &Arc<Self>, source: Arc<dyn UpdateSource>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(UPDATE_POLL);
            loop {
                tick.tick().await;
                let Some(hub) = weak.upgrade() else { break };
                if let Some(v) = source.available_version() {
                    hub.announce_update(&v).await;
                }
            }
        });
    }
}

/// Build the store owner's hub from the daemon flags: the VAPID key at
/// `key_path` (created on first use), the allowlist, the real transport.
/// `None` only with `--no-push`; a refused key still yields a (disabled)
/// hub so `access_push_config` can say why.
pub fn boot(
    store: AccessStore,
    key_path: &std::path::Path,
    options: &super::PushOptions,
    public_url: Option<&str>,
) -> Option<Arc<PushHub>> {
    if options.disabled {
        tracing::info!("push: off (--no-push)");
        return None;
    }
    let policy = EndpointPolicy::new(&options.endpoint_hosts, &options.allow_endpoints);
    for o in policy.exact_origins() {
        tracing::warn!("push: --push-allow-endpoint {o}: pushes may go to this origin (test only)");
    }
    let contact = super::vapid::contact_for(options.contact.as_deref(), public_url);
    let (vapid, reason) = match super::vapid::load_or_create(key_path, contact) {
        Ok(v) => (Some(Arc::new(v)), None),
        Err(e) => {
            tracing::warn!("push: disabled — {e}");
            (
                None,
                Some("the server's push key is unusable; see the server log".to_string()),
            )
        }
    };
    let allow_http = policy
        .exact_origins()
        .iter()
        .any(|o| o.starts_with("http://"));
    let transport: Arc<dyn PushTransport> = match sender::HttpTransport::new(allow_http) {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::warn!("push: disabled — no HTTP client: {e}");
            return None;
        }
    };
    if let Some(v) = &vapid {
        tracing::info!("push: on (VAPID key {})", v.key_id());
    }
    Some(PushHub::new(HubConfig {
        store,
        vapid,
        disabled_reason: reason,
        policy,
        transport,
        backoff: Backoff::default(),
    }))
}
