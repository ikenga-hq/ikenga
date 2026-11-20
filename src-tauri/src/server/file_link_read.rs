//! `GET /internal/file-link/read?path=<stored canonical path>&max=<byte cap>`:
//! the owner's child streams one linked file to the T1 broker (box-files
//! WP-16; 06-file-links-security §1.2, §4.2).
//!
//! Registered only on a principal child, inside `auth_middleware` and
//! outside the app-lock gate, with class `internal`
//! (`access::route_requirement`): only the broker's own request (the
//! per-child bearer, the internal-call header, no caps header) passes. The
//! broker never forwards an outsider's header here and never sends the link
//! token; it sends the canonical path it read from its own root-owned row.
//!
//! The child repeats `file_link_resolve`'s checks 1 to 4 against its own
//! unnarrowed `PathGuard` and adds check 5 (the fresh canonical path must
//! equal the stored one byte for byte) and check 6 (`max`, the broker's
//! per-kind cap, against the open handle's size). The bytes streamed are the
//! bytes of the handle those checks opened: no reopen by path.
//!
//! Every refusal is an empty `404`; the broker turns any non-200 into its
//! uniform "not available" page. The response carries only
//! `Content-Length` and a neutral type: the broker sets every header the
//! outsider sees itself (§1.5) and takes nothing else from here.
//!
//! Not activity-exempt (`server::mod`): a stream in progress keeps the
//! child from idling out.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use super::file_link_resolve;
use super::rpc_shell::PathGuard;
use super::shared::download::file_stream;
use super::AppState;
use crate::access::file_links::LINK_MAX_BYTES;

/// The child-only route (`access::route_requirement` → `internal`).
pub(crate) const READ_PATH: &str = "/internal/file-link/read";

/// Query of [`READ_PATH`]. Both fields are text so a malformed one answers
/// the same empty 404 as every other refusal.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ReadQuery {
    pub path: Option<String>,
    pub max: Option<String>,
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// The route's handler: the account's own guard and home.
pub(crate) async fn handler(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ReadQuery>,
) -> Response {
    read_with(state.path_guard.clone(), None, q).await
}

/// The read, with the guard and (in tests) the home given. `home: None`
/// means "this process's `HOME`" (`file_link_resolve::home_dir`).
pub(crate) async fn read_with(guard: PathGuard, home: Option<PathBuf>, q: ReadQuery) -> Response {
    let Some(path) = q.path.filter(|p| p.starts_with('/') && !p.contains('\0')) else {
        return not_found();
    };
    let Some(max) = q.max.as_deref().and_then(|m| m.parse::<u64>().ok()) else {
        return not_found();
    };
    let max = max.min(LINK_MAX_BYTES);
    let checked = tokio::task::spawn_blocking(move || {
        let home = home.or_else(file_link_resolve::home_dir);
        // Check 5: the stored path is both the input and the expected
        // canonical spelling; anything that resolves elsewhere fails.
        file_link_resolve::check(&guard, home.as_deref(), &path, Some(Path::new(&path)), max)
            .map_err(|r| (r, path))
    })
    .await;
    let linkable = match checked {
        Ok(Ok(l)) => l,
        Ok(Err((refusal, path))) => {
            // The owner's own path in the owner's own log; never a token
            // (this route never receives one).
            tracing::debug!(?refusal, %path, "file link read refused");
            return not_found();
        }
        Err(e) => {
            tracing::warn!("file link read: worker failed: {e}");
            return not_found();
        }
    };
    let size = linkable.size;
    let file = tokio::fs::File::from_std(linkable.file);
    let mut res = axum::body::Body::from_stream(file_stream(file, size)).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::*;

    /// `<tmp>/home` is the home and the whole allowlist, `<tmp>/home/data`
    /// the reserved data dir (the `file_link_resolve` fixture's shape).
    pub(crate) fn guard_for(home: &Path) -> PathGuard {
        let roots_file = home.parent().unwrap().join("roots.json");
        std::fs::write(
            &roots_file,
            serde_json::json!({ "roots": [home.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
        PathGuard::roots(Arc::new(roots)).reserving(crate::server::reserved::Reserved::new(
            Some(home.join("data")),
            vec![],
        ))
    }

    fn q(path: &Path, max: u64) -> ReadQuery {
        ReadQuery {
            path: Some(path.to_string_lossy().into_owned()),
            max: Some(max.to_string()),
        }
    }

    async fn body(res: Response) -> Vec<u8> {
        axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    #[tokio::test]
    async fn streams_the_stored_file_with_its_length_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        std::fs::create_dir_all(home.join("data")).unwrap();
        std::fs::write(home.join("r.pdf"), b"%PDF-1.7 r").unwrap();
        let res = read_with(guard_for(&home), Some(home.clone()), q(&home.join("r.pdf"), 100)).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(res.headers()[header::CONTENT_TYPE], "application/octet-stream");
        assert!(res.headers().get(header::CONTENT_DISPOSITION).is_none());
        assert_eq!(body(res).await, b"%PDF-1.7 r");
    }

    #[tokio::test]
    async fn refuses_a_non_canonical_moved_capped_or_malformed_request() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        std::fs::create_dir_all(home.join("data")).unwrap();
        std::fs::create_dir_all(home.join("proj")).unwrap();
        std::fs::write(home.join("r.pdf"), b"%PDF-1.7 r").unwrap();
        std::fs::write(home.join("s.pdf"), b"%PDF-1.7 s").unwrap();
        let g = || guard_for(&home);
        let h = || Some(home.clone());
        // A `..` spelling resolves, but not to itself: check 5.
        let dotted = home.join("proj/../r.pdf");
        assert_eq!(read_with(g(), h(), q(&dotted, 100)).await.status(), StatusCode::NOT_FOUND);
        // Over the cap: check 6.
        assert_eq!(
            read_with(g(), h(), q(&home.join("r.pdf"), 3)).await.status(),
            StatusCode::NOT_FOUND
        );
        // Replaced by a symlink to a sibling inside the allowlist.
        std::fs::remove_file(home.join("r.pdf")).unwrap();
        std::os::unix::fs::symlink(home.join("s.pdf"), home.join("r.pdf")).unwrap();
        assert_eq!(
            read_with(g(), h(), q(&home.join("r.pdf"), 100)).await.status(),
            StatusCode::NOT_FOUND
        );
        // The daemon's reserved state, a relative path, no max.
        std::fs::write(home.join("data/ikenga.db"), b"db").unwrap();
        assert_eq!(
            read_with(g(), h(), q(&home.join("data/ikenga.db"), 100)).await.status(),
            StatusCode::NOT_FOUND
        );
        let rel = ReadQuery {
            path: Some("s.pdf".into()),
            max: Some("100".into()),
        };
        assert_eq!(read_with(g(), h(), rel).await.status(), StatusCode::NOT_FOUND);
        let no_max = ReadQuery {
            path: Some(home.join("s.pdf").to_string_lossy().into_owned()),
            max: None,
        };
        assert_eq!(read_with(g(), h(), no_max).await.status(), StatusCode::NOT_FOUND);
        let bad_max = ReadQuery {
            path: Some(home.join("s.pdf").to_string_lossy().into_owned()),
            max: Some("-1".into()),
        };
        assert_eq!(read_with(g(), h(), bad_max).await.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_route_is_internal_class() {
        let r = crate::access::route_requirement(READ_PATH).expect("classified");
        assert_eq!(r, crate::access::Requirement::internal());
    }
}
