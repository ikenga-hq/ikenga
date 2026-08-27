//! Content server for installed packages' iframe UI.
//!
//! One axum process bound to `127.0.0.1:0` at app boot. Routes:
//!
//! ```text
//! GET /<pkg_id>/<token>/<file>     → serves <install_path>/dist/<file>
//! GET /<pkg_id>/<token>/           → serves <install_path>/dist/index.html
//! ```
//!
//! The token is per-iframe-mount (not per-pkg). `mint()` is called when a
//! `<PkgIframeHost>` mounts; `revoke()` on unmount; `revoke_pkg()` on
//! uninstall. Untokenized requests get 404 — the port is still discoverable
//! but the secret is not.
//!
//! Per-request CSP and Permission-Policy headers are built from the
//! manifest's optional `ui.csp` / `ui.permissions` blocks, layered over a
//! default-deny baseline. The server itself implements `pkg::Registry` so
//! the kernel's install/uninstall lifecycle drives content registration:
//! a pkg with no `ui.routes` of `kind: "iframe"` is a no-op here.
//!
//! Why a single server, not one per pkg: simpler lifecycle, fewer ports,
//! and the per-token check + per-pkg path prefix already give us isolation.
//! Mirrors the design of `viewer_server` next door but adds pkg awareness.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context, Result};
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use dashmap::DashMap;
use serde_json::{json, Value};
use tower::util::ServiceExt;
use tower_http::services::ServeDir;

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;
// Lifted to the headless core so `server::pkg_static` (the daemon's browser-side
// mount path) shares this exact implementation instead of copying it. Imported
// at module scope so `mod tests`' `use super::*` still resolves them.
use crate::pkg_html::{find_case_insensitive, inject_base_href, inline_subresources};

/// Default CSP — directive → space-separated source list. Manifest entries
/// in `ui.csp` replace the value for a directive (not append). Unset
/// directives fall back to these defaults.
const DEFAULT_CSP: &[(&str, &str)] = &[
    ("default-src", "'self'"),
    ("script-src", "'self' 'unsafe-inline'"),
    ("style-src", "'self' 'unsafe-inline'"),
    ("img-src", "'self' data: blob:"),
    ("font-src", "'self' data:"),
    ("connect-src", "'self'"),
    ("frame-ancestors", "'self'"),
    ("base-uri", "'self'"),
];

#[derive(Clone)]
struct PkgEntry {
    /// Absolute path to `<install_path>/dist`.
    dist_root: PathBuf,
    /// `ui.csp` overrides from the manifest. Directive → source list.
    csp_overrides: HashMap<String, Vec<String>>,
    /// `ui.permissions` overrides → Permission-Policy directives.
    perm_overrides: HashMap<String, Vec<String>>,
    /// Whether the pkg declares `capabilities.supabase` and what it expects.
    /// `None` = no declaration; the pkg never sees URL/anon-key. `Some(true)`
    /// = required (mint fails if vault keys missing). `Some(false)` = opt-in,
    /// missing keys surface as null.
    supabase_required: Option<bool>,
    /// Logical DB name when the pkg declared `capabilities.sqlite` (api ≥ 2).
    /// `None` = pkg didn't declare the capability and never sees the db name.
    sqlite_db: Option<String>,
    /// Named-secret injection declaration (ADR-017). `None` = the pkg didn't
    /// declare `capabilities.secrets`. `Some` carries the declared
    /// `name → vault_key` mappings PLUS the pkg's `permissions["vault.keys"]`
    /// allowlist so the resolver (`pkg_content_html`) can enforce the read
    /// scope without re-loading the manifest. Resolution is trust-gated at
    /// mount; this struct is just the static declaration.
    secrets: Option<SecretsCapabilityEntry>,
}

/// The pkg's `capabilities.secrets` declaration captured at register time,
/// paired with its `permissions["vault.keys"]` allowlist. Held verbatim so
/// `pkg_content_html` can resolve + scope-check each named secret without a
/// manifest reload. The vault keys never reach the iframe.
#[derive(Clone)]
pub struct SecretsCapabilityEntry {
    /// `(name, vault_key, required, format)` for each declared secret. `name`
    /// is what the iframe sees in `hostContext.secrets`; `vault_key` is the
    /// Stronghold key the shell resolves (never exposed).
    pub declarations: Vec<NamedSecretDecl>,
    /// `permissions["vault.keys"]` globs — the read-scope allowlist. Each
    /// declared `vault_key` MUST match one of these or resolution rejects it
    /// as an authoring bug.
    pub vault_keys: Vec<String>,
}

/// One resolved-shape named-secret declaration (mirror of
/// `manifest::NamedSecret`, decoupled from the manifest struct so the content
/// server owns a stable internal type).
#[derive(Clone)]
pub struct NamedSecretDecl {
    pub name: String,
    pub vault_key: String,
    pub required: bool,
    pub format: Option<String>,
}

#[derive(Clone)]
struct TokenEntry {
    pkg_id: String,
}

pub struct PkgContentServer {
    /// pkg_id → registered content entry (set on `register`, removed on
    /// `unregister`). Packages without an iframe-kind ui route are absent.
    pkgs: DashMap<String, PkgEntry>,

    /// token → which pkg it grants access to. Minted by `mint()`, dropped by
    /// `revoke()` / `revoke_pkg()`. A request must present a token whose
    /// `pkg_id` matches the URL's pkg segment.
    tokens: DashMap<String, TokenEntry>,

    /// pkg_id → list of live tokens, so uninstall can drop them all.
    tokens_by_pkg: DashMap<String, Vec<String>>,

    /// Set after `start()` succeeds. `mint()` errors if the server hasn't
    /// bound yet (would be a setup-order bug).
    bound_addr: OnceLock<SocketAddr>,
}

impl PkgContentServer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pkgs: DashMap::new(),
            tokens: DashMap::new(),
            tokens_by_pkg: DashMap::new(),
            bound_addr: OnceLock::new(),
        })
    }

    /// Bind on an ephemeral port and spawn the server task. Idempotent —
    /// second call is a no-op.
    pub async fn start(self: Arc<Self>) -> Result<SocketAddr> {
        if let Some(addr) = self.bound_addr.get() {
            return Ok(*addr);
        }
        let app = Router::new()
            .route("/:pkg_id/:token/", any(serve_index))
            .route("/:pkg_id/:token", any(serve_index))
            .route("/:pkg_id/:token/*path", any(serve_path))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .context("bind pkg_content listener")?;
        let addr = listener.local_addr().context("local_addr")?;
        self.bound_addr
            .set(addr)
            .map_err(|_| anyhow!("pkg_content server already started"))?;
        log::info!("pkg_content server: bound at http://{}", addr);
        tokio::spawn(async move {
            let server = axum::serve(listener, app.into_make_service());
            if let Err(e) = server.await {
                log::warn!("pkg_content server exited: {e}");
            }
        });
        Ok(addr)
    }

    /// Mint a per-iframe access token for `pkg_id`. Errors if the pkg isn't
    /// registered (no iframe-kind route declared) or the server isn't
    /// running. Returns a fully-qualified URL ending in `/` plus the token.
    pub fn mint(&self, pkg_id: &str) -> Result<MintedHandle> {
        let addr = self
            .bound_addr
            .get()
            .ok_or_else(|| anyhow!("pkg_content server not started"))?;
        if !self.pkgs.contains_key(pkg_id) {
            return Err(anyhow!("pkg `{pkg_id}` has no iframe content registered"));
        }
        let token = random_token_hex(32);
        self.tokens.insert(
            token.clone(),
            TokenEntry {
                pkg_id: pkg_id.to_string(),
            },
        );
        self.tokens_by_pkg
            .entry(pkg_id.to_string())
            .or_default()
            .push(token.clone());
        let url = format!("http://{}/{}/{}/", addr, pkg_id, token);
        Ok(MintedHandle { url, token })
    }

    /// Capability declaration for a registered pkg's supabase block. Returns
    /// `None` if the pkg isn't registered or didn't declare the capability;
    /// `Some(required)` when it did.
    pub fn supabase_capability(&self, pkg_id: &str) -> Option<bool> {
        self.pkgs.get(pkg_id).and_then(|e| e.supabase_required)
    }

    /// Logical DB name for a registered pkg that declared `capabilities.sqlite`.
    /// Returns `None` if the pkg isn't registered or didn't declare the capability.
    pub fn sqlite_capability(&self, pkg_id: &str) -> Option<String> {
        self.pkgs.get(pkg_id).and_then(|e| e.sqlite_db.clone())
    }

    /// Named-secret injection declaration (ADR-017) for a registered pkg, paired
    /// with its `vault.keys` allowlist. Returns `None` if the pkg isn't
    /// registered or didn't declare `capabilities.secrets`. The actual resolve
    /// + trust gate happens in `pkg_content_html`; this only surfaces the static
    /// declaration. The vault keys never leave the host.
    pub fn secrets_capability(&self, pkg_id: &str) -> Option<SecretsCapabilityEntry> {
        self.pkgs.get(pkg_id).and_then(|e| e.secrets.clone())
    }

    /// Revoke a single token (iframe unmount).
    pub fn revoke(&self, token: &str) {
        if let Some((_, entry)) = self.tokens.remove(token) {
            if let Some(mut list) = self.tokens_by_pkg.get_mut(&entry.pkg_id) {
                list.retain(|t| t != token);
            }
        }
    }

    /// Revoke every token for a pkg. Called on uninstall.
    fn revoke_pkg(&self, pkg_id: &str) {
        if let Some((_, tokens)) = self.tokens_by_pkg.remove(pkg_id) {
            for t in tokens {
                self.tokens.remove(&t);
            }
        }
    }

    /// Mint a token, read the named html file from the pkg's dist/, and
    /// inject a `<base href>` pointing at the per-token subresource URL.
    /// Used by the iframe host's `srcdoc=` path — see the Linux/WebKitGTK
    /// note in `commands/pkg_content.rs`.
    pub fn mint_html(&self, pkg_id: &str, source: &str) -> Result<MintedHtml> {
        let entry = self
            .pkgs
            .get(pkg_id)
            .ok_or_else(|| anyhow!("pkg `{pkg_id}` has no iframe content registered"))?
            .clone();
        // Strip leading `dist/` (manifest convention) and any leading slash so
        // we always read relative to dist_root.
        let rel = source
            .trim_start_matches('/')
            .strip_prefix("dist/")
            .unwrap_or_else(|| source.trim_start_matches('/'));
        let abs = entry.dist_root.join(rel);
        // Defense in depth: confirm the resolved path is still within
        // dist_root (no `../` traversal).
        let canon_root = entry
            .dist_root
            .canonicalize()
            .with_context(|| format!("canonicalize dist_root {}", entry.dist_root.display()))?;
        let canon_abs = abs
            .canonicalize()
            .with_context(|| format!("canonicalize html path {}", abs.display()))?;
        if !canon_abs.starts_with(&canon_root) {
            return Err(anyhow!("source `{source}` resolves outside dist_root"));
        }
        let raw = std::fs::read_to_string(&canon_abs)
            .with_context(|| format!("read {}", canon_abs.display()))?;

        let MintedHandle {
            url: base_url,
            token,
        } = self.mint(pkg_id)?;
        // Three-step rewrite:
        //   1. inline relative <script src> + <link rel="stylesheet" href>
        //      bodies into the HTML. WebKitGTK silently drops loopback
        //      subresource fetches from `about:srcdoc` documents (Tauri
        //      #12767 territory), so the only reliable way to ship the
        //      bundle into the iframe is to embed it in the document
        //      itself. Dynamic imports (which we cannot inline) still go
        //      via the axum content server using absolutized URLs.
        //   2. inject `<base href>` for any consumers that DO honour it.
        //   3. absolutize remaining relative `src=` / `href=` URLs (images,
        //      fonts, dynamic-import targets the bundler emitted as URLs).
        // Relative subresource URLs (`./assets/x.js`) in the served HTML
        // resolve against the HTML file's OWN directory, not the dist root.
        // This matters when a route's `source` is nested (e.g.
        // `dist/sub/index.html` referencing `./assets/x.js` at
        // `dist/sub/assets/x.js`). Resolve against the file's parent dir; keep
        // `canon_root` as the traversal boundary so `../` can never escape the
        // pkg's dist root.
        let resource_base = canon_abs.parent().unwrap_or(canon_root.as_path());
        let html = inline_subresources(&raw, resource_base, &canon_root);
        // `<base href>` + absolutization must point at the served file's own
        // subdirectory, not the token root — same class of bug as the inliner.
        // For a top-level `dist/index.html` `resource_base == canon_root`, so
        // `content_base == base_url` (unchanged). For a nested
        // `dist/sub/index.html`, non-inlined relative URLs (images, fonts,
        // dynamic-import targets) resolve against `<base>/sub/…` and reach the
        // axum server at `dist/sub/…` where the files actually live.
        let content_base = match resource_base.strip_prefix(&canon_root) {
            Ok(sub) if !sub.as_os_str().is_empty() => {
                format!("{}{}/", base_url, sub.to_string_lossy().replace('\\', "/"))
            }
            _ => base_url.clone(),
        };
        let html = inject_base_href(&html, &content_base);
        let html = inject_error_capture(&html);
        let html = absolutize_relative_urls(&html, &content_base);
        Ok(MintedHtml {
            html,
            base_url,
            token,
        })
    }
}

/// Rewrite `src="./..."` / `href="./..."` and bare relative paths
/// (`src="assets/..."`, `href="assets/..."`) to absolute `<base_url>` paths.
/// Defensive — `<base href>` SHOULD handle this, but WebKitGTK ignores `<base>`
/// in `srcdoc` documents on Linux, so we do the resolution ourselves.
///
/// Pure-string transform; we don't parse HTML. Only rewrites attributes whose
/// value starts with `./` or doesn't start with `/`, `http`, `data:`, `blob:`,
/// or `#` — i.e. clearly-relative subresource paths. Absolute URLs are left
/// alone, as are anchors and data URIs.
fn absolutize_relative_urls(html: &str, base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let mut out = String::with_capacity(html.len() + 256);
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        // Skip past any `<script>...</script>` or `<style>...</style>` block —
        // their contents are bundle bodies (post-inlining) and contain
        // arbitrary `src=`/`href=` literals we must not touch.
        if let Some((skip_to, prefix_len)) = next_skip_block(rest) {
            // skip_to is the index in `rest` of the opening `<` of script/style.
            // We process bytes [0..skip_to] for attribute rewriting, then copy
            // [skip_to..skip_to+prefix_len] (the whole tag + body + close) verbatim.
            let chunk = &rest[..skip_to];
            absolutize_chunk(chunk, base, &mut out);
            out.push_str(&rest[skip_to..skip_to + prefix_len]);
            i += skip_to + prefix_len;
            continue;
        }
        // No more skip blocks — process the rest normally and we're done.
        absolutize_chunk(rest, base, &mut out);
        break;
    }
    out
}

/// If `s` contains a `<script` or `<style>` opener, return (start, total_len)
/// where `total_len` covers the opening tag, body, and matching close tag.
/// Returns None if neither is present.
fn next_skip_block(s: &str) -> Option<(usize, usize)> {
    let lower = s.to_ascii_lowercase();
    let s_idx = lower.find("<script");
    let st_idx = lower.find("<style");
    let (start, close_tag) = match (s_idx, st_idx) {
        (Some(a), Some(b)) => {
            if a <= b {
                (a, "</script>")
            } else {
                (b, "</style>")
            }
        }
        (Some(a), None) => (a, "</script>"),
        (None, Some(b)) => (b, "</style>"),
        (None, None) => return None,
    };
    // Find end of opening tag (must close with `>` for it to count). For a
    // self-closing void use we'd not have a body; bail if no `</closeTag>`.
    let after_open_rel = lower[start..].find('>')?;
    let body_start = start + after_open_rel + 1;
    let close_rel = lower[body_start..].find(close_tag)?;
    let close_end = body_start + close_rel + close_tag.len();
    Some((start, close_end - start))
}

fn absolutize_chunk(html: &str, base: &str, out: &mut String) {
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let attr_start = rest
            .find("src=\"")
            .or_else(|| rest.find("src='"))
            .or_else(|| rest.find("href=\""))
            .or_else(|| rest.find("href='"))
            .or_else(|| rest.find("SRC=\""))
            .or_else(|| rest.find("HREF=\""));
        let attr_idx = match attr_start {
            Some(p) => p,
            None => {
                out.push_str(rest);
                break;
            }
        };
        // Copy everything up to and including the opening quote.
        let attr_open = attr_idx
            + rest[attr_idx..]
                .find(|c| c == '"' || c == '\'')
                .map(|p| p + 1)
                .unwrap_or(0);
        out.push_str(&rest[..attr_open]);
        let quote = rest.as_bytes()[attr_open - 1] as char;
        let val_start = attr_open;
        let val_end = rest[val_start..]
            .find(quote)
            .map(|p| val_start + p)
            .unwrap_or(rest.len());
        let value = &rest[val_start..val_end];
        let should_rewrite = !(value.is_empty()
            || value.starts_with('/')
            || value.starts_with('#')
            || value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("data:")
            || value.starts_with("blob:")
            || value.starts_with("about:")
            || value.starts_with("mailto:")
            || value.starts_with("javascript:"));
        if should_rewrite {
            let trimmed = value.trim_start_matches("./");
            out.push_str(base);
            out.push('/');
            out.push_str(trimmed);
        } else {
            out.push_str(value);
        }
        // Advance past the value (caller's loop will pick up the closing quote
        // and onward).
        i += val_end;
    }
}

pub struct MintedHandle {
    pub url: String,
    pub token: String,
}

pub struct MintedHtml {
    /// Full HTML body with `<base href>` injected. Pass to iframe `srcdoc`.
    pub html: String,
    /// Subresource base URL — `http://127.0.0.1:<port>/<pkg_id>/<token>/`.
    /// Exposed for diagnostics; the iframe shouldn't need it directly because
    /// `<base href>` is already in the HTML.
    pub base_url: String,
    /// Per-iframe token — pass to `revoke()` on unmount.
    pub token: String,
}

/// Inject a tiny error-capture script as the FIRST element inside `<head>`,
/// before the inlined bundle. Stashes uncaught errors + rejections on
/// `window.__pkgErrors` so the parent can read them post-load. Diagnostic-only
/// for the iframe smoke debug; safe to keep in production (cheap, no side
/// effects beyond the global array).
fn inject_error_capture(html: &str) -> String {
    let snippet = "<script>window.__pkgErrors=[];addEventListener('error',function(e){window.__pkgErrors.push({type:'error',msg:e.message,filename:e.filename,line:e.lineno,col:e.colno,stack:e.error&&e.error.stack});});addEventListener('unhandledrejection',function(e){window.__pkgErrors.push({type:'unhandledrejection',reason:String(e.reason),stack:e.reason&&e.reason.stack});});</script>";
    if let Some(idx) = find_case_insensitive(html, "<head>") {
        let insert_at = idx + "<head>".len();
        let mut out = String::with_capacity(html.len() + snippet.len());
        out.push_str(&html[..insert_at]);
        out.push_str(snippet);
        out.push_str(&html[insert_at..]);
        out
    } else {
        format!("{}{}", snippet, html)
    }
}

impl Registry for PkgContentServer {
    fn name(&self) -> &'static str {
        "pkg_content"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        // Only register packages that declare at least one iframe-kind route.
        // Component-kind routes are advisory (host-builtin marker installs)
        // and don't need content serving.
        let ui = match &pkg.manifest.ui {
            Some(b) => b,
            None => return Ok(()),
        };
        let has_iframe = ui.routes.iter().any(|r| r.kind == "iframe");
        if !has_iframe {
            return Ok(());
        }
        let dist_root = pkg.install_path.join("dist");
        // Don't error on a missing dist dir — tolerate marker-style installs
        // for fixtures that haven't shipped a bundle yet. mint() will surface
        // the failure when an iframe actually tries to mount.
        if !dist_root.is_dir() {
            log::info!(
                "[pkg_content] {} declares iframe routes but has no dist/ at {}",
                pkg.manifest.id,
                dist_root.display()
            );
        }
        let csp_overrides = ui.csp.clone().unwrap_or_default();
        let perm_overrides = ui.permissions.clone().unwrap_or_default();
        let supabase_required = pkg
            .manifest
            .capabilities
            .as_ref()
            .and_then(|c| c.supabase.as_ref())
            .map(|s| s.required);
        let sqlite_db = pkg
            .manifest
            .capabilities
            .as_ref()
            .and_then(|c| c.sqlite.as_ref())
            .map(|s| s.db.clone());
        // Named-secret injection declaration (ADR-017). Capture the declared
        // name→vault_key mappings + the pkg's vault.keys allowlist so the
        // resolver can scope-check each read without a manifest reload. A pkg
        // that declares the cap but has zero declarations registers as `None`
        // (nothing to inject).
        let secrets = pkg
            .manifest
            .capabilities
            .as_ref()
            .and_then(|c| c.secrets.as_ref())
            .filter(|s| !s.declarations.is_empty())
            .map(|s| SecretsCapabilityEntry {
                declarations: s
                    .declarations
                    .iter()
                    .map(|d| NamedSecretDecl {
                        name: d.name.clone(),
                        vault_key: d.vault_key.clone(),
                        required: d.required,
                        format: d.format.clone(),
                    })
                    .collect(),
                vault_keys: pkg.manifest.permissions.vault_keys.clone(),
            });
        self.pkgs.insert(
            pkg.manifest.id.clone(),
            PkgEntry {
                dist_root,
                csp_overrides,
                perm_overrides,
                supabase_required,
                sqlite_db,
                secrets,
            },
        );
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        self.pkgs.remove(pkg_id);
        self.revoke_pkg(pkg_id);
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let pkgs: Vec<Value> = self
            .pkgs
            .iter()
            .map(|e| {
                json!({
                    "pkg_id": e.key(),
                    "dist_root": e.dist_root.display().to_string(),
                    "dist_exists": e.dist_root.is_dir(),
                    "csp_overrides": e.csp_overrides,
                    "perm_overrides": e.perm_overrides,
                })
            })
            .collect();
        json!({
            "bound_addr": self.bound_addr.get().map(|a| a.to_string()),
            "pkg_count": pkgs.len(),
            "active_tokens": self.tokens.len(),
            "pkgs": pkgs,
        })
    }
}

async fn serve_index(
    State(server): State<Arc<PkgContentServer>>,
    AxumPath((pkg_id, token)): AxumPath<(String, String)>,
    req: Request<Body>,
) -> Response {
    serve_with_token(server, &pkg_id, &token, "index.html", req).await
}

async fn serve_path(
    State(server): State<Arc<PkgContentServer>>,
    AxumPath((pkg_id, token, path)): AxumPath<(String, String, String)>,
    req: Request<Body>,
) -> Response {
    serve_with_token(server, &pkg_id, &token, &path, req).await
}

async fn serve_with_token(
    server: Arc<PkgContentServer>,
    pkg_id: &str,
    token: &str,
    rel_path: &str,
    mut req: Request<Body>,
) -> Response {
    let token_entry = match server.tokens.get(token) {
        Some(e) => e.clone(),
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    if token_entry.pkg_id != pkg_id {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let pkg_entry = match server.pkgs.get(pkg_id) {
        Some(e) => e.clone(),
        None => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    if !pkg_entry.dist_root.is_dir() {
        return (StatusCode::NOT_FOUND, "pkg has no dist/").into_response();
    }

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

    let svc = ServeDir::new(&pkg_entry.dist_root);
    let mut resp = match svc.oneshot(req).await {
        Ok(r) => r.into_response(),
    };
    let csp = build_csp(&pkg_entry.csp_overrides);
    if let Ok(v) = HeaderValue::from_str(&csp) {
        resp.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, v);
    }
    let perm_policy = build_permission_policy(&pkg_entry.perm_overrides);
    if !perm_policy.is_empty() {
        if let Ok(v) = HeaderValue::from_str(&perm_policy) {
            resp.headers_mut()
                .insert(HeaderName::from_static("permissions-policy"), v);
        }
    }
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // CORS: subresources are fetched cross-origin from the srcdoc iframe
    // (which inherits the parent's tauri:// origin) to this http loopback
    // server. ESM module imports require CORS headers; non-module
    // subresources work even without it. We allow `*` because the per-token
    // path already gates access — without the token the request 404s before
    // any content is served.
    resp.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    resp
}

fn build_csp(overrides: &HashMap<String, Vec<String>>) -> String {
    let mut directives: Vec<(String, String)> = DEFAULT_CSP
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    for (k, v) in overrides {
        let merged = v.join(" ");
        if let Some(existing) = directives.iter_mut().find(|(d, _)| d == k) {
            existing.1 = merged;
        } else {
            directives.push((k.clone(), merged));
        }
    }
    directives
        .into_iter()
        .map(|(k, v)| format!("{} {}", k, v))
        .collect::<Vec<_>>()
        .join("; ")
}

fn build_permission_policy(overrides: &HashMap<String, Vec<String>>) -> String {
    overrides
        .iter()
        .map(|(directive, sources)| {
            if sources.is_empty() {
                format!("{}=()", directive)
            } else {
                format!("{}=({})", directive, sources.join(" "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn random_token_hex(n_bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[allow(dead_code)]
fn touch(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_csp_uses_defaults_when_no_overrides() {
        let csp = build_csp(&HashMap::new());
        assert!(csp.contains("default-src 'self'"));
        assert!(csp.contains("script-src 'self' 'unsafe-inline'"));
    }

    #[test]
    fn build_csp_overrides_replace_directive() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "script-src".to_string(),
            vec!["'self'".to_string(), "https://cdn.example.com".to_string()],
        );
        let csp = build_csp(&overrides);
        assert!(csp.contains("script-src 'self' https://cdn.example.com"));
        // default-src untouched
        assert!(csp.contains("default-src 'self'"));
    }

    #[test]
    fn build_csp_adds_unknown_directive() {
        let mut overrides = HashMap::new();
        overrides.insert("worker-src".to_string(), vec!["'self'".to_string()]);
        let csp = build_csp(&overrides);
        assert!(csp.contains("worker-src 'self'"));
    }

    #[test]
    fn permission_policy_empty_sources_blocks() {
        let mut overrides = HashMap::new();
        overrides.insert("camera".to_string(), Vec::new());
        let pp = build_permission_policy(&overrides);
        assert_eq!(pp, "camera=()");
    }

    #[test]
    fn permission_policy_with_sources() {
        let mut overrides = HashMap::new();
        overrides.insert("clipboard-read".to_string(), vec!["'self'".to_string()]);
        let pp = build_permission_policy(&overrides);
        assert_eq!(pp, "clipboard-read=('self')");
    }

    // ── subresource inlining (Finding A regression) ────────────────────────
    // A route whose `source` is nested one level deep (`dist/sub/index.html`)
    // references `./assets/app.js`, which resolves against the HTML file's OWN
    // directory (`dist/sub`), NOT the dist root. Before the fix the inliner
    // joined against the dist root (`dist/assets/app.js` — nonexistent) and the
    // asset silently failed to inline.
    #[test]
    fn inline_subresources_resolves_nested_index_against_own_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_root = tmp.path().canonicalize().unwrap();
        let sub = dist_root.join("sub");
        std::fs::create_dir_all(sub.join("assets")).unwrap();
        std::fs::write(sub.join("assets/app.js"), "console.log('nested-ok');").unwrap();
        std::fs::write(sub.join("assets/styles.css"), "body{color:red}").unwrap();

        let html = "<!doctype html><html><head>\n\
            <script type=\"module\" src=\"./assets/app.js\"></script>\n\
            <link rel=\"stylesheet\" href=\"./assets/styles.css\">\n\
            </head><body></body></html>";
        let out = inline_subresources(html, &sub, &dist_root);

        assert!(
            out.contains("console.log('nested-ok');"),
            "nested script body should inline: {out}"
        );
        assert!(
            !out.contains("src=\"./assets/app.js\""),
            "external script ref should be gone: {out}"
        );
        assert!(
            out.contains("<script type=\"module\">"),
            "module type attribute should be preserved: {out}"
        );
        assert!(out.contains("body{color:red}"), "nested stylesheet should inline: {out}");
        assert!(
            !out.contains("href=\"./assets/styles.css\""),
            "external stylesheet ref should be gone: {out}"
        );
    }

    // A top-level `dist/index.html` (resource_base == dist_root) still inlines —
    // proves the two-arg change didn't regress the common case.
    #[test]
    fn inline_subresources_top_level_index_still_inlines() {
        let tmp = tempfile::tempdir().unwrap();
        let dist_root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(dist_root.join("assets")).unwrap();
        std::fs::write(dist_root.join("assets/app.js"), "console.log('top-ok');").unwrap();

        let html =
            "<html><head><script type=\"module\" src=\"./assets/app.js\"></script></head></html>";
        let out = inline_subresources(html, &dist_root, &dist_root);
        assert!(out.contains("console.log('top-ok');"), "top-level asset should inline: {out}");
    }

    // Traversal guard stays anchored at `dist_root`: a nested index referencing
    // a path that climbs OUT of the pkg (`../../outside.js`) is rejected — the
    // external ref is left untouched, never inlined.
    #[test]
    fn inline_subresources_rejects_escape_outside_dist_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        // Layout: <tmp>/outside.js  and  <tmp>/dist/sub/index.html
        std::fs::write(root.join("outside.js"), "SECRET").unwrap();
        let dist_root = root.join("dist");
        let sub = dist_root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let html = "<html><head>\
            <script type=\"module\" src=\"../../outside.js\"></script></head></html>";
        let out = inline_subresources(html, &sub, &dist_root);
        assert!(!out.contains("SECRET"), "escaping subresource must NOT inline: {out}");
        assert!(
            out.contains("src=\"../../outside.js\""),
            "rejected ref should be left untouched: {out}"
        );
    }
}
