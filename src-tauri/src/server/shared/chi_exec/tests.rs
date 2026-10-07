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
    /// `Some(reason)`: WSL can't be asked.
    wsl_down: Option<&'static str>,
    distro: Option<&'static str>,
    /// The distro each `in_wsl` probe asked.
    probed: std::sync::Mutex<Vec<Option<String>>>,
}

impl FakeResolver {
    fn native(bins: &[&'static str]) -> Self {
        Self {
            native: bins.to_vec(),
            wsl: vec![],
            wsl_down: None,
            distro: None,
            probed: Default::default(),
        }
    }
    fn wsl(bins: &[&'static str]) -> Self {
        Self {
            native: vec![],
            wsl: bins.to_vec(),
            wsl_down: None,
            distro: None,
            probed: Default::default(),
        }
    }
}

impl EngineResolver for FakeResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        self.native.contains(&binary).then(|| PathBuf::from(binary))
    }
    fn in_wsl<'a>(&'a self, binary: &'a str, distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        self.probed.lock().unwrap().push(distro.map(str::to_string));
        let lookup = match self.wsl_down {
            Some(why) => WslLookup::WslUnavailable(why.to_string()),
            None if self.wsl.contains(&binary) => WslLookup::Found(format!("/usr/bin/{binary}")),
            None => WslLookup::NotFound,
        };
        Box::pin(std::future::ready(lookup))
    }
    fn wsl_distro(&self) -> Option<String> {
        self.distro.map(str::to_string)
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
    fn in_wsl<'a>(&'a self, _binary: &'a str, _distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        Box::pin(std::future::ready(WslLookup::NotFound))
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

#[tokio::test]
async fn test_build_engine_command_antigravity() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["agy"]),
        "antigravity-cli",
        "/tmp",
        Some("gemini-2.0-flash"),
        Some("plan"),
        Some("conv-123"),
    ).await
    .unwrap();

    assert_eq!(cmd.program, "agy");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec![
            "--input-format",
            "stream-json",
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

#[tokio::test]
async fn test_build_engine_command_opencode() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["opencode"]),
        "opencode",
        "/tmp",
        Some("claude-3-7-sonnet"),
        None,
        None,
    ).await
    .unwrap();

    assert_eq!(cmd.program, "opencode");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        vec!["run", "--format", "json", "--model", "claude-3-7-sonnet"]
    );

    let resumed = build_engine_command_with(
        &FakeResolver::native(&["opencode"]),
        "opencode",
        "/tmp",
        None,
        None,
        Some("ses_1"),
    ).await
    .unwrap();
    assert_eq!(
        args_of(&resumed),
        ["run", "--format", "json", "--session", "ses_1"]
    );
}

#[tokio::test]
async fn test_build_engine_command_pi() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["pi"]),
        "pi",
        "/tmp",
        Some("claude-3-7-sonnet"),
        None,
        None,
    ).await
    .unwrap();

    assert_eq!(cmd.program, "pi");
    let args: Vec<&str> = cmd.args.iter().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(args, vec!["--mode", "json", "--model", "claude-3-7-sonnet"]);

    let resumed = build_engine_command_with(
        &FakeResolver::native(&["pi"]),
        "pi",
        "/tmp",
        None,
        None,
        Some("0b1c"),
    ).await
    .unwrap();
    assert_eq!(args_of(&resumed), ["--mode", "json", "--session", "0b1c"]);
}

#[tokio::test]
async fn resolve_engine_prefers_host_path_over_wsl() {
    let r = FakeResolver {
        native: vec!["claude"],
        ..FakeResolver::wsl(&["claude"])
    };
    assert_eq!(
        resolve_engine("claude", &r, "/tmp").await.unwrap(),
        EngineLaunch::Native(PathBuf::from("claude"))
    );
}

#[tokio::test]
async fn resolve_engine_falls_back_to_wsl() {
    assert_eq!(
        resolve_engine("claude", &FakeResolver::wsl(&["claude"]), "/tmp").await.unwrap(),
        EngineLaunch::Wsl {
            binary: "claude".into(),
            distro: None,
        }
    );
}

/// A WSL that couldn't be asked is not "not installed": the error names
/// WSL and its reason, and never tells the user to install the CLI.
#[tokio::test]
async fn resolve_engine_reports_wsl_unavailable_instead_of_not_installed() {
    let r = FakeResolver {
        wsl_down: Some("wsl.exe did not answer within 20s"),
        ..FakeResolver::wsl(&["claude"])
    };
    let err = resolve_engine("claude", &r, "/tmp").await.unwrap_err();
    assert!(
        err.contains("WSL unavailable: wsl.exe did not answer within 20s"),
        "{err}"
    );
    assert!(!err.contains("install it"), "{err}");
    let err = build_engine_command_with(&r, "claude-code", "/tmp", None, None, None)
        .await
        .unwrap_err();
    assert!(err.contains("WSL unavailable:"), "{err}");
}

/// Chi launches WSL engines in the configured distro, as the terminal does.
#[tokio::test]
async fn wsl_launch_uses_the_configured_distro() {
    let r = FakeResolver {
        distro: Some("Debian"),
        ..FakeResolver::wsl(&["claude"])
    };
    assert_eq!(
        resolve_engine("claude", &r, "/tmp").await.unwrap(),
        EngineLaunch::Wsl {
            binary: "claude".into(),
            distro: Some("Debian".into()),
        }
    );
    let cmd = build_engine_command_with(&r, "claude-code", r"C:\work", None, None, None)
        .await
        .unwrap();
    assert_eq!(
        &args_of(&cmd)[..6],
        ["-d", "Debian", "--cd", "C:/work", "-e", "bash"]
    );
}

/// Regression: a project that lives only inside the distro
/// (`\\wsl.localhost\Ubuntu\home\me\proj`) failed to start with "The
/// directory name is invalid (os error 267)" — the host-side cwd was set to
/// a path Windows can't enter. Now the cwd goes only to `wsl.exe --cd`, as
/// the Linux path, in the distro the path names.
#[tokio::test]
async fn wsl_share_project_runs_without_a_host_cwd() {
    let share = r"\\wsl.localhost\Ubuntu\home\me\proj";
    let claude = build_engine_command_with(
        &FakeResolver::wsl(&["claude"]),
        "claude-code",
        share,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(claude.cwd, None);
    assert_eq!(
        &args_of(&claude)[..4],
        ["-d", "Ubuntu", "--cd", "/home/me/proj"]
    );

    let codex =
        build_engine_command_with(&FakeResolver::wsl(&["codex"]), "codex", share, None, None, None)
            .await
            .unwrap();
    assert_eq!(codex.cwd, None);
    let args = args_of(&codex);
    assert_eq!(&args[2..4], ["--cd", "/home/me/proj"], "{args:?}");
    assert!(args[8].contains("'--cd' '.'"), "{args:?}");
}

/// A project on a distro share (`\\wsl.localhost\Debian\…`) exists only in
/// that distro: the run is looked up and launched there, not in the
/// configured (or default) one, where `--cd` would miss or land in a
/// same-named directory of another tree.
#[tokio::test]
async fn wsl_share_project_runs_in_the_distro_it_lives_in() {
    let r = FakeResolver {
        distro: Some("Ubuntu"),
        ..FakeResolver::wsl(&["claude"])
    };
    let cmd = build_engine_command_with(
        &r,
        "claude-code",
        r"\\wsl.localhost\Debian\home\me\proj",
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        &args_of(&cmd)[..4],
        ["-d", "Debian", "--cd", "/home/me/proj"]
    );
    assert_eq!(*r.probed.lock().unwrap(), [Some("Debian".to_string())]);

    // A drive path keeps the configured distro.
    r.probed.lock().unwrap().clear();
    let cmd = build_engine_command_with(&r, "claude-code", r"C:\work", None, None, None)
        .await
        .unwrap();
    assert_eq!(&args_of(&cmd)[..2], ["-d", "Ubuntu"]);
    assert_eq!(*r.probed.lock().unwrap(), [Some("Ubuntu".to_string())]);
}

#[tokio::test]
async fn resolve_engine_errors_clearly_when_nothing_resolves() {
    let err = resolve_engine("claude", &FakeResolver::native(&[]), "/tmp").await.unwrap_err();
    assert!(err.contains("`claude` not found"), "{err}");
    assert!(err.contains("install it or add it to PATH"), "{err}");
    // …and build_engine_command surfaces it instead of an OS spawn error.
    let err = build_engine_command_with(
        &FakeResolver::native(&[]),
        "claude-code",
        "/tmp",
        None,
        None,
        None,
    ).await
    .unwrap_err();
    assert!(err.contains("`claude` not found"), "{err}");
}

#[tokio::test]
async fn claude_code_in_wsl_launches_like_the_terminal() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["claude"]),
        "claude-code",
        r"C:\work\proj",
        Some("opus"),
        None,
        Some("sess-1"),
    ).await
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

#[tokio::test]
async fn claude_code_chi_run_defaults_to_the_chi_role_model() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "/tmp",
        None,
        None,
        None,
    ).await
    .unwrap();
    assert_eq!(model_flags(&cmd), ["claude-sonnet-5-5"]);
}

#[tokio::test]
async fn claude_code_chi_run_explicit_model_wins_once() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "/tmp",
        Some("claude-opus-5-5"),
        None,
        None,
    ).await
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

#[tokio::test]
async fn non_claude_engines_get_no_role_default() {
    let cmd = build_engine_command_with(
        &FakeResolver::native(&["pi"]),
        "pi",
        "/tmp",
        None,
        None,
        None,
    ).await
    .unwrap();
    assert!(model_flags(&cmd).is_empty());
}

#[tokio::test]
async fn wsl_launch_quotes_args_for_bash() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["pi"]),
        "pi",
        "/tmp",
        Some("it's $HOME; rm -rf /"),
        None,
        None,
    ).await
    .unwrap();
    assert_eq!(
        args_of(&cmd)[6],
        r"'pi' '--mode' 'json' '--model' 'it'\''s $HOME; rm -rf /'"
    );
}

/// The spec carries the cwd split it always had (cwd unless the engine
/// takes it as a flag — codex `--cd`), and since WP-P10 an explicit env:
/// cleared, the host env minus the host-only secrets, then the augmented
/// `PATH` on a native launch.
#[tokio::test]
async fn engine_spec_keeps_path_and_the_set_cwd_split() {
    let host_only = |spec: &SpawnSpec| {
        spec.env
            .vars
            .iter()
            .any(|(k, _)| crate::pty::is_host_only_env(&k.to_string_lossy()))
    };
    let claude = build_engine_command_with(
        &FakeResolver::native(&["claude"]),
        "claude-code",
        "/tmp",
        None,
        None,
        None,
    ).await
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
        "/tmp",
        None,
        None,
        None,
    ).await
    .unwrap();
    assert_eq!(codex.cwd, None);
    assert!(codex.env.clear);
    assert_eq!(
        codex.env.vars.last().map(|(k, _)| k.clone()),
        Some("PATH".into())
    );

    // WSL: no host-side cwd — the directory may exist only inside the
    // distro, where Windows can't enter it (os error 267); it reaches the
    // engine through `wsl.exe --cd` alone. wsl.exe gets the scrubbed env but
    // no augmented PATH override.
    let wsl =
        build_engine_command_with(&FakeResolver::wsl(&["pi"]), "pi", "/tmp", None, None, None).await
            .unwrap();
    assert_eq!(wsl.cwd, None);
    assert_eq!(&args_of(&wsl)[..2], ["--cd", "/tmp"]);
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

/// A WSL codex is started in the directory by `wsl.exe --cd`, which maps a
/// drive path through the distro's own automount root; its own `--cd` is
/// `.`, never a guessed `/mnt/<drive>` path.
#[tokio::test]
async fn codex_in_wsl_cds_through_wsl_exe() {
    let cmd = build_engine_command_with(
        &FakeResolver::wsl(&["codex"]),
        "codex",
        r"C:\Users\x\proj",
        None,
        None,
        None,
    ).await
    .unwrap();
    let args = args_of(&cmd);
    assert_eq!(args[1], "C:/Users/x/proj");
    assert!(args[6].contains("'--cd' '.'"), "{}", args[6]);
    assert!(!args[6].contains("/mnt/"), "{}", args[6]);
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
    let cancelled = cancel_run(&env.db, &env.runtime, &env.cache_dir, &sleeper.run_id)
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
    let err = cancel_run(&db, &env.runtime, &env.cache_dir, "nope")
        .await
        .err()
        .unwrap();
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

// ── the prompt never rides argv (I-7) ───────────────────────────────────

/// Marks prompt text in the tests below; must never appear in an argv.
const SENTINEL: &str = "SENTINEL-PROMPT-7f3a";

/// One stub per engine CLI, written once per test process (see
/// [`stub_claude`] on ETXTBSY). Each records its argv (one per line) to
/// `./argv` and its whole stdin to `./stdin` in the directory it runs in
/// (codex: its `--cd`), then speaks just enough of its engine's stream for
/// the run to end `done` with a session id. Stdin containing `sleep` makes
/// it sleep instead — a run to cancel.
#[cfg(unix)]
fn stub_engines() -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ikenga-chi-stubs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let record = r#"args=$(printf '%s\n' "$@")
prev=
for a in "$@"; do [ "$prev" = --cd ] && cd "$a"; prev=$a; done
printf '%s\n' "$args" > argv
cat > stdin
case "$(cat stdin)" in *sleep*) exec sleep 30;; esac
"#;
        let stubs = [
            (
                "claude",
                r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-claude"}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"text","text":"ok"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","stop_reason":"end_turn"}'"#,
            ),
            (
                "agy",
                r#"printf '%s\n' '{"event":"init","conversation_id":"conv-agy"}'
printf '%s\n' '{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"ok"}}'
printf '%s\n' '{"event":"result","result":{"status":"SUCCESS"}}'"#,
            ),
            (
                "codex",
                r#"printf '%s\n' '{"type":"thread.started","thread_id":"thread-codex"}'
printf '%s\n' '{"type":"turn.completed","usage":{}}'"#,
            ),
            (
                "opencode",
                r#"printf '%s\n' '{"type":"text","sessionID":"ses_opencode","part":{"type":"text","text":"ok"}}'
printf '%s\n' '{"type":"step_finish","sessionID":"ses_opencode","part":{"reason":"stop"}}'"#,
            ),
            (
                "pi",
                r#"printf '%s\n' '{"type":"session","version":3,"id":"pi-session"}'
printf '%s\n' '{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"ok"}}'
printf '%s\n' '{"type":"agent_end","messages":[{"role":"assistant","stopReason":"stop"}]}'"#,
            ),
        ];
        for (bin, speak) in stubs {
            let path = dir.join(bin);
            std::fs::write(&path, format!("#!/bin/sh\n{record}{speak}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    })
    .clone()
}

/// Resolves every engine binary to its stub in [`stub_engines`].
#[cfg(unix)]
struct StubDirResolver(PathBuf);

#[cfg(unix)]
impl EngineResolver for StubDirResolver {
    fn native(&self, binary: &str) -> Option<PathBuf> {
        let path = self.0.join(binary);
        path.is_file().then_some(path)
    }
    fn in_wsl<'a>(&'a self, _binary: &'a str, _distro: Option<&'a str>) -> BoxFuture<'a, WslLookup> {
        Box::pin(std::future::ready(WslLookup::NotFound))
    }
}

/// A daemon-shaped env over every stub engine.
#[cfg(unix)]
fn stub_engines_env(root: &Path) -> ChiEnv {
    let mut env = stub_env(root);
    env.resolver = Arc::new(StubDirResolver(stub_engines()));
    env
}

/// Every engine with its stub's session id and the flag its resume passes it
/// with.
const ENGINES: &[(&str, &str, &str)] = &[
    ("claude-code", "sess-claude", "--resume"),
    ("antigravity-cli", "conv-agy", "--conversation"),
    ("codex", "thread-codex", "resume"),
    ("opencode", "ses_opencode", "--session"),
    ("pi", "pi-session", "--session"),
];

/// What `engine` must have read on stdin for `prompt`.
fn assert_stdin_carries(engine: &str, stdin: &str, prompt: &str) {
    match engine {
        "claude-code" => {
            let v: serde_json::Value = serde_json::from_str(stdin.trim()).unwrap();
            assert_eq!(v["type"], "user", "{engine}: {stdin}");
            assert_eq!(v["message"]["content"], prompt, "{engine}");
        }
        "antigravity-cli" => {
            let v: serde_json::Value = serde_json::from_str(stdin.trim()).unwrap();
            assert_eq!(v["event"], "user", "{engine}: {stdin}");
            assert_eq!(v["message"]["content"], prompt, "{engine}");
        }
        _ => assert_eq!(stdin, prompt, "{engine}: the bare prompt"),
    }
}

/// I-7: no engine ever gets the prompt — of a new run or of a resume's
/// follow-up — on its command line, where every uid on the host can read it
/// from `/proc/<pid>/cmdline` unless *its* procfs is `hidepid` (which no check
/// in this process can establish: an SSH session uses the host `/proc`). Each
/// engine runs against a stub that records the argv it was exec'd with and
/// what it read on stdin: the prompt is only ever on stdin. A persistent run
/// of an engine chi-runner would hand the prompt on argv stays in-process.
/// Before the fix, pi / opencode / antigravity-cli ran as `… -p <prompt>`.
#[cfg(unix)]
#[tokio::test]
async fn no_engine_ever_sees_the_prompt_in_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = stub_engines_env(&root);

    for &(engine, session, resume_flag) in ENGINES {
        // chi-runner would launch the real engine CLI, not the stub: persistent
        // is only exercised where it stays in-process by design.
        let detachable = RUNNER_STDIN_ENGINES.contains(&engine);
        for persistent in [false, true] {
            if persistent && detachable {
                continue;
            }
            let cwd = root.join(format!("{engine}-{persistent}"));
            std::fs::create_dir_all(&cwd).unwrap();
            let read = |name: &str| std::fs::read_to_string(cwd.join(name)).unwrap_or_default();

            // A new run.
            let prompt = format!("{SENTINEL} new run, it's $HOME; `id` \"q\"");
            let mut o = opts(engine, &prompt, Some(cwd.to_string_lossy().as_ref()));
            o.persistent = persistent;
            let res = spawn_run(&env, &NoInProcessEngines, o, "cli")
                .await
                .unwrap();
            assert_eq!(res.status, "running", "{engine}");
            if persistent {
                assert_eq!(res.error, Some(not_detachable_warning(engine)), "{engine}");
                assert!(!res.error.as_deref().unwrap().contains(SENTINEL));
            } else {
                assert_eq!(res.error, None, "{engine}");
            }
            let run_id = res.run_id.clone();
            assert!(
                wait_for(|| async { row_status(&env.db, &run_id).await == "done" }).await,
                "{engine}: {:?}",
                cache_get(&env.db, &run_id).await.unwrap().unwrap().error
            );
            let argv = read("argv");
            assert!(!argv.is_empty(), "{engine}: the stub ran");
            assert!(!argv.contains(SENTINEL), "{engine} argv: {argv}");
            assert_stdin_carries(engine, &read("stdin"), &prompt);
            let row = cache_get(&env.db, &run_id).await.unwrap().unwrap();
            assert_eq!(row.external_id.as_deref(), Some(session), "{engine}");
            assert_eq!(row.pid, None, "{engine}: ran in-process");

            // A resume: the follow-up goes to stdin too, the session id to argv.
            let follow_up = format!("{SENTINEL} follow-up for {engine}");
            resume_run(&env, &NoInProcessEngines, run_id.clone(), follow_up.clone())
                .await
                .unwrap();
            assert!(
                wait_for(|| async {
                    row_status(&env.db, &run_id).await == "done"
                        && read("stdin").contains("follow-up")
                })
                .await,
                "{engine}: resume finished"
            );
            let argv = read("argv");
            assert!(!argv.contains(SENTINEL), "{engine} resume argv: {argv}");
            let args: Vec<&str> = argv.lines().collect();
            let flag = args.iter().position(|a| *a == resume_flag);
            assert_eq!(
                flag.map(|i| args[i + 1]),
                Some(session),
                "{engine} resumes its session: {argv}"
            );
            assert_stdin_carries(engine, &read("stdin"), &follow_up);
        }
    }
    // No runner conf was ever written: nothing went detached.
    let confs: Vec<_> = std::fs::read_dir(&env.cache_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".conf.json"))
        .collect();
    assert!(confs.is_empty(), "{confs:?}");
}

/// Cancel of an in-process run of each formerly-argv engine: killed, row
/// `cancelled`, the prompt never on its argv, nothing left behind that holds
/// it but the principal's own row and output file.
#[cfg(unix)]
#[tokio::test]
async fn a_cancelled_run_never_had_the_prompt_in_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = stub_engines_env(&root);
    for engine in ["antigravity-cli", "opencode", "pi"] {
        let cwd = root.join(engine);
        std::fs::create_dir_all(&cwd).unwrap();
        let prompt = format!("{SENTINEL} please sleep");
        let res = spawn_run(
            &env,
            &NoInProcessEngines,
            opts(engine, &prompt, Some(cwd.to_string_lossy().as_ref())),
            "cli",
        )
        .await
        .unwrap();
        // The stub has read its stdin (and so recorded its argv) and sleeps.
        assert!(
            wait_for(|| async {
                std::fs::read_to_string(cwd.join("stdin")).is_ok_and(|s| s.contains("sleep"))
            })
            .await,
            "{engine} started"
        );
        let argv = std::fs::read_to_string(cwd.join("argv")).unwrap();
        assert!(!argv.contains(SENTINEL), "{engine} argv: {argv}");
        let cancelled = cancel_run(&env.db, &env.runtime, &env.cache_dir, &res.run_id)
            .await
            .unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert!(
            wait_for(|| async { env.runtime.len().await == 0 }).await,
            "{engine}: the killed run's reader finished"
        );
        assert_eq!(row_status(&env.db, &res.run_id).await, "cancelled");
    }
    // The cache dir is owner-only and holds no runner conf.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&env.cache_dir)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert!(!std::fs::read_dir(&env.cache_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| e.file_name().to_string_lossy().ends_with(".conf.json")));
}

#[test]
fn every_engine_reads_its_prompt_from_stdin() {
    let p = "line one\nit's \"quoted\"";
    let claude: serde_json::Value =
        serde_json::from_str(stdin_payload("claude-code", p).trim_end()).unwrap();
    assert_eq!(
        claude,
        serde_json::json!({"type":"user","message":{"role":"user","content":p}})
    );
    let agy = stdin_payload("antigravity-cli", p);
    assert!(
        agy.ends_with('\n') && agy.matches('\n').count() == 1,
        "one NDJSON line"
    );
    let agy: serde_json::Value = serde_json::from_str(agy.trim_end()).unwrap();
    assert_eq!(
        agy,
        serde_json::json!({"event":"user","message":{"content":p}})
    );
    for engine in ["codex", "opencode", "pi"] {
        assert_eq!(stdin_payload(engine, p), p, "{engine}");
        assert_eq!(prompt_stdin(engine), PromptStdin::Raw);
    }
    // Only stdin-fed engines go to chi-runner (it builds `agy -p <prompt>`).
    assert_eq!(RUNNER_STDIN_ENGINES, ["claude-code", "codex"]);
}

#[test]
fn opencode_json_lines_parse() {
    let line = |s: &str| parse_opencode_line(&serde_json::from_str(s).unwrap());
    assert_eq!(
        line(r#"{"type":"step_start","sessionID":"ses_1","part":{"type":"step-start"}}"#),
        LineEvent {
            session_id: Some("ses_1".into()),
            ..LineEvent::default()
        }
    );
    let text = line(r#"{"type":"text","sessionID":"ses_1","part":{"type":"text","text":"PONG"}}"#);
    assert_eq!(text.text.as_deref(), Some("PONG"));
    assert!(!text.done);
    assert!(line(r#"{"type":"step_finish","sessionID":"s","part":{"reason":"stop"}}"#).done);
    assert!(!line(r#"{"type":"step_finish","sessionID":"s","part":{"reason":"tool-calls"}}"#).done);
    // The shape opencode 1.18 prints for a failed run.
    let err = line(
        r#"{"type":"error","sessionID":"s","error":{"name":"UnknownError","data":{"message":"Unexpected server error."}}}"#,
    );
    assert_eq!(err.error.as_deref(), Some("Unexpected server error."));
    assert_eq!(
        line(r#"{"type":"error","error":{"name":"APIError"}}"#)
            .error
            .as_deref(),
        Some("APIError")
    );
}

#[test]
fn pi_json_lines_parse() {
    let line = |s: &str| parse_pi_line(&serde_json::from_str(s).unwrap());
    assert_eq!(
        line(r#"{"type":"session","version":3,"id":"9f1e","cwd":"/w"}"#)
            .session_id
            .as_deref(),
        Some("9f1e")
    );
    assert_eq!(
        line(r#"{"type":"message_update","message":{},"assistantMessageEvent":{"type":"text_delta","delta":"He"}}"#)
            .text
            .as_deref(),
        Some("He")
    );
    assert_eq!(
        line(
            r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"x"}}"#
        ),
        LineEvent::default()
    );
    let ok = line(
        r#"{"type":"agent_end","messages":[{"role":"user"},{"role":"assistant","stopReason":"stop"}]}"#,
    );
    assert!(ok.done && ok.error.is_none());
    let failed = line(
        r#"{"type":"agent_end","messages":[{"role":"assistant","stopReason":"error","errorMessage":"429 rate limited"}]}"#,
    );
    assert!(failed.done);
    assert_eq!(failed.error.as_deref(), Some("429 rate limited"));
    assert_eq!(
        line(r#"{"type":"agent_end","messages":[{"role":"assistant","stopReason":"aborted"}]}"#)
            .error
            .as_deref(),
        Some("request aborted")
    );
}

/// A persistent run's runner conf carries the prompt: it is written
/// owner-only, a planted run id can't aim it (or its removal) outside the
/// cache dir, and it is deleted once the runner is done with it — when the
/// sweep sees the run end, and on cancel — but kept while the runner runs.
#[cfg(unix)]
#[tokio::test]
async fn runner_conf_is_private_and_removed_when_the_run_ends() {
    use chi_runner::{conf_path, remove_conf, write_private, PidProbe};
    use std::os::unix::fs::PermissionsExt;

    let db = test_db().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

    // Owner-only, even over a world-readable file already there.
    let stale = dir.join("stale.conf.json");
    std::fs::write(&stale, "old").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&stale, SENTINEL.as_bytes()).unwrap();
    assert_eq!(mode(&stale), 0o600);
    assert_eq!(std::fs::read_to_string(&stale).unwrap(), SENTINEL);

    for bad in ["", "../x", "a/b", "..", "x.conf"] {
        assert_eq!(conf_path(dir, bad), None, "{bad:?}");
    }
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let outside = dir.join("x.conf.json");
    std::fs::write(&outside, "not ours").unwrap();
    remove_conf(&sub, "../x");
    assert!(outside.exists(), "a planted run id deletes nothing outside");

    let conf = |id: &str| {
        let p = conf_path(dir, id).unwrap();
        write_private(&p, SENTINEL.as_bytes()).unwrap();
        p
    };
    let ended = conf("ended");
    let alive = conf("alive");
    let cancelled = conf("cancelled");
    detached_row(&db, dir, "ended", Some(3001), Some(r#"{"status":"done"}"#)).await;
    detached_row(
        &db,
        dir,
        "alive",
        Some(3002),
        Some(r#"{"status":"running"}"#),
    )
    .await;
    detached_row(&db, dir, "cancelled", Some(999_999_999), None).await;
    let probe = |pid: u32| match pid {
        3002 => PidProbe::Ours,
        _ => PidProbe::Dead,
    };
    assert_eq!(
        reconcile_detached_runs_with(&db, dir, &probe)
            .await
            .unwrap(),
        2,
        "ended + the dead runner of `cancelled`"
    );
    assert!(!ended.exists(), "the sweep dropped a finished run's conf");
    assert!(alive.exists(), "a live runner keeps its conf");

    // Cancel drops it too (the pid is no runner of ours: nothing signalled).
    write_private(&cancelled, SENTINEL.as_bytes()).unwrap();
    let runtime = ChiRuntime::new();
    cancel_run(&db, &runtime, dir, "cancelled").await.unwrap();
    assert!(!cancelled.exists(), "cancel dropped the conf");
    remove_conf(dir, "alive");
    assert!(!alive.exists());
    remove_conf(dir, "alive"); // already gone: fine
}

/// Operator-secret regression: the daemon expands only `~` in a run's cwd.
/// Before the fix `shellexpand::full` resolved `$VAR` against the daemon's
/// own environment — which carries every `IKENGA_SECRET_*` operator default —
/// and the value went into codex's `--cd` argv and the runner's conf file,
/// handing a Dispatch-only caller what needs Secrets. `$HOME` / `$PATH`
/// stand in for the secret (same mechanism, no process-env mutation).
#[tokio::test]
async fn the_daemon_expands_only_tilde_in_a_run_cwd() {
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
        &cwd,
        None,
        None,
        None,
    ).await
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
    assert_eq!(desktop.run_cwd(Some("$HOME/proj")), format!("{home}/proj"));
}

fn tail_of(lines: &[&str]) -> StderrTail {
    let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    StderrTail(Some(tokio::spawn(async move { lines })))
}

/// A failed run names an infrastructure cause found in the engine's stderr,
/// and only the classified cause — never the raw stderr text.
#[tokio::test]
async fn failed_run_error_names_the_stderr_cause() {
    let err = explain_failure(
        Some("engine child exited without a done envelope"),
        tail_of(&["prompt: secret stuff", "OAuth error: getaddrinfo EAI_AGAIN platform.claude.com"]),
    )
    .await
    .unwrap();
    assert_eq!(
        err,
        "engine child exited without a done envelope — network unreachable from the engine (EAI_AGAIN)"
    );
    assert!(!err.contains("secret"));

    // Nothing recognisable: the base error stands alone.
    let err = explain_failure(Some("codex reported turn.failed"), tail_of(&["boom"])).await;
    assert_eq!(err.as_deref(), Some("codex reported turn.failed"));

    // A run that didn't fail gets no error at all.
    assert_eq!(explain_failure(None, tail_of(&["EAI_AGAIN"])).await, None);
    // No stderr pipe: base error.
    assert_eq!(
        explain_failure(Some("x"), StderrTail(None)).await.as_deref(),
        Some("x")
    );
}
