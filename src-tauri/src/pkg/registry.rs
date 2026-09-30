//! Registry trait — every "thing a package can contribute" implements this.
//!
//! The kernel holds a `Vec<Box<dyn Registry>>` and walks it during install /
//! uninstall / boot. Each registry owns its own in-memory state (typically
//! behind an `RwLock`) and is responsible for its own idempotency.
//!
//! Naming convention: registry names are short snake_case strings used in
//! logs and rollback diagnostics ("sidecars", "iyke_routes", "cron_jobs",
//! ...). Keep them stable — they appear in diagnostics commands.

use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use super::manifest::Package;

/// One registry per "thing a package contributes." Implementations live in
/// `pkg/registries/`. Implementors must be `Send + Sync` so the kernel can
/// share them across the Tauri runtime.
pub trait Registry: Send + Sync {
    /// Stable short name used in logs and rollback. Must not change across
    /// versions — uninstall finds entries by this name.
    fn name(&self) -> &'static str;

    /// Apply this package's contribution to the registry. Must be idempotent:
    /// re-running with the same package leaves the registry in the same
    /// state. (The kernel calls `register()` again at boot for every
    /// previously-installed enabled package.)
    fn register(&self, pkg: &Package) -> Result<()>;

    /// Remove this package's contribution. Must be a no-op if the package
    /// was never registered.
    fn unregister(&self, pkg_id: &str) -> Result<()>;

    /// Stop every OS process this registry started for `pkg_id` and block
    /// until each has actually exited (or `deadline` passes). `unregister`
    /// only has to *signal*; this is the barrier the kernel runs before it
    /// touches the pkg's folder (uninstall's retire, a reinstall's backup
    /// rename) — on Windows a child still exiting holds the folder open.
    /// Also unregisters, so it is safe to call without a prior `unregister`.
    /// Default: nothing to wait for. `Err` means a process may still be
    /// alive; `lifecycle::StillRunning` is downcastable for the pids.
    fn quiesce(&self, pkg_id: &str, deadline: Duration) -> Result<()> {
        let _ = (pkg_id, deadline);
        Ok(())
    }

    /// Diagnostic snapshot of the registry's current state. Surfaced via
    /// `pkg_kernel_status` for debugging.
    fn snapshot(&self) -> Value;
}
