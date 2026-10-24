//! Approving an action draft from inside a G-ACCESS share (box plan WP-5,
//! `04-ruby-review-and-send.md`; wire contract `approvals-contract.md` v1 in
//! the royalti-co repo).
//!
//! **The problem.** On the T1 box the drafts (`pa_action_drafts`) belong to an
//! agent account (Ruby, `ik-ruby`). A human reaches them through a share of
//! Ruby's project, so every RPC is routed to **the owner's child** — a daemon
//! running as the agent. Nothing that runs as the agent may be trusted to say
//! "a human approved this". The sender (`ikenga-sender`) therefore sends a
//! draft only when its approvals **ledger** holds an approval, written over a
//! Unix socket by exactly two writers told apart by `SO_PEERCRED`: the
//! Mattermost bridge and **this broker (root, uid 0)**.
//!
//! **What this module does.** [`gate`] runs in [`super::proxy::rpc_proxy`],
//! after R-3 authorization and before anything is forwarded, and only for an
//! RPC that arrives through a share:
//!
//! * `pa_actions_list | update | commit | reject` need the viewer's share role
//!   to be **Operator**. The capability check already in front of them is
//!   `approve` (update / commit / reject) or `sessions` (list); a project can
//!   override a role's capability row, so a Reviewer may hold either. Here the
//!   *role* decides, as the plan says: Onyedikachi (Reviewer) approves only
//!   through Mattermost. A Reviewer or Guest gets nothing, not even the list
//!   (it is customer mail); that is the safer of "read-only list" and "nothing".
//! * `pa_actions_list` is forwarded, then the reply is decorated
//!   ([`annotate_list`]): each `awaiting`/`edited` row the sender's ledger
//!   already approves (contract r3 `approval_status`, a read-only exchange)
//!   gains `ledgerApproval: {approver, at}`, so the Outbox can say "Approved by
//!   @x, sending" in the gap before the sender flips the status. Display only:
//!   a socket error leaves the rows unannotated, a `ledgerApproval` the child
//!   put on a row itself is stripped, and nothing here approves, sends or
//!   changes what `commit` requires.
//! * `pa_actions_commit` is the approval. In order: (1) the viewer is an
//!   Operator and is not the share's owner; (2) ask the sender socket for the
//!   draft (`get_draft`: canonical hash, status, the text it would send);
//!   (3) **recompute the canonical hash of the row the viewer says they saw**
//!   (the `seen` argument) and require it to equal the sender's hash;
//!   (4) send the contract's `approve` message, `source: "shell"`, approver =
//!   the *authenticated viewer* (principal id and username from the
//!   [`PrincipalCtx`], never the owner, never anything in the request body);
//!   (5) only on `ok`, answer the client that the draft is approved. The
//!   commit is NOT forwarded to the owner's child: in ledger mode the sender
//!   sends an `awaiting`/`edited` draft on the strength of a matching ledger
//!   approval (contract r2), so the owner's child takes no part in a share
//!   approval, and the sender's claim can't race a child-side status flip.
//!
//! **Fail closed.** No socket, a refused connection, a timeout, a malformed
//! reply, a sender refusal, a hash that does not match: the commit is never
//! forwarded and nothing is approved. A share request that cannot be shown to
//! carry a role is refused for the same reason.
//!
//! **Why the hash is recomputed here.** "The human approves exactly what they
//! saw" needs the approval bound to the bytes the UI displayed. The row the
//! sender hashes lives in a database the agent can write, so between the
//! viewer's screen and the socket the agent could change it; asking the sender
//! for "the hash of draft D now" would then record an approval of text nobody
//! read. The contract (§3.2–3.4) makes the hash a pure function of one row plus
//! the send-time identity, so the broker computes it over the row the viewer
//! *held* ([`SeenRow`]) and compares it with the sender's. Equality proves the
//! approved hash is that of the seen row (SHA-256 collision resistance), whatever
//! the agent did in between. The send-time identity (`smtpFrom`, the listmonk
//! template id, ...) comes from the sender's credentials, which the broker does
//! not hold, so it is read from the `Send-time <k>:` lines of `get_draft`'s
//! `renderedText`; a wrong or forged reading simply makes the hashes differ
//! (refusal), it can never make a bad approval succeed. The vectors in
//! `testdata/approvals-contract-vectors.json` (copied from the contract) pin
//! the implementation.
//!
//! **Scope of drafts.** `pa_action_drafts` has no project column, so a share
//! cannot be narrowed to "the drafts of project P". The scoping rule is: *any
//! share of an owner's project in which the viewer is an Operator may see and
//! decide **all** of that owner's drafts.* On the box that is exactly Ruby's
//! drafts (one share, "Ruby jobs"). If an owner who also produces unrelated
//! drafts shares a project with an Operator, the Operator sees those too; share
//! such an owner's projects with care.
//!
//! Everything here is Linux-only because [`super::proxy`] and [`super`] are.

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::proxy::Narrowing;
use crate::access::t1::BrokerCtx;
use crate::access::{AccessError, Code, Role, ShareCtx};
use crate::server::auth::PrincipalCtx;
use crate::server::rpc::RpcResponse;

/// Environment variable naming the sender's approvals socket.
pub const SOCKET_ENV: &str = "IKENGA_APPROVALS_SOCKET";
/// Where the box's `ikenga-sender-socket.service` listens.
pub const DEFAULT_SOCKET: &str = "/run/ikenga-sender/approvals.sock";
/// Contract version this client speaks.
pub const CONTRACT_VERSION: u32 = 1;
/// The contract's agent uid floor (`APPROVALS_AGENT_UID_MIN`): the per-account
/// `ik-*` users. The socket's *server* must not be one of them.
pub const DEFAULT_AGENT_UID_MIN: u32 = 20_000;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
/// A reply carries one draft's rendered text; the sender caps its *requests*
/// at 64 KiB, so a megabyte is far past anything honest.
const MAX_REPLY_BYTES: usize = 1024 * 1024;
const MAX_DRAFT_ID: usize = 512;

/// The RPCs this module gates inside a share.
pub const GATED_CMDS: [&str; 4] = [
    "pa_actions_list",
    "pa_actions_update",
    "pa_actions_commit",
    "pa_actions_reject",
];

/// Where the sender's socket is and who may serve it.
#[derive(Debug, Clone)]
pub struct ApprovalsConfig {
    pub socket: PathBuf,
    /// A socket served by a uid at or above this is refused (an agent
    /// account could otherwise plant a socket that answers `ok`).
    pub agent_uid_min: u32,
}

impl Default for ApprovalsConfig {
    fn default() -> Self {
        Self {
            socket: PathBuf::from(DEFAULT_SOCKET),
            agent_uid_min: DEFAULT_AGENT_UID_MIN,
        }
    }
}

impl ApprovalsConfig {
    /// [`SOCKET_ENV`] if set and non-empty, else [`DEFAULT_SOCKET`].
    pub fn from_env() -> Self {
        Self::from_value(std::env::var_os(SOCKET_ENV))
    }

    fn from_value(v: Option<std::ffi::OsString>) -> Self {
        let mut c = Self::default();
        if let Some(p) = v.filter(|p| !p.is_empty()) {
            c.socket = PathBuf::from(p);
        }
        c
    }

    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            ..Self::default()
        }
    }
}

// ─── The sender socket client ──────────────────────────────────────────────

/// Why a socket exchange did not produce an `ok` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderError {
    /// No socket, connection refused, timeout, a server that is not the
    /// sender: nothing was approved.
    Unavailable(String),
    /// The sender answered `{ok:false, error}`; the code is the contract's.
    Refused(String),
    /// The reply was not what the contract says.
    Malformed(String),
}

impl SenderError {
    /// The wording shown to the viewer.
    pub fn message(&self) -> String {
        match self {
            SenderError::Unavailable(_) => {
                "Nothing was approved: the box's approval service is not \
                 reachable. Mail from a shared project is sent by the box, and it only sends what \
                 a person approved through that service. Try again in a moment, or ask whoever \
                 runs the box."
                    .to_string()
            }
            SenderError::Refused(code) => refusal_text(code),
            SenderError::Malformed(_) => {
                "Nothing was approved: the box's approval service gave an \
                 answer this app does not understand."
                    .to_string()
            }
        }
    }

    /// The `{code}` of the RPC error: stable, for the UI and the logs.
    pub fn code(&self) -> &'static str {
        match self {
            SenderError::Unavailable(_) => "approval_unavailable",
            SenderError::Refused(_) => "approval_refused",
            SenderError::Malformed(_) => "approval_unavailable",
        }
    }

    /// The raw detail, for logs and `error_data.details` (never secrets: the
    /// sender's reply codes, a socket path, an io error).
    pub fn detail(&self) -> &str {
        match self {
            SenderError::Unavailable(d) | SenderError::Refused(d) | SenderError::Malformed(d) => d,
        }
    }
}

/// The contract §4.4 codes, as sentences for a person.
pub fn refusal_text(code: &str) -> String {
    let c = code.split(':').next().unwrap_or(code).trim();
    let why = match c {
        "unauthorized_approver" => {
            "you are not on the box's list of people who may approve sends".to_string()
        }
        "approver_is_owner" => "an account cannot approve its own draft".to_string(),
        "draft_not_found" => "the box has no such draft".to_string(),
        "ruby_db_unavailable" | "draft_unreadable" => {
            "the box could not read the draft".to_string()
        }
        "unauthorized_peer_uid" | "source_uid_mismatch" => {
            "the approval service does not accept this app as a writer (a box configuration problem)"
                .to_string()
        }
        "ledger_error" => "the approval ledger refused it (this draft may already have been \
             sent or approved)"
            .to_string(),
        "invalid_approver" | "missing_approver" | "invalid_source" | "invalid_payload_hash"
        | "missing_draft_id" | "invalid_json" | "unsupported_version" | "unknown_action"
        | "action_not_allowed_for_peer" | "line_buffer_overflow" => {
            format!("the approval request was malformed ({c})")
        }
        other => format!("refused ({other})"),
    };
    format!("Nothing was approved: {why}.")
}

/// What `get_draft` returns (contract §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftInfo {
    pub status: String,
    /// 64 lowercase hex characters.
    pub payload_hash: String,
    pub rendered_text: String,
}

/// One request/reply on the sender socket (newline-delimited JSON).
async fn exchange(cfg: &ApprovalsConfig, request: &Value) -> Result<Value, SenderError> {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(&cfg.socket)).await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return Err(SenderError::Unavailable(format!(
                "connect {}: {e}",
                cfg.socket.display()
            )))
        }
        Err(_) => {
            return Err(SenderError::Unavailable(format!(
                "connect {}: timed out",
                cfg.socket.display()
            )))
        }
    };
    // The server end must be the sender, not an agent's imitation of it.
    match stream.peer_cred() {
        Ok(c) if c.uid() < cfg.agent_uid_min => {}
        Ok(c) => {
            return Err(SenderError::Unavailable(format!(
                "the socket is served by uid {} (an agent account)",
                c.uid()
            )))
        }
        Err(e) => {
            return Err(SenderError::Unavailable(format!(
                "cannot read the socket's credentials: {e}"
            )))
        }
    }
    let mut line = request.to_string();
    line.push('\n');
    let run = async {
        let (read, mut write) = stream.into_split();
        write.write_all(line.as_bytes()).await?;
        write.flush().await?;
        let mut reader = BufReader::new(read.take(MAX_REPLY_BYTES as u64 + 1));
        let mut reply = String::new();
        reader.read_line(&mut reply).await?;
        Ok::<String, std::io::Error>(reply)
    };
    let reply = match tokio::time::timeout(EXCHANGE_TIMEOUT, run).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(SenderError::Unavailable(format!("socket io: {e}"))),
        Err(_) => return Err(SenderError::Unavailable("the sender did not answer".into())),
    };
    if reply.len() > MAX_REPLY_BYTES {
        return Err(SenderError::Malformed("reply too large".into()));
    }
    if !reply.ends_with('\n') {
        return Err(SenderError::Unavailable(
            "the sender closed the connection without an answer".into(),
        ));
    }
    let v: Value = serde_json::from_str(reply.trim_end())
        .map_err(|e| SenderError::Malformed(format!("reply is not JSON: {e}")))?;
    let Some(obj) = v.as_object() else {
        return Err(SenderError::Malformed("reply is not an object".into()));
    };
    match obj.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v),
        Some(false) => Err(SenderError::Refused(
            obj.get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        )),
        None => Err(SenderError::Malformed("reply has no `ok`".into())),
    }
}

fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `get_draft` (contract §4.2).
pub async fn get_draft(cfg: &ApprovalsConfig, draft_id: &str) -> Result<DraftInfo, SenderError> {
    let v = exchange(
        cfg,
        &json!({ "v": CONTRACT_VERSION, "action": "get_draft", "draftId": draft_id }),
    )
    .await?;
    if let Some(echo) = v.get("draftId").and_then(Value::as_str) {
        if echo != draft_id {
            return Err(SenderError::Malformed("reply names another draft".into()));
        }
    }
    let str_of = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let payload_hash = str_of("payloadHash")
        .filter(|h| is_hash(h))
        .ok_or_else(|| SenderError::Malformed("no valid payloadHash".into()))?;
    Ok(DraftInfo {
        status: str_of("status").unwrap_or_default(),
        payload_hash,
        rendered_text: str_of("renderedText").unwrap_or_default(),
    })
}

/// `approve` with `source: "shell"` (contract §4.1). The approver is the
/// authenticated viewer: `principal_id` + `username`.
pub async fn approve(
    cfg: &ApprovalsConfig,
    draft_id: &str,
    payload_hash: &str,
    principal_id: &str,
    username: &str,
) -> Result<(), SenderError> {
    let v = exchange(
        cfg,
        &json!({
            "v": CONTRACT_VERSION,
            "action": "approve",
            "draftId": draft_id,
            "payloadHash": payload_hash,
            "source": "shell",
            "approver": { "kind": "principal", "id": principal_id, "username": username },
        }),
    )
    .await?;
    // The sender echoes what it recorded; a reply for another draft or source
    // is not the approval we asked for.
    let echoed_draft = v.get("draftId").and_then(Value::as_str);
    let echoed_source = v.get("source").and_then(Value::as_str);
    if echoed_draft.is_some_and(|d| d != draft_id) || echoed_source.is_some_and(|s| s != "shell") {
        return Err(SenderError::Malformed(
            "the approval reply does not match the request".into(),
        ));
    }
    Ok(())
}

// ─── The canonical hash (contract §3) ──────────────────────────────────────

/// The row a viewer held, as the UI got it from `pa_actions_list`: the four
/// `pa_action_drafts` columns the hash covers (besides the id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenRow {
    pub channel: String,
    pub payload_json: String,
    pub edited_json: Option<String>,
    pub scheduled_at: Option<String>,
}

impl SeenRow {
    /// Parse the `seen` argument `{channel, payloadJson, editedJson, scheduledAt}`.
    pub fn from_args(v: &Value) -> Option<SeenRow> {
        let o = v.as_object()?;
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
        let opt = |k: &str| match o.get(k) {
            None | Some(Value::Null) => Some(None),
            Some(Value::String(s)) => Some(Some(s.clone())),
            _ => None,
        };
        Some(SeenRow {
            channel: s("channel")?,
            payload_json: s("payloadJson")?,
            edited_json: opt("editedJson")?,
            scheduled_at: opt("scheduledAt")?,
        })
    }
}

/// Why a row has no canonical hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotApprovable {
    /// `payload_json` is not valid JSON or not an object.
    Payload,
    /// A number outside `|n| <= 2^53 - 1` integers (contract §3.4 rule 4).
    Number,
    /// `edited_json` is non-empty but this parser rejects it. The viewer's
    /// `JSON.parse` accepts some documents `serde_json` does not (lone
    /// surrogate escapes, nesting past 128, numbers past f64), so the edit the
    /// viewer *read* cannot be hashed here. Omitting it would hash a row
    /// without the edit, which an agent could later make the sender's hash by
    /// clearing `edited_json`: refuse instead.
    Edited,
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Contract §3.4, canonical JSON text of `v`.
pub fn canonical_json(v: &Value) -> Result<String, NotApprovable> {
    let mut out = String::new();
    write_canonical(v, &mut out)?;
    Ok(out)
}

fn write_canonical(v: &Value, out: &mut String) -> Result<(), NotApprovable> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i.unsigned_abs() > MAX_SAFE_INTEGER as u64 {
                    return Err(NotApprovable::Number);
                }
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                if u > MAX_SAFE_INTEGER as u64 {
                    return Err(NotApprovable::Number);
                }
                out.push_str(&u.to_string());
            } else {
                // A float is approvable only if it is a safe integer (JS
                // `Number.isSafeInteger`): `1.0`, `1e2` and `-0` are; `1.5` is not.
                let f = n.as_f64().ok_or(NotApprovable::Number)?;
                if !f.is_finite() || f.fract() != 0.0 || f.abs() > MAX_SAFE_INTEGER {
                    return Err(NotApprovable::Number);
                }
                out.push_str(&(f as i64).to_string());
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(e, out)?;
            }
            out.push(']');
        }
        Value::Object(m) => {
            // UTF-16 code-unit order, not UTF-8 byte order (contract §3.4
            // rule 1): U+1D4B3 sorts before U+FF5E.
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_canonical(&m[k.as_str()], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Contract §3.2: the hashed document `H` for one row + send-time identity.
pub fn hashed_document(
    draft_id: &str,
    row: &SeenRow,
    identity: &Map<String, Value>,
) -> Result<Value, NotApprovable> {
    let parsed: Value =
        serde_json::from_str(&row.payload_json).map_err(|_| NotApprovable::Payload)?;
    if !parsed.is_object() {
        return Err(NotApprovable::Payload);
    }
    // `item` is the payload when truthy; `meta` / `source` siblings are not hashed.
    let payload = match parsed.get("item") {
        Some(item) if js_truthy(item) => item.clone(),
        _ => parsed.clone(),
    };
    let mut h = Map::new();
    h.insert("draftId".into(), json!(draft_id));
    h.insert("payload".into(), payload);
    // `edited` is omitted only when `edited_json` is NULL or empty. Text this
    // parser cannot read is NOT treated as absent (see `NotApprovable::Edited`).
    if let Some(text) = row.edited_json.as_deref().filter(|s| !s.is_empty()) {
        let e: Value = serde_json::from_str(text).map_err(|_| NotApprovable::Edited)?;
        h.insert("edited".into(), e);
    }
    h.insert(
        "scheduledAt".into(),
        row.scheduled_at.as_ref().map_or(Value::Null, |s| json!(s)),
    );
    h.insert("channel".into(), json!(row.channel));
    h.insert("sendIdentity".into(), Value::Object(identity.clone()));
    Ok(Value::Object(h))
}

/// Contract §3: `lowercase_hex(SHA-256(canonical(H)))`.
pub fn canonical_hash(
    draft_id: &str,
    row: &SeenRow,
    identity: &Map<String, Value>,
) -> Result<String, NotApprovable> {
    let text = canonical_json(&hashed_document(draft_id, row, identity)?)?;
    let digest = Sha256::digest(text.as_bytes());
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// The send-time identity as `get_draft`'s `renderedText` states it: every
/// `• **Send-time <key>:** <value>` line before the body. Values are strings
/// in the contract (§3.3); the renderer shows an empty one as `(unset)`.
///
/// This is a *reading*, not a trust decision: [`verify_seen`] accepts it only
/// if the hash computed with it equals the sender's own.
pub fn identity_from_rendered(rendered: &str) -> Map<String, Value> {
    let mut m = Map::new();
    for line in rendered.lines() {
        if line.starts_with("**Body:**") {
            break;
        }
        let Some(rest) = line.strip_prefix("• **Send-time ") else {
            continue;
        };
        let Some((key, value)) = rest.split_once(":** ") else {
            continue;
        };
        let value = if value == "(unset)" { "" } else { value };
        m.insert(key.to_string(), json!(value));
    }
    m
}

/// Why a seen row does not match the sender's draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeenMismatch {
    /// The row cannot be hashed at all.
    NotApprovable(NotApprovable),
    /// Its hash is not the sender's: the draft changed, or the identity
    /// could not be read back.
    Changed,
}

/// Does the row the viewer held hash to exactly what the sender would send?
pub fn verify_seen(draft_id: &str, seen: &SeenRow, info: &DraftInfo) -> Result<(), SeenMismatch> {
    let identity = identity_from_rendered(&info.rendered_text);
    let local = canonical_hash(draft_id, seen, &identity).map_err(SeenMismatch::NotApprovable)?;
    if local == info.payload_hash {
        Ok(())
    } else {
        Err(SeenMismatch::Changed)
    }
}

// ─── The gate in front of the proxy ────────────────────────────────────────

fn rpc_error(code: &str, message: &str, details: Value) -> Response {
    Json(RpcResponse::error_with_data(
        format!("{code}: {message}"),
        Some(json!({ "code": code, "message": message, "details": details })),
    ))
    .into_response()
}

fn access_error(code: Code, message: &str) -> Response {
    Json(crate::access::rpc::error_response(&AccessError::new(
        code, message,
    )))
    .into_response()
}

fn sender_error(e: &SenderError) -> Response {
    tracing::warn!(
        code = e.code(),
        detail = e.detail(),
        "share approval refused"
    );
    rpc_error(e.code(), &e.message(), json!({ "senderCode": e.detail() }))
}

/// The draft id of an RPC's args (`draftId` or `draft_id`, as the desktop
/// commands accept both).
fn draft_id_of(args: &Value) -> Option<String> {
    ["draftId", "draft_id"]
        .into_iter()
        .find_map(|k| args.get(k).and_then(Value::as_str))
        .map(str::to_string)
}

/// What [`gate`] decided for a request that was allowed to continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gated {
    /// Forward the request as it came.
    Unchanged,
    /// The ledger approval for this draft is recorded: answer the client,
    /// forward nothing (the sender sends it from the ledger).
    Approved { draft_id: String },
    /// Forward the list, then mark the rows the ledger has already approved
    /// ([`annotate_list`]). Display only: the annotation gates nothing.
    AnnotateList,
}

/// Share-mode checks and the ledger approval for the gated `pa_actions_*`
/// RPCs. `Ok(Gated)` = forward; `Err(response)` = answer the client, forward
/// nothing. A request that is not through a share, or not a gated command, is
/// [`Gated::Unchanged`].
pub async fn gate(
    cfg: &ApprovalsConfig,
    ctx: &PrincipalCtx,
    narrowing: &Narrowing,
    cmd: &str,
    args: &Value,
) -> Result<Gated, Response> {
    if !GATED_CMDS.contains(&cmd) {
        return Ok(Gated::Unchanged);
    }
    let snapshot = narrowing.snapshot::<BrokerCtx>();
    let share_header = narrowing
        .headers
        .iter()
        .any(|(n, _)| n.as_str().starts_with("x-ikenga-share"));
    let Some(share) = snapshot.and_then(|b| b.access.share.as_ref()) else {
        if share_header {
            // Share headers without the broker's own decision: it cannot be
            // shown who the viewer is acting as. Fail closed.
            return Err(access_error(
                Code::Forbidden,
                "this share request carries no access decision",
            ));
        }
        return Ok(Gated::Unchanged);
    };
    let owner = snapshot.and_then(|b| b.owner.as_ref());

    check_role(share, ctx, owner.map(|o| o.id))?;

    match cmd {
        "pa_actions_commit" => approve_commit(cfg, ctx, args).await,
        "pa_actions_list" => Ok(Gated::AnnotateList),
        _ => Ok(Gated::Unchanged),
    }
}

/// The viewer must be an Operator of this share, and not the owner.
fn check_role(
    share: &ShareCtx,
    ctx: &PrincipalCtx,
    owner: Option<crate::executor::PrincipalId>,
) -> Result<(), Response> {
    if share.role != Some(Role::Operator) {
        return Err(access_error(
            Code::Forbidden,
            "reviewing and approving drafts in a shared project needs the Operator role",
        ));
    }
    if owner.is_none_or(|o| o == ctx.principal.id) {
        // No owner to route to, or the viewer *is* the owner: not a share.
        return Err(access_error(
            Code::Forbidden,
            "an account cannot approve drafts through its own share",
        ));
    }
    Ok(())
}

async fn approve_commit(
    cfg: &ApprovalsConfig,
    ctx: &PrincipalCtx,
    args: &Value,
) -> Result<Gated, Response> {
    let bad = |m: &str| access_error(Code::InvalidRequest, m);
    let Some(draft_id) = draft_id_of(args).filter(|d| !d.is_empty() && d.len() <= MAX_DRAFT_ID)
    else {
        return Err(bad("`draftId` is required"));
    };
    // A share approval binds to what the viewer saw. A client that cannot say
    // what that was (an old UI) is refused, not guessed at.
    let Some(seen) = args.get("seen").and_then(SeenRow::from_args) else {
        return Err(rpc_error(
            "approval_refused",
            "Nothing was approved: this page did not say which version of the draft you reviewed. \
             Reload the page and try again.",
            json!({ "reason": "missing_seen" }),
        ));
    };

    let info = match get_draft(cfg, &draft_id).await {
        Ok(i) => i,
        Err(e) => return Err(sender_error(&e)),
    };
    if !matches!(info.status.as_str(), "awaiting" | "edited") {
        return Err(rpc_error(
            "approval_refused",
            &format!(
                "Nothing was approved: this draft is no longer waiting for approval (it is {}).",
                if info.status.is_empty() {
                    "in another state"
                } else {
                    info.status.as_str()
                }
            ),
            json!({ "reason": "status", "status": info.status }),
        ));
    }
    match verify_seen(&draft_id, &seen, &info) {
        Ok(()) => {}
        Err(SeenMismatch::NotApprovable(why)) => {
            return Err(rpc_error(
                "approval_refused",
                "Nothing was approved: this draft contains values the approval ledger cannot \
                 bind to, so it cannot be approved.",
                json!({ "reason": "not_approvable", "why": format!("{why:?}") }),
            ))
        }
        Err(SeenMismatch::Changed) => {
            return Err(rpc_error(
                "draft_changed",
                "Nothing was approved: the draft on the box is not the version you reviewed (it \
                 changed, or could not be checked). Reload the page and review it again.",
                json!({ "reason": "hash_mismatch" }),
            ))
        }
    }

    // The approver is the authenticated viewer, taken from the credential the
    // broker resolved. Nothing in the request body names one.
    if let Err(e) = approve(
        cfg,
        &draft_id,
        &info.payload_hash,
        &ctx.principal.id.to_string(),
        &ctx.principal.username,
    )
    .await
    {
        return Err(sender_error(&e));
    }
    tracing::info!(
        draft = %draft_id,
        approver = %ctx.principal.username,
        hash = &info.payload_hash[..12],
        "share approval recorded in the sender ledger"
    );
    // Nothing is forwarded: the sender sends the draft on this approval.
    Ok(Gated::Approved { draft_id })
}

// ─── Showing ledger approvals in the list (contract r3, display only) ──────

/// Most ids one `approval_status` request may carry (contract r3).
const STATUS_BATCH: usize = 200;
/// The list is polled; a slow sender must not slow it by more than this.
const STATUS_TIMEOUT: Duration = Duration::from_secs(2);
/// A list reply larger than this is not annotated (answered 502, like any
/// proxy failure). Far past any honest outbox.
const MAX_LIST_BODY: usize = 32 * 1024 * 1024;
/// The key the UI reads. Set only by [`annotate_list`].
const ANNOTATION_KEY: &str = "ledgerApproval";

/// One approval the sender's ledger holds for a draft's current hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerApproval {
    pub principal_id: String,
    pub username: String,
    pub at: String,
}

/// `approval_status` (contract r3): which of `ids` (at most [`STATUS_BATCH`])
/// have an unconsumed approval for their current hash. Read-only. Ids the
/// request did not name, malformed entries and consumed ones are dropped.
pub async fn approval_status(
    cfg: &ApprovalsConfig,
    ids: &[String],
) -> Result<std::collections::HashMap<String, LedgerApproval>, SenderError> {
    let v = exchange(
        cfg,
        &json!({ "v": CONTRACT_VERSION, "action": "approval_status", "draftIds": ids }),
    )
    .await?;
    let Some(map) = v.get("approvals").and_then(Value::as_object) else {
        return Err(SenderError::Malformed("reply has no `approvals`".into()));
    };
    let mut out = std::collections::HashMap::new();
    for (id, a) in map {
        if !ids.iter().any(|i| i == id) {
            continue;
        }
        if a.get("consumed").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let text = |p: &str| {
            a.pointer(p)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let (Some(principal_id), Some(username), Some(at)) = (
            text("/approver/principalId"),
            text("/approver/username"),
            text("/at"),
        ) else {
            continue;
        };
        out.insert(
            id.clone(),
            LedgerApproval {
                principal_id,
                username,
                at,
            },
        );
    }
    Ok(out)
}

/// The rows of a `pa_actions_list` reply (`{ok:true,data:[...]}`), if it is one.
fn list_rows(reply: &mut Value) -> Option<&mut Vec<Value>> {
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    reply.get_mut("data")?.as_array_mut()
}

fn is_list_reply(reply: &Value) -> bool {
    reply.get("ok").and_then(Value::as_bool) == Some(true)
        && reply.get("data").is_some_and(Value::is_array)
}

/// Remove every `ledgerApproval` from the rows of any reply whose `data` is an
/// array, whatever its `ok` says: the client treats any truthy `ok` (`1`,
/// `"yes"`) as success, so a strict `ok == true` test alone would let a forged
/// mark through on `{"ok":1,...}`. Returns whether anything was removed.
fn strip_annotations(reply: &mut Value) -> bool {
    let Some(rows) = reply.get_mut("data").and_then(Value::as_array_mut) else {
        return false;
    };
    let mut stripped = false;
    for row in rows.iter_mut() {
        if let Some(o) = row.as_object_mut() {
            stripped |= o.remove(ANNOTATION_KEY).is_some();
        }
    }
    stripped
}

/// Annotate a parsed list reply in place. Every `ledgerApproval` already on a
/// row has been removed first ([`strip_annotations`]): the rows come from the
/// owner's child (an agent), which must not be able to put words in a human's
/// mouth. Then each awaiting/edited row the sender's ledger approves gets
/// `ledgerApproval: {approver, at}`. A sender that cannot be asked leaves the
/// rows unannotated.
async fn annotate_value(cfg: &ApprovalsConfig, reply: &mut Value) {
    let Some(rows) = list_rows(reply) else {
        return;
    };
    let ids: Vec<String> = rows
        .iter()
        .filter(|r| matches!(r["status"].as_str(), Some("awaiting" | "edited")))
        .filter_map(|r| r["id"].as_str())
        .filter(|id| !id.is_empty() && id.len() <= MAX_DRAFT_ID)
        .map(str::to_string)
        .collect();
    let mut approved = std::collections::HashMap::new();
    for chunk in ids.chunks(STATUS_BATCH) {
        match tokio::time::timeout(STATUS_TIMEOUT, approval_status(cfg, chunk)).await {
            Ok(Ok(found)) => approved.extend(found),
            Ok(Err(e)) => {
                // Display only: the list is still correct without it. Polled
                // every few seconds, so not a warning.
                tracing::debug!(
                    detail = e.detail(),
                    "approval_status unavailable; list unannotated"
                );
                break;
            }
            Err(_) => {
                tracing::debug!("approval_status timed out; list unannotated");
                break;
            }
        }
    }
    for row in rows.iter_mut() {
        let Some(a) = row["id"].as_str().and_then(|id| approved.get(id)) else {
            continue;
        };
        if let Some(o) = row.as_object_mut() {
            o.insert(
                ANNOTATION_KEY.into(),
                json!({
                    "approver": { "principalId": a.principal_id, "username": a.username },
                    "at": a.at,
                }),
            );
        }
    }
}

/// Add the ledger's approvals to a forwarded `pa_actions_list` response.
/// A JSON reply that is not a `{ok:true,data:[...]}` list and carries no
/// `ledgerApproval` passes through byte for byte. A 2xx body that is not JSON
/// serde can read is answered 502: the client's `JSON.parse` accepts things
/// serde refuses (a lone `\ud800` escape), so relaying it would relay rows
/// that were never sanitised. This never changes a request, a status or an
/// approval; it only decorates what the viewer is shown.
pub async fn annotate_list(cfg: &ApprovalsConfig, resp: Response) -> Response {
    if !resp.status().is_success() {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    if parts
        .headers
        .contains_key(axum::http::header::CONTENT_ENCODING)
    {
        // The rows could not be inspected (and so not sanitised).
        tracing::warn!("pa_actions_list reply is content-encoded; refusing to relay it");
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let bytes = match axum::body::to_bytes(body, MAX_LIST_BODY).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("pa_actions_list reply unreadable: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let mut reply = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("pa_actions_list reply is not JSON; refusing to relay it: {e}");
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    let stripped = strip_annotations(&mut reply);
    let list = is_list_reply(&reply);
    let out = if list || stripped {
        if list {
            annotate_value(cfg, &mut reply).await;
        }
        match serde_json::to_vec(&reply) {
            Ok(v) => axum::body::Bytes::from(v),
            Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
        }
    } else {
        bytes
    };
    parts.headers.remove(axum::http::header::CONTENT_LENGTH);
    Response::from_parts(parts, axum::body::Body::from(out))
}

/// For tests and diagnostics.
pub fn socket_path(cfg: &ApprovalsConfig) -> &Path {
    &cfg.socket
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::{CapSet, Tier};
    use crate::access::ctx::{AccessCtx, RequestMeta, Via};
    use crate::executor::{Principal, PrincipalId};
    use crate::server::auth::Credential;
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;

    // ── the contract vectors ────────────────────────────────────────────

    /// `docs/approvals-contract-vectors.json` of the royalti-co repo (contract
    /// v1, main 71fef38f), verbatim. The sender, the bridge and this broker all
    /// recompute it.
    const VECTORS: &str = include_str!("testdata/approvals-contract-vectors.json");

    /// Contract §3.3, for the vector env (production reads it back from the
    /// sender's rendered text instead).
    fn derive_identity(channel: &str, payload: &Value, env: &Value) -> Map<String, Value> {
        let mut m = Map::new();
        let env_s = |k: &str| env.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match channel {
            "smtp" => {
                m.insert("smtpFrom".into(), json!(env_s("RUBY_IMAP_USER")));
            }
            "listmonk" => {
                let t = env_s("LISTMONK_TEMPLATE_ID");
                m.insert(
                    "listmonkTemplateId".into(),
                    json!(if t.is_empty() { "1" } else { &t }),
                );
            }
            "buffer" => {
                let id = payload.get("channelId").filter(|v| js_truthy(v));
                match id {
                    Some(Value::String(s)) => {
                        m.insert("bufferChannelId".into(), json!(s));
                    }
                    Some(other) => {
                        m.insert("bufferChannelId".into(), json!(other.to_string()));
                    }
                    None => {
                        let r = payload
                            .get("recipient")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        m.insert("bufferChannelLookupByRecipientName".into(), json!(r));
                    }
                }
            }
            _ => {}
        }
        m
    }

    fn seen_of(row: &Value) -> SeenRow {
        SeenRow {
            channel: row["channel"].as_str().unwrap().to_string(),
            payload_json: row["payload_json"].as_str().unwrap().to_string(),
            edited_json: row["edited_json"].as_str().map(str::to_string),
            scheduled_at: row["scheduled_at"].as_str().map(str::to_string),
        }
    }

    #[test]
    fn the_canonical_hash_matches_every_contract_vector() {
        let doc: Value = serde_json::from_str(VECTORS).unwrap();
        assert_eq!(doc["contract"], 1);
        let vectors = doc["vectors"].as_array().unwrap();
        assert_eq!(vectors.len(), 9, "the contract ships nine vectors");
        for v in vectors {
            let name = v["name"].as_str().unwrap();
            let row = &v["row"];
            let seen = seen_of(row);
            let id = row["id"].as_str().unwrap();
            // The payload `P` the identity rule reads is the hashed document's.
            let probe = hashed_document(id, &seen, &Map::new()).unwrap();
            let identity = derive_identity(&seen.channel, &probe["payload"], &v["env"]);
            let h = hashed_document(id, &seen, &identity).unwrap();
            assert_eq!(
                canonical_json(&h).unwrap(),
                v["canonical"].as_str().unwrap(),
                "{name}: canonical text"
            );
            assert_eq!(
                canonical_hash(id, &seen, &identity).unwrap(),
                v["sha256"].as_str().unwrap(),
                "{name}: sha256"
            );
        }
    }

    #[test]
    fn keys_sort_by_utf16_code_units_not_utf8_bytes() {
        // U+1D4B3 is D835 DCB3 in UTF-16 (sorts first); U+FF5E is FF5E. In
        // UTF-8 bytes it is the other way round.
        let v: Value = serde_json::from_str("{\"\u{ff5e}\":1,\"\u{1d4b3}\":2,\"a\":3}").unwrap();
        assert_eq!(
            canonical_json(&v).unwrap(),
            "{\"a\":3,\"\u{1d4b3}\":2,\"\u{ff5e}\":1}"
        );
    }

    #[test]
    fn numbers_outside_the_contract_are_not_approvable() {
        for ok in [
            "0",
            "-7",
            "9007199254740991",
            "-9007199254740991",
            "1.0",
            "1e2",
            "-0",
        ] {
            let v: Value = serde_json::from_str(ok).unwrap();
            assert!(canonical_json(&v).is_ok(), "{ok}");
        }
        assert_eq!(
            canonical_json(&serde_json::from_str("-0").unwrap()).unwrap(),
            "0"
        );
        assert_eq!(
            canonical_json(&serde_json::from_str("1e2").unwrap()).unwrap(),
            "100"
        );
        for bad in [
            "1.5",
            "9007199254740992",
            "-9007199254740992",
            "1e300",
            "18446744073709551615",
        ] {
            let v: Value = serde_json::from_str(bad).unwrap();
            assert_eq!(canonical_json(&v), Err(NotApprovable::Number), "{bad}");
        }
    }

    #[test]
    fn a_payload_that_is_not_a_json_object_has_no_hash() {
        for payload in ["not json", "[1]", "\"x\"", "null", ""] {
            let row = SeenRow {
                channel: "smtp".into(),
                payload_json: payload.into(),
                edited_json: None,
                scheduled_at: None,
            };
            assert_eq!(
                canonical_hash("d", &row, &Map::new()),
                Err(NotApprovable::Payload),
                "{payload:?}"
            );
        }
    }

    /// `JSON.parse` (the viewer's UI, the sender) accepts these; `serde_json`
    /// does not. Dropping the edit would make the seen row hash like the same
    /// row with `edited_json` NULL, so an agent could show the viewer the edit,
    /// then clear it after the approval is captured. They have no hash.
    #[test]
    fn an_edit_this_parser_cannot_read_is_not_approvable_not_omitted() {
        let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200));
        for edited in [
            r#"{"body":"BENIGN","x":"\ud800"}"#.to_string(),
            r#"{"body":"BENIGN","x":1e400}"#.to_string(),
            format!(r#"{{"body":"BENIGN","x":{deep}}}"#),
            "{not json".to_string(),
        ] {
            let row = SeenRow {
                channel: "smtp".into(),
                payload_json: r#"{"item":{"subject":"s","body":"MALICIOUS"}}"#.into(),
                edited_json: Some(edited.clone()),
                scheduled_at: None,
            };
            assert_eq!(
                canonical_hash("d", &row, &Map::new()),
                Err(NotApprovable::Edited),
                "{edited}"
            );
            let info = DraftInfo {
                status: "awaiting".into(),
                // The sender's hash of the same row with `edited_json` NULL.
                payload_hash: canonical_hash(
                    "d",
                    &SeenRow {
                        edited_json: None,
                        ..row.clone()
                    },
                    &Map::new(),
                )
                .unwrap(),
                rendered_text: String::new(),
            };
            assert_eq!(
                verify_seen("d", &row, &info),
                Err(SeenMismatch::NotApprovable(NotApprovable::Edited)),
                "{edited}"
            );
        }
    }

    #[test]
    fn an_edit_or_a_schedule_or_a_channel_changes_the_hash() {
        let base = SeenRow {
            channel: "smtp".into(),
            payload_json: r#"{"item":{"subject":"s","body":"b"}}"#.into(),
            edited_json: None,
            scheduled_at: None,
        };
        let h = |r: &SeenRow| canonical_hash("d", r, &Map::new()).unwrap();
        let h0 = h(&base);
        let mut edited = base.clone();
        edited.edited_json = Some(r#"{"body":"b2"}"#.into());
        let mut scheduled = base.clone();
        scheduled.scheduled_at = Some("2026-10-21 09:00:00".into());
        let mut chan = base.clone();
        chan.channel = "resend".into();
        let mut other_id = base.clone();
        other_id.payload_json = r#"{"item":{"subject":"s","body":"b"},"meta":{"x":1}}"#.into();
        for (what, r) in [
            ("edit", &edited),
            ("schedule", &scheduled),
            ("channel", &chan),
        ] {
            assert_ne!(h(r), h0, "{what}");
        }
        // meta / source siblings of `item` are not hashed (contract §3.2).
        assert_eq!(h(&other_id), h0);
        // An empty `edited_json` leaves the key out, like NULL.
        let mut r = base.clone();
        r.edited_json = Some(String::new());
        assert_eq!(h(&r), h0);
        // But a valid `null` edit is hashed as null.
        let mut r = base.clone();
        r.edited_json = Some("null".into());
        assert_ne!(h(&r), h0);
        // The draft id is part of the hash.
        assert_ne!(canonical_hash("d2", &base, &Map::new()).unwrap(), h0);
    }

    // ── identity read back from the sender's rendered text ──────────────

    const RENDERED: &str = "**Outbox Action Draft Approval Required**\n\
• **Draft ID:** ruby-reply:em-1001\n\
• **Channel:** smtp\n\
• **Send-time smtpFrom:** ruby@royalti.io\n\
• **Other field x:** • **Send-time smtpFrom:** evil@example.com\n\
\n**Body:**\n> Hi\n• **Send-time smtpFrom:** after-the-body";

    #[test]
    fn the_send_time_identity_is_read_from_before_the_body() {
        let m = identity_from_rendered(RENDERED);
        assert_eq!(m.get("smtpFrom"), Some(&json!("ruby@royalti.io")));
        assert_eq!(m.len(), 1);
        let unset = identity_from_rendered("• **Send-time smtpFrom:** (unset)\n**Body:**");
        assert_eq!(unset.get("smtpFrom"), Some(&json!("")));
    }

    fn vector_info(name: &str, rendered: &str) -> (SeenRow, String, DraftInfo) {
        let doc: Value = serde_json::from_str(VECTORS).unwrap();
        let v = doc["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["name"] == name)
            .unwrap()
            .clone();
        let seen = seen_of(&v["row"]);
        let id = v["row"]["id"].as_str().unwrap().to_string();
        let info = DraftInfo {
            status: "awaiting".into(),
            payload_hash: v["sha256"].as_str().unwrap().to_string(),
            rendered_text: rendered.to_string(),
        };
        (seen, id, info)
    }

    #[test]
    fn verify_seen_accepts_the_row_the_sender_hashed() {
        let (seen, id, info) = vector_info("smtp-ruby-reply", RENDERED);
        assert_eq!(verify_seen(&id, &seen, &info), Ok(()));
    }

    #[test]
    fn verify_seen_refuses_a_row_that_changed_after_it_was_shown() {
        let (mut seen, id, info) = vector_info("smtp-ruby-reply", RENDERED);
        // The viewer's copy differs from the row the sender hashed: here the
        // viewer saw an edited draft that the sender's row does not have.
        seen.edited_json = Some(r#"{"body":"something else"}"#.into());
        assert_eq!(verify_seen(&id, &seen, &info), Err(SeenMismatch::Changed));
    }

    #[test]
    fn a_forged_send_time_identity_cannot_produce_a_match() {
        let (seen, id, info) = vector_info("smtp-ruby-reply", RENDERED);
        let forged = DraftInfo {
            rendered_text: RENDERED.replace("ruby@royalti.io", "evil@example.com"),
            ..info
        };
        assert_eq!(verify_seen(&id, &seen, &forged), Err(SeenMismatch::Changed));
    }

    // ── a fake sender on a Unix socket ──────────────────────────────────

    type Handler = Arc<dyn Fn(&Value) -> String + Send + Sync>;

    struct FakeSender {
        _dir: tempfile::TempDir,
        path: PathBuf,
        seen: Arc<Mutex<Vec<Value>>>,
    }

    impl FakeSender {
        async fn start(handler: Handler) -> FakeSender {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("approvals.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let log = seen.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let (log, handler) = (log.clone(), handler.clone());
                    tokio::spawn(async move {
                        let (r, mut w) = stream.into_split();
                        let mut line = String::new();
                        if BufReader::new(r).read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let req: Value = serde_json::from_str(line.trim()).unwrap();
                        let reply = handler(&req);
                        log.lock().unwrap().push(req);
                        let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                    });
                }
            });
            FakeSender {
                _dir: dir,
                path,
                seen,
            }
        }

        fn cfg(&self) -> ApprovalsConfig {
            // The test process is not an agent account.
            ApprovalsConfig {
                socket: self.path.clone(),
                agent_uid_min: u32::MAX,
            }
        }

        fn requests(&self) -> Vec<Value> {
            self.seen.lock().unwrap().clone()
        }

        fn actions(&self) -> Vec<String> {
            self.requests()
                .iter()
                .map(|r| r["action"].as_str().unwrap().to_string())
                .collect()
        }
    }

    /// A sender that knows one draft: `d-1` (the contract's baseline vector
    /// row), in `status`, with the sender's identity and hash, and records an
    /// approval reply per `approve_reply`.
    fn sender_for_vector(status: &'static str, approve_reply: &'static str) -> Handler {
        let (_, _, info) = vector_info("smtp-ruby-reply", RENDERED);
        let hash = info.payload_hash;
        Arc::new(move |req: &Value| match req["action"].as_str().unwrap() {
            "get_draft" if req["draftId"] == "ruby-reply:em-1001" => json!({
                "ok": true, "draftId": "ruby-reply:em-1001", "status": status,
                "payloadHash": hash, "renderedText": RENDERED,
            })
            .to_string(),
            "get_draft" => json!({"ok": false, "error": "draft_not_found"}).to_string(),
            "approve" => approve_reply.to_string(),
            _ => json!({"ok": false, "error": "unknown_action"}).to_string(),
        })
    }

    const OK_REPLY: &str = r#"{"ok":true,"draftId":"ruby-reply:em-1001","source":"shell"}"#;

    #[tokio::test]
    async fn the_client_speaks_the_contract_for_get_draft_and_approve() {
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let info = get_draft(&s.cfg(), "ruby-reply:em-1001").await.unwrap();
        assert_eq!(info.status, "awaiting");
        assert_eq!(info.payload_hash.len(), 64);
        approve(
            &s.cfg(),
            "ruby-reply:em-1001",
            &info.payload_hash,
            "pid-1",
            "ada",
        )
        .await
        .unwrap();
        let reqs = s.requests();
        assert_eq!(
            reqs[0],
            json!({"v":1,"action":"get_draft","draftId":"ruby-reply:em-1001"})
        );
        assert_eq!(
            reqs[1],
            json!({
                "v": 1, "action": "approve", "draftId": "ruby-reply:em-1001",
                "payloadHash": info.payload_hash, "source": "shell",
                "approver": {"kind": "principal", "id": "pid-1", "username": "ada"},
            })
        );
        assert_eq!(
            get_draft(&s.cfg(), "nope").await,
            Err(SenderError::Refused("draft_not_found".into()))
        );
    }

    #[tokio::test]
    async fn an_absent_socket_is_unavailable_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ApprovalsConfig::at(dir.path().join("none.sock"));
        let e = get_draft(&cfg, "d").await.unwrap_err();
        assert!(matches!(e, SenderError::Unavailable(_)), "{e:?}");
        assert_eq!(e.code(), "approval_unavailable");
        assert!(
            e.message().starts_with("Nothing was approved"),
            "{}",
            e.message()
        );
        // A path that exists but is not a socket is the same.
        let file = dir.path().join("plain");
        std::fs::write(&file, b"x").unwrap();
        assert!(matches!(
            get_draft(&ApprovalsConfig::at(file), "d").await,
            Err(SenderError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn a_socket_served_by_an_agent_uid_is_not_the_sender() {
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        // Everyone at or above uid 0 counts as an agent: the test process is.
        let cfg = ApprovalsConfig {
            agent_uid_min: 0,
            ..s.cfg()
        };
        let e = get_draft(&cfg, "ruby-reply:em-1001").await.unwrap_err();
        assert!(
            matches!(&e, SenderError::Unavailable(d) if d.contains("agent account")),
            "{e:?}"
        );
        assert!(s.requests().is_empty(), "nothing is sent to a fake sender");
    }

    #[tokio::test]
    async fn a_server_that_closes_without_answering_or_answers_garbage_fails_closed() {
        let silent: Handler = Arc::new(|_| String::new());
        let s = FakeSender::start(silent).await;
        // An empty line is not JSON.
        assert!(matches!(
            get_draft(&s.cfg(), "d").await,
            Err(SenderError::Malformed(_))
        ));
        let garbage: Handler = Arc::new(|_| "{\"ok\":true,\"payloadHash\":\"short\"}".to_string());
        let s = FakeSender::start(garbage).await;
        assert!(matches!(
            get_draft(&s.cfg(), "d").await,
            Err(SenderError::Malformed(_))
        ));
        let no_ok: Handler = Arc::new(|_| "{\"hello\":1}".to_string());
        let s = FakeSender::start(no_ok).await;
        assert!(matches!(
            approve(&s.cfg(), "d", &"a".repeat(64), "p", "u").await,
            Err(SenderError::Malformed(_))
        ));
    }

    #[test]
    fn every_contract_refusal_code_reads_as_a_sentence() {
        for code in [
            "unauthorized_approver",
            "approver_is_owner",
            "draft_not_found",
            "ruby_db_unavailable",
            "draft_unreadable",
            "unauthorized_peer_uid",
            "source_uid_mismatch",
            "ledger_error: cannot re-arm consumed approval x",
            "invalid_approver",
            "missing_approver",
            "invalid_source",
            "invalid_payload_hash",
            "invalid_json",
            "unsupported_version",
            "unknown_action",
            "action_not_allowed_for_peer",
            "line_buffer_overflow",
            "something_new",
        ] {
            let t = refusal_text(code);
            assert!(t.starts_with("Nothing was approved: "), "{code}: {t}");
            assert!(t.ends_with('.'), "{code}: {t}");
        }
        assert!(refusal_text("unauthorized_approver").contains("not on the box's list"));
        assert!(refusal_text("something_new").contains("something_new"));
    }

    #[test]
    fn the_socket_path_is_configurable_and_defaults_to_the_boxs() {
        assert_eq!(
            ApprovalsConfig::default().socket,
            PathBuf::from("/run/ikenga-sender/approvals.sock")
        );
        assert_eq!(
            ApprovalsConfig::from_value(None).socket,
            PathBuf::from(DEFAULT_SOCKET)
        );
        assert_eq!(
            ApprovalsConfig::from_value(Some("".into())).socket,
            PathBuf::from(DEFAULT_SOCKET)
        );
        assert_eq!(
            ApprovalsConfig::from_value(Some("/x/y.sock".into())).socket,
            PathBuf::from("/x/y.sock")
        );
    }

    // ── the gate ────────────────────────────────────────────────────────

    fn principal(name: &str) -> Principal {
        Principal {
            id: PrincipalId::new_v7(),
            username: name.into(),
            unix_name: format!("ik-{name}"),
            uid: 20_000,
            gid: 20_000,
            home: format!("/srv/{name}").into(),
            shell: "/bin/sh".into(),
        }
    }

    fn pctx(p: &Principal) -> PrincipalCtx {
        PrincipalCtx {
            principal: p.clone(),
            via: Credential::Session {
                session_id: "s".into(),
            },
        }
    }

    /// What the broker's narrower would have decided for `viewer` in a share of
    /// `owner`'s project with `role`.
    fn narrowing(viewer: &Principal, owner: &Principal, role: Option<Role>) -> Narrowing {
        let share = ShareCtx {
            project_key: format!("{}/ruby-jobs", owner.id),
            project_id: "ruby-jobs".into(),
            member_principal_id: Some(viewer.id.to_string()),
            member_device_id: None,
            role,
            artifact_path: None,
            owner_approval: false,
        };
        let b = BrokerCtx {
            access: AccessCtx {
                principal_id: viewer.id,
                via: Via::Session {
                    session_id: "s".into(),
                },
                device_id: None,
                tier: Tier::Full,
                share: Some(share.clone()),
                share_headers: false,
                caps: CapSet::ALL,
                admin_strength: true,
                is_admin: false,
                meta: RequestMeta::default(),
            },
            owner: Some(owner.clone()),
            socket: 1,
        };
        Narrowing {
            headers: crate::access::share::to_child_headers(&share)
                .into_iter()
                .map(|(n, v)| {
                    (
                        axum::http::HeaderName::from_static(n),
                        axum::http::HeaderValue::from_str(&v).unwrap(),
                    )
                })
                .collect(),
            target: Some(owner.clone()),
            snapshot: Some(Arc::new(b)),
        }
    }

    fn seen_args(draft_id: &str) -> Value {
        let doc: Value = serde_json::from_str(VECTORS).unwrap();
        let row = &doc["vectors"][0]["row"];
        json!({
            "draftId": draft_id,
            "seen": {
                "channel": row["channel"], "payloadJson": row["payload_json"],
                "editedJson": row["edited_json"], "scheduledAt": row["scheduled_at"],
            }
        })
    }

    async fn error_of(r: Result<Gated, Response>) -> Value {
        let res = r.err().expect("the gate refuses");
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    struct Parties {
        ada: Principal,
        ruby: Principal,
    }

    fn parties() -> Parties {
        Parties {
            ada: principal("ada"),
            ruby: principal("ruby"),
        }
    }

    const DRAFT: &str = "ruby-reply:em-1001";

    #[tokio::test]
    async fn an_operator_approves_as_themselves_for_exactly_the_one_draft() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        let mut args = seen_args(DRAFT);
        // Smuggle a second id key: only the approved id is approved.
        args["draft_id"] = json!("some-other-draft");
        let out = gate(&s.cfg(), &pctx(&p.ada), &n, "pa_actions_commit", &args).await;
        assert_eq!(
            out.ok(),
            Some(Gated::Approved { draft_id: DRAFT.to_string() })
        );
        assert_eq!(s.actions(), ["get_draft", "approve"]);
        let approve = &s.requests()[1];
        assert_eq!(approve["source"], "shell");
        assert_eq!(approve["approver"]["kind"], "principal");
        // The viewer, never the owner.
        assert_eq!(approve["approver"]["id"], p.ada.id.to_string());
        assert_eq!(approve["approver"]["username"], "ada");
        assert_ne!(approve["approver"]["id"], p.ruby.id.to_string());
        assert_ne!(approve["approver"]["username"], "ruby");
        let (_, _, info) = vector_info("smtp-ruby-reply", RENDERED);
        assert_eq!(approve["payloadHash"], info.payload_hash);
    }

    #[tokio::test]
    async fn the_approver_comes_from_the_credential_not_from_the_body() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        let mut args = seen_args(DRAFT);
        args["approver"] = json!({"kind": "principal", "id": "x", "username": "chinedum"});
        args["username"] = json!("chinedum");
        args["source"] = json!("mattermost");
        gate(&s.cfg(), &pctx(&p.ada), &n, "pa_actions_commit", &args)
            .await
            .ok()
            .unwrap();
        let approve = &s.requests()[1];
        assert_eq!(approve["approver"]["username"], "ada");
        assert_eq!(approve["source"], "shell");
    }

    #[tokio::test]
    async fn a_reviewer_a_guest_and_a_member_with_no_role_decide_nothing() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        for role in [Some(Role::Reviewer), Some(Role::Guest), None] {
            let n = narrowing(&p.ada, &p.ruby, role);
            for cmd in GATED_CMDS {
                let body =
                    error_of(gate(&s.cfg(), &pctx(&p.ada), &n, cmd, &seen_args(DRAFT)).await).await;
                assert_eq!(body["ok"], false, "{role:?} {cmd}");
                let err = body["error"].as_str().unwrap();
                assert!(err.starts_with("forbidden: "), "{role:?} {cmd}: {err}");
                assert!(err.contains("Operator"), "{err}");
            }
        }
        assert!(
            s.requests().is_empty(),
            "the sender is never asked on a refused role"
        );
    }

    #[tokio::test]
    async fn an_operator_may_list_update_and_reject_and_the_request_is_forwarded_unchanged() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        for cmd in ["pa_actions_update", "pa_actions_reject"] {
            let out = gate(&s.cfg(), &pctx(&p.ada), &n, cmd, &json!({"draftId": DRAFT})).await;
            assert_eq!(out.ok(), Some(Gated::Unchanged), "{cmd}");
        }
        // A list is forwarded and then decorated; the gate itself asks nothing.
        let out = gate(&s.cfg(), &pctx(&p.ada), &n, "pa_actions_list", &json!({})).await;
        assert_eq!(out.ok(), Some(Gated::AnnotateList));
        assert!(s.requests().is_empty(), "only a commit touches the ledger");
    }

    #[tokio::test]
    async fn other_commands_and_non_share_requests_are_untouched() {
        let p = parties();
        let cfg = ApprovalsConfig::at("/nonexistent/approvals.sock");
        let share = narrowing(&p.ada, &p.ruby, Some(Role::Reviewer));
        // A command the gate does not own, even from a Reviewer.
        assert_eq!(
            gate(&cfg, &pctx(&p.ada), &share, "fs_read", &json!({}))
                .await
                .ok(),
            Some(Gated::Unchanged)
        );
        // Her own workspace (desktop-like): no share, no gate, no socket.
        let own = Narrowing::default();
        for cmd in GATED_CMDS {
            assert_eq!(
                gate(&cfg, &pctx(&p.ada), &own, cmd, &json!({"draftId": "x"}))
                    .await
                    .ok(),
                Some(Gated::Unchanged),
                "{cmd}"
            );
        }
    }

    #[tokio::test]
    async fn a_share_request_with_no_access_decision_is_refused() {
        let p = parties();
        let mut n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        n.snapshot = None;
        let body = error_of(
            gate(
                &ApprovalsConfig::at("/none"),
                &pctx(&p.ada),
                &n,
                "pa_actions_commit",
                &seen_args(DRAFT),
            )
            .await,
        )
        .await;
        assert!(
            body["error"].as_str().unwrap().starts_with("forbidden: "),
            "{body}"
        );
    }

    #[tokio::test]
    async fn the_owner_cannot_approve_through_its_own_share() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        // The viewer IS the owner (a corrupt selection): refused before the socket.
        let n = narrowing(&p.ruby, &p.ruby, Some(Role::Operator));
        let body = error_of(
            gate(
                &s.cfg(),
                &pctx(&p.ruby),
                &n,
                "pa_actions_commit",
                &seen_args(DRAFT),
            )
            .await,
        )
        .await;
        assert!(
            body["error"].as_str().unwrap().starts_with("forbidden: "),
            "{body}"
        );
        assert!(s.requests().is_empty());
    }

    #[tokio::test]
    async fn with_the_socket_absent_approving_fails_closed_and_forwards_nothing() {
        let p = parties();
        let dir = tempfile::tempdir().unwrap();
        let cfg = ApprovalsConfig::at(dir.path().join("absent.sock"));
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        let body = error_of(
            gate(
                &cfg,
                &pctx(&p.ada),
                &n,
                "pa_actions_commit",
                &seen_args(DRAFT),
            )
            .await,
        )
        .await;
        assert_eq!(body["ok"], false);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("approval_unavailable: Nothing was approved"),
            "{body}"
        );
        assert_eq!(body["error_data"]["code"], "approval_unavailable");
    }

    #[tokio::test]
    async fn the_senders_refusal_codes_reach_the_viewer_and_nothing_is_forwarded() {
        let p = parties();
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        for (code, phrase) in [
            ("unauthorized_approver", "not on the box's list"),
            ("approver_is_owner", "its own draft"),
            ("source_uid_mismatch", "does not accept this app"),
            (
                "ledger_error: cannot re-arm consumed approval x",
                "ledger refused",
            ),
        ] {
            let reply: &'static str = Box::leak(
                json!({"ok": false, "error": code})
                    .to_string()
                    .into_boxed_str(),
            );
            let s = FakeSender::start(sender_for_vector("awaiting", reply)).await;
            let body = error_of(
                gate(
                    &s.cfg(),
                    &pctx(&p.ada),
                    &n,
                    "pa_actions_commit",
                    &seen_args(DRAFT),
                )
                .await,
            )
            .await;
            let err = body["error"].as_str().unwrap();
            assert!(
                err.starts_with("approval_refused: Nothing was approved"),
                "{err}"
            );
            assert!(err.contains(phrase), "{code}: {err}");
            assert_eq!(body["error_data"]["details"]["senderCode"], code);
        }
    }

    #[tokio::test]
    async fn a_draft_that_changed_since_the_viewer_saw_it_is_not_approved() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        let mut args = seen_args(DRAFT);
        // The viewer's copy says one thing; the sender's row hashes another.
        args["seen"]["payloadJson"] = json!(args["seen"]["payloadJson"]
            .as_str()
            .unwrap()
            .replace("pat@example.com", "evil@example.com"));
        let body =
            error_of(gate(&s.cfg(), &pctx(&p.ada), &n, "pa_actions_commit", &args).await).await;
        assert_eq!(body["error_data"]["code"], "draft_changed", "{body}");
        assert_eq!(s.actions(), ["get_draft"], "no approve message was sent");
    }

    #[tokio::test]
    async fn a_commit_that_does_not_say_what_was_seen_is_refused_before_the_socket() {
        let p = parties();
        let s = FakeSender::start(sender_for_vector("awaiting", OK_REPLY)).await;
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        for args in [
            json!({"draftId": DRAFT}),
            json!({"draftId": DRAFT, "seen": null}),
            json!({"draftId": DRAFT, "seen": {"channel": "smtp"}}),
            json!({"draftId": DRAFT, "seen": {"channel": "smtp", "payloadJson": 1, "editedJson": null, "scheduledAt": null}}),
        ] {
            let body =
                error_of(gate(&s.cfg(), &pctx(&p.ada), &n, "pa_actions_commit", &args).await).await;
            assert!(
                body["error"]
                    .as_str()
                    .unwrap()
                    .starts_with("approval_refused: "),
                "{args}: {body}"
            );
        }
        let body = error_of(
            gate(
                &s.cfg(),
                &pctx(&p.ada),
                &n,
                "pa_actions_commit",
                &json!({"seen": {}}),
            )
            .await,
        )
        .await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("invalid_request: "),
            "{body}"
        );
        assert!(s.requests().is_empty());
    }

    #[tokio::test]
    async fn a_draft_that_is_no_longer_awaiting_is_not_approved_again() {
        let p = parties();
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        for status in ["committed", "sent", "rejected"] {
            let s = FakeSender::start(sender_for_vector(status, OK_REPLY)).await;
            let body = error_of(
                gate(
                    &s.cfg(),
                    &pctx(&p.ada),
                    &n,
                    "pa_actions_commit",
                    &seen_args(DRAFT),
                )
                .await,
            )
            .await;
            let err = body["error"].as_str().unwrap();
            assert!(err.contains(status), "{err}");
            assert_eq!(s.actions(), ["get_draft"]);
        }
        // `edited` is committable, like `awaiting`.
        let s = FakeSender::start(sender_for_vector("edited", OK_REPLY)).await;
        assert!(gate(
            &s.cfg(),
            &pctx(&p.ada),
            &n,
            "pa_actions_commit",
            &seen_args(DRAFT)
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn a_reply_for_another_draft_is_not_an_approval() {
        let p = parties();
        let n = narrowing(&p.ada, &p.ruby, Some(Role::Operator));
        let s = FakeSender::start(sender_for_vector(
            "awaiting",
            r#"{"ok":true,"draftId":"someone-else","source":"shell"}"#,
        ))
        .await;
        let body = error_of(
            gate(
                &s.cfg(),
                &pctx(&p.ada),
                &n,
                "pa_actions_commit",
                &seen_args(DRAFT),
            )
            .await,
        )
        .await;
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .starts_with("approval_unavailable: "),
            "{body}"
        );
    }

    // ── ledger approvals shown in the list (contract r3, display only) ──

    /// `{ "<id>": <the sender's approval entry> }`.
    fn approval_for(id: &str, who: &str) -> Value {
        json!({ id: {
            "approver": { "principalId": format!("pid-{who}"), "username": who },
            "source": "mattermost", "at": "2026-10-24T10:00:00Z", "consumed": false,
        }})
    }

    fn row(id: &str, status: &str) -> Value {
        json!({ "id": id, "status": status, "channel": "email" })
    }

    fn list_response(rows: Value) -> Response {
        Json(json!({ "ok": true, "data": rows })).into_response()
    }

    async fn body_of(r: Response) -> Value {
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A sender whose `approval_status` answers from `approvals` (an object
    /// keyed by draft id), whatever ids were asked.
    fn status_sender(approvals: Value) -> Handler {
        Arc::new(move |req: &Value| match req["action"].as_str().unwrap() {
            "approval_status" => json!({ "ok": true, "approvals": approvals }).to_string(),
            _ => json!({"ok": false, "error": "unknown_action"}).to_string(),
        })
    }

    #[tokio::test]
    async fn a_ledger_approved_awaiting_or_edited_row_is_marked_and_the_rest_are_not() {
        let mut approvals = approval_for("d-1", "ada");
        approvals["d-2"] = approval_for("d-2", "bola")["d-2"].clone();
        // The sender is only asked about awaiting/edited rows, but a sender
        // that answers for another id must not annotate it.
        approvals["d-3"] = approval_for("d-3", "eve")["d-3"].clone();
        let s = FakeSender::start(status_sender(approvals)).await;
        let rows = json!([
            row("d-1", "awaiting"),
            row("d-2", "edited"),
            row("d-3", "committed"),
            row("d-4", "awaiting"),
        ]);
        let out = body_of(annotate_list(&s.cfg(), list_response(rows)).await).await;
        let data = out["data"].as_array().unwrap();
        assert_eq!(data[0]["ledgerApproval"]["approver"]["username"], "ada");
        assert_eq!(
            data[0]["ledgerApproval"]["approver"]["principalId"],
            "pid-ada"
        );
        assert_eq!(data[0]["ledgerApproval"]["at"], "2026-10-24T10:00:00Z");
        assert_eq!(data[1]["ledgerApproval"]["approver"]["username"], "bola");
        assert!(
            data[2].get("ledgerApproval").is_none(),
            "committed is not asked"
        );
        assert!(
            data[3].get("ledgerApproval").is_none(),
            "no approval, no mark"
        );
        // The row is otherwise as the child sent it.
        assert_eq!(data[0]["status"], "awaiting");
        assert_eq!(data[0]["channel"], "email");

        // One read-only exchange, naming exactly the awaiting/edited ids.
        let reqs = s.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0]["v"], 1);
        assert_eq!(reqs[0]["action"], "approval_status");
        assert_eq!(reqs[0]["draftIds"], json!(["d-1", "d-2", "d-4"]));
    }

    #[tokio::test]
    async fn without_awaiting_rows_the_sender_is_not_asked() {
        let s = FakeSender::start(status_sender(json!({}))).await;
        let rows = json!([row("d-1", "sent"), row("d-2", "failed")]);
        let out = body_of(annotate_list(&s.cfg(), list_response(rows.clone())).await).await;
        assert_eq!(out["data"], rows);
        assert!(s.requests().is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_sender_leaves_the_rows_unannotated_and_the_list_ok() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ApprovalsConfig::at(dir.path().join("absent.sock"));
        let rows = json!([row("d-1", "awaiting")]);
        let out = body_of(annotate_list(&cfg, list_response(rows.clone())).await).await;
        assert_eq!(out["ok"], true);
        assert_eq!(out["data"], rows);
    }

    #[tokio::test]
    async fn a_refusing_malformed_or_silent_sender_changes_nothing() {
        let rows = json!([row("d-1", "awaiting")]);
        let replies: [&'static str; 4] = [
            // an older sender that predates r3
            r#"{"ok":false,"error":"unknown_action"}"#,
            r#"{"ok":false,"error":"action_not_allowed_for_peer"}"#,
            "not json",
            r#"{"ok":true}"#,
        ];
        for reply in replies {
            let s = FakeSender::start(Arc::new(move |_: &Value| reply.to_string())).await;
            let out = body_of(annotate_list(&s.cfg(), list_response(rows.clone())).await).await;
            assert_eq!(out["data"], rows, "{reply}");
        }
    }

    #[tokio::test]
    async fn a_socket_served_by_an_agent_uid_annotates_nothing() {
        let s = FakeSender::start(status_sender(approval_for("d-1", "ada"))).await;
        let cfg = ApprovalsConfig {
            agent_uid_min: 0,
            ..s.cfg()
        };
        let rows = json!([row("d-1", "awaiting")]);
        let out = body_of(annotate_list(&cfg, list_response(rows.clone())).await).await;
        assert_eq!(out["data"], rows);
        assert!(s.requests().is_empty(), "nothing is even sent to it");
    }

    #[tokio::test]
    async fn a_consumed_or_malformed_or_unrequested_approval_is_not_shown() {
        let approvals = json!({
            "d-1": { "approver": {"principalId": "p", "username": "ada"},
                     "at": "t", "consumed": true },
            "d-2": { "approver": {"principalId": "p"}, "at": "t" },
            "d-3": { "approver": {"principalId": "p", "username": "ada"} },
            "d-4": { "approver": {"principalId": 7, "username": "ada"}, "at": "t" },
            "elsewhere": { "approver": {"principalId": "p", "username": "ada"}, "at": "t" },
        });
        let s = FakeSender::start(status_sender(approvals)).await;
        let rows = json!([
            row("d-1", "awaiting"),
            row("d-2", "awaiting"),
            row("d-3", "awaiting"),
            row("d-4", "awaiting"),
        ]);
        let out = body_of(annotate_list(&s.cfg(), list_response(rows.clone())).await).await;
        assert_eq!(out["data"], rows);
    }

    #[tokio::test]
    async fn an_annotation_the_owners_child_wrote_is_removed() {
        // The rows come from the owner's daemon, i.e. from the agent. It does
        // not get to say "approved by @chinedum".
        let mut forged = row("d-1", "awaiting");
        forged["ledgerApproval"] =
            json!({"approver": {"principalId": "x", "username": "chinedum"}, "at": "t"});
        let mut forged_sent = row("d-2", "sent");
        forged_sent["ledgerApproval"] = forged["ledgerApproval"].clone();
        let rows = json!([forged, forged_sent]);

        // With the sender down...
        let dir = tempfile::tempdir().unwrap();
        let cfg = ApprovalsConfig::at(dir.path().join("absent.sock"));
        let out = body_of(annotate_list(&cfg, list_response(rows.clone())).await).await;
        assert!(out["data"][0].get("ledgerApproval").is_none());
        assert!(out["data"][1].get("ledgerApproval").is_none());

        // ...and with it up but holding no approval.
        let s = FakeSender::start(status_sender(json!({}))).await;
        let out = body_of(annotate_list(&s.cfg(), list_response(rows)).await).await;
        assert!(out["data"][0].get("ledgerApproval").is_none());
        assert!(out["data"][1].get("ledgerApproval").is_none());
    }

    #[tokio::test]
    async fn more_than_the_cap_is_asked_in_batches_of_at_most_200() {
        let s = FakeSender::start(status_sender(json!({}))).await;
        let rows: Vec<Value> = (0..450)
            .map(|i| row(&format!("d-{i}"), "awaiting"))
            .collect();
        let out = body_of(annotate_list(&s.cfg(), list_response(json!(rows))).await).await;
        assert_eq!(out["data"].as_array().unwrap().len(), 450);
        let sizes: Vec<usize> = s
            .requests()
            .iter()
            .map(|r| r["draftIds"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, [200, 200, 50]);
    }

    #[tokio::test]
    async fn replies_that_are_not_a_successful_list_pass_through_untouched() {
        let s = FakeSender::start(status_sender(approval_for("d-1", "ada"))).await;
        // The child's own error.
        let err = Json(json!({"ok": false, "error": "boom"})).into_response();
        assert_eq!(
            body_of(annotate_list(&s.cfg(), err).await).await,
            json!({"ok": false, "error": "boom"})
        );
        // A non-2xx status.
        let bad = (StatusCode::BAD_GATEWAY, "nope").into_response();
        let out = annotate_list(&s.cfg(), bad).await;
        assert_eq!(out.status(), StatusCode::BAD_GATEWAY);
        // Data that is not a list.
        let odd = Json(json!({"ok": true, "data": {"x": 1}})).into_response();
        assert_eq!(
            body_of(annotate_list(&s.cfg(), odd).await).await,
            json!({"ok": true, "data": {"x": 1}})
        );
        assert!(s.requests().is_empty());
    }

    #[tokio::test]
    async fn a_list_reply_serde_cannot_read_is_not_relayed() {
        let s = FakeSender::start(status_sender(json!({}))).await;
        // Not JSON at all.
        let text = "plain".into_response();
        assert_eq!(
            annotate_list(&s.cfg(), text).await.status(),
            StatusCode::BAD_GATEWAY
        );
        // A lone surrogate: serde refuses it, the browser's JSON.parse does
        // not, so relaying the bytes would relay an unsanitised forged mark.
        let forged = concat!(
            r#"{"ok":true,"data":[{"id":"d-1","status":"awaiting","note":"\ud800","#,
            r#""ledgerApproval":{"approver":{"principalId":"x","username":"chinedum"},"at":"t"}}]}"#
        );
        let r = Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(forged))
            .unwrap();
        assert_eq!(
            annotate_list(&s.cfg(), r).await.status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn a_forged_mark_under_a_truthy_but_not_true_ok_is_removed() {
        // The client treats any truthy `ok` as success.
        let s = FakeSender::start(status_sender(json!({}))).await;
        let mut forged = row("d-1", "awaiting");
        forged["ledgerApproval"] =
            json!({"approver": {"principalId": "x", "username": "chinedum"}, "at": "t"});
        let r = Json(json!({ "ok": 1, "data": [forged] })).into_response();
        let out = body_of(annotate_list(&s.cfg(), r).await).await;
        assert_eq!(out["ok"], 1);
        assert!(out["data"][0].get("ledgerApproval").is_none(), "{out}");
        assert_eq!(out["data"][0]["id"], "d-1");
        assert!(s.requests().is_empty(), "only ok:true lists are annotated");
    }

    #[tokio::test]
    async fn an_encoded_list_reply_is_not_relayed() {
        let s = FakeSender::start(status_sender(json!({}))).await;
        let mut r = list_response(json!([row("d-1", "awaiting")]));
        r.headers_mut().insert(
            axum::http::header::CONTENT_ENCODING,
            axum::http::HeaderValue::from_static("gzip"),
        );
        assert_eq!(
            annotate_list(&s.cfg(), r).await.status(),
            StatusCode::BAD_GATEWAY
        );
    }
}
