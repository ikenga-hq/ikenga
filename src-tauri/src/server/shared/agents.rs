//! `detect_agents` — PATH scan + version + auth probe for KNOWN_AGENTS.
//!
//! Subprocess spawns are wrapped in `tokio::time::timeout` so a hanging CLI
//! can't stall the wizard. All probes execute in parallel via `join_all`.

use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use tokio::time::timeout;

use super::known::{
    family_matches, AgentCapabilities, AgentDef, AuthCheck, ExecutableSpec, KNOWN_AGENTS,
};
// Only `lookup_wsl_executable` reads the family tag directly.
#[cfg(windows)]
use super::known::TargetFamily;
use crate::executor::{PipedOpts, SpawnSpec, StdioMode};

/// tokio `Command::output()`'s stdio (stdin left unset, so inherited; stdout
/// + stderr captured), named explicitly for the executor seam (WP-18b), plus
/// the `kill_on_drop` every probe asked for so a timed-out probe is reaped
/// with its dropped future. `no_console_window` is a no-op off Windows, which
/// is what the old Windows-only call amounted to.
const PROBE_OUTPUT_OPTS: PipedOpts = PipedOpts {
    stdin: StdioMode::Inherit,
    stdout: StdioMode::Piped,
    stderr: StdioMode::Piped,
    kill_on_drop: true,
    no_console_window: true,
    detached: false,
    new_process_group: false,
};

/// `tokio::process::Command::output()` through the executor: the spawn
/// happens when this is called (as `output()`'s does), the wait when the
/// returned future is polled — so wrapping it in `timeout` behaves exactly
/// as wrapping `cmd.output()` did.
fn probe_output(
    spec: SpawnSpec,
) -> impl std::future::Future<Output = std::io::Result<std::process::Output>> {
    let child = crate::executor::current().spawn_piped(spec, PROBE_OUTPUT_OPTS);
    async { child?.wait_with_output().await }
}

// Windows cold start: a freshly-installed CLI's first exec can take
// 500ms-1.7s+ while Defender scans the new binary before letting it run.
// 2s was tight enough to occasionally misreport an installed CLI as absent.
const DEFAULT_VERSION_TIMEOUT: Duration = Duration::from_millis(5000);

#[derive(Debug, Serialize)]
pub struct DetectedAgent {
    pub id: String,
    pub display: String,
    pub executable_path: String,
    pub version: Option<String>,
    pub authed: Option<bool>,
    pub auth_hint: Option<String>,
    pub capabilities: AgentCapabilities,
}

pub async fn detect_all() -> Vec<DetectedAgent> {
    let os = std::env::consts::OS;
    let mut futs = Vec::new();
    for def in KNOWN_AGENTS {
        futs.push(detect_one(def, os));
    }
    let results = futures_join_all(futs).await;
    results.into_iter().flatten().collect()
}

/// Detect a single known agent by id. Returns `None` when the id isn't in
/// `KNOWN_AGENTS` or the executable couldn't be resolved on the current OS.
/// Surfaced as the per-engine variant so the onboarding UI can fan out one
/// call per engine and reveal results as they land instead of blocking on
/// the slowest probe.
pub async fn detect_by_id(agent_id: &str) -> Option<DetectedAgent> {
    detect_by_id_in(agent_id, crate::runtime::augmented_path()).await
}

/// [`detect_by_id`] resolving the executable against `search_path` instead
/// of the process-wide augmented `$PATH`. The augmented path is a `OnceLock`
/// built from `$PATH` the first time anything asks, so mutating `$PATH` later
/// (as a test might) never reaches it — callers that need a specific search
/// path pass it here rather than touching the process environment.
pub(crate) async fn detect_by_id_in(
    agent_id: &str,
    search_path: &std::ffi::OsStr,
) -> Option<DetectedAgent> {
    let os = std::env::consts::OS;
    let def = KNOWN_AGENTS.iter().find(|d| {
        d.id == agent_id
            || (d.id == "gemini-cli" && agent_id == "gemini")
            || (d.id == "antigravity-cli" && agent_id == "antigravity")
            || (d.id == "cursor-agent" && agent_id == "cursor")
            || (d.id == "qwen-code" && agent_id == "qwen")
            || (d.id == "opencode" && agent_id == "opencode-ai")
            || (d.id == "pi" && (agent_id == "pi-coding-agent" || agent_id == "pi-agent"))
    })?;
    let mut detected = detect_one_in(def, os, search_path).await?;
    // If the caller queried by an alias like "gemini", keep the queried id so
    // the frontend map keys line up.
    detected.id = agent_id.to_string();
    Some(detected)
}

/// Inlined tiny join_all so we don't drag in the full `futures` crate.
async fn futures_join_all<I, F>(iter: I) -> Vec<F::Output>
where
    I: IntoIterator<Item = F>,
    F: std::future::Future,
{
    let mut out = Vec::new();
    for fut in iter {
        out.push(fut.await);
    }
    out
}

async fn detect_one(def: &AgentDef, os: &str) -> Option<DetectedAgent> {
    detect_one_in(def, os, crate::runtime::augmented_path()).await
}

async fn detect_one_in(
    def: &AgentDef,
    os: &str,
    search_path: &std::ffi::OsStr,
) -> Option<DetectedAgent> {
    let exec_path = resolve_executable_in(def, os, search_path)?;
    let is_wsl = exec_path.to_string_lossy().starts_with("wsl:");
    let display_path = if is_wsl {
        let raw = exec_path.to_string_lossy();
        let parts: Vec<&str> = raw.split(':').collect();
        if parts.len() >= 3 {
            format!("{} (WSL)", parts[2])
        } else {
            format!("{raw} (WSL)")
        }
    } else {
        exec_path.display().to_string()
    };
    let version = if let Some(arg) = def.version_arg {
        probe_version(&exec_path, arg, def.version_regex).await
    } else {
        None
    };
    let (authed, auth_hint) = match def.auth_check {
        Some(ref check) => probe_auth_with_hint(&exec_path, check).await,
        None => (None, None),
    };
    Some(DetectedAgent {
        id: def.id.to_string(),
        display: def.display.to_string(),
        executable_path: display_path,
        version,
        authed,
        auth_hint,
        capabilities: def.capabilities,
    })
}

#[cfg(test)]
fn resolve_executable(def: &AgentDef, os: &str) -> Option<PathBuf> {
    resolve_executable_in(def, os, crate::runtime::augmented_path())
}

fn resolve_executable_in(
    def: &AgentDef,
    os: &str,
    search_path: &std::ffi::OsStr,
) -> Option<PathBuf> {
    for spec in def.executables {
        if !family_matches(spec.target_family, os) {
            continue;
        }
        if let Some(found) = lookup_spec_in(spec, search_path) {
            return Some(found);
        }
    }
    #[cfg(windows)]
    {
        if let Some(wsl_path) = lookup_wsl_executable(def) {
            return Some(wsl_path);
        }
    }
    None
}

#[cfg(windows)]
fn lookup_wsl_executable(def: &AgentDef) -> Option<PathBuf> {
    let has_wsl = which::which("wsl.exe").is_ok()
        || std::path::Path::new(r"C:\Windows\System32\wsl.exe").exists();
    if !has_wsl {
        return None;
    }

    let mut names = Vec::new();
    for spec in def.executables {
        if matches!(spec.target_family, TargetFamily::Unix | TargetFamily::Any) {
            names.extend(spec.names.iter().copied());
        }
    }
    if names.is_empty() {
        if let Some(spec) = def.executables.first() {
            names.extend(spec.names.iter().copied());
        }
    }

    for name in names {
        let clean_name = name
            .strip_suffix(".cmd")
            .or_else(|| name.strip_suffix(".exe"))
            .or_else(|| name.strip_suffix(".bat"))
            .unwrap_or(name);

        // One candidate name failing to resolve says nothing about the next
        // one — keep probing instead of giving up on the agent.
        if let Some(path_str) = wsl_which(clean_name) {
            return Some(PathBuf::from(format!("wsl:{clean_name}:{path_str}")));
        }
    }
    None
}

/// Absolute path of `name` inside the default WSL distro's login shell, or
/// `None` when WSL is absent or the binary isn't on the distro's PATH. Shared
/// by agent detection and the headless Chi runtime (`commands/chi.rs`), so
/// both agree on whether a WSL-only CLI exists.
#[cfg(windows)]
pub(crate) fn wsl_which(name: &str) -> Option<String> {
    let has_wsl = which::which("wsl.exe").is_ok()
        || std::path::Path::new(r"C:\Windows\System32\wsl.exe").exists();
    if !has_wsl {
        return None;
    }
    let mut spec = SpawnSpec::new("wsl.exe");
    spec.args(["bash", "-l", "-c", &format!("which {name}")]);
    // std `Command::output()`'s stdio (stdin null, stdout + stderr captured)
    // on the executor's blocking path; this whole chain is sync.
    let output = crate::executor::current()
        .spawn_output_blocking(
            spec,
            PipedOpts {
                stdin: StdioMode::Null,
                ..PROBE_OUTPUT_OPTS
            },
        )
        .map_err(|e| tracing::warn!(target: "ikenga::agents", "wsl.exe spawn failed probing `{name}`: {e}"))
        .ok()?;
    if !output.status.success() {
        // A miss and a WSL that couldn't start both land here; only the
        // latter is worth a warning.
        if let Some(class) =
            super::failure_class::classify_output(&output.stdout, &output.stderr)
        {
            tracing::warn!(target: "ikenga::agents", "probing `{name}` in WSL: {}", class.describe());
        }
        return None;
    }
    which_output_path(&String::from_utf8_lossy(&output.stdout))
}

/// The path `which` printed, from a login shell's stdout. Profile scripts
/// (nvm, motd, conda) can print before it, so this is the last line that is
/// an absolute path, not the first line.
#[cfg_attr(not(windows), allow(dead_code))]
fn which_output_path(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .last()
        .map(str::to_string)
}

/// Resolve `spec` against `search_path` (production passes the augmented
/// PATH, ADR-013 §Addendum Decision 2, so a GUI-launched app — which inherits
/// a thin $PATH missing the nvm/npm/homebrew shims — still finds CLIs
/// installed there), then its `extra_dirs`. The process `$PATH` itself is
/// never consulted here.
fn lookup_spec_in(spec: &ExecutableSpec, search_path: &std::ffi::OsStr) -> Option<PathBuf> {
    for name in spec.names {
        // `cwd` is irrelevant here since `name` is always a bare binary name,
        // not a relative path.
        if let Ok(found) = which::which_in(name, Some(search_path), ".") {
            return Some(found);
        }
    }
    // Fallback: scan extra_dirs in order. Tilde-expand against the user's
    // home dir (HOME on Unix, USERPROFILE on Windows).
    for dir in spec.extra_dirs {
        let expanded = expand_tilde(dir);
        for name in spec.names {
            let candidate = expanded.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    // Platform-specific install hints that don't fit the static table.
    // npm-global on Windows lives in %APPDATA%\npm; Claude / Gemini / Codex
    // CLIs land here when installed via `npm install -g`, and that dir is
    // routinely missing from a GUI-launched process's PATH.
    #[cfg(windows)]
    {
        for dir in windows_npm_global_dirs() {
            for name in spec.names {
                let candidate = dir.join(name);
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn windows_npm_global_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(userprofile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        dirs.push(userprofile.join("AppData").join("Roaming").join("npm"));
        dirs.push(userprofile.join("AppData").join("Local").join("pnpm"));
        dirs.push(userprofile.join(".cargo").join("bin"));
        dirs.push(userprofile.join(".bun").join("bin"));
        dirs.push(userprofile.join("scoop").join("shims"));
        dirs.push(userprofile.join("AppData").join("Local").join("Microsoft").join("WinGet").join("Links"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(&local).join("npm"));
        dirs.push(PathBuf::from(&local).join("pnpm"));
        dirs.push(PathBuf::from(&local).join("Programs").join("npm"));
        dirs.push(PathBuf::from(&local).join("Microsoft").join("WinGet").join("Links"));
    }
    if let Some(home) = crate::platform::home_dir() {
        dirs.push(home.join("AppData").join("Roaming").join("npm"));
        dirs.push(home.join(".bun").join("bin"));
    }
    dirs
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = crate::platform::home_dir() {
            return home.join(rest);
        }
    } else if p == "~" {
        if let Some(home) = crate::platform::home_dir() {
            return home;
        }
    }
    PathBuf::from(p)
}

fn is_executable(p: &std::path::Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = p.metadata() {
            return meta.permissions().mode() & 0o111 != 0;
        }
        false
    }
    #[cfg(windows)]
    {
        // On Windows we don't have a portable exec bit; rely on extension.
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        matches!(ext.as_deref(), Some("exe" | "cmd" | "bat"))
    }
}

/// The spec for running an agent CLI. Console-flash suppression moved to the
/// opts (`no_console_window: true` everywhere; a no-op off Windows).
fn create_agent_command(exec: &std::path::Path) -> SpawnSpec {
    #[cfg(windows)]
    {
        let is_batch = exec
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            .unwrap_or(false);
        if is_batch {
            let mut cmd = SpawnSpec::new("cmd.exe");
            cmd.arg("/c").arg(exec);
            cmd
        } else {
            SpawnSpec::new(exec)
        }
    }
    #[cfg(not(windows))]
    {
        SpawnSpec::new(exec)
    }
}

async fn probe_version(exec: &std::path::Path, arg: &str, re: Option<&str>) -> Option<String> {
    #[cfg(windows)]
    let (output_res, regex) = {
        let exec_str = exec.to_string_lossy();
        if let Some(rest) = exec_str.strip_prefix("wsl:") {
            let bin_name = rest.split(':').next().unwrap_or(rest);
            let mut cmd = SpawnSpec::new("wsl.exe");
            cmd.args(["bash", "-l", "-c", &format!("{bin_name} {arg}")]);
            (timeout(DEFAULT_VERSION_TIMEOUT, probe_output(cmd)).await, re.unwrap_or(super::known::DEFAULT_VERSION_REGEX))
        } else {
            let mut cmd = create_agent_command(exec);
            cmd.arg(arg);
            cmd.env("PATH", crate::runtime::augmented_path());
            (timeout(DEFAULT_VERSION_TIMEOUT, probe_output(cmd)).await, re.unwrap_or(super::known::DEFAULT_VERSION_REGEX))
        }
    };
    #[cfg(not(windows))]
    let (output_res, regex) = {
        let mut cmd = create_agent_command(exec);
        cmd.arg(arg);
        cmd.env("PATH", crate::runtime::augmented_path());
        (timeout(DEFAULT_VERSION_TIMEOUT, probe_output(cmd)).await, re.unwrap_or(super::known::DEFAULT_VERSION_REGEX))
    };
    let output = output_res.ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).into_owned();
    }
    let parsed = Regex::new(regex).ok()?;
    let caps = parsed.captures(&text)?;
    caps.get(1).map(|m| m.as_str().to_string())
}

/// Returns `(Some(true), None)` if authed; `(Some(false), Some(hint))` if
/// not authed; `(None, None)` if the probe is inconclusive (e.g. an Exec
/// probe spawns but the binary doesn't exist at that path).
async fn probe_auth_with_hint(
    exec: &std::path::Path,
    check: &AuthCheck,
) -> (Option<bool>, Option<String>) {
    match check {
        AuthCheck::Exec {
            cmd,
            args,
            timeout_ms,
        } => probe_auth_exec(exec, cmd, args, *timeout_ms).await,
        AuthCheck::EnvVar { name } => {
            if env_truthy(name) {
                (Some(true), None)
            } else {
                (Some(false), Some(format!("{name} not set")))
            }
        }
        AuthCheck::FilePresent { paths } => probe_auth_files(paths),
        AuthCheck::Any { checks } => {
            // First successful inner check short-circuits. Without one, the
            // verdict is "signed out" only if every inner check concluded so;
            // an inner check that couldn't run (timeout, spawn failure, WSL or
            // network down) leaves the whole answer unknown.
            let mut hints: Vec<String> = Vec::new();
            let mut inconclusive = false;
            for inner in *checks {
                let (val, hint) = Box::pin(probe_auth_with_hint(exec, inner)).await;
                if val == Some(true) {
                    return (Some(true), None);
                }
                inconclusive |= val.is_none();
                if let Some(h) = hint {
                    hints.push(h);
                }
            }
            let prefix = if inconclusive { "inconclusive" } else { "none of" };
            let hint = if hints.is_empty() {
                None
            } else {
                Some(format!("{prefix}: {}", hints.join(" / ")))
            };
            (if inconclusive { None } else { Some(false) }, hint)
        }
        AuthCheck::AcpHandshake { args, timeout_ms } => {
            probe_auth_acp_handshake(exec, args, *timeout_ms).await
        }
        AuthCheck::FirstConclusive { checks } => {
            // Return the first *conclusive* nested result; fall through only
            // on inconclusive (`None`) so an earlier check (e.g. the ACP
            // handshake) stays authoritative over later fallbacks.
            let mut hints: Vec<String> = Vec::new();
            for inner in *checks {
                let (val, hint) = Box::pin(probe_auth_with_hint(exec, inner)).await;
                if val.is_some() {
                    return (val, hint);
                }
                if let Some(h) = hint {
                    hints.push(h);
                }
            }
            let hint = if hints.is_empty() {
                None
            } else {
                Some(format!("inconclusive: {}", hints.join(" / ")))
            };
            (None, hint)
        }
    }
}

/// Spawn an ACP CLI and run a minimal `initialize` → `session/new` handshake
/// to read auth state from the protocol (ADR-013 §Addendum Decision 1). This
/// is a standalone, throwaway probe — deliberately NOT the runtime transport
/// in `engines/gemini_acp` (that's bound to a thread id, AppHandle, and event
/// channels). Returns `Some(true)` when `session/new` yields a result,
/// `Some(false)` on a `-32000` (`AuthRequired`) error, and `None` (with a
/// hint) on any spawn/IO/parse/timeout failure so the caller can fall back.
async fn probe_auth_acp_handshake(
    exec: &std::path::Path,
    args: &[&str],
    timeout_ms: u64,
) -> (Option<bool>, Option<String>) {
    match timeout(Duration::from_millis(timeout_ms), acp_handshake(exec, args)).await {
        Ok(Ok(true)) => (Some(true), None),
        Ok(Ok(false)) => (
            Some(false),
            Some("not authenticated (ACP session/new → auth_required)".to_string()),
        ),
        Ok(Err(e)) => (None, Some(format!("ACP handshake probe failed: {e}"))),
        Err(_) => (
            None,
            Some(format!(
                "ACP handshake probe timed out after {timeout_ms}ms"
            )),
        ),
    }
}

/// The handshake itself: write `initialize`, then `session/new`, and inspect
/// the `id:2` response. `Ok(true)` = authed, `Ok(false)` = `-32000`, `Err` =
/// transport/parse problem (inconclusive).
/// Does a JSON-RPC `-32000` message actually describe an auth failure?
///
/// `-32000` is ACP's generic server-error bucket, not a dedicated
/// `AuthRequired` code, so the message is the only thing that distinguishes
/// "you are logged out" from "this product was discontinued" or "you are out
/// of quota". Returning `false` here makes the probe inconclusive rather than
/// negative, which lets the cred-file / env-var fallbacks answer instead.
fn is_auth_failure_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("auth")
        || lower.contains("login")
        || lower.contains("log in")
        || lower.contains("credential")
        || lower.contains("sign in")
        || lower.contains("unauthenticated")
        || lower.contains("not authorized")
        || lower.contains("unauthorized")
}

async fn acp_handshake(exec: &std::path::Path, args: &[&str]) -> Result<bool, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut child = create_agent_command(exec);
    child
        .args(args)
        .env("PATH", crate::runtime::augmented_path());
    let opts = PipedOpts {
        stdin: StdioMode::Piped,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Null,
        kill_on_drop: true,
        no_console_window: true,
        detached: false,
        new_process_group: false,
    };

    let mut spawned = crate::executor::current()
        .spawn_piped(child, opts)
        .map_err(|e| format!("spawn `{}` failed: {e}", exec.display()))?;

    let stdin = spawned
        .stdin
        .as_mut()
        .ok_or_else(|| "child stdin not captured".to_string())?;
    let stdout = spawned
        .stdout
        .take()
        .ok_or_else(|| "child stdout not captured".to_string())?;
    let mut lines = BufReader::new(stdout).lines();

    // Protocol handshake — client initialization envelope.
    //
    // `protocolVersion` is a NUMBER in the ACP schema. Sending the string
    // "0.1.0" makes gemini reject `initialize` outright, and the rejection is
    // silent from here: the probe reads its verdict off the id:2 response,
    // which still arrives, so a broken handshake looks like a clean negative.
    // Verified against gemini 0.55.1 —
    //   {"protocolVersion":1}       -> {"id":1,"result":{"protocolVersion":1,…}}
    //   {"protocolVersion":"0.1.0"} -> {"id":1,"error":{"code":-32603,…
    //        "expected":"number","path":["protocolVersion"],
    //        "message":"Invalid input: expected number, received string"}}
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\
              \"params\":{\"protocolVersion\":1,\"clientCapabilities\":{}}}\n",
        )
        .await
        .map_err(|e| format!("write initialize: {e}"))?;
    stdin.flush().await.map_err(|e| format!("flush: {e}"))?;

    // Immediately queue session/new — ACP allows pipelining.
    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"session/new\",\
              \"params\":{\"cwd\":\"/\",\"mcpServers\":[]}}\n",
        )
        .await
        .map_err(|e| format!("write session/new: {e}"))?;
    stdin.flush().await.map_err(|e| format!("flush: {e}"))?;

    // Read line-delimited JSON-RPC until we see the response to id:2. Gemini
    // interleaves the id:1 result, notifications, and the id:2 response; we
    // skip anything that isn't our request id.
    while let Some(line) = lines.next_line().await.map_err(|e| format!("read: {e}"))? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if msg.get("id").and_then(|v| v.as_i64()) != Some(2) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            let code = err.get("code").and_then(|v| v.as_i64());
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            // -32000 is the ACP server-error bucket. It USED to mean
            // `AuthRequired` and nothing else, so the code alone was the
            // verdict. Google now returns it for conditions that have nothing
            // to do with auth, and treating those as "logged out" is worse
            // than useless: `known.rs` wraps this in `FirstConclusive` so a
            // negative here outranks the cred-file fallback, meaning a
            // correctly-authenticated user is reported as signed out and no
            // amount of re-authenticating clears it.
            //
            // Observed against gemini 0.55.1 (2026-08-24), both -32000:
            //   "This client is no longer supported for Gemini Code Assist
            //    for individuals. To continue using Gemini, please migrate to
            //    the Antigravity suite of products: https://antigravity.google"
            //   "Resource has been exhausted (e.g. check quota)."
            //
            // So the code narrows the field and the message decides. Anything
            // we can't positively read as an auth failure is inconclusive,
            // which lets the cred-file / env-var checks answer instead.
            if code == Some(-32000) {
                return if is_auth_failure_message(message) {
                    Ok(false)
                } else {
                    // Not an auth verdict. Surfacing the message keeps a
                    // product deprecation from masquerading as a login
                    // problem in the wizard.
                    Err(format!("session/new -32000 (not an auth failure): {message}"))
                };
            }
            // Any other code is a real problem, not an auth verdict.
            return Err(format!("session/new error: {err}"));
        }
        if msg.get("result").is_some() {
            return Ok(true);
        }
        return Err("session/new response had neither result nor error".to_string());
    }
    Err("child closed stdout before responding to session/new".to_string())
}

/// The verdict of an `AuthCheck::Exec` probe that ran to completion. Exit 0
/// is signed in. A non-zero exit is signed out *unless* the output names an
/// infrastructure failure (WSL down, no network): `claude doctor` and friends
/// fail for those too, and reading that as "signed out" both misleads and
/// hides the engine from the Chi target picker.
fn exec_verdict(out: &std::process::Output, what: &str) -> (Option<bool>, Option<String>) {
    if out.status.success() {
        return (Some(true), None);
    }
    if let Some(class) = super::failure_class::classify_output(&out.stdout, &out.stderr) {
        return (None, Some(format!("couldn't check `{what}`: {}", class.describe())));
    }
    let code = out
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into());
    (Some(false), Some(format!("`{what}` exited {code}")))
}

/// Run an `AuthCheck::Exec` probe. Anything that stops the probe from running
/// — the binary isn't where we looked, the spawn fails, it times out — is
/// inconclusive (`None`), never "signed out".
async fn probe_auth_exec(
    exec_fallback: &std::path::Path,
    cmd: &str,
    args: &[&str],
    timeout_ms: u64,
) -> (Option<bool>, Option<String>) {
    let what = format!("{cmd} {}", args.join(" "));
    let command = {
        #[cfg(windows)]
        {
            let exec_str = exec_fallback.to_string_lossy();
            if let Some(rest) = exec_str.strip_prefix("wsl:") {
                let bin_name = rest.split(':').next().unwrap_or(cmd);
                let mut command = SpawnSpec::new("wsl.exe");
                command.args(["bash", "-l", "-c", &format!("{bin_name} {}", args.join(" "))]);
                command
            } else {
                match host_auth_command(exec_fallback, cmd, args) {
                    Ok(c) => c,
                    Err(hint) => return (None, Some(hint)),
                }
            }
        }
        #[cfg(not(windows))]
        {
            match host_auth_command(exec_fallback, cmd, args) {
                Ok(c) => c,
                Err(hint) => return (None, Some(hint)),
            }
        }
    };
    match timeout(Duration::from_millis(timeout_ms), probe_output(command)).await {
        Ok(Ok(out)) => exec_verdict(&out, &what),
        Ok(Err(e)) => (None, Some(format!("couldn't run `{what}`: {e}"))),
        Err(_) => (None, Some(format!("`{what}` timed out after {timeout_ms}ms"))),
    }
}

/// The spec for a host-side (non-WSL) auth probe, or the hint when `cmd`
/// can't be found.
fn host_auth_command(
    exec_fallback: &std::path::Path,
    cmd: &str,
    args: &[&str],
) -> Result<SpawnSpec, String> {
    let is_cmd = |n: &str| {
        n == cmd
            || (cfg!(windows)
                && [".cmd", ".exe", ".bat"]
                    .iter()
                    .any(|ext| n == format!("{cmd}{ext}")))
    };
    let target: PathBuf = if exec_fallback
        .file_name()
        .and_then(|n| n.to_str())
        .map(is_cmd)
        .unwrap_or(false)
    {
        exec_fallback.to_path_buf()
    } else {
        which::which_in(cmd, Some(crate::runtime::augmented_path()), ".")
            .map_err(|_| format!("auth probe binary `{cmd}` not on PATH"))?
    };
    let mut command = create_agent_command(&target);
    command.args(args);
    command.env("PATH", crate::runtime::augmented_path());
    Ok(command)
}

fn probe_auth_files(paths: &[&str]) -> (Option<bool>, Option<String>) {
    let mut tried: Vec<String> = Vec::new();
    for p in paths {
        let expanded = expand_tilde(p);
        if expanded.is_file() {
            return (Some(true), None);
        }
        #[cfg(windows)]
        {
            let clean_p = p.strip_prefix("~/").unwrap_or(p);
            for prefix in [r"\\wsl.localhost", r"\\wsl$"] {
                let wsl_base = PathBuf::from(prefix);
                if let Ok(distros) = std::fs::read_dir(&wsl_base) {
                    for d in distros.flatten() {
                        let home_dir = d.path().join("home");
                        if let Ok(users) = std::fs::read_dir(&home_dir) {
                            for u in users.flatten() {
                                if u.path().join(clean_p).is_file() {
                                    return (Some(true), None);
                                }
                            }
                        }
                    }
                }
            }
        }
        tried.push(p.to_string());
    }
    (Some(false), Some(format!("missing: {}", tried.join(", "))))
}

fn env_truthy(name: &str) -> bool {
    matches!(std::env::var(name), Ok(v) if !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::known::TargetFamily;

    /// Real `-32000` payloads captured from gemini 0.55.1 on 2026-08-24.
    /// Neither is an auth failure, and treating them as one reports a
    /// logged-in user as signed out — `known.rs` uses `FirstConclusive`, so a
    /// false negative here outranks the cred-file fallback that would
    /// otherwise correct it.
    #[test]
    fn non_auth_minus_32000_messages_are_not_auth_failures() {
        assert!(!is_auth_failure_message(
            "This client is no longer supported for Gemini Code Assist for individuals. \
             To continue using Gemini, please migrate to the Antigravity suite of \
             products: https://antigravity.google"
        ));
        assert!(!is_auth_failure_message(
            "Resource has been exhausted (e.g. check quota)."
        ));
    }

    #[test]
    fn genuine_auth_messages_are_detected() {
        for m in [
            "Authentication required",
            "Please log in with `gemini auth login`",
            "unauthenticated",
            "No credentials found",
            "User is not authorized",
            "Please sign in to continue",
        ] {
            assert!(is_auth_failure_message(m), "should detect auth failure: {m}");
        }
    }

    #[test]
    fn auth_detection_is_case_insensitive() {
        assert!(is_auth_failure_message("AUTHENTICATION REQUIRED"));
        assert!(is_auth_failure_message("Unauthorized"));
    }

    #[test]
    fn expand_tilde_handles_home() {
        // Set the platform-appropriate home env var to a known value for the
        // duration of this test. Windows reads USERPROFILE; Unix reads HOME.
        #[cfg(windows)]
        let var = "USERPROFILE";
        #[cfg(not(windows))]
        let var = "HOME";
        let prev = std::env::var_os(var);
        std::env::set_var(var, "/tmp/fakehome");
        assert_eq!(expand_tilde("~/foo"), PathBuf::from("/tmp/fakehome/foo"));
        assert_eq!(expand_tilde("~"), PathBuf::from("/tmp/fakehome"));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        if let Some(p) = prev {
            std::env::set_var(var, p);
        } else {
            std::env::remove_var(var);
        }
    }

    #[test]
    fn resolve_executable_respects_target_family() {
        let def = AgentDef {
            id: "fake",
            display: "Fake",
            executables: &[ExecutableSpec {
                target_family: TargetFamily::Windows,
                names: &["definitely-not-on-path-fake-cli.exe"],
                extra_dirs: &[],
            }],
            version_arg: None,
            version_regex: None,
            auth_check: None,
            capabilities: AgentCapabilities {
                streaming: false,
                tool_use: false,
                thinking: false,
                artifacts: false,
                mcp: false,
                session_resume: false,
            },
        };
        // On linux, the Windows-only spec should be skipped.
        assert!(resolve_executable(&def, "linux").is_none());
    }

    /// Regression: detection used to be testable only by mutating `$PATH`,
    /// which never reaches the `OnceLock`-cached augmented path and races
    /// every other detection test. The injected search path must be the only
    /// PATH-style source consulted: `sh` is on every host's real `$PATH`, so
    /// finding nothing in an empty dir proves the process PATH was ignored,
    /// and finding the stub proves the injected dir was searched.
    #[cfg(unix)]
    #[test]
    fn lookup_spec_in_searches_only_the_given_path() {
        use std::os::unix::fs::PermissionsExt;
        let spec = ExecutableSpec {
            target_family: TargetFamily::Unix,
            names: &["sh"],
            extra_dirs: &[],
        };

        let empty = tempfile::tempdir().unwrap();
        assert_eq!(lookup_spec_in(&spec, empty.path().as_os_str()), None);

        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("sh");
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(lookup_spec_in(&spec, dir.path().as_os_str()), Some(stub));
    }

    #[tokio::test]
    async fn detect_returns_only_present_agents() {
        // Doesn't assert which agents — just that the call shape works
        // and every returned entry has a non-empty executable_path.
        let detected = detect_all().await;
        for d in detected {
            assert!(!d.executable_path.is_empty(), "{}", d.id);
            assert!(!d.id.is_empty());
        }
    }

    #[tokio::test]
    async fn probe_version_against_sh_returns_string() {
        // `sh --version` reliably prints a semver on every dev box we
        // run CI on. If `sh` isn't on PATH this test is skipped.
        let Some(sh_path) = which::which("sh").ok() else {
            return;
        };
        let v = probe_version(&sh_path, "--version", None).await;
        // We don't assert exact value because `sh` varies (bash, dash, zsh
        // symlink). It just needs to extract *some* semver.
        if let Some(v) = v {
            assert!(v.contains('.'), "got version `{v}`");
        }
    }

    #[test]
    fn env_truthy_recognises_set_var() {
        std::env::set_var("IKENGA_DETECT_TEST_VAR", "yes");
        assert!(env_truthy("IKENGA_DETECT_TEST_VAR"));
        std::env::set_var("IKENGA_DETECT_TEST_VAR", "");
        assert!(!env_truthy("IKENGA_DETECT_TEST_VAR"));
        std::env::remove_var("IKENGA_DETECT_TEST_VAR");
        assert!(!env_truthy("IKENGA_DETECT_TEST_VAR"));
    }

    #[tokio::test]
    async fn first_conclusive_keeps_the_first_conclusive_verdict() {
        // EnvVar is always conclusive. FirstConclusive must return the FIRST
        // conclusive verdict — unlike `Any`, a later positive must NOT flip an
        // earlier negative. This is what keeps the ACP handshake authoritative
        // over the cred-file/env fallbacks (ADR-013 §Addendum Decision 1).
        std::env::set_var("IKENGA_FC_PRESENT", "1");
        std::env::remove_var("IKENGA_FC_ABSENT");
        let dummy = std::path::Path::new("/nonexistent-exec");

        // First conclusive is positive → true.
        let check = AuthCheck::FirstConclusive {
            checks: &[
                AuthCheck::EnvVar {
                    name: "IKENGA_FC_PRESENT",
                },
                AuthCheck::EnvVar {
                    name: "IKENGA_FC_ABSENT",
                },
            ],
        };
        assert_eq!(probe_auth_with_hint(dummy, &check).await.0, Some(true));

        // First conclusive is negative → false, even though a LATER check
        // would be positive. (`Any` would return true here — that's the bug
        // FirstConclusive exists to avoid.)
        let check = AuthCheck::FirstConclusive {
            checks: &[
                AuthCheck::EnvVar {
                    name: "IKENGA_FC_ABSENT",
                },
                AuthCheck::EnvVar {
                    name: "IKENGA_FC_PRESENT",
                },
            ],
        };
        assert_eq!(probe_auth_with_hint(dummy, &check).await.0, Some(false));

        std::env::remove_var("IKENGA_FC_PRESENT");
    }

    /// An `Exec` probe whose binary can't be found never ran — the `Any`
    /// wrapping it must say "unknown", not "signed out" (which also drops the
    /// engine from the Chi target picker).
    #[tokio::test]
    async fn any_with_an_inconclusive_inner_check_is_unknown() {
        std::env::remove_var("IKENGA_ANY_ABSENT");
        let dummy = std::path::Path::new("/nonexistent-exec");
        let check = AuthCheck::Any {
            checks: &[
                AuthCheck::EnvVar {
                    name: "IKENGA_ANY_ABSENT",
                },
                AuthCheck::Exec {
                    cmd: "ikenga-definitely-not-a-real-cli",
                    args: &["doctor"],
                    timeout_ms: 1000,
                },
            ],
        };
        let (val, hint) = probe_auth_with_hint(dummy, &check).await;
        assert_eq!(val, None);
        assert!(hint.unwrap().starts_with("inconclusive:"));

        // All inner checks conclusive and negative: still a firm "no".
        let check = AuthCheck::Any {
            checks: &[AuthCheck::EnvVar {
                name: "IKENGA_ANY_ABSENT",
            }],
        };
        assert_eq!(probe_auth_with_hint(dummy, &check).await.0, Some(false));
    }

    fn output(code: i32, stderr: &str) -> std::process::Output {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;
        #[cfg(unix)]
        let status = std::process::ExitStatus::from_raw(code << 8);
        #[cfg(windows)]
        let status = std::process::ExitStatus::from_raw(code as u32);
        std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn exec_verdict_separates_infrastructure_from_signed_out() {
        assert_eq!(exec_verdict(&output(0, ""), "claude doctor").0, Some(true));
        let (val, hint) = exec_verdict(
            &output(1, "OAuth error: getaddrinfo EAI_AGAIN platform.claude.com"),
            "claude doctor",
        );
        assert_eq!(val, None);
        assert!(hint.unwrap().contains("EAI_AGAIN"));
        assert_eq!(
            exec_verdict(&output(1, "Not logged in"), "claude doctor").0,
            Some(false)
        );
    }

    #[test]
    fn which_output_path_skips_profile_banners() {
        assert_eq!(
            which_output_path("Now using node v22.3.0 (npm v10.8.1)\n/home/u/.nvm/versions/node/v22.3.0/bin/claude\n"),
            Some("/home/u/.nvm/versions/node/v22.3.0/bin/claude".into())
        );
        assert_eq!(which_output_path("claude not found\n"), None);
        assert_eq!(which_output_path(""), None);
    }
}
