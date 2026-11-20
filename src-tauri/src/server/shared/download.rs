//! The download header helpers shared by the authenticated `GET
//! /api/fs/download` (`server::rpc_files`) and the outside file links
//! (box-files WP-16: the child's `/internal/file-link/read` and the broker's
//! `/s/<token>/{view,download}`), so both answer with the same RFC 5987
//! `Content-Disposition` and the same passive-only inline list.

use axum::body::Bytes;

/// The only types served `inline`. Passive: no script runs from them on the
/// daemon's origin. HTML and SVG are deliberately absent (and so is markdown:
/// it is text a viewer renders, not something a browser should).
pub(crate) const INLINE_MIMES: &[&str] = &[
    "application/pdf",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "text/plain",
];

/// Read chunk of a download stream.
pub(crate) const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// RFC 5987 `attr-char`: everything outside it is percent-encoded in `filename*`.
const ATTR_CHAR: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// Bidi embeddings, overrides and isolates (U+202A..U+202E, U+2066..U+2069)
/// and the implicit direction marks (U+200E, U+200F, U+061C): a name like
/// `invoice<U+202E>fdp.exe` must not display as `invoiceexe.pdf`
/// (06-file-links-security §4.1, LS-22).
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

/// A file name safe to put in a header: no control characters (CR and LF
/// included), no bidi controls, no quotes, no path separators. Falls back to
/// `download`.
pub(crate) fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .map(|c| match c {
            '"' | '\\' | '/' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "download".to_string()
    } else {
        cleaned
    }
}

/// `Content-Disposition` value: an ASCII `filename=` fallback plus the real
/// name as `filename*=UTF-8''…` (RFC 5987 / 6266).
pub(crate) fn content_disposition(file_name: &str, inline: bool) -> String {
    let name = sanitize_filename(file_name);
    let ascii: String = name
        .chars()
        .map(|c| if c.is_ascii() && c != '%' && c != ';' { c } else { '_' })
        .collect();
    let kind = if inline { "inline" } else { "attachment" };
    format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{}",
        percent_encoding::utf8_percent_encode(&name, ATTR_CHAR)
    )
}

/// An open file as a body stream of [`DOWNLOAD_CHUNK`] reads, never more
/// than `len` bytes: a large file costs a buffer, not its size.
pub(crate) fn file_stream(
    file: tokio::fs::File,
    len: u64,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    use tokio::io::AsyncReadExt;
    futures_util::stream::unfold(Some(file.take(len)), |st| async move {
        let mut reader = st?;
        let mut buf = vec![0u8; DOWNLOAD_CHUNK];
        match reader.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(reader)))
            }
            Err(e) => Some((Err(e), None)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_strips_quotes_and_control_characters() {
        let d = content_disposition("a\"b\r\nc\td.png", false);
        assert!(!d.contains('\r') && !d.contains('\n') && !d.contains('\t'));
        assert!(d.starts_with("attachment; filename=\"a_bcd.png\";"), "{d}");
        assert_eq!(d.matches('"').count(), 2, "{d}");
    }

    #[test]
    fn disposition_carries_an_ascii_fallback_and_an_rfc5987_name() {
        let d = content_disposition("café 100%.txt", false);
        assert!(d.contains("filename=\"caf_ 100_.txt\""), "{d}");
        assert!(d.contains("filename*=UTF-8''caf%C3%A9%20100%25.txt"), "{d}");
        assert!(d.is_ascii());
    }

    #[test]
    fn empty_or_dot_names_fall_back() {
        for n in ["", "..", "\r\n"] {
            assert!(
                content_disposition(n, false).contains("filename=\"download\""),
                "{n:?}"
            );
        }
    }

    /// LS-22: a right-to-left override never reaches the header.
    #[test]
    fn bidi_controls_are_stripped_from_the_name() {
        let name = "invoice\u{202E}fdp.exe";
        assert_eq!(sanitize_filename(name), "invoicefdp.exe");
        let d = content_disposition(name, false);
        assert!(!d.contains('\u{202E}'));
        assert!(!d.contains("%E2%80%AE"), "{d}");
        for c in ['\u{202A}', '\u{202D}', '\u{2066}', '\u{2069}', '\u{200F}'] {
            assert_eq!(sanitize_filename(&format!("a{c}b.txt")), "ab.txt", "{c:?}");
        }
    }

    #[tokio::test]
    async fn the_file_stream_stops_at_len() {
        use futures_util::StreamExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("f.bin");
        std::fs::write(&p, vec![7u8; DOWNLOAD_CHUNK + 10]).unwrap();
        let f = tokio::fs::File::open(&p).await.unwrap();
        let chunks: Vec<_> = file_stream(f, (DOWNLOAD_CHUNK + 3) as u64).collect().await;
        let total: usize = chunks.iter().map(|c| c.as_ref().unwrap().len()).sum();
        assert_eq!(total, DOWNLOAD_CHUNK + 3);
    }
}
