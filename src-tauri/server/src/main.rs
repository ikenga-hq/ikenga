use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use ikenga_desktop_lib::executor::ExecutorTier;
use ikenga_desktop_lib::server::{
    run_server_with, BootstrapCredentials, ServerConfig, T1ServeOptions,
};
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
    /// The access audit chain (G-ACCESS §6.4, §6.8): verify, export or
    /// reseal it. Operator-wide: on the T1 operator store (`--data-dir`) it
    /// runs as root; `--file <db>` names any access store you can open (a
    /// T0 `<data-dir>/access.db`, or a copy).
    Audit(AuditArgs),
    /// Check whether this host can run an executor tier (G-PRINCIPAL §8).
    /// For t1: identity, capabilities, the operator root and a real test
    /// drop to the reserved probe uid — read-only (no reconcile, no
    /// probe.json). Exits 0 when the tier can run, 1 when it can't.
    Probe(ProbeArgs),
    /// Manage server secrets and rotate the key-encryption key (root only).
    Secrets(SecretsArgs),
    /// Run the server under a minimal init, for hosts without systemd (a
    /// container). The server becomes this process's child and is restarted
    /// whenever it exits, so detached agent runs survive a server restart;
    /// orphans are reaped. SIGHUP restarts the server, SIGTERM stops both.
    /// Pass the server's own flags after `--`, e.g.
    /// `ikenga-server supervise -- --host 0.0.0.0 --data-dir /data`.
    /// Linux-only.
    Supervise(SuperviseArgs),
    /// Internal: the §8 test-drop child (spawned by the probe as the probe uid).
    #[command(name = "__t1-probe-child", hide = true)]
    T1ProbeChild,
    /// Internal: the §7.3 uid-wide kill (spawned as the principal's uid).
    #[command(name = "__t1-kill-all", hide = true)]
    T1KillAll,
}

#[derive(Args, Debug)]
pub struct SuperviseArgs {
    /// The flags to run the server with, exactly as `ikenga-server` alone
    /// would take them. Checked before anything starts.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "SERVER_FLAGS"
    )]
    pub server: Vec<OsString>,
}

#[derive(Args, Debug)]
pub struct AuditArgs {
    /// The T1 operator root (the server's `--data-dir` under t1); the
    /// store is its `operator/accounts.db`. Needs root.
    #[arg(long, env = "IKENGA_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// An access store file instead of the T1 operator store.
    #[arg(long, value_name = "DB", global = true)]
    pub file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: AuditCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuditCommand {
    /// Walk the chain from genesis, recomputing every hash (read-only).
    /// Exits 0 when it holds, 1 when a break no reseal acknowledges remains.
    Verify {
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Export every row (or a filtered set) as JSONL plus a manifest line
    /// (`ikenga-audit-export-v1`). Verifies first; appends `audit.exported`.
    Export {
        /// Write here (owner-only) instead of stdout.
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Rows where this principal is the actor or the subject.
        #[arg(long)]
        who: Option<String>,
        /// Rows of this device (actor's or subject).
        #[arg(long)]
        device: Option<String>,
        /// permission | dispatch | access | pairing | people
        #[arg(long)]
        category: Option<String>,
        /// Free-text search.
        #[arg(long)]
        q: Option<String>,
        /// `<owner_principal_id>/<project_id>`.
        #[arg(long)]
        project_key: Option<String>,
    },
    /// Acknowledge the outstanding break (`--ack` must name it) and clear
    /// `degraded`: appends `audit.resealed`. The break stays in the chain.
    Reseal {
        /// The seq the chain is broken at (from `audit verify`).
        #[arg(long, value_name = "SEQ")]
        ack: i64,
    },
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
pub struct SecretsArgs {
    /// The multi-user server data root (the server's `--data-dir`). Needs root.
    #[arg(long, env = "IKENGA_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: SecretsCommand,
}

#[derive(Subcommand, Debug)]
pub enum SecretsCommand {
    /// Rotate the master key-encryption key (KEK) protecting user secrets.
    ///
    /// Generates a new server KEK and re-wraps every user's secret store
    /// envelope under it. Requires root. Refuses to run if the server or any
    /// session is running.
    RotateKek,
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
    /// Migrate a stopped T0 install into a principal (G-PRINCIPAL §11.2).
    /// For a T0 that ran as a non-root user, first `create <username>
    /// --adopt-unix-user <that user>`: its data dir then moves in and its
    /// home stays. Otherwise (root/Docker) the account is created here as a
    /// fresh principal, and the data dir plus the old home's engine and app
    /// dot-dirs are copied in. The old dir is kept, read-only, as
    /// `<old>.t0-migrated-<ts>`, together with its access store. For an
    /// adopted user, every process of its uid is killed first: run this
    /// from a root session that is not a login of that user.
    #[command(name = "adopt-t0")]
    AdoptT0 {
        username: String,
        /// The T0 daemon's --data-dir. Must be outside the operator root.
        #[arg(long, value_name = "OLD_DATA_DIR")]
        from: PathBuf,
        /// The home the T0 daemon ran with (`/root` for Docker).
        #[arg(long, value_name = "OLD_HOME")]
        home: PathBuf,
        /// When adopt-t0 creates the account: make it an admin. Accepted
        /// on a re-run when the account already exists and is an admin.
        #[arg(long)]
        admin: bool,
        #[command(flatten)]
        password: PasswordArgs,
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

    /// Trusted reverse proxy IP addresses and CIDR subnets (comma-separated, e.g.
    /// `127.0.0.1,::1,10.0.0.0/8`). When set, client addresses behind these proxies
    /// are resolved from the one header named by `--trusted-proxy-header`.
    /// Unset by default (forwarded headers ignored).
    #[arg(long, env = "IKENGA_TRUSTED_PROXIES")]
    pub trusted_proxies: Option<String>,

    /// The single forwarding header the trusted proxy writes: `x-forwarded-for`
    /// (default; Caddy, nginx) or `forwarded` (RFC 7239). The other header is
    /// never read, because proxies pass a client-supplied copy through.
    #[arg(long, env = "IKENGA_TRUSTED_PROXY_HEADER")]
    pub trusted_proxy_header: Option<String>,

    /// Idle timeout in seconds before server automatically shuts down when no
    /// PTY session, open WebSocket or recent request keeps it active. Under
    /// `t1` this is each principal child's idle timeout instead (default 1800;
    /// the broker itself never idles out).
    #[arg(long, env = "IKENGA_IDLE_TIMEOUT")]
    pub idle_timeout: Option<u64>,

    /// Session-executor isolation tier (ADR-023): `t0` in-process, `t1`
    /// per-user uid, `t2` container per session, `t3` firejail per session.
    /// `t1` runs its boot probe (G-PRINCIPAL §8) and serves the multi-user
    /// broker: principals sign in at /auth/login and each gets their own
    /// child process as their own uid. `t2`/`t3` are refused. A refused tier
    /// never falls back to a weaker one.
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

    /// Drop `Secure` from the session cookie (G-PRINCIPAL P-3). Only for a
    /// plain-HTTP deploy on a private network (a tailnet IP); behind HTTPS,
    /// leave it off.
    #[arg(long, env = "IKENGA_INSECURE_COOKIE")]
    pub insecure_cookie: bool,

    /// The public base URL of this server, e.g. `https://ik.example.ts.net`:
    /// the base of device-pairing QR links and invite links (G-ACCESS §3.3).
    /// Without it the pairing sheet falls back to the address a browser
    /// reached the server on.
    #[arg(long, env = "IKENGA_PUBLIC_URL")]
    pub public_url: Option<String>,

    /// T1 only: the most accounts this server may hold; every creation path
    /// (CLI, bootstrap, invites) refuses past it (G-ACCESS P-27). Default:
    /// unlimited.
    #[arg(long, env = "IKENGA_MAX_ACCOUNTS")]
    pub max_accounts: Option<u32>,

    /// T1 only: how many days an invite link stays valid (G-ACCESS P-15).
    /// Default 7, at most 30.
    #[arg(long, env = "IKENGA_INVITE_TTL", value_parser = clap::value_parser!(u32).range(1..=30))]
    pub invite_ttl: Option<u32>,

    /// T1 only: let any project Owner or Operator issue invites that create
    /// a new account. Off by default: only an `is_admin` issuer's invites may
    /// (G-ACCESS §4.4, N-11).
    #[arg(long, env = "IKENGA_MEMBER_INVITES_CREATE_ACCOUNTS")]
    pub member_invites_create_accounts: bool,

    /// Internal: run as a T1 principal child (launched by the broker as the
    /// principal's uid, G-PRINCIPAL §3).
    #[arg(long, hide = true, requires = "expected_uid")]
    pub principal_child: bool,

    /// Internal, with `--principal-child`: the uid the child must be running as.
    #[arg(long, hide = true, requires = "principal_child")]
    pub expected_uid: Option<u32>,
}

fn main() -> anyhow::Result<()> {
    let cli = CliArgs::parse();
    // `supervise` runs before any async runtime exists: it blocks its
    // signals and takes them synchronously, which needs a single-threaded
    // process (`server::supervisor`). It also keeps the environment intact —
    // every server it starts needs `IKENGA_AUTH_TOKEN` and the rest, which
    // each one strips from itself.
    if let Some(Command::Supervise(args)) = &cli.command {
        std::process::exit(supervise(args));
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: CliArgs) -> anyhow::Result<()> {
    let CliArgs {
        command,
        serve: args,
    } = cli;

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
    // This runs BEFORE `run_server`, so nothing downstream can read these
    // from the environment. `IKENGA_AUTH_TOKEN` is safe because clap has
    // already put it in `config`. `IKENGA_VAULT_KEY` is the desktop vault's
    // unlock key and has no reader in the daemon; it is stripped only so a
    // stray one can't leak into a child.
    //
    // The headless per-principal secret store does NOT use it (WP-21). Its
    // key is `IKENGA_PRINCIPAL_SECRETS_KEY` (`secrets::principal_store::
    // WRAP_KEY_ENV`), which the broker sets on each T1 principal child it
    // launches. It is deliberately NOT in this list: the child takes it while
    // building `AppState` (`DaemonSecrets::for_daemon` → `WrapKey::take_from_env`),
    // which reads it once, zeroes the value in the environment block and
    // unsets it before anything is spawned — in every tier, so it never
    // outlives startup. `pty::is_host_only_env` filters it from PTYs as well.
    // Adding it here would make that read always fail.
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
        Some(Command::Audit(audit)) => std::process::exit(run_audit(audit).await),
        Some(Command::Secrets(secrets)) => return run_secrets(secrets).await,
        Some(Command::T1ProbeChild | Command::T1KillAll | Command::Supervise(_)) => {
            unreachable!("handled above")
        }
        None => {}
    }

    // Parsed once, here, before anything is served; handed to the helper
    // directly so the environment is never mutated on a running runtime.
    {
        use ikenga_desktop_lib::server::trusted_proxy::{install, TrustedProxies};
        let tp = TrustedProxies::from_settings(
            args.trusted_proxies.as_deref(),
            args.trusted_proxy_header.as_deref(),
        );
        if tp.is_empty() {
            if args.trusted_proxy_header.is_some() {
                tracing::warn!(
                    "IKENGA_TRUSTED_PROXY_HEADER ignored: no IKENGA_TRUSTED_PROXIES configured"
                );
            }
        } else {
            tracing::info!(
                networks = tp.len(),
                header = ?tp.header(),
                "trusted proxies configured"
            );
        }
        if !install(tp) {
            tracing::warn!("trusted-proxy configuration was already initialised; ignoring");
        }
    }
    // Honoured by the T1 broker after its §8 boot probe, only on an empty
    // accounts table (`Provisioner::bootstrap_admin`); never by a child.
    let bootstrap_admin = match bootstrap {
        Some(b) if args.executor_tier == ExecutorTier::T1 && !args.principal_child => {
            tracing::info!("IKENGA_BOOTSTRAP_ADMIN captured for the T1 broker");
            Some(bootstrap_credentials(b))
        }
        Some(_) => {
            tracing::warn!(
                "IKENGA_BOOTSTRAP_ADMIN ignored: local accounts exist only under --executor-tier t1"
            );
            None
        }
        None => None,
    };

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
        insecure_cookie: args.insecure_cookie,
        principal_child: args.principal_child,
        expected_uid: args.expected_uid,
        bootstrap_admin,
        public_url: args.public_url,
        max_accounts: args.max_accounts,
        invite_ttl_days: args.invite_ttl,
        member_invites_create_accounts: args.member_invites_create_accounts,
    };

    run_server_with(config, t1).await
}

/// `ikenga-server supervise -- <server flags>` → exit code.
fn supervise(args: &SuperviseArgs) -> i32 {
    if let Err(e) = check_supervised_flags(&args.server) {
        eprintln!("supervise: {e}");
        return 2;
    }
    // Logs go to stdout beside the server's own, as `serve` does.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,ikenga_server=debug".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
    // Re-exec by the path this process was started with, not
    // /proc/self/exe: after an upgrade replaces the binary on disk, the next
    // restart (SIGHUP) then runs the new one.
    let program = std::env::args_os()
        .next()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("ikenga-server"));
    let opts =
        ikenga_desktop_lib::server::supervisor::SuperviseOptions::new(program, args.server.clone());
    ikenga_desktop_lib::server::supervisor::run(&opts)
}

/// The supervised flags must start a server: they parse as `ikenga-server`'s
/// own (environment included), name no subcommand and no internal mode. A
/// typo fails here once instead of in a restart loop.
fn check_supervised_flags(server: &[OsString]) -> Result<(), String> {
    let cli = CliArgs::try_parse_from(
        std::iter::once(OsString::from("ikenga-server")).chain(server.iter().cloned()),
    )
    .map_err(|e| e.to_string())?;
    if cli.command.is_some() {
        return Err("takes the server's own flags, not a subcommand".into());
    }
    if cli.serve.principal_child {
        return Err("--principal-child is internal to the multi-user server".into());
    }
    Ok(())
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

/// `ikenga-server audit …` → exit code (0 = done / the chain holds).
async fn run_audit(args: AuditArgs) -> i32 {
    match audit(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("audit: {e:#}");
            2
        }
    }
}

async fn audit(args: AuditArgs) -> anyhow::Result<i32> {
    use ikenga_desktop_lib::access::audit::verify_boot;
    use ikenga_desktop_lib::access::AccessStore;

    let path = verify_boot::cli_store_path(args.data_dir, args.file)?;
    let read_only = matches!(args.command, AuditCommand::Verify { .. });
    if !read_only {
        if let Some(w) = verify_boot::cli_foreign_owner_warning(&path) {
            eprintln!("{w}");
        }
    }
    // `verify` is read-only (review m-8); every path closes the pool
    // before returning, errors included, so no connection outlives it.
    let store = AccessStore::open_cli(&path, read_only).await?;
    let result = audit_command(&store, args.command).await;
    store.pool().close().await;
    result
}

async fn audit_command(
    store: &ikenga_desktop_lib::access::AccessStore,
    command: AuditCommand,
) -> anyhow::Result<i32> {
    use ikenga_desktop_lib::access::audit::{export, list::Filter, reseal, verify_boot};
    use std::io::Write;

    Ok(match command {
        AuditCommand::Verify { json } => {
            let (text, ok) = verify_boot::cli_verify(store, json).await?;
            println!("{}", text.trim_end());
            if ok {
                0
            } else {
                1
            }
        }
        AuditCommand::Export {
            out,
            who,
            device,
            category,
            q,
            project_key,
        } => {
            let filter = Filter::from_parts(who, device, category, q, project_key)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let built = export::export_cli(store, &filter, out.as_deref()).await?;
            match &out {
                Some(p) => eprintln!(
                    "exported {} rows to {} (verified: {})",
                    built.rows,
                    p.display(),
                    built.manifest["verified"]
                ),
                None => {
                    let mut stdout = std::io::stdout();
                    stdout.write_all(built.jsonl.as_bytes())?;
                    stdout.flush()?;
                }
            }
            0
        }
        AuditCommand::Reseal { ack } => {
            let r = reseal::reseal(
                store,
                ack,
                ikenga_desktop_lib::access::audit::Event::new(
                    "audit.resealed",
                    ikenga_desktop_lib::access::audit::AuditVia::Cli,
                ),
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!(
                "resealed the break at #{} with #{} (it stays in the chain)",
                r.broken_at_seq, r.head.seq
            );
            match r.still_broken {
                None => {
                    println!("OK: access changes resume");
                    0
                }
                Some(b) => {
                    println!(
                        "still BROKEN at #{}: {} — reseal that one too after reviewing it",
                        b.broken_at_seq, b.reason
                    );
                    1
                }
            }
        }
    })
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

#[cfg(target_os = "linux")]
fn bootstrap_credentials(b: BootstrapAdmin) -> BootstrapCredentials {
    BootstrapCredentials {
        username: b.username,
        password: b.password,
    }
}

#[cfg(not(target_os = "linux"))]
fn bootstrap_credentials(_b: BootstrapAdmin) -> BootstrapCredentials {
    unreachable!("no bootstrap is captured off Linux")
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
        AccountsCommand::AdoptT0 {
            username,
            from,
            home,
            admin,
            password,
        } => Cmd::AdoptT0 {
            username,
            from,
            home,
            admin,
            password: source(password),
        },
    };
    cli::run(opts, cmd).await
}

#[cfg(target_os = "linux")]
async fn run_secrets(args: SecretsArgs) -> anyhow::Result<()> {
    use ikenga_desktop_lib::server::operator::rotate_kek::{execute_or_resume, CrashSimulation};
    use ikenga_desktop_lib::server::operator::secrets_kek::KekOwner;
    use ikenga_desktop_lib::server::operator::OperatorRoot;

    let data_dir = args.data_dir.ok_or_else(|| {
        anyhow::anyhow!("`secrets` needs --data-dir (or IKENGA_DATA_DIR): the server data root")
    })?;
    let root = OperatorRoot::new(data_dir)?;
    match args.command {
        SecretsCommand::RotateKek => {
            let summary =
                execute_or_resume(&root, KekOwner::Root, "cli", CrashSimulation::None).await?;
            if summary.was_resumed {
                println!(
                    "Resumed and completed secrets KEK rotation: {} stores re-wrapped.",
                    summary.stores_rotated
                );
            } else {
                println!(
                    "Rotated secrets KEK: {} stores re-wrapped.",
                    summary.stores_rotated
                );
            }
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
async fn run_accounts(_args: AccountsArgs) -> anyhow::Result<()> {
    anyhow::bail!("local accounts are part of executor tier t1, which is Linux-only")
}

#[cfg(not(target_os = "linux"))]
async fn run_secrets(_args: SecretsArgs) -> anyhow::Result<()> {
    anyhow::bail!("secrets rotation is part of multi-user server, which is Linux-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_rotate_kek_parses() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "secrets",
            "--data-dir",
            "/srv/ikenga/data",
            "rotate-kek",
        ])
        .unwrap();
        let Some(Command::Secrets(s)) = args.command else {
            panic!("not secrets");
        };
        assert_eq!(
            s.data_dir.as_deref(),
            Some(std::path::Path::new("/srv/ikenga/data"))
        );
        assert!(matches!(s.command, SecretsCommand::RotateKek));
    }

    #[test]
    fn audit_subcommands_parse() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "audit",
            "--file",
            "/tmp/access.db",
            "verify",
            "--json",
        ])
        .unwrap();
        let Some(Command::Audit(a)) = args.command else {
            panic!("not audit")
        };
        assert_eq!(
            a.file.as_deref(),
            Some(std::path::Path::new("/tmp/access.db"))
        );
        assert!(matches!(a.command, AuditCommand::Verify { json: true }));

        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "audit",
            "export",
            "--category",
            "pairing",
            "--out",
            "/tmp/x.jsonl",
        ])
        .unwrap();
        let Some(Command::Audit(a)) = args.command else {
            panic!("not audit")
        };
        assert!(matches!(
            a.command,
            AuditCommand::Export { ref category, .. } if category.as_deref() == Some("pairing")
        ));

        let args =
            CliArgs::try_parse_from(["ikenga-server", "audit", "reseal", "--ack", "42"]).unwrap();
        let Some(Command::Audit(a)) = args.command else {
            panic!("not audit")
        };
        assert!(matches!(a.command, AuditCommand::Reseal { ack: 42 }));
        // `--ack` is required: no reseal of an unnamed break.
        assert!(CliArgs::try_parse_from(["ikenga-server", "audit", "reseal"]).is_err());
    }

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
                "adopt-t0",
                "--from",
                "/opt/ikenga/data-t0",
                "--home",
                "/root",
                "ada",
                "--admin",
                "--password-stdin",
            ],
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
    fn adopt_t0_needs_from_and_home() {
        for argv in [
            vec!["ikenga-server", "accounts", "adopt-t0", "ada"],
            vec![
                "ikenga-server",
                "accounts",
                "adopt-t0",
                "--from",
                "/x",
                "ada",
            ],
            vec![
                "ikenga-server",
                "accounts",
                "adopt-t0",
                "--home",
                "/root",
                "ada",
            ],
            vec![
                "ikenga-server",
                "accounts",
                "adopt-t0",
                "--from",
                "/x",
                "--home",
                "/root",
            ],
        ] {
            assert!(CliArgs::try_parse_from(&argv).is_err(), "{argv:?}");
        }
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "accounts",
            "adopt-t0",
            "--from",
            "/opt/ikenga/data-t0",
            "--home",
            "/home/ikenga",
            "ada",
        ])
        .unwrap();
        let Some(Command::Accounts(a)) = args.command else {
            panic!("expected accounts");
        };
        match a.command {
            AccountsCommand::AdoptT0 {
                username,
                from,
                home,
                admin,
                password,
            } => {
                assert_eq!(username, "ada");
                assert_eq!(from, PathBuf::from("/opt/ikenga/data-t0"));
                assert_eq!(home, PathBuf::from("/home/ikenga"));
                assert!(!admin && !password.password_stdin);
            }
            other => panic!("{other:?}"),
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
    fn insecure_cookie_and_the_hidden_child_flags_parse() {
        let args = CliArgs::try_parse_from(["ikenga-server", "--insecure-cookie"]).unwrap();
        assert!(args.serve.insecure_cookie && !args.serve.principal_child);
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "--executor-tier",
            "t1",
            "--principal-child",
            "--expected-uid",
            "20001",
        ])
        .unwrap();
        assert!(args.serve.principal_child);
        assert_eq!(args.serve.expected_uid, Some(20_001));
        // Each needs the other.
        assert!(CliArgs::try_parse_from(["ikenga-server", "--principal-child"]).is_err());
        assert!(CliArgs::try_parse_from(["ikenga-server", "--expected-uid", "1"]).is_err());
        let help = <CliArgs as clap::CommandFactory>::command()
            .render_long_help()
            .to_string();
        assert!(help.contains("--insecure-cookie"), "{help}");
        assert!(!help.contains("--principal-child"), "{help}");
    }

    /// G-ACCESS §10.1: every Part B flag parses and defaults off / unlimited.
    #[test]
    fn part_b_access_flags_parse_with_safe_defaults() {
        let args = CliArgs::try_parse_from(["ikenga-server"]).unwrap();
        assert_eq!(args.serve.public_url, None);
        assert_eq!(args.serve.max_accounts, None);
        assert_eq!(args.serve.invite_ttl, None);
        assert!(!args.serve.member_invites_create_accounts);
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "--public-url",
            "https://ik.example.ts.net",
            "--max-accounts",
            "25",
            "--invite-ttl",
            "30",
            "--member-invites-create-accounts",
        ])
        .unwrap();
        assert_eq!(
            args.serve.public_url.as_deref(),
            Some("https://ik.example.ts.net")
        );
        assert_eq!(args.serve.max_accounts, Some(25));
        assert_eq!(args.serve.invite_ttl, Some(30));
        assert!(args.serve.member_invites_create_accounts);
        // P-15: the invite TTL tops out at 30 days.
        assert!(CliArgs::try_parse_from(["ikenga-server", "--invite-ttl", "31"]).is_err());
        assert!(CliArgs::try_parse_from(["ikenga-server", "--invite-ttl", "0"]).is_err());
    }

    #[test]
    fn supervise_takes_the_server_flags_after_a_double_dash() {
        let args = CliArgs::try_parse_from([
            "ikenga-server",
            "supervise",
            "--",
            "--host",
            "0.0.0.0",
            "--port",
            "4000",
            "--data-dir",
            "/opt/ikenga/data",
        ])
        .unwrap();
        let Some(Command::Supervise(s)) = args.command else {
            panic!("not supervise")
        };
        let flags: Vec<_> = s.server.iter().map(|f| f.to_str().unwrap()).collect();
        assert_eq!(
            flags,
            [
                "--host",
                "0.0.0.0",
                "--port",
                "4000",
                "--data-dir",
                "/opt/ikenga/data"
            ]
        );
        check_supervised_flags(&s.server).expect("valid server flags");

        // No flags at all is a plain default server.
        let args = CliArgs::try_parse_from(["ikenga-server", "supervise"]).unwrap();
        let Some(Command::Supervise(s)) = args.command else {
            panic!("not supervise")
        };
        assert!(s.server.is_empty());
        check_supervised_flags(&s.server).expect("defaults");
    }

    #[test]
    fn supervise_refuses_flags_that_would_not_start_a_server() {
        let flags = |f: &[&str]| f.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(check_supervised_flags(&flags(&["--no-such-flag"])).is_err());
        assert!(check_supervised_flags(&flags(&["--port", "not-a-port"])).is_err());
        let sub = check_supervised_flags(&flags(&["accounts", "list"])).unwrap_err();
        assert!(sub.contains("subcommand"), "{sub}");
        let nested = check_supervised_flags(&flags(&["supervise"])).unwrap_err();
        assert!(nested.contains("subcommand"), "{nested}");
        let child = check_supervised_flags(&flags(&[
            "--executor-tier",
            "t1",
            "--principal-child",
            "--expected-uid",
            "20001",
        ]))
        .unwrap_err();
        assert!(child.contains("internal"), "{child}");
    }

    #[test]
    fn serve_flags_and_subcommands_do_not_mix() {
        let err = CliArgs::try_parse_from(["ikenga-server", "--port", "4001", "accounts", "list"])
            .expect_err("serve flags conflict with a subcommand");
        assert!(!err.to_string().is_empty());
    }
}
