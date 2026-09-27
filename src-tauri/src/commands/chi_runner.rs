//! Detached `chi-runner` launch, liveness and cancel (WP-18b, ADR-023 D4/D5).
//!
//! A persistent Chi run is a `chi-runner` process spawned **detached** through
//! the executor (`PipedOpts::detached`): its own process group, no PTY, not
//! killed with its handle. It outlives an app (or daemon) restart; the app
//! finds out what happened to it from two things only:
//!
//!   * the pid recorded in `chi_cache.pid`, and
//!   * the status file chi-runner writes at the run's `output_path` —
//!     `{ output, error, done_at, status, external_id }`, rewritten after
//!     every engine line and once more at the end.
//!
//! Protocol:
//!   1. Write a [`RunnerConf`] JSON file to `<chi-cache-dir>/<run_id>.conf.json`.
//!   2. Spawn `chi-runner` detached with `IKENGA_CHI_CONF=<that path>`.
//!   3. chi-runner reads the conf, spawns the engine with piped stdio and
//!      writes the status file.
//!   4. The caller stores the pid in `chi_cache.pid`; the reconciliation
//!      sweep (`chi::reconcile_detached_runs`) folds the file + pid into the
//!      row's terminal status.
//!
//! This replaces the tmux multiplexer (`terminal/multiplexer.rs`, retired
//! here): a detached run has no pane to attach to.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::executor::{PipedOpts, SpawnSpec, StdioMode};

/// The runner vocabulary, pid probe and liveness decision live in the
/// ungated `server::shared::chi_liveness` (WP-19 slice 3: the daemon's
/// `chi_status` reads with the same decision); re-exported so every
/// non-test `chi_runner::…` call site resolves unchanged.
pub(crate) use crate::server::shared::chi_liveness::{
    decide_liveness, probe_runner, PidProbe, RunLiveness, RUNNER_BINARY,
};

/// Config written to disk and passed to chi-runner via `IKENGA_CHI_CONF`.
/// Field-for-field what `chi_runner.rs::RunnerConf` deserialises.
#[derive(Serialize)]
pub(crate) struct RunnerConf<'a> {
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

/// chi-runner next to the current executable (how the bundle ships it), else
/// on the augmented `PATH`. `None` when neither has it — the caller falls back
/// to an in-process run.
pub(crate) fn resolve_runner_path() -> Option<PathBuf> {
    let file_name = format!("{RUNNER_BINARY}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(&file_name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    which::which_in(RUNNER_BINARY, Some(crate::runtime::augmented_path()), ".").ok()
}

/// Write the conf and spawn chi-runner detached through the executor. Returns
/// the runner's pid. The `Child` handle is dropped without waiting — the
/// executor turns `kill_on_drop` off for a detached spawn, so that doesn't end
/// the run. `Err` carries the reason the caller logs before falling back.
pub(crate) fn spawn_detached_runner(
    conf: &RunnerConf<'_>,
    cache_dir: &Path,
) -> Result<u32, String> {
    let runner = resolve_runner_path().ok_or_else(|| format!("{RUNNER_BINARY} not found"))?;

    let conf_path = cache_dir.join(format!("{}.conf.json", conf.run_id));
    let conf_json = serde_json::to_string(conf).map_err(|e| format!("serialize conf: {e}"))?;
    std::fs::write(&conf_path, conf_json).map_err(|e| format!("write conf: {e}"))?;

    let mut spec = SpawnSpec::new(&runner);
    spec.env("IKENGA_CHI_CONF", &conf_path)
        // chi-runner resolves the engine CLI (`claude`, `codex`, `agy`) on its
        // own PATH; give it the same augmented PATH the in-process spawns use,
        // or a GUI-launched app hands it a PATH without nvm / Homebrew bins.
        .env("PATH", crate::runtime::augmented_path());
    let opts = PipedOpts {
        stdin: StdioMode::Null,
        stdout: StdioMode::Null,
        stderr: StdioMode::Null,
        kill_on_drop: false,
        no_console_window: true,
        detached: true,
        new_process_group: false,
    };
    let child = crate::executor::current()
        .spawn_piped(spec, opts)
        .map_err(|e| format!("spawn {}: {e}", runner.display()))?;
    let pid = child
        .id()
        .ok_or_else(|| format!("{RUNNER_BINARY} exited before its pid was read"))?;
    drop(child);
    Ok(pid)
}

// ─── cancel ──────────────────────────────────────────────────────────────────

/// How long a detached run gets to exit on SIGTERM before SIGKILL.
pub(crate) const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Kill a detached run's whole tree. Unix: SIGTERM to the process group
/// (`-pgid`; the runner leads its own group), a bounded wait, then SIGKILL.
/// An already-empty group is success.
///
/// The wait ends early only once no process in the group remains. An
/// unreaped zombie still counts, so a group whose leader nobody has reaped
/// yet waits out `grace` and gets a SIGKILL that finds only zombies — bounded,
/// and harmless.
#[cfg(unix)]
pub(crate) async fn kill_process_group(
    pgid: u32,
    grace: std::time::Duration,
) -> Result<(), String> {
    let pgid = i32::try_from(pgid).map_err(|_| format!("pid {pgid} out of range"))?;
    if pgid <= 1 {
        return Err(format!("refusing to signal process group {pgid}"));
    }
    let signal = |sig: i32| -> Result<bool, String> {
        if unsafe { libc::kill(-pgid, sig) } == 0 {
            return Ok(true);
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(format!("signal {sig} to process group {pgid}: {err}"))
        }
    };
    if !signal(libc::SIGTERM)? {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if !signal(0)? {
            return Ok(());
        }
    }
    signal(libc::SIGKILL).map(|_| ())
}

/// Windows: `taskkill /T /F` ends the runner and every descendant.
#[cfg(windows)]
pub(crate) async fn kill_process_group(
    pid: u32,
    _grace: std::time::Duration,
) -> Result<(), String> {
    use crate::platform::NoConsoleWindow;
    let status = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .no_console_window()
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| format!("taskkill: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("taskkill /PID {pid} exited {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pid of the first line a child prints.
    #[cfg(unix)]
    async fn first_line_pid(child: &mut tokio::process::Child) -> i32 {
        use tokio::io::AsyncBufReadExt;
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        tokio::io::BufReader::new(stdout)
            .read_line(&mut line)
            .await
            .unwrap();
        line.trim().parse().unwrap()
    }

    /// A process's kernel start time (`/proc/<pid>/stat` field 22), so a
    /// later check can tell the same process from a recycled pid — `pid_max`
    /// is 32768 in CI containers and this suite spawns thousands of children.
    #[cfg(unix)]
    fn start_time(pid: i32) -> Option<u64> {
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let (_, rest) = stat.rsplit_once(')')?;
            // `rest` starts at field 3 (state), so field 22 is index 19.
            rest.split_whitespace().nth(19)?.parse().ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = pid;
            None
        }
    }

    /// Gone, a zombie, or the pid now belongs to a different process.
    #[cfg(unix)]
    fn gone_or_zombie(pid: i32, started: Option<u64>) -> bool {
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => true,
                Ok(stat) => {
                    let Some((_, rest)) = stat.rsplit_once(')') else {
                        return false;
                    };
                    let rest = rest.trim_start();
                    if rest.starts_with('Z') {
                        return true;
                    }
                    let now = rest.split_whitespace().nth(19).and_then(|t| t.parse().ok());
                    started.is_some() && now != started
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = started;
            unsafe { libc::kill(pid, 0) != 0 }
        }
    }

    #[cfg(unix)]
    async fn eventually(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if done() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        done()
    }

    #[cfg(unix)]
    fn detached_sh(script: &str) -> tokio::process::Child {
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c").arg(script);
        crate::executor::current()
            .spawn_piped(
                spec,
                PipedOpts {
                    stdin: StdioMode::Null,
                    stdout: StdioMode::Piped,
                    stderr: StdioMode::Null,
                    kill_on_drop: false,
                    no_console_window: true,
                    detached: true,
                    new_process_group: false,
                },
            )
            .unwrap()
    }

    /// Cancel kills the whole detached group — the leader and a background
    /// grandchild — on SIGTERM.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_process_group_kills_a_detached_tree() {
        let mut child = detached_sh("sleep 30 & echo $!; sleep 30");
        let leader = child.id().unwrap() as i32;
        let grandchild = first_line_pid(&mut child).await;
        assert_eq!(unsafe { libc::getpgid(grandchild) }, leader);
        let (leader_start, grandchild_start) = (start_time(leader), start_time(grandchild));

        kill_process_group(leader as u32, CANCEL_GRACE)
            .await
            .unwrap();
        let _ = child.wait().await;
        assert!(
            eventually(|| gone_or_zombie(grandchild, grandchild_start)).await,
            "grandchild survived"
        );
        assert!(gone_or_zombie(leader, leader_start), "leader survived");
    }

    /// A group that ignores SIGTERM is SIGKILLed once the grace runs out.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_process_group_escalates_to_sigkill() {
        let mut child = detached_sh("trap '' TERM; sleep 30 & echo $!; wait");
        let leader = child.id().unwrap() as i32;
        let grandchild = first_line_pid(&mut child).await;
        let (leader_start, grandchild_start) = (start_time(leader), start_time(grandchild));

        kill_process_group(leader as u32, std::time::Duration::from_millis(300))
            .await
            .unwrap();
        let _ = child.wait().await;
        assert!(
            eventually(|| gone_or_zombie(grandchild, grandchild_start)).await,
            "grandchild survived"
        );
        assert!(gone_or_zombie(leader, leader_start), "leader survived");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_process_group_on_an_empty_group_is_ok() {
        let mut child = detached_sh("exit 0");
        let pid = child.id().unwrap();
        child.wait().await.unwrap();
        kill_process_group(pid, CANCEL_GRACE).await.unwrap();
        assert!(kill_process_group(1, CANCEL_GRACE).await.is_err());
    }
}
