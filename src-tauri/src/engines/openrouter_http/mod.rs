//! OpenRouter HTTP engine adapter — WP-20 (G-55 / G-ENGINE-KEY).
//!
//! Every other adapter under `engines/` wraps a **local CLI** that
//! authenticates itself (`claude login`, `codex login`, …). OpenRouter has no
//! CLI: the shell itself is the API client, so this is the first adapter that
//! has to *hold a credential*. That is the whole reason the WP-20a spike
//! existed, and the mechanism it locked is implemented here:
//!
//! 1. The key **name** is read from the installed engine pkg's
//!    `engine.onboarding.requiredVaultKeys[0]`
//!    (`com.ikenga.engine-openrouter`) — the first runtime consumer of a field
//!    that was declarative-only until now. Falls back to
//!    [`DEFAULT_VAULT_KEY`] when the pkg is not installed.
//! 2. The key **value** is resolved *in-process, at prompt time* via
//!    `commands::secrets::read_secret_scoped` — `Scope::Workspace` first (which
//!    carries `read_secret_scoped`'s own legacy-unscoped fallback), then
//!    `Scope::pkg("com.ikenga.engine-openrouter")`. Same precedence as the
//!    ADR-017 `pkg_fetch` credential path, and like that path it runs inside
//!    `spawn_blocking` — `SecretsLock`'s `std::sync::Mutex` is only sound
//!    because no one holds it across an `.await`.
//! 3. No child process is spawned, so there is no env handoff: the key exists
//!    only inside this Rust process, only for the duration of the request, and
//!    is never logged. It is a strictly smaller blast radius than the F-9
//!    settings-secret-env path used for sidecars.
//! 4. The key only leaves the process over TLS, to a host the pkg's
//!    `permissions.net` allowlist names. `base_url` is a settings string
//!    anything with `pkg_settings_set` can rewrite, so it is validated through
//!    the same `pkg::http_proxy` policy core `pkg_fetch` uses
//!    (`server::validate_endpoint`) before a request is built.
//!
//! `model` and `base_url` come from `pkg_settings` for the same pkg id (the
//! manifest's `settings.schema` supplies the declared defaults;
//! [`DEFAULT_BASE_URL`] / [`DEFAULT_MODEL`] are the last resort). Nothing about
//! the endpoint or the credential is hardcoded beyond the public base URL and
//! the allowlist that scopes it.
//!
//! ## Layout
//!
//! - [`sse`] — the pure, network-free half: an incremental SSE frame decoder, a
//!   chunk → event mapper, the `<think>`-tag / `delta.reasoning` thinking
//!   normaliser (G-54), and an indexed tool-call accumulator. Ported from
//!   `ikenga-pkgs/packages/engine/openrouter/src/stream.ts`, which is the
//!   porting spec. All unit tests live there and feed recorded bytes.
//! - [`server`] — `OpenRouterHttpEngine`: session state, config/key resolution,
//!   the `reqwest` streaming request, and the ACP method surface
//!   (`handle_initialize` / `handle_new_session` / `handle_prompt` /
//!   `handle_cancel` / …) that `EngineHandle::OpenRouterHttp` dispatches into.
//!
//! ## Build gating
//!
//! Desktop-only, like every adapter except Antigravity. Config + key
//! resolution both need an `AppHandle` (Stronghold vault + `pkg_settings`),
//! and `server/chat_ws.rs`'s headless match is exhaustive over
//! `EngineHandle::Antigravity` plus a `#[cfg(feature = "desktop")]` catch-all —
//! an ungated variant would make the daemon crate non-exhaustive. The streaming
//! core in [`sse`] carries no Tauri types, so lifting this into the daemon later
//! is a gating change, not a rewrite.

pub mod server;
pub mod sse;

pub use server::{OpenRouterHttpEngine, OpenRouterHttpEngineState};
