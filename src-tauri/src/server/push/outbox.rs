//! The T1 principal child's side of push: a bounded queue the broker drains
//! over `GET /internal/push/events?waitMs=` (an `internal` route: the
//! per-child token plus the broker's internal-call header, never reachable
//! through the broker proxy).
//!
//! A child holds no key and no subscription. It only says "kind + opaque
//! ref happened"; the broker validates both and attributes the event to
//! **this child's** principal ([`super::pump`]), so a compromised child can
//! only ever push to its own principal's devices.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::PushKind;

/// The route the broker long-polls.
pub const EVENTS_PATH: &str = "/internal/push/events";
pub const CAPACITY: usize = 256;
/// The longest a poll may hold.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutEvent {
    pub k: String,
    pub r: String,
    pub at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Batch {
    pub events: Vec<OutEvent>,
    /// Events dropped since the last poll (the queue was full).
    pub dropped: u64,
}

#[derive(Default)]
pub struct Outbox {
    inner: Mutex<(VecDeque<OutEvent>, u64)>,
    notify: Notify,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, kind: PushKind, r: &str) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.0.len() >= CAPACITY {
                inner.0.pop_front();
                inner.1 += 1;
            }
            inner.0.push_back(OutEvent {
                k: kind.as_str().to_string(),
                r: r.to_string(),
                at: super::store::now_ms(),
            });
        }
        self.notify.notify_one();
    }

    fn drain(&self) -> Batch {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Batch {
            events: inner.0.drain(..).collect(),
            dropped: std::mem::take(&mut inner.1),
        }
    }

    /// Everything queued, waiting up to `wait` for the first event. A stale
    /// wake-up (a `Notify` permit left by a push already drained) waits on
    /// for the rest of the window instead of answering empty.
    pub async fn take(&self, wait: Duration) -> Batch {
        let deadline = tokio::time::Instant::now() + wait.min(MAX_WAIT);
        loop {
            let batch = self.drain();
            if !batch.events.is_empty() || batch.dropped > 0 {
                return batch;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return batch;
            }
            let _ = tokio::time::timeout(left, self.notify.notified()).await;
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    #[serde(rename = "waitMs")]
    pub wait_ms: Option<u64>,
}

/// `GET /internal/push/events` (principal child only; the route's
/// `internal` requirement was checked by `auth_middleware`).
pub async fn events_handler(Query(q): Query<WaitQuery>) -> Response {
    let Some(outbox) = super::outbox() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let wait = Duration::from_millis(q.wait_ms.unwrap_or(0)).min(MAX_WAIT);
    Json(outbox.take(wait).await).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_and_wakes_a_waiter() {
        let o = std::sync::Arc::new(Outbox::new());
        for i in 0..(CAPACITY + 3) {
            o.push(PushKind::RunFinished, &format!("run:{i}"));
        }
        let b = o.take(Duration::ZERO).await;
        assert_eq!(b.events.len(), CAPACITY);
        assert_eq!(b.dropped, 3);
        assert_eq!(b.events[0].r, "run:3");

        let waiter = {
            let o = o.clone();
            tokio::spawn(async move { o.take(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        o.push(PushKind::RunFailed, "run:x");
        let b = waiter.await.unwrap();
        assert_eq!(b.events.len(), 1);
        assert_eq!(b.events[0].k, "run_failed");

        let started = std::time::Instant::now();
        assert!(o.take(Duration::from_millis(30)).await.events.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(25));
    }
}
