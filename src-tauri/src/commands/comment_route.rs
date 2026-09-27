//! Routing dispatcher for artifact-grid pin comments.
//!
//! When the user clicks a pin in the artifact grid (or `art.notes.send` is
//! called from inside an artifact), this command decides where the
//! structured prompt should land:
//!
//! - **Terminal** — preferred when an active claude PTY exists. Writes a
//!   one-line prompt referencing the pin id; claude then pulls the full
//!   payload via `mcp-iyke.read_pin(id)`.
//! - **Chi** — spawns a headless one-off agent run (`chi_run`) seeded with
//!   the pin's structured prompt. Returns the `run_id` so the FE can link
//!   to it; the run itself streams into the chi cache, not into any pane.
//! - **Clipboard** — the auto-detect fallback when no claude PTY exists.
//!   Has no side effects and spends no tokens: the command returns the
//!   fully-rendered prompt in `clipboard_text` and the FE copies it, so the
//!   user can paste it into `iyke chi run`, an MCP host, or a terminal.
//!
//! WP-05 removed the `sidepane` and `both` sinks along with the chat
//! surface. `sidepane` used to emit a `pin://routed` event that the
//! side-pane Chat composer consumed; with no composer left the event had
//! no receiver and the sink silently swallowed pins. Clipboard replaces it
//! as the always-available fallback.
//!
//! Per-click override goes through the `override_sink` argument. Without
//! it the dispatcher auto-detects.
//!
//! Failure modes:
//! - **Busy PTY**: a future enhancement; v0 always writes immediately.
//! - **No mcp-iyke**: handled implicitly — claude in the terminal will fail
//!   the `read_pin` call and surface the error to the user.
//! - **Dead PTY mid-write**: falls back to clipboard rather than dropping
//!   the pin, and the recorded sink reflects what actually happened.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use super::chi::{spawn_chi_run, ChiCache, ChiRunOpts, ChiRuntime};
use super::comments::{comment_get, comment_record_routing, Comment};
use super::db::PaDb;
use crate::pty::PtyManager;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RouteSink {
    Terminal,
    Chi,
    Clipboard,
}

impl RouteSink {
    fn as_str(&self) -> &'static str {
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
fn terminal_line(comment: &Comment) -> String {
    format!(
        "address pin #{} (artifact: {} · selector: {})\n",
        comment.id, comment.artifact_path, comment.selector
    )
}

/// Fully self-contained prompt for the sinks that have no `read_pin` access
/// (clipboard paste target, headless chi run). Unlike `terminal_line` this
/// inlines the pin body so it works with no mcp-iyke and no shell running.
fn standalone_prompt(comment: &Comment) -> String {
    let mut s = format!(
        "Address pin #{} on artifact `{}`.\n\nSelector: `{}`\n\nNote:\n{}\n",
        comment.id, comment.artifact_path, comment.selector, comment.text
    );
    if let Some(shot) = comment.screenshot_path.as_deref().filter(|p| !p.is_empty()) {
        s.push_str(&format!("\nScreenshot: {shot}\n"));
    }
    s
}

/// Dispatch a pin to its routing sink. The FE typically invokes this
/// after creating a pin (or when the user re-clicks an existing pin to
/// re-route). `override_sink` forces a specific sink; omit to auto-detect.
#[tauri::command]
pub async fn comment_route(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    pty: State<'_, Arc<PtyManager>>,
    cache: State<'_, ChiCache>,
    runtime: State<'_, Arc<ChiRuntime>>,
    id: i64,
    override_sink: Option<RouteSink>,
    preferred_pty_id: Option<String>,
) -> Result<RouteResult, String> {
    let comment = comment_get(db.clone(), id).await?;

    // Pick the active claude PTY, if any. This is the auto-detect path.
    let claude_pty = pick_claude_pty(&pty, preferred_pty_id.as_deref());

    // Auto-detect prefers a live claude PTY (zero cost, immediate context)
    // and otherwise falls back to clipboard — never to chi, because
    // spawning an agent spends tokens and must stay an explicit choice.
    let chosen = override_sink.unwrap_or(if claude_pty.is_some() {
        RouteSink::Terminal
    } else {
        RouteSink::Clipboard
    });

    let mut pty_id_used: Option<String> = None;
    let mut pty_foreground_used: Option<String> = None;
    let mut run_id_used: Option<String> = None;
    let mut clipboard_text: Option<String> = None;

    match chosen {
        RouteSink::Terminal => {
            if let Some((pty_id, fg_name)) = &claude_pty {
                if pty
                    .write(pty_id, terminal_line(&comment).as_bytes())
                    .is_ok()
                {
                    pty_id_used = Some(pty_id.clone());
                    pty_foreground_used = Some(fg_name.clone());
                }
            }
            // The PTY died between the snapshot and the write, or the caller
            // forced `terminal` with nothing running. Degrade to clipboard so
            // the pin is never silently dropped.
            if pty_id_used.is_none() {
                clipboard_text = Some(standalone_prompt(&comment));
            }
        }
        RouteSink::Chi => {
            let opts = ChiRunOpts {
                engine_id: "claude-code".to_string(),
                prompt: standalone_prompt(&comment),
                cwd: parent_dir(&comment.artifact_path),
                model: None,
                mode: None,
                timeout_seconds: None,
                parent_id: None,
                resume_session_id: None,
                persistent: false,
            };
            let run = spawn_chi_run(
                db.inner().clone(),
                &cache,
                &runtime,
                Some(&app),
                opts,
                "pin",
            )
            .await?;
            run_id_used = Some(run.run_id);
        }
        RouteSink::Clipboard => {
            clipboard_text = Some(standalone_prompt(&comment));
        }
    }

    // Record what actually happened, not what was asked for: a `terminal`
    // request that fell through is audited as `clipboard`.
    let recorded_sink = match chosen {
        RouteSink::Terminal if pty_id_used.is_none() => RouteSink::Clipboard,
        other => other,
    };
    let recorded_sink = recorded_sink.as_str();

    let updated =
        comment_record_routing(db, comment.id, recorded_sink.to_string(), None, None).await?;

    Ok(RouteResult {
        sink: Some(recorded_sink.to_string()),
        pty_id: pty_id_used,
        pty_foreground: pty_foreground_used,
        run_id: run_id_used,
        clipboard_text,
        comment: updated,
    })
}

/// Directory holding the artifact, used as the chi run's cwd so relative
/// paths in the agent's edits resolve against the artifact's own folder.
fn parent_dir(artifact_path: &str) -> Option<String> {
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
fn pick_claude_pty(pty: &PtyManager, preferred_pty_id: Option<&str>) -> Option<(String, String)> {
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
