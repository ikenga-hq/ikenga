//! Chi seats — the seat store (G-SEATS §1–§6, §9.2, §9.5, §10; WP-65).
//!
//! A **seat** is a named, per-project slot (`seat:<project>/<name>`) that
//! points at one session: a Chi run (`chi_cache` row) or an agent terminal
//! tab. The row lives in `iyke_seats` (migration 0067). Everything a seat
//! *is* right now — `vacant` / `live` / `idle` / `run`, whether it can resume,
//! where it is mounted — is **derived at read time** from the stored session
//! and the live world (PTYs, `hooks://event` agent liveness, `chi_cache`,
//! the openrouter adapter). A read never writes.
//!
//! Writes are the 13 `seats_*` commands (§9.2). Each runs under the per-seat
//! async mutex (§4.0), as one write-first transaction, and returns the
//! `seats://changed` events to emit after commit — one per affected seat
//! (§10).
//!
//! The DEC-69 invariants live here:
//! - DEC-69a: a vacant seat resumes, then sends — the text is the resumed
//!   session's first turn, and the seat binds only after the engine call
//!   returns (`resume_core`).
//! - DEC-69b: *Clear* never touches the seat's memory (`clear_core`).
//! - DEC-69c: one session sits in at most one seat; a move is atomic
//!   (`bind_session`, backed by two partial unique indexes).
//!
//! Holds and takeover (§5) are a courtesy protocol between clients, not a
//! security boundary.
//!
//! This is the AppHandle-free core (pool + [`WorldSnapshot`] + an engine
//! callback), shared by the desktop's `#[tauri::command]`s in
//! `iyke/seats.rs` — which build the world from the PTY manager, pane state
//! and the hooks listener, and emit the [`Effects`] — and the daemon's
//! `/api/rpc` arms in `server/rpc_seats.rs`. The tests of these cores live
//! with the desktop glue (`iyke/seats.rs`), which re-exports this module.

// Much of this is reached only from the desktop glue and its tests.
#![cfg_attr(not(feature = "desktop"), allow(dead_code))]

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqliteConnection, SqlitePool, Transaction};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

use super::projects::get_active_project_id;
use crate::pty::TerminalDescriptor;

// ═══════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════

/// App-wide event, one per affected seat, after commit (§10).
pub(crate) const SEATS_CHANGED_EVENT: &str = "seats://changed";

/// P-4: hold TTL default 10 min, clamped 1 s – 60 min.
pub(crate) const HOLD_TTL_DEFAULT_MS: i64 = 600_000;
pub(crate) const HOLD_TTL_MIN_MS: i64 = 1_000;
pub(crate) const HOLD_TTL_MAX_MS: i64 = 3_600_000;
/// P-12: the §4.1 resume claim lives 30 s; the §4.5 queue polls every 2 s.
pub(crate) const CLAIM_TTL_MS: i64 = 30_000;
pub(crate) const QUEUE_POLL: Duration = Duration::from_secs(2);
/// A resume/fill already holds the destination's mutex across the engine
/// call; any further seat it must unbind is locked with this bound, so a
/// move holding that seat and waiting on the destination can't deadlock us.
pub(crate) const EXTRA_LOCK_WAIT: Duration = Duration::from_secs(5);

/// The only engine whose terminal agent reports turns (`SessionStart`,
/// `UserPromptSubmit`, `Stop`, `SessionEnd`) through the hooks bridge.
pub(crate) const CLAUDE_ENGINE: &str = "claude-code";
pub(crate) const OPENROUTER_ENGINE: &str = "openrouter";
/// `chi_cache.owner` for runs the seat store starts.
pub(crate) const SEAT_RUN_OWNER: &str = "seat";

/// The five scope-keyed memory tables (`0016_iyke_memory.sql`). A rename
/// rewrites `scope` in all of them; a Remove with `removeMemory` clears them.
pub(crate) const SCOPE_TABLES: [&str; 5] = [
    "iyke_scratchpads",
    "iyke_todos",
    "iyke_kv",
    "iyke_locks",
    "iyke_timers",
];

macro_rules! seat_cols {
    () => {
        "id, project_id, name, engine_id, session_kind, session_ref, external_id, \
         session_cwd, hold_client, hold_since, hold_expires_at, displaced_client, \
         displaced_by, displaced_at, created_at, last_active_at"
    };
}

pub(crate) const SEAT_BY_ID: &str =
    concat!("SELECT ", seat_cols!(), " FROM iyke_seats WHERE id = ?");
pub(crate) const SEAT_BY_NAME: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE project_id = ? AND name = ?"
);
pub(crate) const SEATS_IN_PROJECT: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE project_id = ? ORDER BY created_at, id"
);
pub(crate) const SEAT_BY_TERMINAL: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE session_kind = 'terminal' AND session_ref = ?"
);
/// §4.3: the seats a bind will unbind — same session ref, or the same engine
/// conversation reached through another ref.
pub(crate) const SEATS_CONFLICTING: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE id <> ? AND ((session_kind = ? AND session_ref = ?) \
     OR (? IS NOT NULL AND engine_id = ? AND external_id = ?)) ORDER BY id"
);

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ═══════════════════════════════════════════════════════════════════════
// Grammar (§1.2, §1.3, §3.1)
// ═══════════════════════════════════════════════════════════════════════

// The two grammar validators live in `server::shared::seat_grammar` (WP-19
// slice 5a) so the headless G-ACTIONS schema can check a `chi` run's `seat`
// with the same code; re-exported so every `seats::validate_*` path stands.
pub(crate) use crate::server::shared::seat_grammar::{validate_project_slug, validate_seat_name};

/// §1.3 canonical address, also the seat's memory scope (§3.1).
pub(crate) fn seat_address(project_id: &str, name: &str) -> String {
    format!("seat:{project_id}/{name}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParsedAddress {
    Qualified {
        project: String,
        name: String,
    },
    /// Resolved in the shell's active project.
    Bare {
        name: String,
    },
}

/// Parse any §1.3 iyke form: `seat:<project>/<name>`, `@<name>`,
/// `<project>/<name>`, `<name>`.
pub(crate) fn parse_seat_address(input: &str) -> Result<ParsedAddress, SeatError> {
    fn qualified(input: &str, project: &str, name: &str) -> Result<ParsedAddress, SeatError> {
        if validate_project_slug(project).is_err() || validate_seat_name(name).is_err() {
            return Err(SeatError::invalid_address(input));
        }
        Ok(ParsedAddress::Qualified {
            project: project.to_string(),
            name: name.to_string(),
        })
    }
    let s = input.trim();
    if let Some(rest) = s.strip_prefix("seat:") {
        let Some((project, name)) = rest.split_once('/') else {
            return Err(SeatError::invalid_address(input));
        };
        return qualified(input, project, name);
    }
    if let Some(name) = s.strip_prefix('@') {
        // `@<name>` is the active-project short form only.
        if validate_seat_name(name).is_err() {
            return Err(SeatError::invalid_address(input));
        }
        return Ok(ParsedAddress::Bare {
            name: name.to_string(),
        });
    }
    if let Some((project, name)) = s.split_once('/') {
        return qualified(input, project, name);
    }
    if validate_seat_name(s).is_err() {
        return Err(SeatError::invalid_address(input));
    }
    Ok(ParsedAddress::Bare {
        name: s.to_string(),
    })
}

// ═══════════════════════════════════════════════════════════════════════
// Errors (§9.5)
// ═══════════════════════════════════════════════════════════════════════

/// `{ code, message, details? }`. Over the bridge each code maps to the HTTP
/// status `http_status()` returns (WP-70).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl std::fmt::Display for SeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl SeatError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The engine call already succeeded and the bind after it failed: the
    /// run exists with the caller's text but the seat didn't take it. Carry
    /// the run id (merged into `details`) so a caller never blindly resends.
    pub(crate) fn after_engine(mut self, run_id: &str) -> Self {
        let mut details = match self.details.take() {
            Some(Value::Object(m)) => m,
            _ => serde_json::Map::new(),
        };
        details.insert("run_id".to_string(), json!(run_id));
        self.details = Some(Value::Object(details));
        self.message = format!(
            "{} — the text was already sent as run {run_id}, which is not seated; don't resend it",
            self.message
        );
        self
    }

    /// §9.5 code → HTTP status.
    #[allow(dead_code)] // WP-70's bridge routes are the consumer.
    pub(crate) fn http_status(&self) -> u16 {
        match self.code {
            "invalid_seat_name" | "invalid_address" => 400,
            "seat_not_found" | "project_not_found" => 404,
            "not_resumable" | "engine_unsupported" => 422,
            "engine_failed" => 502,
            "internal" => 500,
            _ => 409,
        }
    }

    pub(crate) fn invalid_seat_name(msg: String) -> Self {
        Self::new("invalid_seat_name", msg)
    }
    pub(crate) fn invalid_address(input: &str) -> Self {
        Self::new(
            "invalid_address",
            format!("not a seat address: {input:?} (use <name>, @<name>, <project>/<name> or seat:<project>/<name>)"),
        )
    }
    pub(crate) fn seat_not_found() -> Self {
        Self::new("seat_not_found", "that seat no longer exists")
    }
    pub(crate) fn project_not_found(project: &str) -> Self {
        Self::new(
            "project_not_found",
            format!("project {project:?} does not exist or is archived"),
        )
    }
    pub(crate) fn name_taken(name: &str) -> Self {
        Self::new("seat_name_taken", format!("{name} is already a seat"))
    }
    pub(crate) fn engine_mismatch(session_engine: &str, seat_engine: &str) -> Self {
        Self::new(
            "engine_mismatch",
            format!("a {session_engine} session can't sit in a {seat_engine} seat"),
        )
    }
    pub(crate) fn terminal_not_found(terminal: &str) -> Self {
        Self::new(
            "terminal_not_found",
            format!("terminal {terminal:?} is not running in this app"),
        )
    }
    pub(crate) fn not_vacant(name: &str) -> Self {
        Self::new("seat_not_vacant", format!("{name} is not vacant"))
    }
    pub(crate) fn resuming(name: &str) -> Self {
        Self::new("seat_resuming", format!("{name} is already being resumed"))
    }
    pub(crate) fn busy(name: &str) -> Self {
        Self::new(
            "seat_busy",
            format!("{name} already has a text queued for its run"),
        )
    }
    pub(crate) fn agent_not_live(name: &str) -> Self {
        Self::new(
            "agent_not_live",
            format!("the agent in @{name}'s terminal isn't running yet"),
        )
    }
    pub(crate) fn conflict(msg: impl Into<String>) -> Self {
        Self::new("conflict", msg)
    }
    pub(crate) fn held(name: &str, hold: &SeatHold) -> Self {
        Self::new(
            "seat_held",
            format!("{name} is held by {} since {}", hold.client, hold.since),
        )
        .with_details(json!({
            "client": hold.client,
            "since": hold.since,
            "expires_at": hold.expires_at,
        }))
    }
    pub(crate) fn taken_over(name: &str, by: &str, at: i64) -> Self {
        Self::new("seat_taken_over", format!("{by} took over {name} at {at}"))
            .with_details(json!({ "by": by, "at": at }))
    }
    pub(crate) fn not_resumable(reason: NotResumableReason) -> Self {
        Self::new(
            "not_resumable",
            format!("this seat's session can't be resumed ({})", reason.as_str()),
        )
        .with_details(json!({ "reason": reason }))
    }
    pub(crate) fn engine_unsupported(engine_id: &str) -> Self {
        Self::new(
            "engine_unsupported",
            format!("engine {engine_id:?} can't hold a seat"),
        )
        .with_details(json!({ "engine_id": engine_id }))
    }
    pub(crate) fn engine_failed(msg: String) -> Self {
        Self::new("engine_failed", msg)
    }
    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self::new("internal", msg)
    }
}

/// A database error. A unique-constraint violation is a `409 conflict`: the
/// indexes are the backstop for DEC-69c (§4.0) and nothing was changed.
pub(crate) fn db_err(e: sqlx::Error) -> SeatError {
    let msg = e.to_string();
    if msg.contains("UNIQUE constraint failed") {
        SeatError::conflict(format!("the change conflicts with another seat ({msg})"))
    } else {
        SeatError::internal(format!("seat store: {msg}"))
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Wire types (§1.1, §1.6, §9.2, §10)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SeatSession {
    Run {
        run_id: String,
        external_id: Option<String>,
        cwd: Option<String>,
    },
    Terminal {
        terminal_id: String,
        external_id: Option<String>,
        cwd: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatHold {
    pub client: String,
    pub since: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SeatStatus {
    Live,
    Idle,
    Run,
    Vacant,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentState {
    Live,
    Starting,
    Unreported,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EngineResume {
    Durable,
    ProcessLocal,
    None,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotResumableReason {
    NoSession,
    ProcessLocal,
    NoResumeSupport,
    NoResumeId,
    RunMissing,
    EngineUnavailable,
}

impl NotResumableReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoSession => "no_session",
            Self::ProcessLocal => "process_local",
            Self::NoResumeSupport => "no_resume_support",
            Self::NoResumeId => "no_resume_id",
            Self::RunMissing => "run_missing",
            Self::EngineUnavailable => "engine_unavailable",
        }
    }
}

/// `{ resumable: true } | { resumable: false; reason }`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatResume {
    pub resumable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotResumableReason>,
}

impl SeatResume {
    pub(crate) fn yes() -> Self {
        Self {
            resumable: true,
            reason: None,
        }
    }
    pub(crate) fn no(reason: NotResumableReason) -> Self {
        Self {
            resumable: false,
            reason: Some(reason),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatMount {
    pub window_label: String,
    pub pane_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatQueued {
    pub since: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatPadLatest {
    pub name: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatPad {
    pub count: i64,
    pub latest: Option<SeatPadLatest>,
}

/// §1.6 — what `seats_list` / `seats_get` return. Everything after
/// `agent_id` is derived at read time.
#[derive(Debug, Clone, Serialize)]
pub struct SeatView {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub engine_id: String,
    pub session: Option<SeatSession>,
    pub created_at: i64,
    pub last_active_at: i64,
    pub hold: Option<SeatHold>,
    pub address: String,
    pub agent_id: String,
    // ── derived ──
    pub status: SeatStatus,
    pub agent: Option<AgentState>,
    pub resume: SeatResume,
    pub engine_resume: EngineResume,
    pub mount: Option<SeatMount>,
    pub queued: Option<SeatQueued>,
    pub pad: SeatPad,
    pub inbox_count: i64,
}

/// §5.1. The caller asserts `client`; nothing authenticates it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatActor {
    pub client: String,
    #[serde(default)]
    pub hold: bool,
    #[serde(default)]
    pub takeover: bool,
    #[serde(default)]
    pub hold_ttl_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind")]
pub enum SeatSessionRef {
    #[serde(rename = "run", rename_all = "camelCase")]
    Run { run_id: String },
    #[serde(rename = "terminal", rename_all = "camelCase")]
    Terminal {
        terminal_id: String,
        /// Chi engine id (the FE maps wrap ids: claude → claude-code, …).
        engine_id: String,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        external_id: Option<String>,
    },
}

/// `{ seatId } | { address }` — address is any §1.3 iyke form.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SeatAddress {
    Id {
        #[serde(rename = "seatId")]
        seat_id: String,
    },
    Address {
        address: String,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveOpts {
    #[serde(default)]
    pub claim_resume: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveOpts {
    #[serde(default)]
    pub claim: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResumeFallback {
    Fresh,
    Refuse,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeOpts {
    pub fallback: ResumeFallback,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FillOpts {
    #[serde(default)]
    pub persistent: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOpts {
    pub remove_memory: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SeatStart {
    Empty,
    Session { session: SeatSessionRef },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSeatReq {
    #[serde(default)]
    pub project_id: Option<String>,
    pub name: String,
    pub engine_id: String,
    pub start: SeatStart,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "route")]
pub enum SeatRoute {
    #[serde(rename = "pty")]
    Pty {
        seat: SeatView,
        terminal_id: String,
        agent: AgentState,
        lease_holder: Option<String>,
    },
    #[serde(rename = "chi-resume")]
    ChiResume {
        seat: SeatView,
        run_id: String,
        busy: bool,
    },
    #[serde(rename = "vacant")]
    Vacant {
        seat: SeatView,
        resume: SeatResume,
        claim: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct SeatMoveResult {
    pub seat: SeatView,
    pub from_seat_ids: Vec<String>,
    /// §4.1 path T step 3: the move carried a claim that had expired or was
    /// someone else's. The move still bound; this reports it. Absent when
    /// false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub claim_lost: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeOutcome {
    Resumed,
    StartedFresh,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeatResumeResult {
    pub seat: SeatView,
    pub run_id: String,
    pub outcome: ResumeOutcome,
    pub previous: Option<SeatSession>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotResumableReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeatFillResult {
    pub seat: SeatView,
    pub run_id: String,
    pub previous: Option<SeatSession>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeatRemoveResult {
    pub seat_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatEngineInfo {
    pub engine_id: &'static str,
    pub wrap_id: Option<&'static str>,
    pub engine_resume: Option<EngineResume>,
    pub seatable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// §10. Consumers invalidate `seats_list` for `project_id`; never a source
/// of truth.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeatsChangedEvent {
    pub project_id: String,
    pub seat_id: String,
    pub kinds: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_seat_ids: Option<Vec<String>>,
    /// Round 47 erratum E-4: set, with kind `queue-dropped`, when a §4.5
    /// queued text was dropped instead of sent — `cleared`, `removed`,
    /// `no_run` (the seat no longer holds a run), `run_missing` or
    /// `send_failed`. A lost queued text is visible, never silent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_dropped: Option<&'static str>,
}

// ═══════════════════════════════════════════════════════════════════════
// Engine capability (§6.1)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy)]
pub(crate) struct EngineCap {
    pub engine_id: &'static str,
    /// The terminal-wrap engine id, where one exists.
    pub wrap_id: Option<&'static str>,
    /// `None` = the engine can't hold a seat.
    pub resume: Option<EngineResume>,
    /// The CLI binary `build_engine_command_with` resolves, for install state.
    pub binary: Option<&'static str>,
    pub unsupported_reason: Option<&'static str>,
}

/// §6.1, static, from `server/shared/chi_exec.rs::build_engine_command_with`. The test
/// `every_chi_engine_arm_has_a_capability_row` walks that function's arms, so
/// a new engine can't land without a row here.
pub(crate) const ENGINE_CAPS: &[EngineCap] = &[
    EngineCap {
        engine_id: "claude-code",
        wrap_id: Some("claude"),
        resume: Some(EngineResume::Durable),
        binary: Some("claude"),
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "codex",
        wrap_id: Some("codex"),
        resume: Some(EngineResume::Durable),
        binary: Some("codex"),
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "antigravity-cli",
        wrap_id: Some("antigravity"),
        resume: Some(EngineResume::Durable),
        binary: Some("agy"),
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "openrouter",
        wrap_id: None,
        resume: Some(EngineResume::ProcessLocal),
        binary: None,
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "opencode",
        wrap_id: None,
        resume: Some(EngineResume::None),
        binary: Some("opencode"),
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "pi",
        wrap_id: None,
        resume: Some(EngineResume::None),
        binary: Some("pi"),
        unsupported_reason: None,
    },
    EngineCap {
        engine_id: "cursor-agent",
        wrap_id: None,
        resume: None,
        binary: None,
        unsupported_reason: Some("cursor-agent runtime not implemented"),
    },
    EngineCap {
        engine_id: "gemini",
        wrap_id: Some("gemini"),
        resume: None,
        binary: None,
        unsupported_reason: Some("not yet supported by iyke chi"),
    },
];

pub(crate) fn engine_cap(engine_id: &str) -> Option<&'static EngineCap> {
    ENGINE_CAPS.iter().find(|c| c.engine_id == engine_id)
}

/// `None` = the engine can't hold a seat (not in the table, or refused).
pub(crate) fn engine_resume(engine_id: &str) -> Option<EngineResume> {
    engine_cap(engine_id).and_then(|c| c.resume)
}

pub(crate) fn engine_seatable(engine_id: &str) -> bool {
    engine_resume(engine_id).is_some()
}

/// `seats_engines()` rows, with install state from the world.
pub(crate) fn engines_info(world: &WorldSnapshot) -> Vec<SeatEngineInfo> {
    ENGINE_CAPS
        .iter()
        .map(|cap| {
            let capable = cap.resume.is_some();
            let installed = if cap.engine_id == OPENROUTER_ENGINE {
                world.openrouter_registered
            } else {
                world.engine_available(cap.engine_id)
            };
            let reason = if let Some(r) = cap.unsupported_reason {
                Some(r.to_string())
            } else if !installed {
                let why = match world.openrouter_unavailable {
                    Some(r) if cap.engine_id == OPENROUTER_ENGINE => r,
                    _ => world.missing_engine_reason.unwrap_or("not installed"),
                };
                Some(why.to_string())
            } else {
                None
            };
            SeatEngineInfo {
                engine_id: cap.engine_id,
                // No terminal can hold a seat here, so no wrap to spawn one.
                wrap_id: cap
                    .wrap_id
                    .filter(|_| world.terminals_unavailable.is_none()),
                engine_resume: cap.resume,
                seatable: capable && installed,
                reason,
            }
        })
        .collect()
}

/// How long a "not installed" or "couldn't tell" answer is reused. A WSL
/// probe costs seconds — up to its full cold-start timeout when WSL is
/// wedged — and seat listings are rebuilt often; without this every rebuild
/// would wait that timeout out again. A CLI installed (or a WSL repaired)
/// meanwhile shows up within this window. Hits are remembered by the
/// resolver itself.
const ENGINE_MISS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// The last non-hit install answer per binary: `false` = not installed,
/// `true` = couldn't tell (which counts as available).
#[derive(Default)]
struct InstallCache(std::sync::Mutex<HashMap<String, (std::time::Instant, bool)>>);

impl InstallCache {
    fn fresh(&self, binary: &str, now: std::time::Instant) -> Option<bool> {
        let map = self.0.lock().ok()?;
        let (at, available) = map.get(binary)?;
        (now.saturating_duration_since(*at) < ENGINE_MISS_TTL).then_some(*available)
    }

    /// Record a probe outcome (`None` = couldn't tell) and return what it
    /// means for seating.
    fn record(&self, binary: &str, outcome: Option<bool>, now: std::time::Instant) -> bool {
        let available = outcome.unwrap_or(true);
        if let Ok(mut map) = self.0.lock() {
            match outcome {
                Some(true) => {
                    map.remove(binary);
                }
                _ => {
                    map.insert(binary.to_string(), (now, available));
                }
            }
        }
        available
    }
}

/// Whether `binary` is installed — on the host PATH or, on Windows, inside
/// the configured WSL distro (the same lookup a chi run makes). It used to
/// answer `true` unconditionally on Windows, so an engine installed nowhere
/// was offered as seatable. A WSL that couldn't be asked counts as
/// available: "couldn't tell" must not read as "not installed", and chi
/// still fails the run with the real reason if it is absent.
pub(crate) async fn binary_available(binary: &str) -> bool {
    static CACHE: std::sync::OnceLock<InstallCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(available) = cache.fresh(binary, std::time::Instant::now()) {
        return available;
    }
    let outcome = super::chi_exec::engine_installed(binary).await;
    cache.record(binary, outcome, std::time::Instant::now())
}

#[cfg(test)]
mod install_cache_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Regression: a wedged WSL ("couldn't tell") was never cached, so every
    /// seat listing re-paid the full WSL probe timeout. It is now reused for
    /// the TTL like a miss — and still reads as available.
    #[test]
    fn couldnt_tell_and_misses_are_reused_for_the_ttl() {
        let cache = InstallCache::default();
        let t0 = Instant::now();
        assert_eq!(cache.fresh("codex", t0), None);

        assert!(cache.record("codex", None, t0));
        assert_eq!(cache.fresh("codex", t0 + Duration::from_secs(5)), Some(true));
        assert_eq!(cache.fresh("codex", t0 + ENGINE_MISS_TTL), None);

        assert!(!cache.record("pi", Some(false), t0));
        assert_eq!(cache.fresh("pi", t0 + Duration::from_secs(5)), Some(false));

        // A hit clears the entry; the resolver remembers hits itself.
        assert!(cache.record("pi", Some(true), t0));
        assert_eq!(cache.fresh("pi", t0), None);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// In-memory state: per-seat mutex, agent liveness, claims, queue (§4.0)
// ═══════════════════════════════════════════════════════════════════════

/// §2.5: one terminal's agent, from `hooks://event`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AgentLive {
    /// The PTY the hooks came from. A respawned tab (same terminal id, new
    /// PTY) no longer matches, which is the "PTY exit → entry removed" rule.
    pub pty_id: Option<String>,
    /// `SessionEnd` seen after the last `SessionStart`: the tab is a shell.
    pub exited: bool,
    pub turn_in_flight: bool,
    /// The last `SessionStart`'s `session_id`, the engine-native resume id.
    pub session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Claim {
    pub(crate) token: String,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct QueuedText {
    pub(crate) prompt: String,
    pub(crate) since: i64,
    pub(crate) client: String,
}

#[derive(Default)]
pub(crate) struct SeatStore {
    pub(crate) locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    /// Fed by the desktop's `hooks://event` listener; empty on the daemon.
    pub(crate) agents: StdMutex<HashMap<String, AgentLive>>,
    pub(crate) claims: StdMutex<HashMap<String, Claim>>,
    pub(crate) queue: StdMutex<HashMap<String, QueuedText>>,
    /// The desktop listener + poller are installed (`iyke::seats::install`).
    pub(crate) installed: AtomicBool,
}

pub(crate) fn store() -> &'static SeatStore {
    static STORE: OnceLock<SeatStore> = OnceLock::new();
    STORE.get_or_init(SeatStore::default)
}

pub(crate) fn guard<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SeatStore {
    pub(crate) fn lock_for(&self, seat_id: &str) -> Arc<AsyncMutex<()>> {
        guard(&self.locks)
            .entry(seat_id.to_string())
            .or_default()
            .clone()
    }

    pub(crate) async fn lock_one(&self, seat_id: &str) -> OwnedMutexGuard<()> {
        self.lock_for(seat_id).lock_owned().await
    }

    /// Lock every seat in `ids`, in ascending id order (§4.0).
    pub(crate) async fn lock_set(&self, ids: &BTreeSet<String>) -> Vec<OwnedMutexGuard<()>> {
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            guards.push(self.lock_for(id).lock_owned().await);
        }
        guards
    }

    pub(crate) fn forget_lock(&self, seat_id: &str) {
        guard(&self.locks).remove(seat_id);
    }

    pub(crate) fn agents_snapshot(&self) -> HashMap<String, AgentLive> {
        guard(&self.agents).clone()
    }

    pub(crate) fn live_claim(&self, seat_id: &str, now: i64) -> Option<Claim> {
        guard(&self.claims)
            .get(seat_id)
            .filter(|c| c.expires_at > now)
            .cloned()
    }

    pub(crate) fn take_claim(&self, seat_id: &str, now: i64) -> String {
        let token = Uuid::new_v4().to_string();
        guard(&self.claims).insert(
            seat_id.to_string(),
            Claim {
                token: token.clone(),
                expires_at: now + CLAIM_TTL_MS,
            },
        );
        token
    }

    /// §9.2 `seats_move` with `claim`: clear the claim `token` names; true
    /// when it was still live. Another client's live claim is left in place
    /// (the move still binds; the caller reports `claim_lost`), and an
    /// expired claim is swept.
    pub(crate) fn release_claim(&self, seat_id: &str, token: &str, now: i64) -> bool {
        let mut claims = guard(&self.claims);
        match claims.get(seat_id) {
            Some(c) if c.token == token => {
                let live = c.expires_at > now;
                claims.remove(seat_id);
                live
            }
            Some(c) if c.expires_at <= now => {
                claims.remove(seat_id);
                false
            }
            _ => false,
        }
    }

    /// Drop any claim on the seat (Clear, Remove).
    pub(crate) fn drop_claim(&self, seat_id: &str) {
        guard(&self.claims).remove(seat_id);
    }

    pub(crate) fn queued(&self, seat_id: &str) -> Option<QueuedText> {
        guard(&self.queue).get(seat_id).cloned()
    }

    pub(crate) fn queued_ids(&self) -> Vec<String> {
        guard(&self.queue).keys().cloned().collect()
    }

    /// One slot per seat; false when it is already full.
    pub(crate) fn enqueue(&self, seat_id: &str, text: QueuedText) -> bool {
        let mut q = guard(&self.queue);
        if q.contains_key(seat_id) {
            return false;
        }
        q.insert(seat_id.to_string(), text);
        true
    }

    pub(crate) fn dequeue(&self, seat_id: &str) -> Option<QueuedText> {
        guard(&self.queue).remove(seat_id)
    }
}

/// §2.5 map transition for one hook event. Pure, for tests.
pub(crate) fn apply_hook(
    agents: &mut HashMap<String, AgentLive>,
    terminal_id: &str,
    event: &str,
    session_id: Option<String>,
    pty_id: Option<String>,
) {
    match event {
        "SessionStart" => {
            agents.insert(
                terminal_id.to_string(),
                AgentLive {
                    pty_id,
                    exited: false,
                    turn_in_flight: false,
                    session_id,
                },
            );
        }
        "UserPromptSubmit" | "Stop" => {
            let entry = agents
                .entry(terminal_id.to_string())
                .or_insert_with(|| AgentLive {
                    pty_id: pty_id.clone(),
                    ..AgentLive::default()
                });
            if pty_id.is_some() && entry.pty_id != pty_id {
                // A new PTY on a reused tab id: start over from this event.
                *entry = AgentLive {
                    pty_id,
                    ..AgentLive::default()
                };
            }
            entry.exited = false;
            entry.turn_in_flight = event == "UserPromptSubmit";
        }
        "SessionEnd" => {
            let entry = agents
                .entry(terminal_id.to_string())
                .or_insert_with(|| AgentLive {
                    pty_id: pty_id.clone(),
                    ..AgentLive::default()
                });
            entry.exited = true;
            entry.turn_in_flight = false;
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════════════════
// The live world (read-time inputs to §2.2)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Default)]
pub(crate) struct TermLive {
    pub terminal_id: String,
    pub pty_id: String,
    pub running: bool,
    pub cwd: String,
    pub lease_holder: Option<String>,
    pub mount: Option<SeatMount>,
}

/// Everything derivation reads that isn't in the database. Built once per
/// command by `tauri_world` (desktop) or `daemon_world` (`rpc_seats.rs`);
/// tests build it by hand.
#[derive(Debug, Clone, Default)]
pub(crate) struct WorldSnapshot {
    pub terminals: Vec<TermLive>,
    pub agents: HashMap<String, AgentLive>,
    pub openrouter_registered: bool,
    /// thread id → does the adapter hold its transcript.
    pub openrouter_threads: HashMap<String, bool>,
    pub unavailable_engines: Vec<String>,
    // ── the daemon's deltas; all `None` on the desktop ──
    /// Why no terminal can be seated here. The daemon has no hooks bridge
    /// (agent liveness) and no pane state, so its PTYs never enter
    /// `terminals`: binding one is refused with this reason, a vacant seat
    /// gets no path-T claim (its first send is a headless run, path H), and
    /// `seats_engines` reports no terminal wrap.
    pub terminals_unavailable: Option<&'static str>,
    /// Why the in-process openrouter engine can't run here.
    pub openrouter_unavailable: Option<&'static str>,
    /// What `seats_engines` says of an engine whose CLI isn't found
    /// (default "not installed").
    pub missing_engine_reason: Option<&'static str>,
}

impl WorldSnapshot {
    /// A terminal by tab id (preferring a running PTY), then by PTY id.
    pub(crate) fn terminal(&self, id: &str) -> Option<&TermLive> {
        self.terminals
            .iter()
            .filter(|t| t.terminal_id == id)
            .max_by_key(|t| t.running)
            .or_else(|| self.terminals.iter().find(|t| t.pty_id == id))
    }

    pub(crate) fn engine_available(&self, engine_id: &str) -> bool {
        !self.unavailable_engines.iter().any(|e| e == engine_id)
    }

    /// `None` when the openrouter adapter isn't registered.
    pub(crate) fn openrouter_holds(&self, thread: &str) -> Option<bool> {
        if !self.openrouter_registered {
            return None;
        }
        Some(
            self.openrouter_threads
                .get(thread)
                .copied()
                .unwrap_or(false),
        )
    }

    /// The agent reported for this PTY, if any (a stale PTY doesn't count).
    pub(crate) fn live_agent(&self, t: &TermLive) -> Option<&AgentLive> {
        self.agents
            .get(&t.terminal_id)
            .filter(|a| a.pty_id.as_deref().map_or(true, |p| p == t.pty_id))
    }
}

pub(crate) fn mount_of(window_labels: &[String], pane_ids: &[String]) -> Option<SeatMount> {
    let label = if window_labels.iter().any(|l| l == "main") {
        "main".to_string()
    } else {
        window_labels.first()?.clone()
    };
    Some(SeatMount {
        window_label: label,
        pane_ids: pane_ids.to_vec(),
    })
}

pub(crate) fn term_live(d: TerminalDescriptor, now: u64) -> TermLive {
    let mount = mount_of(&d.window_labels, &d.pane_ids);
    let lease_holder = match (d.owner_agent_id, d.lease_expires_at) {
        (Some(agent), Some(exp)) if exp > now => Some(agent),
        (Some(agent), None) => Some(agent),
        _ => None,
    };
    TermLive {
        terminal_id: d.terminal_id,
        pty_id: d.pty_id,
        running: d.status == "running",
        cwd: d.cwd,
        lease_holder,
        mount,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Stored rows
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SeatRow {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub engine_id: String,
    pub session_kind: Option<String>,
    pub session_ref: Option<String>,
    pub external_id: Option<String>,
    pub session_cwd: Option<String>,
    pub hold_client: Option<String>,
    pub hold_since: Option<i64>,
    pub hold_expires_at: Option<i64>,
    pub displaced_client: Option<String>,
    pub displaced_by: Option<String>,
    pub displaced_at: Option<i64>,
    pub created_at: i64,
    pub last_active_at: i64,
}

impl SeatRow {
    pub(crate) fn address(&self) -> String {
        seat_address(&self.project_id, &self.name)
    }

    /// A hold is live while `expires_at > now`; an expired one is absent.
    pub(crate) fn live_hold(&self, now: i64) -> Option<SeatHold> {
        match (&self.hold_client, self.hold_since, self.hold_expires_at) {
            (Some(client), Some(since), Some(expires_at)) if expires_at > now => Some(SeatHold {
                client: client.clone(),
                since,
                expires_at,
            }),
            _ => None,
        }
    }
}

pub(crate) fn seat_from_row(r: &SqliteRow) -> SeatRow {
    SeatRow {
        id: r.get("id"),
        project_id: r.get("project_id"),
        name: r.get("name"),
        engine_id: r.get("engine_id"),
        session_kind: r.get("session_kind"),
        session_ref: r.get("session_ref"),
        external_id: r.get("external_id"),
        session_cwd: r.get("session_cwd"),
        hold_client: r.get("hold_client"),
        hold_since: r.get("hold_since"),
        hold_expires_at: r.get("hold_expires_at"),
        displaced_client: r.get("displaced_client"),
        displaced_by: r.get("displaced_by"),
        displaced_at: r.get("displaced_at"),
        created_at: r.get("created_at"),
        last_active_at: r.get("last_active_at"),
    }
}

/// The `chi_cache` fields derivation reads. A persistent run is a detached
/// `chi-runner` (WP-18b) with no pane, so nothing here locates a mount for
/// it; its runner pid (`chi_cache.pid`) is the chi reconciler's business.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ChiLite {
    pub engine_id: String,
    pub status: String,
    pub external_id: Option<String>,
    pub cwd: Option<String>,
}

pub(crate) async fn fetch_seat<'c, E>(ex: E, seat_id: &str) -> Result<Option<SeatRow>, SeatError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let row = sqlx::query(SEAT_BY_ID)
        .bind(seat_id.to_string())
        .fetch_optional(ex)
        .await
        .map_err(db_err)?;
    Ok(row.as_ref().map(seat_from_row))
}

pub(crate) async fn fetch_seat_by_name<'c, E>(
    ex: E,
    project_id: &str,
    name: &str,
) -> Result<Option<SeatRow>, SeatError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let row = sqlx::query(SEAT_BY_NAME)
        .bind(project_id.to_string())
        .bind(name.to_string())
        .fetch_optional(ex)
        .await
        .map_err(db_err)?;
    Ok(row.as_ref().map(seat_from_row))
}

pub(crate) async fn fetch_seat_by_terminal<'c, E>(
    ex: E,
    terminal_id: &str,
) -> Result<Option<SeatRow>, SeatError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let row = sqlx::query(SEAT_BY_TERMINAL)
        .bind(terminal_id.to_string())
        .fetch_optional(ex)
        .await
        .map_err(db_err)?;
    Ok(row.as_ref().map(seat_from_row))
}

pub(crate) async fn list_rows(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Vec<SeatRow>, SeatError> {
    let rows = sqlx::query(SEATS_IN_PROJECT)
        .bind(project_id.to_string())
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(seat_from_row).collect())
}

pub(crate) async fn fetch_conflicting<'c, E>(
    ex: E,
    dest_id: &str,
    spec: &BindSpec,
) -> Result<Vec<SeatRow>, SeatError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let rows = sqlx::query(SEATS_CONFLICTING)
        .bind(dest_id.to_string())
        .bind(spec.kind.to_string())
        .bind(spec.session_ref.clone())
        .bind(spec.external_id.clone())
        .bind(spec.engine_id.clone())
        .bind(spec.external_id.clone())
        .fetch_all(ex)
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(seat_from_row).collect())
}

pub(crate) async fn fetch_chi<'c, E>(ex: E, run_id: &str) -> Result<Option<ChiLite>, SeatError>
where
    E: sqlx::Executor<'c, Database = Sqlite>,
{
    let row = sqlx::query(
        "SELECT engine_id, status, external_id, cwd
         FROM chi_cache WHERE run_id = ?",
    )
    .bind(run_id.to_string())
    .fetch_optional(ex)
    .await
    .map_err(db_err)?;
    Ok(row.map(|r| ChiLite {
        engine_id: r.get("engine_id"),
        status: r.get::<Option<String>, _>("status").unwrap_or_default(),
        external_id: r.get("external_id"),
        cwd: r.get("cwd"),
    }))
}

pub(crate) async fn load_chi_for(
    pool: &SqlitePool,
    row: &SeatRow,
) -> Result<Option<ChiLite>, SeatError> {
    match (row.session_kind.as_deref(), row.session_ref.as_deref()) {
        (Some("run"), Some(run_id)) => fetch_chi(pool, run_id).await,
        _ => Ok(None),
    }
}

/// `(root_path, archived_at)` of a project, if the row exists.
pub(crate) async fn project_row(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Option<(Option<String>, Option<i64>)>, SeatError> {
    sqlx::query_as::<_, (Option<String>, Option<i64>)>(
        "SELECT root_path, archived_at FROM projects WHERE id = ?",
    )
    .bind(project_id.to_string())
    .fetch_optional(pool)
    .await
    .map_err(db_err)
}

pub(crate) async fn project_root(
    pool: &SqlitePool,
    project_id: &str,
) -> Result<Option<String>, SeatError> {
    Ok(project_row(pool, project_id)
        .await?
        .and_then(|(root, _)| root)
        .filter(|r| !r.trim().is_empty()))
}

pub(crate) async fn active_project(pool: &SqlitePool) -> Result<String, SeatError> {
    get_active_project_id(pool)
        .await
        .map_err(SeatError::internal)
}

pub(crate) async fn resolve_address(
    pool: &SqlitePool,
    seat: &SeatAddress,
) -> Result<SeatRow, SeatError> {
    let found = match seat {
        SeatAddress::Id { seat_id } => fetch_seat(pool, seat_id).await?,
        SeatAddress::Address { address } => match parse_seat_address(address)? {
            ParsedAddress::Qualified { project, name } => {
                fetch_seat_by_name(pool, &project, &name).await?
            }
            ParsedAddress::Bare { name } => {
                let project = active_project(pool).await?;
                fetch_seat_by_name(pool, &project, &name).await?
            }
        },
    };
    found.ok_or_else(SeatError::seat_not_found)
}

/// Open the command's one write transaction and take SQLite's write lock at
/// once (§4.0), so the transaction never upgrades a read lock mid-way. The
/// `PaDb` writer pool is a single connection, which already serializes every
/// in-process writer; the no-op UPDATE covers a second process on the file.
pub(crate) async fn begin_write(
    pool: &SqlitePool,
) -> Result<Transaction<'static, Sqlite>, SeatError> {
    let mut tx = pool.begin().await.map_err(db_err)?;
    sqlx::query("UPDATE iyke_seats SET last_active_at = last_active_at WHERE 0")
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    Ok(tx)
}

// ═══════════════════════════════════════════════════════════════════════
// Derivation (§2.2)
// ═══════════════════════════════════════════════════════════════════════

/// Where a send to the seat goes now; `seats_resolve` turns it into a route.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RouteHint {
    /// Vacant: no route but resume.
    None,
    Pty {
        terminal_id: String,
        agent: AgentState,
        lease_holder: Option<String>,
    },
    /// A Claude terminal whose agent hasn't reported since the PTY spawned.
    AgentStarting,
    Chi {
        run_id: String,
        busy: bool,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct Derived {
    pub status: SeatStatus,
    pub agent: Option<AgentState>,
    pub resume: SeatResume,
    pub mount: Option<SeatMount>,
    /// The stored session, with a run's `external_id` / `cwd` as `chi_cache`
    /// reports them now (§1.1 — read, never written back).
    pub session: Option<SeatSession>,
    pub hint: RouteHint,
}

pub(crate) fn vacant(resume: SeatResume, session: Option<SeatSession>) -> Derived {
    Derived {
        status: SeatStatus::Vacant,
        agent: None,
        resume,
        mount: None,
        session,
        hint: RouteHint::None,
    }
}

/// §6.2: can a vacant seat on `engine_id` resume this session?
pub(crate) fn resume_for(
    engine_id: &str,
    external_id: Option<&str>,
    openrouter_thread: Option<&str>,
    world: &WorldSnapshot,
) -> SeatResume {
    match engine_resume(engine_id) {
        None => SeatResume::no(NotResumableReason::EngineUnavailable),
        Some(EngineResume::None) => SeatResume::no(NotResumableReason::NoResumeSupport),
        Some(EngineResume::Durable) => {
            if external_id.is_none() {
                SeatResume::no(NotResumableReason::NoResumeId)
            } else if !world.engine_available(engine_id) {
                SeatResume::no(NotResumableReason::EngineUnavailable)
            } else {
                SeatResume::yes()
            }
        }
        Some(EngineResume::ProcessLocal) => {
            match (
                openrouter_thread,
                world.openrouter_holds(openrouter_thread.unwrap_or("")),
            ) {
                (_, None) => SeatResume::no(NotResumableReason::EngineUnavailable),
                (Some(_), Some(true)) => SeatResume::yes(),
                _ => SeatResume::no(NotResumableReason::ProcessLocal),
            }
        }
    }
}

/// §2.2, the whole table. Pure: `row` + `chi_cache` + the live world.
pub(crate) fn derive(row: &SeatRow, chi: Option<&ChiLite>, world: &WorldSnapshot) -> Derived {
    let (Some(kind), Some(session_ref)) = (row.session_kind.as_deref(), row.session_ref.as_deref())
    else {
        return vacant(SeatResume::no(NotResumableReason::NoSession), None);
    };
    if kind == "run" {
        derive_run(row, session_ref, chi, world)
    } else {
        derive_terminal(row, session_ref, world)
    }
}

pub(crate) fn derive_run(
    row: &SeatRow,
    run_id: &str,
    chi: Option<&ChiLite>,
    world: &WorldSnapshot,
) -> Derived {
    let Some(chi) = chi else {
        let session = SeatSession::Run {
            run_id: run_id.to_string(),
            external_id: row.external_id.clone(),
            cwd: row.session_cwd.clone(),
        };
        return vacant(
            SeatResume::no(NotResumableReason::RunMissing),
            Some(session),
        );
    };
    let external_id = chi.external_id.clone().or_else(|| row.external_id.clone());
    let session = Some(SeatSession::Run {
        run_id: run_id.to_string(),
        external_id: external_id.clone(),
        cwd: chi.cwd.clone().or_else(|| row.session_cwd.clone()),
    });
    // openrouter keys its transcript by thread id: the external id, else the run id.
    let thread = external_id.clone().unwrap_or_else(|| run_id.to_string());
    let resume = resume_for(
        &row.engine_id,
        external_id.as_deref(),
        Some(thread.as_str()),
        world,
    );
    // §4.4: a Chi run is never mounted — a persistent run is a detached
    // chi-runner with no pane (WP-18b retired the tmux session a terminal
    // could attach to).
    let occupied = |status: SeatStatus, busy: bool, resume: SeatResume| Derived {
        status,
        agent: None,
        resume,
        mount: None,
        session: session.clone(),
        hint: RouteHint::Chi {
            run_id: run_id.to_string(),
            busy,
        },
    };
    match chi.status.as_str() {
        "queued" | "running" => occupied(SeatStatus::Run, true, resume),
        "done" => {
            // A done run is `idle` only if its next turn can actually resume;
            // otherwise it is `vacant` with the reason (§2.2 rows 4–7).
            let idle = match engine_resume(&row.engine_id) {
                Some(EngineResume::Durable) => external_id.is_some(),
                Some(EngineResume::ProcessLocal) => world.openrouter_holds(&thread) == Some(true),
                _ => false,
            };
            if idle {
                occupied(SeatStatus::Idle, false, resume)
            } else {
                vacant(resume, session.clone())
            }
        }
        // failed / cancelled (and anything unknown): the session ended.
        _ => vacant(resume, session.clone()),
    }
}

pub(crate) fn derive_terminal(row: &SeatRow, terminal_id: &str, world: &WorldSnapshot) -> Derived {
    let session = Some(SeatSession::Terminal {
        terminal_id: terminal_id.to_string(),
        external_id: row.external_id.clone(),
        cwd: row.session_cwd.clone(),
    });
    let resume = resume_for(&row.engine_id, row.external_id.as_deref(), None, world);
    let Some(t) = world.terminal(terminal_id).filter(|t| t.running) else {
        // Unknown to PtyManager, or its PTY exited.
        return vacant(resume, session);
    };
    let pty = |agent: AgentState| RouteHint::Pty {
        terminal_id: terminal_id.to_string(),
        agent,
        lease_holder: t.lease_holder.clone(),
    };
    if row.engine_id != CLAUDE_ENGINE {
        // P-11: no liveness signal; `live` while the PTY runs.
        return Derived {
            status: SeatStatus::Live,
            agent: Some(AgentState::Unreported),
            resume,
            mount: t.mount.clone(),
            session,
            hint: pty(AgentState::Unreported),
        };
    }
    match world.live_agent(t) {
        // The agent exited to its shell: never type into that bash (R-1).
        Some(a) if a.exited => vacant(resume, session),
        Some(a) => Derived {
            status: if a.turn_in_flight {
                SeatStatus::Live
            } else {
                SeatStatus::Idle
            },
            agent: Some(AgentState::Live),
            resume,
            mount: t.mount.clone(),
            session,
            hint: pty(AgentState::Live),
        },
        None => Derived {
            status: SeatStatus::Live,
            agent: Some(AgentState::Starting),
            resume,
            mount: t.mount.clone(),
            session,
            hint: RouteHint::AgentStarting,
        },
    }
}

pub(crate) async fn pad_summary(pool: &SqlitePool, address: &str) -> Result<SeatPad, SeatError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM iyke_scratchpads WHERE scope = ?")
        .bind(address.to_string())
        .fetch_one(pool)
        .await
        .map_err(db_err)?;
    let latest: Option<(String, i64)> = sqlx::query_as(
        "SELECT name, updated_at FROM iyke_scratchpads WHERE scope = ?
         ORDER BY updated_at DESC, name LIMIT 1",
    )
    .bind(address.to_string())
    .fetch_optional(pool)
    .await
    .map_err(db_err)?;
    Ok(SeatPad {
        count,
        latest: latest.map(|(name, updated_at)| SeatPadLatest { name, updated_at }),
    })
}

pub(crate) async fn build_view(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    row: SeatRow,
    chi: Option<&ChiLite>,
) -> Result<SeatView, SeatError> {
    let derived = derive(&row, chi, world);
    let address = row.address();
    let pad = pad_summary(pool, &address).await?;
    let inbox_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM iyke_agent_inbox WHERE agent_id = ?")
            .bind(row.id.clone())
            .fetch_one(pool)
            .await
            .map_err(db_err)?;
    let now = now_ms();
    let hold = row.live_hold(now);
    let queued = store()
        .queued(&row.id)
        .map(|q| SeatQueued { since: q.since });
    let engine_resume = engine_resume(&row.engine_id).unwrap_or(EngineResume::None);
    let agent_id = row.id.clone();
    Ok(SeatView {
        id: row.id,
        project_id: row.project_id,
        name: row.name,
        engine_id: row.engine_id,
        session: derived.session,
        created_at: row.created_at,
        last_active_at: row.last_active_at,
        hold,
        address,
        agent_id,
        status: derived.status,
        agent: derived.agent,
        resume: derived.resume,
        engine_resume,
        mount: derived.mount,
        queued,
        pad,
        inbox_count,
    })
}

pub(crate) async fn view_of(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    row: SeatRow,
) -> Result<SeatView, SeatError> {
    let chi = load_chi_for(pool, &row).await?;
    build_view(pool, world, row, chi.as_ref()).await
}

pub(crate) async fn view_by_id(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
) -> Result<SeatView, SeatError> {
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    view_of(pool, world, row).await
}

// ═══════════════════════════════════════════════════════════════════════
// Holds and takeover (§5)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HoldChange {
    Keep,
    Acquire,
    Renew,
    TakeOver { from: String },
}

#[derive(Debug, Clone)]
pub(crate) struct Refusal {
    pub err: SeatError,
    /// §5.3: the refusal that tells a displaced client also clears the
    /// `displaced_*` columns.
    pub clear_displaced: bool,
}

pub(crate) fn hold_ttl(actor: &SeatActor) -> i64 {
    actor
        .hold_ttl_ms
        .unwrap_or(HOLD_TTL_DEFAULT_MS)
        .clamp(HOLD_TTL_MIN_MS, HOLD_TTL_MAX_MS)
}

/// §5.1–§5.3 for one seat a call touches. `acquire` is true for the
/// addressed seat when the actor asked for a hold; the seats a move unbinds
/// are only checked (and taken over, with `takeover`).
pub(crate) fn hold_gate(
    row: &SeatRow,
    actor: &SeatActor,
    now: i64,
    acquire: bool,
) -> Result<HoldChange, Refusal> {
    if row.displaced_client.as_deref() == Some(actor.client.as_str()) {
        return Err(Refusal {
            err: SeatError::taken_over(
                &row.name,
                row.displaced_by.as_deref().unwrap_or("another client"),
                row.displaced_at.unwrap_or(0),
            ),
            clear_displaced: true,
        });
    }
    match row.live_hold(now) {
        Some(hold) if hold.client != actor.client => {
            if actor.takeover {
                Ok(HoldChange::TakeOver { from: hold.client })
            } else {
                Err(Refusal {
                    err: SeatError::held(&row.name, &hold),
                    clear_displaced: false,
                })
            }
        }
        Some(_) if acquire => Ok(HoldChange::Renew),
        Some(_) => Ok(HoldChange::Keep),
        None if acquire => Ok(HoldChange::Acquire),
        None => Ok(HoldChange::Keep),
    }
}

pub(crate) async fn apply_hold(
    conn: &mut SqliteConnection,
    seat_id: &str,
    change: &HoldChange,
    actor: &SeatActor,
    now: i64,
) -> Result<Option<&'static str>, SeatError> {
    let expires = now + hold_ttl(actor);
    match change {
        HoldChange::Keep => Ok(None),
        HoldChange::Acquire => {
            sqlx::query(
                "UPDATE iyke_seats SET hold_client = ?, hold_since = ?, hold_expires_at = ?
                 WHERE id = ?",
            )
            .bind(actor.client.clone())
            .bind(now)
            .bind(expires)
            .bind(seat_id.to_string())
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
            Ok(Some("held"))
        }
        HoldChange::Renew => {
            sqlx::query("UPDATE iyke_seats SET hold_expires_at = ? WHERE id = ?")
                .bind(expires)
                .bind(seat_id.to_string())
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
            Ok(Some("held"))
        }
        HoldChange::TakeOver { from } => {
            sqlx::query(
                "UPDATE iyke_seats SET hold_client = ?, hold_since = ?, hold_expires_at = ?,
                        displaced_client = ?, displaced_by = ?, displaced_at = ?
                 WHERE id = ?",
            )
            .bind(actor.client.clone())
            .bind(now)
            .bind(expires)
            .bind(from.clone())
            .bind(actor.client.clone())
            .bind(now)
            .bind(seat_id.to_string())
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
            Ok(Some("taken-over"))
        }
    }
}

/// Turn a refusal into its error, clearing `displaced_*` first when the
/// refusal is the one-time §5.3 notice. Call with no transaction open.
pub(crate) async fn refuse(pool: &SqlitePool, seat_id: &str, refusal: Refusal) -> SeatError {
    if refusal.clear_displaced {
        let res = sqlx::query(
            "UPDATE iyke_seats SET displaced_client = NULL, displaced_by = NULL, displaced_at = NULL
             WHERE id = ?",
        )
        .bind(seat_id.to_string())
        .execute(pool)
        .await;
        if let Err(e) = res {
            tracing::warn!(target: "ikenga::seats", "clear displaced notice on {seat_id}: {e}");
        }
    }
    refusal.err
}

// ═══════════════════════════════════════════════════════════════════════
// Effects (§10)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PadChange {
    pub scope: String,
    pub name: String,
    pub version: i64,
    pub deleted: bool,
}

/// What a committed command tells the app: one `seats://changed` per affected
/// seat, plus scratchpad wake-ups for pads a rename or remove moved.
#[derive(Debug, Default)]
pub(crate) struct Effects {
    pub events: Vec<SeatsChangedEvent>,
    pub pads: Vec<PadChange>,
}

pub(crate) fn changed(
    row: &SeatRow,
    kinds: Vec<&'static str>,
    from: Option<Vec<String>>,
) -> SeatsChangedEvent {
    SeatsChangedEvent {
        project_id: row.project_id.clone(),
        seat_id: row.id.clone(),
        kinds,
        from_seat_ids: from,
        queue_dropped: None,
    }
}

/// E-4: `ev` with the `queue-dropped` kind and its reason added.
pub(crate) fn with_queue_dropped(
    mut ev: SeatsChangedEvent,
    reason: &'static str,
) -> SeatsChangedEvent {
    ev.kinds.push("queue-dropped");
    ev.queue_dropped = Some(reason);
    ev
}

pub(crate) fn with_hold_kind(
    mut kinds: Vec<&'static str>,
    hold_kind: Option<&'static str>,
) -> Vec<&'static str> {
    if let Some(k) = hold_kind {
        kinds.push(k);
    }
    kinds
}

// ═══════════════════════════════════════════════════════════════════════
// The atomic bind — every move, and the bind step of create / resume / fill
// (§4.0, §4.3)
// ═══════════════════════════════════════════════════════════════════════

/// A session, normalized for binding.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BindSpec {
    pub kind: &'static str,
    pub session_ref: String,
    pub engine_id: String,
    pub external_id: Option<String>,
    pub cwd: Option<String>,
}

pub(crate) struct NewSeat {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub engine_id: String,
}

pub(crate) struct BindOutcome {
    pub(crate) dest: SeatRow,
    pub(crate) from_seat_ids: Vec<String>,
    pub(crate) claim_lost: bool,
    pub(crate) effects: Effects,
}

/// Insert a seat row and its agent row (§1.5), in the caller's transaction.
pub(crate) async fn insert_seat(
    conn: &mut SqliteConnection,
    new: &NewSeat,
    hold: Option<(&str, i64, i64)>,
    now: i64,
) -> Result<(), SeatError> {
    let res = sqlx::query(
        "INSERT INTO iyke_seats
            (id, project_id, name, engine_id, hold_client, hold_since, hold_expires_at,
             created_at, last_active_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new.id.clone())
    .bind(new.project_id.clone())
    .bind(new.name.clone())
    .bind(new.engine_id.clone())
    .bind(hold.map(|h| h.0.to_string()))
    .bind(hold.map(|h| h.1))
    .bind(hold.map(|h| h.2))
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await;
    if let Err(e) = res {
        if e.to_string().contains("iyke_seats.name") {
            return Err(SeatError::name_taken(&new.name));
        }
        return Err(db_err(e));
    }
    sqlx::query(
        "INSERT INTO iyke_agents (id, name, model, metadata, registered_at, last_seen_at)
         VALUES (?, ?, NULL, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             metadata = excluded.metadata,
             last_seen_at = excluded.last_seen_at",
    )
    .bind(new.id.clone())
    .bind(seat_address(&new.project_id, &new.name))
    .bind(json!({ "seat": true }).to_string())
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

/// Bind `spec` to `dest_id` and unbind it from every other seat, in one
/// transaction (§4.3). Locks every affected seat in ascending id order,
/// re-selects the set inside the transaction and retries once if it grew
/// (§4.0). The hold rules apply to every seat touched (§5.1).
///
/// `dest_locked`: the caller already holds the destination's mutex (resume,
/// fill); the other seats are then locked with a bounded wait.
/// `create`: insert this seat (and its agent row) in the same transaction.
pub(crate) async fn bind_session(
    pool: &SqlitePool,
    dest_id: &str,
    spec: &BindSpec,
    actor: &SeatActor,
    create: Option<&NewSeat>,
    dest_locked: bool,
    claim: Option<&str>,
) -> Result<BindOutcome, SeatError> {
    let mut known: BTreeSet<String> = BTreeSet::new();
    known.insert(dest_id.to_string());
    for attempt in 0..2 {
        // 1. Read the affected set.
        for row in fetch_conflicting(pool, dest_id, spec).await? {
            known.insert(row.id);
        }
        // 2. Lock it, ascending.
        let _guards: Vec<OwnedMutexGuard<()>> = if dest_locked {
            let mut guards = Vec::new();
            for id in known.iter().filter(|id| id.as_str() != dest_id) {
                let lock = store().lock_for(id);
                match tokio::time::timeout(EXTRA_LOCK_WAIT, lock.lock_owned()).await {
                    Ok(g) => guards.push(g),
                    Err(_) => {
                        return Err(SeatError::conflict(
                            "another change to a seat this session sits in is in progress",
                        ))
                    }
                }
            }
            guards
        } else {
            store().lock_set(&known).await
        };

        let now = now_ms();
        let mut tx = begin_write(pool).await?;
        if let Some(new) = create {
            if let Err(e) = insert_seat(&mut tx, new, None, now).await {
                let _ = tx.rollback().await;
                return Err(e);
            }
        }
        // 3. Re-select inside the transaction.
        let dest = match fetch_seat(&mut *tx, dest_id).await {
            Ok(Some(d)) => d,
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(SeatError::seat_not_found());
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        if spec.engine_id != dest.engine_id {
            let _ = tx.rollback().await;
            return Err(SeatError::engine_mismatch(&spec.engine_id, &dest.engine_id));
        }
        let conflicts = match fetch_conflicting(&mut *tx, dest_id, spec).await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        // 4. The set grew: roll back, retry once with the larger set.
        if conflicts.iter().any(|c| !known.contains(&c.id)) {
            let _ = tx.rollback().await;
            for c in conflicts {
                known.insert(c.id);
            }
            if attempt == 0 {
                continue;
            }
            return Err(SeatError::conflict(
                "the seats this move touches changed during the move; try again",
            ));
        }

        // Holds, on every seat touched.
        let dest_change = match hold_gate(&dest, actor, now, actor.hold) {
            Ok(c) => c,
            Err(r) => {
                let _ = tx.rollback().await;
                return Err(refuse(pool, &dest.id, r).await);
            }
        };
        let mut from_changes = Vec::with_capacity(conflicts.len());
        for c in &conflicts {
            match hold_gate(c, actor, now, false) {
                Ok(change) => from_changes.push(change),
                Err(r) => {
                    let _ = tx.rollback().await;
                    return Err(refuse(pool, &c.id, r).await);
                }
            }
        }

        // Writes: unbind the others, bind the destination, then holds.
        let written: Result<(Option<&'static str>, Vec<Option<&'static str>>), SeatError> = async {
            for c in &conflicts {
                sqlx::query(
                    "UPDATE iyke_seats SET session_kind = NULL, session_ref = NULL,
                            external_id = NULL, session_cwd = NULL
                     WHERE id = ?",
                )
                .bind(c.id.clone())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            }
            sqlx::query(
                "UPDATE iyke_seats SET session_kind = ?, session_ref = ?, external_id = ?,
                        session_cwd = ?, last_active_at = ?
                 WHERE id = ?",
            )
            .bind(spec.kind.to_string())
            .bind(spec.session_ref.clone())
            .bind(spec.external_id.clone())
            .bind(spec.cwd.clone())
            .bind(now)
            .bind(dest.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            let dest_kind = apply_hold(&mut tx, &dest.id, &dest_change, actor, now).await?;
            let mut from_kinds = Vec::with_capacity(conflicts.len());
            for (c, change) in conflicts.iter().zip(from_changes.iter()) {
                from_kinds.push(apply_hold(&mut tx, &c.id, change, actor, now).await?);
            }
            Ok((dest_kind, from_kinds))
        }
        .await;
        let (dest_kind, from_kinds) = match written {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let dest_after = match fetch_seat(&mut *tx, dest_id).await {
            Ok(Some(d)) => d,
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(SeatError::seat_not_found());
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        tx.commit().await.map_err(db_err)?;

        // After commit: claims, then one event per affected seat.
        // Only a move that carries a claim touches claims: a plain move (a
        // drag, iyke) never wipes another client's live path-T claim.
        let claim_lost = match claim {
            Some(token) => !store().release_claim(dest_id, token, now),
            None => false,
        };
        let from_seat_ids: Vec<String> = conflicts.iter().map(|c| c.id.clone()).collect();
        let mut effects = Effects::default();
        let base = if create.is_some() {
            vec!["created", "bound"]
        } else {
            vec!["bound"]
        };
        effects.events.push(changed(
            &dest_after,
            with_hold_kind(base, dest_kind),
            (!from_seat_ids.is_empty()).then(|| from_seat_ids.clone()),
        ));
        for (c, kind) in conflicts.iter().zip(from_kinds) {
            effects
                .events
                .push(changed(c, with_hold_kind(vec!["unbound"], kind), None));
        }
        return Ok(BindOutcome {
            dest: dest_after,
            from_seat_ids,
            claim_lost,
            effects,
        });
    }
    Err(SeatError::conflict(
        "the seats this move touches changed during the move; try again",
    ))
}

/// Normalize a session ref for binding (§4.3 checks: a terminal must be a
/// running PTY this app can see — daemon terminals can't be seated, P-10).
pub(crate) async fn session_spec(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    session: &SeatSessionRef,
) -> Result<BindSpec, SeatError> {
    match session {
        SeatSessionRef::Run { run_id } => {
            let chi = fetch_chi(pool, run_id)
                .await?
                .ok_or_else(|| SeatError::conflict(format!("chi run not found: {run_id}")))?;
            Ok(BindSpec {
                kind: "run",
                session_ref: run_id.clone(),
                engine_id: chi.engine_id,
                external_id: chi.external_id,
                cwd: chi.cwd,
            })
        }
        SeatSessionRef::Terminal {
            terminal_id,
            engine_id,
            cwd,
            external_id,
        } => {
            if let Some(reason) = world.terminals_unavailable {
                return Err(SeatError::new("terminal_not_found", reason));
            }
            let t = world
                .terminal(terminal_id)
                .filter(|t| t.running)
                .ok_or_else(|| SeatError::terminal_not_found(terminal_id))?;
            let captured = world.live_agent(t).and_then(|a| a.session_id.clone());
            Ok(BindSpec {
                kind: "terminal",
                session_ref: t.terminal_id.clone(),
                engine_id: engine_id.clone(),
                external_id: external_id.clone().or(captured),
                cwd: cwd.clone().or_else(|| Some(t.cwd.clone())),
            })
        }
    }
}

/// The bind spec for a run an engine call just returned.
pub(crate) async fn run_spec(
    pool: &SqlitePool,
    run_id: &str,
    seat_engine: &str,
) -> Result<BindSpec, SeatError> {
    let chi = fetch_chi(pool, run_id).await?;
    Ok(BindSpec {
        kind: "run",
        session_ref: run_id.to_string(),
        engine_id: chi
            .as_ref()
            .map(|c| c.engine_id.clone())
            .unwrap_or_else(|| seat_engine.to_string()),
        external_id: chi.as_ref().and_then(|c| c.external_id.clone()),
        cwd: chi.and_then(|c| c.cwd),
    })
}

// ═══════════════════════════════════════════════════════════════════════
// Command cores (pool + world; no AppHandle — unit-testable)
// ═══════════════════════════════════════════════════════════════════════

/// One engine call, made through the existing Chi code (§9.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EngineCall {
    /// `chi_resume`'s core: the same run continues (same run id).
    ResumeRun { run_id: String, prompt: String },
    /// `spawn_chi_run`: a new run id; `resume_session_id` continues a
    /// conversation, `None` starts fresh.
    Start {
        engine_id: String,
        prompt: String,
        cwd: Option<String>,
        resume_session_id: Option<String>,
        persistent: bool,
    },
}

pub(crate) async fn list_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    rows: Vec<SeatRow>,
) -> Result<Vec<SeatView>, SeatError> {
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        views.push(view_of(pool, world, row).await?);
    }
    Ok(views)
}

/// §9.2 `seats_resolve`: where a send goes now. Applies §5 (acquiring the
/// hold with `actor.hold`) and, with `claim_resume` on a vacant seat, takes
/// the §4.1 claim. Never resumes or starts anything.
pub(crate) async fn resolve_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    actor: &SeatActor,
    claim_resume: bool,
) -> Result<(SeatRoute, Effects), SeatError> {
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    let change = match hold_gate(&row, actor, now, actor.hold) {
        Ok(c) => c,
        Err(r) => return Err(refuse(pool, &row.id, r).await),
    };
    let chi = load_chi_for(pool, &row).await?;
    let derived = derive(&row, chi.as_ref(), world);
    if derived.hint == RouteHint::AgentStarting {
        return Err(SeatError::agent_not_live(&row.name));
    }
    if derived.hint == RouteHint::None && claim_resume && store().live_claim(&row.id, now).is_some()
    {
        return Err(SeatError::resuming(&row.name));
    }

    let mut effects = Effects::default();
    let row = if change != HoldChange::Keep {
        let mut tx = begin_write(pool).await?;
        let kind = match apply_hold(&mut tx, &row.id, &change, actor, now).await {
            Ok(k) => k,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        tx.commit().await.map_err(db_err)?;
        effects
            .events
            .push(changed(&row, with_hold_kind(Vec::new(), kind), None));
        fetch_seat(pool, &row.id)
            .await?
            .ok_or_else(SeatError::seat_not_found)?
    } else {
        row
    };

    // The claim guards path T (§4.1): a terminal-kind or empty vacant seat the
    // UI refills by spawning a terminal. A run-kind vacant seat goes path H
    // (`seats_resume`), which the seat mutex already serializes and which a
    // live claim would refuse — so it gets no claim (`claim: null`). Nor does
    // a seat on a runs-only engine (no terminal wrap: openrouter, opencode,
    // pi): no agent terminal can be spawned for it, so path T is impossible
    // and its path H must not be refused by its own claim.
    // Nor does a seat where no terminal can be seated (the daemon).
    let path_t = row.session_kind.as_deref() != Some("run")
        && engine_cap(&row.engine_id).and_then(|c| c.wrap_id).is_some()
        && world.terminals_unavailable.is_none();
    let claim = (derived.hint == RouteHint::None && claim_resume && path_t)
        .then(|| store().take_claim(&row.id, now));
    let seat = build_view(pool, world, row, chi.as_ref()).await?;
    let route = match derived.hint {
        RouteHint::Pty {
            terminal_id,
            agent,
            lease_holder,
        } => SeatRoute::Pty {
            seat,
            terminal_id,
            agent,
            lease_holder,
        },
        RouteHint::Chi { run_id, busy } => SeatRoute::ChiResume { seat, run_id, busy },
        RouteHint::None | RouteHint::AgentStarting => SeatRoute::Vacant {
            resume: derived.resume,
            seat,
            claim,
        },
    };
    Ok((route, effects))
}

/// §9.2 `seats_create`.
pub(crate) async fn create_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    req: CreateSeatReq,
    actor: &SeatActor,
) -> Result<(SeatMoveResult, Effects), SeatError> {
    validate_seat_name(&req.name).map_err(SeatError::invalid_seat_name)?;
    if !engine_seatable(&req.engine_id) {
        return Err(SeatError::engine_unsupported(&req.engine_id));
    }
    let project_id = match req.project_id.clone().filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => active_project(pool).await?,
    };
    match project_row(pool, &project_id).await? {
        Some((_, None)) => {}
        _ => return Err(SeatError::project_not_found(&project_id)),
    }
    if fetch_seat_by_name(pool, &project_id, &req.name)
        .await?
        .is_some()
    {
        return Err(SeatError::name_taken(&req.name));
    }
    let new = NewSeat {
        id: Uuid::new_v4().to_string(),
        project_id,
        name: req.name.clone(),
        engine_id: req.engine_id.clone(),
    };

    match req.start {
        SeatStart::Empty => {
            let _guard = store().lock_one(&new.id).await;
            let now = now_ms();
            let hold = actor
                .hold
                .then(|| (actor.client.as_str(), now, now + hold_ttl(actor)));
            let mut tx = begin_write(pool).await?;
            if let Err(e) = insert_seat(&mut tx, &new, hold, now).await {
                let _ = tx.rollback().await;
                return Err(e);
            }
            tx.commit().await.map_err(db_err)?;
            let row = fetch_seat(pool, &new.id)
                .await?
                .ok_or_else(SeatError::seat_not_found)?;
            let mut effects = Effects::default();
            effects.events.push(changed(
                &row,
                with_hold_kind(vec!["created"], hold.map(|_| "held")),
                None,
            ));
            let seat = view_of(pool, world, row).await?;
            Ok((
                SeatMoveResult {
                    seat,
                    from_seat_ids: Vec::new(),
                    claim_lost: false,
                },
                effects,
            ))
        }
        // T2a: create with an open session is a move (§4.3).
        SeatStart::Session { session } => {
            let spec = session_spec(pool, world, &session).await?;
            if spec.engine_id != new.engine_id {
                return Err(SeatError::engine_mismatch(&spec.engine_id, &new.engine_id));
            }
            let out = bind_session(pool, &new.id, &spec, actor, Some(&new), false, None).await?;
            let seat = view_of(pool, world, out.dest).await?;
            Ok((
                SeatMoveResult {
                    seat,
                    from_seat_ids: out.from_seat_ids,
                    claim_lost: out.claim_lost,
                },
                out.effects,
            ))
        }
    }
}

/// §9.2 `seats_move` (DEC-69c).
pub(crate) async fn move_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    session: &SeatSessionRef,
    to_seat_id: &str,
    actor: &SeatActor,
    claim: Option<&str>,
) -> Result<(SeatMoveResult, Effects), SeatError> {
    let dest = fetch_seat(pool, to_seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let spec = session_spec(pool, world, session).await?;
    if spec.engine_id != dest.engine_id {
        return Err(SeatError::engine_mismatch(&spec.engine_id, &dest.engine_id));
    }
    let out = bind_session(pool, to_seat_id, &spec, actor, None, false, claim).await?;
    let seat = view_of(pool, world, out.dest).await?;
    Ok((
        SeatMoveResult {
            seat,
            from_seat_ids: out.from_seat_ids,
            claim_lost: out.claim_lost,
        },
        out.effects,
    ))
}

/// §4.1 path H, with §6.2's fresh fallback. Runs under the seat's mutex; the
/// engine call happens with no transaction open, and the seat binds **only
/// after it returns a run id**. If it fails, nothing is written.
// The WP-70 bridge and the tests; the Tauri commands take the mutex first
// and call the `_locked` variant.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn resume_core<F, Fut>(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    prompt: String,
    actor: &SeatActor,
    fallback: ResumeFallback,
    engine: F,
) -> Result<(SeatResumeResult, Effects), SeatError>
where
    F: FnOnce(EngineCall) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let _guard = store().lock_one(seat_id).await;
    resume_locked(pool, world, seat_id, prompt, actor, fallback, engine).await
}

/// `resume_core` for a caller that already holds the seat's mutex — and so
/// built `world` after taking it, so the vacancy check never reads liveness
/// (openrouter thread holds) from before a concurrent resume bound (§4.1).
pub(crate) async fn resume_locked<F, Fut>(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    prompt: String,
    actor: &SeatActor,
    fallback: ResumeFallback,
    engine: F,
) -> Result<(SeatResumeResult, Effects), SeatError>
where
    F: FnOnce(EngineCall) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    if let Err(r) = hold_gate(&row, actor, now, actor.hold) {
        return Err(refuse(pool, &row.id, r).await);
    }
    let chi = load_chi_for(pool, &row).await?;
    let derived = derive(&row, chi.as_ref(), world);
    if derived.status != SeatStatus::Vacant {
        return Err(SeatError::not_vacant(&row.name));
    }
    if store().live_claim(&row.id, now).is_some() {
        return Err(SeatError::resuming(&row.name));
    }
    let previous = derived.session.clone();
    let root = project_root(pool, &row.project_id).await?;

    let resumable_call = if derived.resume.resumable {
        match &previous {
            // A run: the chi_resume core, the same run id continues.
            Some(SeatSession::Run { run_id, .. }) => Some(EngineCall::ResumeRun {
                run_id: run_id.clone(),
                prompt: prompt.clone(),
            }),
            // A terminal: a new run on the same conversation, in its cwd.
            Some(SeatSession::Terminal {
                external_id, cwd, ..
            }) => Some(EngineCall::Start {
                engine_id: row.engine_id.clone(),
                prompt: prompt.clone(),
                cwd: cwd.clone().or_else(|| root.clone()),
                resume_session_id: external_id.clone(),
                persistent: false,
            }),
            None => None,
        }
    } else {
        None
    };
    let (call, outcome, reason) = match resumable_call {
        Some(call) => (call, ResumeOutcome::Resumed, None),
        None => {
            let reason = derived
                .resume
                .reason
                .unwrap_or(NotResumableReason::NoSession);
            match fallback {
                // An explicit resume never falls back (§6.2).
                ResumeFallback::Refuse => return Err(SeatError::not_resumable(reason)),
                // Dispatch: seats_fill's path — a new run with the text.
                ResumeFallback::Fresh => (
                    EngineCall::Start {
                        engine_id: row.engine_id.clone(),
                        prompt,
                        cwd: root,
                        resume_session_id: None,
                        persistent: false,
                    },
                    ResumeOutcome::StartedFresh,
                    Some(reason),
                ),
            }
        }
    };

    // Check the holds on every seat the bind will unbind *before* the engine
    // call, so a refusal never follows a sent text.
    let predicted = BindSpec {
        kind: "run",
        session_ref: match &call {
            EngineCall::ResumeRun { run_id, .. } => run_id.clone(),
            // A new run id: no other seat can hold it yet.
            EngineCall::Start { .. } => String::new(),
        },
        engine_id: row.engine_id.clone(),
        external_id: match &call {
            EngineCall::ResumeRun { .. } => previous.as_ref().and_then(|p| match p {
                SeatSession::Run { external_id, .. }
                | SeatSession::Terminal { external_id, .. } => external_id.clone(),
            }),
            EngineCall::Start {
                resume_session_id, ..
            } => resume_session_id.clone(),
        },
        cwd: None,
    };
    precheck_unbinds(pool, &row.id, &predicted, actor, now).await?;

    let run_id = engine(call).await.map_err(SeatError::engine_failed)?;
    let out = bind_after_engine(pool, &row, &run_id, actor).await?;
    let seat = view_of(pool, world, out.dest).await?;
    Ok((
        SeatResumeResult {
            seat,
            run_id,
            outcome,
            previous,
            reason,
        },
        out.effects,
    ))
}

/// Refuse before an engine call when a seat the following bind would unbind
/// is held by another client (§5.1) — the bind re-checks under its own locks.
pub(crate) async fn precheck_unbinds(
    pool: &SqlitePool,
    dest_id: &str,
    predicted: &BindSpec,
    actor: &SeatActor,
    now: i64,
) -> Result<(), SeatError> {
    for c in fetch_conflicting(pool, dest_id, predicted).await? {
        if let Err(r) = hold_gate(&c, actor, now, false) {
            return Err(refuse(pool, &c.id, r).await);
        }
    }
    Ok(())
}

/// The bind step of path H / fill, once the engine returned `run_id`. Any
/// failure here carries the run id (`SeatError::after_engine`).
pub(crate) async fn bind_after_engine(
    pool: &SqlitePool,
    row: &SeatRow,
    run_id: &str,
    actor: &SeatActor,
) -> Result<BindOutcome, SeatError> {
    let bound = async {
        let spec = run_spec(pool, run_id, &row.engine_id).await?;
        bind_session(pool, &row.id, &spec, actor, None, true, None).await
    }
    .await;
    bound.map_err(|e| e.after_engine(run_id))
}

/// §9.2 `seats_fill`: a new run on the seat's engine, in the project root,
/// bound after the engine call returns. The previous session is unseated.
// The WP-70 bridge and the tests; the Tauri commands take the mutex first
// and call the `_locked` variant.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn fill_core<F, Fut>(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    prompt: String,
    actor: &SeatActor,
    persistent: bool,
    engine: F,
) -> Result<(SeatFillResult, Effects), SeatError>
where
    F: FnOnce(EngineCall) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let _guard = store().lock_one(seat_id).await;
    fill_locked(pool, world, seat_id, prompt, actor, persistent, engine).await
}

/// `fill_core` for a caller that already holds the seat's mutex.
pub(crate) async fn fill_locked<F, Fut>(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    prompt: String,
    actor: &SeatActor,
    persistent: bool,
    engine: F,
) -> Result<(SeatFillResult, Effects), SeatError>
where
    F: FnOnce(EngineCall) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    if let Err(r) = hold_gate(&row, actor, now, actor.hold) {
        return Err(refuse(pool, &row.id, r).await);
    }
    let chi = load_chi_for(pool, &row).await?;
    let previous = derive(&row, chi.as_ref(), world).session;
    let root = project_root(pool, &row.project_id).await?;
    let run_id = engine(EngineCall::Start {
        engine_id: row.engine_id.clone(),
        prompt,
        cwd: root,
        resume_session_id: None,
        persistent,
    })
    .await
    .map_err(SeatError::engine_failed)?;
    let out = bind_after_engine(pool, &row, &run_id, actor).await?;
    let seat = view_of(pool, world, out.dest).await?;
    Ok((
        SeatFillResult {
            seat,
            run_id,
            previous,
        },
        out.effects,
    ))
}

/// §4.5: park one text for a seat's run; the poller sends it when the run
/// leaves `queued` / `running`.
pub(crate) async fn queue_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    prompt: String,
    actor: &SeatActor,
) -> Result<(SeatView, Effects), SeatError> {
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    let change = match hold_gate(&row, actor, now, actor.hold) {
        Ok(c) => c,
        Err(r) => return Err(refuse(pool, &row.id, r).await),
    };
    if row.session_kind.as_deref() != Some("run") {
        return Err(SeatError::conflict(format!(
            "{} has no run to queue a text for",
            row.name
        )));
    }
    if store().queued(&row.id).is_some() {
        return Err(SeatError::busy(&row.name));
    }
    let mut tx = begin_write(pool).await?;
    let written: Result<Option<&'static str>, SeatError> = async {
        sqlx::query("UPDATE iyke_seats SET last_active_at = ? WHERE id = ?")
            .bind(now)
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        apply_hold(&mut tx, &row.id, &change, actor, now).await
    }
    .await;
    let kind = match written {
        Ok(k) => k,
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e);
        }
    };
    tx.commit().await.map_err(db_err)?;
    // Under the seat mutex, so the slot can't have filled since the check.
    store().enqueue(
        &row.id,
        QueuedText {
            prompt,
            since: now,
            client: actor.client.clone(),
        },
    );
    let mut effects = Effects::default();
    effects
        .events
        .push(changed(&row, with_hold_kind(vec!["updated"], kind), None));
    let seat = view_by_id(pool, world, &row.id).await?;
    Ok((seat, effects))
}

/// §4.2 DEC-69b: Clear drops the session pointer and nothing else — never the
/// pad, todos, kv, locks, timers, the agent row or the inbox.
pub(crate) async fn clear_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    actor: &SeatActor,
) -> Result<(SeatView, Effects), SeatError> {
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    let change = match hold_gate(&row, actor, now, actor.hold) {
        Ok(c) => c,
        Err(r) => return Err(refuse(pool, &row.id, r).await),
    };
    let mut tx = begin_write(pool).await?;
    let written: Result<Option<&'static str>, SeatError> = async {
        sqlx::query(
            "UPDATE iyke_seats SET session_kind = NULL, session_ref = NULL, external_id = NULL,
                    session_cwd = NULL, last_active_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(row.id.clone())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        apply_hold(&mut tx, &row.id, &change, actor, now).await
    }
    .await;
    let kind = match written {
        Ok(k) => k,
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e);
        }
    };
    tx.commit().await.map_err(db_err)?;
    store().drop_claim(&row.id);
    let dropped = store().dequeue(&row.id);
    if let Some(q) = &dropped {
        tracing::warn!(
            target: "ikenga::seats",
            "cleared {}: dropped the text {} queued for its run",
            row.address(),
            q.client
        );
    }
    let mut effects = Effects::default();
    let ev = changed(&row, with_hold_kind(vec!["cleared"], kind), None);
    effects.events.push(if dropped.is_some() {
        with_queue_dropped(ev, "cleared")
    } else {
        ev
    });
    let seat = view_by_id(pool, world, &row.id).await?;
    Ok((seat, effects))
}

/// §3.4: rename rewrites the seat, its agent row and `scope` in all five
/// scope-keyed tables, in one transaction; refuses on any row already under
/// the new address. Watchers of every moved pad wake under both keys.
pub(crate) async fn rename_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    new_name: &str,
    actor: &SeatActor,
) -> Result<(SeatView, Effects), SeatError> {
    validate_seat_name(new_name).map_err(SeatError::invalid_seat_name)?;
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    let change = match hold_gate(&row, actor, now, actor.hold) {
        Ok(c) => c,
        Err(r) => return Err(refuse(pool, &row.id, r).await),
    };
    if row.name == new_name && change == HoldChange::Keep {
        let seat = view_of(pool, world, row).await?;
        return Ok((seat, Effects::default()));
    }
    let old_scope = row.address();
    let new_scope = seat_address(&row.project_id, new_name);

    let mut tx = begin_write(pool).await?;
    let written: Result<(Option<&'static str>, Vec<(String, i64)>), SeatError> = async {
        if row.name != new_name {
            let taken: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM iyke_seats WHERE project_id = ? AND name = ? AND id <> ?",
            )
            .bind(row.project_id.clone())
            .bind(new_name.to_string())
            .bind(row.id.clone())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;
            if taken.is_some() {
                return Err(SeatError::name_taken(new_name));
            }
            for table in SCOPE_TABLES {
                let sql = format!("SELECT COUNT(*) FROM {table} WHERE scope = ?");
                let n: i64 = sqlx::query_scalar(&sql)
                    .bind(new_scope.clone())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db_err)?;
                if n > 0 {
                    return Err(SeatError::new(
                        "rename_scope_conflict",
                        format!("{new_scope} already has memory in {table}; rename refused"),
                    ));
                }
            }
            let renamed =
                sqlx::query("UPDATE iyke_seats SET name = ?, last_active_at = ? WHERE id = ?")
                    .bind(new_name.to_string())
                    .bind(now)
                    .bind(row.id.clone())
                    .execute(&mut *tx)
                    .await;
            if let Err(e) = renamed {
                if e.to_string().contains("iyke_seats.name") {
                    return Err(SeatError::name_taken(new_name));
                }
                return Err(db_err(e));
            }
            sqlx::query("UPDATE iyke_agents SET name = ? WHERE id = ?")
                .bind(new_scope.clone())
                .bind(row.id.clone())
                .execute(&mut *tx)
                .await
                .map_err(db_err)?;
            // Pads move with a version bump, so watchers see the change.
            sqlx::query(
                "UPDATE iyke_scratchpads SET scope = ?, updated_at = MAX(updated_at + 1, ?)
                 WHERE scope = ?",
            )
            .bind(new_scope.clone())
            .bind(now)
            .bind(old_scope.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            for table in &SCOPE_TABLES[1..] {
                let sql = format!("UPDATE {table} SET scope = ? WHERE scope = ?");
                sqlx::query(&sql)
                    .bind(new_scope.clone())
                    .bind(old_scope.clone())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        let pads: Vec<(String, i64)> = if row.name != new_name {
            sqlx::query_as("SELECT name, updated_at FROM iyke_scratchpads WHERE scope = ?")
                .bind(new_scope.clone())
                .fetch_all(&mut *tx)
                .await
                .map_err(db_err)?
        } else {
            Vec::new()
        };
        let kind = apply_hold(&mut tx, &row.id, &change, actor, now).await?;
        Ok((kind, pads))
    }
    .await;
    let (kind, pads) = match written {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e);
        }
    };
    tx.commit().await.map_err(db_err)?;

    let mut effects = Effects::default();
    for (name, version) in pads {
        effects.pads.push(PadChange {
            scope: old_scope.clone(),
            name: name.clone(),
            version,
            deleted: true,
        });
        effects.pads.push(PadChange {
            scope: new_scope.clone(),
            name,
            version,
            deleted: false,
        });
    }
    let base = if row.name != new_name {
        vec!["renamed"]
    } else {
        Vec::new()
    };
    effects
        .events
        .push(changed(&row, with_hold_kind(base, kind), None));
    let seat = view_by_id(pool, world, &row.id).await?;
    Ok((seat, effects))
}

/// §4.2 Remove: the seat row and its agent row (and inbox) go; with
/// `remove_memory` (P-6) so does everything under its scope. A running
/// session keeps running, unseated.
pub(crate) async fn remove_core(
    pool: &SqlitePool,
    seat_id: &str,
    remove_memory: bool,
    actor: &SeatActor,
) -> Result<(SeatRemoveResult, Effects), SeatError> {
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    if let Err(r) = hold_gate(&row, actor, now, false) {
        return Err(refuse(pool, &row.id, r).await);
    }
    let scope = row.address();
    let mut tx = begin_write(pool).await?;
    let written: Result<Vec<(String, i64)>, SeatError> = async {
        let mut pads: Vec<(String, i64)> = Vec::new();
        if remove_memory {
            pads = sqlx::query_as("SELECT name, updated_at FROM iyke_scratchpads WHERE scope = ?")
                .bind(scope.clone())
                .fetch_all(&mut *tx)
                .await
                .map_err(db_err)?;
            // Not relying on FK enforcement: comments and blockers first.
            sqlx::query(
                "DELETE FROM iyke_todo_comments
                 WHERE todo_id IN (SELECT id FROM iyke_todos WHERE scope = ?)",
            )
            .bind(scope.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "UPDATE iyke_todos SET blocker_id = NULL
                 WHERE scope <> ? AND blocker_id IN (SELECT id FROM iyke_todos WHERE scope = ?)",
            )
            .bind(scope.clone())
            .bind(scope.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
            for table in SCOPE_TABLES {
                let sql = format!("DELETE FROM {table} WHERE scope = ?");
                sqlx::query(&sql)
                    .bind(scope.clone())
                    .execute(&mut *tx)
                    .await
                    .map_err(db_err)?;
            }
        }
        sqlx::query("DELETE FROM iyke_agent_inbox WHERE agent_id = ?")
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("UPDATE iyke_timers SET agent_id = NULL WHERE agent_id = ?")
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM iyke_agents WHERE id = ?")
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        sqlx::query("DELETE FROM iyke_seats WHERE id = ?")
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        Ok(pads)
    }
    .await;
    let pads = match written {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e);
        }
    };
    tx.commit().await.map_err(db_err)?;

    store().drop_claim(&row.id);
    let dropped = store().dequeue(&row.id).is_some();
    store().forget_lock(&row.id);
    let mut effects = Effects::default();
    for (name, updated_at) in pads {
        let version = now.max(updated_at + 1);
        effects.pads.push(PadChange {
            scope: scope.clone(),
            name,
            version,
            deleted: true,
        });
    }
    let ev = changed(&row, vec!["removed"], None);
    effects.events.push(if dropped {
        with_queue_dropped(ev, "removed")
    } else {
        ev
    });
    Ok((SeatRemoveResult { seat_id: row.id }, effects))
}

/// §5.1: drop the caller's own hold; someone else's needs `takeover`.
pub(crate) async fn release_core(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    seat_id: &str,
    actor: &SeatActor,
) -> Result<(SeatView, Effects), SeatError> {
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let now = now_ms();
    // Displaced notice, and another client's live hold without takeover.
    let change = match hold_gate(&row, actor, now, false) {
        Ok(c) => c,
        Err(r) => return Err(refuse(pool, &row.id, r).await),
    };
    if row.hold_client.is_none() {
        let seat = view_of(pool, world, row).await?;
        return Ok((seat, Effects::default()));
    }
    let mut tx = begin_write(pool).await?;
    let written: Result<(), SeatError> = async {
        if let HoldChange::TakeOver { from } = &change {
            sqlx::query(
                "UPDATE iyke_seats SET displaced_client = ?, displaced_by = ?, displaced_at = ?
                 WHERE id = ?",
            )
            .bind(from.clone())
            .bind(actor.client.clone())
            .bind(now)
            .bind(row.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        sqlx::query(
            "UPDATE iyke_seats SET hold_client = NULL, hold_since = NULL, hold_expires_at = NULL,
                    last_active_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(row.id.clone())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        Ok(())
    }
    .await;
    if let Err(e) = written {
        let _ = tx.rollback().await;
        return Err(e);
    }
    tx.commit().await.map_err(db_err)?;
    let kinds = if matches!(change, HoldChange::TakeOver { .. }) {
        vec!["taken-over", "released"]
    } else {
        vec!["released"]
    };
    let mut effects = Effects::default();
    effects.events.push(changed(&row, kinds, None));
    let seat = view_by_id(pool, world, &row.id).await?;
    Ok((seat, effects))
}

/// §2.5: a `SessionStart` on a seated terminal records its resume id.
///
/// The conversation may still be recorded on another seat (an ended terminal
/// that held it, now resumed here): DEC-69c — resuming a past session moves
/// it — so that seat is unbound in the same transaction, as a §4.3 move
/// would. The capture is no client's write, so it never overrides a hold: if
/// that other seat is held, the capture is skipped (logged) and both seats
/// keep what they had. Returns one event per affected seat (§10).
pub(crate) async fn capture_core(
    pool: &SqlitePool,
    terminal_id: &str,
    session_id: &str,
) -> Result<Vec<SeatsChangedEvent>, SeatError> {
    let Some(found) = fetch_seat_by_terminal(pool, terminal_id).await? else {
        return Ok(Vec::new());
    };
    let _guard = store().lock_one(&found.id).await;
    let Some(row) = fetch_seat(pool, &found.id).await? else {
        return Ok(Vec::new());
    };
    if row.session_kind.as_deref() != Some("terminal")
        || row.session_ref.as_deref() != Some(terminal_id)
        || row.external_id.as_deref() == Some(session_id)
    {
        return Ok(Vec::new());
    }
    let spec = BindSpec {
        kind: "terminal",
        session_ref: terminal_id.to_string(),
        engine_id: row.engine_id.clone(),
        external_id: Some(session_id.to_string()),
        cwd: row.session_cwd.clone(),
    };
    // Lock the other seats with a bounded wait (the destination's mutex is
    // already held, as in resume / fill).
    let mut others: BTreeSet<String> = fetch_conflicting(pool, &row.id, &spec)
        .await?
        .into_iter()
        .map(|c| c.id)
        .collect();
    others.remove(&row.id);
    let mut _guards = Vec::with_capacity(others.len());
    for id in &others {
        let lock = store().lock_for(id);
        match tokio::time::timeout(EXTRA_LOCK_WAIT, lock.lock_owned()).await {
            Ok(g) => _guards.push(g),
            Err(_) => {
                return Err(SeatError::conflict(
                    "another change to a seat holding this conversation is in progress",
                ))
            }
        }
    }

    let now = now_ms();
    let mut tx = begin_write(pool).await?;
    let conflicts = match fetch_conflicting(&mut *tx, &row.id, &spec).await {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.rollback().await;
            return Err(e);
        }
    };
    if conflicts.iter().any(|c| !others.contains(&c.id)) {
        let _ = tx.rollback().await;
        return Err(SeatError::conflict(
            "the seats holding this conversation changed during the capture",
        ));
    }
    if let Some(held) = conflicts.iter().find(|c| c.live_hold(now).is_some()) {
        let _ = tx.rollback().await;
        tracing::warn!(
            target: "ikenga::seats",
            "resume id {session_id} for {} not recorded: {} holds that conversation and is held",
            row.address(),
            held.address()
        );
        return Ok(Vec::new());
    }
    let written: Result<(), SeatError> = async {
        for c in &conflicts {
            sqlx::query(
                "UPDATE iyke_seats SET session_kind = NULL, session_ref = NULL,
                        external_id = NULL, session_cwd = NULL
                 WHERE id = ?",
            )
            .bind(c.id.clone())
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;
        }
        sqlx::query(
            "UPDATE iyke_seats SET external_id = ?
             WHERE id = ? AND session_kind = 'terminal' AND session_ref = ?",
        )
        .bind(session_id.to_string())
        .bind(row.id.clone())
        .bind(terminal_id.to_string())
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
        Ok(())
    }
    .await;
    if let Err(e) = written {
        let _ = tx.rollback().await;
        return Err(e);
    }
    tx.commit().await.map_err(db_err)?;
    let from_seat_ids: Vec<String> = conflicts.iter().map(|c| c.id.clone()).collect();
    let mut events = vec![changed(
        &row,
        vec!["updated"],
        (!from_seat_ids.is_empty()).then(|| from_seat_ids.clone()),
    )];
    for c in &conflicts {
        events.push(changed(c, vec!["unbound"], None));
    }
    Ok(events)
}

/// After a queued text went out: bump `last_active_at` and copy the run's
/// current `external_id` / `cwd` from `chi_cache` (§1.1).
/// Also the WP-70 bridge's idle-run send, through `send_idle_core`.
pub(crate) async fn touch_after_send(
    pool: &SqlitePool,
    seat_id: &str,
    now: i64,
) -> Result<(), SeatError> {
    let refreshed = sqlx::query(
        "UPDATE iyke_seats SET last_active_at = ?,
                external_id = COALESCE(
                    (SELECT external_id FROM chi_cache WHERE run_id = iyke_seats.session_ref),
                    external_id),
                session_cwd = COALESCE(
                    (SELECT cwd FROM chi_cache WHERE run_id = iyke_seats.session_ref),
                    session_cwd)
         WHERE id = ? AND session_kind = 'run'",
    )
    .bind(now)
    .bind(seat_id.to_string())
    .execute(pool)
    .await;
    if refreshed.is_err() {
        // The conversation index refused the new id (another seat holds it):
        // keep the old pointer, still record the activity.
        sqlx::query("UPDATE iyke_seats SET last_active_at = ? WHERE id = ?")
            .bind(now)
            .bind(seat_id.to_string())
            .execute(pool)
            .await
            .map_err(db_err)?;
    }
    Ok(())
}

/// §1.1: a send through the seat bumps `last_active_at` and nothing else.
/// WP-70: the bridge's pty-route send, which has no run to refresh from.
pub(crate) async fn touch_last_active(
    pool: &SqlitePool,
    seat_id: &str,
    now: i64,
) -> Result<(), SeatError> {
    sqlx::query("UPDATE iyke_seats SET last_active_at = ? WHERE id = ?")
        .bind(now)
        .bind(seat_id.to_string())
        .execute(pool)
        .await
        .map_err(db_err)?;
    Ok(())
}

/// What the WP-70 bridge's idle-run send did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum IdleSend {
    /// The text went out through the `chi_resume` core; the run it continued.
    Sent { run_id: String },
    /// Since `seats_resolve`, the run went busy or a text was queued ahead of
    /// this one: nothing was sent, and the caller queues it (§4.5).
    Queue,
}

/// WP-70 `/iyke/seats/send`, idle-run route. Under the seat's mutex, re-checks
/// what `seats_resolve` saw — the caller's hold, the seat still on `run_id`,
/// the run not in flight, the §4.5 slot empty — and only then calls `send`
/// (the `chi_resume` core), so two concurrent sends never start two turns on
/// one run and a direct send never overtakes a queued text (§4.5). A busy run
/// or a full slot answers `Queue`; the caller then calls `seats_queue` (which
/// takes the mutex itself). After a send, §1.1's bump and §10's `updated`.
pub(crate) async fn send_idle_core<F, Fut>(
    pool: &SqlitePool,
    seat_id: &str,
    run_id: &str,
    actor: &SeatActor,
    send: F,
) -> Result<(IdleSend, Effects), SeatError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, SeatError>>,
{
    let _guard = store().lock_one(seat_id).await;
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    // `seats_resolve` already applied the hold change; this only refuses a
    // hold or takeover that landed since.
    match hold_gate(&row, actor, now_ms(), false) {
        Err(r) => return Err(refuse(pool, &row.id, r).await),
        Ok(HoldChange::TakeOver { from }) => {
            return Err(SeatError::conflict(format!(
                "{from} took {} while sending; nothing was sent — retry",
                row.name
            )))
        }
        Ok(_) => {}
    }
    if row.session_kind.as_deref() != Some("run") || row.session_ref.as_deref() != Some(run_id) {
        return Err(SeatError::conflict(format!(
            "{} changed while sending; nothing was sent — retry",
            row.name
        )));
    }
    let busy = match fetch_chi(pool, run_id).await? {
        Some(c) => matches!(c.status.as_str(), "queued" | "running"),
        None => {
            return Err(SeatError::conflict(format!(
                "run {run_id} of {} is gone; nothing was sent",
                row.name
            )))
        }
    };
    if busy || store().queued(&row.id).is_some() {
        return Ok((IdleSend::Queue, Effects::default()));
    }
    let sent = send().await?;
    let mut effects = Effects::default();
    // The text is already out, so a failed bump is only logged.
    match touch_after_send(pool, &row.id, now_ms()).await {
        Ok(()) => effects.events.push(changed(&row, vec!["updated"], None)),
        Err(e) => tracing::warn!(target: "ikenga::seats", "send: touch {}: {e}", row.id),
    }
    Ok((IdleSend::Sent { run_id: sent }, effects))
}

/// §4.5, one seat of the queue poller: send its queued text once its run
/// leaves `queued` / `running`, through `engine` (the `chi_resume` core).
/// Takes the seat's mutex. Returns the §10 event to emit, if any: `updated`
/// after a send, with E-4's `queue-dropped` when the text was dropped.
/// The desktop's `drain_queue` and the daemon's poller both loop over this.
pub(crate) async fn drain_one<F, Fut>(
    pool: &SqlitePool,
    id: &str,
    engine: F,
) -> Option<SeatsChangedEvent>
where
    F: FnOnce(EngineCall) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let _guard = store().lock_one(id).await;
    let queued = store().queued(id)?;
    let row = match fetch_seat(pool, id).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            store().dequeue(id);
            return None;
        }
        Err(e) => {
            tracing::warn!(target: "ikenga::seats", "queue: read seat {id}: {e}");
            return None;
        }
    };
    let run_id = match (row.session_kind.as_deref(), row.session_ref.clone()) {
        (Some("run"), Some(run_id)) => run_id,
        _ => {
            store().dequeue(id);
            tracing::warn!(
                target: "ikenga::seats",
                "queue: {} no longer holds a run; dropped the text {} queued",
                row.address(),
                queued.client
            );
            return Some(with_queue_dropped(
                changed(&row, vec!["updated"], None),
                "no_run",
            ));
        }
    };
    let chi = match fetch_chi(pool, &run_id).await {
        Ok(chi) => chi,
        Err(e) => {
            tracing::warn!(target: "ikenga::seats", "queue: read run {run_id}: {e}");
            return None;
        }
    };
    match chi {
        Some(c) if matches!(c.status.as_str(), "queued" | "running") => return None,
        Some(_) => {}
        None => {
            store().dequeue(id);
            tracing::warn!(
                target: "ikenga::seats",
                "queue: run {run_id} of {} is gone; dropped the queued text",
                row.address()
            );
            return Some(with_queue_dropped(
                changed(&row, vec!["updated"], None),
                "run_missing",
            ));
        }
    }
    store().dequeue(id);
    let sent = engine(EngineCall::ResumeRun {
        run_id: run_id.clone(),
        prompt: queued.prompt,
    })
    .await;
    let ev = changed(&row, vec!["updated"], None);
    Some(match sent {
        Ok(_) => {
            if let Err(e) = touch_after_send(pool, id, now_ms()).await {
                tracing::warn!(target: "ikenga::seats", "queue: touch {id}: {e}");
            }
            ev
        }
        Err(e) => {
            tracing::warn!(
                target: "ikenga::seats",
                "queue: send to {} (run {run_id}) failed: {e}",
                row.address()
            );
            with_queue_dropped(ev, "send_failed")
        }
    })
}
