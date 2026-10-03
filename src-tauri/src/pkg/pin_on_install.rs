//! Apply a manifest's `pin_on_install` views to the activity rail, on the
//! kernel side, at the moment a fresh install succeeds.
//!
//! This used to run in the front end, in a React hook that listened for the
//! kernel's `pkg-installed` event. It never pinned anything for a pkg with a
//! reverse-DNS id (`com.ikenga.meetings`, which is every published pkg): the
//! hook passed the pkg id as the pin's `manifest_id`, and `pins_add` only
//! accepts artifact ids there (`/^[a-z0-9-]+$/`), so every call was rejected
//! and the rejection was swallowed into a console warning. It also depended
//! on that hook being mounted in some window when the event fired.
//!
//! Doing it here removes both failure modes: the kernel writes the pins in
//! the same call that writes the `pkg_installed` row, whichever UI started
//! the install, and before it emits `pkg-installed` so the front end's
//! refresh on that event already sees them.
//!
//! The pins carry no `manifest_id`. That column is the lookup key for
//! `ikenga://artifact/<id>` and must stay unique to artifacts; a pkg view pin
//! is identified by its target, `/pkg/<pkg id><route>`, which is already
//! namespaced by the pkg id. Dedupe is on that target, so a pin the user (or
//! the first-boot rail seed) already made at the same place is left alone.

use crate::db::PaDb;
use crate::pkg::manifest::Package;
use crate::pkg::registries::views::pane_route_for;
use crate::server::shared::activity_bar;

/// Kinds of pin that open a pkg route. Either one at the same target counts
/// as "already pinned".
const ROUTE_PIN_KINDS: [&str; 2] = ["route", "pkg-route"];

/// Pin every `ui.views[]` entry that declares `pin_on_install`, unless a
/// route pin already points at the same pane route. Returns the targets that
/// were pinned. Best-effort: a failure is logged and skipped, never returned,
/// because a missing pin must not fail an install that otherwise worked.
pub async fn apply_pin_on_install(db: &PaDb, pkg: &Package) -> Vec<String> {
    let pkg_id = &pkg.manifest.id;
    let Some(ui) = pkg.manifest.ui.as_ref() else {
        return Vec::new();
    };
    let wanted: Vec<_> = ui.views.iter().filter(|v| v.pin_on_install).collect();
    if wanted.is_empty() {
        return Vec::new();
    }

    let existing = match activity_bar::pins_list(db).await {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[pkg_kernel] pin_on_install for `{pkg_id}`: reading pins failed: {e}");
            return Vec::new();
        }
    };

    let mut pinned = Vec::new();
    for view in wanted {
        let target = pane_route_for(pkg_id, &view.route);
        let already = existing
            .iter()
            .any(|p| ROUTE_PIN_KINDS.contains(&p.kind.as_str()) && p.target == target)
            || pinned.contains(&target);
        if already {
            continue;
        }
        match activity_bar::pins_add(
            db,
            "route".to_string(),
            target.clone(),
            view.title.clone(),
            view.icon.clone(),
            None,
            None,
            None,
        )
        .await
        {
            Ok(_) => pinned.push(target),
            Err(e) => log::warn!(
                "[pkg_kernel] pin_on_install for `{pkg_id}` view `{}` failed: {e}",
                view.id
            ),
        }
    }
    if !pinned.is_empty() {
        log::info!("[pkg_kernel] pinned {} view(s) of `{pkg_id}` to the rail", pinned.len());
    }
    pinned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn fresh_db() -> (PaDb, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let db = PaDb::new(tmp.path().join("pa.db"));
        db.ensure_pool().await.expect("ensure_pool");
        (db, tmp)
    }

    /// The shape of the published Meetings pkg: a reverse-DNS id and one
    /// view that asks to be pinned.
    fn meetings() -> Package {
        let manifest = serde_json::from_value(serde_json::json!({
            "id": "com.ikenga.meetings",
            "name": "Meetings",
            "version": "0.2.1",
            "ikenga_api": "3",
            "ui": {
                "routes": [
                    { "path": "/meetings", "kind": "iframe", "source": "ui/index.html" },
                    { "path": "/notes", "kind": "iframe", "source": "ui/notes.html" }
                ],
                "views": [
                    { "id": "meetings.index", "title": "Meetings", "icon": "video",
                      "route": "/meetings", "pin_on_install": true },
                    { "id": "meetings.notes", "title": "Notes", "route": "/notes" }
                ]
            }
        }))
        .expect("manifest");
        Package { manifest, install_path: PathBuf::from("/tmp/meetings") }
    }

    /// Why the old front-end path never pinned: the pin store refuses a
    /// dotted pkg id as a `manifest_id`. Kept as a test so nobody goes back
    /// to passing one.
    #[tokio::test]
    async fn pin_store_rejects_a_pkg_id_as_manifest_id() {
        let (db, _tmp) = fresh_db().await;
        let err = activity_bar::pins_add(
            &db,
            "route".into(),
            "/pkg/com.ikenga.meetings/meetings".into(),
            "Meetings".into(),
            Some("video".into()),
            None,
            None,
            Some("com.ikenga.meetings".into()),
        )
        .await
        .expect_err("a dotted id is not a valid manifest_id");
        assert!(err.contains("manifest_id"), "{err}");
    }

    #[tokio::test]
    async fn pins_only_views_that_ask_for_it() {
        let (db, _tmp) = fresh_db().await;
        let pinned = apply_pin_on_install(&db, &meetings()).await;
        assert_eq!(pinned, vec!["/pkg/com.ikenga.meetings/meetings".to_string()]);

        let pins = activity_bar::pins_list(&db).await.expect("list");
        assert_eq!(pins.len(), 1);
        let pin = &pins[0];
        assert_eq!(pin.kind, "route");
        assert_eq!(pin.target, "/pkg/com.ikenga.meetings/meetings");
        assert_eq!(pin.label, "Meetings");
        assert_eq!(pin.icon_lucide.as_deref(), Some("video"));
        assert_eq!(pin.manifest_id, None, "pkg pins must not claim an artifact id");
    }

    #[tokio::test]
    async fn an_existing_pin_at_the_same_target_is_left_alone() {
        let (db, _tmp) = fresh_db().await;
        activity_bar::pins_add(
            &db,
            "pkg-route".into(),
            "/pkg/com.ikenga.meetings/meetings".into(),
            "My meetings".into(),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("seed pin");

        assert!(apply_pin_on_install(&db, &meetings()).await.is_empty());
        let pins = activity_bar::pins_list(&db).await.expect("list");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].label, "My meetings");
    }

    #[tokio::test]
    async fn running_twice_never_double_pins() {
        let (db, _tmp) = fresh_db().await;
        apply_pin_on_install(&db, &meetings()).await;
        apply_pin_on_install(&db, &meetings()).await;
        assert_eq!(activity_bar::pins_list(&db).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn a_pkg_without_views_pins_nothing() {
        let (db, _tmp) = fresh_db().await;
        let mut pkg = meetings();
        pkg.manifest.ui = None;
        assert!(apply_pin_on_install(&db, &pkg).await.is_empty());
    }
}
