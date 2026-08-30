//! Tauri command surface for the per-pkg child-webview kernel.
//!
//! Thin wrappers around `pkg::webview::WebviewPanesRegistry` methods. Each
//! command takes a `keep_awake::InflightGuard` at the top so the macOS App-Nap
//! and Windows WebView2 visibility mitigations are in effect for the duration
//! of the call (defensive — Linux is a no-op).
//!
//! Parameters use camelCase across the IPC boundary (FE consumes via
//! `pkgWebviewCreate / Destroy / Navigate / SetRect` in `tauri-cmd.ts`).
//! `eval` is intentionally NOT exposed to the frontend — only the kernel
//! and pkg-MCP servers ever drive it; FE-side use would defeat the whole
//! sandboxing model.
//!
//! ## Threading
//!
//! Each command wraps its kernel call in `pkg::webview::run_on_main` so the
//! synchronous `WebviewWindowBuilder::build()` / `Webview::eval()` /
//! `set_position` calls posted to the GTK / NSApplication main loop don't
//! hang. Phase 3c (`plans/shell/2026-05-13-pkg-browser-phase-3c-report.md`)
//! walks through the original diagnosis — async Tauri commands run on tokio
//! workers, same as axum handlers, so both surfaces need the hop.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State, WebviewWindow};

use crate::pkg::keep_awake;
use crate::pkg::webview::{run_on_main, PaneRect, WebviewPanesRegistry};

pub struct WebviewPanesState(pub Arc<WebviewPanesRegistry>);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PkgWebviewCreateResult {
    /// Opaque kernel-assigned label for the webview (`pkg-<slug>-<pane>`).
    /// FE doesn't need this for normal operation; surfaced for debugging.
    pub webview_label: String,
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn pkg_webview_create(
    app: AppHandle,
    // The window that invoked this command — the pane parents to it, not the
    // literal "main" (multi-window: WP-04). "main" for the normal shell, a
    // detached label when a Flavor C/B window mounts a pkg pane.
    calling_window: WebviewWindow,
    state: State<'_, WebviewPanesState>,
    pkg_id: String,
    pane_id: String,
    url: String,
    rect: PaneRect,
    partition: Option<String>,
) -> Result<PkgWebviewCreateResult, String> {
    let _guard = keep_awake::acquire("ikenga pkg_webview_create");
    let panes = state.0.clone();
    let app_for_main = app.clone();
    let parent_label = calling_window.label().to_string();
    let webview_label = run_on_main(&app, move || {
        panes.create(
            &app_for_main,
            &pkg_id,
            &pane_id,
            &url,
            rect,
            partition.as_deref(),
            &parent_label,
        )
    })
    .await
    .map_err(|e| format!("{e:#}"))?;
    Ok(PkgWebviewCreateResult { webview_label })
}

#[tauri::command]
pub async fn pkg_webview_destroy(
    app: AppHandle,
    state: State<'_, WebviewPanesState>,
    pkg_id: String,
    pane_id: String,
) -> Result<(), String> {
    // No keep-awake on destroy — teardown should never need to inhibit App Nap.
    let panes = state.0.clone();
    run_on_main(&app, move || panes.destroy(&pkg_id, &pane_id))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn pkg_webview_navigate(
    app: AppHandle,
    state: State<'_, WebviewPanesState>,
    pkg_id: String,
    pane_id: String,
    url: String,
) -> Result<(), String> {
    let _guard = keep_awake::acquire("ikenga pkg_webview_navigate");
    let panes = state.0.clone();
    run_on_main(&app, move || panes.navigate(&pkg_id, &pane_id, &url))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn pkg_webview_set_rect(
    app: AppHandle,
    state: State<'_, WebviewPanesState>,
    pkg_id: String,
    pane_id: String,
    rect: PaneRect,
) -> Result<(), String> {
    // No keep-awake on rect changes — typically fired during resize, which
    // by definition means the window is in focus.
    let panes = state.0.clone();
    run_on_main(&app, move || panes.set_rect(&pkg_id, &pane_id, rect))
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn pkg_webview_clear_session(
    app: AppHandle,
    state: State<'_, WebviewPanesState>,
    pkg_id: String,
    pane_id: String,
) -> Result<(), String> {
    let panes = state.0.clone();
    let app_for_main = app.clone();
    run_on_main(&app, move || panes.clear_session(&app_for_main, &pkg_id, &pane_id))
        .await
        .map_err(|e| format!("{e:#}"))
}
