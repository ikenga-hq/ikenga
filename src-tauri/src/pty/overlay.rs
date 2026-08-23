//! Session-isolated CLAUDE_CONFIG_DIR overlay builder (WP-00).
//!
//! Creates an Ikenga-controlled overlay directory (`CLAUDE_CONFIG_DIR`),
//! symlinking user assets (projects, credentials, plugins, skills, mcpServers)
//! and seeding `settings.json` so host features (statusline, hooks) can inject
//! configuration invisibly without modifying `~/.claude/settings.json`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn create_symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    std::os::unix::fs::symlink(original, link)
}

#[cfg(windows)]
fn create_symlink<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    if original.as_ref().is_dir() {
        std::os::windows::fs::symlink_dir(original, link)
    } else {
        std::os::windows::fs::symlink_file(original, link)
    }
}

/// Returns the base directory for Ikenga's Claude config overlay.
pub fn get_claude_overlay_dir() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join("ikenga").join("claude-overlay");
        }
    }
    if let Some(home) = crate::platform::home_dir() {
        return home.join(".cache").join("ikenga").join("claude-overlay");
    }
    std::env::temp_dir().join("ikenga-claude-overlay")
}

/// Ensures the overlay directory exists and is populated with symlinks to real `~/.claude`
/// assets and an isolated `settings.json`.
pub fn ensure_claude_overlay_dir() -> io::Result<PathBuf> {
    let overlay_dir = get_claude_overlay_dir();
    fs::create_dir_all(&overlay_dir)?;

    let real_claude_dir = crate::platform::home_dir().map(|h| h.join(".claude"));
    if let Some(ref real_dir) = real_claude_dir {
        if real_dir.exists() && real_dir.is_dir() {
            // Directories to symlink
            let dirs_to_link = ["projects", "plugins", "skills", "mcpServers", "commands"];
            for dir_name in &dirs_to_link {
                let target = real_dir.join(dir_name);
                let link = overlay_dir.join(dir_name);
                if target.exists() && !link.exists() {
                    let _ = create_symlink(&target, &link);
                }
            }

            // Files to symlink
            let files_to_link = ["credentials.json", ".claude.json", "claude.json"];
            for file_name in &files_to_link {
                let target = real_dir.join(file_name);
                let link = overlay_dir.join(file_name);
                if target.exists() && !link.exists() {
                    let _ = create_symlink(&target, &link);
                }
            }

            // Copy settings.json if real exists and overlay settings.json does not exist
            let real_settings = real_dir.join("settings.json");
            let overlay_settings = overlay_dir.join("settings.json");
            if real_settings.exists() && !overlay_settings.exists() {
                let _ = fs::copy(&real_settings, &overlay_settings);
            }
        }
    }

    // Ensure settings.json exists in overlay even if ~/.claude/settings.json didn't exist
    let overlay_settings = overlay_dir.join("settings.json");
    if !overlay_settings.exists() {
        let _ = fs::write(&overlay_settings, "{}");
    }

    // Configure statusLine command block in the overlay settings.json
    let _ = crate::iyke::statusline::configure_overlay_statusline(&overlay_dir, 0, None);

    // Configure hooks block in the overlay settings.json
    let _ = crate::iyke::hooks::configure_overlay_hooks(&overlay_dir, 0, None);

    Ok(overlay_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_claude_overlay_dir() {
        let overlay = ensure_claude_overlay_dir().expect("should create overlay dir");
        assert!(overlay.exists());
        assert!(overlay.join("settings.json").exists());
    }
}
