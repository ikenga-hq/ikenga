//! Cross-platform user-home + shell helpers.
//!
//! The rest of the codebase used to read `$HOME` directly, which is unset
//! on Windows and produced "HOME not set" install failures. Route every
//! callsite through `home_dir()` so Windows can fall back to `%USERPROFILE%`
//! (and, as a last resort, `%HOMEDRIVE%%HOMEPATH%`).

use std::path::PathBuf;

/// Resolve the current user's home directory. Returns `None` only when no
/// reasonable env-var hint is set (effectively never on a real user session).
pub fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            let pb = PathBuf::from(p);
            if !pb.as_os_str().is_empty() {
                return Some(pb);
            }
        }
        match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
            (Some(drive), Some(path)) => {
                let mut s = drive;
                s.push(&path);
                let pb = PathBuf::from(s);
                if !pb.as_os_str().is_empty() {
                    return Some(pb);
                }
            }
            _ => {}
        }
        // POSIX-style $HOME is sometimes set under MSYS / Git-Bash; honor
        // it as a last resort so those shells aren't broken.
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    } else {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
    }
}

/// Platform-default interactive shell argv for a fresh terminal pane.
/// Windows prefers PowerShell (always present); falls back to `cmd.exe` when
/// PowerShell isn't on PATH. POSIX defaults to the user's `$SHELL` or bash.
pub fn default_shell_argv() -> Vec<String> {
    #[cfg(windows)]
    {
        // Prefer PowerShell 7 (`pwsh`) when available; otherwise the inbox
        // `powershell.exe` (Windows PowerShell 5.1, ships with every supported
        // Windows version). Fall back to `cmd.exe` if neither resolves —
        // shouldn't happen on a real Windows box, but keeps the spawn path
        // honest.
        if which::which("pwsh").is_ok() {
            return vec!["pwsh".to_string(), "-NoLogo".to_string()];
        }
        if which::which("powershell").is_ok() {
            return vec!["powershell.exe".to_string(), "-NoLogo".to_string()];
        }
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string());
        return vec![comspec];
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        vec![shell, "-l".to_string()]
    }
}

/// Windows-only: prevents a console-subsystem child (node, bun, npm.cmd,
/// taskkill, wsl.exe, ...) spawned from this GUI process from popping up its
/// own visible console window. No effect on macOS/Linux.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Shared helper so every spawn site opts out of the console-flash consistently,
/// instead of each callsite re-deriving the flag (or forgetting it). Implemented
/// for both `std::process::Command` and `tokio::process::Command`.
///
/// `creation_flags` *replaces* whatever flags were previously set rather than
/// merging with them, so a callsite that also needs e.g.
/// `CREATE_NEW_PROCESS_GROUP` must OR that flag in before calling this (there's
/// no getter to read back an already-set value) — none of today's callsites do,
/// but keep that in mind before adding one.
pub trait NoConsoleWindow {
    fn no_console_window(&mut self) -> &mut Self;
}

impl NoConsoleWindow for std::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

impl NoConsoleWindow for tokio::process::Command {
    fn no_console_window(&mut self) -> &mut Self {
        // tokio::process::Command exposes `creation_flags` as an inherent
        // Windows-only method (no CommandExt import needed, unlike std's).
        #[cfg(windows)]
        {
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_resolves_on_unix() {
        // On the CI hosts we run, HOME is always set. Don't assert content —
        // just that we get *something* back.
        #[cfg(not(windows))]
        {
            if std::env::var_os("HOME").is_some() {
                assert!(home_dir().is_some());
            }
        }
    }

    #[test]
    fn default_shell_argv_non_empty() {
        let argv = default_shell_argv();
        assert!(!argv.is_empty());
        assert!(!argv[0].is_empty());
    }
}
