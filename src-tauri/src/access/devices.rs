//! Device records, the device credential, and revoke / tier-change
//! plumbing (G-ACCESS §3.8–§3.11). Pairing itself — the code, SPAKE2, the
//! host confirm — is WP-74b (`access::pairing`); it mints rows through
//! [`insert_paired`].
//!
//! * **Credential** (P-4): `ikd1.<device_id>.<secret>`, `secret` = 32 OS-RNG
//!   bytes as base64url (no padding). Only `SHA-256(secret)` is stored;
//!   comparison is constant-time. The token is never logged or audited
//!   (A-12).
//! * **Rotation** (§3.9, P-32): only for a credential presented in the
//!   `ikenga_device` cookie, once it is older than 30 days; the previous hash
//!   stays valid for a 5-minute grace. Bearer tokens never rotate.
//! * **Idle expiry**: unseen for 90 days → refused, row revoked
//!   (`revoked_reason = 'idle'`), audited `device.expired`.
//! * **Revoke / tier change** (§3.10): one transaction with its audit row
//!   (A-18), `grant_epoch` bumped; the caller then closes the device's
//!   sockets (4401 / 4403) through its registry.
//!
//! All times are unix **milliseconds**.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Connection, Row, SqliteConnection};

use super::audit::{chain::Chain, AuditVia, Event};
use super::caps::Tier;
use super::ctx::AccessCtx;
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code};

/// The token prefix (P-4).
pub const TOKEN_PREFIX: &str = "ikd1.";
/// The browser cookie (§3.8).
pub const COOKIE: &str = "ikenga_device";
/// `Max-Age` of the device cookie (400 days, §3.8).
pub const COOKIE_MAX_AGE_SECS: u64 = 34_560_000;

pub const ROTATE_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub const ROTATION_GRACE: Duration = Duration::from_secs(5 * 60);
pub const IDLE_EXPIRY: Duration = Duration::from_secs(90 * 24 * 60 * 60);
/// `last_seen_*` is written at most once per this, per device (§3.9).
pub const LAST_SEEN_EVERY: Duration = Duration::from_secs(60);

/// WS close codes (P-12).
pub const CLOSE_REVOKED: u16 = 4401;
pub const CLOSE_CAPS_CHANGED: u16 = 4403;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A fresh lowercase hyphenated UUIDv7 (device / pairing ids).
pub fn new_id() -> String {
    uuid::Uuid::now_v7().hyphenated().to_string()
}

/// One `devices` row (§3.11), minus the secret hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub device_id: String,
    pub principal_id: String,
    pub kind: String,
    pub name: String,
    pub platform: Option<String>,
    pub tier: Tier,
    pub grant_epoch: i64,
    pub paired_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_seen_addr: Option<String>,
    pub secret_rotated_at: Option<i64>,
    pub revoked_at: Option<i64>,
}

impl DeviceRow {
    pub fn is_host(&self) -> bool {
        self.kind == "host"
    }

    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    pub fn view(&self, live_sockets: usize, this_device: bool) -> DeviceView {
        DeviceView {
            device_id: self.device_id.clone(),
            kind: self.kind.clone(),
            name: self.name.clone(),
            platform: self.platform.clone(),
            tier: self.tier.as_str(),
            paired_at: self.paired_at,
            last_seen_at: self.last_seen_at,
            last_seen_addr: self.last_seen_addr.clone(),
            live_sockets,
            this_device,
        }
    }
}

/// `DeviceView` (§9.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    pub device_id: String,
    pub kind: String,
    pub name: String,
    pub platform: Option<String>,
    pub tier: &'static str,
    pub paired_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_seen_addr: Option<String>,
    pub live_sockets: usize,
    pub this_device: bool,
}

const ROW_COLUMNS: &str = "device_id, principal_id, kind, name, platform, tier, grant_epoch, \
    paired_at, last_seen_at, last_seen_addr, secret_rotated_at, revoked_at";

fn row_from(r: &sqlx::sqlite::SqliteRow) -> Result<DeviceRow, sqlx::Error> {
    let tier: String = r.try_get("tier")?;
    Ok(DeviceRow {
        device_id: r.try_get("device_id")?,
        principal_id: r.try_get("principal_id")?,
        kind: r.try_get("kind")?,
        name: r.try_get("name")?,
        platform: r.try_get("platform")?,
        // The CHECK constraint makes anything else impossible; fail low.
        tier: Tier::parse(&tier).unwrap_or(Tier::View),
        grant_epoch: r.try_get("grant_epoch")?,
        paired_at: r.try_get("paired_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        last_seen_addr: r.try_get("last_seen_addr")?,
        secret_rotated_at: r.try_get("secret_rotated_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

pub async fn get(
    conn: &mut SqliteConnection,
    device_id: &str,
) -> Result<Option<DeviceRow>, sqlx::Error> {
    sqlx::query(&format!(
        "SELECT {ROW_COLUMNS} FROM devices WHERE device_id = ?"
    ))
    .bind(device_id)
    .fetch_optional(&mut *conn)
    .await?
    .map(|r| row_from(&r))
    .transpose()
}

/// A principal's live (unrevoked) devices, host first, then by pairing time.
pub async fn list_active(
    conn: &mut SqliteConnection,
    principal_id: &str,
) -> Result<Vec<DeviceRow>, sqlx::Error> {
    sqlx::query(&format!(
        "SELECT {ROW_COLUMNS} FROM devices WHERE principal_id = ? AND revoked_at IS NULL \
         ORDER BY kind = 'host' DESC, paired_at"
    ))
    .bind(principal_id)
    .fetch_all(&mut *conn)
    .await?
    .iter()
    .map(row_from)
    .collect()
}

/// A freshly minted device secret: the token part and its stored hash.
pub struct MintedSecret {
    /// base64url, no padding (43 chars). Goes to the client once; never stored.
    pub secret: String,
    pub sha256: [u8; 32],
}

pub fn mint_secret() -> MintedSecret {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let sha256 = Sha256::digest(raw).into();
    MintedSecret { secret, sha256 }
}

pub fn token(device_id: &str, secret: &str) -> String {
    format!("{TOKEN_PREFIX}{device_id}.{secret}")
}

/// `ikd1.<device_id>.<secret>` → `(device_id, SHA-256(secret bytes))`.
pub fn parse_token(raw: &str) -> Option<(String, [u8; 32])> {
    let rest = raw.strip_prefix(TOKEN_PREFIX)?;
    let (device_id, secret) = rest.split_once('.')?;
    if device_id.len() != 36 || device_id.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret)
        .ok()?;
    if bytes.len() != 32 {
        return None;
    }
    Some((device_id.to_string(), Sha256::digest(&bytes).into()))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Insert a newly paired device (WP-74b's `access_pair_decide(allow)`), in
/// the caller's transaction. Returns the row; the caller audits
/// `pair.allowed` in the same transaction.
#[allow(clippy::too_many_arguments)]
pub async fn insert_paired(
    conn: &mut SqliteConnection,
    principal_id: &str,
    name: &str,
    platform: Option<&str>,
    tier: Tier,
    secret_sha256: &[u8; 32],
    paired_via_device: Option<&str>,
    pairing_id: Option<&str>,
) -> Result<DeviceRow, sqlx::Error> {
    let device_id = new_id();
    let now = now_ms();
    let name: String = name.chars().take(64).collect();
    sqlx::query(
        "INSERT INTO devices (device_id, principal_id, kind, name, platform, tier, \
         secret_sha256, secret_rotated_at, paired_at, paired_via_device, pairing_id) \
         VALUES (?, ?, 'paired', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&device_id)
    .bind(principal_id)
    .bind(if name.trim().is_empty() {
        "Device".to_string()
    } else {
        name
    })
    .bind(platform)
    .bind(tier.as_str())
    .bind(secret_sha256.as_slice())
    .bind(now)
    .bind(now)
    .bind(paired_via_device)
    .bind(pairing_id)
    .execute(&mut *conn)
    .await?;
    get(conn, &device_id).await?.ok_or(sqlx::Error::RowNotFound)
}

/// How the device credential arrived (§3.9: only a cookie rotates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    Cookie,
    Bearer,
}

/// §2.4: the one device credential a request presents, picked the same way
/// on T0 (`auth_middleware`) and T1 (`device_cookie_middleware` and the
/// `DeviceGrantResolver`): an `Authorization: Bearer ikd1.…` first, else the
/// `ikenga_device` cookie. Only the picked one is evaluated — a present
/// bearer decides even when it is invalid, so a non-browser client's own
/// credential never falls back to a cookie a jar happened to send.
pub fn presented(headers: &axum::http::HeaderMap) -> Option<(String, Presented)> {
    bearer_from(headers)
        .map(|t| (t, Presented::Bearer))
        .or_else(|| cookie_from(headers).map(|t| (t, Presented::Cookie)))
}

/// What a response does with the `ikenga_device` cookie (§2.4, §3.9),
/// decided the same on T0 and T1 and attached to **every** response of the
/// request — a refusal included, since a rotation is committed before the
/// request is authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieAction {
    Keep,
    /// A valid cookie whose secret is due (never a bearer, never on a WS
    /// handshake, which can't carry `Set-Cookie` back to a jar reliably).
    Rotate,
    /// A present but dead cookie: cleared (`Max-Age=0`), without failing a
    /// request another credential authenticates.
    Clear,
}

pub fn cookie_action(auth: &DeviceAuth, presented: Presented, upgrade: bool) -> CookieAction {
    match (auth, presented) {
        (
            DeviceAuth::Valid {
                rotation_due: true, ..
            },
            Presented::Cookie,
        ) if !upgrade => CookieAction::Rotate,
        (DeviceAuth::Invalid(_), Presented::Cookie) => CookieAction::Clear,
        _ => CookieAction::Keep,
    }
}

/// The outcome of presenting a device token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuth {
    Valid {
        row: DeviceRow,
        /// The presented secret is the current one and older than 30 days:
        /// a **cookie** presentation should [`rotate`] it (§3.9). A bearer
        /// presentation never does (P-32).
        rotation_due: bool,
    },
    /// Present but not valid: unknown, revoked, idle-expired, or a wrong
    /// secret. The reason is for logs only — the client sees `401`.
    Invalid(&'static str),
}

/// The in-memory `last_seen` / rotation gate (§3.9: ≤1 write per 60 s per
/// device).
#[derive(Default)]
pub struct SeenGate(Mutex<HashMap<String, Instant>>);

impl SeenGate {
    fn due(&self, device_id: &str) -> bool {
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        match map.get(device_id) {
            Some(at) if now.duration_since(*at) < LAST_SEEN_EVERY => false,
            _ => {
                map.insert(device_id.to_string(), now);
                true
            }
        }
    }
}

/// Resolve a presented device token against the store (§2.3 DeviceGrant
/// row): exactly one `principal_id` per grant (A-5). Account checks (T1:
/// exists, not disabled) are the caller's.
pub async fn resolve(
    store: &AccessStore,
    gate: &SeenGate,
    raw: &str,
    remote_addr: Option<&str>,
) -> anyhow::Result<DeviceAuth> {
    let Some((device_id, presented_hash)) = parse_token(raw) else {
        return Ok(DeviceAuth::Invalid("malformed"));
    };
    let mut conn = store.pool().acquire().await?;
    let row = sqlx::query(
        "SELECT secret_sha256, prev_secret_sha256, prev_valid_until FROM devices \
         WHERE device_id = ? AND kind = 'paired'",
    )
    .bind(&device_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(secrets) = row else {
        return Ok(DeviceAuth::Invalid("unknown"));
    };
    let Some(device) = get(&mut conn, &device_id).await? else {
        return Ok(DeviceAuth::Invalid("unknown"));
    };
    if device.is_revoked() {
        return Ok(DeviceAuth::Invalid("revoked"));
    }
    let now = now_ms();
    let current: Option<Vec<u8>> = secrets.try_get(0)?;
    let prev: Option<Vec<u8>> = secrets.try_get(1)?;
    let prev_until: Option<i64> = secrets.try_get(2)?;
    let matches_current = current
        .as_deref()
        .is_some_and(|h| ct_eq(h, &presented_hash));
    let matches_prev = prev.as_deref().is_some_and(|h| ct_eq(h, &presented_hash))
        && prev_until.is_some_and(|until| now <= until);
    if !matches_current && !matches_prev {
        return Ok(DeviceAuth::Invalid("secret"));
    }

    // Idle expiry (§3.9): judged on the last presentation, or pairing.
    let last = device.last_seen_at.unwrap_or(device.paired_at);
    if now - last > IDLE_EXPIRY.as_millis() as i64 {
        expire_idle(store, &mut conn, &device).await?;
        return Ok(DeviceAuth::Invalid("expired"));
    }

    if gate.due(&device.device_id) {
        sqlx::query("UPDATE devices SET last_seen_at = ?, last_seen_addr = ? WHERE device_id = ?")
            .bind(now)
            .bind(remote_addr)
            .bind(&device.device_id)
            .execute(&mut *conn)
            .await?;
    }
    // Rotation is due only off the current secret (never off the grace one).
    let age = now - device.secret_rotated_at.unwrap_or(device.paired_at);
    let rotation_due = matches_current && age > ROTATE_AFTER.as_millis() as i64;
    Ok(DeviceAuth::Valid {
        row: device,
        rotation_due,
    })
}

/// §3.9 rotation: a new secret now, the old hash valid for the 5-minute
/// grace (covers concurrent requests on the old cookie). Returns the new
/// token for `Set-Cookie`, or `None` when a concurrent request already
/// rotated it (the conditional UPDATE makes rotation happen once). Callers
/// rotate only a cookie presentation.
pub async fn rotate(store: &AccessStore, device_id: &str) -> anyhow::Result<Option<String>> {
    let now = now_ms();
    let mut conn = store.pool().acquire().await?;
    let fresh = mint_secret();
    let n = sqlx::query(
        "UPDATE devices SET prev_secret_sha256 = secret_sha256, prev_valid_until = ?, \
         secret_sha256 = ?, secret_rotated_at = ? WHERE device_id = ? AND revoked_at IS NULL \
         AND coalesce(secret_rotated_at, paired_at) < ?",
    )
    .bind(now + ROTATION_GRACE.as_millis() as i64)
    .bind(fresh.sha256.as_slice())
    .bind(now)
    .bind(device_id)
    .bind(now - ROTATE_AFTER.as_millis() as i64)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok((n == 1).then(|| token(device_id, &fresh.secret)))
}

/// Mark an idle device revoked and audit `device.expired` (one transaction).
async fn expire_idle(
    store: &AccessStore,
    conn: &mut SqliteConnection,
    device: &DeviceRow,
) -> anyhow::Result<()> {
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let n = mark_revoked(&mut tx, &device.device_id, None, "idle").await?;
    if n == 0 {
        return Ok(());
    }
    let ev = Event::new("device.expired", AuditVia::System)
        .subject_principal(device.principal_id.clone())
        .subject_device(device.device_id.clone())
        .target(device.name.clone());
    let head = store.chain().append(&mut tx, &ev).await?;
    tx.commit().await?;
    store.chain().committed(head);
    Ok(())
}

/// Revoke in place: `revoked_*`, hashes nulled, `grant_epoch` bumped. The
/// row stays as a tombstone; its id is never reused (§3.10). The device's
/// push subscriptions (plans/pwa S2) go in the same transaction: every
/// revoke path — user, undelivered, idle expiry, forced logout — comes
/// through here.
async fn mark_revoked(
    conn: &mut SqliteConnection,
    device_id: &str,
    by: Option<&str>,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let n = mark_revoked_row(conn, device_id, by, reason).await?;
    if n > 0 {
        crate::server::push::store::delete_for_device(conn, device_id).await?;
    }
    Ok(n)
}

async fn mark_revoked_row(
    conn: &mut SqliteConnection,
    device_id: &str,
    by: Option<&str>,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE devices SET revoked_at = ?, revoked_by = ?, revoked_reason = ?, \
         secret_sha256 = NULL, prev_secret_sha256 = NULL, prev_valid_until = NULL, \
         grant_epoch = grant_epoch + 1 WHERE device_id = ? AND revoked_at IS NULL AND kind = 'paired'",
    )
    .bind(now_ms())
    .bind(by)
    .bind(reason)
    .bind(device_id)
    .execute(&mut *conn)
    .await?
    .rows_affected())
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

/// Load a device the caller may manage: it exists, is not revoked, and
/// belongs to the caller's principal. Anything else is `not_found` (no
/// existence oracle across principals).
async fn owned(
    conn: &mut SqliteConnection,
    ctx: &AccessCtx,
    device_id: &str,
) -> Result<DeviceRow, AccessError> {
    let row = get(conn, device_id)
        .await
        .map_err(AccessError::internal)?
        .filter(|d| !d.is_revoked() && d.principal_id == ctx.principal_id.to_string())
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such device"))?;
    Ok(row)
}

/// `access_device_set_tier` (§3.10): `admin_strength`; own principal; not
/// the host. Bumps `grant_epoch`; the caller closes the device's sockets
/// with 4403.
pub async fn set_tier(
    store: &AccessStore,
    ctx: &AccessCtx,
    device_id: &str,
    tier: Tier,
) -> Result<(DeviceRow, Tier), AccessError> {
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let before = owned(&mut tx, ctx, device_id).await?;
    if before.is_host() {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "the host device is always full",
        ));
    }
    if before.tier == tier {
        return Ok((before.clone(), before.tier));
    }
    sqlx::query("UPDATE devices SET tier = ?, grant_epoch = grant_epoch + 1 WHERE device_id = ?")
        .bind(tier.as_str())
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
    let ev = Event::by("device.tier_changed", ctx)
        .subject_principal(before.principal_id.clone())
        .subject_device(device_id)
        .target(before.name.clone())
        .detail(serde_json::json!({ "from": before.tier.as_str(), "to": tier.as_str() }));
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    let after = get(&mut tx, device_id)
        .await
        .map_err(AccessError::internal)?
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such device"))?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok((after, before.tier))
}

/// `access_device_revoke` (§3.10): `admin_strength`, or the device revoking
/// itself ("Forget this device"). Never the host. The caller closes the
/// device's sockets with 4401 `device_revoked`.
pub async fn revoke(
    store: &AccessStore,
    ctx: &AccessCtx,
    device_id: &str,
) -> Result<DeviceRow, AccessError> {
    let itself = ctx.device_id.as_deref() == Some(device_id);
    if !ctx.admin_strength && !itself {
        return Err(AccessError::forbidden_admin());
    }
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let row = owned(&mut tx, ctx, device_id).await?;
    if row.is_host() {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "the host device can't be revoked",
        ));
    }
    let by = ctx.principal_id.to_string();
    mark_revoked(&mut tx, device_id, Some(&by), "user")
        .await
        .map_err(AccessError::internal)?;
    let ev = Event::by("device.revoked", ctx)
        .subject_principal(row.principal_id.clone())
        .subject_device(device_id)
        .target(row.name.clone())
        .detail(serde_json::json!({ "reason": "user" }));
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(row)
}

/// WP-74b review m1: a grant minted at `decide(allow)` whose token never
/// reached the device (the session expired, or was dropped, before the
/// first status poll). The row would otherwise be a live grant nobody
/// holds; it is revoked in place (`revoked_reason = 'user'`, the DDL's
/// closed set — the audit detail says `undelivered`), one `device.revoked`
/// row, continuing on a degraded chain (killing a credential, P-35).
pub async fn revoke_undelivered(
    store: &AccessStore,
    device_id: &str,
    pairing_id: &str,
) -> anyhow::Result<()> {
    let mut conn = store.pool().acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT principal_id, name FROM devices WHERE device_id = ?")
            .bind(device_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((principal_id, name)) = row else {
        return Ok(());
    };
    if mark_revoked(&mut tx, device_id, None, "user").await? == 0 {
        return Ok(());
    }
    let ev = Event::new("device.revoked", AuditVia::System)
        .subject_principal(principal_id)
        .subject_device(device_id)
        .target(name)
        .detail(serde_json::json!({ "reason": "undelivered", "pairing_id": pairing_id }))
        .continue_when_degraded();
    let head = store.chain().append(&mut tx, &ev).await?;
    tx.commit().await?;
    store.chain().committed(head);
    Ok(())
}

/// R-11: a forced logout revokes **every** grant of the principal
/// (`revoked_reason = 'sessions_revoked'`, one `device.revoked` row each),
/// inside the forced-logout transaction. Returns the revoked device ids.
///
/// These appends continue on a degraded chain: killing credentials is part
/// of the authentication path (P-35), never paused.
pub async fn revoke_all_for_sessions_revoked(
    conn: &mut SqliteConnection,
    chain: &Chain,
    principal_id: &str,
    via: AuditVia,
) -> anyhow::Result<Vec<String>> {
    let ids: Vec<(String, String)> = sqlx::query_as(
        "SELECT device_id, name FROM devices WHERE principal_id = ? AND kind = 'paired' \
         AND revoked_at IS NULL ORDER BY paired_at",
    )
    .bind(principal_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, name) in &ids {
        mark_revoked(conn, id, None, "sessions_revoked").await?;
        let ev = Event::new("device.revoked", via)
            .subject_principal(principal_id)
            .subject_device(id.clone())
            .target(name.clone())
            .detail(serde_json::json!({ "reason": "sessions_revoked" }))
            .continue_when_degraded();
        chain.append(conn, &ev).await?;
    }
    Ok(ids.into_iter().map(|(id, _)| id).collect())
}

/// A Tailscale address (Round 19, DEC-R19-1): IPv4 `100.64.0.0/10`
/// (Tailscale's CGNAT range), IPv6 `fd7a:115c:a1e0::/48` (its ULA prefix), or
/// an IPv4-mapped `::ffff:100.64.0.0/106` (a dual-stack listener's view of an
/// IPv4 tailnet peer).
pub fn is_tailnet_ip(ip: IpAddr) -> bool {
    fn v4(ip: Ipv4Addr) -> bool {
        let [a, b, ..] = ip.octets();
        a == 100 && (b & 0xc0) == 0x40
    }
    match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(mapped) => v4(mapped),
            None => matches!(ip.segments(), [0xfd7a, 0x115c, 0xa1e0, ..]),
        },
    }
}

/// Whether a `Set-Cookie` (device cookie, or a session cookie where one
/// applies) omits `Secure` (G-PRINCIPAL §12.2 P-3 as amended by Round 19,
/// DEC-R19-1):
///
/// * `--insecure-cookie` always drops it, on every tier.
/// * On **T0** only, a request whose **TCP peer** (axum `ConnectInfo`) is a
///   Tailscale address ([`is_tailnet_ip`]) drops it too: that hop is already
///   WireGuard-encrypted end to end, and a browser drops a `Secure` cookie
///   set over the daemon's plain-HTTP tailnet bind. `X-Forwarded-For` never
///   decides this, so the caller passes the socket peer and nothing else.
/// * The T1 broker is unchanged: P-3 plus `--insecure-cookie` as signed.
pub fn cookie_insecure(tier: StoreTier, insecure_flag: bool, peer: Option<IpAddr>) -> bool {
    insecure_flag || (tier == StoreTier::T0 && peer.is_some_and(is_tailnet_ip))
}

/// Whether a link's host is a tailnet address — an IP in the
/// [`is_tailnet_ip`] ranges, or a MagicDNS `*.ts.net` name (which resolves
/// only to those) — so that a device opening it reaches the T0 daemon from a
/// tailnet peer and gets a non-`Secure` cookie (DEC-R19-1). Only a
/// prediction for the pair sheet; the server decides per request by the
/// peer address.
pub fn is_tailnet_url(link: &str) -> bool {
    match url::Url::parse(link).ok().as_ref().and_then(url::Url::host) {
        Some(url::Host::Ipv4(ip)) => is_tailnet_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_tailnet_ip(IpAddr::V6(ip)),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d.ends_with(".ts.net") && d.len() > ".ts.net".len()
        }
        None => false,
    }
}

/// `Set-Cookie` for a device token (§3.8). `Secure` unless `insecure` —
/// [`cookie_insecure`] decides that per request.
pub fn set_cookie(token: &str, insecure: bool) -> String {
    format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={COOKIE_MAX_AGE_SECS}{}",
        if insecure { "" } else { "; Secure" }
    )
}

/// The cookie that clears a dead device credential (§2.4).
pub fn clear_cookie(insecure: bool) -> String {
    format!(
        "{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{}",
        if insecure { "" } else { "; Secure" }
    )
}

/// The `ikenga_device` cookie value from a `Cookie` header set.
pub fn cookie_from(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

/// `Authorization: Bearer ikd1.…` (non-browser clients, §2.4).
pub fn bearer_from(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|t| t.starts_with(TOKEN_PREFIX))
        .map(str::to_string)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::access::caps::CapSet;
    use crate::access::ctx::{RequestMeta, Via};

    pub(crate) fn operator_ctx(store: &AccessStore) -> AccessCtx {
        AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            via: Via::Operator,
            device_id: store.meta().host_device_id.clone(),
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::ALL,
            admin_strength: true,
            meta: RequestMeta::default(),
        }
    }

    pub(crate) async fn pair(store: &AccessStore, tier: Tier) -> (DeviceRow, String) {
        let owner = store.meta().owner_principal_id.unwrap().to_string();
        let minted = mint_secret();
        let mut conn = store.pool().acquire().await.unwrap();
        let row = insert_paired(
            &mut conn,
            &owner,
            "Pixel 9 · Chrome",
            Some("android"),
            tier,
            &minted.sha256,
            None,
            None,
        )
        .await
        .unwrap();
        let tok = token(&row.device_id, &minted.secret);
        (row, tok)
    }

    fn device_ctx(store: &AccessStore, row: &DeviceRow) -> AccessCtx {
        AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            via: Via::Device {
                device_id: row.device_id.clone(),
            },
            device_id: Some(row.device_id.clone()),
            tier: row.tier,
            share: None,
            share_headers: false,
            caps: row.tier.caps(),
            admin_strength: AccessCtx::admin_strength_of(
                &Via::Device {
                    device_id: row.device_id.clone(),
                },
                row.tier,
            ),
            meta: RequestMeta::default(),
        }
    }

    #[test]
    fn tokens_parse_strictly() {
        let m = mint_secret();
        assert_eq!(m.secret.len(), 43);
        let id = new_id();
        let t = token(&id, &m.secret);
        assert_eq!(parse_token(&t), Some((id.clone(), m.sha256)));
        assert_eq!(parse_token(&t.replacen("ikd1.", "ikd2.", 1)), None);
        assert_eq!(parse_token(&format!("ikd1.{id}.short")), None);
        assert_eq!(
            parse_token(&format!("ikd1.{}.{}", id.to_uppercase(), m.secret)),
            None
        );
        assert_eq!(parse_token("deadbeef"), None);
    }

    /// A-5: a grant resolves to exactly one principal; unknown, revoked or
    /// wrong-secret tokens are invalid.
    #[tokio::test]
    async fn resolve_valid_unknown_revoked_and_wrong_secret() {
        let store = AccessStore::memory_t0().await;
        let gate = SeenGate::default();
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        match resolve(&store, &gate, &tok, Some("100.1.2.3"))
            .await
            .unwrap()
        {
            DeviceAuth::Valid {
                row: got,
                rotation_due,
            } => {
                assert_eq!(got.principal_id, row.principal_id);
                assert!(!rotation_due);
            }
            other => panic!("{other:?}"),
        }
        let wrong = token(&row.device_id, &mint_secret().secret);
        assert_eq!(
            resolve(&store, &gate, &wrong, None).await.unwrap(),
            DeviceAuth::Invalid("secret")
        );
        let unknown = token(&new_id(), &mint_secret().secret);
        assert_eq!(
            resolve(&store, &gate, &unknown, None).await.unwrap(),
            DeviceAuth::Invalid("unknown")
        );
        revoke(&store, &operator_ctx(&store), &row.device_id)
            .await
            .unwrap();
        assert_eq!(
            resolve(&store, &gate, &tok, None).await.unwrap(),
            DeviceAuth::Invalid("revoked")
        );
    }

    /// Handover lead L74-4 / §2.4: the one device credential a request
    /// presents and what the response does with the cookie — the functions
    /// T0's `auth_middleware`, T1's `device_cookie_middleware` and T1's
    /// `DeviceGrantResolver` all call, so the order can't drift.
    #[test]
    fn device_credential_precedence_is_one_table() {
        use axum::http::HeaderMap;
        let h = |pairs: &[(&str, &str)]| {
            let mut m = HeaderMap::new();
            for (k, v) in pairs {
                m.append(
                    axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                );
            }
            m
        };
        let bearer = "Bearer ikd1.dev-b.secret";
        let cookie = "ikenga_device=ikd1.dev-c.secret";
        assert_eq!(presented(&h(&[])), None);
        assert_eq!(
            presented(&h(&[("cookie", cookie)])),
            Some(("ikd1.dev-c.secret".into(), Presented::Cookie))
        );
        // A device bearer wins over the cookie, whichever is valid.
        assert_eq!(
            presented(&h(&[("cookie", cookie), ("authorization", bearer)])),
            Some(("ikd1.dev-b.secret".into(), Presented::Bearer))
        );
        // The operator bearer is not a device credential: the cookie is.
        assert_eq!(
            presented(&h(&[
                ("cookie", cookie),
                ("authorization", "Bearer 0123abcd")
            ])),
            Some(("ikd1.dev-c.secret".into(), Presented::Cookie))
        );
        assert_eq!(presented(&h(&[("cookie", "ikenga_device=")])), None);

        let row = DeviceRow {
            device_id: "d".into(),
            principal_id: "p".into(),
            kind: "paired".into(),
            name: "n".into(),
            platform: None,
            tier: Tier::View,
            grant_epoch: 0,
            paired_at: 0,
            last_seen_at: None,
            last_seen_addr: None,
            secret_rotated_at: None,
            revoked_at: None,
        };
        let due = DeviceAuth::Valid {
            row: row.clone(),
            rotation_due: true,
        };
        let fresh = DeviceAuth::Valid {
            row,
            rotation_due: false,
        };
        let dead = DeviceAuth::Invalid("revoked");
        use CookieAction::*;
        for (auth, presented, upgrade, want) in [
            (&due, Presented::Cookie, false, Rotate),
            (&due, Presented::Cookie, true, Keep),
            (&due, Presented::Bearer, false, Keep),
            (&fresh, Presented::Cookie, false, Keep),
            (&dead, Presented::Cookie, false, Clear),
            (&dead, Presented::Cookie, true, Clear),
            (&dead, Presented::Bearer, false, Keep),
        ] {
            assert_eq!(
                cookie_action(auth, presented, upgrade),
                want,
                "{auth:?} {presented:?} upgrade={upgrade}"
            );
        }
    }

    /// A-35: an old secret is due for rotation; rotating keeps the old hash
    /// valid for the grace and the fresh one isn't due again. (Only a cookie
    /// presentation acts on `rotation_due` — the T0 middleware and the T1
    /// cookie layer; a bearer never does.)
    #[tokio::test]
    async fn rotation_is_due_after_30_days_with_a_grace() {
        let store = AccessStore::memory_t0().await;
        let (row, tok) = pair(&store, Tier::Approve).await;
        let old = now_ms() - ROTATE_AFTER.as_millis() as i64 - 1000;
        sqlx::query("UPDATE devices SET secret_rotated_at = ?, paired_at = ?, last_seen_at = ? WHERE device_id = ?")
            .bind(old)
            .bind(old)
            .bind(now_ms())
            .bind(&row.device_id)
            .execute(store.pool())
            .await
            .unwrap();
        let first = resolve(&store, &SeenGate::default(), &tok, None)
            .await
            .unwrap();
        assert!(
            matches!(
                first,
                DeviceAuth::Valid {
                    rotation_due: true,
                    ..
                }
            ),
            "{first:?}"
        );
        let fresh = rotate(&store, &row.device_id)
            .await
            .unwrap()
            .expect("rotated");
        assert_ne!(fresh, tok);
        assert_eq!(rotate(&store, &row.device_id).await.unwrap(), None, "once");
        // Both work now (grace); neither is due again.
        for t in [&tok, &fresh] {
            let r = resolve(&store, &SeenGate::default(), t, None)
                .await
                .unwrap();
            assert!(
                matches!(
                    r,
                    DeviceAuth::Valid {
                        rotation_due: false,
                        ..
                    }
                ),
                "{r:?}"
            );
        }
        // After the grace, only the fresh one.
        sqlx::query("UPDATE devices SET prev_valid_until = 1 WHERE device_id = ?")
            .bind(&row.device_id)
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(
            resolve(&store, &SeenGate::default(), &tok, None)
                .await
                .unwrap(),
            DeviceAuth::Invalid("secret")
        );
    }

    #[tokio::test]
    async fn idle_devices_expire_and_are_audited() {
        let store = AccessStore::memory_t0().await;
        let (row, tok) = pair(&store, Tier::View).await;
        let stale = now_ms() - IDLE_EXPIRY.as_millis() as i64 - 1000;
        sqlx::query("UPDATE devices SET last_seen_at = ? WHERE device_id = ?")
            .bind(stale)
            .bind(&row.device_id)
            .execute(store.pool())
            .await
            .unwrap();
        assert_eq!(
            resolve(&store, &SeenGate::default(), &tok, None)
                .await
                .unwrap(),
            DeviceAuth::Invalid("expired")
        );
        let reason: String =
            sqlx::query_scalar("SELECT revoked_reason FROM devices WHERE device_id = ?")
                .bind(&row.device_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(reason, "idle");
        let last: String =
            sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(last, "device.expired");
    }

    #[tokio::test]
    async fn tier_change_bumps_the_epoch_and_needs_admin_strength() {
        let store = AccessStore::memory_t0().await;
        let (row, _) = pair(&store, Tier::Dispatch).await;
        let phone = device_ctx(&store, &row);
        assert_eq!(
            set_tier(&store, &phone, &row.device_id, Tier::Full)
                .await
                .unwrap_err()
                .code,
            Code::Forbidden
        );
        let (after, from) = set_tier(&store, &operator_ctx(&store), &row.device_id, Tier::Approve)
            .await
            .unwrap();
        assert_eq!(from, Tier::Dispatch);
        assert_eq!(after.tier, Tier::Approve);
        assert_eq!(after.grant_epoch, row.grant_epoch + 1);
        let host = store.meta().host_device_id.clone().unwrap();
        assert_eq!(
            set_tier(&store, &operator_ctx(&store), &host, Tier::View)
                .await
                .unwrap_err()
                .code,
            Code::InvalidRequest
        );
    }

    #[tokio::test]
    async fn a_device_may_forget_itself_but_not_others() {
        let store = AccessStore::memory_t0().await;
        let (a, _) = pair(&store, Tier::View).await;
        let (b, _) = pair(&store, Tier::View).await;
        let a_ctx = device_ctx(&store, &a);
        assert_eq!(
            revoke(&store, &a_ctx, &b.device_id).await.unwrap_err().code,
            Code::Forbidden
        );
        revoke(&store, &a_ctx, &a.device_id).await.unwrap();
        // Tombstone, epoch bumped, hashes gone (the CHECK enforces it too).
        let (epoch, secret): (i64, Option<Vec<u8>>) =
            sqlx::query_as("SELECT grant_epoch, secret_sha256 FROM devices WHERE device_id = ?")
                .bind(&a.device_id)
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(epoch, a.grant_epoch + 1);
        assert!(secret.is_none());
        // Revoked rows are gone from the list and from management.
        let mut conn = store.pool().acquire().await.unwrap();
        let owner = store.meta().owner_principal_id.unwrap().to_string();
        let ids: Vec<String> = list_active(&mut conn, &owner)
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.device_id)
            .collect();
        assert!(!ids.contains(&a.device_id));
        assert!(ids.contains(&b.device_id));
        assert_eq!(
            ids[0],
            store.meta().host_device_id.clone().unwrap(),
            "host first"
        );
    }

    /// A-18: an access change and its audit row commit together — a refused
    /// audit append (degraded chain) rolls the change back.
    #[tokio::test]
    async fn a_failed_audit_append_rolls_the_change_back() {
        let store = AccessStore::memory_t0().await;
        let (row, _) = pair(&store, Tier::View).await;
        // Break the known head: the next append degrades and refuses.
        let mut conn = store.pool().acquire().await.unwrap();
        sqlx::raw_sql("DROP TRIGGER audit_events_no_update; UPDATE audit_events SET hash = randomblob(32) WHERE seq = 1;")
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        let err = set_tier(&store, &operator_ctx(&store), &row.device_id, Tier::Full)
            .await
            .unwrap_err();
        assert_eq!(err.code, Code::AuditUnavailable, "{err}");
        let tier: String = sqlx::query_scalar("SELECT tier FROM devices WHERE device_id = ?")
            .bind(&row.device_id)
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(tier, "view", "the tier change rolled back");
    }

    #[tokio::test]
    async fn sessions_revoked_kills_every_grant_with_audit_rows() {
        let store = AccessStore::memory_t0().await;
        let (a, _) = pair(&store, Tier::View).await;
        let (b, _) = pair(&store, Tier::Full).await;
        let owner = store.meta().owner_principal_id.unwrap().to_string();
        let mut conn = store.pool().acquire().await.unwrap();
        let mut tx = conn.begin().await.unwrap();
        let ids = revoke_all_for_sessions_revoked(&mut tx, store.chain(), &owner, AuditVia::Cli)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(ids, vec![a.device_id.clone(), b.device_id.clone()]);
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE kind = 'device.revoked' \
             AND json_extract(detail, '$.reason') = 'sessions_revoked'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(n, 2);
        let host_alive: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devices WHERE kind = 'host' AND revoked_at IS NULL",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(host_alive, 1);
    }

    /// A-12 (device half): the token never lands in a column or audit row.
    #[tokio::test]
    async fn no_token_or_secret_is_stored() {
        let store = AccessStore::memory_t0().await;
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        set_tier(&store, &operator_ctx(&store), &row.device_id, Tier::Approve)
            .await
            .unwrap();
        revoke(&store, &operator_ctx(&store), &row.device_id)
            .await
            .unwrap();
        let secret = tok.rsplit('.').next().unwrap().to_string();
        let dump: Vec<String> = sqlx::query_scalar(
            "SELECT coalesce(target,'') || coalesce(detail,'') || coalesce(principal_id,'') FROM audit_events",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        for line in dump {
            assert!(!line.contains(&secret) && !line.contains("ikd1."), "{line}");
        }
    }

    /// DEC-R19-1: the Tailscale ranges, exactly — `100.64.0.0/10`,
    /// `fd7a:115c:a1e0::/48`, and the IPv4-mapped form of the former.
    #[test]
    fn the_tailnet_predicate_matches_exactly_the_tailscale_ranges() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for s in [
            "100.64.0.0",
            "100.64.0.1",
            "100.100.100.100",
            "100.127.255.255",
            "::ffff:100.64.0.0",
            "::ffff:100.101.102.103",
            "::ffff:100.127.255.255",
            "fd7a:115c:a1e0::",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ab12:4843:cd96:6258:b240",
            "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff",
        ] {
            assert!(is_tailnet_ip(ip(s)), "{s} is tailnet");
        }
        for s in [
            "100.63.255.255",
            "100.128.0.0",
            "100.0.0.1",
            "101.64.0.1",
            "99.64.0.1",
            "10.0.0.1",
            "192.168.1.4",
            "172.16.0.1",
            "127.0.0.1",
            "0.0.0.0",
            "::ffff:100.63.255.255",
            "::ffff:100.128.0.0",
            "::ffff:192.168.1.4",
            // IPv4-compatible (deprecated), not mapped: not a tailnet peer.
            "::100.64.0.1",
            "fd7a:115c:a1df:ffff:ffff:ffff:ffff:ffff",
            "fd7a:115c:a1e1::",
            "fd7a:115c::1",
            "fd7a::1",
            "fd00::1",
            "::1",
            "::",
            "2001:db8::1",
        ] {
            assert!(!is_tailnet_ip(ip(s)), "{s} is not tailnet");
        }
    }

    /// DEC-R19-1: only a T0 tailnet peer (or `--insecure-cookie`) drops
    /// `Secure`; the T1 broker follows the flag alone.
    #[test]
    fn cookie_insecure_relaxes_only_t0_tailnet_peers() {
        let tail = Some("100.64.1.2".parse().unwrap());
        let tail6 = Some("fd7a:115c:a1e0::9".parse().unwrap());
        let lan = Some("192.168.1.4".parse().unwrap());
        for (tier, flag, peer, want) in [
            (StoreTier::T0, false, tail, true),
            (StoreTier::T0, false, tail6, true),
            (StoreTier::T0, false, lan, false),
            (StoreTier::T0, false, None, false),
            (StoreTier::T0, true, lan, true),
            (StoreTier::T0, true, None, true),
            (StoreTier::T1, false, tail, false),
            (StoreTier::T1, false, tail6, false),
            (StoreTier::T1, false, lan, false),
            (StoreTier::T1, true, tail, true),
            (StoreTier::T1, true, lan, true),
        ] {
            assert_eq!(
                cookie_insecure(tier, flag, peer),
                want,
                "{tier:?} flag={flag} peer={peer:?}"
            );
        }
    }

    #[test]
    fn tailnet_urls_are_tailnet_ip_hosts_or_magicdns_names() {
        for u in [
            "http://100.64.0.1:4000/remote/pair",
            "http://100.127.255.255/remote/pair",
            "http://[fd7a:115c:a1e0::1]:4000/remote/pair",
            "http://ned-desktop.tail1a2b.ts.net:4000/remote/pair",
            "http://NED-DESKTOP.TAIL1A2B.TS.NET/remote/pair",
        ] {
            assert!(is_tailnet_url(u), "{u}");
        }
        for u in [
            "http://100.63.255.255:4000/remote/pair",
            "http://100.128.0.0:4000/remote/pair",
            "http://192.168.1.4:4000/remote/pair",
            "http://[fd7a::1]:4000/remote/pair",
            "http://ik.example/remote/pair",
            "http://ts.net/remote/pair",
            "http://evil-ts.net/remote/pair",
            "http://localhost:4000/remote/pair",
            "not a url",
        ] {
            assert!(!is_tailnet_url(u), "{u}");
        }
    }

    #[test]
    fn cookies_are_strict_and_secure_by_default() {
        let c = set_cookie("ikd1.x.y", false);
        assert!(c.contains("HttpOnly") && c.contains("SameSite=Strict") && c.ends_with("; Secure"));
        assert!(!set_cookie("t", true).contains("Secure"));
        assert!(clear_cookie(false).contains("Max-Age=0"));
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            "cookie",
            "a=1; ikenga_device=ikd1.abc.def; b=2".parse().unwrap(),
        );
        assert_eq!(cookie_from(&h).as_deref(), Some("ikd1.abc.def"));
        h.insert("authorization", "Bearer hexoperator".parse().unwrap());
        assert_eq!(
            bearer_from(&h),
            None,
            "an operator bearer is not a device token"
        );
        h.insert("authorization", "Bearer ikd1.a.b".parse().unwrap());
        assert_eq!(bearer_from(&h).as_deref(), Some("ikd1.a.b"));
    }
}
