//! The held hooks-gate `permission` row (WP-40), moved out of the
//! desktop-only `crate::notifications::producers` so the headless daemon's
//! terminals record the **same** row the desktop does for a held
//! `PreToolUse` (daemon asks gap 1: `server::term_hooks`). Pure builders; the
//! writer is [`super::routing::record_ask`].
//!
//! One row per held request, keyed `permission:hook:<request id>`
//! ([`hook_gate_key`], which `routing::AskKey::Hook` parses). The row carries
//! Allow / Deny inline (`permission.decide`, `via: "hooks"`); the browser's
//! bell, the Companion cards, the home tile and Web Push all read it.

use serde_json::{json, Value};

use super::routing::{classify, classify_terminal, Attribution};
use super::run::{basename, join_parts, short_id, truncate, TITLE_MAX};
use super::{Coalesce, NewNotification, NotificationKind};

pub const SOURCE_HOOKS: &str = "iyke.hooks";

/// One-line summary of a tool's input for use as a notification body. We
/// keep it conservative: prefer well-known fields (command, path, url,
/// question) and fall back to a generic "(tap to review)". Also used by the
/// WP-40 permission producers (`notifications::producers`) so a persisted
/// notification row and the OS notification describe a tool call identically.
pub fn short_summary_of_input(tool_input: Option<&Value>) -> String {
    let Some(input) = tool_input else {
        return "(tap to review)".into();
    };
    // Try the most common high-signal fields first.
    for key in &["command", "path", "url", "file_path", "question"] {
        if let Some(s) = input.get(*key).and_then(Value::as_str) {
            return truncate(s, 120);
        }
    }
    // AskUserQuestion has a `questions[]` array.
    if let Some(questions) = input.get("questions").and_then(Value::as_array) {
        if let Some(q) = questions
            .first()
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
        {
            return truncate(q, 120);
        }
    }
    "(tap to review)".into()
}

/// Dedupe key of a held hooks-gate request. Resolved (marked read) once the
/// human decides or the gate times out.
pub fn hook_gate_key(request_id: &str) -> String {
    format!("permission:hook:{request_id}")
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

/// The attribution a hook ask is recorded with. `project` is the
/// `(id, root_path)` the cwd resolved to; the root classifies, else the cwd
/// itself. A terminal's asks are always the Owner's own work, so
/// `requested_by` is `None` (§5.7).
pub fn attribution(facts: &AskFacts, project: Option<&(String, String)>) -> Attribution {
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
