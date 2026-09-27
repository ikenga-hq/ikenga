//! Incremental parser for `claude --output-format stream-json --verbose`.
//!
//! Bytes arrive from the PTY in arbitrary chunks. We buffer until `\n`, then
//! parse each line as a JSON envelope. Top-level envelope types captured
//! 2026-04-30 against Claude Code v2.1.123:
//!
//! | type                | subtype        | meaning                     |
//! |---------------------|----------------|-----------------------------|
//! | `system`            | `init`         | session bootstrap           |
//! | `system`            | `hook_started` | hook lifecycle              |
//! | `system`            | `hook_response`| hook lifecycle              |
//! | `assistant`         | —              | wraps an Anthropic message  |
//! | `user`              | —              | wraps tool_result blocks    |
//! | `rate_limit_event`  | —              | passthrough                 |
//! | `result`            | success/error  | final summary               |
//!
//! Inside `assistant.message.content[]` and `user.message.content[]` we walk
//! Anthropic-style blocks: `text`, `thinking`, `tool_use`, `tool_result`.

use serde_json::Value;

use super::event::ChatEvent;

#[derive(Default)]
pub struct StreamParser {
    /// Accumulator for the current incomplete line.
    buf: Vec<u8>,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed PTY bytes; emit zero or more parsed events.
    ///
    /// Lines are LF-delimited. CR is tolerated. Anything that is not valid
    /// JSON yields a `ParseError` event and the parser continues — never
    /// poison the stream over a single bad line.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ChatEvent> {
        let mut out = Vec::new();
        for byte in bytes {
            match *byte {
                b'\n' => {
                    let line = std::mem::take(&mut self.buf);
                    let line = strip_cr(line);
                    if !line.is_empty() {
                        self.dispatch_line(&line, &mut out);
                    }
                }
                _ => self.buf.push(*byte),
            }
        }
        out
    }

    fn dispatch_line(&self, line: &[u8], out: &mut Vec<ChatEvent>) {
        let text = match std::str::from_utf8(line) {
            Ok(t) => t,
            Err(_) => {
                out.push(ChatEvent::ParseError {
                    message: "non-utf8 line".into(),
                    line: String::from_utf8_lossy(line).to_string(),
                });
                return;
            }
        };
        // Lines that aren't JSON are almost always claude TUI noise that
        // leaked when stream-json wasn't honored — surface, but don't choke.
        let value: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                out.push(ChatEvent::ParseError {
                    message: format!("json: {e}"),
                    line: text.to_string(),
                });
                return;
            }
        };
        dispatch_envelope(&value, out);
    }
}

fn strip_cr(mut buf: Vec<u8>) -> Vec<u8> {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    buf
}

/// Dispatch a single parsed envelope. Shared with the on-disk jsonl reader,
/// which builds the same event vector by feeding lines one at a time.
pub(crate) fn dispatch_envelope(value: &Value, out: &mut Vec<ChatEvent>) {
    let envelope_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match envelope_type {
        "system" => dispatch_system(value, out),
        "assistant" => dispatch_assistant(value, out),
        "user" => dispatch_user(value, out),
        "result" => out.push(ChatEvent::Done {
            usage: value.get("usage").cloned(),
            total_cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
            stop_reason: value
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            duration_ms: value.get("duration_ms").and_then(Value::as_u64),
        }),
        "rate_limit_event" => out.push(ChatEvent::RateLimit {
            info: value.clone(),
        }),
        // Tool-permission round-trip. `claude --permission-prompt-tool stdio`
        // writes one of these to stdout when a tool needs approval; the ACP
        // server forwards it as `session/request_permission`, then writes a
        // control_response back to stdin. Two wire shapes across builds, both
        // normalized here (verified against claude 2.1.150):
        //   • legacy `sdk_control_request` — `request_id` nested under
        //     `.request`, `subtype:"permission"`, input at `.tool_input`.
        //   • 2.1.x  `control_request`      — `request_id` at top level,
        //     `subtype:"can_use_tool"`, input at `.input`, plus `.tool_use_id`.
        // Some builds omit `request_id`; fall back to a generated uuid so the
        // bridge always has a correlation key. `subtype` is preserved verbatim
        // so the server can both route it and mirror the response wire shape.
        "sdk_control_request" | "control_request" => {
            let req = value.get("request").cloned().unwrap_or(Value::Null);
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .or_else(|| req.get("request_id").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            out.push(ChatEvent::ControlRequest {
                request_id,
                subtype: req
                    .get("subtype")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                tool_name: req
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                tool_input: req.get("input").or_else(|| req.get("tool_input")).cloned(),
                tool_use_id: req
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
        // Partial-message streaming (`--include-partial-messages`). We do NOT
        // pass that flag today: it double-emits text (once as `text_delta`
        // here, once in the assembled `assistant` envelope dispatch_assistant
        // reads). This arm is defensive — if the flag is ever enabled, or a
        // future claude build emits these unprompted, we (a) never spam the
        // chat feed with `Unknown` rows, and (b) still capture `thinking_delta`
        // text, which the assembled block never carries (it ships
        // signature-only — see the empty-`thinking` case in dispatch_assistant
        // and the encrypted-thinking note in thread.tsx). Text deltas are
        // intentionally ignored: the assembled path owns text. Everything else
        // (signature_delta, message_start/stop, content_block_start/stop) is
        // bookkeeping and dropped silently.
        "stream_event" => {
            if let Some(delta) = value.get("event").and_then(|e| e.get("delta")) {
                if delta.get("type").and_then(Value::as_str) == Some("thinking_delta") {
                    if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(ChatEvent::Thinking {
                                delta: t.to_string(),
                                message_id: None,
                            });
                        }
                    }
                }
            }
        }
        // Internal/control envelopes — JSONL bookkeeping that's not part of
        // the conversation. Drop silently; they're noise to the chat feed.
        "attachment"
        | "queue-operation"
        | "last-prompt"
        | "skill_listing"
        | "deferred_tools_delta"
        | "command_permissions" => {}
        _ => out.push(ChatEvent::Unknown { raw: value.clone() }),
    }
}

fn dispatch_system(value: &Value, out: &mut Vec<ChatEvent>) {
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    match subtype {
        "init" => out.push(ChatEvent::SessionInit {
            session_id: value
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            cwd: value.get("cwd").and_then(Value::as_str).map(str::to_string),
            permission_mode: value
                .get("permissionMode")
                .and_then(Value::as_str)
                .map(str::to_string),
            claude_code_version: value
                .get("claude_code_version")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        "hook_started" | "hook_response" => out.push(ChatEvent::SystemHook {
            hook_event: subtype.to_string(),
            name: value
                .get("hookName")
                .and_then(Value::as_str)
                .map(str::to_string),
            content: Some(value.clone()),
        }),
        _ => out.push(ChatEvent::Unknown { raw: value.clone() }),
    }
}

fn dispatch_assistant(value: &Value, out: &mut Vec<ChatEvent>) {
    let parent_tool_use_id = value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = value.get("message");
    let message_id = message
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let blocks = match content {
        Some(b) => b,
        None => {
            out.push(ChatEvent::Unknown { raw: value.clone() });
            return;
        }
    };
    for block in blocks {
        let block_type = block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    out.push(ChatEvent::Text {
                        delta: t.to_string(),
                        message_id: message_id.clone(),
                    });
                }
            }
            "thinking" => {
                if let Some(t) = block.get("thinking").and_then(Value::as_str) {
                    out.push(ChatEvent::Thinking {
                        delta: t.to_string(),
                        message_id: message_id.clone(),
                    });
                }
            }
            "tool_use" => out.push(ChatEvent::ToolUse {
                id: block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input: block.get("input").cloned().unwrap_or(Value::Null),
                parent_tool_use_id: parent_tool_use_id.clone(),
            }),
            _ => out.push(ChatEvent::Unknown { raw: block.clone() }),
        }
    }
}

fn dispatch_user(value: &Value, out: &mut Vec<ChatEvent>) {
    let parent_tool_use_id = value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let content = value.get("message").and_then(|m| m.get("content"));
    // user.message.content is sometimes a plain string (slash-command echoes),
    // sometimes an array of blocks. Only the array form carries tool_result.
    let blocks = match content.and_then(Value::as_array) {
        Some(b) => b,
        None => return,
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("tool_result") {
            out.push(ChatEvent::ToolResult {
                id: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                output: block.get("content").cloned().unwrap_or(Value::Null),
                is_error: block
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                parent_tool_use_id: parent_tool_use_id.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_kind(events: &[ChatEvent]) -> &'static str {
        match events.first() {
            Some(ChatEvent::SessionInit { .. }) => "session_init",
            Some(ChatEvent::Text { .. }) => "text",
            Some(ChatEvent::Thinking { .. }) => "thinking",
            Some(ChatEvent::ToolUse { .. }) => "tool_use",
            Some(ChatEvent::ToolResult { .. }) => "tool_result",
            Some(ChatEvent::Done { .. }) => "done",
            Some(ChatEvent::RateLimit { .. }) => "rate_limit",
            Some(ChatEvent::SystemHook { .. }) => "system_hook",
            Some(ChatEvent::Artifact { .. }) => "artifact",
            Some(ChatEvent::ControlRequest { .. }) => "control_request",
            Some(ChatEvent::AskUserQuestion { .. }) => "ask_user_question",
            Some(ChatEvent::Unknown { .. }) => "unknown",
            Some(ChatEvent::ParseError { .. }) => "parse_error",
            None => "<none>",
        }
    }

    #[test]
    fn parses_system_init() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-opus-4-7","cwd":"/x","permissionMode":"default"}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        assert_eq!(first_kind(&events), "session_init");
        match &events[0] {
            ChatEvent::SessionInit {
                session_id, model, ..
            } => {
                assert_eq!(session_id, "abc");
                assert_eq!(model.as_deref(), Some("claude-opus-4-7"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn parses_assistant_tool_use() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":null}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ToolUse { id, name, .. } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "Bash");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_user_tool_result_with_parent() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok","is_error":false}]},"parent_tool_use_id":"toolu_parent"}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ToolResult {
                id,
                is_error,
                parent_tool_use_id,
                ..
            } => {
                assert_eq!(id, "toolu_1");
                assert!(!is_error);
                assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_parent"));
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn parses_done_with_cost() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"result","subtype":"success","total_cost_usd":0.351,"stop_reason":"end_turn","duration_ms":7028}
"#;
        let events = p.feed(line);
        match &events[0] {
            ChatEvent::Done {
                total_cost_usd,
                stop_reason,
                duration_ms,
                ..
            } => {
                assert!((total_cost_usd.unwrap() - 0.351).abs() < 1e-6);
                assert_eq!(stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(*duration_ms, Some(7028));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn handles_partial_lines() {
        let mut p = StreamParser::new();
        let part1 = br#"{"type":"assistant","message":{"content":[{"type":"text","text":"hel"#;
        let part2 = br#"lo"}]},"parent_tool_use_id":null}
"#;
        let mut events = p.feed(part1);
        assert!(events.is_empty(), "no newline yet");
        events = p.feed(part2);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::Text { delta, .. } => assert_eq!(delta, "hello"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn stream_event_thinking_delta_emits_thinking_text() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"step one"}}}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::Thinking { delta, .. } => assert_eq!(delta, "step one"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn stream_event_text_and_signature_deltas_are_dropped() {
        // text is owned by the assembled `assistant` envelope; signature_delta
        // is bookkeeping. Neither should surface as an event here (no dupes).
        let mut p = StreamParser::new();
        let text = br#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}}
"#;
        let sig = br#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc"}}}
"#;
        assert!(p.feed(text).is_empty());
        assert!(p.feed(sig).is_empty());
    }

    #[test]
    fn malformed_line_yields_parse_error_and_continues() {
        let mut p = StreamParser::new();
        let bytes = b"this is not json\n{\"type\":\"result\",\"subtype\":\"success\"}\n";
        let events = p.feed(bytes);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], ChatEvent::ParseError { .. }));
        assert!(matches!(events[1], ChatEvent::Done { .. }));
    }

    #[test]
    fn control_request_parses_from_sdk_envelope() {
        let mut p = StreamParser::new();
        let line = br#"{"type":"sdk_control_request","request":{"request_id":"req_42","subtype":"permission","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Confirm?","options":[{"label":"Yes"}]}]}}}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ControlRequest {
                request_id,
                subtype,
                tool_name,
                tool_input,
                ..
            } => {
                assert_eq!(request_id, "req_42");
                assert_eq!(subtype, "permission");
                assert_eq!(tool_name.as_deref(), Some("AskUserQuestion"));
                assert!(tool_input.is_some());
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    #[test]
    fn modern_control_request_parses_top_level_id_and_input() {
        // claude 2.1.x wire shape: type `control_request`, request_id at the
        // top level, subtype `can_use_tool`, tool input under `input`, plus a
        // `tool_use_id` correlating to the assistant tool_use block.
        let mut p = StreamParser::new();
        let line = br#"{"type":"control_request","request_id":"605f","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"Pick"}]},"tool_use_id":"toolu_01"}}
"#;
        let events = p.feed(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ControlRequest {
                request_id,
                subtype,
                tool_name,
                tool_input,
                tool_use_id,
            } => {
                assert_eq!(request_id, "605f");
                assert_eq!(subtype, "can_use_tool");
                assert_eq!(tool_name.as_deref(), Some("AskUserQuestion"));
                assert!(tool_input.is_some(), "input should map to tool_input");
                assert_eq!(tool_use_id.as_deref(), Some("toolu_01"));
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    #[test]
    fn control_request_synthesizes_request_id_when_missing() {
        let mut p = StreamParser::new();
        // Older claude builds emit no request_id; we generate a uuid so the
        // ACP server always has a stable correlation key.
        let line = br#"{"type":"sdk_control_request","request":{"subtype":"permission","tool_name":"Bash","tool_input":{"command":"ls"}}}
"#;
        let events = p.feed(line);
        match &events[0] {
            ChatEvent::ControlRequest { request_id, .. } => {
                assert!(!request_id.is_empty(), "request_id should be synthesized");
                // Loose check: uuid v4 is 36 chars with dashes.
                assert_eq!(request_id.len(), 36);
            }
            other => panic!("expected ControlRequest, got {other:?}"),
        }
    }

    #[test]
    fn tolerates_crlf() {
        let mut p = StreamParser::new();
        let bytes = b"{\"type\":\"result\",\"subtype\":\"success\"}\r\n";
        let events = p.feed(bytes);
        assert!(matches!(events[0], ChatEvent::Done { .. }));
    }
}
