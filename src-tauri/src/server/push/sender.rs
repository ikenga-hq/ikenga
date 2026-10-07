//! The outbound half: endpoint allowlist, one HTTP POST per subscription,
//! retries, and what each status means for the row.
//!
//! **SSRF.** Under T1 every push leaves as root, to a URL a client
//! supplied. An endpoint must be `https`, port 443, a DNS name (no IP
//! literal, no userinfo) ending in a known push-service suffix — checked at
//! subscribe **and** again at send. `--push-endpoint-host` adds suffixes;
//! `--push-allow-endpoint` allows one exact origin (http loopback included)
//! for a local end-to-end test.
//!
//! **Statuses.** 201/202 → success; 404/410 → the subscription is gone,
//! delete it; 429/5xx/timeout → retry (1 s, 5 s, 30 s; `Retry-After`
//! honoured; never past the TTL); 400/401/403/413 → count a failure. Logs
//! carry the endpoint **host**, a `sub_id` prefix and the status only.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use url::Url;

/// Push services a browser can hand us (Chrome/Edge-on-Android FCM,
/// Firefox autopush, Safari/iOS APNs web push, Edge WNS).
pub const DEFAULT_HOST_SUFFIXES: &[&str] = &[
    "fcm.googleapis.com",
    "push.services.mozilla.com",
    "push.apple.com",
    "notify.windows.com",
];

pub const MAX_ENDPOINT_LEN: usize = 2048;

/// Which endpoints this server will ever send to.
#[derive(Debug, Clone)]
pub struct EndpointPolicy {
    suffixes: Vec<String>,
    exact_origins: Vec<String>,
}

impl Default for EndpointPolicy {
    fn default() -> Self {
        Self::new(&[], &[])
    }
}

impl EndpointPolicy {
    pub fn new(extra_suffixes: &[String], exact_origins: &[String]) -> Self {
        let mut suffixes: Vec<String> = DEFAULT_HOST_SUFFIXES
            .iter()
            .map(|s| s.to_string())
            .collect();
        suffixes.extend(
            extra_suffixes
                .iter()
                .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|s| !s.is_empty()),
        );
        let exact_origins = exact_origins
            .iter()
            .filter_map(|o| Url::parse(o.trim()).ok())
            .map(|u| u.origin().ascii_serialization())
            .collect();
        Self {
            suffixes,
            exact_origins,
        }
    }

    pub fn exact_origins(&self) -> &[String] {
        &self.exact_origins
    }

    /// The endpoint's origin if it may be used; why not otherwise.
    pub fn check(&self, endpoint: &str) -> Result<(Url, String), &'static str> {
        if endpoint.len() > MAX_ENDPOINT_LEN {
            return Err("endpoint too long");
        }
        let url = Url::parse(endpoint).map_err(|_| "endpoint is not a URL")?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err("endpoint must not carry credentials");
        }
        if url.fragment().is_some() {
            return Err("endpoint must not carry a fragment");
        }
        let origin = url.origin().ascii_serialization();
        if self.exact_origins.iter().any(|o| o == &origin) {
            return Ok((url, origin));
        }
        if url.scheme() != "https" {
            return Err("endpoint must be https");
        }
        if url.port_or_known_default() != Some(443) {
            return Err("endpoint must use port 443");
        }
        let host = match url.host() {
            Some(url::Host::Domain(h)) => h.to_ascii_lowercase(),
            Some(_) => return Err("endpoint host must be a name, not an IP address"),
            None => return Err("endpoint has no host"),
        };
        let allowed = self
            .suffixes
            .iter()
            .any(|s| host == *s || host.ends_with(&format!(".{s}")));
        if !allowed {
            return Err("endpoint is not a known push service");
        }
        Ok((url, origin))
    }
}

/// One push request, ready to POST.
#[derive(Clone)]
pub struct PushRequest {
    pub endpoint: String,
    pub authorization: String,
    pub ttl: u32,
    pub urgency: &'static str,
    pub topic: &'static str,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for PushRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushRequest")
            .field("ttl", &self.ttl)
            .field("urgency", &self.urgency)
            .field("topic", &self.topic)
            .field("len", &self.body.len())
            .finish()
    }
}

/// A push service's answer: the status and any `Retry-After`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushResponse {
    pub status: u16,
    pub retry_after: Option<Duration>,
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The HTTP leg, swappable for tests. `Err` is a transport failure
/// (timeout, connect, TLS), retried like a 5xx.
pub trait PushTransport: Send + Sync {
    fn post<'a>(&'a self, req: &'a PushRequest) -> BoxFuture<'a, Result<PushResponse, String>>;
}

/// The real client: rustls, https only (an exact test origin aside), no
/// redirects, 10 s timeout, the operator's proxy env honoured. Not the
/// broker's loopback client.
pub struct HttpTransport {
    client: reqwest::Client,
}

impl HttpTransport {
    pub fn new(allow_http: bool) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .https_only(!allow_http)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .user_agent(concat!("ikenga-server/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { client })
    }
}

impl PushTransport for HttpTransport {
    fn post<'a>(&'a self, req: &'a PushRequest) -> BoxFuture<'a, Result<PushResponse, String>> {
        Box::pin(async move {
            let res = self
                .client
                .post(&req.endpoint)
                .header("authorization", &req.authorization)
                .header("content-encoding", "aes128gcm")
                .header("content-type", "application/octet-stream")
                .header("ttl", req.ttl.to_string())
                .header("urgency", req.urgency)
                .header("topic", req.topic)
                .body(req.body.clone())
                .send()
                .await
                // Never the URL: the endpoint path is a capability.
                .map_err(|e| {
                    if e.is_timeout() {
                        "timeout".to_string()
                    } else if e.is_connect() {
                        "connect failed".to_string()
                    } else {
                        "request failed".to_string()
                    }
                })?;
            let retry_after = res
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            Ok(PushResponse {
                status: res.status().as_u16(),
                retry_after,
            })
        })
    }
}

/// What a send did to its subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Delivered(u16),
    /// 404 / 410: the row must be deleted.
    Gone(u16),
    /// A 4xx that retrying can't fix, or retries exhausted (`0` = transport).
    Failed(u16),
}

/// The retry schedule (tests shrink it).
#[derive(Debug, Clone)]
pub struct Backoff(pub Vec<Duration>);

impl Default for Backoff {
    fn default() -> Self {
        Self(vec![
            Duration::from_secs(1),
            Duration::from_secs(5),
            Duration::from_secs(30),
        ])
    }
}

/// Send one request, retrying 429 / 5xx / transport errors on `backoff`,
/// never waiting past the message's TTL.
pub async fn send_with_retries(
    transport: &dyn PushTransport,
    req: &PushRequest,
    backoff: &Backoff,
) -> Outcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(req.ttl as u64);
    let mut attempt = 0usize;
    loop {
        let (status, retry_after) = match transport.post(req).await {
            Ok(r) => match r.status {
                200..=299 => return Outcome::Delivered(r.status),
                404 | 410 => return Outcome::Gone(r.status),
                429 | 500..=599 => (r.status, r.retry_after),
                other => return Outcome::Failed(other),
            },
            Err(_) => (0, None),
        };
        let Some(delay) = backoff.0.get(attempt) else {
            return Outcome::Failed(status);
        };
        let wait = retry_after.map_or(*delay, |r| r.max(*delay));
        if tokio::time::Instant::now() + wait >= deadline {
            return Outcome::Failed(status);
        }
        tokio::time::sleep(wait).await;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn allowlist() {
        let p = EndpointPolicy::default();
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "https://web.push.apple.com/QGx",
            "https://wns2-par02p.notify.windows.com/w/?token=x",
        ] {
            assert!(p.check(ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("http://fcm.googleapis.com/x", "https"),
            ("https://fcm.googleapis.com:8443/x", "443"),
            ("https://127.0.0.1/x", "IP"),
            ("https://[::1]/x", "IP"),
            ("https://evil.example/x", "known"),
            ("https://fcm.googleapis.com.evil.example/x", "known"),
            ("https://evilfcm.googleapis.com/x", "known"),
            ("https://u:p@fcm.googleapis.com/x", "credentials"),
            ("not a url", "URL"),
        ] {
            let e = p.check(bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
        let local = EndpointPolicy::new(
            &["push.example.org".into()],
            &["http://127.0.0.1:9911".into()],
        );
        assert!(local.check("http://127.0.0.1:9911/sub/1").is_ok());
        assert!(local.check("http://127.0.0.1:9912/sub/1").is_err());
        assert!(local.check("https://a.push.example.org/x").is_ok());
    }

    struct Scripted(Mutex<Vec<Result<PushResponse, String>>>, Mutex<usize>);

    impl PushTransport for Scripted {
        fn post<'a>(&'a self, _r: &'a PushRequest) -> BoxFuture<'a, Result<PushResponse, String>> {
            Box::pin(async move {
                *self.1.lock().unwrap() += 1;
                self.0.lock().unwrap().remove(0)
            })
        }
    }

    fn ok(s: u16) -> Result<PushResponse, String> {
        Ok(PushResponse {
            status: s,
            retry_after: None,
        })
    }

    fn req(ttl: u32) -> PushRequest {
        PushRequest {
            endpoint: "https://fcm.googleapis.com/x".into(),
            authorization: "vapid t=x, k=y".into(),
            ttl,
            urgency: "high",
            topic: "permission",
            body: vec![1, 2, 3],
        }
    }

    #[tokio::test]
    async fn retries_then_classifies() {
        let fast = Backoff(vec![Duration::ZERO; 3]);
        let t = Scripted(
            Mutex::new(vec![ok(503), Err("timeout".into()), ok(201)]),
            Mutex::new(0),
        );
        assert_eq!(
            send_with_retries(&t, &req(60), &fast).await,
            Outcome::Delivered(201)
        );
        assert_eq!(*t.1.lock().unwrap(), 3);

        let t = Scripted(Mutex::new(vec![ok(410)]), Mutex::new(0));
        assert_eq!(
            send_with_retries(&t, &req(60), &fast).await,
            Outcome::Gone(410)
        );
        let t = Scripted(Mutex::new(vec![ok(404)]), Mutex::new(0));
        assert_eq!(
            send_with_retries(&t, &req(60), &fast).await,
            Outcome::Gone(404)
        );
        let t = Scripted(Mutex::new(vec![ok(403)]), Mutex::new(0));
        assert_eq!(
            send_with_retries(&t, &req(60), &fast).await,
            Outcome::Failed(403)
        );

        let t = Scripted(Mutex::new(vec![ok(500); 4]), Mutex::new(0));
        assert_eq!(
            send_with_retries(&t, &req(60), &fast).await,
            Outcome::Failed(500)
        );
        assert_eq!(*t.1.lock().unwrap(), 4);
    }

    /// A retry that would land after the TTL isn't made.
    #[tokio::test]
    async fn never_retries_past_the_ttl() {
        let slow = Backoff(vec![Duration::from_secs(30)]);
        let t = Scripted(
            Mutex::new(vec![Ok(PushResponse {
                status: 429,
                retry_after: Some(Duration::from_secs(5)),
            })]),
            Mutex::new(0),
        );
        assert_eq!(
            send_with_retries(&t, &req(10), &slow).await,
            Outcome::Failed(429)
        );
        assert_eq!(*t.1.lock().unwrap(), 1);
    }
}
