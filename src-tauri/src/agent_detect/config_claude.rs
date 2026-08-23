//! `detect_agent_config(agent_id, root_path)` — counts what
//! the user already has under provider config dirs (`.claude/`, `.gemini/`,
//! `.codex/`, `.cursor/`) for the given workspace root, plus global counts
//! under user-home projects/sessions and global MCP servers.
//!
//! The wizard renders these so the user immediately sees what state they're
//! starting from (e.g. "12 skills, 3 agents already installed") and so the
//! "would you like to scaffold config?" step can be skipped on workspaces
//! that already have one.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct AgentConfigInventory {
    pub root_path: String,
    pub config_dir_present: bool,
    pub agent_count: u32,
    pub skill_count: u32,
    pub command_count: u32,
    pub mcp_server_count: u32,
    pub project_count: u32,
}

pub fn build_inventory(agent_id: &str, root_path: &str) -> AgentConfigInventory {
    let root = PathBuf::from(root_path);
    match agent_id {
        "claude-code" | "claude" => build_claude_inventory(root),
        "antigravity-cli" | "antigravity" | "gemini-cli" | "gemini" => {
            build_antigravity_inventory(root)
        }
        "codex" | "chatgpt" | "openai" => build_codex_inventory(root),
        "cursor-agent" | "cursor" => build_cursor_inventory(root),
        _ => AgentConfigInventory {
            root_path: root.display().to_string(),
            config_dir_present: false,
            agent_count: 0,
            skill_count: 0,
            command_count: 0,
            mcp_server_count: 0,
            project_count: 0,
        },
    }
}

fn build_claude_inventory(root: PathBuf) -> AgentConfigInventory {
    let dot_claude = root.join(".claude");
    let config_dir_present = dot_claude.is_dir() || root.join(".claude.json").is_file();

    let agent_count = count_markdown_files(&dot_claude.join("agents"));
    let skill_count = count_skill_dirs(&dot_claude.join("skills"));
    let command_count = count_markdown_files(&dot_claude.join("commands"));
    let mcp_server_count = count_mcp_servers();
    let project_count = count_projects();

    AgentConfigInventory {
        root_path: root.display().to_string(),
        config_dir_present,
        agent_count,
        skill_count,
        command_count,
        mcp_server_count,
        project_count,
    }
}

fn build_antigravity_inventory(root: PathBuf) -> AgentConfigInventory {
    let dot_gemini = root.join(".gemini");
    let dot_agents = root.join(".agents");
    let config_dir_present = dot_gemini.is_dir()
        || dot_agents.is_dir()
        || root.join(".geminirules").is_file();

    let agent_count = count_markdown_files(&dot_gemini.join("agents"))
        + count_markdown_files(&dot_agents.join("agents"));
    let skill_count = count_skill_dirs(&dot_gemini.join("skills"))
        + count_skill_dirs(&dot_agents.join("skills"));
    let command_count = count_markdown_files(&dot_gemini.join("commands"))
        + count_markdown_files(&dot_gemini.join("rules"));
    let mcp_server_count = count_antigravity_mcp_servers();
    let project_count = count_antigravity_projects();

    AgentConfigInventory {
        root_path: root.display().to_string(),
        config_dir_present,
        agent_count,
        skill_count,
        command_count,
        mcp_server_count,
        project_count,
    }
}

fn build_codex_inventory(root: PathBuf) -> AgentConfigInventory {
    let dot_codex = root.join(".codex");
    let dot_openai = root.join(".openai");
    let config_dir_present = dot_codex.is_dir() || dot_openai.is_dir();

    let agent_count = count_markdown_files(&dot_codex.join("agents"));
    let skill_count = count_skill_dirs(&dot_codex.join("skills"));
    let command_count = count_markdown_files(&dot_codex.join("commands"));
    let mcp_server_count = count_codex_mcp_servers();
    let project_count = count_codex_sessions();

    AgentConfigInventory {
        root_path: root.display().to_string(),
        config_dir_present,
        agent_count,
        skill_count,
        command_count,
        mcp_server_count,
        project_count,
    }
}

fn build_cursor_inventory(root: PathBuf) -> AgentConfigInventory {
    let dot_cursor = root.join(".cursor");
    let config_dir_present = dot_cursor.is_dir() || root.join(".cursorrules").is_file();

    let command_count = count_markdown_files(&dot_cursor.join("rules"));
    let skill_count = count_skill_dirs(&dot_cursor.join("skills"));

    AgentConfigInventory {
        root_path: root.display().to_string(),
        config_dir_present,
        agent_count: 0,
        skill_count,
        command_count,
        mcp_server_count: 0,
        project_count: 0,
    }
}

/// Count `.md` files at the top level of `dir`. Doesn't recurse — agents
/// and commands are flat-file by convention.
fn count_markdown_files(dir: &Path) -> u32 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n: u32 = 0;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("md"))
                .unwrap_or(false)
        {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Skills are directories with a `SKILL.md` inside.
fn count_skill_dirs(dir: &Path) -> u32 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n: u32 = 0;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() && path.join("SKILL.md").is_file() {
            n = n.saturating_add(1);
        }
    }
    n
}

/// MCP servers configured in `~/.claude.json` under `mcpServers`.
fn count_mcp_servers() -> u32 {
    let Some(path) = home_join(".claude.json") else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    v.get("mcpServers")
        .and_then(|m| m.as_object())
        .map(|m| u32::try_from(m.len()).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// MCP servers configured in `~/.gemini/antigravity/mcp_config.json` or `~/.gemini/mcp.json`.
fn count_antigravity_mcp_servers() -> u32 {
    for rel in &[
        ".gemini/antigravity/mcp_config.json",
        ".gemini/mcp.json",
        ".config/antigravity/mcp.json",
    ] {
        if let Some(path) = home_join(rel) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(m) = v
                        .get("mcpServers")
                        .or_else(|| v.get("servers"))
                        .and_then(|s| s.as_object())
                    {
                        return u32::try_from(m.len()).unwrap_or(u32::MAX);
                    }
                }
            }
        }
    }
    0
}

/// MCP servers configured in `~/.codex/config.json`.
fn count_codex_mcp_servers() -> u32 {
    if let Some(path) = home_join(".codex/config.json") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(m) = v.get("mcpServers").and_then(|s| s.as_object()) {
                    return u32::try_from(m.len()).unwrap_or(u32::MAX);
                }
            }
        }
    }
    0
}

/// Claude Code per-cwd project histories under `~/.claude/projects/`.
fn count_projects() -> u32 {
    let Some(path) = home_join(".claude/projects") else {
        return 0;
    };
    let Ok(rd) = std::fs::read_dir(&path) else {
        return 0;
    };
    let mut n: u32 = 0;
    for entry in rd.flatten() {
        if entry.path().is_dir() {
            n = n.saturating_add(1);
        }
    }
    n
}

/// Antigravity conversations / projects in `~/.gemini/antigravity/brain` or app data.
fn count_antigravity_projects() -> u32 {
    for rel in &[".gemini/antigravity/brain", ".gemini/projects"] {
        if let Some(path) = home_join(rel) {
            if let Ok(rd) = std::fs::read_dir(&path) {
                let mut n: u32 = 0;
                for entry in rd.flatten() {
                    if entry.path().is_dir() {
                        n = n.saturating_add(1);
                    }
                }
                if n > 0 {
                    return n;
                }
            }
        }
    }
    0
}

/// Codex sessions under `~/.codex/sessions` or `~/.codex/history`.
fn count_codex_sessions() -> u32 {
    for rel in &[".codex/sessions", ".codex/history"] {
        if let Some(path) = home_join(rel) {
            if let Ok(rd) = std::fs::read_dir(&path) {
                let mut n: u32 = 0;
                for entry in rd.flatten() {
                    if entry.path().is_file() || entry.path().is_dir() {
                        n = n.saturating_add(1);
                    }
                }
                if n > 0 {
                    return n;
                }
            }
        }
    }
    0
}

fn home_join(rel: &str) -> Option<PathBuf> {
    crate::platform::home_dir().map(|h| h.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn returns_empty_for_missing_root() {
        let inv = build_inventory(
            "claude-code",
            "/definitely/does/not/exist/ikenga-detect-test",
        );
        assert!(!inv.config_dir_present);
        assert_eq!(inv.agent_count, 0);
        assert_eq!(inv.skill_count, 0);
        assert_eq!(inv.command_count, 0);
    }

    #[test]
    fn counts_markdown_files_and_skill_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let dot = root.join(".claude");
        fs::create_dir_all(dot.join("agents")).unwrap();
        fs::create_dir_all(dot.join("commands")).unwrap();
        fs::create_dir_all(dot.join("skills").join("foo")).unwrap();
        fs::create_dir_all(dot.join("skills").join("bar")).unwrap();
        // A skill dir without SKILL.md doesn't count.
        fs::create_dir_all(dot.join("skills").join("baz")).unwrap();

        fs::write(dot.join("agents/one.md"), "x").unwrap();
        fs::write(dot.join("agents/two.md"), "x").unwrap();
        fs::write(dot.join("agents/notes.txt"), "x").unwrap();
        fs::write(dot.join("commands/alpha.md"), "x").unwrap();
        fs::write(dot.join("skills/foo/SKILL.md"), "x").unwrap();
        fs::write(dot.join("skills/bar/SKILL.md"), "x").unwrap();

        let inv = build_inventory("claude-code", root.to_str().unwrap());
        assert!(inv.config_dir_present);
        assert_eq!(inv.agent_count, 2);
        assert_eq!(inv.skill_count, 2);
        assert_eq!(inv.command_count, 1);
    }

    #[test]
    fn counts_antigravity_inventory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let dot = root.join(".gemini");
        fs::create_dir_all(dot.join("skills").join("custom")).unwrap();
        fs::write(dot.join("skills/custom/SKILL.md"), "x").unwrap();
        fs::create_dir_all(dot.join("rules")).unwrap();
        fs::write(dot.join("rules/guidelines.md"), "x").unwrap();

        let inv = build_inventory("antigravity", root.to_str().unwrap());
        assert!(inv.config_dir_present);
        assert_eq!(inv.skill_count, 1);
        assert_eq!(inv.command_count, 1);
    }
}
