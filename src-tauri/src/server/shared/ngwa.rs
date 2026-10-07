//! Ngwa unified snapshot — the pure join (WP-14, review fixes WP-14a), shared
//! by the desktop `ngwa_snapshot` command and the daemon's `/api/rpc` arm.
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
//! Callers only *collect* inputs; the join itself is the pure
//! [`build_snapshot`], which takes no Tauri state — that is what the golden
//! test (`__fixtures__/ngwa-snapshot.golden.json`) and the vitest DEC-26
//! parity test both consume. It lives here, ungated, so the desktop command
//! (`commands::ngwa`, which re-exports all of this) and the daemon arm
//! (`server::rpc_claude::ngwa_snapshot`) run the very same join.
//!
//! ## What the daemon cannot see ([`NotServed`])
//!
//! The headless daemon runs no pkg kernel supervisor, no `engine_assets`
//! registry and no trust store, and does not scan the transcript corpus. It
//! says so through [`SnapshotInputs::not_served`] and the usage input, and
//! each such source reads `ok: false` with a reason containing
//! [`NOT_AVAILABLE_ON_SERVER`] — never an empty set that reads as healthy.
//! The desktop passes [`NotServed::default()`], which changes nothing.
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

use serde::{Deserialize, Serialize};

use crate::pkg::manifest::{Manifest, RequireSource, RequiresEntry};
use crate::pkg::source::InstallSource;
use crate::pkg::status::InstalledSummary;
use crate::pkg::trust_state::{self as trust, TrustState};
use crate::server::shared::claude_config::{
    ClaudeConfig, ScanError, Scope as ConfigScope, SystemTag,
};
use crate::server::shared::claude_store::{ClaudeStoreEntry, ProvenanceSource};
use crate::server::shared::engine_layout::{
    engine_layouts, ConfigFormat, EngineId, EngineLayout, KindStatus, Mechanism, PrimitiveKind,
};
use crate::transcript::usage::{UsageAggregate, UsageKind, UsageSnapshot};

/// The phrase every "this host does not run that" reason carries. The
/// frontend keys on it (`isNotAvailableOnServer`, `src/lib/transport/
/// unavailable.ts`) to render "Not available on this server" rather than an
/// error, an empty list, or an "unsigned" / "healthy" reading.
pub const NOT_AVAILABLE_ON_SERVER: &str = "not available on this server";

/// How `sources.engine_config.error` starts when the config scan answered but
/// could not read everything it was asked to (`ClaudeConfig.errors`): the
/// source reads `ok: false` while `count` still counts the rows it did read.
/// Each path it names is in backticks; a registered project's root named
/// there was not scanned at all (on the daemon: outside the fs allowlist), so
/// its column is unknown, not empty. The frontend keys on both
/// (`src/lib/ngwa/scan-coverage.ts`).
pub const PARTIALLY_UNREADABLE: &str = "partially unreadable";

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
    pub(crate) fn local() -> Self {
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
                tracing::warn!("[ngwa_snapshot] unknown supervisor state `{other}`; reporting `stopped`");
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
    pub(crate) fn not_applicable() -> Self {
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
    /// Sources this host does not run at all. Default on the desktop.
    pub not_served: NotServed,
}

/// Sources a host does not run at all, each with the reason `sources.*.error`
/// then carries (it should contain [`NOT_AVAILABLE_ON_SERVER`]). `None` =
/// served. The desktop serves everything ([`NotServed::default`]); the
/// headless daemon serves none of these three.
#[derive(Debug, Clone, Default)]
pub struct NotServed {
    /// No sidecar supervisor: every pkg's `runtime` is `null` (unknown, not
    /// stopped) and `sources.kernel` is `ok: false` — its `count` still
    /// lists the pkgs, which ARE known.
    pub runtime: Option<String>,
    /// No `engine_assets` registry: no placement is attributed to a pkg, and
    /// `sources.engine_assets` is `ok: false`.
    pub engine_assets: Option<String>,
    /// No trust store: `trust_pending` and every `PkgInput::trust` are
    /// ignored, each pkg's trust reads `not_applicable`, and `sources.trust`
    /// is `ok: false` — so the UI can tell "not evaluated here" from
    /// "unsigned". `perms` still carries the sensitive permissions the
    /// manifest DECLARES (read, not evaluated or approved) when it loads.
    pub trust: Option<String>,
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

/// The sensitive permissions `m` declares — read from the manifest, which
/// says nothing about whether they were evaluated or approved.
fn declared_perms(m: &Manifest) -> NgwaPermsSummary {
    let p = trust::summarize_sensitive(&m.permissions);
    NgwaPermsSummary {
        shell_execute: p.shell_execute,
        fs_write_outside_sandbox: p.fs_write_outside_sandbox,
        net: p.net,
        vault_keys: p.vault_keys,
    }
}

fn map_trust(state: &TrustState, signed: bool, review_pending: bool, m: &Manifest) -> NgwaTrust {
    let (s, auto_trusted, last) = match state {
        TrustState::AutoTrusted => (NgwaTrustState::AutoTrusted, true, None),
        TrustState::AutoGranted => (NgwaTrustState::AutoGranted, false, None),
        TrustState::Granted { granted_at_ms, .. } => (NgwaTrustState::Granted, false, Some(*granted_at_ms)),
        TrustState::NeedsApproval { .. } => (NgwaTrustState::NeedsApproval, false, None),
    };
    NgwaTrust {
        state: s,
        signed,
        auto_trusted,
        review_pending,
        perms: Some(declared_perms(m)),
        last_granted_at_ms: last,
    }
}

/// `sources.engine_config.error` for a scan that read only part of what it
/// was asked to (see [`PARTIALLY_UNREADABLE`]). An error whose path is a
/// registered project's root means that whole project was not scanned; any
/// other is one unreadable file. Each path is quoted in backticks.
fn partial_scan_error(errors: &[ScanError], projects: &[(String, String)]) -> String {
    let roots: HashSet<String> = projects.iter().map(|(_, r)| normalize_path(r)).collect();
    let (mut not_scanned, mut files) = (Vec::new(), Vec::new());
    for e in errors {
        let line = format!("`{}` ({})", e.path, e.message);
        if roots.contains(&normalize_path(&e.path)) {
            not_scanned.push(line);
        } else {
            files.push(line);
        }
    }
    let mut parts = Vec::new();
    if !not_scanned.is_empty() {
        parts.push(format!("project roots not scanned: {}", not_scanned.join("; ")));
    }
    if !files.is_empty() {
        parts.push(format!("unreadable: {}", files.join("; ")));
    }
    format!("{PARTIALLY_UNREADABLE} — {}", parts.join(" · "))
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
        not_served,
    } = inputs;
    let snap = &usage.snapshot;

    let project_ids: HashMap<String, String> = projects
        .iter()
        .map(|(id, root)| (normalize_path(root), id.clone()))
        .collect();

    let (assets, engine_assets_health) = match &not_served.engine_assets {
        Some(reason) => (Vec::new(), NgwaSourceHealth::failed(reason.clone())),
        None => parse_engine_assets(&registries),
    };
    let supervisor = match &not_served.runtime {
        Some(reason) => Err(reason.clone()),
        None => parse_supervisor(&registries),
    };
    let layouts = engine_layouts();

    let mut items: Vec<NgwaItem> = Vec::new();

    // ── Config-scan placements, grouped by (kind, scope, name) (F-5) ──
    let (rows, config_health) = match &config {
        Ok(c) => {
            let rows = scan_rows(c, &project_ids);
            // A scan that skipped a root or a file is partial, never "ok":
            // a dropped project would otherwise read as one with nothing in
            // it. No errors (the desktop's usual case) is unchanged.
            let health = if c.errors.is_empty() {
                NgwaSourceHealth::ok(rows.len())
            } else {
                NgwaSourceHealth {
                    ok: false,
                    error: Some(partial_scan_error(&c.errors, &projects)),
                    count: rows.len(),
                }
            };
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
            tracing::warn!("[ngwa_snapshot] Ọba entry `{}` has unknown kind `{}`; skipped", entry.name, entry.kind);
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
    let (pending, mut trust_errors) = match (&not_served.trust, trust_pending) {
        (Some(_), _) => (HashSet::new(), Vec::new()),
        (None, Ok(p)) => (p, Vec::new()),
        (None, Err(e)) => (HashSet::new(), vec![format!("pending reviews: {e}")]),
    };
    let trust_count = pending.len();

    for p in pkgs {
        let s = p.summary;
        let manifest = p.manifest.as_ref().ok();
        if let Err(e) = &p.manifest {
            tracing::warn!("[ngwa_snapshot] manifest for `{}` unreadable: {e}", s.id);
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
            // Not evaluated on this host: unknown, which is not "unsigned".
            // The manifest's declared sensitive perms are still known.
            _ if not_served.trust.is_some() => NgwaTrust {
                perms: manifest.map(declared_perms),
                ..NgwaTrust::not_applicable()
            },
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
        tracing::error!("[ngwa_snapshot] duplicate item ids: {dups:?}");
    }

    let trust_health = if let Some(reason) = &not_served.trust {
        NgwaSourceHealth::failed(reason.clone())
    } else if trust_errors.is_empty() {
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

#[cfg(test)]
mod tests {
    //! The join's `not_served` half, in both builds. The desktop half (every
    //! source served) is pinned by the golden test in `commands::ngwa`.

    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_790_000_000_000;

    fn pkg(id: &str, extra: serde_json::Value) -> PkgInput {
        let mut m = json!({ "id": id, "name": id, "version": "1.0.0", "ikenga_api": "1" });
        m.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        PkgInput {
            summary: InstalledSummary {
                id: id.into(),
                version: "1.0.0".into(),
                ikenga_api: "1".into(),
                install_path: format!("/pkgs/{id}"),
                enabled: true,
                installed_at: NOW,
                compatible: true,
                source: InstallSource::Local { path: format!("/pkgs/{id}") },
                project_id: None,
            },
            manifest: Ok(serde_json::from_value(m).unwrap()),
            trust: Ok(TrustState::AutoGranted),
        }
    }

    fn inputs(not_served: NotServed) -> SnapshotInputs {
        SnapshotInputs {
            now_ms: NOW,
            projects: Vec::new(),
            pkgs: vec![
                pkg("com.test.app", json!({ "ui": { "routes": [] } })),
                pkg(
                    "com.test.side",
                    json!({ "sidecars": [{ "name": "pa-com-test-side-w", "bin": "bin/w" }] }),
                ),
            ],
            registries: HashMap::from([
                ("engine_assets".to_string(), json!({ "count": 0, "entries": [] })),
                (
                    "sidecar_supervisor".to_string(),
                    json!({ "count": 1, "entries": [
                        { "pkg_id": "com.test.side", "state": "running", "pid": 7, "uptime_s": 1,
                          "restarts": 0, "last_crash_unix_ms": null, "last_err": null }
                    ]}),
                ),
            ]),
            oba: Ok(Vec::new()),
            config: Ok(ClaudeConfig {
                agents: Vec::new(),
                skills: Vec::new(),
                commands: Vec::new(),
                hooks: Vec::new(),
                mcps: Vec::new(),
                errors: Vec::new(),
            }),
            trust_pending: Ok(HashSet::from(["com.test.app".to_string()])),
            usage: UsageInput::unavailable(format!("usage {NOT_AVAILABLE_ON_SERVER}")),
            not_served,
        }
    }

    fn reason(what: &str) -> Option<String> {
        Some(format!("{what} is {NOT_AVAILABLE_ON_SERVER}"))
    }

    /// What a host does not run reads `ok: false` with its reason — never an
    /// empty-but-healthy set — and every per-item field it would have filled
    /// is unknown (`null` / `not_applicable`), not zeroed.
    #[test]
    fn not_served_sources_read_unavailable_not_empty() {
        let snap = build_snapshot(inputs(NotServed {
            runtime: reason("runtime"),
            engine_assets: reason("engine_assets"),
            trust: reason("trust"),
        }));
        let s = &snap.sources;
        for (name, h) in [
            ("kernel", &s.kernel),
            ("engine_assets", &s.engine_assets),
            ("trust", &s.trust),
            ("usage", &s.usage),
        ] {
            assert!(!h.ok, "{name} must read unavailable");
            assert!(
                h.error.as_deref().unwrap_or("").contains(NOT_AVAILABLE_ON_SERVER),
                "{name}: {:?}",
                h.error
            );
        }
        assert_eq!(s.kernel.count, 2, "the pkgs themselves are known");
        assert_eq!(s.trust.count, 0, "trust_pending is ignored, not counted");
        assert!(s.engine_config.ok && s.oba.ok);

        for it in snap.items.iter().filter(|i| i.id.starts_with("com.test.")) {
            assert_eq!(it.trust.state, NgwaTrustState::NotApplicable, "{}", it.id);
            assert!(!it.trust.signed && !it.trust.auto_trusted, "{}", it.id);
            assert!(it.trust.last_granted_at_ms.is_none(), "{}", it.id);
            assert!(!it.trust.review_pending, "{}", it.id);
            assert!(it.runtime.is_none(), "{}: runtime unknown, not stopped", it.id);
            assert!(it.usage.is_none(), "{}", it.id);
        }
    }

    /// Trust not evaluated still shows what the manifest DECLARES: the
    /// sensitive perms are read, while the state stays `not_applicable`.
    #[test]
    fn not_served_trust_keeps_the_declared_perms() {
        let mut i = inputs(NotServed {
            trust: reason("trust"),
            ..NotServed::default()
        });
        i.pkgs.push(pkg(
            "com.test.perms",
            json!({ "permissions": {
                "shell.execute": ["git *"],
                "fs.write": ["$pkg_data/**", "$home/out/**"],
                "vault.keys": ["k"],
            }}),
        ));
        let snap = build_snapshot(i);
        let it = snap.items.iter().find(|i| i.id == "com.test.perms").unwrap();
        assert_eq!(it.trust.state, NgwaTrustState::NotApplicable);
        let perms = it.trust.perms.as_ref().expect("declared perms");
        assert_eq!(perms.shell_execute, vec!["git *".to_string()]);
        assert_eq!(perms.fs_write_outside_sandbox, vec!["$home/out/**".to_string()]);
        assert_eq!(perms.vault_keys, vec!["k".to_string()]);
        let none = snap.items.iter().find(|i| i.id == "com.test.app").unwrap();
        assert_eq!(none.trust.perms.as_ref().map(|p| p.shell_execute.len()), Some(0));
    }

    /// A config scan with errors reads partially unreadable: `ok: false`,
    /// the rows it did read still counted, a registered project's refused
    /// root named as not scanned, and a file error named apart.
    #[test]
    fn config_errors_read_partially_unreadable() {
        let mut i = inputs(NotServed::default());
        i.projects = vec![("far".into(), "/x/far".into()), ("near".into(), "/x/near".into())];
        i.config = Ok(ClaudeConfig {
            agents: Vec::new(),
            skills: Vec::new(),
            commands: Vec::new(),
            hooks: Vec::new(),
            mcps: Vec::new(),
            errors: vec![
                ScanError {
                    path: "/x/far".into(),
                    message: "path outside allowlist: /x/far".into(),
                },
                ScanError {
                    path: "/x/near/.claude/settings.json".into(),
                    message: "hooks parse: eof".into(),
                },
            ],
        });
        let h = build_snapshot(i).sources.engine_config;
        assert!(!h.ok);
        let e = h.error.unwrap();
        assert!(e.starts_with(PARTIALLY_UNREADABLE), "{e}");
        assert!(
            e.contains("project roots not scanned: `/x/far` (path outside allowlist: /x/far)"),
            "{e}"
        );
        assert!(e.contains("unreadable: `/x/near/.claude/settings.json`"), "{e}");
        assert!(!e.contains("`/x/near`"), "the readable project is not named: {e}");
    }

    /// `NotServed::default()` is the desktop: the same inputs join exactly as
    /// before — supervisor runtime, evaluated trust, the pending review.
    #[test]
    fn default_not_served_changes_nothing() {
        let snap = build_snapshot(inputs(NotServed::default()));
        assert!(snap.sources.kernel.ok && snap.sources.engine_assets.ok && snap.sources.trust.ok);
        assert!(snap.sources.engine_config.ok, "no scan errors: ok, as before");
        let side = snap.items.iter().find(|i| i.id == "com.test.side").unwrap();
        assert_eq!(side.runtime.as_ref().map(|r| r.state), Some(NgwaRuntimeState::Running));
        let app = snap.items.iter().find(|i| i.id == "com.test.app").unwrap();
        assert_eq!(app.trust.state, NgwaTrustState::AutoGranted);
        assert!(app.trust.review_pending);
    }
}
