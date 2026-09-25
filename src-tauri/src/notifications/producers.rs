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
//! | `run_finished` / `run_failed` | [`run_terminal_with_artifacts`] | `commands::chi::cache_update_done` |
//! | `update` | [`update`] | `commands::notifications::notifications_record_update` (FE updater + pkg registry check) |
//! | `violation` | [`violation`] | `pkg::permissions_check::record_violation` |
//! | `invite` | — | **no producer**: D-05's people surface does not exist yet |
//!
//! Action JSON is `{ "kind": "<action kind>", ...params }`; the UI (WP-40b)
//! maps each action kind to its buttons. Action kinds used here:
//!
//! * `permission.decide` — Allow once / Deny inline. `via` says how to answer:
//!   `"hooks"` (held `PreToolUse` gate: `POST /iyke/hooks/decision` with
//!   `requestId`) or `"acp"` (chat-engine round-trip: answer `requestId` on
//!   `threadId` through the engine's permission-respond path,
//!   `ClaudeCodeEngine::resolve_permission`). Hide the buttons once the row
//!   has `resolvedAt`.
//! * `open.terminal` — Claude Code's own prompt inside a terminal. **Open
//!   only**: the answer happens in the terminal, so the row cannot carry
//!   Allow / Deny. It resolves when that terminal's next `PostToolUse` /
//!   `Stop` / `SessionEnd` hook arrives.
//! * `open.thread` (no longer emitted; kept for rows written before ACP rows
//!   became `permission.decide`), `open.chi_run`, `open.release_notes`,
//!   `open.pkg_updates`, `open.violations`.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Coalesce, NewNotification, NotificationKind};
use crate::engines::claude_code::notify::short_summary_of_input;

pub const SOURCE_HOOKS: &str = "iyke.hooks";
pub const SOURCE_ENGINE_CLAUDE: &str = "engine.claude-code";
pub const SOURCE_PKG_PERMISSIONS: &str = "pkg.permissions_check";
pub const SOURCE_CHI: &str = "chi";
pub const SOURCE_UPDATER: &str = "updater";

const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 240;

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max).collect();
        format!("{trimmed}…")
    }
}

fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// Last path component, accepting both separators (Windows cwd values).
fn basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn join_parts(parts: &[Option<String>]) -> Option<String> {
    let joined: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.as_deref())
        .filter(|p| !p.is_empty())
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(truncate(&joined.join(" · "), BODY_MAX))
    }
}

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
/// bus resolves it when the same terminal's next `PostToolUse` / `Stop` /
/// `SessionEnd` arrives (the prompt was answered, or the session ended).
pub fn terminal_permission_key(terminal_id: Option<&str>, session_id: Option<&str>) -> String {
    let scope = terminal_id
        .filter(|t| !t.is_empty())
        .or(session_id.filter(|s| !s.is_empty()))
        .unwrap_or("unknown");
    format!("permission:terminal:{scope}")
}

/// Hook events after which a terminal's pending `PermissionRequest` is over.
pub fn resolves_terminal_permission(hook_event_name: &str) -> bool {
    matches!(hook_event_name, "PostToolUse" | "Stop" | "SessionEnd")
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

/// A chat-engine (ACP) permission round-trip. The row carries Allow / Deny
/// inline (`permission.decide` via `acp`); the in-thread dialog answers the
/// same request, and whichever answers first resolves the row.
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
            "kind": "permission.decide",
            "via": "acp",
            "threadId": thread_id,
            "requestId": request_id,
            // Same shape as the hooks variant so a reader that only knows
            // that one does not trip on a missing field.
            "terminalId": Value::Null,
        })),
        source: SOURCE_ENGINE_CLAUDE.into(),
        dedupe_key: Some(engine_permission_key(thread_id, request_id)),
        coalesce: Coalesce::Once,
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

/// A Chi run reaching a terminal state (`chi_cache` status `done` /
/// `failed`). `cancelled` (a human did it) and any non-terminal status
/// produce nothing. No artifacts; see [`run_terminal_with_artifacts`].
pub fn run_terminal(
    run_id: &str,
    status: &str,
    engine_id: &str,
    brief: Option<&str>,
    cwd: Option<&str>,
    error: Option<&str>,
) -> Option<NewNotification> {
    run_terminal_with_artifacts(run_id, status, engine_id, brief, cwd, error, None)
}

/// `(count, first path)` of a `chi_cache.artifacts` value (a JSON array of
/// `{ path, mime, producedBy }`).
fn artifact_summary(artifacts: Option<&Value>) -> (usize, Option<String>) {
    let Some(items) = artifacts.and_then(Value::as_array) else {
        return (0, None);
    };
    let first = items
        .iter()
        .find_map(|a| a.get("path").and_then(Value::as_str))
        .map(str::to_string);
    (items.len(), first)
}

/// [`run_terminal`] plus what the run produced: the action carries
/// `artifactCount` and `firstArtifactPath` (D-07 "Opens what it produced" /
/// "Open artifact"), and the body leads with "N artifacts".
pub fn run_terminal_with_artifacts(
    run_id: &str,
    status: &str,
    engine_id: &str,
    brief: Option<&str>,
    cwd: Option<&str>,
    error: Option<&str>,
    artifacts: Option<&Value>,
) -> Option<NewNotification> {
    let (artifact_count, first_artifact) = artifact_summary(artifacts);
    let produced = match artifact_count {
        0 => None,
        1 => Some("1 artifact".to_string()),
        n => Some(format!("{n} artifacts")),
    };
    let kind = match status {
        "done" => NotificationKind::RunFinished,
        "failed" => NotificationKind::RunFailed,
        _ => return None,
    };
    let label = brief
        .map(first_line)
        .filter(|l| !l.is_empty())
        .map(|l| truncate(l, 60))
        .unwrap_or_else(|| format!("Chi run {}", short_id(run_id)));
    let (title, body) = match kind {
        NotificationKind::RunFinished => (
            format!("{label} finished"),
            join_parts(&[
                produced.clone(),
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
        _ => (
            format!("{label} failed"),
            join_parts(&[
                error.map(|e| truncate(first_line(e), 120)),
                produced.clone(),
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
    };
    Some(NewNotification {
        kind,
        title: truncate(&title, TITLE_MAX),
        body,
        action: Some(json!({
            "kind": "open.chi_run",
            "runId": run_id,
            "status": status,
            "artifactCount": artifact_count,
            "firstArtifactPath": first_artifact,
        })),
        source: SOURCE_CHI.into(),
        // Status in the key: a resumed run that failed and then finished must
        // not fold its `run_finished` into the unread `run_failed` row.
        dedupe_key: Some(format!("run:{run_id}:{status}")),
        coalesce: Coalesce::WhileUnread,
    })
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
    fn permission_from_engine_carries_acp_decide_scoped_by_thread() {
        let n = permission_from_engine(
            "0123456789abcdef",
            "req-7",
            "Bash",
            Some(&json!({ "command": "ls -la" })),
        );
        assert_eq!(n.kind, NotificationKind::Permission);
        assert_eq!(n.body.as_deref(), Some("ls -la · session 01234567"));
        let action = n.action.as_ref().unwrap();
        assert_eq!(action["kind"], "permission.decide");
        assert_eq!(action["via"], "acp");
        assert_eq!(action["threadId"], "0123456789abcdef");
        assert_eq!(action["requestId"], "req-7");
        assert!(action["terminalId"].is_null());
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
        for ev in ["PostToolUse", "Stop", "SessionEnd"] {
            assert!(resolves_terminal_permission(ev));
        }
        for ev in ["PreToolUse", "PermissionRequest", "Notification", "UserPromptSubmit"] {
            assert!(!resolves_terminal_permission(ev));
        }
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
