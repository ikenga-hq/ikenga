//! Ngwa unified snapshot command (WP-14, review fixes WP-14a).
//!
//! Joins the subsystems specified in gate G-NGWA-ITEM
//! (`plans/shell-ux-rearchitecture/drafts/ngwa-item.md`):
//!   1. Pkg kernel (`InstalledSummary` / manifests / `sidecar_supervisor`)
//!   2. Ọba Claude-asset store (`claude_store_list`)
//!   3. Engine-config scan (`claude_config` scan across projects + personal root)
//!   4. `engine_assets` registry (pkg-contributed placements)
//!   5. Trust (`pkg/trust.rs` sensitive perms + `pkg_trust` capability diff)
//!   6. Transcript JSONL usage mirror (DEC-24, DEC-27)
//!
//! The Tauri command only *collects* inputs. The join itself is the pure
//! [`build_snapshot`], which takes no Tauri state — that is what the golden
//! test (`__fixtures__/ngwa-snapshot.golden.json`) and the vitest DEC-26
//! parity test both consume.
//!
//! ## Item identity (F-5)
//!
//! Config-scan placements are grouped by `(kind, scope, name)`: a personal and
//! a project placement of one skill are two items. Pkg-backed items use the
//! manifest id; everything else is `${kind}:${scope_key}:${name}` where
//! `scope_key` is `personal` or `project:<id>`. `<id>` is the registered
//! project's id when its root is in the `projects` table, otherwise the
//! normalized root path (forward slashes, lowercased drive letter, no trailing
//! slash). **Registering a project later re-keys that project's items** from
//! the path form to the id form.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::commands::claude_config::{self, ClaudeConfig, Scope as ConfigScope, SystemTag};
use crate::commands::claude_store::{self, ClaudeStoreEntry, ProvenanceSource};
use crate::commands::db::PaDb;
use crate::commands::engine_layout::{
    engine_layouts, ConfigFormat, EngineId, EngineLayout, KindStatus, Mechanism, PrimitiveKind,
};
use crate::commands::pkg::KernelState;
use crate::commands::projects;
use crate::pkg::kernel::{InstalledSummary, KernelStatus};
use crate::pkg::manifest::{Manifest, Package, RequireSource, RequiresEntry};
use crate::pkg::source::InstallSource;
use crate::pkg::trust::{self, TrustState};
use crate::transcript::usage::{self, UsageAggregate, UsageKind, UsageSnapshot};

// ── Wire types (exact parity with drafts/ngwa-item.md §2 / @ikenga/contract/ngwa) ──

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaKind {
    App,
    Engine,
    Tool,
    Sidecar,
    Skill,
    Agent,
    Command,
    Hook,
    Bundle,
    Schedule,
    /// Phase 4 — in the union, never emitted by WP-14.
    Workflow,
}

impl NgwaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NgwaKind::App => "app",
            NgwaKind::Engine => "engine",
            NgwaKind::Tool => "tool",
            NgwaKind::Sidecar => "sidecar",
            NgwaKind::Skill => "skill",
            NgwaKind::Agent => "agent",
            NgwaKind::Command => "command",
            NgwaKind::Hook => "hook",
            NgwaKind::Bundle => "bundle",
            NgwaKind::Schedule => "schedule",
            NgwaKind::Workflow => "workflow",
        }
    }

    /// Ọba's `ClaudeStoreEntry.kind` → an item kind. `mcp` is an MCP server,
    /// i.e. a `tool`. Unknown strings are rejected rather than passed through.
    pub fn from_store_kind(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(NgwaKind::Skill),
            "agent" => Some(NgwaKind::Agent),
            "command" => Some(NgwaKind::Command),
            "hook" => Some(NgwaKind::Hook),
            "bundle" => Some(NgwaKind::Bundle),
            "mcp" => Some(NgwaKind::Tool),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NgwaScope {
    #[serde(rename = "personal")]
    Personal,
    #[serde(rename = "project")]
    Project { project_id: String },
}

impl NgwaScope {
    pub fn key(&self) -> String {
        match self {
            NgwaScope::Personal => "personal".to_string(),
            NgwaScope::Project { project_id } => format!("project:{project_id}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaSource {
    Builtin,
    Registry,
    Git,
    Npx,
    Local,
    Dev,
    Catalog,
}

impl From<&RequireSource> for NgwaSource {
    fn from(s: &RequireSource) -> Self {
        match s {
            RequireSource::Git => NgwaSource::Git,
            RequireSource::Npx => NgwaSource::Npx,
            RequireSource::Catalog => NgwaSource::Catalog,
            RequireSource::Local => NgwaSource::Local,
        }
    }
}

impl From<&ProvenanceSource> for NgwaSource {
    fn from(s: &ProvenanceSource) -> Self {
        match s {
            ProvenanceSource::Local => NgwaSource::Local,
            ProvenanceSource::Git => NgwaSource::Git,
            ProvenanceSource::Npx => NgwaSource::Npx,
            ProvenanceSource::Catalog => NgwaSource::Catalog,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaOrigin {
    pub source: NgwaSource,
    pub url: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
    pub resolved_version: Option<String>,
    pub publisher: Option<String>,
    pub managed: bool,
    pub auto_update: bool,
    pub installed_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
}

impl NgwaOrigin {
    fn local() -> Self {
        NgwaOrigin {
            source: NgwaSource::Local,
            url: None,
            r#ref: None,
            resolved_version: None,
            publisher: None,
            managed: false,
            auto_update: false,
            installed_at_ms: None,
            updated_at_ms: None,
        }
    }
}

/// `available` and `update` are set by the WP-15 enrichment layer, never here
/// (gate §3, Round 13); `orphaned` is derived by WP-16 Health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaState {
    Enabled,
    Disabled,
    Available,
    Update,
    Orphaned,
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaRuntimeState {
    Spawning,
    Running,
    Crashed,
    Blocked,
    Parked,
    Stopped,
    #[serde(rename = "shuttingdown")]
    ShuttingDown,
}

impl NgwaRuntimeState {
    /// Map the supervisor's `SidecarStatus.state` string. An unknown value maps
    /// to the defined fallback `stopped` (never an out-of-union string).
    pub fn from_supervisor(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "spawning" => NgwaRuntimeState::Spawning,
            "running" => NgwaRuntimeState::Running,
            "crashed" => NgwaRuntimeState::Crashed,
            "blocked" => NgwaRuntimeState::Blocked,
            "parked" => NgwaRuntimeState::Parked,
            "stopped" => NgwaRuntimeState::Stopped,
            "shuttingdown" | "shutting_down" => NgwaRuntimeState::ShuttingDown,
            other => {
                log::warn!("[ngwa_snapshot] unknown supervisor state `{other}`; reporting `stopped`");
                NgwaRuntimeState::Stopped
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaRuntime {
    pub state: NgwaRuntimeState,
    pub pid: Option<u32>,
    pub uptime_s: Option<u64>,
    pub restarts: u32,
    pub last_err: Option<String>,
    pub last_crash_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaPermsSummary {
    pub shell_execute: Vec<String>,
    pub fs_write_outside_sandbox: Vec<String>,
    pub net: Vec<String>,
    pub vault_keys: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NgwaTrustState {
    AutoTrusted,
    AutoGranted,
    Granted,
    NeedsApproval,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaTrust {
    pub state: NgwaTrustState,
    pub signed: bool,
    pub auto_trusted: bool,
    pub review_pending: bool,
    pub perms: Option<NgwaPermsSummary>,
    pub last_granted_at_ms: Option<i64>,
}

impl NgwaTrust {
    fn not_applicable() -> Self {
        NgwaTrust {
            state: NgwaTrustState::NotApplicable,
            signed: false,
            auto_trusted: false,
            review_pending: false,
            perms: None,
            last_granted_at_ms: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NgwaMechanism {
    SymlinkDir,
    File,
    SettingsKey,
}

impl From<Mechanism> for NgwaMechanism {
    fn from(m: Mechanism) -> Self {
        match m {
            Mechanism::SymlinkDir => NgwaMechanism::SymlinkDir,
            Mechanism::File => NgwaMechanism::File,
            Mechanism::SettingsKey => NgwaMechanism::SettingsKey,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaManagedBy {
    Oba,
    Pkg,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NgwaFormat {
    MdYaml,
    Toml,
    JsonEmbedded,
}

impl From<ConfigFormat> for NgwaFormat {
    fn from(f: ConfigFormat) -> Self {
        match f {
            ConfigFormat::MdYaml => NgwaFormat::MdYaml,
            ConfigFormat::Toml => NgwaFormat::Toml,
            ConfigFormat::JsonEmbedded => NgwaFormat::JsonEmbedded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaPlacementStatus {
    Active,
    Deprecated,
}

impl From<KindStatus> for NgwaPlacementStatus {
    fn from(s: KindStatus) -> Self {
        match s {
            KindStatus::Active => NgwaPlacementStatus::Active,
            KindStatus::Deprecated => NgwaPlacementStatus::Deprecated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaPlacement {
    pub engine: String,
    pub scope: NgwaScope,
    pub path: String,
    pub mechanism: NgwaMechanism,
    pub present: bool,
    pub link_target: Option<String>,
    pub in_store: bool,
    pub managed_by: NgwaManagedBy,
    pub overridden_by: Option<String>,
    pub format: Option<NgwaFormat>,
    pub status: NgwaPlacementStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NgwaUsageSource {
    Hooks,
    Transcript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaUsage {
    pub source: NgwaUsageSource,
    pub last_used_ms: Option<i64>,
    pub count_7d: Option<i64>,
    pub count_30d: Option<i64>,
    pub tokens_30d: Option<i64>,
    pub window_start_ms: i64,
}

impl From<UsageAggregate> for NgwaUsage {
    fn from(a: UsageAggregate) -> Self {
        NgwaUsage {
            source: NgwaUsageSource::Transcript,
            last_used_ms: a.last_used_ms,
            count_7d: Some(a.count_7d),
            count_30d: Some(a.count_30d),
            tokens_30d: Some(a.tokens_30d),
            window_start_ms: a.window_start_ms,
        }
    }
}

/// `kind` stays an open string on the wire (`NgwaKind | string`), mirroring
/// the open `RequiresEntry.kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaRef {
    pub kind: String,
    pub name: String,
    pub item_id: Option<String>,
    pub source: Option<NgwaSource>,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
}

impl From<&RequiresEntry> for NgwaRef {
    fn from(r: &RequiresEntry) -> Self {
        NgwaRef {
            kind: r.kind.clone(),
            name: r.name.clone(),
            item_id: None,
            source: r.source.as_ref().map(NgwaSource::from),
            r#ref: r.r#ref.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaItem {
    pub id: String,
    pub kind: NgwaKind,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    /// Always `null` from the snapshot — WP-15 enriches it (gate §3, Round 13).
    pub latest_version: Option<String>,
    pub scope: NgwaScope,
    pub origin: NgwaOrigin,
    pub state: NgwaState,
    pub runtime: Option<NgwaRuntime>,
    pub trust: NgwaTrust,
    pub placements: Vec<NgwaPlacement>,
    pub usage: Option<NgwaUsage>,
    pub requires: Vec<NgwaRef>,
    pub required_by: Vec<NgwaRef>,
    pub owner_pkg_id: Option<String>,
    pub install_path: Option<String>,
    pub engines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaSourceHealth {
    pub ok: bool,
    pub error: Option<String>,
    pub count: usize,
}

impl NgwaSourceHealth {
    fn ok(count: usize) -> Self {
        NgwaSourceHealth { ok: true, error: None, count }
    }
    fn failed(error: impl Into<String>) -> Self {
        NgwaSourceHealth { ok: false, error: Some(error.into()), count: 0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaSourcesHealth {
    pub kernel: NgwaSourceHealth,
    pub oba: NgwaSourceHealth,
    pub engine_config: NgwaSourceHealth,
    pub engine_assets: NgwaSourceHealth,
    pub trust: NgwaSourceHealth,
    pub usage: NgwaSourceHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NgwaSnapshot {
    pub items: Vec<NgwaItem>,
    pub as_of_ms: i64,
    pub sources: NgwaSourcesHealth,
}

// ── Inputs to the pure join ──────────────────────────────────────────────────

/// One installed pkg, with its manifest and trust pre-loaded by the caller.
pub struct PkgInput {
    pub summary: InstalledSummary,
    /// `Err` = the manifest could not be read or parsed (item state `broken`).
    pub manifest: Result<Manifest, String>,
    /// `Err` = trust could not be evaluated (item trust `not_applicable`,
    /// and `sources.trust` reports the failure).
    pub trust: Result<TrustState, String>,
}

/// Transcript usage, already scanned and loaded.
pub struct UsageInput {
    pub snapshot: UsageSnapshot,
    pub error: Option<String>,
}

impl UsageInput {
    pub fn unavailable(error: impl Into<String>) -> Self {
        UsageInput {
            snapshot: UsageSnapshot::unavailable(),
            error: Some(error.into()),
        }
    }
}

/// Everything [`build_snapshot`] joins. No Tauri state.
pub struct SnapshotInputs {
    pub now_ms: i64,
    /// Registered projects: (project id, root path).
    pub projects: Vec<(String, String)>,
    pub pkgs: Vec<PkgInput>,
    /// `KernelStatus.registries` — the `engine_assets` and `sidecar_supervisor`
    /// snapshots are read from here.
    pub registries: HashMap<String, serde_json::Value>,
    pub oba: Result<Vec<ClaudeStoreEntry>, String>,
    pub config: Result<ClaudeConfig, String>,
    /// Pkg ids with a pending capability-diff review.
    pub trust_pending: Result<HashSet<String>, String>,
    pub usage: UsageInput,
}

// ── Pure helpers ─────────────────────────────────────────────────────────────

/// Normalize a filesystem path for use in ids and prefix matching: forward
/// slashes, lowercased drive letter, no trailing slash.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        let lower = s[..1].to_ascii_lowercase();
        s.replace_range(..1, &lower);
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

fn path_is_under(path: &str, root: &str) -> bool {
    let p = normalize_path(path);
    let r = normalize_path(root);
    !r.is_empty() && (p == r || p.starts_with(&format!("{r}/")))
}

fn engine_str(e: EngineId) -> String {
    serde_json::to_value(e)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Pkg MCP servers are registered into `~/.claude.json` under
/// `pkg-<slug>-<server>` (`pkg/registries/mcp.rs`, `McpRegistry::key_for`), and
/// that key is what appears in transcript tool names.
fn pkg_mcp_server_key(pkg_id: &str, server: &str) -> String {
    format!("pkg-{}-{}", pkg_id.replace('.', "-"), server)
}

/// Kind priority for a pkg (Round 13 / D-02): `engine` → `app` (any `ui`
/// block) → `tool` (`mcp[]`) → `sidecar`. `requires[]` never makes a pkg a
/// `bundle`. A manifest with none of these blocks falls back to `app`.
pub fn pkg_kind(m: &Manifest) -> NgwaKind {
    if m.engine.is_some() {
        NgwaKind::Engine
    } else if m.ui.is_some() {
        NgwaKind::App
    } else if !m.mcp.is_empty() {
        NgwaKind::Tool
    } else if !m.sidecars.is_empty() {
        NgwaKind::Sidecar
    } else {
        NgwaKind::App
    }
}

/// Resolve every `requires[]` edge to an item id and invert the graph into
/// `required_by[]` (gate §2: computed per snapshot, never stored).
///
/// A ref resolves on `(kind, name)` — `mcp` refs match `tool` items — and
/// prefers a target in the requirer's own scope, then `personal`, then the
/// lowest id. Unresolvable refs keep `item_id: null`.
pub fn compute_required_by(items: &mut [NgwaItem]) {
    let mut by_key: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, it) in items.iter().enumerate() {
        by_key
            .entry((it.kind.as_str().to_string(), it.name.clone()))
            .or_default()
            .push(i);
    }
    let mut reverse: Vec<Vec<NgwaRef>> = vec![Vec::new(); items.len()];
    for i in 0..items.len() {
        let from_kind = items[i].kind.as_str().to_string();
        let from_name = items[i].name.clone();
        let from_id = items[i].id.clone();
        let from_source = items[i].origin.source;
        let from_scope = items[i].scope.clone();
        for r in 0..items[i].requires.len() {
            let want_kind = match items[i].requires[r].kind.as_str() {
                "mcp" => "tool".to_string(),
                k => k.to_string(),
            };
            let want = (want_kind, items[i].requires[r].name.clone());
            let target = by_key.get(&want).and_then(|cands| {
                let cands: Vec<usize> = cands.iter().copied().filter(|c| *c != i).collect();
                cands
                    .iter()
                    .copied()
                    .find(|c| items[*c].scope == from_scope)
                    .or_else(|| cands.iter().copied().find(|c| items[*c].scope == NgwaScope::Personal))
                    .or_else(|| cands.iter().copied().min_by(|a, b| items[*a].id.cmp(&items[*b].id)))
            });
            items[i].requires[r].item_id = target.map(|t| items[t].id.clone());
            if let Some(t) = target {
                reverse[t].push(NgwaRef {
                    kind: from_kind.clone(),
                    name: from_name.clone(),
                    item_id: Some(from_id.clone()),
                    source: Some(from_source),
                    r#ref: None,
                });
            }
        }
    }
    for (t, mut refs) in reverse.into_iter().enumerate() {
        refs.sort_by(|a, b| a.item_id.cmp(&b.item_id));
        refs.dedup_by(|a, b| a.item_id == b.item_id);
        items[t].required_by = refs;
    }
}

/// Ids that occur more than once, sorted. Empty for a valid snapshot.
pub fn duplicate_ids(items: &[NgwaItem]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut dups: Vec<String> = items
        .iter()
        .filter(|it| !seen.insert(it.id.as_str()))
        .map(|it| it.id.clone())
        .collect();
    dups.sort();
    dups.dedup();
    dups
}

/// One normalized config-scan row, whichever entry struct it came from.
struct ScanRow {
    kind: NgwaKind,
    prim: PrimitiveKind,
    name: String,
    scope: NgwaScope,
    tag: SystemTag,
    path: String,
    link_target: Option<String>,
    in_store: bool,
    present: bool,
    overridden_by: Option<String>,
    description: Option<String>,
    url: Option<String>,
}

struct AssetRow {
    pkg_id: String,
    target: String,
}

fn resolve_scope(
    scope: ConfigScope,
    project_root: Option<&str>,
    project_ids: &HashMap<String, String>,
) -> NgwaScope {
    match scope {
        ConfigScope::Personal => NgwaScope::Personal,
        ConfigScope::Project => {
            let norm = normalize_path(project_root.unwrap_or(""));
            let project_id = project_ids.get(&norm).cloned().unwrap_or(norm);
            NgwaScope::Project { project_id }
        }
    }
}

fn scan_rows(config: &ClaudeConfig, project_ids: &HashMap<String, String>) -> Vec<ScanRow> {
    let mut rows = Vec::new();
    for a in &config.agents {
        rows.push(ScanRow {
            kind: NgwaKind::Agent,
            prim: PrimitiveKind::Agent,
            name: a.name.clone(),
            scope: resolve_scope(a.scope, a.project_root.as_deref(), project_ids),
            tag: a.tag,
            path: a.path.clone(),
            link_target: a.link_target.clone(),
            in_store: a.in_store,
            present: a.target_exists,
            overridden_by: a.overridden_by.clone(),
            description: a.description.clone(),
            url: None,
        });
    }
    for s in &config.skills {
        rows.push(ScanRow {
            kind: NgwaKind::Skill,
            prim: PrimitiveKind::Skill,
            name: s.name.clone(),
            scope: resolve_scope(s.scope, s.project_root.as_deref(), project_ids),
            tag: s.tag,
            path: s.path.clone(),
            link_target: s.link_target.clone(),
            in_store: s.in_store,
            present: s.target_exists,
            overridden_by: s.overridden_by.clone(),
            description: s.description.clone(),
            url: None,
        });
    }
    for c in &config.commands {
        rows.push(ScanRow {
            kind: NgwaKind::Command,
            prim: PrimitiveKind::Command,
            name: c.name.clone(),
            scope: resolve_scope(c.scope, c.project_root.as_deref(), project_ids),
            tag: c.tag,
            path: c.path.clone(),
            link_target: c.link_target.clone(),
            in_store: c.in_store,
            present: c.target_exists,
            overridden_by: c.overridden_by.clone(),
            description: c.description.clone(),
            url: None,
        });
    }
    for h in &config.hooks {
        rows.push(ScanRow {
            kind: NgwaKind::Hook,
            prim: PrimitiveKind::Hook,
            name: h.name.clone(),
            scope: resolve_scope(h.scope, h.project_root.as_deref(), project_ids),
            tag: h.tag,
            path: h.settings_path.clone(),
            link_target: None,
            in_store: false,
            present: h.target_exists,
            overridden_by: None,
            description: Some(format!("Event: {}", h.event)),
            url: None,
        });
    }
    for m in &config.mcps {
        rows.push(ScanRow {
            kind: NgwaKind::Tool,
            prim: PrimitiveKind::Mcp,
            name: m.name.clone(),
            scope: resolve_scope(m.scope, m.project_root.as_deref(), project_ids),
            tag: m.tag,
            path: m.path.clone(),
            link_target: None,
            in_store: false,
            present: m.target_exists,
            overridden_by: None,
            description: Some(format!("Transport: {}", m.transport)),
            url: m.url.clone(),
        });
    }
    rows
}

/// Build a placement. `mechanism` / `format` / `status` come from the frozen
/// `EngineLayout` cell for (engine, kind) — gate §3, G-ADAPTER (F-6). The
/// scan's own `SystemTag` is used only if an engine has no cell for the kind.
fn placement_for(
    row: &ScanRow,
    layouts: &[EngineLayout],
    assets: &[AssetRow],
) -> (NgwaPlacement, Option<String>) {
    let cell = layouts
        .iter()
        .find(|l| l.engine == row.tag.system)
        .and_then(|l| l.kinds.get(&row.prim));
    let (mechanism, format, status) = match cell {
        Some(c) => (c.mechanism.into(), c.format.into(), c.status.into()),
        None => (NgwaMechanism::File, row.tag.format.into(), row.tag.status.into()),
    };
    let owner = assets
        .iter()
        .find(|a| path_is_under(&row.path, &a.target))
        .map(|a| a.pkg_id.clone());
    let managed_by = if row.in_store {
        NgwaManagedBy::Oba
    } else if owner.is_some() {
        NgwaManagedBy::Pkg
    } else {
        NgwaManagedBy::User
    };
    (
        NgwaPlacement {
            engine: engine_str(row.tag.system),
            scope: row.scope.clone(),
            path: row.path.clone(),
            mechanism,
            present: row.present,
            link_target: row.link_target.clone(),
            in_store: row.in_store,
            managed_by,
            overridden_by: row.overridden_by.clone(),
            format: Some(format),
            status,
        },
        owner,
    )
}

fn parse_engine_assets(
    registries: &HashMap<String, serde_json::Value>,
) -> (Vec<AssetRow>, NgwaSourceHealth) {
    let Some(v) = registries.get("engine_assets") else {
        return (
            Vec::new(),
            NgwaSourceHealth::failed("engine_assets registry missing from kernel status"),
        );
    };
    let Some(entries) = v.get("entries").and_then(|e| e.as_array()) else {
        return (
            Vec::new(),
            NgwaSourceHealth::failed("engine_assets registry snapshot has no `entries` array"),
        );
    };
    let rows: Vec<AssetRow> = entries
        .iter()
        .filter_map(|e| {
            let pkg_id = e.get("pkg_id")?.as_str()?.to_string();
            let target = e.get("target")?.as_str()?.to_string();
            (!target.is_empty()).then_some(AssetRow { pkg_id, target })
        })
        .collect();
    (rows, NgwaSourceHealth::ok(entries.len()))
}

fn parse_supervisor(
    registries: &HashMap<String, serde_json::Value>,
) -> Result<HashMap<String, NgwaRuntime>, String> {
    let v = registries
        .get("sidecar_supervisor")
        .ok_or_else(|| "sidecar_supervisor registry missing from kernel status".to_string())?;
    let entries = v
        .get("entries")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "sidecar_supervisor snapshot has no `entries` array".to_string())?;
    let mut out = HashMap::new();
    for e in entries {
        let Some(pkg_id) = e.get("pkg_id").and_then(|v| v.as_str()) else { continue };
        let state = e.get("state").and_then(|v| v.as_str()).unwrap_or("");
        out.insert(
            pkg_id.to_string(),
            NgwaRuntime {
                state: NgwaRuntimeState::from_supervisor(state),
                pid: e.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32),
                uptime_s: e.get("uptime_s").and_then(|v| v.as_u64()),
                restarts: e.get("restarts").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                last_err: e.get("last_err").and_then(|v| v.as_str()).map(str::to_string),
                last_crash_ms: e.get("last_crash_unix_ms").and_then(|v| v.as_i64()),
            },
        );
    }
    Ok(out)
}

fn usage_for(kind: NgwaKind, name: &str, snap: &UsageSnapshot) -> Option<NgwaUsage> {
    match kind {
        NgwaKind::Skill => snap.for_primitive(UsageKind::Skill, name).map(Into::into),
        NgwaKind::Agent => snap.for_primitive(UsageKind::Agent, name).map(Into::into),
        NgwaKind::Tool => snap.for_servers(&[name.to_string()]).map(Into::into),
        // command, hook, schedule, bundle, workflow: not measurable from transcripts.
        _ => None,
    }
}

fn engines_of(placements: &[NgwaPlacement]) -> Vec<String> {
    let mut e: Vec<String> = placements.iter().map(|p| p.engine.clone()).collect();
    e.sort();
    e.dedup();
    e
}

fn sort_placements(p: &mut Vec<NgwaPlacement>) {
    p.sort_by(|a, b| (&a.engine, &a.path).cmp(&(&b.engine, &b.path)));
    p.dedup();
}

fn map_trust(state: &TrustState, signed: bool, review_pending: bool, m: &Manifest) -> NgwaTrust {
    let (s, auto_trusted, last) = match state {
        TrustState::AutoTrusted => (NgwaTrustState::AutoTrusted, true, None),
        TrustState::AutoGranted => (NgwaTrustState::AutoGranted, false, None),
        TrustState::Granted { granted_at_ms, .. } => (NgwaTrustState::Granted, false, Some(*granted_at_ms)),
        TrustState::NeedsApproval { .. } => (NgwaTrustState::NeedsApproval, false, None),
    };
    let p = trust::summarize_sensitive(&m.permissions);
    NgwaTrust {
        state: s,
        signed,
        auto_trusted,
        review_pending,
        perms: Some(NgwaPermsSummary {
            shell_execute: p.shell_execute,
            fs_write_outside_sandbox: p.fs_write_outside_sandbox,
            net: p.net,
            vault_keys: p.vault_keys,
        }),
        last_granted_at_ms: last,
    }
}

// ── The join ─────────────────────────────────────────────────────────────────

/// The pure join: every subsystem's output in, one `NgwaSnapshot` out.
/// Deterministic for deterministic inputs (items sorted by id, placements by
/// (engine, path)), which is what makes the golden file possible.
pub fn build_snapshot(inputs: SnapshotInputs) -> NgwaSnapshot {
    let SnapshotInputs {
        now_ms,
        projects,
        pkgs,
        registries,
        oba,
        config,
        trust_pending,
        usage,
    } = inputs;
    let snap = &usage.snapshot;

    let project_ids: HashMap<String, String> = projects
        .iter()
        .map(|(id, root)| (normalize_path(root), id.clone()))
        .collect();

    let (assets, engine_assets_health) = parse_engine_assets(&registries);
    let supervisor = parse_supervisor(&registries);
    let layouts = engine_layouts();

    let mut items: Vec<NgwaItem> = Vec::new();

    // ── Config-scan placements, grouped by (kind, scope, name) (F-5) ──
    let (rows, config_health) = match &config {
        Ok(c) => {
            let rows = scan_rows(c, &project_ids);
            let health = NgwaSourceHealth::ok(rows.len());
            (rows, health)
        }
        Err(e) => (Vec::new(), NgwaSourceHealth::failed(e.clone())),
    };
    struct Group {
        scope: NgwaScope,
        placements: Vec<NgwaPlacement>,
        owner: Option<String>,
        descriptions: Vec<String>,
        url: Option<String>,
    }
    let mut groups: BTreeMap<(NgwaKind, String, String), Group> = BTreeMap::new();
    for row in &rows {
        let (placement, owner) = placement_for(row, &layouts, &assets);
        let g = groups
            .entry((row.kind, row.scope.key(), row.name.clone()))
            .or_insert_with(|| Group {
                scope: row.scope.clone(),
                placements: Vec::new(),
                owner: None,
                descriptions: Vec::new(),
                url: None,
            });
        g.placements.push(placement);
        if g.owner.is_none() {
            g.owner = owner;
        }
        if let Some(d) = &row.description {
            if !g.descriptions.contains(d) {
                g.descriptions.push(d.clone());
            }
        }
        if g.url.is_none() {
            g.url = row.url.clone();
        }
    }

    // ── Ọba entries: personal-scope items that absorb the personal group ──
    let (oba_entries, oba_health) = match oba {
        Ok(v) => {
            let n = v.len();
            (v, NgwaSourceHealth::ok(n))
        }
        Err(e) => (Vec::new(), NgwaSourceHealth::failed(e)),
    };
    let mut oba_by_key: HashMap<(NgwaKind, String), &ClaudeStoreEntry> = HashMap::new();
    for entry in &oba_entries {
        let Some(kind) = NgwaKind::from_store_kind(&entry.kind) else {
            log::warn!("[ngwa_snapshot] Ọba entry `{}` has unknown kind `{}`; skipped", entry.name, entry.kind);
            continue;
        };
        oba_by_key.insert((kind, entry.name.clone()), entry);
        let group = groups.remove(&(kind, "personal".to_string(), entry.name.clone()));
        let (mut placements, owner) = group
            .map(|g| (g.placements, g.owner))
            .unwrap_or_default();
        sort_placements(&mut placements);
        items.push(NgwaItem {
            id: format!("{}:personal:{}", kind.as_str(), entry.name),
            kind,
            name: entry.name.clone(),
            display_name: entry.name.clone(),
            description: entry.description.clone(),
            version: entry.provenance.version.clone(),
            latest_version: None,
            scope: NgwaScope::Personal,
            origin: NgwaOrigin {
                source: NgwaSource::from(&entry.provenance.source),
                url: entry.provenance.url.clone(),
                r#ref: entry.provenance.r#ref.clone(),
                resolved_version: entry.provenance.version.clone(),
                publisher: None,
                managed: entry.provenance.managed,
                auto_update: entry.provenance.auto_update,
                installed_at_ms: Some(entry.modified_ms),
                updated_at_ms: None,
            },
            state: if entry.enabled_in.is_empty() {
                NgwaState::Disabled
            } else {
                NgwaState::Enabled
            },
            runtime: None,
            trust: NgwaTrust::not_applicable(),
            engines: engines_of(&placements),
            placements,
            usage: usage_for(kind, &entry.name, snap),
            requires: entry.requires.iter().map(NgwaRef::from).collect(),
            required_by: Vec::new(),
            owner_pkg_id: owner,
            install_path: Some(entry.store_path.clone()),
        });
    }

    // ── Remaining groups: project placements, and anything Ọba does not hold ──
    for ((kind, _scope_key, name), mut g) in groups {
        sort_placements(&mut g.placements);
        let from_store = oba_by_key
            .get(&(kind, name.clone()))
            .filter(|_| g.placements.iter().any(|p| p.in_store));
        let (description, version, origin) = match from_store {
            // A project placement linked into the vault shows the vault's provenance.
            Some(e) => (
                e.description.clone(),
                e.provenance.version.clone(),
                NgwaOrigin {
                    source: NgwaSource::from(&e.provenance.source),
                    url: e.provenance.url.clone(),
                    r#ref: e.provenance.r#ref.clone(),
                    resolved_version: e.provenance.version.clone(),
                    publisher: None,
                    managed: e.provenance.managed,
                    auto_update: e.provenance.auto_update,
                    installed_at_ms: Some(e.modified_ms),
                    updated_at_ms: None,
                },
            ),
            None => {
                let mut origin = NgwaOrigin::local();
                origin.url = g.url.clone();
                let desc = (!g.descriptions.is_empty()).then(|| g.descriptions.join("; "));
                (desc, None, origin)
            }
        };
        items.push(NgwaItem {
            id: format!("{}:{}:{}", kind.as_str(), g.scope.key(), name),
            kind,
            display_name: name.clone(),
            usage: usage_for(kind, &name, snap),
            name,
            description,
            version,
            latest_version: None,
            scope: g.scope,
            origin,
            state: NgwaState::Enabled,
            runtime: None,
            trust: NgwaTrust::not_applicable(),
            engines: engines_of(&g.placements),
            placements: g.placements,
            requires: Vec::new(),
            required_by: Vec::new(),
            owner_pkg_id: g.owner,
            install_path: None,
        });
    }

    // ── Kernel pkgs ──
    let kernel_count = pkgs.len();
    let kernel_health = match &supervisor {
        Ok(_) => NgwaSourceHealth::ok(kernel_count),
        Err(e) => NgwaSourceHealth {
            ok: false,
            error: Some(e.clone()),
            count: kernel_count,
        },
    };
    let supervisor = supervisor.unwrap_or_default();
    let (pending, mut trust_errors) = match trust_pending {
        Ok(p) => (p, Vec::new()),
        Err(e) => (HashSet::new(), vec![format!("pending reviews: {e}")]),
    };
    let trust_count = pending.len();

    for p in pkgs {
        let s = p.summary;
        let manifest = p.manifest.as_ref().ok();
        if let Err(e) = &p.manifest {
            log::warn!("[ngwa_snapshot] manifest for `{}` unreadable: {e}", s.id);
        }
        let scope = match &s.project_id {
            Some(pid) if !pid.is_empty() => NgwaScope::Project { project_id: pid.clone() },
            _ => NgwaScope::Personal,
        };
        let (source, url, publisher) = match &s.source {
            InstallSource::Builtin => (NgwaSource::Builtin, None, None),
            InstallSource::Registry { url, publisher_key } => {
                (NgwaSource::Registry, Some(url.clone()), publisher_key.clone())
            }
            InstallSource::Local { .. } => (NgwaSource::Local, None, None),
            InstallSource::Dev { .. } => (NgwaSource::Dev, None, None),
        };
        let origin = NgwaOrigin {
            source,
            url,
            r#ref: None,
            resolved_version: None,
            publisher,
            managed: !matches!(s.source, InstallSource::Builtin),
            auto_update: matches!(s.source, InstallSource::Registry { .. }),
            installed_at_ms: Some(s.installed_at),
            updated_at_ms: None,
        };
        let state = if !s.enabled {
            NgwaState::Disabled
        } else if !s.compatible || manifest.is_none() {
            NgwaState::Broken
        } else {
            NgwaState::Enabled
        };
        let trust = match (manifest, &p.trust) {
            (Some(m), Ok(t)) => map_trust(t, m.signature.is_some(), pending.contains(&s.id), m),
            (Some(_), Err(e)) => {
                trust_errors.push(format!("{}: {e}", s.id));
                NgwaTrust::not_applicable()
            }
            // No manifest: the item is already `broken`; trust was never evaluable.
            (None, _) => NgwaTrust::not_applicable(),
        };
        let usage = manifest.and_then(|m| {
            if m.mcp.is_empty() {
                None // app/engine/sidecar without an MCP server reads null
            } else {
                let keys: Vec<String> = m
                    .mcp
                    .iter()
                    .map(|srv| pkg_mcp_server_key(&s.id, &srv.name))
                    .collect();
                snap.for_servers(&keys).map(Into::into)
            }
        });
        items.push(NgwaItem {
            id: s.id.clone(),
            kind: manifest.map(pkg_kind).unwrap_or(NgwaKind::App),
            name: s.id.clone(),
            display_name: manifest.map(|m| m.name.clone()).unwrap_or_else(|| s.id.clone()),
            description: manifest.and_then(|m| m.description.clone()),
            version: Some(s.version.clone()),
            latest_version: None,
            scope: scope.clone(),
            origin: origin.clone(),
            state,
            runtime: supervisor.get(&s.id).cloned(),
            trust,
            placements: Vec::new(),
            usage,
            requires: manifest
                .map(|m| m.requires.iter().map(NgwaRef::from).collect())
                .unwrap_or_default(),
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: Some(s.install_path.clone()),
            engines: Vec::new(),
        });

        // Schedules from manifest cron[] only (gate §6 decision).
        if let Some(m) = manifest {
            for cron in &m.cron {
                items.push(NgwaItem {
                    id: format!("schedule:{}:{}:{}", scope.key(), s.id, cron.id),
                    kind: NgwaKind::Schedule,
                    name: cron.id.clone(),
                    display_name: cron.id.clone(),
                    description: Some(format!("Expr: {} | Handler: {}", cron.expr, cron.handler)),
                    version: None,
                    latest_version: None,
                    scope: scope.clone(),
                    origin: origin.clone(),
                    state,
                    runtime: None,
                    trust: NgwaTrust::not_applicable(),
                    placements: Vec::new(),
                    usage: None,
                    requires: Vec::new(),
                    required_by: Vec::new(),
                    owner_pkg_id: Some(s.id.clone()),
                    install_path: None,
                    engines: Vec::new(),
                });
            }
        }
    }

    compute_required_by(&mut items);
    items.sort_by(|a, b| a.id.cmp(&b.id));

    let dups = duplicate_ids(&items);
    debug_assert!(dups.is_empty(), "ngwa_snapshot produced duplicate ids: {dups:?}");
    if !dups.is_empty() {
        log::error!("[ngwa_snapshot] duplicate item ids: {dups:?}");
    }

    let trust_health = if trust_errors.is_empty() {
        NgwaSourceHealth::ok(trust_count)
    } else {
        NgwaSourceHealth {
            ok: false,
            error: Some(trust_errors.join("; ")),
            count: trust_count,
        }
    };
    let usage_health = NgwaSourceHealth {
        ok: snap.is_available(),
        error: usage.error.clone(),
        count: snap.total_sessions(),
    };

    NgwaSnapshot {
        items,
        as_of_ms: now_ms,
        sources: NgwaSourcesHealth {
            kernel: kernel_health,
            oba: oba_health,
            engine_config: config_health,
            engine_assets: engine_assets_health,
            trust: trust_health,
            usage: usage_health,
        },
    }
}

// ── Input collection ─────────────────────────────────────────────────────────

/// Scan the transcript corpus at `root` and load the mirror. Absent corpus or
/// any scan/load failure yields an *unavailable* snapshot (F-1): every item's
/// `usage` then reads `null`, never a zero.
pub async fn collect_usage(pool: &sqlx::SqlitePool, root: Option<PathBuf>, now_ms: i64) -> UsageInput {
    let Some(root) = root else {
        return UsageInput::unavailable("cannot resolve the home directory");
    };
    if !root.is_dir() {
        return UsageInput::unavailable(format!(
            "transcript corpus not found at {}",
            root.display()
        ));
    }
    match usage::scan_and_mirror_transcripts(pool, &root).await {
        Err(e) => {
            log::warn!("[ngwa_snapshot] transcript scan failed: {e}");
            UsageInput::unavailable(format!("transcript scan failed: {e}"))
        }
        Ok(report) => match usage::load_usage_snapshot(pool, now_ms).await {
            Ok(snapshot) => UsageInput {
                snapshot,
                error: report.error_summary(),
            },
            Err(e) => {
                log::warn!("[ngwa_snapshot] load usage snapshot failed: {e}");
                UsageInput::unavailable(e)
            }
        },
    }
}

#[tauri::command]
pub async fn ngwa_snapshot(
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<PaDb>>,
    app: AppHandle,
) -> Result<NgwaSnapshot, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("resolve app_data_dir: {e}"))?;
    ngwa_snapshot_inner(
        kernel.0.status(),
        db.inner(),
        &app_data_dir,
        usage::claude_projects_dir(),
    )
    .await
}

/// The snapshot data path shared by the `ngwa_snapshot` command and the
/// `GET /iyke/ngwa/snapshot` bridge route (WP-28 — iyke handlers have no
/// Tauri `State`, so both call this one function rather than duplicating the
/// join). All inputs are resolved by the caller: `status` is
/// `kernel.status()`, `app_data_dir` anchors trust evaluation, and
/// `transcript_root` is the `~/.claude/projects` corpus
/// (`usage::claude_projects_dir()` in production — tests pass a fixture dir
/// so the cold 130s-scale corpus scan never runs in the harness).
pub async fn ngwa_snapshot_inner(
    status: KernelStatus,
    db: &Arc<PaDb>,
    app_data_dir: &PathBuf,
    transcript_root: Option<PathBuf>,
) -> Result<NgwaSnapshot, String> {
    let now = usage::now_ms();
    let pool = db.ensure_pool().await?;

    let usage = collect_usage(&pool, transcript_root, now).await;

    let projects: Vec<(String, String)> = projects::list_projects(&pool, false)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| p.root_path.map(|r| (p.id, r)))
        .collect();
    let project_roots: Vec<String> = projects.iter().map(|(_, r)| r.clone()).collect();

    let config = claude_config::claude_config_load(project_roots).await;
    let trust_pending =
        crate::commands::pkg_trust::pending_trust_reviews(&pool, &status.installed)
            .await
            .map(|v| v.into_iter().map(|r| r.pkg_id).collect::<HashSet<_>>());
    let oba = claude_store::claude_store_list_inner(db, None).await;

    let mut pkgs = Vec::with_capacity(status.installed.len());
    for summary in status.installed {
        let install_path = PathBuf::from(&summary.install_path);
        let loaded = tokio::task::spawn_blocking(move || Package::load(&install_path))
            .await
            .map_err(|e| format!("manifest load task failed: {e}"))
            .and_then(|r| r.map_err(|e| format!("{e:#}")));
        let (manifest, trust) = match loaded {
            Ok(pkg) => {
                let t = trust::evaluate(&pool, &pkg, &summary.source, app_data_dir)
                    .await
                    .map_err(|e| format!("{e:#}"));
                (Ok(pkg.manifest), t)
            }
            Err(e) => (Err(e), Err("manifest not loaded".to_string())),
        };
        pkgs.push(PkgInput { summary, manifest, trust });
    }

    Ok(build_snapshot(SnapshotInputs {
        now_ms: now,
        projects,
        pkgs,
        registries: status.registries,
        oba,
        config,
        trust_pending,
        usage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::claude_config::{
        AgentEntry, CommandEntry, HookEntry, McpEntry, SkillEntry,
    };
    use serde_json::json;

    const NOW: i64 = 1_790_000_000_000;
    const DAY: i64 = 86_400_000;
    const GOLDEN: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../src/lib/ngwa/__fixtures__/ngwa-snapshot.golden.json"
    );

    fn tag(system: EngineId, format: ConfigFormat) -> SystemTag {
        SystemTag { system, format, status: KindStatus::Active }
    }

    fn skill(
        name: &str,
        scope: ConfigScope,
        root: Option<&str>,
        path: &str,
        in_store: bool,
        overridden_by: Option<&str>,
        system: EngineId,
    ) -> SkillEntry {
        SkillEntry {
            name: name.into(),
            scope,
            project_root: root.map(Into::into),
            path: path.into(),
            dir_path: path.trim_end_matches("/SKILL.md").into(),
            modified_ms: NOW - DAY,
            description: Some(format!("{name} skill")),
            frontmatter: json!({}),
            body: String::new(),
            supporting_files: Vec::new(),
            overridden_by: overridden_by.map(Into::into),
            is_symlink: in_store,
            link_target: in_store.then(|| format!("/home/x/.ikenga/store/skills/{name}")),
            in_store,
            target_exists: true,
            tag: tag(system, ConfigFormat::MdYaml),
        }
    }

    fn config_fixture() -> ClaudeConfig {
        ClaudeConfig {
            agents: vec![AgentEntry {
                name: "reviewer".into(),
                scope: ConfigScope::Personal,
                project_root: None,
                path: "/home/x/.claude/agents/reviewer.md".into(),
                modified_ms: NOW - DAY,
                description: Some("Reviews diffs".into()),
                model: None,
                frontmatter: json!({}),
                body: String::new(),
                overridden_by: None,
                is_symlink: true,
                link_target: Some("/home/x/.ikenga/store/agents/reviewer.md".into()),
                in_store: true,
                target_exists: true,
                tag: tag(EngineId::Claude, ConfigFormat::MdYaml),
            }],
            skills: vec![
                skill(
                    "groundwork",
                    ConfigScope::Personal,
                    None,
                    "/home/x/.claude/skills/groundwork/SKILL.md",
                    true,
                    Some("C:/Users/x/royalti-co/.claude/skills/groundwork/SKILL.md"),
                    EngineId::Claude,
                ),
                skill(
                    "groundwork",
                    ConfigScope::Personal,
                    None,
                    "/home/x/.gemini/skills/groundwork/SKILL.md",
                    true,
                    None,
                    EngineId::Gemini,
                ),
                skill(
                    "groundwork",
                    ConfigScope::Project,
                    Some("C:\\Users\\x\\royalti-co\\"),
                    "C:/Users/x/royalti-co/.claude/skills/groundwork/SKILL.md",
                    true,
                    None,
                    EngineId::Claude,
                ),
                skill(
                    "com-ikenga-iyke",
                    ConfigScope::Personal,
                    None,
                    "/home/x/.claude/skills/com-ikenga-iyke/SKILL.md",
                    false,
                    None,
                    EngineId::Claude,
                ),
            ],
            commands: vec![CommandEntry {
                name: "ship".into(),
                scope: ConfigScope::Personal,
                project_root: None,
                path: "/home/x/.claude/commands/ship.md".into(),
                modified_ms: NOW - DAY,
                description: Some("Ship it".into()),
                model: None,
                argument_hint: None,
                frontmatter: json!({}),
                body: String::new(),
                overridden_by: None,
                is_symlink: false,
                link_target: None,
                in_store: false,
                target_exists: true,
                tag: tag(EngineId::Claude, ConfigFormat::MdYaml),
            }],
            hooks: vec![HookEntry {
                event: "PreToolUse".into(),
                kind: "command".into(),
                name: "secret-scan.sh".into(),
                scope: ConfigScope::Personal,
                project_root: None,
                settings_path: "/home/x/.claude/settings.json".into(),
                command_path: Some("/home/x/.claude/hooks/secret-scan.sh".into()),
                command_raw: Some("~/.claude/hooks/secret-scan.sh".into()),
                raw: json!({}),
                is_symlink: false,
                link_target: None,
                in_store: false,
                target_exists: true,
                tag: tag(EngineId::Claude, ConfigFormat::JsonEmbedded),
            }],
            mcps: vec![McpEntry {
                name: "Claude Browser".into(),
                scope: ConfigScope::Personal,
                project_root: None,
                path: "/home/x/.claude.json".into(),
                transport: "http".into(),
                command: None,
                args: Vec::new(),
                env_keys: Vec::new(),
                url: Some("http://localhost:7000/mcp".into()),
                header_keys: Vec::new(),
                raw: json!({}),
                is_symlink: false,
                link_target: None,
                in_store: false,
                target_exists: true,
                tag: tag(EngineId::Claude, ConfigFormat::JsonEmbedded),
            }],
            errors: Vec::new(),
        }
    }

    fn oba_fixture() -> Vec<ClaudeStoreEntry> {
        serde_json::from_value(json!([
            {
                "kind": "skill", "name": "groundwork",
                "storePath": "/home/x/.ikenga/store/skills/groundwork",
                "description": "Plan scaffolding", "modifiedMs": NOW - 20 * DAY,
                "enabledIn": ["workspace", "project:proj-royalti"],
                "source": "git", "url": "https://github.com/ikenga-hq/groundwork",
                "ref": "v1.2.0", "version": "abc1234",
                "canonicalPath": "/home/x/.ikenga/store/skills/groundwork",
                "managed": true, "autoUpdate": true
            },
            {
                "kind": "agent", "name": "reviewer",
                "storePath": "/home/x/.ikenga/store/agents/reviewer.md",
                "description": "Reviews diffs", "modifiedMs": NOW - 5 * DAY,
                "enabledIn": ["workspace"],
                "canonicalPath": "/home/x/.ikenga/store/agents/reviewer.md"
            },
            {
                "kind": "command", "name": "ship",
                "storePath": "/home/x/.ikenga/store/commands/ship.md",
                "description": "Ship it", "modifiedMs": NOW - 5 * DAY,
                "enabledIn": [],
                "canonicalPath": "/home/x/.ikenga/store/commands/ship.md"
            }
        ]))
        .expect("oba fixture")
    }

    fn manifest(v: serde_json::Value) -> Manifest {
        serde_json::from_value(v).expect("manifest fixture")
    }

    fn summary(id: &str, source: InstallSource, project_id: Option<&str>) -> InstalledSummary {
        InstalledSummary {
            id: id.into(),
            version: "1.0.0".into(),
            ikenga_api: "1".into(),
            install_path: format!("/home/x/.ikenga/pkgs/{id}"),
            enabled: true,
            installed_at: NOW - 30 * DAY,
            compatible: true,
            source,
            project_id: project_id.map(Into::into),
        }
    }

    fn engine_caps() -> serde_json::Value {
        json!({
            "streaming": true, "toolUse": true, "thinking": true, "artifacts": false,
            "fileAttachments": false, "imageInput": false, "slashCommands": true,
            "modelSwitching": true, "promptCaching": true, "agenticTools": true,
            "mcp": true, "sessionResume": true
        })
    }

    fn pkgs_fixture() -> Vec<PkgInput> {
        vec![
            // UI + sidecar + MCP → app (D-02: pkg-git is an app). Requires a skill.
            PkgInput {
                summary: summary(
                    "com.ikenga.git",
                    InstallSource::Registry {
                        url: "https://registry.ikenga.dev/index.json".into(),
                        publisher_key: Some("ed25519:abc".into()),
                    },
                    None,
                ),
                manifest: Ok(manifest(json!({
                    "id": "com.ikenga.git", "name": "Git", "version": "1.0.0", "ikenga_api": "1",
                    "description": "Git workbench",
                    "ui": { "routes": [] },
                    "sidecars": [{ "name": "pa-com-ikenga-git-daemon", "bin": "bin/daemon" }],
                    "mcp": [{ "name": "git", "command": "bin/mcp" }],
                    "cron": [{ "id": "fetch", "expr": "*/15 * * * *", "handler": "fetch" }],
                    "requires": [
                        { "kind": "skill", "name": "groundwork", "source": "git", "ref": "v1.2.0" },
                        { "kind": "skill", "name": "not-installed" }
                    ],
                    "permissions": { "shell.execute": ["git *"], "net": ["https://api.github.com/"] },
                    "signature": "sig"
                }))),
                trust: Ok(TrustState::Granted {
                    version: "1.0.0".into(),
                    granted_at_ms: NOW - 10 * DAY,
                }),
            },
            // Engine block wins over everything.
            PkgInput {
                summary: summary("com.ikenga.engine-claude-code", InstallSource::Builtin, None),
                manifest: Ok(manifest(json!({
                    "id": "com.ikenga.engine-claude-code", "name": "Claude Code", "version": "1.0.0",
                    "ikenga_api": "1",
                    "engine": { "agentId": "claude-code", "capabilities": engine_caps() }
                }))),
                trust: Ok(TrustState::AutoTrusted),
            },
            // MCP only → tool; a measured zero (server never called).
            PkgInput {
                summary: summary(
                    "com.example.lint",
                    InstallSource::Local { path: "/src/lint".into() },
                    Some("proj-royalti"),
                ),
                manifest: Ok(manifest(json!({
                    "id": "com.example.lint", "name": "Lint", "version": "1.0.0", "ikenga_api": "1",
                    "mcp": [{ "name": "lint", "command": "bin/lint" }]
                }))),
                trust: Ok(TrustState::AutoGranted),
            },
            // Sidecar only → sidecar; a pending capability review.
            PkgInput {
                summary: summary(
                    "com.example.watcher",
                    InstallSource::Dev { path: "/src/watcher".into() },
                    None,
                ),
                manifest: Ok(manifest(json!({
                    "id": "com.example.watcher", "name": "Watcher", "version": "1.0.0",
                    "ikenga_api": "1",
                    "sidecars": [{ "name": "pa-com-example-watcher-w", "bin": "bin/w" }]
                }))),
                trust: Ok(TrustState::AutoTrusted),
            },
            // Builtin that contributes the engine_assets skill folder.
            PkgInput {
                summary: summary("com.ikenga.iyke", InstallSource::Builtin, None),
                manifest: Ok(manifest(json!({
                    "id": "com.ikenga.iyke", "name": "Iyke", "version": "1.0.0", "ikenga_api": "1",
                    "ui": { "routes": [] }
                }))),
                trust: Ok(TrustState::AutoTrusted),
            },
        ]
    }

    fn registries_fixture() -> HashMap<String, serde_json::Value> {
        HashMap::from([
            (
                "engine_assets".to_string(),
                json!({ "count": 1, "entries": [{
                    "pkg_id": "com.ikenga.iyke", "engine_id": "claude-code", "kind": "skills",
                    "source": "/home/x/.ikenga/pkgs/com.ikenga.iyke/skills",
                    "target": "/home/x/.claude/skills/com-ikenga-iyke"
                }]}),
            ),
            (
                "sidecar_supervisor".to_string(),
                json!({ "count": 2, "entries": [
                    { "pkg_id": "com.ikenga.git", "state": "running", "pid": 4242, "uptime_s": 903,
                      "restarts": 1, "last_crash_unix_ms": NOW - 2 * DAY, "last_err": null },
                    { "pkg_id": "com.example.watcher", "state": "some-future-state", "pid": null,
                      "uptime_s": null, "restarts": 0, "last_crash_unix_ms": null, "last_err": "boom" }
                ]}),
            ),
        ])
    }

    fn usage_fixture() -> UsageSnapshot {
        UsageSnapshot::from_rows(
            NOW,
            NOW - 45 * DAY,
            vec![
                (UsageKind::Skill, "groundwork".into(), "s1".into(), NOW - DAY),
                (UsageKind::Skill, "groundwork".into(), "s2".into(), NOW - 12 * DAY),
                (UsageKind::Agent, "reviewer".into(), "a1".into(), NOW - 3 * DAY),
                (UsageKind::McpServer, "pkg-com-ikenga-git-git".into(), "s1".into(), NOW - DAY),
                (UsageKind::McpServer, "Claude_Browser".into(), "s2".into(), NOW - 40 * DAY),
            ],
            vec![
                (UsageKind::Skill, "groundwork".into(), 120_000),
                (UsageKind::Agent, "reviewer".into(), 45_000),
            ],
            vec![("pkg-com-ikenga-git-git".into(), "msg-1".into(), 9_000)],
        )
    }

    fn golden_inputs() -> SnapshotInputs {
        SnapshotInputs {
            now_ms: NOW,
            projects: vec![("proj-royalti".into(), "C:/Users/x/royalti-co".into())],
            pkgs: pkgs_fixture(),
            registries: registries_fixture(),
            oba: Ok(oba_fixture()),
            config: Ok(config_fixture()),
            trust_pending: Ok(HashSet::from(["com.example.watcher".to_string()])),
            usage: UsageInput { snapshot: usage_fixture(), error: None },
        }
    }

    fn item<'a>(s: &'a NgwaSnapshot, id: &str) -> &'a NgwaItem {
        s.items.iter().find(|i| i.id == id).unwrap_or_else(|| {
            panic!(
                "no item {id}; have {:?}",
                s.items.iter().map(|i| &i.id).collect::<Vec<_>>()
            )
        })
    }

    /// F-2: the producer's own output, serialized, must equal the committed
    /// golden byte-for-byte. The vitest DEC-26 parity test parses that file.
    /// `NGWA_UPDATE_GOLDEN=1` rewrites it.
    #[test]
    fn golden_snapshot_matches_committed_file() {
        let snap = build_snapshot(golden_inputs());
        let mut actual = serde_json::to_string_pretty(&snap).expect("serialize");
        actual.push('\n');
        if std::env::var("NGWA_UPDATE_GOLDEN").as_deref() == Ok("1") {
            std::fs::write(GOLDEN, &actual).expect("write golden");
            return;
        }
        let expected = std::fs::read_to_string(GOLDEN)
            .expect("golden missing — run with NGWA_UPDATE_GOLDEN=1");
        if actual != expected {
            let first_diff = actual
                .lines()
                .zip(expected.lines())
                .enumerate()
                .find(|(_, (a, e))| a != e)
                .map(|(n, (a, e))| format!("line {}:\n  actual:   {a}\n  expected: {e}", n + 1))
                .unwrap_or_else(|| "length differs".to_string());
            panic!(
                "ngwa_snapshot serialization drifted from the committed golden {GOLDEN}\n\
                 first difference at {first_diff}\n\
                 If intended, regenerate with NGWA_UPDATE_GOLDEN=1 and re-run the vitest parity test."
            );
        }
    }

    /// The golden must keep exercising what DEC-26 guards: every kind WP-14
    /// emits, both usage states, an override, and a dependency edge.
    #[test]
    fn golden_fixture_is_fully_populated() {
        let snap = build_snapshot(golden_inputs());
        let kinds: HashSet<NgwaKind> = snap.items.iter().map(|i| i.kind).collect();
        for k in [
            NgwaKind::App,
            NgwaKind::Engine,
            NgwaKind::Tool,
            NgwaKind::Sidecar,
            NgwaKind::Skill,
            NgwaKind::Agent,
            NgwaKind::Command,
            NgwaKind::Hook,
            NgwaKind::Schedule,
        ] {
            assert!(kinds.contains(&k), "golden has no {k:?} item");
        }
        assert!(snap.items.iter().any(|i| i.usage.is_some()));
        assert!(snap.items.iter().any(|i| i.usage.is_none()));
        assert!(snap
            .items
            .iter()
            .flat_map(|i| &i.placements)
            .any(|p| p.overridden_by.is_some()));
        assert!(snap.items.iter().any(|i| !i.required_by.is_empty()));
        assert!(snap.items.iter().all(|i| i.latest_version.is_none()));

        // Spot checks on the join.
        let git = item(&snap, "com.ikenga.git");
        assert_eq!(git.kind, NgwaKind::App, "UI + sidecar + MCP pkg is an app, not a bundle");
        assert_eq!(git.runtime.as_ref().map(|r| r.state), Some(NgwaRuntimeState::Running));
        let u = git.usage.as_ref().expect("pkg with MCP server is measured");
        assert_eq!((u.count_7d, u.count_30d, u.tokens_30d), (Some(1), Some(1), Some(9_000)));
        assert_eq!(
            item(&snap, "com.example.lint").usage.as_ref().and_then(|u| u.count_30d),
            Some(0)
        );
        assert_eq!(item(&snap, "com.example.lint").kind, NgwaKind::Tool);
        assert_eq!(item(&snap, "com.example.watcher").kind, NgwaKind::Sidecar);
        assert_eq!(item(&snap, "com.ikenga.engine-claude-code").kind, NgwaKind::Engine);
        assert!(item(&snap, "com.example.watcher").trust.review_pending);
        assert_eq!(
            item(&snap, "com.example.watcher").runtime.as_ref().map(|r| r.state),
            Some(NgwaRuntimeState::Stopped),
            "unknown supervisor state -> defined fallback"
        );
        for id in ["com.ikenga.engine-claude-code", "com.example.watcher", "com.ikenga.iyke"] {
            assert!(item(&snap, id).usage.is_none(), "{id}: no MCP server -> unmeasured");
        }
        assert!(item(&snap, "hook:personal:secret-scan.sh").usage.is_none());
        assert!(item(&snap, "command:personal:ship").usage.is_none());
        assert!(item(&snap, "schedule:personal:com.ikenga.git:fetch").usage.is_none());

        // Config-scan MCP key normalized the way Claude Code writes tool names.
        let browser = item(&snap, "tool:personal:Claude Browser");
        let bu = browser.usage.as_ref().expect("measured");
        assert_eq!((bu.count_30d, bu.last_used_ms), (Some(0), Some(NOW - 40 * DAY)));

        // engine_assets ownership via path prefix.
        let iyke_skill = item(&snap, "skill:personal:com-ikenga-iyke");
        assert_eq!(iyke_skill.owner_pkg_id.as_deref(), Some("com.ikenga.iyke"));
        assert_eq!(iyke_skill.placements[0].managed_by, NgwaManagedBy::Pkg);

        // Personal Ọba skill spans two engines.
        let gw = item(&snap, "skill:personal:groundwork");
        assert_eq!(gw.engines, vec!["claude".to_string(), "gemini".to_string()]);
    }

    /// F-5: one skill placed personally and in a project is two items.
    #[test]
    fn personal_and_project_placement_of_one_skill_are_two_items() {
        let snap = build_snapshot(golden_inputs());
        let gw: Vec<&NgwaItem> = snap
            .items
            .iter()
            .filter(|i| i.kind == NgwaKind::Skill && i.name == "groundwork")
            .collect();
        assert_eq!(gw.len(), 2, "{:?}", gw.iter().map(|i| &i.id).collect::<Vec<_>>());
        let personal = item(&snap, "skill:personal:groundwork");
        let project = item(&snap, "skill:project:proj-royalti:groundwork");
        assert_eq!(personal.scope, NgwaScope::Personal);
        assert_eq!(
            project.scope,
            NgwaScope::Project { project_id: "proj-royalti".into() },
            "a registered root (even with backslashes + trailing slash) resolves to its id"
        );
        assert!(personal.placements.iter().all(|p| p.scope == NgwaScope::Personal));
        assert!(project.placements.iter().all(|p| p.scope == project.scope));
        // The personal placement records what shadows it (F-6).
        assert_eq!(
            personal
                .placements
                .iter()
                .find(|p| p.engine == "claude")
                .and_then(|p| p.overridden_by.clone()),
            Some("C:/Users/x/royalti-co/.claude/skills/groundwork/SKILL.md".to_string())
        );
        // A vault-linked project placement shows the vault's provenance.
        assert_eq!(project.origin.source, NgwaSource::Git);
    }

    #[test]
    fn unregistered_project_root_gets_a_stable_normalized_id() {
        let mut inputs = golden_inputs();
        inputs.projects.clear();
        let snap = build_snapshot(inputs);
        assert!(snap
            .items
            .iter()
            .any(|i| i.id == "skill:project:c:/Users/x/royalti-co:groundwork"));
        assert_eq!(normalize_path("C:\\Users\\x\\royalti-co\\"), "c:/Users/x/royalti-co");
        assert_eq!(normalize_path("/home/x/repo/"), "/home/x/repo");
        assert_eq!(normalize_path("/"), "/");
    }

    /// F-6: mechanism / format / status come from the G-ADAPTER layout, not
    /// from the scan's symlink bit or a hardcoded literal.
    #[test]
    fn placement_mechanism_format_status_come_from_engine_layout() {
        let snap = build_snapshot(golden_inputs());
        let layouts = engine_layouts();
        let claude = layouts
            .iter()
            .find(|l| l.engine == EngineId::Claude)
            .expect("claude");
        for (id, prim) in [
            ("agent:personal:reviewer", PrimitiveKind::Agent),
            ("command:personal:ship", PrimitiveKind::Command),
            ("hook:personal:secret-scan.sh", PrimitiveKind::Hook),
            ("tool:personal:Claude Browser", PrimitiveKind::Mcp),
        ] {
            let cell = claude.kinds.get(&prim).expect("cell");
            let p = &item(&snap, id).placements[0];
            assert_eq!(p.mechanism, NgwaMechanism::from(cell.mechanism), "{id}");
            assert_eq!(p.format, Some(NgwaFormat::from(cell.format)), "{id}");
            assert_eq!(p.status, NgwaPlacementStatus::from(cell.status), "{id}");
        }
    }

    /// Item 14: `required_by` is a pure inversion, tested directly.
    #[test]
    fn compute_required_by_inverts_and_resolves() {
        let base = |id: &str, kind: NgwaKind, name: &str, scope: NgwaScope| NgwaItem {
            id: id.into(),
            kind,
            name: name.into(),
            display_name: name.into(),
            description: None,
            version: None,
            latest_version: None,
            scope,
            origin: NgwaOrigin::local(),
            state: NgwaState::Enabled,
            runtime: None,
            trust: NgwaTrust::not_applicable(),
            placements: Vec::new(),
            usage: None,
            requires: Vec::new(),
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: None,
            engines: Vec::new(),
        };
        let proj = NgwaScope::Project { project_id: "p1".into() };
        let req = |kind: &str, name: &str| NgwaRef {
            kind: kind.into(),
            name: name.into(),
            item_id: None,
            source: None,
            r#ref: None,
        };
        let mut app = base("com.x.app", NgwaKind::App, "com.x.app", proj.clone());
        app.requires = vec![
            req("skill", "gw"),
            req("mcp", "srv"),
            req("skill", "missing"),
            req("agent", "gw"),
        ];
        let mut other = base("com.x.other", NgwaKind::App, "com.x.other", NgwaScope::Personal);
        other.requires = vec![req("skill", "gw")];
        let mut items = vec![
            app,
            other,
            base("skill:personal:gw", NgwaKind::Skill, "gw", NgwaScope::Personal),
            base("skill:project:p1:gw", NgwaKind::Skill, "gw", proj.clone()),
            base("tool:personal:srv", NgwaKind::Tool, "srv", NgwaScope::Personal),
        ];
        compute_required_by(&mut items);

        let ids: Vec<Option<&str>> = items[0]
            .requires
            .iter()
            .map(|r| r.item_id.as_deref())
            .collect();
        assert_eq!(
            ids,
            vec![Some("skill:project:p1:gw"), Some("tool:personal:srv"), None, None],
            "same-scope target preferred; mcp->tool; unknown and kind-mismatched refs stay null"
        );
        assert_eq!(items[1].requires[0].item_id.as_deref(), Some("skill:personal:gw"));
        let rb = |i: usize| {
            items[i]
                .required_by
                .iter()
                .map(|r| r.item_id.clone().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(rb(2), vec!["com.x.other".to_string()]);
        assert_eq!(rb(3), vec!["com.x.app".to_string()]);
        assert_eq!(rb(4), vec!["com.x.app".to_string()]);
        assert!(items[0].required_by.is_empty());
        assert_eq!(items[3].required_by[0].kind, "app");
        assert_eq!(items[3].required_by[0].source, Some(NgwaSource::Local));
    }

    /// Item 13: ids are unique in a built snapshot, and the check catches a dup.
    #[test]
    fn ids_are_unique_and_duplicates_are_detected() {
        let snap = build_snapshot(golden_inputs());
        assert!(duplicate_ids(&snap.items).is_empty());
        let mut items = snap.items.clone();
        items.push(items[0].clone());
        assert_eq!(duplicate_ids(&items), vec![items[0].id.clone()]);
    }

    /// Item 13: build_snapshot itself asserts uniqueness (debug builds).
    #[test]
    #[should_panic(expected = "duplicate ids")]
    fn build_snapshot_debug_asserts_unique_ids() {
        let mut inputs = golden_inputs();
        // Two installs reporting the same pkg id — a kernel-invariant breach.
        let dup = pkgs_fixture().remove(4);
        inputs.pkgs.push(dup);
        let _ = build_snapshot(inputs);
    }

    /// F-1 + DoD: an absent corpus yields `usage: null` on every item and
    /// `sources.usage.ok == false`.
    #[tokio::test]
    async fn absent_corpus_yields_null_usage_everywhere() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("ngwa.db"));
        let pool = db.ensure_pool().await.expect("pool");
        let missing = tmp.path().join("no-such-projects-dir");
        let usage = collect_usage(&pool, Some(missing), NOW).await;
        assert!(!usage.snapshot.is_available());

        let mut inputs = golden_inputs();
        inputs.usage = usage;
        let snap = build_snapshot(inputs);
        assert!(!snap.items.is_empty());
        for it in &snap.items {
            assert!(it.usage.is_none(), "{} has usage despite an absent corpus", it.id);
        }
        assert!(!snap.sources.usage.ok);
        assert!(snap
            .sources
            .usage
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not found"));
        assert_eq!(snap.sources.usage.count, 0);
    }

    /// F-1: a present, successfully scanned (empty) corpus measures zeros.
    #[tokio::test]
    async fn present_empty_corpus_yields_measured_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("ngwa.db"));
        let pool = db.ensure_pool().await.expect("pool");
        let corpus = tmp.path().join("projects");
        std::fs::create_dir_all(&corpus).expect("mkdir");
        let usage = collect_usage(&pool, Some(corpus), NOW).await;
        assert!(usage.snapshot.is_available());
        let mut inputs = golden_inputs();
        inputs.usage = usage;
        let snap = build_snapshot(inputs);
        let gw = item(&snap, "skill:personal:groundwork")
            .usage
            .clone()
            .expect("measured");
        assert_eq!(
            (gw.count_7d, gw.count_30d, gw.tokens_30d, gw.last_used_ms),
            (Some(0), Some(0), Some(0), None)
        );
        assert!(snap.sources.usage.ok);
        assert!(item(&snap, "hook:personal:secret-scan.sh").usage.is_none());
    }

    /// F-7: missing registries are reported, not zeroed.
    #[test]
    fn missing_registries_report_unhealthy_sources() {
        let mut inputs = golden_inputs();
        inputs.registries.clear();
        let snap = build_snapshot(inputs);
        assert!(!snap.sources.engine_assets.ok);
        assert!(snap.sources.engine_assets.error.is_some());
        assert!(!snap.sources.kernel.ok);
        assert!(snap
            .sources
            .kernel
            .error
            .as_deref()
            .unwrap_or("")
            .contains("sidecar_supervisor"));
        assert_eq!(snap.sources.kernel.count, 5, "pkgs are still listed");

        let ok = build_snapshot(golden_inputs());
        assert!(ok.sources.kernel.ok && ok.sources.engine_assets.ok);
        assert_eq!(ok.sources.engine_assets.count, 1);
        assert_eq!(ok.sources.usage.count, 5, "usage count is the mirror total, not a scan delta");
    }

    /// Round 13: a pkg whose only block is `requires[]` is not a bundle.
    #[test]
    fn requires_only_pkg_is_not_a_bundle() {
        let m = manifest(json!({
            "id": "com.x.pack", "name": "Pack", "version": "1.0.0", "ikenga_api": "1",
            "requires": [{ "kind": "skill", "name": "gw" }]
        }));
        assert_ne!(pkg_kind(&m), NgwaKind::Bundle);
        assert_eq!(pkg_kind(&m), NgwaKind::App);
    }

    #[test]
    fn broken_manifest_is_broken_state_not_a_trust_failure() {
        let mut inputs = golden_inputs();
        inputs.pkgs[4].manifest = Err("parse error".into());
        inputs.pkgs[4].trust = Err("manifest not loaded".into());
        let snap = build_snapshot(inputs);
        let iyke = item(&snap, "com.ikenga.iyke");
        assert_eq!(iyke.state, NgwaState::Broken);
        assert_eq!(iyke.trust.state, NgwaTrustState::NotApplicable);
        assert!(snap.sources.trust.ok);
    }

    #[test]
    fn wire_enums_serialize_to_contract_literals() {
        let v = |x: serde_json::Value| x.as_str().unwrap().to_string();
        assert_eq!(v(json!(NgwaRuntimeState::ShuttingDown)), "shuttingdown");
        assert_eq!(v(json!(NgwaMechanism::SymlinkDir)), "symlink-dir");
        assert_eq!(v(json!(NgwaFormat::JsonEmbedded)), "json-embedded");
        assert_eq!(v(json!(NgwaTrustState::NotApplicable)), "not_applicable");
        assert_eq!(v(json!(NgwaManagedBy::Oba)), "oba");
        assert_eq!(v(json!(NgwaKind::Schedule)), "schedule");
        assert_eq!(
            serde_json::to_string(&NgwaScope::Project { project_id: "p1".into() }).unwrap(),
            r#"{"kind":"project","project_id":"p1"}"#
        );
    }

    /// WP-28: `GET /iyke/ngwa/snapshot` and the `ngwa_snapshot` command share
    /// `ngwa_snapshot_inner`. Drive it with an empty kernel + fixture dirs
    /// (no AppHandle needed) and diff the serialized wire shape against the
    /// iyke-cli vendored golden (`iyke-cli/tests/fixtures/`): same top-level
    /// keys, same `sources` health keys, same per-item field set when the
    /// golden carries items.
    #[tokio::test]
    async fn ngwa_snapshot_inner_route_shape_matches_cli_golden() {
        const CLI_GOLDEN: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../iyke-cli/tests/fixtures/ngwa-snapshot.golden.json"
        );
        let golden_text = match std::fs::read_to_string(CLI_GOLDEN) {
            Ok(t) => t,
            Err(_) => {
                eprintln!(
                    "ngwa_snapshot_inner golden-shape test skipped — \
                     {CLI_GOLDEN} not found (sibling iyke-cli checkout absent)"
                );
                return;
            }
        };
        let golden: serde_json::Value = serde_json::from_str(&golden_text).unwrap();

        let tmp = tempfile::tempdir().expect("tempdir");
        let db = Arc::new(PaDb::new(tmp.path().join("ikenga.db")));
        let corpus = tmp.path().join("claude-projects");
        std::fs::create_dir_all(&corpus).unwrap();
        let app_data = tmp.path().join("app-data");
        std::fs::create_dir_all(&app_data).unwrap();
        let status = crate::pkg::kernel::KernelStatus {
            installed: vec![],
            registries: HashMap::new(),
            api_version: crate::pkg::manifest::IKENGA_API_VERSION,
        };

        let snap = ngwa_snapshot_inner(status, &db, &app_data, Some(corpus))
            .await
            .expect("inner snapshot builds on empty state");
        let v = serde_json::to_value(&snap).unwrap();

        // Top-level wire shape == golden.
        let mut got: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        got.sort();
        let mut want: Vec<String> = golden
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        want.sort();
        assert_eq!(got, want, "top-level keys drifted from the CLI golden");

        // `sources` health map: same source names, each with the same
        // {ok, error?, count} keys.
        let mut got_sources: Vec<String> = v["sources"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        got_sources.sort();
        let mut want_sources: Vec<String> = golden["sources"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        want_sources.sort();
        assert_eq!(
            got_sources, want_sources,
            "sources map drifted from the CLI golden"
        );

        // Per-item field set == golden's first item (all items share the
        // schema — the golden diff is on the field *names*, not values).
        if let Some(gitem) = golden["items"].as_array().and_then(|a| a.first()) {
            let mut want_item_keys: Vec<String> = gitem
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            want_item_keys.sort();
            // A fresh kernel has no items, so pin the shape via a synthetic
            // serialize round-trip: every NgwaItem field name is in the
            // golden's set. Reuse the WP-14 golden inputs for one real item.
            let inputs = golden_inputs();
            let rich = build_snapshot(inputs);
            let rv = serde_json::to_value(&rich).unwrap();
            let mut got_item_keys: Vec<String> = rv["items"].as_array().unwrap()[0]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            got_item_keys.sort();
            assert_eq!(
                got_item_keys, want_item_keys,
                "item field set drifted from the CLI golden"
            );
        }
    }
}

