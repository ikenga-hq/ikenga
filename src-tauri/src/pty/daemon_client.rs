//! Desktop PtyManager daemon client.
//!
//! Discovers a running `ikenga-server` daemon or launches one in a detached
//! process and waits up to [`SPAWN_READY_TIMEOUT`] for it to become healthy. Decouples PTY process lifecycle from the
//! desktop Tauri GUI window so terminal sessions survive app restarts and reloads.
//!
//! The daemon's bearer token opens a shell as this user. It is handed to the
//! daemon through the environment, never argv, and a running daemon is
//! adopted only when it is this app's version and accepts the token we hold
//! (see `init_daemon`, `server::discovery`).

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// How long a freshly spawned daemon gets to answer `/api/health`. This was
/// 500ms, which no cold start on Windows ever met: measured 574–1744ms for a
/// debug `ikenga-server.exe` (Defender scans the image on first exec), so every
/// launch fell back to ephemeral while the daemon came up a moment later. Only
/// paid when no daemon is already running.
const SPAWN_READY_TIMEOUT: Duration = Duration::from_millis(3000);

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

    /// Re-run [`init_daemon`] (find or spawn) and adopt what it found.
    /// G-ACCESS §2.5 (M-3): the desktop `access_*` proxy calls this once
    /// when the daemon stopped answering (it idles out after 60 s), before
    /// it gives up with `store_unavailable`. Blocking.
    pub fn reinit(&self, app_data_dir: Option<PathBuf>) -> DaemonInfo {
        let info = init_daemon(app_data_dir);
        self.set_info(info.clone());
        info
    }

    /// Send POST /api/shutdown to daemon.
    pub fn shutdown(&self) -> bool {
        let info = self.get_info();
        if !info.available || info.mode != "persistent" {
            return false;
        }
        if post_shutdown(&info.http_url, &info.token) {
            info!("Sent POST /api/shutdown to daemon at {}", info.http_url);
            true
        } else {
            warn!(
                "Failed to send POST /api/shutdown to daemon at {}",
                info.http_url
            );
            false
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

    let parent_target_debug =
        Path::new(manifest_dir)
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

/// The version this app expects its daemon to be. The daemon's `/api/health`
/// reports the same crate's `CARGO_PKG_VERSION`, so equal strings mean the
/// daemon was built from the release this app shipped with.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long to wait for an outdated daemon to exit after asking it to.
const SHUTDOWN_WAIT: Duration = Duration::from_millis(3000);

/// Probe health endpoint of daemon.
pub fn probe_health(http_url: &str, timeout_ms: u64) -> bool {
    health_version(http_url, timeout_ms).is_some()
}

/// `GET /api/health` → the reported `version` (empty if the field is absent).
/// `None` when nothing healthy answers.
fn health_version(http_url: &str, timeout_ms: u64) -> Option<String> {
    let health_url = format!("{http_url}/api/health");
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms))
        .build();
    let res = client.get(&health_url).call().ok()?;
    if res.status() != 200 {
        return None;
    }
    let json: serde_json::Value = res.into_json().unwrap_or_default();
    Some(
        json.get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    )
}

/// What a candidate daemon turned out to be.
#[derive(Debug, PartialEq, Eq)]
enum Probe {
    /// Nothing healthy answered.
    Down,
    /// Healthy, this app's version, and it accepts our token.
    Usable,
    /// Healthy but built from a different release.
    WrongVersion(String),
    /// Healthy and current, but our token doesn't open it. Somebody else's
    /// daemon — possibly another local user's, since 127.0.0.1:4000 is shared
    /// by every account on the machine.
    Unauthorized,
}

/// Decide whether the daemon at `http_url` is one this app may adopt with
/// `token`. `/api/health` is unauthenticated, so a 200 there proves only that
/// *a* daemon is listening; the token is checked separately against a
/// protected route.
fn probe_candidate(http_url: &str, token: &str, timeout_ms: u64) -> Probe {
    let Some(version) = health_version(http_url, timeout_ms) else {
        return Probe::Down;
    };
    if version != APP_VERSION {
        return Probe::WrongVersion(version);
    }
    if token.is_empty() || !token_accepted(http_url, token, timeout_ms) {
        return Probe::Unauthorized;
    }
    Probe::Usable
}

/// Whether `token` passes the daemon's auth middleware. Sends an RPC name no
/// handler implements, so nothing runs: the middleware answers 401 for a bad
/// token before dispatch, and anything past it means the token was accepted.
fn token_accepted(http_url: &str, token: &str, timeout_ms: u64) -> bool {
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(timeout_ms))
        .build();
    match client
        .post(&format!("{http_url}/api/rpc"))
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "cmd": "ikenga.auth_probe", "args": {} }))
    {
        Ok(_) => true,
        Err(ureq::Error::Status(code, _)) => code != 401 && code != 403,
        Err(ureq::Error::Transport(_)) => false,
    }
}

/// `POST /api/shutdown` with `token`. True if the daemon acknowledged.
fn post_shutdown(http_url: &str, token: &str) -> bool {
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_millis(1500))
        .build();
    client
        .post(&format!("{http_url}/api/shutdown"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .is_ok()
}

/// Ask an outdated daemon to exit and wait (bounded) until its port is free,
/// so a fresh one can bind it. Best-effort: if it doesn't go, the spawn below
/// fails to bind and the app falls back to ephemeral mode rather than
/// adopting a daemon from another release.
fn retire_outdated(http_url: &str, token: &str, version: &str) {
    info!("ikenga-server at {http_url} is v{version}, this app is v{APP_VERSION}; replacing it");
    if !post_shutdown(http_url, token) {
        warn!("outdated ikenga-server at {http_url} refused shutdown; it will keep the port");
        return;
    }
    let start = Instant::now();
    while start.elapsed() < SHUTDOWN_WAIT {
        if !probe_health(http_url, 100) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    warn!(
        "outdated ikenga-server at {http_url} did not exit within {}ms",
        SHUTDOWN_WAIT.as_millis()
    );
}

fn persistent_info(host: String, port: u16, token: String, pid: Option<u32>) -> DaemonInfo {
    DaemonInfo {
        available: true,
        http_url: format!("http://{host}:{port}"),
        ws_url: format!("ws://{host}:{port}"),
        host,
        port,
        token,
        pid,
        mode: "persistent".into(),
    }
}

/// Discovery files to try, most specific first. Only files `is_trusted`
/// accepts are ever read (owned by us, owner-only; see `server::discovery`).
fn candidate_metas(app_data_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = app_data_dir {
        v.push(dir.join("daemon/daemon.json"));
    }
    v.push(crate::server::discovery::user_temp_path());
    v
}

/// `IKENGA_PUBLIC_URL` from the desktop's environment, if set and non-empty.
fn public_url_from_env() -> Option<String> {
    std::env::var("IKENGA_PUBLIC_URL")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
}

/// The command that launches the detached daemon.
///
/// The bearer token travels in `IKENGA_AUTH_TOKEN`, never on the command line:
/// argv is world-readable (`/proc/<pid>/cmdline` on Linux, `ps` on macOS), and
/// this token opens a shell as the desktop user. A process's environment is
/// readable only by the same user (or an administrator), and `ikenga-server`
/// removes the variable from its own environment right after parsing, so PTY
/// children don't inherit it either.
fn build_daemon_command(
    binary: &Path,
    host: &str,
    port: u16,
    token: &str,
    daemon_dir: Option<&Path>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(binary);
    cmd.arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--idle-timeout")
        .arg("60")
        .env("IKENGA_AUTH_TOKEN", token);

    if let Some(dir) = daemon_dir {
        cmd.arg("--data-dir").arg(dir);
    }
    // G-ACCESS §3.3: the public base pairing QR codes point at, when the
    // desktop's own environment names one.
    if let Some(url) = public_url_from_env() {
        cmd.arg("--public-url").arg(url);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Detach child process into its own session so GUI window exits / reloads don't kill it
        cmd.process_group(0);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `ikenga-server.exe` is a console-subsystem binary: spawned from the GUI
        // app without CREATE_NO_WINDOW, Windows opens a console window for it.
        // CREATE_NEW_PROCESS_GROUP is the Windows analogue of `process_group(0)`
        // above — console Ctrl+C/Ctrl+Break aimed at us doesn't reach the daemon.
        cmd.creation_flags(crate::platform::DETACHED_PROCESS_FLAGS);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd
}

/// Discovers an already running daemon or launches one in a detached process
/// and waits up to [`SPAWN_READY_TIMEOUT`] for it.
///
/// A daemon is adopted only if it is this app's version **and** accepts the
/// token we hold for it. An outdated one (left running across an update) is
/// shut down and replaced; one we can't authenticate to is left alone.
pub fn init_daemon(app_data_dir: Option<PathBuf>) -> DaemonInfo {
    // 1. Check for existing running daemon metadata
    for meta_path in candidate_metas(app_data_dir.as_deref()) {
        if std::fs::symlink_metadata(&meta_path).is_err() {
            continue;
        }
        if !crate::server::discovery::is_trusted(&meta_path) {
            warn!(
                "ignoring daemon discovery file {} (a symlink, or not owned by this user)",
                meta_path.display()
            );
            continue;
        }
        if crate::server::discovery::tighten(&meta_path) {
            info!(
                "tightened {} to owner-only; it holds a bearer token",
                meta_path.display()
            );
        }
        let Ok(content) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
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

        match probe_candidate(&http_url, &token, 150) {
            Probe::Usable => {
                info!("Found healthy running ikenga-server daemon at {http_url}");
                return persistent_info(host, port, token, pid);
            }
            Probe::WrongVersion(version) => retire_outdated(&http_url, &token, &version),
            Probe::Unauthorized => {
                warn!(
                    "ikenga-server at {http_url} rejected the token from {}",
                    meta_path.display()
                );
            }
            Probe::Down => {}
        }
    }

    // A daemon on the default port with no discovery file: only a manually
    // started one, adoptable only with the token it was given in our env.
    // Without that token there is nothing to authenticate with, and adopting
    // it would leave every terminal failing with 401.
    let env_token = std::env::var("IKENGA_AUTH_TOKEN").unwrap_or_default();
    if !env_token.is_empty() {
        let http_url = "http://127.0.0.1:4000";
        match probe_candidate(http_url, &env_token, 150) {
            Probe::Usable => {
                info!("Found running ikenga-server daemon on port 4000 (no metadata file)");
                return persistent_info("127.0.0.1".into(), 4000, env_token, None);
            }
            Probe::WrongVersion(version) => retire_outdated(http_url, &env_token, &version),
            Probe::Unauthorized | Probe::Down => {}
        }
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

    let daemon_dir = app_data_dir.as_ref().map(|dir| dir.join("daemon"));
    if let Some(ref d) = daemon_dir {
        let _ = std::fs::create_dir_all(d);
    }
    let mut cmd = build_daemon_command(&binary, &host, port, &token, daemon_dir.as_deref());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to spawn ikenga-server daemon ({e}); falling back to ephemeral mode");
            return DaemonInfo::default();
        }
    };

    let pid = child.id();
    info!(
        "Spawned detached ikenga-server (pid {pid}) on {http_url}, awaiting readiness (timeout {}ms)...",
        SPAWN_READY_TIMEOUT.as_millis()
    );

    // Ready means OUR daemon answers: this version, accepting the token only
    // this call knows. A bare health 200 isn't enough — if the port is already
    // taken (another user's daemon, a stale one that wouldn't exit) our child
    // fails to bind while the other one keeps answering health checks.
    let start = Instant::now();
    while start.elapsed() < SPAWN_READY_TIMEOUT {
        if let Ok(Some(status)) = child.try_wait() {
            warn!("ikenga-server exited during startup ({status}); falling back to ephemeral in-process mode");
            return DaemonInfo::default();
        }
        if probe_candidate(&http_url, &token, 40) == Probe::Usable {
            info!(
                "ikenga-server daemon became ready in {}ms",
                start.elapsed().as_millis()
            );
            return persistent_info(host, port, token, Some(pid));
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    warn!(
        "ikenga-server daemon did not become healthy within {}ms; falling back to ephemeral in-process mode",
        SPAWN_READY_TIMEOUT.as_millis()
    );
    // Don't leave it running: it would come up on port 4000 holding a token only
    // this call knew, and the next launch's port-4000 probe would find a healthy
    // daemon it can't authenticate to.
    let _ = child.kill();
    DaemonInfo::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    const TEST_TOKEN: &str = "test-token-not-a-secret-7f3a";

    #[test]
    fn spawned_command_carries_the_token_in_env_not_argv() {
        let cmd = build_daemon_command(
            Path::new("/opt/ikenga/ikenga-server"),
            "127.0.0.1",
            4000,
            TEST_TOKEN,
            Some(Path::new("/tmp/ikenga-data/daemon")),
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.iter().any(|a| a.contains(TEST_TOKEN)),
            "the bearer token must not appear in the daemon's argv"
        );
        assert!(
            !args.iter().any(|a| a == "--auth-token"),
            "--auth-token must not be passed"
        );

        let env_token = cmd
            .get_envs()
            .find(|(k, _)| *k == "IKENGA_AUTH_TOKEN")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert!(
            env_token.as_deref() == Some(TEST_TOKEN),
            "IKENGA_AUTH_TOKEN must carry the token to the daemon"
        );
        // The rest of the invocation is unchanged.
        assert!(args.windows(2).any(|w| w[0] == "--port" && w[1] == "4000"));
        assert!(args.iter().any(|a| a == "--data-dir"));
    }

    /// A one-shot stand-in for `ikenga-server`: `/api/health` reports
    /// `version`, and `/api/rpc` answers 401 unless the bearer token matches.
    fn fake_daemon(version: &'static str, token: &'static str, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut authorized = false;
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if lower.starts_with("authorization:") {
                        authorized = line.trim_end().ends_with(&format!("Bearer {token}"));
                    }
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
                let (status, payload) = if request_line.starts_with("GET /api/health") {
                    (
                        "200 OK",
                        format!("{{\"ok\":true,\"version\":\"{version}\"}}"),
                    )
                } else if !authorized {
                    ("401 Unauthorized", "{}".to_string())
                } else {
                    (
                        "200 OK",
                        "{\"ok\":false,\"error\":\"unknown command\"}".to_string(),
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
            }
        });
        url
    }

    #[test]
    fn a_current_daemon_that_accepts_the_token_is_usable() {
        let url = fake_daemon(APP_VERSION, TEST_TOKEN, 2);
        assert_eq!(probe_candidate(&url, TEST_TOKEN, 2000), Probe::Usable);
    }

    #[test]
    fn a_daemon_from_another_release_is_not_adopted() {
        let url = fake_daemon("0.0.1-old", TEST_TOKEN, 1);
        assert_eq!(
            probe_candidate(&url, TEST_TOKEN, 2000),
            Probe::WrongVersion("0.0.1-old".into())
        );
    }

    #[test]
    fn a_daemon_that_rejects_our_token_is_not_adopted() {
        let url = fake_daemon(APP_VERSION, "somebody-elses-token", 2);
        assert_eq!(probe_candidate(&url, TEST_TOKEN, 2000), Probe::Unauthorized);
    }

    #[test]
    fn an_empty_token_is_never_usable() {
        let url = fake_daemon(APP_VERSION, TEST_TOKEN, 1);
        assert_eq!(probe_candidate(&url, "", 2000), Probe::Unauthorized);
    }

    #[test]
    fn nothing_listening_is_down() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            probe_candidate(&format!("http://127.0.0.1:{port}"), TEST_TOKEN, 300),
            Probe::Down
        );
    }

    #[cfg(unix)]
    #[test]
    fn candidate_metas_use_the_per_user_temp_path() {
        let metas = candidate_metas(Some(Path::new("/data")));
        assert_eq!(metas[0], Path::new("/data/daemon/daemon.json"));
        assert_eq!(metas[1], crate::server::discovery::user_temp_path());
        assert_ne!(metas[1], std::env::temp_dir().join("ikenga-daemon.json"));
    }
}
