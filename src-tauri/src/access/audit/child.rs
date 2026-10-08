//! A T1 principal child's audit channel (daemon asks, gap 2).
//!
//! A principal child never opens the access store (G-ACCESS R-11, §1.7): the
//! T1 broker is the chain's one writer. The daemon's held-hook asks decide and
//! end **inside a child**, so the child cannot append `permission.decided` /
//! `permission.refused` itself. It does what the push outbox does
//! (`server::push::outbox`): it queues a [`Note`] — a closed, typed record,
//! never free text — and the broker, which long-polls
//! `GET /internal/audit/events` ([`spawn`]), validates each note and appends
//! the chain row ([`accept`]). Same channel shape, same trust rule: the broker
//! attributes a note to **that child's** principal, whatever it says.
//!
//! What a child may say, and what the broker believes:
//!
//! * `kind` is one of the two `permission.*` kinds; `decision`, `outcome` and
//!   `reason` are closed vocabularies (anything else drops the note);
//!   `target` and `requested_by` are length-bounded. No tool input, command,
//!   path or token has a field to ride in (§6.2).
//! * The **actor** is the deciding credential as the child saw it (the broker
//!   put it in `X-Ikenga-*`). A child can only ever have been handed its own
//!   principal's credential, or a share member's. The broker takes a reported
//!   member as the actor only if `project_members` has that member active on a
//!   project of **this child's owner**; otherwise the row is attributed to the
//!   child's own principal. A child cannot write a row as a principal it has
//!   no relation to.
//! * `project_key` is kept only if it is one of this owner's.
//!
//! Delivery is best effort and says so: the queue is bounded (a full queue
//! drops the oldest and counts it, logged by the pump) and a note queued when
//! the child dies before the next poll is lost. The row is **not** written
//! when [`Outbox::push`] returns; nothing that answers a caller claims it was.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use super::{AuditVia, Event};
use crate::access::AccessStore;
use crate::executor::PrincipalId;

/// The route the broker long-polls (`internal` class: the per-child token plus
/// the broker's internal-call header; the broker never proxies `/internal/*`).
pub const EVENTS_PATH: &str = "/internal/audit/events";
pub const CAPACITY: usize = 256;
/// The longest a poll may hold.
pub const MAX_WAIT: Duration = Duration::from_secs(25);

const TARGET_MAX: usize = 120;
const ID_MAX: usize = 64;

/// The two audit kinds a child may report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteKind {
    #[serde(rename = "permission.decided")]
    Decided,
    #[serde(rename = "permission.refused")]
    Refused,
}

impl NoteKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            NoteKind::Decided => "permission.decided",
            NoteKind::Refused => "permission.refused",
        }
    }
}

/// The deciding credential, as the child read it from the broker's headers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteBy {
    /// The decider: the principal itself, or a share member.
    pub principal_id: Option<String>,
    /// `session` | `device`; anything else is read as `system`.
    pub via: String,
    pub device_id: Option<String>,
}

/// One audit record a child asks the broker to append.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub kind: Option<NoteKind>,
    /// The ask's title (`Claude wants to use Bash`) — never its input.
    pub target: Option<String>,
    /// `allow_once` | `allow_always_project` | `deny`.
    pub decision: Option<String>,
    /// How the ask ended when nobody decided it: see [`OUTCOMES`].
    pub outcome: Option<String>,
    /// A refusal's reason: see [`REASONS`].
    pub reason: Option<String>,
    /// `shell_notifications.sensitive` (0 / 1 / 2).
    pub sensitive: i64,
    pub requested_by: Option<String>,
    pub project_key: Option<String>,
    pub by: Option<NoteBy>,
}

const DECISIONS: &[&str] = &["allow_once", "allow_always_project", "deny"];
/// How a hook ask ended. `answered` is a human's decision; the rest are the
/// gate's own deny (§5.6: an unanswered ask is denied, never allowed).
pub const OUTCOMES: &[&str] = &[
    "answered",
    "timed_out",
    "terminal_ended",
    "hook_disconnected",
    "held_table_full",
];
/// `permission.refused` reasons: the §6.5 ones the decide core records, plus
/// the daemon hold's own ("the ask ended and nothing was delivered").
pub const REASONS: &[&str] = &[
    "routing_refused",
    "owner_approval_required",
    "forbidden",
    "not_applied",
    "hook_disconnected",
];

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Batch {
    pub notes: Vec<Note>,
    /// Notes dropped since the last poll (the queue was full).
    pub dropped: u64,
}

/// The child's bounded queue.
#[derive(Default)]
pub struct Outbox {
    inner: Mutex<(VecDeque<Note>, u64)>,
    notify: Notify,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, note: Note) {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if inner.0.len() >= CAPACITY {
                inner.0.pop_front();
                inner.1 += 1;
            }
            inner.0.push_back(note);
        }
        self.notify.notify_one();
    }

    fn drain(&self) -> Batch {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Batch {
            notes: inner.0.drain(..).collect(),
            dropped: std::mem::take(&mut inner.1),
        }
    }

    /// Everything queued, waiting up to `wait` for the first note.
    pub async fn take(&self, wait: Duration) -> Batch {
        let deadline = tokio::time::Instant::now() + wait.min(MAX_WAIT);
        loop {
            let batch = self.drain();
            if !batch.notes.is_empty() || batch.dropped > 0 {
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

static OUTBOX: OnceLock<Arc<Outbox>> = OnceLock::new();

/// Install the outbox (a T1 principal child). Once per process.
pub fn install_outbox(outbox: Arc<Outbox>) {
    let _ = OUTBOX.set(outbox);
}

/// The installed outbox, if this process is a T1 principal child.
pub fn outbox() -> Option<Arc<Outbox>> {
    OUTBOX.get().cloned()
}

#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    #[serde(rename = "waitMs")]
    pub wait_ms: Option<u64>,
}

/// `GET /internal/audit/events` (principal child only; the route's `internal`
/// requirement was checked by `auth_middleware`).
pub async fn events_handler(Query(q): Query<WaitQuery>) -> Response {
    let Some(outbox) = outbox() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let wait = Duration::from_millis(q.wait_ms.unwrap_or(0)).min(MAX_WAIT);
    Json(outbox.take(wait).await).into_response()
}

// ─── the broker's side ──────────────────────────────────────────────────────

fn closed(value: &Option<String>, set: &[&'static str]) -> Result<Option<&'static str>, ()> {
    match value.as_deref() {
        None => Ok(None),
        Some(v) => set.iter().find(|s| **s == v).map(|s| Some(*s)).ok_or(()),
    }
}

fn bounded(value: &Option<String>, max: usize) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.chars().take(max).collect())
}

fn id_like(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= ID_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Whether `member` is an active member of a project of `owner`'s.
async fn is_member_of(store: &AccessStore, owner: PrincipalId, member: &str) -> bool {
    let like = format!("{owner}/%");
    sqlx::query_scalar::<_, i64>(
        "SELECT 1 FROM project_members \
         WHERE member_principal_id = ? AND project_key LIKE ? AND removed_at IS NULL LIMIT 1",
    )
    .bind(member)
    .bind(like)
    .fetch_optional(store.pool())
    .await
    .ok()
    .flatten()
    .is_some()
}

/// A child's [`Note`] as the chain row the broker may append, or `None` if it
/// says anything outside the closed vocabularies. `child` is the principal
/// whose child sent it: the row is theirs, whoever the note names.
pub async fn accept(store: &AccessStore, child: PrincipalId, note: &Note) -> Option<Event> {
    let reported = note.by.as_ref();
    let mut actor = None;
    let mut rejected = false;
    if let Some(claimed) = reported.and_then(|b| b.principal_id.as_deref()) {
        if claimed != child.to_string() {
            if is_member_of(store, child, claimed).await {
                actor = Some(claimed.to_string());
            } else {
                rejected = true;
            }
        }
    }
    build_event(child, note, actor, rejected)
}

/// A note with no deciding credential — the gate's own outcome (a timeout, a
/// dead terminal) — as the row the **owner's** chain gets. The one shape both
/// the broker ([`accept`]) and a T0 daemon (which holds the store itself)
/// write, so a timeout reads the same on either tier.
pub fn system_event(owner: PrincipalId, note: &Note) -> Option<Event> {
    build_event(owner, note, None, false)
}

/// Validate `note` against the closed vocabularies and build the row.
/// `actor`: the verified member, or `None` for the owner itself.
fn build_event(
    owner: PrincipalId,
    note: &Note,
    actor: Option<String>,
    actor_rejected: bool,
) -> Option<Event> {
    let kind = note.kind?;
    let decision = closed(&note.decision, DECISIONS).ok()?;
    let outcome = closed(&note.outcome, OUTCOMES).ok()?;
    let reason = closed(&note.reason, REASONS).ok()?;
    if !(0..=2).contains(&note.sensitive) {
        return None;
    }
    // A decided row says what was decided; a refused one says why not.
    match kind {
        NoteKind::Decided if decision.is_none() => return None,
        NoteKind::Refused if reason.is_none() => return None,
        _ => {}
    }

    let owner_id = owner.to_string();
    let reported = note.by.as_ref();
    let via = match reported.map(|b| b.via.as_str()) {
        Some("session") => AuditVia::Session,
        Some("device") => AuditVia::Device,
        _ => AuditVia::System,
    };
    let mut ev = Event::new(kind.as_str(), via);
    ev.principal_id = Some(actor.unwrap_or_else(|| owner_id.clone()));
    // Only the device of a credential we took at its word.
    if !actor_rejected {
        ev.device_id = reported
            .and_then(|b| b.device_id.clone())
            .filter(|d| id_like(d));
    }
    ev.project_key = note
        .project_key
        .as_deref()
        .filter(|k| k.strip_prefix(&format!("{owner_id}/")).is_some_and(id_like))
        .map(str::to_string);
    ev.target = bounded(&note.target, TARGET_MAX);
    let mut detail = serde_json::Map::new();
    match kind {
        NoteKind::Decided => {
            detail.insert("decision".into(), decision.into());
            detail.insert("sensitive".into(), note.sensitive.into());
            detail.insert(
                "requested_by".into(),
                bounded(&note.requested_by, ID_MAX).into(),
            );
        }
        NoteKind::Refused => {
            detail.insert("reason".into(), reason.into());
        }
    }
    if let Some(o) = outcome {
        detail.insert("outcome".into(), o.into());
    }
    if actor_rejected {
        detail.insert("reported_actor_rejected".into(), true.into());
    }
    Some(ev.detail(serde_json::Value::Object(detail)))
}

/// Start the broker's reconcile loop (once, after the access store exists):
/// one long-poll task per running principal child. Linux-only, like the
/// broker it serves (`server::broker` is `#[cfg(target_os = "linux")]`).
#[cfg(target_os = "linux")]
pub fn spawn(children: Arc<crate::server::broker::children::Children>, store: AccessStore) {
    use crate::server::broker::children::ChildEndpoint;
    const RECONCILE_EVERY: Duration = Duration::from_secs(5);
    const WAIT_MS: u64 = 25_000;

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
                tracing::warn!("audit pump: no HTTP client: {e}");
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
                let handle = tokio::spawn(poll_child(
                    client.clone(),
                    store.clone(),
                    id,
                    endpoint.clone(),
                ));
                tasks.insert(id, (endpoint, handle));
            }
        }
    });

    async fn poll_child(
        client: reqwest::Client,
        store: AccessStore,
        principal: PrincipalId,
        endpoint: ChildEndpoint,
    ) {
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
                    tracing::debug!("audit pump: child {principal} answered {}", r.status());
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
                        tracing::warn!(
                            "audit pump: child {principal} dropped {} audit notes",
                            b.dropped
                        );
                    }
                    for note in &b.notes {
                        match accept(&store, principal, note).await {
                            Some(ev) => {
                                crate::server::shared::notifications::routing::append_audit(
                                    &store, &ev,
                                )
                                .await
                            }
                            None => tracing::warn!(
                                "audit pump: child {principal} sent a note outside the closed \
                                 vocabulary; dropped"
                            ),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decided(by: Option<NoteBy>) -> Note {
        Note {
            kind: Some(NoteKind::Decided),
            target: Some("Claude wants to use Bash".into()),
            decision: Some("allow_once".into()),
            sensitive: 1,
            by,
            ..Note::default()
        }
    }

    async fn rows(store: &AccessStore) -> Vec<(String, Option<String>, String, String)> {
        sqlx::query_as("SELECT kind, principal_id, via, detail FROM audit_events ORDER BY seq")
            .fetch_all(store.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_note_becomes_the_childs_own_row() {
        let store = AccessStore::memory_t0().await;
        let me = PrincipalId::new_v7();
        let ev = accept(
            &store,
            me,
            &decided(Some(NoteBy {
                principal_id: Some(me.to_string()),
                via: "device".into(),
                device_id: Some("dev-1".into()),
            })),
        )
        .await
        .unwrap();
        assert_eq!(ev.kind, "permission.decided");
        assert_eq!(ev.principal_id.as_deref(), Some(me.to_string().as_str()));
        assert_eq!(ev.via, AuditVia::Device);
        assert_eq!(ev.device_id.as_deref(), Some("dev-1"));
        assert_eq!(ev.target.as_deref(), Some("Claude wants to use Bash"));
        assert_eq!(ev.detail["decision"], "allow_once");
        assert_eq!(ev.detail["sensitive"], 1);
    }

    /// A child cannot write a row as a principal it has no relation to, and
    /// keeps nothing outside the closed vocabularies.
    #[tokio::test]
    async fn a_child_cannot_name_another_principal_or_a_foreign_project() {
        let store = AccessStore::memory_t0().await;
        let (me, other) = (PrincipalId::new_v7(), PrincipalId::new_v7());
        let mut n = decided(Some(NoteBy {
            principal_id: Some(other.to_string()),
            via: "session".into(),
            device_id: Some("their-device".into()),
        }));
        n.project_key = Some(format!("{other}/their-project"));
        let ev = accept(&store, me, &n).await.unwrap();
        assert_eq!(ev.principal_id.as_deref(), Some(me.to_string().as_str()));
        assert_eq!(ev.device_id, None, "a rejected actor's device is not kept");
        assert_eq!(ev.project_key, None, "another owner's project is not kept");
        assert_eq!(ev.detail["reported_actor_rejected"], true);

        for bad in [
            Note {
                decision: Some("allow_forever".into()),
                ..decided(None)
            },
            Note {
                outcome: Some("made-up".into()),
                ..decided(None)
            },
            Note {
                sensitive: 9,
                ..decided(None)
            },
            Note {
                kind: None,
                ..decided(None)
            },
            // A refusal must say why; a decision must say what.
            Note {
                kind: Some(NoteKind::Refused),
                ..decided(None)
            },
            Note {
                decision: None,
                ..decided(None)
            },
        ] {
            assert!(accept(&store, me, &bad).await.is_none(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn an_active_share_member_of_this_owner_is_the_actor() {
        let store = AccessStore::memory_t0().await;
        let (me, member, stranger) = (
            PrincipalId::new_v7(),
            PrincipalId::new_v7(),
            PrincipalId::new_v7(),
        );
        let key = format!("{me}/site");
        sqlx::query(
            "INSERT INTO project_members (project_key, member_principal_id, role, scope_kind, \
             added_by, added_at) VALUES (?, ?, 'operator', 'project', ?, 1)",
        )
        .bind(&key)
        .bind(member.to_string())
        .bind(me.to_string())
        .execute(store.pool())
        .await
        .unwrap();
        let by = |p: PrincipalId| {
            Some(NoteBy {
                principal_id: Some(p.to_string()),
                via: "session".into(),
                device_id: None,
            })
        };
        let mut n = decided(by(member));
        n.project_key = Some(key.clone());
        let ev = accept(&store, me, &n).await.unwrap();
        assert_eq!(
            ev.principal_id.as_deref(),
            Some(member.to_string().as_str())
        );
        assert_eq!(ev.project_key.as_deref(), Some(key.as_str()));
        // Not a member of this owner's projects: the owner's row.
        let ev = accept(&store, me, &decided(by(stranger))).await.unwrap();
        assert_eq!(ev.principal_id.as_deref(), Some(me.to_string().as_str()));
    }

    #[tokio::test]
    async fn outcomes_without_a_decider_are_system_rows_and_reach_the_chain() {
        let store = AccessStore::memory_t0().await;
        let me = PrincipalId::new_v7();
        let timed_out = Note {
            outcome: Some("timed_out".into()),
            decision: Some("deny".into()),
            by: None,
            ..decided(None)
        };
        let ev = accept(&store, me, &timed_out).await.unwrap();
        assert_eq!(ev.via, AuditVia::System);
        assert_eq!(ev.detail["outcome"], "timed_out");
        crate::server::shared::notifications::routing::append_audit(&store, &ev).await;
        let rows = rows(&store).await;
        let row = rows.last().unwrap();
        assert_eq!(row.0, "permission.decided");
        assert_eq!(row.2, "system");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&row.3).unwrap()["outcome"],
            json!("timed_out")
        );
    }

    #[tokio::test]
    async fn the_outbox_is_bounded_and_wakes_a_waiter() {
        let o = Arc::new(Outbox::new());
        for _ in 0..(CAPACITY + 3) {
            o.push(decided(None));
        }
        let b = o.take(Duration::ZERO).await;
        assert_eq!(b.notes.len(), CAPACITY);
        assert_eq!(b.dropped, 3);

        let waiter = {
            let o = o.clone();
            tokio::spawn(async move { o.take(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        o.push(decided(None));
        assert_eq!(waiter.await.unwrap().notes.len(), 1);
        assert!(o.take(Duration::from_millis(30)).await.notes.is_empty());
    }
}
