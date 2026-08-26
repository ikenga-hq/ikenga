//! Session-as-object model. A `Session` is a chat thread identified by a
//! stable, frontend-minted `thread_id` (uuid). It owns:
//!
//!   * an optional streaming-input claude child (`claude --print
//!     --input-format stream-json --output-format stream-json --verbose`)
//!     for chat turns, and
//!   * an optional PTY (`claude --resume <claude_session_id>` or `bash`)
//!     for "open this conversation in a terminal" affordances.
//!
//! Both transports parse into the same `ChatEvent` stream and emit on the
//! single channel `session://{thread_id}`. The frontend never sees Claude's
//! internal session id except as metadata it can display.
//!
//! Why one object instead of the two parallel maps we had before:
//!   * removes the placeholder-id / real-id alias dance — `thread_id` is the
//!     same before and after `system:init` fires.
//!   * makes Chat | Terminal a property of one session, not two unrelated
//!     things sharing a key.
//!   * lets the route URL stay stable across the placeholder→real transition,
//!     so React doesn't remount and the in-memory event buffer survives.
//!
//! Events: every parsed event is emitted on `session://{thread_id}`. We also
//! mirror to `claude://session/{real_session_id}` once known, so legacy
//! listeners (e.g. live-sessions store keyed on Claude id) keep working until
//! they migrate.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::Arc;
// Only the unix shutdown path (and tests) use this.
#[cfg_attr(windows, allow(unused_imports))]
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};

use crate::claude::{
    artifact_watcher::ArtifactWatcher, event::ChatEvent, stream_parser::StreamParser,
};
use crate::engines::claude_code::mode::AcpSessionMode;
use crate::engines::claude_code::prompt::PromptContent;

/// Options passed to `session_ensure` / `session_send`. Mirrors the subset of
/// claude CLI flags we expose; deliberately narrow.
///
/// Phase 5: `permission_mode` is now an `AcpSessionMode` (typed enum), used
/// both as the initial value for `--permission-mode` on spawn and as the
/// in-memory tracked mode threaded through to `send_set_mode` for runtime
/// switches. The legacy free-form `permissionMode` string from the
/// `session_ensure` Tauri command still deserializes via the enum's
/// camelCase Serde derive (`plan` / `default` / `auto` / `bypassPermissions`).
/// ADR-011 phase 3: 5-step extended-thinking effort control. Maps to
/// claude CLI's `--thinking-budget-tokens` flag at spawn time. `Off` omits
/// the flag entirely so claude defaults apply. Per-turn switching is
/// deferred — changes mutate `SessionOpts.effort` and take effect on the
/// next spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffortLevel {
    #[default]
    Off,
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    /// Token budget passed via `--thinking-budget-tokens`. `Off` returns
    /// `None` so the caller omits the flag.
    pub fn thinking_budget_tokens(&self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(1_000),
            Self::Medium => Some(4_000),
            Self::High => Some(16_000),
            Self::Max => Some(32_000),
        }
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct SessionOpts {
    /// Resume an existing Claude session by its on-disk id. Maps to
    /// `--resume <id>` on spawn.
    #[serde(rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    /// Initial permission mode passed via `--permission-mode` on spawn.
    /// Runtime changes happen via `engines::claude_code::server::handle_set_mode` →
    /// `send_set_mode`.
    #[serde(rename = "permissionMode")]
    pub permission_mode: AcpSessionMode,
    pub model: Option<String>,
    /// ADR-011 phase 3: extended-thinking effort. Applied on next spawn
    /// via `--thinking-budget-tokens`. Mutated by `acp_set_effort`.
    #[serde(default)]
    pub effort: EffortLevel,
}

/// The live streaming child owned by a session, if one is currently spawned.
pub struct StreamingChild {
    /// Held so we can kill the child on cancel/destroy.
    child: Mutex<Child>,
    /// Held so `session_send` can write follow-up user envelopes.
    stdin: Mutex<ChildStdin>,
}

/// Per-session live state. One entry per chat thread the user has touched in
/// this run; created lazily on first `session_ensure` and cleared by
/// `session_destroy` or when the streaming child exits.
pub struct Session {
    pub thread_id: String,
    pub cwd: String,
    pub opts: Mutex<SessionOpts>,
    /// Set once the parser sees the first `system:init` event. Used to mirror
    /// events on `claude://session/{real_session_id}` and to look up the
    /// on-disk JSONL when the frontend reopens later.
    pub claude_session_id: Mutex<Option<String>>,
    /// Streaming child for chat turns. Absent until first `session_send` or
    /// an explicit prompt-on-ensure spawn. `pub(crate)` so the ACP server
    /// can short-circuit on "no live child" in `handle_set_mode` without
    /// going through a fresh helper method. The lock itself is held only
    /// for the read in that path — never held across an await on stdin.
    pub(crate) streaming: Mutex<Option<Arc<StreamingChild>>>,
    /// PTY id (from `PtyManager`) if the session has an attached terminal.
    pub pty_id: Mutex<Option<String>>,
    /// Broadcast channel for parsed `ChatEvent`s observed on this session.
    /// The existing reader task (in `spawn_streaming`) sends every event here
    /// in addition to the `app.emit("session://...")` call so in-process
    /// subscribers (e.g. the ACP `handle_prompt` end-of-turn waiter) can
    /// observe the stream without going through the Tauri event bus.
    /// Capacity is generous because a single prompt can fan out many text
    /// chunks; lagging subscribers will see `RecvError::Lagged` which
    /// `handle_prompt` treats as fatal for the turn.
    pub events: broadcast::Sender<ChatEvent>,
    /// Phase 5: tracked current session mode. Initialized from
    /// `opts.permission_mode`. Updated by `engines::claude_code::server::handle_set_mode`,
    /// which also writes a `set_permission_mode` control_request to claude's
    /// stdin if a streaming child is live. If no child is live, the next
    /// `spawn_streaming` picks up the new mode via the `--permission-mode`
    /// flag — `send_user_message` snapshots this into `opts.permission_mode`
    /// before spawning.
    pub current_mode: Mutex<AcpSessionMode>,
    /// Phase 5: per-session CLAUDE_PROJECT_DIR — the project's `root_path`
    /// for the project this session is attached to. Used by claude skills
    /// + commands that reference `${CLAUDE_PROJECT_DIR}` even when cwd has
    /// been changed by an `--add-dir`.
    pub claude_project_dir: Mutex<Option<String>>,
    /// tool_use ids we've already answered via `send_tool_result` this run.
    /// A `tool_use` must be answered exactly once; this guard makes a duplicate
    /// `session_tool_result` (e.g. an interactive `AskUserQuestion` form that
    /// got resurfaced by a UI remount/race) a harmless no-op instead of
    /// shipping a second `tool_result` into claude's transcript. Authoritative
    /// because the renderer's own state can't always know what was already
    /// sent.
    answered_tool_uses: Mutex<HashSet<String>>,

    /// Which control-protocol wire shape to speak when WE initiate an
    /// outbound control_request (set_permission_mode / interrupt). Detected
    /// from the `claude_code_version` in the session-init envelope (set in
    /// `engines::claude_code::server::handle_prompt`). Defaults to `Modern`
    /// (current claude); downgraded to `Legacy` only for pre-2.1 builds.
    /// claude 2.1.x silently ignores the legacy `sdk_control_request` shape
    /// for these subtypes, so getting this right is what makes mid-session
    /// mode switches actually take effect.
    pub control_wire: Mutex<ControlWire>,
}

impl Session {
    pub fn new(thread_id: String, cwd: String, opts: SessionOpts) -> Self {
        // 1024 outstanding events should comfortably absorb the burstiest
        // assistant turns; chosen empirically — `cargo bench` not warranted
        // until we see a real lag complaint.
        let (events, _) = broadcast::channel(1024);
        let initial_mode = opts.permission_mode;
        Self {
            thread_id,
            cwd,
            opts: Mutex::new(opts),
            claude_session_id: Mutex::new(None),
            streaming: Mutex::new(None),
            pty_id: Mutex::new(None),
            events,
            current_mode: Mutex::new(initial_mode),
            claude_project_dir: Mutex::new(None),
            answered_tool_uses: Mutex::new(HashSet::new()),
            control_wire: Mutex::new(ControlWire::Modern),
        }
    }

    /// Set the spawn-time project root for this session. Called from
    /// `engines::claude_code::server::handle_new_session` after resolving the
    /// project, before the first `spawn_streaming`. Idempotent — re-calling
    /// overwrites, which is fine for the resume path (load / fork) once those
    /// wire it in too.
    ///
    /// This used to also carry a per-session `CLAUDE_CONFIG_DIR` overlay path
    /// (D-13, `plans/2026-07-18-transcripts-and-terminal-architecture/07-retire-the-overlay.md`).
    /// The overlay was retired: it resolved a strictly SMALLER asset set than
    /// claude's own native discovery, and it hid chat transcripts from
    /// `claude --resume`.
    pub async fn set_claude_project_dir(&self, project_dir: Option<String>) {
        *self.claude_project_dir.lock().await = project_dir;
    }
}

/// Sessions are keyed by `thread_id`. Distinct from the legacy
/// `ClaudeManager.by_placeholder` map: there's no placeholder concept here,
/// because the id is minted by the frontend and stays stable.
#[derive(Default)]
pub struct SessionsManager {
    by_thread: Mutex<HashMap<String, Arc<Session>>>,
}

impl SessionsManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get_or_create(
        &self,
        thread_id: &str,
        cwd: &str,
        opts: SessionOpts,
    ) -> Arc<Session> {
        let mut guard = self.by_thread.lock().await;
        if let Some(s) = guard.get(thread_id) {
            return s.clone();
        }
        let s = Arc::new(Session::new(thread_id.to_string(), cwd.to_string(), opts));
        guard.insert(thread_id.to_string(), s.clone());
        s
    }

    pub async fn get(&self, thread_id: &str) -> Option<Arc<Session>> {
        self.by_thread.lock().await.get(thread_id).cloned()
    }

    pub async fn remove(&self, thread_id: &str) -> Option<Arc<Session>> {
        self.by_thread.lock().await.remove(thread_id)
    }

    /// HMR / cold-start hygiene: shut down every streaming child we know
    /// about. Called from `session_destroy_all` on window 'beforeunload' so
    /// dev reloads don't leave zombies. PTYs are owned by `PtyManager`; this
    /// only touches streaming children.
    ///
    /// This used to call `child.start_kill()` — SIGKILL on Unix — which gave
    /// claude no window to flush its `.jsonl` transcript to disk. Since
    /// `session_destroy_all` is wired to `beforeunload`, that meant EVERY
    /// Vite HMR reload could destroy the transcript of any turn in flight.
    /// It now goes through `shutdown_child_gracefully` (SIGTERM → bounded
    /// drain → SIGKILL fallback), which preserves the anti-orphan guarantee
    /// while giving claude time to finish writing.
    pub async fn kill_all_streaming(&self) {
        let snapshot: Vec<Arc<Session>> = {
            let guard = self.by_thread.lock().await;
            guard.values().cloned().collect()
        };
        // Drain concurrently. Each child gets its own bounded grace window +
        // bounded SIGKILL-reap window (see `shutdown_child_gracefully`'s doc
        // comment for the real total-per-child worst case, ~GRACEFUL_SHUTDOWN_
        // TIMEOUT + SIGKILL_REAP_TIMEOUT), so total shutdown latency for THIS
        // function is that same bound regardless of session count, not
        // N × that. `beforeunload` sits on the critical path of every reload,
        // so the wall-clock cost has to stay flat in session count.
        let drains = snapshot.into_iter().map(|s| async move {
            // Take the Arc out of the slot first: that makes us the sole
            // owner of the `StreamingChild` for the duration of the drain,
            // so the `.kill_on_drop(true)` set at spawn time cannot fire
            // underneath us and turn the grace window back into a SIGKILL.
            let taken = s.streaming.lock().await.take();
            if let Some(c) = taken {
                let mut child = c.child.lock().await;
                shutdown_child_gracefully(&mut child, &s.thread_id).await;
            }
        });
        futures_util::future::join_all(drains).await;
    }
}

/// How long a claude child gets to flush its transcript after SIGTERM before
/// we escalate to SIGKILL.
///
/// 2s is chosen to be comfortably longer than a `.jsonl` append + fsync on a
/// warm page cache, and short enough that a reload with several live sessions
/// still feels instant (the drains run concurrently, so this is the total
/// worst case, not per-session). If field logs start showing the escalation
/// `warn!` below, raise this rather than reverting to a hard kill.
///
/// Only referenced from the unix path (fn + tests below) — non-unix has no
/// grace window, so this would otherwise be `dead_code` on a Windows build.
#[cfg(unix)]
const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound on the `wait()` after the SIGKILL fallback. SIGKILL cannot be
/// blocked or ignored by the target, so reaping after it should be near-
/// instant; this exists only to guarantee `shutdown_child_gracefully` itself
/// is bounded even in the practically-unreachable case where `start_kill()`
/// also fails to deliver (e.g. the earlier `libc::kill` EPERM branch, where a
/// same-uid signal we sent was rejected — SIGKILL would fail the same way).
#[cfg(unix)]
const SIGKILL_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Terminate a claude child, giving it a bounded chance to exit cleanly first.
///
/// Unix: SIGTERM → wait up to [`GRACEFUL_SHUTDOWN_TIMEOUT`] → SIGKILL → wait
/// up to [`SIGKILL_REAP_TIMEOUT`]. Total worst case is therefore the sum of
/// both bounds (≈4s today), not just the SIGTERM grace window — the function
/// itself is fully bounded. On the (practically unreachable) path where even
/// the post-SIGKILL reap times out, this logs a `warn!` and returns rather
/// than blocking shutdown forever; the anti-orphan guarantee degrades from
/// "always reaped" to "always attempted and logged" in that one case.
///
/// `thread_id` is only used for log correlation.
#[cfg(unix)]
async fn shutdown_child_gracefully(child: &mut Child, thread_id: &str) {
    // Already exited (common — claude usually finishes its turn and EOFs
    // before we get here). Reap and return without signalling anything.
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }

    let Some(pid) = child.id() else {
        // `id()` is None only once the child has been reaped by `wait()`.
        return;
    };

    // SAFETY: `pid` comes from a child we spawned and have not yet reaped, so
    // it cannot have been recycled by the OS. `kill(2)` with SIGTERM has no
    // preconditions beyond a valid pid.
    let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if sent != 0 {
        // ESRCH (already gone) or EPERM. Nothing to be gained by waiting.
        tracing::warn!(
            target: "ikenga::claude::session",
            thread_id,
            pid,
            errno = std::io::Error::last_os_error().raw_os_error(),
            "SIGTERM to claude child failed; escalating to SIGKILL",
        );
    } else {
        match tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => {
                tracing::info!(
                    target: "ikenga::claude::session",
                    thread_id,
                    pid,
                    status = %status,
                    "claude child exited gracefully after SIGTERM; transcript had a flush window",
                );
                return;
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    target: "ikenga::claude::session",
                    thread_id,
                    pid,
                    error = %e,
                    "wait() on claude child failed after SIGTERM; escalating to SIGKILL",
                );
            }
            Err(_) => {
                tracing::warn!(
                    target: "ikenga::claude::session",
                    thread_id,
                    pid,
                    timeout_ms = GRACEFUL_SHUTDOWN_TIMEOUT.as_millis(),
                    "claude child did not exit within the grace window; escalating to SIGKILL — its transcript may be truncated",
                );
            }
        }
    }

    // Fallback. This is what guarantees no orphan survives app shutdown —
    // bounded, so a child that also can't be reaped (e.g. the same EPERM
    // that would make `start_kill()`'s SIGKILL fail) can't hang shutdown.
    let _ = child.start_kill();
    match tokio::time::timeout(SIGKILL_REAP_TIMEOUT, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            tracing::warn!(
                target: "ikenga::claude::session",
                thread_id,
                pid,
                timeout_ms = SIGKILL_REAP_TIMEOUT.as_millis(),
                "claude child did not reap within the post-SIGKILL timeout; giving up — this child may be unsignalable and could survive as an orphan",
            );
        }
    }
}

/// Windows variant. There is no SIGTERM: the nearest analogues
/// (`GenerateConsoleCtrlEvent`, posting `WM_CLOSE`) don't reach a console-less
/// child spawned with piped stdio, so there is no portable graceful signal to
/// send. We keep the original hard-kill behaviour here rather than pretend a
/// grace window exists. If Windows transcript loss ever shows up in the field
/// the fix is a job-object / `AttachConsole` + CTRL_BREAK path, not a sleep.
#[cfg(not(unix))]
async fn shutdown_child_gracefully(child: &mut Child, thread_id: &str) {
    if let Ok(Some(_)) = child.try_wait() {
        return;
    }
    tracing::info!(
        target: "ikenga::claude::session",
        thread_id,
        "terminating claude child (no SIGTERM equivalent on this platform)",
    );
    let _ = child.start_kill();
    let _ = child.wait().await;
}

pub type SessionsState = Arc<SessionsManager>;

// ─── Spawn / send / cancel ────────────────────────────────────────────────

/// Build the line-delimited user envelope that streaming-input mode expects:
/// `{"type":"user","message":{"role":"user","content":"<text>"}}\n`.
fn user_envelope(text: &str) -> String {
    let value = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

/// Phase 7: build the line-delimited user envelope for a `PromptContent`.
///
/// When `content.images` is empty this returns the same string-content
/// shape as `user_envelope` so the heavily-exercised text path is byte-
/// identical to its Phase 3 form. Only image-bearing messages take the
/// array-content branch — that keeps wire traces simple, doesn't risk
/// regressing the text path, and matches what claude accepts on both
/// sides (it tolerates array-form for text-only too, but the string form
/// is what it ships in its own SDK).
///
/// Image source shape mirrors what the Anthropic API accepts in
/// stream-json mode:
///   `{"type":"image","source":{"type":"base64","media_type":"...","data":"..."}}`
pub fn build_user_envelope(content: &PromptContent) -> String {
    if content.images.is_empty() {
        return user_envelope(&content.text);
    }
    let mut blocks: Vec<serde_json::Value> = Vec::with_capacity(1 + content.images.len());
    if !content.text.is_empty() {
        blocks.push(serde_json::json!({
            "type": "text",
            "text": content.text,
        }));
    }
    for image in &content.images {
        blocks.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.mime_type,
                "data": image.base64_data,
            },
        }));
    }
    let value = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": blocks,
        },
    });
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

/// Build the line-delimited tool_result envelope. The `output` may be a
/// plain string or a structured value — Anthropic accepts both, the latter
/// is how we ferry back e.g. AskUserQuestion answers without losing shape.
fn tool_result_envelope(tool_use_id: &str, output: &serde_json::Value, is_error: bool) -> String {
    let value = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": output,
                "is_error": is_error,
            }]
        }
    });
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

/// Spawn a streaming-input claude child for this session. The first user
/// envelope is written before the reader task starts (claude buffers stdin
/// until it begins reading, so the order is fine).
pub async fn spawn_streaming(
    app: AppHandle,
    session: Arc<Session>,
    initial_prompt: Option<String>,
) -> Result<(), String> {
    let cwd = session.cwd.clone();
    let resolved_cwd = shellexpand::full(&cwd)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| cwd.clone());

    let opts = session.opts.lock().await.clone();
    let project_dir = session.claude_project_dir.lock().await.clone();
    let mut command = {
        #[cfg(windows)]
        {
            let resolved = which::which_in("claude", Some(crate::runtime::augmented_path()), ".")
                .or_else(|_| which::which_in("claude.cmd", Some(crate::runtime::augmented_path()), "."))
                .or_else(|_| which::which_in("claude.exe", Some(crate::runtime::augmented_path()), "."));
            if let Ok(p) = resolved {
                let is_batch = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
                    .unwrap_or(false);
                if is_batch {
                    let mut cmd = Command::new("cmd.exe");
                    cmd.arg("/c").arg(p);
                    cmd
                } else {
                    Command::new(p)
                }
            } else {
                Command::new("claude")
            }
        }
        #[cfg(not(windows))]
        {
            Command::new("claude")
        }
    };
    command
        // Phase 4: `--permission-prompt-tool stdio` opens the
        // `sdk_control_request` channel on stdout so tool approvals become
        // a real round-trip (see `engines::claude_code::server::handle_prompt`).
        //
        // Phase 5: `--dangerously-skip-permissions` retired. Permission
        // behavior is now driven entirely by `--permission-mode` (initial
        // state) plus stdin `sdk_control_request { subtype:
        // "set_permission_mode" }` envelopes (runtime switches via
        // `send_set_mode` below). The four ACP modes map as:
        //   plan              → claude `plan`
        //   default           → claude `default`
        //   auto              → claude `acceptEdits`
        //   bypassPermissions → claude `bypassPermissions`
        // See `crate::engines::claude_code::mode` for the canonical mapping.
        .arg("--permission-prompt-tool")
        .arg("stdio")
        .arg("--permission-mode")
        .arg(opts.permission_mode.as_claude_flag())
        .arg("--print")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--verbose")
        .current_dir(&resolved_cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Augmented PATH so `claude` (and node it shells out to) resolves
        // under nvm/npm/homebrew even from a thin GUI $PATH (ADR-013 §Addendum
        // Decision 2). Set before the layered project `.env` below so an
        // explicit `PATH=` in a project `.env` still wins.
        .env("PATH", crate::runtime::augmented_path())
        // Anti-orphan backstop: if the `StreamingChild` is dropped without
        // anyone calling `cancel_streaming` / `kill_all_streaming` (e.g. the
        // process is tearing down), tokio SIGKILLs the child on drop.
        //
        // This does NOT defeat the graceful shutdown path: both drain sites
        // `take()` the `Arc<StreamingChild>` out of `Session::streaming`
        // before signalling, so they hold the sole owner for the whole grace
        // window and the drop only happens after the child is already reaped.
        // The only other place that clears the slot is the reader task on
        // EOF, which by definition runs after the child's stdout has closed.
        // Keep this flag — removing it would trade a real orphan risk for
        // nothing.
        .kill_on_drop(true);
    // D-13 (`plans/2026-07-18-transcripts-and-terminal-architecture/07-retire-the-overlay.md`):
    // this spawn deliberately sets NO `CLAUDE_CONFIG_DIR` and passes NO
    // `--mcp-config` / `--strict-mcp-config`. The child uses claude's own
    // native discovery, exactly as a `claude` typed into a terminal in the
    // same cwd would — which means the same skills/agents/commands/MCP set,
    // and a transcript written to `~/.claude/projects` where
    // `claude --resume` can find it.
    //
    // The per-session overlay that used to live here resolved a strictly
    // SMALLER set than native (measured at monorepo root: 129 vs 143 skills,
    // 273 vs 298 commands, 14 vs 23 MCP servers, 50 vs 83 tools) because
    // `--strict-mcp-config` suppressed claude's own MCP discovery in favour
    // of a merged file built from the same `~/.claude.json` it was
    // suppressing. Pkg-contributed MCP servers do NOT depend on the overlay:
    // `pkg::registries::mcp::McpRegistry` writes them straight into
    // `~/.claude.json:mcpServers`, the user-tier file native discovery reads.
    //
    // CLAUDE_PROJECT_DIR stays — it is orthogonal. It does not redirect
    // config discovery or the transcript location; it only supplies the
    // `${CLAUDE_PROJECT_DIR}` substitution used by skills + commands that
    // need the project root even when claude's cwd has been moved by
    // `--add-dir`.
    if let Some(ref pd) = project_dir {
        command.env("CLAUDE_PROJECT_DIR", pd);
    }
    // Phase 7 (projects-first-class): layer workspace + project `.env`
    // files into the claude child env. Process env is already inherited
    // by `Command::new` so we only add the additive layers — workspace
    // first, then project's `.env`, then `.env.local` (last wins). The
    // project root is whatever the session was spawned with (`cwd`),
    // resolved through tilde-expansion above.
    {
        let app_data = app.path().app_data_dir().ok();
        let ws_env = app_data.as_ref().map(|d| d.join("workspace.env"));
        let project_root = std::path::Path::new(&resolved_cwd);
        let layered = crate::env_files::build_layered_env(ws_env.as_deref(), Some(project_root));
        if !layered.is_empty() {
            command.envs(layered);
        }
    }
    // Phase 8: forks seed `resume_session_id` with the SOURCE thread's
    // `claude_session_id` at fork time (see `engines::claude_code::server::handle_fork_session`),
    // so the first prompt on a forked thread resumes against the source's
    // on-disk JSONL transcript. The user effectively continues the same
    // claude conversation in a separate Ikenga thread.
    if let Some(ref id) = opts.resume_session_id {
        command.arg("--resume").arg(id);
    }
    if let Some(ref m) = opts.model {
        command.arg("--model").arg(m);
    }
    // ADR-011 phase 3: extended-thinking effort. `Off` skips the flag so
    // claude's own default applies; the other four steps map to discrete
    // thinking-budget-tokens values (see `EffortLevel::thinking_budget_tokens`).
    if let Some(budget) = opts.effort.thinking_budget_tokens() {
        command
            .arg("--thinking-budget-tokens")
            .arg(budget.to_string());
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn streaming claude: {e}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "stdin pipe missing".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout pipe missing".to_string())?;
    let stderr = child.stderr.take();

    if let Some(p) = initial_prompt {
        let envelope = user_envelope(&p);
        if let Err(e) = stdin.write_all(envelope.as_bytes()).await {
            return Err(format!("initial prompt write: {e}"));
        }
        let _ = stdin.flush().await;
    }

    let streaming = Arc::new(StreamingChild {
        child: Mutex::new(child),
        stdin: Mutex::new(stdin),
    });
    *session.streaming.lock().await = Some(streaming);

    // Reader task: stdout → StreamParser → emit ChatEvents
    let parser = std::sync::Mutex::new(StreamParser::new());
    let watcher = std::sync::Mutex::new(ArtifactWatcher::new());
    let app_reader = app.clone();
    let session_reader = session.clone();
    tauri::async_runtime::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut buf = vec![0u8; 8 * 1024];
        loop {
            use tokio::io::AsyncReadExt;
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let mut events = match parser.lock() {
                        Ok(mut p) => p.feed(chunk),
                        Err(_) => break,
                    };
                    let extras = match watcher.lock() {
                        Ok(mut w) => w.observe(&events),
                        Err(_) => Vec::new(),
                    };
                    events.extend(extras);
                    if events.is_empty() {
                        continue;
                    }
                    // Capture the real Claude session id once.
                    let real_id_now = events.iter().find_map(|e| match e {
                        ChatEvent::SessionInit { session_id, .. } if !session_id.is_empty() => {
                            Some(session_id.clone())
                        }
                        _ => None,
                    });
                    if let Some(real) = real_id_now {
                        let mut guard = session_reader.claude_session_id.lock().await;
                        if guard.is_none() {
                            *guard = Some(real.clone());
                        }
                    }
                    emit_events(&app_reader, &session_reader, &events).await;
                }
                Err(e) => {
                    log::debug!("streaming claude reader closed: {e}");
                    break;
                }
            }
        }
        // EOF → child exited. Drop the streaming handle so the next send
        // re-spawns with --resume.
        *session_reader.streaming.lock().await = None;
    });

    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!("claude stderr: {line}");
            }
        });
    }

    Ok(())
}

/// Emit events on `session://{thread_id}` and (once known) mirror to
/// `claude://session/{real_session_id}` for legacy listeners. Also fans out
/// to the in-process broadcast channel so ACP subscribers (`handle_prompt`'s
/// end-of-turn waiter) observe the same stream.
async fn emit_events(app: &AppHandle, session: &Arc<Session>, events: &[ChatEvent]) {
    let thread_channel = format!("session://{}", session.thread_id);
    for e in events {
        let _ = app.emit(&thread_channel, e);
        // `send` only errors when there are zero active receivers — fine,
        // that's the common case when nobody is listening in-process.
        let _ = session.events.send(e.clone());
    }
    if let Some(real) = session.claude_session_id.lock().await.clone() {
        let mirror = format!("claude://session/{real}");
        for e in events {
            let _ = app.emit(&mirror, e);
        }
    }
}

/// Send a user message to a session's streaming child. Spawns one if absent
/// (recovery after claude crashed or HMR). Returns Ok on success or a
/// human-readable error.
pub async fn send_user_message(
    app: AppHandle,
    session: Arc<Session>,
    text: String,
) -> Result<(), String> {
    let needs_spawn = session.streaming.lock().await.is_none();
    if needs_spawn {
        // Resume the conversation if we know its Claude id.
        let resume_id = session.claude_session_id.lock().await.clone();
        if resume_id.is_some() {
            let mut o = session.opts.lock().await;
            if o.resume_session_id.is_none() {
                o.resume_session_id = resume_id;
            }
        }
        spawn_streaming(app, session.clone(), Some(text.clone())).await?;
        return Ok(());
    }
    let streaming = session
        .streaming
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| "streaming child vanished".to_string())?;
    let envelope = user_envelope(&text);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    Ok(())
}

/// Phase 7: variant of `send_user_message` that accepts a structured
/// `PromptContent` (text + optional images). Text-only payloads delegate
/// straight to `send_user_message` so the legacy hot path is unchanged.
/// Image-bearing payloads build an array-content stream-json envelope
/// (see `build_user_envelope`) and write it to the streaming child, with
/// the same spawn-on-first-turn semantics as `send_user_message`.
pub async fn send_user_message_with_content(
    app: AppHandle,
    session: Arc<Session>,
    content: PromptContent,
) -> Result<(), String> {
    if content.images.is_empty() {
        // No images → preserve the byte-for-byte wire shape the text path
        // has been emitting since Phase 3.
        return send_user_message(app, session, content.text).await;
    }

    let needs_spawn = session.streaming.lock().await.is_none();
    if needs_spawn {
        // First-turn spawn: `spawn_streaming` only knows how to wrap a
        // plain text string into a stream-json envelope. For images we
        // spawn without an initial prompt, then write the prebuilt
        // array-content envelope ourselves on the new stdin. The reader
        // task is already drained-by-then but claude buffers stdin
        // before it starts processing, so order on the write side is
        // what matters and it lines up the same way.
        let resume_id = session.claude_session_id.lock().await.clone();
        if resume_id.is_some() {
            let mut o = session.opts.lock().await;
            if o.resume_session_id.is_none() {
                o.resume_session_id = resume_id;
            }
        }
        spawn_streaming(app, session.clone(), None).await?;
    }

    let streaming = session
        .streaming
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| "streaming child vanished".to_string())?;
    let envelope = build_user_envelope(&content);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    Ok(())
}

/// Send a tool_result envelope to the session's streaming child. Used by
/// interactive tool renderers (e.g. AskUserQuestion) to ferry the user's
/// answer back into Claude's agent loop. Fails if the streaming child is
/// not alive — the caller should have made sure a turn is in flight (a
/// tool_use can only arrive while one is).
pub async fn send_tool_result(
    session: Arc<Session>,
    tool_use_id: String,
    output: serde_json::Value,
    is_error: bool,
) -> Result<(), String> {
    // Answer each tool_use exactly once. A duplicate call (e.g. an
    // AskUserQuestion form resurfaced by a UI remount/race after a reload
    // wiped the FE's resolution store) is a harmless no-op rather than a
    // second tool_result in claude's transcript. We mark the id only after a
    // successful write below, so a failed send can still be retried.
    if session
        .answered_tool_uses
        .lock()
        .await
        .contains(&tool_use_id)
    {
        log::debug!(
            target: "ikenga::claude::session",
            "tool_result for {tool_use_id} already sent; ignoring duplicate",
        );
        return Ok(());
    }
    let streaming = session
        .streaming
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| "no streaming child for tool_result".to_string())?;
    let envelope = tool_result_envelope(&tool_use_id, &output, is_error);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    drop(stdin);
    session.answered_tool_uses.lock().await.insert(tool_use_id);
    Ok(())
}

/// Build the line-delimited `sdk_control_response` envelope claude expects
/// Which control-protocol wire shape to speak when replying. Mirrors the
/// request: a `control_request` (claude 2.1.x) is answered with a
/// `control_response`; a legacy `sdk_control_request` with an
/// `sdk_control_response`. Both verified against claude 2.1.150.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlWire {
    /// Legacy: `{"type":"sdk_control_response","response":{<body>,"request_id":..}}`
    Legacy,
    /// 2.1.x: `{"type":"control_response","response":{"subtype":"success","request_id":..,"response":{<body>}}}`
    Modern,
}

impl ControlWire {
    /// Pick the wire shape from claude's `claude_code_version` (session-init).
    /// claude 2.1.0+ speaks the `control_request`/`control_response` protocol
    /// (request_id top-level); older builds use `sdk_control_request`. Anything
    /// unparseable defaults to `Modern` — the current and forward shape.
    /// Verified: 2.1.150 ignores legacy `set_permission_mode`/`interrupt`.
    pub fn from_version(version: &str) -> ControlWire {
        let mut parts = version.trim().split('.');
        let major: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX);
        let minor: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(u32::MAX);
        if major < 2 || (major == 2 && minor < 1) {
            ControlWire::Legacy
        } else {
            ControlWire::Modern
        }
    }
}

/// Build the control-response envelope claude expects in reply to a
/// control_request. `response_body` is the inner decision object
/// (`{"behavior":"allow","updatedInput":{...}}` or
/// `{"behavior":"deny","message":"..."}`); `wire` selects the matching
/// envelope shape (see `ControlWire`). The trailing newline is part of the
/// contract — claude reads stdin line-by-line.
///
/// Public for unit tests; the only caller is `send_control_response`.
pub fn control_response_envelope(
    request_id: &str,
    response_body: &serde_json::Value,
    wire: ControlWire,
) -> String {
    let value = match wire {
        ControlWire::Modern => serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": response_body,
            },
        }),
        ControlWire::Legacy => {
            let mut inner = match response_body {
                serde_json::Value::Object(m) => m.clone(),
                // Defensive: spec says callers pass an object. If they don't,
                // wrap so the envelope still parses on claude's end.
                other => {
                    let mut m = serde_json::Map::new();
                    m.insert("response".into(), other.clone());
                    m
                }
            };
            inner.insert(
                "request_id".into(),
                serde_json::Value::String(request_id.to_string()),
            );
            serde_json::json!({
                "type": "sdk_control_response",
                "response": serde_json::Value::Object(inner),
            })
        }
    };
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

/// Phase 4: write a `sdk_control_response` to the streaming child's stdin
/// in reply to a `sdk_control_request` we observed on stdout. `response`
/// is the response body (sans `request_id`/`type` wrapper) — typically
/// `{"behavior":"allow", "updatedInput": {...}}` or
/// `{"behavior":"deny", "message": "..."}`.
///
/// Errors if no streaming child is alive — the caller should only invoke
/// this in the middle of a prompt turn (which is the only time claude
/// emits a control_request).
pub async fn send_control_response(
    session: Arc<Session>,
    request_id: String,
    response: serde_json::Value,
    wire: ControlWire,
) -> Result<(), String> {
    let streaming = session
        .streaming
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| "no streaming child for control_response".to_string())?;
    let envelope = control_response_envelope(&request_id, &response, wire);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    Ok(())
}

/// Phase 5: write a `set_permission_mode` control_request to claude's
/// stdin so the running session picks up a mode switch without a re-spawn.
/// Claude does NOT reply to this kind of control_request (unlike
/// `permission` which expects a `sdk_control_response`), so we don't park
/// a waiter — fire and forget.
///
/// If there's no streaming child alive, returns Ok without doing anything:
/// the caller is expected to have already updated `session.current_mode`
/// + `session.opts.permission_mode`, and the next `spawn_streaming` will
/// pick up the new mode via the `--permission-mode` CLI flag.
pub async fn send_set_mode(session: Arc<Session>, mode: AcpSessionMode) -> Result<(), String> {
    let streaming = session.streaming.lock().await.as_ref().cloned();
    let Some(streaming) = streaming else {
        // No live child → the mode will be applied on the next spawn via
        // `--permission-mode`. This is the expected path for the very
        // first set_mode call before any prompt has spawned a child.
        return Ok(());
    };
    let request_id = format!("{}", uuid::Uuid::new_v4());
    let wire = *session.control_wire.lock().await;
    let envelope = crate::engines::claude_code::mode::set_mode_envelope(mode, &request_id, wire);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    Ok(())
}

/// Phase 6: write an interrupt control_request to claude's stdin. The
/// streaming child stops mid-turn and emits its normal `Done` envelope
/// (the prompt loop in `engines::claude_code::server::handle_prompt` watches for it), so
/// the transcript stays intact and the child remains alive for the next
/// turn. Unlike `cancel_streaming`, we do NOT kill the process.
///
/// Claude does NOT reply with a `sdk_control_response` for interrupts
/// (unlike `permission` which expects one), so this is fire-and-forget —
/// no waiter parking required.
///
/// If there's no streaming child alive there's nothing to interrupt —
/// returns Ok (idempotent). The ACP `session/cancel` semantics are "best
/// effort"; callers that need a hard guarantee should fall back to
/// `cancel_streaming` themselves.
pub async fn send_interrupt(session: Arc<Session>) -> Result<(), String> {
    let streaming = {
        let guard = session.streaming.lock().await;
        match guard.as_ref() {
            Some(s) => s.clone(),
            None => return Ok(()),
        }
    };
    let request_id = format!("{}", uuid::Uuid::new_v4());
    let wire = *session.control_wire.lock().await;
    let envelope = crate::engines::claude_code::interrupt::interrupt_envelope(&request_id, wire);
    let mut stdin = streaming.stdin.lock().await;
    stdin
        .write_all(envelope.as_bytes())
        .await
        .map_err(|e| format!("stdin write: {e}"))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("stdin flush: {e}"))?;
    Ok(())
}

/// Kill the streaming child. Leaves the in-memory `Session` so subsequent
/// `session_send` can re-spawn with `--resume`. Returns Ok if there was no
/// child to kill (idempotent).
///
/// Phase 6: the ACP `session/cancel` path no longer routes through here —
/// it uses `send_interrupt` instead so the transcript stays intact. This
/// function survives because the legacy `session_cancel` /
/// `session_destroy` Tauri commands (in `commands/claude.rs`) still need
/// the hard-kill semantics for tear-down / HMR hygiene.
///
/// "Hard kill" now means SIGTERM-then-SIGKILL rather than an immediate
/// SIGKILL — see `shutdown_child_gracefully`. The child is (bar the one
/// logged, practically-unreachable edge case documented there) guaranteed
/// dead by the time this returns; it just gets up to `GRACEFUL_SHUTDOWN_TIMEOUT`
/// to flush its transcript on the way out before SIGKILL escalates.
pub async fn cancel_streaming(session: Arc<Session>) -> Result<(), String> {
    // `take()` before draining so we hold the only `Arc<StreamingChild>` —
    // otherwise the `.kill_on_drop(true)` from spawn could fire mid-grace.
    let taken = session.streaming.lock().await.take();
    if let Some(c) = taken {
        let mut child = c.child.lock().await;
        shutdown_child_gracefully(&mut child, &session.thread_id).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn control_response_envelope_wraps_allow_body() {
        // Sanity-check the exact wire shape claude expects in reply to a
        // permission control_request. Trailing newline is part of the
        // contract — claude reads stdin line-by-line.
        let body = json!({
            "behavior": "allow",
            "updatedInput": { "answers": { "Which color?": "Red" } },
        });
        let env = control_response_envelope("req_42", &body, ControlWire::Legacy);
        assert!(env.ends_with('\n'));
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        assert_eq!(parsed["type"], json!("sdk_control_response"));
        assert_eq!(parsed["response"]["request_id"], json!("req_42"));
        assert_eq!(parsed["response"]["behavior"], json!("allow"));
        assert_eq!(
            parsed["response"]["updatedInput"]["answers"]["Which color?"],
            json!("Red"),
        );
    }

    #[test]
    fn control_response_envelope_wraps_deny_body() {
        let body = json!({"behavior": "deny", "message": "User declined"});
        let env = control_response_envelope("req_99", &body, ControlWire::Legacy);
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        assert_eq!(parsed["response"]["behavior"], json!("deny"));
        assert_eq!(parsed["response"]["message"], json!("User declined"));
        assert_eq!(parsed["response"]["request_id"], json!("req_99"));
    }

    #[test]
    fn control_wire_from_version() {
        assert_eq!(ControlWire::from_version("2.1.150"), ControlWire::Modern);
        assert_eq!(ControlWire::from_version("2.1.0"), ControlWire::Modern);
        assert_eq!(ControlWire::from_version("3.0.0"), ControlWire::Modern);
        assert_eq!(ControlWire::from_version("2.0.99"), ControlWire::Legacy);
        assert_eq!(ControlWire::from_version("1.9.0"), ControlWire::Legacy);
        // Unparseable → Modern (current + forward shape).
        assert_eq!(ControlWire::from_version("weird"), ControlWire::Modern);
    }

    #[test]
    fn control_response_envelope_modern_shape() {
        // claude 2.1.x replies are nested: response.subtype="success",
        // response.request_id, and the decision body under response.response.
        // Verified to unblock claude 2.1.150 end-to-end.
        let body = json!({
            "behavior": "allow",
            "updatedInput": { "answers": { "Pick": "Red" } },
        });
        let env = control_response_envelope("605f", &body, ControlWire::Modern);
        assert!(env.ends_with('\n'));
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        assert_eq!(parsed["type"], json!("control_response"));
        assert_eq!(parsed["response"]["subtype"], json!("success"));
        assert_eq!(parsed["response"]["request_id"], json!("605f"));
        assert_eq!(parsed["response"]["response"]["behavior"], json!("allow"));
        assert_eq!(
            parsed["response"]["response"]["updatedInput"]["answers"]["Pick"],
            json!("Red"),
        );
    }

    #[tokio::test]
    async fn send_interrupt_with_no_streaming_child_is_ok() {
        // Phase 6: ACP `session/cancel` semantics are best-effort. If
        // there's no streaming child alive (turn already over, or it was
        // never spawned), the interrupt is a no-op — not an error. The
        // outer `handle_cancel` relies on this so stale Stop clicks
        // don't surface as toast errors.
        let session = Arc::new(Session::new(
            "thread_int_test".into(),
            "/tmp".into(),
            SessionOpts::default(),
        ));
        assert!(session.streaming.lock().await.is_none());
        send_interrupt(session.clone())
            .await
            .expect("no-op send_interrupt returns Ok");
        // Still no child afterwards — interrupt never spawns.
        assert!(session.streaming.lock().await.is_none());
    }

    #[tokio::test]
    async fn send_set_mode_with_no_streaming_child_is_ok() {
        // Phase 5: until the first prompt spawns a child, `set_mode`
        // should just update the in-memory tracked mode and let the next
        // spawn pick it up via `--permission-mode`. The I/O helper itself
        // must therefore be a no-op when no child exists.
        let session = Arc::new(Session::new(
            "thread_test".into(),
            "/tmp".into(),
            SessionOpts::default(),
        ));
        assert!(session.streaming.lock().await.is_none());
        send_set_mode(session.clone(), AcpSessionMode::Auto)
            .await
            .expect("no-op send_set_mode returns Ok");
        // Still no child afterwards.
        assert!(session.streaming.lock().await.is_none());
    }

    #[tokio::test]
    async fn send_tool_result_ignores_already_answered_id() {
        // Idempotency guard: a tool_use answered once is a no-op on a repeat
        // call (e.g. an AskUserQuestion form resurfaced by a remount). The
        // early no-op returns Ok *without* needing a live child, so a
        // duplicate can't push a second tool_result into the transcript.
        let session = Arc::new(Session::new(
            "thread_dedup".into(),
            "/tmp".into(),
            SessionOpts::default(),
        ));
        session
            .answered_tool_uses
            .lock()
            .await
            .insert("tool_1".into());
        send_tool_result(
            session.clone(),
            "tool_1".into(),
            json!({ "answers": {} }),
            false,
        )
        .await
        .expect("duplicate tool_result is a no-op Ok");
    }

    #[tokio::test]
    async fn send_tool_result_unanswered_without_child_errors_and_stays_retryable() {
        // A first-time id with no streaming child errors (nothing to write to)
        // and must NOT be marked answered — otherwise the FE's retry-after-
        // failure path would be silently swallowed by the dedup guard.
        let session = Arc::new(Session::new(
            "thread_retry".into(),
            "/tmp".into(),
            SessionOpts::default(),
        ));
        let err = send_tool_result(session.clone(), "tool_2".into(), json!({}), false)
            .await
            .expect_err("no streaming child should error");
        assert!(err.contains("no streaming child"));
        assert!(!session.answered_tool_uses.lock().await.contains("tool_2"));
    }

    #[test]
    fn user_envelope_with_text_only_uses_string_content() {
        // Phase 7: text-only `PromptContent` must produce byte-identical
        // output to the Phase 3 string-content shape. This preserves the
        // wire trace for the legacy, heavily-exercised text path.
        let content = PromptContent {
            text: "hello".into(),
            images: Vec::new(),
        };
        let env = build_user_envelope(&content);
        assert!(env.ends_with('\n'));
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        assert_eq!(parsed["type"], json!("user"));
        assert_eq!(parsed["message"]["role"], json!("user"));
        // Critically: content stays a STRING, not an array.
        assert_eq!(parsed["message"]["content"], json!("hello"));
    }

    #[test]
    fn user_envelope_with_image_has_array_content() {
        // Phase 7: any image attachment forces the array-content branch.
        // The shape mirrors Anthropic's stream-json image content block:
        // `{"type":"image","source":{"type":"base64","media_type":"...","data":"..."}}`.
        let content = PromptContent {
            text: "what's this?".into(),
            images: vec![crate::engines::claude_code::prompt::PromptImage {
                mime_type: "image/png".into(),
                base64_data: "aGVsbG8=".into(),
            }],
        };
        let env = build_user_envelope(&content);
        assert!(env.ends_with('\n'));
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        let blocks = parsed["message"]["content"]
            .as_array()
            .expect("content is an array when images are present");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], json!("text"));
        assert_eq!(blocks[0]["text"], json!("what's this?"));
        assert_eq!(blocks[1]["type"], json!("image"));
        assert_eq!(blocks[1]["source"]["type"], json!("base64"));
        assert_eq!(blocks[1]["source"]["media_type"], json!("image/png"));
        assert_eq!(blocks[1]["source"]["data"], json!("aGVsbG8="));
    }

    #[test]
    fn user_envelope_with_image_only_omits_text_block() {
        // Edge case: caller built a PromptContent with empty text (e.g.
        // user dragged an image and hit send without typing anything,
        // and the extractor's default-prompt fallback was bypassed).
        // The envelope should not emit an empty text block; the array
        // should contain only the image.
        let content = PromptContent {
            text: String::new(),
            images: vec![crate::engines::claude_code::prompt::PromptImage {
                mime_type: "image/jpeg".into(),
                base64_data: "aGVsbG8=".into(),
            }],
        };
        let env = build_user_envelope(&content);
        let parsed: serde_json::Value =
            serde_json::from_str(env.trim_end()).expect("envelope is JSON");
        let blocks = parsed["message"]["content"].as_array().expect("array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], json!("image"));
    }

    /// The fix's happy path: a child that honours SIGTERM exits on its own,
    /// well inside the grace window, and is reaped. This is the shape of a
    /// claude child that gets to flush its `.jsonl` — before the fix it was
    /// SIGKILLed here with no such window.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_gracefully_lets_a_well_behaved_child_exit_on_sigterm() {
        // Default SIGTERM disposition = terminate. Sleeps far longer than the
        // grace window, so if it dies quickly it can only be because SIGTERM
        // reached it.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("sleep 30")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child");

        let started = std::time::Instant::now();
        shutdown_child_gracefully(&mut child, "thread_graceful").await;
        let elapsed = started.elapsed();

        // Exited via SIGTERM, not via the timeout + SIGKILL fallback.
        assert!(
            elapsed < GRACEFUL_SHUTDOWN_TIMEOUT,
            "expected SIGTERM exit inside the grace window, took {elapsed:?}",
        );
        // Reaped — no zombie left behind. `id()` returns None only after the
        // child has been waited on.
        assert!(
            child.id().is_none(),
            "child must be reaped, not left a zombie"
        );
    }

    /// The anti-orphan guarantee, which is the whole reason this kill path
    /// exists. A child that ignores SIGTERM must still be dead when we
    /// return — the grace window is bounded, not unbounded.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_gracefully_escalates_to_sigkill_for_a_stubborn_child() {
        // `trap "" TERM` makes SIGTERM a no-op. The loop (rather than a bare
        // `sleep`) keeps sh from exec-optimising itself away, so the trap
        // really is installed in the process we signal.
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while true; do sleep 0.1; done")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child");

        // Give sh a moment to actually install the trap, otherwise we'd race
        // it and accidentally test the graceful path instead.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let started = std::time::Instant::now();
        shutdown_child_gracefully(&mut child, "thread_stubborn").await;
        let elapsed = started.elapsed();

        // It must have ridden out the full grace window before escalating.
        assert!(
            elapsed >= GRACEFUL_SHUTDOWN_TIMEOUT,
            "expected the full grace window before SIGKILL, took {elapsed:?}",
        );
        // And it must be dead + reaped regardless. This is the guarantee that
        // must survive the graceful-shutdown change.
        assert!(
            child.id().is_none(),
            "stubborn child must still be killed and reaped",
        );
    }

    /// An already-exited child is a no-op fast path — no signal, no waiting.
    /// This is the common case at `beforeunload`: most turns have already
    /// finished and EOFed.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_gracefully_is_fast_for_an_already_exited_child() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn test child");
        // Let it exit so `try_wait` observes the status.
        let _ = child.wait().await;

        let started = std::time::Instant::now();
        shutdown_child_gracefully(&mut child, "thread_done").await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "already-exited child must not consume the grace window",
        );
    }

    #[test]
    fn session_opts_permission_mode_defaults_to_default() {
        // `Default` is the safest starting state — every tool goes
        // through the permission round-trip.
        let opts = SessionOpts::default();
        assert_eq!(opts.permission_mode, AcpSessionMode::Default);
    }

    #[test]
    fn session_opts_permission_mode_deserializes_camel_case() {
        // The frontend wire-format uses camelCase ACP ids; verify the
        // serde mapping survives a full round-trip.
        let opts: SessionOpts = serde_json::from_value(serde_json::json!({
            "permissionMode": "bypassPermissions"
        }))
        .expect("deserialize ok");
        assert_eq!(opts.permission_mode, AcpSessionMode::BypassPermissions);
    }
}
