//! The audit log (G-ACCESS §6, DEC-80): append-only, hash-chained from day
//! one, in the access store, every row carrying `principal_id` and
//! `device_id`.
//!
//! WP-74a owns the chain core ([`chain`]: the §6.2 hash, the §6.3 append
//! protocol and the §6.4 fail-closed verify) so W3's device events are
//! chained from the first row. WP-77 (W5) fills the rest:
//!
//! * [`list`] — `access_audit_list`, `access_audit_verify` and the
//!   desktop-local `access_audit_record_local` (§6.5, §6.7);
//! * [`export`] — `access_audit_export` and `ikenga-server audit export`
//!   (§6.8);
//! * [`reseal`] — `access_audit_reseal` and `ikenga-server audit reseal`
//!   (§6.4);
//! * [`verify_boot`] — the reseal-aware full walk every start, verify and
//!   export runs (§6.4), and `ikenga-server audit verify`;
//! * [`absorb`] — `access/0002_absorb_auth_events` (§6.6, §8.3);
//! * [`on_client_frame`] — `dispatch.sent` (§6.5, P-22).

pub mod absorb;
pub mod chain;
pub mod export;
pub mod list;
pub mod reseal;
pub mod verify_boot;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::Value;

use super::ws::{Frame, Route};

/// The §6.1 `category` column (D-05 `AUDIT_KINDS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Permission,
    Dispatch,
    Access,
    Pairing,
    People,
}

impl Category {
    pub const fn as_str(self) -> &'static str {
        match self {
            Category::Permission => "permission",
            Category::Dispatch => "dispatch",
            Category::Access => "access",
            Category::Pairing => "pairing",
            Category::People => "people",
        }
    }
}

/// The §6.1 `via` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditVia {
    Session,
    Device,
    Operator,
    Cli,
    System,
}

impl AuditVia {
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditVia::Session => "session",
            AuditVia::Device => "device",
            AuditVia::Operator => "operator",
            AuditVia::Cli => "cli",
            AuditVia::System => "system",
        }
    }

    pub fn of(via: &super::ctx::Via) -> AuditVia {
        use super::ctx::Via;
        match via {
            Via::Session { .. } => AuditVia::Session,
            Via::Device { .. } => AuditVia::Device,
            Via::Operator => AuditVia::Operator,
            Via::ChildToken | Via::Relayed => AuditVia::System,
        }
    }
}

/// The closed `kind` list (§6.5) → its category, and whether the kind is an
/// **access change** that a degraded chain refuses (§6.4, P-35).
const KINDS: &[(&str, Category, bool)] = &[
    // pairing
    ("pair.started", Category::Pairing, true),
    ("pair.failed", Category::Pairing, false),
    ("pair.denied", Category::Pairing, true),
    ("pair.allowed", Category::Pairing, true),
    ("pair.cancelled", Category::Pairing, true),
    ("device.tier_changed", Category::Pairing, true),
    ("device.revoked", Category::Pairing, true),
    ("device.expired", Category::Pairing, false),
    ("routing.changed", Category::Pairing, true),
    // people
    ("member.added", Category::People, true),
    ("member.role_changed", Category::People, true),
    ("member.removed", Category::People, true),
    ("member.restored", Category::People, true),
    ("member.expired", Category::People, false),
    ("invite.issued", Category::People, true),
    ("invite.revoked", Category::People, true),
    ("invite.accepted", Category::People, true),
    ("policy.changed", Category::People, true),
    ("policy.owner_approval_changed", Category::People, true),
    ("ownership.offered", Category::People, true),
    ("ownership.accepted", Category::People, true),
    // permission
    ("permission.decided", Category::Permission, false),
    ("permission.refused", Category::Permission, false),
    // dispatch
    ("dispatch.sent", Category::Dispatch, false),
    // access
    ("auth.login_ok", Category::Access, false),
    ("auth.login_fail", Category::Access, false),
    ("auth.login_throttled", Category::Access, false),
    ("auth.logout", Category::Access, false),
    ("auth.password_changed", Category::Access, false),
    ("auth.account_created", Category::Access, false),
    ("auth.account_disabled", Category::Access, false),
    ("auth.account_enabled", Category::Access, false),
    ("auth.sessions_revoked", Category::Access, false),
    ("auth.provision_failed", Category::Access, false),
    ("auth.probe_failed", Category::Access, false),
    ("share.artifact_viewed", Category::Access, false),
    ("app.locked", Category::Access, false),
    ("app.unlocked", Category::Access, false),
    ("vault.locked", Category::Access, false),
    ("vault.unlocked", Category::Access, false),
    ("store.created", Category::Access, false),
    ("audit.verified", Category::Access, false),
    ("audit.exported", Category::Access, false),
    ("audit.chain_broken", Category::Access, false),
    ("audit.resealed", Category::Access, false),
];

/// `kind` as the list's `'static` spelling, or `None` outside §6.5.
pub fn static_kind(kind: &str) -> Option<&'static str> {
    KINDS.iter().find(|(k, ..)| *k == kind).map(|(k, ..)| *k)
}

/// `kind`'s category, or `None` for a kind outside the closed list.
pub fn category_of(kind: &str) -> Option<Category> {
    KINDS.iter().find(|(k, ..)| *k == kind).map(|(_, c, _)| *c)
}

/// Whether a degraded chain refuses `kind` (§6.4).
pub fn is_access_change(kind: &str) -> bool {
    KINDS.iter().any(|(k, _, gated)| *k == kind && *gated)
}

/// One row to append (§6.1 minus `seq`, `at_ms`, `prev_hash`, `hash`, which
/// the chain assigns).
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: &'static str,
    pub principal_id: Option<String>,
    pub device_id: Option<String>,
    pub via: AuditVia,
    pub subject_principal_id: Option<String>,
    pub subject_device_id: Option<String>,
    pub project_key: Option<String>,
    pub target: Option<String>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
    /// Never secrets, tokens, codes, passwords or full tool inputs (§6.2).
    pub detail: Value,
    /// Append even when the chain is degraded. Set for revocations the
    /// system makes on the authentication path (a forced logout's device
    /// grants, §3.10 / R-11): P-35 keeps authentication — and killing
    /// credentials — running while access *grants* are paused.
    pub continue_when_degraded: bool,
}

impl Event {
    pub fn new(kind: &'static str, via: AuditVia) -> Self {
        debug_assert!(
            category_of(kind).is_some(),
            "audit kind {kind} is not in §6.5"
        );
        Self {
            kind,
            principal_id: None,
            device_id: None,
            via,
            subject_principal_id: None,
            subject_device_id: None,
            project_key: None,
            target: None,
            remote_addr: None,
            user_agent: None,
            detail: Value::Object(Default::default()),
            continue_when_degraded: false,
        }
    }

    /// Actor columns from a request's [`super::ctx::AccessCtx`].
    pub fn by(kind: &'static str, ctx: &super::ctx::AccessCtx) -> Self {
        let mut e = Self::new(kind, AuditVia::of(&ctx.via));
        e.principal_id = Some(ctx.principal_id.to_string());
        e.device_id = ctx.device_id.clone();
        e.remote_addr = ctx.meta.remote_addr.clone();
        e.user_agent = ctx.meta.user_agent.clone();
        if let super::ctx::Via::Session { session_id } = &ctx.via {
            e.detail = serde_json::json!({ "session_ref": session_ref(session_id) });
        }
        e
    }

    pub fn subject_device(mut self, id: impl Into<String>) -> Self {
        self.subject_device_id = Some(id.into());
        self
    }

    pub fn subject_principal(mut self, id: impl Into<String>) -> Self {
        self.subject_principal_id = Some(id.into());
        self
    }

    pub fn target(mut self, t: impl Into<String>) -> Self {
        self.target = Some(t.into());
        self
    }

    /// Merge `fields` into `detail` (an object).
    pub fn detail(mut self, fields: Value) -> Self {
        if let (Value::Object(base), Value::Object(add)) = (&mut self.detail, fields) {
            base.extend(add);
        }
        self
    }

    pub fn continue_when_degraded(mut self) -> Self {
        self.continue_when_degraded = true;
        self
    }

    pub fn category(&self) -> Category {
        category_of(self.kind).unwrap_or(Category::Access)
    }

    /// Whether a degraded chain refuses this append.
    pub fn refused_when_degraded(&self) -> bool {
        is_access_change(self.kind) && !self.continue_when_degraded
    }
}

/// P-34: the first 8 hex chars of SHA-256(session_id) — never the raw id.
pub fn session_ref(session_id: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(session_id.as_bytes()))[..8].to_string()
}

/// Append `ev` in its own `BEGIN IMMEDIATE` (§6.3: events that change
/// nothing else — authentication, dispatch, desktop-local rows, `audit.*`).
pub async fn record(
    store: &super::AccessStore,
    ev: &Event,
) -> Result<chain::Head, chain::AppendError> {
    use sqlx::Connection;
    let mut conn = store.pool().acquire().await?;
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await?;
    let head = store.chain().append(&mut tx, ev).await?;
    tx.commit().await?;
    store.chain().committed(head);
    Ok(head)
}

/// A process-wide socket id: the P-22 coalescing key. The T0 daemon's
/// sockets (`server::pty_ws::SocketAccess`) and the T1 broker's
/// (`access::t1::BrokerCtx`) take one each.
pub fn next_socket_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// P-22: one `dispatch.sent` row per (socket, 10 min).
pub const DISPATCH_WINDOW_MS: i64 = 10 * 60 * 1000;

fn dispatch_seen() -> &'static Mutex<HashMap<u64, i64>> {
    static SEEN: OnceLock<Mutex<HashMap<u64, i64>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// The coalescing gate: `true` when `socket` has no row in the current
/// window, and opens one.
fn dispatch_window_opens(socket: u64, now_ms: i64) -> bool {
    let mut seen = dispatch_seen().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = seen.get(&socket) {
        if now_ms - at < DISPATCH_WINDOW_MS {
            return false;
        }
    }
    if seen.len() >= 1024 {
        seen.retain(|_, at| now_ms - *at < DISPATCH_WINDOW_MS);
    }
    seen.insert(socket, now_ms);
    true
}

/// Whether a client frame is a `Prompt` (chat) or a `Write` (PTY: binary
/// stdin, a `write` frame, or the raw-text write fallback) — §6.5's
/// `dispatch.sent` trigger. Resize, kill and cancel are not.
pub fn is_prompt_or_write(route: Route, frame: Frame<'_>) -> bool {
    let kind = |t: &str| {
        serde_json::from_str::<Value>(t)
            .ok()
            .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
    };
    match (route, frame) {
        (Route::Chat, Frame::Text(t)) => kind(t).as_deref() == Some("prompt"),
        (Route::Pty, Frame::Binary(_)) => true,
        (Route::Pty, Frame::Text(t)) => {
            !matches!(kind(t).as_deref(), Some("resize") | Some("kill"))
        }
        _ => false,
    }
}

/// `"pty · <id>"` / `"chat · <id>"` from a WS path (the T1 broker's view).
pub fn frame_target(path: &str) -> String {
    let path = path.split('?').next().unwrap_or(path);
    let id = path.rsplit('/').next().unwrap_or_default();
    format!("{} · {id}", Route::of(path).as_str())
}

/// The dispatch-audit hook on a delivered client WS frame (§6.5, P-22),
/// called by the T0 daemon's PTY / chat sockets and by the T1 broker's
/// frame hook (the store's one writer on each tier):
///
/// * remote credentials only — a Session or a DeviceGrant, never the T0
///   operator (the desktop) and never a T1 child's relayed context;
/// * the first `Prompt` / `Write` frame per socket, coalesced to one
///   `dispatch.sent {route, target}` row per (socket, 10 min);
/// * best-effort: appended off the frame path, in its own transaction; a
///   failure is logged and never touches the frame. Allowed while the chain
///   is degraded (P-35).
pub fn on_client_frame(
    store: Option<&super::AccessStore>,
    ctx: &super::ctx::AccessCtx,
    socket: u64,
    target: &str,
    route: Route,
    frame: Frame<'_>,
) {
    use super::ctx::Via;
    let Some(store) = store else { return };
    if !matches!(ctx.via, Via::Session { .. } | Via::Device { .. }) {
        return;
    }
    if !is_prompt_or_write(route, frame) {
        return;
    }
    if !dispatch_window_opens(socket, chain::now_ms()) {
        return;
    }
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut ev = Event::by("dispatch.sent", ctx)
        .target(target)
        .detail(serde_json::json!({ "route": route.as_str(), "target": target }));
    ev.project_key = ctx.share.as_ref().map(|s| s.project_key.clone());
    let store = store.clone();
    rt.spawn(async move {
        if let Err(e) = record(&store, &ev).await {
            tracing::warn!("audit dispatch.sent not recorded: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_are_unique_and_categorised() {
        let mut seen = std::collections::BTreeSet::new();
        for (k, ..) in KINDS {
            assert!(seen.insert(*k), "duplicate kind {k}");
        }
        assert_eq!(category_of("device.revoked"), Some(Category::Pairing));
        assert_eq!(category_of("auth.login_ok"), Some(Category::Access));
        assert_eq!(category_of("nope"), None);
        assert!(is_access_change("device.tier_changed"));
        assert!(!is_access_change("permission.decided"));
        assert!(!is_access_change("auth.sessions_revoked"));
    }

    #[test]
    fn session_ref_is_a_hash_prefix_not_the_id() {
        let r = session_ref("secret-session-id");
        assert_eq!(r.len(), 8);
        assert!(!"secret-session-id".contains(&r));
    }

    #[test]
    fn prompts_and_writes_are_dispatch_resizes_are_not() {
        assert!(is_prompt_or_write(Route::Pty, Frame::Binary(b"ls\n")));
        assert!(is_prompt_or_write(
            Route::Pty,
            Frame::Text(r#"{"type":"write","data":"x"}"#)
        ));
        assert!(is_prompt_or_write(Route::Pty, Frame::Text("raw")));
        assert!(!is_prompt_or_write(
            Route::Pty,
            Frame::Text(r#"{"type":"resize","rows":1,"cols":1}"#)
        ));
        assert!(!is_prompt_or_write(
            Route::Pty,
            Frame::Text(r#"{"type":"kill"}"#)
        ));
        assert!(is_prompt_or_write(
            Route::Chat,
            Frame::Text(r#"{"type":"prompt","prompt":"hi"}"#)
        ));
        assert!(!is_prompt_or_write(
            Route::Chat,
            Frame::Text(r#"{"type":"cancel"}"#)
        ));
        assert!(!is_prompt_or_write(Route::Fs, Frame::Text("{}")));
        assert_eq!(frame_target("/ws/pty/abc?spawn=true"), "pty · abc");
        assert_eq!(frame_target("/ws/chat/t1"), "chat · t1");
    }

    #[test]
    fn the_dispatch_window_coalesces_per_socket() {
        let s = next_socket_id();
        let other = next_socket_id();
        assert!(dispatch_window_opens(s, 1_000));
        assert!(!dispatch_window_opens(s, 1_000 + DISPATCH_WINDOW_MS - 1));
        assert!(dispatch_window_opens(other, 1_001));
        assert!(dispatch_window_opens(s, 1_000 + DISPATCH_WINDOW_MS));
    }

    /// §6.5 / P-22: a remote credential's first Prompt / Write per socket
    /// writes one `dispatch.sent`, coalesced per (socket, 10 min); the T0
    /// operator (the desktop) writes none.
    #[tokio::test]
    async fn dispatch_sent_is_remote_only_and_coalesced() {
        use crate::access::caps::Tier;
        use crate::access::devices::tests::{operator_ctx, pair};
        use crate::access::AccessStore;
        let store = AccessStore::memory_t0().await;
        let (row, _) = pair(&store, Tier::Dispatch).await;
        let phone =
            crate::access::audit::list::tests::device_ctx(&store, &row.device_id, Tier::Dispatch);
        let op = operator_ctx(&store);
        let sock = next_socket_id();
        let write = Frame::Text(r#"{"type":"write","data":"ls\n"}"#);
        let resize = Frame::Text(r#"{"type":"resize","rows":1,"cols":1}"#);
        on_client_frame(Some(&store), &phone, sock, "pty · a", Route::Pty, resize);
        on_client_frame(
            Some(&store),
            &op,
            next_socket_id(),
            "pty · a",
            Route::Pty,
            write,
        );
        on_client_frame(Some(&store), &phone, sock, "pty · a", Route::Pty, write);
        on_client_frame(Some(&store), &phone, sock, "pty · a", Route::Pty, write);
        let count = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM audit_events WHERE kind = 'dispatch.sent'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap()
        };
        for _ in 0..100 {
            if count().await > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(count().await, 1);
        let (dev, via, target, cat): (Option<String>, String, Option<String>, String) =
            sqlx::query_as(
                "SELECT device_id, via, target, category FROM audit_events \
                 WHERE kind = 'dispatch.sent'",
            )
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(dev.as_deref(), Some(row.device_id.as_str()));
        assert_eq!((via.as_str(), cat.as_str()), ("device", "dispatch"));
        assert_eq!(target.as_deref(), Some("pty · a"));
    }

    #[test]
    fn static_kinds_come_from_the_closed_list() {
        assert_eq!(static_kind("auth.login_ok"), Some("auth.login_ok"));
        assert_eq!(static_kind("auth.nope"), None);
    }

    #[test]
    fn system_revocations_continue_when_degraded() {
        let e = Event::new("device.revoked", AuditVia::System);
        assert!(e.refused_when_degraded());
        assert!(!e.continue_when_degraded().refused_when_degraded());
    }
}
