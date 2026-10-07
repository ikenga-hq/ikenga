//! Ngwa unified snapshot command (WP-14, review fixes WP-14a).
//!
//! The wire types and the pure join ([`build_snapshot`]) live in the ungated
//! `server::shared::ngwa`, so the daemon's `/api/rpc` arm runs the very same
//! join; everything there is re-exported here, so `crate::commands::ngwa::*`
//! paths (the iyke bridge, tests) keep resolving unchanged. What stays in
//! this file is the desktop's input collection: the live pkg kernel, trust
//! evaluation against `app_data_dir`, and the transcript usage scan.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, Manager, State};

pub use crate::server::shared::ngwa::*;

use crate::commands::claude_config;
use crate::commands::claude_store;
use crate::commands::db::PaDb;
use crate::commands::pkg::KernelState;
use crate::commands::projects;
use crate::pkg::kernel::KernelStatus;
use crate::pkg::manifest::Package;
use crate::pkg::trust;
use crate::transcript::usage;

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
            tracing::warn!("[ngwa_snapshot] transcript scan failed: {e}");
            UsageInput::unavailable(format!("transcript scan failed: {e}"))
        }
        Ok(report) => match usage::load_usage_snapshot(pool, now_ms).await {
            Ok(snapshot) => UsageInput {
                snapshot,
                error: report.error_summary(),
            },
            Err(e) => {
                tracing::warn!("[ngwa_snapshot] load usage snapshot failed: {e}");
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
    let status = kernel_status_off_runtime(kernel.0.clone()).await?;
    ngwa_snapshot_inner(status, db.inner(), &app_data_dir, usage::claude_projects_dir()).await
}

/// `Kernel::status()` walks every registry's sync `snapshot()`, and some of
/// those read SQLite with a blocking `block_on`. Called straight from async
/// code on a tokio worker that panics ("Cannot start a runtime from within a
/// runtime") — the v0.18.4 Installed-tab hang. Run it on the blocking pool.
pub async fn kernel_status_off_runtime(
    kernel: Arc<crate::pkg::kernel::Kernel>,
) -> Result<KernelStatus, String> {
    tokio::task::spawn_blocking(move || kernel.status())
        .await
        .map_err(|e| format!("kernel status task failed: {e}"))
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
        not_served: NotServed::default(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::commands::claude_config::{
        AgentEntry, ClaudeConfig, CommandEntry, HookEntry, McpEntry, Scope as ConfigScope,
        SkillEntry, SystemTag,
    };
    use crate::commands::claude_store::ClaudeStoreEntry;
    use crate::commands::engine_layout::{
        engine_layouts, ConfigFormat, EngineId, KindStatus, PrimitiveKind,
    };
    use crate::pkg::manifest::Manifest;
    use crate::pkg::source::InstallSource;
    use crate::pkg::status::InstalledSummary;
    use crate::pkg::trust::TrustState;
    use crate::transcript::usage::{UsageKind, UsageSnapshot};
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
            not_served: NotServed::default(),
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

