//! First-run wizard discovery: system + agent + agent-config inventory.
//!
//! All three Tauri commands are async (the agent scan runs subprocesses)
//! and return rich JSON-serializable structs the wizard renders verbatim.

pub mod agents;
pub mod config_claude;
pub mod known;
pub mod scaffold;
pub mod system;

use std::path::PathBuf;

use serde::Serialize;
use tauri::Manager;

pub use agents::DetectedAgent;
pub use config_claude::AgentConfigInventory;
pub use scaffold::{ScaffoldRequest, ScaffoldResponse};
pub use system::SystemReport;

#[tauri::command]
pub async fn detect_system(app: tauri::AppHandle) -> Result<SystemReport, String> {
    let dir: PathBuf = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    // build_report does only sync work (no subprocesses). Stay on the
    // current thread — `spawn_blocking` would be overkill.
    Ok(system::build_report(dir))
}

#[tauri::command]
pub async fn detect_agents() -> Result<Vec<DetectedAgent>, String> {
    Ok(agents::detect_all().await)
}

#[tauri::command]
pub async fn detect_agent(agent_id: String) -> Result<Option<DetectedAgent>, String> {
    Ok(agents::detect_by_id(&agent_id).await)
}

#[tauri::command]
pub async fn detect_agent_config(
    agent_id: String,
    root_path: String,
) -> Result<AgentConfigInventory, String> {
    Ok(config_claude::build_inventory(&agent_id, &root_path))
}

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

/// Scan `~/.claude/projects/` (and WSL on Windows) for project session directories.
#[tauri::command]
pub async fn list_claude_projects() -> Result<Vec<ClaudeProjectEntry>, String> {
    let home = match crate::platform::home_dir() {
        Some(h) => h,
        None => return Ok(Vec::new()),
    };
    let mut out: Vec<ClaudeProjectEntry> = Vec::new();

    // 1. Host user home `.claude/projects`
    let host_projects = home.join(".claude").join("projects");
    scan_claude_project_directory(&host_projects, &home, &mut out);

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
    Ok(out)
}

/// Generic project/conversation history lister across supported AI agents.
#[tauri::command]
pub async fn list_agent_projects(agent_id: String) -> Result<Vec<ClaudeProjectEntry>, String> {
    match agent_id.as_str() {
        "antigravity-cli" | "antigravity" | "gemini-cli" | "gemini" => {
            let home = match crate::platform::home_dir() {
                Some(h) => h,
                None => return Ok(Vec::new()),
            };
            let mut out: Vec<ClaudeProjectEntry> = Vec::new();
            let brain_dir = home.join(".gemini").join("antigravity").join("brain");
            if let Ok(rd) = std::fs::read_dir(&brain_dir) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if !p.is_dir() {
                        continue;
                    }
                    let slug = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
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
            Ok(out)
        }
        "codex" | "chatgpt" | "openai" => {
            let home = match crate::platform::home_dir() {
                Some(h) => h,
                None => return Ok(Vec::new()),
            };
            let mut out: Vec<ClaudeProjectEntry> = Vec::new();
            let codex_sessions = home.join(".codex").join("sessions");
            if let Ok(rd) = std::fs::read_dir(&codex_sessions) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    let slug = p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
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
            Ok(out)
        }
        _ => list_claude_projects().await,
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
        && slug.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && (slug[1..].starts_with("--") || slug[1..].starts_with(":-") || slug[1..].starts_with('-'));

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
        let mut acc = format!("/{}", tokens[0]);

        let acc = walk_slug_tokens(acc, &tokens[1..], '/', &exists);
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
    if norm_path.to_lowercase().starts_with(&norm_home.to_lowercase()) {
        let rest = &norm_path[norm_home.len()..];
        return format!("~{}", rest);
    }
    path.to_string()
}

/// Phase 6 — agent-config scaffolder. Lays down the starter set of
/// agents/skills/commands for `provider` under `<root_path>/.claude/` (or
/// the provider's equivalent config dir). `mode` selects conflict
/// behaviour: `augment` (default, only writes missing files), `replace`
/// (overwrites everything), or `skip_conflicts` (same as augment but the
/// response records each skipped path so the wizard can show counts).
///
/// Backwards-compatible with the Phase 4 wrapper signature — it didn't
/// pass `mode`, so an absent value falls back to `augment` inside
/// `scaffold::scaffold`.
#[tauri::command]
pub async fn scaffold_agent_config(
    provider: String,
    root_path: String,
    profile: String,
    mode: Option<String>,
) -> Result<ScaffoldResponse, String> {
    scaffold::scaffold(ScaffoldRequest {
        provider,
        root_path,
        profile,
        mode,
    })
}

#[cfg(test)]
mod claude_slug_tests {
    use super::*;
    use std::collections::HashSet;

    fn probe<'a>(set: &'a HashSet<&'static str>) -> impl Fn(&str) -> bool + 'a {
        move |p| set.contains(p)
    }

    #[test]
    fn naive_decoder_replaces_all_dashes() {
        // No FS context — pure transform.
        assert_eq!(
            decode_claude_slug_naive("-Users-alice-work-stuff-proj"),
            "/Users/alice/work/stuff/proj"
        );
        assert_eq!(decode_claude_slug_naive("plain"), "plain");
    }

    #[test]
    fn greedy_decoder_keeps_hyphenated_component_when_disk_says_so() {
        // Hypothetical disk: `/Users/alice/work-stuff/proj` exists, but
        // `/Users/alice/work` does not. The greedy walk should prefer the
        // dash join at the `work` → `stuff` boundary.
        let set: HashSet<&'static str> = [
            "/Users/alice",
            "/Users/alice/work-stuff",
            "/Users/alice/work-stuff/proj",
        ]
        .into_iter()
        .collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-alice-work-stuff-proj", probe(&set));
        assert_eq!(path, "/Users/alice/work-stuff/proj");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_returns_canonical_slashed_form_when_nothing_exists() {
        // No FS info available. Walk defaults to '/' joins for every
        // unknown boundary — same shape as the old naive fallback but
        // produced by the walk itself.
        let set: HashSet<&'static str> = HashSet::new();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-alice-work-stuff-proj", probe(&set));
        assert_eq!(path, "/Users/alice/work/stuff/proj");
        assert!(!verified);
    }

    #[test]
    fn greedy_decoder_preserves_verified_prefix_when_tail_missing() {
        // The regression case from the onboarding screenshot:
        // `~/royalti-co/royalti-client-2.5` doesn't exist on this machine,
        // but `~/royalti-co` does. We must preserve the dash boundary that
        // FS proved, instead of collapsing the whole path to slashes.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-co"].into_iter().collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-co-royalti-client-2-5", probe(&set));
        assert_eq!(path, "/home/x/royalti-co/royalti/client/2/5");
        assert!(!verified);
    }

    #[test]
    fn greedy_decoder_resolves_dot_separator() {
        // Claude Code encodes `.` as `-` in slugs, so `royalti-client-2.5`
        // becomes `-...-royalti-client-2-5`. The greedy walk must try
        // `2.5` as a candidate when the FS knows about it.
        let set: HashSet<&'static str> = [
            "/Users/alice",
            "/Users/alice/work",
            "/Users/alice/work/v2.5",
        ]
        .into_iter()
        .collect();
        let (path, verified) = decode_claude_slug_with_probe("-Users-alice-work-v2-5", probe(&set));
        assert_eq!(path, "/Users/alice/work/v2.5");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_resolves_underscore_separator() {
        // Underscores in original paths get encoded to '-' too. The walk
        // tries '_' once '/' and '-' both fail.
        let set: HashSet<&'static str> = ["/Users/alice", "/Users/alice/my_proj"]
            .into_iter()
            .collect();
        let (path, verified) = decode_claude_slug_with_probe("-Users-alice-my-proj", probe(&set));
        assert_eq!(path, "/Users/alice/my_proj");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_handles_canonical_slash_path() {
        // Every prefix exists with slashes — should hand back the
        // canonical slashed form verbatim.
        let set: HashSet<&'static str> = [
            "/Users",
            "/Users/iyke",
            "/Users/iyke/projects",
            "/Users/iyke/projects/foo",
        ]
        .into_iter()
        .collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-iyke-projects-foo", probe(&set));
        assert_eq!(path, "/Users/iyke/projects/foo");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_prefers_slash_when_both_candidates_exist() {
        // Edge case: both `/a/b` and `/a-b` exist. Slash wins (canonical
        // Claude encoding) so the user lands on the more common case.
        let set: HashSet<&'static str> = ["/a", "/a/b", "/a-b"].into_iter().collect();
        let (path, _) = decode_claude_slug_with_probe("-a-b", probe(&set));
        assert_eq!(path, "/a/b");
    }

    #[test]
    fn greedy_decoder_handles_windows_drive_slugs() {
        let set: HashSet<&'static str> = [
            "C:\\Users",
            "C:\\Users\\nedJamez",
            "C:\\Users\\nedJamez\\Documents",
            "C:\\Users\\nedJamez\\Documents\\royalti-co",
            "C:\\Users\\nedJamez\\Documents\\royalti-co\\royalti-server-v2-6",
        ]
        .into_iter()
        .collect();

        let (path, verified) = decode_claude_slug_with_probe(
            "C--Users-nedJamez-Documents-royalti-co-royalti-server-v2-6",
            probe(&set),
        );
        assert_eq!(
            path,
            "C:\\Users\\nedJamez\\Documents\\royalti-co\\royalti-server-v2-6"
        );
        assert!(verified);
    }
    #[test]
    fn greedy_decoder_absorbs_a_multi_separator_component() {
        // The component itself contains separators the slug flattened, so no
        // prefix of it exists on disk. Stepping one token at a time can never
        // find it; only lookahead over the whole run does.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-co", "/home/x/royalti-co/api-server-v2-6"]
            .into_iter()
            .collect();

        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-co-api-server-v2-6", probe(&set));
        assert_eq!(path, "/home/x/royalti-co/api-server-v2-6");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_prefers_the_longest_component_that_exists() {
        // Both `a-b` and `a-b-c` exist. Longest-first must win, otherwise the
        // walk stops at `a-b` and splits the rest.
        let set: HashSet<&'static str> = ["/r", "/r/a-b", "/r/a-b-c"].into_iter().collect();

        let (path, verified) = decode_claude_slug_with_probe("-r-a-b-c", probe(&set));
        assert_eq!(path, "/r/a-b-c");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_leaves_mixed_separator_components_unresolved() {
        // Documented limitation: a component mixing `-` and `.` (a real case is
        // `royalti-server-v2.6`) needs a combinatorial search, and the slug
        // alone cannot disambiguate it. The verified prefix is still kept and
        // only the unknown tail defaults to slashes.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-server-v2.6"]
            .into_iter()
            .collect();

        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-server-v2-6", probe(&set));
        assert_eq!(path, "/home/x/royalti/server/v2/6");
        assert!(!verified);
    }

}
