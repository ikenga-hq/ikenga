//! Ngwa in-shell scaffolding engine (WP-23 / locked design D-02).
//!
//! Scaffolds packages and primitives from templates with placeholder substitution
//! (`{{slug}}`, `{{name}}`, `{{description}}`, `{{id}}`, `{{version}}`, `{{author_name}}`,
//! `{{author_key}}`, `{{tools}}`), atomic writes, and safety checks (refuses non-empty target dirs).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::claude_store::{resolve_scope_claude, resolve_scope_root};
use super::db::PaDb;

/// Baked-in package templates from `templates/pkg/`.
static PKG_TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/pkg");

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PkgScaffoldParams {
    /// Kind of equipment:
    /// "skill", "agent", "command", "hook", "workflow", "schedule", "artifact", "app", "tool", "engine", "sidecar", "project"
    pub kind: String,
    /// Display name (e.g. "Release Notes")
    pub name: String,
    /// Lowercase slug (e.g. "release-notes")
    pub slug: String,
    /// Routing description (≥ 20 characters)
    pub description: String,
    /// Scope: "personal", "workspace", or "project:<id>"
    pub scope: String,
    /// Optional project ID if scope is project
    pub project_id: Option<String>,
    /// Optional explicit target directory override (used by tests / custom paths)
    pub target_dir: Option<String>,
    /// Optional tools / capability chips
    pub tools: Option<Vec<String>>,
    /// Optional author name
    pub author_name: Option<String>,
    /// Optional author key
    pub author_key: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PkgScaffoldResult {
    pub ok: bool,
    pub kind: String,
    pub slug: String,
    pub target_path: String,
    pub target_folder: String,
    pub files_written: Vec<String>,
}

/// Validates that slug conforms to lowercase slug pattern: `^[a-z0-9]+(-[a-z0-9]+)*$`
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.trim().is_empty() {
        return Err("slug cannot be empty".to_string());
    }
    let valid = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid || slug.starts_with('-') || slug.ends_with('-') || slug.contains("--") {
        return Err(format!(
            "invalid slug '{slug}': must be lowercase alphanumeric with single hyphens"
        ));
    }
    Ok(())
}

/// Validates description length (minimum 20 characters)
pub fn validate_description(desc: &str) -> Result<(), String> {
    if desc.trim().len() < 20 {
        return Err(format!(
            "description too short ({} chars): minimum 20 characters required",
            desc.trim().len()
        ));
    }
    Ok(())
}

/// Replace all standard placeholders in text.
pub fn substitute_placeholders(
    content: &str,
    params: &PkgScaffoldParams,
    pkg_id: &str,
) -> String {
    let author_name = params.author_name.as_deref().unwrap_or("Royalti");
    let author_key = params.author_key.as_deref().unwrap_or("royalti");
    let tools_str = params
        .tools
        .as_ref()
        .map(|t| t.join(", "))
        .unwrap_or_else(|| "Read, Grep, Glob".to_string());

    content
        .replace("{{slug}}", &params.slug)
        .replace("{{name}}", &params.name)
        .replace("{{description}}", &params.description)
        .replace("{{id}}", pkg_id)
        .replace("{{version}}", "0.1.0")
        .replace("{{author_name}}", author_name)
        .replace("{{author_key}}", author_key)
        .replace("{{tools}}", &tools_str)
        .replace("{{scope}}", &params.scope)
}

/// Resolves target directory and primary target file for a given scaffold request.
pub async fn resolve_destination(
    db: &Arc<PaDb>,
    params: &PkgScaffoldParams,
) -> Result<(PathBuf, PathBuf), String> {
    if let Some(ref explicit) = params.target_dir {
        let folder = PathBuf::from(explicit);
        let primary = match params.kind.as_str() {
            "skill" => folder.join("SKILL.md"),
            "agent" => folder.join(format!("{}.md", params.slug)),
            "command" => folder.join(format!("{}.md", params.slug)),
            "hook" => folder.join(format!("{}.sh", params.slug)),
            "workflow" => folder.join(format!("{}.md", params.slug)),
            "schedule" => folder.join(format!("{}.json", params.slug)),
            "artifact" => folder.join("index.html"),
            "project" => folder.join("CLAUDE.md"),
            _ => folder.join("manifest.json"),
        };
        return Ok((folder, primary));
    }

    let scope_key = if params.scope.is_empty() || params.scope == "personal" {
        "workspace"
    } else {
        params.scope.as_str()
    };

    let scope_claude = resolve_scope_claude(db, scope_key).await?;
    let scope_root = resolve_scope_root(db, scope_key).await?;

    match params.kind.as_str() {
        "skill" => {
            let folder = scope_claude.join("skills").join(&params.slug);
            let primary = folder.join("SKILL.md");
            Ok((folder, primary))
        }
        "agent" => {
            let folder = scope_claude.join("agents");
            let primary = folder.join(format!("{}.md", params.slug));
            Ok((folder, primary))
        }
        "command" => {
            let folder = scope_claude.join("commands");
            let primary = folder.join(format!("{}.md", params.slug));
            Ok((folder, primary))
        }
        "hook" => {
            let folder = scope_claude.join("hooks");
            let primary = folder.join(format!("{}.sh", params.slug));
            Ok((folder, primary))
        }
        "workflow" => {
            let folder = scope_claude.join("workflows");
            let primary = folder.join(format!("{}.md", params.slug));
            Ok((folder, primary))
        }
        "schedule" => {
            let folder = scope_claude.join("schedules");
            let primary = folder.join(format!("{}.json", params.slug));
            Ok((folder, primary))
        }
        "artifact" => {
            let root = scope_root.unwrap_or_else(|| {
                crate::platform::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("Documents")
            });
            let folder = root.join("artifacts").join(&params.slug);
            let primary = folder.join("index.html");
            Ok((folder, primary))
        }
        "project" => {
            let root = scope_root.unwrap_or_else(|| PathBuf::from("."));
            let folder = root.clone();
            let primary = folder.join("CLAUDE.md");
            Ok((folder, primary))
        }
        // Packages: app, tool, engine, sidecar
        _ => {
            let pkg_id = format!("io.royalti.{}", params.slug);
            let folder = if let Some(ref r) = scope_root {
                r.join(".claude").join("pkgs").join(&pkg_id)
            } else {
                let home = crate::platform::home_dir()
                    .ok_or_else(|| "cannot resolve home directory".to_string())?;
                home.join(".ikenga").join("pkgs").join(&pkg_id)
            };
            let primary = folder.join("manifest.json");
            Ok((folder, primary))
        }
    }
}

/// Core execution of scaffolding.
pub fn execute_scaffold(
    params: &PkgScaffoldParams,
    folder: &Path,
    primary_path: &Path,
) -> Result<Vec<String>, String> {
    validate_slug(&params.slug)?;
    validate_description(&params.description)?;

    // Safety guard: refuse if target folder exists and has non-empty contents.
    if folder.exists() && folder.is_dir() {
        let mut entries = fs::read_dir(folder)
            .map_err(|e| format!("read dir {}: {e}", folder.display()))?;
        if entries.next().is_some() {
            // For primitive files sharing a common folder (agents, commands, hooks),
            // check if the specific target file already exists.
            if primary_path.exists() {
                return Err(format!(
                    "target file already exists: {}",
                    primary_path.display()
                ));
            }
        }
    }

    fs::create_dir_all(folder)
        .map_err(|e| format!("failed to create directory {}: {e}", folder.display()))?;

    let pkg_id = format!("io.royalti.{}", params.slug);
    let mut files_written = Vec::new();

    // Check if kind corresponds to a template directory under `templates/pkg/`
    let template_dir_name = match params.kind.as_str() {
        "app" => Some("ui-iframe"),
        "tool" => Some("mcp"),
        "engine" => Some("engine"),
        "sidecar" => Some("sidecar"),
        _ => None,
    };

    if let Some(td_name) = template_dir_name {
        if let Some(td) = PKG_TEMPLATES.get_dir(td_name) {
            write_embedded_dir(td, folder, params, &pkg_id, &mut files_written)?;
            return Ok(files_written);
        }
    }

    // Otherwise, generate primitive blueprints with `<!-- ikenga:auto -->` fences
    match params.kind.as_str() {
        "skill" => {
            // 1. SKILL.md
            let tools_line = params
                .tools
                .as_ref()
                .map(|t| t.join(", "))
                .unwrap_or_else(|| "Read, Grep, Glob".to_string());
            let skill_content = format!(
                r#"---
name: {slug}
description: {desc}
allowed-tools: {tools}
---

# {name}

<!-- ikenga:auto:start body -->
A Chi fills this in when you brief it. Edit freely: it
only rewrites between the fences.
<!-- ikenga:auto:end body -->
"#,
                slug = params.slug,
                desc = params.description.trim(),
                tools = tools_line,
                name = params.name
            );
            let skill_path = folder.join("SKILL.md");
            fs::write(&skill_path, skill_content)
                .map_err(|e| format!("write SKILL.md: {e}"))?;
            files_written.push("SKILL.md".to_string());

            // 2. manifest.json
            let manifest_content = format!(
                r#"{{
  "id": "{pkg_id}",
  "name": "{slug}",
  "version": "0.1.0",
  "ikenga_api": "1",
  "kind": "skill",
  "author": {{ "name": "Royalti", "key": "royalti" }},
  "permissions": {{}}
}}
"#,
                pkg_id = pkg_id,
                slug = params.slug
            );
            let manifest_path = folder.join("manifest.json");
            fs::write(&manifest_path, manifest_content)
                .map_err(|e| format!("write manifest.json: {e}"))?;
            files_written.push("manifest.json".to_string());

            // 3. README.md
            let readme_content = format!("# {}\n\n{}\n", params.name, params.description);
            let readme_path = folder.join("README.md");
            fs::write(&readme_path, readme_content)
                .map_err(|e| format!("write README.md: {e}"))?;
            files_written.push("README.md".to_string());
        }
        "agent" => {
            let tools_line = params
                .tools
                .as_ref()
                .map(|t| t.join(", "))
                .unwrap_or_else(|| "Read, Grep, Glob".to_string());
            let content = format!(
                r#"---
name: {slug}
description: {desc}
tools: {tools}
model: claude-3-5-sonnet-20241022
---

<!-- ikenga:auto:start body -->
# {name}

You are an expert specialist agent. {desc}
<!-- ikenga:auto:end body -->
"#,
                slug = params.slug,
                desc = params.description.trim(),
                tools = tools_line,
                name = params.name
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write agent {}: {e}", primary_path.display()))?;
            files_written.push(format!("{}.md", params.slug));
        }
        "command" => {
            let tools_line = params
                .tools
                .as_ref()
                .map(|t| t.join(", "))
                .unwrap_or_else(|| "Read, Bash".to_string());
            let content = format!(
                r#"---
name: {slug}
description: {desc}
allowed-tools: {tools}
---

<!-- ikenga:auto:start body -->
Execute command task for {name}:
{desc}
<!-- ikenga:auto:end body -->
"#,
                slug = params.slug,
                desc = params.description.trim(),
                tools = tools_line,
                name = params.name
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write command {}: {e}", primary_path.display()))?;
            files_written.push(format!("{}.md", params.slug));
        }
        "hook" => {
            let content = format!(
                r#"#!/usr/bin/env bash
# Hook: {slug}
# Description: {desc}

set -euo pipefail

<!-- ikenga:auto:start body -->
# Security / validation guard
echo "[hook:{slug}] executing guard"
<!-- ikenga:auto:end body -->
"#,
                slug = params.slug,
                desc = params.description.trim()
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write hook {}: {e}", primary_path.display()))?;
            files_written.push(format!("{}.sh", params.slug));
        }
        "workflow" => {
            let phases = params
                .tools
                .as_ref()
                .map(|p| p.clone())
                .unwrap_or_else(|| vec!["research".to_string(), "plan".to_string(), "build".to_string()]);
            let phases_md = phases
                .iter()
                .enumerate()
                .map(|(i, p)| format!("## Phase {}: {}\n\nDescribe tasks for {}.\n", i + 1, p, p))
                .collect::<Vec<_>>()
                .join("\n");
            let content = format!(
                r#"# Workflow: {name}

{desc}

<!-- ikenga:auto:start body -->
{phases_md}
<!-- ikenga:auto:end body -->
"#,
                name = params.name,
                desc = params.description.trim(),
                phases_md = phases_md
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write workflow {}: {e}", primary_path.display()))?;
            files_written.push(format!("{}.md", params.slug));
        }
        "schedule" => {
            let cadence = params
                .tools
                .as_ref()
                .and_then(|t| t.first())
                .cloned()
                .unwrap_or_else(|| "daily 05:00".to_string());
            let cron_expr = match cadence.as_str() {
                "hourly" => "0 * * * *",
                "weekdays 09:00" => "0 9 * * 1-5",
                "weekly Mon" => "0 0 * * 1",
                "monthly 1st" => "0 0 1 * *",
                _ => "0 5 * * *",
            };
            let content = format!(
                r#"{{
  "id": "{slug}",
  "name": "{name}",
  "description": "{desc}",
  "cadence": "{cadence}",
  "cron": "{cron}",
  "enabled": true
}}
"#,
                slug = params.slug,
                name = params.name,
                desc = params.description.trim(),
                cadence = cadence,
                cron = cron_expr
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write schedule {}: {e}", primary_path.display()))?;
            files_written.push(format!("{}.json", params.slug));
        }
        "artifact" => {
            let content = format!(
                r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>{name}</title>
  <style>
    body {{ font-family: system-ui, sans-serif; padding: 2rem; background: #0f172a; color: #f8fafc; }}
  </style>
</head>
<body>
  <h1>{name}</h1>
  <p>{desc}</p>
  <!-- ikenga:auto:start body -->
  <div id="content">Interactive artifact content will render here.</div>
  <!-- ikenga:auto:end body -->
</body>
</html>
"#,
                name = params.name,
                desc = params.description.trim()
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write artifact {}: {e}", primary_path.display()))?;
            files_written.push("index.html".to_string());
        }
        "project" => {
            let content = format!(
                r#"# {name}

{desc}

<!-- ikenga:auto:start body -->
## Project Context
This document seeds the project context for AI agents working in this repository.
<!-- ikenga:auto:end body -->
"#,
                name = params.name,
                desc = params.description.trim()
            );
            fs::write(primary_path, content)
                .map_err(|e| format!("write project CLAUDE.md {}: {e}", primary_path.display()))?;
            files_written.push("CLAUDE.md".to_string());
        }
        _ => {
            return Err(format!("unsupported equipment kind: {}", params.kind));
        }
    }

    Ok(files_written)
}

fn write_embedded_dir(
    dir: &Dir<'_>,
    dest: &Path,
    params: &PkgScaffoldParams,
    pkg_id: &str,
    files_written: &mut Vec<String>,
) -> Result<(), String> {
    for file in dir.files() {
        let rel_path = file.path().to_string_lossy().to_string();
        let target_rel_path = rel_path.replace("{{slug}}", &params.slug);
        let out_file_path = dest.join(&target_rel_path);

        if let Some(parent) = out_file_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
        }

        let raw = file.contents_utf8().unwrap_or("");
        let substituted = substitute_placeholders(raw, params, pkg_id);
        fs::write(&out_file_path, substituted)
            .map_err(|e| format!("write file {}: {e}", out_file_path.display()))?;
        files_written.push(target_rel_path);
    }

    for child in dir.dirs() {
        write_embedded_dir(child, dest, params, pkg_id, files_written)?;
    }

    Ok(())
}

/// Tauri command entrypoint: scaffold an equipment item.
#[tauri::command]
pub async fn pkg_scaffold(
    db: State<'_, Arc<PaDb>>,
    params: PkgScaffoldParams,
) -> Result<PkgScaffoldResult, String> {
    let (folder, primary) = resolve_destination(&db, &params).await?;
    let files = execute_scaffold(&params, &folder, &primary)?;

    Ok(PkgScaffoldResult {
        ok: true,
        kind: params.kind,
        slug: params.slug,
        target_path: primary.to_string_lossy().to_string(),
        target_folder: folder.to_string_lossy().to_string(),
        files_written: files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slug_validation() {
        assert!(validate_slug("release-notes").is_ok());
        assert!(validate_slug("my-agent-123").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("-leading").is_err());
        assert!(validate_slug("trailing-").is_err());
        assert!(validate_slug("double--dash").is_err());
        assert!(validate_slug("Uppercase").is_err());
        assert!(validate_slug("spaces are bad").is_err());
    }

    #[test]
    fn test_description_validation() {
        assert!(validate_description("Short").is_err());
        assert!(validate_description("1234567890123456789").is_err());
        assert!(validate_description("This description contains more than twenty characters.").is_ok());
    }

    #[test]
    fn test_substitute_placeholders() {
        let params = PkgScaffoldParams {
            kind: "skill".to_string(),
            name: "Release Notes".to_string(),
            slug: "release-notes".to_string(),
            description: "Generates release notes from git commits and PRs.".to_string(),
            scope: "personal".to_string(),
            project_id: None,
            target_dir: None,
            tools: Some(vec!["Read".to_string(), "Grep".to_string()]),
            author_name: Some("Royalti".to_string()),
            author_key: Some("royalti".to_string()),
        };

        let template = "Name: {{name}}, Slug: {{slug}}, ID: {{id}}, Tools: {{tools}}";
        let res = substitute_placeholders(template, &params, "io.royalti.release-notes");
        assert_eq!(
            res,
            "Name: Release Notes, Slug: release-notes, ID: io.royalti.release-notes, Tools: Read, Grep"
        );
    }

    #[test]
    fn test_execute_scaffold_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("release-notes");
        let primary = folder.join("SKILL.md");

        let params = PkgScaffoldParams {
            kind: "skill".to_string(),
            name: "Release Notes".to_string(),
            slug: "release-notes".to_string(),
            description: "Generates release notes from git commits and PRs.".to_string(),
            scope: "personal".to_string(),
            project_id: None,
            target_dir: Some(folder.to_string_lossy().to_string()),
            tools: Some(vec!["Read".to_string(), "Bash".to_string()]),
            author_name: None,
            author_key: None,
        };

        let files = execute_scaffold(&params, &folder, &primary).unwrap();
        assert!(files.contains(&"SKILL.md".to_string()));
        assert!(files.contains(&"manifest.json".to_string()));
        assert!(files.contains(&"README.md".to_string()));

        let skill_content = fs::read_to_string(&primary).unwrap();
        assert!(skill_content.contains("name: release-notes"));
        assert!(skill_content.contains("<!-- ikenga:auto:start body -->"));
    }

    #[test]
    fn test_execute_scaffold_refuses_non_empty_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let folder = tmp.path().join("existing");
        fs::create_dir_all(&folder).unwrap();
        let primary = folder.join("SKILL.md");
        fs::write(&primary, "already here").unwrap();

        let params = PkgScaffoldParams {
            kind: "skill".to_string(),
            name: "Release Notes".to_string(),
            slug: "release-notes".to_string(),
            description: "Generates release notes from git commits and PRs.".to_string(),
            scope: "personal".to_string(),
            project_id: None,
            target_dir: Some(folder.to_string_lossy().to_string()),
            tools: None,
            author_name: None,
            author_key: None,
        };

        let err = execute_scaffold(&params, &folder, &primary).unwrap_err();
        assert!(err.contains("already exists"));
    }
}
