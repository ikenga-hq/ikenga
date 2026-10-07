//! `detect_agents` — PATH scan + version + auth probe for KNOWN_AGENTS.
//!
//! Subprocess spawns are wrapped in `tokio::time::timeout` so a hanging CLI
//! (or a wedged `wsl.exe`) can't stall the wizard. Agents are probed
//! concurrently via `join_all`. Probes that go through WSL use the configured
//! `engines.agentWslDistro` and a cold-start allowance (`super::wsl`).

use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use tokio::time::timeout;

use super::known::{
    family_matches, AgentCapabilities, AgentDef, AuthCheck, ExecutableSpec, KNOWN_AGENTS,
};
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
    probe_output_with(spec, PROBE_OUTPUT_OPTS)
}

/// [`probe_output`] with explicit stdio.
fn probe_output_with(
    spec: SpawnSpec,
    opts: PipedOpts,
) -> impl std::future::Future<Output = std::io::Result<std::process::Output>> {
    let child = crate::executor::current().spawn_piped(spec, opts);
    async { child?.wait_with_output().await }
}

/// The answer to "is `<name>` installed inside WSL?". Three outcomes, because
/// a `wsl.exe` that couldn't start (no distro, VM failure, a hang) says
/// nothing about whether the CLI is installed — reporting it as a miss tells
/// the user to install something they already have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WslLookup {
    /// The absolute path the distro's login shell resolves `name` to.
    Found(String),
    /// WSL answered, and `name` is not on the distro's login PATH (or this
    /// machine has no `wsl.exe` at all).
    NotFound,
    /// WSL couldn't be asked. Carries the reason.
    WslUnavailable(String),
}

/// Budget for `command -v` inside WSL: a warm distro answers in 0.5–2 s; a
/// cold one needs [`super::wsl::COLD_START`] on top.
#[cfg_attr(not(windows), allow(dead_code))]
const WSL_WHICH_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// Set when detection couldn't check for this agent at all — today only
    /// "WSL couldn't be asked" (D-10). Such an agent is neither installed nor
    /// missing: `version` / `authed` are `None` and it is not runnable.
    /// Additive: omitted from the wire when absent, so a consumer that
    /// predates it sees exactly the old shape for every checked agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<AgentUnavailable>,
}

/// Why an agent couldn't be checked. `kind` names the dependency that failed
/// (only `"wsl"` today) so the UI can word it; `reason` is the probe's own
/// detail (e.g. wsl.exe's `Wsl/…` error code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentUnavailable {
    pub kind: &'static str,
    pub reason: String,
}

impl AgentUnavailable {
    pub fn wsl(reason: impl Into<String>) -> Self {
        Self {
            kind: "wsl",
            reason: reason.into(),
        }
    }
}

/// The entry reported for an agent whose WSL lookup couldn't run: named, not
/// probed, flagged [`AgentUnavailable`] so the UI says "WSL unavailable"
/// rather than "not installed".
#[cfg_attr(not(windows), allow(dead_code))]
fn wsl_unavailable_agent(def: &AgentDef, name: &str, reason: String) -> DetectedAgent {
    DetectedAgent {
        id: def.id.to_string(),
        display: def.display.to_string(),
        executable_path: format!("{name} (WSL)"),
        version: None,
        authed: None,
        auth_hint: None,
        capabilities: def.capabilities,
        unavailable: Some(AgentUnavailable::wsl(reason)),
    }
}

/// Maps the WSL fallback for an agent the host PATH didn't have onto
/// detection. `lookup` is `None` when this machine has no `wsl.exe`.
/// `Ok(path)` — found inside WSL, go on and probe it; `Err(None)` — absent;
/// `Err(Some(entry))` — WSL couldn't be asked, report the agent unavailable
/// (D-10), never absent. Pure and cfg-free so the mapping is tested on every
/// platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn wsl_fallback(
    def: &AgentDef,
    names: &[&'static str],
    lookup: Option<WslLookup>,
) -> Result<PathBuf, Option<DetectedAgent>> {
    match lookup {
        None | Some(WslLookup::NotFound) => Err(None),
        Some(WslLookup::Found(p)) => Ok(PathBuf::from(p)),
        Some(WslLookup::WslUnavailable(reason)) => {
            tracing::warn!(
                target: "ikenga::agents",
                "couldn't check WSL for {} — reporting it unavailable, not absent: {reason}",
                def.id
            );
            let name = names.first().copied().unwrap_or(def.id);
            Err(Some(wsl_unavailable_agent(def, name, reason)))
        }
    }
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
/// `KNOWN_AGENTS` or the executable is positively absent on the current OS.
/// When WSL couldn't be asked, returns the agent flagged
/// [`DetectedAgent::unavailable`] rather than `None` (D-10).
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

/// Await every probe concurrently: one slow agent (a cold WSL start, a CLI
/// Defender is scanning) must not serialise the rest behind it.
async fn futures_join_all<I, F>(iter: I) -> Vec<F::Output>
where
    I: IntoIterator<Item = F>,
    F: std::future::Future,
{
    futures_util::future::join_all(iter).await
}

async fn detect_one(def: &AgentDef, os: &str) -> Option<DetectedAgent> {
    detect_one_in(def, os, crate::runtime::augmented_path()).await
}

async fn detect_one_in(
    def: &AgentDef,
    os: &str,
    search_path: &std::ffi::OsStr,
) -> Option<DetectedAgent> {
    let exec_path = match resolve_executable_in(def, os, search_path) {
        Some(p) => p,
        #[cfg(windows)]
        None => {
            let names = wsl_candidate_names(def);
            let lookup = if super::wsl::wsl_exe_present() {
                let distro = super::wsl::configured_distro();
                Some(lookup_wsl_with(&names, |n| wsl_which(n, distro.as_deref())).await)
            } else {
                None
            };
            match wsl_fallback(def, &names, lookup) {
                Ok(p) => p,
                Err(entry) => return entry,
            }
        }
        #[cfg(not(windows))]
        None => return None,
    };
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
        unavailable: None,
    })
}

#[cfg(test)]
fn resolve_executable(def: &AgentDef, os: &str) -> Option<PathBuf> {
    resolve_executable_in(def, os, crate::runtime::augmented_path())
}

/// The agent's executable on the host (PATH, then its `extra_dirs`). The WSL
/// fallback is separate ([`lookup_wsl_with`]) because it spawns.
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
    None
}

/// The names to try for `def` inside WSL: its Unix/Any spellings (falling
/// back to the first spec's), with Windows shim suffixes stripped.
#[cfg_attr(not(windows), allow(dead_code))]
fn wsl_candidate_names(def: &AgentDef) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
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
    let mut out: Vec<&'static str> = Vec::new();
    for name in names {
        let clean = name
            .strip_suffix(".cmd")
            .or_else(|| name.strip_suffix(".exe"))
            .or_else(|| name.strip_suffix(".bat"))
            .unwrap_or(name);
        if !out.contains(&clean) {
            out.push(clean);
        }
    }
    out
}

/// Try each candidate name with `which`. `Found` carries the
/// `wsl:<name>:<path>` detection path. One name missing says nothing about
/// the next, so a miss keeps probing; WSL itself failing stops at once —
/// every further name would wait out the same failure — and is reported as
/// `WslUnavailable`, never as a miss. Not cached: the next detection asks
/// again.
#[cfg_attr(not(windows), allow(dead_code))]
async fn lookup_wsl_with<F, Fut>(names: &[&'static str], mut which: F) -> WslLookup
where
    F: FnMut(&'static str) -> Fut,
    Fut: std::future::Future<Output = WslLookup>,
{
    for name in names {
        match which(name).await {
            WslLookup::Found(path) => return WslLookup::Found(format!("wsl:{name}:{path}")),
            WslLookup::NotFound => {}
            unavailable @ WslLookup::WslUnavailable(_) => return unavailable,
        }
    }
    WslLookup::NotFound
}

/// Where `name` lives on `distro`'s login PATH (`None` = the default
/// distro). Shared by agent detection, the headless Chi runtime
/// (`chi_exec::HostResolver`) and seat install state, so all three agree on
/// whether a WSL-only CLI exists. Async with a cold-start-sized timeout: a
/// wedged `wsl.exe` used to hang detection and run start indefinitely.
#[cfg(windows)]
pub(crate) async fn wsl_which(name: &str, distro: Option<&str>) -> WslLookup {
    if !super::wsl::wsl_exe_present() {
        return WslLookup::NotFound;
    }
    let spec = wsl_bash(distro, &format!("command -v -- {}", sh_quote(name)));
    let budget = WSL_WHICH_TIMEOUT + super::wsl::COLD_START;
    let opts = PipedOpts {
        stdin: StdioMode::Null,
        ..PROBE_OUTPUT_OPTS
    };
    match timeout(budget, probe_output_with(spec, opts)).await {
        Err(_) => WslLookup::WslUnavailable(format!(
            "wsl.exe did not answer within {}s",
            budget.as_secs()
        )),
        Ok(Err(e)) => WslLookup::WslUnavailable(format!("couldn't start wsl.exe: {e}")),
        Ok(Ok(out)) => which_verdict(out.status.code(), &out.stdout, &out.stderr),
    }
}

/// `wsl.exe [-d <distro>] -e bash -l -c <script>` — `-e` so the script is
/// handed to bash as-is rather than re-parsed by the distro's login shell
/// (the terminal launches the same way, `src/terminal/claude-wrap.ts`).
#[cfg(windows)]
fn wsl_bash(distro: Option<&str>, script: &str) -> SpawnSpec {
    let mut spec = SpawnSpec::new("wsl.exe");
    spec.args(super::wsl::distro_args(distro));
    spec.args(["-e", "bash", "-l", "-c", script]);
    spec
}

/// Single-quote `s` for a POSIX shell.
#[cfg_attr(not(windows), allow(dead_code))]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Read a finished `wsl.exe … command -v <name>`. Only `command -v`'s own
/// "no such command" (exit 1, nothing WSL-shaped in the output) is a miss;
/// any other failure is WSL not answering.
#[cfg_attr(not(windows), allow(dead_code))]
fn which_verdict(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> WslLookup {
    use super::failure_class::{classify_output, FailureClass};
    if classify_output(stdout, stderr) == Some(FailureClass::WslUnavailable) {
        // wsl.exe's own error text — its `Wsl/…` code is the useful part —
        // on one line, capped.
        let detail = super::wsl::decode_wsl_output(stderr)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return WslLookup::WslUnavailable(if detail.is_empty() {
            FailureClass::WslUnavailable.describe()
        } else {
            detail.chars().take(240).collect()
        });
    }
    match code {
        Some(0) => match which_output_path(&String::from_utf8_lossy(stdout)) {
            Some(p) => WslLookup::Found(p),
            None => WslLookup::NotFound,
        },
        Some(1) => WslLookup::NotFound,
        Some(c) => WslLookup::WslUnavailable(format!("wsl.exe exited {c}")),
        None => WslLookup::WslUnavailable("wsl.exe was terminated".into()),
    }
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
            let distro = super::wsl::configured_distro();
            let cmd = wsl_bash(distro.as_deref(), &format!("{bin_name} {arg}"));
            let budget = super::wsl::probe_budget(exec, DEFAULT_VERSION_TIMEOUT);
            (timeout(budget, probe_output(cmd)).await, re.unwrap_or(super::known::DEFAULT_VERSION_REGEX))
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
        AuthCheck::FilePresent { paths } => probe_auth_files(exec, paths).await,
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
                let distro = super::wsl::configured_distro();
                wsl_bash(distro.as_deref(), &format!("{bin_name} {}", args.join(" ")))
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
    let budget = super::wsl::probe_budget(exec_fallback, Duration::from_millis(timeout_ms));
    match timeout(budget, probe_output(command)).await {
        Ok(Ok(out)) => exec_verdict(&out, &what),
        Ok(Err(e)) => (None, Some(format!("couldn't run `{what}`: {e}"))),
        Err(_) => (
            None,
            Some(format!("`{what}` timed out after {}ms", budget.as_millis())),
        ),
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

/// A `FilePresent` probe. Where the file has to be depends on where the CLI
/// runs: a host CLI reads the host home, a WSL CLI (`wsl:` path) reads its
/// distro's homes. A credential in the other place doesn't sign that CLI in,
/// so it isn't counted.
async fn probe_auth_files(exec: &std::path::Path, paths: &[&str]) -> (Option<bool>, Option<String>) {
    #[cfg(windows)]
    if exec.to_string_lossy().starts_with("wsl:") {
        return probe_auth_files_in_wsl(exec, paths).await;
    }
    let _ = exec;
    for p in paths {
        if expand_tilde(p).is_file() {
            return (Some(true), None);
        }
    }
    (Some(false), Some(format!("missing: {}", paths.join(", "))))
}

/// [`probe_auth_files`] for a WSL CLI: scan the distro shares off the async
/// runtime (`\\wsl.localhost` blocks while a cold distro starts), under a
/// cold-start budget. A share that couldn't be read, or a scan that ran out
/// of time, is "couldn't tell" — never "missing".
#[cfg(windows)]
async fn probe_auth_files_in_wsl(
    exec: &std::path::Path,
    paths: &[&str],
) -> (Option<bool>, Option<String>) {
    let distro = super::wsl::configured_distro();
    let rels: Vec<String> = paths
        .iter()
        .map(|p| p.strip_prefix("~/").unwrap_or(p).to_string())
        .collect();
    let budget = super::wsl::probe_budget(exec, Duration::from_secs(5));
    let scan = tokio::task::spawn_blocking(move || {
        // The CLI runs in the configured (else the default) distro as that
        // distro's default user; only that user's home signs it in. When
        // the registry doesn't say, fall back to the configured distro (or
        // every distro) and every home in it.
        let identity = super::wsl::launch_identity(distro.as_deref());
        let only = identity.as_ref().map(|(name, _)| name.clone()).or(distro);
        let uid = identity.and_then(|(_, uid)| uid);
        let roots = super::wsl::share_roots();
        scan_wsl_shares(&roots, &rels, only.as_deref(), uid)
    });
    match timeout(budget, scan).await {
        Ok(Ok(ShareScan::Found)) => (Some(true), None),
        Ok(Ok(ShareScan::Missing)) => (
            Some(false),
            Some(format!("missing in WSL: {}", paths.join(", "))),
        ),
        Ok(Ok(ShareScan::Unreadable(why))) => {
            (None, Some(format!("couldn't read the WSL share: {why}")))
        }
        Ok(Err(e)) => (None, Some(format!("WSL credential scan failed: {e}"))),
        Err(_) => (
            None,
            Some(format!(
                "WSL credential scan timed out after {}s",
                budget.as_secs()
            )),
        ),
    }
}

/// What a scan of the WSL shares for a credential file found.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum ShareScan {
    Found,
    Missing,
    /// The shares (or a distro's `/home`) couldn't be listed — the file may
    /// well be there.
    Unreadable(String),
}

/// Look for any of `rels` (home-relative) in the WSL shares `roots` (the
/// `\\wsl.localhost` and `\\wsl$` share roots, which list the same distros
/// — the first one that lists answers). `only` limits the scan to the distro
/// the CLI launches in; Docker Desktop's internal distros are never scanned.
/// With `uid` (that distro's default user) only the home `/etc/passwd` gives
/// it is read — a file in another user's home doesn't sign the CLI in.
/// Without it, or when the distro's `/etc/passwd` doesn't name that uid,
/// `/root` and every `/home/<user>` are read.
///
/// Permission-denied on a home is skipped, not an error: `/root` and other
/// users' homes are normally closed to the share's (default) user, and that
/// user's own home is the one its CLI reads. A share root that lists no
/// distro to scan is "couldn't tell", not "missing".
#[cfg_attr(not(windows), allow(dead_code))]
fn scan_wsl_shares(
    roots: &[PathBuf],
    rels: &[String],
    only: Option<&str>,
    uid: Option<u32>,
) -> ShareScan {
    use std::io::ErrorKind;

    // `Ok(true)` present, `Ok(false)` absent or closed to us, `Err` unknown.
    let file_at = |p: &std::path::Path| -> Result<bool, String> {
        match std::fs::metadata(p) {
            Ok(m) => Ok(m.is_file()),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::PermissionDenied) => {
                Ok(false)
            }
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    };

    let mut root_errors: Vec<String> = Vec::new();
    for root in roots {
        let distros = match std::fs::read_dir(root) {
            Ok(d) => d,
            Err(e) => {
                root_errors.push(format!("{}: {e}", root.display()));
                continue;
            }
        };
        let mut errors: Vec<String> = Vec::new();
        let mut scanned = 0usize;
        for entry in distros {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("{}: {e}", root.display()));
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if super::wsl::is_docker_desktop(&name) {
                continue;
            }
            if only.is_some_and(|d| !d.eq_ignore_ascii_case(&name)) {
                continue;
            }
            scanned += 1;
            let base = entry.path();
            let default_home = uid.and_then(|uid| {
                let passwd = std::fs::read_to_string(base.join("etc").join("passwd")).ok()?;
                super::wsl::home_for_uid(&passwd, uid)
            });
            let homes = match default_home {
                Some(home) => vec![base.join(home.trim_start_matches('/'))],
                None => {
                    let mut homes = vec![base.join("root")];
                    match std::fs::read_dir(base.join("home")) {
                        Ok(users) => homes.extend(users.flatten().map(|u| u.path())),
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::NotFound | ErrorKind::PermissionDenied
                            ) => {}
                        Err(e) => errors.push(format!("{}: {e}", base.join("home").display())),
                    }
                    homes
                }
            };
            for home in &homes {
                for rel in rels {
                    match file_at(&home.join(rel)) {
                        Ok(true) => return ShareScan::Found,
                        Ok(false) => {}
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
        if scanned == 0 {
            errors.push(match only {
                Some(d) => format!("WSL distro `{d}` is not listed under {}", root.display()),
                None => format!("no WSL distro listed under {}", root.display()),
            });
        }
        return if errors.is_empty() {
            ShareScan::Missing
        } else {
            ShareScan::Unreadable(errors.join("; "))
        };
    }
    ShareScan::Unreadable(if root_errors.is_empty() {
        "no WSL share root".to_string()
    } else {
        root_errors.join("; ")
    })
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

    /// `command -v` exiting 1 is the only "not installed" verdict; a WSL that
    /// couldn't start, a non-`command -v` exit or a killed wsl.exe is WSL
    /// unavailable — and carries wsl.exe's own error code when it gave one.
    #[test]
    fn which_verdict_separates_a_miss_from_wsl_not_answering() {
        assert_eq!(
            which_verdict(Some(0), b"nvm banner\n/usr/bin/claude\n", b""),
            WslLookup::Found("/usr/bin/claude".into())
        );
        assert_eq!(which_verdict(Some(1), b"", b""), WslLookup::NotFound);
        assert_eq!(which_verdict(Some(0), b"", b""), WslLookup::NotFound);

        let utf16: Vec<u8> = "Error code: Wsl/Service/CreateInstance/E_FAIL\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        match which_verdict(Some(-1), b"", &utf16) {
            WslLookup::WslUnavailable(why) => {
                assert!(why.contains("Wsl/Service/CreateInstance/E_FAIL"), "{why}")
            }
            other => panic!("expected WslUnavailable, got {other:?}"),
        }
        // Exit 1 with a WSL error in the output is still WSL, not a miss.
        assert!(matches!(
            which_verdict(Some(1), b"", b"There is no distribution with the supplied name."),
            WslLookup::WslUnavailable(_)
        ));
        assert!(matches!(which_verdict(Some(127), b"", b""), WslLookup::WslUnavailable(_)));
        assert!(matches!(which_verdict(None, b"", b""), WslLookup::WslUnavailable(_)));
    }

    #[test]
    fn sh_quote_survives_single_quotes() {
        assert_eq!(sh_quote("claude"), "'claude'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    /// D-10: a WSL that couldn't be asked stops the name walk at once and is
    /// reported as unavailable — never as a miss; a miss moves on to the next
    /// candidate name; a hit carries the `wsl:<name>:<path>` detection path.
    #[tokio::test]
    async fn lookup_wsl_with_keeps_unavailable_apart_from_a_miss() {
        use std::cell::RefCell;
        let asked = RefCell::new(Vec::<String>::new());
        let r = lookup_wsl_with(&["a", "b", "c"], |n| {
            asked.borrow_mut().push(n.to_string());
            let v = match n {
                "a" => WslLookup::NotFound,
                "b" => WslLookup::WslUnavailable("Wsl/Service/E_FAIL".into()),
                _ => WslLookup::Found("/usr/bin/c".into()),
            };
            async move { v }
        })
        .await;
        assert_eq!(r, WslLookup::WslUnavailable("Wsl/Service/E_FAIL".into()));
        assert_eq!(*asked.borrow(), vec!["a", "b"], "stops at the first WSL failure");

        let r = lookup_wsl_with(&["a", "c"], |n| {
            let v = if n == "c" {
                WslLookup::Found("/usr/bin/c".into())
            } else {
                WslLookup::NotFound
            };
            async move { v }
        })
        .await;
        assert_eq!(r, WslLookup::Found("wsl:c:/usr/bin/c".into()));

        let r = lookup_wsl_with(&["a"], |_| async { WslLookup::NotFound }).await;
        assert_eq!(r, WslLookup::NotFound);
    }

    #[test]
    fn wsl_candidate_names_strip_windows_shims_and_dedupe() {
        let claude = KNOWN_AGENTS.iter().find(|d| d.id == "claude-code").unwrap();
        let names = wsl_candidate_names(claude);
        assert!(!names.is_empty());
        for n in &names {
            assert!(!n.ends_with(".cmd") && !n.ends_with(".exe") && !n.ends_with(".bat"), "{n}");
        }
        let mut uniq = names.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), names.len());
    }

    /// D-10: the host-miss → WSL mapping. No wsl.exe and a WSL miss are
    /// "absent" (no entry); a WSL hit is probed; a WSL that couldn't be asked
    /// is an unprobed entry flagged unavailable — never absent.
    #[test]
    fn wsl_fallback_maps_each_lookup_outcome() {
        let def = KNOWN_AGENTS.iter().find(|d| d.id == "claude-code").unwrap();
        let names = wsl_candidate_names(def);

        assert!(matches!(wsl_fallback(def, &names, None), Err(None)));
        assert!(matches!(
            wsl_fallback(def, &names, Some(WslLookup::NotFound)),
            Err(None)
        ));
        match wsl_fallback(
            def,
            &names,
            Some(WslLookup::Found("wsl:claude:/usr/bin/claude".into())),
        ) {
            Ok(p) => assert_eq!(p, PathBuf::from("wsl:claude:/usr/bin/claude")),
            other => panic!("expected a path to probe, got {other:?}"),
        }
        match wsl_fallback(
            def,
            &names,
            Some(WslLookup::WslUnavailable("Wsl/Service/E_FAIL".into())),
        ) {
            Err(Some(agent)) => {
                assert_eq!(agent.id, "claude-code");
                assert_eq!(agent.version, None);
                assert_eq!(agent.authed, None);
                assert_eq!(
                    agent.unavailable,
                    Some(AgentUnavailable::wsl("Wsl/Service/E_FAIL"))
                );
            }
            other => panic!("expected an unavailable entry, got {other:?}"),
        }
    }

    /// The wire shape: a checked agent carries no `unavailable` key at all
    /// (older consumers see the old shape); an unavailable one carries
    /// `{kind: "wsl", reason}` with nothing probed.
    #[test]
    fn unavailable_is_additive_on_the_wire() {
        let def = KNOWN_AGENTS.iter().find(|d| d.id == "claude-code").unwrap();
        let agent = wsl_unavailable_agent(def, "claude", "Wsl/Service/E_FAIL".into());
        let v = serde_json::to_value(&agent).unwrap();
        assert_eq!(v["id"], "claude-code");
        assert_eq!(v["executable_path"], "claude (WSL)");
        assert_eq!(v["version"], serde_json::Value::Null);
        assert_eq!(v["authed"], serde_json::Value::Null);
        assert_eq!(
            v["unavailable"],
            serde_json::json!({ "kind": "wsl", "reason": "Wsl/Service/E_FAIL" })
        );

        let checked = DetectedAgent {
            unavailable: None,
            ..wsl_unavailable_agent(def, "claude", String::new())
        };
        let v = serde_json::to_value(&checked).unwrap();
        assert!(v.get("unavailable").is_none(), "{v}");
    }

    /// Regression: the old `futures_join_all` awaited each probe in turn, so
    /// one slow agent (a cold WSL start) delayed every other. Three 300 ms
    /// probes must finish in roughly one probe's time, not three.
    #[tokio::test]
    async fn join_all_runs_probes_concurrently() {
        let started = std::time::Instant::now();
        let futs = (0..3).map(|i| async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            i
        });
        assert_eq!(futures_join_all(futs).await, vec![0, 1, 2]);
        assert!(
            started.elapsed() < Duration::from_millis(800),
            "took {:?}",
            started.elapsed()
        );
    }

    /// A share tree on disk shaped like `\wsl.localhost`: `<distro>/root`,
    /// `<distro>/home/<user>`.
    fn fake_share(distros: &[(&str, &[&str])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (distro, homes) in distros {
            let base = dir.path().join(distro);
            std::fs::create_dir_all(base.join("root")).unwrap();
            std::fs::create_dir_all(base.join("home")).unwrap();
            for h in *homes {
                std::fs::create_dir_all(base.join("home").join(h)).unwrap();
            }
        }
        dir
    }

    fn rels(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wsl_share_scan_reads_root_and_user_homes() {
        let share = fake_share(&[("Ubuntu", &["me"])]);
        let roots = [share.path().to_path_buf()];
        let rel = rels(&[".claude/.credentials.json"]);
        assert_eq!(scan_wsl_shares(&roots, &rel, None, None), ShareScan::Missing);

        let cred = share.path().join("Ubuntu/root/.claude/.credentials.json");
        std::fs::create_dir_all(cred.parent().unwrap()).unwrap();
        std::fs::write(&cred, "{}").unwrap();
        assert_eq!(scan_wsl_shares(&roots, &rel, None, None), ShareScan::Found);
    }

    #[test]
    fn wsl_share_scan_skips_docker_desktop_and_other_distros() {
        let share = fake_share(&[("docker-desktop", &["u"]), ("Debian", &["u"]), ("Ubuntu", &["u"])]);
        let roots = [share.path().to_path_buf()];
        let rel = rels(&[".codex/auth.json"]);
        for distro in ["docker-desktop", "Debian"] {
            let f = share.path().join(distro).join("home/u/.codex/auth.json");
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, "{}").unwrap();
        }
        // docker-desktop never counts; Debian counts only when it is the
        // configured distro (or none is configured).
        assert_eq!(scan_wsl_shares(&roots, &rel, Some("Ubuntu"), None), ShareScan::Missing);
        assert_eq!(scan_wsl_shares(&roots, &rel, Some("debian"), None), ShareScan::Found);
        assert_eq!(scan_wsl_shares(&roots, &rel, None, None), ShareScan::Found);
        std::fs::remove_file(share.path().join("Debian/home/u/.codex/auth.json")).unwrap();
        assert_eq!(scan_wsl_shares(&roots, &rel, None, None), ShareScan::Missing);
    }

    /// A share that can't be listed (WSL down, the share not mounted) or a
    /// configured distro that isn't there is "couldn't tell", not "missing".
    #[test]
    fn wsl_share_scan_read_errors_are_inconclusive() {
        let gone = tempfile::tempdir().unwrap().path().join("no-such-share");
        let rel = rels(&[".claude/.credentials.json"]);
        assert!(matches!(
            scan_wsl_shares(&[gone.clone(), gone], &rel, None, None),
            ShareScan::Unreadable(_)
        ));

        let share = fake_share(&[("Ubuntu", &["me"])]);
        let roots = [share.path().to_path_buf()];
        match scan_wsl_shares(&roots, &rel, Some("Arch"), None) {
            ShareScan::Unreadable(why) => assert!(why.contains("Arch"), "{why}"),
            other => panic!("expected Unreadable, got {other:?}"),
        }

        // A share root that lists nothing (no distro running) or only
        // Docker Desktop's distros, with none configured: couldn't tell.
        let empty = tempfile::tempdir().unwrap();
        match scan_wsl_shares(&[empty.path().to_path_buf()], &rel, None, None) {
            ShareScan::Unreadable(why) => assert!(why.contains("no WSL distro"), "{why}"),
            other => panic!("expected Unreadable, got {other:?}"),
        }
        let docker = fake_share(&[("docker-desktop", &["u"])]);
        assert!(matches!(
            scan_wsl_shares(&[docker.path().to_path_buf()], &rel, None, None),
            ShareScan::Unreadable(_)
        ));
    }

    /// With the distro's default uid known, only that user's home counts: a
    /// stale credential in another user's home doesn't sign the CLI in. A
    /// passwd that doesn't name the uid falls back to every home.
    #[test]
    fn wsl_share_scan_reads_only_the_default_users_home() {
        let share = fake_share(&[("Ubuntu", &["me", "old"])]);
        let roots = [share.path().to_path_buf()];
        let rel = rels(&[".claude/.credentials.json"]);
        std::fs::create_dir_all(share.path().join("Ubuntu/etc")).unwrap();
        std::fs::write(
            share.path().join("Ubuntu/etc/passwd"),
            "root:x:0:0:root:/root:/bin/bash\nme:x:1000:1000::/home/me:/bin/bash\nold:x:1001:1001::/home/old:/bin/bash\n",
        )
        .unwrap();
        let stale = share.path().join("Ubuntu/home/old/.claude/.credentials.json");
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(&stale, "{}").unwrap();

        assert_eq!(scan_wsl_shares(&roots, &rel, Some("Ubuntu"), Some(1000)), ShareScan::Missing);
        assert_eq!(scan_wsl_shares(&roots, &rel, Some("Ubuntu"), Some(1001)), ShareScan::Found);
        // uid not in passwd: every home, as before.
        assert_eq!(scan_wsl_shares(&roots, &rel, Some("Ubuntu"), Some(4242)), ShareScan::Found);

        let mine = share.path().join("Ubuntu/home/me/.claude/.credentials.json");
        std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
        std::fs::write(&mine, "{}").unwrap();
        assert_eq!(scan_wsl_shares(&roots, &rel, Some("Ubuntu"), Some(1000)), ShareScan::Found);
    }

    /// A host CLI's credential is the host file; nothing under WSL counts.
    #[tokio::test]
    async fn host_auth_file_probe_reads_the_host_home_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cred.json");
        let path = file.to_string_lossy().into_owned();
        let paths = [path.as_str()];
        let exec = std::path::Path::new("/usr/bin/claude");
        let (val, hint) = probe_auth_files(exec, &paths).await;
        assert_eq!(val, Some(false));
        assert!(hint.unwrap().starts_with("missing: "));
        std::fs::write(&file, "{}").unwrap();
        assert_eq!(probe_auth_files(exec, &paths).await, (Some(true), None));
    }
}
