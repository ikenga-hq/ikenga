//! Desktop PtyManager daemon client.
//!
//! Discovers a running `ikenga-server` daemon or launches one in a detached
//! process with a 500ms timeout. Decouples PTY process lifecycle from the
//! desktop Tauri GUI window so terminal sessions survive app restarts and reloads.

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonInfo {
    pub available: bool,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub http_url: String,
    pub ws_url: String,
    pub pid: Option<u32>,
    /// "persistent" (daemon-backed) vs "ephemeral" (in-process fallback).
    pub mode: String,
}

impl Default for DaemonInfo {
    fn default() -> Self {
        Self {
            available: false,
            host: "127.0.0.1".into(),
            port: 4000,
            token: String::new(),
            http_url: String::new(),
            ws_url: String::new(),
            pid: None,
            mode: "ephemeral".into(),
        }
    }
}

pub struct DaemonState {
    info: RwLock<DaemonInfo>,
}

impl DaemonState {
    pub fn new(info: DaemonInfo) -> Self {
        Self {
            info: RwLock::new(info),
        }
    }

    pub fn get_info(&self) -> DaemonInfo {
        self.info.read().unwrap().clone()
    }

    pub fn set_info(&self, info: DaemonInfo) {
        *self.info.write().unwrap() = info;
    }

    /// Send POST /api/shutdown to daemon.
    pub fn shutdown(&self) -> bool {
        let info = self.get_info();
        if !info.available || info.mode != "persistent" {
            return false;
        }
        let shutdown_url = format!("{}/api/shutdown", info.http_url);
        let client = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(1500))
            .build();
        match client
            .post(&shutdown_url)
            .set("Authorization", &format!("Bearer {}", info.token))
            .call()
        {
            Ok(_) => {
                info!("Sent POST /api/shutdown to daemon at {}", info.http_url);
                true
            }
            Err(e) => {
                warn!("Failed to send POST /api/shutdown to daemon: {e}");
                false
            }
        }
    }
}

/// Locate the `ikenga-server` binary on disk.
pub fn find_daemon_binary() -> Option<PathBuf> {
    // 1. Check beside current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bin_name = if cfg!(windows) {
                "ikenga-server.exe"
            } else {
                "ikenga-server"
            };
            let candidate = dir.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
            // In dev mode (target/debug/ikenga-desktop)
            let candidate_sibling = dir.join("../server/target/debug").join(bin_name);
            if candidate_sibling.is_file() {
                return Some(candidate_sibling);
            }
        }
    }

    // 2. CARGO_MANIFEST_DIR if present (cargo run / dev)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let target_debug = Path::new(manifest_dir)
        .join("target/debug")
        .join(if cfg!(windows) {
            "ikenga-server.exe"
        } else {
            "ikenga-server"
        });
    if target_debug.is_file() {
        return Some(target_debug);
    }

    let parent_target_debug = Path::new(manifest_dir)
        .join("../target/debug")
        .join(if cfg!(windows) {
            "ikenga-server.exe"
        } else {
            "ikenga-server"
        });
    if parent_target_debug.is_file() {
        return Some(parent_target_debug);
    }

    // 3. Search PATH
    let bin_name = if cfg!(windows) {
        "ikenga-server.exe"
    } else {
        "ikenga-server"
    };
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Probe health endpoint of daemon.
pub fn probe_health(http_url: &str, timeout_ms: u64) -> bool {
    let health_url = format!("{http_url}/api/health");
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms))
        .build();
    match client.get(&health_url).call() {
        Ok(res) => res.status() == 200,
        Err(_) => false,
    }
}

/// Discovers an already running daemon or launches one in a detached process with a 500ms timeout.
pub fn init_daemon(app_data_dir: Option<PathBuf>) -> DaemonInfo {
    let temp_meta = std::env::temp_dir().join("ikenga-daemon.json");
    let candidate_metas = if let Some(ref dir) = app_data_dir {
        vec![dir.join("daemon/daemon.json"), temp_meta]
    } else {
        vec![temp_meta]
    };

    // 1. Check for existing running daemon metadata
    for meta_path in candidate_metas {
        if meta_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let host = json
                        .get("host")
                        .and_then(|v| v.as_str())
                        .unwrap_or("127.0.0.1")
                        .to_string();
                    let port = json.get("port").and_then(|v| v.as_u64()).unwrap_or(4000) as u16;
                    let token = json
                        .get("token")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let pid = json.get("pid").and_then(|v| v.as_u64()).map(|v| v as u32);
                    let http_url = format!("http://{host}:{port}");
                    let ws_url = format!("ws://{host}:{port}");

                    if probe_health(&http_url, 150) {
                        info!("Found healthy running ikenga-server daemon at {http_url}");
                        return DaemonInfo {
                            available: true,
                            host,
                            port,
                            token,
                            http_url,
                            ws_url,
                            pid,
                            mode: "persistent".into(),
                        };
                    }
                }
            }
        }
    }

    // Check default port 4000 just in case
    if probe_health("http://127.0.0.1:4000", 150) {
        info!("Found running ikenga-server daemon on port 4000 (no metadata file)");
        return DaemonInfo {
            available: true,
            host: "127.0.0.1".into(),
            port: 4000,
            token: std::env::var("IKENGA_AUTH_TOKEN").unwrap_or_default(),
            http_url: "http://127.0.0.1:4000".into(),
            ws_url: "ws://127.0.0.1:4000".into(),
            pid: None,
            mode: "persistent".into(),
        };
    }

    // 2. Launch detached daemon if binary exists
    let binary = match find_daemon_binary() {
        Some(b) => b,
        None => {
            warn!("ikenga-server binary not found; terminal falling back to ephemeral in-process mode");
            return DaemonInfo::default();
        }
    };

    let host = "127.0.0.1".to_string();
    let port = 4000u16;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let http_url = format!("http://{host}:{port}");
    let ws_url = format!("ws://{host}:{port}");

    let mut cmd = std::process::Command::new(&binary);
    cmd.arg("--host")
        .arg(&host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--auth-token")
        .arg(&token)
        .arg("--idle-timeout")
        .arg("60");

    if let Some(ref dir) = app_data_dir {
        let daemon_dir = dir.join("daemon");
        let _ = std::fs::create_dir_all(&daemon_dir);
        cmd.arg("--data-dir").arg(daemon_dir);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach child process into its own session so GUI window exits / reloads don't kill it
        cmd.process_group(0);
    }

    #[cfg(windows)]
    {
        // ikenga-server.exe is a console-subsystem binary with no
        // windows_subsystem attribute; spawned unadorned from this GUI
        // process it pops a visible console window that stays open for the
        // life of the detached daemon. CREATE_NEW_PROCESS_GROUP mirrors the
        // unix process_group(0) detach above so the daemon survives the GUI
        // window exiting/reloading.
        use crate::platform::{NoConsoleWindow, CREATE_NEW_PROCESS_GROUP};
        cmd.no_console_window_with(CREATE_NEW_PROCESS_GROUP);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to spawn ikenga-server daemon ({e}); falling back to ephemeral mode");
            return DaemonInfo::default();
        }
    };

    let pid = child.id();
    info!("Spawned detached ikenga-server (pid {pid}) on {http_url}, awaiting readiness (timeout 500ms)...");

    // Poll health endpoint for up to 500ms
    let start = Instant::now();
    let timeout = Duration::from_millis(500);
    while start.elapsed() < timeout {
        if probe_health(&http_url, 40) {
            info!(
                "ikenga-server daemon became ready in {}ms",
                start.elapsed().as_millis()
            );
            return DaemonInfo {
                available: true,
                host,
                port,
                token,
                http_url,
                ws_url,
                pid: Some(pid),
                mode: "persistent".into(),
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    warn!("ikenga-server daemon did not become healthy within 500ms timeout; falling back to ephemeral in-process mode");
    DaemonInfo::default()
}
