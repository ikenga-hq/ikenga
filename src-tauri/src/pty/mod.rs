//! Portable-PTY session pool. One `PtyManager` instance per app, holding a
//! `DashMap<String, PtySession>` keyed by short uuid.
//!
//! Each session runs:
//!  - a blocking reader thread (portable-pty's reader is sync) that pumps stdout
//!    bytes into a tokio mpsc channel
//!  - a tokio task that batches those bytes into ≤8KB chunks at ~120Hz and
//!    hands them to the session's `DataSink` (production: base64 + the
//!    `pty://{id}` Tauri event)
//!  - a child waiter that reports the exit code to the session's `ExitSink`
//!    (production: the `pty://{id}/exit` Tauri event)
//!
//! Attaching a second window to a live session goes through the two-step
//! `attach_begin` / `attach_arm` handshake, which snapshots the scrollback and
//! gates the stream under one lock so the snapshot and the new listener's live
//! stream tile exactly — no duplicated or dropped bytes at the seam.

pub mod foreground;
pub mod overlay;

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
#[cfg(feature = "desktop")]
use base64::Engine;
use dashmap::DashMap;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
// The Tauri sink is one of two ways a session's bytes leave this module; the
// other is the `Custom` callback pair the daemon uses. Only the former needs a
// webview runtime.
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, Notify};
use uuid::Uuid;

const CHUNK_SIZE: usize = 8 * 1024;
const FLUSH_INTERVAL_MS: u64 = 8; // ≈120 Hz
/// Cap on the per-session scrollback ring. A late-attaching window (a popped-out
/// terminal) replays at most this many trailing bytes before subscribing live.
const SCROLLBACK_CAP: usize = 256 * 1024;
/// Upper bound on the bytes an in-flight attach handshake may hold back before
/// the gate releases itself. A handshake is two IPC round-trips (single-digit
/// ms, per the Phase 0.5 numbers); this only trips if the attaching window died
/// between them and the watchdog hasn't fired yet.
const GATE_HOLD_CAP: usize = 256 * 1024;
/// How long an attach gate may hold the stream before the watchdog releases it.
/// Bounds the damage from a window that calls `attach_begin` and never arms.
pub const ATTACH_GATE_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on concurrently-live PTY sessions. A runaway caller (a stuck
/// spawn loop in a pkg or the frontend) can't exhaust file descriptors / child
/// processes past this. On overflow, `spawn` returns a clean `Err` — surfaced
/// to the FE as a failed `pty_spawn` — rather than allocating unbounded PTYs.
/// 64 is comfortably above any realistic number of open terminal panes.
const MAX_LIVE_SESSIONS: usize = 64;
const EXITED_RETENTION: Duration = Duration::from_secs(10 * 60);
const AUDIT_CAP: usize = 1_000;
const MAX_CONTROL_WRITE_BYTES: usize = 1_000_000;

/// Bounded ring of the bytes emitted on `pty://{id}`, plus a monotonic `total`
/// of every byte ever emitted. `total` keeps growing after the tail is trimmed,
/// so a snapshot's end offset stays comparable with the offsets carried on live
/// events: an attaching window's snapshot ends at `total`, and the first chunk
/// its listener receives starts there.
struct ScrollbackRing {
    buf: std::collections::VecDeque<u8>,
    total: u64,
}

impl ScrollbackRing {
    fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            total: 0,
        }
    }

    /// Record freshly-emitted bytes; returns the cumulative end offset
    /// (including this push) so the caller can tag the matching live event.
    fn push(&mut self, data: &[u8]) -> u64 {
        self.total += data.len() as u64;
        self.buf.extend(data.iter().copied());
        while self.buf.len() > SCROLLBACK_CAP {
            self.buf.pop_front();
        }
        self.total
    }

    /// (trailing bytes, end offset). `end - bytes.len()` is the absolute offset
    /// of the first returned byte.
    fn snapshot(&self) -> (Vec<u8>, u64) {
        (self.buf.iter().copied().collect(), self.total)
    }

    fn read_after(&self, after: Option<u64>, limit: Option<usize>) -> OutputSnapshot {
        let available_start = self.total.saturating_sub(self.buf.len() as u64);
        let requested_start = after.unwrap_or(available_start);
        let truncated = requested_start < available_start;
        let mut start = requested_start.max(available_start).min(self.total);
        if let Some(limit) = limit.filter(|limit| *limit > 0) {
            start = start.max(self.total.saturating_sub(limit as u64));
        }
        let offset = (start - available_start) as usize;
        OutputSnapshot {
            data: self.buf.iter().skip(offset).copied().collect(),
            start_offset: start,
            end_offset: self.total,
            available_start_offset: available_start,
            truncated,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputSnapshot {
    pub data: Vec<u8>,
    pub start_offset: u64,
    pub end_offset: u64,
    pub available_start_offset: u64,
    pub truncated: bool,
}

/// An attach handshake in flight. While one is installed the session emits
/// nothing to its sink — every chunk accumulates in `held` instead — so the
/// attaching window can register its listener knowing that (a) the snapshot it
/// was handed is a prefix of the stream with a hard end, and (b) nothing after
/// that end has been delivered to anyone yet. `arm` flushes `held` as the first
/// chunk the new listener sees. Snapshot and gate are installed under the same
/// lock the emitter holds, so there is no window in which a chunk is both in
/// the snapshot and delivered live.
struct AttachGate {
    token: u64,
    held: Vec<u8>,
}

/// The ring plus any in-flight attach gate, behind ONE mutex. Single-lock is
/// load-bearing: `attach_begin` must observe a stream state that no in-flight
/// emission can be straddling.
struct EmitState {
    ring: ScrollbackRing,
    gate: Option<AttachGate>,
}

/// Snapshot handed to a window that is attaching to a live session, plus the
/// token that releases the gate it installed.
pub struct AttachSnapshot {
    pub data: Vec<u8>,
    pub end_offset: u64,
    pub token: u64,
}

/// Sink for a session's emitted byte stream: `(chunk, cumulative end offset)`.
/// Production wires this to the `pty://{id}` Tauri event. Tests substitute a
/// recorder so the attach seam can be asserted against a real PTY without a
/// webview.
pub type DataSink = Arc<dyn Fn(&[u8], u64) + Send + Sync + 'static>;
/// Sink for the child's exit code. Production emits `pty://{id}/exit`.
pub type ExitSink = Box<dyn FnOnce(i32) + Send + 'static>;

/// How a session's sinks are built. The event names depend on the session id,
/// which `spawn_inner` generates, so the choice is passed in and materialized
/// once the id exists.
enum SinkSpec {
    #[cfg(feature = "desktop")]
    Tauri(AppHandle),
    Custom(DataSink, ExitSink),
}

static NEXT_ATTACH_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Wrapper for `MasterPty::process_group_leader`, which is `#[cfg(unix)]` in
/// portable-pty 0.8. On Windows there's no PTY foreground-PG concept, so we
/// surface `None` and let callers degrade gracefully.
#[cfg(unix)]
fn master_process_group_leader(master: &dyn MasterPty) -> Option<i32> {
    master.process_group_leader()
}

#[cfg(not(unix))]
fn master_process_group_leader(_master: &dyn MasterPty) -> Option<i32> {
    None
}

#[cfg(windows)]
fn resolve_windows_pty_command(cmd: &[String], cwd: &str) -> (String, Vec<String>) {
    if cmd.is_empty() {
        return (String::new(), Vec::new());
    }
    let raw_bin = &cmd[0];
    let augmented = crate::runtime::augmented_path();
    if let Ok(resolved) = which::which_in(raw_bin, Some(augmented), cwd) {
        let is_batch = resolved
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            .unwrap_or(false);
        if is_batch {
            let mut args = vec!["/c".to_string(), resolved.to_string_lossy().into_owned()];
            args.extend(cmd[1..].iter().cloned());
            return ("cmd.exe".to_string(), args);
        }
        return (resolved.to_string_lossy().into_owned(), cmd[1..].to_vec());
    }
    (raw_bin.clone(), cmd[1..].to_vec())
}

pub struct SpawnOpts {
    pub terminal_id: Option<String>,
    pub title: Option<String>,
    pub cwd: String,
    pub cmd: Vec<String>,
    pub env: HashMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalDescriptor {
    pub terminal_id: String,
    pub pty_id: String,
    pub title: String,
    pub label: Option<String>,
    pub cwd: String,
    pub argv: Vec<String>,
    pub status: &'static str,
    pub pid: Option<i32>,
    pub foreground_command: Option<foreground::ForegroundProcess>,
    pub created_at: u64,
    pub exited_at: Option<u64>,
    pub exit_code: Option<i32>,
    pub output_start_offset: u64,
    pub output_end_offset: u64,
    pub owner_agent_id: Option<String>,
    pub lease_expires_at: Option<u64>,
    pub mounted: bool,
    pub focused: bool,
    pub pane_ids: Vec<String>,
    pub window_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalAuditEntry {
    pub ts: u64,
    pub terminal_id: String,
    pub pty_id: String,
    pub actor: Option<String>,
    pub action: String,
    pub byte_count: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct TerminalLease {
    agent_id: String,
    token: String,
    expires_at: u64,
}

/// Async callback for byte-level subscribers attached *after* spawn (e.g.
/// the Claude session parser, which also wants the raw PTY bytes that the
/// xterm frontend gets). Subscribers see chunks pre-base64, in the order the
/// reader thread saw them. Errors are swallowed — subscribers detach by
/// dropping the boxed future on the next tick.
pub type ByteSubscriber = Box<dyn Fn(&[u8]) + Send + Sync + 'static>;

struct PtySession {
    terminal_id: String,
    title: String,
    cwd: String,
    argv: Vec<String>,
    created_at: u64,
    exited: AtomicBool,
    exited_at: AtomicU64,
    exit_code: AtomicI32,
    label: Mutex<Option<String>>,
    lease: Mutex<Option<TerminalLease>>,
    last_output_at: AtomicU64,
    output_notify: Notify,
    screen: Mutex<vt100::Parser>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    /// Child process ID if available from the OS. Only read on Windows,
    /// where teardown shells out to `taskkill /T` to kill the process tree.
    #[cfg_attr(not(windows), allow(dead_code))]
    pid: Option<u32>,
    /// Used to signal the reader thread to stop.
    killed: Arc<std::sync::atomic::AtomicBool>,
    /// Killer to terminate the child process directly.
    child_killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    /// Optional byte tap. Set once at spawn time via `spawn_with_subscriber`;
    /// kept as a `Mutex<Option<_>>` rather than baked into the spawn opts so
    /// callers without a subscriber don't pay any cost.
    subscriber: Mutex<Option<ByteSubscriber>>,
    /// Trailing scrollback of the emitted stream + any in-flight attach gate.
    /// One mutex, held across the sink call, so a snapshot taken here can never
    /// race a half-delivered chunk.
    emit: Mutex<EmitState>,
    /// Where emitted chunks go. Held on the session so `attach_begin` /
    /// `attach_arm` can flush a gate through the same path the emitter uses.
    sink: DataSink,
    broadcast_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
}

pub struct PtyManager {
    sessions: DashMap<String, Arc<PtySession>>,
    audit: Mutex<VecDeque<TerminalAuditEntry>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            audit: Mutex::new(VecDeque::with_capacity(AUDIT_CAP)),
        }
    }

    #[cfg(feature = "desktop")]
    pub async fn spawn(self: &Arc<Self>, app: AppHandle, opts: SpawnOpts) -> Result<String> {
        self.spawn_inner(SinkSpec::Tauri(app), opts, None).await
    }

    pub async fn spawn_with_sinks(
        self: &Arc<Self>,
        opts: SpawnOpts,
        data: DataSink,
        exit: ExitSink,
    ) -> Result<String> {
        self.spawn_inner(SinkSpec::Custom(data, exit), opts, None)
            .await
    }

    /// Spawn a headless PTY session without Tauri event sinks.
    /// Chunks are retained in the scrollback ring and broadcast to WebSocket subscribers.
    pub async fn spawn_headless(self: &Arc<Self>, opts: SpawnOpts) -> Result<String> {
        let data: DataSink = Arc::new(|_bytes, _end| {});
        let exit: ExitSink = Box::new(|_code| {});
        self.spawn_inner(SinkSpec::Custom(data, exit), opts, None)
            .await
    }

    /// Spawn with a byte subscriber attached. The subscriber sees raw PTY
    /// bytes alongside the normal `pty://{id}` event emission; used by the
    /// Claude session integration to feed `StreamParser`.
    #[cfg(feature = "desktop")]
    pub async fn spawn_with_subscriber(
        self: &Arc<Self>,
        app: AppHandle,
        opts: SpawnOpts,
        subscriber: ByteSubscriber,
    ) -> Result<String> {
        self.spawn_inner(SinkSpec::Tauri(app), opts, Some(subscriber))
            .await
    }

    async fn spawn_inner(
        self: &Arc<Self>,
        sinks: SinkSpec,
        opts: SpawnOpts,
        subscriber: Option<ByteSubscriber>,
    ) -> Result<String> {
        // Bound the number of live PTYs so a runaway caller can't spawn
        // unbounded child processes / fds. Soft cap: a couple of concurrent
        // spawns could momentarily race past it, which is harmless for a
        // safety bound. Checked before we allocate any PTY resources.
        if self
            .sessions
            .iter()
            .filter(|entry| !entry.value().exited.load(Ordering::Relaxed))
            .count()
            >= MAX_LIVE_SESSIONS
        {
            return Err(anyhow!(
                "PTY session limit reached ({MAX_LIVE_SESSIONS} live); close a terminal before opening another"
            ));
        }

        let id = Uuid::new_v4().to_string();
        let terminal_id = opts.terminal_id.clone().unwrap_or_else(|| id.clone());
        let title = opts.title.clone().unwrap_or_else(|| opts.cmd.join(" "));

        // Materialize the sinks now that the id (and therefore the event names)
        // exist. Chunks are wire-encoded as `<endOffset>:<base64>`; the offset
        // lets the frontend keep an absolute stream position for the capture
        // ring's own snapshot reconciliation.
        let (data_sink, exit_sink): (DataSink, ExitSink) = match sinks {
            #[cfg(feature = "desktop")]
            SinkSpec::Tauri(app) => {
                let event_name = format!("pty://{id}");
                let exit_event = format!("pty://{id}/exit");
                let app_for_exit = app.clone();
                let data: DataSink = Arc::new(move |bytes: &[u8], end_offset: u64| {
                    let engine = base64::engine::general_purpose::STANDARD;
                    let payload = engine.encode(bytes);
                    let _ = app.emit(&event_name, format!("{end_offset}:{payload}"));
                });
                let exit: ExitSink = Box::new(move |code: i32| {
                    let _ = app_for_exit.emit(&exit_event, code);
                });
                (data, exit)
            }
            SinkSpec::Custom(data, exit) => (data, exit),
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: opts.rows.max(1),
                cols: opts.cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let resolved_cwd = shellexpand::full(&opts.cwd)
            .map(|c| c.into_owned())
            .unwrap_or(opts.cwd.clone());

        #[cfg(windows)]
        let (exec_bin, exec_args) = resolve_windows_pty_command(&opts.cmd, &resolved_cwd);
        #[cfg(not(windows))]
        let (exec_bin, exec_args) = (opts.cmd[0].clone(), opts.cmd[1..].to_vec());

        let mut builder = CommandBuilder::new(&exec_bin);
        if !exec_args.is_empty() {
            builder.args(&exec_args);
        }
        builder.cwd(&resolved_cwd);

        // Inherit a sane PATH from the parent process so `claude` (and friends
        // installed in user-local bins) resolve. portable-pty does not inherit
        // env by default.
        for (k, v) in std::env::vars() {
            builder.env(&k, &v);
        }
        // Override PATH with the augmented one (ADR-013 §Addendum Decision 2)
        // so interactive auth terminals (`gemini auth`, `codex login`) and any
        // agent CLI launched in a pane resolve under nvm/npm/homebrew even when
        // the app inherited a thin GUI $PATH. Caller env below still wins.
        builder.env("PATH", crate::runtime::augmented_path());
        // WP-00: Thread CLAUDE_CONFIG_DIR overlay if not explicitly specified by caller env.
        if !opts.env.contains_key("CLAUDE_CONFIG_DIR")
            && std::env::var("CLAUDE_CONFIG_DIR").is_err()
        {
            if let Ok(overlay_dir) = overlay::ensure_claude_overlay_dir() {
                if let Some(dir_str) = overlay_dir.to_str() {
                    builder.env("CLAUDE_CONFIG_DIR", dir_str);
                }
            }
        }
        // Caller-supplied env wins.
        for (k, v) in &opts.env {
            builder.env(k, v);
        }
        // Sensible terminal defaults for full-screen TUIs (claude, vim, htop).
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");

        let mut child = pair.slave.spawn_command(builder).context("spawn child")?;
        let child_pid = child.process_id();
        let child_killer = child.clone_killer();

        let writer = pair.master.take_writer().context("take writer")?;
        let reader = pair.master.try_clone_reader().context("clone reader")?;

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(512);
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let session = Arc::new(PtySession {
            terminal_id,
            title,
            cwd: resolved_cwd,
            argv: opts.cmd.clone(),
            created_at: now_ms(),
            exited: AtomicBool::new(false),
            exited_at: AtomicU64::new(0),
            exit_code: AtomicI32::new(0),
            label: Mutex::new(None),
            lease: Mutex::new(None),
            last_output_at: AtomicU64::new(now_ms()),
            output_notify: Notify::new(),
            screen: Mutex::new(vt100::Parser::new(opts.rows.max(1), opts.cols.max(1), 0)),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            pid: child_pid,
            killed: killed.clone(),
            child_killer: Mutex::new(child_killer),
            subscriber: Mutex::new(subscriber),
            emit: Mutex::new(EmitState {
                ring: ScrollbackRing::new(),
                gate: None,
            }),
            sink: data_sink,
            broadcast_tx,
        });

        self.sessions.insert(id.clone(), session.clone());

        // --- Reader thread: blocking → mpsc + optional subscriber ---
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(256);
        let killed_for_reader = killed.clone();
        let session_for_reader = session.clone();
        thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; CHUNK_SIZE];
            loop {
                if killed_for_reader.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = &buf[..n];
                        if let Ok(mut screen) = session_for_reader.screen.lock() {
                            screen.process(chunk);
                        }
                        session_for_reader
                            .last_output_at
                            .store(now_ms(), Ordering::Relaxed);
                        session_for_reader.output_notify.notify_waiters();
                        if let Ok(mut guard) = session_for_reader.subscriber.lock() {
                            // Guard the subscriber callback: a panicking tap
                            // (e.g. the Claude stream parser) must not unwind
                            // and kill this reader thread — the PTY has to keep
                            // draining so the terminal stays alive. On panic we
                            // log once and detach the tap so it can't re-panic
                            // on every subsequent chunk and flood the log; the
                            // `pty://{id}` event stream is unaffected.
                            let panicked = if let Some(sub) = guard.as_ref() {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    sub(chunk)
                                }))
                                .is_err()
                            } else {
                                false
                            };
                            if panicked {
                                tracing::error!(
                                    "pty subscriber callback panicked; detaching the byte tap so the reader thread survives"
                                );
                                *guard = None;
                            }
                        }
                        if tx.blocking_send(chunk.to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        log::debug!("pty reader closed: {e}");
                        break;
                    }
                }
            }
        });

        // --- Emitter task: batch → ring → sink ---
        let session_for_emit = session.clone();
        tokio::spawn(async move {
            // Record the chunk in the scrollback ring and hand it to the sink
            // WHILE STILL HOLDING THE RING LOCK. That is what makes
            // `attach_begin` atomic: a snapshot taken under this same lock can
            // never observe a chunk that is recorded-but-not-yet-delivered, so
            // there is no overlap for the frontend to reconcile by offset.
            //
            // While an attach handshake is in flight the chunk is held back
            // instead of delivered; `attach_arm` flushes the held bytes as the
            // first chunk the newly-registered listener sees. Snapshot and held
            // bytes therefore tile the stream exactly — no gap, no repeat.
            let emit_chunk = |bytes: &[u8]| {
                let Ok(mut st) = session_for_emit.emit.lock() else {
                    return;
                };
                let end_offset = st.ring.push(bytes);
                if st.gate.is_some() {
                    let overflowed = {
                        // `is_some` checked directly above.
                        let gate = st.gate.as_mut().expect("gate present");
                        gate.held.extend_from_slice(bytes);
                        gate.held.len() > GATE_HOLD_CAP
                    };
                    if overflowed {
                        let gate = st.gate.take().expect("gate present");
                        tracing::warn!(
                            held = gate.held.len(),
                            "pty attach gate overflowed before arm; releasing the stream"
                        );
                        let _ = session_for_emit.broadcast_tx.send(gate.held.clone());
                        (session_for_emit.sink)(&gate.held, end_offset);
                    }
                    return;
                }
                let _ = session_for_emit.broadcast_tx.send(bytes.to_vec());
                (session_for_emit.sink)(bytes, end_offset);
            };
            let mut pending: Vec<u8> = Vec::with_capacity(CHUNK_SIZE);
            let mut interval = tokio::time::interval(Duration::from_millis(FLUSH_INTERVAL_MS));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    maybe_chunk = rx.recv() => {
                        match maybe_chunk {
                            Some(chunk) => pending.extend_from_slice(&chunk),
                            None => {
                                // Channel closed, flush remainder and exit.
                                if !pending.is_empty() {
                                    emit_chunk(&pending);
                                }
                                break;
                            }
                        }
                        // Flush eagerly if we have a full chunk.
                        while pending.len() >= CHUNK_SIZE {
                            let chunk: Vec<u8> = pending.drain(..CHUNK_SIZE).collect();
                            emit_chunk(&chunk);
                        }
                    }
                    _ = interval.tick() => {
                        if !pending.is_empty() {
                            emit_chunk(&pending);
                            pending.clear();
                        }
                    }
                }
            }
        });

        // --- Child waiter: blocking thread → exit event + cleanup ---
        let manager_for_wait = self.clone();
        let session_for_wait = session.clone();
        let id_for_wait = id.clone();
        let runtime = tokio::runtime::Handle::current();
        thread::spawn(move || {
            let exit_code = match child.wait() {
                Ok(status) => status.exit_code() as i32,
                Err(_) => -1,
            };
            session_for_wait
                .exit_code
                .store(exit_code, Ordering::Relaxed);
            session_for_wait
                .exited_at
                .store(now_ms(), Ordering::Relaxed);
            session_for_wait.exited.store(true, Ordering::Release);
            session_for_wait.output_notify.notify_waiters();
            exit_sink(exit_code);
            let manager_for_reap = manager_for_wait.clone();
            runtime.spawn(async move {
                tokio::time::sleep(EXITED_RETENTION).await;
                if let Some(entry) = manager_for_reap.sessions.get(&id_for_wait) {
                    if entry.exited.load(Ordering::Acquire) {
                        drop(entry);
                        manager_for_reap.sessions.remove(&id_for_wait);
                    }
                }
            });
        });

        Ok(id)
    }

    pub fn resolve_id(&self, target: &str) -> Result<String> {
        if self.sessions.contains_key(target) {
            return Ok(target.to_string());
        }
        let mut matches = Vec::new();
        for entry in self.sessions.iter() {
            let session = entry.value();
            let label_matches = session
                .label
                .lock()
                .ok()
                .and_then(|label| label.clone())
                .is_some_and(|label| label == target);
            if session.terminal_id == target || label_matches {
                matches.push((
                    entry.key().clone(),
                    session.exited.load(Ordering::Acquire),
                    session.created_at,
                ));
            }
        }
        matches.sort_by_key(|(_, exited, created_at)| (*exited, std::cmp::Reverse(*created_at)));
        matches
            .first()
            .map(|(id, _, _)| id.clone())
            .ok_or_else(|| anyhow!("unknown terminal: {target}"))
    }

    pub fn read_scrollback(&self, id: &str) -> Option<(Vec<u8>, u64)> {
        let resolved = self.resolve_id(id).ok()?;
        let session = self.sessions.get(&resolved)?.clone();
        let state = session.emit.lock().ok()?;
        Some(state.ring.snapshot())
    }

    pub fn read_output(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<usize>,
    ) -> Result<(OutputSnapshot, bool, Option<i32>)> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal: {id}"))?
            .clone();
        let state = session
            .emit
            .lock()
            .map_err(|_| anyhow!("terminal output lock poisoned"))?;
        let snapshot = state.ring.read_after(after, limit);
        let exited = session.exited.load(Ordering::Acquire);
        let exit_code = exited.then(|| session.exit_code.load(Ordering::Relaxed));
        Ok((snapshot, exited, exit_code))
    }

    pub fn screen_text(&self, id: &str) -> Result<String> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal: {id}"))?
            .clone();
        let screen = session
            .screen
            .lock()
            .map_err(|_| anyhow!("terminal screen lock poisoned"))?;
        Ok(screen.screen().contents())
    }

    pub fn list_terminals(&self) -> Vec<TerminalDescriptor> {
        let mut descriptors: Vec<_> = self
            .sessions
            .iter()
            .map(|entry| self.describe(entry.key(), entry.value()))
            .collect();
        descriptors.sort_by_key(|descriptor| descriptor.created_at);
        descriptors
    }

    fn describe(&self, pty_id: &str, session: &PtySession) -> TerminalDescriptor {
        let (output_start_offset, output_end_offset) = session
            .emit
            .lock()
            .ok()
            .map(|state| {
                let (_, end) = state.ring.snapshot();
                (end.saturating_sub(state.ring.buf.len() as u64), end)
            })
            .unwrap_or_default();
        let exited = session.exited.load(Ordering::Acquire);
        let lease = session.lease.lock().ok().and_then(|lease| lease.clone());
        TerminalDescriptor {
            terminal_id: session.terminal_id.clone(),
            pty_id: pty_id.to_string(),
            title: session.title.clone(),
            label: session.label.lock().ok().and_then(|label| label.clone()),
            cwd: session.cwd.clone(),
            argv: session.argv.clone(),
            status: if exited { "exited" } else { "running" },
            pid: (!exited)
                .then(|| {
                    session
                        .master
                        .lock()
                        .ok()
                        .and_then(|master| master_process_group_leader(&**master))
                })
                .flatten(),
            foreground_command: if exited {
                None
            } else {
                session
                    .master
                    .lock()
                    .ok()
                    .and_then(|master| master_process_group_leader(&**master))
                    .and_then(foreground::lookup)
            },
            created_at: session.created_at,
            exited_at: exited.then(|| session.exited_at.load(Ordering::Relaxed)),
            exit_code: exited.then(|| session.exit_code.load(Ordering::Relaxed)),
            output_start_offset,
            output_end_offset,
            owner_agent_id: lease.as_ref().map(|lease| lease.agent_id.clone()),
            lease_expires_at: lease.as_ref().map(|lease| lease.expires_at),
            mounted: false,
            focused: false,
            pane_ids: Vec::new(),
            window_labels: Vec::new(),
        }
    }

    pub fn set_label(&self, id: &str, label: Option<String>) -> Result<TerminalDescriptor> {
        let resolved = self.resolve_id(id)?;
        if let Some(ref requested) = label {
            if requested.trim().is_empty() {
                return Err(anyhow!("terminal label must not be empty"));
            }
            for entry in self.sessions.iter() {
                if entry.key() != &resolved
                    && !entry.value().exited.load(Ordering::Acquire)
                    && entry
                        .value()
                        .label
                        .lock()
                        .ok()
                        .and_then(|value| value.clone())
                        .as_ref()
                        == Some(requested)
                {
                    return Err(anyhow!("terminal label already in use: {requested}"));
                }
            }
        }
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal"))?;
        *session
            .label
            .lock()
            .map_err(|_| anyhow!("terminal label lock poisoned"))? = label;
        Ok(self.describe(&resolved, &session))
    }

    pub fn acquire_lease(&self, id: &str, agent_id: String, ttl_ms: u64) -> Result<(String, u64)> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal"))?;
        let now = now_ms();
        let mut lease = session
            .lease
            .lock()
            .map_err(|_| anyhow!("terminal lease lock poisoned"))?;
        if lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > now && lease.agent_id != agent_id)
        {
            return Err(anyhow!("terminal is leased by another agent"));
        }
        let token = Uuid::new_v4().to_string();
        let expires_at = now + ttl_ms.clamp(1_000, 600_000);
        *lease = Some(TerminalLease {
            agent_id,
            token: token.clone(),
            expires_at,
        });
        Ok((token, expires_at))
    }

    pub fn release_lease(&self, id: &str, token: &str) -> Result<()> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal"))?;
        let mut lease = session
            .lease
            .lock()
            .map_err(|_| anyhow!("terminal lease lock poisoned"))?;
        if lease.as_ref().is_some_and(|lease| lease.token != token) {
            return Err(anyhow!("terminal lease token does not match"));
        }
        *lease = None;
        Ok(())
    }

    pub fn controlled_write(
        &self,
        id: &str,
        data: &[u8],
        expected_pty_id: Option<&str>,
        actor: Option<&str>,
        lease_token: Option<&str>,
        dry_run: bool,
    ) -> Result<TerminalDescriptor> {
        if data.len() > MAX_CONTROL_WRITE_BYTES {
            return Err(anyhow!("terminal write exceeds 1 MB"));
        }
        let resolved = self.resolve_id(id)?;
        if expected_pty_id.is_some_and(|expected| expected != resolved) {
            return Err(anyhow!(
                "terminal PTY changed; expected {}, found {resolved}",
                expected_pty_id.unwrap_or_default()
            ));
        }
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal"))?
            .clone();
        if session.exited.load(Ordering::Acquire) {
            return Err(anyhow!("terminal has exited"));
        }
        if let Ok(mut lease) = session.lease.lock() {
            if lease
                .as_ref()
                .is_some_and(|lease| lease.expires_at <= now_ms())
            {
                *lease = None;
            }
            if let Some(active) = lease.as_ref() {
                if lease_token != Some(active.token.as_str()) {
                    return Err(anyhow!("terminal is leased by {}", active.agent_id));
                }
            }
        }
        if !dry_run {
            self.write(&resolved, data)?;
        }
        self.record_audit(TerminalAuditEntry {
            ts: now_ms(),
            terminal_id: session.terminal_id.clone(),
            pty_id: resolved.clone(),
            actor: actor.map(str::to_string),
            action: "send".to_string(),
            byte_count: data.len(),
            dry_run,
        });
        Ok(self.describe(&resolved, &session))
    }

    fn record_audit(&self, entry: TerminalAuditEntry) {
        if let Ok(mut audit) = self.audit.lock() {
            if audit.len() >= AUDIT_CAP {
                audit.pop_front();
            }
            audit.push_back(entry);
        }
    }

    pub fn audit_entries(&self) -> Vec<TerminalAuditEntry> {
        self.audit
            .lock()
            .map(|audit| audit.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn wait_for_output(
        &self,
        id: &str,
        after: u64,
        pattern: Option<&regex::Regex>,
        idle_ms: Option<u64>,
        timeout: Duration,
    ) -> Result<(OutputSnapshot, bool, bool, Option<i32>)> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal"))?
            .clone();
        let started = tokio::time::Instant::now();
        loop {
            let (snapshot, exited, exit_code) = self.read_output(&resolved, Some(after), None)?;
            let text = String::from_utf8_lossy(&snapshot.data);
            let matched = pattern.is_some_and(|pattern| pattern.is_match(&text));
            let idle = idle_ms.is_some_and(|idle_ms| {
                now_ms().saturating_sub(session.last_output_at.load(Ordering::Relaxed)) >= idle_ms
            });
            if matched || idle || exited {
                return Ok((snapshot, matched, idle, exit_code));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Ok((snapshot, false, false, exit_code));
            }
            let idle_remaining = idle_ms.map(|idle_ms| {
                let elapsed =
                    now_ms().saturating_sub(session.last_output_at.load(Ordering::Relaxed));
                Duration::from_millis(idle_ms.saturating_sub(elapsed))
            });
            let wake_after = idle_remaining.map_or(remaining, |idle| idle.min(remaining));
            if tokio::time::timeout(wake_after, session.output_notify.notified())
                .await
                .is_err()
                && wake_after == remaining
            {
                let (snapshot, _, exit_code) = self.read_output(&resolved, Some(after), None)?;
                return Ok((snapshot, false, false, exit_code));
            }
        }
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| anyhow!("unknown pty id: {id}"))?
            .clone();
        let mut writer = session
            .writer
            .lock()
            .map_err(|_| anyhow!("writer lock poisoned"))?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<()> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| anyhow!("unknown pty id: {id}"))?
            .clone();
        let master = session
            .master
            .lock()
            .map_err(|_| anyhow!("master lock poisoned"))?;
        master.resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        if let Ok(mut screen) = session.screen.lock() {
            screen.set_size(rows.max(1), cols.max(1));
        }
        Ok(())
    }

    /// PID of the shell process attached to this PTY (the process-group leader
    /// of the slave side). Used by `foreground::lookup` to find what the user
    /// has running in the terminal at the moment. Returns `None` when the
    /// session has died or the platform doesn't surface a PGL.
    pub fn process_group_leader(&self, id: &str) -> Option<i32> {
        let session = self.sessions.get(id)?.clone();
        let master = session.master.lock().ok()?;
        master_process_group_leader(&**master)
    }

    /// Look up the foreground command running in this PTY (the process whose
    /// PGID matches the controlling terminal's foreground PG). Returns `None`
    /// when the session has died or the platform isn't yet supported.
    pub fn foreground(&self, id: &str) -> Option<foreground::ForegroundProcess> {
        let pid = self.process_group_leader(id)?;
        foreground::lookup(pid)
    }

    /// True when the foreground command for this PTY is `claude` (or a
    /// `claude-*` variant). The routing dispatcher uses this to filter PTYs
    /// when picking the active claude session for pin delivery.
    pub fn is_claude_foreground(&self, id: &str) -> bool {
        match self.process_group_leader(id) {
            Some(pid) => foreground::is_claude(pid),
            None => false,
        }
    }

    /// Snapshot the foreground command for every live PTY. Used by the
    /// routing dispatcher to pick the most-recently-touched claude PTY when
    /// dispatching a pin. The map is keyed by PTY id (the same id `pty_spawn`
    /// returns); only PTYs whose lookup succeeded appear in the result.
    pub fn foreground_snapshot(&self) -> HashMap<String, foreground::ForegroundProcess> {
        let mut out = HashMap::new();
        for entry in self.sessions.iter() {
            if let Some(pid) = entry
                .value()
                .master
                .lock()
                .ok()
                .and_then(|m| master_process_group_leader(&**m))
            {
                if let Some(fg) = foreground::lookup(pid) {
                    out.insert(entry.key().clone(), fg);
                }
            }
        }
        out
    }

    /// Begin an attach handshake: snapshot the trailing scrollback AND gate the
    /// stream, under one lock.
    ///
    /// This is the atomic half of the attach seam. Because the emitter holds the
    /// same lock across its sink call, at the instant this returns:
    ///   - every byte in the ring has already been delivered to existing
    ///     listeners (nothing is mid-flight), and
    ///   - no further byte will be delivered to anyone until `attach_arm`.
    ///
    /// The caller registers its listener in that quiet window and then arms;
    /// the flush is the first chunk it sees, starting exactly at `end_offset`.
    /// There is no interval in which a byte is both in the snapshot and on the
    /// live stream, which is why the frontend needs no offset dedup.
    ///
    /// The returned bytes are the last ≤`SCROLLBACK_CAP` emitted (first byte's
    /// absolute offset is `end_offset - data.len()`). `None` once the session
    /// has exited and been reaped.
    ///
    /// An abandoned prior gate (a window that died mid-handshake) is flushed
    /// rather than inherited, so a stale handshake can't stall the stream.
    pub fn attach_begin(&self, id: &str) -> Option<AttachSnapshot> {
        let session = self.sessions.get(id)?.clone();
        let mut st = session.emit.lock().ok()?;
        if let Some(prev) = st.gate.take() {
            tracing::warn!("pty attach gate replaced before arm; flushing held bytes");
            if !prev.held.is_empty() {
                let end = st.ring.total;
                // Both consumers, exactly as `attach_arm` and the overflow
                // path do. Flushing only to the sink drops these bytes from
                // every WebSocket client for good — the gate holds back the
                // broadcast too, so nothing else will ever replay them.
                let _ = session.broadcast_tx.send(prev.held.clone());
                (session.sink)(&prev.held, end);
            }
        }
        let (data, end_offset) = st.ring.snapshot();
        let token = NEXT_ATTACH_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        st.gate = Some(AttachGate {
            token,
            held: Vec::new(),
        });
        Some(AttachSnapshot {
            data,
            end_offset,
            token,
        })
    }

    /// Release the gate installed by `attach_begin`, flushing everything the
    /// session emitted during the handshake as one chunk. Returns `false` if
    /// the session is gone or the gate was already released (a stale token —
    /// e.g. the watchdog beat the caller to it), which callers treat as a
    /// no-op rather than an error.
    pub fn attach_arm(&self, id: &str, token: u64) -> bool {
        let Some(session) = self.sessions.get(id).map(|s| s.clone()) else {
            return false;
        };
        let Ok(mut st) = session.emit.lock() else {
            return false;
        };
        if st.gate.as_ref().map(|g| g.token) != Some(token) {
            return false;
        }
        let gate = st.gate.take().expect("token matched a present gate");
        if !gate.held.is_empty() {
            let end = st.ring.total;
            let _ = session.broadcast_tx.send(gate.held.clone());
            (session.sink)(&gate.held, end);
        }
        true
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        let session = match self.sessions.get(id) {
            Some(s) => s.clone(),
            // Already gone — kill is idempotent.
            None => return Ok(()),
        };
        session
            .killed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut killer) = session.child_killer.lock() {
            let _ = killer.kill();
        }
        #[cfg(windows)]
        {
            if let Some(pid) = session.pid {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .creation_flags(0x08000000)
                    .output();
            }
        }
        // The wait thread will remove the entry once the child reaps.
        Ok(())
    }

    /// Exit state of a session: `None` while running, `Some(code)` once the
    /// child has been waited on.
    ///
    /// An exited session stays in the map for `EXITED_RETENTION` so its
    /// scrollback and exit code remain readable, which means "the session is
    /// gone from `sessions`" and "the shell exited" are different questions.
    /// Callers that need the second one must ask it here rather than inferring
    /// it from a dropped channel.
    pub fn exit_status(&self, id: &str) -> Result<Option<i32>> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal: {id}"))?;
        Ok(session
            .exited
            .load(Ordering::Acquire)
            .then(|| session.exit_code.load(Ordering::Relaxed)))
    }

    /// Resolve once the session has exited, yielding its exit code.
    ///
    /// Returns `None` if the session is unknown (already reaped). The waiter is
    /// registered before the state is re-checked, so an exit landing between
    /// the two cannot be missed.
    pub async fn wait_for_exit(&self, id: &str) -> Option<i32> {
        let resolved = self.resolve_id(id).ok()?;
        let session = self.sessions.get(&resolved)?.clone();
        loop {
            if session.exited.load(Ordering::Acquire) {
                return Some(session.exit_code.load(Ordering::Relaxed));
            }
            let notified = session.output_notify.notified();
            if session.exited.load(Ordering::Acquire) {
                return Some(session.exit_code.load(Ordering::Relaxed));
            }
            notified.await;
        }
    }

    /// Subscribe to live output bytes emitted by the given terminal.
    pub fn subscribe(&self, id: &str) -> Result<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        let resolved = self.resolve_id(id)?;
        let session = self
            .sessions
            .get(&resolved)
            .ok_or_else(|| anyhow!("unknown terminal: {id}"))?;
        Ok(session.broadcast_tx.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// A steady producer: 60 distinct lines over ~700ms, so output is genuinely
    /// in flight while the attach handshake runs.
    fn producer(lines: usize) -> Vec<String> {
        vec![
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "i=0; while [ $i -lt {lines} ]; do echo \"pty-seam-line-$i\"; \
                 sleep 0.01; i=$((i+1)); done"
            ),
        ]
    }

    /// The T-1 claim, stated as a test: **no bytes are duplicated or dropped at
    /// the attach seam.**
    ///
    /// Drives a real PTY that is actively emitting, performs a real attach
    /// handshake against it mid-stream, and asserts the byte sequence an
    /// attaching subscriber ends up with (snapshot ++ everything its listener
    /// received) is byte-identical to the session's actual emitted stream.
    ///
    /// Guards against a vacuous pass: it asserts the gate genuinely held bytes
    /// back during the handshake window, and includes a negative control
    /// reproducing the OLD listen-then-snapshot ordering, which must duplicate.
    /// `wait_for_exit` must resolve on the shell actually exiting, NOT on the
    /// session leaving the map.
    ///
    /// An exited session is retained for `EXITED_RETENTION` (ten minutes) so
    /// its scrollback stays readable, which means its `broadcast_tx` is still
    /// alive that whole time. Code that inferred "exited" from a closed
    /// broadcast channel — as `pty_ws` originally did — saw nothing for ten
    /// minutes and left remote clients reconnecting to a finished shell.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_for_exit_resolves_on_exit_not_on_reap() {
        let mgr = Arc::new(PtyManager::new());
        let id = mgr
            .spawn_headless(SpawnOpts {
                terminal_id: None,
                title: None,
                cwd: "/".into(),
                cmd: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
                env: HashMap::new(),
                rows: 24,
                cols: 80,
            })
            .await
            .expect("spawn");

        let code = tokio::time::timeout(Duration::from_secs(10), mgr.wait_for_exit(&id))
            .await
            .expect("wait_for_exit must resolve well before the retention window");
        assert_eq!(
            code,
            Some(7),
            "the real exit status has to survive the trip"
        );

        // Still in the map, still subscribable — proving the resolution above
        // was driven by the exit itself and not by the entry being removed.
        assert_eq!(mgr.exit_status(&id).expect("still retained"), Some(7));
        assert!(
            mgr.subscribe(&id).is_ok(),
            "an exited session is retained, so its broadcast sender is still alive"
        );

        // Already-exited sessions resolve immediately rather than hanging.
        let again = tokio::time::timeout(Duration::from_secs(2), mgr.wait_for_exit(&id))
            .await
            .expect("must not block once the exit has already happened");
        assert_eq!(again, Some(7));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn attach_seam_delivers_the_stream_exactly_once() {
        let mgr = Arc::new(PtyManager::new());

        // Ground truth: every chunk the session delivers, in order. This is
        // exactly what the origin pane's listener sees.
        let log: Arc<Mutex<Vec<(Vec<u8>, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let exited = Arc::new(AtomicBool::new(false));

        let log_sink = log.clone();
        let data: DataSink = Arc::new(move |bytes: &[u8], end: u64| {
            log_sink.lock().unwrap().push((bytes.to_vec(), end));
        });
        let exit_flag = exited.clone();
        let exit: ExitSink = Box::new(move |_code| exit_flag.store(true, Ordering::SeqCst));

        let id = mgr
            .spawn_with_sinks(
                SpawnOpts {
                    terminal_id: None,
                    title: None,
                    cwd: "/".into(),
                    cmd: producer(60),
                    env: HashMap::new(),
                    rows: 24,
                    cols: 80,
                },
                data,
                exit,
            )
            .await
            .expect("spawn");

        // Let real output accumulate so the ring is non-trivial.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (direct_snapshot, direct_end) = mgr.read_scrollback(&id).expect("session is live");
        assert!(!direct_snapshot.is_empty());
        assert!(direct_end >= direct_snapshot.len() as u64);

        // Negative control mark: where a listener registered BEFORE the
        // snapshot would start receiving (the pre-T-1 ordering).
        let naive_mark = log.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(60)).await;

        // --- the handshake ---
        let snap = mgr.attach_begin(&id).expect("session is live");
        assert!(
            !snap.data.is_empty(),
            "ring should already hold output at attach time"
        );

        // The attaching window registers its listener here. Under the gate the
        // stream is quiet, which is what makes this window safe.
        let mark = log.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            log.lock().unwrap().len(),
            mark,
            "gate must deliver nothing while the listener is registering"
        );

        assert!(
            mgr.attach_arm(&id, snap.token),
            "arm should release the gate"
        );

        {
            let entries = log.lock().unwrap();
            let flush = entries.get(mark).expect("arm should flush a chunk");
            assert!(
                !flush.0.is_empty(),
                "no bytes were emitted during the handshake — the test would be vacuous"
            );
            // The flush starts exactly where the snapshot ended: no gap, no lap.
            assert_eq!(
                flush.1 - flush.0.len() as u64,
                snap.end_offset,
                "flush must start at the snapshot's end offset"
            );
        }

        // Drain to completion.
        for _ in 0..200 {
            if exited.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let _ = mgr.kill(&id);

        let entries = log.lock().unwrap().clone();
        let truth: Vec<u8> = entries
            .iter()
            .flat_map(|(b, _)| b.iter().copied())
            .collect();

        // What the attaching subscriber actually ends up with.
        let mut attached: Vec<u8> = snap.data.clone();
        for (b, _) in &entries[mark..] {
            attached.extend_from_slice(b);
        }

        assert_eq!(
            attached.len(),
            truth.len(),
            "attach seam duplicated or dropped bytes (attached {} vs stream {})",
            attached.len(),
            truth.len()
        );
        assert!(
            attached == truth,
            "attach seam produced a different byte sequence than the stream"
        );

        // Byte-exact against the source: every line the shell printed appears
        // exactly once in the attaching subscriber's view.
        let text = String::from_utf8_lossy(&attached).into_owned();
        for i in 0..60usize {
            assert_eq!(
                text.matches(&format!("pty-seam-line-{i}\r\n")).count(),
                1,
                "line {i} should appear exactly once in the attached view"
            );
        }

        // Negative control: the pre-T-1 ordering (subscribe first, snapshot
        // after) re-covers everything emitted in between. If this does NOT
        // over-count, the timing was too tight for the assertion above to have
        // been meaningful.
        let mut naive: Vec<u8> = snap.data.clone();
        for (b, _) in &entries[naive_mark..] {
            naive.extend_from_slice(b);
        }
        assert!(
            naive.len() > truth.len(),
            "negative control: listen-then-snapshot should duplicate the overlap"
        );
    }

    /// The watchdog path: a window that begins a handshake and dies must not
    /// stall the terminal. `attach_arm` with the same token releases it, and a
    /// second `attach_begin` supersedes (and flushes) an abandoned gate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn abandoned_gate_is_released_by_a_superseding_attach() {
        let mgr = Arc::new(PtyManager::new());
        let log: Arc<Mutex<Vec<(Vec<u8>, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let log_sink = log.clone();
        let data: DataSink = Arc::new(move |bytes: &[u8], end: u64| {
            log_sink.lock().unwrap().push((bytes.to_vec(), end));
        });

        let id = mgr
            .spawn_with_sinks(
                SpawnOpts {
                    terminal_id: None,
                    title: None,
                    cwd: "/".into(),
                    cmd: producer(40),
                    env: HashMap::new(),
                    rows: 24,
                    cols: 80,
                },
                data,
                Box::new(|_| {}),
            )
            .await
            .expect("spawn");

        tokio::time::sleep(Duration::from_millis(150)).await;
        let abandoned = mgr.attach_begin(&id).expect("live");
        tokio::time::sleep(Duration::from_millis(120)).await;

        // Second window attaches; the abandoned gate's held bytes are flushed
        // rather than lost, and the stale token no longer arms anything.
        let before = log.lock().unwrap().len();
        let second = mgr.attach_begin(&id).expect("live");
        assert!(
            log.lock().unwrap().len() > before,
            "superseding attach should flush the abandoned gate"
        );
        assert!(
            !mgr.attach_arm(&id, abandoned.token),
            "stale token is a no-op"
        );
        assert!(mgr.attach_arm(&id, second.token));

        // Stream is live again.
        let n = log.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            log.lock().unwrap().len() > n,
            "stream should flow again once the gate is released"
        );
        let _ = mgr.kill(&id);
    }

    #[test]
    fn cursor_reads_report_retention_truncation() {
        let mut ring = ScrollbackRing::new();
        let data = vec![b'x'; SCROLLBACK_CAP + 32];
        ring.push(&data);
        let snapshot = ring.read_after(Some(0), None);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.available_start_offset, 32);
        assert_eq!(snapshot.start_offset, 32);
        assert_eq!(snapshot.end_offset, (SCROLLBACK_CAP + 32) as u64);
        assert_eq!(snapshot.data.len(), SCROLLBACK_CAP);
    }

    #[test]
    fn vt_parser_exposes_current_screen_instead_of_raw_history() {
        let mut parser = vt100::Parser::new(2, 8, 0);
        parser.process(b"first\rsecond");
        let contents = parser.screen().contents();
        assert!(contents.contains("second"));
        assert!(!contents.contains("first"));
    }
}
