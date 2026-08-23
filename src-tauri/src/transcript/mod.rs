//! Transcript JSONL watcher & parser module (WP-02).
//!
//! Watches live session transcripts at `~/.claude/projects/<slug>/<session>.jsonl`,
//! parses records into typed events (`user`, `assistant`, `tool_result`, `progress`, `ai-title`, `summary`),
//! and emits events over `transcript://{session_id}` bus.

pub mod parser;
pub mod watcher;

pub use parser::{parse_line, TranscriptRecord};
pub use watcher::{read_new_records, watch_transcript_session};
