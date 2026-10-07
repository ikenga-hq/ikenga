//! Router tests for the Ngwa snapshot and the two pkg reads served beside it
//! (`ngwa_snapshot`, `pkg_health_scan`, `pkg_trust_list`).
//!
//! House pattern (see `rpc_claude_vault_tests`): a literal `ServerConfig` →
//! `router_with_store` → `oneshot` POST `/api/rpc` with the bearer token. The
//! router home, store, data dir and `--pkgs-dir` are temp dirs and the fs
//! allowlist is exactly `allowed/`, so nothing here reads the real user's
//! `~/.claude`, store or process-global `fs_roots`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::db::PaDb;
use crate::engines::EngineRegistry;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::pkg_index::{RECORDS_NOT_SERVED, RECORDS_UNAVAILABLE_ID};
use crate::server::rpc_shell::PathGuard;
use crate::server::shared::ngwa::{NOT_AVAILABLE_ON_SERVER, PARTIALLY_UNREADABLE};
use crate::server::shared::projects::{create_project, CreateArgs};
use crate::server::{router_with_store, ServerConfig};

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn manifest(id: &str, api: &str, extra: Value) -> String {
    let mut m = json!({ "id": id, "name": id, "version": "0.1.0", "ikenga_api": api });
    m.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    m.to_string()
}

/// What `--pkgs-dir` the daemon is given.
#[derive(Clone, Copy, PartialEq)]
enum Pkgs {
    /// No `--pkgs-dir` at all.
    Absent,
    /// A `--pkgs-dir` that exists and holds nothing.
    Empty,
    /// The fixture set: live, api-incompatible, unloadable, duplicate.
    Fixture,
}

/// A daemon over temp state. `home_skill` names the one skill in the router
/// home's `~/.claude`, so two daemons can prove each reads only its own.
struct Ng {
    _tmp: tempfile::TempDir,
    store: PathBuf,
    /// The registered project whose root is outside the fs allowlist.
    far: PathBuf,
    /// The `--pkgs-dir` (passed to the daemon unless `Pkgs::Absent`).
    pkgs: PathBuf,
    router: Router,
}

async fn daemon(home_skill: &str, with_data: bool, with_pkgs: bool) -> Ng {
    daemon_with(
        home_skill,
        with_data,
        if with_pkgs {
            Pkgs::Fixture
        } else {
            Pkgs::Absent
        },
    )
    .await
}

async fn daemon_with(home_skill: &str, with_data: bool, pkgs_mode: Pkgs) -> Ng {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (home, store, data) = (root.join("home"), root.join("store"), root.join("data"));
    let (allowed, outside, pkgs) = (
        root.join("allowed"),
        root.join("outside"),
        root.join("pkgs"),
    );
    for d in [&home, &store, &data, &allowed, &outside, &pkgs] {
        std::fs::create_dir_all(d).unwrap();
    }

    write(
        &home.join(format!(".claude/skills/{home_skill}/SKILL.md")),
        &format!("---\nname: {home_skill}\ndescription: home skill\n---\nsteps\n"),
    );
    write(
        &store.join("agents/helper.md"),
        "---\nname: helper\ndescription: store agent\n---\nYou help.\n",
    );
    let proj = allowed.join("proj");
    write(
        &proj.join(".claude/agents/local.md"),
        "---\nname: local\ndescription: project agent\n---\nbody\n",
    );
    let far = outside.join("far");
    write(
        &far.join(".claude/agents/secret.md"),
        "---\nname: secret\ndescription: outside the allowlist\n---\nx\n",
    );

    // --pkgs-dir: one live pkg, one api-incompatible, one unloadable, and a
    // second directory claiming the live pkg's id.
    if pkgs_mode == Pkgs::Fixture {
        let live = json!({
            "ui": { "routes": [{ "path": "/x", "kind": "iframe", "source": "dist/index.html" }] },
            "mcp": [{ "name": "srv", "command": "bin/mcp" }],
            "cron": [{ "id": "tick", "expr": "* * * * *", "handler": "h" }],
            "permissions": { "shell.execute": ["git *"] },
        });
        write(
            &pkgs.join("a-live/manifest.json"),
            &manifest("com.test.live", "1", live),
        );
        write(
            &pkgs.join("b-future/manifest.json"),
            &manifest("com.test.future", "999", json!({})),
        );
        write(&pkgs.join("c-broken/manifest.json"), "{ not json");
        write(
            &pkgs.join("d-dup/manifest.json"),
            &manifest("com.test.live", "1", json!({})),
        );
    }

    let roots_file = root.join("fs_roots.json");
    std::fs::write(
        &roots_file,
        json!({ "roots": [allowed.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();

    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let pool = db.ensure_pool().await.unwrap();
    for (id, path) in [("proj", &proj), ("far", &far)] {
        create_project(
            &pool,
            CreateArgs {
                id: id.into(),
                display_name: id.into(),
                root_path: Some(s(path)),
                icon: None,
                color: None,
                description: None,
            },
        )
        .await
        .unwrap();
    }

    let router = router_with_store(
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: (pkgs_mode != Pkgs::Absent).then(|| pkgs.clone()),
            data_dir: with_data.then(|| data.clone()),
            auth_token: Some("tok".into()),
            allowed_origins: vec![],
            idle_timeout_secs: None,
            executor_tier: ExecutorTier::T0,
        },
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        with_data.then(|| db.clone()),
        Some(home),
        PathGuard::roots(Arc::new(roots)),
        Some(store.clone()),
    );
    Ng {
        _tmp: tmp,
        store,
        far,
        pkgs,
        router,
    }
}

async fn rpc(router: &Router, cmd: &str) -> Value {
    let res = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", "Bearer tok")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn ok(router: &Router, cmd: &str) -> Value {
    let res = rpc(router, cmd).await;
    assert_eq!(res["ok"], true, "{cmd} → {res}");
    res["data"].clone()
}

fn item<'a>(snap: &'a Value, id: &str) -> &'a Value {
    snap["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("no item {id} in {}", snap["items"]))
}

fn ids(snap: &Value) -> Vec<String> {
    snap["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn unavailable(h: &Value) -> bool {
    h["ok"] == false
        && h["error"]
            .as_str()
            .is_some_and(|e| e.contains(NOT_AVAILABLE_ON_SERVER))
}

/// The snapshot carries what the daemon sees — pkgs from `--pkgs-dir`, the
/// router home's config, the project's config keyed by its registered id,
/// the store — and names what it cannot see as unavailable, never as an
/// empty or zeroed source.
#[tokio::test]
async fn ngwa_snapshot_serves_what_the_daemon_sees_and_names_the_rest() {
    let d = daemon("tidy", true, true).await;
    let snap = ok(&d.router, "ngwa_snapshot").await;
    let src = &snap["sources"];

    for name in ["kernel", "engine_assets", "trust", "usage"] {
        assert!(unavailable(&src[name]), "{name}: {}", src[name]);
    }
    assert_eq!(src["kernel"]["count"], 2, "live + incompatible are listed");
    assert_eq!(src["trust"]["count"], 0);
    assert_eq!(src["oba"]["ok"], true, "{}", src["oba"]);

    // The config scan answered, but the registered project outside the fs
    // allowlist was refused: partially unreadable, naming that root — never
    // `ok: true` with the project silently missing.
    let cfg = &src["engine_config"];
    assert_eq!(cfg["ok"], false, "{cfg}");
    let cfg_err = cfg["error"].as_str().unwrap();
    assert!(cfg_err.starts_with(PARTIALLY_UNREADABLE), "{cfg_err}");
    assert!(
        cfg_err.contains(&format!("project roots not scanned: `{}`", s(&d.far))),
        "{cfg_err}"
    );
    assert!(cfg_err.contains("outside allowlist"), "{cfg_err}");
    assert!(
        cfg["count"].as_u64().unwrap() > 0,
        "the rows it read still count"
    );

    let live = item(&snap, "com.test.live");
    assert_eq!(live["kind"], "app");
    assert_eq!(live["state"], "enabled");
    assert_eq!(
        live["trust"]["state"], "not_applicable",
        "not evaluated, not unsigned"
    );
    assert_eq!(live["trust"]["signed"], false);
    assert!(live["trust"]["last_granted_at_ms"].is_null());
    // What the manifest declares is still read — declared, not evaluated.
    assert_eq!(live["trust"]["perms"]["shell_execute"], json!(["git *"]));
    assert_eq!(live["trust"]["perms"]["vault_keys"], json!([]));
    assert!(live["runtime"].is_null(), "unknown, not stopped");
    assert!(live["usage"].is_null(), "unknown, not zero");
    assert_eq!(item(&snap, "com.test.future")["state"], "broken");
    assert_eq!(
        item(&snap, "schedule:personal:com.test.live:tick")["owner_pkg_id"],
        "com.test.live"
    );

    // Config: the router home's skill, the allowlisted project's agent keyed
    // by its registered id, and nothing from the project outside the allowlist.
    assert!(item(&snap, "skill:personal:tidy")["usage"].is_null());
    item(&snap, "agent:project:proj:local");
    assert!(
        !ids(&snap).iter().any(|i| i.contains("secret")),
        "a root outside the allowlist is never scanned: {:?}",
        ids(&snap)
    );

    // Ọba: the router store's agent.
    let helper = item(&snap, "agent:personal:helper");
    assert!(
        helper["install_path"]
            .as_str()
            .unwrap()
            .starts_with(&s(&d.store)),
        "{helper}"
    );
}

/// Config and store are the ROUTER's principal's: two daemons with different
/// homes each see only their own `~/.claude` (G-PRINCIPAL).
#[tokio::test]
async fn ngwa_snapshot_reads_the_router_principals_home() {
    let a = daemon("alpha", true, false).await;
    let b = daemon("beta", true, false).await;
    let (sa, sb) = (
        ok(&a.router, "ngwa_snapshot").await,
        ok(&b.router, "ngwa_snapshot").await,
    );
    assert!(ids(&sa).contains(&"skill:personal:alpha".to_string()));
    assert!(!ids(&sa).contains(&"skill:personal:beta".to_string()));
    assert!(ids(&sb).contains(&"skill:personal:beta".to_string()));
    assert!(!ids(&sb).contains(&"skill:personal:alpha".to_string()));
    assert_eq!(
        sa["sources"]["kernel"]["count"], 0,
        "no --pkgs-dir, no pkgs"
    );
}

/// Without `--data-dir` the snapshot still answers; the store source names
/// the missing database instead of reading as an empty store.
#[tokio::test]
async fn ngwa_snapshot_without_a_data_dir_reports_the_store_unreadable() {
    let d = daemon("tidy", false, true).await;
    let snap = ok(&d.router, "ngwa_snapshot").await;
    assert_eq!(snap["sources"]["oba"]["ok"], false);
    assert!(snap["sources"]["oba"]["error"]
        .as_str()
        .unwrap()
        .contains("--data-dir"));
    item(&snap, "skill:personal:tidy");
    item(&snap, "com.test.live");
}

#[tokio::test]
async fn pkg_trust_list_refuses_as_not_available_on_this_server() {
    let d = daemon("tidy", true, true).await;
    let res = rpc(&d.router, "pkg_trust_list").await;
    assert_eq!(res["ok"], false, "{res}");
    let err = res["error"].as_str().unwrap();
    assert!(err.starts_with("pkg_trust_list: "), "{err}");
    assert!(err.contains(NOT_AVAILABLE_ON_SERVER), "{err}");
}

/// The health scan leads with the `records_unavailable` row, then lists the
/// `--pkgs-dir` entries the daemon could not serve, in the desktop's shape.
#[tokio::test]
async fn pkg_health_scan_names_what_it_cannot_check_and_lists_what_it_can() {
    let d = daemon("tidy", true, true).await;
    let rows = ok(&d.router, "pkg_health_scan").await;
    let rows = rows.as_array().unwrap();
    assert_eq!(rows[0]["id"], RECORDS_UNAVAILABLE_ID);
    assert_eq!(rows[0]["issue"], json!({ "kind": "records_unavailable" }));
    assert_eq!(rows[0]["detail"], RECORDS_NOT_SERVED);
    assert!(RECORDS_NOT_SERVED.contains(NOT_AVAILABLE_ON_SERVER));

    let kinds: Vec<(String, String)> = rows[1..]
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["issue"]["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for want in ["c-broken", "com.test.future"] {
        assert!(
            kinds.contains(&(want.to_string(), "pkgs_dir_unloadable".to_string())),
            "{want} missing from {kinds:?}"
        );
    }

    // The second directory claiming com.test.live is a duplicate, never an
    // unloadable / unregistered row under the SERVED pkg's id — that would
    // mark the working pkg broken in Store and Health.
    let dup: Vec<&Value> = rows[1..]
        .iter()
        .filter(|r| r["id"] == "com.test.live")
        .collect();
    assert_eq!(dup.len(), 1, "{dup:?}");
    assert_eq!(dup[0]["issue"]["kind"], "pkgs_dir_duplicate");
    assert_eq!(
        dup[0]["issue"]["served_path"],
        s(&d.pkgs.join("a-live")),
        "{}",
        dup[0]
    );
    assert_eq!(dup[0]["install_path"], s(&d.pkgs.join("d-dup")));
    assert!(dup[0]["detail"]
        .as_str()
        .unwrap()
        .starts_with("duplicate, not served"));
    for r in &rows[1..] {
        for key in ["id", "install_path", "enabled", "issue", "detail"] {
            assert!(r.get(key).is_some(), "{key} missing from {r}");
        }
    }

    // No --pkgs-dir: still never a bare `[]`.
    let bare = daemon("tidy", true, false).await;
    let rows = ok(&bare.router, "pkg_health_scan").await;
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["issue"]["kind"], "records_unavailable");
}

/// An EMPTY `--pkgs-dir`: no pkg carries a trust error, so only the arm's own
/// `not_served.trust` can make the trust source read unavailable. (With pkgs
/// present, each pkg's refused trust would mask a daemon that stopped
/// declaring trust unserved.)
#[tokio::test]
async fn ngwa_snapshot_with_an_empty_pkgs_dir_still_reads_trust_unavailable() {
    let d = daemon_with("tidy", true, Pkgs::Empty).await;
    let snap = ok(&d.router, "ngwa_snapshot").await;
    let trust = &snap["sources"]["trust"];
    assert_eq!(trust["ok"], false, "{trust}");
    assert!(unavailable(trust), "{trust}");
    assert_eq!(
        trust["error"],
        crate::server::rpc_claude::TRUST_NOT_SERVED,
        "{trust}"
    );
    assert_eq!(snap["sources"]["kernel"]["count"], 0, "no pkgs");

    // The health scan of an empty folder is still never a bare `[]`.
    let rows = ok(&d.router, "pkg_health_scan").await;
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["issue"]["kind"], "records_unavailable");
}
