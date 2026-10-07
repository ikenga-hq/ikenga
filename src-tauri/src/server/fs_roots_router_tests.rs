//! Router tests of the `fs_roots_*` arms on a T1 principal child (gap audit
//! 2026-10-06 ranks 1 and 2): the list seeded with the principal's home, the
//! principal's own add / remove / reset with path validation, the caps and
//! class the arms need, and — once a real folder project exists — the
//! invite path's `share_project_info` resolving it while Default stays
//! unshareable.

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
const OWN: &str = "files,sessions,dispatch,approve,install,settings,secrets";

struct Child {
    _tmp: tempfile::TempDir,
    router: Router,
    home: PathBuf,
    data: PathBuf,
    elsewhere: PathBuf,
}

/// A principal child whose `fs_roots.json` was never set up, loaded the way
/// `serve_single_tenant` loads it for a principal child: seeded with home.
async fn child() -> Child {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    let data = root.join("data");
    let elsewhere = root.join("srv/shared");
    for dir in [&home, &data, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(root.join("srv/notes.txt"), "a file").unwrap();
    let roots = crate::fs_roots::FsRoots::load_seeded(
        data.join("fs_roots.json"),
        vec![home.to_string_lossy().into_owned()],
    )
    .unwrap();

    let db = Arc::new(crate::db::PaDb::new(data.join("ikenga.db")));
    db.ensure_pool().await.unwrap();
    let config = ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir: Some(data.clone()),
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
        Some(home.clone()),
        PathGuard::roots(Arc::new(roots)),
        None,
        DaemonAccess::principal_child(Default::default()),
        None,
        super::UpdateSource::Default,
    );
    Child {
        _tmp: tmp,
        router,
        home,
        data,
        elsewhere,
    }
}

async fn send(c: &Child, req: axum::http::request::Builder, cmd: &str, args: Value) -> Value {
    let res = c
        .router
        .clone()
        .oneshot(
            req.header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn relayed(caps: &str) -> axum::http::request::Builder {
    Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("x-ikenga-principal", PrincipalId::new_v7().to_string())
        .header("x-ikenga-caps", caps)
}

/// The principal's own request, relayed by the broker with full caps.
async fn own(c: &Child, cmd: &str, args: Value) -> Value {
    send(c, relayed(OWN), cmd, args).await
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn err(v: &Value) -> &str {
    v["error"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {v}"))
}

#[tokio::test]
async fn a_principal_starts_with_its_home_and_edits_its_own_list() {
    let c = child().await;
    let listed = own(&c, "fs_roots_list", json!({})).await;
    assert_eq!(listed["data"], json!([s(&c.home)]), "{listed}");

    // Add: stored canonical, deduplicated by where it points.
    let spelled = format!("{}/../shared", s(&c.elsewhere));
    let added = own(&c, "fs_roots_add", json!({ "path": spelled })).await;
    assert_eq!(
        added["data"],
        json!([s(&c.home), s(&c.elsewhere)]),
        "{added}"
    );
    let again = own(&c, "fs_roots_add", json!({ "path": s(&c.elsewhere) })).await;
    assert_eq!(again["data"], added["data"]);

    // The new root is live for the fs arms at once.
    let listing = own(&c, "fs_list", json!({ "path": s(&c.elsewhere) })).await;
    assert_eq!(listing["ok"], true, "{listing}");

    // Remove, then remove what is not there.
    let removed = own(&c, "fs_roots_remove", json!({ "path": s(&c.elsewhere) })).await;
    assert_eq!(removed["data"], json!([s(&c.home)]), "{removed}");
    let missing = own(&c, "fs_roots_remove", json!({ "path": s(&c.elsewhere) })).await;
    assert!(
        err(&missing).contains("is not in the folder list"),
        "{missing}"
    );
    let refused = own(&c, "fs_list", json!({ "path": s(&c.elsewhere) })).await;
    assert!(err(&refused).contains("outside allowlist"), "{refused}");

    // Emptied on purpose, then reset back to the seed.
    let emptied = own(&c, "fs_roots_remove", json!({ "path": s(&c.home) })).await;
    assert_eq!(emptied["data"], json!([]));
    let reset = own(&c, "fs_roots_reset", json!({})).await;
    assert_eq!(reset["data"], json!([s(&c.home)]), "{reset}");
}

#[tokio::test]
async fn a_root_must_be_an_absolute_existing_folder_outside_the_daemons_state() {
    let c = child().await;
    let notes = c.elsewhere.parent().unwrap().join("notes.txt");
    let cases = [
        (json!({}), "`path` is required"),
        (json!({ "path": "  " }), "`path` is required"),
        (json!({ "path": "srv/shared" }), "is not an absolute path"),
        (json!({ "path": "~/work" }), "is not an absolute path"),
        (json!({ "path": s(&c.home.join("nope")) }), "does not exist"),
        (json!({ "path": s(&notes) }), "is not a folder"),
        (json!({ "path": s(&c.data) }), "data"),
    ];
    for (args, want) in cases {
        let res = own(&c, "fs_roots_add", args.clone()).await;
        assert!(err(&res).contains(want), "{args} → {res}");
    }
    let listed = own(&c, "fs_roots_list", json!({})).await;
    assert_eq!(listed["data"], json!([s(&c.home)]), "nothing was added");
}

/// The arm serves its caller's own list only: a `principal` argument never
/// reaches it from the broker (it routes those), so here it is refused; a
/// relay without `settings` (a lower-tier device) and a share member are
/// refused by the access pre-hook.
#[tokio::test]
async fn own_scope_only_and_the_caps_the_arms_need() {
    let c = child().await;
    let res = own(
        &c,
        "fs_roots_add",
        json!({ "path": s(&c.elsewhere), "principal": "bob" }),
    )
    .await;
    assert!(
        err(&res).starts_with("Not available on this server"),
        "{res}"
    );

    let view = send(
        &c,
        relayed("files,sessions"),
        "fs_roots_add",
        json!({ "path": s(&c.elsewhere) }),
    )
    .await;
    assert!(err(&view).contains("missing=settings"), "{view}");
    // Listing needs only `files`.
    let listed = send(&c, relayed("files,sessions"), "fs_roots_list", json!({})).await;
    assert_eq!(listed["ok"], true, "{listed}");

    let member = send(
        &c,
        relayed(OWN)
            .header("x-ikenga-share-project", "p")
            .header(
                "x-ikenga-share-principal",
                PrincipalId::new_v7().to_string(),
            )
            .header("x-ikenga-share-device", "-")
            .header("x-ikenga-share-role", "operator"),
        "fs_roots_reset",
        json!({}),
    )
    .await;
    assert_eq!(member["ok"], false, "{member}");
    let listed = own(&c, "fs_roots_list", json!({})).await;
    assert_eq!(listed["data"], json!([s(&c.home)]));
}

/// Gap audit rank 2: with its home seeded, a T1 principal can open a folder
/// as a project, and the invite path's `share_project_info` (the broker's
/// call on `access_invite_issue`) resolves it — while the folderless
/// Default keeps its explanation.
#[tokio::test]
async fn a_folder_project_under_the_seeded_home_is_shareable() {
    let c = child().await;
    let folder = c.home.join("album");
    std::fs::create_dir_all(&folder).unwrap();
    let created = own(
        &c,
        "project_create",
        json!({ "id": "album", "displayName": "Album", "rootPath": s(&folder) }),
    )
    .await;
    assert_eq!(created["ok"], true, "{created}");
    let project_id = "album";

    let broker = |project: &str| {
        let req = Request::builder()
            .method("POST")
            .uri("/api/rpc")
            .header(crate::access::INTERNAL_CALL_HEADER, "1");
        (req, json!({ "projectId": project }))
    };
    let (req, args) = broker(project_id);
    let info = send(&c, req, "share_project_info", args).await;
    assert_eq!(info["ok"], true, "{info}");
    assert_eq!(info["data"]["root"], s(&folder));

    let (req, args) = broker("default");
    let default = send(&c, req, "share_project_info", args).await;
    assert_eq!(
        default["error"],
        format!(
            "invalid_request: {}",
            crate::access::share::NO_FOLDER_TO_SHARE
        ),
        "{default}"
    );
}
