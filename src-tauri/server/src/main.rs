use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ikenga_desktop_lib::executor::ExecutorTier;
use ikenga_desktop_lib::server::{run_server_with, ServerConfig, T1ServeOptions};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "ikenga-server")]
#[command(author = "Ikenga")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Ikenga Headless Server Daemon", long_about = None)]
// The flat flags stay the implicit default (`serve`), so systemd's
// `ExecStart` and the Docker `ENTRYPOINT` don't change (G-PRINCIPAL §7.1).
// A subcommand and serve flags never mix.
#[command(args_conflicts_with_subcommands = true)]
pub struct CliArgs {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub serve: ServeArgs,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Administer T1 local accounts (root only; G-PRINCIPAL §7). Passwords
    /// are read from the terminal or `--password-stdin`, never from argv.
    Accounts(AccountsArgs),
    /// Check whether this host can run an executor tier (G-PRINCIPAL §8).
    /// For t1: identity, capabilities, the operator root and a real test
    /// drop to the reserved probe uid — read-only (no reconcile, no
    /// probe.json). Exits 0 when the tier can run, 1 when it can't.
    Probe(ProbeArgs),
    /// Internal: the §8 test-drop child (spawned by the probe as the probe uid).
    #[command(name = "__t1-probe-child", hide = true)]
    T1ProbeChild,
    /// Internal: the §7.3 uid-wide kill (spawned as the principal's uid).
    #[command(name = "__t1-kill-all", hide = true)]
    T1KillAll,
}

#[derive(Args, Debug)]
pub struct ProbeArgs {
    /// The tier to probe.
    #[arg(long, env = "IKENGA_EXECUTOR_TIER")]
    pub executor_tier: ExecutorTier,

    /// The T1 operator root (the server's `--data-dir` under t1).
    #[arg(long, env = "IKENGA_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// The operator's uid range, `START-END`; `END` is the probe uid.
    #[arg(long, env = "IKENGA_UID_RANGE", default_value = "20000-29999")]
    pub uid_range: String,

    /// Print the full report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct AccountsArgs {
    /// The T1 operator root — the same directory the server's `--data-dir`
    /// names under `--executor-tier t1`.
    #[arg(long, env = "IKENGA_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// Unix uid range for new accounts, `START-END`. `END` is reserved for the
    /// boot probe and never allocated.
    #[arg(
        long,
        env = "IKENGA_UID_RANGE",
        default_value = "20000-29999",
        global = true
    )]
    pub uid_range: String,

    /// `auto`: useradd/groupadd when on PATH, else a built-in /etc writer.
    /// `external`: never write /etc; adopt pre-created users.
    #[arg(
        long,
        env = "IKENGA_PROVISIONING",
        value_enum,
        default_value = "auto",
        global = true
    )]
    pub provisioning: Provisioning,

    #[command(subcommand)]
    pub command: AccountsCommand,
}

/// Mirrors `operator::provision::ProvisioningMode` (that module is Linux-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Provisioning {
    Auto,
    External,
}

#[derive(Args, Debug)]
pub struct PasswordArgs {
    /// Read the password from one line of stdin instead of prompting.
    #[arg(long)]
    pub password_stdin: bool,
}

#[derive(Subcommand, Debug)]
pub enum AccountsCommand {
    /// Create an account and provision its Unix user and directories.
    Create {
        username: String,
        /// May administer accounts.
        #[arg(long)]
        admin: bool,
        /// Map the account onto this existing Unix user instead of allocating one.
        #[arg(long, value_name = "NAME")]
        adopt_unix_user: Option<String>,
        /// With --adopt-unix-user: allow a system user (uid below login.defs
        /// UID_MIN). Root, nobody and the probe uid are refused regardless.
        #[arg(long, requires = "adopt_unix_user")]
        allow_system_user: bool,
        #[command(flatten)]
        password: PasswordArgs,
    },
    /// Set a new password; revokes every session of the account.
    Passwd {
        username: String,
        #[command(flatten)]
        password: PasswordArgs,
    },
    /// Disable an account: revoke its sessions, lock its Unix user, stop its processes.
    Disable { username: String },
    /// Re-enable a disabled account.
    Enable { username: String },
    /// Forced logout: revoke every session of the account.
    RevokeSessions { username: String },
    /// List every account, disabled ones included.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1", env = "IKENGA_HOST")]
    pub host: String,

    /// Port to listen on
    #[arg(long, default_value_t = 4000, env = "IKENGA_PORT")]
    pub port: u16,

    /// Directory containing static frontend assets (shell/dist)
    #[arg(long, default_value = "./dist", env = "IKENGA_STATIC_DIR")]
    pub static_dir: PathBuf,

    /// Directory containing installed mini-app packages. Walked once at
    /// startup; every pkg with an `iframe` UI route and a `dist/` is served
    /// read-only from `GET /pkgs/<id>/*` behind the auth token. The daemon
    /// installs nothing — point this at a directory something else populates.
    #[arg(long, env = "IKENGA_PKGS_DIR")]
    pub pkgs_dir: Option<PathBuf>,

    /// Data directory for SQLite database and vaults
    #[arg(long, env = "IKENGA_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// Bearer token required on every API and WebSocket route. One is
    /// generated and printed at startup if you don't supply it — the server
    /// never runs unauthenticated.
    ///
    /// Prefer the `IKENGA_AUTH_TOKEN` environment variable (or a systemd
    /// `EnvironmentFile=`) over `--auth-token`: command-line arguments are
    /// readable by every local user (`/proc/<pid>/cmdline`, `ps`), and this
    /// token grants a shell. The flag stays for one-off manual runs.
    #[arg(long, env = "IKENGA_AUTH_TOKEN")]
    pub auth_token: Option<String>,

    /// Extra origin permitted to call the API cross-site, e.g.
    /// `http://localhost:5173` for a Vite dev server. Repeatable. Same-origin
    /// requests never need this.
    #[arg(
        long = "allow-origin",
        env = "IKENGA_ALLOW_ORIGINS",
        value_delimiter = ','
    )]
    pub allowed_origins: Vec<String>,

    /// Idle timeout in seconds before server automatically shuts down when no sessions are active.
    #[arg(long, env = "IKENGA_IDLE_TIMEOUT")]
    pub idle_timeout: Option<u64>,

    /// Session-executor isolation tier (ADR-023): `t0` in-process, `t1`
    /// per-user uid, `t2` container per session, `t3` firejail per session.
    /// `t1` runs its boot probe (G-PRINCIPAL §8) and, on this build, then
    /// refuses to serve (its broker is not built yet); `t2`/`t3` are refused.
    /// A refused tier never falls back to a weaker one.
    #[arg(long, env = "IKENGA_EXECUTOR_TIER", default_value = "t0")]
    pub executor_tier: ExecutorTier,

    /// T1 only: the uid range for accounts, `START-END` (`END` is the boot
    /// probe's reserved uid). Must match the range the accounts store was
    /// first provisioned with.
    #[arg(long, env = "IKENGA_UID_RANGE")]
    pub uid_range: Option<String>,

    /// T1 only: `auto` (useradd, else a built-in /etc writer) or `external`
    /// (never write /etc).
    #[arg(long, env = "IKENGA_PROVISIONING", value_enum, default_value = "auto")]
    pub provisioning: Provisioning,

    /// T1 only: the PATH a principal's processes get. Default:
    /// `<home>/.local/bin:/usr/local/bin:/usr/bin:/bin`.
    #[arg(long, env = "IKENGA_PRINCIPAL_PATH")]
    pub principal_path: Option<std::ffi::OsString>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let CliArgs {
        command,
        serve: args,
    } = CliArgs::parse();

    // The internal entries run as a dropped principal uid, with a cleared
    // environment: no logging setup, nothing else — check, report, exit.
    match command {
        Some(Command::T1ProbeChild) => internal_entry("t1 probe child", t1_probe_child()),
        Some(Command::T1KillAll) => internal_entry("t1 kill-all", t1_kill_all()),
        _ => {}
    }

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,ikenga_server=debug".into());
    if command.is_some() {
        // Subcommands log to stderr: their stdout is their output
        // (`accounts list --json` must stay parseable), and stdout is only
        // ever written once the command is done (see `operator::cli::run`).
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .init();
    } else {
        // `serve` keeps logging to stdout, unchanged for systemd and Docker.
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .init();
    }

    // §7.4 first-admin bootstrap: captured and removed from the environment
    // here, beside the token stripping below and for the same reason — every
    // child would otherwise inherit an admin password.
    let bootstrap = take_bootstrap_admin();

    // Clap has read these into `args`; drop them from the process
    // environment before anything can inherit them.
    //
    // Every PTY this daemon spawns inherits the parent environment wholesale
    // (`pty::PtyManager::spawn_inner`), so without this the bearer token that
    // grants a terminal is readable FROM a terminal — `echo $IKENGA_AUTH_TOKEN`
    // hands any session a credential that outlives it and survives revoking
    // the client. `pty` also filters these by name; this is the belt to that
    // pair of braces, and the one that also covers anything else the process
    // may spawn later.
    //
    // TRAP FOR LATER: this runs BEFORE `run_server`, so anything downstream
    // that expects to read these from the environment will find them gone.
    // `IKENGA_AUTH_TOKEN` is safe because clap has already put it in `config`;
    // `IKENGA_VAULT_KEY` currently has no reader at all. When a headless vault
    // lands it must capture the value HERE, into `config`, rather than calling
    // `env::var` inside `run_server` — that call will always return `Err`.
    //
    // Safety: single-threaded here — the Tokio worker pool is running but no
    // task of ours has started, and `run_server` is called below.
    for key in ["IKENGA_AUTH_TOKEN", "IKENGA_VAULT_KEY"] {
        std::env::remove_var(key);
    }

    match command {
        Some(Command::Accounts(accounts)) => {
            if bootstrap.is_some() {
                tracing::warn!(
                    "IKENGA_BOOTSTRAP_ADMIN is only read by the T1 server at boot; ignored by \
                     `accounts`"
                );
            }
            return run_accounts(accounts).await;
        }
        Some(Command::Probe(probe)) => std::process::exit(run_probe(probe).await),
        Some(Command::T1ProbeChild | Command::T1KillAll) => unreachable!("handled above"),
        None => {}
    }

    if bootstrap.is_some() {
        if args.executor_tier == ExecutorTier::T1 {
            // Honoured by the T1 broker after its §8 boot probe, only on an
            // empty accounts table (`Provisioner::bootstrap_admin`). The broker
            // lands in a later WP-20 slice; on this build T1 refuses to boot.
            tracing::info!("IKENGA_BOOTSTRAP_ADMIN captured for the T1 broker");
        } else {
            tracing::warn!(
                "IKENGA_BOOTSTRAP_ADMIN ignored: local accounts exist only under --executor-tier t1"
            );
        }
    }
    drop(bootstrap);

    let config = ServerConfig {
        host: args.host,
        port: args.port,
        static_dir: args.static_dir,
        pkgs_dir: args.pkgs_dir,
        data_dir: args.data_dir,
        auth_token: args.auth_token,
        allowed_origins: args.allowed_origins,
        idle_timeout_secs: args.idle_timeout,
        executor_tier: args.executor_tier,
    };
    let t1 = T1ServeOptions {
        uid_range: args.uid_range,
        provisioning_external: args.provisioning == Provisioning::External,
        principal_path: args.principal_path,
    };

    run_server_with(config, t1).await
}

/// Exit with the internal entry's verdict: 0, or 1 with the reason on stderr.
fn internal_entry(name: &str, result: Result<(), String>) -> ! {
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("{name}: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
fn t1_probe_child() -> Result<(), String> {
    ikenga_desktop_lib::executor::t1_probe::probe_child_entry()
}

#[cfg(target_os = "linux")]
fn t1_kill_all() -> Result<(), String> {
    ikenga_desktop_lib::executor::t1::kill_all_entry()
}

#[cfg(not(target_os = "linux"))]
fn t1_probe_child() -> Result<(), String> {
    Err("executor tier t1 is Linux-only".into())
}

#[cfg(not(target_os = "linux"))]
fn t1_kill_all() -> Result<(), String> {
    Err("executor tier t1 is Linux-only".into())
}

/// `ikenga-server probe` → exit code (0 = the tier can run here).
async fn run_probe(args: ProbeArgs) -> i32 {
    if args.executor_tier != ExecutorTier::T1 {
        return match ikenga_desktop_lib::executor::probe(args.executor_tier) {
            Ok(caps) => {
                println!("{} probe: PASS {caps:?}", args.executor_tier);
                0
            }
            Err(refusal) => {
                println!("{} probe: FAIL {refusal}", args.executor_tier);
                1
            }
        };
    }
    probe_t1(args).await
}

#[cfg(target_os = "linux")]
async fn probe_t1(args: ProbeArgs) -> i32 {
    use ikenga_desktop_lib::server::operator::{probe, provision::UidRange};
    let uid_range = match args.uid_range.parse::<UidRange>() {
        Ok(range) => range,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    probe::cli(args.data_dir, uid_range, args.json).await
}

#[cfg(not(target_os = "linux"))]
async fn probe_t1(_args: ProbeArgs) -> i32 {
    println!("t1 probe: FAIL at `os`: executor tier t1 is Linux-only");
    1
}

#[cfg(target_os = "linux")]
type BootstrapAdmin = ikenga_desktop_lib::server::operator::provision::BootstrapAdmin;
#[cfg(not(target_os = "linux"))]
type BootstrapAdmin = ();

#[cfg(target_os = "linux")]
fn take_bootstrap_admin() -> Option<BootstrapAdmin> {
    match BootstrapAdmin::take_from_env() {
        Ok(bootstrap) => bootstrap,
        Err(why) => {
            tracing::warn!("{why}");
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn take_bootstrap_admin() -> Option<BootstrapAdmin> {
    for key in ["IKENGA_BOOTSTRAP_ADMIN", "IKENGA_BOOTSTRAP_ADMIN_PASSWORD"] {
        std::env::remove_var(key);
    }
    None
}

#[cfg(target_os = "linux")]
async fn run_accounts(args: AccountsArgs) -> anyhow::Result<()> {
    use ikenga_desktop_lib::server::operator::cli::{
        self, AccountsCommand as Cmd, AccountsOptions, PasswordSource,
    };
    use ikenga_desktop_lib::server::operator::provision::{ProvisioningMode, UidRange};

    let data_dir = args.data_dir.ok_or_else(|| {
        anyhow::anyhow!("`accounts` needs --data-dir (or IKENGA_DATA_DIR): the T1 operator root")
    })?;
    let opts = AccountsOptions {
        data_dir,
        uid_range: args.uid_range.parse::<UidRange>()?,
        provisioning: match args.provisioning {
            Provisioning::Auto => ProvisioningMode::Auto,
            Provisioning::External => ProvisioningMode::External,
        },
    };
    let source = |p: PasswordArgs| {
        if p.password_stdin {
            PasswordSource::Stdin
        } else {
            PasswordSource::Tty
        }
    };
    let cmd = match args.command {
        AccountsCommand::Create {
            username,
            admin,
            adopt_unix_user,
            allow_system_user,
            password,
        } => Cmd::Create {
            username,
            admin,
            adopt_unix_user,
            allow_system_user,
            password: source(password),
        },
        AccountsCommand::Passwd { username, password } => Cmd::Passwd {
            username,
            password: source(password),
        },
        AccountsCommand::Disable { username } => Cmd::Disable { username },
        AccountsCommand::Enable { username } => Cmd::Enable { username },
        AccountsCommand::RevokeSessions { username } => Cmd::RevokeSessions { username },
        AccountsCommand::List { json } => Cmd::List { json },
    };
    cli::run(opts, cmd).await
}

#[cfg(not(target_os = "linux"))]
async fn run_accounts(_args: AccountsArgs) -> anyhow::Result<()> {
    anyhow::bail!("local accounts are part of executor tier t1, which is Linux-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executor_tier_defaults_to_t0() {
        let args = CliArgs::try_parse_from(["ikenga-server"]).unwrap();
        assert!(args.command.is_none());
        assert_eq!(args.serve.executor_tier, ExecutorTier::T0);
    }

    #[test]
    fn executor_tier_parses_every_tier_from_the_flag() {
        for tier in ExecutorTier::ALL {
            let args = CliArgs::try_parse_from(["ikenga-server", "--executor-tier", tier.as_str()])
                .unwrap();
            assert_eq!(args.serve.executor_tier, tier);
        }
    }

    #[test]
    fn unknown_executor_tier_is_a_startup_error() {
        let err = CliArgs::try_parse_from(["ikenga-server", "--executor-tier", "t9"])
            .expect_err("t9 names no tier");
        assert!(err.to_string().contains("t9"), "{err}");
    }

    #[test]
    fn accounts_subcommands_parse_with_global_flags_after_them() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "accounts",
            "create",
            "ada",
            "--admin",
            "--password-stdin",
            "--data-dir",
            "/opt/ikenga/data",
            "--uid-range",
            "30000-30999",
        ])
        .unwrap();
        let Some(Command::Accounts(a)) = args.command else {
            panic!("expected accounts");
        };
        assert_eq!(
            a.data_dir.as_deref(),
            Some(std::path::Path::new("/opt/ikenga/data"))
        );
        assert_eq!(a.uid_range, "30000-30999");
        assert_eq!(a.provisioning, Provisioning::Auto);
        match a.command {
            AccountsCommand::Create {
                username,
                admin,
                adopt_unix_user,
                allow_system_user,
                password,
            } => {
                assert_eq!(username, "ada");
                assert!(admin && password.password_stdin && adopt_unix_user.is_none());
                assert!(!allow_system_user);
            }
            other => panic!("{other:?}"),
        }

        for argv in [
            vec!["ikenga-server", "accounts", "passwd", "ada"],
            vec!["ikenga-server", "accounts", "disable", "ada"],
            vec!["ikenga-server", "accounts", "enable", "ada"],
            vec!["ikenga-server", "accounts", "revoke-sessions", "ada"],
            vec!["ikenga-server", "accounts", "list", "--json"],
            vec![
                "ikenga-server",
                "accounts",
                "--provisioning",
                "external",
                "list",
            ],
        ] {
            let args = CliArgs::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            assert!(
                matches!(args.command, Some(Command::Accounts(_))),
                "{argv:?}"
            );
        }
    }

    /// G-90: no flag ever carries a password.
    #[test]
    fn passwords_are_never_accepted_on_argv() {
        for argv in [
            vec![
                "ikenga-server",
                "accounts",
                "create",
                "ada",
                "--password",
                "x",
            ],
            vec!["ikenga-server", "accounts", "create", "ada", "hunter2"],
            vec!["ikenga-server", "accounts", "passwd", "ada", "hunter2"],
        ] {
            assert!(CliArgs::try_parse_from(&argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn allow_system_user_needs_an_adopted_user() {
        assert!(CliArgs::try_parse_from([
            "ikenga-server",
            "accounts",
            "create",
            "ada",
            "--allow-system-user",
        ])
        .is_err());
        assert!(CliArgs::try_parse_from([
            "ikenga-server",
            "accounts",
            "create",
            "ada",
            "--adopt-unix-user",
            "daemon",
            "--allow-system-user",
        ])
        .is_ok());
    }

    #[test]
    fn probe_and_the_hidden_entries_parse() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "probe",
            "--executor-tier",
            "t1",
            "--data-dir",
            "/opt/ikenga/data",
            "--json",
        ])
        .unwrap();
        let Some(Command::Probe(p)) = args.command else {
            panic!("expected probe");
        };
        assert_eq!(p.executor_tier, ExecutorTier::T1);
        assert!(p.json);
        assert_eq!(p.uid_range, "20000-29999");
        assert!(matches!(
            CliArgs::try_parse_from(["ikenga-server", "__t1-probe-child"])
                .unwrap()
                .command,
            Some(Command::T1ProbeChild)
        ));
        assert!(matches!(
            CliArgs::try_parse_from(["ikenga-server", "__t1-kill-all"])
                .unwrap()
                .command,
            Some(Command::T1KillAll)
        ));
        // Hidden from --help.
        let help = <CliArgs as clap::CommandFactory>::command()
            .render_long_help()
            .to_string();
        assert!(help.contains("probe"), "{help}");
        assert!(!help.contains("__t1"), "{help}");
    }

    #[test]
    fn t1_serve_flags_parse() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "--executor-tier",
            "t1",
            "--uid-range",
            "30000-30999",
            "--provisioning",
            "external",
            "--principal-path",
            "/usr/bin:/bin",
        ])
        .unwrap();
        assert!(args.command.is_none());
        assert_eq!(args.serve.uid_range.as_deref(), Some("30000-30999"));
        assert_eq!(args.serve.provisioning, Provisioning::External);
        assert_eq!(
            args.serve.principal_path.as_deref(),
            Some(std::ffi::OsStr::new("/usr/bin:/bin"))
        );
    }

    #[test]
    fn serve_flags_and_subcommands_do_not_mix() {
        let err = CliArgs::try_parse_from(["ikenga-server", "--port", "4001", "accounts", "list"])
            .expect_err("serve flags conflict with a subcommand");
        assert!(!err.to_string().is_empty());
    }
}
