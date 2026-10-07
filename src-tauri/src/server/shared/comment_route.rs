//! The pin-routing pieces both surfaces share: the sink names, the result
//! shape, the two prompt renderings and the claude-PTY pick. The desktop's
//! `comment_route` command (`commands::comment_route`, whose module note
//! describes the three sinks) and the daemon's arm (`server::rpc_exec`, gap
//! audit 2026-10-06 rank 23) both build on these, so a pin renders and
//! records identically wherever it is routed from.

use serde::{Deserialize, Serialize};

use super::comments::Comment;
use crate::pty::PtyManager;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RouteSink {
    Terminal,
    Chi,
    Clipboard,
}

impl RouteSink {
    pub fn as_str(&self) -> &'static str {
        match self {
            RouteSink::Terminal => "terminal",
            RouteSink::Chi => "chi",
            RouteSink::Clipboard => "clipboard",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteResult {
    /// The sink the dispatcher actually used. Never `None` now that the
    /// clipboard sink is always reachable, but kept optional so the FE's
    /// existing null-handling stays valid.
    pub sink: Option<String>,
    /// PTY id the prompt was written to, when the terminal sink was used.
    /// Useful for the grid UI to show "delivered to term 2 · claude".
    pub pty_id: Option<String>,
    /// Foreground process name on that PTY at routing time. Lets the FE
    /// distinguish "claude" from a wrapper like "claude-code". Audit only.
    pub pty_foreground: Option<String>,
    /// Chi run id, when the `chi` sink was used. The FE links to the run.
    pub run_id: Option<String>,
    /// Rendered prompt, when the `clipboard` sink was used. The FE writes
    /// this to the clipboard — Rust deliberately does not touch the
    /// clipboard so the write stays inside a user gesture.
    pub clipboard_text: Option<String>,
    /// Updated comment after the routing fields were recorded.
    pub comment: Comment,
}

/// One-line nudge written to a live claude PTY. Deliberately terse: claude
/// pulls the full pin payload via `mcp-iyke.read_pin(id)`.
pub fn terminal_line(comment: &Comment) -> String {
    format!(
        "address pin #{} (artifact: {} · selector: {})\n",
        comment.id, comment.artifact_path, comment.selector
    )
}

/// Fully self-contained prompt for the sinks that have no `read_pin` access
/// (clipboard paste target, headless chi run). Unlike `terminal_line` this
/// inlines the pin body so it works with no mcp-iyke and no shell running.
pub fn standalone_prompt(comment: &Comment) -> String {
    let mut s = format!(
        "Address pin #{} on artifact `{}`.\n\nSelector: `{}`\n\nNote:\n{}\n",
        comment.id, comment.artifact_path, comment.selector, comment.text
    );
    if let Some(shot) = comment.screenshot_path.as_deref().filter(|p| !p.is_empty()) {
        s.push_str(&format!("\nScreenshot: {shot}\n"));
    }
    s
}

/// Directory holding the artifact, used as the chi run's cwd so relative
/// paths in the agent's edits resolve against the artifact's own folder.
pub fn parent_dir(artifact_path: &str) -> Option<String> {
    std::path::Path::new(artifact_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Pick a PTY whose foreground command is `claude` (or `claude-*`).
///
/// When `preferred_pty_id` is supplied and that PTY's foreground is still
/// claude, it wins — this lets the FE pin delivery to the *visible* terminal
/// (the most-recently-focused tab) rather than letting HashMap iteration
/// arbitrarily pick a sibling claude PTY. The fallback path scans the full
/// snapshot.
pub fn pick_claude_pty(
    pty: &PtyManager,
    preferred_pty_id: Option<&str>,
) -> Option<(String, String)> {
    let snap = pty.foreground_snapshot();
    if let Some(preferred) = preferred_pty_id {
        if let Some(fg) = snap.get(preferred) {
            if fg.name.starts_with("claude") {
                return Some((preferred.to_string(), fg.name.clone()));
            }
        }
    }
    snap.into_iter()
        .find(|(_, fg)| fg.name.starts_with("claude"))
        .map(|(id, fg)| (id, fg.name))
}
