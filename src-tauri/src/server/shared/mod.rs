//! AppHandle-free cores shared by the desktop `#[tauri::command]` wrappers
//! and the daemon's `/api/rpc` arms (WP-19 slices 2–8).
//!
//! Same house pattern as `crate::db`, `crate::secrets_env` and `pkg::status`:
//! the logic lives here, compiled into both binaries; the desktop command in
//! `commands/` stays a thin call that resolves its Tauri-managed state (or
//! `app_data_dir`) and passes it in; the daemon arm resolves the same thing
//! from `--data-dir` / `AppState`. One implementation, so the two surfaces
//! cannot drift in behaviour or JSON shape.
//!
//! These live under `server/` only because `lib.rs` (which this slice may not
//! edit) declares `crate::settings` and `crate::commands` desktop-only, and
//! `server` is an ungated module in both builds. Nothing here depends on the
//! HTTP server — no axum, no `AppState`.

pub mod acp_mode;
pub mod actions;
pub mod activity_bar;
pub mod agent_config;
pub mod agent_ops;
pub mod agent_projects;
pub mod agent_scaffold;
pub mod agents;
pub mod atelier;
pub mod backups;
pub mod chi;
pub mod chi_exec;
pub mod chi_liveness;
pub mod chi_runner;
pub mod known;
pub mod claude_config;
pub mod claude_launch;
pub mod claude_sessions;
pub mod claude_store;
pub mod comments;
pub mod confined_fs;
pub mod data_health;
pub mod engine_layout;
pub mod failure_class;
pub mod fs;
pub mod git;
pub mod identity;
pub mod model_catalog;
pub mod notifications;
pub mod pa_actions;
pub mod pkg_db;
pub mod pkg_scaffold;
pub mod pkg_workspace;
pub mod projects;
pub mod seat_grammar;
pub mod settings;
pub mod settings_cascade;
pub mod shell_detect;
pub mod studio_threads;
pub mod supabase_config;
pub mod transcoder;
