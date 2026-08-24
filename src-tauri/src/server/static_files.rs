use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{header, Response, StatusCode, Uri};
use mime_guess::from_path;
use tokio::fs;

#[derive(Clone)]
pub struct SpaStaticService {
    pub static_dir: PathBuf,
}

impl SpaStaticService {
    pub fn new(static_dir: impl AsRef<Path>) -> Self {
        Self {
            static_dir: static_dir.as_ref().to_path_buf(),
        }
    }

    pub async fn handle(&self, uri: Uri) -> Response<Body> {
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

        // If file exists and is a file, serve it directly
        if file_path.is_file() {
            if let Ok(bytes) = fs::read(&file_path).await {
                let mime = from_path(&file_path).first_or_octet_stream();
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, mime.as_ref())
                    .body(Body::from(bytes))
                    .unwrap();
            }
        }

        // Otherwise, serve SPA index.html fallback
        let index_path = self.static_dir.join("index.html");
        if index_path.is_file() {
            if let Ok(bytes) = fs::read(&index_path).await {
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .body(Body::from(bytes))
                    .unwrap();
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
}
