//! `pkg_invoke` — the Rust half of the `host.invoke` scoped-passthrough verb
//! (ADR-017, WP-05). A TRUSTED iframe pkg runs a small allowlist of NAMED
//! commands. This is NOT a general shell bridge.
//!
//! POLICY (D-06 + the survey guardrail): named commands only, never `*`. The
//! allowlist is `capabilities.invoke.commands` — invoke's OWN field, NOT
//! `permissions["shell.execute"]`. Reusing `shell.execute` would trip
//! `trust::requires_trust`, so the pkg could only ever reach user-`Granted`
//! (never `AutoTrusted`), and `is_trusted_for_elevated()` would be false →
//! `host.invoke` would ALWAYS deny. Keeping the allowlist in the cap lets a
//! signed/builtin pkg declare invokable commands while leaving `shell.execute`
//! empty, so it stays `AutoTrusted` → elevated. This is the key D-06 fix.
//!
//! Enforcement (all server-side; the FE check is fail-fast UX only):
//! 1. Re-check `resolve_elevated_trust` (a hostile iframe skips the FE gate).
//! 2. `capabilities.invoke` must be present.
//! 3. `command` must glob-match an entry in `capabilities.invoke.commands`
//!    (reuses `permissions_check::check_shell_execute` — the same glob matcher
//!    the kernel uses for spawn allowlists). Deny → audit + refuse.
//! 4. Run the named command; return `{ stdout, stderr, exit_code }`.

use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::commands::db::PaDb;
use crate::commands::pkg::KernelState;
use crate::pkg::manifest::Package;
use crate::pkg::permissions_check::{check_shell_execute, record_violation};
use crate::platform::NoConsoleWindow;

/// Default timeout for one-shot invocations — generous but bounded so a hung
/// process can't pin a Tauri worker forever.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PkgInvokeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}

impl PkgInvokeResult {
    fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            stdout: None,
            stderr: None,
            exit_code: None,
            timed_out: false,
        }
    }
}

/// Load the pkg's manifest off disk and return its `capabilities.invoke.commands`
/// allowlist. `None` when the pkg isn't installed, the manifest won't load, or it
/// didn't declare `capabilities.invoke` (fail-closed — denies the verb).
fn invoke_allowlist(kernel: &crate::pkg::kernel::Kernel, pkg_id: &str) -> Option<Vec<String>> {
    let install_path = kernel.installed_path(pkg_id)?;
    let pkg = Package::load(&install_path)
        .map_err(|e| {
            tracing::warn!("[pkg_invoke] pkg `{pkg_id}` manifest reload failed: {e:#}");
        })
        .ok()?;
    pkg.manifest
        .capabilities
        .as_ref()?
        .invoke
        .as_ref()
        .map(|i| i.commands.clone())
}

#[tauri::command]
pub async fn pkg_invoke(
    app: AppHandle,
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<PaDb>>,
    pkg_id: String,
    command: String,
    args: Vec<String>,
) -> Result<PkgInvokeResult, String> {
    let pool = db.ensure_pool().await.map_err(|e| e.to_string())?;
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;

    // (1) Capability presence — `capabilities.invoke` must be declared.
    let allowlist = match invoke_allowlist(&kernel.0, &pkg_id) {
        Some(list) => list,
        None => {
            return Ok(PkgInvokeResult::err(format!(
                "pkg `{pkg_id}` lacks the 'invoke' capability"
            )));
        }
    };

    // (2) Trust gate — re-checked server-side (the FE check is fail-fast only).
    let trusted =
        crate::pkg::trust::resolve_elevated_trust(&pool, &kernel.0, &app_data, &pkg_id).await;
    if !trusted {
        tracing::warn!(
            "[pkg_invoke] pkg `{pkg_id}` host.invoke denied — not trusted-for-elevated (fail-closed)"
        );
        return Ok(PkgInvokeResult::err(format!(
            "pkg `{pkg_id}` host.invoke requires a trusted pkg"
        )));
    }

    // (3) Command allowlist — glob-match `command` against
    // `capabilities.invoke.commands` (reuses the kernel's spawn-allowlist
    // matcher). Deny → audit to pkg_permission_violations + refuse.
    if let Err(denial) = check_shell_execute(&pkg_id, &allowlist, &command) {
        record_violation(&pool, "capabilities.invoke", &denial)
            .await
            .ok();
        tracing::warn!(
            "[pkg_invoke] pkg `{pkg_id}` host.invoke `{command}` not in capabilities.invoke.commands (declared: {})",
            denial.declared
        );
        return Ok(PkgInvokeResult::err(format!(
            "pkg `{pkg_id}` not permitted to invoke `{command}` (not in capabilities.invoke.commands)"
        )));
    }

    // (4) Run the named command, capturing stdout/stderr/exit_code.
    let mut cmd = Command::new(&command);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .no_console_window();

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(PkgInvokeResult::err(format!(
                "spawn `{command}` failed: {e}"
            )));
        }
    };

    let dur = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let output = match timeout(dur, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Ok(PkgInvokeResult::err(format!(
                "`{command}` wait failed: {e}"
            )));
        }
        Err(_elapsed) => {
            return Ok(PkgInvokeResult {
                ok: false,
                error: Some(format!("`{command}` timed out after {DEFAULT_TIMEOUT_SECS}s")),
                stdout: None,
                stderr: None,
                exit_code: None,
                timed_out: true,
            });
        }
    };

    let exit_code = output.status.code();
    tracing::info!(
        "[pkg_invoke] pkg `{pkg_id}` invoked `{command}` → exit {:?}",
        exit_code
    );
    Ok(PkgInvokeResult {
        ok: output.status.success(),
        error: None,
        stdout: Some(String::from_utf8_lossy(&output.stdout).to_string()),
        stderr: Some(String::from_utf8_lossy(&output.stderr).to_string()),
        exit_code,
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// An allowlisted command passes the same glob gate the command uses
    /// (`check_shell_execute` against `capabilities.invoke.commands`). This is
    /// the gate the live command consults at step (3) — exercising it directly
    /// proves the allowlist matcher without a running shell.
    #[test]
    fn allowlisted_command_passes_the_gate() {
        let allow = s(&["pa_actions_commit", "pa_actions_reject"]);
        assert!(check_shell_execute("com.ikenga.outbound", &allow, "pa_actions_commit").is_ok());
        assert!(check_shell_execute("com.ikenga.outbound", &allow, "pa_actions_reject").is_ok());
    }

    /// A command NOT in the allowlist is denied (and the live command audits +
    /// refuses on this same `Err`).
    #[test]
    fn non_allowlisted_command_is_denied() {
        let allow = s(&["pa_actions_commit"]);
        let denial =
            check_shell_execute("com.ikenga.outbound", &allow, "secrets_set_scoped").unwrap_err();
        assert_eq!(denial.command, "secrets_set_scoped");
        assert_eq!(denial.declared, "pa_actions_commit");
    }

    /// An empty `capabilities.invoke.commands` allowlist denies everything —
    /// declaring the cap without listing commands grants nothing (fail-closed).
    #[test]
    fn empty_allowlist_denies_everything() {
        assert!(check_shell_execute("p", &[], "anything").is_err());
    }

    /// The refusal envelope shape the command returns for the cap-missing /
    /// untrusted / not-permitted branches.
    #[test]
    fn err_envelope_shape() {
        let r = PkgInvokeResult::err("nope");
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("nope"));
        assert!(r.stdout.is_none());
        assert!(!r.timed_out);
    }
}
