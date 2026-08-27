use std::path::PathBuf;

use clap::Parser;
use ikenga_desktop_lib::server::{run_server, ServerConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "ikenga-server")]
#[command(author = "Ikenga")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Ikenga Headless Server Daemon", long_about = None)]
pub struct CliArgs {
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,ikenga_server=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = CliArgs::parse();

    let config = ServerConfig {
        host: args.host,
        port: args.port,
        static_dir: args.static_dir,
        pkgs_dir: args.pkgs_dir,
        data_dir: args.data_dir,
        auth_token: args.auth_token,
        allowed_origins: args.allowed_origins,
    };

    // Clap has read these into `config`; drop them from the process
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

    run_server(config).await
}
