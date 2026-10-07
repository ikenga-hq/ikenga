//! The daemon's event bus: the headless counterpart of the desktop's
//! `app.emit(...)` (remote-access gap audit rank 10).
//!
//! On the desktop every live update — `settings://changed`,
//! `projects:active-changed`, `notifications://changed`, … — goes out on
//! Tauri's event bus to the webview. The daemon has no `AppHandle`, so until
//! this module its arms changed the same rows and emitted nothing, and every
//! browser `listen()` stayed silent. Now the daemon code paths that mirror a
//! desktop emit publish the **same event name and payload** here, and
//! `/ws/events` (`server::events_ws`) relays them to the browser, whose
//! `WebRemoteTransport.listen` hands them to the same handlers the desktop's
//! `listen()` would.
//!
//! # Shape
//!
//! One [`EventBus`] per router (`AppState::events`), i.e. per daemon process:
//! the T0 daemon, or one T1 principal child. Under T1 that is what makes the
//! fan-out principal-scoped — principal A's writes publish on A's child's bus,
//! and the broker only ever proxies A's `/ws/events` to A's own child (a share
//! selection is refused at the handshake; see `events_ws`).
//!
//! * [`EventBus::publish`] never blocks and never fails: no subscriber (no
//!   browser open, tests) is the normal case.
//! * [`EventBus::subscribe`] is the fan-out. Each `/ws/events` socket holds
//!   one receiver; a later in-process consumer (the Web Push hub, plans/pwa,
//!   which today bridges `notifications::subscribe()` itself) can hold
//!   another and see exactly what browsers see.
//! * Every [`Topic`] names the read arm whose requirement gates delivery
//!   ([`Topic::gate`]): a socket only receives an event about state its
//!   credential could read through `/api/rpc` (G-ACCESS §1.6).
//!
//! # Producers
//!
//! | topic | published by | payload |
//! |---|---|---|
//! | `settings://changed` | the settings manager's notifier, after its own writes | `{ path }` |
//! | `projects:active-changed` | `project_set_active` | `{ id }` |
//! | `actions://changed` | `actions_write` / `keybindings_write` (a written file), trust grant / revoke | `{ path, file, scope, reason? }` |
//! | `notifications://changed` | every `notifications::record` / read / mute change, via [`EventBus::subscribe`]'s bridge | `NotificationEvent`, `muted` stamped |
//! | `pa-action-paused` / `-committed` / `-retried` / `-rejected` | the `pa_actions_*` arms | the desktop's structs |
//! | `seats://changed` | every `seats_*` write arm and the queue poller (`server::rpc_seats`) | `SeatsChangedEvent` |
//!
//! What the daemon does **not** do that the desktop does: it starts no file
//! watcher, so a hand edit of `settings.json` / `actions.json` on the server
//! is picked up by the browser's next read, not announced. Event names with no
//! daemon producer at all (`hooks://event`, `statusline://snapshot`,
//! `runtime://bun`, `pkg-installed`, …) are not topics here; the browser is
//! told which names are live (the socket's `ready` frame) and notes the rest
//! once.

use std::sync::{Arc, OnceLock, Weak};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use super::rpc_local::DaemonSettings;
use super::shared::notifications::{self, ChangeReason, NotificationEvent};

/// Buffered events per subscriber. A socket that falls this far behind skips
/// ahead (`RecvError::Lagged`); every event here is an invalidation hint, the
/// rows are the truth.
pub const CAPACITY: usize = 256;

/// One event name the daemon can publish.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Topic {
    SettingsChanged,
    ProjectsActiveChanged,
    ActionsChanged,
    NotificationsChanged,
    PaActionPaused,
    PaActionCommitted,
    PaActionRetried,
    PaActionRejected,
    /// Every seat command's (`server::rpc_seats`) `SeatsChangedEvent`, and
    /// the queue poller's.
    SeatsChanged,
}

impl Topic {
    pub const ALL: [Topic; 9] = [
        Topic::SettingsChanged,
        Topic::ProjectsActiveChanged,
        Topic::ActionsChanged,
        Topic::NotificationsChanged,
        Topic::PaActionPaused,
        Topic::PaActionCommitted,
        Topic::PaActionRetried,
        Topic::PaActionRejected,
        Topic::SeatsChanged,
    ];

    /// The wire name — exactly the desktop's `app.emit` name.
    pub const fn name(self) -> &'static str {
        match self {
            Topic::SettingsChanged => "settings://changed",
            Topic::ProjectsActiveChanged => "projects:active-changed",
            Topic::ActionsChanged => "actions://changed",
            Topic::NotificationsChanged => notifications::EVENT_NAME,
            Topic::PaActionPaused => "pa-action-paused",
            Topic::PaActionCommitted => "pa-action-committed",
            Topic::PaActionRetried => "pa-action-retried",
            Topic::PaActionRejected => "pa-action-rejected",
            Topic::SeatsChanged => "seats://changed",
        }
    }

    pub fn parse(name: &str) -> Option<Topic> {
        Topic::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The RPC whose access requirement (G-ACCESS §1.6) a socket must meet to
    /// receive this topic: the read arm for the state the event announces.
    pub const fn gate(self) -> &'static str {
        match self {
            Topic::SettingsChanged => "settings_read_file",
            Topic::ProjectsActiveChanged => "project_get_active",
            Topic::ActionsChanged => "actions_read_files",
            Topic::NotificationsChanged => "notifications_list",
            Topic::PaActionPaused
            | Topic::PaActionCommitted
            | Topic::PaActionRetried
            | Topic::PaActionRejected => "pa_actions_list",
            Topic::SeatsChanged => "seats_list",
        }
    }

    /// Whether a daemon code path publishes it today. The socket's `ready`
    /// frame lists only these, so the browser can tell a live subscription
    /// from one that will never fire. Every topic has a producer now; a
    /// topic added ahead of its producer is the one to exclude here.
    pub const fn produced(self) -> bool {
        true
    }
}

/// One published event.
#[derive(Clone, Debug)]
pub struct Event {
    pub topic: Topic,
    /// Serialized once at publish; every subscriber shares it.
    pub payload: Arc<Value>,
}

/// See the module doc.
pub struct EventBus {
    tx: broadcast::Sender<Event>,
    /// For `notifications://changed`'s `muted` flag. Weak: the settings
    /// manager's notifier holds this bus.
    settings: OnceLock<Weak<DaemonSettings>>,
    /// Set once the notification bridge is running.
    bridge: OnceLock<()>,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscribers", &self.tx.receiver_count())
            .finish()
    }
}

impl EventBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tx: broadcast::channel(CAPACITY).0,
            settings: OnceLock::new(),
            bridge: OnceLock::new(),
        })
    }

    /// The settings store `notifications://changed` reads `mutedKinds` from.
    pub(crate) fn attach_settings(&self, settings: &Arc<DaemonSettings>) {
        let _ = self.settings.set(Arc::downgrade(settings));
    }

    /// Publish `payload` on `topic`. A payload that does not serialize is a
    /// programming error, logged and dropped rather than sent half-formed.
    pub fn publish(&self, topic: Topic, payload: impl Serialize) {
        let payload = match serde_json::to_value(payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("[events] {} payload did not serialize: {e}", topic.name());
                return;
            }
        };
        // No receivers (no browser connected) is not an error.
        let _ = self.tx.send(Event {
            topic,
            payload: Arc::new(payload),
        });
    }

    /// Every event published from now on. The first call also starts the
    /// relay from the process-wide notification channel, so a daemon nobody
    /// listens to runs no task for it. Needs a Tokio runtime (every caller
    /// is an async handler).
    pub fn subscribe(self: &Arc<Self>) -> broadcast::Receiver<Event> {
        let rx = self.tx.subscribe();
        self.ensure_notification_bridge();
        rx
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    /// Relay `notifications::subscribe()` onto [`Topic::NotificationsChanged`],
    /// stamping `muted` from the personal settings as the desktop's
    /// forwarder (`crate::notifications::spawn_event_forwarder`) does, and
    /// answering a lag with the same `read_all` refetch hint.
    fn ensure_notification_bridge(self: &Arc<Self>) {
        if self.bridge.set(()).is_err() {
            return;
        }
        // Subscribed here, synchronously, so nothing published after the
        // first socket subscribed can be missed.
        let mut rx = notifications::subscribe();
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            use broadcast::error::RecvError;
            loop {
                let event = match rx.recv().await {
                    Ok(event) => Some(event),
                    Err(RecvError::Lagged(skipped)) => {
                        warn!("[events] notification relay lagged {skipped} events");
                        None
                    }
                    Err(RecvError::Closed) => break,
                };
                // The router (and its bus) is gone: stop relaying.
                let Some(bus) = weak.upgrade() else { break };
                let event = match event {
                    Some(mut event) => {
                        if let Some(n) = &event.notification {
                            event.muted = bus.muted_kinds().await.contains(&n.kind);
                        }
                        event
                    }
                    None => NotificationEvent {
                        reason: ChangeReason::ReadAll,
                        notification: None,
                        muted: false,
                    },
                };
                bus.publish(Topic::NotificationsChanged, &event);
            }
            debug!("[events] notification relay stopped");
        });
    }

    async fn muted_kinds(&self) -> Vec<notifications::NotificationKind> {
        match self.settings.get().and_then(Weak::upgrade) {
            Some(settings) => settings.muted_kinds().await,
            // No settings store (no home / data dir): nothing can be muted.
            None => Vec::new(),
        }
    }
}

/// The settings manager's notifier: `settings://changed` with the desktop's
/// [`SettingsChangeEvent`](super::shared::settings::SettingsChangeEvent).
/// Holds the bus weakly — the bus holds the settings store (for `muted`).
pub(crate) fn settings_notifier(bus: &Arc<EventBus>) -> super::shared::settings::ChangeNotifier {
    let bus = Arc::downgrade(bus);
    Arc::new(move |path: &std::path::Path| {
        if let Some(bus) = bus.upgrade() {
            bus.publish(
                Topic::SettingsChanged,
                super::shared::settings::SettingsChangeEvent {
                    path: path.to_string_lossy().into_owned(),
                },
            );
        }
    })
}

/// The actions manager's notifier: `actions://changed` with the desktop's
/// [`ActionsChangeEvent`](super::shared::actions::watch::ActionsChangeEvent)
/// (the manager calls it on a trust grant / revoke; `rpc_files` publishes a
/// written file itself, since the daemon runs no watcher).
pub(crate) fn actions_notifier(
    bus: &Arc<EventBus>,
) -> super::shared::actions::watch::ActionsNotifier {
    let bus = Arc::downgrade(bus);
    Arc::new(move |event| {
        if let Some(bus) = bus.upgrade() {
            bus.publish(Topic::ActionsChanged, event);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_round_trip_and_match_the_desktop_emits() {
        for t in Topic::ALL {
            assert_eq!(Topic::parse(t.name()), Some(t));
        }
        assert_eq!(Topic::parse("hooks://event"), None);
        assert_eq!(
            Topic::NotificationsChanged.name(),
            "notifications://changed"
        );
        assert_eq!(
            Topic::ActionsChanged.name(),
            crate::server::shared::actions::watch::CHANGED_EVENT
        );
    }

    /// Every gate is a real, mapped read arm — a typo would fall to the
    /// all-caps `UNMAPPED` requirement and silently withhold the topic.
    #[test]
    fn every_produced_topic_is_gated_by_a_mapped_read_arm() {
        use crate::access::rpc_requirements::requirement;
        use crate::access::Requirement;
        for t in Topic::ALL.into_iter().filter(|t| t.produced()) {
            assert_ne!(
                requirement(t.gate()),
                Requirement::UNMAPPED,
                "{}: gate {} is unmapped",
                t.name(),
                t.gate()
            );
        }
    }

    #[tokio::test]
    async fn publish_fans_out_to_every_subscriber_and_tolerates_none() {
        let bus = EventBus::new();
        bus.publish(
            Topic::ProjectsActiveChanged,
            serde_json::json!({ "id": "x" }),
        );

        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        bus.publish(
            Topic::ProjectsActiveChanged,
            serde_json::json!({ "id": "p1" }),
        );
        for rx in [&mut a, &mut b] {
            let ev = rx.recv().await.unwrap();
            assert_eq!(ev.topic, Topic::ProjectsActiveChanged);
            assert_eq!(*ev.payload, serde_json::json!({ "id": "p1" }));
        }
    }

    /// The bridge relays the process-wide notification channel. Other tests
    /// in this binary publish on that channel too, so look for our row.
    #[tokio::test]
    async fn notification_changes_reach_the_bus_once_someone_subscribes() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        notifications::publish(ChangeReason::MuteChanged, None);
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let ev = rx.recv().await.unwrap();
                if ev.topic == Topic::NotificationsChanged && ev.payload["reason"] == "mute_changed"
                {
                    return ev;
                }
            }
        })
        .await
        .expect("the relay delivered within 5 s");
        assert_eq!(got.payload["muted"], false);
        assert!(got.payload["notification"].is_null());
    }
}
