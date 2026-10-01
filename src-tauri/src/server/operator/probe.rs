//! The T1 boot probe (G-PRINCIPAL §8), end to end. Refuse, don't fall back
//! (DEC-R9-1): any failing check is `Refusal::ProbeFailed { check, detail }`
//! and the daemon does not start. There is never a fallback to T0.
//!
//! | # | Check | Where |
//! |---|---|---|
//! | 1 | OS is Linux | this module only compiles there |
//! | 2 | euid 0 | `executor::t1_probe::check_identity` |
//! | 3 | `CAP_SETUID`/`SETGID`/`CHOWN`/`KILL` effective | `check_capabilities` |
//! | 4 | NNP / seccomp observations (not a gate) | `observe` |
//! | 5 | operator root: §4 owners/modes, not a T0 layout (I-10) | [`OperatorRoot`] |
//! | — | uid range agrees with the store; the probe uid (`range_end`) is no account's and no host user's/group's | here |
//! | 6 | **real test drop** through the T1 executor | `executor::t1_probe::test_drop` |
//! | 7 | reconcile `/etc` from `accounts.db` (boot only) | [`Provisioner::reconcile`] |
//!
//! Two modes. **Boot** (the T1 server) creates the operator root, migrates
//! `accounts.db` (only the broker migrates, §6.1), pins the uid range,
//! reconciles, writes `operator/probe.json` and, on failure, an
//! `auth_events` `probe_failed` row. **Read-only** (`ikenga-server probe
//! --executor-tier t1`) runs steps 1–6 and writes nothing but the transient
//! step-6 scaffold — no operator dirs, no store, no `/etc`, no `probe.json`.
//!
//! Only a passing probe yields a [`T1Executor`] carrying a [`ProbeStamp`],
//! which is the only way `/api/health` reports `principal_isolation: true`
//! (I-5).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sqlx::{Connection, SqliteConnection, SqlitePool};

use super::accounts::Actor;
use super::auth_events::{self, AuthEvent, AuthEventKind};
use super::provision::{Provisioner, ProvisioningMode, ReconcileReport, UidRange};
use super::{open_accounts, sys, Opener, OperatorRoot, Ownership};
use crate::executor::t1::{T1Config, T1Executor};
use crate::executor::t1_probe::{self, HostObservations, ProbeChildCommand, ProcStatus};
use crate::executor::{ProbeStamp, Refusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeMode {
    /// The T1 server's boot: steps 1–7, creates and writes.
    Boot,
    /// `ikenga-server probe`: steps 1–6, writes nothing lasting.
    ReadOnly,
}

#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// `--data-dir`: the operator root (P-1). Required.
    pub data_dir: Option<PathBuf>,
    pub uid_range: UidRange,
    pub provisioning: ProvisioningMode,
    /// `--principal-path` for the executor the probe yields.
    pub principal_path: Option<OsString>,
    pub mode: ProbeMode,
    /// What step 6 runs as the probe uid.
    pub child: ProbeChildCommand,
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    pub check: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// The full report: the log, `operator/probe.json` and `ikenga-server
/// probe` get this; `/api/health` gets only `{ok, at}`.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub tier: &'static str,
    pub mode: ProbeMode,
    pub ok: bool,
    /// Unix seconds.
    pub at: u64,
    pub operator_root: Option<PathBuf>,
    pub uid_range: String,
    pub probe_uid: u32,
    pub observations: Option<HostObservations>,
    pub checks: Vec<CheckResult>,
    pub reconcile: Option<ReconcileReport>,
}

impl ProbeReport {
    fn new(opts: &ProbeOptions) -> Self {
        Self {
            tier: "t1",
            mode: opts.mode,
            ok: false,
            at: 0,
            operator_root: None,
            uid_range: opts.uid_range.to_string(),
            probe_uid: opts.uid_range.probe_uid(),
            observations: None,
            checks: Vec::new(),
            reconcile: None,
        }
    }

    fn pass(&mut self, check: &'static str, detail: impl Into<String>) {
        self.checks.push(CheckResult {
            check,
            ok: true,
            detail: detail.into(),
        });
    }

    /// Record a failure and hand the refusal back for `?`.
    fn fail(&mut self, refusal: Refusal) -> Refusal {
        if let Refusal::ProbeFailed { check, detail } = &refusal {
            self.checks.push(CheckResult {
                check,
                ok: false,
                detail: detail.clone(),
            });
        }
        refusal
    }

    /// The first failing check, if any.
    pub fn failure(&self) -> Option<&CheckResult> {
        self.checks.iter().find(|c| !c.ok)
    }

    /// The human form `ikenga-server probe` prints and the boot logs.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let mode = match self.mode {
            ProbeMode::Boot => "boot",
            ProbeMode::ReadOnly => "read-only",
        };
        let _ = writeln!(
            out,
            "T1 probe ({mode}): {}",
            if self.ok { "PASS" } else { "FAIL" }
        );
        if let Some(root) = &self.operator_root {
            let _ = writeln!(out, "  operator root  {}", root.display());
        }
        let _ = writeln!(
            out,
            "  uid range      {} (probe uid {})",
            self.uid_range, self.probe_uid
        );
        for c in &self.checks {
            let _ = writeln!(
                out,
                "  [{}] {:<14} {}",
                if c.ok { " ok " } else { "FAIL" },
                c.check,
                c.detail
            );
        }
        if let Some(o) = &self.observations {
            let opt = |v: Option<u32>| v.map_or_else(|| "?".to_string(), |v| v.to_string());
            let _ = writeln!(
                out,
                "  observed       euid {}, CapEff {}, NoNewPrivs {}, Seccomp {} ({} filters)",
                o.euid,
                o.cap_eff,
                opt(o.no_new_privs),
                opt(o.seccomp),
                opt(o.seccomp_filters)
            );
        }
        out
    }
}

fn failed(check: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal::ProbeFailed {
        check,
        detail: detail.into(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run the probe. Returns the report either way, and — only when every
/// check passed — the [`T1Executor`] stamped with this probe.
pub async fn run(opts: &ProbeOptions) -> (ProbeReport, Result<T1Executor, Refusal>) {
    let mut report = ProbeReport::new(opts);
    let mut store: Option<SqlitePool> = None;
    let mut root: Option<OperatorRoot> = None;
    let result = steps(opts, &mut report, &mut store, &mut root).await;
    report.ok = result.is_ok();
    report.at = now_secs();

    if opts.mode == ProbeMode::Boot {
        if let Some(root) = &root {
            write_report(root, &report);
        }
        if let (Err(Refusal::ProbeFailed { check, detail }), Some(pool)) = (&result, &store) {
            record_failure(pool, check, detail).await;
        }
    }
    if let Some(pool) = store {
        pool.close().await;
    }
    let result = result.map(|config| {
        T1Executor::with_probe(
            config,
            ProbeStamp {
                ok: true,
                at: report.at,
            },
        )
    });
    (report, result)
}

async fn steps(
    opts: &ProbeOptions,
    report: &mut ProbeReport,
    store: &mut Option<SqlitePool>,
    root_out: &mut Option<OperatorRoot>,
) -> Result<T1Config, Refusal> {
    // 1. Linux: this module exists only there.
    report.pass("os", std::env::consts::OS);

    // 2–4.
    let status = ProcStatus::read_self().map_err(|e| report.fail(failed("identity", e)))?;
    t1_probe::check_identity(&status).map_err(|r| report.fail(r))?;
    report.pass("identity", "euid 0");
    t1_probe::check_capabilities(&status).map_err(|r| report.fail(r))?;
    report.pass(
        "capabilities",
        format!(
            "CapEff {:#018x} has CAP_SETUID, CAP_SETGID, CAP_CHOWN, CAP_KILL",
            status.cap_eff
        ),
    );
    report.observations = Some(t1_probe::observe(&status));

    // 5. The operator root.
    let data_dir = opts.data_dir.as_ref().ok_or_else(|| {
        report.fail(failed(
            "operator_root",
            "--data-dir is required under t1: it names the operator root (G-PRINCIPAL P-1)",
        ))
    })?;
    let data_dir = if data_dir.is_absolute() {
        data_dir.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| report.fail(failed("operator_root", format!("cwd: {e}"))))?
            .join(data_dir)
    };
    let root = OperatorRoot::new(data_dir)
        .map_err(|e| report.fail(failed("operator_root", e.to_string())))?;
    report.operator_root = Some(root.root().to_path_buf());
    match opts.mode {
        ProbeMode::Boot => {
            root.prepare(Ownership::Enforce)
                .map_err(|e| report.fail(failed("operator_root", e.to_string())))?;
            *root_out = Some(root.clone());
            report.pass("operator_root", "§4 owners and modes hold; not a T0 layout");
        }
        ProbeMode::ReadOnly => {
            let missing = root
                .check_layout(Ownership::Enforce)
                .map_err(|e| report.fail(failed("operator_root", e.to_string())))?;
            report.pass(
                "operator_root",
                if missing.is_empty() {
                    "§4 owners and modes hold; not a T0 layout".to_string()
                } else {
                    let names: Vec<String> =
                        missing.iter().map(|p| p.display().to_string()).collect();
                    format!(
                        "not a T0 layout; the boot would create {}",
                        names.join(", ")
                    )
                },
            );
        }
    }

    // The uid range, and the probe uid's precondition.
    let probe_uid = opts.uid_range.probe_uid();
    let prov = Provisioner::new(
        root.clone(),
        opts.uid_range,
        opts.provisioning,
        Actor::Broker,
    );
    let rows_holding_probe_uid = match opts.mode {
        ProbeMode::Boot => {
            // Only the broker migrates (§6.1).
            let pool = open_accounts(&root, Opener::Broker)
                .await
                .map_err(|e| report.fail(failed("operator_root", format!("accounts.db: {e:#}"))))?;
            *store = Some(pool.clone());
            let pinned = async {
                let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
                prov.pin_uid_range(&mut tx).await?;
                tx.commit().await?;
                Ok::<_, super::provision::ProvisionError>(())
            }
            .await;
            pinned.map_err(|e| report.fail(failed("uid_range", e.to_string())))?;
            report.pass(
                "uid_range",
                format!("{} pinned in accounts.db", opts.uid_range),
            );
            let mut conn = pool
                .acquire()
                .await
                .map_err(|e| report.fail(failed("probe_uid", e.to_string())))?;
            rows_holding(&mut conn, probe_uid)
                .await
                .map_err(|e| report.fail(failed("probe_uid", e.to_string())))?
        }
        ProbeMode::ReadOnly => {
            match read_only_store_view(&root, probe_uid)
                .await
                .map_err(|e| report.fail(failed("uid_range", e)))?
            {
                None => {
                    report.pass("uid_range", "no accounts.db yet; nothing pinned");
                    0
                }
                Some((pinned, holding)) => {
                    match pinned {
                        Some(stored) if stored != opts.uid_range.to_string() => {
                            return Err(report.fail(failed(
                                "uid_range",
                                format!(
                                    "--uid-range {} differs from {stored}, the range accounts.db \
                                     is pinned to; pass --uid-range {stored}",
                                    opts.uid_range
                                ),
                            )))
                        }
                        Some(stored) => {
                            report.pass("uid_range", format!("{stored} matches accounts.db"))
                        }
                        None => report.pass("uid_range", "accounts.db has no range pinned yet"),
                    }
                    holding
                }
            }
        }
    };
    if rows_holding_probe_uid > 0 {
        return Err(report.fail(failed(
            "probe_uid",
            format!(
                "uid/gid {probe_uid} (the range's reserved probe uid) is held by an account in \
                 accounts.db; the probe never acts as a live identity"
            ),
        )));
    }
    let host_user = sys::user_by_uid(probe_uid)
        .map_err(|e| report.fail(failed("probe_uid", format!("getpwuid({probe_uid}): {e}"))))?;
    if let Some(pw) = host_user {
        return Err(report.fail(failed(
            "probe_uid",
            format!(
                "uid {probe_uid} (the reserved probe uid) belongs to host user `{}`; move \
                 --uid-range",
                pw.name
            ),
        )));
    }
    if sys::group_gid_exists(probe_uid)
        .map_err(|e| report.fail(failed("probe_uid", format!("getgrgid({probe_uid}): {e}"))))?
    {
        return Err(report.fail(failed(
            "probe_uid",
            format!("gid {probe_uid} (the reserved probe uid) is a host group; move --uid-range"),
        )));
    }
    report.pass(
        "probe_uid",
        format!("{probe_uid} is no account's, host user's or group's"),
    );

    // 6. The real test drop, through the T1 executor.
    let config = T1Config {
        principals_dir: root.principals_dir(),
        principal_path: opts.principal_path.clone(),
    };
    let base = if root.principals_dir().is_dir() {
        root.principals_dir()
    } else {
        // Read-only on a root that doesn't exist yet.
        std::env::temp_dir()
    };
    let executor = T1Executor::new(config.clone());
    t1_probe::test_drop(&executor, probe_uid, &base, &opts.child)
        .await
        .map_err(|r| report.fail(r))?;
    report.pass(
        "test_drop",
        format!(
            "{} ran as uid {probe_uid} through the T1 executor: ids dropped, no groups, \
             setuid(0) EPERM, root-only dir EACCES, no capabilities, NoNewPrivs 1, its file \
             owned by the uid",
            opts.child.program.display()
        ),
    );

    // 7. Reconcile (boot only).
    if opts.mode == ProbeMode::Boot {
        let pool = store.as_ref().expect("the boot opened the store");
        let reconciled = prov
            .reconcile(pool)
            .await
            .map_err(|e| report.fail(failed("reconcile", e.to_string())))?;
        report.pass(
            "reconcile",
            format!(
                "{} active account(s) checked; {} host user(s) recreated, {} shell(s) restored \
                 [backend: {}]",
                reconciled.checked,
                reconciled.created.len(),
                reconciled.repaired_shell.len(),
                prov.backend_name()
            ),
        );
        report.reconcile = Some(reconciled);
    }
    Ok(config)
}

/// Rows whose uid or gid is `uid`.
async fn rows_holding(conn: &mut SqliteConnection, uid: u32) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM accounts WHERE unix_uid = ?1 OR unix_gid = ?1")
        .bind(i64::from(uid))
        .fetch_one(conn)
        .await
}

/// Read-only: `(pinned uid range, rows holding the probe uid)`, or `None`
/// when there is no initialised store. Opens `accounts.db` read-only and
/// never migrates or creates it.
async fn read_only_store_view(
    root: &OperatorRoot,
    probe_uid: u32,
) -> Result<Option<(Option<String>, i64)>, String> {
    let path = root.accounts_db();
    if !path.exists() {
        return Ok(None);
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true)
        .create_if_missing(false);
    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('accounts', 'operator_meta')",
    )
    .fetch_all(&mut conn)
    .await
    .map_err(|e| format!("{}: {e}", path.display()))?;
    if !tables.iter().any(|t| t == "accounts") {
        let _ = conn.close().await;
        return Ok(None);
    }
    let pinned = if tables.iter().any(|t| t == "operator_meta") {
        sqlx::query_scalar("SELECT value FROM operator_meta WHERE key = 'uid_range'")
            .fetch_optional(&mut conn)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        None
    };
    let holding = rows_holding(&mut conn, probe_uid)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = conn.close().await;
    Ok(Some((pinned, holding)))
}

/// `operator/probe.json` (root, 0600): the last full report.
fn write_report(root: &OperatorRoot, report: &ProbeReport) {
    if !root.operator_dir().is_dir() {
        return;
    }
    let path = root.probe_json();
    let written = serde_json::to_string_pretty(report)
        .map_err(std::io::Error::other)
        .and_then(|json| crate::server::discovery::write_private(&path, &json));
    if let Err(e) = written {
        tracing::warn!("could not write {}: {e}", path.display());
    }
}

/// `auth_events` `probe_failed` (through the one writer, R-2).
async fn record_failure(pool: &SqlitePool, check: &str, detail: &str) {
    let recorded = async {
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        auth_events::record(
            &mut tx,
            AuthEvent::new(AuthEventKind::ProbeFailed)
                .detail(serde_json::json!({ "check": check, "detail": detail })),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = recorded {
        tracing::warn!("could not record probe_failed: {e}");
    }
}

// ─── entry points ───────────────────────────────────────────────────────────

/// The T1 server's boot probe (§8: first in `run_server`, before anything
/// binds). Logs the full report; returns the stamped executor or the
/// refusal.
pub async fn boot(
    data_dir: Option<PathBuf>,
    uid_range: UidRange,
    provisioning: ProvisioningMode,
    principal_path: Option<OsString>,
) -> Result<T1Executor, Refusal> {
    let child = ProbeChildCommand::current_exe().map_err(|e| {
        failed(
            "setup",
            format!("resolving this binary for the probe child: {e}"),
        )
    })?;
    let opts = ProbeOptions {
        data_dir,
        uid_range,
        provisioning,
        principal_path,
        mode: ProbeMode::Boot,
        child,
    };
    let (report, result) = run(&opts).await;
    let json = serde_json::to_string(&report).unwrap_or_default();
    if result.is_ok() {
        tracing::info!("{}", report.render_text().trim_end());
        tracing::debug!("t1 probe report: {json}");
    } else {
        tracing::error!("{}", report.render_text().trim_end());
        tracing::error!("t1 probe report: {json}");
    }
    result
}

/// `ikenga-server probe --executor-tier t1 [--json]` (§8): steps 1–6,
/// read-only. Prints the report; the exit code is 0 on a pass, 1 otherwise.
pub async fn cli(data_dir: Option<PathBuf>, uid_range: UidRange, json: bool) -> i32 {
    let child = match ProbeChildCommand::current_exe() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("t1 probe: resolving this binary for the probe child: {e}");
            return 1;
        }
    };
    let opts = ProbeOptions {
        data_dir,
        uid_range,
        provisioning: ProvisioningMode::Auto,
        principal_path: None,
        mode: ProbeMode::ReadOnly,
        child,
    };
    let (report, result) = run(&opts).await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        print!("{}", report.render_text());
    }
    i32::from(result.is_err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::t1_probe::tests::test_child;
    use crate::server::operator::test_support;

    fn opts(data_dir: Option<PathBuf>, mode: ProbeMode) -> ProbeOptions {
        ProbeOptions {
            data_dir,
            uid_range: UidRange::new(28_600, 28_610).unwrap(),
            provisioning: ProvisioningMode::Auto,
            principal_path: None,
            mode,
            child: test_child(),
        }
    }

    fn is_root() -> bool {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    /// Unprivileged, the probe stops at step 2; as root (with the caps), it
    /// gets as far as the missing `--data-dir`. Either way: a typed refusal,
    /// no executor, and the report names the failing check.
    #[tokio::test]
    async fn without_a_data_dir_or_root_the_probe_refuses() {
        let (report, result) = run(&opts(None, ProbeMode::ReadOnly)).await;
        let refusal = result.err().expect("refused");
        let Refusal::ProbeFailed { check, .. } = &refusal else {
            panic!("{refusal}");
        };
        assert!(
            ["identity", "capabilities", "operator_root"].contains(check),
            "{refusal}"
        );
        if is_root() && t1_probe::check_this_host().is_ok() {
            assert_eq!(*check, "operator_root");
        }
        assert!(!report.ok);
        assert_eq!(report.failure().unwrap().check, *check);
        assert_eq!(report.checks[0].check, "os");
        let text = report.render_text();
        assert!(text.contains("FAIL"), "{text}");
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["tier"], "t1");
        assert_eq!(json["mode"], "read_only");
        assert_eq!(json["probe_uid"], 28_610);
    }

    #[tokio::test]
    async fn the_read_only_view_reports_the_pinned_range_and_probe_uid_holders() {
        let (_tmp, root) = test_support::temp_root();
        assert_eq!(read_only_store_view(&root, 28_610).await.unwrap(), None);
        let pool = open_accounts(&root, Opener::Broker).await.unwrap();
        assert_eq!(
            read_only_store_view(&root, 28_610).await.unwrap(),
            Some((None, 0))
        );
        sqlx::query("INSERT INTO operator_meta (key, value) VALUES ('uid_range', '28600-28610')")
            .execute(&pool)
            .await
            .unwrap();
        // A row holding the probe uid as its gid (an adopted user whose
        // primary group is that number) counts too.
        sqlx::query(
            "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, home, \
             created_at, updated_at) VALUES (?, 'x', 'ik-x', 1500, 28610, '/h', 0, 0)",
        )
        .bind(crate::executor::PrincipalId::new_v7().to_string())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        assert_eq!(
            read_only_store_view(&root, 28_610).await.unwrap(),
            Some((Some("28600-28610".into()), 1))
        );
    }

    /// Real host probes. Root only (`t1_root`); they write the real `/etc`.
    mod t1_root {
        use super::*;
        use crate::executor::t1::tests::t1_root::require_root;
        use crate::executor::t1_probe::tests::traversable_tempdir;
        use crate::executor::SessionExecutor;
        use crate::server::operator::accounts;

        /// Removes a host user, with whichever backend created it.
        struct HostUser(&'static str);
        impl Drop for HostUser {
            fn drop(&mut self) {
                for tool in ["userdel", "groupdel"] {
                    for dir in ["/usr/sbin", "/sbin"] {
                        let path = std::path::Path::new(dir).join(tool);
                        if path.exists() {
                            let _ = std::process::Command::new(path).arg(self.0).output();
                            break;
                        }
                    }
                }
                let _ = crate::server::operator::etc_files::EtcFiles::system().remove_user(self.0);
            }
        }

        /// §8 end to end, read-only: passes on this host, creates nothing
        /// under a fresh operator root, leaves no scaffold.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_read_only_probe_passes_and_writes_nothing() {
            require_root();
            let tmp = traversable_tempdir();
            let data = tmp.path().join("root");
            let (report, result) = run(&opts(Some(data.clone()), ProbeMode::ReadOnly)).await;
            assert!(result.is_ok(), "{}", report.render_text());
            let names: Vec<_> = report.checks.iter().map(|c| c.check).collect();
            assert_eq!(
                names,
                [
                    "os",
                    "identity",
                    "capabilities",
                    "operator_root",
                    "uid_range",
                    "probe_uid",
                    "test_drop"
                ]
            );
            assert!(!data.exists(), "read-only creates no operator root");
        }

        /// §8 boot: steps 1–7, the stamped executor (I-5), `probe.json`,
        /// and reconcile recreating a host user a redeploy lost.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_boot_probe_reconciles_and_stamps_the_executor() {
            require_root();
            let _cleanup = HostUser("ik-t1probe-ada");
            let tmp = traversable_tempdir();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            let o = opts(Some(root.root().to_path_buf()), ProbeMode::Boot);
            let prov = Provisioner::new(root.clone(), o.uid_range, o.provisioning, Actor::Cli);
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let ada = prov
                .create(&pool, "t1probe-ada", "a long enough password", false, None)
                .await
                .unwrap();
            pool.close().await;
            // A redeploy reset /etc.
            HostUser("ik-t1probe-ada").drop_now();

            let (report, result) = run(&o).await;
            let exec = result.unwrap_or_else(|r| panic!("{r}\n{}", report.render_text()));
            assert!(exec.capabilities().principal_isolation, "I-5");
            assert_eq!(exec.probe_stamp().map(|s| s.ok), Some(true));
            assert_eq!(
                report.reconcile.as_ref().unwrap().created,
                vec!["ik-t1probe-ada".to_string()]
            );
            let pw = sys::user_by_name("ik-t1probe-ada").unwrap().unwrap();
            assert_eq!((pw.uid, pw.home.clone()), (ada.unix_uid, ada.home.clone()));

            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(root.probe_json()).unwrap()).unwrap();
            assert_eq!(json["ok"], true);
            use std::os::unix::fs::MetadataExt;
            let meta = std::fs::metadata(root.probe_json()).unwrap();
            assert_eq!((meta.uid(), meta.mode() & 0o777), (0, 0o600));
            // The scaffold is gone: principals/ holds only ada's dir.
            let entries: Vec<_> = std::fs::read_dir(root.principals_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(
                entries,
                vec![std::ffi::OsString::from(ada.principal_id.to_string())]
            );
        }

        /// The probe uid's precondition: if an account holds it, the probe
        /// refuses before acting as it, and the boot records `probe_failed`.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_boot_refuses_when_an_account_holds_the_probe_uid() {
            require_root();
            let tmp = traversable_tempdir();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            sqlx::query(
                "INSERT INTO accounts (principal_id, username, unix_name, unix_uid, unix_gid, \
                 home, adopted, created_at, updated_at) VALUES (?, 'p', 'ik-p', 28610, 28610, \
                 '/h', 1, 0, 0)",
            )
            .bind(crate::executor::PrincipalId::new_v7().to_string())
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
            let (report, result) =
                run(&opts(Some(root.root().to_path_buf()), ProbeMode::Boot)).await;
            assert!(matches!(
                result,
                Err(Refusal::ProbeFailed {
                    check: "probe_uid",
                    ..
                })
            ));
            assert!(!report.checks.iter().any(|c| c.check == "test_drop"));
            let pool = open_accounts(&root, Opener::Broker).await.unwrap();
            let mut conn = pool.acquire().await.unwrap();
            let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM auth_events")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            assert_eq!(kinds, ["probe_failed"]);
            assert_eq!(accounts::count(&mut conn).await.unwrap(), 1);
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(root.probe_json()).unwrap()).unwrap();
            assert_eq!(json["ok"], false);
        }

        impl HostUser {
            fn drop_now(self) {
                drop(self);
            }
        }
    }
}
