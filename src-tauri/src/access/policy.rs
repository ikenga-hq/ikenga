//! Project policies: the role × cap matrix and "Require Owner approval"
//! (G-ACCESS §4.1, §5.2; WP-76).
//!
//! * The matrix is §4.1's defaults (`Role::default_caps`) with the
//!   project's `project_role_caps` overrides applied. Owner is fixed (all
//!   seven, no cell editable); `secrets` is never grantable to a non-Owner —
//!   the API refuses it here and the table's `CHECK (cap <> 'secrets')`
//!   refuses a raw insert (A-4).
//! * `shared_projects.owner_approval_required` is per project, default on
//!   (§5.2). It only matters inside shares; WP-75's decide core enforces it.
//! * On T0 there is one principal, so the matrix is the defaults, read-only
//!   (§4.5.5): `access_policy_get` answers them; the setters `requires_t1`.
//!
//! Every write is an access change: it commits with its audit row in one
//! `BEGIN IMMEDIATE` (§6.3, A-18) and is refused while the chain is
//! degraded (§6.4). A change closes the project's members' sockets (4403),
//! so no socket runs on stale caps (§1.4).

use serde_json::{json, Map, Value};
use sqlx::{Connection, SqliteConnection};

use super::audit::Event;
use super::caps::{Cap, CapSet, Role, CAPS, ROLES};
use super::ctx::AccessCtx;
use super::rpc::Env;
use super::share::{broker_host, now_ms, OwnerCalls};
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code};

/// One matrix cell (§9.1 `access_policy_get`).
pub fn cell(role: Role, cap: Cap, row: CapSet) -> &'static str {
    if role.never().contains(cap) {
        "never"
    } else if row.contains(cap) {
        "allowed"
    } else {
        "withheld"
    }
}

/// `role`'s override-applied row in `project_key` (§4.1): the defaults,
/// then each override. `secrets` is never in a non-Owner row, whatever is
/// stored (§1.4's `role_caps` removes it again).
pub async fn effective_row(
    conn: &mut SqliteConnection,
    project_key: &str,
    role: Role,
) -> Result<CapSet, AccessError> {
    if role == Role::Owner {
        return Ok(CapSet::ALL);
    }
    let overrides: Vec<(String, i64)> = sqlx::query_as(
        "SELECT cap, allowed FROM project_role_caps WHERE project_key = ? AND role = ?",
    )
    .bind(project_key)
    .bind(role.as_str())
    .fetch_all(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    let mut row = role.default_caps();
    for (cap, allowed) in overrides {
        if let Some(cap) = Cap::parse(&cap) {
            row = if allowed != 0 {
                row.with(cap)
            } else {
                row.without(cap)
            };
        }
    }
    Ok(row.intersect(CapSet::of(&CAPS).without(Cap::Secrets)))
}

/// `shared_projects.owner_approval_required` (§5.2), default on.
pub async fn owner_approval_required(
    conn: &mut SqliteConnection,
    project_key: &str,
) -> Result<bool, AccessError> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT owner_approval_required FROM shared_projects WHERE project_key = ?",
    )
    .bind(project_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    Ok(v.map_or(true, |v| v != 0))
}

/// The full matrix for `project_key`.
pub async fn matrix(conn: &mut SqliteConnection, project_key: &str) -> Result<Value, AccessError> {
    let mut out = Map::new();
    for role in ROLES {
        let row = effective_row(conn, project_key, role).await?;
        out.insert(role.as_str().into(), row_json(role, row));
    }
    Ok(Value::Object(out))
}

fn row_json(role: Role, row: CapSet) -> Value {
    let mut cells = Map::new();
    for cap in CAPS {
        cells.insert(cap.as_str().into(), json!(cell(role, cap, row)));
    }
    Value::Object(cells)
}

/// §4.5.5: the default matrix (T0, or a project nobody has overridden).
pub fn default_matrix() -> Value {
    let mut out = Map::new();
    for role in ROLES {
        out.insert(role.as_str().into(), row_json(role, role.default_caps()));
    }
    Value::Object(out)
}

/// Whose project a request names (§9.1 "own project"): in your own
/// workspace, `<you>/<projectId>`, and you are its Owner; through a share,
/// the share's project (and `projectId` must name it), with the share's
/// role.
pub fn project_scope(ctx: &AccessCtx, args: &Value) -> Result<(String, String, Role), AccessError> {
    let project_id = args
        .get("projectId")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty() && !p.contains('/'))
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`projectId` is required"))?;
    match &ctx.share {
        None => Ok((
            format!("{}/{project_id}", ctx.principal_id),
            project_id.to_string(),
            Role::Owner,
        )),
        Some(share) if share.project_id == project_id => Ok((
            share.project_key.clone(),
            project_id.to_string(),
            share.role.unwrap_or(Role::Guest),
        )),
        Some(_) => Err(AccessError::new(Code::NotFound, "no such shared project")),
    }
}

pub(crate) fn audit_err(e: super::audit::chain::AppendError) -> AccessError {
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

pub(crate) fn require_owner_admin(ctx: &AccessCtx, role: Role) -> Result<(), AccessError> {
    if role != Role::Owner {
        return Err(AccessError::new(
            Code::Forbidden,
            "only the project's Owner can do this",
        ));
    }
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    Ok(())
}

fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

/// `access_policy_get`, `access_policy_set_cell`,
/// `access_policy_set_owner_approval` (§9.1).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    if env.tier == StoreTier::T0 {
        return match cmd {
            "access_policy_get" => Ok(json!({
                "matrix": default_matrix(),
                "ownerApprovalRequired": true,
            })),
            _ => Err(requires_t1()),
        };
    }
    let calls = broker_host().map(|h| &*h.calls);
    serve(store(env)?, ctx, calls, cmd, args).await
}

pub(crate) fn requires_t1() -> AccessError {
    AccessError::new(
        Code::RequiresT1,
        "Sharing needs an Ikenga server with accounts (T1)",
    )
}

/// The T1 bodies (the broker's access store), testable without a broker.
pub async fn serve(
    store: &AccessStore,
    ctx: &AccessCtx,
    calls: Option<&dyn OwnerCalls>,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    let (project_key, project_id, role) = project_scope(ctx, args)?;
    match cmd {
        "access_policy_get" => {
            let mut conn = store
                .pool()
                .acquire()
                .await
                .map_err(AccessError::internal)?;
            Ok(json!({
                "matrix": matrix(&mut conn, &project_key).await?,
                "ownerApprovalRequired": owner_approval_required(&mut conn, &project_key).await?,
            }))
        }
        "access_policy_set_cell" => {
            require_owner_admin(ctx, role)?;
            let target = args
                .get("role")
                .and_then(Value::as_str)
                .and_then(Role::parse)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`role` is required"))?;
            let cap = args
                .get("cap")
                .and_then(Value::as_str)
                .and_then(Cap::parse)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`cap` is required"))?;
            let allowed = args
                .get("allowed")
                .and_then(Value::as_bool)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`allowed` is required"))?;
            let m = set_cell(store, ctx, &project_key, target, cap, allowed).await?;
            close_members(store, calls, &project_key).await;
            Ok(m)
        }
        "access_policy_set_owner_approval" => {
            require_owner_admin(ctx, role)?;
            let required = args
                .get("required")
                .and_then(Value::as_bool)
                .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`required` is required"))?;
            let name = match calls {
                Some(c) => {
                    super::share::owner_project_info(c, ctx.principal_id, &project_id)
                        .await?
                        .name
                }
                None => project_id.clone(),
            };
            set_owner_approval(store, ctx, &project_key, &project_id, &name, required).await?;
            close_members(store, calls, &project_key).await;
            Ok(json!({}))
        }
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no access command `{other}`"),
        )),
    }
}

/// §4.1 / A-4: `role ≠ owner`, `cap ≠ secrets`; audited `policy.changed`.
pub async fn set_cell(
    store: &AccessStore,
    ctx: &AccessCtx,
    project_key: &str,
    role: Role,
    cap: Cap,
    allowed: bool,
) -> Result<Value, AccessError> {
    if role == Role::Owner {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "the Owner's row is fixed: all seven",
        ));
    }
    if cap == Cap::Secrets {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "secrets is never grantable to a non-Owner — the vault stays on the host",
        ));
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
    sqlx::query(
        "INSERT INTO project_role_caps (project_key, role, cap, allowed, updated_at, updated_by) \
         VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (project_key, role, cap) DO UPDATE SET \
           allowed = excluded.allowed, updated_at = excluded.updated_at, \
           updated_by = excluded.updated_by",
    )
    .bind(project_key)
    .bind(role.as_str())
    .bind(cap.as_str())
    .bind(allowed as i64)
    .bind(now_ms())
    .bind(ctx.principal_id.to_string())
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("policy.changed", ctx)
        .target(format!(
            "{} · {} → {}",
            role.as_str(),
            cap.as_str(),
            if allowed { "allowed" } else { "withheld" }
        ))
        .detail(json!({ "role": role.as_str(), "cap": cap.as_str(), "allowed": allowed }));
    ev.project_key = Some(project_key.to_string());
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    let m = matrix(&mut tx, project_key).await?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);
    Ok(m)
}

/// Upsert `shared_projects` (display name from `share_project_info`).
pub(crate) async fn upsert_shared_project(
    conn: &mut SqliteConnection,
    project_key: &str,
    project_id: &str,
    owner: &str,
    name: &str,
) -> Result<(), AccessError> {
    let now = now_ms();
    let name: String = name.chars().take(200).collect();
    sqlx::query(
        "INSERT INTO shared_projects (project_key, owner_principal_id, project_id, display_name, \
           created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT (project_key) DO UPDATE SET display_name = excluded.display_name, \
           updated_at = excluded.updated_at",
    )
    .bind(project_key)
    .bind(owner)
    .bind(project_id)
    .bind(if name.is_empty() {
        project_id
    } else {
        name.as_str()
    })
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    Ok(())
}

/// §5.2: audited `policy.owner_approval_changed`.
pub async fn set_owner_approval(
    store: &AccessStore,
    ctx: &AccessCtx,
    project_key: &str,
    project_id: &str,
    name: &str,
    required: bool,
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
    upsert_shared_project(
        &mut tx,
        project_key,
        project_id,
        &ctx.principal_id.to_string(),
        name,
    )
    .await?;
    sqlx::query(
        "UPDATE shared_projects SET owner_approval_required = ?, updated_at = ? \
         WHERE project_key = ?",
    )
    .bind(required as i64)
    .bind(now_ms())
    .bind(project_key)
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut ev = Event::by("policy.owner_approval_changed", ctx)
        .target(if required {
            "Require Owner approval → on"
        } else {
            "Require Owner approval → off"
        })
        .detail(json!({ "required": required }));
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

/// Close every active member's sockets of `project_key` (4403): their caps
/// were computed against the old matrix / policy (§1.4).
pub(crate) async fn close_members(
    store: &AccessStore,
    calls: Option<&dyn OwnerCalls>,
    project_key: &str,
) {
    let Some(calls) = calls else { return };
    let members: Vec<String> = match sqlx::query_scalar(
        "SELECT member_principal_id FROM project_members \
         WHERE project_key = ? AND removed_at IS NULL",
    )
    .bind(project_key)
    .fetch_all(store.pool())
    .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("closing member sockets of {project_key}: {e}");
            return;
        }
    };
    for m in members {
        if let Ok(id) = m.parse() {
            calls.close_principal(id);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::access::caps::Tier;
    use crate::access::ctx::{RequestMeta, ShareCtx, Via};
    use crate::executor::PrincipalId;

    pub(crate) async fn t1_store() -> AccessStore {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        // The T1 store sits beside WP-20's `accounts` (§2.5); these tests
        // need only its two columns the access layer reads.
        sqlx::query(
            "CREATE TABLE accounts (principal_id TEXT PRIMARY KEY, username TEXT NOT NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
        AccessStore::attach_t1(pool).await.unwrap()
    }

    pub(crate) fn session(principal: PrincipalId) -> AccessCtx {
        AccessCtx {
            principal_id: principal,
            via: Via::Session {
                session_id: "s".into(),
            },
            device_id: None,
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::ALL,
            admin_strength: true,
            meta: RequestMeta::default(),
        }
    }

    #[test]
    fn the_default_matrix_is_d05_with_secrets_never() {
        let m = default_matrix();
        assert_eq!(m["owner"]["secrets"], "allowed");
        for role in ["operator", "reviewer", "guest"] {
            assert_eq!(m[role]["secrets"], "never", "{role}");
        }
        assert_eq!(m["operator"]["approve"], "allowed");
        assert_eq!(m["operator"]["install"], "withheld");
        assert_eq!(m["reviewer"]["dispatch"], "withheld");
        assert_eq!(
            m["guest"]["files"], "withheld",
            "the cell means project-wide files"
        );
    }

    /// A-4: secrets is refused by the API and by the CHECK; the Owner row is
    /// fixed; an override applies and is audited.
    #[tokio::test]
    async fn cells_apply_and_secrets_is_never_grantable() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let ctx = session(owner);
        let key = format!("{owner}/royalti-co");
        let args = |role: &str, cap: &str, allowed: bool| json!({ "projectId": "royalti-co", "role": role, "cap": cap, "allowed": allowed });
        let e = serve(
            &store,
            &ctx,
            None,
            "access_policy_set_cell",
            &args("operator", "secrets", true),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::InvalidRequest);
        let e = serve(
            &store,
            &ctx,
            None,
            "access_policy_set_cell",
            &args("owner", "files", false),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::InvalidRequest);
        let raw = sqlx::query(
            "INSERT INTO project_role_caps VALUES (?, 'operator', 'secrets', 1, 0, 'x')",
        )
        .bind(&key)
        .execute(store.pool())
        .await;
        assert!(raw.is_err(), "the CHECK refuses a raw secrets row");

        let m = serve(
            &store,
            &ctx,
            None,
            "access_policy_set_cell",
            &args("reviewer", "dispatch", true),
        )
        .await
        .unwrap();
        assert_eq!(m["reviewer"]["dispatch"], "allowed");
        let mut conn = store.pool().acquire().await.unwrap();
        let row = effective_row(&mut conn, &key, Role::Reviewer)
            .await
            .unwrap();
        assert!(row.contains(Cap::Dispatch) && !row.contains(Cap::Secrets));
        let kinds: Vec<String> =
            sqlx::query_scalar("SELECT kind FROM audit_events WHERE kind LIKE 'policy.%'")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(kinds, vec!["policy.changed"]);
    }

    /// Only the Owner (with admin strength) sets cells; a member reads.
    #[tokio::test]
    async fn members_read_and_only_the_owner_writes() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        let member = PrincipalId::new_v7();
        let mut ctx = session(member);
        ctx.share = Some(ShareCtx {
            project_key: format!("{owner}/p"),
            project_id: "p".into(),
            member_principal_id: Some(member.to_string()),
            member_device_id: None,
            role: Some(Role::Operator),
            artifact_path: None,
            owner_approval: true,
        });
        let get = serve(
            &store,
            &ctx,
            None,
            "access_policy_get",
            &json!({"projectId": "p"}),
        )
        .await
        .unwrap();
        assert_eq!(get["ownerApprovalRequired"], true);
        let e = serve(
            &store,
            &ctx,
            None,
            "access_policy_set_owner_approval",
            &json!({"projectId": "p", "required": false}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
        let e = serve(
            &store,
            &ctx,
            None,
            "access_policy_get",
            &json!({"projectId": "q"}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::NotFound, "another project through this share");

        let mut weak = session(owner);
        weak.admin_strength = false;
        let e = serve(
            &store,
            &weak,
            None,
            "access_policy_set_owner_approval",
            &json!({"projectId": "p", "required": false}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, Code::Forbidden, "admin_strength (P-26)");
        serve(
            &store,
            &session(owner),
            None,
            "access_policy_set_owner_approval",
            &json!({"projectId": "p", "required": false}),
        )
        .await
        .unwrap();
        let mut conn = store.pool().acquire().await.unwrap();
        assert!(!owner_approval_required(&mut conn, &format!("{owner}/p"))
            .await
            .unwrap());
    }

    /// A-18: an injected audit failure rolls the change back.
    #[tokio::test]
    async fn an_audit_failure_rolls_the_cell_back() {
        let store = t1_store().await;
        let owner = PrincipalId::new_v7();
        sqlx::query(
            "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_events \
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let key = format!("{owner}/p");
        let e = set_cell(&store, &session(owner), &key, Role::Guest, Cap::Files, true)
            .await
            .unwrap_err();
        assert_eq!(e.code, Code::Internal);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_role_caps")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }
}
