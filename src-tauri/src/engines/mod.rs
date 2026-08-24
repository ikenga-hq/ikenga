pub mod antigravity_acp;
pub mod claude_code;
pub mod codex_pty;
pub mod cursor_agent;
pub mod opencode_acp;
pub mod pi_acp;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub use antigravity_acp::AntigravityEngineState;
pub use opencode_acp::OpencodeEngineState;
pub use pi_acp::PiEngineState;
use crate::engines::claude_code::server::ClaudeCodeEngineState;
use crate::engines::codex_pty::CodexPtyEngineState;
use crate::engines::cursor_agent::CursorAgentEngineState;

#[derive(Default, Clone)]
pub struct EngineRegistry {
    by_id: Arc<RwLock<HashMap<String, EngineHandle>>>,
}

#[derive(Clone)]
pub enum EngineHandle {
    ClaudeCode(ClaudeCodeEngineState),
    CodexPty(CodexPtyEngineState),
    CursorAgent(CursorAgentEngineState),
    Antigravity(AntigravityEngineState),
    Opencode(OpencodeEngineState),
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

pub type EngineRegistryState = Arc<EngineRegistry>;
