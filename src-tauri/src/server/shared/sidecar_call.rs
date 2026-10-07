//! One-shot pkg sidecar spawn behind `pkg_sidecar_call`: pipe stdin, wait
//! with a timeout, capture stdout / stderr / exit code. Shared by the
//! desktop command (`commands::pkg_sidecar`, which resolves the binary
//! through the kernel's `SidecarsRegistry`) and the daemon's arm
//! (`server::rpc_exec`, which resolves it from its read-only `--pkgs-dir`
//! index — gap audit 2026-10-06 rank 21), so the two surfaces return the
//! same JSON for the same run.
//!
//! The caller builds the [`SpawnSpec`] (program, args, cwd, env); this
//! spawns it through `executor::current()` — under T1, inside the signed-in
//! principal's child, as that principal's uid.

use std::path::Path;

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::time::{timeout, Duration};

use crate::executor::{PipedOpts, SpawnSpec, StdioMode};

#[derive(Serialize, Debug)]
pub struct PkgSidecarCallResult {
    pub ok: bool,
    pub error: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

impl PkgSidecarCallResult {
    /// A call that never produced a run: `ok: false` with `msg`, no output.
    pub fn failed(msg: impl Into<String>) -> Self {
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

/// Default timeout for one-shot sidecar invocations. Pollers/sends should
/// finish in well under a minute; 120s gives slow networks headroom without
/// letting a hung process pin a worker forever.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Spawn `spec` (whose binary is `bin`, for error text), write `stdin` if
/// given (else close it at once), and wait up to `timeout_secs`. A timeout
/// kills the child (`kill_on_drop`) and reports `timed_out: true`.
pub async fn run_one_shot(
    spec: SpawnSpec,
    bin: &Path,
    stdin: Option<String>,
    timeout_secs: Option<u64>,
) -> PkgSidecarCallResult {
    let opts = PipedOpts {
        stdin: StdioMode::Piped,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        kill_on_drop: true,
        no_console_window: true,
        detached: false,
        new_process_group: false,
    };

    let mut child = match crate::executor::current().spawn_piped(spec, opts) {
        Ok(c) => c,
        Err(e) => return PkgSidecarCallResult::failed(format!("spawn `{}`: {e}", bin.display())),
    };

    // Pipe stdin if provided, then drop the writer so the child sees EOF.
    if let Some(payload) = stdin {
        if let Some(mut stdin_handle) = child.stdin.take() {
            if let Err(e) = stdin_handle.write_all(payload.as_bytes()).await {
                // Best-effort: kill the child and return the write error.
                let _ = child.start_kill();
                return PkgSidecarCallResult::failed(format!("write stdin: {e}"));
            }
            if let Err(e) = stdin_handle.shutdown().await {
                tracing::warn!("[pkg_sidecar_call] stdin shutdown: {e}");
            }
        }
    } else {
        // Drop stdin handle immediately so the child sees EOF.
        drop(child.stdin.take());
    }

    let dur = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));
    let output = match timeout(dur, child.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return PkgSidecarCallResult::failed(format!("wait: {e}")),
        Err(_) => {
            return PkgSidecarCallResult {
                timed_out: true,
                ..PkgSidecarCallResult::failed(format!(
                    "sidecar timed out after {}s",
                    dur.as_secs()
                ))
            };
        }
    };

    let exit_code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    PkgSidecarCallResult {
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
    }
}

/// Host's rust target triple. Hard-coded per-OS arms cover what the host
/// supports today; wrong arch hits the catch-all and surfaces as an install
/// error rather than silently accepting an incompatible binary.
pub fn host_target_triple() -> String {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu".into()
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu".into()
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin".into()
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin".into()
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc".into()
    } else {
        "unknown".into()
    }
}
