//! # Two binaries, one crate
//!
//! `ikenga-desktop` (default features) is the Tauri application.
//! `ikenga-server` is the headless daemon, built with `--no-default-features`,
//! which drops `tauri/wry` and with it the whole GTK + WebKit stack. That is
//! not a size optimisation: with wry linked the daemon will not START on a
//! server, because the dynamic linker demands `libwebkit2gtk-4.1` and five
//! friends that no headless box has.
//!
//! The module list below is in three groups. The first, the headless core,
//! is every module the daemon compiles — deliberately short. The second
//! is a set of thin desktop facades over code that already lives in the
//! ungated `server::shared`. The third is desktop-only — built around
//! `#[tauri::command]` and `AppHandle`, neither of which exists without a
//! webview runtime (`AppHandle`'s default type parameter IS `Wry`).
//!
//! The Tauri command surface itself is registered in `commands/registry.rs`
//! (the one `generate_handler!` list, which both parity gates parse); `run()`
//! below only installs it.

// --- Headless core: compiled into BOTH binaries ---
// G-ACCESS (WP-74a): caps, principals/AccessCtx, the access store, devices,
// the audit chain and the Part B arm skeleton. Opened only by the daemon
// (T0) or the broker (T1) — never by the desktop (P-20, A-32).
pub mod access;
// `db` holds PaDb + the embedded migration set; `commands::db` keeps only the
// two #[tauri::command] wrappers and re-exports PaDb.
pub mod db;
pub mod engines;
// `SessionExecutor` + tier probe (ADR-023, WP-18). Headless on purpose: the
// daemon installs its executor at boot and every in-scope spawn goes through it.
pub mod executor;
mod fs_roots;
// The watcher pool is sink-driven (`fs_watch::FsEventSink`), so nothing in
// it needs an `AppHandle`. The desktop emit-backed sink is the one piece
// gated inside the module; the daemon's `/ws/fs` supplies its own.
pub mod fs_watch;
pub mod path_allow;
pub mod path_fix;
// Only `pkg::manifest` + `pkg::registry` are headless — they are pure serde
// and have never touched tauri. Everything else in `pkg/` (kernel, lifecycle,
// webview, trust, …) is gated inside `pkg/mod.rs`. The daemon reads manifests
// to serve `/pkgs/:id/*` read-only; it installs nothing.
pub mod pkg;
// Pure HTML rewrites (subresource inlining + `<base href>`) shared by the
// desktop `pkg_content` srcdoc path and the daemon's `/pkgs` mount path. Same
// code, two different reasons to need it — see the module docs.
pub mod pkg_html;
pub mod platform;
pub mod pty;
mod runtime;
// The secrets substrate compiles into both binaries: the desktop's keychain
// backend stays desktop-only inside it (ADR-022), and the daemon gets the
// per-principal store (remote-access WP-21, `secrets::principal_store`).
pub mod secrets;
pub mod secrets_env;
pub mod server;
// Transcript usage mirror (`transcript::usage`) compiles into both binaries:
// the shared Ngwa snapshot join (`server::shared::ngwa`) names its types. The
// live-session watcher, which emits on a Tauri event channel, stays
// desktop-only inside the module.
pub mod transcript;

// --- Desktop facades over the headless `server::shared` substrate ---
// The implementations already compile into both binaries (WP-19 slices
// 5a/5b); these modules only keep the desktop's historical `crate::…` paths
// and add the `AppHandle`-backed pieces (the `settings://changed` /
// `actions://changed` emitting constructors, the OS opener behind
// `actions_open_file`, the `claude_config_resolve_cascade` command).
// TODO(WP-19 PR B): ungate or fold these into `server::shared` call sites —
// out of scope for the registry move (part A), which changes no behaviour.
// WP-50: actions.json / keybindings.json file layer + project-trust record.
#[cfg(feature = "desktop")]
pub mod actions;
#[cfg(feature = "desktop")]
pub mod settings;
#[cfg(feature = "desktop")]
pub mod settings_cascade;
#[cfg(feature = "desktop")]
mod terminal;

// --- Desktop-only ---
#[cfg(feature = "desktop")]
mod agent_detect;
#[cfg(feature = "desktop")]
pub mod claude;
#[cfg(feature = "desktop")]
pub mod commands;
#[cfg(feature = "desktop")]
pub mod env_files;
#[cfg(feature = "desktop")]
mod iyke;
// WP-40: the `notifications` aggregation table, its producers, mute prefs and
// the `notifications://changed` forwarder.
#[cfg(feature = "desktop")]
pub mod notifications;
#[cfg(feature = "desktop")]
mod pkg_content;
#[cfg(feature = "desktop")]
mod viewer_server;
// Multi-window substrate (plans/multi-window): the G-WINDOW-MODEL contract
// (WP-02) + the window registry / spawn-close-list commands (WP-03).
#[cfg(feature = "desktop")]
mod window;
// WP-37: `#[ignore]`d Phase 5a migration rehearsal against a copy of app data.
#[cfg(all(test, feature = "desktop"))]
mod rehearsal_5a;

#[cfg(feature = "desktop")]
use std::sync::Arc;

#[cfg(feature = "desktop")]
use tauri::Manager;
// tauri-plugin-sql is loaded as a plugin (below) so the frontend's
// `@tauri-apps/plugin-sql` callers resolve, but it does NOT own the
// migration list — that lives in `commands::db::ensure_schema`.
#[cfg(feature = "desktop")]
use tokio::sync::Mutex;

#[cfg(feature = "desktop")]
use commands::db::PaDb;
#[cfg(feature = "desktop")]
#[cfg(debug_assertions)]
use commands::new_bg_spike_state;
#[cfg(feature = "desktop")]
use commands::screenshot::new_pending as new_screenshot_pending;
// Managed-state types only. The `#[tauri::command]` functions are named by
// `commands/registry.rs`, which holds the one `generate_handler!` list.
#[cfg(feature = "desktop")]
use commands::{
    AppLockState, ChiCache, ChiRuntime, IykeRuntimeState, KernelState, PkgContentState,
    PkgSettingsState, ScreenshotConfigState, ScreenshotConfigStateRef, ScreenshotPending,
    SecretsLock, SidecarSupervisorState, SidecarsRegistryState, StreamingSidecarManager,
    StreamingSidecarManagerState, WebviewPanesState,
};
#[cfg(feature = "desktop")]
use fs_watch::FsWatchManager;
#[cfg(feature = "desktop")]
use iyke::{IykeRpc, IykeState};
#[cfg(feature = "desktop")]
use pty::PtyManager;
#[cfg(feature = "desktop")]
use viewer_server::ViewerServerManager;

#[cfg(feature = "desktop")]
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
    let antigravity_engine: engines::antigravity_acp::AntigravityEngineState =
        Arc::new(engines::antigravity_acp::AntigravityEngine::new());
    let cursor_agent_engine: engines::cursor_agent::CursorAgentEngineState =
        Arc::new(engines::cursor_agent::CursorAgentEngine::new());
    let opencode_engine: engines::opencode_acp::OpencodeEngineState =
        Arc::new(engines::opencode_acp::OpencodeEngine::new());
    let pi_engine: engines::pi_acp::PiEngineState = Arc::new(engines::pi_acp::PiEngine::new());
    // WP-20: the OpenRouter HTTP engine. No CLI to spawn and no binary to
    // detect — it resolves `OPENROUTER_API_KEY` from Stronghold at prompt time
    // (see `engines/openrouter_http/mod.rs`), so it is constructed
    // unconditionally and simply fails the turn with a "key is not set"
    // message until the user stores one.
    let openrouter_engine: engines::openrouter_http::OpenRouterHttpEngineState =
        Arc::new(engines::openrouter_http::OpenRouterHttpEngine::new());
    // The engine needs an `AppHandle` for vault + `pkg_settings` reads, but it
    // is constructed before the app exists and its `run_prompt` keeps the same
    // `AppHandle`-free signature as every sibling adapter. Handed over in
    // `setup` below; a turn that somehow runs before that fails with a clear
    // message rather than panicking.
    let openrouter_for_setup = openrouter_engine.clone();
    // Multi-engine dispatcher used by `commands/chat.rs`. Built once
    // here and `.manage()`d so every Tauri command resolves engines
    // through the same registry.
    let engine_registry: engines::EngineRegistryState = Arc::new(engines::EngineRegistry::new());
    {
        let reg = engine_registry.clone();
        let antigravity_handle = engines::EngineHandle::Antigravity(antigravity_engine.clone());
        let claude_handle = engines::EngineHandle::ClaudeCode(claude_code_engine.clone());
        let codex_handle = engines::EngineHandle::CodexPty(codex_pty_engine.clone());
        let cursor_agent_handle = engines::EngineHandle::CursorAgent(cursor_agent_engine.clone());
        let opencode_handle = engines::EngineHandle::Opencode(opencode_engine.clone());
        let pi_handle = engines::EngineHandle::Pi(pi_engine.clone());
        let openrouter_handle = engines::EngineHandle::OpenRouterHttp(openrouter_engine.clone());
        tauri::async_runtime::block_on(async move {
            reg.insert("antigravity", antigravity_handle.clone()).await;
            reg.insert("antigravity-cli", antigravity_handle).await;
            reg.insert("claude-code", claude_handle).await;
            reg.insert("codex", codex_handle).await;
            reg.insert("cursor-agent", cursor_agent_handle).await;
            reg.insert("opencode", opencode_handle).await;
            reg.insert("pi", pi_handle).await;
            // Keyed by the pkg manifest's `engine.agentId`.
            reg.insert(
                engines::openrouter_http::server::OPENROUTER_ENGINE_ID,
                openrouter_handle,
            )
            .await;
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
        .plugin(commands::os_shortcuts::global_shortcut_plugin())
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
        .manage(pty_manager.clone())
        .manage(fs_watch_manager)
        .manage(viewer_manager)
        .manage(sessions_manager)
        .manage(claude_code_engine)
        .manage(codex_pty_engine)
        .manage(cursor_agent_engine)
        .manage(opencode_engine)
        .manage(pi_engine)
        .manage(openrouter_engine)
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

            // WP-20: give the OpenRouter engine the handle it resolves its
            // vault key + `pkg_settings` through.
            openrouter_for_setup.attach_app(app.handle().clone());

            // Ensure app data dir exists; SQLite + Stronghold both write here.
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|e| format!("app_data_dir: {e}"))?;
            std::fs::create_dir_all(&data_dir)?;

            // WP-72: app lock. Rust owns the lock so a webview reload can't
            // clear it and every window shows the same state. Loads
            // `app-lock.json`; starts locked when idle lock is on (see
            // `commands/app_lock.rs`). The ticker locks on idle.
            let app_lock = AppLockState::new();
            app_lock.configure(data_dir.join(commands::app_lock::CONFIG_FILENAME));
            app.manage(app_lock);
            commands::app_lock::spawn_idle_ticker(app.handle().clone());

            let secrets_ready = match app.state::<SecretsLock>().configure_data_dir(&data_dir) {
                Ok(()) => true,
                Err(error) => {
                    log::error!("[secrets] unlock state configuration failed: {error}");
                    if let Err(mark_error) = app
                        .state::<SecretsLock>()
                        .mark_unavailable(error)
                    {
                        log::error!("[secrets] unavailable state failed: {mark_error}");
                    }
                    false
                }
            };

            if secrets_ready {
                match secrets::migrate::run(&data_dir) {
                    Ok(_) => {}
                    Err(error) => {
                        if let Err(mark_error) = app
                            .state::<SecretsLock>()
                            .mark_unavailable(error.clone())
                        {
                            log::error!("[secrets] unavailable state failed: {mark_error}");
                        }
                        if let Err(invalidation_error) =
                            commands::secrets::invalidate_env_vaults(app.handle())
                        {
                            log::error!(
                                "[secrets] env-vault invalidation after migration failure failed: {invalidation_error}"
                            );
                        }
                        log::error!("[secrets] migration failed: {error}");
                    }
                }
            } else if let Err(invalidation_error) =
                commands::secrets::invalidate_env_vaults(app.handle())
            {
                log::error!(
                    "[secrets] env-vault invalidation after unlock setup failure failed: {invalidation_error}"
                );
            }

            let idle_lock = app.state::<SecretsLock>().inner().clone();
            let idle_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if idle_lock.expire_if_idle() {
                        if let Err(error) = commands::secrets::invalidate_env_vaults(&idle_app) {
                            log::warn!("[secrets] idle env-vault invalidation failed: {error}");
                        }
                    }
                }
            });

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

            let settings_manager = Arc::new(settings::SettingsManager::new(
                app.handle().clone(),
                pa_db.clone(),
                data_dir.clone(),
            ));
            if let Err(e) = tauri::async_runtime::block_on(settings_manager.initialize()) {
                tracing::warn!("[settings] initialization failed: {e}");
            }
            app.manage(settings_manager.clone());
            // WP-40: relay notification changes to the webview as
            // `notifications://changed` (muted flag stamped from settings).
            notifications::spawn_event_forwarder(app.handle().clone());
            {
                use tauri::Listener;
                let app_for_settings = app.handle().clone();
                let manager_for_settings = settings_manager.clone();
                app_for_settings.listen("projects:active-changed", move |_evt| {
                    let manager = manager_for_settings.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = manager.refresh_watch().await {
                            tracing::warn!("[settings] project watcher refresh failed: {e}");
                        }
                    });
                });
            }

            // WP-50: actions.json / keybindings.json watchers (personal +
            // active project, `actions://changed`) and the trust record.
            let actions_manager = Arc::new(actions::ActionsManager::new(
                app.handle().clone(),
                pa_db.clone(),
                data_dir.clone(),
            ));
            if let Err(e) = tauri::async_runtime::block_on(actions_manager.refresh_watch()) {
                tracing::warn!("[actions] watcher initialization failed: {e}");
            }
            app.manage(actions_manager.clone());
            {
                use tauri::Listener;
                let app_for_actions = app.handle().clone();
                app_for_actions.listen("projects:active-changed", move |_evt| {
                    let manager = actions_manager.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = manager.refresh_watch().await {
                            tracing::warn!("[actions] project watcher refresh failed: {e}");
                        }
                    });
                });
            }

            // WP-01 (chi-first agent surface): cache directory + metadata cache.
            app.manage(ChiCache::new(data_dir.clone()));

            // WP-02: runtime state for live Chi children.
            app.manage(Arc::new(ChiRuntime::new()));

            // Wave 2: Desktop terminal daemon proxy & discovery
            let daemon_info = pty::daemon_client::init_daemon(Some(data_dir.clone()));
            let daemon_state = Arc::new(pty::daemon_client::DaemonState::new(daemon_info));
            app.manage(daemon_state);

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
            app.manage(screenshot_cfg.clone());
            if let Err(e) = tauri::async_runtime::block_on(
                commands::screenshot::sync_from_settings(&settings_manager, &screenshot_cfg),
            ) {
                tracing::warn!("[settings] screenshot state sync failed: {e:#}");
            }
            commands::screenshot::install_settings_listener(
                app.handle(),
                settings_manager.clone(),
                screenshot_cfg,
            );

            log::info!("ikenga app data dir: {}", data_dir.display());

            // OS-wide shortcuts (G-ACTIONS §6): the default `os.*` rules now;
            // the webview replaces them with the effective set on load.
            commands::os_shortcuts::register_default_os_shortcuts(app.handle());

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
            // Manifest v5 (G-MANIFEST-V5 §2, WP-28): contribution-block
            // registries. Pure in-memory record registries — snapshots flow
            // through `pkg_kernel_status` for the Phase-1 slots (Explorer
            // Views section, rail pins) and the Phase 5/6 consumers.
            let views_reg = Arc::new(pkg::registries::ViewsRegistry::new());
            let explorer_sections_reg =
                Arc::new(pkg::registries::ExplorerSectionsRegistry::new());
            let companion_panels_reg =
                Arc::new(pkg::registries::CompanionPanelsRegistry::new());
            let context_actions_reg =
                Arc::new(pkg::registries::ContextActionsRegistry::new());
            let widgets_reg = Arc::new(pkg::registries::WidgetsRegistry::new());
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
            // Linux/Windows: the handler stays ENABLED. It is the ONLY source of
            // a dropped file's absolute path (WebKitGTK blanks `dataTransfer`
            // for security when the handler is off, so HTML5 yields empty
            // `files`/`getData`); `onDragDropEvent` paths are routed to the
            // surface under the cursor (see `src/lib/dnd/os-file-drop.ts`) and
            // the composer drop is bridged there.
            //
            // The catch: on Windows, WebView2 delivers NO HTML5 drag events to
            // the page while this handler is on (Tauri: disabling it "is
            // required to use HTML5 drag and drop APIs on the frontend on
            // Windows"). So in-page drags — pane/dock tab move, split and
            // reorder, pin reordering — don't use HTML5 DnD at all; they run
            // on pointer events (`src/lib/panes/pointer-drag.ts`), which no
            // native handler intercepts. Don't add new HTML5 `draggable`
            // surfaces: they silently won't work on Windows.
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
                    activity_bar_reg.clone() as Arc<dyn pkg::Registry>,
                    views_reg as Arc<dyn pkg::Registry>,
                    explorer_sections_reg as Arc<dyn pkg::Registry>,
                    companion_panels_reg as Arc<dyn pkg::Registry>,
                    context_actions_reg as Arc<dyn pkg::Registry>,
                    widgets_reg as Arc<dyn pkg::Registry>,
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
            // WP-40: resolve `update` notifications whose version is now
            // installed (an app update relaunches into this).
            commands::notifications::spawn_boot_update_sweep(app.handle().clone());
            app.manage(PkgSettingsState(settings_reg));
            app.manage(crate::commands::ActivityBarState(activity_bar_reg.clone()));
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
                        let active_for_reconcile = active.clone();
                        // Registries call `block_on` inside register/unregister, so
                        // the reconcile must not run on this tokio worker (#130).
                        match tauri::async_runtime::spawn_blocking(move || {
                            kernel.reconcile_for_project(&active_for_reconcile)
                        })
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => log::warn!(
                                "[pkg_kernel] reconcile for active=`{active}` failed: {e:#}"
                            ),
                            Err(join) => log::error!(
                                "[pkg_kernel] reconcile for active=`{active}` panicked: {join}"
                            ),
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
            // to ~/.config/ikenga-actions/env (%LOCALAPPDATA%\ikenga-actions\env
            // on Windows) or ikenga/.env.
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
        // The command list lives in `commands/registry.rs` (WP-19 final slice A).
        .invoke_handler(commands::registry::handler())
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Phase 14: best-effort cleanup of the runtime env-vault file
            // when the app is shutting down. Not critical (the file lives
            // in $XDG_RUNTIME_DIR / $TMPDIR, both per-user-volatile, or the
            // per-user %LOCALAPPDATA% on Windows), but keeps the surface tidy.
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                // WP-34: with a passphrase configured, the env-vault files
                // (including the durable one) are plaintext only while
                // unlocked — overwrite them and drop the DEK on exit, the
                // same invalidation an explicit lock performs. No-op without
                // a passphrase (WP-33 durable-file contract).
                commands::secrets::wipe_env_vaults_on_exit(_app);
                commands::secrets::cleanup_runtime_file();
                #[cfg(feature = "desktop")]
                {
                    use tauri::Manager;
                    if let Some(daemon_state) = _app.try_state::<Arc<pty::daemon_client::DaemonState>>() {
                        daemon_state.shutdown();
                    }
                    if let Some(state) = _app.try_state::<commands::pkg_webview::WebviewPanesState>() {
                        state.0.cleanup_clear_on_exit(_app);
                    }
                }
            }
        });
}

/// Set up tracing → stderr + a rolling file in the platform log dir. Best
/// effort; if the dir is unavailable we just log to stderr.
#[cfg(feature = "desktop")]
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

// ─── CLI screenshot dispatch ──────────────────────────────────────────────────
//
// `ikenga-desktop --screenshot=window` / `--screenshot=pane:<id>` is
// intercepted at the very top of `run()`. We never start a Tauri instance
// for a CLI invocation — instead we read the running app's `control.json`,
// POST to its iyke server, print the result, and exit. If the app isn't
// running there's no daemon mode to fall through to: print an error and
// exit 1.

#[cfg(feature = "desktop")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ScreenshotCli {
    Window,
    Pane(String),
}

#[cfg(feature = "desktop")]
fn parse_screenshot_arg<I: Iterator<Item = String>>(args: I) -> Option<ScreenshotCli> {
    for a in args.skip(1) {
        if let Some(rest) = a.strip_prefix("--screenshot=") {
            return parse_screenshot_value(rest);
        }
    }
    None
}

#[cfg(feature = "desktop")]
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

#[cfg(feature = "desktop")]
#[derive(serde::Deserialize)]
struct ControlFileRead {
    port: u16,
    token: String,
}

#[cfg(feature = "desktop")]
fn screenshot_cli_control_path() -> Option<std::path::PathBuf> {
    // Identifier must match `tauri.conf.json:identifier` so the running
    // shell and the --screenshot CLI path agree on where control.json
    // lives. Pre-strip this hardcoded `io.royalti.pa.desktop`, which had
    // drifted from the real bundle id `app.ikenga` and broke the CLI on
    // any clean install.
    // Resolve $HOME lazily — the Windows branch below doesn't need it, and
    // `std::env::var_os("HOME")` is unset there, which used to make this
    // whole function return `None` (an early-return before the Windows
    // branch even ran). `log_dir` has the same-shaped bug but is owned by
    // PR #201 (fix/windows-terminal-clipboard-links-daemon).
    #[cfg(target_os = "macos")]
    {
        let home = crate::platform::home_dir()?;
        Some(home.join("Library/Application Support/app.ikenga/control.json"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = crate::platform::home_dir()?;
        Some(home.join(".local/share/app.ikenga/control.json"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("app.ikenga").join("control.json"))
    }
}

#[cfg(feature = "desktop")]
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

#[cfg(feature = "desktop")]
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

#[cfg(feature = "desktop")]
fn log_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
        Some(home.join("Library/Logs/Ikenga"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
        Some(home.join(".local/share/ikenga/logs"))
    }
    // No HOME lookup here: Windows doesn't set HOME for a normal GUI launch, and
    // an early `var_os("HOME")?` used to return None before reaching this branch,
    // so the installed app wrote no log file at all.
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|p| p.join("Ikenga").join("logs"))
    }
}
