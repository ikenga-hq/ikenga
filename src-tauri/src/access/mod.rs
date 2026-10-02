//! G-ACCESS: capabilities, devices, the access store, the audit chain and
//! the hooks every request passes (`plans/shell-ux-rearchitecture/drafts/
//! access-schema.md`, frozen Round 60).
//!
//! Headless core: compiled into both binaries. Module map (§10.1):
//!
//! | module | what | owner |
//! |---|---|---|
//! | [`caps`], [`rpc_requirements`] | the vocabulary and the route → caps table (§1) | WP-74a (caps frozen; requirements append-only) |
//! | [`ctx`] | `AccessCtx` (§2.3) | WP-74a |
//! | [`store`], [`migrations`] | the access store and its `access` set (§2.5, §8) | WP-74a |
//! | [`devices`], [`sockets`] | device credentials, revoke immediacy (§3.8–§3.11) | WP-74a |
//! | [`http`] | `/access/*` public routes + their `Origin` layer, T0 credential resolution | WP-74a (pairing handlers: 74b; invite handlers: WP-76) |
//! | [`rpc`] | the `access_*` / relay / internal arm group | WP-74a |
//! | [`ws`] | WS handshake and frame checks | WP-74a |
//! | [`audit`] | the hash chain (WP-74a); list/export/reseal/absorb (WP-77) | |
//! | [`routing`] | routing prefs | WP-75 |
//! | [`members`], [`invites`], [`policy`], [`share`] | roles, invites, policies, share confinement | WP-76 |
//!
//! **Enforcement points (§1.7).** T0 daemon: `server::auth_middleware`
//! resolves an [`AccessCtx`] and checks non-RPC routes; `rpc_handler` calls
//! [`authorize_rpc`] (pre-hook) and [`postfilter`] (post-hook); the WS
//! handlers check frames. T1 broker: the same checks run in the R-3 hooks
//! (`server::broker::proxy`) before proxying, and the child re-checks with
//! caps taken only from `X-Ikenga-Caps`.

pub mod audit;
pub mod caps;
#[cfg(test)]
mod caps_ts;
pub mod ctx;
pub mod devices;
#[cfg(test)]
mod e2e_tests;
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
pub mod ws;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::HeaderMap;
use serde_json::Value;

use caps::{ArmClass, CapSet, Role};
use ctx::{AccessCtx, Credential, HostIdentity, ShareCtx};
use sockets::{Registry, SocketControl};
use store::AccessStore;

/// An access arm's result: data, or `"<code>: <message>"` (§9.1).
pub type RpcResult = Result<Value, String>;

/// The stub answer of an arm a later WP fills (§9.2).
pub fn not_implemented(wp: &str) -> String {
    format!("internal: not implemented ({wp})")
}

/// Which process this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The T0 daemon: serves `access_*` from `<data-dir>/access.db`.
    Daemon,
    /// A T1 principal child: no store, no synthetic owner; `access_*` →
    /// `served_by_broker` (§1.7, A-32).
    PrincipalChild,
    /// The T1 broker: serves `access_*` as root from `operator/accounts.db`.
    Broker,
}

/// Every Part B daemon flag (§10.1, review M-5). Defaults: off / unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccessOptions {
    /// `--public-url` / `IKENGA_PUBLIC_URL` (§3.3).
    pub public_url: Option<String>,
    /// `--insecure-cookie` (P-3, R-9): no `Secure` on the device cookie
    /// either. Tier-agnostic.
    pub insecure_cookie: bool,
    /// `--max-accounts N` (P-27); `None` = unlimited. Read by WP-76.
    pub max_accounts: Option<u32>,
    /// `--invite-ttl` days (P-15: default 7, at most 30). Read by WP-76.
    pub invite_ttl_days: Option<u32>,
    /// `--member-invites-create-accounts` (§4.4, N-11). Read by WP-76.
    pub member_invites_create_accounts: bool,
    /// `--allow-origin` values: the `/access/*` `Origin` layer honours them
    /// as the protected routes do.
    pub allowed_origins: Vec<String>,
}

/// The T1 principal directory the broker hands the access arms
/// (`AccessStatus.principal.username` / `isAdmin`).
pub trait PrincipalDirectory: Send + Sync {
    fn lookup<'a>(
        &'a self,
        principal_id: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<(String, bool)>> + Send + 'a>>;
}

/// The access state a router carries (an axum `Extension`, never an
/// `AppState` field — X-2).
pub struct Runtime {
    pub mode: Mode,
    pub store: Option<Arc<AccessStore>>,
    /// The T0 / child socket registry the WS handlers register with.
    pub registry: Arc<Registry>,
    /// What the access arms close sockets through (the T0 registry, or the
    /// broker's).
    pub sockets: Arc<dyn SocketControl>,
    pub options: AccessOptions,
    pub host: HostIdentity,
    pub directory: Option<Arc<dyn PrincipalDirectory>>,
    /// Keep-alive leases (§2.5 M-3): open pairing sessions (WP-74b) and
    /// pending relay asks (WP-75) keep the desktop's daemon from idling out.
    keepalive: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("mode", &self.mode)
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// Held while something must keep the daemon alive; dropping releases it.
pub struct KeepAlive(Arc<AtomicUsize>);

impl Drop for KeepAlive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Runtime {
    fn build(mode: Mode, store: Option<Arc<AccessStore>>, options: AccessOptions) -> Self {
        let registry = Registry::new();
        Runtime {
            mode,
            store,
            sockets: registry.clone(),
            registry,
            options,
            host: HostIdentity::from_env(),
            directory: None,
            keepalive: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// A T0 daemon with no access store (no `--data-dir`, or a test router):
    /// the operator bearer still works; access arms answer
    /// `store_unavailable`; no device credential resolves.
    pub fn none() -> Self {
        Self::build(Mode::Daemon, None, AccessOptions::default())
    }

    /// The single-tenant daemon's runtime (§2.5, §1.7). A principal child
    /// never opens or creates `access.db` (A-32); a T0 daemon with a data
    /// dir opens (creating) it. A store that can't be opened leaves the
    /// daemon serving with no store: access arms answer `store_unavailable`
    /// and no device credential resolves (fail closed).
    pub async fn for_daemon(
        data_dir: Option<&Path>,
        principal_child: bool,
        options: AccessOptions,
    ) -> Self {
        if principal_child {
            return Self::build(Mode::PrincipalChild, None, options);
        }
        let store = match data_dir {
            None => None,
            Some(dir) => match AccessStore::open_t0(dir).await {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::error!(
                        "access store {}: {e:#} — pairing and device credentials are off",
                        dir.join(store::T0_FILE).display()
                    );
                    None
                }
            },
        };
        Self::build(Mode::Daemon, store, options)
    }

    /// The T1 broker's runtime over its `accounts.db` store and socket
    /// registry.
    pub fn for_broker(
        store: Arc<AccessStore>,
        sockets: Arc<dyn SocketControl>,
        directory: Arc<dyn PrincipalDirectory>,
        options: AccessOptions,
    ) -> Self {
        let mut rt = Self::build(Mode::Broker, Some(store), options);
        rt.sockets = sockets;
        rt.directory = Some(directory);
        rt
    }

    /// Take a keep-alive lease (§2.5).
    pub fn keepalive(&self) -> KeepAlive {
        self.keepalive.fetch_add(1, Ordering::SeqCst);
        KeepAlive(self.keepalive.clone())
    }

    /// What the idle watcher counts besides PTYs (§2.5 M-3): leases plus
    /// live device sockets.
    pub fn activity(&self) -> usize {
        let host = self
            .store
            .as_ref()
            .and_then(|s| s.host_device_id.as_deref());
        self.keepalive.load(Ordering::SeqCst) + self.registry.device_sockets(host)
    }

    /// The operator bearer's context on this daemon.
    pub fn operator_ctx(&self) -> AccessCtx {
        let (owner, host) = match &self.store {
            Some(s) => (s.owner, s.host_device_id.clone()),
            None => (None, None),
        };
        AccessCtx::operator(owner, host)
    }
}

/// `X-Ikenga-*` request headers. Narrowing only, read only on per-child-
/// token requests (§4.5.3); everywhere else they are stripped.
pub const CAPS_HEADER: &str = "x-ikenga-caps";
pub const SHARE_PROJECT_HEADER: &str = "x-ikenga-share-project";
pub const SHARE_PRINCIPAL_HEADER: &str = "x-ikenga-share-principal";
pub const SHARE_DEVICE_HEADER: &str = "x-ikenga-share-device";
pub const SHARE_ROLE_HEADER: &str = "x-ikenga-share-role";
pub const SHARE_ARTIFACT_HEADER: &str = "x-ikenga-share-artifact";
pub const SHARE_POLICY_HEADER: &str = "x-ikenga-share-policy";

/// Remove every `X-Ikenga-*` header (A-29: client-supplied ones never
/// reach a check).
pub fn strip_ikenga_headers(headers: &mut HeaderMap) {
    let names: Vec<_> = headers
        .keys()
        .filter(|k| k.as_str().starts_with("x-ikenga-"))
        .cloned()
        .collect();
    for n in names {
        headers.remove(n);
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The T1 child's context from the broker's headers (§1.7, §4.5.3): caps
/// **only** from `X-Ikenga-Caps` (absent → none); a share only when the
/// share headers are complete and well-formed. Anything malformed grants
/// nothing.
pub fn child_ctx(headers: &HeaderMap) -> AccessCtx {
    let caps = header(headers, CAPS_HEADER).and_then(CapSet::parse_header);
    let any_share = headers
        .keys()
        .any(|k| k.as_str().starts_with("x-ikenga-share-"));
    if !any_share {
        return AccessCtx::child(caps, None);
    }
    let share = (|| {
        let project_id = header(headers, SHARE_PROJECT_HEADER)?.to_string();
        let role = Role::parse(header(headers, SHARE_ROLE_HEADER)?)?;
        if role == Role::Owner || project_id.is_empty() {
            return None;
        }
        let policy = header(headers, SHARE_POLICY_HEADER);
        if policy.is_some_and(|p| p != "owner-approval") {
            return None;
        }
        Some(ShareCtx {
            project_key: String::new(),
            project_id,
            member_principal_id: header(headers, SHARE_PRINCIPAL_HEADER).map(str::to_string),
            member_device_id: header(headers, SHARE_DEVICE_HEADER)
                .filter(|d| *d != "-")
                .map(str::to_string),
            role,
            artifact_path: header(headers, SHARE_ARTIFACT_HEADER).map(str::to_string),
            owner_approval: policy.is_some(),
        })
    })();
    match share {
        Some(s) => AccessCtx::child(caps, Some(s)),
        // Present but malformed: no caps at all.
        None => AccessCtx::child(Some(CapSet::EMPTY), None),
    }
}

/// The `rpc_handler` pre-hook (§1.7, §9.2): class first, then caps.
/// `Err` is the RPC error string.
pub fn authorize_rpc(ctx: &AccessCtx, cmd: &str) -> Result<(), String> {
    let req = rpc_requirements::requirement(cmd);
    match req.class {
        // Per-command rules live with the arms (`access::rpc`).
        ArmClass::Access => Ok(()),
        ArmClass::Internal => {
            if ctx.via == Credential::ChildToken && ctx.share.is_none() {
                Ok(())
            } else {
                Err("forbidden: class=internal".into())
            }
        }
        ArmClass::Operator => {
            if ctx.is_operator() {
                Ok(())
            } else {
                Err("forbidden: class=operator".into())
            }
        }
        ArmClass::Owner | ArmClass::Shared => ws::check_requirement(ctx, req),
    }
}

/// The pre-hook's narrowing step: for a share request, the narrowed state
/// (WP-76's `share::narrow_state`). Run after [`authorize_rpc`].
pub fn prehook_state(
    ctx: &AccessCtx,
    cmd: &str,
    state: &Arc<crate::server::AppState>,
) -> Result<Option<Arc<crate::server::AppState>>, String> {
    share::narrow_state(ctx, cmd, state)
}

/// The `rpc_handler` post-hook (§9.2): share filtering (WP-76) and the
/// permission read model (WP-75's `routing::annotate`). Runs on successful
/// results only; an `Err` replaces the response.
pub fn postfilter(ctx: &AccessCtx, cmd: &str, data: &mut Value) -> Result<(), String> {
    share::filter(ctx, cmd, data)?;
    crate::server::shared::notifications::routing::annotate(ctx, cmd, data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::{Cap, Tier};
    use crate::executor::PrincipalId;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        h
    }

    /// §1.7: the child takes caps only from `X-Ikenga-Caps`; malformed share
    /// headers grant nothing.
    #[test]
    fn the_child_reads_narrowing_headers_only() {
        assert_eq!(child_ctx(&headers(&[])).caps, CapSet::EMPTY);
        assert_eq!(
            child_ctx(&headers(&[("x-ikenga-caps", "files,sessions")])).caps,
            Tier::View.caps()
        );
        assert_eq!(
            child_ctx(&headers(&[("x-ikenga-caps", "files,superuser")])).caps,
            CapSet::EMPTY
        );
        let shared = child_ctx(&headers(&[
            ("x-ikenga-caps", "files"),
            ("x-ikenga-share-project", "royalti-co"),
            ("x-ikenga-share-role", "reviewer"),
            ("x-ikenga-share-device", "-"),
            ("x-ikenga-share-policy", "owner-approval"),
        ]));
        let share = shared.share.unwrap();
        assert_eq!(share.role, Role::Reviewer);
        assert!(share.owner_approval);
        assert_eq!(share.member_device_id, None);
        for bad in [
            vec![
                ("x-ikenga-caps", "files"),
                ("x-ikenga-share-role", "reviewer"),
            ],
            vec![
                ("x-ikenga-caps", "files"),
                ("x-ikenga-share-project", "p"),
                ("x-ikenga-share-role", "owner"),
            ],
        ] {
            let c = child_ctx(&headers(&bad));
            assert_eq!(c.caps, CapSet::EMPTY, "{bad:?}");
            assert!(c.share.is_none());
        }
        let mut h = headers(&[("x-ikenga-caps", "files"), ("x-other", "1")]);
        strip_ikenga_headers(&mut h);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn rpc_authorization_checks_class_then_caps() {
        let p = PrincipalId::new_v7();
        let view = AccessCtx::device(p, "d".into(), Tier::View, 0);
        let op = AccessCtx::operator(Some(p), None);
        assert!(authorize_rpc(&view, "fs_read").is_ok());
        assert_eq!(
            authorize_rpc(&view, "fs_write").unwrap_err(),
            "forbidden: missing=dispatch"
        );
        assert!(authorize_rpc(&view, "db_exec")
            .unwrap_err()
            .starts_with("forbidden: missing="));
        // Unmapped: all seven + owner — a full device passes, a view one not.
        assert!(authorize_rpc(&view, "brand_new_arm").is_err());
        assert!(authorize_rpc(&op, "brand_new_arm").is_ok());
        // Operator class: the bearer only, not even a full device.
        let full = AccessCtx::device(p, "d".into(), Tier::Full, 0);
        assert_eq!(
            authorize_rpc(&full, "permission_relay_put").unwrap_err(),
            "forbidden: class=operator"
        );
        assert!(authorize_rpc(&op, "permission_relay_put").is_ok());
        // Internal: the per-child token without a share only.
        assert!(authorize_rpc(&op, "share_project_info").is_err());
        assert!(authorize_rpc(&AccessCtx::child(None, None), "share_project_info").is_ok());
        // Owner class through a share: refused whatever the caps.
        let mut shared = AccessCtx::child(Some(CapSet::ALL), None);
        shared.share = Some(ShareCtx {
            project_key: String::new(),
            project_id: "p".into(),
            member_principal_id: None,
            member_device_id: None,
            role: Role::Operator,
            artifact_path: None,
            owner_approval: true,
        });
        assert_eq!(
            authorize_rpc(&shared, "pty_spawn").unwrap_err(),
            "forbidden: class=owner"
        );
        assert!(authorize_rpc(&shared, "fs_read").is_ok());
        assert!(!view.caps.contains(Cap::Approve));
    }

    /// A-32 (child half): a principal child creates no `access.db`.
    #[tokio::test]
    async fn a_principal_child_opens_no_access_store() {
        let dir = tempfile::tempdir().unwrap();
        let rt = Runtime::for_daemon(Some(dir.path()), true, AccessOptions::default()).await;
        assert_eq!(rt.mode, Mode::PrincipalChild);
        assert!(rt.store.is_none());
        assert!(!dir.path().join(store::T0_FILE).exists());
        let rt = Runtime::for_daemon(Some(dir.path()), false, AccessOptions::default()).await;
        assert!(rt.store.is_some());
        assert!(dir.path().join(store::T0_FILE).exists());
    }

    /// A-28 / A-32 code structure: no T0 path builds a G-PRINCIPAL
    /// `Principal`, and nothing outside the daemon/broker opens the store.
    #[test]
    fn code_structure_keeps_t0_principal_free_and_the_store_daemon_only() {
        for (name, src) in [
            ("access/mod.rs", include_str!("mod.rs")),
            ("access/ctx.rs", include_str!("ctx.rs")),
            ("access/store.rs", include_str!("store.rs")),
            ("access/devices.rs", include_str!("devices.rs")),
            ("access/http.rs", include_str!("http.rs")),
            ("access/rpc.rs", include_str!("rpc.rs")),
        ] {
            let needle = ["PrincipalCtx", " {"].concat();
            assert!(!src.contains(&needle), "{name} builds a PrincipalCtx");
            let needle = ["Principal", " {"].concat();
            assert!(!src.contains(&needle), "{name} builds a Principal");
        }
        let opener = ["AccessStore::", "open"].concat();
        for (name, src) in [
            ("lib.rs", include_str!("../lib.rs")),
            ("commands/access.rs", include_str!("../commands/access.rs")),
            (
                "pty/daemon_client.rs",
                include_str!("../pty/daemon_client.rs"),
            ),
        ] {
            assert!(
                !src.contains(&opener),
                "{name} opens the access store (P-20, A-32)"
            );
        }
    }
}
