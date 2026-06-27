//! Tauri command surface. One file per domain. The full surface is locked in
//! during phase 1 even where the implementation is a stub — the typed wrappers
//! in `src/lib/tauri-cmd.ts` mirror this, so later phases just fill in the
//! Rust side.

pub mod activity_bar;
pub mod agent_ops;
pub mod backup;
#[cfg(debug_assertions)]
pub mod bg_spike;
pub mod chi;
pub mod claude;
pub mod claude_config;
pub mod claude_store;
pub mod comment_route;
pub mod comments;
pub mod data_health;
pub mod db;
pub mod desktop;
pub mod engine_layout;
pub mod fs;
pub mod fs_roots;
pub mod identity;
pub mod iyke;
pub mod pa_actions;
pub mod permissions_audit;
pub mod pkg;
pub mod pkg_content;
pub mod pkg_dev;
pub mod pkg_fetch;
pub mod pkg_invoke;
pub mod pkg_mcp;
pub mod pkg_sidecar;
pub mod pkg_sidecar_stream;
pub mod pkg_studio;
pub mod pkg_trust;
pub mod pkg_webview;
pub mod projects;
pub mod pty;
pub mod runtime;
pub mod screenshot;
pub mod secrets;
pub mod settings_kv;
pub mod skill_roster;
pub mod spike;
pub mod studio_threads;
pub mod supabase_config;
pub mod trust;
pub mod viewer;
pub mod window;
#[cfg(debug_assertions)]
pub mod window_cost;

pub use activity_bar::{
    activity_pins_add, activity_pins_list, activity_pins_remove, activity_pins_reorder,
    activity_pins_resolve_artifact, activity_pins_touch_open, activity_sections_create,
    activity_sections_list, activity_sections_remove, activity_sections_update,
};
pub use agent_ops::{
    agent_ops_delete_job, agent_ops_list_jobs, agent_ops_run_now, agent_ops_set_enabled,
    agent_ops_tail_run, agent_ops_upsert_job,
};
pub use backup::{
    backup_delete, backup_export, backup_import, backup_list, db_export_ndjson, db_import_ndjson,
};
#[cfg(debug_assertions)]
pub use bg_spike::{bg_spike_reply, bg_spike_run, new_state as new_bg_spike_state};
pub use chi::{chi_cancel, chi_list, chi_resume, chi_run, chi_status, ChiCache, ChiRuntime};
pub use claude::{
    claude_list_sessions, claude_read_jsonl, session_cancel, session_destroy, session_destroy_all,
    session_ensure, session_send, session_tool_result,
};
pub use claude_config::{
    claude_asset_list_pins, claude_asset_pin, claude_asset_unpin, claude_assets_discover,
    claude_config_load, claude_config_read_file, claude_config_unwatch, claude_config_watch,
};
pub use claude_store::{
    claude_primitive_copy, claude_primitive_copy_batch, claude_primitive_disable,
    claude_primitive_disable_for, claude_primitive_enable, claude_primitive_enable_for,
    claude_primitive_move, claude_primitive_remove, claude_primitive_remove_for,
    claude_store_import, claude_store_list, oba_auto_update_all, oba_backfill_registry,
    oba_check_update, oba_dependents, oba_forget, oba_install_bundle, oba_install_git,
    oba_install_local, oba_install_npx, oba_install_with_deps, oba_missing_requires,
    oba_relink_dependents, oba_safe_delete, oba_set_auto_update, oba_unlink_one, oba_update,
};
pub use comment_route::comment_route;
pub use comments::{
    comment_create, comment_delete, comment_get, comment_list, comment_record_routing,
    comment_set_status, pin_screenshot_write,
};
pub use data_health::data_health_scan;
pub use db::{db_exec, db_query};
pub use desktop::{iyke_mcp_info, set_dock_badge};
pub use engine_layout::engine_layout;
pub use fs::{
    fs_exists, fs_kind, fs_list, fs_mime, fs_mkdir, fs_read, fs_rename, fs_search, fs_trash,
    fs_unwatch, fs_watch, fs_write,
};
pub use fs_roots::{fs_roots_add, fs_roots_list, fs_roots_remove, fs_roots_reset};
pub use identity::os_username;
pub use iyke::{
    iyke_action_done, iyke_dom_done, iyke_dom_query, iyke_endpoint, iyke_log_push,
    iyke_network_push, iyke_query_cache_done, iyke_set_shell, iyke_terminal_read_done,
    iyke_terminal_spawn_done, iyke_wait_done, IykeRuntimeState,
};
pub use pa_actions::{
    pa_actions_commit, pa_actions_list, pa_actions_pause, pa_actions_reject, pa_actions_retry,
    pa_actions_update,
};
pub use permissions_audit::{pkg_permission_violations_clear, pkg_permission_violations_list};
pub use pkg::{
    list_all_skill_actions, list_skill_actions, pkg_db_diag, pkg_discover_workspace,
    pkg_health_remove, pkg_health_remove_all, pkg_health_scan, pkg_install_from_path,
    pkg_install_from_registry, pkg_is_trusted_for_elevated, pkg_kernel_status,
    pkg_preview_manifest, pkg_screenshot, pkg_set_enabled, pkg_set_scope, pkg_settings_get,
    pkg_settings_set, pkg_uninstall, KernelState, PkgSettingsState,
};
pub use pkg_content::{pkg_content_html, pkg_content_revoke, pkg_content_url, PkgContentState};
pub use pkg_dev::{pkg_dev_register, pkg_dev_reload, pkg_dev_unregister};
pub use pkg_fetch::pkg_fetch;
pub use pkg_invoke::pkg_invoke;
pub use pkg_mcp::{
    dev_bind_port, dev_release_port, pkg_mcp_call, pkg_supervisor_restart, SidecarSupervisorState,
};
pub use pkg_sidecar::{pkg_sidecar_call, SidecarsRegistryState};
pub use pkg_sidecar_stream::{
    pkg_sidecar_rpc_send, pkg_sidecar_rpc_shutdown, StreamingSidecarManager,
    StreamingSidecarManagerState,
};
pub use pkg_studio::pkg_studio_request_project_access;
pub use pkg_trust::{pkg_trust_approve, pkg_trust_list_pending, pkg_trust_reject};
pub use pkg_webview::{
    pkg_webview_create, pkg_webview_destroy, pkg_webview_navigate, pkg_webview_set_rect,
    WebviewPanesState,
};
pub use projects::{
    project_archive, project_artifacts_walk, project_create, project_get_active, project_inventory,
    project_list, project_scaffold_claude, project_set_active, project_skills_list, project_update,
};
pub use pty::{
    pty_attach_arm, pty_attach_begin, pty_foreground, pty_foreground_snapshot, pty_kill,
    pty_resize, pty_spawn, pty_terminal_list, pty_write, terminal_detect_shells,
};
pub use runtime::runtime_retry_bun_fetch;
pub use screenshot::{
    screenshot_capture_done, screenshot_capture_failed, screenshot_capture_native_crop,
    screenshot_get_config, screenshot_pane, screenshot_set_dir, screenshot_window,
    ScreenshotConfigState, ScreenshotConfigStateRef, ScreenshotPending, ScreenshotResult,
};
pub use secrets::{
    secrets_delete, secrets_delete_scoped, secrets_get, secrets_get_scoped, secrets_list_keys,
    secrets_list_keys_scoped, secrets_set, secrets_set_scoped, secrets_vault_status, SecretsLock,
};
pub use settings_kv::{settings_clear_all, settings_get, settings_get_all, settings_set};
pub use skill_roster::{atelier_file_read, atelier_file_write};
pub use spike::{spike_grant_fs_read, spike_setup_test_file};
pub use studio_threads::{
    studio_message_append, studio_message_list, studio_thread_delete, studio_thread_get,
    studio_thread_get_or_create, studio_thread_list_recent,
};
pub use supabase_config::{supabase_config_clear, supabase_config_get, supabase_config_set};
pub use trust::{pkg_trust_grant, pkg_trust_list, pkg_trust_preview, pkg_trust_revoke};
pub use viewer::{viewer_port, viewer_serve, viewer_stop};
pub use window::{window_close, window_list, window_spawn};
#[cfg(debug_assertions)]
pub use window_cost::{
    new_state as new_window_cost_state, window_cost_ping, window_cost_run,
};

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// Resolve `~/...` and env vars, then enforce the user-configurable allowlist
/// (see `crate::fs_roots`). Returns the canonical absolute path.
///
/// The active root set lives in a process-global `OnceLock` set by
/// `lib.rs::run` during `.setup()`, so this function does not need to thread
/// `tauri::State` through every fs command + the viewer.
pub fn resolve_allowlisted(input: &str) -> Result<PathBuf> {
    let expanded = shellexpand::full(input)
        .map(|c| c.into_owned())
        .map_err(|e| anyhow!("shellexpand failed: {e}"))?;
    let path = PathBuf::from(&expanded);
    let abs = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };

    // `canonicalize` requires the path to exist. For writes to new files we
    // canonicalize the parent and re-attach the filename so the allowlist
    // check still works.
    let canonical = if abs.exists() {
        abs.canonicalize()?
    } else if let Some(parent) = abs.parent() {
        if parent.exists() {
            let parent_canon = parent.canonicalize()?;
            match abs.file_name() {
                Some(name) => parent_canon.join(name),
                None => parent_canon,
            }
        } else {
            return Err(anyhow!("path does not exist: {}", abs.display()));
        }
    } else {
        return Err(anyhow!("path has no parent: {}", abs.display()));
    };

    if !is_allowed(&canonical)? {
        return Err(anyhow!("path outside allowlist: {}", canonical.display()));
    }
    Ok(canonical)
}

fn is_allowed(path: &Path) -> Result<bool> {
    let roots = crate::fs_roots::current().ok_or_else(|| anyhow!("fs_roots not initialized"))?;
    Ok(roots.is_allowed(path))
}
