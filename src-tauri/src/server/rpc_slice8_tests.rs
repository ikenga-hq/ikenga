//! Router tests for the WP-19 slice 8 arms: `pin_screenshot_write`,
//! `scaffold_agent_config`, `pkg_preview_manifest`, `pkg_discover_workspace`
//! and `pkg_scaffold`.
//!
//! House pattern (see `rpc_shell`'s tests): a literal `ServerConfig` →
//! `router_with` → `oneshot` POST `/api/rpc` with the bearer token. The data
//! dir, router home, `--pkgs-dir` and fs allowlist are temp dirs; nothing here
//! touches the real user's home or installs the process-global `fs_roots`.
//!
//! Each arm: the happy path compared with the desktop body over a twin dir
//! (shape parity), both argument spellings, the refusals (outside the
//! allowlist, inside the data dir, `..`, links live and dangling at every
//! node a write touches), and the no-`--data-dir` error. The desktop bodies'
//! regression tests (they still follow links, as before) are at the end.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::Router;
use base64::Engine as _;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::db::PaDb;
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::rpc_shell::PathGuard;
use crate::server::shared::confined_fs::SYMLINK_REFUSAL;
use crate::server::shared::projects::{create_project, CreateArgs};
use crate::server::shared::{agent_scaffold, comments, pkg_scaffold, pkg_workspace};
use crate::server::{router_with, ServerConfig};

const INSTALLED_ID: &str = "com.test.installed";

/// A daemon with `--data-dir`, a router home, a `--pkgs-dir` indexing one pkg
/// (`INSTALLED_ID`), and an fs allowlist of exactly `allowed/` — or, with
/// `allow_root`, the whole temp root, so the data dir and home are inside
/// the allowlist and only the reserved rule keeps the data dir out.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    home: PathBuf,
    allowed: PathBuf,
    outside: PathBuf,
    db: Arc<PaDb>,
    router: Router,
}

fn config(data_dir: Option<PathBuf>, pkgs_dir: Option<PathBuf>) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir,
        data_dir,
        auth_token: Some("tok".into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

fn manifest(id: &str) -> String {
    format!(r#"{{"id":"{id}","name":"N {id}","version":"0.1.0","ikenga_api":"1"}}"#)
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn present(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn fixture(allow_root: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home) = (root.join("data"), root.join("home"));
    let (allowed, outside, pkgs) = (
        root.join("allowed"),
        root.join("outside"),
        root.join("pkgs"),
    );
    for d in [&data, &home, &allowed, &outside, &pkgs] {
        std::fs::create_dir_all(d).unwrap();
    }
    write(
        &pkgs.join(INSTALLED_ID).join("manifest.json"),
        &manifest(INSTALLED_ID),
    );
    let roots_file = root.join("fs_roots.json");
    let allow = if allow_root { &root } else { &allowed };
    std::fs::write(&roots_file, json!({ "roots": [s(allow)] }).to_string()).unwrap();
    let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let router = router_with(
        config(Some(data.clone()), Some(pkgs)),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db.clone()),
        None,
        Some(home.clone()),
        PathGuard::roots(Arc::new(roots)),
    );
    Fx {
        _tmp: tmp,
        root,
        data,
        home,
        allowed,
        outside,
        db,
        router,
    }
}

/// No `--data-dir` (so no `PaDb`, no allowlist).
fn bare() -> Router {
    router_with(
        config(None, None),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        None,
        None,
        None,
        PathGuard::allowlist(),
    )
}

async fn send(router: &Router, body: String) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", "Bearer tok")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
    let res = send(router, json!({ "cmd": cmd, "args": args }).to_string()).await;
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn ok(router: &Router, cmd: &str, args: Value) -> Value {
    let res = rpc(router, cmd, args.clone()).await;
    assert_eq!(res["ok"], true, "{cmd} {args} → {res}");
    res.get("data").cloned().unwrap_or(Value::Null)
}

async fn err(router: &Router, cmd: &str, args: Value) -> String {
    let res = rpc(router, cmd, args.clone()).await;
    assert_eq!(res["ok"], false, "{cmd} {args} should fail → {res}");
    res["error"].as_str().unwrap().to_string()
}

fn wire<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap()
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

// ── no data dir ─────────────────────────────────────────────────────────────

/// Every slice-8 arm whose state (the screenshots dir, the allowlist, the
/// project DB) lives in `--data-dir` names the flag without one.
#[tokio::test]
async fn every_arm_without_data_dir_names_the_flag() {
    let r = bare();
    let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nxx");
    for (cmd, args) in [
        ("pin_screenshot_write", json!({ "base64Png": png })),
        (
            "scaffold_agent_config",
            json!({ "provider": "claude-code", "rootPath": "/tmp", "profile": "starter" }),
        ),
        ("pkg_preview_manifest", json!({ "installPath": "/tmp" })),
        ("pkg_discover_workspace", json!({ "workspaceDir": "/tmp" })),
        (
            "pkg_scaffold",
            json!({ "params": {
                "kind": "skill", "name": "N", "slug": "n",
                "description": "a description of twenty plus chars",
                "scope": "personal", "targetDir": "/tmp/x"
            } }),
        ),
    ] {
        let e = err(&r, cmd, args).await;
        assert!(e.starts_with(&format!("{cmd}: ")), "{cmd}: {e}");
        assert!(e.contains("--data-dir"), "{cmd}: {e}");
    }
    // No workspace dir at all reads nothing, so it is the desktop's `[]`.
    if std::env::var_os("IKENGA_WORKSPACE_DIR").is_none() {
        assert_eq!(ok(&r, "pkg_discover_workspace", json!({})).await, json!([]));
    }
}

// ── pin_screenshot_write ────────────────────────────────────────────────────

fn png(extra: &[u8]) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(extra);
    v
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The full flow: write → `comment_create` with that path → accepted and
/// stored as written. The file lands, canonical, directly in
/// `<data-dir>/pin-screenshots/` under a minted `<uuid>.png`.
#[tokio::test]
async fn pin_screenshot_write_feeds_comment_create() {
    let f = fixture(false);
    let r = &f.router;
    let bytes = png(b"element crop");
    let shots = f.data.join(comments::SCREENSHOTS_DIR);

    let a = ok(
        r,
        "pin_screenshot_write",
        json!({ "base64Png": b64(&bytes) }),
    )
    .await;
    let b = ok(
        r,
        "pin_screenshot_write",
        json!({ "base64_png": b64(&bytes) }),
    )
    .await;
    for p in [&a, &b] {
        let p = PathBuf::from(p.as_str().expect("a string path, as on the desktop"));
        assert_eq!(p.parent(), Some(shots.as_path()), "{p:?}");
        assert_eq!(p.extension().and_then(|e| e.to_str()), Some("png"));
        let stem = p.file_stem().unwrap().to_str().unwrap();
        assert!(uuid::Uuid::parse_str(stem).is_ok(), "minted name: {stem}");
        assert_eq!(std::fs::read(&p).unwrap(), bytes);
    }
    assert_ne!(a, b, "each write mints its own name");

    // Shape parity: the desktop body answers a string path too.
    let twin = f.root.join("twin");
    let desk = comments::write_screenshot(&twin, &b64(&bytes), comments::ShotLimit::Desktop)
        .expect("desktop body");
    assert!(
        desk.starts_with(&s(&twin.join(comments::SCREENSHOTS_DIR))),
        "{desk}"
    );

    let c = ok(
        r,
        "comment_create",
        json!({ "artifactPath": "a.html", "selector": "#x", "text": "t", "screenshotPath": a }),
    )
    .await;
    assert_eq!(c["screenshotPath"], a);

    // Outside `pin-screenshots` is still refused, even a real PNG in the data
    // dir or one the allowlist covers.
    std::fs::write(f.data.join("loose.png"), &bytes).unwrap();
    std::fs::write(f.allowed.join("pic.png"), &bytes).unwrap();
    for bad in [f.data.join("loose.png"), f.allowed.join("pic.png")] {
        let e = err(
            r,
            "comment_create",
            json!({ "artifactPath": "a", "selector": "s", "text": "t", "screenshotPath": s(&bad) }),
        )
        .await;
        assert!(e.contains("screenshotPath must be a file in"), "{e}");
    }
}

#[tokio::test]
async fn pin_screenshot_write_keeps_the_desktop_checks() {
    let f = fixture(false);
    let r = &f.router;
    let shots = f.data.join(comments::SCREENSHOTS_DIR);
    let e = err(
        r,
        "pin_screenshot_write",
        json!({ "base64Png": "!!not base64!!" }),
    )
    .await;
    assert!(
        e.starts_with("pin_screenshot_write: base64 decode: "),
        "{e}"
    );
    let e = err(
        r,
        "pin_screenshot_write",
        json!({ "base64Png": b64(b"GIF89a....") }),
    )
    .await;
    assert_eq!(e, "pin_screenshot_write: not a PNG (bad magic)");
    let e = err(r, "pin_screenshot_write", json!({})).await;
    assert!(e.contains("`base64Png` is required"), "{e}");
    assert!(
        !shots.exists() || std::fs::read_dir(&shots).unwrap().next().is_none(),
        "nothing written for a refused blob"
    );
}

/// The daemon-only cap. `/api/rpc`'s request-body limit is now
/// `RPC_BODY_LIMIT` (16 MiB, raised from axum's 2 MiB default for editor
/// saves), so the cap in the shared body is what refuses an oversized
/// screenshot — on the base64 length alone, before decoding or writing.
#[tokio::test]
async fn pin_screenshot_write_is_capped_on_the_daemon_only() {
    let f = fixture(false);
    // `/api/rpc`'s body limit is `RPC_BODY_LIMIT` (16 MiB, plans/file-editing),
    // no longer axum's 2 MB default, so an over-cap screenshot now reaches the
    // arm — and the arm's own cap is what refuses it, before anything is written.
    // (A body over the route limit is still a 413: rpc_files'
    // `fs_write_over_rpc_body_limit_is_413_and_writes_nothing`.)
    let big = b64(&png(&vec![0u8; comments::DAEMON_MAX_SCREENSHOT_BYTES]));
    let res = send(
        &f.router,
        json!({ "cmd": "pin_screenshot_write", "args": { "base64Png": big } }).to_string(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap())
            .unwrap();
    assert_eq!(body["ok"], false, "{body}");
    assert!(
        body["error"].as_str().unwrap_or_default().contains("screenshot too large"),
        "{body}"
    );
    assert!(!f.data.join(comments::SCREENSHOTS_DIR).exists());

    let cap = comments::ShotLimit::Daemon { max_bytes: 64 };
    let at_cap = png(&[7u8; 56]);
    assert_eq!(
        comments::decode_screenshot(&b64(&at_cap), cap).unwrap(),
        at_cap
    );
    for over in [png(&[7u8; 57]), png(&[7u8; 4096])] {
        let e = comments::decode_screenshot(&b64(&over), cap).unwrap_err();
        assert!(e.contains("screenshot too large"), "{e}");
    }
    // Refused on length before decoding: not even valid base64 gets that far.
    let e = comments::decode_screenshot(&"A".repeat(4096), cap).unwrap_err();
    assert!(e.contains("screenshot too large"), "{e}");
    // The desktop has no cap.
    let big = png(&vec![1u8; 4096]);
    assert_eq!(
        comments::decode_screenshot(&b64(&big), comments::ShotLimit::Desktop).unwrap(),
        big
    );
}

// ── scaffold_agent_config ───────────────────────────────────────────────────

fn agent_args(root: &Path, mode: &str) -> Value {
    json!({ "provider": "claude-code", "rootPath": s(root), "profile": "starter", "mode": mode })
}

/// Every file the starter tree writes, relative to `.claude/`.
fn starter_files() -> Vec<String> {
    let tmp = tempfile::tempdir().unwrap();
    agent_scaffold::scaffold(agent_scaffold::ScaffoldRequest {
        provider: "claude-code".into(),
        root_path: s(tmp.path()),
        profile: "starter".into(),
        mode: None,
    })
    .unwrap()
    .written
}

#[tokio::test]
async fn scaffold_agent_config_writes_the_desktop_tree() {
    let f = fixture(false);
    let r = &f.router;
    let root = f.allowed.join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let res = ok(r, "scaffold_agent_config", agent_args(&root, "augment")).await;

    // Shape and content parity with the desktop body over a twin root.
    let twin = f.root.join("twin");
    std::fs::create_dir_all(&twin).unwrap();
    let desk = agent_scaffold::scaffold(agent_scaffold::ScaffoldRequest {
        provider: "claude-code".into(),
        root_path: s(&twin),
        profile: "starter".into(),
        mode: Some("augment".into()),
    })
    .unwrap();
    assert_eq!(res, wire(&desk));
    assert_eq!(res["ok"], true);
    assert!(res["files_written"].as_u64().unwrap() > 0);
    for rel in starter_files() {
        assert_eq!(
            std::fs::read(root.join(".claude").join(&rel)).unwrap(),
            std::fs::read(twin.join(".claude").join(&rel)).unwrap(),
            "{rel}"
        );
    }

    // snake_case spelling; augment again skips everything.
    let again = ok(
        r,
        "scaffold_agent_config",
        json!({ "provider": "claude-code", "root_path": s(&root), "profile": "starter" }),
    )
    .await;
    assert_eq!(again["files_written"], 0);
    assert_eq!(
        again["skipped"].as_array().unwrap().len(),
        starter_files().len()
    );

    // replace overwrites a regular file in place.
    let marker = root.join(".claude/agents/release-coordinator.md");
    std::fs::write(&marker, "USER").unwrap();
    let rep = ok(r, "scaffold_agent_config", agent_args(&root, "replace")).await;
    assert_eq!(rep["ok"], true);
    assert_ne!(std::fs::read_to_string(&marker).unwrap(), "USER");

    let e = err(
        r,
        "scaffold_agent_config",
        json!({ "provider": "codex", "rootPath": s(&root), "profile": "starter" }),
    )
    .await;
    assert!(e.contains("unsupported provider: codex"), "{e}");
    let e = err(
        r,
        "scaffold_agent_config",
        json!({ "provider": "claude-code" }),
    )
    .await;
    assert!(e.contains("`rootPath` is required"), "{e}");
}

#[tokio::test]
async fn scaffold_agent_config_root_stays_in_the_allowlist_and_out_of_the_data_dir() {
    let f = fixture(false);
    let r = &f.router;
    let e = err(
        r,
        "scaffold_agent_config",
        agent_args(&f.outside, "augment"),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    assert!(!f.outside.join(".claude").exists());
    let escape = format!("{}/../outside", f.allowed.display());
    let e = err(
        r,
        "scaffold_agent_config",
        json!({ "provider": "claude-code", "rootPath": escape, "profile": "starter" }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    let e = err(
        r,
        "scaffold_agent_config",
        agent_args(&f.allowed.join("nope"), "augment"),
    )
    .await;
    assert!(e.contains("root_path is not a directory"), "{e}");

    // An allowlist that covers the data dir still cannot reach it.
    let f = fixture(true);
    for root in [f.data.clone(), f.data.join("sub")] {
        std::fs::create_dir_all(&root).unwrap();
        let e = err(
            &f.router,
            "scaffold_agent_config",
            agent_args(&root, "augment"),
        )
        .await;
        assert!(e.contains("inside the daemon's data directory"), "{e}");
        assert!(!root.join(".claude").exists());
    }
}

#[tokio::test]
#[cfg(unix)]
async fn scaffold_agent_config_refuses_a_link_at_dot_claude() {
    let f = fixture(false);
    let r = &f.router;
    // Live, to a dir outside the allowlist.
    let a = f.allowed.join("a");
    symlink(&f.outside, &a.join(".claude"));
    let e = err(r, "scaffold_agent_config", agent_args(&a, "augment")).await;
    assert!(e.contains(SYMLINK_REFUSAL), "{e}");
    assert_eq!(std::fs::read_dir(&f.outside).unwrap().count(), 0);
    // Live, to a dir inside the allowlist: still not followed.
    let b = f.allowed.join("b");
    std::fs::create_dir_all(f.allowed.join("elsewhere")).unwrap();
    symlink(&f.allowed.join("elsewhere"), &b.join(".claude"));
    let e = err(r, "scaffold_agent_config", agent_args(&b, "replace")).await;
    assert!(e.contains(SYMLINK_REFUSAL), "{e}");
    assert_eq!(
        std::fs::read_dir(f.allowed.join("elsewhere"))
            .unwrap()
            .count(),
        0
    );
    // Dangling: would otherwise create its target.
    let c = f.allowed.join("c");
    symlink(&f.outside.join("made"), &c.join(".claude"));
    let e = err(r, "scaffold_agent_config", agent_args(&c, "augment")).await;
    assert!(e.contains(SYMLINK_REFUSAL), "{e}");
    assert!(!f.outside.join("made").exists());
}

/// A link planted at every file the tree writes, and at every directory
/// below `.claude`: nothing is written through any of them, dangling or live.
#[tokio::test]
#[cfg(unix)]
async fn scaffold_agent_config_never_writes_through_a_planted_link() {
    let f = fixture(false);
    let r = &f.router;
    let files = starter_files();

    // Dangling link at every file: replace refuses each one, augment skips it
    // (something is there), and no target is ever created.
    let root = f.allowed.join("dangling");
    for (i, rel) in files.iter().enumerate() {
        symlink(
            &f.outside.join(format!("d{i}")),
            &root.join(".claude").join(rel),
        );
    }
    let rep = ok(r, "scaffold_agent_config", agent_args(&root, "replace")).await;
    assert_eq!(rep["ok"], false);
    assert_eq!(rep["files_written"], 0);
    let errors = rep["errors"].as_array().unwrap();
    assert_eq!(errors.len(), files.len(), "{rep}");
    for e in errors {
        assert!(
            e["reason"].as_str().unwrap().contains(SYMLINK_REFUSAL),
            "{e}"
        );
    }
    let aug = ok(r, "scaffold_agent_config", agent_args(&root, "augment")).await;
    assert_eq!(aug["files_written"], 0);
    assert_eq!(aug["skipped"].as_array().unwrap().len(), files.len());
    assert_eq!(std::fs::read_dir(&f.outside).unwrap().count(), 0);

    // Live link at every file, to a file outside the allowlist.
    let root = f.allowed.join("live");
    for (i, rel) in files.iter().enumerate() {
        let target = f.outside.join(format!("l{i}"));
        std::fs::write(&target, "ORIGINAL").unwrap();
        symlink(&target, &root.join(".claude").join(rel));
    }
    let rep = ok(r, "scaffold_agent_config", agent_args(&root, "replace")).await;
    assert_eq!(rep["files_written"], 0);
    for i in 0..files.len() {
        assert_eq!(
            std::fs::read_to_string(f.outside.join(format!("l{i}"))).unwrap(),
            "ORIGINAL"
        );
    }

    // A link at a directory below `.claude` (live and dangling): the files
    // under it are refused, the rest still land.
    let root = f.allowed.join("dirs");
    std::fs::create_dir_all(f.outside.join("agents-out")).unwrap();
    symlink(&f.outside.join("agents-out"), &root.join(".claude/agents"));
    symlink(&f.outside.join("cmds-out"), &root.join(".claude/commands"));
    let rep = ok(r, "scaffold_agent_config", agent_args(&root, "replace")).await;
    assert_eq!(rep["ok"], false);
    let refused: Vec<&str> = rep["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert!(!refused.is_empty());
    assert!(
        refused
            .iter()
            .all(|p| p.starts_with("agents/") || p.starts_with("commands/")),
        "{refused:?}"
    );
    assert!(root
        .join(".claude/skills/release-planner/SKILL.md")
        .is_file());
    assert_eq!(
        std::fs::read_dir(f.outside.join("agents-out"))
            .unwrap()
            .count(),
        0
    );
    assert!(!f.outside.join("cmds-out").exists());
}

// ── pkg_preview_manifest ────────────────────────────────────────────────────

#[tokio::test]
async fn pkg_preview_manifest_reads_an_allowlisted_manifest() {
    let f = fixture(false);
    let r = &f.router;
    let pkg = f.allowed.join("pkg-a");
    write(&pkg.join("manifest.json"), &manifest("com.test.a"));
    let a = ok(r, "pkg_preview_manifest", json!({ "installPath": s(&pkg) })).await;
    let b = ok(
        r,
        "pkg_preview_manifest",
        json!({ "install_path": s(&pkg) }),
    )
    .await;
    assert_eq!(a, b);
    assert_eq!(a, pkg_workspace::preview_manifest(&pkg).unwrap());
    assert_eq!(a["id"], "com.test.a");

    // The desktop's own errors for what is inside the boundary.
    let empty = f.allowed.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let e = err(
        r,
        "pkg_preview_manifest",
        json!({ "installPath": s(&empty) }),
    )
    .await;
    assert_eq!(
        e,
        format!(
            "pkg_preview_manifest: {}",
            pkg_workspace::preview_manifest(&empty).unwrap_err()
        )
    );
    assert!(e.contains("manifest.json"), "{e}");
    let bad = f.allowed.join("bad");
    write(
        &bad.join("manifest.json"),
        r#"{"id":"nodots","name":"x","version":"1","ikenga_api":"1"}"#,
    );
    let e = err(r, "pkg_preview_manifest", json!({ "installPath": s(&bad) })).await;
    assert!(e.contains("reverse-DNS"), "{e}");
    let e = err(r, "pkg_preview_manifest", json!({})).await;
    assert!(e.contains("`installPath` is required"), "{e}");
}

#[tokio::test]
#[cfg(unix)]
async fn pkg_preview_manifest_stays_in_the_allowlist() {
    let f = fixture(false);
    let r = &f.router;
    let out_pkg = f.outside.join("pkg");
    write(&out_pkg.join("manifest.json"), &manifest("com.test.secret"));
    let e = err(
        r,
        "pkg_preview_manifest",
        json!({ "installPath": s(&out_pkg) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    let e = err(
        r,
        "pkg_preview_manifest",
        json!({ "installPath": format!("{}/../outside/pkg", f.allowed.display()) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    // A linked pkg dir, and a linked manifest.json, pointing outside.
    symlink(&out_pkg, &f.allowed.join("linked"));
    let e = err(
        r,
        "pkg_preview_manifest",
        json!({ "installPath": s(&f.allowed.join("linked")) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    let half = f.allowed.join("half");
    symlink(&out_pkg.join("manifest.json"), &half.join("manifest.json"));
    let e = err(
        r,
        "pkg_preview_manifest",
        json!({ "installPath": s(&half) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    assert!(!e.contains("com.test.secret"), "{e}");

    // An allowlist that covers the data dir still cannot read in it.
    let f = fixture(true);
    let inside = f.data.join("pkg");
    write(&inside.join("manifest.json"), &manifest("com.test.data"));
    let e = err(
        &f.router,
        "pkg_preview_manifest",
        json!({ "installPath": s(&inside) }),
    )
    .await;
    assert!(e.contains("inside the daemon's data directory"), "{e}");
    let linked = f.allowed.join("to-data");
    symlink(&inside.join("manifest.json"), &linked.join("manifest.json"));
    let e = err(
        &f.router,
        "pkg_preview_manifest",
        json!({ "installPath": s(&linked) }),
    )
    .await;
    assert!(e.contains("inside the daemon's data directory"), "{e}");
}

// ── pkg_discover_workspace ──────────────────────────────────────────────────

fn sorted(v: Value) -> Vec<Value> {
    let mut a = v.as_array().unwrap().clone();
    a.sort_by_key(|e| e["install_path"].as_str().unwrap().to_string());
    a
}

#[tokio::test]
#[cfg(unix)]
async fn pkg_discover_workspace_scans_in_the_desktop_shape() {
    let f = fixture(false);
    let r = &f.router;
    let ws = f.allowed.join("ws");
    write(&ws.join("a/manifest.json"), &manifest("com.test.a"));
    write(
        &ws.join(INSTALLED_ID).join("manifest.json"),
        &manifest(INSTALLED_ID),
    );
    write(&ws.join("broken/manifest.json"), "{ not json");
    std::fs::create_dir_all(ws.join("no-manifest")).unwrap();
    write(&ws.join("loose-file"), "x");
    // Links out of the allowlist read as absent.
    write(
        &f.outside.join("c/manifest.json"),
        &manifest("com.test.outside"),
    );
    symlink(&f.outside.join("c"), &ws.join("linked-dir"));
    symlink(
        &f.outside.join("c/manifest.json"),
        &ws.join("half/manifest.json"),
    );

    let a = ok(
        r,
        "pkg_discover_workspace",
        json!({ "workspaceDir": s(&ws) }),
    )
    .await;
    let b = ok(
        r,
        "pkg_discover_workspace",
        json!({ "workspace_dir": s(&ws) }),
    )
    .await;
    assert_eq!(sorted(a.clone()), sorted(b));
    let got = sorted(a);
    let ids: Vec<&str> = got.iter().map(|e| e["id"].as_str().unwrap()).collect();
    // Sorted by install_path: a, broken, com.test.installed.
    assert_eq!(ids, vec!["com.test.a", "", INSTALLED_ID], "{got:?}");
    assert!(!format!("{got:?}").contains("com.test.outside"));

    // `installed` is the daemon's index — the set `pkg_kernel_status` reports.
    assert_eq!(got[0]["installed"], false);
    assert_eq!(got[2]["installed"], true);
    assert_eq!(got[1]["valid"], false);
    assert!(got[1]["error"].as_str().unwrap().contains("parse manifest"));

    // Shape parity: the desktop scan of the same dir (links removed, since it
    // would follow them) with the same installed set, key for key.
    std::fs::remove_file(ws.join("linked-dir")).unwrap();
    std::fs::remove_dir_all(ws.join("half")).unwrap();
    let installed = [INSTALLED_ID.to_string()].into_iter().collect();
    let desk = sorted(wire(pkg_workspace::discover(
        &ws,
        &installed,
        pkg_workspace::Reach::Follow,
    )));
    assert_eq!(got, desk);
    assert_eq!(
        keys(&got[0]),
        vec![
            "compatible",
            "error",
            "id",
            "install_path",
            "installed",
            "name",
            "valid",
            "version"
        ]
    );
}

#[tokio::test]
async fn pkg_discover_workspace_stays_in_the_allowlist() {
    let f = fixture(false);
    let r = &f.router;
    write(&f.outside.join("p/manifest.json"), &manifest("com.test.p"));
    let e = err(
        r,
        "pkg_discover_workspace",
        json!({ "workspaceDir": s(&f.outside) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    let e = err(
        r,
        "pkg_discover_workspace",
        json!({ "workspaceDir": format!("{}/../outside", f.allowed.display()) }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    // Missing inside the allowlist, or empty: nothing to read, as on the desktop.
    assert_eq!(
        ok(
            r,
            "pkg_discover_workspace",
            json!({ "workspaceDir": s(&f.allowed.join("gone/deeper")) })
        )
        .await,
        json!([])
    );
    assert_eq!(
        ok(r, "pkg_discover_workspace", json!({ "workspaceDir": "" })).await,
        json!([])
    );

    let f = fixture(true);
    write(&f.data.join("p/manifest.json"), &manifest("com.test.p"));
    let e = err(
        &f.router,
        "pkg_discover_workspace",
        json!({ "workspaceDir": s(&f.data) }),
    )
    .await;
    assert!(e.contains("inside the daemon's data directory"), "{e}");
    // A scan of a parent of the data dir: the data dir itself (here holding a
    // manifest.json of its own) reads as absent.
    write(&f.data.join("manifest.json"), &manifest("com.test.data"));
    write(&f.root.join("sib/manifest.json"), &manifest("com.test.sib"));
    let got = ok(
        &f.router,
        "pkg_discover_workspace",
        json!({ "workspaceDir": s(&f.root) }),
    )
    .await;
    let ids: Vec<&str> = got
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"com.test.sib"), "{ids:?}");
    assert!(!ids.contains(&"com.test.data"), "{ids:?}");
}

// ── pkg_scaffold ────────────────────────────────────────────────────────────

fn scaffold_params(kind: &str, slug: &str, target: Option<&Path>, scope: &str) -> Value {
    let mut p = json!({
        "kind": kind,
        "name": "Release Notes",
        "slug": slug,
        "description": "Generates release notes from git commits and PRs.",
        "scope": scope,
    });
    if let Some(t) = target {
        p["targetDir"] = json!(s(t));
    }
    json!({ "params": p })
}

#[tokio::test]
async fn pkg_scaffold_writes_the_desktop_files() {
    let f = fixture(false);
    let r = &f.router;
    let folder = f.allowed.join("new/release-notes");
    let res = ok(
        r,
        "pkg_scaffold",
        scaffold_params("skill", "release-notes", Some(&folder), "personal"),
    )
    .await;
    assert_eq!(
        keys(&res),
        vec![
            "filesWritten",
            "kind",
            "ok",
            "slug",
            "targetFolder",
            "targetPath"
        ]
    );
    assert_eq!(res["targetFolder"], s(&folder));
    assert_eq!(res["targetPath"], s(&folder.join("SKILL.md")));

    // Parity with the desktop body over a twin folder.
    let params: pkg_scaffold::PkgScaffoldParams = serde_json::from_value(
        scaffold_params("skill", "release-notes", None, "personal")["params"].clone(),
    )
    .unwrap();
    let twin = f.root.join("twin/release-notes");
    let files = pkg_scaffold::execute_scaffold(&params, &twin, &twin.join("SKILL.md")).unwrap();
    let desk = wire(pkg_scaffold::result(
        params,
        &folder,
        &folder.join("SKILL.md"),
        files.clone(),
    ));
    assert_eq!(res, desk);
    for f_name in &files {
        assert_eq!(
            std::fs::read(folder.join(f_name)).unwrap(),
            std::fs::read(twin.join(f_name)).unwrap(),
            "{f_name}"
        );
    }

    // A template kind (`app` → the embedded ui-iframe tree).
    let app = f.allowed.join("apps/my-app");
    let res = ok(
        r,
        "pkg_scaffold",
        scaffold_params("app", "my-app", Some(&app), "personal"),
    )
    .await;
    let written = res["filesWritten"].as_array().unwrap();
    assert!(!written.is_empty());
    for w in written {
        assert!(app.join(w.as_str().unwrap()).is_file(), "{w}");
        assert!(!w.as_str().unwrap().starts_with("ui-iframe"), "{w}");
    }
    assert!(
        std::path::Path::new(res["targetPath"].as_str().unwrap()).is_file(),
        "targetPath exists: {res}"
    );
    assert!(!app.join("ui-iframe").exists());

    // Project scope: the project row's root, from the daemon's ikenga.db.
    let proj = f.allowed.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let pool = f.db.ensure_pool().await.unwrap();
    create_project(
        &pool,
        CreateArgs {
            id: "proj".into(),
            display_name: "P".into(),
            root_path: Some(s(&proj)),
            icon: None,
            color: None,
            description: None,
        },
    )
    .await
    .unwrap();
    let res = ok(
        r,
        "pkg_scaffold",
        scaffold_params("command", "ship-it", None, "project:proj"),
    )
    .await;
    assert_eq!(
        res["targetPath"],
        s(&proj.join(".claude/commands/ship-it.md"))
    );
    assert!(proj.join(".claude/commands/ship-it.md").is_file());

    // Desktop validation and conflict errors come through unchanged.
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("skill", "Bad Slug", Some(&f.allowed.join("x")), "personal"),
    )
    .await;
    assert!(e.contains("invalid slug"), "{e}");
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("skill", "release-notes", Some(&folder), "personal"),
    )
    .await;
    assert!(e.contains("target file already exists"), "{e}");
    let e = err(r, "pkg_scaffold", json!({})).await;
    assert!(e.contains("`params` is required"), "{e}");
}

#[tokio::test]
async fn pkg_scaffold_destination_stays_in_the_allowlist() {
    let f = fixture(false);
    let r = &f.router;
    for (target, want) in [
        (f.outside.join("x"), "outside allowlist"),
        (f.allowed.join("../outside/x"), "`..`"),
        (PathBuf::from("relative/x"), "not absolute"),
    ] {
        let e = err(
            r,
            "pkg_scaffold",
            scaffold_params("skill", "x-skill", Some(&target), "personal"),
        )
        .await;
        assert!(e.contains(want), "{target:?}: {e}");
    }
    assert_eq!(std::fs::read_dir(&f.outside).unwrap().count(), 0);
    // The workspace default resolves under the router home, which this
    // allowlist does not cover.
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("agent", "helper", None, "personal"),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    assert!(!f.home.join(".claude").exists());
    // The desktop's `project` fallback is the process cwd: refused.
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("project", "here", None, "workspace"),
    )
    .await;
    assert!(e.contains("not absolute"), "{e}");

    // Allowlist over the whole root: the router home is reachable, the data
    // dir still is not.
    let f = fixture(true);
    let res = ok(
        &f.router,
        "pkg_scaffold",
        scaffold_params("agent", "helper", None, "personal"),
    )
    .await;
    assert_eq!(
        res["targetPath"],
        s(&f.home.join(".claude/agents/helper.md"))
    );
    assert!(f.home.join(".claude/agents/helper.md").is_file());
    for target in [f.data.join("pkgs/x"), f.data.clone()] {
        let e = err(
            &f.router,
            "pkg_scaffold",
            scaffold_params("skill", "x-skill", Some(&target), "personal"),
        )
        .await;
        assert!(e.contains("inside the daemon's data directory"), "{e}");
    }
    assert!(!f.data.join("pkgs").exists());
}

#[tokio::test]
#[cfg(unix)]
async fn pkg_scaffold_never_writes_through_a_planted_link() {
    let f = fixture(false);
    let r = &f.router;
    // The folder itself a link, live (out of the allowlist) or dangling.
    symlink(&f.outside, &f.allowed.join("live"));
    symlink(&f.outside.join("made"), &f.allowed.join("dangling"));
    for folder in [
        f.allowed.join("live/x"),
        f.allowed.join("live"),
        f.allowed.join("dangling/x"),
        f.allowed.join("dangling"),
    ] {
        let e = err(
            r,
            "pkg_scaffold",
            scaffold_params("skill", "x-skill", Some(&folder), "personal"),
        )
        .await;
        assert!(
            e.contains("outside allowlist")
                || e.contains("canonicalize")
                || e.contains(SYMLINK_REFUSAL),
            "{folder:?}: {e}"
        );
    }
    assert_eq!(std::fs::read_dir(&f.outside).unwrap().count(), 0);
    assert!(!f.outside.join("made").exists());

    // A link inside the allowlist is resolved once, by the guard, and the
    // write then runs from the canonical form (reported as such).
    std::fs::create_dir_all(f.allowed.join("real")).unwrap();
    symlink(&f.allowed.join("real"), &f.allowed.join("alias"));
    let res = ok(
        r,
        "pkg_scaffold",
        scaffold_params(
            "skill",
            "via-alias",
            Some(&f.allowed.join("alias/s")),
            "personal",
        ),
    )
    .await;
    assert_eq!(
        res["targetFolder"],
        s(&f.allowed.join("real/s")),
        "resolved, then written canonical"
    );

    // A dangling link at the primary target: "already exists", never written.
    let agents = f.allowed.join("agents");
    symlink(&f.outside.join("agent.md"), &agents.join("helper.md"));
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("agent", "helper", Some(&agents), "personal"),
    )
    .await;
    assert!(e.contains("target file already exists"), "{e}");
    assert!(!f.outside.join("agent.md").exists());

    // A link at a secondary file (dangling and live).
    let skill = f.allowed.join("skill");
    symlink(
        &f.outside.join("manifest.json"),
        &skill.join("manifest.json"),
    );
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("skill", "skill", Some(&skill), "personal"),
    )
    .await;
    assert!(
        e.contains("write manifest.json") && e.contains(SYMLINK_REFUSAL),
        "{e}"
    );
    assert!(!f.outside.join("manifest.json").exists());
    let skill2 = f.allowed.join("skill2");
    std::fs::write(f.outside.join("readme"), "ORIGINAL").unwrap();
    symlink(&f.outside.join("readme"), &skill2.join("README.md"));
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("skill", "skill2", Some(&skill2), "personal"),
    )
    .await;
    assert!(e.contains(SYMLINK_REFUSAL), "{e}");
    assert_eq!(
        std::fs::read_to_string(f.outside.join("readme")).unwrap(),
        "ORIGINAL"
    );

    // A link at a template subdirectory (the ui-iframe tree's `src/`, which
    // lands directly under the folder).
    let app = f.allowed.join("app");
    std::fs::create_dir_all(f.outside.join("src-out")).unwrap();
    symlink(&f.outside.join("src-out"), &app.join("src"));
    let e = err(
        r,
        "pkg_scaffold",
        scaffold_params("app", "app", Some(&app), "personal"),
    )
    .await;
    assert!(e.contains(SYMLINK_REFUSAL), "{e}");
    assert_eq!(
        std::fs::read_dir(f.outside.join("src-out"))
            .unwrap()
            .count(),
        0
    );
}

// ── desktop regression: Follow is unchanged ─────────────────────────────────

/// The desktop bodies still follow links exactly as before the move (their
/// caller is the user's own renderer); only the daemon confines.
#[test]
#[cfg(unix)]
fn desktop_bodies_still_follow_links() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (proj, elsewhere) = (root.join("proj"), root.join("elsewhere"));
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, &proj.join(".claude"));
    let res = agent_scaffold::scaffold(agent_scaffold::ScaffoldRequest {
        provider: "claude-code".into(),
        root_path: s(&proj),
        profile: "starter".into(),
        mode: None,
    })
    .unwrap();
    assert!(res.ok && res.files_written > 0);
    assert!(elsewhere.join("agents/release-coordinator.md").is_file());

    let folder = root.join("pkg");
    std::fs::create_dir_all(&folder).unwrap();
    symlink(&root.join("readme-target"), &folder.join("README.md"));
    let params: pkg_scaffold::PkgScaffoldParams =
        serde_json::from_value(scaffold_params("skill", "s", None, "personal")["params"].clone())
            .unwrap();
    pkg_scaffold::execute_scaffold(&params, &folder, &folder.join("SKILL.md")).unwrap();
    assert!(
        root.join("readme-target").is_file(),
        "fs::write followed the dangling link"
    );
    assert!(present(&folder.join("README.md")));
}
