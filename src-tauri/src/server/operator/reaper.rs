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

use std::ffi::OsString;
use std::path::PathBuf;

use super::provision::{ReapOutcome, UidReaper};
use super::OperatorRoot;
use crate::executor::t1::{T1Config, T1Executor, KILL_ALL_UID_ENV};
use crate::executor::{PipedOpts, Principal, SessionExecutor, SpawnSpec, StdioMode};

/// The hidden argv entry that runs `executor::t1::kill_all_entry`.
pub const KILL_ALL_ARG: &str = "__t1-kill-all";

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
}

impl T1Reaper {
    pub fn new(root: &OperatorRoot, helper: HelperCommand) -> Self {
        Self {
            executor: T1Executor::new(T1Config {
                principals_dir: root.principals_dir(),
                principal_path: None,
            }),
            helper,
        }
    }
}

impl UidReaper for T1Reaper {
    fn kill_all(&self, principal: &Principal) -> anyhow::Result<ReapOutcome> {
        let mut spec = SpawnSpec::new(&self.helper.program);
        spec.args(&self.helper.args)
            .env(KILL_ALL_UID_ENV, principal.uid.to_string())
            // Not the home: a disabled principal's files may be gone.
            .current_dir("/")
            .principal(Some(principal.clone()));
        let out = self.executor.spawn_output_blocking(
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
        )?;
        if !out.status.success() {
            anyhow::bail!(
                "{} {} as uid {} exited {}: {}",
                self.helper.program.display(),
                KILL_ALL_ARG,
                principal.uid,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(ReapOutcome::Killed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::t1::tests::{principal, t1_root};
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

    fn test_helper() -> HelperCommand {
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
}
