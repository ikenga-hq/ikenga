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
//! Writes are the 13 `seats_*` Tauri commands (§9.2). Each runs under the
//! per-seat async mutex (§4.0), as one write-first transaction, and emits
//! `seats://changed` after commit — one event per affected seat (§10).
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

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, Sqlite, SqliteConnection, SqlitePool, Transaction};
use tauri::{AppHandle, Emitter, Listener, Manager, State};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

use crate::commands::chi::{
    openrouter_holds_thread, resume_chi_run, spawn_chi_run, ChiCache, ChiRunOpts, ChiRuntime,
};
use crate::commands::db::PaDb;
use crate::commands::projects::get_active_project_id;
use crate::iyke::hooks::HookPayload;
use crate::iyke::memory::scratchpad_changed;
use crate::iyke::state::IykeState;
use crate::iyke::terminal::enrich_terminals;
use crate::pty::{PtyManager, TerminalDescriptor};
use crate::window::registry::WindowRegistry;

// ═══════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════

/// App-wide event, one per affected seat, after commit (§10).
pub(crate) const SEATS_CHANGED_EVENT: &str = "seats://changed";
/// Re-emitted Claude hook events (`iyke/hooks.rs`); the liveness source (§2.5).
const HOOKS_EVENT: &str = "hooks://event";
/// Same event `memory.rs` emits on a scratchpad write; a rename moves pads.
const SCRATCHPAD_CHANGED_EVENT: &str = "iyke://scratchpad-changed";

/// P-4: hold TTL default 10 min, clamped 1 s – 60 min.
const HOLD_TTL_DEFAULT_MS: i64 = 600_000;
const HOLD_TTL_MIN_MS: i64 = 1_000;
const HOLD_TTL_MAX_MS: i64 = 3_600_000;
/// P-12: the §4.1 resume claim lives 30 s; the §4.5 queue polls every 2 s.
const CLAIM_TTL_MS: i64 = 30_000;
const QUEUE_POLL: Duration = Duration::from_secs(2);
/// A resume/fill already holds the destination's mutex across the engine
/// call; any further seat it must unbind is locked with this bound, so a
/// move holding that seat and waiting on the destination can't deadlock us.
const EXTRA_LOCK_WAIT: Duration = Duration::from_secs(5);


/// The only engine whose terminal agent reports turns (`SessionStart`,
/// `UserPromptSubmit`, `Stop`, `SessionEnd`) through the hooks bridge.
const CLAUDE_ENGINE: &str = "claude-code";
const OPENROUTER_ENGINE: &str = "openrouter";
/// `chi_cache.owner` for runs the seat store starts.
const SEAT_RUN_OWNER: &str = "seat";

/// The five scope-keyed memory tables (`0016_iyke_memory.sql`). A rename
/// rewrites `scope` in all of them; a Remove with `removeMemory` clears them.
const SCOPE_TABLES: [&str; 5] = [
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

const SEAT_BY_ID: &str = concat!("SELECT ", seat_cols!(), " FROM iyke_seats WHERE id = ?");
const SEAT_BY_NAME: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE project_id = ? AND name = ?"
);
const SEATS_IN_PROJECT: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE project_id = ? ORDER BY created_at, id"
);
const SEAT_BY_TERMINAL: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE session_kind = 'terminal' AND session_ref = ?"
);
/// §4.3: the seats a bind will unbind — same session ref, or the same engine
/// conversation reached through another ref.
const SEATS_CONFLICTING: &str = concat!(
    "SELECT ",
    seat_cols!(),
    " FROM iyke_seats WHERE id <> ? AND ((session_kind = ? AND session_ref = ?) \
     OR (? IS NOT NULL AND engine_id = ? AND external_id = ?)) ORDER BY id"
);

fn now_ms() -> i64 {
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
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The engine call already succeeded and the bind after it failed: the
    /// run exists with the caller's text but the seat didn't take it. Carry
    /// the run id (merged into `details`) so a caller never blindly resends.
    fn after_engine(mut self, run_id: &str) -> Self {
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

    fn invalid_seat_name(msg: String) -> Self {
        Self::new("invalid_seat_name", msg)
    }
    fn invalid_address(input: &str) -> Self {
        Self::new(
            "invalid_address",
            format!("not a seat address: {input:?} (use <name>, @<name>, <project>/<name> or seat:<project>/<name>)"),
        )
    }
    fn seat_not_found() -> Self {
        Self::new("seat_not_found", "that seat no longer exists")
    }
    fn project_not_found(project: &str) -> Self {
        Self::new(
            "project_not_found",
            format!("project {project:?} does not exist or is archived"),
        )
    }
    fn name_taken(name: &str) -> Self {
        Self::new("seat_name_taken", format!("{name} is already a seat"))
    }
    fn engine_mismatch(session_engine: &str, seat_engine: &str) -> Self {
        Self::new(
            "engine_mismatch",
            format!("a {session_engine} session can't sit in a {seat_engine} seat"),
        )
    }
    fn terminal_not_found(terminal: &str) -> Self {
        Self::new(
            "terminal_not_found",
            format!("terminal {terminal:?} is not running in this app"),
        )
    }
    fn not_vacant(name: &str) -> Self {
        Self::new("seat_not_vacant", format!("{name} is not vacant"))
    }
    fn resuming(name: &str) -> Self {
        Self::new("seat_resuming", format!("{name} is already being resumed"))
    }
    fn busy(name: &str) -> Self {
        Self::new(
            "seat_busy",
            format!("{name} already has a text queued for its run"),
        )
    }
    fn agent_not_live(name: &str) -> Self {
        Self::new(
            "agent_not_live",
            format!("the agent in @{name}'s terminal isn't running yet"),
        )
    }
    fn conflict(msg: impl Into<String>) -> Self {
        Self::new("conflict", msg)
    }
    fn held(name: &str, hold: &SeatHold) -> Self {
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
    fn taken_over(name: &str, by: &str, at: i64) -> Self {
        Self::new("seat_taken_over", format!("{by} took over {name} at {at}"))
            .with_details(json!({ "by": by, "at": at }))
    }
    fn not_resumable(reason: NotResumableReason) -> Self {
        Self::new(
            "not_resumable",
            format!("this seat's session can't be resumed ({})", reason.as_str()),
        )
        .with_details(json!({ "reason": reason }))
    }
    fn engine_unsupported(engine_id: &str) -> Self {
        Self::new(
            "engine_unsupported",
            format!("engine {engine_id:?} can't hold a seat"),
        )
        .with_details(json!({ "engine_id": engine_id }))
    }
    fn engine_failed(msg: String) -> Self {
        Self::new("engine_failed", msg)
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self::new("internal", msg)
    }
}

/// A database error. A unique-constraint violation is a `409 conflict`: the
/// indexes are the backstop for DEC-69c (§4.0) and nothing was changed.
fn db_err(e: sqlx::Error) -> SeatError {
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
    fn as_str(self) -> &'static str {
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
    fn yes() -> Self {
        Self {
            resumable: true,
            reason: None,
        }
    }
    fn no(reason: NotResumableReason) -> Self {
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
                Some("not installed".to_string())
            } else {
                None
            };
            SeatEngineInfo {
                engine_id: cap.engine_id,
                wrap_id: cap.wrap_id,
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
async fn binary_available(binary: &str) -> bool {
    static CACHE: std::sync::OnceLock<InstallCache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(available) = cache.fresh(binary, std::time::Instant::now()) {
        return available;
    }
    let outcome = crate::server::shared::chi_exec::engine_installed(binary).await;
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
struct Claim {
    token: String,
    expires_at: i64,
}

#[derive(Debug, Clone)]
struct QueuedText {
    prompt: String,
    since: i64,
    client: String,
}

#[derive(Default)]
struct SeatStore {
    locks: StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    agents: StdMutex<HashMap<String, AgentLive>>,
    claims: StdMutex<HashMap<String, Claim>>,
    queue: StdMutex<HashMap<String, QueuedText>>,
    installed: AtomicBool,
}

fn store() -> &'static SeatStore {
    static STORE: OnceLock<SeatStore> = OnceLock::new();
    STORE.get_or_init(SeatStore::default)
}

fn guard<T>(m: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SeatStore {
    fn lock_for(&self, seat_id: &str) -> Arc<AsyncMutex<()>> {
        guard(&self.locks)
            .entry(seat_id.to_string())
            .or_default()
            .clone()
    }

    async fn lock_one(&self, seat_id: &str) -> OwnedMutexGuard<()> {
        self.lock_for(seat_id).lock_owned().await
    }

    /// Lock every seat in `ids`, in ascending id order (§4.0).
    async fn lock_set(&self, ids: &BTreeSet<String>) -> Vec<OwnedMutexGuard<()>> {
        let mut guards = Vec::with_capacity(ids.len());
        for id in ids {
            guards.push(self.lock_for(id).lock_owned().await);
        }
        guards
    }

    fn forget_lock(&self, seat_id: &str) {
        guard(&self.locks).remove(seat_id);
    }

    fn agents_snapshot(&self) -> HashMap<String, AgentLive> {
        guard(&self.agents).clone()
    }

    fn live_claim(&self, seat_id: &str, now: i64) -> Option<Claim> {
        guard(&self.claims)
            .get(seat_id)
            .filter(|c| c.expires_at > now)
            .cloned()
    }

    fn take_claim(&self, seat_id: &str, now: i64) -> String {
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
    fn release_claim(&self, seat_id: &str, token: &str, now: i64) -> bool {
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
    fn drop_claim(&self, seat_id: &str) {
        guard(&self.claims).remove(seat_id);
    }

    fn queued(&self, seat_id: &str) -> Option<QueuedText> {
        guard(&self.queue).get(seat_id).cloned()
    }

    fn queued_ids(&self) -> Vec<String> {
        guard(&self.queue).keys().cloned().collect()
    }

    /// One slot per seat; false when it is already full.
    fn enqueue(&self, seat_id: &str, text: QueuedText) -> bool {
        let mut q = guard(&self.queue);
        if q.contains_key(seat_id) {
            return false;
        }
        q.insert(seat_id.to_string(), text);
        true
    }

    fn dequeue(&self, seat_id: &str) -> Option<QueuedText> {
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
/// command by `tauri_world`; tests build it by hand.
#[derive(Debug, Clone, Default)]
pub(crate) struct WorldSnapshot {
    pub terminals: Vec<TermLive>,
    pub agents: HashMap<String, AgentLive>,
    pub openrouter_registered: bool,
    /// thread id → does the adapter hold its transcript.
    pub openrouter_threads: HashMap<String, bool>,
    pub unavailable_engines: Vec<String>,
}

impl WorldSnapshot {
    /// A terminal by tab id (preferring a running PTY), then by PTY id.
    fn terminal(&self, id: &str) -> Option<&TermLive> {
        self.terminals
            .iter()
            .filter(|t| t.terminal_id == id)
            .max_by_key(|t| t.running)
            .or_else(|| self.terminals.iter().find(|t| t.pty_id == id))
    }

    fn engine_available(&self, engine_id: &str) -> bool {
        !self.unavailable_engines.iter().any(|e| e == engine_id)
    }

    /// `None` when the openrouter adapter isn't registered.
    fn openrouter_holds(&self, thread: &str) -> Option<bool> {
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
    fn live_agent(&self, t: &TermLive) -> Option<&AgentLive> {
        self.agents
            .get(&t.terminal_id)
            .filter(|a| a.pty_id.as_deref().map_or(true, |p| p == t.pty_id))
    }
}

fn mount_of(window_labels: &[String], pane_ids: &[String]) -> Option<SeatMount> {
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

fn term_live(d: TerminalDescriptor, now: u64) -> TermLive {
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
    fn address(&self) -> String {
        seat_address(&self.project_id, &self.name)
    }

    /// A hold is live while `expires_at > now`; an expired one is absent.
    fn live_hold(&self, now: i64) -> Option<SeatHold> {
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

fn seat_from_row(r: &SqliteRow) -> SeatRow {
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

async fn fetch_seat<'c, E>(ex: E, seat_id: &str) -> Result<Option<SeatRow>, SeatError>
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

async fn fetch_seat_by_name<'c, E>(
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

async fn fetch_seat_by_terminal<'c, E>(
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

async fn list_rows(pool: &SqlitePool, project_id: &str) -> Result<Vec<SeatRow>, SeatError> {
    let rows = sqlx::query(SEATS_IN_PROJECT)
        .bind(project_id.to_string())
        .fetch_all(pool)
        .await
        .map_err(db_err)?;
    Ok(rows.iter().map(seat_from_row).collect())
}

async fn fetch_conflicting<'c, E>(
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

async fn fetch_chi<'c, E>(ex: E, run_id: &str) -> Result<Option<ChiLite>, SeatError>
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

async fn load_chi_for(pool: &SqlitePool, row: &SeatRow) -> Result<Option<ChiLite>, SeatError> {
    match (row.session_kind.as_deref(), row.session_ref.as_deref()) {
        (Some("run"), Some(run_id)) => fetch_chi(pool, run_id).await,
        _ => Ok(None),
    }
}

/// `(root_path, archived_at)` of a project, if the row exists.
async fn project_row(
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

async fn project_root(pool: &SqlitePool, project_id: &str) -> Result<Option<String>, SeatError> {
    Ok(project_row(pool, project_id)
        .await?
        .and_then(|(root, _)| root)
        .filter(|r| !r.trim().is_empty()))
}

async fn active_project(pool: &SqlitePool) -> Result<String, SeatError> {
    get_active_project_id(pool)
        .await
        .map_err(SeatError::internal)
}

async fn resolve_address(pool: &SqlitePool, seat: &SeatAddress) -> Result<SeatRow, SeatError> {
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
async fn begin_write(pool: &SqlitePool) -> Result<Transaction<'static, Sqlite>, SeatError> {
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

fn vacant(resume: SeatResume, session: Option<SeatSession>) -> Derived {
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
fn resume_for(
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

fn derive_run(
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

fn derive_terminal(row: &SeatRow, terminal_id: &str, world: &WorldSnapshot) -> Derived {
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

async fn pad_summary(pool: &SqlitePool, address: &str) -> Result<SeatPad, SeatError> {
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

async fn build_view(
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

async fn view_of(
    pool: &SqlitePool,
    world: &WorldSnapshot,
    row: SeatRow,
) -> Result<SeatView, SeatError> {
    let chi = load_chi_for(pool, &row).await?;
    build_view(pool, world, row, chi.as_ref()).await
}

async fn view_by_id(
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

fn hold_ttl(actor: &SeatActor) -> i64 {
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

async fn apply_hold(
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
async fn refuse(pool: &SqlitePool, seat_id: &str, refusal: Refusal) -> SeatError {
    if refusal.clear_displaced {
        let res = sqlx::query(
            "UPDATE iyke_seats SET displaced_client = NULL, displaced_by = NULL, displaced_at = NULL
             WHERE id = ?",
        )
        .bind(seat_id.to_string())
        .execute(pool)
        .await;
        if let Err(e) = res {
            log::warn!(target: "ikenga::seats", "clear displaced notice on {seat_id}: {e}");
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

fn changed(
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
fn with_queue_dropped(mut ev: SeatsChangedEvent, reason: &'static str) -> SeatsChangedEvent {
    ev.kinds.push("queue-dropped");
    ev.queue_dropped = Some(reason);
    ev
}

fn with_hold_kind(
    mut kinds: Vec<&'static str>,
    hold_kind: Option<&'static str>,
) -> Vec<&'static str> {
    if let Some(k) = hold_kind {
        kinds.push(k);
    }
    kinds
}

fn emit_effects(app: &AppHandle, effects: Effects) {
    for event in effects.events {
        let _ = app.emit(SEATS_CHANGED_EVENT, &event);
    }
    for pad in effects.pads {
        let _ = app.emit(
            SCRATCHPAD_CHANGED_EVENT,
            json!({
                "scope": pad.scope,
                "name": pad.name,
                "action": if pad.deleted { "delete" } else { "write" },
                "updated_at": pad.version,
            }),
        );
    }
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

struct BindOutcome {
    dest: SeatRow,
    from_seat_ids: Vec<String>,
    claim_lost: bool,
    effects: Effects,
}

/// Insert a seat row and its agent row (§1.5), in the caller's transaction.
async fn insert_seat(
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
async fn bind_session(
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
async fn run_spec(
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
    let path_t = row.session_kind.as_deref() != Some("run")
        && engine_cap(&row.engine_id).and_then(|c| c.wrap_id).is_some();
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
async fn precheck_unbinds(
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
async fn bind_after_engine(
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
        log::warn!(
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
        scratchpad_changed(&old_scope, &name, version, true);
        scratchpad_changed(&new_scope, &name, version, false);
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
        scratchpad_changed(&scope, &name, version, true);
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
        log::warn!(
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
        Err(e) => log::warn!(target: "ikenga::seats", "send: touch {}: {e}", row.id),
    }
    Ok((IdleSend::Sent { run_id: sent }, effects))
}

// ═══════════════════════════════════════════════════════════════════════
// Tauri glue: the live world, engine calls, listener, poller
// ═══════════════════════════════════════════════════════════════════════

async fn pool_of(db: &PaDb) -> Result<SqlitePool, SeatError> {
    db.ensure_pool().await.map_err(SeatError::internal)
}

/// Re-derive a seat's view after a bind, with a world probed for the seat's
/// *new* session (a create or move builds its world before the destination
/// holds the session, so e.g. a bound openrouter run's thread was never
/// probed and a `done` run would read vacant instead of idle, §2.2).
async fn refreshed_view(
    app: &AppHandle,
    pool: &SqlitePool,
    seat_id: &str,
) -> Result<SeatView, SeatError> {
    let row = fetch_seat(pool, seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(app, pool, std::slice::from_ref(&row), false).await;
    view_of(pool, &world, row).await
}

async fn app_pool(app: &AppHandle) -> Result<SqlitePool, SeatError> {
    let db = app
        .try_state::<Arc<PaDb>>()
        .ok_or_else(|| SeatError::internal("the database is not ready"))?;
    pool_of(&db).await
}

/// WP-70 bridge: a PTY write through the seat went out (§7.2 pty route).
/// Bumps `last_active_at` (§1.1) and emits `updated` (§10).
pub(crate) async fn note_pty_send(app: &AppHandle, seat_id: &str) -> Result<(), SeatError> {
    let pool = app_pool(app).await?;
    touch_last_active(&pool, seat_id, now_ms()).await?;
    if let Some(row) = fetch_seat(&pool, seat_id).await? {
        let _ = app.emit(
            SEATS_CHANGED_EVENT,
            &changed(&row, vec!["updated"], None),
        );
    }
    Ok(())
}

/// WP-70 bridge glue for `send_idle_core`: emits its §10 events.
pub(crate) async fn send_idle<F, Fut>(
    app: &AppHandle,
    seat_id: &str,
    run_id: &str,
    actor: &SeatActor,
    send: F,
) -> Result<IdleSend, SeatError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, SeatError>>,
{
    let pool = app_pool(app).await?;
    let (outcome, effects) = send_idle_core(&pool, seat_id, run_id, actor, send).await?;
    emit_effects(app, effects);
    Ok(outcome)
}

/// Build the world derivation reads, for `rows` (openrouter threads and
/// engine install state are probed only for these seats, or for every engine
/// with `all_engines`).
async fn tauri_world(
    app: &AppHandle,
    pool: &SqlitePool,
    rows: &[SeatRow],
    all_engines: bool,
) -> WorldSnapshot {
    let mut world = WorldSnapshot {
        agents: store().agents_snapshot(),
        ..WorldSnapshot::default()
    };

    // Terminals, with `mount` from `enrich_terminals` (§1.6).
    let descriptors = app
        .try_state::<Arc<PtyManager>>()
        .map(|m| m.list_terminals());
    if let Some(mut descriptors) = descriptors {
        let iyke_state: Option<Arc<IykeState>> =
            app.try_state::<Arc<IykeState>>().map(|s| s.inner().clone());
        let panes = match iyke_state {
            Some(s) => s.snapshot().await.panes,
            None => None,
        };
        let windows = app
            .try_state::<WindowRegistry>()
            .map(|r| r.list_live(app))
            .unwrap_or_default();
        enrich_terminals(&mut descriptors, panes.as_ref(), &windows);
        let now = now_ms().max(0) as u64;
        world.terminals = descriptors.into_iter().map(|d| term_live(d, now)).collect();
    }

    // Engine install state.
    let mut engines: BTreeSet<String> = rows.iter().map(|r| r.engine_id.clone()).collect();
    if all_engines {
        engines.extend(ENGINE_CAPS.iter().map(|c| c.engine_id.to_string()));
    }
    // Probed concurrently: each may wait on a cold WSL start.
    let probes = engines.iter().filter_map(|engine| {
        let binary = engine_cap(engine).and_then(|c| c.binary)?;
        Some(async move { (engine.clone(), binary_available(binary).await) })
    });
    for (engine, available) in futures_util::future::join_all(probes).await {
        if !available {
            world.unavailable_engines.push(engine);
        }
    }

    // The openrouter adapter: registered, and which threads it still holds.
    if engines.contains(OPENROUTER_ENGINE) {
        let mut threads: BTreeSet<String> = BTreeSet::new();
        for row in rows.iter().filter(|r| r.engine_id == OPENROUTER_ENGINE) {
            if let Some(ext) = &row.external_id {
                threads.insert(ext.clone());
            }
            if let (Some("run"), Some(run_id)) =
                (row.session_kind.as_deref(), row.session_ref.as_deref())
            {
                threads.insert(run_id.to_string());
                if let Ok(Some(chi)) = fetch_chi(pool, run_id).await {
                    if let Some(ext) = chi.external_id {
                        threads.insert(ext);
                    }
                }
            }
        }
        world.openrouter_registered = openrouter_holds_thread(app, "").await.is_some();
        if world.openrouter_registered {
            for thread in threads {
                let held = openrouter_holds_thread(app, &thread).await.unwrap_or(false);
                world.openrouter_threads.insert(thread, held);
            }
        }
    }
    world
}

async fn call_engine(app: &AppHandle, call: EngineCall) -> Result<String, String> {
    let db: Arc<PaDb> = app
        .try_state::<Arc<PaDb>>()
        .ok_or("the database is not ready")?
        .inner()
        .clone();
    let cache: ChiCache = app
        .try_state::<ChiCache>()
        .ok_or("the chi cache is not ready")?
        .inner()
        .clone();
    let runtime: Arc<ChiRuntime> = app
        .try_state::<Arc<ChiRuntime>>()
        .ok_or("the chi runtime is not ready")?
        .inner()
        .clone();
    let result = match call {
        EngineCall::ResumeRun { run_id, prompt } => {
            resume_chi_run(app, db, &cache, &runtime, run_id, prompt).await?
        }
        EngineCall::Start {
            engine_id,
            prompt,
            cwd,
            resume_session_id,
            persistent,
        } => {
            let opts = ChiRunOpts {
                engine_id,
                prompt,
                cwd,
                model: None,
                mode: None,
                timeout_seconds: None,
                parent_id: None,
                resume_session_id,
                persistent,
            };
            spawn_chi_run(db, &cache, &runtime, Some(app), opts, SEAT_RUN_OWNER).await?
        }
    };
    Ok(result.run_id)
}

/// Install the §2.5 `hooks://event` listener and the §4.5 queue poller.
/// Idempotent; called once from `iyke::start`.
pub(crate) fn install(app: &AppHandle) {
    if store().installed.swap(true, Ordering::SeqCst) {
        return;
    }
    let app_for_hooks = app.clone();
    let _ = app.listen(HOOKS_EVENT, move |event| {
        match serde_json::from_str::<HookPayload>(event.payload()) {
            Ok(payload) => on_hook_event(&app_for_hooks, payload),
            Err(e) => log::debug!(target: "ikenga::seats", "hooks://event parse: {e}"),
        }
    });
    let app_for_queue = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(QUEUE_POLL).await;
            drain_queue(&app_for_queue).await;
        }
    });
}

/// `(terminal_id, pty_id)` of the running PTY for a hook's terminal id.
fn current_terminal(app: &AppHandle, id: &str) -> Option<(String, String)> {
    let manager = app.try_state::<Arc<PtyManager>>()?;
    manager
        .list_terminals()
        .into_iter()
        .filter(|d| d.status == "running" && (d.terminal_id == id || d.pty_id == id))
        .max_by_key(|d| d.created_at)
        .map(|d| (d.terminal_id, d.pty_id))
}

fn on_hook_event(app: &AppHandle, payload: HookPayload) {
    let Some(raw_id) = payload.ikenga_terminal_id.clone().filter(|t| !t.is_empty()) else {
        return;
    };
    let Some(event) = payload.hook_event_name.as_deref() else {
        return;
    };
    if !matches!(
        event,
        "SessionStart" | "UserPromptSubmit" | "Stop" | "SessionEnd"
    ) {
        return;
    }
    let (terminal_id, pty_id) = match current_terminal(app, &raw_id) {
        Some((t, p)) => (t, Some(p)),
        None => (raw_id, None),
    };
    let session_id = payload.session_id.clone().filter(|s| !s.is_empty());
    {
        let mut agents = guard(&store().agents);
        apply_hook(&mut agents, &terminal_id, event, session_id.clone(), pty_id);
    }
    if event == "SessionStart" {
        if let Some(session_id) = session_id {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let Some(db) = app.try_state::<Arc<PaDb>>().map(|s| s.inner().clone()) else {
                    return;
                };
                let Ok(pool) = db.ensure_pool().await else {
                    return;
                };
                match capture_core(&pool, &terminal_id, &session_id).await {
                    Ok(events) => {
                        for event in events {
                            let _ = app.emit(SEATS_CHANGED_EVENT, &event);
                        }
                    }
                    Err(e) => log::warn!(
                        target: "ikenga::seats",
                        "record resume id for terminal {terminal_id}: {e}"
                    ),
                }
            });
        }
    }
}

/// §4.5: send each queued text once its run leaves `queued` / `running`.
async fn drain_queue(app: &AppHandle) {
    let ids = store().queued_ids();
    if ids.is_empty() {
        return;
    }
    let Some(db) = app.try_state::<Arc<PaDb>>().map(|s| s.inner().clone()) else {
        return;
    };
    let Ok(pool) = db.ensure_pool().await else {
        return;
    };
    for id in ids {
        let _guard = store().lock_one(&id).await;
        let Some(queued) = store().queued(&id) else {
            continue;
        };
        let row = match fetch_seat(&pool, &id).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                store().dequeue(&id);
                continue;
            }
            Err(e) => {
                log::warn!(target: "ikenga::seats", "queue: read seat {id}: {e}");
                continue;
            }
        };
        let run_id = match (row.session_kind.as_deref(), row.session_ref.clone()) {
            (Some("run"), Some(run_id)) => run_id,
            _ => {
                store().dequeue(&id);
                log::warn!(
                    target: "ikenga::seats",
                    "queue: {} no longer holds a run; dropped the text {} queued",
                    row.address(),
                    queued.client
                );
                let _ = app.emit(
                    SEATS_CHANGED_EVENT,
                    &with_queue_dropped(changed(&row, vec!["updated"], None), "no_run"),
                );
                continue;
            }
        };
        let chi = match fetch_chi(&pool, &run_id).await {
            Ok(chi) => chi,
            Err(e) => {
                log::warn!(target: "ikenga::seats", "queue: read run {run_id}: {e}");
                continue;
            }
        };
        match chi {
            Some(c) if matches!(c.status.as_str(), "queued" | "running") => continue,
            Some(_) => {}
            None => {
                store().dequeue(&id);
                log::warn!(
                    target: "ikenga::seats",
                    "queue: run {run_id} of {} is gone; dropped the queued text",
                    row.address()
                );
                let _ = app.emit(
                    SEATS_CHANGED_EVENT,
                    &with_queue_dropped(changed(&row, vec!["updated"], None), "run_missing"),
                );
                continue;
            }
        }
        store().dequeue(&id);
        let sent = call_engine(
            app,
            EngineCall::ResumeRun {
                run_id: run_id.clone(),
                prompt: queued.prompt,
            },
        )
        .await;
        let ev = changed(&row, vec!["updated"], None);
        let ev = match sent {
            Ok(_) => {
                if let Err(e) = touch_after_send(&pool, &id, now_ms()).await {
                    log::warn!(target: "ikenga::seats", "queue: touch {id}: {e}");
                }
                ev
            }
            Err(e) => {
                log::warn!(
                    target: "ikenga::seats",
                    "queue: send to {} (run {run_id}) failed: {e}",
                    row.address()
                );
                with_queue_dropped(ev, "send_failed")
            }
        };
        let _ = app.emit(SEATS_CHANGED_EVENT, &ev);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Tauri commands (§9.2) — frozen names and signatures
// ═══════════════════════════════════════════════════════════════════════

/// Views of every seat in `projectId` (default: the active project), ordered
/// by `created_at`. Never writes.
#[tauri::command]
pub async fn seats_list(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    project_id: Option<String>,
) -> Result<Vec<SeatView>, SeatError> {
    let pool = pool_of(&db).await?;
    let project = match project_id.filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => active_project(&pool).await?,
    };
    let rows = list_rows(&pool, &project).await?;
    let world = tauri_world(&app, &pool, &rows, false).await;
    list_core(&pool, &world, rows).await
}

/// One view, by id or by any §1.3 address.
#[tauri::command]
pub async fn seats_get(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat: SeatAddress,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = resolve_address(&pool, &seat).await?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    view_of(&pool, &world, row).await
}

/// §6.1 with install state, for the create form.
#[tauri::command]
pub async fn seats_engines(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
) -> Result<Vec<SeatEngineInfo>, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], true).await;
    Ok(engines_info(&world))
}

#[tauri::command]
pub async fn seats_resolve(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat: SeatAddress,
    actor: SeatActor,
    opts: Option<ResolveOpts>,
) -> Result<SeatRoute, SeatError> {
    let pool = pool_of(&db).await?;
    let row = resolve_address(&pool, &seat).await?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let claim_resume = opts.map(|o| o.claim_resume).unwrap_or(false);
    let (route, effects) = resolve_core(&pool, &world, &row.id, &actor, claim_resume).await?;
    emit_effects(&app, effects);
    Ok(route)
}

#[tauri::command]
pub async fn seats_create(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    req: CreateSeatReq,
    actor: SeatActor,
) -> Result<SeatMoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], false).await;
    let (mut result, effects) = create_core(&pool, &world, req, &actor).await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    let seat_id = result.seat.id.clone();
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_move(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    session: SeatSessionRef,
    to_seat_id: String,
    actor: SeatActor,
    opts: Option<MoveOpts>,
) -> Result<SeatMoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let world = tauri_world(&app, &pool, &[], false).await;
    let claim = opts.and_then(|o| o.claim);
    let (mut result, effects) = move_core(
        &pool,
        &world,
        &session,
        &to_seat_id,
        &actor,
        claim.as_deref(),
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    let seat_id = result.seat.id.clone();
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_resume(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
    opts: ResumeOpts,
) -> Result<SeatResumeResult, SeatError> {
    let pool = pool_of(&db).await?;
    // Take the seat's mutex before building the world, so the vacancy check
    // reads liveness from after any resume that raced this one (§4.1).
    let _guard = store().lock_one(&seat_id).await;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let engine_app = app.clone();
    let (mut result, effects) = resume_locked(
        &pool,
        &world,
        &seat_id,
        prompt,
        &actor,
        opts.fallback,
        move |call| async move { call_engine(&engine_app, call).await },
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_fill(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
    opts: Option<FillOpts>,
) -> Result<SeatFillResult, SeatError> {
    let pool = pool_of(&db).await?;
    let _guard = store().lock_one(&seat_id).await;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let persistent = opts.map(|o| o.persistent).unwrap_or(false);
    let engine_app = app.clone();
    let (mut result, effects) = fill_locked(
        &pool,
        &world,
        &seat_id,
        prompt,
        &actor,
        persistent,
        move |call| async move { call_engine(&engine_app, call).await },
    )
    .await?;
    emit_effects(&app, effects);
    // The write committed: a failed re-read keeps the core's view.
    if let Ok(view) = refreshed_view(&app, &pool, &seat_id).await {
        result.seat = view;
    }
    Ok(result)
}

#[tauri::command]
pub async fn seats_queue(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    prompt: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = queue_core(&pool, &world, &seat_id, prompt, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_clear(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = clear_core(&pool, &world, &seat_id, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_rename(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    name: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = rename_core(&pool, &world, &seat_id, &name, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

#[tauri::command]
pub async fn seats_remove(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    opts: RemoveOpts,
    actor: SeatActor,
) -> Result<SeatRemoveResult, SeatError> {
    let pool = pool_of(&db).await?;
    let (result, effects) = remove_core(&pool, &seat_id, opts.remove_memory, &actor).await?;
    emit_effects(&app, effects);
    Ok(result)
}

#[tauri::command]
pub async fn seats_release(
    app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    seat_id: String,
    actor: SeatActor,
) -> Result<SeatView, SeatError> {
    let pool = pool_of(&db).await?;
    let row = fetch_seat(&pool, &seat_id)
        .await?
        .ok_or_else(SeatError::seat_not_found)?;
    let world = tauri_world(&app, &pool, std::slice::from_ref(&row), false).await;
    let (seat, effects) = release_core(&pool, &world, &seat_id, &actor).await?;
    emit_effects(&app, effects);
    Ok(seat)
}

// ═══════════════════════════════════════════════════════════════════════
// Tests — written under DEC-50 (not run before CI).
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    // ── fixtures ────────────────────────────────────────────────────────

    /// Comment-stripping `;` splitter for the migration files used here.
    fn split_sql(sql: &str) -> Vec<String> {
        let stripped: Vec<&str> = sql
            .lines()
            .map(|line| match line.find("--") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect();
        stripped
            .join("\n")
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            include_str!("../../migrations/0016_iyke_memory.sql"),
            include_str!("../../migrations/0059_chi_cache.sql"),
            include_str!("../../migrations/0067_iyke_seats.sql"),
            include_str!("../../migrations/0068_chi_cache_runner_pid.sql"),
        ] {
            for stmt in split_sql(sql) {
                sqlx::query(&stmt).execute(&pool).await.unwrap();
            }
        }
        for stmt in [
            "CREATE TABLE projects (
                id TEXT PRIMARY KEY, display_name TEXT NOT NULL, root_path TEXT, icon TEXT,
                color TEXT, description TEXT, position INTEGER NOT NULL DEFAULT 0,
                is_default INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL,
                archived_at INTEGER)",
            "CREATE TABLE settings_kv (key TEXT PRIMARY KEY, value TEXT NOT NULL,
                updated_at INTEGER NOT NULL)",
            "INSERT INTO projects (id, display_name, root_path, is_default, created_at)
             VALUES ('default', 'Default', '/work/default', 1, 0)",
            "INSERT INTO projects (id, display_name, root_path, created_at)
             VALUES ('royalti-co', 'Royalti', '/work/royalti', 0)",
            "INSERT INTO projects (id, display_name, root_path, created_at, archived_at)
             VALUES ('old', 'Old', NULL, 0, 1)",
        ] {
            sqlx::query(stmt).execute(&pool).await.unwrap();
        }
        pool
    }

    fn actor(client: &str) -> SeatActor {
        SeatActor {
            client: client.to_string(),
            hold: false,
            takeover: false,
            hold_ttl_ms: None,
        }
    }

    fn ui() -> SeatActor {
        actor("ui")
    }

    fn term(id: &str) -> TermLive {
        TermLive {
            terminal_id: id.to_string(),
            pty_id: format!("pty-{id}"),
            running: true,
            cwd: "/work/default".to_string(),
            lease_holder: None,
            mount: Some(SeatMount {
                window_label: "main".to_string(),
                pane_ids: vec!["pane-1".to_string()],
            }),
        }
    }

    fn world_with(terminals: &[&str]) -> WorldSnapshot {
        WorldSnapshot {
            terminals: terminals.iter().map(|t| term(t)).collect(),
            ..WorldSnapshot::default()
        }
    }

    fn term_ref(id: &str, engine: &str) -> SeatSessionRef {
        SeatSessionRef::Terminal {
            terminal_id: id.to_string(),
            engine_id: engine.to_string(),
            cwd: None,
            external_id: None,
        }
    }

    fn empty_req(name: &str, engine: &str, project: &str) -> CreateSeatReq {
        CreateSeatReq {
            project_id: Some(project.to_string()),
            name: name.to_string(),
            engine_id: engine.to_string(),
            start: SeatStart::Empty,
        }
    }

    async fn create(pool: &SqlitePool, name: &str, engine: &str) -> String {
        create_core(
            pool,
            &WorldSnapshot::default(),
            empty_req(name, engine, "default"),
            &ui(),
        )
        .await
        .unwrap()
        .0
        .seat
        .id
    }

    async fn row(pool: &SqlitePool, id: &str) -> SeatRow {
        fetch_seat(pool, id).await.unwrap().unwrap()
    }

    async fn insert_run(
        pool: &SqlitePool,
        run_id: &str,
        engine: &str,
        status: &str,
        ext: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO chi_cache (run_id, engine_id, external_id, cwd, status, owner)
             VALUES (?, ?, ?, '/work/run', ?, 'test')",
        )
        .bind(run_id.to_string())
        .bind(engine.to_string())
        .bind(ext.map(str::to_string))
        .bind(status.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn set_session(pool: &SqlitePool, seat: &str, kind: &str, sref: &str, ext: Option<&str>) {
        sqlx::query(
            "UPDATE iyke_seats SET session_kind = ?, session_ref = ?, external_id = ?,
                    session_cwd = '/work/docs' WHERE id = ?",
        )
        .bind(kind.to_string())
        .bind(sref.to_string())
        .bind(ext.map(str::to_string))
        .bind(seat.to_string())
        .execute(pool)
        .await
        .unwrap();
    }

    async fn exec(pool: &SqlitePool, sql: &str, scope: &str) {
        sqlx::query(sql)
            .bind(scope.to_string())
            .execute(pool)
            .await
            .unwrap();
    }

    /// One row under `scope` in each of the five scope-keyed tables.
    async fn seed_memory(pool: &SqlitePool, scope: &str) {
        exec(
            pool,
            "INSERT INTO iyke_scratchpads (id, scope, name, body, created_at, updated_at)
             VALUES (lower(hex(randomblob(16))), ?, 'notes', 'kept', 1, 5)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_todos (id, scope, title, created_at, updated_at)
             VALUES (lower(hex(randomblob(16))), ?, 'todo', 1, 1)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_kv (scope, key, value, updated_at) VALUES (?, 'k', '1', 1)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_locks (scope, resource, holder, acquired_at, expires_at)
             VALUES (?, 'r', 'h', 1, 2)",
            scope,
        )
        .await;
        exec(
            pool,
            "INSERT INTO iyke_timers (id, scope, fire_at, title, created_at)
             VALUES (lower(hex(randomblob(16))), ?, 9, 'timer', 1)",
            scope,
        )
        .await;
    }

    async fn counts_under(pool: &SqlitePool, scope: &str) -> Vec<i64> {
        let mut out = Vec::new();
        for table in SCOPE_TABLES {
            let sql = format!("SELECT COUNT(*) FROM {table} WHERE scope = ?");
            let n: i64 = sqlx::query_scalar(&sql)
                .bind(scope.to_string())
                .fetch_one(pool)
                .await
                .unwrap();
            out.push(n);
        }
        out
    }

    async fn count(pool: &SqlitePool, sql: &str, arg: &str) -> i64 {
        sqlx::query_scalar(sql)
            .bind(arg.to_string())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn seat_row(
        engine: &str,
        kind: Option<&str>,
        sref: Option<&str>,
        ext: Option<&str>,
    ) -> SeatRow {
        SeatRow {
            id: "s1".into(),
            project_id: "default".into(),
            name: "lead".into(),
            engine_id: engine.into(),
            session_kind: kind.map(str::to_string),
            session_ref: sref.map(str::to_string),
            external_id: ext.map(str::to_string),
            session_cwd: None,
            hold_client: None,
            hold_since: None,
            hold_expires_at: None,
            displaced_client: None,
            displaced_by: None,
            displaced_at: None,
            created_at: 0,
            last_active_at: 0,
        }
    }

    fn chi(engine: &str, status: &str, ext: Option<&str>) -> ChiLite {
        ChiLite {
            engine_id: engine.into(),
            status: status.into(),
            external_id: ext.map(str::to_string),
            cwd: None,
        }
    }

    fn never(_call: EngineCall) -> std::future::Ready<Result<String, String>> {
        panic!("the engine must not be called here")
    }

    // ── grammar (§1.2, §1.3, §3.1) ──────────────────────────────────────

    #[test]
    fn seat_name_grammar() {
        let max = "a".repeat(32);
        let over = "a".repeat(33);
        for ok in ["a", "lead", "nightly-2", "0", "a-b-c", max.as_str()] {
            assert!(validate_seat_name(ok).is_ok(), "should accept {ok:?}");
        }
        for bad in [
            "",
            "-a",
            "a-",
            "Lead",
            "le_ad",
            "le.ad",
            "le ad",
            "a/b",
            "ä",
            over.as_str(),
        ] {
            assert!(validate_seat_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn address_forms() {
        let q = |p: &str, n: &str| ParsedAddress::Qualified {
            project: p.into(),
            name: n.into(),
        };
        let b = |n: &str| ParsedAddress::Bare { name: n.into() };
        assert_eq!(
            parse_seat_address("seat:royalti-co/lead").unwrap(),
            q("royalti-co", "lead")
        );
        assert_eq!(
            parse_seat_address("royalti-co/lead").unwrap(),
            q("royalti-co", "lead")
        );
        assert_eq!(parse_seat_address("@lead").unwrap(), b("lead"));
        assert_eq!(parse_seat_address("lead").unwrap(), b("lead"));
        for bad in [
            "seat:lead",
            "seat:a/b/c",
            "@a/b",
            "Lead",
            "a/b/c",
            "",
            "seat:/x",
        ] {
            assert_eq!(
                parse_seat_address(bad).unwrap_err().code,
                "invalid_address",
                "{bad:?}"
            );
        }
        assert_eq!(seat_address("royalti-co", "lead"), "seat:royalti-co/lead");
    }

    /// §3.2: the `seat:` arm's project rule is `projects.rs::validate_slug`.
    /// That function is private, so this drives it through `create_project`,
    /// which calls it first, and requires the shared copy to agree.
    #[tokio::test]
    async fn project_slug_copy_agrees_with_projects_rs() {
        use crate::commands::projects::{create_project, CreateArgs};
        let pool = pool().await;
        let long_ok = "q".repeat(64);
        let long_bad = "r".repeat(65);
        let candidates: Vec<&str> = vec![
            "a",
            "abc123",
            "music-2026",
            "x_y_z",
            "0lead",
            "z-",
            "y_",
            "",
            "-bad",
            "_bad",
            "Bad",
            "with space",
            "with.dot",
            "with!",
            "ä",
            "a/b",
            long_ok.as_str(),
            long_bad.as_str(),
        ];
        for id in candidates {
            let shared = validate_project_slug(id).is_ok();
            let projects = create_project(
                &pool,
                CreateArgs {
                    id: id.to_string(),
                    display_name: "x".to_string(),
                    root_path: None,
                    icon: None,
                    color: None,
                    description: None,
                },
            )
            .await;
            if let Err(e) = &projects {
                assert!(
                    e.starts_with("invalid project id"),
                    "{id:?}: unexpected error {e}"
                );
            }
            assert_eq!(shared, projects.is_ok(), "slug rules disagree on {id:?}");
        }
    }

    // ── engine capability (§6.1) ────────────────────────────────────────

    /// Walks `build_engine_command_with`'s match arms in
    /// `server/shared/chi_exec.rs` (moved from `commands/chi.rs` by WP-P10):
    /// every arm needs a capability row; an arm that refuses to build can't
    /// hold a seat, and every other arm can.
    #[test]
    fn every_chi_engine_arm_has_a_capability_row() {
        let src = include_str!("../server/shared/chi_exec.rs");
        let start = src
            .find("fn build_engine_command_with(")
            .expect("build_engine_command_with in chi_exec.rs");
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("end of build_engine_command_with");
        let body = &body[..end];
        let arm = regex::Regex::new(r#"(?m)^\s*"([a-z0-9][a-z0-9-]*)"\s*=>(.*)$"#).unwrap();
        let mut arms: Vec<String> = Vec::new();
        for c in arm.captures_iter(body) {
            let id = c[1].to_string();
            let cap = engine_cap(&id).unwrap_or_else(|| {
                panic!("chi engine arm {id:?} has no seat capability row (G-SEATS §6.1)")
            });
            if c[2].contains("Err(") {
                assert!(
                    cap.resume.is_none(),
                    "{id} refuses to build but is seatable"
                );
            } else {
                assert!(cap.resume.is_some(), "{id} builds but is not seatable");
                assert!(cap.binary.is_some(), "{id} has no binary for install state");
            }
            arms.push(id);
        }
        assert!(arms.len() >= 6, "arms found: {arms:?}");
        // The fallback arm: anything else can't hold a seat.
        assert!(body.contains("engine not yet supported by iyke chi"));
        assert!(!engine_seatable("gemini"));
        assert!(!engine_seatable("no-such-engine"));
        // openrouter is dispatched in-process, before the arms: the shared
        // core asks its in-process engines first, and the desktop's (here)
        // and the daemon's both claim openrouter.
        assert!(src.contains("if engines.handles(&opts.engine_id)"));
        assert!(include_str!("../commands/chi.rs").contains("engine_id == \"openrouter\""));
        {
            use crate::server::shared::chi_exec::{InProcessEngines, NoInProcessEngines};
            assert!(NoInProcessEngines.handles("openrouter"));
        }
        assert_eq!(
            engine_resume("openrouter"),
            Some(EngineResume::ProcessLocal)
        );
        // Every seatable CLI row maps to an arm.
        for cap in ENGINE_CAPS {
            if cap.resume.is_some() && cap.engine_id != OPENROUTER_ENGINE {
                assert!(
                    arms.iter().any(|a| a == cap.engine_id),
                    "{} has no arm",
                    cap.engine_id
                );
            }
        }
    }

    #[test]
    fn capability_values_and_engines_info() {
        assert_eq!(engine_resume("claude-code"), Some(EngineResume::Durable));
        assert_eq!(engine_resume("codex"), Some(EngineResume::Durable));
        assert_eq!(
            engine_resume("antigravity-cli"),
            Some(EngineResume::Durable)
        );
        assert_eq!(engine_resume("opencode"), Some(EngineResume::None));
        assert_eq!(engine_resume("pi"), Some(EngineResume::None));
        assert_eq!(engine_resume("cursor-agent"), None);
        let world = WorldSnapshot {
            unavailable_engines: vec!["codex".into()],
            ..WorldSnapshot::default()
        };
        let info = engines_info(&world);
        let get = |id: &str| info.iter().find(|i| i.engine_id == id).unwrap().clone();
        assert!(get("claude-code").seatable);
        assert!(!get("codex").seatable);
        assert_eq!(get("codex").reason.as_deref(), Some("not installed"));
        assert!(!get("gemini").seatable);
        assert!(
            !get("openrouter").seatable,
            "the adapter is not registered in this world"
        );
        assert_eq!(
            serde_json::to_value(EngineResume::ProcessLocal).unwrap(),
            json!("process-local")
        );
    }

    // ── storage (§1.4, §1.5) ────────────────────────────────────────────

    #[tokio::test]
    async fn database_refuses_a_double_seated_session() {
        let pool = pool().await;
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        set_session(&pool, &a, "terminal", "t1", Some("conv-1")).await;
        // The same ref in a second seat.
        let same_ref = sqlx::query(
            "UPDATE iyke_seats SET session_kind = 'terminal', session_ref = 't1' WHERE id = ?",
        )
        .bind(b.clone())
        .execute(&pool)
        .await;
        assert!(same_ref.is_err());
        // The same conversation through another ref.
        let same_conv = sqlx::query(
            "UPDATE iyke_seats SET session_kind = 'run', session_ref = 'r9',
                    external_id = 'conv-1'
             WHERE id = ?",
        )
        .bind(b.clone())
        .execute(&pool)
        .await;
        assert!(same_conv.is_err());
        // Half a session pointer is refused by the CHECK.
        let half = sqlx::query("UPDATE iyke_seats SET session_kind = 'run' WHERE id = ?")
            .bind(b.clone())
            .execute(&pool)
            .await;
        assert!(half.is_err());
        // The seat's agent row (§1.5).
        let (name, metadata): (String, String) =
            sqlx::query_as("SELECT name, metadata FROM iyke_agents WHERE id = ?")
                .bind(a.clone())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "seat:default/a");
        assert_eq!(
            serde_json::from_str::<Value>(&metadata).unwrap(),
            json!({ "seat": true })
        );
    }

    #[tokio::test]
    async fn create_refusals() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let code = |r: Result<(SeatMoveResult, Effects), SeatError>| r.unwrap_err().code;
        assert_eq!(
            code(create_core(&pool, &w, empty_req("Bad", "claude-code", "default"), &ui()).await),
            "invalid_seat_name"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("g", "gemini", "default"), &ui()).await),
            "engine_unsupported"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("c", "cursor-agent", "default"), &ui()).await),
            "engine_unsupported"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("x", "codex", "old"), &ui()).await),
            "project_not_found"
        );
        assert_eq!(
            code(create_core(&pool, &w, empty_req("x", "codex", "nope"), &ui()).await),
            "project_not_found"
        );
        create(&pool, "lead", "claude-code").await;
        assert_eq!(
            code(create_core(&pool, &w, empty_req("lead", "codex", "default"), &ui()).await),
            "seat_name_taken"
        );
        // The same name in another project is fine: unique per project.
        assert!(
            create_core(&pool, &w, empty_req("lead", "codex", "royalti-co"), &ui())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn create_with_an_open_session_is_a_move() {
        let pool = pool().await;
        let w = world_with(&["t5"]);
        let old = create(&pool, "old-seat", "claude-code").await;
        move_core(&pool, &w, &term_ref("t5", "claude-code"), &old, &ui(), None)
            .await
            .unwrap();
        let req = CreateSeatReq {
            project_id: Some("default".into()),
            name: "fresh".into(),
            engine_id: "claude-code".into(),
            start: SeatStart::Session {
                session: term_ref("t5", "claude-code"),
            },
        };
        let (result, effects) = create_core(&pool, &w, req, &ui()).await.unwrap();
        assert_eq!(result.from_seat_ids, vec![old.clone()]);
        assert!(row(&pool, &old).await.session_ref.is_none());
        assert_eq!(effects.events[0].kinds, vec!["created", "bound"]);
        // A session on another engine is refused.
        let req = CreateSeatReq {
            project_id: Some("default".into()),
            name: "mismatch".into(),
            engine_id: "claude-code".into(),
            start: SeatStart::Session {
                session: term_ref("t5", "codex"),
            },
        };
        assert_eq!(
            create_core(&pool, &w, req, &ui()).await.unwrap_err().code,
            "engine_mismatch"
        );
    }

    // ── DEC-69c: moves (§4.3) ───────────────────────────────────────────

    #[tokio::test]
    async fn move_unbinds_from_every_other_seat_atomically() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");

        let (first, _) = move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();
        assert!(first.from_seat_ids.is_empty());
        assert_eq!(row(&pool, &a).await.session_ref.as_deref(), Some("t1"));

        let (second, effects) = move_core(&pool, &w, &t1, &b, &ui(), None).await.unwrap();
        assert_eq!(second.from_seat_ids, vec![a.clone()]);
        assert!(row(&pool, &a).await.session_ref.is_none());
        assert_eq!(row(&pool, &b).await.session_ref.as_deref(), Some("t1"));
        // One event per affected seat (§10).
        assert_eq!(effects.events.len(), 2);
        assert_eq!(effects.events[0].seat_id, b);
        assert_eq!(effects.events[0].kinds, vec!["bound"]);
        assert_eq!(effects.events[0].from_seat_ids, Some(vec![a.clone()]));
        assert_eq!(effects.events[1].seat_id, a);
        assert_eq!(effects.events[1].kinds, vec!["unbound"]);

        // Moving into the seat that already holds it is a no-op refresh.
        let (again, _) = move_core(&pool, &w, &t1, &b, &ui(), None).await.unwrap();
        assert!(again.from_seat_ids.is_empty());
        assert_eq!(row(&pool, &b).await.session_ref.as_deref(), Some("t1"));
    }

    #[tokio::test]
    async fn move_by_conversation_unbinds_the_other_ref() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        set_session(&pool, &a, "terminal", "t-old", Some("conv-7")).await;
        insert_run(&pool, "run-7", "claude-code", "done", Some("conv-7")).await;
        let run = SeatSessionRef::Run {
            run_id: "run-7".into(),
        };
        let (moved, _) = move_core(&pool, &w, &run, &b, &ui(), None).await.unwrap();
        assert_eq!(moved.from_seat_ids, vec![a.clone()]);
        assert!(row(&pool, &a).await.external_id.is_none());
        assert_eq!(row(&pool, &b).await.external_id.as_deref(), Some("conv-7"));
    }

    #[tokio::test]
    async fn concurrent_moves_never_seat_a_session_twice() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let c = create(&pool, "c", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");
        move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();

        let actor = ui();
        let (r1, r2) = tokio::join!(
            move_core(&pool, &w, &t1, &b, &actor, None),
            move_core(&pool, &w, &t1, &c, &actor, None),
        );
        assert!(r1.is_ok() || r2.is_ok());
        for r in [&r1, &r2] {
            if let Err(e) = r {
                assert_eq!(e.code, "conflict");
            }
        }
        let seated = count(
            &pool,
            "SELECT COUNT(*) FROM iyke_seats WHERE session_ref = ?",
            "t1",
        )
        .await;
        assert_eq!(seated, 1, "one session, at most one seat");
        assert!(row(&pool, &a).await.session_ref.is_none());
    }

    #[tokio::test]
    async fn move_checks_engine_and_visibility() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let e = move_core(&pool, &w, &term_ref("t1", "codex"), &a, &ui(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "engine_mismatch");
        // Not a PTY this app can see (e.g. a daemon terminal, P-10).
        let e = move_core(
            &pool,
            &w,
            &term_ref("daemon-1", "claude-code"),
            &a,
            &ui(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "terminal_not_found");
        assert!(row(&pool, &a).await.session_ref.is_none());
    }

    // ── DEC-69b: Clear keeps the pad (§4.2) ─────────────────────────────

    #[tokio::test]
    async fn clear_keeps_the_pad_and_all_memory() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        move_core(&pool, &w, &term_ref("t1", "claude-code"), &a, &ui(), None)
            .await
            .unwrap();
        seed_memory(&pool, "seat:default/a").await;
        exec(
            &pool,
            "INSERT INTO iyke_agent_inbox (id, agent_id, kind, payload, created_at)
             VALUES ('i1', ?, 'timer-fired', '{}', 1)",
            &a,
        )
        .await;

        let (view, effects) = clear_core(&pool, &w, &a, &ui()).await.unwrap();
        assert!(view.session.is_none());
        assert_eq!(view.status, SeatStatus::Vacant);
        assert_eq!(view.resume.reason, Some(NotResumableReason::NoSession));
        assert_eq!(
            counts_under(&pool, "seat:default/a").await,
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(view.pad.count, 1);
        assert_eq!(view.inbox_count, 1);
        assert_eq!(effects.events[0].kinds, vec!["cleared"]);
    }

    /// Round 47 erratum E-4: clearing a seat with a queued text reports the
    /// drop on its event instead of only logging it.
    #[tokio::test]
    async fn clear_reports_a_dropped_queued_text() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let a = create(&pool, "q", "claude-code").await;
        assert!(store().enqueue(
            &a,
            QueuedText {
                prompt: "later".into(),
                since: 1,
                client: "ui".into(),
            },
        ));
        let (_view, effects) = clear_core(&pool, &w, &a, &ui()).await.unwrap();
        assert_eq!(effects.events[0].kinds, vec!["cleared", "queue-dropped"]);
        assert_eq!(effects.events[0].queue_dropped, Some("cleared"));
        assert!(store().queued(&a).is_none());
    }

    // ── DEC-69a: resume, then send (§4.1 path H, §6.2) ──────────────────

    #[tokio::test]
    async fn resume_binds_only_after_the_engine_call_returns() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let docs = create(&pool, "docs", "claude-code").await;
        set_session(&pool, &docs, "terminal", "tab-9", Some("conv-9")).await;

        let probe_pool = pool.clone();
        let probe_seat = docs.clone();
        let (result, effects) = resume_core(
            &pool,
            &w,
            &docs,
            "hello".to_string(),
            &ui(),
            ResumeFallback::Refuse,
            move |call| async move {
                // At call time nothing is bound yet: the seat still points
                // at its old terminal session.
                let before = fetch_seat(&probe_pool, &probe_seat).await.unwrap().unwrap();
                assert_eq!(before.session_ref.as_deref(), Some("tab-9"));
                // The text is the resumed conversation's first turn.
                assert_eq!(
                    call,
                    EngineCall::Start {
                        engine_id: "claude-code".into(),
                        prompt: "hello".into(),
                        cwd: Some("/work/docs".into()),
                        resume_session_id: Some("conv-9".into()),
                        persistent: false,
                    }
                );
                insert_run(
                    &probe_pool,
                    "run-new",
                    "claude-code",
                    "running",
                    Some("conv-9"),
                )
                .await;
                Ok("run-new".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::Resumed);
        assert_eq!(result.run_id, "run-new");
        assert!(matches!(
            result.previous,
            Some(SeatSession::Terminal { .. })
        ));
        assert_eq!(result.seat.status, SeatStatus::Run);
        let after = row(&pool, &docs).await;
        assert_eq!(after.session_kind.as_deref(), Some("run"));
        assert_eq!(after.session_ref.as_deref(), Some("run-new"));
        assert_eq!(after.external_id.as_deref(), Some("conv-9"));
        assert_eq!(effects.events[0].kinds, vec!["bound"]);

        // Now occupied: a second resume is refused, and never reaches the engine.
        let e = resume_core(
            &pool,
            &w,
            &docs,
            "x".into(),
            &ui(),
            ResumeFallback::Fresh,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_not_vacant");
    }

    #[tokio::test]
    async fn resume_of_a_run_continues_the_same_run_id() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "codex").await;
        insert_run(&pool, "run-1", "codex", "failed", Some("thread-1")).await;
        set_session(&pool, &s, "run", "run-1", Some("thread-1")).await;
        let (result, _) = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Refuse,
            |call| async move {
                assert_eq!(
                    call,
                    EngineCall::ResumeRun {
                        run_id: "run-1".into(),
                        prompt: "go".into()
                    }
                );
                Ok("run-1".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.run_id, "run-1");
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-1"));
    }

    #[tokio::test]
    async fn engine_failure_writes_nothing() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "docs", "claude-code").await;
        set_session(&pool, &s, "terminal", "tab-1", Some("conv-1")).await;
        let before = row(&pool, &s).await;
        let e = resume_core(
            &pool,
            &w,
            &s,
            "hi".into(),
            &ui(),
            ResumeFallback::Fresh,
            |_| async { Err::<String, String>("claude not found".to_string()) },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "engine_failed");
        assert_eq!(row(&pool, &s).await, before);
    }

    #[tokio::test]
    async fn not_resumable_is_flagged_refused_and_falls_back_fresh() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "openrouter").await;
        insert_run(&pool, "run-or", "openrouter", "done", None).await;
        set_session(&pool, &s, "run", "run-or", None).await;
        // After a restart the adapter holds no transcript.
        let restarted = WorldSnapshot {
            openrouter_registered: true,
            ..WorldSnapshot::default()
        };
        let view = view_by_id(&pool, &restarted, &s).await.unwrap();
        assert_eq!(view.status, SeatStatus::Vacant);
        assert_eq!(view.resume.reason, Some(NotResumableReason::ProcessLocal));
        // The "not resumable after restart" flag is carried at all times.
        assert_eq!(view.engine_resume, EngineResume::ProcessLocal);

        // An explicit resume never falls back.
        let e = resume_core(
            &pool,
            &restarted,
            &s,
            "x".into(),
            &ui(),
            ResumeFallback::Refuse,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "not_resumable");
        assert_eq!(e.details, Some(json!({ "reason": "process_local" })));
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-or"));

        // A dispatch starts fresh, and says why.
        let probe_pool = pool.clone();
        let (result, _) = resume_core(
            &pool,
            &restarted,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Fresh,
            move |call| async move {
                assert_eq!(
                    call,
                    EngineCall::Start {
                        engine_id: "openrouter".into(),
                        prompt: "go".into(),
                        cwd: Some("/work/default".into()),
                        resume_session_id: None,
                        persistent: false,
                    }
                );
                insert_run(&probe_pool, "run-or-2", "openrouter", "running", None).await;
                Ok("run-or-2".to_string())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::StartedFresh);
        assert_eq!(result.reason, Some(NotResumableReason::ProcessLocal));
        assert_eq!(
            row(&pool, &s).await.session_ref.as_deref(),
            Some("run-or-2")
        );

        // Before a restart the same kind of done run is idle.
        let live = WorldSnapshot {
            openrouter_registered: true,
            openrouter_threads: HashMap::from([("run-or-2".to_string(), true)]),
            ..WorldSnapshot::default()
        };
        sqlx::query("UPDATE chi_cache SET status = 'done' WHERE run_id = 'run-or-2'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            view_by_id(&pool, &live, &s).await.unwrap().status,
            SeatStatus::Idle
        );
    }

    #[tokio::test]
    async fn fill_starts_a_new_run_and_unseats_the_previous() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let s = create(&pool, "lead", "claude-code").await;
        move_core(&pool, &w, &term_ref("t1", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        let probe_pool = pool.clone();
        let (result, _) = fill_core(
            &pool,
            &w,
            &s,
            "start".into(),
            &ui(),
            true,
            move |call| async move {
                assert!(matches!(
                    &call,
                    EngineCall::Start {
                        resume_session_id: None,
                        persistent: true,
                        ..
                    }
                ));
                insert_run(&probe_pool, "run-f", "claude-code", "running", None).await;
                Ok("run-f".to_string())
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            result.previous,
            Some(SeatSession::Terminal { .. })
        ));
        assert_eq!(row(&pool, &s).await.session_ref.as_deref(), Some("run-f"));
    }

    // ── §4.1 claim, §4.5 queue, resolve ─────────────────────────────────

    #[tokio::test]
    async fn resume_claim_blocks_a_second_resume() {
        let pool = pool().await;
        let w = world_with(&["t2"]);
        let s = create(&pool, "c1", "claude-code").await;
        let (route, _) = resolve_core(&pool, &w, &s, &ui(), true).await.unwrap();
        let claim = match route {
            SeatRoute::Vacant { claim, resume, .. } => {
                assert_eq!(resume.reason, Some(NotResumableReason::NoSession));
                claim.expect("a claim")
            }
            other => panic!("expected vacant, got {other:?}"),
        };
        assert_eq!(
            resolve_core(&pool, &w, &s, &ui(), true)
                .await
                .unwrap_err()
                .code,
            "seat_resuming"
        );
        let e = resume_core(
            &pool,
            &w,
            &s,
            "x".into(),
            &ui(),
            ResumeFallback::Fresh,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_resuming");
        // Path T step 3: the bind clears the claim.
        let (moved, _) = move_core(
            &pool,
            &w,
            &term_ref("t2", "claude-code"),
            &s,
            &ui(),
            Some(claim.as_str()),
        )
        .await
        .unwrap();
        assert!(!moved.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_none());
    }

    #[tokio::test]
    async fn resolve_routes_by_agent_liveness() {
        let pool = pool().await;
        let s = create(&pool, "lead", "claude-code").await;
        let mut w = world_with(&["t7"]);
        move_core(&pool, &w, &term_ref("t7", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        // Not reported since the PTY spawned: fail closed.
        assert_eq!(
            resolve_core(&pool, &w, &s, &ui(), false)
                .await
                .unwrap_err()
                .code,
            "agent_not_live"
        );
        // Live, between turns.
        w.agents.insert(
            "t7".into(),
            AgentLive {
                pty_id: Some("pty-t7".into()),
                ..AgentLive::default()
            },
        );
        match resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0 {
            SeatRoute::Pty { agent, seat, .. } => {
                assert_eq!(agent, AgentState::Live);
                assert_eq!(seat.status, SeatStatus::Idle);
            }
            other => panic!("expected pty, got {other:?}"),
        }
        // Exited to its shell: vacant, never a PTY route.
        w.agents.get_mut("t7").unwrap().exited = true;
        assert!(matches!(
            resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0,
            SeatRoute::Vacant { .. }
        ));
    }

    #[tokio::test]
    async fn queue_has_one_slot() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-q", "claude-code", "running", Some("conv-q")).await;
        set_session(&pool, &s, "run", "run-q", Some("conv-q")).await;
        match resolve_core(&pool, &w, &s, &ui(), false).await.unwrap().0 {
            SeatRoute::ChiResume { busy, run_id, .. } => {
                assert!(busy);
                assert_eq!(run_id, "run-q");
            }
            other => panic!("expected chi-resume, got {other:?}"),
        }
        let (view, _) = queue_core(&pool, &w, &s, "next".into(), &ui())
            .await
            .unwrap();
        assert!(view.queued.is_some());
        assert_eq!(
            queue_core(&pool, &w, &s, "again".into(), &ui())
                .await
                .unwrap_err()
                .code,
            "seat_busy"
        );
        store().dequeue(&s);
    }

    // ── §5 holds and takeover ───────────────────────────────────────────

    #[tokio::test]
    async fn hold_takeover_and_displaced_once() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "lead", "claude-code").await;
        let orch_hold = SeatActor {
            hold: true,
            ..actor("orch")
        };
        clear_core(&pool, &w, &s, &orch_hold).await.unwrap();
        assert_eq!(row(&pool, &s).await.hold_client.as_deref(), Some("orch"));

        // Another client is refused, with "held by X since T".
        let e = clear_core(&pool, &w, &s, &ui()).await.unwrap_err();
        assert_eq!(e.code, "seat_held");
        let details = e.details.clone().unwrap();
        assert_eq!(details["client"], json!("orch"));
        assert!(details.get("since").is_some());

        // An explicit takeover transfers the hold and records the displaced.
        let take = SeatActor {
            takeover: true,
            ..ui()
        };
        let (view, effects) = clear_core(&pool, &w, &s, &take).await.unwrap();
        assert_eq!(view.hold.as_ref().unwrap().client, "ui");
        assert_eq!(effects.events[0].kinds, vec!["cleared", "taken-over"]);
        let after = row(&pool, &s).await;
        assert_eq!(after.displaced_client.as_deref(), Some("orch"));
        assert_eq!(after.displaced_by.as_deref(), Some("ui"));

        // The displaced client is told once, on its next call…
        let e = clear_core(&pool, &w, &s, &actor("orch")).await.unwrap_err();
        assert_eq!(e.code, "seat_taken_over");
        assert_eq!(e.details.clone().unwrap()["by"], json!("ui"));
        assert!(row(&pool, &s).await.displaced_client.is_none());
        // …then treated as any other client.
        assert_eq!(
            clear_core(&pool, &w, &s, &actor("orch"))
                .await
                .unwrap_err()
                .code,
            "seat_held"
        );

        // Release: someone else's hold needs takeover; one's own doesn't.
        assert_eq!(
            release_core(&pool, &w, &s, &actor("orch"))
                .await
                .unwrap_err()
                .code,
            "seat_held"
        );
        let (released, _) = release_core(&pool, &w, &s, &ui()).await.unwrap();
        assert!(released.hold.is_none());
        assert!(clear_core(&pool, &w, &s, &actor("orch")).await.is_ok());

        // An expired hold is absent.
        exec(
            &pool,
            "UPDATE iyke_seats SET hold_client = 'orch', hold_since = 1, hold_expires_at = 2
             WHERE id = ?",
            &s,
        )
        .await;
        assert!(clear_core(&pool, &w, &s, &ui()).await.is_ok());
    }

    #[tokio::test]
    async fn a_move_checks_the_hold_on_the_seat_it_unbinds() {
        let pool = pool().await;
        let w = world_with(&["t1"]);
        let a = create(&pool, "a", "claude-code").await;
        let b = create(&pool, "b", "claude-code").await;
        let t1 = term_ref("t1", "claude-code");
        move_core(&pool, &w, &t1, &a, &ui(), None).await.unwrap();
        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'orch', hold_since = 1, hold_expires_at = ?
             WHERE id = ?",
        )
        .bind(now_ms() + 600_000)
        .bind(a.clone())
        .execute(&pool)
        .await
        .unwrap();

        let e = move_core(&pool, &w, &t1, &b, &ui(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "seat_held");
        assert_eq!(row(&pool, &a).await.session_ref.as_deref(), Some("t1"));
        assert!(row(&pool, &b).await.session_ref.is_none());

        let take = SeatActor {
            takeover: true,
            ..ui()
        };
        let (moved, effects) = move_core(&pool, &w, &t1, &b, &take, None).await.unwrap();
        assert_eq!(moved.from_seat_ids, vec![a.clone()]);
        let a_after = row(&pool, &a).await;
        assert!(a_after.session_ref.is_none());
        assert_eq!(a_after.displaced_client.as_deref(), Some("orch"));
        assert_eq!(effects.events[1].kinds, vec!["unbound", "taken-over"]);
    }

    // ── §3.4 rename, §4.2 remove ────────────────────────────────────────

    #[tokio::test]
    async fn rename_rewrites_all_five_scope_tables() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "lead", "claude-code").await;
        seed_memory(&pool, "seat:default/lead").await;
        let (view, effects) = rename_core(&pool, &w, &s, "captain", &ui()).await.unwrap();
        assert_eq!(view.name, "captain");
        assert_eq!(view.address, "seat:default/captain");
        assert_eq!(
            counts_under(&pool, "seat:default/lead").await,
            vec![0, 0, 0, 0, 0]
        );
        assert_eq!(
            counts_under(&pool, "seat:default/captain").await,
            vec![1, 1, 1, 1, 1]
        );
        let agent: String = sqlx::query_scalar("SELECT name FROM iyke_agents WHERE id = ?")
            .bind(s.clone())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(agent, "seat:default/captain");
        assert_eq!(effects.events[0].kinds, vec!["renamed"]);
        // Watchers of the moved pad wake under both keys.
        assert!(effects
            .pads
            .iter()
            .any(|p| p.scope == "seat:default/lead" && p.name == "notes" && p.deleted));
        assert!(effects
            .pads
            .iter()
            .any(|p| p.scope == "seat:default/captain" && p.name == "notes" && !p.deleted));
    }

    #[tokio::test]
    async fn rename_refuses_on_conflict_and_changes_nothing() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "a1", "claude-code").await;
        create(&pool, "taken", "claude-code").await;
        seed_memory(&pool, "seat:default/a1").await;
        // Memory already under the new address (e.g. a removed seat's).
        exec(
            &pool,
            "INSERT INTO iyke_kv (scope, key, value, updated_at) VALUES (?, 'k', '1', 1)",
            "seat:default/b1",
        )
        .await;
        let e = rename_core(&pool, &w, &s, "b1", &ui()).await.unwrap_err();
        assert_eq!(e.code, "rename_scope_conflict");
        assert_eq!(row(&pool, &s).await.name, "a1");
        assert_eq!(
            counts_under(&pool, "seat:default/a1").await,
            vec![1, 1, 1, 1, 1]
        );
        assert_eq!(
            rename_core(&pool, &w, &s, "taken", &ui())
                .await
                .unwrap_err()
                .code,
            "seat_name_taken"
        );
        assert_eq!(
            rename_core(&pool, &w, &s, "Nope", &ui())
                .await
                .unwrap_err()
                .code,
            "invalid_seat_name"
        );
    }

    #[tokio::test]
    async fn remove_with_and_without_memory() {
        let pool = pool().await;
        let keep = create(&pool, "keep", "claude-code").await;
        seed_memory(&pool, "seat:default/keep").await;
        exec(
            &pool,
            "INSERT INTO iyke_agent_inbox (id, agent_id, kind, payload, created_at)
             VALUES ('i2', ?, 'timer-fired', '{}', 1)",
            &keep,
        )
        .await;
        let (removed, effects) = remove_core(&pool, &keep, false, &ui()).await.unwrap();
        assert_eq!(removed.seat_id, keep);
        assert_eq!(effects.events[0].kinds, vec!["removed"]);
        assert!(fetch_seat(&pool, &keep).await.unwrap().is_none());
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM iyke_agents WHERE id = ?",
                &keep
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &pool,
                "SELECT COUNT(*) FROM iyke_agent_inbox WHERE agent_id = ?",
                &keep
            )
            .await,
            0
        );
        assert_eq!(
            counts_under(&pool, "seat:default/keep").await,
            vec![1, 1, 1, 1, 1]
        );

        let wipe = create(&pool, "wipe", "claude-code").await;
        seed_memory(&pool, "seat:default/wipe").await;
        remove_core(&pool, &wipe, true, &ui()).await.unwrap();
        assert_eq!(
            counts_under(&pool, "seat:default/wipe").await,
            vec![0, 0, 0, 0, 0]
        );
    }

    // ── §2.5 hooks ──────────────────────────────────────────────────────

    #[test]
    fn hook_events_drive_the_liveness_map() {
        let mut agents = HashMap::new();
        let pty = || Some("pty-1".to_string());
        apply_hook(
            &mut agents,
            "t1",
            "SessionStart",
            Some("sess".into()),
            pty(),
        );
        assert_eq!(
            agents["t1"],
            AgentLive {
                pty_id: pty(),
                exited: false,
                turn_in_flight: false,
                session_id: Some("sess".into()),
            }
        );
        apply_hook(&mut agents, "t1", "UserPromptSubmit", None, pty());
        assert!(agents["t1"].turn_in_flight);
        apply_hook(&mut agents, "t1", "Stop", None, pty());
        assert!(!agents["t1"].turn_in_flight);
        apply_hook(&mut agents, "t1", "SessionEnd", None, pty());
        assert!(agents["t1"].exited);
        apply_hook(&mut agents, "t1", "PreToolUse", None, pty());
        assert!(agents["t1"].exited, "other events change nothing");
        // A new PTY on the reused tab id starts over.
        apply_hook(
            &mut agents,
            "t1",
            "UserPromptSubmit",
            None,
            Some("pty-2".into()),
        );
        assert_eq!(agents["t1"].pty_id.as_deref(), Some("pty-2"));
        assert!(!agents["t1"].exited);
        assert!(agents["t1"].session_id.is_none());
    }

    #[tokio::test]
    async fn session_start_captures_the_resume_id() {
        let pool = pool().await;
        let w = world_with(&["t8"]);
        let s = create(&pool, "lead", "claude-code").await;
        move_core(&pool, &w, &term_ref("t8", "claude-code"), &s, &ui(), None)
            .await
            .unwrap();
        let events = capture_core(&pool, "t8", "sess-1").await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kinds, vec!["updated"]);
        assert_eq!(row(&pool, &s).await.external_id.as_deref(), Some("sess-1"));
        assert!(capture_core(&pool, "t8", "sess-1")
            .await
            .unwrap()
            .is_empty());
        assert!(capture_core(&pool, "unseated", "sess-2")
            .await
            .unwrap()
            .is_empty());
    }

    /// DEC-69c on the capture path: the conversation a new terminal resumed
    /// leaves the seat that still recorded it — unless that seat is held.
    #[tokio::test]
    async fn capture_moves_the_conversation_off_another_seat() {
        let pool = pool().await;
        let old = create(&pool, "old", "claude-code").await;
        let new = create(&pool, "new", "claude-code").await;
        set_session(&pool, &old, "terminal", "t-ended", Some("conv-1")).await;
        let w = world_with(&["t9"]);
        move_core(&pool, &w, &term_ref("t9", "claude-code"), &new, &ui(), None)
            .await
            .unwrap();
        let events = capture_core(&pool, "t9", "conv-1").await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seat_id, new);
        assert_eq!(events[0].from_seat_ids, Some(vec![old.clone()]));
        assert_eq!(events[1].seat_id, old);
        assert_eq!(events[1].kinds, vec!["unbound"]);
        assert_eq!(
            row(&pool, &new).await.external_id.as_deref(),
            Some("conv-1")
        );
        let old_row = row(&pool, &old).await;
        assert!(old_row.session_kind.is_none() && old_row.external_id.is_none());

        // A held seat keeps its conversation; the capture is skipped.
        let held = create(&pool, "held", "claude-code").await;
        let other = create(&pool, "other", "claude-code").await;
        set_session(&pool, &held, "terminal", "t-gone", Some("conv-2")).await;
        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'iyke', hold_since = ?, hold_expires_at = ?
             WHERE id = ?",
        )
        .bind(now_ms())
        .bind(now_ms() + 60_000)
        .bind(held.clone())
        .execute(&pool)
        .await
        .unwrap();
        let w = world_with(&["t10"]);
        move_core(
            &pool,
            &w,
            &term_ref("t10", "claude-code"),
            &other,
            &ui(),
            None,
        )
        .await
        .unwrap();
        assert!(capture_core(&pool, "t10", "conv-2")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            row(&pool, &held).await.external_id.as_deref(),
            Some("conv-2")
        );
        assert!(row(&pool, &other).await.external_id.is_none());
    }

    // ── §2.2 derivation table ───────────────────────────────────────────

    #[test]
    fn derivation_table() {
        let w = WorldSnapshot::default();
        let d = derive(&seat_row("claude-code", None, None, None), None, &w);
        assert_eq!(
            (d.status, d.resume.reason),
            (SeatStatus::Vacant, Some(NotResumableReason::NoSession))
        );

        let run = |engine: &str, ext: Option<&str>| seat_row(engine, Some("run"), Some("r1"), ext);
        let d = derive(&run("claude-code", None), None, &w);
        assert_eq!(d.resume.reason, Some(NotResumableReason::RunMissing));
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "running", None)),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Run);
        assert_eq!(
            d.hint,
            RouteHint::Chi {
                run_id: "r1".into(),
                busy: true
            }
        );
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "done", Some("c"))),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Idle);
        assert_eq!(
            d.hint,
            RouteHint::Chi {
                run_id: "r1".into(),
                busy: false
            }
        );
        let d = derive(
            &run("claude-code", None),
            Some(&chi("claude-code", "done", None)),
            &w,
        );
        assert_eq!(
            (d.status, d.resume.reason),
            (SeatStatus::Vacant, Some(NotResumableReason::NoResumeId))
        );
        let d = derive(
            &run("opencode", None),
            Some(&chi("opencode", "done", None)),
            &w,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::NoResumeSupport));
        let d = derive(
            &run("openrouter", None),
            Some(&chi("openrouter", "done", None)),
            &w,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::EngineUnavailable));
        let d = derive(
            &run("claude-code", Some("c")),
            Some(&chi("claude-code", "failed", Some("c"))),
            &w,
        );
        assert_eq!(d.status, SeatStatus::Vacant);
        assert!(d.resume.resumable);
        let gone = WorldSnapshot {
            unavailable_engines: vec!["claude-code".into()],
            ..WorldSnapshot::default()
        };
        let d = derive(
            &run("claude-code", Some("c")),
            Some(&chi("claude-code", "cancelled", Some("c"))),
            &gone,
        );
        assert_eq!(d.resume.reason, Some(NotResumableReason::EngineUnavailable));

        let tab = |engine: &str| seat_row(engine, Some("terminal"), Some("t1"), Some("c"));
        let d = derive(&tab("claude-code"), None, &w);
        assert_eq!(d.status, SeatStatus::Vacant, "unknown to PtyManager");
        assert!(d.resume.resumable);
        let mut live = world_with(&["t1"]);
        let d = derive(&tab("codex"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Unreported))
        );
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Starting))
        );
        assert_eq!(d.hint, RouteHint::AgentStarting);
        assert_eq!(d.mount.as_ref().unwrap().window_label, "main");
        live.agents.insert(
            "t1".into(),
            AgentLive {
                pty_id: Some("pty-t1".into()),
                turn_in_flight: true,
                ..AgentLive::default()
            },
        );
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            (d.status, d.agent),
            (SeatStatus::Live, Some(AgentState::Live))
        );
        live.agents.get_mut("t1").unwrap().pty_id = Some("pty-stale".into());
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(
            d.agent,
            Some(AgentState::Starting),
            "a stale PTY's report doesn't count"
        );
        live.agents.get_mut("t1").unwrap().pty_id = Some("pty-t1".into());
        live.agents.get_mut("t1").unwrap().exited = true;
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(d.status, SeatStatus::Vacant);
        live.terminals[0].running = false;
        live.agents.clear();
        let d = derive(&tab("claude-code"), None, &live);
        assert_eq!(d.status, SeatStatus::Vacant, "PTY exited");
    }

    /// §9.4: the UI resolves with `claimResume` before every dispatch; a
    /// run-kind vacant seat then goes path H, which its own claim must not
    /// block — so such a seat gets no claim.
    #[tokio::test]
    async fn a_run_kind_vacant_seat_gets_no_claim() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "nightly", "codex").await;
        insert_run(&pool, "run-v", "codex", "failed", Some("thread-v")).await;
        set_session(&pool, &s, "run", "run-v", Some("thread-v")).await;
        match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, resume, .. } => {
                assert!(claim.is_none());
                assert!(resume.resumable);
            }
            other => panic!("expected vacant, got {other:?}"),
        }
        let (result, _) = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Fresh,
            |_| async { Ok::<String, String>("run-v".to_string()) },
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ResumeOutcome::Resumed);
    }

    /// An empty seat on a runs-only engine can only go path H, so resolving
    /// it with `claimResume` takes no claim and `seats_resume` goes through.
    #[tokio::test]
    async fn an_empty_runs_only_seat_gets_no_claim() {
        let pool = pool().await;
        let w = WorldSnapshot::default();
        let s = create(&pool, "scout", "opencode").await;
        match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, .. } => assert!(claim.is_none()),
            other => panic!("expected vacant, got {other:?}"),
        }
        assert!(store().live_claim(&s, now_ms()).is_none());
        let e = resume_core(
            &pool,
            &w,
            &s,
            "go".into(),
            &ui(),
            ResumeFallback::Refuse,
            never,
        )
        .await
        .unwrap_err();
        assert_eq!(
            e.code, "not_resumable",
            "refused for its session, not a claim"
        );
    }

    /// A move without a claim leaves another client's live claim alone; the
    /// claim-carrying move then clears it and reports nothing lost.
    #[tokio::test]
    async fn a_plain_move_keeps_another_clients_claim() {
        let pool = pool().await;
        let w = world_with(&["t3", "t4"]);
        let s = create(&pool, "c2", "claude-code").await;
        let claim = match resolve_core(&pool, &w, &s, &ui(), true).await.unwrap().0 {
            SeatRoute::Vacant { claim, .. } => claim.expect("a claim"),
            other => panic!("expected vacant, got {other:?}"),
        };
        let (plain, _) = move_core(
            &pool,
            &w,
            &term_ref("t3", "claude-code"),
            &s,
            &actor("iyke"),
            None,
        )
        .await
        .unwrap();
        assert!(!plain.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_some());
        let (claimed, _) = move_core(
            &pool,
            &w,
            &term_ref("t4", "claude-code"),
            &s,
            &ui(),
            Some(claim.as_str()),
        )
        .await
        .unwrap();
        assert!(!claimed.claim_lost);
        assert!(store().live_claim(&s, now_ms()).is_none());
    }

    /// A bind that fails after the engine call returned keeps the refusal's
    /// details and adds the run id, so no caller resends the text blindly.
    #[test]
    fn a_failed_bind_after_the_engine_call_carries_the_run_id() {
        let hold = SeatHold {
            client: "iyke".into(),
            since: 1,
            expires_at: 2,
        };
        let e = SeatError::held("lead", &hold).after_engine("run-9");
        assert_eq!(e.code, "seat_held");
        let d = e.details.unwrap();
        assert_eq!(d["run_id"], "run-9");
        assert_eq!(d["client"], "iyke");
        let e = SeatError::conflict("busy").after_engine("run-8");
        assert_eq!(e.details.unwrap(), json!({ "run_id": "run-8" }));
    }

    #[test]
    fn wire_shapes() {
        let session = SeatSession::Run {
            run_id: "r".into(),
            external_id: None,
            cwd: None,
        };
        assert_eq!(
            serde_json::to_value(&session).unwrap(),
            json!({ "kind": "run", "run_id": "r", "external_id": null, "cwd": null })
        );
        assert_eq!(
            serde_json::to_value(SeatResume::no(NotResumableReason::NoResumeId)).unwrap(),
            json!({ "resumable": false, "reason": "no_resume_id" })
        );
        assert_eq!(
            serde_json::to_value(SeatResume::yes()).unwrap(),
            json!({ "resumable": true })
        );
        let r: SeatSessionRef = serde_json::from_value(json!({
            "kind": "terminal", "terminalId": "t", "engineId": "claude-code", "cwd": "/x"
        }))
        .unwrap();
        assert!(
            matches!(r, SeatSessionRef::Terminal { ref terminal_id, .. } if terminal_id == "t")
        );
        let a: SeatAddress = serde_json::from_value(json!({ "seatId": "abc" })).unwrap();
        assert!(matches!(a, SeatAddress::Id { ref seat_id } if seat_id == "abc"));
        let a: SeatAddress = serde_json::from_value(json!({ "address": "@lead" })).unwrap();
        assert!(matches!(a, SeatAddress::Address { .. }));
        let actor: SeatActor =
            serde_json::from_value(json!({ "client": "iyke", "hold": true, "holdTtlMs": 5 }))
                .unwrap();
        assert_eq!(hold_ttl(&actor), HOLD_TTL_MIN_MS);
        let start: SeatStart = serde_json::from_value(json!({ "kind": "empty" })).unwrap();
        assert!(matches!(start, SeatStart::Empty));
        assert_eq!(
            SeatError::not_resumable(NotResumableReason::ProcessLocal).http_status(),
            422
        );
        assert_eq!(SeatError::seat_not_found().http_status(), 404);
        assert_eq!(SeatError::engine_failed("x".into()).http_status(), 502);
        assert_eq!(SeatError::resuming("a").http_status(), 409);
    }

    // ── WP-70 bridge sends ──────────────────────────────────────────────

    async fn must_not_send(why: &'static str) -> Result<String, SeatError> {
        panic!("{why}")
    }

    #[tokio::test]
    async fn touch_last_active_bumps_any_seat() {
        let pool = pool().await;
        let s = create(&pool, "lead", "claude-code").await;
        touch_last_active(&pool, &s, 9_999_999_999_999).await.unwrap();
        assert_eq!(row(&pool, &s).await.last_active_at, 9_999_999_999_999);
    }

    #[tokio::test]
    async fn send_idle_sends_once_and_bumps_the_seat() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-i", "claude-code", "done", Some("conv-i")).await;
        set_session(&pool, &s, "run", "run-i", Some("conv-i")).await;
        let before = row(&pool, &s).await.last_active_at;
        let (out, effects) = send_idle_core(&pool, &s, "run-i", &ui(), || async {
            Ok::<_, SeatError>("run-i".to_string())
        })
        .await
        .unwrap();
        assert_eq!(
            out,
            IdleSend::Sent {
                run_id: "run-i".into()
            }
        );
        assert_eq!(effects.events.len(), 1);
        assert_eq!(effects.events[0].kinds, vec!["updated"]);
        assert!(row(&pool, &s).await.last_active_at >= before);
    }

    #[tokio::test]
    async fn send_idle_queues_when_the_run_went_busy_or_a_text_waits() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-b", "claude-code", "running", Some("conv-b")).await;
        set_session(&pool, &s, "run", "run-b", Some("conv-b")).await;
        let (out, _) = send_idle_core(&pool, &s, "run-b", &ui(), || {
            must_not_send("a busy run is never resumed over (§4.5)")
        })
        .await
        .unwrap();
        assert_eq!(out, IdleSend::Queue);

        sqlx::query("UPDATE chi_cache SET status = 'done' WHERE run_id = 'run-b'")
            .execute(&pool)
            .await
            .unwrap();
        store().enqueue(
            &s,
            QueuedText {
                prompt: "first".into(),
                since: 0,
                client: "ui".into(),
            },
        );
        let (out, _) = send_idle_core(&pool, &s, "run-b", &ui(), || {
            must_not_send("a direct send never overtakes a queued text (§4.5)")
        })
        .await
        .unwrap();
        assert_eq!(out, IdleSend::Queue);
        store().dequeue(&s);
    }

    #[tokio::test]
    async fn send_idle_refuses_a_seat_that_moved_or_is_held() {
        let pool = pool().await;
        let s = create(&pool, "nightly", "claude-code").await;
        insert_run(&pool, "run-m", "claude-code", "done", Some("conv-m")).await;
        insert_run(&pool, "run-n", "claude-code", "done", Some("conv-n")).await;
        set_session(&pool, &s, "run", "run-n", Some("conv-n")).await;
        let e = send_idle_core(&pool, &s, "run-m", &ui(), || {
            must_not_send("nothing is sent to a run the seat no longer holds")
        })
        .await
        .unwrap_err();
        assert_eq!(e.code, "conflict");

        sqlx::query(
            "UPDATE iyke_seats SET hold_client = 'orchestrator', hold_since = 0,
                    hold_expires_at = ? WHERE id = ?",
        )
        .bind(now_ms() + 60_000)
        .bind(s.clone())
        .execute(&pool)
        .await
        .unwrap();
        let e = send_idle_core(&pool, &s, "run-n", &ui(), || {
            must_not_send("a hold taken since resolve is honoured")
        })
        .await
        .unwrap_err();
        assert_eq!(e.code, "seat_held");
    }
}
