//! Codex CLI engine adapter.
//!
//! ADR-013 Phase 3: codex CLI doesn't speak ACP natively (no `--acp` flag,
//! no `acp` subcommand). Instead, this engine drives `codex exec --json`
//! one-shot per turn, parses the line-delimited JSON event stream
//! (`thread.started` → `turn.started` → `item.*` → `turn.completed`), and
//! emits ACP-shaped `SessionUpdate` envelopes on the same
//! `chat://session/{thread_id}` Tauri channel Claude and Gemini use. From
//! the FE's perspective the wire is uniform.
//!
//! Resume across turns is handled by capturing the `thread_id` codex
//! returns on the first `thread.started` event and feeding it back via
//! `codex exec resume <id>` on the next prompt. Context lives on disk in
//! codex's session store; we just track the id.
//!
//! ### Why the module is still called `codex_pty`
//!
//! The original Phase 3 scaffold wrapped codex in a PTY because the only
//! mode that existed was the TUI. `codex exec --json` is a structured
//! non-interactive mode that landed later. The module name is misleading
//! but the rename is churn for zero behavioural change; deferred to a
//! future ADR. See ADR-013 §6 ("Negative: keeps a misleading suffix") for
//! the rationale.
//!
//! ### What's NOT in scope
//!
//! - Interactive permission round-trips. Codex exec uses its own
//!   `--sandbox` policy; we don't bridge approval prompts to the FE.
//! - Image input. The exec surface is text-only.
//! - Per-turn model / effort switching. Codex reads those from its own
//!   config; the chat header still stages the values but they no-op here.

// The chat adapter emits on Tauri channels, so it is desktop-only. The JSONL
// `parser` is pure (serde + ACP schema types) and compiles into the headless
// daemon too, which reads Chi runs' codex output with it (WP-P10).
#[cfg(feature = "desktop")]
pub mod engine;
pub mod parser;

#[cfg(feature = "desktop")]
pub use engine::{CodexPtyEngine, CodexPtyEngineState};
