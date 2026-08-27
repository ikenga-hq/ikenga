//! Serving already-installed pkgs to a browser session. Two surfaces:
//!
//! * `GET /pkgs/:id/*path` — read-only bytes straight off disk.
//! * `PkgStaticService::mint_html` — the daemon's `pkg_content_html`, which
//!   hands `<PkgIframeHost>` a mountable entry document.
//!
//! # What this is not
//!
//! This is **not** the desktop `pkg_content` server and it is not a second
//! kernel. It installs nothing, trusts nothing, spawns nothing, and never
//! writes. At startup it walks `--pkgs-dir`, reads each `manifest.json`, and
//! records `pkg_id → <install_path>/dist` for the pkgs that declare an
//! `iframe` UI route.
//!
//! It also mints no per-mount token, resolves no vault, and builds no CSP.
//! `pkg_content` does all three; the `token` field on [`MintedPkgHtml`] is an
//! opaque handle that exists only because the frontend contract has the field.
//!
//! # Auth, and why `mint_html` inlines
//!
//! Both routes are mounted **inside** the daemon's protected router, so the
//! existing `auth_middleware` (bearer header or `?token=`) gates every
//! request. No second token is minted.
//!
//! The consequence drives the whole design of `mint_html`: a browser attaches
//! neither an `Authorization` header nor the `?token=` query to an iframe's
//! *relative subresource* fetches. `<iframe src="/pkgs/x/">` would get its
//! `index.html`, and then `./assets/app.js` would arrive with no credential
//! and be rejected with 401 — a blank pane. So `mint_html` inlines the entry
//! document's scripts and stylesheets (sharing `crate::pkg_html` with the
//! desktop path, which does the same thing for an unrelated WebKitGTK
//! reason), leaving a bundle that issues no subresource request at all.
//!
//! That covers `index.html` + JS chunk + CSS chunk, which is the shape the
//! pkg templates produce. It does **not** cover what a stylesheet reaches
//! through `url(...)`, `<img src>`, fonts, or dynamic `import()` — those
//! still resolve against `base_url` and still 401. Fixing that properly needs
//! a credential a browser replays on subresources (a cookie), which is a
//! daemon-wide auth change and deliberately out of scope here.
//!
//! # Same-origin caveat
//!
//! Serving pkg HTML from the daemon's own origin puts it in the SPA's origin.
//! A framed pkg document is then same-origin with the shell and can reach
//! `window.parent`. On desktop the pkg content server is a *different* origin
//! (its own `127.0.0.1:<port>`), which is what actually enforces the
//! boundary; CSP cannot substitute for that. Untrusted pkgs should not be
//! served this way until the daemon can hand them a distinct origin.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, Response, StatusCode};
use axum::response::IntoResponse;
use mime_guess::from_path;
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use serde::Serialize;
use tokio::fs;
use tracing::{info, warn};

use super::AppState;
use crate::pkg::manifest::Package;
use crate::pkg_html::{inject_base_href, inline_subresources};

/// Bytes that may not appear verbatim inside one URL path segment. `/` is in
/// the set because a pkg id containing one would otherwise invent a path
/// component; `%` because we are producing the encoded form ourselves.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'%');

/// One serveable pkg: where its bytes live, plus the host capabilities its
/// manifest declared.
///
/// The capability flags are read once, at discovery, so [`PkgStaticService::mint_html`]
/// can refuse a mount it cannot honour without re-reading the manifest off
/// disk on every request.
#[derive(Clone)]
struct PkgEntry {
    /// Canonicalised `<install_path>/dist`.
    dist_root: PathBuf,
    /// `capabilities.supabase.required` — `None` when the block is absent.
    supabase_required: Option<bool>,
    /// Logical names of `capabilities.secrets` declarations marked `required`.
    required_secrets: Vec<String>,
    /// Whether `capabilities.secrets` was declared at all (a pkg with only
    /// optional declarations still gets a warning, not an error).
    declares_secrets: bool,
}

/// What `pkg_content_html` returns to the frontend.
///
/// **The field names are a hand-matched contract** with
/// `PkgContentHtmlHandle` in `src/lib/tauri-cmd.ts`, which is destructured in
/// `pkg-iframe-host.tsx` as `handle.html` / `handle.baseUrl` / `handle.token`.
/// `base_url` therefore MUST serialize as `baseUrl`; snake_case here would
/// leave the host waiting on a `baseUrl` that is forever `undefined`, and
/// `if (!srcDoc || !baseUrl)` renders the loading state forever — a blank
/// pane with no error anywhere.
///
/// `supabase` and `secrets` are **deliberately absent**, not `null`-valued.
/// The frontend reads them as `handle.supabase ?? null`, so an omitted field
/// and an explicit `null` are the same value to it; omitting says the daemon
/// has no vault at all rather than implying it looked and found nothing.
/// `sqlite` is omitted for a different reason — see the note on `mint_html`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MintedPkgHtml {
    /// Entry document with its relative `<script src>` / `<link rel=stylesheet>`
    /// inlined and a `<base href>` injected. Assign to `<iframe srcdoc>`.
    pub html: String,
    /// Where any remaining relative subresource resolves: `/pkgs/<id>/…`.
    pub base_url: String,
    /// Opaque per-mount handle. The frontend hands it back to
    /// `pkg_content_revoke` on unmount; the daemon has nothing to revoke (see
    /// the module docs — there is no per-mount token here, the bearer token
    /// is the whole boundary), so it exists to satisfy the contract and to
    /// give a mount something to be named by in a log.
    pub token: String,
}

/// `pkg_id → its entry`.
///
/// Built once at router construction and immutable thereafter — there is no
/// install path in the daemon, so nothing can add to it at runtime.
#[derive(Clone, Default)]
pub struct PkgStaticService {
    roots: HashMap<String, PkgEntry>,
}

impl PkgStaticService {
    /// Walk `pkgs_dir` and register every pkg that can actually be served.
    ///
    /// Deliberately forgiving: a directory that isn't a pkg, or a pkg whose
    /// manifest doesn't parse, is skipped with a warning rather than failing
    /// the daemon's startup. An operator with one broken pkg still gets a
    /// running server.
    pub fn discover(pkgs_dir: Option<&Path>) -> Self {
        let Some(dir) = pkgs_dir else {
            return Self::default();
        };
        if !dir.is_dir() {
            warn!(
                "--pkgs-dir {} is not a directory; /pkgs/* will serve nothing",
                dir.display()
            );
            return Self::default();
        }

        let mut roots: HashMap<String, PkgEntry> = HashMap::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("--pkgs-dir {} unreadable: {e}", dir.display());
                return Self::default();
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // `.staging-*` / `.backup-*` are the installer's scratch dirs on
            // desktop; they are never a pkg.
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
            {
                continue;
            }
            if !path.join("manifest.json").is_file() {
                continue;
            }

            let pkg = match Package::load(&path) {
                Ok(p) => p,
                Err(e) => {
                    warn!("[pkg_static] skipping {}: {e:#}", path.display());
                    continue;
                }
            };

            // Same rule the desktop content server applies: only `iframe`
            // routes need bytes served. `component` routes are markers.
            let serves_iframe = pkg
                .manifest
                .ui
                .as_ref()
                .is_some_and(|ui| ui.routes.iter().any(|r| r.kind == "iframe"));
            if !serves_iframe {
                continue;
            }

            // Canonicalise once, here. Every later traversal check compares
            // against this, so it must not be a path containing `..` or a
            // symlink itself.
            let dist_root = match path.join("dist").canonicalize() {
                Ok(p) if p.is_dir() => p,
                Ok(p) => {
                    warn!(
                        "[pkg_static] {} has a dist/ that is not a directory ({})",
                        pkg.manifest.id,
                        p.display()
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        "[pkg_static] {} declares iframe routes but has no readable dist/: {e}",
                        pkg.manifest.id
                    );
                    continue;
                }
            };

            if let Some(existing) = roots.get(&pkg.manifest.id) {
                warn!(
                    "[pkg_static] duplicate pkg id {} — keeping {}, ignoring {}",
                    pkg.manifest.id,
                    existing.dist_root.display(),
                    dist_root.display()
                );
                continue;
            }

            let caps = pkg.manifest.capabilities.as_ref();
            let supabase_required = caps.and_then(|c| c.supabase.as_ref()).map(|s| s.required);
            let secrets = caps.and_then(|c| c.secrets.as_ref());
            let required_secrets: Vec<String> = secrets
                .map(|s| {
                    s.declarations
                        .iter()
                        .filter(|d| d.required)
                        .map(|d| d.name.clone())
                        .collect()
                })
                .unwrap_or_default();

            roots.insert(
                pkg.manifest.id.clone(),
                PkgEntry {
                    dist_root,
                    supabase_required,
                    required_secrets,
                    declares_secrets: secrets.is_some(),
                },
            );
        }

        let mut ids: Vec<&str> = roots.keys().map(String::as_str).collect();
        ids.sort_unstable();
        if ids.is_empty() {
            info!(
                "[pkg_static] no serveable pkgs under {} — /pkgs/* will 404",
                dir.display()
            );
        } else {
            info!(
                "[pkg_static] serving {} pkg(s) from {}: {}",
                ids.len(),
                dir.display(),
                ids.join(", ")
            );
        }
        Self { roots }
    }

    /// Ids this service will serve. Used by tests and diagnostics.
    pub fn ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self.roots.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }

    /// Serve `<dist_root>/<rest>`, or `index.html` when `rest` is empty.
    ///
    /// `pkg_id` is only ever a `HashMap` key — it is never joined onto a
    /// path — so a hostile id cannot escape anywhere. `rest` is the part that
    /// touches the filesystem and goes through [`safe_join`].
    pub async fn handle(&self, pkg_id: &str, rest: &str) -> Response<Body> {
        let Some(root) = self.roots.get(pkg_id).map(|e| &e.dist_root) else {
            return not_found();
        };

        let rel = rest.trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };

        let Some(file_path) = safe_join(root, rel) else {
            // 404, not 403: a traversal attempt learns nothing about which
            // paths exist.
            return not_found();
        };
        if !file_path.is_file() {
            return not_found();
        }

        let Ok(bytes) = fs::read(&file_path).await else {
            return not_found();
        };
        let mime = from_path(&file_path).first_or_octet_stream();
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            // The bytes are third-party pkg content; never let a browser
            // re-interpret a .txt as a script.
            .header("x-content-type-options", "nosniff")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| not_found())
    }

    /// The daemon's `pkg_content_html`: hand `<PkgIframeHost>` the entry
    /// document for `pkg_id` so an iframe pkg can mount in a browser session.
    ///
    /// # Why this inlines, when the module docs say a real browser needs none
    /// of the WebKitGTK workaround
    ///
    /// Different bug, same fix. `pkg_content` inlines because WebKitGTK will
    /// not fetch subresources from an `about:srcdoc` document. Here the
    /// document loads fine, but `/pkgs/:id/*` sits **inside** the daemon's
    /// auth layer and a browser attaches neither an `Authorization` header nor
    /// `?token=` to an iframe's *relative subresource* requests — so
    /// `./assets/app.js` would arrive bare and get a 401. Inlining means the
    /// bundle issues no subresource request at all, so there is nothing to
    /// authenticate. It is the only way to serve a normal multi-file Vite
    /// bundle here without minting a second credential the browser will
    /// replay (a cookie), which is a daemon-wide auth change and out of scope.
    ///
    /// What inlining does **not** cover: assets a stylesheet reaches through
    /// `url(...)`, `<img src>`, fonts, and dynamic `import()` targets. Those
    /// still resolve against `base_url` and still hit the 401. A pkg whose
    /// entry document is `index.html` + one JS chunk + one CSS chunk — which
    /// is the shape `docs/pkg-patterns` produces — is fully covered; one that
    /// loads an image at runtime will show a broken image.
    ///
    /// # Capability policy: omit the optional, refuse the required
    ///
    /// There is no vault here (`crate::secrets_env`), so `supabase` and
    /// `secrets` can never be resolved. One rule covers both:
    ///
    /// * declared and **required** → hard error naming the pkg and the
    ///   capability. Its manifest asked the host to refuse rather than mount
    ///   without the value; a daemon that mounted it anyway would produce the
    ///   blank, unexplained pane this is meant to avoid.
    /// * declared and **optional** → the field is omitted and a warning is
    ///   logged. This is exactly what the desktop does when the vault has no
    ///   keys for a non-required Supabase block: `supabase: null`, mount
    ///   proceeds, the pkg falls back.
    ///
    /// `sqlite` is a third case and is omitted unconditionally. `db_query` /
    /// `db_exec` DO work on the daemon (W2), but the frontend's own gate for
    /// `host.dbQuery` (`pkgDeclaresSqlite` in `pkg-iframe-host.tsx`) resolves
    /// the capability through `pkgKernelStatus()` + `pkgPreviewManifest()`,
    /// and the daemon serves neither. So the verb fails closed in the browser
    /// regardless of what this field says, and asserting `sqlite` here would
    /// only claim a capability the caller cannot use.
    ///
    /// # Blocking IO
    ///
    /// The reads are synchronous. This runs once per iframe mount against a
    /// handful of small local files, on the same disk `discover()` already
    /// walked synchronously at startup. Making it async would mean threading
    /// `tokio::fs` through `inline_subresources`, which is shared verbatim
    /// with the desktop path.
    pub fn mint_html(&self, pkg_id: &str, source: &str) -> Result<MintedPkgHtml, String> {
        let Some(entry) = self.roots.get(pkg_id) else {
            let known = self.ids();
            return Err(if known.is_empty() {
                format!(
                    "pkg `{pkg_id}` is not served by this daemon, and neither is any other: \
                     the daemon was started without a usable --pkgs-dir (or nothing under it \
                     declares an iframe route and ships a dist/). Check the [pkg_static] line \
                     in the daemon's startup log."
                )
            } else {
                format!(
                    "pkg `{pkg_id}` is not served by this daemon — it is not under --pkgs-dir, \
                     its manifest did not parse, or it declares no iframe route. Serving: {}",
                    known.join(", ")
                )
            });
        };

        if entry.supabase_required == Some(true) {
            return Err(format!(
                "pkg `{pkg_id}` declares capabilities.supabase with required:true, but the \
                 headless daemon has no Stronghold vault to resolve VITE_SUPABASE_URL / \
                 VITE_SUPABASE_ANON_KEY from — see crate::secrets_env for why there is none. \
                 Refusing the mount: `required` is the manifest asking the host not to run it \
                 half-configured. Run this pkg in the desktop app, or set required:false if it \
                 can work without Supabase."
            ));
        }
        if !entry.required_secrets.is_empty() {
            return Err(format!(
                "pkg `{pkg_id}` declares capabilities.secrets with required declaration(s): {}. \
                 The headless daemon has no vault (crate::secrets_env), so hostContext.secrets \
                 is omitted entirely and those values can never arrive. Refusing rather than \
                 mounting half-configured. Run this pkg in the desktop app.",
                entry.required_secrets.join(", ")
            ));
        }
        if entry.supabase_required == Some(false) {
            warn!(
                "[pkg_static] pkg `{pkg_id}` declares an optional capabilities.supabase; the \
                 daemon has no vault, so hostContext.supabase is omitted and the pkg must use \
                 its own fallback"
            );
        }
        if entry.declares_secrets {
            warn!(
                "[pkg_static] pkg `{pkg_id}` declares capabilities.secrets with no required \
                 entries; the daemon has no vault, so hostContext.secrets is omitted"
            );
        }

        // Manifest convention: `ui.routes[].source` is written `dist/index.html`
        // but `dist_root` already IS that directory. Same trim the desktop
        // `mint_html` does.
        let rel = source.trim_start_matches('/');
        let rel = rel.strip_prefix("dist/").unwrap_or(rel);
        let rel = if rel.is_empty() { "index.html" } else { rel };

        // Same traversal boundary the byte-serving path uses — a `source` is
        // caller-supplied and gets no more trust than a URL.
        let abs = safe_join(&entry.dist_root, rel).ok_or_else(|| {
            format!("pkg `{pkg_id}`: source `{source}` resolves outside the pkg's dist/")
        })?;
        if !abs.is_file() {
            return Err(format!(
                "pkg `{pkg_id}`: source `{source}` is not a file under its dist/"
            ));
        }
        let raw = std::fs::read_to_string(&abs)
            .map_err(|e| format!("pkg `{pkg_id}`: read {}: {e}", abs.display()))?;

        // Relative URLs in the document resolve against the document's OWN
        // directory, not the dist root — this is the nested-`dist/sub/index.html`
        // case `pkg_content` was fixed for. `dist_root` stays the traversal
        // boundary so a `../` can climb out of `sub/` but never out of the pkg.
        let resource_base = abs.parent().unwrap_or(entry.dist_root.as_path());
        let base_url = {
            let enc = utf8_percent_encode(pkg_id, PATH_SEGMENT);
            match resource_base.strip_prefix(&entry.dist_root) {
                Ok(sub) if !sub.as_os_str().is_empty() => {
                    format!("/pkgs/{}/{}/", enc, sub.to_string_lossy().replace('\\', "/"))
                }
                _ => format!("/pkgs/{enc}/"),
            }
        };

        let html = inline_subresources(&raw, resource_base, &entry.dist_root);
        let html = inject_base_href(&html, &base_url);
        let token = uuid::Uuid::new_v4().simple().to_string();
        info!("[pkg_static] mounted `{pkg_id}` ({rel}) at {base_url}");
        Ok(MintedPkgHtml {
            html,
            base_url,
            token,
        })
    }
}

fn not_found() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Not Found"))
        .expect("static 404 response is well-formed")
}

/// Join `rel` under `root`, refusing anything that leaves it.
///
/// Two independent checks, because either alone has a hole:
///
/// 1. **Lexical** — only `Normal` components survive. `..`, a leading `/`,
///    and a Windows drive prefix are all rejected outright, so
///    `/pkgs/x/../../etc/passwd` never reaches the filesystem. (axum has
///    already percent-decoded the path, so `%2e%2e` arrives as `..` and is
///    caught here too.)
/// 2. **Canonical** — the result is canonicalised and must still be under
///    `root`, which catches a symlink *inside* the pkg pointing outside it.
///
/// Note the difference from [`super::static_files::SpaStaticService`]: there,
/// a failing `canonicalize()` silently skips the check. Here it returns
/// `None`. A path we cannot resolve is a path we do not serve.
fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            // `./` is meaningless but harmless.
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

/// `GET /pkgs/:id` — the pkg's `index.html`.
pub async fn pkg_static_root_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(pkg_id): AxumPath<String>,
) -> impl IntoResponse {
    state.pkg_static.handle(&pkg_id, "").await
}

/// `GET /pkgs/:id/*path` — one file out of the pkg's `dist/`.
pub async fn pkg_static_file_handler(
    State(state): State<Arc<AppState>>,
    AxumPath((pkg_id, path)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    state.pkg_static.handle(&pkg_id, &path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a pkg directory that `discover` should accept.
    fn write_pkg(root: &Path, id: &str, with_dist: bool) -> PathBuf {
        write_pkg_with_caps(root, id, with_dist, None)
    }

    /// Same, with an optional `capabilities` block spliced in — the input to
    /// `mint_html`'s omit-the-optional / refuse-the-required policy.
    fn write_pkg_with_caps(
        root: &Path,
        id: &str,
        with_dist: bool,
        capabilities: Option<&str>,
    ) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let caps = capabilities
            .map(|c| format!(r#","capabilities":{c}"#))
            .unwrap_or_default();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","name":"T","version":"0.1.0","ikenga_api":"1"{caps},
                    "ui":{{"routes":[{{"path":"/x","kind":"iframe","source":"dist/index.html"}}]}}}}"#
            ),
        )
        .unwrap();
        if with_dist {
            std::fs::create_dir_all(dir.join("dist")).unwrap();
            std::fs::write(dir.join("dist").join("index.html"), "<h1>hi</h1>").unwrap();
        }
        dir
    }

    #[test]
    fn discover_registers_only_serveable_pkgs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        write_pkg(root, "com.test.good", true);
        // Declares iframe routes but shipped no bundle.
        write_pkg(root, "com.test.nodist", false);
        // Installer scratch dir.
        let staging = root.join(".staging-com.test.good");
        std::fs::create_dir_all(staging.join("dist")).unwrap();
        std::fs::write(staging.join("manifest.json"), "{}").unwrap();
        // A directory that is not a pkg at all.
        std::fs::create_dir_all(root.join("random")).unwrap();
        // A loose file.
        std::fs::write(root.join("README.md"), "nope").unwrap();
        // Unparseable manifest.
        let broken = root.join("com.test.broken");
        std::fs::create_dir_all(broken.join("dist")).unwrap();
        std::fs::write(broken.join("manifest.json"), "{ not json").unwrap();
        // Valid manifest, but no iframe route → nothing to serve.
        let comp = root.join("com.test.component");
        std::fs::create_dir_all(comp.join("dist")).unwrap();
        std::fs::write(
            comp.join("manifest.json"),
            r#"{"id":"com.test.component","name":"C","version":"0.1.0","ikenga_api":"1",
               "ui":{"routes":[{"path":"/x","kind":"component","source":"X"}]}}"#,
        )
        .unwrap();

        let svc = PkgStaticService::discover(Some(root));
        assert_eq!(svc.ids(), vec!["com.test.good"]);
    }

    #[test]
    fn discover_without_pkgs_dir_is_empty() {
        assert!(PkgStaticService::discover(None).ids().is_empty());
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        assert!(PkgStaticService::discover(Some(&missing)).ids().is_empty());
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::write(root.join("index.html"), "ok").unwrap();

        assert!(safe_join(&root, "index.html").is_some());
        assert!(safe_join(&root, "./index.html").is_some());

        // The exact shape named in the security requirement.
        assert!(safe_join(&root, "../../etc/passwd").is_none());
        assert!(safe_join(&root, "..").is_none());
        assert!(safe_join(&root, "a/../../b").is_none());
        assert!(safe_join(&root, "/etc/passwd").is_none());
        // Unresolvable path: we refuse rather than skipping the check the way
        // SpaStaticService does.
        assert!(safe_join(&root, "does-not-exist.js").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn safe_join_rejects_symlink_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dist");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, "s3cret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link.txt")).unwrap();

        // Purely lexical checks pass here — only canonicalisation catches it.
        assert!(safe_join(&root, "link.txt").is_none());
    }

    /// Routing-level, not handler-level. `handle()` passing tells you nothing
    /// about whether a request actually *reaches* it: `/*path` does not match
    /// an empty tail, so `/pkgs/:id/` used to fall through to the SPA
    /// fallback — outside the auth layer — and serve the shell's index.html
    /// unauthenticated. Only a test that goes through `create_router` catches
    /// that class of bug.
    #[tokio::test]
    async fn router_serves_every_url_form_and_requires_the_token() {
        use axum::body::Body as AxumBody;
        use axum::http::Request;
        use tower::ServiceExt;

        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "com.test.good", true);

        let config = crate::server::ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: tmp.path().join("no-spa-here"),
            pkgs_dir: Some(tmp.path().to_path_buf()),
            data_dir: None,
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
        };
        let router = crate::server::create_router(
            config,
            Arc::new(crate::pty::PtyManager::new()),
            Arc::new(crate::engines::EngineRegistry::new()),
            None,
        );

        let get = |uri: &str, token: bool| {
            let mut req = Request::builder().uri(uri);
            if token {
                req = req.header("authorization", "Bearer tok");
            }
            router
                .clone()
                .oneshot(req.body(AxumBody::empty()).unwrap())
        };

        // All three URL forms must land on the pkg, not on the SPA fallback.
        for uri in [
            "/pkgs/com.test.good",
            "/pkgs/com.test.good/",
            "/pkgs/com.test.good/index.html",
        ] {
            let res = get(uri, true).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri} should serve the pkg");
            assert_eq!(
                res.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/html",
                "{uri} served the SPA fallback instead of the pkg"
            );

            // ...and every one of them must be behind the token.
            assert_eq!(
                get(uri, false).await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "{uri} is reachable without the auth token"
            );
        }

        let res = get("/pkgs/com.test.good/../../etc/passwd", true)
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn handle_serves_index_and_404s_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "com.test.good", true);
        std::fs::write(
            tmp.path().join("com.test.good/dist/app.js"),
            "console.log(1)",
        )
        .unwrap();
        let svc = PkgStaticService::discover(Some(tmp.path()));

        let res = svc.handle("com.test.good", "").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html"
        );

        let res = svc.handle("com.test.good", "app.js").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .contains("javascript"));
        assert_eq!(res.headers().get("x-content-type-options").unwrap(), "nosniff");

        // Unknown pkg, unknown file, and traversal all look identical.
        assert_eq!(
            svc.handle("com.test.nope", "index.html").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            svc.handle("com.test.good", "missing.js").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            svc.handle("com.test.good", "../../etc/passwd").await.status(),
            StatusCode::NOT_FOUND
        );
    }

    // ── mint_html (W4) ────────────────────────────────────────────────────

    /// Write a realistic Vite-shaped bundle: entry document plus one JS chunk
    /// and one CSS chunk, both referenced relatively.
    fn write_bundle(dist: &Path, entry_rel: &str) {
        let entry = dist.join(entry_rel);
        std::fs::create_dir_all(entry.parent().unwrap().join("assets")).unwrap();
        std::fs::write(
            entry.parent().unwrap().join("assets/app.js"),
            "console.log('BUNDLE_BODY');",
        )
        .unwrap();
        std::fs::write(
            entry.parent().unwrap().join("assets/app.css"),
            "body{color:STYLE_BODY}",
        )
        .unwrap();
        std::fs::write(
            &entry,
            "<!doctype html><html><head>\
             <link rel=\"stylesheet\" href=\"./assets/app.css\">\
             </head><body>\
             <script type=\"module\" src=\"./assets/app.js\"></script>\
             </body></html>",
        )
        .unwrap();
    }

    /// The whole point of the arm: a normal multi-file bundle comes back as ONE
    /// document with no outstanding subresource request. Every un-inlined
    /// `src=`/`href=` would be a 401 in a browser, because a relative
    /// subresource fetch carries neither the bearer header nor `?token=`.
    #[test]
    fn mint_html_inlines_the_bundle_and_injects_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = write_pkg(tmp.path(), "com.test.good", true);
        write_bundle(&dir.join("dist"), "index.html");
        let svc = PkgStaticService::discover(Some(tmp.path()));

        let out = svc.mint_html("com.test.good", "dist/index.html").expect("mints");

        assert!(out.html.contains("BUNDLE_BODY"), "script not inlined: {}", out.html);
        assert!(out.html.contains("STYLE_BODY"), "stylesheet not inlined: {}", out.html);
        assert!(
            !out.html.contains("./assets/app.js") && !out.html.contains("./assets/app.css"),
            "a subresource ref survived and will 401 in a browser: {}",
            out.html
        );
        assert!(
            out.html.contains("<base href=\"/pkgs/com.test.good/\">"),
            "base href missing: {}",
            out.html
        );
        assert_eq!(out.base_url, "/pkgs/com.test.good/");
        assert_eq!(out.token.len(), 32, "token should be a simple uuid");
    }

    /// `source` is the manifest spelling (`dist/index.html`) but `dist_root`
    /// already IS that directory. Both spellings, and the empty one, must land
    /// on the same document.
    #[test]
    fn mint_html_accepts_every_source_spelling() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "com.test.good", true);
        let svc = PkgStaticService::discover(Some(tmp.path()));

        for source in ["dist/index.html", "index.html", "/dist/index.html", ""] {
            let out = svc
                .mint_html("com.test.good", source)
                .unwrap_or_else(|e| panic!("source {source:?} should mint: {e}"));
            assert!(out.html.contains("<h1>hi</h1>"), "source {source:?}");
        }
    }

    /// A route whose `source` is nested resolves its assets against its OWN
    /// directory, and `base_url` has to point there too or the un-inlined
    /// leftovers (images, fonts) resolve one directory too high.
    #[test]
    fn mint_html_nested_source_bases_on_its_own_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = write_pkg(tmp.path(), "com.test.good", true);
        write_bundle(&dir.join("dist"), "sub/index.html");
        let svc = PkgStaticService::discover(Some(tmp.path()));

        let out = svc
            .mint_html("com.test.good", "dist/sub/index.html")
            .expect("mints");
        assert_eq!(out.base_url, "/pkgs/com.test.good/sub/");
        assert!(out.html.contains("BUNDLE_BODY"), "nested asset not inlined: {}", out.html);
    }

    /// The field names are hand-matched against `PkgContentHtmlHandle` in
    /// `src/lib/tauri-cmd.ts`, destructured in `pkg-iframe-host.tsx` as
    /// `handle.baseUrl`. snake_case would leave `baseUrl` undefined, and
    /// `if (!srcDoc || !baseUrl)` renders the loading state forever — a blank
    /// pane with nothing in any log. Also pins the omissions: `supabase` and
    /// `secrets` must be ABSENT, which the frontend reads as `?? null`.
    #[test]
    fn mint_html_wire_shape_matches_the_frontend() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "com.test.good", true);
        let svc = PkgStaticService::discover(Some(tmp.path()));
        let out = svc.mint_html("com.test.good", "dist/index.html").unwrap();

        let wire = serde_json::to_value(&out).expect("serialize");
        let obj = wire.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["baseUrl", "html", "token"]);
        assert!(!obj.contains_key("supabase"), "supabase must be omitted, not null");
        assert!(!obj.contains_key("secrets"), "secrets must be omitted, not null");
    }

    /// A `required` host capability the daemon cannot resolve is a refusal
    /// with a named reason, never a mount. The error text is the only thing
    /// the user sees — `pkg-iframe-host.tsx` puts it straight into `setError`.
    #[test]
    fn mint_html_refuses_required_capabilities_it_cannot_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg_with_caps(
            tmp.path(),
            "com.test.supa",
            true,
            Some(r#"{"supabase":{"required":true}}"#),
        );
        write_pkg_with_caps(
            tmp.path(),
            "com.test.sec",
            true,
            Some(
                r#"{"secrets":{"declarations":[
                    {"name":"RESEND","vault_key":"RESEND_API_KEY","required":true}]}}"#,
            ),
        );
        let svc = PkgStaticService::discover(Some(tmp.path()));

        let e = svc.mint_html("com.test.supa", "dist/index.html").unwrap_err();
        assert!(e.contains("com.test.supa"), "{e}");
        assert!(e.contains("capabilities.supabase"), "{e}");

        let e = svc.mint_html("com.test.sec", "dist/index.html").unwrap_err();
        assert!(e.contains("capabilities.secrets"), "{e}");
        assert!(e.contains("RESEND"), "error should name the declaration: {e}");
    }

    /// Optional capabilities are the desktop's own fallback path (`supabase:
    /// null`, mount proceeds), so they must NOT be an error here either —
    /// refusing them would kill pkgs that already know how to cope.
    #[test]
    fn mint_html_mounts_pkgs_whose_capabilities_are_optional() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg_with_caps(
            tmp.path(),
            "com.test.opt",
            true,
            Some(
                r#"{"supabase":{"required":false},
                    "secrets":{"declarations":[
                      {"name":"OPT","vault_key":"OPT_KEY","required":false}]}}"#,
            ),
        );
        let svc = PkgStaticService::discover(Some(tmp.path()));
        let out = svc.mint_html("com.test.opt", "dist/index.html").expect("mounts");
        assert!(out.html.contains("<h1>hi</h1>"));
    }

    /// An unknown pkg must say what IS served rather than just "not found" —
    /// the overwhelmingly likely cause is a `--pkgs-dir` that does not contain
    /// what the operator thought it did.
    #[test]
    fn mint_html_unknown_pkg_names_what_is_served() {
        let tmp = tempfile::tempdir().unwrap();
        write_pkg(tmp.path(), "com.test.good", true);
        let svc = PkgStaticService::discover(Some(tmp.path()));

        let e = svc.mint_html("com.test.absent", "dist/index.html").unwrap_err();
        assert!(e.contains("com.test.absent"), "{e}");
        assert!(e.contains("com.test.good"), "should list what it does serve: {e}");

        let empty = PkgStaticService::discover(None);
        let e = empty.mint_html("com.test.good", "dist/index.html").unwrap_err();
        assert!(e.contains("--pkgs-dir"), "should name the missing flag: {e}");
    }

    /// `source` is caller-supplied and gets exactly the traversal treatment a
    /// URL path does — it goes through the same `safe_join`.
    #[test]
    fn mint_html_refuses_a_source_that_escapes_dist() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = write_pkg(tmp.path(), "com.test.good", true);
        std::fs::write(dir.join("secret.txt"), "s3cret").unwrap();
        let svc = PkgStaticService::discover(Some(tmp.path()));

        for source in ["../secret.txt", "dist/../secret.txt", "/etc/passwd"] {
            let e = svc
                .mint_html("com.test.good", source)
                .unwrap_err_or_panic(source);
            assert!(!e.contains("s3cret"), "{source} leaked: {e}");
        }
    }

    /// Tiny helper so the loop above reads as an assertion rather than a match.
    trait UnwrapErrOrPanic {
        fn unwrap_err_or_panic(self, ctx: &str) -> String;
    }
    impl UnwrapErrOrPanic for Result<MintedPkgHtml, String> {
        fn unwrap_err_or_panic(self, ctx: &str) -> String {
            match self {
                Ok(_) => panic!("source {ctx:?} should NOT have minted"),
                Err(e) => e,
            }
        }
    }
}
