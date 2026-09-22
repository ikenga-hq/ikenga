//! WP-28 fixture/parity tests (G-MANIFEST-V5 §2/§4) — canonical fixture set.
//!
//! `@ikenga/contract@0.19` owns the fixtures at
//! `contract/src/__fixtures__/manifest-v5/`; its README pins a
//! verdict-by-folder contract:
//!
//! | folder      | expected verdict                                        |
//! |-------------|---------------------------------------------------------|
//! | `valid/`    | parses                                                  |
//! | `invalid/`  | rejected (Zod `.parse` fails)                           |
//! | `alias/`    | parses; the `ui.nav`→`ui.views` alias applies (§4)      |
//!
//! This module is the Rust side of that Rust↔Zod parity check. It walks the
//! same folders out of the sibling workspace checkout via
//! `CARGO_MANIFEST_DIR`-relative paths (same convention the
//! `ikenga-pkgs` fleet test uses) and additionally pins the snapshot wire
//! shape the TS consumers destructure — the snake_case keys asserted below
//! are exactly the fields read in `src/lib/pkg/use-activity-bar-entries.ts`
//! (`PkgViewEntry`, `PkgActivityBarEntry`, `PkgNavEntry`) and the Explorer
//! Views section. A drifted field name fails here first.
//!
//! One deliberate layer difference: the Zod `UiBlockSchema.superRefine`
//! rejects an undeclared `views[].route` at `.parse`; the Rust parser keeps
//! serde shape-only and enforces the same rule in `ViewsRegistry::register`
//! (§2b — see `ViewEntry.route`). The `invalid/view-route-*` fixtures are
//! therefore asserted parse-ok + register-err — the manifest is still
//! rejected before the pkg can serve a contribution, one layer later.

#![cfg(test)]

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::manifest::{Manifest, NavAliasOutcome, Package};
use super::registries::{
    ActivityBarRegistry, CompanionPanelsRegistry, ContextActionsRegistry, ExplorerSectionsRegistry,
    ViewsRegistry, WidgetsRegistry,
};
use super::registry::Registry;

/// `contract/` is a sibling of `shell/` in the workspace root — the same
/// sibling-checkout convention `ikenga_pkgs_fleet_parses_and_registers` uses.
const FIXTURE_ROOT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contract/src/__fixtures__/manifest-v5"
);

fn fixture_root() -> PathBuf {
    Path::new(FIXTURE_ROOT)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(FIXTURE_ROOT))
}

/// Sorted `*.json` paths in one verdict folder. `None` when the contract
/// checkout is absent (standalone crate build — same skip convention as the
/// fleet test; a present-but-unreadable dir is a hard failure).
fn folder_paths(folder: &str) -> Option<Vec<PathBuf>> {
    let dir = fixture_root().join(folder);
    if !dir.is_dir() {
        eprintln!(
            "manifest_v5_parity: skipping `{folder}` — {} not found \
             (sibling contract/ checkout absent)",
            dir.display()
        );
        return None;
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    Some(paths)
}

fn read_fixture(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn fixture_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn pkg_from_json(json: &str) -> Package {
    let mut manifest: Manifest =
        serde_json::from_str(json).expect("fixture manifest must deserialize");
    // `Package::load` is the only site that applies the alias — replicate it
    // here so fixture tests exercise the same normalization path.
    manifest.apply_nav_views_alias();
    Package {
        manifest,
        install_path: PathBuf::from("/tmp/_fixture"),
    }
}

/// The sorted key set of a serialized object — compared verbatim against the
/// wire shape the TS hook destructures.
fn sorted_keys(v: &Value) -> Vec<String> {
    let mut keys: Vec<String> = v
        .as_object()
        .expect("registry entry must serialize to an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// Register the pkg against the v5 contribution surface — the same six
/// manifest→registry projections the fleet test drives.
fn register_all(pkg: &Package) -> Vec<Box<dyn Registry>> {
    let registries: Vec<Box<dyn Registry>> = vec![
        Box::new(ViewsRegistry::new()),
        Box::new(ActivityBarRegistry::new()),
        Box::new(ExplorerSectionsRegistry::new()),
        Box::new(CompanionPanelsRegistry::new()),
        Box::new(ContextActionsRegistry::new()),
        Box::new(WidgetsRegistry::new()),
    ];
    for reg in &registries {
        reg.register(pkg)
            .unwrap_or_else(|e| panic!("{} rejected {}: {e}", reg.name(), pkg.manifest.id));
    }
    registries
}

// ── Verdict-by-folder contract ───────────────────────────────────────────────

/// `valid/` — every fixture parses, and (as a stronger check than the Zod
/// side can make alone) registers cleanly across the v5 contribution
/// surface.
#[test]
fn valid_fixtures_parse_and_register() {
    let Some(paths) = folder_paths("valid") else {
        return;
    };
    // Guard against an emptied/mispointed folder silently passing.
    assert!(
        paths.len() >= 8,
        "expected the canonical valid/ set ({} files on contract@0.19), found {}",
        8,
        paths.len()
    );
    for path in paths {
        let name = fixture_name(&path);
        let pkg = pkg_from_json(&read_fixture(&path));
        register_all(&pkg);
        eprintln!("valid/{name}: parsed + registered");
    }
}

/// `invalid/` — rejected. Shape-level violations fail at `Manifest`
/// deserialize; the two `view-route-*` fixtures fail one layer later at
/// `ViewsRegistry::register` (§2b — the Rust check is deliberately deferred,
/// see module docs). Both verdicts mean "the pkg cannot serve the
/// contribution".
#[test]
fn invalid_fixtures_are_rejected() {
    /// Files whose rejection happens at registry registration, not parse.
    const REGISTER_FAILS: &[&str] =
        &["view-route-no-routes.json", "view-route-undeclared.json"];

    let Some(paths) = folder_paths("invalid") else {
        return;
    };
    assert!(
        paths.len() >= 10,
        "expected the canonical invalid/ set (10 files on contract@0.19), found {}",
        paths.len()
    );
    let mut covered: Vec<String> = Vec::new();
    for path in paths {
        let name = fixture_name(&path);
        let json = read_fixture(&path);
        if REGISTER_FAILS.contains(&name.as_str()) {
            let pkg = pkg_from_json(&json);
            let err = ViewsRegistry::new()
                .register(&pkg)
                .expect_err(&format!("invalid/{name} must fail view-route validation"));
            assert!(
                err.to_string().contains("route"),
                "invalid/{name}: register error should name the route rule, got: {err}"
            );
        } else {
            assert!(
                serde_json::from_str::<Manifest>(&json).is_err(),
                "invalid/{name} must fail Manifest parse"
            );
        }
        covered.push(name);
    }
    // Keep the classification honest: if the contract set adds a new
    // register-time fixture (or renames one) the table must be updated.
    for expected in REGISTER_FAILS {
        assert!(
            covered.iter().any(|n| n == expected),
            "canonical invalid/{expected} missing — fixture set drifted"
        );
    }
}

/// `alias/` — parses, and `apply_nav_views_alias` maps `ui.nav` → `ui.views`.
/// `nav-only.json` is api=1 on purpose: the real alias population is the
/// api=1..4 fleet still declaring `ui.nav` (README §contract).
#[test]
fn alias_fixture_nav_only_applies() {
    let Some(paths) = folder_paths("alias") else {
        return;
    };
    assert_eq!(
        paths.len(),
        1,
        "alias/ is a single-fixture folder on contract@0.19"
    );
    let mut manifest: Manifest =
        serde_json::from_str(&read_fixture(&paths[0])).expect("alias/nav-only must parse");
    assert_eq!(
        manifest.apply_nav_views_alias(),
        Some(NavAliasOutcome::Applied)
    );

    let views = &manifest.ui.as_ref().unwrap().views;
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].id, "home");
    assert_eq!(views[0].title, "Home");
    assert_eq!(views[0].route, "/home");
    // The alias pins exactly the rail claim (views[0]).
    assert!(views[0].pin_on_install);
    assert_eq!(views[1].id, "settings");
    assert!(!views[1].pin_on_install);

    // …and the aliased pkg registers across the whole v5 surface.
    register_all(&Package {
        manifest,
        install_path: PathBuf::from("/tmp/_fixture"),
    });
}

/// `valid/v4-manifest.json` — api=4 stays inside the compat window, and its
/// `ui.nav` aliases (the alias is not v5-gated; api=1..4 is the population it
/// exists for).
#[test]
fn v4_fixture_stays_compatible_and_aliases() {
    let json = read_fixture(&fixture_root().join("valid/v4-manifest.json"));
    let manifest: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(manifest.ikenga_api, "4");
    let pkg = Package {
        manifest,
        install_path: PathBuf::from("/tmp/_fixture"),
    };
    assert!(
        pkg.is_compatible(),
        "api=4 must remain inside [IKENGA_API_MIN_SUPPORTED, IKENGA_API_VERSION]"
    );
    let mut manifest = pkg.manifest;
    assert_eq!(
        manifest.apply_nav_views_alias(),
        Some(NavAliasOutcome::Applied),
        "api=4 ui.nav aliases to views"
    );
    let views = &manifest.ui.as_ref().unwrap().views;
    assert_eq!(views[0].id, "home");
    assert!(views[0].pin_on_install);
}

/// `valid/nav-and-views.json` — when both are declared, `views` wins and
/// `nav` is ignored (§4: the alias never fails a parse).
#[test]
fn nav_and_views_declared_views_win() {
    let json = read_fixture(&fixture_root().join("valid/nav-and-views.json"));
    let mut manifest: Manifest = serde_json::from_str(&json).unwrap();
    assert_eq!(
        manifest.apply_nav_views_alias(),
        Some(NavAliasOutcome::IgnoredBothDeclared)
    );
    let views = &manifest.ui.as_ref().unwrap().views;
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].id, "grid");
}

// ── Snapshot wire shapes (the fields the TS consumers destructure) ───────────

/// `views` snapshot entries carry exactly the keys `PkgViewEntry`
/// destructures — and `pin_on_install` survives the manifest→registry→
/// snapshot bridge. Fixture: `valid/views.json` (2 views, declaration
/// order: `grid` is views[0], the rail claim).
#[test]
fn views_snapshot_wire_shape_and_pin_on_install() {
    let reg = ViewsRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/views.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    assert_eq!(snap["count"], 2);
    let entries = snap["entries"].as_array().unwrap();
    // Declaration order is preserved — `grid` is views[0], the rail claim.
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec![
            "icon",
            "id",
            "pane_route",
            "pin_on_install",
            "pkg_id",
            "pkg_name",
            "qualified_id",
            "route",
            "title"
        ]
    );
    assert_eq!(first["qualified_id"], "com.ikenga.views-demo:grid");
    assert_eq!(first["route"], "/grid");
    assert_eq!(first["pane_route"], "/pkg/com.ikenga.views-demo/grid");
    assert_eq!(first["pin_on_install"], true);
    assert_eq!(entries[1]["pin_on_install"], false);
}

/// Fixture: `valid/explorer-sections.json` — explicit `order` on the first
/// entry, omitted (→ `null` on the wire) on the second.
#[test]
fn explorer_sections_snapshot_wire_shape() {
    let reg = ExplorerSectionsRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/explorer-sections.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec![
            "data_route",
            "decl_index",
            "icon",
            "id",
            "order",
            "pkg_id",
            "qualified_id",
            "title"
        ]
    );
    assert_eq!(first["qualified_id"], "com.ikenga.explorer-demo:open-tasks");
    assert_eq!(first["decl_index"], 0);
    assert_eq!(first["order"], 10);
    assert_eq!(
        first["data_route"],
        "/pkg/com.ikenga.explorer-demo/sections/open-tasks"
    );
    // Undeclared `order` surfaces as `null`, not a synthesized index.
    assert_eq!(entries[1]["order"], Value::Null);
    assert_eq!(entries[1]["decl_index"], 1);
}

/// Fixture: `valid/companion-panels.json` — `session_scoped` true on the
/// first panel, defaulting to false on the second.
#[test]
fn companion_panels_snapshot_wire_shape() {
    let reg = CompanionPanelsRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/companion-panels.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec![
            "icon",
            "id",
            "pane_route",
            "pkg_id",
            "qualified_id",
            "route",
            "session_scoped",
            "title"
        ]
    );
    assert_eq!(
        first["qualified_id"],
        "com.ikenga.companion-demo:session-state"
    );
    assert_eq!(first["session_scoped"], true);
    assert_eq!(entries[1]["session_scoped"], false);
}

/// Tagged unions serialize with the `kind` discriminator the TS side
/// switches on. Fixture: `valid/context-actions.json` — all four selector
/// kinds (`file`/`artifact`/`session`/`ngwa-item`) and both run kinds.
#[test]
fn context_actions_snapshot_wire_shape_and_tagged_unions() {
    let reg = ContextActionsRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/context-actions.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec!["id", "label", "pkg_id", "qualified_id", "run", "when"]
    );
    assert_eq!(
        first["when"],
        serde_json::json!({"kind": "file", "glob": "*.md"})
    );
    assert_eq!(
        first["run"],
        serde_json::json!({
            "kind": "dispatch",
            "prompt": "Review {{file.path}} in {{project.root}}",
            "target": "chi"
        })
    );
    assert_eq!(entries[1]["when"], serde_json::json!({"kind": "artifact"}));
    assert_eq!(entries[2]["when"], serde_json::json!({"kind": "session"}));
    assert_eq!(
        entries[3]["when"],
        serde_json::json!({"kind": "ngwa-item", "kinds": ["task", "alert"]})
    );
    assert_eq!(
        entries[1]["run"],
        serde_json::json!({"kind": "view", "route": "/review"})
    );
}

/// Fixture: `valid/widgets.json` — all three spans; the middle entry relies
/// on the `medium` default.
#[test]
fn widgets_snapshot_wire_shape_and_span_values() {
    let reg = WidgetsRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/widgets.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec![
            "id",
            "pane_route",
            "pkg_id",
            "qualified_id",
            "route",
            "span",
            "title"
        ]
    );
    assert_eq!(first["span"], "small");
    // The `span` default — declared without it — serializes as "medium".
    assert_eq!(entries[1]["span"], "medium");
    assert_eq!(entries[2]["span"], "wide");
}

/// The rail-claim projection (`activity_bar`) is views-sourced post-WP-28:
/// `route` is views[0]'s *pane* route and `nav` carries the full views list
/// mapped onto the NavEntry wire shape the pkg-mode sidebar reads.
/// Fixture: `valid/views.json`.
#[test]
fn activity_bar_snapshot_is_views_sourced() {
    let reg = ActivityBarRegistry::new();
    reg.register(&pkg_from_json(&read_fixture(
        &fixture_root().join("valid/views.json"),
    )))
    .unwrap();
    let snap = reg.snapshot();

    let first = &snap["entries"][0];
    let mut keys = sorted_keys(first);
    keys.retain(|k| k != "badge" && k != "parked" && k != "parked_reason");
    assert_eq!(
        keys,
        vec!["icon", "id", "label", "nav", "pkg_id", "pkg_name", "route", "section"]
    );
    assert_eq!(first["route"], "/pkg/com.ikenga.views-demo/grid");
    let nav = first["nav"].as_array().unwrap();
    assert_eq!(nav.len(), 2);
    assert_eq!(
        sorted_keys(&nav[0]),
        vec!["icon", "id", "label", "route", "section"]
    );
    assert_eq!(nav[0]["label"], "Grid");
    assert_eq!(nav[0]["route"], "/pkg/com.ikenga.views-demo/grid");
}

/// `valid/full.json` — every contribution block in one manifest: parse,
/// alias is a no-op (no `nav`), all six registries accept it.
#[test]
fn full_fixture_registers_everything() {
    let mut manifest: Manifest =
        serde_json::from_str(&read_fixture(&fixture_root().join("valid/full.json"))).unwrap();
    assert_eq!(manifest.ikenga_api, "5");
    assert_eq!(manifest.apply_nav_views_alias(), None);
    register_all(&Package {
        manifest,
        install_path: PathBuf::from("/tmp/_fixture"),
    });
}
