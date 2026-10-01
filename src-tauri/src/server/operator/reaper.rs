//! The §7.3 uid-wide kill: on `accounts disable`, every process of the
//! principal's uid is killed by a helper spawned **through the T1 executor as
//! that uid** — `ikenga-server __t1-kill-all`, which calls
//! `kill(-1, SIGKILL)`. Running as the uid (not as root signalling pids it
//! found) means the kernel decides what is in reach: exactly that uid's
//! processes, detached chi-runners in their own process groups included, and
//! nothing a pid-reuse race could misdirect.
//!
//! The executor here is unprobed (the root CLI runs no §8 probe), but every
//! spawn is verified the same way: the helper can't start unless the drop to
//! the uid, no groups and `setuid(0) == EPERM` held (§9.2).
//!
//! The helper runs as the very uid it kills, so that uid's still-running
//! processes can interfere with it (review S2-2): `SIGSTOP` it between exec
//! and `kill(-1)`, or — once exec has made it dumpable again, on a host with
//! Yama `ptrace_scope=0` — ptrace it and make it exit 0. So root trusts
//! neither its exit code nor its liveness:
//!
//! - the wait is bounded ([`HELPER_TIMEOUT`]); on a timeout root SIGKILLs the
//!   helper (its own unreaped child, so no pid-reuse race) and reports
//!   [`ReapOutcome::Failed`];
//! - afterwards root **sweeps** `/proc` for any live process whose real,
//!   effective, saved or fs uid is the target's, and kills each survivor
//!   through a pidfd (`pidfd_open` → re-check `/proc/<pid>/status` → the
//!   pidfd is still live, so that status was this process's →
//!   `pidfd_send_signal(SIGKILL)`), until a full pass finds none. Only a
//!   helper that exited 0 **and** a clean sweep is [`ReapOutcome::Killed`].

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::Child;
use std::time::{Duration, Instant};

use super::provision::{ReapOutcome, UidReaper};
use super::OperatorRoot;
use crate::executor::t1::{T1Config, T1Executor, KILL_ALL_UID_ENV};
use crate::executor::{PipedOpts, Principal, SpawnSpec, StdioMode};

/// The hidden argv entry that runs `executor::t1::kill_all_entry`.
pub const KILL_ALL_ARG: &str = "__t1-kill-all";

/// How long root waits for the helper before killing it.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(10);

/// Root's sweep: up to this many passes over `/proc`, [`SWEEP_PAUSE`] apart
/// (a SIGKILLed process can take a moment to leave a `D` state).
const SWEEP_PASSES: u32 = 20;
const SWEEP_PAUSE: Duration = Duration::from_millis(100);

/// How the reaper runs its helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl HelperCommand {
    /// This binary's `__t1-kill-all` entry.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![KILL_ALL_ARG.into()],
        })
    }
}

/// [`UidReaper`] through the T1 executor.
#[derive(Debug)]
pub struct T1Reaper {
    executor: T1Executor,
    helper: HelperCommand,
    timeout: Duration,
}

impl T1Reaper {
    pub fn new(root: &OperatorRoot, helper: HelperCommand) -> Self {
        Self {
            executor: T1Executor::new(T1Config {
                principals_dir: root.principals_dir(),
                principal_path: None,
            }),
            helper,
            timeout: HELPER_TIMEOUT,
        }
    }

    /// A different helper deadline (tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Spawn the helper as the principal and wait for it, at most
    /// `self.timeout`. `Err` names what went wrong.
    fn run_helper(&self, principal: &Principal) -> Result<(), String> {
        let what = format!(
            "{} {} as uid {}",
            self.helper.program.display(),
            KILL_ALL_ARG,
            principal.uid
        );
        let mut spec = SpawnSpec::new(&self.helper.program);
        spec.args(&self.helper.args)
            .env(KILL_ALL_UID_ENV, principal.uid.to_string())
            // Not the home: a disabled principal's files may be gone.
            .current_dir("/")
            .principal(Some(principal.clone()));
        let mut child = self
            .executor
            .spawn_std(
                spec,
                PipedOpts {
                    stdin: StdioMode::Null,
                    stdout: StdioMode::Null,
                    stderr: StdioMode::Piped,
                    kill_on_drop: false,
                    no_console_window: false,
                    detached: false,
                    new_process_group: false,
                },
            )
            .map_err(|e| format!("{what}: {e}"))?;
        let status = match wait_bounded(&mut child, self.timeout) {
            Ok(Some(status)) => status,
            Ok(None) => {
                // Still our unreaped child: its pid can't have been reused.
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{what} did not exit within {:?} (a process of the uid may have stopped or \
                     traced it); root killed the helper",
                    self.timeout
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{what}: waiting: {e}"));
            }
        };
        if !status.success() {
            return Err(format!(
                "{what} exited {status}: {}",
                drain_nonblocking(child.stderr.take()).trim()
            ));
        }
        Ok(())
    }
}

impl UidReaper for T1Reaper {
    fn kill_all(&self, principal: &Principal) -> anyhow::Result<ReapOutcome> {
        // The helper never runs as root (`admit` refuses uid/gid 0), and the
        // sweep must never target root either.
        if principal.uid == 0 || principal.gid == 0 {
            anyhow::bail!("refusing a uid-wide kill of uid {}", principal.uid);
        }
        let helper = self.run_helper(principal);
        // Whatever the helper did, root checks for itself.
        let sweep = sweep_uid(principal.uid);
        Ok(match (helper, sweep) {
            (Ok(()), Ok(0)) => ReapOutcome::Killed,
            (Ok(()), Ok(n)) => {
                tracing::warn!(
                    "{n} process(es) of uid {} survived the kill helper; root killed them",
                    principal.uid
                );
                ReapOutcome::Killed
            }
            (Err(h), Ok(n)) => ReapOutcome::Failed(format!(
                "{h}; root's sweep afterwards killed {n} process(es) of uid {} and found none left",
                principal.uid
            )),
            (Ok(()), Err(s)) => ReapOutcome::Failed(s),
            (Err(h), Err(s)) => ReapOutcome::Failed(format!("{h}; {s}")),
        })
    }
}

/// `try_wait` until `timeout`. `Ok(None)`: still running.
fn wait_bounded(
    child: &mut Child,
    timeout: Duration,
) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whatever the helper wrote to stderr, without ever blocking (a process of
/// the uid could hold the pipe open).
fn drain_nonblocking(pipe: Option<std::process::ChildStderr>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let fd = pipe.as_raw_fd();
    // SAFETY: fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return String::new();
        }
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while buf.len() < 64 * 1024 {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// `/proc/<pid>/status` → is this a live (not zombie/dead) process with
/// `uid` as its real, effective, saved or fs uid?
fn status_holds_uid(status: &str, uid: u32) -> bool {
    let mut live = false;
    let mut holds = false;
    for line in status.lines() {
        if let Some(state) = line.strip_prefix("State:") {
            live = !matches!(state.trim_start().chars().next(), Some('Z' | 'X' | 'x'));
        } else if let Some(ids) = line.strip_prefix("Uid:") {
            holds = ids
                .split_whitespace()
                .any(|id| id.parse::<u32>().ok() == Some(uid));
        }
    }
    live && holds
}

fn read_status(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status")).ok()
}

/// The nearest ancestor of this process (parent, grandparent, … up to pid 1)
/// that runs as `uid`, if any. A uid-wide kill would take it down, and with
/// it this process's session: `sudo adopt-t0` from a login of the very user
/// being adopted would be cut off by its own kill (SIGHUP) half-way.
pub(crate) fn ancestor_holding_uid(uid: u32) -> io::Result<Option<i32>> {
    // SAFETY: getppid has no preconditions and cannot fail.
    let mut pid = unsafe { libc::getppid() };
    // Bounded: a pid chain can't be longer than the pid space, but a racing
    // reparent must not loop us forever either.
    for _ in 0..4096 {
        if pid <= 1 {
            return Ok(None);
        }
        let Some(status) = read_status(pid) else {
            return Ok(None);
        };
        if status_holds_uid(&status, uid) {
            return Ok(Some(pid));
        }
        pid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|p| p.trim().parse::<i32>().ok())
            .unwrap_or(0);
    }
    Ok(None)
}

/// Live processes of `uid`, from a pass over `/proc`.
fn processes_of(uid: u32) -> io::Result<Vec<i32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        // Gone already (ENOENT/ESRCH) is fine.
        if read_status(pid).is_some_and(|s| status_holds_uid(&s, uid)) {
            pids.push(pid);
        }
    }
    Ok(pids)
}

fn pidfd_send_signal(fd: &OwnedFd, sig: i32) -> io::Result<()> {
    // SAFETY: a pidfd we own; no siginfo.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// SIGKILL `pid` from root, but only if it is still a process of `uid`.
/// `Ok(false)`: it was gone, or not the uid's, by the time we looked.
fn kill_if_still_uid(pid: i32, uid: u32) -> io::Result<bool> {
    // SAFETY: plain syscall.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0u32) };
    if raw < 0 {
        let e = io::Error::last_os_error();
        return match e.raw_os_error() {
            Some(libc::ESRCH) => Ok(false),
            _ => Err(io::Error::new(e.kind(), format!("pidfd_open({pid}): {e}"))),
        };
    }
    // SAFETY: pidfd_open returned a fresh fd we now own.
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    let matches = read_status(pid).is_some_and(|s| status_holds_uid(&s, uid));
    // Still alive through the pidfd → the pid wasn't reused, so the status
    // just read was this process's.
    match pidfd_send_signal(&fd, 0) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => return Ok(false),
        Err(e) => return Err(e),
    }
    if !matches {
        return Ok(false);
    }
    match pidfd_send_signal(&fd, libc::SIGKILL) {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == Some(libc::ESRCH) => Ok(false),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("pidfd_send_signal({pid}, SIGKILL): {e}"),
        )),
    }
}

/// Root's after-the-fact check: kill survivors until a full pass over
/// `/proc` finds no live process of `uid`. `Ok(n)`: `n` were killed from
/// root. `Err`: some survived every pass, or `/proc` could not be read.
fn sweep_uid(uid: u32) -> Result<usize, String> {
    let mut killed = std::collections::HashSet::new();
    for pass in 0..SWEEP_PASSES {
        let pids = processes_of(uid).map_err(|e| format!("reading /proc: {e}"))?;
        if pids.is_empty() {
            return Ok(killed.len());
        }
        if pass + 1 == SWEEP_PASSES {
            return Err(format!(
                "process(es) {pids:?} of uid {uid} are still alive after the kill"
            ));
        }
        for pid in pids {
            match kill_if_still_uid(pid, uid) {
                Ok(true) => {
                    killed.insert(pid);
                }
                Ok(false) => {}
                Err(e) => return Err(format!("killing survivor {pid} of uid {uid}: {e}")),
            }
        }
        std::thread::sleep(SWEEP_PAUSE);
    }
    unreachable!("the last pass returns")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::executor::t1::tests::{principal, t1_root};
    use crate::executor::SessionExecutor;
    use crate::server::operator::test_support;

    /// The kill helper never runs as root, and a refused spawn is an error
    /// (which `disable` reports as `ReapOutcome::Failed`).
    #[test]
    fn a_root_principal_is_refused_before_any_spawn() {
        let (_tmp, root) = test_support::temp_root();
        let reaper = T1Reaper::new(
            &root,
            HelperCommand {
                program: "/bin/true".into(),
                args: vec![],
            },
        );
        assert!(reaper.kill_all(&principal(0, "/root")).is_err());
    }

    #[test]
    fn status_parsing_matches_any_live_uid_field() {
        let status = |state: &str, uids: &str| {
            format!("Name:\tsleep\nState:\t{state}\nTgid:\t9\nUid:\t{uids}\nGid:\t0\t0\t0\t0\n")
        };
        assert!(status_holds_uid(
            &status("S (sleeping)", "28520\t28520\t28520\t28520"),
            28_520
        ));
        // Only the saved uid (reachable by kill(2)) still counts.
        assert!(status_holds_uid(
            &status("T (stopped)", "1000\t1000\t28520\t1000"),
            28_520
        ));
        assert!(!status_holds_uid(
            &status("S (sleeping)", "1000\t1000\t1000\t1000"),
            28_520
        ));
        // Zombies and dead tasks are not survivors.
        assert!(!status_holds_uid(
            &status("Z (zombie)", "28520\t28520\t28520\t28520"),
            28_520
        ));
        assert!(!status_holds_uid(
            &status("X (dead)", "28520\t28520\t28520\t28520"),
            28_520
        ));
    }

    /// The lib test binary's stand-in for `ikenga-server __t1-kill-all`.
    #[test]
    #[ignore = "t1-root (kill helper entry)"]
    fn t1_root_kill_all_entry() {
        if std::env::var_os(KILL_ALL_UID_ENV).is_none() {
            return;
        }
        match crate::executor::t1::kill_all_entry() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("t1 kill-all: {e}");
                std::process::exit(3);
            }
        }
    }

    pub(crate) fn test_helper() -> HelperCommand {
        HelperCommand {
            program: std::env::current_exe().unwrap(),
            args: [
                "--exact",
                "server::operator::reaper::tests::t1_root_kill_all_entry",
                "--ignored",
                "--test-threads=1",
                "-q",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        }
    }

    /// §7.3: a detached, own-process-group process of the uid dies; a
    /// process of another uid does not.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_kill_all_reaps_every_process_of_the_uid_only() {
        t1_root::require_root();
        let (uid, other) = (28_520, 28_521);
        let (_t1, home) = t1_root::home_for(uid);
        let (_t2, other_home) = t1_root::home_for(other);
        let exec = T1Executor::new(crate::executor::t1::tests::config());
        let spawn_sleeper = |p: Principal| {
            let mut spec = SpawnSpec::new("/bin/sh");
            spec.arg("-c").arg("exec sleep 300").principal(Some(p));
            let mut opts = crate::executor::t1::tests::piped();
            opts.detached = true;
            exec.spawn_piped(spec, opts).unwrap()
        };
        let victim = principal(uid, &home);
        let mut target = spawn_sleeper(victim.clone());
        let mut bystander = spawn_sleeper(principal(other, &other_home));

        let (_tmp, root) = test_support::temp_root();
        let reaper = T1Reaper::new(&root, test_helper());
        assert_eq!(reaper.kill_all(&victim).unwrap(), ReapOutcome::Killed);

        let status = tokio::time::timeout(std::time::Duration::from_secs(10), target.wait())
            .await
            .expect("the uid's process was killed")
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(
            bystander.try_wait().unwrap().is_none(),
            "another uid's process is out of reach"
        );
        bystander.kill().await.unwrap();

        // Nothing left to kill is still success (ESRCH).
        assert_eq!(reaper.kill_all(&victim).unwrap(), ReapOutcome::Killed);
    }

    fn sh_helper(script: &str) -> HelperCommand {
        HelperCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }
    }

    fn spawn_sleeper(exec: &T1Executor, p: Principal) -> tokio::process::Child {
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c").arg("exec sleep 300").principal(Some(p));
        let mut opts = crate::executor::t1::tests::piped();
        opts.detached = true;
        exec.spawn_piped(spec, opts).unwrap()
    }

    /// Review S2-2: a helper that "succeeds" without killing (as a ptraced
    /// one could be made to) is caught by root's sweep, which kills the
    /// survivor itself.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_root_sweeps_survivors_of_a_lying_helper() {
        t1_root::require_root();
        let uid = 28_522;
        let (_t, home) = t1_root::home_for(uid);
        let exec = T1Executor::new(crate::executor::t1::tests::config());
        let victim = principal(uid, &home);
        let mut survivor = spawn_sleeper(&exec, victim.clone());

        let (_tmp, root) = test_support::temp_root();
        let reaper = T1Reaper::new(&root, sh_helper("exit 0"));
        assert_eq!(reaper.kill_all(&victim).unwrap(), ReapOutcome::Killed);
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), survivor.wait())
            .await
            .expect("root killed the survivor")
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
    }

    /// Review S2-2: a helper stopped by a process of its uid (here: by
    /// itself) never hangs the CLI. Root kills it at the deadline, reports
    /// `Failed`, and still sweeps the uid's processes.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_a_stopped_helper_times_out_and_is_reported_failed() {
        t1_root::require_root();
        let uid = 28_523;
        let (_t, home) = t1_root::home_for(uid);
        let exec = T1Executor::new(crate::executor::t1::tests::config());
        let victim = principal(uid, &home);
        let mut survivor = spawn_sleeper(&exec, victim.clone());

        let (_tmp, root) = test_support::temp_root();
        let reaper = T1Reaper::new(&root, sh_helper("kill -STOP $$; exit 0"))
            .with_timeout(std::time::Duration::from_secs(1));
        let started = std::time::Instant::now();
        let outcome = reaper.kill_all(&victim).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(8));
        let ReapOutcome::Failed(why) = outcome else {
            panic!("expected Failed, got {outcome:?}");
        };
        assert!(why.contains("did not exit within"), "{why}");
        assert!(why.contains("found none left"), "{why}");
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), survivor.wait())
            .await
            .expect("root's sweep killed the survivor")
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(processes_of(uid).unwrap().is_empty());
    }
}
