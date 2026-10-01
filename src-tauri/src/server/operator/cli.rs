//! `ikenga-server accounts …` (G-PRINCIPAL §7.1): local operator
//! administration. Root only; never a network surface (no HTTP route creates
//! an account — no self-signup, D3).
//!
//! Passwords **never** come from argv (G-90): they are read from the
//! controlling TTY with echo off, or from stdin with `--password-stdin`.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use zeroize::Zeroizing;

use super::accounts::{self, Actor, NoDeviceGrants};
use super::provision::{
    Adopt, Provisioner, ProvisioningMode, ReapOutcome, ReaperPendingT1Executor, UidRange,
};
use super::{open_accounts, sys, Opener, OperatorRoot, Ownership};

/// Options shared by every `accounts` subcommand.
#[derive(Debug, Clone)]
pub struct AccountsOptions {
    /// The operator root (`--data-dir`, P-1).
    pub data_dir: PathBuf,
    pub uid_range: UidRange,
    pub provisioning: ProvisioningMode,
}

/// Where a new password comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordSource {
    /// Prompt twice on `/dev/tty` with echo off.
    Tty,
    /// One line from stdin (`--password-stdin`).
    Stdin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountsCommand {
    Create {
        username: String,
        admin: bool,
        adopt_unix_user: Option<String>,
        /// With `adopt_unix_user`: permit a system user (uid < `UID_MIN`).
        allow_system_user: bool,
        password: PasswordSource,
    },
    Passwd {
        username: String,
        password: PasswordSource,
    },
    Disable {
        username: String,
    },
    Enable {
        username: String,
    },
    /// Forced logout (G-ACCESS R-11): bump `session_epoch` and run the
    /// sessions-revoked hook.
    RevokeSessions {
        username: String,
    },
    List {
        json: bool,
    },
}

/// Entry point for `main.rs`: checks `euid == 0`, prepares the operator root
/// (refusing a T0 layout, I-10), opens `accounts.db` as the CLI (never
/// migrating an existing set, §6.1), and runs `cmd`.
pub async fn run(opts: AccountsOptions, cmd: AccountsCommand) -> anyhow::Result<()> {
    if sys::geteuid() != 0 {
        anyhow::bail!(
            "`ikenga-server accounts` must run as root (it writes the root-owned operator store \
             and provisions Unix users)"
        );
    }
    let data_dir = if opts.data_dir.is_absolute() {
        opts.data_dir.clone()
    } else {
        std::env::current_dir()?.join(&opts.data_dir)
    };
    let root = OperatorRoot::new(data_dir)?;
    let opener = if matches!(cmd, AccountsCommand::Create { .. }) {
        // Only `create` may initialise a brand-new store (the first admin,
        // §7.4, before any T1 boot).
        Opener::Cli
    } else {
        // Everything else needs an existing, current store and creates
        // nothing: a mistyped --data-dir must not grow an operator root.
        root.refuse_t0_layout()?;
        if !root.accounts_db().exists() {
            anyhow::bail!(
                "no operator store at {} (no {}); check --data-dir, or create the first account \
                 with `ikenga-server accounts create <name> --admin`",
                root.root().display(),
                root.accounts_db().display()
            );
        }
        Opener::CliExisting
    };
    root.prepare(Ownership::Enforce)?;
    let pool = open_accounts(&root, opener).await?;
    let prov = Provisioner::new(root, opts.uid_range, opts.provisioning, Actor::Cli);
    // Output is buffered and written after the command, never through a held
    // `StdoutLock`: holding it across awaits deadlocks against any log event
    // another thread emits (sqlx's workers at debug), and logs go to stderr
    // anyway (`main.rs`), so `list --json` stays clean.
    let mut out = Vec::new();
    let result = run_with(&prov, &pool, cmd, &mut read_password, &mut out).await;
    pool.close().await;
    let written = {
        let mut stdout = io::stdout();
        stdout.write_all(&out).and_then(|()| stdout.flush())
    };
    result?;
    Ok(written?)
}

/// [`run`] after setup, with the password reader and output injectable.
pub(crate) async fn run_with(
    prov: &Provisioner,
    pool: &sqlx::SqlitePool,
    cmd: AccountsCommand,
    read_password: &mut dyn FnMut(PasswordSource) -> anyhow::Result<Zeroizing<String>>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    match cmd {
        AccountsCommand::Create {
            username,
            admin,
            adopt_unix_user,
            allow_system_user,
            password,
        } => {
            // Validate the name before prompting for a password.
            if adopt_unix_user.is_none() {
                accounts::unix_name_for(&username)?;
                if allow_system_user {
                    anyhow::bail!("--allow-system-user only applies with --adopt-unix-user");
                }
            }
            let pw = read_password(password)?;
            let adopt = adopt_unix_user.as_deref().map(|unix_user| Adopt {
                unix_user,
                allow_system_user,
            });
            let a = prov.create(pool, &username, &pw, admin, adopt).await?;
            writeln!(
                out,
                "created {}account {} ({}) as {} uid {} home {} [backend: {}]",
                if a.is_admin { "admin " } else { "" },
                a.username,
                a.principal_id,
                a.unix_name,
                a.unix_uid,
                a.home.display(),
                prov.backend_name()
            )?;
        }
        AccountsCommand::Passwd { username, password } => {
            {
                let mut conn = pool.acquire().await?;
                if accounts::by_username(&mut conn, &username).await?.is_none() {
                    anyhow::bail!("no account named `{username}`");
                }
            }
            let pw = read_password(password)?;
            let a = prov.passwd(pool, &username, &pw).await?;
            writeln!(
                out,
                "password changed for {}; every session of it is revoked (session epoch {})",
                a.username, a.session_epoch
            )?;
        }
        AccountsCommand::Disable { username } => {
            let report = prov
                .disable(pool, &username, &ReaperPendingT1Executor)
                .await?;
            writeln!(
                out,
                "disabled {} ({}); sessions revoked (session epoch {}); passwd entry locked",
                report.account.username, report.account.principal_id, report.account.session_epoch
            )?;
            match report.reap {
                ReapOutcome::Killed => writeln!(
                    out,
                    "killed every process of uid {}",
                    report.account.unix_uid
                )?,
                ReapOutcome::Unavailable(why) => writeln!(out, "note: no processes killed: {why}")?,
            }
        }
        AccountsCommand::Enable { username } => {
            let a = prov.enable(pool, &username).await?;
            writeln!(
                out,
                "enabled {} ({}); shell {}",
                a.username,
                a.principal_id,
                a.shell.display()
            )?;
        }
        AccountsCommand::RevokeSessions { username } => {
            let a = prov
                .revoke_sessions(pool, &username, &NoDeviceGrants)
                .await?;
            writeln!(
                out,
                "revoked every session of {} (session epoch {})",
                a.username, a.session_epoch
            )?;
        }
        AccountsCommand::List { json } => {
            let mut conn = pool.acquire().await?;
            let all = accounts::list(&mut conn).await?;
            if json {
                serde_json::to_writer_pretty(&mut *out, &all)?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "{:<20} {:<36} {:<20} {:>10} {:<5} {:<8}",
                    "USERNAME", "PRINCIPAL_ID", "UNIX_NAME", "UID", "ADMIN", "STATUS"
                )?;
                for a in all {
                    writeln!(
                        out,
                        "{:<20} {:<36} {:<20} {:>10} {:<5} {:<8}",
                        a.username,
                        a.principal_id,
                        a.unix_name,
                        a.unix_uid,
                        if a.is_admin { "yes" } else { "no" },
                        if a.is_disabled() {
                            "disabled"
                        } else {
                            "active"
                        }
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// Read a new password from `source`.
pub fn read_password(source: PasswordSource) -> anyhow::Result<Zeroizing<String>> {
    match source {
        PasswordSource::Stdin => read_password_line(&mut io::stdin().lock()),
        PasswordSource::Tty => {
            let first = tty::prompt("New password: ")?;
            let second = tty::prompt("Repeat password: ")?;
            if *first != *second {
                anyhow::bail!("passwords do not match");
            }
            Ok(first)
        }
    }
}

/// One line, without its `\n` / `\r\n`. An empty stream is an error.
pub(crate) fn read_password_line(input: &mut dyn BufRead) -> anyhow::Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(String::new());
    if input.read_line(&mut line)? == 0 {
        anyhow::bail!("--password-stdin: no password on stdin");
    }
    let trimmed = line.trim_end_matches(['\n', '\r']).len();
    line.truncate(trimmed);
    Ok(line)
}

mod tty {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;

    use zeroize::Zeroizing;

    /// Restores the terminal's attributes when dropped.
    struct EchoOff {
        fd: i32,
        saved: libc::termios,
    }

    impl Drop for EchoOff {
        fn drop(&mut self) {
            // SAFETY: `fd` is open for the guard's lifetime; `saved` came from
            // tcgetattr on it.
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.saved);
            }
        }
    }

    pub(super) fn prompt(text: &str) -> anyhow::Result<Zeroizing<String>> {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|e| anyhow::anyhow!("no terminal to prompt on ({e}); use --password-stdin"))?;
        let fd = tty.as_raw_fd();
        // SAFETY: all-zero is a valid termios to be overwritten by tcgetattr.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is a valid open fd; `saved` is writable.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            anyhow::bail!("/dev/tty: {}", std::io::Error::last_os_error());
        }
        let mut quiet = saved;
        quiet.c_lflag &= !(libc::ECHO | libc::ECHONL);
        quiet.c_lflag |= libc::ICANON;
        let guard = EchoOff { fd, saved };
        // SAFETY: as above; `quiet` is a modified copy of the current state.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
            anyhow::bail!("/dev/tty: {}", std::io::Error::last_os_error());
        }
        (&tty).write_all(text.as_bytes())?;
        (&tty).flush()?;
        let mut line = Zeroizing::new(String::new());
        BufReader::new(&tty).read_line(&mut line)?;
        drop(guard);
        (&tty).write_all(b"\n")?;
        let trimmed = line.trim_end_matches(['\n', '\r']).len();
        line.truncate(trimmed);
        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::operator::etc_files::tests::fake_etc;
    use crate::server::operator::{open_accounts, test_support};

    #[test]
    fn stdin_password_strips_the_line_ending_only() {
        for (input, want) in [
            ("hunter2 hunter2\n", "hunter2 hunter2"),
            ("crlf password\r\n", "crlf password"),
            ("  spaces kept  \n", "  spaces kept  "),
            ("no newline", "no newline"),
        ] {
            let got = read_password_line(&mut io::Cursor::new(input)).unwrap();
            assert_eq!(got.as_str(), want);
        }
        assert!(read_password_line(&mut io::Cursor::new("")).is_err());
    }

    #[tokio::test]
    async fn create_list_disable_enable_round_trip() {
        let (_root_tmp, root) = test_support::temp_root();
        let (etc_tmp, _etc) = fake_etc(true);
        let range = UidRange::new(3_900_000_020, 3_900_000_030).unwrap();
        let prov = Provisioner::for_tests(root.clone(), range, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let mut reads = Vec::new();
        let mut pw = |src: PasswordSource| {
            reads.push(src);
            Ok(Zeroizing::new("correct horse battery".to_string()))
        };
        let mut out = Vec::new();
        for cmd in [
            AccountsCommand::Create {
                username: "ada".into(),
                admin: true,
                adopt_unix_user: None,
                allow_system_user: false,
                password: PasswordSource::Stdin,
            },
            AccountsCommand::Passwd {
                username: "ada".into(),
                password: PasswordSource::Tty,
            },
            AccountsCommand::Disable {
                username: "ada".into(),
            },
            AccountsCommand::Enable {
                username: "ada".into(),
            },
            AccountsCommand::RevokeSessions {
                username: "ada".into(),
            },
            AccountsCommand::List { json: false },
        ] {
            run_with(&prov, &pool, cmd, &mut pw, &mut out)
                .await
                .unwrap();
        }
        assert_eq!(reads, [PasswordSource::Stdin, PasswordSource::Tty]);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("created admin account ada"), "{text}");
        assert!(text.contains("note: no processes killed"), "{text}");
        assert!(text.contains("ik-ada"), "{text}");
        assert!(!text.contains("argon2"), "never print a hash: {text}");

        let mut out = Vec::new();
        let mut no_prompt = |_: PasswordSource| -> anyhow::Result<Zeroizing<String>> {
            panic!("list never prompts")
        };
        run_with(
            &prov,
            &pool,
            AccountsCommand::List { json: true },
            &mut no_prompt,
            &mut out,
        )
        .await
        .unwrap();
        let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(rows[0]["username"], "ada");
        assert_eq!(rows[0]["unix_uid"], 3_900_000_020u32);
        assert_eq!(
            rows[0]["session_epoch"], 3,
            "passwd + disable + revoke-sessions"
        );
        assert!(rows[0].get("password_phc").is_none());
    }

    #[tokio::test]
    async fn bad_usernames_fail_before_any_password_prompt() {
        let (_root_tmp, root) = test_support::temp_root();
        let (etc_tmp, _etc) = fake_etc(true);
        let prov = Provisioner::for_tests(root.clone(), UidRange::DEFAULT, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let mut prompted = false;
        let mut pw = |_: PasswordSource| {
            prompted = true;
            Ok(Zeroizing::new(String::new()))
        };
        let cmd = AccountsCommand::Create {
            username: "no.dots".into(),
            admin: false,
            adopt_unix_user: None,
            allow_system_user: false,
            password: PasswordSource::Tty,
        };
        assert!(run_with(&prov, &pool, cmd, &mut pw, &mut Vec::new())
            .await
            .is_err());
        let cmd = AccountsCommand::Passwd {
            username: "ghost".into(),
            password: PasswordSource::Tty,
        };
        assert!(run_with(&prov, &pool, cmd, &mut pw, &mut Vec::new())
            .await
            .is_err());
        assert!(!prompted);
    }
}
