//! Pure HTML rewrites for serving a pkg's iframe entry document.
//!
//! Headless core (no `tauri`, no `AppHandle`) so BOTH surfaces that hand a pkg
//! document to an iframe share one implementation:
//!
//! * desktop — `crate::pkg_content::PkgContentServer::mint_html`, which puts
//!   the result in an `<iframe srcdoc>` because WebKitGTK on Linux refuses
//!   subresource fetches from `about:srcdoc` documents targeting
//!   `http://127.0.0.1:*` (Tauri #12767 territory).
//! * headless daemon — `crate::server::pkg_static::PkgStaticService::mint_html`,
//!   which inlines for a completely different reason: `/pkgs/:id/*` lives
//!   inside the daemon's auth layer, and a browser attaches neither an
//!   `Authorization` header nor `?token=` to an iframe's *relative subresource*
//!   fetches. An inlined bundle issues no subresource fetch, so there is
//!   nothing to authenticate.
//!
//! Same code, two motivations — which is exactly why it lives here rather than
//! being copied. Lifted verbatim out of `pkg_content/mod.rs` (the
//! `crate::path_allow` shape), with two deliberate changes: the two failed-
//! inline warnings moved from `log::warn!` to `tracing::warn!` (the `log::`
//! macros are silently dropped in this crate, so on desktop those lines were
//! never reaching anyone), and their prefix went from `[pkg_content]` to
//! `[pkg_html]` because both surfaces emit them now.
//!
//! What did NOT move, and why: `absolutize_relative_urls` (and its
//! `next_skip_block` / `absolutize_chunk` helpers) stayed in `pkg_content`.
//! That one really is a WebKitGTK workaround — it exists because WebKit
//! ignores `<base href>` in `srcdoc` documents. Every browser the daemon
//! talks to honours `<base>`, so porting it would have been carrying a
//! workaround into a place that does not have the bug.

use std::path::Path;

use anyhow::{anyhow, Context, Result};


/// Replace relative `<script src="…">` and `<link rel="stylesheet" href="…">`
/// tags with their on-disk contents inlined as `<script>` / `<style>`.
///
/// Why: WebKitGTK on Linux refuses to issue subresource fetches from
/// `about:srcdoc` iframes targeting `http://127.0.0.1:*` (Tauri #12767
/// territory). `<base href>` and absolutized URLs both fail. Inlining is
/// the only reliable path. Dynamic imports inside the bundle still go
/// through the axum server (those URLs survive `absolutize_relative_urls`).
///
/// Pure string transform — we don't parse HTML. Only acts on tags whose
/// `src` / `href` value is a relative path (no scheme, no leading `/`).
/// Relative paths resolve against `resource_base` (the served HTML file's own
/// directory); `dist_root` is the traversal boundary — a resolved subresource
/// that escapes it (e.g. via `../`) is rejected. For a top-level
/// `dist/index.html` the two are the same directory.
pub(crate) fn inline_subresources(html: &str, resource_base: &Path, dist_root: &Path) -> String {
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let rest = &html[cursor..];
        let s_idx = find_case_insensitive(rest, "<script");
        let l_idx = find_case_insensitive(rest, "<link");
        let (tag_rel, is_script) = match (s_idx, l_idx) {
            (Some(s), Some(l)) => {
                if s <= l {
                    (s, true)
                } else {
                    (l, false)
                }
            }
            (Some(s), None) => (s, true),
            (None, Some(l)) => (l, false),
            (None, None) => {
                out.push_str(rest);
                break;
            }
        };
        let tag_start = cursor + tag_rel;
        out.push_str(&html[cursor..tag_start]);
        // Find end of opening tag.
        let open_end_rel = match html[tag_start..].find('>') {
            Some(p) => p + 1,
            None => {
                out.push_str(&html[tag_start..]);
                break;
            }
        };
        let open_end = tag_start + open_end_rel;
        let open_tag = &html[tag_start..open_end];

        if is_script {
            if let Some(src) = extract_attr(open_tag, "src") {
                if is_relative_url(&src) {
                    let close_search = &html[open_end..];
                    if let Some(close_rel) = find_case_insensitive(close_search, "</script>") {
                        let close_end = open_end + close_rel + "</script>".len();
                        match read_subresource(resource_base, dist_root, &src) {
                            Ok(content) => {
                                let lower = open_tag.to_ascii_lowercase();
                                let is_module = lower.contains("type=\"module\"")
                                    || lower.contains("type='module'");
                                if is_module {
                                    out.push_str("<script type=\"module\">");
                                } else {
                                    out.push_str("<script>");
                                }
                                // Avoid breaking out of the script context if
                                // the bundle contains a literal `</script>`.
                                out.push_str(&content.replace("</script", "<\\/script"));
                                out.push_str("</script>");
                                cursor = close_end;
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!("[pkg_html] inline script {src} failed: {e}");
                            }
                        }
                    }
                }
            }
            out.push_str(open_tag);
            cursor = open_end;
        } else {
            // <link>
            let lower = open_tag.to_ascii_lowercase();
            let is_stylesheet = lower.contains("rel=\"stylesheet\"")
                || lower.contains("rel='stylesheet'")
                || lower.contains("rel=stylesheet");
            if is_stylesheet {
                if let Some(href) = extract_attr(open_tag, "href") {
                    if is_relative_url(&href) {
                        match read_subresource(resource_base, dist_root, &href) {
                            Ok(content) => {
                                out.push_str("<style>");
                                out.push_str(&content.replace("</style", "<\\/style"));
                                out.push_str("</style>");
                                cursor = open_end;
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!("[pkg_html] inline stylesheet {href} failed: {e}");
                            }
                        }
                    }
                }
            }
            out.push_str(open_tag);
            cursor = open_end;
        }
    }
    out
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needles = [
        (format!("{}=\"", name.to_ascii_lowercase()), '"'),
        (format!("{}='", name.to_ascii_lowercase()), '\''),
    ];
    for (needle, quote) in &needles {
        if let Some(p) = lower.find(needle) {
            // Make sure we matched on a word boundary (preceded by space, tab,
            // newline, or `<` for the opener case — but `<name=` doesn't
            // happen for real attributes, so just check the preceding char).
            if p > 0 {
                let prev = tag.as_bytes()[p - 1];
                if !(prev == b' ' || prev == b'\t' || prev == b'\n' || prev == b'\r') {
                    continue;
                }
            }
            let val_start = p + needle.len();
            let rest = &tag[val_start..];
            if let Some(end) = rest.find(*quote) {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn is_relative_url(url: &str) -> bool {
    !(url.is_empty()
        || url.starts_with('/')
        || url.starts_with('#')
        || url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("blob:")
        || url.starts_with("about:")
        || url.starts_with("mailto:")
        || url.starts_with("javascript:"))
}

fn read_subresource(resource_base: &Path, dist_root: &Path, url: &str) -> Result<String> {
    // Strip query string + fragment. Vite emits hashed filenames so we don't
    // expect either, but be defensive.
    let bare = url.split(['?', '#']).next().unwrap_or(url);
    // Resolve relative to the served HTML file's own directory. `./assets/x.js`
    // in `dist/sub/index.html` lives at `dist/sub/assets/x.js`. Strip a leading
    // `./` / `/`, but leave `../` intact so `Path::join` climbs correctly.
    let trimmed = bare.trim_start_matches("./").trim_start_matches('/');
    let abs = resource_base.join(trimmed);
    let canon = abs
        .canonicalize()
        .with_context(|| format!("canonicalize {}", abs.display()))?;
    // Traversal guard stays anchored at `dist_root`: a `../` may climb out of
    // `resource_base` but must never escape the pkg's dist root.
    if !canon.starts_with(dist_root) {
        return Err(anyhow!("subresource `{url}` resolves outside dist_root"));
    }
    std::fs::read_to_string(&canon).with_context(|| format!("read {}", canon.display()))
}

/// Inject a `<base href="<base_url>">` into the HTML's `<head>` so relative
/// subresource loads (`./app.js`, `styles.css`) resolve against the per-token
/// pkg-content URL. Pure-string transform: we don't parse HTML. If `<head>`
/// is missing we prepend a synthetic head (rare for real packages but the
/// no-script smoke fixtures relied on it).
pub(crate) fn inject_base_href(html: &str, base_url: &str) -> String {
    let tag = format!("<base href=\"{}\">", base_url);
    if let Some(idx) = find_case_insensitive(html, "<head>") {
        let insert_at = idx + "<head>".len();
        let mut out = String::with_capacity(html.len() + tag.len());
        out.push_str(&html[..insert_at]);
        out.push_str(&tag);
        out.push_str(&html[insert_at..]);
        out
    } else if let Some(idx) = find_case_insensitive(html, "<html") {
        // No <head>: insert one right after <html ...> (closing >).
        let after_html = html[idx..]
            .find('>')
            .map(|p| idx + p + 1)
            .unwrap_or(html.len());
        let mut out = String::with_capacity(html.len() + tag.len() + 16);
        out.push_str(&html[..after_html]);
        out.push_str("<head>");
        out.push_str(&tag);
        out.push_str("</head>");
        out.push_str(&html[after_html..]);
        out
    } else {
        // Headless fragment — prepend tag.
        format!("{}{}", tag, html)
    }
}

pub(crate) fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let lower = haystack.to_ascii_lowercase();
    lower.find(&needle.to_ascii_lowercase())
}
