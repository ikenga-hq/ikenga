//! ACP session permission modes — the pure vocabulary (WP-P10).
//!
//! Moved out of `engines::claude_code::mode` (desktop-only, because its
//! runtime-switch envelope needs `claude::session::ControlWire`) so the
//! headless daemon's Chi runs (`server::shared::chi_exec`) map an ACP mode id
//! to claude's `--permission-mode` flag with the same table the desktop uses.
//! `engines::claude_code::mode` re-exports everything here.

use serde::{Deserialize, Serialize};

pub const MODE_PLAN: &str = "plan";
pub const MODE_DEFAULT: &str = "default";
pub const MODE_AUTO: &str = "auto";
pub const MODE_BYPASS: &str = "bypassPermissions";

/// The four canonical ACP session modes we expose. `Default` is the
/// safest starting state — every tool invocation goes through the
/// permission round-trip (Phase 4). Sessions opt in to more permissive
/// modes explicitly via `session/set_mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AcpSessionMode {
    Plan,
    #[default]
    Default,
    Auto,
    BypassPermissions,
}

impl AcpSessionMode {
    /// The canonical ACP id used on the wire (`session/new` response,
    /// `session/set_mode` request). Stable across agents.
    pub fn as_acp_id(&self) -> &'static str {
        match self {
            Self::Plan => MODE_PLAN,
            Self::Default => MODE_DEFAULT,
            Self::Auto => MODE_AUTO,
            Self::BypassPermissions => MODE_BYPASS,
        }
    }

    /// The string we pass to claude's `--permission-mode` CLI flag and to
    /// the runtime `set_permission_mode` control_request. Note `Auto` maps
    /// to `acceptEdits` — that's claude's name for the same concept.
    pub fn as_claude_flag(&self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::Auto => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
        }
    }

    /// Inverse of `as_acp_id`. Returns `None` for any unknown id so callers
    /// can decide whether to surface an error or fall back to default.
    pub fn from_acp_id(id: &str) -> Option<Self> {
        match id {
            MODE_PLAN => Some(Self::Plan),
            MODE_DEFAULT => Some(Self::Default),
            MODE_AUTO => Some(Self::Auto),
            MODE_BYPASS => Some(Self::BypassPermissions),
            _ => None,
        }
    }
}
