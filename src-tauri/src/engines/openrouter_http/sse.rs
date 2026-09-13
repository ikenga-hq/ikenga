//! Incremental SSE decoding + OpenRouter chunk normalisation.
//!
//! Everything in this file is pure: bytes in, events out, no network, no Tauri,
//! no `AppHandle`. That is deliberate — it is the half that needs tests, and
//! `server.rs` is the half that needs a live key.
//!
//! Ported from `ikenga-pkgs/packages/engine/openrouter/src/stream.ts`. Three
//! behaviours are carried over verbatim and two are corrections:
//!
//! | Behaviour | stream.ts | here |
//! |---|---|---|
//! | `delta.content` → message delta | yes | yes |
//! | `delta.reasoning` / `delta.thinking` → thinking delta (G-54) | yes | yes |
//! | *leading* `<think>…</think>` in `delta.content` → thinking delta (G-54) | **no** | **yes** ([`ThinkSplitter`]) |
//! | indexed tool-call fragments | dropped unless one delta carried both `id` and `function.name` | accumulated by `index`, flushed at `finish_reason` ([`ToolCallAccumulator`]) |
//! | `usage` / `error` / `finish_reason` | yes | yes |
//!
//! G-54 names two shapes for reasoning tokens: a dedicated `delta.reasoning`
//! field (Claude-family thinking, o-series) and inline `<think>` tags inside
//! ordinary content (DeepSeek R1 and its distills). Both must land on the same
//! `AgentThoughtChunk`, and the tag form has to survive being split across
//! chunk boundaries — `<thi` arriving in one SSE frame and `nk>` in the next is
//! routine, not an edge case.
//!
//! The tag form is recognised **only at the start of the reply**, which is the
//! only place R1 emits it. See [`ThinkSplitter`] for why scanning the whole
//! message is not a harmless superset.

use std::collections::BTreeMap;

/// One decoded SSE frame. Comments (`: OPENROUTER PROCESSING` keep-alives) are
/// surfaced rather than silently dropped so the caller can treat them as
//  liveness without re-parsing the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    /// The joined `data:` payload of one event.
    Data(String),
    /// A `:`-prefixed comment line (OpenRouter's processing keep-alive).
    Comment(String),
}

/// Incremental SSE framer.
///
/// Holds a partial-line buffer between `push` calls so a frame split across TCP
/// reads is reassembled rather than dropped. Never reads to end: `push` returns
/// only the frames that are complete as of the bytes fed so far.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Trailing bytes that do not yet form a complete UTF-8 character. A
    /// multi-byte character split across two TCP reads would otherwise decode
    /// as U+FFFD and corrupt the payload — which for a `data:` line means the
    /// JSON no longer parses and the whole event is dropped.
    byte_buf: Vec<u8>,
    /// Bytes after the last `\n` — an incomplete line.
    line_buf: String,
    /// `data:` values collected since the last dispatching blank line.
    data_buf: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of the response body. Only the complete-UTF-8 prefix is
    /// consumed; the remainder waits for the next read.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<SseFrame> {
        self.byte_buf.extend_from_slice(bytes);
        let text = self.take_decodable();
        if text.is_empty() {
            return Vec::new();
        }
        self.push_str(&text)
    }

    /// Drain the buffer up to the last complete character. Genuinely invalid
    /// sequences are replaced and stepped over (otherwise they would wedge the
    /// decoder forever); a merely *incomplete* tail is left for the next read.
    fn take_decodable(&mut self) -> String {
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.byte_buf) {
                Ok(s) => {
                    text.push_str(s);
                    self.byte_buf.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        let head: Vec<u8> = self.byte_buf.drain(..valid).collect();
                        text.push_str(&String::from_utf8_lossy(&head));
                    }
                    match e.error_len() {
                        Some(n) => {
                            self.byte_buf.drain(..n);
                            text.push('\u{FFFD}');
                        }
                        None => break,
                    }
                }
            }
        }
        text
    }

    pub fn push_str(&mut self, chunk: &str) -> Vec<SseFrame> {
        let mut out = Vec::new();
        self.line_buf.push_str(chunk);

        // Drain whole lines only; whatever trails the final `\n` stays buffered.
        loop {
            let Some(idx) = self.line_buf.find('\n') else {
                break;
            };
            let line: String = self.line_buf.drain(..=idx).collect();
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            self.consume_line(line, &mut out);
        }
        out
    }

    /// Flush whatever a truncated stream left behind. A well-formed stream ends
    /// with a blank line, so this is normally a no-op; a server that closes
    /// mid-frame would otherwise silently lose its last event.
    pub fn finish(&mut self) -> Vec<SseFrame> {
        let mut out = Vec::new();
        if !self.byte_buf.is_empty() {
            // A truncated character at end-of-body: surface it lossily rather
            // than dropping whatever line it was part of.
            let tail = std::mem::take(&mut self.byte_buf);
            self.line_buf.push_str(&String::from_utf8_lossy(&tail));
        }
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            let line = line.trim_end_matches('\n').trim_end_matches('\r').to_string();
            self.consume_line(&line, &mut out);
        }
        self.dispatch(&mut out);
        out
    }

    fn consume_line(&mut self, line: &str, out: &mut Vec<SseFrame>) {
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if let Some(comment) = line.strip_prefix(':') {
            out.push(SseFrame::Comment(comment.trim().to_string()));
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // Exactly one leading space is part of the framing, not the payload.
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            self.data_buf.push(value.to_string());
            return;
        }
        // `event:` / `id:` / `retry:` — OpenRouter does not use them; ignore
        // rather than treat an unknown field as data.
    }

    fn dispatch(&mut self, out: &mut Vec<SseFrame>) {
        if self.data_buf.is_empty() {
            return;
        }
        out.push(SseFrame::Data(self.data_buf.join("\n")));
        self.data_buf.clear();
    }
}

/// One event decoded straight off a chunk, before thinking-normalisation and
/// tool accumulation. Kept separate from [`OrEvent`] so the JSON mapping can be
/// tested without the stateful layers on top.
#[derive(Debug, Clone, PartialEq)]
pub enum RawEvent {
    /// `delta.content` — may still contain `<think>` tags.
    Content(String),
    /// `delta.reasoning` / `delta.thinking` — already known to be reasoning.
    Reasoning(String),
    /// One fragment of an indexed tool call.
    ToolCallDelta {
        index: u32,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    Finish(String),
    /// Top-level `error` object on the chunk.
    Error(String),
    /// The `[DONE]` sentinel.
    Done,
}

/// A normalised engine event. Maps 1:1 onto the contract's engine events and,
/// in `server.rs`, onto ACP `SessionUpdate` variants.
#[derive(Debug, Clone, PartialEq)]
pub enum OrEvent {
    /// → `thinking_delta` / `SessionUpdate::AgentThoughtChunk`.
    Thinking(String),
    /// → `message_delta` / `SessionUpdate::AgentMessageChunk`.
    Text(String),
    /// → `tool_use` / `SessionUpdate::ToolCall`. Emitted whole, at finish.
    ToolUse {
        id: String,
        name: String,
        arguments: String,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
    },
    /// The raw OpenRouter `finish_reason`.
    Finish(String),
    Error(String),
    Done,
}

/// Decode one SSE `data:` payload into zero or more [`RawEvent`]s.
///
/// Returns an empty vec (never an error) for anything unparseable: OpenRouter
/// interleaves keep-alives and the occasional non-chunk object, and a decode
/// failure must not be mistaken for the end of the turn.
pub fn parse_data_frame(data: &str) -> Vec<RawEvent> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed == "[DONE]" {
        return vec![RawEvent::Done];
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Vec::new();
    };
    parse_chunk(&value)
}

/// Map a parsed chunk object onto raw events. Order matches `stream.ts`:
/// error short-circuits, then usage, then per-choice reasoning → content →
/// tool calls → finish.
pub fn parse_chunk(chunk: &serde_json::Value) -> Vec<RawEvent> {
    let mut out = Vec::new();

    if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("OpenRouter API stream error");
        out.push(RawEvent::Error(msg.to_string()));
        return out;
    }

    if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
        out.push(RawEvent::Usage {
            input_tokens: usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
            output_tokens: usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        });
    }

    let choices = chunk.get("choices").and_then(|c| c.as_array());
    for choice in choices.into_iter().flatten() {
        if let Some(delta) = choice.get("delta").filter(|d| d.is_object()) {
            // G-54, shape 1: a dedicated reasoning field. `reasoning` wins over
            // `thinking` when a provider sends both (they are the same text).
            let reasoning = delta
                .get("reasoning")
                .and_then(|v| v.as_str())
                .or_else(|| delta.get("thinking").and_then(|v| v.as_str()))
                .filter(|s| !s.is_empty());
            if let Some(text) = reasoning {
                out.push(RawEvent::Reasoning(text.to_string()));
            }

            if let Some(content) = delta
                .get("content")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                out.push(RawEvent::Content(content.to_string()));
            }

            for (pos, tc) in delta
                .get("tool_calls")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .enumerate()
            {
                // `index` is what ties fragments of one call together. When a
                // provider omits it, array position is the only stand-in.
                let index = tc
                    .get("index")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(pos as u32);
                out.push(RawEvent::ToolCallDelta {
                    index,
                    id: tc
                        .get("id")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    name: tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    arguments: tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }

        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            out.push(RawEvent::Finish(reason.to_string()));
        }
    }

    out
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Longest run of leading whitespace held while still undecided about a
/// message-opening `<think>`. A reply that opens with more blank space than
/// this is not an R1-style reasoning preamble; release it as text.
const MAX_LEADING_WHITESPACE: usize = 64;

/// Splits a *leading* `<think>…</think>` block out of content deltas
/// (G-54, shape 2).
///
/// **Position-gated on purpose.** DeepSeek-R1 and its distills open the reply
/// with the tag; nothing emits one mid-sentence. An earlier revision scanned
/// every delta for `<think>` unconditionally, which silently re-routed a
/// literal `<think>` inside an ordinary answer (a question *about* the tag, an
/// XML snippet, a prompt-engineering answer) to the thought channel — and since
/// only `OrEvent::Text` is accumulated into the assistant message, the rest of
/// the reply vanished from the bubble *and* from `session.history`. The gate:
///
/// - `armed` — true only until the turn proves what it is. Leading whitespace
///   plus `<think>` arms the splitter; anything else disarms it permanently and
///   every later byte of the turn is verbatim text.
/// - `in_think` — survives between calls, because the open and close tags
///   almost never land in the same delta. The closing tag also disarms, so a
///   second, literal `<think>` later in the same reply stays visible.
/// - `pending` — a trailing run that *could* still be part of a tag (`"<thi"`,
///   `"</thi"`), so a tag split across a chunk boundary is not emitted as
///   literal text. Released the moment the next chunk proves it was not one.
#[derive(Debug)]
pub struct ThinkSplitter {
    in_think: bool,
    armed: bool,
    pending: String,
}

impl Default for ThinkSplitter {
    fn default() -> Self {
        Self {
            in_think: false,
            armed: true,
            pending: String::new(),
        }
    }
}

impl ThinkSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn in_think(&self) -> bool {
        self.in_think
    }

    /// True while the turn could still turn out to open with `<think>`.
    pub fn armed(&self) -> bool {
        self.armed
    }

    /// Feed one content delta; returns the thinking/text split of it.
    pub fn push(&mut self, text: &str) -> Vec<OrEvent> {
        let mut out = Vec::new();
        let mut work = std::mem::take(&mut self.pending);
        work.push_str(text);
        if work.is_empty() {
            return out;
        }

        if self.armed {
            debug_assert!(!self.in_think, "armed and in_think are exclusive");
            let lead = work.trim_start();
            if lead.is_empty() {
                // Nothing but whitespace so far — undecided, unless the run has
                // grown past anything a reasoning preamble would produce.
                if work.len() <= MAX_LEADING_WHITESPACE {
                    self.pending = work;
                    return out;
                }
                self.armed = false;
                out.push(OrEvent::Text(work));
                return out;
            }
            if let Some(rest) = lead.strip_prefix(THINK_OPEN) {
                // Decided: an R1-style reasoning preamble. The leading
                // whitespace belongs to the tag, not to the answer.
                self.armed = false;
                self.in_think = true;
                work = rest.to_string();
            } else if THINK_OPEN.starts_with(lead) {
                // `"<"`, `"<thi"`, … — still undecided.
                self.pending = work;
                return out;
            } else {
                // Real content came first: this reply has no reasoning preamble,
                // and any `<think>` later in it is literal text.
                self.armed = false;
                out.push(OrEvent::Text(work));
                return out;
            }
        }

        if !self.in_think {
            out.push(OrEvent::Text(work));
            return out;
        }

        if let Some(pos) = work.find(THINK_CLOSE) {
            let head = &work[..pos];
            if !head.is_empty() {
                out.push(OrEvent::Thinking(head.to_string()));
            }
            self.in_think = false;
            let rest = &work[pos + THINK_CLOSE.len()..];
            if !rest.is_empty() {
                out.push(OrEvent::Text(rest.to_string()));
            }
            return out;
        }

        // Still inside the block; hold back a trailing partial `</think>`.
        let keep = partial_tag_suffix_len(&work, THINK_CLOSE);
        let split = work.len() - keep;
        if split > 0 {
            out.push(OrEvent::Thinking(work[..split].to_string()));
        }
        self.pending = work[split..].to_string();
        out
    }

    /// Flush held-back bytes at end of turn and reset the tag state.
    ///
    /// Resetting is the point: a stream truncated inside `<think>` (an R1 turn
    /// that hits `max_tokens` before its `</think>`) must not leave the splitter
    /// latched, or every delta after the finish marker would keep landing on the
    /// thought channel.
    pub fn finish(&mut self) -> Vec<OrEvent> {
        let rest = std::mem::take(&mut self.pending);
        let was_think = self.in_think;
        self.in_think = false;
        self.armed = false;
        if rest.is_empty() {
            return Vec::new();
        }
        if was_think {
            vec![OrEvent::Thinking(rest)]
        } else {
            vec![OrEvent::Text(rest)]
        }
    }
}

/// Length of the longest suffix of `hay` that is a proper prefix of `tag`.
///
/// `("abc<thi", "<think>") -> 4`. Zero when nothing could become a tag, which
/// is the common case and costs one scan of at most `tag.len()-1` bytes.
fn partial_tag_suffix_len(hay: &str, tag: &str) -> usize {
    let max = (tag.len() - 1).min(hay.len());
    for n in (1..=max).rev() {
        let start = hay.len() - n;
        // Only consider char boundaries — a multi-byte char can never be the
        // start of an ASCII tag anyway.
        if !hay.is_char_boundary(start) {
            continue;
        }
        if tag.as_bytes().starts_with(&hay.as_bytes()[start..]) {
            return n;
        }
    }
    0
}

/// Reassembles tool calls from indexed fragments.
///
/// OpenRouter sends the id and `function.name` on the first delta of a call and
/// then streams `function.arguments` in pieces, all keyed by `index`. The TS
/// port emitted a `tool_use` per *delta* that happened to carry both an id and
/// a name, which yields a call with truncated arguments and drops any call
/// whose id arrived on a later fragment. Accumulating and flushing once at
/// `finish_reason` is the fix.
#[derive(Debug, Default)]
pub struct ToolCallAccumulator {
    calls: BTreeMap<u32, PartialToolCall>,
}

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, index: u32, id: Option<String>, name: Option<String>, arguments: &str) {
        let entry = self.calls.entry(index).or_default();
        if let Some(id) = id {
            entry.id = Some(id);
        }
        if let Some(name) = name {
            entry.name = Some(name);
        }
        entry.arguments.push_str(arguments);
    }

    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Drain every accumulated call, in `index` order.
    ///
    /// A call with no `function.name` is dropped — there is nothing to invoke —
    /// but a missing `id` is synthesised from the index rather than dropping the
    /// call, because the id is only a correlation handle.
    pub fn flush(&mut self) -> Vec<OrEvent> {
        let calls = std::mem::take(&mut self.calls);
        calls
            .into_iter()
            .filter_map(|(index, call)| {
                let name = call.name?;
                Some(OrEvent::ToolUse {
                    id: call.id.unwrap_or_else(|| format!("openrouter-tool-{index}")),
                    name,
                    arguments: call.arguments,
                })
            })
            .collect()
    }
}

/// The whole pipeline: bytes → SSE frames → chunk events → normalised events.
///
/// One per turn. `push` is incremental and allocation-light; nothing here ever
/// waits for the body to complete.
#[derive(Debug, Default)]
pub struct OpenRouterNormalizer {
    decoder: SseDecoder,
    think: ThinkSplitter,
    tools: ToolCallAccumulator,
}

impl OpenRouterNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_bytes(&mut self, bytes: &[u8]) -> Vec<OrEvent> {
        let frames = self.decoder.push_bytes(bytes);
        self.drain_frames(frames)
    }

    pub fn push_str(&mut self, chunk: &str) -> Vec<OrEvent> {
        let frames = self.decoder.push_str(chunk);
        self.drain_frames(frames)
    }

    /// End-of-body flush: trailing partial frame, held-back `<think` bytes, and
    /// any tool call the provider never terminated with a `finish_reason`.
    pub fn finish(&mut self) -> Vec<OrEvent> {
        let frames = self.decoder.finish();
        let mut out = self.drain_frames(frames);
        out.extend(self.think.finish());
        out.extend(self.tools.flush());
        out
    }

    fn drain_frames(&mut self, frames: Vec<SseFrame>) -> Vec<OrEvent> {
        let mut out = Vec::new();
        for frame in frames {
            let SseFrame::Data(data) = frame else {
                continue; // keep-alive comment
            };
            for raw in parse_data_frame(&data) {
                match raw {
                    RawEvent::Content(text) => out.extend(self.think.push(&text)),
                    RawEvent::Reasoning(text) => out.push(OrEvent::Thinking(text)),
                    RawEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments,
                    } => self.tools.push(index, id, name, &arguments),
                    RawEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => out.push(OrEvent::Usage {
                        input_tokens,
                        output_tokens,
                    }),
                    RawEvent::Finish(reason) => {
                        // Tools first: a client that sees `finish` before the
                        // calls it is meant to run has already ended the turn.
                        out.extend(self.think.finish());
                        out.extend(self.tools.flush());
                        out.push(OrEvent::Finish(reason));
                    }
                    RawEvent::Error(msg) => out.push(OrEvent::Error(msg)),
                    RawEvent::Done => out.push(OrEvent::Done),
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── SSE framing ─────────────────────────────────────────────────────────

    #[test]
    fn decoder_emits_one_frame_per_blank_line_delimited_event() {
        let mut d = SseDecoder::new();
        let frames = d.push_str("data: {\"a\":1}\n\ndata: {\"b\":2}\n\n");
        assert_eq!(
            frames,
            vec![
                SseFrame::Data("{\"a\":1}".into()),
                SseFrame::Data("{\"b\":2}".into())
            ]
        );
    }

    #[test]
    fn decoder_reassembles_a_frame_split_across_reads() {
        // This is the property that makes the parse incremental rather than
        // read-to-end: a payload cut mid-JSON must not decode as garbage.
        let mut d = SseDecoder::new();
        assert!(d.push_str("data: {\"choi").is_empty());
        assert!(d.push_str("ces\":[]}").is_empty());
        assert_eq!(
            d.push_str("\n\n"),
            vec![SseFrame::Data("{\"choices\":[]}".into())]
        );
    }

    #[test]
    fn decoder_surfaces_keepalive_comments_and_handles_crlf() {
        let mut d = SseDecoder::new();
        let frames = d.push_str(": OPENROUTER PROCESSING\r\n\r\ndata: [DONE]\r\n\r\n");
        assert_eq!(
            frames,
            vec![
                SseFrame::Comment("OPENROUTER PROCESSING".into()),
                SseFrame::Data("[DONE]".into())
            ]
        );
    }

    #[test]
    fn decoder_reassembles_a_multibyte_character_split_across_reads() {
        // A naive `from_utf8_lossy` per read turns the two halves of "é" into
        // two U+FFFDs, which breaks the JSON and silently drops the event.
        let payload = "data: {\"t\":\"é\"}\n\n";
        let bytes = payload.as_bytes();
        let split = payload.find('é').expect("é present") + 1;
        let mut d = SseDecoder::new();
        assert!(d.push_bytes(&bytes[..split]).is_empty());
        assert_eq!(
            d.push_bytes(&bytes[split..]),
            vec![SseFrame::Data("{\"t\":\"é\"}".into())]
        );
    }

    #[test]
    fn decoder_joins_multiline_data_and_flushes_an_unterminated_tail() {
        let mut d = SseDecoder::new();
        assert!(d.push_str("data: one\ndata: two\n").is_empty());
        assert_eq!(d.finish(), vec![SseFrame::Data("one\ntwo".into())]);
    }

    // ── chunk → raw events ──────────────────────────────────────────────────

    #[test]
    fn content_delta_becomes_a_content_event() {
        assert_eq!(
            parse_data_frame(
                r#"{"choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#
            ),
            vec![RawEvent::Content("Hello".into())]
        );
    }

    #[test]
    fn reasoning_and_thinking_fields_both_map_to_reasoning() {
        assert_eq!(
            parse_data_frame(r#"{"choices":[{"index":0,"delta":{"reasoning":"hmm"}}]}"#),
            vec![RawEvent::Reasoning("hmm".into())]
        );
        assert_eq!(
            parse_data_frame(r#"{"choices":[{"index":0,"delta":{"thinking":"hmm2"}}]}"#),
            vec![RawEvent::Reasoning("hmm2".into())]
        );
    }

    #[test]
    fn null_and_empty_deltas_produce_nothing() {
        assert!(parse_data_frame(r#"{"choices":[{"index":0,"delta":{"content":null}}]}"#).is_empty());
        assert!(parse_data_frame(r#"{"choices":[{"index":0,"delta":{"content":""}}]}"#).is_empty());
        assert!(parse_data_frame("not json at all").is_empty());
        assert!(parse_data_frame("   ").is_empty());
    }

    #[test]
    fn done_sentinel_and_error_object_are_recognised() {
        assert_eq!(parse_data_frame("[DONE]"), vec![RawEvent::Done]);
        assert_eq!(
            parse_data_frame(r#"{"error":{"message":"rate limited","code":429}}"#),
            vec![RawEvent::Error("rate limited".into())]
        );
    }

    #[test]
    fn usage_maps_prompt_and_completion_tokens() {
        assert_eq!(
            parse_data_frame(r#"{"usage":{"prompt_tokens":12,"completion_tokens":34},"choices":[]}"#),
            vec![RawEvent::Usage {
                input_tokens: 12,
                output_tokens: 34
            }]
        );
    }

    // ── thinking normaliser (G-54) ──────────────────────────────────────────

    #[test]
    fn think_tags_in_one_delta_split_into_thinking_and_text() {
        let mut s = ThinkSplitter::new();
        assert_eq!(
            s.push("<think>weighing it</think>Answer."),
            vec![
                OrEvent::Thinking("weighing it".into()),
                OrEvent::Text("Answer.".into())
            ]
        );
        assert!(!s.in_think());
    }

    #[test]
    fn think_state_carries_across_chunk_boundaries() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<think>step "), vec![OrEvent::Thinking("step ".into())]);
        assert!(s.in_think());
        assert_eq!(s.push("one"), vec![OrEvent::Thinking("one".into())]);
        assert_eq!(s.push("</think>done"), vec![OrEvent::Text("done".into())]);
        assert!(!s.in_think());
    }

    #[test]
    fn an_opening_tag_split_mid_token_is_never_emitted_as_literal_text() {
        // The regression this guards: naive per-chunk matching emits "<thi" as
        // visible assistant output and then never recognises the open tag.
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<thi"), Vec::new());
        assert_eq!(s.push("nk>deep"), vec![OrEvent::Thinking("deep".into())]);
        assert_eq!(s.push("</thi"), Vec::new());
        assert_eq!(s.push("nk>out"), vec![OrEvent::Text("out".into())]);
    }

    #[test]
    fn leading_whitespace_before_the_tag_still_arms_the_splitter() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("\n "), Vec::new());
        assert_eq!(
            s.push("<think>mm</think>hi"),
            vec![OrEvent::Thinking("mm".into()), OrEvent::Text("hi".into())]
        );
    }

    #[test]
    fn a_literal_think_tag_inside_an_ordinary_answer_stays_visible_text() {
        // The whole point of the position gate. Before it, this reply lost
        // everything after "<think>" from both the bubble and the history that
        // is resent on the next turn.
        let mut s = ThinkSplitter::new();
        assert_eq!(
            s.push("DeepSeek emits a <think> tag. "),
            vec![OrEvent::Text("DeepSeek emits a <think> tag. ".into())]
        );
        assert_eq!(
            s.push("It marks reasoning."),
            vec![OrEvent::Text("It marks reasoning.".into())]
        );
        assert!(!s.in_think(), "a mid-message tag must not latch think mode");
        assert!(!s.armed());
    }

    #[test]
    fn a_tag_after_the_reasoning_block_closed_is_also_literal() {
        let mut s = ThinkSplitter::new();
        assert_eq!(
            s.push("<think>plan</think>Use <think> like this."),
            vec![
                OrEvent::Thinking("plan".into()),
                OrEvent::Text("Use <think> like this.".into())
            ]
        );
    }

    #[test]
    fn a_held_back_partial_tag_is_released_as_text_when_it_turns_out_not_to_be_one() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<t"), Vec::new());
        assert_eq!(s.push("able>"), vec![OrEvent::Text("<table>".into())]);
    }

    #[test]
    fn finish_flushes_a_truncated_partial_tag_as_text() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<thin"), Vec::new());
        assert_eq!(s.finish(), vec![OrEvent::Text("<thin".into())]);
        assert_eq!(s.finish(), Vec::new());
    }

    #[test]
    fn finish_resets_think_mode_so_a_truncated_block_cannot_latch() {
        // An R1 turn that hits max_tokens mid-reasoning never delivers its
        // `</think>`. Before the reset, every later delta — including the next
        // turn's, on a reused splitter — kept landing on the thought channel.
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("<think>a"), vec![OrEvent::Thinking("a".into())]);
        assert!(s.in_think());
        assert_eq!(s.finish(), Vec::new());
        assert!(!s.in_think());
        assert_eq!(s.push("after"), vec![OrEvent::Text("after".into())]);
    }

    #[test]
    fn a_long_whitespace_run_disarms_rather_than_buffering_forever() {
        let mut s = ThinkSplitter::new();
        let ws = " ".repeat(MAX_LEADING_WHITESPACE + 1);
        assert_eq!(s.push(&ws), vec![OrEvent::Text(ws.clone())]);
        assert!(!s.armed());
    }

    #[test]
    fn multibyte_content_is_not_split_mid_character() {
        let mut s = ThinkSplitter::new();
        assert_eq!(s.push("héllo — ✓"), vec![OrEvent::Text("héllo — ✓".into())]);
    }

    // ── tool-call accumulation ──────────────────────────────────────────────

    #[test]
    fn indexed_tool_call_fragments_are_reassembled_in_order() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(0, Some("call_a".into()), Some("search".into()), "{\"q\":");
        acc.push(0, None, None, "\"beats\"}");
        acc.push(1, Some("call_b".into()), Some("render".into()), "{}");
        assert_eq!(
            acc.flush(),
            vec![
                OrEvent::ToolUse {
                    id: "call_a".into(),
                    name: "search".into(),
                    arguments: "{\"q\":\"beats\"}".into()
                },
                OrEvent::ToolUse {
                    id: "call_b".into(),
                    name: "render".into(),
                    arguments: "{}".into()
                }
            ]
        );
        assert!(acc.is_empty(), "flush drains");
    }

    #[test]
    fn a_call_missing_its_id_still_survives_but_one_missing_its_name_does_not() {
        let mut acc = ToolCallAccumulator::new();
        acc.push(0, None, Some("search".into()), "{}");
        acc.push(1, Some("call_x".into()), None, "{}");
        assert_eq!(
            acc.flush(),
            vec![OrEvent::ToolUse {
                id: "openrouter-tool-0".into(),
                name: "search".into(),
                arguments: "{}".into()
            }]
        );
    }

    // ── end-to-end over recorded bytes ──────────────────────────────────────

    /// A recorded DeepSeek-R1-shaped stream: inline `<think>` tags, split
    /// across SSE frames, then content, then `[DONE]`.
    const RECORDED_INLINE_THINK: &str = concat!(
        ": OPENROUTER PROCESSING\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"<thi\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"nk>let me check</think>\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"42\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    /// A recorded Claude-thinking-shaped stream: `delta.reasoning`, an indexed
    /// tool call streamed in fragments, usage, then `tool_calls` finish.
    const RECORDED_REASONING_AND_TOOLS: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"planning\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Lagos\\\"}\"}}]}}]}\n\n",
        "data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9},\"choices\":[]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    /// Feed a recorded stream one byte at a time. Byte-at-a-time is the harshest
    /// possible framing — if the pipeline is incremental it produces exactly the
    /// same events as a single push.
    fn drip(recorded: &str) -> Vec<OrEvent> {
        let mut n = OpenRouterNormalizer::new();
        let mut out = Vec::new();
        for b in recorded.as_bytes() {
            out.extend(n.push_bytes(&[*b]));
        }
        out.extend(n.finish());
        out
    }

    #[test]
    fn recorded_inline_think_stream_normalises_to_thinking_then_text() {
        let mut n = OpenRouterNormalizer::new();
        let mut events = n.push_str(RECORDED_INLINE_THINK);
        events.extend(n.finish());
        assert_eq!(
            events,
            vec![
                OrEvent::Thinking("let me check".into()),
                OrEvent::Text("42".into()),
                OrEvent::Finish("stop".into()),
                OrEvent::Done,
            ]
        );
    }

    #[test]
    fn recorded_reasoning_and_tool_stream_normalises_to_one_complete_tool_use() {
        let mut n = OpenRouterNormalizer::new();
        let mut events = n.push_str(RECORDED_REASONING_AND_TOOLS);
        events.extend(n.finish());
        assert_eq!(
            events,
            vec![
                OrEvent::Thinking("planning".into()),
                OrEvent::Usage {
                    input_tokens: 7,
                    output_tokens: 9
                },
                OrEvent::ToolUse {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: "{\"city\":\"Lagos\"}".into()
                },
                OrEvent::Finish("tool_calls".into()),
                OrEvent::Done,
            ]
        );
    }

    #[test]
    fn byte_at_a_time_delivery_yields_identical_events() {
        for recorded in [
            RECORDED_INLINE_THINK,
            RECORDED_REASONING_AND_TOOLS,
            RECORDED_LITERAL_THINK_MENTION,
        ] {
            let mut whole = OpenRouterNormalizer::new();
            let mut expected = whole.push_str(recorded);
            expected.extend(whole.finish());
            assert_eq!(drip(recorded), expected, "chunking must not change events");
        }
    }

    /// A reply that merely *mentions* the tag. Every byte must reach the
    /// message channel, because only `OrEvent::Text` is written back to
    /// `OpenRouterSession::history`.
    const RECORDED_LITERAL_THINK_MENTION: &str = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"DeepSeek emits a <think> tag. \"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"It marks reasoning.\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    #[test]
    fn a_reply_that_mentions_the_think_tag_keeps_every_byte_as_message_text() {
        let mut n = OpenRouterNormalizer::new();
        let mut events = n.push_str(RECORDED_LITERAL_THINK_MENTION);
        events.extend(n.finish());
        assert_eq!(
            events,
            vec![
                OrEvent::Text("DeepSeek emits a <think> tag. ".into()),
                OrEvent::Text("It marks reasoning.".into()),
                OrEvent::Finish("stop".into()),
                OrEvent::Done,
            ]
        );
        let text: String = events
            .iter()
            .filter_map(|e| match e {
                OrEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "DeepSeek emits a <think> tag. It marks reasoning.");
    }

    #[test]
    fn a_mid_stream_error_object_surfaces_as_an_error_event() {
        let mut n = OpenRouterNormalizer::new();
        let events = n.push_str(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n\
             data: {\"error\":{\"message\":\"upstream 502\"}}\n\n",
        );
        assert_eq!(
            events,
            vec![
                OrEvent::Text("partial".into()),
                OrEvent::Error("upstream 502".into())
            ]
        );
    }
}
