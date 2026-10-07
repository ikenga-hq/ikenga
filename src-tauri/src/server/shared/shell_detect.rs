//! Auto-detection of installed shells and WSL distributions on the host machine.
//!
//! Lives in the ungated `server::shared` (WP-19 slice 5b); `crate::terminal`
//! re-exports it for the desktop's `terminal_detect_shells`. On Unix it only
//! probes fixed shell paths (`is_file`) and reads `$SHELL` — no process. The
//! Windows arm runs `wsl.exe -l -q` (`read_wsl_distros_from_wsl_exe`), so the
//! daemon serves this only on non-Windows hosts; see `server::rpc_claude`.

// Used only by the Windows-only detection fns below.
#[cfg(windows)]
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellProfile {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub cmd: Vec<String>,
    pub is_default: bool,
    pub kind: String,
    pub distro: Option<String>,
}

/// Detects available shells on the current operating system.
pub fn detect_shells() -> Vec<ShellProfile> {
    let mut profiles = Vec::new();

    #[cfg(windows)]
    {
        detect_windows_shells(&mut profiles);
    }

    #[cfg(not(windows))]
    {
        detect_unix_shells(&mut profiles);
    }

    // Ensure at least one profile exists as default fallback
    if profiles.is_empty() {
        #[cfg(windows)]
        profiles.push(ShellProfile {
            id: "powershell".to_string(),
            label: "Windows PowerShell".to_string(),
            icon: "powershell".to_string(),
            cmd: vec!["powershell.exe".to_string(), "-NoLogo".to_string()],
            is_default: true,
            kind: "powershell".to_string(),
            distro: None,
        });

        #[cfg(not(windows))]
        profiles.push(ShellProfile {
            id: "bash".to_string(),
            label: "bash".to_string(),
            icon: "bash".to_string(),
            cmd: vec!["bash".to_string(), "-l".to_string()],
            is_default: true,
            kind: "bash".to_string(),
            distro: None,
        });
    }

    // Guarantee exactly one is marked is_default if none was set
    if !profiles.iter().any(|p| p.is_default) && !profiles.is_empty() {
        profiles[0].is_default = true;
    }

    profiles
}

#[cfg(windows)]
fn detect_windows_shells(profiles: &mut Vec<ShellProfile>) {
    let mut default_set = false;

    // 1. PowerShell 7+ (pwsh.exe) - modern preferred default
    let pwsh_paths = [
        which::which("pwsh.exe").ok(),
        std::env::var_os("ProgramFiles")
            .map(|p| PathBuf::from(p).join("PowerShell").join("7").join("pwsh.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("Microsoft").join("WindowsApps").join("pwsh.exe")),
    ];

    for candidate in pwsh_paths.into_iter().flatten() {
        if candidate.is_file() {
            profiles.push(ShellProfile {
                id: "pwsh".to_string(),
                label: "PowerShell 7".to_string(),
                icon: "powershell".to_string(),
                cmd: vec![candidate.to_string_lossy().into_owned(), "-NoLogo".to_string()],
                is_default: !default_set,
                kind: "pwsh".to_string(),
                distro: None,
            });
            default_set = true;
            break;
        }
    }

    // 2. Windows PowerShell (powershell.exe) - legacy built-in fallback
    let powershell_path = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    if powershell_path.is_file() || which::which("powershell.exe").is_ok() {
        profiles.push(ShellProfile {
            id: "powershell".to_string(),
            label: "Windows PowerShell".to_string(),
            icon: "powershell".to_string(),
            cmd: vec!["powershell.exe".to_string(), "-NoLogo".to_string()],
            is_default: !default_set,
            kind: "powershell".to_string(),
            distro: None,
        });
        // No later profile reads default_set: WSL distros are always pushed
        // with is_default: false, and the invariant above backstops the case
        // where nothing claimed it.
    }

    // 3. WSL Distributions
    detect_wsl_distributions(profiles);

    // 4. Git Bash
    let git_bash_candidates = [
        std::env::var_os("ProgramFiles")
            .map(|p| PathBuf::from(p).join("Git").join("bin").join("bash.exe")),
        std::env::var_os("ProgramFiles(x86)")
            .map(|p| PathBuf::from(p).join("Git").join("bin").join("bash.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("Programs").join("Git").join("bin").join("bash.exe")),
    ];

    for candidate in git_bash_candidates.into_iter().flatten() {
        if candidate.is_file() {
            profiles.push(ShellProfile {
                id: "git-bash".to_string(),
                label: "Git Bash".to_string(),
                icon: "bash".to_string(),
                cmd: vec![candidate.to_string_lossy().into_owned(), "-l".to_string()],
                is_default: false,
                kind: "bash".to_string(),
                distro: None,
            });
            break;
        }
    }

    // 5. Command Prompt (cmd.exe)
    profiles.push(ShellProfile {
        id: "cmd".to_string(),
        label: "Command Prompt".to_string(),
        icon: "cmd".to_string(),
        cmd: vec!["cmd.exe".to_string()],
        is_default: false,
        kind: "cmd".to_string(),
        distro: None,
    });
}

#[cfg(windows)]
fn detect_wsl_distributions(profiles: &mut Vec<ShellProfile>) {
    if !crate::server::shared::wsl::wsl_exe_present() {
        return;
    }
    // A profile per user distro. No distro listed — or `wsl.exe -l -q`
    // failing, timing out, or listing nothing — offers no WSL profile at
    // all: a "WSL (Default)" entry there opens a terminal that only prints
    // WSL's own error.
    match read_wsl_distros_from_wsl_exe() {
        Ok(distros) => profiles.extend(wsl_profiles(distros)),
        Err(e) => tracing::warn!(target: "ikenga::terminal", "listing WSL distros: {e}"),
    }
}

/// One terminal profile per WSL distro.
#[cfg_attr(not(windows), allow(dead_code))]
fn wsl_profiles(distros: Vec<String>) -> impl Iterator<Item = ShellProfile> {
    distros.into_iter().map(|distro| ShellProfile {
        id: format!("wsl:{distro}"),
        label: format!("WSL: {distro}"),
        icon: "wsl".to_string(),
        cmd: vec!["wsl.exe".to_string(), "-d".to_string(), distro.clone()],
        is_default: false,
        kind: "wsl".to_string(),
        distro: Some(distro),
    })
}

/// How long `wsl.exe -l -q` may take. Listing doesn't boot the VM, so this
/// only trips on a wedged WSL service — which would otherwise hang shell
/// detection (and the terminal menu) indefinitely.
#[cfg(windows)]
const WSL_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The user distros `wsl.exe -l -q` lists (Docker Desktop's dropped), or why
/// it couldn't say.
#[cfg(windows)]
fn read_wsl_distros_from_wsl_exe() -> Result<Vec<String>, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    use crate::platform::NoConsoleWindow;

    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.no_console_window();
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start wsl.exe: {e}"))?;

    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= WSL_LIST_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "wsl.exe -l -q did not answer within {}s",
                    WSL_LIST_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(e) => return Err(format!("waiting for wsl.exe: {e}")),
        }
    };
    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if !status.success() {
        let mut stderr = Vec::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_end(&mut stderr);
        }
        // `-l -q` with no distro installed exits non-zero and explains on
        // stdout; either stream may carry the reason.
        let text = crate::server::shared::wsl::decode_wsl_output(if stderr.is_empty() {
            &stdout
        } else {
            &stderr
        });
        let reason = text.split_whitespace().collect::<Vec<_>>().join(" ");
        return Err(format!("wsl.exe -l -q exited {status}: {reason}"));
    }
    Ok(crate::server::shared::wsl::parse_distro_list(&stdout))
}

#[cfg(not(windows))]
fn detect_unix_shells(profiles: &mut Vec<ShellProfile>) {
    let mut default_shell = std::env::var("SHELL").unwrap_or_default();
    if default_shell.is_empty() {
        default_shell = "/bin/bash".to_string();
    }

    let candidates = [
        ("/bin/zsh", "zsh", "zsh"),
        ("/usr/bin/zsh", "zsh", "zsh"),
        ("/opt/homebrew/bin/zsh", "zsh (homebrew)", "zsh"),
        ("/bin/bash", "bash", "bash"),
        ("/usr/bin/bash", "bash", "bash"),
        ("/usr/local/bin/bash", "bash (local)", "bash"),
        ("/opt/homebrew/bin/bash", "bash (homebrew)", "bash"),
        ("/usr/bin/fish", "fish", "fish"),
        ("/opt/homebrew/bin/fish", "fish (homebrew)", "fish"),
    ];

    let mut added_ids = std::collections::HashSet::new();

    for (path, label, kind) in candidates {
        if std::path::Path::new(path).is_file() {
            let id = kind.to_string();
            if !added_ids.contains(&id) {
                let is_def = path == default_shell || (path.ends_with("zsh") && default_shell.ends_with("zsh")) || (path.ends_with("bash") && default_shell.ends_with("bash"));
                profiles.push(ShellProfile {
                    id: id.clone(),
                    label: label.to_string(),
                    icon: kind.to_string(),
                    cmd: vec![path.to_string(), "-l".to_string()],
                    is_default: is_def,
                    kind: kind.to_string(),
                    distro: None,
                });
                added_ids.insert(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each listed distro becomes a `wsl -d <distro>` profile; nothing listed
    /// means no WSL profile (no "WSL (Default)" fallback that can't open).
    #[test]
    fn wsl_profiles_name_each_distro_and_none_when_empty() {
        let profiles: Vec<ShellProfile> = wsl_profiles(vec!["Ubuntu".into()]).collect();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].id, "wsl:Ubuntu");
        assert_eq!(profiles[0].cmd, ["wsl.exe", "-d", "Ubuntu"]);
        assert_eq!(profiles[0].distro.as_deref(), Some("Ubuntu"));
        assert_eq!(wsl_profiles(Vec::new()).count(), 0);
    }
}
