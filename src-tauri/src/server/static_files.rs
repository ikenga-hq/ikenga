use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use axum::body::Body;
use axum::http::{header, HeaderMap, Response, StatusCode, Uri};
use flate2::write::GzEncoder;
use flate2::Compression;
use mime_guess::from_path;
use tokio::fs;

/// Below this the gzip header and the extra round of CPU cost more than they save.
const MIN_COMPRESS_BYTES: usize = 1024;

/// Bound on cached compressed bodies. A built `dist/` is a few hundred files.
const MAX_CACHED_BODIES: usize = 1024;

/// Vite writes every content-hashed file under `assets/`, so a hit there can
/// never change for the same URL. Everything else keeps a stable name.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";
const SHORT: &str = "public, max-age=3600";

/// The PWA entry files (plans/pwa W1/W2). Both decide what a returning or
/// installed client runs next, so both always revalidate, and both are served
/// with an explicit type rather than whatever `mime_guess` happens to know.
const SW_SCRIPT: &str = "sw.js";
const WEB_MANIFEST: &str = "manifest.webmanifest";
const SW_MIME: &str = "text/javascript; charset=utf-8";
const MANIFEST_MIME: &str = "application/manifest+json";

/// Sent by a browser on the fetch of a script it is about to install as a
/// service worker.
const SERVICE_WORKER_HEADER: &str = "service-worker";

/// `(path, mtime secs, byte length)`: a rebuilt `dist/` changes the key, so a
/// stale compressed copy is never served.
type CacheKey = (PathBuf, u64, u64);

#[derive(Clone)]
pub struct SpaStaticService {
    pub static_dir: PathBuf,
    gzip_cache: Arc<Mutex<HashMap<CacheKey, Arc<Vec<u8>>>>>,
}

impl SpaStaticService {
    pub fn new(static_dir: impl AsRef<Path>) -> Self {
        Self {
            static_dir: static_dir.as_ref().to_path_buf(),
            gzip_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Serve `uri` with no content negotiation. Kept for callers that have no
    /// request headers to hand; the daemon itself uses [`Self::handle_with`].
    pub async fn handle(&self, uri: Uri) -> Response<Body> {
        self.handle_with(uri, &HeaderMap::new()).await
    }

    pub async fn handle_with(&self, uri: Uri, req_headers: &HeaderMap) -> Response<Body> {
        let path = uri.path().trim_start_matches('/');
        let file_path = self.static_dir.join(path);

        // Security check: ensure path stays inside static_dir
        if let Ok(canonical_static) = self.static_dir.canonicalize() {
            if let Ok(canonical_file) = file_path.canonicalize() {
                if !canonical_file.starts_with(&canonical_static) {
                    return Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(Body::from("Forbidden"))
                        .unwrap();
                }
            }
        }

        let wants_gzip = accepts_gzip(req_headers);

        // Only `/sw.js` may be installed as a service worker. Any other
        // same-origin script (a worker bundle, a stray `.mjs`) registered with
        // scope `/` would control every page of the app, so a browser asking
        // for one is refused before the file is read.
        if is_service_worker_fetch(req_headers) && path != SW_SCRIPT {
            return status_response(StatusCode::FORBIDDEN, "Forbidden");
        }

        // `/sw.js` and `/manifest.webmanifest` never fall back to the SPA: a
        // build without the PWA plugin (the desktop dist) would otherwise
        // hand `index.html` to `navigator.serviceWorker.register`, or to the
        // browser as the app manifest. Missing means 404.
        if path == SW_SCRIPT || path == WEB_MANIFEST {
            if file_path.is_file() {
                if let Ok(bytes) = fs::read(&file_path).await {
                    let mime = if path == SW_SCRIPT {
                        SW_MIME
                    } else {
                        MANIFEST_MIME
                    };
                    // No `Service-Worker-Allowed`: the worker's scope is
                    // capped at `/`, the directory it is served from.
                    return self
                        .respond(&file_path, mime, REVALIDATE, bytes, wants_gzip)
                        .await;
                }
            }
            return status_response(StatusCode::NOT_FOUND, "Not Found");
        }

        // If file exists and is a file, serve it directly
        if file_path.is_file() {
            if let Ok(bytes) = fs::read(&file_path).await {
                let mime = from_path(&file_path).first_or_octet_stream();
                let cache_control = if path.starts_with("assets/") {
                    IMMUTABLE
                } else {
                    SHORT
                };
                return self
                    .respond(&file_path, mime.as_ref(), cache_control, bytes, wants_gzip)
                    .await;
            }
        }

        // Otherwise, serve SPA index.html fallback
        let index_path = self.static_dir.join("index.html");
        if index_path.is_file() {
            if let Ok(bytes) = fs::read(&index_path).await {
                // The entry point names the hashed bundles, so it must always
                // be revalidated or a deploy would never reach a returning tab.
                return self
                    .respond(&index_path, "text/html; charset=utf-8", REVALIDATE, bytes, wants_gzip)
                    .await;
            }
        }

        // If even index.html is missing (e.g. dist/ not built yet), return placeholder
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(
                "<!DOCTYPE html><html><head><title>Ikenga Server</title></head><body><h1>Ikenga Server is running</h1><p>Static assets directory not found. Please build the frontend (`bun run build`) to serve the full React shell.</p></body></html>"
            ))
            .unwrap()
    }

    async fn respond(
        &self,
        file_path: &Path,
        mime: &str,
        cache_control: &'static str,
        bytes: Vec<u8>,
        wants_gzip: bool,
    ) -> Response<Body> {
        let mut builder = Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .header(header::CACHE_CONTROL, cache_control);

        // Vary on every response whose encoding depends on the request,
        // compressed or not, so a shared cache never hands a gzip body to a
        // client that didn't ask for one. Binary types are never negotiated.
        if is_compressible(mime) {
            builder = builder.header(header::VARY, "Accept-Encoding");
        }

        let compressible = is_compressible(mime) && bytes.len() >= MIN_COMPRESS_BYTES;
        if wants_gzip && compressible {
            if let Some(gz) = self.gzip_cached(file_path, bytes.clone()).await {
                builder = builder.header(header::CONTENT_ENCODING, "gzip");
                return builder.body(Body::from(gz.as_ref().clone())).unwrap();
            }
        }
        builder.body(Body::from(bytes)).unwrap()
    }

    /// gzip `bytes`, reusing an earlier result for the same file version.
    /// Compression runs on the blocking pool: a 750 KB bundle takes tens of
    /// milliseconds, which would otherwise stall the async worker.
    async fn gzip_cached(&self, file_path: &Path, bytes: Vec<u8>) -> Option<Arc<Vec<u8>>> {
        let meta = fs::metadata(file_path).await.ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key: CacheKey = (file_path.to_path_buf(), mtime, bytes.len() as u64);

        if let Some(hit) = self.gzip_cache.lock().ok()?.get(&key) {
            return Some(hit.clone());
        }

        let compressed = tokio::task::spawn_blocking(move || {
            let mut enc = GzEncoder::new(Vec::with_capacity(bytes.len() / 3), Compression::new(6));
            enc.write_all(&bytes).ok()?;
            enc.finish().ok()
        })
        .await
        .ok()??;

        let compressed = Arc::new(compressed);
        if let Ok(mut cache) = self.gzip_cache.lock() {
            if cache.len() >= MAX_CACHED_BODIES {
                cache.clear();
            }
            cache.insert(key, compressed.clone());
        }
        Some(compressed)
    }
}

/// True when the browser is fetching this script to install it as a service
/// worker (`Service-Worker: script`).
pub fn is_service_worker_fetch(headers: &HeaderMap) -> bool {
    headers
        .get(SERVICE_WORKER_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("script"))
}

fn status_response(status: StatusCode, text: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, REVALIDATE)
        .body(Body::from(text))
        .expect("static status response is well-formed")
}

/// True when the client lists `gzip` with a non-zero quality.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    value.split(',').any(|part| {
        let mut pieces = part.trim().split(';');
        let coding = pieces.next().unwrap_or("").trim();
        if !(coding.eq_ignore_ascii_case("gzip") || coding == "*") {
            return false;
        }
        // `gzip;q=0` means "not acceptable".
        !pieces.any(|p| {
            let p = p.trim();
            p.strip_prefix("q=")
                .and_then(|q| q.parse::<f32>().ok())
                .is_some_and(|q| q <= 0.0)
        })
    })
}

/// Text-like types compress well; already-compressed media (png, woff2, mp4,
/// wasm) only burns CPU.
fn is_compressible(mime: &str) -> bool {
    let base = mime.split(';').next().unwrap_or("").trim();
    base.starts_with("text/")
        || matches!(
            base,
            "application/javascript"
                | "application/json"
                | "application/xml"
                | "application/manifest+json"
                | "image/svg+xml"
        )
        || base.ends_with("+json")
        || base.ends_with("+xml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn service_with(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, SpaStaticService) {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        let svc = SpaStaticService::new(dir.path());
        (dir, svc)
    }

    fn accept(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, value.parse().unwrap());
        h
    }

    async fn body_bytes(resp: Response<Body>) -> Vec<u8> {
        axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec()
    }

    fn big_js() -> Vec<u8> {
        "export const x = 'the same line over and over';\n"
            .repeat(200)
            .into_bytes()
    }

    #[tokio::test]
    async fn gzips_compressible_assets_when_the_client_accepts_it() {
        let js = big_js();
        let (_d, svc) = service_with(&[("assets/app-abc12345.js", js.clone())]);
        let resp = svc
            .handle_with("/assets/app-abc12345.js".parse().unwrap(), &accept("gzip, br"))
            .await;
        assert_eq!(resp.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(resp.headers()[header::VARY], "Accept-Encoding");
        let body = body_bytes(resp).await;
        assert!(body.len() < js.len() / 4, "expected a large saving");
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut out).unwrap();
        assert_eq!(out, js, "gzip body must round-trip to the original bytes");
    }

    #[tokio::test]
    async fn serves_identity_when_the_client_does_not_accept_gzip() {
        let js = big_js();
        let (_d, svc) = service_with(&[("assets/app-abc12345.js", js.clone())]);
        for headers in [HeaderMap::new(), accept("identity"), accept("gzip;q=0")] {
            let resp = svc
                .handle_with("/assets/app-abc12345.js".parse().unwrap(), &headers)
                .await;
            assert!(resp.headers().get(header::CONTENT_ENCODING).is_none());
            assert_eq!(body_bytes(resp).await, js);
        }
    }

    #[tokio::test]
    async fn never_compresses_binary_or_tiny_files() {
        let png = vec![0x89u8; 4096];
        let tiny = b"{\"a\":1}".to_vec();
        let (_d, svc) = service_with(&[("logo.png", png.clone()), ("tiny.json", tiny.clone())]);
        for (uri, expect) in [("/logo.png", png), ("/tiny.json", tiny)] {
            let resp = svc.handle_with(uri.parse().unwrap(), &accept("gzip")).await;
            assert!(resp.headers().get(header::CONTENT_ENCODING).is_none(), "{uri}");
            assert_eq!(body_bytes(resp).await, expect);
        }
    }

    #[tokio::test]
    async fn hashed_assets_are_immutable_and_the_entry_point_always_revalidates() {
        let (_d, svc) = service_with(&[
            ("index.html", b"<html></html>".to_vec()),
            ("assets/app-abc12345.js", big_js()),
            ("install-catalog.json", b"{}".to_vec()),
        ]);
        let cc = |r: &Response<Body>| r.headers()[header::CACHE_CONTROL].to_str().unwrap().to_string();

        let asset = svc.handle("/assets/app-abc12345.js".parse().unwrap()).await;
        assert_eq!(cc(&asset), IMMUTABLE);
        let index = svc.handle("/".parse().unwrap()).await;
        assert_eq!(cc(&index), REVALIDATE);
        // An unknown route is the SPA fallback, so it is index.html too.
        let deep = svc.handle("/some/client/route".parse().unwrap()).await;
        assert_eq!(cc(&deep), REVALIDATE);
        let stable = svc.handle("/install-catalog.json".parse().unwrap()).await;
        assert_eq!(cc(&stable), SHORT);
    }

    #[tokio::test]
    async fn a_rebuilt_file_is_not_served_from_a_stale_gzip_cache() {
        let (dir, svc) = service_with(&[("assets/app-abc12345.js", big_js())]);
        let uri: Uri = "/assets/app-abc12345.js".parse().unwrap();
        let _ = svc.handle_with(uri.clone(), &accept("gzip")).await;

        let newer = "export const y = 'different content entirely';\n".repeat(150).into_bytes();
        std::fs::write(dir.path().join("assets/app-abc12345.js"), &newer).unwrap();
        let resp = svc.handle_with(uri, &accept("gzip")).await;
        let body = body_bytes(resp).await;
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(&body[..]).read_to_end(&mut out).unwrap();
        assert_eq!(out, newer);
    }

    fn sw_fetch() -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(SERVICE_WORKER_HEADER, "script".parse().unwrap());
        h
    }

    #[tokio::test]
    async fn the_service_worker_is_revalidated_typed_and_never_widens_its_scope() {
        let sw = b"self.addEventListener('fetch',()=>{});".to_vec();
        let (_d, svc) = service_with(&[
            ("index.html", b"<html></html>".to_vec()),
            ("sw.js", sw.clone()),
        ]);
        for headers in [HeaderMap::new(), sw_fetch()] {
            let resp = svc.handle_with("/sw.js".parse().unwrap(), &headers).await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(resp.headers()[header::CACHE_CONTROL], REVALIDATE);
            assert_eq!(resp.headers()[header::CONTENT_TYPE], SW_MIME);
            assert!(
                resp.headers().get("service-worker-allowed").is_none(),
                "the worker must stay scoped to the directory it is served from"
            );
            assert_eq!(body_bytes(resp).await, sw);
        }
    }

    #[tokio::test]
    async fn the_manifest_is_revalidated_with_the_manifest_type() {
        let manifest = br#"{"name":"Ikenga","start_url":"/"}"#.to_vec();
        let (_d, svc) = service_with(&[("manifest.webmanifest", manifest.clone())]);
        let resp = svc.handle("/manifest.webmanifest".parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], REVALIDATE);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], MANIFEST_MIME);
        assert_eq!(body_bytes(resp).await, manifest);
    }

    #[tokio::test]
    async fn a_missing_worker_or_manifest_is_404_not_the_spa_fallback() {
        // The desktop dist has no PWA files. Falling back to index.html here
        // would register HTML as a service worker.
        let (_d, svc) = service_with(&[("index.html", b"<html>spa</html>".to_vec())]);
        for (uri, headers) in [
            ("/sw.js", HeaderMap::new()),
            ("/sw.js", sw_fetch()),
            ("/manifest.webmanifest", HeaderMap::new()),
        ] {
            let resp = svc.handle_with(uri.parse().unwrap(), &headers).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            assert!(!String::from_utf8_lossy(&body_bytes(resp).await).contains("spa"));
        }
    }

    #[tokio::test]
    async fn no_other_script_can_be_installed_as_a_service_worker() {
        let (_d, svc) = service_with(&[
            ("index.html", b"<html></html>".to_vec()),
            ("assets/app-abc12345.js", big_js()),
            ("pdf.worker.min.mjs", b"x".to_vec()),
        ]);
        for uri in [
            "/assets/app-abc12345.js",
            "/pdf.worker.min.mjs",
            "/nope.js",
            "/",
        ] {
            let resp = svc.handle_with(uri.parse().unwrap(), &sw_fetch()).await;
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
            // The same files still load normally.
            let plain = svc.handle(uri.parse().unwrap()).await;
            assert_eq!(plain.status(), StatusCode::OK, "{uri}");
        }
    }

    /// Through the real T0 router: the worker and manifest are fetched by the
    /// browser with no bearer token (a service-worker script fetch never
    /// carries `Authorization`), so they must sit outside the auth layer.
    #[tokio::test]
    async fn the_t0_router_serves_the_pwa_entry_files_without_a_token() {
        use axum::http::Request;
        use tower::ServiceExt;

        let (dir, _svc) = service_with(&[
            ("index.html", b"<html></html>".to_vec()),
            ("sw.js", b"/* sw */".to_vec()),
            ("manifest.webmanifest", b"{}".to_vec()),
        ]);
        let config = crate::server::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: dir.path().to_path_buf(),
            pkgs_dir: None,
            data_dir: None,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: crate::executor::ExecutorTier::T0,
        };
        let router = crate::server::create_router(
            config,
            Arc::new(crate::pty::PtyManager::new()),
            Arc::new(crate::engines::EngineRegistry::new()),
            None,
            None,
        );
        for (uri, mime) in [
            ("/sw.js", SW_MIME),
            ("/manifest.webmanifest", MANIFEST_MIME),
        ] {
            let req = Request::builder()
                .uri(uri)
                .header(SERVICE_WORKER_HEADER, "script")
                .body(Body::empty())
                .unwrap();
            let req = if uri == "/sw.js" {
                req
            } else {
                Request::builder().uri(uri).body(Body::empty()).unwrap()
            };
            let res = router.clone().oneshot(req).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
            assert_eq!(res.headers()[header::CONTENT_TYPE], mime, "{uri}");
            assert_eq!(res.headers()[header::CACHE_CONTROL], REVALIDATE, "{uri}");
            assert!(
                res.headers().get("service-worker-allowed").is_none(),
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn icons_keep_the_short_cache() {
        let (_d, svc) = service_with(&[("icons/icon-192.png", vec![0x89u8; 64])]);
        let resp = svc.handle("/icons/icon-192.png".parse().unwrap()).await;
        assert_eq!(resp.headers()[header::CACHE_CONTROL], SHORT);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/png");
    }

    #[tokio::test]
    async fn path_traversal_is_still_refused() {
        let (_d, svc) = service_with(&[("index.html", b"<html></html>".to_vec())]);
        let resp = svc
            .handle_with("/../../../../etc/passwd".parse().unwrap(), &accept("gzip"))
            .await;
        // Either refused outright or folded into the SPA fallback; never the real file.
        let body = body_bytes(resp).await;
        assert!(!String::from_utf8_lossy(&body).contains("root:"));
    }
}
