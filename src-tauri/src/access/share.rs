//! Share confinement (G-ACCESS §4.5, WP-76): the **broker's** share
//! selection and the **child's** confinement of a share request.
//!
//! Broker side (T1, Linux):
//!
//! * [`broker_select`] — §4.5.2: the membership a request selects
//!   (`X-Ikenga-Share` / `?share=`), its role context and ceiling for §1.4,
//!   and the Owner whose child the request is routed into. A selection that
//!   names no active membership is `not_found` (no existence oracle); an
//!   expired one is `403` (`expired: …`). `access::t1` calls it once per
//!   request / WS handshake and turns the result into the
//!   `X-Ikenga-Share-*` headers ([`to_child_headers`]) and the proxy target.
//! * [`OwnerCalls`] / [`install_broker_host`] — the broker's handle on the
//!   Owner's child (the `internal` arms `share_project_info` and
//!   `notifications_record_access`, sent with
//!   [`super::INTERNAL_CALL_HEADER`]) and on its socket registry, installed
//!   once at broker boot (`server::broker::proxy::ShareChildCalls`).
//!
//! Child side (every daemon; only a T1 principal child ever sees a share):
//!
//! * [`prehook`] — the `rpc_handler` pre-hook for a share request: resolve
//!   the shared project's root from this child's own `ikenga.db`, then serve
//!   the arm on a cloned `AppState` whose `path_guard` is
//!   [narrowed](crate::server::AppState) to the project root (or the one
//!   artifact) and whose actions manager is rebuilt on that guard
//!   ([`actions_dispatch`]'s per-request manager, review M-7) — or refuse /
//!   answer it here (personal settings scope, a foreign comment, a
//!   transcript whose `cwd` is outside the share);
//! * [`filter`] — the post-hook: list filtering to the shared project and,
//!   for a Reviewer, cost stripping (§4.5.4, P-36);
//! * [`chat_cwd`], [`fs_watch_root`] — the `/ws/chat` `Prompt { cwd }` and
//!   `/ws/fs` watch-root confinement (A-36), from the root cache the
//!   pre-hook and [`internal`] keep;
//! * [`run_env`] — no vault env in share-originated runs (§4.5.1, N-10);
//! * [`internal`] — the two `internal` arms, served with the child's
//!   `AppState` (`share_project_info`, `notifications_record_access`).
//!
//! [`from_child_headers`] / [`to_child_headers`] (WP-74a) are the two ends
//! of the broker → child narrowing headers (§4.5.3); the child reads them
//! only on per-child-token requests.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use axum::http::HeaderMap;
use serde_json::{json, Value};

use super::caps::Role;
#[cfg(target_os = "linux")]
use super::caps::{CapSet, RoleContext};
use super::ctx::{AccessCtx, ShareCtx};
use super::{AccessError, AccessOptions, Code, PreHook};
use crate::executor::PrincipalId;
use crate::server::rpc::RpcResponse;

/// Whether `headers` carry any `X-Ikenga-Share-*` header (§4.5.3: an
/// `internal` arm is accepted only when none is present).
pub fn any_share_header(headers: &HeaderMap) -> bool {
    headers
        .keys()
        .any(|k| k.as_str().starts_with("x-ikenga-share-"))
}

/// `X-Ikenga-Share-*` → [`ShareCtx`] (§4.5.3). `None` when the request
/// carries no `X-Ikenga-Share-Project`. Narrowing only — never a grant.
pub fn from_child_headers(headers: &HeaderMap) -> Option<ShareCtx> {
    let h = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let project_id = h("x-ikenga-share-project")?;
    let member = h("x-ikenga-share-principal");
    let device = h("x-ikenga-share-device").filter(|d| d != "-");
    Some(ShareCtx {
        project_key: format!(
            "{}/{}",
            h("x-ikenga-principal").unwrap_or_default(),
            project_id
        ),
        project_id,
        member_principal_id: member,
        member_device_id: device,
        role: h("x-ikenga-share-role").and_then(|r| Role::parse(&r)),
        artifact_path: h("x-ikenga-share-artifact"),
        owner_approval: h("x-ikenga-share-policy").as_deref() == Some("owner-approval"),
    })
}

/// [`ShareCtx`] → the `X-Ikenga-Share-*` headers the broker sets (§4.5.3);
/// the inverse of [`from_child_headers`]. `X-Ikenga-Principal` (the Owner,
/// the proxy target) is set by the proxy itself.
pub fn to_child_headers(share: &ShareCtx) -> Vec<(&'static str, String)> {
    let mut out = vec![("x-ikenga-share-project", share.project_id.clone())];
    if let Some(m) = &share.member_principal_id {
        out.push(("x-ikenga-share-principal", m.clone()));
    }
    out.push((
        "x-ikenga-share-device",
        share.member_device_id.clone().unwrap_or_else(|| "-".into()),
    ));
    if let Some(r) = share.role {
        out.push(("x-ikenga-share-role", r.as_str().to_string()));
    }
    if let Some(a) = &share.artifact_path {
        out.push(("x-ikenga-share-artifact", a.clone()));
    }
    if share.owner_approval {
        out.push(("x-ikenga-share-policy", "owner-approval".into()));
    }
    out
}

/// The share a client request selects (§4.5.2): `X-Ikenga-Share:
/// <owner>/<project>` on HTTP, `?share=<owner>/<project>` on a WebSocket
/// (it only selects; the credential authenticates). `None` = own workspace.
pub fn selected(parts: &axum::http::request::Parts) -> Option<String> {
    if let Some(v) = parts
        .headers
        .get("x-ikenga-share")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return Some(v.to_string());
    }
    parts.uri.query().and_then(|q| {
        q.split('&').find_map(|p| {
            let (k, v) = p.split_once('=')?;
            let k = percent_encoding::percent_decode_str(k).decode_utf8_lossy();
            (k == "share" && !v.is_empty()).then(|| {
                percent_encoding::percent_decode_str(v)
                    .decode_utf8_lossy()
                    .into_owned()
            })
        })
    })
}

// ─── shared helpers ──────────────────────────────────────────────────────────

/// A boxed `Send` future (the [`OwnerCalls`] trait is object-safe).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Unix ms now.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// §4.2: an artifact path is relative to the project root and normalized —
/// no `..`, no `.`, no absolute path, no backslash, no NUL, no empty
/// segment. Returns the normalized `a/b/c` spelling. The child still
/// confirms it at use (canonicalized under the root, so a symlink can't
/// escape: [`share_target`]).
pub fn normalize_artifact_path(raw: &str) -> Result<String, AccessError> {
    let bad = |why: &str| AccessError::new(Code::InvalidRequest, format!("artifactPath {why}"));
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(bad("is required for artifact scope"));
    }
    if raw.len() > 1024 {
        return Err(bad("is too long"));
    }
    if raw.contains('\\') || raw.contains('\0') {
        return Err(bad("may not contain a backslash or NUL"));
    }
    let path = Path::new(raw);
    if path.is_absolute() || raw.starts_with('/') {
        return Err(bad("must be relative to the project root"));
    }
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => {
                parts.push(s.to_str().ok_or_else(|| bad("must be UTF-8"))?.to_string())
            }
            Component::CurDir => {}
            _ => return Err(bad("may not contain `..`")),
        }
    }
    if parts.is_empty() {
        return Err(bad("names no file"));
    }
    Ok(parts.join("/"))
}

/// `<owner_principal_id>/<project_id>` → its two halves, validated: a
/// lowercase UUID and a non-empty project slug with no `/`.
pub fn split_project_key(key: &str) -> Option<(PrincipalId, String)> {
    let (owner, project) = key.split_once('/')?;
    if project.is_empty() || project.contains('/') || owner != owner.to_ascii_lowercase() {
        return None;
    }
    Some((owner.parse().ok()?, project.to_string()))
}

/// Whether `path` lies under (or is) `base`, comparing canonical forms when
/// they resolve and the lexical form otherwise. Used to filter rows a child
/// reports (session cwds, comment paths), never to admit a write — writes
/// go through the narrowed `PathGuard`.
pub fn is_under(path: &str, base: &Path) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = Path::new(path);
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    p.starts_with(base)
}

// ─── the broker's handle on owner children ───────────────────────────────────

/// What the broker does on a principal's child on the access layer's behalf
/// (installed once at broker boot; `server::broker::proxy::ShareChildCalls`).
pub trait OwnerCalls: Send + Sync {
    /// Call an `internal` arm on `owner`'s child (spawning it lazily): a
    /// broker-originated request with [`super::INTERNAL_CALL_HEADER`], no
    /// caps header and no share header (§4.5.3). The child's `error` string
    /// (`<code>: <message>`) comes back as an [`AccessError`].
    fn call<'a>(
        &'a self,
        owner: PrincipalId,
        cmd: &'a str,
        args: Value,
    ) -> BoxFuture<'a, Result<Value, AccessError>>;

    /// Close every open socket of `principal` with 4403 (caps changed): a
    /// membership's role, scope, expiry or project policy moved, so the
    /// member reconnects and the broker recomputes its caps (§1.4).
    fn close_principal(&self, principal: PrincipalId) -> usize;
}

/// The broker's access-layer handles beyond `Env` (§4.5, §7): the Part B
/// flags and the [`OwnerCalls`].
pub struct BrokerHost {
    pub options: AccessOptions,
    pub calls: Arc<dyn OwnerCalls>,
}

static BROKER_HOST: OnceLock<BrokerHost> = OnceLock::new();

/// Install the broker host (once, at broker boot). A second call is ignored.
pub fn install_broker_host(host: BrokerHost) {
    let _ = BROKER_HOST.set(host);
}

/// The installed broker host, `None` outside a T1 broker.
pub fn broker_host() -> Option<&'static BrokerHost> {
    BROKER_HOST.get()
}

/// Broker boot (T1, WP-76): install the [`BrokerHost`], start the
/// membership expiry sweeper (§4.2: an expired member's sockets close
/// within ≤2 s), and build the invite-accept host (§7.3) the public
/// `/access/invite/*` routes serve against.
#[cfg(target_os = "linux")]
pub fn install_t1(
    t1: &Arc<super::t1::T1Access>,
    calls: Arc<dyn OwnerCalls>,
    provisioner: crate::server::operator::provision::Provisioner,
) -> Arc<super::invites::InviteHost> {
    install_broker_host(BrokerHost {
        options: t1.options.clone(),
        calls: calls.clone(),
    });
    super::members::spawn_expiry_sweeper(t1.store.clone(), calls.clone());
    Arc::new(super::invites::InviteHost {
        store: t1.store.clone(),
        options: t1.options.clone(),
        provisioner,
        calls: Some(calls),
        throttle: Default::default(),
    })
}

/// `share_project_info` on the Owner's child, decoded (§4.5.4): the
/// broker's validation of a `projectId` and its display-name cache.
pub async fn owner_project_info(
    calls: &dyn OwnerCalls,
    owner: PrincipalId,
    project_id: &str,
) -> Result<ProjectInfo, AccessError> {
    let v = calls
        .call(
            owner,
            "share_project_info",
            json!({ "projectId": project_id }),
        )
        .await?;
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(project_id)
        .to_string();
    let root = v
        .get("root")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(ProjectInfo { name, root })
}

/// `share_project_info`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectInfo {
    pub name: String,
    pub root: String,
}

// ─── broker: share selection (§4.5.2) ────────────────────────────────────────

/// A share the broker selected for one request (§4.5.2 steps 2–3).
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct BrokerShare {
    /// What the child is told (§4.5.3) — and the broker's `AccessCtx.share`.
    pub share: ShareCtx,
    /// `role_caps` for §1.4: the membership's role, the project's
    /// override-applied row and the artifact grant.
    pub context: RoleContext,
    /// §1.4 `share_ceiling` (all seven unless the share narrows further).
    pub ceiling: CapSet,
    /// The project's Owner: the request is routed into **their** child.
    pub owner: crate::executor::Principal,
}

/// How often the broker writes a member's `last_active_at` (≤1/min, §8.2).
pub const LAST_ACTIVE_EVERY_MS: i64 = 60_000;

/// The broker's share selection (§4.5.2): resolve the membership the
/// request [selects](selected) for `ctx`'s principal, or `Ok(None)` for an
/// own-workspace request. Called once per request / WS handshake by
/// `access::t1::T1Access::access_ctx`.
///
/// * a malformed selector, an unknown project, no active membership, or an
///   Owner whose account is gone or disabled → `not_found` (the same answer
///   for each, so membership is no existence oracle);
/// * an expired membership → `forbidden` with an `expired:` message (§4.2:
///   "the membership is refused (`403 expired`)" — `Code::Expired` would be
///   a 401, which a browser reads as "sign in again");
/// * selecting your own project is `not_found`: your own workspace needs no
///   share.
#[cfg(target_os = "linux")]
pub async fn broker_select(
    t1: &super::t1::T1Access,
    ctx: &crate::server::auth::PrincipalCtx,
    parts: &axum::http::request::Parts,
) -> Result<Option<BrokerShare>, AccessError> {
    let Some(selector) = selected(parts) else {
        return Ok(None);
    };
    let share = select_membership(
        &t1.pool,
        &selector,
        ctx.principal.id,
        ctx.via.device_id(),
        now_ms(),
    )
    .await?;
    // Warm the Owner's child's root cache on a WebSocket handshake: the
    // child's `/ws/chat` / `/ws/fs` hooks are synchronous and confine from
    // that cache (best effort — the child fails closed without it).
    if parts.uri.path().starts_with("/ws/") {
        if let Some(host) = broker_host() {
            let _ = owner_project_info(&*host.calls, share.owner.id, &share.share.project_id).await;
        }
    }
    Ok(Some(share))
}

/// [`broker_select`]'s store half (testable without a request).
#[cfg(target_os = "linux")]
pub async fn select_membership(
    pool: &sqlx::SqlitePool,
    selector: &str,
    member: PrincipalId,
    member_device: Option<&str>,
    now: i64,
) -> Result<BrokerShare, AccessError> {
    use crate::server::operator::accounts;
    let not_found = || AccessError::new(Code::NotFound, "no such shared project");
    let (owner_id, project_id) = split_project_key(selector).ok_or_else(not_found)?;
    if owner_id == member {
        return Err(not_found());
    }
    let project_key = format!("{owner_id}/{project_id}");
    let mut conn = pool.acquire().await.map_err(AccessError::internal)?;
    let row: Option<(
        i64,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT id, role, scope_kind, artifact_path, expires_at, last_active_at \
             FROM project_members \
             WHERE project_key = ? AND member_principal_id = ? AND removed_at IS NULL",
    )
    .bind(&project_key)
    .bind(member.to_string())
    .fetch_optional(&mut *conn)
    .await
    .map_err(AccessError::internal)?;
    let Some((id, role, scope_kind, artifact_path, expires_at, last_active)) = row else {
        return Err(not_found());
    };
    if expires_at.is_some_and(|e| e <= now) {
        return Err(AccessError::new(
            Code::Forbidden,
            "expired: your access to this shared project has expired",
        ));
    }
    let Some(owner) = accounts::by_id(&mut conn, owner_id)
        .await
        .map_err(AccessError::internal)?
    else {
        return Err(not_found());
    };
    if owner.is_disabled() {
        return Err(not_found());
    }
    let role = Role::parse(&role)
        .filter(|r| *r != Role::Owner)
        .ok_or_else(not_found)?;
    let row_caps = super::policy::effective_row(&mut conn, &project_key, role).await?;
    let owner_approval = super::policy::owner_approval_required(&mut conn, &project_key).await?;
    if last_active.map_or(true, |t| now - t >= LAST_ACTIVE_EVERY_MS) {
        // Best effort: a busy store never fails the request over this.
        let _ = sqlx::query("UPDATE project_members SET last_active_at = ? WHERE id = ?")
            .bind(now)
            .bind(id)
            .execute(&mut *conn)
            .await;
    }
    let artifact_scope = scope_kind == "artifact";
    Ok(BrokerShare {
        share: ShareCtx {
            project_key,
            project_id,
            member_principal_id: Some(member.to_string()),
            member_device_id: member_device.map(str::to_string),
            role: Some(role),
            artifact_path: if artifact_scope { artifact_path } else { None },
            owner_approval,
        },
        context: RoleContext::Share {
            role,
            row: row_caps,
            artifact_scope,
        },
        ceiling: CapSet::ALL,
        owner: owner.principal(),
    })
}

// ─── child: the shared project's root ───────────────────────────────────────

/// A shared project as this child resolves it from its own `ikenga.db`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRoot {
    /// Canonical project root.
    pub root: PathBuf,
    pub name: String,
}

/// project id → its resolved root, refreshed by every share pre-hook and
/// every `share_project_info`. The synchronous WS hooks ([`chat_cwd`],
/// [`fs_watch_root`]) read it; a miss fails closed.
static ROOTS: LazyLock<Mutex<HashMap<String, ShareRoot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn remember_root(project_id: &str, root: &ShareRoot) {
    ROOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(project_id.to_string(), root.clone());
}

/// The cached root of `project_id`, if this child resolved it.
pub fn cached_root(project_id: &str) -> Option<ShareRoot> {
    ROOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(project_id)
        .cloned()
}

/// Resolve `project_id` against this child's `ikenga.db` (§4.5.4: the child
/// resolves the root itself, because it owns that database). An unknown,
/// archived or rootless project — or a root that no longer resolves — is
/// `not_found`.
pub async fn resolve_root(
    state: &crate::server::AppState,
    project_id: &str,
) -> Result<ShareRoot, AccessError> {
    let not_found = || AccessError::new(Code::NotFound, "no such project");
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::NotFound, "this workspace has no project store"))?;
    let pool = db
        .ensure_reader_pool()
        .await
        .map_err(AccessError::internal)?;
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT display_name, root_path FROM projects WHERE id = ? AND archived_at IS NULL",
    )
    .bind(project_id)
    .fetch_optional(&pool)
    .await
    .map_err(AccessError::internal)?;
    let (name, root) = row.ok_or_else(not_found)?;
    let root = root
        .filter(|r| !r.trim().is_empty())
        .ok_or_else(not_found)?;
    let root = Path::new(&root).canonicalize().map_err(|_| not_found())?;
    if !root.is_dir() {
        return Err(not_found());
    }
    let resolved = ShareRoot { root, name };
    remember_root(project_id, &resolved);
    Ok(resolved)
}

/// What a share request is confined to (§4.5.4): the project root, or for
/// artifact scope the one file (canonical, and inside the root — a symlink
/// that points out of the project is `not_found`).
pub fn share_target(share: &ShareCtx, root: &ShareRoot) -> Result<PathBuf, AccessError> {
    let Some(raw) = share.artifact_path.as_deref() else {
        return Ok(root.root.clone());
    };
    let rel = normalize_artifact_path(raw)?;
    let not_found = || AccessError::new(Code::NotFound, "the shared artifact is gone");
    let file = root
        .root
        .join(rel)
        .canonicalize()
        .map_err(|_| not_found())?;
    if !file.starts_with(&root.root) || !file.is_file() {
        return Err(not_found());
    }
    Ok(file)
}

/// The directory a share request's runs and watches are confined to: the
/// project root, or the shared artifact's parent directory.
fn share_dir(share: &ShareCtx, root: &ShareRoot) -> Result<PathBuf, AccessError> {
    let target = share_target(share, root)?;
    Ok(if target.is_file() {
        target.parent().map(Path::to_path_buf).unwrap_or(target)
    } else {
        target
    })
}

fn forbidden(msg: impl Into<String>) -> AccessError {
    AccessError::new(Code::Forbidden, msg)
}

fn answer(e: AccessError) -> PreHook {
    PreHook::Answered(super::rpc::error_response(&e))
}

// ─── child: the rpc pre-hook ────────────────────────────────────────────────

/// `permission` notification ids per shared project, snapshotted by the
/// pre-hook of a share's `notifications_list` for its post-filter
/// (`shell_notifications.project_id` isn't part of the row the arm
/// returns). A row raised after the snapshot is simply left out until the
/// next poll: fail closed.
static PERMISSION_IDS: LazyLock<Mutex<HashMap<String, HashSet<i64>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Arms that name a project by id and must name the shared one.
const PROJECT_ID_ARMS: &[&str] = &[
    "actions_read_files",
    "actions_trust_status",
    "actions_write",
    "keybindings_write",
    "settings_read_file",
    "settings_write_field",
];

/// `actions_*` arms rerouted onto a per-request actions manager.
const ACTIONS_ARMS: &[&str] = &[
    "actions_read_files",
    "actions_trust_status",
    "actions_write",
    "keybindings_write",
];

fn str_arg<'a>(args: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|n| args.get(*n).and_then(Value::as_str))
}

/// The share-mode pre-hook (§4.5.4, wired by WP-74a at the head of
/// `rpc_handler`; the arm's class and caps were already checked). Resolves
/// the shared project, applies the per-arm rules, and serves the arm on a
/// narrowed `AppState` clone.
pub async fn prehook(
    state: &Arc<crate::server::AppState>,
    ctx: &AccessCtx,
    share: &ShareCtx,
    cmd: &str,
    args: &Value,
) -> PreHook {
    match prehook_inner(state, ctx, share, cmd, args).await {
        Ok(p) => p,
        Err(e) => answer(e),
    }
}

async fn prehook_inner(
    state: &Arc<crate::server::AppState>,
    ctx: &AccessCtx,
    share: &ShareCtx,
    cmd: &str,
    args: &Value,
) -> Result<PreHook, AccessError> {
    let root = resolve_root(state, &share.project_id).await?;
    let target = share_target(share, &root)?;

    if PROJECT_ID_ARMS.contains(&cmd)
        && str_arg(args, &["projectId", "project_id"]) != Some(share.project_id.as_str())
    {
        return Err(forbidden(
            "a shared project request must name the shared project (projectId)",
        ));
    }
    match cmd {
        // P-23: the legacy key/value settings and the personal scope are the
        // Owner's own; a share writes (and reads) project scope only.
        "settings_get" | "settings_set" => {
            return Err(forbidden(
                "personal settings are not shared (project scope only)",
            ))
        }
        "settings_read_file" => {
            if str_arg(args, &["scope"]).unwrap_or("project") != "project" {
                return Err(forbidden(
                    "personal settings are not shared (project scope only)",
                ));
            }
        }
        "settings_write_field" | "actions_write" | "keybindings_write" => {
            if str_arg(args, &["scope"]) != Some("project") {
                return Err(forbidden("a share writes project scope only"));
            }
        }
        "project_get_active" => return project_row(state, &share.project_id).await,
        "claude_read_jsonl" => transcript_check(state, args, &target).await?,
        "comment_create" => return comment_create(state, ctx, share, args, &target).await,
        "comment_set_status" | "comment_delete" | "comment_record_routing" => {
            comment_owned(state, share, args, &target, cmd != "comment_record_routing").await?
        }
        "notifications_unread_count" => return unread_count(state, &share.project_id).await,
        "notifications_list" => snapshot_permission_ids(state, &share.project_id).await?,
        "studio_thread_get_or_create" => {
            let folder = str_arg(args, &["folderPath", "folder_path"]).unwrap_or_default();
            if !is_under(folder, &target) {
                return Err(forbidden("outside the shared project"));
            }
        }
        _ => {}
    }
    let narrowed = narrowed_state(state, &target, ACTIONS_ARMS.contains(&cmd))?;
    Ok(PreHook::Narrowed(narrowed))
}

/// The cloned `AppState` a share request runs on: its `path_guard`
/// narrowed to `target` (`PathGuard::narrowed_to`) and — for the
/// `actions_*` arms, whose manager captured the router's guard when it was
/// built — a per-request actions manager on that narrowed guard, with no
/// personal home (§4.5.4 "Paths the cloned guard does not reach").
fn narrowed_state(
    state: &Arc<crate::server::AppState>,
    target: &Path,
    rebuild_actions: bool,
) -> Result<Arc<crate::server::AppState>, AccessError> {
    let mut s = (**state).clone();
    s.path_guard = state
        .path_guard
        .narrowed_to(target)
        .map_err(|e| AccessError::new(Code::NotFound, e))?;
    if rebuild_actions {
        s.actions = actions_manager(&s);
    }
    Ok(Arc::new(s))
}

/// [`narrowed_state`]'s actions manager: the router's trust record, no
/// personal home (a directory that never exists, inside the reserved data
/// dir), and every project root checked against the narrowed guard.
fn actions_manager(
    narrowed: &crate::server::AppState,
) -> Option<Arc<crate::server::shared::actions::ActionsManager>> {
    use crate::server::shared::actions::ActionsManager;
    let db = narrowed.pa_db.clone()?;
    let data_dir = narrowed.config.data_dir.clone()?;
    narrowed.actions.as_ref()?;
    let guard = narrowed.path_guard.clone();
    let no_home = data_dir.join("share-no-personal-home");
    Some(Arc::new(
        ActionsManager::with_notifier(None, db, &data_dir, no_home).with_root_guard(Arc::new(
            move |root: &Path| {
                guard.check_maybe_missing(root).map_err(|e| {
                    format!(
                        "the project root is outside the shared project: {} ({e})",
                        root.display()
                    )
                })
            },
        )),
    ))
}

/// Share-mode `actions_*` dispatch on a narrowed guard (review M-7). The
/// pre-hook does this by serving the arm on [`narrowed_state`]'s per-request
/// manager; this entry point is kept for callers that hold no `AppState`
/// and refuses them.
pub async fn actions_dispatch(
    _ctx: &AccessCtx,
    _share: &ShareCtx,
    cmd: &str,
    _args: &Value,
) -> Result<Value, AccessError> {
    Err(AccessError::new(
        Code::Internal,
        format!("{cmd} under a share is served through the rpc pre-hook"),
    ))
}

/// `project_get_active` under a share: the shared project, whatever the
/// Owner has active.
async fn project_row(
    state: &crate::server::AppState,
    project_id: &str,
) -> Result<PreHook, AccessError> {
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such project"))?;
    let pool = db.ensure_pool().await.map_err(AccessError::internal)?;
    let projects = crate::server::shared::projects::list_projects(&pool, false)
        .await
        .map_err(AccessError::internal)?;
    let p = projects
        .into_iter()
        .find(|p| p.id == project_id)
        .ok_or_else(|| AccessError::new(Code::NotFound, "no such project"))?;
    Ok(PreHook::Answered(RpcResponse::success(p)))
}

/// `claude_read_jsonl` under a share (§4.5.4): the transcript's recorded
/// `cwd` must lie under the share, else `not_found`.
async fn transcript_check(
    state: &crate::server::AppState,
    args: &Value,
    target: &Path,
) -> Result<(), AccessError> {
    use crate::server::shared::claude_sessions;
    let not_found = || AccessError::new(Code::NotFound, "no such session in the shared project");
    let session_id = str_arg(args, &["sessionId", "session_id"])
        .ok_or_else(not_found)?
        .to_string();
    let home = state.home.clone().ok_or_else(not_found)?;
    let root = claude_sessions::projects_root_in(&home);
    let events =
        tokio::task::spawn_blocking(move || claude_sessions::read_session(&root, &session_id))
            .await
            .map_err(AccessError::internal)?
            .map_err(|_| not_found())?;
    let events = serde_json::to_value(events).map_err(AccessError::internal)?;
    let cwd = events.as_array().and_then(|a| {
        a.iter()
            .find_map(|e| e.get("cwd").and_then(Value::as_str).map(str::to_string))
    });
    match cwd {
        Some(c) if is_under(&c, target) => Ok(()),
        _ => Err(not_found()),
    }
}

/// `comment_create` under a share: the artifact must be inside the share,
/// and `author_principal_id` is the member (`X-Ikenga-Share-Principal`).
async fn comment_create(
    state: &crate::server::AppState,
    ctx: &AccessCtx,
    share: &ShareCtx,
    args: &Value,
    target: &Path,
) -> Result<PreHook, AccessError> {
    let path = str_arg(args, &["artifactPath", "artifact_path"]).unwrap_or_default();
    if !is_under(path, target) {
        return Err(forbidden(
            "the comment's artifact is outside the shared project",
        ));
    }
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::Internal, "no project store"))?;
    let text = |n: &[&str]| str_arg(args, n).map(str::to_string);
    let num = |n: &[&str]| n.iter().find_map(|k| args.get(*k).and_then(Value::as_f64));
    // A member can't attach a screenshot path: it would name a file the
    // pre-hook never confined.
    let author = share
        .member_principal_id
        .clone()
        .unwrap_or_else(|| ctx.principal_id.to_string());
    let c = crate::server::shared::comments::create_as(
        db,
        Some(author),
        path.to_string(),
        text(&["selector"]).unwrap_or_default(),
        text(&["text"]).unwrap_or_default(),
        None,
        num(&["positionX", "position_x"]),
        num(&["positionY", "position_y"]),
    )
    .await
    .map_err(|e| AccessError::new(Code::InvalidRequest, e))?;
    Ok(PreHook::Answered(RpcResponse::success(c)))
}

/// `comment_set_status` / `comment_delete` (`own = true`) and
/// `comment_record_routing` under a share: the row must be inside the share,
/// and a non-Owner may change only their own rows (§4.5.4; the comparison is
/// narrowing only — it can refuse, never grant).
async fn comment_owned(
    state: &crate::server::AppState,
    share: &ShareCtx,
    args: &Value,
    target: &Path,
    own: bool,
) -> Result<(), AccessError> {
    let not_found = || AccessError::new(Code::NotFound, "no such comment");
    let id = args
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(not_found)?;
    let db = state.pa_db.as_ref().ok_or_else(not_found)?;
    let (path, author) = crate::server::shared::comments::path_and_author(db, id)
        .await
        .map_err(AccessError::internal)?
        .ok_or_else(not_found)?;
    if !is_under(&path, target) {
        return Err(not_found());
    }
    if own && (author.is_none() || author != share.member_principal_id) {
        return Err(forbidden("you can change only your own comments"));
    }
    Ok(())
}

/// The shared project's unresolved `permission` rows, unread (§5.7).
async fn unread_count(
    state: &crate::server::AppState,
    project_id: &str,
) -> Result<PreHook, AccessError> {
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::Internal, "no project store"))?;
    let pool = db
        .ensure_reader_pool()
        .await
        .map_err(AccessError::internal)?;
    let (unread, pending): (i64, i64) = sqlx::query_as(
        "SELECT \
           COALESCE(SUM(CASE WHEN read_at IS NULL AND resolved_at IS NULL THEN 1 ELSE 0 END), 0), \
           COALESCE(SUM(CASE WHEN resolved_at IS NULL THEN 1 ELSE 0 END), 0) \
         FROM shell_notifications WHERE kind = 'permission' AND project_id = ?",
    )
    .bind(project_id)
    .fetch_one(&pool)
    .await
    .map_err(AccessError::internal)?;
    let mut by_kind = serde_json::Map::new();
    if unread > 0 {
        by_kind.insert("permission".into(), json!(unread));
    }
    Ok(PreHook::Answered(RpcResponse::success(json!({
        "total": unread,
        "byKind": by_kind,
        "pendingPermissions": pending,
    }))))
}

async fn snapshot_permission_ids(
    state: &crate::server::AppState,
    project_id: &str,
) -> Result<(), AccessError> {
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::Internal, "no project store"))?;
    let pool = db
        .ensure_reader_pool()
        .await
        .map_err(AccessError::internal)?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM shell_notifications WHERE kind = 'permission' AND project_id = ?",
    )
    .bind(project_id)
    .fetch_all(&pool)
    .await
    .map_err(AccessError::internal)?;
    PERMISSION_IDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(project_id.to_string(), ids.into_iter().collect());
    Ok(())
}

// ─── child: the post-hook filter ────────────────────────────────────────────

/// Row fields that carry a row's working directory / path.
const PATH_FIELDS: &[&str] = &[
    "cwd",
    "projectDir",
    "project_dir",
    "projectPath",
    "project_path",
    "folderPath",
    "folder_path",
    "artifactPath",
    "artifact_path",
    "rootPath",
    "root_path",
];

fn row_under(row: &Value, base: &Path) -> bool {
    PATH_FIELDS.iter().any(|f| {
        row.get(*f)
            .and_then(Value::as_str)
            .is_some_and(|p| is_under(p, base))
    })
}

/// Whether a `cost*` / `*_cost*` / `*Cost*` / `usage` key (§4.5.4, P-36).
pub fn is_cost_key(k: &str) -> bool {
    k.starts_with("cost") || k.contains("_cost") || k.contains("Cost") || k == "usage"
}

/// Remove every cost key, recursively (a Reviewer share: "No cost figures").
pub fn strip_costs(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|k, _| !is_cost_key(k));
            for child in map.values_mut() {
                strip_costs(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_costs),
        _ => {}
    }
}

/// Whether `cmd` is a `sessions`-cap read whose result a Reviewer share
/// gets without cost fields.
fn is_session_read(cmd: &str) -> bool {
    ["chi_", "claude_", "studio_", "agent_ops_"]
        .iter()
        .any(|p| cmd.starts_with(p))
}

/// The share-mode post-hook (§4.5.4 table): list results narrowed to the
/// shared project, single rows outside it dropped (`null`), and for a
/// Reviewer every cost field removed. Without a resolved root every list is
/// emptied (fail closed).
pub fn filter(_ctx: &AccessCtx, share: &ShareCtx, cmd: &str, data: &mut Value) {
    let base = cached_root(&share.project_id).and_then(|r| share_target(share, &r).ok());
    let keep = |row: &Value| base.as_deref().is_some_and(|b| row_under(row, b));
    match cmd {
        "project_list" => retain(data, |row| {
            row.get("id").and_then(Value::as_str) == Some(share.project_id.as_str())
        }),
        "notifications_list" => {
            let ids = PERMISSION_IDS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&share.project_id)
                .cloned()
                .unwrap_or_default();
            retain(data, |row| {
                row.get("kind").and_then(Value::as_str) == Some("permission")
                    && row
                        .get("id")
                        .and_then(Value::as_i64)
                        .is_some_and(|id| ids.contains(&id))
                    && row
                        .get("projectId")
                        .and_then(Value::as_str)
                        .map_or(true, |p| p == share.project_id)
            })
        }
        "claude_list_sessions"
        | "claude_session_list"
        | "chi_list"
        | "studio_thread_list_recent"
        | "comment_list" => retain(data, keep),
        "comment_get" | "chi_status" | "studio_thread_get" => {
            if !keep(data) {
                *data = Value::Null;
            }
        }
        _ => {}
    }
    if share.role == Some(Role::Reviewer) && is_session_read(cmd) {
        strip_costs(data);
    }
}

/// Keep the array entries `keep` admits (a list arm's `data` is an array,
/// or an object holding one under a list-ish key).
fn retain(data: &mut Value, keep: impl Fn(&Value) -> bool) {
    match data {
        Value::Array(rows) => rows.retain(|r| keep(r)),
        Value::Object(map) => {
            for key in [
                "items",
                "rows",
                "sessions",
                "threads",
                "runs",
                "notifications",
            ] {
                if let Some(Value::Array(rows)) = map.get_mut(key) {
                    rows.retain(|r| keep(r));
                }
            }
        }
        _ => {}
    }
}

// ─── child: WS hooks ────────────────────────────────────────────────────────

fn share_base(share: &ShareCtx, file_ok: bool) -> Result<PathBuf, AccessError> {
    let root = cached_root(&share.project_id).ok_or_else(|| {
        forbidden("the shared project isn't resolved on this server yet — reopen it")
    })?;
    if file_ok {
        share_target(share, &root)
    } else {
        share_dir(share, &root)
    }
}

/// `/ws/chat` `Prompt { cwd }` under a share (§4.5.4, A-36): no `cwd` runs
/// at the share's directory; a `cwd` under it runs there (canonical); any
/// other is refused. Own-workspace requests pass `cwd` through unchanged.
pub fn chat_cwd(ctx: &AccessCtx, cwd: Option<String>) -> Result<Option<String>, AccessError> {
    let Some(share) = &ctx.share else {
        return Ok(cwd);
    };
    let base = share_base(share, false)?;
    match cwd {
        None => Ok(Some(base.to_string_lossy().into_owned())),
        Some(c) => {
            let canonical = Path::new(&c)
                .canonicalize()
                .map_err(|_| forbidden("the run's directory is outside the shared project"))?;
            if canonical.starts_with(&base) {
                Ok(Some(canonical.to_string_lossy().into_owned()))
            } else {
                Err(forbidden(
                    "the run's directory is outside the shared project",
                ))
            }
        }
    }
}

/// Share-originated runs get no Ikenga vault env (§4.5.1, N-10): returns
/// whether the run may receive vault injection. The daemon's chat engines
/// inject none today (the `/ws/chat` hook logs the refusal), and
/// `secrets_env`'s `IKENGA_SECRET_*` values are RPC-only — `pty`'s
/// host-only filter keeps them out of every child process.
pub fn run_env(ctx: &AccessCtx) -> bool {
    ctx.share.is_none()
}

/// `/ws/fs` watch roots under a share (A-36): the root must resolve inside
/// the share (the project tree, or exactly the shared artifact). Own
/// workspace: unchanged.
pub fn fs_watch_root(ctx: &AccessCtx, root: &str) -> Result<(), AccessError> {
    let Some(share) = &ctx.share else {
        return Ok(());
    };
    let base = share_base(share, true)?;
    let canonical = Path::new(root)
        .canonicalize()
        .map_err(|_| forbidden("the watch root is outside the shared project"))?;
    let admitted = if base.is_file() {
        canonical == base
    } else {
        canonical.starts_with(&base)
    };
    if admitted {
        Ok(())
    } else {
        Err(forbidden("the watch root is outside the shared project"))
    }
}

// ─── child: the internal arms ───────────────────────────────────────────────

/// The two `internal` arms (§9.1), served with the child's `AppState` (the
/// `rpc_handler` pre-hook answers them; the arm's class was already
/// checked: broker → child only). `None` for any other command.
pub async fn internal(
    state: &crate::server::AppState,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Option<RpcResponse> {
    let r = match cmd {
        "share_project_info" => internal_project_info(state, args).await,
        "notifications_record_access" => internal_record_access(state, ctx, args).await,
        _ => return None,
    };
    Some(match r {
        Ok(v) => RpcResponse::success(v),
        Err(e) => super::rpc::error_response(&e),
    })
}

/// `share_project_info {projectId}` → `{root, name}`; unknown → `not_found`.
async fn internal_project_info(
    state: &crate::server::AppState,
    args: &Value,
) -> Result<Value, AccessError> {
    let project_id = str_arg(args, &["projectId", "project_id"])
        .filter(|p| !p.is_empty())
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`projectId` is required"))?;
    let r = resolve_root(state, project_id).await?;
    Ok(json!({ "root": r.root.to_string_lossy(), "name": r.name }))
}

/// `notifications_record_access {kind:'invite', title, body}` → `{id}`: the
/// first `invite` producer (§7.3).
async fn internal_record_access(
    state: &crate::server::AppState,
    _ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    use crate::server::shared::notifications::{
        record, Coalesce, NewNotification, NotificationKind,
    };
    if str_arg(args, &["kind"]) != Some("invite") {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "kind must be `invite`",
        ));
    }
    let title = str_arg(args, &["title"])
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`title` is required"))?;
    let clamp = |s: &str| s.chars().take(200).collect::<String>();
    let db = state
        .pa_db
        .as_ref()
        .ok_or_else(|| AccessError::new(Code::Internal, "no project store"))?;
    let pool = db.ensure_pool().await.map_err(AccessError::internal)?;
    let out = record(
        &pool,
        NewNotification {
            kind: NotificationKind::Invite,
            title: clamp(title),
            body: str_arg(args, &["body"]).map(clamp),
            action: Some(json!({ "type": "route", "to": "/settings/members" })),
            source: "access".into(),
            dedupe_key: None,
            coalesce: Coalesce::Never,
        },
    )
    .await
    .map_err(AccessError::internal)?;
    Ok(json!({ "id": out.notification().map(|n| n.id) }))
}

/// `share_project_info` reached through `access::rpc::dispatch` (no
/// `AppState`): the `rpc_handler` pre-hook serves it ([`internal`]), so
/// this path only answers a caller that bypassed it.
pub fn project_info(_ctx: &AccessCtx, _args: &Value) -> Result<Value, AccessError> {
    Err(AccessError::new(
        Code::Internal,
        "share_project_info is served by the rpc pre-hook",
    ))
}

/// `notifications_record_access` reached without an `AppState`: see
/// [`project_info`].
pub fn record_access(_ctx: &AccessCtx, _args: &Value) -> Result<Value, AccessError> {
    Err(AccessError::new(
        Code::Internal,
        "notifications_record_access is served by the rpc pre-hook",
    ))
}

/// How long a cached root is trusted without a refresh is unbounded: every
/// share RPC re-resolves it. Exposed for tests.
#[cfg(test)]
pub(crate) fn forget_root(project_id: &str) {
    ROOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(project_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_headers_parse_and_absent_means_none() {
        assert!(from_child_headers(&HeaderMap::new()).is_none());
        let mut h = HeaderMap::new();
        for (k, v) in [
            ("x-ikenga-principal", "owner"),
            ("x-ikenga-share-project", "royalti-co"),
            ("x-ikenga-share-principal", "ada"),
            ("x-ikenga-share-device", "-"),
            ("x-ikenga-share-role", "reviewer"),
            ("x-ikenga-share-policy", "owner-approval"),
        ] {
            h.insert(k, v.parse().unwrap());
        }
        let s = from_child_headers(&h).unwrap();
        assert_eq!(s.project_key, "owner/royalti-co");
        assert_eq!(s.member_device_id, None);
        assert_eq!(s.role, Some(Role::Reviewer));
        assert!(s.owner_approval);
        assert!(s.artifact_path.is_none());
    }

    /// The broker's headers parse back into the same share (§4.5.3).
    #[test]
    fn to_child_headers_round_trips() {
        let share = ShareCtx {
            project_key: "owner/royalti-co".into(),
            project_id: "royalti-co".into(),
            member_principal_id: Some("ada".into()),
            member_device_id: None,
            role: Some(Role::Guest),
            artifact_path: Some("docs/brief.md".into()),
            owner_approval: false,
        };
        let mut h = HeaderMap::new();
        h.insert("x-ikenga-principal", "owner".parse().unwrap());
        for (k, v) in to_child_headers(&share) {
            h.insert(k, v.parse().unwrap());
        }
        assert_eq!(from_child_headers(&h), Some(share));
        assert!(any_share_header(&h));
    }

    #[test]
    fn a_share_is_selected_by_header_or_query() {
        let parts = |uri: &str, header: Option<&str>| {
            let mut b = axum::http::Request::builder().uri(uri);
            if let Some(v) = header {
                b = b.header("x-ikenga-share", v);
            }
            b.body(()).unwrap().into_parts().0
        };
        assert_eq!(selected(&parts("/api/rpc", None)), None);
        assert_eq!(
            selected(&parts("/api/rpc", Some("o/p"))).as_deref(),
            Some("o/p")
        );
        assert_eq!(
            selected(&parts("/ws/chat/x?cols=1&share=o%2Fp", None)).as_deref(),
            Some("o/p")
        );
        assert_eq!(selected(&parts("/ws/chat/x?share=", None)), None);
    }

    #[test]
    fn artifact_paths_are_normalized_relative_paths() {
        assert_eq!(
            normalize_artifact_path("./plans//board.html").unwrap(),
            "plans/board.html"
        );
        for bad in ["", "/etc/passwd", "../x", "a/../../b", "a\\b", "."] {
            assert!(normalize_artifact_path(bad).is_err(), "{bad:?}");
        }
        let owner = PrincipalId::new_v7();
        assert_eq!(
            split_project_key(&format!("{owner}/royalti-co")),
            Some((owner, "royalti-co".into()))
        );
        for bad in ["o/p", "x", &format!("{owner}/"), &format!("{owner}/a/b")] {
            assert!(split_project_key(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn reviewer_reads_lose_every_cost_field() {
        let mut v = json!({"runs": [{"id": 1, "costUsd": 1.2, "total_cost_usd": 3,
            "usage": {"in": 1}, "brief": "x", "nested": {"cost": 2, "ok": true}}]});
        strip_costs(&mut v);
        assert_eq!(
            v,
            json!({"runs": [{"id": 1, "brief": "x", "nested": {"ok": true}}]})
        );
    }

    fn share_ctx(project_id: &str, role: Role, artifact: Option<&str>) -> AccessCtx {
        AccessCtx {
            principal_id: PrincipalId::new_v7(),
            via: super::super::Via::Relayed,
            device_id: None,
            tier: super::super::Tier::View,
            share: Some(ShareCtx {
                project_key: format!("o/{project_id}"),
                project_id: project_id.into(),
                member_principal_id: Some("m".into()),
                member_device_id: None,
                role: Some(role),
                artifact_path: artifact.map(str::to_string),
                owner_approval: true,
            }),
            share_headers: true,
            caps: super::super::CapSet::ALL,
            admin_strength: false,
            meta: Default::default(),
        }
    }

    /// A-36: under a share, `/ws/chat` cwds and `/ws/fs` watch roots outside
    /// the share are refused; an unresolved share fails closed.
    #[test]
    fn ws_hooks_confine_to_the_share() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::write(project.join("docs/brief.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        let id = format!("ws-hooks-{}", PrincipalId::new_v7());
        let ctx = share_ctx(&id, Role::Operator, None);
        assert_eq!(
            chat_cwd(&ctx, None).unwrap_err().code,
            Code::Forbidden,
            "unresolved: fail closed"
        );
        remember_root(
            &id,
            &ShareRoot {
                root: project.clone(),
                name: "P".into(),
            },
        );
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(chat_cwd(&ctx, None).unwrap(), Some(s(&project)));
        assert_eq!(
            chat_cwd(&ctx, Some(s(&project.join("docs")))).unwrap(),
            Some(s(&project.join("docs")))
        );
        assert!(chat_cwd(&ctx, Some(s(&root.join("other")))).is_err());
        assert!(chat_cwd(&ctx, Some(format!("{}/../other", s(&project)))).is_err());
        assert!(fs_watch_root(&ctx, &s(&project.join("docs"))).is_ok());
        assert!(fs_watch_root(&ctx, &s(&root.join("other"))).is_err());

        let guest = share_ctx(&id, Role::Guest, Some("docs/brief.md"));
        assert!(fs_watch_root(&guest, &s(&project.join("docs/brief.md"))).is_ok());
        assert!(fs_watch_root(&guest, &s(&project.join("docs"))).is_err());
        assert_eq!(
            chat_cwd(&guest, None).unwrap(),
            Some(s(&project.join("docs")))
        );
        assert!(
            !run_env(&ctx),
            "N-10: no vault env in share-originated runs"
        );
        forget_root(&id);

        let own = AccessCtx { share: None, ..ctx };
        assert_eq!(
            chat_cwd(&own, Some("/anywhere".into())).unwrap(),
            Some("/anywhere".into())
        );
        assert!(fs_watch_root(&own, "/anywhere").is_ok());
        assert!(run_env(&own));
    }

    /// §4.2: an artifact symlinked out of the project is `not_found`.
    #[cfg(unix)]
    #[test]
    fn a_shared_artifact_cannot_escape_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(root.join("outside.md"), "x").unwrap();
        std::os::unix::fs::symlink(root.join("outside.md"), project.join("link.md")).unwrap();
        let r = ShareRoot {
            root: project.clone(),
            name: "P".into(),
        };
        let ctx = share_ctx("p", Role::Guest, Some("link.md"));
        assert_eq!(
            share_target(ctx.share.as_ref().unwrap(), &r)
                .unwrap_err()
                .code,
            Code::NotFound
        );
    }

    #[test]
    fn the_post_filter_keeps_the_shared_project_only() {
        let id = format!("filter-{}", PrincipalId::new_v7());
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().canonicalize().unwrap();
        remember_root(
            &id,
            &ShareRoot {
                root: project.clone(),
                name: "P".into(),
            },
        );
        let ctx = share_ctx(&id, Role::Reviewer, None);
        let share = ctx.share.clone().unwrap();
        let mut list = json!([{"id": id}, {"id": "other"}]);
        filter(&ctx, &share, "project_list", &mut list);
        assert_eq!(list, json!([{"id": id}]));
        let inside = project.join("x").to_string_lossy().into_owned();
        let mut sessions = json!([
            {"sessionId": "a", "projectDir": inside, "costUsd": 1},
            {"sessionId": "b", "projectDir": "/elsewhere"},
            {"sessionId": "c"}
        ]);
        filter(&ctx, &share, "claude_list_sessions", &mut sessions);
        assert_eq!(sessions, json!([{"sessionId": "a", "projectDir": inside}]));
        let mut notes = json!([{"id": 7, "kind": "permission"}, {"id": 8, "kind": "update"}]);
        PERMISSION_IDS
            .lock()
            .unwrap()
            .insert(id.clone(), [7].into_iter().collect());
        filter(&ctx, &share, "notifications_list", &mut notes);
        assert_eq!(notes, json!([{"id": 7, "kind": "permission"}]));
        forget_root(&id);
        let mut gone = json!([{"sessionId": "a", "projectDir": inside}]);
        filter(&ctx, &share, "claude_list_sessions", &mut gone);
        assert_eq!(gone, json!([]), "no resolved root: fail closed");
    }

    /// A-20: a share request never reaches an owner- or operator-class arm,
    /// whatever its caps — one check per mapped arm.
    #[test]
    fn a_share_never_reaches_owner_or_operator_arms() {
        use super::super::{authorize, rpc_requirements::RPC_REQUIREMENTS, ArmClass};
        let ctx = share_ctx("p", Role::Operator, None);
        let mut checked = 0;
        for (cmd, req) in RPC_REQUIREMENTS {
            if matches!(
                req.class,
                ArmClass::Owner | ArmClass::Operator | ArmClass::Internal
            ) {
                assert!(
                    authorize(&ctx, cmd).is_err(),
                    "{cmd} reached through a share"
                );
                checked += 1;
            }
        }
        assert!(checked > 50, "{checked}");
        assert!(authorize(&ctx, "fs_read").is_ok());
        assert!(authorize(&ctx, "brand_new_unmapped_cmd").is_err());
    }

    /// §4.5.2 against a real `accounts.db`: membership, expiry (403), an
    /// unknown project and a disabled Owner (404), the override-applied row.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn the_broker_selects_only_active_memberships() {
        use crate::server::operator::{open_accounts, test_support::temp_root, Opener};
        let (_tmp, root) = temp_root();
        let pool = open_accounts(&root, Opener::Broker).await.unwrap();
        let store = super::super::AccessStore::attach_t1(pool.clone())
            .await
            .unwrap();
        let mut ids = Vec::new();
        for (name, uid) in [("ned", 20001), ("ada", 20002)] {
            let id = PrincipalId::new_v7();
            sqlx::query(
                "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, \
                 home, shell, is_admin, session_epoch, adopted, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, '/h', '/bin/sh', 0, 0, 0, 0, 0)",
            )
            .bind(id.to_string())
            .bind(name)
            .bind(format!("ik-{name}"))
            .bind(uid)
            .bind(uid)
            .execute(&pool)
            .await
            .unwrap();
            ids.push(id);
        }
        let (owner, ada) = (ids[0], ids[1]);
        let key = format!("{owner}/royalti-co");
        let now = now_ms();
        assert_eq!(
            select_membership(&pool, &key, ada, None, now)
                .await
                .unwrap_err()
                .code,
            Code::NotFound
        );
        crate::access::members::tests::add(
            &store,
            &key,
            &ada.to_string(),
            "reviewer",
            None,
            Some(now + 60_000),
        )
        .await;
        crate::access::policy::set_cell(
            &store,
            &crate::access::policy::tests::session(owner),
            &key,
            Role::Reviewer,
            super::super::Cap::Dispatch,
            true,
        )
        .await
        .unwrap();
        let s = select_membership(&pool, &key, ada, Some("dev-1"), now)
            .await
            .unwrap();
        assert_eq!(s.owner.id, owner);
        assert_eq!(s.share.role, Some(Role::Reviewer));
        assert_eq!(s.share.member_device_id.as_deref(), Some("dev-1"));
        assert!(s.share.owner_approval, "default on");
        match s.context {
            RoleContext::Share {
                row,
                artifact_scope,
                ..
            } => {
                assert!(row.contains(super::super::Cap::Dispatch) && !artifact_scope);
                assert!(!row.contains(super::super::Cap::Secrets));
            }
            other => panic!("{other:?}"),
        }
        let e = select_membership(&pool, &key, ada, None, now + 120_000)
            .await
            .unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
        assert!(e.message.starts_with("expired"));
        assert_eq!(
            select_membership(&pool, &key, owner, None, now)
                .await
                .unwrap_err()
                .code,
            Code::NotFound,
            "your own project needs no share"
        );
        sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
            .bind(owner.to_string())
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            select_membership(&pool, &key, ada, None, now)
                .await
                .unwrap_err()
                .code,
            Code::NotFound
        );
    }
}
