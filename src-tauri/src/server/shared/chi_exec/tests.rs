//! Tests for the shared Chi write core. Ported from `commands::chi`'s suite
//! (WP-P10) — none of them ever needed an `AppHandle` — plus the daemon-facing
//! additions: a stub engine driven through run → status → resume → cancel,
//! the identity guard, env hygiene and write confinement.
//!
//! `pub(crate)` pieces ([`stub_claude`], [`StubResolver`], [`wait_for`]) are
//! reused by the daemon arm tests in `server::rpc_local`.

use super::*;
use crate::server::shared::chi::{cache_list, parse_output_file};

pub(crate) async fn test_db() -> PaDb {
    let file_name = format!("ikenga-chi-test-{}.db", uuid::Uuid::new_v4());
    let db_path = std::env::temp_dir().join(file_name);
    PaDb::new(db_path)
}

/// The old `ChiCache` test shape: a cache dir under `root`.
struct TestCache {
    root: PathBuf,
}

impl TestCache {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn cache_dir(&self) -> PathBuf {
        self.root.join(super::super::chi::CACHE_DIR)
    }
    fn run_output_path(&self, run_id: &str) -> PathBuf {
        self.cache_dir().join(format!("{run_id}.json"))
    }
    fn ensure_cache_dir(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.cache_dir()).map_err(|e| e.to_string())
    }
}

/// Stands in for the host PATH and WSL. `native` binaries resolve to a
/// bare path of the same name so program assertions stay readable.
struct FakeResolver {
    native: Vec<&'static str>,
    wsl: Vec<&'static str>,
}

impl FakeResolver {
    fn native(bins: &[&'static str]) -> Self {
        Self {
            native: bins.to_vec(),
            wsl: vec![],
        }
    }
    fn wsl(bins: &[&'static str]) -> Self {
        Self {
            native: vec![],
            wsl: bins.to_vec(),
        }
    }
}

impl EngineResolver for FakeResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        self.native.contains(&binary).then(|| PathBuf::from(binary))
    }
    fn in_wsl(&self, binary: &str) -> bool {
        self.wsl.contains(&binary)
    }
}

fn args_of(cmd: &SpawnSpec) -> Vec<String> {
    cmd.args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect()
}

fn model_flags(cmd: &SpawnSpec) -> Vec<String> {
    let args = args_of(cmd);
    args.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--model")
        .map(|(i, _)| args[i + 1].clone())
        .collect()
}

fn opts(engine_id: &str, prompt: &str, cwd: Option<&str>) -> ChiRunOpts {
    ChiRunOpts {
        engine_id: engine_id.into(),
        prompt: prompt.into(),
        cwd: cwd.map(str::to_string),
        model: None,
        mode: None,
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: false,
    }
}

// ── stub engine ─────────────────────────────────────────────────────────

/// A fake `claude` speaking just enough stream-json: it reads the prompt
/// envelope from stdin, then prints `system:init` (session `sess-stub`), one
/// assistant text carrying its uid, cwd and argv, and a `result`. A prompt
/// containing `sleep` makes it `exec sleep 30` instead — a run to cancel.
///
/// Written once per test process: writing an executable while other test
/// threads fork can make a concurrent exec of it fail with ETXTBSY.
#[cfg(unix)]
pub(crate) fn stub_claude() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static STUB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    STUB.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ikenga-chi-stub-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude");
        std::fs::write(
            &path,
            r#"#!/bin/sh
prompt=$(cat)
case "$prompt" in *sleep*) exec sleep 30;; esac
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-stub"}'
printf '{"type":"assistant","message":{"content":[{"type":"text","text":"uid=%s cwd=%s args=%s"}]}}\n' "$(id -u)" "$(pwd)" "$*"
printf '%s\n' '{"type":"result","subtype":"success","stop_reason":"end_turn"}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    })
    .clone()
}

/// Resolves `claude` to the stub, nothing else.
pub(crate) struct StubResolver(pub PathBuf);

impl EngineResolver for StubResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        (binary == "claude").then(|| self.0.clone())
    }
    fn in_wsl(&self, _binary: &str) -> bool {
        false
    }
}

/// Poll `check` every 25 ms for up to 10 s.
pub(crate) async fn wait_for<F, Fut>(mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..400 {
        if check().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    check().await
}

/// A daemon-shaped env (cache-dir confinement, a default cwd) over a fresh
/// db and the stub engine.
#[cfg(unix)]
fn stub_env(root: &Path) -> ChiEnv {
    let db = Arc::new(PaDb::new(root.join("ikenga.db")));
    ChiEnv {
        db,
        cache_dir: root.join("chi-cache"),
        runtime: Arc::new(ChiRuntime::new()),
        default_cwd: Some(root.to_path_buf()),
        files: OutputFiles::InCacheDir,
        resolver: Arc::new(StubResolver(stub_claude())),
        cwd_expansion: CwdExpansion::TildeOnly,
        prompt_in_argv: PromptInArgv::Allowed,
    }
}

async fn row_status(db: &PaDb, run_id: &str) -> String {
    cache_get(db, run_id).await.unwrap().unwrap().status
}

#[tokio::test]
async fn chi_cache_round_trip() {
    let db = test_db().await;
    let cache = TestCache::new(std::env::temp_dir());
    cache.ensure_cache_dir().unwrap();

    let run_id = uuid::Uuid::new_v4().to_string();
    let output_path = cache.run_output_path(&run_id);
    let opts = ChiRunOpts {
        engine_id: "claude-code".into(),
        prompt: "hello".into(),
        cwd: Some("/tmp".into()),
        model: None,
        mode: None,
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: false,
    };

    cache_insert(&db, &run_id, &opts, &output_path, "cli")
        .await
        .unwrap();

    let row = cache_get(&db, &run_id).await.unwrap().unwrap();
    assert_eq!(row.run_id, run_id);
    assert_eq!(row.engine_id, "claude-code");
    assert_eq!(row.status, "queued");
    assert_eq!(row.cwd.as_deref(), Some("/tmp"));

    cache_update_status(&db, &run_id, "running", None)
        .await
        .unwrap();
    let row = cache_get(&db, &run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "running");
    assert!(row.last_seen_at.is_some());

    let rows = cache_list(&db, None, 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_id, run_id);
}

#[tokio::test]
async fn chi_output_file_round_trip() {
    let cache = TestCache::new(std::env::temp_dir());
    cache.ensure_cache_dir().unwrap();
    let path = cache.run_output_path("test-run");
    write_output_file(&path, "partial output", None)
        .await
        .unwrap();
    let file = read_output_file(&path).await.unwrap();
    assert_eq!(file.output.as_deref(), Some("partial output"));
}

#[test]
fn test_build_engine_command_antigravity() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["agy"]),
        "antigravity-cli",
        "hello",
        "/tmp",
        Some("gemini-2.0-flash"),
        Some("plan"),
        Some("conv-123"),
    )
    .unwrap();

    assert_eq!(cmd.program, "agy");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec![
            "-p",
            "hello",
            "--output-format",
            "stream-json",
            "--conversation",
            "conv-123",
            "--model",
            "gemini-2.0-flash",
            "--mode",
            "plan"
        ]
    );
}

#[test]
fn test_build_engine_command_opencode() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["opencode"]),
        "opencode",
        "fix the bug",
        "/tmp",
        Some("claude-3-7-sonnet"),
        None,
        None,
    )
    .unwrap();

    assert_eq!(cmd.program, "opencode");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec!["run", "-p", "fix the bug", "--model", "claude-3-7-sonnet",]
    );
}

#[test]
fn test_build_engine_command_pi() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["pi"]),
        "pi",
        "refactor this file",
        "/tmp",
        Some("claude-3-7-sonnet"),
        None,
        None,
    )
    .unwrap();

    assert_eq!(cmd.program, "pi");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec!["-p", "refactor this file", "--model", "claude-3-7-sonnet",]
    );
}

#[test]
fn resolve_engine_prefers_host_path_over_wsl() {
    let r = FakeResolver {
        native: vec!["claude"],
        wsl: vec!["claude"],
    };
    assert_eq!(
        resolve_engine("claude", &r).unwrap(),
        EngineLaunch::Native(PathBuf::from("claude"))
    );
}

#[test]
fn resolve_engine_falls_back_to_wsl() {
    assert_eq!(
        resolve_engine("claude", &FakeResolver::wsl(&["claude"])).unwrap(),
        EngineLaunch::Wsl {
            binary: "claude".into()
        }
    );
}

#[test]
fn resolve_engine_errors_clearly_when_nothing_resolves() {
    let err = resolve_engine("claude", &FakeResolver::native(&[])).unwrap_err();
    assert!(err.contains("`claude` not found"), "{err}");
    assert!(err.contains("install it or add it to PATH"), "{err}");
    // …and build_engine_command surfaces it instead of an OS spawn error.
    let err = build_engine_command_with(
        &FakeResolver::native(&[]),
        "claude-code",
        "hi",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(err.contains("`claude` not found"), "{err}");
}

#[test]
fn claude_code_in_wsl_launches_like_the_terminal() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["claude"]),
        "claude-code",
        "ignored: claude reads the prompt from stdin",
        r"C:\work\proj",
        Some("opus"),
        None,
        Some("sess-1"),
    )
    .unwrap();
    assert_eq!(cmd.program, "wsl.exe");
    let args = args_of(&cmd);
    assert_eq!(
        &args[..6],
        ["--cd", "C:/work/proj", "-e", "bash", "-l", "-c"]
    );
    assert_eq!(
        args[6],
        "'claude' '--permission-prompt-tool' 'stdio' '--permission-mode' 'default' \
         '--print' '--input-format' 'stream-json' '--output-format' 'stream-json' \
         '--verbose' '--resume' 'sess-1' '--model' 'opus'"
    );
    assert_eq!(args.len(), 7);
}

#[test]
fn claude_code_chi_run_defaults_to_the_chi_role_model() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(model_flags(&cmd), ["claude-sonnet-5-5"]);
}

#[test]
fn claude_code_chi_run_explicit_model_wins_once() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "",
        "/tmp",
        Some("claude-opus-5-5"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(model_flags(&cmd), ["claude-opus-5-5"]);
}

#[test]
fn runner_conf_model_resolves_for_claude_only() {
    assert_eq!(
        runner_model("claude-code", None).as_deref(),
        Some("claude-sonnet-5-5")
    );
    assert_eq!(
        runner_model("claude-code", Some("opus")).as_deref(),
        Some("opus")
    );
    assert_eq!(runner_model("codex", None), None);
    assert_eq!(
        runner_model("codex", Some("gpt-5")).as_deref(),
        Some("gpt-5")
    );
}

#[test]
fn non_claude_engines_get_no_role_default() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["pi"]),
        "pi",
        "x",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert!(model_flags(&cmd).is_empty());
}

#[test]
fn wsl_launch_quotes_prompts_for_bash() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["pi"]),
        "pi",
        "it's $HOME; rm -rf /",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(args_of(&cmd)[6], r"'pi' '-p' 'it'\''s $HOME; rm -rf /'");
}

/// The spec carries the cwd split it always had (cwd unless the engine
/// takes it as a flag — codex `--cd`), and since WP-P10 an explicit env:
/// cleared, the host env minus the host-only secrets, then the augmented
/// `PATH` on a native launch.
#[test]
fn engine_spec_keeps_path_and_the_set_cwd_split() {
    let host_only = |spec: &SpawnSpec| {
        spec.env
            .vars
            .iter()
            .any(|(k, _)| crate::pty::is_host_only_env(&k.to_string_lossy()))
    };
    let claude = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(claude.cwd, Some(PathBuf::from("/tmp")));
    assert!(claude.env.clear);
    assert!(!host_only(&claude));
    assert_eq!(
        claude.env.vars.last(),
        Some(&(
            OsString::from("PATH"),
            crate::runtime::augmented_path().to_os_string()
        ))
    );

    let codex = build_engine_command_with(
        &FakeResolver::native(&["codex"]),
        "codex",
        "",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(codex.cwd, None);
    assert!(codex.env.clear);
    assert_eq!(
        codex.env.vars.last().map(|(k, _)| k.clone()),
        Some("PATH".into())
    );

    // WSL: cwd is set on the host side too; wsl.exe gets the scrubbed env
    // but no augmented PATH override.
    let wsl = build_engine_command_with(
        &FakeResolver::wsl(&["pi"]),
        "pi",
        "",
        "/tmp",
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(wsl.cwd, Some(PathBuf::from("/tmp")));
    assert!(wsl.env.clear);
    assert!(!host_only(&wsl));

    assert_eq!(
        ENGINE_PIPED_OPTS,
        PipedOpts {
            stdin: StdioMode::Piped,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
            kill_on_drop: false,
            no_console_window: true,
            detached: false,
            new_process_group: false,
        }
    );
}

/// The executor-routed spawn hands back all three pipes, wired to the
/// child.
#[cfg(unix)]
#[tokio::test]
async fn spawn_engine_child_pipes_all_three_streams() {
    let mut spec = SpawnSpec::new("/bin/sh");
    spec.args(["-c", "cat; echo err >&2"]);
    let (mut child, mut stdin, mut stdout, stderr) = spawn_engine_child(spec).unwrap();
    stdin.write_all(b"ping").await.unwrap();
    drop(stdin);
    let mut out = String::new();
    stdout.read_to_string(&mut out).await.unwrap();
    let mut err = String::new();
    stderr
        .expect("stderr piped")
        .read_to_string(&mut err)
        .await
        .unwrap();
    assert!(child.wait().await.unwrap().success());
    assert_eq!(out, "ping");
    assert_eq!(err.trim(), "err");
}

#[test]
fn codex_in_wsl_gets_a_linux_cd_path() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["codex"]),
        "codex",
        "",
        r"C:\Users\x\proj",
        None,
        None,
        None,
    )
    .unwrap();
    let args = args_of(&cmd);
    assert_eq!(args[1], "C:/Users/x/proj");
    assert!(
        args[6].contains("'--cd' '/mnt/c/Users/x/proj'"),
        "{}",
        args[6]
    );
    assert_eq!(to_wsl_path("/already/linux"), "/already/linux");
}

/// A persistent run that fell back to in-process surfaces the fallback in
/// the result — still `running`, so no caller treats it as a failure —
/// while a plain in-process run carries no error at all.
#[test]
fn in_process_start_surfaces_a_persistent_fallback() {
    let warning = chi_runner::persistent_fallback_warning("chi-runner not found");
    let r = in_process_started("r1".into(), Some(warning.clone()));
    assert_eq!(r.status, "running");
    assert_eq!(r.error.as_deref(), Some(warning.as_str()));
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["status"], "running");
    assert_eq!(json["error"], serde_json::Value::String(warning));

    let plain = in_process_started("r2".into(), None);
    assert_eq!(plain.status, "running");
    assert_eq!(plain.error, None);
}

#[tokio::test]
async fn spawn_failure_marks_the_run_failed() {
    let db = test_db().await;
    let cache = TestCache::new(std::env::temp_dir());
    let run_id = uuid::Uuid::new_v4().to_string();
    let opts = ChiRunOpts {
        engine_id: "claude-code".into(),
        prompt: "hello".into(),
        cwd: None,
        model: None,
        mode: None,
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: false,
    };
    cache_insert(&db, &run_id, &opts, &cache.run_output_path(&run_id), "cli")
        .await
        .unwrap();

    // Resolution failure (engine not installed).
    let err = spawn_engine_or_fail(
        &db,
        &run_id,
        Err("engine binary `claude` not found on PATH or inside WSL".into()),
    )
    .await
    .unwrap_err();
    let row = cache_get(&db, &run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some(err.as_str()));
    assert!(row.ended_at.is_some());

    // OS spawn failure (resolved, but the program can't start).
    cache_update_status(&db, &run_id, "running", None)
        .await
        .unwrap();
    let bogus = SpawnSpec::new("ikenga-definitely-not-a-real-binary");
    let err = spawn_engine_or_fail(&db, &run_id, Ok(bogus))
        .await
        .unwrap_err();
    assert!(err.starts_with("spawn engine:"), "{err}");
    let row = cache_get(&db, &run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some(err.as_str()));
}

/// WP-40: a Chi run reaching `failed` / `done` produces a `run_failed` /
/// `run_finished` notification; `cancelled` produces none.
#[tokio::test]
async fn terminal_run_statuses_produce_run_notifications() {
    use crate::server::shared::notifications::{self, ListQuery, NotificationKind};

    let db = test_db().await;
    let cache = TestCache::new(std::env::temp_dir());
    let mk_opts = || ChiRunOpts {
        engine_id: "claude-code".into(),
        prompt: "pulse-refresh".into(),
        cwd: Some("/tmp/royalti-co".into()),
        model: None,
        mode: None,
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: false,
    };
    let failed_id = uuid::Uuid::new_v4().to_string();
    let done_id = uuid::Uuid::new_v4().to_string();
    let cancelled_id = uuid::Uuid::new_v4().to_string();
    for id in [&failed_id, &done_id, &cancelled_id] {
        cache_insert(&db, id, &mk_opts(), &cache.run_output_path(id), "cli")
            .await
            .unwrap();
    }
    cache_update_done(&db, &failed_id, "failed", Some("exit 1"), false, None)
        .await
        .unwrap();
    let produced = serde_json::json!([
        { "path": "/tmp/royalti-co/snap-1.json", "mime": "application/json", "producedBy": "Write" }
    ]);
    cache_update_done(&db, &done_id, "done", None, false, Some(&produced))
        .await
        .unwrap();
    cache_update_done(&db, &cancelled_id, "cancelled", None, false, None)
        .await
        .unwrap();

    let pool = db.ensure_pool().await.unwrap();
    let rows = notifications::list(&pool, &ListQuery::default())
        .await
        .unwrap();
    let for_run = |id: &str| {
        rows.iter()
            .filter(|n| n.action.as_ref().and_then(|a| a["runId"].as_str()) == Some(id))
            .collect::<Vec<_>>()
    };
    let failed = for_run(&failed_id);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].kind, NotificationKind::RunFailed);
    assert_eq!(failed[0].title, "pulse-refresh failed");
    let done = for_run(&done_id);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].kind, NotificationKind::RunFinished);
    let action = done[0].action.as_ref().unwrap();
    assert_eq!(action["artifactCount"], 1);
    assert_eq!(action["firstArtifactPath"], "/tmp/royalti-co/snap-1.json");
    assert_eq!(failed[0].action.as_ref().unwrap()["artifactCount"], 0);
    assert!(for_run(&cancelled_id).is_empty());
}

// ── WP-18b: detached chi-runner runs ────────────────────────────────

/// chi-runner's status file parses with its `status` / `external_id`; an
/// in-process file still parses; a half-written one is "no info".
#[test]
fn output_file_parse_reads_runner_fields_and_tolerates_partial_writes() {
    let runner = parse_output_file(
        r#"{"output":"hi","error":null,"done_at":"2026-09-27T10:00:00Z","status":"done","external_id":"sess-1"}"#,
    )
    .unwrap();
    assert_eq!(runner.status.as_deref(), Some("done"));
    assert_eq!(runner.external_id.as_deref(), Some("sess-1"));
    assert_eq!(runner.output.as_deref(), Some("hi"));

    let in_process = parse_output_file(r#"{"output":"x","done_at":"t"}"#).unwrap();
    assert_eq!(in_process.status, None);
    assert_eq!(in_process.external_id, None);

    for partial in [
        "",
        "{",
        r#"{"output":"hel"#,
        r#"{"status":"do"#,
        r#"{"status":"done","#,
    ] {
        assert!(
            parse_output_file(partial).is_none(),
            "{partial:?} must be no-info"
        );
    }
}

/// Insert a `running` detached row with `pid`, and (optionally) its
/// status file with `contents`.
async fn detached_row(
    db: &PaDb,
    dir: &Path,
    run_id: &str,
    pid: Option<i64>,
    contents: Option<&str>,
) {
    let output_path = dir.join(format!("{run_id}.json"));
    let opts = ChiRunOpts {
        engine_id: "claude-code".into(),
        prompt: format!("brief {run_id}"),
        cwd: Some("/tmp/work".into()),
        model: None,
        mode: None,
        timeout_seconds: None,
        parent_id: None,
        resume_session_id: None,
        persistent: true,
    };
    cache_insert(db, run_id, &opts, &output_path, "cli")
        .await
        .unwrap();
    match pid {
        Some(pid) => cache_mark_detached(db, run_id, pid as u32).await.unwrap(),
        None => cache_update_status(db, run_id, "running", None)
            .await
            .unwrap(),
    }
    if let Some(contents) = contents {
        std::fs::write(&output_path, contents).unwrap();
    }
}

/// G-88: every branch of the sweep, and each finished run notified once.
#[tokio::test]
async fn reconcile_detached_runs_finishes_each_run_once_and_notifies_once() {
    use crate::server::shared::notifications::{self, ListQuery, NotificationKind};
    use chi_runner::PidProbe;

    let db = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    detached_row(
        &db,
        dir,
        "done",
        Some(1001),
        Some(r#"{"output":"ok","status":"done","external_id":"sess-done"}"#),
    )
    .await;
    detached_row(
        &db,
        dir,
        "timed-out",
        Some(1002),
        Some(r#"{"status":"timed_out","error":"timed out after 5s"}"#),
    )
    .await;
    detached_row(
        &db,
        dir,
        "crashed",
        Some(1003),
        Some(r#"{"status":"running"}"#),
    )
    .await;
    detached_row(
        &db,
        dir,
        "crashed-partial",
        Some(1004),
        Some(r#"{"status":"do"#),
    )
    .await;
    detached_row(
        &db,
        dir,
        "reused-pid",
        Some(1005),
        Some(r#"{"status":"running"}"#),
    )
    .await;
    detached_row(
        &db,
        dir,
        "alive",
        Some(1006),
        Some(r#"{"status":"running","external_id":"sess-alive"}"#),
    )
    .await;
    detached_row(&db, dir, "alive-no-file", Some(1007), None).await;
    detached_row(&db, dir, "alive-partial", Some(1008), Some(r#"{"outp"#)).await;
    detached_row(&db, dir, "in-process", None, None).await;
    detached_row(&db, dir, "being-cancelled", Some(1009), None).await;

    let probe = |pid: u32| match pid {
        1005 => PidProbe::Foreign,
        1006 | 1007 => PidProbe::Ours,
        1008 => PidProbe::Unverified,
        _ => PidProbe::Dead,
    };
    let guard = CancellingGuard::hold("being-cancelled");

    assert_eq!(
        reconcile_detached_runs_with(&db, dir, &probe)
            .await
            .unwrap(),
        5
    );
    // Idempotent: nothing left to transition, nothing re-notified.
    assert_eq!(
        reconcile_detached_runs_with(&db, dir, &probe)
            .await
            .unwrap(),
        0
    );

    let row = |id: &'static str| {
        let db = &db;
        async move { cache_get(db, id).await.unwrap().unwrap() }
    };
    let done = row("done").await;
    assert_eq!(
        (done.status.as_str(), done.error.as_deref()),
        ("done", None)
    );
    assert_eq!(done.external_id.as_deref(), Some("sess-done"));
    assert!(done.ended_at.is_some());
    let timed_out = row("timed-out").await;
    assert_eq!(timed_out.status, "failed");
    assert_eq!(timed_out.error.as_deref(), Some("timed out after 5s"));
    for id in ["crashed", "crashed-partial", "reused-pid"] {
        let r = row(id).await;
        assert_eq!(r.status, "failed", "{id}");
        assert_eq!(
            r.error.as_deref(),
            Some(crate::server::shared::chi_liveness::RUNNER_EXITED_ERROR),
            "{id}"
        );
    }
    for id in [
        "alive",
        "alive-no-file",
        "alive-partial",
        "in-process",
        "being-cancelled",
    ] {
        assert_eq!(row(id).await.status, "running", "{id}");
    }
    // A live run's engine session id is picked up before it finishes.
    assert_eq!(
        row("alive").await.external_id.as_deref(),
        Some("sess-alive")
    );

    // Released: the next sweep may settle it.
    drop(guard);
    assert_eq!(
        reconcile_detached_runs_with(&db, dir, &probe)
            .await
            .unwrap(),
        1
    );

    let pool = db.ensure_pool().await.unwrap();
    let rows = notifications::list(&pool, &ListQuery::default())
        .await
        .unwrap();
    let for_run = |id: &str| {
        rows.iter()
            .filter(|n| n.action.as_ref().and_then(|a| a["runId"].as_str()) == Some(id))
            .collect::<Vec<_>>()
    };
    let finished = for_run("done");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].kind, NotificationKind::RunFinished);
    assert_eq!(finished[0].count, 1, "notified exactly once");
    for id in [
        "timed-out",
        "crashed",
        "crashed-partial",
        "reused-pid",
        "being-cancelled",
    ] {
        let n = for_run(id);
        assert_eq!(n.len(), 1, "{id}");
        assert_eq!(n[0].kind, NotificationKind::RunFailed, "{id}");
        assert_eq!(n[0].count, 1, "{id} notified exactly once");
    }
    for id in ["alive", "alive-no-file", "alive-partial", "in-process"] {
        assert!(for_run(id).is_empty(), "{id}");
    }
}

/// End-to-end against a real `chi-runner` (built from iyke-cli): spawn
/// detached, record the pid, let the runner fail an unknown engine, and
/// reconcile. Ignored by default — needs the binary on PATH:
/// `PATH=<dir with chi-runner>:$PATH cargo test --lib detached_chi_runner_smoke -- --ignored`
#[tokio::test]
#[ignore]
async fn detached_chi_runner_smoke() {
    let db = Arc::new(test_db().await);
    let env = ChiEnv::new(
        db.clone(),
        std::env::temp_dir().join(format!("chi-smoke-{}", uuid::Uuid::new_v4())),
        Arc::new(ChiRuntime::new()),
    );
    assert!(
        chi_runner::resolve_runner_path().is_some(),
        "chi-runner not on PATH"
    );
    let opts = ChiRunOpts {
        engine_id: "not-a-runner-engine".into(),
        prompt: "hello".into(),
        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        model: None,
        mode: None,
        timeout_seconds: Some(30),
        parent_id: None,
        resume_session_id: None,
        persistent: true,
    };
    let res = spawn_run(&env, &NoInProcessEngines, opts, "cli")
        .await
        .unwrap();
    let row = cache_get(&db, &res.run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "running");
    assert!(row.pid.is_some(), "detached run records its pid");

    let mut finished = 0;
    for _ in 0..100 {
        finished += reconcile_detached_runs_with(&db, &env.cache_dir, &chi_runner::probe_runner)
            .await
            .unwrap();
        if finished > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(finished, 1);
    let row = cache_get(&db, &res.run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(
        row.error.as_deref(),
        Some("engine not supported by chi-runner: not-a-runner-engine")
    );
}

/// The transition guard: a run already finished elsewhere (a cancel, the
/// in-process path) is neither overwritten nor re-notified by the sweep.
#[tokio::test]
async fn guarded_finish_leaves_an_already_finished_run_alone() {
    let db = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    detached_row(&db, tmp.path(), "cancelled", Some(2001), None).await;
    cache_update_status(&db, "cancelled", "cancelled", None)
        .await
        .unwrap();

    assert!(
        !cache_update_done_if_live(&db, "cancelled", "failed", Some("x"))
            .await
            .unwrap()
    );
    let r = cache_get(&db, "cancelled").await.unwrap().unwrap();
    assert_eq!(r.status, "cancelled");
    assert_eq!(r.error, None);
    assert_eq!(r.pid, Some(2001));
}

// ── run → status → resume → cancel against a stub engine ────────────────

#[cfg(unix)]
#[tokio::test]
async fn stub_run_finishes_resumes_and_cancels() {
    use crate::server::shared::notifications::{self, ListQuery, NotificationKind};

    let tmp = tempfile::tempdir().unwrap();
    let env = stub_env(&tmp.path().canonicalize().unwrap());

    // Run: `running` straight away, `done` once the stub exits.
    let res = spawn_run(
        &env,
        &NoInProcessEngines,
        opts("claude-code", "hello", None),
        "cli",
    )
    .await
    .unwrap();
    assert_eq!(res.status, "running");
    assert_eq!(res.error, None);
    let run_id = res.run_id.clone();
    assert!(wait_for(|| async { row_status(&env.db, &run_id).await == "done" }).await);
    let row = cache_get(&env.db, &run_id).await.unwrap().unwrap();
    assert_eq!(row.external_id.as_deref(), Some("sess-stub"));
    assert_eq!(row.owner, "cli");
    assert!(row.ended_at.is_some());
    let status = super::super::chi::status(
        &env.db,
        &env.cache_dir,
        &run_id,
        &chi_runner::probe_runner,
        OutputFiles::InCacheDir,
    )
    .await
    .unwrap();
    assert_eq!(status.status, "done");
    let out = status.output.unwrap();
    assert!(out.contains("--permission-mode default"), "{out}");
    assert!(
        !out.contains("--resume"),
        "a new run resumes nothing: {out}"
    );
    // The reader released its handle once the run ended.
    assert!(wait_for(|| async { env.runtime.len().await == 0 }).await);

    // Resume: same run id, the engine gets `--resume <its session id>`.
    let resumed = resume_run(&env, &NoInProcessEngines, run_id.clone(), "again".into())
        .await
        .unwrap();
    assert_eq!(resumed.run_id, run_id);
    assert_eq!(resumed.status, "running");
    assert!(
        wait_for(|| async {
            let file = read_output_file(&env.run_output_path(&run_id)).await;
            row_status(&env.db, &run_id).await == "done"
                && file
                    .and_then(|f| f.output)
                    .is_some_and(|o| o.contains("--resume sess-stub"))
        })
        .await,
        "resume must relaunch with --resume sess-stub"
    );

    // Cancel: a run that sleeps is killed and ends `cancelled`, unnotified.
    let sleeper = spawn_run(
        &env,
        &NoInProcessEngines,
        opts("claude-code", "please sleep", None),
        "cli",
    )
    .await
    .unwrap();
    assert!(
        crate::access::keepalive_count() >= 1,
        "a live run holds the daemon awake"
    );
    let cancelled = cancel_run(&env.db, &env.runtime, &sleeper.run_id)
        .await
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert_eq!(cancelled.run_id, sleeper.run_id);
    assert!(
        wait_for(|| async { env.runtime.len().await == 0 }).await,
        "the killed run's reader finished"
    );
    let row = cache_get(&env.db, &sleeper.run_id).await.unwrap().unwrap();
    assert_eq!(row.status, "cancelled");
    let pool = env.db.ensure_pool().await.unwrap();
    let rows = notifications::list(&pool, &ListQuery::default())
        .await
        .unwrap();
    let for_run = |id: &str| {
        rows.iter()
            .filter(|n| n.action.as_ref().and_then(|a| a["runId"].as_str()) == Some(id))
            .map(|n| n.kind)
            .collect::<Vec<_>>()
    };
    assert!(
        for_run(&sleeper.run_id).is_empty(),
        "cancelled is not notified"
    );
    assert_eq!(for_run(&run_id), vec![NotificationKind::RunFinished]);
}

/// A run naming no cwd starts in the surface default (the daemon's home).
#[cfg(unix)]
#[tokio::test]
async fn a_run_without_cwd_uses_the_surface_default() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = stub_env(&root);
    assert_eq!(env.run_cwd(None), root.to_string_lossy());
    assert_eq!(env.run_cwd(Some("/elsewhere")), "/elsewhere");
    let desktop = ChiEnv::new(env.db.clone(), env.cache_dir.clone(), env.runtime.clone());
    assert_eq!(
        desktop.run_cwd(None),
        std::env::current_dir().unwrap().to_string_lossy(),
        "the desktop keeps the process cwd"
    );
}

#[tokio::test]
async fn cancel_and_resume_of_an_unknown_run_say_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(test_db().await);
    let env = ChiEnv::new(
        db.clone(),
        tmp.path().join("chi-cache"),
        Arc::new(ChiRuntime::new()),
    );
    let err = cancel_run(&db, &env.runtime, "nope").await.err().unwrap();
    assert_eq!(err, "chi run not found: nope");
    let err = resume_run(&env, &NoInProcessEngines, "nope".into(), "x".into())
        .await
        .err()
        .unwrap();
    assert_eq!(err, "chi run not found: nope");
}

/// The daemon has no in-process engines: openrouter is refused, fail-closed,
/// and the row says why.
#[tokio::test]
async fn the_headless_daemon_refuses_openrouter_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Arc::new(test_db().await);
    let env = ChiEnv::new(
        db.clone(),
        tmp.path().join("chi-cache"),
        Arc::new(ChiRuntime::new()),
    );
    let err = spawn_run(
        &env,
        &NoInProcessEngines,
        opts("openrouter", "hi", Some("/tmp")),
        "cli",
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err, HEADLESS_OPENROUTER);
    let rows = cache_list(&db, Some("openrouter"), 10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "failed");
    assert_eq!(rows[0].error.as_deref(), Some(HEADLESS_OPENROUTER));

    // A (planted) finished openrouter row cannot be resumed here either.
    let err = resume_run(
        &env,
        &NoInProcessEngines,
        rows[0].run_id.clone(),
        "x".into(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err, HEADLESS_OPENROUTER);
}

/// A persistent run with no chi-runner installed falls back in-process and
/// says so in `error` while staying `running`. Skipped where a dev has
/// chi-runner on PATH.
#[cfg(unix)]
#[tokio::test]
async fn a_persistent_run_without_chi_runner_falls_back_loudly() {
    if chi_runner::resolve_runner_path().is_some() {
        eprintln!("chi-runner installed on this host — skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let env = stub_env(&tmp.path().canonicalize().unwrap());
    let mut o = opts("claude-code", "hello", None);
    o.persistent = true;
    let res = spawn_run(&env, &NoInProcessEngines, o, "cli")
        .await
        .unwrap();
    assert_eq!(res.status, "running");
    assert!(
        res.error
            .as_deref()
            .is_some_and(|e| e.contains("persistent run fell back to in-process")),
        "{:?}",
        res.error
    );
    assert!(wait_for(|| async { row_status(&env.db, &res.run_id).await == "done" }).await);
}

/// On the daemon a resume confines the row's output path before it reads or
/// writes anything: a row planted (via `db_exec`) to aim the reader at some
/// other file is refused.
#[tokio::test]
async fn resume_refuses_an_output_path_outside_the_cache_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let db = Arc::new(test_db().await);
    let mut env = ChiEnv::new(
        db.clone(),
        root.join("chi-cache"),
        Arc::new(ChiRuntime::new()),
    );
    env.files = OutputFiles::InCacheDir;
    env.ensure_cache_dir().unwrap();
    let victim = root.join("victim.json");
    let mut o = opts("claude-code", "x", None);
    o.resume_session_id = Some("sess-1".into());
    cache_insert(&db, "planted", &o, &victim, "cli")
        .await
        .unwrap();
    cache_update_status(&db, "planted", "done", None)
        .await
        .unwrap();
    let err = resume_run(&env, &NoInProcessEngines, "planted".into(), "x".into())
        .await
        .err()
        .unwrap();
    assert!(err.contains("outside the chi cache dir"), "{err}");
    assert!(!victim.exists(), "nothing was written");
    assert_eq!(row_status(&db, "planted").await, "done", "row untouched");
}

#[test]
fn confine_for_write_keeps_writes_inside_the_cache_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let cache = root.join("chi-cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("there.json"), "{}").unwrap();
    std::fs::write(root.join("outside.json"), "{}").unwrap();

    assert!(confine_for_write(&cache, &cache.join("there.json")).is_ok());
    assert!(confine_for_write(&cache, &cache.join("new.json")).is_ok());
    assert!(confine_for_write(&cache, &root.join("outside.json")).is_err());
    assert!(confine_for_write(&cache, &root.join("new-outside.json")).is_err());
    assert!(confine_for_write(&cache, &cache.join("../escape.json")).is_err());
    assert!(confine_for_write(&cache, Path::new("/nonexistent-dir/x.json")).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("outside.json"), cache.join("link.json")).unwrap();
        assert!(confine_for_write(&cache, &cache.join("link.json")).is_err());
        std::os::unix::fs::symlink(root.join("dangling"), cache.join("dangle.json")).unwrap();
        assert!(
            confine_for_write(&cache, &cache.join("dangle.json")).is_err(),
            "a write would follow a dangling link out"
        );
    }
}

// ── identity guard and env hygiene ──────────────────────────────────────

#[test]
fn identity_refusal_blocks_root_above_t0_only() {
    let refused = identity_refusal(ExecutorTier::T1, Some(0)).unwrap();
    assert!(refused.contains("as root"), "{refused}");
    assert!(refused.contains("t1"), "{refused}");
    assert!(identity_refusal(ExecutorTier::T2, Some(0)).is_some());
    // A principal child (T1, non-root) runs; a T0 daemon is single-user.
    assert_eq!(identity_refusal(ExecutorTier::T1, Some(28200)), None);
    assert_eq!(identity_refusal(ExecutorTier::T0, Some(0)), None);
    assert_eq!(identity_refusal(ExecutorTier::T1, None), None);
}

#[test]
fn scrubbed_env_drops_the_host_only_secrets() {
    let vars = [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/ada"),
        ("ANTHROPIC_API_KEY", "user-key"),
        ("IKENGA_AUTH_TOKEN", "bearer"),
        ("IKENGA_VAULT_KEY", "vault"),
        ("IKENGA_SECRET_FAL", "operator-default"),
        ("IKENGA_PRINCIPAL_SECRETS_KEY", "wrap"),
        ("IKENGA_PKG_DB_TOKEN", "pkg"),
    ]
    .map(|(k, v)| (OsString::from(k), OsString::from(v)));
    let kept: Vec<String> = scrubbed_env(vars)
        .into_iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    // The user's own engine credentials pass; the daemon's never do.
    assert_eq!(kept, ["PATH", "HOME", "ANTHROPIC_API_KEY"]);
}

// ── prompt in argv (I-7) ───────────────────────────────────────────────

/// `PROMPT_IN_ARGV_ENGINES` is exactly the set of engines whose built
/// command carries the prompt, so the T1 refusal can't miss one: moving an
/// engine to stdin, or adding an argv-prompt engine, fails here until the
/// list follows.
#[test]
fn prompt_in_argv_matches_the_engine_commands() {
    const PROMPT: &str = "SENTINEL-PROMPT-7f3a";
    let resolver = FakeResolver::native(&["claude", "agy", "codex", "opencode", "pi"]);
    for engine in ["claude-code", "antigravity-cli", "codex", "opencode", "pi"] {
        for resume in [None, Some("sess-1")] {
            let cmd =
                build_engine_command_with(&resolver, engine, PROMPT, "/tmp", None, None, resume)
                    .unwrap();
            let in_argv = cmd
                .args
                .iter()
                .any(|a| a.to_string_lossy().contains(PROMPT));
            assert_eq!(
                in_argv,
                prompt_rides_argv(engine),
                "{engine} (resume {resume:?}): prompt in argv = {in_argv}"
            );
        }
    }
    assert!(!prompt_rides_argv("claude-code"));
    assert!(!prompt_rides_argv("codex"));
}

#[test]
fn prompt_in_argv_is_refused_only_above_t0_without_hidepid() {
    use PromptInArgv::{Allowed, Refused};
    assert_eq!(prompt_in_argv_policy(ExecutorTier::T0, false), Allowed);
    assert_eq!(prompt_in_argv_policy(ExecutorTier::T0, true), Allowed);
    assert_eq!(prompt_in_argv_policy(ExecutorTier::T1, false), Refused);
    assert_eq!(prompt_in_argv_policy(ExecutorTier::T1, true), Allowed);
    assert_eq!(prompt_in_argv_policy(ExecutorTier::T2, false), Refused);
}

#[test]
fn mountinfo_hidepid_detection() {
    let line = |opts: &str| {
        format!(
            "22 1 0:21 / /sys rw,nosuid shared:7 - sysfs sysfs rw\n\
             25 1 0:23 / /proc rw,nosuid,nodev,noexec,relatime shared:13 - proc proc {opts}\n"
        )
    };
    // A stock /proc (the docker / CI container case): not hidden.
    assert!(!mountinfo_proc_hides_other_uids(&line("rw")));
    assert!(!mountinfo_proc_hides_other_uids(&line("rw,hidepid=0")));
    assert!(!mountinfo_proc_hides_other_uids(&line("rw,hidepid=off")));
    // systemd ProtectProc=invisible, and the other hiding modes.
    for mode in ["invisible", "2", "noaccess", "1", "ptraceable", "4"] {
        assert!(
            mountinfo_proc_hides_other_uids(&line(&format!("rw,hidepid={mode}"))),
            "{mode}"
        );
    }
    // The topmost /proc mount decides: a hidepid mount stacked over a plain
    // one hides, a plain one stacked over a hidepid one does not.
    let stacked = |a: &str, b: &str| {
        format!(
            "25 1 0:23 / /proc rw - proc proc {a}\n\
             90 25 0:50 / /proc rw - proc proc {b}\n"
        )
    };
    assert!(mountinfo_proc_hides_other_uids(&stacked(
        "rw",
        "rw,hidepid=invisible"
    )));
    assert!(!mountinfo_proc_hides_other_uids(&stacked(
        "rw,hidepid=invisible",
        "rw"
    )));
    // Not /proc, or not procfs: ignored. Nothing parseable: not hidden.
    assert!(!mountinfo_proc_hides_other_uids(
        "30 1 0:23 / /mnt/proc rw - proc proc rw,hidepid=2\n"
    ));
    assert!(!mountinfo_proc_hides_other_uids(
        "30 1 0:23 / /proc rw - tmpfs tmpfs rw,hidepid=2\n"
    ));
    assert!(!mountinfo_proc_hides_other_uids(""));
    assert!(!mountinfo_proc_hides_other_uids("garbage line"));
}

/// Resolves nothing, but records every binary it was asked for — proof an
/// engine was never even looked up.
#[derive(Default)]
struct RecordingResolver(std::sync::Mutex<Vec<String>>);

impl EngineResolver for RecordingResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        self.0.lock().unwrap().push(binary.to_string());
        None
    }
    fn in_wsl(&self, _binary: &str) -> bool {
        false
    }
}

/// I-7 regression: where another uid could read `/proc/<pid>/cmdline`
/// (T1 without hidepid), an engine taking the prompt as `-p <prompt>` never
/// starts — new run or resume, in-process or persistent — and its row says
/// why. Before the fix a T1 `chi_run {engineId:"pi"}` spawned `pi -p
/// <prompt>`, readable by every other principal on the host.
#[tokio::test]
async fn argv_prompt_engines_are_refused_where_proc_is_not_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let db = Arc::new(test_db().await);
    let resolver = Arc::new(RecordingResolver::default());
    let mut env = ChiEnv::new(
        db.clone(),
        root.join("chi-cache"),
        Arc::new(ChiRuntime::new()),
    );
    env.files = OutputFiles::InCacheDir;
    env.cwd_expansion = CwdExpansion::TildeOnly;
    env.prompt_in_argv = PromptInArgv::Refused;
    env.resolver = resolver.clone();

    for engine in PROMPT_IN_ARGV_ENGINES {
        for persistent in [false, true] {
            let mut o = opts(engine, "ADA-PRIVATE payroll", Some("/tmp"));
            o.persistent = persistent;
            let err = spawn_run(&env, &NoInProcessEngines, o, "cli")
                .await
                .err()
                .unwrap();
            assert_eq!(err, prompt_in_argv_refusal(engine), "{engine}");
            assert!(
                !err.contains("ADA-PRIVATE"),
                "the refusal never echoes the prompt"
            );
        }
        let rows = cache_list(&db, Some(engine), 10).await.unwrap();
        assert_eq!(rows.len(), 2, "{engine}");
        for row in &rows {
            assert_eq!(row.status, "failed", "{engine}");
            assert_eq!(
                row.error.as_deref(),
                Some(prompt_in_argv_refusal(engine).as_str())
            );
        }

        // A resumable row of that engine is refused too, and left alone.
        let resume_id = format!("planted-{engine}");
        let mut o = opts(engine, "x", None);
        o.resume_session_id = Some("sess-1".into());
        cache_insert(&db, &resume_id, &o, &env.run_output_path(&resume_id), "cli")
            .await
            .unwrap();
        cache_update_status(&db, &resume_id, "done", None)
            .await
            .unwrap();
        let err = resume_run(
            &env,
            &NoInProcessEngines,
            resume_id.clone(),
            "secret".into(),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err, prompt_in_argv_refusal(engine));
        assert_eq!(row_status(&db, &resume_id).await, "done", "row untouched");
    }
    assert!(
        resolver.0.lock().unwrap().is_empty(),
        "no engine binary was even looked up: {:?}",
        resolver.0.lock().unwrap()
    );
}

/// The stdin engines are untouched by the refusal: a claude-code run still
/// runs to `done` where argv prompts are refused.
#[cfg(unix)]
#[tokio::test]
async fn stdin_prompt_engines_still_run_where_argv_prompts_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let mut env = stub_env(&tmp.path().canonicalize().unwrap());
    env.prompt_in_argv = PromptInArgv::Refused;
    let res = spawn_run(
        &env,
        &NoInProcessEngines,
        opts("claude-code", "hello", None),
        "cli",
    )
    .await
    .unwrap();
    assert_eq!(res.status, "running");
    assert!(wait_for(|| async { row_status(&env.db, &res.run_id).await == "done" }).await);
}

/// Operator-secret regression: the daemon expands only `~` in a run's cwd.
/// Before the fix `shellexpand::full` resolved `$VAR` against the daemon's
/// own environment — which carries every `IKENGA_SECRET_*` operator default —
/// and the value went into codex's `--cd` argv and the runner's conf file,
/// handing a Dispatch-only caller what needs Secrets. `$HOME` / `$PATH`
/// stand in for the secret (same mechanism, no process-env mutation).
#[test]
fn the_daemon_expands_only_tilde_in_a_run_cwd() {
    let home = std::env::var("HOME").expect("HOME is set");
    let path = std::env::var("PATH").expect("PATH is set");
    let mut env = ChiEnv::new(
        Arc::new(PaDb::new(std::env::temp_dir().join("unused.db"))),
        std::env::temp_dir().join("chi-cache"),
        Arc::new(ChiRuntime::new()),
    );
    env.cwd_expansion = CwdExpansion::TildeOnly;

    for (asked, want) in [
        (
            "$IKENGA_SECRET_DEMO_KEY",
            "$IKENGA_SECRET_DEMO_KEY".to_string(),
        ),
        ("$PATH", "$PATH".to_string()),
        ("/w/${PATH}/x", "/w/${PATH}/x".to_string()),
        ("$HOME/proj", "$HOME/proj".to_string()),
        ("~/proj", format!("{home}/proj")),
        ("~", home.clone()),
        ("/abs/dir", "/abs/dir".to_string()),
    ] {
        assert_eq!(env.run_cwd(Some(asked)), want, "{asked}");
    }

    // End to end into the engine argv: codex's `--cd` is the literal cwd.
    let cwd = env.run_cwd(Some("$PATH"));
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["codex"]),
        "codex",
        "x",
        &cwd,
        None,
        None,
        None,
    )
    .unwrap();
    let args = args_of(&cmd);
    let cd = args.iter().position(|a| a == "--cd").unwrap();
    assert_eq!(args[cd + 1], "$PATH");
    assert!(
        !args.iter().any(|a| a.contains(&path)),
        "no environment value reaches the argv: {args:?}"
    );

    // The desktop keeps its historical full expansion.
    let desktop = ChiEnv::new(env.db.clone(), env.cache_dir.clone(), env.runtime.clone());
    assert_eq!(desktop.cwd_expansion, CwdExpansion::Full);
    assert_eq!(desktop.prompt_in_argv, PromptInArgv::Allowed);
    assert_eq!(desktop.run_cwd(Some("$HOME/proj")), format!("{home}/proj"));
}
