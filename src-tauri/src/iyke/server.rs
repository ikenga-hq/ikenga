//! Iyke axum server. Binds to `127.0.0.1:0`, registers the read + write
//! routes under the bearer-token middleware, and returns
//! `(url, port, shutdown_tx)`. Caller is responsible for keeping the
//! shutdown sender alive — dropping it triggers graceful shutdown.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    http::{HeaderValue, Method as HttpMethod},
    middleware,
    routing::{get, post},
    Extension, Router,
};
use tauri::AppHandle;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};

use super::auth::{require_token, AuthState};
use super::browser_handlers::{
    get_browser_list, get_browser_profiles, get_browser_targets, post_browser_back,
    post_browser_click, post_browser_close, post_browser_eval, post_browser_fill,
    post_browser_focus, post_browser_forward, post_browser_goto, post_browser_launch_profile,
    post_browser_open, post_browser_pause, post_browser_press_key, post_browser_read_text,
    post_browser_reload, post_browser_reply, post_browser_resume, post_browser_screenshot,
    post_browser_select, post_browser_snapshot, post_browser_wait_for, BrowserPort,
};
use super::browser_rpc::BrowserRpc;
use super::browser_sessions::{
    get_browser_session_list, post_browser_session_create, post_browser_session_delete,
    post_browser_session_resolve,
};
use super::claude::{
    get_claude_asset_pins, get_claude_assets_list, post_claude_asset_pin, post_claude_asset_unpin,
};
use super::comments::{get_pin_read, post_pin_acknowledge, post_pin_resolve};
use super::handlers::{
    get_chi_list, get_chi_status, get_dom, get_iframe_state, get_logs, get_network, get_pkg_list,
    get_query_cache, get_state, get_terminal_read, post_chi_cancel, post_chi_resume, post_chi_run,
    post_click, post_close, post_devtools, post_focus, post_go, post_iframe_message, post_key,
    post_mode, post_oba_install_local, post_open,
    post_pkg_badge_set, post_pkg_dev_register, post_pkg_dev_reload, post_pkg_dev_unregister,
    post_pkg_health_remove, post_pkg_health_remove_all, post_pkg_health_scan, post_pkg_install,
    post_pkg_scope_set, post_pkg_uninstall, post_refresh, post_resize, post_screenshot_pane,
    post_screenshot_window,
    post_sidebar, post_split, post_terminal_send, post_type, post_wait,
};
use super::hooks::{get_hook_events, post_hook_decision, post_hook_event};
use super::ide::{get_ide_lock_status, post_ide_open_file};
use super::layout::{get_layout, post_layout_reset};
use super::mcp::{get_mcp_list, post_mcp_restart};
use super::memory::{
    get_agent_inbox, get_kv_get, get_kv_list, get_lock_status, get_scratchpad_list,
    get_scratchpad_read, get_scratchpad_watch, get_timer_list, get_todo_list, post_agent_inbox_ack,
    post_agent_register, post_kv_delete, post_kv_set, post_lock_acquire, post_lock_release,
    post_lock_renew, post_scratchpad_append, post_scratchpad_delete, post_scratchpad_write,
    post_timer_cancel, post_timer_schedule, post_todo_complete, post_todo_create, post_todo_update,
    TimerScheduler,
};
use super::pa_actions::post_pa_actions_pause;
use super::permissions_audit::get_violations_list;
use super::pkg_dispatch::pkg_dispatch;
use super::projects::{
    get_project_active, get_project_list, post_project_archive, post_project_create,
    post_project_set_active, post_project_update,
};
use super::secrets::{get_secret, get_secret_list, post_secret_delete, post_secret_set};
use super::state::IykeState;
use super::statusline::{get_statusline_snapshot, post_statusline_event};
use super::tasks::{get_task_list, post_task_complete, post_task_create, post_task_update};
use super::terminal::{
    get_terminal_audit, get_terminals, get_windows, post_tab_activate, post_terminal_get,
    post_terminal_kill, post_terminal_label, post_terminal_lease_acquire,
    post_terminal_lease_release, post_terminal_spawn, post_terminal_wait,
};
use super::trust::{get_trust_list, get_trust_preview, post_trust_grant, post_trust_revoke};
use super::IykeRpc;
use crate::commands::db::PaDb;
use crate::commands::ScreenshotPending;
use crate::pkg::registries::IykeRoutesRegistry;
use crate::pkg::webview::WebviewPanesRegistry;
use crate::pty::PtyManager;

#[allow(clippy::too_many_arguments)]
pub async fn serve(
    state: Arc<IykeState>,
    rpc: IykeRpc,
    browser_rpc: BrowserRpc,
    webview_panes: Arc<WebviewPanesRegistry>,
    playwright_proxy: Arc<super::playwright_proxy::PlaywrightProxy>,
    pa_db: Arc<PaDb>,
    pty_manager: Arc<PtyManager>,
    token: String,
    app_handle: AppHandle,
    screenshot_pending: ScreenshotPending,
    iyke_routes: Arc<IykeRoutesRegistry>,
    timer_scheduler: TimerScheduler,
) -> Result<(String, u16, oneshot::Sender<()>)> {
    // Bind first so we can thread the live port into handlers that need to
    // bake it into in-page eval closures (pkg-browser reply fetch).
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .context("bind iyke listener")?;
    let local_addr = listener.local_addr().context("local_addr")?;
    let port = local_addr.port();
    let url = format!("http://{}", local_addr);

    let auth_state = Arc::new(AuthState { token });

    // Authed routes — everything except the pkg-browser reply endpoint,
    // which uses a per-request oneshot_token instead of the global bearer.
    let authed = Router::new()
        .route("/iyke/state", get(get_state))
        .route("/iyke/go", post(post_go))
        .route("/iyke/mode", post(post_mode))
        .route("/iyke/sidebar", post(post_sidebar))
        .route("/iyke/open", post(post_open))
        .route("/iyke/split", post(post_split))
        .route("/iyke/focus", post(post_focus))
        .route("/iyke/close", post(post_close))
        .route("/iyke/resize", post(post_resize))
        .route("/iyke/refresh", post(post_refresh))
        .route("/iyke/screenshot/window", post(post_screenshot_window))
        .route("/iyke/screenshot/pane", post(post_screenshot_pane))
        // Phase A — runtime inspection + driving.
        .route("/iyke/dom", get(get_dom))
        .route("/iyke/logs", get(get_logs))
        .route("/iyke/network", get(get_network))
        .route("/iyke/query-cache", get(get_query_cache))
        .route("/iyke/click", post(post_click))
        .route("/iyke/type", post(post_type))
        .route("/iyke/key", post(post_key))
        .route("/iyke/terminal/send", post(post_terminal_send))
        .route("/iyke/terminal/read", get(get_terminal_read))
        .route("/iyke/terminal/list", get(get_terminals))
        .route("/iyke/terminal/get", post(post_terminal_get))
        .route("/iyke/terminal/wait", post(post_terminal_wait))
        .route("/iyke/terminal/spawn", post(post_terminal_spawn))
        .route("/iyke/terminal/kill", post(post_terminal_kill))
        .route("/iyke/terminal/label", post(post_terminal_label))
        .route(
            "/iyke/terminal/lease/acquire",
            post(post_terminal_lease_acquire),
        )
        .route(
            "/iyke/terminal/lease/release",
            post(post_terminal_lease_release),
        )
        .route("/iyke/terminal/audit", get(get_terminal_audit))
        .route("/iyke/windows", get(get_windows))
        .route("/iyke/tab/activate", post(post_tab_activate))
        .route("/iyke/wait", post(post_wait))
        .route("/iyke/devtools", post(post_devtools))
        .route("/iyke/statusline/event", post(post_statusline_event))
        .route("/iyke/statusline/snapshot", get(get_statusline_snapshot))
        .route("/iyke/hooks/event", post(post_hook_event))
        .route("/iyke/hooks/events", get(get_hook_events))
        .route("/iyke/hooks/decision", post(post_hook_decision))
        .route("/iyke/ide/open_file", post(post_ide_open_file))
        .route("/iyke/ide/lock", get(get_ide_lock_status))
        .route("/iyke/pkg/install", post(post_pkg_install))
        .route("/iyke/pkg/uninstall", post(post_pkg_uninstall))
        .route("/iyke/pkg/list", get(get_pkg_list))
        .route("/iyke/pkg/scope-set", post(post_pkg_scope_set))
        .route("/iyke/pkg/badge-set", post(post_pkg_badge_set))
        .route("/iyke/oba/install-local", post(post_oba_install_local))
        .route("/iyke/pkg/dev/register", post(post_pkg_dev_register))
        .route("/iyke/pkg/dev/unregister", post(post_pkg_dev_unregister))
        .route("/iyke/pkg/dev/reload", post(post_pkg_dev_reload))
        .route("/iyke/pkg/health/scan", post(post_pkg_health_scan))
        .route("/iyke/pkg/health/remove", post(post_pkg_health_remove))
        .route(
            "/iyke/pkg/health/remove-all",
            post(post_pkg_health_remove_all),
        )
        .route("/iyke/iframe-state", get(get_iframe_state))
        .route("/iyke/iframe-message", post(post_iframe_message))
        // pkg-browser bridge (kernel-direct + eval).
        .route("/iyke/browser/open", post(post_browser_open))
        .route("/iyke/browser/close", post(post_browser_close))
        .route("/iyke/browser/list", get(get_browser_list))
        // Attach picker data (chrome-only global queries — no pane).
        .route("/iyke/browser/profiles", get(get_browser_profiles))
        .route("/iyke/browser/targets", get(get_browser_targets))
        .route(
            "/iyke/browser/launch_profile",
            post(post_browser_launch_profile),
        )
        .route("/iyke/browser/focus", post(post_browser_focus))
        .route("/iyke/browser/goto", post(post_browser_goto))
        .route("/iyke/browser/back", post(post_browser_back))
        .route("/iyke/browser/forward", post(post_browser_forward))
        .route("/iyke/browser/reload", post(post_browser_reload))
        .route("/iyke/browser/snapshot", post(post_browser_snapshot))
        .route("/iyke/browser/read-text", post(post_browser_read_text))
        .route("/iyke/browser/screenshot", post(post_browser_screenshot))
        .route("/iyke/browser/click", post(post_browser_click))
        .route("/iyke/browser/fill", post(post_browser_fill))
        .route("/iyke/browser/select", post(post_browser_select))
        .route("/iyke/browser/press-key", post(post_browser_press_key))
        .route("/iyke/browser/wait-for", post(post_browser_wait_for))
        .route("/iyke/browser/eval", post(post_browser_eval))
        // Pause / resume (Phase 5).
        .route("/iyke/browser/pause", post(post_browser_pause))
        .route("/iyke/browser/resume", post(post_browser_resume))
        // Named sessions (Phase 4).
        .route(
            "/iyke/browser/session/create",
            post(post_browser_session_create),
        )
        .route("/iyke/browser/session/list", get(get_browser_session_list))
        .route(
            "/iyke/browser/session/delete",
            post(post_browser_session_delete),
        )
        .route(
            "/iyke/browser/session/resolve",
            post(post_browser_session_resolve),
        )
        // Projects (Phase 0 of projects-first-class plan).
        .route("/iyke/project/list", get(get_project_list))
        .route("/iyke/project/create", post(post_project_create))
        .route("/iyke/project/update", post(post_project_update))
        .route("/iyke/project/archive", post(post_project_archive))
        .route("/iyke/project/set-active", post(post_project_set_active))
        .route("/iyke/project/active", get(get_project_active))
        // Chi-first agent surface (WP-03).
        .route("/iyke/chi/run", post(post_chi_run))
        .route("/iyke/chi/resume", post(post_chi_resume))
        .route("/iyke/chi/status", get(get_chi_status))
        .route("/iyke/chi/list", get(get_chi_list))
        .route("/iyke/chi/cancel", post(post_chi_cancel))
        // Claude config (Phase 4 — 4-tier discovery + pins).
        .route("/iyke/claude/assets", get(get_claude_assets_list))
        .route("/iyke/claude/asset/pin", post(post_claude_asset_pin))
        .route("/iyke/claude/asset/unpin", post(post_claude_asset_unpin))
        .route("/iyke/claude/asset/pins", get(get_claude_asset_pins))
        // MCP supervisor + per-project resolved set (Phase 5).
        .route("/iyke/mcp/list", get(get_mcp_list))
        .route("/iyke/mcp/restart", post(post_mcp_restart))
        // Trust gating (Phase 9). Grant is human-only; the MCP tools
        // surface read-only status only.
        .route("/iyke/trust/list", get(get_trust_list))
        .route("/iyke/trust/grant", post(post_trust_grant))
        .route("/iyke/trust/revoke", post(post_trust_revoke))
        .route("/iyke/trust/preview/:pkg_id", get(get_trust_preview))
        // Runtime-ACL violations (2026-05-15). Read-only by design — clearing
        // is a human action via Settings → Pkgs only.
        .route("/iyke/violations/list", get(get_violations_list))
        .route("/iyke/layout/get", get(get_layout))
        .route("/iyke/layout/reset", post(post_layout_reset))
        .route("/iyke/secret/get", get(get_secret))
        .route("/iyke/secret/list", get(get_secret_list))
        .route("/iyke/secret/set", post(post_secret_set))
        .route("/iyke/secret/delete", post(post_secret_delete))
        // Memory primitives (Phase 1 — DESIGN.md §4-6).
        .route("/iyke/scratchpad/write", post(post_scratchpad_write))
        .route("/iyke/scratchpad/append", post(post_scratchpad_append))
        .route("/iyke/scratchpad/read", get(get_scratchpad_read))
        .route("/iyke/scratchpad/list", get(get_scratchpad_list))
        .route("/iyke/scratchpad/watch", get(get_scratchpad_watch))
        .route("/iyke/scratchpad/delete", post(post_scratchpad_delete))
        // Approve-gate producer hand-off (WP-8) — mcp-iyke `pa_actions_pause` tool.
        .route("/iyke/pa-actions/pause", post(post_pa_actions_pause))
        .route("/iyke/kv/set", post(post_kv_set))
        .route("/iyke/kv/get", get(get_kv_get))
        .route("/iyke/kv/delete", post(post_kv_delete))
        .route("/iyke/kv/list", get(get_kv_list))
        .route("/iyke/lock/acquire", post(post_lock_acquire))
        .route("/iyke/lock/status", get(get_lock_status))
        .route("/iyke/lock/release", post(post_lock_release))
        .route("/iyke/lock/renew", post(post_lock_renew))
        .route("/iyke/agent/register", post(post_agent_register))
        .route("/iyke/agent/inbox", get(get_agent_inbox))
        .route("/iyke/agent/inbox/ack", post(post_agent_inbox_ack))
        .route("/iyke/task/list", get(get_task_list))
        .route("/iyke/task/create", post(post_task_create))
        .route("/iyke/task/update", post(post_task_update))
        .route("/iyke/task/complete", post(post_task_complete))
        .route("/iyke/todo/create", post(post_todo_create))
        .route("/iyke/todo/update", post(post_todo_update))
        .route("/iyke/todo/list", get(get_todo_list))
        .route("/iyke/todo/complete", post(post_todo_complete))
        .route("/iyke/timer/schedule", post(post_timer_schedule))
        .route("/iyke/timer/cancel", post(post_timer_cancel))
        .route("/iyke/timer/list", get(get_timer_list))
        // Artifact-grid pin comments (see migration 0022). Claude reads pin
        // context via read; lifecycle transitions via acknowledge/resolve.
        .route("/iyke/pin/read", get(get_pin_read))
        .route("/iyke/pin/acknowledge", post(post_pin_acknowledge))
        .route("/iyke/pin/resolve", post(post_pin_resolve))
        // Catch-all for package-installed routes. The dispatcher reads the
        // method+path off the request and consults `IykeRoutesRegistry`.
        .route("/pkg/*path", get(pkg_dispatch).post(pkg_dispatch))
        .layer(middleware::from_fn_with_state(
            auth_state.clone(),
            require_token,
        ));

    // Unauthed reply endpoint — partner-site JS POSTs here to fulfill an
    // in-flight pkg-browser request. Auth is via the per-request
    // `oneshot_token` validated inside `BrowserRpc::resolve`.
    let unauthed = Router::new().route("/iyke/browser/_reply", post(post_browser_reply));

    let app = authed
        .merge(unauthed)
        .layer(Extension(state))
        .layer(Extension(rpc))
        .layer(Extension(browser_rpc))
        .layer(Extension(webview_panes))
        .layer(Extension(playwright_proxy))
        .layer(Extension(pa_db))
        .layer(Extension(pty_manager))
        .layer(Extension(BrowserPort(port)))
        .layer(Extension(auth_state))
        .layer(Extension(app_handle))
        .layer(Extension(screenshot_pending))
        .layer(Extension(iyke_routes))
        .layer(Extension(timer_scheduler))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([HttpMethod::GET, HttpMethod::POST, HttpMethod::OPTIONS])
                .allow_headers(Any),
        );

    // Silence the warning if HeaderValue isn't directly used in this file.
    let _ = HeaderValue::from_static;

    let (tx, rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        let server =
            axum::serve(listener, app.into_make_service()).with_graceful_shutdown(async move {
                let _ = rx.await;
            });
        if let Err(e) = server.await {
            log::warn!("iyke server exited: {e}");
        }
    });

    Ok((url, port, tx))
}
