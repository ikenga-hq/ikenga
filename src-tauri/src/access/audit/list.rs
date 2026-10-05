//! The audit read surface (G-ACCESS §6.7) and the desktop-local writer
//! (§6.5), WP-77:
//!
//! * `access_audit_list {filter, before?, limit?}` → `{rows, nextBefore}`,
//!   newest first, paged by `seq`;
//! * `access_audit_verify {}` → `{ok, rows, headHash, brokenAtSeq?}`: the
//!   reseal-aware full walk (§6.4), recorded as `audit.verified`;
//! * `access_audit_record_local {kind, target, detail?}` → `{}`: the T0
//!   operator bearer only (the desktop), and only `app.locked`,
//!   `app.unlocked`, `vault.locked`, `vault.unlocked`, `permission.decided`
//!   and `permission.refused`.
//!
//! **Visibility (§6.7).** Reading needs effective `settings` in your own
//! workspace (a `full` device or a session; P-14) — never through a share.
//! On T0 the owner sees everything. On T1 you see rows where you are the
//! actor or the subject, plus rows whose `project_key` is one of your
//! projects; `is_admin` adds nothing (operator-wide audit is the root CLI).
//! The filter narrows inside that set; it never widens it.

use serde_json::{json, Map, Value};
use sqlx::{Row, SqliteConnection};

use super::chain::{AppendError, StoredRow, COLUMNS};
use super::{record, static_kind, Category, Event};
use crate::access::caps::{Cap, CapSet};
use crate::access::ctx::AccessCtx;
use crate::access::rpc::Env;
use crate::access::store::{AccessStore, StoreTier};
use crate::access::{AccessError, Code};

/// The six kinds `access_audit_record_local` accepts (§6.5, §9.1).
pub const LOCAL_KINDS: [&str; 6] = [
    "app.locked",
    "app.unlocked",
    "vault.locked",
    "vault.unlocked",
    "permission.decided",
    "permission.refused",
];

pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;
/// `target` and the filter strings are display text, not payloads.
const MAX_TEXT: usize = 200;
/// A desktop-local `detail` is names and ids only (§6.2).
const MAX_DETAIL_BYTES: usize = 2048;

/// Who may see which rows (§6.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    /// T0's owner, and the root CLI: everything.
    All,
    /// T1: rows this principal acted in or is the subject of, and rows of
    /// projects it owns.
    Principal(String),
}

/// `access_audit_list`'s / `_export`'s `filter` (§9.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// A principal: the actor **or** the subject.
    pub who: Option<String>,
    /// A device: the actor's device **or** the subject device.
    pub device: Option<String>,
    pub category: Option<Category>,
    /// Free text over kind, target, detail, principal and device ids.
    pub q: Option<String>,
    pub project_key: Option<String>,
}

fn parse_category(s: &str) -> Option<Category> {
    Some(match s {
        "permission" => Category::Permission,
        "dispatch" => Category::Dispatch,
        "access" => Category::Access,
        "pairing" => Category::Pairing,
        "people" => Category::People,
        _ => return None,
    })
}

impl Filter {
    /// Parse `args.filter` (absent or `null` = no filter). Unknown keys are
    /// ignored; a wrong type is `invalid_request`.
    pub fn parse(v: Option<&Value>) -> Result<Self, AccessError> {
        let obj = match v {
            None | Some(Value::Null) => return Ok(Self::default()),
            Some(Value::Object(o)) => o,
            Some(_) => {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "`filter` must be an object",
                ))
            }
        };
        let text = |key: &str| -> Result<Option<String>, AccessError> {
            match obj.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => {
                    let s = s.trim();
                    if s.is_empty() {
                        Ok(None)
                    } else if s.chars().count() > MAX_TEXT {
                        Err(AccessError::new(
                            Code::InvalidRequest,
                            format!("`filter.{key}` is too long"),
                        ))
                    } else {
                        Ok(Some(s.to_string()))
                    }
                }
                Some(_) => Err(AccessError::new(
                    Code::InvalidRequest,
                    format!("`filter.{key}` must be a string"),
                )),
            }
        };
        let category = match text("category")? {
            None => None,
            Some(c) => Some(parse_category(&c).ok_or_else(|| {
                AccessError::new(
                    Code::InvalidRequest,
                    "`filter.category` must be permission|dispatch|access|pairing|people",
                )
            })?),
        };
        Ok(Self {
            who: text("who")?,
            device: text("device")?,
            category,
            q: text("q")?,
            project_key: text("projectKey")?,
        })
    }

    /// The CLI's flags (`ikenga-server audit export`), checked like the RPC's.
    pub fn from_parts(
        who: Option<String>,
        device: Option<String>,
        category: Option<String>,
        q: Option<String>,
        project_key: Option<String>,
    ) -> Result<Self, AccessError> {
        Self::parse(Some(&json!({
            "who": who,
            "device": device,
            "category": category,
            "q": q,
            "projectKey": project_key,
        })))
    }

    /// The filter as the wire spells it (export manifests, `audit.exported`).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: &Option<String>| {
            if let Some(v) = v {
                m.insert(k.into(), json!(v));
            }
        };
        put("who", &self.who);
        put("device", &self.device);
        put("q", &self.q);
        put("projectKey", &self.project_key);
        if let Some(c) = self.category {
            m.insert("category".into(), json!(c.as_str()));
        }
        Value::Object(m)
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A bind value for the dynamic `WHERE`.
#[derive(Debug, Clone)]
pub(crate) enum Bind {
    S(String),
    I(i64),
}

/// `LIKE` with `\` escapes.
fn like_pattern(q: &str) -> String {
    let mut p = String::with_capacity(q.len() + 2);
    p.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

/// `WHERE …` for `filter` under `vis`, rows with `seq < before`.
pub(crate) fn where_clause(
    filter: &Filter,
    vis: &Visibility,
    before: Option<i64>,
) -> (String, Vec<Bind>) {
    let mut parts: Vec<String> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();
    if let Visibility::Principal(p) = vis {
        parts.push(
            "(principal_id = ? OR subject_principal_id = ? OR \
             (project_key IS NOT NULL AND substr(project_key, 1, ?) = ?))"
                .into(),
        );
        let prefix = format!("{p}/");
        binds.push(Bind::S(p.clone()));
        binds.push(Bind::S(p.clone()));
        binds.push(Bind::I(prefix.chars().count() as i64));
        binds.push(Bind::S(prefix));
    }
    if let Some(who) = &filter.who {
        parts.push("(principal_id = ? OR subject_principal_id = ?)".into());
        binds.push(Bind::S(who.clone()));
        binds.push(Bind::S(who.clone()));
    }
    if let Some(device) = &filter.device {
        parts.push("(device_id = ? OR subject_device_id = ?)".into());
        binds.push(Bind::S(device.clone()));
        binds.push(Bind::S(device.clone()));
    }
    if let Some(c) = filter.category {
        parts.push("category = ?".into());
        binds.push(Bind::S(c.as_str().into()));
    }
    if let Some(pk) = &filter.project_key {
        parts.push("project_key = ?".into());
        binds.push(Bind::S(pk.clone()));
    }
    if let Some(q) = &filter.q {
        let pat = like_pattern(q);
        parts.push(
            "(kind LIKE ? ESCAPE '\\' OR coalesce(target, '') LIKE ? ESCAPE '\\' \
             OR detail LIKE ? ESCAPE '\\' OR coalesce(principal_id, '') LIKE ? ESCAPE '\\' \
             OR coalesce(device_id, '') LIKE ? ESCAPE '\\')"
                .into(),
        );
        for _ in 0..5 {
            binds.push(Bind::S(pat.clone()));
        }
    }
    if let Some(b) = before {
        parts.push("seq < ?".into());
        binds.push(Bind::I(b));
    }
    if parts.is_empty() {
        (String::new(), binds)
    } else {
        (format!("WHERE {}", parts.join(" AND ")), binds)
    }
}

/// Run `SELECT <every column> … <where> ORDER BY seq <order> LIMIT ?`.
pub(crate) async fn select_rows(
    conn: &mut SqliteConnection,
    filter: &Filter,
    vis: &Visibility,
    before: Option<i64>,
    newest_first: bool,
    limit: Option<i64>,
) -> Result<Vec<StoredRow>, sqlx::Error> {
    let (w, binds) = where_clause(filter, vis, before);
    let sql = format!(
        "SELECT {COLUMNS} FROM audit_events {w} ORDER BY seq {} {}",
        if newest_first { "DESC" } else { "ASC" },
        if limit.is_some() { "LIMIT ?" } else { "" }
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = match b {
            Bind::S(s) => q.bind(s),
            Bind::I(i) => q.bind(i),
        };
    }
    if let Some(l) = limit {
        q = q.bind(l);
    }
    let rows = q.fetch_all(&mut *conn).await?;
    rows.iter().map(StoredRow::from_sql).collect()
}

/// §6.7's gate: effective `settings` in your own workspace.
pub(crate) fn may_read(env: &Env<'_>, ctx: &AccessCtx) -> Result<Visibility, AccessError> {
    if ctx.share.is_some() || ctx.share_headers {
        return Err(AccessError::new(
            Code::Forbidden,
            "the audit log is read in your own workspace, not through a share",
        ));
    }
    if !ctx.has(Cap::Settings) {
        return Err(AccessError::missing(CapSet::of(&[Cap::Settings])));
    }
    Ok(match env.tier {
        StoreTier::T0 => Visibility::All,
        StoreTier::T1 => Visibility::Principal(ctx.principal_id.to_string()),
    })
}

pub(crate) fn store<'a>(env: &Env<'a>) -> Result<&'a AccessStore, AccessError> {
    env.store.ok_or_else(AccessError::store_unavailable)
}

pub(crate) fn audit_err(e: AppendError) -> AccessError {
    match e {
        AppendError::AuditUnavailable(b) => AccessError::new(
            Code::AuditUnavailable,
            format!(
                "the audit chain is broken at #{} — access changes are paused",
                b.broken_at_seq
            ),
        ),
        AppendError::Sql(e) => AccessError::internal(e),
    }
}

/// Display names for the ids on a page: device names (both tiers), and
/// usernames (T1 `accounts`; T0's one principal is the host user).
async fn names(
    conn: &mut SqliteConnection,
    env: &Env<'_>,
    rows: &[StoredRow],
) -> Result<(Map<String, Value>, Map<String, Value>), sqlx::Error> {
    let mut devices = Map::new();
    for r in sqlx::query("SELECT device_id, name FROM devices")
        .fetch_all(&mut *conn)
        .await?
    {
        devices.insert(r.get::<String, _>(0), json!(r.get::<String, _>(1)));
    }
    let mut people = Map::new();
    let mut ids: Vec<&str> = rows
        .iter()
        .flat_map(|r| [r.principal_id.as_deref(), r.subject_principal_id.as_deref()])
        .flatten()
        .collect();
    ids.sort_unstable();
    ids.dedup();
    match env.tier {
        StoreTier::T0 => {
            if let Some(owner) = env.store.and_then(|s| s.meta().owner_principal_id) {
                people.insert(owner.to_string(), json!(env.principal.username));
            }
        }
        StoreTier::T1 => {
            for chunk in ids.chunks(200) {
                let sql = format!(
                    "SELECT principal_id, username FROM accounts WHERE principal_id IN ({})",
                    vec!["?"; chunk.len()].join(",")
                );
                let mut q = sqlx::query(&sql);
                for id in chunk {
                    q = q.bind(*id);
                }
                for r in q.fetch_all(&mut *conn).await? {
                    people.insert(r.get::<String, _>(0), json!(r.get::<String, _>(1)));
                }
            }
        }
    }
    Ok((devices, people))
}

/// One row as the Audit view reads it (camelCase; `detail` parsed).
fn row_view(r: &StoredRow, devices: &Map<String, Value>, people: &Map<String, Value>) -> Value {
    let name = |m: &Map<String, Value>, id: &Option<String>| {
        id.as_ref()
            .and_then(|i| m.get(i).cloned())
            .unwrap_or(Value::Null)
    };
    json!({
        "seq": r.seq,
        "atMs": r.at_ms,
        "kind": r.kind,
        "category": r.category,
        "principalId": r.principal_id,
        "actorName": name(people, &r.principal_id),
        "deviceId": r.device_id,
        "deviceName": name(devices, &r.device_id),
        "via": r.via,
        "subjectPrincipalId": r.subject_principal_id,
        "subjectName": name(people, &r.subject_principal_id),
        "subjectDeviceId": r.subject_device_id,
        "subjectDeviceName": name(devices, &r.subject_device_id),
        "projectKey": r.project_key,
        "target": r.target,
        "remoteAddr": r.remote_addr,
        "userAgent": r.user_agent,
        "detail": serde_json::from_str::<Value>(&r.detail).unwrap_or(Value::Null),
    })
}

/// `access_audit_list` (§9.1).
pub async fn dispatch(env: &Env<'_>, ctx: &AccessCtx, args: &Value) -> Result<Value, AccessError> {
    let vis = may_read(env, ctx)?;
    let store = store(env)?;
    let filter = Filter::parse(args.get("filter"))?;
    let before = match args.get("before") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_i64().filter(|b| *b > 0).ok_or_else(|| {
            AccessError::new(Code::InvalidRequest, "`before` must be a positive seq")
        })?),
    };
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => v
            .as_i64()
            .filter(|l| *l > 0)
            .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`limit` must be positive"))?
            .min(MAX_LIMIT),
    };
    let mut conn = store
        .pool()
        .acquire()
        .await
        .map_err(AccessError::internal)?;
    let rows = select_rows(&mut conn, &filter, &vis, before, true, Some(limit))
        .await
        .map_err(AccessError::internal)?;
    let (devices, people) = names(&mut conn, env, &rows)
        .await
        .map_err(AccessError::internal)?;
    let next_before = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| r.seq))
        .flatten();
    Ok(json!({
        "rows": rows.iter().map(|r| row_view(r, &devices, &people)).collect::<Vec<_>>(),
        "nextBefore": next_before,
    }))
}

/// `access_audit_verify` (§6.4, §9.1): the reseal-aware full walk. A break
/// enters `degraded` and is recorded; the call itself appends
/// `audit.verified {rows, ok, broken_at_seq}`.
pub async fn verify(env: &Env<'_>, ctx: &AccessCtx) -> Result<Value, AccessError> {
    may_read(env, ctx)?;
    let store = store(env)?;
    let report = {
        let mut conn = store
            .pool()
            .acquire()
            .await
            .map_err(AccessError::internal)?;
        store
            .chain()
            .verify_boot(&mut conn)
            .await
            .map_err(AccessError::internal)?
    };
    let broken = report.broken.as_ref().map(|b| b.broken_at_seq);
    let ev = Event::by("audit.verified", ctx).detail(json!({
        "rows": report.rows,
        "ok": report.ok(),
        "broken_at_seq": broken,
    }));
    if let Err(e) = record(store, &ev).await {
        tracing::warn!("audit.verified not recorded: {e}");
    }
    let mut out = json!({
        "ok": report.ok(),
        "rows": report.rows,
        "headHash": report.head.map(|h| hex::encode(h.hash)).unwrap_or_default(),
    });
    if let Some(seq) = broken {
        out["brokenAtSeq"] = json!(seq);
    }
    Ok(out)
}

/// `access_audit_record_local` (§6.5, §6.9): the desktop's app-lock, vault
/// and in-process permission-decision rows. The T0 operator bearer only.
pub async fn record_local(
    env: &Env<'_>,
    ctx: &AccessCtx,
    args: &Value,
) -> Result<Value, AccessError> {
    if env.tier != StoreTier::T0 || !ctx.is_operator() {
        return Err(AccessError::new(Code::Forbidden, "class=operator"));
    }
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .filter(|k| LOCAL_KINDS.contains(k))
        .and_then(static_kind)
        .ok_or_else(|| {
            AccessError::new(
                Code::InvalidRequest,
                format!("`kind` must be one of {}", LOCAL_KINDS.join(", ")),
            )
        })?;
    let target = args
        .get("target")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| AccessError::new(Code::InvalidRequest, "`target` is required"))?;
    if target.chars().count() > MAX_TEXT {
        return Err(AccessError::new(
            Code::InvalidRequest,
            "`target` is too long",
        ));
    }
    let detail = match args.get("detail") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(Value::Object(o)) => {
            if serde_json::to_string(o).map_or(true, |s| s.len() > MAX_DETAIL_BYTES) {
                return Err(AccessError::new(
                    Code::InvalidRequest,
                    "`detail` is too large (names and ids only)",
                ));
            }
            Value::Object(o.clone())
        }
        Some(_) => {
            return Err(AccessError::new(
                Code::InvalidRequest,
                "`detail` must be an object",
            ))
        }
    };
    let store = store(env)?;
    let mut ev = Event::by(kind, ctx).detail(detail);
    if !target.is_empty() {
        ev = ev.target(target);
    }
    record(store, &ev).await.map_err(audit_err)?;
    Ok(json!({}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::access::caps::Tier;
    use crate::access::ctx::{RequestMeta, Via};
    use crate::access::devices::tests::{operator_ctx, pair};
    use crate::access::rpc::{dispatch as rpc, PrincipalInfo};
    use crate::access::sockets::Registry;

    pub(crate) fn env<'a>(store: &'a AccessStore, sockets: &'a Registry) -> Env<'a> {
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

    pub(crate) fn device_ctx(store: &AccessStore, device_id: &str, tier: Tier) -> AccessCtx {
        AccessCtx {
            principal_id: store.meta().owner_principal_id.unwrap(),
            via: Via::Device {
                device_id: device_id.into(),
            },
            device_id: Some(device_id.into()),
            tier,
            share: None,
            share_headers: false,
            caps: tier.caps(),
            admin_strength: tier == Tier::Full,
            meta: RequestMeta::default(),
        }
    }

    #[tokio::test]
    async fn record_local_takes_the_six_kinds_from_the_operator_only() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        for kind in LOCAL_KINDS {
            rpc(
                &e,
                &op,
                "access_audit_record_local",
                &json!({"kind": kind, "target": "workspace", "detail": {"reason": "idle"}}),
            )
            .await
            .unwrap_or_else(|err| panic!("{kind}: {err}"));
        }
        let err = rpc(
            &e,
            &op,
            "access_audit_record_local",
            &json!({"kind": "device.revoked", "target": "x"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest);
        let err = rpc(
            &e,
            &op,
            "access_audit_record_local",
            &json!({"kind": "app.locked", "target": "x", "detail": "nope"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::InvalidRequest);
        // A full paired device is not the operator.
        let (row, _) = pair(&store, Tier::Full).await;
        let phone = device_ctx(&store, &row.device_id, Tier::Full);
        let err = rpc(
            &e,
            &phone,
            "access_audit_record_local",
            &json!({"kind": "app.locked", "target": "x"}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, Code::Forbidden);
        // The rows carry the operator's actor columns.
        let list = rpc(
            &e,
            &op,
            "access_audit_list",
            &json!({"filter": {"q": "vault"}}),
        )
        .await
        .unwrap();
        let rows = list["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["kind"], "vault.unlocked");
        assert_eq!(rows[0]["via"], "operator");
        assert_eq!(rows[0]["actorName"], "ned");
        assert_eq!(rows[0]["deviceName"], "test-host");
        assert_eq!(rows[0]["detail"]["reason"], "idle");
        assert_eq!(rows[0]["target"], "workspace");
    }

    #[tokio::test]
    async fn list_pages_newest_first_and_filters_narrow() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        for i in 0..7 {
            rpc(
                &e,
                &op,
                "access_audit_record_local",
                &json!({"kind": "app.locked", "target": format!("t{i}")}),
            )
            .await
            .unwrap();
        }
        // 1 store.created + 7.
        let page = rpc(&e, &op, "access_audit_list", &json!({"limit": 5}))
            .await
            .unwrap();
        let rows = page["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["seq"], 8);
        assert_eq!(page["nextBefore"], 4);
        let rest = rpc(
            &e,
            &op,
            "access_audit_list",
            &json!({"limit": 5, "before": 4}),
        )
        .await
        .unwrap();
        assert_eq!(rest["rows"].as_array().unwrap().len(), 3);
        assert_eq!(rest["nextBefore"], Value::Null);
        let pairing = rpc(
            &e,
            &op,
            "access_audit_list",
            &json!({"filter": {"category": "pairing"}}),
        )
        .await
        .unwrap();
        assert!(pairing["rows"].as_array().unwrap().is_empty());
        // A wildcard in q is literal.
        let none = rpc(&e, &op, "access_audit_list", &json!({"filter": {"q": "%"}}))
            .await
            .unwrap();
        assert!(none["rows"].as_array().unwrap().is_empty());
        let bad = rpc(
            &e,
            &op,
            "access_audit_list",
            &json!({"filter": {"category": "nope"}}),
        )
        .await
        .unwrap_err();
        assert_eq!(bad.code, Code::InvalidRequest);
    }

    /// §6.7 / P-14: a phone below `full` can't read the log.
    #[tokio::test]
    async fn reading_needs_settings() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let (row, _) = pair(&store, Tier::Approve).await;
        let phone = device_ctx(&store, &row.device_id, Tier::Approve);
        for cmd in [
            "access_audit_list",
            "access_audit_verify",
            "access_audit_export",
        ] {
            let err = rpc(&e, &phone, cmd, &json!({})).await.unwrap_err();
            assert_eq!(err.code, Code::Forbidden, "{cmd}");
            assert!(err.message.contains("settings"), "{cmd}: {err}");
        }
        let full = device_ctx(&store, &row.device_id, Tier::Full);
        rpc(&e, &full, "access_audit_list", &json!({}))
            .await
            .unwrap();
    }

    #[test]
    fn t1_visibility_is_actor_subject_or_owned_project() {
        let (w, binds) = where_clause(
            &Filter::default(),
            &Visibility::Principal("p1".into()),
            None,
        );
        assert!(w.contains("principal_id = ?"));
        assert!(w.contains("subject_principal_id = ?"));
        assert!(w.contains("substr(project_key, 1, ?) = ?"));
        assert!(matches!(&binds[3], Bind::S(s) if s == "p1/"));
        let (w, _) = where_clause(&Filter::default(), &Visibility::All, None);
        assert!(w.is_empty());
    }

    #[tokio::test]
    async fn verify_reports_ok_and_records_itself() {
        let store = AccessStore::memory_t0().await;
        let reg = Registry::new();
        let e = env(&store, &reg);
        let op = operator_ctx(&store);
        let v = rpc(&e, &op, "access_audit_verify", &json!({}))
            .await
            .unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["rows"], 1);
        assert_eq!(v["headHash"].as_str().unwrap().len(), 64);
        let last = rpc(&e, &op, "access_audit_list", &json!({"limit": 1}))
            .await
            .unwrap();
        assert_eq!(last["rows"][0]["kind"], "audit.verified");
    }
}
