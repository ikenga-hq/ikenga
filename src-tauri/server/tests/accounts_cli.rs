//! The built `ikenga-server accounts …` binary under verbose logging (WP-20
//! review F2): log events from other threads must never block the command,
//! and they go to stderr so stdout stays the command's output.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_ikenga-server");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ikenga-server-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

/// Run the binary with `RUST_LOG=trace`, feeding `stdin`, failing the test
/// if it hasn't exited within `timeout` (the F2 deadlock hung forever).
fn run(scratch: &Path, args: &[&str], stdin: &str, timeout: Duration) -> Run {
    let out_path = scratch.join("stdout");
    let err_path = scratch.join("stderr");
    let mut child = Command::new(BIN)
        .args(args)
        .env("RUST_LOG", "trace")
        .env_remove("IKENGA_DATA_DIR")
        .stdin(Stdio::piped())
        // Files, not pipes: trace output can exceed a pipe buffer.
        .stdout(File::create(&out_path).unwrap())
        .stderr(File::create(&err_path).unwrap())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "`ikenga-server {}` hung under RUST_LOG=trace",
                args.join(" ")
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    Run {
        status,
        stdout: std::fs::read_to_string(out_path).unwrap(),
        stderr: std::fs::read_to_string(err_path).unwrap(),
    }
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

/// Any user: a refused subcommand exits promptly, prints nothing on stdout,
/// and — review F10 — a read-only command never creates an operator root.
#[test]
fn a_refused_subcommand_logs_to_stderr_and_creates_nothing() {
    let tmp = TempDir::new("refused");
    let missing = tmp.0.join("typo");
    let run = run(
        &tmp.0,
        &[
            "accounts",
            "--data-dir",
            missing.to_str().unwrap(),
            "list",
            "--json",
        ],
        "",
        Duration::from_secs(60),
    );
    assert!(!run.status.success());
    assert_eq!(run.stdout, "", "stdout is the command's output only");
    assert!(!run.stderr.is_empty());
    assert!(
        !missing.exists(),
        "list must not create {}",
        missing.display()
    );
}

/// Root only: a real create and `list --json` under trace logging, which
/// makes sqlx log from its worker threads.
#[test]
#[ignore = "t1-root"]
fn t1_root_accounts_cli_under_trace_logging() {
    assert!(is_root(), "t1-root tests must run as root");
    struct HostUser;
    impl Drop for HostUser {
        fn drop(&mut self) {
            let _ = Command::new("userdel").arg("ik-t1root-log").output();
            let _ = Command::new("groupdel").arg("ik-t1root-log").output();
        }
    }
    let _cleanup = HostUser;
    let tmp = TempDir::new("trace");
    let root = tmp.0.join("root");
    let root = root.to_str().unwrap();
    let common = ["accounts", "--data-dir", root, "--uid-range", "28100-28110"];

    let mut create = common.to_vec();
    create.extend(["create", "t1root-log", "--admin", "--password-stdin"]);
    let run1 = run(
        &tmp.0,
        &create,
        "correct horse battery\n",
        Duration::from_secs(120),
    );
    assert!(run1.status.success(), "{}", run1.stderr);
    assert!(
        run1.stdout.starts_with("created admin account t1root-log")
            && run1.stdout.lines().count() == 1,
        "{}",
        run1.stdout
    );
    assert!(run1.stderr.contains("sqlx"), "trace logs reach stderr");

    let mut list = common.to_vec();
    list.extend(["list", "--json"]);
    let run2 = run(&tmp.0, &list, "", Duration::from_secs(60));
    assert!(run2.status.success(), "{}", run2.stderr);
    let json = run2.stdout.trim();
    assert!(json.starts_with('[') && json.ends_with(']'), "{json}");
    assert!(json.contains("\"username\": \"t1root-log\""), "{json}");
}
