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
//! | `run_finished` / `run_failed` | [`run_terminal`] | `commands::chi::cache_update_done` |
//! | `update` | [`update`] | `commands::notifications::notifications_record_update` (FE updater + pkg registry check) |
//! | `violation` | [`violation`] | `pkg::permissions_check::record_violation` |
//! | `invite` | — | **no producer**: D-05's people surface does not exist yet |
//!
//! Action JSON is `{ "kind": "<action kind>", ...params }`; the UI (WP-40b)
//! maps each action kind to its buttons. Action kinds used here:
//! `permission.decide` (hooks gate: Allow once / Deny via
//! `/iyke/hooks/decision`), `open.thread`, `open.terminal`, `open.chi_run`,
//! `open.release_notes`, `open.pkg_updates`, `open.violations`.

use serde_json::{json, Value};

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
/// round-trip completes (answered, cancelled or timed out).
pub fn engine_permission_key(request_id: &str) -> String {
    format!("permission:acp:{request_id}")
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
    let scope = terminal_id
        .or(session_id)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".into());
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
        dedupe_key: Some(format!("permission:terminal:{scope}")),
        coalesce: Coalesce::WhileUnread,
    }
}

/// A chat-engine (ACP) permission round-trip. The in-UI dialog answers it;
/// the row opens the thread.
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
        dedupe_key: Some(engine_permission_key(request_id)),
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

/// A denied pkg action (`pkg_permission_violations` row). Repeats of the same
/// (pkg, scope, target) fold into one unread row whose `count` is the number
/// of denials ("3 denials" in D-07).
pub fn violation(pkg_id: &str, scope_kind: &str, attempted: &str) -> NewNotification {
    let short = pkg_id.rsplit('.').next().unwrap_or(pkg_id);
    NewNotification {
        kind: NotificationKind::Violation,
        title: truncate(
            &format!(
                "{short} was blocked from {} {}",
                violation_verb(scope_kind),
                truncate(attempted, 80)
            ),
            TITLE_MAX,
        ),
        body: join_parts(&[Some(pkg_id.to_string()), Some(scope_kind.to_string())]),
        action: Some(json!({ "kind": "open.violations", "pkgId": pkg_id })),
        source: SOURCE_PKG_PERMISSIONS.into(),
        dedupe_key: Some(format!("violation:{pkg_id}:{scope_kind}:{attempted}")),
        coalesce: Coalesce::WhileUnread,
    }
}

// ─── run finished / failed ──────────────────────────────────────────────────

/// A Chi run reaching a terminal state (`chi_cache` status `done` /
/// `failed`). `cancelled` (a human did it) and any non-terminal status
/// produce nothing.
pub fn run_terminal(
    run_id: &str,
    status: &str,
    engine_id: &str,
    brief: Option<&str>,
    cwd: Option<&str>,
    error: Option<&str>,
) -> Option<NewNotification> {
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
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
        _ => (
            format!("{label} failed"),
            join_parts(&[
                error.map(|e| truncate(first_line(e), 120)),
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
    };
    Some(NewNotification {
        kind,
        title: truncate(&title, TITLE_MAX),
        body,
        action: Some(json!({ "kind": "open.chi_run", "runId": run_id, "status": status })),
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

/// A shell or pkg release is available. One row per (target, version), ever:
/// the updater re-checks every 6 h and must not re-announce a version the
/// user has already seen.
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
        assert_eq!(n.coalesce, Coalesce::WhileUnread);
        // No terminal id: fall back to the Claude session id.
        let n = permission_from_hook_request(None, None, None, Some("sess"), None);
        assert_eq!(n.title, "Claude is asking to use a tool");
        assert_eq!(n.dedupe_key.as_deref(), Some("permission:terminal:sess"));
    }

    #[test]
    fn permission_from_engine_opens_the_thread() {
        let n = permission_from_engine(
            "0123456789abcdef",
            "req-7",
            "Bash",
            Some(&json!({ "command": "ls -la" })),
        );
        assert_eq!(n.kind, NotificationKind::Permission);
        assert_eq!(n.body.as_deref(), Some("ls -la · session 01234567"));
        assert_eq!(n.action.as_ref().unwrap()["kind"], "open.thread");
        assert_eq!(n.action.as_ref().unwrap()["threadId"], "0123456789abcdef");
        assert_eq!(n.dedupe_key.as_deref(), Some("permission:acp:req-7"));
        assert_eq!(n.source, SOURCE_ENGINE_CLAUDE);
    }

    #[test]
    fn violation_names_the_pkg_and_folds_per_target() {
        let n = violation("com.ikenga.pkg-browser", "shell.execute", "ffmpeg");
        assert_eq!(n.kind, NotificationKind::Violation);
        assert_eq!(n.title, "pkg-browser was blocked from spawning ffmpeg");
        assert_eq!(n.body.as_deref(), Some("com.ikenga.pkg-browser · shell.execute"));
        assert_eq!(n.action.as_ref().unwrap()["kind"], "open.violations");
        assert_eq!(
            n.dedupe_key.as_deref(),
            Some("violation:com.ikenga.pkg-browser:shell.execute:ffmpeg")
        );
        assert_eq!(n.coalesce, Coalesce::WhileUnread);
        let fetch = violation("p", "http.fetch", "https://example.com");
        assert_eq!(fetch.title, "p was blocked from fetching https://example.com");
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
