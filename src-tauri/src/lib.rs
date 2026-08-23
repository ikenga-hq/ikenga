mod agent_detect;
pub mod claude;
mod commands;
pub mod engines;
pub mod env_files;
mod fs_roots;
mod fs_watch;
mod iyke;
pub mod path_fix;
mod pkg;
mod pkg_content;
pub mod platform;
mod pty;
mod runtime;
pub mod vault_key;
mod terminal;


mod viewer_server;
// Multi-window substrate (plans/multi-window): the G-WINDOW-MODEL contract
// (WP-02) + the window registry / spawn-close-list commands (WP-03).
mod window;

use std::sync::Arc;

use tauri::Manager;
// tauri-plugin-sql is loaded as a plugin (below) so the frontend's
// `@tauri-apps/plugin-sql` callers resolve, but it does NOT own the
// migration list — that lives in `commands::db::ensure_schema`.
use tokio::sync::Mutex;

use commands::db::PaDb;
use commands::screenshot::new_pending as new_screenshot_pending;
use commands::{
    activity_pins_add, activity_pins_list, activity_pins_remove, activity_pins_reorder,
    activity_pins_resolve_artifact, activity_pins_touch_open, activity_sections_create,
    activity_sections_list, activity_sections_remove, activity_sections_update,
    agent_ops_delete_job, agent_ops_list_jobs, agent_ops_run_now, agent_ops_set_enabled,
    agent_ops_tail_run, agent_ops_upsert_job, atelier_file_read, atelier_file_write, backup_delete,
    backup_export, backup_import, backup_list, chi_cancel, chi_list, chi_resume, chi_run,
    chi_status, ChiRuntime, claude_asset_list_pins, claude_asset_pin, claude_asset_unpin,
    claude_assets_discover, claude_config_load, claude_config_read_file, claude_config_unwatch,
    claude_config_watch, claude_list_sessions, claude_primitive_copy, claude_primitive_copy_batch,
    claude_primitive_disable, claude_primitive_disable_for, claude_primitive_enable,
    claude_primitive_enable_for, claude_primitive_move, claude_primitive_remove,
    claude_primitive_remove_for, claude_read_jsonl, claude_store_import, claude_store_list,
    comment_create, comment_delete, comment_get, comment_list, comment_record_routing,
    comment_route, comment_set_status, data_health_scan, db_exec, db_export_ndjson,
    db_import_ndjson, db_query, dev_bind_port, dev_release_port, engine_layout, fs_exists, fs_kind,
    fs_list, fs_mime, fs_mkdir, fs_read, fs_rename, fs_roots_add, fs_roots_list, fs_roots_remove,
    fs_roots_reset, fs_search, fs_trash, fs_unwatch, fs_watch, fs_write, iyke_action_done,
    iyke_dom_done, iyke_dom_query, iyke_endpoint, iyke_log_push, iyke_mcp_info, iyke_network_push,
    iyke_query_cache_done, iyke_set_shell, iyke_terminal_read_done, iyke_terminal_spawn_done,
    iyke_wait_done, list_all_skill_actions, list_skill_actions, oba_auto_update_all,
    oba_backfill_registry, oba_check_update, oba_dependents, oba_forget, oba_install_bundle,
    oba_install_git, oba_install_local, oba_install_npx, oba_install_with_deps,
    oba_missing_requires, oba_relink_dependents, oba_safe_delete, oba_set_auto_update,
    oba_unlink_one, oba_update, os_username, pin_screenshot_write, pkg_content_html,
    pkg_content_revoke, pkg_content_url, pkg_db_diag, pkg_dev_register, pkg_dev_reload,
    pkg_dev_unregister, pkg_discover_workspace, pkg_fetch, pkg_health_remove,
    pkg_health_remove_all, pkg_health_scan, pkg_install_from_path, pkg_install_from_registry,
    pkg_invoke, pkg_is_trusted_for_elevated, pkg_kernel_status, pkg_mcp_call, pkg_preview_manifest,
    pkg_screenshot, pkg_set_enabled, pkg_set_scope, pkg_settings_get, pkg_settings_set,
    pkg_sidecar_call, pkg_sidecar_rpc_send, pkg_sidecar_rpc_shutdown,
    pkg_studio_request_project_access, pkg_supervisor_restart, pkg_uninstall, pkg_webview_create,
    pkg_webview_destroy, pkg_webview_navigate, pkg_webview_set_rect, project_archive,
    project_artifacts_walk, project_create, project_get_active, project_inventory, project_list,
    project_scaffold_claude, project_set_active, project_skills_list, project_update,
    pty_attach_arm, pty_attach_begin, pty_foreground, pty_foreground_snapshot, pty_kill,
    pty_resize, pty_spawn, pty_terminal_list, pty_write, runtime_retry_bun_fetch,
    terminal_detect_shells,
    screenshot_capture_done, screenshot_capture_failed, screenshot_capture_native_crop,
    screenshot_get_config, screenshot_pane, screenshot_set_dir, screenshot_window, secrets_delete,
    secrets_delete_scoped, secrets_get, secrets_get_scoped, secrets_list_keys,
    secrets_list_keys_scoped, secrets_set, secrets_set_scoped, secrets_vault_status,
    set_dock_badge, settings_clear_all, settings_get, settings_get_all, settings_set,
    spike_grant_fs_read, spike_setup_test_file, studio_message_append, studio_message_list,
    studio_thread_delete, studio_thread_get, studio_thread_get_or_create,
    studio_thread_list_recent, window_close, window_list, window_spawn, ChiCache, KernelState,
    PkgContentState, PkgSettingsState, SidecarSupervisorState, SidecarsRegistryState,
    StreamingSidecarManager, StreamingSidecarManagerState, WebviewPanesState,
};
#[cfg(debug_assertions)]
use commands::{bg_spike_reply, bg_spike_run, new_bg_spike_state};
use commands::{
    pa_actions_commit, pa_actions_list, pa_actions_pause, pa_actions_reject, pa_actions_retry,
    pa_actions_update, pkg_permission_violations_clear, pkg_permission_violations_list,
    pkg_trust_approve, pkg_trust_grant, pkg_trust_list, pkg_trust_list_pending, pkg_trust_preview,
    pkg_trust_reject, pkg_trust_revoke, session_cancel, session_destroy, session_destroy_all,
    session_ensure, session_send, session_tool_result, supabase_config_clear, supabase_config_get,
    supabase_config_set, viewer_port, viewer_serve, viewer_stop, IykeRuntimeState,
    ScreenshotConfigState, ScreenshotConfigStateRef, ScreenshotPending, SecretsLock,
};
use fs_watch::FsWatchManager;
use iyke::{IykeRpc, IykeState};
use pty::PtyManager;
use viewer_server::ViewerServerManager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // CLI intercept: if invoked with --screenshot=window or --screenshot=pane:<id>,
    // talk to the already-running app over its iyke control bridge and exit.
    // Runs before Tokio/Tauri initialize so a second invocation never starts a
    // second instance.
    if let Some(arg) = parse_screenshot_arg(std::env::args()) {
        std::process::exit(run_screenshot_cli(arg));
    }

    init_logging();

    // Repair $PATH on macOS GUI launches (Dock/Finder/Spotlight inherit
    // launchd's minimal env, missing user-installed tools like `claude`).
    // Must run before any sub-process spawns inherit our env.
    path_fix::apply();

    let pty_manager = Arc::new(PtyManager::new());
    let fs_watch_manager = Arc::new(FsWatchManager::new());
    let viewer_manager = Arc::new(ViewerServerManager::new());
    let viewer_manager_for_start = viewer_manager.clone();
    let sessions_manager: claude::session::SessionsState =
        Arc::new(claude::session::SessionsManager::new());
    // ACP server shares the same `SessionsManager` so the legacy
    // `session_*` commands and the new ACP path operate on the same in-
    // memory session table. Phase 11 retires the legacy path.
    let claude_code_engine: engines::claude_code::server::ClaudeCodeEngineState = Arc::new(
        engines::claude_code::server::ClaudeCodeEngine::new(sessions_manager.clone()),
    );
    // Phase 3: Codex PTY engine. Lazy-spawns the `codex` CLI in a PTY on
    // first prompt per thread. Shares the global `PtyManager` so codex
    // children show up in pty diagnostics alongside the rest of the
    // shell's PTY surface.
    let codex_pty_engine: engines::codex_pty::CodexPtyEngineState =
        Arc::new(engines::codex_pty::CodexPtyEngine::new(pty_manager.clone()));
    // Phase 4: cursor-agent scaffold (ADR-013 §6). Runtime stubbed —
    // every method returns `cursor-agent runtime not implemented` until
    // the Cursor CLI is installable and an `--acp`-equivalent is
    // verified. Registered now so the FE engine catalog can show the
    // row and the dispatcher can resolve "cursor-agent" without a
    // special case.
    let cursor_agent_engine: engines::cursor_agent::CursorAgentEngineState =
        Arc::new(engines::cursor_agent::CursorAgentEngine::new());
    // Multi-engine dispatcher used by `commands/chat.rs`. Built once
    // here and `.manage()`d so every Tauri command resolves engines
    // through the same registry.
    let engine_registry: engines::EngineRegistryState = Arc::new(engines::EngineRegistry::new());
    {
        let reg = engine_registry.clone();
        let claude_handle = engines::EngineHandle::ClaudeCode(claude_code_engine.clone());
        let codex_handle = engines::EngineHandle::CodexPty(codex_pty_engine.clone());
        let cursor_agent_handle = engines::EngineHandle::CursorAgent(cursor_agent_engine.clone());
        tauri::async_runtime::block_on(async move {
            reg.insert("claude-code", claude_handle).await;
            reg.insert("codex", codex_handle).await;
            reg.insert("cursor-agent", cursor_agent_handle).await;
        });
    }
    let screenshot_pending: ScreenshotPending = new_screenshot_pending();

    // Migrations are the responsibility of `commands::db::ensure_schema`
    // (the Rust-side sqlx runner). The tauri-plugin-sql migration path only
    // fires when JS calls `Database.load()` and that path has been observed
    // to silently hang (see workspace.tsx::raceTimeout); previously this
    // file maintained a parallel migration list that ran in zero practical
    // contexts. The plugin is still registered so frontend callers of
    // `@tauri-apps/plugin-sql` (Database.load for ad-hoc reads) work; it
    // just doesn't own the schema.

    tauri::Builder::default()
        // MUST be first (documented ordering requirement): if a second launch
        // is detected, this plugin runs its `.setup` before any later plugin
        // initializes, forwards the new process's argv to us here, and exits
        // the second process. Without it, double-clicking the launcher (or the
        // updater relaunch racing a manual reopen) forks a whole second app —
        // its own SQLite handle, iyke bridge, and pkg kernel — which then race
        // the running instance on the shared `ikenga.db`. See the update-flow
        // pileup this was added for. The `--screenshot` CLI intercept above
        // exits before this Builder is constructed, so it never participates.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // A second launch means "show me the app" — always raise the main
            // window (never hide, unlike the ⌘-summon toggle). Mirrors the
            // summon handler's else-branch below.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(global_shortcut_plugin())
        .plugin(tauri_plugin_clipboard_manager::init())
        // Phase 9 (ACP migration): OS notifications for the
        // user-attention hooks (Notification + PermissionRequest). The
        // frontend dispatcher (`src/lib/notifications/acp-notify-bridge.ts`)
        // owns the focus-suppression policy and fires sendNotification
        // through this plugin's JS surface.
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(
            tauri_plugin_stronghold::Builder::new(|_password: &str| {
                // Phase 14: vault key is bootstrapped from the OS keychain.
                // The plugin's `_password` is ignored — we always pass an
                // empty string from the Rust side and the keychain is the
                // single source of truth. If the keychain is unavailable the
                // callback returns an empty Vec; Stronghold::new will fail
                // and the secrets_vault_status command surfaces the error to
                // the UI.
                vault_key::fetch_or_create().unwrap_or_else(|e| {
                    log::error!("vault key bootstrap failed: {e}");
                    Vec::new()
                })
            })
            .build(),
        )
        .manage(pty_manager.clone())
        .manage(fs_watch_manager)
        .manage(viewer_manager)
        .manage(sessions_manager)
        .manage(claude_code_engine)
        .manage(codex_pty_engine)
        .manage(cursor_agent_engine)
        .manage(engine_registry)
        .manage(screenshot_pending.clone())
        .manage(SecretsLock::new())
        .manage(window::WindowRegistry::new())
        .setup(move |app| {
            // Resolve the bundled Bun (per ADR-010) before anything spawns
            // sidecars or MCP children. Idempotent; safe even if the binary
            // is missing — `runtime::resolve_command` then falls back to
            // PATH lookup with a warning.
            runtime::init_from_app(&app.handle());

            // Ensure app data dir exists; SQLite + Stronghold both write here.
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app_data_dir: {e}"))?;
            std::fs::create_dir_all(&data_dir)?;

            // User-configurable FS allowlist. Must be installed before the
            // first call to `commands::resolve_allowlisted` (which fs_*,
            // viewer_serve, and a handful of other commands depend on).
            match fs_roots::FsRoots::load(data_dir.join("fs_roots.json")) {
                Ok(roots) => {
                    let arc = Arc::new(roots);
                    if let Err(e) = fs_roots::install(arc) {
                        log::error!("[fs_roots] install failed: {e:#}");
                    }
                }
                Err(e) => log::error!("[fs_roots] load failed: {e:#}"),
            }

            // One-time rename of the legacy local stores (pa.db → ikenga.db,
            // pa.sqlite → ikenga-terminal.sqlite) BEFORE any pool opens and
            // before the staged-restore swap (which targets ikenga.db).
            // WAL-safe, idempotent, atomic-on-failure: a botched migration
            // leaves the legacy file in place and we open it under the old
            // name. block_on is safe here — setup runs outside any tokio
            // runtime.
            let main_db_name = tauri::async_runtime::block_on(
                commands::backup::migrate_legacy_db_names(&data_dir),
            );

            // If the user staged a backup restore last session, swap pa.db
            // now — before any pool opens. apply_staged_restore_if_present
            // also wipes -wal/-shm sidecars so SQLite re-derives them
            // against the restored snapshot.
            match commands::backup::apply_staged_restore_if_present(&data_dir) {
                Ok(true) => log::info!("[backup] staged restore applied this boot"),
                Ok(false) => {}
                Err(e) => log::error!("[backup] staged restore failed: {e}"),
            }

            // Db wrapper points at the same file the plugin manages. Normally
            // "ikenga.db"; falls back to the legacy "pa.db" if the boot-time
            // rename above was skipped or failed (atomic-on-failure).
            let db_path = data_dir.join(main_db_name);
            let pa_db = Arc::new(PaDb::new(db_path));
            app.manage(pa_db.clone());

            // WP-01 (chi-first agent surface): cache directory + metadata cache.
            app.manage(ChiCache::new(data_dir.clone()));

            // WP-02: runtime state for live Chi children.
            app.manage(Arc::new(ChiRuntime::new()));

            // Phase 1 (projects-first-class): expire-and-delete sweeper for
            // iyke_locks. 30s cadence; cheap.
            iyke::memory::spawn_lock_sweeper(pa_db.clone());

            // Phase 1: timer firing loop. Shares a `TimerScheduler` notify
            // handle with the iyke axum router so timer/schedule and
            // timer/cancel wake the loop without polling.
            let timer_scheduler = iyke::memory::TimerScheduler::new();
            iyke::memory::spawn_timer_fire_loop(
                pa_db.clone(),
                timer_scheduler.clone(),
                app.handle().clone(),
            );

            // Phase 2 of staged-restore: replay the decrypted secrets blob
            // (if present) into Stronghold. Runs after SecretsLock is in
            // state (registered via .manage() above on the Builder) and
            // after pa.db has been swapped. Stronghold opens lazily inside
            // bulk_set, so no extra wiring is needed here.
            commands::backup::apply_staged_secrets(app.handle());

            // Phase 3 of staged-restore: rewrite ${IKENGA_HOME} → current
            // $HOME in the eight path-bearing columns. Independent of
            // secrets; safe to call unconditionally.
            commands::backup::apply_staged_path_rewrites(app.handle());

            // User-overridable screenshot output dir. JSON-backed; loaded
            // synchronously here so the first capture sees the right value.
            let screenshot_cfg: ScreenshotConfigStateRef =
                Arc::new(ScreenshotConfigState::load(&data_dir));
            app.manage(screenshot_cfg);

            log::info!("ikenga app data dir: {}", data_dir.display());

            if let Err(e) = register_summon_shortcut(app.handle()) {
                log::warn!("global shortcut not registered (continuing): {e}");
            }
            register_screenshot_shortcuts(app.handle());

            // Iyke (Phase 11): localhost control bridge. Boot synchronously so
            // the server is ready by the time the webview asks for its
            // endpoint via `iyke_endpoint`. block_on is safe here — setup
            // runs outside any tokio runtime.
            let iyke_state = Arc::new(IykeState::new());
            let iyke_rpc = IykeRpc::new();
            let browser_rpc = iyke::BrowserRpc::new();
            let local_data_dir = app
                .path()
                .app_local_data_dir()
                .map_err(|e| format!("app_local_data_dir: {e}"))?;
            let control_path = local_data_dir.join("control.json");

            // Build the iyke routes registry *before* iyke::start so the
            // axum router gets a handle to it. Same Arc passes to the
            // kernel further down — register/unregister mutate it live.
            let iyke_routes_reg = Arc::new(pkg::registries::IykeRoutesRegistry::new());

            // Webview-panes registry: tracks pkg-owned child webviews; cleanup
            // runs on uninstall so pkgs can't leave orphan browser surfaces
            // behind. Constructed before iyke::start so the pkg-browser HTTP
            // bridge handlers can hold an Arc to it via an Extension layer.
            let webview_panes_reg = Arc::new(pkg::webview::WebviewPanesRegistry::new());


            // Playwright reverse-proxy: lazy-spawns the `@ikenga/sidecar-
            // playwright-browser` Node sidecar on the first chrome request and
            // forwards every `engine=chrome` verb to it. Constructed before
            // iyke::start so the `/iyke/browser/*` handlers can hold an Arc to it
            // via an Extension layer; `.manage()`d after so Tauri commands can
            // too. The proxy holds a `PaDb` handle (G2: `pa_db` exists above) and
            // resolves the sidecar entry by-id on first spawn (A1, WP-A1.1):
            // `IKENGA_PW_SIDECAR` → `pkg_installed.install_path` for
            // `com.ikenga.sidecar-playwright-browser` → in-workspace dev fallback.
            let playwright_proxy = Arc::new(iyke::playwright_proxy::PlaywrightProxy::new(
                pa_db.clone(),
            ));

            let iyke_state_for_start = iyke_state.clone();
            let iyke_rpc_for_start = iyke_rpc.clone();
            let browser_rpc_for_start = browser_rpc.clone();
            let webview_panes_for_start = webview_panes_reg.clone();
            let playwright_proxy_for_start = playwright_proxy.clone();
            let pa_db_for_iyke = pa_db.clone();
            let pty_manager_for_iyke = pty_manager.clone();
            let app_handle_for_iyke = app.handle().clone();
            let pending_for_iyke = screenshot_pending.clone();
            let iyke_routes_for_start = iyke_routes_reg.clone();
            let timer_scheduler_for_start = timer_scheduler.clone();
            let runtime = tauri::async_runtime::block_on(async move {
                iyke::start(
                    iyke_state_for_start,
                    iyke_rpc_for_start,
                    browser_rpc_for_start,
                    webview_panes_for_start,
                    playwright_proxy_for_start,
                    pa_db_for_iyke,
                    pty_manager_for_iyke,
                    control_path,
                    app_handle_for_iyke,
                    pending_for_iyke,
                    iyke_routes_for_start,
                    timer_scheduler_for_start,
                )
                .await
            })
            .map_err(|e| format!("iyke start: {e:#}"))?;

            app.manage(iyke_state);
            app.manage(iyke_rpc);
            app.manage(browser_rpc);
            let runtime_state: IykeRuntimeState = Arc::new(Mutex::new(Some(runtime)));
            app.manage(runtime_state);

            // pkg kernel: stand up the registries, wire AppHandle in, and
            // expose under Tauri state. boot() is a no-op today but lands the
            // call site so the SQLite-backed install path drops in cleanly.
            let sidecars_reg = Arc::new(pkg::registries::SidecarsRegistry::new());
            let perms_reg = Arc::new(pkg::registries::PermissionsRegistry::new(
                app.handle().clone(),
                pa_db.clone(),
            ));
            let settings_reg = Arc::new(pkg::registries::SettingsRegistry::new(pa_db.clone()));
            let cron_reg = Arc::new(pkg::registries::CronRegistry::new(
                app.handle().clone(),
                sidecars_reg.clone(),
            ));
            let ui_routes_reg = Arc::new(pkg::registries::UiRoutesRegistry::new());
            let activity_bar_reg = Arc::new(pkg::registries::ActivityBarRegistry::new());
            // ADR-012 Tracks D + P: kernel-resident engine adapter registry.
            // v1 contains exactly one adapter — `ClaudeCodeAdapter` — and
            // it's registered statically here. Both the `McpRegistry` (MCP
            // fan-out, Track D) and the `EngineAssetsRegistry` (skills /
            // commands / agents fan-out, Track P) hold an Arc to this
            // registry and dispatch their per-pkg writes through it.
            // Construction order matters — the adapters must be registered
            // before the consuming registries are built so boot-replay sees
            // them.
            let engine_adapters_reg = Arc::new(pkg::EngineAdaptersRegistry::new());
            engine_adapters_reg.register(Arc::new(pkg::engine_adapters::ClaudeCodeAdapter::new()));
            // ADR-012 Phase 6, Track G — Gemini CLI portability adapter.
            engine_adapters_reg.register(Arc::new(pkg::engine_adapters::GeminiAdapter::new()));
            // ADR-012 Phase 6, Track C — Codex CLI portability adapter.
            // Registry is order-independent for dispatch; ordering here is
            // claude → gemini → codex purely for readability.
            engine_adapters_reg.register(Arc::new(pkg::engine_adapters::CodexAdapter::new()));
            let engine_assets_reg = Arc::new(
                pkg::registries::EngineAssetsRegistry::new_with_adapters(
                    engine_adapters_reg.clone(),
                ),
            );
            let mcp_reg = Arc::new(pkg::registries::McpRegistry::new(engine_adapters_reg.clone()));
            let queries_reg = Arc::new(pkg::registries::QueriesRegistry::new());
            // `webview_panes_reg` was already constructed above (before iyke::start)
            // so the HTTP bridge handlers can hold an Arc to it. It also feeds the
            // kernel registry list further down — same Arc, so cookie-partition
            // cleanup on uninstall still flows through `Registry::unregister`.
            // Sidecar supervisor: itself a Registry, owns long-lived MCP
            // children for any pkg with `mcp[].lifecycle = "long-lived"`.
            // Held separately as an Arc so `pkg_mcp_call` can dispatch to it
            // without going through the kernel snapshot.
            let sidecar_supervisor = Arc::new(
                pkg::SidecarSupervisor::with_app(app.handle().clone())
                    .with_db(pa_db.clone()),
            );
            let pkg_content_server = pkg_content::PkgContentServer::new();
            // Bind the content server before the kernel boots so that
            // boot-replay's `register()` calls find an already-running server
            // ready to serve. Failure here is non-fatal — iframe mounts will
            // surface the error via mint() instead of the app refusing to
            // start.
            let pkg_content_for_start = pkg_content_server.clone();
            if let Err(e) = tauri::async_runtime::block_on(async move {
                pkg_content_for_start.start().await
            }) {
                log::warn!("[pkg_content] start failed (continuing): {e:#}");
            }

            // Viewer-server: single shared axum bound at startup so every
            // artifact iframe is same-origin with the shell (Vite proxies
            // /__viewer/* to it in dev; in prod the shell loads directly from
            // this server via the programmatic WebviewWindowBuilder below,
            // and the server's catch-all serves the bundled frontend dist via
            // Tauri's `AssetResolver`). The bound port is captured here and
            // threaded into the window URL — if the preferred port (47821) is
            // in use (e.g. a concurrent debug instance), the server falls back
            // to an OS-chosen port and the window still finds it.
            let viewer_for_start = viewer_manager_for_start.clone();
            let viewer_app_handle = app.handle().clone();
            let viewer_port = tauri::async_runtime::block_on(async move {
                viewer_for_start.start(&viewer_app_handle).await
            })
            .map_err(|e| {
                tracing::error!("[viewer] start failed: {e:#}");
                e
            })?;

            // Main window — built programmatically so the prod webview loads
            // from our viewer-server. That puts the shell on the same origin as
            // `/__viewer/*`, which is what lets the audio/video renderers use
            // relative URLs (in dev the Vite proxy stands in for it). It does
            // NOT give the parent reach into artifact iframes: those are
            // sandboxed without `allow-same-origin`, so their documents are
            // opaque whatever URL they load from, and Studio comment-mode,
            // screenshots and the iyke bridge all go through the postMessage
            // bridge in `src/lib/artifact/bridge-messages.ts`.
            // In dev, Vite serves the shell at :1420; in prod the
            // viewer-server's catch-all serves the bundled dist at
            // `viewer_port`. The label "main" matches the capability target
            // in `capabilities/default.json`; `remote.urls` there grants IPC
            // trust to both candidate localhost URLs.
            #[cfg(debug_assertions)]
            let window_url = {
                // Dev loads from Vite (:1420); the viewer-server is still started
                // above for parity, but its port is unused in this build.
                let _ = viewer_port;
                "http://localhost:1420/".to_string()
            };
            #[cfg(not(debug_assertions))]
            let window_url = format!("http://localhost:{viewer_port}/");
            let url: tauri::Url = window_url.parse().expect("static window URL");
            let builder = tauri::WebviewWindowBuilder::new(
                app.handle(),
                "main",
                tauri::WebviewUrl::External(url),
            )
            .title("Ikenga")
            .inner_size(1280.0, 800.0)
            .min_inner_size(960.0, 600.0)
            .resizable(true)
            .visible(true);
            // Drag-drop handler policy is per-OS.
            //
            // macOS: WKWebView's native handler intercepts ALL drag operations
            // before the page sees them, including in-page HTML5 DnD (pane
            // split/move). So it stays DISABLED there and the webview's HTML5
            // events are authoritative — meaning no OS-path drop into terminals
            // on macOS (documented limitation), but the composer's HTML5
            // file-drop keeps working.
            //
            // Linux/Windows: the native handler intercepts only OS→webview FILE
            // drops, not in-page element drags, so pane DnD is unaffected — and
            // it is the ONLY source of a dropped file's absolute path (WebKitGTK
            // blanks `dataTransfer` for security when the handler is off, so
            // HTML5 yields empty `files`/`getData`). We keep it ENABLED and route
            // `onDragDropEvent` paths to the surface under the cursor (see
            // `src/lib/dnd/os-file-drop.ts`); the composer drop is bridged there.
            #[cfg(target_os = "macos")]
            let builder = builder.disable_drag_drop_handler();
            // Overlay title-bar + hidden title are macOS-only; the rest of
            // the window config applies on every platform.
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true);
            let main_win = builder.build()?;
            let _ = main_win.show();
            let _ = main_win.set_focus();
            let kernel = Arc::new(pkg::Kernel::new(
                app.handle().clone(),
                pa_db.clone(),
                vec![
                    sidecars_reg.clone() as Arc<dyn pkg::Registry>,
                    perms_reg as Arc<dyn pkg::Registry>,
                    iyke_routes_reg as Arc<dyn pkg::Registry>,
                    settings_reg.clone() as Arc<dyn pkg::Registry>,
                    cron_reg as Arc<dyn pkg::Registry>,
                    ui_routes_reg as Arc<dyn pkg::Registry>,
                    activity_bar_reg as Arc<dyn pkg::Registry>,
                    engine_assets_reg as Arc<dyn pkg::Registry>,
                    mcp_reg as Arc<dyn pkg::Registry>,
                    queries_reg as Arc<dyn pkg::Registry>,
                    webview_panes_reg.clone() as Arc<dyn pkg::Registry>,
                    pkg_content_server.clone() as Arc<dyn pkg::Registry>,
                    sidecar_supervisor.clone() as Arc<dyn pkg::Registry>,
                ],
            ));
            if let Err(e) = kernel.boot() {
                log::warn!("[pkg_kernel] boot failed (continuing): {e:#}");
            }
            // Auto-install bundled built-in packages (com.ikenga.iyke, …).
            // Skips anything already in pkg_installed, so this runs every
            // boot but only does work on first launch / after uninstall.
            //
            // Two candidate sources, tried in order:
            //   1. The Tauri bundled resource dir (prod builds — populated
            //      from `tauri.conf.json:bundle.resources`).
            //   2. `$CARGO_MANIFEST_DIR/resources/` (dev — bundled resources
            //      aren't copied into target/ during `tauri dev`, so we read
            //      the source tree directly via the compile-time env var).
            // First one with a `builtin-pkgs/` subdir wins.
            // In debug builds, read from the source tree so editing a built-in
            // pkg's skill / command is live without rebuilding the bundle.
            // In release builds, the Tauri-bundled resource_dir is the only
            // sane source — but the glob shape in `tauri.conf.json:resources`
            // dictates the exact subpath, so prefer the source-tree path
            // unless we're in a release build with no source tree available.
            let mut builtin_root: Option<std::path::PathBuf> = None;
            #[cfg(debug_assertions)]
            {
                let dev_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("resources");
                if dev_root.join("builtin-pkgs").is_dir() {
                    builtin_root = Some(dev_root);
                }
            }
            if builtin_root.is_none() {
                if let Ok(resource_dir) = app.path().resource_dir() {
                    if resource_dir.join("builtin-pkgs").is_dir() {
                        builtin_root = Some(resource_dir);
                    }
                }
            }
            match builtin_root {
                Some(root) => {
                    if let Err(e) = kernel.install_builtins(&root) {
                        log::warn!("[pkg_kernel] builtins install failed (continuing): {e:#}");
                    }
                }
                None => log::info!("[pkg_kernel] no builtin-pkgs/ found in resource_dir or CARGO_MANIFEST_DIR/resources"),
            }
            // Pick up pkgs that landed in <app_data_dir>/pkgs/ while the shell
            // was offline — typically CLI-installed pkgs (`ikenga add ...`).
            // Idempotent: already-tracked entries are skipped. Runs after the
            // boot replay + builtin install so it only sees genuinely new dirs.
            if let Err(e) = kernel.install_from_pkgs_dir() {
                log::warn!("[pkg_kernel] pkgs-dir discovery failed (continuing): {e:#}");
            }
            // Phase 2 (projects-first-class): seed the live set from the
            // boot-registered pkgs, then reconcile against the active
            // project so anything scoped to a non-active project is parked
            // on first paint. boot() registered every enabled row; we
            // mark them live, then prune.
            kernel.mark_all_live();
            {
                let db_for_active = pa_db.clone();
                let active_info = tauri::async_runtime::block_on(async move {
                    let pool = db_for_active.ensure_pool().await.ok()?;
                    let id = crate::commands::projects::get_active_project_id(&pool).await.ok()?;
                    // Best-effort: also fetch the active project's root_path
                    // so we can seed `IKENGA_CODEX_PROJECT_ROOT` before the
                    // first Codex install/uninstall fires (ADR-012 follow-up,
                    // 2026-05-18).
                    let root = crate::commands::projects::get_project(&pool, &id)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|p| p.root_path);
                    Some((id, root))
                });
                if let Some((active, root)) = active_info {
                    // Set the Codex adapter's project-root env var before
                    // any registry replay or reconcile fires. Pkgs scoped
                    // to the active project will pick up the new path on
                    // their next install/uninstall.
                    crate::pkg::engine_adapters::codex::set_project_root_env(root.as_deref());
                    if let Err(e) = kernel.reconcile_for_project(&active) {
                        log::warn!("[pkg_kernel] initial reconcile failed (continuing): {e:#}");
                    }
                }
            }
            let kernel_arc_for_listener = kernel.clone();
            app.manage(KernelState(kernel));
            app.manage(PkgSettingsState(settings_reg));
            app.manage(PkgContentState(pkg_content_server));
            // Clone for the post-launch bun-fetch task BEFORE `app.manage`
            // consumes the Arc — the task calls `wake_runtime_blocked()` on a
            // successful fetch to re-spawn sidecars parked on RuntimeNotReady.
            let sup_for_bun = sidecar_supervisor.clone();
            app.manage(SidecarSupervisorState(sidecar_supervisor));
            app.manage(SidecarsRegistryState(sidecars_reg));
            app.manage(StreamingSidecarManagerState(Arc::new(
                StreamingSidecarManager::new(),
            )));
            app.manage(WebviewPanesState(webview_panes_reg));
            // Playwright reverse-proxy — managed as the bare Arc so the iyke
            // Extension layer and any Tauri command resolve the same handle.
            app.manage(playwright_proxy);

            // Phase 2 (projects-first-class): subscribe to
            // `projects:active-changed` and reconcile pkg liveness on
            // every switch. Debounced 250ms because rapid ⌘P spamming
            // through the picker emits one event per step.
            {
                use tauri::Listener;
                let app_for_listener = app.handle().clone();
                let kernel = kernel_arc_for_listener.clone();
                let pa_db_for_listener = pa_db.clone();
                let debounce_token: Arc<std::sync::atomic::AtomicU64> =
                    Arc::new(std::sync::atomic::AtomicU64::new(0));
                app_for_listener.clone().listen("projects:active-changed", move |evt| {
                    // Pull `id` out of the payload.
                    let active = serde_json::from_str::<serde_json::Value>(evt.payload())
                        .ok()
                        .and_then(|v| v.get("id").and_then(|s| s.as_str().map(String::from)));
                    let Some(active) = active else { return };
                    let token =
                        debounce_token.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    let kernel = kernel.clone();
                    let token_check = debounce_token.clone();
                    let pa_db = pa_db_for_listener.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        // If another active-changed landed during the sleep,
                        // a later iteration will win — drop this one.
                        if token_check.load(std::sync::atomic::Ordering::Relaxed) != token {
                            return;
                        }
                        // ADR-012 follow-up: keep `IKENGA_CODEX_PROJECT_ROOT`
                        // in sync with the active project's root_path. The
                        // adapter reads this per-call so future installs see
                        // the new path. Pkgs that stay live across the switch
                        // don't get their existing assets re-materialized
                        // (documented limitation in STATUS.md).
                        if let Ok(pool) = pa_db.ensure_pool().await {
                            let root = crate::commands::projects::get_project(&pool, &active)
                                .await
                                .ok()
                                .flatten()
                                .and_then(|p| p.root_path);
                            crate::pkg::engine_adapters::codex::set_project_root_env(
                                root.as_deref(),
                            );
                        }
                        if let Err(e) = kernel.reconcile_for_project(&active) {
                            log::warn!(
                                "[pkg_kernel] reconcile for active=`{active}` failed: {e:#}"
                            );
                        }
                    });
                });
            }

            // Phase 7 (projects-first-class): re-dump the runtime env-vault
            // file on every project switch so sidecars and per-call MCP
            // children see the active project's resolved secrets cascade
            // on next spawn. The dump itself is sync + Stronghold-locked,
            // so we hop it onto a background thread to keep the Tauri
            // event-loop responsive (same reasoning as the boot-time dump
            // below).
            {
                use tauri::Listener;
                let app_for_secrets = app.handle().clone();
                app_for_secrets
                    .clone()
                    .listen("projects:active-changed", move |_evt| {
                        let app_for_dump = app_for_secrets.clone();
                        std::thread::spawn(move || {
                            match commands::secrets::dump_to_runtime_file(&app_for_dump) {
                                Ok(path) => log::info!(
                                    "env-vault re-dumped after project switch -> {}",
                                    path.display()
                                ),
                                Err(e) => log::warn!(
                                    "env-vault re-dump after project switch skipped: {e}"
                                ),
                            }
                        });
                    });
            }

            // Phase 0.5 background-execution spike state. Debug builds only.
            // See commands/bg_spike.rs.
            #[cfg(debug_assertions)]
            app.manage(new_bg_spike_state());

            // Phase 14: write the runtime env-vault file so the actions
            // sidecar can read vault values via its existing dotenv loader.
            // Best-effort: a failure here just means sidecars fall through
            // to ~/.config/pa-actions/env or ikenga/.env.
            //
            // FE-init-fix (2026-05-13): this used to run synchronously
            // here, but Stronghold::new + get_client can block the setup
            // thread indefinitely on Linux when the snapshot file is in
            // a degraded state — which stalls the GTK event loop and
            // prevents the main window from ever being presented. We
            // hop it onto a background OS thread so setup returns
            // immediately. The actions sidecar only reads the env-vault
            // file when it spawns, which is well after boot.
            let app_for_dump = app.handle().clone();
            std::thread::spawn(move || {
                match commands::secrets::dump_to_runtime_file(&app_for_dump) {
                    Ok(path) => log::info!("env-vault dumped to {}", path.display()),
                    Err(e) => log::warn!("env-vault dump skipped: {e}"),
                }
            });

            // Post-launch bun runtime fetch (B+A hybrid). `init_from_app`
            // never fetches — it only resolves what already exists. If bun
            // didn't resolve at boot (fresh install, no system bun), fetch it
            // now that the window is up. `ensure_bun` owns ALL `runtime://bun`
            // emits via its on_progress closure; this task only wakes the
            // RuntimeNotReady-parked sidecars on success.
            {
                use tauri::Emitter;
                let app_for_bun = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if crate::runtime::bun_ready() {
                        return;
                    }
                    let app_for_emit = app_for_bun.clone();
                    let emit = move |p: crate::runtime::BunFetchProgress| {
                        let _ = app_for_emit.emit("runtime://bun", p.to_payload());
                    };
                    match crate::runtime::ensure_bun(&app_for_bun, emit).await {
                        Ok(_) => {
                            log::info!("[runtime] bun fetched after launch — waking deferred sidecars");
                            sup_for_bun.wake_runtime_blocked();
                        }
                        Err(msg) => {
                            log::warn!("[runtime] post-launch bun fetch failed: {msg}");
                        }
                    }
                });
            }

            // Emit `window://focus-changed` for the PRIMARY window too —
            // detached windows get theirs in `WindowRegistry::spawn`, but the
            // main window is owned here, so hook its focus transitions directly.
            if let Some(main_window) = app.get_webview_window("main") {
                let app_for_focus = app.handle().clone();
                main_window.on_window_event(move |ev| {
                    if let tauri::WindowEvent::Focused(focused) = ev {
                        crate::window::emit_focus_changed(&app_for_focus, "main", *focused);
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // agent-ops host bridge (WP-09 / G-TRIGGER + WP-14 CRUD)
            agent_ops_run_now,
            agent_ops_set_enabled,
            agent_ops_list_jobs,
            agent_ops_upsert_job,
            agent_ops_delete_job,
            agent_ops_tail_run,
            // pty
            pty_spawn,
            pty_write,
            pty_resize,
            pty_kill,
            pty_attach_begin,
            pty_attach_arm,
            pty_foreground,
            pty_foreground_snapshot,
            pty_terminal_list,
            terminal_detect_shells,
            // multi-window substrate (plans/multi-window WP-03)
            window_spawn,
            window_close,
            window_list,
            // fs
            fs_read,
            fs_write,
            fs_mkdir,
            fs_exists,
            fs_kind,
            fs_list,
            fs_mime,
            fs_watch,
            fs_unwatch,
            fs_trash,
            fs_rename,
            fs_search,
            // fs allowlist (user-configurable roots)
            fs_roots_list,
            fs_roots_add,
            fs_roots_remove,
            fs_roots_reset,
            // claude
            claude_list_sessions,
            claude_read_jsonl,
            session_ensure,
            session_send,
            session_tool_result,
            session_cancel,
            session_destroy,
            session_destroy_all,
            // chi-first agent surface (WP-01)
            chi_run,
            chi_resume,
            chi_status,
            chi_list,
            chi_cancel,
            // claude config browser
            claude_config_load,
            claude_config_watch,
            claude_config_unwatch,
            claude_config_read_file,
            // claude config — Phase 4 (4-tier discovery + pin CRUD)
            claude_assets_discover,
            claude_asset_pin,
            claude_asset_unpin,
            claude_asset_list_pins,
            // Ngwa store layer — WP-02 (central store + symlink farm)
            claude_store_list,
            claude_store_import,
            claude_primitive_enable,
            claude_primitive_disable,
            claude_primitive_copy,
            claude_primitive_move,
            claude_primitive_remove,
            // Ngwa Phase-2 v2b write — unified per-engine dispatch + cross-engine copy
            claude_primitive_enable_for,
            claude_primitive_disable_for,
            claude_primitive_remove_for,
            claude_primitive_copy_batch,
            // Ngwa Ọba registry — WP-04 dependent-aware safe delete + WP-06 finish
            oba_dependents,
            oba_safe_delete,
            oba_relink_dependents,
            oba_unlink_one,
            oba_forget,
            oba_backfill_registry,
            oba_install_git,
            oba_install_npx,
            oba_install_local,
            oba_install_bundle,
            oba_install_with_deps,
            oba_missing_requires,
            oba_check_update,
            oba_update,
            oba_auto_update_all,
            oba_set_auto_update,
            os_username,
            // Ngwa Phase-2 cross-system — G-ADAPTER engine layout descriptor
            engine_layout,
            // viewer
            viewer_serve,
            viewer_stop,
            viewer_port,
            // secrets
            secrets_get,
            secrets_set,
            secrets_delete,
            secrets_list_keys,
            secrets_vault_status,
            // secrets — Phase 7 scoped variants
            secrets_get_scoped,
            secrets_set_scoped,
            secrets_delete_scoped,
            secrets_list_keys_scoped,
            // settings_kv (durable mirror for Zustand-backed prefs)
            settings_get,
            settings_set,
            settings_get_all,
            settings_clear_all,
            // projects (phase 0 of projects-first-class plan)
            project_create,
            project_update,
            project_list,
            project_archive,
            project_set_active,
            project_get_active,
            project_inventory,
            project_skills_list,
            project_scaffold_claude,
            project_artifacts_walk,
            // atelier skill files (WP-16b / WP-10) — generic reader for
            // <project_root>/.atelier/<skill>/<file>; the Tasks roster read is one caller.
            atelier_file_read,
            // atelier instance write path (WP-18b) — the setup-chat confirm-write
            // persists <project_root>/.atelier/<skill>/manifest.json through here.
            atelier_file_write,
            // supabase config (URL + anon key manifest)
            supabase_config_get,
            supabase_config_set,
            supabase_config_clear,
            // trust gating (Phase 9)
            pkg_trust_list,
            pkg_trust_preview,
            pkg_trust_grant,
            pkg_trust_revoke,
            // trust-review modal (2026-05-15) — capability-diff batch surface
            pkg_trust_list_pending,
            pkg_trust_approve,
            pkg_trust_reject,
            // per-folder Studio project-access gate (WP-04)
            pkg_studio_request_project_access,
            // runtime-ACL violations audit (2026-05-15)
            pkg_permission_violations_list,
            pkg_permission_violations_clear,
            // approve-gate run-then-pause seam (pa_action_drafts, WP-3)
            pa_actions_pause,
            pa_actions_list,
            pa_actions_update,
            pa_actions_commit,
            pa_actions_reject,
            pa_actions_retry,
            // db
            db_query,
            db_exec,
            // data health (orphan audit)
            data_health_scan,
            // iyke
            iyke_endpoint,
            iyke_set_shell,
            iyke_log_push,
            iyke_network_push,
            iyke_dom_done,
            iyke_dom_query,
            iyke_query_cache_done,
            iyke_wait_done,
            iyke_terminal_read_done,
            iyke_terminal_spawn_done,
            iyke_action_done,
            iyke::browser_handlers::iyke_browser_reply,
            // backup / restore
            backup_export,
            backup_import,
            backup_list,
            backup_delete,
            db_export_ndjson,
            db_import_ndjson,
            // desktop
            set_dock_badge,
            iyke_mcp_info,
            // screenshots
            screenshot_window,
            screenshot_pane,
            screenshot_capture_done,
            screenshot_capture_failed,
            screenshot_capture_native_crop,
            screenshot_get_config,
            screenshot_set_dir,
            // spike: dynamic ACL verification (delete after kernel lands)
            spike_grant_fs_read,
            spike_setup_test_file,
            // pkg-browser child webviews
            pkg_webview_create,
            pkg_webview_destroy,
            pkg_webview_navigate,
            pkg_webview_set_rect,
            // Phase 0.5 bg-execution spike. Debug builds only.
            #[cfg(debug_assertions)]
            bg_spike_run,
            #[cfg(debug_assertions)]
            bg_spike_reply,
            // pkg kernel
            pkg_install_from_path,
            pkg_install_from_registry,
            pkg_uninstall,
            pkg_set_enabled,
            pkg_set_scope,
            pkg_kernel_status,
            list_skill_actions,
            list_all_skill_actions,
            pkg_discover_workspace,
            pkg_db_diag,
            pkg_health_scan,
            pkg_health_remove,
            pkg_health_remove_all,
            pkg_settings_get,
            pkg_settings_set,
            pkg_preview_manifest,
            pkg_screenshot,
            pkg_content_url,
            pkg_content_html,
            pkg_content_revoke,
            pkg_is_trusted_for_elevated,
            // trusted-pkg elevated verbs (ADR-017, WP-04/05)
            pkg_fetch,
            pkg_invoke,
            pkg_mcp_call,
            pkg_sidecar_call,
            pkg_sidecar_rpc_send,
            pkg_sidecar_rpc_shutdown,
            pkg_supervisor_restart,
            pkg_dev_register,
            pkg_dev_unregister,
            pkg_dev_reload,
            runtime_retry_bun_fetch,
            dev_bind_port,
            dev_release_port,
            // first-run wizard detection
            agent_detect::detect_system,
            agent_detect::detect_agents,
            agent_detect::detect_agent,
            agent_detect::detect_agent_config,
            agent_detect::list_claude_projects,
            agent_detect::list_agent_projects,
            agent_detect::scaffold_agent_config,
            // activity bar pinning
            activity_pins_list,
            activity_pins_add,
            activity_pins_remove,
            activity_pins_reorder,
            activity_pins_resolve_artifact,
            activity_pins_touch_open,
            activity_sections_list,
            activity_sections_create,
            activity_sections_update,
            activity_sections_remove,
            // artifact-grid pin comments
            comment_create,
            comment_get,
            comment_list,
            comment_record_routing,
            comment_set_status,
            comment_delete,
            comment_route,
            pin_screenshot_write,
            // artifact-studio chat threads (one per folder, D3)
            studio_thread_get_or_create,
            studio_thread_get,
            studio_thread_list_recent,
            studio_thread_delete,
            studio_message_append,
            studio_message_list,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Phase 14: best-effort cleanup of the runtime env-vault file
            // when the app is shutting down. Not critical (the file lives
            // in $XDG_RUNTIME_DIR / $TMPDIR, both per-user-volatile), but
            // keeps the surface tidy.
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                commands::secrets::cleanup_runtime_file();
            }
        });
}

/// Set up tracing → stderr + a rolling file in the platform log dir. Best
/// effort; if the dir is unavailable we just log to stderr.
fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let log_dir = log_dir();
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    if let Some(dir) = log_dir {
        let _ = std::fs::create_dir_all(&dir);
        let appender = tracing_appender::rolling::daily(&dir, "ikenga.log");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(appender);
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .with(file_layer)
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer)
            .init();
    }
}

/// Build the global-shortcut plugin. The handler dispatches by shortcut:
/// the summon binding toggles window visibility, the screenshot bindings
/// emit `screenshot://shortcut` events that the FE picks up and routes
/// back through `screenshot_window` / `screenshot_pane`. Doing the
/// focused-pane resolution in the FE avoids mirroring `usePaneStore` on
/// the Rust side just for one handler.
fn global_shortcut_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_global_shortcut::{Builder, Code, Modifiers, Shortcut, ShortcutState};

    let summon = if cfg!(target_os = "macos") {
        Shortcut::new(Some(Modifiers::ALT), Code::Space)
    } else {
        Shortcut::new(Some(Modifiers::SUPER), Code::Space)
    };
    let shot_window = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyS,
    );
    let shot_pane = Shortcut::new(
        Some(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT),
        Code::KeyP,
    );

    Builder::new()
        .with_handler(move |app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            if shortcut == &summon {
                // Summon always targets the PRIMARY window — "bring Ikenga to
                // the front" means the main window, not whatever is focused
                // (multi-window: intentionally stays "main").
                if let Some(window) = app.get_webview_window("main") {
                    let visible = window.is_visible().unwrap_or(false);
                    if visible && window.is_focused().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.unminimize();
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            } else if shortcut == &shot_window {
                // Target the focused window that actually hosts the screenshot
                // listener (`useScreenshotListener`, mounted only inside
                // `<Workspace/>`). `focused_listener_window_label` returns a
                // focused `Workspace`-kind spawned window (Flavor B) if any,
                // else `None` — deliberately never a `single-surface`/`pane-set`
                // detached window or a pkg-pane child webview, which have no
                // listener. `None` → "main", exactly today's behavior; on
                // WebKitGTK `is_focused` can under-report, which also falls back
                // to "main" (safe). Detached-window capture also needs the
                // listener + `capture_window_png` de-"main"'d before it lights
                // up in practice.
                let target = crate::window::focused_listener_window_label(app)
                    .unwrap_or_else(|| "main".to_string());
                let _ = crate::window::emit_to_label(
                    app,
                    &target,
                    "screenshot://shortcut",
                    serde_json::json!({ "kind": "window" }),
                );
            } else if shortcut == &shot_pane {
                let target = crate::window::focused_listener_window_label(app)
                    .unwrap_or_else(|| "main".to_string());
                let _ = crate::window::emit_to_label(
                    app,
                    &target,
                    "screenshot://shortcut",
                    serde_json::json!({ "kind": "pane-focused" }),
                );
            }
        })
        .build()
}

/// ⌥Space on Mac, Super+Space on Linux. Toggle main window visibility.
fn register_summon_shortcut(
    app: &tauri::AppHandle,
) -> Result<(), tauri_plugin_global_shortcut::Error> {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

    let modifiers = if cfg!(target_os = "macos") {
        Modifiers::ALT
    } else {
        Modifiers::SUPER
    };
    let shortcut = Shortcut::new(Some(modifiers), Code::Space);
    app.global_shortcut().register(shortcut)?;
    Ok(())
}

/// Ctrl+Alt+Shift+S = window screenshot, Ctrl+Alt+Shift+P = focused-pane
/// screenshot. The plugin handler dispatches to the correct branch by
/// matching the `Shortcut` value. Tolerant: each binding is registered
/// individually so a clash on one doesn't kill the other.
fn register_screenshot_shortcuts(app: &tauri::AppHandle) {
    use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};

    let mods = Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
    for (sc, label) in [
        (Shortcut::new(Some(mods), Code::KeyS), "screenshot:window"),
        (Shortcut::new(Some(mods), Code::KeyP), "screenshot:pane"),
    ] {
        if let Err(e) = app.global_shortcut().register(sc) {
            // `log::` macros are dropped in this crate (no log→tracing
            // bridge); use tracing so the warning actually emits.
            tracing::warn!("{label} shortcut not registered (continuing): {e}");
        }
    }
}

// ─── CLI screenshot dispatch ──────────────────────────────────────────────────
//
// `ikenga-desktop --screenshot=window` / `--screenshot=pane:<id>` is
// intercepted at the very top of `run()`. We never start a Tauri instance
// for a CLI invocation — instead we read the running app's `control.json`,
// POST to its iyke server, print the result, and exit. If the app isn't
// running there's no daemon mode to fall through to: print an error and
// exit 1.

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenshotCli {
    Window,
    Pane(String),
}

fn parse_screenshot_arg<I: Iterator<Item = String>>(args: I) -> Option<ScreenshotCli> {
    for a in args.skip(1) {
        if let Some(rest) = a.strip_prefix("--screenshot=") {
            return parse_screenshot_value(rest);
        }
    }
    None
}

fn parse_screenshot_value(v: &str) -> Option<ScreenshotCli> {
    if v == "window" {
        Some(ScreenshotCli::Window)
    } else if let Some(id) = v.strip_prefix("pane:") {
        if id.is_empty() {
            None
        } else {
            Some(ScreenshotCli::Pane(id.to_string()))
        }
    } else {
        None
    }
}

#[derive(serde::Deserialize)]
struct ControlFileRead {
    port: u16,
    token: String,
}

fn screenshot_cli_control_path() -> Option<std::path::PathBuf> {
    // Identifier must match `tauri.conf.json:identifier` so the running
    // shell and the --screenshot CLI path agree on where control.json
    // lives. Pre-strip this hardcoded `io.royalti.pa.desktop`, which had
    // drifted from the real bundle id `app.ikenga` and broke the CLI on
    // any clean install.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Application Support/app.ikenga/control.json"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Some(home.join(".local/share/app.ikenga/control.json"))
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("app.ikenga").join("control.json"))
    }
}

fn run_screenshot_cli(cmd: ScreenshotCli) -> i32 {
    let Some(control_path) = screenshot_cli_control_path() else {
        eprintln!("error: could not resolve control.json path");
        return 1;
    };
    if !control_path.exists() {
        eprintln!(
            "error: app not running ({} not found)",
            control_path.display()
        );
        return 1;
    }
    let json = match std::fs::read(&control_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: read {}: {e}", control_path.display());
            return 1;
        }
    };
    let cf: ControlFileRead = match serde_json::from_slice(&json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: parse control.json: {e}");
            return 1;
        }
    };

    let (path, body) = match &cmd {
        ScreenshotCli::Window => ("/iyke/screenshot/window", serde_json::json!({})),
        ScreenshotCli::Pane(id) => (
            "/iyke/screenshot/pane",
            serde_json::json!({ "pane_id": id }),
        ),
    };
    let url = format!("http://127.0.0.1:{}{}", cf.port, path);

    // ureq is blocking — fine here because we run before the Tokio runtime
    // exists and exit immediately after.
    let req = ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", cf.token))
        .set("Content-Type", "application/json")
        // A pane FE-clone can take >15s on a heavy artifact; keep this
        // comfortably above the in-app CAPTURE_TIMEOUT (60s) so the CLI
        // reports the real result instead of a false client-side timeout.
        .timeout(std::time::Duration::from_secs(70));
    match req.send_json(body) {
        Ok(resp) => {
            let body = resp.into_string().unwrap_or_default();
            println!("{body}");
            0
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            eprintln!("error: {code} {body}");
            1
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn parses_window() {
        let args = ["ikenga-desktop", "--screenshot=window"]
            .into_iter()
            .map(String::from);
        assert_eq!(parse_screenshot_arg(args), Some(ScreenshotCli::Window));
    }

    #[test]
    fn parses_pane() {
        let args = ["ikenga-desktop", "--screenshot=pane:abc-123"]
            .into_iter()
            .map(String::from);
        assert_eq!(
            parse_screenshot_arg(args),
            Some(ScreenshotCli::Pane("abc-123".into()))
        );
    }

    #[test]
    fn rejects_empty_pane_id() {
        assert_eq!(parse_screenshot_value("pane:"), None);
    }

    #[test]
    fn ignores_other_args() {
        let args = ["bin", "--other", "value"].into_iter().map(String::from);
        assert_eq!(parse_screenshot_arg(args), None);
    }
}

fn log_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let home = std::path::PathBuf::from(home);
    #[cfg(target_os = "macos")]
    {
        Some(home.join("Library/Logs/Ikenga"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Some(home.join(".local/share/ikenga/logs"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Ikenga").join("logs"))
    }
}
