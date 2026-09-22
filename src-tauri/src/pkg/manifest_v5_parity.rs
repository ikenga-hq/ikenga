//! WP-28 fixture/parity tests (G-MANIFEST-V5 §2/§4).
//!
//! `@ikenga/contract@0.19` is not published yet, so these tests pin the Rust
//! serialization shape against the TypeScript consumers by hand: the
//! snake_case keys asserted here are exactly the fields read in
//! `src/lib/pkg/use-activity-bar-entries.ts` (`PkgViewEntry`,
//! `PkgActivityBarEntry`, `PkgNavEntry`) and the Explorer Views section.
//! When the contract package lands, its Zod schemas replace this file's
//! expectations; until then a drifted field name fails here first.
//!
//! Fixtures live in `testdata/manifest-v5/` and are embedded with
//! `include_str!` so the tests never depend on cwd or a sibling checkout.

#![cfg(test)]

use std::path::PathBuf;

use serde_json::Value;

use super::manifest::{Manifest, NavAliasOutcome, Package};
use super::registries::{
    ActivityBarRegistry, CompanionPanelsRegistry, ContextActionsRegistry, ExplorerSectionsRegistry,
    ViewsRegistry, WidgetsRegistry,
};
use super::registry::Registry;

const FULL_V5: &str = include_str!("testdata/manifest-v5/full-v5.manifest.json");
const LEGACY_NAV: &str = include_str!("testdata/manifest-v5/legacy-nav.manifest.json");
const API4: &str = include_str!("testdata/manifest-v5/api4-window.manifest.json");

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

#[test]
fn all_fixtures_parse() {
    for (name, json) in [
        ("full-v5", FULL_V5),
        ("legacy-nav", LEGACY_NAV),
        ("api4-window", API4),
    ] {
        serde_json::from_str::<Manifest>(json)
            .unwrap_or_else(|e| panic!("fixture {name} must parse: {e}"));
    }
}

#[test]
fn api4_fixture_stays_compatible_inside_the_window() {
    let pkg = pkg_from_json(API4);
    assert_eq!(pkg.manifest.ikenga_api, "4");
    assert!(
        pkg.is_compatible(),
        "api=4 must remain inside [IKENGA_API_MIN_SUPPORTED, IKENGA_API_VERSION]"
    );
}

/// `views` snapshot entries carry exactly the keys `PkgViewEntry`
/// destructures — and `pin_on_install` survives the manifest→registry→
/// snapshot bridge.
#[test]
fn views_snapshot_wire_shape_and_pin_on_install() {
    let reg = ViewsRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    assert_eq!(snap["count"], 2);
    let entries = snap["entries"].as_array().unwrap();
    // Declaration order is preserved — `jobs` is views[0], the rail claim.
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
    assert_eq!(first["qualified_id"], "com.ikenga.agentops-fixture:jobs");
    assert_eq!(first["route"], "/jobs");
    assert_eq!(first["pane_route"], "/pkg/com.ikenga.agentops-fixture/jobs");
    assert_eq!(first["pin_on_install"], true);
    assert_eq!(entries[1]["pin_on_install"], false);
}

#[test]
fn explorer_sections_snapshot_wire_shape() {
    let reg = ExplorerSectionsRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    let first = &snap["entries"][0];
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
    assert_eq!(first["decl_index"], 0);
    assert_eq!(first["order"], 3);
    assert_eq!(
        first["data_route"],
        "/pkg/com.ikenga.agentops-fixture/sections/queue"
    );
}

#[test]
fn companion_panels_snapshot_wire_shape() {
    let reg = CompanionPanelsRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    let first = &snap["entries"][0];
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
    assert_eq!(first["session_scoped"], true);
}

/// Tagged unions serialize with the `kind` discriminator the TS side
/// switches on (`"ngwa-item"` / `"dispatch"` / `"view"` / `"file"`).
#[test]
fn context_actions_snapshot_wire_shape_and_tagged_unions() {
    let reg = ContextActionsRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
    let first = &entries[0];
    assert_eq!(
        sorted_keys(first),
        vec!["id", "label", "pkg_id", "qualified_id", "run", "when"]
    );
    assert_eq!(
        first["when"],
        serde_json::json!({"kind": "ngwa-item", "kinds": ["automation-run"]})
    );
    assert_eq!(
        first["run"],
        serde_json::json!({
            "kind": "dispatch",
            "prompt": "Retry {{ngwa.item}}",
            "target": "agentops"
        })
    );
    assert_eq!(entries[1]["when"]["kind"], "file");
    assert_eq!(entries[1]["when"]["glob"], "**/*.log");
    assert_eq!(
        entries[1]["run"],
        serde_json::json!({"kind": "view", "route": "/jobs"})
    );
}

#[test]
fn widgets_snapshot_wire_shape_and_span_values() {
    let reg = WidgetsRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    let entries = snap["entries"].as_array().unwrap();
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
    assert_eq!(first["span"], "wide");
    // The `span` default — declared without it — serializes as "medium".
    assert_eq!(entries[1]["span"], "medium");
}

/// The rail-claim projection (`activity_bar`) is views-sourced post-WP-28:
/// `route` is views[0]'s *pane* route and `nav` carries the full views list
/// mapped onto the NavEntry wire shape the pkg-mode sidebar reads.
#[test]
fn activity_bar_snapshot_is_views_sourced() {
    let reg = ActivityBarRegistry::new();
    reg.register(&pkg_from_json(FULL_V5)).unwrap();
    let snap = reg.snapshot();

    let first = &snap["entries"][0];
    let mut keys = sorted_keys(first);
    keys.retain(|k| k != "badge" && k != "parked" && k != "parked_reason");
    assert_eq!(
        keys,
        vec!["icon", "id", "label", "nav", "pkg_id", "pkg_name", "route", "section"]
    );
    assert_eq!(first["route"], "/pkg/com.ikenga.agentops-fixture/jobs");
    let nav = first["nav"].as_array().unwrap();
    assert_eq!(nav.len(), 2);
    assert_eq!(
        sorted_keys(&nav[0]),
        vec!["icon", "id", "label", "route", "section"]
    );
    assert_eq!(nav[0]["label"], "Jobs");
    assert_eq!(nav[0]["route"], "/pkg/com.ikenga.agentops-fixture/jobs");
}

/// §4 alias, end to end: a legacy nav-only fixture aliases to `views` at
/// parse, and the `views` registry hands the FE normalized namespace routes +
/// computed pane routes + `pin_on_install` on the rail claim.
#[test]
fn legacy_nav_fixture_aliases_into_views_registry() {
    let mut manifest: Manifest = serde_json::from_str(LEGACY_NAV).unwrap();
    assert_eq!(
        manifest.apply_nav_views_alias(),
        Some(NavAliasOutcome::Applied)
    );

    let pkg = Package {
        manifest,
        install_path: PathBuf::from("/tmp/_fixture"),
    };
    let reg = ViewsRegistry::new();
    reg.register(&pkg).unwrap();

    let entries = reg.snapshot()["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 2);
    // `/pkg/com.ikenga.git-fixture/` normalized to the declared `/` route.
    assert_eq!(entries[0]["route"], "/");
    assert_eq!(entries[0]["pane_route"], "/pkg/com.ikenga.git-fixture/");
    assert_eq!(entries[0]["title"], "Changes");
    assert_eq!(entries[0]["pin_on_install"], true);
    assert_eq!(entries[1]["route"], "/history");
    assert_eq!(entries[1]["pin_on_install"], false);

    // The whole registry surface accepts the aliased pkg.
    for reg in [
        Box::new(ActivityBarRegistry::new()) as Box<dyn Registry>,
        Box::new(ExplorerSectionsRegistry::new()),
        Box::new(CompanionPanelsRegistry::new()),
        Box::new(ContextActionsRegistry::new()),
        Box::new(WidgetsRegistry::new()),
    ] {
        reg.register(&pkg)
            .unwrap_or_else(|e| panic!("{} rejected aliased pkg: {e}", reg.name()));
    }
}
