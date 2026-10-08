//! Claude terminal hooks and statusline for daemon terminals (remote-access
//! gap audit rank 11): the headless counterpart of the desktop's iyke bridge
//! receivers (`iyke::hooks`, `iyke::statusline`).
//!
//! On the desktop a `claude` launched in an Ikenga terminal is handed a
//! `--settings` file whose hooks and statusline `curl` the iyke bridge, which
//! emits `hooks://event`, `hooks://decision` and `statusline://snapshot` to
//! the webview. A daemon terminal got none of that: the browser has no
//! `iyke_endpoint`, and the daemon's `pty_spawn` dropped `settingsPath`. Now
//! the daemon writes the same per-terminal settings file itself
//! ([`TermHooks::wire`]), pointed at its own endpoint ([`router`]), and
//! publishes the same event names and payloads on the event bus
//! (`server::events`) for `/ws/events` to relay.
//!
//! # Scope (user decision, this round)
//!
//! The statusline HUD and the permission inbox. NOT the tool feed or a hooks
//! replay: `hooks://event` carries only the events the inbox's lifecycle
//! needs ([`published`]) — an ask (`PermissionRequest`, a held `PreToolUse`),
//! what resolves it (`PostToolUse`, `PostToolUseFailure`, `Stop`,
//! `SessionEnd`, `UserPromptSubmit`) and `Notification` — and those carry no
//! `tool_output`. Every other event is accepted and dropped.
//!
//! # Authentication
//!
//! The endpoint is mounted OUTSIDE `auth_middleware` (a hook is curl, with no
//! cookie and no operator bearer), behind its own credential:
//!
//! * a **per-terminal secret**, minted when the settings file is written
//!   (256 random bits), good for that terminal's two routes only. It is
//!   neither the daemon's `IKENGA_AUTH_TOKEN` nor a device token, so a leaked
//!   hook secret opens nothing else;
//! * carried in a 0600 **header file** that the hook commands name with
//!   `curl -H @file`, never inline: an inline secret is in curl's argv for
//!   the duration of every hook, readable through `/proc` by any other user
//!   on the host, and the statusline fires constantly. The file sits in a
//!   0700 directory under `--data-dir`, owned by the uid the daemon (T0) or
//!   the principal child (T1) runs as, which is also the uid of the PTY;
//! * bound to the terminal id: the `?terminal=` query must name a terminal
//!   this process registered, and the secret must be THAT terminal's. The
//!   body's own idea of its terminal is ignored. Unknown terminal, missing
//!   header and wrong secret are one indistinguishable 401;
//! * revoked when the PTY exits ([`TermHooks::revoke`]), so a secret copied
//!   out of a dead terminal opens nothing.
//!
//! **Principals (T1).** Each principal child runs its own router and its own
//! [`TermHooks`]; a hook secret is only known to the child that minted it. The
//! endpoint is on that child's loopback port, so another principal who finds
//! the port faces the same 401, and what a terminal publishes goes only to
//! the bus of the child that owns it (`/ws/events` is proxied to the caller's
//! own child, never another's).
//!
//! # The permission decision
//!
//! A held `PreToolUse` (opt-in per terminal by the same
//! `permissions.hold_terminal_<id>` setting the desktop reads) parks the
//! response for [`GATE_HOLD_SECS`] while the inbox answers through the
//! `term_hooks_decide` arm. The arm removes the held request from the map
//! before answering, so a decision is **single-use** — the second gets
//! `gated:false`, as does one that arrives after the hold timed out. A
//! timeout, a dropped connection and a full table all **deny**, with the
//! desktop's exact response, never allow. `term_hooks_decide` needs `approve`
//! on an `owner`-class credential, so the routing preference (G-ACCESS §5.1)
//! and shares apply as they do to `permission_decide`.
//!
//! # The notification row and the audit trail
//!
//! A held ask also writes the desktop's `permission` row, and every way it can
//! end flips that row and is audited: see [`super::hook_asks`]. The invariant
//! this file keeps for it is **one remover**: whoever takes a request out of
//! the held table ([`TermHooks::take`]) owns how it ended — a decision
//! ([`TermHooks::decide_held_record`]), the hold running out
//! ([`TermHooks::expire`]), the PTY exiting ([`TermHooks::revoke`]) or the
//! hook's connection dropping ([`HeldGuard`]) — so a row is flipped and an
//! outcome audited exactly once, and never left looking pending.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tracing::{debug, warn};

use super::events::{EventBus, Topic};
use super::hook_asks::{AskRecord, HookAsks, Outcome};
use super::rpc::RpcResponse;
use super::shared::hook_settings::{self as settings_doc, Auth, Wiring, GATE_HOLD_SECS};
use super::{ct_eq, AppState, ServerConfig};

/// The two routes a terminal's hooks and statusline POST to.
pub const EVENT_PATH: &str = "/term-hooks/event";
pub const STATUSLINE_PATH: &str = "/term-hooks/statusline";

/// Directory, under `--data-dir`, that holds the per-terminal files.
const DIR_NAME: &str = "term-hooks";

/// Largest hook/statusline body accepted (a `PostToolUse` carries the tool's
/// whole response). Checked only after the secret is, so an unauthenticated
/// caller reads nothing.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Held asks at once. A runaway claude cannot have more than a handful in
/// flight; past this a new gate is denied rather than parked.
const MAX_HELD: usize = 64;

/// Largest statusline snapshot kept and broadcast. A real one is about a
/// kilobyte; the cap keeps a misbehaving client from filling the map.
const MAX_SNAPSHOT: usize = 256 * 1024;

/// Statusline snapshots kept (one per terminal). Bounded so a long-lived
/// daemon that has seen many terminals does not grow without limit.
const MAX_SNAPSHOTS: usize = 1024;

const NOT_AVAILABLE: &str = "Not available on this server";

/// The settings file name for a terminal — the same name the desktop uses, so
/// the frontend computes one path for both.
pub fn terminal_file_name(terminal_id: &str) -> String {
    format!("claude-hooks-{terminal_id}.json")
}

fn header_file_name(terminal_id: &str) -> String {
    format!("claude-hooks-{terminal_id}.hdr")
}

/// A terminal id that is safe to put in a file name and a query string.
fn valid_terminal_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// 256 random bits, hex.
fn mint_secret() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn mint_request_id() -> String {
    format!("perm-{}", uuid::Uuid::new_v4().simple())
}

/// A hook secret handed out by [`TermHooks::wire`]; [`TermHooks::revoke`]
/// takes it back only if it is still the live one (a respawn of the same
/// terminal id mints a new secret, which the old PTY's exit must not revoke).
#[derive(Debug, Clone)]
pub struct Grant {
    terminal_id: String,
    secret: String,
}

/// The wire shape the inbox posts back, and `hooks://decision`'s payload
/// (the desktop's `HookDecision`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecision {
    #[serde(rename = "requestId")]
    pub request_id: String,
    /// `approved` | `denied`.
    pub decision: String,
}

struct Held {
    terminal_id: String,
    tx: oneshot::Sender<HookDecision>,
    /// The ask's row machinery (`None` without a database).
    record: Option<Arc<AskRecord>>,
}

/// See the module doc. One per router, i.e. per daemon process — under T1,
/// one per principal child.
pub struct TermHooks {
    /// `<data-dir>/term-hooks`; `None` without a data dir.
    dir: Option<PathBuf>,
    events: Arc<EventBus>,
    /// How long a held `PreToolUse` waits. [`GATE_HOLD_SECS`] in production.
    hold: Duration,
    /// terminal id → live hook secret.
    terminals: Mutex<HashMap<String, String>>,
    /// request id → the parked hook response.
    held: Mutex<HashMap<String, Held>>,
    /// terminal id → its latest statusline snapshot.
    snapshots: Mutex<HashMap<String, Value>>,
    /// The row-and-audit machinery for held asks, attached by the router once
    /// it knows its database and access state.
    asks: RwLock<Option<Arc<HookAsks>>>,
}

impl TermHooks {
    pub fn new(data_dir: Option<&Path>, events: Arc<EventBus>) -> Arc<Self> {
        let hold = Duration::from_secs(GATE_HOLD_SECS);
        // A test builds its router on its own thread, so a thread-local is a
        // per-test knob that cannot leak into a neighbour.
        #[cfg(test)]
        let hold = tests::TEST_HOLD.with(|h| h.get()).unwrap_or(hold);
        let dir = data_dir.map(|d| d.join(DIR_NAME));
        // Files from an earlier run carry secrets nothing recognises any more.
        if let Some(dir) = &dir {
            sweep(dir);
        }
        Arc::new(Self {
            dir,
            events,
            hold,
            terminals: Mutex::new(HashMap::new()),
            held: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            asks: RwLock::new(None),
        })
    }

    /// Give held asks their notification row and audit trail.
    pub(super) fn attach_asks(&self, asks: Arc<HookAsks>) {
        if let Ok(mut slot) = self.asks.write() {
            *slot = Some(asks);
        }
    }

    pub(super) fn asks(&self) -> Option<Arc<HookAsks>> {
        self.asks.read().ok().and_then(|a| a.clone())
    }

    /// The ask is over by `outcome`: flip its row, audit what nobody decided.
    fn conclude(&self, record: Option<Arc<AskRecord>>, outcome: Outcome) {
        if let Some(asks) = self.asks() {
            asks.conclude(record, outcome);
        }
    }

    /// Where the hooks POST, or why the server cannot take them.
    fn base_url(config: &ServerConfig) -> Result<String, String> {
        if !cfg!(unix) {
            return Err(format!(
                "{NOT_AVAILABLE}: terminal hooks need a Unix host (they are curl commands)"
            ));
        }
        if config.port == 0 {
            return Err(format!(
                "{NOT_AVAILABLE}: the server has not finished binding its port"
            ));
        }
        let ip: std::net::IpAddr = config
            .host
            .parse()
            .map_err(|_| format!("{NOT_AVAILABLE}: the server's bind address is not an IP"))?;
        // A wildcard bind is reachable on loopback; a specific address (a
        // tailnet IP) may NOT be, so use exactly that one.
        let ip = if ip.is_unspecified() {
            match ip {
                std::net::IpAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                std::net::IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
            }
        } else {
            ip
        };
        Ok(format!(
            "http://{}",
            std::net::SocketAddr::new(ip, config.port)
        ))
    }

    fn dir_or_reason(&self) -> Result<&Path, String> {
        self.dir.as_deref().ok_or_else(|| {
            format!(
                "{NOT_AVAILABLE}: it runs without a data folder (--data-dir), so there is \
                 nowhere to keep per-terminal hook settings"
            )
        })
    }

    /// `term_hooks_info`: the directory the frontend builds
    /// `claude-hooks-<terminal>.json` paths under, or why there is none.
    pub fn info(&self, config: &ServerConfig) -> Value {
        match Self::base_url(config).and_then(|_| self.dir_or_reason().map(Path::to_path_buf)) {
            Ok(dir) => json!({ "settingsDir": dir.to_string_lossy(), "reason": null }),
            Err(reason) => json!({ "settingsDir": null, "reason": reason }),
        }
    }

    /// Write the per-terminal settings file (and the header file it names),
    /// register the secret, and return its [`Grant`]. `settings_path` is the
    /// path the caller already put in `claude`'s argv; it must be exactly the
    /// one this server would have chosen, so a caller cannot make the daemon
    /// write anywhere else.
    pub fn wire(
        &self,
        config: &ServerConfig,
        terminal_id: &str,
        settings_path: &str,
    ) -> Result<Grant, String> {
        let base = Self::base_url(config)?;
        let dir = self.dir_or_reason()?;
        if !valid_terminal_id(terminal_id) {
            return Err(format!(
                "{NOT_AVAILABLE}: terminal id `{terminal_id}` cannot name a hook settings file"
            ));
        }
        let settings = dir.join(terminal_file_name(terminal_id));
        if Path::new(settings_path) != settings {
            return Err(format!(
                "hook settings path {settings_path} is not this terminal's ({})",
                settings.display()
            ));
        }
        make_private_dir(dir).map_err(|e| format!("hook settings directory: {e}"))?;

        let secret = mint_secret();
        let header = dir.join(header_file_name(terminal_id));
        write_private(
            &header,
            format!("Authorization: Bearer {secret}\n").as_bytes(),
        )
        .map_err(|e| format!("hook header file: {e}"))?;
        let doc = settings_doc::build(&Wiring {
            base_url: &base,
            hook_path: EVENT_PATH,
            statusline_path: STATUSLINE_PATH,
            auth: Auth::HeaderFile(&header),
            terminal_id: Some(terminal_id),
        });
        let body = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        write_private(&settings, &body).map_err(|e| format!("hook settings file: {e}"))?;

        // Registered last: nothing can present the secret before the files
        // that name it exist, and a failed write registers nothing.
        if let Ok(mut t) = self.terminals.lock() {
            t.insert(terminal_id.to_string(), secret.clone());
        }
        // A respawned terminal starts with a clean HUD.
        if let Ok(mut s) = self.snapshots.lock() {
            s.remove(terminal_id);
        }
        Ok(Grant {
            terminal_id: terminal_id.to_string(),
            secret,
        })
    }

    /// The PTY is gone: its secret stops working and its files are removed —
    /// unless the terminal was already re-wired with a newer secret.
    pub fn revoke(&self, grant: &Grant) {
        let live = match self.terminals.lock() {
            Ok(mut t) => {
                if t.get(&grant.terminal_id)
                    .is_some_and(|s| ct_eq(s, &grant.secret))
                {
                    t.remove(&grant.terminal_id);
                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        };
        if !live {
            return;
        }
        // Parked hooks of a dead terminal: dropping them denies them, and the
        // inbox is told, as for any ask that ends unanswered. This is their
        // remover, so it owns the outcome: their rows are flipped now, not
        // when the (possibly already gone) hook connection notices.
        let orphaned: Vec<(String, Held)> = match self.held.lock() {
            Ok(mut h) => {
                let ids: Vec<String> = h
                    .iter()
                    .filter(|(_, held)| held.terminal_id == grant.terminal_id)
                    .map(|(id, _)| id.clone())
                    .collect();
                ids.into_iter()
                    .filter_map(|id| h.remove(&id).map(|held| (id, held)))
                    .collect()
            }
            Err(_) => Vec::new(),
        };
        for (request_id, held) in orphaned {
            self.events.publish(
                Topic::HooksDecision,
                HookDecision {
                    request_id,
                    decision: "denied".into(),
                },
            );
            self.conclude(held.record, Outcome::TerminalEnded);
        }
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_file(dir.join(terminal_file_name(&grant.terminal_id)));
            let _ = std::fs::remove_file(dir.join(header_file_name(&grant.terminal_id)));
        }
    }

    /// Whether `secret` is the live one for `terminal_id`.
    fn authenticates(&self, terminal_id: &str, secret: &str) -> bool {
        self.terminals
            .lock()
            .ok()
            .and_then(|t| t.get(terminal_id).cloned())
            .is_some_and(|expected| ct_eq(&expected, secret))
    }

    /// `term_hooks_statusline_snapshot`: every terminal's latest snapshot.
    pub fn snapshots(&self) -> Value {
        let map = self.snapshots.lock().map(|g| g.clone()).unwrap_or_default();
        json!(map)
    }

    /// Take a parked request out of the table. The caller is its remover and
    /// owns how it ended (see the module doc).
    fn take(&self, request_id: &str) -> Option<Held> {
        self.held.lock().ok().and_then(|mut h| h.remove(request_id))
    }

    /// Answer a parked gate. `Some` (the ask's record, if it has one) only if
    /// a request was parked under `request_id` and took the answer; the
    /// request is removed first, so a second answer (a replay, or a late one
    /// after the timeout) is `None`.
    pub(super) fn decide_held_record(
        &self,
        request_id: &str,
        approved: bool,
    ) -> Option<Option<Arc<AskRecord>>> {
        let held = self.take(request_id)?;
        let decision = HookDecision {
            request_id: request_id.to_string(),
            decision: if approved { "approved" } else { "denied" }.into(),
        };
        if held.tx.send(decision.clone()).is_ok() {
            self.events.publish(Topic::HooksDecision, &decision);
            self.conclude(held.record.clone(), Outcome::Answered);
            return Some(held.record);
        }
        // The hook stopped waiting between our take and the send: it timed
        // out under the decider. Nothing was delivered; the inbox is told.
        self.events.publish(
            Topic::HooksDecision,
            HookDecision {
                request_id: request_id.to_string(),
                decision: "denied".into(),
            },
        );
        self.conclude(held.record, Outcome::TimedOut);
        None
    }

    /// [`decide_held_record`](Self::decide_held_record) for a caller that only
    /// needs to know whether the gate took the answer.
    pub(super) fn decide_held(&self, request_id: &str, approved: bool) -> bool {
        self.decide_held_record(request_id, approved).is_some()
    }

    /// The hold ran out with nobody answering: the gate denied. A no-op if an
    /// answer took the request first.
    fn expire(&self, request_id: &str) {
        let Some(held) = self.take(request_id) else {
            return;
        };
        self.events.publish(
            Topic::HooksDecision,
            HookDecision {
                request_id: request_id.to_string(),
                decision: "denied".into(),
            },
        );
        self.conclude(held.record, Outcome::TimedOut);
    }

    fn park(
        &self,
        request_id: &str,
        terminal_id: &str,
        record: Option<Arc<AskRecord>>,
    ) -> Option<oneshot::Receiver<HookDecision>> {
        let (tx, rx) = oneshot::channel();
        let mut held = self.held.lock().ok()?;
        if held.len() >= MAX_HELD {
            return None;
        }
        held.insert(
            request_id.to_string(),
            Held {
                terminal_id: terminal_id.to_string(),
                tx,
                record,
            },
        );
        Some(rx)
    }

    fn store_snapshot(&self, terminal_id: &str, snapshot: &Value) {
        let Ok(mut s) = self.snapshots.lock() else {
            return;
        };
        if s.len() >= MAX_SNAPSHOTS && !s.contains_key(terminal_id) {
            let live: Vec<String> = self
                .terminals
                .lock()
                .map(|t| t.keys().cloned().collect())
                .unwrap_or_default();
            if let Some(stale) = s.keys().find(|k| !live.contains(k)).cloned() {
                s.remove(&stale);
            }
        }
        s.insert(terminal_id.to_string(), snapshot.clone());
    }
}

/// Removes the parked request when its hook response ends. If nobody answered
/// and nothing else took it — the hook's connection dropped, and this future
/// with it — the inbox is told it was denied, as on the desktop, and the ask's
/// row is flipped.
struct HeldGuard {
    hooks: Arc<TermHooks>,
    request_id: String,
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        // Still parked here means this future ended without an answer, a
        // timeout or a revoke taking the request: the hook hung up.
        if let Some(held) = self.hooks.take(&self.request_id) {
            self.hooks.events.publish(
                Topic::HooksDecision,
                HookDecision {
                    request_id: self.request_id.clone(),
                    decision: "denied".into(),
                },
            );
            self.hooks.conclude(held.record, Outcome::HookDisconnected);
        }
    }
}

// ─── files ───────────────────────────────────────────────────────────────────

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)?;
    // `recursive` leaves an existing directory's mode alone; make sure ours
    // is closed even if it was created by an earlier build.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// 0600, atomically (write a sibling, rename over).
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(data)?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn sweep(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("claude-hooks-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ─── the endpoint ────────────────────────────────────────────────────────────

/// The terminal a request authenticated as.
#[derive(Clone)]
struct AuthedTerminal(String);

fn unauthorized() -> Response {
    // One body for every failure: no oracle for which terminal ids exist.
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "ok": false, "error": "unauthorized" })),
    )
        .into_response()
}

/// `?terminal=<id>` from a request URI.
fn terminal_query(req: &Request) -> Option<String> {
    req.uri().query()?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == "terminal").then(|| v.to_string())
    })
}

/// Runs before the body is read: the per-terminal secret, or 401.
async fn require_terminal_secret(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_string);
    let terminal = terminal_query(&req);
    let (Some(presented), Some(terminal)) = (presented, terminal) else {
        return unauthorized();
    };
    if !state.term_hooks.authenticates(&terminal, &presented) {
        debug!("term-hooks: refused a request for terminal {terminal}");
        return unauthorized();
    }
    req.extensions_mut().insert(AuthedTerminal(terminal));
    next.run(req).await
}

/// The two routes, behind [`require_terminal_secret`]. Merged into the main
/// router OUTSIDE `auth_middleware`; carries no CORS layer, so a web page
/// cannot reach it from a browser.
pub(super) fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(EVENT_PATH, post(hook_event))
        .route(STATUSLINE_PATH, post(statusline_event))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_terminal_secret,
        ))
        .with_state(state)
}

/// What a hook POSTs that the inbox's lifecycle reads. Unknown fields (the
/// tool's whole response, among them) are skipped without being allocated.
#[derive(Debug, Default, Deserialize)]
struct HookIn {
    hook_event_name: Option<String>,
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<Value>,
    tool_use_id: Option<String>,
    prompt: Option<String>,
    /// The session's working directory: the project the ask belongs to.
    cwd: Option<String>,
}

/// Whether `hooks://event` carries this event (see the module doc).
fn published(event: &str) -> bool {
    matches!(
        event,
        "PermissionRequest"
            | "Notification"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "Stop"
            | "SessionEnd"
            | "UserPromptSubmit"
    )
}

/// The desktop's `HookPayload`, minus what the inbox does not read.
fn event_payload(terminal: &str, h: &HookIn, request_id: Option<&str>) -> Value {
    let event = h.hook_event_name.as_deref().unwrap_or("");
    // The text of a prompt is only the inbox's business when it is the ask.
    let keeps_prompt = matches!(event, "PermissionRequest" | "Notification" | "PreToolUse");
    let mut p = json!({
        "ikenga_terminal_id": terminal,
        "hook_event_name": h.hook_event_name,
        "session_id": h.session_id,
        "tool_name": h.tool_name,
        "tool_input": h.tool_input,
        "tool_use_id": h.tool_use_id,
        "cwd": h.cwd,
        "prompt": if keeps_prompt { json!(h.prompt) } else { Value::Null },
    });
    if let Some(id) = request_id {
        p["request_id"] = json!(id);
        p["held"] = json!(true);
    }
    p
}

/// `permissions.hold_terminal_<id>`: the same opt-in the desktop reads.
async fn gate_enabled(state: &AppState, terminal: &str) -> bool {
    let Ok(settings) = super::rpc_local::settings(state).await else {
        return false;
    };
    matches!(
        settings
            .get_legacy(&format!("permissions.hold_terminal_{terminal}"))
            .await,
        Ok(Some(v)) if v == "true" || v == "1"
    )
}

/// The desktop's `PreToolUse` answer: deny blocks THIS tool call;
/// `continue:false` would end the whole session.
fn gate_response(allowed: bool, request_id: &str) -> Response {
    let (decision, reason) = if allowed {
        ("allow", "Approved in the Ikenga permission inbox.")
    } else {
        (
            "deny",
            "Denied in the Ikenga permission inbox (or the request timed out).",
        )
    };
    (
        StatusCode::OK,
        Json(json!({
            "continue": true,
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": decision,
                "permissionDecisionReason": reason,
            },
            "request_id": request_id,
            "gated": true,
        })),
    )
        .into_response()
}

async fn hook_event(
    State(state): State<Arc<AppState>>,
    Extension(AuthedTerminal(terminal)): Extension<AuthedTerminal>,
    Json(h): Json<HookIn>,
) -> Response {
    let hooks = &state.term_hooks;
    let event = h.hook_event_name.as_deref().unwrap_or("");

    if event == "PreToolUse" && gate_enabled(&state, &terminal).await {
        let request_id = mint_request_id();
        let asks = hooks.asks();
        let record = asks.as_ref().map(|_| {
            AskRecord::new(
                &request_id,
                &terminal,
                h.tool_name.as_deref(),
                h.tool_input.as_ref(),
                h.cwd.as_deref(),
                hooks.hold,
            )
        });
        let Some(rx) = hooks.park(&request_id, &terminal, record.clone()) else {
            warn!("term-hooks: {MAX_HELD} asks already held; denying a new one");
            if let Some(asks) = &asks {
                asks.refused_unparked(
                    h.tool_name.as_deref(),
                    h.tool_input.as_ref(),
                    &terminal,
                    h.cwd.as_deref(),
                );
            }
            return gate_response(false, &request_id);
        };
        // Armed before the event goes out: whatever ends this future from
        // here on removes the parked request and tells the inbox.
        let _guard = HeldGuard {
            hooks: hooks.clone(),
            request_id: request_id.clone(),
        };
        hooks.events.publish(
            Topic::HooksEvent,
            event_payload(&terminal, &h, Some(&request_id)),
        );
        // The row is written AFTER the request is parked (so an answer to it
        // always finds a request) and off this path (a busy DB write must
        // never eat the hold's margin under curl's --max-time).
        if let (Some(asks), Some(record)) = (&asks, &record) {
            asks.raise(record);
        }
        // The inbox answers through `permission_decide` on the ask's row, or
        // `term_hooks_decide`. Bounded by the hold, which must stay below
        // curl's --max-time and the hook timeout the settings file declares
        // (`shared::hook_settings`).
        let allowed = match tokio::time::timeout(hooks.hold, rx).await {
            Ok(Ok(HookDecision { decision, .. })) => decision == "approved",
            // The sender was dropped: the terminal's PTY exited (`revoke`
            // took the request and owns the outcome).
            Ok(Err(_)) => false,
            Err(_) => {
                hooks.expire(&request_id);
                false
            }
        };
        return gate_response(allowed, &request_id);
    }

    if published(event) {
        hooks
            .events
            .publish(Topic::HooksEvent, event_payload(&terminal, &h, None));
    }
    Json(json!({ "continue": true })).into_response()
}

async fn statusline_event(
    State(state): State<Arc<AppState>>,
    Extension(AuthedTerminal(terminal)): Extension<AuthedTerminal>,
    Json(body): Json<Value>,
) -> Response {
    let Value::Object(mut snapshot) = body else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "statusline body must be a JSON object" })),
        )
            .into_response();
    };
    // The terminal is the authenticated one, whatever the body claims.
    snapshot.insert("ikenga_terminal_id".into(), json!(terminal));
    let snapshot = Value::Object(snapshot);
    if snapshot.to_string().len() > MAX_SNAPSHOT {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({ "ok": false, "error": "statusline snapshot too large" })),
        )
            .into_response();
    }
    state.term_hooks.store_snapshot(&terminal, &snapshot);
    state.events.publish(Topic::StatuslineSnapshot, &snapshot);
    // The statusline command's stdout IS the status text claude draws, so
    // answer with nothing (the desktop's `{"status":"ok"}` shows up there).
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .unwrap_or_default()
}

// ─── the rpc arms ────────────────────────────────────────────────────────────
//
// `term_hooks_decide` lives in `hook_asks` with `permission_decide`: it
// answers through the ask's row when there is one.

pub(super) fn info_arm(state: &AppState) -> RpcResponse {
    RpcResponse::success(state.term_hooks.info(&state.config))
}

pub(super) fn snapshot_arm(state: &AppState) -> RpcResponse {
    match TermHooks::base_url(&state.config)
        .and_then(|_| state.term_hooks.dir_or_reason().map(|_| ()))
    {
        Ok(()) => RpcResponse::success(state.term_hooks.snapshots()),
        Err(reason) => RpcResponse::error(reason),
    }
}

#[cfg(test)]
#[path = "term_hooks_tests.rs"]
mod tests;
