//! Router tests for the Ngwa vault arms (WP-19 slice 7): `claude_store_*`,
//! `claude_primitive_*` and the served `oba_*`.
//!
//! House pattern (see `rpc_claude`'s tests): a literal `ServerConfig` →
//! `router_with_store` → `oneshot` POST `/api/rpc` with the bearer token. The
//! router home is a temp dir with a fixture `~/.claude`, the store is a temp
//! store, the data dir holds `ikenga.db` with one project row (`project:proj`)
//! and a would-be secret, and the fs allowlist is exactly `allowed/`. Nothing
//! here touches the real user's home, store or process env.
//!
//! Happy paths are compared with the desktop body (`Vault::follow_in`, the
//! desktop's reach over the same temp vault) or checked on disk; the refusals
//! prove a link planted at a confined path never becomes a read, write or
//! delete outside the vault; the regression tests prove the desktop reach
//! still follows those same links exactly as before.

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
use crate::server::shared::claude_store::{
    self as cs, ClaudeStoreEntry, ClaudeStoreMutation, RegistryFile, RegistryProvenance, RelinkRow,
    SafeDeleteOutcome, Vault,
};
use crate::server::shared::projects::{create_project, CreateArgs};
use crate::server::{router_with_store, ServerConfig};

const AGENT: &str = "---\nname: helper\ndescription: store agent\n---\nYou help.\n";
const SKILL: &str = "---\nname: tidy\ndescription: store skill\n---\nsteps\n";
const COMMAND: &str = "---\ndescription: store command\n---\nsay hello\n";
const OUTSIDE_SECRET: &str = "outside secret";
const DATA_SECRET: &str = "daemon secret";

fn config(data_dir: Option<PathBuf>) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: None,
        data_dir,
        auth_token: Some("tok".into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn is_link(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

fn present(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) {
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, at).unwrap();
}

fn wire<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap()
}

/// A daemon over a temp vault. `outside/` is a sibling the allowlist does not
/// cover; `data/` is `--data-dir` (when given) and, deliberately, inside the
/// allowlist.
struct Vt {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    store: PathBuf,
    data: PathBuf,
    allowed: PathBuf,
    outside: PathBuf,
    proj: PathBuf,
    db: Arc<PaDb>,
    router: Router,
}

impl Vt {
    fn claude(&self) -> PathBuf {
        self.home.join(".claude")
    }

    fn proj_claude(&self) -> PathBuf {
        self.proj.join(".claude")
    }

    /// The desktop's reach over this same temp vault.
    fn desktop(&self) -> Vault<'static> {
        Vault::follow_in(Some(self.home.clone()), Some(self.store.clone()))
    }

    fn write_registry(&self, entries: Vec<ClaudeStoreEntry>) {
        let rf = RegistryFile {
            entries,
            ..RegistryFile::default()
        };
        write(
            &self.store.join("registry.json"),
            &serde_json::to_string_pretty(&rf).unwrap(),
        );
    }

    fn registry(&self) -> Value {
        serde_json::from_str(&read(&self.store.join("registry.json"))).unwrap()
    }
}

fn entry(kind: &str, name: &str, store_path: &Path) -> ClaudeStoreEntry {
    ClaudeStoreEntry {
        kind: kind.into(),
        name: name.into(),
        store_path: s(store_path),
        description: None,
        modified_ms: 0,
        enabled_in: vec![],
        requires: vec![],
        members: vec![],
        provenance: RegistryProvenance::local(s(store_path)),
    }
}

async fn vault_with(with_data: bool) -> Vt {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (home, store, data) = (root.join("home"), root.join("store"), root.join("data"));
    let (allowed, outside) = (root.join("allowed"), root.join("outside"));
    let proj = allowed.join("proj");
    for d in [&home, &store, &data, &allowed, &outside, &proj] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::create_dir_all(home.join(".claude/agents")).unwrap();
    std::fs::create_dir_all(proj.join(".claude")).unwrap();

    // The store: an agent, a command, a skill, a hook and an MCP fragment.
    write(&store.join("agents/helper.md"), AGENT);
    write(&store.join("commands/hello.md"), COMMAND);
    write(&store.join("skills/tidy/SKILL.md"), SKILL);
    write(&store.join("skills/tidy/notes.md"), "supporting");
    write(
        &store.join("hooks/guard.json"),
        &json!({
            "event": "PreToolUse",
            "file": "shared",
            "block": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }],
        })
        .to_string(),
    );
    write(
        &store.join("mcp/royalti.json"),
        &json!({ "command": "npx", "args": ["royalti"] }).to_string(),
    );

    write(&outside.join("secret.md"), OUTSIDE_SECRET);
    write(&data.join("secret.json"), DATA_SECRET);

    let roots_file = root.join("fs_roots.json");
    std::fs::write(
        &roots_file,
        // The data dir is inside the allowlist on purpose: the misconfiguration
        // `server::reserved` exists for, so its refusals are what is tested.
        json!({ "roots": [allowed.to_string_lossy(), data.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let roots = crate::fs_roots::FsRoots::load(roots_file).unwrap();

    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let pool = db.ensure_pool().await.unwrap();
    create_project(
        &pool,
        CreateArgs {
            id: "proj".into(),
            display_name: "Proj".into(),
            root_path: Some(s(&proj)),
            icon: None,
            color: None,
            description: None,
        },
    )
    .await
    .unwrap();

    let router = router_with_store(
        config(with_data.then(|| data.clone())),
        Arc::new(PtyManager::new()),
        Arc::new(EngineRegistry::new()),
        with_data.then(|| db.clone()),
        Some(home.clone()),
        PathGuard::roots(Arc::new(roots)),
        Some(store.clone()),
    );
    Vt {
        _tmp: tmp,
        home,
        store,
        data,
        allowed,
        outside,
        proj,
        db,
        router,
    }
}

async fn vault() -> Vt {
    vault_with(true).await
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

fn mutation(kind: &str, name: &str, scope: &str, path: &Path, target: Option<&Path>) -> Value {
    wire(ClaudeStoreMutation {
        kind: kind.into(),
        name: name.into(),
        scope: scope.into(),
        path: s(path),
        link_target: target.map(s),
    })
}

// ── store list ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn store_list_is_the_desktop_body_over_the_daemon_vault() {
    let v = vault().await;
    let r = &v.router;
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "workspace" }),
    )
    .await;

    let got = ok(r, "claude_store_list", json!({})).await;
    let expect = cs::claude_store_list_in(&v.desktop(), &v.db, None)
        .await
        .unwrap();
    assert_eq!(got, wire(&expect));
    // Shape parity: the wire decodes as the desktop type and round-trips.
    let decoded: Vec<ClaudeStoreEntry> = serde_json::from_value(got.clone()).unwrap();
    assert_eq!(decoded, expect);
    let helper = got
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "helper")
        .unwrap();
    assert_eq!(helper["enabledIn"], json!(["workspace"]));
    assert_eq!(helper["storePath"], s(&v.store.join("agents/helper.md")));
    assert_eq!(helper["description"], "store agent");
    assert_eq!(helper["managed"], true);
    assert_eq!(helper["source"], "local");

    let agents = ok(r, "claude_store_list", json!({ "kind": "agent" })).await;
    assert_eq!(agents.as_array().unwrap().len(), 1);
    let e = err(r, "claude_store_list", json!({ "kind": "widget" })).await;
    assert!(e.contains("kind must be one of"), "{e}");
}

// ── primitives ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn primitive_arms_serve_every_verb() {
    let v = vault().await;
    let r = &v.router;
    let store_agent = v.store.join("agents/helper.md");

    // enable (file kind, workspace): a link into the store.
    let m = ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "workspace" }),
    )
    .await;
    let ws_agent = v.claude().join("agents/helper.md");
    assert_eq!(
        m,
        mutation(
            "agent",
            "helper",
            "workspace",
            &ws_agent,
            Some(&store_agent)
        )
    );
    assert!(is_link(&ws_agent));
    assert_eq!(ws_agent.canonicalize().unwrap(), store_agent);
    // Shape parity with the desktop type.
    let _: ClaudeStoreMutation = serde_json::from_value(m).unwrap();

    // enable (skill, project scope by DB id).
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "skill", "name": "tidy", "scope": "project:proj" }),
    )
    .await;
    let proj_skill = v.proj_claude().join("skills/tidy");
    assert!(is_link(&proj_skill));

    // disable: only the link goes; the store copy stays.
    ok(
        r,
        "claude_primitive_disable",
        json!({ "kind": "skill", "name": "tidy", "scope": "project:proj" }),
    )
    .await;
    assert!(!present(&proj_skill));
    assert!(v.store.join("skills/tidy/SKILL.md").exists());

    // copy: workspace (a link) → project, a real standalone file. Both spellings.
    let m = ok(
        r,
        "claude_primitive_copy",
        json!({ "kind": "agent", "name": "helper", "fromScope": "workspace", "toScope": "project:proj" }),
    )
    .await;
    let proj_agent = v.proj_claude().join("agents/helper.md");
    assert_eq!(
        m,
        mutation("agent", "helper", "project:proj", &proj_agent, None)
    );
    assert!(!is_link(&proj_agent));
    assert_eq!(read(&proj_agent), AGENT);
    // A real file at the destination is refused unless `overwrite`.
    let e = err(
        r,
        "claude_primitive_copy",
        json!({ "kind": "agent", "name": "helper", "from_scope": "workspace", "to_scope": "project:proj" }),
    )
    .await;
    assert!(
        e.contains("a real file is never overwritten silently"),
        "{e}"
    );
    ok(
        r,
        "claude_primitive_copy",
        json!({
            "kind": "agent", "name": "helper", "from_scope": "workspace",
            "to_scope": "project:proj", "overwrite": true,
        }),
    )
    .await;

    // move: the source link goes once the destination copy lands.
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "command", "name": "hello", "scope": "workspace" }),
    )
    .await;
    let ws_cmd = v.claude().join("commands/hello.md");
    assert!(is_link(&ws_cmd));
    ok(
        r,
        "claude_primitive_move",
        json!({ "kind": "command", "name": "hello", "fromScope": "workspace", "toScope": "project:proj" }),
    )
    .await;
    assert!(!present(&ws_cmd));
    assert_eq!(read(&v.proj_claude().join("commands/hello.md")), COMMAND);

    // remove: the scope-local real file goes; the store copy stays.
    ok(
        r,
        "claude_primitive_remove",
        json!({ "kind": "agent", "name": "helper", "scope": "project:proj" }),
    )
    .await;
    assert!(!present(&proj_agent));
    assert!(store_agent.exists());

    // Per engine, both spellings of `hookFile`.
    let m = ok(
        r,
        "claude_primitive_enable_for",
        json!({ "engine": "codex", "kind": "skill", "name": "tidy", "scope": "workspace", "hookFile": null }),
    )
    .await;
    let codex_skill = v.home.join(".agents/skills/tidy");
    assert_eq!(m["path"], s(&codex_skill));
    assert!(is_link(&codex_skill));
    ok(
        r,
        "claude_primitive_disable_for",
        json!({ "engine": "codex", "kind": "skill", "name": "tidy", "scope": "workspace", "hook_file": null }),
    )
    .await;
    assert!(!present(&codex_skill));
    ok(
        r,
        "claude_primitive_enable_for",
        json!({ "engine": "claude", "kind": "agent", "name": "helper", "scope": "project:proj" }),
    )
    .await;
    assert!(is_link(&proj_agent));
    ok(
        r,
        "claude_primitive_remove_for",
        json!({ "engine": "claude", "kind": "agent", "name": "helper", "scope": "project:proj" }),
    )
    .await;
    assert!(!present(&proj_agent));
    let e = err(
        r,
        "claude_primitive_enable_for",
        json!({ "engine": "vim", "kind": "agent", "name": "helper", "scope": "workspace" }),
    )
    .await;
    assert!(e.contains("engine must be"), "{e}");

    // hook / mcp fragments, spliced into the project's settings files.
    let m = ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "hook", "name": "guard", "scope": "project:proj" }),
    )
    .await;
    let settings = v.proj_claude().join("settings.json");
    assert_eq!(m["path"], s(&settings));
    assert!(read(&settings).contains("echo hi"));
    ok(
        r,
        "claude_primitive_disable",
        json!({ "kind": "hook", "name": "guard", "scope": "project:proj" }),
    )
    .await;
    assert!(!read(&settings).contains("echo hi"));
    let m = ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "mcp", "name": "royalti", "scope": "project:proj" }),
    )
    .await;
    let mcp = v.proj.join(".mcp.json");
    assert_eq!(m["path"], s(&mcp));
    let doc: Value = serde_json::from_str(&read(&mcp)).unwrap();
    assert_eq!(doc["mcpServers"]["royalti"]["command"], "npx");
    ok(
        r,
        "claude_primitive_remove",
        json!({ "kind": "mcp", "name": "royalti", "scope": "project:proj" }),
    )
    .await;
    let doc: Value = serde_json::from_str(&read(&mcp)).unwrap();
    assert!(doc["mcpServers"].get("royalti").is_none(), "{doc}");

    // An unknown project id is the desktop's error.
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "project:nope" }),
    )
    .await;
    assert!(e.contains("no project with id"), "{e}");
}

#[tokio::test]
async fn copy_batch_fans_out_and_moves() {
    let v = vault().await;
    let r = &v.router;
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "workspace" }),
    )
    .await;

    let got = ok(
        r,
        "claude_primitive_copy_batch",
        json!({
            "fromEngine": "claude", "kind": "agent", "name": "helper", "fromScope": "workspace",
            "destinations": [
                { "engine": "codex", "scope": "workspace" },
                { "engine": "gemini", "scope": "project:proj", "mode": "same" },
                { "engine": "vim", "scope": "workspace" },
            ],
            "move": false,
        }),
    )
    .await;
    let rows = got["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["engine"], "codex");
    assert_eq!(rows[0]["mode"], "transcode");
    assert_eq!(rows[0]["ok"], true);
    let codex = v.home.join(".codex/agents/helper.toml");
    assert_eq!(rows[0]["mutation"]["path"], s(&codex));
    assert!(read(&codex).contains("You help."));
    assert_eq!(rows[1]["mode"], "same");
    assert_eq!(rows[1]["ok"], true);
    assert_eq!(read(&v.home.join(".gemini/agents/helper.md")), AGENT);
    assert_eq!(rows[2]["ok"], false);
    assert_eq!(rows[2]["mode"], "blocked");
    // Not a move: the source link stays.
    assert!(is_link(&v.claude().join("agents/helper.md")));

    // snake_case, and a move: the source goes after the batch lands.
    let got = ok(
        r,
        "claude_primitive_copy_batch",
        json!({
            "from_engine": "claude", "kind": "agent", "name": "helper", "from_scope": "workspace",
            "destinations": [{ "engine": "claude", "scope": "project:proj" }],
            "move": true,
        }),
    )
    .await;
    assert_eq!(got["rows"][0]["ok"], true, "{got}");
    assert_eq!(read(&v.proj_claude().join("agents/helper.md")), AGENT);
    assert!(!present(&v.claude().join("agents/helper.md")));
}

// ── Ọba registry ────────────────────────────────────────────────────────────

#[tokio::test]
async fn oba_arms_serve_the_registry_verbs() {
    let v = vault().await;
    let r = &v.router;
    let store_agent = v.store.join("agents/helper.md");
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "workspace" }),
    )
    .await;
    let ws_agent = v.claude().join("agents/helper.md");

    // dependents: the desktop body, over the same vault.
    let deps = ok(
        r,
        "oba_dependents",
        json!({ "kind": "agent", "name": "helper" }),
    )
    .await;
    let expect = cs::oba_dependents_in(&v.desktop(), &v.db, "agent".into(), "helper".into())
        .await
        .unwrap();
    assert_eq!(deps, wire(&expect));
    assert_eq!(deps, json!([s(&ws_agent)]));

    // safe delete refuses while a dependent lives …
    let out = ok(
        r,
        "oba_safe_delete",
        json!({ "kind": "agent", "name": "helper" }),
    )
    .await;
    let out: SafeDeleteOutcome = serde_json::from_value(out).unwrap();
    assert_eq!(out.verdict, "refused_dependents");
    assert_eq!(out.dependents, vec![s(&ws_agent)]);
    assert!(store_agent.exists());
    // … unlink the one placement …
    assert_eq!(
        ok(r, "oba_unlink_one", json!({ "path": s(&ws_agent) })).await,
        json!(true)
    );
    assert!(!present(&ws_agent));
    assert_eq!(
        ok(r, "oba_unlink_one", json!({ "path": s(&ws_agent) })).await,
        json!(false)
    );
    // … and the managed master goes.
    let out = ok(
        r,
        "oba_safe_delete",
        json!({ "kind": "agent", "name": "helper" }),
    )
    .await;
    assert_eq!(out["verdict"], "deleted");
    assert!(!store_agent.exists());

    // forget / set_auto_update / missing_requires over registry.json.
    let tidy = v.store.join("skills/tidy");
    let mut with_requires = entry("skill", "tidy", &tidy);
    with_requires.requires = serde_json::from_value(json!([
        { "kind": "command", "name": "hello" },
        { "kind": "skill", "name": "absent" },
    ]))
    .unwrap();
    v.write_registry(vec![
        with_requires,
        entry("command", "hello", &v.store.join("commands/hello.md")),
    ]);
    let missing = ok(
        r,
        "oba_missing_requires",
        json!({ "kind": "skill", "name": "tidy" }),
    )
    .await;
    let missing = missing.as_array().unwrap();
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(missing[0]["kind"], "skill");
    assert_eq!(missing[0]["name"], "absent");
    assert_eq!(
        ok(
            r,
            "oba_set_auto_update",
            json!({ "kind": "skill", "name": "tidy", "enabled": true })
        )
        .await,
        json!(true)
    );
    assert_eq!(v.registry()["entries"][0]["autoUpdate"], true);
    let e = err(
        r,
        "oba_set_auto_update",
        json!({ "kind": "skill", "name": "nope", "enabled": true }),
    )
    .await;
    assert!(e.contains("not in registry"), "{e}");
    assert_eq!(
        ok(r, "oba_forget", json!({ "kind": "skill", "name": "tidy" })).await,
        json!(true)
    );
    assert_eq!(
        ok(r, "oba_forget", json!({ "kind": "skill", "name": "tidy" })).await,
        json!(false)
    );
    assert!(tidy.join("SKILL.md").exists(), "forget touches no files");
}

#[cfg(unix)]
#[tokio::test]
async fn backfill_relink_and_external_masters() {
    let v = vault().await;
    let r = &v.router;
    // An external master kept in place (the `groundwork` shape), placed by a link.
    let ext = v.allowed.join("masters/ext");
    write(&ext.join("SKILL.md"), SKILL);
    let placed = v.claude().join("skills/ext");
    link(&ext, &placed);

    assert_eq!(ok(r, "oba_backfill_registry", json!({})).await, json!(1));
    let reg = v.registry();
    assert_eq!(reg["entries"][0]["name"], "ext");
    assert_eq!(reg["entries"][0]["managed"], false);
    assert_eq!(reg["entries"][0]["canonicalPath"], s(&ext));
    // An external master is refused, not an error, and never touched.
    let out = ok(
        r,
        "oba_safe_delete",
        json!({ "kind": "skill", "name": "ext" }),
    )
    .await;
    assert_eq!(out["verdict"], "refused_external");
    assert!(ext.join("SKILL.md").exists());

    // relink the placement at a new master (inside the allowlist), both spellings.
    let v2 = v.allowed.join("masters/ext-v2");
    write(&v2.join("SKILL.md"), SKILL);
    let rows = ok(
        r,
        "oba_relink_dependents",
        json!({ "dependents": [s(&placed)], "newMaster": s(&v2) }),
    )
    .await;
    let rows: Vec<RelinkRow> = serde_json::from_value(rows).unwrap();
    assert_eq!(
        rows,
        vec![RelinkRow {
            link: s(&placed),
            ok: true,
            error: None
        }]
    );
    assert_eq!(placed.canonicalize().unwrap(), v2);
    let rows = ok(
        r,
        "oba_relink_dependents",
        json!({ "dependents": [s(&placed)], "new_master": s(&ext) }),
    )
    .await;
    assert_eq!(rows[0]["ok"], true);
    assert_eq!(placed.canonicalize().unwrap(), ext);
}

// ── confinement: copies ─────────────────────────────────────────────────────

/// A link planted at a scope path must not turn a copy / move / batch into a
/// read of anything outside the vault: another dir, the daemon's data dir, a
/// system file. Nothing lands at the destination; a move keeps its source.
#[cfg(unix)]
#[tokio::test]
async fn confined_copies_refuse_links_out_of_the_vault() {
    let v = vault().await;
    let r = &v.router;
    let agents = v.claude().join("agents");
    let mut plants = vec![
        (
            "leak",
            v.outside.join("secret.md"),
            "outside the Ngwa vault",
        ),
        ("dd", v.data.join("secret.json"), "daemon's data directory"),
    ];
    if Path::new("/etc/hostname").exists() {
        plants.push((
            "host",
            PathBuf::from("/etc/hostname"),
            "outside the Ngwa vault",
        ));
    }
    for (name, target, why) in &plants {
        let at = agents.join(format!("{name}.md"));
        link(target, &at);
        let dest = v.proj_claude().join(format!("agents/{name}.md"));

        let e = err(
            r,
            "claude_primitive_copy",
            json!({ "kind": "agent", "name": name, "fromScope": "workspace", "toScope": "project:proj" }),
        )
        .await;
        assert!(e.contains(why), "copy {name}: {e}");
        assert!(!present(&dest), "copy {name} wrote");

        let e = err(
            r,
            "claude_primitive_move",
            json!({ "kind": "agent", "name": name, "fromScope": "workspace", "toScope": "project:proj" }),
        )
        .await;
        assert!(e.contains(why), "move {name}: {e}");
        assert!(!present(&dest), "move {name} wrote");
        assert!(is_link(&at), "move {name} lost its source");

        let e = err(
            r,
            "claude_primitive_copy_batch",
            json!({
                "fromEngine": "claude", "kind": "agent", "name": name, "fromScope": "workspace",
                "destinations": [{ "engine": "gemini", "scope": "workspace" }], "move": true,
            }),
        )
        .await;
        assert!(e.contains(why), "batch {name}: {e}");
        assert!(!present(&v.home.join(format!(".gemini/agents/{name}.md"))));
        assert!(is_link(&at), "batch {name} lost its source");
    }

    // A skill dir that is real but holds a link out: the whole copy is refused.
    let sneaky = v.claude().join("skills/sneaky");
    write(&sneaky.join("SKILL.md"), SKILL);
    link(&v.outside.join("secret.md"), &sneaky.join("loot.md"));
    let e = err(
        r,
        "claude_primitive_copy",
        json!({ "kind": "skill", "name": "sneaky", "fromScope": "workspace", "toScope": "project:proj" }),
    )
    .await;
    assert!(e.contains("outside the Ngwa vault"), "{e}");
    assert!(!present(&v.proj_claude().join("skills/sneaky")));

    // A skill placed as a link into the store is inside the vault: copied.
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "skill", "name": "tidy", "scope": "workspace" }),
    )
    .await;
    ok(
        r,
        "claude_primitive_copy",
        json!({ "kind": "skill", "name": "tidy", "fromScope": "workspace", "toScope": "project:proj" }),
    )
    .await;
    assert_eq!(
        read(&v.proj_claude().join("skills/tidy/notes.md")),
        "supporting"
    );
}

/// The desktop reach is unchanged: the same planted links are followed, as the
/// user's own same-uid renderer has always had them followed.
#[cfg(unix)]
#[tokio::test]
async fn desktop_reach_still_follows_symlinks() {
    let v = vault().await;
    let desk = v.desktop();
    link(
        &v.outside.join("secret.md"),
        &v.claude().join("agents/leak.md"),
    );
    cs::claude_primitive_copy_in(
        &desk,
        &v.db,
        "agent".into(),
        "leak".into(),
        "workspace".into(),
        "project:proj".into(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        read(&v.proj_claude().join("agents/leak.md")),
        OUTSIDE_SECRET
    );

    let sneaky = v.claude().join("skills/sneaky");
    write(&sneaky.join("SKILL.md"), SKILL);
    link(&v.outside.join("secret.md"), &sneaky.join("loot.md"));
    cs::claude_primitive_copy_in(
        &desk,
        &v.db,
        "skill".into(),
        "sneaky".into(),
        "workspace".into(),
        "project:proj".into(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        read(&v.proj_claude().join("skills/sneaky/loot.md")),
        OUTSIDE_SECRET
    );

    // Import follows a link inside the source dir, and relink / unlink take any
    // path, as they always have.
    let evil = v.outside.join("evil");
    write(&evil.join("SKILL.md"), SKILL);
    link(&v.outside.join("secret.md"), &evil.join("loot.md"));
    cs::claude_store_import_in(&desk, "skill".into(), "evil".into(), s(&evil))
        .await
        .unwrap();
    assert_eq!(read(&v.store.join("skills/evil/loot.md")), OUTSIDE_SECRET);
    let stray = v.outside.join("stray-link");
    link(&v.outside.join("secret.md"), &stray);
    assert!(cs::oba_unlink_one_in(&desk, None, s(&stray)).await.unwrap());
    assert!(!present(&stray));
    assert_eq!(read(&v.outside.join("secret.md")), OUTSIDE_SECRET);
}

// ── confinement: writes and deletes ─────────────────────────────────────────

/// A scope dir that is itself a link out of the vault is not written through.
#[cfg(unix)]
#[tokio::test]
async fn confined_writes_refuse_a_scope_dir_linked_out() {
    let v = vault().await;
    let r = &v.router;
    let elsewhere = v.outside.join("cmds");
    std::fs::create_dir_all(&elsewhere).unwrap();
    link(&elsewhere, &v.proj_claude().join("commands"));
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "command", "name": "hello", "scope": "project:proj" }),
    )
    .await;
    assert!(e.contains("outside the Ngwa vault"), "{e}");
    assert!(!present(&elsewhere.join("hello.md")));

    link(&v.data, &v.proj_claude().join("agents"));
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "project:proj" }),
    )
    .await;
    assert!(e.contains("daemon's data directory"), "{e}");
    assert!(!present(&v.data.join("helper.md")));

    // The project's `.mcp.json` linked into the data dir is not rewritten.
    link(&v.data.join("secret.json"), &v.proj.join(".mcp.json"));
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "mcp", "name": "royalti", "scope": "project:proj" }),
    )
    .await;
    assert!(e.contains("daemon's data directory"), "{e}");
    assert_eq!(read(&v.data.join("secret.json")), DATA_SECRET);
}

/// Every delete stays `lstat`-first: a link planted where a primitive lives is
/// unlinked, never recursed into, and what it points at survives. A link at a
/// PARENT (the scope's `skills/` dir) is refused rather than deleted through.
#[cfg(unix)]
#[tokio::test]
async fn deletes_unlink_planted_links_and_never_recurse() {
    let v = vault().await;
    let r = &v.router;
    let victim = v.outside.join("victim");
    write(&victim.join("keep.txt"), "keep me");
    let survives = |what: &str| {
        assert_eq!(
            read(&victim.join("keep.txt")),
            "keep me",
            "{what} reached the target"
        )
    };

    let ws_skill = v.claude().join("skills/victim");
    for cmd in ["claude_primitive_disable", "claude_primitive_remove"] {
        link(&victim, &ws_skill);
        ok(
            r,
            cmd,
            json!({ "kind": "skill", "name": "victim", "scope": "workspace" }),
        )
        .await;
        assert!(!present(&ws_skill), "{cmd} left the link");
        survives(cmd);
    }
    let agents_skill = v.home.join(".agents/skills/victim");
    for cmd in [
        "claude_primitive_disable_for",
        "claude_primitive_remove_for",
    ] {
        link(&victim, &agents_skill);
        ok(
            r,
            cmd,
            json!({ "engine": "codex", "kind": "skill", "name": "victim", "scope": "workspace" }),
        )
        .await;
        assert!(!present(&agents_skill), "{cmd} left the link");
        survives(cmd);
    }
    link(&victim, &ws_skill);
    assert_eq!(
        ok(r, "oba_unlink_one", json!({ "path": s(&ws_skill) })).await,
        json!(true)
    );
    survives("oba_unlink_one");
    // A store entry that is a link: safe delete unlinks it, nothing more.
    let store_skill = v.store.join("skills/victim");
    link(&victim, &store_skill);
    let out = ok(
        r,
        "oba_safe_delete",
        json!({ "kind": "skill", "name": "victim" }),
    )
    .await;
    assert_eq!(out["verdict"], "unlinked");
    assert!(!present(&store_skill));
    survives("oba_safe_delete");
    // A real file is never unlinked by path.
    let real = v.claude().join("agents/mine.md");
    write(&real, "mine");
    let e = err(r, "oba_unlink_one", json!({ "path": s(&real) })).await;
    assert!(e.contains("not a symlink"), "{e}");
    assert!(real.exists());

    // The scope's `skills/` dir is a link out: disabling a real dir under it
    // would `remove_dir_all` outside the vault, so it is refused.
    let parent = v.outside.join("skills-parent");
    write(&parent.join("victim/keep.txt"), "keep me too");
    std::fs::remove_dir_all(v.claude().join("skills")).unwrap();
    link(&parent, &v.claude().join("skills"));
    for cmd in ["claude_primitive_disable", "claude_primitive_remove"] {
        let e = err(
            r,
            cmd,
            json!({ "kind": "skill", "name": "victim", "scope": "workspace" }),
        )
        .await;
        assert!(e.contains("outside the Ngwa vault"), "{cmd}: {e}");
        assert_eq!(read(&parent.join("victim/keep.txt")), "keep me too");
    }
}

/// relink / unlink name only placements in a known scope's scan dirs; a new
/// master must pass the fs allowlist; caller paths never expand `$VAR`.
#[cfg(unix)]
#[tokio::test]
async fn relink_and_unlink_name_only_scope_placements() {
    let v = vault().await;
    let r = &v.router;
    let master = v.allowed.join("m");
    write(&master.join("SKILL.md"), SKILL);

    let stray = v.outside.join("stray");
    link(&master, &stray);
    let in_data = v.data.join("stray");
    link(&master, &in_data);
    let nested = v.claude().join("skills/deep/er");
    link(&master, &nested);
    for (p, why) in [
        (&stray, "not a placement"),
        (&in_data, "daemon's data directory"),
        (&nested, "not a placement"),
    ] {
        let e = err(r, "oba_unlink_one", json!({ "path": s(p) })).await;
        assert!(e.contains(why), "{}: {e}", p.display());
        assert!(is_link(p));
    }
    for (raw, why) in [
        ("relative/x", "must be absolute"),
        ("$HOME/x", "must be absolute"),
        ("/a/../b", "`..`"),
    ] {
        let e = err(r, "oba_unlink_one", json!({ "path": raw })).await;
        assert!(e.contains(why), "{raw}: {e}");
    }

    // relink: per-row refusals; a placement in a scan dir relinks.
    let placed = v.proj_claude().join("skills/m");
    link(&v.store.join("skills/tidy"), &placed);
    let rows = ok(
        r,
        "oba_relink_dependents",
        json!({ "dependents": [s(&stray), s(&in_data), s(&placed)], "newMaster": s(&master) }),
    )
    .await;
    assert_eq!(rows[0]["ok"], false);
    assert!(rows[0]["error"]
        .as_str()
        .unwrap()
        .contains("not a placement"));
    assert_eq!(rows[1]["ok"], false);
    assert!(rows[1]["error"]
        .as_str()
        .unwrap()
        .contains("daemon's data directory"));
    assert_eq!(rows[2]["ok"], true);
    assert_eq!(placed.canonicalize().unwrap(), master);
    assert_eq!(
        stray.canonicalize().unwrap(),
        master,
        "refused row untouched"
    );

    // The new master must pass the fs allowlist and stay out of the data dir.
    for (m, why) in [
        (v.outside.join("secret.md"), "outside allowlist"),
        (v.data.join("secret.json"), "daemon's data directory"),
    ] {
        let e = err(
            r,
            "oba_relink_dependents",
            json!({ "dependents": [s(&placed)], "newMaster": s(&m) }),
        )
        .await;
        assert!(e.contains(why), "{}: {e}", m.display());
    }
    assert_eq!(placed.canonicalize().unwrap(), master);
}

// ── confinement: import ─────────────────────────────────────────────────────

#[tokio::test]
async fn import_is_confined_to_the_allowlist() {
    let v = vault().await;
    let r = &v.router;
    let src = v.allowed.join("import");
    write(&src.join("agent.md"), AGENT);
    write(&src.join("skill/SKILL.md"), SKILL);
    write(&src.join("skill/more.md"), "more");

    let got = ok(
        r,
        "claude_store_import",
        json!({ "kind": "agent", "name": "imported", "sourcePath": s(&src.join("agent.md")) }),
    )
    .await;
    let entry: ClaudeStoreEntry = serde_json::from_value(got).unwrap();
    assert_eq!(entry.store_path, s(&v.store.join("agents/imported.md")));
    assert_eq!(entry.description.as_deref(), Some("store agent"));
    assert_eq!(read(&v.store.join("agents/imported.md")), AGENT);
    ok(
        r,
        "claude_store_import",
        json!({ "kind": "skill", "name": "imskill", "source_path": s(&src.join("skill")) }),
    )
    .await;
    assert_eq!(read(&v.store.join("skills/imskill/more.md")), "more");

    for (source, why) in [
        (v.outside.join("secret.md"), "outside allowlist"),
        (v.data.join("secret.json"), "daemon's data directory"),
        (PathBuf::from("/etc/hostname"), "outside allowlist"),
    ] {
        let e = err(
            r,
            "claude_store_import",
            json!({ "kind": "agent", "name": "x", "sourcePath": s(&source) }),
        )
        .await;
        assert!(e.contains(why), "{}: {e}", source.display());
    }
    assert!(!present(&v.store.join("agents/x.md")));
    let e = err(
        r,
        "claude_store_import",
        json!({ "kind": "agent", "name": "x", "sourcePath": "rel/agent.md" }),
    )
    .await;
    assert!(e.contains("must be absolute"), "{e}");
    let e = err(
        r,
        "claude_store_import",
        json!({ "kind": "hook", "name": "x", "sourcePath": s(&src.join("agent.md")) }),
    )
    .await;
    assert!(e.contains("is JSON-fragment"), "{e}");

    #[cfg(unix)]
    {
        // A file source that is a link out of the allowlist.
        link(&v.outside.join("secret.md"), &src.join("link.md"));
        let e = err(
            r,
            "claude_store_import",
            json!({ "kind": "agent", "name": "x", "sourcePath": s(&src.join("link.md")) }),
        )
        .await;
        assert!(e.contains("outside allowlist"), "{e}");
        assert!(!present(&v.store.join("agents/x.md")));

        // A dir source holding a link that escapes it: refused, not dereferenced.
        let evil = src.join("evil");
        write(&evil.join("SKILL.md"), SKILL);
        link(&v.outside.join("secret.md"), &evil.join("loot.md"));
        let e = err(
            r,
            "claude_store_import",
            json!({ "kind": "skill", "name": "evil", "sourcePath": s(&evil) }),
        )
        .await;
        assert!(e.contains("leads out of the source dir"), "{e}");
        assert!(!present(&v.store.join("skills/evil")));
        // Even one pointing elsewhere inside the allowlist leaves the source.
        std::fs::remove_file(evil.join("loot.md")).unwrap();
        link(&src.join("agent.md"), &evil.join("loot.md"));
        let e = err(
            r,
            "claude_store_import",
            json!({ "kind": "skill", "name": "evil", "sourcePath": s(&evil) }),
        )
        .await;
        assert!(e.contains("leads out of the source dir"), "{e}");

        // A link that stays inside the source dir is copied (as bytes).
        let okdir = src.join("okdir");
        write(&okdir.join("SKILL.md"), SKILL);
        link(&okdir.join("SKILL.md"), &okdir.join("alias.md"));
        ok(
            r,
            "claude_store_import",
            json!({ "kind": "skill", "name": "okdir", "sourcePath": s(&okdir) }),
        )
        .await;
        assert_eq!(read(&v.store.join("skills/okdir/alias.md")), SKILL);
    }
}

// ── argument validation ─────────────────────────────────────────────────────

#[tokio::test]
async fn names_and_scopes_are_validated_before_any_path() {
    let v = vault().await;
    let r = &v.router;
    for bad in ["..", "a/b", ".hidden", "a\\b", ""] {
        let calls = [
            (
                "claude_primitive_enable",
                json!({ "kind": "agent", "name": bad, "scope": "workspace" }),
            ),
            (
                "claude_primitive_disable",
                json!({ "kind": "agent", "name": bad, "scope": "workspace" }),
            ),
            (
                "claude_primitive_remove",
                json!({ "kind": "skill", "name": bad, "scope": "workspace" }),
            ),
            (
                "claude_primitive_copy",
                json!({ "kind": "agent", "name": bad, "fromScope": "workspace", "toScope": "project:proj" }),
            ),
            (
                "claude_primitive_move",
                json!({ "kind": "agent", "name": bad, "fromScope": "workspace", "toScope": "project:proj" }),
            ),
            (
                "claude_primitive_enable_for",
                json!({ "engine": "codex", "kind": "skill", "name": bad, "scope": "workspace" }),
            ),
            (
                "claude_primitive_copy_batch",
                json!({
                    "fromEngine": "claude", "kind": "agent", "name": bad, "fromScope": "workspace",
                    "destinations": [], "move": false,
                }),
            ),
            ("oba_dependents", json!({ "kind": "skill", "name": bad })),
            ("oba_safe_delete", json!({ "kind": "skill", "name": bad })),
            ("oba_forget", json!({ "kind": "skill", "name": bad })),
            (
                "oba_missing_requires",
                json!({ "kind": "skill", "name": bad }),
            ),
            (
                "claude_store_import",
                json!({ "kind": "agent", "name": bad, "sourcePath": s(&v.allowed) }),
            ),
        ];
        for (cmd, args) in calls {
            let e = err(r, cmd, args).await;
            assert!(
                e.contains("name must") || e.contains("invalid name length"),
                "{cmd} {bad:?}: {e}"
            );
        }
    }
    assert!(v.store.join("agents/helper.md").exists());
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "global" }),
    )
    .await;
    assert!(
        e.contains("scope must be 'workspace' or 'project:<id>'"),
        "{e}"
    );
    let e = err(
        r,
        "claude_primitive_enable",
        json!({ "kind": "agent", "name": "helper", "scope": "project:../x" }),
    )
    .await;
    assert!(e.contains("invalid project id"), "{e}");
}

#[tokio::test]
async fn vault_arms_name_what_is_missing() {
    let v = vault_with(false).await;
    let r = &v.router;
    for (cmd, args) in [
        ("claude_store_list", json!({})),
        (
            "claude_primitive_enable",
            json!({ "kind": "agent", "name": "helper", "scope": "workspace" }),
        ),
        (
            "claude_primitive_copy",
            json!({ "kind": "agent", "name": "helper", "fromScope": "workspace", "toScope": "workspace" }),
        ),
        (
            "oba_dependents",
            json!({ "kind": "agent", "name": "helper" }),
        ),
        (
            "oba_safe_delete",
            json!({ "kind": "agent", "name": "helper" }),
        ),
        ("oba_backfill_registry", json!({})),
        (
            "oba_missing_requires",
            json!({ "kind": "agent", "name": "helper" }),
        ),
        (
            "oba_relink_dependents",
            json!({ "dependents": [], "newMaster": s(&v.allowed) }),
        ),
        ("oba_unlink_one", json!({ "path": s(&v.claude()) })),
    ] {
        let e = err(r, cmd, args).await;
        assert!(e.contains("--data-dir"), "{cmd}: {e}");
    }
    assert!(v.store.join("agents/helper.md").exists());
    // Store-only arms need no database.
    assert_eq!(
        ok(
            r,
            "oba_forget",
            json!({ "kind": "agent", "name": "helper" })
        )
        .await,
        json!(false)
    );
}

/// The live repro: `~/.claude/skills/tidy` is the user's own real dir and the
/// store also holds a `tidy` (e.g. after an import). Disable, in both the
/// Claude and the per-engine form, is refused and deletes nothing; remove is
/// still the explicit way to delete it.
#[tokio::test]
async fn disable_refuses_a_real_dir_that_is_not_a_placement() {
    let v = vault().await;
    let r = &v.router;
    let real = v.claude().join("skills/tidy");
    write(&real.join("SKILL.md"), "my own skill");
    write(&real.join("refs/notes.md"), "my notes");

    let e = err(
        r,
        "claude_primitive_disable",
        json!({ "kind": "skill", "name": "tidy", "scope": "workspace" }),
    )
    .await;
    assert!(e.contains("not a vault placement"), "{e}");
    let e = err(
        r,
        "claude_primitive_disable_for",
        json!({ "engine": "claude", "kind": "skill", "name": "tidy", "scope": "workspace" }),
    )
    .await;
    assert!(e.contains("not a vault placement"), "{e}");
    assert_eq!(read(&real.join("SKILL.md")), "my own skill");
    assert_eq!(read(&real.join("refs/notes.md")), "my notes");
    assert_eq!(read(&v.store.join("skills/tidy/SKILL.md")), SKILL, "store untouched");

    // A store link at the same place is still disabled.
    std::fs::remove_dir_all(&real).unwrap();
    ok(
        r,
        "claude_primitive_enable",
        json!({ "kind": "skill", "name": "tidy", "scope": "workspace" }),
    )
    .await;
    assert!(is_link(&real));
    ok(
        r,
        "claude_primitive_disable",
        json!({ "kind": "skill", "name": "tidy", "scope": "workspace" }),
    )
    .await;
    assert!(!present(&real));
    assert_eq!(read(&v.store.join("skills/tidy/SKILL.md")), SKILL);
}
