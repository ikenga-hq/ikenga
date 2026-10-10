//! Router tests for the Ọba git / npx installers and updaters served in
//! WP-18b part c: `oba_install_git`, `oba_install_npx`, `oba_install_bundle`,
//! `oba_install_with_deps`, `oba_resolve_source`, `oba_check_update`,
//! `oba_update`, `oba_auto_update_all`.
//!
//! House pattern (see `rpc_ngwa_tests`): a literal `ServerConfig` →
//! `router_with_store` → `oneshot` POST `/api/rpc` with the bearer token, over
//! a temp home, store and data dir — nothing here reads the real user's
//! `~/.claude`, store or the process-global `fs_roots`. The suite is offline:
//! it proves the arms decode `tauri-cmd.ts`'s argument keys, answer in its
//! response shapes, and refuse — before any fetch and without touching the
//! store — every source the daemon policy (`claude_store::remote`) forbids.
//! The positive fetch (a real public skill over git and npx) is the live
//! check, run against a built `ikenga-server`.

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
use crate::server::rpc_shell::PathGuard;
use crate::server::{router_with_store, ServerConfig};

struct D {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    router: Router,
}

async fn daemon(with_data: bool) -> D {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (home, store, data, allowed) = (
        root.join("home"),
        root.join("store"),
        root.join("data"),
        root.join("allowed"),
    );
    for d in [&home, &store, &data, &allowed] {
        std::fs::create_dir_all(d).unwrap();
    }
    let roots_file = root.join("fs_roots.json");
    std::fs::write(
        &roots_file,
        json!({ "roots": [allowed.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    db.ensure_pool().await.unwrap();

    let router = router_with_store(
        ServerConfig {
            host: "127.0.0.1".into(),
            port: 0,
            static_dir: PathBuf::from("no-spa-here"),
            pkgs_dir: None,
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
    D {
        _tmp: tmp,
        root,
        store,
        router,
    }
}

async fn rpc(router: &Router, cmd: &str, args: Value) -> Value {
    let res = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/rpc")
                .header("authorization", "Bearer tok")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "cmd": cmd, "args": args }).to_string()))
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

fn refused(res: &Value, cmd: &str, needle: &str) {
    assert_eq!(res["ok"], false, "{cmd} must be refused: {res}");
    let e = res["error"].as_str().unwrap_or_default();
    assert!(e.starts_with(&format!("{cmd}: ")), "{e}");
    assert!(e.contains(needle), "{cmd}: expected `{needle}` in `{e}`");
}

fn store_untouched(store: &Path) {
    let names: Vec<_> = std::fs::read_dir(store)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.is_empty(), "the store was touched: {names:?}");
}

/// No `oba_install_*` / update arm answers "not implemented in headless
/// daemon" any more; each decodes the keys `tauri-cmd.ts` sends.
#[tokio::test]
async fn arms_are_served_and_decode_the_wire_keys() {
    let d = daemon(true).await;
    // Required argument missing → a decode error from the arm, not an
    // unserved command.
    for (cmd, args, missing) in [
        (
            "oba_install_git",
            json!({"name": "n", "url": "u"}),
            "`kind` is required",
        ),
        (
            "oba_install_git",
            json!({"kind": "skill", "name": "n"}),
            "`url` is required",
        ),
        (
            "oba_install_npx",
            json!({"kind": "skill", "name": "n"}),
            "`spec` is required",
        ),
        (
            "oba_install_bundle",
            json!({"name": "n"}),
            "`spec` is required",
        ),
        (
            "oba_install_with_deps",
            json!({"kind": "skill", "name": "n", "url": "u"}),
            "`source` is required",
        ),
        ("oba_resolve_source", json!({}), "`url` is required"),
        (
            "oba_check_update",
            json!({"kind": "skill"}),
            "`name` is required",
        ),
        ("oba_update", json!({"name": "n"}), "`kind` is required"),
    ] {
        let res = rpc(&d.router, cmd, args).await;
        refused(&res, cmd, missing);
        assert!(
            !res["error"].as_str().unwrap().contains("not implemented"),
            "{cmd} is served: {res}"
        );
    }
    // A bad kind decodes then fails the core's own parse.
    let res = rpc(
        &d.router,
        "oba_install_git",
        json!({"kind": "spaceship", "name": "n", "url": "https://github.com/o/r"}),
    )
    .await;
    assert_eq!(res["ok"], false);
    store_untouched(&d.store);
}

/// The git installer refuses, with its reason and before any fetch, every
/// source that is not a public https URL — the camelCase keys `tauri-cmd.ts`
/// sends decode (gitRef / fromCatalog / expectSha / expectHash).
#[tokio::test]
async fn install_git_refuses_every_non_https_source() {
    let d = daemon(true).await;
    let local_repo = d.root.join("allowed/repo");
    std::fs::create_dir_all(&local_repo).unwrap();
    for url in [
        format!("file://{}", local_repo.display()),
        local_repo.to_string_lossy().into_owned(),
        "~/repo".to_string(),
        "ext::sh -c touch /tmp/oba-arm-pwned".to_string(),
        "fd::17".to_string(),
        "--upload-pack=touch /tmp/oba-arm-pwned".to_string(),
        "git@github.com:o/r.git".to_string(),
        "ssh://git@github.com/o/r".to_string(),
        "git://github.com/o/r".to_string(),
        "http://github.com/o/r".to_string(),
        "https://user:pw@github.com/o/r".to_string(),
        "https://127.0.0.1/o/r".to_string(),
        "https://localhost/o/r".to_string(),
        "https://169.254.169.254/latest".to_string(),
    ] {
        let res = rpc(
            &d.router,
            "oba_install_git",
            json!({
                "kind": "skill", "name": "demo", "url": url,
                "gitRef": null, "fromCatalog": false,
                "expectSha": null, "expectHash": null,
            }),
        )
        .await;
        refused(&res, "oba_install_git", "public host");
    }
    // A hostile ref on an otherwise fine URL.
    let res = rpc(
        &d.router,
        "oba_install_git",
        json!({"kind": "skill", "name": "demo", "url": "https://github.com/o/r",
               "gitRef": "--upload-pack=x"}),
    )
    .await;
    refused(&res, "oba_install_git", "plain branch or tag");
    // A malformed pin is refused before anything.
    let res = rpc(
        &d.router,
        "oba_install_git",
        json!({"kind": "skill", "name": "demo", "url": "https://github.com/o/r",
               "expectSha": "not-hex"}),
    )
    .await;
    refused(&res, "oba_install_git", "not a commit SHA");
    store_untouched(&d.store);
}

#[tokio::test]
async fn npx_bundle_and_resolve_refuse_local_and_odd_specs() {
    let d = daemon(true).await;
    for spec in [
        "./local-skill",
        "/home/me/skill",
        "~/skill",
        "file:///home/me/skill",
        "a/b/c",
        "-y",
        "git@github.com:o/r",
    ] {
        let res = rpc(
            &d.router,
            "oba_install_npx",
            json!({"kind": "skill", "name": "demo", "spec": spec,
                   "fromCatalog": false, "expectSha": null, "expectHash": null}),
        )
        .await;
        refused(&res, "oba_install_npx", "public host");

        let res = rpc(
            &d.router,
            "oba_install_bundle",
            json!({"name": "demo", "spec": spec, "scope": null, "fromCatalog": false}),
        )
        .await;
        refused(&res, "oba_install_bundle", "public host");
    }
    // The dry-run resolve: a local path / file:// is the same refusal.
    for url in ["/home/me/skill", "file:///home/me/skill", "./x.git"] {
        let res = rpc(
            &d.router,
            "oba_resolve_source",
            json!({"url": url, "kind": null, "name": null, "gitRef": null}),
        )
        .await;
        assert_eq!(res["ok"], false, "{url}: {res}");
        let e = res["error"].as_str().unwrap();
        assert!(
            e.contains("public host") || e.contains("not a git URL"),
            "{url}: {e}"
        );
    }
    let res = rpc(
        &d.router,
        "oba_resolve_source",
        json!({"url": "file:///home/me/skill"}),
    )
    .await;
    refused(&res, "oba_resolve_source", "public host");
    store_untouched(&d.store);
}

/// `oba_install_with_deps`: a `local` target source, and a `local` dependency
/// named by a caller-supplied catalog row, are refused.
#[tokio::test]
async fn install_with_deps_refuses_a_local_source() {
    let d = daemon(true).await;
    let res = rpc(
        &d.router,
        "oba_install_with_deps",
        json!({"kind": "skill", "name": "demo", "source": "local",
               "url": d.root.join("allowed").to_string_lossy(),
               "gitRef": null, "fromCatalog": false, "catalog": [],
               "expectSha": null, "expectHash": null}),
    )
    .await;
    refused(&res, "oba_install_with_deps", "desktop-only");

    let res = rpc(
        &d.router,
        "oba_install_with_deps",
        json!({"kind": "skill", "name": "demo", "source": "git",
               "url": "file:///tmp/whatever",
               "catalog": [{"kind": "skill", "name": "dep", "source": "local", "url": "/tmp"}]}),
    )
    .await;
    refused(&res, "oba_install_with_deps", "public host");

    // An unknown fetch source is the core's own error.
    let res = rpc(
        &d.router,
        "oba_install_with_deps",
        json!({"kind": "skill", "name": "demo", "source": "ftp", "url": "x"}),
    )
    .await;
    refused(&res, "oba_install_with_deps", "git|npx|local");
    store_untouched(&d.store);
}

/// Without `--data-dir` the closure installer (which needs the scope list)
/// says so, like every other arm that takes the database.
#[tokio::test]
async fn install_with_deps_needs_the_database() {
    let d = daemon(false).await;
    let res = rpc(
        &d.router,
        "oba_install_with_deps",
        json!({"kind": "skill", "name": "demo", "source": "git",
               "url": "https://github.com/o/r"}),
    )
    .await;
    assert_eq!(res["ok"], false);
    assert!(
        res["error"].as_str().unwrap().contains("no database"),
        "{res}"
    );
}

fn registry(entries: Value) -> String {
    json!({ "schemaVersion": 2, "entries": entries }).to_string()
}

fn entry(kind: &str, name: &str, source: &str, url: &str, store: &Path) -> Value {
    let dir = store.join(format!("{kind}s")).join(name);
    json!({
        "kind": kind, "name": name, "storePath": dir.to_string_lossy(),
        "description": null, "modifiedMs": 0, "enabledIn": [], "requires": [],
        "members": [], "source": source, "url": url, "ref": null,
        "version": "0123456789abcdef0123456789abcdef01234567",
        "canonicalPath": dir.to_string_lossy(), "managed": true,
        "installedAt": null, "updatedAt": null, "fromCatalog": false,
        "autoUpdate": true, "pinned": false, "hash": null,
    })
}

/// Update, update-check and the auto-update batch re-fetch from the URL a
/// registry row RECORDS. A row written by the desktop with a `file://` source
/// (or whose provenance was edited) cannot make the daemon read a local path:
/// the policy applies to the recorded URL exactly as to a typed one.
#[tokio::test]
async fn update_check_and_auto_update_apply_the_policy_to_recorded_urls() {
    let d = daemon(true).await;
    std::fs::write(
        d.store.join("registry.json"),
        registry(json!([
            entry("skill", "from-file", "git", "file:///etc", &d.store),
            entry("skill", "from-path", "git", "/etc", &d.store),
            entry("skill", "from-npx", "npx", "../../etc", &d.store),
        ])),
    )
    .unwrap();

    for name in ["from-file", "from-path"] {
        let res = rpc(
            &d.router,
            "oba_check_update",
            json!({"kind": "skill", "name": name}),
        )
        .await;
        refused(&res, "oba_check_update", "public host");
        let res = rpc(
            &d.router,
            "oba_update",
            json!({"kind": "skill", "name": name, "expectSha": null, "expectHash": null}),
        )
        .await;
        refused(&res, "oba_update", "public host");
    }
    let res = rpc(
        &d.router,
        "oba_check_update",
        json!({"kind": "skill", "name": "from-npx"}),
    )
    .await;
    // npx entries are checked through `gh_url(spec)` (always github.com);
    // whatever the spec says, no local path is reachable. It may fail to
    // reach the network; it must not succeed against /etc.
    if res["ok"] == true {
        assert_eq!(res["data"]["behind"], true);
    }
    let res = rpc(
        &d.router,
        "oba_update",
        json!({"kind": "skill", "name": "from-npx"}),
    )
    .await;
    assert_eq!(res["ok"], false, "{res}");

    // Not in the registry at all: the core's own error, in the wire envelope.
    let res = rpc(
        &d.router,
        "oba_check_update",
        json!({"kind": "skill", "name": "nope"}),
    )
    .await;
    refused(&res, "oba_check_update", "not in registry");

    // The batch reports each failure per row and never aborts; its shape is
    // `AutoUpdateSummary` in tauri-cmd.ts. `pins: null` is what the FE sends.
    let res = rpc(&d.router, "oba_auto_update_all", json!({"pins": null})).await;
    assert_eq!(res["ok"], true, "{res}");
    let data = &res["data"];
    assert!(data["updated"].is_array() && data["current"].is_array() && data["errored"].is_array());
    let errored: Vec<_> = data["errored"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    for name in ["from-file", "from-path"] {
        assert!(errored.contains(&name.to_string()), "{name} in {errored:?}");
    }
    for row in data["errored"].as_array().unwrap() {
        assert_eq!(row["status"], "error");
        assert!(row["error"].is_string(), "{row}");
    }
    // Nothing about the vault changed: still only the registry we wrote.
    let mut names: Vec<_> = std::fs::read_dir(&d.store)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["registry.json"]);
}

/// An empty batch has the documented empty shape (not null, not an error).
#[tokio::test]
async fn auto_update_all_on_an_empty_vault_is_the_empty_summary() {
    let d = daemon(true).await;
    let res = rpc(&d.router, "oba_auto_update_all", json!({})).await;
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(
        res["data"],
        json!({"updated": [], "current": [], "errored": []})
    );
}
