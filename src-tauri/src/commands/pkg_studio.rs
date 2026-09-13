//! WP-04 — per-folder Studio project-access trust gate.
//!
//! `com.ikenga.studio`'s launcher (WP-07) and its sidecar (WP-03) both need to
//! open an **arbitrary** user folder as a project — a capability that sits
//! outside the pkg's `$pkg_data` sandbox and therefore must be user-approved.
//! This module lands the frozen-signature Tauri command they compile against:
//!
//! ```ignore
//! pkg_studio_request_project_access(path: PathBuf) -> Result<RequestAccessResponse, String>
//! struct RequestAccessResponse { granted: bool }
//! ```
//!
//! Behaviour (idempotent, prompt-once-per-folder):
//! 1. Canonicalize `path` (`std::fs::canonicalize`) — symlinks resolve,
//!    `/foo/./bar` collapses to `/foo/bar`, and a non-existent path errors
//!    up-front (`path_not_found`) rather than prompting on a phantom.
//! 2. If `com.ikenga.studio` already holds a granted `__studio_project` row for
//!    the canonical path → return `{ granted: true }` **without prompting**.
//!    Constant-time re-hit.
//! 3. Otherwise pop the trust prompt and await the user's decision. On grant →
//!    insert the row + return `{ granted: true }`. On decline → return
//!    `{ granted: false }` with **no** row and no side-effect.
//!
//! Prompt surface: the existing `tauri-plugin-dialog` message dialog (already
//! initialised in `lib.rs`). It is awaitable from Rust — a hard requirement for
//! this command's synchronous `-> { granted }` contract — and adds **no new FE
//! component**. The styled in-app trust surfaces (`Settings → Pkgs → Trust`,
//! the boot-review banner) are *poll-based* list UIs with no awaitable
//! "decide now" entry point, so they can't back a command that must return the
//! decision inline. Wiring those instead would require a oneshot-park + event
//! emit + FE response-command bridge (the shape `chat_respond_permission` uses)
//! — deferred; see the WP-04 report.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::commands::db::PaDb;
use crate::pkg::trust;

/// Frozen response shape for `pkg_studio_request_project_access`. Single
/// boolean field so callers (WP-03 sidecar, WP-07 launcher) bind against a
/// stable, minimal contract.
#[derive(Debug, Clone, Serialize)]
pub struct RequestAccessResponse {
    pub granted: bool,
}

/// Canonicalize a caller-supplied project path into the key the grant row is
/// stored under. Errors (`path_not_found`) if the path doesn't resolve — the
/// caller is expected to pass an existing directory. On Windows the `\\?\` UNC
/// verbatim prefix `canonicalize` prepends is stripped so the stored key is
/// platform-portable and matches what a user would recognise.
pub fn canonicalize(path: &Path) -> Result<String, String> {
    let canon = std::fs::canonicalize(path)
        .map_err(|e| format!("path_not_found: {} ({e})", path.display()))?;
    Ok(strip_verbatim_prefix(canon))
}

#[cfg(windows)]
fn strip_verbatim_prefix(p: PathBuf) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(p: PathBuf) -> String {
    p.display().to_string()
}

/// Await the trust prompt for `canonical_path`. Returns `true` on grant,
/// `false` on decline. Bridges the plugin's callback-style `show` into an
/// awaitable via a oneshot; a dropped sender (window torn down before the user
/// answers) resolves to `false` (fail-closed — no grant without an explicit
/// yes).
async fn prompt_for_access(app: &AppHandle, canonical_path: &str) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    app.dialog()
        .message(format!(
            "Ikenga Studio is requesting access to the folder:\n\n{canonical_path}\n\n\
             Granting lets Studio read and write files in this folder. \
             You'll only be asked once per folder."
        ))
        .title("Grant Studio folder access?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Grant access".to_string(),
            "Deny".to_string(),
        ))
        .show(move |granted| {
            // FnOnce(bool): true == the "Grant access" (Ok) button.
            let _ = tx.send(granted);
        });
    rx.await.unwrap_or(false)
}

/// Transport-independent trust decision.
///
/// Extracted so the Tauri command and the iyke localhost relay
/// (`iyke::pkg_trust`, used by the Studio sidecar — a separate OS process that
/// cannot `invoke()`) resolve to exactly the *same* decision and cannot drift.
/// See `plans/studio/verify/2026-09-12-wp32-live/wp04/design-note.md`.
pub async fn request_project_access(
    app: &AppHandle,
    db: &Arc<PaDb>,
    path: PathBuf,
) -> Result<RequestAccessResponse, String> {
    let canonical = canonicalize(&path)?;
    let pool = db.ensure_pool().await?;

    // (1) Re-hit: already granted → constant-time no-op, no prompt.
    if trust::has_studio_project_grant(&pool, &canonical).await.map_err(|e| e.to_string())? {
        tracing::info!(
            "[pkg_studio] project access already granted for `{canonical}` — skipping prompt"
        );
        return Ok(RequestAccessResponse { granted: true });
    }

    // (2) First time for this folder → prompt and await the decision.
    let granted = prompt_for_access(app, &canonical).await;
    if !granted {
        tracing::info!("[pkg_studio] project access DECLINED for `{canonical}` — no row written");
        return Ok(RequestAccessResponse { granted: false });
    }

    // (3) Grant. Re-check before insert so a concurrent request that resolved
    // first doesn't cause a wasted write (record_* is idempotent regardless,
    // but the re-check keeps the log honest under the two-caller race).
    if !trust::has_studio_project_grant(&pool, &canonical).await.map_err(|e| e.to_string())? {
        trust::record_studio_project_grant(&pool, &canonical)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!("[pkg_studio] project access GRANTED for `{canonical}` — row written");
    }
    Ok(RequestAccessResponse { granted: true })
}

/// Frozen-signature command. See module docs. One-line wrapper over
/// [`request_project_access`] so the Tauri and HTTP transports cannot drift.
#[tauri::command]
pub async fn pkg_studio_request_project_access(
    app: AppHandle,
    db: tauri::State<'_, Arc<PaDb>>,
    path: PathBuf,
) -> Result<RequestAccessResponse, String> {
    request_project_access(&app, db.inner(), path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/foo/./bar` collapses to `/foo/bar` — the two spellings canonicalize to
    /// the same stored key, so a grant under one is a hit under the other.
    #[test]
    fn canonicalize_collapses_dot_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("bar");
        std::fs::create_dir(&nested).expect("mkdir bar");

        let plain = canonicalize(&nested).expect("canonicalize plain");
        // Insert a `.` segment: <dir>/./bar
        let dotted = dir.path().join(".").join("bar");
        let dotted = canonicalize(&dotted).expect("canonicalize dotted");
        assert_eq!(plain, dotted, "/foo/./bar must collapse to /foo/bar");
    }

    /// A symlink resolves to its target's canonical path — the grant follows
    /// the resolved directory, not the link name.
    #[cfg(unix)]
    #[test]
    fn canonicalize_resolves_symlinks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        std::fs::create_dir(&real).expect("mkdir real");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let via_real = canonicalize(&real).expect("canon real");
        let via_link = canonicalize(&link).expect("canon link");
        assert_eq!(via_real, via_link, "symlink must resolve to its target");
    }

    /// A non-existent path fails up-front rather than being treated as a
    /// grantable folder — the command returns `path_not_found`, never prompts.
    #[test]
    fn canonicalize_errors_on_missing_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist");
        let err = canonicalize(&missing).expect_err("missing path must error");
        assert!(
            err.starts_with("path_not_found:"),
            "expected path_not_found, got: {err}"
        );
    }
}
