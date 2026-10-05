//! The executor of a T1 **principal child** (G-PRINCIPAL §3, topology B).
//!
//! Under T1 the broker launches one `ikenga-server --executor-tier t1
//! --principal-child` per principal, through the T1 executor, already dropped
//! to that principal's uid. The child is today's single-tenant daemon: its
//! PTYs, engines and sidecars spawn exactly as T0 does — the process itself
//! *is* the isolation boundary — so [`PrincipalChildExecutor`] reuses T0's
//! spawn mechanics ([`InProcessExecutor`]).
//!
//! What differs is the probe. It does not trust that the broker dropped the
//! process; it verifies it, from `/proc/self/status`, before the child
//! serves anything (§3 "pinned for the child"):
//!
//! * the effective uid is not 0, and every uid (real, effective, saved, fs)
//!   equals the expected one;
//! * `CapEff == CapPrm == 0`;
//! * `NoNewPrivs == 1`.
//!
//! Only when all four hold does it report `principal_isolation: true` (and a
//! probe stamp); a failing check refuses the boot (DEC-R9-1).
//!
//! Linux-only, like T1.

use portable_pty::PtySize;

use super::t1_probe::ProcStatus;
use super::{
    Capabilities, ExecutorTier, InProcessExecutor, PipedOpts, ProbeStamp, PtyChild, Refusal,
    SessionExecutor, SpawnSpec,
};

fn failed(detail: impl Into<String>) -> Refusal {
    Refusal::ProbeFailed {
        check: "principal_child",
        detail: detail.into(),
    }
}

/// The four §3 child checks against one `/proc/<pid>/status` reading.
pub fn verify_dropped(status: &ProcStatus, expected_uid: u32) -> Result<(), Refusal> {
    if expected_uid == 0 {
        return Err(failed(
            "the expected principal uid is 0; a principal is never root (I-1)",
        ));
    }
    let euid = status.uid[1];
    if euid == 0 {
        return Err(failed(
            "running with euid 0: the broker did not drop this child",
        ));
    }
    if status.uid.iter().any(|&u| u != expected_uid) {
        return Err(failed(format!(
            "uids {:?} are not all the expected principal uid {expected_uid}",
            status.uid
        )));
    }
    if status.cap_eff != 0 || status.cap_prm != 0 {
        return Err(failed(format!(
            "capabilities remain (CapEff {:#x}, CapPrm {:#x}); a principal child holds none",
            status.cap_eff, status.cap_prm
        )));
    }
    if status.no_new_privs != Some(1) {
        return Err(failed(format!(
            "NoNewPrivs is {:?}, not 1 (OD-9)",
            status.no_new_privs
        )));
    }
    Ok(())
}

/// T0 spawn mechanics, T1 tier, isolation proven by [`verify_dropped`].
#[derive(Debug)]
pub struct PrincipalChildExecutor {
    stamp: ProbeStamp,
}

impl PrincipalChildExecutor {
    /// Probe this process. The only constructor: an unverified child never
    /// gets an executor.
    pub fn probe(expected_uid: u32) -> Result<Self, Refusal> {
        let status = ProcStatus::read_self().map_err(failed)?;
        Self::from_status(&status, expected_uid)
    }

    pub(crate) fn from_status(status: &ProcStatus, expected_uid: u32) -> Result<Self, Refusal> {
        verify_dropped(status, expected_uid)?;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(Self {
            stamp: ProbeStamp { ok: true, at },
        })
    }
}

impl SessionExecutor for PrincipalChildExecutor {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            tier: ExecutorTier::T1,
            pty: true,
            piped: true,
            // Only ever constructed from a passing `verify_dropped` (I-5).
            principal_isolation: self.stamp.ok,
        }
    }

    fn probe_stamp(&self) -> Option<ProbeStamp> {
        Some(self.stamp)
    }

    fn spawn_pty(&self, spec: SpawnSpec, size: PtySize) -> anyhow::Result<PtyChild> {
        InProcessExecutor.spawn_pty(spec, size)
    }

    fn spawn_piped(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<tokio::process::Child> {
        InProcessExecutor.spawn_piped(spec, opts)
    }

    fn spawn_output_blocking(
        &self,
        spec: SpawnSpec,
        opts: PipedOpts,
    ) -> std::io::Result<std::process::Output> {
        InProcessExecutor.spawn_output_blocking(spec, opts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(uid: u32, cap_eff: u64, cap_prm: u64, nnp: Option<u32>) -> ProcStatus {
        ProcStatus {
            uid: [uid; 4],
            gid: [uid; 4],
            groups: vec![],
            cap_eff,
            cap_prm,
            no_new_privs: nnp,
            seccomp: Some(0),
            seccomp_filters: Some(0),
        }
    }

    #[test]
    fn a_dropped_child_passes_and_reports_isolation() {
        let exec = PrincipalChildExecutor::from_status(&status(20_001, 0, 0, Some(1)), 20_001)
            .expect("all four checks hold");
        let caps = exec.capabilities();
        assert_eq!(caps.tier, ExecutorTier::T1);
        assert!(caps.principal_isolation && caps.pty && caps.piped);
        assert!(exec.probe_stamp().unwrap().ok);
    }

    #[test]
    fn each_failed_check_refuses() {
        let cases = [
            (status(0, 0, 0, Some(1)), 20_001, "euid 0"),
            (
                status(20_002, 0, 0, Some(1)),
                20_001,
                "expected principal uid",
            ),
            (status(20_001, 0x80, 0, Some(1)), 20_001, "capabilities"),
            (status(20_001, 0, 0x80, Some(1)), 20_001, "capabilities"),
            (status(20_001, 0, 0, Some(0)), 20_001, "NoNewPrivs"),
            (status(20_001, 0, 0, None), 20_001, "NoNewPrivs"),
            (status(20_001, 0, 0, Some(1)), 0, "never root"),
        ];
        for (st, expected, why) in cases {
            let err = PrincipalChildExecutor::from_status(&st, expected).unwrap_err();
            let Refusal::ProbeFailed { check, detail } = &err else {
                panic!("{err}");
            };
            assert_eq!(*check, "principal_child");
            assert!(detail.contains(why), "{why}: {detail}");
        }
        // A saved uid left at root is caught too.
        let mut st = status(20_001, 0, 0, Some(1));
        st.uid[2] = 0;
        assert!(PrincipalChildExecutor::from_status(&st, 20_001).is_err());
    }

    /// This test process is not a dropped principal child (it may be root, or
    /// an ordinary user without NoNewPrivs), so the real probe refuses.
    #[test]
    fn the_test_process_itself_is_not_a_principal_child() {
        // SAFETY: no preconditions.
        let uid = unsafe { libc::getuid() };
        if let Ok(st) = ProcStatus::read_self() {
            if st.no_new_privs != Some(1) || uid == 0 {
                assert!(PrincipalChildExecutor::probe(uid.max(1)).is_err());
            }
        }
    }
}
