//! Per-principal permission-routing preference (G-ACCESS §5.1, DEC-79;
//! WP-75). WP-74a registered `access_routing_get` / `access_routing_set`,
//! created the `routing_prefs` table (§8.2) and calls [`routing_ok`]
//! everywhere effective caps are computed; this file fills the bodies.
//!
//! | `mode` | Meaning | `routing_ok(ctx)` |
//! |---|---|---|
//! | `any_approve` (default) | any device of mine whose caps include `approve` | true |
//! | `this_device` | only the named `device_id` | `ctx.device_id == pref.device_id` |
//!
//! `routing_ok` removes `approve` from the effective caps (§1.4), so the
//! same check covers `permission_decide`, the `pa_actions_*` mutations and
//! `can_decide` on rows. A device can never approve beyond its own tier: the
//! tier term of §1.4 caps it whatever the preference says.

use serde_json::{json, Value};
use sqlx::{Connection, Row, SqliteConnection};

use super::audit::Event;
use super::caps::{Cap, Tier};
use super::ctx::AccessCtx;
use super::devices;
use super::rpc::Env;
use super::sockets::Close;
use super::store::{AccessStore, StoreTier};
use super::{AccessError, Code};
use crate::executor::PrincipalId;

/// `routing_prefs.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    AnyApprove,
    ThisDevice,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::AnyApprove => "any_approve",
            Mode::ThisDevice => "this_device",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "any_approve" => Some(Mode::AnyApprove),
            "this_device" => Some(Mode::ThisDevice),
            _ => None,
        }
    }
}

/// A principal's preference (absent row = the default `any_approve`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pref {
    pub mode: Mode,
    pub device_id: Option<String>,
}

impl Default for Pref {
    fn default() -> Self {
        Pref {
            mode: Mode::AnyApprove,
            device_id: None,
        }
    }
}

impl Pref {
    /// §5.1's `routing_ok` column for a credential on `device_id`.
    pub fn admits(&self, device_id: Option<&str>) -> bool {
        match self.mode {
            Mode::AnyApprove => true,
            // A password session (no device) never satisfies it.
            Mode::ThisDevice => device_id.is_some() && device_id == self.device_id.as_deref(),
        }
    }
}

pub async fn read_pref(
    conn: &mut SqliteConnection,
    principal_id: &str,
) -> Result<Pref, sqlx::Error> {
    let row = sqlx::query("SELECT mode, device_id FROM routing_prefs WHERE principal_id = ?")
        .bind(principal_id)
        .fetch_optional(&mut *conn)
        .await?;
    let Some(r) = row else {
        return Ok(Pref::default());
    };
    let mode: String = r.try_get("mode")?;
    Ok(Pref {
        // The CHECK keeps `mode` closed; anything else fails closed to "this
        // device", naming no device: nobody may answer.
        mode: Mode::parse(&mode).unwrap_or(Mode::ThisDevice),
        device_id: r.try_get("device_id")?,
    })
}

/// §5.1 `routing_ok(ctx)`: may this credential answer the principal's
/// permission asks? `false` removes `approve` from the effective caps
/// (§1.4).
///
/// Called **once per request / WS handshake**, wherever effective caps are
/// computed: T0 `DaemonAccess::{operator_ctx, device_ctx}` and T1
/// `T1Access::access_ctx`. `device_id` is the credential's device: the host
/// device for the T0 operator bearer, the paired device for a grant, `None`
/// for a T1 password session. `store` is `None` when the access store is
/// unavailable: then no preference can exist (no store, no pairing), and the
/// default `any_approve` holds. A store **error** fails closed (`false`).
pub async fn routing_ok(
    store: Option<&AccessStore>,
    principal_id: &PrincipalId,
    device_id: Option<&str>,
) -> bool {
    let Some(store) = store else {
        return true;
    };
    let pref = async {
        let mut conn = store.pool().acquire().await?;
        read_pref(&mut conn, &principal_id.to_string()).await
    }
    .await;
    match pref {
        Ok(p) => p.admits(device_id),
        Err(e) => {
            tracing::warn!("routing_ok: routing_prefs unreadable, failing closed: {e}");
            false
        }
    }
}

fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

/// `access_routing_get` / `access_routing_set` (§9.1).
pub async fn dispatch(
    env: &Env<'_>,
    ctx: &AccessCtx,
    cmd: &str,
    args: &Value,
) -> Result<Value, AccessError> {
    match cmd {
        "access_routing_get" => get(store(env)?, ctx).await,
        "access_routing_set" => set(env, store(env)?, ctx, args).await,
        other => Err(AccessError::new(
            Code::NotFound,
            format!("no routing command `{other}`"),
        )),
    }
}

/// `{mode, deviceId, deviceName}` — `deviceName` (additive to §9.1) lets a
/// read-only card say "Answer on ned-desktop (this device only)" (§5.7).
async fn view(conn: &mut SqliteConnection, pref: &Pref) -> Result<Value, AccessError> {
    let name = match &pref.device_id {
        Some(id) => devices::get(conn, id)
            .await
            .map_err(AccessError::internal)?
            .filter(|d| !d.is_revoked())
            .map(|d| d.name),
        None => None,
    };
    Ok(json!({
        "mode": pref.mode.as_str(),
        "deviceId": pref.device_id,
        "deviceName": name,
    }))
}

/// Any authenticated caller, for its own principal (§9.1 "any").
async fn get(store: &AccessStore, ctx: &AccessCtx) -> Result<Value, AccessError> {
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let pref = read_pref(&mut conn, &ctx.principal_id.to_string())
        .await
        .map_err(AccessError::internal)?;
    view(&mut conn, &pref).await
}

/// `access_routing_set {mode, deviceId?}` (§5.1): `admin_strength`;
/// audited `routing.changed {mode, device_id?}` in the same transaction
/// (A-18); then the principal's device sockets that held `approve` close
/// 4403 so they reconnect with the new caps.
async fn set(
    env: &Env<'_>,
    store: &AccessStore,
    ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    if !ctx.admin_strength {
        return Err(AccessError::forbidden_admin());
    }
    if ctx.share.is_some() {
        // A preference is the principal's own; never set through a share.
        return Err(AccessError::class(super::caps::ArmClass::Owner));
    }
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .and_then(Mode::parse)
        .ok_or_else(|| {
            AccessError::new(
                Code::InvalidRequest,
                "mode must be any_approve | this_device",
            )
        })?;
    let principal = ctx.principal_id.to_string();
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let mut tx = conn
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(AccessError::internal)?;

    let device_id = match mode {
        Mode::AnyApprove => None,
        Mode::ThisDevice => {
            // Default: the device this request came from (the host for the
            // desktop: D-05's "This device only" means ned-desktop).
            let id = args
                .get("deviceId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| ctx.device_id.clone())
                .ok_or_else(|| {
                    AccessError::new(
                        Code::InvalidRequest,
                        "a password session has no device: name a paired device (deviceId)",
                    )
                })?;
            let row = devices::get(&mut tx, &id)
                .await
                .map_err(AccessError::internal)?
                .filter(|d| d.principal_id == principal && !d.is_revoked())
                .ok_or_else(|| AccessError::new(Code::NotFound, "no such device"))?;
            if env.tier == StoreTier::T1 && row.is_host() {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "on a server the named device must be a paired device",
                ));
            }
            if !row.tier.caps().contains(Cap::Approve) {
                // §5.1: capped by grants — naming a device that can't approve
                // would leave nobody able to answer.
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    format!(
                        "{} is {} and can't approve; raise it to Dispatch + approve first",
                        row.name,
                        row.tier.label().0
                    ),
                ));
            }
            Some(row.device_id)
        }
    };

    let before = read_pref(&mut tx, &principal)
        .await
        .map_err(AccessError::internal)?;
    let after = Pref { mode, device_id };
    if before == after {
        let v = view(&mut tx, &after).await?;
        tx.commit().await.map_err(AccessError::internal)?;
        return Ok(v);
    }
    sqlx::query(
        "INSERT INTO routing_prefs (principal_id, mode, device_id, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(principal_id) DO UPDATE SET mode = excluded.mode, \
         device_id = excluded.device_id, updated_at = excluded.updated_at",
    )
    .bind(&principal)
    .bind(after.mode.as_str())
    .bind(&after.device_id)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&mut *tx)
    .await
    .map_err(AccessError::internal)?;
    let mut detail = json!({ "mode": after.mode.as_str() });
    if let Some(d) = &after.device_id {
        detail["device_id"] = json!(d);
    }
    let mut ev = Event::by("routing.changed", ctx)
        .subject_principal(principal.clone())
        .detail(detail);
    if let Some(d) = &after.device_id {
        ev = ev.subject_device(d.clone());
    }
    let head = store
        .chain()
        .append(&mut tx, &ev)
        .await
        .map_err(audit_err)?;
    let out = view(&mut tx, &after).await?;
    let devices = devices::list_active(&mut tx, &principal)
        .await
        .map_err(AccessError::internal)?;
    tx.commit().await.map_err(AccessError::internal)?;
    store.chain().committed(head);

    // §5.1: close the sockets that held `approve` under the old preference.
    // Effective caps are computed per request, so a stale socket could never
    // decide anything; the close makes clients re-read their caps.
    for d in devices
        .iter()
        .filter(|d| !d.is_host() && d.tier.caps().contains(Cap::Approve))
        .filter(|d| before.admits(Some(&d.device_id)))
    {
        env.sockets.close_device(&d.device_id, Close::CAPS_CHANGED);
    }
    Ok(out)
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

/// Whether a tier can ever approve (the D-05 note's "a device can never
/// approve more than its own capability allows").
pub fn tier_can_approve(tier: Tier) -> bool {
    tier.caps().contains(Cap::Approve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::ctx::{RequestMeta, Via};
    use crate::access::devices::tests::{operator_ctx, pair};
    use crate::access::rpc::{dispatch as rpc_dispatch, PrincipalInfo};
    use crate::access::sockets::Registry;
    use crate::access::{CapSet, DaemonAccess};

    fn env<'a>(store: &'a AccessStore, sockets: &'a Registry) -> Env<'a> {
        Env {
            tier: StoreTier::T0,
            store: Some(store),
            pairing: None,
            sockets,
            principal: PrincipalInfo {
                username: "ned".into(),
                is_admin: false,
            },
            public_url: None,
            insecure_cookie: false,
        }
    }

    fn device_ctx(store: &AccessStore, device_id: &str, tier: Tier) -> AccessCtx {
        let via = Via::Device {
            device_id: device_id.into(),
        };
        AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            admin_strength: AccessCtx::admin_strength_of(&via, tier),
            via,
            device_id: Some(device_id.into()),
            tier,
            share: None,
            share_headers: false,
            caps: tier.caps(),
            meta: RequestMeta::default(),
        }
    }

    async fn audit_kinds(store: &AccessStore) -> Vec<(String, String)> {
        sqlx::query("SELECT kind, detail FROM audit_events ORDER BY seq")
            .fetch_all(store.pool())
            .await
            .unwrap()
            .iter()
            .map(|r| (r.get("kind"), r.get("detail")))
            .collect()
    }

    #[tokio::test]
    async fn default_is_any_approve_and_no_store_admits() {
        let store = AccessStore::memory_t0().await;
        let owner = store.meta().owner_principal_id.unwrap();
        assert!(routing_ok(Some(&store), &owner, Some("x")).await);
        assert!(routing_ok(Some(&store), &owner, None).await);
        assert!(routing_ok(None, &owner, None).await);
        let reg = Registry::new();
        let v = rpc_dispatch(
            &env(&store, &reg),
            &operator_ctx(&store),
            "access_routing_get",
            &json!({}),
        )
        .await
        .unwrap();
        assert_eq!(
            v,
            json!({"mode": "any_approve", "deviceId": null, "deviceName": null})
        );
    }

    /// §5.1 / A-23 (the caps half): `this_device` from the desktop names the
    /// host; then only the host's ctx keeps `approve`; audited; the phone's
    /// approve sockets close 4403.
    #[tokio::test]
    async fn this_device_from_the_desktop_names_the_host() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let (phone, _) = pair(&store, Tier::Approve).await;
        let mut sock = reg.register(Some(phone.device_id.clone()));
        let op = operator_ctx(&store);
        let e = env(&store, &reg);
        let v = rpc_dispatch(
            &e,
            &op,
            "access_routing_set",
            &json!({"mode": "this_device"}),
        )
        .await
        .unwrap();
        let host = store.meta().host_device_id.clone().unwrap();
        assert_eq!(v["mode"], "this_device");
        assert_eq!(v["deviceId"], json!(host));
        assert!(v["deviceName"].is_string());
        assert_eq!(sock.closed.try_recv().unwrap().code, 4403);

        let owner = store.meta().owner_principal_id.unwrap();
        assert!(routing_ok(Some(&store), &owner, Some(&host)).await);
        assert!(!routing_ok(Some(&store), &owner, Some(&phone.device_id)).await);

        // The daemon's own ctx builders apply it (§1.4).
        let access = DaemonAccess::with_store(store.clone());
        let op_ctx = access.operator_ctx(RequestMeta::default()).await;
        assert!(op_ctx.caps.contains(Cap::Approve));
        let phone_ctx = access.device_ctx(&phone, RequestMeta::default()).await;
        assert!(!phone_ctx.caps.contains(Cap::Approve));
        assert!(phone_ctx.caps.contains(Cap::Dispatch));
        // …and the pre-hook answers `routing_refused` for it (A-23).
        let err = crate::access::authorize(&phone_ctx, "permission_decide").unwrap_err();
        assert_eq!(err.code, Code::RoutingRefused);
        let err = crate::access::authorize(&phone_ctx, "pa_actions_commit").unwrap_err();
        assert_eq!(err.code, Code::RoutingRefused);

        let kinds = audit_kinds(&store).await;
        let last = kinds.last().unwrap();
        assert_eq!(last.0, "routing.changed");
        assert!(last.1.contains("\"mode\":\"this_device\""), "{}", last.1);

        // Back to any: the phone approves again; no-op sets don't audit.
        rpc_dispatch(
            &e,
            &op,
            "access_routing_set",
            &json!({"mode": "any_approve"}),
        )
        .await
        .unwrap();
        let n = audit_kinds(&store).await.len();
        rpc_dispatch(
            &e,
            &op,
            "access_routing_set",
            &json!({"mode": "any_approve"}),
        )
        .await
        .unwrap();
        assert_eq!(audit_kinds(&store).await.len(), n);
        assert!(routing_ok(Some(&store), &owner, Some(&phone.device_id)).await);
    }

    #[tokio::test]
    async fn set_needs_admin_strength_and_an_approving_device() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let (view_dev, _) = pair(&store, Tier::View).await;
        let (approve_dev, _) = pair(&store, Tier::Approve).await;

        // Below `full`, a device can't change the preference.
        let low = device_ctx(&store, &approve_dev.device_id, Tier::Approve);
        let err = rpc_dispatch(
            &e,
            &low,
            "access_routing_set",
            &json!({"mode": "this_device"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::Forbidden);
        // …but may read it.
        assert!(rpc_dispatch(&e, &low, "access_routing_get", &json!({}))
            .await
            .is_ok());

        let op = operator_ctx(&store);
        for (args, code) in [
            (json!({"mode": "sometimes"}), Code::InvalidRequest),
            (
                json!({"mode": "this_device", "deviceId": view_dev.device_id}),
                Code::InvalidRequest,
            ),
            (
                json!({"mode": "this_device", "deviceId": "nope"}),
                Code::NotFound,
            ),
        ] {
            assert_eq!(
                rpc_dispatch(&e, &op, "access_routing_set", &args)
                    .await
                    .unwrap_err()
                    .code,
                code,
                "{args}"
            );
        }
        let v = rpc_dispatch(
            &e,
            &op,
            "access_routing_set",
            &json!({"mode": "this_device", "deviceId": approve_dev.device_id}),
        )
        .await
        .unwrap();
        assert_eq!(v["deviceId"], json!(approve_dev.device_id));
        let owner = store.meta().owner_principal_id.unwrap();
        assert!(routing_ok(Some(&store), &owner, Some(&approve_dev.device_id)).await);
        let host = store.meta().host_device_id.clone().unwrap();
        assert!(!routing_ok(Some(&store), &owner, Some(&host)).await);
        // A password session never satisfies `this_device`.
        assert!(!routing_ok(Some(&store), &owner, None).await);
        // A revoked named device leaves nobody able to answer; its name is gone.
        crate::access::devices::revoke(&store, &op, &approve_dev.device_id)
            .await
            .unwrap();
        let v = rpc_dispatch(&e, &op, "access_routing_get", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["deviceName"], Value::Null);
        let _ = CapSet::EMPTY;
    }

    #[test]
    fn pref_admits() {
        let any = Pref::default();
        assert!(any.admits(None) && any.admits(Some("x")));
        let here = Pref {
            mode: Mode::ThisDevice,
            device_id: Some("d".into()),
        };
        assert!(here.admits(Some("d")));
        assert!(!here.admits(Some("e")));
        assert!(!here.admits(None));
        assert!(tier_can_approve(Tier::Approve) && !tier_can_approve(Tier::Dispatch));
    }
}
