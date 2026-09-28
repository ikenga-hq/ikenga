//! Phase 6 — agent-config scaffolder.
//!
//! Walks the embedded starter templates (compiled in via `include_dir!`)
//! and writes them into the user's chosen project root under the provider's
//! conventional config dir (Claude Code → `.claude/`). Supports three
//! conflict modes that mirror APPROVAL.md's wizard step:
//!
//!   replace        — overwrite all existing files (caller is responsible
//!                    for taking a backup beforehand if desired).
//!   augment        — write only files that don't already exist (the
//!                    "merge / add missing" mode — the default).
//!   skip_conflicts — same as augment but the result lists every
//!                    conflict-skipped path so the wizard can show counts.
//!
//! Provider abstraction lives in the TS layer (`src/lib/onboarding/
//! agent-config-providers/`) since the wizard UI is provider-agnostic.
//! The Rust side just dispatches by `provider_id`. v1 ships `claude-code`
//! only; Codex / Gemini / Cursor providers can drop in their own
//! template tree + match arm later.
//!
//! **Where this lives.** The body of `scaffold_agent_config`, shared by the
//! desktop command (`agent_detect::scaffold_agent_config`, which calls
//! [`scaffold`] — [`Reach::Follow`], unchanged behaviour and strings) and the
//! daemon's `/api/rpc` arm (`server::rpc_claude`, WP-19 slice 8), which
//! calls [`scaffold_in`] with a canonical, `PathGuard`-admitted root and
//! [`Reach::Confined`]: a symlink at `<root>/.claude` refuses the whole call
//! before anything is written, and every directory and file below it goes
//! through `confined_fs` — no link, live or dangling, is followed, and every
//! node is checked against the guard again. `agent_detect::scaffold`
//! re-exports this module, so every existing path resolves.

use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};
use serde::{Deserialize, Serialize};

pub use super::confined_fs::Reach;

/// Source of truth for the starter templates. Files under
/// `src-tauri/templates/starter/claude-code/` are baked into the release
/// binary at compile time so the scaffolder works offline and doesn't
/// require cc-config to be installed alongside the shell.
static CLAUDE_CODE_STARTER: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/templates/starter/claude-code");

#[derive(Debug, Deserialize)]
pub struct ScaffoldRequest {
    pub provider: String,
    pub root_path: String,
    pub profile: String,
    /// One of `replace` | `augment` | `skip_conflicts`. Optional for
    /// backwards compatibility with the Phase 4 stub which didn't take a
    /// mode (it falls back to `augment`).
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ScaffoldFileResult {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ScaffoldResponse {
    pub ok: bool,
    pub files_written: u32,
    pub message: String,
    pub written: Vec<String>,
    pub skipped: Vec<ScaffoldFileResult>,
    pub errors: Vec<ScaffoldFileResult>,
}

/// Mode parsed from the request string. Defaults to `Augment` when absent
/// or unrecognised — the safest choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldMode {
    Replace,
    Augment,
    SkipConflicts,
}

impl ScaffoldMode {
    fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("augment") {
            "replace" => Self::Replace,
            "skip_conflicts" => Self::SkipConflicts,
            _ => Self::Augment,
        }
    }
}

/// Entry point invoked by the Tauri command. Resolves the template tree
/// for `provider`, then walks it into `<root_path>/<config_dir>`.
pub fn scaffold(req: ScaffoldRequest) -> Result<ScaffoldResponse, String> {
    scaffold_in(req, Reach::Follow)
}

/// [`scaffold`] with the reach explicit. The daemon passes
/// [`Reach::Confined`] and a `root_path` it has already canonicalized and
/// checked against its `PathGuard`.
pub fn scaffold_in(req: ScaffoldRequest, reach: Reach<'_>) -> Result<ScaffoldResponse, String> {
    let mode = ScaffoldMode::parse(req.mode.as_deref());
    match req.provider.as_str() {
        "claude-code" => scaffold_claude_code(&req.root_path, &req.profile, mode, reach),
        other => Err(format!("unsupported provider: {other}")),
    }
}

fn scaffold_claude_code(
    root_path: &str,
    profile: &str,
    mode: ScaffoldMode,
    reach: Reach<'_>,
) -> Result<ScaffoldResponse, String> {
    // v1 only ships the `starter` profile. Future profiles (music-label,
    // studio) will pick a different sub-dir under `templates/starter/`.
    if profile != "starter" {
        return Err(format!(
            "unknown profile '{profile}' for claude-code provider (expected 'starter')"
        ));
    }

    let root = PathBuf::from(root_path);
    if !root.is_dir() {
        return Err(format!(
            "root_path does not exist or is not a directory: {root_path}"
        ));
    }

    let target = root.join(".claude");
    if let Reach::Confined(_) = reach {
        // The whole tree lands under `.claude`: a link there is refused
        // outright, before anything is created (as `project_scaffold_claude`
        // refuses one), rather than reported file by file.
        if std::fs::symlink_metadata(&target)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(format!(
                "refusing to follow a symlink out of the project: {}",
                target.display()
            ));
        }
    }
    write_dir(&CLAUDE_CODE_STARTER, &target, mode, reach)
}

/// Recursively writes `src` into `dest` honouring the conflict mode.
/// Tracks per-file outcomes for the response.
fn write_dir(
    src: &Dir<'_>,
    dest: &Path,
    mode: ScaffoldMode,
    reach: Reach<'_>,
) -> Result<ScaffoldResponse, String> {
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<ScaffoldFileResult> = Vec::new();
    let mut errors: Vec<ScaffoldFileResult> = Vec::new();

    walk(
        src,
        dest,
        mode,
        reach,
        &mut written,
        &mut skipped,
        &mut errors,
    );

    let files_written = u32::try_from(written.len()).unwrap_or(u32::MAX);
    let message = if errors.is_empty() {
        format!(
            "wrote {} file(s); skipped {} existing",
            written.len(),
            skipped.len()
        )
    } else {
        format!(
            "wrote {} file(s); skipped {}; {} error(s) — see errors[]",
            written.len(),
            skipped.len(),
            errors.len()
        )
    };

    Ok(ScaffoldResponse {
        ok: errors.is_empty(),
        files_written,
        message,
        written,
        skipped,
        errors,
    })
}

fn walk(
    src: &Dir<'_>,
    dest: &Path,
    mode: ScaffoldMode,
    reach: Reach<'_>,
    written: &mut Vec<String>,
    skipped: &mut Vec<ScaffoldFileResult>,
    errors: &mut Vec<ScaffoldFileResult>,
) {
    // Ensure dest dir exists. Failure here is fatal for the whole subtree
    // — record it and bail on this branch rather than continuing into
    // children that would all fail.
    if let Err(e) = reach.create_dir_all(dest) {
        errors.push(ScaffoldFileResult {
            path: dest.display().to_string(),
            reason: format!("create_dir_all: {e}"),
        });
        return;
    }

    // Files first, then recurse into subdirs. Order doesn't matter for
    // correctness but keeps test assertions stable.
    for file in src.files() {
        let rel = file.path();
        // `file.path()` is relative to the include_dir root. The first
        // path component is the embedded root dir name; strip nothing —
        // include_dir already gives us paths relative to the embed root.
        let target_path = dest.join(rel.file_name().expect("file has name"));
        // Re-derive the full relative path within the embed for nested
        // files (skills/foo/SKILL.md). include_dir's File::path() returns
        // the path relative to the root of the embed.
        let nested_target = dest.join(rel);

        // Use nested_target so subdir structure is preserved.
        let target = if rel
            .parent()
            .map(|p| p.as_os_str().is_empty())
            .unwrap_or(true)
        {
            target_path
        } else {
            nested_target
        };

        let exists = reach.exists(&target);
        match (mode, exists) {
            (ScaffoldMode::Augment, true) | (ScaffoldMode::SkipConflicts, true) => {
                skipped.push(ScaffoldFileResult {
                    path: rel.display().to_string(),
                    reason: "exists".to_string(),
                });
                continue;
            }
            _ => {}
        }

        // Make sure the parent dir exists for nested files like
        // skills/foo/SKILL.md.
        if let Some(parent) = target.parent() {
            if let Err(e) = reach.create_dir_all(parent) {
                errors.push(ScaffoldFileResult {
                    path: rel.display().to_string(),
                    reason: format!("create_dir_all parent: {e}"),
                });
                continue;
            }
        }

        match reach.write(&target, file.contents()) {
            Ok(()) => written.push(rel.display().to_string()),
            Err(e) => errors.push(ScaffoldFileResult {
                path: rel.display().to_string(),
                reason: format!("write: {e}"),
            }),
        }
    }

    for dir in src.dirs() {
        // For nested dirs, dest stays the same — file paths are already
        // relative to the embed root, so we don't need to re-join the dir
        // name here. Recursing once with the original `dest` would
        // duplicate dir names. Iterate dir contents but keep dest pointed
        // at the same root.
        walk(dir, dest, mode, reach, written, skipped, errors);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_files(root: &Path) -> usize {
        let mut n = 0;
        fn rec(p: &Path, n: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        rec(&path, n);
                    } else if path.is_file() {
                        *n += 1;
                    }
                }
            }
        }
        rec(root, &mut n);
        n
    }

    #[test]
    fn embedded_templates_present() {
        // Sanity: include_dir picked up something. If this fails the build
        // path is wrong.
        assert!(
            CLAUDE_CODE_STARTER.entries().len() > 0,
            "embedded template dir is empty — include_dir path is wrong"
        );
    }

    #[test]
    fn scaffold_starter_into_fresh_tmpdir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let req = ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "starter".into(),
            mode: Some("augment".into()),
        };
        let resp = scaffold(req).expect("scaffold ok");
        assert!(resp.ok);
        assert!(resp.files_written > 0, "wrote at least one file");
        assert!(resp.skipped.is_empty(), "nothing to skip in fresh dir");
        assert!(tmp.path().join(".claude").is_dir());
        // Spot-check a known file.
        assert!(
            tmp.path()
                .join(".claude/agents/release-coordinator.md")
                .is_file(),
            "release-coordinator agent should be present"
        );
        assert!(
            tmp.path()
                .join(".claude/skills/release-planner/SKILL.md")
                .is_file(),
            "release-planner skill should be present"
        );
        assert!(
            tmp.path().join(".claude/commands/release.md").is_file(),
            "release command should be present"
        );
    }

    #[test]
    fn augment_skips_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // First pass — populate.
        scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "starter".into(),
            mode: Some("augment".into()),
        })
        .unwrap();

        let total = count_files(&tmp.path().join(".claude"));

        // Mutate a known file so we can prove augment doesn't clobber it.
        let marker = tmp.path().join(".claude/agents/release-coordinator.md");
        std::fs::write(&marker, "USER_EDITED").unwrap();

        // Second pass — everything is already there, so every file should
        // be skipped.
        let resp = scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "starter".into(),
            mode: Some("augment".into()),
        })
        .unwrap();
        assert_eq!(resp.files_written, 0);
        assert_eq!(
            resp.skipped.len(),
            total,
            "every embedded file should have been skipped"
        );
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "USER_EDITED");
    }

    #[test]
    fn replace_overwrites_existing_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Pre-populate with a stub.
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        let marker = agents.join("release-coordinator.md");
        std::fs::write(&marker, "OLD_STUB").unwrap();

        let resp = scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "starter".into(),
            mode: Some("replace".into()),
        })
        .unwrap();
        assert!(resp.ok);
        assert!(resp.files_written > 0);
        let after = std::fs::read_to_string(&marker).unwrap();
        assert_ne!(after, "OLD_STUB", "replace mode must overwrite");
        assert!(after.contains("Release Coordinator"));
    }

    #[test]
    fn skip_conflicts_records_conflicts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agents = tmp.path().join(".claude/agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("release-coordinator.md"), "OLD").unwrap();

        let resp = scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "starter".into(),
            mode: Some("skip_conflicts".into()),
        })
        .unwrap();
        assert!(resp.ok);
        assert!(
            resp.skipped
                .iter()
                .any(|f| f.path.ends_with("release-coordinator.md")),
            "conflict must appear in skipped[]"
        );
        // The non-conflicting files should have been written.
        assert!(resp.files_written > 0);
        // Untouched original.
        assert_eq!(
            std::fs::read_to_string(agents.join("release-coordinator.md")).unwrap(),
            "OLD"
        );
    }

    #[test]
    fn unknown_provider_errors() {
        let resp = scaffold(ScaffoldRequest {
            provider: "codex".into(),
            root_path: "/tmp".into(),
            profile: "starter".into(),
            mode: None,
        });
        assert!(resp.is_err());
    }

    #[test]
    fn unknown_profile_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resp = scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: tmp.path().to_string_lossy().to_string(),
            profile: "music-label".into(),
            mode: None,
        });
        assert!(resp.is_err());
    }

    #[test]
    fn missing_root_errors() {
        let resp = scaffold(ScaffoldRequest {
            provider: "claude-code".into(),
            root_path: "/definitely/not/a/dir/ikenga-scaffold-test".into(),
            profile: "starter".into(),
            mode: None,
        });
        assert!(resp.is_err());
    }
}
