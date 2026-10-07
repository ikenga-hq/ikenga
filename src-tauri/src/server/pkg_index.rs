//! The daemon's read-only index of the pkgs under `--pkgs-dir` (WP-19).
//!
//! Backs the `pkg_kernel_status`, `list_skill_actions`,
//! `list_all_skill_actions`, `pkg_settings_get` / `pkg_settings_set` and
//! `pkg_sidecar_call` RPC arms. Built once, in `create_router`, from the
//! same directory walk that feeds [`super::PkgStaticService`], so the two can
//! never disagree about which directories are pkgs.
//!
//! # What this is not
//!
//! Not a kernel. The daemon installs, trusts and enables nothing, and
//! supervises no pkg child (see `server::pkg_static`); the one thing it runs
//! is a one-shot `pkg_sidecar_call`, resolved through [`PkgIndex::sidecar`].
//! This index only answers "what is on disk, and what
//! would its UI routes be" — so the one registry it runs is the real
//! [`UiRoutesRegistry`], fed by the real `register()`. `pkg_kernel_status`
//! reports exactly that registry and no other: listing a desktop registry the
//! daemon never populates would claim an authoritative empty set for
//! something it does not do.
//!
//! # How each [`InstalledSummary`] field is filled
//!
//! The row is the same struct the desktop kernel returns, so every field has
//! to be something the daemon can state truthfully:
//!
//! * `source` — `InstallSource::Local { path }`. This IS a read-only
//!   discovery from a local directory, which is what `Local` means. A new
//!   variant (e.g. `daemon`) would be more specific but is a wire-contract
//!   change (`InstallSource` is mirrored in `tauri-cmd.ts`), so it is not
//!   introduced here.
//! * `enabled: true` — the daemon serves every pkg it discovers; there is no
//!   disable toggle to report.
//! * `project_id: None` — workspace scope. The daemon has no project model.
//! * `compatible` — the real [`Package::is_compatible`] check. An
//!   incompatible pkg is still reported (status says what is installed, and
//!   `compatible: false` is precisely how a caller learns why it is inert) but
//!   it never goes live: its UI routes are not registered and it contributes
//!   no skill actions — mirroring the desktop, where such a pkg never reaches
//!   the registries.
//! * `installed_at` — `manifest.json`'s mtime in **Unix milliseconds**, the
//!   unit the desktop stores (`Kernel::install_from_path` stamps
//!   `timestamp_millis()`). There is no install event on the daemon; the
//!   manifest's mtime is the nearest true fact. `0` if the filesystem cannot
//!   report it.
//!
//! Non-iframe pkgs (`component` / `webview` routes, or no UI at all) are
//! included: status reports what is installed. Whether the daemon can *serve*
//! an iframe bundle is `PkgStaticService`'s concern, not this one's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use tracing::{info, warn};

use crate::pkg::manifest::{Package, SettingsField, IKENGA_API_VERSION};
use crate::pkg::registries::{
    ActivityBarBadge, ActivityBarRegistry, UiRoutesRegistry, ViewsRegistry,
};
use crate::pkg::registry::Registry;
use crate::pkg::skill_actions::{list_actions_for_pkg, SkillAction};
use crate::pkg::{assemble_status, InstallSource, InstalledSummary, KernelStatus};

/// Walk `pkgs_dir` and load every valid pkg in it, sorted by directory path.
///
/// Deliberately forgiving: a directory that isn't a pkg, or a pkg whose
/// manifest doesn't parse, is skipped with a warning rather than failing the
/// daemon's startup. Ids are unique in the result — the kernel keys installs
/// by id, so a second directory claiming the same id is skipped (the first in
/// path order wins, which makes the choice deterministic across restarts).
pub fn scan(pkgs_dir: Option<&Path>) -> Vec<Package> {
    let Some(dir) = pkgs_dir else {
        return Vec::new();
    };
    if !dir.is_dir() {
        warn!(
            "--pkgs-dir {} is not a directory; no pkgs will be served or indexed",
            dir.display()
        );
        return Vec::new();
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!("--pkgs-dir {} unreadable: {e}", dir.display());
            return Vec::new();
        }
    };

    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        // `.staging-*` / `.backup-*` are the installer's scratch dirs on
        // desktop; they are never a pkg.
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .filter(|p| p.join("manifest.json").is_file())
        .collect();
    dirs.sort();

    let mut out: Vec<Package> = Vec::with_capacity(dirs.len());
    for path in dirs {
        let pkg = match Package::load(&path) {
            Ok(p) => p,
            Err(e) => {
                warn!("[pkg_index] skipping {}: {e:#}", path.display());
                continue;
            }
        };
        if let Some(first) = out.iter().find(|p| p.manifest.id == pkg.manifest.id) {
            warn!(
                "[pkg_index] duplicate pkg id {} — keeping {}, ignoring {}",
                pkg.manifest.id,
                first.install_path.display(),
                path.display()
            );
            continue;
        }
        out.push(pkg);
    }
    out
}

/// `manifest.json`'s mtime in Unix milliseconds, or `0` when the filesystem
/// can't say. See the module docs for why this stands in for `installed_at`.
fn manifest_mtime_ms(install_path: &Path) -> i64 {
    std::fs::metadata(install_path.join("manifest.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Discovered pkgs as `InstalledSummary` rows plus live `ui_routes`, `views`,
/// and `activity_bar` registries. Immutable after construction — the daemon has no install path.
#[derive(Default)]
pub struct PkgIndex {
    /// Sorted by id so `pkg_kernel_status` is stable across calls.
    installed: Vec<InstalledSummary>,
    ui_routes: UiRoutesRegistry,
    views: ViewsRegistry,
    activity_bar: ActivityBarRegistry,
    /// `pkg_id` → declared `settings.schema`, captured at index time the way
    /// the desktop `SettingsRegistry` captures it at register time — through
    /// the same `settings_values::declared_schema`, and only for pkgs that go
    /// live (compatible + registered).
    settings_schemas: HashMap<String, Vec<SettingsField>>,
    /// `pkg_id` → the loaded pkg, for live pkgs only (compatible +
    /// registered): what [`Self::sidecar`] resolves a `pkg_sidecar_call`
    /// against. An incompatible pkg never goes live, so it runs nothing —
    /// as on the desktop, where it never reaches the `SidecarsRegistry`.
    live: HashMap<String, Package>,
}

/// A sidecar binary [`PkgIndex::sidecar`] resolved: canonical, a regular
/// file, inside its pkg's canonical install dir.
#[derive(Debug, Clone)]
pub struct ResolvedSidecar {
    pub bin_path: PathBuf,
    pub install_path: PathBuf,
}

impl PkgIndex {
    pub fn from_packages(pkgs: &[Package]) -> Self {
        let ui_routes = UiRoutesRegistry::new();
        let views = ViewsRegistry::new();
        let activity_bar = ActivityBarRegistry::new();
        let mut settings_schemas = HashMap::new();
        let mut live = HashMap::new();
        let mut installed: Vec<InstalledSummary> = Vec::with_capacity(pkgs.len());
        for pkg in pkgs {
            let compatible = pkg.is_compatible();
            if compatible {
                // Same call the desktop kernel makes on install. `register`
                // validates every route before inserting any, so a failure
                // leaves nothing behind; the pkg is then not reported at all,
                // exactly as a desktop install that fails a registry is
                // rolled back and never recorded.
                if let Err(e) = ui_routes.register(pkg) {
                    warn!(
                        "[pkg_index] skipping {}: ui_routes rejected it: {e:#}",
                        pkg.manifest.id
                    );
                    continue;
                }
                if let Err(e) = views.register(pkg) {
                    warn!(
                        "[pkg_index] {}: no views entry (views rejected it): {e:#}",
                        pkg.manifest.id
                    );
                }
                if let Err(e) = activity_bar.register(pkg) {
                    warn!(
                        "[pkg_index] {}: no rail entry (activity_bar rejected it): {e:#}",
                        pkg.manifest.id
                    );
                }
                if let Some(schema) = crate::pkg::settings_values::declared_schema(pkg) {
                    settings_schemas.insert(pkg.manifest.id.clone(), schema);
                }
                live.insert(pkg.manifest.id.clone(), pkg.clone());
            } else {
                warn!(
                    "[pkg_index] {} declares ikenga_api={} outside this host's support window — \
                     reported as incompatible, not registered",
                    pkg.manifest.id, pkg.manifest.ikenga_api
                );
            }
            let install_path = pkg.install_path.display().to_string();
            installed.push(InstalledSummary {
                id: pkg.manifest.id.clone(),
                version: pkg.manifest.version.clone(),
                ikenga_api: pkg.manifest.ikenga_api.clone(),
                installed_at: manifest_mtime_ms(&pkg.install_path),
                enabled: true,
                compatible,
                source: InstallSource::Local {
                    path: install_path.clone(),
                },
                install_path,
                project_id: None,
            });
        }
        installed.sort_by(|a, b| a.id.cmp(&b.id));
        if !installed.is_empty() {
            info!(
                "[pkg_index] indexed {} pkg(s): {}",
                installed.len(),
                installed
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Self {
            installed,
            ui_routes,
            views,
            activity_bar,
            settings_schemas,
            live,
        }
    }

    /// Resolve `pkg_id`'s sidecar `name` the way the desktop
    /// `SidecarsRegistry` validates one at install — target triple,
    /// `{target}` expansion, a regular file that canonicalizes inside the
    /// pkg's install dir (so a symlink or `..` in `bin` cannot point it
    /// anywhere else) — but at call time and read-only: nothing is chmodded,
    /// since `--pkgs-dir` is the operator's and a T1 principal child cannot
    /// write it anyway. Only a sidecar the pkg ITSELF declares resolves, so a
    /// caller naming another pkg's sidecar gets "declares no sidecar".
    pub fn sidecar(&self, pkg_id: &str, name: &str) -> Result<ResolvedSidecar, String> {
        let pkg = self
            .live
            .get(pkg_id)
            .ok_or_else(|| format!("pkg `{pkg_id}` is not installed on this server"))?;
        let spec = pkg
            .manifest
            .sidecars
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("pkg `{pkg_id}` declares no sidecar `{name}`"))?;
        let host_target = super::shared::sidecar_call::host_target_triple();
        if !pkg.manifest.targets.is_empty() && !pkg.manifest.targets.contains(&host_target) {
            return Err(format!(
                "pkg `{pkg_id}` ships targets {:?}, this server is `{host_target}`",
                pkg.manifest.targets
            ));
        }
        let bin_rel = spec.bin.replace("{target}", &host_target);
        let bin_path = pkg
            .resolve_relative(&bin_rel)
            .map_err(|e| format!("sidecar `{name}` of `{pkg_id}`: {e:#}"))?;
        if !bin_path.is_file() {
            return Err(format!(
                "sidecar `{name}` of `{pkg_id}`: bin `{}` is not a file",
                bin_path.display()
            ));
        }
        let install_path = pkg
            .install_path
            .canonicalize()
            .map_err(|e| format!("pkg `{pkg_id}` install dir: {e}"))?;
        Ok(ResolvedSidecar {
            bin_path,
            install_path,
        })
    }

    /// The live pkg `pkg_id` (compatible + registered), if this index has it.
    pub fn live_pkg(&self, pkg_id: &str) -> Option<&Package> {
        self.live.get(pkg_id)
    }

    /// The declared settings schema for `pkg_id`, or `None` — for a pkg that
    /// declares none, and for one the index doesn't have (unknown or not
    /// live). `None` is what the desktop `SettingsRegistry::schema_for` gives
    /// in both cases too; `pkg_settings_get` then reports `schema: null`.
    pub fn settings_schema(&self, pkg_id: &str) -> Option<Vec<SettingsField>> {
        self.settings_schemas.get(pkg_id).cloned()
    }

    /// Rows for every discovered pkg.
    pub fn installed(&self) -> &[InstalledSummary] {
        &self.installed
    }

    /// Set an activity-bar badge for an installed pkg.
    pub fn set_badge(&self, pkg_id: &str, badge: Option<ActivityBarBadge>) -> anyhow::Result<bool> {
        self.activity_bar.set_badge(pkg_id, badge)
    }

    /// The `pkg_kernel_status` payload — through the same
    /// [`assemble_status`] the desktop `Kernel::status` uses, with the
    /// registries the daemon runs (`ui_routes`, `views`, `activity_bar`).
    pub fn status(&self) -> KernelStatus {
        assemble_status(
            self.installed.clone(),
            &[
                &self.ui_routes as &dyn Registry,
                &self.views as &dyn Registry,
                &self.activity_bar as &dyn Registry,
            ],
            IKENGA_API_VERSION,
        )
    }

    /// `list_skill_actions` against an explicit store root. Mirrors the
    /// desktop command: an unknown id returns `[]` (the desktop returns
    /// `Vec::new()` for a pkg that isn't installed, it never errors), and the
    /// manifest is re-read from disk so `requires` is current.
    pub fn skill_actions(&self, pkg_id: &str, store: Option<&Path>) -> Vec<SkillAction> {
        let Some(summary) = self.installed.iter().find(|s| s.id == pkg_id) else {
            tracing::warn!(%pkg_id, "list_skill_actions: pkg not installed");
            return Vec::new();
        };
        Self::actions_for(summary, store)
    }

    /// `list_all_skill_actions` against an explicit store root.
    pub fn all_skill_actions(&self, store: Option<&Path>) -> Vec<SkillAction> {
        self.installed
            .iter()
            .flat_map(|s| Self::actions_for(s, store))
            .collect()
    }

    fn actions_for(summary: &InstalledSummary, store: Option<&Path>) -> Vec<SkillAction> {
        // Incompatible pkgs never go live (module docs); on desktop they are
        // not in the installed set at all, so they contribute nothing.
        if !summary.compatible {
            return Vec::new();
        }
        let Some(store) = store else {
            return Vec::new();
        };
        let pkg = match Package::load(Path::new(&summary.install_path)) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(pkg_id = %summary.id, error = %format!("{e:#}"), "list_skill_actions: manifest load failed");
                return Vec::new();
            }
        };
        list_actions_for_pkg(&summary.id, &pkg.manifest.requires, store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(root: &Path, dir_name: &str, body: &str) -> PathBuf {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), body).unwrap();
        dir
    }

    fn manifest(id: &str, api: &str, extra: &str) -> String {
        format!(r#"{{"id":"{id}","name":"T","version":"0.1.0","ikenga_api":"{api}"{extra}}}"#)
    }

    #[test]
    fn scan_skips_non_pkgs_and_dedups_ids_in_path_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_manifest(root, "a-first", &manifest("com.test.dup", "1", ""));
        write_manifest(root, "b-second", &manifest("com.test.dup", "1", ""));
        write_manifest(root, ".staging-x", &manifest("com.test.staged", "1", ""));
        write_manifest(root, "broken", "{ not json");
        std::fs::create_dir_all(root.join("not-a-pkg")).unwrap();
        std::fs::write(root.join("loose.txt"), "x").unwrap();

        let pkgs = scan(Some(root));
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].manifest.id, "com.test.dup");
        assert!(pkgs[0].install_path.ends_with("a-first"));

        assert!(scan(None).is_empty());
        assert!(scan(Some(&root.join("missing"))).is_empty());
    }

    /// `settings_schema` holds what the desktop `SettingsRegistry` would: the
    /// declared fields of live pkgs only — none for an empty block, an
    /// incompatible pkg, or an unknown id.
    #[test]
    fn settings_schema_mirrors_the_desktop_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let settings =
            r#","settings":{"schema":[{"key":"k","type":"string","label":"K","default":"d"}]}"#;
        write_manifest(root, "live", &manifest("com.test.live", "1", settings));
        write_manifest(
            root,
            "future",
            &manifest("com.test.future", "999", settings),
        );
        write_manifest(
            root,
            "empty",
            &manifest("com.test.empty", "1", r#","settings":{"schema":[]}"#),
        );

        let pkgs = scan(Some(root));
        let index = PkgIndex::from_packages(&pkgs);
        let live = pkgs
            .iter()
            .find(|p| p.manifest.id == "com.test.live")
            .unwrap();
        let schema = index.settings_schema("com.test.live").expect("declared");
        assert_eq!(schema.len(), 1);
        assert_eq!(schema[0].key, "k");
        assert_eq!(
            serde_json::to_value(&schema).unwrap(),
            serde_json::to_value(crate::pkg::settings_values::declared_schema(live)).unwrap()
        );
        for id in ["com.test.future", "com.test.empty", "com.test.unknown"] {
            assert!(index.settings_schema(id).is_none(), "{id}");
        }
    }

    #[test]
    fn index_reports_every_valid_pkg_but_registers_only_compatible_routes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let route =
            r#","ui":{"routes":[{"path":"/x","kind":"iframe","source":"dist/index.html"}]}"#;
        write_manifest(root, "iframe", &manifest("com.test.iframe", "1", route));
        write_manifest(
            root,
            "comp",
            &manifest(
                "com.test.comp",
                "1",
                r#","ui":{"routes":[{"path":"/c","kind":"component","source":"C"}]}"#,
            ),
        );
        write_manifest(root, "noui", &manifest("com.test.noui", "1", ""));
        write_manifest(root, "future", &manifest("com.test.future", "999", route));

        let index = PkgIndex::from_packages(&scan(Some(root)));
        let ids: Vec<&str> = index.installed().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "com.test.comp",
                "com.test.future",
                "com.test.iframe",
                "com.test.noui"
            ]
        );

        let future = &index.installed()[1];
        assert!(!future.compatible);
        let iframe = &index.installed()[2];
        assert!(iframe.compatible && iframe.enabled && iframe.project_id.is_none());
        assert_eq!(
            iframe.source,
            InstallSource::Local {
                path: iframe.install_path.clone()
            }
        );
        assert!(iframe.installed_at > 0, "manifest mtime in ms");

        let wire = serde_json::to_value(index.status()).unwrap();
        let mut routed: Vec<&str> = wire["registries"]["ui_routes"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["pkg_id"].as_str().unwrap())
            .collect();
        routed.sort_unstable();
        assert_eq!(
            routed,
            vec!["com.test.comp", "com.test.iframe"],
            "incompatible pkg must not go live"
        );
        assert!(wire["registries"].get("views").is_some());
        assert!(wire["registries"].get("activity_bar").is_some());
    }

    #[test]
    fn skill_actions_follow_requires_into_the_store_and_unknown_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path().join("pkgs");
        write_manifest(
            &pkgs,
            "skilled",
            &manifest(
                "com.test.skilled",
                "1",
                r#","requires":[{"kind":"skill","name":"pa"}]"#,
            ),
        );
        let store = tmp.path().join("store");
        let actions = store.join("skills").join("pa").join("actions");
        std::fs::create_dir_all(&actions).unwrap();
        std::fs::write(
            actions.join("send.md"),
            "---\nname: send\nux_mode: confirm\n---\nbody\n",
        )
        .unwrap();

        let index = PkgIndex::from_packages(&scan(Some(&pkgs)));
        let found = index.skill_actions("com.test.skilled", Some(&store));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].verb, "send");
        assert_eq!(found[0].pkg_id, "com.test.skilled");

        assert!(index
            .skill_actions("com.test.nope", Some(&store))
            .is_empty());
        assert!(index.skill_actions("com.test.skilled", None).is_empty());
        assert_eq!(index.all_skill_actions(Some(&store)).len(), 1);
    }
}
