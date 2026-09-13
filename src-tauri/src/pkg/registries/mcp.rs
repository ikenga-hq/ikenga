//! MCP registry — wires package-contributed MCP servers into Claude Code's
//! global MCP config (`~/.claude.json:mcpServers`).
//!
//! Manifest contributes stdio-style entries: `{ name, command, args, env }`.
//! Each entry is keyed in `mcpServers` as `pkg-<slug>-<name>` so it can't
//! collide with the user's own hand-configured MCPs. Uninstall removes only
//! those keys — anything else in `mcpServers` is left alone.
//!
//! Concurrency: `~/.claude.json` is shared with Claude Code itself. Writes go
//! through a temp-file + rename so a partial write can never leave the file
//! corrupted. Reads are tolerant of a missing file (treated as empty config).
//! There is no cross-process lock — we accept that a simultaneous edit by
//! Claude Code could clobber our changes; in practice that only happens via
//! `claude mcp add` which is rare. If we hit one we'll add an `flock(2)`.
//!
//! Why ~/.claude.json and not `~/.claude/settings.json`: Claude Code reads
//! MCP servers from `mcpServers` in the home `.claude.json` file (verified
//! via the user's existing `exa` + `pencil` entries). settings.json doesn't
//! carry MCPs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::pkg::engine_adapter::{EngineAdaptersRegistry, InstallReport};
use crate::pkg::manifest::{McpServer, Package};
use crate::pkg::registry::Registry;

#[derive(Debug, Clone, Serialize)]
pub struct McpEntry {
    pub pkg_id: String,
    /// Logical name from the manifest (e.g. `royalti-cms`).
    pub name: String,
    /// Key written into `~/.claude.json:mcpServers` (e.g. `pkg-com-royalti-cms-royalti-cms`).
    pub key: String,
}

pub struct McpRegistry {
    /// `pkg_id` → list of registered entries. Used for snapshot + uninstall.
    entries: RwLock<HashMap<String, Vec<McpEntry>>>,
    /// ADR-012 Track D: per-pkg, per-engine fan-out reports. Populated when
    /// `register()` walks the engine adapters after its own kernel-side
    /// write completes. Surfaced via `snapshot()` for Track E's UI to
    /// render "what this pkg wrote into which engine's settings file".
    ///
    /// Outer key: `pkg_id`. Inner key: `engine_id` (e.g. `"claude-code"`).
    adapter_reports: RwLock<HashMap<String, HashMap<String, InstallReport>>>,
    /// Shared handle to the kernel's engine adapter registry. v1 holds
    /// exactly one adapter (`ClaudeCodeAdapter`); fan-out grows naturally
    /// as Gemini / Codex adapters land. Optional for the `Default` impl
    /// used by tests that don't care about adapter fan-out.
    adapters: Arc<EngineAdaptersRegistry>,
}

impl Default for McpRegistry {
    fn default() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            adapter_reports: RwLock::new(HashMap::new()),
            adapters: Arc::new(EngineAdaptersRegistry::new()),
        }
    }
}

impl McpRegistry {
    /// Construct with the kernel-wide adapter registry. The Arc clone is
    /// cheap (refcount); the registry itself is owned by `lib.rs::run`.
    pub fn new(adapters: Arc<EngineAdaptersRegistry>) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            adapter_reports: RwLock::new(HashMap::new()),
            adapters,
        }
    }

    fn config_path() -> Result<PathBuf> {
        let home = crate::platform::home_dir().ok_or_else(|| {
            anyhow!("could not resolve home directory (HOME / USERPROFILE unset)")
        })?;
        Ok(home.join(".claude.json"))
    }

    fn key_for(pkg: &Package, server: &McpServer) -> String {
        format!("pkg-{}-{}", pkg.slug(), server.name)
    }

    /// Read `~/.claude.json` into a JSON object. A missing file is treated as
    /// an empty `{}` so first-install works on a clean machine.
    fn load_config(path: &PathBuf) -> Result<Map<String, Value>> {
        match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => Ok(Map::new()),
            Ok(s) => {
                let v: Value = serde_json::from_str(&s)
                    .with_context(|| format!("parse {}", path.display()))?;
                match v {
                    Value::Object(m) => Ok(m),
                    other => Err(anyhow!(
                        "{} is not a JSON object (got {:?})",
                        path.display(),
                        other
                    )),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
            Err(e) => Err(anyhow!("read {}: {e}", path.display())),
        }
    }

    /// Atomic write: serialize → temp file in same dir → rename. The rename
    /// is atomic on POSIX, so readers either see the old or the new file —
    /// never a half-written one.
    fn save_config(path: &PathBuf, cfg: &Map<String, Value>) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("config path has no parent"))?;
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        let pretty = serde_json::to_string_pretty(&Value::Object(cfg.clone()))
            .map_err(|e| anyhow!("serialize claude config: {e}"))?;
        let tmp = path.with_extension("json.pkg-tmp");
        std::fs::write(&tmp, pretty).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }
}

impl Registry for McpRegistry {
    fn name(&self) -> &'static str {
        "mcp"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        if pkg.manifest.mcp.is_empty() {
            return Ok(());
        }

        // Build the new entries first so a single bad shape aborts cleanly,
        // before we touch ~/.claude.json.
        let mut new_keys: Vec<(String, McpEntry, Value)> =
            Vec::with_capacity(pkg.manifest.mcp.len());
        for server in &pkg.manifest.mcp {
            if server.name.is_empty() {
                return Err(anyhow!("mcp entry of `{}` has empty name", pkg.manifest.id));
            }
            if server.command.is_empty() {
                return Err(anyhow!(
                    "mcp `{}` of `{}` has empty command",
                    server.name,
                    pkg.manifest.id
                ));
            }
            let key = Self::key_for(pkg, server);
            let mut value = json!({
                "type": "stdio",
                "command": server.command,
                "args": server.args,
                "env": server.env,
            });
            if !pkg.install_path.as_os_str().is_empty() {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "cwd".to_string(),
                        Value::String(pkg.install_path.to_string_lossy().into_owned()),
                    );
                }
            }
            new_keys.push((
                key.clone(),
                McpEntry {
                    pkg_id: pkg.manifest.id.clone(),
                    name: server.name.clone(),
                    key,
                },
                value,
            ));
        }

        let path = Self::config_path()?;
        let mut cfg = Self::load_config(&path)?;
        let servers = cfg
            .entry("mcpServers".to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        let servers_map = match servers {
            Value::Object(m) => m,
            _ => {
                return Err(anyhow!(
                    "{} `mcpServers` is not an object — refusing to overwrite",
                    path.display()
                ));
            }
        };

        // Idempotency: if the key exists and is byte-equal to what we'd
        // write, it's a re-register (boot replay) — leave it. If it exists
        // with different content, replace (could happen if the manifest's
        // server entry changed across versions).
        for (key, _entry, value) in &new_keys {
            servers_map.insert(key.clone(), value.clone());
        }

        Self::save_config(&path, &cfg)?;

        let mut map = self
            .entries
            .write()
            .map_err(|_| anyhow!("mcp registry lock poisoned"))?;
        map.insert(
            pkg.manifest.id.clone(),
            new_keys.into_iter().map(|(_, e, _)| e).collect(),
        );
        drop(map);

        // ADR-012 §4: fan out to every installed engine adapter so the
        // server entry also lands in each engine's external settings file
        // (`~/.claude/settings.json`, future: `~/.gemini/...`, `~/.codex/...`).
        // The kernel-side write above already succeeded; adapter failures
        // are best-effort — surfaced as warnings on the InstallReport, not
        // propagated up. This mirrors the ADR's "v1 ships with disabled =
        // true as default for long-lived servers" framing: external configs
        // are convenience, not load-bearing for runtime.
        let mut per_engine: HashMap<String, InstallReport> = HashMap::new();
        for adapter in self.adapters.iter() {
            let engine_id = adapter.id().to_string();
            let mut bucket = InstallReport::default();
            for server in &pkg.manifest.mcp {
                match adapter.register_mcp_server(server, &pkg.manifest.id, &pkg.slug()) {
                    Ok(r) => bucket.merge(r),
                    Err(e) => {
                        log::warn!(
                            "[pkg.mcp] engine `{engine_id}` register `{}` for pkg `{}` failed: {e:#}",
                            server.name,
                            pkg.manifest.id
                        );
                        bucket.warnings.push(format!(
                            "engine `{engine_id}` register `{}` failed: {e}",
                            server.name
                        ));
                    }
                }
            }
            per_engine.insert(engine_id, bucket);
        }
        if !per_engine.is_empty() {
            let mut reports = self
                .adapter_reports
                .write()
                .map_err(|_| anyhow!("mcp adapter_reports lock poisoned"))?;
            reports.insert(pkg.manifest.id.clone(), per_engine);
        }
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let removed: Vec<McpEntry> = self
            .entries
            .write()
            .map_err(|_| anyhow!("mcp registry lock poisoned"))?
            .remove(pkg_id)
            .unwrap_or_default();
        // Clear the per-pkg adapter reports map regardless — even if the
        // kernel-side entry was already gone, prior fan-out reports should
        // not linger.
        if let Ok(mut reports) = self.adapter_reports.write() {
            reports.remove(pkg_id);
        }
        if removed.is_empty() {
            return Ok(());
        }
        let path = Self::config_path()?;
        let mut cfg = match Self::load_config(&path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!(
                    "[pkg.mcp] load {} failed during uninstall: {e:#}",
                    path.display()
                );
                return Ok(());
            }
        };
        if let Some(Value::Object(servers)) = cfg.get_mut("mcpServers") {
            for e in &removed {
                servers.remove(&e.key);
            }
        }
        if let Err(e) = Self::save_config(&path, &cfg) {
            log::warn!(
                "[pkg.mcp] save {} failed during uninstall: {e:#}",
                path.display()
            );
        }

        // ADR-012 §4: fan out unregister to every adapter. Best-effort —
        // we don't have the original pkg slug here, but `McpEntry.key` was
        // built as `pkg-<slug>-<name>` (Self::key_for); recover the slug
        // from the entry's stored key. The TS adapter's `unregisterMcpServer`
        // takes server-name + pkg-slug separately, so we need both. We
        // derive slug by stripping the `pkg-` prefix and the trailing
        // `-<name>`.
        for adapter in self.adapters.iter() {
            for entry in &removed {
                let slug = entry
                    .key
                    .strip_prefix("pkg-")
                    .and_then(|s| s.strip_suffix(&format!("-{}", entry.name)))
                    .unwrap_or_default();
                if let Err(e) = adapter.unregister_mcp_server(&entry.name, &entry.pkg_id, slug) {
                    log::warn!(
                        "[pkg.mcp] engine `{}` unregister `{}` for pkg `{}` failed: {e:#}",
                        adapter.id(),
                        entry.name,
                        entry.pkg_id
                    );
                }
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let map = match self.entries.read() {
            Ok(g) => g,
            Err(_) => return json!({ "error": "lock poisoned" }),
        };
        let entries: Vec<Value> = map
            .values()
            .flatten()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect();
        // Per ADR-012 Track E: surface per-(pkg, engine) reports so the pkg
        // manager UI can render "this pkg wrote ... into engine X".
        let adapter_reports = match self.adapter_reports.read() {
            Ok(g) => serde_json::to_value(&*g).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        };
        json!({
            "count": entries.len(),
            "entries": entries,
            "config_path": Self::config_path().map(|p| p.display().to_string()).unwrap_or_default(),
            "adapter_reports": adapter_reports,
        })
    }
}
