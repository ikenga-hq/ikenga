use agent_client_protocol::schema::{
    AgentCapabilities, InitializeRequest, InitializeResponse, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, ProtocolVersion,
    RequestPermissionResponse,
};
use std::sync::Arc;
use tauri::AppHandle;

/// OpenCode CLI engine adapter normalizing to SessionUpdate ACP envelopes.
pub struct OpencodeEngine;

impl Default for OpencodeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl OpencodeEngine {
    pub fn new() -> Self {
        Self
    }

    pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1;

    pub fn handle_initialize(&self, req: InitializeRequest) -> InitializeResponse {
        let negotiated = std::cmp::min(req.protocol_version, Self::PROTOCOL_VERSION);
        let mut caps = AgentCapabilities::default();
        caps.load_session = true;
        caps.prompt_capabilities.image = false;
        caps.prompt_capabilities.audio = false;
        caps.mcp_capabilities.http = true;
        caps.mcp_capabilities.sse = true;
        InitializeResponse::new(negotiated)
            .agent_capabilities(caps)
            .auth_methods(Vec::new())
    }

    pub async fn handle_new_session(
        &self,
        _app: AppHandle,
        _req: NewSessionRequest,
    ) -> Result<NewSessionResponse, String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        Ok(NewSessionResponse::new(session_id))
    }

    pub async fn handle_prompt(
        &self,
        _app: AppHandle,
        _req: PromptRequest,
    ) -> Result<PromptResponse, String> {
        Ok(PromptResponse::new(agent_client_protocol::schema::StopReason::EndTurn))
    }

    pub async fn handle_cancel(&self, _thread_id: String) -> Result<(), String> {
        Ok(())
    }

    pub async fn resolve_permission(
        &self,
        _request_id: String,
        _response: RequestPermissionResponse,
    ) -> Result<(), String> {
        Ok(())
    }

    pub async fn handle_load_session(
        &self,
        _thread_id: String,
    ) -> Result<LoadSessionResponse, String> {
        Ok(LoadSessionResponse::new())
    }
}

pub type OpencodeEngineState = Arc<OpencodeEngine>;
