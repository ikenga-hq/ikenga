//! Host health for the admin "Server" card (`server_health`, plans: the
//! Lagos-latency / small-box report of 2026-10-08).
//!
//! One bounded, cached snapshot of the machine the daemon runs on: CPU count,
//! load, memory and swap, Linux pressure-stall numbers (PSI), the data
//! directory's filesystem, uptime, the backup job's `status.json`, the state
//! of the backup timers and the database tunnels, and (T1 only) how many
//! terminals and `claude` processes each account has running.
//!
//! * **Who may ask** ([`authorize`]): under T1 an enabled admin whose
//!   credential has admin strength; under T0 the owner (admin strength: the
//!   host bearer, a password session or a `full` device). Never a share.
//! * **Where it runs**: the T0 daemon, or the T1 *broker* (the arm is
//!   `access` class and `access::is_broker_arm` keeps it off the principals'
//!   children). The broker is the one process that sees the whole box; a
//!   child sees only its own uid's processes (`ProtectProc=invisible` hides
//!   the rest from the broker), so the per-account figures are *asked of each
//!   running child* ([`AccountLoads`], installed by the broker) rather than
//!   counted from `/proc`.
//! * **Cost**: [`snapshot`] takes at most one measurement per
//!   [`CACHE_TTL`] however many admins poll, runs the blocking reads and the
//!   two `systemctl` calls on the blocking pool, and gives up after
//!   [`BUDGET`] (serving the last good snapshot, marked `stale`, when it has
//!   one). Nothing here ever waits on a person's own child that is starting.
//! * **What is exposed**: numbers, unit names, database names and the backup
//!   job's documented status fields. No command lines, paths, bucket URLs,
//!   environment or another account's files. Every field is optional: a
//!   source that cannot be read (non-Linux, no systemd, no PSI, no backups
//!   provisioned) is absent and named in `unavailable`.
//!
//! The parsers are pure functions over text, tested against fixtures.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::access::rpc::Env;
use crate::access::store::StoreTier;
use crate::access::{AccessCtx, AccessError, Code};

/// The shape version of the snapshot; the card refuses a newer one.
pub const SCHEMA: u32 = 1;
/// One measurement is shared by every caller for this long.
pub const CACHE_TTL: Duration = Duration::from_secs(5);
/// The whole measurement, children and `systemctl` included.
pub const BUDGET: Duration = Duration::from_secs(4);
/// One `systemctl` call.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(2);
/// `systemctl`'s output is a few lines per unit; anything bigger is not.
const MAX_CMD_OUTPUT: usize = 256 * 1024;
/// The backup job's status file is a few KiB.
const MAX_STATUS_FILE: u64 = 256 * 1024;
const MAX_UNITS: usize = 32;
const MAX_DATABASES: usize = 64;
/// `provision.sh backups` writes here (README "status.json"); the symlink to
/// `status/status.json` is root-made, so this path is the documented one.
pub const DEFAULT_BACKUP_STATUS: &str = "/var/lib/ikenga-backup/status.json";
/// Overrides [`DEFAULT_BACKUP_STATUS`] (container tests).
pub const BACKUP_STATUS_ENV: &str = "IKENGA_BACKUP_STATUS_FILE";

// ─── the snapshot ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Load {
    pub m1: f64,
    pub m5: f64,
    pub m15: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Swap {
    pub total_bytes: u64,
    pub used_bytes: u64,
}

/// One resource's pressure-stall averages, in percent of the window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Psi {
    pub some_avg10: f64,
    pub some_avg60: f64,
    /// Absent where the kernel reports no `full` line (CPU, older kernels).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_avg10: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_avg60: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Pressure {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Psi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<Psi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io: Option<Psi>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Disk {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// One database in the backup job's status file (README `status.json`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackupDb {
    pub name: String,
    pub schedule: Option<String>,
    pub last_attempt: Option<String>,
    pub last_attempt_ok: Option<bool>,
    pub last_success: Option<String>,
    /// A short token (`gcs-auth`, `dump-failed`, …), `null` after a success.
    pub last_error_kind: Option<String>,
    pub last_error_at: Option<String>,
    pub bytes: Option<u64>,
    pub duration_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackupSchedule {
    pub name: String,
    pub last_run: Option<String>,
    pub last_run_ok: Option<bool>,
    /// Database names that failed in the last run.
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Backups {
    pub enabled: bool,
    pub updated: Option<String>,
    pub databases: Vec<BackupDb>,
    pub schedules: Vec<BackupSchedule>,
}

/// A systemd unit the card cares about.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitState {
    pub name: String,
    /// `timer` (an `ikenga-backup-*.timer`), `tunnel` (a `*-tunnel.service`)
    /// or `backup_run` (a *failed* `ikenga-backup@*.service` instance).
    pub kind: &'static str,
    pub active_state: String,
    pub sub_state: Option<String>,
    /// A service's last result (`success`, `exit-code`, `timeout`, …).
    pub result: Option<String>,
    /// Timers: when the timer last fired / will next fire, Unix seconds.
    pub last_trigger: Option<i64>,
    pub next_elapse: Option<i64>,
}

impl UnitState {
    /// Whether systemd considers this unit failed.
    pub fn failed(&self) -> bool {
        self.active_state == "failed"
            || self
                .result
                .as_deref()
                .is_some_and(|r| !matches!(r, "success" | "" | "n/a"))
    }
}

/// One account's footprint, as its own running child reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountLoad {
    pub username: String,
    /// Whether the account's child is up at all (an idle account has none).
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminals: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_processes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostHealth {
    pub schema: u32,
    pub taken_at_ms: u64,
    pub version: &'static str,
    pub tier: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<Load>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<Memory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap: Option<Swap>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pressure: Option<Pressure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<Disk>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backups: Option<Backups>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<UnitState>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<AccountLoad>>,
    /// The sections this server could not measure (absent above).
    pub unavailable: Vec<&'static str>,
}

// ─── pure parsers ───────────────────────────────────────────────────────────

/// `/proc/loadavg`: `0.00 0.01 0.05 1/123 4567`.
pub fn parse_loadavg(text: &str) -> Option<Load> {
    let mut it = text.split_whitespace();
    let m1 = it.next()?.parse().ok()?;
    let m5 = it.next()?.parse().ok()?;
    let m15 = it.next()?.parse().ok()?;
    Some(Load { m1, m5, m15 })
}

/// `/proc/meminfo` (kB lines). Memory needs `MemTotal` and `MemAvailable`
/// (kernels before 3.14 have no estimate: absent rather than guessed); swap
/// needs `SwapTotal` and `SwapFree`. A swapless box is `Swap{0, 0}`, which
/// is a fact, not a gap.
pub fn parse_meminfo(text: &str) -> (Option<Memory>, Option<Swap>) {
    let mut kb: BTreeMap<&str, u64> = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse().ok()) else {
            continue;
        };
        kb.insert(key.trim(), n);
    }
    let bytes = |k: &str| kb.get(k).map(|n| n.saturating_mul(1024));
    let memory = match (bytes("MemTotal"), bytes("MemAvailable")) {
        (Some(total), Some(avail)) if total > 0 => Some(Memory {
            total_bytes: total,
            available_bytes: avail.min(total),
        }),
        _ => None,
    };
    let swap = match (bytes("SwapTotal"), bytes("SwapFree")) {
        (Some(total), Some(free)) => Some(Swap {
            total_bytes: total,
            used_bytes: total.saturating_sub(free),
        }),
        _ => None,
    };
    (memory, swap)
}

/// One `/proc/pressure/*` file:
/// `some avg10=0.95 avg60=2.54 avg300=3.68 total=25563918341`.
pub fn parse_pressure(text: &str) -> Option<Psi> {
    fn avgs(line: &str) -> Option<(f64, f64)> {
        let mut a10 = None;
        let mut a60 = None;
        for field in line.split_whitespace() {
            if let Some(v) = field.strip_prefix("avg10=") {
                a10 = v.parse().ok();
            } else if let Some(v) = field.strip_prefix("avg60=") {
                a60 = v.parse().ok();
            }
        }
        Some((a10?, a60?))
    }
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        match line.split_whitespace().next() {
            Some("some") => some = avgs(line),
            Some("full") => full = avgs(line),
            _ => {}
        }
    }
    let (some_avg10, some_avg60) = some?;
    Some(Psi {
        some_avg10,
        some_avg60,
        full_avg10: full.map(|f| f.0),
        full_avg60: full.map(|f| f.1),
    })
}

/// `/proc/uptime`: `4019275.82 14806495.18`.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

/// A name the backup job writes into its status file: a database or a
/// schedule. Anything else is dropped rather than shown.
fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// An error kind is one of a fixed set of short tokens (README); an unknown
/// or odd value is shown as `unknown`, never verbatim.
fn error_kind(s: &str) -> String {
    if !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
        s.to_string()
    } else {
        "unknown".to_string()
    }
}

/// An ISO-8601 instant as the job writes it (`2026-10-08T00:00:12Z`).
fn stamp(v: &Option<String>) -> Option<String> {
    v.as_ref()
        .filter(|s| {
            s.len() <= 40
                && s.bytes().all(|b| {
                    b.is_ascii_digit() || matches!(b, b'-' | b':' | b'T' | b'Z' | b'.' | b'+')
                })
        })
        .cloned()
}

#[derive(Deserialize)]
struct RawStatus {
    schema: Option<u32>,
    enabled: Option<bool>,
    updated: Option<String>,
    #[serde(default)]
    databases: BTreeMap<String, RawDb>,
    #[serde(default)]
    schedules: BTreeMap<String, RawSchedule>,
}

#[derive(Deserialize)]
struct RawDb {
    schedule: Option<String>,
    last_attempt: Option<String>,
    last_attempt_ok: Option<bool>,
    last_success: Option<String>,
    last_error_kind: Option<String>,
    last_error_at: Option<String>,
    bytes: Option<u64>,
    duration_s: Option<u64>,
}

#[derive(Deserialize)]
struct RawSchedule {
    last_run: Option<String>,
    last_run_ok: Option<bool>,
    #[serde(default)]
    failed: Vec<String>,
}

/// The backup job's `status.json` (`schema: 1`). Only the documented fields
/// are read (`object`, the destination URL, is deliberately never copied);
/// a different schema is `None`, not a guess.
pub fn parse_backup_status(text: &str) -> Option<Backups> {
    let raw: RawStatus = serde_json::from_str(text).ok()?;
    if raw.schema != Some(1) {
        return None;
    }
    let databases = raw
        .databases
        .into_iter()
        .filter(|(name, _)| name_ok(name))
        .take(MAX_DATABASES)
        .map(|(name, d)| BackupDb {
            name,
            schedule: d.schedule.filter(|s| name_ok(s)),
            last_attempt: stamp(&d.last_attempt),
            last_attempt_ok: d.last_attempt_ok,
            last_success: stamp(&d.last_success),
            last_error_kind: d.last_error_kind.as_deref().map(error_kind),
            last_error_at: stamp(&d.last_error_at),
            bytes: d.bytes,
            duration_s: d.duration_s,
        })
        .collect();
    let schedules = raw
        .schedules
        .into_iter()
        .filter(|(name, _)| name_ok(name))
        .take(MAX_DATABASES)
        .map(|(name, s)| BackupSchedule {
            name,
            last_run: stamp(&s.last_run),
            last_run_ok: s.last_run_ok,
            failed: s
                .failed
                .into_iter()
                .filter(|n| name_ok(n))
                .take(MAX_DATABASES)
                .collect(),
        })
        .collect();
    Some(Backups {
        enabled: raw.enabled.unwrap_or(true),
        updated: stamp(&raw.updated),
        databases,
        schedules,
    })
}

/// A unit name this module is willing to hand to `systemctl show`: a plain
/// systemd name of one of the three shapes it looks for.
pub fn unit_name_ok(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'@' | b':'))
    {
        return false;
    }
    unit_kind(name).is_some()
}

fn unit_kind(name: &str) -> Option<&'static str> {
    if name.starts_with("ikenga-backup-") && name.ends_with(".timer") {
        Some("timer")
    } else if name.ends_with("-tunnel.service") && !name.starts_with('-') {
        Some("tunnel")
    } else if name.starts_with("ikenga-backup@") && name.ends_with(".service") {
        Some("backup_run")
    } else {
        None
    }
}

/// `systemctl list-unit-files|list-units --no-legend --plain`: the first
/// column of each line, kept when it is a unit this module looks for.
pub fn parse_unit_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        // `list-units` may prefix a failed unit with a bullet.
        let Some(name) = line
            .split_whitespace()
            .find(|t| !matches!(*t, "●" | "*" | "○" | "×"))
        else {
            continue;
        };
        if unit_name_ok(name) && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
        if out.len() >= MAX_UNITS {
            break;
        }
    }
    out
}

/// `Thu 2026-10-08 18:39:49 UTC` (systemctl run with `TZ=UTC LC_ALL=C`) →
/// Unix seconds. `n/a`, empty and anything else is `None`.
pub fn parse_unit_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let rest = s.strip_suffix(" UTC")?;
    // Drop the weekday.
    let (_, date_time) = rest.split_once(' ')?;
    let dt = chrono::NaiveDateTime::parse_from_str(date_time, "%Y-%m-%d %H:%M:%S").ok()?;
    Some(dt.and_utc().timestamp())
}

/// `systemctl show -p Id,ActiveState,… a b c`: blocks of `Key=Value` lines
/// separated by a blank line, one block per unit. Units whose `LoadState`
/// is `not-found` are dropped.
pub fn parse_systemctl_show(text: &str) -> Vec<UnitState> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut props: BTreeMap<&str, &str> = BTreeMap::new();
        for line in block.lines() {
            if let Some((k, v)) = line.split_once('=') {
                props.insert(k.trim(), v.trim());
            }
        }
        let Some(&id) = props.get("Id") else { continue };
        let Some(kind) = unit_kind(id).filter(|_| unit_name_ok(id)) else {
            continue;
        };
        if props.get("LoadState") == Some(&"not-found") {
            continue;
        }
        let Some(&active) = props.get("ActiveState") else {
            continue;
        };
        let opt = |k: &str| {
            props
                .get(k)
                .copied()
                .filter(|v| !v.is_empty() && *v != "n/a")
                .map(str::to_string)
        };
        let timer = kind == "timer";
        out.push(UnitState {
            name: id.to_string(),
            kind,
            active_state: active.to_string(),
            sub_state: opt("SubState"),
            result: if timer { None } else { opt("Result") },
            last_trigger: timer
                .then(|| {
                    props
                        .get("LastTriggerUSec")
                        .and_then(|v| parse_unit_timestamp(v))
                })
                .flatten(),
            next_elapse: timer
                .then(|| {
                    props
                        .get("NextElapseUSecRealtime")
                        .and_then(|v| parse_unit_timestamp(v))
                })
                .flatten(),
        });
    }
    out
}

// ─── who may ask ────────────────────────────────────────────────────────────

/// Under T1 an enabled admin with an admin-strength credential; under T0 the
/// owner (admin strength). A share (a member inside the owner's project) is
/// never enough, nor is a relayed child call.
pub fn authorize(
    tier: StoreTier,
    principal_is_admin: bool,
    ctx: &AccessCtx,
) -> Result<(), AccessError> {
    if ctx.share.is_some() || ctx.share_headers {
        return Err(AccessError::new(
            Code::Forbidden,
            "server health is for administrators",
        ));
    }
    match tier {
        StoreTier::T1 if !principal_is_admin => Err(AccessError::new(
            Code::Forbidden,
            "server health is for administrators",
        )),
        _ if !ctx.admin_strength => Err(AccessError::forbidden_admin()),
        _ => Ok(()),
    }
}

// ─── installed by the process that serves the arm ───────────────────────────

/// Per-account figures, asked of each running child (T1 broker only).
pub trait AccountLoads: Send + Sync {
    fn loads(&self) -> BoxFuture<'static, Option<Vec<AccountLoad>>>;
}

static ACCOUNTS: Mutex<Option<Arc<dyn AccountLoads>>> = Mutex::new(None);
static DATA_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The T1 broker's handle on its children. Once per process.
pub fn install_account_loads(p: Arc<dyn AccountLoads>) {
    *ACCOUNTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
}

/// The directory whose filesystem the disk figures describe: `--data-dir`
/// (the operator root under T1). Without one, the working directory's.
pub fn set_data_dir(dir: PathBuf) {
    *DATA_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir);
}

fn data_dir() -> PathBuf {
    DATA_DIR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
}

fn backup_status_path() -> PathBuf {
    std::env::var_os(BACKUP_STATUS_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BACKUP_STATUS))
}

// ─── readers (Linux) ────────────────────────────────────────────────────────

/// Total / available bytes of the filesystem holding `dir`.
#[cfg(unix)]
pub fn disk_of(dir: &Path) -> Option<Disk> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` only writes into the zeroed struct we hand it.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = if st.f_frsize > 0 {
        st.f_frsize
    } else {
        st.f_bsize
    } as u64;
    let total = (st.f_blocks as u64).checked_mul(frsize)?;
    let free = (st.f_bavail as u64).checked_mul(frsize)?;
    (total > 0).then_some(Disk {
        total_bytes: total,
        free_bytes: free.min(total),
    })
}

#[cfg(not(unix))]
pub fn disk_of(_dir: &Path) -> Option<Disk> {
    None
}

#[cfg(target_os = "linux")]
fn read_small(path: &str) -> Option<String> {
    use std::io::Read;
    let mut s = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(64 * 1024)
        .read_to_string(&mut s)
        .ok()?;
    Some(s)
}

/// The backup job's status file, when it exists and is a plain, small file.
fn read_backups(path: &Path) -> Option<Backups> {
    use std::io::Read;
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_STATUS_FILE {
        return None;
    }
    let mut s = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_STATUS_FILE)
        .read_to_string(&mut s)
        .ok()?;
    parse_backup_status(&s)
}

/// Run `bin args…` with a minimal environment (`TZ=UTC LC_ALL=C` so
/// timestamps parse), stdin closed, stdout capped, killed after `timeout`.
/// `None` on any failure; stderr is discarded.
#[cfg(target_os = "linux")]
fn run_capped(bin: &Path, args: &[String], timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(bin)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout)
            .take(MAX_CMD_OUTPUT as u64)
            .read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(_) => break None,
        }
    };
    let out = reader.join().ok()?;
    status.filter(|s| s.success())?;
    String::from_utf8(out).ok()
}

#[cfg(target_os = "linux")]
fn systemctl_bin() -> Option<PathBuf> {
    ["/usr/bin/systemctl", "/bin/systemctl"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// The backup timers, the database tunnels and any failed backup run.
#[cfg(target_os = "linux")]
fn read_units() -> Option<Vec<UnitState>> {
    let bin = systemctl_bin()?;
    let list = |sub: &[&str], patterns: &[&str]| {
        let mut args: Vec<String> = sub.iter().map(|s| s.to_string()).collect();
        args.extend(["--no-legend", "--no-pager", "--plain"].map(String::from));
        args.extend(patterns.iter().map(|s| s.to_string()));
        run_capped(&bin, &args, SYSTEMCTL_TIMEOUT)
    };
    // Installed timers and tunnels (a stopped one still shows), plus the
    // backup run instances systemd currently holds as failed.
    let files = list(
        &["list-unit-files"],
        &["ikenga-backup-*.timer", "*-tunnel.service"],
    )?;
    let failed = list(
        &["list-units", "--state=failed"],
        &["ikenga-backup@*.service"],
    )
    .unwrap_or_default();
    let mut names = parse_unit_list(&files);
    for n in parse_unit_list(&failed) {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.truncate(MAX_UNITS);
    if names.is_empty() {
        return Some(Vec::new());
    }
    let mut args: Vec<String> = [
        "show",
        "--no-pager",
        "-p",
        "Id,LoadState,ActiveState,SubState,Result,LastTriggerUSec,NextElapseUSecRealtime",
    ]
    .map(String::from)
    .to_vec();
    args.extend(names);
    let shown = run_capped(&bin, &args, SYSTEMCTL_TIMEOUT)?;
    let mut units = parse_systemctl_show(&shown);
    units.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    Some(units)
}

/// How many processes of *this* uid have `comm` == `name`. Counts only:
/// names and command lines are never read past `comm`, and other users'
/// processes are never looked at. A principal child answers for its own uid.
#[cfg(target_os = "linux")]
pub fn count_own_processes(name: &str) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let mut n = 0u32;
    for (seen, entry) in std::fs::read_dir("/proc").ok()?.flatten().enumerate() {
        if seen > 50_000 {
            break;
        }
        let file = entry.file_name();
        let Some(pid) = file
            .to_str()
            .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        if entry.metadata().map(|m| m.uid()).ok() != Some(me) {
            continue;
        }
        if read_small(&format!("/proc/{pid}/comm")).is_some_and(|c| c.trim() == name) {
            n += 1;
        }
    }
    Some(n)
}

#[cfg(not(target_os = "linux"))]
pub fn count_own_processes(_name: &str) -> Option<u32> {
    None
}

/// Everything that is a local read (no children): run on the blocking pool.
#[cfg(target_os = "linux")]
fn collect_local(data_dir: &Path, backups: &Path) -> Local {
    let (memory, swap) = read_small("/proc/meminfo")
        .map(|t| parse_meminfo(&t))
        .unwrap_or((None, None));
    let pressure = {
        let p = Pressure {
            cpu: read_small("/proc/pressure/cpu").and_then(|t| parse_pressure(&t)),
            memory: read_small("/proc/pressure/memory").and_then(|t| parse_pressure(&t)),
            io: read_small("/proc/pressure/io").and_then(|t| parse_pressure(&t)),
        };
        (p.cpu.is_some() || p.memory.is_some() || p.io.is_some()).then_some(p)
    };
    Local {
        load: read_small("/proc/loadavg").and_then(|t| parse_loadavg(&t)),
        memory,
        swap,
        pressure,
        uptime_secs: read_small("/proc/uptime").and_then(|t| parse_uptime(&t)),
        disk: disk_of(data_dir),
        backups: read_backups(backups),
        units: read_units(),
    }
}

#[cfg(not(target_os = "linux"))]
fn collect_local(data_dir: &Path, backups: &Path) -> Local {
    Local {
        disk: disk_of(data_dir),
        backups: read_backups(backups),
        ..Local::default()
    }
}

#[derive(Default)]
struct Local {
    load: Option<Load>,
    memory: Option<Memory>,
    swap: Option<Swap>,
    pressure: Option<Pressure>,
    uptime_secs: Option<u64>,
    disk: Option<Disk>,
    backups: Option<Backups>,
    units: Option<Vec<UnitState>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn assemble(tier: &'static str, local: Local, accounts: Option<Vec<AccountLoad>>) -> HostHealth {
    let mut unavailable = Vec::new();
    let mut note = |present: bool, name: &'static str| {
        if !present {
            unavailable.push(name);
        }
    };
    note(local.load.is_some(), "load");
    note(local.memory.is_some(), "memory");
    note(local.swap.is_some(), "swap");
    note(local.pressure.is_some(), "pressure");
    note(local.disk.is_some(), "disk");
    note(local.uptime_secs.is_some(), "uptime");
    note(local.backups.is_some(), "backups");
    note(local.units.is_some(), "units");
    note(accounts.is_some(), "accounts");
    HostHealth {
        schema: SCHEMA,
        taken_at_ms: now_ms(),
        version: env!("CARGO_PKG_VERSION"),
        tier,
        cpu_count: std::thread::available_parallelism()
            .ok()
            .map(|n| n.get() as u32),
        load: local.load,
        memory: local.memory,
        swap: local.swap,
        pressure: local.pressure,
        disk: local.disk,
        uptime_secs: local.uptime_secs,
        backups: local.backups,
        units: local.units,
        accounts,
        unavailable,
    }
}

/// One uncached measurement.
pub async fn measure(tier: StoreTier) -> HostHealth {
    let dir = data_dir();
    let backups = backup_status_path();
    let provider = ACCOUNTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let local = tokio::task::spawn_blocking(move || collect_local(&dir, &backups));
    let accounts = async {
        match provider {
            Some(p) => p.loads().await,
            None => None,
        }
    };
    let (local, accounts) = tokio::join!(local, accounts);
    assemble(tier.as_str(), local.unwrap_or_default(), accounts)
}

// ─── cache ──────────────────────────────────────────────────────────────────

/// Single-flight, time-boxed cache: concurrent callers share one
/// measurement; a measurement that overruns [`BUDGET`] serves the last good
/// value marked `"stale": true`, or fails when there is none.
pub struct Cached {
    slot: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

impl Cached {
    pub const fn new() -> Self {
        Self {
            slot: tokio::sync::Mutex::const_new(None),
        }
    }

    pub async fn get<F, Fut>(
        &self,
        ttl: Duration,
        budget: Duration,
        measure: F,
    ) -> Result<Value, AccessError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Value>,
    {
        let mut slot = self.slot.lock().await;
        if let Some((at, v)) = slot.as_ref() {
            if at.elapsed() < ttl {
                return Ok(v.clone());
            }
        }
        match tokio::time::timeout(budget, measure()).await {
            Ok(v) => {
                *slot = Some((Instant::now(), v.clone()));
                Ok(v)
            }
            Err(_) => match slot.as_ref() {
                Some((_, v)) => {
                    let mut v = v.clone();
                    if let Value::Object(m) = &mut v {
                        m.insert("stale".into(), Value::Bool(true));
                    }
                    Ok(v)
                }
                None => Err(AccessError::new(
                    Code::Internal,
                    "the server health probe timed out",
                )),
            },
        }
    }
}

impl Default for Cached {
    fn default() -> Self {
        Self::new()
    }
}

static CACHE: Cached = Cached::new();

/// The cached snapshot as JSON.
pub async fn snapshot(tier: StoreTier) -> Result<Value, AccessError> {
    CACHE
        .get(CACHE_TTL, BUDGET, || async move {
            serde_json::to_value(measure(tier).await).unwrap_or(Value::Null)
        })
        .await
}

/// The `server_health` arm (`access::rpc::dispatch`).
pub async fn dispatch(env: &Env<'_>, ctx: &AccessCtx) -> Result<Value, AccessError> {
    authorize(env.tier, env.principal.is_admin, ctx)?;
    snapshot(env.tier).await
}

#[cfg(test)]
mod tests;
