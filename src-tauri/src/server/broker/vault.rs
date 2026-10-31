//! Setting the box sender's credentials from Settings -> Integrations (WP-K,
//! `16-settings-sender-credentials.md`; wire contract `approvals-contract.md`
//! section 4.10 in the royalti-co repo).
//!
//! The values go to **the sender's vault only**. This module is a relay: the
//! broker (uid 0, the one peer the sender accepts for these operations) writes
//! one newline-JSON request to the sender socket and answers the browser with
//! an allow-listed rendering of the reply. It never holds a value past the
//! request, never logs one, and never puts one in a response or an error.
//!
//! **Identity.** The caller is the [`PrincipalCtx`] the session middleware
//! resolved (`ctx.principal.id` + `username`) and the account row read fresh
//! (`is_admin`, not disabled). Nothing in the request body names a person, an
//! approver or a role: a body with any field but `group` / `values` is
//! refused, and the `approver` block of the sender request is built from the
//! session alone. A request that arrived through a share, or with a credential
//! below admin strength (P-26: a password session, or a `full` device), is
//! refused: a View-tier device must not be able to replace a mailbox password.
//!
//! **Two layers.** [`authorise`] mirrors the sender's rule first (a member:
//! only their own outreach mailbox; an admin: the shared groups, their own
//! mailbox, and clearing anyone's), so an obvious refusal costs no socket
//! call. The **sender is the authority**: it re-decides from its own config
//! (`SENDER_VAULT_ADMINS`, `SENDER_VAULT_PEOPLE`) and its answer wins. Which
//! mailbox is "own" is the sender's `person` from `vault.status`, not a guess
//! from the username here.
//!
//! **Founder note (K-7).** Every successful set / replace / clear returns the
//! sender's `event` (key names only). The server has no Mattermost client and
//! no outbound note path, and this module does not invent one: the event and a
//! ready-to-post note line go into the shell's own audit row
//! (`sender.vault_changed`). TODO(WP-K): the Mattermost note itself is the
//! follow-up of the bridge polling the sender's `vault.events` (its own
//! cursor) and posting to the founder; a server-side notifier reading
//! [`founder_note`]'s text from that audit row is the alternative.
//!
//! Linux-only, like everything under `broker`.

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::{json, Map, Value};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use super::approvals::{exchange_value, ApprovalsConfig, SenderError, CONTRACT_VERSION};
use super::BrokerState;
use crate::access::audit::Event;
use crate::access::caps::Cap;
use crate::server::auth::{json_error, Credential, PrincipalCtx};
use crate::server::operator::accounts;

/// Most a request body may be (eight fields, a few hundred bytes each).
pub const MAX_BODY: usize = 16 * 1024;
/// A login test is two TLS handshakes at up to 10 s a step.
const TEST_TIMEOUT: Duration = Duration::from_secs(45);
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_VALUES: usize = 16;
const MAX_VALUE_LEN: usize = 4096;

/// The shared groups (contract 4.10). The sender owns the per-group name list.
pub const SHARED_GROUPS: [&str; 5] = ["ruby_mail", "buffer", "listmonk", "cms_draft", "tasks"];

/// The audit kind, registered in `access::audit::KINDS`.
pub const AUDIT_KIND: &str = "sender.vault_changed";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Status,
    Set,
    Test,
    Clear,
}

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        match s {
            "status" => Some(Op::Status),
            "set" => Some(Op::Set),
            "test" => Some(Op::Test),
            "clear" => Some(Op::Clear),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Op::Status => "status",
            Op::Set => "set",
            Op::Test => "test",
            Op::Clear => "clear",
        }
    }

    fn action(self) -> &'static str {
        match self {
            Op::Status => "vault.status",
            Op::Set => "vault.set",
            Op::Test => "vault.test",
            Op::Clear => "vault.clear",
        }
    }
}

/// Who is asking, from the session. Never from the request body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub principal_id: String,
    pub username: String,
    pub is_admin: bool,
}

/// A group id the sender knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Group {
    /// `outreach:<PERSON>`.
    Outreach(String),
    /// One of [`SHARED_GROUPS`].
    Shared(String),
}

impl Group {
    pub fn parse(s: &str) -> Option<Group> {
        if let Some(p) = s.strip_prefix("outreach:") {
            return is_person(p).then(|| Group::Outreach(p.to_string()));
        }
        SHARED_GROUPS
            .contains(&s)
            .then(|| Group::Shared(s.to_string()))
    }

    pub fn id(&self) -> String {
        match self {
            Group::Outreach(p) => format!("outreach:{p}"),
            Group::Shared(g) => g.clone(),
        }
    }
}

/// `[A-Z][A-Z0-9]{1,31}`.
fn is_person(p: &str) -> bool {
    let b = p.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `[A-Z][A-Z0-9_]{1,80}`: the shape of a vault key name.
fn is_key_name(k: &str) -> bool {
    let b = k.as_bytes();
    (2..=81).contains(&b.len())
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// The values of one group. Debug prints key names only, so a stray
/// `{:?}` can never leak a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Values(Map<String, Value>);

impl fmt::Debug for Values {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.keys()).finish()
    }
}

impl Values {
    pub fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

/// A parsed body: `{group?, values?}` and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultRequest {
    pub group: Option<String>,
    pub values: Option<Values>,
}

/// An error answer: a stable code, one fixed sentence, a status, and the few
/// fixed extras a refusal may carry (never a value).
#[derive(Debug, Clone, PartialEq)]
pub struct VaultError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub extra: Map<String, Value>,
}

impl VaultError {
    fn new(status: StatusCode, code: &str, message: &str) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.to_string(),
            extra: Map::new(),
        }
    }

    fn with(mut self, k: &str, v: Value) -> Self {
        self.extra.insert(k.to_string(), v);
        self
    }

    fn invalid(message: &str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    fn into_response(self) -> Response {
        let mut body = json!({ "ok": false, "error": self.message, "code": self.code });
        if let Some(o) = body.as_object_mut() {
            for (k, v) in self.extra {
                o.insert(k, v);
            }
        }
        (self.status, Json(body)).into_response()
    }
}

/// Parse the body. Anything but `group` and `values` is refused, so an
/// `approver`, `principal`, `role` or `source` smuggled in a body is an error
/// rather than something to ignore quietly.
pub fn parse_body(raw: &[u8]) -> Result<VaultRequest, VaultError> {
    if raw.is_empty() {
        return Ok(VaultRequest {
            group: None,
            values: None,
        });
    }
    let v: Value = serde_json::from_slice(raw)
        .map_err(|_| VaultError::invalid("The request was not JSON."))?;
    let Some(o) = v.as_object() else {
        return Err(VaultError::invalid("The request must be an object."));
    };
    if o.keys().any(|k| k != "group" && k != "values") {
        return Err(VaultError::invalid(
            "The request may only carry `group` and `values`.",
        ));
    }
    let group = match o.get("group") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(VaultError::invalid("`group` must be a string.")),
    };
    let values = match o.get("values") {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => {
            if m.len() > MAX_VALUES {
                return Err(VaultError::invalid("Too many fields."));
            }
            for (k, v) in m {
                let Some(s) = v.as_str() else {
                    return Err(VaultError::invalid("Every value must be text."));
                };
                if !is_key_name(k) || s.chars().count() > MAX_VALUE_LEN {
                    return Err(VaultError::invalid("A field name or value is not valid."));
                }
            }
            Some(Values(m.clone()))
        }
        Some(_) => return Err(VaultError::invalid("`values` must be an object.")),
    };
    Ok(VaultRequest { group, values })
}

fn forbidden(code: &str, message: &str) -> VaultError {
    VaultError::new(StatusCode::FORBIDDEN, code, message)
}

/// The shell's own first layer, mirroring the sender's rule. `own_person` is
/// the sender's `person` for this caller (only consulted for an outreach
/// group; `None` = the caller has no mailbox slot).
pub fn authorise(
    op: Op,
    group: &Group,
    caller: &Caller,
    own_person: Option<&str>,
) -> Result<(), VaultError> {
    match group {
        Group::Shared(_) => {
            if caller.is_admin {
                Ok(())
            } else {
                Err(forbidden(
                    "not_permitted",
                    "Only an administrator can change the shared credentials.",
                ))
            }
        }
        Group::Outreach(p) => {
            let own = own_person == Some(p.as_str());
            // An admin may read and clear anyone's mailbox, never set or test it.
            let allowed = match op {
                Op::Status | Op::Clear => own || caller.is_admin,
                Op::Set | Op::Test => own,
            };
            if allowed {
                Ok(())
            } else {
                Err(forbidden(
                    "not_permitted",
                    "You can only set your own outreach mailbox.",
                ))
            }
        }
    }
}

/// The audit facts of one call. Never a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Audit {
    pub action: &'static str,
    pub group: Option<String>,
    pub names: Vec<String>,
    pub result: String,
    /// The sender's `event` on an ok set / replace / clear.
    pub event: Option<Value>,
}

/// Run one operation: authorise, relay, render. Returns the audit facts and
/// either the response data or the error to send.
pub async fn relay(
    cfg: &ApprovalsConfig,
    caller: &Caller,
    op: Op,
    req: VaultRequest,
) -> (Audit, Result<Value, VaultError>) {
    let mut audit = Audit {
        action: op.name(),
        group: None,
        names: Vec::new(),
        result: "ok".into(),
        event: None,
    };
    let out = relay_inner(cfg, caller, op, req, &mut audit).await;
    if let Err(e) = &out {
        audit.result = e.code.clone();
        if let Some(t) = e.extra.get("test").and_then(Value::as_str) {
            audit.result = format!("{}:{t}", e.code);
        }
    }
    (audit, out)
}

async fn relay_inner(
    cfg: &ApprovalsConfig,
    caller: &Caller,
    op: Op,
    req: VaultRequest,
    audit: &mut Audit,
) -> Result<Value, VaultError> {
    let group = match req.group.as_deref() {
        None => None,
        Some(g) => Some(Group::parse(g).ok_or_else(|| {
            VaultError::new(
                StatusCode::BAD_REQUEST,
                "invalid_group",
                "That is not a credential group.",
            )
        })?),
    };
    audit.group = group.as_ref().map(Group::id);

    let approver = json!({
        "kind": "principal",
        "id": caller.principal_id,
        "username": caller.username,
    });
    let base = |extra: Value| {
        let mut m = json!({
            "v": CONTRACT_VERSION,
            "action": op.action(),
            "source": "shell",
            "approver": approver.clone(),
        });
        if let (Some(o), Value::Object(e)) = (m.as_object_mut(), extra) {
            o.extend(e);
        }
        m
    };

    if op == Op::Status {
        if let Some(g) = &group {
            // A member cannot even read a group that is not theirs.
            let need_person = matches!(g, Group::Outreach(_)) && !caller.is_admin;
            let person = if need_person {
                lookup_person(cfg, &approver).await?
            } else {
                None
            };
            authorise(op, g, caller, person.as_deref())?;
        }
        let request = match &group {
            Some(g) => base(json!({ "group": g.id() })),
            None => base(json!({})),
        };
        let reply = send(cfg, &request, STATUS_TIMEOUT, false).await?;
        return Ok(sanitize_status(&reply));
    }

    let Some(group) = group else {
        return Err(VaultError::invalid("`group` is required."));
    };
    // The one case that needs no person: an admin clearing a mailbox.
    let skip_lookup = match &group {
        Group::Shared(_) => true,
        Group::Outreach(_) => op == Op::Clear && caller.is_admin,
    };
    let person = if skip_lookup {
        None
    } else {
        lookup_person(cfg, &approver).await?
    };
    authorise(op, &group, caller, person.as_deref())?;

    let request = match op {
        Op::Clear => base(json!({ "group": group.id() })),
        Op::Set | Op::Test => {
            let Some(values) = req.values else {
                return Err(VaultError::invalid("`values` is required."));
            };
            if let Group::Outreach(p) = &group {
                let prefix = format!("OUTREACH_{p}_");
                if values.0.keys().any(|k| !k.starts_with(&prefix)) {
                    return Err(VaultError::invalid(
                        "Those fields do not belong to this mailbox.",
                    ));
                }
            }
            audit.names = values.names();
            base(json!({ "group": group.id(), "values": Value::Object(values.0) }))
        }
        Op::Status => unreachable!("handled above"),
    };
    let timeout = if op == Op::Clear {
        STATUS_TIMEOUT
    } else {
        TEST_TIMEOUT
    };
    let reply = send(cfg, &request, timeout, matches!(op, Op::Set | Op::Clear)).await?;
    let (data, event, names) = sanitize_write(&reply, op);
    if !names.is_empty() {
        audit.names = names;
    }
    audit.event = event;
    Ok(data)
}

/// `vault.status` with no group, for the sender's idea of who the caller is.
async fn lookup_person(
    cfg: &ApprovalsConfig,
    approver: &Value,
) -> Result<Option<String>, VaultError> {
    let request = json!({
        "v": CONTRACT_VERSION, "action": "vault.status", "source": "shell", "approver": approver,
    });
    let reply = send(cfg, &request, STATUS_TIMEOUT, false).await?;
    Ok(reply
        .get("person")
        .and_then(Value::as_str)
        .filter(|p| is_person(p))
        .map(str::to_string))
}

/// One exchange; an `ok:false` reply becomes the mapped [`VaultError`]. The
/// socket must be served by the sender's own uid (`cfg.sender_uid`); with no
/// such uid configured the operation fails closed. `writes`: the request may
/// change the vault, so a failure after it was sent is reported as an unknown
/// outcome, not as "nothing was changed".
async fn send(
    cfg: &ApprovalsConfig,
    request: &Value,
    timeout: Duration,
    writes: bool,
) -> Result<Value, VaultError> {
    let Some(uid) = cfg.sender_uid else {
        tracing::warn!("sender vault: no sender uid configured; refusing");
        return Err(VaultError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "sender_unavailable",
            "The box's sender service is not reachable. Nothing was changed. Try again in a moment.",
        ));
    };
    match exchange_value(cfg, request, timeout, Some(uid)).await {
        Ok(v) if v.get("ok").and_then(Value::as_bool) == Some(true) => Ok(v),
        Ok(v) => Err(map_refusal(&v)),
        Err(e) => Err(map_transport(&e, writes)),
    }
}

/// Whether the request had certainly not reached the sender's handler.
fn not_sent(e: &SenderError) -> bool {
    match e {
        SenderError::Unavailable(d) => {
            d.starts_with("connect ")
                || d.contains("is served by uid")
                || d.contains("an agent account")
                || d.starts_with("cannot read the socket")
        }
        _ => false,
    }
}

fn map_transport(e: &SenderError, writes: bool) -> VaultError {
    // The detail (socket path, io error) goes to the log, never to the browser.
    tracing::warn!(detail = %e.detail(), "sender vault: socket exchange failed");
    if writes && !not_sent(e) {
        // The request was written; the sender may have applied it.
        return VaultError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "outcome_unknown",
            "The box did not answer in time; the change may or may not have been saved. Refresh to check.",
        );
    }
    match e {
        SenderError::Unavailable(_) => VaultError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "sender_unavailable",
            "The box's sender service is not reachable. Nothing was changed. Try again in a moment.",
        ),
        _ => VaultError::new(
            StatusCode::BAD_GATEWAY,
            "sender_error",
            "The box's sender service gave an answer this app does not understand. Nothing was changed.",
        ),
    }
}

/// Fixed text per sender code. Only the contract's fixed extras are copied
/// (`test`, `tested`, `group`, `names`, `reason`, `retryAfterSec`), and only
/// when they have the expected shape.
fn map_refusal(reply: &Value) -> VaultError {
    let code = reply
        .get("error")
        .and_then(Value::as_str)
        .filter(|c| {
            !c.is_empty()
                && c.len() <= 48
                && c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
        .unwrap_or("sender_error");
    let (status, message) = match code {
        "vault_unconfigured" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Setting credentials from here is not turned on for this box. Ask whoever runs it.",
        ),
        "unauthorized_approver" => (
            StatusCode::FORBIDDEN,
            "The box's sender does not recognise your account for credential changes.",
        ),
        "not_permitted" => (
            StatusCode::FORBIDDEN,
            "The box's sender does not allow you to change this group.",
        ),
        "invalid_group" => (StatusCode::BAD_REQUEST, "That is not a credential group."),
        "invalid_value" | "invalid_values" => (
            StatusCode::BAD_REQUEST,
            "One of the values is not in a form the sender accepts. Nothing was saved.",
        ),
        "rate_limited" => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many attempts. Wait a few minutes and try again.",
        ),
        "busy" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The sender is busy testing other logins. Try again in a moment.",
        ),
        "test_failed" => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "The login test did not pass. Nothing was saved.",
        ),
        "vault_unsafe" | "vault_io" | "internal_error" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "The box could not update its credential file. Nothing was changed. Ask whoever runs the box.",
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "The box's sender refused the request.",
        ),
    };
    let mut e = VaultError::new(status, code, message);
    let fixed = |k: &str| {
        reply
            .get(k)
            .and_then(Value::as_str)
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 48
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            .map(|s| json!(s))
    };
    for k in ["test", "tested", "reason"] {
        if let Some(v) = fixed(k) {
            e = e.with(k, v);
        }
    }
    if let Some(g) = reply
        .get("group")
        .and_then(Value::as_str)
        .and_then(Group::parse)
    {
        e = e.with("group", json!(g.id()));
    }
    let names = key_names(reply.get("names"));
    if !names.is_empty() {
        e = e.with("names", json!(names));
    }
    if let Some(n) = reply.get("retryAfterSec").and_then(Value::as_u64) {
        e = e.with("retryAfterSec", json!(n.min(86_400)));
    }
    e
}

fn key_names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|k| is_key_name(k))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn int_of(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_u64)
}

fn short_text(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| s.chars().count() <= max && !s.chars().any(char::is_control))
        .map(str::to_string)
}

/// `vault.status` rendered from an allow-list: group metadata, key names,
/// integer lengths and times. A string where a length belongs is dropped.
pub fn sanitize_status(reply: &Value) -> Value {
    let own_person = reply
        .get("person")
        .and_then(Value::as_str)
        .filter(|p| is_person(p));
    let groups: Vec<Value> = reply
        .get("groups")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|g| {
                    let id = g.get("group").and_then(Value::as_str)?;
                    Group::parse(id)?;
                    let names: Vec<Value> = g
                        .get("names")
                        .and_then(Value::as_array)
                        .map(|n| {
                            n.iter()
                                .filter_map(|n| {
                                    let name = n.get("name").and_then(Value::as_str)?;
                                    is_key_name(name).then(|| {
                                        json!({
                                            "name": name,
                                            "required": n.get("required").and_then(Value::as_bool).unwrap_or(false),
                                        })
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    // Lengths of other people's mailbox secrets are not shown
                    // (an admin sees which keys exist, not how long they are).
                    let others = matches!(Group::parse(id), Some(Group::Outreach(ref p)) if Some(p.as_str()) != own_person);
                    let present: Map<String, Value> = g
                        .get("present")
                        .and_then(Value::as_object)
                        .map(|p| {
                            p.iter()
                                .filter(|(k, _)| is_key_name(k))
                                .filter_map(|(k, v)| {
                                    int_of(Some(v))
                                        .map(|n| (k.clone(), if others { Value::Null } else { json!(n) }))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let readiness = g.get("readiness").and_then(Value::as_object).map(|r| {
                        let mut m = Map::new();
                        for k in ["identityKnown", "approverListed"] {
                            match r.get(k) {
                                Some(Value::Bool(b)) => m.insert(k.into(), json!(b)),
                                Some(Value::Null) => m.insert(k.into(), Value::Null),
                                _ => None,
                            };
                        }
                        Value::Object(m)
                    });
                    Some(json!({
                        "group": id,
                        "label": short_text(g.get("label"), 80),
                        "shared": g.get("shared").and_then(Value::as_bool).unwrap_or(false),
                        "names": names,
                        "present": present,
                        "missing": key_names(g.get("missing")),
                        "complete": g.get("complete").and_then(Value::as_bool).unwrap_or(false),
                        "lastChanged": short_text(g.get("lastChanged"), 40),
                        "lastChangedBy": short_text(g.get("lastChangedBy"), 64),
                        "readiness": readiness,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "person": reply.get("person").and_then(Value::as_str).filter(|p| is_person(p)),
        "admin": reply.get("admin").and_then(Value::as_bool).unwrap_or(false),
        "groups": groups,
    })
}

/// A set / test / clear reply rendered from an allow-list, plus the sender's
/// `event` (kept for the audit row, not sent to the browser) and the key
/// names that changed.
fn sanitize_write(reply: &Value, op: Op) -> (Value, Option<Value>, Vec<String>) {
    let names = key_names(reply.get("names"));
    let group = reply
        .get("group")
        .and_then(Value::as_str)
        .and_then(Group::parse)
        .map(|g| g.id());
    let mut data = json!({ "group": group });
    if let Some(o) = data.as_object_mut() {
        match op {
            Op::Test => {}
            Op::Set => {
                o.insert("names".into(), json!(names));
                o.insert(
                    "replaced".into(),
                    json!(reply
                        .get("replaced")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)),
                );
            }
            Op::Clear => {
                o.insert("names".into(), json!(names));
            }
            Op::Status => {}
        }
        if op != Op::Clear {
            for k in ["test", "tested"] {
                if let Some(s) = short_text(reply.get(k), 24) {
                    o.insert(k.into(), json!(s));
                }
            }
        }
    }
    let event = reply
        .get("event")
        .filter(|e| e.is_object())
        .map(clean_event);
    (data, event, names)
}

/// The sender's event object reduced to its documented fields.
fn clean_event(e: &Value) -> Value {
    json!({
        "id": int_of(e.get("id")),
        "at": short_text(e.get("at"), 40),
        "actor": short_text(e.get("actor"), 64),
        "group": e.get("group").and_then(Value::as_str).and_then(Group::parse).map(|g| g.id()),
        "label": short_text(e.get("label"), 80),
        "action": short_text(e.get("action"), 16),
        "names": key_names(e.get("names")),
    })
}

/// The short line for the founder (K-7): who, which group, which key NAMES.
pub fn founder_note(event: &Value, fallback_actor: &str) -> String {
    let who = event
        .get("actor")
        .and_then(Value::as_str)
        .unwrap_or(fallback_actor);
    let verb = match event.get("action").and_then(Value::as_str) {
        Some("clear") => "cleared",
        Some("replace") => "replaced",
        _ => "set",
    };
    let what = event
        .get("label")
        .and_then(Value::as_str)
        .or_else(|| event.get("group").and_then(Value::as_str))
        .unwrap_or("a sender credential group");
    let names = key_names(event.get("names"));
    let list = if names.is_empty() {
        "no keys".to_string()
    } else {
        names.join(", ")
    };
    let at = event.get("at").and_then(Value::as_str).unwrap_or("");
    format!(
        "{who} {verb} {what} in the box sender vault ({list}; key names only){}",
        if at.is_empty() {
            String::new()
        } else {
            format!(" at {at}")
        }
    )
}

// --- the HTTP handler --------------------------------------------------------

fn data_response(data: Value) -> Response {
    (StatusCode::OK, Json(json!({ "ok": true, "data": data }))).into_response()
}

/// What the handler established about the request before relaying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Admission {
    /// The account exists and is not disabled.
    pub account_enabled: bool,
    /// P-26: a password session, or a `full` device.
    pub strong: bool,
    /// The request came through a share (or carried share headers).
    pub in_share: bool,
    /// The credential's effective caps include Settings.
    pub has_settings: bool,
}

/// The handler's own refusals, before any socket call. The route is not in
/// the broker's `route_requirement` path (that table is consulted on the
/// child), so the Settings capability is checked here explicitly.
pub fn admit(a: &Admission) -> Result<(), VaultError> {
    if a.account_enabled && a.strong && !a.in_share && a.has_settings {
        Ok(())
    } else {
        Err(VaultError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "forbidden",
        ))
    }
}

/// `POST /api/sender/vault/:op` (broker only). The caller is the session's
/// principal; the account is re-read now, so a demotion or a disable takes
/// effect on the next request.
pub async fn vault_op(
    State(state): State<Arc<BrokerState>>,
    Extension(ctx): Extension<PrincipalCtx>,
    Path(op): Path<String>,
    req: Request,
) -> Response {
    let Some(op) = Op::parse(&op) else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let (parts, body) = req.into_parts();

    let account = async {
        let mut conn = state.pool.acquire().await?;
        accounts::by_id(&mut conn, ctx.principal.id).await
    }
    .await;
    let account = match account {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("sender vault: account lookup failed: {e}");
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth_unavailable",
                "authentication is temporarily unavailable",
            );
        }
    };

    let (strong, in_share, has_settings, actx) = match &state.access_t1 {
        Some(t1) => match t1.ctx_for(&ctx, &parts).await {
            Ok(b) => (
                b.access.admin_strength,
                b.access.share.is_some() || b.access.share_headers,
                b.access.caps.contains(Cap::Settings),
                Some(b.access),
            ),
            Err(_) => (false, true, false, None),
        },
        // No access layer (unit tests): only a password session.
        None => (
            matches!(ctx.via, Credential::Session { .. }),
            false,
            true,
            None,
        ),
    };
    let admission = Admission {
        account_enabled: account.as_ref().is_some_and(|a| !a.is_disabled()),
        strong,
        in_share,
        has_settings,
    };
    if let Err(e) = admit(&admission) {
        tracing::warn!(principal = %ctx.principal.id, "sender vault: refused at admission");
        return e.into_response();
    }
    let Some(account) = account else {
        return json_error(StatusCode::FORBIDDEN, "forbidden", "forbidden");
    };

    let Ok(raw) = axum::body::to_bytes(body, MAX_BODY).await else {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request",
            "body too large",
        );
    };
    let caller = Caller {
        principal_id: ctx.principal.id.to_string(),
        username: account.username.clone(),
        is_admin: account.is_admin,
    };
    let (audit, out) = handle(&state.approvals, &admission, &caller, op, &raw).await;
    if op != Op::Status {
        record(&state, &caller, actx.as_ref(), &audit).await;
    }
    match out {
        Ok(data) => data_response(data),
        Err(e) => e.into_response(),
    }
}

/// Admission, then parse and relay; the handler minus the broker state, so a
/// refusal is testable without one. A refusal makes no socket call.
pub async fn handle(
    cfg: &ApprovalsConfig,
    admission: &Admission,
    caller: &Caller,
    op: Op,
    raw: &Bytes,
) -> (Audit, Result<Value, VaultError>) {
    if let Err(e) = admit(admission) {
        return (
            Audit {
                action: op.name(),
                group: None,
                names: Vec::new(),
                result: e.code.clone(),
                event: None,
            },
            Err(e),
        );
    }
    run(cfg, caller, op, raw).await
}

/// Parse and relay. Split from the handler so it runs without a broker.
pub async fn run(
    cfg: &ApprovalsConfig,
    caller: &Caller,
    op: Op,
    raw: &Bytes,
) -> (Audit, Result<Value, VaultError>) {
    match parse_body(raw) {
        Ok(req) => relay(cfg, caller, op, req).await,
        Err(e) => (
            Audit {
                action: op.name(),
                group: None,
                names: Vec::new(),
                result: e.code.clone(),
                event: None,
            },
            Err(e),
        ),
    }
}

/// The shell's own audit entry, and a log line. Names only.
async fn record(
    state: &BrokerState,
    caller: &Caller,
    actx: Option<&crate::access::AccessCtx>,
    audit: &Audit,
) {
    tracing::info!(
        principal = %caller.principal_id,
        action = audit.action,
        group = audit.group.as_deref().unwrap_or("-"),
        names = audit.names.len(),
        result = %audit.result,
        "sender vault"
    );
    let (Some(t1), Some(actx)) = (&state.access_t1, actx) else {
        return;
    };
    let note = audit
        .event
        .as_ref()
        .map(|e| founder_note(e, &caller.username));
    let ev = Event::by(AUDIT_KIND, actx).detail(json!({
        "username": caller.username,
        "action": audit.action,
        "group": audit.group,
        "names": audit.names,
        "result": audit.result,
        "sender_event": audit.event,
        "founder_note": note,
    }));
    crate::server::shared::notifications::routing::append_audit(&t1.store, &ev).await;
}

#[cfg(test)]
mod tests;
