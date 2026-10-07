//! The `fix.wsl_network` notification (honest-failure-states WP-2, D-2, D-8,
//! D-19): WSL in a distro has no working network.
//!
//! Ungated (WP-7, D-21): raised by the desktop's `wsl_health_*` commands and
//! by the shared Chi run path (`server::shared::chi_exec`), which probes WSL
//! before launching a `wsl:` engine — in both binaries. The desktop's
//! `crate::notifications::producers` re-exports the builder.

use serde_json::{json, Value};

use super::run::{truncate, BODY_MAX, TITLE_MAX};
use super::{Coalesce, NewNotification, NotificationKind};
use crate::server::shared::wsl_health::{WslHealth, WslHealthState};

pub const SOURCE_WSL_HEALTH: &str = "wsl.health";

/// Prefix shared by every [`wsl_network_key`].
pub const WSL_NETWORK_KEY_PREFIX: &str = "wsl:network:";

/// Dedupe key of a distro's WSL network problem: `wsl:network:<distro>`
/// (lower-cased; `default` for the default distro). One row per episode
/// ([`Coalesce::WhileUnresolved`]); resolved by the next `ok` probe.
pub fn wsl_network_key(distro: Option<&str>) -> String {
    format!(
        "{WSL_NETWORK_KEY_PREFIX}{}",
        distro.map_or_else(|| "default".to_string(), str::to_ascii_lowercase)
    )
}

/// A WSL network problem worth telling the user about (D-2, D-8), or `None`
/// when `health` isn't WSL's fault (`ok`, `host_offline`, `not_installed`).
/// Uses the `system` kind (D-19): an environment problem, not a pkg's
/// denial, and mutable on its own.
pub fn wsl_network(
    health: &WslHealth,
) -> Option<NewNotification> {
    let title = match health.state {
        WslHealthState::NoRoute => "WSL has no network",
        WslHealthState::DnsOnly => "WSL can't resolve names",
        WslHealthState::WslDown => "WSL isn't starting",
        _ => return None,
    };
    let distro = health.distro.as_deref();
    Some(NewNotification {
        kind: NotificationKind::System,
        title: truncate(
            &format!("{title} · {}", distro.unwrap_or("default distro")),
            TITLE_MAX,
        ),
        body: Some(truncate(&health.detail, BODY_MAX)),
        action: Some(json!({
            "kind": "fix.wsl_network",
            "distro": distro,
            "state": health.state.as_str(),
        })),
        source: SOURCE_WSL_HEALTH.into(),
        dedupe_key: Some(wsl_network_key(distro)),
        coalesce: Coalesce::WhileUnresolved,
    })
}

/// The open `fix.wsl_network` rows a freshly measured `health` ends, given
/// the open rows under [`WSL_NETWORK_KEY_PREFIX`] as `(key, action)`.
///
/// * `ok` ends its own distro's row, and every row whose state was
///   `no_route` / `wsl_down`: all WSL 2 distros share one VM and one network,
///   so those can't still hold when any distro is online. (This is also what
///   ties a row raised for `default` to a probe of the same distro by name.)
///   Another distro's `dns_only` stays: `/etc/resolv.conf` is per distro.
/// * `not_installed` ends its own row (the distro is gone), and every row
///   when it is about WSL as a whole (no distro named).
/// * Anything else ends nothing (`host_offline` says nothing about WSL).
pub fn wsl_network_keys_to_resolve(
    health: &WslHealth,
    open: &[(String, Option<Value>)],
) -> Vec<String> {
    let own = wsl_network_key(health.distro.as_deref());
    let mut out: Vec<String> = Vec::new();
    let mut push = |k: &str| {
        if !out.iter().any(|x| x == k) {
            out.push(k.to_string());
        }
    };
    match health.state {
        WslHealthState::Ok => {
            push(&own);
            for (key, action) in open {
                let state = action.as_ref().and_then(|a| a.get("state")).and_then(Value::as_str);
                if matches!(state, Some("no_route" | "wsl_down")) {
                    push(key);
                }
            }
        }
        WslHealthState::NotInstalled => {
            push(&own);
            if health.distro.is_none() {
                for (key, _) in open {
                    push(key);
                }
            }
        }
        _ => {}
    }
    // Only rows that are actually open (and in this family).
    out.retain(|k| open.iter().any(|(o, _)| o == k));
    out
}

/// Raise or resolve `fix.wsl_network` rows for a **freshly measured**
/// `health` (a cached result was already reported when it was measured):
/// a WSL-caused failure records its row (one per episode per distro); `ok`
/// / `not_installed` end the rows [`wsl_network_keys_to_resolve`] names.
/// Best-effort: a notification write never fails the caller.
pub async fn report_with_db(db: &crate::db::PaDb, health: &WslHealth) {
    if let Some(n) = wsl_network(health) {
        super::record_with_db(db, n).await;
        return;
    }
    if !matches!(health.state, WslHealthState::Ok | WslHealthState::NotInstalled) {
        return;
    }
    let pool = match db.ensure_pool().await {
        Ok(pool) => pool,
        Err(e) => {
            log::warn!(target: "ikenga::notifications", "no db pool: {e}");
            return;
        }
    };
    let open = match super::open_rows_with_key_prefix(&pool, WSL_NETWORK_KEY_PREFIX).await {
        Ok(open) => open,
        Err(e) => {
            log::warn!(target: "ikenga::notifications", "{e}");
            return;
        }
    };
    for key in wsl_network_keys_to_resolve(health, &open) {
        if let Err(e) = super::resolve_by_key(&pool, &key).await {
            log::warn!(target: "ikenga::notifications", "could not resolve {key}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::WslHealthState as S;

    fn wsl_health(
        state: WslHealthState,
        distro: Option<&str>,
    ) -> WslHealth {
        WslHealth {
            state,
            distro: distro.map(str::to_string),
            detail: "WSL has no network: only the loopback interface is up.".into(),
            mirrored_failure: None,
            networking_mode: Some("mirrored".into()),
            checked_at: 1,
            inconclusive: false,
        }
    }

    #[test]
    fn wsl_network_is_one_fix_row_per_distro() {
        let n = wsl_network(&wsl_health(S::NoRoute, Some("Ubuntu"))).unwrap();
        assert_eq!(n.kind, NotificationKind::System);
        assert_eq!(n.title, "WSL has no network · Ubuntu");
        assert_eq!(n.dedupe_key.as_deref(), Some("wsl:network:ubuntu"));
        assert_eq!(n.coalesce, Coalesce::WhileUnresolved);
        assert_eq!(n.source, SOURCE_WSL_HEALTH);
        assert_eq!(
            n.action,
            Some(json!({ "kind": "fix.wsl_network", "distro": "Ubuntu", "state": "no_route" }))
        );
        let d = wsl_network(&wsl_health(S::DnsOnly, None)).unwrap();
        assert_eq!(d.dedupe_key.as_deref(), Some("wsl:network:default"));
        assert_eq!(
            d.action,
            Some(json!({ "kind": "fix.wsl_network", "distro": null, "state": "dns_only" }))
        );
        assert!(wsl_network(&wsl_health(S::WslDown, None)).is_some());
        for quiet in [S::Ok, S::HostOffline, S::NotInstalled] {
            assert_eq!(wsl_network(&wsl_health(quiet, Some("Ubuntu"))), None);
        }
    }

    #[test]
    fn wsl_recovery_ends_vm_wide_rows_but_not_another_distros_dns() {
        let row = |key: &str, state: &str| {
            (key.to_string(), Some(json!({ "kind": "fix.wsl_network", "state": state })))
        };
        let open = vec![
            row("wsl:network:default", "no_route"),
            row("wsl:network:ubuntu", "dns_only"),
            row("wsl:network:debian", "dns_only"),
            row("wsl:network:arch", "wsl_down"),
        ];
        // `Ubuntu` is online: its own row, plus the VM-wide ones (the
        // `default` row raised from a tab without -d included).
        let mut got = wsl_network_keys_to_resolve(&wsl_health(S::Ok, Some("Ubuntu")), &open);
        got.sort();
        assert_eq!(got, ["wsl:network:arch", "wsl:network:default", "wsl:network:ubuntu"]);
        // A removed distro ends its own row; WSL gone entirely ends all.
        assert_eq!(
            wsl_network_keys_to_resolve(&wsl_health(S::NotInstalled, Some("Debian")), &open),
            ["wsl:network:debian"]
        );
        assert_eq!(wsl_network_keys_to_resolve(&wsl_health(S::NotInstalled, None), &open).len(), 4);
        // Still failing, or the PC offline: nothing ends.
        for s in [S::HostOffline, S::NoRoute, S::DnsOnly, S::WslDown] {
            assert!(wsl_network_keys_to_resolve(&wsl_health(s, Some("Ubuntu")), &open).is_empty());
        }
        // No open row for the probed distro: nothing to resolve.
        assert!(wsl_network_keys_to_resolve(&wsl_health(S::Ok, Some("Alpine")), &[]).is_empty());
    }

    /// The shared report records a fault once per episode and the next `ok`
    /// ends it — the same path the desktop command and the Chi pre-run probe
    /// both use.
    #[tokio::test]
    async fn report_records_a_fault_and_ok_resolves_it() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::PaDb::new(tmp.path().join("ikenga.db"));
        report_with_db(&db, &wsl_health(S::NoRoute, Some("Ubuntu"))).await;
        report_with_db(&db, &wsl_health(S::NoRoute, Some("Ubuntu"))).await;
        let pool = db.ensure_pool().await.unwrap();
        let open = crate::server::shared::notifications::open_rows_with_key_prefix(
            &pool,
            WSL_NETWORK_KEY_PREFIX,
        )
        .await
        .unwrap();
        assert_eq!(open.len(), 1, "{open:?}");
        assert_eq!(open[0].0, "wsl:network:ubuntu");
        // Not WSL's fault: nothing changes.
        report_with_db(&db, &wsl_health(S::HostOffline, Some("Ubuntu"))).await;
        assert_eq!(
            crate::server::shared::notifications::open_rows_with_key_prefix(&pool, WSL_NETWORK_KEY_PREFIX)
                .await
                .unwrap()
                .len(),
            1
        );
        report_with_db(&db, &wsl_health(S::Ok, Some("Ubuntu"))).await;
        assert!(crate::server::shared::notifications::open_rows_with_key_prefix(
            &pool,
            WSL_NETWORK_KEY_PREFIX
        )
        .await
        .unwrap()
        .is_empty());
    }
}
