//! Shell integration hooks emitting OSC 133 prompt markers (WP-08 / T-10).

use std::path::{Path, PathBuf};

use crate::executor::SpawnSpec;

const BASH_SCRIPT: &str = include_str!("../../../src/terminal/shell-integration/bash.sh");
const ZSH_SCRIPT: &str = include_str!("../../../src/terminal/shell-integration/zsh.zsh");
const FISH_SCRIPT: &str = include_str!("../../../src/terminal/shell-integration/fish.fish");

/// Writes shell integration scripts to a deterministic directory on disk and
/// returns that path.
pub fn ensure_shell_integration_dir() -> std::io::Result<PathBuf> {
    let base_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base_dir.join("ikenga").join("shell-integration");
    std::fs::create_dir_all(&dir)?;

    std::fs::write(dir.join("bash.sh"), BASH_SCRIPT)?;
    std::fs::write(dir.join("zsh.zsh"), ZSH_SCRIPT)?;
    std::fs::write(dir.join("fish.fish"), FISH_SCRIPT)?;

    let zsh_dir = dir.join("zsh");
    std::fs::create_dir_all(&zsh_dir)?;
    let zsh_rc = r#"# Ikenga Zsh bootstrap
if [ -n "$USER_ZDOTDIR" ]; then
    ZDOTDIR="$USER_ZDOTDIR"
else
    unset ZDOTDIR
fi
if [ -f "$ZDOTDIR/.zshrc" ]; then
    . "$ZDOTDIR/.zshrc"
elif [ -f "$HOME/.zshrc" ]; then
    . "$HOME/.zshrc"
fi
if [ -f "$(dirname "$0")/../zsh.zsh" ]; then
    . "$(dirname "$0")/../zsh.zsh"
fi
"#;
    std::fs::write(zsh_dir.join(".zshrc"), zsh_rc)?;

    Ok(dir)
}

/// Injects shell integration environment variables and hooks into the PTY
/// child's spawn spec. Appended after the caller's env, so these win — the
/// same precedence they had when this wrote straight to the `CommandBuilder`.
pub fn inject_shell_integration(
    builder: &mut SpawnSpec,
    exec_bin: &str,
    existing_env: &std::collections::HashMap<String, String>,
) {
    let Ok(dir) = ensure_shell_integration_dir() else {
        return;
    };
    let dir_str = dir.to_string_lossy().to_string();

    builder.env("IKENGA_SHELL_INTEGRATION", "1");
    builder.env("IKENGA_SHELL_INTEGRATION_DIR", &dir_str);

    let bin_name = Path::new(exec_bin)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    if bin_name == "bash" || bin_name.starts_with("bash") {
        let bash_script = dir.join("bash.sh");
        let existing_pc = existing_env
            .get("PROMPT_COMMAND")
            .cloned()
            .or_else(|| std::env::var("PROMPT_COMMAND").ok());
        let pc = match existing_pc {
            Some(existing) if !existing.is_empty() => {
                format!(". \"{}\"; {}", bash_script.display(), existing)
            }
            _ => format!(". \"{}\"", bash_script.display()),
        };
        builder.env("PROMPT_COMMAND", pc);
    } else if bin_name == "zsh" || bin_name.starts_with("zsh") {
        let zsh_dir = dir.join("zsh");
        if let Ok(existing_zdotdir) = std::env::var("ZDOTDIR") {
            builder.env("USER_ZDOTDIR", existing_zdotdir);
        }
        builder.env("ZDOTDIR", zsh_dir.to_string_lossy().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_shell_integration_dir() {
        let dir = ensure_shell_integration_dir().expect("should create integration directory");
        assert!(dir.join("bash.sh").exists());
        assert!(dir.join("zsh.zsh").exists());
        assert!(dir.join("fish.fish").exists());
        assert!(dir.join("zsh/.zshrc").exists());

        let bash_content = std::fs::read_to_string(dir.join("bash.sh")).unwrap();
        assert!(bash_content.contains("133;A"));
        assert!(bash_content.contains("133;B"));
        assert!(bash_content.contains("133;C"));
        assert!(bash_content.contains("133;D"));
    }
}
