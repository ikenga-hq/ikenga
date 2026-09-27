//! Per-project Atelier skill files: `<project_root>/.atelier/<skill>/<file>`
//! (e.g. the Tasks pkg's roster at `.atelier/skill-tasks/roster.json`, written
//! by `skill-tasks setup`). The bodies of the desktop's `atelier_file_read` /
//! `atelier_file_write` (`commands::skill_roster`) and the daemon's arms of
//! the same names (WP-19 slice 6).
//!
//! **Two reaches.** [`Reach::Follow`] is the desktop, unchanged: `project_root`
//! is the shell's own `projects` row, trusted by provenance; the only checks
//! are [`is_safe_segment`] on `skill` / `file`, and paths are followed as the
//! OS resolves them. [`Reach::Confined`] is the daemon, whose caller is a
//! remote token holder naming any root it likes, so on top of the segment
//! checks:
//!
//! * the root must pass the daemon's `PathGuard` (absolute, no `..`, inside the
//!   fs allowlist, not the daemon's data dir / discovery file) both as given
//!   and canonicalized, and must be an existing directory;
//! * everything below it is built from the CANONICAL root, and a symlink at
//!   `.atelier`, `.atelier/<skill>` or the file itself is refused — so no read
//!   or write can follow a link out of the root, dangling or not;
//! * the final target goes through the guard again (the data dir compared by
//!   inode, e.g. a bind mount inside the root);
//! * the write's temp file is created exclusively (`create_new`, which does
//!   not follow a planted link), and the directory it lands in must still
//!   canonicalize to itself after `create_dir_all`.
//!
//! Every error string the desktop returns is returned unchanged; the daemon's
//! extra refusals carry the same `atelier_file_{read,write}: ` prefix.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A single path segment (`skill` or `file`) is safe iff it is non-empty and
/// contains no path separator or `..` traversal sequence. Normal filenames with
/// dots (`roster.json`) are allowed; only traversal is blocked.
pub fn is_safe_segment(seg: &str) -> bool {
    !seg.is_empty() && !seg.contains('/') && !seg.contains('\\') && !seg.contains("..")
}

/// A path check: `Ok` when the path may be touched. The daemon passes its
/// `PathGuard::check_maybe_missing`.
pub type PathCheck<'a> = &'a (dyn Fn(&Path) -> Result<(), String> + Sync);

/// How far the helpers may follow the paths they touch (see the module doc).
#[derive(Clone, Copy)]
pub enum Reach<'a> {
    /// The desktop: paths as given, symlinks followed.
    Follow,
    /// The daemon: confined to the canonical root, which `check` admits.
    Confined(PathCheck<'a>),
}

/// Monotonic sequence appended to temp filenames so two concurrent writes to
/// the same target from this process never collide on the temp path.
static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Refuse a symlink at `path` (`lstat`; a missing path is fine).
fn refuse_symlink(cmd: &str, path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{cmd}: refusing to follow a symlink out of the project root: {}",
            path.display()
        )),
        _ => Ok(()),
    }
}

/// [`Reach::Confined`]'s boundary: the target `<canonical root>/.atelier/
/// <skill>/<file>`, or `None` when the (admitted) root is not an existing
/// directory — nothing under it to read, and nowhere the daemon will create.
fn confine(
    cmd: &str,
    check: PathCheck<'_>,
    root: &str,
    skill: &str,
    file: &str,
) -> Result<Option<PathBuf>, String> {
    let given = Path::new(root);
    // Absolute, no `..`, and — resolving its nearest existing ancestor — in
    // the allowlist and outside the daemon's state. A missing root is admitted
    // here only so the reader can answer "absent" for it, as the desktop does.
    check(given).map_err(|e| format!("{cmd}: {e}"))?;
    let canonical = match given.canonicalize() {
        Ok(c) if c.is_dir() => c,
        _ => return Ok(None),
    };
    check(&canonical).map_err(|e| format!("{cmd}: {e}"))?;
    let atelier = canonical.join(".atelier");
    let dir = atelier.join(skill);
    let target = dir.join(file);
    for p in [&atelier, &dir, &target] {
        refuse_symlink(cmd, p)?;
    }
    check(&target).map_err(|e| format!("{cmd}: {e}"))?;
    Ok(Some(target))
}

/// Read `<project_root>/.atelier/<skill>/<file>`.
///
/// `Ok(None)` for everything the desktop folds into "no file": no / empty
/// root, an unsafe segment, an absent file, any IO error — the FE then falls
/// back to its static defaults. Under [`Reach::Follow`] this never errs.
/// Under [`Reach::Confined`] a root or path the boundary refuses is an `Err`
/// (a refusal, not an absent file).
pub fn read(
    project_root: Option<&str>,
    skill: &str,
    file: &str,
    reach: Reach<'_>,
) -> Result<Option<String>, String> {
    const CMD: &str = "atelier_file_read";
    let Some(root) = project_root.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if !is_safe_segment(skill) || !is_safe_segment(file) {
        tracing::warn!(
            skill = %skill,
            file = %file,
            "atelier_file_read: rejected unsafe path segment"
        );
        return Ok(None);
    }
    let path = match reach {
        Reach::Follow => PathBuf::from(root).join(".atelier").join(skill).join(file),
        Reach::Confined(check) => match confine(CMD, check, root, skill, file)? {
            Some(target) => target,
            None => return Ok(None),
        },
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(Some(contents)),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                // Log unexpected errors (permissions, etc.) but still return
                // None — the FE static fallback handles it gracefully.
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "atelier_file_read: could not read file"
                );
            }
            Ok(None)
        }
    }
}

/// Atomically write `<project_root>/.atelier/<skill>/<file>` with `content`;
/// the written absolute path on success. Parent directories are created as
/// needed; `content` lands in a uniquely-named sibling temp file that is then
/// `rename`d over the target, so a concurrent [`read`] sees the old bytes or
/// the new ones, never a torn file. Failures are surfaced (the setup surface
/// must know whether the confirm-write landed).
pub fn write(
    project_root: Option<&str>,
    skill: &str,
    file: &str,
    content: &str,
    reach: Reach<'_>,
) -> Result<String, String> {
    const CMD: &str = "atelier_file_write";
    let root = project_root
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "atelier_file_write: no project root configured".to_string())?;
    if !is_safe_segment(skill) || !is_safe_segment(file) {
        tracing::warn!(
            skill = %skill,
            file = %file,
            "atelier_file_write: rejected unsafe path segment"
        );
        return Err(format!(
            "atelier_file_write: unsafe path segment (skill={skill:?}, file={file:?})"
        ));
    }
    let (dir, confined) = match reach {
        Reach::Follow => (PathBuf::from(root).join(".atelier").join(skill), false),
        Reach::Confined(check) => {
            let target = confine(CMD, check, root, skill, file)?
                .ok_or_else(|| format!("{CMD}: project root is not a directory: {root}"))?;
            let dir = target
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| format!("{CMD}: failed to resolve {}", target.display()))?;
            (dir, true)
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "atelier_file_write: could not create {}: {e}",
            dir.display()
        )
    })?;
    if confined && dir.canonicalize().ok().as_deref() != Some(dir.as_path()) {
        // Swapped for a link between the check and the create.
        return Err(format!(
            "{CMD}: refusing to follow a symlink out of the project root: {}",
            dir.display()
        ));
    }
    let path = dir.join(file);

    // Atomic commit: write to a hidden, uniquely-named sibling then rename over
    // the target. `.file.tmp-<pid>-<seq>` is itself a plain filename (no
    // separators — `file` already passed is_safe_segment), so it stays inside
    // the locked directory.
    let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{file}.tmp-{}-{seq}", std::process::id()));
    let written = if confined {
        // Exclusive create: a link planted at the temp name is not followed.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut f| f.write_all(content.as_bytes()))
    } else {
        std::fs::write(&tmp, content.as_bytes())
    };
    written.map_err(|e| format!("atelier_file_write: could not write temp file: {e}"))?;
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(path.to_string_lossy().into_owned()),
        Err(e) => {
            // Best-effort cleanup so a failed commit doesn't litter the dir.
            let _ = std::fs::remove_file(&tmp);
            Err(format!("atelier_file_write: could not commit write: {e}"))
        }
    }
}
