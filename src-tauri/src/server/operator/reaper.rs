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
//!   effective, saved or fs uid is the target's (a thread group whose leader
//!   is a zombie but whose other threads still run counts as live: review
//!   R2-1), and kills each survivor through a pidfd (`pidfd_open` →
//!   re-check `/proc/<pid>/status` → the pidfd is still live, so that status
//!   was this process's →
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

/// `State:` of a `/proc/.../status` is zombie or dead.
fn status_dead(status: &str) -> bool {
    status.lines().any(|line| {
        line.strip_prefix("State:")
            .is_some_and(|state| matches!(state.trim_start().chars().next(), Some('Z' | 'X' | 'x')))
    })
}

/// `Threads:` of a `/proc/<pid>/status` (0 if absent).
fn status_threads(status: &str) -> u32 {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// `uid` is the real, effective, saved or fs uid of a `/proc/.../status`.
fn status_has_uid(status: &str, uid: u32) -> bool {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .is_some_and(|ids| {
            ids.split_whitespace()
                .any(|id| id.parse::<u32>().ok() == Some(uid))
        })
}

/// Given its leader's `/proc/<pid>/status`, is this thread group a live
/// process of `uid`?
///
/// A zombie **leader** is not a dead process (review R2-1): when the main
/// thread `pthread_exit`s while others run, the leader shows `State: Z` but
/// the group lives on (`Threads:` > 1, and some `/proc/<pid>/task/*` is not
/// Z/X). It still holds the uid, is still a `kill(2)` target, and `pidfd_open`
/// and SIGKILL on its tgid reach the whole group. `any_task_live` is asked
/// only for a zombie leader whose `Threads:` doesn't already settle it.
fn group_holds_uid(status: &str, uid: u32, any_task_live: impl FnOnce() -> bool) -> bool {
    status_has_uid(status, uid)
        && (!status_dead(status) || status_threads(status) > 1 || any_task_live())
}

/// Some thread of group `pid` is neither zombie nor dead.
fn any_task_live(pid: i32) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks.flatten().any(|task| {
        std::fs::read_to_string(task.path().join("status")).is_ok_and(|s| !status_dead(&s))
    })
}

/// `/proc/<pid>` is a live thread group holding `uid` (see
/// [`group_holds_uid`]).
fn pid_holds_uid(pid: i32, uid: u32) -> bool {
    read_status(pid).is_some_and(|s| group_holds_uid(&s, uid, || any_task_live(pid)))
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
        if pid_holds_uid(pid, uid) {
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
    // Same liveness rule as the sweep: a zombie leader with live threads is
    // a target, and SIGKILL through its pidfd kills the whole group.
    let matches = pid_holds_uid(pid, uid);
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
        let status = |state: &str, threads: u32, uids: &str| {
            format!(
                "Name:\tsleep\nState:\t{state}\nTgid:\t9\nUid:\t{uids}\nGid:\t0\t0\t0\t0\n\
                 Threads:\t{threads}\n"
            )
        };
        let holds = |status: &str, tasks_live: bool| group_holds_uid(status, 28_520, || tasks_live);
        let all = "28520\t28520\t28520\t28520";
        assert!(holds(&status("S (sleeping)", 1, all), false));
        // Only the saved uid (reachable by kill(2)) still counts.
        assert!(holds(
            &status("T (stopped)", 1, "1000\t1000\t28520\t1000"),
            false
        ));
        assert!(!holds(
            &status("S (sleeping)", 1, "1000\t1000\t1000\t1000"),
            true
        ));
        // A dead process (zombie leader, no thread left) or dead task is not
        // a survivor.
        assert!(!holds(&status("Z (zombie)", 1, all), false));
        assert!(!holds(&status("X (dead)", 1, all), false));
        // Review R2-1: a zombie leader whose group still runs threads is.
        assert!(holds(&status("Z (zombie)", 2, all), false));
        assert!(holds(&status("Z (zombie)", 1, all), true));
        // ... but only if it holds the uid.
        assert!(!holds(
            &status("Z (zombie)", 2, "1000\t1000\t1000\t1000"),
            true
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

    /// Set in the survivor that [`t1_root_zombie_leader_entry`] becomes.
    const ZOMBIE_LEADER_ENV: &str = "IKENGA_TEST_ZOMBIE_LEADER";

    /// The lib test binary re-exec'd as a thread group whose main thread has
    /// exited while another thread sleeps: the leader is a zombie
    /// (`State: Z`), the group is alive. A signal handler makes the **main**
    /// thread leave with a raw `exit(2)` (thread-only, no unwinding,
    /// async-signal-safe); a spawned thread keeps sleeping.
    #[test]
    #[ignore = "t1-root (zombie-leader survivor entry)"]
    fn t1_root_zombie_leader_entry() {
        if std::env::var_os(ZOMBIE_LEADER_ENV).is_none() {
            return;
        }
        extern "C" fn exit_main_thread(_: libc::c_int) {
            // SAFETY: raw SYS_exit ends only the calling thread.
            unsafe {
                if libc::syscall(libc::SYS_gettid) == libc::getpid() as libc::c_long {
                    libc::syscall(libc::SYS_exit, 0);
                }
            }
        }
        // The survivor's own thread, whichever thread libtest runs us on.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(300));
            std::process::exit(0);
        });
        // SAFETY: installing a handler, then signalling the main thread.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = exit_main_thread as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
            let pid = libc::getpid();
            libc::syscall(libc::SYS_tgkill, pid, pid, libc::SIGUSR1);
        }
        std::thread::sleep(Duration::from_secs(300));
        std::process::exit(0);
    }

    /// Review R2-1: a survivor whose thread-group leader is a zombie (main
    /// thread exited, another still running) is not mistaken for dead. With
    /// a lying helper (`exit 0`) root's sweep must find and SIGKILL it, and
    /// only then report `Killed`.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_sweep_kills_a_group_whose_leader_is_a_zombie() {
        t1_root::require_root();
        let uid = 28_524;
        let (_t, home) = t1_root::home_for(uid);
        let exec = T1Executor::new(crate::executor::t1::tests::config());
        let victim = principal(uid, &home);
        let mut spec = SpawnSpec::new(std::env::current_exe().unwrap());
        spec.args([
            "--exact",
            "server::operator::reaper::tests::t1_root_zombie_leader_entry",
            "--ignored",
            "--test-threads=1",
            "-q",
        ])
        .env(ZOMBIE_LEADER_ENV, "1")
        .current_dir("/")
        .principal(Some(victim.clone()));
        let mut opts = crate::executor::t1::tests::piped();
        opts.detached = true;
        let mut survivor = exec.spawn_piped(spec, opts).unwrap();
        let pid = survivor.id().unwrap() as i32;

        // Wait for the shape under test: leader Z, group still threaded.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = read_status(pid).expect("the survivor is running");
            if status_dead(&status) && status_threads(&status) > 1 {
                assert!(status_has_uid(&status, uid));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the main thread never exited:\n{status}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(any_task_live(pid));
        assert_eq!(processes_of(uid).unwrap(), vec![pid], "the sweep sees it");

        let (_tmp, root) = test_support::temp_root();
        let reaper = T1Reaper::new(&root, sh_helper("exit 0"));
        assert_eq!(reaper.kill_all(&victim).unwrap(), ReapOutcome::Killed);
        // `Killed` came only after the group was gone: no live thread is
        // left by the time kill_all returns.
        assert!(
            !any_task_live(pid),
            "kill_all returned with the group alive"
        );
        assert!(processes_of(uid).unwrap().is_empty());

        let status = tokio::time::timeout(Duration::from_secs(10), survivor.wait())
            .await
            .expect("root killed the group")
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
