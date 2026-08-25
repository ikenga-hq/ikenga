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
pub mod event;
pub mod jsonl_reader;
pub mod session;
pub mod session_browser;
pub mod stream_parser;

use std::path::{Path, PathBuf};

/// Convert an absolute project dir to the Claude Code on-disk slug.
#[allow(dead_code)]
pub fn project_dir_to_slug(project_dir: &str) -> String {
    project_dir.replace('/', "-")
}

/// Inverse of `project_dir_to_slug` — `-Users-jane-work` →
/// `/Users/jane/work`. Best-effort; some legacy slugs may have
/// lost trailing slashes.
pub fn slug_to_project_dir(slug: &str) -> String {
    slug.replace('-', "/")
}

/// Resolve `~/.claude/projects/<slug>` for a given slug.
#[allow(dead_code)]
pub fn project_log_dir(slug: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".claude").join("projects").join(slug))
}

/// `~/.claude/projects/` root.
pub fn projects_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".claude").join("projects"))
}

/// True for files Claude Code writes as session logs.
pub fn is_session_jsonl(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| {
                // sessionId is a uuid v4 — 36 chars with 4 hyphens.
                s.len() == 36 && s.matches('-').count() == 4
            })
            .unwrap_or(false)
}
