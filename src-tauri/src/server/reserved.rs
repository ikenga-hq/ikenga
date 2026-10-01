//! The daemon's own state, which no caller path may reach — whatever the
//! operator's fs allowlist says.
//!
//! The served fs arms check a caller's path against `fs_roots` (the
//! operator's allowlist). An operator whose allowlist covers `--data-dir` —
//! roots `["/"]`, `["~"]` with the data dir under home, `/opt/ikenga` — would
//! otherwise hand every token holder the daemon's own state: `fs_roots.json`
//! (widen the allowlist on the next restart), `ikenga.db`,
//! `actions-trust.json`, `supabase.json` (the service-role key),
//! `daemon.json` (the daemon token), `backups/`, `chi-cache/`,
//! `pin-screenshots/`. The per-user discovery file
//! ([`super::discovery::user_temp_path`]) carries the token too.
//!
//! [`Reserved`] names those paths; `rpc_shell::PathGuard` consults it after
//! every allowlist check, so every arm that resolves a caller path through
//! the guard — `fs_read` / `fs_write` / `fs_list` / `fs_mkdir` / `fs_exists`,
//! `fs_kind` / `fs_mime` / `fs_search` / `fs_rename`, the project-root and
//! claude-config arms, the actions `RootGuard`, the atelier and
//! `action_git_branch` arms, `/ws/fs` watches — refuses
//! them. The desktop (`commands::fs` via `resolve_allowlisted`) does not use
//! this; its `app_data_dir` is its own business.
//!
//! **What is refused.** The data dir itself and anything inside it; the
//! discovery file, exactly, plus the `.<name>.<uuid>.tmp` siblings
//! `discovery::write_private` stages it through (they hold the token for a
//! moment). The data dir's *entry* in its parent is not hidden: `fs_list` of
//! the parent still shows the name (as it shows every other entry), but
//! nothing can list, read, write, stat or watch inside it. `fs_search` skips
//! the data dir entirely — neither matched nor descended — because a result
//! is something the UI would open, and every arm refuses it.
//!
//! **How paths are compared.** Both sides are canonical: the caller's path by
//! the guard (the nearest existing ancestor canonicalized, a missing tail
//! re-attached), the data dir here, fresh on every check — so a data dir
//! given through a symlink, or created after the router, compares by its real
//! location. On Unix an existing ancestor of the caller's path is also
//! compared with the data dir by `(dev, inode)`, which catches aliases a
//! string compare cannot: a bind mount of the data dir inside the allowlist,
//! or a case-folded spelling on a case-insensitive filesystem.

use std::path::{Path, PathBuf};

pub(crate) const INSIDE_DATA_DIR: &str = "path is inside the daemon's data directory";
pub(crate) const DISCOVERY_FILE: &str = "path is the daemon's discovery file";

/// The paths no caller may reach. Held by the router's `PathGuard`.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reserved {
    /// `--data-dir`, as the operator gave it (absolute or not).
    data_dir: Option<PathBuf>,
    /// Files refused exactly (with their staging siblings): the per-user
    /// discovery file.
    files: Vec<PathBuf>,
}

impl Reserved {
    pub(crate) fn new(data_dir: Option<PathBuf>, files: Vec<PathBuf>) -> Self {
        Self { data_dir, files }
    }

    /// The reserved set resolved now. Taken once per check (or once per
    /// `fs_search` walk), never cached across requests: the data dir may be
    /// created, or re-pointed through a symlink, after the router is built.
    pub(crate) fn snapshot(&self) -> Snapshot {
        let data_dir = self
            .data_dir
            .as_deref()
            .map(|raw| match canonical_lenient(raw) {
                Some(path) => {
                    let id = std::fs::metadata(&path).ok().and_then(|m| file_id(&m));
                    DataDir { path, id }
                }
                // Unresolvable (a relative path with no cwd, or a `..` past its
                // existing part): compare by the literal spelling rather than
                // silently disable the check.
                None => DataDir {
                    path: raw.to_path_buf(),
                    id: None,
                },
            });
        let files = self
            .files
            .iter()
            .filter_map(|f| canonical_lenient(f))
            .collect();
        Snapshot { data_dir, files }
    }
}

struct DataDir {
    path: PathBuf,
    /// `(dev, inode)` when the data dir exists (Unix only).
    id: Option<(u64, u64)>,
}

/// [`Reserved`], resolved at one moment.
pub(crate) struct Snapshot {
    data_dir: Option<DataDir>,
    files: Vec<PathBuf>,
}

impl Snapshot {
    /// Why `path` (canonical, or a canonical ancestor plus a missing tail) is
    /// reserved, or `None` when it is not.
    pub(crate) fn reason(&self, path: &Path) -> Option<&'static str> {
        if let Some(dd) = &self.data_dir {
            if path.starts_with(&dd.path) {
                return Some(INSIDE_DATA_DIR);
            }
            if let Some(id) = dd.id {
                // Every existing ancestor, by identity: a bind mount or a
                // case-folded spelling of the data dir has another path but
                // the same inode.
                let mut cur = Some(path);
                while let Some(p) = cur {
                    if let Ok(meta) = std::fs::metadata(p) {
                        if meta.is_dir() && file_id(&meta) == Some(id) {
                            return Some(INSIDE_DATA_DIR);
                        }
                    }
                    cur = p.parent();
                }
            }
        }
        if self.files.iter().any(|f| is_file_or_staging(path, f)) {
            return Some(DISCOVERY_FILE);
        }
        None
    }

    /// The cheap per-entry form of [`Self::reason`] for a directory walk
    /// rooted at a canonical, non-reserved root that never follows symlinks:
    /// every entry path is then canonical, so a prefix compare is exact, and
    /// only a directory entry can be an inode alias of the data dir. `meta`
    /// is the entry's own (`lstat`) metadata.
    pub(crate) fn skips_entry(&self, path: &Path, meta: Option<&std::fs::Metadata>) -> bool {
        if let Some(dd) = &self.data_dir {
            if path.starts_with(&dd.path) {
                return true;
            }
            if let (Some(id), Some(meta)) = (dd.id, meta) {
                if meta.is_dir() && file_id(meta) == Some(id) {
                    return true;
                }
            }
        }
        self.files.iter().any(|f| is_file_or_staging(path, f))
    }
}

/// Why an absolute `path` that could not be canonicalized (its parent is
/// missing) would be reserved once it existed: its nearest existing ancestor
/// canonicalized, the missing tail re-attached, then [`Snapshot::reason`].
/// `None` when it would not be, or the tail holds `..`.
pub(crate) fn lenient_reason(reserved: &Reserved, path: &Path) -> Option<&'static str> {
    reserved.snapshot().reason(&canonical_lenient(path)?)
}

/// `path` is `file`, or one of the `.<name>.<uuid>.tmp` siblings
/// `discovery::write_private` writes before renaming over it.
fn is_file_or_staging(path: &Path, file: &Path) -> bool {
    if path == file {
        return true;
    }
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return false;
    };
    if path.parent() != Some(dir) {
        return false;
    }
    let Some(candidate) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let prefix = format!(".{}.", name.to_string_lossy());
    candidate.starts_with(&prefix) && candidate.ends_with(".tmp")
}

/// `path` made absolute and canonicalized as far as it exists: the nearest
/// existing ancestor canonicalized, the missing tail re-attached. `None` only
/// when a relative path has no cwd to resolve against, or it contains `..`
/// past its existing part (which cannot be re-attached safely).
fn canonical_lenient(path: &Path) -> Option<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut ancestor = abs.as_path();
    loop {
        if let Ok(c) = ancestor.canonicalize() {
            let tail = abs.strip_prefix(ancestor).ok()?;
            if tail
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return None;
            }
            return Some(c.join(tail));
        }
        ancestor = ancestor.parent()?;
    }
}

/// Whether `path` — a canonical ancestor plus a missing tail — reaches
/// through a symlink that did not canonicalize away: the first component the
/// filesystem knows (`lstat`) is a link, so a write would follow it wherever
/// it points, a path the guard never checked. In practice a dangling link
/// (a live one is resolved by canonicalization), which is exactly how a
/// not-yet-existing file inside the data dir could be aliased from outside.
pub(crate) fn through_unresolved_symlink(path: &Path) -> bool {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            return meta.file_type().is_symlink();
        }
        cur = p.parent();
    }
    false
}

/// `(dev, inode)` on Unix; `None` elsewhere (the canonical-path compare is
/// then the whole check).
fn file_id(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_siblings_of_the_discovery_file_are_reserved() {
        let f = Path::new("/run/user/1/ikenga-daemon.json");
        assert!(is_file_or_staging(f, f));
        assert!(is_file_or_staging(
            Path::new("/run/user/1/.ikenga-daemon.json.0123abcd.tmp"),
            f
        ));
        assert!(!is_file_or_staging(
            Path::new("/run/user/1/ikenga-daemon.json.bak"),
            f
        ));
        assert!(!is_file_or_staging(
            Path::new("/run/user/1/sub/.ikenga-daemon.json.x.tmp"),
            f
        ));
        assert!(!is_file_or_staging(Path::new("/run/user/1/other.json"), f));
    }

    #[test]
    fn a_data_dir_that_does_not_exist_yet_is_still_reserved() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let data = root.join("later/data");
        let snap = Reserved::new(Some(data.clone()), vec![]).snapshot();
        assert_eq!(snap.reason(&data.join("x")), Some(INSIDE_DATA_DIR));
        assert_eq!(snap.reason(&root.join("later/other")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_data_dir_given_through_a_symlink_compares_by_its_real_location() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let snap = Reserved::new(Some(root.join("alias")), vec![]).snapshot();
        assert_eq!(
            snap.reason(&root.join("real/fs_roots.json")),
            Some(INSIDE_DATA_DIR)
        );
        assert_eq!(snap.reason(&root.join("realish")), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_link_in_the_tail_is_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        assert!(through_unresolved_symlink(&root.join("dangling")));
        assert!(through_unresolved_symlink(&root.join("dangling/a/b")));
        assert!(!through_unresolved_symlink(&root.join("missing/a")));
        assert!(!through_unresolved_symlink(&root));
    }
}

#[cfg(test)]
mod router_tests {
    //! House pattern (see `rpc_shell`'s / `rpc_files`' tests): a literal
    //! `ServerConfig` → the router → `oneshot` POST `/api/rpc` with the bearer
    //! token. The allowlist is a local root set that CONTAINS the data dir —
    //! the operator misconfiguration this module exists for — so every
    //! refusal below comes from the reserved check, not the allowlist.

    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::Router;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::{Reserved, DISCOVERY_FILE, INSIDE_DATA_DIR};
    use crate::db::PaDb;
    use crate::engines::EngineRegistry;
    use crate::executor::ExecutorTier;
    use crate::pty::PtyManager;
    use crate::server::rpc_shell::PathGuard;
    use crate::server::{router_with, ServerConfig};

    fn config(data_dir: PathBuf) -> ServerConfig {
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
            data_dir: Some(data_dir),
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        }
    }

    fn roots_of(tmp: &Path, root: &Path) -> PathGuard {
        let file = tmp.join("roots.json");
        std::fs::write(&file, json!({ "roots": [s(root)] }).to_string()).unwrap();
        PathGuard::roots(Arc::new(crate::fs_roots::FsRoots::load(file).unwrap()))
    }

    /// `root/` is the whole allowlist; `root/data/` is `--data-dir`, seeded
    /// with the files a daemon keeps there; `root/sibling.txt` is ordinary.
    struct Daemon {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        data: PathBuf,
        router: Router,
    }

    fn daemon() -> Daemon {
        daemon_with(|root| root.join("data"))
    }

    /// `data_dir_as_given(root)` is how the operator spells `--data-dir`;
    /// the real directory is always `root/data`.
    fn daemon_with(data_dir_as_given: impl Fn(&Path) -> PathBuf) -> Daemon {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("root");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(data.join("fs_roots.json"), r#"{"roots":["/"]}"#).unwrap();
        std::fs::write(data.join("supabase.json"), r#"{"service_role_key":"k"}"#).unwrap();
        std::fs::write(data.join("daemon.json"), r#"{"token":"tok"}"#).unwrap();
        std::fs::write(root.join("sibling.txt"), b"hello").unwrap();
        let given = data_dir_as_given(&root);
        let router = router_with(
            config(given),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            // Not in the data dir: `untouched` pins its exact contents.
            Some(Arc::new(PaDb::new(base.join("ikenga.db")))),
            None,
            None,
            roots_of(&base, &root),
        );
        Daemon {
            _tmp: tmp,
            root,
            data,
            router,
        }
    }

    async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/rpc")
                    .header("authorization", "Bearer tok")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
        res.get("data").cloned().unwrap_or(Value::Null)
    }

    async fn err(router: &Router, cmd: &str, args: Value) -> String {
        let res = rpc(router, cmd, args.clone()).await;
        assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
        res["error"].as_str().unwrap().to_string()
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    /// Refused as the daemon's data dir by every path arm that errors, and
    /// folded into `"missing"` by `fs_kind` (the desktop's contract).
    async fn refused_everywhere(r: &Router, path: &str) {
        for (cmd, args) in [
            ("fs_read", json!({ "path": path })),
            ("fs_write", json!({ "path": path, "content": "pwned" })),
            ("fs_mkdir", json!({ "path": path })),
            ("fs_list", json!({ "path": path })),
            ("fs_exists", json!({ "path": path })),
            ("fs_mime", json!({ "path": path })),
            ("fs_rename", json!({ "from": path, "toName": "moved" })),
        ] {
            let e = err(r, cmd, args).await;
            assert!(e.contains(INSIDE_DATA_DIR), "{cmd} {path}: {e}");
        }
        assert_eq!(ok(r, "fs_kind", json!({ "path": path })).await, "missing");
    }

    /// The data dir holds exactly what the fixture put there, unchanged.
    fn untouched(d: &Daemon) {
        assert_eq!(
            std::fs::read_to_string(d.data.join("fs_roots.json")).unwrap(),
            r#"{"roots":["/"]}"#
        );
        let mut names: Vec<String> = std::fs::read_dir(&d.data)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["daemon.json", "fs_roots.json", "supabase.json"],
            "nothing may be created, moved or removed in the data dir"
        );
    }

    #[tokio::test]
    async fn every_fs_arm_refuses_the_data_dir_and_its_contents() {
        let d = daemon();
        let r = &d.router;
        for p in [
            d.data.clone(),
            d.data.join("fs_roots.json"),
            d.data.join("supabase.json"),
            d.data.join("daemon.json"),
            d.data.join("x"),
            d.data.join("deep/new/chain"),
            d.data.join("backups/b.db"),
        ] {
            refused_everywhere(r, &s(&p)).await;
        }
        untouched(&d);
    }

    #[tokio::test]
    async fn siblings_inside_the_same_root_are_still_served() {
        let d = daemon();
        let r = &d.router;
        let sib = s(&d.root.join("sibling.txt"));
        // The desktop's FileReadResult: the bytes of "hello", and a MIME.
        let read = ok(r, "fs_read", json!({ "path": sib })).await;
        assert_eq!(read["bytes"], json!([104, 101, 108, 108, 111]));
        assert_eq!(ok(r, "fs_exists", json!({ "path": sib })).await, true);
        assert_eq!(ok(r, "fs_kind", json!({ "path": sib })).await, "file");
        assert_eq!(ok(r, "fs_mime", json!({ "path": sib })).await, "text/plain");
        let new = s(&d.root.join("new.txt"));
        ok(r, "fs_write", json!({ "path": new, "content": "n" })).await;
        ok(r, "fs_mkdir", json!({ "path": s(&d.root.join("a/b/c")) })).await;
        // A name that merely starts like the data dir is not inside it.
        let lookalike = s(&d.root.join("data-notes.txt"));
        ok(r, "fs_write", json!({ "path": lookalike, "content": "n" })).await;
        let renamed = ok(
            r,
            "fs_rename",
            json!({ "from": new, "toName": "renamed.txt" }),
        )
        .await;
        assert_eq!(renamed, s(&d.root.join("renamed.txt")));

        // Listing the parent shows the data dir's *entry* (a name, like
        // every other entry) — only its contents are refused.
        let listed = ok(r, "fs_list", json!({ "path": s(&d.root) })).await;
        let names: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"data"), "{names:?}");
        assert!(names.contains(&"sibling.txt"), "{names:?}");
        untouched(&d);
    }

    #[tokio::test]
    async fn fs_rename_cannot_land_on_the_data_dir() {
        let d = daemon();
        let r = &d.router;
        let from = s(&d.root.join("sibling.txt"));
        let e = err(r, "fs_rename", json!({ "from": from, "toName": "data" })).await;
        assert!(e.contains(INSIDE_DATA_DIR), "{e}");
        assert!(d.root.join("sibling.txt").exists());
        untouched(&d);
    }

    #[tokio::test]
    async fn fs_search_from_an_ancestor_never_returns_data_dir_paths() {
        let d = daemon();
        let r = &d.router;
        std::fs::write(d.root.join("fs_roots.json.bak"), b"").unwrap();
        std::fs::write(d.root.join("sub/daemon.json"), b"").unwrap();
        for query in ["json", "fs_roots", "data", "daemon", "supabase"] {
            let res = ok(
                r,
                "fs_search",
                json!({ "root": s(&d.root), "query": query, "showHidden": true, "showIgnored": true }),
            )
            .await;
            for m in res["matches"].as_array().unwrap() {
                let m = Path::new(m.as_str().unwrap());
                assert!(!m.starts_with(&d.data), "{query}: {}", m.display());
            }
        }
        let res = ok(
            r,
            "fs_search",
            json!({ "root": s(&d.root), "query": "json", "showHidden": false, "showIgnored": false }),
        )
        .await;
        let mut got: Vec<String> = res["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                s(&d.root.join("fs_roots.json.bak")),
                s(&d.root.join("sub/daemon.json"))
            ]
        );
        // A search rooted at the data dir is refused outright.
        let e = err(
            r,
            "fs_search",
            json!({ "root": s(&d.data), "query": "json", "showHidden": true, "showIgnored": true }),
        )
        .await;
        assert!(e.contains(INSIDE_DATA_DIR), "{e}");
    }

    #[tokio::test]
    async fn dot_dot_into_the_data_dir_is_refused() {
        let d = daemon();
        let r = &d.router;
        // The prefix exists: canonicalized, then refused as the data dir.
        refused_everywhere(r, &format!("{}/sub/../data/fs_roots.json", s(&d.root))).await;
        refused_everywhere(r, &format!("{}/sub/../data/x", s(&d.root))).await;
        // A missing component before the `..`: refused before any walk.
        for cmd in ["fs_write", "fs_mkdir"] {
            let path = format!("{}/nope/../data/z", s(&d.root));
            let e = err(r, cmd, json!({ "path": path, "content": "x" })).await;
            assert!(e.contains("may not contain `..`"), "{cmd}: {e}");
        }
        untouched(&d);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_alias_of_the_data_dir_is_refused() {
        let d = daemon();
        let r = &d.router;
        std::os::unix::fs::symlink(&d.data, d.root.join("alias")).unwrap();
        std::os::unix::fs::symlink(&d.data, d.root.join("sub/deeper-alias")).unwrap();
        for p in [
            d.root.join("alias"),
            d.root.join("alias/fs_roots.json"),
            d.root.join("alias/x"),
            d.root.join("alias/new/chain"),
            d.root.join("sub/deeper-alias/daemon.json"),
        ] {
            refused_everywhere(r, &s(&p)).await;
        }
        // The link is an entry of `root/` and may match by name, but it is
        // never descended — nothing under it comes back.
        let res = ok(
            r,
            "fs_search",
            json!({ "root": s(&d.root), "query": "json", "showHidden": true, "showIgnored": true }),
        )
        .await;
        for m in res["matches"].as_array().unwrap() {
            let m = m.as_str().unwrap();
            assert!(!m.contains("alias/"), "{m}");
        }
        untouched(&d);
    }

    /// A dangling link to a not-yet-existing path in the data dir: a write
    /// would follow it, so it is refused before anything is created.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_link_into_the_data_dir_is_refused() {
        let d = daemon();
        let r = &d.router;
        std::os::unix::fs::symlink(d.data.join("planted"), d.root.join("dangle")).unwrap();
        std::os::unix::fs::symlink(d.data.join("newdir"), d.root.join("dangle-dir")).unwrap();
        for (cmd, path) in [
            ("fs_write", d.root.join("dangle")),
            ("fs_write", d.root.join("dangle-dir/f")),
            ("fs_mkdir", d.root.join("dangle-dir")),
            ("fs_mkdir", d.root.join("dangle-dir/a/b")),
        ] {
            let e = err(r, cmd, json!({ "path": s(&path), "content": "x" })).await;
            assert!(e.contains("symlink that does not resolve"), "{cmd}: {e}");
        }
        untouched(&d);
    }

    /// `--data-dir` given through a symlink: its real location is refused.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_data_dir_given_through_a_symlink_is_refused_by_its_real_path() {
        let d = daemon_with(|root| {
            let link = root.parent().unwrap().join("data-link");
            std::os::unix::fs::symlink(root.join("data"), &link).unwrap();
            link
        });
        refused_everywhere(&d.router, &s(&d.data.join("fs_roots.json"))).await;
        refused_everywhere(&d.router, &s(&d.data.join("x"))).await;
        untouched(&d);
    }

    #[tokio::test]
    async fn a_project_root_inside_the_data_dir_is_refused() {
        let d = daemon();
        for (cmd, args) in [
            (
                "project_create",
                json!({ "id": "p", "displayName": "P", "rootPath": s(&d.data) }),
            ),
            ("project_scaffold_claude", json!({ "rootPath": s(&d.data) })),
        ] {
            let e = err(&d.router, cmd, args).await;
            assert!(e.contains(INSIDE_DATA_DIR), "{cmd}: {e}");
        }
        untouched(&d);
    }

    #[test]
    fn the_actions_root_guard_refuses_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("root");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let guard = roots_of(&base, &root).reserving(Reserved::new(Some(data.clone()), vec![]));
        for p in [data.clone(), data.join("proj/new")] {
            let e = guard.check_maybe_missing(&p).unwrap_err();
            assert!(e.contains(INSIDE_DATA_DIR), "{e}");
        }
        guard.check_maybe_missing(&root.join("proj")).unwrap();
    }

    /// The per-user discovery file carries the daemon token. Read-only arms
    /// only: this is the real path, and a running daemon may own it.
    #[tokio::test]
    async fn the_discovery_file_is_refused() {
        let discovery = crate::server::discovery::user_temp_path();
        let Some(dir) = discovery.parent().and_then(|p| p.canonicalize().ok()) else {
            return; // no temp dir to allowlist on this host
        };
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let router = router_with(
            config(data),
            Arc::new(PtyManager::new()),
            Arc::new(EngineRegistry::new()),
            None,
            None,
            None,
            roots_of(&base, &dir),
        );
        let name = discovery
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let file = s(&dir.join(&name));
        for (cmd, args) in [
            ("fs_read", json!({ "path": file })),
            ("fs_exists", json!({ "path": file })),
            ("fs_mime", json!({ "path": file })),
        ] {
            let e = err(&router, cmd, args).await;
            assert!(e.contains(DISCOVERY_FILE), "{cmd}: {e}");
        }
        assert_eq!(
            ok(&router, "fs_kind", json!({ "path": file })).await,
            "missing"
        );
        // Its staging sibling (`discovery::write_private`) too.
        let staging = s(&dir.join(format!(".{name}.0123abcd.tmp")));
        let e = err(&router, "fs_mime", json!({ "path": staging })).await;
        assert!(e.contains(DISCOVERY_FILE), "{e}");
        // Not the rest of the directory.
        let other = s(&dir.join("ikenga-unrelated-file.md"));
        assert_eq!(
            ok(&router, "fs_mime", json!({ "path": other })).await,
            "text/markdown"
        );
    }
}
