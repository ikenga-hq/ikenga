//! Session browser and session enumeration engine (WP-04).
//!
//! Scans `~/.claude/projects/` across projects or for a specific project slug,
//! extracts metadata (title, model, record count, timestamp), and provides the
//! backend feed for the session browser pane and resume affordances.
//!
//! The scan itself lives in the ungated `server::shared::claude_sessions`
//! (WP-19 slice 5b) so the daemon's `claude_session_list` arm runs the same
//! code; this module keeps the `#[tauri::command]` and the desktop root.

use std::path::PathBuf;

use crate::server::shared::claude_sessions::enumerate_sessions;
pub use crate::server::shared::claude_sessions::{summarize_session_file, ClaudeSessionSummary};
use crate::server::shared::projects::FsReach;

/// Enumerates all sessions across projects or within a specified project slug directory.
pub fn enumerate_claude_sessions(project_slug_filter: Option<&str>) -> Vec<ClaudeSessionSummary> {
    let projects_dir = PathBuf::from(shellexpand::tilde("~/.claude/projects").to_string());
    enumerate_sessions(&projects_dir, project_slug_filter, FsReach::Follow)
}

/// Tauri command to list all Claude sessions.
#[tauri::command]
pub fn claude_session_list(project_slug: Option<String>) -> Vec<ClaudeSessionSummary> {
    enumerate_claude_sessions(project_slug.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_summarize_session_file() {
        let tmp = TempDir::new().expect("tempdir");
        let session_file = tmp.path().join("sess-99.jsonl");

        let jsonl_content = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-23T10:00:05Z","message":{"model":"claude-3-5-sonnet","role":"assistant","content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Greeting Session"}"#,
            "\n"
        );

        fs::write(&session_file, jsonl_content).expect("write session file");

        let summary = summarize_session_file(&session_file, "my-project").expect("summary");
        assert_eq!(summary.session_id, "sess-99");
        assert_eq!(summary.project_slug, "my-project");
        assert_eq!(summary.title.as_deref(), Some("Greeting Session"));
        assert_eq!(summary.last_model.as_deref(), Some("claude-3-5-sonnet"));
        assert_eq!(summary.record_count, 3);
    }
}
