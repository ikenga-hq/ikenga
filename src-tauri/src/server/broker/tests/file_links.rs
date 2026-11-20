//! box-files WP-16: the outside-link routes through the real broker router,
//! against fake children that serve `/internal/file-link/read` with the
//! **real** child read (`server::file_link_read::read_with` over a real
//! `PathGuard` on a temp home), or with a deliberately hostile answer. The
//! test ids are 06-file-links-security §9's.
//!
//! What these cannot show unprivileged: LS-04's uid half (a file of another
//! account's uid refused by the kernel) and the "no open() by the broker
//! pid" trace; LS-14's and LS-20's live-browser halves; LS-15's journald
//! half. Those are G-LINKSEC's live checks.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Query};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::access::file_links::{self as links, IMAGE_PREVIEW_MAX_BYTES, TEXT_PREVIEW_MAX_BYTES};
use crate::access::share::now_ms;
use crate::access::AccessStore;
use crate::executor::{Principal, PrincipalId};
use crate::server::auth::BoxFuture;
use crate::server::broker::children::{tests::FakeProcess, ChildLauncher, LaunchedChild};
use crate::server::broker::file_links::{LinkConfig, LinkGate, PasscodeVerifier, COOKIE_NAME};
use crate::server::broker::{router, BrokerExtensions, BrokerState};
use crate::server::file_link_read::{read_with, ReadQuery, READ_PATH};
use crate::server::operator::password::LoginVerifier;
use crate::server::operator::{open_accounts, test_support, Opener};

const HOST: &str = "ik.test:4000";
const RIGHT: &str = "right-passcode";
/// The stand-in PHC: the fake verifier accepts `PHC_PREFIX + passcode` (the
/// CHECK wants the `$argon2id$` prefix).
const PHC_PREFIX: &str = "$argon2id$test$";

/// How a fake child answers `/internal/file-link/read`.
#[derive(Clone)]
enum Mode {
    /// The real child read over this home.
    Real { home: PathBuf },
    /// A hostile account's child: HTML type, a cookie, inline disposition,
    /// and these bytes, whatever was asked.
    Hostile { body: Arc<Vec<u8>> },
}

#[derive(Default)]
struct LinkLauncher {
    mode: Mutex<Option<Mode>>,
    launches: AtomicUsize,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    delay_ms: AtomicU64,
    /// Every read request a child got: (uri, headers).
    reads: Arc<Mutex<Vec<(String, HeaderMap)>>>,
    tokens: Mutex<Vec<(PrincipalId, String)>>,
}

impl LinkLauncher {
    fn set_mode(&self, m: Mode) {
        *self.mode.lock().unwrap() = Some(m);
    }
}

async fn child_read(
    mode: Mode,
    reads: Arc<Mutex<Vec<(String, HeaderMap)>>>,
    req: Request<Body>,
) -> Response {
    let (parts, _) = req.into_parts();
    reads
        .lock()
        .unwrap()
        .push((parts.uri.to_string(), parts.headers.clone()));
    match mode {
        Mode::Real { home } => {
            let Ok(Query(q)) = Query::<ReadQuery>::try_from_uri(&parts.uri) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let guard = crate::server::file_link_read::tests::guard_for(&home);
            read_with(guard, Some(home), q).await
        }
        Mode::Hostile { body } => {
            let mut res = Body::from(body.to_vec()).into_response();
            let h = res.headers_mut();
            h.insert("content-type", HeaderValue::from_static("text/html"));
            h.insert("set-cookie", HeaderValue::from_static("evil=1; Path=/"));
            h.insert(
                "content-disposition",
                HeaderValue::from_static("inline; filename=\"x.html\""),
            );
            h.insert("content-length", HeaderValue::from(body.len()));
            res
        }
    }
}

impl ChildLauncher for LinkLauncher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        Box::pin(async move {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            let delay = self.delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            let mode = self.mode.lock().unwrap().clone().expect("a mode is set");
            let reads = self.reads.clone();
            let app = Router::new().route(
                READ_PATH,
                get(move |req: Request<Body>| child_read(mode.clone(), reads.clone(), req)),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            self.tokens
                .lock()
                .unwrap()
                .push((principal.id, token.to_string()));
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(LaunchedChild {
                addr,
                process: Box::new(FakeProcess::default()),
            })
        })
    }
}

struct H {
    _tmp: tempfile::TempDir,
    _files: tempfile::TempDir,
    pool: SqlitePool,
    state: Arc<BrokerState>,
    launcher: Arc<LinkLauncher>,
    app: Router,
    home: PathBuf,
    owner: PrincipalId,
}

fn fake_verifier() -> PasscodeVerifier {
    Arc::new(|phc: &str, given: &str| phc.strip_prefix(PHC_PREFIX) == Some(given))
}

async fn harness(enabled: bool) -> H {
    let (tmp, root) = test_support::temp_root();
    let pool = open_accounts(&root, Opener::Broker).await.unwrap();
    let store = AccessStore::attach_t1(pool.clone()).await.unwrap();
    let files = tempfile::tempdir().unwrap();
    let home = files.path().canonicalize().unwrap().join("home");
    std::fs::create_dir_all(home.join("data")).unwrap();
    let launcher = Arc::new(LinkLauncher::default());
    launcher.set_mode(Mode::Real { home: home.clone() });
    let verifier = Arc::new(
        tokio::task::spawn_blocking(LoginVerifier::new)
            .await
            .unwrap(),
    );
    let mut state = BrokerState::new(pool.clone(), verifier, launcher.clone()).unwrap();
    state.file_links = Arc::new(
        LinkGate::new(LinkConfig {
            enabled,
            public_url: Some("https://box.example".into()),
            insecure_cookie: false,
            store: Some(store),
        })
        .with_verifier(fake_verifier()),
    );
    let state = Arc::new(state);
    let sessions = crate::server::auth::backend::open_session_store(&root)
        .await
        .unwrap();
    let app = router(
        state.clone(),
        sessions,
        &tmp.path().join("no-spa"),
        vec![],
        false,
        BrokerExtensions::default(),
    );
    let owner = super::insert_account(&pool, "ada", 20001, false).await;
    H {
        _tmp: tmp,
        _files: files,
        pool,
        state,
        launcher,
        app,
        home,
        owner,
    }
}

/// A link row, as WP-15's create would have written it.
struct Spec<'a> {
    path: &'a Path,
    name: &'a str,
    kind: &'a str,
    mime: &'a str,
    expires_at: Option<i64>,
    passcode: Option<&'a str>,
}

impl<'a> Spec<'a> {
    fn of(path: &'a Path, name: &'a str, kind: &'a str, mime: &'a str) -> Self {
        Self {
            path,
            name,
            kind,
            mime,
            expires_at: Some(now_ms() + 86_400_000),
            passcode: None,
        }
    }
}

/// Returns (token, link_id).
async fn add_link(h: &H, owner: PrincipalId, s: Spec<'_>) -> (String, String) {
    let minted = links::mint_token();
    let link_id = PrincipalId::new_v7().to_string();
    let size = std::fs::metadata(s.path).map(|m| m.len() as i64).unwrap_or(0);
    let created = now_ms() - 10_000;
    sqlx::query(
        "INSERT INTO file_links (link_id, token_sha256, owner_principal_id, path, name, \
           preview_kind, mime, size_at_create, created_at, created_via, expires_at, passcode_phc) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'session', ?, ?)",
    )
    .bind(&link_id)
    .bind(minted.sha256.as_slice())
    .bind(owner.to_string())
    .bind(s.path.to_string_lossy().into_owned())
    .bind(s.name)
    .bind(s.kind)
    .bind(s.mime)
    .bind(size)
    .bind(created)
    .bind(s.expires_at)
    .bind(s.passcode.map(|p| format!("{PHC_PREFIX}{p}")))
    .execute(&h.pool)
    .await
    .unwrap();
    (minted.token, link_id)
}

async fn pdf_link(h: &H) -> (String, String) {
    let p = h.home.join("r.pdf");
    std::fs::write(&p, b"%PDF-1.7 r").unwrap();
    add_link(h, h.owner, Spec::of(&p, "r.pdf", "pdf", "application/pdf")).await
}

fn ip(i: u32) -> IpAddr {
    IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 | i))
}

/// A request from `from` (as the peer address the broker sees).
fn req(method: &str, uri: &str, from: IpAddr) -> axum::http::request::Builder {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    b.extensions_mut()
        .unwrap()
        .insert(ConnectInfo(SocketAddr::new(from, 40_000)));
    b
}

async fn send(app: &Router, r: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(r).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

async fn get_from(
    h: &H,
    uri: &str,
    from: IpAddr,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut b = req("GET", uri, from);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    send(&h.app, b.body(Body::empty()).unwrap()).await
}

async fn post_passcode(
    h: &H,
    token: &str,
    from: IpAddr,
    passcode: &str,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("passcode", passcode)
        .finish();
    send(
        &h.app,
        req("POST", &format!("/s/{token}"), from)
            .header("origin", format!("https://{HOST}"))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}

/// `ikenga_link=<v>` from a response's `Set-Cookie`.
fn link_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{COOKIE_NAME}=")))
        .map(|v| v.split(';').next().unwrap().to_string())
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

const NOT_AVAILABLE: &str = "This link isn&#39;t available";

fn without_date(h: &HeaderMap) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = h
        .iter()
        .filter(|(k, _)| k.as_str() != "date")
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    v.sort();
    v
}

async fn status_of(h: &H, path: &str, from: IpAddr) -> StatusCode {
    get_from(h, path, from, None).await.0
}

// ─── LS-01 / LS-02 / LS-03: no oracle, no SPA ───────────────────────────────

/// LS-01: random well-formed tokens all answer the same 404 bytes; after 30
/// failures an address gets 429, a valid link included.
#[tokio::test]
async fn ls01_random_tokens_are_uniform_404s_and_failures_throttle_the_address() {
    let h = harness(true).await;
    let (valid, _) = pdf_link(&h).await;
    let mut first: Option<(Vec<(String, String)>, Vec<u8>)> = None;
    for i in 0..10_000u32 {
        let t = links::mint_token().token;
        // 20 per address: under both the bucket and the 30-failure budget.
        let (s, hd, b) = get_from(&h, &format!("/s/{t}"), ip(1 + i / 20), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let got = (without_date(&hd), b);
        match &first {
            None => first = Some(got),
            Some(f) => assert!(f == &got, "token {i} answered differently"),
        }
    }
    assert!(text(&first.unwrap().1).contains(NOT_AVAILABLE));
    assert_eq!(
        h.launcher.launches.load(Ordering::SeqCst),
        0,
        "unknown tokens launch nothing"
    );

    let me = ip(900_000);
    for _ in 0..30 {
        let t = links::mint_token().token;
        assert_eq!(status_of(&h, &format!("/s/{t}"), me).await, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        status_of(&h, &format!("/s/{valid}"), me).await,
        StatusCode::TOO_MANY_REQUESTS,
        "the 429 is not an oracle"
    );
    // Malformed tokens count as failures too.
    let other = ip(900_001);
    for _ in 0..30 {
        assert_eq!(status_of(&h, "/s/short", other).await, StatusCode::NOT_FOUND);
    }
    assert_eq!(
        status_of(&h, &format!("/s/{valid}"), other).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Another address is unaffected.
    assert_eq!(
        status_of(&h, &format!("/s/{valid}"), ip(900_002)).await,
        StatusCode::OK
    );
}

/// LS-01: the request bucket (120 a minute) also answers 429.
#[tokio::test]
async fn ls01_the_request_bucket_limits_a_fast_address() {
    let h = harness(true).await;
    let (valid, _) = pdf_link(&h).await;
    let me = ip(77);
    // The bucket refills while the loop runs, so: the first 100 pass, and
    // well before 240 (two minutes' budget) the address is slowed down.
    for i in 0..100 {
        assert_eq!(status_of(&h, &format!("/s/{valid}"), me).await, StatusCode::OK, "{i}");
    }
    let mut throttled = false;
    for _ in 0..140 {
        if status_of(&h, &format!("/s/{valid}"), me).await == StatusCode::TOO_MANY_REQUESTS {
            throttled = true;
            break;
        }
    }
    assert!(throttled);
}

/// LS-02: unknown, revoked, expired, owner-disabled, file-deleted and
/// setting-off answer identical status, headers and bytes.
#[tokio::test]
async fn ls02_every_unavailable_reason_answers_the_same_bytes() {
    let h = harness(true).await;
    let now = now_ms();
    let (revoked, revoked_id) = pdf_link(&h).await;
    sqlx::query(
        "UPDATE file_links SET revoked_at = ?, revoked_by = ?, revoked_reason = 'owner' \
         WHERE link_id = ?",
    )
    .bind(now)
    .bind(h.owner.to_string())
    .bind(&revoked_id)
    .execute(&h.pool)
    .await
    .unwrap();
    let p = h.home.join("e.pdf");
    std::fs::write(&p, b"%PDF e").unwrap();
    let mut spec = Spec::of(&p, "e.pdf", "pdf", "application/pdf");
    spec.expires_at = Some(now - 9_000);
    let (expired, _) = add_link(&h, h.owner, spec).await;
    let bob = super::insert_account(&h.pool, "bob", 20002, false).await;
    let bp = h.home.join("b.pdf");
    std::fs::write(&bp, b"%PDF b").unwrap();
    let (disabled, _) = add_link(&h, bob, Spec::of(&bp, "b.pdf", "pdf", "application/pdf")).await;
    sqlx::query("UPDATE accounts SET disabled_at = 1 WHERE principal_id = ?")
        .bind(bob.to_string())
        .execute(&h.pool)
        .await
        .unwrap();
    let gp = h.home.join("gone.pdf");
    std::fs::write(&gp, b"%PDF g").unwrap();
    let (deleted, _) = add_link(
        &h,
        h.owner,
        Spec::of(&gp, "gone.pdf", "pdf", "application/pdf"),
    )
    .await;
    std::fs::remove_file(&gp).unwrap();
    let unknown = links::mint_token().token;

    let off = harness(false).await;
    let (off_valid, _) = pdf_link(&off).await;

    let mut answers = Vec::new();
    for (i, t) in [&unknown, &revoked, &expired, &disabled].iter().enumerate() {
        for suffix in ["", "/view", "/download"] {
            answers.push(get_from(&h, &format!("/s/{t}{suffix}"), ip(100 + i as u32), None).await);
        }
    }
    answers.push(get_from(&h, &format!("/s/{deleted}/download"), ip(200), None).await);
    answers.push(get_from(&h, &format!("/s/{deleted}/view"), ip(200), None).await);
    for suffix in ["", "/view", "/download"] {
        answers.push(get_from(&off, &format!("/s/{off_valid}{suffix}"), ip(300), None).await);
    }
    let (s0, h0, b0) = answers[0].clone();
    assert_eq!(s0, StatusCode::NOT_FOUND);
    assert!(text(&b0).contains(NOT_AVAILABLE));
    for (i, (s, hd, b)) in answers.iter().enumerate() {
        assert_eq!(*s, s0, "answer {i}");
        assert_eq!(without_date(hd), without_date(&h0), "answer {i}");
        assert_eq!(*b, b0, "answer {i}");
    }
}

/// LS-03: with the setting off, every /s path is the 404 page (never the
/// SPA), the routes exist, and nothing launches.
#[tokio::test]
async fn ls03_setting_off_answers_404_everywhere_and_never_the_spa() {
    let h = harness(false).await;
    let (valid, _) = pdf_link(&h).await;
    let paths = [
        format!("/s/{valid}"),
        format!("/s/{valid}/download"),
        format!("/s/{valid}/view"),
        "/s".to_string(),
        "/s/".to_string(),
        "/s/x/y".to_string(),
    ];
    for (i, path) in paths.iter().enumerate() {
        let (s, hd, b) = get_from(&h, path, ip(10 + i as u32), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(hd["content-type"], "text/html; charset=utf-8", "{path}");
        assert!(text(&b).contains(NOT_AVAILABLE), "{path}");
    }
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), 0);
    // Even on: an unknown sub-path and a wrong method are the same 404.
    let on = harness(true).await;
    let (t, _) = pdf_link(&on).await;
    let asks = [
        ("GET", "/s".to_string()),
        ("GET", "/s/".to_string()),
        ("GET", format!("/s/{t}/other")),
        ("GET", format!("/s/{t}/")),
        ("DELETE", format!("/s/{t}")),
        ("PUT", format!("/s/{t}/download")),
    ];
    for (i, (m, p)) in asks.iter().enumerate() {
        let mut b = req(m, p, ip(50 + i as u32));
        if *m != "GET" {
            b = b.header("origin", format!("https://{HOST}"));
        }
        let (s, _, body) = send(&on.app, b.body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{m} {p}");
        assert!(text(&body).contains(NOT_AVAILABLE), "{m} {p}");
    }
}

// ─── LS-04 / LS-06: the owner's child reads, exactly the stored path ────────

/// LS-04 (unprivileged half): the bytes come from the owner's child, asked
/// with its own bearer and the internal marker; nothing of the outsider's
/// request (range, cookie, caps, credentials, token) reaches it.
#[tokio::test]
async fn ls04_bytes_come_from_the_owners_child_with_nothing_of_the_outsider() {
    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    let (s, hd, b) = send(
        &h.app,
        req("GET", &format!("/s/{t}/download"), ip(1))
            .header("range", "bytes=0-1")
            .header("cookie", "ikenga_session=abc")
            .header("x-ikenga-caps", "files")
            .header("authorization", "Bearer outsider")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"%PDF-1.7 r");
    assert_eq!(hd["content-length"], "10");
    assert!(hd.get("accept-ranges").is_none());
    let reads = h.launcher.reads.lock().unwrap().clone();
    assert_eq!(reads.len(), 1);
    let (uri, ch) = &reads[0];
    assert!(uri.starts_with(READ_PATH), "{uri}");
    assert!(!uri.contains(&t), "the token never reaches the child");
    assert!(uri.contains("max="), "{uri}");
    assert_eq!(ch["x-ikenga-internal-call"], "1");
    assert_eq!(ch["x-ikenga-principal"], h.owner.to_string().as_str());
    assert!(ch.get("x-ikenga-caps").is_none());
    assert!(ch.get("range").is_none());
    assert!(ch.get("cookie").is_none());
    let tokens = h.launcher.tokens.lock().unwrap().clone();
    let child_token = tokens.iter().find(|(p, _)| *p == h.owner).unwrap().1.clone();
    assert_eq!(ch["authorization"], format!("Bearer {child_token}").as_str());
}

/// LS-06: replaced by a symlink (inside or outside the allowlist), a parent
/// replaced by a symlink, moved away → 404; moved back → served again.
#[tokio::test]
async fn ls06_a_swapped_or_moved_file_is_not_served() {
    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    let r = h.home.join("r.pdf");
    std::fs::write(h.home.join("s.pdf"), b"%PDF-1.7 s").unwrap();
    let dl = format!("/s/{t}/download");
    assert_eq!(status_of(&h, &dl, ip(1)).await, StatusCode::OK);
    std::fs::rename(&r, h.home.join("r.bak")).unwrap();
    std::os::unix::fs::symlink(h.home.join("s.pdf"), &r).unwrap();
    assert_eq!(status_of(&h, &dl, ip(2)).await, StatusCode::NOT_FOUND, "symlink inside");
    std::fs::remove_file(&r).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", &r).unwrap();
    assert_eq!(status_of(&h, &dl, ip(3)).await, StatusCode::NOT_FOUND, "symlink outside");
    std::fs::remove_file(&r).unwrap();
    assert_eq!(status_of(&h, &dl, ip(4)).await, StatusCode::NOT_FOUND, "moved away");
    std::fs::rename(h.home.join("r.bak"), &r).unwrap();
    let (s, _, b) = get_from(&h, &dl, ip(5), None).await;
    assert_eq!(s, StatusCode::OK, "moved back");
    assert_eq!(b, b"%PDF-1.7 r");

    // A parent directory replaced by a symlink.
    std::fs::create_dir_all(h.home.join("dir")).unwrap();
    let a = h.home.join("dir/a.txt");
    std::fs::write(&a, b"a").unwrap();
    let (t2, _) = add_link(
        &h,
        h.owner,
        Spec::of(&a, "a.txt", "text", "text/plain; charset=utf-8"),
    )
    .await;
    let view = format!("/s/{t2}/view");
    assert_eq!(status_of(&h, &view, ip(6)).await, StatusCode::OK);
    std::fs::rename(h.home.join("dir"), h.home.join("dir2")).unwrap();
    std::os::unix::fs::symlink(h.home.join("dir2"), h.home.join("dir")).unwrap();
    assert_eq!(status_of(&h, &view, ip(7)).await, StatusCode::NOT_FOUND);
}

// ─── LS-09 / LS-10 / LS-25: what the outsider's browser is given ────────────

/// LS-09: HTML and SVG are download-only octet-stream; an "image" that is
/// really HTML is served as the row's image type with nosniff and a sandbox;
/// the store can't name text/html at all.
#[tokio::test]
async fn ls09_no_script_type_is_ever_served_inline() {
    let h = harness(true).await;
    for (i, name) in ["x.html", "x.svg"].iter().enumerate() {
        let p = h.home.join(name);
        std::fs::write(&p, b"<script>alert(1)</script>").unwrap();
        let (t, _) = add_link(&h, h.owner, Spec::of(&p, name, "none", links::OCTET_STREAM)).await;
        let from = ip(10 + i as u32);
        assert_eq!(status_of(&h, &format!("/s/{t}/view"), from).await, StatusCode::NOT_FOUND);
        let (s, hd, _) = get_from(&h, &format!("/s/{t}/download"), from, None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(hd["content-type"], links::OCTET_STREAM);
        assert!(hd["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;"));
        // The page offers no preview for them.
        let (_, _, page) = get_from(&h, &format!("/s/{t}"), from, None).await;
        assert!(!text(&page).contains("/view"), "{name}");
    }
    let p = h.home.join("x.png");
    std::fs::write(&p, b"<html><script>alert(1)</script></html>").unwrap();
    let (t, _) = add_link(&h, h.owner, Spec::of(&p, "x.png", "image", "image/png")).await;
    let (s, hd, _) = get_from(&h, &format!("/s/{t}/view"), ip(20), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hd["content-type"], "image/png");
    assert_eq!(hd["x-content-type-options"], "nosniff");
    assert!(hd["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("sandbox"));
    assert!(hd["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("inline;"));
    // The CHECK refuses an HTML row outright.
    let bad = sqlx::query(
        "INSERT INTO file_links (link_id, token_sha256, owner_principal_id, path, name, \
           preview_kind, mime, size_at_create, created_at, created_via) \
         VALUES (?, ?, ?, '/x.html', 'x.html', 'none', 'text/html', 1, 1, 'session')",
    )
    .bind(PrincipalId::new_v7().to_string())
    .bind(links::mint_token().sha256.as_slice())
    .bind(h.owner.to_string())
    .execute(&h.pool)
    .await;
    assert!(bad.is_err());
}

/// LS-10: a hostile child's headers are ignored and an over-long answer is
/// refused (the stream cap itself: `cap_stream`'s unit test).
#[tokio::test]
async fn ls10_a_hostile_childs_headers_and_lengths_are_not_trusted() {
    let h = harness(true).await;
    let p = h.home.join("n.txt");
    std::fs::write(&p, b"notes").unwrap();
    let (t, _) = add_link(
        &h,
        h.owner,
        Spec::of(&p, "n.txt", "text", "text/plain; charset=utf-8"),
    )
    .await;
    h.launcher.set_mode(Mode::Hostile {
        body: Arc::new(b"<script>alert(1)</script>".to_vec()),
    });
    let (s, hd, b) = get_from(&h, &format!("/s/{t}/view"), ip(1), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(hd["content-type"], "text/plain; charset=utf-8");
    assert!(hd.get("set-cookie").is_none());
    let cd = hd["content-disposition"].to_str().unwrap().to_string();
    assert!(cd.contains("filename=\"n.txt\""), "{cd}");
    assert!(!cd.contains("x.html"), "{cd}");
    assert_eq!(b.len(), 25);
    let (_, hd, _) = get_from(&h, &format!("/s/{t}/download"), ip(2), None).await;
    assert!(hd["content-disposition"]
        .to_str()
        .unwrap()
        .starts_with("attachment;"));
    assert!(hd.get("set-cookie").is_none());

    // Longer than the text preview limit: refused for /view. The running
    // child keeps its old mode, so stop it and let the next one start.
    h.launcher.set_mode(Mode::Hostile {
        body: Arc::new(vec![b'a'; TEXT_PREVIEW_MAX_BYTES as usize + 1]),
    });
    h.state.children.stop(h.owner).await;
    assert_eq!(status_of(&h, &format!("/s/{t}/view"), ip(3)).await, StatusCode::NOT_FOUND);
}

/// LS-25: a file grown past the image preview limit → /view 404, /download 200.
#[tokio::test]
async fn ls25_a_file_grown_past_its_preview_limit_is_download_only() {
    let h = harness(true).await;
    let p = h.home.join("big.png");
    std::fs::write(&p, b"png").unwrap();
    let (t, _) = add_link(&h, h.owner, Spec::of(&p, "big.png", "image", "image/png")).await;
    assert_eq!(status_of(&h, &format!("/s/{t}/view"), ip(1)).await, StatusCode::OK);
    // Sparse: costs no disk.
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(IMAGE_PREVIEW_MAX_BYTES + 1)
        .unwrap();
    assert_eq!(status_of(&h, &format!("/s/{t}/view"), ip(2)).await, StatusCode::NOT_FOUND);
    let res = h
        .app
        .clone()
        .oneshot(
            req("GET", &format!("/s/{t}/download"), ip(3))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()["content-length"],
        (IMAGE_PREVIEW_MAX_BYTES + 1).to_string().as_str()
    );
}

// ─── LS-11 / LS-12 / LS-13: the passcode ────────────────────────────────────

async fn passcode_link(h: &H, file: &str) -> (String, String) {
    let p = h.home.join(file);
    std::fs::write(&p, b"%PDF p").unwrap();
    let mut spec = Spec::of(&p, file, "pdf", "application/pdf");
    spec.passcode = Some(RIGHT);
    add_link(h, h.owner, spec).await
}

/// LS-11: 5 wrong per address then 429 without a verification; 20 wrong
/// across addresses lock the link; the right passcode is refused while
/// locked.
#[tokio::test]
async fn ls11_passcode_guesses_are_throttled_then_lock_the_link() {
    let h = harness(true).await;
    let (t, link_id) = passcode_link(&h, "r.pdf").await;
    let gate = h.state.file_links.clone();
    let a = ip(1);
    for i in 0..5 {
        let (s, _, b) = post_passcode(&h, &t, a, "wrong-guess").await;
        assert_eq!(s, StatusCode::OK, "guess {i}");
        assert!(text(&b).contains("passcode isn"), "guess {i}");
    }
    let before = gate.verifications.load(Ordering::SeqCst);
    let (s, _, _) = post_passcode(&h, &t, a, RIGHT).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        gate.verifications.load(Ordering::SeqCst),
        before,
        "no Argon2 run"
    );

    let mut last = StatusCode::OK;
    for addr in 2..=4 {
        for _ in 0..5 {
            last = post_passcode(&h, &t, ip(addr), "wrong-guess").await.0;
        }
    }
    assert_eq!(last, StatusCode::FORBIDDEN, "the 20th wrong guess locks it");
    let (until, failures): (Option<i64>, i64) = sqlx::query_as(
        "SELECT passcode_locked_until, passcode_failures FROM file_links WHERE link_id = ?",
    )
    .bind(&link_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert!(until.unwrap() > now_ms());
    assert_eq!(failures, 0, "reset on lock");
    let (s, hd, b) = post_passcode(&h, &t, ip(9), RIGHT).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(link_cookie(&hd).is_none());
    assert!(text(&b).contains("locked"));
    let (s, _, b) = get_from(&h, &format!("/s/{t}"), ip(9), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(text(&b).contains("locked"));
}

/// LS-12: the right passcode sets a path-scoped cookie that opens the page
/// and the bytes; a tampered or another link's cookie doesn't; after revoke
/// even a valid cookie gets the 404. (Another boot's key: the unit test.)
#[tokio::test]
async fn ls12_the_unlock_cookie_is_bound_and_never_outlives_the_link() {
    let h = harness(true).await;
    let (t, link_id) = passcode_link(&h, "r.pdf").await;
    let (t2, _) = passcode_link(&h, "r2.pdf").await;

    let (s, _, b) = get_from(&h, &format!("/s/{t}"), ip(1), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(text(&b).contains("name=\"passcode\""));
    assert!(!text(&b).contains("r.pdf"), "the name waits for the passcode");
    assert_eq!(
        status_of(&h, &format!("/s/{t}/download"), ip(1)).await,
        StatusCode::NOT_FOUND
    );

    let (s, hd, _) = post_passcode(&h, &t, ip(1), RIGHT).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(hd["location"], format!("/s/{t}").as_str());
    let set = hd["set-cookie"].to_str().unwrap().to_string();
    assert!(set.contains(&format!("Path=/s/{t}")), "{set}");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.contains("Secure"));
    let cookie = link_cookie(&hd).unwrap();

    let (s, _, b) = get_from(&h, &format!("/s/{t}"), ip(1), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(text(&b).contains("r.pdf"));
    let (s, _, _) = get_from(&h, &format!("/s/{t}/download"), ip(1), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    // Another link's page with this cookie: the form.
    let (_, _, b) = get_from(&h, &format!("/s/{t2}"), ip(2), Some(&cookie)).await;
    assert!(text(&b).contains("name=\"passcode\""));
    // Tampered.
    let mut tampered = cookie.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    let (_, _, b) = get_from(&h, &format!("/s/{t}"), ip(3), Some(&tampered)).await;
    assert!(text(&b).contains("name=\"passcode\""));
    // Revoked: the valid cookie gets the uniform 404.
    sqlx::query(
        "UPDATE file_links SET revoked_at = 1, revoked_by = 'x', revoked_reason = 'owner' \
         WHERE link_id = ?",
    )
    .bind(&link_id)
    .execute(&h.pool)
    .await
    .unwrap();
    for suffix in ["", "/download"] {
        let (s, _, b) = get_from(&h, &format!("/s/{t}{suffix}"), ip(4), Some(&cookie)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert!(text(&b).contains(NOT_AVAILABLE));
    }
}

/// LS-13: a cross-origin passcode POST is refused by the Origin gate and
/// spends nothing.
#[tokio::test]
async fn ls13_a_cross_origin_passcode_post_is_refused() {
    let h = harness(true).await;
    let (t, link_id) = passcode_link(&h, "r.pdf").await;
    let (s, _, _) = send(
        &h.app,
        req("POST", &format!("/s/{t}"), ip(1))
            .header("origin", "https://evil.example")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("passcode=wrong-guess"))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h.state.file_links.verifications.load(Ordering::SeqCst), 0);
    let failures: i64 =
        sqlx::query_scalar("SELECT passcode_failures FROM file_links WHERE link_id = ?")
            .bind(&link_id)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(failures, 0);
}

// ─── LS-14 / LS-15 / LS-19: headers, logs, the app's cookies ────────────────

fn assert_page_headers(hd: &HeaderMap, what: &str) {
    let csp = hd["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("frame-ancestors 'none'"), "{what}: {csp}");
    assert!(csp.contains("default-src 'none'"), "{what}: {csp}");
    assert!(!csp.contains("script-src"), "{what}: {csp}");
    assert_eq!(hd["x-frame-options"], "DENY", "{what}");
    assert_eq!(hd["cache-control"], "no-store", "{what}");
    assert_eq!(hd["cross-origin-opener-policy"], "same-origin", "{what}");
}

fn assert_common_headers(hd: &HeaderMap, what: &str) {
    assert_eq!(hd["referrer-policy"], "no-referrer", "{what}");
    assert_eq!(hd["x-robots-tag"], "noindex, nofollow, noarchive", "{what}");
    assert_eq!(hd["x-content-type-options"], "nosniff", "{what}");
}

/// LS-14 / LS-15: framing and referrer headers on every /s/* answer.
#[tokio::test]
async fn ls14_ls15_every_answer_carries_the_link_headers() {
    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    let (tp, _) = passcode_link(&h, "q.pdf").await;
    let unknown = links::mint_token().token;
    let pages = [
        format!("/s/{t}"),
        format!("/s/{tp}"),
        format!("/s/{unknown}"),
        "/s/x".to_string(),
    ];
    for (i, path) in pages.iter().enumerate() {
        let (_, hd, _) = get_from(&h, path, ip(10 + i as u32), None).await;
        assert_page_headers(&hd, path);
        assert_common_headers(&hd, path);
    }
    for (i, path) in [format!("/s/{t}/view"), format!("/s/{t}/download")]
        .iter()
        .enumerate()
    {
        let (s, hd, _) = get_from(&h, path, ip(20 + i as u32), None).await;
        assert_eq!(s, StatusCode::OK);
        assert_common_headers(&hd, path);
        assert_eq!(hd["cross-origin-resource-policy"], "same-origin");
        assert_eq!(hd["cache-control"], "private, no-store");
        let csp = hd["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("frame-ancestors 'self'") && csp.contains("sandbox"),
            "{csp}"
        );
    }
    let (_, hd, _) = post_passcode(&h, &tp, ip(30), RIGHT).await;
    assert_common_headers(&hd, "303");
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// LS-15: with every log level on, no token, cookie or passcode appears in
/// the logs of a run that opens, unlocks, guesses, previews, downloads and
/// misses.
#[tokio::test]
async fn ls15_no_token_is_ever_logged() {
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    let (tp, tp_id) = passcode_link(&h, "q.pdf").await;
    let unknown = links::mint_token().token;
    get_from(&h, &format!("/s/{t}"), ip(1), None).await;
    get_from(&h, &format!("/s/{t}/view"), ip(1), None).await;
    get_from(&h, &format!("/s/{t}/download"), ip(1), None).await;
    post_passcode(&h, &tp, ip(2), "wrong-guess").await;
    let (_, hd, _) = post_passcode(&h, &tp, ip(2), RIGHT).await;
    let cookie = link_cookie(&hd).unwrap();
    get_from(&h, &format!("/s/{tp}/download"), ip(2), Some(&cookie)).await;
    get_from(&h, &format!("/s/{unknown}"), ip(3), None).await;
    std::fs::remove_file(h.home.join("r.pdf")).unwrap();
    get_from(&h, &format!("/s/{t}/download"), ip(4), None).await;

    let logs = String::from_utf8_lossy(&buf.0.lock().unwrap()).into_owned();
    assert!(
        logs.contains(&tp_id),
        "the capture works (the wrong guess names the link id)"
    );
    for token in [&t, &tp, &unknown] {
        assert!(!logs.contains(token.as_str()), "a token reached the logs");
    }
    let cookie_value = cookie.split_once('=').unwrap().1;
    assert!(!logs.contains(cookie_value), "the unlock cookie reached the logs");
    assert!(!logs.contains(RIGHT), "a passcode reached the logs");
}

/// LS-19: a signed-in browser's session is neither touched nor exposed by
/// /s/*: no Set-Cookie but ikenga_link, and the session still works.
#[tokio::test]
async fn ls19_the_apps_session_is_never_touched() {
    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    let (tp, _) = passcode_link(&h, "q.pdf").await;
    let session = super::login_cookie(&h.app, "ada").await;
    let paths = [
        format!("/s/{t}"),
        format!("/s/{t}/view"),
        format!("/s/{t}/download"),
        format!("/s/{tp}"),
    ];
    for (i, path) in paths.iter().enumerate() {
        let (_, hd, _) = get_from(&h, path, ip(10 + i as u32), Some(&session)).await;
        assert!(hd.get("set-cookie").is_none(), "{path}");
    }
    let (_, hd, _) = send(
        &h.app,
        req("POST", &format!("/s/{tp}"), ip(20))
            .header("origin", format!("https://{HOST}"))
            .header("cookie", &session)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!("passcode={RIGHT}")))
            .unwrap(),
    )
    .await;
    let set: Vec<String> = hd
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();
    assert_eq!(set.len(), 1, "{set:?}");
    assert!(set[0].starts_with(&format!("{COOKIE_NAME}=")));
    let (s, _) = super::me(&h.app, &session).await;
    assert_eq!(s, StatusCode::OK, "the session still works");
}

// ─── LS-18 / LS-21 / LS-22: launches, owners, names ─────────────────────────

/// LS-18: parallel downloads for cold owners launch at most two children at
/// once (the rest 503); unknown tokens launch none; a link holds at most four
/// streams; the page alone launches nothing.
#[tokio::test]
async fn ls18_cold_launches_and_streams_are_bounded() {
    let h = harness(true).await;
    h.launcher.delay_ms.store(300, Ordering::SeqCst);
    let mut targets = Vec::new();
    for i in 0..5u32 {
        let owner = super::insert_account(&h.pool, &format!("u{i}"), 21_000 + i, false).await;
        let p = h.home.join(format!("f{i}.pdf"));
        std::fs::write(&p, b"%PDF f").unwrap();
        let (t, _) = add_link(&h, owner, Spec::of(&p, "f.pdf", "pdf", "application/pdf")).await;
        targets.push(t);
    }
    let unknown: Vec<String> = (0..20).map(|_| links::mint_token().token).collect();
    let mut futs = Vec::new();
    let all: Vec<&String> = targets.iter().chain(&targets).chain(&targets).collect();
    for (i, t) in all.iter().enumerate() {
        let r = req("GET", &format!("/s/{t}/download"), ip(100 + i as u32))
            .body(Body::empty())
            .unwrap();
        futs.push(h.app.clone().oneshot(r));
    }
    for (i, t) in unknown.iter().enumerate() {
        let r = req("GET", &format!("/s/{t}/download"), ip(200 + i as u32))
            .body(Body::empty())
            .unwrap();
        futs.push(h.app.clone().oneshot(r));
    }
    let results = futures_util::future::join_all(futs).await;
    let statuses: Vec<StatusCode> = results.into_iter().map(|r| r.unwrap().status()).collect();
    assert!(h.launcher.max_in_flight.load(Ordering::SeqCst) <= 2);
    assert!(h.launcher.launches.load(Ordering::SeqCst) <= 5);
    assert!(
        statuses.contains(&StatusCode::SERVICE_UNAVAILABLE),
        "{statuses:?}"
    );
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert_eq!(
        statuses[15..]
            .iter()
            .filter(|s| **s == StatusCode::NOT_FOUND)
            .count(),
        20
    );

    // Streams per link: four held open, the fifth is refused.
    h.launcher.delay_ms.store(0, Ordering::SeqCst);
    let (t, _) = pdf_link(&h).await;
    let mut held = Vec::new();
    for i in 0..4u32 {
        let res = h
            .app
            .clone()
            .oneshot(
                req("GET", &format!("/s/{t}/download"), ip(300 + i))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        held.push(res);
    }
    assert_eq!(
        status_of(&h, &format!("/s/{t}/download"), ip(310)).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    held.pop();
    assert_eq!(
        status_of(&h, &format!("/s/{t}/download"), ip(311)).await,
        StatusCode::OK
    );
    drop(held);

    // The page alone never launches a child.
    let before = h.launcher.launches.load(Ordering::SeqCst);
    let cold = super::insert_account(&h.pool, "cold", 22_000, false).await;
    let p = h.home.join("c.pdf");
    std::fs::write(&p, b"%PDF c").unwrap();
    let (tc, _) = add_link(&h, cold, Spec::of(&p, "c.pdf", "pdf", "application/pdf")).await;
    assert_eq!(status_of(&h, &format!("/s/{tc}"), ip(400)).await, StatusCode::OK);
    assert_eq!(h.launcher.launches.load(Ordering::SeqCst), before);
}

/// LS-21: a disabled owner's links 404; enabled again, they serve.
#[tokio::test]
async fn ls21_a_disabled_owners_links_stop_serving() {
    let h = harness(true).await;
    let (t, _) = pdf_link(&h).await;
    for (disabled, want, base) in [
        (Some(1i64), StatusCode::NOT_FOUND, 1u32),
        (None, StatusCode::OK, 10u32),
    ] {
        sqlx::query("UPDATE accounts SET disabled_at = ? WHERE principal_id = ?")
            .bind(disabled)
            .bind(h.owner.to_string())
            .execute(&h.pool)
            .await
            .unwrap();
        for (i, s) in ["", "/view", "/download"].iter().enumerate() {
            assert_eq!(
                status_of(&h, &format!("/s/{t}{s}"), ip(base + i as u32)).await,
                want,
                "{disabled:?} {s}"
            );
        }
    }
}

/// LS-22: a right-to-left override in a stored name never reaches the page
/// or the header.
#[tokio::test]
async fn ls22_bidi_overrides_are_stripped_from_page_and_header() {
    let h = harness(true).await;
    let p = h.home.join("invoice.exe");
    std::fs::write(&p, b"MZ").unwrap();
    let (t, _) = add_link(
        &h,
        h.owner,
        Spec::of(&p, "invoice\u{202E}fdp.exe", "none", links::OCTET_STREAM),
    )
    .await;
    let (s, _, page) = get_from(&h, &format!("/s/{t}"), ip(1), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!text(&page).contains('\u{202E}'));
    assert!(text(&page).contains("invoicefdp.exe"));
    let (_, hd, _) = get_from(&h, &format!("/s/{t}/download"), ip(2), None).await;
    let cd = hd["content-disposition"].to_str().unwrap();
    assert!(!cd.contains("%E2%80%AE"), "{cd}");
}

/// The page itself (§5.1): name, size, host, expiry, preview and Download;
/// never the owner, the path or the link id.
#[tokio::test]
async fn the_page_shows_the_file_and_nothing_about_its_owner() {
    let h = harness(true).await;
    let (t, link_id) = pdf_link(&h).await;
    let (s, _, b) = get_from(&h, &format!("/s/{t}"), ip(1), None).await;
    assert_eq!(s, StatusCode::OK);
    let page = text(&b);
    assert!(page.contains("<h1>r.pdf</h1>"), "{page}");
    assert!(page.contains("10 bytes"));
    assert!(page.contains("Shared from box.example."));
    assert!(page.contains("Expires "));
    assert!(page.contains(&format!(
        "<object data=\"/s/{t}/view\" type=\"application/pdf\">"
    )));
    assert!(page.contains(&format!("href=\"/s/{t}/download\" download")));
    assert!(!page.contains(&h.owner.to_string()));
    assert!(!page.contains(&link_id));
    assert!(!page.contains(&*h.home.to_string_lossy()));
    assert!(!page.contains("<script"));
    assert!(page.contains("noindex"));
}
