//! `OpenRouterHttpEngine` — the key-holding, CLI-less engine adapter.
//!
//! Structurally this is `antigravity_acp/server.rs` with the child process
//! replaced by an HTTPS request: same per-thread session map, same
//! `run_prompt(&self, thread_id, text, model, on_update_cb)` signature so a
//! single `EngineHandle` dispatcher can drive every adapter the same way, same
//! sticky-cancel semantics, same `handle_*` method surface.
//!
//! What is genuinely new is credential handling — see the module docs in
//! `mod.rs`. Three rules matter here:
//!
//! 1. The resolved key is moved straight into the `Authorization` header and is
//!    never logged, never put in an error string, and never written to disk.
//! 2. The key only ever goes to a host the engine pkg's `permissions.net`
//!    allowlist names, over TLS. `base_url` is a user/pkg-writable settings
//!    string, so it is validated against the same allowlist + scheme policy the
//!    ADR-017 `pkg_fetch` path uses ([`validate_endpoint`]) before a request is
//!    built. A `base_url` pointing anywhere else fails the turn rather than
//!    mailing the credential to it.
//! 3. Reading the key touches Stronghold, which is synchronous and holds a
//!    `std::sync::Mutex`; per the `SecretsLock` contract that work runs inside
//!    `spawn_blocking`, never on a tokio worker across an `.await`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use agent_client_protocol::schema::{
    AgentCapabilities, ContentBlock, ContentChunk, EmbeddedResourceResource, InitializeRequest,
    InitializeResponse, LoadSessionResponse, McpCapabilities, NewSessionRequest, NewSessionResponse,
    PromptCapabilities, PromptRequest, PromptResponse, ProtocolVersion, RequestPermissionResponse,
    SessionId, SessionNotification, SessionUpdate, StopReason, TextContent, ToolCall, ToolCallId,
    ToolCallStatus, ToolKind, Usage,
};
use futures_util::StreamExt;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex as TokioMutex, Notify};

use super::sse::{OpenRouterNormalizer, OrEvent};
use crate::commands::secrets::{read_secret_scoped, Scope, SecretsLock};
use crate::pkg::http_proxy::{
    net_allowlist_permits_http, parse_request_url, url_matches_net_allowlist, FetchRefusal,
};

/// Pkg id of the OpenRouter engine package. The manifest that owns the
/// `requiredVaultKeys` declaration and the `model` / `base_url` settings
/// schema: `ikenga-pkgs/packages/engine/openrouter/manifest.json`.
pub const OPENROUTER_PKG_ID: &str = "com.ikenga.engine-openrouter";

/// Engine id the `EngineRegistry` is keyed by (matches `engine.agentId`).
pub const OPENROUTER_ENGINE_ID: &str = "openrouter";

/// Vault key name used when the pkg is not installed (so the shell can still be
/// pointed at OpenRouter by setting the secret alone). When the pkg IS
/// installed, its `engine.onboarding.requiredVaultKeys[0]` wins.
pub const DEFAULT_VAULT_KEY: &str = "OPENROUTER_API_KEY";

/// Public API root. Not a secret; overridden by the `base_url` pkg setting
/// (within the bounds of [`resolve_net_allowlist`]).
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// Where the bearer token may be sent when the engine pkg is not installed, or
/// declares no `permissions.net`. A bare prefix is a `startsWith` match.
pub const DEFAULT_NET_ALLOWLIST: &[&str] = &["https://openrouter.ai/api/"];

/// Last-resort model when neither `pkg_settings` nor the manifest's declared
/// default supplies one. `openrouter/auto` lets OpenRouter route rather than
/// pinning a slug that will be stale within months — the exact failure Round 15
/// caught in the original manifest sketch.
pub const DEFAULT_MODEL: &str = "openrouter/auto";

const SETTING_MODEL: &str = "model";
const SETTING_BASE_URL: &str = "base_url";

/// Attribution headers OpenRouter uses for its app-ranking board. Public
/// values, sent on every request.
const REFERER: &str = "https://ikenga.dev";
const TITLE: &str = "Ikenga";

/// A hung TCP connect is never a legitimate turn.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Idle-read ceiling. There is deliberately no *total* timeout — a long
/// reasoning turn can legitimately stream for many minutes — but a socket that
/// goes silent (a provider hang, a half-open connection left by sleep or a
/// dropped Wi-Fi link) must not park the turn forever. OpenRouter emits
/// `: OPENROUTER PROCESSING` keep-alive comments while a provider is thinking,
/// so a gap this long means the stream is dead, not slow.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Keeps NAT/firewall state alive on a long, quiet reasoning stream.
const TCP_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);

/// Hard cap on how much of a non-2xx body is read before summarising it. A
/// misconfigured proxy or captive portal can answer with a multi-megabyte HTML
/// page (or a chunked body with no `Content-Length` at all); the toast only
/// ever shows 300 characters, so there is no reason to materialise more.
const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

/// Conversation-window budget. OpenRouter's completions endpoint is stateless,
/// so the whole history is resent every turn; without a bound a long thread
/// eventually exceeds the model's context window and then fails *permanently*,
/// because every retry resends the same oversized body.
pub const MAX_HISTORY_MESSAGES: usize = 40;
/// Rough character budget for the same window (~4 chars/token ⇒ ~24k tokens),
/// applied alongside the message count.
pub const MAX_HISTORY_CHARS: usize = 96_000;

/// One message in the chat-completions payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
    fn is_user(&self) -> bool {
        self.role == "user"
    }
}

/// Everything a turn needs that isn't the prompt. Resolved fresh per turn so a
/// settings change or a rotated key takes effect without a restart — and so the
/// key is not held in memory between turns.
#[derive(Clone)]
pub struct OpenRouterConfig {
    /// The fully-resolved, allowlist-validated chat-completions endpoint.
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
}

impl std::fmt::Debug for OpenRouterConfig {
    /// Hand-written so a stray `{:?}` in a log line can never print the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouterConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Cooperative cancellation for one in-flight turn.
///
/// A bare `AtomicBool` is not enough: the read loop spends essentially all of
/// its time parked in `stream.next().await`, and a flag is only observed
/// *between* chunks. On a stalled or half-open connection the next chunk never
/// arrives, so a flag-only cancel never lands and the turn hangs until the
/// process dies. The `Notify` is what lets `tokio::select!` interrupt the park.
#[derive(Debug, Default)]
pub struct AbortSignal {
    flag: AtomicBool,
    notify: Notify,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_aborted(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Flip the flag and wake the reader. `notify_one` (not `notify_waiters`)
    /// so a cancel that lands before the reader parks still stores a permit and
    /// is not lost to the race.
    pub fn trigger(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    /// Resolves once [`trigger`](Self::trigger) has been called. Cancel-safe:
    /// dropping the future and re-creating it on the next loop turn is fine,
    /// because the flag is re-checked before every park.
    pub async fn wait(&self) {
        loop {
            if self.is_aborted() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

/// Per-thread state.
pub struct OpenRouterSession {
    pub cwd: String,
    /// Sticky per-session model, set by `handle_set_model`; a `run_prompt`
    /// override beats it for one turn only.
    pub model: Option<String>,
    /// Conversation so far. OpenRouter's completions endpoint is stateless, so
    /// the history IS the resume mechanism — there is no server-side session id
    /// to hand back the way the CLI engines do.
    ///
    /// Invariant: strictly alternating `user` / `assistant`, committed a pair at
    /// a time by a turn that actually produced a reply. Nothing partial and
    /// nothing from a failed turn is ever stored here.
    pub history: Vec<ChatMessage>,
    /// Tool definitions to advertise on every turn, in OpenAI function shape.
    /// Empty = no `tools` key on the request, which is what makes a model reply
    /// in prose instead of emitting `tool_calls`.
    pub tools: Vec<serde_json::Value>,
    /// Set by `handle_cancel`, consumed by the turn it lands on. Sticky so a
    /// cancel racing the request start is still honoured.
    pub cancelled: bool,
    /// Shared with the in-flight streaming loop so a cancel interrupts the read
    /// rather than waiting for the next chunk (or forever).
    pub abort: Option<Arc<AbortSignal>>,
}

impl OpenRouterSession {
    pub fn new(cwd: String) -> Self {
        Self {
            cwd,
            model: None,
            history: Vec::new(),
            tools: Vec::new(),
            cancelled: false,
            abort: None,
        }
    }
}

/// Build the chat-completions endpoint from a configured base URL.
///
/// Tolerates a trailing slash and a base that already names the endpoint, since
/// both are things a user will type into a settings field. This only *shapes*
/// the URL — [`validate_endpoint`] is what decides whether it may be called.
pub fn completions_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let base = if trimmed.is_empty() {
        DEFAULT_BASE_URL
    } else {
        trimmed
    };
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

/// Gate the endpoint the API key would be sent to.
///
/// `base_url` arrives from a `pkg_settings` row, which any caller of
/// `pkg_settings_set` can write. Without this check a single settings write
/// redirects the next turn — bearer token included — at an arbitrary host over
/// plaintext, and the attacker's response body is then rendered into the turn
/// error. The policy is the same one `pkg_fetch` enforces for mediated fetches:
/// absolute http(s) URL, TLS unless the allowlist itself names an `http://`
/// origin (the author's explicit loopback/dev opt-in), and the URL must match
/// the pkg's declared `permissions.net`.
pub fn validate_endpoint(url: &str, net: &[String]) -> Result<String, String> {
    let parsed = parse_request_url(url, net_allowlist_permits_http(net)).map_err(|r| match r {
        FetchRefusal::InsecureScheme => format!(
            "openrouter base_url must use https (got `{url}`). The API key is never sent over \
             plaintext."
        ),
        _ => format!("openrouter base_url is not a valid absolute http(s) URL: `{url}`"),
    })?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("openrouter base_url must not embed credentials".to_string());
    }
    if !url_matches_net_allowlist(&parsed, net) {
        let host = parsed.host_str().unwrap_or("<none>");
        return Err(format!(
            "openrouter base_url host `{host}` is not in the engine pkg's permissions.net \
             allowlist ({net:?}); refusing to send the API key there"
        ));
    }
    Ok(parsed.to_string())
}

/// The streaming request body. Pure, so the wire shape is assertable without a
/// network. `usage.include` is what makes OpenRouter emit the usage chunk mid-
/// stream instead of only on the non-streaming path.
///
/// `tools` is load-bearing, not decoration: a chat-completions request with no
/// `tools` array can never come back with `delta.tool_calls`, so omitting it
/// makes the whole tool-call path unreachable no matter what the pkg manifest
/// advertises.
pub fn build_request_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "usage": { "include": true },
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::Value::Array(tools.to_vec());
        body["tool_choice"] = serde_json::Value::String("auto".into());
    }
    body
}

/// Normalise tool declarations into the OpenAI function shape OpenRouter wants.
///
/// Accepts either the wire shape already (`{type:"function",function:{name,…}}`)
/// or the flatter MCP-ish shape (`{name, description, inputSchema}`), so an MCP
/// bridge can hand its `tools/list` output over unchanged.
pub fn normalize_tool_defs(tools: Vec<serde_json::Value>) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::with_capacity(tools.len());
    for (i, tool) in tools.into_iter().enumerate() {
        if tool.get("function").and_then(|f| f.get("name")).is_some() {
            out.push(tool);
            continue;
        }
        let name = tool
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("tool definition #{i} has no name"))?;
        let parameters = tool
            .get("parameters")
            .or_else(|| tool.get("inputSchema"))
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
        let mut function = serde_json::json!({ "name": name, "parameters": parameters });
        if let Some(desc) = tool.get("description").and_then(|v| v.as_str()) {
            function["description"] = serde_json::Value::String(desc.to_string());
        }
        out.push(serde_json::json!({ "type": "function", "function": function }));
    }
    Ok(out)
}

/// Keep the resent conversation inside a bounded window.
///
/// Newest-first, subject to both budgets, then trimmed forward to start on a
/// `user` message (a window that opens with an assistant turn is rejected by
/// several OpenRouter upstreams). The newest message is always kept even when it
/// alone blows the character budget — dropping the prompt the user just typed
/// would be worse than the upstream 400 it may cause.
pub fn trim_history(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut kept: Vec<ChatMessage> = Vec::new();
    let mut chars = 0usize;
    for msg in messages.into_iter().rev() {
        if !kept.is_empty() {
            if kept.len() >= MAX_HISTORY_MESSAGES {
                break;
            }
            if chars + msg.content.len() > MAX_HISTORY_CHARS {
                break;
            }
        }
        chars += msg.content.len();
        kept.push(msg);
    }
    kept.reverse();
    // Drop any leading assistant turns the cut exposed.
    let start = kept.iter().position(ChatMessage::is_user).unwrap_or(0);
    if start > 0 {
        kept.drain(..start);
    }
    kept
}

/// OpenRouter `finish_reason` → ACP `StopReason`.
///
/// `tool_calls` is an ordinary end of turn (the client runs the tools and
/// prompts again); `length` is distinct from a clean stop and must not be
/// reported as one; `cancelled` is a provider-side abort and maps to the ACP
/// cancel reason, matching `stream.ts:124`.
pub fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "length" => StopReason::MaxTokens,
        "content_filter" => StopReason::Refusal,
        "error" => StopReason::Refusal,
        "cancelled" | "canceled" => StopReason::Cancelled,
        _ => StopReason::EndTurn,
    }
}

/// Best-effort category for a model-named function, so a chat UI can pick an
/// icon instead of rendering every OpenRouter tool call as an untyped row.
/// Mirrors the intent of `claude_code::mapping::tool_kind_for`, but keyed on
/// substrings because the names here come from arbitrary MCP servers rather
/// than a fixed CLI tool set.
pub fn tool_kind_for(name: &str) -> ToolKind {
    let n = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|k| n.contains(k));
    if has(&["fetch", "http", "curl", "browse", "crawl", "download", "scrape"]) {
        ToolKind::Fetch
    } else if has(&["search", "grep", "glob", "find", "query", "lookup", "list"]) {
        ToolKind::Search
    } else if has(&["exec", "bash", "shell", "terminal", "command", "run_"]) {
        ToolKind::Execute
    } else if has(&["delete", "remove", "trash"]) {
        ToolKind::Delete
    } else if has(&["move", "rename"]) {
        ToolKind::Move
    } else if has(&["write", "edit", "patch", "create", "update", "append"]) {
        ToolKind::Edit
    } else if has(&["read", "open", "cat_", "get_file"]) {
        ToolKind::Read
    } else if has(&["think", "plan", "reason"]) {
        ToolKind::Think
    } else {
        ToolKind::Other
    }
}

/// A row header a human can read. MCP names arrive namespaced
/// (`mcp__server__tool`); only the leaf carries meaning.
pub fn tool_title_for(name: &str) -> String {
    let leaf = name.rsplit("__").next().unwrap_or(name);
    let spaced = leaf.replace(['_', '-'], " ");
    let trimmed = spaced.trim();
    if trimmed.is_empty() {
        return name.to_string();
    }
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => name.to_string(),
    }
}

fn text_chunk(text: String) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
}

/// Map one normalised stream event onto an ACP `SessionUpdate`.
///
/// `Usage`, `Finish`, `Error` and `Done` carry turn-level meaning rather than
/// content, so they return `None` and are handled by the caller.
pub fn update_for_event(event: &OrEvent) -> Option<SessionUpdate> {
    match event {
        OrEvent::Text(text) => Some(SessionUpdate::AgentMessageChunk(text_chunk(text.clone()))),
        OrEvent::Thinking(text) => Some(SessionUpdate::AgentThoughtChunk(text_chunk(text.clone()))),
        OrEvent::ToolUse {
            id,
            name,
            arguments,
        } => {
            // Arguments stream as a JSON *string*; parse when it is complete and
            // fall back to the raw text when a truncated stream left it partial.
            let raw_input = serde_json::from_str::<serde_json::Value>(arguments)
                .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
            Some(SessionUpdate::ToolCall(
                ToolCall::new(ToolCallId::new(id.clone()), tool_title_for(name))
                    .kind(tool_kind_for(name))
                    .status(ToolCallStatus::Pending)
                    .raw_input(raw_input),
            ))
        }
        OrEvent::Usage { .. } | OrEvent::Finish(_) | OrEvent::Error(_) | OrEvent::Done => None,
    }
}

/// Everything one turn accumulates off the stream.
#[derive(Debug, Default)]
pub struct TurnState {
    /// The visible reply — and only the visible reply. Thinking is streamed to
    /// the client but never becomes part of the conversation history.
    pub assistant_text: String,
    /// `None` until the provider actually reported one.
    pub stop_reason: Option<StopReason>,
    /// Set by `finish_reason` or the `[DONE]` sentinel. A stream that ends
    /// without either was truncated, whatever the socket did.
    pub saw_terminal: bool,
    pub error: Option<String>,
    /// `(input_tokens, output_tokens)` from the `usage.include` chunk.
    pub usage: Option<(u64, u64)>,
}

impl TurnState {
    /// The reason to report when the turn completed normally.
    pub fn stop_reason(&self) -> StopReason {
        self.stop_reason.unwrap_or(StopReason::EndTurn)
    }

    fn acp_usage(&self) -> Option<Usage> {
        self.usage
            .map(|(input, output)| Usage::new(input.saturating_add(output), input, output))
    }
}

/// OpenRouter HTTP engine adapter normalizing to SessionUpdate ACP envelopes.
pub struct OpenRouterHttpEngine {
    sessions: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<OpenRouterSession>>>>>,
    /// `Err` when the TLS backend could not be initialised. Held as a `Result`
    /// rather than unwrapped: `reqwest::Client::default()` delegates to
    /// `Client::new()`, which *panics* on the same failure, so an
    /// `unwrap_or_default()` fallback would take the whole shell down at launch
    /// instead of failing one turn.
    client: Result<reqwest::Client, String>,
    /// Set once during Tauri `setup`. Config + key resolution need it, but the
    /// engine is constructed before the app exists, and `run_prompt` must keep
    /// the same `AppHandle`-free signature as every sibling adapter.
    app: OnceLock<AppHandle>,
}

impl Default for OpenRouterHttpEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenRouterHttpEngine {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(TokioMutex::new(HashMap::new())),
            client: build_client(),
            app: OnceLock::new(),
        }
    }

    pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V1;

    /// Hand the engine the `AppHandle` it needs for vault + settings reads.
    /// Idempotent; the first handle wins.
    pub fn attach_app(&self, app: AppHandle) {
        let _ = self.app.set(app);
    }

    fn app(&self) -> Result<&AppHandle, String> {
        self.app
            .get()
            .ok_or_else(|| "openrouter engine is not attached to the app yet".to_string())
    }

    fn client(&self) -> Result<&reqwest::Client, String> {
        self.client.as_ref().map_err(|e| e.clone())
    }

    pub fn handle_initialize(&self, req: InitializeRequest) -> InitializeResponse {
        let negotiated = std::cmp::min(req.protocol_version, Self::PROTOCOL_VERSION);
        // Text and embedded text resources only. Images/audio are refused by
        // `extract_prompt_text` rather than dropped on the floor, so these flags
        // describe what actually happens.
        let prompt_caps = PromptCapabilities::default()
            .image(false)
            .embedded_context(true)
            .audio(false);
        // The adapter connects to no MCP servers of its own: tools reach it via
        // `handle_set_tools`, which is a different thing from accepting an
        // `McpServer` list in `session/new`.
        let mcp_caps = McpCapabilities::default();
        let mut caps = AgentCapabilities::default();
        // History lives in this process. `handle_load_session` replays it for a
        // thread this process still knows and errors for one it does not, so the
        // client never silently resumes into an empty transcript.
        caps.load_session = true;
        caps.prompt_capabilities = prompt_caps;
        caps.mcp_capabilities = mcp_caps;
        InitializeResponse::new(negotiated)
            .agent_capabilities(caps)
            .auth_methods(Vec::new())
    }

    /// Get (or create) the session for `thread_id`. An empty `cwd` means
    /// "caller doesn't know" and never downgrades a cwd already recorded.
    pub async fn register_session(
        &self,
        thread_id: String,
        cwd: String,
    ) -> Arc<TokioMutex<OpenRouterSession>> {
        let mut guard = self.sessions.lock().await;
        guard
            .entry(thread_id)
            .or_insert_with(|| Arc::new(TokioMutex::new(OpenRouterSession::new(cwd))))
            .clone()
    }

    async fn existing_session(&self, thread_id: &str) -> Option<Arc<TokioMutex<OpenRouterSession>>> {
        self.sessions.lock().await.get(thread_id).cloned()
    }

    pub async fn handle_new_session(
        &self,
        app: AppHandle,
        req: NewSessionRequest,
    ) -> Result<NewSessionResponse, String> {
        self.attach_app(app);
        let thread_id = req
            .meta
            .as_ref()
            .and_then(|meta| meta.get("threadId"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let cwd = req.cwd.to_string_lossy().into_owned();
        self.register_session(thread_id.clone(), cwd).await;
        Ok(NewSessionResponse::new(SessionId::new(thread_id)))
    }

    /// Desktop entry point: emit each delta on the Tauri channel the chat layer
    /// listens to. Same callback indirection as `antigravity_acp` — the
    /// `AppHandle` is captured by the closure, not threaded into `run_prompt`.
    pub async fn handle_prompt(
        &self,
        app: AppHandle,
        req: PromptRequest,
    ) -> Result<PromptResponse, String> {
        self.attach_app(app.clone());
        let thread_id = req.session_id.0.to_string();
        let prompt_text = extract_prompt_text(&req)?;
        let channel = format!("chat://session/{thread_id}/openrouter");
        let session_id = SessionId::new(thread_id.clone());
        let emit_app = app.clone();

        let emit = move |update: SessionUpdate| {
            let notif = SessionNotification::new(session_id.clone(), update);
            let _ = emit_app.emit(&channel, &notif);
        };
        let cb: &(dyn Fn(SessionUpdate) + Send + Sync) = &emit;

        self.run_prompt(&thread_id, &prompt_text, None, Some(cb))
            .await
    }

    /// Run one turn against OpenRouter, streaming deltas as they arrive.
    ///
    /// `model` overrides the session's sticky model for this turn only.
    ///
    /// History discipline: nothing is written to `session.history` until the
    /// turn has actually produced a reply. A failed, truncated or reply-less
    /// turn leaves the transcript exactly as it found it — no phantom user
    /// message, no half-written assistant message, and never two `user`
    /// messages in a row.
    pub async fn run_prompt(
        &self,
        thread_id: &str,
        text: &str,
        model: Option<&str>,
        on_update_cb: Option<&(dyn Fn(SessionUpdate) + Send + Sync)>,
    ) -> Result<PromptResponse, String> {
        let app = self.app()?.clone();
        let session_arc = self
            .register_session(thread_id.to_string(), String::new())
            .await;

        let abort = Arc::new(AbortSignal::new());
        let (prior, session_model, tools) = {
            let mut s = session_arc.lock().await;
            // Clear a cancel left over from a previous turn so it can't
            // retroactively cancel this one.
            s.cancelled = false;
            s.abort = Some(abort.clone());
            (s.history.clone(), s.model.clone(), s.tools.clone())
        };

        let user_message = ChatMessage::user(text);
        let mut messages = prior;
        messages.push(user_message.clone());
        let messages = trim_history(messages);

        // Resolved per turn, not cached: a rotated key or a changed model takes
        // effect on the next prompt with no restart, and the credential is not
        // held in memory between turns.
        let cfg = match resolve_config(&app, model.map(|m| m.to_string()).or(session_model)).await {
            Ok(cfg) => cfg,
            Err(e) => return self.fail_turn(&session_arc, e).await,
        };
        let client = match self.client() {
            Ok(c) => c.clone(),
            Err(e) => return self.fail_turn(&session_arc, e).await,
        };

        let body = build_request_body(&cfg.model, &messages, &tools);
        // Serialised by hand rather than with `RequestBuilder::json` — reqwest
        // is pulled in with `default-features = false` (rustls + stream only),
        // so the `json` feature is not enabled and adding it just for this
        // would change the dependency for the whole crate.
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(e) => {
                return self
                    .fail_turn(&session_arc, format!("openrouter request encode failed: {e}"))
                    .await
            }
        };

        let response = client
            .post(&cfg.endpoint)
            .bearer_auth(&cfg.api_key)
            .header("HTTP-Referer", REFERER)
            .header("X-Title", TITLE)
            .header("Accept", "text/event-stream")
            .header("Content-Type", "application/json")
            .body(body_bytes)
            .send()
            .await;

        let response = match response {
            Ok(r) => r,
            Err(e) => {
                // `reqwest::Error`'s Display never contains the header value, so
                // this cannot leak the key; the URL it may contain is public.
                return self
                    .fail_turn(&session_arc, format!("openrouter request failed: {e}"))
                    .await;
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let detail = read_capped_body(response, MAX_ERROR_BODY_BYTES).await;
            return self
                .fail_turn(
                    &session_arc,
                    format!(
                        "openrouter HTTP {}: {}",
                        status.as_u16(),
                        summarise_error_body(&detail)
                    ),
                )
                .await;
        }

        let mut normalizer = OpenRouterNormalizer::new();
        let mut turn = TurnState::default();
        let mut read_error: Option<String> = None;
        let mut stream = response.bytes_stream();

        // Incremental: each body chunk is normalised and emitted as it lands.
        // Nothing here buffers the response. The `select!` is what makes cancel
        // work on a stalled stream — polling the flag between chunks only helps
        // when chunks keep arriving.
        loop {
            let next = tokio::select! {
                biased;
                () = abort.wait() => break,
                next = stream.next() => next,
            };
            let Some(next) = next else { break };
            let chunk = match next {
                Ok(b) => b,
                Err(e) => {
                    read_error = Some(format!("openrouter stream read failed: {e}"));
                    break;
                }
            };
            for event in normalizer.push_bytes(&chunk) {
                handle_event(event, on_update_cb, &mut turn);
            }
        }

        if !abort.is_aborted() {
            for event in normalizer.finish() {
                handle_event(event, on_update_cb, &mut turn);
            }
        }
        // Drop the body before the session lock so a cancelled request's socket
        // closes immediately rather than at end of scope.
        drop(stream);

        let cancelled = {
            let mut s = session_arc.lock().await;
            s.abort = None;
            std::mem::take(&mut s.cancelled)
        };

        let truncated = !cancelled
            && read_error.is_none()
            && turn.error.is_none()
            && !turn.saw_terminal;

        // Commit only a complete exchange: a user message paired with a reply
        // the model actually finished (or that the user deliberately stopped).
        let commit = !turn.assistant_text.is_empty()
            && read_error.is_none()
            && turn.error.is_none()
            && (cancelled || turn.saw_terminal);
        if commit {
            let mut s = session_arc.lock().await;
            s.history.push(user_message);
            s.history.push(ChatMessage::assistant(turn.assistant_text.clone()));
            let trimmed = trim_history(std::mem::take(&mut s.history));
            s.history = trimmed;
        }

        // Cancellation wins over everything: ACP requires `Cancelled` after a
        // `session/cancel`, even when the abort made the read fail.
        if cancelled {
            return Ok(PromptResponse::new(StopReason::Cancelled).usage(turn.acp_usage()));
        }
        if let Some(e) = read_error {
            return Err(e);
        }
        if let Some(e) = turn.error {
            return Err(e);
        }
        if truncated {
            // A clean socket close with no `finish_reason` and no `[DONE]` is a
            // truncated turn, not a finished one. Reporting `EndTurn` here would
            // present half an answer as a complete reply and commit it to the
            // history every later turn is conditioned on.
            return Err(
                "openrouter stream ended before the model reported a finish reason (turn \
                 truncated); nothing was added to the conversation"
                    .to_string(),
            );
        }
        if turn.assistant_text.is_empty() {
            log::warn!(
                "[openrouter] turn produced no visible text (stop: {:?}); not recording it in \
                 history",
                turn.stop_reason()
            );
        }
        Ok(PromptResponse::new(turn.stop_reason()).usage(turn.acp_usage()))
    }

    /// Release the turn's abort slot and hand the error back. Nothing to unwind:
    /// `run_prompt` does not touch `history` until the turn succeeds.
    ///
    /// A cancel that landed while the turn was still setting up wins over the
    /// error, because ACP requires `Cancelled` after a `session/cancel` "even if
    /// the cancellation causes exceptions in underlying operations".
    async fn fail_turn(
        &self,
        session_arc: &Arc<TokioMutex<OpenRouterSession>>,
        error: String,
    ) -> Result<PromptResponse, String> {
        let cancelled = {
            let mut s = session_arc.lock().await;
            s.abort = None;
            std::mem::take(&mut s.cancelled)
        };
        if cancelled {
            return Ok(PromptResponse::new(StopReason::Cancelled));
        }
        Err(error)
    }

    pub async fn handle_cancel(&self, thread_id: String) -> Result<(), String> {
        if let Some(session) = self.existing_session(&thread_id).await {
            let mut s = session.lock().await;
            s.cancelled = true;
            if let Some(abort) = s.abort.take() {
                abort.trigger();
            }
        }
        Ok(())
    }

    /// No permission round-trips: the HTTP engine executes no tools itself, it
    /// only reports the calls the model asked for.
    pub async fn resolve_permission(
        &self,
        _request_id: String,
        _response: RequestPermissionResponse,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Replay an in-process transcript.
    ///
    /// OpenRouter's endpoint is stateless and `OpenRouterSession::history` is
    /// process-local, so — unlike the CLI adapters, which can hand a resume id
    /// to a process that still holds the transcript — there is nothing to
    /// restore after a shell restart. Saying so with an error is the point: the
    /// previous stub manufactured an empty session, and the client then showed a
    /// long conversation while prompting the model with no context at all.
    pub async fn handle_load_session(
        &self,
        thread_id: String,
        on_update_cb: Option<&(dyn Fn(SessionUpdate) + Send + Sync)>,
    ) -> Result<LoadSessionResponse, String> {
        let Some(session) = self.existing_session(&thread_id).await else {
            return Err(format!(
                "openrouter has no transcript for thread `{thread_id}`: history is held in this \
                 process only and does not survive a restart"
            ));
        };
        let history = session.lock().await.history.clone();
        if let Some(cb) = on_update_cb {
            for msg in history {
                let chunk = text_chunk(msg.content);
                cb(if msg.role == "assistant" {
                    SessionUpdate::AgentMessageChunk(chunk)
                } else {
                    SessionUpdate::UserMessageChunk(chunk)
                });
            }
        }
        Ok(LoadSessionResponse::new())
    }

    /// OpenRouter exposes no mode concept. Reported as unsupported rather than
    /// silently accepted — an `Ok(())` would tell the UI the setting took.
    pub async fn handle_set_mode(
        &self,
        _thread_id: String,
        _mode_id: String,
    ) -> Result<(), String> {
        Err("openrouter has no mode surface".to_string())
    }

    /// Store the model for subsequent turns. An empty string clears it so the
    /// pkg setting takes over again.
    pub async fn handle_set_model(
        &self,
        thread_id: String,
        model: Option<String>,
    ) -> Result<(), String> {
        let session = self.register_session(thread_id, String::new()).await;
        session.lock().await.model = model.filter(|m| !m.is_empty());
        Ok(())
    }

    /// Declare the tools this thread's model may call. An empty list clears
    /// them, which returns the thread to prose-only replies.
    pub async fn handle_set_tools(
        &self,
        thread_id: String,
        tools: Vec<serde_json::Value>,
    ) -> Result<(), String> {
        let normalized = normalize_tool_defs(tools)?;
        let session = self.register_session(thread_id, String::new()).await;
        session.lock().await.tools = normalized;
        Ok(())
    }

    /// Reasoning effort is per-model on OpenRouter (and expressed as a
    /// `reasoning` request field, not a session setting), so there is nothing
    /// session-level to set.
    pub async fn handle_set_effort(
        &self,
        _thread_id: String,
        _effort: crate::claude::session::EffortLevel,
    ) -> Result<(), String> {
        Err("openrouter reasoning effort is selected per model, not per session".to_string())
    }
}

fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .tcp_keepalive(TCP_KEEPALIVE)
        .build()
        .map_err(|e| format!("openrouter http client unavailable: {e}"))
}

/// Apply one normalised event to the turn's accumulators + emit its envelope.
pub fn handle_event(
    event: OrEvent,
    on_update_cb: Option<&(dyn Fn(SessionUpdate) + Send + Sync)>,
    turn: &mut TurnState,
) {
    match &event {
        OrEvent::Text(t) => turn.assistant_text.push_str(t),
        OrEvent::Finish(reason) => {
            turn.saw_terminal = true;
            turn.stop_reason = Some(map_finish_reason(reason));
        }
        // `[DONE]` without a `finish_reason` still means the provider closed the
        // turn deliberately; only the *absence* of both means truncation.
        OrEvent::Done => turn.saw_terminal = true,
        OrEvent::Usage {
            input_tokens,
            output_tokens,
        } => turn.usage = Some((*input_tokens, *output_tokens)),
        OrEvent::Error(msg) => {
            turn.stop_reason = Some(StopReason::Refusal);
            turn.saw_terminal = true;
            if turn.error.is_none() {
                turn.error = Some(format!("openrouter: {msg}"));
            }
        }
        OrEvent::Thinking(_) | OrEvent::ToolUse { .. } => {}
    }
    if let (Some(cb), Some(update)) = (on_update_cb, update_for_event(&event)) {
        cb(update);
    }
}

pub type OpenRouterHttpEngineState = Arc<OpenRouterHttpEngine>;

/// Flatten a prompt into the single string the completions endpoint takes.
///
/// Embedded *text* resources are inlined (that is what `embeddedContext: true`
/// promises); images and audio are refused rather than silently discarded,
/// because the prompt capabilities declare no support for them and a dropped
/// attachment produces an answer that looks like the model ignored the user.
pub fn extract_prompt_text(req: &PromptRequest) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    for block in &req.prompt {
        match block {
            ContentBlock::Text(t) => parts.push(t.text.clone()),
            ContentBlock::ResourceLink(link) => {
                parts.push(format!("[resource: {} <{}>]", link.name, link.uri));
            }
            ContentBlock::Resource(res) => match &res.resource {
                EmbeddedResourceResource::TextResourceContents(t) => {
                    parts.push(format!("<resource uri=\"{}\">\n{}\n</resource>", t.uri, t.text));
                }
                EmbeddedResourceResource::BlobResourceContents(b) => {
                    return Err(format!(
                        "openrouter engine cannot send binary attachments ({}); paste the text \
                         instead",
                        b.uri
                    ));
                }
                _ => {
                    return Err(
                        "openrouter engine cannot send this embedded resource type".to_string()
                    )
                }
            },
            ContentBlock::Image(_) => {
                return Err(
                    "openrouter engine does not accept image prompt blocks (promptCapabilities \
                     declare image: false)"
                        .to_string(),
                )
            }
            ContentBlock::Audio(_) => {
                return Err(
                    "openrouter engine does not accept audio prompt blocks (promptCapabilities \
                     declare audio: false)"
                        .to_string(),
                )
            }
            other => {
                return Err(format!(
                    "openrouter engine cannot send this prompt block type: {}",
                    serde_json::to_value(other)
                        .ok()
                        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
                        .unwrap_or_else(|| "unknown".into())
                ))
            }
        }
    }
    Ok(parts.join("\n"))
}

/// Append up to a total of `cap` bytes; `true` once the buffer is full.
fn append_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    let room = cap.saturating_sub(buf.len());
    if room == 0 {
        return true;
    }
    let take = room.min(chunk.len());
    buf.extend_from_slice(&chunk[..take]);
    buf.len() >= cap
}

/// Read at most `cap` bytes of a response body.
///
/// `Response::text()` would materialise the whole thing first, and the body on
/// this path comes from whatever host `base_url` named — including a gateway
/// that answers a failed turn with megabytes of HTML.
async fn read_capped_body(response: reqwest::Response, cap: usize) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(next) = stream.next().await {
        let Ok(chunk) = next else { break };
        if append_capped(&mut buf, &chunk, cap) {
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Trim an error body to something safe and short for a toast. OpenRouter
/// returns JSON; anything else gets truncated raw.
pub fn summarise_error_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "(no response body)".to_string();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(msg) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .filter(|s| !s.is_empty())
        {
            return msg.to_string();
        }
    }
    let mut s: String = trimmed.chars().take(300).collect();
    if trimmed.chars().count() > 300 {
        s.push('…');
    }
    s
}

// ─── config + credential resolution ─────────────────────────────────────────

/// Resolve endpoint, model and API key for one turn.
///
/// `model_override` (per-turn or sticky session model) beats the pkg setting,
/// which beats the manifest's declared default, which beats [`DEFAULT_MODEL`].
pub async fn resolve_config(
    app: &AppHandle,
    model_override: Option<String>,
) -> Result<OpenRouterConfig, String> {
    let (api_key, net) = resolve_key_and_policy(app).await?;
    let settings = read_pkg_settings(app).await;

    let base_url = settings
        .get(SETTING_BASE_URL)
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let endpoint = validate_endpoint(&completions_url(&base_url), &net)?;

    let model = model_override
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            settings
                .get(SETTING_MODEL)
                .cloned()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    Ok(OpenRouterConfig {
        endpoint,
        model,
        api_key,
    })
}

/// The blocking half of config resolution — Stronghold decryption and a
/// manifest read off disk — hoisted onto a blocking thread.
///
/// `SecretsLock` documents its `std::sync::Mutex` as sound *because* all
/// blocking work happens inside `spawn_blocking`. The first read after boot
/// loads and decrypts the whole snapshot while holding that mutex; doing it on a
/// tokio worker stalls the worker and serialises concurrent threads inside async
/// context.
async fn resolve_key_and_policy(app: &AppHandle) -> Result<(String, Vec<String>), String> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        let key_name = resolve_vault_key_name(&app);
        let net = resolve_net_allowlist(&app);
        let key = resolve_api_key(&app, &key_name)?;
        Ok::<(String, Vec<String>), String>((key, net))
    })
    .await
    .map_err(|e| format!("openrouter credential lookup failed to run: {e}"))?
}

/// The key NAME the installed pkg declares.
///
/// This is the first runtime consumer of `engine.onboarding.requiredVaultKeys`;
/// until now the field was parsed and then read by nothing. Falls back to
/// [`DEFAULT_VAULT_KEY`] when the pkg is absent or declares no key, so the
/// engine is still usable before the pkg is installed.
pub fn resolve_vault_key_name(app: &AppHandle) -> String {
    let Some(pkg) = load_engine_pkg(app) else {
        return DEFAULT_VAULT_KEY.to_string();
    };
    pkg.manifest
        .engine
        .as_ref()
        .and_then(|e| e.onboarding.required_vault_keys.first().cloned())
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| DEFAULT_VAULT_KEY.to_string())
}

/// Hosts the bearer token may be sent to: the engine pkg's declared
/// `permissions.net`, or [`DEFAULT_NET_ALLOWLIST`] when the pkg is not installed
/// or declares nothing. Never empty — an empty list would fail closed on the
/// public endpoint too, which just makes the engine unusable.
pub fn resolve_net_allowlist(app: &AppHandle) -> Vec<String> {
    let declared = load_engine_pkg(app)
        .map(|pkg| pkg.manifest.permissions.net.clone())
        .unwrap_or_default();
    if declared.is_empty() {
        return DEFAULT_NET_ALLOWLIST.iter().map(|s| s.to_string()).collect();
    }
    declared
}

/// Load the engine pkg's manifest off disk, or `None` when it is not installed.
/// Blocking file IO — callers must already be on a blocking thread.
fn load_engine_pkg(app: &AppHandle) -> Option<crate::pkg::manifest::Package> {
    let kernel = app.try_state::<crate::commands::KernelState>()?;
    let path = kernel.0.installed_path(OPENROUTER_PKG_ID)?;
    match crate::pkg::manifest::Package::load(&path) {
        Ok(pkg) => Some(pkg),
        Err(e) => {
            log::warn!("[openrouter] manifest for {OPENROUTER_PKG_ID} unreadable ({e})");
            None
        }
    }
}

/// Read the key VALUE out of Stronghold. Blocking; call from `spawn_blocking`.
///
/// Workspace scope first (`read_secret_scoped` folds in its own legacy-unscoped
/// fallback), then the pkg's own scope — the same precedence the ADR-017
/// `pkg_fetch` credential path uses. The value is returned by move and never
/// logged; failures report the key *name* only.
pub fn resolve_api_key(app: &AppHandle, key_name: &str) -> Result<String, String> {
    let Some(lock) = app.try_state::<SecretsLock>() else {
        return Err("secrets vault unavailable".to_string());
    };
    let value = read_secret_scoped(app, lock.inner(), &Scope::Workspace, key_name)
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            read_secret_scoped(app, lock.inner(), &Scope::pkg(OPENROUTER_PKG_ID), key_name)
                .ok()
                .flatten()
                .filter(|v| !v.trim().is_empty())
        });

    value.ok_or_else(|| {
        format!(
            "{key_name} is not set. Add it in Settings → Secrets (workspace scope, or scoped to \
             {OPENROUTER_PKG_ID})."
        )
    })
}

/// `pkg_settings` rows for the engine pkg, as plain strings.
///
/// Async rather than the `block_on` shape `pkg_settings_get` uses: that command
/// is a *sync* Tauri handler, whereas this runs inside the prompt future — a
/// `tauri::async_runtime::block_on` there would panic ("cannot start a runtime
/// from within a runtime").
///
/// Best-effort: a DB error yields an empty map and the caller falls back to
/// declared/compiled defaults rather than failing the turn. Manifest-declared
/// defaults form the baseline, exactly as `pkg_settings_get` merges them.
async fn read_pkg_settings(app: &AppHandle) -> HashMap<String, String> {
    let mut out = HashMap::new();

    if let Some(settings) = app.try_state::<crate::commands::PkgSettingsState>() {
        if let Some(fields) = settings.0.schema_for(OPENROUTER_PKG_ID) {
            for field in fields {
                if let Some(s) = json_as_string(&field.default) {
                    out.insert(field.key, s);
                }
            }
        }
    }

    // The `State` guard is dropped before the first await — it borrows the
    // app's state map and has no business being held across one.
    let db = {
        let Some(state) = app.try_state::<Arc<crate::commands::db::PaDb>>() else {
            return out;
        };
        state.inner().clone()
    };

    let rows: Vec<(String, String)> = match db.ensure_pool().await {
        Ok(pool) => sqlx::query_as("SELECT key, value_json FROM pkg_settings WHERE pkg_id = ?")
            .bind(OPENROUTER_PKG_ID)
            .fetch_all(&pool)
            .await
            .unwrap_or_else(|e| {
                log::warn!("[openrouter] read pkg_settings: {e}");
                Vec::new()
            }),
        Err(e) => {
            log::warn!("[openrouter] pkg_settings db unavailable: {e}");
            Vec::new()
        }
    };

    for (key, value_json) in rows {
        let parsed = serde_json::from_str::<serde_json::Value>(&value_json)
            .unwrap_or(serde_json::Value::String(value_json));
        if let Some(s) = json_as_string(&parsed) {
            out.insert(key, s);
        }
    }
    out
}

/// Settings values round-trip as schemaless JSON, so a string setting can come
/// back as a bare string or as a JSON string. Non-scalars are ignored.
fn json_as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_net() -> Vec<String> {
        DEFAULT_NET_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn completions_url_handles_the_shapes_a_user_will_actually_type() {
        assert_eq!(
            completions_url("https://openrouter.ai/api/v1"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            completions_url("https://openrouter.ai/api/v1/"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        // Already-complete endpoint must not be doubled up.
        assert_eq!(
            completions_url("https://proxy.internal/v1/chat/completions"),
            "https://proxy.internal/v1/chat/completions"
        );
        // Empty setting falls back to the public default, never to a bare path.
        assert_eq!(
            completions_url("   "),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn the_default_endpoint_passes_the_allowlist() {
        assert_eq!(
            validate_endpoint(&completions_url(DEFAULT_BASE_URL), &default_net()).unwrap(),
            "https://openrouter.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn a_base_url_pointing_off_the_allowlist_never_gets_the_key() {
        // The attack: one `pkg_settings_set` write redirects the bearer token.
        for hostile in [
            "http://attacker.example/v1",
            "https://attacker.example/v1",
            "https://openrouter.ai.attacker.example/api/v1",
        ] {
            let err = validate_endpoint(&completions_url(hostile), &default_net())
                .expect_err("must refuse");
            assert!(
                err.contains("https") || err.contains("allowlist"),
                "unexpected refusal for {hostile}: {err}"
            );
        }
    }

    #[test]
    fn plaintext_http_is_refused_even_on_the_allowlisted_host() {
        let err = validate_endpoint("http://openrouter.ai/api/v1/chat/completions", &default_net())
            .expect_err("must refuse");
        assert!(err.contains("https"), "{err}");
    }

    #[test]
    fn a_declared_proxy_is_honoured_and_an_http_entry_is_the_only_plaintext_opt_in() {
        let net = vec!["https://proxy.internal/".to_string()];
        assert!(validate_endpoint("https://proxy.internal/v1/chat/completions", &net).is_ok());
        assert!(validate_endpoint("http://proxy.internal/v1/chat/completions", &net).is_err());

        let dev = vec!["http://127.0.0.1:11434/".to_string()];
        assert!(validate_endpoint("http://127.0.0.1:11434/v1/chat/completions", &dev).is_ok());
    }

    #[test]
    fn a_base_url_with_embedded_credentials_is_refused() {
        let err = validate_endpoint(
            "https://user:pass@openrouter.ai/api/v1/chat/completions",
            &default_net(),
        )
        .expect_err("must refuse");
        assert!(err.contains("credentials"), "{err}");
    }

    #[test]
    fn request_body_is_streaming_and_carries_the_model_and_messages() {
        let msgs = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        let body = build_request_body("deepseek/deepseek-r1", &msgs, &[]);
        assert_eq!(body["model"], "deepseek/deepseek-r1");
        assert_eq!(body["stream"], true);
        assert_eq!(body["usage"]["include"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
        assert_eq!(body["messages"][1]["role"], "assistant");
        // No tools declared → no `tools` key at all (an empty array is not the
        // same thing to every upstream).
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn declared_tools_reach_the_wire_or_no_tool_call_can_ever_come_back() {
        let tools = normalize_tool_defs(vec![serde_json::json!({
            "name": "get_weather",
            "description": "Look up weather",
            "inputSchema": {"type": "object", "properties": {"city": {"type": "string"}}}
        })])
        .expect("normalizes");
        let body = build_request_body("openrouter/auto", &[ChatMessage::user("hi")], &tools);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["tools"][0]["function"]["description"], "Look up weather");
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["city"]["type"],
            "string"
        );
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn tool_defs_already_in_wire_shape_pass_through_and_nameless_ones_are_rejected() {
        let wire = serde_json::json!({"type":"function","function":{"name":"x","parameters":{}}});
        assert_eq!(
            normalize_tool_defs(vec![wire.clone()]).unwrap(),
            vec![wire]
        );
        assert!(normalize_tool_defs(vec![serde_json::json!({"description": "no name"})]).is_err());
    }

    #[tokio::test]
    async fn set_tools_stores_normalized_defs_and_an_empty_list_clears_them() {
        let engine = OpenRouterHttpEngine::new();
        engine
            .handle_set_tools("t-tools".into(), vec![serde_json::json!({"name": "search"})])
            .await
            .unwrap();
        let s = engine.register_session("t-tools".into(), String::new()).await;
        assert_eq!(s.lock().await.tools.len(), 1);
        engine.handle_set_tools("t-tools".into(), Vec::new()).await.unwrap();
        assert!(s.lock().await.tools.is_empty());
    }

    #[test]
    fn finish_reasons_map_to_distinct_stop_reasons() {
        assert_eq!(map_finish_reason("stop"), StopReason::EndTurn);
        // A tool-call turn ended cleanly — the client runs the tools next.
        assert_eq!(map_finish_reason("tool_calls"), StopReason::EndTurn);
        // Truncation must not be reported as a clean stop.
        assert_eq!(map_finish_reason("length"), StopReason::MaxTokens);
        assert_eq!(map_finish_reason("content_filter"), StopReason::Refusal);
        // A provider-side abort is a cancel, not a completed turn (stream.ts:124).
        assert_eq!(map_finish_reason("cancelled"), StopReason::Cancelled);
        assert_eq!(map_finish_reason("canceled"), StopReason::Cancelled);
    }

    #[test]
    fn thinking_and_text_events_map_to_different_acp_chunks() {
        let think = update_for_event(&OrEvent::Thinking("weighing".into())).expect("update");
        let text = update_for_event(&OrEvent::Text("answer".into())).expect("update");
        assert!(matches!(think, SessionUpdate::AgentThoughtChunk(_)));
        assert!(matches!(text, SessionUpdate::AgentMessageChunk(_)));
    }

    #[test]
    fn tool_use_parses_its_arguments_and_falls_back_to_raw_on_partial_json() {
        let ok = update_for_event(&OrEvent::ToolUse {
            id: "call_1".into(),
            name: "search_files".into(),
            arguments: r#"{"q":"beats"}"#.into(),
        })
        .expect("update");
        let SessionUpdate::ToolCall(call) = ok else {
            panic!("expected a ToolCall update");
        };
        assert_eq!(call.tool_call_id.0.as_ref(), "call_1");
        // A readable row header + a kind the UI can pick an icon from, the same
        // two things claude_code's mapping supplies.
        assert_eq!(call.title, "Search files");
        assert_eq!(call.kind, ToolKind::Search);
        assert_eq!(call.raw_input, Some(serde_json::json!({"q": "beats"})));

        let partial = update_for_event(&OrEvent::ToolUse {
            id: "call_2".into(),
            name: "search".into(),
            arguments: r#"{"q":"#.into(),
        })
        .expect("update");
        let SessionUpdate::ToolCall(call) = partial else {
            panic!("expected a ToolCall update");
        };
        assert_eq!(
            call.raw_input,
            Some(serde_json::Value::String(r#"{"q":"#.into()))
        );
    }

    #[test]
    fn tool_kinds_and_titles_are_derived_from_the_function_name() {
        assert_eq!(tool_kind_for("read_file"), ToolKind::Read);
        assert_eq!(tool_kind_for("write_file"), ToolKind::Edit);
        assert_eq!(tool_kind_for("run_bash"), ToolKind::Execute);
        assert_eq!(tool_kind_for("web_fetch"), ToolKind::Fetch);
        assert_eq!(tool_kind_for("grep_repo"), ToolKind::Search);
        assert_eq!(tool_kind_for("wibble"), ToolKind::Other);
        assert_eq!(tool_title_for("mcp__royalti-cms__findPosts"), "FindPosts");
        assert_eq!(tool_title_for("get_weather"), "Get weather");
    }

    #[test]
    fn turn_level_events_produce_no_envelope() {
        assert!(update_for_event(&OrEvent::Done).is_none());
        assert!(update_for_event(&OrEvent::Finish("stop".into())).is_none());
        assert!(update_for_event(&OrEvent::Error("boom".into())).is_none());
        assert!(update_for_event(&OrEvent::Usage {
            input_tokens: 1,
            output_tokens: 2
        })
        .is_none());
    }

    #[test]
    fn handle_event_accumulates_text_and_records_the_stop_reason() {
        let mut turn = TurnState::default();
        handle_event(OrEvent::Text("Hel".into()), None, &mut turn);
        handle_event(OrEvent::Text("lo".into()), None, &mut turn);
        // Thinking is streamed to the client but is NOT part of the reply that
        // goes back into the conversation history.
        handle_event(OrEvent::Thinking("hmm".into()), None, &mut turn);
        handle_event(OrEvent::Finish("length".into()), None, &mut turn);
        assert_eq!(turn.assistant_text, "Hello");
        assert_eq!(turn.stop_reason(), StopReason::MaxTokens);
        assert!(turn.saw_terminal);
        assert!(turn.error.is_none());
    }

    #[test]
    fn a_stream_with_no_finish_marker_is_not_a_clean_end_turn() {
        // The failure this guards: a proxy closing the body mid-answer produced
        // `Ok(EndTurn)` and committed half a reply to history.
        let mut turn = TurnState::default();
        handle_event(OrEvent::Text("half an ans".into()), None, &mut turn);
        assert!(!turn.saw_terminal, "no finish_reason and no [DONE]");
        // …whereas either marker on its own closes the turn.
        let mut done_only = TurnState::default();
        handle_event(OrEvent::Done, None, &mut done_only);
        assert!(done_only.saw_terminal);
        assert_eq!(done_only.stop_reason(), StopReason::EndTurn);
    }

    #[test]
    fn usage_is_captured_and_surfaced_on_the_response() {
        let mut turn = TurnState::default();
        handle_event(
            OrEvent::Usage {
                input_tokens: 11,
                output_tokens: 7,
            },
            None,
            &mut turn,
        );
        assert_eq!(turn.usage, Some((11, 7)));
        let usage = turn.acp_usage().expect("usage surfaced");
        assert_eq!(usage.input_tokens, 11);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.total_tokens, 18);
        assert!(TurnState::default().acp_usage().is_none());
    }

    #[test]
    fn a_stream_error_refuses_the_turn_and_keeps_the_first_message() {
        let mut turn = TurnState::default();
        handle_event(OrEvent::Error("rate limited".into()), None, &mut turn);
        handle_event(OrEvent::Error("second".into()), None, &mut turn);
        assert_eq!(turn.stop_reason(), StopReason::Refusal);
        assert_eq!(turn.error.as_deref(), Some("openrouter: rate limited"));
    }

    #[test]
    fn error_bodies_are_summarised_without_dumping_the_whole_page() {
        assert_eq!(
            summarise_error_body(r#"{"error":{"message":"No auth credentials found","code":401}}"#),
            "No auth credentials found"
        );
        assert_eq!(summarise_error_body("   "), "(no response body)");
        let long = "x".repeat(400);
        let summarised = summarise_error_body(&long);
        assert_eq!(summarised.chars().count(), 301, "300 chars plus an ellipsis");
    }

    #[test]
    fn error_bodies_stop_being_read_at_the_cap() {
        // A gateway answering with megabytes of HTML must not be materialised
        // just to print 300 characters of it.
        let mut buf = Vec::new();
        let chunk = vec![b'x'; MAX_ERROR_BODY_BYTES / 2];
        assert!(!append_capped(&mut buf, &chunk, MAX_ERROR_BODY_BYTES), "half full");
        assert!(append_capped(&mut buf, &chunk, MAX_ERROR_BODY_BYTES), "full");
        assert_eq!(buf.len(), MAX_ERROR_BODY_BYTES);
        // A chunk that straddles the cap is truncated, not dropped or appended
        // whole.
        let mut partial = vec![b'x'; MAX_ERROR_BODY_BYTES - 10];
        assert!(append_capped(
            &mut partial,
            &vec![b'y'; 4096],
            MAX_ERROR_BODY_BYTES
        ));
        assert_eq!(partial.len(), MAX_ERROR_BODY_BYTES);
        let huge = vec![b'x'; 4096];
        assert!(append_capped(&mut buf, &huge, MAX_ERROR_BODY_BYTES));
        assert_eq!(buf.len(), MAX_ERROR_BODY_BYTES, "never grows past the cap");
    }

    #[test]
    fn history_is_trimmed_to_a_bounded_window_that_starts_on_a_user_message() {
        let mut long = Vec::new();
        for i in 0..60 {
            long.push(ChatMessage::user(format!("q{i}")));
            long.push(ChatMessage::assistant(format!("a{i}")));
        }
        let trimmed = trim_history(long);
        assert!(trimmed.len() <= MAX_HISTORY_MESSAGES);
        assert!(trimmed[0].is_user(), "window must open on a user message");
        assert_eq!(trimmed.last().unwrap().content, "a59", "newest is kept");
    }

    #[test]
    fn history_trimming_also_respects_the_character_budget() {
        let big = "x".repeat(MAX_HISTORY_CHARS / 2 + 10);
        let msgs = vec![
            ChatMessage::user(big.clone()),
            ChatMessage::assistant(big.clone()),
            ChatMessage::user("newest"),
        ];
        let trimmed = trim_history(msgs);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0].content, "newest");
    }

    #[test]
    fn a_single_oversized_message_is_never_dropped() {
        let huge = "x".repeat(MAX_HISTORY_CHARS * 2);
        let trimmed = trim_history(vec![ChatMessage::user(huge.clone())]);
        assert_eq!(trimmed.len(), 1);
        assert_eq!(trimmed[0].content.len(), huge.len());
    }

    #[test]
    fn a_short_history_is_returned_untouched() {
        let msgs = vec![
            ChatMessage::user("a"),
            ChatMessage::assistant("b"),
            ChatMessage::user("c"),
        ];
        assert_eq!(trim_history(msgs.clone()), msgs);
    }

    #[test]
    fn the_config_debug_impl_never_prints_the_key() {
        let cfg = OpenRouterConfig {
            endpoint: DEFAULT_BASE_URL.into(),
            model: "openrouter/auto".into(),
            api_key: "sk-or-v1-TOPSECRET".into(),
        };
        let rendered = format!("{cfg:?}");
        assert!(!rendered.contains("TOPSECRET"), "key must never be printed");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn settings_values_decode_from_either_json_form() {
        assert_eq!(
            json_as_string(&serde_json::json!("deepseek/deepseek-r1")).as_deref(),
            Some("deepseek/deepseek-r1")
        );
        assert_eq!(json_as_string(&serde_json::json!(7)).as_deref(), Some("7"));
        assert!(json_as_string(&serde_json::json!({"a": 1})).is_none());
        assert!(json_as_string(&serde_json::Value::Null).is_none());
    }

    #[test]
    fn the_http_client_is_a_result_so_a_tls_failure_cannot_panic_the_shell() {
        // `unwrap_or_default()` would call `Client::new()`, which panics on the
        // same failure — during `run()`, taking the whole app down.
        let engine = OpenRouterHttpEngine::new();
        assert!(engine.client().is_ok(), "TLS backend available in tests");
    }

    #[tokio::test]
    async fn a_prompt_without_an_attached_app_fails_the_turn_rather_than_panicking() {
        let engine = OpenRouterHttpEngine::new();
        let err = engine
            .run_prompt("t-detached", "hi", None, None)
            .await
            .expect_err("no AppHandle attached");
        assert!(err.contains("not attached"), "{err}");
    }

    #[tokio::test]
    async fn a_cancel_that_lands_during_setup_is_reported_as_cancelled_not_an_error() {
        let engine = OpenRouterHttpEngine::new();
        let session = engine.register_session("t-race".into(), String::new()).await;
        session.lock().await.cancelled = true;
        let resp = engine
            .fail_turn(&session, "key lookup failed".into())
            .await
            .expect("ACP requires Cancelled once session/cancel landed");
        assert_eq!(resp.stop_reason, StopReason::Cancelled);
        assert!(session.lock().await.abort.is_none(), "abort slot released");
    }

    #[tokio::test]
    async fn a_failed_setup_leaves_the_transcript_untouched() {
        // The invariant the old unwind comments tried to describe, now
        // structural: nothing is written to history until a turn succeeds.
        let engine = OpenRouterHttpEngine::new();
        let session = engine.register_session("t-fail".into(), String::new()).await;
        session.lock().await.history.push(ChatMessage::user("earlier"));
        let err = engine
            .fail_turn(&session, "openrouter request failed".into())
            .await
            .expect_err("still an error");
        assert_eq!(err, "openrouter request failed");
        let s = session.lock().await;
        assert_eq!(s.history.len(), 1, "no phantom user message");
        assert!(s.abort.is_none());
    }

    #[tokio::test]
    async fn cancel_is_sticky_until_a_turn_consumes_it() {
        let engine = OpenRouterHttpEngine::new();
        engine.register_session("t-1".into(), "/tmp".into()).await;
        engine.handle_cancel("t-1".into()).await.unwrap();
        let s = engine.register_session("t-1".into(), String::new()).await;
        assert!(s.lock().await.cancelled);
    }

    #[tokio::test]
    async fn cancel_on_an_unknown_thread_is_not_an_error() {
        // Stale Stop clicks must never fail — same contract as the CLI engines.
        let engine = OpenRouterHttpEngine::new();
        engine
            .handle_cancel("never-existed".into())
            .await
            .expect("cancel on absent thread is Ok");
    }

    #[tokio::test]
    async fn cancel_wakes_a_reader_parked_on_a_stalled_stream() {
        // The bug this guards: the abort flag was only read *after*
        // `stream.next()` returned, so a stalled socket (sleep, dropped Wi-Fi,
        // provider hang) could never be cancelled at all.
        let engine = OpenRouterHttpEngine::new();
        let session = engine.register_session("t-stall".into(), String::new()).await;
        let abort = Arc::new(AbortSignal::new());
        session.lock().await.abort = Some(abort.clone());

        let waiter = tokio::spawn({
            let abort = abort.clone();
            async move {
                tokio::select! {
                    () = abort.wait() => "cancelled",
                    // Stands in for a body chunk that never arrives.
                    () = std::future::pending::<()>() => "chunk",
                }
            }
        });

        engine.handle_cancel("t-stall".into()).await.unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("cancel must not depend on the next chunk")
            .expect("waiter task");
        assert_eq!(outcome, "cancelled");
    }

    #[tokio::test]
    async fn a_cancel_that_lands_before_the_reader_parks_is_not_lost() {
        let abort = AbortSignal::new();
        abort.trigger();
        tokio::time::timeout(std::time::Duration::from_secs(5), abort.wait())
            .await
            .expect("a pre-park trigger must still resolve");
        assert!(abort.is_aborted());
    }

    #[tokio::test]
    async fn set_model_stores_and_an_empty_string_clears() {
        let engine = OpenRouterHttpEngine::new();
        engine
            .handle_set_model("t-2".into(), Some("x-ai/grok-4".into()))
            .await
            .unwrap();
        let s = engine.register_session("t-2".into(), String::new()).await;
        assert_eq!(s.lock().await.model.as_deref(), Some("x-ai/grok-4"));
        engine
            .handle_set_model("t-2".into(), Some(String::new()))
            .await
            .unwrap();
        assert!(s.lock().await.model.is_none());
    }

    #[tokio::test]
    async fn unsupported_controls_are_refused_rather_than_silently_accepted() {
        let engine = OpenRouterHttpEngine::new();
        assert!(engine.handle_set_mode("t".into(), "plan".into()).await.is_err());
        assert!(engine
            .handle_set_effort("t".into(), crate::claude::session::EffortLevel::High)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn register_session_does_not_downgrade_a_known_cwd() {
        let engine = OpenRouterHttpEngine::new();
        engine
            .register_session("t-3".into(), "/work/project".into())
            .await;
        let s = engine.register_session("t-3".into(), String::new()).await;
        assert_eq!(s.lock().await.cwd, "/work/project");
    }

    #[tokio::test]
    async fn load_session_refuses_a_thread_this_process_never_saw() {
        // The stub it replaces silently manufactured an empty session, so the
        // UI restored a long conversation the model was never shown.
        let engine = OpenRouterHttpEngine::new();
        let err = engine
            .handle_load_session("gone-after-restart".into(), None)
            .await
            .expect_err("nothing to resume");
        assert!(err.contains("does not survive a restart"), "{err}");
        assert!(
            engine.existing_session("gone-after-restart").await.is_none(),
            "a failed load must not create a session"
        );
    }

    #[tokio::test]
    async fn load_session_replays_the_in_process_transcript() {
        let engine = OpenRouterHttpEngine::new();
        let session = engine.register_session("t-load".into(), String::new()).await;
        {
            let mut s = session.lock().await;
            s.history.push(ChatMessage::user("q"));
            s.history.push(ChatMessage::assistant("a"));
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        {
            let seen = seen.clone();
            let cb = move |u: SessionUpdate| seen.lock().unwrap().push(u);
            let cb_ref: &(dyn Fn(SessionUpdate) + Send + Sync) = &cb;
            engine
                .handle_load_session("t-load".into(), Some(cb_ref))
                .await
                .expect("replays");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(matches!(seen[0], SessionUpdate::UserMessageChunk(_)));
        assert!(matches!(seen[1], SessionUpdate::AgentMessageChunk(_)));
    }

    #[test]
    fn prompt_extraction_inlines_text_resources_and_refuses_what_it_cannot_send() {
        use agent_client_protocol::schema::{
            EmbeddedResource, ImageContent, ResourceLink, TextResourceContents,
        };

        let req = PromptRequest::new(
            SessionId::new("t"),
            vec![
                ContentBlock::Text(TextContent::new("summarise this")),
                ContentBlock::Resource(EmbeddedResource::new(
                    EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
                        "line one",
                        "file:///notes.md",
                    )),
                )),
                ContentBlock::ResourceLink(ResourceLink::new("notes.md", "file:///notes.md")),
            ],
        );
        let text = extract_prompt_text(&req).expect("text + embedded context");
        assert!(text.contains("summarise this"));
        assert!(text.contains("line one"), "embedded text must be inlined");
        assert!(text.contains("file:///notes.md"));

        // An attachment that cannot be sent must fail the turn, not vanish — a
        // silently dropped image makes the model look like it ignored the user.
        let with_image = PromptRequest::new(
            SessionId::new("t"),
            vec![ContentBlock::Image(ImageContent::new("AAAA", "image/png"))],
        );
        let err = extract_prompt_text(&with_image).expect_err("images are refused");
        assert!(err.contains("image"), "{err}");
    }

    #[test]
    fn the_advertised_prompt_capabilities_match_what_the_engine_accepts() {
        let engine = OpenRouterHttpEngine::new();
        let resp = engine.handle_initialize(InitializeRequest::new(ProtocolVersion::V1));
        let caps = resp.agent_capabilities;
        assert!(!caps.prompt_capabilities.image, "images are refused, not sent");
        assert!(!caps.prompt_capabilities.audio);
        assert!(caps.prompt_capabilities.embedded_context, "text resources inline");
        assert!(caps.load_session);
        // No MCP server list is consumed, so claiming the transports would be a
        // lie the settings UI would act on.
        assert!(!caps.mcp_capabilities.http);
        assert!(!caps.mcp_capabilities.sse);
    }
}
