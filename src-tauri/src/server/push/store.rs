//! `push_subscriptions` / `push_meta` (access migration `0003_push`).
//!
//! Every query is scoped to one principal except the sender's bookkeeping
//! (by `sub_id`) and the lifecycle deletes (by device, session or
//! principal). Rows are never returned to a client with their keys or full
//! endpoint ([`SubRow::view`]).

use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, SqliteConnection, SqlitePool};

use super::PushKind;
use crate::access::caps::{Cap, Tier};
use crate::access::routing::Pref;
use crate::access::store::StoreTier;

/// Per-principal cap; a new subscription past it evicts the least recently
/// updated one.
pub const MAX_PER_PRINCIPAL: i64 = 20;
/// A subscription failing this many times with no success in
/// [`PRUNE_AFTER_MS`] is pruned.
pub const PRUNE_FAILURES: i64 = 10;
pub const PRUNE_AFTER_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub fn now_ms() -> i64 {
    crate::access::devices::now_ms()
}

/// A fresh client handle (128-bit random hex).
pub fn new_sub_id() -> String {
    let mut b = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

/// `sha256(session id)` hex: what a session subscription keeps, so logout
/// can find it without the store ever holding a session id.
pub fn session_ref(session_id: &str) -> String {
    hex::encode(Sha256::digest(session_id.as_bytes()))
}

/// How the subscribing credential authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubVia {
    Device,
    Session,
    Operator,
}

impl SubVia {
    pub const fn as_str(self) -> &'static str {
        match self {
            SubVia::Device => "device",
            SubVia::Session => "session",
            SubVia::Operator => "operator",
        }
    }

    pub fn parse(s: &str) -> Option<SubVia> {
        match s {
            "device" => Some(SubVia::Device),
            "session" => Some(SubVia::Session),
            "operator" => Some(SubVia::Operator),
            _ => None,
        }
    }
}

/// What `access_push_subscribe` stores.
#[derive(Debug, Clone)]
pub struct NewSub {
    pub principal_id: String,
    pub device_id: Option<String>,
    pub via: SubVia,
    pub session_epoch: Option<i64>,
    pub session_ref: Option<String>,
    pub endpoint: String,
    pub endpoint_origin: String,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
    pub vapid_key_id: String,
    pub kinds: Vec<PushKind>,
    pub label: Option<String>,
    pub user_agent: Option<String>,
}

/// One stored subscription. `Debug` never prints the endpoint or keys.
#[derive(Clone)]
pub struct SubRow {
    pub sub_id: String,
    pub principal_id: String,
    pub device_id: Option<String>,
    pub via: SubVia,
    pub session_epoch: Option<i64>,
    pub session_ref: Option<String>,
    pub endpoint: String,
    pub endpoint_origin: String,
    pub p256dh: Vec<u8>,
    pub auth: Vec<u8>,
    pub vapid_key_id: String,
    pub kinds: Vec<PushKind>,
    pub label: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_success_at: Option<i64>,
    pub failure_count: i64,
}

impl std::fmt::Debug for SubRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubRow")
            .field("sub_id", &self.short_id())
            .field("via", &self.via)
            .field("host", &self.endpoint_host())
            .finish()
    }
}

impl SubRow {
    /// The first 8 hex chars — what logs carry.
    pub fn short_id(&self) -> &str {
        &self.sub_id[..self.sub_id.len().min(8)]
    }

    /// The push service host, never the path (the path is a capability).
    pub fn endpoint_host(&self) -> String {
        url::Url::parse(&self.endpoint)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// The `access_push_list` row (§7): no keys, no full endpoint.
    pub fn view(&self, this_device: bool) -> Value {
        json!({
            "subId": self.sub_id,
            "label": self.label,
            "via": self.via.as_str(),
            "deviceId": self.device_id,
            "endpointHost": self.endpoint_host(),
            "kinds": self.kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "createdAt": self.created_at,
            "lastSuccessAt": self.last_success_at,
            "thisDevice": this_device,
        })
    }
}

pub fn kinds_json(kinds: &[PushKind]) -> String {
    serde_json::to_string(&kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".into())
}

fn parse_kinds(raw: &str) -> Vec<PushKind> {
    serde_json::from_str::<Vec<String>>(raw)
        .unwrap_or_default()
        .iter()
        .filter_map(|s| PushKind::parse(s))
        .collect()
}

const COLUMNS: &str =
    "sub_id, principal_id, device_id, via, session_epoch, session_ref, endpoint, \
     endpoint_origin, p256dh, auth, vapid_key_id, kinds, label, created_at, updated_at, \
     last_success_at, failure_count";

fn row(r: &sqlx::sqlite::SqliteRow) -> Result<SubRow, sqlx::Error> {
    let via: String = r.try_get("via")?;
    let kinds: String = r.try_get("kinds")?;
    Ok(SubRow {
        sub_id: r.try_get("sub_id")?,
        principal_id: r.try_get("principal_id")?,
        device_id: r.try_get("device_id")?,
        via: SubVia::parse(&via).unwrap_or(SubVia::Device),
        session_epoch: r.try_get("session_epoch")?,
        session_ref: r.try_get("session_ref")?,
        endpoint: r.try_get("endpoint")?,
        endpoint_origin: r.try_get("endpoint_origin")?,
        p256dh: r.try_get("p256dh")?,
        auth: r.try_get("auth")?,
        vapid_key_id: r.try_get("vapid_key_id")?,
        kinds: parse_kinds(&kinds),
        label: r.try_get("label")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        last_success_at: r.try_get("last_success_at")?,
        failure_count: r.try_get("failure_count")?,
    })
}

/// Insert, re-subscribe, or move an endpoint to this principal (upsert on
/// `endpoint`), then hold the per-principal cap. Returns the `sub_id`: the
/// same one on a re-subscribe by the same principal, a fresh one when the
/// endpoint changes hands (the previous owner's handle stops working).
pub async fn upsert(pool: &SqlitePool, new: &NewSub) -> Result<String, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let now = now_ms();
    let existing: Option<(String, String, i64)> = sqlx::query_as(
        "SELECT sub_id, principal_id, created_at FROM push_subscriptions WHERE endpoint = ?",
    )
    .bind(&new.endpoint)
    .fetch_optional(&mut *tx)
    .await?;
    let (sub_id, created_at) = match existing {
        Some((id, owner, created)) if owner == new.principal_id => (id, created),
        Some((id, _, _)) => {
            sqlx::query("DELETE FROM push_subscriptions WHERE sub_id = ?")
                .bind(&id)
                .execute(&mut *tx)
                .await?;
            (new_sub_id(), now)
        }
        None => (new_sub_id(), now),
    };
    sqlx::query(
        "INSERT INTO push_subscriptions (sub_id, principal_id, device_id, via, session_epoch, \
         session_ref, endpoint, endpoint_origin, p256dh, auth, vapid_key_id, kinds, label, \
         user_agent, created_at, updated_at, failure_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0) \
         ON CONFLICT(sub_id) DO UPDATE SET device_id = excluded.device_id, via = excluded.via, \
         session_epoch = excluded.session_epoch, session_ref = excluded.session_ref, \
         endpoint_origin = excluded.endpoint_origin, p256dh = excluded.p256dh, \
         auth = excluded.auth, vapid_key_id = excluded.vapid_key_id, kinds = excluded.kinds, \
         label = excluded.label, user_agent = excluded.user_agent, \
         updated_at = excluded.updated_at, failure_count = 0, last_status = NULL",
    )
    .bind(&sub_id)
    .bind(&new.principal_id)
    .bind(&new.device_id)
    .bind(new.via.as_str())
    .bind(new.session_epoch)
    .bind(&new.session_ref)
    .bind(&new.endpoint)
    .bind(&new.endpoint_origin)
    .bind(&new.p256dh)
    .bind(&new.auth)
    .bind(&new.vapid_key_id)
    .bind(kinds_json(&new.kinds))
    .bind(&new.label)
    .bind(&new.user_agent)
    .bind(created_at)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // The cap: drop the least recently updated beyond it.
    sqlx::query(
        "DELETE FROM push_subscriptions WHERE principal_id = ? AND sub_id IN ( \
           SELECT sub_id FROM push_subscriptions WHERE principal_id = ? \
           ORDER BY updated_at DESC, created_at DESC LIMIT -1 OFFSET ?)",
    )
    .bind(&new.principal_id)
    .bind(&new.principal_id)
    .bind(MAX_PER_PRINCIPAL)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(sub_id)
}

/// The principal's subscriptions, newest first.
pub async fn list(pool: &SqlitePool, principal_id: &str) -> Result<Vec<SubRow>, sqlx::Error> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM push_subscriptions WHERE principal_id = ? \
         ORDER BY created_at DESC"
    ))
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row).collect()
}

/// One subscription by id (any principal — the sender's bookkeeping).
pub async fn get(pool: &SqlitePool, sub_id: &str) -> Result<Option<SubRow>, sqlx::Error> {
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM push_subscriptions WHERE sub_id = ?"
    ))
    .bind(sub_id)
    .fetch_optional(pool)
    .await?;
    r.as_ref().map(row).transpose()
}

/// Set a subscription's kinds; `false` when it isn't the principal's.
pub async fn set_kinds(
    pool: &SqlitePool,
    principal_id: &str,
    sub_id: &str,
    kinds: &[PushKind],
) -> Result<bool, sqlx::Error> {
    let n = sqlx::query(
        "UPDATE push_subscriptions SET kinds = ?, updated_at = ? \
         WHERE sub_id = ? AND principal_id = ?",
    )
    .bind(kinds_json(kinds))
    .bind(now_ms())
    .bind(sub_id)
    .bind(principal_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(n == 1)
}

/// Remove the principal's subscription by id or endpoint.
pub async fn delete_own(
    pool: &SqlitePool,
    principal_id: &str,
    sub_id: Option<&str>,
    endpoint: Option<&str>,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM push_subscriptions WHERE principal_id = ? \
         AND (sub_id = ? OR endpoint = ?)",
    )
    .bind(principal_id)
    .bind(sub_id)
    .bind(endpoint)
    .execute(pool)
    .await?
    .rows_affected())
}

/// The push service said the endpoint is gone (404 / 410).
pub async fn delete_by_id(pool: &SqlitePool, sub_id: &str) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM push_subscriptions WHERE sub_id = ?")
            .bind(sub_id)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

pub async fn mark_success(pool: &SqlitePool, sub_id: &str, status: u16) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE push_subscriptions SET last_success_at = ?, failure_count = 0, last_status = ? \
         WHERE sub_id = ?",
    )
    .bind(now_ms())
    .bind(status as i64)
    .bind(sub_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_failure(pool: &SqlitePool, sub_id: &str, status: u16) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE push_subscriptions SET last_failure_at = ?, failure_count = failure_count + 1, \
         last_status = ? WHERE sub_id = ?",
    )
    .bind(now_ms())
    .bind(status as i64)
    .bind(sub_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Drop subscriptions that keep failing and haven't worked in 30 days.
pub async fn prune(pool: &SqlitePool, now: i64) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "DELETE FROM push_subscriptions WHERE failure_count >= ? \
         AND coalesce(last_success_at, created_at) < ?",
    )
    .bind(PRUNE_FAILURES)
    .bind(now - PRUNE_AFTER_MS)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Device grant revoked (`devices::mark_revoked`, same transaction).
pub async fn delete_for_device(
    conn: &mut SqliteConnection,
    device_id: &str,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM push_subscriptions WHERE device_id = ?")
            .bind(device_id)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
    )
}

/// Whether this connection's store has the push tables (a T1 `accounts.db`
/// whose access set was never migrated has none).
pub async fn has_tables(conn: &mut SqliteConnection) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'push_subscriptions'",
    )
    .fetch_one(&mut *conn)
    .await?;
    Ok(n == 1)
}

/// Logout: the session's own subscriptions.
pub async fn delete_for_session(
    conn: &mut SqliteConnection,
    session_id: &str,
) -> Result<u64, sqlx::Error> {
    if !has_tables(conn).await? {
        return Ok(0);
    }
    Ok(
        sqlx::query("DELETE FROM push_subscriptions WHERE session_ref = ?")
            .bind(session_ref(session_id))
            .execute(&mut *conn)
            .await?
            .rows_affected(),
    )
}

/// Account disabled: every subscription of the principal.
pub async fn delete_for_principal(
    conn: &mut SqliteConnection,
    principal_id: &str,
) -> Result<u64, sqlx::Error> {
    if !has_tables(conn).await? {
        return Ok(0);
    }
    Ok(
        sqlx::query("DELETE FROM push_subscriptions WHERE principal_id = ?")
            .bind(principal_id)
            .execute(&mut *conn)
            .await?
            .rows_affected(),
    )
}

pub async fn meta_get(pool: &SqlitePool, k: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT v FROM push_meta WHERE k = ?")
        .bind(k)
        .fetch_optional(pool)
        .await
}

pub async fn meta_set(pool: &SqlitePool, k: &str, v: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO push_meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
    )
    .bind(k)
    .bind(v)
    .execute(pool)
    .await?;
    Ok(())
}

// ─── Entitlement at send time ──────────────────────────────────────────────

/// The live state of the credential behind a subscription, read at send
/// time (never trusted from subscribe time).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub sub: SubRow,
    /// `devices` row for `device_id`: `(tier, revoked)`; `None` = no row.
    pub device: Option<(Tier, bool)>,
}

/// T1 only: the account behind every subscription of the principal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    pub disabled: bool,
    pub session_epoch: i64,
    pub is_admin: bool,
}

/// The principal's subscriptions with their device rows.
pub async fn candidates(
    pool: &SqlitePool,
    principal_id: &str,
) -> Result<Vec<Candidate>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT s.sub_id, s.principal_id, s.device_id, s.via, s.session_epoch, s.session_ref, \
         s.endpoint, s.endpoint_origin, s.p256dh, s.auth, s.vapid_key_id, s.kinds, s.label, \
         s.created_at, s.updated_at, s.last_success_at, s.failure_count, \
         d.tier AS d_tier, d.revoked_at AS d_revoked \
         FROM push_subscriptions s LEFT JOIN devices d ON d.device_id = s.device_id \
         WHERE s.principal_id = ?",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let tier: Option<String> = r.try_get("d_tier")?;
        let revoked: Option<i64> = r.try_get("d_revoked")?;
        out.push(Candidate {
            sub: row(r)?,
            device: tier
                .and_then(|t| Tier::parse(&t))
                .map(|t| (t, revoked.is_some())),
        });
    }
    Ok(out)
}

/// T1: the account row (`None` on T0, where there is no `accounts` table).
pub async fn account_state(
    pool: &SqlitePool,
    tier: StoreTier,
    principal_id: &str,
) -> Result<Option<AccountState>, sqlx::Error> {
    if tier != StoreTier::T1 {
        return Ok(None);
    }
    let r: Option<(Option<i64>, i64, i64)> = sqlx::query_as(
        "SELECT disabled_at, session_epoch, is_admin FROM accounts WHERE principal_id = ?",
    )
    .bind(principal_id)
    .fetch_optional(pool)
    .await?;
    Ok(r.map(|(disabled, epoch, admin)| AccountState {
        disabled: disabled.is_some(),
        session_epoch: epoch,
        is_admin: admin != 0,
    }))
}

/// Whether `c` may receive `kind` right now (plans/pwa S2 §9). `pref` is
/// the principal's routing preference; `key_id` the current VAPID key.
pub fn entitled(
    c: &Candidate,
    kind: PushKind,
    store_tier: StoreTier,
    account: Option<&AccountState>,
    pref: &Pref,
    key_id: &str,
) -> bool {
    let s = &c.sub;
    // A subscription made under another VAPID key is dead.
    if s.vapid_key_id != key_id {
        return false;
    }
    if kind != PushKind::Test && !s.kinds.contains(&kind) {
        return false;
    }
    // T1: the account must exist and be enabled, whatever the credential —
    // honoured even when a CLI write skipped the broker's deletes.
    if store_tier == StoreTier::T1 && !account.is_some_and(|a| !a.disabled) {
        return false;
    }
    let tier = match s.via {
        SubVia::Device => match c.device {
            Some((t, false)) if s.device_id.is_some() => t,
            _ => return false,
        },
        SubVia::Session => {
            let Some(a) = account.filter(|_| store_tier == StoreTier::T1) else {
                return false;
            };
            if s.session_epoch != Some(a.session_epoch) {
                return false;
            }
            Tier::Full
        }
        SubVia::Operator => {
            if store_tier != StoreTier::T0 {
                return false;
            }
            Tier::Full
        }
    };
    let admin_strength = matches!(s.via, SubVia::Session | SubVia::Operator) || tier == Tier::Full;
    match kind {
        PushKind::Permission => {
            let device = match s.via {
                SubVia::Session => None,
                _ => s.device_id.as_deref(),
            };
            tier.caps().contains(Cap::Approve) && pref.admits(device)
        }
        PushKind::Pairing => admin_strength,
        PushKind::Update => {
            admin_strength
                && match store_tier {
                    StoreTier::T0 => true,
                    StoreTier::T1 => account.is_some_and(|a| a.is_admin),
                }
        }
        PushKind::RunFinished
        | PushKind::RunFailed
        | PushKind::RunCancelled
        | PushKind::Invite
        | PushKind::Test => true,
    }
}

/// The kinds a credential may subscribe to (for `access_push_config`): the
/// same rules as [`entitled`], minus routing (a preference can change).
pub fn kinds_for(tier: Tier, admin_strength: bool, is_admin_or_t0: bool) -> Vec<PushKind> {
    PushKind::SELECTABLE
        .into_iter()
        .filter(|k| match k {
            PushKind::Permission => tier.caps().contains(Cap::Approve),
            PushKind::Pairing => admin_strength,
            PushKind::Update => admin_strength && is_admin_or_t0,
            _ => true,
        })
        .collect()
}
