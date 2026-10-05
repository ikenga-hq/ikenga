//! Members, roles and "Shared with you" (G-ACCESS §4, §4.5.2; WP-76).
//!
//! A membership is a `project_members` row: a principal holding a role
//! (Operator / Reviewer / Guest — never Owner, A-30) in **someone else's**
//! project, scoped to the whole project or one artifact, optionally
//! expiring (a Guest always expires and is always artifact-scoped, §4.2).
//! The Owner's row in the Members table is synthetic: the Owner is the
//! principal whose workspace holds the project, and nothing changes it.
//!
//! Rows are added only by invite acceptance (`access::invites`); here the
//! Owner changes a role (`access_member_set_role`), removes a member
//! (`access_member_remove`) and undoes that within 10 s
//! (`access_member_restore`, P-17). Every change commits with its audit row
//! (§6.3, A-18) and closes the member's sockets with 4403, so they
//! reconnect on the new caps (§1.4). A member whose expiry passes is marked
//! `expired` and audited by the broker's sweeper ([`sweep_expired`]).
//!
//! T0 has one principal: every arm here answers `requires_t1` (§4.5.5).

use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{Connection, SqliteConnection};

use super::audit::{AuditVia, Event};
use super::caps::Role;
use super::ctx::AccessCtx;
use super::policy::{audit_err, project_scope, require_owner_admin, requires_t1};
use super::rpc::Env;
use super::share::{broker_host, normalize_artifact_path, now_ms, OwnerCalls};
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code};
use crate::executor::PrincipalId;

/// How long `access_member_restore` may undo a removal (D-05's toast).
pub const RESTORE_WINDOW_MS: i64 = 10_000;

/// One member, as the Members table shows it (§9.1 `MemberView`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberView {
    pub principal_id: String,
    pub username: Option<String>,
    pub role: String,
    pub scope: String,
    pub artifact_path: Option<String>,
    pub expires_at: Option<i64>,
    pub added_at: i64,
    pub last_active_at: Option<i64>,
    /// Stored, not enforced in v1 (§15 N-3).
    pub weekly_spend_cap_cents: Option<i64>,
}

/// One pending invite (§9.1 `InviteView`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteView {
    pub invite_id: String,
    pub label: Option<String>,
    pub mode: String,
    pub role: String,
    pub scope: String,
    pub artifact_path: Option<String>,
    pub issued_at: i64,
    pub issued_by: String,
    pub expires_at: i64,
    pub member_expires_at: Option<i64>,
    pub allow_new_account: bool,
    /// `pending` until the token expires, then `expired` until the Owner
    /// dismisses it (§7.4).
    pub state: String,
}

/// A project shared with the caller (§4.5.2 "Shared with you").
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareView {
    pub project_key: String,
    pub owner_principal_id: String,
    pub owner_username: Option<String>,
    pub project_id: String,
    pub project_name: String,
    pub role: String,
    pub scope: String,
    pub artifact_path: Option<String>,
    pub expires_at: Option<i64>,
    pub added_at: i64,
}

type MemberRow = (
    String,         // member_principal_id
    String,         // role
    String,         // scope_kind
    Option<String>, // artifact_path
    Option<i64>,    // expires_at
    i64,            // added_at
    Option<i64>,    // last_active_at
    Option<i64>,    // weekly_spend_cap_cents
    Option<String>, // username
);

const MEMBER_SELECT: &str = "SELECT m.member_principal_id, m.role, m.scope_kind, m.artifact_path, \
     m.expires_at, m.added_at, m.last_active_at, m.weekly_spend_cap_cents, \
     (SELECT username FROM accounts a WHERE a.principal_id = m.member_principal_id) \
     FROM project_members m";

fn member_view(r: MemberRow) -> MemberView {
    MemberView {
        principal_id: r.0,
        role: r.1,
        scope: r.2,
        artifact_path: r.3,
        expires_at: r.4,
        added_at: r.5,
        last_active_at: r.6,
        weekly_spend_cap_cents: r.7,
        username: r.8,
    }
}

/// The username of `principal` (T1 `accounts`); `None` if unknown.
pub(crate) async fn username_of(conn: &mut SqliteConnection, principal: &str) -> Option<String> {
    sqlx::query_scalar("SELECT username FROM accounts WHERE principal_id = ?")
        .bind(principal)
        .fetch_optional(&mut *conn)
        .await
        .ok()
        .flatten()
}

async fn active_member(
    conn: &mut SqliteConnection,
    project_key: &str,
    member: &str,
) -> Result<Option<MemberView>, AccessError> {
    let row: Option<MemberRow> = sqlx::query_as(&format!(
        "{MEMBER_SELECT} WHERE m.project_key = ? AND m.member_principal_id = ? \
         AND m.removed_at IS NULL"
    ))
    .bind(project_key)
    .bind(member)
    .fetch_optional(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    Ok(row.map(member_view))
}

fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

/// `access_members_list`, `access_member_set_role`, `access_member_remove`,
/// `access_member_restore`, `access_shares_list` (§9.1).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    if env.tier == StoreTier::T0 {
        return Err(requires_t1());
    }
    let calls = broker_host().map(|h| &*h.calls);
    serve(store(env)?, ctx, &env.principal.username, calls, cmd, args).await
}

/// The T1 bodies, testable without a broker.
pub async fn serve(
    store: &AccessStore,
    ctx: &AccessCtx,
    username: &str,
    calls: Option<&dyn OwnerCalls>,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    if cmd == "access_shares_list" {
        return shares_list(store, ctx).await;
    }
    let (project_key, _project_id, role) = project_scope(ctx, args)?;
    let principal_arg = || -> Result<String, AccessError> {
        let p = args
            .get("principalId")
            .and_then(Value::as_str)
            .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`principalId` is required"))?;
        p.parse::<PrincipalId>()
            .map(|p| p.to_string())
            .map_err(|_| AccessError::new(Code::NotFound, "no such member"))
    };
    match cmd {
        "access_members_list" => {
            if !matches!(role, Role::Owner | Role::Operator) {
                return Err(AccessError::new(
                    Code::Forbidden,
                    "only the Owner and Operators see the members list",
                ));
            }
            members_list(store, ctx, username, &project_key).await
        }
        "access_member_set_role" => {
            require_owner_admin(ctx, role)?;
            let member = principal_arg()?;
            let to = args
                .get("role")
                .and_then(Value::as_str)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`role` is required"))?;
            let to = Role::parse(to)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "unknown role"))?;
            let artifact = args.get("artifactPath").and_then(Value::as_str);
            let expires = args.get("expiresAt").and_then(Value::as_i64);
            let v = set_role(store, ctx, &project_key, &member, to, artifact, expires).await?;
            close(calls, &member);
            serde_json::to_value(v).map_err(AccessError::internal)
        }
        "access_member_remove" => {
            require_owner_admin(ctx, role)?;
            let member = principal_arg()?;
            remove(store, ctx, &project_key, &member).await?;
            close(calls, &member);
            Ok(json!({}))
        }
        "access_member_restore" => {
            if role != Role::Owner {
                return Err(AccessError::new(
                    Code::Forbidden,
                    "only the project's Owner can do this",
                ));
            }
            let member = principal_arg()?;
            let v = restore(store, ctx, &project_key, &member, now_ms()).await?;
            serde_json::to_value(v).map_err(AccessError::internal)
        }
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}

fn close(calls: Option<&dyn OwnerCalls>, member: &str) {
    if let (Some(c), Ok(id)) = (calls, member.parse::<PrincipalId>()) {
        c.close_principal(id);
    }
}

/// `access_members_list {projectId}` → `{owner, members, invites, counts}`.
pub async fn members_list(
    store: &AccessStore,
    ctx: &AccessCtx,
    username: &str,
    project_key: &str,
) -> Result<Value, AccessError> {
    let now = now_ms();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let owner = project_key
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let owner_name = if owner == ctx.principal_id.to_string() {
        Some(username.to_string())
    } else {
        username_of(&mut conn, &owner).await
    };
    let rows: Vec<MemberRow> = sqlx::query_as(&format!(
        "{MEMBER_SELECT} WHERE m.project_key = ? AND m.removed_at IS NULL \
         AND (m.expires_at IS NULL OR m.expires_at > ?) ORDER BY m.added_at"
    ))
    .bind(project_key)
    .bind(now)
    .fetch_all(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    let members: Vec<MemberView> = rows.into_iter().map(member_view).collect();
    let invites = pending_invites(&mut conn, project_key, now).await?;
    let pending = invites.iter().filter(|i| i.state == "pending").count();
    let name: Option<String> =
        sqlx::query_scalar("SELECT display_name FROM shared_projects WHERE project_key = ?")
            .bind(project_key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(AccessError::internal)?;
    Ok(json!({
        "projectKey": project_key,
        "projectName": name,
        "owner": { "principalId": owner, "username": owner_name },
        "counts": { "members": members.len(), "pendingInvites": pending },
        "members": members,
        "invites": invites,
    }))
}

/// Unaccepted, unrevoked invites of `project_key`, newest first.
pub(crate) async fn pending_invites(
    conn: &mut SqliteConnection,
    project_key: &str,
    now: i64,
) -> Result<Vec<InviteView>, AccessError> {
    type Row = (
        String,
        Option<String>,
        String,
        String,
        String,
        Option<String>,
        i64,
        String,
        i64,
        Option<i64>,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT invite_id, invitee_label, mode, role, scope_kind, artifact_path, issued_at, \
           issued_by, expires_at, member_expires_at, allow_new_account \
         FROM invites WHERE project_key = ? AND accepted_at IS NULL AND revoked_at IS NULL \
         ORDER BY issued_at DESC",
    )
    .bind(project_key)
    .fetch_all(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    Ok(rows
        .into_iter()
        .map(|r| InviteView {
            invite_id: r.0,
            label: r.1,
            mode: r.2,
            role: r.3,
            scope: r.4,
            artifact_path: r.5,
            issued_at: r.6,
            issued_by: r.7,
            expires_at: r.8,
            member_expires_at: r.9,
            allow_new_account: r.10 != 0,
            state: if r.8 <= now { "expired" } else { "pending" }.into(),
        })
        .collect())
}

/// `access_shares_list {}` → the projects shared with the caller.
pub async fn shares_list(store: &AccessStore, ctx: &AccessCtx) -> Result<Value, AccessError> {
    type Row = (
        String,
        String,
        Option<String>,
        Option<i64>,
        i64,
        Option<String>,
        Option<String>,
    );
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT m.project_key, m.role, m.artifact_path, m.expires_at, m.added_at, \
           s.display_name, \
           (SELECT username FROM accounts a WHERE a.principal_id = s.owner_principal_id) \
         FROM project_members m LEFT JOIN shared_projects s ON s.project_key = m.project_key \
         WHERE m.member_principal_id = ? AND m.removed_at IS NULL \
           AND (m.expires_at IS NULL OR m.expires_at > ?) \
         ORDER BY m.added_at DESC",
    )
    .bind(ctx.principal_id.to_string())
    .bind(now_ms())
    .fetch_all(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    let views: Vec<ShareView> = rows
        .into_iter()
        .filter_map(|r| {
            let (owner, project) = r.0.split_once('/')?;
            Some(ShareView {
                owner_principal_id: owner.to_string(),
                project_id: project.to_string(),
                project_name: r.5.unwrap_or_else(|| project.to_string()),
                owner_username: r.6,
                role: r.1,
                scope: if r.2.is_some() { "artifact" } else { "project" }.into(),
                artifact_path: r.2,
                expires_at: r.3,
                added_at: r.4,
                project_key: r.0.clone(),
            })
        })
        .collect();
    serde_json::to_value(views).map_err(AccessError::internal)
}

/// The scope a role change lands on (§4.2): to Guest needs an artifact and
/// an expiry; away from Guest resets to project scope and clears the
/// expiry, as D-05's role menu does; otherwise the scope stays, an
/// `artifactPath` narrows it, and `expiresAt` sets the expiry.
pub fn next_scope(
    from: &MemberView,
    to: Role,
    artifact: Option<&str>,
    expires: Option<i64>,
    now: i64,
) -> Result<(Option<String>, Option<i64>), AccessError> {
    let bad = |m: &str| AccessError::new(Code::InvalidRequest, m.to_string());
    if let Some(e) = expires {
        if e <= now {
            return Err(bad("expiresAt must be in the future"));
        }
    }
    let artifact = artifact.map(normalize_artifact_path).transpose()?;
    match to {
        Role::Owner => Err(AccessError::new(
            Code::InvalidRequest,
            "a project keeps one Owner; nothing assigns the owner role",
        )),
        Role::Guest => {
            let artifact = artifact
                .or_else(|| from.artifact_path.clone())
                .ok_or_else(|| bad("a Guest needs one artifact (artifactPath)"))?;
            let expires = expires
                .or(from.expires_at)
                .ok_or_else(|| bad("a Guest's access must expire (expiresAt)"))?;
            Ok((Some(artifact), Some(expires)))
        }
        _ if from.role == "guest" => Ok((None, None)),
        _ => Ok((
            artifact.or_else(|| from.artifact_path.clone()),
            expires.or(from.expires_at),
        )),
    }
}

/// `access_member_set_role` (§4.2, §4.3): audited `member.role_changed`.
pub async fn set_role(
    store: &AccessStore,
    ctx: &AccessCtx,
    project_key: &str,
    member: &str,
    to: Role,
    artifact: Option<&str>,
    expires: Option<i64>,
) -> Result<MemberView, AccessError> {
    let now = now_ms();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let from = active_member(&mut tx, project_key, member)
        .await?
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such member"))?;
    let (artifact, expires) = next_scope(&from, to, artifact, expires, now)?;
    sqlx::query(
        "UPDATE project_members SET role = ?, scope_kind = ?, artifact_path = ?, expires_at = ? \
         WHERE project_key = ? AND member_principal_id = ? AND removed_at IS NULL",
    )
    .bind(to.as_str())
    .bind(if artifact.is_some() {
        "artifact"
    } else {
        "project"
    })
    .bind(&artifact)
    .bind(expires)
    .bind(project_key)
    .bind(member)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let who = from.username.clone().unwrap_or_else(|| member.to_string());
    let mut ev = Event::by("member.role_changed", ctx)
        .subject_principal(member)
        .target(format!("{who} → {}", title(to)))
        .detail(json!({ "from": from.role, "to": to.as_str() }));
    ev.project_key = Some(project_key.to_string());
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    let after = active_member(&mut tx, project_key, member)
        .await?
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such member"))?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(after)
}

fn title(r: Role) -> &'static str {
    match r {
        Role::Owner => "Owner",
        Role::Operator => "Operator",
        Role::Reviewer => "Reviewer",
        Role::Guest => "Guest",
    }
}

/// `access_member_remove`: audited `member.removed`.
pub async fn remove(
    store: &AccessStore,
    ctx: &AccessCtx,
    project_key: &str,
    member: &str,
) -> Result<(), AccessError> {
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let from = active_member(&mut tx, project_key, member)
        .await?
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such member"))?;
    sqlx::query(
        "UPDATE project_members SET removed_at = ?, removed_by = ?, removed_reason = 'removed' \
         WHERE project_key = ? AND member_principal_id = ? AND removed_at IS NULL",
    )
    .bind(now_ms())
    .bind(ctx.principal_id.to_string())
    .bind(project_key)
    .bind(member)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("member.removed", ctx)
        .subject_principal(member)
        .target(from.username.unwrap_or_else(|| member.to_string()))
        .detail(json!({ "role": from.role }));
    ev.project_key = Some(project_key.to_string());
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(())
}

/// `access_member_restore` (P-17): within [`RESTORE_WINDOW_MS`] of a
/// removal **by this Owner**, re-add the member with the prior role, scope
/// and expiry. Audited `member.restored`.
pub async fn restore(
    store: &AccessStore,
    ctx: &AccessCtx,
    project_key: &str,
    member: &str,
    now: i64,
) -> Result<MemberView, AccessError> {
    type Row = (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        Option<String>,
        i64,
    );
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    if active_member(&mut tx, project_key, member).await?.is_some() {
        return Err(AccessError::new(Code::Conflict, "already a member"));
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT role, scope_kind, artifact_path, expires_at, weekly_spend_cap_cents, invite_id, \
           removed_at FROM project_members \
         WHERE project_key = ? AND member_principal_id = ? AND removed_reason = 'removed' \
           AND removed_by = ? ORDER BY removed_at DESC LIMIT 1",
    )
    .bind(project_key)
    .bind(member)
    .bind(ctx.principal_id.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let Some((role, scope_kind, artifact, expires, cap, invite, removed_at)) = row else {
        return Err(AccessError::new(Code::NotFound, "nothing to restore"));
    };
    if now - removed_at > RESTORE_WINDOW_MS {
        return Err(AccessError::new(
            Code::Gone,
            "the undo window has passed — invite them again",
        ));
    }
    if expires.is_some_and(|e| e <= now) {
        return Err(AccessError::new(
            Code::Gone,
            "that access has expired since",
        ));
    }
    sqlx::query(
        "INSERT INTO project_members (project_key, member_principal_id, role, scope_kind, \
           artifact_path, expires_at, weekly_spend_cap_cents, invite_id, added_by, added_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(project_key)
    .bind(member)
    .bind(&role)
    .bind(&scope_kind)
    .bind(&artifact)
    .bind(expires)
    .bind(cap)
    .bind(&invite)
    .bind(ctx.principal_id.to_string())
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("member.restored", ctx)
        .subject_principal(member)
        .detail(json!({ "role": role }));
    ev.project_key = Some(project_key.to_string());
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    let after = active_member(&mut tx, project_key, member)
        .await?
        .ok_or_else(|| AccessError::new(Code::Internal, "restore lost the row"))?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(after)
}

/// How often the broker sweeps expired memberships (§4.2: their sockets
/// close within ≤2 s).
pub const SWEEP_EVERY: Duration = Duration::from_secs(2);

/// §4.2 / A-27: mark every membership whose `expires_at` passed as
/// `removed_reason = 'expired'`, audit `member.expired` (a system event;
/// recorded even on a degraded chain, like other expiries), and return the
/// members whose sockets the caller closes. Expired *requests* are refused
/// by `share::select_membership` already; this keeps the rows and sockets in
/// step.
pub async fn sweep_expired(store: &AccessStore, now: i64) -> Result<Vec<String>, AccessError> {
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;
    let due: Vec<(i64, String, String, String)> = sqlx::query_as(
        "SELECT id, project_key, member_principal_id, role FROM project_members \
         WHERE removed_at IS NULL AND expires_at IS NOT NULL AND expires_at <= ?",
    )
    .bind(now)
    .fetch_all(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    if due.is_empty() {
        return Ok(Vec::new());
    }
    let mut head = None;
    for (id, project_key, member, role) in &due {
        sqlx::query(
            "UPDATE project_members SET removed_at = ?, removed_reason = 'expired' WHERE id = ?",
        )
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AccessError::internal)?;
        let mut ev = Event::new("member.expired", AuditVia::System)
            .subject_principal(member.clone())
            .detail(json!({ "role": role }));
        ev.project_key = Some(project_key.clone());
        head = Some(
            store
                .chain()
                .append(&mut tx, &ev)
                .await
                .map_err(audit_err)?,
        );
    }
    tx.commit().await.map_err(AccessError::internal)?;
    if let Some(h) = head {
        store.chain().committed(h);
    }
    Ok(due.into_iter().map(|d| d.2).collect())
}

/// The broker's sweeper task: [`sweep_expired`] every [`SWEEP_EVERY`],
/// closing the swept members' sockets (4403).
pub fn spawn_expiry_sweeper(store: AccessStore, calls: std::sync::Arc<dyn OwnerCalls>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            match sweep_expired(&store, now_ms()).await {
                Ok(members) => {
                    for m in members {
                        if let Ok(id) = m.parse::<PrincipalId>() {
                            calls.close_principal(id);
                        }
                    }
                }
                Err(e) => tracing::warn!("membership expiry sweep: {e}"),
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::access::policy::tests::{session, t1_store};

    /// Insert an active membership directly (invite acceptance's insert).
    pub(crate) async fn add(
        store: &AccessStore,
        project_key: &str,
        member: &str,
        role: &str,
        artifact: Option<&str>,
        expires: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO project_members (project_key, member_principal_id, role, scope_kind, \
               artifact_path, expires_at, added_by, added_at) VALUES (?, ?, ?, ?, ?, ?, 'x', 1)",
        )
        .bind(project_key)
        .bind(member)
        .bind(role)
        .bind(if artifact.is_some() {
            "artifact"
        } else {
            "project"
        })
        .bind(artifact)
        .bind(expires)
        .execute(store.pool())
        .await
        .unwrap();
    }

    fn view(role: &str, artifact: Option<&str>, expires: Option<i64>) -> MemberView {
        MemberView {
            principal_id: "m".into(),
            username: None,
            role: role.into(),
            scope: if artifact.is_some() {
                "artifact"
            } else {
                "project"
            }
            .into(),
            artifact_path: artifact.map(str::to_string),
            expires_at: expires,
            added_at: 0,
            last_active_at: None,
            weekly_spend_cap_cents: None,
        }
    }

    /// §4.2 / A-27 / A-30: the role menu's scope rules; nothing assigns owner.
    #[test]
    fn role_changes_follow_the_scope_rules() {
        let now = 1_000;
        let e =
            next_scope(&view("reviewer", None, None), Role::Guest, None, None, now).unwrap_err();
        assert!(e.message.contains("artifact"));
        let e = next_scope(
            &view("reviewer", None, None),
            Role::Guest,
            Some("a.md"),
            None,
            now,
        )
        .unwrap_err();
        assert!(e.message.contains("expire"));
        assert_eq!(
            next_scope(
                &view("reviewer", None, None),
                Role::Guest,
                Some("./a/b.md"),
                Some(2_000),
                now
            )
            .unwrap(),
            (Some("a/b.md".into()), Some(2_000))
        );
        assert_eq!(
            next_scope(
                &view("guest", Some("a.md"), Some(2_000)),
                Role::Reviewer,
                None,
                None,
                now
            )
            .unwrap(),
            (None, None),
            "away from Guest resets scope and expiry"
        );
        assert!(next_scope(&view("guest", None, None), Role::Owner, None, None, now).is_err());
        assert!(next_scope(
            &view("reviewer", None, None),
            Role::Guest,
            Some("../x"),
            Some(2_000),
            now
        )
        .is_err());
        assert!(next_scope(
            &view("reviewer", None, None),
            Role::Reviewer,
            None,
            Some(10),
            now
        )
        .is_err());
    }

    #[tokio::test]
    async fn list_set_remove_restore_and_shares() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let member = PrincipalId::new_v7();
        let key = format!("{owner}/royalti-co");
        add(&store, &key, &member.to_string(), "reviewer", None, None).await;
        let ctx = session(owner);
        let args = |extra: Value| {
            let mut a = json!({ "projectId": "royalti-co", "principalId": member.to_string() });
            a.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            a
        };
        let list = serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_members_list",
            &args(json!({})),
        )
        .await
        .unwrap();
        assert_eq!(list["owner"]["username"], "ned");
        assert_eq!(list["members"][0]["role"], "reviewer");

        let v = serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_member_set_role",
            &args(json!({ "role": "operator" })),
        )
        .await
        .unwrap();
        assert_eq!(v["role"], "operator");
        let e = serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_member_set_role",
            &args(json!({ "role": "owner" })),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::InvalidRequest, "A-30");

        serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_member_remove",
            &args(json!({})),
        )
        .await
        .unwrap();
        let list = serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_members_list",
            &args(json!({})),
        )
        .await
        .unwrap();
        assert_eq!(list["members"].as_array().unwrap().len(), 0);
        let v = serve(
            &store,
            &ctx,
            "ned",
            None,
            "access_member_restore",
            &args(json!({})),
        )
        .await
        .unwrap();
        assert_eq!(v["role"], "operator", "the prior role comes back");
        let e = restore(&store, &ctx, &key, &member.to_string(), now_ms())
            .await
            .unwrap_err();
        assert_eq!(e.code, Code::Conflict);

        let shares = serve(
            &store,
            &session(member),
            "ada",
            None,
            "access_shares_list",
            &json!({}),
        )
        .await
        .unwrap();
        assert_eq!(shares[0]["projectKey"], key);
        assert_eq!(shares[0]["role"], "operator");

        let kinds: Vec<String> = sqlx::query_scalar(
            "SELECT kind FROM audit_events WHERE category = 'people' ORDER BY seq",
        )
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(
            kinds,
            vec!["member.role_changed", "member.removed", "member.restored"]
        );
    }

    #[tokio::test]
    async fn restore_is_bounded_by_the_undo_window() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let member = PrincipalId::new_v7().to_string();
        let key = format!("{owner}/p");
        add(&store, &key, &member, "reviewer", None, None).await;
        let ctx = session(owner);
        remove(&store, &ctx, &key, &member).await.unwrap();
        let e = restore(
            &store,
            &ctx,
            &key,
            &member,
            now_ms() + RESTORE_WINDOW_MS + 1_000,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Gone);
    }

    /// A Reviewer member can't list members; nobody but the Owner changes them.
    #[tokio::test]
    async fn only_owner_and_operators_list() {
        use crate::access::ctx::ShareCtx;
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let member = PrincipalId::new_v7();
        let mut ctx = session(member);
        let share = |role| ShareCtx {
            project_key: format!("{owner}/p"),
            project_id: "p".into(),
            member_principal_id: Some(member.to_string()),
            member_device_id: None,
            role: Some(role),
            artifact_path: None,
            owner_approval: true,
        };
        ctx.share = Some(share(Role::Reviewer));
        let e = serve(
            &store,
            &ctx,
            "ada",
            None,
            "access_members_list",
            &json!({"projectId": "p"}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
        ctx.share = Some(share(Role::Operator));
        serve(
            &store,
            &ctx,
            "ada",
            None,
            "access_members_list",
            &json!({"projectId": "p"}),
        )
        .await
        .unwrap();
        let e = serve(
            &store,
            &ctx,
            "ada",
            None,
            "access_member_remove",
            &json!({"projectId": "p", "principalId": PrincipalId::new_v7().to_string()}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
    }

    /// A-27: a Guest row needs artifact scope and an expiry (CHECK); the
    /// sweeper expires it and audits `member.expired`.
    #[tokio::test]
    async fn guests_expire() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let key = format!("{owner}/p");
        let raw = sqlx::query(
            "INSERT INTO project_members (project_key, member_principal_id, role, scope_kind, \
               added_by, added_at) VALUES (?, 'g', 'guest', 'project', 'x', 1)",
        )
        .bind(&key)
        .execute(store.pool())
        .await;
        assert!(
            raw.is_err(),
            "a project-scope Guest is refused by the CHECK"
        );
        let guest = PrincipalId::new_v7().to_string();
        add(
            &store,
            &key,
            &guest,
            "guest",
            Some("plans/board.html"),
            Some(5_000),
        )
        .await;
        assert!(sweep_expired(&store, 4_000).await.unwrap().is_empty());
        assert_eq!(
            sweep_expired(&store, 5_000).await.unwrap(),
            vec![guest.clone()]
        );
        let reason: String = sqlx::query_scalar(
            "SELECT removed_reason FROM project_members WHERE member_principal_id = ?",
        )
        .bind(&guest)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(reason, "expired");
        let kind: String =
            sqlx::query_scalar("SELECT kind FROM audit_events ORDER BY seq DESC LIMIT 1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(kind, "member.expired");
    }
}
