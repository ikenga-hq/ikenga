//! Claude Code session integration (phase 3).
//!
//! Two parsers feed the same `ChatEvent` stream:
//!  - `stream_parser` parses `claude --output-format stream-json --verbose`
//!    output (live PTY). Envelope shape: `system:init`, `assistant`, `user`,
//!    `result`, `rate_limit_event`, `system:hook_*`. Captured 2026-04-30
//!    against Claude Code v2.1.123 — see `<workspace>/plans/shell/phase-0-report.md` § Test 7-8.
//!  - `jsonl_reader` parses on-disk session logs at
//!    `~/.claude/projects/<slug>/<uuid>.jsonl`. Envelope shape:
//!    `user` / `assistant` / `attachment` / `queue-operation` / `last-prompt`.
//!    The inner `message.content[]` blocks are identical to the live stream
//!    (Anthropic message shape) — `text` / `thinking` / `tool_use` /
//!    `tool_result` — so dispatch into them is shared.
//!
//! `slug` translates a project dir to a Claude Code project slug. Claude Code
//! replaces every `/` with `-`, so `/Users/jane/work` becomes
//! `-Users-jane-work`. We keep the inverse for display.

pub mod artifact_watcher;
pub mod discovery;
pub mod session;
pub mod session_browser;

// The parsers and the slug / session-log helpers are AppHandle-free and live
// in the ungated `server::shared::claude_sessions` (WP-19 slice 5b), so the
// daemon's session arms compile in the headless build. Re-exported so every
// `crate::claude::{event, jsonl_reader, stream_parser, …}` path still resolves.
pub use crate::server::shared::claude_sessions::{
    event, is_session_jsonl, jsonl_reader, slug_to_project_dir, stream_parser,
};

use std::path::PathBuf;

/// Convert an absolute project dir to the Claude Code on-disk slug.
#[allow(dead_code)]
pub fn project_dir_to_slug(project_dir: &str) -> String {
    project_dir.replace('/', "-")
}

/// Resolve `~/.claude/projects/<slug>` for a given slug.
#[allow(dead_code)]
pub fn project_log_dir(slug: &str) -> Option<PathBuf> {
    // $HOME is unset on Windows; use the platform resolver.
    let home = crate::platform::home_dir()?;
    Some(home.join(".claude").join("projects").join(slug))
}

/// `~/.claude/projects/` root.
pub fn projects_root() -> Option<PathBuf> {
    let home = crate::platform::home_dir()?;
    Some(home.join(".claude").join("projects"))
}
