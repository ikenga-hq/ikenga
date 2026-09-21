//! Transcript JSONL usage mirror (WP-14 / DEC-24, reworked by WP-14a / DEC-27).
//!
//! Walks `~/.claude/projects/**.jsonl`, extracts per-item usage facts from
//! assistant records, and mirrors them into SQLite (migrations 0064, 0065) so the
//! aggregates outlive Claude Code's own 30-day eviction of the source files.
//!
//! ## What a "use" is (DEC-27)
//!
//! `count_7d` / `count_30d` count **distinct sessions**, uniformly for every
//! kind. A session key is a main transcript's `sessionId`, or — inside a
//! subagent transcript (`…/subagents/agent-<agentId>.jsonl`) — that run's
//! `agentId`.
//!
//! - **skill**: the session has an assistant turn whose `attributionSkill`
//!   names it, or a `Skill` tool-use whose `input.skill` names it.
//! - **agent**: one subagent run of that type. The type comes from the run's
//!   `agent-<id>.meta.json` (`agentType`), falling back to the records'
//!   `attributionAgent`. The parent's `Agent`/`Task` tool-use is deliberately
//!   *not* counted: it carries no `agentId`, so counting it too would count
//!   every run twice under two different keys.
//! - **MCP server**: the session called any `mcp__<server>__<tool>`.
//!
//! `tokens_30d` is the sum over the assistant turns attributed to the item
//! (input + output + cache-creation + cache-read). Claude Code writes one
//! record per content block and repeats the message's `usage` on each, so
//! turns are keyed by message id and upserted with `MAX()`.
//!
//! ## Resumed and forked sessions (DEC-29, migration 0065)
//!
//! Resuming a session copies its history into a new file under a new
//! `sessionId`; the copied assistant records keep their `message.id` and
//! timestamps. Every attribution this module counts sits on an `assistant`
//! record carrying a `message.id` (the fallback keys, `requestId` and `uuid`,
//! are copied unchanged too), so a message id identifies a copy.
//!
//! A session counts an item only through messages it **owns**. Of the sessions
//! holding a message id, the owner is the one with the earliest start — the
//! earliest record timestamp its files show — ties broken by the smallest
//! session key. Ownership depends on every file, and files are scanned
//! incrementally in no fixed order, so the scan stores *holdings*
//! (`ngwa_usage_messages`) and *starts* (`ngwa_usage_session_starts`) and
//! [`resolve_owned_sessions`] decides ownership when the mirror is loaded. The
//! result therefore does not depend on scan order. Token accounting is
//! per-message already and is unchanged.
//!
//! ## Incremental scan (F-4 / F-8)
//!
//! Per file, `ngwa_transcript_files` stores the **post-read** byte position of
//! the last complete line consumed, plus a hash of the file's first
//! `min(4 KiB, offset)` bytes. A trailing line with no newline is never
//! consumed (the writer may be mid-line). A read error stops at the last good
//! line. If the file shrank or its head hash changed (truncation or in-place
//! rewrite), every row it contributed is deleted and it is rescanned from 0.
//! Every write is an upsert keyed on a natural key, so re-reading a line can
//! never double-count.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Bytes of a file's head hashed to detect an in-place rewrite.
pub const HEAD_HASH_LEN: u64 = 4096;
const DAY_MS: i64 = 86_400_000;

// ── Facts ───────────────────────────────────────────────────────────────────

/// What a usage row is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UsageKind {
    Skill,
    Agent,
    McpServer,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageKind::Skill => "skill",
            UsageKind::Agent => "agent",
            UsageKind::McpServer => "mcp_server",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(UsageKind::Skill),
            "agent" => Some(UsageKind::Agent),
            "mcp_server" => Some(UsageKind::McpServer),
            _ => None,
        }
    }
}

/// One attribution extracted from one assistant record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageFact {
    pub kind: UsageKind,
    pub name: String,
    pub session_key: String,
    pub message_id: String,
    pub timestamp_ms: i64,
    /// Tokens of the turn attributed to this item (0 for a bare `Skill`
    /// invocation, which names the skill but is not a turn run under it).
    pub tokens: i64,
}

/// Per-file context: a subagent transcript keys its session by `agentId`.
#[derive(Debug, Clone, Default)]
pub struct FileContext {
    pub subagent: Option<SubagentContext>,
}

#[derive(Debug, Clone)]
pub struct SubagentContext {
    pub agent_id: String,
    pub agent_type: Option<String>,
}

/// Classify a transcript path. A subagent run is `agent-<id>.jsonl` under a
/// `subagents` directory; its type is read from the sibling `.meta.json`.
pub fn file_context(path: &Path) -> FileContext {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return FileContext::default();
    };
    let under_subagents = path
        .ancestors()
        .skip(1)
        .any(|a| a.file_name().and_then(|n| n.to_str()) == Some("subagents"));
    let agent_id = file_name
        .strip_prefix("agent-")
        .and_then(|s| s.strip_suffix(".jsonl"));
    match (under_subagents, agent_id) {
        (true, Some(id)) if !id.is_empty() => FileContext {
            subagent: Some(SubagentContext {
                agent_id: id.to_string(),
                agent_type: read_subagent_meta_type(path),
            }),
        },
        _ => FileContext::default(),
    }
}

fn read_subagent_meta_type(jsonl_path: &Path) -> Option<String> {
    let file_name = jsonl_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".jsonl")?;
    let meta_path = jsonl_path.with_file_name(format!("{stem}.meta.json"));
    let raw = std::fs::read_to_string(meta_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("agentType")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Parse an RFC 3339 timestamp to unix millis. `None` drops the event (F-12):
/// an undatable event is never dated `now()`.
pub fn parse_timestamp_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts.trim())
        .ok()
        .map(|d| d.timestamp_millis())
}

/// `mcp__<server>__<tool>` → `<server>`.
pub fn mcp_server_of(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        None
    } else {
        Some(server)
    }
}

/// Claude Code builds tool names from the configured server key with every
/// character outside `[A-Za-z0-9_-]` replaced by `_` (`Claude Browser` →
/// `mcp__Claude_Browser__…`). Apply the same to a config key before lookup.
pub fn normalize_mcp_server_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn non_empty(v: Option<&serde_json::Value>) -> Option<&str> {
    v.and_then(|s| s.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// What one JSONL line contributes: the session it dates (for the session's
/// start, DEC-29) and the usage facts it carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordFacts {
    /// `(session_key, timestamp_ms)` for any record, of any type, with a
    /// parseable timestamp and a session key.
    pub dated: Option<(String, i64)>,
    pub facts: Vec<UsageFact>,
}

/// Extract the usage facts one JSONL line carries. Non-assistant records,
/// malformed JSON, and records without a parseable timestamp, session key or
/// message id yield nothing.
pub fn extract_facts(line: &str, ctx: &FileContext) -> Vec<UsageFact> {
    extract_record(line, ctx).facts
}

/// Parse one JSONL line once, returning both its date (every record type) and
/// its usage facts (assistant records only).
pub fn extract_record(line: &str, ctx: &FileContext) -> RecordFacts {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return RecordFacts::default();
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return RecordFacts::default();
    };
    let Some(timestamp_ms) = v
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(parse_timestamp_ms)
    else {
        return RecordFacts::default();
    };
    let session_key = match &ctx.subagent {
        Some(s) => s.agent_id.clone(),
        None => match non_empty(v.get("sessionId")) {
            Some(s) => s.to_string(),
            None => return RecordFacts::default(),
        },
    };
    let dated = Some((session_key.clone(), timestamp_ms));
    if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
        return RecordFacts { dated, facts: Vec::new() };
    }
    RecordFacts {
        dated,
        facts: assistant_facts(&v, ctx, session_key, timestamp_ms),
    }
}

fn assistant_facts(
    v: &serde_json::Value,
    ctx: &FileContext,
    session_key: String,
    timestamp_ms: i64,
) -> Vec<UsageFact> {
    let msg = v.get("message");
    let Some(message_id) = non_empty(msg.and_then(|m| m.get("id")))
        .or_else(|| non_empty(v.get("requestId")))
        .or_else(|| non_empty(v.get("uuid")))
        .map(str::to_string)
    else {
        return Vec::new();
    };

    let usage = msg.and_then(|m| m.get("usage"));
    let tok = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };
    let tokens = tok("input_tokens")
        + tok("output_tokens")
        + tok("cache_creation_input_tokens")
        + tok("cache_read_input_tokens");

    // (kind, name) → tokens, deduplicated within the record.
    let mut found: Vec<(UsageKind, String, i64)> = Vec::new();
    let mut add = |kind: UsageKind, name: &str, t: i64| {
        if let Some(e) = found.iter_mut().find(|(k, n, _)| *k == kind && n == name) {
            e.2 = e.2.max(t);
        } else {
            found.push((kind, name.to_string(), t));
        }
    };

    if let Some(skill) = non_empty(v.get("attributionSkill")) {
        add(UsageKind::Skill, skill, tokens);
    }
    if let Some(sub) = &ctx.subagent {
        let agent_type = sub
            .agent_type
            .as_deref()
            .or_else(|| non_empty(v.get("attributionAgent")));
        if let Some(t) = agent_type {
            add(UsageKind::Agent, t, tokens);
        }
    }
    if let Some(blocks) = msg
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let Some(name) = block.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            if name == "Skill" {
                if let Some(skill) = non_empty(block.get("input").and_then(|i| i.get("skill"))) {
                    add(UsageKind::Skill, skill, 0);
                }
            } else if let Some(server) = mcp_server_of(name) {
                add(UsageKind::McpServer, server, tokens);
            }
        }
    }

    found
        .into_iter()
        .map(|(kind, name, tokens)| UsageFact {
            kind,
            name,
            session_key: session_key.clone(),
            message_id: message_id.clone(),
            timestamp_ms,
            tokens,
        })
        .collect()
}

// ── Per-file scan (blocking) ────────────────────────────────────────────────

/// Stored per-file watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watermark {
    pub mtime_ms: i64,
    pub byte_offset: u64,
    pub head_len: u64,
    pub head_hash: String,
}

/// One attributed message a session holds (DEC-29): whether it *counts* for
/// the session is decided at aggregation time, by [`resolve_owned_sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub kind: UsageKind,
    pub name: String,
    pub session_key: String,
    pub message_id: String,
    /// Latest timestamp among the message's records.
    pub timestamp_ms: i64,
}

/// The earliest record timestamp one file read shows for a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStart {
    pub session_key: String,
    pub start_ms: i64,
}

/// Aggregated turn row produced by one file read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub kind: UsageKind,
    pub name: String,
    pub message_id: String,
    pub session_key: String,
    pub timestamp_ms: i64,
    pub tokens: i64,
}

/// Result of reading one changed file. Committed in one transaction.
#[derive(Debug, Clone)]
pub struct FileScan {
    pub path: String,
    pub mtime_ms: i64,
    /// Post-read position: the end of the last complete line consumed.
    pub byte_offset: u64,
    pub head_len: u64,
    pub head_hash: String,
    /// The file shrank or was rewritten: delete its old contributions first.
    pub reset: bool,
    pub messages: Vec<MessageRow>,
    pub starts: Vec<SessionStart>,
    pub turns: Vec<TurnRow>,
    /// A read error stopped the scan early; the watermark stays at the last
    /// good line so the rest is retried next time.
    pub read_error: Option<String>,
    /// False when the read made no progress and invalidated nothing: there is
    /// nothing to write, and the stored watermark must stay as it is.
    pub commit: bool,
}

fn file_mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// SHA-256 (hex) of the first `len` bytes of `path`.
pub fn hash_head(path: &Path, len: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut buf = Vec::with_capacity(len as usize);
    (&mut f).take(len).read_to_end(&mut buf)?;
    if (buf.len() as u64) < len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "file shorter than hashed head",
        ));
    }
    Ok(hex::encode(Sha256::digest(&buf)))
}

/// Read whatever is new in `path` since `wm`. `None` = unchanged (or vanished).
pub fn scan_file(path: &Path, wm: Option<&Watermark>) -> Option<FileScan> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    let mtime_ms = file_mtime_ms(&meta);
    let path_str = path.to_string_lossy().to_string();

    if let Some(w) = wm {
        if w.mtime_ms == mtime_ms && w.byte_offset == size {
            return None;
        }
    }

    let mut reset = false;
    let mut start = 0u64;
    if let Some(w) = wm {
        if size < w.byte_offset {
            reset = true;
        } else {
            match hash_head(path, w.head_len) {
                Ok(h) if h == w.head_hash => start = w.byte_offset,
                Ok(_) => reset = true,
                Err(e) => {
                    log::warn!("[ngwa usage] head hash of {path_str} failed: {e}");
                    return None;
                }
            }
        }
    }

    let ctx = file_context(path);
    let mut offset = start;
    let mut read_error: Option<String> = None;
    let mut messages: HashMap<(UsageKind, String, String, String), i64> = HashMap::new();
    let mut starts: HashMap<String, i64> = HashMap::new();
    let mut turns: HashMap<(UsageKind, String, String), (String, i64, i64)> = HashMap::new();

    let open = File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(start))?;
        Ok(f)
    });
    match open {
        Err(e) => read_error = Some(format!("{path_str}: {e}")),
        Ok(f) => {
            let mut reader = BufReader::new(f);
            let mut buf: Vec<u8> = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.last() != Some(&b'\n') {
                            // Partial trailing line: leave it for the next scan.
                            break;
                        }
                        offset += n as u64;
                        let line = String::from_utf8_lossy(&buf);
                        let rec = extract_record(&line, &ctx);
                        if let Some((sk, ts)) = rec.dated {
                            let s = starts.entry(sk).or_insert(ts);
                            *s = (*s).min(ts);
                        }
                        for f in rec.facts {
                            let m = messages
                                .entry((
                                    f.kind,
                                    f.name.clone(),
                                    f.session_key.clone(),
                                    f.message_id.clone(),
                                ))
                                .or_insert(f.timestamp_ms);
                            *m = (*m).max(f.timestamp_ms);
                            let t = turns
                                .entry((f.kind, f.name, f.message_id))
                                .or_insert((f.session_key, f.timestamp_ms, f.tokens));
                            t.1 = t.1.max(f.timestamp_ms);
                            t.2 = t.2.max(f.tokens);
                        }
                    }
                    Err(e) => {
                        // F-8: stop at the last good line; never advance past it.
                        read_error = Some(format!("{path_str}: {e}"));
                        break;
                    }
                }
            }
        }
    }

    if read_error.is_some() && offset == start && !reset {
        // No progress and nothing to invalidate: leave the watermark alone.
        return Some(FileScan {
            path: path_str,
            mtime_ms,
            byte_offset: start,
            head_len: wm.map(|w| w.head_len).unwrap_or(0),
            head_hash: wm.map(|w| w.head_hash.clone()).unwrap_or_default(),
            reset: false,
            messages: Vec::new(),
            starts: Vec::new(),
            turns: Vec::new(),
            read_error,
            commit: false,
        });
    }

    let head_len = offset.min(HEAD_HASH_LEN);
    let head_hash = match hash_head(path, head_len) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("[ngwa usage] head hash of {path_str} failed: {e}");
            return None;
        }
    };

    let mut messages: Vec<MessageRow> = messages
        .into_iter()
        .map(|((kind, name, session_key, message_id), ts)| MessageRow {
            kind,
            name,
            session_key,
            message_id,
            timestamp_ms: ts,
        })
        .collect();
    messages.sort_by(|a, b| {
        (a.kind, &a.name, &a.session_key, &a.message_id)
            .cmp(&(b.kind, &b.name, &b.session_key, &b.message_id))
    });
    let mut starts: Vec<SessionStart> = starts
        .into_iter()
        .map(|(session_key, start_ms)| SessionStart { session_key, start_ms })
        .collect();
    starts.sort_by(|a, b| a.session_key.cmp(&b.session_key));
    let mut turns: Vec<TurnRow> = turns
        .into_iter()
        .map(|((kind, name, message_id), (session_key, ts, tokens))| TurnRow {
            kind,
            name,
            message_id,
            session_key,
            timestamp_ms: ts,
            tokens,
        })
        .collect();
    turns.sort_by(|a, b| (a.kind, &a.name, &a.message_id).cmp(&(b.kind, &b.name, &b.message_id)));

    Some(FileScan {
        path: path_str,
        mtime_ms,
        byte_offset: offset,
        head_len,
        head_hash,
        reset,
        messages,
        starts,
        turns,
        read_error,
        commit: true,
    })
}

// ── Corpus walk + commit ────────────────────────────────────────────────────

/// Resolve the default Claude Code projects directory (`~/.claude/projects`).
pub fn claude_projects_dir() -> Option<PathBuf> {
    crate::platform::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Walk all `.jsonl` files under a root directory recursively, sorted.
pub fn discover_transcript_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_dir_recursive(root, &mut files);
    files.sort();
    files
}

fn walk_dir_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_dir_recursive(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// Summary of one incremental scan.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub files_seen: usize,
    pub files_read: usize,
    pub files_reset: usize,
    pub message_rows: usize,
    pub start_rows: usize,
    pub turn_rows: usize,
    pub read_errors: Vec<String>,
}

impl ScanReport {
    /// A non-fatal note for `sources.usage.error`, when some files were unreadable.
    pub fn error_summary(&self) -> Option<String> {
        if self.read_errors.is_empty() {
            None
        } else {
            Some(format!(
                "{} transcript file(s) could not be fully read; first: {}",
                self.read_errors.len(),
                self.read_errors[0]
            ))
        }
    }
}

async fn load_watermarks(pool: &SqlitePool) -> Result<HashMap<String, Watermark>, String> {
    let rows: Vec<(String, i64, i64, i64, String)> = sqlx::query_as(
        "SELECT path, mtime_ms, byte_offset, head_len, head_hash FROM ngwa_transcript_files",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("read transcript watermarks: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|(p, mtime_ms, off, head_len, head_hash)| {
            (
                p,
                Watermark {
                    mtime_ms,
                    byte_offset: off.max(0) as u64,
                    head_len: head_len.max(0) as u64,
                    head_hash,
                },
            )
        })
        .collect())
}

/// Commit one file's scan in a single transaction (F-10).
pub async fn commit_file_scan(pool: &SqlitePool, scan: &FileScan, now: i64) -> Result<(), String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    if scan.reset {
        for table in ["ngwa_usage_messages", "ngwa_usage_session_starts"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE source_path = ?"))
                .bind(&scan.path)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        sqlx::query("DELETE FROM ngwa_usage_turns WHERE source_path = ?")
            .bind(&scan.path)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    for m in &scan.messages {
        sqlx::query(
            r#"INSERT INTO ngwa_usage_messages
                 (kind, name, session_key, message_id, timestamp_ms, source_path)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT(kind, name, session_key, message_id) DO UPDATE SET
                 timestamp_ms = MAX(timestamp_ms, excluded.timestamp_ms)"#,
        )
        .bind(m.kind.as_str())
        .bind(&m.name)
        .bind(&m.session_key)
        .bind(&m.message_id)
        .bind(m.timestamp_ms)
        .bind(&scan.path)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    for st in &scan.starts {
        sqlx::query(
            r#"INSERT INTO ngwa_usage_session_starts (session_key, source_path, start_ms)
               VALUES (?, ?, ?)
               ON CONFLICT(session_key, source_path) DO UPDATE SET
                 start_ms = MIN(start_ms, excluded.start_ms)"#,
        )
        .bind(&st.session_key)
        .bind(&scan.path)
        .bind(st.start_ms)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    for t in &scan.turns {
        sqlx::query(
            r#"INSERT INTO ngwa_usage_turns
                 (kind, name, message_id, session_key, timestamp_ms, tokens, source_path)
               VALUES (?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(kind, name, message_id) DO UPDATE SET
                 timestamp_ms = MAX(timestamp_ms, excluded.timestamp_ms),
                 tokens       = MAX(tokens, excluded.tokens)"#,
        )
        .bind(t.kind.as_str())
        .bind(&t.name)
        .bind(&t.message_id)
        .bind(&t.session_key)
        .bind(t.timestamp_ms)
        .bind(t.tokens)
        .bind(&scan.path)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    sqlx::query(
        r#"INSERT INTO ngwa_transcript_files
             (path, mtime_ms, byte_offset, head_len, head_hash, scanned_at_ms)
           VALUES (?, ?, ?, ?, ?, ?)
           ON CONFLICT(path) DO UPDATE SET
             mtime_ms = excluded.mtime_ms,
             byte_offset = excluded.byte_offset,
             head_len = excluded.head_len,
             head_hash = excluded.head_hash,
             scanned_at_ms = excluded.scanned_at_ms"#,
    )
    .bind(&scan.path)
    .bind(scan.mtime_ms)
    .bind(scan.byte_offset as i64)
    .bind(scan.head_len as i64)
    .bind(&scan.head_hash)
    .bind(now)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())
}

/// Run an incremental scan of `root` and mirror it into SQLite. File I/O and
/// parsing run under `spawn_blocking` (F-10). `Err` means the scan failed as a
/// whole (absent corpus, unreadable watermarks, a failed write): the caller
/// must then report usage as unmeasured, never as zero.
pub async fn scan_and_mirror_transcripts(
    pool: &SqlitePool,
    root: &Path,
) -> Result<ScanReport, String> {
    if !root.is_dir() {
        return Err(format!("transcript corpus not found at {}", root.display()));
    }
    let watermarks = load_watermarks(pool).await?;
    let root_owned = root.to_path_buf();
    let (files_seen, scans) = tokio::task::spawn_blocking(move || {
        let files = discover_transcript_files(&root_owned);
        let seen = files.len();
        let scans: Vec<FileScan> = files
            .iter()
            .filter_map(|p| scan_file(p, watermarks.get(p.to_string_lossy().as_ref())))
            .collect();
        (seen, scans)
    })
    .await
    .map_err(|e| format!("transcript scan task failed: {e}"))?;

    let now = now_ms();
    let mut report = ScanReport {
        files_seen,
        ..ScanReport::default()
    };
    for scan in &scans {
        if let Some(e) = &scan.read_error {
            log::warn!("[ngwa usage] {e}");
            report.read_errors.push(e.clone());
        }
        if !scan.commit {
            continue;
        }
        commit_file_scan(pool, scan, now).await?;
        report.files_read += 1;
        if scan.reset {
            report.files_reset += 1;
        }
        report.message_rows += scan.messages.len();
        report.start_rows += scan.starts.len();
        report.turn_rows += scan.turns.len();
    }
    Ok(report)
}

// ── Aggregation ─────────────────────────────────────────────────────────────

/// A measured usage value for one item. Every count is a real measurement;
/// "unmeasured" is expressed by the absence of this struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageAggregate {
    pub last_used_ms: Option<i64>,
    pub count_7d: i64,
    pub count_30d: i64,
    pub tokens_30d: i64,
    pub window_start_ms: i64,
}

/// In-memory view of the mirror. `unavailable()` answers `None` for every
/// lookup (F-1): absent corpus or failed scan must read as unmeasured.
#[derive(Debug, Clone)]
pub struct UsageSnapshot {
    available: bool,
    now_ms: i64,
    window_start_ms: i64,
    total_sessions: usize,
    sessions: HashMap<(UsageKind, String), Vec<(String, i64)>>,
    /// Skill / agent tokens within 30 days, summed per item.
    tokens_30d: HashMap<(UsageKind, String), i64>,
    /// MCP-server turns within 30 days: server → [(message_id, tokens)]. Kept
    /// per message so a pkg with several servers counts a turn that called two
    /// of them once, not once per server.
    mcp_turns_30d: HashMap<String, Vec<(String, i64)>>,
}

impl UsageSnapshot {
    /// No measurement is possible: every lookup returns `None`.
    pub fn unavailable() -> Self {
        Self {
            available: false,
            now_ms: 0,
            window_start_ms: 0,
            total_sessions: 0,
            sessions: HashMap::new(),
            tokens_30d: HashMap::new(),
            mcp_turns_30d: HashMap::new(),
        }
    }

    /// Build from raw mirror rows. `sessions`: (kind, name, session_key,
    /// last_used_ms); `tokens_30d`: (kind, name, tokens within 30 days) for
    /// skills and agents (MCP-server rows here are ignored); `mcp_turns_30d`:
    /// (server, message_id, tokens) for MCP-server turns within 30 days.
    /// `window_start_ms` is the earliest instant the mirror can see.
    pub fn from_rows(
        now_ms: i64,
        window_start_ms: i64,
        sessions: Vec<(UsageKind, String, String, i64)>,
        tokens_30d: Vec<(UsageKind, String, i64)>,
        mcp_turns_30d: Vec<(String, String, i64)>,
    ) -> Self {
        let total_sessions = sessions.len();
        let mut by_item: HashMap<(UsageKind, String), Vec<(String, i64)>> = HashMap::new();
        for (kind, name, session_key, last) in sessions {
            by_item.entry((kind, name)).or_default().push((session_key, last));
        }
        let tokens_30d = tokens_30d
            .into_iter()
            .filter(|(k, _, _)| *k != UsageKind::McpServer)
            .map(|(k, n, t)| ((k, n), t))
            .collect();
        let mut mcp: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        for (server, message_id, t) in mcp_turns_30d {
            mcp.entry(server).or_default().push((message_id, t));
        }
        Self {
            available: true,
            now_ms,
            window_start_ms,
            total_sessions,
            sessions: by_item,
            tokens_30d,
            mcp_turns_30d: mcp,
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Total (kind, name, session) rows in the mirror — `sources.usage.count`.
    pub fn total_sessions(&self) -> usize {
        self.total_sessions
    }

    /// Usage of a skill or agent. `None` when the mirror is unavailable.
    pub fn for_primitive(&self, kind: UsageKind, name: &str) -> Option<UsageAggregate> {
        self.aggregate(&[(kind, name.to_string())])
    }

    /// Usage across one or more MCP server keys (a pkg may contribute several;
    /// sessions are unioned, not summed). Names are normalized as Claude Code
    /// normalizes them into tool names. `None` when unavailable.
    pub fn for_servers(&self, names: &[String]) -> Option<UsageAggregate> {
        let keys: Vec<(UsageKind, String)> = names
            .iter()
            .map(|n| (UsageKind::McpServer, normalize_mcp_server_name(n)))
            .collect();
        self.aggregate(&keys)
    }

    fn aggregate(&self, keys: &[(UsageKind, String)]) -> Option<UsageAggregate> {
        if !self.available {
            return None;
        }
        let d7 = self.now_ms - 7 * DAY_MS;
        let d30 = self.now_ms - 30 * DAY_MS;
        let mut union: HashMap<&str, i64> = HashMap::new();
        let mut tokens = 0i64;
        let mut mcp_msgs: HashMap<&str, i64> = HashMap::new();
        for key in keys {
            if let Some(rows) = self.sessions.get(key) {
                for (sk, last) in rows {
                    let e = union.entry(sk.as_str()).or_insert(*last);
                    *e = (*e).max(*last);
                }
            }
            if key.0 == UsageKind::McpServer {
                for (msg, t) in self.mcp_turns_30d.get(&key.1).into_iter().flatten() {
                    let e = mcp_msgs.entry(msg.as_str()).or_insert(*t);
                    *e = (*e).max(*t);
                }
            } else {
                tokens += self.tokens_30d.get(key).copied().unwrap_or(0);
            }
        }
        tokens += mcp_msgs.values().sum::<i64>();
        Some(UsageAggregate {
            last_used_ms: union.values().copied().max(),
            count_7d: union.values().filter(|l| **l >= d7).count() as i64,
            count_30d: union.values().filter(|l| **l >= d30).count() as i64,
            tokens_30d: tokens,
            window_start_ms: self.window_start_ms,
        })
    }
}

/// DEC-29: the session that owns each message, among those holding it.
///
/// `holdings` are `(message_id, session_key)` pairs; `starts` maps a session
/// to its start. The owner is the holder with the earliest start, ties broken
/// by the smallest session key. A holder with no recorded start sorts last
/// (it cannot happen for a scanned file: every fact is itself dated). The
/// answer depends only on the set of holdings, never on their order.
pub fn message_owners<'a>(
    holdings: impl IntoIterator<Item = (&'a str, &'a str)>,
    starts: &HashMap<String, i64>,
) -> HashMap<&'a str, &'a str> {
    let start = |sk: &str| starts.get(sk).copied().unwrap_or(i64::MAX);
    let mut owners: HashMap<&'a str, &'a str> = HashMap::new();
    for (mid, sk) in holdings {
        match owners.get(mid) {
            Some(cur) if (start(cur), *cur) <= (start(sk), sk) => {}
            _ => {
                owners.insert(mid, sk);
            }
        }
    }
    owners
}

/// DEC-29: collapse message-level holdings into the session rows a
/// [`UsageSnapshot`] counts, keeping only messages each session **owns**.
/// Returns `(kind, name, session_key, last_used_ms)`, sorted.
pub fn resolve_owned_sessions(
    messages: &[MessageRow],
    starts: &HashMap<String, i64>,
) -> Vec<(UsageKind, String, String, i64)> {
    let owners = message_owners(
        messages
            .iter()
            .map(|m| (m.message_id.as_str(), m.session_key.as_str())),
        starts,
    );
    let mut sessions: HashMap<(UsageKind, &str, &str), i64> = HashMap::new();
    for m in messages {
        if owners.get(m.message_id.as_str()) != Some(&m.session_key.as_str()) {
            continue;
        }
        let e = sessions
            .entry((m.kind, m.name.as_str(), m.session_key.as_str()))
            .or_insert(m.timestamp_ms);
        *e = (*e).max(m.timestamp_ms);
    }
    let mut out: Vec<(UsageKind, String, String, i64)> = sessions
        .into_iter()
        .map(|((k, n, s), last)| (k, n.to_string(), s.to_string(), last))
        .collect();
    out.sort();
    out
}

/// The mirror's session rows after ownership (DEC-29) is resolved across
/// every file scanned so far: `(kind, name, session_key, last_used_ms)`.
pub async fn load_owned_sessions(
    pool: &SqlitePool,
) -> Result<Vec<(UsageKind, String, String, i64)>, String> {
    let raw_messages: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT kind, name, session_key, message_id, timestamp_ms FROM ngwa_usage_messages",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load usage messages: {e}"))?;
    let raw_starts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT session_key, MIN(start_ms) FROM ngwa_usage_session_starts GROUP BY session_key",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load session starts: {e}"))?;
    let messages: Vec<MessageRow> = raw_messages
        .into_iter()
        .filter_map(|(k, name, session_key, message_id, timestamp_ms)| {
            UsageKind::parse(&k).map(|kind| MessageRow {
                kind,
                name,
                session_key,
                message_id,
                timestamp_ms,
            })
        })
        .collect();
    let starts: HashMap<String, i64> = raw_starts.into_iter().collect();
    Ok(resolve_owned_sessions(&messages, &starts))
}

/// Load the mirror into a [`UsageSnapshot`].
pub async fn load_usage_snapshot(pool: &SqlitePool, now_ms: i64) -> Result<UsageSnapshot, String> {
    let d30 = now_ms - 30 * DAY_MS;
    let sessions = load_owned_sessions(pool).await?;
    let raw_tokens: Vec<(String, String, i64)> = sqlx::query_as(
        r#"SELECT kind, name, SUM(tokens) FROM ngwa_usage_turns
           WHERE timestamp_ms >= ? AND kind <> 'mcp_server' GROUP BY kind, name"#,
    )
    .bind(d30)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load usage tokens: {e}"))?;
    let mcp_turns: Vec<(String, String, i64)> = sqlx::query_as(
        r#"SELECT name, message_id, tokens FROM ngwa_usage_turns
           WHERE timestamp_ms >= ? AND kind = 'mcp_server'"#,
    )
    .bind(d30)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("load mcp usage tokens: {e}"))?;
    let min_first: Option<i64> =
        sqlx::query_scalar("SELECT MIN(timestamp_ms) FROM ngwa_usage_messages")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("load usage window: {e}"))?;

    let tokens = raw_tokens
        .into_iter()
        .filter_map(|(k, n, t)| UsageKind::parse(&k).map(|k| (k, n, t)))
        .collect();
    // Earliest instant the mirror can see; an empty mirror sees from now on.
    let window_start_ms = min_first.unwrap_or(now_ms);
    Ok(UsageSnapshot::from_rows(now_ms, window_start_ms, sessions, tokens, mcp_turns))
}

/// Returns current unix timestamp in milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Shared test fixtures: a synthetic resumed-session pair shaped like the real
/// one (`8f46768f…` → `e94d16cb…`, DEC-29). Used here and by the 0065
/// migration test in `db.rs`.
#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::{Path, PathBuf};

    /// Parent session key. Deliberately sorts *after* the child's, so the tie
    /// break cannot be what picks the parent: only the earlier start can.
    pub const PARENT: &str = "p-zzz-parent";
    pub const CHILD: &str = "c-aaa-child";

    /// A `queue-operation` record, which opens a real main transcript. A
    /// resume does not copy these, so they are what gives a parent its
    /// earlier start (on the real pair: 21:52:10.852 and 21:52:11.172, before
    /// the copied opening prompt).
    pub fn queue_op(session: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"queue-operation","operation":"enqueue","timestamp":"{ts}","sessionId":"{session}"}}"#
        )
    }

    pub fn user(session: &str, uuid: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"{session}","uuid":"{uuid}","timestamp":"{ts}","message":{{"role":"user","content":"hi"}}}}"#
        )
    }

    /// An assistant record. `skill` sets `attributionSkill`; `content` is the
    /// raw JSON of the content blocks.
    pub fn assistant(session: &str, uuid: &str, msg_id: &str, ts: &str, skill: Option<&str>, content: &str) -> String {
        let attr = skill
            .map(|s| format!(r#","attributionSkill":"{s}""#))
            .unwrap_or_default();
        format!(
            r#"{{"type":"assistant","sessionId":"{session}","uuid":"{uuid}","requestId":"req-{msg_id}","timestamp":"{ts}"{attr},"message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"content":[{content}]}}}}"#
        )
    }

    pub const SKILL_X_CALL: &str = r#"{"type":"tool_use","name":"Skill","input":{"skill":"skill-x"}}"#;
    pub const MCP_CALL: &str = r#"{"type":"tool_use","name":"mcp__iyke__iyke_state","input":{}}"#;

    /// Timestamp of the parent's uncopied leading `queue-operation`.
    pub const PARENT_QUEUE_TS: &str = "2026-09-20T10:00:00.000Z";
    /// Timestamp of the opening prompt, which a resume copies unchanged.
    pub const OPENING_TS: &str = "2026-09-20T10:00:02.000Z";

    /// The copyable history, keyed to `session` with uuids prefixed `pfx`. A
    /// resume copies all of it, opening prompt included, with the original
    /// timestamps; only the `sessionId` and (in this fixture) the uuids change.
    fn history(session: &str, pfx: &str) -> Vec<String> {
        let mut v = Vec::new();
        v.push(user(session, &format!("{pfx}-u1"), OPENING_TS));
        v.push(assistant(session, &format!("{pfx}-a1"), "msg-1", "2026-09-20T10:00:05.000Z", None, SKILL_X_CALL));
        v.push(assistant(session, &format!("{pfx}-a2"), "msg-2", "2026-09-20T10:00:06.000Z", Some("skill-x"), MCP_CALL));
        v.push(assistant(session, &format!("{pfx}-a3"), "msg-2", "2026-09-20T10:00:06.500Z", Some("skill-x"), ""));
        v
    }

    /// Write the pair under `root/proj/`. Session A (parent) uses `skill-x` and
    /// the `iyke` MCP server. Session B (child) is a copy of A's history - new
    /// `sessionId` and uuids, same `message.id`s and timestamps - that then
    /// uses only `skill-y` in its own messages, plus `skill-x` again when
    /// `child_reuses_x`. Returns `(parent_path, child_path)`.
    pub fn write_fork_pair(root: &Path, child_reuses_x: bool) -> (PathBuf, PathBuf) {
        write_fork_pair_with(root, child_reuses_x, false)
    }

    /// As [`write_fork_pair`]. With `parent_continues`, the parent keeps
    /// writing after the child's last record (its own `skill-z` use at
    /// 12:00), so the parent both starts *and* ends outside the child: a rule
    /// keyed on the latest record rather than the earliest would pick the
    /// child as owner of the inherited history.
    pub fn write_fork_pair_with(
        root: &Path,
        child_reuses_x: bool,
        parent_continues: bool,
    ) -> (PathBuf, PathBuf) {
        let dir = root.join("proj");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Only the parent has the leading queue-operation: it is not copied.
        let mut parent = vec![queue_op(PARENT, PARENT_QUEUE_TS)];
        parent.extend(history(PARENT, "p"));
        if parent_continues {
            parent.push(user(PARENT, "p-u20", "2026-09-20T12:00:00.000Z"));
            parent.push(assistant(PARENT, "p-a20", "msg-20", "2026-09-20T12:00:05.000Z", Some("skill-z"), ""));
        }
        let mut child = history(CHILD, "c");
        child.push(user(CHILD, "c-u9", "2026-09-20T11:00:00.000Z"));
        child.push(assistant(CHILD, "c-a9", "msg-9", "2026-09-20T11:00:05.000Z", Some("skill-y"), ""));
        if child_reuses_x {
            child.push(assistant(CHILD, "c-a10", "msg-10", "2026-09-20T11:30:00.000Z", Some("skill-x"), ""));
        }
        let p = dir.join(format!("{PARENT}.jsonl"));
        let c = dir.join(format!("{CHILD}.jsonl"));
        std::fs::write(&p, format!("{}\n", parent.join("\n"))).expect("write parent");
        std::fs::write(&c, format!("{}\n", child.join("\n"))).expect("write child");
        (p, c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::PaDb;
    use std::io::Write;

    fn main_ctx() -> FileContext {
        FileContext::default()
    }

    fn assistant(session: &str, msg_id: &str, ts: &str, extra: &str, content: &str, out: i64) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{session}","timestamp":"{ts}"{extra},"message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":{out},"cache_creation_input_tokens":5,"cache_read_input_tokens":100}},"content":[{content}]}}}}"#
        )
    }

    async fn temp_pool() -> (tempfile::TempDir, SqlitePool) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("usage.db"));
        let pool = db.ensure_pool().await.expect("pool");
        (tmp, pool)
    }

    /// Sessions counting `(kind, name)` after DEC-29 ownership is resolved.
    async fn owned_sessions(pool: &SqlitePool, kind: &str, name: &str) -> Vec<String> {
        load_owned_sessions(pool)
            .await
            .expect("owned sessions")
            .into_iter()
            .filter(|(k, n, _, _)| k.as_str() == kind && n == name)
            .map(|(_, _, s, _)| s)
            .collect()
    }

    async fn count_sessions(pool: &SqlitePool, kind: &str, name: &str) -> i64 {
        owned_sessions(pool, kind, name).await.len() as i64
    }

    type Dump = (
        Vec<(String, String, String, String, i64)>,
        Vec<(String, i64)>,
        Vec<(String, String, String, i64)>,
    );

    async fn table_dump(pool: &SqlitePool) -> Dump {
        let s = sqlx::query_as(
            "SELECT kind, name, session_key, message_id, timestamp_ms FROM ngwa_usage_messages ORDER BY kind, name, session_key, message_id",
        )
        .fetch_all(pool)
        .await
        .expect("messages");
        let st = sqlx::query_as(
            "SELECT session_key, start_ms FROM ngwa_usage_session_starts ORDER BY session_key, source_path",
        )
        .fetch_all(pool)
        .await
        .expect("starts");
        let t = sqlx::query_as(
            "SELECT kind, name, message_id, tokens FROM ngwa_usage_turns ORDER BY kind, name, message_id",
        )
        .fetch_all(pool)
        .await
        .expect("turns");
        (s, st, t)
    }

    #[test]
    fn parse_timestamp_rfc3339_and_rejects_garbage() {
        let a = parse_timestamp_ms("2026-09-15T22:06:07.889Z").expect("ms");
        let b = parse_timestamp_ms("2026-09-15T22:06:07Z").expect("no ms");
        assert_eq!(a - b, 889);
        assert_eq!(parse_timestamp_ms("2026-09-15T23:06:07+01:00"), Some(b));
        assert_eq!(parse_timestamp_ms("not a time"), None);
        assert_eq!(parse_timestamp_ms("2026-13-45T99:99:99Z"), None);
    }

    #[test]
    fn unparseable_timestamp_drops_the_event() {
        // F-12: never dated now().
        let line = r#"{"type":"assistant","sessionId":"s1","timestamp":"garbage","attributionSkill":"groundwork","message":{"id":"m1","content":[]}}"#;
        assert!(extract_facts(line, &main_ctx()).is_empty());
        let no_ts = r#"{"type":"assistant","sessionId":"s1","attributionSkill":"groundwork","message":{"id":"m1","content":[]}}"#;
        assert!(extract_facts(no_ts, &main_ctx()).is_empty());
    }

    #[test]
    fn extracts_skill_mcp_and_ignores_parent_agent_spawn() {
        let line = assistant(
            "s1",
            "m1",
            "2026-09-15T22:06:07.889Z",
            r#","attributionSkill":"groundwork""#,
            r#"{"type":"tool_use","name":"Skill","input":{"skill":"workflow-authoring"}},
               {"type":"tool_use","name":"mcp__iyke__iyke_state","input":{}},
               {"type":"tool_use","name":"mcp__iyke__iyke_go","input":{}},
               {"type":"tool_use","name":"Agent","input":{"subagent_type":"Explore"}}"#,
            20,
        );
        let facts = extract_facts(&line, &main_ctx());
        let summary: Vec<(UsageKind, &str, i64)> =
            facts.iter().map(|f| (f.kind, f.name.as_str(), f.tokens)).collect();
        assert_eq!(
            summary,
            vec![
                (UsageKind::Skill, "groundwork", 135),
                (UsageKind::Skill, "workflow-authoring", 0),
                (UsageKind::McpServer, "iyke", 135),
            ],
            "one row per (kind, name) per record; the Agent spawn is not counted"
        );
        assert!(facts.iter().all(|f| f.session_key == "s1" && f.message_id == "m1"));
    }

    #[test]
    fn subagent_file_keys_by_agent_id_and_reads_meta_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj").join("sess-1").join("subagents");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let jsonl = dir.join("agent-abc123.jsonl");
        std::fs::write(&jsonl, "").expect("write");
        std::fs::write(dir.join("agent-abc123.meta.json"), r#"{"agentType":"Explore"}"#)
            .expect("meta");
        let ctx = file_context(&jsonl);
        let sub = ctx.subagent.as_ref().expect("subagent ctx");
        assert_eq!(sub.agent_id, "abc123");
        assert_eq!(sub.agent_type.as_deref(), Some("Explore"));

        let line = assistant("parent-session", "m9", "2026-09-15T22:06:07Z", "", "", 1);
        let facts = extract_facts(&line, &ctx);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].kind, UsageKind::Agent);
        assert_eq!(facts[0].name, "Explore");
        assert_eq!(facts[0].session_key, "abc123", "a subagent run is its own session");

        // A journal or main file is not a subagent context.
        assert!(file_context(&dir.join("journal.jsonl")).subagent.is_none());
        assert!(file_context(&tmp.path().join("proj").join("agent-x.jsonl")).subagent.is_none());
    }

    #[test]
    fn snapshot_unavailable_returns_none_everywhere() {
        let s = UsageSnapshot::unavailable();
        assert!(s.for_primitive(UsageKind::Skill, "groundwork").is_none());
        assert!(s.for_primitive(UsageKind::Agent, "Explore").is_none());
        assert!(s.for_servers(&["iyke".to_string()]).is_none());
    }

    #[test]
    fn snapshot_counts_distinct_sessions_and_measured_zero() {
        let now = 1_800_000_000_000;
        let s = UsageSnapshot::from_rows(
            now,
            now - 40 * DAY_MS,
            vec![
                (UsageKind::Skill, "groundwork".into(), "s1".into(), now - DAY_MS),
                (UsageKind::Skill, "groundwork".into(), "s2".into(), now - 10 * DAY_MS),
                (UsageKind::Skill, "groundwork".into(), "s3".into(), now - 35 * DAY_MS),
                (UsageKind::McpServer, "a".into(), "s1".into(), now - DAY_MS),
                (UsageKind::McpServer, "b".into(), "s1".into(), now - 2 * DAY_MS),
                (UsageKind::McpServer, "b".into(), "s4".into(), now - 3 * DAY_MS),
            ],
            vec![(UsageKind::Skill, "groundwork".into(), 900)],
            vec![
                // m1 called both servers a and b: its tokens must count once.
                ("a".into(), "m1".into(), 100),
                ("b".into(), "m1".into(), 100),
                ("b".into(), "m2".into(), 40),
            ],
        );
        let g = s.for_primitive(UsageKind::Skill, "groundwork").expect("measured");
        assert_eq!((g.count_7d, g.count_30d, g.tokens_30d), (1, 2, 900));
        assert_eq!(g.last_used_ms, Some(now - DAY_MS));

        let zero = s.for_primitive(UsageKind::Skill, "never-used").expect("measured zero");
        assert_eq!((zero.count_7d, zero.count_30d, zero.tokens_30d, zero.last_used_ms), (0, 0, 0, None));

        // Two servers of one pkg: s1 used both, counted once.
        let pkg = s.for_servers(&["a".into(), "b".into()]).expect("measured");
        assert_eq!(pkg.count_30d, 2);
        assert_eq!(pkg.tokens_30d, 140, "a turn calling two servers of one pkg counts once");
        assert_eq!(s.for_servers(&["b".into()]).expect("b").tokens_30d, 140);
        assert_eq!(s.for_servers(&["a".into()]).expect("a").tokens_30d, 100);
    }

    #[tokio::test]
    async fn double_scan_is_idempotent_and_append_adds_only_new_sessions() {
        let (_tmp_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        let proj = corpus.path().join("proj");
        std::fs::create_dir_all(&proj).expect("mkdir");
        let main = proj.join("s1.jsonl");
        let ts = "2026-09-20T10:00:00Z";
        // Two records of ONE message (streamed blocks): tokens must count once,
        // at the final (larger) usage.
        let lines = [
            assistant("s1", "m1", ts, r#","attributionSkill":"groundwork""#, "", 1),
            assistant("s1", "m1", ts, r#","attributionSkill":"groundwork""#, r#"{"type":"tool_use","name":"mcp__iyke__x","input":{}}"#, 50),
            assistant("s1", "m2", ts, r#","attributionSkill":"groundwork""#, "", 5),
        ];
        std::fs::write(&main, format!("{}\n", lines.join("\n"))).expect("write");

        let r1 = scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 1");
        assert_eq!(r1.files_read, 1);
        let first = table_dump(&pool).await;
        assert_eq!(count_sessions(&pool, "skill", "groundwork").await, 1);
        let tokens: i64 = sqlx::query_scalar("SELECT SUM(tokens) FROM ngwa_usage_turns WHERE kind='skill'")
            .fetch_one(&pool)
            .await
            .expect("tokens");
        assert_eq!(tokens, (10 + 50 + 5 + 100) + (10 + 5 + 5 + 100), "m1 counted once at its final usage");

        // Second scan of an unchanged corpus: nothing read, tables identical.
        let r2 = scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 2");
        assert_eq!(r2.files_read, 0, "unchanged file must not be re-read");
        assert_eq!(table_dump(&pool).await, first);

        // Forcing a full re-read (watermark wiped) still changes nothing: upserts.
        sqlx::query("DELETE FROM ngwa_transcript_files").execute(&pool).await.expect("wipe");
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 3");
        assert_eq!(table_dump(&pool).await, first, "re-reading lines is idempotent");

        // Append: one more turn in s1 (same session) + a new session file.
        let mut f = std::fs::OpenOptions::new().append(true).open(&main).expect("open");
        writeln!(f, "{}", assistant("s1", "m3", "2026-09-20T11:00:00Z", r#","attributionSkill":"groundwork""#, "", 1)).expect("append");
        drop(f);
        std::fs::write(
            proj.join("s2.jsonl"),
            format!("{}\n", assistant("s2", "m4", ts, r#","attributionSkill":"groundwork""#, "", 1)),
        )
        .expect("write s2");
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 4");
        assert_eq!(count_sessions(&pool, "skill", "groundwork").await, 2, "append adds exactly the new session");
        assert_eq!(count_sessions(&pool, "mcp_server", "iyke").await, 1);
    }

    #[tokio::test]
    async fn partial_trailing_line_is_left_for_next_scan() {
        let (_tmp_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        let file = corpus.path().join("s1.jsonl");
        let full = assistant("s1", "m1", "2026-09-20T10:00:00Z", r#","attributionSkill":"groundwork""#, "", 1);
        let (head, tail) = full.split_at(full.len() / 2);
        std::fs::write(&file, head).expect("write partial");
        let r = scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");
        assert_eq!(r.files_read, 1);
        let off: i64 = sqlx::query_scalar("SELECT byte_offset FROM ngwa_transcript_files")
            .fetch_one(&pool)
            .await
            .expect("offset");
        assert_eq!(off, 0, "a line without a newline is not consumed");
        assert_eq!(count_sessions(&pool, "skill", "groundwork").await, 0);

        let mut f = std::fs::OpenOptions::new().append(true).open(&file).expect("open");
        write!(f, "{tail}\n").expect("finish line");
        drop(f);
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 2");
        assert_eq!(count_sessions(&pool, "skill", "groundwork").await, 1);
        let off: i64 = sqlx::query_scalar("SELECT byte_offset FROM ngwa_transcript_files")
            .fetch_one(&pool)
            .await
            .expect("offset");
        assert_eq!(off as usize, full.len() + 1, "watermark is the post-read position");
    }

    #[tokio::test]
    async fn truncation_or_rewrite_deletes_contributions_and_rescans() {
        let (_tmp_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        let file = corpus.path().join("s1.jsonl");
        let ts = "2026-09-20T10:00:00Z";
        std::fs::write(
            &file,
            format!(
                "{}\n{}\n",
                assistant("s1", "m1", ts, r#","attributionSkill":"old-skill""#, "", 1),
                assistant("s1", "m2", ts, r#","attributionSkill":"old-skill""#, "", 1)
            ),
        )
        .expect("write");
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");
        assert_eq!(count_sessions(&pool, "skill", "old-skill").await, 1);

        // Truncate + rewrite with different content (shorter file).
        std::fs::write(
            &file,
            format!("{}\n", assistant("s9", "m7", ts, r#","attributionSkill":"new""#, "", 1)),
        )
        .expect("rewrite");
        let r = scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 2");
        assert_eq!(r.files_reset, 1);
        assert_eq!(count_sessions(&pool, "skill", "old-skill").await, 0, "old contributions deleted");
        assert_eq!(count_sessions(&pool, "skill", "new").await, 1);

        // Same-length in-place rewrite of the head is caught by the head hash.
        let len_before = std::fs::metadata(&file).expect("meta").len();
        let rewritten = assistant("s8", "m7", ts, r#","attributionSkill":"neu""#, "", 1);
        assert_eq!(rewritten.len() as u64 + 1, len_before);
        std::fs::write(&file, format!("{rewritten}\n")).expect("rewrite same size");
        // Make sure the mtime check alone cannot hide it: force a differing mtime.
        let later = SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .expect("open")
            .set_modified(later)
            .expect("set mtime");
        let r = scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan 3");
        assert_eq!(r.files_reset, 1);
        assert_eq!(count_sessions(&pool, "skill", "new").await, 0);
        assert_eq!(count_sessions(&pool, "skill", "neu").await, 1);
    }

    #[tokio::test]
    async fn absent_corpus_is_an_error_not_a_zero() {
        let (_tmp_db, pool) = temp_pool().await;
        let missing = std::env::temp_dir().join("ngwa-definitely-missing-corpus-7c1e");
        assert!(scan_and_mirror_transcripts(&pool, &missing).await.is_err());
    }

    // ── DEC-29: resumed / forked sessions ──────────────────────────────────

    use super::fixtures::{self, CHILD, PARENT};

    async fn scan_in_order(pool: &SqlitePool, files: &[&Path]) {
        for f in files {
            let scan = scan_file(f, None).expect("scan");
            commit_file_scan(pool, &scan, 0).await.expect("commit");
        }
    }

    #[tokio::test]
    async fn fork_counts_inherited_history_once() {
        let (_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        fixtures::write_fork_pair(corpus.path(), false);
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");

        // The parent's earlier start comes only from its uncopied leading
        // queue-operation; the copied opening prompt has the same timestamp in
        // both files, as on the real pair.
        let starts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT session_key, start_ms FROM ngwa_usage_session_starts ORDER BY session_key",
        )
        .fetch_all(&pool)
        .await
        .expect("starts");
        assert_eq!(
            starts,
            vec![
                (CHILD.to_string(), parse_timestamp_ms(fixtures::OPENING_TS).unwrap()),
                (PARENT.to_string(), parse_timestamp_ms(fixtures::PARENT_QUEUE_TS).unwrap()),
            ]
        );

        // Both files hold skill-x, but only the parent owns those messages.
        assert_eq!(owned_sessions(&pool, "skill", "skill-x").await, vec![PARENT.to_string()]);
        assert_eq!(owned_sessions(&pool, "skill", "skill-y").await, vec![CHILD.to_string()]);
        assert_eq!(owned_sessions(&pool, "mcp_server", "iyke").await, vec![PARENT.to_string()]);
        // The holdings really are shared: the pre-fix rule would count two.
        let holders: i64 = sqlx::query_scalar(
            "SELECT COUNT(DISTINCT session_key) FROM ngwa_usage_messages WHERE kind='skill' AND name='skill-x'",
        )
        .fetch_one(&pool)
        .await
        .expect("holders");
        assert_eq!(holders, 2);

        // Tokens stay per-message: msg-2 counted once, not once per copy.
        let now = parse_timestamp_ms("2026-09-21T00:00:00Z").unwrap();
        let snap = load_usage_snapshot(&pool, now).await.expect("snapshot");
        let x = snap.for_primitive(UsageKind::Skill, "skill-x").expect("measured");
        assert_eq!((x.count_7d, x.count_30d, x.tokens_30d), (1, 1, 30));
        assert_eq!(x.last_used_ms, parse_timestamp_ms("2026-09-20T10:00:06.500Z"));
        let y = snap.for_primitive(UsageKind::Skill, "skill-y").expect("measured");
        assert_eq!(y.count_30d, 1);
    }

    #[tokio::test]
    async fn fork_counts_do_not_depend_on_scan_order() {
        let corpus = tempfile::tempdir().expect("corpus");
        let (parent, child) = fixtures::write_fork_pair(corpus.path(), false);

        let (_a, parent_first) = temp_pool().await;
        scan_in_order(&parent_first, &[&parent, &child]).await;

        let (_b, child_first) = temp_pool().await;
        scan_in_order(&child_first, &[&child]).await;
        // Child alone: it is the only holder, so it owns the copy for now.
        assert_eq!(owned_sessions(&child_first, "skill", "skill-x").await, vec![CHILD.to_string()]);
        scan_in_order(&child_first, &[&parent]).await;
        // The parent arriving later takes the messages over.
        assert_eq!(owned_sessions(&child_first, "skill", "skill-x").await, vec![PARENT.to_string()]);

        let a = load_owned_sessions(&parent_first).await.expect("a");
        let b = load_owned_sessions(&child_first).await.expect("b");
        assert_eq!(a, b, "the same corpus scanned in either order yields the same sessions");
        assert_eq!(table_dump(&parent_first).await, table_dump(&child_first).await);
        for (kind, name, want) in [("skill", "skill-x", 1), ("skill", "skill-y", 1), ("mcp_server", "iyke", 1)] {
            assert_eq!(count_sessions(&parent_first, kind, name).await, want, "{name} parent-first");
            assert_eq!(count_sessions(&child_first, kind, name).await, want, "{name} child-first");
        }
    }

    #[tokio::test]
    async fn fork_owner_is_the_earliest_start_even_when_the_parent_ends_last() {
        // The parent keeps writing after the child's last record. Ownership
        // must follow the earliest start (the parent's queue-operation), not
        // the latest record, which here would be the parent's and so hand the
        // inherited history to the child.
        let (_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        let (parent, child) = fixtures::write_fork_pair_with(corpus.path(), false, true);
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");

        let last = |p: &Path| {
            std::fs::read_to_string(p)
                .expect("read")
                .lines()
                .filter_map(|l| extract_record(l, &main_ctx()).dated.map(|(_, ts)| ts))
                .max()
                .expect("dated records")
        };
        assert!(last(&parent) > last(&child), "the parent really does end after the child");

        assert_eq!(owned_sessions(&pool, "skill", "skill-x").await, vec![PARENT.to_string()]);
        assert_eq!(owned_sessions(&pool, "mcp_server", "iyke").await, vec![PARENT.to_string()]);
        assert_eq!(owned_sessions(&pool, "skill", "skill-y").await, vec![CHILD.to_string()]);
        assert_eq!(owned_sessions(&pool, "skill", "skill-z").await, vec![PARENT.to_string()]);
    }

    #[tokio::test]
    async fn tied_starts_count_a_shared_message_once() {
        let (_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        let ts = "2026-09-20T10:00:00.000Z";
        for s in ["s-b", "s-a"] {
            std::fs::write(
                corpus.path().join(format!("{s}.jsonl")),
                format!("{}\n", fixtures::assistant(s, &format!("{s}-u"), "shared", ts, Some("skill-x"), "")),
            )
            .expect("write");
        }
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");
        let starts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT session_key, start_ms FROM ngwa_usage_session_starts ORDER BY session_key",
        )
        .fetch_all(&pool)
        .await
        .expect("starts");
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0].1, starts[1].1, "the starts really are tied");
        assert_eq!(
            owned_sessions(&pool, "skill", "skill-x").await,
            vec!["s-a".to_string()],
            "counted exactly once, by the smaller session key"
        );
    }

    #[tokio::test]
    async fn fork_child_counts_its_own_later_use() {
        let (_db, pool) = temp_pool().await;
        let corpus = tempfile::tempdir().expect("corpus");
        fixtures::write_fork_pair(corpus.path(), true);
        scan_and_mirror_transcripts(&pool, corpus.path()).await.expect("scan");
        assert_eq!(
            owned_sessions(&pool, "skill", "skill-x").await,
            vec![CHILD.to_string(), PARENT.to_string()],
            "a genuinely new use of skill-x after the resume counts the child"
        );
        let last: Vec<i64> = load_owned_sessions(&pool)
            .await
            .expect("owned")
            .into_iter()
            .filter(|(k, n, s, _)| *k == UsageKind::Skill && n == "skill-x" && s == CHILD)
            .map(|(_, _, _, l)| l)
            .collect();
        assert_eq!(
            last,
            vec![parse_timestamp_ms("2026-09-20T11:30:00Z").unwrap()],
            "the child's row reflects only its own message, not the inherited ones"
        );
    }

    #[test]
    fn message_owners_ignores_holding_order() {
        let starts: HashMap<String, i64> =
            [("late".to_string(), 20), ("early".to_string(), 10), ("tie-b".to_string(), 10)]
                .into_iter()
                .collect();
        let holdings = [
            ("m1", "late"),
            ("m1", "tie-b"),
            ("m1", "early"),
            ("m2", "late"),
            ("m3", "unknown"),
            ("m3", "late"),
        ];
        let forward = message_owners(holdings.iter().copied(), &starts);
        let backward = message_owners(holdings.iter().rev().copied(), &starts);
        assert_eq!(forward, backward);
        assert_eq!(forward.get("m1"), Some(&"early"), "earliest start; 'early' < 'tie-b' breaks the tie");
        assert_eq!(forward.get("m2"), Some(&"late"), "a sole holder owns its message");
        assert_eq!(forward.get("m3"), Some(&"late"), "a holder with no recorded start sorts last");
    }

    /// DoD live check — read-only over the real `~/.claude/projects`, mirrored
    /// into a throwaway database in a temp dir. Never touches the user's
    /// Ikenga database. Run with:
    /// `cargo test --lib live_corpus_top_usage -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "reads the real ~/.claude/projects corpus; run explicitly"]
    async fn live_corpus_top_usage() {
        let root = claude_projects_dir().expect("home");
        let (tmp, pool) = temp_pool().await;
        println!("temp db: {}", tmp.path().join("usage.db").display());
        let t0 = std::time::Instant::now();
        let r1 = scan_and_mirror_transcripts(&pool, &root).await.expect("scan");
        println!(
            "cold scan: {:?} — files_seen={} files_read={} message_rows={} start_rows={} turn_rows={} read_errors={}",
            t0.elapsed(), r1.files_seen, r1.files_read, r1.message_rows, r1.start_rows, r1.turn_rows, r1.read_errors.len()
        );
        let t1 = std::time::Instant::now();
        let r2 = scan_and_mirror_transcripts(&pool, &root).await.expect("scan 2");
        println!("warm rescan: {:?} — files_read={} (only files written since)", t1.elapsed(), r2.files_read);

        let now = now_ms();
        let snap = load_usage_snapshot(&pool, now).await.expect("load");
        println!("mirror sessions (kind,name,session rows): {}", snap.total_sessions());
        println!("window_start: {}", chrono::DateTime::from_timestamp_millis(snap.window_start_ms).map(|d| d.to_rfc3339()).unwrap_or_default());
        for kind in [UsageKind::Skill, UsageKind::Agent] {
            let mut names: Vec<String> = snap
                .sessions
                .keys()
                .filter(|(k, _)| *k == kind)
                .map(|(_, n)| n.clone())
                .collect();
            names.sort();
            let mut rows: Vec<(String, UsageAggregate)> = names
                .into_iter()
                .map(|n| {
                    let a = snap.for_primitive(kind, &n).expect("available");
                    (n, a)
                })
                .collect();
            rows.sort_by(|a, b| b.1.count_30d.cmp(&a.1.count_30d).then(a.0.cmp(&b.0)));
            println!("\nTop 10 {} by count_30d (distinct sessions):", kind.as_str());
            println!("{:<42} {:>9} {:>8} {:>14}  last_used", "name", "count_30d", "count_7d", "tokens_30d");
            for (n, a) in rows.iter().take(10) {
                let last = a
                    .last_used_ms
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("{:<42} {:>9} {:>8} {:>14}  {}", n, a.count_30d, a.count_7d, a.tokens_30d, last);
            }
        }

        // DEC-29 on the real resumed pair: what each session *holds* (the
        // pre-fix rule counted every holding) versus what it *owns*.
        let d30 = now - 30 * DAY_MS;
        let holders_30d = |name: &'static str| {
            let pool = pool.clone();
            async move {
                let n: i64 = sqlx::query_scalar(
                    r#"SELECT COUNT(*) FROM (SELECT session_key FROM ngwa_usage_messages
                        WHERE kind = 'skill' AND name = ? GROUP BY session_key
                        HAVING MAX(timestamp_ms) >= ?)"#,
                )
                .bind(name)
                .bind(d30)
                .fetch_one(&pool)
                .await
                .expect("holders");
                n
            }
        };
        for name in ["groundwork", "royalti-design"] {
            let owned = snap.for_primitive(UsageKind::Skill, name).map(|a| a.count_30d).unwrap_or(0);
            println!(
                "\n{name}: count_30d = {owned} (sessions owning a use); every-holder rule on the same rows = {}",
                holders_30d(name).await
            );
        }
        let owned = load_owned_sessions(&pool).await.expect("owned");
        for sk in [
            "8f46768f-0f8b-4218-93cf-c613d458e744",
            "e94d16cb-4734-4aa2-956c-0840da85f5f9",
        ] {
            let start: Option<i64> = sqlx::query_scalar(
                "SELECT MIN(start_ms) FROM ngwa_usage_session_starts WHERE session_key = ?",
            )
            .bind(sk)
            .fetch_one(&pool)
            .await
            .expect("start");
            let held: Vec<(String, String, i64)> = sqlx::query_as(
                r#"SELECT kind, name, COUNT(DISTINCT message_id) FROM ngwa_usage_messages
                   WHERE session_key = ? AND kind <> 'agent' GROUP BY kind, name ORDER BY kind, name"#,
            )
            .bind(sk)
            .fetch_all(&pool)
            .await
            .expect("held");
            let counts: Vec<String> = owned
                .iter()
                .filter(|(_, _, s, _)| s == sk)
                .map(|(k, n, _, _)| format!("{}:{n}", k.as_str()))
                .collect();
            println!(
                "\nsession {sk} start={}",
                start
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_default()
            );
            println!(
                "  holds (messages): {}",
                held.iter().map(|(k, n, c)| format!("{k}:{n}={c}")).collect::<Vec<_>>().join(", ")
            );
            println!("  counts for (owns): {}", counts.join(", "));
        }
    }
}
