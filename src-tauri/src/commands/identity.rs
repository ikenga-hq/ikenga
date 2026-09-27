//! OS-level identity fallback.
//!
//! `os_username` backs the shell's `hostContext.operator` field (see
//! `@ikenga/contract`'s `host-context.ts`) when the user hasn't set an
//! onboarding display name (`useShellStore().userName`). It is a fallback
//! source only, not a durable account id. The body lives in
//! `server::shared::identity`, which the headless daemon serves too.

/// Returns the current OS username, or `"unknown"` if the environment
/// doesn't expose one.
#[tauri::command]
pub fn os_username() -> String {
    crate::server::shared::identity::os_username()
}
