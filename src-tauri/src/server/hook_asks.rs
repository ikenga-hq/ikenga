//! Daemon asks: the notification row and the audit trail of a held hook gate
//! (the two gaps `server::term_hooks`'s first cut left).
//!
//! On the desktop a held `PreToolUse` writes a `permission` row
//! (`notifications::producers::permission_from_hook_gate`) that the bell, the
//! Companion cards, the home tile and Web Push all read, and that
//! `permission_decide` answers through the shared decide core. A daemon
//! terminal's gate now does the same, over the daemon's own `ikenga.db`:
//!
//! * **The row** is the desktop's row — same builder, same
//!   `permission:hook:<request id>` key, same `permission.decide` action —
//!   plus the attribution (`project_id`, `sensitive`) and `expiresAtMs` (the
//!   hold, which is also Web Push's TTL). It is written through
//!   `notifications::record`, so `notifications://changed` reaches `/ws/events`
//!   and the push bridge sees a created `permission` row exactly as it does a
//!   desktop mirror's. Under T1 this runs in the principal's own child, on the
//!   principal's own `ikenga.db`: another principal's child never holds it.
//! * **Answering** from the bell or a card is `permission_decide`, which
//!   claims the row (atomically, `decide_with`) and then asks
//!   [`HookResolvers`] — the daemon's `AskKey::Hook` resolver — to answer the
//!   held request through [`TermHooks::decide_held`](super::term_hooks). The
//!   held request is removed before it is answered, so a decision is
//!   single-use whichever door it came through: a second answer finds the row
//!   resolved (`conflict`) or the request gone (`gated: false`). The legacy
//!   `term_hooks_decide` arm goes through the same row when one is open.
//! * **Every end resolves the row.** Whoever removes a request from the held
//!   table owns how it ended ([`Outcome`]) and flips the row; a decision, a
//!   timeout, a dead terminal and a dropped hook connection are each exactly
//!   one remover. The row is written *after* the request is parked, and the
//!   end waits for that write ([`AskRecord::wait_written`]), so an ask that
//!   ends instantly can neither leave a pending row behind nor resolve one
//!   that does not exist yet.
//! * **Audit.** A decision is audited by the decide core as every other
//!   `permission_decide` is (`permission.decided` / `permission.refused`). The
//!   outcomes with no decider — the gate's own deny on timeout, a table that
//!   was full, a terminal that ended, a hook that hung up — are audited here
//!   as the same two kinds with an `outcome`. T0 appends to its access store;
//!   a T1 child holds none, so it queues a note for the broker, which is the
//!   chain's one writer (`access::audit::child`).
//!
//! What this does not do, and says so: a hook's `PermissionRequest` (Claude
//! showing its own prompt in the terminal) is still not recorded as an
//! open-terminal row, and the desktop's no-row path (`/iyke/hooks/decision`)
//! is still unaudited. Both are follow-ups.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::watch;
use tracing::{debug, warn};

use super::shared::notifications::hook_ask::{self, AskFacts};
use super::shared::notifications::routing::{
    self, AskKey, AskResolvers, AuditTo, DecidedBy, Decision, RelayResolvers,
};
use super::shared::notifications::{NewNotification, Notification};
use super::term_hooks::TermHooks;
use super::AppState;
use crate::access::audit::child::{self, Note, NoteKind};
use crate::access::{AccessCtx, AccessError, AccessStore, Code, DaemonAccess, DaemonMode};
use crate::executor::PrincipalId;

/// How long [`HookAsks::end`] waits for the ask's row to be written before it
/// resolves by key anyway (the write itself is bounded by the DB's busy
/// timeout, 5 s).
const WRITE_WAIT: Duration = Duration::from_secs(8);

#[cfg(test)]
thread_local! {
    /// Per-test outbox for a principal child's audit notes (read by
    /// [`HookAsks::for_access`]; a test builds its router on its own thread).
    pub(crate) static TEST_OUTBOX: std::cell::RefCell<Option<Arc<child::Outbox>>> =
        const { std::cell::RefCell::new(None) };
}

/// How a held ask ended. Whoever removes it from the held table owns this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Someone decided it (`permission_decide` or `term_hooks_decide`); the
    /// decision is audited by whichever door took it, so there is nothing
    /// more to say here than "the row is over".
    Answered,
    /// Nobody answered inside the hold: the gate denied.
    TimedOut,
    /// The held table was full: denied without parking (no row exists).
    TableFull,
    /// The terminal's PTY exited with the ask held.
    TerminalEnded,
    /// The hook's HTTP connection went away with the ask held.
    HookDisconnected,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Outcome::Answered => "answered",
            Outcome::TimedOut => "timed_out",
            Outcome::TableFull => "held_table_full",
            Outcome::TerminalEnded => "terminal_ended",
            Outcome::HookDisconnected => "hook_disconnected",
        }
    }
}

/// Where this process's audit rows go.
enum AuditSink {
    /// The T0 daemon's access store.
    Store(AccessStore),
    /// A T1 child's queue, drained by the broker.
    Outbox(Arc<child::Outbox>),
    /// Neither (a router built without a store, or a child without its
    /// outbox): nothing is claimed, and it is logged once per use.
    Off,
}

/// What a row write learned, for the audit of how the ask ends.
#[derive(Default)]
struct RowFacts {
    sensitive: i64,
    project_id: Option<String>,
}

/// One held ask's row machinery. Lives in the held table beside the parked
/// response.
pub(crate) struct AskRecord {
    key: String,
    new: NewNotification,
    facts: AskFacts,
    expires_ms: i64,
    /// Fail closed until the write classifies it.
    row: Mutex<RowFacts>,
    written: watch::Sender<bool>,
}

impl AskRecord {
    /// Pure: the row the ask will get, from the hook's own fields.
    pub(crate) fn new(
        request_id: &str,
        terminal_id: &str,
        tool_name: Option<&str>,
        tool_input: Option<&Value>,
        cwd: Option<&str>,
        hold: Duration,
    ) -> Arc<Self> {
        let new = hook_ask::permission_from_hook_gate(
            tool_name,
            tool_input,
            Some(terminal_id),
            cwd,
            request_id,
        );
        Arc::new(Self {
            key: hook_ask::hook_gate_key(request_id),
            new,
            facts: hook_ask::ask_facts(tool_name, tool_input, cwd, false),
            expires_ms: chrono::Utc::now().timestamp_millis() + hold.as_millis() as i64,
            row: Mutex::new(RowFacts {
                sensitive: 2,
                project_id: None,
            }),
            written: watch::channel(false).0,
        })
    }

    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) fn title(&self) -> &str {
        &self.new.title
    }

    fn facts(&self) -> (i64, Option<String>) {
        let g = self.row.lock().unwrap_or_else(|e| e.into_inner());
        (g.sensitive, g.project_id.clone())
    }

    /// Resolves once the row write finished or was given up on.
    async fn wait_written(&self) {
        let mut rx = self.written.subscribe();
        let _ = tokio::time::timeout(WRITE_WAIT, rx.wait_for(|done| *done)).await;
    }
}

/// The daemon's hook-ask machinery: one per router.
pub(crate) struct HookAsks {
    db: Arc<crate::db::PaDb>,
    audit: AuditSink,
    /// The principal a system outcome is the owner's row of (T0's synthetic
    /// owner; a T1 child's rows are attributed by the broker instead).
    owner: PrincipalId,
}

impl HookAsks {
    /// Over the router's own `ikenga.db` and access state. `None` without a
    /// database: the gate then works exactly as it did before rows.
    pub(crate) fn for_access(
        db: Option<Arc<crate::db::PaDb>>,
        access: &DaemonAccess,
    ) -> Option<Arc<Self>> {
        let db = db?;
        let audit = match (access.mode, access.store()) {
            (DaemonMode::T0, Some(store)) => AuditSink::Store(store.clone()),
            (DaemonMode::PrincipalChild, _) => {
                // A test routes a child's notes to a queue of its own: the
                // process-global outbox is shared by every router in the run.
                #[cfg(test)]
                let outbox = TEST_OUTBOX
                    .with(|t| t.borrow().clone())
                    .or_else(child::outbox);
                #[cfg(not(test))]
                let outbox = child::outbox();
                match outbox {
                    Some(o) => AuditSink::Outbox(o),
                    None => AuditSink::Off,
                }
            }
            (DaemonMode::T0, None) => AuditSink::Off,
        };
        Some(Arc::new(Self {
            db,
            audit,
            owner: access.owner(),
        }))
    }

    /// A test seam: the same machinery with an explicit outbox.
    #[cfg(test)]
    pub(crate) fn with_outbox(
        db: Arc<crate::db::PaDb>,
        owner: PrincipalId,
        outbox: Arc<child::Outbox>,
    ) -> Arc<Self> {
        Arc::new(Self {
            db,
            audit: AuditSink::Outbox(outbox),
            owner,
        })
    }

    fn audit_to(&self) -> AuditTo<'_> {
        match &self.audit {
            AuditSink::Store(s) => AuditTo::Store(s),
            AuditSink::Outbox(o) => AuditTo::Outbox(o),
            AuditSink::Off => AuditTo::Off,
        }
    }

    async fn pool(&self) -> Result<sqlx::SqlitePool, String> {
        self.db.ensure_pool().await
    }

    /// An earlier run's open hook rows: their hold died with that process, so
    /// none can still be answered (the daemon's counterpart of the desktop's
    /// boot sweep). `before_ms` is this process's start; an ask raised since
    /// is live.
    pub(crate) fn sweep_orphans(self: &Arc<Self>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = self.clone();
        let before = chrono::Utc::now().timestamp_millis();
        rt.spawn(async move {
            match me.pool().await {
                Ok(pool) => {
                    let swept = routing::sweep_orphaned_asks(&pool, before).await;
                    if !swept.is_empty() {
                        debug!("hook asks: closed {} rows from an earlier run", swept.len());
                    }
                }
                Err(e) => debug!("hook asks: no pool for the boot sweep: {e}"),
            }
        });
    }

    /// Write the ask's row (after it is parked). Never fails the ask: a row
    /// that cannot be written leaves the gate answerable through
    /// `term_hooks_decide` and says so in the log.
    pub(crate) fn raise(self: &Arc<Self>, rec: &Arc<AskRecord>) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            rec.written.send_replace(true);
            return;
        };
        let (me, rec) = (self.clone(), rec.clone());
        rt.spawn(async move {
            let row = me.write_row(&rec).await;
            if row.is_none() {
                warn!(
                    "hook asks: no row for {}; it stays answerable on term_hooks_decide",
                    rec.key
                );
            }
            // `send_replace`, not `send`: nobody may be subscribed yet, and a
            // plain `send` then keeps the old value.
            rec.written.send_replace(true);
        });
    }

    async fn write_row(&self, rec: &AskRecord) -> Option<Notification> {
        let pool = match self.pool().await {
            Ok(p) => p,
            Err(e) => {
                warn!("hook asks: no db pool: {e}");
                return None;
            }
        };
        let project = match rec.facts.cwd.as_deref() {
            Some(cwd) => routing::project_for_path(&pool, cwd).await,
            None => None,
        };
        let attribution = hook_ask::attribution(&rec.facts, project.as_ref());
        {
            let mut g = rec.row.lock().unwrap_or_else(|e| e.into_inner());
            g.sensitive = attribution.sensitivity.level();
            g.project_id = attribution.project_id.clone();
        }
        let mut new = rec.new.clone();
        // The hold bounds the ask's life; Web Push's TTL and "is this still
        // answerable" both read it.
        if let Some(Value::Object(action)) = new.action.as_mut() {
            action.insert("expiresAtMs".into(), json!(rec.expires_ms));
        }
        match routing::record_ask(&pool, new, &attribution).await {
            Ok(row) => row,
            Err(e) => {
                warn!("hook asks: could not record {}: {e}", rec.key);
                None
            }
        }
    }

    /// The ask is over: flip its row, and audit the outcomes nobody decided.
    /// Detached — a hook's reply never waits on a DB write.
    pub(crate) fn conclude(self: &Arc<Self>, rec: Option<Arc<AskRecord>>, outcome: Outcome) {
        let Some(rec) = rec else { return };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = self.clone();
        rt.spawn(async move { me.end(&rec, outcome).await });
    }

    /// The ask never got parked (the table was full): there is no row to
    /// flip, but the gate denied it, and that is a decision to record.
    pub(crate) fn refused_unparked(
        self: &Arc<Self>,
        tool_name: Option<&str>,
        tool_input: Option<&Value>,
        terminal_id: &str,
        cwd: Option<&str>,
    ) {
        let rec = AskRecord::new(
            "unparked",
            terminal_id,
            tool_name,
            tool_input,
            cwd,
            Duration::ZERO,
        );
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let me = self.clone();
        rt.spawn(async move {
            // Never parked, so never classified by a row write.
            let level = hook_ask::attribution(&rec.facts, None).sensitivity.level();
            rec.row.lock().unwrap_or_else(|e| e.into_inner()).sensitive = level;
            me.audit_outcome(&rec, Outcome::TableFull).await
        });
    }

    async fn end(&self, rec: &AskRecord, outcome: Outcome) {
        rec.wait_written().await;
        match self.pool().await {
            Ok(pool) => {
                if let Err(e) = super::shared::notifications::resolve_by_key(&pool, rec.key()).await
                {
                    warn!("hook asks: could not resolve {}: {e}", rec.key());
                }
            }
            Err(e) => warn!("hook asks: could not resolve {}: {e}", rec.key()),
        }
        self.audit_outcome(rec, outcome).await;
    }

    /// `permission.decided {decision: deny, outcome}` for a deny the gate
    /// delivered on its own; `permission.refused {reason, outcome}` for an ask
    /// that ended with no live hook to answer.
    async fn audit_outcome(&self, rec: &AskRecord, outcome: Outcome) {
        let (kind, decision, reason) = match outcome {
            Outcome::Answered => return,
            Outcome::TimedOut | Outcome::TableFull => {
                (NoteKind::Decided, Some("deny".to_string()), None)
            }
            Outcome::TerminalEnded | Outcome::HookDisconnected => (
                NoteKind::Refused,
                None,
                Some("hook_disconnected".to_string()),
            ),
        };
        let (sensitive, project_id) = rec.facts();
        let note = Note {
            kind: Some(kind),
            target: Some(rec.title().to_string()),
            decision,
            outcome: Some(outcome.as_str().to_string()),
            reason,
            sensitive,
            requested_by: None,
            project_key: project_id.map(|p| format!("{}/{p}", self.owner)),
            by: None,
        };
        match &self.audit {
            AuditSink::Store(store) => match child::system_event(self.owner, &note) {
                Some(ev) => routing::append_audit(store, &ev).await,
                None => warn!("hook asks: an outcome note failed its own vocabulary"),
            },
            AuditSink::Outbox(outbox) => outbox.push(note),
            AuditSink::Off => debug!(
                "hook asks: no audit sink; {} not recorded",
                outcome.as_str()
            ),
        }
    }

    /// The open row for `key`, if any (the legacy arm answers through it).
    pub(crate) async fn open_row(&self, key: &str) -> Option<i64> {
        let pool = self.pool().await.ok()?;
        routing::find_open_by_key(&pool, key).await
    }

    /// The audit of a decision taken on the legacy arm with no row to claim.
    pub(crate) async fn audit_raw_decision(
        &self,
        ctx: &AccessCtx,
        rec: Option<&AskRecord>,
        approved: bool,
    ) {
        let (sensitive, project_id) = rec.map(AskRecord::facts).unwrap_or((2, None));
        let decision = if approved { "allow_once" } else { "deny" };
        let title = rec.map(|r| r.title().to_string());
        match &self.audit {
            AuditSink::Store(store) => {
                let mut ev = crate::access::audit::Event::by("permission.decided", ctx).detail(
                    json!({ "decision": decision, "sensitive": sensitive, "requested_by": null }),
                );
                ev.target = title;
                ev.project_key = project_id.map(|p| format!("{}/{p}", ctx.principal_id));
                routing::append_audit(store, &ev).await;
            }
            AuditSink::Outbox(outbox) => {
                let by = routing::Decider::from_ctx(ctx).by;
                outbox.push(Note {
                    kind: Some(NoteKind::Decided),
                    target: title,
                    decision: Some(decision.to_string()),
                    sensitive,
                    project_key: project_id.map(|p| format!("{}/{p}", ctx.principal_id)),
                    by: Some(child::NoteBy {
                        principal_id: by.principal_id,
                        via: by.via.to_string(),
                        device_id: by.device_id,
                    }),
                    ..Note::default()
                });
            }
            AuditSink::Off => {}
        }
    }
}

// ─── the decide core's hook resolver ─────────────────────────────────────────

/// The daemon's `AskKey::Hook` resolver: answers the held request the row
/// names. `None` for every other key (the relay's resolver handles those).
pub(crate) struct HookResolvers<'a>(pub(crate) &'a Arc<TermHooks>);

impl AskResolvers for HookResolvers<'_> {
    fn resolve<'a>(
        &'a self,
        key: &'a AskKey,
        decision: Decision,
        _by: &'a DecidedBy,
    ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
        let AskKey::Hook { request_id } = key else {
            return None;
        };
        let answered = self.0.decide_held(request_id, decision.allows());
        Some(Box::pin(async move {
            if answered {
                Ok(())
            } else {
                // The hold ended under the decider (timed out, terminal gone):
                // the row stays resolved, the decision is not recorded.
                Err(AccessError::new(
                    Code::Conflict,
                    "the ask timed out before the decision reached it",
                ))
            }
        }))
    }
}

/// Two resolver tables, first match wins.
struct Either<'r>(&'r dyn AskResolvers, Option<&'r dyn AskResolvers>);

impl AskResolvers for Either<'_> {
    fn resolve<'a>(
        &'a self,
        key: &'a AskKey,
        decision: Decision,
        by: &'a DecidedBy,
    ) -> Option<BoxFuture<'a, Result<(), AccessError>>> {
        self.0
            .resolve(key, decision, by)
            .or_else(|| self.1.and_then(|r| r.resolve(key, decision, by)))
    }
}

// ─── the arms ────────────────────────────────────────────────────────────────

/// `permission_decide` on a daemon (T0 or a T1 child): the shared decide core
/// over this process's `ikenga.db`, with the hook resolver beside the T0
/// relay's, audited where this tier audits.
pub(super) async fn permission_decide(
    state: &AppState,
    access: Option<&DaemonAccess>,
    ctx: Option<&AccessCtx>,
    args: &Value,
) -> super::rpc::RpcResponse {
    use super::rpc::RpcResponse;
    let Some(ctx) = ctx else {
        return crate::access::rpc::error_response(&AccessError::new(
            Code::Unauthenticated,
            "no access context",
        ));
    };
    let Some(access) = access else {
        return crate::access::rpc::error_response(&AccessError::store_unavailable());
    };
    let Some(id) = args.get("notificationId").and_then(Value::as_i64) else {
        return crate::access::rpc::error_response(&AccessError::new(
            Code::InvalidRequest,
            "`notificationId` is required",
        ));
    };
    let decision = args.get("decision").and_then(Value::as_str).unwrap_or("");
    match decide_row(state, access, ctx, id, decision).await {
        Ok(v) => RpcResponse::success(v),
        Err(e) => crate::access::rpc::error_response(&e),
    }
}

async fn decide_row(
    state: &AppState,
    access: &DaemonAccess,
    ctx: &AccessCtx,
    id: i64,
    decision: &str,
) -> Result<Value, AccessError> {
    // The T0 relay, if this process installed one, shares the table: a
    // desktop's mirrored ask is answered through it exactly as before. Only
    // when it runs over THIS router's database (always, in production, where
    // `run_server` hands both the one handle).
    let relay = routing::daemon().filter(|rt| {
        state
            .pa_db
            .as_ref()
            .is_some_and(|db| Arc::ptr_eq(&rt.db, db))
    });
    let pool = match relay {
        Some(rt) => rt.pool().await?,
        None => state
            .pa_db
            .as_ref()
            .ok_or_else(|| AccessError::new(Code::NotFound, "no such ask"))?
            .ensure_pool()
            .await
            .map_err(AccessError::internal)?,
    };
    let hook = HookResolvers(&state.term_hooks);
    let relay_resolvers = relay.map(|rt| RelayResolvers(rt.relay.clone()));
    let resolvers = Either(
        &hook,
        relay_resolvers.as_ref().map(|r| r as &dyn AskResolvers),
    );
    let asks = state.term_hooks.asks();
    // Audit where this tier audits: the asks' sink when a hook-ask machine is
    // attached, else the access store the process holds (T0).
    let audit = match (&asks, access.store()) {
        (Some(a), _) => a.audit_to(),
        (None, Some(store)) => AuditTo::Store(store),
        (None, None) => AuditTo::Off,
    };
    routing::decide_on(
        &pool,
        &resolvers,
        relay.map(|rt| &rt.relay),
        audit,
        ctx,
        id,
        decision,
    )
    .await
}

/// `term_hooks_decide {requestId, decision}`: answered through the ask's row
/// when one is open (so the claim, `can_decide`, the audit and the row's own
/// resolution are the decide core's), else directly on the held request.
pub(super) async fn term_hooks_decide(
    state: &AppState,
    access: Option<&DaemonAccess>,
    ctx: Option<&AccessCtx>,
    args: &Value,
) -> super::rpc::RpcResponse {
    use super::rpc::RpcResponse;
    let request_id = args
        .get("requestId")
        .or_else(|| args.get("request_id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let approved = match args.get("decision").and_then(Value::as_str) {
        Some("approved") => true,
        Some("denied") => false,
        _ => {
            return RpcResponse::error(
                "term_hooks_decide: `decision` must be \"approved\" or \"denied\"",
            )
        }
    };
    let Some(request_id) = request_id else {
        return RpcResponse::error("term_hooks_decide: `requestId` is required");
    };
    let hooks = &state.term_hooks;
    if let (Some(asks), Some(access), Some(ctx)) = (hooks.asks(), access, ctx) {
        if let Some(row) = asks.open_row(&hook_ask::hook_gate_key(request_id)).await {
            let decision = if approved { "allow_once" } else { "deny" };
            return match decide_row(state, access, ctx, row, decision).await {
                Ok(_) => RpcResponse::success(json!({ "recorded": true, "gated": true })),
                // The ask is over (answered, timed out): single-use, as ever.
                Err(e) if e.code == Code::Conflict => {
                    RpcResponse::success(json!({ "recorded": true, "gated": false }))
                }
                Err(e) => crate::access::rpc::error_response(&e),
            };
        }
    }
    // No open row (none was recorded, or it is already resolved): the request
    // itself is the single-use token.
    match hooks.decide_held_record(request_id, approved) {
        Some(rec) => {
            if let (Some(asks), Some(ctx)) = (hooks.asks(), ctx) {
                asks.audit_raw_decision(ctx, rec.as_deref(), approved).await;
            }
            RpcResponse::success(json!({ "recorded": true, "gated": true }))
        }
        None => RpcResponse::success(json!({ "recorded": true, "gated": false })),
    }
}
