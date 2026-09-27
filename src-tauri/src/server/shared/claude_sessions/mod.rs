//! Claude Code on-disk session logs (`~/.claude/projects/<slug>/<uuid>.jsonl`):
//! the listing, the transcript read and the session browser, shared by the
//! desktop `#[tauri::command]`s (`commands::claude`, `claude::session_browser`,
//! `commands::chi`'s merge) and the daemon's `/api/rpc` arms (WP-19 slice 5b).
//!
//! The parsers moved here from the desktop-gated `crate::claude` /
//! `crate::transcript` so the headless build has them; those modules
//! re-export them, so every `crate::claude::event::ChatEvent`-style path still
//! resolves:
//!
//! * [`event`] — the `ChatEvent` wire enum both parsers emit.
//! * [`stream_parser`] — the live `stream-json` parser (desktop-only user,
//!   compiled here because [`jsonl_reader`] dispatches through it).
//! * [`jsonl_reader`] — `read_jsonl` / `summarize` over an on-disk log.
//! * [`transcript_parser`] — the typed `TranscriptRecord` line parser the
//!   session browser summarizes with.
//!
//! **Roots and reach.** Every entry point takes the `projects/` root it scans
//! and an [`FsReach`]. The desktop passes its process home's root and
//! `Follow` (exactly its old behaviour). The daemon passes its router home's
//! root — the daemon PROCESS's home, a single-user seam (G-PRINCIPAL / WP-20:
//! under topology B each principal's daemon runs as that uid with its own
//! HOME) — and `Confined`, under which a slug dir or `.jsonl` that does not
//! canonicalize inside that root reads as absent, and a transcript read
//! refuses one. A session id is never a path on the daemon: see
//! [`read_session`].

#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

pub mod event;
pub mod jsonl_reader;
pub mod stream_parser;
pub mod transcript_parser;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::projects::FsReach;
use event::ChatEvent;
use jsonl_reader::{read_jsonl, summarize, SessionSummary as JsonlSessionSummary};
use transcript_parser::{parse_line, TranscriptRecord};

// ─── slugs ───────────────────────────────────────────────────────────────────

/// Inverse of `project_dir_to_slug` — `-Users-jane-work` →
/// `/Users/jane/work`. Best-effort; some legacy slugs may have
/// lost trailing slashes.
pub fn slug_to_project_dir(slug: &str) -> String {
    slug.replace('-', "/")
}

/// True for files Claude Code writes as session logs.
pub fn is_session_jsonl(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| {
                // sessionId is a uuid v4 — 36 chars with 4 hyphens.
                s.len() == 36 && s.matches('-').count() == 4
            })
            .unwrap_or(false)
}

/// `<home>/.claude/projects`.
pub(crate) fn projects_root_in(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

// ─── reach ───────────────────────────────────────────────────────────────────

/// The canonical root a `Confined` read must stay inside; `None` when
/// following. A root that does not canonicalize (missing) admits nothing —
/// which is also what an unconfined scan of a missing root finds.
struct Fence(Option<Option<PathBuf>>);

impl Fence {
    fn new(reach: FsReach, root: &Path) -> Self {
        Fence(match reach {
            FsReach::Follow => None,
            FsReach::Confined => Some(root.canonicalize().ok()),
        })
    }

    fn admits(&self, path: &Path) -> bool {
        match &self.0 {
            None => true,
            Some(None) => false,
            Some(Some(base)) => path
                .canonicalize()
                .map(|c| c.starts_with(base))
                .unwrap_or(false),
        }
    }
}

// ─── claude_list_sessions ────────────────────────────────────────────────────

/// `claude_list_sessions`' wire type (camelCase — the FE's `SessionSummary`).
#[derive(Serialize)]
pub struct SessionSummary {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "projectDir")]
    pub project_dir: String,
    #[serde(rename = "startedAt")]
    pub started_at: String,
    #[serde(rename = "lastMessageAt")]
    pub last_message_at: Option<String>,
    #[serde(rename = "messageCount")]
    pub message_count: u64,
    pub title: Option<String>,
    pub model: Option<String>,
}

impl From<JsonlSessionSummary> for SessionSummary {
    fn from(s: JsonlSessionSummary) -> Self {
        Self {
            session_id: s.session_id,
            project_dir: s.project_dir,
            started_at: s.started_at,
            last_message_at: s.last_message_at,
            message_count: s.message_count,
            title: s.title,
            model: s.model,
        }
    }
}

/// Summaries of the session logs under `root` (`<home>/.claude/projects`),
/// newest first. `project_dir` restricts to that dir's slug (`""` = all);
/// `limit` caps how many files are summarized.
pub(crate) fn list_sessions(
    root: &Path,
    project_dir: Option<&str>,
    limit: Option<usize>,
    reach: FsReach,
) -> Result<Vec<SessionSummary>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let fence = Fence::new(reach, root);

    // If a project dir is provided, restrict to its slug. Empty string is
    // treated as "all projects" so the frontend can pass cwd or "" without
    // branching. The slug is only ever compared against a directory entry's
    // name, never joined onto `root`, so it cannot name a path.
    let slug_filter = project_dir
        .filter(|s| !s.is_empty())
        .map(|d| d.replace('/', "-"));

    // Two-phase scan to keep the list view snappy when ~/.claude/projects has
    // thousands of jsonl files (real numbers: ~9k+). Phase 1 only reads
    // directory entries + mtime metadata; phase 2 calls `summarize` (which
    // reads the file contents) on the top-N most recently modified.
    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => return Err(format!("read projects root: {e}")),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Some(ref slug) = slug_filter {
            if dir.file_name().and_then(|n| n.to_str()) != Some(slug.as_str()) {
                continue;
            }
        }
        let inner = match std::fs::read_dir(&dir) {
            Ok(i) => i,
            Err(_) => continue,
        };
        for file in inner.flatten() {
            let path = file.path();
            if !is_session_jsonl(&path) || !fence.admits(&path) {
                continue;
            }
            let mtime = file
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            candidates.push((path, mtime));
        }
    }

    // Newest mtime first. mtime ≈ last_message_at because claude appends to
    // the jsonl on every envelope it writes.
    candidates.sort_by(|a, b| b.1.cmp(&a.1));

    let take = limit.unwrap_or(usize::MAX);
    let mut summaries: Vec<SessionSummary> = Vec::with_capacity(take.min(candidates.len()));
    for (path, _) in candidates.into_iter().take(take) {
        match summarize(&path) {
            Ok(Some(s)) => summaries.push(s.into()),
            Ok(None) => {}
            Err(e) => log::debug!("summarize {} failed: {e}", path.display()),
        }
    }

    // Re-sort by the actual `last_message_at` from the summaries — mtime is a
    // good predictor but the in-file timestamp is canonical.
    summaries.sort_by(|a, b| {
        let key_a = a
            .last_message_at
            .as_deref()
            .unwrap_or(a.started_at.as_str());
        let key_b = b
            .last_message_at
            .as_deref()
            .unwrap_or(b.started_at.as_str());
        key_b.cmp(key_a)
    });
    Ok(summaries)
}

// ─── claude_read_jsonl ───────────────────────────────────────────────────────

/// Pure helper: scan each `projects/` root's slug dirs for `<session_id>.jsonl`.
/// First match wins; roots are searched in order (legacy `$HOME` first).
pub(crate) fn locate_jsonl_in_roots(roots: &[PathBuf], session_id: &str) -> Option<PathBuf> {
    let target = format!("{session_id}.jsonl");
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else {
            continue;
        };
        for slug_entry in rd.flatten() {
            let candidate = slug_entry.path().join(&target);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

/// A session id the daemon will look up: Claude's are uuids, so anything
/// outside `[A-Za-z0-9_-]` is refused. This is what keeps an id from being a
/// path — `format!("{id}.jsonl")` joined onto a slug dir would otherwise
/// follow `..`, a separator, or (for an absolute id) replace the dir outright.
pub(crate) fn check_session_id(session_id: &str) -> Result<(), String> {
    let ok = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid session id {session_id:?}: expected a Claude session uuid"
        ))
    }
}

/// The daemon's `claude_read_jsonl`: the session id must be an id (see
/// [`check_session_id`]) and the log it resolves to must canonicalize inside
/// `root` — a `.jsonl` symlinked (or a slug dir symlinked) out of
/// `~/.claude/projects` is refused, never read. Errors otherwise match the
/// desktop's.
pub(crate) fn read_session(root: &Path, session_id: &str) -> Result<Vec<ChatEvent>, String> {
    check_session_id(session_id)?;
    let path = locate_jsonl_in_roots(&[root.to_path_buf()], session_id)
        .ok_or_else(|| format!("session {session_id} not found on disk"))?;
    if !Fence::new(FsReach::Confined, root).admits(&path) {
        return Err(format!(
            "session {session_id} resolves outside {}",
            root.display()
        ));
    }
    read_jsonl(&path).map_err(|e| format!("read_jsonl: {e}"))
}

// ─── claude_session_list (session browser, WP-04) ────────────────────────────

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
                TranscriptRecord::Assistant {
                    message, timestamp, ..
                } => {
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

/// Enumerates all sessions under `projects_dir` (`<home>/.claude/projects`),
/// or within one project slug directory.
pub(crate) fn enumerate_sessions(
    projects_dir: &Path,
    project_slug_filter: Option<&str>,
    reach: FsReach,
) -> Vec<ClaudeSessionSummary> {
    let mut summaries = Vec::new();

    if !projects_dir.is_dir() {
        return summaries;
    }
    let fence = Fence::new(reach, projects_dir);

    let entries = match fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return summaries,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let slug = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if let Some(filter) = project_slug_filter {
            if slug != filter {
                continue;
            }
        }

        if let Ok(files) = fs::read_dir(&path) {
            for f_entry in files.flatten() {
                let f_path = f_entry.path();
                if f_path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && fence.admits(&f_path)
                {
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
