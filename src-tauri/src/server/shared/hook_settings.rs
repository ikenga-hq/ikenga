//! The Claude Code `--settings` document that wires a terminal's hooks and
//! statusline to a receiver, shared by the desktop (`iyke::hook_settings`,
//! the iyke bridge) and the daemon (`server::term_hooks`, its own
//! per-terminal endpoint).
//!
//! The two differ only in WHERE the commands POST and HOW they authenticate:
//! the desktop bakes the bridge bearer into each command, the daemon points
//! curl at a 0600 header file so the per-terminal secret never appears in a
//! process argument list another user could read. Everything else (the event
//! list, the matcher shapes, and the nested timeouts of the held
//! `PreToolUse` gate) is one copy here so the two cannot drift.

use std::path::Path;

use serde_json::json;

/// How long the receiver parks a held `PreToolUse` response waiting for a
/// human decision (ikenga#154). Must stay strictly below
/// [`GATE_CURL_MAX_TIME_SECS`].
pub const GATE_HOLD_SECS: u64 = 30;

/// `curl --max-time` for the gateable `PreToolUse` hook. Must outlast the
/// hold so the decision actually reaches Claude Code, and stay below the hook
/// timeout so curl is not killed mid-read.
pub const GATE_CURL_MAX_TIME_SECS: u32 = 35;

/// Explicit Claude Code hook timeout for `PreToolUse`. Claude Code defaults to
/// 60s; we set it so the whole chain is declared in one place.
pub const GATE_HOOK_TIMEOUT_SECS: u32 = 40;

/// `curl --max-time` for every hook that answers immediately.
pub const FAST_HOOK_MAX_TIME_SECS: u32 = 2;

/// Every hook event the shell's terminal surfaces consume.
///
/// `PreToolUse`/`PostToolUse` drive the tool-call feed and the git ledger;
/// `UserPromptSubmit`/`SessionStart` drive context injection; `PreCompact`
/// drives the compaction guard; `Notification`/`PermissionRequest` drive the
/// permission inbox; `SessionEnd` closes the session out. `Stop` and
/// `PostToolUseFailure` resolve the notification row of an in-terminal
/// `PermissionRequest` (see `notifications::producers::
/// {ends_terminal_permissions, finishes_terminal_tool}` — every event those
/// match must be listed here).
pub const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "PreCompact",
    "Notification",
    "PermissionRequest",
    "Stop",
    "PostToolUseFailure",
];

/// How a hook command proves who it is.
pub enum Auth<'a> {
    /// `-H 'Authorization: Bearer <token>'`, inline (the desktop bridge).
    Bearer(&'a str),
    /// `-H @<file>`: curl reads the header from a private file, so the secret
    /// is never in an argv (the daemon). The file holds
    /// `Authorization: Bearer <token>`.
    HeaderFile(&'a Path),
}

/// Where a terminal's hooks and statusline POST.
pub struct Wiring<'a> {
    /// `http://127.0.0.1:<port>` (no trailing slash).
    pub base_url: &'a str,
    pub hook_path: &'a str,
    pub statusline_path: &'a str,
    pub auth: Auth<'a>,
    /// Baked into every URL as `?terminal=` so an event is attributed to the
    /// Ikenga terminal that spawned the claude session.
    pub terminal_id: Option<&'a str>,
}

/// POSIX single-quote escape: wrap in `'…'`, replace each `'` with `'\''`.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Build the settings document. Split out from the write so it can be
/// asserted on directly — a test that only checked "a file exists" would have
/// passed against the port-0 overlay that ikenga#149 removed.
pub fn build(w: &Wiring<'_>) -> serde_json::Value {
    // `-s` keeps curl quiet on success; `--max-time` matters because a hook
    // that hangs stalls the session.
    //
    // The budget is NOT uniform, and the ordering is load-bearing. A held
    // `PreToolUse` gate parks the HTTP response for up to `GATE_HOLD_SECS`
    // while a human decides, so the three timeouts must nest strictly:
    //
    //     receiver hold (30s)  <  curl --max-time (35s)  <  hook timeout (40s)
    //
    // If curl gives up first it exits non-zero with empty stdout and Claude
    // Code proceeds with the tool call — the gate silently does nothing. If
    // Claude Code's own hook timeout fires first it kills curl, same outcome.
    // Every other hook keeps the tight 2s budget.
    let auth = match &w.auth {
        Auth::Bearer(token) => format!("-H 'Authorization: Bearer {token}'"),
        Auth::HeaderFile(path) => format!("-H {}", sh_quote(&format!("@{}", path.display()))),
    };
    let post_with = |path: &str, max_time: u32| {
        let suffix = w
            .terminal_id
            .map(|t| format!("?terminal={t}"))
            .unwrap_or_default();
        let url = format!("{}{path}{suffix}", w.base_url);
        // The Bearer form keeps the desktop's exact historical shape; the
        // header-file form quotes the URL, since a base URL could carry `[`
        // (IPv6) that a shell would glob.
        match &w.auth {
            Auth::Bearer(_) => format!(
                "curl -s --max-time {max_time} -X POST {auth} \
-H 'Content-Type: application/json' --data-binary @- {url}"
            ),
            Auth::HeaderFile(_) => format!(
                "curl -s --max-time {max_time} -X POST {auth} \
-H 'Content-Type: application/json' --data-binary @- {}",
                sh_quote(&url)
            ),
        }
    };
    let post = |path: &str| post_with(path, FAST_HOOK_MAX_TIME_SECS);

    let hook_block = json!([{ "type": "command", "command": post(w.hook_path) }]);

    // `PreToolUse` is the only gateable event, so it is the only one that gets
    // the wide budget plus an explicit `timeout` (Claude Code defaults to 60s,
    // which would outlive curl and leave the hold un-answered).
    let gate_block = json!([{
        "type": "command",
        "command": post_with(w.hook_path, GATE_CURL_MAX_TIME_SECS),
        "timeout": GATE_HOOK_TIMEOUT_SECS,
    }]);

    let mut hooks = serde_json::Map::new();
    for event in HOOK_EVENTS {
        // Claude Code's hook schema takes a matcher list for tool-scoped
        // events and a bare hook list for the rest. `PermissionRequest` needs
        // its matcher or the action runner never sees a native prompt open
        // (WP-53 N3: a PTY inject's trailing CR would answer it).
        let value = if matches!(
            *event,
            "PreToolUse"
                | "PostToolUse"
                | "PostToolUseFailure"
                | "PermissionRequest"
                | "PreCompact"
        ) {
            let hooks = if *event == "PreToolUse" {
                &gate_block
            } else {
                &hook_block
            };
            json!([{ "matcher": "*", "hooks": hooks }])
        } else {
            json!([{ "hooks": hook_block }])
        };
        hooks.insert((*event).to_string(), value);
    }

    json!({
        "statusLine": {
            "type": "command",
            "command": post(w.statusline_path),
            "padding": 0,
            "refreshInterval": 300
        },
        "hooks": hooks
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate is only real if the three timeouts nest (see `build`).
    #[test]
    fn the_gate_timeouts_nest() {
        assert!((GATE_HOLD_SECS as u32) < GATE_CURL_MAX_TIME_SECS);
        assert!(GATE_CURL_MAX_TIME_SECS < GATE_HOOK_TIMEOUT_SECS);
    }

    #[test]
    fn a_header_file_keeps_the_secret_out_of_every_command() {
        let hdr = Path::new("/data/term-hooks/claude-hooks-t1.hdr");
        let v = build(&Wiring {
            base_url: "http://127.0.0.1:9",
            hook_path: "/term-hooks/event",
            statusline_path: "/term-hooks/statusline",
            auth: Auth::HeaderFile(hdr),
            terminal_id: Some("t1"),
        });
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("Bearer"), "no inline secret: {s}");
        assert_eq!(
            s.matches("curl ").count(),
            s.matches("-H '@/data/term-hooks/claude-hooks-t1.hdr'")
                .count(),
            "every curl reads the header file"
        );
        assert!(s.contains("'http://127.0.0.1:9/term-hooks/event?terminal=t1'"));
    }

    #[test]
    fn sh_quote_survives_an_embedded_quote() {
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
    }
}
