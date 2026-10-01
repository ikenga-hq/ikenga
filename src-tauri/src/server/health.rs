use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

static START_TIME_SECS: AtomicU64 = AtomicU64::new(0);

pub fn init_uptime() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    START_TIME_SECS.store(now, Ordering::Relaxed);
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub name: &'static str,
    pub version: &'static str,
    pub status: &'static str,
    pub uptime_secs: u64,
    /// The session-executor tier every spawn on this server goes through, and
    /// what it honours (ADR-023, WP-18). Read from the executor actually
    /// installed, not from config, so it can't claim a tier spawns don't get.
    pub executor: crate::executor::Capabilities,
    /// The §8 boot probe this process passed (T1 only; absent otherwise):
    /// `{ok, at}` and nothing more — no uids, names, capability masks or
    /// paths on an unauthenticated route (G-PRINCIPAL §8 "Results"). The
    /// full report is in the log and `operator/probe.json`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<crate::executor::ProbeStamp>,
}

pub async fn health_handler() -> impl IntoResponse {
    Json(health_for(crate::executor::current()))
}

/// The health body for `executor` (the installed one, in production).
pub fn health_for(executor: &dyn crate::executor::SessionExecutor) -> HealthResponse {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let start = START_TIME_SECS.load(Ordering::Relaxed);
    let uptime = if start > 0 {
        now.saturating_sub(start)
    } else {
        0
    };

    HealthResponse {
        ok: true,
        name: "ikenga-server",
        version: env!("CARGO_PKG_VERSION"),
        status: "ready",
        uptime_secs: uptime,
        executor: executor.capabilities(),
        probe: executor.probe_stamp(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t0_health_has_no_probe_field() {
        let json = serde_json::to_value(health_for(&crate::executor::InProcessExecutor)).unwrap();
        assert_eq!(json["executor"]["tier"], "t0");
        assert!(json.get("probe").is_none(), "{json}");
    }

    /// I-5 on the wire: isolation only from a passing probe, off once the
    /// executor is degraded, and the probe field carries `ok`/`at` only.
    #[cfg(target_os = "linux")]
    #[test]
    fn t1_health_reports_the_probe_and_isolation_from_the_executor() {
        use crate::executor::t1::{T1Config, T1Executor};
        use crate::executor::ProbeStamp;
        let config = T1Config {
            principals_dir: "/srv/ikenga/principals".into(),
            principal_path: None,
        };

        let unprobed = serde_json::to_value(health_for(&T1Executor::new(config.clone()))).unwrap();
        assert_eq!(unprobed["executor"]["tier"], "t1");
        assert_eq!(unprobed["executor"]["principal_isolation"], false);
        assert!(unprobed.get("probe").is_none());

        let exec = T1Executor::with_probe(
            config,
            ProbeStamp {
                ok: true,
                at: 1_700_000_000,
            },
        );
        let json = serde_json::to_value(health_for(&exec)).unwrap();
        assert_eq!(json["executor"]["principal_isolation"], true);
        assert_eq!(
            json["probe"],
            serde_json::json!({ "ok": true, "at": 1_700_000_000u64 })
        );
        // Unchanged Capabilities shape: exactly the four fields.
        assert_eq!(json["executor"].as_object().unwrap().len(), 4);

        exec.degrade_for_tests();
        let json = serde_json::to_value(health_for(&exec)).unwrap();
        assert_eq!(json["executor"]["principal_isolation"], false);
    }
}
