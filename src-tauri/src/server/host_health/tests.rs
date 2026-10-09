use super::*;
use crate::access::caps::{CapSet, Tier};
use crate::access::ctx::{RequestMeta, Via};
use crate::access::rpc::PrincipalInfo;
use crate::access::sockets::Registry;
use crate::executor::PrincipalId;
use serde_json::json;

// ─── fixtures (copied from a real box, 2026-10-08) ──────────────────────────

const LOADAVG: &str = "0.52 0.41 0.30 2/412 18873\n";

const MEMINFO: &str = "\
MemTotal:        3965792 kB
MemFree:          212340 kB
MemAvailable:    1874560 kB
Buffers:          102400 kB
Cached:          1650000 kB
SwapCached:            0 kB
SwapTotal:             0 kB
SwapFree:              0 kB
HugePages_Total:       0
";

const MEMINFO_SWAP: &str = "\
MemTotal:        3965792 kB
MemAvailable:    1874560 kB
SwapTotal:       2097148 kB
SwapFree:        1572860 kB
";

const PSI_MEMORY: &str = "\
some avg10=0.95 avg60=2.54 avg300=3.68 total=25563918341
full avg10=0.50 avg60=1.76 avg300=2.06 total=17994778257
";

/// Kernels before 5.13 (and every CPU file) report no `full` line.
const PSI_CPU: &str = "some avg10=19.79 avg60=16.45 avg300=24.10 total=341308591908\n";

const BACKUP_STATUS: &str = r#"{
  "schema": 1, "enabled": true, "updated": "2026-10-08T02:00:41Z",
  "databases": {
    "royalti-prod-db": {
      "schedule": "4hourly",
      "last_attempt": "2026-10-08T00:00:12Z", "last_attempt_ok": true,
      "last_success": "2026-10-08T00:00:12Z",
      "last_error_kind": null, "last_error_at": null,
      "object": "gs://db-backups-archive/royalti-prod-db/2026/10/royalti-prod-db-20261008-000012.sql.gz",
      "bytes": 48211934, "duration_s": 31
    },
    "devotee-db": {
      "schedule": "daily",
      "last_attempt": "2026-10-08T02:00:41Z", "last_attempt_ok": false,
      "last_success": "2026-10-07T02:00:30Z",
      "last_error_kind": "upload-failed", "last_error_at": "2026-10-08T02:00:41Z",
      "object": "gs://db-backups-archive/devotee-db/2026/10/x.sql.gz",
      "bytes": 1200, "duration_s": 4
    }
  },
  "schedules": {
    "4hourly": { "last_run": "2026-10-08T00:00:12Z", "last_run_ok": true, "succeeded": 1, "failed": [] },
    "daily": { "last_run": "2026-10-08T02:00:41Z", "last_run_ok": false, "succeeded": 0, "failed": ["devotee-db"] }
  }
}"#;

const UNIT_FILES: &str = "\
ikenga-backup-4hourly.timer enabled  enabled
ikenga-backup-daily.timer   enabled  enabled
ikenga-backup-weekly.timer  enabled  enabled
devotee-db-tunnel.service   enabled  enabled
";

const FAILED_UNITS: &str = "\
● ikenga-backup@daily.service loaded failed failed Database backup (daily)
";

const SYSTEMCTL_SHOW: &str = "\
Id=ikenga-backup-daily.timer
LoadState=loaded
ActiveState=active
SubState=waiting
Result=success
LastTriggerUSec=Thu 2026-10-08 02:00:00 UTC
NextElapseUSecRealtime=Fri 2026-10-09 02:00:00 UTC

Id=ikenga-backup-weekly.timer
LoadState=loaded
ActiveState=active
SubState=waiting
Result=success
LastTriggerUSec=n/a
NextElapseUSecRealtime=Sun 2026-10-11 03:00:00 UTC

Id=devotee-db-tunnel.service
LoadState=loaded
ActiveState=failed
SubState=failed
Result=exit-code

Id=ikenga-backup@daily.service
LoadState=loaded
ActiveState=failed
SubState=failed
Result=exit-code

Id=gone-tunnel.service
LoadState=not-found
ActiveState=inactive
SubState=dead
Result=success
";

// ─── /proc parsers ──────────────────────────────────────────────────────────

#[test]
fn loadavg_parses_three_averages_and_refuses_garbage() {
    let l = parse_loadavg(LOADAVG).unwrap();
    assert_eq!((l.m1, l.m5, l.m15), (0.52, 0.41, 0.30));
    assert_eq!(parse_loadavg(""), None);
    assert_eq!(parse_loadavg("a b c"), None);
    assert_eq!(parse_loadavg("0.1 0.2"), None);
}

#[test]
fn meminfo_reads_total_available_and_a_swapless_box() {
    let (mem, swap) = parse_meminfo(MEMINFO);
    let mem = mem.unwrap();
    assert_eq!(mem.total_bytes, 3_965_792 * 1024);
    assert_eq!(mem.available_bytes, 1_874_560 * 1024);
    // No swap is a fact the card needs (warns when memory is also high).
    assert_eq!(
        swap.unwrap(),
        Swap {
            total_bytes: 0,
            used_bytes: 0
        }
    );
}

#[test]
fn meminfo_swap_used_is_total_minus_free() {
    let (_, swap) = parse_meminfo(MEMINFO_SWAP);
    let swap = swap.unwrap();
    assert_eq!(swap.total_bytes, 2_097_148 * 1024);
    assert_eq!(swap.used_bytes, (2_097_148 - 1_572_860) * 1024);
}

#[test]
fn meminfo_without_memavailable_has_no_memory_rather_than_a_guess() {
    let (mem, swap) = parse_meminfo("MemTotal: 100 kB\nMemFree: 50 kB\n");
    assert_eq!(mem, None);
    assert_eq!(swap, None);
    assert_eq!(parse_meminfo("").0, None);
    // Available can never exceed total.
    let (mem, _) = parse_meminfo("MemTotal: 100 kB\nMemAvailable: 900 kB\n");
    assert_eq!(mem.unwrap().available_bytes, 100 * 1024);
}

#[test]
fn pressure_reads_some_and_full_and_tolerates_a_missing_full() {
    let m = parse_pressure(PSI_MEMORY).unwrap();
    assert_eq!((m.some_avg10, m.some_avg60), (0.95, 2.54));
    assert_eq!((m.full_avg10, m.full_avg60), (Some(0.50), Some(1.76)));

    let c = parse_pressure(PSI_CPU).unwrap();
    assert_eq!((c.some_avg10, c.some_avg60), (19.79, 16.45));
    assert_eq!((c.full_avg10, c.full_avg60), (None, None));

    assert_eq!(parse_pressure(""), None);
    assert_eq!(parse_pressure("full avg10=1.0 avg60=1.0\n"), None);
    assert_eq!(parse_pressure("some avg10=x avg60=1.0\n"), None);
}

#[test]
fn uptime_is_whole_seconds() {
    assert_eq!(parse_uptime("4019275.82 14806495.18\n"), Some(4_019_275));
    assert_eq!(parse_uptime("-1 0"), None);
    assert_eq!(parse_uptime("nan 0"), None);
    assert_eq!(parse_uptime(""), None);
}

// ─── backup status.json ─────────────────────────────────────────────────────

#[test]
fn backup_status_reads_only_the_documented_fields() {
    let b = parse_backup_status(BACKUP_STATUS).unwrap();
    assert!(b.enabled);
    assert_eq!(b.updated.as_deref(), Some("2026-10-08T02:00:41Z"));
    assert_eq!(b.databases.len(), 2);
    let prod = b
        .databases
        .iter()
        .find(|d| d.name == "royalti-prod-db")
        .unwrap();
    assert_eq!(prod.schedule.as_deref(), Some("4hourly"));
    assert_eq!(prod.last_attempt_ok, Some(true));
    assert_eq!(prod.last_error_kind, None);
    assert_eq!(prod.bytes, Some(48_211_934));
    assert_eq!(prod.duration_s, Some(31));
    let dev = b.databases.iter().find(|d| d.name == "devotee-db").unwrap();
    assert_eq!(dev.last_error_kind.as_deref(), Some("upload-failed"));
    assert_eq!(dev.last_success.as_deref(), Some("2026-10-07T02:00:30Z"));
    let daily = b.schedules.iter().find(|s| s.name == "daily").unwrap();
    assert_eq!(daily.last_run_ok, Some(false));
    assert_eq!(daily.failed, vec!["devotee-db".to_string()]);

    // The destination object is never copied out, in any form.
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("gs://"), "{json}");
    assert!(!json.contains("object"), "{json}");
    assert!(!json.contains(".sql.gz"), "{json}");
}

#[test]
fn backup_status_refuses_other_schemas_and_non_json() {
    assert_eq!(
        parse_backup_status(r#"{"schema": 2, "databases": {}}"#),
        None
    );
    assert_eq!(parse_backup_status(r#"{"databases": {}}"#), None);
    assert_eq!(parse_backup_status("not json"), None);
    assert_eq!(parse_backup_status(""), None);
}

#[test]
fn backup_status_sanitises_what_it_does_show() {
    let b = parse_backup_status(
        r#"{"schema":1,"enabled":false,"databases":{
            "ok-db":{"last_error_kind":"gcs-auth","last_success":"yesterday at noon"},
            "weird":{"last_error_kind":"Some Free Text With Spaces"},
            "bad name!":{"last_error_kind":"gcs-auth"},
            "../escape":{}
          },"schedules":{"daily":{"failed":["ok-db","bad name!"]}}}"#,
    )
    .unwrap();
    assert!(!b.enabled);
    let names: Vec<_> = b.databases.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names, vec!["ok-db", "weird"]);
    let ok = &b.databases[0];
    assert_eq!(ok.last_error_kind.as_deref(), Some("gcs-auth"));
    // A time that is not an ISO instant is dropped, not echoed.
    assert_eq!(ok.last_success, None);
    // An error kind outside the token shape is shown as `unknown`.
    assert_eq!(b.databases[1].last_error_kind.as_deref(), Some("unknown"));
    assert_eq!(b.schedules[0].failed, vec!["ok-db".to_string()]);
}

#[test]
fn read_backups_reads_a_plain_small_file_only() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("status.json");
    std::fs::write(&file, BACKUP_STATUS).unwrap();
    assert_eq!(read_backups(&file).unwrap().databases.len(), 2);

    assert_eq!(read_backups(&dir.path().join("missing.json")), None);
    // A directory is not a status file.
    assert_eq!(read_backups(dir.path()), None);
    // Nor is one past the size cap.
    let big = dir.path().join("big.json");
    std::fs::write(&big, vec![b' '; (MAX_STATUS_FILE + 1) as usize]).unwrap();
    assert_eq!(read_backups(&big), None);
}

// ─── systemd ────────────────────────────────────────────────────────────────

#[test]
fn unit_list_keeps_only_the_three_shapes_and_drops_the_bullet() {
    let files = parse_unit_list(UNIT_FILES);
    assert_eq!(
        files,
        vec![
            "ikenga-backup-4hourly.timer",
            "ikenga-backup-daily.timer",
            "ikenga-backup-weekly.timer",
            "devotee-db-tunnel.service",
        ]
    );
    assert_eq!(
        parse_unit_list(FAILED_UNITS),
        vec!["ikenga-backup@daily.service"]
    );
}

#[test]
fn unit_names_are_screened_before_they_become_arguments() {
    for bad in [
        "",
        "sshd.service",
        "ikenga-backup-daily.timer; rm -rf /",
        "ikenga-backup-$(id).timer",
        "../ikenga-backup-x.timer",
        "-tunnel.service",
        "ikenga-backup-x.service",
        "a b-tunnel.service",
    ] {
        assert!(!unit_name_ok(bad), "{bad:?}");
    }
    for ok in [
        "ikenga-backup-daily.timer",
        "devotee-db-tunnel.service",
        "ikenga-backup@daily.service",
    ] {
        assert!(unit_name_ok(ok), "{ok:?}");
    }
    // A hostile listing line never reaches `systemctl show`.
    assert!(parse_unit_list("evil.service enabled\nikenga-backup-a.timer;x enabled\n").is_empty());
}

#[test]
fn unit_timestamps_parse_in_utc_and_refuse_everything_else() {
    assert_eq!(
        parse_unit_timestamp("Thu 2026-10-08 18:39:49 UTC"),
        Some(1_791_484_789)
    );
    for bad in [
        "",
        "n/a",
        "0",
        "Thu 2026-10-08 18:39:49 WAT",
        "2026-10-08",
        "Thu garbage UTC",
    ] {
        assert_eq!(parse_unit_timestamp(bad), None, "{bad:?}");
    }
}

#[test]
fn systemctl_show_blocks_become_unit_states() {
    let units = parse_systemctl_show(SYSTEMCTL_SHOW);
    let names: Vec<_> = units.iter().map(|u| u.name.as_str()).collect();
    // `gone-tunnel.service` is not-found: dropped.
    assert_eq!(
        names,
        vec![
            "ikenga-backup-daily.timer",
            "ikenga-backup-weekly.timer",
            "devotee-db-tunnel.service",
            "ikenga-backup@daily.service",
        ]
    );
    let daily = &units[0];
    assert_eq!(daily.kind, "timer");
    assert_eq!(daily.active_state, "active");
    assert_eq!(daily.sub_state.as_deref(), Some("waiting"));
    assert_eq!(
        daily.last_trigger,
        parse_unit_timestamp("Thu 2026-10-08 02:00:00 UTC")
    );
    assert_eq!(
        daily.next_elapse,
        parse_unit_timestamp("Fri 2026-10-09 02:00:00 UTC")
    );
    assert!(!daily.failed());
    assert_eq!(daily.result, None, "a timer's Result is not its health");

    // A timer that has never fired.
    assert_eq!(units[1].last_trigger, None);
    assert!(units[1].next_elapse.is_some());

    let tunnel = &units[2];
    assert_eq!(tunnel.kind, "tunnel");
    assert_eq!(tunnel.result.as_deref(), Some("exit-code"));
    assert!(tunnel.failed());
    assert_eq!(tunnel.last_trigger, None);

    let run = &units[3];
    assert_eq!(run.kind, "backup_run");
    assert!(run.failed());
}

#[test]
fn a_running_tunnel_and_an_inactive_one_are_not_failed() {
    let up = parse_systemctl_show(
        "Id=a-tunnel.service\nLoadState=loaded\nActiveState=active\nSubState=running\nResult=success\n",
    );
    assert!(!up[0].failed());
    let down = parse_systemctl_show(
        "Id=a-tunnel.service\nLoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\n",
    );
    assert!(!down[0].failed());
    assert_eq!(down[0].active_state, "inactive");
    // Garbage and foreign units produce nothing.
    assert!(parse_systemctl_show("garbage\n\nId=sshd.service\nActiveState=active\n").is_empty());
}

// ─── who may ask ────────────────────────────────────────────────────────────

fn ctx(via: Via, tier: Tier) -> AccessCtx {
    AccessCtx {
        principal_id: PrincipalId::new_v7(),
        admin_strength: AccessCtx::admin_strength_of(&via, tier),
        via,
        device_id: None,
        tier,
        share: None,
        share_headers: false,
        caps: CapSet::ALL,
        meta: RequestMeta::default(),
    }
}

fn session() -> Via {
    Via::Session {
        session_id: "s".into(),
    }
}

fn device() -> Via {
    Via::Device {
        device_id: "d".into(),
    }
}

#[test]
fn t1_admin_with_admin_strength_may_ask() {
    assert!(authorize(StoreTier::T1, true, &ctx(session(), Tier::Full)).is_ok());
    assert!(authorize(StoreTier::T1, true, &ctx(device(), Tier::Full)).is_ok());
}

#[test]
fn t1_member_is_refused_whatever_their_credential() {
    for c in [
        ctx(session(), Tier::Full),
        ctx(device(), Tier::Full),
        ctx(device(), Tier::View),
    ] {
        let e = authorize(StoreTier::T1, false, &c).unwrap_err();
        assert_eq!(e.code, Code::Forbidden);
        assert!(e.message.contains("administrators"), "{}", e.message);
    }
}

#[test]
fn t1_admin_on_a_weak_device_is_refused() {
    for tier in [Tier::View, Tier::Dispatch, Tier::Approve] {
        let e = authorize(StoreTier::T1, true, &ctx(device(), tier)).unwrap_err();
        assert_eq!(e.code, Code::Forbidden, "{tier}");
    }
}

#[test]
fn t0_owner_may_ask_a_view_device_may_not() {
    assert!(authorize(StoreTier::T0, false, &ctx(Via::Operator, Tier::Full)).is_ok());
    assert!(authorize(StoreTier::T0, false, &ctx(device(), Tier::Full)).is_ok());
    for tier in [Tier::View, Tier::Dispatch, Tier::Approve] {
        assert!(
            authorize(StoreTier::T0, false, &ctx(device(), tier)).is_err(),
            "{tier}"
        );
    }
}

#[test]
fn a_share_is_never_enough() {
    let mut c = ctx(session(), Tier::Full);
    c.share_headers = true;
    assert!(authorize(StoreTier::T1, true, &c).is_err());
    assert!(authorize(StoreTier::T0, true, &c).is_err());
}

fn env<'a>(tier: StoreTier, is_admin: bool, sockets: &'a Registry) -> Env<'a> {
    Env {
        tier,
        store: None,
        pairing: None,
        sockets,
        principal: PrincipalInfo {
            username: "someone".into(),
            is_admin,
        },
        public_url: None,
        insecure_cookie: false,
    }
}

/// The arm itself, through the dispatcher the T0 daemon and the T1 broker
/// both call: a non-admin T1 principal is refused before anything is read.
#[tokio::test]
async fn the_arm_refuses_a_non_admin_t1_principal() {
    let reg = Registry::new();
    let c = ctx(session(), Tier::Full);
    let e = crate::access::rpc::dispatch(
        &env(StoreTier::T1, false, &reg),
        &c,
        "server_health",
        &json!({}),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, Code::Forbidden);
    assert!(
        !e.message.contains("version"),
        "the refusal carries no data"
    );
}

#[tokio::test]
async fn the_arm_answers_a_t1_admin_with_a_snapshot() {
    let reg = Registry::new();
    let c = ctx(session(), Tier::Full);
    let v = crate::access::rpc::dispatch(
        &env(StoreTier::T1, true, &reg),
        &c,
        "server_health",
        &json!({}),
    )
    .await
    .unwrap();
    assert_eq!(v["schema"], 1);
    assert_eq!(v["tier"], "t1");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["unavailable"].is_array());
}

/// A principal child never serves it: the broker does.
#[tokio::test]
async fn a_principal_child_answers_served_by_broker() {
    let child = crate::access::DaemonAccess::principal_child(Default::default());
    let c = child.child_ctx(&Default::default(), RequestMeta::default());
    let r =
        crate::access::rpc::serve_daemon(Some(&child), Some(&c), "server_health", &json!({})).await;
    assert!(r.error.unwrap().starts_with("served_by_broker:"));
}

#[test]
fn the_arm_is_broker_served_and_access_class() {
    assert!(crate::access::is_broker_arm("server_health"));
    assert!(crate::access::is_broker_arm("access_status"));
    assert!(!crate::access::is_broker_arm("server_open_terminals"));
    assert!(!crate::access::is_broker_arm("pty_list"));
    assert_eq!(
        crate::access::rpc_requirements::requirement("server_health").class,
        crate::access::ArmClass::Access
    );
}

// ─── cache ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn one_measurement_serves_every_caller_inside_the_ttl() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let cache = Cached::new();
    let n = Arc::new(AtomicU32::new(0));
    let measure = || {
        let n = n.clone();
        async move { json!({ "n": n.fetch_add(1, Ordering::SeqCst) }) }
    };
    let ttl = Duration::from_millis(80);
    let budget = Duration::from_secs(1);
    // Four concurrent callers share one measurement.
    let all = futures_util::future::join_all((0..4).map(|_| cache.get(ttl, budget, measure))).await;
    assert!(all.iter().all(|r| r.as_ref().unwrap()["n"] == 0));
    assert_eq!(n.load(Ordering::SeqCst), 1);
    // After the ttl a new one is taken.
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(cache.get(ttl, budget, measure).await.unwrap()["n"], 1);
    assert_eq!(n.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_slow_measurement_serves_the_last_good_one_marked_stale() {
    let cache = Cached::new();
    let ttl = Duration::from_millis(1);
    let budget = Duration::from_millis(60);
    let first = cache
        .get(ttl, budget, || async { json!({ "n": 1 }) })
        .await
        .unwrap();
    assert!(first.get("stale").is_none());
    tokio::time::sleep(Duration::from_millis(10)).await;
    let slow = cache
        .get(ttl, budget, || async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            json!({ "n": 2 })
        })
        .await
        .unwrap();
    assert_eq!(slow["n"], 1);
    assert_eq!(slow["stale"], true);
}

#[tokio::test]
async fn a_slow_first_measurement_is_an_error_not_a_hang() {
    let cache = Cached::new();
    let e = cache
        .get(CACHE_TTL, Duration::from_millis(40), || async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            json!({})
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, Code::Internal);
}

// ─── assembly and the local readers ─────────────────────────────────────────

#[test]
fn every_absent_section_is_named_and_omitted_from_the_json() {
    let h = assemble("t0", Local::default(), None);
    let v = serde_json::to_value(&h).unwrap();
    for key in [
        "load",
        "memory",
        "swap",
        "pressure",
        "disk",
        "backups",
        "units",
        "accounts",
        "uptime_secs",
    ] {
        assert!(v.get(key).is_none(), "{key} should be absent: {v}");
    }
    let unavailable: Vec<_> = v["unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    for key in [
        "load", "memory", "swap", "pressure", "disk", "uptime", "backups", "units", "accounts",
    ] {
        assert!(
            unavailable.contains(&key),
            "{key} not listed in {unavailable:?}"
        );
    }
    assert!(v["cpu_count"].as_u64().unwrap_or(1) >= 1);
}

#[cfg(unix)]
#[test]
fn disk_of_reports_a_consistent_filesystem() {
    let d = disk_of(Path::new(".")).unwrap();
    assert!(d.total_bytes > 0);
    assert!(d.free_bytes <= d.total_bytes);
    assert_eq!(disk_of(Path::new("/definitely/not/a/path")), None);
}

#[cfg(target_os = "linux")]
#[test]
fn the_local_readers_work_on_this_machine() {
    let dir = tempfile::tempdir().unwrap();
    let local = collect_local(dir.path(), &dir.path().join("none.json"));
    assert!(local.load.is_some());
    assert!(local.memory.is_some());
    assert!(local.swap.is_some());
    assert!(local.uptime_secs.unwrap() > 0);
    assert!(local.disk.is_some());
    assert!(
        local.backups.is_none(),
        "no status file: absent, not an error"
    );
    // This process's own uid has a `comm`-matching count of at least zero,
    // and a name nothing has counts zero.
    assert_eq!(count_own_processes("no-such-process-name-xyz"), Some(0));
}

#[cfg(target_os = "linux")]
#[test]
fn run_capped_kills_a_command_that_overruns_and_caps_nothing_else() {
    let sleep = ["/usr/bin/sleep", "/bin/sleep"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file());
    if let Some(sleep) = sleep {
        let t = Instant::now();
        assert_eq!(
            run_capped(&sleep, &["5".to_string()], Duration::from_millis(150)),
            None
        );
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "must not wait for the child"
        );
    }
    let echo = ["/usr/bin/echo", "/bin/echo"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file());
    if let Some(echo) = echo {
        assert_eq!(
            run_capped(&echo, &["hi".to_string()], Duration::from_secs(2)).as_deref(),
            Some("hi\n")
        );
    }
}
