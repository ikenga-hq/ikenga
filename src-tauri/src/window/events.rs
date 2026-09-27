//! Cross-window event envelope — the Rust source-of-truth mirrored by the TS
//! Zod schema in `@ikenga/contract` at `src/window.ts`. Lockstep with TS.
//!
//! Half of the `G-WINDOW-MODEL` freeze gate (plans/multi-window WP-02). The
//! default for streaming/data channels is `broadcast` (each window filters its
//! own subscription); only the racy channels (`WINDOW_TARGETED_CHANNELS`) carry
//! window targeting via `emit_to`.

use serde::{Deserialize, Serialize};

/// Envelope + descriptor wire version. Mirrors `WINDOW_CONTRACT_VERSION`.
pub const WINDOW_CONTRACT_VERSION: u8 = 1;

/// Where a window event goes. Mirrors `WindowEventTargetSchema`
/// (a `kind`-tagged discriminated union).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WindowEventTarget {
    /// Every window receives it; each filters its own subscription.
    Broadcast,
    /// Only the named window receives it (the race-free path).
    Window { label: String },
}

/// The base shape every cross-window event rides. Mirrors
/// `WindowEventEnvelopeSchema`. `payload` is opaque at the contract layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowEventEnvelope<T = serde_json::Value> {
    pub v: u8,
    /// Channel name, e.g. "window://opened".
    pub topic: String,
    /// Emitting window label, or "core" for the Rust core.
    pub source_label: String,
    pub target: WindowEventTarget,
    pub payload: T,
}

impl<T> WindowEventEnvelope<T> {
    /// Build an envelope at the current contract version.
    pub fn new(topic: impl Into<String>, source_label: impl Into<String>, target: WindowEventTarget, payload: T) -> Self {
        Self {
            v: WINDOW_CONTRACT_VERSION,
            topic: topic.into(),
            source_label: source_label.into(),
            target,
            payload,
        }
    }
}

/// Canonical window-lifecycle channels emitted by the Rust core. Mirrors
/// `WINDOW_TOPICS`.
pub mod topics {
    pub const OPENED: &str = "window://opened";
    pub const CLOSED: &str = "window://closed";
    // Emitted on every window's `Focused(true/false)` transition (main +
    // detached) so the FE cross-window bus (WP-05/WP-06) can track focus.
    pub const FOCUS_CHANGED: &str = "window://focus-changed";
    // WP-69 (G-SEATS §4.4): a live detached window's `surface_set` grew (a
    // Pop out joined Window 2) or shrank (Move back to main window). Emitted
    // to that window and to `main` only (`registry::emit_surfaces_changed`),
    // payload `registry::SurfacesChanged`. HOST-ONLY for now: not yet in
    // `@ikenga/contract`'s `WINDOW_TOPICS`; the FE names it in
    // `src/lib/window/surfaces-topic.ts` until the contract mirrors it.
    pub const SURFACES_CHANGED: &str = "window://surfaces-changed";
}

/// Channels that MUST be window-targeted (`emit_to`) rather than broadcast.
/// Mirrors `WINDOW_TARGETED_CHANNELS`. Consumed by WP-04's channel migration.
#[allow(dead_code)]
pub const WINDOW_TARGETED_CHANNELS: &[&str] =
    &["screenshot://request", "screenshot://shortcut", "projects:active-changed"];

#[cfg(test)]
mod tests {
    use super::*;

    // The SAME canonical fixture as contract/src/window.test.ts.
    const ENVELOPE_JSON: &str = r#"{
        "v": 1,
        "topic": "window://opened",
        "source_label": "core",
        "target": { "kind": "window", "label": "detached-1" },
        "payload": { "label": "detached-1" }
    }"#;

    #[test]
    fn envelope_round_trips_the_canonical_fixture() {
        let e: WindowEventEnvelope = serde_json::from_str(ENVELOPE_JSON).unwrap();
        assert_eq!(e.v, WINDOW_CONTRACT_VERSION);
        assert_eq!(e.topic, topics::OPENED);
        assert_eq!(
            e.target,
            WindowEventTarget::Window { label: "detached-1".to_string() }
        );
        let s = serde_json::to_string(&e).unwrap();
        let e2: WindowEventEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(e, e2);
    }

    #[test]
    fn target_broadcast_serializes_matching_ts() {
        let s = serde_json::to_string(&WindowEventTarget::Broadcast).unwrap();
        assert_eq!(s, r#"{"kind":"broadcast"}"#);
    }

    #[test]
    fn target_window_serializes_matching_ts() {
        let s = serde_json::to_string(&WindowEventTarget::Window {
            label: "detached-1".to_string(),
        })
        .unwrap();
        assert_eq!(s, r#"{"kind":"window","label":"detached-1"}"#);
    }
}
