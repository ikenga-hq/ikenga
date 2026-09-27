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
        let mut cmd = Command::new(&spec.program);
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
        cmd.kill_on_drop(opts.kill_on_drop);
        cmd.spawn()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Principal, StdioMode};

    fn piped_all() -> PipedOpts {
        PipedOpts {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
            kill_on_drop: true,
            no_console_window: true,
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
            .principal(Some(Principal { id: "someone-else".into() }));

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
                .principal(Some(Principal { id: "someone-else".into() }));
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
}
