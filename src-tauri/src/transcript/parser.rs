//! JSONL transcript line parser (WP-02).
//!
//! Parses raw JSONL records from `~/.claude/projects/<slug>/<session>.jsonl`
//! into strongly-typed `TranscriptRecord` events. Tolerant of unknown fields and
//! newer CLI record types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptRecord {
    User {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default, rename = "parentUuid")]
        parent_uuid: Option<String>,
        #[serde(default)]
        message: Option<UserMessage>,
        #[serde(default, rename = "isMeta")]
        is_meta: Option<bool>,
    },
    Assistant {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default, rename = "parentUuid")]
        parent_uuid: Option<String>,
        #[serde(default)]
        message: Option<AssistantMessage>,
        #[serde(default, rename = "requestId")]
        request_id: Option<String>,
    },
    ToolResult {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default, rename = "toolUseID")]
        tool_use_id: Option<String>,
        #[serde(default, rename = "toolName")]
        tool_name: Option<String>,
        #[serde(default)]
        content: Option<serde_json::Value>,
        #[serde(default)]
        success: Option<bool>,
        #[serde(default)]
        error: Option<String>,
    },
    Progress {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default, rename = "parentToolUseID")]
        parent_tool_use_id: Option<String>,
        #[serde(default)]
        data: Option<serde_json::Value>,
    },
    #[serde(rename = "ai-title")]
    AiTitle {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default, rename = "aiTitle")]
        ai_title: Option<String>,
    },
    Summary {
        #[serde(default)]
        uuid: Option<String>,
        #[serde(default, rename = "sessionId")]
        session_id: Option<String>,
        #[serde(default, rename = "summaryText")]
        summary_text: Option<String>,
    },
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<MessageContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssistantMessage {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Vec<MessageContent>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
}

/// Parses a single JSONL line into a `TranscriptRecord`.
pub fn parse_line(line: &str) -> Option<TranscriptRecord> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str::<TranscriptRecord>(trimmed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_user_line() {
        let raw = r#"{"type":"user","uuid":"u1","sessionId":"s1","message":{"role":"user","content":[{"type":"text","text":"hello world"}]}}"#;
        let record = parse_line(raw).expect("should parse user record");
        match record {
            TranscriptRecord::User { uuid, message, .. } => {
                assert_eq!(uuid.as_deref(), Some("u1"));
                let msg = message.expect("has message");
                assert_eq!(msg.content.len(), 1);
                match &msg.content[0] {
                    MessageContent::Text { text } => assert_eq!(text, "hello world"),
                    _ => panic!("expected text content"),
                }
            }
            _ => panic!("expected User record"),
        }
    }

    #[test]
    fn test_parse_assistant_tool_use() {
        let raw = r#"{"type":"assistant","uuid":"a1","sessionId":"s1","message":{"model":"claude-3-5-sonnet","role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"Bash","input":{"command":"ls -la"}}]}}"#;
        let record = parse_line(raw).expect("should parse assistant record");
        match record {
            TranscriptRecord::Assistant { message, .. } => {
                let msg = message.expect("has message");
                assert_eq!(msg.model.as_deref(), Some("claude-3-5-sonnet"));
                match &msg.content[0] {
                    MessageContent::ToolUse { id, name, input } => {
                        assert_eq!(id, "call_1");
                        assert_eq!(name, "Bash");
                        assert_eq!(input["command"], "ls -la");
                    }
                    _ => panic!("expected ToolUse content"),
                }
            }
            _ => panic!("expected Assistant record"),
        }
    }

    #[test]
    fn test_parse_summary_compaction() {
        let raw = r#"{"type":"summary","uuid":"sum1","sessionId":"s1","summaryText":"Prior conversation compacted."}"#;
        let record = parse_line(raw).expect("should parse summary record");
        match record {
            TranscriptRecord::Summary { summary_text, .. } => {
                assert_eq!(summary_text.as_deref(), Some("Prior conversation compacted."));
            }
            _ => panic!("expected Summary record"),
        }
    }
}
