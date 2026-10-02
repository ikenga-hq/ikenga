//! G-ACCESS — the access layer (`plans/shell-ux-rearchitecture/drafts/
//! access-schema.md`, FROZEN Round 60): capability vocabulary, principals
//! and credentials on T0 and T1, devices, the audit chain, and the
//! skeleton of every Part B command.
//!
//! WP-74a (this skeleton) owns:
//!
//! * [`caps`] — the one source of truth for caps / tiers / roles / arm
//!   classes (§1), rendered to `src/lib/access/caps.gen.ts` by [`caps_ts`];
//! * [`rpc_requirements`] — route → capability for every `rpc.rs` arm (§1.6),
//!   append-only for arm-adding WPs;
//! * [`ctx`] — [`AccessCtx`], built per request (§2.3);
//! * [`store`] + [`migrations`] — the access store and its `access`
//!   migration set (§2.5, §8.1, §8.2);
//! * [`devices`] + [`sockets`] — device records, the device credential, and
//!   revoke / tier-change immediacy (§3.8–§3.11);
//! * [`audit`] — the hash chain core (§6.1–§6.4);
//! * [`rpc`], [`ws`], [`http`] — the §9.1 arm surface, the WS frame checks
//!   and the public `/access/*` routes, registered skeleton-first (§9.2);
//! * `t1` (Linux) — the fills for WP-20's broker hooks (R-3, R-4, R-5,
//!   R-11).
//!
//! Stub modules later waves fill (§9.2): [`routing`] (WP-75), [`members`],
//! [`invites`], [`policy`], [`share`] (WP-76), and `audit::{list, export,
//! absorb, reseal}` (WP-77). Pairing (`access::pairing`, SPAKE2, the
//! fingerprint) is WP-74b.

pub mod audit;
pub mod caps;
pub mod caps_ts;
pub mod ctx;
pub mod devices;
pub mod http;
pub mod invites;
pub mod members;
pub mod migrations;
pub mod policy;
pub mod routing;
pub mod rpc;
pub mod rpc_requirements;
pub mod share;
pub mod sockets;
pub mod store;
#[cfg(test)]
mod t0_tests;
#[cfg(target_os = "linux")]
pub mod t1;
pub mod ws;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub use caps::{ArmClass, Cap, CapSet, Requirement, Role, Tier};
pub use ctx::{AccessCtx, HostIdentity, RequestMeta, ShareCtx, Via};
pub use store::{AccessStore, StoreTier};

use crate::executor::PrincipalId;

/// The closed error-code set (§9.1 "Errors"). RPC `error` strings are
/// `<code>: <message>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Unauthenticated,
    Forbidden,
    NotFound,
    Conflict,
    Gone,
    Expired,
    Throttled,
    InvalidRequest,
    RoutingRefused,
    OwnerApprovalRequired,
    AnswerInTerminal,
    RequiresT1,
    ServedByBroker,
    AuditUnavailable,
    StoreUnavailable,
    Internal,
}

impl Code {
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::Unauthenticated => "unauthenticated",
            Code::Forbidden => "forbidden",
            Code::NotFound => "not_found",
            Code::Conflict => "conflict",
            Code::Gone => "gone",
            Code::Expired => "expired",
            Code::Throttled => "throttled",
            Code::InvalidRequest => "invalid_request",
            Code::RoutingRefused => "routing_refused",
            Code::OwnerApprovalRequired => "owner_approval_required",
            Code::AnswerInTerminal => "answer_in_terminal",
            Code::RequiresT1 => "requires_t1",
            Code::ServedByBroker => "served_by_broker",
            Code::AuditUnavailable => "audit_unavailable",
            Code::StoreUnavailable => "store_unavailable",
            Code::Internal => "internal",
        }
    }

    /// The HTTP status a non-RPC endpoint answers with.
    pub fn status(self) -> axum::http::StatusCode {
        use axum::http::StatusCode as S;
        match self {
            Code::Unauthenticated | Code::Expired => S::UNAUTHORIZED,
            Code::Forbidden
            | Code::RoutingRefused
            | Code::OwnerApprovalRequired
            | Code::RequiresT1 => S::FORBIDDEN,
            Code::NotFound | Code::ServedByBroker => S::NOT_FOUND,
            Code::Conflict | Code::AnswerInTerminal => S::CONFLICT,
            Code::Gone => S::GONE,
            Code::Throttled => S::TOO_MANY_REQUESTS,
            Code::InvalidRequest => S::BAD_REQUEST,
            Code::AuditUnavailable | Code::StoreUnavailable => S::SERVICE_UNAVAILABLE,
            Code::Internal => S::INTERNAL_SERVER_ERROR,
        }
    }
}

/// An access-layer refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessError {
    pub code: Code,
    pub message: String,
}

impl AccessError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!("access: {e}");
        Self::new(
            Code::Internal,
            "the access store failed; see the server log",
        )
    }

    pub fn forbidden_admin() -> Self {
        Self::new(
            Code::Forbidden,
            "needs a password session, the host, or a full device (admin_strength)",
        )
    }

    pub fn missing(caps: CapSet) -> Self {
        Self::new(Code::Forbidden, format!("missing={}", caps.to_header()))
    }

    pub fn class(class: ArmClass) -> Self {
        Self::new(Code::Forbidden, format!("class={class}"))
    }

    /// A §9.2 stub: the arm is registered; its body lands with `wp`.
    pub fn not_implemented(wp: &str) -> Self {
        Self::new(Code::Internal, format!("not implemented ({wp})"))
    }

    pub fn store_unavailable() -> Self {
        Self::new(
            Code::StoreUnavailable,
            "no access store: the daemon runs without --data-dir (or the store failed to open)",
        )
    }
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AccessError {}

/// The Part B daemon flags the access layer reads (§10.1): every one
/// defaults off or unlimited. Parsed by `ikenga-server`'s clap and carried on
/// `server::T1ServeOptions` beside WP-20's `--insecure-cookie` (reused, R-9:
/// read by every tier).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessOptions {
    /// `--public-url` / `IKENGA_PUBLIC_URL`: the pairing / invite link base
    /// (§3.3 rule 1).
    pub public_url: Option<String>,
    /// `--insecure-cookie`: drop `Secure` from the device cookie too (§3.8).
    pub insecure_cookie: bool,
    /// `--max-accounts N` (P-27): caps every account-creation path. `None`
    /// is unlimited.
    pub max_accounts: Option<u32>,
    /// `--invite-ttl DAYS`: the invite token TTL, 7 by default, at most 30
    /// (P-15).
    pub invite_ttl_days: u32,
    /// `--member-invites-create-accounts` (§4.4, N-11 / DEC-85): off by
    /// default.
    pub member_invites_create_accounts: bool,
}

impl Default for AccessOptions {
    fn default() -> Self {
        Self {
            public_url: None,
            insecure_cookie: false,
            max_accounts: None,
            invite_ttl_days: DEFAULT_INVITE_TTL_DAYS,
            member_invites_create_accounts: false,
        }
    }
}

/// Set by the T1 broker on a call **it** makes to a principal child (an
/// `internal` arm, §9.1 — WP-76's `share_project_info` /
/// `notifications_record_access` callers), never on a relayed request: the
/// proxy strips every client `x-ikenga-*` header and adds only its narrowing
/// set. Without it a per-child-token request is [`Via::Relayed`], which no
/// `internal` arm accepts — fail closed, whatever the relay's caps.
pub const INTERNAL_CALL_HEADER: &str = "x-ikenga-internal-call";

pub const DEFAULT_INVITE_TTL_DAYS: u32 = 7;
pub const MAX_INVITE_TTL_DAYS: u32 = 30;

/// What keeps the desktop's daemon alive besides PTYs and sockets (§2.5,
/// review M-3): open pairing sessions (WP-74b) and open relay asks (WP-75)
/// each hold one of these while they are pending. The idle watcher in
/// `run_server` adds [`keepalive_count`] to its activity check.
static KEEPALIVE: AtomicUsize = AtomicUsize::new(0);

/// Held while something pending must keep the daemon from idling out.
#[derive(Debug)]
pub struct KeepAlive(());

impl KeepAlive {
    pub fn hold() -> Self {
        KEEPALIVE.fetch_add(1, Ordering::SeqCst);
        KeepAlive(())
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        KEEPALIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn keepalive_count() -> usize {
    KEEPALIVE.load(Ordering::SeqCst)
}

/// Which single-tenant daemon this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonMode {
    /// The T0 daemon: opens `<data-dir>/access.db`, owns pairing.
    T0,
    /// A T1 principal child (§1.7, review C-22): no access store, no
    /// synthetic owner, every `access_*` arm → `served_by_broker`, caps only
    /// from `X-Ikenga-Caps`.
    PrincipalChild,
}

/// The T0 daemon's (or a principal child's) access state. Attached to the
/// router through an `Extension` layer, **not** an `AppState` field (X-2).
pub struct DaemonAccess {
    pub mode: DaemonMode,
    store: Option<AccessStore>,
    /// The synthetic owner (§2.1) — or, with no store, an ephemeral id that
    /// names nothing persistent.
    owner: PrincipalId,
    pub host: HostIdentity,
    pub sockets: Arc<sockets::Registry>,
    pub seen: devices::SeenGate,
    pub options: AccessOptions,
}

impl std::fmt::Debug for DaemonAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonAccess")
            .field("mode", &self.mode)
            .field("store", &self.store)
            .finish()
    }
}

impl DaemonAccess {
    fn build(mode: DaemonMode, store: Option<AccessStore>, options: AccessOptions) -> Arc<Self> {
        let owner = store
            .as_ref()
            .and_then(|s| s.meta().owner_principal_id)
            .unwrap_or_else(PrincipalId::new_v7);
        Arc::new(Self {
            mode,
            store,
            owner,
            host: HostIdentity::detect(),
            sockets: sockets::Registry::new(),
            seen: devices::SeenGate::default(),
            options,
        })
    }

    /// T0 boot (§2.5): open, migrate and verify `<data-dir>/access.db`. With
    /// no `--data-dir` — or a store that refuses to open (a newer schema, a
    /// T1 store) — the daemon still serves, but every store-backed access
    /// arm answers `store_unavailable` and pairing is off.
    pub async fn boot_t0(data_dir: Option<&Path>, options: AccessOptions) -> Arc<Self> {
        let host = HostIdentity::detect();
        let store = match data_dir {
            None => None,
            Some(dir) => match AccessStore::open_t0(dir, &host).await {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::error!(
                        "access store {} unavailable: {e:#} — pairing is off",
                        AccessStore::t0_path(dir).display()
                    );
                    None
                }
            },
        };
        Self::build(DaemonMode::T0, store, options)
    }

    /// A T1 principal child: never opens or creates `access.db` (A-32).
    pub fn principal_child(options: AccessOptions) -> Arc<Self> {
        Self::build(DaemonMode::PrincipalChild, None, options)
    }

    /// No store (routers built without `run_server`, e.g. tests).
    pub fn unavailable() -> Arc<Self> {
        Self::build(DaemonMode::T0, None, AccessOptions::default())
    }

    #[cfg(test)]
    pub fn with_store(store: AccessStore) -> Arc<Self> {
        Self::build(DaemonMode::T0, Some(store), AccessOptions::default())
    }

    pub fn store(&self) -> Option<&AccessStore> {
        self.store.as_ref()
    }

    pub fn owner(&self) -> PrincipalId {
        self.owner
    }

    /// The T0 operator bearer → synthetic owner, host device, `full` (§2.3).
    /// `approve` only if [`routing::routing_ok`] (§5.1; the host device
    /// satisfies a T0 `this_device` preference).
    pub async fn operator_ctx(&self, meta: RequestMeta) -> AccessCtx {
        let device_id = self.store().and_then(|s| s.meta().host_device_id.clone());
        let routing_ok = routing::routing_ok(self.store(), &self.owner, device_id.as_deref()).await;
        AccessCtx {
            principal_id: self.owner,
            via: Via::Operator,
            device_id,
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: caps::effective(
                caps::RoleContext::OwnWorkspace,
                Tier::Full,
                CapSet::ALL,
                routing_ok,
            ),
            admin_strength: true,
            meta,
        }
    }

    /// A T0 device grant → the row's principal and tier (§2.3).
    pub async fn device_ctx(&self, row: &devices::DeviceRow, meta: RequestMeta) -> AccessCtx {
        let via = Via::Device {
            device_id: row.device_id.clone(),
        };
        let principal_id = row.principal_id.parse().unwrap_or(self.owner);
        let routing_ok =
            routing::routing_ok(self.store(), &principal_id, Some(&row.device_id)).await;
        AccessCtx {
            principal_id,
            admin_strength: AccessCtx::admin_strength_of(&via, row.tier),
            via,
            device_id: Some(row.device_id.clone()),
            tier: row.tier,
            share: None,
            share_headers: false,
            // Own workspace (T0 has one principal): role = Owner, so the
            // tier decides (§1.4), and routing (§5.1) gates `approve`.
            caps: caps::effective(
                caps::RoleContext::OwnWorkspace,
                row.tier,
                CapSet::ALL,
                routing_ok,
            ),
            meta,
        }
    }

    /// A per-child-token request on a T1 principal child (§1.7): caps only
    /// from `X-Ikenga-Caps` (absent → none), share headers parsed for
    /// narrowing. `X-Ikenga-Principal` is attribution only. The broker's own
    /// call ([`INTERNAL_CALL_HEADER`], and no caps header — a relay always
    /// carries one) is [`Via::ChildToken`]; anything else is a relayed
    /// principal request, [`Via::Relayed`].
    pub fn child_ctx(&self, headers: &axum::http::HeaderMap, meta: RequestMeta) -> AccessCtx {
        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        let caps = header("x-ikenga-caps")
            .map(|v| CapSet::parse_header(&v))
            .unwrap_or(CapSet::EMPTY);
        let principal_id = header("x-ikenga-principal")
            .and_then(|v| v.parse().ok())
            .unwrap_or(self.owner);
        let broker_call = header(INTERNAL_CALL_HEADER).as_deref() == Some("1")
            && !headers.contains_key("x-ikenga-caps");
        AccessCtx {
            principal_id,
            via: if broker_call {
                Via::ChildToken
            } else {
                Via::Relayed
            },
            device_id: None,
            tier: if caps == CapSet::ALL {
                Tier::Full
            } else {
                Tier::View
            },
            share: share::from_child_headers(headers),
            share_headers: share::any_share_header(headers),
            caps,
            admin_strength: false,
            meta,
        }
    }
}

/// §1.6 rules 2–4 for one requirement: the class first, then the caps.
pub fn check(ctx: &AccessCtx, req: Requirement) -> Result<(), AccessError> {
    match req.class {
        ArmClass::Operator if !ctx.is_operator() => return Err(AccessError::class(req.class)),
        ArmClass::Internal
            if !(ctx.via == Via::ChildToken && ctx.share.is_none() && !ctx.share_headers) =>
        {
            return Err(AccessError::class(req.class))
        }
        ArmClass::Owner if ctx.share.is_some() => return Err(AccessError::class(req.class)),
        _ => {}
    }
    let missing = ctx.caps.missing(req.caps);
    if !missing.is_empty() {
        return Err(AccessError::missing(missing));
    }
    Ok(())
}

/// Authorize one RPC `cmd` (§1.7): the same check on the T0 daemon, the T1
/// broker (before proxying) and the T1 child (caps from the header).
pub fn authorize(ctx: &AccessCtx, cmd: &str) -> Result<(), AccessError> {
    check(ctx, rpc_requirements::requirement(cmd))
}

/// The requirement of a non-RPC protected route (§1.6 "Non-RPC routes");
/// `None` for `/api/rpc`, which is checked per command.
pub fn route_requirement(path: &str) -> Option<Requirement> {
    if path == "/api/shutdown" {
        Some(Requirement::operator())
    } else if path.starts_with("/ws/pty/") {
        Some(Requirement::owner(&[Cap::Sessions]))
    } else if path.starts_with("/ws/chat/") {
        Some(Requirement::shared(&[Cap::Sessions]))
    } else if path == "/ws/fs" {
        Some(Requirement::shared(&[Cap::Files]))
    } else if path.starts_with("/pkgs/") || path == "/pkgs" {
        Some(Requirement::shared(&[Cap::Files]))
    } else {
        None
    }
}

/// What the `rpc_handler` pre-hook decided (§9.2).
pub enum PreHook {
    /// Serve the arm with the router's state.
    Proceed,
    /// Serve the arm with this narrowed `AppState` clone (share mode,
    /// §4.5.4 — WP-76 builds it in `share::prehook`).
    Narrowed(Arc<crate::server::AppState>),
    /// The access layer answered (a refusal, or a share-mode reroute such as
    /// `actions_*` → `share::actions_dispatch`).
    Answered(crate::server::rpc::RpcResponse),
}

/// The `rpc_handler` pre-hook (§9.2): authorize, then (share mode) narrow.
pub async fn rpc_prehook(
    state: &Arc<crate::server::AppState>,
    ctx: Option<&AccessCtx>,
    cmd: &str,
    args: &serde_json::Value,
) -> PreHook {
    let Some(ctx) = ctx else {
        return PreHook::Answered(rpc::error_response(&AccessError::new(
            Code::Unauthenticated,
            "no access context",
        )));
    };
    if let Err(e) = authorize(ctx, cmd) {
        return PreHook::Answered(rpc::error_response(&e));
    }
    match &ctx.share {
        None => PreHook::Proceed,
        Some(share) => share::prehook(state, ctx, share, cmd, args).await,
    }
}

/// The `rpc_handler` post-hook (§9.2): share filtering (WP-76) and the
/// permission-row annotation (WP-75's `routing::annotate`, the one copy).
pub fn postfilter(
    ctx: Option<&AccessCtx>,
    cmd: &str,
    mut res: crate::server::rpc::RpcResponse,
) -> crate::server::rpc::RpcResponse {
    let Some(ctx) = ctx else { return res };
    if let Some(data) = res.data.as_mut() {
        if let Some(share) = &ctx.share {
            share::filter(ctx, share, cmd, data);
        }
        if cmd == "notifications_list" {
            crate::server::shared::notifications::routing::annotate(ctx, data);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(via: Via, tier: Tier) -> AccessCtx {
        AccessCtx {
            principal_id: PrincipalId::new_v7(),
            admin_strength: AccessCtx::admin_strength_of(&via, tier),
            via,
            device_id: None,
            tier,
            share: None,
            share_headers: false,
            caps: tier.caps(),
            meta: RequestMeta::default(),
        }
    }

    fn device(tier: Tier) -> AccessCtx {
        ctx(
            Via::Device {
                device_id: "d".into(),
            },
            tier,
        )
    }

    #[test]
    fn class_then_caps() {
        let op = ctx(Via::Operator, Tier::Full);
        for cmd in [
            "pty_write",
            "db_exec",
            "permission_relay_put",
            "access_status",
        ] {
            assert!(authorize(&op, cmd).is_ok(), "{cmd}");
        }
        assert_eq!(
            authorize(&op, "share_project_info").unwrap_err().message,
            "class=internal"
        );

        let phone = device(Tier::Dispatch);
        assert!(authorize(&phone, "pty_write").is_ok());
        assert!(authorize(&phone, "fs_write").is_ok());
        let e = authorize(&phone, "pa_actions_commit").unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (Code::Forbidden, "missing=approve")
        );
        assert_eq!(
            authorize(&phone, "permission_relay_take")
                .unwrap_err()
                .message,
            "class=operator"
        );
        let view = device(Tier::View);
        assert_eq!(
            authorize(&view, "pty_write").unwrap_err().message,
            "missing=dispatch"
        );
        assert!(authorize(&view, "fs_read").is_ok());
        // §1.6 rule 2: unmapped → owner, all seven.
        assert_eq!(
            authorize(&device(Tier::Approve), "brand_new_cmd")
                .unwrap_err()
                .code,
            Code::Forbidden
        );
        assert!(authorize(&device(Tier::Full), "brand_new_cmd").is_ok());
    }

    /// §4.5.3 / review F-4: an `internal` arm is refused on a child request
    /// carrying **any** `X-Ikenga-Share-*` header, even one that doesn't
    /// parse into a share (no `X-Ikenga-Share-Project`).
    #[test]
    fn internal_arms_refuse_any_share_header() {
        let child = DaemonAccess::principal_child(AccessOptions::default());
        let mut broker = axum::http::HeaderMap::new();
        broker.insert(INTERNAL_CALL_HEADER, "1".parse().unwrap());
        let plain = child.child_ctx(&broker, RequestMeta::default());
        assert_eq!(plain.via, Via::ChildToken);
        assert!(authorize(&plain, "share_project_info").is_ok());
        for name in ["x-ikenga-share-principal", "x-ikenga-share-role"] {
            let mut h = broker.clone();
            h.insert(name, "x".parse().unwrap());
            let c = child.child_ctx(&h, RequestMeta::default());
            assert!(c.share.is_none(), "{name} alone selects no share");
            assert!(c.share_headers);
            assert_eq!(
                authorize(&c, "share_project_info").unwrap_err().message,
                "class=internal",
                "{name}"
            );
        }
    }

    /// Handover lead L74-3: a per-child-token request the broker relayed is
    /// a non-granting context of its own ([`Via::Relayed`]), never the
    /// broker's internal one — with no caps header (a broker without the
    /// narrower), with empty caps, with full caps, or with the internal-call
    /// marker beside a caps header (a relay always carries one).
    #[test]
    fn a_relayed_request_never_reaches_an_internal_arm() {
        let child = DaemonAccess::principal_child(AccessOptions::default());
        let cases: [&[(&str, &str)]; 6] = [
            &[],
            &[("x-ikenga-caps", "")],
            &[("x-ikenga-caps", "files,sessions,dispatch,approve,manage")],
            &[(INTERNAL_CALL_HEADER, "1"), ("x-ikenga-caps", "")],
            &[(INTERNAL_CALL_HEADER, "true")],
            &[(INTERNAL_CALL_HEADER, "")],
        ];
        for headers in cases {
            let mut h = axum::http::HeaderMap::new();
            for (k, v) in headers {
                h.insert(*k, v.parse().unwrap());
            }
            let c = child.child_ctx(&h, RequestMeta::default());
            assert_eq!(c.via, Via::Relayed, "{headers:?}");
            for cmd in ["share_project_info", "notifications_record_access"] {
                assert_eq!(
                    authorize(&c, cmd).unwrap_err().message,
                    "class=internal",
                    "{cmd} {headers:?}"
                );
            }
        }
    }

    #[test]
    fn owner_class_is_never_reachable_through_a_share() {
        let mut c = ctx(Via::ChildToken, Tier::Full);
        c.caps = CapSet::ALL;
        c.share = Some(ShareCtx {
            project_key: "o/p".into(),
            project_id: "p".into(),
            member_principal_id: None,
            member_device_id: None,
            role: Some(Role::Operator),
            artifact_path: None,
            owner_approval: true,
        });
        assert_eq!(authorize(&c, "db_exec").unwrap_err().message, "class=owner");
        assert!(authorize(&c, "fs_read").is_ok());
        assert_eq!(
            authorize(&c, "share_project_info").unwrap_err().message,
            "class=internal",
            "internal arms refuse any share header"
        );
        c.share = None;
        assert!(authorize(&c, "share_project_info").is_ok());
    }

    #[test]
    fn route_requirements_cover_the_protected_routes() {
        assert_eq!(
            route_requirement("/api/shutdown").unwrap().class,
            ArmClass::Operator
        );
        assert_eq!(
            route_requirement("/ws/pty/abc").unwrap().caps,
            CapSet::of(&[Cap::Sessions])
        );
        assert_eq!(
            route_requirement("/ws/fs").unwrap().caps,
            CapSet::of(&[Cap::Files])
        );
        assert_eq!(
            route_requirement("/pkgs/x/index.html").unwrap().class,
            ArmClass::Shared
        );
        assert!(route_requirement("/api/rpc").is_none());
    }

    /// A-32: a principal child has no store and answers access arms with
    /// `served_by_broker`; its caps come only from the header.
    #[test]
    fn a_principal_child_has_no_store_and_header_caps_only() {
        let child = DaemonAccess::principal_child(AccessOptions::default());
        assert!(child.store().is_none());
        let none = child.child_ctx(&axum::http::HeaderMap::new(), RequestMeta::default());
        assert_eq!(none.caps, CapSet::EMPTY);
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-ikenga-caps", "files,sessions,bogus".parse().unwrap());
        let some = child.child_ctx(&h, RequestMeta::default());
        assert_eq!(some.caps, CapSet::of(&[Cap::Files, Cap::Sessions]));
        assert!(!some.admin_strength);
    }

    #[test]
    fn error_strings_are_code_colon_message() {
        assert_eq!(
            AccessError::missing(CapSet::of(&[Cap::Dispatch])).to_string(),
            "forbidden: missing=dispatch"
        );
        assert_eq!(
            AccessError::not_implemented("WP-75").to_string(),
            "internal: not implemented (WP-75)"
        );
    }

    #[test]
    fn keepalive_counts_holds() {
        let before = keepalive_count();
        let a = KeepAlive::hold();
        let b = KeepAlive::hold();
        assert_eq!(keepalive_count(), before + 2);
        drop((a, b));
        assert_eq!(keepalive_count(), before);
    }
}
