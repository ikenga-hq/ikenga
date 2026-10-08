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
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let data = root.join("data");
    let home = root.join("home");
    let work = root.join("work");
    let project = work.join("proj");
    let outside = work.join("not-shared.txt");
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
        pkgs_dir: None,
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
}
