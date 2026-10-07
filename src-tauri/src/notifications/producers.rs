//! Pure builders for every notification producer (WP-40).
//!
//! Each emit site calls one of these and hands the result to
//! [`super::record`]. Keeping the copy, action and dedupe key here — away
//! from `AppHandle`, sockets and child processes — is what makes every kind's
//! producer unit-testable without running the app (DEC-50).
//!
//! | kind | builder | wired at |
//! |---|---|---|
//! | `permission` | [`permission_from_hook_gate`] | `iyke::hooks::post_hook_event` (held `PreToolUse`) |
//! | `permission` | [`permission_from_hook_request`] | `iyke::hooks::post_hook_event` (`PermissionRequest` hook) |
//! | `permission` | [`permission_from_engine`] | `engines::claude_code::server::spawn_permission_round_trip` |
//! | `run_finished` / `run_failed` | [`run_terminal_with_artifacts`] (in `server::shared::notifications::run`) | `server::shared::chi_exec::cache_update_done` |
//! | `update` | [`update`] | `commands::notifications::notifications_record_update` (FE updater + pkg registry check) |
//! | `violation` | [`violation`] | `pkg::permissions_check::record_violation` |
//! | `system` | [`wsl_network`] | `commands::wsl_health` (a fresh probe with a WSL-caused failure; resolved by the next `ok` probe) |
//! | `invite` | — | **no producer**: D-05's people surface does not exist yet |
//!
//! Action JSON is `{ "kind": "<action kind>", ...params }`; the UI (WP-40b)
//! maps each action kind to its buttons. Action kinds used here:
//!
//! * `permission.decide` — Allow once / Deny inline. **Hooks gate only**
//!   (`via: "hooks"`: a held `PreToolUse`, answered by
//!   `POST /iyke/hooks/decision` with `requestId`). Hide the buttons once the
//!   row has `resolvedAt`.
//! * `open.thread` — a chat-engine (ACP) permission ask. The in-thread
//!   dialog answers it; since WP-75 the row is also answerable inline through
//!   `permission_decide` (G-ACCESS §5.5: `ClaudeCodeEngine::answer_permission`
//!   is the decide core's ACP resolver). Resolves when the round-trip
//!   completes.
//! * `open.terminal` — Claude Code's own prompt inside a terminal. **Open
//!   only**: the answer happens in the terminal, so the row cannot carry
//!   Allow / Deny. It resolves on that terminal's `PostToolUse` for the same
//!   tool call (see [`TerminalPrompts`]), or its `Stop` /
//!   `SessionEnd`.
//! * `open.chi_run`, `open.release_notes`, `open.pkg_updates`,
//!   `open.violations`.
//! * `fix.wsl_network` — `{ kind, distro, state }`: WSL in `distro` (`null`
//!   = the default distro) has no working network; the UI offers the fixes
//!   for `state` (`wsl_health_fix`). Resolves when a probe returns `ok`.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Coalesce, NewNotification, NotificationKind};
use crate::engines::claude_code::notify::short_summary_of_input;

pub const SOURCE_HOOKS: &str = "iyke.hooks";
pub const SOURCE_ENGINE_CLAUDE: &str = "engine.claude-code";
pub const SOURCE_PKG_PERMISSIONS: &str = "pkg.permissions_check";
pub use crate::server::shared::notifications::run::{
    run_terminal, run_terminal_with_artifacts, SOURCE_CHI,
};
pub const SOURCE_UPDATER: &str = "updater";
pub const SOURCE_WSL_HEALTH: &str = "wsl.health";

// The text helpers are shared with the run producer, which moved to the
// ungated `server::shared::notifications::run` (WP-P10).
use crate::server::shared::notifications::run::{
    basename, join_parts, short_id, truncate, BODY_MAX, TITLE_MAX,
};

// ─── permission ─────────────────────────────────────────────────────────────

/// Dedupe key of a held hooks-gate request. Resolved (marked read) by
/// `post_hook_event` once the human decides or the gate times out.
pub fn hook_gate_key(request_id: &str) -> String {
    format!("permission:hook:{request_id}")
}

/// Dedupe key of an ACP-engine permission round-trip. Resolved when the
/// round-trip completes (answered, cancelled or timed out). Scoped by thread:
/// claude's control `request_id` is only unique within one session, and the
/// key is `Coalesce::Once`, so an unscoped id reused by another session (or
/// after a restart) would silently drop a real ask.
pub fn engine_permission_key(thread_id: &str, request_id: &str) -> String {
    format!("permission:acp:{thread_id}:{request_id}")
}

/// Dedupe key of Claude Code's in-terminal `PermissionRequest` prompt: the
/// Ikenga terminal id, else the Claude session id, else `unknown`. The hooks
/// bus resolves it once every prompt folded into it is over (see
/// [`TerminalPrompts`]).
pub fn terminal_permission_key(terminal_id: Option<&str>, session_id: Option<&str>) -> String {
    let scope = terminal_id
        .filter(|t| !t.is_empty())
        .or(session_id.filter(|s| !s.is_empty()))
        .unwrap_or("unknown");
    format!("permission:terminal:{scope}")
}

/// Hook events after which **every** pending `PermissionRequest` in a
/// terminal is over: Claude cannot stop a turn (`Stop`), end the session
/// (`SessionEnd`), or accept the user's next prompt (`UserPromptSubmit` — the
/// input box is replaced by the permission dialog while one is up) while a
/// prompt is still waiting. `UserPromptSubmit` is the backstop for a prompt
/// the user *denied* in the terminal when the turn's `Stop` never reached us.
/// `PostToolUse` / `PostToolUseFailure` are not among them — they only end
/// the prompt for the same tool call ([`TerminalPrompts::tool_finished`]).
///
/// Every event here must be registered in `iyke::hook_settings::HOOK_EVENTS`,
/// or Claude Code never sends it and the branch is dead.
pub fn ends_terminal_permissions(hook_event_name: &str) -> bool {
    matches!(hook_event_name, "Stop" | "SessionEnd" | "UserPromptSubmit")
}

/// Hook events that report one tool call as done: `PostToolUse` (it ran) and
/// `PostToolUseFailure` (it was approved but failed). Either ends the
/// in-terminal prompt that asked about that call.
pub fn finishes_terminal_tool(hook_event_name: &str) -> bool {
    matches!(hook_event_name, "PostToolUse" | "PostToolUseFailure")
}

/// Most prompts remembered per terminal key. Claude shows them one at a
/// time, so this is only a bound against a key that never sees `Stop`.
const MAX_PROMPTS_PER_TERMINAL: usize = 32;

/// One in-terminal `PermissionRequest` still waiting for its tool call.
///
/// Claude Code's `PermissionRequest` hook payload carries **no**
/// `tool_use_id` (only `tool_name`, `tool_input` and the permission
/// suggestions), so in practice the match against `PostToolUse` /
/// `PostToolUseFailure` goes through `fingerprint`. The id comparison is kept
/// for a payload that does carry one; it is never required.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPrompt {
    tool_use_id: Option<String>,
    /// `tool_name` + 16 hex of sha256(`tool_input`), the identity used
    /// whenever either hook payload lacks a `tool_use_id` (always, for
    /// `PermissionRequest` today).
    fingerprint: String,
}

impl PendingPrompt {
    fn new(tool_use_id: Option<&str>, tool_name: Option<&str>, tool_input: Option<&Value>) -> Self {
        let input = tool_input.map(Value::to_string).unwrap_or_default();
        let digest = Sha256::digest(input.as_bytes());
        Self {
            tool_use_id: tool_use_id.filter(|id| !id.is_empty()).map(str::to_string),
            fingerprint: format!("{}:{}", tool_name.unwrap_or(""), hex::encode(&digest[..8])),
        }
    }

    fn matches(&self, other: &PendingPrompt) -> bool {
        match (&self.tool_use_id, &other.tool_use_id) {
            (Some(a), Some(b)) => a == b,
            _ => self.fingerprint == other.fingerprint,
        }
    }
}

/// Which in-terminal `PermissionRequest` prompts are still waiting, per
/// [`terminal_permission_key`]. Several prompts can fold into one row
/// (`Coalesce::WhileUnread`), and a parallel tool call in the same terminal
/// can finish while a prompt is still up — so a `PostToolUse` resolves the
/// row only when it is the tool call a pending prompt asked about (same
/// `tool_use_id`, else same tool name + input) and no other prompt on that
/// key is left. `PostToolUseFailure` counts the same. `Stop` / `SessionEnd` /
/// the terminal's next `UserPromptSubmit` clear the key outright
/// ([`ends_terminal_permissions`]) — the only way a prompt *denied* in the
/// terminal (which never gets a `PostToolUse`) is resolved.
///
/// Pure bookkeeping (the hooks bus holds one behind a mutex); every method
/// returns the row keys that should now be resolved.
#[derive(Debug, Default)]
pub struct TerminalPrompts {
    pending: std::collections::HashMap<String, Vec<PendingPrompt>>,
}

impl TerminalPrompts {
    /// A `PermissionRequest` hook arrived; its row is keyed by `key`.
    pub fn requested(
        &mut self,
        key: &str,
        tool_use_id: Option<&str>,
        tool_name: Option<&str>,
        tool_input: Option<&Value>,
    ) {
        let list = self.pending.entry(key.to_string()).or_default();
        if list.len() >= MAX_PROMPTS_PER_TERMINAL {
            list.remove(0);
        }
        list.push(PendingPrompt::new(tool_use_id, tool_name, tool_input));
    }

    /// A `PostToolUse` / `PostToolUseFailure` hook arrived. `keys` are the row keys it could belong
    /// to (terminal-scoped, then session-scoped). Drops the first matching
    /// pending prompt; returns the key when that left it empty.
    pub fn tool_finished(
        &mut self,
        keys: &[String],
        tool_use_id: Option<&str>,
        tool_name: Option<&str>,
        tool_input: Option<&Value>,
    ) -> Vec<String> {
        let done = PendingPrompt::new(tool_use_id, tool_name, tool_input);
        for key in keys {
            let Some(list) = self.pending.get_mut(key) else {
                continue;
            };
            let Some(i) = list.iter().position(|p| p.matches(&done)) else {
                continue;
            };
            list.remove(i);
            if list.is_empty() {
                self.pending.remove(key);
                return vec![key.clone()];
            }
            return Vec::new();
        }
        Vec::new()
    }

    /// `Stop` / `SessionEnd` / `UserPromptSubmit`: every prompt on these keys is over. Returns all
    /// of `keys` — the row may predate this tracker (app restart), so resolve
    /// it whether or not anything was remembered.
    pub fn ended(&mut self, keys: &[String]) -> Vec<String> {
        for key in keys {
            self.pending.remove(key);
        }
        keys.to_vec()
    }
}

/// Row keys a terminal hook event may resolve: terminal-scoped first, plus
/// the session-scoped key when both ids are known (a row recorded before the
/// terminal id was known is keyed by the session).
pub fn terminal_permission_keys(
    terminal_id: Option<&str>,
    session_id: Option<&str>,
) -> Vec<String> {
    let mut keys = vec![terminal_permission_key(terminal_id, session_id)];
    if terminal_id.is_some_and(|t| !t.is_empty()) && session_id.is_some_and(|s| !s.is_empty()) {
        keys.push(terminal_permission_key(None, session_id));
    }
    keys
}

/// A `PreToolUse` hook held by the permission-inbox gate: the tool call is
/// blocked until someone answers, so the row carries Allow / Deny.
pub fn permission_from_hook_gate(
    tool_name: Option<&str>,
    tool_input: Option<&Value>,
    terminal_id: Option<&str>,
    cwd: Option<&str>,
    request_id: &str,
) -> NewNotification {
    let tool = tool_name.filter(|t| !t.is_empty()).unwrap_or("a tool");
    NewNotification {
        kind: NotificationKind::Permission,
        title: truncate(&format!("Claude wants to use {tool}"), TITLE_MAX),
        body: join_parts(&[
            Some(short_summary_of_input(tool_input)),
            terminal_id.map(|t| format!("terminal {}", short_id(t))),
            cwd.map(|c| basename(c).to_string()),
        ]),
        action: Some(json!({
            "kind": "permission.decide",
            "via": "hooks",
            "requestId": request_id,
            "terminalId": terminal_id,
        })),
        source: SOURCE_HOOKS.into(),
        dedupe_key: Some(hook_gate_key(request_id)),
        coalesce: Coalesce::Once,
    }
}

/// Claude Code's own `PermissionRequest` hook in an Ikenga terminal: Claude is
/// showing its permission prompt in the terminal. The answer happens there,
/// so the action opens the terminal. Repeats in one terminal fold into one
/// unread row.
pub fn permission_from_hook_request(
    tool_name: Option<&str>,
    tool_input: Option<&Value>,
    terminal_id: Option<&str>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> NewNotification {
    let tool = tool_name.filter(|t| !t.is_empty()).unwrap_or("a tool");
    NewNotification {
        kind: NotificationKind::Permission,
        title: truncate(&format!("Claude is asking to use {tool}"), TITLE_MAX),
        body: join_parts(&[
            Some(short_summary_of_input(tool_input)),
            terminal_id.map(|t| format!("terminal {}", short_id(t))),
            cwd.map(|c| basename(c).to_string()),
        ]),
        action: Some(json!({
            "kind": "open.terminal",
            "terminalId": terminal_id,
            "sessionId": session_id,
        })),
        source: SOURCE_HOOKS.into(),
        dedupe_key: Some(terminal_permission_key(terminal_id, session_id)),
        coalesce: Coalesce::WhileUnread,
    }
}

/// A chat-engine (ACP) permission round-trip. The in-thread dialog answers
/// it; the row opens the thread and resolves when the round-trip completes.
///
/// Deliberately **not** `permission.decide`: nothing in production can answer
/// an ACP request from outside the thread yet
/// (`ClaudeCodeEngine::resolve_permission` has no caller), and WP-40b reads
/// every `permission.decide` as a hooks-gate decision. Inline Allow / Deny
/// for ACP rows is a follow-up that needs a real resolve path first.
pub fn permission_from_engine(
    thread_id: &str,
    request_id: &str,
    tool_name: &str,
    tool_input: Option<&Value>,
) -> NewNotification {
    let tool = if tool_name.is_empty() { "a tool" } else { tool_name };
    NewNotification {
        kind: NotificationKind::Permission,
        title: truncate(&format!("Claude wants to use {tool}"), TITLE_MAX),
        body: join_parts(&[
            Some(short_summary_of_input(tool_input)),
            Some(format!("session {}", short_id(thread_id))),
        ]),
        action: Some(json!({
            "kind": "open.thread",
            "threadId": thread_id,
            "requestId": request_id,
        })),
        source: SOURCE_ENGINE_CLAUDE.into(),
        dedupe_key: Some(engine_permission_key(thread_id, request_id)),
        coalesce: Coalesce::Once,
    }
}

// ─── permission attribution (G-ACCESS §5.3, §5.7; WP-75) ────────────────────

/// What a permission producer knows about the ask, for its attribution
/// columns (`shell_notifications.{requested_by, project_id, sensitive}`).
#[derive(Debug, Clone, PartialEq)]
pub struct AskFacts {
    pub tool_name: String,
    pub tool_input: Value,
    /// The asking session's working directory: the project it belongs to,
    /// and the root §5.3 rule 2 classifies against.
    pub cwd: Option<String>,
    /// Claude Code's own terminal prompt (§5.3 rule 1: shell exec).
    pub terminal: bool,
}

pub fn ask_facts(
    tool_name: Option<&str>,
    tool_input: Option<&Value>,
    cwd: Option<&str>,
    terminal: bool,
) -> AskFacts {
    AskFacts {
        tool_name: tool_name.unwrap_or_default().to_string(),
        tool_input: tool_input.cloned().unwrap_or(Value::Null),
        cwd: cwd.filter(|c| !c.is_empty()).map(str::to_string),
        terminal,
    }
}

/// The attribution a desktop ask is recorded with. `project` is the
/// `(id, root_path)` the cwd resolved to; the root classifies, else the cwd
/// itself. The desktop's asks are always the Owner's own work, so
/// `requested_by` is `None` (§5.7).
pub fn attribution(
    facts: &AskFacts,
    project: Option<&(String, String)>,
) -> crate::server::shared::notifications::routing::Attribution {
    use crate::server::shared::notifications::routing::{classify, classify_terminal, Attribution};
    let root = project
        .map(|(_, root)| root.as_str())
        .or(facts.cwd.as_deref());
    let sensitivity = if facts.terminal {
        classify_terminal(&facts.tool_name, &facts.tool_input, root)
    } else {
        classify(&facts.tool_name, &facts.tool_input, root)
    };
    Attribution {
        requested_by: None,
        project_id: project.map(|(id, _)| id.clone()),
        sensitivity,
    }
}

// ─── violation ──────────────────────────────────────────────────────────────

fn violation_verb(scope_kind: &str) -> &'static str {
    match scope_kind {
        "shell.execute" => "spawning",
        "http.fetch" => "fetching",
        "capabilities.secrets" => "reading secret",
        "capabilities.invoke" => "invoking",
        _ => "using",
    }
}

/// Scheme + host (+ port) + path of a URL — no userinfo, query or fragment.
/// A bare `/` path is dropped. `None` when `raw` is not an absolute URL.
fn url_without_query(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let host = url.host_str()?;
    let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = match url.path() {
        "/" => "",
        p => p,
    };
    Some(format!("{}://{host}{port}{path}", url.scheme()))
}

/// What a violation is "about", with request payload stripped. For
/// `http.fetch` the attempted string is `METHOD <url>` or
/// `METHOD <url> -> <redirect location>`: the refused target is the last URL,
/// kept as `METHOD scheme://host/path` (query strings vary per call and can
/// carry tokens). Anything else is the attempted string as-is.
pub fn violation_target(scope_kind: &str, attempted: &str) -> String {
    if scope_kind == "http.fetch" {
        let tokens: Vec<&str> = attempted.split_whitespace().collect();
        if let Some(url) = tokens.iter().rev().find_map(|t| url_without_query(t)) {
            let method = tokens
                .first()
                .filter(|m| !m.is_empty() && m.chars().all(|c| c.is_ascii_uppercase()));
            return match method {
                Some(m) => format!("{m} {url}"),
                None => url,
            };
        }
    }
    attempted.trim().to_string()
}

/// `violation:<pkg>:<scope>:<16 hex of sha256(target)>` — bounded and carrying
/// no payload (the key is returned to the webview and on the iyke bridge).
pub fn violation_key(pkg_id: &str, scope_kind: &str, attempted: &str) -> String {
    let digest = Sha256::digest(violation_target(scope_kind, attempted).as_bytes());
    let hash = hex::encode(&digest[..8]);
    format!("violation:{pkg_id}:{scope_kind}:{hash}")
}

/// A denied pkg action (`pkg_permission_violations` row). Repeats of the same
/// (pkg, scope, normalized target) fold into one unread row whose `count` is
/// the number of denials ("3 denials" in D-07) — for `http.fetch`, calls that
/// differ only in query string fold together.
pub fn violation(pkg_id: &str, scope_kind: &str, attempted: &str) -> NewNotification {
    let short = pkg_id.rsplit('.').next().unwrap_or(pkg_id);
    NewNotification {
        kind: NotificationKind::Violation,
        title: truncate(
            &format!(
                "{short} was blocked from {} {}",
                violation_verb(scope_kind),
                truncate(&violation_target(scope_kind, attempted), 80)
            ),
            TITLE_MAX,
        ),
        body: join_parts(&[Some(pkg_id.to_string()), Some(scope_kind.to_string())]),
        action: Some(json!({ "kind": "open.violations", "pkgId": pkg_id })),
        source: SOURCE_PKG_PERMISSIONS.into(),
        dedupe_key: Some(violation_key(pkg_id, scope_kind, attempted)),
        coalesce: Coalesce::WhileUnread,
    }
}

// ─── run finished / failed ──────────────────────────────────────────────────
//
// `run_terminal` / `run_terminal_with_artifacts` live in the ungated
// `server::shared::notifications::run` (WP-P10: the daemon's Chi runs produce
// them too) and are re-exported at the top of this file.

// ─── wsl network ────────────────────────────────────────────────────────────

/// Prefix shared by every [`wsl_network_key`].
pub const WSL_NETWORK_KEY_PREFIX: &str = "wsl:network:";

/// Dedupe key of a distro's WSL network problem: `wsl:network:<distro>`
/// (lower-cased; `default` for the default distro). One row per episode
/// ([`Coalesce::WhileUnresolved`]); resolved by the next `ok` probe.
pub fn wsl_network_key(distro: Option<&str>) -> String {
    format!(
        "{WSL_NETWORK_KEY_PREFIX}{}",
        distro.map_or_else(|| "default".to_string(), str::to_ascii_lowercase)
    )
}

/// A WSL network problem worth telling the user about (D-2, D-8), or `None`
/// when `health` isn't WSL's fault (`ok`, `host_offline`, `not_installed`).
/// Uses the `system` kind (D-19): an environment problem, not a pkg's
/// denial, and mutable on its own.
pub fn wsl_network(
    health: &crate::server::shared::wsl_health::WslHealth,
) -> Option<NewNotification> {
    use crate::server::shared::wsl_health::WslHealthState;
    let title = match health.state {
        WslHealthState::NoRoute => "WSL has no network",
        WslHealthState::DnsOnly => "WSL can't resolve names",
        WslHealthState::WslDown => "WSL isn't starting",
        _ => return None,
    };
    let distro = health.distro.as_deref();
    Some(NewNotification {
        kind: NotificationKind::System,
        title: truncate(
            &format!("{title} · {}", distro.unwrap_or("default distro")),
            TITLE_MAX,
        ),
        body: Some(truncate(&health.detail, BODY_MAX)),
        action: Some(json!({
            "kind": "fix.wsl_network",
            "distro": distro,
            "state": health.state.as_str(),
        })),
        source: SOURCE_WSL_HEALTH.into(),
        dedupe_key: Some(wsl_network_key(distro)),
        coalesce: Coalesce::WhileUnresolved,
    })
}

/// The open `fix.wsl_network` rows a freshly measured `health` ends, given
/// the open rows under [`WSL_NETWORK_KEY_PREFIX`] as `(key, action)`.
///
/// * `ok` ends its own distro's row, and every row whose state was
///   `no_route` / `wsl_down`: all WSL 2 distros share one VM and one network,
///   so those can't still hold when any distro is online. (This is also what
///   ties a row raised for `default` to a probe of the same distro by name.)
///   Another distro's `dns_only` stays: `/etc/resolv.conf` is per distro.
/// * `not_installed` ends its own row (the distro is gone), and every row
///   when it is about WSL as a whole (no distro named).
/// * Anything else ends nothing (`host_offline` says nothing about WSL).
pub fn wsl_network_keys_to_resolve(
    health: &crate::server::shared::wsl_health::WslHealth,
    open: &[(String, Option<Value>)],
) -> Vec<String> {
    use crate::server::shared::wsl_health::WslHealthState;
    let own = wsl_network_key(health.distro.as_deref());
    let mut out: Vec<String> = Vec::new();
    let mut push = |k: &str| {
        if !out.iter().any(|x| x == k) {
            out.push(k.to_string());
        }
    };
    match health.state {
        WslHealthState::Ok => {
            push(&own);
            for (key, action) in open {
                let state = action.as_ref().and_then(|a| a.get("state")).and_then(Value::as_str);
                if matches!(state, Some("no_route" | "wsl_down")) {
                    push(key);
                }
            }
        }
        WslHealthState::NotInstalled => {
            push(&own);
            if health.distro.is_none() {
                for (key, _) in open {
                    push(key);
                }
            }
        }
        _ => {}
    }
    // Only rows that are actually open (and in this family).
    out.retain(|k| open.iter().any(|(o, _)| o == k));
    out
}

// ─── update ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateSource {
    Shell,
    Pkg,
}

impl UpdateSource {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "shell" => Ok(UpdateSource::Shell),
            "pkg" => Ok(UpdateSource::Pkg),
            other => Err(format!("unknown update source: {other}")),
        }
    }
}

/// A shell or pkg release is available. One row per (target, version) while
/// the row exists: the updater re-checks every 6 h and must not re-announce a
/// version the user has already seen. Once the read row is pruned (30 days,
/// `READ_RETENTION_MS`) a still-uninstalled version is announced once more —
/// accepted, no tombstone is kept. The row is resolved once that version is
/// installed (`notifications::resolve_installed_updates`).
pub fn update(
    source: UpdateSource,
    version: &str,
    pkg_id: Option<&str>,
    pkg_name: Option<&str>,
) -> Result<NewNotification, String> {
    let version = version.trim();
    if version.is_empty() || version.chars().count() > 64 {
        return Err("update version must be 1..=64 characters".into());
    }
    match source {
        UpdateSource::Shell => Ok(NewNotification {
            kind: NotificationKind::Update,
            title: format!("Ikenga {version} is available"),
            body: Some("Shell update".into()),
            action: Some(json!({
                "kind": "open.release_notes",
                "source": "shell",
                "version": version,
            })),
            source: SOURCE_UPDATER.into(),
            dedupe_key: Some(format!("update:shell:{version}")),
            coalesce: Coalesce::Once,
        }),
        UpdateSource::Pkg => {
            let pkg_id = pkg_id
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| "a pkg update needs a pkgId".to_string())?;
            let name = pkg_name
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .unwrap_or(pkg_id);
            Ok(NewNotification {
                kind: NotificationKind::Update,
                title: truncate(&format!("{name} {version} is available"), TITLE_MAX),
                body: Some(truncate(&format!("Package update · {pkg_id}"), BODY_MAX)),
                action: Some(json!({
                    "kind": "open.pkg_updates",
                    "pkgId": pkg_id,
                    "version": version,
                })),
                source: SOURCE_UPDATER.into(),
                dedupe_key: Some(format!("update:pkg:{pkg_id}@{version}")),
                coalesce: Coalesce::Once,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsl_health(
        state: crate::server::shared::wsl_health::WslHealthState,
        distro: Option<&str>,
    ) -> crate::server::shared::wsl_health::WslHealth {
        crate::server::shared::wsl_health::WslHealth {
            state,
            distro: distro.map(str::to_string),
            detail: "WSL has no network: only the loopback interface is up.".into(),
            mirrored_failure: None,
            networking_mode: Some("mirrored".into()),
            checked_at: 1,
        }
    }

    #[test]
    fn wsl_network_is_one_fix_row_per_distro() {
        use crate::server::shared::wsl_health::WslHealthState as S;
        let n = wsl_network(&wsl_health(S::NoRoute, Some("Ubuntu"))).unwrap();
        assert_eq!(n.kind, NotificationKind::System);
        assert_eq!(n.title, "WSL has no network · Ubuntu");
        assert_eq!(n.dedupe_key.as_deref(), Some("wsl:network:ubuntu"));
        assert_eq!(n.coalesce, Coalesce::WhileUnresolved);
        assert_eq!(n.source, SOURCE_WSL_HEALTH);
        assert_eq!(
            n.action,
            Some(json!({ "kind": "fix.wsl_network", "distro": "Ubuntu", "state": "no_route" }))
        );
        let d = wsl_network(&wsl_health(S::DnsOnly, None)).unwrap();
        assert_eq!(d.dedupe_key.as_deref(), Some("wsl:network:default"));
        assert_eq!(
            d.action,
            Some(json!({ "kind": "fix.wsl_network", "distro": null, "state": "dns_only" }))
        );
        assert!(wsl_network(&wsl_health(S::WslDown, None)).is_some());
        for quiet in [S::Ok, S::HostOffline, S::NotInstalled] {
            assert_eq!(wsl_network(&wsl_health(quiet, Some("Ubuntu"))), None);
        }
    }

    #[test]
    fn wsl_recovery_ends_vm_wide_rows_but_not_another_distros_dns() {
        use crate::server::shared::wsl_health::WslHealthState as S;
        let row = |key: &str, state: &str| {
            (key.to_string(), Some(json!({ "kind": "fix.wsl_network", "state": state })))
        };
        let open = vec![
            row("wsl:network:default", "no_route"),
            row("wsl:network:ubuntu", "dns_only"),
            row("wsl:network:debian", "dns_only"),
            row("wsl:network:arch", "wsl_down"),
        ];
        // `Ubuntu` is online: its own row, plus the VM-wide ones (the
        // `default` row raised from a tab without -d included).
        let mut got = wsl_network_keys_to_resolve(&wsl_health(S::Ok, Some("Ubuntu")), &open);
        got.sort();
        assert_eq!(got, ["wsl:network:arch", "wsl:network:default", "wsl:network:ubuntu"]);
        // A removed distro ends its own row; WSL gone entirely ends all.
        assert_eq!(
            wsl_network_keys_to_resolve(&wsl_health(S::NotInstalled, Some("Debian")), &open),
            ["wsl:network:debian"]
        );
        assert_eq!(wsl_network_keys_to_resolve(&wsl_health(S::NotInstalled, None), &open).len(), 4);
        // Still failing, or the PC offline: nothing ends.
        for s in [S::HostOffline, S::NoRoute, S::DnsOnly, S::WslDown] {
            assert!(wsl_network_keys_to_resolve(&wsl_health(s, Some("Ubuntu")), &open).is_empty());
        }
        // No open row for the probed distro: nothing to resolve.
        assert!(wsl_network_keys_to_resolve(&wsl_health(S::Ok, Some("Alpine")), &[]).is_empty());
    }

    /// G-ACCESS §5.7 (WP-75): desktop asks are the Owner's own work, sorted
    /// into the project their cwd sits in and classified against its root.
    #[test]
    fn attribution_classifies_against_the_project_root() {
        use crate::server::shared::notifications::routing::Sensitivity;
        let project = ("royalti-co".to_string(), "/work/royalti-co".to_string());
        let inside = ask_facts(
            Some("Edit"),
            Some(&json!({"file_path": "/work/royalti-co/src/a.rs"})),
            Some("/work/royalti-co/src"),
            false,
        );
        let a = attribution(&inside, Some(&project));
        assert_eq!(a.requested_by, None);
        assert_eq!(a.project_id.as_deref(), Some("royalti-co"));
        assert_eq!(a.sensitivity, Sensitivity::NONE);
        let outside = ask_facts(
            Some("Edit"),
            Some(&json!({"file_path": "/etc/hosts"})),
            Some("/x"),
            false,
        );
        assert_eq!(
            attribution(&outside, None).sensitivity,
            Sensitivity::SENSITIVE
        );
        let secret = ask_facts(
            Some("Read"),
            Some(&json!({"file_path": ".env"})),
            Some("/x"),
            false,
        );
        assert_eq!(attribution(&secret, None).sensitivity, Sensitivity::SECRET);
        let term = ask_facts(
            Some("Read"),
            Some(&json!({"file_path": "a"})),
            Some("/x"),
            true,
        );
        assert_eq!(attribution(&term, None).sensitivity, Sensitivity::SENSITIVE);
        assert_eq!(ask_facts(None, None, Some(""), false).cwd, None);
    }

    #[test]
    fn permission_hook_gate_carries_allow_deny_and_a_per_request_key() {
        let n = permission_from_hook_gate(
            Some("Read"),
            Some(&json!({ "file_path": "royalti-server-v2.6/.env" })),
            Some("term-1234567890"),
            Some("/home/me/royalti-co"),
            "perm-1-2",
        );
        assert_eq!(n.kind, NotificationKind::Permission);
        assert_eq!(n.title, "Claude wants to use Read");
        assert_eq!(
            n.body.as_deref(),
            Some("royalti-server-v2.6/.env · terminal term-123 · royalti-co")
        );
        assert_eq!(n.action.as_ref().unwrap()["kind"], "permission.decide");
        assert_eq!(n.action.as_ref().unwrap()["requestId"], "perm-1-2");
        assert_eq!(n.dedupe_key.as_deref(), Some("permission:hook:perm-1-2"));
        assert_eq!(n.coalesce, Coalesce::Once);
        assert_eq!(n.source, SOURCE_HOOKS);
    }

    #[test]
    fn permission_hook_request_folds_per_terminal_and_opens_it() {
        let n = permission_from_hook_request(
            Some("Bash"),
            Some(&json!({ "command": "rm -rf target" })),
            Some("t-9"),
            Some("sess"),
            Some("C:\\Users\\me\\proj\\"),
        );
        assert_eq!(n.kind, NotificationKind::Permission);
        assert_eq!(n.body.as_deref(), Some("rm -rf target · terminal t-9 · proj"));
        assert_eq!(n.action.as_ref().unwrap()["kind"], "open.terminal");
        assert_eq!(n.dedupe_key.as_deref(), Some("permission:terminal:t-9"));
        assert_eq!(
            n.dedupe_key.as_deref(),
            Some(terminal_permission_key(Some("t-9"), Some("sess")).as_str())
        );
        assert_eq!(n.coalesce, Coalesce::WhileUnread);
        // No terminal id: fall back to the Claude session id.
        let n = permission_from_hook_request(None, None, None, Some("sess"), None);
        assert_eq!(n.title, "Claude is asking to use a tool");
        assert_eq!(n.dedupe_key.as_deref(), Some("permission:terminal:sess"));
    }

    #[test]
    fn permission_from_engine_opens_the_thread_scoped_by_thread() {
        let n = permission_from_engine(
            "0123456789abcdef",
            "req-7",
            "Bash",
            Some(&json!({ "command": "ls -la" })),
        );
        assert_eq!(n.kind, NotificationKind::Permission);
        assert_eq!(n.body.as_deref(), Some("ls -la · session 01234567"));
        let action = n.action.as_ref().unwrap();
        // Open-only: no resolve path can answer an ACP ask from outside the
        // thread yet, and WP-40b reads every `permission.decide` as hooks.
        assert_eq!(action["kind"], "open.thread");
        assert!(action.get("via").is_none());
        assert_eq!(action["threadId"], "0123456789abcdef");
        assert_eq!(action["requestId"], "req-7");
        assert_eq!(
            n.dedupe_key.as_deref(),
            Some("permission:acp:0123456789abcdef:req-7")
        );
        // Same request id in another session is a different ask.
        assert_ne!(
            engine_permission_key("thread-a", "req-7"),
            engine_permission_key("thread-b", "req-7")
        );
        assert_eq!(n.source, SOURCE_ENGINE_CLAUDE);
    }

    #[test]
    fn terminal_permission_key_prefers_terminal_then_session() {
        assert_eq!(terminal_permission_key(Some("t"), Some("s")), "permission:terminal:t");
        assert_eq!(terminal_permission_key(None, Some("s")), "permission:terminal:s");
        assert_eq!(terminal_permission_key(Some(""), Some("s")), "permission:terminal:s");
        assert_eq!(terminal_permission_key(None, None), "permission:terminal:unknown");
        for ev in ["Stop", "SessionEnd"] {
            assert!(ends_terminal_permissions(ev));
        }
        for ev in ["PostToolUse", "PreToolUse", "PermissionRequest", "Notification"] {
            assert!(!ends_terminal_permissions(ev));
        }
        assert_eq!(
            terminal_permission_keys(Some("t"), Some("s")),
            vec!["permission:terminal:t".to_string(), "permission:terminal:s".to_string()]
        );
        assert_eq!(
            terminal_permission_keys(None, Some("s")),
            vec!["permission:terminal:s".to_string()]
        );
    }

    #[test]
    fn parallel_tool_finishing_does_not_resolve_a_waiting_prompt() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), Some("s1"));
        let key = &keys[0];
        prompts.requested(
            key,
            Some("toolu_bash"),
            Some("Bash"),
            Some(&json!({ "command": "rm -rf build" })),
        );
        // A parallel Read in the same terminal finishes first: not the ask.
        let resolved = prompts.tool_finished(
            &keys,
            Some("toolu_read"),
            Some("Read"),
            Some(&json!({ "file_path": "a.rs" })),
        );
        assert!(resolved.is_empty());
        // The asked-about call finishes: now the row is over.
        let resolved = prompts.tool_finished(
            &keys,
            Some("toolu_bash"),
            Some("Bash"),
            Some(&json!({ "command": "rm -rf build" })),
        );
        assert_eq!(resolved, vec![key.clone()]);
        // Nothing left: a later PostToolUse resolves nothing.
        assert!(prompts
            .tool_finished(&keys, Some("toolu_bash"), Some("Bash"), None)
            .is_empty());
    }

    #[test]
    fn prompt_matches_on_name_and_input_when_a_payload_has_no_tool_use_id() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), None);
        let bash = json!({ "command": "cargo publish" });
        prompts.requested(&keys[0], None, Some("Bash"), Some(&bash));
        // Same tool, different input: a parallel call, not the ask.
        let other = json!({ "command": "ls" });
        assert!(prompts
            .tool_finished(&keys, Some("toolu_x"), Some("Bash"), Some(&other))
            .is_empty());
        assert_eq!(
            prompts.tool_finished(&keys, Some("toolu_y"), Some("Bash"), Some(&bash)),
            vec![keys[0].clone()]
        );
    }

    #[test]
    fn folded_prompts_resolve_only_when_the_last_one_finishes() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), None);
        prompts.requested(&keys[0], Some("a"), Some("Bash"), None);
        prompts.requested(&keys[0], Some("b"), Some("Edit"), None);
        assert!(prompts.tool_finished(&keys, Some("a"), Some("Bash"), None).is_empty());
        assert_eq!(
            prompts.tool_finished(&keys, Some("b"), Some("Edit"), None),
            vec![keys[0].clone()]
        );
    }

    #[test]
    fn stop_ends_every_prompt_even_ones_not_remembered() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), Some("s1"));
        prompts.requested(&keys[0], Some("a"), Some("Bash"), None);
        assert_eq!(prompts.ended(&keys), keys);
        assert!(prompts.tool_finished(&keys, Some("a"), Some("Bash"), None).is_empty());
        // After a restart nothing is remembered; Stop still resolves the row.
        assert_eq!(TerminalPrompts::default().ended(&keys), keys);
    }

    #[test]
    fn end_events_cover_a_prompt_denied_in_the_terminal() {
        // A denied prompt never gets `PostToolUse`; each of these must end it.
        for event in ["Stop", "SessionEnd", "UserPromptSubmit"] {
            assert!(ends_terminal_permissions(event), "{event} must end prompts");
        }
        for event in ["PostToolUse", "PostToolUseFailure", "PreToolUse", "Notification"] {
            assert!(!ends_terminal_permissions(event), "{event} must not end every prompt");
        }
        assert!(finishes_terminal_tool("PostToolUse"));
        assert!(finishes_terminal_tool("PostToolUseFailure"));
        assert!(!finishes_terminal_tool("Stop"));
    }

    #[test]
    fn denied_prompt_resolves_on_the_next_user_prompt() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), Some("s1"));
        // PermissionRequest carries no tool_use_id.
        prompts.requested(
            &keys[0],
            None,
            Some("Bash"),
            Some(&json!({ "command": "rm -rf build" })),
        );
        // Denied: no PostToolUse ever arrives. The next UserPromptSubmit ends it.
        assert!(ends_terminal_permissions("UserPromptSubmit"));
        assert_eq!(prompts.ended(&keys), keys);
        assert!(prompts
            .tool_finished(
                &keys,
                Some("toolu_late"),
                Some("Bash"),
                Some(&json!({ "command": "rm -rf build" })),
            )
            .is_empty());
    }

    #[test]
    fn approved_but_failed_tool_resolves_via_post_tool_use_failure_fingerprint() {
        let mut prompts = TerminalPrompts::default();
        let keys = terminal_permission_keys(Some("t1"), None);
        let input = json!({ "command": "cargo publish" });
        prompts.requested(&keys[0], None, Some("Bash"), Some(&input));
        // PostToolUseFailure carries a tool_use_id, the request did not:
        // the tool_name + input fingerprint is what matches.
        assert!(finishes_terminal_tool("PostToolUseFailure"));
        assert_eq!(
            prompts.tool_finished(&keys, Some("toolu_fail"), Some("Bash"), Some(&input)),
            vec![keys[0].clone()]
        );
    }

    #[test]
    fn violation_names_the_pkg_and_folds_per_target() {
        let n = violation("com.ikenga.pkg-browser", "shell.execute", "ffmpeg");
        assert_eq!(n.kind, NotificationKind::Violation);
        assert_eq!(n.title, "pkg-browser was blocked from spawning ffmpeg");
        assert_eq!(n.body.as_deref(), Some("com.ikenga.pkg-browser · shell.execute"));
        assert_eq!(n.action.as_ref().unwrap()["kind"], "open.violations");
        assert_eq!(
            n.dedupe_key,
            Some(violation_key("com.ikenga.pkg-browser", "shell.execute", "ffmpeg"))
        );
        assert_eq!(n.coalesce, Coalesce::WhileUnread);
        let fetch = violation("p", "http.fetch", "https://example.com");
        assert_eq!(fetch.title, "p was blocked from fetching https://example.com");
    }

    #[test]
    fn violation_key_is_bounded_and_carries_no_payload() {
        let secret = "GET https://api.example.com/v1/items?token=s3cr3t&page=2#frag";
        let key = violation_key("com.x.p", "http.fetch", secret);
        assert!(key.starts_with("violation:com.x.p:http.fetch:"));
        assert_eq!(key.rsplit(':').next().unwrap().len(), 16);
        assert!(!key.contains("s3cr3t") && !key.contains("api.example.com"));
        // Differing query strings / fragments fold into one key.
        assert_eq!(
            key,
            violation_key("com.x.p", "http.fetch", "GET https://api.example.com/v1/items?page=3")
        );
        // A different path, method or host does not.
        assert_ne!(
            key,
            violation_key("com.x.p", "http.fetch", "GET https://api.example.com/v1/other")
        );
        assert_ne!(
            key,
            violation_key("com.x.p", "http.fetch", "POST https://api.example.com/v1/items")
        );
        // A huge attempted string still yields a bounded key.
        let long = format!("ffmpeg {}", "x".repeat(10_000));
        assert!(violation_key("com.x.p", "shell.execute", &long).len() < 80);
    }

    #[test]
    fn violation_target_normalizes_http_fetch_and_keeps_other_scopes() {
        assert_eq!(
            violation_target("http.fetch", "GET https://a.test:8443/x/y?q=1#f"),
            "GET https://a.test:8443/x/y"
        );
        // Redirect refusal: the refused target is the redirect location.
        assert_eq!(
            violation_target("http.fetch", "GET https://a.test/start -> https://evil.test/p?t=1"),
            "GET https://evil.test/p"
        );
        assert_eq!(
            violation_target("http.fetch", "https://user:pw@a.test/?q"),
            "https://a.test"
        );
        // Not a URL: left as-is.
        assert_eq!(violation_target("http.fetch", "GET not a url"), "GET not a url");
        assert_eq!(violation_target("shell.execute", " ffmpeg "), "ffmpeg");
        // The title shows the normalized target too — no query string.
        let n = violation("p", "http.fetch", "GET https://a.test/x?token=abc");
        assert_eq!(n.title, "p was blocked from fetching GET https://a.test/x");
    }

    #[test]
    fn run_terminal_with_artifacts_carries_count_and_first_path() {
        let artifacts = json!([
            { "path": "/tmp/out/snap-1.png", "mime": "image/png", "producedBy": "Write" },
            { "path": "/tmp/out/snap-2.png", "mime": "image/png", "producedBy": "Write" },
        ]);
        let done = run_terminal_with_artifacts(
            "r1",
            "done",
            "claude-code",
            Some("pulse-refresh"),
            Some("/home/me/royalti-co"),
            None,
            Some(&artifacts),
        )
        .unwrap();
        let action = done.action.as_ref().unwrap();
        assert_eq!(action["kind"], "open.chi_run");
        assert_eq!(action["artifactCount"], 2);
        assert_eq!(action["firstArtifactPath"], "/tmp/out/snap-1.png");
        assert_eq!(
            done.body.as_deref(),
            Some("2 artifacts · claude-code · royalti-co")
        );

        let failed = run_terminal_with_artifacts(
            "r1",
            "failed",
            "codex",
            None,
            None,
            Some("exit 1"),
            Some(&json!([{ "path": "/tmp/partial.md" }])),
        )
        .unwrap();
        assert_eq!(failed.body.as_deref(), Some("exit 1 · 1 artifact · codex"));
        assert_eq!(failed.action.as_ref().unwrap()["artifactCount"], 1);

        // No artifacts: count 0, null path, body unchanged.
        let plain = run_terminal("r2", "done", "claude-code", None, None, None).unwrap();
        assert_eq!(plain.action.as_ref().unwrap()["artifactCount"], 0);
        assert!(plain.action.as_ref().unwrap()["firstArtifactPath"].is_null());
        assert_eq!(plain.body.as_deref(), Some("claude-code"));
    }

    #[test]
    fn run_terminal_maps_done_to_finished_and_failed_to_failed() {
        let done = run_terminal(
            "run-abcdefgh-1",
            "done",
            "claude-code",
            Some("pulse-refresh\nsecond line"),
            Some("/home/me/royalti-co"),
            None,
        )
        .unwrap();
        assert_eq!(done.kind, NotificationKind::RunFinished);
        assert_eq!(done.title, "pulse-refresh finished");
        assert_eq!(done.body.as_deref(), Some("claude-code · royalti-co"));
        assert_eq!(done.action.as_ref().unwrap()["kind"], "open.chi_run");
        assert_eq!(done.dedupe_key.as_deref(), Some("run:run-abcdefgh-1:done"));

        let failed = run_terminal(
            "run-abcdefgh-1",
            "failed",
            "codex",
            None,
            None,
            Some("exit 1\nstack…"),
        )
        .unwrap();
        assert_eq!(failed.kind, NotificationKind::RunFailed);
        assert_eq!(failed.title, "Chi run run-abcd failed");
        assert_eq!(failed.body.as_deref(), Some("exit 1 · codex"));
        assert_ne!(failed.dedupe_key, done.dedupe_key);
    }

    #[test]
    fn run_terminal_ignores_cancelled_and_non_terminal_statuses() {
        for status in ["cancelled", "running", "queued", "awaiting_auth"] {
            assert!(run_terminal("r", status, "claude-code", None, None, None).is_none());
        }
    }

    #[test]
    fn update_is_once_per_target_and_version() {
        let shell = update(UpdateSource::Shell, " 0.9.1 ", None, None).unwrap();
        assert_eq!(shell.kind, NotificationKind::Update);
        assert_eq!(shell.title, "Ikenga 0.9.1 is available");
        assert_eq!(shell.action.as_ref().unwrap()["kind"], "open.release_notes");
        assert_eq!(shell.dedupe_key.as_deref(), Some("update:shell:0.9.1"));
        assert_eq!(shell.coalesce, Coalesce::Once);

        let pkg = update(
            UpdateSource::Pkg,
            "1.2.0",
            Some("com.ikenga.tasks"),
            Some("Tasks"),
        )
        .unwrap();
        assert_eq!(pkg.title, "Tasks 1.2.0 is available");
        assert_eq!(pkg.action.as_ref().unwrap()["pkgId"], "com.ikenga.tasks");
        assert_eq!(pkg.dedupe_key.as_deref(), Some("update:pkg:com.ikenga.tasks@1.2.0"));

        assert!(update(UpdateSource::Pkg, "1.2.0", None, None).is_err());
        assert!(update(UpdateSource::Shell, "   ", None, None).is_err());
        assert!(UpdateSource::parse("shell").is_ok());
        assert!(UpdateSource::parse("os").is_err());
    }

    #[test]
    fn basename_handles_both_separators_and_trailing_slashes() {
        assert_eq!(basename("/a/b/c"), "c");
        assert_eq!(basename("/a/b/c/"), "c");
        assert_eq!(basename("C:\\x\\y"), "y");
        assert_eq!(basename("solo"), "solo");
    }
}
