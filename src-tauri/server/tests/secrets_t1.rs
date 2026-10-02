//! Remote-access WP-21 end to end, as root: the built `ikenga-server
//! --executor-tier t1` broker with real per-principal children. Each
//! principal's `secrets_*` RPCs land in their own store, sealed under a key
//! the broker derived for them from `operator/secrets-kek`; the
//! `IKENGA_SECRET_*` operator default shows through where they have no value
//! of their own (ADR-023 §4, G-PRINCIPAL §5 rows 15–16).
//!
//! Like `broker_t1.rs`, the binary must be executable by the principals' uids,
//! so the CI `t1-root` job runs this from `/opt/t1` with `IKENGA_SERVER_BIN`.

#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const PASSWORD: &str = "correct horse battery";
const RANGE: &str = "28560-28570";

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
            "ikenga-secrets-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct HostUsers(Vec<String>);

impl Drop for HostUsers {
    fn drop(&mut self) {
        for name in &self.0 {
            let _ = Command::new("pkill").args(["-KILL", "-u", name]).output();
            let _ = Command::new("userdel").arg(name).output();
            let _ = Command::new("groupdel").arg(name).output();
        }
    }
}

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

fn accounts(root: &Path, args: &[&str], stdin: &str) -> Vec<u8> {
    let mut child = Command::new(bin())
        .args([
            "accounts",
            "--data-dir",
            root.to_str().unwrap(),
            "--uid-range",
            RANGE,
        ])
        .args(args)
        .env_remove("IKENGA_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
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
    out.stdout
}

fn account_id(root: &Path, username: &str) -> String {
    let list: Value = serde_json::from_slice(&accounts(root, &["list", "--json"], "")).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .find(|a| a["username"] == username)
        .map(|a| a["principal_id"].as_str().unwrap().to_string())
        .unwrap()
}

fn start_broker(tmp: &Path, root: &Path) -> Broker {
    let log = tmp.join("broker.log");
    let child = Command::new(bin())
        .args([
            "--executor-tier",
            "t1",
            "--data-dir",
            root.to_str().unwrap(),
            "--uid-range",
            RANGE,
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
        // The operator default layer (§5 row 15).
        .env("IKENGA_SECRET_SHARED", "op-shared")
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
            panic!("the broker exited ({status}): {}", broker.log());
        }
        assert!(
            Instant::now() < deadline,
            "no daemon.json: {}",
            broker.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    broker
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

async fn login(broker: &Broker, username: &str) -> String {
    let res = http()
        .post(broker.url("/auth/login"))
        .header("content-type", "application/json")
        .body(json!({ "username": username, "password": PASSWORD }).to_string())
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

async fn data(broker: &Broker, cookie: &str, cmd: &str, args: Value) -> Value {
    let v = rpc(broker, cookie, cmd, args).await;
    assert_eq!(v["ok"], true, "{cmd}: {v}\n{}", broker.log());
    v["data"].clone()
}

/// pids of the running `--principal-child` processes.
fn child_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains("--principal-child") {
            pids.push(pid);
        }
    }
    pids
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
#[ignore = "t1-root"]
fn t1_root_secrets_principal_store_over_the_operator_default() {
    assert!(is_root(), "t1-root tests run as root");
    let users = HostUsers(vec!["ik-t1sec-ada".into(), "ik-t1sec-bob".into()]);
    let tmp = TempDir::new("wp21");
    let root = tmp.0.join("root");
    let pw = format!("{PASSWORD}\n");
    accounts(
        &root,
        &["create", "t1sec-ada", "--admin", "--password-stdin"],
        &pw,
    );
    accounts(&root, &["create", "t1sec-bob", "--password-stdin"], &pw);
    let ada_id = account_id(&root, "t1sec-ada");
    let bob_id = account_id(&root, "t1sec-bob");

    let rt = tokio::runtime::Runtime::new().unwrap();
    {
        let broker = start_broker(&tmp.0, &root);
        rt.block_on(async {
            let ada = login(&broker, "t1sec-ada").await;
            let bob = login(&broker, "t1sec-bob").await;

            let status = data(&broker, &ada, "secrets_vault_status", json!({})).await;
            assert_eq!(status["mode"], "principal", "{status}");
            assert_eq!(status["writable"], true);
            assert_eq!(status["available"], true);
            let lock = data(&broker, &ada, "secrets_lock_state", json!({})).await;
            assert_eq!(
                (lock["configured"].clone(), lock["locked"].clone()),
                (json!(true), json!(false))
            );
            let refused = rpc(
                &broker,
                &ada,
                "secrets_unlock",
                json!({ "passphrase": "x" }),
            )
            .await;
            assert_eq!(refused["ok"], false);

            // Both see the operator default; Ada overrides it for herself only.
            for who in [&ada, &bob] {
                let v = data(&broker, who, "secrets_get", json!({ "key": "SHARED" })).await;
                assert_eq!(v, "op-shared");
            }
            data(
                &broker,
                &ada,
                "secrets_set",
                json!({ "key": "SHARED", "value": "ada-own" }),
            )
            .await;
            data(
                &broker,
                &ada,
                "secrets_set",
                json!({ "key": "ADA_ONLY", "value": "a" }),
            )
            .await;
            let p1 = json!({ "kind": "project", "id": "p1" });
            data(
                &broker,
                &ada,
                "secrets_set_scoped",
                json!({ "scope": p1, "key": "TOKEN", "value": "t" }),
            )
            .await;
            assert_eq!(
                data(&broker, &ada, "secrets_get", json!({ "key": "SHARED" })).await,
                "ada-own"
            );
            assert_eq!(
                data(&broker, &bob, "secrets_get", json!({ "key": "SHARED" })).await,
                "op-shared"
            );
            assert_eq!(
                data(&broker, &bob, "secrets_get", json!({ "key": "ADA_ONLY" })).await,
                Value::Null
            );
            assert_eq!(
                data(
                    &broker,
                    &bob,
                    "secrets_list_keys_scoped",
                    json!({ "scope": p1 })
                )
                .await,
                json!([])
            );
            assert_eq!(
                data(
                    &broker,
                    &ada,
                    "secrets_get_scoped",
                    json!({ "scope": p1, "key": "TOKEN" })
                )
                .await,
                "t"
            );
            assert_eq!(
                data(&broker, &bob, "secrets_list_keys", json!({})).await,
                json!(["SHARED"])
            );
        });

        // The KEK is root 0600 in operator/, and no child's environment block
        // holds it, another principal's key, or (after startup) its own.
        let kek_path = root.join("operator/secrets-kek");
        let meta = std::fs::symlink_metadata(&kek_path).unwrap();
        assert_eq!(
            (meta.uid(), meta.gid(), meta.mode() & 0o7777),
            (0, 0, 0o600)
        );
        let text = std::fs::read_to_string(&kek_path).unwrap();
        let kek_hex = text.trim().rsplit(':').next().unwrap().to_string();
        let mut kek = [0u8; 32];
        for (i, b) in kek.iter_mut().enumerate() {
            *b = u8::from_str_radix(&kek_hex[2 * i..2 * i + 2], 16).unwrap();
        }
        let key_hex = |id: &str| {
            let key = ikenga_desktop_lib::secrets::WrapKey::derive(&kek, id).unwrap();
            key.to_env_value().rsplit(':').next().unwrap().to_string()
        };
        let pids = child_pids();
        assert_eq!(pids.len(), 2, "two principal children");
        for pid in pids {
            let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
            let hex = to_hex(&environ);
            let text = String::from_utf8_lossy(&environ);
            assert!(
                !text.contains(&kek_hex) && !hex.contains(&kek_hex),
                "the KEK reached a child"
            );
            for id in [&ada_id, &bob_id] {
                assert!(
                    !text.contains(&key_hex(id)),
                    "a wrapping key outlived startup in {pid}"
                );
            }
        }

        // §4: the store lives in the principal's own 0700 data dir.
        let secrets = root.join("principals").join(&ada_id).join("data/secrets");
        let m = std::fs::symlink_metadata(&secrets).unwrap();
        assert_eq!((m.uid(), m.mode() & 0o7777), (28_560, 0o700));
        let values = std::fs::read_to_string(secrets.join("values.json")).unwrap();
        assert!(!values.contains("ada-own"), "nothing plaintext at rest");
    }

    // A broker restart re-derives the same key from the persisted KEK.
    let broker = start_broker(&tmp.0, &root);
    rt.block_on(async {
        let ada = login(&broker, "t1sec-ada").await;
        assert_eq!(
            data(&broker, &ada, "secrets_get", json!({ "key": "SHARED" })).await,
            "ada-own"
        );
        data(&broker, &ada, "secrets_delete", json!({ "key": "SHARED" })).await;
        assert_eq!(
            data(&broker, &ada, "secrets_get", json!({ "key": "SHARED" })).await,
            "op-shared",
            "deleting her own value reveals the operator default"
        );
    });
    drop(broker);
    drop(users);
}
