//! T0 — in-process executor. Every child runs as the host process's own user,
//! exactly as the call sites spawned it before `SessionExecutor` existed.
//!
//! "Byte-for-byte the current spawn" is the whole contract of this file. Each
//! method issues the same builder calls, in the same order, that the call site
//! used to issue inline; the call site still decides every env / cwd / PATH
//! value (the PTY's `env_clear` + `is_host_only_env` rebuild, the engines'
//! augmented `PATH`) and hands the result over in the `SpawnSpec`.

use anyhow::Context;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::process::Command;

use super::{
    probe, Capabilities, ExecutorTier, PipedOpts, PtyChild, SessionExecutor, SpawnSpec,
};
use crate::platform::NoConsoleWindow;

/// The T0 executor. Stateless; `principal` on the spec is ignored.
#[derive(Debug, Clone, Copy, Default)]
pub struct InProcessExecutor;

impl SessionExecutor for InProcessExecutor {
    fn capabilities(&self) -> Capabilities {
        probe(ExecutorTier::T0).expect("t0 always passes its own probe")
    }

    fn spawn_pty(&self, spec: SpawnSpec, size: PtySize) -> anyhow::Result<PtyChild> {
        let pair = native_pty_system().openpty(size).context("openpty")?;

        let mut builder = CommandBuilder::new(&spec.program);
        if !spec.args.is_empty() {
            builder.args(&spec.args);
        }
        if let Some(cwd) = &spec.cwd {
            builder.cwd(cwd);
        }
        // `CommandBuilder` seeds itself from the parent environment at
        // construction, so a spec that wants a filtered environment must clear
        // it first — see the long note at the PTY call site.
        if spec.env.clear {
            builder.env_clear();
        }
        for (k, v) in &spec.env.vars {
            builder.env(k, v);
        }

        let child = pair.slave.spawn_command(builder).context("spawn child")?;
        Ok(PtyChild {
            master: pair.master,
            slave: pair.slave,
            child,
        })
    }

    fn spawn_piped(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<tokio::process::Child> {
        let mut cmd = Command::from(std_command(&spec, &opts));
        if opts.detached {
            return spawn_detached(cmd);
        }
        cmd.kill_on_drop(opts.kill_on_drop);
        cmd.spawn()
    }

    fn spawn_output_blocking(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<std::process::Output> {
        if opts.detached {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "spawn_output_blocking waits for the child; `detached` makes no sense here",
            ));
        }
        std_command(&spec, &opts).output()
    }
}

/// The builder calls shared by the tokio ([`SessionExecutor::spawn_piped`])
/// and blocking ([`SessionExecutor::spawn_output_blocking`]) paths: program,
/// args, cwd, stdio, env (clear first, then vars in order),
/// `no_console_window` and `new_process_group`. The async-only knobs
/// (`kill_on_drop`, `detached`) are applied by the caller on the tokio
/// wrapper.
fn std_command(spec: &SpawnSpec, opts: &PipedOpts) -> std::process::Command {
    let mut cmd = std::process::Command::new(&spec.program);
    if opts.no_console_window {
        cmd.no_console_window();
    }
    cmd.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.stdin(opts.stdin.to_stdio())
        .stdout(opts.stdout.to_stdio())
        .stderr(opts.stderr.to_stdio());
    if spec.env.clear {
        cmd.env_clear();
    }
    for (k, v) in &spec.env.vars {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    if opts.new_process_group {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// The `PipedOpts::detached` spawn. `kill_on_drop` is forced off: the caller
/// drops the handle on purpose and the child must keep running.
///
/// `no_console_window` needs no separate handling here: Windows'
/// `creation_flags` *replaces* the flags already set, and the detached flag
/// set includes `CREATE_NO_WINDOW` itself.
fn spawn_detached(mut cmd: Command) -> std::io::Result<tokio::process::Child> {
    cmd.kill_on_drop(false);

    #[cfg(unix)]
    {
        // Own process group (pgid == pid): out of reach of a signal aimed at
        // the app's group, and killable as a tree with `kill(-pid, sig)`.
        cmd.process_group(0);
        cmd.spawn()
    }

    #[cfg(windows)]
    {
        use crate::platform::{CREATE_BREAKAWAY_FROM_JOB, DETACHED_PROCESS_FLAGS};
        // Win32 ERROR_ACCESS_DENIED: what CreateProcess returns when the job
        // the app runs in does not allow breakaway.
        const ERROR_ACCESS_DENIED: i32 = 5;
        // OWED (Windows live check, WP-18b): the breakaway attempt, its
        // fallback, and that the detached child outlives the app are
        // unverified on a real Windows host — written and tested on Linux.
        cmd.creation_flags(DETACHED_PROCESS_FLAGS | CREATE_BREAKAWAY_FROM_JOB);
        match cmd.spawn() {
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                cmd.creation_flags(DETACHED_PROCESS_FLAGS);
                cmd.spawn()
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Principal, PrincipalId, StdioMode};

    /// Reserved on T0: any resolved principal, which T0 must ignore.
    fn someone_else() -> Principal {
        Principal {
            id: PrincipalId::new_v7(),
            username: "someone-else".into(),
            unix_name: "ik-someone-else".into(),
            uid: 20000,
            gid: 20000,
            home: "/nonexistent".into(),
            shell: "/bin/sh".into(),
        }
    }

    fn piped_all() -> PipedOpts {
        PipedOpts {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
            kill_on_drop: true,
            no_console_window: true,
            detached: false,
            new_process_group: false,
        }
    }

    #[cfg(unix)]
    fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        // macOS tempdirs live behind the /var -> /private/var symlink, and a
        // shell with no inherited $PWD reports the physical path.
        let canon = std::fs::canonicalize(tmp.path()).unwrap();
        (tmp, canon)
    }

    /// T0 piped spawn delivers the spec's env and cwd to the child, and a
    /// cleared env really is cleared.
    #[cfg(unix)]
    #[tokio::test]
    async fn piped_spawn_round_trips_env_and_cwd() {
        let (_tmp, dir) = canonical_tempdir();
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c")
            .arg(r#"printf '%s|%s|%s' "$WP18_PROBE" "$(pwd -P)" "${HOME-unset}""#)
            .env_clear()
            .env("WP18_PROBE", "first")
            // Later entries win, as with repeated `Command::env`.
            .env("WP18_PROBE", "hello world")
            .current_dir(&dir)
            // Reserved; T0 must ignore it rather than fail.
            .principal(Some(someone_else()));

        let child = InProcessExecutor.spawn_piped(spec, piped_all()).unwrap();
        let out = child.wait_with_output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("hello world|{}|unset", dir.display())
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn piped_spawn_round_trips_env_and_cwd() {
        // Not canonicalised: that yields a `\\?\` verbatim path, which cmd.exe
        // refuses as a working directory. Two plain-argv spawns sidestep
        // cmd's quoting rules entirely.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let run = |args: &[&str]| {
            let mut spec = SpawnSpec::new("cmd.exe");
            spec.arg("/d")
                .args(args.iter())
                .env("WP18_PROBE", "first")
                .env("WP18_PROBE", "hello")
                .current_dir(&dir)
                .principal(Some(someone_else()));
            InProcessExecutor.spawn_piped(spec, piped_all()).unwrap()
        };

        let out = run(&["/c", "set", "WP18_PROBE"]).wait_with_output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "WP18_PROBE=hello");

        let out = run(&["/c", "cd"]).wait_with_output().await.unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim().to_lowercase(),
            dir.display().to_string().to_lowercase()
        );
    }

    #[tokio::test]
    async fn piped_spawn_reports_a_missing_program_as_io_error() {
        let spec = SpawnSpec::new("ikenga-wp18-definitely-not-a-binary");
        let err = InProcessExecutor
            .spawn_piped(spec, piped_all())
            .expect_err("no such program");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    /// T0 PTY spawn delivers the spec's env and cwd to a child on the slave
    /// side of a real PTY.
    #[cfg(unix)]
    #[test]
    fn pty_spawn_round_trips_env_and_cwd() {
        use std::io::Read;

        let (_tmp, dir) = canonical_tempdir();
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c")
            .arg(r#"printf 'BEGIN:%s|%s:END' "$WP18_PROBE" "$(pwd -P)""#)
            .env_clear()
            .env("WP18_PROBE", "pty-ok")
            .current_dir(&dir);

        let PtyChild {
            master,
            slave,
            mut child,
        } = InProcessExecutor
            .spawn_pty(
                spec,
                PtySize {
                    rows: 24,
                    cols: 200,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            )
            .unwrap();
        // Drop our slave handle so the reader sees EOF/EIO once the child exits.
        drop(slave);

        let mut reader = master.try_clone_reader().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        out.extend_from_slice(&buf[..n]);
                        if out.windows(4).any(|w| w == b":END") {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(out);
        });

        let out = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("pty output within 10s");
        let status = child.wait().unwrap();
        assert!(status.success(), "{status:?}");
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains(&format!("BEGIN:pty-ok|{}:END", dir.display())),
            "unexpected pty output: {text:?}"
        );
        drop(master);
    }

    #[test]
    fn capabilities_are_t0() {
        let caps = InProcessExecutor.capabilities();
        assert_eq!(caps.tier, ExecutorTier::T0);
        assert!(!caps.principal_isolation);
    }

    /// True once `pid` no longer runs: gone, or a zombie nobody has reaped
    /// yet (containers often have no init reaping re-parented orphans).
    #[cfg(unix)]
    fn gone_or_zombie(pid: i32) -> bool {
        #[cfg(target_os = "linux")]
        {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => true,
                // `pid (comm) S ...` — the state follows the last `)`.
                Ok(stat) => stat
                    .rsplit_once(')')
                    .map(|(_, rest)| rest.trim_start().starts_with('Z'))
                    .unwrap_or(false),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            unsafe { libc::kill(pid, 0) != 0 }
        }
    }

    #[cfg(unix)]
    async fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if done() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        done()
    }

    #[cfg(unix)]
    fn sleeper(detached: bool) -> (SpawnSpec, PipedOpts) {
        let mut spec = SpawnSpec::new("sleep");
        spec.arg("30");
        let opts = PipedOpts {
            stdin: StdioMode::Null,
            stdout: StdioMode::Null,
            stderr: StdioMode::Null,
            // Asked for, and overridden by `detached`.
            kill_on_drop: true,
            no_console_window: true,
            detached,
            new_process_group: false,
        };
        (spec, opts)
    }

    /// WP-18b: a detached child leads its own process group and outlives its
    /// dropped `Child` handle even though `kill_on_drop: true` was asked for.
    #[cfg(unix)]
    #[tokio::test]
    async fn detached_child_has_its_own_group_and_survives_a_dropped_handle() {
        let (spec, opts) = sleeper(true);
        let child = InProcessExecutor.spawn_piped(spec, opts).unwrap();
        let pid = child.id().expect("pid") as i32;
        assert_eq!(unsafe { libc::getpgid(pid) }, pid, "own process group");
        assert_ne!(unsafe { libc::getpgid(pid) }, unsafe { libc::getpgid(0) });

        drop(child);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let alive = !gone_or_zombie(pid);
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        assert!(
            alive,
            "detached child must survive its handle being dropped"
        );
    }

    /// Non-detached behaviour is unchanged: the child shares our process
    /// group and `kill_on_drop` still kills it with the handle.
    #[cfg(unix)]
    #[tokio::test]
    async fn non_detached_child_keeps_kill_on_drop_and_our_group() {
        let (spec, opts) = sleeper(false);
        let child = InProcessExecutor.spawn_piped(spec, opts).unwrap();
        let pid = child.id().expect("pid") as i32;
        assert_eq!(unsafe { libc::getpgid(pid) }, unsafe { libc::getpgid(0) });

        drop(child);
        let killed = wait_until(|| gone_or_zombie(pid)).await;
        if !killed {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(killed, "kill_on_drop must still kill a non-detached child");
    }

    /// WP-18b: `new_process_group` gives the child its own group (so the
    /// caller can signal `-pid`) WITHOUT detaching it: `kill_on_drop` is
    /// still honoured when the handle is dropped.
    #[cfg(unix)]
    #[tokio::test]
    async fn new_process_group_child_leads_its_group_and_keeps_kill_on_drop() {
        let (spec, mut opts) = sleeper(false);
        opts.new_process_group = true;
        let child = InProcessExecutor.spawn_piped(spec, opts).unwrap();
        let pid = child.id().expect("pid") as i32;
        assert_eq!(unsafe { libc::getpgid(pid) }, pid, "own process group");
        assert_ne!(unsafe { libc::getpgid(pid) }, unsafe { libc::getpgid(0) });

        drop(child);
        let killed = wait_until(|| gone_or_zombie(pid)).await;
        if !killed {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        assert!(killed, "kill_on_drop must still kill a new-group child");
    }

    /// The blocking path collects stdout / stderr / status like
    /// `Command::output`, and applies the spec's env (cleared) and cwd.
    #[cfg(unix)]
    #[test]
    fn output_blocking_collects_output_and_applies_env_and_cwd() {
        let (_tmp, dir) = canonical_tempdir();
        let mut spec = SpawnSpec::new("/bin/sh");
        spec.arg("-c")
            .arg(
                r#"printf '%s|%s|%s' "$WP18_PROBE" "$(pwd -P)" "${HOME-unset}"; printf 'oops' 1>&2; exit 7"#,
            )
            .env_clear()
            .env("WP18_PROBE", "first")
            .env("WP18_PROBE", "blocking")
            .current_dir(&dir);
        let out = InProcessExecutor
            .spawn_output_blocking(spec, piped_all())
            .unwrap();
        assert_eq!(out.status.code(), Some(7));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("blocking|{}|unset", dir.display())
        );
        assert_eq!(String::from_utf8_lossy(&out.stderr), "oops");
    }

    /// The blocking path shares the flag application, `new_process_group`
    /// included: the child's pgid is its own pid only when asked for.
    #[cfg(unix)]
    #[test]
    fn output_blocking_honours_new_process_group() {
        let run = |new_group: bool| {
            let mut spec = SpawnSpec::new("/bin/sh");
            spec.arg("-c").arg("echo $$ $(ps -o pgid= -p $$)");
            let mut opts = piped_all();
            opts.new_process_group = new_group;
            let out = InProcessExecutor.spawn_output_blocking(spec, opts).unwrap();
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let mut it = text.split_whitespace();
            let pid: i32 = it.next().expect("pid").parse().unwrap();
            let pgid: i32 = it.next().expect("pgid").parse().unwrap();
            (pid, pgid)
        };
        let (pid, pgid) = run(true);
        assert_eq!(pid, pgid, "own group");
        let (pid, pgid) = run(false);
        assert_ne!(pid, pgid, "inherits our group");
        assert_eq!(pgid, unsafe { libc::getpgid(0) });
    }

    #[test]
    fn output_blocking_refuses_detached_and_reports_missing_programs() {
        let mut opts = piped_all();
        opts.detached = true;
        let err = InProcessExecutor
            .spawn_output_blocking(SpawnSpec::new("true"), opts)
            .expect_err("detached refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        let err = InProcessExecutor
            .spawn_output_blocking(
                SpawnSpec::new("ikenga-wp18-definitely-not-a-binary"),
                piped_all(),
            )
            .expect_err("no such program");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
