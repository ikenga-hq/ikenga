//! T1 — per-principal Unix uid executor (G-PRINCIPAL §9, ADR-023 D1).
//!
//! Every spawn runs **as a resolved [`Principal`]**: the child drops to the
//! principal's uid/gid with no supplementary groups before `exec`, then proves
//! the drop in a `pre_exec` closure that *verifies instead of trusting*
//! (§9.2). A spawn with no principal, or one naming uid/gid 0, is refused
//! (I-1). A failed verification refuses the spawn **and** marks the executor
//! degraded: every later spawn is refused and `/api/health` reports
//! `principal_isolation: false` until restart (§8 "runtime drift", I-5).
//!
//! ## The order std performs the drop in (§9.2 "to verify at implementation")
//!
//! Verified against the Rust 1.97.0 standard library source,
//! `library/std/src/sys/process/unix/unix.rs`, `Command::do_exec` — the
//! fork/exec path, which std always takes when a `pre_exec` closure is set
//! (`posix_spawn` is skipped whenever `get_closures()` is non-empty). In the
//! forked child, after the stdio `dup2`s, std does:
//!
//! 1. `setgroups(len, list)` — only with the unstable explicit `groups()`
//!    set, which this executor never uses;
//! 2. `setgid(gid)` — error → spawn fails;
//! 3. when a uid is set and no explicit groups were: `setgroups(0, NULL)`,
//!    **ignoring `EPERM`** (no `CAP_SETGID`), then `setuid(uid)` — error →
//!    spawn fails;
//! 4. `chroot` (unset here), then `chdir(cwd)` — already as the new uid;
//! 5. `setpgid` (when a process group was asked for), `setsid`, `SIGPIPE`
//!    reset;
//! 6. **then** the `pre_exec` closures, in registration order; then `execvp`.
//!
//! So the contract's "setgid → setgroups([]) → setuid → chdir → pre_exec"
//! holds, and because step 3 can silently leave supplementary groups behind,
//! the closure below re-checks everything after the fact. If a future std
//! ever reorders these calls, the checks fail closed rather than trusting it.
//!
//! ## Environment and cwd (§9.3)
//!
//! The child never inherits the broker's environment. It gets exactly the
//! floor — `HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `TMPDIR`, plus `LANG` /
//! `LC_*` / `TZ` passed through from the broker — then the spec's own vars,
//! minus anything host-only (`is_host_only_env`: the auth token, vault key,
//! pkg DB token, `IKENGA_SECRET_*`) and `IKENGA_BOOTSTRAP_*`. This amends
//! `EnvSpec`'s "the executor does not second-guess" for T1 only: here the
//! floor is a security boundary, so it is the executor's job. The only way to
//! pass a host-only value is [`T1Executor::spawn_piped_with_host_env`], which
//! the broker's child launcher uses for its per-child token and the
//! `IKENGA_SECRET_*` operator defaults (§5 row 15).
//!
//! A missing cwd becomes the principal's home, and a relative one is resolved
//! against it: the executor never inherits the broker's cwd.
//!
//! ## PTYs
//!
//! `spawn_pty` is refused: under topology B (OD-1) the broker's only setuid
//! spawn is the per-principal child, and PTYs live inside that child, which
//! already runs as the uid (§9.2). portable-pty 0.8.1 has no `pre_exec` hook
//! to drop through anyway (§3).
//!
//! Linux-only, like T1.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::PtySize;

use super::{
    Capabilities, ExecutorTier, PipedOpts, Principal, ProbeStamp, PtyChild, SessionExecutor,
    SpawnSpec,
};

/// The errno a failed drop verification reports through std's exec-error
/// pipe. `ENOTRECOVERABLE` is never produced by `setgid`/`setuid`/`chdir`/
/// `setpgid`/`execve`, so the parent can tell "the drop did not hold" apart
/// from an ordinary spawn failure (a missing program, an unreadable cwd)
/// without allocating in the child.
pub const DROP_VERIFY_ERRNO: i32 = libc::ENOTRECOVERABLE;

/// What the T1 executor needs from the operator, resolved before any spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct T1Config {
    /// `<root>/principals`: a principal's `TMPDIR` is
    /// `<principals_dir>/<principal_id>/data/tmp` (§4, §9.3) — `/tmp` is
    /// shared across principals.
    pub principals_dir: PathBuf,
    /// The operator's `--principal-path`. `None` →
    /// `<home>/.local/bin:/usr/local/bin:/usr/bin:/bin` (§9.3).
    pub principal_path: Option<OsString>,
}

/// The T1 executor. Built either from a passing §8 probe
/// ([`with_probe`](Self::with_probe)), which is the only way it reports
/// `principal_isolation: true` (I-5), or unprobed for a root CLI helper (the
/// §7.3 uid-wide kill), whose spawns are verified exactly the same way.
#[derive(Debug)]
pub struct T1Executor {
    config: T1Config,
    probe: Option<ProbeStamp>,
    degraded: AtomicBool,
}

/// Default `PATH` for a principal (§9.3).
fn default_path(home: &std::path::Path) -> OsString {
    let mut path = home.join(".local/bin").into_os_string();
    path.push(":/usr/local/bin:/usr/bin:/bin");
    path
}

/// The §9.3 deny floor: never handed to a principal through a spec.
pub fn is_denied_env(key: &OsStr) -> bool {
    let key = key.to_string_lossy();
    crate::pty::is_host_only_env(&key) || key.starts_with("IKENGA_BOOTSTRAP_")
}

/// Passed through from the broker's own environment (§9.3).
fn is_locale_env(key: &OsStr) -> bool {
    let key = key.as_bytes();
    key == b"LANG" || key == b"TZ" || key.starts_with(b"LC_")
}

impl T1Executor {
    /// An executor with no probe result: spawns are verified, but it never
    /// reports `principal_isolation: true`.
    pub fn new(config: T1Config) -> Self {
        Self {
            config,
            probe: None,
            degraded: AtomicBool::new(false),
        }
    }

    /// The executor a passing §8 boot probe produces.
    pub fn with_probe(config: T1Config, stamp: ProbeStamp) -> Self {
        Self {
            probe: Some(stamp),
            ..Self::new(config)
        }
    }

    pub fn config(&self) -> &T1Config {
        &self.config
    }

    /// Whether a spawn-time drop verification has failed in this process.
    pub fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn degrade_for_tests(&self) {
        self.mark_degraded("test");
    }

    fn mark_degraded(&self, why: &str) {
        if !self.degraded.swap(true, Ordering::SeqCst) {
            tracing::error!(
                "T1 executor DEGRADED: {why}. Every further T1 spawn is refused and \
                 /api/health reports principal_isolation=false until restart (G-PRINCIPAL §8)."
            );
        }
    }

    /// I-1 and the degraded latch, before anything is built.
    fn admit<'a>(&self, spec: &'a SpawnSpec) -> io::Result<&'a Principal> {
        if self.is_degraded() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the T1 executor is degraded (a privilege-drop verification failed earlier); \
                 refusing every spawn until restart",
            ));
        }
        let Some(p) = spec.principal.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "T1 refuses a spawn without a resolved principal: nothing a session asks for \
                 runs as root (G-PRINCIPAL §9.1, I-1)",
            ));
        };
        if p.uid == 0 || p.gid == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "T1 refuses to spawn as uid {} gid {}: a principal is never root (I-1)",
                    p.uid, p.gid
                ),
            ));
        }
        if !p.home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("principal home {} is not absolute", p.home.display()),
            ));
        }
        Ok(p)
    }

    /// The child's complete environment (§9.3), in application order.
    fn environment(
        &self,
        p: &Principal,
        spec: &SpawnSpec,
        host_env: &[(OsString, OsString)],
    ) -> Vec<(OsString, OsString)> {
        let tmpdir = self
            .config
            .principals_dir
            .join(p.id.to_string())
            .join("data")
            .join("tmp");
        let mut env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), p.home.clone().into_os_string()),
            ("USER".into(), p.unix_name.clone().into()),
            ("LOGNAME".into(), p.unix_name.clone().into()),
            ("SHELL".into(), p.shell.clone().into_os_string()),
            (
                "PATH".into(),
                self.config
                    .principal_path
                    .clone()
                    .unwrap_or_else(|| default_path(&p.home)),
            ),
            ("TMPDIR".into(), tmpdir.into_os_string()),
        ];
        env.extend(std::env::vars_os().filter(|(k, _)| is_locale_env(k)));
        env.extend(
            spec.env
                .vars
                .iter()
                .filter(|(k, _)| !is_denied_env(k))
                .cloned(),
        );
        env.extend(host_env.iter().cloned());
        env
    }

    /// Everything about the spawn except the async-only knobs.
    fn std_command(
        &self,
        spec: &SpawnSpec,
        opts: &PipedOpts,
        host_env: &[(OsString, OsString)],
    ) -> io::Result<std::process::Command> {
        let p = self.admit(spec)?;
        for (k, _) in host_env {
            if !crate::pty::is_host_only_env(&k.to_string_lossy()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{} is not a host-only variable; pass it in the spec instead",
                        k.to_string_lossy()
                    ),
                ));
            }
        }
        // §9.3: never the broker's cwd. A missing cwd is the home, and a
        // relative one (pty_ws asks for ".") is resolved against the home —
        // chdir() after fork would otherwise resolve it against the
        // broker's inherited cwd.
        let cwd = match &spec.cwd {
            Some(cwd) if cwd.is_absolute() => cwd.clone(),
            Some(cwd) => p.home.join(cwd),
            None => p.home.clone(),
        };
        let mut cmd = std::process::Command::new(&spec.program);
        cmd.args(&spec.args)
            .current_dir(cwd)
            .stdin(opts.stdin.to_stdio())
            .stdout(opts.stdout.to_stdio())
            .stderr(opts.stderr.to_stdio())
            .env_clear()
            .envs(self.environment(p, spec, host_env))
            .gid(p.gid)
            .uid(p.uid);
        if opts.new_process_group || opts.detached {
            cmd.process_group(0);
        }
        let (uid, gid) = (p.uid, p.gid);
        #[cfg(test)]
        let fault = fault::injected();
        // SAFETY: the closure runs in the forked child before exec. It only
        // makes raw syscalls on captured `u32`s and returns an `Os` error —
        // no allocation, lock, logging or NSS lookup (§9.1).
        unsafe {
            cmd.pre_exec(move || {
                #[cfg(test)]
                if fault {
                    return Err(io::Error::from_raw_os_error(DROP_VERIFY_ERRNO));
                }
                verify_dropped(uid, gid)
            });
        }
        Ok(cmd)
    }

    /// The spawn's error, after noting a failed drop verification.
    fn after_spawn<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if let Err(e) = &result {
            if e.raw_os_error() == Some(DROP_VERIFY_ERRNO) {
                self.mark_degraded("a spawned child failed its privilege-drop verification");
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the child failed its privilege-drop verification (G-PRINCIPAL §9.2); \
                     the T1 executor is now degraded",
                ));
            }
        }
        result
    }

    /// A synchronous spawn the caller waits on itself — for the root CLI's
    /// uid-wide kill, which must bound the wait (a process of the uid could
    /// SIGSTOP the helper) and kill the helper from root on a timeout.
    /// Same admission, environment, drop and verification as every spawn.
    pub fn spawn_std(&self, spec: SpawnSpec, opts: PipedOpts) -> io::Result<std::process::Child> {
        if opts.detached {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "spawn_std is for children the caller waits on; `detached` makes no sense here",
            ));
        }
        let mut cmd = self.std_command(&spec, &opts, &[])?;
        self.after_spawn(cmd.spawn())
    }

    /// [`SessionExecutor::spawn_piped`] plus explicitly listed host-only
    /// variables (§9.3's one exception): the broker's per-child token and the
    /// `IKENGA_SECRET_*` operator defaults. Every entry must be host-only —
    /// anything else belongs in the spec.
    pub fn spawn_piped_with_host_env(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
        host_env: &[(OsString, OsString)],
    ) -> io::Result<tokio::process::Child> {
        let mut cmd = tokio::process::Command::from(self.std_command(&spec, &opts, host_env)?);
        cmd.kill_on_drop(opts.kill_on_drop && !opts.detached);
        self.after_spawn(cmd.spawn())
    }
}

/// The §9.2 checks, in the forked child after std's drop and before `exec`.
/// Any failure returns [`DROP_VERIFY_ERRNO`], so the spawn fails.
fn verify_dropped(uid: u32, gid: u32) -> io::Result<()> {
    let fail = || Err(io::Error::from_raw_os_error(DROP_VERIFY_ERRNO));
    let (mut r, mut e, mut s) = (0, 0, 0);
    // SAFETY: plain syscalls on stack out-params.
    unsafe {
        // 1. Real, effective and saved ids all dropped.
        if libc::getresuid(&mut r, &mut e, &mut s) != 0 || (r, e, s) != (uid, uid, uid) {
            return fail();
        }
        if libc::getresgid(&mut r, &mut e, &mut s) != 0 || (r, e, s) != (gid, gid, gid) {
            return fail();
        }
        // 2. No supplementary groups (std ignores a setgroups EPERM).
        if libc::getgroups(0, std::ptr::null_mut()) != 0 {
            return fail();
        }
        // 3. Root can't be regained.
        if libc::setuid(0) != -1 || *libc::__errno_location() != libc::EPERM {
            return fail();
        }
        // 4. OD-9: no privilege through setuid binaries either.
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1
        {
            return fail();
        }
    }
    Ok(())
}

impl SessionExecutor for T1Executor {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tier: ExecutorTier::T1,
            // The broker's own executor spawns no PTYs: they live in the
            // already-dropped principal child (§9.2, topology B).
            pty: false,
            piped: true,
            // I-5: only from a probe that passed in this process, and only
            // while no spawn has failed its drop verification.
            principal_isolation: self.probe.is_some_and(|s| s.ok) && !self.is_degraded(),
        }
    }

    fn probe_stamp(&self) -> Option<ProbeStamp> {
        self.probe
    }

    fn spawn_pty(&self, spec: SpawnSpec, _size: PtySize) -> anyhow::Result<PtyChild> {
        self.admit(&spec)?;
        anyhow::bail!(
            "T1 spawns no PTY from the broker: terminals run inside the principal's own child, \
             which is already dropped to its uid (G-PRINCIPAL §9.2, topology B)"
        )
    }

    fn spawn_piped(&self, spec: SpawnSpec, opts: PipedOpts) -> io::Result<tokio::process::Child> {
        self.spawn_piped_with_host_env(spec, opts, &[])
    }

    fn spawn_output_blocking(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> io::Result<std::process::Output> {
        if opts.detached {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "spawn_output_blocking waits for the child; `detached` makes no sense here",
            ));
        }
        let mut cmd = self.std_command(&spec, &opts, &[])?;
        self.after_spawn(cmd.output())
    }
}

/// `ikenga-server __t1-kill-all` — the §7.3 uid-wide kill helper, spawned
/// **through the T1 executor as the principal**: `kill(-1, SIGKILL)` reaches
/// every process that uid may signal (Linux skips the caller and init),
/// detached chi-runners in their own process groups included.
///
/// It refuses to run as root — `kill(-1)` as root would take down the host —
/// and unless `IKENGA_T1_KILL_ALL_UID` names its own uid, so an accidental
/// manual run can't fire it.
pub fn kill_all_entry() -> Result<(), String> {
    // SAFETY: no preconditions.
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid == 0 || euid == 0 {
        return Err("refusing to kill(-1) as root".into());
    }
    let expected = std::env::var(KILL_ALL_UID_ENV).ok();
    if expected.as_deref() != Some(uid.to_string().as_str()) {
        return Err(format!(
            "{KILL_ALL_UID_ENV} must name this process's uid ({uid}); refusing"
        ));
    }
    // SAFETY: plain syscall.
    if unsafe { libc::kill(-1, libc::SIGKILL) } != 0 {
        let e = io::Error::last_os_error();
        // ESRCH: nothing of this uid's was running.
        if e.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("kill(-1, SIGKILL): {e}"));
        }
    }
    Ok(())
}

/// Must equal the helper's own uid (see [`kill_all_entry`]).
pub const KILL_ALL_UID_ENV: &str = "IKENGA_T1_KILL_ALL_UID";

/// Test-only fault injection: makes the next spawns' `pre_exec` report a
/// failed verification, to exercise the degraded latch without a broken host.
#[cfg(test)]
pub(crate) mod fault {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FAIL_VERIFY: AtomicBool = AtomicBool::new(false);

    pub(crate) fn injected() -> bool {
        FAIL_VERIFY.load(Ordering::SeqCst)
    }

    pub(crate) fn set(on: bool) {
        FAIL_VERIFY.store(on, Ordering::SeqCst);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::executor::{PrincipalId, StdioMode};

    pub(crate) fn principal(uid: u32, home: impl Into<PathBuf>) -> Principal {
        Principal {
            id: PrincipalId::new_v7(),
            username: format!("t1-test-{uid}"),
            unix_name: format!("ik-t1-test-{uid}"),
            uid,
            gid: uid,
            home: home.into(),
            shell: "/bin/sh".into(),
        }
    }

    pub(crate) fn config() -> T1Config {
        T1Config {
            principals_dir: "/srv/ikenga/principals".into(),
            principal_path: None,
        }
    }

    pub(crate) fn piped() -> PipedOpts {
        PipedOpts {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
            kill_on_drop: true,
            no_console_window: false,
            detached: false,
            new_process_group: false,
        }
    }

    fn sh(script: &str, p: Option<Principal>) -> SpawnSpec {
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c").arg(script).principal(p);
        spec
    }

    /// I-1: no principal, or a root one, never spawns — and is refused
    /// before any fork (so this holds unprivileged, too).
    #[tokio::test]
    async fn i1_spawns_without_a_principal_or_as_root_are_refused() {
        let exec = T1Executor::new(config());
        for p in [
            None,
            Some(principal(0, "/root")),
            Some(Principal {
                gid: 0,
                ..principal(20_000, "/h")
            }),
            Some(Principal {
                uid: 0,
                ..principal(20_000, "/h")
            }),
        ] {
            let err = exec
                .spawn_piped(sh("true", p.clone()), piped())
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{p:?}: {err}");
            let err = exec
                .spawn_output_blocking(sh("true", p.clone()), piped())
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{p:?}: {err}");
            assert!(exec.spawn_pty(sh("true", p), PtySize::default()).is_err());
        }
        assert!(!exec.is_degraded(), "a refusal is not drift");
    }

    #[test]
    fn pty_is_refused_even_with_a_principal() {
        let exec = T1Executor::new(config());
        let err = exec
            .spawn_pty(
                sh("true", Some(principal(20_000, "/h"))),
                PtySize::default(),
            )
            .err()
            .expect("refused");
        assert!(err.to_string().contains("PTY"), "{err}");
    }

    /// §9.3: the floor, then the spec's vars minus the deny floor.
    #[test]
    fn environment_is_the_floor_then_the_filtered_spec() {
        let exec = T1Executor::new(config());
        let p = principal(20_001, "/srv/ikenga/principals/x/home");
        let mut spec = sh("true", Some(p.clone()));
        spec.env("KEEP", "1")
            .env("IKENGA_AUTH_TOKEN", "tok")
            .env("IKENGA_VAULT_KEY", "k")
            .env("IKENGA_PKG_DB_TOKEN", "d")
            .env("IKENGA_SECRET_OPENAI", "s")
            .env("IKENGA_BOOTSTRAP_ADMIN", "ada")
            .env("IKENGA_BOOTSTRAP_ADMIN_PASSWORD", "pw")
            .env("PATH", "/custom");
        let env = exec.environment(&p, &spec, &[]);
        let get = |k: &str| {
            env.iter()
                .rev()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(
            get("HOME").as_deref(),
            Some("/srv/ikenga/principals/x/home")
        );
        assert_eq!(get("USER").as_deref(), Some("ik-t1-test-20001"));
        assert_eq!(get("LOGNAME").as_deref(), Some("ik-t1-test-20001"));
        assert_eq!(get("SHELL").as_deref(), Some("/bin/sh"));
        assert_eq!(
            get("TMPDIR"),
            Some(format!("/srv/ikenga/principals/{}/data/tmp", p.id))
        );
        assert_eq!(get("KEEP").as_deref(), Some("1"));
        // The spec's own vars come after the floor and win.
        assert_eq!(get("PATH").as_deref(), Some("/custom"));
        for denied in [
            "IKENGA_AUTH_TOKEN",
            "IKENGA_VAULT_KEY",
            "IKENGA_PKG_DB_TOKEN",
            "IKENGA_SECRET_OPENAI",
            "IKENGA_BOOTSTRAP_ADMIN",
            "IKENGA_BOOTSTRAP_ADMIN_PASSWORD",
        ] {
            assert_eq!(get(denied), None, "{denied} must not reach a principal");
        }
        // Nothing of the broker's own environment beyond locale vars.
        for (k, _) in &env {
            let k = k.to_string_lossy();
            assert!(
                ["HOME", "USER", "LOGNAME", "SHELL", "PATH", "TMPDIR", "KEEP", "LANG", "TZ"]
                    .contains(&k.as_ref())
                    || k.starts_with("LC_"),
                "unexpected {k}"
            );
        }

        // Default PATH (§9.3) and the operator's --principal-path.
        let spec = sh("true", Some(p.clone()));
        let env = exec.environment(&p, &spec, &[]);
        let path = env.iter().find(|(k, _)| k == "PATH").unwrap().1.clone();
        assert_eq!(
            path,
            OsString::from("/srv/ikenga/principals/x/home/.local/bin:/usr/local/bin:/usr/bin:/bin")
        );
        let exec = T1Executor::new(T1Config {
            principal_path: Some("/opt/bin:/usr/bin".into()),
            ..config()
        });
        let env = exec.environment(&p, &spec, &[]);
        let path = env.iter().find(|(k, _)| k == "PATH").unwrap().1.clone();
        assert_eq!(path, OsString::from("/opt/bin:/usr/bin"));
    }

    /// The one exception is explicit, and only for host-only names.
    #[test]
    fn host_env_exception_is_explicit_and_host_only() {
        let exec = T1Executor::new(config());
        let p = principal(20_002, "/h");
        let spec = sh("true", Some(p.clone()));
        let env = exec.environment(
            &p,
            &spec,
            &[("IKENGA_AUTH_TOKEN".into(), "per-child".into())],
        );
        assert!(env
            .iter()
            .any(|(k, v)| k == "IKENGA_AUTH_TOKEN" && v == "per-child"));
        let err = exec
            .std_command(&spec, &piped(), &[("NOT_HOST_ONLY".into(), "x".into())])
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// §9.3 / review S2-3: the child's cwd is never relative to the broker's.
    #[test]
    fn cwd_is_the_home_or_resolved_against_it() {
        let exec = T1Executor::new(config());
        let home = PathBuf::from("/srv/ikenga/principals/x/home");
        let p = principal(20_003, &home);
        let cwd_of = |cwd: Option<&str>| {
            let mut spec = sh("true", Some(p.clone()));
            if let Some(cwd) = cwd {
                spec.current_dir(cwd);
            }
            let cmd = exec.std_command(&spec, &piped(), &[]).unwrap();
            cmd.get_current_dir().map(PathBuf::from)
        };
        assert_eq!(cwd_of(None), Some(home.clone()));
        assert_eq!(cwd_of(Some(".")), Some(home.join(".")));
        assert_eq!(cwd_of(Some("proj/a")), Some(home.join("proj/a")));
        assert_eq!(
            cwd_of(Some("/srv/other")),
            Some(PathBuf::from("/srv/other"))
        );
    }

    /// I-5: isolation is reported only from a passing probe, and drift turns
    /// it off and refuses every later spawn.
    #[tokio::test]
    async fn i5_isolation_needs_a_probe_and_no_drift() {
        assert!(!T1Executor::new(config()).capabilities().principal_isolation);
        let stamp = ProbeStamp { ok: true, at: 1 };
        let exec = T1Executor::with_probe(config(), stamp);
        let caps = exec.capabilities();
        assert_eq!(caps.tier, ExecutorTier::T1);
        assert!(caps.principal_isolation && caps.piped && !caps.pty);
        assert_eq!(exec.probe_stamp(), Some(stamp));

        // A verification failure reported by a child marks it degraded.
        let res: io::Result<()> = Err(io::Error::from_raw_os_error(DROP_VERIFY_ERRNO));
        let err = exec.after_spawn(res).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(exec.is_degraded());
        assert!(!exec.capabilities().principal_isolation);
        let err = exec
            .spawn_piped(sh("true", Some(principal(20_000, "/"))), piped())
            .unwrap_err();
        assert!(err.to_string().contains("degraded"), "{err}");

        // An ordinary spawn error is not drift.
        let exec = T1Executor::with_probe(config(), stamp);
        let res: io::Result<()> = Err(io::Error::from_raw_os_error(libc::ENOENT));
        assert!(exec.after_spawn(res).is_err());
        assert!(!exec.is_degraded());
    }

    #[test]
    fn kill_all_entry_refuses_without_its_marker() {
        // Whoever runs the suite: as root it refuses for being root; as
        // anyone else, for the missing marker. Either way nothing is killed.
        std::env::remove_var(KILL_ALL_UID_ENV);
        assert!(kill_all_entry().is_err());
    }

    /// Real drops. Root only: `#[ignore]`d and run by the `t1-root` CI job
    /// (`--ignored --test-threads=1 t1_root`) and locally as root.
    pub(crate) mod t1_root {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        pub(crate) fn require_root() {
            // SAFETY: no preconditions.
            assert_eq!(unsafe { libc::geteuid() }, 0, "t1-root tests run as root");
        }

        /// A root-created dir any uid can traverse, with a `home/` owned by
        /// `uid`.
        pub(crate) fn home_for(uid: u32) -> (tempfile::TempDir, PathBuf) {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
            let home = tmp.path().join("home");
            std::fs::create_dir(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::lchown(&home, Some(uid), Some(uid)).unwrap();
            (tmp, home)
        }

        fn status_field<'a>(status: &'a str, key: &str) -> &'a str {
            status
                .lines()
                .find_map(|l| l.strip_prefix(key))
                .unwrap_or_else(|| panic!("{key} missing"))
                .trim()
        }

        /// I-1 + I-2: the child runs as the principal, with no groups, no
        /// capabilities, NNP set, the §9.3 env and cwd = home.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_piped_spawn_drops_to_the_principal() {
            require_root();
            let uid = 28_501;
            let (_tmp, home) = home_for(uid);
            let exec = T1Executor::new(config());
            let mut spec = sh(
                r#"cat /proc/self/status; echo "CWD=$(pwd -P)"; echo "ENV_HOME=$HOME"; \
                   echo "ENV_USER=$USER"; echo "AUTH=${IKENGA_AUTH_TOKEN-unset}""#,
                Some(principal(uid, &home)),
            );
            spec.env("IKENGA_AUTH_TOKEN", "leak");
            let out = exec
                .spawn_piped(spec, piped())
                .unwrap()
                .wait_with_output()
                .await
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            let s = String::from_utf8_lossy(&out.stdout);
            let ids = format!("{uid}\t{uid}\t{uid}\t{uid}");
            assert_eq!(status_field(&s, "Uid:"), ids);
            assert_eq!(status_field(&s, "Gid:"), ids);
            assert_eq!(status_field(&s, "Groups:"), "");
            assert_eq!(status_field(&s, "NoNewPrivs:"), "1");
            assert_eq!(status_field(&s, "CapEff:"), "0000000000000000");
            assert_eq!(status_field(&s, "CapPrm:"), "0000000000000000");
            assert_eq!(status_field(&s, "CWD="), home.display().to_string());
            assert_eq!(status_field(&s, "ENV_HOME="), home.display().to_string());
            assert_eq!(status_field(&s, "ENV_USER="), format!("ik-t1-test-{uid}"));
            assert_eq!(status_field(&s, "AUTH="), "unset");
            assert!(!exec.is_degraded());
        }

        /// I-2: root can't be regained from inside the child.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_blocking_spawn_cannot_regain_root() {
            require_root();
            let uid = 28_502;
            let (_tmp, home) = home_for(uid);
            let exec = T1Executor::new(config());
            // `su`/`sudo` may be absent from a slim image; NNP plus no
            // capabilities is what makes any setuid binary inert, and the
            // pre_exec already proved setuid(0) == EPERM. Here: a root-owned
            // 0700 dir is unreadable, and writing to / fails.
            let sealed = _tmp.path().join("sealed");
            std::fs::create_dir(&sealed).unwrap();
            std::fs::set_permissions(&sealed, std::fs::Permissions::from_mode(0o700)).unwrap();
            let out = exec
                .spawn_output_blocking(
                    sh(
                        &format!(
                            "ls {} >/dev/null 2>&1 && echo READ; touch /t1-root-probe 2>/dev/null \
                             && echo WROTE; echo done",
                            sealed.display()
                        ),
                        Some(principal(uid, &home)),
                    ),
                    piped(),
                )
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
            assert!(!std::path::Path::new("/t1-root-probe").exists());
        }

        /// §8 runtime drift: a failed verification refuses that spawn, then
        /// every later one, and turns isolation off.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_a_failed_verification_degrades_the_executor() {
            require_root();
            let uid = 28_503;
            let (_tmp, home) = home_for(uid);
            let exec = T1Executor::with_probe(config(), ProbeStamp { ok: true, at: 1 });
            fault::set(true);
            let res = exec.spawn_piped(sh("true", Some(principal(uid, &home))), piped());
            fault::set(false);
            let err = res.unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
            assert!(exec.is_degraded());
            assert!(!exec.capabilities().principal_isolation);
            assert!(exec
                .spawn_piped(sh("true", Some(principal(uid, &home))), piped())
                .is_err());
        }

        /// Review S2-6: the verifier itself, with no fault injection. A child
        /// that never dropped (still root), and one dropped to the wrong ids,
        /// both fail with `DROP_VERIFY_ERRNO` before exec.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_the_verifier_rejects_a_child_that_did_not_drop() {
            require_root();
            let spawn = |drop_to: Option<u32>| {
                let mut cmd = std::process::Command::new("/bin/true");
                cmd.current_dir("/")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                if let Some(id) = drop_to {
                    cmd.uid(id).gid(id);
                }
                // SAFETY: as in `std_command`: raw syscalls only.
                unsafe {
                    cmd.pre_exec(|| verify_dropped(28_505, 28_505));
                }
                cmd.spawn().map(|mut c| c.wait())
            };
            // Still root: getresuid says (0, 0, 0).
            let err = spawn(None).expect_err("a root child must fail verification");
            assert_eq!(err.raw_os_error(), Some(DROP_VERIFY_ERRNO), "{err}");
            // Dropped, but not to the principal's ids.
            let err = spawn(Some(28_506)).expect_err("wrong ids must fail verification");
            assert_eq!(err.raw_os_error(), Some(DROP_VERIFY_ERRNO), "{err}");
            // The right ids pass (and the child runs).
            let status = spawn(Some(28_505)).unwrap().unwrap();
            assert!(status.success());
        }

        /// Review S2-3: a relative cwd is the home's, not the broker's.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_a_relative_cwd_is_resolved_against_the_home() {
            require_root();
            let uid = 28_507;
            let (_tmp, home) = home_for(uid);
            let exec = T1Executor::new(config());
            let mut spec = sh("pwd -P", Some(principal(uid, &home)));
            spec.current_dir(".");
            let out = exec.spawn_output_blocking(spec, piped()).unwrap();
            assert!(out.status.success(), "{out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                home.display().to_string()
            );
        }

        /// A missing cwd is the principal's home, never the broker's cwd.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_cwd_defaults_to_home_and_unreachable_cwd_fails() {
            require_root();
            let uid = 28_504;
            let (_tmp, home) = home_for(uid);
            let exec = T1Executor::new(config());
            let out = exec
                .spawn_output_blocking(sh("pwd -P", Some(principal(uid, &home))), piped())
                .unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                home.display().to_string()
            );
            // chdir runs as the uid: a root-only dir is refused, not entered.
            let mut spec = sh("pwd", Some(principal(uid, &home)));
            spec.current_dir("/root");
            if std::fs::metadata("/root")
                .map(|m| m.permissions().mode() & 0o001 == 0)
                .unwrap_or(false)
            {
                assert!(exec.spawn_output_blocking(spec, piped()).is_err());
            }
            assert!(!exec.is_degraded(), "a chdir failure is not drift");
        }
    }
}
