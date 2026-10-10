//! Viewer commands: register a `(token, root)` mount in the shared
//! viewer-server's registry and return a shell-origin-relative URL prefix.
//! The actual server is bound once at startup (see
//! `ViewerServerManager::start` in `lib.rs`); commands here are just
//! mount-registry edits.
//!
//! `rootDir` and `filePath` are allowlist-checked. `filePath` is the page being
//! previewed: the root the FE asks for is derived from that page's own markup,
//! so it is bounded here by the app's own project registry (see
//! [`crate::viewer_guard`]), exactly as the browser daemon does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use crate::commands::resolve_allowlisted;
use crate::db::PaDb;
use crate::viewer_guard;
use crate::viewer_server::ViewerServerManager;

#[derive(Serialize)]
pub struct ViewerHandle {
    /// Shell-origin-relative URL prefix, e.g. `/__viewer/<token>/`.
    /// FE appends the file path (resolved against the viewer mount root).
    pub url: String,
    /// 32-byte hex token; pass back to `viewer_stop` to release the mount.
    pub token: String,
}

#[tauri::command]
pub async fn viewer_serve(
    manager: State<'_, Arc<ViewerServerManager>>,
    db: State<'_, Arc<PaDb>>,
    #[allow(non_snake_case)] rootDir: String,
    #[allow(non_snake_case)] filePath: String,
) -> Result<ViewerHandle, String> {
    let resolved = resolve_allowlisted(&rootDir).map_err(|e| e.to_string())?;
    if !resolved.is_dir() {
        return Err(format!("not a directory: {}", resolved.display()));
    }
    let file = resolve_allowlisted(&filePath).map_err(|e| e.to_string())?;
    if !file.is_file() {
        return Err(format!("not a file: {}", file.display()));
    }
    let projects = viewer_guard::project_roots(&db).await;
    let home = crate::platform::home_dir().and_then(|h| h.canonicalize().ok());
    let (url, token) = register_checked(&manager, resolved, &file, &projects, home.as_deref())?;
    Ok(ViewerHandle { url, token })
}

/// Bound `root` for the page `file` (both canonical) and register the mount.
/// The part of `viewer_serve` that does not need the Tauri state.
pub(crate) fn register_checked(
    manager: &ViewerServerManager,
    root: PathBuf,
    file: &Path,
    projects: &[PathBuf],
    home: Option<&Path>,
) -> Result<(String, String), String> {
    let scope = viewer_guard::resolve_mount(&root, file, projects, home).inspect_err(|e| {
        tracing::warn!(
            "viewer_serve: refused root {} for {}: {e}",
            root.display(),
            file.display()
        );
    })?;
    Ok(manager.register(root, scope))
}

#[tauri::command]
pub async fn viewer_stop(
    manager: State<'_, Arc<ViewerServerManager>>,
    token: String,
) -> Result<(), String> {
    manager.unregister(&token);
    Ok(())
}

/// Bound port of the shared viewer server. Returned to the FE so dev mode
/// (Vite shell origin) can build absolute URLs when the proxy isn't wired,
/// and so prod (localhost-plugin) can confirm the port matches.
#[tauri::command]
pub async fn viewer_port(
    manager: State<'_, Arc<ViewerServerManager>>,
) -> Result<Option<u16>, String> {
    Ok(manager.bound_port())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::fs;
    use tower::util::ServiceExt;

    async fn status(app: &axum::Router, uri: &str) -> (StatusCode, String) {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let st = resp.status();
        let body = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
        (st, String::from_utf8_lossy(&body).into_owned())
    }

    struct World {
        _dir: tempfile::TempDir,
        home: PathBuf,
    }

    fn world() -> World {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join("proj/pages")).unwrap();
        fs::create_dir_all(home.join("proj/_shared")).unwrap();
        fs::create_dir_all(home.join("proj/.ssh")).unwrap();
        fs::create_dir_all(home.join("proj/sub/.claude")).unwrap();
        fs::write(home.join(".ssh/id_rsa"), "FAKE-PRIVATE-KEY").unwrap();
        fs::write(home.join(".claude/.credentials.json"), "FAKE-CLAUDE-TOKEN").unwrap();
        fs::write(home.join("notes.txt"), "private notes").unwrap();
        fs::write(home.join("evil.html"), "<p>evil</p>").unwrap();
        fs::write(home.join("proj/pages/p.html"), "<p>p</p>").unwrap();
        fs::write(home.join("proj/_shared/t.css"), "a{}").unwrap();
        fs::write(home.join("proj/.env"), "SECRET=1").unwrap();
        fs::write(home.join("proj/.ssh/x"), "SECRET").unwrap();
        fs::write(home.join("proj/sub/.claude/y"), "SECRET").unwrap();
        World { _dir: dir, home }
    }

    fn mount(
        m: &ViewerServerManager,
        root: &Path,
        file: &Path,
        projects: &[PathBuf],
        home: &Path,
    ) -> Result<String, String> {
        register_checked(m, root.to_path_buf(), file, projects, Some(home)).map(|(url, _)| url)
    }

    /// `~/evil.html`, in no project: its own directory is the home, so the
    /// mount is single-file. The page loads; `~/.ssh/id_rsa` (any spelling) and
    /// `~/notes.txt` are 404.
    #[tokio::test]
    async fn desktop_page_in_home_without_project_is_single_file() {
        let w = world();
        let m = ViewerServerManager::new();
        let page = w.home.join("evil.html");
        let url = mount(&m, &w.home, &page, &[], &w.home).unwrap();
        let app = m.viewer_router();
        assert_eq!(
            status(&app, &format!("{url}evil.html")).await.0,
            StatusCode::OK
        );
        for p in [
            ".ssh/id_rsa",
            "%2essh/id_rsa",
            ".ssh%2fid_rsa",
            "notes.txt",
            ".claude/.credentials.json",
            "",
        ] {
            let (st, body) = status(&app, &format!("{url}{p}")).await;
            assert_eq!(st, StatusCode::NOT_FOUND, "{p:?}");
            assert!(
                !body.contains("FAKE-") && !body.contains("private"),
                "{p:?}"
            );
        }
        // The root is never widened above the page's directory.
        let parent = w.home.parent().unwrap();
        assert!(mount(&m, parent, &page, &[], &w.home).is_err());
    }

    /// A project rooted at the home bounds nothing: same single-file mount.
    #[tokio::test]
    async fn desktop_project_at_home_lands_in_single_file_mode() {
        let w = world();
        let m = ViewerServerManager::new();
        let page = w.home.join("evil.html");
        let url = mount(&m, &w.home, &page, &[w.home.clone()], &w.home).unwrap();
        let app = m.viewer_router();
        assert_eq!(
            status(&app, &format!("{url}evil.html")).await.0,
            StatusCode::OK
        );
        for p in [".ssh/id_rsa", "notes.txt"] {
            assert_eq!(
                status(&app, &format!("{url}{p}")).await.0,
                StatusCode::NOT_FOUND,
                "{p}"
            );
        }
        // A page below home keeps a tree bounded by its own directory.
        let sub = w.home.join("proj/pages/p.html");
        assert!(mount(&m, &w.home, &sub, &[w.home.clone()], &w.home).is_err());
        let url = mount(
            &m,
            &w.home.join("proj/pages"),
            &sub,
            &[w.home.clone()],
            &w.home,
        )
        .unwrap();
        assert_eq!(
            status(&m.viewer_router(), &format!("{url}p.html")).await.0,
            StatusCode::OK
        );
    }

    /// A normal project: `../_shared` assets still load from the project
    /// mount; widening to the home is refused; credential paths are 404 inside it.
    #[tokio::test]
    async fn desktop_project_mount_keeps_shared_assets_and_hides_credentials() {
        let w = world();
        let m = ViewerServerManager::new();
        let proj = w.home.join("proj");
        let page = proj.join("pages/p.html");
        let projects = vec![proj.clone()];

        assert!(mount(&m, &w.home, &page, &projects, &w.home).is_err());
        let url = mount(&m, &proj, &page, &projects, &w.home).unwrap();
        let app = m.viewer_router();
        let (st, body) = status(&app, &format!("{url}_shared/t.css")).await;
        assert_eq!((st, body.as_str()), (StatusCode::OK, "a{}"));
        assert_eq!(
            status(&app, &format!("{url}pages/p.html")).await.0,
            StatusCode::OK
        );
        for p in [
            ".env",
            ".ssh/x",
            "sub/.claude/y",
            "../.ssh/id_rsa",
            "%2e%2e/.ssh/id_rsa",
        ] {
            let (st, body) = status(&app, &format!("{url}{p}")).await;
            assert_ne!(st, StatusCode::OK, "{p}");
            assert!(!body.contains("SECRET") && !body.contains("FAKE-"), "{p}");
        }
        assert_eq!(
            status(&app, &format!("{url}.env")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(&app, &format!("{url}sub/.claude/y")).await.0,
            StatusCode::NOT_FOUND
        );
        // The page's own directory is fine too.
        mount(&m, &proj.join("pages"), &page, &projects, &w.home).unwrap();
    }
}
