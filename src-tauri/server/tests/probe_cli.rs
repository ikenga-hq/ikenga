//! The built `ikenga-server probe --executor-tier t1` (G-PRINCIPAL §8) and
//! the T1 boot. Exit 0 only when the host can run T1; 1 otherwise.

#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_ikenga-server");

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
    Command::new(BIN)
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
/// `probe.json`) and refuses to serve on this build.
#[test]
#[ignore = "t1-root"]
fn t1_root_probe_passes_read_only_and_the_boot_refuses_after_probing() {
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

    let boot = Command::new(BIN)
        .args(["--executor-tier", "t1", "--data-dir", data_s, "--port", "0"])
        .args(range)
        .env_remove("IKENGA_DATA_DIR")
        .env("RUST_BACKTRACE", "0")
        .output()
        .unwrap();
    assert_eq!(boot.status.code(), Some(1), "{boot:?}");
    let stderr = String::from_utf8_lossy(&boot.stderr);
    assert!(stderr.contains("no T1 broker yet"), "{stderr}");
    let probe_json = std::fs::read_to_string(data.join("operator/probe.json")).unwrap();
    assert!(probe_json.contains("\n  \"ok\": true"), "{probe_json}");
    assert!(probe_json.contains("\"mode\": \"boot\""), "{probe_json}");
}
