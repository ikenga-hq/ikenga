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
use std::time::Duration;

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

        let full = root.join("com.ikenga.full");
        std::fs::create_dir_all(full.join("bin")).unwrap();
        std::fs::write(
            full.join("manifest.json"),
            json!({
                "id": "com.ikenga.full", "name": "Full", "version": "0.1.0", "ikenga_api": "1",
                "capabilities": {
                    "invoke": {
                        "commands": ["echo", "echo *"]
                    }
                },
                "mcp": [
                    { "name": "full-mcp", "command": "bin/mcp" }
                ],
                "permissions": {
                    "shell.execute": ["bin/mcp", "echo"]
                }
            })
            .to_string(),
        )
        .unwrap();
        let mcp_bin = full.join("bin/mcp");
        std::fs::write(
            &mcp_bin,
            "#!/bin/sh\nwhile IFS= read -r line; do\n  id=$(echo \"$line\" | grep -o '\"id\":[0-9]*' | cut -d: -f2)\n  if [ -n \"$id\" ]; then\n    echo \"{\\\"jsonrpc\\\":\\\"2.0\\\",\\\"id\\\":$id,\\\"result\\\":{\\\"ok\\\":true}}\"\n  fi\ndone\n",
        )
        .unwrap();
        std::fs::set_permissions(&mcp_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let empty_mcp = root.join("com.test.empty-mcp");
        std::fs::create_dir_all(empty_mcp.join("bin")).unwrap();
        std::fs::write(
            empty_mcp.join("manifest.json"),
            json!({
                "id": "com.test.empty-mcp", "name": "EmptyMcp", "version": "0.1.0", "ikenga_api": "1",
                "mcp": [{ "name": "srv", "command": "bin/mcp" }],
                "permissions": { "shell.execute": [] }
            })
            .to_string(),
        )
        .unwrap();
        let empty_bin = empty_mcp.join("bin/mcp");
        std::fs::write(&empty_bin, "#!/bin/sh\necho ok\n").unwrap();
        std::fs::set_permissions(&empty_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let sensitive = root.join("com.test.sensitive");
        std::fs::create_dir_all(sensitive.join("bin")).unwrap();
        std::fs::write(
            sensitive.join("manifest.json"),
            json!({
                "id": "com.test.sensitive", "name": "Sensitive", "version": "0.1.0", "ikenga_api": "1",
                "capabilities": { "invoke": { "commands": ["echo"] } },
                "permissions": { "shell.execute": ["echo"] }
            })
            .to_string(),
        )
        .unwrap();

        let sidecar_pkg = root.join("com.test.sidecar");
        std::fs::create_dir_all(sidecar_pkg.join("bin")).unwrap();
        std::fs::write(
            sidecar_pkg.join("manifest.json"),
            json!({
                "id": "com.test.sidecar", "name": "Sidecar", "version": "0.1.0", "ikenga_api": "1",
                "sidecars": [{ "name": "pa-com-test-sidecar-loop", "bin": "bin/loop" }]
            })
            .to_string(),
        )
        .unwrap();
        let loop_bin = sidecar_pkg.join("bin/loop");
        std::fs::write(&loop_bin, "#!/bin/sh\necho $$ > sidecar.pid\nwhile true; do sleep 1; done\n").unwrap();
        std::fs::set_permissions(&loop_bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The fixture lives in a process-wide `OnceLock`, which never drops, so
        // remove its directory when the test process exits.
        extern "C" fn remove_pkgs_fixture() {
            if let Some(parent) = PKGS.get().and_then(|root| root.parent()) {
                let _ = std::fs::remove_dir_all(parent);
            }
        }
        // SAFETY: `remove_pkgs_fixture` is a plain `extern "C" fn` that only
        // touches the filesystem.
        unsafe {
            libc::atexit(remove_pkgs_fixture);
        }

        root.canonicalize().unwrap()
    })
    .clone()
}

/// Kills the `com.test.sidecar` loop (and anything it spawned) and removes the
/// pid file it wrote, however the test ends: an assertion failing before the
/// test's own shutdown would otherwise leave `bin/loop` running for good.
#[cfg(unix)]
struct SidecarLoopGuard {
    pid_file: PathBuf,
}

#[cfg(unix)]
impl Drop for SidecarLoopGuard {
    fn drop(&mut self) {
        if let Some(pid) = std::fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|c| c.trim().parse::<libc::pid_t>().ok())
            .filter(|pid| *pid > 1)
        {
            // The sidecar is its own process-group leader (`process_group(0)`).
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_file(&self.pid_file);
    }
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
    // `clipboard` / `chi` are recorded in the audit column (WP-S 0071).
    assert_eq!(routed["comment"]["sink"], "clipboard");

    // A forced terminal with no claude PTY degrades, and is audited as such.
    let forced = ok(
        &ada.router,
        "comment_route",
        json!({ "id": id, "overrideSink": "terminal" }),
    )
    .await;
    assert_eq!(forced["sink"], "clipboard", "{forced}");
    assert_eq!(forced["comment"]["sink"], "clipboard");

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
    assert_eq!(chi["comment"]["sink"], "chi");
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

#[cfg(unix)]
#[tokio::test]
async fn t1_streaming_sidecar_rpc_send_and_shutdown() {
    let p = principal();
    // 1. Send to live sidecar
    let res = ok(
        &p.router,
        "pkg_sidecar_rpc_send",
        json!({
            "pkgId": PKG,
            "name": SIDECAR,
            "message": "hello stream"
        }),
    )
    .await;
    assert_eq!(res, Value::Null);

    // 2. Shut down the streaming sidecar
    let res = ok(
        &p.router,
        "pkg_sidecar_rpc_shutdown",
        json!({
            "pkgId": PKG,
            "name": SIDECAR,
        }),
    )
    .await;
    assert_eq!(res, json!(true));

    // Shut down again is idempotent (returns false)
    let res2 = ok(
        &p.router,
        "pkg_sidecar_rpc_shutdown",
        json!({
            "pkgId": PKG,
            "name": SIDECAR,
        }),
    )
    .await;
    assert_eq!(res2, json!(false));

    // 3. Unknown pkg or sidecar is refused
    let e = err(
        &p.router,
        "pkg_sidecar_rpc_send",
        json!({
            "pkgId": "com.test.nope",
            "name": SIDECAR,
            "message": "ping"
        }),
    )
    .await;
    assert!(e.contains("not installed"), "{e}");
}

#[cfg(unix)]
#[tokio::test]
async fn t1_pkg_invoke_and_mcp_call() {
    let p = principal();

    // 1. pkg_invoke: permitted command succeeds
    let out = ok(
        &p.router,
        "pkg_invoke",
        json!({
            "pkgId": "com.ikenga.full",
            "command": "echo",
            "args": ["daemon-test"],
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["exitCode"], 0);
    assert_eq!(out["stdout"].as_str().unwrap().trim(), "daemon-test");

    // 2. pkg_invoke: command outside invoke allowlist is refused
    let out = ok(
        &p.router,
        "pkg_invoke",
        json!({
            "pkgId": "com.ikenga.full",
            "command": "sh",
            "args": ["-c", "id"],
        }),
    )
    .await;
    assert_eq!(out["ok"], false, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(err.contains("not permitted to invoke `sh`"), "{err}");

    // 3. pkg_mcp_call: succeeds against mock MCP server
    let out = ok(
        &p.router,
        "pkg_mcp_call",
        json!({
            "pkgId": "com.ikenga.full",
            "tool": "probe",
            "args": {},
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["ok"], true);

    // 4. pkg_mcp_call: pkg without MCP block returns ok: false
    let out = ok(
        &p.router,
        "pkg_mcp_call",
        json!({
            "pkgId": PKG,
            "tool": "probe",
        }),
    )
    .await;
    assert_eq!(out["ok"], false, "{out}");
    assert!(out["error"]
        .as_str()
        .unwrap()
        .contains("declares no mcp servers"));
}

#[cfg(unix)]
fn principal_child_harness() -> (Principal, crate::executor::PrincipalId, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let principal_id = crate::executor::PrincipalId::new_v7();
    let (data, home, allowed, outside) = (
        root.join(format!("principals/{principal_id}/data")),
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
    let access = crate::access::DaemonAccess::principal_child(Default::default());
    let router = crate::server::router_for_exec_tests_with_access(
        config(data, pkgs_fixture()),
        Arc::new(PtyManager::new()),
        Some(db.clone()),
        Some(home.clone()),
        guard,
        chi,
        access,
    );
    (
        Principal {
            _tmp: tmp,
            home,
            allowed,
            outside,
            db,
            router,
        },
        principal_id,
        root,
    )
}

async fn rpc_as_child(
    router: &Router,
    principal_id: crate::executor::PrincipalId,
    is_admin: bool,
    cmd: &str,
    args: Value,
) -> Value {
    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri("/api/rpc")
        .header("authorization", "Bearer tok")
        .header("content-type", "application/json")
        .header("x-ikenga-principal", principal_id.to_string())
        .header(
            "x-ikenga-caps",
            "files,sessions,dispatch,approve,install,settings,secrets",
        );
    if is_admin {
        builder = builder.header("x-ikenga-admin", "1");
    }
    let res = router
        .clone()
        .oneshot(
            builder
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

#[cfg(unix)]
#[tokio::test]
async fn arm_level_unassigned_refused_and_admin_allowed() {
    let (p, pid, root) = principal_child_harness();
    // Configure pkg-assignments.json to NOT assign PKG
    std::fs::write(
        root.join("pkg-assignments.json"),
        json!({ PKG: ["other-user-uuid"] }).to_string(),
    )
    .unwrap();

    // 1. pkg_sidecar_call: unassigned member is refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_sidecar_call",
        json!({ "pkgId": PKG, "name": SIDECAR }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("not assigned"),
        "{res}"
    );

    // 2. pkg_mcp_call: unassigned member is refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_mcp_call",
        json!({ "pkgId": PKG, "tool": "probe" }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("not assigned"),
        "{res}"
    );

    // 3. pkg_invoke: unassigned member is refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_invoke",
        json!({ "pkgId": PKG, "command": "echo" }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("not assigned"),
        "{res}"
    );

    // 4. pkg_sidecar_rpc_send: unassigned member is refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_sidecar_rpc_send",
        json!({ "pkgId": PKG, "name": SIDECAR, "message": "hi" }),
    )
    .await;
    assert_eq!(res["ok"], false);
    assert!(
        res["error"].as_str().unwrap().contains("not assigned"),
        "{res}"
    );

    // 5. Admin caller succeeds through assignment gate
    let res_admin = rpc_as_child(
        &p.router,
        pid,
        true,
        "pkg_sidecar_call",
        json!({ "pkgId": PKG, "name": SIDECAR }),
    )
    .await;
    assert_eq!(res_admin["ok"], true);
    assert_eq!(res_admin["data"]["ok"], true, "{res_admin}");
}

#[cfg(unix)]
#[tokio::test]
async fn arm_level_unapproved_sensitive_pkg_refused() {
    let (p, pid, root) = principal_child_harness();
    // Assign com.test.sensitive to everyone, but do not approve in pkg-trust.json
    std::fs::write(
        root.join("pkg-assignments.json"),
        json!({ "com.test.sensitive": ["*"] }).to_string(),
    )
    .unwrap();

    // 1. pkg_invoke: unapproved sensitive pkg refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_invoke",
        json!({ "pkgId": "com.test.sensitive", "command": "echo", "args": ["hi"] }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("host.invoke requires a trusted pkg"),
        "{res}"
    );

    // 2. pkg_sidecar_rpc_send: unapproved sensitive pkg refused
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_sidecar_rpc_send",
        json!({ "pkgId": "com.test.sensitive", "name": "srv", "message": "hi" }),
    )
    .await;
    assert_eq!(res["ok"], false);
    assert!(
        res["error"]
            .as_str()
            .unwrap()
            .contains("requires admin trust approval"),
        "{res}"
    );

    // 3. Now approve with version and snapshot hash in pkg-trust.json
    let perms = crate::pkg::manifest::Permissions {
        shell_execute: vec!["echo".into()],
        ..Default::default()
    };
    let hash = crate::pkg::trust_state::snapshot_hash(
        &crate::pkg::trust_state::summarize_sensitive(&perms),
    );
    std::fs::write(
        root.join("pkg-trust.json"),
        json!({
            "com.test.sensitive": {
                "version": "0.1.0",
                "snapshot_hash": hash,
                "trust_state": "granted"
            }
        })
        .to_string(),
    )
    .unwrap();

    let res_ok = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_invoke",
        json!({ "pkgId": "com.test.sensitive", "command": "echo", "args": ["hi"] }),
    )
    .await;
    assert_eq!(res_ok["ok"], true);
    assert_eq!(res_ok["data"]["ok"], true, "{res_ok}");
}

#[cfg(unix)]
#[tokio::test]
async fn arm_level_spoofed_ikenga_prefix_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let principal_id = crate::executor::PrincipalId::new_v7();
    let (data, home, allowed, outside) = (
        root.join(format!("principals/{principal_id}/data")),
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

    // A com.ikenga.* pkg whose files live OUTSIDE the configured pkgs dir and
    // are only reachable through a symlink inside it. It loads, but it is not
    // "loaded from --pkgs-dir", so it must not be auto-trusted.
    let sideloaded_pkgs = root.join("sideloaded");
    let external = root.join("external-elsewhere");
    let spoof_pkg = external.join("com.ikenga.spoofed");
    std::fs::create_dir_all(spoof_pkg.join("bin")).unwrap();
    std::fs::create_dir_all(&sideloaded_pkgs).unwrap();
    std::os::unix::fs::symlink(&spoof_pkg, sideloaded_pkgs.join("com.ikenga.spoofed")).unwrap();
    std::fs::write(
        spoof_pkg.join("manifest.json"),
        json!({
            "id": "com.ikenga.spoofed", "name": "Spoofed", "version": "0.1.0", "ikenga_api": "1",
            "capabilities": { "invoke": { "commands": ["echo"] } },
            "permissions": { "shell.execute": ["echo"] }
        })
        .to_string(),
    )
    .unwrap();

    let access = crate::access::DaemonAccess::principal_child(Default::default());
    let router = crate::server::router_for_exec_tests_with_access(
        config(data, sideloaded_pkgs),
        Arc::new(PtyManager::new()),
        Some(db.clone()),
        Some(home.clone()),
        guard,
        chi,
        access,
    );

    // Call pkg_invoke: must be REFUSED because sideloaded com.ikenga.* is not auto-trusted
    let res = rpc_as_child(
        &router,
        principal_id,
        false,
        "pkg_invoke",
        json!({ "pkgId": "com.ikenga.spoofed", "command": "echo", "args": ["evil"] }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("host.invoke requires a trusted pkg"),
        "{res}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn arm_level_empty_shell_execute_refused() {
    let (p, pid, root) = principal_child_harness();
    std::fs::write(
        root.join("pkg-assignments.json"),
        json!({ "com.test.empty-mcp": ["*"] }).to_string(),
    )
    .unwrap();
    let res = rpc_as_child(
        &p.router,
        pid,
        false,
        "pkg_mcp_call",
        json!({ "pkgId": "com.test.empty-mcp", "tool": "anything" }),
    )
    .await;
    assert_eq!(res["ok"], true);
    assert_eq!(res["data"]["ok"], false);
    assert!(
        res["data"]["error"]
            .as_str()
            .unwrap()
            .contains("shell.execute denied"),
        "{res}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn arm_level_cross_account_sidecar_isolation_and_process_killed() {
    let (p, pid_a, root) = principal_child_harness();
    let pid_b = crate::executor::PrincipalId::new_v7();

    let sidecar_dir = pkgs_fixture().join("com.test.sidecar");
    let pid_file = sidecar_dir.join("sidecar.pid");
    let _ = std::fs::remove_file(&pid_file);
    let _cleanup = SidecarLoopGuard {
        pid_file: pid_file.clone(),
    };

    // Assign com.test.sidecar to everyone
    std::fs::write(
        root.join("pkg-assignments.json"),
        json!({ "com.test.sidecar": ["*"] }).to_string(),
    )
    .unwrap();

    // Account A starts a streaming sidecar
    let res_a = rpc_as_child(
        &p.router,
        pid_a,
        false,
        "pkg_sidecar_rpc_send",
        json!({ "pkgId": "com.test.sidecar", "name": "pa-com-test-sidecar-loop", "message": "ping" }),
    )
    .await;
    assert_eq!(res_a["ok"], true, "{res_a}");

    // Wait for the sidecar process to write its pid
    let mut sidecar_pid: u32 = 0;
    for _ in 0..50 {
        if let Ok(content) = std::fs::read_to_string(&pid_file) {
            if let Ok(val) = content.trim().parse::<u32>() {
                sidecar_pid = val;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sidecar_pid > 0, "sidecar must have written its pid");
    unsafe {
        assert_eq!(
            libc::kill(sidecar_pid as libc::pid_t, 0),
            0,
            "sidecar process must be alive"
        );
    }

    // Account B tries to shut down Account A's sidecar -> returns false (isolated!)
    let res_b_shutdown = rpc_as_child(
        &p.router,
        pid_b,
        false,
        "pkg_sidecar_rpc_shutdown",
        json!({ "pkgId": "com.test.sidecar", "name": "pa-com-test-sidecar-loop" }),
    )
    .await;
    assert_eq!(res_b_shutdown["ok"], true);
    assert_eq!(
        res_b_shutdown["data"], false,
        "Account B must not shut down Account A's sidecar"
    );
    unsafe {
        assert_eq!(
            libc::kill(sidecar_pid as libc::pid_t, 0),
            0,
            "sidecar process must still be alive"
        );
    }

    // Account A shuts down its sidecar -> returns true and process is killed
    let res_a_shutdown = rpc_as_child(
        &p.router,
        pid_a,
        false,
        "pkg_sidecar_rpc_shutdown",
        json!({ "pkgId": "com.test.sidecar", "name": "pa-com-test-sidecar-loop" }),
    )
    .await;
    assert_eq!(res_a_shutdown["ok"], true);
    assert_eq!(res_a_shutdown["data"], true);

    tokio::time::sleep(Duration::from_millis(50)).await;
    unsafe {
        assert_ne!(
            libc::kill(sidecar_pid as libc::pid_t, 0),
            0,
            "sidecar process must be killed"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_loop_guard_kills_group_and_removes_pid_file() {
    use std::os::unix::process::ExitStatusExt;

    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("sidecar.pid");
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "echo $$ > {}; while true; do sleep 1; done",
            pid_file.display()
        ))
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if std::fs::read_to_string(&pid_file).is_ok_and(|c| c.trim().parse::<u32>().is_ok()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    drop(SidecarLoopGuard {
        pid_file: pid_file.clone(),
    });

    // `wait` reaps it; a guard that kills nothing leaves this timing out.
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("the guard must kill the sidecar")
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!pid_file.exists(), "the guard must remove the pid file");
}
