//! Chi-first agent surface.
//!
//! WP-01: a thin cache-backed command surface for running, resuming, listing,
//! and cancelling agent sessions.
//! WP-02: wires the Claude Code engine so `iyke chi run` / `resume` / `cancel`
//! and `list` / `status` actually spawn, monitor, and read the agent child.
//! WP-07: multi-engine parity — Codex (`codex exec --json`) wired;
//!         cursor-agent returns `RUNTIME_NOT_IMPLEMENTED` cleanly.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::claude::event::ChatEvent;
use crate::claude::stream_parser::StreamParser;
use crate::commands::claude::claude_list_sessions;
use crate::commands::db::PaDb;
use crate::engines::claude_code::mode::AcpSessionMode;
use crate::engines::codex_pty::parser as codex_parser;
use crate::terminal::multiplexer;

/// Cache state. Lives in `app_data_dir` and is `.manage()`d in `lib.rs`.
#[derive(Clone, Debug)]
pub struct ChiCache {
    app_data_dir: PathBuf,
}

impl ChiCache {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self { app_data_dir }
    }

    /// `<app-data-dir>/chi-cache/`
    pub fn cache_dir(&self) -> PathBuf {
        self.app_data_dir.join("chi-cache")
    }

    /// Per-run artifact / output tail file.
    pub fn run_output_path(&self, run_id: &str) -> PathBuf {
        self.cache_dir().join(format!("{run_id}.json"))
    }

    /// Ensure the JSON cache directory exists.
    pub fn ensure_cache_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.cache_dir()).map_err(|e| format!("chi-cache dir: {e}"))
    }
}

/// Runtime state for live Chi children. `.manage()`d in `lib.rs`.
#[derive(Default, Clone)]
pub struct ChiRuntime {
    running: Arc<Mutex<HashMap<String, Arc<ChiRunHandle>>>>,
}

impl ChiRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, run_id: &str, handle: Arc<ChiRunHandle>) {
        self.running.lock().await.insert(run_id.to_string(), handle);
    }

    pub async fn remove(&self, run_id: &str) -> Option<Arc<ChiRunHandle>> {
        self.running.lock().await.remove(run_id)
    }
}

/// Handle to a live Chi child so `chi_cancel` can interrupt it.
pub struct ChiRunHandle {
    /// The actual OS child process. Shared with the reader task.
    pub child: Arc<Mutex<Child>>,
    /// Set to true by `chi_cancel` before killing the child. The reader task
    /// uses this to distinguish a manual cancel from a natural exit / failure.
    pub cancelled: Arc<AtomicBool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChiRunOpts {
    pub engine_id: String,
    pub prompt: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    #[allow(dead_code)]
    pub timeout_seconds: Option<u32>,
    pub parent_id: Option<String>,
    #[serde(rename = "resumeSessionId")]
    pub resume_session_id: Option<String>,
    /// If true, try to launch via the tmux multiplexer so the session
    /// survives an app restart. Falls back to in-process if tmux is
    /// unavailable. The tmux session name is stored in
    /// `chi_cache.terminal_session_id`.
    #[serde(default)]
    pub persistent: bool,
}

#[derive(Serialize)]
pub struct ChiRunResult {
    pub run_id: String,
    pub status: String,
    pub output: Option<String>,
    pub output_truncated: Option<bool>,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ChiCacheRow {
    pub run_id: String,
    pub engine_id: String,
    pub external_id: Option<String>,
    pub brief: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub status: String,
    pub output_path: Option<String>,
    pub output_truncated: Option<bool>,
    pub error: Option<String>,
    pub artifacts: Option<serde_json::Value>,
    pub parent_id: Option<String>,
    pub owner: String,
    pub terminal_session_id: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub last_seen_at: Option<String>,
    pub expires_at: Option<String>,
}

/// On-disk shape for a per-run output file. The cache row points at this file.
#[derive(Serialize, Deserialize)]
struct RunOutputFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_at: Option<String>,
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn one_hour_from_now_iso() -> String {
    chrono::Utc::now()
        .checked_add_signed(chrono::TimeDelta::hours(1))
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339()
}

/// Convert a SQLite row into a `ChiCacheRow`. Explicit typing keeps sqlx happy
/// when the query is built from a `&'static str`.
fn row_to_cache_row(r: &sqlx::sqlite::SqliteRow) -> Result<ChiCacheRow, String> {
    let artifacts: Option<String> = r.try_get("artifacts").ok().flatten();
    let artifacts = artifacts.and_then(|s| serde_json::from_str(&s).ok());

    let output_truncated: Option<i64> = r.try_get("output_truncated").ok().flatten();
    let output_truncated = output_truncated.map(|v| v != 0);

    Ok(ChiCacheRow {
        run_id: r.get("run_id"),
        engine_id: r.get("engine_id"),
        external_id: r.get("external_id"),
        brief: r.get("brief"),
        cwd: r.get("cwd"),
        model: r.get("model"),
        mode: r.get("mode"),
        status: r.get("status"),
        output_path: r.get("output_path"),
        output_truncated,
        error: r.get("error"),
        artifacts,
        parent_id: r.get("parent_id"),
        owner: r.get("owner"),
        terminal_session_id: r.get("terminal_session_id"),
        started_at: r.get("started_at"),
        ended_at: r.get("ended_at"),
        last_seen_at: r.get("last_seen_at"),
        expires_at: r.get("expires_at"),
    })
}

/// Upsert a cache row from the options. Returns the run_id.
async fn cache_insert(
    db: &PaDb,
    run_id: &str,
    opts: &ChiRunOpts,
    output_path: &Path,
    owner: &str,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    let expires = one_hour_from_now_iso();
    let output_path_string = output_path.to_string_lossy().to_string();

    sqlx::query(
        "INSERT INTO chi_cache (
            run_id, engine_id, external_id, brief, cwd, model, mode, status,
            output_path, output_truncated, error, artifacts, parent_id, owner,
            terminal_session_id, started_at, ended_at, last_seen_at, expires_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(&opts.engine_id)
    .bind(&opts.resume_session_id) // initial external_id is the resume id, if any
    .bind(&opts.prompt)
    .bind(&opts.cwd)
    .bind(&opts.model)
    .bind(&opts.mode)
    .bind("queued")
    .bind(output_path_string)
    .bind(0i64) // output_truncated; boolean stored as integer
    .bind::<Option<String>>(None)
    .bind::<Option<String>>(None) // artifacts as JSON string
    .bind(&opts.parent_id)
    .bind(owner)
    .bind::<Option<String>>(None)
    .bind(&now)
    .bind::<Option<String>>(None)
    .bind(&now)
    .bind(&expires)
    .execute(&pool)
    .await
    .map_err(|e| format!("chi_cache insert: {e}"))?;
    Ok(())
}

async fn cache_get(db: &PaDb, run_id: &str) -> Result<Option<ChiCacheRow>, String> {
    let pool = db.ensure_pool().await?;
    let row = sqlx::query(
        "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                output_path, output_truncated, error, artifacts, parent_id, owner,
                terminal_session_id, started_at, ended_at, last_seen_at, expires_at
         FROM chi_cache WHERE run_id = ?",
    )
    .bind(run_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| format!("chi_cache get: {e}"))?;

    row.as_ref().map(row_to_cache_row).transpose()
}

async fn cache_list(
    db: &PaDb,
    engine_id: Option<&str>,
    limit: i64,
) -> Result<Vec<ChiCacheRow>, String> {
    let pool = db.ensure_pool().await?;
    let rows = if let Some(engine) = engine_id {
        sqlx::query(
            "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                    output_path, output_truncated, error, artifacts, parent_id, owner,
                    terminal_session_id, started_at, ended_at, last_seen_at, expires_at
             FROM chi_cache
             WHERE engine_id = ?
             ORDER BY last_seen_at DESC
             LIMIT ?",
        )
        .bind(engine)
        .bind(limit)
        .fetch_all(&pool)
        .await
    } else {
        sqlx::query(
            "SELECT run_id, engine_id, external_id, brief, cwd, model, mode, status,
                    output_path, output_truncated, error, artifacts, parent_id, owner,
                    terminal_session_id, started_at, ended_at, last_seen_at, expires_at
             FROM chi_cache
             ORDER BY last_seen_at DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&pool)
        .await
    }
    .map_err(|e| format!("chi_cache list: {e}"))?;

    rows.iter().map(row_to_cache_row).collect()
}

async fn cache_update_status(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    sqlx::query("UPDATE chi_cache SET status = ?, error = ?, last_seen_at = ? WHERE run_id = ?")
        .bind(status)
        .bind(error)
        .bind(&now)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache update status: {e}"))?;
    Ok(())
}

async fn cache_update_external_id(
    db: &PaDb,
    run_id: &str,
    external_id: &str,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    sqlx::query("UPDATE chi_cache SET external_id = ?, last_seen_at = ? WHERE run_id = ?")
        .bind(external_id)
        .bind(&now)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|e| format!("chi_cache update external_id: {e}"))?;
    Ok(())
}

async fn cache_update_done(
    db: &PaDb,
    run_id: &str,
    status: &str,
    error: Option<&str>,
    output_truncated: bool,
    artifacts: Option<&serde_json::Value>,
) -> Result<(), String> {
    let pool = db.ensure_pool().await?;
    let now = now_iso();
    let ended = now_iso();
    let artifacts_json = artifacts.map(|v| v.to_string());
    sqlx::query(
        "UPDATE chi_cache SET
            status = ?, error = ?, output_truncated = ?, artifacts = ?,
            ended_at = ?, last_seen_at = ?
         WHERE run_id = ?",
    )
    .bind(status)
    .bind(error)
    .bind(output_truncated as i64)
    .bind(artifacts_json)
    .bind(&ended)
    .bind(&now)
    .bind(run_id)
    .execute(&pool)
    .await
    .map_err(|e| format!("chi_cache update done: {e}"))?;
    Ok(())
}

/// Build the line-delimited user envelope that streaming-input mode expects.
fn user_envelope(text: &str) -> String {
    let value = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    let mut s = serde_json::to_string(&value).unwrap_or_else(|_| String::from("{}"));
    s.push('\n');
    s
}

fn create_chi_command(binary: &str) -> Command {
    #[cfg(windows)]
    {
        let resolved = which::which_in(binary, Some(crate::runtime::augmented_path()), ".")
            .or_else(|_| which::which_in(format!("{binary}.cmd"), Some(crate::runtime::augmented_path()), "."))
            .or_else(|_| which::which_in(format!("{binary}.exe"), Some(crate::runtime::augmented_path()), "."));
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
            Command::new(binary)
        }
    }
    #[cfg(not(windows))]
    {
        Command::new(binary)
    }
}

/// Return a `(command, child)` for the requested engine.
fn build_engine_command(
    engine_id: &str,
    prompt: &str,
    cwd: &str,
    model: Option<&str>,
    mode: Option<&str>,
    resume_id: Option<&str>,
) -> Result<Command, String> {
    match engine_id {
        "claude-code" => {
            let permission_mode = mode
                .and_then(AcpSessionMode::from_acp_id)
                .unwrap_or_default()
                .as_claude_flag();

            let mut cmd = create_chi_command("claude");
            cmd.arg("--permission-prompt-tool")
                .arg("stdio")
                .arg("--permission-mode")
                .arg(permission_mode)
                .arg("--print")
                .arg("--input-format")
                .arg("stream-json")
                .arg("--output-format")
                .arg("stream-json")
                .arg("--verbose")
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("PATH", crate::runtime::augmented_path());

            if let Some(id) = resume_id {
                cmd.arg("--resume").arg(id);
            }
            if let Some(m) = model {
                cmd.arg("--model").arg(m);
            }

            Ok(cmd)
        }
        "antigravity-cli" => {
            let mut cmd = create_chi_command("agy");
            cmd.arg("-p")
                .arg(prompt)
                .arg("--output-format")
                .arg("stream-json")
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("PATH", crate::runtime::augmented_path());

            if let Some(id) = resume_id {
                cmd.arg("--conversation").arg(id);
            }
            if let Some(m) = model {
                cmd.arg("--model").arg(m);
            }
            if let Some(mo) = mode {
                cmd.arg("--mode").arg(mo);
            }

            Ok(cmd)
        }
        // Codex uses `codex exec --json` for new sessions and
        // `codex exec resume <thread_id> --json` for subsequent turns.
        // `--skip-git-repo-check` makes the spawn predictable inside
        // arbitrary project dirs (codex defaults to refusing outside a
        // git repo). `-` as the positional arg means "read prompt from stdin".
        "codex" => {
            let mut cmd = create_chi_command("codex");
            if let Some(id) = resume_id {
                cmd.args(["exec", "resume", id, "--json"]);
            } else {
                cmd.args(["exec", "--json"]);
            }
            cmd.args(["--skip-git-repo-check", "--cd", cwd, "-"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("PATH", crate::runtime::augmented_path());
            // `--model` is a codex global flag (before the subcommand);
            // codex itself selects the default model from its config if
            // omitted, so we only pass it when explicitly set.
            if let Some(m) = model {
                cmd.arg("--model").arg(m);
            }
            Ok(cmd)
        }
        "opencode" => {
            let mut cmd = create_chi_command("opencode");
            cmd.arg("run")
                .arg("-p")
                .arg(prompt)
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("PATH", crate::runtime::augmented_path());

            if let Some(m) = model {
                cmd.arg("--model").arg(m);
            }

            Ok(cmd)
        }
        "pi" => {
            let mut cmd = create_chi_command("pi");
            cmd.arg("-p")
                .arg(prompt)
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .env("PATH", crate::runtime::augmented_path());

            if let Some(m) = model {
                cmd.arg("--model").arg(m);
            }

            Ok(cmd)
        }
        // cursor-agent is scaffolded but not yet runnable through the chi
        // surface. Return a clean error rather than falling through to an
        // unhelpful "command not found" OS error.
        "cursor-agent" => Err(
            "cursor-agent runtime not implemented — \
             the cursor-agent CLI does not yet expose a stable non-interactive mode \
             compatible with the chi pipe protocol (ADR-013 Phase 4)"
                .to_string(),
        ),
        _ => Err(format!("engine not yet supported by iyke chi: {engine_id}")),
    }
}

/// Spawns the engine child and returns the (child, stdin, stdout, stderr).
fn spawn_engine_child(
    mut cmd: Command,
) -> Result<(Child, tokio::process::ChildStdin, tokio::process::ChildStdout, Option<tokio::process::ChildStderr>), String>
{
    let mut child = cmd.spawn().map_err(|e| format!("spawn engine: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "engine stdin pipe missing".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "engine stdout pipe missing".to_string())?;
    let stderr = child.stderr.take();
    Ok((child, stdin, stdout, stderr))
}

/// Background task for a Claude Code one-off. Reads `stdout`, writes partial
/// output to `output_path`, and updates `chi_cache` as the run progresses.
async fn claude_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    prompt: String,
) {
    // Send the initial prompt envelope.
    let envelope = user_envelope(&prompt);
    if let Err(e) = stdin.write_all(envelope.as_bytes()).await {
        cache_update_status(&db, &run_id, "failed", Some(&format!("stdin write: {e}"))).await.ok();
        return;
    }
    let _ = stdin.flush().await;
    // Close stdin so claude knows no more input is coming for this turn.
    let _ = stdin.shutdown().await;

    // Spawn stderr logger.
    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "claude stderr: {line}");
            }
        });
    }

    let mut parser = StreamParser::new();
    let mut reader = BufReader::new(stdout);
    let mut buf = vec![0u8; 8 * 1024];
    let mut output = String::new();
    let mut artifacts = Vec::<serde_json::Value>::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut stop_reason: Option<String> = None;

    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let events = parser.feed(&buf[..n]);
                for event in events {
                    match event {
                        ChatEvent::SessionInit { session_id, .. } if !session_id.is_empty() => {
                            if external_id.is_none() {
                                external_id = Some(session_id.clone());
                                cache_update_external_id(&db, &run_id, &session_id).await.ok();
                                cache_update_status(&db, &run_id, "running", None).await.ok();
                            }
                        }
                        ChatEvent::Text { delta, .. } => {
                            output.push_str(&delta);
                        }
                        ChatEvent::Artifact { path, mime, produced_by } => {
                            artifacts.push(serde_json::json!({
                                "path": path,
                                "mime": mime,
                                "producedBy": produced_by,
                            }));
                        }
                        ChatEvent::Done { stop_reason: s, .. } => {
                            saw_done = true;
                            stop_reason = s.clone();
                        }
                        ChatEvent::ControlRequest { subtype, .. }
                            if subtype == "permission" =>
                        {
                            cache_update_status(&db, &run_id, "awaiting_auth", None)
                                .await
                                .ok();
                        }
                        ChatEvent::AskUserQuestion { .. } => {
                            cache_update_status(&db, &run_id, "awaiting_auth", None)
                                .await
                                .ok();
                        }
                        _ => {}
                    }
                }

                // Periodically flush partial output to disk.
                write_output_file(&output_path, &output, None).await.ok();
            }
            Err(e) => {
                log::debug!(target: "ikenga::chi", "claude reader closed: {e}");
                break;
            }
        }
    }

    // Determine final status.
    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            (
                "failed",
                Some("claude reported stop_reason error"),
                Some(output),
            )
        } else {
            ("done", None, Some(output))
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
            Some(output),
        )
    };

    let output_truncated = done_output.as_ref().map(|s| s.len() > 100_000).unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    // Write final output file.
    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    let artifacts_value = if artifacts.is_empty() {
        None
    } else {
        Some(serde_json::Value::Array(artifacts))
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        artifacts_value.as_ref(),
    )
    .await
    .ok();

    // Reap the child so the OS handle is released.
    let mut child = child.lock().await;
    let _ = child.try_wait();
}

async fn antigravity_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    _stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    _prompt: String,
) {
    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "antigravity stderr: {line}");
            }
        });
    }

    let mut reader = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut stop_reason: Option<String> = None;

    while let Ok(Some(line)) = reader.next_line().await {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(event) = val.get("event").and_then(|e| e.as_str()) {
                match event {
                    "init" => {
                        if let Some(conv_id) = val.get("conversation_id").and_then(|id| id.as_str()) {
                            if external_id.is_none() {
                                external_id = Some(conv_id.to_string());
                                cache_update_external_id(&db, &run_id, conv_id).await.ok();
                                cache_update_status(&db, &run_id, "running", None).await.ok();
                            }
                        }
                    }
                    "step_update" => {
                        if let Some(step_update) = val.get("step_update") {
                            if let Some(step_type) = step_update.get("step_type").and_then(|t| t.as_str()) {
                                if step_type == "agent_response" {
                                    if let Some(delta) = step_update.get("text_delta").and_then(|d| d.as_str()) {
                                        output.push_str(delta);
                                    }
                                }
                            }
                        }
                    }
                    "result" => {
                        saw_done = true;
                        if let Some(result) = val.get("result") {
                            if let Some(status) = result.get("status").and_then(|s| s.as_str()) {
                                if status != "SUCCESS" {
                                    stop_reason = Some("error".to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        write_output_file(&output_path, &output, None).await.ok();
    }

    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done {
        if stop_reason.as_deref() == Some("error") {
            (
                "failed",
                Some("antigravity reported stop_reason error"),
                Some(output),
            )
        } else {
            ("done", None, Some(output))
        }
    } else {
        (
            "failed",
            Some("engine child exited without a done envelope"),
            Some(output),
        )
    };

    let output_truncated = done_output.as_ref().map(|s| s.len() > 100_000).unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        None,
    )
    .await
    .ok();

    let mut child = child.lock().await;
    let _ = child.try_wait();
}

/// Background task for a Codex one-off (`codex exec --json`).
///
/// Reads the JSONL event stream from stdout via the existing
/// `codex_pty::parser`, extracts `agent_message` text chunks and the
/// `thread.started` thread id (stored as `external_id` so `chi_resume`
/// can pass it back as `--resume <id>`).
async fn codex_one_off_task(
    db: Arc<PaDb>,
    _cache: ChiCache,
    run_id: String,
    output_path: PathBuf,
    child: Arc<Mutex<Child>>,
    cancelled: Arc<AtomicBool>,
    mut stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: Option<tokio::process::ChildStderr>,
    prompt: String,
) {
    // Write prompt to stdin then close it so codex knows EOF.
    if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
        cache_update_status(&db, &run_id, "failed", Some(&format!("stdin write: {e}")))
            .await
            .ok();
        return;
    }
    let _ = stdin.flush().await;
    let _ = stdin.shutdown().await;

    if let Some(stderr) = stderr {
        tauri::async_runtime::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "ikenga::chi", "codex stderr: {line}");
            }
        });
    }

    let mut reader = BufReader::new(stdout).lines();
    let mut output = String::new();
    let mut external_id: Option<String> = None;
    let mut saw_done = false;
    let mut failed = false;

    while let Ok(Some(line)) = reader.next_line().await {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        let event = match codex_parser::parse_event(&line) {
            Ok(e) => e,
            Err(e) => {
                log::debug!(target: "ikenga::chi", "codex parse error: {e}");
                continue;
            }
        };
        match &event {
            codex_parser::ParsedEvent::ThreadStarted { thread_id } => {
                if external_id.is_none() {
                    external_id = Some(thread_id.clone());
                    cache_update_external_id(&db, &run_id, thread_id).await.ok();
                    cache_update_status(&db, &run_id, "running", None).await.ok();
                }
            }
            codex_parser::ParsedEvent::TurnCompleted { .. } => {
                saw_done = true;
            }
            codex_parser::ParsedEvent::TurnFailed { message } => {
                saw_done = true;
                failed = true;
                output.push_str(&format!("[error] {message}\n"));
            }
            codex_parser::ParsedEvent::Error { message } => {
                output.push_str(&format!("[warning] {message}\n"));
            }
            codex_parser::ParsedEvent::Item { phase, kind } => {
                // Accumulate agent_message text chunks (completed/updated phases only).
                if matches!(
                    phase,
                    codex_parser::ItemPhase::Completed | codex_parser::ItemPhase::Updated
                ) {
                    if let codex_parser::ItemKind::AgentMessage { text, .. } = kind {
                        if !text.is_empty() {
                            output.push_str(text);
                        }
                    }
                }
            }
            _ => {}
        }
        write_output_file(&output_path, &output, None).await.ok();
    }

    let (status, error, done_output) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, Some(output))
    } else if saw_done && !failed {
        ("done", None, Some(output))
    } else if failed {
        ("failed", Some("codex reported turn.failed"), Some(output))
    } else {
        (
            "failed",
            Some("codex child exited without turn.completed"),
            Some(output),
        )
    };

    let output_truncated = done_output.as_ref().map(|s| s.len() > 100_000).unwrap_or(false);
    let output_json = done_output.as_deref().unwrap_or("");

    let file_error = if let Err(e) = write_output_file(&output_path, output_json, error).await {
        Some(format!("write output file: {e}"))
    } else {
        error.map(|s| s.to_string())
    };

    cache_update_done(
        &db,
        &run_id,
        status,
        file_error.as_deref(),
        output_truncated,
        None,
    )
    .await
    .ok();

    let mut child = child.lock().await;
    let _ = child.try_wait();
}

async fn write_output_file(
    path: &Path,
    output: &str,
    error: Option<&str>,
) -> Result<(), String> {
    let file = RunOutputFile {
        output: Some(output.to_string()),
        error: error.map(|s| s.to_string()),
        done_at: Some(now_iso()),
    };
    let json = serde_json::to_string(&file).map_err(|e| format!("serialize output: {e}"))?;
    tokio::fs::write(path, json)
        .await
        .map_err(|e| format!("write output file: {e}"))?;
    Ok(())
}

async fn read_output_file(path: &Path) -> Option<RunOutputFile> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => serde_json::from_str(&s).ok(),
        Err(_) => None,
    }
}

/// Run a Chi. Spawns the engine child in the background and returns the
/// run id immediately.
#[tauri::command]
pub async fn chi_run(
    _app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    runtime: State<'_, Arc<ChiRuntime>>,
    opts: ChiRunOpts,
) -> Result<ChiRunResult, String> {
    spawn_chi_run(db.inner().clone(), cache.inner(), runtime.inner(), opts, "cli").await
}

/// Core of `chi_run`, callable from other commands that need to launch an
/// agent without going through the Tauri command boundary (e.g.
/// `comment_route`'s `chi` sink). `owner` tags the cache row so the audit
/// trail distinguishes a CLI-initiated run from a pin-initiated one.
pub(crate) async fn spawn_chi_run(
    db: Arc<PaDb>,
    cache: &ChiCache,
    runtime: &Arc<ChiRuntime>,
    opts: ChiRunOpts,
    owner: &str,
) -> Result<ChiRunResult, String> {
    cache.ensure_cache_dir()?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let output_path = cache.run_output_path(&run_id);

    // Initial one-off TTL is 1 hour; long-lived sessions will refresh this.
    cache_insert(&db, &run_id, &opts, &output_path, owner).await?;

    let cwd = opts
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| ".".to_string());
    let cwd = shellexpand::full(&cwd)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| cwd.clone());

    // ── Persistent (tmux-backed) path ────────────────────────────────────────
    // Try this first so we never spawn a redundant in-process child.
    if opts.persistent && multiplexer::tmux_available() {
        let conf = multiplexer::RunnerConf {
            run_id: &run_id,
            engine_id: &opts.engine_id,
            prompt: &opts.prompt,
            cwd: &cwd,
            model: opts.model.as_deref(),
            mode: opts.mode.as_deref(),
            resume_session_id: opts.resume_session_id.as_deref(),
            output_path: &output_path.to_string_lossy(),
            timeout_seconds: opts.timeout_seconds.map(|s| s as u64),
        };
        match multiplexer::spawn_in_tmux(&conf, &cache.cache_dir()) {
            multiplexer::SpawnResult::Ok { session_name } => {
                cache_update_status(&db, &run_id, "running", None).await.ok();
                if let Ok(pool) = db.ensure_pool().await {
                    sqlx::query(
                        "UPDATE chi_cache SET terminal_session_id = ? WHERE run_id = ?",
                    )
                    .bind(&session_name)
                    .bind(&run_id)
                    .execute(&pool)
                    .await
                    .ok();
                }
                log::info!(
                    target: "ikenga::chi",
                    "chi run {run_id} started in tmux session '{session_name}'"
                );
                return Ok(ChiRunResult {
                    run_id,
                    status: "running".to_string(),
                    output: None,
                    output_truncated: None,
                    error: None,
                });
            }
            multiplexer::SpawnResult::Unavailable { reason } => {
                log::warn!(
                    target: "ikenga::chi",
                    "tmux unavailable ({reason}), falling back to in-process task"
                );
            }
        }
    }

    // ── In-process (non-persistent) path ─────────────────────────────────────
    let cmd = build_engine_command(
        &opts.engine_id,
        &opts.prompt,
        &cwd,
        opts.model.as_deref(),
        opts.mode.as_deref(),
        opts.resume_session_id.as_deref(),
    )?;
    let (child, stdin, stdout, stderr) = spawn_engine_child(cmd)?;
    cache_update_status(&db, &run_id, "running", None).await?;

    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(ChiRunHandle {
        child: child.clone(),
        cancelled: cancelled.clone(),
    });
    runtime.insert(&run_id, handle).await;

    let cache = cache.clone();
    let output_path = output_path.clone();
    let prompt = opts.prompt.clone();
    let run_id_for_task = run_id.clone();
    let engine_id = opts.engine_id.clone();
    tauri::async_runtime::spawn(async move {
        if engine_id == "antigravity-cli" {
            antigravity_one_off_task(
                db, cache, run_id_for_task, output_path, child, cancelled, stdin, stdout, stderr, prompt,
            )
            .await;
        } else if engine_id == "codex" {
            codex_one_off_task(
                db, cache, run_id_for_task, output_path, child, cancelled, stdin, stdout, stderr, prompt,
            )
            .await;
        } else {
            claude_one_off_task(
                db, cache, run_id_for_task, output_path, child, cancelled, stdin, stdout, stderr, prompt,
            )
            .await;
        }
    });

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}



/// Resume an existing Chi session using its engine-native `external_id`.
#[tauri::command]
pub async fn chi_resume(
    _app: AppHandle,
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    runtime: State<'_, Arc<ChiRuntime>>,
    #[allow(non_snake_case)] runId: String,
    prompt: String,
) -> Result<ChiRunResult, String> {
    let run_id = runId;
    let row = cache_get(&db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    let db = db.inner().clone();
    let cache = cache.inner().clone();
    let output_path = PathBuf::from(row.output_path.as_deref().unwrap_or(""));

    let resume_id = row.external_id.ok_or_else(|| {
        format!("chi run {run_id} has no engine session id to resume against")
    })?;

    cache_update_status(&db, &run_id, "running", None).await?;

    let cwd = row
        .cwd
        .unwrap_or_else(|| ".".to_string());
    let cwd = shellexpand::full(&cwd)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| cwd.clone());

    let cmd = build_engine_command(
        &row.engine_id,
        &prompt,
        &cwd,
        row.model.as_deref(),
        row.mode.as_deref(),
        Some(&resume_id),
    )?;
    let (child, stdin, stdout, stderr) = spawn_engine_child(cmd)?;

    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let handle = Arc::new(ChiRunHandle {
        child: child.clone(),
        cancelled: cancelled.clone(),
    });
    runtime.insert(&run_id, handle).await;

    let db = db.clone();
    let cache = cache.clone();
    let output_path = output_path.clone();
    let engine_id = row.engine_id.clone();
    let run_id_for_task = run_id.clone();
    tauri::async_runtime::spawn(async move {
        if engine_id == "antigravity-cli" {
            antigravity_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else if engine_id == "codex" {
            codex_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        } else {
            claude_one_off_task(
                db,
                cache,
                run_id_for_task,
                output_path,
                child,
                cancelled,
                stdin,
                stdout,
                stderr,
                prompt,
            )
            .await;
        }
    });

    Ok(ChiRunResult {
        run_id,
        status: "running".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

/// Read the status of a Chi run from the cache and its output file.
#[tauri::command]
pub async fn chi_status(
    db: State<'_, Arc<PaDb>>,
    cache: State<'_, ChiCache>,
    #[allow(non_snake_case)] runId: String,
) -> Result<ChiRunResult, String> {
    let run_id = runId;
    let row = cache_get(&db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    let output_path = row
        .output_path
        .as_deref()
        .map(Path::new)
        .map(|p| {
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                cache.cache_dir().join(p)
            }
        });
    let file = if let Some(path) = output_path {
        read_output_file(&path).await
    } else {
        None
    };

    Ok(ChiRunResult {
        run_id: row.run_id,
        status: row.status,
        output: file.as_ref().and_then(|f| f.output.clone()).or(row.brief),
        output_truncated: row.output_truncated,
        error: file.and_then(|f| f.error).or(row.error),
    })
}

/// List cached Chi runs, optionally filtered by engine. Merges with agent-native
/// session records on disk.
#[tauri::command]
pub async fn chi_list(
    db: State<'_, Arc<PaDb>>,
    #[allow(non_snake_case)] engineId: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<ChiCacheRow>, String> {
    let engine_id = engineId.as_deref();
    let mut rows = cache_list(&db, engine_id, limit.unwrap_or(50).clamp(1, 200)).await?;

    // Merge with Claude JSONL records when no engine filter or claude-code.
    if engine_id.is_none() || engine_id == Some("claude-code") {
        match claude_list_sessions(None, Some(limit.unwrap_or(50).clamp(1, 200) as usize)).await {
            Ok(sessions) => {
                let mut seen: std::collections::HashSet<String> =
                    rows.iter().filter_map(|r| r.external_id.clone()).collect();
                for s in sessions {
                    if seen.contains(&s.session_id) {
                        // Refresh last_seen_at on matching cache rows.
                        for row in rows.iter_mut() {
                            if row.external_id.as_deref() == Some(&s.session_id) {
                                row.last_seen_at = s.last_message_at.clone().or(Some(s.started_at.clone()));
                            }
                        }
                        continue;
                    }
                    seen.insert(s.session_id.clone());
                    rows.push(ChiCacheRow {
                        run_id: s.session_id.clone(),
                        engine_id: "claude-code".to_string(),
                        external_id: Some(s.session_id.clone()),
                        brief: s.title.clone(),
                        cwd: Some(s.project_dir.clone()),
                        model: s.model.clone(),
                        mode: None,
                        status: "done".to_string(),
                        output_path: None,
                        output_truncated: None,
                        error: None,
                        artifacts: None,
                        parent_id: None,
                        owner: "agent".to_string(),
                        terminal_session_id: None,
                        started_at: Some(s.started_at.clone()),
                        ended_at: s.last_message_at.clone(),
                        last_seen_at: s.last_message_at.clone().or(Some(s.started_at.clone())),
                        expires_at: None,
                    });
                }
            }
            Err(e) => {
                log::debug!(target: "ikenga::chi", "claude_list_sessions failed: {e}");
            }
        }
    }

    rows.sort_by(|a, b| {
        b.last_seen_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.last_seen_at.as_deref().unwrap_or(""))
    });

    let limit = limit.unwrap_or(50).clamp(1, 200) as usize;
    rows.truncate(limit);
    Ok(rows)
}

/// Cancel a Chi run. Kills the engine child process.
#[tauri::command]
pub async fn chi_cancel(
    db: State<'_, Arc<PaDb>>,
    runtime: State<'_, Arc<ChiRuntime>>,
    #[allow(non_snake_case)] runId: String,
) -> Result<ChiRunResult, String> {
    let run_id = runId;
    let row = cache_get(&db, &run_id)
        .await?
        .ok_or_else(|| format!("chi run not found: {run_id}"))?;

    if let Some(handle) = runtime.remove(&run_id).await {
        handle.cancelled.store(true, Ordering::SeqCst);
        let mut child = handle.child.lock().await;
        if let Err(e) = child.start_kill() {
            return Err(format!("kill child: {e}"));
        }
    }

    // A persistent run lives in a detached tmux session, so killing the local
    // child leaves the work running. `terminal_session_id` is written by the
    // spawn path for exactly this purpose; without this the cancel was
    // cosmetic for persistent runs.
    if let Some(session_name) = row.terminal_session_id.as_deref() {
        if multiplexer::session_alive(session_name) {
            multiplexer::kill_tmux_session(session_name);
        }
    }

    cache_update_status(&db, &run_id, "cancelled", None).await?;

    Ok(ChiRunResult {
        run_id: row.run_id,
        status: "cancelled".to_string(),
        output: None,
        output_truncated: None,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> PaDb {
        let file_name = format!("ikenga-chi-test-{}.db", uuid::Uuid::new_v4());
        let db_path = std::env::temp_dir().join(file_name);
        PaDb::new(db_path)
    }

    #[tokio::test]
    async fn chi_cache_round_trip() {
        let db = test_db().await;
        let cache = ChiCache::new(std::env::temp_dir());
        cache.ensure_cache_dir().unwrap();

        let run_id = uuid::Uuid::new_v4().to_string();
        let output_path = cache.run_output_path(&run_id);
        let opts = ChiRunOpts {
            engine_id: "claude-code".into(),
            prompt: "hello".into(),
            cwd: Some("/tmp".into()),
            model: None,
            mode: None,
            timeout_seconds: None,
            parent_id: None,
            resume_session_id: None,
            persistent: false,
        };

        cache_insert(&db, &run_id, &opts, &output_path, "cli")
            .await
            .unwrap();

        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.run_id, run_id);
        assert_eq!(row.engine_id, "claude-code");
        assert_eq!(row.status, "queued");
        assert_eq!(row.cwd.as_deref(), Some("/tmp"));

        cache_update_status(&db, &run_id, "running", None)
            .await
            .unwrap();
        let row = cache_get(&db, &run_id).await.unwrap().unwrap();
        assert_eq!(row.status, "running");
        assert!(row.last_seen_at.is_some());

        let rows = cache_list(&db, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run_id, run_id);
    }

    #[tokio::test]
    async fn chi_output_file_round_trip() {
        let cache = ChiCache::new(std::env::temp_dir());
        cache.ensure_cache_dir().unwrap();
        let path = cache.run_output_path("test-run");
        write_output_file(&path, "partial output", None).await.unwrap();
        let file = read_output_file(&path).await.unwrap();
        assert_eq!(file.output.as_deref(), Some("partial output"));
    }

    #[test]
    fn test_build_engine_command_antigravity() {
        let cmd = build_engine_command(
            "antigravity-cli",
            "hello",
            "/tmp",
            Some("gemini-2.0-flash"),
            Some("plan"),
            Some("conv-123"),
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "agy");
        let args: Vec<&str> = cmd.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, vec![
            "-p", "hello",
            "--output-format", "stream-json",
            "--conversation", "conv-123",
            "--model", "gemini-2.0-flash",
            "--mode", "plan"
        ]);
    }

    #[test]
    fn test_build_engine_command_opencode() {
        let cmd = build_engine_command(
            "opencode",
            "fix the bug",
            "/tmp",
            Some("claude-3-7-sonnet"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "opencode");
        let args: Vec<&str> = cmd.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, vec![
            "run",
            "-p", "fix the bug",
            "--model", "claude-3-7-sonnet",
        ]);
    }

    #[test]
    fn test_build_engine_command_pi() {
        let cmd = build_engine_command(
            "pi",
            "refactor this file",
            "/tmp",
            Some("claude-3-7-sonnet"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(cmd.as_std().get_program(), "pi");
        let args: Vec<&str> = cmd.as_std().get_args().map(|s| s.to_str().unwrap()).collect();
        assert_eq!(args, vec![
            "-p", "refactor this file",
            "--model", "claude-3-7-sonnet",
        ]);
    }
}
