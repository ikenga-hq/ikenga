use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::IykeRuntimeState;
use crate::iyke::hook_settings;
use crate::pty::{foreground::ForegroundProcess, PtyManager, SpawnOpts, TerminalDescriptor};

/// Trailing scrollback handed to a window that is attaching to a live PTY,
/// plus the token that releases the gate `pty_attach_begin` installed. `data`
/// is base64 (same encoding as the `pty://{id}` live events); `endOffset` is
/// the cumulative byte count those bytes end at — the exact offset the first
/// post-arm live chunk starts from, so the two tile the stream with no overlap
/// and nothing to dedup.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyAttachSnapshot {
    data: String,
    end_offset: u64,
    token: u64,
}

#[tauri::command]
pub async fn pty_spawn(
    app: AppHandle,
    manager: State<'_, Arc<PtyManager>>,
    runtime: State<'_, IykeRuntimeState>,
    terminal_id: Option<String>,
    title: Option<String>,
    cwd: String,
    cmd: Vec<String>,
    env: Option<HashMap<String, String>>,
    rows: u16,
    cols: u16,
    settings_path: Option<String>,
) -> Result<String, String> {
    if cmd.is_empty() {
        return Err("cmd must contain at least one element".into());
    }

    // Write the per-terminal claude hook settings file before the child
    // starts. The file path is deterministic and was already baked into the
    // argv by the frontend; writing it here means the live port + token land
    // in `claude`'s hands at the exact moment it reads `--settings`.
    if let Some(path) = settings_path {
        if let Some(term_id) = terminal_id.as_deref() {
            let guard = runtime.lock().await;
            if let Some(rt) = guard.as_ref() {
                let expected: PathBuf = rt
                    .hooks_settings_dir()
                    .join(hook_settings::terminal_file_name(term_id));
                if Path::new(&path) == expected {
                    if let Err(e) = hook_settings::write_for_terminal(
                        rt.hooks_settings_dir(),
                        rt.port,
                        &rt.token,
                        Some(term_id),
                    ) {
                        tracing::warn!("pty_spawn: could not write claude hook settings for {term_id}: {e:#}");
                    } else {
                        tracing::info!("pty_spawn: wrote claude hook settings for {term_id}");
                    }
                } else {
                    tracing::warn!(
                        ?path,
                        ?expected,
                        "pty_spawn: settings path mismatch; refusing to write"
                    );
                }
            } else {
                tracing::warn!("pty_spawn: iyke runtime not ready; skipping hook settings");
            }
        }
    }

    let opts = SpawnOpts {
        terminal_id,
        title,
        cwd,
        cmd,
        env: env.unwrap_or_default(),
        rows,
        cols,
    };

    manager
        .spawn(app, opts)
        .await
        .map_err(|e| format!("spawn failed: {e}"))
}

#[tauri::command]
pub async fn pty_write(
    manager: State<'_, Arc<PtyManager>>,
    id: String,
    data: String,
) -> Result<(), String> {
    manager
        .write(&id, data.as_bytes())
        .map_err(|e| format!("write failed: {e}"))
}

#[tauri::command]
pub async fn pty_resize(
    manager: State<'_, Arc<PtyManager>>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    manager
        .resize(&id, rows, cols)
        .map_err(|e| format!("resize failed: {e}"))
}

#[tauri::command]
pub async fn pty_kill(manager: State<'_, Arc<PtyManager>>, id: String) -> Result<(), String> {
    manager.kill(&id).map_err(|e| format!("kill failed: {e}"))
}

/// Step 1 of the atomic attach handshake, called by a window attaching to a PTY
/// that is already running (a popped-out terminal). Snapshots the trailing
/// scrollback and gates the stream under one lock — see
/// `PtyManager::attach_begin`. The caller MUST follow with `pty_attach_arm`
/// once its listener is registered; a watchdog releases the gate after
/// `ATTACH_GATE_TIMEOUT` if it doesn't, so a window that dies mid-handshake
/// cannot stall the terminal. Returns `None` once the session has exited and
/// been reaped.
#[tauri::command]
pub async fn pty_attach_begin(
    manager: State<'_, Arc<PtyManager>>,
    id: String,
) -> Result<Option<PtyAttachSnapshot>, String> {
    let Some(snap) = manager.attach_begin(&id) else {
        return Ok(None);
    };

    // Watchdog. `attach_arm` is token-checked, so this can never release a
    // *later* handshake's gate — a stale fire is a no-op.
    let watchdog = Arc::clone(&manager);
    let watched_id = id.clone();
    let token = snap.token;
    tokio::spawn(async move {
        tokio::time::sleep(crate::pty::ATTACH_GATE_TIMEOUT).await;
        if watchdog.attach_arm(&watched_id, token) {
            tracing::warn!(
                pty = %watched_id,
                "attach handshake never armed; watchdog released the gate"
            );
        }
    });

    let engine = base64::engine::general_purpose::STANDARD;
    Ok(Some(PtyAttachSnapshot {
        data: engine.encode(&snap.data),
        end_offset: snap.end_offset,
        token: snap.token,
    }))
}

/// Step 2 of the atomic attach handshake: the caller's listener is registered,
/// so release the gate. Everything the PTY emitted during the handshake is
/// flushed as the first chunk that listener sees, starting exactly where the
/// snapshot ended. `false` means the gate was already released (watchdog, or a
/// superseding attach) — a no-op, not an error.
#[tauri::command]
pub async fn pty_attach_arm(
    manager: State<'_, Arc<PtyManager>>,
    id: String,
    token: u64,
) -> Result<bool, String> {
    Ok(manager.attach_arm(&id, token))
}

/// Foreground command for a single PTY. Returns `None` when the PTY is gone
/// or the platform can't surface the foreground PG (macOS/Windows in v0).
#[tauri::command]
pub async fn pty_foreground(
    manager: State<'_, Arc<PtyManager>>,
    id: String,
) -> Result<Option<ForegroundProcess>, String> {
    Ok(manager.foreground(&id))
}

/// Foreground snapshot across every live PTY. Used by the routing dispatcher
/// (and the artifact-grid status indicator) to find the active claude
/// session. Keyed by PTY id; PTYs with no observable foreground are omitted.
#[tauri::command]
pub async fn pty_foreground_snapshot(
    manager: State<'_, Arc<PtyManager>>,
) -> Result<HashMap<String, ForegroundProcess>, String> {
    Ok(manager.foreground_snapshot())
}

/// Every live terminal descriptor — the same view `GET /iyke/terminal/list`
/// serves to agents, exposed to the frontend so tab titles can name a terminal
/// by what it actually is (cwd + live foreground command + any agent-assigned
/// label) instead of a constant "Terminal".
///
/// Read-only and cheap: the foreground lookup behind it is cached for 1s per
/// PTY in `pty::foreground`, so a few-second UI poll costs one `/proc` read
/// per terminal at worst.
#[tauri::command]
pub async fn pty_terminal_list(
    manager: State<'_, Arc<PtyManager>>,
) -> Result<Vec<TerminalDescriptor>, String> {
    Ok(manager.list_terminals())
}

/// Detects available host shell profiles (e.g. PowerShell 7, Windows PowerShell, WSL distros, Git Bash, cmd.exe on Windows, or zsh/bash on Unix).
#[tauri::command]
pub async fn terminal_detect_shells() -> Result<Vec<crate::terminal::shell_detect::ShellProfile>, String> {
    Ok(crate::terminal::shell_detect::detect_shells())
}
