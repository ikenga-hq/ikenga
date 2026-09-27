//! Reader for on-disk session logs at
//! `~/.claude/projects/<slug>/<uuid>.jsonl`.
//!
//! On-disk envelopes captured 2026-04-30 against Claude Code v2.1.123 are a
//! superset of the live stream-json: in addition to `assistant` / `user` /
//! `result`, the file contains internal types (`attachment`,
//! `queue-operation`, `last-prompt`, `skill_listing`, `deferred_tools_delta`,
//! `command_permissions`) plus pseudo-system info on each envelope (`cwd`,
//! `gitBranch`, `version`). The internal types are dropped by
//! `dispatch_envelope` — they're protocol bookkeeping, not chat content.
//!
//! There is no `system:init` envelope on disk. We synthesize a `SessionInit`
//! event from the first envelope's `sessionId` + `cwd` so consumers see the
//! same "session bootstrap" event they'd see on the live stream.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::event::ChatEvent;
use super::stream_parser::dispatch_envelope;

/// Lightweight summary used by `claude_list_sessions`. Cheap to build —
/// reads only what's needed.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub session_id: String,
    pub project_dir: String,
    pub started_at: String,
    pub last_message_at: Option<String>,
    pub message_count: u64,
    pub title: Option<String>,
    pub model: Option<String>,
}

/// Read an entire session file into ChatEvents.
///
/// Implementation: parse lines into `Value`s once, then do a tiny first-pass
/// to collect `sessionId` / `cwd` / `model` from any envelope (the first one
/// is often a `queue-operation` that lacks `cwd`). Emit a synthesized
/// `SessionInit`, then dispatch each envelope in order.
pub fn read_jsonl(path: &Path) -> Result<Vec<ChatEvent>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut parsed: Vec<Result<Value, ChatEvent>> = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                parsed.push(Err(ChatEvent::ParseError {
                    message: format!("line {} io: {e}", lineno + 1),
                    line: String::new(),
                }));
                continue;
            }
        };
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(v) => parsed.push(Ok(v)),
            Err(e) => parsed.push(Err(ChatEvent::ParseError {
                message: format!("json: {e}"),
                line,
            })),
        }
    }

    // Pre-scan: pull session-level metadata from whichever envelope has it.
    let mut session_id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut model: Option<String> = None;
    let mut permission_mode: Option<String> = None;
    for entry in &parsed {
        let v = match entry {
            Ok(v) => v,
            Err(_) => continue,
        };
        if session_id.is_none() {
            session_id = v
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if model.is_none() {
            model = v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if permission_mode.is_none() {
            permission_mode = v
                .get("permissionMode")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if session_id.is_some() && cwd.is_some() && model.is_some() {
            break;
        }
    }

    let mut events: Vec<ChatEvent> = Vec::new();
    if let Some(id) = session_id {
        events.push(ChatEvent::SessionInit {
            session_id: id,
            model,
            cwd,
            permission_mode,
            // No system:init envelope on disk; version is irrelevant for replay
            // (we never send outbound control_requests to a transcript).
            claude_code_version: None,
        });
    }

    for entry in parsed {
        match entry {
            Ok(v) => dispatch_envelope(&v, &mut events),
            Err(e) => events.push(e),
        }
    }

    Ok(events)
}

/// Cheap summary scan — first 16 envelopes + last envelope. Keeps list view
/// snappy when ~/.claude/projects/ has thousands of sessions.
pub fn summarize(path: &Path) -> Result<Option<SessionSummary>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut session_id: Option<String> = None;
    let mut project_dir: Option<String> = None;
    let mut model: Option<String> = None;
    let mut started_at: Option<String> = None;
    let mut last_ts: Option<String> = None;
    let mut message_count: u64 = 0;
    let mut title: Option<String> = None;

    for line in reader.lines().map_while(Result::ok) {
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str::<Value>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if session_id.is_none() {
            session_id = value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if project_dir.is_none() {
            project_dir = value.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if started_at.is_none() {
            started_at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if let Some(ts) = value.get("timestamp").and_then(Value::as_str) {
            last_ts = Some(ts.to_string());
        }
        if model.is_none() {
            if let Some(m) = value
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
            {
                model = Some(m.to_string());
            }
        }

        let envelope_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        match envelope_type {
            "assistant" | "user" => {
                message_count += 1;
                if title.is_none() && envelope_type == "user" {
                    title = first_user_text(&value).map(|t| truncate_title(&t));
                }
            }
            _ => {}
        }
    }

    let session_id = match session_id {
        Some(id) => id,
        None => return Ok(None),
    };
    let project_dir = project_dir.unwrap_or_else(|| {
        // Fall back to the slug-decoded directory name.
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(super::slug_to_project_dir)
            .unwrap_or_default()
    });
    let started_at = started_at.unwrap_or_else(|| {
        // Fall back to file mtime so the row at least sorts correctly.
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                chrono_iso8601_secs(d.as_secs())
                    .unwrap_or_else(|| String::from("1970-01-01T00:00:00Z"))
            })
            .unwrap_or_default()
    });

    Ok(Some(SessionSummary {
        session_id,
        project_dir,
        started_at,
        last_message_at: last_ts,
        message_count,
        title,
        model,
    }))
}

fn first_user_text(value: &Value) -> Option<String> {
    let content = value.get("message").and_then(|m| m.get("content"))?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn truncate_title(s: &str) -> String {
    // Strip slash-command framing if present, otherwise take the first line.
    let cleaned = s
        .lines()
        .find(|line| !line.trim().is_empty() && !line.trim().starts_with('<'))
        .unwrap_or("")
        .trim();
    let mut out: String = cleaned.chars().take(120).collect();
    if cleaned.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Tiny ISO-8601 formatter for seconds since epoch — avoids pulling chrono
/// in just for fallback timestamp rendering.
fn chrono_iso8601_secs(secs: u64) -> Option<String> {
    // Days in each month for non-leap years.
    const DAYS_BY_MONTH: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut days = (secs / 86_400) as i64;
    let mut rem = secs % 86_400;
    let hour = rem / 3600;
    rem %= 3600;
    let minute = rem / 60;
    let second = rem % 60;

    let mut year = 1970i64;
    loop {
        let leap = is_leap(year);
        let yd = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        year += 1;
    }
    let mut month = 0usize;
    let mut day = days as u32;
    while month < 12 {
        let mut dim = DAYS_BY_MONTH[month];
        if month == 1 && is_leap(year) {
            dim = 29;
        }
        if day < dim {
            break;
        }
        day -= dim;
        month += 1;
    }
    Some(format!(
        "{year:04}-{:02}-{:02}T{hour:02}:{minute:02}:{second:02}Z",
        month + 1,
        day + 1
    ))
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_jsonl(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        f
    }

    #[test]
    fn synthesizes_session_init_from_first_envelope() {
        let f = write_jsonl(&[
            r#"{"type":"user","sessionId":"abc","cwd":"/x","timestamp":"2026-04-30T12:00:00Z","message":{"role":"user","content":"hi"}}"#,
        ]);
        let events = read_jsonl(f.path()).unwrap();
        assert!(matches!(
            events[0],
            ChatEvent::SessionInit { ref session_id, .. } if session_id == "abc"
        ));
    }

    #[test]
    fn parses_assistant_blocks_from_disk() {
        let f = write_jsonl(&[
            r#"{"type":"user","sessionId":"abc","cwd":"/x","timestamp":"2026-04-30T12:00:00Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","sessionId":"abc","message":{"role":"assistant","content":[{"type":"text","text":"hi back"}]}}"#,
        ]);
        let events = read_jsonl(f.path()).unwrap();
        let texts: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                ChatEvent::Text { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["hi back"]);
    }

    #[test]
    fn summarize_extracts_basics() {
        let f = write_jsonl(&[
            r#"{"type":"user","sessionId":"abc","cwd":"/x","timestamp":"2026-04-30T12:00:00Z","message":{"role":"user","content":"hello world"}}"#,
            r#"{"type":"assistant","sessionId":"abc","timestamp":"2026-04-30T12:00:05Z","message":{"role":"assistant","model":"claude-sonnet-4-6","content":[{"type":"text","text":"hi"}]}}"#,
        ]);
        let s = summarize(f.path()).unwrap().unwrap();
        assert_eq!(s.session_id, "abc");
        assert_eq!(s.project_dir, "/x");
        assert_eq!(s.message_count, 2);
        assert_eq!(s.title.as_deref(), Some("hello world"));
        assert_eq!(s.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(s.last_message_at.as_deref(), Some("2026-04-30T12:00:05Z"));
    }

    #[test]
    fn drops_internal_control_envelopes() {
        let f = write_jsonl(&[
            r#"{"type":"user","sessionId":"abc","cwd":"/x","timestamp":"2026-04-30T12:00:00Z","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"queue-operation","sessionId":"abc","operation":"enqueue","timestamp":"2026-04-30T12:00:01Z"}"#,
            r#"{"type":"attachment","sessionId":"abc","attachment":{"command":"hook","content":"x"}}"#,
            r#"{"type":"last-prompt","sessionId":"abc","lastPrompt":"hi"}"#,
            r#"{"type":"skill_listing","sessionId":"abc"}"#,
            r#"{"type":"deferred_tools_delta","sessionId":"abc"}"#,
            r#"{"type":"command_permissions","sessionId":"abc"}"#,
            r#"{"type":"assistant","sessionId":"abc","message":{"role":"assistant","content":[{"type":"text","text":"hi back"}]}}"#,
        ]);
        let events = read_jsonl(f.path()).unwrap();
        // SessionInit (synthesized) + Text "hi back". No Unknowns.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ChatEvent::Unknown { .. })),
            "control envelopes leaked as Unknown: {:?}",
            events
        );
    }

    #[test]
    fn truncate_title_strips_command_framing() {
        let with_framing =
            "<command-message>foo</command-message>\n<command-name>/bar</command-name>";
        assert_eq!(truncate_title(with_framing), "");
        let normal = "Help me debug this Rust panic";
        assert_eq!(truncate_title(normal), "Help me debug this Rust panic");
        let long = "x".repeat(200);
        let t = truncate_title(&long);
        assert!(t.ends_with('…'));
    }
}
