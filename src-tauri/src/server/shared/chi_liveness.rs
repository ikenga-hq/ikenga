//! Detached chi-runner liveness: the pid probe and the G-88 decision
//! (WP-18b), shared by the desktop's chi commands / reconciliation sweep and
//! the daemon's `chi_status` read (WP-19 slice 3).
//!
//! Moved verbatim out of `commands::chi_runner` (desktop-gated in `lib.rs`),
//! which re-exports every item here, so desktop call sites are unchanged.
//! Pure reads only: nothing here signals, spawns or writes. The kill path
//! (`kill_process_group`) and the runner launch stay in `commands::chi_runner`.

/// The runner binary's file stem.
pub(crate) const RUNNER_BINARY: &str = "chi-runner";

/// Recorded when the runner is gone but its status file never reached a
/// terminal status (crash, SIGKILL, OOM, a reboot mid-run).
pub(crate) const RUNNER_EXITED_ERROR: &str = "chi-runner exited without writing a terminal status";

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
}
