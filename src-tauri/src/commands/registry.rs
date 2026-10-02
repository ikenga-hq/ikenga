//! The desktop's Tauri command registry — the ONE `generate_handler!` list.
//!
//! Moved out of `lib.rs` in the WP-19 final slice (part A) so the command
//! surface lives in one file that later WPs edit on disjoint lines: each
//! domain has its own `// ── <domain>` section, and the remote-access
//! Part B sections (devices / access / permission routing / audit) are
//! pre-stubbed at the bottom.
//!
//! Every entry is written with the module that DEFINES the command
//! (`pty::pty_spawn`, `crate::iyke::seats::seats_list`), not a re-export:
//! `#[tauri::command]` emits its `__cmd__<name>` wrapper macro next to the
//! function (and `#[macro_export]`s it to the crate root, which is the only
//! reason bare names resolved while this list lived in `lib.rs`). Naming the
//! defining module makes the wrapper resolve from here.
//!
//! Two gates parse this file as text (one entry per line, `path::to::cmd,`;
//! `#[cfg(...)]` lines and `//` comments are skipped):
//!
//! * `scripts/check-acl-parity.ts` (`bun run test:acl-parity`) — this list
//!   must equal `allow-app-commands` in `permissions/app-commands.toml`;
//! * `server/parity.rs` (a `cargo test`, both feature sets) — every entry is
//!   either served by the daemon's `rpc.rs` or allowlisted in
//!   `server/desktop_only.toml`.
//!
//! Both also assert there is exactly one handler list across `lib.rs` and
//! this file, so a second `generate_handler!` cannot silently bypass them.
//! Adding a command: one line here, one in `app-commands.toml`, and an
//! `rpc.rs` arm or a `desktop_only.toml` table.

use super::*;

/// The full Tauri invoke handler, handed to `Builder::invoke_handler` in
/// `lib.rs`.
pub(crate) fn handler() -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static {
    tauri::generate_handler![
        // ── agents: agent-ops, sessions, chi, seats ─────────────────────────
        // agent-ops host bridge (WP-09 / G-TRIGGER + WP-14 CRUD)
        agent_ops::agent_ops_run_now,
        agent_ops::agent_ops_set_enabled,
        agent_ops::agent_ops_list_jobs,
        agent_ops::agent_ops_upsert_job,
        agent_ops::agent_ops_delete_job,
        agent_ops::agent_ops_tail_run,
        // claude sessions
        claude::claude_list_sessions,
        claude::claude_read_jsonl,
        claude::session_ensure,
        claude::session_send,
        claude::session_tool_result,
        claude::session_cancel,
        claude::session_destroy,
        claude::session_destroy_all,
        // chi-first agent surface (WP-01)
        chi::chi_run,
        chi::chi_resume,
        chi::chi_status,
        chi::chi_list,
        chi::chi_cancel,
        // Chi seats (G-SEATS §9.2, WP-65) — `src-tauri/src/iyke/seats.rs`.
        crate::iyke::seats::seats_list,
        crate::iyke::seats::seats_get,
        crate::iyke::seats::seats_engines,
        crate::iyke::seats::seats_resolve,
        crate::iyke::seats::seats_create,
        crate::iyke::seats::seats_move,
        crate::iyke::seats::seats_resume,
        crate::iyke::seats::seats_fill,
        crate::iyke::seats::seats_queue,
        crate::iyke::seats::seats_clear,
        crate::iyke::seats::seats_rename,
        crate::iyke::seats::seats_remove,
        crate::iyke::seats::seats_release,
        // first-run wizard detection
        crate::agent_detect::detect_system,
        crate::agent_detect::detect_agents,
        crate::agent_detect::detect_agent,
        crate::agent_detect::detect_agent_config,
        crate::agent_detect::list_claude_projects,
        crate::agent_detect::list_agent_projects,
        crate::agent_detect::scaffold_agent_config,
        // ── terminal (pty) ──────────────────────────────────────────────────
        pty::pty_spawn,
        pty::pty_write,
        pty::pty_resize,
        pty::pty_kill,
        pty::pty_attach_begin,
        pty::pty_attach_arm,
        pty::pty_foreground,
        pty::pty_foreground_snapshot,
        pty::pty_terminal_list,
        pty::terminal_detect_shells,
        pty::pty_daemon_info,
        pty::pty_daemon_shutdown,
        // ── windows + desktop chrome ────────────────────────────────────────
        // multi-window substrate (plans/multi-window WP-03)
        window::window_spawn,
        window::window_close,
        window::window_list,
        // WP-69: Pop out joins Window 2 (G-SEATS §4.4)
        window::window_join_surface,
        window::window_remove_surface,
        // desktop
        desktop::set_dock_badge,
        desktop::iyke_mcp_info,
        // OS-wide shortcuts from the effective keymap (WP-54, G-ACTIONS §6)
        os_shortcuts::os_shortcuts_apply,
        // screenshots
        screenshot::screenshot_window,
        screenshot::screenshot_pane,
        screenshot::screenshot_capture_done,
        screenshot::screenshot_capture_failed,
        screenshot::screenshot_capture_native_crop,
        screenshot::screenshot_get_config,
        screenshot::screenshot_set_dir,
        // ── fs ──────────────────────────────────────────────────────────────
        fs::fs_read,
        fs::fs_write,
        fs::fs_mkdir,
        fs::fs_exists,
        fs::fs_kind,
        fs::fs_list,
        fs::fs_mime,
        fs::fs_watch,
        fs::fs_unwatch,
        fs::fs_trash,
        fs::fs_rename,
        fs::fs_search,
        // fs allowlist (user-configurable roots)
        fs_roots::fs_roots_list,
        fs_roots::fs_roots_add,
        fs_roots::fs_roots_remove,
        fs_roots::fs_roots_reset,
        // ── claude config + Ngwa ────────────────────────────────────────────
        // claude config browser
        claude_config::claude_config_load,
        claude_config::claude_config_watch,
        claude_config::claude_config_unwatch,
        claude_config::claude_config_read_file,
        crate::settings_cascade::claude_config_resolve_cascade,
        crate::claude::session_browser::claude_session_list,
        // claude config — Phase 4 (4-tier discovery + pin CRUD)
        claude_config::claude_assets_discover,
        claude_config::claude_asset_pin,
        claude_config::claude_asset_unpin,
        claude_config::claude_asset_list_pins,
        // Ngwa store layer — WP-02 (central store + symlink farm)
        claude_store::claude_store_list,
        claude_store::claude_store_import,
        claude_store::claude_primitive_enable,
        claude_store::claude_primitive_disable,
        claude_store::claude_primitive_copy,
        claude_store::claude_primitive_move,
        claude_store::claude_primitive_remove,
        // Ngwa Phase-2 v2b write — unified per-engine dispatch + cross-engine copy
        claude_store::claude_primitive_enable_for,
        claude_store::claude_primitive_disable_for,
        claude_store::claude_primitive_remove_for,
        claude_store::claude_primitive_copy_batch,
        // Ngwa Ọba registry — WP-04 dependent-aware safe delete + WP-06 finish
        claude_store::oba_dependents,
        claude_store::oba_safe_delete,
        claude_store::oba_relink_dependents,
        claude_store::oba_unlink_one,
        claude_store::oba_forget,
        claude_store::oba_backfill_registry,
        claude_store::oba_install_git,
        claude_store::oba_install_npx,
        claude_store::oba_install_local,
        claude_store::oba_install_bundle,
        claude_store::oba_install_with_deps,
        claude_store::oba_missing_requires,
        claude_store::oba_check_update,
        claude_store::oba_update,
        claude_store::oba_resolve_source,
        claude_store::oba_auto_update_all,
        claude_store::oba_set_auto_update,
        // Ngwa Phase-2 cross-system — G-ADAPTER engine layout descriptor
        engine_layout::engine_layout,
        // Ngwa Phase-2 — WP-14 unified snapshot (G-NGWA-ITEM)
        ngwa::ngwa_snapshot,
        // ── identity + app lock ─────────────────────────────────────────────
        identity::os_username,
        // app lock (WP-72, D-05 `locked`)
        app_lock::app_lock_status,
        app_lock::app_lock_touch,
        app_lock::app_lock_lock,
        app_lock::app_lock_unlock,
        app_lock::app_lock_unlock_biometric,
        app_lock::app_lock_configure,
        app_lock::app_lock_set_secret,
        app_lock::app_lock_clear_secret,
        // ── secrets ─────────────────────────────────────────────────────────
        secrets::secrets_get,
        secrets::secrets_set,
        secrets::secrets_delete,
        secrets::secrets_list_keys,
        secrets::secrets_index_names,
        secrets::secrets_vault_status,
        secrets::secrets_set_passphrase,
        secrets::secrets_unlock,
        secrets::secrets_lock,
        secrets::secrets_lock_state,
        // secrets — Phase 7 scoped variants
        secrets::secrets_get_scoped,
        secrets::secrets_set_scoped,
        secrets::secrets_delete_scoped,
        secrets::secrets_list_keys_scoped,
        // supabase config (URL + anon key manifest)
        supabase_config::supabase_config_get,
        supabase_config::supabase_config_set,
        supabase_config::supabase_config_clear,
        // ── settings, actions, keybindings ──────────────────────────────────
        // settings_kv (durable mirror for Zustand-backed prefs)
        settings_kv::settings_get,
        settings_kv::settings_set,
        settings_kv::settings_get_all,
        settings_kv::settings_clear_all,
        settings_kv::settings_read_file,
        settings_kv::settings_write_field,
        settings_kv::settings_open_file,
        // actions — WP-50 actions.json / keybindings.json + project trust
        actions::actions_read_files,
        actions::actions_write,
        actions::keybindings_write,
        actions::actions_open_file,
        actions::actions_trust_status,
        actions::actions_trust_grant,
        actions::actions_trust_revoke,
        // action runner — WP-53 `shell` run kind + `{{branch}}`
        action_exec::action_exec,
        action_exec::action_git_branch,
        // ── notifications ───────────────────────────────────────────────────
        // WP-40 aggregation table (D-07 notification centre)
        notifications::notifications_list,
        notifications::notifications_unread_count,
        notifications::notifications_mark_read,
        notifications::notifications_mark_all_read,
        notifications::notifications_mute_state,
        notifications::notifications_mute_kind,
        notifications::notifications_unmute_kind,
        notifications::notifications_record_update,
        // ── projects + atelier ──────────────────────────────────────────────
        // projects (phase 0 of projects-first-class plan)
        projects::project_create,
        projects::project_update,
        projects::project_list,
        projects::project_archive,
        projects::project_set_active,
        projects::project_get_active,
        projects::project_inventory,
        projects::project_skills_list,
        projects::project_scaffold_claude,
        projects::project_artifacts_walk,
        // atelier skill files (WP-16b / WP-10) — generic reader for
        // <project_root>/.atelier/<skill>/<file>; the Tasks roster read is one caller.
        skill_roster::atelier_file_read,
        // atelier instance write path (WP-18b) — the setup-chat confirm-write
        // persists <project_root>/.atelier/<skill>/manifest.json through here.
        skill_roster::atelier_file_write,
        // ── db, data health, backup ─────────────────────────────────────────
        db::db_query,
        db::db_exec,
        // data health (orphan audit + DEC-32 db size)
        data_health::data_health_scan,
        data_health::data_health_db_size,
        // backup / restore
        backup::backup_export,
        backup::backup_import,
        backup::backup_list,
        backup::backup_delete,
        backup::db_export_ndjson,
        backup::db_import_ndjson,
        // ── iyke bridge + viewer ────────────────────────────────────────────
        iyke::iyke_endpoint,
        iyke::iyke_set_shell,
        crate::iyke::handlers::iyke_set_frame,
        // WP-62: the `iyke` actions/menus/keys surface — FE→Rust
        // effective-model mirror push + the write/query round-trip
        // callback (`src-tauri/src/iyke/actions_routes.rs`).
        crate::iyke::actions_routes::iyke_set_actions_frame,
        crate::iyke::actions_routes::iyke_actions_request_done,
        iyke::iyke_log_push,
        iyke::iyke_network_push,
        iyke::iyke_dom_done,
        iyke::iyke_dom_query,
        iyke::iyke_query_cache_done,
        iyke::iyke_wait_done,
        iyke::iyke_terminal_read_done,
        iyke::iyke_terminal_spawn_done,
        iyke::iyke_action_done,
        crate::iyke::browser_handlers::iyke_browser_reply,
        // viewer
        viewer::viewer_serve,
        viewer::viewer_stop,
        viewer::viewer_port,
        // ── pkg trust + permissions ─────────────────────────────────────────
        // trust gating (Phase 9)
        trust::pkg_trust_list,
        trust::pkg_trust_preview,
        trust::pkg_trust_grant,
        trust::pkg_trust_revoke,
        // trust-review modal (2026-05-15) — capability-diff batch surface
        pkg_trust::pkg_trust_list_pending,
        pkg_trust::pkg_trust_preview_incoming,
        pkg_trust::pkg_trust_approve,
        pkg_trust::pkg_trust_reject,
        // per-folder Studio project-access gate (WP-04)
        pkg_studio::pkg_studio_request_project_access,
        // runtime-ACL violations audit (2026-05-15)
        permissions_audit::pkg_permission_violations_list,
        permissions_audit::pkg_permission_violations_clear,
        // approve-gate run-then-pause seam (pa_action_drafts, WP-3)
        pa_actions::pa_actions_pause,
        pa_actions::pa_actions_list,
        pa_actions::pa_actions_update,
        pa_actions::pa_actions_commit,
        pa_actions::pa_actions_reject,
        pa_actions::pa_actions_retry,
        // spike: dynamic ACL verification (delete after kernel lands)
        spike::spike_grant_fs_read,
        spike::spike_setup_test_file,
        // ── pkg kernel ──────────────────────────────────────────────────────
        pkg::pkg_install_from_path,
        pkg::pkg_install_from_registry,
        pkg::pkg_uninstall,
        pkg::pkg_set_enabled,
        pkg::pkg_set_scope,
        pkg::pkg_kernel_status,
        pkg::list_skill_actions,
        pkg::list_all_skill_actions,
        pkg::pkg_discover_workspace,
        pkg::pkg_db_diag,
        pkg::pkg_health_scan,
        pkg::pkg_health_remove,
        pkg::pkg_health_remove_all,
        pkg::pkg_settings_get,
        pkg::pkg_settings_set,
        pkg::pkg_activity_bar_set_badge,
        pkg::pkg_preview_manifest,
        scaffold::pkg_scaffold,
        pkg::pkg_screenshot,
        pkg_content::pkg_content_url,
        pkg_content::pkg_content_html,
        pkg_content::pkg_content_revoke,
        pkg::pkg_is_trusted_for_elevated,
        // trusted-pkg elevated verbs (ADR-017, WP-04/05)
        pkg_fetch::pkg_fetch,
        pkg_invoke::pkg_invoke,
        pkg_mcp::pkg_mcp_call,
        pkg_sidecar::pkg_sidecar_call,
        pkg_sidecar_stream::pkg_sidecar_rpc_send,
        pkg_sidecar_stream::pkg_sidecar_rpc_shutdown,
        pkg_mcp::pkg_supervisor_restart,
        pkg_dev::pkg_dev_register,
        pkg_dev::pkg_dev_unregister,
        pkg_dev::pkg_dev_reload,
        runtime::runtime_retry_bun_fetch,
        pkg_mcp::dev_bind_port,
        pkg_mcp::dev_release_port,
        // pkg-browser child webviews
        pkg_webview::pkg_webview_create,
        pkg_webview::pkg_webview_allow_origin,
        pkg_webview::pkg_webview_destroy,
        pkg_webview::pkg_webview_navigate,
        pkg_webview::pkg_webview_set_rect,
        pkg_webview::pkg_webview_clear_session,
        // ── activity bar, comments, studio ──────────────────────────────────
        // activity bar pinning
        activity_bar::activity_pins_list,
        activity_bar::activity_pins_add,
        activity_bar::activity_pins_remove,
        activity_bar::activity_pins_reorder,
        activity_bar::activity_pins_resolve_artifact,
        activity_bar::activity_pins_touch_open,
        activity_bar::activity_sections_list,
        activity_bar::activity_sections_create,
        activity_bar::activity_sections_update,
        activity_bar::activity_sections_remove,
        // artifact-grid pin comments
        comments::comment_create,
        comments::comment_get,
        comments::comment_list,
        comments::comment_record_routing,
        comments::comment_set_status,
        comments::comment_delete,
        comment_route::comment_route,
        comments::pin_screenshot_write,
        // artifact-studio chat threads (one per folder, D3)
        studio_threads::studio_thread_get_or_create,
        studio_threads::studio_thread_get,
        studio_threads::studio_thread_list_recent,
        studio_threads::studio_thread_delete,
        studio_threads::studio_message_append,
        studio_threads::studio_message_list,
        // ── debug-only spikes ───────────────────────────────────────────────
        // Phase 0.5 bg-execution spike. Debug builds only.
        #[cfg(debug_assertions)]
        bg_spike::bg_spike_run,
        #[cfg(debug_assertions)]
        bg_spike::bg_spike_reply,
        // ── devices (WP-74) ─────────────────────────────────────────────────
        // G-ACCESS §9.1, registered skeleton-first by WP-74a (§9.2): every
        // command below is a thin proxy to the local daemon (P-20), except
        // `permission_decide`, served in-process (review C-05).
        access::access_status,
        access::access_devices_list,
        access::access_device_set_tier,
        access::access_device_revoke,
        access::access_pair_begin,
        access::access_pair_cancel,
        access::access_pair_pending,
        access::access_pair_decide,
        // ── access / members (WP-76) ────────────────────────────────────────
        access::access_members_list,
        access::access_member_set_role,
        access::access_member_remove,
        access::access_member_restore,
        access::access_policy_get,
        access::access_policy_set_cell,
        access::access_policy_set_owner_approval,
        access::access_invite_issue,
        access::access_invite_revoke,
        access::access_shares_list,
        // ── permission routing (WP-75) ──────────────────────────────────────
        access::access_routing_get,
        access::access_routing_set,
        access::permission_decide,
        // ── audit (WP-77) ───────────────────────────────────────────────────
        access::access_audit_list,
        access::access_audit_verify,
        access::access_audit_export,
        access::access_audit_record_local,
        access::access_audit_reseal,
    ]
}
