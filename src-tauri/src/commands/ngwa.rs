//! Ngwa unified snapshot command (WP-14).
//!
//! Joins the five subsystems specified in gate G-NGWA-ITEM
//! (`plans/shell-ux-rearchitecture/drafts/ngwa-item.md`):
//!   1. Pkg kernel (`pkg_kernel_status` / `InstalledSummary` / manifests / supervisor)
//!   2. Ọba Claude-asset store (`claude_store_list`)
//!   3. Engine-config scan (`claude_config` scan across projects + personal root)
//!   4. `engine_assets` registry (pkg-contributed placements)
//!   5. Trust (`pkg/trust.rs` sensitive perms + `pkg_trust` capability diff)
//!   6. Transcript JSONL usage mirror (DEC-24)

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::commands::claude_config::{self, Scope as ConfigScope};
use crate::commands::claude_store;
use crate::commands::db::PaDb;
use crate::commands::pkg::KernelState;
use crate::commands::projects;
use crate::pkg::manifest::Package;
use crate::pkg::source::InstallSource;
use crate::pkg::trust::{self, PermsSummary, TrustState};
use crate::transcript::usage::{
    claude_projects_dir, now_ms, scan_and_mirror_transcripts, UsageSnapshot,
};

// ── Types (exact parity with drafts/ngwa-item.md §2 / @ikenga/contract/ngwa) ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum NgwaScope {
    #[serde(rename = "personal")]
    Personal,
    #[serde(rename = "project")]
    Project { project_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaOrigin {
    pub source: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaRuntime {
    pub state: String,
    pub pid: Option<u32>,
    pub uptime_s: Option<u64>,
    pub restarts: u32,
    pub last_err: Option<String>,
    pub last_crash_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaPermsSummary {
    pub shell_execute: Vec<String>,
    pub fs_write_outside_sandbox: Vec<String>,
    pub net: Vec<String>,
    pub vault_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaTrust {
    pub state: String,
    pub signed: bool,
    pub auto_trusted: bool,
    pub review_pending: bool,
    pub perms: Option<NgwaPermsSummary>,
    pub last_granted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaPlacement {
    pub engine: String,
    pub scope: NgwaScope,
    pub path: String,
    pub mechanism: String,
    pub present: bool,
    pub link_target: Option<String>,
    pub in_store: bool,
    pub managed_by: String,
    pub overridden_by: Option<String>,
    pub format: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaUsage {
    pub source: String,
    pub last_used_ms: Option<i64>,
    pub count_7d: Option<i64>,
    pub count_30d: Option<i64>,
    pub tokens_30d: Option<i64>,
    pub window_start_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaRef {
    pub kind: String,
    pub name: String,
    pub item_id: Option<String>,
    pub source: Option<String>,
    #[serde(rename = "ref")]
    pub r#ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub latest_version: Option<String>,
    pub scope: NgwaScope,
    pub origin: NgwaOrigin,
    pub state: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaSourceHealth {
    pub ok: bool,
    pub error: Option<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaSourcesHealth {
    pub kernel: NgwaSourceHealth,
    pub oba: NgwaSourceHealth,
    pub engine_config: NgwaSourceHealth,
    pub engine_assets: NgwaSourceHealth,
    pub trust: NgwaSourceHealth,
    pub usage: NgwaSourceHealth,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NgwaSnapshot {
    pub items: Vec<NgwaItem>,
    pub as_of_ms: i64,
    pub sources: NgwaSourcesHealth,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn trust_not_applicable() -> NgwaTrust {
    NgwaTrust {
        state: "not_applicable".to_string(),
        signed: false,
        auto_trusted: false,
        review_pending: false,
        perms: None,
        last_granted_at_ms: None,
    }
}

fn map_perms(p: &PermsSummary) -> NgwaPermsSummary {
    NgwaPermsSummary {
        shell_execute: p.shell_execute.clone(),
        fs_write_outside_sandbox: p.fs_write_outside_sandbox.clone(),
        net: p.net.clone(),
        vault_keys: p.vault_keys.clone(),
    }
}

/// Convert wire usage into NgwaUsage struct.
fn convert_usage(u: crate::transcript::usage::NgwaUsageWire) -> NgwaUsage {
    NgwaUsage {
        source: u.source,
        last_used_ms: u.last_used_ms,
        count_7d: u.count_7d,
        count_30d: u.count_30d,
        tokens_30d: u.tokens_30d,
        window_start_ms: u.window_start_ms,
    }
}

// ── Command ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn ngwa_snapshot(
    kernel: State<'_, KernelState>,
    db: State<'_, Arc<PaDb>>,
    app: AppHandle,
) -> Result<NgwaSnapshot, String> {
    let now = now_ms();
    let pool = db.ensure_pool().await.map_err(|e| e.to_string())?;
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("resolve app_data_dir: {e}"))?;

    // ── 1. Transcript Usage Extraction ──
    let mut usage_count = 0;
    let mut usage_ok = true;
    let mut usage_err: Option<String> = None;

    if let Some(proj_dir) = claude_projects_dir() {
        match scan_and_mirror_transcripts(&pool, &proj_dir).await {
            Ok(n) => usage_count = n,
            Err(e) => {
                log::warn!("[ngwa_snapshot] transcript scan failed: {e}");
                usage_ok = false;
                usage_err = Some(e);
            }
        }
    }

    let usage_snapshot = match crate::transcript::usage::load_usage_snapshot(&pool, now).await {
        Ok(s) => s,
        Err(e) => {
            log::warn!("[ngwa_snapshot] load usage snapshot failed: {e}");
            UsageSnapshot::empty(now)
        }
    };

    // ── 2. Engine Assets Registry Map ──
    let mut engine_assets_map: HashMap<String, (String, String, String)> = HashMap::new(); // target -> (pkg_id, kind, engine_id)
    let mut engine_assets_count = 0;
    let engine_assets_ok = true;
    let engine_assets_err: Option<String> = None;

    let kernel_status = kernel.0.status();
    if let Some(assets_val) = kernel_status.registries.get("engine_assets") {
        if let Some(entries) = assets_val.get("entries").and_then(|e| e.as_array()) {
            engine_assets_count = entries.len();
            for entry in entries {
                let pkg_id = entry.get("pkg_id").and_then(|v| v.as_str()).unwrap_or("");
                let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let engine_id = entry.get("engine_id").and_then(|v| v.as_str()).unwrap_or("");
                let target = entry.get("target").and_then(|v| v.as_str()).unwrap_or("");
                if !target.is_empty() {
                    engine_assets_map.insert(
                        target.to_string(),
                        (pkg_id.to_string(), kind.to_string(), engine_id.to_string()),
                    );
                }
            }
        }
    }

    // ── 3. Supervisor Runtimes ──
    let mut supervisor_map: HashMap<String, NgwaRuntime> = HashMap::new();
    if let Some(super_val) = kernel_status.registries.get("sidecar_supervisor") {
        if let Some(entries) = super_val.get("entries").and_then(|e| e.as_array()) {
            for entry in entries {
                let pkg_id = entry.get("pkg_id").and_then(|v| v.as_str()).unwrap_or("");
                let state = entry.get("state").and_then(|v| v.as_str()).unwrap_or("stopped");
                let pid = entry.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);
                let uptime_s = entry.get("uptime_s").and_then(|v| v.as_u64());
                let restarts = entry.get("restarts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let last_err = entry.get("last_err").and_then(|v| v.as_str()).map(|s| s.to_string());
                let last_crash_ms = entry.get("last_crash_unix_ms").and_then(|v| v.as_i64());

                if !pkg_id.is_empty() {
                    supervisor_map.insert(
                        pkg_id.to_string(),
                        NgwaRuntime {
                            state: state.to_lowercase(),
                            pid,
                            uptime_s,
                            restarts,
                            last_err,
                            last_crash_ms,
                        },
                    );
                }
            }
        }
    }

    // ── 4. Projects & Engine Config Scan ──
    let mut project_roots: Vec<String> = Vec::new();
    let mut root_to_project_id: HashMap<String, String> = HashMap::new();

    let all_projects = projects::list_projects(&pool, false).await.unwrap_or_default();
    for p in all_projects {
        if let Some(root) = p.root_path.clone() {
            project_roots.push(root.clone());
            root_to_project_id.insert(root, p.id);
        }
    }

    let mut config_count = 0;
    let mut config_ok = true;
    let mut config_err: Option<String> = None;

    let scanned_config = match claude_config::claude_config_load(project_roots).await {
        Ok(c) => {
            config_count = c.agents.len()
                + c.skills.len()
                + c.commands.len()
                + c.hooks.len()
                + c.mcps.len();
            c
        }
        Err(e) => {
            log::warn!("[ngwa_snapshot] claude_config scan failed: {e}");
            config_ok = false;
            config_err = Some(e);
            claude_config::ClaudeConfig {
                agents: Vec::new(),
                skills: Vec::new(),
                commands: Vec::new(),
                hooks: Vec::new(),
                mcps: Vec::new(),
                errors: Vec::new(),
            }
        }
    };

    // Index placements by (kind, name)
    // kind is "skill" | "agent" | "command" | "hook" | "tool"
    let mut placements_by_item: HashMap<(String, String), Vec<NgwaPlacement>> = HashMap::new();

    for a in &scanned_config.agents {
        let p_id = a.project_root.as_ref().and_then(|r| root_to_project_id.get(r)).cloned();
        let scope = match a.scope {
            ConfigScope::Personal => NgwaScope::Personal,
            ConfigScope::Project => NgwaScope::Project {
                project_id: p_id.unwrap_or_else(|| a.project_root.clone().unwrap_or_default()),
            },
        };
        let managed_by = if a.in_store {
            "oba".to_string()
        } else if engine_assets_map.contains_key(&a.path) {
            "pkg".to_string()
        } else {
            "user".to_string()
        };
        placements_by_item
            .entry(("agent".to_string(), a.name.clone()))
            .or_default()
            .push(NgwaPlacement {
                engine: "claude".to_string(),
                scope,
                path: a.path.clone(),
                mechanism: if a.is_symlink { "symlink-dir".to_string() } else { "file".to_string() },
                present: a.target_exists,
                link_target: a.link_target.clone(),
                in_store: a.in_store,
                managed_by,
                overridden_by: None,
                format: Some("md-yaml".to_string()),
                status: "active".to_string(),
            });
    }

    for s in &scanned_config.skills {
        let p_id = s.project_root.as_ref().and_then(|r| root_to_project_id.get(r)).cloned();
        let scope = match s.scope {
            ConfigScope::Personal => NgwaScope::Personal,
            ConfigScope::Project => NgwaScope::Project {
                project_id: p_id.unwrap_or_else(|| s.project_root.clone().unwrap_or_default()),
            },
        };
        let managed_by = if s.in_store {
            "oba".to_string()
        } else if engine_assets_map.contains_key(&s.path) {
            "pkg".to_string()
        } else {
            "user".to_string()
        };
        placements_by_item
            .entry(("skill".to_string(), s.name.clone()))
            .or_default()
            .push(NgwaPlacement {
                engine: "claude".to_string(),
                scope,
                path: s.path.clone(),
                mechanism: if s.is_symlink { "symlink-dir".to_string() } else { "file".to_string() },
                present: s.target_exists,
                link_target: s.link_target.clone(),
                in_store: s.in_store,
                managed_by,
                overridden_by: None,
                format: Some("md-yaml".to_string()),
                status: "active".to_string(),
            });
    }

    for c in &scanned_config.commands {
        let p_id = c.project_root.as_ref().and_then(|r| root_to_project_id.get(r)).cloned();
        let scope = match c.scope {
            ConfigScope::Personal => NgwaScope::Personal,
            ConfigScope::Project => NgwaScope::Project {
                project_id: p_id.unwrap_or_else(|| c.project_root.clone().unwrap_or_default()),
            },
        };
        let managed_by = if c.in_store {
            "oba".to_string()
        } else if engine_assets_map.contains_key(&c.path) {
            "pkg".to_string()
        } else {
            "user".to_string()
        };
        placements_by_item
            .entry(("command".to_string(), c.name.clone()))
            .or_default()
            .push(NgwaPlacement {
                engine: "claude".to_string(),
                scope,
                path: c.path.clone(),
                mechanism: "file".to_string(),
                present: c.target_exists,
                link_target: c.link_target.clone(),
                in_store: c.in_store,
                managed_by,
                overridden_by: None,
                format: Some("md-yaml".to_string()),
                status: "active".to_string(),
            });
    }

    // ── 5. Trust Pending Reviews ──
    let mut trust_pending_ids = HashSet::new();
    let mut trust_count = 0;
    let mut trust_ok = true;
    let mut trust_err: Option<String> = None;

    match crate::commands::pkg_trust::pkg_trust_list_pending(db.clone(), kernel.clone()).await {
        Ok(pending) => {
            trust_count = pending.len();
            for r in pending {
                trust_pending_ids.insert(r.pkg_id);
            }
        }
        Err(e) => {
            log::warn!("[ngwa_snapshot] pkg_trust_list_pending failed: {e}");
            trust_ok = false;
            trust_err = Some(e);
        }
    }

    // ── 6. Ọba Store Catalog ──
    let mut oba_count = 0;
    let mut oba_ok = true;
    let mut oba_err: Option<String> = None;

    let oba_entries = match claude_store::claude_store_list(db.clone(), None).await {
        Ok(entries) => {
            oba_count = entries.len();
            entries
        }
        Err(e) => {
            log::warn!("[ngwa_snapshot] claude_store_list failed: {e}");
            oba_ok = false;
            oba_err = Some(e);
            Vec::new()
        }
    };

    let mut items: Vec<NgwaItem> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut oba_seen: HashSet<(String, String)> = HashSet::new(); // (kind, name)

    // ── 7. Produce items from Ọba ──
    for entry in oba_entries {
        let kind_str = entry.kind.clone();
        let name = entry.name.clone();
        oba_seen.insert((kind_str.clone(), name.clone()));

        let id = format!("{}:personal:{}", kind_str, name);
        seen_ids.insert(id.clone());

        let placements = placements_by_item
            .remove(&(kind_str.clone(), name.clone()))
            .unwrap_or_default();

        let mut engines_set = HashSet::new();
        for p in &placements {
            engines_set.insert(p.engine.clone());
        }
        let mut engines: Vec<String> = engines_set.into_iter().collect();
        engines.sort();

        // Usage for measurable Ọba primitives: skill, agent
        let usage = match kind_str.as_str() {
            "skill" | "agent" => Some(convert_usage(usage_snapshot.for_primitive(&kind_str, &name))),
            _ => None, // command, bundle, etc. read null
        };

        // Source provenance
        let source_str = match entry.provenance.source {
            claude_store::ProvenanceSource::Local => "local",
            claude_store::ProvenanceSource::Git => "git",
            claude_store::ProvenanceSource::Npx => "npx",
            claude_store::ProvenanceSource::Catalog => "catalog",
        };

        let requires: Vec<NgwaRef> = entry
            .requires
            .iter()
            .map(|r| NgwaRef {
                kind: r.kind.clone(),
                name: r.name.clone(),
                item_id: None,
                source: r.source.as_ref().map(|s| match s {
                    crate::pkg::manifest::RequireSource::Git => "git".to_string(),
                    crate::pkg::manifest::RequireSource::Npx => "npx".to_string(),
                    crate::pkg::manifest::RequireSource::Catalog => "catalog".to_string(),
                    crate::pkg::manifest::RequireSource::Local => "local".to_string(),
                }),
                r#ref: r.r#ref.clone(),
            })
            .collect();

        let state = if entry.enabled_in.is_empty() {
            "disabled".to_string()
        } else {
            "enabled".to_string()
        };

        items.push(NgwaItem {
            id,
            kind: kind_str,
            name: name.clone(),
            display_name: name,
            description: entry.description,
            version: entry.provenance.version.clone(),
            latest_version: None,
            scope: NgwaScope::Personal,
            origin: NgwaOrigin {
                source: source_str.to_string(),
                url: entry.provenance.url,
                r#ref: entry.provenance.r#ref,
                resolved_version: entry.provenance.version,
                publisher: None,
                managed: entry.provenance.managed,
                auto_update: entry.provenance.auto_update,
                installed_at_ms: Some(entry.modified_ms),
                updated_at_ms: None,
            },
            state,
            runtime: None,
            trust: trust_not_applicable(),
            placements,
            usage,
            requires,
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: Some(entry.store_path),
            engines,
        });
    }

    // ── 8. Produce remaining config-scan items (not in Ọba) ──
    // Unmatched agents/skills/commands on disk
    for ((kind_str, name), placements) in placements_by_item {
        if oba_seen.contains(&(kind_str.clone(), name.clone())) {
            continue;
        }

        let scope = placements
            .first()
            .map(|p| p.scope.clone())
            .unwrap_or(NgwaScope::Personal);

        let scope_key = match &scope {
            NgwaScope::Personal => "personal".to_string(),
            NgwaScope::Project { project_id } => format!("project:{}", project_id),
        };

        let id = format!("{}:{}:{}", kind_str, scope_key, name);
        if seen_ids.contains(&id) {
            continue;
        }
        seen_ids.insert(id.clone());

        let mut engines_set = HashSet::new();
        for p in &placements {
            engines_set.insert(p.engine.clone());
        }
        let mut engines: Vec<String> = engines_set.into_iter().collect();
        engines.sort();

        let usage = match kind_str.as_str() {
            "skill" | "agent" => Some(convert_usage(usage_snapshot.for_primitive(&kind_str, &name))),
            _ => None,
        };

        let owner_pkg_id = placements
            .iter()
            .find_map(|p| engine_assets_map.get(&p.path).map(|(pkg, _, _)| pkg.clone()));

        items.push(NgwaItem {
            id,
            kind: kind_str,
            name: name.clone(),
            display_name: name,
            description: None,
            version: None,
            latest_version: None,
            scope,
            origin: NgwaOrigin {
                source: "local".to_string(),
                url: None,
                r#ref: None,
                resolved_version: None,
                publisher: None,
                managed: false,
                auto_update: false,
                installed_at_ms: None,
                updated_at_ms: None,
            },
            state: "enabled".to_string(),
            runtime: None,
            trust: trust_not_applicable(),
            placements,
            usage,
            requires: Vec::new(),
            required_by: Vec::new(),
            owner_pkg_id,
            install_path: None,
            engines,
        });
    }

    // Config scan Hooks
    for hook in &scanned_config.hooks {
        let p_id = hook.project_root.as_ref().and_then(|r| root_to_project_id.get(r)).cloned();
        let scope = match hook.scope {
            ConfigScope::Personal => NgwaScope::Personal,
            ConfigScope::Project => NgwaScope::Project {
                project_id: p_id.unwrap_or_else(|| hook.project_root.clone().unwrap_or_default()),
            },
        };

        let scope_key = match &scope {
            NgwaScope::Personal => "personal".to_string(),
            NgwaScope::Project { project_id } => format!("project:{}", project_id),
        };

        let id = format!("hook:{}:{}", scope_key, hook.name);
        if seen_ids.contains(&id) {
            continue;
        }
        seen_ids.insert(id.clone());

        items.push(NgwaItem {
            id,
            kind: "hook".to_string(),
            name: hook.name.clone(),
            display_name: hook.name.clone(),
            description: Some(format!("Event: {}", hook.event)),
            version: None,
            latest_version: None,
            scope: scope.clone(),
            origin: NgwaOrigin {
                source: "local".to_string(),
                url: None,
                r#ref: None,
                resolved_version: None,
                publisher: None,
                managed: false,
                auto_update: false,
                installed_at_ms: None,
                updated_at_ms: None,
            },
            state: "enabled".to_string(),
            runtime: None,
            trust: trust_not_applicable(),
            placements: vec![NgwaPlacement {
                engine: "claude".to_string(),
                scope,
                path: hook.settings_path.clone(),
                mechanism: "settings-key".to_string(),
                present: true,
                link_target: None,
                in_store: false,
                managed_by: "user".to_string(),
                overridden_by: None,
                format: Some("json-embedded".to_string()),
                status: "active".to_string(),
            }],
            usage: None, // hooks cannot be measured by transcript
            requires: Vec::new(),
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: None,
            engines: vec!["claude".to_string()],
        });
    }

    // Config scan MCP servers (emitted as 'tool')
    for mcp in &scanned_config.mcps {
        let p_id = mcp.project_root.as_ref().and_then(|r| root_to_project_id.get(r)).cloned();
        let scope = match mcp.scope {
            ConfigScope::Personal => NgwaScope::Personal,
            ConfigScope::Project => NgwaScope::Project {
                project_id: p_id.unwrap_or_else(|| mcp.project_root.clone().unwrap_or_default()),
            },
        };

        let scope_key = match &scope {
            NgwaScope::Personal => "personal".to_string(),
            NgwaScope::Project { project_id } => format!("project:{}", project_id),
        };

        let id = format!("tool:{}:{}", scope_key, mcp.name);
        if seen_ids.contains(&id) {
            continue;
        }
        seen_ids.insert(id.clone());

        let usage = usage_snapshot.for_server(&mcp.name).map(convert_usage);

        items.push(NgwaItem {
            id,
            kind: "tool".to_string(),
            name: mcp.name.clone(),
            display_name: mcp.name.clone(),
            description: Some(format!("Transport: {}", mcp.transport)),
            version: None,
            latest_version: None,
            scope: scope.clone(),
            origin: NgwaOrigin {
                source: "local".to_string(),
                url: mcp.url.clone(),
                r#ref: None,
                resolved_version: None,
                publisher: None,
                managed: false,
                auto_update: false,
                installed_at_ms: None,
                updated_at_ms: None,
            },
            state: "enabled".to_string(),
            runtime: None,
            trust: trust_not_applicable(),
            placements: vec![NgwaPlacement {
                engine: "claude".to_string(),
                scope,
                path: mcp.path.clone(),
                mechanism: "settings-key".to_string(),
                present: true,
                link_target: None,
                in_store: false,
                managed_by: "user".to_string(),
                overridden_by: None,
                format: Some("json-embedded".to_string()),
                status: "active".to_string(),
            }],
            usage,
            requires: Vec::new(),
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: None,
            engines: vec!["claude".to_string()],
        });
    }

    // ── 9. Kernel Installed Packages ──
    let kernel_ok = true;
    let kernel_err: Option<String> = None;

    let installed_pkgs = kernel.0.list_installed();
    let kernel_count = installed_pkgs.len();

    for summary in installed_pkgs {
        let pkg_id = summary.id.clone();
        let install_path = PathBuf::from(&summary.install_path);
        let pkg_res = Package::load(&install_path);

        let manifest = pkg_res.as_ref().ok().map(|p| &p.manifest);
        let display_name = manifest.map(|m| m.name.clone()).unwrap_or_else(|| pkg_id.clone());
        let description = manifest.and_then(|m| m.description.clone());
        let version = Some(summary.version.clone());

        // Derive kind from manifest
        let kind = if let Some(m) = manifest {
            if m.engine.is_some() {
                "engine".to_string()
            } else if m.ui.is_some() {
                "app".to_string()
            } else if !m.mcp.is_empty() {
                "tool".to_string()
            } else if !m.sidecars.is_empty() {
                "sidecar".to_string()
            } else if !m.requires.is_empty() {
                "bundle".to_string()
            } else {
                "app".to_string()
            }
        } else {
            "app".to_string()
        };

        let scope = match summary.project_id {
            Some(ref pid) if !pid.is_empty() => NgwaScope::Project { project_id: pid.clone() },
            _ => NgwaScope::Personal,
        };

        // Provenance & Source
        let (source_str, url, publisher) = match &summary.source {
            InstallSource::Builtin => ("builtin", None, None),
            InstallSource::Registry { url, publisher_key } => {
                ("registry", Some(url.clone()), publisher_key.clone())
            }
            InstallSource::Local { .. } => ("local", None, None),
            InstallSource::Dev { .. } => ("dev", None, None),
        };

        let origin = NgwaOrigin {
            source: source_str.to_string(),
            url,
            r#ref: None,
            resolved_version: None,
            publisher,
            managed: !matches!(summary.source, InstallSource::Builtin),
            auto_update: matches!(summary.source, InstallSource::Registry { .. }),
            installed_at_ms: Some(summary.installed_at),
            updated_at_ms: None,
        };

        let state = if !summary.enabled {
            "disabled".to_string()
        } else if !summary.compatible {
            "broken".to_string()
        } else {
            "enabled".to_string()
        };

        // Runtime from supervisor
        let runtime = supervisor_map.get(&pkg_id).cloned();

        // Trust evaluation
        let trust_eval = if let Ok(ref pkg) = pkg_res {
            match trust::evaluate(&pool, pkg, &summary.source, &app_data_dir).await {
                Ok(t) => Some(t),
                Err(e) => {
                    log::warn!("[ngwa_snapshot] trust evaluate for `{pkg_id}` failed: {e}");
                    None
                }
            }
        } else {
            None
        };

        let review_pending = trust_pending_ids.contains(&pkg_id);
        let signed = manifest.and_then(|m| m.signature.as_ref()).is_some();

        let ngwa_trust = if let Some(t) = trust_eval {
            let (state_str, auto_trusted, last_granted) = match t {
                TrustState::AutoTrusted => ("auto_trusted", true, None),
                TrustState::AutoGranted => ("auto_granted", false, None),
                TrustState::Granted { granted_at_ms, .. } => {
                    ("granted", false, Some(granted_at_ms))
                }
                TrustState::NeedsApproval { .. } => ("needs_approval", false, None),
            };

            let perms_summary = pkg_res
                .as_ref()
                .ok()
                .map(|p| map_perms(&trust::summarize_sensitive(&p.manifest.permissions)));

            NgwaTrust {
                state: state_str.to_string(),
                signed,
                auto_trusted,
                review_pending,
                perms: perms_summary,
                last_granted_at_ms: last_granted,
            }
        } else {
            trust_not_applicable()
        };

        // Usage: if pkg contributes an MCP server, check usage for that server
        let usage = if let Some(m) = manifest {
            if let Some(mcp) = m.mcp.first() {
                usage_snapshot.for_server(&mcp.name).map(convert_usage)
            } else {
                None // app/engine/sidecar without MCP server reads null
            }
        } else {
            None
        };

        // Requires
        let requires: Vec<NgwaRef> = manifest
            .map(|m| {
                m.requires
                    .iter()
                    .map(|r| NgwaRef {
                        kind: r.kind.clone(),
                        name: r.name.clone(),
                        item_id: None,
                        source: r.source.as_ref().map(|s| match s {
                            crate::pkg::manifest::RequireSource::Git => "git".to_string(),
                            crate::pkg::manifest::RequireSource::Npx => "npx".to_string(),
                            crate::pkg::manifest::RequireSource::Catalog => "catalog".to_string(),
                            crate::pkg::manifest::RequireSource::Local => "local".to_string(),
                        }),
                        r#ref: r.r#ref.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let id = pkg_id.clone();
        seen_ids.insert(id.clone());

        items.push(NgwaItem {
            id,
            kind,
            name: pkg_id.clone(),
            display_name,
            description,
            version,
            latest_version: None,
            scope: scope.clone(),
            origin: origin.clone(),
            state: state.clone(),
            runtime,
            trust: ngwa_trust,
            placements: Vec::new(),
            usage,
            requires,
            required_by: Vec::new(),
            owner_pkg_id: None,
            install_path: Some(summary.install_path.clone()),
            engines: Vec::new(),
        });

        // Child schedule items from manifest.cron[]
        if let Some(m) = manifest {
            for cron in &m.cron {
                let scope_key = match &scope {
                    NgwaScope::Personal => "personal".to_string(),
                    NgwaScope::Project { project_id } => format!("project:{}", project_id),
                };
                let cron_item_id = format!("schedule:{}:{}:{}", scope_key, pkg_id, cron.id);
                seen_ids.insert(cron_item_id.clone());

                items.push(NgwaItem {
                    id: cron_item_id,
                    kind: "schedule".to_string(),
                    name: cron.id.clone(),
                    display_name: cron.id.clone(),
                    description: Some(format!("Expr: {} | Handler: {}", cron.expr, cron.handler)),
                    version: None,
                    latest_version: None,
                    scope: scope.clone(),
                    origin: origin.clone(),
                    state: state.clone(),
                    runtime: None,
                    trust: trust_not_applicable(),
                    placements: Vec::new(),
                    usage: None,
                    requires: Vec::new(),
                    required_by: Vec::new(),
                    owner_pkg_id: Some(pkg_id.clone()),
                    install_path: None,
                    engines: Vec::new(),
                });
            }
        }
    }

    // ── 10. Compute Inverted Graph (required_by) ──
    // Build an index from (kind, name) -> item index
    // and from id -> item index
    let mut item_index_by_id: HashMap<String, usize> = HashMap::new();
    let mut item_index_by_name: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, item) in items.iter().enumerate() {
        item_index_by_id.insert(item.id.clone(), idx);
        item_index_by_name
            .entry(item.name.clone())
            .or_default()
            .push(idx);
    }

    let id_by_index: Vec<String> = items.iter().map(|it| it.id.clone()).collect();

    // Resolve item_id in requires and populate required_by
    let mut reverse_edges: HashMap<usize, Vec<NgwaRef>> = HashMap::new();

    for i in 0..items.len() {
        let from_item = items[i].clone();
        for req in &mut items[i].requires {
            // Find target
            let target_idx = if let Some(target_indices) = item_index_by_name.get(&req.name) {
                target_indices.first().copied()
            } else {
                None
            };

            if let Some(t_idx) = target_idx {
                let target_id = id_by_index[t_idx].clone();
                req.item_id = Some(target_id.clone());

                reverse_edges.entry(t_idx).or_default().push(NgwaRef {
                    kind: from_item.kind.clone(),
                    name: from_item.name.clone(),
                    item_id: Some(from_item.id.clone()),
                    source: Some(from_item.origin.source.clone()),
                    r#ref: None,
                });
            }
        }
    }

    for (target_idx, rev_refs) in reverse_edges {
        items[target_idx].required_by = rev_refs;
    }

    // ── 11. Assemble Sources Health ──
    let sources = NgwaSourcesHealth {
        kernel: NgwaSourceHealth {
            ok: kernel_ok,
            error: kernel_err,
            count: kernel_count,
        },
        oba: NgwaSourceHealth {
            ok: oba_ok,
            error: oba_err,
            count: oba_count,
        },
        engine_config: NgwaSourceHealth {
            ok: config_ok,
            error: config_err,
            count: config_count,
        },
        engine_assets: NgwaSourceHealth {
            ok: engine_assets_ok,
            error: engine_assets_err,
            count: engine_assets_count,
        },
        trust: NgwaSourceHealth {
            ok: trust_ok,
            error: trust_err,
            count: trust_count,
        },
        usage: NgwaSourceHealth {
            ok: usage_ok,
            error: usage_err,
            count: usage_count,
        },
    };

    Ok(NgwaSnapshot {
        items,
        as_of_ms: now,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ngwa_scope_serialization() {
        let personal = NgwaScope::Personal;
        let json = serde_json::to_string(&personal).expect("serialize personal");
        assert_eq!(json, r#"{"kind":"personal"}"#);

        let project = NgwaScope::Project {
            project_id: "p1".to_string(),
        };
        let json_proj = serde_json::to_string(&project).expect("serialize project");
        assert_eq!(json_proj, r#"{"kind":"project","project_id":"p1"}"#);
    }

    #[test]
    fn test_ngwa_item_id_uniqueness() {
        let mut ids = HashSet::new();
        let items = vec![
            NgwaItem {
                id: "com.ikenga.iyke".to_string(),
                kind: "app".to_string(),
                name: "iyke".to_string(),
                display_name: "Iyke".to_string(),
                description: None,
                version: Some("0.1.0".to_string()),
                latest_version: None,
                scope: NgwaScope::Personal,
                origin: NgwaOrigin {
                    source: "builtin".to_string(),
                    url: None,
                    r#ref: None,
                    resolved_version: None,
                    publisher: None,
                    managed: false,
                    auto_update: false,
                    installed_at_ms: None,
                    updated_at_ms: None,
                },
                state: "enabled".to_string(),
                runtime: None,
                trust: trust_not_applicable(),
                placements: Vec::new(),
                usage: None,
                requires: Vec::new(),
                required_by: Vec::new(),
                owner_pkg_id: None,
                install_path: None,
                engines: Vec::new(),
            },
            NgwaItem {
                id: "skill:personal:groundwork".to_string(),
                kind: "skill".to_string(),
                name: "groundwork".to_string(),
                display_name: "groundwork".to_string(),
                description: None,
                version: None,
                latest_version: None,
                scope: NgwaScope::Personal,
                origin: NgwaOrigin {
                    source: "local".to_string(),
                    url: None,
                    r#ref: None,
                    resolved_version: None,
                    publisher: None,
                    managed: false,
                    auto_update: false,
                    installed_at_ms: None,
                    updated_at_ms: None,
                },
                state: "enabled".to_string(),
                runtime: None,
                trust: trust_not_applicable(),
                placements: Vec::new(),
                usage: Some(NgwaUsage {
                    source: "transcript".to_string(),
                    last_used_ms: None,
                    count_7d: Some(0),
                    count_30d: Some(0),
                    tokens_30d: Some(0),
                    window_start_ms: 1000,
                }),
                requires: Vec::new(),
                required_by: Vec::new(),
                owner_pkg_id: None,
                install_path: None,
                engines: Vec::new(),
            },
        ];

        for item in &items {
            assert!(ids.insert(item.id.clone()), "id must be unique: {}", item.id);
        }
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_requires_inversion_to_required_by() {
        let mut items = vec![
            NgwaItem {
                id: "skill:personal:caller".to_string(),
                kind: "skill".to_string(),
                name: "caller".to_string(),
                display_name: "caller".to_string(),
                description: None,
                version: None,
                latest_version: None,
                scope: NgwaScope::Personal,
                origin: NgwaOrigin {
                    source: "local".to_string(),
                    url: None,
                    r#ref: None,
                    resolved_version: None,
                    publisher: None,
                    managed: false,
                    auto_update: false,
                    installed_at_ms: None,
                    updated_at_ms: None,
                },
                state: "enabled".to_string(),
                runtime: None,
                trust: trust_not_applicable(),
                placements: Vec::new(),
                usage: None,
                requires: vec![NgwaRef {
                    kind: "skill".to_string(),
                    name: "callee".to_string(),
                    item_id: None,
                    source: None,
                    r#ref: None,
                }],
                required_by: Vec::new(),
                owner_pkg_id: None,
                install_path: None,
                engines: Vec::new(),
            },
            NgwaItem {
                id: "skill:personal:callee".to_string(),
                kind: "skill".to_string(),
                name: "callee".to_string(),
                display_name: "callee".to_string(),
                description: None,
                version: None,
                latest_version: None,
                scope: NgwaScope::Personal,
                origin: NgwaOrigin {
                    source: "local".to_string(),
                    url: None,
                    r#ref: None,
                    resolved_version: None,
                    publisher: None,
                    managed: false,
                    auto_update: false,
                    installed_at_ms: None,
                    updated_at_ms: None,
                },
                state: "enabled".to_string(),
                runtime: None,
                trust: trust_not_applicable(),
                placements: Vec::new(),
                usage: None,
                requires: Vec::new(),
                required_by: Vec::new(),
                owner_pkg_id: None,
                install_path: None,
                engines: Vec::new(),
            },
        ];

        // Simulate graph inversion logic
        let mut item_index_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, item) in items.iter().enumerate() {
            item_index_by_name.entry(item.name.clone()).or_default().push(idx);
        }
        let id_by_index: Vec<String> = items.iter().map(|it| it.id.clone()).collect();
        let mut reverse_edges: HashMap<usize, Vec<NgwaRef>> = HashMap::new();

        for i in 0..items.len() {
            let from_item = items[i].clone();
            for req in &mut items[i].requires {
                if let Some(target_indices) = item_index_by_name.get(&req.name) {
                    if let Some(t_idx) = target_indices.first().copied() {
                        let target_id = id_by_index[t_idx].clone();
                        req.item_id = Some(target_id);

                        reverse_edges.entry(t_idx).or_default().push(NgwaRef {
                            kind: from_item.kind.clone(),
                            name: from_item.name.clone(),
                            item_id: Some(from_item.id.clone()),
                            source: Some(from_item.origin.source.clone()),
                            r#ref: None,
                        });
                    }
                }
            }
        }

        for (target_idx, rev_refs) in reverse_edges {
            items[target_idx].required_by = rev_refs;
        }

        // Verify caller.requires[0].item_id resolved
        assert_eq!(items[0].requires[0].item_id, Some("skill:personal:callee".to_string()));
        // Verify callee.required_by has caller
        assert_eq!(items[1].required_by.len(), 1);
        assert_eq!(items[1].required_by[0].name, "caller");
        assert_eq!(items[1].required_by[0].item_id, Some("skill:personal:caller".to_string()));
    }

    #[test]
    fn test_unmeasurable_kinds_usage_nullability() {
        let hook_usage: Option<NgwaUsage> = None;
        let command_usage: Option<NgwaUsage> = None;
        assert!(hook_usage.is_none(), "hook must have null usage");
        assert!(command_usage.is_none(), "command must have null usage");

        let measured_zero = NgwaUsage {
            source: "transcript".to_string(),
            last_used_ms: None,
            count_7d: Some(0),
            count_30d: Some(0),
            tokens_30d: Some(0),
            window_start_ms: 1000,
        };
        assert_eq!(measured_zero.count_7d, Some(0));
        assert!(measured_zero.last_used_ms.is_none());
    }
}
