//! The provisioning core (G-PRINCIPAL §7.2–§7.4): create, adopt, disable,
//! enable, passwd, forced logout and the first-admin bootstrap.
//!
//! ## Create (§7.2, G-ACCESS R-8)
//!
//! [`Provisioner::create_in`] runs inside a **caller-owned** `BEGIN IMMEDIATE`
//! transaction (Round 16's §7.2 clarification), so the CLI, the env bootstrap
//! and — later — the broker's invite acceptance (WP-76) share one core:
//!
//! 1. *(caller)* `BEGIN IMMEDIATE`.
//! 2. Allocate `unix_uid = max(range_start, MAX(unix_uid over non-adopted
//!    rows) + 1)`. Disabled rows count (uids are never reused, I-4). Fail if
//!    the result is `>= range_end` — `range_end` is the §8 probe uid and is
//!    never allocated — or if the host already holds the uid or gid.
//! 3. Mint the UUIDv7, derive `unix_name = "ik-" + lowercase(username)`,
//!    insert the row and write `account_created`.
//! 4. Provision: user-private group `gid = uid`, the passwd entry (home
//!    `<root>/principals/<id>/home`, the shell), then create and chown the §4
//!    dirs.
//!
//! It returns a [`ProvisionGuard`] that **undoes step 4 when dropped** unless
//! [`ProvisionGuard::committed`] is called after the caller's `COMMIT`. A
//! failure inside step 4 undoes what that step did before returning. The
//! caller rolls back, and records `provision_failed` in a fresh transaction —
//! [`Provisioner::create`] is that caller for the CLI.
//!
//! ## Backends (§7.2, OD-4)
//!
//! `groupadd`/`useradd` when they are on `PATH`; otherwise the built-in `/etc`
//! writer (`etc_files`). `--provisioning external` never writes `/etc`: the
//! operator pre-creates users and maps accounts onto them with
//! `--adopt-unix-user`.
//!
//! These spawns are host self-maintenance by root (like `path_fix.rs`'s), not
//! session spawns, so they do not go through the session executor (§9.5).

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

use sqlx::{Sqlite, SqlitePool, Transaction};
use zeroize::Zeroizing;

use super::accounts::{self, Account, Actor, SessionsRevokedHook, UsernameError};
use super::auth_events::{self, AuthEvent, AuthEventKind};
use super::etc_files::{EtcFiles, NewUser};
use super::password::{self, PasswordPolicyError};
use super::{sys, OperatorRoot, Ownership};
use crate::executor::{Principal, PrincipalId};

// ─── uid range (P-2) ────────────────────────────────────────────────────────

/// The operator's uid range, `--uid-range START-END` (default 20000-29999).
/// `START..END` is allocatable; `END` itself (`range_end`) is **permanently
/// reserved** as the §8 probe uid and is never an account's uid or gid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UidRange {
    start: u32,
    end: u32,
}

impl UidRange {
    pub const DEFAULT: UidRange = UidRange {
        start: 20_000,
        end: 29_999,
    };

    pub fn new(start: u32, end: u32) -> Result<Self, UidRangeError> {
        if start == 0 {
            return Err(UidRangeError("the range may not include uid 0".into()));
        }
        if start >= end {
            return Err(UidRangeError(format!(
                "start {start} must be below end {end} (end is the reserved probe uid)"
            )));
        }
        // 65534 is `nobody`/overflowuid; (uid_t)-1 and -2 are sentinels.
        if (start..=end).contains(&65_534) || end >= u32::MAX - 1 {
            return Err(UidRangeError(
                "the range may not include 65534 (nobody) or the (uid_t)-1/-2 sentinels".into(),
            ));
        }
        Ok(Self { start, end })
    }

    pub fn start(&self) -> u32 {
        self.start
    }

    /// `range_end`: the §8 probe uid, never allocated.
    pub fn probe_uid(&self) -> u32 {
        self.end
    }
}

impl Default for UidRange {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for UidRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UidRangeError(pub String);

impl fmt::Display for UidRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid --uid-range: {}", self.0)
    }
}

impl std::error::Error for UidRangeError {}

impl FromStr for UidRange {
    type Err = UidRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, b) = s
            .trim()
            .split_once('-')
            .ok_or_else(|| UidRangeError(format!("`{s}` is not START-END")))?;
        let parse = |v: &str| {
            v.trim()
                .parse::<u32>()
                .map_err(|_| UidRangeError(format!("`{v}` is not a uid")))
        };
        UidRange::new(parse(a)?, parse(b)?)
    }
}

// ─── backends ───────────────────────────────────────────────────────────────

/// `--provisioning` (§7.2 / OD-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum ProvisioningMode {
    /// `useradd`/`groupadd` when on `PATH`, else the built-in `/etc` writer.
    #[default]
    Auto,
    /// Never write `/etc`; accounts map onto pre-created users
    /// (`create --adopt-unix-user`).
    External,
}

/// Absolute paths of the shadow-utils tools, resolved once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShadowTools {
    groupadd: PathBuf,
    useradd: PathBuf,
    usermod: PathBuf,
    userdel: PathBuf,
    groupdel: PathBuf,
}

/// The only places the shadow-utils tools are looked for. Never the caller's
/// `$PATH`: this runs as root, and a stray `.` or user-writable entry there
/// would hand root to whatever `useradd` it found. Same list as the spawned
/// tools' own fixed `PATH`.
const SHADOW_TOOL_DIRS: [&str; 4] = ["/usr/sbin", "/sbin", "/usr/bin", "/bin"];

/// `dir/tool` if it is a root-owned regular executable that neither group nor
/// others can write (symlinks resolved, as on merged-/usr hosts).
fn trusted_tool(dir: &str, tool: &str) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let path = Path::new(dir).join(tool);
    let meta = fs::metadata(&path).ok()?;
    let ok =
        meta.is_file() && meta.uid() == 0 && meta.mode() & 0o022 == 0 && meta.mode() & 0o111 != 0;
    ok.then_some(path)
}

impl ShadowTools {
    fn find() -> Option<Self> {
        let find = |tool: &str| {
            SHADOW_TOOL_DIRS
                .iter()
                .find_map(|dir| trusted_tool(dir, tool))
        };
        Some(Self {
            groupadd: find("groupadd")?,
            useradd: find("useradd")?,
            usermod: find("usermod")?,
            userdel: find("userdel")?,
            groupdel: find("groupdel")?,
        })
    }

    fn run(&self, tool: &Path, args: &[&std::ffi::OsStr]) -> anyhow::Result<()> {
        // Root host maintenance: a fixed PATH and nothing else inherited, so
        // no operator secret (IKENGA_SECRET_*, tokens) reaches the tool.
        let out = Command::new(tool)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("LC_ALL", "C")
            .output()
            .map_err(|e| anyhow::anyhow!("{}: {e}", tool.display()))?;
        if !out.status.success() {
            anyhow::bail!(
                "{} {:?} failed ({}): {}",
                tool.display(),
                args,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Backend {
    ShadowUtils(ShadowTools),
    Builtin(EtcFiles),
    External,
}

const GECOS: &str = "ikenga principal";

impl Backend {
    fn name(&self) -> &'static str {
        match self {
            Backend::ShadowUtils(_) => "useradd",
            Backend::Builtin(_) => "builtin",
            Backend::External => "external",
        }
    }

    /// Host-side "is this uid/gid/name free?", beyond NSS: the built-in
    /// writer's files (identical to NSS `files` on the real `/etc`; the only
    /// view in a unit test's temp prefix).
    fn files(&self) -> Option<&EtcFiles> {
        match self {
            Backend::Builtin(files) => Some(files),
            _ => None,
        }
    }

    fn create_identity(
        &self,
        name: &str,
        uid: u32,
        home: &Path,
        shell: &Path,
    ) -> anyhow::Result<()> {
        match self {
            Backend::ShadowUtils(t) => {
                let (uid_s, home_s) = (uid.to_string(), home.as_os_str());
                t.run(&t.groupadd, &["-g".as_ref(), uid_s.as_ref(), name.as_ref()])?;
                let useradd = t.run(
                    &t.useradd,
                    &[
                        "-u".as_ref(),
                        uid_s.as_ref(),
                        "-g".as_ref(),
                        uid_s.as_ref(),
                        // No home creation (we create and chown it), no
                        // per-user group (made above), no lastlog entry.
                        "-M".as_ref(),
                        "-N".as_ref(),
                        "-l".as_ref(),
                        // No /etc/subuid or /etc/subgid range: with
                        // newuidmap a principal could otherwise own a block
                        // of 65 536 host ids (Debian's SUB_UID_COUNT). The
                        // built-in writer creates none either.
                        "-K".as_ref(),
                        "SUB_UID_COUNT=0".as_ref(),
                        "-K".as_ref(),
                        "SUB_GID_COUNT=0".as_ref(),
                        "-d".as_ref(),
                        home_s,
                        "-s".as_ref(),
                        shell.as_os_str(),
                        "-c".as_ref(),
                        GECOS.as_ref(),
                        name.as_ref(),
                    ],
                );
                if let Err(e) = useradd {
                    let _ = t.run(&t.groupdel, &[name.as_ref()]);
                    return Err(e);
                }
                Ok(())
            }
            Backend::Builtin(files) => Ok(files.add_user(&NewUser {
                name,
                uid,
                gid: uid,
                gecos: GECOS,
                home,
                shell,
            })?),
            Backend::External => anyhow::bail!("external provisioning never writes /etc"),
        }
    }

    fn remove_identity(&self, name: &str) -> anyhow::Result<()> {
        match self {
            Backend::ShadowUtils(t) => {
                let user = t.run(&t.userdel, &[name.as_ref()]);
                let group = t.run(&t.groupdel, &[name.as_ref()]);
                user.and(group)
            }
            Backend::Builtin(files) => Ok(files.remove_user(name)?),
            Backend::External => Ok(()),
        }
    }

    /// §7.3 disable: `!` password, nologin shell.
    fn lock(&self, name: &str, nologin: &Path) -> anyhow::Result<()> {
        match self {
            Backend::ShadowUtils(t) => t.run(
                &t.usermod,
                &[
                    "-L".as_ref(),
                    "-s".as_ref(),
                    nologin.as_os_str(),
                    name.as_ref(),
                ],
            ),
            Backend::Builtin(files) => {
                files.lock_password(name)?;
                if !files.set_shell(name, nologin)? {
                    anyhow::bail!("no passwd entry for {name}");
                }
                Ok(())
            }
            Backend::External => Ok(()),
        }
    }

    /// §7.3 enable: restore the shell. The password field stays locked —
    /// principals never have a Unix password; they log in through the daemon.
    fn unlock(&self, name: &str, shell: &Path) -> anyhow::Result<()> {
        match self {
            Backend::ShadowUtils(t) => t.run(
                &t.usermod,
                &["-s".as_ref(), shell.as_os_str(), name.as_ref()],
            ),
            Backend::Builtin(files) => {
                if !files.set_shell(name, shell)? {
                    anyhow::bail!("no passwd entry for {name}");
                }
                Ok(())
            }
            Backend::External => Ok(()),
        }
    }
}

fn nologin_shell() -> PathBuf {
    for candidate in ["/usr/sbin/nologin", "/sbin/nologin"] {
        if Path::new(candidate).exists() {
            return candidate.into();
        }
    }
    PathBuf::from("/usr/sbin/nologin")
}

/// The default login shell when the operator sets none (§1).
pub const DEFAULT_SHELL: &str = "/bin/sh";

// ─── uid-wide kill (§7.3) ───────────────────────────────────────────────────

/// What a disable did about the principal's running processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapOutcome {
    /// Every process of the uid was sent SIGKILL.
    Killed,
    /// No reaper was configured; nothing was signalled.
    Unavailable(&'static str),
    /// The helper could not run or failed. The account is disabled
    /// regardless (the disable committed first); its processes may live on.
    Failed(String),
}

/// Kills every process of a principal's uid (§7.3): a helper spawned
/// **through the T1 executor as that uid** calls `kill(-1, SIGKILL)`, which
/// also reaches detached chi-runners in their own process groups. The real
/// one is [`super::reaper::T1Reaper`].
pub trait UidReaper: Send + Sync {
    fn kill_all(&self, principal: &Principal) -> anyhow::Result<ReapOutcome>;
}

/// A reaper that signals nothing (tests, and callers that stop the
/// principal's processes some other way).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoReaper;

impl UidReaper for NoReaper {
    fn kill_all(&self, _principal: &Principal) -> anyhow::Result<ReapOutcome> {
        Ok(ReapOutcome::Unavailable("no uid reaper configured"))
    }
}

// ─── errors ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ProvisionError {
    Username(UsernameError),
    Password(PasswordPolicyError),
    UsernameTaken(String),
    NoSuchAccount(String),
    RangeExhausted(UidRange),
    UidTaken(u32),
    UnixNameTaken(String),
    /// `--provisioning external` can only adopt existing users.
    ExternalNeedsAdopt,
    AdoptRefused(String),
    /// `--uid-range` differs from the range the store was first used with.
    UidRangeMismatch {
        stored: String,
        requested: UidRange,
    },
    /// §8 step 7: the host's `/etc` disagrees with `accounts.db` in a way
    /// reconcile must not paper over.
    Drift(String),
    Host(anyhow::Error),
    Db(sqlx::Error),
}

impl fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProvisionError::Username(e) => e.fmt(f),
            ProvisionError::Password(e) => e.fmt(f),
            ProvisionError::UsernameTaken(u) => write!(f, "an account named `{u}` already exists"),
            ProvisionError::NoSuchAccount(u) => write!(f, "no account named `{u}`"),
            ProvisionError::RangeExhausted(r) => write!(
                f,
                "uid range {r} is exhausted (its last uid is reserved for the boot probe); \
                 widen --uid-range"
            ),
            ProvisionError::UidTaken(uid) => write!(
                f,
                "uid/gid {uid} is already held by a host user or group; the allocator never skips \
                 (uids are never reused) — move --uid-range or remove the host entry"
            ),
            ProvisionError::UnixNameTaken(n) => {
                write!(f, "a host user or group named `{n}` already exists")
            }
            ProvisionError::ExternalNeedsAdopt => f.write_str(
                "--provisioning external never writes /etc: pre-create the Unix user and pass \
                 --adopt-unix-user <name>",
            ),
            ProvisionError::AdoptRefused(why) => write!(f, "cannot adopt: {why}"),
            ProvisionError::UidRangeMismatch { stored, requested } => write!(
                f,
                "--uid-range {requested} differs from {stored}, the range this operator store \
                 was first provisioned with; the broker and the CLI must agree on it (its last \
                 uid is the boot probe's) — pass --uid-range {stored}"
            ),
            ProvisionError::Drift(why) => write!(f, "/etc disagrees with accounts.db: {why}"),
            ProvisionError::Host(e) => write!(f, "provisioning the host failed: {e:#}"),
            ProvisionError::Db(e) => write!(f, "accounts.db: {e}"),
        }
    }
}

impl std::error::Error for ProvisionError {}

impl From<sqlx::Error> for ProvisionError {
    fn from(e: sqlx::Error) -> Self {
        ProvisionError::Db(e)
    }
}

impl From<UsernameError> for ProvisionError {
    fn from(e: UsernameError) -> Self {
        ProvisionError::Username(e)
    }
}

impl From<PasswordPolicyError> for ProvisionError {
    fn from(e: PasswordPolicyError) -> Self {
        ProvisionError::Password(e)
    }
}

fn host_err(e: impl Into<anyhow::Error>) -> ProvisionError {
    ProvisionError::Host(e.into())
}

// ─── the guard (R-8) ────────────────────────────────────────────────────────

/// What step 4 created, undone on drop.
#[derive(Debug)]
struct Undo {
    backend: Backend,
    unix_name: String,
    identity: bool,
    dir: Option<PathBuf>,
}

impl Undo {
    fn run(self) {
        if self.identity {
            if let Err(e) = self.backend.remove_identity(&self.unix_name) {
                tracing::error!(
                    "provisioning rollback: could not remove host user {}: {e:#}",
                    self.unix_name
                );
            }
        }
        if let Some(dir) = self.dir {
            if let Err(e) = fs::remove_dir_all(&dir) {
                tracing::error!(
                    "provisioning rollback: could not remove {}: {e}",
                    dir.display()
                );
            }
        }
    }
}

/// The result of [`Provisioner::create_in`] (G-ACCESS R-8). Holds the inserted
/// row. Dropping it **undoes step 4** (the host user, group and principal
/// dirs) unless [`committed`](Self::committed) was called — so a caller whose
/// `COMMIT` fails, or who bails out, leaves no host state behind.
#[derive(Debug)]
#[must_use = "dropping the guard undoes the host provisioning; call committed() after COMMIT"]
pub struct ProvisionGuard {
    account: Account,
    undo: Option<Undo>,
}

impl ProvisionGuard {
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// Call after the caller's `COMMIT` succeeded: keeps step 4.
    pub fn committed(mut self) -> Account {
        self.undo = None;
        self.account.clone()
    }
}

impl Drop for ProvisionGuard {
    fn drop(&mut self) {
        if let Some(undo) = self.undo.take() {
            tracing::warn!(
                "provisioning of {} not committed; undoing its host user and dirs",
                self.account.username
            );
            undo.run();
        }
    }
}

// ─── the provisioner ────────────────────────────────────────────────────────

/// What §8 step 7 did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReconcileReport {
    /// Non-disabled rows checked.
    pub checked: usize,
    /// Host users (re)created.
    pub created: Vec<String>,
    /// Host users whose drifted shell was restored.
    pub repaired_shell: Vec<String>,
}

/// Result of a §7.3 disable.
#[derive(Debug)]
pub struct DisableReport {
    pub account: Account,
    pub reap: ReapOutcome,
}

/// Result of the env bootstrap (§7.4).
#[derive(Debug)]
pub enum BootstrapOutcome {
    Created(Account),
    /// `accounts` already had rows: the env vars were ignored.
    IgnoredNotEmpty,
}

/// `IKENGA_BOOTSTRAP_ADMIN` + `IKENGA_BOOTSTRAP_ADMIN_PASSWORD` (§7.4, OD-7).
pub struct BootstrapAdmin {
    pub username: String,
    pub password: Zeroizing<String>,
}

impl fmt::Debug for BootstrapAdmin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootstrapAdmin")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

pub const BOOTSTRAP_ADMIN_ENV: &str = "IKENGA_BOOTSTRAP_ADMIN";
pub const BOOTSTRAP_ADMIN_PASSWORD_ENV: &str = "IKENGA_BOOTSTRAP_ADMIN_PASSWORD";

impl BootstrapAdmin {
    /// Capture both bootstrap vars and **remove them from the process env**
    /// (§7.4: next to `main.rs`'s token stripping), whatever their values.
    /// `Ok(None)` when neither is set; an error when only one is.
    ///
    /// Must run before anything can spawn: the vars are gone afterwards.
    pub fn take_from_env() -> Result<Option<Self>, String> {
        Self::take_from(&mut |key| {
            let value = std::env::var_os(key);
            std::env::remove_var(key);
            value
        })
    }

    /// [`take_from_env`](Self::take_from_env) over any environment: `take`
    /// returns a variable's value **and removes it**. Tests use this with a
    /// map, never the process env (concurrent `setenv` is UB in glibc).
    pub(crate) fn take_from(
        take: &mut dyn FnMut(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Option<Self>, String> {
        let user = take(BOOTSTRAP_ADMIN_ENV);
        let pass = take(BOOTSTRAP_ADMIN_PASSWORD_ENV);
        let pass = pass.map(|p| Zeroizing::new(p.to_string_lossy().into_owned()));
        match (user, pass) {
            (None, None) => Ok(None),
            (Some(u), Some(p)) => Ok(Some(BootstrapAdmin {
                username: u.to_string_lossy().into_owned(),
                password: p,
            })),
            (Some(_), None) => Err(format!(
                "{BOOTSTRAP_ADMIN_ENV} is set without {BOOTSTRAP_ADMIN_PASSWORD_ENV}; ignoring both"
            )),
            (None, Some(_)) => Err(format!(
                "{BOOTSTRAP_ADMIN_PASSWORD_ENV} is set without {BOOTSTRAP_ADMIN_ENV}; ignoring both"
            )),
        }
    }
}

/// `create --adopt-unix-user <name> [--allow-system-user]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adopt<'a> {
    pub unix_user: &'a str,
    /// Permit a uid below `login.defs` `UID_MIN` (a system account).
    pub allow_system_user: bool,
}

impl<'a> Adopt<'a> {
    pub fn user(unix_user: &'a str) -> Self {
        Self {
            unix_user,
            allow_system_user: false,
        }
    }
}

/// The overflow id (`nobody` / `nogroup`): shared by every unmapped file and
/// many daemons, never a principal.
const OVERFLOW_ID: u32 = 65_534;

/// Why `pw` can't be adopted under `range`, if it can't.
fn adopt_refusal(
    pw: &sys::PasswdInfo,
    range: UidRange,
    uid_min: u32,
    allow_system_user: bool,
) -> Option<String> {
    let name = &pw.name;
    if pw.uid == 0 || pw.gid == 0 {
        return Some(format!(
            "`{name}` is root (uid {} gid {}); a principal never runs as root (I-1)",
            pw.uid, pw.gid
        ));
    }
    let probe = range.probe_uid();
    if pw.uid == probe || pw.gid == probe {
        return Some(format!(
            "`{name}` holds {probe}, the reserved probe uid of --uid-range {range}"
        ));
    }
    for id in [pw.uid, pw.gid] {
        if id == OVERFLOW_ID || id >= u32::MAX - 1 {
            return Some(format!(
                "`{name}` holds {id}, the overflow id (nobody) or a (uid_t)-1/-2 sentinel"
            ));
        }
    }
    if pw.uid < uid_min && !allow_system_user {
        return Some(format!(
            "`{name}` (uid {}) is a system user (below UID_MIN {uid_min}); pass \
             --allow-system-user if that is really meant",
            pw.uid
        ));
    }
    None
}

/// `UID_MIN` from `login.defs` text; `None` when absent or unparsable.
fn parse_uid_min(login_defs: &str) -> Option<u32> {
    login_defs.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "UID_MIN")
            .then(|| words.next()?.parse().ok())
            .flatten()
    })
}

/// The host's `UID_MIN` (`/etc/login.defs`), else 1000.
fn login_defs_uid_min() -> u32 {
    fs::read_to_string("/etc/login.defs")
        .ok()
        .and_then(|text| parse_uid_min(&text))
        .unwrap_or(1000)
}

/// The provisioning core, configured for one operator root.
#[derive(Debug, Clone)]
pub struct Provisioner {
    root: OperatorRoot,
    range: UidRange,
    backend: Backend,
    ownership: Ownership,
    actor: Actor,
    nologin: PathBuf,
}

impl Provisioner {
    pub fn new(root: OperatorRoot, range: UidRange, mode: ProvisioningMode, actor: Actor) -> Self {
        let backend = match mode {
            ProvisioningMode::External => Backend::External,
            ProvisioningMode::Auto => match ShadowTools::find() {
                Some(tools) => Backend::ShadowUtils(tools),
                None => Backend::Builtin(EtcFiles::system()),
            },
        };
        Self {
            root,
            range,
            backend,
            ownership: Ownership::Enforce,
            actor,
            nologin: nologin_shell(),
        }
    }

    /// Built-in writer against `<etc_prefix>/etc`, owners not enforced.
    #[cfg(test)]
    pub(crate) fn for_tests(root: OperatorRoot, range: UidRange, etc_prefix: &Path) -> Self {
        Self {
            root,
            range,
            backend: Backend::Builtin(EtcFiles::at(etc_prefix)),
            ownership: Ownership::SkipForTests,
            actor: Actor::Cli,
            nologin: PathBuf::from("/usr/sbin/nologin"),
        }
    }

    #[cfg(test)]
    pub(crate) fn external_for_tests(root: OperatorRoot, range: UidRange) -> Self {
        Self {
            backend: Backend::External,
            ..Self::for_tests(root, range, Path::new("/nonexistent"))
        }
    }

    pub fn root(&self) -> &OperatorRoot {
        &self.root
    }

    pub fn range(&self) -> UidRange {
        self.range
    }

    /// `useradd`, `builtin` or `external`.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    fn host_uid_taken(&self, uid: u32) -> Result<bool, ProvisionError> {
        if sys::user_by_uid(uid).map_err(host_err)?.is_some()
            || sys::group_gid_exists(uid).map_err(host_err)?
        {
            return Ok(true);
        }
        if let Some(files) = self.backend.files() {
            return Ok(files.uid_taken(uid).map_err(host_err)?
                || files.gid_taken(uid).map_err(host_err)?);
        }
        Ok(false)
    }

    fn host_name_taken(&self, name: &str) -> Result<bool, ProvisionError> {
        if sys::user_by_name(name).map_err(host_err)?.is_some()
            || sys::group_name_exists(name).map_err(host_err)?
        {
            return Ok(true);
        }
        if let Some(files) = self.backend.files() {
            return files.name_taken(name).map_err(host_err);
        }
        Ok(false)
    }

    /// §7.2 step 2.
    async fn allocate_uid(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<u32, ProvisionError> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(unix_uid) FROM accounts WHERE adopted = 0")
                .fetch_one(&mut **tx)
                .await?;
        let next = match max {
            Some(m) => (m + 1).max(i64::from(self.range.start)),
            None => i64::from(self.range.start),
        };
        if next >= i64::from(self.range.probe_uid()) {
            return Err(ProvisionError::RangeExhausted(self.range));
        }
        let uid = next as u32;
        if self.host_uid_taken(uid)? {
            return Err(ProvisionError::UidTaken(uid));
        }
        Ok(uid)
    }

    /// The §4 principal dirs, all `0700` and owned by `uid:gid`: `<id>/`,
    /// `<id>/data/`, `<id>/data/tmp/` and, unless the home lives elsewhere
    /// (adopted), `<id>/home/`.
    ///
    /// The whole subtree is created and chmodded **while still root-owned**,
    /// then chowned **leaf first, `<id>/` last**. Until that last chown the
    /// uid can't even enter `<id>/`, so no process of an (adopted, possibly
    /// live) uid can swap a child for a symlink between root's create and
    /// root's path-based chmod (which follows symlinks). `lchown` never
    /// follows one.
    fn make_principal_dirs(
        &self,
        id: PrincipalId,
        uid: u32,
        gid: u32,
        with_home: bool,
    ) -> io::Result<()> {
        let data = self.root.principal_data(id);
        let mut dirs = vec![self.root.principal_dir(id)];
        if with_home {
            dirs.push(self.root.principal_home(id));
        }
        dirs.push(data.clone());
        dirs.push(data.join("tmp"));
        for dir in &dirs {
            fs::DirBuilder::new().mode(0o700).create(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        if self.ownership == Ownership::Enforce {
            for dir in dirs.iter().rev() {
                std::os::unix::fs::lchown(dir, Some(uid), Some(gid))?;
            }
        }
        Ok(())
    }

    /// Record `--uid-range` in the store the first time it provisions, and
    /// refuse a different one afterwards (the CLI and the broker are separate
    /// invocations; a mismatch would let one allocate the other's probe uid).
    /// Runs inside the caller's `BEGIN IMMEDIATE`. The broker's boot and the
    /// §8 probe (WP-20 slices 2/3) call it too.
    pub async fn pin_uid_range(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
    ) -> Result<(), ProvisionError> {
        let stored: Option<String> =
            sqlx::query_scalar("SELECT value FROM operator_meta WHERE key = 'uid_range'")
                .fetch_optional(&mut **tx)
                .await?;
        match stored {
            None => {
                sqlx::query("INSERT INTO operator_meta (key, value) VALUES ('uid_range', ?)")
                    .bind(self.range.to_string())
                    .execute(&mut **tx)
                    .await?;
                Ok(())
            }
            Some(stored) if stored == self.range.to_string() => Ok(()),
            Some(stored) => Err(ProvisionError::UidRangeMismatch {
                stored,
                requested: self.range,
            }),
        }
    }

    async fn insert_row(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        row: &Account,
    ) -> Result<(), ProvisionError> {
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, password_phc, unix_name, unix_uid, \
             unix_gid, home, shell, is_admin, session_epoch, adopted, disabled_at, created_at, \
             updated_at, password_changed_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, NULL, ?, ?, ?)",
        )
        .bind(row.principal_id.to_string())
        .bind(&row.username)
        .bind(&row.password_phc)
        .bind(&row.unix_name)
        .bind(i64::from(row.unix_uid))
        .bind(i64::from(row.unix_gid))
        .bind(row.home.to_string_lossy().into_owned())
        .bind(row.shell.to_string_lossy().into_owned())
        .bind(i64::from(row.is_admin))
        .bind(i64::from(row.adopted))
        .bind(row.created_at)
        .bind(row.updated_at)
        .bind(row.password_changed_at)
        .execute(&mut **tx)
        .await?;
        auth_events::record(
            tx,
            AuthEvent::new(AuthEventKind::AccountCreated)
                .principal(row.principal_id)
                .detail(serde_json::json!({
                    "via": self.actor.as_str(),
                    "unix_name": row.unix_name,
                    "unix_uid": row.unix_uid,
                    "is_admin": row.is_admin,
                    "adopted": row.adopted,
                    "backend": self.backend.name(),
                })),
        )
        .await?;
        Ok(())
    }

    async fn refuse_taken_username(
        tx: &mut Transaction<'_, Sqlite>,
        username: &str,
    ) -> Result<(), ProvisionError> {
        if accounts::by_username(tx, username).await?.is_some() {
            return Err(ProvisionError::UsernameTaken(username.into()));
        }
        Ok(())
    }

    /// §7.2 steps 2–4 inside the caller's `BEGIN IMMEDIATE` (R-8). See the
    /// module docs for the contract of the returned guard.
    ///
    /// On an `Err`, the caller **must** roll back and then call
    /// [`record_provision_failed`](Self::record_provision_failed) (§7.2: a
    /// failed create records `provision_failed`), as [`create`](Self::create)
    /// does for the CLI.
    pub async fn create_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        username: &str,
        password: &str,
        is_admin: bool,
    ) -> Result<ProvisionGuard, ProvisionError> {
        let unix_name = accounts::unix_name_for(username)?;
        password::validate_new_password(password)?;
        if self.backend == Backend::External {
            return Err(ProvisionError::ExternalNeedsAdopt);
        }
        self.pin_uid_range(tx).await?;
        Self::refuse_taken_username(tx, username).await?;

        // Step 2.
        let uid = self.allocate_uid(tx).await?;
        if self.host_name_taken(&unix_name)? {
            return Err(ProvisionError::UnixNameTaken(unix_name));
        }

        // Step 3.
        let id = PrincipalId::new_v7();
        let phc = password::hash(Zeroizing::new(password.to_string()))
            .await
            .map_err(ProvisionError::Host)?;
        let now = accounts::now_secs();
        let account = Account {
            principal_id: id,
            username: username.to_string(),
            password_phc: Some(phc),
            unix_name: unix_name.clone(),
            unix_uid: uid,
            unix_gid: uid,
            home: self.root.principal_home(id),
            shell: PathBuf::from(DEFAULT_SHELL),
            is_admin,
            session_epoch: 0,
            adopted: false,
            disabled_at: None,
            created_at: now,
            updated_at: now,
            password_changed_at: Some(now),
        };
        self.insert_row(tx, &account).await?;

        // Step 4. The guard is armed as each piece lands, so an early return
        // drops it and undoes exactly what exists.
        let mut guard = ProvisionGuard {
            account,
            undo: Some(Undo {
                backend: self.backend.clone(),
                unix_name: unix_name.clone(),
                identity: false,
                dir: None,
            }),
        };
        self.backend
            .create_identity(&unix_name, uid, &guard.account.home, &guard.account.shell)
            .map_err(ProvisionError::Host)?;
        if let Some(undo) = guard.undo.as_mut() {
            undo.identity = true;
            undo.dir = Some(self.root.principal_dir(id));
        }
        self.make_principal_dirs(id, uid, uid, true)
            .map_err(host_err)?;
        Ok(guard)
    }

    /// `create --adopt-unix-user <name>` (§7.1, §11.2): map an account onto an
    /// **existing** host user (`adopted = 1`). The uid may lie outside the
    /// range, but never 0 and never `range_end` (the probe uid). Its home is
    /// the user's passwd home; only `<id>/data` is created. Works under every
    /// backend — it is the only create `--provisioning external` allows.
    ///
    /// Also refused: `nobody` / the overflow id 65534, the `(uid_t)-1/-2`
    /// sentinels, and system users (uid below `login.defs` `UID_MIN`) unless
    /// [`Adopt::allow_system_user`] is set. Errors are recorded by the caller
    /// as for [`create_in`](Self::create_in).
    pub async fn adopt_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        username: &str,
        password: &str,
        is_admin: bool,
        adopt: Adopt<'_>,
    ) -> Result<ProvisionGuard, ProvisionError> {
        let unix_user = adopt.unix_user;
        if username.is_empty() || username.chars().count() > accounts::MAX_USERNAME_CHARS {
            return Err(ProvisionError::Username(if username.is_empty() {
                UsernameError::Empty
            } else {
                UsernameError::TooLong
            }));
        }
        password::validate_new_password(password)?;
        self.pin_uid_range(tx).await?;
        Self::refuse_taken_username(tx, username).await?;
        let pw = sys::user_by_name(unix_user)
            .map_err(host_err)?
            .ok_or_else(|| ProvisionError::AdoptRefused(format!("no host user `{unix_user}`")))?;
        if let Some(why) = adopt_refusal(
            &pw,
            self.range,
            login_defs_uid_min(),
            adopt.allow_system_user,
        ) {
            return Err(ProvisionError::AdoptRefused(why));
        }
        let held: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE unix_uid = ? OR unix_name = ?")
                .bind(i64::from(pw.uid))
                .bind(unix_user)
                .fetch_one(&mut **tx)
                .await?;
        if held > 0 {
            return Err(ProvisionError::AdoptRefused(format!(
                "`{unix_user}` (uid {}) is already an account's Unix user",
                pw.uid
            )));
        }
        if !pw.home.is_absolute() {
            return Err(ProvisionError::AdoptRefused(format!(
                "`{unix_user}` has no absolute home"
            )));
        }

        let id = PrincipalId::new_v7();
        let phc = password::hash(Zeroizing::new(password.to_string()))
            .await
            .map_err(ProvisionError::Host)?;
        let now = accounts::now_secs();
        let shell = if pw.shell.as_os_str().is_empty() {
            PathBuf::from(DEFAULT_SHELL)
        } else {
            pw.shell.clone()
        };
        let account = Account {
            principal_id: id,
            username: username.to_string(),
            password_phc: Some(phc),
            unix_name: unix_user.to_string(),
            unix_uid: pw.uid,
            unix_gid: pw.gid,
            home: pw.home.clone(),
            shell,
            is_admin,
            session_epoch: 0,
            adopted: true,
            disabled_at: None,
            created_at: now,
            updated_at: now,
            password_changed_at: Some(now),
        };
        self.insert_row(tx, &account).await?;
        let guard = ProvisionGuard {
            account,
            undo: Some(Undo {
                backend: self.backend.clone(),
                unix_name: unix_user.to_string(),
                identity: false, // never remove a user we did not create
                dir: Some(self.root.principal_dir(id)),
            }),
        };
        self.make_principal_dirs(id, pw.uid, pw.gid, false)
            .map_err(host_err)?;
        Ok(guard)
    }

    /// Write `provision_failed` for `username` in its own `BEGIN IMMEDIATE`
    /// (§7.2). Call it **after** rolling back a failed
    /// [`create_in`](Self::create_in) / [`adopt_in`](Self::adopt_in); a
    /// failure to record is logged, never returned.
    pub async fn record_provision_failed(
        &self,
        pool: &SqlitePool,
        username: &str,
        err: &ProvisionError,
    ) {
        let recorded = async {
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            auth_events::record(
                &mut tx,
                AuthEvent::new(AuthEventKind::ProvisionFailed)
                    .username_tried(username)
                    .detail(serde_json::json!({
                        "via": self.actor.as_str(),
                        "backend": self.backend.name(),
                        "error": err.to_string(),
                    })),
            )
            .await?;
            tx.commit().await
        }
        .await;
        if let Err(e) = recorded {
            tracing::error!("could not record provision_failed for {username}: {e}");
        }
    }

    /// The CLI's create: `BEGIN IMMEDIATE`, [`create_in`](Self::create_in)
    /// (or [`adopt_in`](Self::adopt_in)), `COMMIT`, `committed()`. On any
    /// failure the transaction rolls back, step 4 is undone, and
    /// `provision_failed` is recorded.
    pub async fn create(
        &self,
        pool: &SqlitePool,
        username: &str,
        password: &str,
        is_admin: bool,
        adopt: Option<Adopt<'_>>,
    ) -> Result<Account, ProvisionError> {
        let result = async {
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            let guard = match adopt {
                Some(adopt) => {
                    self.adopt_in(&mut tx, username, password, is_admin, adopt)
                        .await?
                }
                None => {
                    self.create_in(&mut tx, username, password, is_admin)
                        .await?
                }
            };
            // A failed COMMIT drops `guard`, which undoes step 4.
            tx.commit().await?;
            Ok(guard.committed())
        }
        .await;
        if let Err(e) = &result {
            self.record_provision_failed(pool, username, e).await;
        }
        result
    }

    async fn lookup(
        tx: &mut Transaction<'_, Sqlite>,
        username: &str,
    ) -> Result<Account, ProvisionError> {
        accounts::by_username(tx, username)
            .await?
            .ok_or_else(|| ProvisionError::NoSuchAccount(username.into()))
    }

    /// §7.3 passwd: rehash, set `password_changed_at`, bump `session_epoch`,
    /// write `password_changed`.
    pub async fn passwd(
        &self,
        pool: &SqlitePool,
        username: &str,
        password: &str,
    ) -> Result<Account, ProvisionError> {
        password::validate_new_password(password)?;
        // Hashed before the write lock is taken.
        let phc = password::hash(Zeroizing::new(password.to_string()))
            .await
            .map_err(ProvisionError::Host)?;
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let account = Self::lookup(&mut tx, username).await?;
        let account =
            accounts::set_password_in(&mut tx, account.principal_id, &phc, self.actor).await?;
        tx.commit().await?;
        Ok(account)
    }

    /// Forced logout (G-ACCESS R-11): bump the epoch, write
    /// `sessions_revoked`, and run `hook` in the same transaction.
    pub async fn revoke_sessions(
        &self,
        pool: &SqlitePool,
        username: &str,
        hook: &dyn SessionsRevokedHook,
    ) -> Result<Account, ProvisionError> {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let account = Self::lookup(&mut tx, username).await?;
        let account = accounts::revoke_sessions_in(&mut tx, account.principal_id, self.actor, hook)
            .await
            .map_err(ProvisionError::Host)?;
        tx.commit().await?;
        Ok(account)
    }

    /// §7.3 disable: set `disabled_at`, bump `session_epoch`, lock the passwd
    /// entry (`!` password, nologin shell) — all before `COMMIT`, so a failed
    /// lock disables nothing — then kill every process of the uid. Files, uid
    /// and `principal_id` are kept. Stopping the principal's child is the
    /// broker's (slice 3).
    pub async fn disable(
        &self,
        pool: &SqlitePool,
        username: &str,
        reaper: &dyn UidReaper,
    ) -> Result<DisableReport, ProvisionError> {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let account = Self::lookup(&mut tx, username).await?;
        let account = accounts::mark_disabled_in(&mut tx, account.principal_id, self.actor).await?;
        self.backend
            .lock(&account.unix_name, &self.nologin)
            .map_err(ProvisionError::Host)?;
        tx.commit().await?;
        // After COMMIT: the account is disabled whatever the kill does, and a
        // failed kill is reported rather than turned into a failed disable.
        let reap = reaper
            .kill_all(&account.principal())
            .unwrap_or_else(|e| ReapOutcome::Failed(format!("{e:#}")));
        Ok(DisableReport { account, reap })
    }

    /// §7.3 enable: clear `disabled_at` and restore the shell. The Unix
    /// password stays locked: allocated principals never have one. (An
    /// adopted user's own Unix password, locked by disable, is left for the
    /// operator to unlock with `usermod -U`.)
    pub async fn enable(
        &self,
        pool: &SqlitePool,
        username: &str,
    ) -> Result<Account, ProvisionError> {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let account = Self::lookup(&mut tx, username).await?;
        let account = accounts::mark_enabled_in(&mut tx, account.principal_id, self.actor).await?;
        self.backend
            .unlock(&account.unix_name, &account.shell)
            .map_err(ProvisionError::Host)?;
        if let Err(e) = tx.commit().await {
            // Fail closed: the row is still disabled, so lock /etc again.
            let _ = self.backend.lock(&account.unix_name, &self.nologin);
            return Err(e.into());
        }
        Ok(account)
    }

    /// The passwd entry named `name`, as the host (or, in a unit test, the
    /// built-in writer's temp prefix) sees it.
    fn passwd_by_name(&self, name: &str) -> Result<Option<sys::PasswdInfo>, ProvisionError> {
        match self.backend.files() {
            Some(files) if !files.is_system() => files.passwd_by_name(name).map_err(host_err),
            _ => sys::user_by_name(name).map_err(host_err),
        }
    }

    fn passwd_by_uid(&self, uid: u32) -> Result<Option<sys::PasswdInfo>, ProvisionError> {
        match self.backend.files() {
            Some(files) if !files.is_system() => files.passwd_by_uid(uid).map_err(host_err),
            _ => sys::user_by_uid(uid).map_err(host_err),
        }
    }

    /// §8 step 7 — `/etc` is a projection of `accounts.db` (§4): for every
    /// non-disabled row the passwd entry must match `(unix_name, uid, gid,
    /// home, shell)`. A missing entry of an allocated account is recreated
    /// with the §7.2 backend; a drifted shell is restored. Anything else —
    /// the row's uid or name held by a **different** host entry, a moved
    /// home, a missing adopted user, or a missing entry under
    /// `--provisioning external` — is refused, never rewritten. Reconcile
    /// never writes the probe uid: no row holds it (§7.2).
    pub async fn reconcile(&self, pool: &SqlitePool) -> Result<ReconcileReport, ProvisionError> {
        let rows = {
            let mut conn = pool.acquire().await?;
            accounts::list(&mut conn).await?
        };
        let mut report = ReconcileReport::default();
        for a in rows.iter().filter(|a| !a.is_disabled()) {
            report.checked += 1;
            let who = format!("account `{}` ({})", a.username, a.unix_name);
            if let Some(holder) = self.passwd_by_uid(a.unix_uid)? {
                if holder.name != a.unix_name {
                    return Err(ProvisionError::Drift(format!(
                        "{who}: uid {} is held by host user `{}`",
                        a.unix_uid, holder.name
                    )));
                }
            }
            match self.passwd_by_name(&a.unix_name)? {
                Some(pw) => {
                    if (pw.uid, pw.gid) != (a.unix_uid, a.unix_gid) {
                        return Err(ProvisionError::Drift(format!(
                            "{who}: the host entry has uid:gid {}:{}, accounts.db {}:{}",
                            pw.uid, pw.gid, a.unix_uid, a.unix_gid
                        )));
                    }
                    if pw.home != a.home {
                        return Err(ProvisionError::Drift(format!(
                            "{who}: the host entry's home is {}, accounts.db's {}",
                            pw.home.display(),
                            a.home.display()
                        )));
                    }
                    if pw.shell != a.shell {
                        if a.adopted || matches!(self.backend, Backend::External) {
                            tracing::warn!(
                                "{who}: host shell {} differs from {} (operator-managed; left \
                                 as is)",
                                pw.shell.display(),
                                a.shell.display()
                            );
                        } else {
                            self.backend
                                .unlock(&a.unix_name, &a.shell)
                                .map_err(ProvisionError::Host)?;
                            report.repaired_shell.push(a.unix_name.clone());
                        }
                    }
                }
                None if a.adopted => {
                    return Err(ProvisionError::Drift(format!(
                        "{who} is adopted, but host user `{}` no longer exists; recreate it \
                         (uid {}, home {})",
                        a.unix_name,
                        a.unix_uid,
                        a.home.display()
                    )))
                }
                None if matches!(self.backend, Backend::External) => {
                    return Err(ProvisionError::Drift(format!(
                        "{who} has no host user, and --provisioning external never writes \
                         /etc; pre-create `{}` with uid {} and home {}",
                        a.unix_name,
                        a.unix_uid,
                        a.home.display()
                    )))
                }
                None => {
                    self.backend
                        .create_identity(&a.unix_name, a.unix_uid, &a.home, &a.shell)
                        .map_err(ProvisionError::Host)?;
                    tracing::info!(
                        "reconcile: recreated host user {} (uid {})",
                        a.unix_name,
                        a.unix_uid
                    );
                    report.created.push(a.unix_name.clone());
                }
            }
        }
        Ok(report)
    }

    /// §7.4: create `bootstrap` as an admin **only if `accounts` is empty**,
    /// checked inside the same `BEGIN IMMEDIATE` as the create. Logged without
    /// the password.
    pub async fn bootstrap_admin(
        &self,
        pool: &SqlitePool,
        bootstrap: &BootstrapAdmin,
    ) -> Result<BootstrapOutcome, ProvisionError> {
        let result = async {
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
            if accounts::count(&mut tx).await? > 0 {
                return Ok(BootstrapOutcome::IgnoredNotEmpty);
            }
            let guard = self
                .create_in(&mut tx, &bootstrap.username, &bootstrap.password, true)
                .await?;
            tx.commit().await?;
            Ok(BootstrapOutcome::Created(guard.committed()))
        }
        .await;
        match &result {
            Ok(BootstrapOutcome::Created(a)) => tracing::info!(
                "{BOOTSTRAP_ADMIN_ENV}: created admin account `{}` ({})",
                a.username,
                a.principal_id
            ),
            Ok(BootstrapOutcome::IgnoredNotEmpty) => tracing::warn!(
                "{BOOTSTRAP_ADMIN_ENV} ignored: accounts already exist (it is honoured only on an \
                 empty accounts table)"
            ),
            Err(e) => {
                self.record_provision_failed(pool, &bootstrap.username, e)
                    .await;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::operator::accounts::NoDeviceGrants;
    use crate::server::operator::etc_files::tests::fake_etc;
    use crate::server::operator::{open_accounts, test_support, Opener};
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Far above any real host's ids, so NSS never collides in CI. The fake
    /// `/etc` holds a host user at 3_900_000_003 and a group at …004.
    const RANGE: UidRange = UidRange {
        start: 3_900_000_000,
        end: 3_900_000_006,
    };
    const PW: &str = "correct horse battery";

    struct Fixture {
        _root_tmp: tempfile::TempDir,
        _etc_tmp: tempfile::TempDir,
        etc: EtcFiles,
        prov: Provisioner,
        pool: SqlitePool,
    }

    async fn fixture() -> Fixture {
        let (root_tmp, root) = test_support::temp_root();
        let (etc_tmp, etc) = fake_etc(true);
        let prov = Provisioner::for_tests(root.clone(), RANGE, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        Fixture {
            _root_tmp: root_tmp,
            _etc_tmp: etc_tmp,
            etc,
            prov,
            pool,
        }
    }

    async fn events(pool: &SqlitePool) -> Vec<(String, Option<String>)> {
        sqlx::query_as("SELECT kind, principal_id FROM auth_events ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[test]
    fn uid_range_parses_and_reserves_its_end() {
        let r: UidRange = "20000-29999".parse().unwrap();
        assert_eq!(r, UidRange::DEFAULT);
        assert_eq!(r.probe_uid(), 29_999);
        assert_eq!(r.to_string(), "20000-29999");
        for bad in [
            "",
            "20000",
            "0-10",
            "10-10",
            "11-10",
            "60000-70000",
            "a-b",
            "1-4294967295",
        ] {
            assert!(bad.parse::<UidRange>().is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn create_provisions_the_row_the_etc_entries_and_the_dirs() {
        let f = fixture().await;
        let a = f.prov.create(&f.pool, "Ada", PW, true, None).await.unwrap();
        assert_eq!(a.unix_name, "ik-ada");
        assert_eq!((a.unix_uid, a.unix_gid), (RANGE.start, RANGE.start));
        assert!(a.is_admin && !a.adopted && !a.is_disabled());
        assert_eq!(a.home, f.prov.root().principal_home(a.principal_id));
        assert_eq!(a.shell, Path::new(DEFAULT_SHELL));
        assert!(password::verify_blocking(
            a.password_phc.as_deref().unwrap(),
            PW
        ));

        let line = f.etc.line("passwd", "ik-ada").unwrap();
        assert_eq!(
            line,
            format!(
                "ik-ada:x:{0}:{0}:ikenga principal:{1}:/bin/sh",
                RANGE.start,
                a.home.display()
            )
        );
        assert!(f.etc.line("group", "ik-ada").is_some());

        let root = f.prov.root();
        for dir in [
            root.principal_dir(a.principal_id),
            root.principal_home(a.principal_id),
            root.principal_data(a.principal_id),
            root.principal_data(a.principal_id).join("tmp"),
        ] {
            let meta = fs::symlink_metadata(&dir).unwrap();
            assert!(meta.is_dir(), "{}", dir.display());
            assert_eq!(meta.mode() & 0o7777, 0o700, "{}", dir.display());
        }
        assert_eq!(
            events(&f.pool).await,
            vec![("account_created".into(), Some(a.principal_id.to_string()))]
        );
    }

    #[tokio::test]
    async fn a_dropped_guard_undoes_step_4_and_the_rollback_undoes_the_row() {
        let f = fixture().await;
        let id = {
            let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let guard = f.prov.create_in(&mut tx, "ada", PW, false).await.unwrap();
            let id = guard.account().principal_id;
            assert!(f.etc.line("passwd", "ik-ada").is_some());
            assert!(f.prov.root().principal_dir(id).exists());
            drop(guard); // e.g. the caller bailed before COMMIT
            id
        };
        assert!(f.etc.line("passwd", "ik-ada").is_none());
        assert!(f.etc.line("group", "ik-ada").is_none());
        assert!(!f.prov.root().principal_dir(id).exists());
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts")
            .fetch_one(&f.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn committed_keeps_step_4() {
        let f = fixture().await;
        let mut tx = f.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let guard = f.prov.create_in(&mut tx, "ada", PW, false).await.unwrap();
        tx.commit().await.unwrap();
        let a = guard.committed();
        assert!(f.etc.line("passwd", "ik-ada").is_some());
        assert!(f.prov.root().principal_home(a.principal_id).exists());
    }

    /// I-4 + §7.2 step 2: disabled rows count, adopted rows don't, ids and
    /// uids are never reused.
    #[tokio::test]
    async fn allocator_never_reuses_and_counts_disabled_rows() {
        let f = fixture().await;
        let a = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        f.prov.disable(&f.pool, "ada", &NoReaper).await.unwrap();
        let b = f
            .prov
            .create(&f.pool, "bob", PW, false, None)
            .await
            .unwrap();
        assert_eq!(b.unix_uid, a.unix_uid + 1);
        assert_ne!(a.principal_id, b.principal_id);

        // An adopted row far above the allocation point doesn't move it.
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             adopted, created_at, updated_at) VALUES (?, 'old', 'old', ?, ?, '/home/old', 1, 0, 0)",
        )
        .bind(PrincipalId::new_v7().to_string())
        .bind(i64::from(RANGE.start + 5))
        .bind(i64::from(RANGE.start + 5))
        .execute(&f.pool)
        .await
        .unwrap();
        let c = f.prov.create(&f.pool, "cy", PW, false, None).await.unwrap();
        assert_eq!(c.unix_uid, b.unix_uid + 1);

        // Rows are tombstones: a disabled account is still listed and its
        // principal_id still resolves.
        let mut conn = f.pool.acquire().await.unwrap();
        let all = accounts::list(&mut conn).await.unwrap();
        assert_eq!(all.len(), 4);
        assert!(accounts::by_id(&mut conn, a.principal_id)
            .await
            .unwrap()
            .unwrap()
            .is_disabled());
    }

    #[tokio::test]
    async fn host_held_uid_fails_records_provision_failed_and_leaves_nothing() {
        let f = fixture().await;
        // Walk the allocator up to the fake host user at start+3.
        for name in ["a1", "a2", "a3"] {
            f.prov.create(&f.pool, name, PW, false, None).await.unwrap();
        }
        let err = f
            .prov
            .create(&f.pool, "a4", PW, false, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProvisionError::UidTaken(uid) if uid == RANGE.start + 3),
            "{err}"
        );
        assert!(f.etc.line("passwd", "ik-a4").is_none());
        let mut conn = f.pool.acquire().await.unwrap();
        assert!(accounts::by_username(&mut conn, "a4")
            .await
            .unwrap()
            .is_none());
        let last: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT kind, principal_id, username_tried FROM auth_events ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(last, ("provision_failed".into(), None, Some("a4".into())));
    }

    #[tokio::test]
    async fn the_range_end_is_never_allocated() {
        let (_root_tmp, root) = test_support::temp_root();
        let (etc_tmp, _etc) = fake_etc(true);
        let range = UidRange::new(3_900_000_010, 3_900_000_012).unwrap();
        let prov = Provisioner::for_tests(root.clone(), range, etc_tmp.path());
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        prov.create(&pool, "a", PW, false, None).await.unwrap();
        let b = prov.create(&pool, "b", PW, false, None).await.unwrap();
        assert_eq!(b.unix_uid, 3_900_000_011);
        let err = prov.create(&pool, "c", PW, false, None).await.unwrap_err();
        assert!(matches!(err, ProvisionError::RangeExhausted(_)), "{err}");
    }

    #[tokio::test]
    async fn duplicate_names_are_refused() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let err = f
            .prov
            .create(&f.pool, "ADA", PW, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ProvisionError::UsernameTaken(_)), "{err}");
        // A host user already named ik-<username>.
        f.etc
            .add_user(&NewUser {
                name: "ik-eve",
                uid: 3_900_000_100,
                gid: 3_900_000_100,
                gecos: "",
                home: Path::new("/home/eve"),
                shell: Path::new("/bin/sh"),
            })
            .unwrap();
        let err = f
            .prov
            .create(&f.pool, "eve", PW, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ProvisionError::UnixNameTaken(_)), "{err}");
        // Bad username / weak password never reach the host.
        assert!(matches!(
            f.prov
                .create(&f.pool, "a.b", PW, false, None)
                .await
                .unwrap_err(),
            ProvisionError::Username(_)
        ));
        assert!(matches!(
            f.prov
                .create(&f.pool, "zed", "short", false, None)
                .await
                .unwrap_err(),
            ProvisionError::Password(_)
        ));
    }

    #[tokio::test]
    async fn passwd_bumps_the_epoch_and_changes_the_hash() {
        let f = fixture().await;
        let a = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let b = f
            .prov
            .passwd(&f.pool, "ada", "a whole new password")
            .await
            .unwrap();
        assert_eq!(b.session_epoch, a.session_epoch + 1);
        assert!(password::verify_blocking(
            b.password_phc.as_deref().unwrap(),
            "a whole new password"
        ));
        assert!(!password::verify_blocking(
            b.password_phc.as_deref().unwrap(),
            PW
        ));
        assert_eq!(events(&f.pool).await.last().unwrap().0, "password_changed");
        assert!(matches!(
            f.prov
                .passwd(&f.pool, "nobody", "a whole new password")
                .await,
            Err(ProvisionError::NoSuchAccount(_))
        ));
    }

    struct CountingReaper(AtomicUsize);
    impl UidReaper for CountingReaper {
        fn kill_all(&self, p: &Principal) -> anyhow::Result<ReapOutcome> {
            assert_ne!(p.uid, 0);
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ReapOutcome::Killed)
        }
    }

    #[tokio::test]
    async fn disable_locks_bumps_and_reaps_and_enable_restores() {
        let f = fixture().await;
        let a = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let reaper = CountingReaper(AtomicUsize::new(0));
        let report = f.prov.disable(&f.pool, "ada", &reaper).await.unwrap();
        assert!(report.account.is_disabled());
        assert_eq!(report.account.session_epoch, a.session_epoch + 1);
        assert_eq!(report.reap, ReapOutcome::Killed);
        assert_eq!(reaper.0.load(Ordering::SeqCst), 1);
        assert!(f
            .etc
            .line("passwd", "ik-ada")
            .unwrap()
            .ends_with(":/usr/sbin/nologin"));
        assert!(f
            .etc
            .line("shadow", "ik-ada")
            .unwrap()
            .starts_with("ik-ada:!"));
        // Files are kept.
        assert!(f.prov.root().principal_home(a.principal_id).exists());

        let e = f.prov.enable(&f.pool, "ada").await.unwrap();
        assert!(!e.is_disabled());
        assert_eq!(
            e.session_epoch, report.account.session_epoch,
            "enable revives nothing"
        );
        assert!(f
            .etc
            .line("passwd", "ik-ada")
            .unwrap()
            .ends_with(":/bin/sh"));
        let kinds: Vec<String> = events(&f.pool).await.into_iter().map(|e| e.0).collect();
        assert_eq!(
            kinds,
            ["account_created", "account_disabled", "account_enabled"]
        );
    }

    #[tokio::test]
    async fn no_reaper_reports_unavailable() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let report = f.prov.disable(&f.pool, "ada", &NoReaper).await.unwrap();
        assert!(matches!(report.reap, ReapOutcome::Unavailable(_)));
    }

    struct BrokenReaper;
    impl UidReaper for BrokenReaper {
        fn kill_all(&self, _p: &Principal) -> anyhow::Result<ReapOutcome> {
            anyhow::bail!("helper could not run")
        }
    }

    /// A failed kill is reported, not turned into a failed disable: the
    /// account is disabled either way (the disable committed first).
    #[tokio::test]
    async fn a_failed_reap_still_disables() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let report = f.prov.disable(&f.pool, "ada", &BrokenReaper).await.unwrap();
        assert!(report.account.is_disabled());
        assert!(
            matches!(&report.reap, ReapOutcome::Failed(why) if why.contains("helper")),
            "{:?}",
            report.reap
        );
    }

    // ── §8 step 7: reconcile ────────────────────────────────────────────────

    /// A redeploy reset `/etc`: reconcile recreates the entries of active
    /// allocated accounts from `accounts.db`, and leaves disabled ones.
    #[tokio::test]
    async fn reconcile_recreates_missing_entries_of_active_accounts() {
        let f = fixture().await;
        let ada = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        f.prov
            .create(&f.pool, "bob", PW, false, None)
            .await
            .unwrap();
        f.prov.disable(&f.pool, "bob", &NoReaper).await.unwrap();
        // Nothing to do on a consistent host.
        let report = f.prov.reconcile(&f.pool).await.unwrap();
        assert_eq!(
            report,
            ReconcileReport {
                checked: 1,
                ..Default::default()
            }
        );

        f.etc.remove_user("ik-ada").unwrap();
        f.etc.remove_user("ik-bob").unwrap();
        let report = f.prov.reconcile(&f.pool).await.unwrap();
        assert_eq!(report.created, vec!["ik-ada".to_string()]);
        let pw = f.etc.passwd_by_name("ik-ada").unwrap().unwrap();
        assert_eq!((pw.uid, pw.gid), (ada.unix_uid, ada.unix_gid));
        assert_eq!(pw.home, ada.home);
        assert_eq!(pw.shell, ada.shell);
        assert!(f.etc.line("group", "ik-ada").is_some());
        assert!(
            f.etc.passwd_by_name("ik-bob").unwrap().is_none(),
            "a disabled account is not re-projected"
        );
        // Idempotent.
        assert!(f.prov.reconcile(&f.pool).await.unwrap().created.is_empty());
    }

    #[tokio::test]
    async fn reconcile_restores_a_drifted_shell() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        f.etc.set_shell("ik-ada", Path::new("/bin/bash")).unwrap();
        let report = f.prov.reconcile(&f.pool).await.unwrap();
        assert_eq!(report.repaired_shell, vec!["ik-ada".to_string()]);
        assert!(f
            .etc
            .line("passwd", "ik-ada")
            .unwrap()
            .ends_with(":/bin/sh"));
    }

    /// Refuse, never rewrite: the row's uid held by another host user, or
    /// its entry pointing elsewhere.
    #[tokio::test]
    async fn reconcile_refuses_identity_drift() {
        let f = fixture().await;
        let ada = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        f.etc.remove_user("ik-ada").unwrap();
        f.etc
            .add_user(&NewUser {
                name: "intruder",
                uid: ada.unix_uid,
                gid: ada.unix_gid,
                gecos: "",
                home: Path::new("/home/intruder"),
                shell: Path::new("/bin/sh"),
            })
            .unwrap();
        let err = f.prov.reconcile(&f.pool).await.unwrap_err();
        assert!(
            matches!(&err, ProvisionError::Drift(why) if why.contains("intruder")),
            "{err}"
        );
        assert!(
            f.etc.passwd_by_name("ik-ada").unwrap().is_none(),
            "nothing written"
        );

        f.etc.remove_user("intruder").unwrap();
        f.etc
            .add_user(&NewUser {
                name: "ik-ada",
                uid: ada.unix_uid,
                gid: ada.unix_gid,
                gecos: "",
                home: Path::new("/elsewhere"),
                shell: Path::new("/bin/sh"),
            })
            .unwrap();
        let err = f.prov.reconcile(&f.pool).await.unwrap_err();
        assert!(
            matches!(&err, ProvisionError::Drift(why) if why.contains("home")),
            "{err}"
        );
    }

    /// `--provisioning external` never writes `/etc`, so a missing entry is
    /// the operator's to recreate.
    #[tokio::test]
    async fn reconcile_under_external_refuses_a_missing_entry() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        f.etc.remove_user("ik-ada").unwrap();
        // The same store, now administered with `--provisioning external`
        // (its lookups fall back to NSS, which has no `ik-ada`).
        let ext = Provisioner::external_for_tests(f.prov.root().clone(), RANGE);
        let err = ext.reconcile(&f.pool).await.unwrap_err();
        assert!(
            matches!(&err, ProvisionError::Drift(why) if why.contains("external")),
            "{err}"
        );
    }

    struct FailingHook;
    impl SessionsRevokedHook for FailingHook {
        fn on_sessions_revoked<'a, 'c>(
            &'a self,
            tx: &'a mut Transaction<'c, Sqlite>,
            _id: PrincipalId,
        ) -> accounts::HookFuture<'a>
        where
            'c: 'a,
        {
            Box::pin(async move {
                // Proves the hook runs inside the transaction: this write is
                // rolled back with everything else.
                sqlx::query("INSERT INTO auth_events (at, kind) VALUES (0, 'logout')")
                    .execute(&mut **tx)
                    .await?;
                anyhow::bail!("device store unavailable")
            })
        }
    }

    #[tokio::test]
    async fn forced_logout_runs_the_r11_hook_in_its_transaction() {
        let f = fixture().await;
        let a = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let b = f
            .prov
            .revoke_sessions(&f.pool, "ada", &NoDeviceGrants)
            .await
            .unwrap();
        assert_eq!(b.session_epoch, a.session_epoch + 1);
        assert_eq!(events(&f.pool).await.last().unwrap().0, "sessions_revoked");

        let before = events(&f.pool).await.len();
        assert!(f
            .prov
            .revoke_sessions(&f.pool, "ada", &FailingHook)
            .await
            .is_err());
        assert_eq!(
            events(&f.pool).await.len(),
            before,
            "hook failure rolls back"
        );
        let mut conn = f.pool.acquire().await.unwrap();
        let c = accounts::by_username(&mut conn, "ada")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(c.session_epoch, b.session_epoch);
    }

    #[tokio::test]
    async fn bootstrap_is_honoured_only_on_an_empty_table() {
        let f = fixture().await;
        let boot = BootstrapAdmin {
            username: "root-admin".into(),
            password: Zeroizing::new(PW.into()),
        };
        let BootstrapOutcome::Created(a) = f.prov.bootstrap_admin(&f.pool, &boot).await.unwrap()
        else {
            panic!("an empty table must honour the bootstrap");
        };
        assert!(a.is_admin);
        let again = BootstrapAdmin {
            username: "second".into(),
            password: Zeroizing::new(PW.into()),
        };
        assert!(matches!(
            f.prov.bootstrap_admin(&f.pool, &again).await.unwrap(),
            BootstrapOutcome::IgnoredNotEmpty
        ));
        let mut conn = f.pool.acquire().await.unwrap();
        assert_eq!(accounts::count(&mut conn).await.unwrap(), 1);
        assert!(format!("{again:?}").contains("<redacted>"));
    }

    #[tokio::test]
    async fn external_mode_only_adopts() {
        let (_root_tmp, root) = test_support::temp_root();
        let prov = Provisioner::external_for_tests(root.clone(), RANGE);
        let pool = open_accounts(&root, Opener::Cli).await.unwrap();
        let err = prov
            .create(&pool, "ada", PW, false, None)
            .await
            .unwrap_err();
        assert!(matches!(err, ProvisionError::ExternalNeedsAdopt), "{err}");
        // Root can never be adopted (I-1).
        let root_name = sys::user_by_uid(0).unwrap().unwrap().name;
        let err = prov
            .create(&pool, "ada", PW, false, Some(Adopt::user(&root_name)))
            .await
            .unwrap_err();
        assert!(matches!(err, ProvisionError::AdoptRefused(_)), "{err}");
        let err = prov
            .create(
                &pool,
                "ada",
                PW,
                false,
                Some(Adopt::user("ik-no-such-user-wp20")),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ProvisionError::AdoptRefused(_)), "{err}");
        assert_eq!(events(&pool).await.len(), 3, "every failure is recorded");
        // Review F4: `nobody` (65534) is never adoptable, whatever the flag.
        if let Some(nobody) = sys::user_by_uid(OVERFLOW_ID).unwrap() {
            let err = prov
                .create(
                    &pool,
                    "nob",
                    PW,
                    false,
                    Some(Adopt {
                        unix_user: &nobody.name,
                        allow_system_user: true,
                    }),
                )
                .await
                .unwrap_err();
            assert!(err.to_string().contains("overflow id"), "{err}");
        }
    }

    fn passwd(name: &str, uid: u32, gid: u32) -> sys::PasswdInfo {
        sys::PasswdInfo {
            name: name.into(),
            uid,
            gid,
            home: PathBuf::from("/home/x"),
            shell: PathBuf::from("/bin/sh"),
        }
    }

    #[test]
    fn adopt_refuses_root_probe_overflow_sentinels_and_system_users() {
        let range = UidRange::DEFAULT;
        let refused = |pw: &sys::PasswdInfo, allow: bool| adopt_refusal(pw, range, 1000, allow);
        assert!(refused(&passwd("root", 0, 0), true).is_some());
        assert!(refused(&passwd("g0", 1500, 0), true).is_some());
        assert!(refused(&passwd("probe", 29_999, 1500), true).is_some());
        assert!(refused(&passwd("nobody", 65_534, 65_534), true).is_some());
        assert!(refused(&passwd("nogroup", 1500, 65_534), true).is_some());
        assert!(refused(&passwd("m1", u32::MAX, 1500), true).is_some());
        assert!(refused(&passwd("m2", 1500, u32::MAX - 1), true).is_some());
        // System users only with the explicit flag.
        let daemon = passwd("daemon", 1, 1);
        assert!(refused(&daemon, false)
            .unwrap()
            .contains("--allow-system-user"));
        assert!(refused(&daemon, true).is_none());
        assert!(refused(&passwd("www-data", 33, 33), false).is_some());
        // An ordinary login user, even with a low shared primary group.
        assert!(refused(&passwd("ada", 1000, 100), false).is_none());
    }

    #[test]
    fn uid_min_is_read_from_login_defs() {
        let defs = "# comment UID_MIN 5\nUID_MAX\t\t60000\nUID_MIN\t\t\t 1500\nSYS_UID_MIN 100\n";
        assert_eq!(parse_uid_min(defs), Some(1500));
        assert_eq!(parse_uid_min("UID_MIN nope\n"), None);
        assert_eq!(parse_uid_min(""), None);
    }

    /// Review F11: the first provisioning pins --uid-range in the store; a
    /// different one is refused before anything is allocated.
    #[tokio::test]
    async fn the_uid_range_is_pinned_by_the_first_create() {
        let f = fixture().await;
        f.prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        let (etc_tmp, etc) = fake_etc(true);
        let other = Provisioner::for_tests(
            f.prov.root().clone(),
            UidRange::new(3_900_000_200, 3_900_000_300).unwrap(),
            etc_tmp.path(),
        );
        let err = other
            .create(&f.pool, "bob", PW, false, None)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ProvisionError::UidRangeMismatch { stored, .. }
                if *stored == RANGE.to_string()),
            "{err}"
        );
        assert!(etc.line("passwd", "ik-bob").is_none());
        assert_eq!(events(&f.pool).await.last().unwrap().0, "provision_failed");
        // The original range still works.
        f.prov
            .create(&f.pool, "bob", PW, false, None)
            .await
            .unwrap();
    }

    /// Review F6: the subtree is never handed over parent-first.
    #[tokio::test]
    async fn principal_dirs_are_all_0700() {
        let f = fixture().await;
        let a = f
            .prov
            .create(&f.pool, "ada", PW, false, None)
            .await
            .unwrap();
        for dir in [
            f.prov.root().principal_dir(a.principal_id),
            f.prov.root().principal_home(a.principal_id),
            f.prov.root().principal_data(a.principal_id).join("tmp"),
        ] {
            assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o7777, 0o700);
        }
    }

    /// Over a map, never the process env: `setenv` racing the other tests'
    /// `getenv` is UB in glibc (review F16).
    #[test]
    fn bootstrap_env_is_captured_and_always_removed() {
        use std::collections::HashMap;
        use std::ffi::OsString;
        let mut env: HashMap<String, OsString> = HashMap::from([
            (BOOTSTRAP_ADMIN_ENV.to_string(), "ada".into()),
            (BOOTSTRAP_ADMIN_PASSWORD_ENV.to_string(), PW.into()),
            ("OTHER".to_string(), "kept".into()),
        ]);
        let b = BootstrapAdmin::take_from(&mut |k| env.remove(k))
            .unwrap()
            .unwrap();
        assert_eq!(b.username, "ada");
        assert_eq!(b.password.as_str(), PW);
        assert_eq!(env.keys().collect::<Vec<_>>(), ["OTHER"]);

        env.insert(BOOTSTRAP_ADMIN_PASSWORD_ENV.to_string(), PW.into());
        assert!(BootstrapAdmin::take_from(&mut |k| env.remove(k)).is_err());
        assert!(!env.contains_key(BOOTSTRAP_ADMIN_PASSWORD_ENV));
        assert!(BootstrapAdmin::take_from(&mut |k| env.remove(k))
            .unwrap()
            .is_none());
    }

    /// Real host provisioning. These need root (euid 0 with CAP_CHOWN /
    /// DAC_OVERRIDE / FOWNER) and write the real `/etc`, so they are
    /// `#[ignore]`d and run by the `t1-root` CI job (and locally as root):
    /// `cargo test --lib -- --ignored --test-threads=1 t1_root`.
    mod t1_root {
        use super::*;
        use crate::server::operator::open_accounts;

        const RANGE: UidRange = UidRange {
            start: 28_000,
            end: 28_010,
        };

        /// Removes a host user this test created, whatever the outcome.
        struct HostUser(&'static str);
        impl Drop for HostUser {
            fn drop(&mut self) {
                if let Some(t) = ShadowTools::find() {
                    let _ = t.run(&t.userdel, &[self.0.as_ref()]);
                    let _ = t.run(&t.groupdel, &[self.0.as_ref()]);
                }
                let _ = EtcFiles::system().remove_user(self.0);
            }
        }

        fn require_root() {
            assert_eq!(sys::geteuid(), 0, "t1-root tests must run as root");
        }

        fn enforced_root() -> (tempfile::TempDir, OperatorRoot) {
            let tmp = tempfile::tempdir().unwrap();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            (tmp, root)
        }

        fn assert_owned(path: &Path, uid: u32, gid: u32, mode: u32) {
            let meta = fs::symlink_metadata(path).unwrap();
            assert_eq!(
                (meta.uid(), meta.gid(), meta.mode() & 0o7777),
                (uid, gid, mode),
                "{}",
                path.display()
            );
        }

        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_create_provisions_and_chowns_the_section_4_layout() {
            require_root();
            let _cleanup = HostUser("ik-t1root-ada");
            let (_tmp, root) = enforced_root();
            let prov = Provisioner::new(root.clone(), RANGE, ProvisioningMode::Auto, Actor::Cli);
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let a = prov
                .create(&pool, "t1root-ada", PW, true, None)
                .await
                .unwrap_or_else(|e| panic!("[{}] {e}", prov.backend_name()));
            assert_eq!(a.unix_uid, RANGE.start);

            // NSS resolves the new identity (§4: a real passwd entry).
            let pw = sys::user_by_name("ik-t1root-ada")
                .unwrap()
                .expect("passwd entry");
            assert_eq!((pw.uid, pw.gid), (a.unix_uid, a.unix_gid));
            assert_eq!(pw.home, a.home);
            assert_eq!(pw.shell, Path::new(DEFAULT_SHELL));
            assert!(sys::group_gid_exists(a.unix_gid).unwrap());
            // Review F5: no subordinate id range for a principal.
            for file in ["/etc/subuid", "/etc/subgid"] {
                if let Ok(text) = fs::read_to_string(file) {
                    assert!(
                        !text.lines().any(|l| l.starts_with("ik-t1root-ada:")),
                        "{file}: {text}"
                    );
                }
            }

            // §4 owners and modes; I-9 for the principal subtree.
            assert_owned(root.root(), 0, 0, 0o755);
            assert_owned(&root.operator_dir(), 0, 0, 0o700);
            assert_owned(&root.principals_dir(), 0, 0, 0o711);
            assert_owned(&root.accounts_db(), 0, 0, 0o600);
            for dir in [
                root.principal_dir(a.principal_id),
                root.principal_home(a.principal_id),
                root.principal_data(a.principal_id),
                root.principal_data(a.principal_id).join("tmp"),
            ] {
                assert_owned(&dir, a.unix_uid, a.unix_gid, 0o700);
            }

            // §7.3 disable locks the entry; enable restores the shell.
            prov.disable(&pool, "t1root-ada", &NoReaper).await.unwrap();
            let locked = sys::user_by_name("ik-t1root-ada").unwrap().unwrap();
            assert!(
                locked.shell.ends_with("nologin"),
                "{}",
                locked.shell.display()
            );
            prov.enable(&pool, "t1root-ada").await.unwrap();
            let unlocked = sys::user_by_name("ik-t1root-ada").unwrap().unwrap();
            assert_eq!(unlocked.shell, Path::new(DEFAULT_SHELL));
        }

        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_a_dropped_guard_removes_the_real_user_and_dirs() {
            require_root();
            let _cleanup = HostUser("ik-t1root-bob");
            let (_tmp, root) = enforced_root();
            let prov = Provisioner::new(
                root.clone(),
                UidRange::new(28_020, 28_030).unwrap(),
                ProvisioningMode::Auto,
                Actor::Cli,
            );
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
            let guard = prov
                .create_in(&mut tx, "t1root-bob", PW, false)
                .await
                .unwrap();
            let id = guard.account().principal_id;
            assert!(sys::user_by_name("ik-t1root-bob").unwrap().is_some());
            drop(guard);
            drop(tx);
            assert!(sys::user_by_name("ik-t1root-bob").unwrap().is_none());
            assert!(!sys::group_name_exists("ik-t1root-bob").unwrap());
            assert!(!root.principal_dir(id).exists());
        }

        /// The built-in writer against the real `/etc`, under `lckpwdf`.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_builtin_writer_on_the_real_etc() {
            require_root();
            let _cleanup = HostUser("ik-t1root-cy");
            let etc = EtcFiles::system();
            let before = fs::read_to_string("/etc/passwd").unwrap();
            let mode = fs::metadata("/etc/passwd").unwrap().mode() & 0o7777;
            etc.add_user(&NewUser {
                name: "ik-t1root-cy",
                uid: 28_040,
                gid: 28_040,
                gecos: GECOS,
                home: Path::new("/nonexistent/t1root-cy"),
                shell: Path::new("/bin/sh"),
            })
            .unwrap();
            let pw = sys::user_by_name("ik-t1root-cy")
                .unwrap()
                .expect("NSS sees the new entry");
            assert_eq!((pw.uid, pw.gid), (28_040, 28_040));
            assert!(sys::group_gid_exists(28_040).unwrap());
            assert_eq!(fs::metadata("/etc/passwd").unwrap().mode() & 0o7777, mode);
            etc.remove_user("ik-t1root-cy").unwrap();
            assert!(sys::user_by_name("ik-t1root-cy").unwrap().is_none());
            assert_eq!(fs::read_to_string("/etc/passwd").unwrap(), before);
        }

        /// §8 step 5 under real ownership: a non-root-owned operator dir is
        /// refused.
        #[test]
        #[ignore = "t1-root"]
        fn t1_root_operator_dirs_must_be_root_owned() {
            require_root();
            let (_tmp, root) = enforced_root();
            std::os::unix::fs::chown(root.operator_dir(), Some(28_050), Some(28_050)).unwrap();
            let err = root.prepare(Ownership::Enforce).unwrap_err();
            assert!(err.to_string().contains("not root:root"), "{err}");
        }
    }
}
