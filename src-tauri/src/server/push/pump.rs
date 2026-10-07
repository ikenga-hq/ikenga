//! The T1 broker's side of the child → broker push channel.
//!
//! One task per **running** principal child long-polls the child's
//! `GET /internal/push/events` ([`super::outbox`]) with that child's token
//! and the internal-call header, validates every event (`k` against the
//! kind enum, `r` against the ref pattern), and hands it to the hub
//! attributed to **that child's** principal. A child can therefore only
//! push to its own principal's devices, whatever it puts in the queue.
//!
//! The pump never launches a child ([`Children::running_endpoint`]) and the
//! child doesn't count the poll as activity, so pushes never keep an idle
//! child alive. A task ends when its child stops answering; the reconcile
//! tick (5 s) starts one for each newly running child.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::outbox::{Batch, EVENTS_PATH};
use super::{PushEvent, PushKind};
use crate::executor::PrincipalId;
use crate::server::broker::children::{ChildEndpoint, Children};

pub const RECONCILE_EVERY: Duration = Duration::from_secs(5);
const WAIT_MS: u64 = 25_000;

/// The events a child batch may yield, already validated. `test` is never
/// accepted from a child (only `access_push_test`, broker-side, sends one),
/// nor are the broker's own kinds.
pub fn accept(principal: PrincipalId, batch: &Batch) -> Vec<PushEvent> {
    batch
        .events
        .iter()
        .filter_map(|e| {
            let kind = PushKind::parse(&e.k)?;
            if matches!(
                kind,
                PushKind::Test | PushKind::Invite | PushKind::Pairing | PushKind::Update
            ) {
                return None;
            }
            super::valid_ref(&e.r).then(|| PushEvent::new(Some(principal), kind, e.r.clone()))
        })
        .collect()
}

/// Start the reconcile loop (the broker, once, after it installed the hub).
pub fn spawn(children: Arc<Children>) {
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_millis(WAIT_MS) + Duration::from_secs(10))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("push pump: no HTTP client: {e}");
                return;
            }
        };
        let mut tasks: HashMap<PrincipalId, (ChildEndpoint, tokio::task::JoinHandle<()>)> =
            HashMap::new();
        let mut tick = tokio::time::interval(RECONCILE_EVERY);
        loop {
            tick.tick().await;
            tasks.retain(|_, (_, h)| !h.is_finished());
            for id in children.running().await {
                let Some(endpoint) = children.running_endpoint(id).await else {
                    continue;
                };
                if tasks.get(&id).is_some_and(|(e, _)| e == &endpoint) {
                    continue;
                }
                if let Some((_, old)) = tasks.remove(&id) {
                    old.abort();
                }
                let handle = tokio::spawn(poll_child(client.clone(), id, endpoint.clone()));
                tasks.insert(id, (endpoint, handle));
            }
        }
    });
}

async fn poll_child(client: reqwest::Client, principal: PrincipalId, endpoint: ChildEndpoint) {
    let url = format!("http://{}{EVENTS_PATH}?waitMs={WAIT_MS}", endpoint.addr);
    let mut failures = 0u32;
    loop {
        let res = client
            .get(&url)
            .bearer_auth(&*endpoint.token)
            .header(
                crate::server::broker::proxy::PRINCIPAL_HEADER,
                principal.to_string(),
            )
            .header(crate::access::INTERNAL_CALL_HEADER, "1")
            .send()
            .await;
        let batch = match res {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => serde_json::from_slice::<Batch>(&b).ok(),
                Err(_) => None,
            },
            Ok(r) => {
                tracing::debug!("push pump: child {principal} answered {}", r.status());
                None
            }
            // The child is gone (reaped, restarted on another port).
            Err(e) if e.is_connect() => return,
            Err(_) => None,
        };
        match batch {
            Some(b) => {
                failures = 0;
                if b.dropped > 0 {
                    tracing::warn!("push pump: child {principal} dropped {} events", b.dropped);
                }
                if let Some(hub) = super::hub() {
                    for ev in accept(principal, &b) {
                        hub.emit(ev);
                    }
                }
            }
            None => {
                failures += 1;
                if failures >= 5 {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::outbox::OutEvent;
    use super::*;

    /// A child can't pick the principal, smuggle text, or send broker kinds.
    #[test]
    fn events_are_validated_and_attributed_to_the_child() {
        let me = PrincipalId::new_v7();
        let ev = |k: &str, r: &str| OutEvent {
            k: k.into(),
            r: r.into(),
            at: 0,
        };
        let batch = Batch {
            events: vec![
                ev("run_finished", "run:abc"),
                ev("permission", "n:9"),
                ev("run_failed", "https://evil/x"),
                ev("bogus", "n:1"),
                ev("test", "test"),
                ev("update", "update"),
                ev("pairing", "pair:x"),
                ev("invite", "n:3"),
            ],
            dropped: 0,
        };
        let out = accept(me, &batch);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.principal == Some(me)));
        assert_eq!(out[0].kind, PushKind::RunFinished);
        assert_eq!(out[1].r, "n:9");
    }
}
