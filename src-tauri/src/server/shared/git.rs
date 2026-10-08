//! The `{{branch}}` action variable (G-ACTIONS §8.2): the branch checked out
//! at a work tree, read from `.git/HEAD` — no subprocess. Shared by the
//! desktop's `action_git_branch` (`commands::action_exec`, which passes a
//! reader that admits everything, exactly as it always read) and the daemon's
//! arm (WP-19 slice 6, which admits only paths inside its fs allowlist and
//! outside its own state).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The branch checked out at `root`, read from `.git/HEAD` (no subprocess).
/// `None` when `root` is not a git work tree or HEAD is detached.
pub fn git_branch_at(root: &Path) -> Option<String> {
    git_branch_at_with(root, &|_| true)
}

/// [`git_branch_at`], consulting `may_read` before touching `<root>/.git`
/// and before reading the `HEAD` it resolves to (which, for a worktree or
/// submodule, is wherever the `.git` file's `gitdir:` line points — a path the
/// caller did not name). A path `may_read` refuses reads as `None`: no branch.
pub fn git_branch_at_with(root: &Path, may_read: &dyn Fn(&Path) -> bool) -> Option<String> {
    let dot_git = root.join(".git");
    if !may_read(&dot_git) {
        return None;
    }
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        // A worktree / submodule: `.git` is a file `gitdir: <path>`.
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?
            .trim();
        let path = PathBuf::from(target);
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let head_path = git_dir.join("HEAD");
    if !may_read(&head_path) {
        return None;
    }
    let head = std::fs::read_to_string(head_path).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

/// 1 MiB cap on `git status` output.
pub const GIT_STATUS_OUTPUT_CAP: u64 = 1024 * 1024;
/// 5-second timeout for `git status`.
pub const GIT_STATUS_TIMEOUT_SECS: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusResult {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub detached: bool,
    pub ahead: u32,
    pub behind: u32,
    pub staged: Vec<GitFileChange>,
    pub unstaged: Vec<GitFileChange>,
    pub untracked: Vec<GitFileChange>,
    pub conflicted: Vec<GitFileChange>,
    pub modified: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
}

/// Strip wrapping quotes if git quoted a path with spaces or special chars.
fn unquote_path(p: &str) -> String {
    let p = p.trim();
    if p.starts_with('"') && p.ends_with('"') && p.len() >= 2 {
        &p[1..p.len() - 1]
    } else {
        p
    }
    .to_string()
}

/// Parse stdout of `git status --porcelain=v2 --branch`.
pub fn parse_porcelain_v2(stdout: &str) -> GitStatusResult {
    let mut branch_head: Option<String> = None;
    let mut head_oid: Option<String> = None;
    let mut ahead: u32 = 0;
    let mut behind: u32 = 0;
    let mut detached = false;

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicted = Vec::new();

    for line in stdout.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some(head) = rest.strip_prefix("branch.head ") {
                let head = head.trim();
                if head == "(detached)" || head == "(none)" {
                    detached = true;
                } else {
                    branch_head = Some(head.to_string());
                }
            } else if let Some(oid) = rest.strip_prefix("branch.oid ") {
                let oid = oid.trim();
                if oid != "(initial)" && !oid.is_empty() {
                    head_oid = Some(oid.to_string());
                }
            } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                for part in ab.split_whitespace() {
                    if let Some(a) = part.strip_prefix('+') {
                        ahead = a.parse().unwrap_or(0);
                    } else if let Some(b) = part.strip_prefix('-') {
                        behind = b.parse().unwrap_or(0);
                    }
                }
            }
        } else if let Some(path) = line.strip_prefix("? ") {
            untracked.push(GitFileChange {
                path: unquote_path(path),
            });
        } else if let Some(rest) = line.strip_prefix("u ") {
            // Unmerged: u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>
            let parts: Vec<&str> = rest.splitn(10, ' ').collect();
            if parts.len() == 10 {
                conflicted.push(GitFileChange {
                    path: unquote_path(parts[9]),
                });
            }
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // Ordinary change: 1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>
            let parts: Vec<&str> = rest.splitn(8, ' ').collect();
            if parts.len() == 8 {
                let xy = parts[0];
                let path_clean = unquote_path(parts[7]);
                let mut chars = xy.chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                if x != '.' {
                    staged.push(GitFileChange {
                        path: path_clean.clone(),
                    });
                }
                if y != '.' {
                    unstaged.push(GitFileChange { path: path_clean });
                }
            }
        } else if let Some(rest) = line.strip_prefix("2 ") {
            // Renamed/copied: 2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path><TAB><origPath>
            let parts: Vec<&str> = rest.splitn(9, ' ').collect();
            if parts.len() == 9 {
                let xy = parts[0];
                let path_tab = parts[8];
                let path = path_tab.split('\t').next().unwrap_or(path_tab);
                let path_clean = unquote_path(path);
                let mut chars = xy.chars();
                let x = chars.next().unwrap_or('.');
                let y = chars.next().unwrap_or('.');
                if x != '.' {
                    staged.push(GitFileChange {
                        path: path_clean.clone(),
                    });
                }
                if y != '.' {
                    unstaged.push(GitFileChange { path: path_clean });
                }
            }
        }
    }

    let effective_branch = if detached {
        head_oid.as_ref().map(|s| s.chars().take(7).collect())
    } else {
        branch_head
    };
    let modified = staged.len() + unstaged.len() + untracked.len() + conflicted.len();

    GitStatusResult {
        branch: effective_branch,
        head_sha: head_oid,
        detached,
        ahead,
        behind,
        staged,
        unstaged,
        untracked,
        conflicted,
        modified,
    }
}

/// Run a hardened `git status --porcelain=v2 --branch` confined to `root`.
/// Returns `Ok(None)` if `root` is not a git repository.
pub async fn run_hardened_git_status(root: &Path) -> Result<Option<GitStatusResult>, String> {
    use crate::executor::{PipedOpts, SpawnSpec, StdioMode};
    use tokio::io::AsyncReadExt as _;

    let mut spec = SpawnSpec::new("git");
    spec.current_dir(root);
    spec.env("GIT_CONFIG_NOSYSTEM", "1");
    spec.env("GIT_CONFIG_GLOBAL", "/dev/null");
    spec.env("GIT_OPTIONAL_LOCKS", "0");
    spec.env("PAGER", "cat");
    spec.env("GIT_PAGER", "cat");

    spec.args([
        "--no-pager",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.quotepath=false",
        "-c",
        "diff.external=",
        "-c",
        "diff.textconv=false",
        "-c",
        &format!("safe.directory={}", root.to_string_lossy()),
        "status",
        "--porcelain=v2",
        "--branch",
    ]);

    let opts = PipedOpts {
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        kill_on_drop: true,
        no_console_window: true,
        detached: false,
        new_process_group: false,
    };

    let mut child = crate::executor::current()
        .spawn_piped(spec, opts)
        .map_err(|e| format!("spawn git: {e}"))?;

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();

    let read_task = async {
        let stdout_fut = async {
            if let Some(mut out) = child.stdout.take() {
                let mut limited = (&mut out).take(GIT_STATUS_OUTPUT_CAP);
                let _ = limited.read_to_end(&mut stdout_bytes).await;
            }
        };
        let stderr_fut = async {
            if let Some(mut err) = child.stderr.take() {
                let mut limited = (&mut err).take(64 * 1024);
                let _ = limited.read_to_end(&mut stderr_bytes).await;
            }
        };
        tokio::join!(stdout_fut, stderr_fut);
        child.wait().await
    };

    let timeout_dur = Duration::from_secs(GIT_STATUS_TIMEOUT_SECS);
    let status = match tokio::time::timeout(timeout_dur, read_task).await {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(format!("git wait: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "git status timed out after {GIT_STATUS_TIMEOUT_SECS}s"
            ));
        }
    };

    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr_bytes);
        if stderr_text.contains("not a git repository") || status.code() == Some(128) {
            return Ok(None);
        }
        return Err(format!("git status failed: {}", stderr_text.trim()));
    }

    let stdout_text = String::from_utf8_lossy(&stdout_bytes);
    Ok(Some(parse_porcelain_v2(&stdout_text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_v2_normal_branch_with_changes() {
        let sample = "\
# branch.oid 0123456789abcdef0123456789abcdef01234567
# branch.head feat/test
# branch.upstream origin/main
# branch.ab +3 -1
1 .M N... 100644 100644 100644 h1 h2 unstaged.txt
1 M. N... 100644 100644 100644 h1 h2 staged.txt
1 MM N... 100644 100644 100644 h1 h2 both.txt
2 R. N... 100644 100644 100644 h1 h2 R100 renamed.txt\told.txt
? untracked.txt
u UU N... 100644 100644 100644 100644 h1 h2 h3 conflict.txt
";
        let res = parse_porcelain_v2(sample);
        assert_eq!(res.branch.as_deref(), Some("feat/test"));
        assert_eq!(
            res.head_sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(!res.detached);
        assert_eq!(res.ahead, 3);
        assert_eq!(res.behind, 1);
        assert_eq!(
            res.staged,
            vec![
                GitFileChange {
                    path: "staged.txt".into()
                },
                GitFileChange {
                    path: "both.txt".into()
                },
                GitFileChange {
                    path: "renamed.txt".into()
                },
            ]
        );
        assert_eq!(
            res.unstaged,
            vec![
                GitFileChange {
                    path: "unstaged.txt".into()
                },
                GitFileChange {
                    path: "both.txt".into()
                },
            ]
        );
        assert_eq!(
            res.untracked,
            vec![GitFileChange {
                path: "untracked.txt".into()
            }]
        );
        assert_eq!(
            res.conflicted,
            vec![GitFileChange {
                path: "conflict.txt".into()
            }]
        );
        assert_eq!(res.modified, 7);
    }

    #[test]
    fn parse_porcelain_v2_detached_head() {
        let sample = "\
# branch.oid abcdef1234567890abcdef1234567890abcdef12
# branch.head (detached)
# branch.ab +0 -0
";
        let res = parse_porcelain_v2(sample);
        assert_eq!(res.branch.as_deref(), Some("abcdef1"));
        assert!(res.detached);
        assert_eq!(res.modified, 0);
    }
}
