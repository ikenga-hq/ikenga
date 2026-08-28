//! HTTP handlers for the Iyke server.
//!
//! Read side: `GET /iyke/state` returns the shell snapshot the FE has
//! pushed via `iyke_set_shell`.
//!
//! Write side (Phase 11 Day 2/3): `POST /iyke/{go,mode,open,split,focus,close}`
//! validate their bodies and emit a typed Tauri event. A FE listener
//! mounted in `<Workspace />` translates events into `usePaneStore` /
//! `useShellStore` mutations. Handlers return 200 immediately — think
//! "command accepted" rather than "command executed". Empirical latency
//! is sub-frame, so callers don't notice.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Json as JsonBody, Query},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, LogicalSize, Manager};

use super::rpc;
use super::state::{IykeState, LogEntry, NetworkEntry};
use super::IykeRpc;
use crate::commands::chi::{chi_cancel, chi_list, chi_resume, chi_run, chi_status, ChiCache, ChiRunOpts, ChiRuntime};
use crate::commands::db::PaDb;
use crate::pty::PtyManager;

const DOM_TIMEOUT: Duration = Duration::from_secs(5);
const QUERY_CACHE_TIMEOUT: Duration = Duration::from_secs(5);
const WAIT_TIMEOUT_DEFAULT_MS: u64 = 10_000;
const WAIT_TIMEOUT_MAX_MS: u64 = 60_000;
const CLICK_TIMEOUT: Duration = Duration::from_secs(5);

// --- types shared with the IykeRpc bundle ---------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomResult {
    /// Plaintext snapshot in Playwright-style accessibility tree format.
    pub text: String,
    /// Same tree as structured JSON (array of { role, name, ref, value, children }).
    pub json: Value,
    /// Snapshot generation. FE bumps it each time so callers can detect
    /// stale refs. Echoed in click/type/key responses.
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalReadResult {
    pub text: String,
    pub bytes_available: usize,
    pub bytes_returned: usize,
    pub session_id: Option<String>,
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub pty_id: Option<String>,
    #[serde(default)]
    pub start_offset: u64,
    #[serde(default)]
    pub end_offset: u64,
    #[serde(default)]
    pub available_start_offset: u64,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub exited: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryCacheResult {
    /// Array of { queryKey, status, dataUpdatedAt, errorUpdatedAt, fetchStatus,
    ///            isStale, error?, dataPreview? }.
    pub entries: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub satisfied: bool,
    pub elapsed_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// FE → host result for a click/type/key round-trip: did the FE resolve the
/// target (ref / selector / text) to a live element and action it? Carried on
/// the shared `IykeRpc::action` channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub matched: bool,
}

/// HTTP response for the click/type/key endpoints. Mirrors the iframe bridge's
/// `doClick` contract: `ok:true` when an element was matched and actioned,
/// otherwise `ok:false` with a human error. HTTP stays 200 either way — a
/// missed target is a well-formed request that found nothing, not a fault.
#[derive(Serialize)]
pub struct ActionResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct StateResponse {
    pub schema_version: u32,
    pub app: AppInfo,
    pub shell: ShellInfo,
    pub terminals: Vec<crate::pty::TerminalDescriptor>,
    pub windows: Vec<super::terminal::IykeWindowInfo>,
}

#[derive(Serialize)]
pub struct AppInfo {
    pub pid: u32,
    pub started_at_unix_ms: u128,
    pub identifier: &'static str,
}

#[derive(Serialize)]
pub struct ShellInfo {
    pub mode: Option<String>,
    pub route: Option<String>,
    /// Phase 12: opaque pane-tree blob pushed by the FE. `null` when the
    /// FE hasn't pushed anything yet. See
    /// `ikenga-desktop/src/lib/iyke/use-iyke-shell-sync.ts` for the
    /// schema (`{ leaves: [...], tree }`).
    pub panes: Option<Value>,
    /// Sidebar collapsed state, mirrored from `shell-store`. `null` until the
    /// FE has pushed once. Present so `/iyke/sidebar` is observable and not
    /// just actuate-only.
    pub sidebar_collapsed: Option<bool>,
}

pub async fn get_state(
    Extension(state): Extension<Arc<IykeState>>,
    Extension(app): Extension<AppHandle>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
) -> Json<StateResponse> {
    let shell = state.snapshot().await;
    let registry = app.state::<crate::window::registry::WindowRegistry>();
    let mut windows = vec![super::terminal::IykeWindowInfo::from_descriptor(
        crate::window::descriptor::WindowDescriptor {
            label: "main".to_string(),
            kind: crate::window::descriptor::WindowKind::Primary,
            surface_set: Vec::new(),
            project_id: None,
            layout_key: "main".to_string(),
        },
        shell.panes.clone(),
    )];
    let detached_windows = registry.list_live(&app);
    windows.extend(
        detached_windows
            .iter()
            .cloned()
            .map(|descriptor| super::terminal::IykeWindowInfo::from_descriptor(descriptor, None)),
    );
    let mut terminals = pty_manager.list_terminals();
    super::terminal::enrich_terminals(&mut terminals, shell.panes.as_ref(), &detached_windows);
    Json(StateResponse {
        schema_version: 1,
        app: AppInfo {
            pid: state.pid(),
            started_at_unix_ms: state.started_at_unix_ms(),
            identifier: "app.ikenga",
        },
        shell: ShellInfo {
            mode: shell.mode,
            route: shell.route,
            panes: shell.panes,
            sidebar_collapsed: shell.sidebar_collapsed,
        },
        terminals,
        windows,
    })
}

#[derive(Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

fn ok() -> Json<OkResponse> {
    Json(OkResponse { ok: true })
}

// --- write-side bodies ----------------------------------------------------

#[derive(Deserialize)]
pub struct GoBody {
    pub path: String,
}

#[derive(Deserialize)]
pub struct ModeBody {
    pub mode: String,
}

#[derive(Deserialize)]
pub struct SplitBody {
    pub direction: String,
    #[serde(default)]
    pub pane_id: Option<String>,
}

#[derive(Deserialize)]
pub struct FocusBody {
    #[serde(default)]
    pub pane_id: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
}

#[derive(Deserialize)]
pub struct CloseBody {
    #[serde(default)]
    pub pane_id: Option<String>,
}

/// Window resize. Either supply `width` + `height` (logical pixels) for an
/// explicit size, or `preset` for a window-manager state change.
#[derive(Deserialize)]
pub struct ResizeBody {
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// One of: maximize, unmaximize, fullscreen, unfullscreen, minimize.
    #[serde(default)]
    pub preset: Option<String>,
}

// --- handlers -------------------------------------------------------------

pub async fn post_go(
    Extension(app): Extension<AppHandle>,
    Extension(state): Extension<Arc<IykeState>>,
    JsonBody(body): JsonBody<GoBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if !body.path.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("path must start with '/', got {:?}", body.path),
        ));
    }
    // Update the Rust mirror eagerly so a follow-up GET /iyke/state sees
    // the new route even if the FE listener hasn't run yet.
    state
        .set_shell(None, Some(body.path.clone()), None, None)
        .await;
    emit(&app, "iyke:go", serde_json::json!({ "path": body.path }))?;
    Ok(ok())
}

pub async fn post_mode(
    Extension(app): Extension<AppHandle>,
    Extension(state): Extension<Arc<IykeState>>,
    JsonBody(body): JsonBody<ModeBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if !is_valid_mode(&body.mode) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("invalid mode: {:?}", body.mode),
        ));
    }
    state
        .set_shell(Some(body.mode.clone()), None, None, None)
        .await;
    emit(&app, "iyke:mode", serde_json::json!({ "mode": body.mode }))?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct SidebarBody {
    /// `"toggle"` (default) | `"open"` | `"close"`.
    #[serde(default)]
    pub action: Option<String>,
}

/// `/iyke/sidebar` — collapse/expand the sidebar, the same state the ⌘B
/// shortcut and an activity-bar re-click drive.
///
/// Unlike `/iyke/mode`, the Rust mirror is NOT updated eagerly here: for
/// `toggle` the resulting value depends on the FE's current state, which
/// Rust doesn't own. Writing a guess would make `/iyke/state` briefly
/// disagree with the UI, and a caller polling right after a toggle would
/// read the wrong value. The FE pushes the true value back through
/// `iyke_set_shell` as soon as the store updates.
pub async fn post_sidebar(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<SidebarBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let action = body.action.as_deref().unwrap_or("toggle");
    if !matches!(action, "toggle" | "open" | "close") {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("invalid action: {action:?} (expected toggle | open | close)"),
        ));
    }
    emit(
        &app,
        "iyke:sidebar",
        serde_json::json!({ "action": action }),
    )?;
    Ok(ok())
}

/// `/iyke/open` accepts a free-form body to keep the wire surface small.
/// The FE listener does the actual validation against its `PaneView`
/// union — that way new view kinds can land FE-only without touching
/// the Rust side. We only sanity-check that `kind` is one we recognize.
pub async fn post_open(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<Value>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "missing field: kind".into()))?;
    // `artifact-grid` is accepted as a wire-protocol alias for the unified
    // `artifact-studio` at grid density. New iyke skills emit
    // `artifact-studio` with an explicit `density` field; the FE listener
    // collapses both to the unified shape.
    if !matches!(
        kind,
        "route"
            | "terminal"
            | "chat"
            | "artifact"
            | "artifact-grid"
            | "artifact-studio"
            | "mini-app"
    ) {
        return Err((StatusCode::BAD_REQUEST, format!("invalid kind: {kind:?}")));
    }
    emit(&app, "iyke:open", body)?;
    Ok(ok())
}

pub async fn post_split(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<SplitBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if !matches!(body.direction.as_str(), "horizontal" | "vertical") {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "direction must be 'horizontal' or 'vertical', got {:?}",
                body.direction
            ),
        ));
    }
    emit(
        &app,
        "iyke:split",
        serde_json::json!({
            "direction": body.direction,
            "pane_id": body.pane_id,
        }),
    )?;
    Ok(ok())
}

pub async fn post_focus(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<FocusBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if body.pane_id.is_none() && body.index.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "must provide one of: pane_id, index".into(),
        ));
    }
    emit(
        &app,
        "iyke:focus",
        serde_json::json!({
            "pane_id": body.pane_id,
            "index": body.index,
        }),
    )?;
    Ok(ok())
}

pub async fn post_close(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<CloseBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    emit(
        &app,
        "iyke:close",
        serde_json::json!({ "pane_id": body.pane_id }),
    )?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct RefreshBody {
    #[serde(default)]
    pub pane_id: Option<String>,
}

pub async fn post_refresh(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<RefreshBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    emit(
        &app,
        "iyke:refresh",
        serde_json::json!({ "pane_id": body.pane_id }),
    )?;
    Ok(ok())
}

/// Resize / maximize / fullscreen the main window. Unlike the pane verbs
/// this doesn't round-trip through the FE — Tauri exposes the webview
/// window directly so we drive it from the Rust side.
pub async fn post_resize(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<ResizeBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    // iyke bridge is app-level (single identity) — drives the PRIMARY window
    // today. TODO(multi-window): route to a target window once the bridge is
    // window-aware (research 03).
    let window = app.get_webview_window("main").ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "main window not found".into(),
    ))?;

    if let Some(preset) = body.preset.as_deref() {
        let map = |e: tauri::Error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("window op failed: {e}"),
            )
        };
        match preset {
            "maximize" => window.maximize().map_err(map)?,
            "unmaximize" => window.unmaximize().map_err(map)?,
            "fullscreen" => window.set_fullscreen(true).map_err(map)?,
            "unfullscreen" => window.set_fullscreen(false).map_err(map)?,
            "minimize" => window.minimize().map_err(map)?,
            other => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unknown preset: {other:?}"),
                ));
            }
        }
        return Ok(ok());
    }

    let (w, h) = match (body.width, body.height) {
        (Some(w), Some(h)) => (w, h),
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "must provide preset or width+height".into(),
            ));
        }
    };
    if !(200..=10_000).contains(&w) || !(200..=10_000).contains(&h) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("width/height out of range: {w}x{h} (allowed 200..=10000)"),
        ));
    }
    window
        .set_size(LogicalSize::new(w as f64, h as f64))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("set_size failed: {e}"),
            )
        })?;
    Ok(ok())
}

// --- screenshot ------------------------------------------------------------

#[derive(Deserialize)]
pub struct ScreenshotBody {
    #[serde(default)]
    pub out_path: Option<String>,
    /// Required when hitting `/iyke/screenshot/pane`; ignored on the window
    /// route.
    #[serde(default)]
    pub pane_id: Option<String>,
}

/// RPC-shaped: unlike the pane-verb handlers above, screenshot routes await
/// the full FE round-trip and return the saved file path. Callers (CLI, MCP)
/// need the path back, fire-and-forget doesn't help them.
pub async fn post_screenshot_window(
    Extension(app): Extension<AppHandle>,
    Extension(pending): Extension<crate::commands::ScreenshotPending>,
    JsonBody(body): JsonBody<ScreenshotBody>,
) -> Result<Json<crate::commands::ScreenshotResult>, (StatusCode, String)> {
    let result = crate::commands::screenshot::capture(
        &app,
        &pending,
        crate::commands::screenshot::ScreenshotKind::Window,
        None,
        body.out_path,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

pub async fn post_screenshot_pane(
    Extension(app): Extension<AppHandle>,
    Extension(pending): Extension<crate::commands::ScreenshotPending>,
    JsonBody(body): JsonBody<ScreenshotBody>,
) -> Result<Json<crate::commands::ScreenshotResult>, (StatusCode, String)> {
    let pane_id = body
        .pane_id
        .as_deref()
        .ok_or((StatusCode::BAD_REQUEST, "missing field: pane_id".into()))?;
    let result = crate::commands::screenshot::capture(
        &app,
        &pending,
        crate::commands::screenshot::ScreenshotKind::Pane,
        Some(pane_id),
        body.out_path,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

// --- DOM / accessibility-tree snapshot ------------------------------------

#[derive(Deserialize)]
pub struct DomQuery {
    /// Substring filter against the role/name/value of each entry.
    /// Case-insensitive. None returns the full tree.
    #[serde(default)]
    pub query: Option<String>,
    /// `false` (default) drops aria-hidden + display:none + visibility:hidden
    /// + 0-size nodes. `true` keeps them.
    #[serde(default)]
    pub all: bool,
    /// Pane id. Phase A only honors "shell" / unset (the main webview);
    /// Phase B routes other ids to iframe sidecar bridges.
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn get_dom(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    Query(q): Query<DomQuery>,
) -> Result<Json<DomResult>, (StatusCode, String)> {
    let pane = q.pane.clone();
    let query = q.query.clone();
    let all = q.all;
    let result = rpc::request(
        &app,
        &rpc.dom,
        "iyke://dom-request",
        DOM_TIMEOUT,
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "pane": pane,
                "query": query,
                "all": all,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

// --- query-cache dump -----------------------------------------------------

#[derive(Deserialize)]
pub struct QueryCacheQuery {
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn get_query_cache(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    Query(q): Query<QueryCacheQuery>,
) -> Result<Json<QueryCacheResult>, (StatusCode, String)> {
    let pane = q.pane.clone();
    let result = rpc::request(
        &app,
        &rpc.query_cache,
        "iyke://query-cache-request",
        QUERY_CACHE_TIMEOUT,
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "pane": pane,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

// --- terminal-read --------------------------------------------------------

const TERMINAL_READ_TIMEOUT: Duration = Duration::from_secs(5);

fn terminal_target_from_panes(panes: Option<&Value>, pane: Option<&str>) -> Option<String> {
    let leaves = panes?.get("leaves")?.as_array()?;
    let leaf = match pane {
        Some(id) if id != "shell" => leaves
            .iter()
            .find(|leaf| leaf.get("id").and_then(Value::as_str) == Some(id))?,
        _ => leaves
            .iter()
            .find(|leaf| leaf.get("focused").and_then(Value::as_bool) == Some(true))?,
    };
    let active = leaf.get("activeTabIdx")?.as_u64()? as usize;
    let tab = leaf.get("tabs")?.as_array()?.get(active)?;
    if tab.get("kind")?.as_str()? != "terminal" {
        return None;
    }
    tab.get("ptyId")
        .and_then(Value::as_str)
        .or_else(|| tab.get("terminalId").and_then(Value::as_str))
        .map(str::to_string)
}

#[derive(Deserialize)]
pub struct TerminalReadQuery {
    /// Pane id. Defaults to the focused pane on the FE side.
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub after: Option<u64>,
    #[serde(default)]
    pub mode: Option<String>,
    /// Tail size in bytes. None → return the whole buffer (cap is per-session,
    /// 256 KiB by default). 0 is treated as "all".
    #[serde(default)]
    pub bytes: Option<usize>,
    /// `false` (default) strips ANSI/VT escapes. `true` returns raw bytes
    /// as a UTF-8 string.
    #[serde(default)]
    pub raw: bool,
}

pub async fn get_terminal_read(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    Extension(state): Extension<Arc<IykeState>>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
    Query(q): Query<TerminalReadQuery>,
) -> Result<Json<TerminalReadResult>, (StatusCode, String)> {
    let direct_targets = [
        q.terminal.as_deref(),
        q.label.as_deref(),
        q.session.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if direct_targets.len() > 1 || (!direct_targets.is_empty() && q.pane.is_some()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "set only one of: pane, terminal, label, session".into(),
        ));
    }
    let pane_target = if direct_targets.is_empty() {
        terminal_target_from_panes(state.snapshot().await.panes.as_ref(), q.pane.as_deref())
    } else {
        None
    };
    if let Some(target) = direct_targets.first().copied().or(pane_target.as_deref()) {
        let pty_id = pty_manager
            .resolve_id(target)
            .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;
        let descriptor = pty_manager
            .list_terminals()
            .into_iter()
            .find(|terminal| terminal.pty_id == pty_id)
            .ok_or((StatusCode::NOT_FOUND, "terminal not found".to_string()))?;
        if q.mode.as_deref() == Some("screen") {
            let text = pty_manager
                .screen_text(&pty_id)
                .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;
            return Ok(Json(TerminalReadResult {
                bytes_available: text.len(),
                bytes_returned: text.len(),
                text,
                session_id: Some(descriptor.terminal_id.clone()),
                terminal_id: Some(descriptor.terminal_id),
                pty_id: Some(pty_id),
                start_offset: descriptor.output_start_offset,
                end_offset: descriptor.output_end_offset,
                available_start_offset: descriptor.output_start_offset,
                truncated: false,
                exited: descriptor.status == "exited",
                exit_code: descriptor.exit_code,
                error: None,
            }));
        }
        let (snapshot, exited, exit_code) = pty_manager
            .read_output(&pty_id, q.after, q.bytes)
            .map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;
        let bytes_returned = snapshot.data.len();
        let decoded = if q.raw {
            String::from_utf8_lossy(&snapshot.data).into_owned()
        } else {
            String::from_utf8_lossy(&strip_ansi_escapes::strip(&snapshot.data)).into_owned()
        };
        return Ok(Json(TerminalReadResult {
            text: decoded,
            bytes_available: descriptor
                .output_end_offset
                .saturating_sub(descriptor.output_start_offset)
                as usize,
            bytes_returned,
            session_id: Some(descriptor.terminal_id.clone()),
            terminal_id: Some(descriptor.terminal_id),
            pty_id: Some(pty_id),
            start_offset: snapshot.start_offset,
            end_offset: snapshot.end_offset,
            available_start_offset: snapshot.available_start_offset,
            truncated: snapshot.truncated,
            exited,
            exit_code,
            error: None,
        }));
    }
    let pane = q.pane.clone();
    let bytes = q.bytes;
    let raw = q.raw;
    let result = rpc::request(
        &app,
        &rpc.terminal_read,
        "iyke://terminal-read-request",
        TERMINAL_READ_TIMEOUT,
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "pane": pane,
                "bytes": bytes,
                "raw": raw,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

// --- wait -----------------------------------------------------------------

#[derive(Deserialize)]
pub struct WaitBody {
    /// One of: "text", "selector", "ref", "gone-text", "gone-selector".
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn post_wait(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    JsonBody(body): JsonBody<WaitBody>,
) -> Result<Json<WaitResult>, (StatusCode, String)> {
    if !matches!(
        body.kind.as_str(),
        "text" | "selector" | "ref" | "gone-text" | "gone-selector"
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown wait kind: {:?}", body.kind),
        ));
    }
    let timeout_ms = body
        .timeout_ms
        .unwrap_or(WAIT_TIMEOUT_DEFAULT_MS)
        .min(WAIT_TIMEOUT_MAX_MS);
    let kind = body.kind.clone();
    let value = body.value.clone();
    let pane = body.pane.clone();

    // Bridge polls until satisfied / timed out, then resolves. Use the
    // wall timeout + 1s slack so the rpc::request timeout doesn't
    // pre-empt a legitimate timeout response.
    let rpc_timeout = Duration::from_millis(timeout_ms + 1000);
    let result = rpc::request(
        &app,
        &rpc.wait,
        "iyke://wait-request",
        rpc_timeout,
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "kind": kind,
                "value": value,
                "timeout_ms": timeout_ms,
                "pane": pane,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(result))
}

// --- click / type / key (fire-and-forget) ---------------------------------

#[derive(Deserialize)]
pub struct ClickBody {
    /// One of: ref, selector, text. Exactly one of (ref|selector|text)
    /// must be set.
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn post_click(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    JsonBody(body): JsonBody<ClickBody>,
) -> Result<Json<ActionResponse>, (StatusCode, String)> {
    require_one_target(&body.r#ref, &body.selector, &body.text)?;
    let r#ref = body.r#ref.clone();
    let selector = body.selector.clone();
    let text = body.text.clone();
    let pane = body.pane.clone();
    // Round-trip so the response reflects whether the FE actually found the
    // target — previously this was fire-and-forget and returned `ok:true`
    // even when nothing matched (a silent no-op). Same mechanism as `get_dom`.
    let result = rpc::request(
        &app,
        &rpc.action,
        "iyke://click",
        CLICK_TIMEOUT,
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "ref": r#ref,
                "selector": selector,
                "text": text,
                "pane": pane,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(if result.matched {
        ActionResponse {
            ok: true,
            error: None,
        }
    } else {
        ActionResponse {
            ok: false,
            error: Some("target not found".into()),
        }
    }))
}

#[derive(Deserialize)]
pub struct TypeBody {
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    pub text: String,
    /// If true, replace the current value. Default appends.
    #[serde(default)]
    pub replace: bool,
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn post_type(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<TypeBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    require_one_target(&body.r#ref, &body.selector, &None)?;
    emit(
        &app,
        "iyke://type",
        serde_json::json!({
            "ref": body.r#ref,
            "selector": body.selector,
            "text": body.text,
            "replace": body.replace,
            "pane": body.pane,
        }),
    )?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct KeyBody {
    /// Comma- or plus-separated combo, e.g. "Enter", "Ctrl+S", "Meta+K".
    pub combo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub pane: Option<String>,
}

pub async fn post_key(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<KeyBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if body.combo.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "combo must not be empty".into()));
    }
    emit(
        &app,
        "iyke://key",
        serde_json::json!({
            "combo": body.combo,
            "ref": body.r#ref,
            "selector": body.selector,
            "pane": body.pane,
        }),
    )?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct TerminalSendBody {
    /// Optional leaf id. Defaults to the currently focused pane on the FE
    /// side. The active tab on the resolved pane must be `kind: 'terminal'`.
    #[serde(default)]
    pub pane: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default)]
    pub terminal: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub expected_pty_id: Option<String>,
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub lease_token: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    /// Raw text to write to the PTY. Written before any keys.
    #[serde(default)]
    pub data: Option<String>,
    /// Key combos to write after `data`. Each combo is translated to its
    /// terminal escape sequence (Enter → `\r`, Ctrl+C → `\x03`, ArrowUp →
    /// `\x1b[A`, etc.). Multiple may be specified for chord sequences.
    #[serde(default)]
    pub keys: Vec<String>,
}

pub async fn post_terminal_send(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    Extension(state): Extension<Arc<IykeState>>,
    Extension(pty_manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalSendBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.data.is_none() && body.keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "must set at least one of: data, keys".into(),
        ));
    }
    let direct_targets = [
        body.terminal.as_deref(),
        body.label.as_deref(),
        body.session.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if direct_targets.len() > 1 || (!direct_targets.is_empty() && body.pane.is_some()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "set only one of: pane, terminal, label, session".into(),
        ));
    }
    let mut data = body.data.as_deref().unwrap_or_default().as_bytes().to_vec();
    let mut unknown_keys = Vec::new();
    for key in &body.keys {
        if let Some(bytes) = terminal_key_bytes(key) {
            data.extend(bytes);
        } else {
            unknown_keys.push(key.clone());
        }
    }
    if !unknown_keys.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("unknown terminal key combo(s): {}", unknown_keys.join(", ")),
        ));
    }
    if data.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "terminal send produced no bytes".into(),
        ));
    }
    let pane_target = if direct_targets.is_empty() {
        terminal_target_from_panes(state.snapshot().await.panes.as_ref(), body.pane.as_deref())
    } else {
        None
    };
    if let Some(target) = direct_targets.first().copied().or(pane_target.as_deref()) {
        let descriptor = pty_manager
            .controlled_write(
                target,
                &data,
                body.expected_pty_id.as_deref(),
                body.actor.as_deref(),
                body.lease_token.as_deref(),
                body.dry_run,
            )
            .map_err(|error| (StatusCode::CONFLICT, error.to_string()))?;
        return Ok(Json(
            serde_json::to_value(descriptor).unwrap_or(Value::Null),
        ));
    }
    if body.dry_run {
        return Ok(Json(
            serde_json::json!({ "ok": true, "dry_run": true, "bytes": data.len() }),
        ));
    }
    let pane = body.pane.clone();
    let result = rpc::request(
        &app,
        &rpc.action,
        "iyke://terminal-send",
        Duration::from_secs(5),
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "pane": pane,
                "data": String::from_utf8_lossy(&data),
                "keys": [],
            })
        },
    )
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")))?;
    Ok(Json(serde_json::json!({
        "ok": result.matched,
        "error": if result.matched { Value::Null } else { Value::String("pane has no writable terminal".into()) }
    })))
}

fn terminal_key_bytes(combo: &str) -> Option<Vec<u8>> {
    let mut key = String::new();
    let mut ctrl = false;
    let mut alt = false;
    let mut meta = false;
    for part in combo
        .split(['+', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" => alt = true,
            "shift" => {}
            "meta" | "cmd" | "command" | "super" => meta = true,
            "enter" => key = "Enter".to_string(),
            "esc" | "escape" => key = "Escape".to_string(),
            "tab" => key = "Tab".to_string(),
            "space" => key = " ".to_string(),
            "up" => key = "ArrowUp".to_string(),
            "down" => key = "ArrowDown".to_string(),
            "left" => key = "ArrowLeft".to_string(),
            "right" => key = "ArrowRight".to_string(),
            "backspace" => key = "Backspace".to_string(),
            "delete" => key = "Delete".to_string(),
            "home" => key = "Home".to_string(),
            "end" => key = "End".to_string(),
            _ => key = part.to_string(),
        }
    }
    if ctrl && !alt && !meta && key.len() == 1 {
        let code = key.as_bytes()[0].to_ascii_uppercase();
        if (64..=95).contains(&code) {
            return Some(vec![code - 64]);
        }
    }
    let alt_prefix = if alt {
        b"\x1b".as_slice()
    } else {
        b"".as_slice()
    };
    let sequence: &[u8] = match key.as_str() {
        "Enter" => b"\r",
        "Tab" => b"\t",
        "Escape" => b"\x1b",
        "Backspace" => b"\x7f",
        " " => b" ",
        "ArrowUp" => b"\x1b[A",
        "ArrowDown" => b"\x1b[B",
        "ArrowRight" => b"\x1b[C",
        "ArrowLeft" => b"\x1b[D",
        "Home" => b"\x1b[H",
        "End" => b"\x1b[F",
        "Delete" => b"\x1b[3~",
        "PageUp" => b"\x1b[5~",
        "PageDown" => b"\x1b[6~",
        _ => {
            if let Some(number) = key.strip_prefix('F').or_else(|| key.strip_prefix('f')) {
                match number.parse::<u8>().ok()? {
                    1 => b"\x1bOP",
                    2 => b"\x1bOQ",
                    3 => b"\x1bOR",
                    4 => b"\x1bOS",
                    5 => b"\x1b[15~",
                    6 => b"\x1b[17~",
                    7 => b"\x1b[18~",
                    8 => b"\x1b[19~",
                    9 => b"\x1b[20~",
                    10 => b"\x1b[21~",
                    11 => b"\x1b[23~",
                    12 => b"\x1b[24~",
                    _ => return None,
                }
            } else if key.chars().count() == 1 && !ctrl && !meta {
                let mut bytes = alt_prefix.to_vec();
                bytes.extend(key.as_bytes());
                return Some(bytes);
            } else {
                return None;
            }
        }
    };
    let mut bytes = if alt && !sequence.starts_with(b"\x1b") {
        alt_prefix.to_vec()
    } else {
        Vec::new()
    };
    bytes.extend(sequence);
    Some(bytes)
}

fn require_one_target(
    r#ref: &Option<String>,
    selector: &Option<String>,
    text: &Option<String>,
) -> Result<(), (StatusCode, String)> {
    let count = r#ref.is_some() as u8 + selector.is_some() as u8 + text.is_some() as u8;
    if count != 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            "must set exactly one of: ref, selector, text".into(),
        ));
    }
    Ok(())
}

// --- logs / network reads -------------------------------------------------

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default)]
    pub level: Option<String>,
    // u64, not u128: axum's Query uses serde_urlencoded which does NOT support
    // u128 (every `?since=…` was rejected with "u128 is not supported").
    // unix-ms fits u64; `recent_logs` casts to u128 at the comparison site.
    #[serde(default)]
    pub since: Option<u64>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Serialize)]
pub struct LogsResponse {
    pub entries: Vec<LogEntry>,
}

pub async fn get_logs(
    Extension(state): Extension<Arc<IykeState>>,
    Query(q): Query<LogsQuery>,
) -> Json<LogsResponse> {
    let entries = state
        .recent_logs(q.level.as_deref(), q.since, q.source.as_deref())
        .await;
    Json(LogsResponse { entries })
}

#[derive(Deserialize)]
pub struct NetworkQuery {
    #[serde(default)]
    pub since: Option<u128>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Serialize)]
pub struct NetworkResponse {
    pub entries: Vec<NetworkEntry>,
}

pub async fn get_network(
    Extension(state): Extension<Arc<IykeState>>,
    Query(q): Query<NetworkQuery>,
) -> Json<NetworkResponse> {
    let entries = state.recent_network(q.since, q.source.as_deref()).await;
    Json(NetworkResponse { entries })
}

// --- iframe state + message (Phase C) -------------------------------------

#[derive(Deserialize)]
pub struct IframeStateQuery {
    pub pane: String,
}

/// Read the latest published state object for an iframe pane. The FE
/// piggy-backs the answer on the same Pending<DomResult> channel: the
/// state object goes in `json`, generation tracks the registry version.
pub async fn get_iframe_state(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<IykeRpc>,
    Query(q): Query<IframeStateQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let pane = q.pane.clone();
    let result = rpc::request(
        &app,
        &rpc.dom,
        "iyke://iframe-state-request",
        Duration::from_secs(2),
        |request_id| {
            serde_json::json!({
                "request_id": request_id,
                "pane": pane,
            })
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(serde_json::json!({
        "pane": q.pane,
        "state": result.json,
        "generation": result.generation,
    })))
}

#[derive(Deserialize)]
pub struct IframeMessageBody {
    pub pane: String,
    pub kind: String,
    #[serde(default)]
    pub payload: Option<Value>,
}

pub async fn post_iframe_message(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<IframeMessageBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if body.kind.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "kind must not be empty".into()));
    }
    emit(
        &app,
        "iyke://iframe-message",
        serde_json::json!({
            "pane": body.pane,
            "kind": body.kind,
            "payload": body.payload,
        }),
    )?;
    Ok(ok())
}

// --- devtools -------------------------------------------------------------

#[derive(Deserialize)]
pub struct PkgInstallBody {
    pub install_path: String,
    /// Phase 2 (projects-first-class): scope picker.
    /// `"workspace"` / `"project:<id>"` / null (defaults to active project).
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Deserialize)]
pub struct PkgUninstallBody {
    pub pkg_id: String,
}

pub async fn post_pkg_uninstall(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgUninstallBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let pkg_id = body.pkg_id;
    tokio::task::spawn_blocking(move || kernel_arc.uninstall(&pkg_id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("join error: {e}"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(ok())
}

#[derive(serde::Deserialize)]
pub struct PkgListQuery {
    /// When true, return pkgs scoped to other projects too. Default false.
    #[serde(default)]
    pub include_other_projects: bool,
    /// "workspace" → only workspace-scoped, "project" → only project-scoped.
    pub kind: Option<String>,
}

/// Phase 2 (projects-first-class) bridge endpoint: list installed pkgs
/// with scope-aware filtering. Default returns workspace + active-project
/// pkgs; pass `include_other_projects=true` to include the rest.
pub async fn get_pkg_list(
    Extension(app): Extension<AppHandle>,
    Extension(db): Extension<Arc<crate::commands::db::PaDb>>,
    axum::extract::Query(q): axum::extract::Query<PkgListQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let active = {
        let pool = db
            .ensure_pool()
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        crate::commands::projects::get_active_project_id(&pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?
    };
    let mut entries: Vec<Value> = Vec::new();
    for s in kernel.0.list_installed() {
        let kind_match = match q.kind.as_deref() {
            Some("workspace") => s.project_id.is_none(),
            Some("project") => s.project_id.is_some(),
            _ => true,
        };
        if !kind_match {
            continue;
        }
        let visible = match &s.project_id {
            None => true,
            Some(p) => p == &active,
        };
        if !visible && !q.include_other_projects {
            continue;
        }
        let scope_wire = match &s.project_id {
            None => "workspace".to_string(),
            Some(p) => format!("project:{p}"),
        };
        entries.push(serde_json::json!({
            "id": s.id,
            "version": s.version,
            "ikenga_api": s.ikenga_api,
            "install_path": s.install_path,
            "enabled": s.enabled,
            "installed_at": s.installed_at,
            "compatible": s.compatible,
            "source": s.source,
            "scope": scope_wire,
            "active_now": visible,
        }));
    }
    Ok(Json(serde_json::json!({
        "active_project_id": active,
        "pkgs": entries,
    })))
}

#[derive(Deserialize)]
pub struct PkgScopeSetBody {
    pub pkg_id: String,
    /// "workspace" | "project:<id>" | null (defaults to active project).
    pub scope: Option<String>,
}

pub async fn post_pkg_scope_set(
    Extension(app): Extension<AppHandle>,
    Extension(db): Extension<Arc<crate::commands::db::PaDb>>,
    JsonBody(body): JsonBody<PkgScopeSetBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let project_id = crate::commands::pkg::resolve_install_scope_for_iyke(db.clone(), body.scope)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let pool = db
        .ensure_pool()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let active = crate::commands::projects::get_active_project_id(&pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let kernel_arc = kernel.0.clone();
    let pkg_id = body.pkg_id.clone();
    let active_for_task = active.clone();
    let project_for_task = project_id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        kernel_arc
            .set_scope(&pkg_id, project_for_task)
            .map_err(|e| format!("{e:#}"))?;
        kernel_arc
            .reconcile_for_project(&active_for_task)
            .map_err(|e| format!("{e:#}"))?;
        Ok(())
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("join error: {e}"),
        )
    })?
    .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct PkgBadgeSetBody {
    pub pkg_id: String,
    /// `None` (or the field omitted) clears the badge.
    #[serde(default)]
    pub badge: Option<crate::pkg::registries::ActivityBarBadge>,
}

/// WP-11 — set/clear a pkg's activity-bar status badge from outside the
/// shell (external `iyke` driving, or smoke-testing without a real iframe).
/// Mirrors the in-shell `host.pkg.setBadge` AppBridge verb; both call
/// `pkg_activity_bar_set_badge`.
pub async fn post_pkg_badge_set(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgBadgeSetBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    use tauri::Manager;
    let activity_bar = app
        .try_state::<crate::commands::ActivityBarState>()
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "activity-bar state not registered".into(),
        ))?;
    crate::commands::pkg_activity_bar_set_badge(app.clone(), activity_bar, body.pkg_id, body.badge)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(ok())
}

#[derive(Deserialize)]
pub struct ObaInstallLocalBody {
    pub kind: String,
    pub name: String,
    pub path: String,
}

/// Install a primitive into the Ọba store from a LOCAL path. Drive surface for
/// the `oba_install_local` Tauri command (WP-25) — the install itself is
/// copy-only staging with provenance `local` and `auto_update` off; this
/// handler only relays the call so iyke-driven sessions can populate the store
/// without an FE affordance.
pub async fn post_oba_install_local(
    JsonBody(body): JsonBody<ObaInstallLocalBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let entry = crate::commands::claude_store::oba_install_local(body.kind, body.name, body.path)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    serde_json::to_value(&entry)
        .map(|v| Json(serde_json::json!({ "installed": v })))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize store entry: {e}"),
            )
        })
}

pub async fn post_pkg_install(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgInstallBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    // The kernel's registries call `tauri::async_runtime::block_on` internally
    // (DB writes, content-server registration). Calling that from a Tokio
    // worker panics with "Cannot start a runtime from within a runtime", so
    // run the install on a blocking thread.
    let kernel_arc = kernel.0.clone();
    let path = std::path::PathBuf::from(&body.install_path);
    // iyke-driven installs are local sideloads — same provenance class as
    // the FE workspace install path.
    let source = crate::pkg::InstallSource::Local {
        path: body.install_path.clone(),
    };
    // Resolve scope. The bridge has the PaDb in Extension; reuse the same
    // helper as the FE Tauri command so the wire format stays single-sourced.
    let pa_db = app
        .try_state::<std::sync::Arc<crate::commands::db::PaDb>>()
        .ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "pa_db state not registered".into(),
        ))?
        .inner()
        .clone();
    let project_id = crate::commands::pkg::resolve_install_scope_for_iyke(pa_db, body.scope)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let installed = tokio::task::spawn_blocking(move || {
        crate::pkg::materialize_npm_deps(&path)?;
        kernel_arc.install_from_path(&path, source, project_id)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("join error: {e}"),
        )
    })?
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    serde_json::to_value(&installed)
        .map(|v| Json(serde_json::json!({ "installed": v })))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize installed summary: {e}"),
            )
        })
}

// --- pkg dev-mode bridge ---------------------------------------------------

#[derive(Deserialize)]
pub struct PkgDevRegisterBody {
    pub install_path: String,
}

#[derive(Deserialize)]
pub struct PkgDevPkgIdBody {
    pub pkg_id: String,
}

#[derive(Deserialize)]
pub struct PkgHealthRemoveBody {
    pub pkg_id: String,
}

/// Symlink (or, today: register a path-rooted install) under the dev
/// trust gate. Mirrors `pkg_dev_register` Tauri command for the
/// `ikenga dev <path>` CLI surface.
pub async fn post_pkg_dev_register(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgDevRegisterBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let path = std::path::PathBuf::from(&body.install_path);
    let install_path_str = body.install_path;

    let kernel_for_install = std::sync::Arc::clone(&kernel_arc);
    let source = crate::pkg::InstallSource::Dev {
        path: install_path_str,
    };
    let summary = tokio::task::spawn_blocking(move || {
        crate::pkg::materialize_npm_deps(&path)?;
        kernel_for_install.install_from_path(&path, source, None)
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("install join: {e}"),
        )
    })?
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let path_for_watcher = std::path::PathBuf::from(&summary.install_path);
    let extra_globs =
        crate::commands::pkg_dev::collect_restart_globs(&path_for_watcher).unwrap_or_default();
    kernel_arc
        .spawn_dev_watcher(&summary.id, &path_for_watcher, extra_globs)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("watcher: {e:#}")))?;

    serde_json::to_value(&summary)
        .map(|v| Json(serde_json::json!({ "installed": v })))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize summary: {e}"),
            )
        })
}

pub async fn post_pkg_dev_unregister(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgDevPkgIdBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let pkg_id = body.pkg_id;

    let is_dev = kernel_arc
        .installed_summary(&pkg_id)
        .map(|s| s.source.is_dev())
        .unwrap_or(false);
    if !is_dev {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("pkg `{pkg_id}` is not a dev install"),
        ));
    }
    kernel_arc.drop_dev_watcher(&pkg_id);
    let kernel_for_uninstall = std::sync::Arc::clone(&kernel_arc);
    let id_for_uninstall = pkg_id.clone();
    tokio::task::spawn_blocking(move || kernel_for_uninstall.uninstall(&id_for_uninstall))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("uninstall join: {e}"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(ok())
}

pub async fn post_pkg_dev_reload(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgDevPkgIdBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let pkg_id = body.pkg_id;
    let summary = tokio::task::spawn_blocking(move || kernel_arc.reload_pkg(&pkg_id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("reload join: {e}"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    serde_json::to_value(&summary)
        .map(|v| Json(serde_json::json!({ "installed": v })))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize summary: {e}"),
            )
        })
}

/// Scan for broken / orphaned install records (read-only). Mirrors the
/// `pkg_health_scan` Tauri command for the `ikenga doctor` CLI surface.
pub async fn post_pkg_health_scan(
    Extension(app): Extension<AppHandle>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let issues = tokio::task::spawn_blocking(move || kernel_arc.health_scan())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("scan join: {e}")))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    serde_json::to_value(&issues)
        .map(|v| Json(serde_json::json!({ "issues": v })))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("serialize issues: {e}"),
            )
        })
}

/// Remove one broken install record. Mirrors `pkg_health_remove`.
pub async fn post_pkg_health_remove(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<PkgHealthRemoveBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let pkg_id = body.pkg_id;
    tokio::task::spawn_blocking(move || kernel_arc.purge_install_record(&pkg_id))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("remove join: {e}"),
            )
        })?
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(ok())
}

/// Remove every currently-detected broken record + orphan row. Mirrors
/// `pkg_health_remove_all`; returns the removed counts.
pub async fn post_pkg_health_remove_all(
    Extension(app): Extension<AppHandle>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use tauri::Manager;
    let kernel = app.try_state::<crate::commands::KernelState>().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "pkg kernel state not registered".into(),
    ))?;
    let kernel_arc = kernel.0.clone();
    let (removed_records, removed_orphans) =
        tokio::task::spawn_blocking(move || kernel_arc.purge_all_broken())
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("remove-all join: {e}"),
                )
            })?
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(serde_json::json!({
        "removed_records": removed_records,
        "removed_orphans": removed_orphans,
    })))
}

pub async fn post_devtools(
    Extension(app): Extension<AppHandle>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    // DevTools on the PRIMARY window (debug builds). TODO(multi-window): accept
    // a target label once the iyke bridge is window-aware.
    let window = app.get_webview_window("main").ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "main window not found".into(),
    ))?;
    #[cfg(debug_assertions)]
    {
        window.open_devtools();
        Ok(ok())
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = window;
        Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "devtools only available in debug builds".into(),
        ))
    }
}

// --- chi ------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct ChiResumeBody {
    #[serde(rename = "runId")]
    run_id: String,
    prompt: String,
}

#[derive(serde::Deserialize)]
pub struct ChiCancelBody {
    #[serde(rename = "runId")]
    run_id: String,
}

#[derive(serde::Deserialize)]
pub struct ChiListQuery {
    #[serde(rename = "engineId")]
    engine_id: Option<String>,
    limit: Option<i64>,
}

#[derive(serde::Deserialize)]
pub struct ChiStatusQuery {
    #[serde(rename = "runId")]
    run_id: String,
}

pub async fn post_chi_run(
    Extension(app): Extension<AppHandle>,
    JsonBody(opts): JsonBody<ChiRunOpts>,
) -> Result<Json<crate::commands::chi::ChiRunResult>, (StatusCode, String)> {
    let app_for_call = app.clone();
    let db = app.state::<std::sync::Arc<PaDb>>();
    let cache = app.state::<ChiCache>();
    let runtime = app.state::<std::sync::Arc<ChiRuntime>>();
    chi_run(app_for_call, db, cache, runtime, opts)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub async fn post_chi_resume(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<ChiResumeBody>,
) -> Result<Json<crate::commands::chi::ChiRunResult>, (StatusCode, String)> {
    let app_for_call = app.clone();
    let db = app.state::<std::sync::Arc<PaDb>>();
    let cache = app.state::<ChiCache>();
    let runtime = app.state::<std::sync::Arc<ChiRuntime>>();
    chi_resume(app_for_call, db, cache, runtime, body.run_id, body.prompt)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub async fn get_chi_status(
    Extension(app): Extension<AppHandle>,
    Query(q): Query<ChiStatusQuery>,
) -> Result<Json<crate::commands::chi::ChiRunResult>, (StatusCode, String)> {
    let db = app.state::<std::sync::Arc<PaDb>>();
    let cache = app.state::<ChiCache>();
    chi_status(db, cache, q.run_id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub async fn get_chi_list(
    Extension(app): Extension<AppHandle>,
    Query(q): Query<ChiListQuery>,
) -> Result<Json<Vec<crate::commands::chi::ChiCacheRow>>, (StatusCode, String)> {
    let db = app.state::<std::sync::Arc<PaDb>>();
    chi_list(db, q.engine_id, q.limit)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub async fn post_chi_cancel(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<ChiCancelBody>,
) -> Result<Json<crate::commands::chi::ChiRunResult>, (StatusCode, String)> {
    let db = app.state::<std::sync::Arc<PaDb>>();
    let runtime = app.state::<std::sync::Arc<ChiRuntime>>();
    chi_cancel(db, runtime, body.run_id)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

// --- helpers --------------------------------------------------------------

fn emit(app: &AppHandle, event: &str, payload: Value) -> Result<(), (StatusCode, String)> {
    app.emit(event, payload).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to emit {event}: {e}"),
        )
    })
}

/// Modes recognized by the in-app `useShellStore`. Kept in sync with
/// `src/lib/shell/shell-store.ts` (`ActivityMode`). Server-side check is
/// a sanity gate; the FE listener is the source of truth.
///
/// CORE modes mirror the `CoreMode` union. Dynamic `pkg:<id>` modes (one per
/// installed app pkg) are accepted by prefix — the FE reconciles a stale pkg
/// mode to 'app' if the pkg isn't installed, so the bridge needn't know the
/// live pkg set.
fn is_valid_mode(m: &str) -> bool {
    matches!(
        m,
        "app" | "files" | "sessions" | "artifact-grid" | "ngwa" | "pkgs" | "settings"
    ) || m.starts_with("pkg:")
}

#[cfg(test)]
mod tests {
    use super::terminal_key_bytes;

    #[test]
    fn terminal_key_translation_matches_frontend_sequences() {
        assert_eq!(terminal_key_bytes("Enter").unwrap(), b"\r");
        assert_eq!(terminal_key_bytes("Ctrl+C").unwrap(), b"\x03");
        assert_eq!(terminal_key_bytes("Up").unwrap(), b"\x1b[A");
        assert_eq!(terminal_key_bytes("F12").unwrap(), b"\x1b[24~");
        assert_eq!(terminal_key_bytes("Alt+x").unwrap(), b"\x1bx");
        assert_eq!(terminal_key_bytes("Meta+Enter").unwrap(), b"\r");
    }
}
