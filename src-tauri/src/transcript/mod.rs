//! Transcript JSONL watcher & parser module (WP-02).
//!
//! Watches live session transcripts at `~/.claude/projects/<slug>/<session>.jsonl`,
//! parses records into typed events (`user`, `assistant`, `tool_result`, `progress`, `ai-title`, `summary`),
//! and emits events over `transcript://{session_id}` bus.

// The line parser moved to the ungated `server::shared::claude_sessions`
// (WP-19 slice 5b) — the daemon's session browser arm summarizes with it.
pub use crate::server::shared::claude_sessions::transcript_parser as parser;
pub mod usage;
pub mod watcher;

pub use parser::{parse_line, TranscriptRecord};
pub use usage::{scan_and_mirror_transcripts, UsageSnapshot};
pub use watcher::{read_new_records, watch_transcript_session};
