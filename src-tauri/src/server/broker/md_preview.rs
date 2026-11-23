//! Markdown for the public link page: a file shown as a readable document
//! instead of its source, with nothing the file contains able to run, load or
//! navigate (plans/2026-11-20-box-files-sharing/06-file-links-security.md §5.1,
//! amended after the 0.32.2 phone check).
//!
//! The renderer is an emitter over the parser's events, not a sanitiser over
//! its HTML output: only the tags written here can appear, and every piece of
//! file text goes through [escape]. So:
//!
//! - raw HTML in the file (blocks and inline) is shown as text, never parsed;
//! - an image is shown only when its bytes are inside the file (a base64 data:
//!   URI for png, jpeg, gif or webp, never svg); any other image is shown as
//!   `[image: alt]` and no address is ever fetched;
//! - a link is shown as its text followed by the address, as plain text, and
//!   only for http, https and mailto: nothing is clickable, so a file cannot
//!   dress a hostile address up as a trusted word;
//! - the document is served under a CSP with no script, no style but its own
//!   hash, no fetch and the sandbox directive, inside a sandboxed iframe.

use base64::Engine as _;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Past this much output the document stops with a note. A 5 MiB file of
/// nested quotes or list markers would otherwise grow several-fold.
const MAX_OUT: usize = 8 * 1024 * 1024;
/// A shown address is cut here.
const MAX_URL_SHOWN: usize = 300;

const STYLE: &str = "body{margin:0;padding:1.25rem 1.5rem 2rem;font:15px/1.6 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;\
background:#fff;color:#1d1b18;overflow-wrap:anywhere}h1,h2,h3,h4,h5,h6{line-height:1.25;margin:1.4em 0 .5em}\
h1{font-size:1.6rem}h2{font-size:1.35rem}h3{font-size:1.15rem}h4,h5,h6{font-size:1rem}\
p,ul,ol,pre,table,blockquote{margin:.7em 0}ul,ol{padding-left:1.5rem}\
code,pre{font:13px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}\
code{background:#f1eee8;padding:.1em .3em;border-radius:4px}\
pre{background:#f1eee8;padding:.75rem 1rem;border-radius:6px;overflow:auto;white-space:pre-wrap}\
pre code{background:none;padding:0}pre.front{color:#5d574e;border-left:3px solid #ddd6cb}\
blockquote{margin-left:0;padding-left:1rem;border-left:3px solid #ddd6cb;color:#4a443b}\
table{border-collapse:collapse;display:block;overflow:auto}th,td{border:1px solid #ddd6cb;padding:.35rem .6rem;text-align:left}\
th{background:#f6f4f0}hr{border:0;border-top:1px solid #ddd6cb;margin:1.5rem 0}.addr{color:#5d574e}\
@media (prefers-color-scheme:dark){body{background:#222;color:#ece7df}code,pre{background:#2d2a26}\
pre.front{color:#a89f92;border-color:#4a443b}blockquote{border-color:#4a443b;color:#c9c1b5}\
th,td{border-color:#4a443b}th{background:#2a2723}hr{border-color:#4a443b}.addr{color:#a89f92}}";

/// The CSP this document is served under: no script, no fetch, its one
/// stylesheet by hash, and the sandbox directive.
pub fn csp() -> &'static str {
    static CSP: OnceLock<String> = OnceLock::new();
    CSP.get_or_init(|| {
        let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(STYLE.as_bytes()));
        format!(
            "default-src 'none'; style-src 'sha256-{hash}'; img-src data:; base-uri 'none'; form-action 'none'; \
             frame-ancestors 'self'; sandbox"
        )
    })
}

/// Whether a file name is markdown (the page's preview kind is "text" for
/// every text type; this picks the one rendered as a document).
pub fn is_markdown(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// An address worth showing: http, https or mailto only.
fn shown_address(url: &str) -> Option<&str> {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let ok = lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:");
    ok.then_some(url)
}

/// An image whose bytes are inside the file: a base64 `data:` URI for png,
/// jpeg, gif or webp (never svg), checked character by character. Any other
/// image is not fetched or shown.
fn embedded_image(url: &str) -> Option<String> {
    let rest = url.strip_prefix("data:image/")?;
    let (kind, rest) = rest.split_once(';')?;
    if !matches!(kind, "png" | "jpeg" | "gif" | "webp") {
        return None;
    }
    let data = rest.strip_prefix("base64,")?;
    let clean = !data.is_empty()
        && data
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    clean.then(|| format!("data:image/{kind};base64,{data}"))
}

/// The whole HTML document for markdown text.
pub fn render(markdown: &str) -> String {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut out = String::with_capacity(markdown.len() * 2 + 1024);
    out.push_str(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"referrer\" content=\"no-referrer\"><style>",
    );
    out.push_str(STYLE);
    out.push_str("</style></head><body>");
    let mut links: Vec<Option<String>> = Vec::new();
    let mut in_head = false;
    let mut truncated = false;
    // Inside an image only its alt text is kept, as plain text: nothing the
    // alt contains can open a tag or leave the attribute it is written into.
    let mut image: Option<(Option<String>, String, usize)> = None;
    for ev in Parser::new_ext(markdown, opts) {
        if out.len() > MAX_OUT {
            truncated = true;
            break;
        }
        if let Some((_, alt, depth)) = image.as_mut() {
            match ev {
                Event::Start(Tag::Image { .. }) => *depth += 1,
                Event::End(TagEnd::Image) if *depth > 0 => *depth -= 1,
                Event::End(TagEnd::Image) => {
                    let (src, alt, _) = image.take().unwrap_or_default();
                    match src {
                        Some(src) => {
                            out.push_str("<img src=\"");
                            escape_into(&mut out, &src);
                            out.push_str("\" alt=\"");
                            escape_into(&mut out, &alt);
                            out.push_str("\">");
                        }
                        None => {
                            out.push_str("[image: ");
                            escape_into(&mut out, &alt);
                            out.push(']');
                        }
                    }
                }
                Event::Text(t) | Event::Code(t) | Event::Html(t) | Event::InlineHtml(t) => {
                    if alt.len() < 1000 {
                        alt.push_str(&t);
                    }
                }
                Event::SoftBreak | Event::HardBreak => alt.push(' '),
                _ => {}
            }
            continue;
        }
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.push_str("<p>"),
                Tag::Heading { level, .. } => {
                    let n = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    out.push_str(&format!("<h{n}>"));
                }
                Tag::BlockQuote(_) => out.push_str("<blockquote>"),
                Tag::CodeBlock(_) => out.push_str("<pre><code>"),
                Tag::List(Some(start)) => out.push_str(&format!("<ol start=\"{start}\">")),
                Tag::List(None) => out.push_str("<ul>"),
                Tag::Item => out.push_str("<li>"),
                Tag::Emphasis => out.push_str("<em>"),
                Tag::Strong => out.push_str("<strong>"),
                Tag::Strikethrough => out.push_str("<del>"),
                Tag::Link { dest_url, .. } => links.push(
                    shown_address(&dest_url)
                        .map(|u| u.chars().take(MAX_URL_SHOWN).collect::<String>()),
                ),
                Tag::Image { dest_url, .. } => {
                    image = Some((embedded_image(&dest_url), String::new(), 0))
                }
                Tag::Table(_) => out.push_str("<table>"),
                Tag::TableHead => {
                    in_head = true;
                    out.push_str("<thead><tr>");
                }
                Tag::TableRow => out.push_str("<tr>"),
                Tag::TableCell => out.push_str(if in_head { "<th>" } else { "<td>" }),
                Tag::MetadataBlock(_) => out.push_str("<pre class=\"front\">"),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out.push_str("</p>"),
                TagEnd::Heading(level) => {
                    let n = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    out.push_str(&format!("</h{n}>"));
                }
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>"),
                TagEnd::CodeBlock => out.push_str("</code></pre>"),
                TagEnd::List(true) => out.push_str("</ol>"),
                TagEnd::List(false) => out.push_str("</ul>"),
                TagEnd::Item => out.push_str("</li>"),
                TagEnd::Emphasis => out.push_str("</em>"),
                TagEnd::Strong => out.push_str("</strong>"),
                TagEnd::Strikethrough => out.push_str("</del>"),
                TagEnd::Link => {
                    if let Some(Some(url)) = links.pop() {
                        out.push_str(" <span class=\"addr\">(");
                        escape_into(&mut out, &url);
                        out.push_str(")</span>");
                    }
                }
                TagEnd::Table => out.push_str("</table>"),
                TagEnd::TableHead => {
                    in_head = false;
                    out.push_str("</tr></thead>");
                }
                TagEnd::TableRow => out.push_str("</tr>"),
                TagEnd::TableCell => out.push_str(if in_head { "</th>" } else { "</td>" }),
                TagEnd::MetadataBlock(_) => out.push_str("</pre>"),
                _ => {}
            },
            Event::Code(t) => {
                out.push_str("<code>");
                escape_into(&mut out, &t);
                out.push_str("</code>");
            }
            // Raw HTML is text, never markup.
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => escape_into(&mut out, &t),
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            Event::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            Event::FootnoteReference(l) => {
                out.push('[');
                escape_into(&mut out, &l);
                out.push(']');
            }
            _ => {}
        }
    }
    if truncated {
        out.push_str(
            "<p class=\"addr\">The rest of this file is not shown. Download it to read it all.</p>",
        );
    }
    out.push_str("</body></html>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(md: &str) -> String {
        render(md)
    }

    #[test]
    fn renders_a_document() {
        let h = body("# Title\n\nSome *words* and **bold** and ~~gone~~.\n\n- a\n- b\n\n1. one\n\n> quote\n\n---\n");
        for want in [
            "<h1>Title</h1>",
            "<em>words</em>",
            "<strong>bold</strong>",
            "<del>gone</del>",
            "<ul>",
            "<ol start=\"1\">",
            "<blockquote>",
            "<hr>",
        ] {
            assert!(h.contains(want), "missing {want} in {h}");
        }
    }

    #[test]
    fn renders_tables_code_tasks_and_front_matter() {
        let h = body("---\nname: Devotee\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n\n```rust\nlet x = 1 < 2;\n```\n");
        assert!(h.contains("<pre class=\"front\">"), "{h}");
        assert!(h.contains("name: Devotee"), "{h}");
        assert!(h.contains("<th>a</th>") && h.contains("<td>1</td>"), "{h}");
        assert!(h.contains("☑ ") && h.contains("☐ "), "{h}");
        assert!(h.contains("let x = 1 &lt; 2;"), "{h}");
    }

    /// Everything that could run, load or navigate is inert.
    #[test]
    fn nothing_in_a_file_can_become_markup() {
        let payloads = [
            "<script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "<iframe src=\"https://evil.example\"></iframe>",
            "<a href=\"javascript:alert(1)\">x</a>",
            "[x](javascript:alert(1))",
            "[x](data:text/html;base64,PHNjcmlwdD4=)",
            "[x](vbscript:msgbox)",
            "[x]( JaVaScRiPt:alert(1))",
            "![x](https://tracker.example/p.gif)",
            "![x](javascript:alert(1))",
            "<svg onload=alert(1)>",
            "<style>*{background:url(https://t.example)}</style>",
            "<meta http-equiv=refresh content=\"0;url=https://evil.example\">",
            "<base href=\"https://evil.example/\">",
            "<form action=https://evil.example><button>go</button></form>",
            "[a](<https://x.example/\" onmouseover=\"alert(1)>)",
            "<!-- --><script>1</script>",
        ];
        for p in payloads {
            let h = render(p);
            let inner = h.split("</style></head><body>").nth(1).unwrap();
            for bad in [
                "<script", "<img", "<iframe", "<svg", "<style", "<meta", "<base", "<form", "<a ",
                "<button",
            ] {
                // Only as escaped text: the raw tag opener must not appear.
                assert!(!inner.contains(bad), "{p:?} produced {bad}: {inner}");
            }
        }
    }

    #[test]
    fn links_are_text_and_only_for_safe_schemes() {
        let h = body("[docs](https://example.com/a?b=1&c=2) [bad](javascript:alert(1)) [mail](mailto:a@b.co)");
        assert!(
            h.contains("docs <span class=\"addr\">(https://example.com/a?b=1&amp;c=2)</span>"),
            "{h}"
        );
        assert!(h.contains("bad") && !h.contains("javascript:"), "{h}");
        assert!(h.contains("(mailto:a@b.co)"), "{h}");
        assert!(!h.contains("<a "), "{h}");
    }

    #[test]
    fn an_image_is_its_alt_text_and_never_a_request() {
        let h = body("![a chart](https://t.example/p.png)");
        assert!(h.contains("[image: a chart]"), "{h}");
        assert!(!h.contains("t.example") && !h.contains("<img"), "{h}");
        let h = body("![rel](./chart.png)");
        assert!(h.contains("[image: rel]") && !h.contains("<img"), "{h}");
    }

    #[test]
    fn an_embedded_raster_image_is_shown_and_nothing_else_is() {
        let ok = body("![dot](data:image/png;base64,iVBORw0KGgo=)");
        assert!(
            ok.contains("<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"dot\">"),
            "{ok}"
        );
        for bad in [
            "![x](data:image/svg+xml;base64,PHN2Zz4=)",
            "![x](data:text/html;base64,PHNjcmlwdD4=)",
            "![x](data:image/png;base64,AAAA\"onerror=\"alert(1))",
            "![x](data:image/png,<script>)",
            "![x](data:image/png;base64,)",
        ] {
            let h = body(bad);
            assert!(!h.contains("<img"), "{bad:?} -> {h}");
        }
        // Alt text cannot leave its attribute.
        let h = body("![a\" onerror=\"alert(1)](data:image/gif;base64,R0lGOD==)");
        assert!(h.contains("alt=\"a&quot; onerror=&quot;alert(1)\""), "{h}");
    }

    #[test]
    fn deep_nesting_and_large_input_are_bounded() {
        let deep = ">".repeat(20_000) + " x";
        let h = render(&deep);
        assert!(h.len() < MAX_OUT * 2);
        let big = "- a\n".repeat(2_000_000);
        let h = render(&big);
        assert!(h.len() < MAX_OUT * 2);
        assert!(h.ends_with("</body></html>"));
    }

    #[test]
    fn csp_has_no_script_and_names_the_style_hash() {
        let c = csp();
        assert!(
            c.contains("default-src 'none'")
                && c.contains("sandbox")
                && c.contains("style-src 'sha256-")
        );
        assert!(!c.contains("script-src") && !c.contains("unsafe"));
    }

    #[test]
    fn picks_markdown_by_extension() {
        assert!(is_markdown("DESIGN.md") && is_markdown("a.MARKDOWN"));
        assert!(!is_markdown("a.txt") && !is_markdown("md") && !is_markdown("a.csv"));
    }
}
