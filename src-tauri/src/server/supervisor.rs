//! `ikenga-server supervise`: a minimal init for running the server where
//! there is no systemd — a container.
//!
//! Detached agent runs (chi-runner) are spawned in their own process group so
//! that they outlive the process that started them; the next server reads
//! their pid and status file back (`shared::chi_liveness`). Under systemd the
//! multi-user unit sets `KillMode=process`, so a restart signals only the
//! server and those runs keep going.
//!
//! A container has no such setting. When the server is the container's
//! PID 1 and exits — a crash, an upgrade, an operator restart — the kernel
//! tears down the whole PID namespace and every detached run dies with it.
//!
//! `supervise` takes PID 1 instead and runs the server as its child:
//!
//! * **Restart.** When the server exits on its own it is started again, after
//!   1 s, doubling up to 30 s while it keeps exiting within 10 s of starting.
//!   `SIGHUP` restarts the server on purpose (SIGTERM to it, then an
//!   immediate start). Either way the supervisor — and so the namespace —
//!   stays up, and detached runs are untouched: they are never signalled.
//! * **Reap.** A run whose launching process has exited is re-parented to
//!   PID 1 (or, outside a container, to this process: it registers as a
//!   child subreaper). Every such orphan is reaped when it exits, so a
//!   finished run never lingers as a zombie.
//! * **Stop.** `SIGTERM` / `SIGINT` / `SIGQUIT` forward `SIGTERM` to the
//!   server, wait for it, and exit with its status (a second one sends
//!   `SIGKILL`). Stopping the container still ends every run in it — no
//!   process survives its own PID namespace — so this keeps runs alive across
//!   a *server* restart, not a container restart.
//!
//! The server is re-executed from the path it was first started by, so
//! replacing the binary on disk and sending `SIGHUP` runs the new one.
//!
//! Linux-only: it relies on `sigtimedwait` and `PR_SET_CHILD_SUBREAPER`.
//! The supervisor must be single-threaded when [`run`] is called (no async
//! runtime started), because it blocks its signals and takes them
//! synchronously.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// What [`run`] supervises, and how patiently.
#[derive(Debug, Clone)]
pub struct SuperviseOptions {
    /// The server executable.
    pub program: PathBuf,
    /// Its arguments (the server's own flags).
    pub args: Vec<OsString>,
    /// The first restart delay, and the delay after a run that stayed up.
    pub min_delay: Duration,
    /// The longest restart delay while the server keeps exiting quickly.
    pub max_delay: Duration,
    /// A run at least this long counts as healthy and resets the delay.
    pub healthy_after: Duration,
}

impl SuperviseOptions {
    pub fn new(program: PathBuf, args: Vec<OsString>) -> Self {
        Self {
            program,
            args,
            min_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            healthy_after: Duration::from_secs(10),
        }
    }
}

/// The restart backoff. Pure.
///
/// Given the delay that would apply now and how long the server ran, returns
/// `(wait before this restart, delay for the one after)`. A healthy run
/// starts over from `min_delay`; a quick exit waits the current delay and
/// doubles it, capped at `max_delay`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn backoff(
    opts: &SuperviseOptions,
    current: Duration,
    ran: Duration,
) -> (Duration, Duration) {
    let wait = if ran >= opts.healthy_after {
        opts.min_delay
    } else {
        current.clamp(opts.min_delay, opts.max_delay)
    };
    (wait, (wait * 2).min(opts.max_delay))
}

/// A wait status as a shell would report it: the exit code, or 128 + the
/// signal number. Pure.
#[cfg(target_os = "linux")]
pub(crate) fn exit_code(status: libc::c_int) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

/// Run the supervisor until it is told to stop; returns the process exit code.
#[cfg(not(target_os = "linux"))]
pub fn run(_opts: &SuperviseOptions) -> i32 {
    tracing::error!("supervise is Linux-only (it is meant for containers)");
    2
}

/// Run the supervisor until it is told to stop; returns the process exit code.
#[cfg(target_os = "linux")]
pub fn run(opts: &SuperviseOptions) -> i32 {
    linux::run(opts)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use tracing::{debug, error, info, warn};

    use super::{backoff, exit_code, SuperviseOptions};

    const SIGNALS: [libc::c_int; 5] = [
        libc::SIGTERM,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGHUP,
        libc::SIGCHLD,
    ];

    struct Server {
        pid: libc::pid_t,
        started: Instant,
    }

    fn signal_set() -> libc::sigset_t {
        // SAFETY: sigemptyset initialises the set before any read.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for sig in SIGNALS {
                libc::sigaddset(&mut set, sig);
            }
            set
        }
    }

    /// The next signal in `set`, or `None` when `timeout` passes first.
    fn wait_signal(set: &libc::sigset_t, timeout: Duration) -> Option<libc::c_int> {
        // Zeroed then filled: some targets give `timespec` a padding field.
        // SAFETY: all-zero is a valid timespec.
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        ts.tv_sec = timeout.as_secs() as libc::time_t;
        ts.tv_nsec = timeout.subsec_nanos() as _;
        // SAFETY: `set` and `ts` are valid for the call; no siginfo wanted.
        let sig = unsafe { libc::sigtimedwait(set, std::ptr::null_mut(), &ts) };
        (sig > 0).then_some(sig)
    }

    fn signal(pid: libc::pid_t, sig: libc::c_int) {
        // SAFETY: plain syscall. Only ever the server's own pid — never a
        // process group, so detached runs are not reached.
        if unsafe { libc::kill(pid, sig) } != 0 {
            debug!(
                "signal {sig} to server {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn spawn(opts: &SuperviseOptions) -> std::io::Result<libc::pid_t> {
        let mut cmd = Command::new(&opts.program);
        cmd.args(&opts.args);
        // The supervisor blocks its signals to take them synchronously; the
        // server must start with an empty mask (a blocked SIGTERM would make
        // it unstoppable).
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                let mut empty: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut empty);
                if libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        // Reaped below with waitpid(-1); dropping the handle neither waits
        // nor kills.
        Ok(child.id() as libc::pid_t)
    }

    pub(super) fn run(opts: &SuperviseOptions) -> i32 {
        let set = signal_set();
        // SAFETY: valid set; the process is single-threaded here (see the
        // module docs), so this mask is the whole process's.
        if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) } != 0 {
            error!(
                "supervise: could not block signals: {}",
                std::io::Error::last_os_error()
            );
            return 1;
        }
        // As PID 1 every orphan in the namespace is ours already; anywhere
        // else, become their reaper explicitly.
        // SAFETY: plain prctl with integer arguments.
        if std::process::id() != 1
            && unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0
        {
            warn!(
                "supervise: could not become a child subreaper ({}); orphaned runs re-parent \
                 to PID 1 instead",
                std::io::Error::last_os_error()
            );
        }
        info!(
            "supervise: pid {}, server {}",
            std::process::id(),
            opts.program.display()
        );

        let mut server: Option<Server> = None;
        let mut next_start = Some(Instant::now());
        let mut delay = opts.min_delay;
        let mut restart_requested = false;
        let mut stopping = false;
        let mut code = 0;

        loop {
            // Reap everything that has exited: the server, and any orphan.
            loop {
                let mut status: libc::c_int = 0;
                // SAFETY: plain syscall with a valid out-pointer.
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
                let Some(s) = server.as_ref().filter(|s| s.pid == pid) else {
                    debug!("supervise: reaped orphan {pid} ({})", exit_code(status));
                    continue;
                };
                let ran = s.started.elapsed();
                server = None;
                let status_code = exit_code(status);
                if stopping {
                    code = status_code;
                } else if restart_requested {
                    restart_requested = false;
                    delay = opts.min_delay;
                    next_start = Some(Instant::now());
                    info!("supervise: server {pid} stopped ({status_code}); restarting");
                } else {
                    let (wait, next) = backoff(opts, delay, ran);
                    delay = next;
                    next_start = Some(Instant::now() + wait);
                    warn!(
                        "supervise: server {pid} exited ({status_code}) after {}s; restarting \
                         in {}s",
                        ran.as_secs(),
                        wait.as_secs_f32()
                    );
                }
            }

            if stopping && server.is_none() {
                info!("supervise: stopped ({code})");
                return code;
            }

            if server.is_none() && next_start.is_some_and(|t| Instant::now() >= t) {
                match spawn(opts) {
                    Ok(pid) => {
                        info!("supervise: started server {pid}");
                        server = Some(Server {
                            pid,
                            started: Instant::now(),
                        });
                        next_start = None;
                    }
                    Err(e) => {
                        let (wait, next) = backoff(opts, delay, Duration::ZERO);
                        delay = next;
                        next_start = Some(Instant::now() + wait);
                        error!(
                            "supervise: could not start {}: {e}; retrying in {}s",
                            opts.program.display(),
                            wait.as_secs_f32()
                        );
                    }
                }
            }

            // Wake on a signal, at the next start, or at least once a second
            // (a SIGCHLD that coalesced is then still reaped promptly).
            let timeout = next_start
                .map(|t| t.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(1))
                .clamp(Duration::from_millis(10), Duration::from_secs(1));
            match wait_signal(&set, timeout) {
                Some(libc::SIGTERM | libc::SIGINT | libc::SIGQUIT) => {
                    if stopping {
                        if let Some(s) = &server {
                            warn!("supervise: second stop signal; killing server {}", s.pid);
                            signal(s.pid, libc::SIGKILL);
                        }
                    } else {
                        stopping = true;
                        next_start = None;
                        match &server {
                            Some(s) => {
                                info!("supervise: stopping server {}", s.pid);
                                signal(s.pid, libc::SIGTERM);
                            }
                            None => {
                                info!("supervise: stopped (no server running)");
                                return 0;
                            }
                        }
                    }
                }
                Some(libc::SIGHUP) if !stopping => match &server {
                    Some(s) => {
                        info!("supervise: SIGHUP; restarting server {}", s.pid);
                        restart_requested = true;
                        signal(s.pid, libc::SIGTERM);
                    }
                    None => {
                        delay = opts.min_delay;
                        next_start = Some(Instant::now());
                    }
                },
                // SIGCHLD or the timeout: the loop reaps and restarts.
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> SuperviseOptions {
        SuperviseOptions::new(PathBuf::from("ikenga-server"), Vec::new())
    }

    #[test]
    fn backoff_doubles_while_the_server_keeps_exiting_quickly() {
        let o = opts();
        let quick = Duration::from_millis(200);
        let (w1, d1) = backoff(&o, o.min_delay, quick);
        assert_eq!((w1, d1), (Duration::from_secs(1), Duration::from_secs(2)));
        let (w2, d2) = backoff(&o, d1, quick);
        assert_eq!((w2, d2), (Duration::from_secs(2), Duration::from_secs(4)));
        let mut d = d2;
        for _ in 0..10 {
            d = backoff(&o, d, quick).1;
        }
        assert_eq!(d, o.max_delay, "capped");
        assert_eq!(backoff(&o, d, quick).0, o.max_delay);
    }

    #[test]
    fn a_healthy_run_resets_the_backoff() {
        let o = opts();
        let (wait, next) = backoff(&o, o.max_delay, Duration::from_secs(60));
        assert_eq!(wait, o.min_delay);
        assert_eq!(next, o.min_delay * 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exit_codes_read_like_a_shell() {
        // Exited with 3: the status word carries it in bits 8..15.
        assert_eq!(exit_code(3 << 8), 3);
        assert_eq!(exit_code(0), 0);
        // Killed by SIGKILL (9): the low 7 bits.
        assert_eq!(exit_code(9), 137);
    }
}
