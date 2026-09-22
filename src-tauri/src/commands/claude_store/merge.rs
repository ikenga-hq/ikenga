//! Settings-JSON merge engine (WP-03) — pure library for splicing a single
//! keyed JSON block into / out of Claude Code's config files while preserving
//! every unrelated key.
//!
//! Two block families:
//!   - **hooks**     → live under the top-level `hooks` object in
//!                     `settings.json` / `settings.local.json`. The block key
//!                     is the hook *event* name (e.g. `PreToolUse`); the value
//!                     is the per-event array of matcher groups Claude Code
//!                     expects.
//!   - **mcpServers** → live under the top-level `mcpServers` object in
//!                     `<root>/.mcp.json` (project scope) or `~/.claude.json`
//!                     (user scope). The block key is the server name.
//!
//! Mechanic (every mutation): **read whole object → splice the single child
//! key under the fixed parent key → serialize → temp-file in the same dir →
//! atomic rename.** Every unrelated top-level key is carried through the
//! `serde_json::Map` untouched, so its content is byte-preserved. (`serde_json`
//! is compiled without `preserve_order` in this crate, so the map is a
//! `BTreeMap`: serialization is deterministic and a round-trip
//! enable→disable returns the file to byte-identical state.)
//!
//! **`~/.claude.json` is never rewritten wholesale** beyond this read→splice
//! `mcpServers`→write cycle — its OAuth / session keys are read in, left
//! untouched in the map, and written back verbatim. We only ever mutate the
//! `mcpServers` key.
//!
//! Pure functions only — no Tauri commands, no shared state. The store layer
//! (WP-02, `claude_store.rs`) resolves a `scope` string to a concrete project
//! `root_path` (an async DB lookup it owns) and calls these with the resolved
//! directory; this module stays synchronous and side-effect-bounded to the
//! one target file.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use crate::commands::engine_layout::{
    engine_layout_by_id, ConfigFormat, EngineId, KindLayout, PrimitiveKind,
};

// ─── Typed write errors (G-WRITE) ─────────────────────────────────────────────
//
// The Phase-1 JSON path returns `anyhow::Result` (untyped strings). The v2b
// settings-embedded write engine adds a *typed* error so callers (WP-23/24/25/26)
// can branch on the failure mode — most importantly the Gemini strict-key
// rejection, which must be a refusal-before-write, never a write-and-fail.

/// Typed failure modes of the settings-embedded write engine (JSON + TOML).
/// Serializable so it can cross the Tauri boundary as a structured error.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[allow(dead_code)]
pub enum StoreError {
    /// A Gemini-style strict (`additionalProperties:false`) settings file would
    /// reject this key — refused *before* any write so the file is never left
    /// in a state the engine fails to load. `engine` is the stable engine id
    /// (`gemini`); `key` is the rejected top-level settings key.
    StrictKeyRejected { engine: String, key: String },
    /// The backing parent (`mcpServers` / `hooks` / `mcp_servers`) exists but is
    /// not an object/table — refusing to clobber a load-bearing scalar.
    NonTableParent { path: String, key: String },
    /// The target file failed to parse as its declared format.
    Parse { path: String, message: String },
    /// A value in the spliced block has no representation in the target format
    /// (e.g. JSON `null` → TOML). A typed error rather than a silent drop.
    UnrepresentableValue { path: String, message: String },
    /// Filesystem error (read / write / rename / mkdir).
    Io { path: String, message: String },
    /// Scope grammar / resolution error, or an unsupported (engine, kind) cell.
    Unsupported { message: String },
    /// A cross-engine copy/move direction has no transcode path (WP-24). The
    /// only blocked directions in v2b are TOML→MD (Codex agent → Claude/Gemini,
    /// Gemini command → Claude command) — there is no reverse transcoder. This is
    /// a refusal *before* any disk write, never a write-and-fail, so a blocked
    /// destination never leaves a partial file. `from`/`to` are the stable engine
    /// ids; `reason` is the human message the FE renders in the greyed/tooltip
    /// state. (`plans/cockpit/06-cross-engine-transcode.md`.)
    TranscodeUnsupported {
        from: String,
        to: String,
        reason: String,
    },
    /// The settings entry at `key` is not exactly what the store placed there:
    /// it is the user's own entry, another store item's, or a store entry they
    /// have edited since it was enabled. Refused *before* any write, so it is
    /// never silently overwritten or deleted.
    NotOurs {
        path: String,
        key: String,
        reason: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::StrictKeyRejected { engine, key } => write!(
                f,
                "{engine} settings.json is strict (additionalProperties:false) and would reject key `{key}`"
            ),
            StoreError::NonTableParent { path, key } => {
                write!(f, "{path}: `{key}` is not an object/table — refusing to overwrite")
            }
            StoreError::Parse { path, message } => write!(f, "parse {path}: {message}"),
            StoreError::UnrepresentableValue { path, message } => {
                write!(f, "{path}: {message}")
            }
            StoreError::Io { path, message } => write!(f, "{path}: {message}"),
            StoreError::Unsupported { message } => write!(f, "{message}"),
            StoreError::TranscodeUnsupported { from, to, reason } => {
                write!(f, "no {from}→{to} transcode: {reason}")
            }
            StoreError::NotOurs { path, key, reason } => {
                write!(f, "refusing to change `{key}` in {path}: {reason}")
            }
        }
    }
}

impl std::error::Error for StoreError {}

/// Bridge an `anyhow::Error` from the Phase-1 JSON path into the typed error so
/// the engine-aware dispatch presents one error type. The JSON path's own
/// messages already carry path + cause, so we wrap them as `Io` (the catch-all
/// for "something went wrong touching this file") unless they were our own
/// non-object refusal, which we surface structurally.
#[allow(dead_code)]
fn from_anyhow(path: &Path, e: anyhow::Error) -> StoreError {
    let msg = e.to_string();
    if msg.contains("is not an object") {
        StoreError::NonTableParent {
            path: path.to_string_lossy().to_string(),
            // The JSON splice only ever refuses these two parents.
            key: if msg.contains("mcpServers") {
                "mcpServers".to_string()
            } else {
                "hooks".to_string()
            },
        }
    } else {
        StoreError::Io {
            path: path.to_string_lossy().to_string(),
            message: msg,
        }
    }
}

/// Which on-disk settings file a hook block lands in. Mirrors the two project
/// settings files Claude Code reads (`settings.local.json` shadows
/// `settings.json`); user-scope hooks land in `~/.claude/settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFile {
    /// `settings.json` (shared / committed).
    Shared,
    /// `settings.local.json` (machine-local, git-ignored).
    Local,
}

// ─── Scope grammar ──────────────────────────────────────────────────────────
//
// Reuses the pin layer's grammar: `'workspace' | 'project:<id>'`. Kept as a
// local copy because the canonical `validate_pin_scope` in `claude_config.rs`
// is private; the grammar is frozen by the G-CONTRACT so duplicating it here
// is safe and keeps this module dependency-light. The orchestrator may swap
// this for a shared import at integration with no behavioural change.

/// Validate a scope string against the frozen grammar `workspace |
/// project:<id>` and return the resolved kind. `<id>` must be a non-empty,
/// ≤64-char `[a-z0-9][a-z0-9_-]*`.
pub fn validate_scope(scope: &str) -> Result<ScopeKind> {
    if scope == "workspace" {
        return Ok(ScopeKind::Workspace);
    }
    if let Some(id) = scope.strip_prefix("project:") {
        if id.is_empty() || id.len() > 64 {
            return Err(anyhow!("invalid project id length in scope {scope:?}"));
        }
        let mut chars = id.chars();
        let first = chars.next().unwrap();
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return Err(anyhow!("invalid project id in scope {scope:?}"));
        }
        for c in chars {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
                return Err(anyhow!("invalid project id in scope {scope:?}"));
            }
        }
        return Ok(ScopeKind::Project(id.to_string()));
    }
    Err(anyhow!(
        "scope must be 'workspace' or 'project:<id>', got {scope:?}"
    ))
}

/// Resolved scope kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeKind {
    /// User / workspace-wide scope — targets `~/.claude/...` and `~/.claude.json`.
    Workspace,
    /// A specific project by id — targets files under the project's `root_path`.
    Project(String),
}

// ─── Public API — hooks ──────────────────────────────────────────────────────

/// Enable (add or overwrite) a hook block keyed by `event` in the settings
/// file for `scope`. `project_root` is the project's resolved `root_path`,
/// required for `project:<id>` scopes and ignored for `workspace`. `block` is
/// the value placed at `hooks.<event>` (typically a JSON array of matcher
/// groups). All other keys in the file are preserved.
pub fn enable_hook(
    scope: &str,
    project_root: Option<&Path>,
    file: HookFile,
    event: &str,
    block: Value,
) -> Result<()> {
    let path = hook_path(scope, project_root, file)?;
    // `hooks.<event>` holds every hook on that event, so replacing it would wipe
    // the user's own hooks (see the guard note).
    guard_json(&path, "hooks", event, &block, false)?;
    splice_nested(&path, "hooks", event, Some(block))
}

/// Disable (remove) the hook block keyed by `event` from the settings file for
/// `scope`. Removing the last hook leaves an empty `hooks: {}` object in place
/// (so the parent key's presence is stable); all other keys are preserved. A
/// missing file or missing key is a no-op.
pub fn disable_hook(
    scope: &str,
    project_root: Option<&Path>,
    file: HookFile,
    event: &str,
    store_block: &Value,
) -> Result<()> {
    let path = hook_path(scope, project_root, file)?;
    // Removes the whole event, so only when it holds exactly the store's block.
    guard_json(&path, "hooks", event, store_block, true)?;
    splice_nested(&path, "hooks", event, None)
}

// ─── Ownership guard ──────────────────────────────────────────────────────────
//
// Every splice below writes or removes one keyed entry (`mcpServers.<name>`,
// `hooks.<event>`) in a file the user also edits by hand. The splice itself
// replaces or deletes whatever is at that key, so without a guard:
//   - disabling an MCP server the user wrote themselves deleted it, env and
//     API keys included, with nothing in the store to bring it back;
//   - enabling a store hook on an event (`PreToolUse`, say) replaced *every*
//     hook already on that event, including the user's own and security hooks;
//   - enabling a store MCP server replaced a user's own server of that name.
//
// The rule: a write may only replace or remove an entry that is **exactly**
// what the store puts there. Enable accepts an absent entry or one already
// equal to the store block (idempotent). Disable accepts an absent entry
// (no-op) or one equal to the store block. Anything else is someone's data and
// is refused with `NotOurs`. That includes a store entry the user has edited
// since enabling it (a pasted-in API key, say): disabling it would silently
// discard the edit.
//
// Consequence: two store hooks can't share an event. That case was already
// broken (the second enable overwrote the first); now it is refused out loud.

/// The current value at `parent.child` in a JSON settings file, if any.
fn current_nested(path: &Path, parent_key: &str, child_key: &str) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let root = read_object(path)?;
    Ok(root
        .get(parent_key)
        .and_then(Value::as_object)
        .and_then(|o| o.get(child_key))
        .cloned())
}

/// Why a splice of `store` at `parent.child` would destroy data that is not
/// the store's, or `None` when it is safe.
pub(super) fn ownership_conflict_reason(removing: bool) -> String {
    if removing {
        "it is not exactly what the store placed there. It is your own entry, or \
         one you have edited since it was enabled, and disabling would delete it."
            .to_string()
    } else {
        "something different is already there: your own entry, or another store \
         item's. Enabling would overwrite it."
            .to_string()
    }
}

/// The JSON ownership check. `Ok(None)` means safe to splice.
fn json_ownership_conflict(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    store: &Value,
    removing: bool,
) -> Result<Option<String>> {
    Ok(match current_nested(path, parent_key, child_key)? {
        None => None,
        Some(current) if current == *store => None,
        Some(_) => Some(ownership_conflict_reason(removing)),
    })
}

/// The JSON ownership check, as an `anyhow` error for the Phase-1 API.
fn guard_json(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    store: &Value,
    removing: bool,
) -> Result<()> {
    match json_ownership_conflict(path, parent_key, child_key, store, removing)? {
        None => Ok(()),
        Some(reason) => Err(anyhow!(
            "refusing to change `{parent_key}.{child_key}` in {}: {reason}",
            path.display()
        )),
    }
}

/// The JSON ownership check, as a typed `NotOurs` for the engine-aware API.
fn guard_json_typed(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    store: &Value,
    removing: bool,
) -> Result<(), StoreError> {
    match json_ownership_conflict(path, parent_key, child_key, store, removing)
        .map_err(|e| from_anyhow(path, e))?
    {
        None => Ok(()),
        Some(reason) => Err(StoreError::NotOurs {
            path: path.to_string_lossy().to_string(),
            key: format!("{parent_key}.{child_key}"),
            reason,
        }),
    }
}

// ─── Public API — mcpServers ──────────────────────────────────────────────────

/// Enable (add or overwrite) an MCP server definition keyed by `name`.
///   - `workspace` scope → `~/.claude.json` (only the `mcpServers` key is
///     touched; OAuth / session keys are preserved verbatim).
///   - `project:<id>` scope → `<root>/.mcp.json`.
/// `server_def` is the value placed at `mcpServers.<name>`.
pub fn enable_mcp(
    scope: &str,
    project_root: Option<&Path>,
    name: &str,
    server_def: Value,
) -> Result<()> {
    let path = mcp_path(scope, project_root)?;
    // Never replace a different server of the same name (see the guard note).
    guard_json(&path, "mcpServers", name, &server_def, false)?;
    splice_nested(&path, "mcpServers", name, Some(server_def))
}

/// Disable (remove) the MCP server keyed by `name`. Removing the last server
/// leaves an empty `mcpServers: {}` object; every other key — crucially the
/// `~/.claude.json` session state — is preserved. A missing file or missing
/// key is a no-op.
///
/// `store_def` is the store's definition for `name`. The entry is removed only
/// if it is exactly that; a user's own server, or an edited one, is refused.
pub fn disable_mcp(
    scope: &str,
    project_root: Option<&Path>,
    name: &str,
    store_def: &Value,
) -> Result<()> {
    let path = mcp_path(scope, project_root)?;
    guard_json(&path, "mcpServers", name, store_def, true)?;
    splice_nested(&path, "mcpServers", name, None)
}

// ─── Engine-aware dispatch (G-WRITE) ──────────────────────────────────────────
//
// The Phase-1 functions above are Claude-only (JSON, fixed paths). The v2b
// write engine threads `EngineId` + the frozen `EngineLayout` so a single
// per-engine command can target the right file in the right format:
//   - Claude / Gemini hooks  → JSON `settings{,.local}.json#hooks`
//   - Claude MCP             → JSON `.mcp.json` / `~/.claude.json#mcpServers`
//   - Gemini MCP             → JSON `~/.gemini/settings.json#mcpServers` (STRICT)
//   - Codex hooks            → JSON `~/.codex/hooks.json` (standalone JSON file)
//                              *or* inline TOML `config.toml#hooks`
//   - Codex MCP              → TOML `~/.codex/config.toml#mcp_servers`
//
// All paths are per-engine confined (resolved from the layout's `location`
// template under the engine's own root); the strict-key guard runs *before*
// any write for engines whose backing settings file is strict (Gemini).

/// The on-disk target + format for a settings-embedded write, resolved from the
/// frozen `EngineLayout` for one (engine, kind, scope).
#[allow(dead_code)]
struct WriteTarget {
    path: PathBuf,
    format: ConfigFormat,
    /// The strict-key flag from the layout cell — drives the Gemini guard.
    strict_keys: bool,
    /// Engine id string (for typed-error reporting).
    engine: String,
}

/// Look up the `KindLayout` cell for an (engine, kind), erroring if absent.
#[allow(dead_code)]
fn layout_cell(engine: EngineId, kind: PrimitiveKind) -> Result<KindLayout, StoreError> {
    let layout = engine_layout_by_id(engine).ok_or_else(|| StoreError::Unsupported {
        message: format!("no layout for engine {engine:?}"),
    })?;
    layout
        .kinds
        .get(&kind)
        .cloned()
        .ok_or_else(|| StoreError::Unsupported {
            message: format!("engine {engine:?} has no {kind:?} cell"),
        })
}

/// Resolve the settings-embedded write target for hooks. `hook_file` selects
/// shared vs local for the JSON engines (Claude/Gemini); it is ignored for
/// Codex (single standalone `hooks.json` / inline `config.toml`).
#[allow(dead_code)]
fn hook_target(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    hook_file: HookFile,
) -> Result<WriteTarget, StoreError> {
    let cell = layout_cell(engine, PrimitiveKind::Hook)?;
    let path = match engine {
        // Claude + Gemini hooks live in the engine's `.claude`/`.gemini`-style
        // settings file as a JSON `#hooks` key. Claude uses `.claude/`; Gemini
        // uses `~/.gemini/settings.json` (user-only) — but the v2b interview
        // scopes hook writes to the JSON `settings{,.local}.json` shape the
        // Phase-1 path already produces for Claude. We reuse `claude_dir` for
        // Claude; Gemini's settings file is its user `settings.json`.
        EngineId::Claude => hook_path(scope, project_root, hook_file).map_err(map_path_err)?,
        EngineId::Gemini => gemini_settings_path()?,
        EngineId::Antigravity => antigravity_config_path()?,
        // Codex hooks: standalone `~/.codex/hooks.json` (JSON) per the layout
        // `location`. (Inline `config.toml#hooks` is the alternate home handled
        // by the TOML engine; the layout's canonical write target is hooks.json.)
        EngineId::Codex => codex_dir()?.join("hooks.json"),
    };
    Ok(WriteTarget {
        path,
        format: cell.format,
        strict_keys: cell.strict_keys,
        engine: engine_id_str(engine).to_string(),
    })
}

/// Resolve the settings-embedded write target for MCP.
#[allow(dead_code)]
fn mcp_target(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
) -> Result<WriteTarget, StoreError> {
    let cell = layout_cell(engine, PrimitiveKind::Mcp)?;
    let path = match engine {
        // Claude MCP: standalone `.mcp.json` (project) / `~/.claude.json` (user).
        EngineId::Claude => mcp_path(scope, project_root).map_err(map_path_err)?,
        EngineId::Gemini => gemini_settings_path()?,
        EngineId::Antigravity => antigravity_config_path()?,
        // Codex MCP: `~/.codex/config.toml#mcp_servers` (TOML, lenient).
        EngineId::Codex => codex_dir()?.join("config.toml"),
    };
    Ok(WriteTarget {
        path,
        format: cell.format,
        strict_keys: cell.strict_keys,
        engine: engine_id_str(engine).to_string(),
    })
}

/// The strict-key guard. For a strict (`additionalProperties:false`) backing
/// settings file, refuse to write a top-level parent key the engine's schema
/// would reject — **before** touching the file. Gemini recognizes `mcpServers`
/// and `hooks` as valid top-level keys, so those pass; any other key is refused.
/// Lenient engines (Claude/Codex) never refuse here.
#[allow(dead_code)]
fn strict_key_guard(target: &WriteTarget, parent_key: &str) -> Result<(), StoreError> {
    if !target.strict_keys {
        return Ok(());
    }
    // The frozen set of top-level keys Gemini's strict settings.json accepts for
    // the primitives Ngwa writes. (Both are recognized; the guard exists to
    // refuse a *future* mis-routed key before it corrupts the strict file.)
    const ALLOWED_STRICT_TOP_KEYS: &[&str] = &["mcpServers", "hooks"];
    if ALLOWED_STRICT_TOP_KEYS.contains(&parent_key) {
        Ok(())
    } else {
        Err(StoreError::StrictKeyRejected {
            engine: target.engine.clone(),
            key: parent_key.to_string(),
        })
    }
}

/// Enable a hook block keyed by `event` for `(engine, scope)`. Routes JSON vs
/// TOML by the layout cell's format; runs the strict-key guard first.
#[allow(dead_code)]
pub fn enable_hook_for(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    file: HookFile,
    event: &str,
    block: Value,
) -> Result<(), StoreError> {
    let target = hook_target(engine, scope, project_root, file)?;
    strict_key_guard(&target, "hooks")?;
    match target.format {
        ConfigFormat::JsonEmbedded => {
            guard_json_typed(&target.path, "hooks", event, &block, false)?;
            splice_nested(&target.path, "hooks", event, Some(block))
                .map_err(|e| from_anyhow(&target.path, e))
        }
        ConfigFormat::Toml => super::toml_merge::enable_hook(&target.path, event, block),
        ConfigFormat::MdYaml => Err(StoreError::Unsupported {
            message: format!("hooks are not an md-yaml primitive for {engine:?}"),
        }),
    }
}

/// Disable the hook block keyed by `event` for `(engine, scope)`.
#[allow(dead_code)]
pub fn disable_hook_for(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    file: HookFile,
    event: &str,
    store_block: &Value,
) -> Result<(), StoreError> {
    let target = hook_target(engine, scope, project_root, file)?;
    match target.format {
        ConfigFormat::JsonEmbedded => {
            guard_json_typed(&target.path, "hooks", event, store_block, true)?;
            splice_nested(&target.path, "hooks", event, None)
                .map_err(|e| from_anyhow(&target.path, e))
        }
        ConfigFormat::Toml => super::toml_merge::disable_hook(&target.path, event, store_block),
        ConfigFormat::MdYaml => Err(StoreError::Unsupported {
            message: format!("hooks are not an md-yaml primitive for {engine:?}"),
        }),
    }
}

/// Enable an MCP server definition keyed by `name` for `(engine, scope)`.
#[allow(dead_code)]
pub fn enable_mcp_for(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    name: &str,
    server_def: Value,
) -> Result<(), StoreError> {
    let target = mcp_target(engine, scope, project_root)?;
    // JSON engines splice under `mcpServers`; Codex TOML under `mcp_servers`.
    let parent = match target.format {
        ConfigFormat::Toml => "mcp_servers",
        _ => "mcpServers",
    };
    strict_key_guard(&target, parent)?;
    match target.format {
        ConfigFormat::JsonEmbedded => {
            guard_json_typed(&target.path, "mcpServers", name, &server_def, false)?;
            splice_nested(&target.path, "mcpServers", name, Some(server_def))
                .map_err(|e| from_anyhow(&target.path, e))
        }
        ConfigFormat::Toml => super::toml_merge::enable_mcp(&target.path, name, server_def),
        ConfigFormat::MdYaml => Err(StoreError::Unsupported {
            message: format!("mcp is not an md-yaml primitive for {engine:?}"),
        }),
    }
}

/// Disable the MCP server keyed by `name` for `(engine, scope)`.
#[allow(dead_code)]
pub fn disable_mcp_for(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    name: &str,
    store_def: &Value,
) -> Result<(), StoreError> {
    let target = mcp_target(engine, scope, project_root)?;
    match target.format {
        ConfigFormat::JsonEmbedded => {
            guard_json_typed(&target.path, "mcpServers", name, store_def, true)?;
            splice_nested(&target.path, "mcpServers", name, None)
                .map_err(|e| from_anyhow(&target.path, e))
        }
        ConfigFormat::Toml => super::toml_merge::disable_mcp(&target.path, name, store_def),
        ConfigFormat::MdYaml => Err(StoreError::Unsupported {
            message: format!("mcp is not an md-yaml primitive for {engine:?}"),
        }),
    }
}

/// The resolved write target for a settings-embedded mutation, exposed so the
/// command layer can report the touched file in its `ClaudeStoreMutation.path`
/// without re-deriving it.
#[allow(dead_code)]
pub fn resolve_hook_target(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
    file: HookFile,
) -> Result<PathBuf, StoreError> {
    Ok(hook_target(engine, scope, project_root, file)?.path)
}

/// As [`resolve_hook_target`], for MCP.
#[allow(dead_code)]
pub fn resolve_mcp_target(
    engine: EngineId,
    scope: &str,
    project_root: Option<&Path>,
) -> Result<PathBuf, StoreError> {
    Ok(mcp_target(engine, scope, project_root)?.path)
}

#[allow(dead_code)]
fn engine_id_str(engine: EngineId) -> &'static str {
    match engine {
        EngineId::Claude => "claude",
        EngineId::Gemini => "gemini",
        EngineId::Codex => "codex",
        EngineId::Antigravity => "antigravity-cli",
    }
}

/// Gemini's strict user settings file (`~/.gemini/settings.json`).
#[allow(dead_code)]
fn gemini_settings_path() -> Result<PathBuf, StoreError> {
    Ok(home_dir()
        .map_err(|e| StoreError::Unsupported {
            message: e.to_string(),
        })?
        .join(".gemini")
        .join("settings.json"))
}

/// Antigravity's config file (`~/.gemini/config/mcp_config.json`).
#[allow(dead_code)]
fn antigravity_config_path() -> Result<PathBuf, StoreError> {
    Ok(home_dir()
        .map_err(|e| StoreError::Unsupported {
            message: e.to_string(),
        })?
        .join(".gemini")
        .join("config")
        .join("mcp_config.json"))
}

/// Codex's user config dir (`~/.codex`).
#[allow(dead_code)]
fn codex_dir() -> Result<PathBuf, StoreError> {
    Ok(home_dir()
        .map_err(|e| StoreError::Unsupported {
            message: e.to_string(),
        })?
        .join(".codex"))
}

/// Map a Phase-1 path-resolution `anyhow::Error` (scope grammar / missing root)
/// into the typed error.
#[allow(dead_code)]
fn map_path_err(e: anyhow::Error) -> StoreError {
    StoreError::Unsupported {
        message: e.to_string(),
    }
}

// ─── Path resolution ──────────────────────────────────────────────────────────

fn home_dir() -> Result<PathBuf> {
    // $HOME is unset on Windows; route through the platform resolver.
    crate::platform::home_dir().ok_or_else(|| anyhow!("home directory not found"))
}

/// `<dir>/.claude` for a project root, or `~/.claude` for workspace scope.
fn claude_dir(scope: &str, project_root: Option<&Path>) -> Result<PathBuf> {
    match validate_scope(scope)? {
        ScopeKind::Workspace => Ok(home_dir()?.join(".claude")),
        ScopeKind::Project(_) => {
            let root = project_root.ok_or_else(|| {
                anyhow!("project scope {scope:?} requires a resolved project_root")
            })?;
            Ok(root.join(".claude"))
        }
    }
}

fn hook_path(scope: &str, project_root: Option<&Path>, file: HookFile) -> Result<PathBuf> {
    let name = match file {
        HookFile::Shared => "settings.json",
        HookFile::Local => "settings.local.json",
    };
    Ok(claude_dir(scope, project_root)?.join(name))
}

fn mcp_path(scope: &str, project_root: Option<&Path>) -> Result<PathBuf> {
    match validate_scope(scope)? {
        // User-scope MCP servers live in the home `~/.claude.json` that Claude
        // Code actually reads (see registries/mcp.rs). NOT `~/.claude/...`.
        ScopeKind::Workspace => Ok(home_dir()?.join(".claude.json")),
        // Project-scope MCP servers live in `<root>/.mcp.json` (Claude Code's
        // canonical per-project MCP file).
        ScopeKind::Project(_) => {
            let root = project_root.ok_or_else(|| {
                anyhow!("project scope {scope:?} requires a resolved project_root")
            })?;
            Ok(root.join(".mcp.json"))
        }
    }
}

// ─── Core splice ──────────────────────────────────────────────────────────────

/// Read `path` as a JSON object, set or remove `parent_key.child_key`, then
/// atomically write it back. `value = Some(_)` inserts/overwrites; `None`
/// removes. All sibling keys (top-level and within `parent_key`) are carried
/// through untouched.
///
/// Removal of a missing file / parent / child is a no-op (no write, no error)
/// — important so a `disable_*` on a clean machine never materializes a file.
fn splice_nested(
    path: &Path,
    parent_key: &str,
    child_key: &str,
    value: Option<Value>,
) -> Result<()> {
    let removing = value.is_none();
    let existed = path.exists();

    // No-op fast paths for removal so we never create a file just to delete
    // from it.
    if removing && !existed {
        return Ok(());
    }

    let mut root = read_object(path)?;

    if removing {
        // Only descend if the parent object exists and holds the child.
        let removed = match root.get_mut(parent_key) {
            Some(Value::Object(inner)) => inner.remove(child_key).is_some(),
            _ => false,
        };
        if !removed {
            // Nothing to do — leave the file byte-identical by not rewriting.
            return Ok(());
        }
        write_object(path, &root)?;
        return Ok(());
    }

    // Insert / overwrite. Get-or-create the parent object; refuse to clobber a
    // non-object parent (a user/Claude-Code-set scalar there is load-bearing).
    let block = value.expect("checked Some above");
    let entry = root
        .entry(parent_key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    match entry {
        Value::Object(inner) => {
            inner.insert(child_key.to_string(), block);
        }
        _ => {
            return Err(anyhow!(
                "{} `{parent_key}` is not an object — refusing to overwrite",
                path.display()
            ));
        }
    }
    write_object(path, &root)?;
    Ok(())
}

/// Read a JSON file into a root object. Missing / empty file → empty object so
/// first-write works on a clean machine. A non-object root is an error
/// (matches `read_settings` / `load_config` behaviour — refuse to overwrite).
fn read_object(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok(Map::new()),
        Ok(s) => {
            let v: Value =
                serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?;
            match v {
                Value::Object(m) => Ok(m),
                other => Err(anyhow!(
                    "{} is not a JSON object (got {other:?}) — refusing to overwrite",
                    path.display()
                )),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(anyhow!("read {}: {e}", path.display())),
    }
}

/// Atomic write: serialize → temp file in the same dir → fsync-free rename.
/// Pretty-printed with a trailing newline to match the engine adapter's
/// `write_settings` output (byte equivalence across the two write sites).
fn write_object(path: &Path, root: &Map<String, Value>) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent"))?;
    std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    let mut pretty = serde_json::to_string_pretty(&Value::Object(root.clone()))
        .map_err(|e| anyhow!("serialize claude settings: {e}"))?;
    pretty.push('\n');
    let tmp_name = format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "settings".to_string()),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(tmp_name);
    std::fs::write(&tmp, pretty).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    // ── Ownership guard: never destroy an entry that isn't the store's ──────

    fn write_json(p: &Path, v: &Value) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    /// The reported bug: "disabling" a hand-written MCP server deleted it,
    /// env and API keys included.
    #[test]
    fn disable_mcp_refuses_a_users_own_server() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = root.join(".mcp.json");
        write_json(
            &p,
            &json!({ "mcpServers": {
                "github": { "command": "gh-mcp", "env": { "GITHUB_TOKEN": "ghp_mine" } },
                "exa": { "command": "exa-mcp" }
            }}),
        );
        let before = fs::read(&p).unwrap();

        let store_def = json!({ "command": "some-other-github-mcp" });
        let err = disable_mcp("project:demo", Some(root), "github", &store_def).unwrap_err();
        assert!(err.to_string().contains("refusing to change `mcpServers.github`"), "{err}");
        assert_eq!(fs::read(&p).unwrap(), before, "config byte-for-byte untouched");
    }

    /// A store server the user has since edited (a pasted-in API key, say) is
    /// not silently discarded either.
    #[test]
    fn disable_mcp_refuses_an_edited_store_server() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let def = json!({ "command": "royalti-mcp", "env": { "API_KEY": "" } });
        enable_mcp("project:demo", Some(root), "royalti", def.clone()).unwrap();
        let p = root.join(".mcp.json");
        let mut v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        v["mcpServers"]["royalti"]["env"]["API_KEY"] = json!("sk-pasted-by-user");
        write_json(&p, &v);
        let before = fs::read(&p).unwrap();

        assert!(disable_mcp("project:demo", Some(root), "royalti", &def).is_err());
        assert_eq!(fs::read(&p).unwrap(), before, "the edit survives");
    }

    /// Enabling a store server must not replace a user's own server of the
    /// same name.
    #[test]
    fn enable_mcp_refuses_to_replace_a_users_own_server() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = root.join(".mcp.json");
        write_json(&p, &json!({ "mcpServers": { "github": { "command": "gh-mcp" } } }));
        let before = fs::read(&p).unwrap();

        let err = enable_mcp("project:demo", Some(root), "github", json!({ "command": "store-gh" }))
            .unwrap_err();
        assert!(err.to_string().contains("Enabling would overwrite it"), "{err}");
        assert_eq!(fs::read(&p).unwrap(), before);
    }

    /// `hooks.<event>` holds every hook on that event. Enabling a store hook
    /// used to replace the whole array, wiping the user's own hooks, security
    /// hooks such as a secret scanner included.
    #[test]
    fn enable_hook_refuses_to_wipe_existing_hooks_on_the_event() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = root.join(".claude").join("settings.json");
        let users_hooks = json!([{ "matcher": "Bash", "hooks": [
            { "type": "command", "command": ".claude/hooks/secret-scan.sh" }
        ]}]);
        write_json(&p, &json!({ "hooks": { "PreToolUse": users_hooks.clone() } }));
        let before = fs::read(&p).unwrap();

        let store_block = json!([{ "matcher": "Edit", "hooks": [
            { "type": "command", "command": "store-hook" }
        ]}]);
        let err = enable_hook(
            "project:demo",
            Some(root),
            HookFile::Shared,
            "PreToolUse",
            store_block,
        )
        .unwrap_err();
        assert!(err.to_string().contains("refusing to change `hooks.PreToolUse`"), "{err}");
        assert_eq!(fs::read(&p).unwrap(), before, "the secret-scan hook survives");
    }

    /// Disabling removes the whole event, so it is refused unless the event
    /// holds exactly the store's block.
    #[test]
    fn disable_hook_refuses_when_the_event_holds_other_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = root.join(".claude").join("settings.json");
        let store_block = json!([{ "matcher": "Edit", "hooks": [] }]);
        let mixed = json!([
            { "matcher": "Edit", "hooks": [] },
            { "matcher": "Bash", "hooks": [{ "type": "command", "command": "mine" }] }
        ]);
        write_json(&p, &json!({ "hooks": { "PreToolUse": mixed } }));
        let before = fs::read(&p).unwrap();

        assert!(disable_hook(
            "project:demo",
            Some(root),
            HookFile::Shared,
            "PreToolUse",
            &store_block
        )
        .is_err());
        assert_eq!(fs::read(&p).unwrap(), before);
    }

    /// Re-enabling an entry that already equals the store block is idempotent.
    #[test]
    fn enable_is_idempotent_when_the_entry_is_already_the_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let def = json!({ "command": "royalti-mcp" });
        enable_mcp("project:demo", Some(root), "royalti", def.clone()).unwrap();
        enable_mcp("project:demo", Some(root), "royalti", def.clone()).unwrap();
        disable_mcp("project:demo", Some(root), "royalti", &def).unwrap();
    }

    /// The engine-aware path reports the refusal as a typed `NotOurs`.
    #[test]
    fn engine_aware_disable_reports_not_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = root.join(".mcp.json");
        write_json(&p, &json!({ "mcpServers": { "github": { "command": "gh-mcp" } } }));
        let err = disable_mcp_for(
            EngineId::Claude,
            "project:demo",
            Some(root),
            "github",
            &json!({ "command": "other" }),
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::NotOurs { .. }), "{err:?}");
    }

    // ── Scope grammar (mirrors the pin layer's coverage) ──────────────────
    #[test]
    fn scope_grammar() {
        assert_eq!(validate_scope("workspace").unwrap(), ScopeKind::Workspace);
        assert_eq!(
            validate_scope("project:music-2026").unwrap(),
            ScopeKind::Project("music-2026".to_string())
        );
        assert!(validate_scope("personal").is_err());
        assert!(validate_scope("project:").is_err());
        assert!(validate_scope("project:BadCase").is_err());
        assert!(validate_scope("workspace:x").is_err());
    }

    /// A `settings.json` in the canonical write form this engine emits, with
    /// several unrelated keys we must never disturb.
    fn seed_settings(dir: &Path) -> PathBuf {
        let claude = dir.join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let p = claude.join("settings.json");
        let initial = json!({
            "model": "claude-opus-4",
            "permissions": { "allow": ["Bash(ls:*)"], "deny": [] },
            "statusLine": { "type": "command", "command": "~/bin/sl" },
        });
        // Write through our own writer so the baseline is in canonical form
        // (sorted keys + pretty + trailing newline). The round-trip assertion
        // then proves enable→disable returns to *this* exact byte sequence.
        let m = match initial {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        write_object(&p, &m).unwrap();
        p
    }

    #[test]
    fn hook_round_trip_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let p = seed_settings(root);
        let baseline = fs::read(&p).unwrap();
        let scope = "project:demo";

        let block = json!([
            { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo hi" } ] }
        ]);
        enable_hook(scope, Some(root), HookFile::Shared, "PreToolUse", block.clone()).unwrap();

        let after_add: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        // Hook landed under hooks.PreToolUse.
        assert!(after_add
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .is_some());
        // Unrelated keys untouched.
        assert_eq!(after_add.get("model").unwrap(), &json!("claude-opus-4"));
        assert_eq!(
            after_add.get("statusLine").unwrap(),
            &json!({ "type": "command", "command": "~/bin/sl" })
        );

        disable_hook(scope, Some(root), HookFile::Shared, "PreToolUse", &block).unwrap();
        let after_remove = fs::read(&p).unwrap();

        // The only delta disable leaves is an empty `hooks: {}` object — assert
        // that explicitly, then prove every original key is byte-identical by
        // stripping the added empty parent and re-serializing.
        let mut v: Value = serde_json::from_slice(&after_remove).unwrap();
        assert_eq!(
            v.get("hooks").unwrap(),
            &json!({}),
            "removing the last hook leaves an empty hooks object"
        );
        if let Value::Object(m) = &mut v {
            m.remove("hooks");
        }
        let mut reser = serde_json::to_string_pretty(&v).unwrap();
        reser.push('\n');
        assert_eq!(
            reser.as_bytes(),
            baseline.as_slice(),
            "all unrelated keys byte-identical after enable→disable"
        );
    }

    #[test]
    fn project_mcp_round_trip_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Seed a project `.mcp.json` in canonical form with one pre-existing
        // user server we must preserve.
        let p = root.join(".mcp.json");
        let seed = json!({
            "mcpServers": {
                "exa": { "type": "stdio", "command": "exa-mcp", "args": [] }
            }
        });
        let m = match seed {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        write_object(&p, &m).unwrap();
        let baseline = fs::read(&p).unwrap();
        let scope = "project:demo";

        let def = json!({ "type": "stdio", "command": "royalti-mcp", "args": ["--stdio"] });
        enable_mcp(scope, Some(root), "royalti", def.clone()).unwrap();

        let after_add: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        let servers = after_add.get("mcpServers").unwrap();
        assert!(servers.get("royalti").is_some());
        // Pre-existing server preserved verbatim.
        assert_eq!(
            servers.get("exa").unwrap(),
            &json!({ "type": "stdio", "command": "exa-mcp", "args": [] })
        );

        disable_mcp(scope, Some(root), "royalti", &def).unwrap();
        let after_remove = fs::read(&p).unwrap();
        assert_eq!(
            after_remove, baseline,
            "project .mcp.json byte-identical after enable→disable (exa untouched)"
        );
    }

    /// The load-bearing safety property: a workspace-scope MCP enable/disable
    /// touches ONLY the `mcpServers` key of `~/.claude.json` and leaves OAuth /
    /// session state byte-identical. We point HOME at a temp dir so the test is
    /// hermetic and never touches the real `~/.claude.json`.
    #[test]
    fn user_claude_json_session_keys_untouched() {
        // Serialize against the WP-22 HOME-mutating tests below (they share the
        // process-global HOME). Without this lock the parallel test runner can
        // interleave HOME swaps and corrupt this test's view.
        let _g = home_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let claude_json = home.join(".claude.json");
        // Realistic shape: OAuth / session keys alongside an existing server.
        let seed = json!({
            "oauthAccount": { "accountUuid": "abc-123", "emailAddress": "x@y.z" },
            "mcpServers": {
                "pencil": { "type": "stdio", "command": "pencil", "args": [] }
            },
            "numStartups": 42,
            "userID": "user_xyz",
        });
        let m = match seed {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        write_object(&claude_json, &m).unwrap();
        let baseline = fs::read(&claude_json).unwrap();

        // Run enable+disable with HOME overridden. Env mutation is process-wide
        // — guard it for this single-threaded test path. USERPROFILE too,
        // since `home_dir()` prefers it on Windows.
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);

        let def = json!({ "type": "stdio", "command": "royalti-mcp", "args": [] });
        enable_mcp("workspace", None, "royalti", def.clone()).unwrap();

        let after_add: Value = serde_json::from_slice(&fs::read(&claude_json).unwrap()).unwrap();
        // Session keys still present & equal after the add.
        assert_eq!(
            after_add.get("oauthAccount").unwrap(),
            &json!({ "accountUuid": "abc-123", "emailAddress": "x@y.z" })
        );
        assert_eq!(after_add.get("numStartups").unwrap(), &json!(42));
        assert_eq!(after_add.get("userID").unwrap(), &json!("user_xyz"));
        // Our server landed; pencil preserved.
        let servers = after_add.get("mcpServers").unwrap();
        assert!(servers.get("royalti").is_some());
        assert!(servers.get("pencil").is_some());

        disable_mcp("workspace", None, "royalti", &def).unwrap();
        let after_remove = fs::read(&claude_json).unwrap();

        // Restore HOME/USERPROFILE before any assertion can early-return.
        match prev_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match prev_userprofile {
            Some(h) => std::env::set_var("USERPROFILE", h),
            None => std::env::remove_var("USERPROFILE"),
        }

        assert_eq!(
            after_remove, baseline,
            "~/.claude.json byte-identical after enable→disable — only mcpServers was touched"
        );
    }

    #[test]
    fn disable_on_clean_machine_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // No files exist yet.
        disable_hook("project:demo", Some(root), HookFile::Local, "PreToolUse", &json!([])).unwrap();
        disable_mcp("project:demo", Some(root), "nope", &json!({})).unwrap();
        assert!(!root.join(".claude").join("settings.local.json").exists());
        assert!(!root.join(".mcp.json").exists());
    }

    #[test]
    fn project_scope_requires_root() {
        let block = json!([]);
        assert!(enable_hook("project:demo", None, HookFile::Shared, "E", block).is_err());
        assert!(enable_mcp("project:demo", None, "n", json!({})).is_err());
    }

    #[test]
    fn refuses_non_object_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let claude = root.join(".claude");
        fs::create_dir_all(&claude).unwrap();
        let p = claude.join("settings.json");
        // hooks is a scalar — engine must refuse rather than clobber.
        fs::write(&p, "{\"hooks\": \"oops\"}\n").unwrap();
        let err = enable_hook("project:demo", Some(root), HookFile::Shared, "E", json!([]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not an object"), "got: {err}");
    }

    // ── WP-22 (G-WRITE) — engine-aware dispatch + strict-key guard ───────────
    //
    // These exercise the v2b `*_for(EngineId, …)` surface: per-engine path +
    // format resolution from the frozen `EngineLayout`, the Gemini strict-key
    // refusal (typed, before any write), Codex TOML routing, and the
    // per-engine path confinement that drops out of the layout templates.
    //
    // The Gemini/Codex paths resolve `~/.gemini` / `~/.codex`, so these mutate
    // the process-global `HOME` and serialize on a module-local lock — the same
    // `HomeGuard` pattern `claude_config.rs` uses.

    fn home_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
        previous_userprofile: Option<std::ffi::OsString>,
        _tmp: tempfile::TempDir,
    }
    impl HomeGuard {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("tempdir");
            let previous = std::env::var_os("HOME");
            std::env::set_var("HOME", tmp.path());
            // `home_dir()` prefers USERPROFILE on Windows, so it must be
            // pointed at the tempdir too or these tests would resolve the
            // real user profile there instead of the fixture.
            let previous_userprofile = std::env::var_os("USERPROFILE");
            std::env::set_var("USERPROFILE", tmp.path());
            Self {
                previous,
                previous_userprofile,
                _tmp: tmp,
            }
        }
        fn home(&self) -> PathBuf {
            self._tmp.path().to_path_buf()
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_userprofile.take() {
                Some(h) => std::env::set_var("USERPROFILE", h),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    use crate::commands::engine_layout::EngineId;

    /// Claude hooks still route through the JSON path under the engine-aware API
    /// — proving the Phase-1 behaviour is preserved when threaded by `EngineId`.
    #[test]
    fn claude_hook_dispatch_matches_phase1_json() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let scope = "project:demo";
        let block = json!([{ "matcher": "Bash", "hooks": [] }]);
        enable_hook_for(
            EngineId::Claude,
            scope,
            Some(root),
            HookFile::Shared,
            "PreToolUse",
            block.clone(),
        )
        .unwrap();
        let p = root.join(".claude").join("settings.json");
        let v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert!(v.pointer("/hooks/PreToolUse").is_some());
        disable_hook_for(
            EngineId::Claude,
            scope,
            Some(root),
            HookFile::Shared,
            "PreToolUse",
            &block,
        )
        .unwrap();
        let v2: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
        assert!(v2.pointer("/hooks/PreToolUse").is_none());
    }

    /// Codex MCP routes to the TOML engine at `~/.codex/config.toml` and splices
    /// only `mcp_servers.<name>`, preserving an unrelated server byte-identical.
    #[test]
    fn codex_mcp_dispatch_routes_to_toml() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let codex = home.home().join(".codex");
        fs::create_dir_all(&codex).unwrap();
        let cfg = codex.join("config.toml");
        fs::write(
            &cfg,
            "model = \"o3\"\n\n[mcp_servers.exa]\ncommand = \"exa-mcp\"\n",
        )
        .unwrap();
        let baseline = fs::read(&cfg).unwrap();

        let def = json!({ "command": "royalti-mcp", "args": ["--stdio"] });
        // user scope → no project root needed for Codex
        enable_mcp_for(EngineId::Codex, "workspace", None, "royalti", def.clone()).unwrap();

        // target resolves to ~/.codex/config.toml
        assert_eq!(
            resolve_mcp_target(EngineId::Codex, "workspace", None).unwrap(),
            cfg
        );
        let after: toml::Value = toml::from_str(&fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(
            after
                .get("mcp_servers")
                .and_then(|m| m.get("royalti"))
                .and_then(|r| r.get("command"))
                .and_then(|c| c.as_str()),
            Some("royalti-mcp")
        );
        assert!(after
            .get("mcp_servers")
            .and_then(|m| m.get("exa"))
            .is_some());

        disable_mcp_for(EngineId::Codex, "workspace", None, "royalti", &def).unwrap();
        assert_eq!(
            fs::read(&cfg).unwrap(),
            baseline,
            "config.toml byte-identical after Codex enable→disable"
        );
    }

    /// The strict-key guard: Gemini's strict `settings.json` accepts `mcpServers`
    /// + `hooks` but the guard refuses any *other* top-level key with a TYPED
    /// `StrictKeyRejected` error — proving the refusal mechanism. We force the
    /// rejection via the guard directly with a bogus key (the public `*_for`
    /// entry points only ever pass the two allowed parents, so this is the only
    /// way to drive the refusal arm).
    #[test]
    fn gemini_strict_key_guard_refuses_bogus_key_typed() {
        let _g = home_lock();
        let _home = HomeGuard::new();
        // A Gemini MCP write target is strict (additionalProperties:false).
        let target = mcp_target(EngineId::Gemini, "workspace", None).unwrap();
        assert!(
            target.strict_keys,
            "Gemini MCP settings file must be strict"
        );

        // The allowed parents pass.
        assert!(strict_key_guard(&target, "mcpServers").is_ok());
        assert!(strict_key_guard(&target, "hooks").is_ok());

        // A non-recognized key is refused with the typed variant — before write.
        let err = strict_key_guard(&target, "totallyMadeUpKey").unwrap_err();
        match err {
            StoreError::StrictKeyRejected { engine, key } => {
                assert_eq!(engine, "gemini");
                assert_eq!(key, "totallyMadeUpKey");
            }
            other => panic!("expected StrictKeyRejected, got {other:?}"),
        }
    }

    /// Claude + Codex are lenient — the guard never refuses, regardless of key.
    #[test]
    fn lenient_engines_never_strict_refuse() {
        let _g = home_lock();
        let _home = HomeGuard::new();
        let cl = mcp_target(EngineId::Claude, "workspace", None).unwrap();
        let cx = mcp_target(EngineId::Codex, "workspace", None).unwrap();
        assert!(!cl.strict_keys);
        assert!(!cx.strict_keys);
        assert!(strict_key_guard(&cl, "anything").is_ok());
        assert!(strict_key_guard(&cx, "anything").is_ok());
    }

    /// A real Gemini MCP enable lands `mcpServers.<name>` in
    /// `~/.gemini/settings.json` (the strict file), preserving unrelated keys.
    /// The recognized `mcpServers` parent passes the guard, so the write
    /// succeeds — the guard refuses only mis-routed keys, never the legitimate
    /// MCP/hook writes.
    #[test]
    fn gemini_mcp_enable_writes_recognized_key() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let gem = home.home().join(".gemini");
        fs::create_dir_all(&gem).unwrap();
        let settings = gem.join("settings.json");
        // strict file with an unrelated recognized key we must preserve
        let m = match json!({ "theme": "dark", "mcpServers": {} }) {
            Value::Object(m) => m,
            _ => unreachable!(),
        };
        write_object(&settings, &m).unwrap();

        let def = json!({ "command": "royalti-mcp", "args": [] });
        enable_mcp_for(EngineId::Gemini, "workspace", None, "royalti", def.clone()).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert!(v.pointer("/mcpServers/royalti").is_some());
        assert_eq!(v.get("theme").unwrap(), &json!("dark"));

        disable_mcp_for(EngineId::Gemini, "workspace", None, "royalti", &def).unwrap();
        let v2: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert!(v2.pointer("/mcpServers/royalti").is_none());
        assert_eq!(v2.get("theme").unwrap(), &json!("dark"));
    }

    /// Per-engine path confinement: each engine's resolved write target sits
    /// under that engine's own root, never another engine's — the confinement
    /// falls directly out of the frozen `EngineLayout` location templates.
    #[test]
    fn per_engine_path_confinement() {
        let _g = home_lock();
        let home = HomeGuard::new();
        let h = home.home();

        // Codex MCP/hook targets live under ~/.codex, NOT ~/.gemini or ~/.claude.
        let cx_mcp = resolve_mcp_target(EngineId::Codex, "workspace", None).unwrap();
        assert!(
            cx_mcp.starts_with(h.join(".codex")),
            "codex mcp: {cx_mcp:?}"
        );
        let cx_hook =
            resolve_hook_target(EngineId::Codex, "workspace", None, HookFile::Shared).unwrap();
        assert!(
            cx_hook.starts_with(h.join(".codex")),
            "codex hook: {cx_hook:?}"
        );

        // Gemini MCP/hook targets live under ~/.gemini.
        let gm_mcp = resolve_mcp_target(EngineId::Gemini, "workspace", None).unwrap();
        assert!(
            gm_mcp.starts_with(h.join(".gemini")),
            "gemini mcp: {gm_mcp:?}"
        );

        // Claude user MCP lands in ~/.claude.json (a file directly under HOME);
        // a Claude *project* hook target lands under the project root's .claude,
        // never under HOME's other-engine dirs.
        let cl_mcp = resolve_mcp_target(EngineId::Claude, "workspace", None).unwrap();
        assert_eq!(cl_mcp, h.join(".claude.json"));
        let proj = h.join("someproj");
        let cl_hook =
            resolve_hook_target(EngineId::Claude, "project:p", Some(&proj), HookFile::Local)
                .unwrap();
        assert_eq!(cl_hook, proj.join(".claude").join("settings.local.json"));
        // crucially NOT under another engine's dir
        assert!(!cl_hook.starts_with(h.join(".gemini")));
        assert!(!cl_hook.starts_with(h.join(".codex")));
    }

    /// Atomicity for the TOML engine: an interrupted write (temp staged, rename
    /// not yet performed) leaves the destination holding the ORIGINAL content —
    /// the partial write is quarantined in the temp file, mirroring the JSON
    /// path's interrupted-write guarantee.
    #[test]
    fn codex_toml_interrupted_write_leaves_dest_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("config.toml");
        fs::write(
            &cfg,
            "model = \"o3\"\n[mcp_servers.exa]\ncommand = \"exa\"\n",
        )
        .unwrap();
        let original = fs::read(&cfg).unwrap();

        // Stage a temp file in the same dir mimicking the write-half of the
        // atomic rename, WITHOUT renaming it over the dest.
        let staged = tmp.path().join(".config.toml.partial.tmp");
        fs::write(&staged, "GARBAGE not valid toml {{{").unwrap();

        // Dest still holds the original, fully-valid content.
        assert_eq!(
            fs::read(&cfg).unwrap(),
            original,
            "dest untouched until rename"
        );

        // Completing the rename flips atomically.
        fs::rename(&staged, &cfg).unwrap();
        assert_ne!(
            fs::read(&cfg).unwrap(),
            original,
            "rename commits new content"
        );
    }
}
