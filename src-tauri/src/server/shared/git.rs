//! The `{{branch}}` action variable (G-ACTIONS §8.2): the branch checked out
//! at a work tree, read from `.git/HEAD` — no subprocess. Shared by the
//! desktop's `action_git_branch` (`commands::action_exec`, which passes a
//! reader that admits everything, exactly as it always read) and the daemon's
//! arm (WP-19 slice 6, which admits only paths inside its fs allowlist and
//! outside its own state).

use std::ffi::OsStr;
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

/// Where `root`'s git directory is, or `None` when `root` is not a work tree
/// root. `root/.git` must be a real directory (pinned as `GIT_DIR`) or a
/// `gitdir:` file (worktree / submodule) whose target `may_read` admits. A
/// symlinked `.git` is refused: it would read some other repository's index
/// through a project the caller may read. Git is never left to *discover* a
/// repository above `root`, so a project that is a subdirectory of a larger
/// repo reads as "not a repository" instead of listing paths outside itself.
pub fn resolve_git_dir(
    root: &Path,
    may_read: &dyn Fn(&Path) -> bool,
) -> Result<Option<PathBuf>, String> {
    let dot_git = root.join(".git");
    let meta = match std::fs::symlink_metadata(&dot_git) {
        Ok(m) => m,
        Err(_) => return Ok(None),
    };
    if meta.file_type().is_symlink() {
        return Err("`.git` is a symlink; refusing to read it".to_string());
    }
    if meta.is_dir() {
        return Ok(Some(dot_git));
    }
    let text = read_small_text(&dot_git, 8192).map_err(|e| format!("read .git file: {e}"))?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .ok_or_else(|| "`.git` file has no `gitdir:` line".to_string())?
        .trim();
    let path = PathBuf::from(target);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    let canon = path
        .canonicalize()
        .map_err(|e| format!("`.git` gitdir target is unreadable: {e}"))?;
    if !may_read(&canon) {
        return Err("`.git` points outside the readable roots".to_string());
    }
    Ok(Some(canon))
}

/// What a hardened git run returned.
struct GitOut {
    success: bool,
    stdout: Vec<u8>,
    stderr: String,
}

/// The `-c` pairs and env every hardened invocation shares. `git_dir` is the
/// private sandbox git directory (see [`GitSandbox`]), never the repository's
/// own: the repo's config is not read at all.
fn hardened_spec(root: &Path, git_dir: &Path, tail: &[&str]) -> crate::executor::SpawnSpec {
    use crate::executor::SpawnSpec;
    let mut spec = SpawnSpec::new("git");
    spec.current_dir(root);
    spec.env("GIT_CONFIG_NOSYSTEM", "1");
    spec.env("GIT_CONFIG_GLOBAL", "/dev/null");
    spec.env("GIT_OPTIONAL_LOCKS", "0");
    spec.env("GIT_TERMINAL_PROMPT", "0");
    spec.env("PAGER", "cat");
    spec.env("GIT_PAGER", "cat");
    // Pin the repository and the work tree: no discovery above `root`, and no
    // `core.worktree` to move the work tree (the sandbox config has none, and
    // `GIT_WORK_TREE` wins over it regardless).
    spec.env("GIT_DIR", git_dir);
    spec.env("GIT_WORK_TREE", root);
    // Everything else that could redirect where git reads from, pinned to the
    // sandbox (or neutralised), whatever the daemon's own environment holds.
    spec.env("GIT_COMMON_DIR", git_dir);
    spec.env("GIT_INDEX_FILE", git_dir.join("index"));
    spec.env("GIT_OBJECT_DIRECTORY", git_dir.join("objects"));
    spec.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", "");
    spec.env("GIT_CONFIG_COUNT", "0");
    spec.env("GIT_CONFIG_PARAMETERS", "");
    // Stable English diagnostics: `is_not_a_repo` matches git's own message.
    spec.env("LC_ALL", "C");
    if let Some(parent) = root.parent() {
        spec.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let mut args: Vec<String> = vec![
        "--no-pager".into(),
        "--no-optional-locks".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.quotepath=false".into(),
        "-c".into(),
        "core.attributesFile=/dev/null".into(),
        "-c".into(),
        "core.untrackedCache=false".into(),
        "-c".into(),
        "diff.external=".into(),
        "-c".into(),
        "diff.textconv=false".into(),
        "-c".into(),
        format!("safe.directory={}", root.to_string_lossy()),
    ];
    args.extend(tail.iter().map(|s| s.to_string()));
    spec.args(args);
    spec
}

/// Run `spec` with a timeout and a stdout cap. Output past `cap` kills git and
/// is an error: a clipped status would read as a smaller, wrong one.
async fn run_capped(spec: crate::executor::SpawnSpec, cap: u64) -> Result<GitOut, String> {
    use crate::executor::{PipedOpts, StdioMode};
    use tokio::io::AsyncReadExt as _;

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
    let mut overflow = false;

    let read_task = async {
        let stdout_fut = async {
            if let Some(mut out) = child.stdout.take() {
                let mut limited = (&mut out).take(cap + 1);
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
        if stdout_bytes.len() as u64 > cap {
            overflow = true;
            let _ = child.start_kill();
        }
        child.wait().await
    };

    let status =
        match tokio::time::timeout(Duration::from_secs(GIT_STATUS_TIMEOUT_SECS), read_task).await {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => return Err(format!("git wait: {e}")),
            Err(_) => {
                let _ = child.start_kill();
                return Err(format!("git timed out after {GIT_STATUS_TIMEOUT_SECS}s"));
            }
        };
    if overflow {
        return Err(format!("git output exceeded {cap} bytes"));
    }
    Ok(GitOut {
        success: status.success(),
        stdout: stdout_bytes,
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
    })
}

/// Only git's own "not a repository" message means "not a repository". Any
/// other failure (a corrupt index, a missing object, a shallow repo we could
/// not describe...) is an error, never a quiet "no chip".
fn is_not_a_repo(out: &GitOut) -> bool {
    out.stderr.contains("not a git repository")
}

/// Read a small regular file without ever blocking on it: `O_NONBLOCK` (a FIFO
/// opens at once instead of waiting for a writer), `O_NOFOLLOW`, then `fstat`
/// on the open descriptor must say "regular file" and a size within `max`.
/// A repository is untrusted input; `.git/HEAD` as a FIFO must not park a
/// worker thread.
fn read_regular_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    read_regular_capped_mtime(path, max).map(|(b, _)| b)
}

/// [`read_regular_capped`] plus the file's mtime, taken from the open
/// descriptor BEFORE the bytes are read: if the file is rewritten in between,
/// the (older) mtime is the conservative one for git's racy-entry check.
fn read_regular_capped_mtime(
    path: &Path,
    max: u64,
) -> std::io::Result<(Vec<u8>, Option<std::time::SystemTime>)> {
    use std::io::Read as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = opts.open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    if meta.len() > max {
        return Err(std::io::Error::other(format!(
            "file is larger than {max} bytes"
        )));
    }
    let mtime = meta.modified().ok();
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(std::io::Error::other(format!(
            "file is larger than {max} bytes"
        )));
    }
    Ok((buf, mtime))
}

fn read_small_text(path: &Path, max: u64) -> std::io::Result<String> {
    String::from_utf8(read_regular_capped(path, max)?)
        .map_err(|_| std::io::Error::other("not valid UTF-8"))
}

/// Largest `index` / `sharedindex.*` read and copied (a clipped index would be
/// a wrong status).
const MAX_INDEX_BYTES: u64 = 128 * 1024 * 1024;
/// Directory entries visited while checking `refs/` and `objects/` for
/// symlinks. Past it the repository is "too large to verify", an error.
const MAX_SCAN_ENTRIES: usize = 1_000_000;

/// Refuse any symlink anywhere under `dir`. A symlink in `refs/` or `objects/`
/// lets a repository make git read a ref / pack / object from wherever the
/// serving principal can read, past the allowlist.
fn scan_no_symlinks(dir: &Path, budget: &mut usize, depth: u8) -> Result<(), String> {
    if depth > 64 {
        return Err("repository directories are nested too deeply to verify".to_string());
    }
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read repository directory: {e}"))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("read repository directory: {e}"))?;
        *budget = budget
            .checked_sub(1)
            .ok_or_else(|| "repository is too large to verify (run `git gc`)".to_string())?;
        let ft = entry
            .file_type()
            .map_err(|e| format!("read repository directory: {e}"))?;
        if ft.is_symlink() {
            return Err(format!(
                "`{}` is a symlink inside the repository; refusing to follow it",
                entry.file_name().to_string_lossy()
            ));
        }
        if ft.is_dir() {
            scan_no_symlinks(&entry.path(), budget, depth + 1)?;
        } else if !ft.is_file() {
            // A FIFO / socket / device: git would block on it until the
            // timeout. Refuse it up front instead of tying up a git process.
            return Err(format!(
                "`{}` is not a regular file inside the repository; refusing it",
                entry.file_name().to_string_lossy()
            ));
        }
    }
    Ok(())
}

fn is_fanout_dir(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `true` when an `info/alternates` body names at least one store.
fn names_alternates(body: &str) -> bool {
    body.lines().any(|l| {
        let l = l.trim();
        !l.is_empty() && !l.starts_with('#')
    })
}

/// A path git must never be told to `lstat`: absolute, or climbing out with `..`.
fn escapes_root(path: &[u8]) -> bool {
    path.first() == Some(&b'/') || path.split(|b| *b == b'/').any(|c| c == b"..")
}

/// Read a variable-length offset the way git's index v4 does.
fn index_varint(buf: &[u8], pos: &mut usize) -> Option<usize> {
    let mut c = *buf.get(*pos)?;
    *pos += 1;
    let mut val = (c & 127) as usize;
    while c & 128 != 0 {
        val = val.checked_add(1)?;
        c = *buf.get(*pos)?;
        *pos += 1;
        val = val.checked_mul(128)?.checked_add((c & 127) as usize)?;
    }
    Some(val)
}

/// Parse an index with `hash` -byte object ids. `Ok(link)` carries the split-index
/// `link` extension's data when present. `Err(Bad)` = this is not an index with
/// that hash width; `Err(Escape)` = an entry names a path outside the root.
enum IndexErr {
    Bad,
    Escape,
}

fn parse_index(buf: &[u8], hash: usize) -> Result<Option<Vec<u8>>, IndexErr> {
    if buf.len() < 12 + hash || &buf[..4] != b"DIRC" {
        return Err(IndexErr::Bad);
    }
    let version = u32::from_be_bytes(buf[4..8].try_into().unwrap());
    if !(2..=4).contains(&version) {
        return Err(IndexErr::Bad);
    }
    let count = u32::from_be_bytes(buf[8..12].try_into().unwrap()) as usize;
    let end = buf.len() - hash;
    let mut pos = 12usize;
    let mut prev: Vec<u8> = Vec::new();
    for _ in 0..count {
        let fixed = 40 + hash + 2;
        let start = pos;
        let flags_at = start + 40 + hash;
        let flags = u16::from_be_bytes(
            buf.get(flags_at..flags_at + 2)
                .ok_or(IndexErr::Bad)?
                .try_into()
                .unwrap(),
        );
        let mut name_at = start + fixed;
        if version >= 3 && flags & 0x4000 != 0 {
            name_at += 2;
        }
        if name_at > end {
            return Err(IndexErr::Bad);
        }
        let path: Vec<u8>;
        if version == 4 {
            let mut p = name_at;
            let strip = index_varint(&buf[..end], &mut p).ok_or(IndexErr::Bad)?;
            if strip > prev.len() {
                return Err(IndexErr::Bad);
            }
            let nul = buf[p..end]
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexErr::Bad)?;
            let mut full = prev[..prev.len() - strip].to_vec();
            full.extend_from_slice(&buf[p..p + nul]);
            pos = p + nul + 1;
            path = full;
        } else {
            let nul = buf[name_at..end]
                .iter()
                .position(|b| *b == 0)
                .ok_or(IndexErr::Bad)?;
            path = buf[name_at..name_at + nul].to_vec();
            pos = start + ((name_at - start + nul + 8) & !7);
            if pos > end {
                return Err(IndexErr::Bad);
            }
        }
        if escapes_root(&path) {
            return Err(IndexErr::Escape);
        }
        prev = path;
    }
    // Extensions, then exactly the trailing checksum.
    let mut link = None;
    while pos < end {
        let head = buf.get(pos..pos + 8).ok_or(IndexErr::Bad)?;
        let size = u32::from_be_bytes(head[4..8].try_into().unwrap()) as usize;
        let data = buf
            .get(pos + 8..pos + 8 + size)
            .filter(|_| pos + 8 + size <= end)
            .ok_or(IndexErr::Bad)?;
        if &head[..4] == b"link" {
            link = Some(data.to_vec());
        }
        pos += 8 + size;
    }
    if pos != end {
        return Err(IndexErr::Bad);
    }
    Ok(link)
}

/// Check an index (or shared index) before git sees it: every entry path must
/// stay inside the work tree. Git `lstat`s each entry, so an entry such as
/// `../../x` would make the arm an existence oracle for files outside the
/// root. Returns the split-index `link` data, if any.
fn scan_index(buf: &[u8]) -> Result<Option<Vec<u8>>, String> {
    for hash in [20usize, 32] {
        match parse_index(buf, hash) {
            Ok(link) => return Ok(link),
            Err(IndexErr::Escape) => {
                return Err("the index names a path outside the project; refusing".to_string())
            }
            Err(IndexErr::Bad) => continue,
        }
    }
    Err("the repository index is not in a recognised format".to_string())
}

/// The repository's files a status run needs, resolved to canonical paths that
/// `may_read` admitted and checked. Built synchronously by
/// [`resolve_git_paths`] (on a blocking thread, under a timeout) so the
/// caller's reader never has to live across an `await`.
#[cfg_attr(test, derive(Debug))]
pub struct GitPaths {
    /// The repo's `config` (in the common dir for a linked worktree), read only
    /// as data for an allowlist of keys; `None` when the file is absent.
    config: Option<PathBuf>,
    /// `(name inside the sandbox, bytes)`: HEAD, the index and any shared
    /// index. COPIED, not linked, so what was checked is what git reads.
    files: Vec<(String, Vec<u8>, Option<std::time::SystemTime>)>,
    /// `(name inside the sandbox, target)` symlinks to data git only reads:
    /// refs, packed-refs, shallow, info/exclude, objects/pack and the loose
    /// fan-out directories.
    links: Vec<(String, PathBuf)>,
}

fn checked(p: &Path, may_read: &dyn Fn(&Path) -> bool) -> Result<Option<PathBuf>, String> {
    match p.canonicalize() {
        Ok(c) if may_read(&c) => Ok(Some(c)),
        Ok(_) => Err("`.git` points outside the readable roots".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("`.git` entry is unreadable: {e}")),
    }
}

/// [`checked`], and the target must be a regular file (git would block on a
/// FIFO until the timeout).
fn checked_file(p: &Path, may_read: &dyn Fn(&Path) -> bool) -> Result<Option<PathBuf>, String> {
    match checked(p, may_read)? {
        Some(c) => {
            if std::fs::metadata(&c).is_ok_and(|m| m.is_file()) {
                Ok(Some(c))
            } else {
                Err(format!(
                    "`{}` in the repository is not a regular file",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ))
            }
        }
        None => Ok(None),
    }
}

/// Resolve the files of the repository at `git_dir` (from [`resolve_git_dir`]).
/// `Ok(None)` when it lacks `HEAD`, `objects` or `refs` (not a repository).
/// A linked worktree's `commondir` and every entry that is a symlink are
/// followed only if `may_read` admits where they land; `refs/` and `objects/`
/// may hold no symlink at all; `objects/info/alternates` must be empty; and the
/// index is parsed so no entry can name a path outside the work tree.
pub fn resolve_git_paths(
    git_dir: &Path,
    may_read: &dyn Fn(&Path) -> bool,
) -> Result<Option<GitPaths>, String> {
    let common = match read_small_text(&git_dir.join("commondir"), 4096) {
        Ok(t) => {
            let p = PathBuf::from(t.trim());
            let p = if p.is_absolute() { p } else { git_dir.join(p) };
            checked(&p, may_read)?.ok_or_else(|| "`commondir` target is missing".to_string())?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => git_dir.to_path_buf(),
        Err(e) => return Err(format!("read commondir: {e}")),
    };
    let Some(head) = checked_file(&git_dir.join("HEAD"), may_read)? else {
        return Ok(None);
    };
    let (Some(objects), Some(refs)) = (
        checked(&common.join("objects"), may_read)?,
        checked(&common.join("refs"), may_read)?,
    ) else {
        return Ok(None);
    };
    let head_bytes = read_regular_capped(&head, 4096).map_err(|e| format!("read HEAD: {e}"))?;

    let mut budget = MAX_SCAN_ENTRIES;
    scan_no_symlinks(&refs, &mut budget, 0)?;

    // Objects: never link the directory whole (git would then follow its
    // `info/alternates`, which a writer can set at any moment). The sandbox
    // gets its own `objects/` holding only symlinks to `pack/` and the loose
    // fan-out directories, so alternates cannot be reached at all; a
    // repository that uses them is refused up front with a reason.
    for alt in ["info/alternates", "info/http-alternates"] {
        match read_small_text(&objects.join(alt), 1024 * 1024) {
            Ok(body) if names_alternates(&body) => {
                return Err(
                    "this repository uses alternate object stores, which are not supported here"
                        .to_string(),
                )
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("read objects/{alt}: {e}")),
        }
    }
    let mut links: Vec<(String, PathBuf)> = vec![("refs".into(), refs)];
    let rd = std::fs::read_dir(&objects).map_err(|e| format!("read objects: {e}"))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("read objects: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let ft = entry
            .file_type()
            .map_err(|e| format!("read objects: {e}"))?;
        if ft.is_symlink() {
            return Err(format!(
                "`objects/{name}` is a symlink; refusing to follow it"
            ));
        }
        if ft.is_dir() && (name == "pack" || is_fanout_dir(&name)) {
            scan_no_symlinks(&entry.path(), &mut budget, 0)?;
            links.push((format!("objects/{name}"), entry.path()));
        }
    }
    for (name, p) in [
        ("packed-refs", common.join("packed-refs")),
        ("info/exclude", common.join("info").join("exclude")),
        ("shallow", common.join("shallow")),
    ] {
        if let Some(c) = checked_file(&p, may_read)? {
            links.push((name.to_string(), c));
        }
    }

    let mut files: Vec<(String, Vec<u8>, Option<std::time::SystemTime>)> =
        vec![("HEAD".into(), head_bytes, None)];
    if let Some(idx) = checked_file(&git_dir.join("index"), may_read)? {
        let (bytes, idx_mtime) = read_regular_capped_mtime(&idx, MAX_INDEX_BYTES)
            .map_err(|e| format!("read index: {e}"))?;
        if let Some(link) = scan_index(&bytes)? {
            // Split index: the entries of the shared base live in
            // `sharedindex.<hash>` next to the index. The `link` data starts
            // with that hash; its width is the repo's (20 or 32 bytes).
            for width in [20usize, 32] {
                let Some(raw) = link.get(..width) else {
                    continue;
                };
                let name = format!(
                    "sharedindex.{}",
                    raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
                );
                if let Some(sp) = checked_file(&git_dir.join(&name), may_read)? {
                    let (sb, sb_mtime) = read_regular_capped_mtime(&sp, MAX_INDEX_BYTES)
                        .map_err(|e| format!("read shared index: {e}"))?;
                    scan_index(&sb)?;
                    files.push((name, sb, sb_mtime));
                    break;
                }
            }
        }
        files.push(("index".into(), bytes, idx_mtime));
    }
    Ok(Some(GitPaths {
        config: checked_file(&common.join("config"), may_read)?,
        files,
        links,
    }))
}

/// [`resolve_git_dir`] + [`resolve_git_paths`] on a blocking thread under the
/// same timeout as git itself. Every read of the repository's own files (a
/// repository is untrusted input: FIFOs, huge directories) happens here, off
/// the async workers, and a stall past the timeout is an error.
pub async fn prepare_git_paths(
    root: PathBuf,
    may_read: impl Fn(&Path) -> bool + Send + 'static,
) -> Result<Option<GitPaths>, String> {
    let work = tokio::task::spawn_blocking(move || {
        let Some(git_dir) = resolve_git_dir(&root, &may_read)? else {
            return Ok(None);
        };
        resolve_git_paths(&git_dir, &may_read)
    });
    match tokio::time::timeout(Duration::from_secs(GIT_STATUS_TIMEOUT_SECS), work).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Err(format!("reading the repository failed: {e}")),
        Err(_) => Err(format!(
            "reading the repository timed out after {GIT_STATUS_TIMEOUT_SECS}s"
        )),
    }
}

/// A private git directory git is run against instead of the repository's own.
///
/// Why: git has no switch for "no filters" and the repo's config can define
/// `filter.<n>.clean`, `core.fsmonitor`, `core.worktree`, ... Enumerating names
/// out of the config and blanking them is racy (the config can change between
/// the read and the run) and incomplete. Here git never reads the repo's
/// config or `info/attributes`: the sandbox holds copies of `HEAD` and the
/// index, symlinks to the repo's `refs` / `packed-refs` / `shallow` /
/// `info/exclude` and to `objects/pack` plus each loose fan-out directory
/// (data git only reads; never `objects/info`, so no alternates), and a
/// `config` this module authored from an allowlist of inert keys (see
/// [`sanitize_config`]). With no driver defined anywhere, a `filter=x`
/// attribute is inert, and there is no second run for the repo to change
/// underneath.
struct GitSandbox {
    dir: PathBuf,
}

impl GitSandbox {
    fn create() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!("ikenga-gitstatus-{}", uuid::Uuid::new_v4()));
        // Not `create_dir_all`: fail if the name exists, and owner-only.
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            b.mode(0o700);
        }
        b.create(&dir).map_err(|e| format!("create sandbox: {e}"))?;
        // Drop removes `dir`, also on the early returns below.
        Ok(Self { dir })
    }
}

impl Drop for GitSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Fill the sandbox `dir`. Blocking (writes up to two index copies).
#[cfg(unix)]
fn populate_sandbox(dir: &Path, paths: &GitPaths, config: &str) -> Result<(), String> {
    std::fs::write(dir.join("config"), config).map_err(|e| format!("write config: {e}"))?;
    std::fs::create_dir(dir.join("objects")).map_err(|e| format!("sandbox dir: {e}"))?;
    for (name, bytes, mtime) in &paths.files {
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("write {name}: {e}"))?;
        // Keep the source's mtime. Git decides an entry is "racily clean" by
        // comparing its mtime with the INDEX FILE's mtime; a copy stamped
        // "now" never trips that check, so a same-size rewrite made in the
        // same second as the last index write would read as clean.
        if let Some(m) = mtime {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(*m))
                .map_err(|e| format!("set mtime of {name}: {e}"))?;
        }
    }
    for (name, target) in &paths.links {
        let link = dir.join(name);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("sandbox dir: {e}"))?;
        }
        std::os::unix::fs::symlink(target, &link)
            .map_err(|e| format!("sandbox link `{name}`: {e}"))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn populate_sandbox(_dir: &Path, _paths: &GitPaths, _config: &str) -> Result<(), String> {
    Err("git status is not available on this platform".to_string())
}

/// Author the sandbox `config` from the raw `git config --list -z` output of
/// the repo's config: only keys that change what `status` reports and cannot
/// make git run anything. Everything else (filters, fsmonitor, hooks, includes,
/// `core.worktree`, remotes' urls, ...) is dropped. Values are re-escaped.
fn sanitize_config(raw: &[u8]) -> Result<String, String> {
    fn quote(v: &str) -> String {
        let mut o = String::from("\"");
        for c in v.chars() {
            match c {
                '\\' => o.push_str("\\\\"),
                '"' => o.push_str("\\\""),
                '\n' => o.push_str("\\n"),
                '\t' => o.push_str("\\t"),
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }
    fn plain(s: &str) -> bool {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }
    let text = String::from_utf8_lossy(raw);
    let mut version = "0".to_string();
    let mut objectformat: Option<String> = None;
    let mut body = String::new();
    for entry in text.split('\0').filter(|e| !e.is_empty()) {
        let (key, value) = match entry.split_once('\n') {
            Some((k, v)) => (k, Some(v)),
            None => (entry, None),
        };
        let (Some(first), Some(last)) = (key.find('.'), key.rfind('.')) else {
            continue;
        };
        let section = &key[..first];
        let var = &key[last + 1..];
        let sub = (first != last).then(|| &key[first + 1..last]);
        if !plain(section) || !plain(var) {
            continue;
        }
        let kept = match (section, sub, var) {
            ("core", None, "repositoryformatversion") => {
                if matches!(value, Some("0") | Some("1")) {
                    version = value.unwrap().to_string();
                }
                false
            }
            ("extensions", None, "objectformat") => {
                if matches!(value, Some("sha1") | Some("sha256")) {
                    objectformat = value.map(str::to_string);
                }
                false
            }
            ("extensions", None, "refstorage") => {
                if value.map(|v| v.to_ascii_lowercase()) != Some("files".to_string()) {
                    return Err("this repository's ref storage is not supported here".to_string());
                }
                false
            }
            (
                "core",
                None,
                "ignorecase" | "filemode" | "symlinks" | "precomposeunicode" | "autocrlf" | "eol",
            ) => true,
            ("status", None, "showuntrackedfiles" | "aheadbehind") => true,
            ("branch", Some(_), "remote" | "merge") => true,
            ("remote", Some(_), "fetch") => true,
            _ => false,
        };
        if !kept {
            continue;
        }
        if sub.is_some_and(|s| s.contains('\n')) {
            continue;
        }
        match sub {
            Some(s) => body.push_str(&format!("[{section} {}]\n", quote(s))),
            None => body.push_str(&format!("[{section}]\n")),
        }
        match value {
            Some(v) => body.push_str(&format!("\t{var} = {}\n", quote(v))),
            None => body.push_str(&format!("\t{var}\n")),
        }
    }
    let mut out = format!("[core]\n\trepositoryformatversion = {version}\n\tbare = false\n");
    if let (Some(f), "1") = (&objectformat, version.as_str()) {
        out.push_str(&format!("[extensions]\n\tobjectformat = {f}\n"));
    }
    out.push_str(&body);
    Ok(out)
}

/// Run a hardened `git status --porcelain=v2 --branch` confined to `root`
/// (`paths` from [`prepare_git_paths`]), against a private sandbox git dir (see
/// [`GitSandbox`]). Returns `Ok(None)` if git says `root` is not a repository.
/// A truncated, timed-out or unrecognisable result is an error, never a
/// smaller-looking state.
pub async fn run_hardened_git_status(
    root: &Path,
    paths: GitPaths,
) -> Result<Option<GitStatusResult>, String> {
    let sandbox = GitSandbox::create()?;

    // Read the repo's config once, as data only: `--file` + `--no-includes` has
    // git parse that one file and nothing else, and parsing runs no program.
    let raw_config = match &paths.config {
        Some(cfg) => {
            let mut spec = crate::executor::SpawnSpec::new("git");
            spec.current_dir(&sandbox.dir);
            spec.env("GIT_CONFIG_NOSYSTEM", "1");
            spec.env("GIT_CONFIG_GLOBAL", "/dev/null");
            if let Some(tmp) = sandbox.dir.parent() {
                spec.env("GIT_CEILING_DIRECTORIES", tmp);
            }
            spec.args([
                OsStr::new("config"),
                OsStr::new("--file"),
                cfg.as_os_str(),
                OsStr::new("--no-includes"),
                OsStr::new("--list"),
                OsStr::new("-z"),
            ]);
            let out = run_capped(spec, 1024 * 1024).await?;
            if !out.success {
                return Err("git could not read the repository config".to_string());
            }
            out.stdout
        }
        None => Vec::new(),
    };
    let config = sanitize_config(&raw_config)?;
    let dir = sandbox.dir.clone();
    tokio::task::spawn_blocking(move || populate_sandbox(&dir, &paths, &config))
        .await
        .map_err(|e| format!("sandbox setup failed: {e}"))??;

    // `--ignore-submodules=all`: status would otherwise run `git status` inside
    // each submodule, under that submodule's own (hostile) config. The cost: a
    // dirty submodule is not shown (only a moved submodule commit would be, and
    // not even that is reported as a file change here).
    let spec = hardened_spec(
        root,
        &sandbox.dir,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--ignore-submodules=all",
        ],
    );
    let out = run_capped(spec, GIT_STATUS_OUTPUT_CAP).await?;
    if !out.success {
        if is_not_a_repo(&out) {
            return Ok(None);
        }
        let msg = out.stderr.trim();
        let msg: String = msg.chars().take(300).collect();
        return Err(format!("git status failed: {msg}"));
    }
    let stdout_text = String::from_utf8_lossy(&out.stdout);
    // `--branch` always emits a `# branch.oid` header; its absence means this
    // was not git's status output.
    if !stdout_text.lines().any(|l| l.starts_with("# branch.oid ")) {
        return Err("git status produced unrecognised output".to_string());
    }
    let res = parse_porcelain_v2(&stdout_text);
    // Belt and braces behind the index pre-scan: never echo a path that is not
    // inside the root.
    let all = [&res.staged, &res.unstaged, &res.untracked, &res.conflicted];
    if all
        .iter()
        .flat_map(|v| v.iter())
        .any(|c| escapes_root(c.path.as_bytes()))
    {
        return Err("git reported a path outside the project; refusing".to_string());
    }
    Ok(Some(res))
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

    #[test]
    fn sanitize_config_keeps_inert_keys_and_drops_the_rest() {
        let raw =
            b"core.repositoryformatversion\n0\0core.fsmonitor\n/x/evil\0core.worktree\n/etc\0\
            core.hookspath\n/x\0filter.evil.clean\n/x/evil\0filter.a.b.c.process\n/x\0\
            include.path\nmore.cfg\0core.ignorecase\ntrue\0core.filemode\0\
            branch.main.remote\norigin\0branch.main.merge\nrefs/heads/main\0\
            remote.origin.url\n/x\0remote.origin.fetch\n+refs/heads/*:refs/remotes/origin/*\0\
            branch.we\"ird.remote\nor\"igin\0diff.external\n/x/evil\0";
        let out = sanitize_config(raw).unwrap();
        for bad in [
            "fsmonitor",
            "worktree",
            "hookspath",
            "filter",
            "include",
            "url",
            "external",
        ] {
            assert!(!out.to_lowercase().contains(bad), "{bad} leaked: {out}");
        }
        assert!(out.contains("ignorecase = \"true\""), "{out}");
        assert!(out.contains("\tfilemode\n"), "{out}");
        assert!(
            out.contains("[branch \"main\"]\n\tremote = \"origin\""),
            "{out}"
        );
        assert!(out.contains("[branch \"we\\\"ird\"]"), "{out}");
        assert!(out.contains("remote = \"or\\\"igin\""), "{out}");
        assert!(
            out.contains("fetch = \"+refs/heads/*:refs/remotes/origin/*\""),
            "{out}"
        );
    }

    #[test]
    fn sanitize_config_refuses_unsupported_ref_storage() {
        assert!(sanitize_config(b"extensions.refstorage\nreftable\0").is_err());
        assert!(sanitize_config(b"extensions.refstorage\nfiles\0").is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_errors_on_oversized_output_instead_of_truncating() {
        let mut spec = crate::executor::SpawnSpec::new("sh");
        spec.args(["-c", "head -c 5000000 /dev/zero | tr '\\0' x"]);
        let e = run_capped(spec, 1024).await.err().expect("must error");
        assert!(e.contains("exceeded"), "{e}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_capped_times_out_a_hanging_process() {
        let mut spec = crate::executor::SpawnSpec::new("sh");
        spec.args(["-c", "sleep 30"]);
        let started = std::time::Instant::now();
        let e = run_capped(spec, 1024).await.err().expect("must error");
        assert!(e.contains("timed out"), "{e}");
        assert!(started.elapsed() < Duration::from_secs(GIT_STATUS_TIMEOUT_SECS + 3));
    }

    // ── hostile-repository checks ───────────────────────────────────────────

    fn out(stderr: &str) -> GitOut {
        GitOut {
            success: false,
            stdout: Vec::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn only_gits_not_a_repository_message_means_not_a_repo() {
        assert!(is_not_a_repo(&out(
            "fatal: not a git repository (or any of the parent directories): .git"
        )));
        // Exit 128 for any other reason (a shallow/split repo we mis-described,
        // a missing object, a corrupt index) is an error, not "no chip".
        for e in [
            "fatal: bad object HEAD",
            "fatal: unable to read tree 0123",
            "fatal: index file corrupt",
            "fatal: could not read split-index file",
            "",
        ] {
            assert!(!is_not_a_repo(&out(e)), "{e}");
        }
    }

    #[test]
    fn escapes_root_catches_absolute_and_parent_components() {
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a/b/..",
            "..",
            "../../../outside/f",
        ] {
            assert!(escapes_root(bad.as_bytes()), "{bad}");
        }
        for good in ["a.txt", "dir/sub/f", "..hidden", "a/..b/c", "dir/", "x..y"] {
            assert!(!escapes_root(good.as_bytes()), "{good}");
        }
    }

    /// A syntactically valid index (checksum bytes are not verified by the
    /// pre-scan; git does that itself).
    fn build_index(version: u32, hash: usize, paths: &[&str]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"DIRC");
        b.extend_from_slice(&version.to_be_bytes());
        b.extend_from_slice(&(paths.len() as u32).to_be_bytes());
        let mut prev: Vec<u8> = Vec::new();
        for p in paths {
            let start = b.len();
            b.extend_from_slice(&[0u8; 40]);
            b.extend_from_slice(&vec![7u8; hash]);
            b.extend_from_slice(&((p.len().min(0xFFF)) as u16).to_be_bytes());
            if version == 4 {
                let common = prev
                    .iter()
                    .zip(p.as_bytes())
                    .take_while(|(a, b)| a == b)
                    .count();
                let strip = prev.len() - common;
                // git's encode_varint
                let mut v = vec![(strip & 127) as u8];
                let mut val = strip;
                while {
                    val >>= 7;
                    val != 0
                } {
                    val -= 1;
                    v.insert(0, 128 | (val & 127) as u8);
                }
                b.extend_from_slice(&v);
                b.extend_from_slice(&p.as_bytes()[common..]);
                b.push(0);
            } else {
                b.extend_from_slice(p.as_bytes());
                let name_at = 40 + hash + 2;
                let total = (name_at + p.len() + 8) & !7;
                b.resize(start + total, 0);
            }
            prev = p.as_bytes().to_vec();
        }
        b.extend_from_slice(&vec![0u8; hash]);
        b
    }

    #[test]
    fn scan_index_refuses_entries_outside_the_root_in_every_format() {
        for (version, hash) in [(2u32, 20usize), (3, 20), (4, 20), (2, 32), (4, 32)] {
            let ok = build_index(version, hash, &["a.txt", "dir/b.txt", "dir/sub/c.txt"]);
            assert!(scan_index(&ok).is_ok(), "v{version}/{hash} clean index");
            for bad in ["../../../outside/secret", "/etc/passwd", "dir/../../x"] {
                let idx = build_index(version, hash, &["a.txt", "dir/b.txt", bad]);
                let e = scan_index(&idx).err().unwrap_or_default();
                assert!(
                    e.contains("outside the project"),
                    "v{version}/{hash} {bad}: {e:?}"
                );
            }
        }
        // v4 prefix compression must not let a `..` hide in a shared prefix.
        let idx = build_index(4, 20, &["dir/a", "dir/../x"]);
        assert!(scan_index(&idx).is_err());
        assert!(scan_index(b"not an index at all, just text").is_err());
    }

    #[test]
    fn scan_index_reports_the_split_index_link() {
        let mut idx = build_index(2, 20, &["a.txt"]);
        let trailer = idx.split_off(idx.len() - 20);
        idx.extend_from_slice(b"link");
        idx.extend_from_slice(&20u32.to_be_bytes());
        idx.extend_from_slice(&[0xab; 20]);
        idx.extend_from_slice(&trailer);
        assert_eq!(scan_index(&idx).unwrap(), Some(vec![0xab; 20]));
    }

    #[test]
    fn scan_no_symlinks_refuses_any_symlink_at_any_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        std::fs::create_dir_all(d.join("heads/feat")).unwrap();
        std::fs::write(d.join("heads/feat/x"), "ref").unwrap();
        let mut budget = 100;
        assert!(scan_no_symlinks(d, &mut budget, 0).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", d.join("heads/feat/link")).unwrap();
            let mut budget = 100;
            let e = scan_no_symlinks(d, &mut budget, 0).unwrap_err();
            assert!(e.contains("symlink"), "{e}");
        }
        let mut budget = 1;
        assert!(
            scan_no_symlinks(d, &mut budget, 0).is_err(),
            "budget must bound the scan"
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_no_symlinks_refuses_a_fifo_without_opening_it() {
        use std::os::unix::ffi::OsStrExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("main");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let mut budget = 10;
        let e = scan_no_symlinks(tmp.path(), &mut budget, 0).unwrap_err();
        assert!(e.contains("not a regular file"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn populate_sandbox_keeps_the_source_mtime_of_the_index() {
        let sb = GitSandbox::create().unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let paths = GitPaths {
            config: None,
            files: vec![
                ("HEAD".into(), b"ref: refs/heads/main\n".to_vec(), None),
                ("index".into(), b"DIRC".to_vec(), Some(old)),
            ],
            links: vec![],
        };
        populate_sandbox(&sb.dir, &paths, "[core]\n").unwrap();
        let got = std::fs::metadata(sb.dir.join("index"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(got, old);
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_capped_never_blocks_on_a_fifo_and_caps_size() {
        use std::os::unix::ffi::OsStrExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("HEAD");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(read_regular_capped(&fifo, 4096).is_err());
        assert!(read_small_text(&fifo, 4096).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let big = tmp.path().join("big");
        std::fs::write(&big, vec![b'x'; 5000]).unwrap();
        assert!(read_regular_capped(&big, 4096).is_err());
        assert_eq!(read_regular_capped(&big, 5000).unwrap().len(), 5000);
        std::os::unix::fs::symlink(&big, tmp.path().join("l")).unwrap();
        assert!(read_regular_capped(&tmp.path().join("l"), 5000).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_is_owner_only_and_has_no_way_to_alternates() {
        use std::os::unix::fs::PermissionsExt as _;
        let sb = GitSandbox::create().unwrap();
        let mode = std::fs::metadata(&sb.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let tmp = tempfile::tempdir().unwrap();
        let pack = tmp.path().join("pack");
        std::fs::create_dir_all(&pack).unwrap();
        let paths = GitPaths {
            config: None,
            files: vec![("HEAD".into(), b"ref: refs/heads/main\n".to_vec(), None)],
            links: vec![("objects/pack".into(), pack)],
        };
        populate_sandbox(&sb.dir, &paths, "[core]\n").unwrap();
        assert!(sb.dir.join("objects/pack").exists());
        assert!(!sb.dir.join("objects/info").exists());
        assert!(std::fs::symlink_metadata(sb.dir.join("objects"))
            .unwrap()
            .file_type()
            .is_dir());
    }

    #[test]
    fn hardened_spec_pins_every_repository_redirect() {
        let spec = hardened_spec(Path::new("/p/root"), Path::new("/sb"), &["status"]);
        let get = |k: &str| {
            spec.env
                .vars
                .iter()
                .rev()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.to_string_lossy().into_owned())
        };
        assert_eq!(get("GIT_DIR").as_deref(), Some("/sb"));
        assert_eq!(get("GIT_COMMON_DIR").as_deref(), Some("/sb"));
        assert_eq!(get("GIT_OBJECT_DIRECTORY").as_deref(), Some("/sb/objects"));
        assert_eq!(get("GIT_INDEX_FILE").as_deref(), Some("/sb/index"));
        assert_eq!(get("GIT_ALTERNATE_OBJECT_DIRECTORIES").as_deref(), Some(""));
        assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("0"));
        assert_eq!(get("LC_ALL").as_deref(), Some("C"));
        assert!(spec.args.iter().any(|a| a == "core.untrackedCache=false"));
    }

    #[cfg(unix)]
    mod resolve {
        use super::super::*;

        fn git(dir: &Path, args: &[&str]) {
            assert!(std::process::Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@e.x"])
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap()
                .status
                .success());
        }

        fn repo() -> tempfile::TempDir {
            let t = tempfile::tempdir().unwrap();
            git(t.path(), &["init", "-q", "-b", "main"]);
            std::fs::write(t.path().join("a.txt"), "a").unwrap();
            git(t.path(), &["add", "-A"]);
            git(t.path(), &["commit", "-qm", "i"]);
            t
        }

        #[test]
        fn resolves_a_clean_repo_and_copies_head_and_index() {
            let t = repo();
            let g = t.path().join(".git");
            let p = resolve_git_paths(&g, &|_| true).unwrap().unwrap();
            let names: Vec<_> = p.files.iter().map(|(n, _, _)| n.as_str()).collect();
            assert!(
                names.contains(&"HEAD") && names.contains(&"index"),
                "{names:?}"
            );
            assert!(p.links.iter().any(|(n, _)| n == "refs"));
            // `objects/info` is never linked, so alternates are unreachable.
            assert!(p.links.iter().all(|(n, _)| !n.starts_with("objects/info")));
        }

        #[test]
        fn alternates_naming_a_store_are_refused_and_comments_are_not() {
            let t = repo();
            let g = t.path().join(".git");
            let alt = g.join("objects/info/alternates");
            std::fs::write(&alt, "/somewhere/else/objects\n").unwrap();
            let e = resolve_git_paths(&g, &|_| true).err().unwrap();
            assert!(e.contains("alternate"), "{e}");
            std::fs::write(&alt, "# nothing\n\n").unwrap();
            assert!(resolve_git_paths(&g, &|_| true).is_ok());
            std::fs::remove_file(&alt).unwrap();
            std::fs::write(g.join("objects/info/http-alternates"), "http://x/objects\n").unwrap();
            assert!(resolve_git_paths(&g, &|_| true).is_err());
        }

        #[test]
        fn a_symlinked_ref_pack_or_fanout_is_refused() {
            let t = repo();
            let g = t.path().join(".git");
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("f");
            std::fs::write(&target, "x").unwrap();
            let link = g.join("refs/heads/evil");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert!(resolve_git_paths(&g, &|_| true)
                .unwrap_err()
                .contains("symlink"));
            std::fs::remove_file(&link).unwrap();
            assert!(resolve_git_paths(&g, &|_| true).is_ok());
            let fan = std::fs::read_dir(g.join("objects"))
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| p.file_name().unwrap().len() == 2)
                .unwrap();
            let obj = std::fs::read_dir(&fan)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let keep = std::fs::read(&obj).unwrap();
            std::fs::remove_file(&obj).unwrap();
            let moved = outside.path().join("obj");
            std::fs::write(&moved, keep).unwrap();
            std::os::unix::fs::symlink(&moved, &obj).unwrap();
            assert!(resolve_git_paths(&g, &|_| true)
                .unwrap_err()
                .contains("symlink"));
        }

        #[test]
        fn non_regular_repo_files_are_refused_not_waited_on() {
            use std::os::unix::ffi::OsStrExt as _;
            let t = repo();
            let g = t.path().join(".git");
            let packed = g.join("packed-refs");
            let c = std::ffi::CString::new(packed.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            let started = std::time::Instant::now();
            assert!(resolve_git_paths(&g, &|_| true).is_err());
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn shallow_and_shared_index_are_carried_into_the_sandbox() {
            let t = repo();
            let g = t.path().join(".git");
            std::fs::write(
                g.join("shallow"),
                "0123456789012345678901234567890123456789\n",
            )
            .unwrap();
            git(t.path(), &["update-index", "--split-index"]);
            let p = resolve_git_paths(&g, &|_| true).unwrap().unwrap();
            assert!(p.links.iter().any(|(n, _)| n == "shallow"));
            assert!(
                p.files.iter().any(|(n, _, _)| n.starts_with("sharedindex.")),
                "{:?}",
                p.files.iter().map(|(n, _, _)| n).collect::<Vec<_>>()
            );
        }

        #[tokio::test]
        async fn prepare_reports_a_may_read_refusal() {
            let t = repo();
            let r = prepare_git_paths(t.path().to_path_buf(), |_| false).await;
            assert!(r.is_err(), "a guard that admits nothing must refuse");
        }
    }
}
