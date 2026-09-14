//! Chi session multiplexer — tmux-backed persistent runner (WP-09).
//!
//! Spawns a `chi-runner` daemon inside a named tmux session so the engine
//! child survives an Ikenga app restart. The tmux session is named after the
//! `run_id` and is visible to `iyke chi attach <run_id>`.
//!
//! Protocol:
//!   1. Write a `RunnerConf` JSON file to `<chi-cache-dir>/<run_id>.conf.json`.
//!   2. `tmux new-session -d -s <run_id> -e IKENGA_CHI_CONF=<path> chi-runner`
//!   3. chi-runner reads the conf, spawns the engine with Stdio::piped(), and
//!      writes output to `<chi-cache-dir>/<run_id>.json` (the standard output
//!      path that `chi_status` already polls).
//!   4. Store the tmux session name in `chi_cache.terminal_session_id`.
//!
//! Fallback: if tmux is absent or the spawn fails, the caller falls back to
//! the in-process background task (existing behaviour).

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use crate::platform::NoConsoleWindow;

/// Config written to disk and passed to chi-runner via IKENGA_CHI_CONF.
#[derive(Serialize)]
pub struct RunnerConf<'a> {
    pub run_id: &'a str,
    pub engine_id: &'a str,
    pub prompt: &'a str,
    pub cwd: &'a str,
    pub model: Option<&'a str>,
    pub mode: Option<&'a str>,
    pub resume_session_id: Option<&'a str>,
    pub output_path: &'a str,
    /// Seconds before chi-runner self-terminates.
    pub timeout_seconds: Option<u64>,
}

/// Outcome of a multiplexer spawn attempt.
pub enum SpawnResult {
    /// chi-runner launched in tmux session `session_name`.
    Ok { session_name: String },
    /// tmux not available or spawn failed. Caller should fall back.
    Unavailable { reason: String },
}

/// Probe whether `tmux` is available on PATH.
pub fn tmux_available() -> bool {
    // tmux isn't native to Windows, but if a port of it is on PATH (e.g. via
    // MSYS/Cygwin), this probe would otherwise flash a console window on
    // every daemon startup that checks it.
    Command::new("tmux")
        .arg("-V")
        .no_console_window()
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Spawn `chi-runner` inside a tmux session named after `run_id`.
///
/// Returns `SpawnResult::Ok { session_name }` on success so the caller can
/// persist `session_name` into `chi_cache.terminal_session_id`.
pub fn spawn_in_tmux(conf: &RunnerConf<'_>, cache_dir: &Path) -> SpawnResult {
    // Write runner conf file.
    let conf_path: PathBuf = cache_dir.join(format!("{}.conf.json", conf.run_id));
    let conf_json = match serde_json::to_string(conf) {
        Ok(j) => j,
        Err(e) => {
            return SpawnResult::Unavailable {
                reason: format!("serialize conf: {e}"),
            }
        }
    };
    if let Err(e) = std::fs::write(&conf_path, &conf_json) {
        return SpawnResult::Unavailable {
            reason: format!("write conf: {e}"),
        };
    }

    let session_name = conf.run_id.to_string();

    // Resolve chi-runner path — look next to the iyke binary first, then PATH.
    let runner_path = resolve_runner_path();

    let status = Command::new("tmux")
        .args([
            "new-session",
            "-d",                         // detached
            "-s", &session_name,          // session name = run_id
            "-e", &format!("IKENGA_CHI_CONF={}", conf_path.display()),
            &runner_path,
        ])
        .no_console_window()
        .status();

    match status {
        Ok(s) if s.success() => SpawnResult::Ok { session_name },
        Ok(s) => SpawnResult::Unavailable {
            reason: format!("tmux exited {}", s.code().unwrap_or(-1)),
        },
        Err(e) => SpawnResult::Unavailable {
            reason: format!("tmux spawn: {e}"),
        },
    }
}

/// Kill the tmux session for a run (used by chi_cancel when persistence is on).
pub fn kill_tmux_session(session_name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session_name])
        .no_console_window()
        .status();
}

/// Check whether a tmux session with this name is still alive.
pub fn session_alive(session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .no_console_window()
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn resolve_runner_path() -> String {
    // 1. Next to the current executable.
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("chi-runner");
        if sibling.exists() {
            return sibling.to_string_lossy().into_owned();
        }
    }
    // 2. Fall back to PATH.
    "chi-runner".to_string()
}
