//! House pattern (see `rpc_local`'s tests): a literal `ServerConfig` → the
//! router → `oneshot` POST `/api/rpc` with the bearer token, everything under
//! a tempdir, never the real `~/.ikenga` or `~/.claude`.
//!
//! The T1 property each arm must hold — a principal acts only as itself and
//! only inside its own allowlist — is checked the way `rpc_local`'s
//! `another_principals_runs_are_not_found` checks it: under T1 each
//! principal IS its own daemon process (its own child, home, `--data-dir`,
//! `ikenga.db`, PTYs), so two routers built over two homes / data dirs stand
//! in for two principals. The process-level half — that the child runs as
//! the principal's uid — is the `t1-root` broker test; here every spawn is
//! shown to run as the serving process's own uid.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::sidecar_spec;
use crate::db::PaDb;
use crate::executor::ExecutorTier;
use crate::pty::PtyManager;
use crate::server::rpc_local::DaemonChi;
use crate::server::rpc_shell::PathGuard;
use crate::server::shared::chi_exec;
use crate::server::{router_for_exec_tests, ServerConfig};

const PKG: &str = "com.test.exec";
const SIDECAR: &str = "pa-com-test-exec-probe";

fn config(data: PathBuf, pkgs: PathBuf) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: 0,
        static_dir: PathBuf::from("no-spa-here"),
        pkgs_dir: Some(pkgs),
        data_dir: Some(data),
        auth_token: Some("tok".into()),
        allowed_origins: vec![],
        idle_timeout_secs: None,
        executor_tier: ExecutorTier::T0,
    }
}

/// One `--pkgs-dir` for the whole test process, written once: writing an
/// executable while other test threads fork can make a concurrent exec of it
/// fail with ETXTBSY (see `chi_exec::tests::stub_claude`).
///
/// * `com.test.exec` — declares the probe sidecar (prints its uid, cwd,
///   argv and stdin) and one setting, `greeting`.
/// * `com.test.escape` — declares a sidecar whose `bin` is a symlink to
///   `/bin/sh`, outside its own directory.
/// * `com.test.other` — declares no sidecar.
#[cfg(unix)]
fn pkgs_fixture() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static PKGS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PKGS.get_or_init(|| {
        let root = std::env::temp_dir()
            .join(format!("ikenga-rpc-exec-pkgs-{}", uuid::Uuid::new_v4()))
            .join("pkgs");
        let exec = root.join(PKG);
        std::fs::create_dir_all(exec.join("bin")).unwrap();
        std::fs::write(
            exec.join("manifest.json"),
            json!({
                "id": PKG, "name": "Exec", "version": "0.1.0", "ikenga_api": "1",
                "sidecars": [{ "name": SIDECAR, "bin": "bin/probe" }],
                "settings": { "schema": [
                    { "key": "greeting", "type": "string", "label": "Greeting", "default": "hi" }
                ]}
            })
            .to_string(),
        )
        .unwrap();
        let probe = exec.join("bin/probe");
        std::fs::write(
            &probe,
            "#!/bin/sh\ninput=$(cat)\nprintf 'uid=%s\\ncwd=%s\\nargs=%s\\nstdin=%s\\n' \"$(id -u)\" \"$(pwd -P)\" \"$*\" \"$input\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let escape = root.join("com.test.escape");
        std::fs::create_dir_all(escape.join("bin")).unwrap();
        std::fs::write(
            escape.join("manifest.json"),
            json!({
                "id": "com.test.escape", "name": "Escape", "version": "0.1.0", "ikenga_api": "1",
                "sidecars": [{ "name": "pa-com-test-escape-sh", "bin": "bin/sh" }]
            })
            .to_string(),
        )
        .unwrap();
        std::os::unix::fs::symlink("/bin/sh", escape.join("bin/sh")).unwrap();

        let other = root.join("com.test.other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("manifest.json"),
            json!({ "id": "com.test.other", "name": "Other", "version": "0.1.0", "ikenga_api": "1" })
                .to_string(),
        )
        .unwrap();
        root.canonicalize().unwrap()
    })
    .clone()
}

/// One principal's daemon: its own home, data dir and PTYs, an allowlist of
/// exactly `allowed`, and `outside` beside it (outside the allowlist).
struct Principal {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    allowed: PathBuf,
    outside: PathBuf,
    db: Arc<PaDb>,
    router: Router,
}

#[cfg(unix)]
fn principal() -> Principal {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (data, home, allowed, outside) = (
        root.join("data"),
        root.join("home"),
        root.join("allowed"),
        root.join("outside"),
    );
    for d in [&data, &home, &allowed, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    let roots_file = root.join("fs_roots.json");
    std::fs::write(
        &roots_file,
        json!({ "roots": [allowed.to_string_lossy()] }).to_string(),
    )
    .unwrap();
    let guard = PathGuard::roots(Arc::new(
        crate::fs_roots::FsRoots::load(roots_file).unwrap(),
    ));
    let db = Arc::new(PaDb::new(data.join("ikenga.db")));
    let chi = Arc::new(DaemonChi {
        runtime: Arc::new(chi_exec::ChiRuntime::new()),
        resolver: Arc::new(chi_exec::tests::StubResolver(chi_exec::tests::stub_claude())),
    });
    let router = router_for_exec_tests(
        config(data, pkgs_fixture()),
        Arc::new(PtyManager::new()),
        Some(db.clone()),
        Some(home.clone()),
        guard,
        chi,
    );
    Principal {
        _tmp: tmp,
        home,
        allowed,
        outside,
        db,
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

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[cfg(unix)]
fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

// ── pkg_sidecar_call ─────────────────────────────────────────────────────────

/// The declared sidecar runs as the serving process's uid, in its own pkg
/// directory, with the caller's argv and stdin.
#[cfg(unix)]
#[tokio::test]
async fn t1_sidecar_runs_as_the_serving_principal_inside_its_pkg() {
    let p = principal();
    let out = ok(
        &p.router,
        "pkg_sidecar_call",
        json!({
            "pkgId": PKG, "name": SIDECAR, "args": ["repo", "snapshot"],
            "stdin": "{\"method\":\"repo.snapshot\"}", "timeoutSecs": 10,
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["timed_out"], false);
    assert_eq!(out["exit_code"], 0);
    let stdout = out["stdout"].as_str().unwrap();
    assert!(stdout.contains(&format!("uid={}\n", euid())), "{stdout}");
    let pkg_dir = pkgs_fixture().join(PKG);
    assert!(
        stdout.contains(&format!("cwd={}\n", pkg_dir.display())),
        "{stdout}"
    );
    assert!(stdout.contains("args=repo snapshot\n"), "{stdout}");
    assert!(
        stdout.contains("stdin={\"method\":\"repo.snapshot\"}\n"),
        "{stdout}"
    );
}

/// Only a sidecar the named pkg itself declares, whose binary stays inside
/// that pkg's directory, ever runs — each refusal an `ok: false` naming why
/// (the desktop's shape), never a spawn.
#[cfg(unix)]
#[tokio::test]
async fn t1_sidecar_refuses_other_pkgs_and_paths_outside_the_pkg() {
    let p = principal();
    let r = &p.router;
    let call = |pkg: &str, name: &str| json!({ "pkgId": pkg, "name": name, "args": [] });

    let unknown = ok(r, "pkg_sidecar_call", call("com.test.nope", SIDECAR)).await;
    assert_eq!(unknown["ok"], false);
    assert_eq!(
        unknown["error"],
        "pkg `com.test.nope` is not installed on this server"
    );

    // Another pkg's sidecar, claimed by a pkg that declares none.
    let foreign = ok(r, "pkg_sidecar_call", call("com.test.other", SIDECAR)).await;
    assert_eq!(foreign["ok"], false);
    assert_eq!(
        foreign["error"],
        format!("pkg `com.test.other` declares no sidecar `{SIDECAR}`")
    );

    // A `bin` symlinked out of the pkg's directory.
    let escape = ok(
        r,
        "pkg_sidecar_call",
        call("com.test.escape", "pa-com-test-escape-sh"),
    )
    .await;
    assert_eq!(escape["ok"], false);
    let e = escape["error"].as_str().unwrap();
    assert!(e.contains("escapes install dir"), "{e}");
    assert_eq!(escape["stdout"], Value::Null, "nothing ran");

    // A malformed argument is an RPC error, as a Tauri arg decode is.
    let bad = err(
        r,
        "pkg_sidecar_call",
        json!({ "pkgId": PKG, "name": SIDECAR, "args": "x" }),
    )
    .await;
    assert_eq!(bad, "pkg_sidecar_call: `args` must be an array of strings");
}

/// The child's environment is this process's minus the host-only secrets
/// (explicitly: cleared first, then re-filled), so no daemon bearer, vault
/// key or `IKENGA_SECRET_*` default reaches pkg code.
#[test]
fn sidecar_spec_scrubs_the_host_only_environment() {
    let spec = sidecar_spec(Path::new("/x/bin/probe"), Path::new("/x"), &["a".into()]);
    assert!(spec.env.clear, "the inherited env must be cleared first");
    for (k, _) in &spec.env.vars {
        assert!(
            !crate::pty::is_host_only_env(&k.to_string_lossy()),
            "{k:?} leaked into a sidecar"
        );
    }
    assert_eq!(spec.cwd.as_deref(), Some(Path::new("/x")));
    assert_eq!(
        spec.args.last().map(|a| a.to_string_lossy().into_owned()),
        Some("a".into())
    );
}

// ── action_exec ──────────────────────────────────────────────────────────────

/// A personal `shell` action whose cwd is `{{project.root}}`, written through
/// the served `actions_write` (the validated path), and its run hash.
#[cfg(unix)]
async fn write_personal_pwd(p: &Principal) -> String {
    let run = json!({ "kind": "shell", "command": "pwd -P", "cwd": "{{project.root}}" });
    let doc = json!({ "version": 1, "actions": [
        { "id": "where", "name": "Where", "scope": "personal", "run": run }
    ]});
    let w = ok(
        &p.router,
        "actions_write",
        json!({ "scope": "personal", "document": doc }),
    )
    .await;
    assert_eq!(w["written"], true, "{w}");
    crate::server::shared::actions::trust::run_hash(&run)
}

fn exec_request(hash: &str, root: &Path) -> Value {
    json!({ "request": {
        "scope": "personal", "actionId": "where", "runHash": hash,
        "variables": { "project.root": s(root) }, "timeoutSecs": 10,
    }})
}

/// The pinned run executes as this process in a directory inside the
/// allowlist; a cwd outside it is refused `invalid-cwd` before anything
/// spawns; another principal (another home) has no such action at all.
#[cfg(unix)]
#[tokio::test]
async fn t1_action_exec_runs_only_own_actions_inside_the_allowlist() {
    let ada = principal();
    let hash = write_personal_pwd(&ada).await;

    let ran = ok(
        &ada.router,
        "action_exec",
        exec_request(&hash, &ada.allowed),
    )
    .await;
    assert_eq!(ran["ok"], true, "{ran}");
    assert_eq!(ran["stdout"].as_str().unwrap().trim(), s(&ada.allowed));
    assert_eq!(ran["cwd"], s(&ada.allowed));
    assert_eq!(ran["refusal"], Value::Null);

    let refused = ok(
        &ada.router,
        "action_exec",
        exec_request(&hash, &ada.outside),
    )
    .await;
    assert_eq!(refused["refusal"], "invalid-cwd", "{refused}");
    assert_eq!(refused["exitCode"], Value::Null, "nothing spawned");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("outside allowlist"),
        "{refused}"
    );

    // Bob's daemon reads Bob's home: Ada's action id names nothing there.
    let bob = principal();
    let foreign = ok(
        &bob.router,
        "action_exec",
        exec_request(&hash, &bob.allowed),
    )
    .await;
    assert_eq!(foreign["refusal"], "not-found", "{foreign}");

    // A run hash that is not the pinned run's is never executed.
    let changed = ok(
        &ada.router,
        "action_exec",
        exec_request("0000", &ada.allowed),
    )
    .await;
    assert_eq!(changed["refusal"], "changed", "{changed}");
}

// ── comment_route ────────────────────────────────────────────────────────────

async fn pin(db: &PaDb, artifact: &Path) -> i64 {
    crate::server::shared::comments::create(
        db,
        s(artifact),
        "#hero".into(),
        "make it bigger".into(),
        None,
        None,
        None,
    )
    .await
    .unwrap()
    .id
}

/// Auto-detect with no claude PTY in THIS process is the clipboard (Ada's
/// PTY ids name nothing in Bob's process); a comment id from another
/// principal's db is not found; the chi sink refuses an artifact directory
/// outside the allowlist and runs one inside it, as this process.
#[cfg(unix)]
#[tokio::test]
async fn t1_comment_route_stays_in_the_principals_ptys_db_and_allowlist() {
    let ada = principal();
    let inside = ada.allowed.join("page.html");
    std::fs::write(&inside, "<h1>x</h1>").unwrap();
    let id = pin(&ada.db, &inside).await;

    let routed = ok(
        &ada.router,
        "comment_route",
        json!({ "id": id, "preferredPtyId": "someone-elses-pty" }),
    )
    .await;
    assert_eq!(routed["sink"], "clipboard", "{routed}");
    assert_eq!(routed["pty_id"], Value::Null);
    assert!(routed["clipboard_text"]
        .as_str()
        .unwrap()
        .contains("make it bigger"));
    // `clipboard` / `chi` are not values the audit column admits; the row is
    // left untouched rather than failing a delivered prompt.
    assert_eq!(routed["comment"]["sink"], Value::Null);

    // A forced terminal with no claude PTY degrades, and is audited as such.
    let forced = ok(
        &ada.router,
        "comment_route",
        json!({ "id": id, "overrideSink": "terminal" }),
    )
    .await;
    assert_eq!(forced["sink"], "clipboard", "{forced}");

    // Bob's db has no comment `id`.
    let bob = principal();
    assert_eq!(
        err(&bob.router, "comment_route", json!({ "id": id })).await,
        format!("comment_route: comment {id} not found")
    );

    // The chi sink: outside the allowlist is refused, nothing is started.
    let outside = ada.outside.join("page.html");
    std::fs::write(&outside, "<h1>y</h1>").unwrap();
    let out_id = pin(&ada.db, &outside).await;
    let e = err(
        &ada.router,
        "comment_route",
        json!({ "id": out_id, "overrideSink": "chi" }),
    )
    .await;
    assert!(e.contains("outside allowlist"), "{e}");
    let runs = ok(
        &ada.router,
        "chi_list",
        json!({ "engineId": "claude-code" }),
    )
    .await;
    assert_eq!(runs, json!([]), "no run may start for a refused pin");

    // Inside it, a run starts as this process, in the artifact's directory.
    let chi = ok(
        &ada.router,
        "comment_route",
        json!({ "id": id, "overrideSink": "chi" }),
    )
    .await;
    assert_eq!(chi["sink"], "chi", "{chi}");
    let run_id = chi["run_id"].as_str().unwrap().to_string();
    assert!(
        chi_exec::tests::wait_for(|| async {
            ok(&ada.router, "chi_status", json!({ "runId": run_id })).await["status"] == "done"
        })
        .await
    );
    let out = ok(&ada.router, "chi_status", json!({ "runId": run_id })).await["output"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(out.contains(&format!("uid={} ", euid())), "{out}");
    assert!(
        out.contains(&format!("cwd={} ", ada.allowed.display())),
        "{out}"
    );
    // Bob cannot read Ada's run either.
    assert_eq!(
        err(&bob.router, "chi_status", json!({ "runId": run_id })).await,
        format!("chi_status: chi run not found: {run_id}")
    );
}

// ── agent_ops_run_now ────────────────────────────────────────────────────────

fn write_jobs(home: &Path, ids: &[&str]) {
    let dir = home.join(".atelier/skill-agent-ops");
    std::fs::create_dir_all(&dir).unwrap();
    let jobs: Vec<Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "label": id, "schedule": "0 * * * *", "command": "true" }))
        .collect();
    std::fs::write(dir.join("jobs.json"), Value::Array(jobs).to_string()).unwrap();
}

fn write_lock(home: &Path, pid: u32) {
    let dir = home.join(".agent-ops");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("daemon.lock"),
        json!({ "pid": pid, "port": 9, "secret": "s3cret" }).to_string(),
    )
    .unwrap();
}

/// Only a job in the principal's own `jobs.json` is triggered — another
/// principal's job id is `not_found` with no request made — and, on Linux,
/// only through an agent-ops daemon running as this uid.
#[cfg(unix)]
#[tokio::test]
async fn t1_run_now_triggers_only_own_jobs_through_own_daemon() {
    let ada = principal();
    let bob = principal();
    write_jobs(&ada.home, &["ada:nightly"]);
    write_jobs(&bob.home, &["bob:hourly"]);

    let foreign = ok(
        &bob.router,
        "agent_ops_run_now",
        json!({ "jobId": "ada:nightly" }),
    )
    .await;
    assert_eq!(foreign["ok"], false);
    assert_eq!(foreign["code"], "not_found", "{foreign}");
    assert_eq!(
        foreign["error"],
        "`ada:nightly` is not one of your agent-ops jobs"
    );

    // No lock: the job is Ada's, so it reaches `run_now`, which says the
    // daemon is down (the lock is missing) without touching the network.
    let down = ok(
        &ada.router,
        "agent_ops_run_now",
        json!({ "jobId": "ada:nightly" }),
    )
    .await;
    assert_eq!(down["code"], "daemon_down", "{down}");

    // A lock naming a process that is not this uid's (pid 1 is init, root).
    #[cfg(target_os = "linux")]
    if euid() != 0 {
        write_lock(&ada.home, 1);
        let refused = ok(
            &ada.router,
            "agent_ops_run_now",
            json!({ "jobId": "ada:nightly" }),
        )
        .await;
        assert_eq!(refused["code"], "forbidden", "{refused}");
        assert!(
            refused["error"].as_str().unwrap().contains("not as you"),
            "{refused}"
        );
    }
}

// ── pkg_settings_set ─────────────────────────────────────────────────────────

/// A write lands in this principal's own `ikenga.db` (Bob still sees the
/// manifest default), only for a declared key of an indexed pkg, and the
/// owning `pkg_installed` row is recorded disabled, never over a real one.
#[cfg(unix)]
#[tokio::test]
async fn t1_pkg_settings_set_is_principal_scoped_and_declared_only() {
    let ada = principal();
    let bob = principal();
    let set = |key: &str, value: Value| json!({ "pkgId": PKG, "key": key, "value": value });

    assert_eq!(
        ok(
            &ada.router,
            "pkg_settings_set",
            set("greeting", json!("hello"))
        )
        .await,
        Value::Null
    );
    let got = ok(&ada.router, "pkg_settings_get", json!({ "pkgId": PKG })).await;
    assert_eq!(got["values"]["greeting"], "hello", "{got}");
    // Twice: the owner row already exists, the upsert still lands.
    ok(
        &ada.router,
        "pkg_settings_set",
        set("greeting", json!("again")),
    )
    .await;
    let got = ok(&ada.router, "pkg_settings_get", json!({ "pkgId": PKG })).await;
    assert_eq!(got["values"]["greeting"], "again");

    let bobs = ok(&bob.router, "pkg_settings_get", json!({ "pkgId": PKG })).await;
    assert_eq!(bobs["values"]["greeting"], "hi", "Ada's write is not Bob's");

    let pool = ada.db.ensure_pool().await.unwrap();
    let (enabled, path): (i64, String) =
        sqlx::query_as("SELECT enabled, install_path FROM pkg_installed WHERE id = ?")
            .bind(PKG)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(enabled, 0, "the owner row never enables the pkg anywhere");
    assert_eq!(path, s(&pkgs_fixture().join(PKG)));

    assert_eq!(
        err(&ada.router, "pkg_settings_set", set("nope", json!(1))).await,
        format!("pkg_settings_set: pkg `{PKG}` declares no setting `nope`")
    );
    assert_eq!(
        err(
            &ada.router,
            "pkg_settings_set",
            json!({ "pkgId": "com.test.ghost", "key": "greeting", "value": 1 })
        )
        .await,
        "pkg_settings_set: pkg `com.test.ghost` is not installed on this server"
    );
    assert_eq!(
        err(
            &ada.router,
            "pkg_settings_set",
            json!({ "pkgId": PKG, "key": "greeting" })
        )
        .await,
        "pkg_settings_set: `value` is required"
    );
}
