//! Iyke localhost control bridge (Phase 11).
//!
//! App-lifetime axum server bound to `127.0.0.1:0` with a per-launch bearer
//! token. The token + port are written to a known control file so the
//! external `iyke` CLI (Day 2) and MCP server (Day 3) can find us. The
//! in-app frontend reads the same fields via the `iyke_endpoint` Tauri
//! command and goes through the HTTP server like any other client — that
//! way the contract is identical for every caller.
//!
//! Day 1 ships the server + auth + read-only `GET /iyke/state`. Write-side
//! handlers (go, mode, open, split, focus, close) and the CLI/MCP packages
//! land in Day 2/3.

pub mod auth;
pub mod browser_handlers;
pub mod browser_rpc;
pub mod browser_sessions;
pub mod claude;
pub mod comments;
pub mod handlers;
pub mod hooks;
pub mod layout;
pub mod mcp;
pub mod memory;
pub mod pa_actions;
pub mod permissions_audit;
pub mod pkg_dispatch;
pub mod playwright_proxy;
pub mod projects;
pub mod rpc;
pub mod secrets;
pub mod server;
pub mod state;
pub mod statusline;
pub mod tasks;
pub mod terminal;
pub mod trust;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::AppHandle;
use tokio::sync::oneshot;

pub use browser_rpc::BrowserRpc;
pub use rpc::{new_pending, Pending};
pub use state::IykeState;

/// FE round-trip RPC registries. Bundled so handlers can claim a single
/// `Extension<IykeRpc>` instead of three separate ones.
#[derive(Clone)]
pub struct IykeRpc {
    pub dom: Pending<handlers::DomResult>,
    pub query_cache: Pending<handlers::QueryCacheResult>,
    pub wait: Pending<handlers::WaitResult>,
    pub terminal_read: Pending<handlers::TerminalReadResult>,
    /// Click/type/key match round-trips. Shared across the three action verbs
    /// (each request_id is a UUID, so concurrent in-flight actions coexist).
    pub action: Pending<handlers::ActionResult>,
    /// Terminal spawn/kill round-trips. Distinct from `action` because the
    /// frontend has to hand back the `terminal_id` it minted — `ActionResult`
    /// only carries a match flag. Per WP-08 the frontend owns terminal
    /// creation (it holds the session store), so spawn cannot be Rust-local.
    pub terminal_spawn: Pending<terminal::TerminalSpawnResult>,
}

impl IykeRpc {
    pub fn new() -> Self {
        Self {
            dom: new_pending(),
            query_cache: new_pending(),
            wait: new_pending(),
            terminal_read: new_pending(),
            action: new_pending(),
            terminal_spawn: new_pending(),
        }
    }
}

impl Default for IykeRpc {
    fn default() -> Self {
        Self::new()
    }
}

/// Endpoint info shared with the in-app frontend via `iyke_endpoint`.
/// Same fields external callers find in `control.json`.
#[derive(Clone, Serialize)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
    pub port: u16,
}

/// Live runtime handle. Held by Tauri-managed state so its `Drop` impl
/// fires on app shutdown — that's what cleans up `control.json` and
/// signals the server to exit.
pub struct IykeRuntime {
    pub url: String,
    pub token: String,
    pub port: u16,
    pub control_path: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
}

impl IykeRuntime {
    pub fn endpoint(&self) -> Endpoint {
        Endpoint {
            url: self.url.clone(),
            token: self.token.clone(),
            port: self.port,
        }
    }
}

impl Drop for IykeRuntime {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if self.control_path.exists() {
            match std::fs::remove_file(&self.control_path) {
                Ok(()) => log::info!("iyke: removed {}", self.control_path.display()),
                Err(e) => {
                    log::warn!(
                        "iyke: failed to remove {}: {e}",
                        self.control_path.display()
                    )
                }
            }
        }
    }
}

/// Spawn the Iyke server and write the control file. The `state` arg is
/// the same `Arc<IykeState>` Tauri exposes to the frontend via
/// `iyke_set_shell` — both the server and the FE update path share it.
/// `app_handle` is what write-side handlers use to emit Tauri events
/// the FE listener picks up.
pub async fn start(
    state: Arc<IykeState>,
    rpc: IykeRpc,
    browser_rpc: BrowserRpc,
    webview_panes: Arc<crate::pkg::webview::WebviewPanesRegistry>,
    playwright_proxy: Arc<playwright_proxy::PlaywrightProxy>,
    pa_db: Arc<crate::commands::db::PaDb>,
    pty_manager: Arc<crate::pty::PtyManager>,
    control_path: PathBuf,
    app_handle: AppHandle,
    screenshot_pending: crate::commands::ScreenshotPending,
    iyke_routes: Arc<crate::pkg::registries::IykeRoutesRegistry>,
    timer_scheduler: memory::TimerScheduler,
) -> Result<IykeRuntime> {
    let token = auth::random_token_hex(32);

    let (url, port, shutdown) = server::serve(
        state.clone(),
        rpc,
        browser_rpc,
        webview_panes,
        playwright_proxy,
        pa_db,
        pty_manager,
        token.clone(),
        app_handle,
        screenshot_pending,
        iyke_routes,
        timer_scheduler,
    )
    .await
    .context("start iyke server")?;

    write_control_file(&control_path, port, &token, state.started_at_unix_ms())
        .with_context(|| format!("write {}", control_path.display()))?;

    log::info!("iyke: listening on {url} (token {}…)", &token[..8]);
    log::info!("iyke: wrote {}", control_path.display());

    Ok(IykeRuntime {
        url,
        token,
        port,
        control_path,
        shutdown: Some(shutdown),
    })
}

#[derive(Serialize)]
struct ControlFile<'a> {
    schema_version: u32,
    port: u16,
    token: &'a str,
    pid: u32,
    started_at_unix_ms: u128,
    identifier: &'static str,
}

fn write_control_file(path: &Path, port: u16, token: &str, started_at_unix_ms: u128) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create control file parent dir")?;
    }

    let payload = ControlFile {
        schema_version: 1,
        port,
        token,
        pid: std::process::id(),
        started_at_unix_ms,
        identifier: "app.ikenga",
    };
    let json = serde_json::to_vec_pretty(&payload).context("serialize control.json")?;

    // Atomic replace: write to a sibling tmp file with mode 0600, then
    // rename. POSIX rename is atomic when src and dst are on the same
    // filesystem (always true here — both inside app_local_data_dir).
    let tmp = path.with_extension("json.tmp");
    write_file_secure(&tmp, &json)?;
    std::fs::rename(&tmp, path).context("rename control.json")?;
    Ok(())
}

#[cfg(unix)]
fn write_file_secure(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .context("open control.json with mode 0600")?;
    f.write_all(data).context("write control.json")?;
    Ok(())
}

#[cfg(not(unix))]
fn write_file_secure(path: &Path, data: &[u8]) -> Result<()> {
    // Windows isn't a v1 target; fall back to a plain write. The token
    // file is still in the user-scoped local data dir.
    std::fs::write(path, data).context("write control.json")?;
    Ok(())
}
