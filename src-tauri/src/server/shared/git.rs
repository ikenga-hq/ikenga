//! The `{{branch}}` action variable (G-ACTIONS §8.2): the branch checked out
//! at a work tree, read from `.git/HEAD` — no subprocess. Shared by the
//! desktop's `action_git_branch` (`commands::action_exec`, which passes a
//! reader that admits everything, exactly as it always read) and the daemon's
//! arm (WP-19 slice 6, which admits only paths inside its fs allowlist and
//! outside its own state).

use std::path::{Path, PathBuf};

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
