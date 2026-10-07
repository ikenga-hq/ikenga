//! In-app server updates (WP-P9 steps 2–3; workspace
//! `plans/remote-provisioning/01-plan.md`). Founder decision 2026-10-06:
//! **admins may update**, notify-only checking, never auto-apply.
//!
//! Root does every privileged step, through units that
//! `scripts/server/provision.sh install-update-units` installs:
//!
//! ```text
//!  ikenga-update-check.timer ─▶ provision check-update ─▶ /var/lib/ikenga-update/available.json
//!  admin ─▶ POST /api/server/update/apply ─▶ <request path>      (this module; the only write)
//!  ikenga-update.path (PathExists) ─▶ provision apply-request ─▶ /var/lib/ikenga-update/status.json
//! ```
//!
//! The server never upgrades itself: it reads root's two files, counts the
//! terminals a restart would end, and publishes one small request file. Root
//! treats that file as untrusted input — it can only say "apply the version
//! root itself advertised, now" — so everything decided here (who may ask,
//! the open-terminal acknowledgement) is about the UI, not root's safety.
//!
//! Who may ask:
//! - **T1** (the broker answers these routes itself; they are never
//!   proxied): an enabled admin, read fresh from `accounts.db` on every call,
//!   holding admin strength (a password session, or a device at the Full
//!   tier). Everyone else gets a bare 403 that carries no version.
//! - **T0**: the operator bearer only (`access::route_requirement` classes
//!   both routes `operator`, like `/api/shutdown`); a paired device of any
//!   tier gets 403.
//!
//! Every constant that names a path or a schema has a twin in
//! `provision.sh`; keep the two in step.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::AppState;
use crate::access::audit::Event;
use crate::access::{AccessCtx, DaemonAccess};

/// provision.sh `STATE_DIR`: root-owned, 0755. Read here, never written.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/ikenga-update";
/// provision.sh `IKENGA_UPDATE_STATE_DIR` (the container tests' override).
pub const STATE_DIR_ENV: &str = "IKENGA_UPDATE_STATE_DIR";
/// provision.sh `update_request_path`: `<data-dir>/update-request.json` on
/// T0, `<data-dir>/operator/update-request.json` on T1.
pub const REQUEST_FILE: &str = "update-request.json";
/// Written by `provision.sh check-update`.
pub const AVAILABLE_FILE: &str = "available.json";
/// Written by `provision.sh apply-request`.
pub const STATUS_FILE: &str = "status.json";

const AVAILABLE_SCHEMA: &str = "ikenga-update-available/1";
const STATUS_SCHEMA: &str = "ikenga-update-status/1";
const REQUEST_SCHEMA: &str = "ikenga-update-request/1";

/// Root's files are small; anything bigger is not one of them.
const MAX_STATE_FILE: u64 = 64 * 1024;
/// provision.sh `apply-request` reads at most this much of a request.
const MAX_REQUEST: usize = 4096;
/// `ikenga-update.service` `TimeoutStartSec=15min`, plus slack: a `running`
/// status older than this is an apply that died, reported as `interrupted`.
const RUNNING_STALE: Duration = Duration::from_secs(20 * 60);
/// provision.sh `UPDATE_REQUEST_MAX_AGE` (900 s), plus slack: root refuses an
/// older request, so one still lying here was never picked up (no path unit)
/// and must not block a fresh one forever.
const REQUEST_STALE: Duration = Duration::from_secs(16 * 60);
/// provision.sh `UPDATE_RETRY_COOLDOWN`, mirrored so the UI can say why.
const RETRY_COOLDOWN: Duration = Duration::from_secs(60 * 60);
/// One accepted request per process per this long.
const APPLY_THROTTLE: Duration = Duration::from_secs(30);
/// provision.sh accepts at most four digits.
const MAX_ACK: u32 = 9999;

/// The state directory: [`STATE_DIR_ENV`] when set, else
/// [`DEFAULT_STATE_DIR`].
pub fn state_dir() -> PathBuf {
    std::env::var_os(STATE_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

// ─── root's files ───────────────────────────────────────────────────────────

/// `available.json` (`ikenga-update-available/1`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AvailableFile {
    schema: String,
    checked_at: Option<String>,
    #[allow(dead_code)]
    channel: Option<String>,
    #[allow(dead_code)]
    installed: Option<String>,
    latest: Option<String>,
    min_upgrade_from: Option<String>,
    blocked: bool,
    blocked_reason: Option<String>,
    notes_url: Option<String>,
    published_at: Option<String>,
    last_error: Option<String>,
}

/// `status.json` (`ikenga-update-status/1`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusFile {
    schema: String,
    state: String,
    from: Option<String>,
    to: Option<String>,
    request_id: Option<String>,
    requested_by: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    rolled_back: bool,
    exit_code: Option<i64>,
    message: Option<String>,
    #[serde(default)]
    log_tail: Vec<String>,
}

const RUN_STATES: &[&str] = &[
    "running",
    "succeeded",
    "noop",
    "rolled_back",
    "failed",
    "refused",
];

fn is_semver(s: &str) -> bool {
    s.len() <= 32 && {
        let parts: Vec<&str> = s.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
    }
}

fn semver_tuple(s: &str) -> Option<(u64, u64, u64)> {
    if !is_semver(s) {
        return None;
    }
    let mut it = s.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// `a < b`; false when either isn't `X.Y.Z`.
pub fn semver_lt(a: &str, b: &str) -> bool {
    matches!((semver_tuple(a), semver_tuple(b)), (Some(a), Some(b)) if a < b)
}

fn version(v: Option<String>) -> Option<String> {
    v.filter(|v| is_semver(v))
}

/// Display text from a root-written file: no control characters, bounded.
fn text(v: Option<String>, max: usize) -> Option<String> {
    v.map(|s| {
        s.chars()
            .filter(|c| !c.is_control())
            .take(max)
            .collect::<String>()
    })
    .filter(|s| !s.is_empty())
}

fn age_of(iso: Option<&str>) -> Option<Duration> {
    let t = chrono::DateTime::parse_from_rfc3339(iso?).ok()?;
    let secs = chrono::Utc::now().timestamp() - t.timestamp();
    Some(Duration::from_secs(secs.max(0) as u64))
}

/// A regular file of at most `cap` bytes, read in full; `None` otherwise.
fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > cap {
        return None;
    }
    let mut buf = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(cap + 1)
        .read_to_end(&mut buf)
        .ok()?;
    (buf.len() as u64 <= cap).then_some(buf)
}

fn read_available(dir: &Path) -> Option<AvailableFile> {
    let raw = read_capped(&dir.join(AVAILABLE_FILE), MAX_STATE_FILE)?;
    serde_json::from_slice::<AvailableFile>(&raw)
        .map_err(|e| tracing::warn!("update: {AVAILABLE_FILE} is not {AVAILABLE_SCHEMA}: {e}"))
        .ok()
        .filter(|a| a.schema == AVAILABLE_SCHEMA)
}

fn read_status(dir: &Path) -> Option<StatusFile> {
    let raw = read_capped(&dir.join(STATUS_FILE), MAX_STATE_FILE)?;
    serde_json::from_slice::<StatusFile>(&raw)
        .map_err(|e| tracing::warn!("update: {STATUS_FILE} is not {STATUS_SCHEMA}: {e}"))
        .ok()
        .filter(|s| s.schema == STATUS_SCHEMA && RUN_STATES.contains(&s.state.as_str()))
}

// ─── the view ───────────────────────────────────────────────────────────────

/// `GET /api/server/update` → `data`. Mirrored by
/// `src/lib/transport/server-update.ts` (`ServerUpdateView`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpdateView {
    /// Root's update units are installed here (it has written a file).
    pub supported: bool,
    /// This server's version (may differ from the SPA bundle a browser
    /// still has loaded).
    pub current: String,
    /// The advertised release, only when it is newer than `current`.
    pub available: Option<AvailableView>,
    pub checked_at: Option<String>,
    pub check_error: Option<String>,
    pub last_run: Option<RunView>,
    /// A request published but not yet claimed by root.
    pub pending_request: Option<PendingView>,
    /// Terminals a restart would end.
    pub open_terminals: usize,
    /// Some children could not be asked (busy or slow), so the count may be low.
    pub open_terminals_partial: bool,
    pub can_apply: bool,
    /// `unsupported` | `none_available` | `blocked_min_upgrade` | `running`
    /// | `pending` | `cooldown`.
    pub apply_blocked_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AvailableView {
    pub version: String,
    pub notes_url: Option<String>,
    pub published_at: Option<String>,
    pub min_upgrade_from: Option<String>,
    pub blocked: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunView {
    /// A `RUN_STATES` value, or `interrupted` for a `running` that is too
    /// old to still be running.
    pub state: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub request_id: Option<String>,
    pub requested_by: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub rolled_back: bool,
    pub exit_code: Option<i64>,
    pub message: Option<String>,
    pub log_tail: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PendingView {
    pub version: Option<String>,
    pub request_id: Option<String>,
    pub requested_by: Option<String>,
    pub requested_at: Option<String>,
}

/// The apply body (`POST /api/server/update/apply`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyBody {
    pub version: String,
    /// The open-terminal count the admin was shown and confirmed.
    pub acknowledged_open_terminals: u32,
}

/// A refused apply: HTTP status, stable `code`, human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub open_terminals: Option<usize>,
}

impl ApplyError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            open_terminals: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub version: String,
    pub request_id: String,
    /// False when the same version was already pending (idempotent re-POST).
    pub new: bool,
}

/// Root's files plus our own pending request, read once per call.
struct Snapshot {
    supported: bool,
    available: Option<AvailableFile>,
    status: Option<StatusFile>,
    pending: Option<PendingView>,
    /// The request file exists but is too old for root to accept.
    pending_stale: bool,
}

/// One per server process: the T0 daemon, or the T1 broker.
pub struct UpdateCtl {
    state_dir: PathBuf,
    request_path: PathBuf,
    current: String,
    /// Serializes applies, and remembers the last accepted one (throttle).
    last_apply: Mutex<Option<Instant>>,
}

impl std::fmt::Debug for UpdateCtl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateCtl")
            .field("state_dir", &self.state_dir)
            .field("request_path", &self.request_path)
            .finish()
    }
}

impl UpdateCtl {
    pub fn new(state_dir: PathBuf, request_path: PathBuf) -> Self {
        Self::with_version(state_dir, request_path, env!("CARGO_PKG_VERSION"))
    }

    /// [`new`](Self::new) with the running version made explicit (tests).
    pub fn with_version(state_dir: PathBuf, request_path: PathBuf, current: &str) -> Self {
        Self {
            state_dir,
            request_path,
            current: current.to_string(),
            last_apply: Mutex::new(None),
        }
    }

    pub fn request_path(&self) -> &Path {
        &self.request_path
    }

    fn snapshot(&self) -> Snapshot {
        let available = read_available(&self.state_dir);
        let status = read_status(&self.state_dir);
        // Supported = root's units have written something. The directory
        // alone is not enough: a manual `provision.sh upgrade` creates it for
        // its lock on a box that has no timer behind it.
        let supported = available.is_some()
            || status.is_some()
            || self.state_dir.join(AVAILABLE_FILE).exists()
            || self.state_dir.join(STATUS_FILE).exists();
        let (pending, pending_stale) = self.read_pending();
        Snapshot {
            supported,
            available,
            status,
            pending,
            pending_stale,
        }
    }

    fn read_pending(&self) -> (Option<PendingView>, bool) {
        let Ok(meta) = std::fs::symlink_metadata(&self.request_path) else {
            return (None, false);
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .is_some_and(|age| age > REQUEST_STALE);
        if stale {
            return (None, true);
        }
        let v: Value = read_capped(&self.request_path, MAX_REQUEST as u64)
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or(Value::Null);
        let field = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        (
            Some(PendingView {
                version: version(field("version")),
                request_id: text(field("request_id"), 36),
                requested_by: text(field("requested_by"), 64),
                requested_at: text(field("requested_at"), 40),
            }),
            false,
        )
    }

    /// The advertised release, when it is newer than this server. Computed
    /// from `latest`, not from any flag in the file, which goes stale after
    /// a manual upgrade.
    fn newer(&self, a: &AvailableFile) -> Option<AvailableView> {
        let latest = version(a.latest.clone())?;
        if !semver_lt(&self.current, &latest) {
            return None;
        }
        Some(AvailableView {
            version: latest,
            notes_url: text(a.notes_url.clone(), 300)
                .filter(|u| u.starts_with("https://github.com/")),
            published_at: text(a.published_at.clone(), 40),
            min_upgrade_from: version(a.min_upgrade_from.clone()),
            blocked: a.blocked,
            blocked_reason: text(a.blocked_reason.clone(), 200),
        })
    }

    fn run_view(s: &StatusFile) -> RunView {
        let interrupted = s.state == "running"
            && age_of(s.started_at.as_deref()).map_or(true, |age| age > RUNNING_STALE);
        RunView {
            state: if interrupted {
                "interrupted".into()
            } else {
                s.state.clone()
            },
            from: version(s.from.clone()),
            to: version(s.to.clone()),
            request_id: text(s.request_id.clone(), 36),
            requested_by: text(s.requested_by.clone(), 64),
            started_at: text(s.started_at.clone(), 40),
            finished_at: text(s.finished_at.clone(), 40),
            rolled_back: s.rolled_back,
            exit_code: s.exit_code,
            message: text(s.message.clone(), 300),
            log_tail: s
                .log_tail
                .iter()
                .rev()
                .take(40)
                .rev()
                .filter_map(|l| text(Some(l.clone()), 300))
                .collect(),
        }
    }

    /// Root refuses the same version for an hour after it failed; say so
    /// before the admin tries.
    fn cooling_down(status: Option<&RunView>, version: &str) -> bool {
        status.is_some_and(|r| {
            matches!(r.state.as_str(), "rolled_back" | "failed")
                && r.to.as_deref() == Some(version)
                && age_of(r.finished_at.as_deref()).is_some_and(|age| age < RETRY_COOLDOWN)
        })
    }

    fn blocked_reason(
        snap: &Snapshot,
        available: Option<&AvailableView>,
        run: Option<&RunView>,
    ) -> Option<&'static str> {
        if !snap.supported {
            return Some("unsupported");
        }
        let Some(available) = available else {
            return Some("none_available");
        };
        if available.blocked {
            return Some("blocked_min_upgrade");
        }
        if run.is_some_and(|r| r.state == "running") {
            return Some("running");
        }
        if snap.pending.is_some() {
            return Some("pending");
        }
        if Self::cooling_down(run, &available.version) {
            return Some("cooldown");
        }
        None
    }

    /// What an admin sees. `open_terminals` is counted by the caller (T0:
    /// its own PTYs; T1: every running child).
    pub fn view(&self, open_terminals: usize, open_terminals_partial: bool) -> UpdateView {
        let snap = self.snapshot();
        if !snap.supported {
            return UpdateView {
                supported: false,
                current: self.current.clone(),
                available: None,
                checked_at: None,
                check_error: None,
                last_run: None,
                pending_request: None,
                open_terminals,
                open_terminals_partial,
                can_apply: false,
                apply_blocked_reason: Some("unsupported"),
            };
        }
        let available = snap.available.as_ref().and_then(|a| self.newer(a));
        let last_run = snap.status.as_ref().map(Self::run_view);
        let reason = Self::blocked_reason(&snap, available.as_ref(), last_run.as_ref());
        UpdateView {
            supported: true,
            current: self.current.clone(),
            checked_at: snap
                .available
                .as_ref()
                .and_then(|a| text(a.checked_at.clone(), 40)),
            check_error: snap
                .available
                .as_ref()
                .and_then(|a| text(a.last_error.clone(), 200)),
            available,
            last_run,
            pending_request: snap.pending.clone(),
            open_terminals,
            open_terminals_partial,
            can_apply: reason.is_none(),
            apply_blocked_reason: reason,
        }
    }

    /// Publish an admin's request for root to apply.
    pub fn apply(
        &self,
        body: &ApplyBody,
        requested_by: &str,
        open_terminals: usize,
    ) -> Result<Applied, ApplyError> {
        // Serializes concurrent applies in this process.
        let mut last = self.last_apply.lock().unwrap_or_else(|e| e.into_inner());
        let snap = self.snapshot();
        if !snap.supported {
            return Err(ApplyError::new(
                StatusCode::NOT_FOUND,
                "unsupported",
                "server updates are not managed on this host",
            ));
        }
        let available = snap.available.as_ref().and_then(|a| self.newer(a));
        let Some(available) = available.filter(|a| a.version == body.version) else {
            return Err(ApplyError::new(
                StatusCode::CONFLICT,
                "not_advertised",
                "that version is not the available update",
            ));
        };
        if available.blocked {
            return Err(ApplyError::new(
                StatusCode::CONFLICT,
                "blocked",
                available
                    .blocked_reason
                    .clone()
                    .unwrap_or_else(|| "this update has to be applied over SSH".into()),
            ));
        }
        let run = snap.status.as_ref().map(Self::run_view);
        if run.as_ref().is_some_and(|r| r.state == "running") {
            return Err(ApplyError::new(
                StatusCode::CONFLICT,
                "update_running",
                "an update is already running",
            ));
        }
        if let Some(p) = &snap.pending {
            if p.version.as_deref() == Some(body.version.as_str()) {
                return Ok(Applied {
                    version: body.version.clone(),
                    request_id: p.request_id.clone().unwrap_or_default(),
                    new: false,
                });
            }
            return Err(ApplyError::new(
                StatusCode::CONFLICT,
                "pending",
                "another update request is waiting to be applied",
            ));
        }
        if Self::cooling_down(run.as_ref(), &body.version) {
            return Err(ApplyError::new(
                StatusCode::CONFLICT,
                "cooldown",
                "this version failed less than an hour ago; try again later",
            ));
        }
        // More terminals may have opened since the dialog was shown: the
        // admin confirms the count that is true now.
        if open_terminals > 0 && (body.acknowledged_open_terminals as usize) < open_terminals {
            return Err(ApplyError {
                open_terminals: Some(open_terminals),
                ..ApplyError::new(
                    StatusCode::CONFLICT,
                    "terminals_open",
                    format!(
                        "updating restarts the server and ends {open_terminals} open terminal(s)"
                    ),
                )
            });
        }
        if last.is_some_and(|t| t.elapsed() < APPLY_THROTTLE) {
            return Err(ApplyError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "throttled",
                "an update was requested moments ago",
            ));
        }
        if snap.pending_stale {
            // Root would refuse it; it only blocks the name.
            let _ = std::fs::remove_file(&self.request_path);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let doc = json!({
            "schema": REQUEST_SCHEMA,
            "version": body.version,
            "request_id": request_id,
            "requested_by": requested_by,
            "requested_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            "acknowledged_open_terminals": body.acknowledged_open_terminals.min(MAX_ACK),
        });
        match publish(&self.request_path, doc.to_string().as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Raced another writer: whatever is there now decides.
                return match self.read_pending().0 {
                    Some(p) if p.version.as_deref() == Some(body.version.as_str()) => Ok(Applied {
                        version: body.version.clone(),
                        request_id: p.request_id.unwrap_or_default(),
                        new: false,
                    }),
                    _ => Err(ApplyError::new(
                        StatusCode::CONFLICT,
                        "pending",
                        "another update request is waiting to be applied",
                    )),
                };
            }
            Err(e) => {
                tracing::error!(
                    "update: could not write {}: {e}",
                    self.request_path.display()
                );
                return Err(ApplyError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "the update request could not be written",
                ));
            }
        }
        *last = Some(Instant::now());
        Ok(Applied {
            version: body.version.clone(),
            request_id,
            new: true,
        })
    }
}

/// Write `bytes` to `path` so that nothing ever sees a partial file and an
/// existing request is never replaced: a private temp file in the same
/// directory, synced, then moved into place with no-replace semantics.
/// (`ikenga-update.path` fires on the name appearing, so a plain
/// create-then-write could hand root an empty file.) `AlreadyExists` when a
/// request is already there.
fn publish(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "request path has no parent",
        )
    })?;
    let mut rand = [0u8; 8];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut rand);
    let tmp = dir.join(format!(".update-request.{}.tmp", hex::encode(rand)));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        move_no_replace(&tmp, path)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// `renameat2(RENAME_NOREPLACE)`: one atomic step, so the final name never
/// has a second link (root refuses a request with nlink != 1).
#[cfg(target_os = "linux")]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in path"))
    };
    let (a, b) = (c(from)?, c(to)?);
    // SAFETY: two valid NUL-terminated paths, AT_FDCWD for both dirfds.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        // A filesystem without RENAME_NOREPLACE: link (fails if the name
        // exists), then drop the temp name.
        Some(libc::EINVAL) | Some(libc::ENOSYS) => {
            std::fs::hard_link(from, to)?;
            std::fs::remove_file(from)
        }
        _ => Err(err),
    }
}

#[cfg(not(target_os = "linux"))]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

// ─── HTTP ───────────────────────────────────────────────────────────────────

/// `{ ok: false, error, code }`: the broker's error envelope
/// (`server::auth::json_error`, which is Linux-only).
pub(crate) fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "ok": false, "error": message, "code": code })),
    )
        .into_response()
}

fn ok(status: StatusCode, data: impl Serialize) -> Response {
    (status, Json(json!({ "ok": true, "data": data }))).into_response()
}

fn apply_error(e: &ApplyError) -> Response {
    let mut body = json!({ "ok": false, "error": e.message, "code": e.code });
    if let Some(n) = e.open_terminals {
        body["open_terminals"] = json!(n);
    }
    (e.status, Json(body)).into_response()
}

fn parse_body(raw: &[u8]) -> Result<ApplyBody, Response> {
    let body: ApplyBody = serde_json::from_slice(raw).map_err(|_| {
        json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "expected {version, acknowledged_open_terminals}",
        )
    })?;
    if !is_semver(&body.version) {
        return Err(json_error(
            StatusCode::CONFLICT,
            "not_advertised",
            "that version is not the available update",
        ));
    }
    Ok(body)
}

fn unsupported() -> Response {
    json_error(
        StatusCode::NOT_FOUND,
        "unsupported",
        "server updates are not managed on this host",
    )
}

/// The audit row for an accepted request (T0 and T1).
fn requested_event(ctx: &AccessCtx, current: &str, applied: &Applied, open: usize) -> Event {
    Event::by("server.update_requested", ctx).detail(json!({
        "from": current,
        "to": applied.version,
        "request_id": applied.request_id,
        "open_terminals": open,
    }))
}

fn accepted(applied: &Applied) -> Response {
    ok(
        StatusCode::ACCEPTED,
        json!({ "state": "queued", "version": applied.version, "request_id": applied.request_id }),
    )
}

/// T0 `GET /api/server/update` (operator only: `access::route_requirement`).
pub(crate) async fn t0_status(State(state): State<Arc<AppState>>) -> Response {
    let Some(ctl) = state.update.clone() else {
        return unsupported();
    };
    let open = state.pty_manager.active_session_count();
    let view = tokio::task::spawn_blocking(move || ctl.view(open, false)).await;
    match view {
        Ok(v) => ok(StatusCode::OK, v),
        Err(_) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        ),
    }
}

/// T0 `POST /api/server/update/apply` (operator only).
pub(crate) async fn t0_apply(
    State(state): State<Arc<AppState>>,
    Extension(access): Extension<Arc<DaemonAccess>>,
    Extension(ctx): Extension<AccessCtx>,
    raw: Bytes,
) -> Response {
    // `route_requirement` already refused everyone else; this is the
    // belt to its braces.
    if !ctx.is_operator() {
        return json_error(StatusCode::FORBIDDEN, "forbidden", "admin only");
    }
    let Some(ctl) = state.update.clone() else {
        return unsupported();
    };
    let body = match parse_body(&raw) {
        Ok(b) => b,
        Err(r) => return r,
    };
    let open = state.pty_manager.active_session_count();
    let current = env!("CARGO_PKG_VERSION");
    let result = {
        let ctl = ctl.clone();
        let body = body.clone();
        tokio::task::spawn_blocking(move || ctl.apply(&body, "operator", open)).await
    };
    let applied = match result {
        Ok(Ok(a)) => a,
        Ok(Err(e)) => return apply_error(&e),
        Err(_) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    };
    if applied.new {
        tracing::info!(
            "server update {current} -> {} requested by the operator (request {}, {open} open terminals)",
            applied.version,
            applied.request_id
        );
        if let Some(store) = access.store() {
            crate::server::shared::notifications::routing::append_audit(
                store,
                &requested_event(&ctx, current, &applied, open),
            )
            .await;
        }
    }
    accepted(&applied)
}

#[cfg(target_os = "linux")]
pub use self::broker_routes::{broker_apply, broker_status};

/// The T1 broker's side: it answers these routes itself, after a fresh
/// admin check, and never forwards them to a principal's child.
#[cfg(target_os = "linux")]
mod broker_routes {
    use std::time::Duration;

    use axum::extract::{Request, State};
    use axum::http::request::Parts;
    use axum::http::StatusCode;
    use axum::response::Response;
    use axum::Extension;
    use serde_json::{json, Value};

    use super::{accepted, apply_error, json_error, ok, parse_body, requested_event, unsupported};
    use crate::access::AccessCtx;
    use crate::server::auth::{Credential, PrincipalCtx};
    use crate::server::broker::proxy::PRINCIPAL_HEADER;
    use crate::server::broker::BrokerState;
    use crate::server::operator::accounts::{self, Account};

    /// How long the broker waits for one child's terminal count.
    const CHILD_TIMEOUT: Duration = Duration::from_secs(2);

    fn forbidden() -> Response {
        json_error(StatusCode::FORBIDDEN, "forbidden", "admin only")
    }

    /// An enabled admin with admin strength, read fresh on every call (a
    /// demotion or a disable takes effect on the next request, like
    /// `/auth/me`). Returns the account and, when the access layer is
    /// installed, the request's access context (for the audit row).
    async fn admin(
        state: &BrokerState,
        ctx: &PrincipalCtx,
        parts: &Parts,
    ) -> Result<(Account, Option<AccessCtx>), Response> {
        let account = async {
            let mut conn = state.pool.acquire().await?;
            accounts::by_id(&mut conn, ctx.principal.id).await
        }
        .await;
        let account = match account {
            Ok(Some(a)) if !a.is_disabled() && a.is_admin => a,
            Ok(_) => {
                tracing::warn!(
                    "server update: refused principal {} (not an enabled admin)",
                    ctx.principal.id
                );
                return Err(forbidden());
            }
            Err(e) => {
                tracing::error!("server update: account lookup failed: {e}");
                return Err(json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "auth_unavailable",
                    "authentication is temporarily unavailable",
                ));
            }
        };
        let (strong, actx) = match &state.access_t1 {
            Some(t1) => match t1.ctx_for(ctx, parts).await {
                Ok(b) => (b.access.admin_strength, Some(b.access)),
                Err(_) => (false, None),
            },
            // No access layer (unit tests): only a password session.
            None => (matches!(ctx.via, Credential::Session { .. }), None),
        };
        if !strong {
            tracing::warn!(
                "server update: refused principal {} (credential lacks admin strength)",
                ctx.principal.id
            );
            return Err(forbidden());
        }
        Ok((account, actx))
    }

    /// Open terminals across every running child. Never launches a child,
    /// never waits on one that is starting; a child that is busy, slow or
    /// unreachable makes the count `partial`. (The broker cannot count them
    /// itself: `ProtectProc=invisible` and no `CAP_SYS_PTRACE` hide the
    /// principals' processes from it.)
    pub(crate) async fn open_terminals(state: &BrokerState) -> (usize, bool) {
        let (endpoints, mut partial) = state.children.running_endpoints();
        let calls = endpoints.into_iter().map(|(id, ep)| {
            let http = state.http.clone();
            async move {
                let sent = http
                    .post(format!("http://{}/api/rpc", ep.addr))
                    .bearer_auth(&*ep.token)
                    .header(PRINCIPAL_HEADER, id.to_string())
                    .header(crate::access::INTERNAL_CALL_HEADER, "1")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(json!({ "cmd": "server_open_terminals", "args": {} }).to_string())
                    .send();
                let resp = tokio::time::timeout(CHILD_TIMEOUT, sent).await.ok()?.ok()?;
                let bytes = tokio::time::timeout(CHILD_TIMEOUT, resp.bytes())
                    .await
                    .ok()?
                    .ok()?;
                let v: Value = serde_json::from_slice(&bytes).ok()?;
                if v.get("ok").and_then(Value::as_bool) != Some(true) {
                    return None;
                }
                v.pointer("/data/open").and_then(Value::as_u64)
            }
        });
        let mut total = 0usize;
        for n in futures_util::future::join_all(calls).await {
            match n {
                Some(n) => total += n as usize,
                None => partial = true,
            }
        }
        (total, partial)
    }

    /// T1 `GET /api/server/update`.
    pub async fn broker_status(
        State(state): State<std::sync::Arc<BrokerState>>,
        Extension(ctx): Extension<PrincipalCtx>,
        req: Request,
    ) -> Response {
        let (parts, _) = req.into_parts();
        if let Err(r) = admin(&state, &ctx, &parts).await {
            return r;
        }
        let Some(ctl) = state.update.clone() else {
            return unsupported();
        };
        let (open, partial) = open_terminals(&state).await;
        match tokio::task::spawn_blocking(move || ctl.view(open, partial)).await {
            Ok(v) => ok(StatusCode::OK, v),
            Err(_) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            ),
        }
    }

    /// T1 `POST /api/server/update/apply`.
    pub async fn broker_apply(
        State(state): State<std::sync::Arc<BrokerState>>,
        Extension(ctx): Extension<PrincipalCtx>,
        req: Request,
    ) -> Response {
        let (parts, body) = req.into_parts();
        let (account, actx) = match admin(&state, &ctx, &parts).await {
            Ok(a) => a,
            Err(r) => return r,
        };
        let Some(ctl) = state.update.clone() else {
            return unsupported();
        };
        let Ok(raw) = axum::body::to_bytes(body, super::MAX_REQUEST).await else {
            return json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request",
                "body too large",
            );
        };
        let body = match parse_body(&raw) {
            Ok(b) => b,
            Err(r) => return r,
        };
        let (open, _) = open_terminals(&state).await;
        let current = env!("CARGO_PKG_VERSION");
        let requested_by = account.username.clone();
        let result = {
            let ctl = ctl.clone();
            let body = body.clone();
            tokio::task::spawn_blocking(move || ctl.apply(&body, &requested_by, open)).await
        };
        let applied = match result {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => return apply_error(&e),
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        };
        if applied.new {
            tracing::info!(
                "server update {current} -> {} requested by {} ({}; request {}, {open} open terminals)",
                applied.version,
                account.username,
                ctx.principal.id,
                applied.request_id
            );
            if let (Some(t1), Some(actx)) = (&state.access_t1, &actx) {
                crate::server::shared::notifications::routing::append_audit(
                    &t1.store,
                    &requested_event(actx, current, &applied, open),
                )
                .await;
            }
        }
        accepted(&applied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, v: Value) {
        std::fs::write(dir.join(name), v.to_string()).unwrap();
    }

    fn available(latest: &str) -> Value {
        json!({
            "schema": AVAILABLE_SCHEMA, "checked_at": "2026-10-06T00:00:00Z", "channel": "stable",
            "installed": "0.20.0", "latest": latest, "min_upgrade_from": null, "blocked": false,
            "blocked_reason": null, "notes_url": format!("https://github.com/ikenga-hq/ikenga/releases/tag/v{latest}"),
            "published_at": "2026-10-05T12:00:00Z", "last_error": null
        })
    }

    fn status(state: &str, to: &str, started_ago: i64, finished_ago: Option<i64>) -> Value {
        let at = |ago: i64| {
            (chrono::Utc::now() - chrono::Duration::seconds(ago))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        json!({
            "schema": STATUS_SCHEMA, "state": state, "from": "0.20.0", "to": to,
            "request_id": "6f1c7a52-6c1e-4f1c-9c3e-6a1f2b3c4d5e", "requested_by": "ada",
            "started_at": at(started_ago), "finished_at": finished_ago.map(at),
            "rolled_back": state == "rolled_back", "exit_code": null, "message": "m", "log_tail": ["==> a", "    b"]
        })
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        state: PathBuf,
        data: PathBuf,
        ctl: UpdateCtl,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let ctl = UpdateCtl::with_version(state.clone(), data.join(REQUEST_FILE), "0.20.0");
        Fixture {
            _tmp: tmp,
            state,
            data,
            ctl,
        }
    }

    fn body(v: &str, ack: u32) -> ApplyBody {
        ApplyBody {
            version: v.into(),
            acknowledged_open_terminals: ack,
        }
    }

    #[test]
    fn semver_compare() {
        assert!(semver_lt("0.9.9", "0.10.0"));
        assert!(!semver_lt("0.10.0", "0.10.0"));
        assert!(!semver_lt("1.0.0", "0.10.0"));
        assert!(!semver_lt("x", "1.0.0"));
        assert!(!is_semver("1.0"));
        assert!(!is_semver("1.0.0-rc1"));
        assert!(!is_semver("0.18.3$(touch /tmp/x)"));
    }

    #[test]
    fn unsupported_until_root_writes_a_file() {
        let f = fixture();
        let v = f.ctl.view(0, false);
        assert!(!v.supported);
        assert_eq!(v.current, "0.20.0");
        assert_eq!(v.apply_blocked_reason, Some("unsupported"));
        // A missing state dir is the same.
        let ctl =
            UpdateCtl::with_version(f.state.join("nope"), f.data.join(REQUEST_FILE), "0.20.0");
        assert!(!ctl.view(0, false).supported);
        let e = f.ctl.apply(&body("0.21.0", 0), "op", 0).unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::NOT_FOUND, "unsupported"));
    }

    #[test]
    fn files_are_parsed_strictly() {
        let f = fixture();
        // Unknown field: not the schema.
        let mut a = available("0.21.0");
        a["surprise"] = json!(1);
        write(&f.state, AVAILABLE_FILE, a);
        assert!(read_available(&f.state).is_none());
        // Wrong schema.
        let mut a = available("0.21.0");
        a["schema"] = json!("ikenga-update-available/2");
        write(&f.state, AVAILABLE_FILE, a);
        assert!(read_available(&f.state).is_none());
        // Over the size cap.
        std::fs::write(f.state.join(AVAILABLE_FILE), vec![b' '; 70 * 1024]).unwrap();
        assert!(read_available(&f.state).is_none());
        // Unknown run state.
        let mut s = status("succeeded", "0.21.0", 60, Some(30));
        s["state"] = json!("exploded");
        write(&f.state, STATUS_FILE, s);
        assert!(read_status(&f.state).is_none());
        // A file that exists but does not parse still marks the host supported.
        assert!(f.ctl.view(0, false).supported);
    }

    #[test]
    fn available_only_when_newer() {
        let f = fixture();
        write(&f.state, AVAILABLE_FILE, available("0.21.0"));
        let v = f.ctl.view(2, true);
        assert!(v.supported);
        let a = v.available.unwrap();
        assert_eq!(a.version, "0.21.0");
        assert_eq!(
            a.notes_url.as_deref(),
            Some("https://github.com/ikenga-hq/ikenga/releases/tag/v0.21.0")
        );
        assert_eq!((v.open_terminals, v.open_terminals_partial), (2, true));
        assert!(v.can_apply);

        // The file still says 0.20.0 is latest after a manual upgrade to it.
        write(&f.state, AVAILABLE_FILE, available("0.20.0"));
        let v = f.ctl.view(0, false);
        assert!(v.available.is_none());
        assert_eq!(v.apply_blocked_reason, Some("none_available"));
    }

    #[test]
    fn a_stale_running_is_interrupted_and_does_not_block() {
        let f = fixture();
        write(&f.state, AVAILABLE_FILE, available("0.21.0"));
        write(&f.state, STATUS_FILE, status("running", "0.21.0", 60, None));
        let v = f.ctl.view(0, false);
        assert_eq!(v.last_run.as_ref().unwrap().state, "running");
        assert_eq!(v.apply_blocked_reason, Some("running"));
        let e = f.ctl.apply(&body("0.21.0", 0), "op", 0).unwrap_err();
        assert_eq!(e.code, "update_running");

        write(
            &f.state,
            STATUS_FILE,
            status("running", "0.21.0", 21 * 60, None),
        );
        let v = f.ctl.view(0, false);
        assert_eq!(v.last_run.as_ref().unwrap().state, "interrupted");
        assert!(v.can_apply);
    }

    #[test]
    fn apply_refusals() {
        let f = fixture();
        let mut a = available("0.21.0");
        write(&f.state, AVAILABLE_FILE, a.clone());

        let e = f.ctl.apply(&body("0.22.0", 0), "op", 0).unwrap_err();
        assert_eq!((e.status, e.code), (StatusCode::CONFLICT, "not_advertised"));
        let e = f.ctl.apply(&body("0.19.0", 0), "op", 0).unwrap_err();
        assert_eq!(e.code, "not_advertised");

        a["blocked"] = json!(true);
        a["blocked_reason"] = json!("requires 0.20.5 first");
        write(&f.state, AVAILABLE_FILE, a);
        let e = f.ctl.apply(&body("0.21.0", 0), "op", 0).unwrap_err();
        assert_eq!(e.code, "blocked");
        assert_eq!(
            f.ctl.view(0, false).apply_blocked_reason,
            Some("blocked_min_upgrade")
        );

        write(&f.state, AVAILABLE_FILE, available("0.21.0"));
        // Terminals: acknowledging fewer than are open now re-prompts.
        let e = f.ctl.apply(&body("0.21.0", 1), "op", 3).unwrap_err();
        assert_eq!((e.code, e.open_terminals), ("terminals_open", Some(3)));
        assert!(!f.data.join(REQUEST_FILE).exists());

        // Cooldown after a failed attempt at the same version.
        write(
            &f.state,
            STATUS_FILE,
            status("rolled_back", "0.21.0", 120, Some(60)),
        );
        let e = f.ctl.apply(&body("0.21.0", 0), "op", 0).unwrap_err();
        assert_eq!(e.code, "cooldown");
        assert_eq!(f.ctl.view(0, false).apply_blocked_reason, Some("cooldown"));
        write(
            &f.state,
            STATUS_FILE,
            status("rolled_back", "0.21.0", 7300, Some(7200)),
        );
        assert!(f.ctl.view(0, false).can_apply);
    }

    #[test]
    fn apply_publishes_one_complete_private_file_and_is_idempotent() {
        let f = fixture();
        write(&f.state, AVAILABLE_FILE, available("0.21.0"));
        let a = f.ctl.apply(&body("0.21.0", 2), "ada", 2).unwrap();
        assert!(a.new);
        let path = f.data.join(REQUEST_FILE);
        let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(doc["schema"], REQUEST_SCHEMA);
        assert_eq!(doc["version"], "0.21.0");
        assert_eq!(doc["requested_by"], "ada");
        assert_eq!(doc["acknowledged_open_terminals"], 2);
        assert_eq!(doc["request_id"], a.request_id.as_str());
        assert_eq!(doc["request_id"].as_str().unwrap().len(), 36);
        assert!(
            chrono::DateTime::parse_from_rfc3339(doc["requested_at"].as_str().unwrap()).is_ok()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(&path).unwrap();
            assert_eq!(meta.mode() & 0o777, 0o600);
            assert_eq!(meta.nlink(), 1, "no second name may point at the request");
        }
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&f.data)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");

        // Same version again: queued, same request, still one file.
        let again = f.ctl.apply(&body("0.21.0", 2), "ada", 2).unwrap();
        assert!(!again.new);
        assert_eq!(again.request_id, a.request_id);
        assert_eq!(f.ctl.view(0, false).apply_blocked_reason, Some("pending"));
        let v = f.ctl.view(0, false);
        assert_eq!(
            v.pending_request.unwrap().version.as_deref(),
            Some("0.21.0")
        );
    }

    #[test]
    fn a_different_pending_version_conflicts_and_throttle_applies() {
        let f = fixture();
        write(&f.state, AVAILABLE_FILE, available("0.21.0"));
        f.ctl.apply(&body("0.21.0", 0), "ada", 0).unwrap();
        // Root claims it (the file disappears), then advertises a newer one.
        std::fs::remove_file(f.data.join(REQUEST_FILE)).unwrap();
        write(&f.state, AVAILABLE_FILE, available("0.21.1"));
        let e = f.ctl.apply(&body("0.21.1", 0), "ada", 0).unwrap_err();
        assert_eq!(
            (e.status, e.code),
            (StatusCode::TOO_MANY_REQUESTS, "throttled")
        );

        // A pending request for another version.
        let f = fixture();
        write(&f.state, AVAILABLE_FILE, available("0.21.1"));
        std::fs::write(
            f.data.join(REQUEST_FILE),
            json!({"schema": REQUEST_SCHEMA, "version": "0.21.0"}).to_string(),
        )
        .unwrap();
        let e = f.ctl.apply(&body("0.21.1", 0), "ada", 0).unwrap_err();
        assert_eq!(e.code, "pending");
    }

    #[test]
    fn publish_never_replaces() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(REQUEST_FILE);
        publish(&p, b"one").unwrap();
        let e = publish(&p, b"two").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    /// T0 over HTTP: the operator bearer only; the request lands in the data
    /// dir; every accepted apply is audited.
    mod t0_http {
        use std::path::PathBuf;
        use std::sync::Arc;

        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use axum::Router;
        use serde_json::{json, Value};
        use tower::ServiceExt;

        use super::{available, write, AVAILABLE_FILE, REQUEST_FILE};
        use crate::access::caps::Tier;
        use crate::access::devices::tests::pair;
        use crate::access::{AccessStore, DaemonAccess};
        use crate::executor::ExecutorTier;
        use crate::pty::PtyManager;
        use crate::server::update::UpdateCtl;
        use crate::server::{router_with_update, ServerConfig};

        const OP: &str = "operator-token-for-update-tests";

        struct T0 {
            _tmp: tempfile::TempDir,
            state: PathBuf,
            data: PathBuf,
            router: Router,
            access: Arc<DaemonAccess>,
        }

        async fn t0(access: Arc<DaemonAccess>, with_update: bool) -> T0 {
            let tmp = tempfile::tempdir().unwrap();
            let state = tmp.path().join("state");
            let data = tmp.path().join("data");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::create_dir_all(&data).unwrap();
            let config = ServerConfig {
                host: "127.0.0.1".into(),
                port: 0,
                static_dir: PathBuf::from("no-spa-here"),
                pkgs_dir: None,
                data_dir: Some(data.clone()),
                auth_token: Some(OP.into()),
                allowed_origins: vec![],
                idle_timeout_secs: None,
                executor_tier: ExecutorTier::T0,
            };
            let ctl = with_update.then(|| {
                Arc::new(UpdateCtl::with_version(
                    state.clone(),
                    data.join(REQUEST_FILE),
                    "0.20.0",
                ))
            });
            let router =
                router_with_update(config, access.clone(), Arc::new(PtyManager::new()), ctl);
            T0 {
                _tmp: tmp,
                state,
                data,
                router,
                access,
            }
        }

        async fn call(
            router: &Router,
            method: &str,
            path: &str,
            bearer: &str,
            origin: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut req = Request::builder()
                .method(method)
                .uri(path)
                .header("host", "box.test:4000")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json");
            if let Some(o) = origin {
                req = req.header("origin", o);
            }
            let body = body
                .map(|b| Body::from(b.to_string()))
                .unwrap_or_else(Body::empty);
            let res = router
                .clone()
                .oneshot(req.body(body).unwrap())
                .await
                .unwrap();
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        fn apply_body() -> Value {
            json!({"version": "0.21.0", "acknowledged_open_terminals": 0})
        }

        #[tokio::test]
        async fn the_operator_reads_the_view_and_publishes_an_audited_request() {
            let d = t0(
                DaemonAccess::with_store(AccessStore::memory_t0().await),
                true,
            )
            .await;

            // Root has written nothing yet: unsupported, and the UI shows nothing.
            let (s, b) = call(&d.router, "GET", "/api/server/update", OP, None, None).await;
            assert_eq!(s, StatusCode::OK, "{b}");
            assert_eq!(b["data"]["supported"], false);

            write(&d.state, AVAILABLE_FILE, available("0.21.0"));
            let (s, b) = call(&d.router, "GET", "/api/server/update", OP, None, None).await;
            assert_eq!(s, StatusCode::OK);
            assert_eq!(b["data"]["available"]["version"], "0.21.0");
            assert_eq!(b["data"]["can_apply"], true);
            assert_eq!(b["data"]["open_terminals"], 0);

            let (s, b) = call(
                &d.router,
                "POST",
                "/api/server/update/apply",
                OP,
                Some("http://box.test:4000"),
                Some(apply_body()),
            )
            .await;
            assert_eq!(s, StatusCode::ACCEPTED, "{b}");
            assert_eq!(b["data"]["state"], "queued");
            let doc: Value =
                serde_json::from_slice(&std::fs::read(d.data.join(REQUEST_FILE)).unwrap()).unwrap();
            assert_eq!(doc["version"], "0.21.0");
            assert_eq!(doc["requested_by"], "operator");
            assert_eq!(doc["request_id"], b["data"]["request_id"]);

            let store = d.access.store().unwrap();
            let rows: Vec<(String, String)> =
                sqlx::query_as("SELECT kind, detail FROM audit_events WHERE kind = ?")
                    .bind("server.update_requested")
                    .fetch_all(store.pool())
                    .await
                    .unwrap();
            assert_eq!(rows.len(), 1, "one audit row per accepted request");
            let detail: Value = serde_json::from_str(&rows[0].1).unwrap();
            assert_eq!(detail["to"], "0.21.0");
            assert_eq!(detail["request_id"], doc["request_id"]);

            // The view now reports the pending request.
            let (_, b) = call(&d.router, "GET", "/api/server/update", OP, None, None).await;
            assert_eq!(b["data"]["apply_blocked_reason"], "pending");
        }

        #[tokio::test]
        async fn a_paired_device_of_any_tier_is_refused() {
            let d = t0(
                DaemonAccess::with_store(AccessStore::memory_t0().await),
                true,
            )
            .await;
            write(&d.state, AVAILABLE_FILE, available("0.21.0"));
            for tier in [Tier::View, Tier::Dispatch, Tier::Full] {
                let (_, tok) = pair(d.access.store().unwrap(), tier).await;
                let (s, b) = call(&d.router, "GET", "/api/server/update", &tok, None, None).await;
                assert_eq!(s, StatusCode::FORBIDDEN, "{tier:?}");
                assert!(b.get("data").is_none(), "no version leaks: {b}");
                let (s, _) = call(
                    &d.router,
                    "POST",
                    "/api/server/update/apply",
                    &tok,
                    None,
                    Some(apply_body()),
                )
                .await;
                assert_eq!(s, StatusCode::FORBIDDEN, "{tier:?}");
            }
            assert!(!d.data.join(REQUEST_FILE).exists());
        }

        #[tokio::test]
        async fn a_cross_origin_apply_is_rejected() {
            let d = t0(
                DaemonAccess::with_store(AccessStore::memory_t0().await),
                true,
            )
            .await;
            write(&d.state, AVAILABLE_FILE, available("0.21.0"));
            let (s, _) = call(
                &d.router,
                "POST",
                "/api/server/update/apply",
                OP,
                Some("https://evil.example"),
                Some(apply_body()),
            )
            .await;
            assert_eq!(s, StatusCode::UNAUTHORIZED);
            assert!(!d.data.join(REQUEST_FILE).exists());
        }

        #[tokio::test]
        async fn no_update_state_answers_unsupported() {
            // A router without an update controller (desktop-spawned, or no
            // --data-dir): 404 unsupported, nothing written.
            let d = t0(
                DaemonAccess::with_store(AccessStore::memory_t0().await),
                false,
            )
            .await;
            let (s, b) = call(&d.router, "GET", "/api/server/update", OP, None, None).await;
            assert_eq!(
                (s, b["code"].as_str()),
                (StatusCode::NOT_FOUND, Some("unsupported"))
            );
            let (s, _) = call(
                &d.router,
                "POST",
                "/api/server/update/apply",
                OP,
                None,
                Some(apply_body()),
            )
            .await;
            assert_eq!(s, StatusCode::NOT_FOUND);
        }

        #[tokio::test]
        async fn a_principal_child_never_serves_updates() {
            // Even with a controller wired in (the default wiring gives a
            // principal child none), its bearer is a child token, not the
            // operator's: `route_requirement` refuses before the handler.
            let d = t0(DaemonAccess::principal_child(Default::default()), true).await;
            write(&d.state, AVAILABLE_FILE, available("0.21.0"));
            for (m, p, body) in [
                ("GET", "/api/server/update", None),
                ("POST", "/api/server/update/apply", Some(apply_body())),
            ] {
                let (s, _) = call(&d.router, m, p, OP, None, body).await;
                assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
            }
            assert!(!d.data.join(REQUEST_FILE).exists());
        }
    }

    #[test]
    fn display_text_is_cleaned() {
        assert_eq!(
            text(Some("a\u{1b}[31mb\n".into()), 10).as_deref(),
            Some("a[31mb")
        );
        assert_eq!(text(Some("x".repeat(500)), 300).unwrap().len(), 300);
        assert_eq!(text(Some(String::new()), 10), None);
    }
}
