//! Invites — "Share kola" (G-ACCESS §7, DEC-81; T1 only; WP-76).
//!
//! * **Token** (§7.1): `iki1.<invite_id>.<secret>`, `invite_id` a UUIDv7,
//!   `secret` 32 random bytes base64url. Only `SHA-256(token)` is stored
//!   (A-12); the URL is `<public base>/remote/invite#t=<token>`, so the
//!   token stays out of server logs.
//! * **Issue** (§7.2, `access_invite_issue`): the project's Owner or an
//!   Operator member, with an `admin_strength` credential. Role, scope and
//!   artifact are fixed at issue; an Operator invites at most Operator and
//!   nobody invites an Owner. The broker validates `projectId` on the
//!   Owner's child (`share_project_info`) and caches its name.
//!   `allow_new_account = issuer.is_admin || --member-invites-create-accounts`
//!   (§4.4, N-11 / DEC-85), fixed on the row. Single use, expiring at
//!   `min(now + --invite-ttl, memberExpiresAt)`. No email is sent (N-4).
//! * **Revoke** (§7.4, `access_invite_revoke`): the issuer or the Owner;
//!   final, no Undo (P-17). An expired invite is *dismissed*.
//! * **Inspect / accept** (§7.3, `POST /access/invite/{inspect,accept}`,
//!   public, origin-checked, throttled): accepting only sets credentials —
//!   an existing account (its session cookie) or a new one through the
//!   G-PRINCIPAL §7.2 provisioning core, `Provisioner::create_in`, inside
//!   **one** caller-owned `BEGIN IMMEDIATE` with the invite update, the
//!   membership insert and the `invite.accepted` + `member.added` audit rows
//!   (R-8, review C-07). Any failure rolls back, the `ProvisionGuard` undoes
//!   the host user and dirs, and the invite stays unaccepted (A-26). Then
//!   the Owner's child records the first `invite` notification.
//!
//! Every issue / revoke / accept is an access change: it commits with its
//! audit row (A-18) and is refused while the chain is degraded (§6.4).

use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use rand::RngCore;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Connection, SqliteConnection};

use super::audit::Event;
use super::caps::Role;
use super::ctx::AccessCtx;
use super::policy::{audit_err, project_scope, requires_t1, upsert_shared_project};
use super::rpc::{Env, PrincipalInfo};
use super::share::{broker_host, normalize_artifact_path, now_ms, owner_project_info, OwnerCalls};
use super::store::{AccessStore, StoreTier};
use super::{AccessError, AccessOptions, Code, MAX_INVITE_TTL_DAYS};
use crate::executor::PrincipalId;

/// The token's version prefix (§7.1).
pub const TOKEN_PREFIX: &str = "iki1.";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The longest an inviter or invitee label may be.
const MAX_LABEL: usize = 200;

/// `iki1.<invite_id>.<secret>` and its stored hash.
pub struct MintedToken {
    pub invite_id: String,
    pub token: String,
    pub sha256: [u8; 32],
}

/// Mint a fresh invite token (§7.1).
pub fn mint_token() -> MintedToken {
    let invite_id = PrincipalId::new_v7().to_string();
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let token = format!(
        "{TOKEN_PREFIX}{invite_id}.{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
    );
    let sha256 = token_hash(&token);
    MintedToken {
        invite_id,
        token,
        sha256,
    }
}

/// `SHA-256(token)`: the only form at rest.
pub fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.trim().as_bytes()).into()
}

/// Whether `raw` is shaped like an invite token (cheap pre-check before a
/// lookup; the hash lookup is the real test).
pub fn looks_like_token(raw: &str) -> bool {
    let raw = raw.trim();
    raw.starts_with(TOKEN_PREFIX) && raw.len() < 256 && raw.matches('.').count() == 2
}

/// The invite's URL (§7.1): the fragment keeps the token out of logs.
pub fn invite_url(base: Option<&str>, token: &str) -> String {
    let base = base.unwrap_or("").trim_end_matches('/');
    format!("{base}/remote/invite#t={token}")
}

/// The public base for an invite link: `--public-url`, else the request's
/// own origin (scheme from `Origin`, host from `Host`), else relative.
fn public_base(env_public: Option<&str>, ctx: &AccessCtx) -> Option<String> {
    if let Some(p) = env_public.filter(|p| !p.is_empty()) {
        return Some(p.to_string());
    }
    let host = ctx.meta.host.as_deref()?;
    let scheme = ctx.meta.scheme.as_deref().unwrap_or("https");
    Some(format!("{scheme}://{host}"))
}

fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

/// `access_invite_issue`, `access_invite_revoke` (§9.1).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    if env.tier == StoreTier::T0 {
        return Err(requires_t1());
    }
    let host = broker_host()
        .ok_or_else(|| AccessError::new(Code::Internal, "the broker's invite host is not wired"))?;
    match cmd {
        "access_invite_issue" => {
            let base = public_base(env.public_url.as_deref(), ctx);
            issue(
                store(env)?,
                ctx,
                &env.principal,
                &host.options,
                &*host.calls,
                base.as_deref(),
                args,
                now_ms(),
            )
            .await
        }
        "access_invite_revoke" => revoke(store(env)?, ctx, args, now_ms()).await,
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}

fn invalid(m: impl Into<String>) -> AccessError {
    AccessError::new(Code::InvalidRequest, m)
}

/// What an issue fixes on the invite (§7.2), validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueSpec {
    pub mode: String,
    pub label: Option<String>,
    pub role: Role,
    pub artifact: Option<String>,
    pub member_expires_at: Option<i64>,
}

/// Parse and validate `access_invite_issue`'s args (§7.2, §4.2). The
/// issuer is an Owner or an Operator; every role an invite can carry
/// (Operator, Reviewer, Guest) is "at most Operator", and Owner is refused.
pub fn issue_spec(args: &Value, now: i64) -> Result<IssueSpec, AccessError> {
    let s = |k: &str| args.get(k).and_then(Value::as_str);
    let mode = match s("mode") {
        Some(m @ ("email" | "link")) => m.to_string(),
        _ => return Err(invalid("mode must be email or link")),
    };
    let label = s("inviteeLabel")
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(MAX_LABEL).collect::<String>());
    if mode == "email" && label.is_none() {
        return Err(invalid("an email invite needs the address (inviteeLabel)"));
    }
    let role = match s("role").and_then(Role::parse) {
        Some(Role::Owner) => {
            return Err(AccessError::new(
                Code::Forbidden,
                "nobody invites an Owner — a project keeps one",
            ))
        }
        Some(r) => r,
        None => return Err(invalid("role must be operator, reviewer or guest")),
    };
    let artifact = match s("scope") {
        Some("project") | None => None,
        Some("artifact") => Some(normalize_artifact_path(s("artifactPath").unwrap_or(""))?),
        Some(_) => return Err(invalid("scope must be project or artifact")),
    };
    let member_expires_at = args.get("memberExpiresAt").and_then(Value::as_i64);
    if member_expires_at.is_some_and(|e| e <= now) {
        return Err(invalid("memberExpiresAt must be in the future"));
    }
    if role == Role::Guest && (artifact.is_none() || member_expires_at.is_none()) {
        return Err(invalid(
            "a Guest gets one artifact and an expiry (scope artifact + memberExpiresAt)",
        ));
    }
    Ok(IssueSpec {
        mode,
        label,
        role,
        artifact,
        member_expires_at,
    })
}

/// `access_invite_issue` (§7.2) → `{inviteId, url, expiresAt, allowNewAccount}`.
#[allow(clippy::too_many_arguments)]
pub async fn issue(
    store: &AccessStore,
    ctx: &AccessCtx,
    issuer: &PrincipalInfo,
    options: &AccessOptions,
    calls: &dyn OwnerCalls,
    base: Option<&str>,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    let (project_key, project_id, issuer_role) = project_scope(ctx, args)?;
    if !matches!(issuer_role, Role::Owner | Role::Operator) {
        return Err(AccessError::new(
            Code::Forbidden,
            "only the Owner or an Operator can invite",
        ));
    }
    if ctx
        .share
        .as_ref()
        .is_some_and(|s| s.artifact_path.is_some())
    {
        return Err(AccessError::new(
            Code::Forbidden,
            "an artifact-scoped member can't invite",
        ));
    }
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    let spec = issue_spec(args, now)?;
    let (owner, _) = super::share::split_project_key(&project_key)
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such project"))?;
    // §4.5.4: validate the project on its Owner's child, and cache its name.
    let info = owner_project_info(calls, owner, &project_id).await?;
    let allow_new_account = issuer.is_admin || options.member_invites_create_accounts;
    let ttl = i64::from(options.invite_ttl_days.clamp(1, MAX_INVITE_TTL_DAYS)) * DAY_MS;
    let expires_at = spec
        .member_expires_at
        .map_or(now + ttl, |m| m.min(now + ttl));
    let minted = mint_token();

    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    upsert_shared_project(
        &mut tx,
        &project_key,
        &project_id,
        &owner.to_string(),
        &info.name,
    )
    .await?;
    sqlx::query(
        "INSERT INTO invites (invite_id, project_key, role, scope_kind, artifact_path, \
           member_expires_at, mode, invitee_label, allow_new_account, token_sha256, issued_by, \
           issued_by_device, issued_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&minted.invite_id)
    .bind(&project_key)
    .bind(spec.role.as_str())
    .bind(if spec.artifact.is_some() {
        "artifact"
    } else {
        "project"
    })
    .bind(&spec.artifact)
    .bind(spec.member_expires_at)
    .bind(&spec.mode)
    .bind(&spec.label)
    .bind(allow_new_account as i64)
    .bind(minted.sha256.as_slice())
    .bind(ctx.principal_id.to_string())
    .bind(&ctx.device_id)
    .bind(now)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("invite.issued", ctx)
        .target(format!(
            "{} · {}",
            spec.label.clone().unwrap_or_else(|| "link".into()),
            spec.role.as_str()
        ))
        .detail(json!({
            "invite_id": minted.invite_id,
            "role": spec.role.as_str(),
            "scope": if spec.artifact.is_some() { "artifact" } else { "project" },
            "mode": spec.mode,
            "allow_new_account": allow_new_account,
        }));
    ev.project_key = Some(project_key.clone());
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(json!({
        "inviteId": minted.invite_id,
        "url": invite_url(base, &minted.token),
        "expiresAt": expires_at,
        "allowNewAccount": allow_new_account,
    }))
}

/// `access_invite_revoke {inviteId}` (§7.4): the issuer or the Owner, with
/// `admin_strength`. A pending invite is `revoked`; an expired one is
/// `dismissed`. Final (P-17).
pub async fn revoke(
    store: &AccessStore,
    ctx: &AccessCtx,
    args: &Value,
    now: i64,
) -> Result<Value, AccessError> {
    let not_found = || AccessError::new(Code::NotFound, "no such invite");
    let invite_id = args
        .get("inviteId")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`inviteId` is required"))?;
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
    let row: Option<(
        String,
        String,
        Option<i64>,
        Option<i64>,
        i64,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT project_key, issued_by, accepted_at, revoked_at, expires_at, invitee_label \
             FROM invites WHERE invite_id = ?",
    )
    .bind(invite_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let (project_key, issued_by, accepted, revoked, expires_at, label) =
        row.ok_or_else(not_found)?;
    let me = ctx.principal_id.to_string();
    let owner = project_key.split('/').next().unwrap_or_default();
    // A share request names the share's project; an own-workspace one only
    // reaches invites the caller issued or owns.
    let in_scope = match &ctx.share {
        Some(s) => s.project_key == project_key,
        None => true,
    };
    if !in_scope || (issued_by != me && owner != me) {
        return Err(not_found());
    }
    if accepted.is_some() {
        return Err(AccessError::new(
            Code::Conflict,
            "the invite was already accepted",
        ));
    }
    if revoked.is_some() {
        return Ok(json!({}));
    }
    let reason = if expires_at <= now {
        "dismissed"
    } else {
        "revoked"
    };
    sqlx::query(
        "UPDATE invites SET revoked_at = ?, revoked_by = ?, revoked_reason = ? \
         WHERE invite_id = ? AND accepted_at IS NULL AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(&me)
    .bind(reason)
    .bind(invite_id)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("invite.revoked", ctx)
        .target(label.unwrap_or_else(|| "link".into()))
        .detail(json!({ "invite_id": invite_id, "reason": reason }));
    ev.project_key = Some(project_key);
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(json!({}))
}

// ─── inspect / accept (public, T1) ──────────────────────────────────────────

/// A live invite, as `POST /access/invite/inspect` shows it (§7.3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InspectView {
    pub project_name: String,
    pub owner_username: Option<String>,
    pub role: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_expires_at: Option<i64>,
    pub allow_new_account: bool,
}

/// An unaccepted, unrevoked, unexpired invite row.
#[derive(Debug, Clone)]
pub struct LiveInvite {
    pub invite_id: String,
    pub project_key: String,
    pub role: Role,
    pub artifact_path: Option<String>,
    pub member_expires_at: Option<i64>,
    pub allow_new_account: bool,
    pub issued_by: String,
    pub expires_at: i64,
}

fn gone() -> AccessError {
    AccessError::new(
        Code::Gone,
        "This invite link is not valid: it was used, revoked or has expired.",
    )
}

/// Look `token` up (by hash): `gone` unless it is live.
pub async fn live_invite(
    conn: &mut SqliteConnection,
    token: &str,
    now: i64,
) -> Result<LiveInvite, AccessError> {
    if !looks_like_token(token) {
        return Err(gone());
    }
    type Row = (
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        i64,
        String,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT invite_id, project_key, role, artifact_path, member_expires_at, \
           allow_new_account, issued_by, expires_at, accepted_at, revoked_at \
         FROM invites WHERE token_sha256 = ?",
    )
    .bind(token_hash(token).as_slice())
    .fetch_optional(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    let Some(r) = row else { return Err(gone()) };
    if r.8.is_some() || r.9.is_some() || r.7 <= now || r.4.is_some_and(|m| m <= now) {
        return Err(gone());
    }
    Ok(LiveInvite {
        invite_id: r.0,
        project_key: r.1,
        role: Role::parse(&r.2).ok_or_else(gone)?,
        artifact_path: r.3,
        member_expires_at: r.4,
        allow_new_account: r.5 != 0,
        issued_by: r.6,
        expires_at: r.7,
    })
}

/// `POST /access/invite/inspect {token}` (§7.3).
pub async fn inspect(
    store: &AccessStore,
    token: &str,
    now: i64,
) -> Result<InspectView, AccessError> {
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let inv = live_invite(&mut conn, token, now).await?;
    let (name, owner_username): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT (SELECT display_name FROM shared_projects WHERE project_key = ?1), \
           (SELECT username FROM accounts WHERE principal_id = ?2)",
    )
    .bind(&inv.project_key)
    .bind(inv.project_key.split('/').next().unwrap_or_default())
    .fetch_one(&mut *conn)
    .await
    .unwrap_or((None, None));
    let project_id = inv
        .project_key
        .split_once('/')
        .map(|(_, p)| p.to_string())
        .unwrap_or_default();
    Ok(InspectView {
        project_name: name.unwrap_or(project_id),
        owner_username,
        role: inv.role.as_str().into(),
        scope: if inv.artifact_path.is_some() {
            "artifact"
        } else {
            "project"
        }
        .into(),
        artifact_path: inv.artifact_path,
        expires_at: inv.expires_at,
        member_expires_at: inv.member_expires_at,
        allow_new_account: inv.allow_new_account,
    })
}

/// Per-address throttle for the public invite endpoints (§7.3: "throttled
/// like §3.7"): [`InviteThrottle::MAX_FAILS`] bad tokens per address per
/// [`InviteThrottle::WINDOW_MS`] → `429 throttled`.
#[derive(Default)]
pub struct InviteThrottle {
    fails: Mutex<HashMap<String, Vec<i64>>>,
}

impl InviteThrottle {
    pub const MAX_FAILS: usize = 5;
    pub const WINDOW_MS: i64 = 10 * 60 * 1000;

    /// `Err(throttled)` while `addr` is over its budget.
    pub fn check(&self, addr: &str, now: i64) -> Result<(), AccessError> {
        let mut map = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(addr.to_string()).or_default();
        entry.retain(|t| now - t < Self::WINDOW_MS);
        if entry.len() >= Self::MAX_FAILS {
            let retry = entry.first().map_or(0, |t| Self::WINDOW_MS - (now - t));
            return Err(AccessError::new(
                Code::Throttled,
                format!("too many invalid invite links — try later. retry_after_ms={retry}"),
            ));
        }
        Ok(())
    }

    /// Whether an accept outcome spends `addr`'s budget: a gone token, and
    /// (review WP76-R8) a taken or invalid username/password — otherwise a
    /// live `allow_new_account` token is an unlimited username oracle.
    pub fn counts(code: Code) -> bool {
        matches!(code, Code::Gone | Code::Conflict | Code::InvalidRequest)
    }

    /// Record a failed (gone) lookup from `addr`.
    pub fn fail(&self, addr: &str, now: i64) {
        let mut map = self.fails.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(addr.to_string()).or_default().push(now);
        if map.len() > 4096 {
            map.retain(|_, v| v.last().is_some_and(|t| now - t < Self::WINDOW_MS));
        }
    }
}

/// How the invitee accepts (§7.3).
pub enum AcceptForm {
    /// Form 1: the request carries a valid session; the membership is added
    /// for that principal.
    Existing(PrincipalId),
    /// Form 2: new credentials (refused unless `allow_new_account`).
    New { username: String, password: String },
}

/// What an accept produced.
#[derive(Debug)]
pub struct Accepted {
    pub principal_id: PrincipalId,
    pub username: String,
    pub project_key: String,
    pub project_name: String,
    pub role: Role,
    /// Fixed at issue: `Some` for an artifact-scoped membership (WP76-R7).
    pub artifact_path: Option<String>,
    /// The Owner's username, for the share selection (WP76-R7).
    pub owner_username: Option<String>,
    pub new_account: bool,
    #[cfg(target_os = "linux")]
    pub account: Option<crate::server::operator::accounts::Account>,
}

/// The broker's invite host: what the public accept endpoint needs.
#[cfg(target_os = "linux")]
pub struct InviteHost {
    pub store: AccessStore,
    pub options: AccessOptions,
    pub provisioner: crate::server::operator::provision::Provisioner,
    pub calls: Option<std::sync::Arc<dyn OwnerCalls>>,
    pub throttle: InviteThrottle,
}

#[cfg(target_os = "linux")]
fn provision_error(e: &crate::server::operator::provision::ProvisionError) -> AccessError {
    use crate::server::operator::provision::ProvisionError as P;
    match e {
        P::Username(_) | P::Password(_) => invalid(e.to_string()),
        P::UsernameTaken(_) | P::UnixNameTaken(_) => {
            AccessError::new(Code::Conflict, e.to_string())
        }
        _ => {
            tracing::error!("invite accept: provisioning failed: {e}");
            AccessError::new(
                Code::Internal,
                "provision_failed: the server could not create the account; ask an admin",
            )
        }
    }
}

/// `POST /access/invite/accept` (§7.3): one `BEGIN IMMEDIATE` holding the
/// invite check and update, `create_in` (form 2), the membership insert and
/// the audit rows; `committed()` only after `COMMIT` (R-8).
#[cfg(target_os = "linux")]
pub async fn accept(
    host: &InviteHost,
    token: &str,
    form: AcceptForm,
    meta: &super::ctx::RequestMeta,
    now: i64,
) -> Result<Accepted, AccessError> {
    use super::audit::AuditVia;
    use crate::server::operator::accounts;
    use crate::server::operator::provision::ProvisionError;
    let store = &host.store;
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let inv = live_invite(&mut tx, token, now).await?;
    let (owner, project_id) = super::share::split_project_key(&inv.project_key).ok_or_else(gone)?;

    let (principal, username, guard, new_account) = match &form {
        AcceptForm::Existing(id) => {
            let a = accounts::by_id(&mut tx, *id)
                .await
                .map_err(AccessError::internal)?
                .filter(|a| !a.is_disabled())
                .ok_or_else(|| AccessError::new(Code::Unauthenticated, "sign in to accept"))?;
            (a.principal_id, a.username, None, false)
        }
        AcceptForm::New { username, password } => {
            if !inv.allow_new_account {
                return Err(AccessError::new(
                    Code::Forbidden,
                    "This invite is for an existing account on this server — sign in to accept.",
                ));
            }
            if let Some(max) = host.options.max_accounts {
                let n = accounts::count(&mut tx)
                    .await
                    .map_err(AccessError::internal)?;
                if n >= i64::from(max) {
                    // §4.4 / §7.3: a refused creation is a provision failure,
                    // recorded in its own transaction (review WP76-R6).
                    drop(tx);
                    host.provisioner
                        .record_provision_failed(
                            store.pool(),
                            username,
                            &ProvisionError::Host(anyhow::anyhow!(
                                "reason=max_accounts (--max-accounts {max})"
                            )),
                        )
                        .await;
                    return Err(AccessError::new(
                        Code::Forbidden,
                        "provision_failed: reason=max_accounts — this server has no room for \
                         another account",
                    ));
                }
            }
            let guard = match host
                .provisioner
                .create_in(&mut tx, username, password, false)
                .await
            {
                Ok(g) => g,
                Err(e) => {
                    let mapped = provision_error(&e);
                    drop(tx);
                    host.provisioner
                        .record_provision_failed(store.pool(), username, &e)
                        .await;
                    return Err(mapped);
                }
            };
            let a = guard.account().clone();
            (a.principal_id, a.username, Some(guard), true)
        }
    };
    // Everything after `create_in` runs in this block so that a failure
    // there (a lost single-use race, a degraded audit chain, a failed
    // COMMIT) still undoes the host user and records `auth.provision_failed`
    // in its own transaction (§7.3, review WP76-R6).
    let steps = async {
        if principal == owner {
            return Err(AccessError::new(
                Code::Conflict,
                "this is your own project — you are its Owner",
            ));
        }
        let member = principal.to_string();
        let already: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM project_members WHERE project_key = ? AND member_principal_id = ? \
             AND removed_at IS NULL",
        )
        .bind(&inv.project_key)
        .bind(&member)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
        if already.is_some() {
            return Err(AccessError::new(
                Code::Conflict,
                "you already have access to this project",
            ));
        }
        // A-25: single use — the conditional update is the claim.
        let claimed = sqlx::query(
            "UPDATE invites SET accepted_at = ?, accepted_by = ? \
             WHERE invite_id = ? AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(&member)
        .bind(&inv.invite_id)
        .execute(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
        if claimed.rows_affected() != 1 {
            return Err(gone());
        }
        sqlx::query(
            "INSERT INTO project_members (project_key, member_principal_id, role, scope_kind, \
               artifact_path, expires_at, invite_id, added_by, added_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&inv.project_key)
        .bind(&member)
        .bind(inv.role.as_str())
        .bind(if inv.artifact_path.is_some() {
            "artifact"
        } else {
            "project"
        })
        .bind(&inv.artifact_path)
        .bind(inv.member_expires_at)
        .bind(&inv.invite_id)
        .bind(&inv.issued_by)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
        let project_name: Option<String> =
            sqlx::query_scalar("SELECT display_name FROM shared_projects WHERE project_key = ?")
                .bind(&inv.project_key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(AccessError::internal)?;
        let project_name = project_name.unwrap_or_else(|| project_id.clone());
        let owner_username: Option<String> =
            sqlx::query_scalar("SELECT username FROM accounts WHERE principal_id = ?")
                .bind(owner.to_string())
                .fetch_optional(&mut *tx)
                .await
                .map_err(AccessError::internal)?;
        let via = if new_account {
            AuditVia::System
        } else {
            AuditVia::Session
        };
        let actor = |kind: &'static str| {
            let mut e = Event::new(kind, via);
            e.principal_id = Some(member.clone());
            e.subject_principal_id = Some(member.clone());
            e.project_key = Some(inv.project_key.clone());
            e.remote_addr = meta.remote_addr.clone();
            e.user_agent = meta.user_agent.clone();
            e
        };
        let accepted_ev = actor("invite.accepted")
            .target(username.clone())
            .detail(json!({ "invite_id": inv.invite_id, "new_account": new_account }));
        let added_ev = actor("member.added")
            .target(format!("{username} → {}", inv.role.as_str()))
            .detail(json!({
                "role": inv.role.as_str(),
                "scope": if inv.artifact_path.is_some() { "artifact" } else { "project" },
            }));
        let chain = store.chain();
        chain
            .append(&mut tx, &accepted_ev)
            .await
            .map_err(audit_err)?;
        let head = chain.append(&mut tx, &added_ev).await.map_err(audit_err)?;
        Ok::<_, AccessError>((head, project_name, owner_username))
    }
    .await;
    let committed = match steps {
        Ok((head, project_name, owner_username)) => tx
            .commit()
            .await
            .map(|()| (head, project_name, owner_username))
            .map_err(AccessError::internal),
        Err(e) => {
            drop(tx);
            Err(e)
        }
    };
    let (head, project_name, owner_username) = match committed {
        Ok(v) => v,
        Err(e) => {
            // The guard (if any) undoes the host user and dirs on drop.
            if let Some(g) = guard {
                drop(g);
                host.provisioner
                    .record_provision_failed(
                        store.pool(),
                        &username,
                        &ProvisionError::Host(anyhow::anyhow!("invite accept: {e}")),
                    )
                    .await;
            }
            return Err(e);
        }
    };
    let chain = store.chain();
    chain.committed(head);
    let account = guard.map(|g| g.committed());

    // §7.3: the first `invite` producer, on the Owner's child (best effort).
    if let Some(calls) = &host.calls {
        let title = format!("{username} accepted your invite");
        let body = format!("{} · {project_name}", role_title(inv.role));
        if let Err(e) = calls
            .call(
                owner,
                "notifications_record_access",
                json!({ "kind": "invite", "title": title, "body": body }),
            )
            .await
        {
            tracing::warn!("invite accepted, but the Owner's notification failed: {e}");
        }
    }
    Ok(Accepted {
        principal_id: principal,
        username,
        project_key: inv.project_key,
        project_name,
        role: inv.role,
        artifact_path: inv.artifact_path,
        owner_username,
        new_account,
        account,
    })
}

fn role_title(r: Role) -> &'static str {
    match r {
        Role::Owner => "Owner",
        Role::Operator => "Operator",
        Role::Reviewer => "Reviewer",
        Role::Guest => "Guest",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::access::policy::tests::{session, t1_store};
    use crate::access::share::BoxFuture;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An [`OwnerCalls`] whose child knows one project.
    #[derive(Default)]
    pub(crate) struct FakeChild {
        pub calls: Mutex<Vec<(String, Value)>>,
        pub closed: AtomicUsize,
    }

    impl OwnerCalls for FakeChild {
        fn call<'a>(
            &'a self,
            _owner: PrincipalId,
            cmd: &'a str,
            args: Value,
        ) -> BoxFuture<'a, Result<Value, AccessError>> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push((cmd.to_string(), args.clone()));
                match cmd {
                    "share_project_info" if args["projectId"] == "royalti-co" => {
                        Ok(json!({ "root": "/p/royalti-co", "name": "Royalti" }))
                    }
                    "share_project_info" => {
                        Err(AccessError::new(Code::NotFound, "no such project"))
                    }
                    _ => Ok(json!({ "id": 1 })),
                }
            })
        }

        fn close_principal(&self, _principal: PrincipalId) -> usize {
            self.closed.fetch_add(1, Ordering::SeqCst);
            0
        }
    }

    pub(crate) fn info(is_admin: bool) -> PrincipalInfo {
        PrincipalInfo {
            username: "ned".into(),
            is_admin,
        }
    }

    #[test]
    fn tokens_are_prefixed_and_hashed() {
        let m = mint_token();
        assert!(m.token.starts_with("iki1."));
        assert!(looks_like_token(&m.token));
        assert_eq!(m.token.split('.').nth(1), Some(m.invite_id.as_str()));
        assert_eq!(token_hash(&m.token), m.sha256);
        assert!(!looks_like_token("ikd1.x.y"));
        assert_eq!(
            invite_url(Some("https://ik.example/"), "iki1.a.b"),
            "https://ik.example/remote/invite#t=iki1.a.b"
        );
    }

    #[test]
    fn issue_rules() {
        let now = 1_000;
        let spec = |v: Value| issue_spec(&v, now);
        assert!(
            spec(json!({"mode": "email", "role": "reviewer"})).is_err(),
            "email needs a label"
        );
        assert_eq!(
            spec(json!({"mode": "link", "role": "owner"}))
                .unwrap_err()
                .code,
            Code::Forbidden
        );
        assert!(
            spec(json!({"mode": "link", "role": "guest", "scope": "artifact",
            "artifactPath": "a.md"}))
            .is_err(),
            "a Guest expires"
        );
        assert!(
            spec(json!({"mode": "link", "role": "guest", "memberExpiresAt": 2_000})).is_err(),
            "a Guest is artifact-scoped"
        );
        let ok = spec(json!({"mode": "link", "role": "guest", "scope": "artifact",
            "artifactPath": "plans/board.html", "memberExpiresAt": 2_000}))
        .unwrap();
        assert_eq!(ok.artifact.as_deref(), Some("plans/board.html"));
        assert!(spec(json!({"mode": "link", "role": "reviewer", "memberExpiresAt": 10})).is_err());
    }

    async fn issue_one(store: &AccessStore, owner: PrincipalId, args: Value, admin: bool) -> Value {
        issue(
            store,
            &session(owner),
            &info(admin),
            &AccessOptions::default(),
            &FakeChild::default(),
            Some("https://ik.example"),
            &args,
            now_ms(),
        )
        .await
        .unwrap()
    }

    /// A-12: the token is never stored; N-11: allowNewAccount follows the
    /// issuer's is_admin (or the flag); inspect shows a live invite and
    /// `gone` after revoke.
    #[tokio::test]
    async fn issue_inspect_revoke() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let args = json!({"projectId": "royalti-co", "mode": "email",
            "inviteeLabel": "tomi@example.com", "role": "reviewer"});
        let out = issue_one(&store, owner, args.clone(), false).await;
        assert_eq!(out["allowNewAccount"], false);
        let url = out["url"].as_str().unwrap();
        let token = url.split("#t=").nth(1).unwrap();
        assert!(url.starts_with("https://ik.example/remote/invite#t=iki1."));
        let stored: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM invites WHERE invitee_label = 'tomi@example.com'",
        )
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(stored, 1);
        let leak: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_events WHERE detail LIKE '%' || ? || '%'",
        )
        .bind(token.split('.').nth(2).unwrap())
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(leak, 0, "A-12: no token in audit");

        let v = inspect(&store, token, now_ms()).await.unwrap();
        assert_eq!(v.project_name, "Royalti");
        assert_eq!(v.role, "reviewer");
        assert!(!v.allow_new_account);

        let admin = issue_one(&store, owner, args, true).await;
        assert_eq!(admin["allowNewAccount"], true);

        let id = out["inviteId"].as_str().unwrap();
        let stranger = session(PrincipalId::new_v7());
        assert_eq!(
            revoke(&store, &stranger, &json!({"inviteId": id}), now_ms())
                .await
                .unwrap_err()
                .code,
            Code::NotFound
        );
        revoke(&store, &session(owner), &json!({"inviteId": id}), now_ms())
            .await
            .unwrap();
        assert_eq!(
            inspect(&store, token, now_ms()).await.unwrap_err().code,
            Code::Gone
        );
        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM audit_events WHERE category = 'people' ORDER BY seq",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(
            kinds,
            vec!["invite.issued", "invite.issued", "invite.revoked"]
        );
    }

    #[tokio::test]
    async fn an_unknown_project_or_a_reviewer_cannot_issue() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let e = issue(
            &store,
            &session(owner),
            &info(true),
            &AccessOptions::default(),
            &FakeChild::default(),
            None,
            &json!({"projectId": "nope", "mode": "link", "role": "reviewer"}),
            now_ms(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::NotFound);
        let mut reviewer = session(PrincipalId::new_v7());
        reviewer.share = Some(crate::access::ctx::ShareCtx {
            project_key: format!("{owner}/royalti-co"),
            project_id: "royalti-co".into(),
            member_principal_id: Some(reviewer.principal_id.to_string()),
            member_device_id: None,
            role: Some(Role::Reviewer),
            artifact_path: None,
            owner_approval: true,
        });
        let e = issue(
            &store,
            &reviewer,
            &info(true),
            &AccessOptions::default(),
            &FakeChild::default(),
            None,
            &json!({"projectId": "royalti-co", "mode": "link", "role": "reviewer"}),
            now_ms(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
    }

    #[test]
    fn the_throttle_counts_failures_per_address() {
        let t = InviteThrottle::default();
        for _ in 0..InviteThrottle::MAX_FAILS {
            t.check("1.2.3.4", 0).unwrap();
            t.fail("1.2.3.4", 0);
        }
        assert_eq!(t.check("1.2.3.4", 1).unwrap_err().code, Code::Throttled);
        t.check("5.6.7.8", 1).unwrap();
        t.check("1.2.3.4", InviteThrottle::WINDOW_MS + 1).unwrap();
        // WP76-R8: a taken / invalid username spends the budget too.
        for c in [Code::Gone, Code::Conflict, Code::InvalidRequest] {
            assert!(InviteThrottle::counts(c), "{c:?}");
        }
        assert!(!InviteThrottle::counts(Code::Unauthenticated));
        assert!(!InviteThrottle::counts(Code::Internal));
    }

    /// §7.3 against a real `accounts.db` and the provisioning core (a fake
    /// `/etc`, owners not enforced).
    #[cfg(target_os = "linux")]
    mod accept_tests {
        use super::*;
        use crate::access::ctx::RequestMeta;
        use crate::server::operator::provision::{Provisioner, UidRange};
        use crate::server::operator::{open_accounts, test_support::temp_root, Opener};
        use std::sync::Arc;

        const PW: &str = "correct horse battery";

        /// A minimal `/etc` under a temp prefix (the provisioning core's
        /// built-in writer works against it).
        fn fake_etc() -> tempfile::TempDir {
            use std::os::unix::fs::PermissionsExt;
            let tmp = tempfile::tempdir().unwrap();
            let etc = tmp.path().join("etc");
            std::fs::create_dir(&etc).unwrap();
            for (f, body, mode) in [
                ("passwd", "root:x:0:0:root:/root:/bin/bash\n", 0o644),
                ("group", "root:x:0:\n", 0o644),
                ("shadow", "root:*:19000:0:99999:7:::\n", 0o640),
                ("gshadow", "root:*::\n", 0o640),
            ] {
                std::fs::write(etc.join(f), body).unwrap();
                std::fs::set_permissions(etc.join(f), std::fs::Permissions::from_mode(mode))
                    .unwrap();
            }
            tmp
        }

        struct Fx {
            _root: tempfile::TempDir,
            etc_tmp: tempfile::TempDir,
            host: InviteHost,
            child: Arc<FakeChild>,
            owner: PrincipalId,
        }

        async fn fx() -> Fx {
            let (root_tmp, root) = temp_root();
            let etc_tmp = fake_etc();
            let range = UidRange::new(3_900_100_000, 3_900_100_020).unwrap();
            let prov = Provisioner::for_tests(root.clone(), range, etc_tmp.path());
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            let store = AccessStore::attach_t1(pool.clone()).await.unwrap();
            let owner = prov.create(&pool, "ned", PW, true, None).await.unwrap();
            let child = Arc::new(FakeChild::default());
            Fx {
                _root: root_tmp,
                etc_tmp,
                host: InviteHost {
                    store,
                    options: AccessOptions::default(),
                    provisioner: prov,
                    calls: Some(child.clone()),
                    throttle: Default::default(),
                },
                child,
                owner: owner.principal_id,
            }
        }

        async fn token(f: &Fx, admin: bool) -> String {
            let out = issue(
                &f.host.store,
                &session(f.owner),
                &info(admin),
                &AccessOptions::default(),
                &*f.child,
                None,
                &json!({"projectId": "royalti-co", "mode": "link", "role": "reviewer"}),
                now_ms(),
            )
            .await
            .unwrap();
            out["url"]
                .as_str()
                .unwrap()
                .split("#t=")
                .nth(1)
                .unwrap()
                .to_string()
        }

        fn new_form(name: &str) -> AcceptForm {
            AcceptForm::New {
                username: name.into(),
                password: PW.into(),
            }
        }

        fn passwd_has(f: &Fx, unix_name: &str) -> bool {
            std::fs::read_to_string(f.etc_tmp.path().join("etc/passwd"))
                .unwrap()
                .lines()
                .any(|l| l.starts_with(&format!("{unix_name}:")))
        }

        async fn count(f: &Fx, sql: &str) -> i64 {
            sqlx::query_scalar(sql)
                .fetch_one(f.host.store.pool())
                .await
                .unwrap()
        }

        /// N-11: a new-account accept needs `allow_new_account`; with it the
        /// account, membership, invite update and audit rows land together
        /// and the Owner's child gets the first `invite` notification.
        #[tokio::test]
        async fn a_new_account_accept_provisions_and_joins() {
            let f = fx().await;
            let plain = token(&f, false).await;
            let e = accept(
                &f.host,
                &plain,
                new_form("tomi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, Code::Forbidden);
            assert_eq!(count(&f, "SELECT COUNT(*) FROM accounts").await, 1);

            let t = token(&f, true).await;
            let a = accept(
                &f.host,
                &t,
                new_form("tomi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap();
            assert!(a.new_account && a.account.is_some());
            assert_eq!(a.role, Role::Reviewer);
            assert!(passwd_has(&f, "ik-tomi"));
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM project_members WHERE removed_at IS NULL"
                )
                .await,
                1
            );
            let kinds: Vec<String> = sqlx::query_scalar(
                "SELECT kind FROM audit_events WHERE category = 'people' ORDER BY seq",
            )
            .fetch_all(f.host.store.pool())
            .await
            .unwrap();
            assert_eq!(
                kinds,
                vec![
                    "invite.issued",
                    "invite.issued",
                    "invite.accepted",
                    "member.added"
                ]
            );
            assert!(f
                .child
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(c, a)| c == "notifications_record_access"
                    && a["title"] == "tomi accepted your invite"));
            // Single use.
            let e = accept(
                &f.host,
                &t,
                new_form("femi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, Code::Gone);
        }

        /// A-26: a failure after `create_in` rolls everything back — no
        /// account row, no host user, the invite unaccepted.
        #[tokio::test]
        async fn a_failed_accept_leaves_no_account_behind() {
            let f = fx().await;
            let t = token(&f, true).await;
            sqlx::query(
                "CREATE TRIGGER fail_member BEFORE INSERT ON project_members \
                 BEGIN SELECT RAISE(ABORT, 'injected'); END",
            )
            .execute(f.host.store.pool())
            .await
            .unwrap();
            let e = accept(
                &f.host,
                &t,
                new_form("tomi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, Code::Internal);
            assert_eq!(
                count(&f, "SELECT COUNT(*) FROM accounts").await,
                1,
                "only the owner"
            );
            assert!(!passwd_has(&f, "ik-tomi"), "the guard undid step 4");
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM auth_events WHERE kind = 'provision_failed'"
                )
                .await,
                1,
                "§7.3: still recorded, in its own transaction (WP76-R6)"
            );
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM invites WHERE accepted_at IS NOT NULL"
                )
                .await,
                0
            );
            sqlx::query("DROP TRIGGER fail_member")
                .execute(f.host.store.pool())
                .await
                .unwrap();
            accept(
                &f.host,
                &t,
                new_form("tomi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap();
        }

        /// §4.4: `--max-accounts` refuses the creation and records
        /// `auth.provision_failed` (review WP76-R6).
        #[tokio::test]
        async fn max_accounts_refuses_and_records_the_failure() {
            let mut f = fx().await;
            f.host.options.max_accounts = Some(1);
            let t = token(&f, true).await;
            let e = accept(
                &f.host,
                &t,
                new_form("tomi"),
                &RequestMeta::default(),
                now_ms(),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, Code::Forbidden);
            assert!(e.message.contains("max_accounts"), "{}", e.message);
            assert_eq!(count(&f, "SELECT COUNT(*) FROM accounts").await, 1);
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM auth_events WHERE kind = 'provision_failed'"
                )
                .await,
                1
            );
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM invites WHERE accepted_at IS NOT NULL"
                )
                .await,
                0
            );
        }

        /// A-25: two concurrent accepts of one token — exactly one succeeds.
        #[tokio::test]
        async fn an_invite_accepts_exactly_once_under_concurrency() {
            let f = fx().await;
            let mut ids = Vec::new();
            for name in ["ada", "bob"] {
                let a = f
                    .host
                    .provisioner
                    .create(f.host.store.pool(), name, PW, false, None)
                    .await
                    .unwrap();
                ids.push(a.principal_id);
            }
            let t = token(&f, false).await;
            let meta = RequestMeta::default();
            let (a, b) = tokio::join!(
                accept(&f.host, &t, AcceptForm::Existing(ids[0]), &meta, now_ms()),
                accept(&f.host, &t, AcceptForm::Existing(ids[1]), &meta, now_ms()),
            );
            assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "{a:?} {b:?}");
            let loser = if a.is_ok() { b } else { a };
            assert_eq!(loser.unwrap_err().code, Code::Gone);
            assert_eq!(
                count(
                    &f,
                    "SELECT COUNT(*) FROM project_members WHERE removed_at IS NULL"
                )
                .await,
                1
            );
            let e = accept(
                &f.host,
                &token(&f, false).await,
                AcceptForm::Existing(f.owner),
                &meta,
                now_ms(),
            )
            .await
            .unwrap_err();
            assert_eq!(
                e.code,
                Code::Conflict,
                "the Owner can't join their own project"
            );
        }
    }
}
