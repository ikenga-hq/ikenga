//! Device records and the device credential (G-ACCESS §3.8–§3.11).
//!
//! * **Format** (P-4): `ikd1.<device_id>.<secret>`, `device_id` a UUIDv7,
//!   `secret` 32 OS-random bytes in unpadded base64url. Only
//!   `SHA-256(secret)` is stored, compared in constant time; the token is
//!   never logged or audited (A-12).
//! * **Resolution** (§2.3): exactly one `principal_id` per grant
//!   (`devices.principal_id` is `NOT NULL` and nothing here ever updates it,
//!   A-5). Unknown, revoked, idle-expired → refused.
//! * **Rotation** (§3.9, P-32): only for a cookie-presented secret older than
//!   30 days; the previous hash stays valid for 5 minutes. Bearer tokens are
//!   never rotated.
//! * **Revoke / tier change** (§3.10): one transaction with the audit row,
//!   `grant_epoch` bumped; the caller then closes the device's sockets
//!   (4401 / 4403) through `access::sockets`.
//!
//! Pairing (issuing a grant after the SPAKE2 handshake and the host's
//! confirm) is WP-74b's `access::pairing`; it issues through
//! [`issue_paired_in`].

use base64::Engine as _;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::audit::chain::{self, now_ms, Event};
use super::caps::Tier;
use super::ctx::AccessCtx;
use super::store::AccessStore;
use crate::executor::PrincipalId;

/// The T1 resolver's rejection reason for a dead grant (unknown, revoked,
/// expired, wrong secret): the only one that clears the cookie.
pub const REJECTED_DEAD_GRANT: &str = "device grant: dead";

/// The HttpOnly device cookie (§2.4, P-4).
pub const DEVICE_COOKIE: &str = "ikenga_device";
/// The token prefix that tells a device grant from the T0 operator bearer.
pub const TOKEN_PREFIX: &str = "ikd1.";
/// `Max-Age` of the device cookie (§3.8): 400 days.
pub const COOKIE_MAX_AGE_SECS: i64 = 34_560_000;
/// §3.9: rotate a cookie-presented secret older than this.
pub const ROTATE_AFTER_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// §3.9: the previous hash stays valid this long after a rotation.
pub const ROTATION_GRACE_MS: i64 = 5 * 60 * 1000;
/// §3.9: a grant unseen this long is refused and revoked (`idle`).
pub const IDLE_EXPIRY_MS: i64 = 90 * 24 * 60 * 60 * 1000;

/// One `devices` row, without its secret hashes.
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
    pub secret_rotated_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub last_seen_addr: Option<String>,
    pub revoked_at: Option<i64>,
    pub revoked_reason: Option<String>,
}

/// `DeviceView` (§9.1), as the RPC returns it.
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

impl DeviceRow {
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

    pub fn principal(&self) -> Option<PrincipalId> {
        self.principal_id.parse().ok()
    }
}

/// A display name: control characters removed, trimmed, at most 64 chars
/// (the `devices.name` CHECK), never empty.
pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let capped: String = cleaned.chars().take(64).collect();
    if capped.is_empty() {
        "Device".to_string()
    } else {
        capped
    }
}

fn is_uuid_text(s: &str) -> bool {
    s.len() == 36 && !s.bytes().any(|b| b.is_ascii_uppercase()) && uuid::Uuid::try_parse(s).is_ok()
}

/// Split `ikd1.<device_id>.<secret>`; `None` for anything else.
pub fn parse_token(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('.')?;
    let well_formed = is_uuid_text(id)
        && secret.len() == 43
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    well_formed.then_some((id, secret))
}

/// A fresh secret (32 OS-random bytes, base64url, 43 chars) and its hash.
pub fn mint_secret() -> (String, [u8; 32]) {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = secret_sha256(&secret);
    (secret, hash)
}

pub fn secret_sha256(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

pub fn token(device_id: &str, secret: &str) -> String {
    format!("{TOKEN_PREFIX}{device_id}.{secret}")
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

const SELECT: &str = "SELECT device_id, principal_id, kind, name, platform, tier, grant_epoch, \
     paired_at, secret_rotated_at, last_seen_at, last_seen_addr, revoked_at, revoked_reason \
     FROM devices";

fn decode(r: &sqlx::sqlite::SqliteRow) -> Result<DeviceRow, sqlx::Error> {
    let tier: String = r.try_get(5)?;
    Ok(DeviceRow {
        device_id: r.try_get(0)?,
        principal_id: r.try_get(1)?,
        kind: r.try_get(2)?,
        name: r.try_get(3)?,
        platform: r.try_get(4)?,
        tier: Tier::parse(&tier)
            .ok_or_else(|| sqlx::Error::Decode(format!("bad tier {tier:?}").into()))?,
        grant_epoch: r.try_get(6)?,
        paired_at: r.try_get(7)?,
        secret_rotated_at: r.try_get(8)?,
        last_seen_at: r.try_get(9)?,
        last_seen_addr: r.try_get(10)?,
        revoked_at: r.try_get(11)?,
        revoked_reason: r.try_get(12)?,
    })
}

pub async fn get(conn: &mut SqliteConnection, device_id: &str) -> sqlx::Result<Option<DeviceRow>> {
    let row = sqlx::query(&format!("{SELECT} WHERE device_id = ?"))
        .bind(device_id)
        .fetch_optional(&mut *conn)
        .await?;
    row.as_ref().map(decode).transpose()
}

/// A principal's live (unrevoked) devices, host first, then by pairing time.
pub async fn list_for(
    conn: &mut SqliteConnection,
    principal_id: &str,
) -> sqlx::Result<Vec<DeviceRow>> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE principal_id = ? AND revoked_at IS NULL \
         ORDER BY kind = 'host' DESC, paired_at, device_id"
    ))
    .bind(principal_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(decode).collect()
}

/// How a device credential was presented (§2.4, §3.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presented {
    /// `ikenga_device` cookie: eligible for rotation.
    Cookie,
    /// `Authorization: Bearer ikd1.…`: never rotated.
    Bearer,
}

/// Why a presented device credential was refused. Callers answer every
/// variant with the same 401 (no oracle); the variant is for logs and the
/// cookie-clearing decision only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Malformed,
    Unknown,
    NotPairable,
    Revoked,
    BadSecret,
    /// §3.9 idle expiry: the row was revoked (`idle`) by this presentation.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    Ok {
        row: DeviceRow,
        /// The secret matched the previous hash (inside the rotation grace).
        used_prev: bool,
    },
    Refused(Refusal),
}

/// §2.3: resolve a presented device token against the store. Does not check
/// a T1 account (the T1 resolver reads `accounts` itself) and never rotates
/// (see [`rotate_if_due`]).
pub async fn resolve(store: &AccessStore, token: &str, now: i64) -> anyhow::Result<Resolved> {
    let Some((device_id, secret)) = parse_token(token) else {
        return Ok(Resolved::Refused(Refusal::Malformed));
    };
    let mut conn = store.pool.acquire().await?;
    let row = sqlx::query(
        "SELECT secret_sha256, prev_secret_sha256, prev_valid_until FROM devices WHERE device_id = ?",
    )
    .bind(device_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Resolved::Refused(Refusal::Unknown));
    };
    let cur: Option<Vec<u8>> = row.try_get(0)?;
    let prev: Option<Vec<u8>> = row.try_get(1)?;
    let prev_until: Option<i64> = row.try_get(2)?;
    let Some(device) = get(&mut conn, device_id).await? else {
        return Ok(Resolved::Refused(Refusal::Unknown));
    };
    if device.kind != "paired" {
        return Ok(Resolved::Refused(Refusal::NotPairable));
    }
    if device.revoked_at.is_some() {
        return Ok(Resolved::Refused(Refusal::Revoked));
    }
    let presented = secret_sha256(secret);
    let used_prev = match (&cur, &prev) {
        (Some(c), _) if ct_eq(c, &presented) => false,
        (_, Some(p)) if prev_until.is_some_and(|u| now < u) && ct_eq(p, &presented) => true,
        _ => return Ok(Resolved::Refused(Refusal::BadSecret)),
    };
    drop(conn);
    let last = device.last_seen_at.unwrap_or(device.paired_at);
    if now - last > IDLE_EXPIRY_MS {
        expire_idle(store, &device, now).await?;
        return Ok(Resolved::Refused(Refusal::Expired));
    }
    Ok(Resolved::Ok {
        row: device,
        used_prev,
    })
}

/// §3.9: mark an idle grant revoked (`idle`) and audit `device.expired`.
async fn expire_idle(store: &AccessStore, device: &DeviceRow, now: i64) -> anyhow::Result<()> {
    let mut tx = store.begin().await?;
    let n = sqlx::query(
        "UPDATE devices SET revoked_at = ?, revoked_by = NULL, revoked_reason = 'idle', \
         secret_sha256 = NULL, prev_secret_sha256 = NULL, prev_valid_until = NULL, \
         grant_epoch = grant_epoch + 1 WHERE device_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(&device.device_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 1 {
        let head = chain::append(
            &mut tx,
            &store.chain,
            Event::new("device.expired", "system")
                .subject(
                    Some(device.principal_id.clone()),
                    Some(device.device_id.clone()),
                )
                .target(device.name.clone()),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        tx.commit().await?;
        store.chain.committed(head);
    }
    Ok(())
}

/// §3.9: when a cookie-presented secret (the current one, not the grace
/// one) is older than 30 days, mint a new one. Returns the new token for a
/// `Set-Cookie`. Conditional on the stored hash, so two concurrent requests
/// rotate once.
pub async fn rotate_if_due(
    store: &AccessStore,
    row: &DeviceRow,
    used_prev: bool,
    presented: Presented,
    now: i64,
) -> anyhow::Result<Option<String>> {
    if presented != Presented::Cookie || used_prev {
        return Ok(None);
    }
    let since = row.secret_rotated_at.unwrap_or(row.paired_at);
    if now - since < ROTATE_AFTER_MS {
        return Ok(None);
    }
    let (secret, hash) = mint_secret();
    let n = sqlx::query(
        "UPDATE devices SET prev_secret_sha256 = secret_sha256, prev_valid_until = ?, \
         secret_sha256 = ?, secret_rotated_at = ? \
         WHERE device_id = ? AND revoked_at IS NULL AND secret_rotated_at IS ?",
    )
    .bind(now + ROTATION_GRACE_MS)
    .bind(hash.as_slice())
    .bind(now)
    .bind(&row.device_id)
    .bind(row.secret_rotated_at)
    .execute(&store.pool)
    .await?
    .rows_affected();
    Ok((n == 1).then(|| token(&row.device_id, &secret)))
}

/// §3.9: `last_seen_at` / `last_seen_addr`, at most once per 60 s per device.
pub async fn touch(store: &AccessStore, device_id: &str, addr: Option<&str>, now: i64) {
    if !store.last_seen_due(device_id, std::time::Instant::now()) {
        return;
    }
    let res = sqlx::query(
        "UPDATE devices SET last_seen_at = ?, last_seen_addr = COALESCE(?, last_seen_addr) \
         WHERE device_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(addr)
    .bind(device_id)
    .execute(&store.pool)
    .await;
    if let Err(e) = res {
        tracing::warn!("devices: last_seen for {device_id}: {e}");
    }
}

/// The low-level issue primitive WP-74b's pairing calls after `allow`
/// (§3.8), inside its transaction (with its `pair.allowed` audit row).
/// Returns the row and the token — the only time the secret exists outside
/// the client.
pub async fn issue_paired_in(
    conn: &mut SqliteConnection,
    principal_id: &str,
    name: &str,
    platform: Option<&str>,
    tier: Tier,
    pairing_id: Option<&str>,
    paired_via_device: Option<&str>,
) -> sqlx::Result<(DeviceRow, String)> {
    let device_id = uuid::Uuid::now_v7().to_string();
    let (secret, hash) = mint_secret();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO devices (device_id, principal_id, kind, name, platform, tier, secret_sha256, \
         secret_rotated_at, paired_at, paired_via_device, pairing_id) \
         VALUES (?, ?, 'paired', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&device_id)
    .bind(principal_id)
    .bind(sanitize_name(name))
    .bind(platform)
    .bind(tier.as_str())
    .bind(hash.as_slice())
    .bind(now)
    .bind(now)
    .bind(paired_via_device)
    .bind(pairing_id)
    .execute(&mut *conn)
    .await?;
    let row = get(conn, &device_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok((row, token(&device_id, &secret)))
}

/// The T1 broker's view of each resolved device's tier. G-PRINCIPAL's
/// `PrincipalCtx` carries no tier, so the DeviceGrant resolver records it
/// here and the R-3 hooks read it. A tier change bumps `grant_epoch` and
/// closes the device's sockets (§3.10), so a socket never outlives the tier
/// it was opened with; a missing entry grants nothing.
#[derive(Debug, Default)]
pub struct TierCache(std::sync::Mutex<std::collections::HashMap<String, Tier>>);

impl TierCache {
    pub fn record(&self, device_id: &str, tier: Tier) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(device_id.to_string(), tier);
    }

    pub fn get(&self, device_id: &str) -> Option<Tier> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device_id)
            .copied()
    }
}

/// An RPC-shaped error: `<code>: <message>` (§9.1).
fn err(code: &str, msg: &str) -> String {
    format!("{code}: {msg}")
}

fn audit_err(e: chain::AppendError) -> String {
    e.to_string()
}

fn sql_err(e: sqlx::Error) -> String {
    format!("internal: {e}")
}

/// Whose devices `ctx` may manage: its own principal's, nothing else.
fn owned(ctx: &AccessCtx, row: &DeviceRow) -> bool {
    ctx.principal_id
        .is_some_and(|p| p.to_string() == row.principal_id)
}

/// `access_device_revoke` (§3.10): `admin_strength`, or a device revoking
/// itself. One transaction with `device.revoked`; never the host row; the
/// id is never reused (the row stays as a tombstone). The caller closes the
/// device's sockets with 4401 after this returns.
pub async fn revoke(
    store: &AccessStore,
    ctx: &AccessCtx,
    device_id: &str,
) -> Result<DeviceRow, String> {
    let self_revoke = ctx.device_id.as_deref() == Some(device_id)
        && matches!(ctx.via, super::ctx::Credential::DeviceGrant { .. });
    if !ctx.admin_strength && !self_revoke {
        return Err(err("forbidden", "needs a full device or the host"));
    }
    let mut tx = store.begin().await.map_err(sql_err)?;
    let row = get(&mut tx, device_id)
        .await
        .map_err(sql_err)?
        .filter(|r| owned(ctx, r))
        .ok_or_else(|| err("not_found", "no such device"))?;
    if row.kind == "host" {
        return Err(err("invalid_request", "the host device can't be revoked"));
    }
    if row.revoked_at.is_some() {
        return Err(err("not_found", "no such device"));
    }
    let now = now_ms();
    sqlx::query(
        "UPDATE devices SET revoked_at = ?, revoked_by = ?, revoked_reason = 'user', \
         secret_sha256 = NULL, prev_secret_sha256 = NULL, prev_valid_until = NULL, \
         grant_epoch = grant_epoch + 1 WHERE device_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(ctx.principal_id.map(|p| p.to_string()))
    .bind(device_id)
    .execute(&mut *tx)
    .await
    .map_err(sql_err)?;
    let head = chain::append(
        &mut tx,
        &store.chain,
        Event::new("device.revoked", ctx.via.via_str())
            .actor(
                ctx.principal_id.map(|p| p.to_string()),
                ctx.device_id.clone(),
            )
            .subject(Some(row.principal_id.clone()), Some(row.device_id.clone()))
            .target(row.name.clone())
            .detail(&serde_json::json!({ "reason": "user" })),
    )
    .await
    .map_err(audit_err)?;
    tx.commit().await.map_err(sql_err)?;
    store.chain.committed(head);
    let mut conn = store.pool.acquire().await.map_err(sql_err)?;
    get(&mut conn, device_id)
        .await
        .map_err(sql_err)?
        .ok_or_else(|| err("internal", "revoked row vanished"))
}

/// `access_device_set_tier` (§3.10): `admin_strength`, own principal, not
/// the host. Bumps `grant_epoch`; the caller closes the device's sockets
/// with 4403 (`caps_changed`).
pub async fn set_tier(
    store: &AccessStore,
    ctx: &AccessCtx,
    device_id: &str,
    tier: Tier,
) -> Result<DeviceRow, String> {
    if !ctx.admin_strength {
        return Err(err("forbidden", "needs a full device or the host"));
    }
    let mut tx = store.begin().await.map_err(sql_err)?;
    let row = get(&mut tx, device_id)
        .await
        .map_err(sql_err)?
        .filter(|r| owned(ctx, r) && r.revoked_at.is_none())
        .ok_or_else(|| err("not_found", "no such device"))?;
    if row.kind == "host" {
        return Err(err("invalid_request", "the host device is always full"));
    }
    if row.tier == tier {
        return Ok(row);
    }
    sqlx::query("UPDATE devices SET tier = ?, grant_epoch = grant_epoch + 1 WHERE device_id = ?")
        .bind(tier.as_str())
        .bind(device_id)
        .execute(&mut *tx)
        .await
        .map_err(sql_err)?;
    let head = chain::append(
        &mut tx,
        &store.chain,
        Event::new("device.tier_changed", ctx.via.via_str())
            .actor(
                ctx.principal_id.map(|p| p.to_string()),
                ctx.device_id.clone(),
            )
            .subject(Some(row.principal_id.clone()), Some(row.device_id.clone()))
            .target(row.name.clone())
            .detail(&serde_json::json!({ "from": row.tier.as_str(), "to": tier.as_str() })),
    )
    .await
    .map_err(audit_err)?;
    tx.commit().await.map_err(sql_err)?;
    store.chain.committed(head);
    let mut conn = store.pool.acquire().await.map_err(sql_err)?;
    get(&mut conn, device_id)
        .await
        .map_err(sql_err)?
        .ok_or_else(|| err("internal", "row vanished"))
}

/// R-11 (§3.10): a forced logout revokes every grant of the principal
/// (`sessions_revoked`), one `device.revoked` row each, inside the forced
/// logout's own transaction. Returns the revoked device ids. A store with
/// no `devices` table yet (the access set not migrated) has none.
pub async fn revoke_all_for_principal_in(
    conn: &mut SqliteConnection,
    chain_view: &chain::Chain,
    principal_id: &str,
    via: &str,
) -> anyhow::Result<Vec<String>> {
    let has_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'devices'",
    )
    .fetch_one(&mut *conn)
    .await?;
    if has_table == 0 {
        return Ok(Vec::new());
    }
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE principal_id = ? AND kind = 'paired' AND revoked_at IS NULL"
    ))
    .bind(principal_id)
    .fetch_all(&mut *conn)
    .await?;
    let devices: Vec<DeviceRow> = rows.iter().map(decode).collect::<Result<_, _>>()?;
    let now = now_ms();
    let mut ids = Vec::new();
    for d in devices {
        sqlx::query(
            "UPDATE devices SET revoked_at = ?, revoked_by = NULL, \
             revoked_reason = 'sessions_revoked', secret_sha256 = NULL, \
             prev_secret_sha256 = NULL, prev_valid_until = NULL, \
             grant_epoch = grant_epoch + 1 WHERE device_id = ?",
        )
        .bind(now)
        .bind(&d.device_id)
        .execute(&mut *conn)
        .await?;
        chain::append(
            conn,
            chain_view,
            Event::new("device.revoked", via)
                .subject(Some(d.principal_id.clone()), Some(d.device_id.clone()))
                .target(d.name.clone())
                .detail(&serde_json::json!({ "reason": "sessions_revoked" })),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        ids.push(d.device_id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::CapSet;
    use crate::access::store::test_support;

    async fn pair(store: &AccessStore, tier: Tier) -> (DeviceRow, String) {
        let owner = store.owner.unwrap().to_string();
        let mut tx = store.begin().await.unwrap();
        let out = issue_paired_in(
            &mut tx,
            &owner,
            "Pixel 9 · Chrome",
            Some("android"),
            tier,
            None,
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        out
    }

    fn host_ctx(store: &AccessStore) -> AccessCtx {
        AccessCtx::operator(store.owner, store.host_device_id.clone())
    }

    #[test]
    fn tokens_parse_strictly() {
        let id = uuid::Uuid::now_v7().to_string();
        let (secret, _) = mint_secret();
        assert_eq!(secret.len(), 43);
        let t = token(&id, &secret);
        assert_eq!(parse_token(&t), Some((id.as_str(), secret.as_str())));
        assert_eq!(parse_token(&t.to_uppercase()), None);
        assert_eq!(parse_token(&format!("ikd2.{id}.{secret}")), None);
        assert_eq!(parse_token(&format!("ikd1.{id}.{secret}x")), None);
        assert_eq!(parse_token(&format!("ikd1.not-a-uuid.{secret}")), None);
        assert_eq!(parse_token("deadbeef"), None);
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize_name("  Pixel\n9\t·  Chrome "), "Pixel 9 · Chrome");
        assert_eq!(sanitize_name(""), "Device");
        assert_eq!(sanitize_name(&"x".repeat(100)).chars().count(), 64);
    }

    /// A-5: a grant resolves to exactly its row's principal; unknown,
    /// revoked and wrong-secret tokens are refused; the host row can't be
    /// presented.
    #[tokio::test]
    async fn resolution_is_exact_and_fails_closed() {
        let (_d, store) = test_support::t0().await;
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        let now = now_ms();
        match resolve(&store, &tok, now).await.unwrap() {
            Resolved::Ok { row: r, used_prev } => {
                assert_eq!(r.principal_id, store.owner.unwrap().to_string());
                assert_eq!(r.device_id, row.device_id);
                assert!(!used_prev);
            }
            other => panic!("{other:?}"),
        }
        let (other_secret, _) = mint_secret();
        assert_eq!(
            resolve(&store, &token(&row.device_id, &other_secret), now)
                .await
                .unwrap(),
            Resolved::Refused(Refusal::BadSecret)
        );
        let unknown = token(&uuid::Uuid::now_v7().to_string(), &other_secret);
        assert_eq!(
            resolve(&store, &unknown, now).await.unwrap(),
            Resolved::Refused(Refusal::Unknown)
        );
        let host = token(store.host_device_id.as_deref().unwrap(), &other_secret);
        assert_eq!(
            resolve(&store, &host, now).await.unwrap(),
            Resolved::Refused(Refusal::NotPairable)
        );
        revoke(&store, &host_ctx(&store), &row.device_id)
            .await
            .unwrap();
        assert_eq!(
            resolve(&store, &tok, now).await.unwrap(),
            Resolved::Refused(Refusal::Revoked)
        );
        // A-5: nothing in this module ever updates principal_id.
        let src = include_str!("devices.rs");
        let needle = ["SET principal_id", ", principal_id ="];
        for n in needle {
            assert_eq!(
                src.matches(n).count(),
                1,
                "only this assertion may name `{n}`"
            );
        }
    }

    /// §3.10: revoke bumps the epoch, nulls the hashes, keeps a tombstone,
    /// and writes `device.revoked` in the same transaction.
    #[tokio::test]
    async fn revoke_is_one_audited_transaction_and_final() {
        let (_d, store) = test_support::t0().await;
        let (row, _) = pair(&store, Tier::Approve).await;
        let ctx = host_ctx(&store);
        let revoked = revoke(&store, &ctx, &row.device_id).await.unwrap();
        assert_eq!(revoked.grant_epoch, row.grant_epoch + 1);
        assert_eq!(revoked.revoked_reason.as_deref(), Some("user"));
        let hashes: (Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as(
            "SELECT secret_sha256, prev_secret_sha256 FROM devices WHERE device_id = ?",
        )
        .bind(&row.device_id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(hashes, (None, None));
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq")
            .fetch_all(&store.pool)
            .await
            .unwrap();
        assert_eq!(kinds, ["store.created", "device.revoked"]);
        // Final: no second revoke, no un-revoke path, the host is refused.
        assert!(revoke(&store, &ctx, &row.device_id)
            .await
            .unwrap_err()
            .starts_with("not_found"));
        let host = store.host_device_id.clone().unwrap();
        assert!(revoke(&store, &ctx, &host)
            .await
            .unwrap_err()
            .starts_with("invalid_request"));
        let mut conn = store.pool.acquire().await.unwrap();
        assert!(list_for(&mut conn, &store.owner.unwrap().to_string())
            .await
            .unwrap()
            .iter()
            .all(|d| d.device_id != row.device_id));
    }

    /// P-26: a device below full can't revoke another device, but can
    /// revoke itself ("Forget this device").
    #[tokio::test]
    async fn only_admin_strength_or_the_device_itself_may_revoke() {
        let (_d, store) = test_support::t0().await;
        let (a, _) = pair(&store, Tier::Dispatch).await;
        let (b, _) = pair(&store, Tier::Dispatch).await;
        let owner = store.owner.unwrap();
        let a_ctx = AccessCtx::device(owner, a.device_id.clone(), Tier::Dispatch, a.grant_epoch);
        assert!(revoke(&store, &a_ctx, &b.device_id)
            .await
            .unwrap_err()
            .starts_with("forbidden"));
        assert!(set_tier(&store, &a_ctx, &b.device_id, Tier::View)
            .await
            .unwrap_err()
            .starts_with("forbidden"));
        revoke(&store, &a_ctx, &a.device_id).await.unwrap();
        // Another principal's device is not found, not forbidden.
        let stranger = AccessCtx::operator(Some(PrincipalId::new_v7()), None);
        assert!(revoke(&store, &stranger, &b.device_id)
            .await
            .unwrap_err()
            .starts_with("not_found"));
    }

    #[tokio::test]
    async fn a_tier_change_bumps_the_epoch_and_is_audited() {
        let (_d, store) = test_support::t0().await;
        let (row, _) = pair(&store, Tier::Dispatch).await;
        let out = set_tier(&store, &host_ctx(&store), &row.device_id, Tier::View)
            .await
            .unwrap();
        assert_eq!(out.tier, Tier::View);
        assert_eq!(out.grant_epoch, row.grant_epoch + 1);
        let detail: String = sqlx::query_scalar(
            "SELECT detail FROM audit_events WHERE kind = 'device.tier_changed'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(detail, r#"{"from":"dispatch","to":"view"}"#);
        let caps = AccessCtx::device(
            store.owner.unwrap(),
            row.device_id,
            out.tier,
            out.grant_epoch,
        )
        .caps;
        assert_eq!(
            caps,
            CapSet::of(&[
                crate::access::caps::Cap::Files,
                crate::access::caps::Cap::Sessions
            ])
        );
    }

    /// A-18 for devices: an audit failure rolls the change back.
    #[tokio::test]
    async fn an_audit_failure_rolls_the_revoke_back() {
        let (_d, store) = test_support::t0().await;
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        // Inject the failure: the audit table refuses every insert.
        sqlx::raw_sql(
            "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_events \
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .execute(&store.pool)
        .await
        .unwrap();
        let e = revoke(&store, &host_ctx(&store), &row.device_id)
            .await
            .unwrap_err();
        assert!(e.contains("injected"), "{e}");
        let mut conn = store.pool.acquire().await.unwrap();
        let after = get(&mut conn, &row.device_id).await.unwrap().unwrap();
        assert_eq!(after.revoked_at, None);
        assert_eq!(after.grant_epoch, row.grant_epoch);
        drop(conn);
        assert!(matches!(
            resolve(&store, &tok, now_ms()).await.unwrap(),
            Resolved::Ok { .. }
        ));
    }

    /// A-35: bearer-presented tokens never rotate; a cookie older than 30
    /// days rotates once, and the old secret works for 5 minutes.
    #[tokio::test]
    async fn rotation_is_cookie_only_once_with_grace() {
        let (_d, store) = test_support::t0().await;
        let (row, old) = pair(&store, Tier::Dispatch).await;
        let later = row.paired_at + ROTATE_AFTER_MS + 1;
        // Keep it from idling out in this test's clock.
        sqlx::query("UPDATE devices SET last_seen_at = ? WHERE device_id = ?")
            .bind(later)
            .bind(&row.device_id)
            .execute(&store.pool)
            .await
            .unwrap();
        let Resolved::Ok { row: r, used_prev } = resolve(&store, &old, later).await.unwrap() else {
            panic!()
        };
        assert_eq!(
            rotate_if_due(&store, &r, used_prev, Presented::Bearer, later)
                .await
                .unwrap(),
            None
        );
        let fresh = rotate_if_due(&store, &r, used_prev, Presented::Cookie, later)
            .await
            .unwrap()
            .expect("rotated");
        assert_ne!(fresh, old);
        // A concurrent request that loaded the same row rotates nothing.
        assert_eq!(
            rotate_if_due(&store, &r, used_prev, Presented::Cookie, later)
                .await
                .unwrap(),
            None
        );
        // The old secret is the grace one now; it doesn't rotate again.
        let Resolved::Ok { row: r2, used_prev } =
            resolve(&store, &old, later + 1000).await.unwrap()
        else {
            panic!()
        };
        assert!(used_prev);
        assert_eq!(
            rotate_if_due(&store, &r2, used_prev, Presented::Cookie, later)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            resolve(&store, &old, later + ROTATION_GRACE_MS + 1)
                .await
                .unwrap(),
            Resolved::Refused(Refusal::BadSecret)
        );
        assert!(matches!(
            resolve(&store, &fresh, later + ROTATION_GRACE_MS + 1)
                .await
                .unwrap(),
            Resolved::Ok {
                used_prev: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_idle_grant_expires_on_presentation() {
        let (_d, store) = test_support::t0().await;
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        let much_later = row.paired_at + IDLE_EXPIRY_MS + 1;
        assert_eq!(
            resolve(&store, &tok, much_later).await.unwrap(),
            Resolved::Refused(Refusal::Expired)
        );
        let mut conn = store.pool.acquire().await.unwrap();
        let after = get(&mut conn, &row.device_id).await.unwrap().unwrap();
        assert_eq!(after.revoked_reason.as_deref(), Some("idle"));
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE kind = 'device.expired'")
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(n, 1);
    }

    /// A-12 (device half): no token or secret lands in any column or in the
    /// audit detail.
    #[tokio::test]
    async fn no_secret_is_stored_or_audited() {
        let (dir, store) = test_support::t0().await;
        let (row, tok) = pair(&store, Tier::Dispatch).await;
        set_tier(&store, &host_ctx(&store), &row.device_id, Tier::View)
            .await
            .unwrap();
        revoke(&store, &host_ctx(&store), &row.device_id)
            .await
            .unwrap();
        let secret = parse_token(&tok).unwrap().1.to_string();
        store.pool.close().await;
        for f in std::fs::read_dir(dir.path()).unwrap() {
            let bytes = std::fs::read(f.unwrap().path()).unwrap();
            let hay = String::from_utf8_lossy(&bytes);
            assert!(
                !hay.contains(&secret),
                "the device secret reached the store files"
            );
            assert!(!hay.contains(&tok));
        }
    }
}
