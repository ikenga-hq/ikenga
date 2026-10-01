//! `SessionExecutor` — the one seam every child-process spawn is meant to
//! route through (ADR-023 §1, remote-access WP-18).
//!
//! The isolation mechanism behind a spawn is a *deployment* choice, not an
//! architectural one: the same call site builds the same [`SpawnSpec`] whether
//! the host runs everything in-process (T0), as a per-user Unix uid (T1), in a
//! container per session (T2) or under firejail (T3). Only the executor
//! changes. This slice implements **T0 only** — [`InProcessExecutor`], which is
//! byte-for-byte the spawn each call site used to perform inline.
//!
//! ## Tier selection and the boot probe
//!
//! The tier comes from config (`IKENGA_EXECUTOR_TIER` / `--executor-tier` on
//! `ikenga-server`, default `t0`). [`probe`] turns a requested tier into the
//! [`Capabilities`] this build can honour, or a typed [`Refusal`]. The daemon
//! refuses to start on a refusal rather than falling back to a weaker tier
//! (DEC-R9-1): an operator who asked for per-user isolation and silently got
//! a shared uid is worse off than one whose server would not boot.
//!
//! [`install`] probes and then publishes the executor process-wide; spawn
//! sites fetch it with [`current`]. When nothing was installed — the desktop
//! app, which is by definition a single trusted user on their own machine —
//! [`current`] is T0. That default is the desktop's tier, not a fallback: the
//! daemon always installs before it serves anything, and exits if it can't.
//!
//! ## What is in scope (slice 1)
//!
//! `pty::PtyManager::spawn_inner`, `claude::session::spawn_streaming`, the
//! codex and antigravity engines. Chi's detached `chi-runner` launch
//! (WP-18b) goes through [`PipedOpts::detached`]. WP-18b part b routed the
//! remaining desktop spawns: pkg MCP (per-call + long-lived), pkg sidecars
//! (one-shot, streaming, invoke, cron), the npm/bun and claude-store
//! installers ([`SessionExecutor::spawn_output_blocking`]), the playwright
//! proxy, `action_exec` ([`PipedOpts::new_process_group`]) and the agent
//! detection probes.
//!
//! This module compiles without the `desktop` feature: the daemon needs it.

mod in_process;
mod tier;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::OnceLock;

use portable_pty::{Child as PtyProcess, MasterPty, PtySize, SlavePty};
use serde::{Deserialize, Serialize};

pub use in_process::InProcessExecutor;
pub use tier::{probe, Capabilities, ExecutorTier, ParseTierError, Refusal};

/// A principal's stable id (G-PRINCIPAL §1): an opaque **UUIDv7**, minted once
/// when the account is created and never reused, even after the account is
/// disabled. Distinct from the username (mutable, human-facing) and from the
/// uid (a host attribute a restored host may renumber).
///
/// Wire form (`Display`, `FromStr`, serde) is the lowercase hyphenated
/// 36-char string. Parsing is strict: anything else — upper case, braces, the
/// simple/URN forms, or a UUID of any other version — is rejected, so one id
/// has exactly one spelling wherever it is used as a key (WP-21 secrets,
/// WP-22 OIDC links, G-ACCESS roles/devices/audit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrincipalId(uuid::Uuid);

impl PrincipalId {
    /// Mint a fresh id. Time-ordered (v7), so the `accounts` PK index stays
    /// append-mostly.
    pub fn new_v7() -> Self {
        PrincipalId(uuid::Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }
}

/// A string that is not a lowercase hyphenated UUIDv7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsePrincipalIdError(pub String);

impl fmt::Display for ParsePrincipalIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a principal id (expected a lowercase hyphenated UUIDv7)",
            self.0
        )
    }
}

impl std::error::Error for ParsePrincipalIdError {}

impl FromStr for PrincipalId {
    type Err = ParsePrincipalIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParsePrincipalIdError(s.to_string());
        // `Uuid::parse_str` also accepts upper case, braces, URN and simple
        // forms; the canonical spelling is the only one a key may have.
        if s.len() != 36 || s.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(err());
        }
        let uuid = uuid::Uuid::try_parse(s).map_err(|_| err())?;
        if uuid.get_version() != Some(uuid::Version::SortRand) {
            return Err(err());
        }
        Ok(PrincipalId(uuid))
    }
}

impl fmt::Display for PrincipalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Hyphenated`'s Display is lowercase.
        fmt::Display::fmt(&self.0.hyphenated(), f)
    }
}

impl Serialize for PrincipalId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PrincipalId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The identity a spawn runs *as* (G-PRINCIPAL §1). T0 ignores it (everything
/// runs as the host process's own user, and T0 never constructs one). T1 maps
/// it to its Unix uid; T2/T3 to a per-session sandbox.
///
/// A `Principal` is **fully resolved before any spawn** (§9.1): the caller
/// reads it from `operator/accounts.db`, and the executor never reads the
/// accounts DB, NSS or `/etc/passwd` on the spawn path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub id: PrincipalId,
    /// Login name (display); mutable; NOT used for any path or key.
    pub username: String,
    /// passwd name; immutable once provisioned (§7.2).
    pub unix_name: String,
    /// Never 0; inside the operator's uid range unless adopted (§11.2).
    pub uid: u32,
    /// User-private group; == uid for allocated accounts.
    pub gid: u32,
    /// Absolute; the passwd home (§4).
    pub home: PathBuf,
    /// passwd shell; `/bin/sh` when the operator sets none.
    pub shell: PathBuf,
}

/// How the child's environment is assembled. Applied in order: when `clear`
/// is set the inherited environment is dropped first, then every entry of
/// `vars` is set, later entries overriding earlier ones for the same key —
/// exactly the `env_clear()` + sequence-of-`env()` a call site would issue on
/// the underlying command builder.
///
/// Whatever filtering a call site does (the PTY's `is_host_only_env` denylist,
/// the augmented `PATH`) stays at the call site and is expressed here as the
/// resulting list. The executor applies it; it does not second-guess it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSpec {
    pub clear: bool,
    pub vars: Vec<(OsString, OsString)>,
}

/// What to run, where, with what environment, and (reserved) as whom.
///
/// The builder methods mirror `std::process::Command` / `portable_pty`'s
/// `CommandBuilder` so a call site reads the same after the move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: EnvSpec,
    /// `None` inherits the host process's working directory.
    pub cwd: Option<PathBuf>,
    /// Reserved for T1+; ignored by T0.
    pub principal: Option<Principal>,
}

impl SpawnSpec {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: EnvSpec::default(),
            cwd: None,
            principal: None,
        }
    }

    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }

    /// Drop the inherited environment before `vars` are applied.
    pub fn env_clear(&mut self) -> &mut Self {
        self.env.clear = true;
        self
    }

    pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.env
            .vars
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars {
            self.env(k, v);
        }
        self
    }

    pub fn current_dir(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn principal(&mut self, principal: Option<Principal>) -> &mut Self {
        self.principal = principal;
        self
    }
}

/// One stdio stream of a piped child. `Stdio` itself is neither `Clone` nor
/// inspectable, so the spec carries this and the executor materialises it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    Piped,
    Null,
    Inherit,
}

impl StdioMode {
    pub(crate) fn to_stdio(self) -> Stdio {
        match self {
            StdioMode::Piped => Stdio::piped(),
            StdioMode::Null => Stdio::null(),
            StdioMode::Inherit => Stdio::inherit(),
        }
    }
}

/// Per-spawn knobs for a piped (non-PTY) child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipedOpts {
    pub stdin: StdioMode,
    pub stdout: StdioMode,
    pub stderr: StdioMode,
    /// `tokio::process::Command::kill_on_drop`.
    pub kill_on_drop: bool,
    /// `platform::NoConsoleWindow` — suppress the Windows console flash. A
    /// no-op off Windows.
    pub no_console_window: bool,
    /// Start the child detached from the host so it outlives the app (a chi
    /// run's `chi-runner`, ADR-023 D4/D5). Unix: the child leads its own
    /// process group (`process_group(0)`), so a signal to the app's group
    /// doesn't reach it and the whole tree can be signalled as `-pid`.
    /// Windows: `CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`, plus
    /// `CREATE_BREAKAWAY_FROM_JOB` when the parent's job allows it. Forces
    /// `kill_on_drop` off — dropping the handle must not end the run.
    ///
    /// A process group does NOT escape a systemd cgroup: under a unit with
    /// the default `KillMode=control-group`, stopping the unit still kills a
    /// detached child (see `scripts/server/ikenga-server.service`).
    pub detached: bool,
    /// Unix: the child leads its own process group (`process_group(0)`), so
    /// the caller can signal the whole tree as `-pid` (`action_exec`'s
    /// timeout `kill_tree`). Unlike [`detached`](Self::detached) it does NOT
    /// force `kill_on_drop` off — the handle keeps whatever was asked for.
    /// A no-op off Unix.
    pub new_process_group: bool,
}

/// A spawned PTY child: what `PtyManager::spawn_inner` used to get from
/// `openpty` + `slave.spawn_command`.
///
/// `slave` is kept (not dropped inside the executor) so its lifetime at the
/// call site is exactly what it was before the move: dropped when the caller's
/// binding goes out of scope, after the master and child have been taken.
pub struct PtyChild {
    pub master: Box<dyn MasterPty + Send>,
    pub slave: Box<dyn SlavePty + Send>,
    pub child: Box<dyn PtyProcess + Send + Sync>,
}

/// Spawns child processes for the tier it implements.
///
/// Both methods are synchronous — the underlying spawns are — and object-safe
/// so the installed executor can be a `dyn` chosen at boot.
pub trait SessionExecutor: Send + Sync {
    /// What this executor honours. Reported on `/api/health`.
    fn capabilities(&self) -> Capabilities;

    fn tier(&self) -> ExecutorTier {
        self.capabilities().tier
    }

    /// Open a PTY of `size` and spawn `spec` on its slave side. Errors carry
    /// the same `openpty` / `spawn child` context the inline spawn did.
    fn spawn_pty(&self, spec: SpawnSpec, size: PtySize) -> anyhow::Result<PtyChild>;

    /// Spawn `spec` with the stdio wiring in `opts`. Returns the
    /// `tokio::process::Child` the engine call sites already hold.
    fn spawn_piped(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<tokio::process::Child>;

    /// Run `spec` to completion and collect its output, blocking the calling
    /// thread — `std::process::Command::output` behind the seam, for call
    /// chains with no async context (`pkg::npm_install`, the claude-store
    /// installer). `opts`' stdio modes are applied as given (unset streams
    /// would otherwise follow `output()`'s defaults, so call sites name all
    /// three). `kill_on_drop` has no meaning here and is ignored; `detached`
    /// is refused as `InvalidInput` — waiting on a child that must outlive
    /// the caller is a contradiction.
    fn spawn_output_blocking(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<std::process::Output>;
}

static INSTALLED: OnceLock<Box<dyn SessionExecutor>> = OnceLock::new();
static DESKTOP_DEFAULT: InProcessExecutor = InProcessExecutor;

/// The process-wide executor. T0 when nothing has been [`install`]ed — see the
/// module docs for why that is the desktop's tier rather than a fallback.
pub fn current() -> &'static dyn SessionExecutor {
    match INSTALLED.get() {
        Some(executor) => executor.as_ref(),
        None => &DESKTOP_DEFAULT,
    }
}

/// Probe `tier` and, if this build can honour it, publish its executor as
/// [`current`]. Idempotent for the same tier; installing a *different* tier
/// after one is live is refused rather than silently ignored.
pub fn install(tier: ExecutorTier) -> Result<Capabilities, Refusal> {
    let capabilities = probe(tier)?;
    let executor: Box<dyn SessionExecutor> = match tier {
        ExecutorTier::T0 => Box::new(InProcessExecutor),
        // `probe` refuses every tier this build does not implement, so this
        // arm is only reachable if the two drift apart — refuse, don't guess.
        other => return Err(Refusal::NotImplemented { tier: other }),
    };
    let installed = INSTALLED.get_or_init(|| executor);
    if installed.tier() != tier {
        return Err(Refusal::AlreadyInstalled {
            installed: installed.tier(),
            requested: tier,
        });
    }
    Ok(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_builder_records_calls_in_order() {
        let mut spec = SpawnSpec::new("prog");
        spec.arg("a")
            .args(["b", "c"])
            .env_clear()
            .env("K", "1")
            .envs([("K", "2"), ("J", "3")])
            .current_dir("/tmp");
        assert_eq!(spec.program, OsString::from("prog"));
        assert_eq!(spec.args, vec!["a", "b", "c"]);
        assert!(spec.env.clear);
        assert_eq!(
            spec.env.vars,
            vec![
                ("K".into(), "1".into()),
                ("K".into(), "2".into()),
                ("J".into(), "3".into()),
            ]
        );
        assert_eq!(spec.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(spec.principal, None);
    }

    #[test]
    fn principal_id_round_trips_its_canonical_spelling() {
        let id = PrincipalId::new_v7();
        let s = id.to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(s, s.to_lowercase());
        assert_eq!(s.parse::<PrincipalId>(), Ok(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{s}\""));
        assert_eq!(serde_json::from_str::<PrincipalId>(&json).unwrap(), id);
    }

    #[test]
    fn principal_id_parse_rejects_every_other_spelling_and_version() {
        let id = PrincipalId::new_v7().to_string();
        let v4 = uuid::Uuid::new_v4().hyphenated().to_string();
        for bad in [
            id.to_uppercase(),
            format!("{{{id}}}"),
            format!("urn:uuid:{id}"),
            id.replace('-', ""),
            v4,
            String::new(),
            "not-a-uuid".into(),
        ] {
            assert!(bad.parse::<PrincipalId>().is_err(), "{bad} must not parse");
        }
        assert!(serde_json::from_str::<PrincipalId>("\"nope\"").is_err());
    }

    #[test]
    fn principal_ids_are_time_ordered() {
        let a = PrincipalId::new_v7();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = PrincipalId::new_v7();
        assert!(a < b);
        assert!(a.to_string() < b.to_string());
    }

    #[test]
    fn current_is_t0_and_installing_t0_is_idempotent() {
        assert_eq!(current().tier(), ExecutorTier::T0);
        assert_eq!(install(ExecutorTier::T0).unwrap().tier, ExecutorTier::T0);
        assert_eq!(install(ExecutorTier::T0).unwrap().tier, ExecutorTier::T0);
        assert_eq!(current().tier(), ExecutorTier::T0);
    }

    #[test]
    fn install_refuses_unimplemented_tiers_without_touching_current() {
        for tier in [ExecutorTier::T1, ExecutorTier::T2, ExecutorTier::T3] {
            assert_eq!(install(tier), Err(Refusal::NotImplemented { tier }));
        }
        assert_eq!(current().tier(), ExecutorTier::T0);
    }
}
