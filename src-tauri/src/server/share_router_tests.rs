//! End-to-end router test of the T1 child's share pre-hook (G-ACCESS
//! §4.5.4, WP-76 deferred → WP-78a): a principal child's router with a
//! fixture fs allowlist, driven over HTTP the way the broker relays a
//! member's request — the per-child token, `X-Ikenga-Caps` and the
//! `X-Ikenga-Share-*` set. Covers one allowed arm served on the narrowed
//! `PathGuard`, one refusal for a path outside the share that the router's
//! own guard admits, and the Reviewer cost strip.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::rpc_shell::PathGuard;
use super::ServerConfig;
use crate::access::DaemonAccess;
use crate::engines::EngineRegistry;
use crate::executor::{ExecutorTier, PrincipalId};
use crate::pty::PtyManager;

const TOKEN: &str = "per-child-token-for-tests";
const SESSION: &str = "22222222-2222-4222-8222-222222222222";
const PROJECT: &str = "shared-proj";

struct Child {
    _tmp: tempfile::TempDir,
    router: Router,
    project: PathBuf,
    outside: PathBuf,
    pkgs: PathBuf,
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A transcript under `cwd` whose `result` envelope carries usage and cost.
fn transcript(cwd: &str) -> String {
    [
        json!({
            "type": "user", "sessionId": SESSION, "cwd": cwd,
            "timestamp": "2026-10-01T10:00:00Z", "uuid": "u1",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }] },
        }),
        json!({
            "type": "result", "sessionId": SESSION, "cwd": cwd,
            "usage": { "input_tokens": 10, "output_tokens": 5 },
            "total_cost_usd": 0.42, "stop_reason": "end_turn", "duration_ms": 900,
        }),
    ]
    .iter()
    .map(|v| format!("{v}\n"))
    .collect()
}

async fn child() -> Child {
    child_with_pkg(None).await
}

/// `pkg_id`: the daemon is given a `--pkgs-dir` (`Child::pkgs`) already holding
/// that pkg (the index is built once, at router construction).
async fn child_with_pkg(pkg_id: Option<&str>) -> Child {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let home = root.join("home");
    let work = root.join("work");
    let project = work.join("proj");
    let outside = work.join("not-shared.txt");
    let pkgs = root.join("pkgs");
    if let Some(id) = pkg_id {
        write(
            &pkgs.join(id).join("manifest.json"),
            &format!(r#"{{"id":"{id}","name":"N {id}","version":"0.1.0","ikenga_api":"1"}}"#),
        );
    }
    std::fs::create_dir_all(&data).unwrap();
    write(&project.join("docs/brief.md"), "the brief");
    write(&outside, "the Owner's other file");
    write(
        &home.join(format!(".claude/projects/-proj/{SESSION}.jsonl")),
        &transcript(&project.to_string_lossy()),
    );

    let roots_file = root.join("fs_roots.json");
    std::fs::write(
        &roots_file,
        json!({ "roots": [work.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();

    let db = Arc::new(crate::db::PaDb::new(data.join("ikenga.db")));
    let pool = db.ensure_pool().await.unwrap();
    sqlx::query(
        "INSERT INTO projects (id, display_name, root_path, position, is_default, created_at) \
         VALUES (?, 'Shared', ?, 1, 0, 0)",
    )
    .bind(PROJECT)
    .bind(project.to_string_lossy().into_owned())
    .execute(&pool)
    .await
    .unwrap();

    let config = ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: pkg_id.map(|_| pkgs.clone()),
        data_dir: Some(data),
        auth_token: Some(TOKEN.into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    };
    let router = super::build_router(
        config,
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        Some(db),
        None,
        Some(home),
        PathGuard::roots(Arc::new(roots)),
        None,
        DaemonAccess::principal_child(Default::default()),
        None,
        super::UpdateSource::Default,
    );
    Child {
        _tmp: tmp,
        router,
        project,
        outside,
        pkgs,
    }
}

/// A member's request as the broker relays it into the Owner's child; with
/// `role: None`, the Owner's own (own-workspace) request.
async fn call(c: &Child, role: Option<&str>, cmd: &str, args: Value) -> Value {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string());
    match role {
        Some(role) => {
            req = req
                .header("x-ikenga-caps", "files,sessions")
                .header("x-ikenga-share-project", PROJECT)
                .header(
                    "x-ikenga-share-principal",
                    PrincipalId::new_v7().to_string(),
                )
                .header("x-ikenga-share-device", "-")
                .header("x-ikenga-share-role", role)
        }
        None => req = req.header("x-ikenga-caps", "files,sessions"),
    }
    let res = c
        .router
        .clone()
        .oneshot(
            req.body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn has_cost_key(v: &Value) -> bool {
    match v {
        Value::Object(map) => map
            .iter()
            .any(|(k, child)| crate::access::share::is_cost_key(k) || has_cost_key(child)),
        Value::Array(items) => items.iter().any(has_cost_key),
        _ => false,
    }
}

/// `fs_read` answers the desktop's `{ bytes, mime }` shape, not a bare string.
fn read_text(resp: &Value) -> String {
    let bytes: Vec<u8> = resp["data"]["bytes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| b.as_u64().map(|b| b as u8))
                .collect()
        })
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn the_share_prehook_end_to_end() {
    let c = child().await;
    let inside = c.project.join("docs/brief.md");
    let s = |p: &Path| p.to_string_lossy().into_owned();

    // The Owner's own request: the router's guard admits both files.
    let own = call(&c, None, "fs_read", json!({ "path": s(&c.outside) })).await;
    assert_eq!(read_text(&own), "the Owner's other file", "{own}");

    // 1. An allowed arm, served on the narrowed guard.
    let read = call(
        &c,
        Some("operator"),
        "fs_read",
        json!({ "path": s(&inside) }),
    )
    .await;
    assert_eq!(read["ok"], true, "{read}");
    assert_eq!(read_text(&read), "the brief");

    // 2. A path the router's guard admits but the share doesn't: refused.
    let refused = call(
        &c,
        Some("operator"),
        "fs_read",
        json!({ "path": s(&c.outside) }),
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused.get("data").map_or(true, Value::is_null),
        "{refused}"
    );
    let climb = format!("{}/../not-shared.txt", s(&c.project));
    let refused = call(&c, Some("operator"), "fs_read", json!({ "path": climb })).await;
    assert_eq!(refused["ok"], false, "`..` never climbs out: {refused}");

    // 3. The Reviewer cost strip: the same transcript, with and without.
    let args = json!({ "sessionId": SESSION });
    let owner = call(&c, None, "claude_read_jsonl", args.clone()).await;
    assert_eq!(owner["ok"], true, "{owner}");
    assert!(has_cost_key(&owner["data"]), "the Owner sees cost: {owner}");
    let operator = call(&c, Some("operator"), "claude_read_jsonl", args.clone()).await;
    assert_eq!(operator["ok"], true, "{operator}");
    assert!(has_cost_key(&operator["data"]), "an Operator keeps cost");
    let reviewer = call(&c, Some("reviewer"), "claude_read_jsonl", args).await;
    assert_eq!(reviewer["ok"], true, "{reviewer}");
    assert!(!has_cost_key(&reviewer["data"]), "{reviewer}");
    assert_eq!(
        reviewer["data"].as_array().map(Vec::len),
        operator["data"].as_array().map(Vec::len),
        "only cost fields go, never the events"
    );
}

/// A member previews a page in the shared project. The mount is confined to
/// the share: the project root (and below) works, anything above it, though
/// the Owner's own allowlist admits it, is refused, and so is a page outside
/// the share.
#[tokio::test]
async fn a_share_member_cannot_widen_a_viewer_root_past_the_shared_project() {
    let c = child().await;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let page = c.project.join("docs/brief.md");
    let work = c.project.parent().unwrap().to_path_buf();

    let ok = call(
        &c,
        Some("operator"),
        "viewer_serve",
        json!({ "rootDir": s(&c.project), "filePath": s(&page) }),
    )
    .await;
    assert_eq!(ok["ok"], true, "{ok}");
    let below = call(
        &c,
        Some("operator"),
        "viewer_serve",
        json!({ "rootDir": s(&c.project.join("docs")), "filePath": s(&page) }),
    )
    .await;
    assert_eq!(below["ok"], true, "{below}");

    // Above the project: the Owner may mount `work`, a member may not.
    let owner = call(
        &c,
        None,
        "viewer_serve",
        json!({ "rootDir": s(&work), "filePath": s(&page) }),
    )
    .await;
    assert_eq!(
        owner["ok"], false,
        "{owner}: the project bound applies to the Owner too"
    );
    let widened = call(
        &c,
        Some("operator"),
        "viewer_serve",
        json!({ "rootDir": s(&work), "filePath": s(&page) }),
    )
    .await;
    assert_eq!(widened["ok"], false, "{widened}");

    // A page outside the share, with its own directory as the root.
    let outside = call(
        &c,
        Some("operator"),
        "viewer_serve",
        json!({ "rootDir": s(&work), "filePath": s(&c.outside) }),
    )
    .await;
    assert_eq!(outside["ok"], false, "{outside}");
}

/// The same relay with an explicit caps header.
async fn call_with_caps(
    c: &Child,
    role: Option<&str>,
    caps: &str,
    cmd: &str,
    args: Value,
) -> Value {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header("x-ikenga-caps", caps);
    if let Some(role) = role {
        req = req
            .header("x-ikenga-share-project", PROJECT)
            .header(
                "x-ikenga-share-principal",
                PrincipalId::new_v7().to_string(),
            )
            .header("x-ikenga-share-device", "-")
            .header("x-ikenga-share-role", role);
    }
    let res = c
        .router
        .clone()
        .oneshot(
            req.body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// The broker's own `internal` call into this child (§4.5.3): the per-child
/// token and the internal-call marker, no caps and no share headers.
async fn broker_call(c: &Child, cmd: &str, args: Value) -> Value {
    let req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header(crate::access::INTERNAL_CALL_HEADER, "1")
        .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
        .unwrap();
    let res = c.router.clone().oneshot(req).await.unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// WP-P10: the Chi write arms are owner-class. A Chi run executes as the
/// child's principal (the Owner) with the Owner's engine logins, so a share
/// member — even an operator holding `dispatch` — may not start, resume or
/// cancel one in the Owner's child; the refusal comes before the arm, so no
/// row is ever written. The Owner's own request needs `dispatch`.
#[tokio::test]
async fn chi_write_arms_are_owner_only_in_a_principal_child() {
    let c = child().await;
    let run = json!({ "opts": { "engineId": "cursor-agent", "prompt": "hi", "cwd": "/tmp" } });
    let all = "files,sessions,dispatch,approve,install,settings,secrets";

    for (cmd, args) in [
        ("chi_run", run.clone()),
        ("chi_resume", json!({ "runId": "r1", "prompt": "x" })),
        ("chi_cancel", json!({ "runId": "r1" })),
    ] {
        let shared = call_with_caps(&c, Some("operator"), all, cmd, args.clone()).await;
        assert_eq!(shared["ok"], false, "{cmd}: {shared}");
        assert!(
            shared["error"].as_str().unwrap().starts_with("forbidden"),
            "{cmd}: {shared}"
        );
        let no_dispatch = call_with_caps(&c, None, "files,sessions", cmd, args).await;
        assert_eq!(
            no_dispatch["error"], "forbidden: missing=dispatch",
            "{cmd}: {no_dispatch}"
        );
    }
    let listed = call_with_caps(
        &c,
        None,
        all,
        "chi_list",
        json!({ "engineId": "cursor-agent" }),
    )
    .await;
    assert_eq!(
        listed["data"],
        json!([]),
        "no refused call wrote a row: {listed}"
    );

    // The Owner's own request with `dispatch` reaches the arm (cursor-agent
    // then fails to start, leaving a failed row of the Owner's).
    let own = call_with_caps(&c, None, all, "chi_run", run).await;
    assert_eq!(own["ok"], false, "{own}");
    assert!(
        own["error"]
            .as_str()
            .unwrap()
            .contains("cursor-agent runtime not implemented"),
        "{own}"
    );
    let listed = call_with_caps(
        &c,
        None,
        all,
        "chi_list",
        json!({ "engineId": "cursor-agent" }),
    )
    .await;
    assert_eq!(listed["data"][0]["status"], "failed", "{listed}");
}

/// Gap audit 2026-10-06 rank 2: inviting to the built-in Default project
/// failed with a bare "no such project". Default has no folder, and §4.5.4
/// confines a share to the project root, so it can't be shared — but the
/// broker's `share_project_info` now says so instead of claiming it does
/// not exist. An unknown id stays `not_found`; a rooted project resolves.
#[tokio::test]
async fn share_project_info_says_why_a_folderless_project_cannot_be_shared() {
    let c = child().await;
    let shared = broker_call(&c, "share_project_info", json!({ "projectId": PROJECT })).await;
    assert_eq!(shared["ok"], true, "{shared}");
    assert_eq!(shared["data"]["name"], "Shared");

    let default = broker_call(&c, "share_project_info", json!({ "projectId": "default" })).await;
    assert_eq!(
        default["error"],
        format!(
            "invalid_request: {}",
            crate::access::share::NO_FOLDER_TO_SHARE
        ),
        "{default}"
    );

    let unknown = broker_call(&c, "share_project_info", json!({ "projectId": "nope" })).await;
    assert_eq!(unknown["error"], "not_found: no such project", "{unknown}");

    // A member's relayed request still gets a uniform `not_found` for a
    // folderless project (the share pre-hook's own resolution).
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header("x-ikenga-caps", "files,sessions")
        .header("x-ikenga-share-project", "default")
        .header(
            "x-ikenga-share-principal",
            PrincipalId::new_v7().to_string(),
        )
        .header("x-ikenga-share-device", "-")
        .header("x-ikenga-share-role", "operator");
    req = req.header("x-ikenga-share-policy", "owner-approval");
    let res = c
        .router
        .clone()
        .oneshot(
            req.body(Body::from(
                json!({ "cmd": "fs_list", "args": { "path": "/" } }).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["error"], "not_found: no such project", "{body}");
}

#[tokio::test]
async fn git_status_under_share_confinement() {
    let c = child().await;
    let s = |p: &Path| p.to_string_lossy().into_owned();

    // 1. Inside the shared project: git_status is allowed for a share member (viewer).
    // (c.project is not a git repo, so it returns ok: true with data: null).
    let inside = call(
        &c,
        Some("viewer"),
        "git_status",
        json!({ "root": s(&c.project) }),
    )
    .await;
    assert_eq!(inside["ok"], true, "{inside}");
    assert_eq!(inside["data"], Value::Null);

    // 1b. Once the shared project is a repository, the member reads its state.
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&c.project)
        .output()
        .unwrap()
        .status
        .success());
    let repo = call(
        &c,
        Some("viewer"),
        "git_status",
        json!({ "root": s(&c.project) }),
    )
    .await;
    assert_eq!(repo["ok"], true, "{repo}");
    assert!(repo["data"]["branch"].is_string(), "{repo}");

    // 2. A path outside the shared project: refused for a share member.
    let outside_dir = c.outside.parent().unwrap();
    let refused = call(
        &c,
        Some("viewer"),
        "git_status",
        json!({ "root": s(outside_dir) }),
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");

    // 3. A request specifying a different projectId: refused.
    let bad_proj = call(
        &c,
        Some("viewer"),
        "git_status",
        json!({ "root": s(&c.project), "projectId": "other-project" }),
    )
    .await;
    assert_eq!(bad_proj["ok"], false, "{bad_proj}");
    assert!(
        bad_proj["error"]
            .as_str()
            .unwrap()
            .contains("must name the shared project"),
        "{bad_proj}"
    );

    // 4. A principal without the Files capability: refused (the pre-hook, not
    // the arm), whether it holds other capabilities or none.
    for caps in ["sessions", ""] {
        let no_files = call_with_caps(
            &c,
            Some("viewer"),
            caps,
            "git_status",
            json!({ "root": s(&c.project) }),
        )
        .await;
        assert_eq!(no_files["ok"], false, "caps `{caps}`: {no_files}");
    }
}

/// A pkg view in a share reads its own manifest to check declared capabilities
/// (`pkg-iframe-host` `checkPkgManifest`); that was owner-only, which broke every
/// pkg view for an Operator in a shared project. An indexed pkg answers by exact
/// path even though it lives outside the share; any other path stays confined to
/// the shared project.
#[tokio::test]
async fn a_share_member_reads_an_indexed_pkgs_manifest_and_nothing_outside_the_share() {
    let c = child_with_pkg(Some("com.test.outbound")).await;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let indexed = c.pkgs.join("com.test.outbound");

    // 1. The indexed pkg (outside the shared project) previews for a viewer.
    let res = call(
        &c,
        Some("viewer"),
        "pkg_preview_manifest",
        json!({ "installPath": s(&indexed) }),
    )
    .await;
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["data"]["id"], "com.test.outbound");

    // 2. A manifest the index does not name, outside the share: refused.
    let other = c.pkgs.join("not-indexed");
    write(
        &other.join("manifest.json"),
        r#"{"id":"com.test.other","name":"N","version":"0.1.0","ikenga_api":"1"}"#,
    );
    let refused = call(
        &c,
        Some("viewer"),
        "pkg_preview_manifest",
        json!({ "installPath": s(&other) }),
    )
    .await;
    assert_eq!(refused["ok"], false, "{refused}");

    // 3. A manifest inside the shared project reads through the narrowed guard.
    let inside = c.project.join("my-pkg");
    write(
        &inside.join("manifest.json"),
        r#"{"id":"com.test.mine","name":"N","version":"0.1.0","ikenga_api":"1"}"#,
    );
    let ok = call(
        &c,
        Some("viewer"),
        "pkg_preview_manifest",
        json!({ "installPath": s(&inside) }),
    )
    .await;
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(ok["data"]["id"], "com.test.mine");
}

/// A member's `GET /api/fs/download`, as the broker relays it into the Owner's
/// child: per-child token, caps header and (for a member) the share set.
async fn download(c: &Child, role: Option<&str>, caps: &str, path: &str) -> (u16, Vec<u8>, String) {
    let mut req = Request::builder()
        .method("GET")
        .uri(format!("/api/fs/download?path={path}"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header("x-ikenga-caps", caps);
    if let Some(role) = role {
        req = req
            .header("x-ikenga-share-project", PROJECT)
            .header(
                "x-ikenga-share-principal",
                PrincipalId::new_v7().to_string(),
            )
            .header("x-ikenga-share-device", "-")
            .header("x-ikenga-share-role", role);
    }
    let res = c
        .router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status().as_u16();
    let disposition = res
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, body.to_vec(), disposition)
}

/// WP-04: `GET /api/fs/download` serves on the share-narrowed guard. A member
/// gets the shared project's file and nothing the Owner's allowlist admits
/// beyond it; the Owner's own request still reads the wider allowlist.
#[tokio::test]
async fn a_share_member_download_is_narrowed_to_the_shared_project() {
    let c = child().await;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let inside = s(&c.project.join("docs/brief.md"));

    // 1. Inside the project: exact bytes, as an attachment.
    let (status, body, disposition) = download(&c, Some("operator"), "files,sessions", &inside).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"the brief");
    assert!(disposition.starts_with("attachment"), "{disposition}");

    // 2. A file the router's own guard admits but the share does not: 404.
    let (status, body, _) = download(&c, Some("operator"), "files,sessions", &s(&c.outside)).await;
    assert_eq!(status, 404);
    assert!(!String::from_utf8_lossy(&body).contains("Owner's other file"));

    // 3. A `..` climb out of the project: 404.
    let climb = format!("{}/../not-shared.txt", s(&c.project));
    let (status, _, _) = download(&c, Some("operator"), "files,sessions", &climb).await;
    assert_eq!(status, 404, "`..` never climbs out");

    // 4. No Files capability: refused before any read.
    let (status, _, _) = download(&c, Some("operator"), "sessions", &inside).await;
    assert!(status == 403 || status == 401, "no files cap: {status}");

    // 5. The Owner's own request (no share headers) still reads the wider root.
    let (status, body, _) = download(&c, None, "files,sessions", &s(&c.outside)).await;
    assert_eq!(status, 200);
    assert_eq!(body, b"the Owner's other file");
}

/// A Guest's request through an artifact-scope share of `artifact`
/// (project-relative), as the broker relays it into the Owner's child.
async fn call_artifact(c: &Child, artifact: &str, cmd: &str, args: Value) -> Value {
    let req = Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header("x-ikenga-caps", "files")
        .header("x-ikenga-share-project", PROJECT)
        .header(
            "x-ikenga-share-principal",
            PrincipalId::new_v7().to_string(),
        )
        .header("x-ikenga-share-device", "-")
        .header("x-ikenga-share-role", "guest")
        .header("x-ikenga-share-artifact", artifact);
    let res = c
        .router
        .clone()
        .oneshot(
            req.body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// `GET /__viewer/<token>/<rel>`: the status a mounted path answers.
async fn viewer_get(c: &Child, url: &str, rel: &str) -> u16 {
    let res = c
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("{url}{rel}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    res.status().as_u16()
}

/// The quiet refusal a share member gets for anything outside the share.
fn refused_quietly(v: &Value) -> bool {
    v["ok"] == false
        && v["error"]
            .as_str()
            .is_some_and(|e| e.ends_with(super::rpc_shell::SHARE_REFUSAL))
}

/// Live defects D1/D2 (box-files): a Guest shared ONE file that sits in its
/// project root. A relative path resolves against the project root (the
/// artifact path is project-relative), the files beside the shared one read
/// (fs_read and the viewer mount, so markdown / html assets load), and
/// nothing else does: no sub-folder, no parent, no home, no `/etc`, no
/// `..` climb, no listing.
#[tokio::test]
async fn an_artifact_guest_reads_its_file_relatively_and_the_files_beside_it() {
    let c = child().await;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    write(
        &c.project.join("notes.md"),
        "# Notes\n\n![chart](./chart.png)\n",
    );
    write(&c.project.join("chart.png"), "png bytes");
    let home_file = c
        .project
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("home/home-secret.txt");
    write(&home_file, "home");
    let guest = |cmd: &'static str, args: Value| {
        let c = &c;
        async move { call_artifact(c, "notes.md", cmd, args).await }
    };

    // D1: the shared file by its project-relative path, and absolutely.
    for p in [
        "notes.md",
        "./notes.md",
        s(&c.project.join("notes.md")).as_str(),
    ] {
        let r = guest("fs_read", json!({ "path": p })).await;
        assert_eq!(r["ok"], true, "{p}: {r}");
        assert!(read_text(&r).starts_with("# Notes"), "{p}");
    }
    let st = guest("fs_stat", json!({ "path": "notes.md" })).await;
    assert_eq!(st["ok"], true, "{st}");

    // D2: a file beside it, both spellings.
    for p in ["chart.png", s(&c.project.join("chart.png")).as_str()] {
        let r = guest("fs_read", json!({ "path": p })).await;
        assert_eq!(r["ok"], true, "{p}: {r}");
        assert_eq!(read_text(&r), "png bytes");
    }

    // Still refused, every one with the same quiet text.
    let refusals = [
        s(&c.project.join("docs/brief.md")), // a sub-folder of the root
        "docs/brief.md".to_string(),
        s(&c.outside), // beside the project folder
        "../not-shared.txt".to_string(),
        "x/../../not-shared.txt".to_string(),
        "docs/../../not-shared.txt".to_string(),
        s(&home_file),
        "/etc/hostname".to_string(),
        "~/home-secret.txt".to_string(),
    ];
    for p in &refusals {
        let r = guest("fs_read", json!({ "path": p })).await;
        assert!(refused_quietly(&r), "{p}: {r}");
        assert!(!r.to_string().contains(&s(&c.project)), "{p}: {r}");
    }
    let list = guest("fs_list", json!({ "dir": s(&c.project) })).await;
    assert!(refused_quietly(&list), "a listing names no sibling: {list}");
    let list = guest("fs_list", json!({ "dir": "." })).await;
    assert!(refused_quietly(&list), "{list}");
    let write_sib = guest(
        "fs_write",
        json!({ "path": "chart.png", "contents": [1, 2, 3] }),
    )
    .await;
    assert_eq!(
        write_sib["ok"], false,
        "sibling grant is read-only: {write_sib}"
    );

    // The viewer mount of the file's folder serves the files beside it and
    // nothing below them.
    let mount = guest(
        "viewer_serve",
        json!({ "rootDir": s(&c.project), "filePath": s(&c.project.join("notes.md")) }),
    )
    .await;
    assert_eq!(mount["ok"], true, "{mount}");
    let url = mount["data"]["url"].as_str().unwrap().to_string();
    assert_eq!(viewer_get(&c, &url, "notes.md").await, 200);
    assert_eq!(viewer_get(&c, &url, "chart.png").await, 200);
    assert_eq!(viewer_get(&c, &url, "docs/brief.md").await, 404);
    assert_eq!(viewer_get(&c, &url, "docs/").await, 404);
    // A mount above the folder is refused outright.
    let wide = guest(
        "viewer_serve",
        json!({ "rootDir": s(c.project.parent().unwrap()), "filePath": s(&c.project.join("notes.md")) }),
    )
    .await;
    assert!(refused_quietly(&wide), "{wide}");
}

/// The nested case keeps its folder grant and refuses the folder's parent:
/// a Guest shared `docs/brief.md` reads `docs/` (and below) but not a file
/// in the project root, and its relative paths are project-relative too.
#[tokio::test]
async fn an_artifact_guest_in_a_sub_folder_reads_that_folder_only() {
    let c = child().await;
    let s = |p: &Path| p.to_string_lossy().into_owned();
    write(&c.project.join("docs/img/a.png"), "a");
    write(&c.project.join("top.md"), "top");
    let guest = |cmd: &'static str, args: Value| {
        let c = &c;
        async move { call_artifact(c, "docs/brief.md", cmd, args).await }
    };
    for p in ["docs/brief.md", "docs/img/a.png"] {
        let r = guest("fs_read", json!({ "path": p })).await;
        assert_eq!(r["ok"], true, "{p}: {r}");
    }
    for p in [
        "top.md".to_string(),
        s(&c.project.join("top.md")),
        "docs/../top.md".to_string(),
        "brief.md".to_string(), // relative to the project root, not docs/
    ] {
        let r = guest("fs_read", json!({ "path": p })).await;
        assert!(refused_quietly(&r), "{p}: {r}");
    }
}
