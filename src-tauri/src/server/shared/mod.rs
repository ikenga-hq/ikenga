//! AppHandle-free cores shared by the desktop `#[tauri::command]` wrappers
//! and the daemon's `/api/rpc` arms (WP-19 slice 2).
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

pub mod backups;
pub mod data_health;
pub mod settings;
pub mod supabase_config;
