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

/// The runner binary's file stem.
pub(crate) const RUNNER_BINARY: &str = "chi-runner";

/// Recorded when the runner is gone but its status file never reached a
/// terminal status (crash, SIGKILL, OOM, a reboot mid-run).
pub(crate) const RUNNER_EXITED_ERROR: &str = "chi-runner exited without writing a terminal status";

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

// ─── liveness ────────────────────────────────────────────────────────────────

/// What a pid probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidProbe {
    /// No such process (or, on Linux, a zombie: it has exited).
    Dead,
    /// Running, and its executable / command line names the runner.
    Ours,
    /// Running, but it is something else — the pid was reused.
    Foreign,
    /// Running, but the platform wouldn't say what it is (`EPERM`, an
    /// unreadable image path). Counted as alive; never signalled.
    Unverified,
}

impl PidProbe {
    /// Whether the run should still be considered running on this probe.
    pub(crate) fn alive(self) -> bool {
        matches!(self, PidProbe::Ours | PidProbe::Unverified)
    }
}

/// A run's state as far as the pid and its status file can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunLiveness {
    Running,
    /// `status` is a `chi_cache` status: `done` / `failed` / `cancelled`.
    Terminal {
        status: &'static str,
        error: Option<String>,
    },
}

/// Map chi-runner's status vocabulary onto `chi_cache`'s terminal statuses.
///
/// chi-runner writes `running` while it works and exactly one of `done`,
/// `failed` or `timed_out` at the end (`iyke-cli/src/chi_runner.rs::run`).
/// `timed_out` has no `chi_cache` counterpart and is a failure (the file's
/// `error` says "timed out after Ns"). `cancelled` isn't written by today's
/// runner but is terminal if a later one does.
pub(crate) fn runner_terminal_status(status: &str) -> Option<&'static str> {
    match status {
        "done" => Some("done"),
        "failed" | "timed_out" => Some("failed"),
        "cancelled" => Some("cancelled"),
        _ => None,
    }
}

/// The liveness decision (G-88). Pure.
///
/// * The file names a terminal status → that status (and the file's error).
/// * Otherwise, the runner is gone → `failed`, [`RUNNER_EXITED_ERROR`].
/// * Otherwise → still running.
///
/// `file_status` is `None` when the file is missing or unparseable — a
/// half-written file is "no information yet", never a verdict. Callers probe
/// the pid **before** reading the file: chi-runner writes its last status
/// before it exits, so a pid seen dead guarantees the file read after it is
/// the final one.
pub(crate) fn decide_liveness(
    pid_alive: bool,
    file_status: Option<&str>,
    file_error: Option<&str>,
) -> RunLiveness {
    if let Some(status) = file_status.and_then(runner_terminal_status) {
        let error = file_error.map(str::to_string).or_else(|| {
            (status == "failed")
                .then(|| format!("chi-runner reported {}", file_status.unwrap_or("failed")))
        });
        return RunLiveness::Terminal { status, error };
    }
    if !pid_alive {
        return RunLiveness::Terminal {
            status: "failed",
            error: Some(RUNNER_EXITED_ERROR.to_string()),
        };
    }
    RunLiveness::Running
}

/// Probe a recorded runner pid.
pub(crate) fn probe_runner(pid: u32) -> PidProbe {
    probe_pid(pid, RUNNER_BINARY)
}

/// Probe `pid`, corroborating that it is still the process named `needle`
/// where the platform lets us (defeats pid reuse).
#[cfg(unix)]
pub(crate) fn probe_pid(pid: u32, needle: &str) -> PidProbe {
    let Ok(pid) = i32::try_from(pid) else {
        return PidProbe::Dead;
    };
    if pid <= 0 {
        return PidProbe::Dead;
    }
    // Signal 0: existence + permission check, nothing is delivered. EPERM
    // means it exists but belongs to someone else.
    if unsafe { libc::kill(pid, 0) } != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        if errno != Some(libc::EPERM) {
            return PidProbe::Dead;
        }
    }
    identify(pid, needle)
}

#[cfg(target_os = "linux")]
fn identify(pid: i32, needle: &str) -> PidProbe {
    match std::fs::read(format!("/proc/{pid}/cmdline")) {
        // `hidepid` hides other users' processes; this one exists (the signal
        // probe said so) but won't say what it is.
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => PidProbe::Unverified,
        // Gone between the two reads.
        Err(_) => PidProbe::Dead,
        // An empty cmdline means one of two things: a zombie (it has exited,
        // only the reap is left) or a process caught mid-`execve` — the vfork
        // parent resumes before the new image's argv is mapped, so a probe
        // right after spawn can land here. Only the state tells them apart;
        // a live process we can't name yet is never signalled (not `Ours`),
        // but it must not read as dead either.
        Ok(bytes) if bytes.is_empty() => match proc_state(pid) {
            Some('Z') | Some('X') | None => PidProbe::Dead,
            Some(_) => PidProbe::Unverified,
        },
        Ok(bytes) => {
            if String::from_utf8_lossy(&bytes).contains(needle) {
                PidProbe::Ours
            } else {
                PidProbe::Foreign
            }
        }
    }
}

/// The one-letter state from `/proc/<pid>/stat` (`R`, `S`, `Z`, …).
#[cfg(target_os = "linux")]
fn proc_state(pid: i32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    rest.trim_start().chars().next()
}

#[cfg(target_os = "macos")]
fn identify(pid: i32, needle: &str) -> PidProbe {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return PidProbe::Unverified;
    }
    if String::from_utf8_lossy(&buf[..len as usize]).contains(needle) {
        PidProbe::Ours
    } else {
        PidProbe::Foreign
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn identify(_pid: i32, _needle: &str) -> PidProbe {
    PidProbe::Unverified
}

#[cfg(windows)]
pub(crate) fn probe_pid(pid: u32, needle: &str) -> PidProbe {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const ERROR_ACCESS_DENIED: i32 = 5;

    if pid == 0 {
        return PidProbe::Dead;
    }
    // OWED (Windows live check, WP-18b): compiled against windows-sys 0.60's
    // signatures but not run on a Windows host.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED) {
            PidProbe::Unverified
        } else {
            PidProbe::Dead
        };
    }
    let mut code: u32 = 0;
    let probe = if unsafe { GetExitCodeProcess(handle, &mut code) } == 0 {
        PidProbe::Unverified
    } else if code != STILL_ACTIVE as u32 {
        PidProbe::Dead
    } else {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        if ok == 0 {
            PidProbe::Unverified
        } else if String::from_utf16_lossy(&buf[..len as usize])
            .to_lowercase()
            .contains(&needle.to_lowercase())
        {
            PidProbe::Ours
        } else {
            PidProbe::Foreign
        }
    };
    unsafe { CloseHandle(handle) };
    probe
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

    #[test]
    fn runner_vocabulary_maps_onto_chi_cache_statuses() {
        assert_eq!(runner_terminal_status("done"), Some("done"));
        assert_eq!(runner_terminal_status("failed"), Some("failed"));
        assert_eq!(runner_terminal_status("timed_out"), Some("failed"));
        assert_eq!(runner_terminal_status("cancelled"), Some("cancelled"));
        assert_eq!(runner_terminal_status("running"), None);
        assert_eq!(runner_terminal_status(""), None);
        assert_eq!(runner_terminal_status("DONE"), None);
    }

    /// Every branch of the G-88 decision table.
    #[test]
    fn liveness_decision_table() {
        let failed = |e: &str| RunLiveness::Terminal {
            status: "failed",
            error: Some(e.to_string()),
        };
        let exited = failed(RUNNER_EXITED_ERROR);

        // File terminal: wins whatever the pid says.
        for alive in [true, false] {
            assert_eq!(
                decide_liveness(alive, Some("done"), None),
                RunLiveness::Terminal {
                    status: "done",
                    error: None
                }
            );
            assert_eq!(
                decide_liveness(alive, Some("failed"), Some("engine exited with code 1")),
                failed("engine exited with code 1")
            );
            assert_eq!(
                decide_liveness(alive, Some("timed_out"), Some("timed out after 60s")),
                failed("timed out after 60s")
            );
            assert_eq!(
                decide_liveness(alive, Some("cancelled"), None),
                RunLiveness::Terminal {
                    status: "cancelled",
                    error: None
                }
            );
        }
        // A terminal failure without an error still says something true.
        assert_eq!(
            decide_liveness(true, Some("timed_out"), None),
            failed("chi-runner reported timed_out")
        );

        // Pid dead, file non-terminal / missing / unparseable → failed.
        assert_eq!(decide_liveness(false, Some("running"), None), exited);
        assert_eq!(decide_liveness(false, None, None), exited);
        assert_eq!(decide_liveness(false, Some("mystery"), Some("x")), exited);

        // Pid alive, file non-terminal / missing → running.
        assert_eq!(
            decide_liveness(true, Some("running"), None),
            RunLiveness::Running
        );
        assert_eq!(decide_liveness(true, None, None), RunLiveness::Running);
    }

    #[test]
    fn probe_alive_counts_ours_and_unverified_only() {
        assert!(PidProbe::Ours.alive());
        assert!(PidProbe::Unverified.alive());
        assert!(!PidProbe::Foreign.alive());
        assert!(!PidProbe::Dead.alive());
    }

    #[cfg(unix)]
    #[test]
    fn probe_rejects_impossible_pids() {
        assert_eq!(probe_pid(0, RUNNER_BINARY), PidProbe::Dead);
        assert_eq!(probe_pid(u32::MAX, RUNNER_BINARY), PidProbe::Dead);
    }

    /// The probe tells a live process apart from a reused pid by what it runs.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn probe_corroborates_the_process_identity() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        // Right after spawn the child can still be mid-`execve` (argv not
        // mapped yet), which reads as `Unverified` — never `Dead`.
        let mut first = probe_pid(pid, "sleep");
        for _ in 0..100 {
            if first != PidProbe::Unverified {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            first = probe_pid(pid, "sleep");
        }
        assert_eq!(first, PidProbe::Ours);
        assert_eq!(probe_pid(pid, RUNNER_BINARY), PidProbe::Foreign);
        child.kill().await.unwrap();
        // Reaped by `kill()` (it waits), so the pid is gone — unless the
        // kernel has already handed it to another process. With a small
        // `pid_max` (32768 in CI containers) and a parallel test suite that
        // spawns thousands of short-lived children, that happens; a reused
        // pid is exactly what this probe exists to tell apart, so only
        // assert `Dead` when nothing holds the pid on either side of the probe.
        let exists = || unsafe { libc::kill(pid as i32, 0) } == 0;
        let before = exists();
        let after_reap = probe_pid(pid, "sleep");
        if !before && !exists() {
            assert_eq!(after_reap, PidProbe::Dead);
        }
    }

    /// An exited-but-unreaped child (a zombie) reads as `Dead`.
    #[cfg(target_os = "linux")]
    #[test]
    fn probe_reads_a_zombie_as_dead() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        // Not reaped yet: wait for it to become a zombie.
        for _ in 0..200 {
            if proc_state(pid as i32) == Some('Z') {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(proc_state(pid as i32), Some('Z'));
        assert_eq!(probe_pid(pid, "true"), PidProbe::Dead);
        child.wait().unwrap();
    }

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
