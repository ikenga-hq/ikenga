//! `/ws/events` — the daemon's event bus, delivered to a browser client.
//!
//! The desktop delivers `settings://changed`, `notifications://changed`, … on
//! Tauri's event bus; a browser session has none, so this socket carries the
//! daemon's [`EventBus`](super::events::EventBus) instead.
//! `src/lib/transport/events-socket.ts` is the client half; the two are
//! matched by hand, so a field renamed here must be renamed there.
//!
//! # Wire protocol
//!
//! Client → server (JSON text):
//! ```text
//! {"type":"subscribe",  "events":["settings://changed", …]}
//! {"type":"unsubscribe","events":["settings://changed", …]}
//! ```
//! Server → client (JSON text):
//! ```text
//! {"type":"ready","events":[<live names>],"withheld":[<names this credential may not read>]}
//! {"type":"event","event":"<name>","payload":<the desktop's payload>}
//! {"type":"error","message":"…"}
//! ```
//! `ready` is sent once, first. `events` lists every name the daemon has a
//! producer for **and** this socket may receive; a name in neither list has
//! no daemon producer and will never fire. Subscribing to such a name is not
//! an error — the subscription is simply never fed.
//!
//! # Access
//!
//! Same auth, origin check and lifetime as `/ws/fs`: the router's
//! `auth_middleware` resolves the credential and checks the route's
//! requirement (G-ACCESS §1.6: class `owner`, so a share selection is
//! refused at the handshake), the socket registers in the T0 revocation
//! registry (4401 / 4403, §3.10), and it counts as activity while open.
//! Then each topic is delivered only if this socket's caps meet the
//! requirement of the read arm that topic announces ([`Topic::gate`]) —
//! caps are fixed per handshake (§1.4), so the set is computed once.
//!
//! **Principals (T1).** Every daemon process has its own bus, and a T1
//! principal child serves one principal; the broker proxies `/ws/events` to
//! the caller's own child and never another's (a share, which would route
//! into the Owner's child, is refused here as well as by the route class).

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use super::events::{Event, Topic};
use super::pty_ws::{close_message, closed, SocketAccess};
use super::AppState;
use crate::access::{AccessCtx, DaemonAccess};

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventsControlMessage {
    Subscribe {
        events: Vec<String>,
    },
    Unsubscribe {
        events: Vec<String>,
    },
    /// A viewer's round-trip probe (the Health page's connection indicator):
    /// answered at once with `{type:"pong", id}`. Costs one tiny frame each
    /// way on a socket that is open anyway.
    Ping {
        id: u64,
    },
}

/// The topics `ctx` may receive: produced, and its caps meet the gate.
fn permitted(ctx: Option<&AccessCtx>) -> HashSet<Topic> {
    let Some(ctx) = ctx else {
        return HashSet::new();
    };
    Topic::ALL
        .into_iter()
        .filter(|t| t.produced())
        .filter(|t| {
            crate::access::check(ctx, crate::access::rpc_requirements::requirement(t.gate()))
                .is_ok()
        })
        .collect()
}

fn ready_frame(permitted: &HashSet<Topic>) -> Message {
    let (events, withheld): (Vec<Topic>, Vec<Topic>) = Topic::ALL
        .into_iter()
        .filter(|t| t.produced())
        .partition(|t| permitted.contains(t));
    let names = |ts: Vec<Topic>| ts.into_iter().map(Topic::name).collect::<Vec<_>>();
    Message::Text(
        json!({ "type": "ready", "events": names(events), "withheld": names(withheld) })
            .to_string(),
    )
}

fn event_frame(event: &Event) -> Message {
    Message::Text(
        json!({
            "type": "event",
            "event": event.topic.name(),
            "payload": &*event.payload,
        })
        .to_string(),
    )
}

fn error_frame(message: &str) -> Message {
    Message::Text(json!({ "type": "error", "message": message }).to_string())
}

pub async fn events_ws_handler(
    State(state): State<Arc<AppState>>,
    access: Option<Extension<Arc<DaemonAccess>>>,
    ctx: Option<Extension<AccessCtx>>,
    ws: WebSocketUpgrade,
) -> Response {
    let ctx = ctx.map(|Extension(c)| c);
    // The route class already refused a parsed share; this also refuses a
    // T1 child request carrying share headers that did not parse, and a
    // router built without the auth layer (no context at all). Events are
    // about the caller's own state only.
    match &ctx {
        Some(c) if c.share.is_none() && !c.share_headers => {}
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "ok": false,
                    "error": "live events are only available for your own workspace, not a share",
                })),
            )
                .into_response()
        }
    }
    let guard = SocketAccess::new(access.map(|Extension(a)| a), ctx).with_target("events");
    ws.on_upgrade(move |socket| {
        super::activity::track_ws(handle_events_socket(socket, state, guard))
    })
    .into_response()
}

async fn handle_events_socket(socket: WebSocket, state: Arc<AppState>, mut guard: SocketAccess) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let closed_fut = closed(guard.take_closed());
    tokio::pin!(closed_fut);
    let permitted = permitted(guard.ctx.as_ref());
    let mut bus = state.events.subscribe();
    let mut subscribed: HashSet<Topic> = HashSet::new();

    info!("Events WebSocket client connected");
    if ws_tx.send(ready_frame(&permitted)).await.is_err() {
        return;
    }

    loop {
        let out = tokio::select! {
            // G-ACCESS §3.10: revoked (4401) / caps changed (4403).
            close = &mut closed_fut => {
                let _ = ws_tx.send(close_message(&close)).await;
                break;
            }
            next = ws_rx.next() => match next {
                Some(Ok(Message::Text(text))) => {
                    handle_control(&text, &mut subscribed)
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                // Binary/ping/pong carry nothing this socket understands.
                // Axum answers pings itself.
                Some(Ok(_)) => None,
            },
            ev = bus.recv() => match ev {
                Ok(ev) if subscribed.contains(&ev.topic) && permitted.contains(&ev.topic) => {
                    Some(event_frame(&ev))
                }
                Ok(_) => None,
                Err(RecvError::Lagged(skipped)) => {
                    // The desktop forwarder's answer to a lag: tell the
                    // notification views to refetch. The other topics have
                    // no payload-free form to fake, so they are skipped.
                    warn!("[events_ws] client lagged {skipped} events");
                    let topic = Topic::NotificationsChanged;
                    (subscribed.contains(&topic) && permitted.contains(&topic)).then(|| {
                        event_frame(&Event {
                            topic,
                            payload: Arc::new(json!({
                                "reason": "read_all",
                                "notification": null,
                                "muted": false,
                            })),
                        })
                    })
                }
                Err(RecvError::Closed) => break,
            },
        };
        if let Some(frame) = out {
            if ws_tx.send(frame).await.is_err() {
                break;
            }
        }
    }
    info!("Events WebSocket client disconnected");
}

/// Apply one control frame; the reply to send, if any.
fn handle_control(raw: &str, subscribed: &mut HashSet<Topic>) -> Option<Message> {
    match serde_json::from_str::<EventsControlMessage>(raw) {
        Ok(EventsControlMessage::Subscribe { events }) => {
            // Names with no topic have no producer: nothing to feed them.
            subscribed.extend(events.iter().filter_map(|n| Topic::parse(n)));
            None
        }
        Ok(EventsControlMessage::Unsubscribe { events }) => {
            for topic in events.iter().filter_map(|n| Topic::parse(n)) {
                subscribed.remove(&topic);
            }
            None
        }
        Ok(EventsControlMessage::Ping { id }) => Some(Message::Text(
            json!({ "type": "pong", "id": id }).to_string(),
        )),
        Err(e) => {
            debug!("[events_ws] undecodable control frame: {e}");
            Some(error_frame(&format!("bad events control frame: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{Cap, CapSet, RequestMeta, ShareCtx, Tier, Via};

    fn ctx(caps: CapSet) -> AccessCtx {
        AccessCtx {
            principal_id: crate::executor::PrincipalId::new_v7(),
            via: Via::Relayed,
            device_id: None,
            tier: Tier::View,
            share: None,
            share_headers: false,
            caps,
            admin_strength: false,
            meta: RequestMeta::default(),
        }
    }

    #[test]
    fn ping_is_answered_with_a_pong_carrying_its_id() {
        let mut subs = HashSet::new();
        let reply = handle_control(r#"{"type":"ping","id":42}"#, &mut subs).expect("a pong");
        let Message::Text(text) = reply else {
            panic!("pong must be a text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v, json!({ "type": "pong", "id": 42 }));
        assert!(subs.is_empty(), "a ping subscribes to nothing");
        // A malformed ping is the existing error frame, not a silent drop.
        let bad = handle_control(r#"{"type":"ping"}"#, &mut subs).expect("an error frame");
        let Message::Text(text) = bad else { panic!() };
        assert!(text.contains("\"type\":\"error\""));
    }

    #[test]
    fn control_frames_decode_and_ignore_unproduced_names() {
        let mut subs = HashSet::new();
        assert!(handle_control(
            r#"{"type":"subscribe","events":["settings://changed","runtime://bun"]}"#,
            &mut subs
        )
        .is_none());
        assert_eq!(subs, HashSet::from([Topic::SettingsChanged]));
        assert!(handle_control(
            r#"{"type":"unsubscribe","events":["settings://changed"]}"#,
            &mut subs
        )
        .is_none());
        assert!(subs.is_empty());
    }

    #[test]
    fn an_unknown_control_frame_is_answered_not_ignored() {
        let mut subs = HashSet::new();
        let Some(Message::Text(raw)) = handle_control(r#"{"type":"nope"}"#, &mut subs) else {
            panic!("expected an error frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["type"], "error");
    }

    /// Caps gate topics by their read arm: `files` alone reads settings,
    /// projects and actions but not notifications or the approve gate
    /// (`sessions`); no caps receives nothing.
    #[test]
    fn topics_are_gated_by_the_read_arms_requirement() {
        let files = permitted(Some(&ctx(CapSet::of(&[Cap::Files]))));
        assert!(files.contains(&Topic::SettingsChanged));
        assert!(files.contains(&Topic::ProjectsActiveChanged));
        assert!(files.contains(&Topic::ActionsChanged));
        assert!(!files.contains(&Topic::NotificationsChanged));
        assert!(!files.contains(&Topic::PaActionCommitted));

        let all = permitted(Some(&ctx(CapSet::ALL)));
        assert!(Topic::ALL
            .into_iter()
            .filter(|t| t.produced())
            .all(|t| all.contains(&t)));
        assert!(all.contains(&Topic::SeatsChanged));

        assert!(permitted(Some(&ctx(CapSet::EMPTY))).is_empty());
        assert!(permitted(None).is_empty());
    }

    #[test]
    fn owner_topics_are_withheld_from_a_share_context() {
        let mut c = ctx(CapSet::ALL);
        c.share = Some(ShareCtx {
            project_key: "o/p".into(),
            project_id: "p".into(),
            member_principal_id: None,
            member_device_id: None,
            role: None,
            artifact_path: None,
            owner_approval: false,
        });
        let p = permitted(Some(&c));
        // `pa_actions_list` is an owner arm: never on a share.
        assert!(!p.contains(&Topic::PaActionCommitted));
    }

    #[test]
    fn the_ready_frame_splits_live_from_withheld() {
        let p = permitted(Some(&ctx(CapSet::of(&[Cap::Files]))));
        let Message::Text(raw) = ready_frame(&p) else {
            panic!("text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["type"], "ready");
        let events: Vec<&str> = v["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        let withheld: Vec<&str> = v["withheld"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert!(events.contains(&"settings://changed"));
        assert!(withheld.contains(&"notifications://changed"));
        // `seats_list` is gated by `sessions`, which `files` alone lacks.
        assert!(withheld.contains(&"seats://changed"));
    }

    /// The exact field names `events-socket.ts` destructures.
    #[test]
    fn an_event_frame_carries_the_shape_the_client_reads() {
        let Message::Text(raw) = event_frame(&Event {
            topic: Topic::ProjectsActiveChanged,
            payload: Arc::new(json!({ "id": "p1" })),
        }) else {
            panic!("text frame");
        };
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v,
            json!({ "type": "event", "event": "projects:active-changed", "payload": { "id": "p1" } })
        );
    }
}
