//! IDE lock file manager — the discovery half of `claude --ide`.
//!
//! Claude Code discovers IDEs by scanning `~/.claude/ide/` for `<port>.lock`
//! files. The **file name is the port** and the transport is a WebSocket
//! speaking MCP; the body carries the fields the CLI reads. A real one, written
//! by the VS Code extension, looks like:
//!
//! ```json
//! { "pid": 3626265, "workspaceFolders": ["/home/me/proj"], "ideName": "Devin",
//!   "transport": "ws", "runningInWindows": false, "authToken": "…" }
//! ```
//!
//! Two things this module got wrong before ikenga#155 was reopened, both worth
//! keeping written down:
//!
//! * It wrote `{port, authToken, pid, lock_path}` — no `transport`, no
//!   `ideName`, no `workspaceFolders`. Claude Code cannot use that.
//! * It wrote with a plain `fs::write`, i.e. 0644, while the body carries the
//!   iyke bridge bearer token. `control.json` and the per-terminal hook
//!   settings both go through an explicit 0600 for exactly that reason.
//!
//! The server the lock points at lives in [`super::ide_ws`].

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use axum::{http::StatusCode, response::IntoResponse, Extension, Json};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

static ACTIVE_LOCK: OnceLock<Arc<Mutex<Option<IdeLockInfo>>>> = OnceLock::new();

fn get_lock_store() -> &'static Arc<Mutex<Option<IdeLockInfo>>> {
    ACTIVE_LOCK.get_or_init(|| Arc::new(Mutex::new(None)))
}

/// The on-disk lock body. Field names and shape are dictated by Claude Code —
/// do not rename or drop any of them. `port` and `lock_path` are deliberately
/// **not** serialized: the port is the file name, and a self-referential path
/// is not part of the format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeLockFile {
    pub pid: u32,
    #[serde(rename = "workspaceFolders")]
    pub workspace_folders: Vec<String>,
    #[serde(rename = "ideName")]
    pub ide_name: String,
    pub transport: String,
    #[serde(rename = "runningInWindows")]
    pub running_in_windows: bool,
    #[serde(rename = "authToken")]
    pub auth_token: String,
}

/// What we hand back to the caller — the lock body plus the two things the
/// file name and the filesystem carry rather than the JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdeLockInfo {
    pub port: u16,
    #[serde(rename = "authToken")]
    pub auth_token: String,
    pub pid: u32,
    pub lock_path: String,
}

/// The `ideName` we advertise. Also the marker that lets us reap our own stale
/// locks without touching another editor's.
pub const IDE_NAME: &str = "Ikenga";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFileParams {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

/// Writes `ide/<port>.lock` in the specified base directory (e.g. `~/.claude`).
///
/// Called from `iyke::start` with the LIVE bridge `port` and `token`, and
/// removed from `IykeRuntime::Drop` on app shutdown. This lets `claude --ide`
/// discover a running Ikenga bridge instead of advertising `port: 0` and a
/// placeholder token. See ikenga#155.
pub fn write_ide_lock_file(
    base_dir: &Path,
    port: u16,
    auth_token: &str,
    workspace_folders: Vec<String>,
) -> std::io::Result<IdeLockInfo> {
    let ide_dir = base_dir.join("ide");
    fs::create_dir_all(&ide_dir)?;

    let pid = std::process::id();
    let lock_path = ide_dir.join(format!("{}.lock", port));
    let body = IdeLockFile {
        pid,
        workspace_folders,
        ide_name: IDE_NAME.to_string(),
        transport: "ws".to_string(),
        running_in_windows: cfg!(target_os = "windows"),
        auth_token: auth_token.to_string(),
    };

    let content = serde_json::to_vec_pretty(&body)?;
    write_private(&lock_path, &content)?;

    let lock_info = IdeLockInfo {
        port,
        auth_token: auth_token.to_string(),
        pid,
        lock_path: lock_path.to_string_lossy().to_string(),
    };
    if let Ok(mut store) = get_lock_store().lock() {
        *store = Some(lock_info.clone());
    }

    Ok(lock_info)
}

/// 0600 — the body carries the iyke bridge bearer token. Same treatment as
/// `control.json` and the per-terminal hook settings file.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(data)
}

/// Remove `Ikenga` lock files left behind by a previous run whose process is
/// gone. `IykeRuntime::Drop` handles the graceful path, but it does not run on
/// SIGKILL or a crash, and a stale lock points `claude` at a dead port.
///
/// Only ever removes locks whose `ideName` is ours **and** whose pid is no
/// longer alive — another editor's lock, or a second live Ikenga, is left
/// alone.
pub fn reap_stale_locks(base_dir: &Path) -> usize {
    let ide_dir = base_dir.join("ide");
    let Ok(entries) = fs::read_dir(&ide_dir) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else { continue };
        let Ok(body) = serde_json::from_slice::<IdeLockFile>(&bytes) else {
            continue;
        };
        if body.ide_name != IDE_NAME || body.pid == std::process::id() {
            continue;
        }
        if process_is_alive(body.pid) {
            continue;
        }
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // Signal 0 performs the existence + permission check without delivering.
    // `== 0` alone is WRONG: for a live process we do not own, kill(2) fails
    // with EPERM, which would read as dead and reap a lock that is in use.
    // Only ESRCH means "no such process".
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    // Conservative on non-unix: never reap, rather than reap a live lock.
    true
}

/// Post route handler: POST /iyke/ide/open_file
pub async fn post_ide_open_file(
    Extension(app): Extension<AppHandle>,
    Json(params): Json<OpenFileParams>,
) -> impl IntoResponse {
    let _ = app.emit("ide://open_file", &params);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "opened": params.file_path
        })),
    )
}

/// Get route handler: GET /iyke/ide/lock
pub async fn get_ide_lock_status() -> impl IntoResponse {
    let lock = get_lock_store().lock().ok().and_then(|guard| guard.clone());

    (StatusCode::OK, Json(lock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// The shape is dictated by Claude Code. A real lock, written by the VS
    /// Code extension, carries exactly these keys — and notably does NOT carry
    /// `port` (the file name is the port) or `lock_path`. The previous version
    /// of this test asserted `raw.contains("12345")`, which passed only because
    /// the body wrongly serialized the port; it was locking in the defect.
    #[test]
    fn the_lock_body_matches_what_claude_code_reads() {
        let tmp = TempDir::new().expect("tempdir");
        let lock_info = write_ide_lock_file(
            tmp.path(),
            12345,
            "secret-token-777",
            vec!["/home/me/proj".to_string()],
        )
        .expect("write lock file");

        assert_eq!(lock_info.port, 12345);
        let path = PathBuf::from(&lock_info.lock_path);
        assert_eq!(
            path.file_name().unwrap(),
            "12345.lock",
            "port is the file name"
        );

        let body: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("parse");

        assert_eq!(body["transport"], "ws");
        assert_eq!(body["ideName"], IDE_NAME);
        assert_eq!(body["authToken"], "secret-token-777");
        assert_eq!(body["workspaceFolders"][0], "/home/me/proj");
        assert!(body["pid"].is_number());
        assert_eq!(body["runningInWindows"], cfg!(target_os = "windows"));

        assert!(
            body.get("port").is_none(),
            "the port is the file name, not a field"
        );
        assert!(
            body.get("lock_path").is_none(),
            "a self-referential path is not part of the format"
        );
    }

    /// The body carries the iyke bridge bearer token. It shipped as 0644 in the
    /// first cut of ikenga#155 because it used a plain `fs::write`.
    #[cfg(unix)]
    #[test]
    fn the_lock_is_private_because_it_carries_the_bridge_token() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().expect("tempdir");
        let info = write_ide_lock_file(tmp.path(), 4242, "tok", vec![]).expect("write");
        let mode = fs::metadata(&info.lock_path)
            .expect("stat")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600, "lock must not be group/world readable");
    }

    /// `Drop` does not run on SIGKILL, so a crashed run leaves a lock pointing
    /// at a dead port. Reaping must be narrow: ours, and only when it is dead.
    #[test]
    fn reaping_takes_our_dead_locks_and_nothing_else() {
        let tmp = TempDir::new().expect("tempdir");
        let ide_dir = tmp.path().join("ide");
        fs::create_dir_all(&ide_dir).expect("mkdir");

        let write = |name: &str, ide_name: &str, pid: u32| {
            let body = serde_json::json!({
                "pid": pid,
                "workspaceFolders": [],
                "ideName": ide_name,
                "transport": "ws",
                "runningInWindows": false,
                "authToken": "t",
            });
            fs::write(ide_dir.join(name), body.to_string()).expect("write");
        };

        // pid 1 is init: alive, and never us.
        write("1.lock", IDE_NAME, 1);
        write("2.lock", "Devin", 999_999_998);
        write("3.lock", IDE_NAME, 999_999_999);
        write("4.lock", IDE_NAME, std::process::id());
        fs::write(ide_dir.join("notalock.txt"), "x").expect("write");

        assert_eq!(reap_stale_locks(tmp.path()), 1, "only the dead Ikenga lock");

        assert!(ide_dir.join("1.lock").exists(), "a live Ikenga lock stays");
        assert!(
            ide_dir.join("2.lock").exists(),
            "another editor's lock is never touched"
        );
        assert!(!ide_dir.join("3.lock").exists(), "our dead lock is reaped");
        assert!(
            ide_dir.join("4.lock").exists(),
            "our own current lock stays"
        );
        assert!(ide_dir.join("notalock.txt").exists());
    }
}
