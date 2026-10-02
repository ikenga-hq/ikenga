//! The built `ikenga-server accounts …` binary under verbose logging (WP-20
//! review F2): log events from other threads must never block the command,
//! and they go to stderr so stdout stays the command's output.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    let mut child = Command::new(bin())
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

/// Root: `accounts disable` kills every process of the account's uid
/// (G-PRINCIPAL §7.3) through the built binary's `__t1-kill-all`, spawned via
/// the T1 executor as that uid — a detached process in its own group too.
#[test]
#[ignore = "t1-root"]
fn t1_root_disable_kills_every_process_of_the_uid() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    assert!(is_root(), "t1-root tests must run as root");
    struct HostUser;
    impl Drop for HostUser {
        fn drop(&mut self) {
            let _ = Command::new("userdel").arg("ik-t1root-reap").output();
            let _ = Command::new("groupdel").arg("ik-t1root-reap").output();
        }
    }
    let _cleanup = HostUser;
    let tmp = TempDir::new("reap");
    let root = tmp.0.join("root");
    let root = root.to_str().unwrap();
    let common = ["accounts", "--data-dir", root, "--uid-range", "28120-28130"];

    let mut create = common.to_vec();
    create.extend(["create", "t1root-reap", "--password-stdin"]);
    let created = run(
        &tmp.0,
        &create,
        "correct horse battery\n",
        Duration::from_secs(120),
    );
    assert!(created.status.success(), "{}", created.stderr);

    // A process of that uid, detached into its own process group.
    let mut victim = Command::new("sleep")
        .arg("300")
        .uid(28_120)
        .gid(28_120)
        .process_group(0)
        .current_dir("/")
        .spawn()
        .unwrap();

    let mut disable = common.to_vec();
    disable.extend(["disable", "t1root-reap"]);
    let disabled = run(&tmp.0, &disable, "", Duration::from_secs(60));
    assert!(disabled.status.success(), "{}", disabled.stderr);
    assert!(
        disabled
            .stdout
            .contains("killed every process of uid 28120"),
        "{}",
        disabled.stdout
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = victim.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the uid's process survived disable"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.signal(), Some(9), "SIGKILL");
}

/// Root: `accounts adopt-t0` end to end through the built binary (G-PRINCIPAL
/// §11.2, G-ACCESS R-10): a root/Docker-shaped T0 install is copied into a
/// fresh principal the command creates, the access store stays in the
/// root-only archive, and the principal's tree is the uid's alone (I-9).
#[test]
#[ignore = "t1-root"]
fn t1_root_adopt_t0_through_the_binary() {
    use std::os::unix::fs::MetadataExt;
    assert!(is_root(), "t1-root tests must run as root");
    struct HostUser;
    impl Drop for HostUser {
        fn drop(&mut self) {
            let _ = Command::new("userdel").arg("ik-t1root-mig").output();
            let _ = Command::new("groupdel").arg("ik-t1root-mig").output();
        }
    }
    let _cleanup = HostUser;
    let tmp = TempDir::new("adopt");
    let (from, home) = (tmp.0.join("t0-data"), tmp.0.join("t0-home"));
    std::fs::create_dir_all(from.join("chi-cache")).unwrap();
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::write(from.join("supabase.json"), "{}").unwrap();
    std::fs::write(from.join("chi-cache/x"), "run").unwrap();
    std::fs::write(from.join("access.db"), "device hashes").unwrap();
    std::fs::write(
        from.join("fs_roots.json"),
        format!(r#"{{"roots":["{}/code"]}}"#, home.display()),
    )
    .unwrap();
    std::fs::write(home.join(".gemini/oauth"), "login").unwrap();
    let root = tmp.0.join("root");
    let args = [
        "accounts",
        "--data-dir",
        root.to_str().unwrap(),
        "--uid-range",
        "28140-28150",
        "adopt-t0",
        "--from",
        from.to_str().unwrap(),
        "--home",
        home.to_str().unwrap(),
        "--admin",
        "--password-stdin",
        "t1root-mig",
    ];
    let out = run(
        &tmp.0,
        &args,
        "correct horse battery\n",
        Duration::from_secs(120),
    );
    assert!(out.status.success(), "{}\n{}", out.stdout, out.stderr);
    assert!(
        out.stdout.contains("created admin account t1root-mig"),
        "{}",
        out.stdout
    );
    assert!(out.stdout.contains("holds access.db"), "{}", out.stdout);

    let principals: Vec<_> = std::fs::read_dir(root.join("principals"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(principals.len(), 1);
    let pdir = &principals[0];
    let uid = std::fs::metadata(pdir).unwrap().uid();
    assert_eq!(uid, 28_140);
    fn walk(p: &Path, uid: u32, seen: &mut usize) {
        let m = std::fs::symlink_metadata(p).unwrap();
        assert_eq!((m.uid(), m.gid()), (uid, uid), "{}", p.display());
        assert_eq!(m.mode() & 0o6077, 0, "{}: {:o}", p.display(), m.mode());
        *seen += 1;
        if m.is_dir() {
            for e in std::fs::read_dir(p).unwrap() {
                walk(&e.unwrap().path(), uid, seen);
            }
        }
    }
    let mut seen = 0;
    walk(pdir, uid, &mut seen);
    assert!(seen >= 9, "{seen}");
    assert!(!pdir.join("data/access.db").exists(), "R-10");
    assert_eq!(
        std::fs::read_to_string(pdir.join("home/.gemini/oauth")).unwrap(),
        "login"
    );
    let roots = std::fs::read_to_string(pdir.join("data/fs_roots.json")).unwrap();
    assert!(
        roots.contains(&format!("{}/code", pdir.join("home").display())),
        "{roots}"
    );
    let archive = std::fs::read_dir(&tmp.0)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().contains("t0-data.t0-migrated-"))
        .expect("archive");
    let m = std::fs::metadata(&archive).unwrap();
    assert_eq!((m.uid(), m.mode() & 0o7777), (0, 0o500));
    assert!(archive.join("access.db").exists());
    assert!(!from.exists());
}
