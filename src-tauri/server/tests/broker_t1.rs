//! The built `ikenga-server --executor-tier t1` broker, end to end, as root
//! (G-PRINCIPAL §3 topology B, §11.1 I-7 and I-8). Accounts come from the
//! real `accounts` CLI; children are real `--principal-child` processes
//! launched through the T1 executor as each principal's uid.
//!
//! The binary must be executable by the principals' uids, so the CI
//! `t1-root` job runs these from `/opt/t1` with `IKENGA_SERVER_BIN`.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite;

const PASSWORD: &str = "correct horse battery";

fn bin() -> PathBuf {
    std::env::var_os("IKENGA_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ikenga-server")))
}

fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(2).map(|euid| euid == "0"))
        })
        .unwrap_or(false)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ikenga-broker-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Principals must traverse to their own dirs under the operator root.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Removes the host users the test's accounts created.
struct HostUsers(Vec<String>);

impl Drop for HostUsers {
    fn drop(&mut self) {
        for name in &self.0 {
            // Anything still running as the uid would block userdel.
            let _ = Command::new("pkill").args(["-KILL", "-u", name]).output();
            let _ = Command::new("userdel").arg(name).output();
            let _ = Command::new("groupdel").arg(name).output();
        }
    }
}

/// The broker process; SIGTERM + wait on drop.
struct Broker {
    child: Child,
    port: u16,
    log: PathBuf,
}

impl Broker {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        // SAFETY: plain syscall on our own child.
        unsafe {
            libc_kill(self.child.id() as i32, 15);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

fn accounts(root: &Path, range: &str, args: &[&str], stdin: &str) {
    let mut cmd = Command::new(bin());
    cmd.args([
        "accounts",
        "--data-dir",
        root.to_str().unwrap(),
        "--uid-range",
        range,
    ])
    .args(args)
    .env_remove("IKENGA_DATA_DIR")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "accounts {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn start_broker(tmp: &Path, root: &Path, range: &str) -> Broker {
    start_broker_with_env(tmp, root, range, &[])
}

/// [`start_broker`] with extra environment for the broker process.
fn start_broker_with_env(tmp: &Path, root: &Path, range: &str, env: &[(&str, &str)]) -> Broker {
    let log = tmp.join("broker.log");
    let child = Command::new(bin())
        .args([
            "--executor-tier",
            "t1",
            "--data-dir",
            root.to_str().unwrap(),
            "--uid-range",
            range,
            "--host",
            "127.0.0.1",
            "--port",
            "0",
            "--insecure-cookie",
            "--static-dir",
            tmp.join("no-spa").to_str().unwrap(),
        ])
        .env_remove("IKENGA_DATA_DIR")
        .env_remove("IKENGA_AUTH_TOKEN")
        .env("RUST_LOG", "info")
        .envs(env.iter().copied())
        .stdout(File::create(&log).unwrap())
        .stderr(File::create(tmp.join("broker.err")).unwrap())
        .spawn()
        .unwrap();
    let mut broker = Broker {
        child,
        port: 0,
        log,
    };
    let meta = root.join("operator/daemon.json");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(text) = std::fs::read_to_string(&meta) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                if v["pid"] == json!(broker.child.id()) {
                    broker.port = v["port"].as_u64().unwrap() as u16;
                    break;
                }
            }
        }
        if let Ok(Some(status)) = broker.child.try_wait() {
            panic!(
                "the broker exited ({status}): {}\n{}",
                broker.log(),
                std::fs::read_to_string(tmp.join("broker.err")).unwrap_or_default()
            );
        }
        assert!(
            Instant::now() < deadline,
            "no operator/daemon.json: {}",
            broker.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !root.join("daemon.json").exists(),
        "<root>/daemon.json is a T0 marker; the broker writes operator/daemon.json"
    );
    broker
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn login(broker: &Broker, username: &str, password: &str) -> String {
    let res = http()
        .post(broker.url("/auth/login"))
        .header("content-type", "application/json")
        .body(json!({ "username": username, "password": password }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 204, "login {username}");
    res.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("ikenga_session="))
        .map(|v| v.split(';').next().unwrap().to_string())
        .expect("a session cookie")
}

async fn rpc(broker: &Broker, cookie: &str, cmd: &str, args: Value) -> Value {
    let res = http()
        .post(broker.url("/api/rpc"))
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .body(json!({ "cmd": cmd, "args": args }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "{cmd}: {}", broker.log());
    serde_json::from_str(&res.text().await.unwrap()).unwrap()
}

async fn get_json(broker: &Broker, path: &str, cookie: Option<&str>) -> (u16, Value) {
    let mut req = http().get(broker.url(path));
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    let res = req.send().await.unwrap();
    let status = res.status().as_u16();
    (
        status,
        serde_json::from_str(&res.text().await.unwrap()).unwrap_or(Value::Null),
    )
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws(broker: &Broker, path: &str, cookie: &str) -> Ws {
    use tungstenite::client::IntoClientRequest;
    let mut req = format!("ws://127.0.0.1:{}{path}", broker.port)
        .into_client_request()
        .unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    req.headers_mut().insert(
        "origin",
        format!("http://127.0.0.1:{}", broker.port).parse().unwrap(),
    );
    tokio_tungstenite::connect_async(req)
        .await
        .unwrap_or_else(|e| panic!("ws {path}: {e}\n{}", broker.log()))
        .0
}

/// uids of the running `--principal-child` processes.
fn child_uids() -> Vec<u32> {
    let mut uids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if !String::from_utf8_lossy(&cmdline).contains("--principal-child") {
            continue;
        }
        if let Ok(meta) = std::fs::metadata(entry.path()) {
            uids.push(meta.uid());
        }
    }
    uids.sort_unstable();
    uids
}

fn account_id(root: &Path, range: &str, username: &str) -> String {
    let out = Command::new(bin())
        .args([
            "accounts",
            "--data-dir",
            root.to_str().unwrap(),
            "--uid-range",
            range,
        ])
        .args(["list", "--json"])
        .env_remove("IKENGA_DATA_DIR")
        .output()
        .unwrap();
    let list: Value = serde_json::from_slice(&out.stdout).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == username)
        .map(|a| a["principal_id"].as_str().unwrap().to_string())
        .unwrap()
}

/// I-7 (and §3): two principals, two children, two uids. Neither can see,
/// attach to or read the other's PTY or files; the T1 health reports a
/// verified probe; `?token=` grants nothing.
#[test]
#[ignore = "t1-root"]
fn t1_root_broker_i7_two_principals_are_isolated() {
    assert!(is_root(), "t1-root tests run as root");
    let users = HostUsers(vec!["ik-t1brk-ada".into(), "ik-t1brk-bob".into()]);
    let tmp = TempDir::new("i7");
    let root = tmp.0.join("root");
    let range = "28200-28210";
    accounts(
        &root,
        range,
        &["create", "t1brk-ada", "--admin", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    accounts(
        &root,
        range,
        &["create", "t1brk-bob", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    let broker = start_broker(&tmp.0, &root, range);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (status, health) = get_json(&broker, "/api/health", None).await;
        assert_eq!(status, 200);
        assert_eq!(health["executor"]["tier"], "t1");
        assert_eq!(health["executor"]["principal_isolation"], true, "{health}");
        assert_eq!(health["probe"]["ok"], true);

        // I-6 end to end: nothing without a session.
        let (status, _) = get_json(&broker, "/auth/me", None).await;
        assert_eq!(status, 401);
        let res = http()
            .post(broker.url("/api/rpc?token=x"))
            .header("authorization", "Bearer x")
            .body(r#"{"cmd":"pty_list"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);

        let ada = login(&broker, "t1brk-ada", PASSWORD).await;
        let bob = login(&broker, "t1brk-bob", PASSWORD).await;
        let (_, me) = get_json(&broker, "/auth/me", Some(&ada)).await;
        assert_eq!(me["username"], "t1brk-ada");
        assert_eq!(me["is_admin"], true);

        // Each principal's RPC runs in its own child, as its own user.
        let who_a = rpc(&broker, &ada, "os_username", json!({})).await;
        let who_b = rpc(&broker, &bob, "os_username", json!({})).await;
        assert_eq!(who_a["data"], "ik-t1brk-ada", "{who_a}");
        assert_eq!(who_b["data"], "ik-t1brk-bob", "{who_b}");
        assert_eq!(child_uids(), vec![28_200, 28_201]);

        // Ada's PTY is invisible to Bob, by id and by attach.
        let spawned = rpc(
            &broker,
            &ada,
            "pty_spawn",
            json!({ "terminal_id": "ada-term", "cmd": ["/bin/sh", "-c", "sleep 120"] }),
        )
        .await;
        let pty_id = spawned["data"]["pty_id"].as_str().unwrap_or_else(|| panic!("{spawned}")).to_string();
        let ada_list = rpc(&broker, &ada, "pty_list", json!({})).await.to_string();
        let bob_list = rpc(&broker, &bob, "pty_list", json!({})).await.to_string();
        assert!(ada_list.contains(&pty_id), "{ada_list}");
        assert!(!bob_list.contains(&pty_id) && !bob_list.contains("ada-term"), "{bob_list}");
        let mut attach = ws(&broker, &format!("/ws/pty/{pty_id}"), &bob).await;
        let first = tokio::time::timeout(Duration::from_secs(10), attach.next())
            .await
            .expect("a frame")
            .expect("a frame")
            .unwrap();
        assert!(
            matches!(&first, tungstenite::Message::Text(t) if t.contains("ikenga.gone")),
            "{first:?}"
        );

        // Files: the kernel keeps Bob's child out of Ada's data (0700, uid).
        let ada_id = account_id(&root, range, "t1brk-ada");
        let ada_data = root.join("principals").join(&ada_id).join("data");
        let meta = std::fs::metadata(&ada_data).unwrap();
        assert_eq!((meta.uid(), meta.mode() & 0o777), (28_200, 0o700), "I-9");
        let peek = rpc(
            &broker,
            &bob,
            "pty_spawn",
            json!({
                "terminal_id": "bob-peek",
                "cmd": ["/bin/sh", "-c", format!("ls {} > /dev/null 2>&1; echo rc=$? > $HOME/peek", ada_data.display())],
            }),
        )
        .await;
        assert_eq!(peek["ok"], true, "{peek}");
        let bob_id = account_id(&root, range, "t1brk-bob");
        let peek_file = root.join("principals").join(&bob_id).join("home/peek");
        let deadline = Instant::now() + Duration::from_secs(10);
        let rc = loop {
            if let Ok(s) = std::fs::read_to_string(&peek_file) {
                if !s.is_empty() {
                    break s;
                }
            }
            assert!(Instant::now() < deadline, "bob's probe never ran");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_ne!(rc.trim(), "rc=0", "bob listed ada's data dir");
    });
    drop(broker);
    // Stopping the broker stops its children.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !child_uids().is_empty() {
        assert!(Instant::now() < deadline, "children outlived the broker");
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(users);
}

/// I-8: open a proxied WebSocket, run `accounts passwd` from the CLI (a
/// different process), and the socket is closed with 4401 within 2 s; the old
/// session is dead on its next request; the new password works.
#[test]
#[ignore = "t1-root"]
fn t1_root_broker_i8_cli_passwd_closes_a_proxied_socket_within_two_seconds() {
    assert!(is_root(), "t1-root tests run as root");
    let users = HostUsers(vec!["ik-t1brk-eve".into()]);
    let tmp = TempDir::new("i8");
    let root = tmp.0.join("root");
    let range = "28220-28230";
    accounts(
        &root,
        range,
        &["create", "t1brk-eve", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    let broker = start_broker(&tmp.0, &root, range);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let cookie = login(&broker, "t1brk-eve", PASSWORD).await;
        let mut socket = ws(&broker, "/ws/fs", &cookie).await;
        // The socket is live through the broker.
        socket
            .send(tungstenite::Message::Ping(b"hi".to_vec()))
            .await
            .unwrap();

        let started = Instant::now();
        accounts(
            &root,
            range,
            &["passwd", "t1brk-eve", "--password-stdin"],
            "a different long password\n",
        );
        let cli_done = Instant::now();
        let code = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(msg) = socket.next().await {
                match msg {
                    Ok(tungstenite::Message::Close(frame)) => {
                        return frame.map(|f| u16::from(f.code))
                    }
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
            None
        })
        .await
        .expect("closed");
        assert_eq!(code, Some(4401), "{}", broker.log());
        assert!(
            cli_done.elapsed() < Duration::from_secs(2),
            "closed {:?} after the CLI's write (started {:?} ago)",
            cli_done.elapsed(),
            started.elapsed()
        );

        let (status, _) = get_json(&broker, "/auth/me", Some(&cookie)).await;
        assert_eq!(status, 401, "the old session is revoked");
        let fresh = login(&broker, "t1brk-eve", "a different long password").await;
        let (status, _) = get_json(&broker, "/auth/me", Some(&fresh)).await;
        assert_eq!(status, 200);
    });
    drop(broker);
    drop(users);
}

/// A stub `claude` (stream-json) in `home/.local/bin`, owned by the
/// principal: it reports `uid=<its uid>`, or `exec sleep 60` when the prompt
/// says `sleep`. The child's augmented PATH includes `$HOME/.local/bin`.
fn install_stub_claude(home: &Path) {
    let meta = std::fs::metadata(home).unwrap();
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("claude");
    std::fs::write(
        &path,
        r#"#!/bin/sh
prompt=$(cat)
case "$prompt" in *sleep*) exec sleep 60;; esac
printf '%s\n' '{"type":"system","subtype":"init","session_id":"sess-t1"}'
printf '{"type":"assistant","message":{"content":[{"type":"text","text":"uid=%s"}]}}\n' "$(id -u)"
printf '%s\n' '{"type":"result","subtype":"success","stop_reason":"end_turn"}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for p in [home.join(".local"), bin, path] {
        std::os::unix::fs::chown(&p, Some(meta.uid()), Some(meta.gid())).unwrap();
    }
}

/// WP-P10: a Chi run requested through the broker executes in the signed-in
/// principal's child, as that principal's uid (never root), with its output
/// in that principal's own 0700 data dir; another principal's run id is
/// "not found" for status, resume and cancel, and leaves the run alone.
#[test]
#[ignore = "t1-root"]
fn t1_root_broker_chi_runs_are_per_principal() {
    assert!(is_root(), "t1-root tests run as root");
    let users = HostUsers(vec!["ik-t1chi-ada".into(), "ik-t1chi-bob".into()]);
    let tmp = TempDir::new("chi");
    let root = tmp.0.join("root");
    let range = "28220-28230";
    accounts(
        &root,
        range,
        &["create", "t1chi-ada", "--admin", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    accounts(
        &root,
        range,
        &["create", "t1chi-bob", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    let ada_id = account_id(&root, range, "t1chi-ada");
    let ada_home = root.join("principals").join(&ada_id).join("home");
    let broker = start_broker(&tmp.0, &root, range);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let ada = login(&broker, "t1chi-ada", PASSWORD).await;
        let bob = login(&broker, "t1chi-bob", PASSWORD).await;
        // Both children up (and so both homes provisioned) before the stub
        // goes into Ada's.
        for who in [&ada, &bob] {
            let r = rpc(&broker, who, "os_username", json!({})).await;
            assert_eq!(r["ok"], true, "{r}");
        }
        let ada_uid = std::fs::metadata(&ada_home).unwrap().uid();
        assert_ne!(ada_uid, 0);
        install_stub_claude(&ada_home);

        let started = rpc(
            &broker,
            &ada,
            "chi_run",
            json!({ "opts": { "engineId": "claude-code", "prompt": "hello" } }),
        )
        .await;
        assert_eq!(started["ok"], true, "{started}");
        let run_id = started["data"]["run_id"].as_str().unwrap().to_string();
        let deadline = Instant::now() + Duration::from_secs(20);
        let done = loop {
            let s = rpc(&broker, &ada, "chi_status", json!({ "runId": run_id })).await;
            if s["data"]["status"] == "done" {
                break s;
            }
            assert!(Instant::now() < deadline, "the run never finished: {s}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(
            done["data"]["output"],
            format!("uid={ada_uid}"),
            "the engine ran as the principal: {done}"
        );
        let cache = root.join("principals").join(&ada_id).join("data/chi-cache");
        let meta = std::fs::metadata(&cache).unwrap();
        assert_eq!(meta.uid(), ada_uid, "the chi cache is the principal's");

        // Bob cannot read, resume or cancel it — it does not exist for him.
        let not_found = format!("chi run not found: {run_id}");
        for (cmd, args) in [
            ("chi_status", json!({ "runId": run_id })),
            ("chi_resume", json!({ "runId": run_id, "prompt": "x" })),
            ("chi_cancel", json!({ "runId": run_id })),
        ] {
            let r = rpc(&broker, &bob, cmd, args).await;
            assert_eq!(r["ok"], false, "{cmd}: {r}");
            assert_eq!(r["error"], format!("{cmd}: {not_found}"), "{cmd}");
        }
        let still = rpc(&broker, &ada, "chi_status", json!({ "runId": run_id })).await;
        assert_eq!(still["data"]["status"], "done", "{still}");

        // A live run: Bob's cancel misses, Ada's lands.
        let sleeper = rpc(
            &broker,
            &ada,
            "chi_run",
            json!({ "opts": { "engineId": "claude-code", "prompt": "please sleep" } }),
        )
        .await;
        let sleeper_id = sleeper["data"]["run_id"].as_str().unwrap().to_string();
        let miss = rpc(&broker, &bob, "chi_cancel", json!({ "runId": sleeper_id })).await;
        assert_eq!(miss["ok"], false, "{miss}");
        let hit = rpc(&broker, &ada, "chi_cancel", json!({ "runId": sleeper_id })).await;
        assert_eq!(hit["data"]["status"], "cancelled", "{hit}");

        let (_, health) = get_json(&broker, "/api/health", None).await;
        assert_eq!(health["executor"]["principal_isolation"], true, "{health}");
    });
    drop(broker);
    drop(users);
}

/// An executable owned by the principal in `home/.local/bin` (on the
/// child's PATH under T1).
fn install_principal_stub(home: &Path, name: &str, script: &str) {
    let meta = std::fs::metadata(home).unwrap();
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for p in [home.join(".local"), bin, path] {
        std::os::unix::fs::chown(&p, Some(meta.uid()), Some(meta.gid())).unwrap();
    }
}

/// WP-P10 review regressions, on a real broker:
///
/// - **I-7, prompt in argv.** No engine gets the prompt on its command
///   line, where every uid can read it from `/proc/<pid>/cmdline` unless the
///   *reader's* procfs is `hidepid` (an SSH session uses the host `/proc`, so
///   the unit's `ProtectProc=invisible` proves nothing). `pi`, which used to
///   run as `pi -p <prompt>`, now runs on any `/proc` and reads the prompt
///   from stdin.
/// - **Operator secrets via cwd.** A `cwd` of `$IKENGA_SECRET_DEMO_KEY` is
///   not expanded against the principal child's environment (which holds
///   the operator default): codex gets the literal string as `--cd`.
#[test]
#[ignore = "t1-root"]
fn t1_root_broker_chi_prompts_and_operator_secrets_stay_private() {
    assert!(is_root(), "t1-root tests run as root");
    const SECRET: &str = "operator-default-sk-DEMO123";
    const PROMPT: &str = "ADA-PRIVATE: payroll export for Q3";
    let users = HostUsers(vec!["ik-t1chp-ada".into()]);
    let tmp = TempDir::new("chi-private");
    let root = tmp.0.join("root");
    let range = "28240-28250";
    accounts(
        &root,
        range,
        &["create", "t1chp-ada", "--admin", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    let ada_id = account_id(&root, range, "t1chp-ada");
    let ada_home = root.join("principals").join(&ada_id).join("home");
    let broker = start_broker_with_env(&tmp.0, &root, range, &[("IKENGA_SECRET_DEMO_KEY", SECRET)]);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let ada = login(&broker, "t1chp-ada", PASSWORD).await;
        let r = rpc(&broker, &ada, "os_username", json!({})).await;
        assert_eq!(r["ok"], true, "{r}");
        install_principal_stub(
            &ada_home,
            "pi",
            r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/pi-argv.txt.tmp"
cat > "$HOME/pi-stdin.txt"
mv "$HOME/pi-argv.txt.tmp" "$HOME/pi-argv.txt"
echo '{"type":"session","version":3,"id":"pi-t1"}'
echo '{"type":"agent_end","messages":[{"role":"assistant","stopReason":"stop"}]}'
"#,
        );
        install_principal_stub(
            &ada_home,
            "codex",
            r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/codex-argv.txt.tmp"
mv "$HOME/codex-argv.txt.tmp" "$HOME/codex-argv.txt"
cat >/dev/null
echo '{"type":"thread.started","thread_id":"t1"}'
echo '{"type":"turn.completed","usage":{}}'
"#,
        );

        // I-7: the engine that used to take `-p <prompt>` runs, prompt on stdin.
        let run = rpc(
            &broker,
            &ada,
            "chi_run",
            json!({ "opts": { "engineId": "pi", "prompt": PROMPT } }),
        )
        .await;
        assert_eq!(run["ok"], true, "{run}");
        let pi_argv = ada_home.join("pi-argv.txt");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !pi_argv.exists() {
            assert!(Instant::now() < deadline, "pi never ran: {}", broker.log());
            std::thread::sleep(Duration::from_millis(100));
        }
        let argv = std::fs::read_to_string(&pi_argv).unwrap();
        assert!(!argv.contains("ADA-PRIVATE"), "prompt in pi's argv: {argv}");
        assert_eq!(argv.lines().collect::<Vec<_>>(), ["--mode", "json"]);
        assert_eq!(
            std::fs::read_to_string(ada_home.join("pi-stdin.txt")).unwrap(),
            PROMPT
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let list = rpc(&broker, &ada, "chi_list", json!({ "engineId": "pi" })).await;
            if list["data"][0]["status"] == "done" {
                assert_eq!(list["data"][0]["external_id"], "pi-t1", "{list}");
                break;
            }
            assert!(Instant::now() < deadline, "pi run never finished: {list}");
            std::thread::sleep(Duration::from_millis(100));
        }

        // Operator secret: the cwd stays literal all the way into the argv.
        let run = rpc(
            &broker,
            &ada,
            "chi_run",
            json!({ "opts": {
                "engineId": "codex", "prompt": "x", "cwd": "$IKENGA_SECRET_DEMO_KEY",
            } }),
        )
        .await;
        assert_eq!(run["ok"], true, "{run}");
        let argv_file = ada_home.join("codex-argv.txt");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !argv_file.exists() {
            assert!(
                Instant::now() < deadline,
                "codex never ran: {}",
                broker.log()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let argv = std::fs::read_to_string(&argv_file).unwrap();
        assert!(
            !argv.contains(SECRET),
            "the operator secret reached the engine argv"
        );
        let args: Vec<&str> = argv.lines().collect();
        let cd = args.iter().position(|a| *a == "--cd").expect("--cd");
        assert_eq!(args[cd + 1], "$IKENGA_SECRET_DEMO_KEY");
    });
    drop(broker);
    drop(users);
}
