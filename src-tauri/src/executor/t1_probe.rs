//! The host-side steps of the T1 boot probe (G-PRINCIPAL §8): identity (2),
//! capabilities (3), observations (4) and the **real test drop** (6), plus
//! the probe child that step 6 runs (`ikenga-server __t1-probe-child`).
//!
//! The operator-side steps — the operator root (5), the probe-uid
//! precondition against `accounts.db`, reconcile (7) and `probe.json` — live
//! in `server::operator::probe`, which orchestrates the whole probe and calls
//! into this module. Step 1 (OS) is this module existing at all: it is
//! compiled on Linux only.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

use super::t1::T1Executor;
use super::{PipedOpts, Principal, PrincipalId, Refusal, SessionExecutor, SpawnSpec, StdioMode};

/// `CAP_CHOWN`, `CAP_KILL`, `CAP_SETGID`, `CAP_SETUID` (bits 0, 5, 6, 7).
pub const REQUIRED_CAPS: [(u32, &str); 4] = [
    (7, "CAP_SETUID"),
    (6, "CAP_SETGID"),
    (0, "CAP_CHOWN"),
    (5, "CAP_KILL"),
];

/// How long the parent waits for the probe child (§8 step 6).
pub const TEST_DROP_TIMEOUT: Duration = Duration::from_secs(10);

/// The fields of `/proc/<pid>/status` the probe reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStatus {
    /// Real, effective, saved, filesystem.
    pub uid: [u32; 4],
    pub gid: [u32; 4],
    pub groups: Vec<u32>,
    pub cap_eff: u64,
    pub cap_prm: u64,
    pub no_new_privs: Option<u32>,
    pub seccomp: Option<u32>,
    pub seccomp_filters: Option<u32>,
}

impl ProcStatus {
    pub fn parse(text: &str) -> Result<Self, String> {
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix(':')))
                .map(str::trim)
        };
        let ids = |key: &str| -> Result<[u32; 4], String> {
            let v: Vec<u32> = field(key)
                .ok_or_else(|| format!("no {key}: line"))?
                .split_whitespace()
                .map(|s| s.parse().map_err(|_| format!("bad {key}: `{s}`")))
                .collect::<Result<_, _>>()?;
            v.try_into().map_err(|_| format!("{key}: expected 4 ids"))
        };
        let caps = |key: &str| -> Result<u64, String> {
            let hex = field(key).ok_or_else(|| format!("no {key}: line"))?;
            u64::from_str_radix(hex, 16).map_err(|_| format!("bad {key}: `{hex}`"))
        };
        let num = |key: &str| field(key).and_then(|v| v.parse().ok());
        Ok(Self {
            uid: ids("Uid")?,
            gid: ids("Gid")?,
            groups: field("Groups")
                .unwrap_or("")
                .split_whitespace()
                .map(|s| s.parse().map_err(|_| format!("bad Groups: `{s}`")))
                .collect::<Result<_, _>>()?,
            cap_eff: caps("CapEff")?,
            cap_prm: caps("CapPrm")?,
            no_new_privs: num("NoNewPrivs"),
            seccomp: num("Seccomp"),
            seccomp_filters: num("Seccomp_filters"),
        })
    }

    pub fn read_self() -> Result<Self, String> {
        let text = fs::read_to_string("/proc/self/status")
            .map_err(|e| format!("/proc/self/status: {e}"))?;
        Self::parse(&text)
    }
}

/// Step 4's record. Not a gate: NNP=1 on the broker does not stop a
/// `CAP_SETUID` holder from dropping, and the real drop (6) is the proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostObservations {
    pub euid: u32,
    /// `CapEff` as `0x…` hex, as `/proc` shows it.
    pub cap_eff: String,
    pub no_new_privs: Option<u32>,
    pub seccomp: Option<u32>,
    pub seccomp_filters: Option<u32>,
}

fn failed(check: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal::ProbeFailed {
        check,
        detail: detail.into(),
    }
}

/// Step 2: euid 0.
pub fn check_identity(status: &ProcStatus) -> Result<(), Refusal> {
    let euid = status.uid[1];
    if euid != 0 {
        return Err(failed(
            "identity",
            format!(
                "euid is {euid}, not 0: the T1 broker provisions users and drops to them, which \
                 needs root in the container (capabilities without root are not a v1 mode)"
            ),
        ));
    }
    Ok(())
}

/// Step 3: `CAP_SETUID`, `CAP_SETGID`, `CAP_CHOWN`, `CAP_KILL` effective.
pub fn check_capabilities(status: &ProcStatus) -> Result<(), Refusal> {
    let missing: Vec<&str> = REQUIRED_CAPS
        .iter()
        .filter(|(bit, _)| status.cap_eff & (1u64 << bit) == 0)
        .map(|(_, name)| *name)
        .collect();
    if !missing.is_empty() {
        return Err(failed(
            "capabilities",
            format!(
                "CapEff {:#018x} lacks {}; run with CAP_SETUID, CAP_SETGID, CAP_CHOWN and CAP_KILL",
                status.cap_eff,
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Step 4: record, don't gate.
pub fn observe(status: &ProcStatus) -> HostObservations {
    HostObservations {
        euid: status.uid[1],
        cap_eff: format!("{:#018x}", status.cap_eff),
        no_new_privs: status.no_new_privs,
        seccomp: status.seccomp,
        seccomp_filters: status.seccomp_filters,
    }
}

/// Steps 2–4 against a parsed status.
pub fn check_host(status: &ProcStatus) -> Result<HostObservations, Refusal> {
    check_identity(status)?;
    check_capabilities(status)?;
    Ok(observe(status))
}

/// Steps 2–4 on this process.
pub fn check_this_host() -> Result<HostObservations, Refusal> {
    check_host(&ProcStatus::read_self().map_err(|e| failed("identity", e))?)
}

/// How the probe runs its child: `ikenga-server` re-executes itself with the
/// hidden `__t1-probe-child` entry. (Tests substitute their own binary.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeChildCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// The hidden argv entry that runs [`probe_child_entry`].
pub const PROBE_CHILD_ARG: &str = "__t1-probe-child";

impl ProbeChildCommand {
    /// This binary, resolved from `/proc/self/exe`. Resolved (not the magic
    /// link itself) so the exec walks the real path as the probe uid — the
    /// same path the broker will exec its principal children through, which
    /// must be reachable and executable by every principal (§4).
    pub fn current_exe() -> io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![PROBE_CHILD_ARG.into()],
        })
    }
}

/// Parameters of the child, passed in its environment (the T1 floor lets
/// them through; argv stays the bare entry name).
const ENV_UID: &str = "IKENGA_T1_PROBE_UID";
const ENV_GID: &str = "IKENGA_T1_PROBE_GID";
const ENV_SEALED: &str = "IKENGA_T1_PROBE_SEALED";
const ENV_WRITE: &str = "IKENGA_T1_PROBE_WRITE";
const ENV_NNP: &str = "IKENGA_T1_PROBE_NNP";

/// Whether this process was started as a probe child (its parameters set).
pub fn is_probe_child() -> bool {
    std::env::var_os(ENV_UID).is_some()
}

/// What the probe child proves, running as the probe uid after a real T1
/// spawn (§8 step 6): every id dropped, no groups, `setuid(0)` is `EPERM`, a
/// root-only dir is `EACCES`, no capabilities left, NNP set (OD-9), and it
/// can write in its own dir (the parent then checks the file's owner).
pub fn probe_child_entry() -> Result<(), String> {
    let var = |k: &str| std::env::var_os(k).ok_or_else(|| format!("{k} unset"));
    let num = |k: &str| -> Result<u32, String> {
        var(k)?
            .to_str()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("{k} is not a uid"))
    };
    let (uid, gid) = (num(ENV_UID)?, num(ENV_GID)?);
    let sealed = PathBuf::from(var(ENV_SEALED)?);
    let write = PathBuf::from(var(ENV_WRITE)?);
    let want_nnp = std::env::var_os(ENV_NNP).is_some_and(|v| v == "1");

    let (mut r, mut e, mut s) = (0, 0, 0);
    // SAFETY: plain syscalls on stack out-params.
    unsafe {
        if libc::getresuid(&mut r, &mut e, &mut s) != 0 || (r, e, s) != (uid, uid, uid) {
            return Err(format!("getresuid: ({r}, {e}, {s}), expected {uid}"));
        }
        if libc::getresgid(&mut r, &mut e, &mut s) != 0 || (r, e, s) != (gid, gid, gid) {
            return Err(format!("getresgid: ({r}, {e}, {s}), expected {gid}"));
        }
        let n = libc::getgroups(0, std::ptr::null_mut());
        if n != 0 {
            return Err(format!(
                "getgroups: {n} supplementary groups, expected none"
            ));
        }
        if libc::setuid(0) == 0 {
            return Err("setuid(0) succeeded: root was regained".into());
        }
        let errno = io::Error::last_os_error();
        if errno.raw_os_error() != Some(libc::EPERM) {
            return Err(format!("setuid(0): {errno}, expected EPERM"));
        }
    }
    match fs::read_dir(&sealed) {
        Err(e) if e.raw_os_error() == Some(libc::EACCES) => {}
        Err(e) => return Err(format!("{}: {e}, expected EACCES", sealed.display())),
        Ok(_) => return Err(format!("{} (root, 0700) was readable", sealed.display())),
    }
    let status = ProcStatus::read_self()?;
    if status.uid != [uid; 4] || status.gid != [gid; 4] {
        return Err(format!(
            "/proc/self/status ids {:?}/{:?}, expected {uid}/{gid}",
            status.uid, status.gid
        ));
    }
    if status.cap_eff != 0 || status.cap_prm != 0 {
        return Err(format!(
            "capabilities survived the drop: CapEff {:#x} CapPrm {:#x}",
            status.cap_eff, status.cap_prm
        ));
    }
    if want_nnp && status.no_new_privs != Some(1) {
        return Err(format!(
            "NoNewPrivs is {:?}, expected 1",
            status.no_new_privs
        ));
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&write)
        .map_err(|e| format!("{}: {e}", write.display()))?;
    Ok(())
}

/// The step-6 scaffold: `<base>/.t1-probe-XXXXXX/` (root, 0711) holding
/// `sealed/` (root, 0700) and `home/` (probe uid, 0700). Removed on drop.
struct Scaffold {
    dir: PathBuf,
    home: PathBuf,
    sealed: PathBuf,
}

impl Scaffold {
    fn create(base: &Path, probe_uid: u32) -> io::Result<Self> {
        let mut template = base.join(".t1-probe-XXXXXX").into_os_string().into_vec();
        template.push(0);
        // SAFETY: a NUL-terminated, writable template; mkdtemp creates the
        // dir 0700 with a random suffix, atomically (no symlink race in a
        // shared parent).
        let made = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
        if made.is_null() {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        let dir = PathBuf::from(OsString::from_vec(template));
        let scaffold = Self {
            home: dir.join("home"),
            sealed: dir.join("sealed"),
            dir,
        };
        fs::set_permissions(&scaffold.dir, fs::Permissions::from_mode(0o711))?;
        for sub in [&scaffold.sealed, &scaffold.home] {
            fs::create_dir(sub)?;
            fs::set_permissions(sub, fs::Permissions::from_mode(0o700))?;
        }
        std::os::unix::fs::lchown(&scaffold.home, Some(probe_uid), Some(probe_uid))?;
        Ok(scaffold)
    }
}

impl Drop for Scaffold {
    fn drop(&mut self) {
        // Back to root first, so removing the probe's files needs no
        // CAP_DAC_OVERRIDE.
        let _ = std::os::unix::fs::lchown(&self.home, Some(0), Some(0));
        if let Err(e) = fs::remove_dir_all(&self.dir) {
            tracing::warn!("t1 probe: could not remove {}: {e}", self.dir.display());
        }
    }
}

/// Step 6: spawn `child` **through the T1 executor** as the probe principal
/// (uid = gid = `probe_uid`, no passwd entry, home a fresh root-created dir
/// chowned to it under `base`), wait up to [`TEST_DROP_TIMEOUT`], and confirm
/// it exited 0 and that the file it wrote is owned by the probe uid. The
/// caller has already checked that `probe_uid` is nobody's (§8 precondition).
pub async fn test_drop(
    executor: &T1Executor,
    probe_uid: u32,
    base: &Path,
    child: &ProbeChildCommand,
) -> Result<(), Refusal> {
    let fail = |detail: String| failed("test_drop", detail);
    if probe_uid == 0 {
        return Err(fail("the probe uid is 0".into()));
    }
    let scaffold = Scaffold::create(base, probe_uid).map_err(|e| {
        fail(format!(
            "creating the probe dir under {}: {e}",
            base.display()
        ))
    })?;
    let written = scaffold.home.join("probe-write");
    let principal = Principal {
        id: PrincipalId::new_v7(),
        username: "t1-probe".into(),
        unix_name: "ik-t1-probe".into(),
        uid: probe_uid,
        gid: probe_uid,
        home: scaffold.home.clone(),
        shell: "/bin/sh".into(),
    };
    let mut spec = SpawnSpec::new(&child.program);
    spec.args(&child.args)
        .env(ENV_UID, probe_uid.to_string())
        .env(ENV_GID, probe_uid.to_string())
        .env(ENV_SEALED, &scaffold.sealed)
        .env(ENV_WRITE, &written)
        .env(ENV_NNP, "1")
        .env("TMPDIR", &scaffold.home)
        .principal(Some(principal));
    let opts = PipedOpts {
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
        kill_on_drop: true,
        no_console_window: false,
        detached: false,
        new_process_group: false,
    };
    let proc = executor.spawn_piped(spec, opts).map_err(|e| {
        fail(format!(
            "spawning {} as uid {probe_uid}: {e}",
            child.program.display()
        ))
    })?;
    let out = tokio::time::timeout(TEST_DROP_TIMEOUT, proc.wait_with_output())
        .await
        .map_err(|_| {
            fail(format!(
                "the probe child did not exit within {TEST_DROP_TIMEOUT:?}"
            ))
        })?
        .map_err(|e| fail(format!("waiting for the probe child: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        // The last ~400 bytes, cut on a char boundary.
        let cut = (stderr.len().saturating_sub(400)..=stderr.len())
            .find(|&i| stderr.is_char_boundary(i))
            .unwrap_or(stderr.len());
        let tail = &stderr[cut..];
        return Err(fail(format!(
            "the probe child exited {}: {tail}",
            out.status
        )));
    }
    let meta = fs::symlink_metadata(&written)
        .map_err(|e| fail(format!("the probe child's file {}: {e}", written.display())))?;
    if !meta.is_file() || meta.uid() != probe_uid || meta.gid() != probe_uid {
        return Err(fail(format!(
            "the probe child's file is owned by {}:{}, expected {probe_uid}:{probe_uid} — the \
             uid change is cosmetic on this host",
            meta.uid(),
            meta.gid()
        )));
    }
    drop(scaffold);
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Railway's status (G-73, `04` Round 14): CapEff 0x800405fb, NNP 0,
    /// seccomp 2 with 3 filters.
    pub(crate) const RAILWAY: &str = "Name:\tikenga-server\nUmask:\t0022\nState:\tR (running)\n\
        Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nGroups:\t0 \nCapInh:\t0000000000000000\n\
        CapPrm:\t00000000800405fb\nCapEff:\t00000000800405fb\nCapBnd:\t00000000800405fb\n\
        NoNewPrivs:\t0\nSeccomp:\t2\nSeccomp_filters:\t3\n";

    #[test]
    fn parses_proc_status() {
        let s = ProcStatus::parse(RAILWAY).unwrap();
        assert_eq!(s.uid, [0; 4]);
        assert_eq!(s.groups, vec![0]);
        assert_eq!(s.cap_eff, 0x8004_05fb);
        assert_eq!(s.no_new_privs, Some(0));
        assert_eq!(s.seccomp, Some(2));
        assert_eq!(s.seccomp_filters, Some(3));
        assert!(ProcStatus::parse("Uid:\t0\t0\n").is_err());
        assert!(ProcStatus::read_self().is_ok());
    }

    #[test]
    fn railway_passes_steps_2_to_4() {
        let obs = check_host(&ProcStatus::parse(RAILWAY).unwrap()).unwrap();
        assert_eq!(obs.euid, 0);
        assert_eq!(obs.cap_eff, "0x00000000800405fb");
        assert_eq!((obs.no_new_privs, obs.seccomp_filters), (Some(0), Some(3)));
    }

    #[test]
    fn a_non_root_euid_fails_identity() {
        let text = RAILWAY.replace("Uid:\t0\t0\t0\t0", "Uid:\t1000\t1000\t1000\t1000");
        let err = check_host(&ProcStatus::parse(&text).unwrap()).unwrap_err();
        assert!(
            matches!(
                err,
                Refusal::ProbeFailed {
                    check: "identity",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn each_missing_capability_fails_step_3() {
        for (bit, name) in REQUIRED_CAPS {
            let mask = 0x8004_05fbu64 & !(1u64 << bit);
            let text = RAILWAY.replace(
                "CapEff:\t00000000800405fb",
                &format!("CapEff:\t{mask:016x}"),
            );
            let err = check_host(&ProcStatus::parse(&text).unwrap()).unwrap_err();
            match err {
                Refusal::ProbeFailed {
                    check: "capabilities",
                    detail,
                } => assert!(detail.contains(name), "{detail}"),
                other => panic!("{other}"),
            }
        }
    }

    #[test]
    fn the_probe_child_fails_closed_without_its_parameters() {
        assert!(!is_probe_child() || std::env::var_os(ENV_UID).is_some());
        if !is_probe_child() {
            assert!(probe_child_entry().is_err());
        }
    }

    /// The test binary's stand-in for `ikenga-server __t1-probe-child`: when
    /// the probe spawns this test binary with its parameters set, this runs
    /// the child checks and exits; in a normal run it does nothing.
    #[test]
    #[ignore = "t1-root (probe child entry)"]
    fn t1_root_probe_child_entry() {
        if !is_probe_child() {
            return;
        }
        match probe_child_entry() {
            Ok(()) => std::process::exit(0),
            Err(e) => {
                eprintln!("t1 probe child: {e}");
                std::process::exit(3);
            }
        }
    }

    /// How a lib test runs [`t1_root_probe_child_entry`] as the probe child.
    pub(crate) fn test_child() -> ProbeChildCommand {
        ProbeChildCommand {
            program: std::env::current_exe().unwrap(),
            args: [
                "--exact",
                "executor::t1_probe::tests::t1_root_probe_child_entry",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
                "-q",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
        }
    }

    pub(crate) fn traversable_tempdir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        tmp
    }

    /// §8 step 6 end to end on a real host, through the real T1 executor:
    /// the child proves the drop and the parent sees its file owned by the
    /// probe uid.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_test_drop_passes_and_cleans_up() {
        super::super::t1::tests::t1_root::require_root();
        let tmp = traversable_tempdir();
        let exec = T1Executor::new(super::super::t1::tests::config());
        test_drop(&exec, 28_599, tmp.path(), &test_child())
            .await
            .unwrap();
        assert_eq!(
            fs::read_dir(tmp.path()).unwrap().count(),
            0,
            "the scaffold is removed"
        );
        assert!(!exec.is_degraded());
    }

    /// A child that fails its checks fails the probe, with its reason.
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_test_drop_reports_a_failing_child() {
        super::super::t1::tests::t1_root::require_root();
        let tmp = traversable_tempdir();
        let exec = T1Executor::new(super::super::t1::tests::config());
        let child = ProbeChildCommand {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo nope >&2; exit 4".into()],
        };
        let err = test_drop(&exec, 28_598, tmp.path(), &child)
            .await
            .unwrap_err();
        match err {
            Refusal::ProbeFailed {
                check: "test_drop",
                detail,
            } => assert!(detail.contains("nope"), "{detail}"),
            other => panic!("{other}"),
        }
        // A child that exits 0 without writing its file fails too.
        let child = ProbeChildCommand {
            program: "/bin/true".into(),
            args: vec![],
        };
        assert!(test_drop(&exec, 28_598, tmp.path(), &child).await.is_err());
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    /// The parent's own view of the child's identity: unprobed, the child of
    /// an unreachable program fails cleanly (not as drift).
    #[tokio::test]
    #[ignore = "t1-root"]
    async fn t1_root_test_drop_with_a_missing_program_is_not_drift() {
        super::super::t1::tests::t1_root::require_root();
        let tmp = traversable_tempdir();
        let exec = T1Executor::new(super::super::t1::tests::config());
        let child = ProbeChildCommand {
            program: "/nonexistent/ikenga-server".into(),
            args: vec![],
        };
        assert!(test_drop(&exec, 28_597, tmp.path(), &child).await.is_err());
        assert!(!exec.is_degraded());
    }
}
