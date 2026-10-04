//! The browser cookie that lets an installed app's own files load on a
//! single-user server.
//!
//! The browser client authenticates its fetches with the operator bearer in
//! an `Authorization` header. An app's iframe document, and the ES modules,
//! images and stylesheets it loads from `/pkgs/<id>/…`, are fetched by the
//! browser itself and can't carry that header, so they were refused with 401.
//!
//! When a request authenticates with the operator bearer **in the header**,
//! the server sets `ikenga_pkgs`: an `HttpOnly`, `SameSite=Strict` cookie
//! scoped to `Path=/pkgs` that holds an expiry and an HMAC over it keyed by
//! the operator bearer. It never contains the bearer, it is accepted only on
//! `/pkgs` paths (never on `/api/rpc`, the WebSocket routes or anything else),
//! it expires on its own, and changing the bearer invalidates every cookie
//! minted under it.
//!
//! A multi-user server doesn't use it: there the broker authenticates
//! `/pkgs/*` with each person's own session cookie and forwards the request
//! to that person's server only, stripping every cookie on the way.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The cookie's name.
pub const COOKIE: &str = "ikenga_pkgs";
/// The only path the cookie is sent to, and the only paths it authenticates.
pub const PATH: &str = "/pkgs";
/// Lifetime of one cookie.
pub const MAX_AGE_SECS: u64 = 12 * 60 * 60;

/// Domain separation: this MAC means "may load app files until <expiry>".
const PURPOSE: &[u8] = b"ikenga-pkgs-cookie:v1\0";

type HmacSha256 = Hmac<Sha256>;

fn mac(secret: &str, expires: u64) -> HmacSha256 {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(PURPOSE);
    mac.update(expires.to_string().as_bytes());
    mac
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether `path` is one the cookie may authenticate.
pub fn is_pkgs_path(path: &str) -> bool {
    path == PATH || path.starts_with("/pkgs/")
}

/// A fresh cookie value: `<expiry>.<hex hmac>`.
pub fn mint(secret: &str, now: u64) -> String {
    let expires = now + MAX_AGE_SECS;
    let tag = mac(secret, expires).finalize().into_bytes();
    format!("{expires}.{}", hex::encode(tag))
}

/// Whether `value` was minted under `secret` and hasn't expired.
pub fn verify(secret: &str, value: &str, now: u64) -> bool {
    let Some((expires, tag)) = value.split_once('.') else {
        return false;
    };
    let Ok(expires) = expires.parse::<u64>() else {
        return false;
    };
    if expires <= now || expires > now.saturating_add(MAX_AGE_SECS) {
        return false;
    }
    let Ok(tag) = hex::decode(tag) else {
        return false;
    };
    mac(secret, expires).verify_slice(&tag).is_ok()
}

/// Whether the request carries a valid `ikenga_pkgs` cookie.
pub fn presented_ok(headers: &axum::http::HeaderMap, secret: &str, now: u64) -> bool {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .filter(|(k, _)| *k == COOKIE)
        .any(|(_, v)| verify(secret, v, now))
}

/// The `Set-Cookie` value. `insecure` drops `Secure` under the same rule as
/// the device cookie (`access::devices::cookie_insecure`).
pub fn set_cookie(secret: &str, now: u64, insecure: bool) -> String {
    format!(
        "{COOKIE}={}; HttpOnly; SameSite=Strict; Path={PATH}; Max-Age={MAX_AGE_SECS}{}",
        mint(secret, now),
        if insecure { "" } else { "; Secure" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_cookie_verifies_until_it_expires() {
        let v = mint("tok", 1_000);
        assert!(verify("tok", &v, 1_000));
        assert!(verify("tok", &v, 1_000 + MAX_AGE_SECS - 1));
        assert!(!verify("tok", &v, 1_000 + MAX_AGE_SECS));
    }

    #[test]
    fn a_cookie_never_contains_the_secret_and_needs_it_to_verify() {
        let v = mint("operator-secret", 1_000);
        assert!(!v.contains("operator-secret"));
        assert!(!verify("another-secret", &v, 1_000));
    }

    #[test]
    fn tampered_or_malformed_values_are_refused() {
        let v = mint("tok", 1_000);
        let (exp, tag) = v.split_once('.').unwrap();
        // A later expiry under the old tag.
        let later = format!("{}.{tag}", exp.parse::<u64>().unwrap() + 60);
        assert!(!verify("tok", &later, 1_000));
        // Too far in the future, even if correctly signed.
        let far = 1_000 + 10 * MAX_AGE_SECS;
        let forged = format!(
            "{far}.{}",
            hex::encode(mac("tok", far).finalize().into_bytes())
        );
        assert!(!verify("tok", &forged, 1_000));
        for bad in [
            "",
            "tok",
            "1.2",
            "x.y",
            &format!("{exp}."),
            &format!(".{tag}"),
        ] {
            assert!(!verify("tok", bad, 1_000), "{bad:?}");
        }
    }

    #[test]
    fn only_pkgs_paths_qualify() {
        assert!(is_pkgs_path("/pkgs"));
        assert!(is_pkgs_path("/pkgs/com.x/index.html"));
        assert!(!is_pkgs_path("/pkgsx"));
        assert!(!is_pkgs_path("/api/rpc"));
        assert!(!is_pkgs_path("/ws/pty/1"));
    }

    #[test]
    fn set_cookie_is_scoped_and_http_only() {
        let c = set_cookie("tok", 1_000, false);
        assert!(c.starts_with("ikenga_pkgs="));
        assert!(c.contains("; HttpOnly"));
        assert!(c.contains("; SameSite=Strict"));
        assert!(c.contains("; Path=/pkgs;"));
        assert!(c.ends_with("; Secure"));
        assert!(!set_cookie("tok", 1_000, true).contains("Secure"));
    }
}
