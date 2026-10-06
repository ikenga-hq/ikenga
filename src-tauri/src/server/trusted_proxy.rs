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
//! When the direct TCP peer matches a trusted proxy, client address resolution walks
//! the forwarded hops from right to left (closest hop to furthest) and picks the
//! right-most untrusted hop. If all hops are trusted, the leftmost/origin hop is chosen.
//! If headers are absent or invalid, the socket peer address is returned as fallback.

use axum::http::HeaderMap;
use ipnet::IpNet;
use std::net::IpAddr;
use std::str::FromStr;

pub const TRUSTED_PROXIES_ENV: &str = "IKENGA_TRUSTED_PROXIES";

/// Collection of trusted IP subnets / addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustedProxies {
    nets: Vec<IpNet>,
}

impl TrustedProxies {
    /// Create an empty set (no trusted proxies; forwarded headers always ignored).
    pub fn empty() -> Self {
        Self { nets: Vec::new() }
    }

    /// Read trusted proxies from the `IKENGA_TRUSTED_PROXIES` environment variable.
    pub fn from_env() -> Self {
        match std::env::var(TRUSTED_PROXIES_ENV) {
            Ok(val) => Self::parse(&val),
            Err(_) => Self::empty(),
        }
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
        Self { nets }
    }

    /// Whether any trusted proxy is configured.
    pub fn is_empty(&self) -> bool {
        self.nets.is_empty()
    }

    /// Check if the given IP address is within any trusted proxy network.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        self.nets.iter().any(|net| net.contains(ip))
    }

    /// Resolve the effective client IP address from the direct peer address and request headers.
    ///
    /// Rules:
    /// 1. If `peer_ip` is `None`, return `None`.
    /// 2. If `peer_ip` is NOT in the trusted proxies list (or trusted proxies is empty),
    ///    return `peer_ip` directly. Never inspect forwarded headers from an untrusted peer.
    /// 3. If `peer_ip` IS trusted:
    ///    - Inspect `Forwarded` (RFC 7239 `for=...`) or `X-Forwarded-For`.
    ///    - Parse the chain of hops.
    ///    - Walk right-to-left (closest to furthest) and pick the first hop that is NOT trusted.
    ///    - If all hops are trusted, return the leftmost (origin) hop.
    ///    - If no parseable hops exist, return `peer_ip`.
    pub fn resolve_client_ip(
        &self,
        peer_ip: Option<IpAddr>,
        headers: &HeaderMap,
    ) -> Option<IpAddr> {
        let peer = peer_ip?;
        if !self.contains(&peer) {
            return Some(peer);
        }

        // Direct peer is trusted. Try parsing Forwarded (RFC 7239) first, then X-Forwarded-For.
        let hops = if let Some(hops) = parse_forwarded_header(headers) {
            hops
        } else if let Some(hops) = parse_x_forwarded_for_header(headers) {
            hops
        } else {
            Vec::new()
        };

        if hops.is_empty() {
            return Some(peer);
        }

        // Walk right-to-left: pick the right-most untrusted hop.
        for hop in hops.iter().rev() {
            if !self.contains(hop) {
                return Some(*hop);
            }
        }

        // If all hops in the chain are trusted, return the origin (leftmost) hop.
        hops.first().copied().or(Some(peer))
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

/// Resolve effective client IP using the process-level `IKENGA_TRUSTED_PROXIES` configuration.
pub fn client_ip(peer_ip: Option<IpAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    TrustedProxies::from_env().resolve_client_ip(peer_ip, headers)
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
fn parse_forwarded_header(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
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
                    // Note: preserve case if needed, but for IP parsing strip quotes & port
                    let raw_for = &param[param.len() - rest.len()..];
                    if let Some(ip) = parse_ip_or_bracketed_port(raw_for) {
                        all_hops.push(ip);
                    }
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
fn parse_x_forwarded_for_header(headers: &HeaderMap) -> Option<Vec<IpAddr>> {
    let mut all_hops = Vec::new();
    for val in headers.get_all("x-forwarded-for") {
        let s = val.to_str().ok()?;
        for token in s.split(',') {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if let Some(ip) = parse_ip_or_bracketed_port(token) {
                all_hops.push(ip);
            }
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
    fn trusted_peer_all_hops_trusted_picks_origin() {
        // If all hops in XFF are trusted (e.g. internal proxies), origin is the leftmost hop.
        let tp = TrustedProxies::parse("127.0.0.1, 10.0.0.0/8, 172.16.0.0/12");
        let peer: IpAddr = "127.0.0.1".parse().unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.1, 172.16.0.5"),
        );

        assert_eq!(
            tp.resolve_client_ip(Some(peer), &headers),
            Some("10.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn forwarded_header_rfc7239_parsing() {
        let tp = TrustedProxies::parse("127.0.0.1");
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
}
