//! Multi-window substrate (plans/multi-window).
//!
//! WP-02 (this commit) lands the `G-WINDOW-MODEL` contract: the
//! [`WindowDescriptor`] (what a window is) + the cross-window event envelope
//! ([`WindowEventEnvelope`]). Both are the Rust source-of-truth mirrored by the
//! TS Zod schema in `@ikenga/contract` at `src/window.ts`.
//!
//! The window registry + spawn/close/list lifecycle that consumes this contract
//! lands in WP-03. WP-69 adds last-focus tracking ("Window 2", G-SEATS P-7)
//! and add/remove-surface on a live window, so a detached window holds
//! several surfaces as tabs.

pub mod descriptor;
pub mod events;
pub mod registry;

#[allow(unused_imports)]
pub use descriptor::{WindowDescriptor, WindowKind};
// Some re-exports (WINDOW_CONTRACT_VERSION, WINDOW_TARGETED_CHANNELS) are
// consumed by WP-04's channel migration, not yet here.
#[allow(unused_imports)]
pub use events::{
    topics, WindowEventEnvelope, WindowEventTarget, WINDOW_CONTRACT_VERSION,
    WINDOW_TARGETED_CHANNELS,
};
#[allow(unused_imports)]
pub use registry::{
    emit_focus_changed, emit_to_focused, emit_to_label, focused_listener_window_label,
    pick_window_two, SurfacesChanged, WindowRegistry,
};
