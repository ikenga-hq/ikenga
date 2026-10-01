//! The built `ikenga-server probe --executor-tier t1` (G-PRINCIPAL §8) and
//! the T1 boot. Exit 0 only when the host can run T1; 1 otherwise.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

/// The binary under test: cargo's, unless `IKENGA_SERVER_BIN` names a copy
/// (the CI `t1-root` job runs these test binaries in a container with the
/// server installed at `/opt/t1/ikenga-server`).
fn bin() -> PathBuf {
    std::env::var_os("IKENGA_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ikenga-server")))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ikenga-probe-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The probe uid must be able to traverse to the step-6 scaffold.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn probe(args: &[&str]) -> Output {
    Command::new(bin())
        .arg("probe")
        .args(args)
        .env_remove("IKENGA_DATA_DIR")
        .env_remove("IKENGA_UID_RANGE")
        .output()
        .unwrap()
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

/// Any user: without an operator root (or without root) the probe fails —
/// exit 1, a parseable JSON report naming the failing check, nothing created.
#[test]
fn the_probe_fails_closed_without_root_or_an_operator_root() {
    let out = probe(&["--executor-tier", "t1", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    // No serde_json in this crate: the pretty report's lines are enough.
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.trim_start().starts_with('{'), "{report}");
    assert!(report.contains("\"tier\": \"t1\""), "{report}");
    assert!(report.contains("\n  \"ok\": false"), "{report}");
    assert_eq!(
        report.matches("\"ok\": false").count(),
        2,
        "one failing check: {report}"
    );
    if !is_root() {
        assert!(
            report.contains("\"check\": \"identity\",\n      \"ok\": false"),
            "{report}"
        );
    }

    // Text form, and a bad --uid-range, exit 1 too.
    let out = probe(&["--executor-tier", "t1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL"));
    let out = probe(&["--executor-tier", "t1", "--uid-range", "5-5"]);
    assert_eq!(out.status.code(), Some(1));
}

/// Root: a fresh operator root passes the read-only probe and is left
/// uncreated; the T1 boot then probes (creating the root, writing
/// `probe.json`) and serves the broker — and boots again on the same root
/// (its discovery file is `operator/daemon.json`, never the T0 marker
/// `<root>/daemon.json`, slice 1's trap).
#[test]
#[ignore = "t1-root"]
fn t1_root_probe_passes_read_only_and_the_boot_serves_after_probing() {
    assert!(is_root(), "t1-root tests run as root");
    let tmp = TempDir::new("pass");
    let data = tmp.0.join("root");
    let data_s = data.to_str().unwrap();
    let range = ["--uid-range", "28800-28810"];

    let out = probe(&[
        "--executor-tier",
        "t1",
        "--data-dir",
        data_s,
        "--json",
        range[0],
        range[1],
    ]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(report.contains("\n  \"ok\": true"), "{report}");
    assert!(!report.contains("\"ok\": false"), "{report}");
    assert!(report.contains("\"probe_uid\": 28810"), "{report}");
    assert!(!data.exists(), "read-only creates nothing");

    for boot in 1..=2 {
        let mut broker = Command::new(bin())
            .args(["--executor-tier", "t1", "--data-dir", data_s, "--port", "0"])
            .args(range)
            .env_remove("IKENGA_DATA_DIR")
            .env("RUST_BACKTRACE", "0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let meta = data.join("operator/daemon.json");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let port = loop {
            if let Ok(text) = std::fs::read_to_string(&meta) {
                if text.contains(&format!("\"pid\":{}", broker.id())) {
                    let port = text
                        .split("\"port\":")
                        .nth(1)
                        .and_then(|r| r.split([',', '}']).next())
                        .and_then(|p| p.trim().parse::<u16>().ok())
                        .unwrap();
                    break port;
                }
            }
            if let Some(status) = broker.try_wait().unwrap() {
                panic!("boot {boot}: the broker exited: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "boot {boot}: never ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        assert!(port > 0);
        assert!(
            !data.join("daemon.json").exists(),
            "no T0 marker at the root"
        );
        // SIGTERM (a syscall: the CI image has no `kill` binary), then a
        // bounded wait — the broker never idles out on its own.
        // SAFETY: plain syscall on our own, unreaped child.
        assert_eq!(unsafe { libc_kill(broker.id() as i32, 15) }, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let status = loop {
            if let Some(status) = broker.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = broker.kill();
                let _ = broker.wait();
                panic!("boot {boot}: the broker ignored SIGTERM for 30 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        // A clean shutdown removes the discovery file.
        assert!(status.success(), "boot {boot}: {status}");
        assert!(
            !meta.exists(),
            "boot {boot}: operator/daemon.json left behind"
        );
    }
    let probe_json = std::fs::read_to_string(data.join("operator/probe.json")).unwrap();
    assert!(probe_json.contains("\n  \"ok\": true"), "{probe_json}");
    assert!(probe_json.contains("\"mode\": \"boot\""), "{probe_json}");
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}
