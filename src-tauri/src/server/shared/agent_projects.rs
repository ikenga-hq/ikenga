//! The first-run wizard's project-history listers (`list_claude_projects`,
//! `list_agent_projects`) and the Claude slug decoder they use — moved from
//! `agent_detect` into the ungated `server::shared` (WP-19 slice 5b) so the
//! daemon's `/api/rpc` arms list with the same code. `agent_detect` keeps the
//! `#[tauri::command]`s and re-exports everything here.
//!
//! Read-only and spawn-free: directory entries, their mtimes and `.jsonl`
//! counts, plus `exists()` probes while decoding a slug. No caller path is
//! read — every root is under `home`, which is a parameter: the desktop
//! passes the process home, the daemon its router home (the daemon PROCESS's
//! home — single-user seam, G-PRINCIPAL / WP-20).

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ClaudeProjectEntry {
    pub slug: String,
    pub path: String,
    pub display_path: String,
    pub session_count: u32,
    pub last_modified_ms: u64,
    /// True when `path` was confirmed to exist on disk via `metadata()`.
    /// When false, the wizard renders it as a best-effort guess so the
    /// user can verify before adding it as a project root.
    pub path_verified: bool,
}

/// Scan `~/.claude/projects/` for project session directories. Each entry
/// reflects a slugged project path (Claude Code encodes the real path by
/// replacing `/` with `-`). The Phase 4 roots step uses this to seed
/// suggestions for `claudeProjectRoots`.
fn scan_claude_project_directory(
    projects: &std::path::Path,
    home: &std::path::Path,
    out: &mut Vec<ClaudeProjectEntry>,
) {
    let Ok(read) = std::fs::read_dir(projects) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let slug = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        let (decoded, path_verified) = decode_claude_slug_with_fs(&slug);

        let mut session_count: u32 = 0;
        let mut last_modified_ms: u64 = 0;
        if let Ok(entries) = std::fs::read_dir(&path) {
            for f in entries.flatten() {
                let fp = f.path();
                if fp.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    session_count += 1;
                    if let Ok(md) = f.metadata() {
                        if let Ok(modified) = md.modified() {
                            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                                let ms = dur.as_millis() as u64;
                                if ms > last_modified_ms {
                                    last_modified_ms = ms;
                                }
                            }
                        }
                    }
                }
            }
        }

        out.push(ClaudeProjectEntry {
            slug,
            path: decoded.clone(),
            display_path: contract_home(&decoded, home),
            session_count,
            last_modified_ms,
            path_verified,
        });
    }
}

/// Scan `<home>/.claude/projects/` (and WSL on Windows) for project session
/// directories. No home = no projects.
pub(crate) fn list_claude_projects_in(home: Option<&Path>) -> Vec<ClaudeProjectEntry> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut out: Vec<ClaudeProjectEntry> = Vec::new();

    // 1. Host user home `.claude/projects`
    let host_projects = home.join(".claude").join("projects");
    scan_claude_project_directory(&host_projects, home, &mut out);

    // 2. On Windows, scan WSL distributions if present
    #[cfg(windows)]
    {
        for wsl_root in &[r"\\wsl.localhost", r"\\wsl$"] {
            let root = std::path::Path::new(wsl_root);
            if let Ok(distros) = std::fs::read_dir(root) {
                for distro in distros.flatten() {
                    let home_dir = distro.path().join("home");
                    if let Ok(users) = std::fs::read_dir(&home_dir) {
                        for user in users.flatten() {
                            let wsl_claude_projects = user.path().join(".claude").join("projects");
                            if wsl_claude_projects.is_dir() {
                                scan_claude_project_directory(
                                    &wsl_claude_projects,
                                    &user.path(),
                                    &mut out,
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    out.sort_by(|a, b| b.last_modified_ms.cmp(&a.last_modified_ms));
    out
}

/// Generic project/conversation history lister across supported AI agents,
/// under `home`. No home = no projects.
pub(crate) fn list_agent_projects_in(
    agent_id: &str,
    home: Option<&Path>,
) -> Vec<ClaudeProjectEntry> {
    match agent_id {
        "antigravity-cli" | "antigravity" | "gemini-cli" | "gemini" => {
            let Some(home) = home else {
                return Vec::new();
            };
            let mut out: Vec<ClaudeProjectEntry> = Vec::new();
            let brain_dir = home.join(".gemini").join("antigravity").join("brain");
            if let Ok(rd) = std::fs::read_dir(&brain_dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if !p.is_dir() {
                        continue;
                    }
                    let slug = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let mut file_count: u32 = 0;
                    let mut mtime: u64 = 0;
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                                mtime = dur.as_millis() as u64;
                            }
                        }
                    }
                    if let Ok(sub) = std::fs::read_dir(&p) {
                        for f in sub.flatten() {
                            file_count += 1;
                            if let Ok(meta) = f.metadata() {
                                if let Ok(mod_t) = meta.modified() {
                                    if let Ok(dur) = mod_t.duration_since(std::time::UNIX_EPOCH) {
                                        let ms = dur.as_millis() as u64;
                                        if ms > mtime {
                                            mtime = ms;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    out.push(ClaudeProjectEntry {
                        slug: slug.clone(),
                        path: p.display().to_string(),
                        display_path: format!("brain/{}", slug),
                        session_count: file_count,
                        last_modified_ms: mtime,
                        path_verified: true,
                    });
                }
            }
            out.sort_by(|a, b| b.last_modified_ms.cmp(&a.last_modified_ms));
            out
        }
        "codex" | "chatgpt" | "openai" => {
            let Some(home) = home else {
                return Vec::new();
            };
            let mut out: Vec<ClaudeProjectEntry> = Vec::new();
            let codex_sessions = home.join(".codex").join("sessions");
            if let Ok(rd) = std::fs::read_dir(&codex_sessions) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    let slug = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let mut mtime: u64 = 0;
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                                mtime = dur.as_millis() as u64;
                            }
                        }
                    }
                    out.push(ClaudeProjectEntry {
                        slug: slug.clone(),
                        path: p.display().to_string(),
                        display_path: format!(".codex/{}", slug),
                        session_count: 1,
                        last_modified_ms: mtime,
                        path_verified: true,
                    });
                }
            }
            out.sort_by(|a, b| b.last_modified_ms.cmp(&a.last_modified_ms));
            out
        }
        _ => list_claude_projects_in(home),
    }
}

/// Pure-string fallback used when no FS probe matches: prepend `/` and
/// replace every `-` with `/`. Exposed for unit tests.
#[allow(dead_code)]
pub fn decode_claude_slug_naive(slug: &str) -> String {
    if slug.starts_with('-') {
        let mut s = String::from("/");
        s.push_str(&slug[1..].replace('-', "/"));
        s
    } else {
        slug.to_string()
    }
}

/// Separators Claude's slug encoding flattens into `-`. The path separator is
/// probed before these — that's the canonical encoding.
const INNER_SEPARATORS: [char; 3] = ['-', '_', '.'];

/// How many tokens a single path component may absorb during lookahead.
/// Bounds the probe count at `3 * (MAX_LOOKAHEAD - 1)` per miss.
const MAX_LOOKAHEAD: usize = 8;

/// Walk `tokens`, extending `acc` one component at a time.
///
/// Fast path is a single token joined with `dir_sep` (then the inner
/// separators). On a real filesystem every prefix exists, so this hits
/// immediately and costs one probe.
///
/// When nothing matches, the component itself may contain separators that the
/// slug flattened, in which case *no* prefix of it exists and stepping one
/// token at a time can never reach it. The motivating case is a directory
/// named `royalti-server-v2-6`: neither `royalti-server` nor `royalti-server-v2`
/// is a directory, so the walk used to commit to a wrong split of
/// `royalti / server / v2 / 6`. Lookahead joins several tokens with a uniform
/// separator and probes that, longest first.
///
/// Mixed separators inside one component (`royalti-server-v2.6`) remain
/// unresolved: that needs a combinatorial search, and the ambiguity is genuine
/// since the slug alone cannot distinguish them. When nothing matches we keep
/// the previous behaviour and default the unknown tail to `dir_sep`, so every
/// verified prefix stays accurate.
fn walk_slug_tokens<F: Fn(&str) -> bool>(
    seed: String,
    tokens: &[&str],
    dir_sep: char,
    exists: &F,
) -> String {
    let mut acc = seed;
    let mut i = 0;

    while i < tokens.len() {
        let single: Vec<String> = std::iter::once(dir_sep)
            .chain(INNER_SEPARATORS)
            .map(|sep| format!("{}{}{}", acc, sep, tokens[i]))
            .collect();

        if let Some(hit) = single.iter().find(|p| exists(p)) {
            acc = hit.clone();
            i += 1;
            continue;
        }

        let max_k = (tokens.len() - i).min(MAX_LOOKAHEAD);
        let mut matched: Option<(String, usize)> = None;
        'lookahead: for k in (2..=max_k).rev() {
            for sep in INNER_SEPARATORS {
                let segment = tokens[i..i + k].join(&sep.to_string());
                let candidate = format!("{}{}{}", acc, dir_sep, segment);
                if exists(&candidate) {
                    matched = Some((candidate, k));
                    break 'lookahead;
                }
            }
        }

        if let Some((candidate, k)) = matched {
            acc = candidate;
            i += k;
            continue;
        }

        // Nothing exists — keep the canonical separator and move on.
        acc = single[0].clone();
        i += 1;
    }

    acc
}

/// Greedy existence-checked decoder. Returns `(path, verified)` where
/// `verified` is true iff `metadata(path)` succeeded.
///
/// Approach: tokenise on `-` after dropping the leading dash. Walk forward
/// building up a path; for each token decide whether to join with `/`,
/// `-`, `_`, or `.` based on which (if any) candidate currently exists on
/// disk. We always prefer the `/` candidate first — that's the canonical
/// Claude encoding. When no candidate exists we keep the partial-FS-aware
/// walk (every verified prefix stays accurate; only the unknown tail
/// defaults to `/`), because that's strictly more useful than discarding
/// the walk in favour of an all-slashes naive form.
pub fn decode_claude_slug_with_fs(slug: &str) -> (String, bool) {
    decode_claude_slug_with_probe(slug, |p| std::path::Path::new(p).exists())
}

/// Test-seam over `decode_claude_slug_with_fs`. The probe closure stands
/// in for the real filesystem so unit tests can assert the greedy walk
/// against a fixture set without touching `~/`.
pub fn decode_claude_slug_with_probe<F: Fn(&str) -> bool>(slug: &str, exists: F) -> (String, bool) {
    // 1. Windows drive slug: e.g. "C--Users-nedJamez-..." or "C-Users-..."
    let is_win_drive = slug.len() >= 3
        && slug
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic())
            .unwrap_or(false)
        && (slug[1..].starts_with("--")
            || slug[1..].starts_with(":-")
            || slug[1..].starts_with('-'));

    if is_win_drive {
        let drive = &slug[0..1];
        let body = if slug[1..].starts_with("--") || slug[1..].starts_with(":-") {
            &slug[3..]
        } else {
            &slug[2..]
        };
        let tokens: Vec<&str> = body.split('-').collect();
        if tokens.is_empty() {
            let root = format!("{}:\\", drive.to_ascii_uppercase());
            return (root.clone(), exists(&root));
        }

        // Seed with the drive letter, e.g. `C:\Users`.
        let seed = format!("{}:\\{}", drive.to_ascii_uppercase(), tokens[0]);
        let acc = walk_slug_tokens(seed, &tokens[1..], '\\', &exists);
        let verified = exists(&acc);
        return (acc, verified);
    }

    // 2. Unix slug starting with '-'
    if slug.starts_with('-') {
        let body = &slug[1..];
        let tokens: Vec<&str> = body.split('-').collect();
        if tokens.is_empty() {
            return ("/".to_string(), exists("/"));
        }

        // Seed: leading `/<first-token>`. We don't FS-check this — the user's
        // FS root almost certainly contains it (`/Users`, `/home`, etc.).
        let seed = format!("/{}", tokens[0]);
        let acc = walk_slug_tokens(seed, &tokens[1..], '/', &exists);
        let verified = exists(&acc);
        return (acc, verified);
    }

    (slug.to_string(), exists(slug))
}

fn contract_home(path: &str, home: &std::path::Path) -> String {
    let home_str = match home.to_str() {
        Some(s) => s,
        None => return path.to_string(),
    };
    if let Some(rest) = path.strip_prefix(home_str) {
        return format!("~{}", rest.replace('\\', "/"));
    }
    // Also check with normalized slashes and case-insensitivity on Windows
    let norm_path = path.replace('\\', "/");
    let norm_home = home_str.replace('\\', "/");
    if norm_path
        .to_lowercase()
        .starts_with(&norm_home.to_lowercase())
    {
        let rest = &norm_path[norm_home.len()..];
        return format!("~{}", rest);
    }
    path.to_string()
}
