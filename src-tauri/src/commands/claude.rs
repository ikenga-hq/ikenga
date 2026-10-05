//! Claude Code session integration.
//!
//! Chat threads keyed by a stable, frontend-minted `thread_id`. Each session
//! owns an optional streaming-input claude child (piped stdin/stdout — claude
//! rejects stream-json over a TTY). Events emit on `session://{thread_id}`.
//! The session machinery lives in `crate::claude::session`.
//!
//! "Open in terminal" affordances (session-detail, new-session dialog, claude
//! Run Command) spawn `bash -c "claude …; exec $SHELL -i"` directly via the
//! generic `pty_spawn` from `commands::pty` — no claude-specific PTY path is
//! kept on the Rust side anymore.
//!
//! Wires:
//!  - `session_ensure` / `session_send` / `session_tool_result` /
//!    `session_cancel` / `session_destroy` / `session_destroy_all` — chat
//!    lifecycle.
//!  - `claude_list_sessions` — scans `~/.claude/projects/**` and summarizes
//!    every `.jsonl` it finds.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::claude::{
    event::ChatEvent,
    jsonl_reader::read_jsonl,
    projects_root,
    session::{cancel_streaming, send_tool_result, send_user_message, SessionOpts, SessionsState},
};
/// `claude_list_sessions`' wire type. Lives in the ungated
/// `server::shared::claude_sessions` (WP-19 slice 5b) with the scan, so the
/// daemon arm serializes the same shape.
pub use crate::server::shared::claude_sessions::SessionSummary;
use crate::server::shared::claude_sessions::{list_sessions, locate_jsonl_in_roots};
use crate::server::shared::projects::FsReach;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ClaudeOpts {
    pub prompt: Option<String>,
    #[serde(rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    /// PTY rows. Defaults to 24. Ignored by streaming-chat spawn (no PTY).
    pub rows: Option<u16>,
    /// PTY cols. Defaults to 100. Ignored by streaming-chat spawn.
    pub cols: Option<u16>,
    /// WP-11: plugin folders, set as `CLAUDE_CODE_PLUGIN_DIRS` on the child.
    #[serde(rename = "pluginDirs")]
    pub plugin_dirs: Vec<String>,
    /// WP-11: passed as `--append-system-prompt`.
    #[serde(rename = "appendSystemPrompt")]
    pub append_system_prompt: Option<String>,
    /// WP-11: launch role (`chi` | `pane` | `plan`). Picks the catalog
    /// default model when `model` is unset.
    pub role: Option<String>,
}

#[tauri::command]
pub async fn claude_list_sessions(
    #[allow(non_snake_case)] projectDir: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<SessionSummary>, String> {
    let root = projects_root().ok_or_else(|| "HOME unset".to_string())?;
    // The scan (two-phase: mtime sort, then summarize the newest `limit`) is
    // the shared core the daemon arm also calls; the desktop follows paths as
    // it always has.
    list_sessions(&root, projectDir.as_deref(), limit, FsReach::Follow)
}

#[tauri::command]
pub async fn claude_read_jsonl(
    app: AppHandle,
    #[allow(non_snake_case)] sessionId: String,
) -> Result<Vec<ChatEvent>, String> {
    let path = locate_jsonl_for_session(&app, &sessionId)
        .ok_or_else(|| format!("session {sessionId} not found on disk"))?;
    read_jsonl(&path).map_err(|e| format!("read_jsonl: {e}"))
}

// ─── Session-as-object commands ──────────────────────────────────────────────
//
// `thread_id` is a stable, frontend-minted uuid that identifies a chat thread
// for its entire lifetime. Claude's session id and any PTY id are attributes
// of the `Session`. Events emit on `session://{thread_id}` (single channel).

/// Idempotently create / fetch a session. No process is spawned; the
/// streaming child is lazy and lifts on the first `session_send`. Use this
/// when the UI wants to register a thread (e.g. open an empty chat tab)
/// before any prompt has been typed.
#[tauri::command]
pub async fn session_ensure(
    sessions: State<'_, SessionsState>,
    #[allow(non_snake_case)] threadId: String,
    cwd: String,
    opts: ClaudeOpts,
) -> Result<SessionHandle, String> {
    // Phase 5: translate the legacy free-form `permissionMode` string into
    // the typed `AcpSessionMode`. Unknown / missing values fall back to
    // `Default` (the safest mode); the legacy chat surface never sets
    // anything outside the canonical four, so this is a no-op in practice.
    let permission_mode = opts
        .permission_mode
        .as_deref()
        .and_then(crate::engines::claude_code::mode::AcpSessionMode::from_acp_id)
        .unwrap_or_default();
    let opts = SessionOpts {
        resume_session_id: opts.resume_session_id,
        permission_mode,
        model: opts.model,
        // ADR-011 phase 3: legacy session_ensure path does not yet take
        // effort from the frontend. The composer mutates this post-spawn
        // via `acp_set_effort` instead. Default `Off` matches claude's own.
        effort: Default::default(),
        plugin_dirs: opts.plugin_dirs,
        append_system_prompt: opts.append_system_prompt,
        role: opts.role,
    };
    let session = sessions.get_or_create(&threadId, &cwd, opts).await;
    let claude_session_id = session.claude_session_id.lock().await.clone();
    Ok(SessionHandle {
        thread_id: threadId,
        claude_session_id,
    })
}

/// Send a user message to the thread's streaming child. If no streaming
/// child is live, one is spawned (with `--resume <claude_session_id>` when
/// we already know it, so the conversation continues). The initial prompt
/// is the first stdin envelope of the spawn — single round-trip, no race.
#[tauri::command]
pub async fn session_send(
    app: AppHandle,
    sessions: State<'_, SessionsState>,
    #[allow(non_snake_case)] threadId: String,
    text: String,
) -> Result<(), String> {
    let session = sessions
        .get(&threadId)
        .await
        .ok_or_else(|| format!("no session for thread {threadId}"))?;
    send_user_message(app, session, text).await
}

/// Submit a tool result back to Claude — used by interactive tool
/// renderers like `AskUserQuestion` to ferry the user's answer into the
/// agent loop. `output` is a JSON value (Anthropic accepts plain strings
/// or structured payloads); set `isError: true` to signal failure.
#[tauri::command]
pub async fn session_tool_result(
    sessions: State<'_, SessionsState>,
    #[allow(non_snake_case)] threadId: String,
    #[allow(non_snake_case)] toolUseId: String,
    output: serde_json::Value,
    #[allow(non_snake_case)] isError: Option<bool>,
) -> Result<(), String> {
    let session = sessions
        .get(&threadId)
        .await
        .ok_or_else(|| format!("no session for thread {threadId}"))?;
    send_tool_result(session, toolUseId, output, isError.unwrap_or(false)).await
}

/// Kill the streaming child but leave the in-memory session row so the next
/// `session_send` can re-spawn (with `--resume`). Idempotent.
#[tauri::command]
pub async fn session_cancel(
    sessions: State<'_, SessionsState>,
    #[allow(non_snake_case)] threadId: String,
) -> Result<(), String> {
    let Some(session) = sessions.get(&threadId).await else {
        return Ok(());
    };
    cancel_streaming(session).await
}

/// Tear down the session entirely. Kills any streaming child + removes the
/// in-memory entry. PTYs are owned by `PtyManager` and must be killed via
/// `pty_kill` separately. Idempotent.
#[tauri::command]
pub async fn session_destroy(
    sessions: State<'_, SessionsState>,
    #[allow(non_snake_case)] threadId: String,
) -> Result<(), String> {
    if let Some(session) = sessions.remove(&threadId).await {
        cancel_streaming(session).await?;
    }
    Ok(())
}

/// HMR / page-reload hygiene: kill every streaming child this app owns.
/// Called by the frontend on window 'beforeunload' so dev reloads don't
/// orphan claude processes. PTYs are handled by `PtyManager` separately.
#[tauri::command]
pub async fn session_destroy_all(sessions: State<'_, SessionsState>) -> Result<(), String> {
    sessions.kill_all_streaming().await;
    Ok(())
}

#[derive(Serialize)]
pub struct SessionHandle {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "claudeSessionId")]
    pub claude_session_id: Option<String>,
}

// ─── internals ────────────────────────────────────────────────────────────────

/// Collect every `projects/` root that might hold a session transcript.
///
/// There is exactly one: `$HOME/.claude/projects/`. Every chat thread we spawn
/// writes there, the same as a terminal session, because D-13 retired the
/// per-session `CLAUDE_CONFIG_DIR` overlay (see
/// `plans/2026-07-18-transcripts-and-terminal-architecture/07-retire-the-overlay.md`).
///
/// S-2: this function used to ALSO walk `<app_cache>/sessions/<thread_id>/
/// .claude/projects/` because pre-D-13 threads wrote their transcripts there
/// and those were the only copies. That walk is gone now — the S-3 migration
/// has been applied and verified: 19 of 19 transcripts moved into
/// `$HOME/.claude/projects` and hash-checked. Nothing reachable is lost by
/// dropping the walk.
fn jsonl_projects_roots(_app: &AppHandle) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = projects_root() {
        roots.push(home);
    }
    roots
}

/// Given a session id, find its on-disk jsonl by scanning project slug dirs
/// across `$HOME/.claude/projects` and every surviving pre-D-13 per-session
/// overlay root (see `jsonl_projects_roots`).
///
/// `pub(crate)` so the claude_code engine can use it as a resume-existence
/// guard before seeding `--resume <id>` on a reopened session — a stale id
/// whose transcript is gone would otherwise hard-fail the turn.
pub(crate) fn locate_jsonl_for_session(app: &AppHandle, session_id: &str) -> Option<PathBuf> {
    locate_jsonl_in_roots(&jsonl_projects_roots(app), session_id)
}

// `locate_jsonl_in_roots` (the pure scan behind `locate_jsonl_for_session`)
// lives in `server::shared::claude_sessions` with the daemon's confined read.

#[cfg(test)]
mod jsonl_locator_tests {
    use super::locate_jsonl_in_roots;
    use std::fs;

    #[test]
    fn finds_jsonl_in_an_overlay_root_when_legacy_root_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("home/.claude/projects");
        let overlay = tmp.path().join("cache/sessions/thread-1/.claude/projects");
        let slug = overlay.join("-home-me-proj");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&slug).unwrap();
        let sid = "dc2e0da9-eadd-418f-82a3-8830150f36e0";
        fs::write(slug.join(format!("{sid}.jsonl")), b"{}").unwrap();

        let found = locate_jsonl_in_roots(&[legacy, overlay.clone()], sid);
        assert_eq!(found, Some(slug.join(format!("{sid}.jsonl"))));
    }

    #[test]
    fn legacy_root_wins_when_present_in_both() {
        let tmp = tempfile::tempdir().unwrap();
        let sid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let legacy_slug = tmp.path().join("home/.claude/projects/slug");
        let overlay_slug = tmp.path().join("cache/sessions/t/.claude/projects/slug");
        fs::create_dir_all(&legacy_slug).unwrap();
        fs::create_dir_all(&overlay_slug).unwrap();
        fs::write(legacy_slug.join(format!("{sid}.jsonl")), b"{}").unwrap();
        fs::write(overlay_slug.join(format!("{sid}.jsonl")), b"{}").unwrap();

        let found = locate_jsonl_in_roots(
            &[
                tmp.path().join("home/.claude/projects"),
                tmp.path().join("cache/sessions/t/.claude/projects"),
            ],
            sid,
        );
        assert_eq!(found, Some(legacy_slug.join(format!("{sid}.jsonl"))));
    }

    #[test]
    fn returns_none_when_absent_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("home/.claude/projects");
        fs::create_dir_all(root.join("slug")).unwrap();
        assert_eq!(locate_jsonl_in_roots(&[root], "no-such-session"), None);
    }
}
