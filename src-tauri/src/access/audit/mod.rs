//! The access audit log (G-ACCESS §6, DEC-80).
//!
//! * [`chain`] — the hash chain: append, verify, degraded mode (WP-74a; the
//!   degraded-mode completion and §6.3 forward-verify hardening are WP-77's).
//! * [`list`], [`export`], [`reseal`], [`absorb`] — WP-77 stubs.
//! * [`on_client_frame`] — the `dispatch.sent` hook site in the WS handlers
//!   (§6.5, P-22); WP-77 fills the body.

pub mod absorb;
pub mod chain;
pub mod export;
pub mod list;
pub mod reseal;

pub use chain::{append, verify, Chain, Event};

use super::ctx::AccessCtx;

/// Which WebSocket a client frame arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRoute {
    Pty,
    Chat,
}

/// Called for every client `Prompt` / `Write` frame that passed its cap
/// check (§6.5 `dispatch.sent`: remote credentials only, one row per
/// (socket, 10 min)). WP-77 fills this; until then it records nothing.
pub fn on_client_frame(_ctx: &AccessCtx, _route: FrameRoute, _target: &str) {}
