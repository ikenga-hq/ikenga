//! OS-level identity fallback, shared by the desktop `os_username` command
//! and the daemon's `/api/rpc` arm (WP-19 slice 3).
//!
//! `os_username` backs the shell's `hostContext.operator` field (see
//! `@ikenga/contract`'s `host-context.ts`) when the user hasn't set an
//! onboarding display name (`useShellStore().userName`). It is a fallback
//! source only, not a durable account id.

/// Returns the current OS username, or `"unknown"` if the environment
/// doesn't expose one. Mirrors the precedent in `backup.rs`'s
/// `BackupManifest::new` (`std::env::var("USER")`).
///
/// On the daemon this is the daemon PROCESS's user, the same answer for every
/// caller holding its token. Single-user seam (G-PRINCIPAL / WP-20): under
/// per-principal isolation (T1) it must be the calling principal's username.
pub fn os_username() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into())
}
