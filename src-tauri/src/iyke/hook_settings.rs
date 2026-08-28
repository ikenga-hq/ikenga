//! The settings file Ikenga hands to `claude` via `--settings`.
//!
//! # Why this exists, and why it is NOT a `CLAUDE_CONFIG_DIR` overlay
//!
//! The shell has a live view of what a `claude` session is doing — the cost
//! HUD, the tool-call feed, the permission inbox, the git ledger (all mounted
//! in `shell/panes/views/terminal-view.tsx`). Every one of them is fed by
//! Claude Code hooks and by the `statusLine` command POSTing to this app's
//! iyke bridge.
//!
//! That wiring used to be delivered by pointing `CLAUDE_CONFIG_DIR` at an
//! Ikenga-owned overlay directory and seeding a `settings.json` inside it. It
//! was removed in ikenga#149 for two independent reasons: it shadowed the
//! user's real `~/.claude` (they got a login prompt and a 0-project config),
//! and it hardcoded `port: 0`, so every hook fired
//! `curl … http://127.0.0.1:0/iyke/hooks/event` — a failing curl on every tool
//! call, on seven hook events.
//!
//! `--settings` is the right seam and the CLI documents it as such: "Path to a
//! settings JSON file or a JSON string to load **additional** settings from."
//! Additional, i.e. layered on top of user/project/local rather than replacing
//! them — `--setting-sources` is the separate flag that controls those. So:
//!
//! * the user's `~/.claude.json`, `~/.claude/settings.json`, credentials,
//!   skills, agents, commands and MCP servers are all discovered natively and
//!   are never written to;
//! * the port and token are baked in at bridge-start, so they are always the
//!   live ones — the class of bug that made the overlay useless cannot recur
//!   silently, because a wrong port here means the file was written before the
//!   server bound, which is impossible by construction;
//! * only terminals Ikenga itself launches get the wiring. A `claude` the user
//!   starts by hand in a plain shell is untouched, which is the correct
//!   default — the shell has no business injecting hooks into sessions it
//!   does not own.
//!
//! The file lives beside `control.json` (same directory, same 0600 mode) and
//! carries a bearer token, so it is written with the same care.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

/// Every hook event the shell's terminal surfaces consume.
///
/// `PreToolUse`/`PostToolUse` drive the tool-call feed and the git ledger;
/// `UserPromptSubmit`/`SessionStart` drive context injection; `PreCompact`
/// drives the compaction guard; `Notification`/`PermissionRequest` drive the
/// permission inbox; `SessionEnd` closes the session out.
const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "PreCompact",
    "Notification",
    "PermissionRequest",
];

/// `--settings` file name, written next to `control.json`.
pub const FILE_NAME: &str = "claude-hooks-settings.json";

/// Build the settings document. Split out from the write so it can be asserted
/// on directly — the whole point of this module is that the port and token are
/// real, and a test that only checked "a file exists" would have passed against
/// the port-0 overlay too.
///
/// `terminal_id` is baked into the hook and statusline URLs so every event can
/// be attributed to the Ikenga terminal that spawned the claude session, even
/// when several terminals share the same cwd.
pub fn build_for_terminal(port: u16, token: &str, terminal_id: Option<&str>) -> serde_json::Value {
    // `-s` keeps curl quiet on success; `--max-time` matters because a hook
    // that hangs stalls the session, and the shell is a local listener that
    // either answers immediately or is gone (app quit mid-session).
    let post = |path: &str| {
        let suffix = terminal_id.map(|t| format!("?terminal={}", t)).unwrap_or_default();
        format!(
            "curl -s --max-time 2 -X POST -H 'Authorization: Bearer {token}' \
-H 'Content-Type: application/json' --data-binary @- \
http://127.0.0.1:{port}{path}{suffix}"
        )
    };

    let hook_cmd = post("/iyke/hooks/event");
    let hook_block = json!([{ "type": "command", "command": hook_cmd }]);

    let mut hooks = serde_json::Map::new();
    for event in HOOK_EVENTS {
        // Claude Code's hook schema takes a matcher list for tool-scoped
        // events and a bare hook list for the rest. `PreToolUse` /
        // `PostToolUse` / `PreCompact` are the matcher-shaped ones.
        let value = if matches!(*event, "PreToolUse" | "PostToolUse" | "PreCompact") {
            json!([{ "matcher": "*", "hooks": hook_block }])
        } else {
            json!([{ "hooks": hook_block }])
        };
        hooks.insert((*event).to_string(), value);
    }

    json!({
        "statusLine": {
            "type": "command",
            "command": post("/iyke/statusline/event"),
            "padding": 0,
            "refreshInterval": 300
        },
        "hooks": hooks
    })
}

/// Per-terminal settings file name, written next to `control.json`.
/// The frontend and Rust both compute the same path from the terminal id.
pub fn terminal_file_name(terminal_id: &str) -> String {
    format!("claude-hooks-{terminal_id}.json")
}

/// Write a per-terminal settings file next to `control.json`, 0600, atomically.
///
/// `terminal_id` is baked into the hook + statusline URLs as a query parameter.
/// Returns the absolute path to hand to `claude --settings`.
pub fn write_for_terminal(
    dir: &Path,
    port: u16,
    token: &str,
    terminal_id: Option<&str>,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create {}", dir.display()))?;
    let file_name = terminal_id.map(terminal_file_name).unwrap_or_else(|| FILE_NAME.to_string());
    let path = dir.join(file_name);
    let data = serde_json::to_vec_pretty(&build_for_terminal(port, token, terminal_id))
        .context("serialize claude hook settings")?;

    let tmp = path.with_extension("json.tmp");
    write_private(&tmp, &data)?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {}", path.display()))?;
    Ok(path)
}

/// 0600 on unix — it carries the bridge bearer token, same as `control.json`.
fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("open {} with mode 0600", path.display()))?;
    f.write_all(data)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression that motivated this module: the overlay wrote `port: 0`
    /// and no token, so every hook curl failed. Assert the real values land in
    /// every command string — "a settings file was produced" is not the claim.
    #[test]
    fn every_command_carries_the_live_port_and_token() {
        let v = build_for_terminal(44945, "tok-abc123", None);
        let s = serde_json::to_string(&v).unwrap();

        assert!(!s.contains("127.0.0.1:0/"), "port 0 leaked back in: {s}");
        assert!(s.contains("127.0.0.1:44945/iyke/hooks/event"));
        assert!(s.contains("127.0.0.1:44945/iyke/statusline/event"));

        let commands: Vec<&str> = s.match_indices("curl ").map(|(i, _)| &s[i..]).collect();
        assert!(!commands.is_empty());
        // Every curl in the document must be authenticated.
        assert_eq!(
            s.matches("curl ").count(),
            s.matches("Authorization: Bearer tok-abc123").count(),
            "an unauthenticated curl slipped into the settings document"
        );
    }

    #[test]
    fn covers_every_event_the_shell_listens_for() {
        let v = build_for_terminal(1, "t", None);
        let hooks = v["hooks"].as_object().expect("hooks object");
        for event in HOOK_EVENTS {
            assert!(hooks.contains_key(*event), "missing hook event {event}");
        }
        // Tool-scoped events must carry a matcher, or Claude Code ignores them.
        for event in ["PreToolUse", "PostToolUse", "PreCompact"] {
            assert!(
                hooks[event][0].get("matcher").is_some(),
                "{event} needs a matcher"
            );
        }
        for event in ["SessionStart", "SessionEnd", "UserPromptSubmit"] {
            assert!(
                hooks[event][0].get("matcher").is_none(),
                "{event} must not carry a matcher"
            );
        }
    }

    #[test]
    fn writes_a_private_file_and_returns_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_for_terminal(dir.path(), 4242, "tok", None).expect("write");
        assert_eq!(path.file_name().unwrap(), FILE_NAME);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("127.0.0.1:4242"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "settings file carries a bearer token");
        }
        // No `.tmp` left behind.
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn per_terminal_build_bakes_terminal_id_into_urls() {
        let v = build_for_terminal(44945, "tok-abc123", Some("term-xyz"));
        let s = serde_json::to_string(&v).unwrap();

        assert!(s.contains("127.0.0.1:44945/iyke/hooks/event?terminal=term-xyz"));
        assert!(s.contains("127.0.0.1:44945/iyke/statusline/event?terminal=term-xyz"));
        // No un-attributed (bare) hook URLs.
        assert!(!s.contains("/iyke/hooks/event\""));
        assert!(!s.contains("/iyke/statusline/event\""));
    }
}
