//! Repair `$PATH` when the app was launched from a GUI surface (Dock, Finder,
//! Spotlight) and inherited launchd's minimal environment. Without this,
//! tools the user installed via Homebrew, asdf, `~/.claude/local`, etc. are
//! invisible to anything we spawn through `portable-pty` or `tokio::process`
//! (notably `claude`, which lives in `~/.claude/local/claude` on a fresh Mac).
//!
//! Mirrors what VS Code / IntelliJ / `fix-path-env` do: ask the user's
//! interactive login shell what `PATH` it uses, merge into ours, and
//! `std::env::set_var` so subsequent `std::env::vars()` reads (including
//! `portable-pty::CommandBuilder`) pick up the augmented value.

#[cfg(not(target_os = "macos"))]
pub fn apply() {
    // On Linux, GUI launchers (most desktop environments) source the user's
    // shell rc files via PAM/systemd-user, so the launched process inherits
    // the right PATH. We could probe defensively, but the cost is a real
    // sub-process on every boot — skip it. Re-add per-platform when we see
    // a Linux user with the same symptom.
}

#[cfg(target_os = "macos")]
pub fn apply() {
    macos::apply();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    const START: &str = "__IKENGA_PATH_START__";
    const END: &str = "__IKENGA_PATH_END__";
    /// Hard cap on the shell-probe round trip. Most invocations finish in
    /// under 200 ms; we abandon at 3 s to keep app boot snappy if the user's
    /// rc files do something pathological.
    const TIMEOUT: Duration = Duration::from_secs(3);

    pub fn apply() {
        let started = Instant::now();
        let probed = match probe_shell_path() {
            Ok(p) => Some(p),
            Err(e) => {
                log::warn!("[path_fix] shell PATH probe failed: {e}");
                None
            }
        };
        let mut merged = current_path_entries();
        let mut changed = false;
        if let Some(extra) = probed {
            for entry in extra {
                if !merged.contains(&entry) {
                    merged.push(entry);
                    changed = true;
                }
            }
        }
        for hint in claude_install_hints() {
            if !merged.contains(&hint) {
                merged.insert(0, hint);
                changed = true;
            }
        }
        if changed {
            let joined = merged.join(":");
            std::env::set_var("PATH", &joined);
            log::info!(
                "[path_fix] PATH augmented in {:?}: {}",
                started.elapsed(),
                joined
            );
        } else {
            log::debug!("[path_fix] no changes in {:?}", started.elapsed());
        }
    }

    fn current_path_entries() -> Vec<String> {
        std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Likely install dirs for `claude` on macOS, plus the Homebrew bins.
    /// We only prepend ones that actually contain an executable named
    /// `claude` (or, for the Homebrew bins, exist on disk) so we don't
    /// pollute PATH with empty hints.
    fn claude_install_hints() -> Vec<String> {
        let mut out = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            let home = std::path::PathBuf::from(home);
            for sub in &[".claude/local", ".local/bin", ".bun/bin", ".cargo/bin"] {
                let candidate = home.join(sub);
                if candidate.join("claude").exists() {
                    out.push(candidate.to_string_lossy().to_string());
                }
            }
        }
        for fixed in &["/opt/homebrew/bin", "/usr/local/bin"] {
            let p = std::path::PathBuf::from(fixed);
            if p.exists() {
                out.push(fixed.to_string());
            }
        }
        out
    }

    /// Spawn the user's `$SHELL` as an interactive login shell and ask it to
    /// print its PATH. Bracketed with sentinels so any noise from rc files
    /// (banners, version checks, etc.) can be stripped reliably.
    fn probe_shell_path() -> Result<Vec<String>, String> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let script = format!(r#"printf '%s' "{START}";printf '%s' "$PATH";printf '%s' "{END}""#);
        // Not executor-routed (WP-18b): runs from desktop `lib.rs::run()`
        // before the Tokio runtime exists, to repair the host process's own
        // PATH (not a session spawn), and needs a std `Child` it can kill on
        // its own timeout — no `SessionExecutor` method hands one back.
        let mut child = Command::new(&shell)
            .arg("-ilc")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Belt-and-braces: ensure the probe shell can't try to attach to
            // a controlling tty we don't have. Without this, some shells
            // (notably bash) write "stty: stdin isn't a terminal" and exit 1.
            .env("TERM", "dumb")
            .spawn()
            .map_err(|e| format!("spawn {shell}: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "missing stdout".to_string())?;

        // Run the wait in a side thread so we can enforce TIMEOUT without
        // dragging in tokio (this code runs before the tauri runtime exists).
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            use std::io::Read;
            let mut stdout = stdout;
            let mut buf = String::new();
            let _ = stdout.read_to_string(&mut buf);
            let _ = tx.send(buf);
        });

        let buf = rx
            .recv_timeout(TIMEOUT)
            .map_err(|_| format!("{shell} PATH probe timed out after {:?}", TIMEOUT))?;
        let _ = child.kill();
        let _ = child.wait();

        let start = buf
            .find(START)
            .ok_or_else(|| "start sentinel missing".to_string())?
            + START.len();
        let end = buf
            .find(END)
            .ok_or_else(|| "end sentinel missing".to_string())?;
        if end < start {
            return Err("sentinels reversed".to_string());
        }
        let path = &buf[start..end];
        Ok(path
            .split(':')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect())
    }
}
