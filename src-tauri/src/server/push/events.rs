//! Producers that ride the notification centre (plans/pwa S3).
//!
//! [`from_notification`] maps a freshly **created** notification row to a
//! push. Only `Created`: a coalesced repeat (a terminal prompt asked again,
//! a violation counted again) never pushes twice. The row stays the source
//! of truth — the push carries only `n:<row id>` / `run:<run id>` and the
//! app fetches the rest after the tap (W4).
//!
//! | row kind | push | ref |
//! |---|---|---|
//! | `permission` | `permission` (TTL = the ask's remaining life, ≤600 s) | `n:<id>` |
//! | `run_finished` / `run_failed` | same | `run:<runId>` |
//! | `invite` | — (the T1 broker pushes it at accept, `access::invites`) | |
//! | `update` / `violation` | — (the desktop app's own updater; violations stay in-app) | |
//! | `system` | — (a local environment problem, e.g. WSL networking; stays on the machine it's about) | |
//!
//! `run_cancelled` has no row (`notifications::run`): `chi_exec` calls
//! [`super::emit_run_cancelled`] directly.

use serde_json::Value;

use super::{PushEvent, PushKind};
use crate::server::shared::notifications::{
    self, ChangeReason, Notification, NotificationEvent, NotificationKind,
};

/// The push a notification row stands for, if any.
pub fn from_notification(n: &Notification, now_ms: i64) -> Option<PushEvent> {
    let action = n.action.as_ref();
    let (kind, r, ttl) = match n.kind {
        NotificationKind::Permission => {
            let ttl = action
                .and_then(|a| a.get("expiresAtMs"))
                .and_then(Value::as_i64)
                .map(|exp| ((exp - now_ms) / 1000).max(0) as u32);
            if ttl == Some(0) {
                return None;
            }
            (PushKind::Permission, format!("n:{}", n.id), ttl)
        }
        NotificationKind::RunFinished | NotificationKind::RunFailed => {
            let run_id = action
                .and_then(|a| a.get("runId"))
                .and_then(Value::as_str)?;
            let kind = if n.kind == NotificationKind::RunFinished {
                PushKind::RunFinished
            } else {
                PushKind::RunFailed
            };
            (kind, format!("run:{run_id}"), None)
        }
        NotificationKind::Invite
        | NotificationKind::Update
        | NotificationKind::Violation
        | NotificationKind::System => return None,
    };
    if !super::valid_ref(&r) {
        return None;
    }
    Some(PushEvent {
        principal: None,
        kind,
        r,
        ttl,
    })
}

/// The event a centre change pushes: created rows only.
pub fn from_event(ev: &NotificationEvent, now_ms: i64) -> Option<PushEvent> {
    if ev.reason != ChangeReason::Created {
        return None;
    }
    from_notification(ev.notification.as_ref()?, now_ms)
}

/// Relay this process's notification centre into [`super::emit`]. Called
/// once by a process that installed a sink (T0 daemon, T1 child).
pub fn spawn_bridge() {
    let mut rx = notifications::subscribe();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    if let Some(push) = from_event(&ev, super::store::now_ms()) {
                        super::emit(push);
                    }
                }
                // Lagged: skip ahead; the table is the truth.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(kind: NotificationKind, title: &str, body: &str, action: Value) -> Notification {
        Notification {
            id: 41,
            kind,
            title: title.into(),
            body: Some(body.into()),
            action: Some(action),
            source: "relay".into(),
            dedupe_key: None,
            count: 1,
            created_at: 0,
            updated_at: 0,
            read_at: None,
            resolved_at: None,
        }
    }

    /// W4: nothing of the ask — title, body, tool, command, path, agent —
    /// reaches the payload. Only the kind and the row id.
    #[test]
    fn permission_push_carries_only_kind_and_row_id() {
        let now = 1_000_000;
        let n = row(
            NotificationKind::Permission,
            "Claude wants to run Bash: rm -rf /home/ned/secrets",
            "agent reviewer-7 · tool Bash · /home/ned/royalti-co",
            json!({"kind": "permission.decide", "key": "permission:hook:abc", "expiresAtMs": now + 120_000,
                   "routing": {"requestedBy": "reviewer-7", "projectId": "royalti-co"}}),
        );
        let ev = from_notification(&n, now).unwrap();
        assert_eq!(ev.kind, PushKind::Permission);
        assert_eq!(ev.r, "n:41");
        assert_eq!(ev.ttl(), 120);
        let payload = String::from_utf8(super::super::payload(ev.kind, &ev.r).unwrap()).unwrap();
        assert_eq!(payload, r#"{"v":1,"k":"permission","r":"n:41"}"#);
        for leak in [
            "Bash", "rm -rf", "secrets", "reviewer", "royalti", "Claude", "hook", "/home",
        ] {
            assert!(!payload.contains(leak), "{leak} leaked into {payload}");
        }
    }

    #[test]
    fn expired_asks_and_non_push_kinds_produce_nothing() {
        let now = 5_000_000;
        let expired = row(
            NotificationKind::Permission,
            "t",
            "b",
            json!({"expiresAtMs": now - 1}),
        );
        assert!(from_notification(&expired, now).is_none());
        for k in [
            NotificationKind::Invite,
            NotificationKind::Update,
            NotificationKind::Violation,
        ] {
            assert!(
                from_notification(&row(k, "t", "b", json!({})), now).is_none(),
                "{k:?}"
            );
        }
    }

    #[test]
    fn runs_use_the_run_id_and_only_created_pushes() {
        let n = row(
            NotificationKind::RunFailed,
            "Deploy the site failed",
            "exit 1 · claude-code · royalti-io-website",
            json!({"kind": "open.chi_run", "runId": "0192f0a4-1b2c-7d3e-8f40-0123456789ab", "status": "failed"}),
        );
        let ev = from_notification(&n, 0).unwrap();
        assert_eq!(ev.kind, PushKind::RunFailed);
        assert_eq!(ev.r, "run:0192f0a4-1b2c-7d3e-8f40-0123456789ab");
        let created = NotificationEvent {
            reason: ChangeReason::Created,
            notification: Some(n.clone()),
            muted: false,
        };
        assert!(from_event(&created, 0).is_some());
        let coalesced = NotificationEvent {
            reason: ChangeReason::Coalesced,
            notification: Some(n),
            muted: false,
        };
        assert!(from_event(&coalesced, 0).is_none());
        // A run id that isn't a plain token is dropped, not forwarded.
        let weird = row(
            NotificationKind::RunFinished,
            "t",
            "b",
            json!({"runId": "../x y"}),
        );
        assert!(from_notification(&weird, 0).is_none());
    }
}
