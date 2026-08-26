//! `pkg_sidecar_call` — invoke a package-declared sidecar binary one-shot.
//!
//! Pkg manifests can declare `sidecars: [{ name, bin }]`. The
//! `SidecarsRegistry` indexes those binaries by name during install. This
//! command spawns one of them with a chosen subcommand + args, optional
//! stdin, and returns captured stdout/stderr/exit_code. It's the runtime
//! companion to the registry — a pkg's iframe (or the cron registry) reaches
//! the binary through here rather than via Tauri's static `externalBin`
//! shell scope.
//!
//! Permission model: the caller must pass the `pkg_id` it claims to be.
//! The sidecar registry stores `(pkg_id, name) -> bin_path`; we enforce
//! `entry.pkg_id == pkg_id` so a pkg cannot invoke another pkg's sidecar.
//! Iframe-origin to pkg_id resolution lives in the AppBridge wrapper that
//! ultimately calls this.

use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use tauri::State;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::commands::pkg::KernelState;
use crate::pkg::registries::SidecarsRegistry;

/// Tauri-state wrapper so commands can resolve sidecar paths without going
/// through the kernel snapshot.
pub struct SidecarsRegistryState(pub Arc<SidecarsRegistry>);

#[derive(Serialize)]
pub struct PkgSidecarCallResult {
    pub ok: bool,
    pub error: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

/// Default timeout for one-shot sidecar invocations. Pollers/sends should
/// finish in well under a minute; 120s gives slow networks headroom without
/// letting a hung process pin a Tauri worker forever.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[tauri::command]
pub async fn pkg_sidecar_call(
    kernel: State<'_, KernelState>,
    sidecars: State<'_, SidecarsRegistryState>,
    pkg_id: String,
    name: String,
    args: Vec<String>,
    stdin: Option<String>,
    timeout_secs: Option<u64>,
) -> Result<PkgSidecarCallResult, String> {
    // Verify pkg is installed (and by extension enabled — disabled pkgs
    // unregister from SidecarsRegistry, so resolve() below would also miss).
    let install_path = match kernel.0.installed_path(&pkg_id) {
        Some(p) => p,
        None => {
            return Ok(err(format!("pkg `{pkg_id}` is not installed")));
        }
    };

    // Resolve the binary path. The registry is the single source of truth;
    // it's populated at install time after validating that the bin exists
    // and lives under the package install dir.
    let entry = match sidecars.0.resolve(&name) {
        Some(e) => e,
        None => {
            return Ok(err(format!(
                "sidecar `{name}` is not registered (pkg may not be installed or declares no such sidecar)"
            )));
        }
    };

    // Permission gate: a pkg can only invoke its own sidecars. Mismatch is
    // a programmer error from the caller, not a runtime condition the user
    // should ever see; surface as a structured error so iframe code can
    // log it cleanly.
    if entry.pkg_id != pkg_id {
        return Ok(err(format!(
            "sidecar `{name}` belongs to `{}`, not `{pkg_id}`",
            entry.pkg_id
        )));
    }

    log::info!(
        "[pkg_sidecar_call] pkg={pkg_id} name={name} bin={} args={:?}",
        entry.bin_path.display(),
        args
    );

    let mut cmd = Command::new(&entry.bin_path);
    cmd.args(&args);
    cmd.current_dir(&install_path);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(err(format!("spawn `{}`: {e}", entry.bin_path.display())));
        }
    };

    // Pipe stdin if provided, then drop the writer so the child sees EOF.
    if let Some(payload) = stdin {
        if let Some(mut stdin_handle) = child.stdin.take() {
            if let Err(e) = stdin_handle.write_all(payload.as_bytes()).await {
                // Best-effort: kill the child and return the write error.
                let _ = child.start_kill();
                return Ok(err(format!("write stdin: {e}")));
            }
            if let Err(e) = stdin_handle.shutdown().await {
                log::warn!("[pkg_sidecar_call] stdin shutdown: {e}");
            }
        }
    } else {
        // Drop stdin handle immediately so the child sees EOF.
        drop(child.stdin.take());
    }

    let dur = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let output = match timeout(dur, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            return Ok(err(format!("wait: {e}")));
        }
        Err(_) => {
            return Ok(PkgSidecarCallResult {
                ok: false,
                error: Some(format!("sidecar timed out after {}s", dur.as_secs())),
                stdout: None,
                stderr: None,
                exit_code: None,
                timed_out: true,
            });
        }
    };

    let exit_code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    Ok(PkgSidecarCallResult {
        ok: output.status.success(),
        error: if output.status.success() {
            None
        } else {
            Some(format!(
                "exit code {}",
                exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "<signal>".into())
            ))
        },
        stdout: Some(stdout),
        stderr: Some(stderr),
        exit_code,
        timed_out: false,
    })
}

fn err(msg: String) -> PkgSidecarCallResult {
    PkgSidecarCallResult {
        ok: false,
        error: Some(msg),
        stdout: None,
        stderr: None,
        exit_code: None,
        timed_out: false,
    }
}

#[cfg(test)]
mod tests {
    // The only test here is unix-gated, so nothing consumes this elsewhere.
    #[cfg_attr(not(unix), allow(unused_imports))]
    use super::*;

    /// Sanity-check the spawn primitives that pkg_sidecar_call uses end-to-
    /// end against `/bin/echo`. Doesn't go through the Tauri command path
    /// (full `State<…>` mocking is heavier than the test value warrants);
    /// the registry-side tests in `pkg::registries::sidecars` already
    /// exercise `resolve()` and the Registry contract.
    #[cfg(unix)]
    #[tokio::test]
    async fn echo_spawn_captures_stdout() {
        let mut cmd = Command::new("/bin/echo");
        cmd.args(["hello", "world"]);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        let output = cmd.output().await.expect("echo run");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("hello world"), "stdout was: {stdout}");
    }
}
