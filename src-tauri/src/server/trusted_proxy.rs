//! Trusted proxy resolution and client IP extraction.
//!
//! When Ikenga runs behind a reverse proxy (e.g. Caddy, Nginx, or an AWS ALB),
//! requests terminate at the proxy and reach Ikenga from loopback (`127.0.0.1`
//! or `::1`) or a private subnet. Without trusted-proxy configuration,
//! every client appears to originate from the proxy address, causing rate limits,
//! pairing throttling, and audit logs to collapse onto that single IP.
//!
//! Setting `IKENGA_TRUSTED_PROXIES` configures an opt-in list of trusted proxy
//! IP addresses and CIDR networks (comma-separated, e.g. `127.0.0.1,::1,10.0.0.0/8`).
//! When unset or empty, existing behavior is preserved: headers like `X-Forwarded-For`
//! and `Forwarded` are ignored and the raw socket peer is always used.
//!
//! Exactly ONE forwarding header is honoured: the one the configured proxy writes,
//! chosen by `IKENGA_TRUSTED_PROXY_HEADER` (`x-forwarded-for`, the default, or
//! `forwarded`). The other header is never read. Caddy and nginx
//! (`$proxy_add_x_forwarded_for`) rewrite only `X-Forwarded-For` and pass a client's
//! `Forwarded` header through untouched, so reading whichever header happens to parse
//! would let any client name its own address.
//!
//! When the direct TCP peer matches a trusted proxy, client address resolution walks
//! the forwarded hops from right to left (closest hop to furthest) and picks the
//! right-most untrusted hop. If every hop is itself a trusted proxy, the right-most
//! hop is chosen: the direct peer wrote it from its own socket, whereas anything
//! further left may have been prepended by a client that sits inside a trusted range.
//! A hop that does not parse as an IP (e.g. `for=unknown`, `for=_hidden`, garbage) ends
//! the walk: everything to its left is unverifiable, so the socket peer is returned.
//! If the header is absent or invalid, the socket peer address is returned as fallback.
//!
//! IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`, what a dual-stack listener
//! reports for an IPv4 peer) are matched and returned in their IPv4 form.
//!
//! The configuration is parsed ONCE per process ([`install`] at daemon start, or
//! lazily from the environment on first use), so a bad value warns once rather
//! than on every request.

use axum::http::HeaderMap;
use ipnet::IpNet;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::OnceLock;

pub const TRUSTED_PROXIES_ENV: &str = "IKENGA_TRUSTED_PROXIES";
pub const TRUSTED_PROXY_HEADER_ENV: &str = "IKENGA_TRUSTED_PROXY_HEADER";

/// The single forwarding header a trusted proxy is known to write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForwardHeader {
    /// `X-Forwarded-For` (Caddy, nginx, HAProxy, most load balancers). Default.
    #[default]
    XForwardedFor,
    /// RFC 7239 `Forwarded`. Only for a proxy that writes (and sanitises) it.
    Forwarded,
}

impl ForwardHeader {
    /// Parse `IKENGA_TRUSTED_PROXY_HEADER`. Unset/empty selects the default.
    /// An unrecognised value warns and falls back to `X-Forwarded-For` rather
    /// than reading both headers.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "x-forwarded-for" | "xff" => Self::XForwardedFor,
            "forwarded" => Self::Forwarded,
            other => {
                tracing::warn!(
                    "invalid {TRUSTED_PROXY_HEADER_ENV} {other:?}; using x-forwarded-for"
                );
                Self::XForwardedFor
            }
        }
    }

    pub fn from_env() -> Self {
        match std::env::var(TRUSTED_PROXY_HEADER_ENV) {
            Ok(val) => Self::parse(&val),
            Err(_) => Self::default(),
        }
    }
}

/// Collection of trusted IP subnets / addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    nets: Vec<IpNet>,
    header: ForwardHeader,
}

impl TrustedProxies {
    /// Create an empty set (no trusted proxies; forwarded headers always ignored).
    pub fn empty() -> Self {
        Self {
            nets: Vec::new(),
            header: ForwardHeader::default(),
        }
    }

    /// Read trusted proxies from `IKENGA_TRUSTED_PROXIES` and the honoured
    /// header from `IKENGA_TRUSTED_PROXY_HEADER`.
    pub fn from_env() -> Self {
        Self::from_settings(
            std::env::var(TRUSTED_PROXIES_ENV).ok().as_deref(),
            std::env::var(TRUSTED_PROXY_HEADER_ENV).ok().as_deref(),
        )
    }

    /// Build from the two settings as given (CLI flag or environment).
    /// Without a proxy list the header setting is irrelevant and ignored.
    pub fn from_settings(proxies: Option<&str>, header: Option<&str>) -> Self {
        match proxies {
            Some(list) => {
                Self::parse(list).with_header(header.map(ForwardHeader::parse).unwrap_or_default())
            }
            None => Self::empty(),
        }
    }

    /// The header this configuration honours.
    pub fn header(&self) -> ForwardHeader {
        self.header
    }

    /// Number of configured networks.
    pub fn len(&self) -> usize {
        self.nets.len()
    }

    /// Select which forwarding header is honoured (the only one ever read).
    pub fn with_header(mut self, header: ForwardHeader) -> Self {
        self.header = header;
        self
    }

    /// Parse a comma-separated list of IPs and CIDRs.
    ///
    /// Malformed tokens are skipped with a warning trace, rather than failing the entire server.
    pub fn parse(s: &str) -> Self {
        let mut nets = Vec::new();
        for token in s.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Ok(net) = IpNet::from_str(token) {
                nets.push(net);
            } else if let Ok(ip) = IpAddr::from_str(token) {
                nets.push(IpNet::from(ip));
            } else {
                tracing::warn!("invalid IP or CIDR in {TRUSTED_PROXIES_ENV}: {token:?}");
            }
        }
        Self {
            nets,
            header: ForwardHeader::default(),
        }
    }

    /// Whether any trusted proxy is configured.
    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    /// Check if the given IP address is within any trusted proxy network.
    /// An IPv4-mapped IPv6 address matches as its IPv4 form.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.nets.iter().any(|net| net.contains(&ip))
    }

    /// Resolve the effective client IP address from the direct peer address and request headers.
    ///
    /// Rules:
    /// 1. If `peer_ip` is `None`, return `None`.
    /// 2. If `peer_ip` is NOT in the trusted proxies list (or trusted proxies is empty),
    ///    return `peer_ip` directly. Never inspect forwarded headers from an untrusted peer.
    /// 3. If `peer_ip` IS trusted:
    ///    - Inspect ONLY the configured header (`X-Forwarded-For` by default, or
    ///      RFC 7239 `Forwarded`). The other header is never consulted.
    ///    - Parse the chain of hops.
    ///    - Walk right-to-left (closest to furthest) and pick the first hop that is NOT trusted.
    ///    - A hop that is not an IP ends the walk and returns `peer_ip`.
    ///    - If all hops are trusted, return the right-most hop (written by the
    ///      direct peer itself; anything to its left may be client-prepended).
    ///    - If no hops exist, return `peer_ip`.
    ///
    /// The untrusted-peer and fallback paths return `peer_ip` exactly as given
    /// (no canonicalisation), so an unset configuration is the old behaviour.
    pub fn resolve_client_ip(
        &self,
        peer_ip: Option<IpAddr>,
        headers: &HeaderMap,
    ) -> Option<IpAddr> {
        let peer = peer_ip?;
        if !self.contains(&peer) {
            return Some(peer);
        }

        // Direct peer is trusted. Read only the header the proxy is configured
        // to write; never fall through to the other one (a client controls it).
        let hops = match self.header {
            ForwardHeader::XForwardedFor => parse_x_forwarded_for_header(headers),
            ForwardHeader::Forwarded => parse_forwarded_header(headers),
        };
        let Some(hops) = hops else {
            return Some(peer);
        };
        if hops.is_empty() {
            return Some(peer);
        }

        // Walk right-to-left: pick the right-most untrusted hop. An unparseable
        // hop means we cannot vouch for anything to its left, so stop there.
        for hop in hops.iter().rev() {
            match hop {
                None => return Some(peer),
                Some(ip) if !self.contains(ip) => return Some(ip.to_canonical()),
                Some(_) => {}
            }
        }

        // Every hop is a trusted proxy. The right-most one was written by the
        // direct peer from its own socket; the left-most could have been
        // prepended by a client inside a trusted range, so never pick it.
        hops.last()
            .copied()
            .flatten()
            .map(|ip| ip.to_canonical())
            .or(Some(peer))
    }

    /// Helper that returns the resolved client IP as an `Option<String>`.
    pub fn resolve_client_addr(
        &self,
        peer_ip: Option<IpAddr>,
        headers: &HeaderMap,
    ) -> Option<String> {
        self.resolve_client_ip(peer_ip, headers)
            .map(|ip| ip.to_string())
    }
}

static CONFIG: OnceLock<TrustedProxies> = OnceLock::new();

/// Install the process-wide configuration. The daemon calls this once at
/// startup, before it serves anything, with the parsed CLI/env settings, so
/// the environment is never mutated at runtime. Returns `false` (and keeps the
/// existing value) if a configuration was already installed or lazily read.
pub fn install(tp: TrustedProxies) -> bool {
    CONFIG.set(tp).is_ok()
}

/// The process-wide configuration: the installed one, else parsed from the
/// environment on first use (the desktop app's embedded server). Parsed once.
pub fn config() -> &'static TrustedProxies {
    CONFIG.get_or_init(TrustedProxies::from_env)
}

/// Resolve effective client IP using the process-level `IKENGA_TRUSTED_PROXIES`
/// (and `IKENGA_TRUSTED_PROXY_HEADER`) configuration.
pub fn client_ip(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    #[cfg(test)]
    if let Some(tp) = test_override::current() {
        return tp.resolve_client_ip(peer_ip, headers);
    }
    config().resolve_client_ip(peer_ip, headers)
}

/// Test-only, thread-scoped configuration override. Tests must not mutate the
/// process environment: `cargo test` runs tests in parallel, and a leaked
/// `IKENGA_TRUSTED_PROXIES` silently changes what every other test's handler
/// resolves. `#[tokio::test]` runs handlers on the test's own thread, so a
/// thread-local reaches them without touching anyone else.
#[cfg(test)]
pub(crate) mod test_override {
    use super::TrustedProxies;
    use std::cell::RefCell;

    thread_local! {
        static OVERRIDE: RefCell<Option<TrustedProxies>> = const { RefCell::new(None) };
    }

    pub(crate) fn current() -> Option<TrustedProxies> {
        OVERRIDE.with(|o| o.borrow().clone())
    }

    /// Installs `tp` for this thread until the guard drops.
    pub(crate) fn set(tp: TrustedProxies) -> Guard {
        OVERRIDE.with(|o| *o.borrow_mut() = Some(tp));
        Guard(())
    }

    pub(crate) struct Guard(());

    impl Drop for Guard {
        fn drop(&mut self) {
            OVERRIDE.with(|o| *o.borrow_mut() = None);
        }
    }
}

/// Resolve effective client address (as a String) using the process-level `IKENGA_TRUSTED_PROXIES` configuration.
pub fn client_addr(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> Option<String> {
    client_ip(peer_ip, headers).map(|ip| ip.to_string())
}

/// Resolve client address string or fallback to "unknown".
pub fn addr_of(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> String {
    client_addr(peer_ip, headers).unwrap_or_else(|| "unknown".into())
}

/// Extract peer IP from axum `ConnectInfo<SocketAddr>`.
pub fn peer_from_connect_info(
    conn: &Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
) -> Option<IpAddr> {
    conn.as_ref().map(|c| c.0.ip())
}

/// Convenience function to extract client address string from axum `ConnectInfo` and `HeaderMap`.
pub fn client_addr_from_conn(
    conn: &Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: &HeaderMap,
) -> Option<String> {
    client_addr(peer_from_connect_info(conn), headers)
}

/// Convenience function to extract client address string (or "unknown") from axum `ConnectInfo` and `HeaderMap`.
pub fn addr_of_conn(
    conn: &Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: &HeaderMap,
) -> String {
    addr_of(peer_from_connect_info(conn), headers)
}

/// Parse RFC 7239 `Forwarded` header: `for=192.0.2.60;proto=http;by=203.0.113.43, for="[2001:db8:cafe::17]:4711"`
///
/// One entry per `for=` parameter; `None` marks a hop that is not an IP
/// (`unknown`, an obfuscated `_token`, garbage). Returns `None` when the header
/// is absent, not visible ASCII, or names no `for=` hop.
fn parse_forwarded_header(headers: &HeaderMap) -> Option<Vec<Option<IpAddr>>> {
    let mut all_hops = Vec::new();
    for val in headers.get_all("forwarded") {
        let s = val.to_str().ok()?;
        // Forwarded elements are comma-separated
        for element in s.split(',') {
            // Within an element, parameters are semicolon-separated
            for param in element.split(';') {
                let param = param.trim();
                let lower = param.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("for=") {
                    let raw_for = &param[param.len() - rest.len()..];
                    all_hops.push(parse_ip_or_bracketed_port(raw_for));
                }
            }
        }
    }
    if all_hops.is_empty() {
        None
    } else {
        Some(all_hops)
    }
}

/// Parse standard `X-Forwarded-For` header: `client, proxy1, proxy2`
///
/// `None` marks a hop that is not an IP. Returns `None` when the header is
/// absent, not visible ASCII, or empty.
fn parse_x_forwarded_for_header(headers: &HeaderMap) -> Option<Vec<Option<IpAddr>>> {
    let mut all_hops = Vec::new();
    for val in headers.get_all("x-forwarded-for") {
        let s = val.to_str().ok()?;
        for token in s.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            all_hops.push(parse_ip_or_bracketed_port(token));
        }
    }
    if all_hops.is_empty() {
        None
    } else {
        Some(all_hops)
    }
}

/// Parse an IP string that might be quoted or have a port appended:
/// - `"192.0.2.1"` or `192.0.2.1` or `192.0.2.1:8080`
/// - `"[2001:db8::1]:443"` or `"[2001:db8::1]"` or `2001:db8::1`
fn parse_ip_or_bracketed_port(s: &str) -> Option<IpAddr> {
    let mut s = s.trim();
    // Strip quotes
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        s = &s[1..s.len() - 1];
    }
    s = s.trim();

    // Check for bracketed IPv6: `[2001:db8::1]:port` or `[2001:db8::1]`
    if s.starts_with('[') {
        if let Some(closing) = s.find(']') {
            let ip_str = &s[1..closing];
            return IpAddr::from_str(ip_str).ok();
        }
    }

    // Try parsing directly as IpAddr
    if let Ok(ip) = IpAddr::from_str(s) {
        return Some(ip);
    }

    // Try stripping IPv4 port: `192.0.2.1:8080` (only if exactly one colon)
    if let Some((ip_part, _port_part)) = s.split_once(':') {
        // If there are more colons, it's an unbracketed IPv6 and shouldn't be split as ip:port
        if !ip_part.contains(':') && !_port_part.contains(':') {
            if let Ok(ip) = IpAddr::from_str(ip_part) {
                return Some(ip);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parse_trusted_proxies_list() {
        let tp = TrustedProxies::parse("127.0.0.1, ::1, 10.0.0.0/8, 172.16.0.0/12");
        assert!(tp.contains(&"127.0.0.1".parse().unwrap()));
        assert!(tp.contains(&"::1".parse().unwrap()));
        assert!(tp.contains(&"10.1.2.3".parse().unwrap()));
        assert!(tp.contains(&"172.20.0.1".parse().unwrap()));
        assert!(!tp.contains(&"192.168.1.1".parse().unwrap()));
        assert!(!tp.contains(&"8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn empty_trusted_proxies_ignores_headers() {
        let tp = TrustedProxies::empty();
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.195"));

        // When untrusted, returns socket peer directly
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }

    #[test]
    fn untrusted_peer_ignores_spoofed_headers() {
        let tp = TrustedProxies::parse("10.0.0.1");
        let untrusted_peer: IpAddr = "192.168.1.50".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.2.3.4"));
        headers.insert(
            "forwarded",
            HeaderValue::from_static("for=1.2.3.4;proto=https"),
        );

        // Untrusted peer -> headers ignored completely
        assert_eq!(
            tp.resolve_client_ip(Some(untrusted_peer), &headers),
            Some(untrusted_peer)
        );
    }

    #[test]
    fn trusted_peer_resolves_x_forwarded_for() {
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8");
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.195"));
        assert_eq!(
            tp.resolve_client_ip(Some(loopback), &headers),
            Some("203.0.113.195".parse().unwrap())
        );

        // With port
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.195:443"),
        );
        assert_eq!(
            tp.resolve_client_ip(Some(loopback), &headers),
            Some("203.0.113.195".parse().unwrap())
        );

        // IPv6 bracketed with port
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("[2001:db8::1]:1234"),
        );
        assert_eq!(
            tp.resolve_client_ip(Some(loopback), &headers),
            Some("2001:db8::1".parse().unwrap())
        );
    }

    #[test]
    fn trusted_peer_multi_hop_picks_rightmost_untrusted() {
        // Architecture: Client (203.0.113.1) -> Proxy1 (198.51.100.2) -> Proxy2 (10.0.0.2) -> Ikenga (peer 127.0.0.1)
        // Trusted proxies: 127.0.0.1, 10.0.0.0/8
        // Hop list in XFF: "203.0.113.1, 198.51.100.2, 10.0.0.2"
        // 10.0.0.2 is trusted.
        // 198.51.100.2 is UNTRUSTED.
        // Right-most untrusted hop is 198.51.100.2!
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.1, 198.51.100.2, 10.0.0.2"),
        );

        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("198.51.100.2".parse().unwrap())
        );
    }

    #[test]
    fn trusted_peer_all_hops_trusted_picks_rightmost() {
        // Every hop is trusted: the right-most one is what the direct peer saw
        // on its own socket; the left-most could be client-prepended.
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8, 172.16.0.0/12");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.1, 172.16.0.5"),
        );

        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("172.16.0.5".parse().unwrap())
        );
    }

    /// A client inside a trusted range (10.0.0.5 behind nginx, 10.0.0.0/8
    /// trusted) rotating a prepended trusted-looking X-Forwarded-For value
    /// must not get a fresh address per request.
    #[test]
    fn trusted_range_client_cannot_rotate_prepended_hop() {
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        for n in 1..=20 {
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-forwarded-for",
                HeaderValue::from_str(&format!("10.9.9.{n}, 10.0.0.5")).unwrap(),
            );
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some("10.0.0.5".parse().unwrap()),
                "rotation {n}"
            );
        }
    }

    /// Multi-hop chains where the attacker prepends arbitrary (untrusted or
    /// garbage) values: the right-most untrusted hop is the address the
    /// nearest trusted proxy actually saw, whatever is to its left.
    #[test]
    fn attacker_prepended_values_never_win() {
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        for chain in [
            "9.9.9.9, 203.0.113.7",
            "1.1.1.1, 2.2.2.2, 3.3.3.3, 203.0.113.7",
            "garbage, 203.0.113.7",
            "9.9.9.9, 203.0.113.7, 10.0.0.2",
            "unknown, 9.9.9.9, 203.0.113.7, 10.0.0.2, 10.0.0.3",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", HeaderValue::from_str(chain).unwrap());
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some("203.0.113.7".parse().unwrap()),
                "{chain}"
            );
        }

        // The client sends its own X-Forwarded-For line and nginx
        // (`$proxy_add_x_forwarded_for`) appends a second: same answer.
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", HeaderValue::from_static("9.9.9.9"));
        headers.append("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("203.0.113.7".parse().unwrap())
        );
    }

    /// Malformed values fall back to the socket peer rather than to any
    /// client-supplied hop.
    #[test]
    fn malformed_values_fall_back_to_peer() {
        let tp = TrustedProxies::parse("127.0.0.1");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        for bad in [
            "",
            " , ",
            "garbage",
            "9.9.9.9, garbage",
            "1.2.3.4:5:6",
            "300.1.1.1",
            "fe80::1%eth0",
            "[2001:db8::1",
            "9.9.9.9, [not-v6]:80",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some(peer),
                "{bad:?}"
            );
        }
        // Not visible ASCII: the whole header is rejected.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_bytes(b"9.9.9.9, \xff203.0.113.7").unwrap(),
        );
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));

        // Forwarded mode: no `for=` at all, or a broken one.
        let tp = tp.with_header(ForwardHeader::Forwarded);
        for bad in [
            "proto=https",
            "for=",
            "for=\"[2001:db8::1\"",
            "for=9.9.9.9, for=unknown",
            "for=9.9.9.9, for=\"_hidden\"",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("forwarded", HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some(peer),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn ipv6_peers_hops_and_cidrs() {
        let tp = TrustedProxies::parse("::1, fd00::/8");
        let v6_peer: IpAddr = "::1".parse().unwrap();

        // Unbracketed, bracketed, bracketed-with-port, and IPv4 hops.
        for (xff, want) in [
            ("2001:db8::7", "2001:db8::7"),
            ("[2001:db8::7]", "2001:db8::7"),
            ("[2001:db8::7]:4711", "2001:db8::7"),
            ("2001:db8::7, fd00::3", "2001:db8::7"),
            ("203.0.113.7, fd12::1", "203.0.113.7"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", HeaderValue::from_str(xff).unwrap());
            assert_eq!(
                tp.resolve_client_ip(Some(v6_peer), &headers),
                Some(want.parse().unwrap()),
                "{xff}"
            );
        }

        // A v4 peer is not covered by a v6-only list.
        let v4_peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("9.9.9.9"));
        assert_eq!(tp.resolve_client_ip(Some(v4_peer), &headers), Some(v4_peer));

        // A v6 peer outside the list is returned as-is.
        let other: IpAddr = "2001:db8::99".parse().unwrap();
        assert_eq!(tp.resolve_client_ip(Some(other), &headers), Some(other));
    }

    /// A dual-stack listener reports an IPv4 proxy as `::ffff:a.b.c.d`; it
    /// must match an IPv4 entry, and mapped hops come back in IPv4 form.
    #[test]
    fn ipv4_mapped_peer_and_hops_are_canonicalised() {
        let tp = TrustedProxies::parse("127.0.0.1");
        let mapped_peer: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("::ffff:203.0.113.7"),
        );
        assert_eq!(
            tp.resolve_client_ip(Some(mapped_peer), &headers),
            Some("203.0.113.7".parse().unwrap())
        );
        // A mapped hop that is itself a trusted proxy is skipped.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.8, ::ffff:127.0.0.1"),
        );
        assert_eq!(
            tp.resolve_client_ip(Some(mapped_peer), &headers),
            Some("203.0.113.8".parse().unwrap())
        );
        // Untrusted mapped peer: returned exactly as given (old behaviour).
        let untrusted: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        assert_eq!(
            tp.resolve_client_ip(Some(untrusted), &headers),
            Some(untrusted)
        );
    }

    #[test]
    fn cidr_edges() {
        // Boundaries of a /24 and a /30.
        let tp = TrustedProxies::parse("10.1.2.0/24, 192.168.0.4/30");
        for (ip, want) in [
            ("10.1.2.0", true),
            ("10.1.2.255", true),
            ("10.1.1.255", false),
            ("10.1.3.0", false),
            ("192.168.0.4", true),
            ("192.168.0.7", true),
            ("192.168.0.3", false),
            ("192.168.0.8", false),
        ] {
            assert_eq!(tp.contains(&ip.parse().unwrap()), want, "{ip}");
        }

        // Host bits set: the network is what is matched.
        let tp = TrustedProxies::parse("127.0.0.9/8");
        assert!(tp.contains(&"127.200.0.1".parse().unwrap()));
        assert!(!tp.contains(&"128.0.0.1".parse().unwrap()));

        // /32 and /128 are single hosts; /0 is everything of that family.
        let tp = TrustedProxies::parse("192.0.2.1/32, 2001:db8::1/128");
        assert!(tp.contains(&"192.0.2.1".parse().unwrap()));
        assert!(!tp.contains(&"192.0.2.2".parse().unwrap()));
        assert!(tp.contains(&"2001:db8::1".parse().unwrap()));
        assert!(!tp.contains(&"2001:db8::2".parse().unwrap()));
        let tp = TrustedProxies::parse("0.0.0.0/0");
        assert!(tp.contains(&"8.8.8.8".parse().unwrap()));
        assert!(!tp.contains(&"::1".parse().unwrap()));

        // IPv6 prefix boundary.
        let tp = TrustedProxies::parse("2001:db8:0:1::/64");
        assert!(tp.contains(&"2001:db8:0:1:ffff:ffff:ffff:ffff".parse().unwrap()));
        assert!(!tp.contains(&"2001:db8:0:2::".parse().unwrap()));

        // Invalid tokens are dropped; an all-invalid list is "unset".
        for bad in [
            "127.0.0.1/33",
            "::1/129",
            "10.0.0.0/",
            "not-an-ip",
            "10.0.0.0/8/8",
        ] {
            let tp = TrustedProxies::parse(bad);
            assert!(tp.is_empty(), "{bad} should be rejected");
        }
        let tp = TrustedProxies::parse("127.0.0.1/33, 10.0.0.0/8");
        assert_eq!(tp.len(), 1);
    }

    #[test]
    fn from_settings_wires_both_values() {
        assert!(TrustedProxies::from_settings(None, Some("forwarded")).is_empty());
        let tp = TrustedProxies::from_settings(Some("127.0.0.1"), None);
        assert_eq!(tp.header(), ForwardHeader::XForwardedFor);
        let tp = TrustedProxies::from_settings(Some("127.0.0.1"), Some("forwarded"));
        assert_eq!(tp.header(), ForwardHeader::Forwarded);
        assert!(TrustedProxies::from_settings(Some(""), None).is_empty());
    }

    #[test]
    fn forwarded_header_rfc7239_parsing() {
        let tp = TrustedProxies::parse("127.0.0.1").with_header(ForwardHeader::Forwarded);
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            "forwarded",
            HeaderValue::from_static("for=192.0.2.43, for=\"[2001:db8:cafe::17]:4711\""),
        );

        // Right-most untrusted is [2001:db8:cafe::17]
        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("2001:db8:cafe::17".parse().unwrap())
        );
    }

    #[test]
    fn missing_or_invalid_headers_fallbacks_to_peer() {
        let tp = TrustedProxies::parse("127.0.0.1");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        // No headers
        let headers = HeaderMap::new();
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));

        // Invalid IP in header
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }

    /// Regression (gap audit, WP-B review): behind Caddy/nginx, which rewrite
    /// only `X-Forwarded-For` and pass a client's `Forwarded` through, a
    /// client rotating `Forwarded: for=9.9.9.N` must NOT get a fresh address
    /// per request. The default honours X-Forwarded-For only.
    #[test]
    fn default_ignores_client_forwarded_and_honours_proxy_xff() {
        let tp = TrustedProxies::parse("127.0.0.1");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        for n in 1..=20 {
            let mut headers = HeaderMap::new();
            // What Caddy wrote: the real client.
            headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.7"));
            // What the client injected, passed through untouched.
            headers.insert(
                "forwarded",
                HeaderValue::from_str(&format!("for=9.9.9.{n}")).unwrap(),
            );
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some("203.0.113.7".parse().unwrap()),
                "rotation {n} escaped the proxy-written address"
            );
        }
    }

    /// A `Forwarded` header alone (no X-Forwarded-For) is never read in the
    /// default mode: the address collapses to the peer, it is not spoofed.
    #[test]
    fn default_never_reads_forwarded_even_alone() {
        let tp = TrustedProxies::parse("127.0.0.1");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("forwarded", HeaderValue::from_static("for=9.9.9.9"));
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }

    /// The mirror image: a proxy configured as `forwarded` never consults a
    /// client-supplied X-Forwarded-For.
    #[test]
    fn forwarded_mode_ignores_xff() {
        let tp = TrustedProxies::parse("127.0.0.1").with_header(ForwardHeader::Forwarded);
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("9.9.9.9"));
        headers.insert("forwarded", HeaderValue::from_static("for=203.0.113.7"));
        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("203.0.113.7".parse().unwrap())
        );
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("9.9.9.9"));
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }

    /// An unparseable hop (a proxy writing `for=unknown`/`_hidden`, or junk)
    /// ends the walk: the client-controlled hop to its left is never chosen.
    #[test]
    fn unparseable_hop_stops_the_walk() {
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        let tp = TrustedProxies::parse("127.0.0.1").with_header(ForwardHeader::Forwarded);
        for written in ["for=unknown", "for=_hidden", "for=\"_x\""] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "forwarded",
                HeaderValue::from_str(&format!("for=9.9.9.9, {written}")).unwrap(),
            );
            assert_eq!(
                tp.resolve_client_ip(Some(peer), &headers),
                Some(peer),
                "{written} let the client hop through"
            );
        }

        let tp = TrustedProxies::parse("127.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("9.9.9.9, garbage"),
        );
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }

    #[test]
    fn forward_header_setting_parses() {
        assert_eq!(ForwardHeader::parse(""), ForwardHeader::XForwardedFor);
        assert_eq!(
            ForwardHeader::parse("X-Forwarded-For"),
            ForwardHeader::XForwardedFor
        );
        assert_eq!(ForwardHeader::parse("xff"), ForwardHeader::XForwardedFor);
        assert_eq!(
            ForwardHeader::parse(" Forwarded "),
            ForwardHeader::Forwarded
        );
        // Unknown never means "read both".
        assert_eq!(ForwardHeader::parse("both"), ForwardHeader::XForwardedFor);
        assert_eq!(
            TrustedProxies::parse("127.0.0.1").header,
            ForwardHeader::XForwardedFor
        );
    }

    /// Unset configuration is the pre-WP-B behaviour: the socket peer, whatever
    /// either header says, including from loopback.
    #[test]
    fn unset_is_raw_peer_for_every_header_combo() {
        let tp = TrustedProxies::parse("");
        assert!(tp.is_empty());
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("9.9.9.9"));
        headers.insert("forwarded", HeaderValue::from_static("for=8.8.8.8"));
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
        let tp = tp.with_header(ForwardHeader::Forwarded);
        assert_eq!(tp.resolve_client_ip(Some(peer), &headers), Some(peer));
    }
}
