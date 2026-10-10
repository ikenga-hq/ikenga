//! What a viewer preview mount may serve. Shared by the daemon
//! (`server::viewer`) and the desktop (`commands::viewer` + `viewer_server`),
//! so both bound a preview the same way.
//!
//! The mount root is computed client-side from the previewed page's own markup
//! (`../` ascents), and the page is untrusted, so nothing here trusts it:
//!
//! 1. [`resolve_mount`] decides, from the host's own project registry, how high
//!    a root may go (the file's project root, or for a file in no project its
//!    own directory) and whether the mount is a [`MountScope::Tree`] or a
//!    [`MountScope::SingleFile`]. A bound that is the home directory, an
//!    ancestor of it, or `/` is far too wide to serve a tree from, so the
//!    mount then serves the previewed file and nothing else.
//! 2. [`may_serve`] is the serve-time gate, run on the canonical path of every
//!    request: the single-file rule, then [`is_sensitive_path`].
//!
//! Every refusal at serve time is a plain 404 so a page cannot probe which
//! credential files exist.

use std::path::{Component, Path, PathBuf};

/// Why `viewer_serve` refused a root. Names neither the project root nor any
/// path it was not handed: the caller already knows what it asked for.
pub const ROOT_ABOVE_PROJECT: &str =
    "preview root is above the project root; refusing to widen the viewer mount beyond it";

/// What a mount serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MountScope {
    /// Everything under the mount root (subject to [`is_sensitive_path`]).
    Tree,
    /// Only this canonical file; every other path under the mount is a 404.
    SingleFile(PathBuf),
}

/// The deepest project root (from the host's `projects` table) that contains
/// `file`, or `None` when the file belongs to no project.
///
/// A root that is, or contains, `home` is skipped: a project registered at the
/// whole home directory bounds nothing, so a file under it falls back to the
/// next project, or to its own directory. The default project has no root.
pub fn project_root_of(
    file: &Path,
    project_roots: &[PathBuf],
    home: Option<&Path>,
) -> Option<PathBuf> {
    project_roots
        .iter()
        .filter(|r| r.parent().is_some()) // `/` is not a project
        .filter(|r| home.is_none_or(|h| !h.starts_with(r)))
        .filter(|r| file.starts_with(r))
        .max_by_key(|r| r.components().count())
        .cloned()
}

/// Whether `dir` is too wide to serve a tree from: the home directory, an
/// ancestor of it, `/`, or a top-level directory (`/tmp`, `/home`, `/mnt`).
fn is_broad_dir(dir: &Path, home: Option<&Path>) -> bool {
    let depth = dir
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    depth <= 1
        || dir.parent().is_none()
        || home.is_some_and(|h| h.starts_with(dir))
        // Someone's home directory (a sibling of ours, or anything directly
        // under /home) is as broad as our own, even when registered as a project.
        || dir.parent().is_some_and(|p| {
            p == Path::new("/home") || home.is_some_and(|h| h.parent() == Some(p))
        })
}

/// Decide what a viewer mount rooted at `root` may serve for the page `file`
/// (both canonical). The root must contain the file and may reach no higher
/// than the file's project root, or, for a file in no project, its own
/// directory; a root above that bound is refused, never silently clamped.
///
/// When the bound is the home directory, an ancestor of it or `/` (a file
/// straight in `~`, or in a project rooted there), the mount is
/// [`MountScope::SingleFile`]: the preview still works, but only for the
/// previewed file.
pub fn resolve_mount(
    root: &Path,
    file: &Path,
    project_roots: &[PathBuf],
    home: Option<&Path>,
) -> Result<MountScope, String> {
    if !file.starts_with(root) {
        return Err("preview root does not contain the file".to_string());
    }
    let own_dir = file
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "preview file has no directory".to_string())?;
    let bound = project_root_of(file, project_roots, home).unwrap_or_else(|| own_dir.clone());
    if !root.starts_with(&bound) {
        return Err(ROOT_ABOVE_PROJECT.to_string());
    }
    if is_broad_dir(&bound, home) {
        Ok(MountScope::SingleFile(file.to_path_buf()))
    } else {
        Ok(MountScope::Tree)
    }
}

/// Names that are credentials or credential stores wherever they sit.
const SENSITIVE_NAMES: &[&str] = &[
    ".ssh",
    ".claude",
    ".aws",
    ".gnupg",
    ".config",
    ".netrc",
    ".git-credentials",
    ".env",
    ".docker",
    ".kube",
    ".azure",
    ".npmrc",
    ".pgpass",
    ".password-store",
];

/// Whether any component of the canonical `path` names a credential store or
/// credential file: the [`SENSITIVE_NAMES`], `.env.*`, and `id_*` private keys
/// (`id_rsa`, `id_ed25519`, ...). Applies whatever the mount root is, so a
/// project root that happens to contain `.ssh` still does not serve it.
pub fn is_sensitive_path(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(name) => {
            let Some(name) = name.to_str() else {
                return false;
            };
            let n = name.to_ascii_lowercase();
            SENSITIVE_NAMES.contains(&n.as_str())
                || n.starts_with(".env.")
                || ["rsa", "dsa", "ecdsa", "ed25519"]
                    .iter()
                    .any(|k| n.strip_prefix("id_").is_some_and(|r| r.starts_with(k)))
        }
        _ => false,
    })
}

/// The serve-time gate for a request that resolved to `target` (canonical,
/// inside the mount root). `false` means answer 404.
pub fn may_serve(scope: &MountScope, target: &Path) -> bool {
    if is_sensitive_path(target) {
        return false;
    }
    // The denylist is by name, so a hardlink with an innocent name to a
    // credential (e.g. pages/notes.txt -> ~/.ssh/id_rsa) would pass it. A
    // previewable file never needs more than one link.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(md) = std::fs::metadata(target) {
            if md.is_file() && md.nlink() > 1 {
                return false;
            }
        }
    }
    match scope {
        MountScope::Tree => true,
        MountScope::SingleFile(only) => target == only,
    }
}

/// Canonical roots of the active projects in a `ikenga.db`. Unreadable rows
/// (missing directory, no root) are skipped, which can only shrink the set of
/// project roots and so only tighten the bound.
pub async fn project_roots(db: &crate::db::PaDb) -> Vec<PathBuf> {
    let Ok(pool) = db.ensure_reader_pool().await else {
        return Vec::new();
    };
    let rows: Vec<(Option<String>,)> =
        sqlx::query_as("SELECT root_path FROM projects WHERE archived_at IS NULL")
            .fetch_all(&pool)
            .await
            .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(r,)| r)
        .filter(|r| !r.trim().is_empty())
        .filter_map(|r| Path::new(&r).canonicalize().ok())
        .filter(|r| r.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn project_root_of_picks_deepest_and_skips_home_and_slash() {
        let projects = vec![p("/h/proj"), p("/h/proj/inner"), p("/h"), p("/")];
        let home = p("/h");
        assert_eq!(
            project_root_of(Path::new("/h/proj/inner/x/a.html"), &projects, Some(&home)),
            Some(p("/h/proj/inner"))
        );
        assert_eq!(
            project_root_of(Path::new("/h/other/a.html"), &projects, Some(&home)),
            None
        );
    }

    #[test]
    fn resolve_mount_bounds_and_scopes() {
        let projects = vec![p("/h/proj"), p("/h/proj/inner"), p("/h")];
        let home = p("/h");
        let r = |root: &str, file: &str| {
            resolve_mount(Path::new(root), Path::new(file), &projects, Some(&home))
        };
        assert_eq!(r("/h/proj", "/h/proj/a/b.html"), Ok(MountScope::Tree));
        assert_eq!(r("/h/proj/a", "/h/proj/a/b.html"), Ok(MountScope::Tree));
        assert!(r("/h", "/h/proj/a/b.html").is_err());
        // Nested project: the deepest one bounds, so `/h/proj` is too high.
        assert!(r("/h/proj", "/h/proj/inner/b.html").is_err());
        // No project: own directory only.
        assert_eq!(r("/h/other", "/h/other/a.html"), Ok(MountScope::Tree));
        assert!(r("/h", "/h/other/a.html").is_err());
        // The root must contain the file.
        assert!(r("/h/proj/a", "/h/proj/b/c.html").is_err());
    }

    #[test]
    fn a_file_whose_bound_is_home_or_above_is_single_file() {
        let home = p("/h/u");
        let file = p("/h/u/evil.html");
        // In no project: its own directory is the home.
        assert_eq!(
            resolve_mount(&home, &file, &[], Some(&home)),
            Ok(MountScope::SingleFile(file.clone()))
        );
        // A project at home bounds nothing: same.
        assert_eq!(
            resolve_mount(&home, &file, &[home.clone()], Some(&home)),
            Ok(MountScope::SingleFile(file.clone()))
        );
        // A project above home (`/h`) bounds nothing either.
        assert_eq!(
            resolve_mount(&home, &file, &[p("/h")], Some(&home)),
            Ok(MountScope::SingleFile(file.clone()))
        );
        // Above the file's own directory is still refused, not widened.
        assert!(resolve_mount(Path::new("/h"), &file, &[], Some(&home)).is_err());
        assert!(resolve_mount(Path::new("/"), &file, &[], Some(&home)).is_err());
        // A file directly under `/` or a top-level directory.
        assert_eq!(
            resolve_mount(Path::new("/"), Path::new("/x.html"), &[], None),
            Ok(MountScope::SingleFile(p("/x.html")))
        );
        assert_eq!(
            resolve_mount(Path::new("/tmp"), Path::new("/tmp/x.html"), &[], None),
            Ok(MountScope::SingleFile(p("/tmp/x.html")))
        );
        // `/` is never a project.
        assert!(resolve_mount(Path::new("/"), Path::new("/x/a.html"), &[p("/")], None).is_err());
        // A page below home in no project keeps a tree of its own directory.
        assert_eq!(
            resolve_mount(
                Path::new("/h/u/docs"),
                Path::new("/h/u/docs/a.html"),
                &[],
                Some(&home)
            ),
            Ok(MountScope::Tree)
        );
    }

    #[test]
    fn sensitive_paths() {
        for s in [
            "/h/.ssh/id_rsa",
            "/h/.ssh",
            "/h/.claude/.credentials.json",
            "/h/proj/sub/.claude/y",
            "/h/proj/.env",
            "/h/proj/.env.local",
            "/h/proj/.ENV.production",
            "/h/.aws/credentials",
            "/h/.gnupg/pubring.kbx",
            "/h/.config/gh/hosts.yml",
            "/h/.netrc",
            "/h/.git-credentials",
            "/h/.docker/config.json",
            "/h/.kube/config",
            "/h/proj/keys/id_ed25519",
            "/h/proj/keys/id_rsa.pub",
            "/h/proj/id_ecdsa_sk",
        ] {
            assert!(is_sensitive_path(Path::new(s)), "{s}");
        }
        for s in [
            "/h/proj/index.html",
            "/h/proj/environment.md",
            "/h/proj/.envrc.md",
            "/h/proj/id_card.png",
            "/h/proj/identity/id.css",
            "/h/proj/.sshfoo/x",
            "/h/proj/config/app.json",
            "/tmp/.tmpAbC123/a.html",
        ] {
            assert!(!is_sensitive_path(Path::new(s)), "{s}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn may_serve_refuses_a_hardlinked_file() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("key");
        std::fs::write(&secret, "FAKE-KEY").unwrap();
        let innocent = dir.path().join("notes.txt");
        std::fs::hard_link(&secret, &innocent).unwrap();
        let plain = dir.path().join("page.html");
        std::fs::write(&plain, "<p>ok</p>").unwrap();
        assert!(!may_serve(&MountScope::Tree, &innocent));
        assert!(!may_serve(
            &MountScope::SingleFile(innocent.clone()),
            &innocent
        ));
        assert!(may_serve(&MountScope::Tree, &plain));
    }

    #[test]
    fn another_users_home_is_broad() {
        let home = p("/home/alice");
        assert!(is_broad_dir(&p("/home/bob"), Some(&home)));
        assert!(is_broad_dir(&p("/home/bob"), None));
        assert!(is_broad_dir(&home, Some(&home)));
        assert!(!is_broad_dir(&p("/home/alice/work/proj"), Some(&home)));
        assert!(!is_broad_dir(&p("/srv/projects/app"), Some(&home)));
    }

    #[test]
    fn may_serve_applies_both_rules() {
        let only = p("/h/u/evil.html");
        let single = MountScope::SingleFile(only.clone());
        assert!(may_serve(&single, &only));
        assert!(!may_serve(&single, Path::new("/h/u/notes.txt")));
        assert!(!may_serve(&single, Path::new("/h/u/.ssh/id_rsa")));
        assert!(may_serve(&MountScope::Tree, Path::new("/h/proj/a.css")));
        assert!(!may_serve(&MountScope::Tree, Path::new("/h/proj/.env")));
        // The previewed file itself being a credential is still a 404.
        assert!(!may_serve(
            &MountScope::SingleFile(p("/h/u/.ssh/id_rsa")),
            Path::new("/h/u/.ssh/id_rsa")
        ));
    }
}
