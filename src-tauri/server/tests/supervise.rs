//! `ikenga-server supervise`, end to end against the built binary, with no
//! systemd and no root: a detached run started from a server session
//! outlives the server process — a crash and a requested restart — while the
//! supervisor brings the server back, and the run is reaped (not left a
//! zombie) once it exits.
//!
//! The run stands in for chi-runner: it leaves its launching session
//! (`setsid`) and is orphaned when its launcher exits, the same shape a
//! detached runner has. In a container the supervisor is PID 1 and adopts
//! orphans by default; here it is a child subreaper, which is equivalent.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const TOKEN: &str = "supervise-e2e-operator-token-6f1c2b9a4d7e";
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const SIGHUP: i32 = 1;

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn kill(pid: i32, sig: i32) {
    // SAFETY: plain syscall on a pid this test owns.
    unsafe {
        libc_kill(pid, sig);
    }
}

fn bin() -> PathBuf {
    std::env::var_os("IKENGA_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ikenga-server")))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ikenga-supervise-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `(state letter, parent pid)` from `/proc/<pid>/stat`; `None` once the
/// process is gone (reaped).
fn stat(pid: i32) -> Option<(char, i32)> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, rest) = text.rsplit_once(')')?;
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((state, ppid))
}

/// Running (or sleeping): present and not a zombie.
fn alive(pid: i32) -> bool {
    matches!(stat(pid), Some((s, _)) if s != 'Z' && s != 'X')
}

fn ppid(pid: i32) -> Option<i32> {
    stat(pid).map(|(_, p)| p)
}

fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The supervisor process, plus the detached run once it exists. On drop:
/// SIGKILL to both (a passing test has already stopped the supervisor).
struct Supervisor {
    child: Child,
    data: PathBuf,
    log: PathBuf,
    err: PathBuf,
    run: Option<i32>,
}

impl Supervisor {
    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn logs(&self) -> String {
        format!(
            "--- stdout\n{}\n--- stderr\n{}",
            std::fs::read_to_string(&self.log).unwrap_or_default(),
            std::fs::read_to_string(&self.err).unwrap_or_default()
        )
    }

    /// The server the supervisor is running now — `(pid, port)` from the
    /// discovery file it writes once bound — skipping `previous`.
    fn wait_server(&mut self, previous: Option<i32>) -> (i32, u16) {
        let meta = self.data.join("daemon.json");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("the supervisor exited ({status}):\n{}", self.logs());
            }
            if let Some(v) = std::fs::read_to_string(&meta)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                let pid = v["pid"].as_i64().unwrap_or(0) as i32;
                let port = v["port"].as_u64().unwrap_or(0) as u16;
                if pid > 0 && Some(pid) != previous && alive(pid) {
                    assert_eq!(
                        ppid(pid),
                        Some(self.pid()),
                        "the server is the supervisor's own child"
                    );
                    return (pid, port);
                }
            }
            assert!(
                Instant::now() < deadline,
                "no new server came up:\n{}",
                self.logs()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(run) = self.run {
            kill(run, SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn rpc(port: u16, cmd: &str, args: Value) -> Value {
    let res = http()
        .post(format!("http://127.0.0.1:{port}/api/rpc"))
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("content-type", "application/json")
        .body(json!({ "cmd": cmd, "args": args }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "{cmd}");
    serde_json::from_str(&res.text().await.unwrap()).unwrap()
}

async fn health(port: u16) -> u16 {
    http()
        .get(format!("http://127.0.0.1:{port}/api/health"))
        .send()
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0)
}

fn read_pid(path: &Path) -> i32 {
    let mut pid = 0;
    wait_until(
        "the detached run wrote its pid",
        Duration::from_secs(20),
        || {
            pid = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            pid > 0
        },
    );
    pid
}

#[test]
fn a_detached_run_survives_server_restarts_under_supervise() {
    let tmp = TempDir::new();
    let data = tmp.0.join("data");
    let home = tmp.0.join("home");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let log = tmp.0.join("supervise.log");
    let err = tmp.0.join("supervise.err");

    let child = Command::new(bin())
        .args(["supervise", "--"])
        .args(["--host", "127.0.0.1", "--port", "0", "--data-dir"])
        .arg(&data)
        .arg("--static-dir")
        .arg(tmp.0.join("no-spa"))
        .env_remove("IKENGA_DATA_DIR")
        .env_remove("IKENGA_EXECUTOR_TIER")
        .env_remove("IKENGA_IDLE_TIMEOUT")
        .env("IKENGA_AUTH_TOKEN", TOKEN)
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &tmp.0)
        .env("RUST_LOG", "info")
        .stdout(File::create(&log).unwrap())
        .stderr(File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let mut sup = Supervisor {
        child,
        data: data.clone(),
        log,
        err,
        run: None,
    };
    let sup_pid = sup.pid();
    let rt = tokio::runtime::Runtime::new().unwrap();

    let (first, port) = sup.wait_server(None);
    assert_eq!(rt.block_on(health(port)), 200);

    // A detached run, launched from a server terminal: it leaves the
    // terminal's session, reports its pid, and its launcher exits.
    let pid_file = tmp.0.join("run.pid");
    let script = format!(
        "setsid sh -c 'echo $$ > {p}.tmp && mv {p}.tmp {p} && exec sleep 600' \
         </dev/null >/dev/null 2>&1 & \
         while [ ! -s {p} ]; do sleep 0.1; done",
        p = pid_file.display()
    );
    let spawned = rt.block_on(rpc(
        port,
        "pty_spawn",
        json!({ "terminal_id": "supervise-e2e", "cmd": ["/bin/sh", "-c", script] }),
    ));
    assert_eq!(spawned["ok"], true, "{spawned}");
    let run = read_pid(&pid_file);
    sup.run = Some(run);
    assert!(alive(run), "the run started");
    // Orphaned: the supervisor adopts it (PID 1's job in a container).
    wait_until(
        "the orphaned run re-parents to the supervisor",
        Duration::from_secs(20),
        || ppid(run) == Some(sup_pid),
    );

    // 1. The server crashes. The supervisor starts a new one; the run lives.
    kill(first, SIGKILL);
    let (second, port) = sup.wait_server(Some(first));
    assert!(alive(run), "the run survived a server crash");
    assert_eq!(rt.block_on(health(port)), 200, "{}", sup.logs());

    // 2. A requested restart: SIGHUP restarts only the server.
    kill(sup_pid, SIGHUP);
    let (third, port) = sup.wait_server(Some(second));
    assert!(!alive(second), "the old server is gone");
    assert!(alive(run), "the run survived a requested restart");
    assert_eq!(ppid(run), Some(sup_pid));
    assert_eq!(rt.block_on(health(port)), 200, "{}", sup.logs());

    // 3. The run finishes: the supervisor reaps it, so it does not linger
    //    as a zombie (which a pid-liveness probe would have to special-case).
    kill(run, SIGTERM);
    wait_until(
        "the finished run is reaped",
        Duration::from_secs(10),
        || stat(run).is_none(),
    );
    sup.run = None;

    // 4. SIGTERM stops the server, then the supervisor, cleanly.
    kill(sup_pid, SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = sup.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the supervisor did not stop:\n{}",
            sup.logs()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "{status}:\n{}", sup.logs());
    assert!(!alive(third), "the server stopped with the supervisor");
}
