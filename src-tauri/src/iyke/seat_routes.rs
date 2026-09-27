//! WP-70: the iyke bridge surface for Chi seats — G-SEATS §7.2
//! (`plans/shell-ux-rearchitecture/drafts/seats-schema.md`).
//!
//! Every handler here is a thin call into WP-65's seat store
//! (`iyke/seats.rs`): the `seats_*` Tauri commands own the per-seat mutex,
//! the world snapshot, the write transaction and the `seats://changed`
//! events, so the bridge and the UI share one code path into the store.
//!
//! | Route | Store call |
//! |---|---|
//! | `GET  /iyke/seats/list?project=` | `seats_list` |
//! | `GET  /iyke/seats/get?seat=` | `seats_get` |
//! | `POST /iyke/seats/create` | `seats_create` (a `session` / `resume` ref is a move) |
//! | `POST /iyke/seats/resume` | `session` → `seats_move`; `prompt` → `seats_resume` (`refuse`); else `409 needs_prompt` |
//! | `POST /iyke/seats/fill` | `prompt` → `seats_fill`; else `409 needs_prompt` |
//! | `POST /iyke/seats/clear` | `seats_clear` |
//! | `POST /iyke/seats/send` | `seats_resolve`, then the route it names (below) |
//! | `POST /iyke/seats/release` | `seats_release` |
//!
//! `/iyke/seats/send` follows `seats_resolve` (§7.2, §9.4):
//! - an occupied terminal → the `/iyke/terminal/send` write path
//!   (`PtyManager::controlled_write`), under that PTY's lease;
//! - a busy run → `seats_queue` (§4.5);
//! - an idle run → the `chi_resume` core;
//! - a vacant seat → `seats_resume` with `fallback: 'fresh'` (path H).
//!
//! Every POST body takes the §5 actor fields `client`, `hold`, `takeover`
//! (and optional `hold_ttl_ms`). Errors are the §9.5 `{code, message,
//! details?}` body at the code's HTTP status (`SeatError::http_status`,
//! with erratum E-3's `internal` → 500). A body the route can't parse is
//! `400 invalid_request` — a bridge-only code, never emitted by the store.

use std::sync::Arc;

use axum::{
    extract::{Json as JsonBody, Query},
    http::StatusCode,
    Extension, Json,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use super::seats::{
    self, CreateSeatReq, NotResumableReason, ResolveOpts, ResumeFallback, ResumeOpts,
    ResumeOutcome, SeatActor, SeatAddress, SeatError, SeatFillResult, SeatResumeResult, SeatRoute,
    SeatSessionRef, SeatStart, SeatView,
};
use crate::commands::chi::{resume_chi_run, ChiCache, ChiRuntime};
use crate::commands::db::PaDb;
use crate::pty::{PtyManager, TerminalDescriptor};

/// The §5 client id a bridge caller that names none is recorded as — the
/// same default the `iyke` CLI's `--as` uses.
const DEFAULT_CLIENT: &str = "iyke";

type ApiError = (StatusCode, Json<SeatError>);
type ApiResult<T> = Result<Json<T>, ApiError>;

// ═══════════════════════════════════════════════════════════════════════
// Errors
// ═══════════════════════════════════════════════════════════════════════

fn seat_error(code: &'static str, message: impl Into<String>) -> SeatError {
    SeatError {
        code,
        message: message.into(),
        details: None,
    }
}

/// A body or query the route can't use. Bridge-only; the store never emits it.
fn invalid_request(message: impl Into<String>) -> SeatError {
    seat_error("invalid_request", message)
}

/// §7.2 / P-9: resume and fill without a prompt start an interactive agent
/// terminal, which needs the UI; over the bridge they are refused in Phase 7.
fn needs_prompt(verb: &str) -> SeatError {
    seat_error(
        "needs_prompt",
        format!(
            "{verb} without a prompt starts an interactive agent terminal, which needs the \
             Ikenga window — pass a prompt, or use the seat's {verb} in the app"
        ),
    )
}

/// §9.5 code → HTTP status. `invalid_request` is the bridge's own 400; every
/// store code goes through `SeatError::http_status` (erratum E-3 included).
pub(crate) fn status_for(err: &SeatError) -> StatusCode {
    let code = match err.code {
        "invalid_request" => 400,
        _ => err.http_status(),
    };
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn api_err(err: SeatError) -> ApiError {
    (status_for(&err), Json(err))
}

/// A failed PTY write on the pty route. The lease case names its holder, so
/// a caller learns about both the seat hold and the PTY lease (§5.4).
pub(crate) fn pty_write_error(
    terminal_id: &str,
    message: &str,
    lease_holder: Option<&str>,
) -> SeatError {
    if message.contains("leased by") {
        let mut err = seat_error(
            "conflict",
            format!("terminal {terminal_id} refused the write: {message}"),
        );
        err.details = Some(json!({ "terminal_id": terminal_id, "lease_holder": lease_holder }));
        err
    } else if message.contains("exited") || message.contains("unknown terminal") {
        seat_error(
            "terminal_not_found",
            format!("terminal {terminal_id:?} is not running in this app ({message})"),
        )
    } else {
        seat_error(
            "conflict",
            format!("terminal {terminal_id} refused the write: {message}"),
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Request shapes (§7.2)
// ═══════════════════════════════════════════════════════════════════════

/// The §5 actor fields every POST body carries.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub(crate) struct ActorFields {
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub hold: bool,
    #[serde(default)]
    pub takeover: bool,
    #[serde(default)]
    pub hold_ttl_ms: Option<i64>,
}

impl ActorFields {
    pub(crate) fn actor(&self) -> SeatActor {
        let client = self
            .client
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .unwrap_or(DEFAULT_CLIENT)
            .to_string();
        SeatActor {
            client,
            hold: self.hold,
            takeover: self.takeover,
            hold_ttl_ms: self.hold_ttl_ms,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct CreateBody {
    #[serde(default)]
    pub project: Option<String>,
    pub name: String,
    #[serde(default)]
    pub engine: Option<String>,
    /// An open session (terminal id or run id) — a move.
    #[serde(default)]
    pub session: Option<String>,
    /// A past session (terminal id or run id) — also a move; it resumes on
    /// the next send.
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(flatten)]
    pub actor: ActorFields,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct ResumeBody {
    pub seat: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(flatten)]
    pub actor: ActorFields,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct FillBody {
    pub seat: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(flatten)]
    pub actor: ActorFields,
}

/// `clear` and `release`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct SeatBody {
    pub seat: String,
    #[serde(flatten)]
    pub actor: ActorFields,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct SendBody {
    pub seat: String,
    pub text: String,
    /// The PTY lease token, when the seat's terminal is leased (§5.4).
    #[serde(default)]
    pub lease_token: Option<String>,
    #[serde(flatten)]
    pub actor: ActorFields,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GetQuery {
    #[serde(default)]
    pub seat: Option<String>,
}

/// Parse a POST body; a shape error is `400 invalid_request`.
pub(crate) fn parse_body<T: DeserializeOwned>(body: Value) -> Result<T, SeatError> {
    serde_json::from_value(body).map_err(|e| invalid_request(format!("bad request body: {e}")))
}

/// A `seat` field: any §1.3 iyke form (`<name>`, `@<name>`,
/// `<project>/<name>`, `seat:<project>/<name>`). The store's resolver does
/// the parsing; an empty value is refused here.
pub(crate) fn seat_address(seat: &str) -> Result<SeatAddress, SeatError> {
    let seat = seat.trim();
    if seat.is_empty() {
        return Err(invalid_request("`seat` must not be empty"));
    }
    Ok(SeatAddress::Address {
        address: seat.to_string(),
    })
}

/// A prompt that is absent or blank is no prompt (§7.2 `needs_prompt`).
pub(crate) fn prompt_of(prompt: Option<String>) -> Option<String> {
    prompt.filter(|p| !p.trim().is_empty())
}

/// Validate a create body into the parts the store needs: the explicit
/// engine (if any) and the session ref (if any). `session` and `resume` are
/// both a move (§7.2) and are mutually exclusive.
pub(crate) fn create_parts(body: &CreateBody) -> Result<Option<&str>, SeatError> {
    let session = body
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let resume = body
        .resume
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (session, resume) {
        (Some(_), Some(_)) => Err(invalid_request(
            "set at most one of `session` (an open session) and `resume` (a past one)",
        )),
        (Some(r), None) | (None, Some(r)) => Ok(Some(r)),
        (None, None) => Ok(None),
    }
}

/// The Chi engine a wrap-engine terminal runs, read off its argv (the
/// agent-wrap script names the CLI it launches: `claude`, `codex`, `agy`,
/// `gemini`). `None` unless exactly one engine is named — a plain shell, or
/// an argv naming two, is not guessed at.
pub(crate) fn infer_terminal_engine(argv: &[String]) -> Option<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for arg in argv {
        for token in arg.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
            let engine = match token {
                "claude" => "claude-code",
                "codex" => "codex",
                "agy" | "antigravity" => "antigravity-cli",
                // No Chi id: `seats_create` / `seats_move` refuse it by engine.
                "gemini" => "gemini",
                _ => continue,
            };
            if !found.contains(&engine) {
                found.push(engine);
            }
        }
    }
    match found.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// The terminal a `<ref>` names: by terminal id, then PTY id, preferring a
/// running PTY (the same order the store's `session_spec` uses).
pub(crate) fn find_terminal<'a>(
    terminals: &'a [TerminalDescriptor],
    reference: &str,
) -> Option<&'a TerminalDescriptor> {
    terminals
        .iter()
        .filter(|t| t.terminal_id == reference)
        .max_by_key(|t| t.status == "running")
        .or_else(|| terminals.iter().find(|t| t.pty_id == reference))
}

/// What a `<ref>` (§7.3: "a terminal id or a run id") is.
enum RefKind {
    Run {
        engine_id: String,
    },
    Terminal {
        terminal_id: String,
        inferred: Option<&'static str>,
    },
}

// ═══════════════════════════════════════════════════════════════════════
// Plumbing
// ═══════════════════════════════════════════════════════════════════════

fn db_state(app: &AppHandle) -> Result<State<'_, Arc<PaDb>>, SeatError> {
    app.try_state::<Arc<PaDb>>()
        .ok_or_else(|| seat_error("internal", "the database is not ready"))
}

/// Classify a `<ref>`: a `chi_cache` row identifies a run id (§7.3);
/// otherwise a terminal this app's `PtyManager` knows. Neither is
/// `409 conflict` (erratum E-3's refusal for a run ref with no row).
async fn classify_ref(
    app: &AppHandle,
    pty_manager: &PtyManager,
    reference: &str,
) -> Result<RefKind, SeatError> {
    let pool = db_state(app)?
        .ensure_pool()
        .await
        .map_err(|e| seat_error("internal", e))?;
    let run_engine: Option<String> =
        sqlx::query_scalar::<_, String>("SELECT engine_id FROM chi_cache WHERE run_id = ?")
            .bind(reference.to_string())
            .fetch_optional(&pool)
            .await
            .map_err(|e| seat_error("internal", format!("chi_cache: {e}")))?;
    if let Some(engine_id) = run_engine {
        return Ok(RefKind::Run { engine_id });
    }
    let terminals = pty_manager.list_terminals();
    if let Some(t) = find_terminal(&terminals, reference) {
        return Ok(RefKind::Terminal {
            terminal_id: t.terminal_id.clone(),
            inferred: infer_terminal_engine(&t.argv),
        });
    }
    Err(seat_error(
        "conflict",
        format!("no Chi run or terminal in this app has the id {reference:?}"),
    ))
}

/// The store's session ref for a classified `<ref>`, and the session's
/// engine. A run's engine is its `chi_cache` engine. A terminal's is read off
/// its argv, else `fallback_engine` (the caller's `engine`, or the seat's own
/// engine on a resume); with neither, the caller must name it.
fn session_ref_for(
    reference: &str,
    kind: &RefKind,
    fallback_engine: Option<&str>,
) -> Result<(SeatSessionRef, String), SeatError> {
    match kind {
        RefKind::Run { engine_id } => Ok((
            SeatSessionRef::Run {
                run_id: reference.to_string(),
            },
            engine_id.clone(),
        )),
        RefKind::Terminal {
            terminal_id,
            inferred,
        } => {
            let engine = (*inferred).or(fallback_engine).ok_or_else(|| {
                invalid_request(format!(
                    "can't tell which engine terminal {terminal_id} runs — pass `engine`"
                ))
            })?;
            Ok((
                SeatSessionRef::Terminal {
                    terminal_id: terminal_id.clone(),
                    engine_id: engine.to_string(),
                    cwd: None,
                    external_id: None,
                },
                engine.to_string(),
            ))
        }
    }
}

async fn get_view(app: &AppHandle, seat: &str) -> Result<SeatView, SeatError> {
    let address = seat_address(seat)?;
    seats::seats_get(app.clone(), db_state(app)?, address).await
}

// ═══════════════════════════════════════════════════════════════════════
// Handlers
// ═══════════════════════════════════════════════════════════════════════

/// `GET /iyke/seats/list?project=<id>` — default: the active project.
pub async fn get_seats_list(
    Extension(app): Extension<AppHandle>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Vec<SeatView>> {
    let project = q.project.filter(|p| !p.trim().is_empty());
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_list(app.clone(), db, project)
        .await
        .map(Json)
        .map_err(api_err)
}

/// `GET /iyke/seats/get?seat=<address>`.
pub async fn get_seats_get(
    Extension(app): Extension<AppHandle>,
    Query(q): Query<GetQuery>,
) -> ApiResult<SeatView> {
    let seat = q
        .seat
        .ok_or_else(|| api_err(invalid_request("missing `seat` query parameter")))?;
    get_view(&app, &seat).await.map(Json).map_err(api_err)
}

/// `POST /iyke/seats/create` `{project?, name, engine?, session?, resume?}`.
/// Returns the new seat's `SeatView`.
pub async fn post_seats_create(
    Extension(app): Extension<AppHandle>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<SeatView> {
    let body: CreateBody = parse_body(body).map_err(api_err)?;
    let reference = create_parts(&body).map_err(api_err)?;
    let explicit_engine = body
        .engine
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let (engine_id, start) = match reference {
        None => {
            let engine = explicit_engine.ok_or_else(|| {
                api_err(invalid_request(
                    "`engine` is required when the seat starts without a session",
                ))
            })?;
            (engine.to_string(), SeatStart::Empty)
        }
        Some(r) => {
            let kind = classify_ref(&app, &pty_manager, r).await.map_err(api_err)?;
            let (session, session_engine) =
                session_ref_for(r, &kind, explicit_engine).map_err(api_err)?;
            // The seat's engine is the caller's; the store refuses a session
            // on another engine with `409 engine_mismatch` (§4.3).
            let engine = explicit_engine
                .map(str::to_string)
                .unwrap_or(session_engine);
            (engine, SeatStart::Session { session })
        }
    };
    let req = CreateSeatReq {
        project_id: body.project.clone().filter(|p| !p.trim().is_empty()),
        name: body.name.trim().to_string(),
        engine_id,
        start,
    };
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_create(app.clone(), db, req, body.actor.actor())
        .await
        .map(|r| Json(r.seat))
        .map_err(api_err)
}

/// `/iyke/seats/resume` answers with the moved seat's view, or the resume.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResumeReply {
    Moved(SeatView),
    Resumed(SeatResumeResult),
}

/// `POST /iyke/seats/resume` `{seat, session?, prompt?}`: `session` → a move
/// (DEC-69c); else `prompt` → `seats_resume` with `fallback: 'refuse'` (an
/// explicit resume never falls back, §6.2); else `409 needs_prompt`.
pub async fn post_seats_resume(
    Extension(app): Extension<AppHandle>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<ResumeReply> {
    let body: ResumeBody = parse_body(body).map_err(api_err)?;
    let session = body
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let prompt = prompt_of(body.prompt.clone());
    if session.is_some() && prompt.is_some() {
        return Err(api_err(invalid_request(
            "set `session` (seat that session) or `prompt` (resume the seat's own session \
             with it), not both",
        )));
    }
    let view = get_view(&app, &body.seat).await.map_err(api_err)?;
    let actor = body.actor.actor();
    if let Some(r) = session {
        let kind = classify_ref(&app, &pty_manager, r).await.map_err(api_err)?;
        let (session, _) =
            session_ref_for(r, &kind, Some(view.engine_id.as_str())).map_err(api_err)?;
        let db = db_state(&app).map_err(api_err)?;
        return seats::seats_move(app.clone(), db, session, view.id.clone(), actor, None)
            .await
            .map(|r| Json(ResumeReply::Moved(r.seat)))
            .map_err(api_err);
    }
    let Some(prompt) = prompt else {
        return Err(api_err(needs_prompt("resume")));
    };
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_resume(
        app.clone(),
        db,
        view.id.clone(),
        prompt,
        actor,
        ResumeOpts {
            fallback: ResumeFallback::Refuse,
        },
    )
    .await
    .map(|r| Json(ResumeReply::Resumed(r)))
    .map_err(api_err)
}

/// `POST /iyke/seats/fill` `{seat, prompt?}`: a new run on the seat's engine
/// with `prompt`; no prompt is `409 needs_prompt`.
pub async fn post_seats_fill(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<SeatFillResult> {
    let body: FillBody = parse_body(body).map_err(api_err)?;
    let Some(prompt) = prompt_of(body.prompt.clone()) else {
        return Err(api_err(needs_prompt("fill")));
    };
    let view = get_view(&app, &body.seat).await.map_err(api_err)?;
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_fill(
        app.clone(),
        db,
        view.id.clone(),
        prompt,
        body.actor.actor(),
        None,
    )
    .await
    .map(Json)
    .map_err(api_err)
}

/// `POST /iyke/seats/clear` `{seat}` — the pad is kept (DEC-69b).
pub async fn post_seats_clear(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<SeatView> {
    let body: SeatBody = parse_body(body).map_err(api_err)?;
    let view = get_view(&app, &body.seat).await.map_err(api_err)?;
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_clear(app.clone(), db, view.id.clone(), body.actor.actor())
        .await
        .map(Json)
        .map_err(api_err)
}

/// `POST /iyke/seats/release` `{seat}` — drops the caller's own hold;
/// someone else's needs `takeover` (§5.1).
pub async fn post_seats_release(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<SeatView> {
    let body: SeatBody = parse_body(body).map_err(api_err)?;
    let view = get_view(&app, &body.seat).await.map_err(api_err)?;
    let db = db_state(&app).map_err(api_err)?;
    seats::seats_release(app.clone(), db, view.id.clone(), body.actor.actor())
        .await
        .map(Json)
        .map_err(api_err)
}

/// `/iyke/seats/send`'s answer: `{route, run_id | null, outcome?, reason?,
/// queued?}` (§7.2), plus `terminal_id` on the pty route and the seat's view.
#[derive(Debug, Serialize)]
pub struct SeatSendResult {
    /// `pty` | `chi-resume` | `vacant` — the `seats_resolve` route taken.
    pub route: &'static str,
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ResumeOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotResumableReason>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
    pub seat: SeatView,
}

/// The bytes a pty-route send writes: the text, then Enter — the same
/// `${text}\r` the UI's PTY inject writes (`resolve-target.ts`).
pub(crate) fn pty_bytes(text: &str) -> Vec<u8> {
    let mut data = text.as_bytes().to_vec();
    data.push(b'\r');
    data
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Send to an idle run through the `chi_resume` core (same run id).
async fn resume_idle_run(
    app: &AppHandle,
    run_id: String,
    text: String,
) -> Result<String, SeatError> {
    let db: Arc<PaDb> = db_state(app)?.inner().clone();
    let cache: ChiCache = app
        .try_state::<ChiCache>()
        .ok_or_else(|| seat_error("internal", "the chi cache is not ready"))?
        .inner()
        .clone();
    let runtime: Arc<ChiRuntime> = app
        .try_state::<Arc<ChiRuntime>>()
        .ok_or_else(|| seat_error("internal", "the chi runtime is not ready"))?
        .inner()
        .clone();
    let result = resume_chi_run(app, db, &cache, &runtime, run_id, text)
        .await
        .map_err(|e| seat_error("engine_failed", e))?;
    if result.status == "failed" {
        return Err(seat_error(
            "engine_failed",
            result
                .error
                .unwrap_or_else(|| format!("run {} failed", result.run_id)),
        ));
    }
    Ok(result.run_id)
}

/// `POST /iyke/seats/send` `{seat, text}` — `iyke terminal-send --seat`.
/// Routes by `seats_resolve` (§7.2); a vacant seat resumes, then sends
/// (DEC-69a, path H, with the §6.2 fresh fallback).
pub async fn post_seats_send(
    Extension(app): Extension<AppHandle>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult<SeatSendResult> {
    let body: SendBody = parse_body(body).map_err(api_err)?;
    if body.text.trim().is_empty() {
        return Err(api_err(invalid_request("`text` must not be empty")));
    }
    let address = seat_address(&body.seat).map_err(api_err)?;
    let actor = body.actor.actor();

    // §9.4: a vacant seat that was filled between resolve and resume
    // (`seat_not_vacant`, a race) is resolved again, once.
    for attempt in 0..2 {
        let db = db_state(&app).map_err(api_err)?;
        let route = seats::seats_resolve(
            app.clone(),
            db,
            address.clone(),
            actor.clone(),
            // Path H only: the bridge never spawns a terminal, so it never
            // takes the path-T claim (§4.1, erratum E-1).
            Some(ResolveOpts {
                claim_resume: false,
            }),
        )
        .await
        .map_err(api_err)?;
        match route {
            SeatRoute::Pty {
                seat,
                terminal_id,
                lease_holder,
                ..
            } => {
                // `agent: 'unreported'` behaves exactly as `--label` does:
                // no new guard, no weaker one (§7.2).
                pty_manager
                    .controlled_write(
                        &terminal_id,
                        &pty_bytes(&body.text),
                        None,
                        Some(actor.client.as_str()),
                        body.lease_token.as_deref(),
                        false,
                    )
                    .map_err(|e| {
                        api_err(pty_write_error(
                            &terminal_id,
                            &e.to_string(),
                            lease_holder.as_deref(),
                        ))
                    })?;
                return Ok(Json(SeatSendResult {
                    route: "pty",
                    run_id: None,
                    terminal_id: Some(terminal_id),
                    outcome: None,
                    reason: None,
                    queued: false,
                    seat,
                }));
            }
            SeatRoute::ChiResume {
                seat,
                run_id,
                busy: true,
            } => {
                // §4.5: never `chi_resume` over a turn in flight — queue.
                let db = db_state(&app).map_err(api_err)?;
                let seat = seats::seats_queue(
                    app.clone(),
                    db,
                    seat.id.clone(),
                    body.text.clone(),
                    actor.clone(),
                )
                .await
                .map_err(api_err)?;
                return Ok(Json(SeatSendResult {
                    route: "chi-resume",
                    run_id: Some(run_id),
                    terminal_id: None,
                    outcome: None,
                    reason: None,
                    queued: true,
                    seat,
                }));
            }
            SeatRoute::ChiResume {
                seat,
                run_id,
                busy: false,
            } => {
                let sent = resume_idle_run(&app, run_id, body.text.clone())
                    .await
                    .map_err(api_err)?;
                // §1.1: a send through the seat bumps `last_active_at`. The
                // text is already out, so a failed bump is only logged.
                if let Ok(db) = db_state(&app) {
                    if let Ok(pool) = db.ensure_pool().await {
                        if let Err(e) = seats::touch_after_send(&pool, &seat.id, now_ms()).await {
                            log::warn!(target: "ikenga::seats", "send: touch {}: {e}", seat.id);
                        }
                    }
                }
                return Ok(Json(SeatSendResult {
                    route: "chi-resume",
                    run_id: Some(sent),
                    terminal_id: None,
                    outcome: None,
                    reason: None,
                    queued: false,
                    seat,
                }));
            }
            SeatRoute::Vacant { seat, .. } => {
                let db = db_state(&app).map_err(api_err)?;
                match seats::seats_resume(
                    app.clone(),
                    db,
                    seat.id.clone(),
                    body.text.clone(),
                    actor.clone(),
                    ResumeOpts {
                        fallback: ResumeFallback::Fresh,
                    },
                )
                .await
                {
                    Ok(r) => {
                        return Ok(Json(SeatSendResult {
                            route: "vacant",
                            run_id: Some(r.run_id),
                            terminal_id: None,
                            outcome: Some(r.outcome),
                            reason: r.reason,
                            queued: false,
                            seat: r.seat,
                        }));
                    }
                    Err(e) if e.code == "seat_not_vacant" && attempt == 0 => continue,
                    Err(e) => return Err(api_err(e)),
                }
            }
        }
    }
    Err(api_err(seat_error(
        "conflict",
        "the seat changed twice while sending; nothing was sent — retry",
    )))
}

// ═══════════════════════════════════════════════════════════════════════
// Tests — written under DEC-50 (not run before CI).
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iyke::seats::{parse_seat_address, ParsedAddress};

    fn err(code: &'static str) -> SeatError {
        seat_error(code, "x")
    }

    fn descriptor(terminal_id: &str, pty_id: &str, status: &'static str) -> TerminalDescriptor {
        TerminalDescriptor {
            terminal_id: terminal_id.to_string(),
            pty_id: pty_id.to_string(),
            title: String::new(),
            label: None,
            cwd: "/work".to_string(),
            argv: vec!["bash".to_string()],
            status,
            pid: None,
            foreground_command: None,
            created_at: 0,
            exited_at: None,
            exit_code: None,
            output_start_offset: 0,
            output_end_offset: 0,
            owner_agent_id: None,
            lease_expires_at: None,
            mounted: false,
            focused: false,
            pane_ids: Vec::new(),
            window_labels: Vec::new(),
        }
    }

    // ── error mapping (§9.5 + E-3) ─────────────────────────────────────

    #[test]
    fn every_seat_error_code_maps_to_its_http_status() {
        let table: &[(&'static str, u16)] = &[
            ("invalid_seat_name", 400),
            ("invalid_address", 400),
            ("invalid_request", 400),
            ("seat_not_found", 404),
            ("project_not_found", 404),
            ("seat_name_taken", 409),
            ("rename_scope_conflict", 409),
            ("engine_mismatch", 409),
            ("terminal_not_found", 409),
            ("seat_not_vacant", 409),
            ("seat_resuming", 409),
            ("seat_busy", 409),
            ("agent_not_live", 409),
            ("conflict", 409),
            ("needs_prompt", 409),
            ("seat_held", 409),
            ("seat_taken_over", 409),
            ("not_resumable", 422),
            ("engine_unsupported", 422),
            ("internal", 500),
            ("engine_failed", 502),
        ];
        for &(code, status) in table {
            assert_eq!(
                status_for(&err(code)).as_u16(),
                status,
                "{code} should be HTTP {status}"
            );
        }
    }

    #[test]
    fn error_body_is_code_message_and_optional_details() {
        let (status, Json(body)) = api_err(needs_prompt("resume"));
        assert_eq!(status, StatusCode::CONFLICT);
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["code"], "needs_prompt");
        assert!(v["message"].as_str().unwrap().contains("resume"));
        assert!(
            v.get("details").is_none(),
            "no details key when there are none"
        );

        let (status, Json(body)) = api_err(invalid_request("bad"));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::to_value(&body).unwrap()["code"],
            "invalid_request"
        );
    }

    #[test]
    fn pty_write_errors_name_the_lease_holder_or_a_missing_terminal() {
        let leased = pty_write_error(
            "t1",
            "terminal is leased by orchestrator",
            Some("orchestrator"),
        );
        assert_eq!(leased.code, "conflict");
        assert_eq!(status_for(&leased), StatusCode::CONFLICT);
        assert_eq!(
            leased.details.as_ref().unwrap()["lease_holder"],
            "orchestrator"
        );

        let gone = pty_write_error("t1", "terminal has exited", None);
        assert_eq!(gone.code, "terminal_not_found");
        let unknown = pty_write_error("t1", "unknown terminal", None);
        assert_eq!(unknown.code, "terminal_not_found");

        let other = pty_write_error("t1", "terminal write exceeds 1 MB", None);
        assert_eq!(other.code, "conflict");
        assert!(other.details.is_none());
    }

    // ── request parsing ────────────────────────────────────────────────

    #[test]
    fn actor_fields_default_the_client_to_iyke() {
        let body: SeatBody = parse_body(json!({ "seat": "lead" })).unwrap();
        let actor = body.actor.actor();
        assert_eq!(actor.client, "iyke");
        assert!(!actor.hold);
        assert!(!actor.takeover);
        assert_eq!(actor.hold_ttl_ms, None);

        let blank: SeatBody = parse_body(json!({ "seat": "lead", "client": "  " })).unwrap();
        assert_eq!(blank.actor.actor().client, "iyke");
    }

    #[test]
    fn every_post_body_carries_client_hold_and_takeover() {
        let actor = json!({ "client": "orchestrator", "hold": true, "takeover": true,
                            "hold_ttl_ms": 90000 });
        let with = |mut v: Value| {
            v.as_object_mut()
                .unwrap()
                .extend(actor.as_object().unwrap().clone());
            v
        };
        let expected = ActorFields {
            client: Some("orchestrator".into()),
            hold: true,
            takeover: true,
            hold_ttl_ms: Some(90_000),
        };
        let create: CreateBody = parse_body(with(json!({ "name": "lead" }))).unwrap();
        assert_eq!(create.actor, expected);
        let resume: ResumeBody = parse_body(with(json!({ "seat": "lead" }))).unwrap();
        assert_eq!(resume.actor, expected);
        let fill: FillBody = parse_body(with(json!({ "seat": "lead" }))).unwrap();
        assert_eq!(fill.actor, expected);
        let seat: SeatBody = parse_body(with(json!({ "seat": "lead" }))).unwrap();
        assert_eq!(seat.actor, expected);
        let send: SendBody = parse_body(with(json!({ "seat": "lead", "text": "hi" }))).unwrap();
        assert_eq!(send.actor, expected);

        let a = expected.actor();
        assert_eq!(a.client, "orchestrator");
        assert!(a.hold && a.takeover);
        assert_eq!(a.hold_ttl_ms, Some(90_000));
    }

    #[test]
    fn create_body_parses_the_locked_form_lines() {
        // `seat create docs --engine claude-code`
        let b: CreateBody = parse_body(json!({ "name": "docs", "engine": "claude-code" })).unwrap();
        assert_eq!(b.engine.as_deref(), Some("claude-code"));
        assert_eq!(create_parts(&b).unwrap(), None);

        // `seat create docs --session <id>`
        let b: CreateBody = parse_body(json!({ "name": "docs", "session": "t-1" })).unwrap();
        assert_eq!(create_parts(&b).unwrap(), Some("t-1"));

        // `seat create docs --engine claude-code --resume <id> --project royalti-co`
        let b: CreateBody = parse_body(json!({
            "name": "docs", "engine": "claude-code", "resume": "run-9", "project": "royalti-co"
        }))
        .unwrap();
        assert_eq!(create_parts(&b).unwrap(), Some("run-9"));
        assert_eq!(b.project.as_deref(), Some("royalti-co"));

        // both refs → 400
        let b: CreateBody =
            parse_body(json!({ "name": "docs", "session": "a", "resume": "b" })).unwrap();
        assert_eq!(create_parts(&b).unwrap_err().code, "invalid_request");

        // a blank ref is no ref
        let b: CreateBody = parse_body(json!({ "name": "docs", "session": "  " })).unwrap();
        assert_eq!(create_parts(&b).unwrap(), None);
    }

    #[test]
    fn malformed_bodies_are_invalid_request() {
        let e = parse_body::<CreateBody>(json!({ "engine": "codex" })).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        assert_eq!(status_for(&e), StatusCode::BAD_REQUEST);
        let e = parse_body::<SendBody>(json!({ "seat": "lead" })).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        let e = parse_body::<SeatBody>(json!({ "seat": 7 })).unwrap_err();
        assert_eq!(e.code, "invalid_request");
        let e = parse_body::<SeatBody>(json!({ "seat": "lead", "hold": "yes" })).unwrap_err();
        assert_eq!(e.code, "invalid_request");
    }

    #[test]
    fn a_missing_or_blank_prompt_is_no_prompt() {
        assert_eq!(prompt_of(None), None);
        assert_eq!(prompt_of(Some("   ".into())), None);
        assert_eq!(prompt_of(Some("go".into())).as_deref(), Some("go"));

        let r: ResumeBody = parse_body(json!({ "seat": "docs" })).unwrap();
        assert_eq!(prompt_of(r.prompt), None);
        let f: FillBody = parse_body(json!({ "seat": "docs", "prompt": "" })).unwrap();
        assert_eq!(prompt_of(f.prompt), None);
        assert_eq!(needs_prompt("fill").code, "needs_prompt");
    }

    #[test]
    fn send_body_takes_text_and_an_optional_lease_token() {
        let b: SendBody =
            parse_body(json!({ "seat": "lead", "text": "hi", "lease_token": "tok" })).unwrap();
        assert_eq!(b.text, "hi");
        assert_eq!(b.lease_token.as_deref(), Some("tok"));
        assert_eq!(pty_bytes("hi"), b"hi\r".to_vec());
    }

    // ── address resolution (§1.3) ──────────────────────────────────────

    #[test]
    fn seat_field_accepts_every_iyke_form() {
        for form in ["lead", "@lead", "royalti-co/lead", "seat:royalti-co/lead"] {
            match seat_address(form).unwrap() {
                SeatAddress::Address { address } => assert_eq!(address, form),
                other => panic!("expected an address, got {other:?}"),
            }
        }
        assert_eq!(seat_address("  ").unwrap_err().code, "invalid_request");
        assert_eq!(
            parse_seat_address("lead").unwrap(),
            ParsedAddress::Bare {
                name: "lead".into()
            }
        );
        assert_eq!(
            parse_seat_address("@lead").unwrap(),
            ParsedAddress::Bare {
                name: "lead".into()
            }
        );
        let qualified = ParsedAddress::Qualified {
            project: "royalti-co".into(),
            name: "lead".into(),
        };
        assert_eq!(parse_seat_address("royalti-co/lead").unwrap(), qualified);
        assert_eq!(
            parse_seat_address("seat:royalti-co/lead").unwrap(),
            qualified
        );
    }

    #[test]
    fn malformed_addresses_are_400_invalid_address() {
        for bad in [
            "seat:lead",
            "Lead",
            "a/b/c",
            "seat:royalti-co/",
            "@",
            "-lead",
        ] {
            let e = parse_seat_address(bad).unwrap_err();
            assert_eq!(e.code, "invalid_address", "{bad}");
            assert_eq!(status_for(&e), StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[test]
    fn a_ref_finds_its_terminal_by_terminal_id_then_pty_id() {
        let terminals = vec![
            descriptor("tab-1", "pty-old", "exited"),
            descriptor("tab-1", "pty-new", "running"),
            descriptor("tab-2", "pty-2", "running"),
        ];
        assert_eq!(
            find_terminal(&terminals, "tab-1").unwrap().pty_id,
            "pty-new"
        );
        assert_eq!(
            find_terminal(&terminals, "pty-2").unwrap().terminal_id,
            "tab-2"
        );
        assert!(find_terminal(&terminals, "nope").is_none());
    }

    #[test]
    fn a_terminal_engine_is_read_off_its_wrap_argv() {
        let argv = |s: &str| vec!["bash".to_string(), "-lc".to_string(), s.to_string()];
        assert_eq!(
            infer_terminal_engine(&argv(
                "claude --settings '/data/app.ikenga/claude-hooks-t1.json' 'go'; exec \"${SHELL:-bash}\" -i"
            )),
            Some("claude-code")
        );
        assert_eq!(
            infer_terminal_engine(&argv("codex resume abc")),
            Some("codex")
        );
        assert_eq!(
            infer_terminal_engine(&argv("agy --conversation x")),
            Some("antigravity-cli")
        );
        assert_eq!(infer_terminal_engine(&argv("gemini")), Some("gemini"));
        // a plain shell, or two engines named: not guessed
        assert_eq!(
            infer_terminal_engine(&["bash".to_string(), "-l".to_string()]),
            None
        );
        assert_eq!(infer_terminal_engine(&argv("claude; codex")), None);
        // `claude-hooks-…` alone is not the `claude` token
        assert_eq!(
            infer_terminal_engine(&argv("cat claude-hooks-t1.json")),
            None
        );
    }

    #[test]
    fn a_ref_becomes_the_store_session_ref() {
        let run = RefKind::Run {
            engine_id: "codex".into(),
        };
        let (session, engine) = session_ref_for("run-9", &run, Some("claude-code")).unwrap();
        assert!(matches!(session, SeatSessionRef::Run { ref run_id } if run_id == "run-9"));
        assert_eq!(engine, "codex", "a run's engine is its chi_cache engine");

        let inferred = RefKind::Terminal {
            terminal_id: "tab-1".into(),
            inferred: Some("claude-code"),
        };
        let (session, engine) = session_ref_for("pty-1", &inferred, Some("codex")).unwrap();
        match session {
            SeatSessionRef::Terminal {
                terminal_id,
                engine_id,
                cwd,
                external_id,
            } => {
                assert_eq!(
                    terminal_id, "tab-1",
                    "the tab id, not the pty id the caller gave"
                );
                assert_eq!(engine_id, "claude-code", "argv wins over the fallback");
                assert!(cwd.is_none() && external_id.is_none());
            }
            other => panic!("expected a terminal ref, got {other:?}"),
        }
        assert_eq!(engine, "claude-code");

        let unknown = RefKind::Terminal {
            terminal_id: "tab-2".into(),
            inferred: None,
        };
        let (_, engine) = session_ref_for("tab-2", &unknown, Some("codex")).unwrap();
        assert_eq!(engine, "codex");
        let e = session_ref_for("tab-2", &unknown, None).unwrap_err();
        assert_eq!(e.code, "invalid_request");
    }
}
