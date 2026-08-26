//! Engine adapters for the chat surface.
//!
//! Each engine drives one underlying coding-assistant CLI and feeds the
//! shell's chat layer through the same wire contract (Agent Client
//! Protocol-shaped events on `chat://session/{id}` Tauri channels). The
//! contract is defined in the `agent-client-protocol` crate types —
//! re-using its `SessionUpdate` / `RequestPermissionRequest` / etc. structs
//! keeps the shape identical to what a native-ACP peer (Gemini, future
//! Codex-via-Zed-adapter) emits.
//!
//! The `Engine` trait below is the surface every adapter implements. The
//! Tauri commands in `commands/chat.rs` dispatch into here via an
//! `EngineRegistry` keyed by string id (`"claude-code"`, `"gemini"`,
//! `"codex"`).

// Antigravity is the only adapter the headless daemon can drive today
// (`server/chat_ws.rs`), and the only one whose CLI needs no desktop session.
// The rest reach for `AppHandle` to emit on Tauri channels, or for
// `crate::claude` / `crate::commands`, so they are desktop-only.
pub mod antigravity_acp;
#[cfg(feature = "desktop")]
pub mod claude_code;
#[cfg(feature = "desktop")]
pub mod codex_pty;
#[cfg(feature = "desktop")]
pub mod cursor_agent;
#[cfg(feature = "desktop")]
pub mod opencode_acp;
#[cfg(feature = "desktop")]
pub mod pi_acp;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(feature = "desktop")]
use crate::engines::claude_code::server::ClaudeCodeEngineState;
#[cfg(feature = "desktop")]
use crate::engines::codex_pty::CodexPtyEngineState;
#[cfg(feature = "desktop")]
use crate::engines::cursor_agent::CursorAgentEngineState;
pub use antigravity_acp::AntigravityEngineState;
#[cfg(feature = "desktop")]
pub use opencode_acp::OpencodeEngineState;
#[cfg(feature = "desktop")]
pub use pi_acp::PiEngineState;

/// In-memory registry of available engine adapters, keyed by stable id.
///
/// The `Arc<RwLock<...>>` shape leaves room for dynamic registration (e.g.
/// engine pkgs declaring their own adapter at install time) without
/// re-plumbing the call sites; current use is boot-time fill + read-only
/// lookups.
#[derive(Default, Clone)]
pub struct EngineRegistry {
    by_id: Arc<RwLock<HashMap<String, EngineHandle>>>,
}

/// Opaque handle to an engine adapter. The variant carries the concrete
/// state object that Tauri's `State<'_, T>` machinery resolves against.
/// Per-engine submodules expose their own typed accessors (e.g.
/// `claude_code::server::ClaudeCodeEngineState`) for direct use; the
/// registry exists so `commands/chat.rs` can dispatch on `engine_id`
/// without a giant match on concrete types.
#[derive(Clone)]
pub enum EngineHandle {
    #[cfg(feature = "desktop")]
    ClaudeCode(ClaudeCodeEngineState),
    #[cfg(feature = "desktop")]
    CodexPty(CodexPtyEngineState),
    /// Phase 4 scaffold (ADR-013). Runtime stubbed — see
    /// `cursor_agent::server` for the per-method error surface.
    #[cfg(feature = "desktop")]
    CursorAgent(CursorAgentEngineState),
    /// Antigravity CLI adapter (`engines/antigravity_acp/server`). WP-15.
    Antigravity(AntigravityEngineState),
    #[cfg(feature = "desktop")]
    Opencode(OpencodeEngineState),
    #[cfg(feature = "desktop")]
    Pi(PiEngineState),
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn insert(&self, id: impl Into<String>, handle: EngineHandle) {
        self.by_id.write().await.insert(id.into(), handle);
    }

    pub async fn get(&self, id: &str) -> Option<EngineHandle> {
        self.by_id.read().await.get(id).cloned()
    }

    pub async fn ids(&self) -> Vec<String> {
        self.by_id.read().await.keys().cloned().collect()
    }
}

/// Tauri-friendly wrapper around the registry.
pub type EngineRegistryState = Arc<EngineRegistry>;
