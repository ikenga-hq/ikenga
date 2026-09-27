//! Window registry — owns the lifecycle of spawned windows (plans/multi-window
//! WP-03). Consumes the WP-02 `G-WINDOW-MODEL` contract.
//!
//! Each non-primary window is created with a [`WindowDescriptor`]-derived label
//! and tracked here. The registry emits the canonical `window://` lifecycle
//! events (via the contract envelope) and exposes a window-targeted emit helper
//! (`emit_to_window`) — the race-free path for `WINDOW_TARGETED_CHANNELS`.
//!
//! WP-69 (G-SEATS §4.4, DEC-69d, pin P-7) adds two things a *Pop out* that
//! joins "Window 2" needs:
//! - **last-focus tracking** — every spawned window's focus gains are stamped
//!   with a monotonic sequence, so "Window 2" (the most recently focused live
//!   non-`main` window, excluding `Workspace` windows bound to another
//!   project) is a pure pick over the live list ([`pick_window_two`]);
//! - **add / remove surface** on a live window. A detached window's
//!   `surface_set` was fixed at spawn; now it can grow (a join) and shrink (a
//!   move back). Each change is emitted to that window's label and to `main`
//!   as `window://surfaces-changed` ([`topics::SURFACES_CHANGED`]), so the
//!   thin window re-renders its tabs and the primary's detached-surface
//!   tracker follows without a round trip. A surface lives in at most one
//!   detached window: joining it to one takes it out of any other, and a
//!   window left with no surfaces is closed.

use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use super::descriptor::{WindowDescriptor, WindowKind};
use super::events::{topics, WindowEventEnvelope, WindowEventTarget};

/// Process-global registry of spawned (non-`main`) windows. Managed in Tauri
/// state; the primary `main` window is owned by `lib.rs` setup and is not held
/// here.
#[derive(Default)]
pub struct WindowRegistry {
    inner: RwLock<HashMap<String, WindowDescriptor>>,
    /// WP-69: last-focus order of spawned windows (P-7). In memory only.
    focus: RwLock<FocusOrder>,
}

/// Monotonic focus stamps: a higher stamp was focused more recently. A window
/// is stamped when it is spawned (a new window opens focused) and on every
/// `Focused(true)`; its stamp goes when the window does.
#[derive(Default)]
struct FocusOrder {
    seq: u64,
    by_label: HashMap<String, u64>,
}

/// Payload of the host-only `window://surfaces-changed` event (WP-69). The
/// full `surface_set` after the change travels with it, so a listener never
/// has to re-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SurfacesChanged {
    pub label: String,
    pub surface_set: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// True when the removal is a *Move back to main window*: the primary
    /// mounts the surface again (in a pane when none holds it).
    pub move_back: bool,
}

/// "Window 2" (G-SEATS §4.4, pin P-7): the most recently focused live
/// non-`main` window that isn't a `Workspace` window bound to another project.
/// `None` when no such window exists, and the caller spawns one.
///
/// `active_project` is the primary's active project. A `Workspace` window
/// bound to a project is excluded unless that project is the active one; with
/// no active project known, every project-bound `Workspace` window is
/// excluded. A window never stamped ranks below any stamped one; ties break on
/// the label so the pick is deterministic.
pub fn pick_window_two(
    windows: &[WindowDescriptor],
    focus: &HashMap<String, u64>,
    active_project: Option<&str>,
) -> Option<String> {
    windows
        .iter()
        .filter(|d| d.label != "main")
        .filter(|d| !bound_to_other_project(d, active_project))
        .max_by(|a, b| {
            let fa = focus.get(&a.label).copied().unwrap_or(0);
            let fb = focus.get(&b.label).copied().unwrap_or(0);
            fa.cmp(&fb).then_with(|| b.label.cmp(&a.label))
        })
        .map(|d| d.label.clone())
}

fn bound_to_other_project(d: &WindowDescriptor, active_project: Option<&str>) -> bool {
    if !matches!(d.kind, WindowKind::Workspace) {
        return false;
    }
    match (d.project_id.as_deref(), active_project) {
        (None, _) => false,
        (Some(bound), Some(active)) => bound != active,
        (Some(_), None) => true,
    }
}

fn kind_str(kind: &WindowKind) -> &'static str {
    match kind {
        WindowKind::Primary => "primary",
        WindowKind::SingleSurface => "single-surface",
        WindowKind::PaneSet => "pane-set",
        WindowKind::Workspace => "workspace",
    }
}

impl WindowRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a labeled window from a descriptor. The window loads the same app
    /// URL as `main` with `?window=<label>&surfaces=…` appended; WP-05's thin
    /// entry reads those params to mount only the declared surface_set.
    pub fn spawn(&self, app: &AppHandle, desc: WindowDescriptor) -> Result<String> {
        if desc.label == "main" {
            return Err(anyhow!("'main' is the primary window and cannot be spawned"));
        }
        if self.inner.read().unwrap().contains_key(&desc.label)
            || app.get_webview_window(&desc.label).is_some()
        {
            return Err(anyhow!("window '{}' already exists", desc.label));
        }

        // Derive the URL from the primary window so dev (localhost:1420) and
        // prod (viewer_port) both work without re-plumbing the port here.
        let main = app
            .get_webview_window("main")
            .ok_or_else(|| anyhow!("no primary window yet"))?;
        let mut url = main.url().map_err(|e| anyhow!("read main url: {e}"))?;
        {
            let mut qp = url.query_pairs_mut();
            qp.clear();
            qp.append_pair("window", &desc.label);
            qp.append_pair("kind", kind_str(&desc.kind));
            // One repeated `surfaces` param per entry — do NOT comma-join: a
            // surface id can legally contain a comma (e.g. `viewer:/a/b,c.md`),
            // which a comma-split on the FE would fracture. The FE reads them
            // with `params.getAll('surfaces')`.
            for s in &desc.surface_set {
                qp.append_pair("surfaces", s);
            }
            if let Some(p) = &desc.project_id {
                qp.append_pair("project", p);
            }
        }

        let window =
            WebviewWindowBuilder::new(app, &desc.label, WebviewUrl::External(url))
                .title("Ikenga")
                .inner_size(960.0, 700.0)
                .min_inner_size(480.0, 360.0)
                .resizable(true)
                .disable_drag_drop_handler()
                .build()
                .map_err(|e| anyhow!("build window '{}': {e}", desc.label))?;

        self.inner
            .write()
            .unwrap()
            .insert(desc.label.clone(), desc.clone());
        // A new window opens focused; stamp it now so a Pop out issued before
        // its first `Focused(true)` lands still finds it (P-7).
        self.note_focus(&desc.label);

        // Cleanup + closed event when the OS window is destroyed (user close),
        // plus focus-changed on every focus transition (part of the frozen
        // window contract the FE cross-window bus subscribes to).
        let app_for_close = app.clone();
        let label_for_close = desc.label.clone();
        window.on_window_event(move |ev| match ev {
            WindowEvent::Destroyed => {
                if let Some(reg) = app_for_close.try_state::<WindowRegistry>() {
                    reg.inner.write().unwrap().remove(&label_for_close);
                    reg.forget_focus(&label_for_close);
                }
                // A pkg pane parented to this window would otherwise leak in the
                // panes map (macOS/Windows) or as a top-level surface + listener
                // entry (Linux) until pkg uninstall.
                cleanup_panes_for_parent(&app_for_close, &label_for_close);
                let env = WindowEventEnvelope::new(
                    topics::CLOSED,
                    "core",
                    WindowEventTarget::Broadcast,
                    serde_json::json!({ "label": label_for_close }),
                );
                let _ = app_for_close.emit(topics::CLOSED, env);
            }
            WindowEvent::Focused(focused) => {
                if *focused {
                    if let Some(reg) = app_for_close.try_state::<WindowRegistry>() {
                        reg.note_focus(&label_for_close);
                    }
                }
                emit_focus_changed(&app_for_close, &label_for_close, *focused);
            }
            _ => {}
        });

        let opened = WindowEventEnvelope::new(
            topics::OPENED,
            "core",
            WindowEventTarget::Broadcast,
            serde_json::json!({ "label": desc.label, "kind": kind_str(&desc.kind) }),
        );
        let _ = app.emit(topics::OPENED, opened);

        Ok(desc.label)
    }

    /// Close a spawned window by label. `main` is refused.
    pub fn close(&self, app: &AppHandle, label: &str) -> Result<()> {
        if label == "main" {
            return Err(anyhow!("cannot close the primary window"));
        }
        if let Some(w) = app.get_webview_window(label) {
            w.close().map_err(|e| anyhow!("close '{label}': {e}"))?;
        }
        // The Destroyed handler also removes it, but remove here too so a
        // close() immediately reflects in list() even before the event fires.
        self.inner.write().unwrap().remove(label);
        self.forget_focus(label);
        Ok(())
    }

    /// Stamp `label` as the most recently focused spawned window (P-7).
    /// `main` is never stamped: it is never "Window 2".
    pub fn note_focus(&self, label: &str) {
        if label == "main" {
            return;
        }
        let mut f = self.focus.write().unwrap();
        f.seq += 1;
        let seq = f.seq;
        f.by_label.insert(label.to_string(), seq);
    }

    fn forget_focus(&self, label: &str) {
        self.focus.write().unwrap().by_label.remove(label);
    }

    /// "Window 2" over the live list (P-7). See [`pick_window_two`].
    pub fn window_two(&self, app: &AppHandle, active_project: Option<&str>) -> Option<String> {
        let live = self.list_live(app);
        let focus = self.focus.read().unwrap().by_label.clone();
        pick_window_two(&live, &focus, active_project)
    }

    /// *Pop out* joins Window 2 (DEC-69d): put `surface_id` into Window 2 and
    /// return its label, or `Ok(None)` when there is no Window 2 — the caller
    /// then spawns one. The pick and the add happen in one call, so a Window 2
    /// that closes in between can't be picked and then missed.
    pub fn join_surface(
        &self,
        app: &AppHandle,
        surface_id: &str,
        active_project: Option<&str>,
    ) -> Result<Option<String>> {
        if surface_id.trim().is_empty() {
            return Err(anyhow!("surface id is empty"));
        }
        let Some(label) = self.window_two(app, active_project) else {
            return Ok(None);
        };
        self.add_surface(app, &label, surface_id)?;
        Ok(Some(label))
    }

    /// Add `surface_id` to the live window `label` as a new tab. A surface
    /// lives in at most one detached window, so it leaves any other one first
    /// (which closes when that empties it). Idempotent for a surface the
    /// window already holds. Returns the window's `surface_set` after the add.
    pub fn add_surface(&self, app: &AppHandle, label: &str, surface_id: &str) -> Result<Vec<String>> {
        if label == "main" {
            return Err(anyhow!("'main' is not a detached window"));
        }
        if surface_id.trim().is_empty() {
            return Err(anyhow!("surface id is empty"));
        }
        if app.get_webview_window(label).is_none() {
            return Err(anyhow!("window '{label}' is not open"));
        }
        let mut shrunk: Vec<(String, Vec<String>)> = Vec::new();
        let mut emptied: Vec<String> = Vec::new();
        let (set, added) = {
            let mut g = self.inner.write().unwrap();
            if !g.contains_key(label) {
                return Err(anyhow!("window '{label}' is not open"));
            }
            for (other, d) in g.iter_mut() {
                if other != label && d.surface_set.iter().any(|s| s == surface_id) {
                    d.surface_set.retain(|s| s != surface_id);
                    if d.surface_set.is_empty() {
                        emptied.push(other.clone());
                    } else {
                        shrunk.push((other.clone(), d.surface_set.clone()));
                    }
                }
            }
            let d = g
                .get_mut(label)
                .ok_or_else(|| anyhow!("window '{label}' is not open"))?;
            let added = !d.surface_set.iter().any(|s| s == surface_id);
            if added {
                d.surface_set.push(surface_id.to_string());
            }
            (d.surface_set.clone(), added)
        };
        for (other, other_set) in shrunk {
            emit_surfaces_changed(
                app,
                SurfacesChanged {
                    label: other,
                    surface_set: other_set,
                    added: Vec::new(),
                    removed: vec![surface_id.to_string()],
                    move_back: false,
                },
            );
        }
        for other in emptied {
            if let Err(e) = self.close(app, &other) {
                tracing::warn!("[window] close emptied window `{other}`: {e}");
            }
        }
        emit_surfaces_changed(
            app,
            SurfacesChanged {
                label: label.to_string(),
                surface_set: set.clone(),
                added: if added { vec![surface_id.to_string()] } else { Vec::new() },
                removed: Vec::new(),
                move_back: false,
            },
        );
        Ok(set)
    }

    /// Take `surface_id` out of the detached window `label`. The window closes
    /// when that was its last surface. `move_back` marks a *Move back to main
    /// window*. Removing a surface the window doesn't hold is a no-op that
    /// still reports the current set. Returns the `surface_set` after.
    pub fn remove_surface(
        &self,
        app: &AppHandle,
        label: &str,
        surface_id: &str,
        move_back: bool,
    ) -> Result<Vec<String>> {
        if label == "main" {
            return Err(anyhow!("'main' is not a detached window"));
        }
        let (set, removed) = {
            let mut g = self.inner.write().unwrap();
            let d = g
                .get_mut(label)
                .ok_or_else(|| anyhow!("window '{label}' is not open"))?;
            let before = d.surface_set.len();
            d.surface_set.retain(|s| s != surface_id);
            (d.surface_set.clone(), d.surface_set.len() != before)
        };
        if !removed {
            return Ok(set);
        }
        // Emitted before any close, so the primary learns it was a move back
        // (and mounts the surface) before `window://closed` arrives.
        emit_surfaces_changed(
            app,
            SurfacesChanged {
                label: label.to_string(),
                surface_set: set.clone(),
                added: Vec::new(),
                removed: vec![surface_id.to_string()],
                move_back,
            },
        );
        if set.is_empty() {
            self.close(app, label)?;
        }
        Ok(set)
    }

    /// Descriptors of all currently-spawned windows (raw in-memory view; may
    /// contain a ghost if a `Destroyed` event was missed). Prefer `list_live`
    /// wherever an `AppHandle` is available.
    pub fn list(&self) -> Vec<WindowDescriptor> {
        self.inner.read().unwrap().values().cloned().collect()
    }

    /// Like `list`, but reconciles against the OS: any tracked label that no
    /// longer resolves via `get_webview_window` is dropped (and its panes /
    /// listeners cleaned up), so a missed `Destroyed` event can't leave a
    /// permanent ghost in the registry.
    pub fn list_live(&self, app: &AppHandle) -> Vec<WindowDescriptor> {
        let dead: Vec<String> = {
            let g = self.inner.read().unwrap();
            g.keys()
                .filter(|label| app.get_webview_window(label).is_none())
                .cloned()
                .collect()
        };
        if !dead.is_empty() {
            {
                let mut g = self.inner.write().unwrap();
                for label in &dead {
                    g.remove(label);
                }
            }
            for label in &dead {
                self.forget_focus(label);
            }
            for label in &dead {
                tracing::warn!(
                    "[window] reconciled ghost window `{label}` (Destroyed event missed)"
                );
                cleanup_panes_for_parent(app, label);
                let env = WindowEventEnvelope::new(
                    topics::CLOSED,
                    "core",
                    WindowEventTarget::Broadcast,
                    serde_json::json!({ "label": label }),
                );
                let _ = app.emit(topics::CLOSED, env);
            }
        }
        self.inner.read().unwrap().values().cloned().collect()
    }

    /// Window-targeted emit — the race-free path for `WINDOW_TARGETED_CHANNELS`
    /// (vs broadcast `app.emit`). WP-04 migrates the racy existing channels here.
    #[allow(dead_code)]
    pub fn emit_to_window<T: Serialize + Clone>(
        &self,
        app: &AppHandle,
        label: &str,
        topic: &str,
        payload: T,
    ) -> Result<()> {
        app.emit_to(label, topic, payload)
            .map_err(|e| anyhow!("emit_to '{label}': {e}"))
    }
}

/// Emit `topic` to the currently-focused window only, falling back to a
/// broadcast if no window reports focus. Used for global-shortcut-driven
/// events (`screenshot://shortcut`) that must reach the window the user is
/// looking at, not race across every window (research 03 — the broadcast
/// shortcut made every window's screenshot listener respond simultaneously).
/// Emit to a specific window label, falling back to a broadcast if no window
/// with that label exists. Use this when a topic has exactly ONE consumer
/// window (e.g. `screenshot://shortcut`, whose FE listener lives only in the
/// primary window) — `emit_to_focused` would mis-route to a focused pkg-pane /
/// detached window that has no listener.
pub fn emit_to_label<T: Serialize + Clone>(
    app: &AppHandle,
    label: &str,
    topic: &str,
    payload: T,
) -> Result<()> {
    if app.get_webview_window(label).is_some() {
        app.emit_to(label, topic, payload)
            .map_err(|e| anyhow!("emit_to '{label}': {e}"))
    } else {
        app.emit(topic, payload)
            .map_err(|e| anyhow!("broadcast '{topic}': {e}"))
    }
}

// Superseded for the screenshot shortcut by `focused_listener_window_label` +
// `emit_to_label` (this walks ALL `webview_windows`, which mis-routes to focused
// pkg-pane child webviews on Linux). Kept as the generic WP-04 primitive.
#[allow(dead_code)]
pub fn emit_to_focused<T: Serialize + Clone>(
    app: &AppHandle,
    topic: &str,
    payload: T,
) -> Result<()> {
    let focused = app.webview_windows().into_iter().find_map(|(label, w)| {
        if w.is_focused().unwrap_or(false) {
            Some(label)
        } else {
            None
        }
    });
    match focused {
        Some(label) => app
            .emit_to(&label, topic, payload)
            .map_err(|e| anyhow!("emit_to '{label}': {e}")),
        None => app
            .emit(topic, payload)
            .map_err(|e| anyhow!("broadcast '{topic}': {e}")),
    }
}

/// Emit the canonical `window://focus-changed` broadcast for `label`. Called
/// from every window's `Focused` hook (main in `lib.rs` setup, detached in
/// `spawn`). Broadcast (not window-targeted) — focus-changed isn't in
/// `WINDOW_TARGETED_CHANNELS`; each window filters its own subscription.
pub fn emit_focus_changed(app: &AppHandle, label: &str, focused: bool) {
    let env = WindowEventEnvelope::new(
        topics::FOCUS_CHANGED,
        "core",
        WindowEventTarget::Broadcast,
        serde_json::json!({ "label": label, "focused": focused }),
    );
    let _ = app.emit(topics::FOCUS_CHANGED, env);
}

/// Emit `window://surfaces-changed` (WP-69) to the window whose set changed
/// and to `main` — window-targeted, not broadcast: those are its only two
/// listeners (the thin window's tab host, the primary's detached-surface
/// tracker). Best-effort, like every lifecycle emit here.
fn emit_surfaces_changed(app: &AppHandle, change: SurfacesChanged) {
    let mut targets = vec![change.label.clone()];
    if change.label != "main" {
        targets.push("main".to_string());
    }
    for target in targets {
        let env = WindowEventEnvelope::new(
            topics::SURFACES_CHANGED,
            "core",
            WindowEventTarget::Window { label: target.clone() },
            change.clone(),
        );
        let _ = app.emit_to(target.as_str(), topics::SURFACES_CHANGED, env);
    }
}

/// Label of the currently-focused window that actually hosts the screenshot
/// listener (`useScreenshotListener`, mounted only inside `<Workspace/>`).
///
/// Only `Workspace`-kind spawned windows qualify: `single-surface` / `pane-set`
/// detached windows render the thin `DetachedRoot` (no listener), and pkg-pane
/// child webviews (Linux `TopLevel` surfaces) also appear in
/// `app.webview_windows()` but carry no shell listeners — routing a shortcut to
/// either silently no-ops (the `emit_to_focused` regression pinned to "main" in
/// 86a766a). Returns `None` when no such window is focused — including whenever
/// `main` itself is focused, or when WebKitGTK's `is_focused` under-reports — so
/// the caller falls back to "main", preserving today's behavior exactly.
pub fn focused_listener_window_label(app: &AppHandle) -> Option<String> {
    let reg = app.try_state::<WindowRegistry>()?;
    reg.list()
        .into_iter()
        .filter(|d| matches!(d.kind, WindowKind::Workspace))
        .map(|d| d.label)
        .find(|label| {
            app.get_webview_window(label)
                .and_then(|w| w.is_focused().ok())
                .unwrap_or(false)
        })
}

/// Tear down any pkg-webview panes parented to a now-destroyed window. Bridges
/// the window registry to `WebviewPanesRegistry` without either owning the
/// other; a no-op if the panes state isn't managed yet (early boot).
fn cleanup_panes_for_parent(app: &AppHandle, parent_label: &str) {
    if let Some(panes) = app.try_state::<crate::commands::pkg_webview::WebviewPanesState>() {
        panes.0.cleanup_for_parent(parent_label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_empty_on_new() {
        let reg = WindowRegistry::new();
        assert!(reg.list().is_empty());
    }

    fn desc(label: &str, kind: WindowKind, project: Option<&str>) -> WindowDescriptor {
        WindowDescriptor {
            label: label.to_string(),
            kind,
            surface_set: vec![format!("terminal:{label}")],
            project_id: project.map(str::to_string),
            layout_key: label.to_string(),
        }
    }

    fn stamps(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(l, s)| (l.to_string(), *s)).collect()
    }

    #[test]
    fn window_two_is_none_without_a_secondary_window() {
        let windows = vec![desc("main", WindowKind::Primary, None)];
        assert_eq!(pick_window_two(&windows, &HashMap::new(), Some("p")), None);
        assert_eq!(pick_window_two(&[], &HashMap::new(), None), None);
    }

    #[test]
    fn window_two_is_the_most_recently_focused_secondary_window() {
        let windows = vec![
            desc("detached-a", WindowKind::SingleSurface, None),
            desc("detached-b", WindowKind::SingleSurface, None),
            desc("main", WindowKind::Primary, None),
        ];
        let focus = stamps(&[("detached-a", 7), ("detached-b", 3), ("main", 99)]);
        assert_eq!(
            pick_window_two(&windows, &focus, Some("p")).as_deref(),
            Some("detached-a")
        );
        let focus = stamps(&[("detached-a", 7), ("detached-b", 8)]);
        assert_eq!(
            pick_window_two(&windows, &focus, Some("p")).as_deref(),
            Some("detached-b")
        );
    }

    #[test]
    fn window_two_skips_workspace_windows_bound_to_another_project() {
        let windows = vec![
            desc("detached-ws-other", WindowKind::Workspace, Some("other")),
            desc("detached-thin", WindowKind::SingleSurface, None),
        ];
        let focus = stamps(&[("detached-ws-other", 9), ("detached-thin", 1)]);
        assert_eq!(
            pick_window_two(&windows, &focus, Some("mine")).as_deref(),
            Some("detached-thin")
        );
        // The same Workspace window bound to the ACTIVE project qualifies.
        assert_eq!(
            pick_window_two(&windows, &focus, Some("other")).as_deref(),
            Some("detached-ws-other")
        );
        // An unbound Workspace window follows the primary, so it qualifies.
        let unbound = vec![desc("detached-ws", WindowKind::Workspace, None)];
        assert_eq!(
            pick_window_two(&unbound, &HashMap::new(), Some("mine")).as_deref(),
            Some("detached-ws")
        );
        // With no active project known, a project-bound one is excluded.
        let only_bound = vec![desc("detached-ws-other", WindowKind::Workspace, Some("other"))];
        assert_eq!(pick_window_two(&only_bound, &HashMap::new(), None), None);
    }

    #[test]
    fn window_two_ranks_unstamped_below_stamped_and_breaks_ties_on_label() {
        let windows = vec![
            desc("detached-b", WindowKind::SingleSurface, None),
            desc("detached-a", WindowKind::SingleSurface, None),
            desc("detached-c", WindowKind::SingleSurface, None),
        ];
        assert_eq!(
            pick_window_two(&windows, &stamps(&[("detached-c", 1)]), None).as_deref(),
            Some("detached-c")
        );
        assert_eq!(
            pick_window_two(&windows, &HashMap::new(), None).as_deref(),
            Some("detached-a")
        );
    }

    #[test]
    fn note_focus_orders_labels_and_never_stamps_main() {
        let reg = WindowRegistry::new();
        reg.note_focus("detached-a");
        reg.note_focus("detached-b");
        reg.note_focus("main");
        reg.note_focus("detached-a");
        {
            let f = reg.focus.read().unwrap();
            assert!(f.by_label["detached-a"] > f.by_label["detached-b"]);
            assert!(!f.by_label.contains_key("main"));
        }
        reg.forget_focus("detached-a");
        assert!(!reg.focus.read().unwrap().by_label.contains_key("detached-a"));
    }

    #[test]
    fn surfaces_changed_serializes_snake_case() {
        let v = serde_json::to_value(SurfacesChanged {
            label: "detached-1".into(),
            surface_set: vec!["terminal:p1".into(), "terminal:p2".into()],
            added: vec!["terminal:p2".into()],
            removed: vec![],
            move_back: false,
        })
        .unwrap();
        assert_eq!(v["surface_set"][1], "terminal:p2");
        assert_eq!(v["move_back"], false);
    }

    #[test]
    fn kind_str_matches_contract_kebab_case() {
        assert_eq!(kind_str(&WindowKind::SingleSurface), "single-surface");
        assert_eq!(kind_str(&WindowKind::Workspace), "workspace");
    }
}
