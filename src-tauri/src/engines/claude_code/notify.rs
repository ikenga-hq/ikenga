//! Phase 9: OS notification + sidebar-badge dispatch for user-attention
//! events (Notification hook, PermissionRequest). The shape is:
//!
//!   1. `engines::claude_code::server::handle_prompt` observes a `SystemHook` event or a
//!      `ControlRequest { subtype: "permission" }`.
//!   2. Builds a `NotifyPayload` via `payload_from_system_hook` /
//!      `payload_from_permission`, emits it as an `acp://notify` Tauri
//!      event.
//!   3. The frontend dispatcher (`src/lib/notifications/acp-notify-bridge.ts`)
//!      receives it, asks "is the window focused AND is the active pane on
//!      this thread?". If yes, no OS notification (the in-UI permission
//!      dialog already covers the user-attention need). Otherwise it fires
//!      `sendNotification` via `tauri-plugin-notification` AND bumps a
//!      per-thread sidebar badge counter in `thread-badges-store`.
//!
//! Doing the dispatch from the frontend rather than calling
//! tauri-plugin-notification from Rust keeps the focus-state check on the
//! side that actually knows about routes + focused panes — the Rust core
//! has no notion of "which TanStack-Router pane is currently displaying
//! which threadId". This module is the pure-function half: take a
//! ChatEvent payload, produce a wire-shaped struct, or `None` for hooks
//! we don't want to surface to the OS.

use serde::Serialize;
use serde_json::Value;

/// Wire payload for `acp://notify`. Serialized to JSON via Tauri's event
/// system; the frontend decodes via the matching TS interface in
/// `src/lib/tauri-cmd.ts` (`AcpNotifyPayload`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyPayload {
    pub thread_id: String,
    pub title: String,
    pub body: String,
    pub kind: NotifyKind,
}

/// Distinguishes the source of the notification on the frontend. Used for
/// per-kind UX (e.g. a different icon for permission requests vs. plain
/// agent notifications) and may feed usage metrics in the future.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotifyKind {
    /// Claude's `Notification` hook fired — the agent itself asked for
    /// the user's attention (e.g. "Need your input on the next step").
    Notification,
    /// `PermissionRequest` round-trip in flight — claude paused on a
    /// tool-use approval. Distinct from `Notification` because the
    /// in-UI dialog ALREADY surfaces the request when the user is on
    /// the right thread; we only emit the OS notification as a
    /// fallback for unfocused / different-thread cases.
    PermissionRequest,
}

/// Translate a `ChatEvent::SystemHook` payload into a wire `NotifyPayload`.
/// Returns `None` for hooks we don't surface to the OS — most hooks
/// (PreToolUse, SessionStart, etc.) are diagnostic-only.
///
/// `hook` is the full `ChatEvent::SystemHook.content` Value. We accept
/// either of the two shapes claude has used in stream-json builds:
///   - `{"hookEventName":"Notification","message":"..."}` (newer)
///   - `{"hook_event_name":"Notification","title":"..","message":".."}`
///
/// Title falls back to the thread id (short form) so the OS notification
/// at minimum tells the user which thread is asking.
pub fn payload_from_system_hook(thread_id: &str, hook: &Value) -> Option<NotifyPayload> {
    // Accept both camelCase and snake_case variants — older stream-json
    // builds used snake_case, newer ones use camelCase.
    let event_name = hook
        .get("hookEventName")
        .and_then(Value::as_str)
        .or_else(|| hook.get("hook_event_name").and_then(Value::as_str))
        .or_else(|| hook.get("hookEvent").and_then(Value::as_str))?;

    if event_name != "Notification" && event_name != "PermissionRequest" {
        // PreToolUse, PostToolUse, SessionStart, Stop, etc. are noisy
        // diagnostics — don't surface them as OS notifications.
        return None;
    }

    let message = hook
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            hook.get("content")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Claude needs your input".to_string());

    let title = hook
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("Claude — {}", short_thread(thread_id)));

    let kind = if event_name == "PermissionRequest" {
        NotifyKind::PermissionRequest
    } else {
        NotifyKind::Notification
    };

    Some(NotifyPayload {
        thread_id: thread_id.to_string(),
        title,
        body: message,
        kind,
    })
}

/// Build a `NotifyPayload` for a permission round-trip. The engines::claude_code::server
/// emits this alongside the `acp://session/{threadId}/request` event so the
/// frontend can either render the in-UI `PermissionDialog` (focused case)
/// or fire an OS notification (unfocused case).
pub fn payload_from_permission(
    thread_id: &str,
    tool_name: &str,
    tool_input: Option<&Value>,
) -> NotifyPayload {
    NotifyPayload {
        thread_id: thread_id.to_string(),
        title: format!("Approval needed — {tool_name}"),
        body: short_summary_of_input(tool_input),
        kind: NotifyKind::PermissionRequest,
    }
}

/// First-8-chars of a uuidv4 thread id, used as a human-friendly fallback
/// title when the hook payload doesn't carry one.
fn short_thread(thread_id: &str) -> String {
    thread_id.chars().take(8).collect()
}

/// One-line summary of a tool's input for use as a notification body. We
/// keep it conservative: prefer well-known fields (command, path, url,
/// question) and fall back to a generic "(tap to review)". Also used by the
/// WP-40 permission producers (`notifications::producers`) so a persisted
/// notification row and the OS notification describe a tool call identically.
pub(crate) fn short_summary_of_input(tool_input: Option<&Value>) -> String {
    let Some(input) = tool_input else {
        return "(tap to review)".into();
    };
    // Try the most common high-signal fields first.
    for key in &["command", "path", "url", "file_path", "question"] {
        if let Some(s) = input.get(*key).and_then(Value::as_str) {
            return truncate(s, 120);
        }
    }
    // AskUserQuestion has a `questions[]` array.
    if let Some(questions) = input.get("questions").and_then(Value::as_array) {
        if let Some(q) = questions
            .first()
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
        {
            return truncate(q, 120);
        }
    }
    "(tap to review)".into()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max).collect();
        format!("{trimmed}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_from_permission_includes_tool_name() {
        let p = payload_from_permission("t_abc", "Bash", Some(&json!({"command": "ls -la"})));
        assert_eq!(p.thread_id, "t_abc");
        assert!(p.title.contains("Bash"));
        assert_eq!(p.body, "ls -la");
        assert!(matches!(p.kind, NotifyKind::PermissionRequest));
    }

    #[test]
    fn payload_from_permission_falls_back_when_input_missing() {
        let p = payload_from_permission("t_abc", "Read", None);
        assert!(p.title.contains("Read"));
        assert_eq!(p.body, "(tap to review)");
    }

    #[test]
    fn payload_from_permission_uses_question_for_ask_user() {
        // AskUserQuestion's input has a `questions[]` array; the body
        // should pull out the first question text.
        let input = json!({
            "questions": [
                {"question": "Which color?", "options": [{"label": "Red"}]}
            ]
        });
        let p = payload_from_permission("t_abc", "AskUserQuestion", Some(&input));
        assert_eq!(p.body, "Which color?");
    }

    #[test]
    fn payload_from_permission_truncates_long_body() {
        let cmd: String = "x".repeat(200);
        let p = payload_from_permission("t", "Bash", Some(&json!({"command": cmd.clone()})));
        // Should be truncated to 120 chars + ellipsis.
        assert!(p.body.ends_with('…'));
        assert!(p.body.chars().count() < cmd.len());
    }

    #[test]
    fn payload_from_system_hook_handles_notification_event() {
        let hook = json!({
            "hookEventName": "Notification",
            "title": "Heads up",
            "message": "Need your decision",
        });
        let p = payload_from_system_hook("t_xyz", &hook).expect("notification surfaces");
        assert_eq!(p.thread_id, "t_xyz");
        assert_eq!(p.title, "Heads up");
        assert_eq!(p.body, "Need your decision");
        assert!(matches!(p.kind, NotifyKind::Notification));
    }

    #[test]
    fn payload_from_system_hook_accepts_snake_case_event_name() {
        // Older stream-json builds emitted snake_case; we accept both.
        let hook = json!({
            "hook_event_name": "Notification",
            "message": "Hello",
        });
        let p = payload_from_system_hook("t1", &hook).expect("snake_case ok");
        assert_eq!(p.body, "Hello");
        // Title falls back to the short thread id when not provided.
        assert!(p.title.contains("Claude"));
    }

    #[test]
    fn payload_from_system_hook_handles_permission_request_event() {
        let hook = json!({
            "hookEventName": "PermissionRequest",
            "message": "Need tool approval",
        });
        let p = payload_from_system_hook("t_q", &hook).expect("permission request surfaces");
        assert!(matches!(p.kind, NotifyKind::PermissionRequest));
    }

    #[test]
    fn payload_from_system_hook_ignores_unrelated_hooks() {
        // PreToolUse, PostToolUse, SessionStart, Stop, etc. should NOT
        // produce OS notifications — they're diagnostic-only.
        for name in &[
            "PreToolUse",
            "PostToolUse",
            "SessionStart",
            "Stop",
            "SubagentStop",
            "hook_started",
            "hook_response",
        ] {
            let hook = json!({"hookEventName": name, "message": "x"});
            assert!(
                payload_from_system_hook("t", &hook).is_none(),
                "expected None for hook {name}",
            );
        }
    }

    #[test]
    fn payload_from_system_hook_returns_none_when_event_name_missing() {
        let hook = json!({"message": "no event name"});
        assert!(payload_from_system_hook("t", &hook).is_none());
    }

    #[test]
    fn notify_payload_serializes_with_camel_case_thread_id() {
        // The frontend bridge keys off `threadId` (camelCase) on the wire.
        // Verify the serialization shape because it's the contract for
        // `acp-notify-bridge.ts`.
        let p = NotifyPayload {
            thread_id: "abc".into(),
            title: "t".into(),
            body: "b".into(),
            kind: NotifyKind::Notification,
        };
        let v = serde_json::to_value(&p).expect("serializes");
        assert_eq!(v["threadId"], json!("abc"));
        assert_eq!(v["title"], json!("t"));
        assert_eq!(v["body"], json!("b"));
        assert_eq!(v["kind"], json!("notification"));
    }
}
