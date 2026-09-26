//! WP-53: headless exec for the `shell` run kind (G-ACTIONS §8.1), plus the
//! `{{branch}}` lookup (§8.2). Typed client: `src/lib/actions/runner/shell.ts`.
//!
//! The frontend runner interpolates the template (each value already one
//! quoted argument — §8.2) and applies the DEC-55 trust gate; this command
//! runs the result headless with a `cwd`, bounded in time and output, and
//! returns stdout / stderr / exit code. No PTY, no terminal pane.
//!
//! The shell is fixed so the runner's quoting matches it: `/bin/sh -c` on
//! unix (POSIX single quotes), `powershell.exe -NoProfile -NonInteractive
//! -Command` on Windows (PowerShell verbatim strings).
//!
//! **Defense in depth for project actions.** A `scope: "project"` request
//! must name its action id and the `run` hash it was gated on; the command
//! re-reads the user-side trust record (WP-50, computed from the in-force
//! document, fail-closed) and refuses unless that id is a `shell` action
//! pinned `trusted` at exactly that hash. This does not make the command a
//! security boundary against the frontend itself — any code that can call
//! app commands can already spawn a PTY — but a runner bug cannot execute
//! an untrusted project action.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::actions::trust::{ActionTrust, TrustState};
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecRequest {
    /// The interpolated command line.
    pub command: String,
    /// Absolute working directory; absent or empty = the home directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// `"personal"` or `"project"` — where the action is defined.
    pub scope: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Required for `scope: "project"`.
    #[serde(default)]
    pub action_id: Option<String>,
    /// SHA-256 of the canonical `run` JSON (B-14); required for `scope: "project"`.
    #[serde(default)]
    pub run_hash: Option<String>,
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
    /// Spawn / wait failure (the command never produced an exit status).
    pub error: Option<String>,
}

/// The DEC-55 check for one project `shell` run against the in-force trust
/// entries. Fail-closed: an id that is not listed is untrusted.
pub fn check_project_shell_trust(
    actions: &[ActionTrust],
    action_id: &str,
    run_hash: &str,
) -> Result<(), String> {
    let Some(entry) = actions.iter().find(|entry| entry.id == action_id) else {
        return Err(format!("project action `{action_id}` is not trusted"));
    };
    if entry.kind != "shell" {
        return Err(format!("project action `{action_id}` is not a shell action"));
    }
    if entry.hash != run_hash {
        return Err(format!(
            "project action `{action_id}` changed since it was reviewed; trust it again"
        ));
    }
    match entry.state {
        TrustState::Trusted => Ok(()),
        TrustState::Changed => Err(format!(
            "project action `{action_id}` changed since it was trusted; trust it again"
        )),
        _ => Err(format!("project action `{action_id}` is not trusted")),
    }
}

/// Resolves and checks the working directory.
pub fn resolve_cwd(cwd: Option<&str>, home: Option<PathBuf>) -> Result<PathBuf, String> {
    let dir = match cwd.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => PathBuf::from(value),
        None => home.ok_or_else(|| "no working directory and no home directory".to_string())?,
    };
    if !dir.is_absolute() {
        return Err(format!("working directory `{}` is not absolute", dir.display()));
    }
    if !dir.is_dir() {
        return Err(format!("working directory `{}` does not exist", dir.display()));
    }
    Ok(dir)
}

fn clamp_timeout(requested: Option<u64>) -> u64 {
    requested
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .min(MAX_TIMEOUT_SECS)
}

fn shell_command(command_line: &str, cwd: &Path) -> (Command, &'static str) {
    #[cfg(windows)]
    let (std_cmd, shell) = {
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", command_line]);
        (cmd, "powershell")
    };
    #[cfg(not(windows))]
    let (std_cmd, shell) = {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command_line);
        #[cfg(unix)]
        {
            // Own process group, so a timeout can stop the whole tree.
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        (cmd, "sh")
    };
    let mut cmd = Command::from(std_cmd);
    cmd.current_dir(cwd)
        .env("PATH", crate::runtime::augmented_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .no_console_window();
    (cmd, shell)
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

#[cfg(unix)]
fn kill_tree(child: &tokio::process::Child) {
    if let Some(pid) = child.id() {
        // The child leads its own group (`process_group(0)`): signal the group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_tree(_child: &tokio::process::Child) {}

/// Runs one interpolated `shell` action headless.
pub async fn exec(request: ActionExecRequest, home: Option<PathBuf>) -> ActionExecResult {
    let cwd = match resolve_cwd(request.cwd.as_deref(), home) {
        Ok(dir) => dir,
        Err(error) => {
            return ActionExecResult {
                error: Some(error),
                ..Default::default()
            }
        }
    };
    let (mut cmd, shell) = shell_command(&request.command, &cwd);
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

    let secs = clamp_timeout(request.timeout_secs);
    match timeout(Duration::from_secs(secs), child.wait()).await {
        Ok(Ok(status)) => {
            result.ok = status.success();
            result.exit_code = status.code();
        }
        Ok(Err(e)) => result.error = Some(format!("{shell} wait failed: {e}")),
        Err(_) => {
            result.timed_out = true;
            result.error = Some(format!("timed out after {secs}s"));
            kill_tree(&child);
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
    tracing::info!(
        "[action_exec] scope={} action={:?} exit={:?} timed_out={}",
        request.scope,
        request.action_id,
        result.exit_code,
        result.timed_out
    );
    result
}

/// `shell` run kind. A project action is re-checked against the trust
/// record before anything is spawned (see the module note).
#[tauri::command]
pub async fn action_exec(
    manager: State<'_, Arc<ActionsManager>>,
    request: ActionExecRequest,
) -> Result<ActionExecResult, String> {
    if request.command.trim().is_empty() {
        return Err("empty command".into());
    }
    match request.scope.as_str() {
        "personal" => {}
        "project" => {
            let (Some(action_id), Some(run_hash)) =
                (request.action_id.as_deref(), request.run_hash.as_deref())
            else {
                return Err("a project action run needs its action id and run hash".into());
            };
            let status = manager.trust_status(request.project_id.as_deref()).await?;
            check_project_shell_trust(&status.actions, action_id, run_hash)?;
        }
        other => return Err(format!("unknown action scope `{other}`")),
    }
    Ok(exec(request, crate::platform::home_dir()).await)
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
        let target = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
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

    fn entry(id: &str, kind: &str, hash: &str, state: TrustState) -> ActionTrust {
        ActionTrust {
            id: id.into(),
            name: None,
            kind: kind.into(),
            run: json!({ "kind": kind }),
            hash: hash.into(),
            state,
        }
    }

    #[test]
    fn project_trust_is_fail_closed() {
        let actions = vec![
            entry("ok", "shell", "h1", TrustState::Trusted),
            entry("new", "shell", "h2", TrustState::Untrusted),
            entry("edited", "shell", "h3", TrustState::Changed),
            entry("chi", "chi", "h4", TrustState::NotGated),
        ];
        assert!(check_project_shell_trust(&actions, "ok", "h1").is_ok());
        assert!(check_project_shell_trust(&actions, "missing", "h1").is_err());
        assert!(check_project_shell_trust(&actions, "new", "h2").is_err());
        assert!(check_project_shell_trust(&actions, "edited", "h3").is_err());
        // A caller holding a different `run` than the one pinned.
        assert!(check_project_shell_trust(&actions, "ok", "other").is_err());
        // Only shell actions come through this command.
        assert!(check_project_shell_trust(&actions, "chi", "h4").is_err());
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
    #[tokio::test]
    async fn exec_runs_in_cwd_and_captures() {
        let tmp = std::env::temp_dir();
        let request = ActionExecRequest {
            command: "pwd; echo err 1>&2; exit 3".into(),
            cwd: Some(tmp.display().to_string()),
            scope: "personal".into(),
            project_id: None,
            action_id: None,
            run_hash: None,
            timeout_secs: Some(10),
        };
        let result = exec(request, None).await;
        assert!(!result.ok);
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.stderr.trim(), "err");
        assert!(!result.stdout.trim().is_empty());
        assert_eq!(result.shell, "sh");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn quoted_argument_is_not_spliced() {
        // What the runner's POSIX quoting produces for `a'; echo pwned #`.
        let request = ActionExecRequest {
            command: r#"printf '%s' 'a'\''; echo pwned #'"#.into(),
            cwd: Some(std::env::temp_dir().display().to_string()),
            scope: "personal".into(),
            project_id: None,
            action_id: None,
            run_hash: None,
            timeout_secs: Some(10),
        };
        let result = exec(request, None).await;
        assert_eq!(result.stdout, "a'; echo pwned #");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exec_times_out() {
        let request = ActionExecRequest {
            command: "sleep 30".into(),
            cwd: Some(std::env::temp_dir().display().to_string()),
            scope: "personal".into(),
            project_id: None,
            action_id: None,
            run_hash: None,
            timeout_secs: Some(1),
        };
        let result = exec(request, None).await;
        assert!(result.timed_out);
        assert!(!result.ok);
    }
}
