//! Pkg helper execution, assignment gate (R4-2), trust gate (R4-4)
//! and per-account streaming sidecar manager (R4-3).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::Mutex as AsyncMutex;

use crate::access::AccessCtx;
use crate::executor::{PipedOpts, PrincipalId, SpawnSpec, StdioMode};
use crate::pkg::manifest::{McpServer, Package};
use crate::server::shared::chi_exec;

// ─── Wire Result Types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PkgMcpCallResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PkgInvokeResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
}

impl PkgInvokeResult {
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            stdout: None,
            stderr: None,
            exit_code: None,
            timed_out: false,
        }
    }
}

/// Resolve the operator root / broker directory where global config files live
/// (`pkg-assignments.json`, `pkg-trust.json`).
///
/// Under T1, the child's `data_dir` is `<root>/principals/<id>/data`. The
/// operator root is 3 levels up (`<root>`), which is owned by root (0755) and
/// never writable by a principal.
///
/// Under T0, `data_dir` itself is the operator root.
pub fn resolve_operator_dir(data_dir: Option<&Path>) -> Option<PathBuf> {
    let data_dir = data_dir?;
    if data_dir.ends_with("data") {
        if let Some(principal_dir) = data_dir.parent() {
            if let Some(principals) = principal_dir.parent() {
                if principals.ends_with("principals") {
                    if let Some(root) = principals.parent() {
                        return Some(root.to_path_buf());
                    }
                }
            }
        }
    }
    Some(data_dir.to_path_buf())
}

// ─── Pkg Assignment Gate (R4-2) ───────────────────────────────────────────────
///
/// `pkg-assignments.json` defines per-account pkg assignment policies.
/// Location: `<operator_dir>/pkg-assignments.json` (or `<data_dir>/pkg-assignments.json`).
///
/// Schema:
/// Map of package pattern (`*`, `com.ikenga.*`, or exact pkg ID) to either:
/// - A string `"all"` or `"*"`: accessible to all authenticated accounts.
/// - A list of principal UUID strings: `["01923456-...", ...]`: accessible only to specified principals.
///
/// Safe default behavior:
/// - If `pkg-assignments.json` does not exist, built-in pkgs (`com.ikenga.*`) default to `"*"`
///   (all authenticated accounts can run them, preserving core UX like git branch chip).
/// - Third-party packages default to denied unless assigned or caller is an admin.
/// - Box admins (`ctx.is_admin == true`) always bypass pkg assignment checks.
/// - Requests without an access context (`ctx.is_none()`) are strictly denied.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PkgAssignmentValue {
    All(String),
    List(Vec<String>),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PkgAssignments(pub HashMap<String, PkgAssignmentValue>);

/// Check if `pkg_id` is assigned to `principal_id`.
/// Supports exact matches and wildcard glob patterns (e.g. `*`, `com.ikenga.*`).
impl PkgAssignments {
    pub fn is_assigned(
        &self,
        pkg_id: &str,
        principal_id: Option<PrincipalId>,
        is_admin: bool,
    ) -> bool {
        // Admins are always allowed (R4-2).
        if is_admin {
            return true;
        }
        for (pattern, assignment) in &self.0 {
            let matched = if pattern == "*" || pattern == pkg_id {
                true
            } else if pattern.contains('*') {
                glob::Pattern::new(pattern)
                    .map(|p| p.matches(pkg_id))
                    .unwrap_or(false)
            } else {
                false
            };
            if matched {
                let allowed = match assignment {
                    PkgAssignmentValue::All(s) => s == "*" || s.eq_ignore_ascii_case("all"),
                    PkgAssignmentValue::List(list) => {
                        if let Some(pid) = principal_id {
                            let pid_str = pid.to_string();
                            list.iter().any(|item| {
                                item == &pid_str || item == "*" || item.eq_ignore_ascii_case("all")
                            })
                        } else {
                            false
                        }
                    }
                };
                if allowed {
                    return true;
                }
            }
        }
        false
    }
}

/// Check if `pkg_id` is assigned to the caller.
/// Admins always allowed; otherwise default-deny against `pkg-assignments.json`.
/// With no access context, deny.
/// Safe default: if `pkg-assignments.json` is missing, built-in pkgs (`com.ikenga.*`)
/// default to assigned for everyone (`"*"`), while third-party pkgs default-deny.
pub fn check_pkg_assignment(
    data_dir: Option<&Path>,
    pkg_id: &str,
    ctx: Option<&AccessCtx>,
) -> Result<(), String> {
    let ctx = ctx.ok_or_else(|| format!("pkg `{pkg_id}` denied: no access context"))?;
    if ctx.is_admin {
        return Ok(());
    }

    let principal_id = Some(ctx.principal_id);
    let operator_dir = resolve_operator_dir(data_dir);
    let assignments_file = operator_dir
        .as_ref()
        .map(|d| d.join("pkg-assignments.json"));

    let mut assignments = match assignments_file.as_ref() {
        Some(path) if path.exists() => std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<PkgAssignments>(&s).ok())
            .unwrap_or_default(),
        _ => PkgAssignments::default(),
    };
    assignments
        .0
        .entry("com.ikenga.*".to_string())
        .or_insert_with(|| PkgAssignmentValue::All("*".to_string()));

    if assignments.is_assigned(pkg_id, principal_id, ctx.is_admin) {
        Ok(())
    } else {
        let id_str = ctx.principal_id.to_string();
        Err(format!(
            "pkg `{pkg_id}` is not assigned to account `{id_str}`"
        ))
    }
}

// ─── Pkg Trust Gate (R4-4) ───────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PkgTrustRecord {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub snapshot_hash: Option<String>,
    #[serde(default)]
    pub trust_state: String,
    #[serde(default)]
    pub granted_at_ms: Option<i64>,
    #[serde(default)]
    pub granted_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PkgTrustFormat {
    Map(HashMap<String, PkgTrustRecord>),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PkgTrustRecords(pub Option<PkgTrustFormat>);

impl PkgTrustRecords {
    pub fn is_approved(&self, pkg_id: &str, version: &str, expected_hash: &str) -> bool {
        match &self.0 {
            Some(PkgTrustFormat::Map(map)) => {
                if let Some(record) = map.get(pkg_id) {
                    let state_ok =
                        record.trust_state == "granted" || record.trust_state == "auto_trusted";
                    let version_ok = record.version.as_deref() == Some(version);
                    let hash_ok = record.snapshot_hash.as_deref() == Some(expected_hash);
                    state_ok && version_ok && hash_ok
                } else {
                    false
                }
            }
            None => false,
        }
    }
}

/// True when `path` resolves to somewhere inside `dir`.
///
/// Both sides are canonicalized, so `..` segments and symlinks cannot make a
/// path outside `dir` look like it is inside. If either side cannot be
/// resolved (missing, unreadable) the answer is no.
fn is_within_dir(dir: &Path, path: &Path) -> bool {
    match (std::fs::canonicalize(dir), std::fs::canonicalize(path)) {
        (Ok(dir), Ok(path)) => path != dir && path.starts_with(dir),
        _ => false,
    }
}

/// Determine whether a package is a built-in: it was loaded from the
/// configured `--pkgs-dir` (and its directory really resolves inside it).
/// With no `--pkgs-dir` nothing is built-in.
pub fn pkg_source(pkgs_dir: Option<&Path>, pkg: &Package) -> crate::pkg::InstallSource {
    let path = &pkg.install_path;
    let is_builtin = pkgs_dir.is_some_and(|dir| is_within_dir(dir, path));
    if is_builtin {
        crate::pkg::InstallSource::Builtin
    } else {
        crate::pkg::InstallSource::Local {
            path: path.display().to_string(),
        }
    }
}

/// Check if `pkg` is trusted to run on this server.
/// Builtin pkgs (`com.ikenga.*` installed as Builtin) and pkgs declaring no sensitive permissions
/// are auto-trusted/granted.
/// Pkgs with sensitive permissions require an admin approval recorded box-wide in `pkg-trust.json`
/// bound to the pkg version and snapshot hash.
pub fn check_pkg_trust(
    data_dir: Option<&Path>,
    pkgs_dir: Option<&Path>,
    pkg: &Package,
) -> Result<(), String> {
    let pkg_id = &pkg.manifest.id;
    let source = pkg_source(pkgs_dir, pkg);

    if crate::pkg::trust_state::is_auto_trusted(pkg_id, &source) {
        return Ok(());
    }

    let sensitive = crate::pkg::trust_state::summarize_sensitive(&pkg.manifest.permissions);
    if sensitive.shell_execute.is_empty()
        && sensitive.fs_write_outside_sandbox.is_empty()
        && sensitive.net.is_empty()
        && sensitive.vault_keys.is_empty()
    {
        return Ok(());
    }

    let expected_hash = crate::pkg::trust_state::snapshot_hash(&sensitive);
    let operator_dir = resolve_operator_dir(data_dir);
    let trust_file = operator_dir.as_ref().map(|d| d.join("pkg-trust.json"));
    let trust_records = trust_file
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<PkgTrustRecords>(&s).ok())
        .unwrap_or_default();

    if trust_records.is_approved(pkg_id, &pkg.manifest.version, &expected_hash) {
        Ok(())
    } else {
        Err(format!(
            "pkg `{pkg_id}` requires admin trust approval for sensitive permissions (version {}, hash {expected_hash})",
            pkg.manifest.version
        ))
    }
}

/// Check if `pkg` is elevated-trusted for `pkg_invoke`.
pub fn check_invoke_trust(
    data_dir: Option<&Path>,
    pkgs_dir: Option<&Path>,
    pkg: &Package,
) -> Result<(), String> {
    let pkg_id = &pkg.manifest.id;
    let source = pkg_source(pkgs_dir, pkg);

    if crate::pkg::trust_state::is_auto_trusted(pkg_id, &source) {
        return Ok(());
    }

    let sensitive = crate::pkg::trust_state::summarize_sensitive(&pkg.manifest.permissions);
    let expected_hash = crate::pkg::trust_state::snapshot_hash(&sensitive);

    let operator_dir = resolve_operator_dir(data_dir);
    let trust_file = operator_dir.as_ref().map(|d| d.join("pkg-trust.json"));
    let trust_records = trust_file
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<PkgTrustRecords>(&s).ok())
        .unwrap_or_default();

    if trust_records.is_approved(pkg_id, &pkg.manifest.version, &expected_hash) {
        Ok(())
    } else {
        Err(format!("pkg `{pkg_id}` host.invoke requires a trusted pkg"))
    }
}

// ─── Glob Matcher ─────────────────────────────────────────────────────────────

pub fn matches_glob_list(globs: &[String], command: &str) -> bool {
    for raw in globs {
        if let Ok(pat) = glob::Pattern::new(raw) {
            if pat.matches(command) {
                return true;
            }
        }
    }
    false
}

// ─── MCP Call Runner ──────────────────────────────────────────────────────────

pub fn pick_server<'a>(servers: &'a [McpServer], name: &str) -> anyhow::Result<&'a McpServer> {
    if servers.is_empty() {
        return Err(anyhow::anyhow!("package declares no mcp servers"));
    }
    if name.is_empty() {
        if servers.len() == 1 {
            return Ok(&servers[0]);
        }
        return Err(anyhow::anyhow!(
            "package declares {} mcp servers; specify which by name",
            servers.len()
        ));
    }
    servers
        .iter()
        .find(|s| s.name == name)
        .ok_or_else(|| anyhow::anyhow!("no mcp server named `{name}`"))
}

pub async fn call_mcp_tool(
    install_path: &Path,
    server: &McpServer,
    tool: &str,
    args: Value,
    pkg_id: &str,
    shell_execute: &[String],
) -> anyhow::Result<Value> {
    crate::pkg::trust_state::check_shell_execute(pkg_id, shell_execute, &server.command)
        .map_err(|denial| anyhow::anyhow!("shell.execute denied: {denial:?}"))?;

    let program = crate::runtime::resolve_command(&server.command);
    let mut cmd = SpawnSpec::new(program);
    cmd.args(&server.args);
    cmd.current_dir(install_path);
    chi_exec::inherit_scrubbed_env(&mut cmd);
    for (k, v) in &server.env {
        cmd.env(k, v);
    }
    cmd.env("PATH", crate::runtime::augmented_path());

    let opts = PipedOpts {
        stdin: StdioMode::Piped,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        kill_on_drop: true,
        no_console_window: true,
        detached: false,
        new_process_group: true,
    };

    let mut child = crate::executor::current()
        .spawn_piped(cmd, opts)
        .map_err(|e| anyhow::anyhow!("spawn `{} {:?}`: {e}", server.command, server.args))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let mut reader = BufReader::new(stdout).lines();

    if let Some(stderr) = child.stderr.take() {
        let tool_name = tool.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("[pkg_mcp.{tool_name}.stderr] {line}");
            }
        });
    }

    let result = tokio::time::timeout(Duration::from_secs(15), async {
        // 1. initialize
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "ikenga-server", "version": "0.1.0" }
                }
            }),
        )
        .await?;
        expect_response(&mut reader, 1).await?;

        // 2. initialized notification
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }),
        )
        .await?;

        // 3. tools/call
        write_json_line(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            }),
        )
        .await?;
        expect_response(&mut reader, 2).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("mcp tool `{tool}` timed out"))??;

    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    Ok(result)
}

async fn expect_response<R>(
    reader: &mut tokio::io::Lines<BufReader<R>>,
    id: i64,
) -> anyhow::Result<Value>
where
    R: tokio::io::AsyncRead + Unpin,
{
    loop {
        let line = match reader.next_line().await? {
            Some(l) => l,
            None => return Err(anyhow::anyhow!("mcp server closed stdout before id={id}")),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let env: Value = match serde_json::from_str(trimmed) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("[pkg_mcp] non-JSON line dropped: {e} :: {trimmed:.120}");
                continue;
            }
        };
        if env.get("id").and_then(|v| v.as_i64()) == Some(id) {
            if let Some(err) = env.get("error") {
                return Err(anyhow::anyhow!("mcp server error: {err}"));
            }
            return Ok(env.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

async fn write_json_line(stdin: &mut ChildStdin, msg: &Value) -> anyhow::Result<()> {
    let mut buf = serde_json::to_vec(msg)?;
    buf.push(b'\n');
    stdin.write_all(&buf).await?;
    stdin.flush().await?;
    Ok(())
}

// ─── Streaming Sidecar Manager (R4-3) ─────────────────────────────────────────

/// Key into the streaming sidecar map: `(caller_principal, pkg_id, sidecar_name)`.
/// This guarantees sidecars are isolated per account (R4-3) and never shared.
pub type SidecarKey = (Option<PrincipalId>, String, String);

pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    if pid > 1 {
        unsafe {
            let _ = libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            let _ = libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

pub struct SidecarChild {
    pub stdin: Arc<AsyncMutex<ChildStdin>>,
    pub pid: u32,
    pub last_activity: Arc<AsyncMutex<Instant>>,
    pub cancel: tokio::sync::watch::Sender<bool>,
}

impl std::fmt::Debug for SidecarChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarChild")
            .field("pid", &self.pid)
            .finish()
    }
}

impl Drop for SidecarChild {
    fn drop(&mut self) {
        kill_process_group(self.pid);
    }
}

pub const MAX_SIDECARS_PER_ACCOUNT: usize = 8;

#[derive(Clone, Default)]
pub struct DaemonSidecarManager {
    children: Arc<AsyncMutex<HashMap<SidecarKey, Arc<SidecarChild>>>>,
    /// Number of times any idle reaper owned by this manager (and its clones)
    /// has gone round its loop. A healthy reaper wakes once on spawn and then
    /// every 10s; a reaper that spins shows up as thousands. Per-manager, so
    /// parallel tests cannot see each other's reapers.
    reaper_wakeups: Arc<AtomicU64>,
}

impl DaemonSidecarManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Total reaper loop iterations so far (see `reaper_wakeups`).
    pub fn reaper_wakeups(&self) -> u64 {
        self.reaper_wakeups.load(Ordering::Relaxed)
    }

    pub async fn get_or_spawn<F, Fut>(
        &self,
        key: SidecarKey,
        spawn_fn: F,
    ) -> Result<Arc<SidecarChild>, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<
            Output = Result<(ChildStdin, u32, tokio::process::ChildStdout), String>,
        >,
    {
        let mut lock = self.children.lock().await;
        if let Some(child) = lock.get(&key) {
            *child.last_activity.lock().await = Instant::now();
            return Ok(child.clone());
        }

        let count = lock.iter().filter(|(k, _)| k.0 == key.0).count();
        if count >= MAX_SIDECARS_PER_ACCOUNT {
            return Err(format!(
                "maximum concurrent sidecars per account ({MAX_SIDECARS_PER_ACCOUNT}) reached"
            ));
        }

        let (stdin, pid, stdout) = spawn_fn().await?;
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        let last_activity = Arc::new(AsyncMutex::new(Instant::now()));

        let child = Arc::new(SidecarChild {
            stdin: Arc::new(AsyncMutex::new(stdin)),
            pid,
            last_activity: last_activity.clone(),
            cancel: cancel_tx,
        });

        lock.insert(key.clone(), child.clone());

        // Background stdout reader task
        let children_map = self.children.clone();
        let key_for_reader = key.clone();
        let child_for_reader = child.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    tracing::debug!("[daemon_sidecar {:?}] {trimmed}", key_for_reader);
                }
            }
            tracing::info!("[daemon_sidecar {:?}] stdout closed", key_for_reader);
            let mut lock = children_map.lock().await;
            if let Some(c) = lock.get(&key_for_reader) {
                if Arc::ptr_eq(c, &child_for_reader) {
                    if let Some(removed) = lock.remove(&key_for_reader) {
                        let _ = removed.cancel.send(true);
                        kill_process_group(removed.pid);
                    }
                }
            }
        });

        // Idle reaper task (5 min idle timeout, R4-3)
        let children_map = self.children.clone();
        let key_for_reap = key.clone();
        let last_act = last_activity.clone();
        let child_weak = Arc::downgrade(&child);
        let wakeups = self.reaper_wakeups.clone();
        tokio::spawn(async move {
            loop {
                wakeups.fetch_add(1, Ordering::Relaxed);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        let elapsed = last_act.lock().await.elapsed();
                        if elapsed > Duration::from_secs(300) {
                            tracing::info!(
                                "[daemon_sidecar {:?}] idle timeout reached ({:?}), stopping",
                                key_for_reap,
                                elapsed
                            );
                            if let Some(child_for_reap) = child_weak.upgrade() {
                                let mut lock = children_map.lock().await;
                                if let Some(c) = lock.get(&key_for_reap) {
                                    if Arc::ptr_eq(c, &child_for_reap) {
                                        if let Some(child) = lock.remove(&key_for_reap) {
                                            let _ = child.cancel.send(true);
                                            kill_process_group(child.pid);
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                    res = cancel_rx.changed() => {
                        if res.is_err() || *cancel_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(child)
    }

    pub async fn send_line(&self, child: &SidecarChild, message: &str) -> Result<(), String> {
        let mut stdin = child.stdin.lock().await;
        stdin
            .write_all(message.as_bytes())
            .await
            .map_err(|e| format!("write stdin: {e}"))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("write stdin newline: {e}"))?;
        stdin
            .flush()
            .await
            .map_err(|e| format!("flush stdin: {e}"))?;
        *child.last_activity.lock().await = Instant::now();
        Ok(())
    }

    pub async fn shutdown(&self, key: &SidecarKey) -> bool {
        let mut lock = self.children.lock().await;
        if let Some(child) = lock.remove(key) {
            let _ = child.cancel.send(true);
            kill_process_group(child.pid);
            let mut stdin = child.stdin.lock().await;
            let _ = stdin.shutdown().await;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{CapSet, RequestMeta, Tier, Via};
    use crate::pkg::manifest::Package;
    use serde_json::json;
    use tempfile::tempdir;

    fn mock_ctx(admin: bool) -> AccessCtx {
        AccessCtx {
            principal_id: crate::executor::PrincipalId::new_v7(),
            via: Via::Relayed,
            device_id: None,
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::of(&[]),
            admin_strength: admin,
            is_admin: admin,
            meta: RequestMeta::default(),
        }
    }

    #[test]
    fn test_matches_glob() {
        let globs = vec!["git*".to_string(), "cargo *".to_string()];
        assert!(matches_glob_list(&globs, "git status"));
        assert!(matches_glob_list(&globs, "cargo test"));
        assert!(!matches_glob_list(&globs, "rm -rf /"));
    }

    #[test]
    fn test_assignment_gate() {
        let tmp = tempdir().unwrap();
        let operator_dir = tmp.path();

        // 0. Deny when no access context
        assert!(check_pkg_assignment(Some(operator_dir), "pkg-a", None).is_err());

        // 1. Admin caller is always allowed
        let admin_ctx = mock_ctx(true);
        assert!(check_pkg_assignment(Some(operator_dir), "pkg-a", Some(&admin_ctx)).is_ok());

        // 2. Non-admin caller without assignment file -> default deny for third-party (R4-2)
        let member_ctx = mock_ctx(false);
        let err = check_pkg_assignment(Some(operator_dir), "pkg-a", Some(&member_ctx)).unwrap_err();
        assert!(err.contains("not assigned"), "got: {err}");

        // Built-ins default to assigned for everyone when assignments file is missing
        assert!(
            check_pkg_assignment(Some(operator_dir), "com.ikenga.git", Some(&member_ctx)).is_ok()
        );

        // 3. Write pkg-assignments.json granting member_ctx's principal_id
        let assignments = json!({
            "pkg-a": [member_ctx.principal_id.to_string(), "user-3"]
        });
        std::fs::write(
            operator_dir.join("pkg-assignments.json"),
            assignments.to_string(),
        )
        .unwrap();

        // Now member is allowed
        assert!(check_pkg_assignment(Some(operator_dir), "pkg-a", Some(&member_ctx)).is_ok());

        // But user-4 is denied
        let user4_ctx = mock_ctx(false);
        let err = check_pkg_assignment(Some(operator_dir), "pkg-a", Some(&user4_ctx)).unwrap_err();
        assert!(err.contains("not assigned"), "got: {err}");

        // Wildcard assignment allows all
        let assignments_wildcard = json!({
            "pkg-a": ["*"]
        });
        std::fs::write(
            operator_dir.join("pkg-assignments.json"),
            assignments_wildcard.to_string(),
        )
        .unwrap();
        assert!(check_pkg_assignment(Some(operator_dir), "pkg-a", Some(&user4_ctx)).is_ok());
    }

    #[test]
    fn test_builtin_means_inside_configured_pkgs_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let pkgs_dir = root.join("pkgs");
        std::fs::create_dir_all(&pkgs_dir).unwrap();

        let pkg_at = |id: &str, install_path: PathBuf| -> Package {
            std::fs::create_dir_all(&install_path).unwrap();
            let manifest: crate::pkg::manifest::Manifest = serde_json::from_value(json!({
                "id": id, "name": id, "version": "1.0.0", "ikenga_api": "1",
                "permissions": { "shell.execute": ["*"] },
            }))
            .unwrap();
            Package {
                manifest,
                install_path,
            }
        };
        let is_builtin = |pkg: &Package| {
            matches!(
                pkg_source(Some(&pkgs_dir), pkg),
                crate::pkg::InstallSource::Builtin
            )
        };

        // Loaded from the configured pkgs dir: built-in.
        let inside = pkg_at("com.ikenga.git", pkgs_dir.join("com.ikenga.git"));
        assert!(is_builtin(&inside));
        assert!(check_pkg_trust(Some(&root), Some(&pkgs_dir), &inside).is_ok());
        assert!(check_invoke_trust(Some(&root), Some(&pkgs_dir), &inside).is_ok());

        // A directory merely NAMED `builtin-pkgs`, elsewhere on disk.
        let named = pkg_at(
            "com.ikenga.evil",
            root.join("x/builtin-pkgs/com.ikenga.evil"),
        );
        assert!(
            !is_builtin(&named),
            "a `builtin-pkgs` path component proves nothing"
        );
        assert!(check_pkg_trust(Some(&root), Some(&pkgs_dir), &named).is_err());
        assert!(check_invoke_trust(Some(&root), Some(&pkgs_dir), &named).is_err());

        // The old hard-coded location, which is not the configured dir.
        let opt = root.join("opt/ikenga/pkgs/com.ikenga.evil");
        assert!(!is_builtin(&pkg_at("com.ikenga.evil", opt)));

        // `..` that starts inside the pkgs dir and climbs out of it.
        let outside = root.join("outside/com.ikenga.evil");
        std::fs::create_dir_all(&outside).unwrap();
        let dotdot = pkg_at(
            "com.ikenga.evil",
            pkgs_dir.join("../outside/com.ikenga.evil"),
        );
        assert!(!is_builtin(&dotdot), "`..` must not smuggle a path in");
        assert!(check_pkg_trust(Some(&root), Some(&pkgs_dir), &dotdot).is_err());

        // A symlink inside the pkgs dir that points out of it.
        #[cfg(unix)]
        {
            let link = pkgs_dir.join("com.ikenga.linked");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let linked = pkg_at("com.ikenga.evil", link);
            assert!(
                !is_builtin(&linked),
                "a symlink out of the pkgs dir is not built-in"
            );
            assert!(check_invoke_trust(Some(&root), Some(&pkgs_dir), &linked).is_err());
        }

        // No pkgs dir configured: nothing is built-in.
        assert!(!matches!(
            pkg_source(None, &inside),
            crate::pkg::InstallSource::Builtin
        ));
        // The pkgs dir itself is not a pkg inside it.
        let itself = pkg_at("com.ikenga.evil", pkgs_dir.clone());
        assert!(!is_builtin(&itself));
    }

    #[test]
    fn test_trust_gate() {
        let tmp = tempdir().unwrap();
        let operator_dir = tmp.path();
        let pkgs_dir = operator_dir.join("pkgs");
        std::fs::create_dir_all(&pkgs_dir).unwrap();

        let make_pkg = |id: &str, shell_exec: Vec<&str>, is_builtin: bool| -> Package {
            let manifest: crate::pkg::manifest::Manifest = serde_json::from_value(json!({
                "id": id,
                "name": id,
                "version": "1.0.0",
                "ikenga_api": "1",
                "permissions": {
                    "shell.execute": shell_exec,
                },
            }))
            .unwrap();
            let install_path = if is_builtin {
                pkgs_dir.join(id)
            } else {
                operator_dir.join(id)
            };
            std::fs::create_dir_all(&install_path).unwrap();
            Package {
                manifest,
                install_path,
            }
        };

        // 1. Built-in com.ikenga.* is auto-trusted
        let ikenga_builtin = make_pkg("com.ikenga.official", vec!["sh"], true);
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &ikenga_builtin).is_ok());

        // Spoofed com.ikenga.* from non-builtin source is NOT auto-trusted (R4-4 Blocker 3)
        let ikenga_spoof = make_pkg("com.ikenga.spoofed", vec!["sh"], false);
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &ikenga_spoof).is_err());

        // 2. Non-official pkg with non-sensitive perms -> auto-trusted
        let benign_pkg = make_pkg("com.other.benign", vec![], false);
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &benign_pkg).is_ok());

        // 3. Non-official pkg with sensitive perms -> unapproved without pkg-trust.json
        let mut sensitive_pkg = make_pkg("com.other.sensitive", vec!["rm"], false);
        let err = check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &sensitive_pkg).unwrap_err();
        assert!(err.contains("requires admin trust approval"), "got: {err}");

        // 4. Admin approves sensitive pkg in pkg-trust.json bound to version + snapshot_hash (R4-4 Blocker 2)
        let hash = crate::pkg::trust_state::snapshot_hash(
            &crate::pkg::trust_state::summarize_sensitive(&sensitive_pkg.manifest.permissions),
        );
        let trust_json = json!({
            "com.other.sensitive": {
                "version": "1.0.0",
                "snapshot_hash": hash,
                "trust_state": "granted"
            }
        });
        std::fs::write(operator_dir.join("pkg-trust.json"), trust_json.to_string()).unwrap();
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &sensitive_pkg).is_ok());

        // Version update (1.0.1) without re-approval is refused
        sensitive_pkg.manifest.version = "1.0.1".into();
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &sensitive_pkg).is_err());

        // Wider permissions without re-approval is refused
        sensitive_pkg.manifest.version = "1.0.0".into();
        sensitive_pkg.manifest.permissions.shell_execute = vec!["rm".into(), "*".into()];
        assert!(check_pkg_trust(Some(operator_dir), Some(&pkgs_dir), &sensitive_pkg).is_err());
    }

    #[tokio::test]
    async fn test_cross_account_sidecar_isolation() {
        let manager = DaemonSidecarManager::new();

        let acc_a = Some(PrincipalId::new_v7());
        let acc_b = Some(PrincipalId::new_v7());

        let key_a = (acc_a, "pkg-1".to_string(), "sidecar-1".to_string());
        let key_b = (acc_b, "pkg-1".to_string(), "sidecar-1".to_string());

        // Helper to spawn a mock child process: cat
        let spawn_cat = || async {
            let mut cmd = tokio::process::Command::new("cat");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        let child_a = manager
            .get_or_spawn(key_a.clone(), spawn_cat)
            .await
            .unwrap();
        let child_b = manager
            .get_or_spawn(key_b.clone(), spawn_cat)
            .await
            .unwrap();

        // R4-3: Account A and B get distinct children and separate PIDs!
        assert_ne!(child_a.pid, child_b.pid);

        // Sending to A
        assert!(manager.send_line(&child_a, "hello a").await.is_ok());
        // Sending to B
        assert!(manager.send_line(&child_b, "hello b").await.is_ok());

        // Shutting down A leaves B active
        assert!(manager.shutdown(&key_a).await);
        assert!(!manager.shutdown(&key_a).await); // already shut down
        assert!(manager.shutdown(&key_b).await); // B was still active
    }

    #[tokio::test]
    async fn test_sidecar_per_account_cap() {
        let manager = DaemonSidecarManager::new();
        let acc = Some(PrincipalId::new_v7());

        let spawn_cat = || async {
            let mut cmd = tokio::process::Command::new("cat");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        for i in 0..MAX_SIDECARS_PER_ACCOUNT {
            let key = (acc, "pkg".to_string(), format!("s-{i}"));
            assert!(manager.get_or_spawn(key, spawn_cat).await.is_ok());
        }

        // 9th sidecar for the same account must be rejected
        let key_overflow = (acc, "pkg".to_string(), "overflow".to_string());
        let err = manager
            .get_or_spawn(key_overflow, spawn_cat)
            .await
            .unwrap_err();
        assert!(err.contains("maximum concurrent sidecars"), "{err}");
    }

    #[tokio::test]
    async fn test_sidecar_shutdown_kills_group_including_grandchild() {
        let manager = DaemonSidecarManager::new();
        let acc = Some(PrincipalId::new_v7());
        let key = (acc, "pkg".to_string(), "sidecar".to_string());

        let spawn_tree = || async {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("sleep 1000 & wait");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        let child = manager.get_or_spawn(key.clone(), spawn_tree).await.unwrap();
        let pid = child.pid;
        #[cfg(unix)]
        unsafe {
            assert_eq!(libc::kill(pid as libc::pid_t, 0), 0, "child must be alive");
        }

        assert!(manager.shutdown(&key).await);

        tokio::time::sleep(Duration::from_millis(50)).await;

        #[cfg(unix)]
        unsafe {
            assert_ne!(
                libc::kill(pid as libc::pid_t, 0),
                0,
                "child should be killed"
            );
            assert_ne!(
                libc::kill(-(pid as libc::pid_t), 0),
                0,
                "child process group should be killed"
            );
        }
    }

    #[tokio::test]
    async fn test_sidecar_restart_not_killed_by_stale_reader() {
        let manager = DaemonSidecarManager::new();
        let acc = Some(PrincipalId::new_v7());
        let key = (acc, "pkg".to_string(), "sidecar".to_string());

        let spawn_inst1 = || async {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("read line; sleep 0.05");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        let child_1 = manager
            .get_or_spawn(key.clone(), spawn_inst1)
            .await
            .unwrap();
        let pid_1 = child_1.pid;

        // Shutdown instance 1: closes stdin, so inst 1 will wake up, sleep 50ms, and exit
        assert!(manager.shutdown(&key).await);

        // Immediately spawn instance 2 under same key while instance 1's stdout is still open
        let spawn_inst2 = || async {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("while true; do sleep 1; done");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        let child_2 = manager
            .get_or_spawn(key.clone(), spawn_inst2)
            .await
            .unwrap();
        let pid_2 = child_2.pid;
        assert_ne!(pid_1, pid_2);

        // Wait 150ms for instance 1 to finish sleeping, exit, and its reader task to hit EOF
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Instance 2 must NOT have been killed by instance 1's stale stdout reader!
        #[cfg(unix)]
        unsafe {
            assert_eq!(
                libc::kill(pid_2 as libc::pid_t, 0),
                0,
                "child 2 must still be alive"
            );
        }

        // And instance 2 must still be in the manager map
        assert!(
            manager.shutdown(&key).await,
            "child 2 must be in manager map"
        );
    }

    #[tokio::test]
    async fn test_reaper_no_spin_after_natural_exit() {
        let manager = DaemonSidecarManager::new();
        let acc = Some(PrincipalId::new_v7());

        let spawn_exit = || async {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("exit 0");
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn().map_err(|e| e.to_string())?;
            let stdin = child.stdin.take().unwrap();
            let stdout = child.stdout.take().unwrap();
            let pid = child.id().unwrap_or(0);
            Ok((stdin, pid, stdout))
        };

        for i in 0..4 {
            let key = (acc, "pkg".to_string(), format!("s-{i}"));
            let child = manager.get_or_spawn(key, spawn_exit).await.unwrap();
            drop(child);
        }

        // Let every sidecar exit, its reader cancel the reaper, and the
        // reaper finish. A reaper that treats a closed cancel channel as
        // "keep going" would instead loop here without ever sleeping.
        tokio::time::sleep(Duration::from_millis(400)).await;

        // Healthy: one wakeup per reaper on spawn (4 reapers), plus at most
        // one more each for the cancel signal. Spinning: thousands.
        let wakeups = manager.reaper_wakeups();
        assert!(
            wakeups <= 8,
            "reaper tasks must terminate and not spin; {wakeups} wakeups for 4 reapers in 400ms"
        );
    }

    #[test]
    fn test_t0_owner_device_is_admin_for_assignment() {
        let tmp = tempdir().unwrap();
        let operator_dir = tmp.path();

        let owner_id = PrincipalId::new_v7();
        let other_id = PrincipalId::new_v7();

        // T0 owner device context has is_admin = true
        let owner_device_ctx = AccessCtx {
            principal_id: owner_id,
            via: Via::Device {
                device_id: "dev-owner".into(),
            },
            device_id: Some("dev-owner".into()),
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::of(&[]),
            admin_strength: true,
            is_admin: true,
            meta: RequestMeta::default(),
        };

        // Non-owner device context has is_admin = false
        let non_owner_device_ctx = AccessCtx {
            principal_id: other_id,
            via: Via::Device {
                device_id: "dev-other".into(),
            },
            device_id: Some("dev-other".into()),
            tier: Tier::Full,
            share: None,
            share_headers: false,
            caps: CapSet::of(&[]),
            admin_strength: false,
            is_admin: false,
            meta: RequestMeta::default(),
        };

        // Owner device is allowed to access third-party pkg even without assignments file
        assert!(
            check_pkg_assignment(Some(operator_dir), "com.acme.tool", Some(&owner_device_ctx))
                .is_ok()
        );

        // Non-owner device is denied
        assert!(check_pkg_assignment(
            Some(operator_dir),
            "com.acme.tool",
            Some(&non_owner_device_ctx)
        )
        .is_err());
    }

    #[test]
    fn test_assignment_merges_builtins_when_file_exists() {
        let tmp = tempdir().unwrap();
        let operator_dir = tmp.path();

        let member_ctx = mock_ctx(false);

        // Write pkg-assignments.json granting only a third-party pkg
        let assignments = json!({
            "com.acme.tool": [member_ctx.principal_id.to_string()]
        });
        std::fs::write(
            operator_dir.join("pkg-assignments.json"),
            assignments.to_string(),
        )
        .unwrap();

        // Built-ins (com.ikenga.*) MUST remain assigned even when assignments file exists
        assert!(
            check_pkg_assignment(Some(operator_dir), "com.ikenga.git", Some(&member_ctx)).is_ok(),
            "com.ikenga.* must remain assigned even when pkg-assignments.json exists"
        );
        // Explicitly assigned third-party pkg is allowed
        assert!(
            check_pkg_assignment(Some(operator_dir), "com.acme.tool", Some(&member_ctx)).is_ok()
        );
        // Unassigned third-party pkg is denied
        assert!(
            check_pkg_assignment(Some(operator_dir), "com.acme.other", Some(&member_ctx)).is_err()
        );
    }
}
