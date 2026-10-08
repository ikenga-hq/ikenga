//! Authenticated viewer file serving for browser sessions (gap audit rank 8).
//!
//! Mirrors desktop's `ViewerServerManager` (`src-tauri/src/viewer_server/mod.rs`),
//! but integrates into the daemon's own HTTP server and permission system:
//! - Mounts are registered via `viewer_serve` (behind session auth & path allowlist)
//! - Paths are confined to `mount.root` via `safe_join` (no `..`, no symlinks escape)
//! - Paths are checked against the daemon's `PathGuard` allowlist and reserved directories
//! - Content-Security-Policy enforces `sandbox allow-scripts;` (opaque origin, no cookies,
//!   no /api/rpc access)
//! - Supports HTTP Range requests (audio/video seeking) via `tower_http::services::ServeDir`
//! - Injects artifact bridge, tokens stylesheet, and iyke bridge on `text/html` responses
//! - Token-scoped URLs (`/__viewer/:token/*path`)

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use tower::util::ServiceExt;
use tower_http::services::ServeDir;

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
pub const VIEWER_CSP: &str = "sandbox allow-scripts; default-src 'self' data: blob:; \
script-src 'self' 'unsafe-inline' 'unsafe-eval' https://cdn.jsdelivr.net https://cdn.tailwindcss.com https://esm.sh https://cdn.skypack.dev; \
style-src 'self' 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net https://cdn.tailwindcss.com; \
img-src 'self' data: blob: https:; \
font-src 'self' data: https://fonts.gstatic.com https://cdn.jsdelivr.net; \
media-src 'self' blob: https:; \
connect-src 'self'";

/// URL path prefix for viewer routes.
pub const VIEWER_PATH_PREFIX: &str = "/__viewer";

#[derive(Clone, Debug)]
pub struct ViewerMount {
    pub root: PathBuf,
}

#[derive(Clone, Default)]
pub struct ViewerService {
    mounts: Arc<DashMap<String, ViewerMount>>,
}

impl ViewerService {
    pub fn new() -> Self {
        Self {
            mounts: Arc::new(DashMap::new()),
        }
    }

    /// Register a mount root. Under multi-user (T1), `principal_id` is encoded
    /// in the token prefix (`<principal_uuid>_<random_hex>`) so the broker can
    /// route subresource requests to the principal's child even when cookies
    /// are omitted by the sandboxed iframe. Under T0, a 32-byte hex token is minted.
    pub fn register(&self, root: PathBuf, principal_id: Option<uuid::Uuid>) -> (String, String) {
        let token = match principal_id {
            Some(pid) => format!("{}_{}", pid.simple(), random_token_hex(24)),
            None => random_token_hex(32),
        };
        self.mounts
            .insert(token.clone(), ViewerMount { root: root.clone() });
        let url = format!("{VIEWER_PATH_PREFIX}/{token}/");
        tracing::info!(
            "viewer mount: registered {} at {} (token {})",
            root.display(),
            url,
            &token[..8]
        );
        (url, token)
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
        self.mounts.contains_key(token)
    }

    pub fn get_mount(&self, token: &str) -> Option<ViewerMount> {
        self.mounts.get(token).map(|m| m.clone())
    }
}

pub fn random_token_hex(n_bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
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
            Some(c) if c.is_ascii_whitespace() => {
                return html[i..].find('>').map(|j| i + j + 1)
            }
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
    let script = format!("{IYKE_INJECT_MARKER}\n<script type=\"module\">\n{IYKE_BRIDGE_JS}\n</script>\n");
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

    if req.method() != Method::GET && req.method() != Method::HEAD {
        return (StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into_response();
    }

    if refuse_service_worker(req.headers()) {
        return (StatusCode::FORBIDDEN, "forbidden: service worker").into_response();
    }

    // Path verification:
    let Some(canonical) = safe_join(root, rel_path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    // Allowlist & reserved directory verification
    if let Err(e) = state.path_guard.check(&canonical) {
        tracing::warn!("viewer: path_guard check refused {}: {e}", canonical.display());
        return (StatusCode::FORBIDDEN, format!("forbidden: {e}")).into_response();
    }

    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.to_string());

    let uri_str = if rel_path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", rel_path)
    };
    let new_uri = match uri_str.parse() {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad path").into_response(),
    };
    *req.uri_mut() = new_uri;

    let svc = ServeDir::new(root);
    let resp = match svc.oneshot(req).await {
        Ok(r) => r.into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "serve failed").into_response(),
    };

    let (mut parts, body) = resp.into_parts();

    // Headers common to all responses
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    parts
        .headers
        .insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    parts
        .headers
        .insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    parts
        .headers
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));

    let csp_str = if let Some(h) = &host {
        csp_for_host(VIEWER_CSP, h)
    } else {
        VIEWER_CSP.to_string()
    };
    if let Ok(csp_val) = HeaderValue::from_str(&csp_str) {
        parts.headers.insert(header::CONTENT_SECURITY_POLICY, csp_val);
    }

    let is_html = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().starts_with("text/html"))
        .unwrap_or(false);

    // Only inject bridges on 200 OK HTML responses (not partial content / ranges)
    if is_html && parts.status == StatusCode::OK {
        let bytes = match to_bytes(body, HTML_INJECT_MAX_BYTES).await {
            Ok(b) => b,
            Err(_) => return Response::from_parts(parts, Body::empty()),
        };
        match std::str::from_utf8(&bytes) {
            Ok(html) => {
                let with_artifact = inject_artifact_bridge(html);
                let with_all = inject_iyke_bridge(&with_artifact);
                let new_bytes = with_all.into_bytes();
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(new_bytes.len()));
                return Response::from_parts(parts, Body::from(new_bytes));
            }
            Err(_) => {
                parts
                    .headers
                    .insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
                return Response::from_parts(parts, Body::from(bytes));
            }
        }
    }

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
    fn csp_contains_sandbox() {
        assert!(VIEWER_CSP.starts_with("sandbox allow-scripts;"));
        let host_csp = csp_for_host(VIEWER_CSP, "localhost:3000");
        assert!(host_csp.contains("sandbox allow-scripts;"));
        assert!(host_csp.contains("http://localhost:3000"));
    }

    #[test]
    fn bridge_injection_ordering() {
        let sample = "<!doctype html><html><head><title>Test</title></head><body><h1>Hi</h1></body></html>";
        let injected = inject_artifact_bridge(sample);
        assert!(injected.contains(ARTIFACT_INJECT_MARKER));
        assert!(injected.contains("ikenga-tokens"));
        assert!(injected.contains("window.__ikenga_host__"));

        let iyke = inject_iyke_bridge(&injected);
        assert!(iyke.contains(IYKE_INJECT_MARKER));
        assert!(iyke.contains("iframe-bridge"));
    }

    #[tokio::test]
    async fn e2e_viewer_router_serving_and_auth() {
        use axum::http::{header, Request, StatusCode};
        use tower::ServiceExt;
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sub = root.join("assets");
        std::fs::create_dir_all(&sub).unwrap();

        let html_content = "<!doctype html><html><head><title>T</title></head><body><h1>Hello</h1></body></html>";
        std::fs::write(sub.join("index.html"), html_content).unwrap();
        std::fs::write(sub.join("style.css"), "body { color: red; }").unwrap();
        let audio_bytes = vec![0u8; 100];
        std::fs::write(sub.join("test.mp3"), &audio_bytes).unwrap();

        let roots_file = dir.path().join("roots.json");
        std::fs::write(&roots_file, json!({ "roots": [root.to_string_lossy()] }).to_string()).unwrap();
        let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();

        let config = crate::server::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: dir.path().to_path_buf(),
            pkgs_dir: None,
            data_dir: None,
            auth_token: Some("secret-token".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: crate::executor::ExecutorTier::T0,
        };

        let app = crate::server::build_router(
            config,
            std::sync::Arc::new(crate::pty::PtyManager::new()),
            std::sync::Arc::new(crate::engines::EngineRegistry::new()),
            None,
            None,
            None,
            crate::server::rpc_shell::PathGuard::roots(std::sync::Arc::new(roots)),
            None,
            crate::access::DaemonAccess::unavailable(),
            None,
            crate::server::UpdateSource::Default,
        );

        // 1. Call viewer_serve RPC to mount directory
        let rpc_req = Request::builder()
            .method("POST")
            .uri("/api/rpc")
            .header(header::AUTHORIZATION, "Bearer secret-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({
                "cmd": "viewer_serve",
                "args": { "rootDir": sub.to_string_lossy() }
            }).to_string()))
            .unwrap();
        let rpc_resp = app.clone().oneshot(rpc_req).await.unwrap();
        assert_eq!(rpc_resp.status(), StatusCode::OK);
        let rpc_bytes = axum::body::to_bytes(rpc_resp.into_body(), 100_000).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&rpc_bytes).unwrap();
        assert!(val["ok"].as_bool().unwrap_or(false), "RPC failed: {:?}", val["error"]);
        let data = &val["data"];
        let token = data["token"].as_str().unwrap().to_string();
        let url = data["url"].as_str().unwrap().to_string();
        assert_eq!(url, format!("/__viewer/{token}/"));

        // 2. Serve HTML: check 200 OK, CSP sandbox, bridge markers
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/index.html"))
            .header(header::HOST, "localhost:5173")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let csp = resp.headers().get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
        assert!(csp.contains("sandbox allow-scripts"));
        assert!(csp.contains("http://localhost:5173"));
        let body_bytes = axum::body::to_bytes(resp.into_body(), 100_000).await.unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();
        assert!(body_str.contains(ARTIFACT_INJECT_MARKER));
        assert!(body_str.contains(IYKE_INJECT_MARKER));

        // 3. Serve CSS: check 200 OK, no bridge injection, text/css
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/style.css"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/css");
        let body_bytes = axum::body::to_bytes(resp.into_body(), 100_000).await.unwrap();
        assert_eq!(&body_bytes[..], b"body { color: red; }");

        // 4. Serve Audio with Range request: check 206 Partial Content
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/test.mp3"))
            .header(header::RANGE, "bytes=0-9")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(), "bytes 0-9/100");
        let body_bytes = axum::body::to_bytes(resp.into_body(), 100_000).await.unwrap();
        assert_eq!(body_bytes.len(), 10);

        // 5. Refuse Service Worker request
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/sw.js"))
            .header("service-worker", "script")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);

        // 6. Refuse path traversal
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/../secret.txt"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // 7. Stop mount via viewer_stop RPC
        let stop_req = Request::builder()
            .method("POST")
            .uri("/api/rpc")
            .header(header::AUTHORIZATION, "Bearer secret-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({
                "cmd": "viewer_stop",
                "args": { "token": token }
            }).to_string()))
            .unwrap();
        let stop_resp = app.clone().oneshot(stop_req).await.unwrap();
        assert_eq!(stop_resp.status(), StatusCode::OK);

        // Mount is gone -> capability token no longer valid, auth middleware refuses with 401 Unauthorized
        let req = Request::builder()
            .uri(format!("/__viewer/{token}/index.html"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

