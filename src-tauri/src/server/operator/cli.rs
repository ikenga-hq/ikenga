//! `ikenga-server accounts …` (G-PRINCIPAL §7.1): local operator
//! administration. Root only; never a network surface (no HTTP route creates
//! an account — no self-signup, D3).
//!
//! Passwords **never** come from argv (G-90): they are read from the
//! controlling TTY with echo off, or from stdin with `--password-stdin`.

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use zeroize::Zeroizing;

use super::accounts::{self, Actor, DeviceGrantsRevoked};
use super::adopt_t0;
use super::provision::{Adopt, Provisioner, ProvisioningMode, ReapOutcome, UidRange, UidReaper};
use super::reaper::{HelperCommand, T1Reaper};
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
    /// §11.2: migrate a T0 data dir (and, for a fresh principal, the old
    /// home's dot-dirs) into `username`'s principal. Creates the account
    /// when it doesn't exist (`admin` and `password` apply only then).
    AdoptT0 {
        username: String,
        from: PathBuf,
        home: PathBuf,
        admin: bool,
        password: PasswordSource,
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
    let cmd = match cmd {
        AccountsCommand::AdoptT0 {
            username,
            from,
            home,
            admin,
            password,
        } => {
            let cwd = std::env::current_dir()?;
            AccountsCommand::AdoptT0 {
                username,
                from: cwd.join(from),
                home: cwd.join(home),
                admin,
                password,
            }
        }
        other => other,
    };
    let opener = if matches!(
        cmd,
        AccountsCommand::Create { .. } | AccountsCommand::AdoptT0 { .. }
    ) {
        // Only `create` (and `adopt-t0`, which may create the account) may
        // initialise a brand-new store (the first admin, §7.4, before any
        // T1 boot).
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
    // §7.3's uid-wide kill: this binary's `__t1-kill-all`, spawned through
    // the T1 executor as the principal.
    let reaper = T1Reaper::new(&root, HelperCommand::current_exe()?);
    let prov = Provisioner::new(root, opts.uid_range, opts.provisioning, Actor::Cli);
    // Output is buffered and written after the command, never through a held
    // `StdoutLock`: holding it across awaits deadlocks against any log event
    // another thread emits (sqlx's workers at debug), and logs go to stderr
    // anyway (`main.rs`), so `list --json` stays clean.
    let mut out = Vec::new();
    let result = run_with(&prov, &pool, &reaper, cmd, &mut read_password, &mut out).await;
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
    reaper: &dyn UidReaper,
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
            let report = prov.disable(pool, &username, reaper).await?;
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
                ReapOutcome::Failed(why) => {
                    writeln!(
                        out,
                        "WARNING: could not kill the processes of uid {}: {why}",
                        report.account.unix_uid
                    )?;
                    anyhow::bail!(
                        "{} is disabled, but its running processes may not have been stopped",
                        report.account.username
                    );
                }
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
                .revoke_sessions(pool, &username, &DeviceGrantsRevoked { via_cli: true })
                .await?;
            writeln!(
                out,
                "revoked every session of {} (session epoch {})",
                a.username, a.session_epoch
            )?;
        }
        AccountsCommand::AdoptT0 {
            username,
            from,
            home,
            admin,
            password,
        } => {
            // Everything about the old install is checked before an
            // account is created, so a bad --from or --home leaves nothing
            // behind. A failure after that (in `migrate`) can leave the new
            // account in place; re-running the same command then finds it
            // and goes on (`--admin` included, when it is already admin).
            let pre = adopt_t0::preflight(prov.root(), &from, &home, &adopt_t0::stamp_now())?;
            let existing = {
                let mut conn = pool.acquire().await?;
                accounts::by_username(&mut conn, &username).await?
            };
            let account = match existing {
                Some(a) => {
                    if admin && !a.is_admin {
                        anyhow::bail!(
                            "`{username}` already exists and is not an admin; --admin only \
                             applies when adopt-t0 creates the account"
                        );
                    }
                    a
                }
                None => {
                    accounts::unix_name_for(&username)?;
                    let pw = read_password(password)?;
                    let a = prov.create(pool, &username, &pw, admin, None).await?;
                    writeln!(
                        out,
                        "created {}account {} ({}) as {} uid {}",
                        if a.is_admin { "admin " } else { "" },
                        a.username,
                        a.principal_id,
                        a.unix_name,
                        a.unix_uid
                    )?;
                    a
                }
            };
            let report =
                adopt_t0::migrate(prov.root(), prov.ownership(), &account, &pre, reaper).await?;
            write!(out, "{}", report.summary())?;
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
    use crate::server::operator::provision::NoReaper;
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
            run_with(&prov, &pool, &NoReaper, cmd, &mut pw, &mut out)
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
            &NoReaper,
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
        assert!(
            run_with(&prov, &pool, &NoReaper, cmd, &mut pw, &mut Vec::new())
                .await
                .is_err()
        );
        let cmd = AccountsCommand::Passwd {
            username: "ghost".into(),
            password: PasswordSource::Tty,
        };
        assert!(
            run_with(&prov, &pool, &NoReaper, cmd, &mut pw, &mut Vec::new())
                .await
                .is_err()
        );
        assert!(!prompted);
    }

    /// `adopt-t0` for a name with no account: preflight first (a bad
    /// --from creates nothing and prompts for nothing), then create + copy.
    #[tokio::test]
    async fn adopt_t0_creates_the_account_and_migrates() {
        let (root_tmp, root) = test_support::temp_root();
        let (etc_tmp, _etc) = fake_etc(true);
        let range = UidRange::new(3_900_000_060, 3_900_000_070).unwrap();
        let prov = Provisioner::for_tests(root.clone(), range, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let base = std::fs::canonicalize(root_tmp.path()).unwrap();
        let (from, home) = (base.join("t0"), base.join("home"));
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::write(home.join(".codex/auth.json"), "{}").unwrap();
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("supabase.json"), "{}").unwrap();
        std::fs::write(from.join("access.db"), "chain").unwrap();

        let prompted = std::cell::Cell::new(0);
        let mut pw = |_: PasswordSource| {
            prompted.set(prompted.get() + 1);
            Ok(Zeroizing::new("correct horse battery".to_string()))
        };
        let bad = AccountsCommand::AdoptT0 {
            username: "ada".into(),
            from: base.join("missing"),
            home: home.clone(),
            admin: true,
            password: PasswordSource::Stdin,
        };
        assert!(
            run_with(&prov, &pool, &NoReaper, bad, &mut pw, &mut Vec::new())
                .await
                .is_err()
        );
        {
            let mut conn = pool.acquire().await.unwrap();
            assert_eq!(accounts::count(&mut conn).await.unwrap(), 0);
        }

        let mut out = Vec::new();
        let cmd = AccountsCommand::AdoptT0 {
            username: "ada".into(),
            from: from.clone(),
            home: home.clone(),
            admin: true,
            password: PasswordSource::Stdin,
        };
        run_with(&prov, &pool, &NoReaper, cmd, &mut pw, &mut out)
            .await
            .unwrap();
        assert_eq!(prompted.get(), 1);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("created admin account ada"), "{text}");
        assert!(text.contains("copy into a fresh principal"), "{text}");
        assert!(text.contains("holds access.db"), "{text}");
        let a = {
            let mut conn = pool.acquire().await.unwrap();
            accounts::by_username(&mut conn, "ada")
                .await
                .unwrap()
                .unwrap()
        };
        let data = root.principal_data(a.principal_id);
        assert!(data.join("supabase.json").exists());
        assert!(!data.join("access.db").exists(), "R-10");
        assert!(a.home.join(".codex/auth.json").exists());

        // An existing account is never re-created. Re-running the same
        // command (--admin included: ada is admin) finds it and goes on,
        // without a password prompt; here it stops at the used data dir.
        std::fs::create_dir_all(base.join("t0b")).unwrap();
        std::fs::write(base.join("t0b/ikenga.db"), b"").unwrap();
        let again = |username: &str| AccountsCommand::AdoptT0 {
            username: username.into(),
            from: base.join("t0b"),
            home: home.clone(),
            admin: true,
            password: PasswordSource::Stdin,
        };
        let err = run_with(
            &prov,
            &pool,
            &NoReaper,
            again("ada"),
            &mut pw,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("never run"), "{err}");
        assert_eq!(prompted.get(), 1);
        // --admin on an existing account that is not admin is refused.
        prov.create(&pool, "bob", "correct horse battery", false, None)
            .await
            .unwrap();
        let err = run_with(
            &prov,
            &pool,
            &NoReaper,
            again("bob"),
            &mut pw,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--admin"), "{err}");
        // Unseal the archive for the tempdir cleanup.
        for e in std::fs::read_dir(&base).unwrap() {
            let p = e.unwrap().path();
            if p.to_string_lossy().contains(".t0-migrated-") {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
    }

    struct BrokenReaper;
    impl UidReaper for BrokenReaper {
        fn kill_all(
            &self,
            _p: &crate::executor::Principal,
        ) -> anyhow::Result<super::super::provision::ReapOutcome> {
            anyhow::bail!("no CAP_SETUID here")
        }
    }

    /// A failed uid-wide kill is loud: the account is disabled (and says
    /// so), but the command fails so the operator sees the warning.
    #[tokio::test]
    async fn disable_reports_a_failed_kill_and_exits_nonzero() {
        let (_root_tmp, root) = test_support::temp_root();
        let (etc_tmp, _etc) = fake_etc(true);
        let range = UidRange::new(3_900_000_040, 3_900_000_050).unwrap();
        let prov = Provisioner::for_tests(root.clone(), range, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let mut pw = |_: PasswordSource| Ok(Zeroizing::new("correct horse battery".to_string()));
        let create = AccountsCommand::Create {
            username: "ada".into(),
            admin: false,
            adopt_unix_user: None,
            allow_system_user: false,
            password: PasswordSource::Stdin,
        };
        run_with(&prov, &pool, &NoReaper, create, &mut pw, &mut Vec::new())
            .await
            .unwrap();
        let mut out = Vec::new();
        let disable = AccountsCommand::Disable {
            username: "ada".into(),
        };
        let err = run_with(&prov, &pool, &BrokenReaper, disable, &mut pw, &mut out)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is disabled"), "{err}");
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("disabled ada"), "{text}");
        assert!(
            text.contains("WARNING") && text.contains("no CAP_SETUID"),
            "{text}"
        );
        let mut conn = pool.acquire().await.unwrap();
        assert!(accounts::by_username(&mut conn, "ada")
            .await
            .unwrap()
            .unwrap()
            .is_disabled());
    }
}
