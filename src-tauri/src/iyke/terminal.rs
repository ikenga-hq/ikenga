use std::sync::Arc;
use std::time::Duration;

use axum::{extract::Json as JsonBody, http::StatusCode, Extension, Json};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::iyke::state::IykeState;
use crate::pty::{PtyManager, TerminalAuditEntry, TerminalDescriptor};
use crate::window::descriptor::{WindowDescriptor, WindowKind};
use crate::window::registry::WindowRegistry;

fn err(status: StatusCode, message: impl Into<String>) -> (StatusCode, String) {
    (status, message.into())
}

#[derive(Clone, Serialize)]
pub struct IykeWindowInfo {
    pub label: String,
    pub kind: WindowKind,
    pub surface_set: Vec<String>,
    pub project_id: Option<String>,
    pub layout_key: String,
    pub panes: Option<Value>,
}

impl IykeWindowInfo {
    pub fn from_descriptor(descriptor: WindowDescriptor, panes: Option<Value>) -> Self {
        Self {
            label: descriptor.label,
            kind: descriptor.kind,
            surface_set: descriptor.surface_set,
            project_id: descriptor.project_id,
            layout_key: descriptor.layout_key,
            panes,
        }
    }
}

#[derive(Deserialize)]
pub struct TerminalTargetBody {
    pub terminal: String,
}

#[derive(Deserialize)]
pub struct TerminalLabelBody {
    pub terminal: String,
    pub label: Option<String>,
}

#[derive(Deserialize)]
pub struct TerminalLeaseBody {
    pub terminal: String,
    pub agent_id: String,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
}

#[derive(Deserialize)]
pub struct TerminalLeaseReleaseBody {
    pub terminal: String,
    pub token: String,
}

#[derive(Deserialize)]
pub struct TerminalWaitBody {
    pub terminal: String,
    #[serde(default)]
    pub r#match: Option<String>,
    #[serde(default)]
    pub until_idle_ms: Option<u64>,
    #[serde(default)]
    pub after: Option<u64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub raw: bool,
}

#[derive(Serialize)]
pub struct TerminalWaitResponse {
    pub satisfied: bool,
    pub matched: bool,
    pub idle: bool,
    pub timed_out: bool,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub text: String,
    pub start_offset: u64,
    pub end_offset: u64,
    pub available_start_offset: u64,
    pub truncated: bool,
}

#[derive(Deserialize)]
pub struct TabActivateBody {
    pub pane: String,
    #[serde(default)]
    pub index: Option<usize>,
    #[serde(default)]
    pub terminal: Option<String>,
}

pub fn enrich_terminals(
    terminals: &mut [TerminalDescriptor],
    panes: Option<&Value>,
    windows: &[WindowDescriptor],
) {
    if let Some(leaves) = panes
        .and_then(|panes| panes.get("leaves"))
        .and_then(Value::as_array)
    {
        for leaf in leaves {
            let pane_id = leaf.get("id").and_then(Value::as_str).unwrap_or_default();
            let focused_leaf = leaf
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let active_index = leaf
                .get("activeTabIdx")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            if let Some(tabs) = leaf.get("tabs").and_then(Value::as_array) {
                for (index, tab) in tabs.iter().enumerate() {
                    let terminal_id = tab.get("terminalId").and_then(Value::as_str);
                    let pty_id = tab.get("ptyId").and_then(Value::as_str);
                    if let Some(terminal) = terminals.iter_mut().find(|terminal| {
                        pty_id == Some(terminal.pty_id.as_str())
                            || terminal_id == Some(terminal.terminal_id.as_str())
                    }) {
                        if !pane_id.is_empty() && !terminal.pane_ids.iter().any(|id| id == pane_id)
                        {
                            terminal.pane_ids.push(pane_id.to_string());
                        }
                        if !terminal.window_labels.iter().any(|label| label == "main") {
                            terminal.window_labels.push("main".to_string());
                        }
                        if index == active_index {
                            terminal.mounted = true;
                            terminal.focused |= focused_leaf;
                        }
                    }
                }
            }
        }
    }
    for window in windows {
        for surface in &window.surface_set {
            if let Some(pty_id) = surface.strip_prefix("terminal:") {
                if let Some(terminal) = terminals
                    .iter_mut()
                    .find(|terminal| terminal.pty_id == pty_id)
                {
                    terminal.mounted = true;
                    if !terminal
                        .window_labels
                        .iter()
                        .any(|label| label == &window.label)
                    {
                        terminal.window_labels.push(window.label.clone());
                    }
                }
            }
        }
    }
}

pub async fn get_terminals(
    Extension(manager): Extension<Arc<PtyManager>>,
    Extension(state): Extension<Arc<IykeState>>,
    Extension(app): Extension<AppHandle>,
) -> Json<Vec<TerminalDescriptor>> {
    let panes = state.snapshot().await.panes;
    let registry = app.state::<WindowRegistry>();
    let windows = registry.list_live(&app);
    let mut terminals = manager.list_terminals();
    enrich_terminals(&mut terminals, panes.as_ref(), &windows);
    Json(terminals)
}

pub async fn post_terminal_get(
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalTargetBody>,
) -> Result<Json<TerminalDescriptor>, (StatusCode, String)> {
    manager
        .list_terminals()
        .into_iter()
        .find(|terminal| {
            terminal.terminal_id == body.terminal
                || terminal.pty_id == body.terminal
                || terminal.label.as_deref() == Some(body.terminal.as_str())
        })
        .map(Json)
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "terminal not found"))
}

// ─── WP-08: terminal lifecycle over the bridge ───────────────────────────────
//
// Before this, an agent could drive terminals but not create them: the only
// way to get a PTY was for a human to click "New tab". That made the whole
// multi-agent story unreachable from outside the app.
//
// Spawn round-trips through the FRONTEND rather than calling `PtyManager`
// directly (decision D-1 in the Phase 4 plan). The frontend owns the terminal
// session store, and a Rust-local PTY would exist in the registry while being
// invisible in the pane tree — unwatchable and unreclaimable. Going through
// the frontend means an agent's terminal is an ordinary tab: you can see it,
// pop it out, and take it over.

/// How long to wait for the frontend to mint a terminal id.
const SPAWN_RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the PTY itself to appear in the registry after the
/// frontend replies. The frontend spawns on mount, so this covers the React
/// commit plus `Pty.spawn`'s IPC round-trip.
const SPAWN_PTY_TIMEOUT: Duration = Duration::from_secs(10);
const SPAWN_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalSpawnResult {
    /// Terminal id the frontend minted, or None when it refused (e.g. the
    /// requested pane doesn't exist).
    #[serde(default)]
    pub terminal_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct TerminalSpawnBody {
    /// Working directory. Defaults to the frontend's active-project cwd.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Command + args. Defaults to the platform login shell.
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub title: Option<String>,
    /// Apply this unique label once the PTY exists — saves a second call on
    /// the orchestration path, where every terminal wants a role name.
    #[serde(default)]
    pub label: Option<String>,
    /// Target pane leaf id. Defaults to the focused pane.
    #[serde(default)]
    pub pane: Option<String>,
    /// Acquire a lease for this agent id and return the token, so a spawning
    /// orchestrator owns the terminal from birth and no one else can write to
    /// it in the window between spawn and an explicit lease call.
    #[serde(default)]
    pub lease_for: Option<String>,
    #[serde(default)]
    pub lease_ttl_ms: Option<u64>,
}

pub async fn post_terminal_spawn(
    Extension(app): Extension<AppHandle>,
    Extension(rpc): Extension<crate::iyke::IykeRpc>,
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalSpawnBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if let Some(argv) = &body.argv {
        if argv.is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "argv must not be empty"));
        }
    }
    // Reject a duplicate label up front and reserve it atomically so concurrent calls fail immediately.
    let mut reservation = if let Some(label) = &body.label {
        if label.trim().is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "label must not be empty"));
        }
        Some(
            manager
                .reserve_label(label)
                .map_err(|e| err(StatusCode::CONFLICT, e.to_string()))?,
        )
    } else {
        None
    };

    let cwd = body.cwd.clone();
    let argv = body.argv.clone();
    let title = body.title.clone();
    let pane = body.pane.clone();
    let result = crate::iyke::rpc::request(
        &app,
        &rpc.terminal_spawn,
        "iyke://terminal-spawn",
        SPAWN_RPC_TIMEOUT,
        |request_id| {
            json!({
                "request_id": request_id,
                "cwd": cwd,
                "argv": argv,
                "title": title,
                "pane": pane,
            })
        },
    )
    .await
    .map_err(|error| err(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;

    if let Some(error) = result.error {
        return Err(err(StatusCode::BAD_REQUEST, error));
    }
    let terminal_id = result.terminal_id.ok_or_else(|| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "frontend returned no terminal id",
        )
    })?;

    // The frontend replies as soon as it has minted the id; the PTY spawns when
    // the tab mounts. Poll until the kernel actually has it, so callers never
    // receive an id they can't immediately write to.
    //
    // Require a RUNNING descriptor, not merely a matching one. A single
    // `terminal_id` can legitimately own several PTY records at once: React
    // StrictMode double-mounts in dev (spawn → dispose → respawn, leaving an
    // exited record behind), and in production the same shape appears whenever
    // a shell exits and is restarted from the pane. Exited records linger for
    // ten minutes by design. Taking the first match would hand the caller a
    // dead `pty_id` and lease a corpse — the write would then fail against a
    // terminal the agent believes it owns.
    let deadline = tokio::time::Instant::now() + SPAWN_PTY_TIMEOUT;
    let descriptor = loop {
        let mut matches = manager
            .list_terminals()
            .into_iter()
            .filter(|t| t.terminal_id == terminal_id)
            .collect::<Vec<_>>();
        // Newest first, so a respawn wins over the record it replaced.
        matches.sort_by_key(|t| std::cmp::Reverse(t.created_at));
        if let Some(d) = matches.into_iter().find(|t| t.status == "running") {
            break d;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(err(
                StatusCode::GATEWAY_TIMEOUT,
                format!("terminal {terminal_id} never reached the pty registry in a running state"),
            ));
        }
        tokio::time::sleep(SPAWN_POLL_INTERVAL).await;
    };

    // Address the label and lease by the concrete `pty_id` we just resolved,
    // not the logical `terminal_id`: with several records sharing that id,
    // `resolve_id` could land on the exited one.
    let target = descriptor.pty_id.clone();

    let mut out = json!({
        "terminal_id": descriptor.terminal_id,
        "pty_id": descriptor.pty_id,
        "cwd": descriptor.cwd,
        "argv": descriptor.argv,
        "status": descriptor.status,
    });

    if let Some(label) = body.label {
        match manager.set_label(&target, Some(label)) {
            Ok(d) => {
                if let Some(r) = reservation.as_mut() {
                    r.commit();
                }
                out["label"] = json!(d.label);
            }
            Err(error) => out["label_error"] = json!(error.to_string()),
        }
    }
    if let Some(agent_id) = body.lease_for {
        if agent_id.trim().is_empty() {
            return Err(err(StatusCode::BAD_REQUEST, "lease_for must not be empty"));
        }
        match manager.acquire_lease(&target, agent_id, body.lease_ttl_ms.unwrap_or(60_000)) {
            Ok((token, expires_at)) => {
                out["lease_token"] = json!(token);
                out["lease_expires_at"] = json!(expires_at);
            }
            Err(error) => out["lease_error"] = json!(error.to_string()),
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct TerminalKillBody {
    /// Terminal id, pty id, or label.
    pub terminal: String,
    /// Also remove the tab from the pane tree. Off by default: killing the
    /// process leaves the tab in place showing its exit status, which is the
    /// same thing that happens when a shell exits on its own, and keeps the
    /// scrollback readable for a post-mortem.
    #[serde(default)]
    pub close_tab: bool,
}

pub async fn post_terminal_kill(
    Extension(app): Extension<AppHandle>,
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalKillBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let resolved = manager
        .resolve_id(&body.terminal)
        .map_err(|error| err(StatusCode::NOT_FOUND, error.to_string()))?;
    let terminal_id = manager
        .list_terminals()
        .into_iter()
        .find(|t| t.pty_id == resolved)
        .map(|t| t.terminal_id);
    manager
        .kill(&resolved)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    if body.close_tab {
        if let Some(id) = &terminal_id {
            // Fire-and-forget, like `tab-activate`: the process is already
            // dead, so a frontend that misses this leaves a harmless exited tab.
            let _ = app.emit("iyke://terminal-close-tab", json!({ "terminal_id": id }));
        }
    }
    Ok(Json(
        json!({ "ok": true, "pty_id": resolved, "terminal_id": terminal_id }),
    ))
}

pub async fn post_terminal_label(
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalLabelBody>,
) -> Result<Json<TerminalDescriptor>, (StatusCode, String)> {
    manager
        .set_label(&body.terminal, body.label)
        .map(Json)
        .map_err(|error| err(StatusCode::BAD_REQUEST, error.to_string()))
}

pub async fn post_terminal_lease_acquire(
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalLeaseBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.agent_id.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "agent_id must not be empty"));
    }
    let (token, expires_at) = manager
        .acquire_lease(&body.terminal, body.agent_id, body.ttl_ms.unwrap_or(60_000))
        .map_err(|error| err(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(json!({ "token": token, "expires_at": expires_at })))
}

pub async fn post_terminal_lease_release(
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalLeaseReleaseBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    manager
        .release_lease(&body.terminal, &body.token)
        .map_err(|error| err(StatusCode::CONFLICT, error.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn get_terminal_audit(
    Extension(manager): Extension<Arc<PtyManager>>,
) -> Json<Vec<TerminalAuditEntry>> {
    Json(manager.audit_entries())
}

pub async fn post_terminal_wait(
    Extension(manager): Extension<Arc<PtyManager>>,
    JsonBody(body): JsonBody<TerminalWaitBody>,
) -> Result<Json<TerminalWaitResponse>, (StatusCode, String)> {
    if body.r#match.is_none() == body.until_idle_ms.is_none() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "set exactly one of: match, until_idle_ms",
        ));
    }
    let pattern = body
        .r#match
        .as_deref()
        .map(Regex::new)
        .transpose()
        .map_err(|error| err(StatusCode::BAD_REQUEST, format!("invalid regex: {error}")))?;
    let timeout_ms = body.timeout_ms.unwrap_or(10_000).clamp(1, 300_000);
    let (snapshot, matched, idle, exit_code) = manager
        .wait_for_output(
            &body.terminal,
            body.after.unwrap_or(0),
            pattern.as_ref(),
            body.until_idle_ms,
            Duration::from_millis(timeout_ms),
        )
        .await
        .map_err(|error| err(StatusCode::NOT_FOUND, error.to_string()))?;
    let text = if body.raw {
        String::from_utf8_lossy(&snapshot.data).into_owned()
    } else {
        String::from_utf8_lossy(&strip_ansi_escapes::strip(&snapshot.data)).into_owned()
    };
    let exited = exit_code.is_some();
    let satisfied = matched || idle;
    Ok(Json(TerminalWaitResponse {
        satisfied,
        matched,
        idle,
        timed_out: !satisfied && !exited,
        exited,
        exit_code,
        text,
        start_offset: snapshot.start_offset,
        end_offset: snapshot.end_offset,
        available_start_offset: snapshot.available_start_offset,
        truncated: snapshot.truncated,
    }))
}

pub async fn get_windows(
    Extension(app): Extension<AppHandle>,
    Extension(state): Extension<Arc<IykeState>>,
) -> Json<Vec<IykeWindowInfo>> {
    let registry = app.state::<WindowRegistry>();
    let panes = state.snapshot().await.panes;
    let mut windows = vec![IykeWindowInfo::from_descriptor(
        WindowDescriptor {
            label: "main".to_string(),
            kind: WindowKind::Primary,
            surface_set: Vec::new(),
            project_id: None,
            layout_key: "main".to_string(),
        },
        panes,
    )];
    windows.extend(
        registry
            .list_live(&app)
            .into_iter()
            .map(|descriptor| IykeWindowInfo::from_descriptor(descriptor, None)),
    );
    Json(windows)
}

pub async fn post_tab_activate(
    Extension(app): Extension<AppHandle>,
    JsonBody(body): JsonBody<TabActivateBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if body.index.is_none() == body.terminal.is_none() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "set exactly one of: index, terminal",
        ));
    }
    app.emit(
        "iyke://tab-activate",
        json!({ "pane": body.pane, "index": body.index, "terminal": body.terminal }),
    )
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(json!({ "ok": true })))
}
