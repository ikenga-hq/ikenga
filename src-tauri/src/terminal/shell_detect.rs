//! Auto-detection of installed shells and WSL distributions on the host machine.

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
        if !default_set {
            default_set = true;
        }
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
    let wsl_bin = PathBuf::from(r"C:\Windows\System32\wsl.exe");
    if !wsl_bin.is_file() && which::which("wsl.exe").is_err() {
        return;
    }

    // Query distros via registry or wsl.exe -l -q
    let distros = read_wsl_distros_from_wsl_exe();

    if !distros.is_empty() {
        for distro in distros {
            profiles.push(ShellProfile {
                id: format!("wsl:{distro}"),
                label: format!("WSL: {distro}"),
                icon: "wsl".to_string(),
                cmd: vec!["wsl.exe".to_string(), "-d".to_string(), distro.clone()],
                is_default: false,
                kind: "wsl".to_string(),
                distro: Some(distro),
            });
        }
    } else {
        // Fallback default WSL profile if wsl.exe exists
        profiles.push(ShellProfile {
            id: "wsl:default".to_string(),
            label: "WSL (Default)".to_string(),
            icon: "wsl".to_string(),
            cmd: vec!["wsl.exe".to_string()],
            is_default: false,
            kind: "wsl".to_string(),
            distro: None,
        });
    }
}

#[cfg(windows)]
fn read_wsl_distros_from_wsl_exe() -> Vec<String> {
    use std::process::Command;

    let mut distros = Vec::new();
    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000
        cmd.creation_flags(0x08000000);
    }

    if let Ok(output) = cmd.output() {
        if output.status.success() {
            // wsl.exe -l -q outputs UTF-16LE or UTF-8 depending on Windows build
            let text = if output.stdout.len() >= 2 && (output.stdout[1] == 0 || output.stdout[0] == 0xff && output.stdout[1] == 0xfe) {
                let u16s: Vec<u16> = output.stdout
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&u16s)
            } else {
                String::from_utf8_lossy(&output.stdout).to_string()
            };

            for line in text.lines() {
                let clean = line.trim().trim_matches('\0').trim();
                if !clean.is_empty() && !distros.contains(&clean.to_string()) {
                    distros.push(clean.to_string());
                }
            }
        }
    }
    distros
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
        if Path::new(path).is_file() {
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
