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

use std::sync::Arc;

use tauri::State;

use crate::commands::pkg::KernelState;
use crate::executor::SpawnSpec;
use crate::pkg::registries::SidecarsRegistry;
pub use crate::server::shared::sidecar_call::PkgSidecarCallResult;

/// Tauri-state wrapper so commands can resolve sidecar paths without going
/// through the kernel snapshot.
pub struct SidecarsRegistryState(pub Arc<SidecarsRegistry>);

/// The spawn, stdin, timeout and capture live in
/// `server::shared::sidecar_call::run_one_shot`, which the daemon's
/// `pkg_sidecar_call` arm calls too (gap audit 2026-10-06 rank 21).
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

    tracing::info!(
        "[pkg_sidecar_call] pkg={pkg_id} name={name} bin={} args={:?}",
        entry.bin_path.display(),
        args
    );

    let (program, pre_args) = crate::runtime::sidecar_program(&entry.bin_path);
    let mut cmd = SpawnSpec::new(&program);
    cmd.args(&pre_args);
    cmd.args(&args);
    cmd.current_dir(&install_path);
    // WP-23 (D-18): hand this pkg its scoped database accessor —
    // `IKENGA_PKG_DB_URL` + a per-pkg `IKENGA_PKG_DB_TOKEN` good only for the
    // two `/iyke/pkg-db/*` routes, enforced against this pkg's own
    // `permissions["sqlite.tables"]`. See `pkg::db_scope`.
    crate::pkg::db_scope::inject_env(&mut cmd, &pkg_id, &install_path);

    Ok(
        crate::server::shared::sidecar_call::run_one_shot(
            cmd,
            &entry.bin_path,
            stdin,
            timeout_secs,
        )
        .await,
    )
}

fn err(msg: String) -> PkgSidecarCallResult {
    PkgSidecarCallResult::failed(msg)
}

#[cfg(test)]
mod tests {
    // The only test here is unix-gated, so nothing consumes these elsewhere.
    // (The call site itself now spawns through `executor::current()`, which
    // the executor's own tests cover; this keeps the raw-primitive check.)
    #[cfg_attr(not(unix), allow(unused_imports))]
    use std::process::Stdio;
    #[cfg_attr(not(unix), allow(unused_imports))]
    use tokio::process::Command;

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
