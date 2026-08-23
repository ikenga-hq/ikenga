//! Session browser and session enumeration engine (WP-04).
//!
//! Scans `~/.claude/projects/` across projects or for a specific project slug,
//! extracts metadata (title, model, record count, timestamp), and provides the
//! backend feed for the session browser pane and resume affordances.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::transcript::parser::{parse_line, TranscriptRecord};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeSessionSummary {
    pub session_id: String,
    pub project_slug: String,
    pub transcript_path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    pub record_count: usize,
    #[serde(default)]
    pub last_model: Option<String>,
}

/// Scans a single transcript `.jsonl` file to extract session summary metadata.
pub fn summarize_session_file(path: &Path, project_slug: &str) -> Option<ClaudeSessionSummary> {
    if !path.is_file() {
        return None;
    }

    let file_stem = path.file_stem()?.to_string_lossy().to_string();
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut record_count = 0usize;
    let mut title: Option<String> = None;
    let mut last_model: Option<String> = None;
    let mut updated_at: Option<String> = None;

    for line in reader.lines().flatten() {
        if line.trim().is_empty() {
            continue;
        }
        record_count += 1;

        if let Some(record) = parse_line(&line) {
            match record {
                TranscriptRecord::AiTitle { ai_title, .. } => {
                    if ai_title.is_some() {
                        title = ai_title;
                    }
                }
                TranscriptRecord::Assistant { message, timestamp, .. } => {
                    if let Some(ts) = timestamp {
                        updated_at = Some(ts);
                    }
                    if let Some(msg) = message {
                        if msg.model.is_some() {
                            last_model = msg.model;
                        }
                    }
                }
                TranscriptRecord::User { timestamp, .. } => {
                    if let Some(ts) = timestamp {
                        updated_at = Some(ts);
                    }
                }
                _ => {}
            }
        }
    }

    // Fallback updated_at to file modified time if absent
    if updated_at.is_none() {
        if let Ok(meta) = fs::metadata(path) {
            if let Ok(mtime) = meta.modified() {
                if let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    updated_at = Some(format!("{}", dur.as_secs()));
                }
            }
        }
    }

    Some(ClaudeSessionSummary {
        session_id: file_stem,
        project_slug: project_slug.to_string(),
        transcript_path: path.to_string_lossy().to_string(),
        title,
        updated_at,
        record_count,
        last_model,
    })
}

/// Enumerates all sessions across projects or within a specified project slug directory.
pub fn enumerate_claude_sessions(project_slug_filter: Option<&str>) -> Vec<ClaudeSessionSummary> {
    let mut summaries = Vec::new();
    let projects_dir = match shellexpand::tilde("~/.claude/projects").to_string() {
        s => PathBuf::from(s),
    };

    if !projects_dir.is_dir() {
        return summaries;
    }

    let entries = match fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return summaries,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let slug = path.file_name().unwrap_or_default().to_string_lossy().to_string();

        if let Some(filter) = project_slug_filter {
            if slug != filter {
                continue;
            }
        }

        if let Ok(files) = fs::read_dir(&path) {
            for f_entry in files.flatten() {
                let f_path = f_entry.path();
                if f_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if let Some(summary) = summarize_session_file(&f_path, &slug) {
                        summaries.push(summary);
                    }
                }
            }
        }
    }

    // Sort by updated_at descending
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    summaries
}

/// Tauri command to list all Claude sessions.
#[tauri::command]
pub fn claude_session_list(project_slug: Option<String>) -> Vec<ClaudeSessionSummary> {
    enumerate_claude_sessions(project_slug.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_summarize_session_file() {
        let tmp = TempDir::new().expect("tempdir");
        let session_file = tmp.path().join("sess-99.jsonl");

        let jsonl_content = concat!(
            r#"{"type":"user","uuid":"u1","timestamp":"2026-08-23T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#, "\n",
            r#"{"type":"assistant","uuid":"a1","timestamp":"2026-08-23T10:00:05Z","message":{"model":"claude-3-5-sonnet","role":"assistant","content":[{"type":"text","text":"hi"}]}}"#, "\n",
            r#"{"type":"ai-title","aiTitle":"Greeting Session"}"#, "\n"
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
