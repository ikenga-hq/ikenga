//! Per-pkg child webview kernel.
//!
//! Owns the `(pkg_id, pane_id) → PaneSurface` mapping and the per-jar
//! cookie partition resolution. Tauri commands in `commands/pkg_webview.rs`
//! are thin wrappers around the methods here.
//!
//! ## Per-OS architecture
//!
//! `pkg-browser` needs a webview that loads arbitrary external URLs (incl.
//! ones that deny iframe embedding via CSP `frame-ancestors`) **and** is
//! visually embedded in the shell pane. Tauri 2 offers two primitives for
//! this; we use whichever produces the better UX per OS:
//!
//! - **macOS / Windows**: `Window::add_child(WebviewBuilder, pos, size)` —
//!   a true in-window child webview. The compositor stacks it as a sibling
//!   of the main webview at the requested rect; visually identical to an
//!   iframe but unconstrained by CSP. Tauri PR #11616 fixed the rendering
//!   bugs on these two platforms in Nov 2024.
//! - **Linux WebKitGTK**: `add_child` is broken (Tauri issues #10420 /
//!   #13071 / #11170 — wry's GTK box layout silently ignores the position
//!   args and packs the child as a sibling widget). Wry docs explicitly
//!   mark `build_as_child` as "Linux X11 only" and the X11 path is broken
//!   too. Documented community status as of May 2026: no fork has merged
//!   a `GtkOverlay`/`GtkFixed` fix; no upstream PR is in flight. So we
//!   fall back to a borderless top-level `WebviewWindow` parented to main,
//!   manually tracking parent move/resize to keep the child overlaid on
//!   the placeholder rect. Functionally correct; visually a separate
//!   floating rectangle. Wayland additionally ignores `set_position` —
//!   document the `GDK_BACKEND=x11` workaround for Wayland users until
//!   wry adopts xdg-popup positioning.
//!
//! Both paths land in the same `PaneSurface` enum so the rest of the
//! kernel doesn't have to think about which one it's holding.
//!
//! ## Lifecycle
//!
//! - **register** (kernel `Registry`): caches the pkg's webview capability
//!   so `create` doesn't have to walk back to the manifest.
//! - **create** (FE-triggered): allocate a stable label, resolve the
//!   partition's data store, build the OS-specific surface, install a
//!   destroy listener so manual close is reflected in the panes map. On
//!   Linux additionally install a single parent-window event listener
//!   (idempotent across all panes) that keeps each child window aligned
//!   with the parent on move/resize.
//! - **destroy**: close the surface and drop the handle. Cookie partition
//!   data persists on disk; a future create with the same partition name
//!   picks up the same jar.
//! - **navigate / eval / set_rect**: dispatch to the active surface
//!   variant. `set_rect` on Linux recomputes screen coords from the parent
//!   inner_position; on macOS/Windows it sets pane-relative coords directly.
//!
//! ## Cookie partition resolution
//!
//! Per-pkg, per-partition isolation. The path / id is derived
//! deterministically from `(pkg_id, partition_name)` so a re-create after
//! restart picks up the same cookies, localStorage, etc.
//!
//! - **Linux + Windows**: `data_directory(PathBuf)`. Path:
//!   `app_data_dir/webjars/<pkg-slug>/<partition>/`.
//! - **macOS 14+ WKWebView**: `data_store_identifier([u8; 16])` derived
//!   from `sha256(pkg_id || '/' || partition_name)[..16]`.
//!
//! ## Capability check
//!
//! Create is rejected if the pkg's manifest doesn't declare
//! `capabilities.webview.child_webviews = true`. Partition names are
//! validated against `capabilities.webview.partitions`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use tauri::{
    AppHandle, LogicalSize, Manager, WebviewUrl, WebviewWindow,
    WindowEvent,
};

#[cfg(target_os = "linux")]
use tauri::{PhysicalPosition, WebviewWindowBuilder};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use tauri::{
    webview::{PageLoadEvent, WebviewBuilder},
    LogicalPosition, Webview,
};

use crate::pkg::manifest::Package;
use crate::pkg::registry::Registry;

/// Rect passed across the Tauri command boundary in main-window-client
/// coordinates (the placeholder div's `getBoundingClientRect()` rounded to
/// integers).
#[derive(Debug, Clone, Copy, serde::Deserialize, Serialize)]
pub struct PaneRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Snapshot row surfaced via `Registry::snapshot` into `pkg_kernel_status`.
/// `live_*` fields are queried from Tauri at snapshot time so they reflect
/// what the kernel-side surface actually thinks its rect is.
#[derive(Debug, Clone, Serialize)]
pub struct WebviewPaneStatus {
    pub pkg_id: String,
    pub pane_id: String,
    pub webview_label: String,
    pub current_url: Option<String>,
    pub partition: String,
    /// Last-requested rect (in main-window-client coords) from FE.
    pub stored_rect: PaneRect,
    /// Live position reported by the surface. macOS/Windows: client-area
    /// coords (relative to main window). Linux: screen coords.
    pub live_position: Option<[i32; 2]>,
    pub live_size: Option<[u32; 2]>,
    /// `"in-window"` (macOS/Windows native child) or `"top-level"` (Linux
    /// borderless overlay window). Surfaced so the FE / dev tools can
    /// distinguish without doing OS detection.
    pub surface_kind: &'static str,
    /// Phase 5: true when the pane is paused (snapshot/interaction tools
    /// return 409). Navigation + destroy still work.
    pub paused: bool,
}

/// Per-OS native handle. See module docstring for why the variants split.
enum PaneSurface {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    InWindow(Webview),
    #[cfg(target_os = "linux")]
    TopLevel(WebviewWindow),
}

impl PaneSurface {
    fn navigate(&self, url: url::Url) -> tauri::Result<()> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(w) => w.navigate(url),
            #[cfg(target_os = "linux")]
            Self::TopLevel(w) => w.navigate(url),
        }
    }

    fn eval(&self, js: &str) -> tauri::Result<()> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(w) => w.eval(js),
            #[cfg(target_os = "linux")]
            Self::TopLevel(w) => w.eval(js),
        }
    }

    fn close(&self) -> tauri::Result<()> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(w) => w.close(),
            #[cfg(target_os = "linux")]
            Self::TopLevel(w) => w.close(),
        }
    }

    fn position(&self) -> Option<[i32; 2]> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(w) => w.position().ok().map(|p| [p.x as i32, p.y as i32]),
            #[cfg(target_os = "linux")]
            Self::TopLevel(w) => w.outer_position().ok().map(|p| [p.x as i32, p.y as i32]),
        }
    }

    fn size(&self) -> Option<[u32; 2]> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(w) => w.size().ok().map(|s| [s.width, s.height]),
            #[cfg(target_os = "linux")]
            Self::TopLevel(w) => w.outer_size().ok().map(|s| [s.width, s.height]),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            Self::InWindow(_) => "in-window",
            #[cfg(target_os = "linux")]
            Self::TopLevel(_) => "top-level",
        }
    }
}

struct PaneHandle {
    surface: PaneSurface,
    label: String,
    /// Label of the window this pane is parented to / tracked against. "main"
    /// for the primary window; a detached label otherwise (multi-window:
    /// WP-04). Read on Linux by the parent-tracking listener + `set_surface_rect`
    /// to resolve the correct parent window's screen coords.
    parent_label: String,
    partition: String,
    current_url: RwLock<Option<String>>,
    /// The rect the FE measured. Held so `set_rect` and (on Linux) the
    /// parent-event listener have a fresh source of truth.
    stored_rect: RwLock<PaneRect>,
    /// Phase 5: when true, snapshot/interaction tools return 409 Conflict.
    /// Navigation and lifecycle (destroy, set_rect) still work — pause is
    /// about agent-driven actions, not the user's ability to recover.
    paused: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
pub struct WebviewPanesRegistry {
    panes: RwLock<HashMap<(String, String), Arc<PaneHandle>>>,
    pkg_capabilities: RwLock<HashMap<String, PkgCapability>>,
    /// Linux only: labels of parent windows that already have a move/resize
    /// listener installed. One listener per distinct parent window — a pane in
    /// a non-`main` window needs its own listener watching that window's
    /// geometry, otherwise it would never reposition (multi-window: WP-04).
    parent_listeners: RwLock<HashSet<String>>,
}

#[derive(Debug, Clone, Default)]
struct PkgCapability {
    child_webviews: bool,
    partitions: Vec<String>,
    /// `None` = field absent (permissive + warn), `Some(list)` = enforced.
    allowed_origins: Option<Vec<String>>,
}

/// Does `origin` satisfy the pkg's declared `allowed_origins`?
///
/// `None` (the manifest never declared the field) is permissive — a pkg
/// authored before v4, or from the pre-v4 scaffold template, must keep
/// mounting rather than failing with an error naming a field it was never
/// taught to write. Callers emit a `tracing::warn!` on that path.
///
/// A declared list is enforced, and an empty one denies everything (an
/// explicit lockdown is a legitimate thing to write). Entries match as:
///   - `*`                        — any origin
///   - `https://app.example.com`  — exact origin
///   - `https://*.example.com`    — any strict subdomain of example.com
fn origin_allowed(origin: &str, allowed: &Option<Vec<String>>) -> bool {
    let Some(list) = allowed else { return true };
    list.iter().any(|pat| origin_matches(origin, pat))
}

fn origin_matches(origin: &str, pat: &str) -> bool {
    if pat == "*" || pat == origin {
        return true;
    }
    // `<scheme>://*.<suffix>` — the only glob shape we accept. Anything else
    // is treated as a literal, so a typo fails closed instead of widening.
    let Some((pat_scheme, pat_rest)) = pat.split_once("://") else {
        return false;
    };
    let Some(suffix) = pat_rest.strip_prefix("*.") else {
        return false;
    };
    if suffix.is_empty() || suffix.contains('/') {
        return false;
    }
    let Some((origin_scheme, host)) = origin.split_once("://") else {
        return false;
    };
    if origin_scheme != pat_scheme {
        return false;
    }
    // Strict subdomain: `a.example.com` matches `*.example.com`;
    // bare `example.com` does not (list it explicitly if you want it).
    host.len() > suffix.len() + 1
        && host.ends_with(suffix)
        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
}

impl WebviewPanesRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a child webview for `(pkg_id, pane_id)`. Idempotent for the
    /// same key: if a webview already exists under that pair, it's destroyed
    /// first so the new one gets the requested URL / partition / rect.
    pub fn create(
        self: &Arc<Self>,
        app: &AppHandle,
        pkg_id: &str,
        pane_id: &str,
        url: &str,
        rect: PaneRect,
        partition: Option<&str>,
        parent_label: &str,
    ) -> Result<String> {
        // ── Common scaffolding ────────────────────────────────────────────
        let cap = self
            .pkg_capabilities
            .read()
            .map_err(|_| anyhow!("pkg_capabilities lock poisoned"))?
            .get(pkg_id)
            .cloned()
            .unwrap_or_default();

        if !cap.child_webviews {
            return Err(anyhow!(
                "pkg `{pkg_id}` did not declare `capabilities.webview.child_webviews = true`"
            ));
        }
        let partition = partition.unwrap_or("default");
        let wildcard = cap.partitions.iter().any(|p| p == "*");
        if partition != "default" && !wildcard && !cap.partitions.iter().any(|p| p == partition) {
            return Err(anyhow!(
                "pkg `{pkg_id}` did not declare partition `{partition}` (declared: {:?}). \
                 Use `partitions: [\"*\"]` in the manifest to allow any.",
                cap.partitions
            ));
        }

        // Idempotency hook for FE strict-mode double-mount.
        self.destroy(pkg_id, pane_id).ok();

        // Parent the pane to the window that asked for it — the calling FE
        // window for the Tauri command, "main" for the agent-driven iyke
        // bridge — NOT the literal "main", so panes can live in any window
        // (multi-window: WP-04).
        let parent_window = app.get_webview_window(parent_label).ok_or_else(|| {
            anyhow!(
                "no webview window labeled `{parent_label}` — pane parent missing \
                 (kernel called before setup completed, or window closed?)"
            )
        })?;

        let label = webview_label(pkg_id, pane_id);
        let parsed_url = url::Url::parse(url).with_context(|| format!("parse url `{url}`"))?;

        let origin = parsed_url.origin().unicode_serialization();
        if cap.allowed_origins.is_none() {
            tracing::warn!(
                pkg_id,
                origin = %origin,
                "pkg declares no `capabilities.webview.allowed_origins`; mounting \
                 without an origin boundary. Declare the field to bound navigation."
            );
        }
        if !origin_allowed(&origin, &cap.allowed_origins) {
            return Err(anyhow!(
                "origin `{origin}` is not in `capabilities.webview.allowed_origins` \
                 (declared: {:?})",
                cap.allowed_origins.as_deref().unwrap_or(&[])
            ));
        }

        let data_dir = partition_dir(app, pkg_id, partition)?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("create webview data dir {}", data_dir.display()))?;

        // ── Build the OS-specific surface ────────────────────────────────
        let surface = build_surface(
            app,
            &parent_window,
            &label,
            parsed_url,
            rect,
            partition,
            pkg_id,
            data_dir,
        )?;

        // ── Wire destroy listener + (Linux only) parent listener ─────────
        let destroy_window = surface_inner_window(&surface);
        if let Some(w) = destroy_window.as_ref() {
            Self::install_child_destroy_listener(self, pkg_id.to_string(), pane_id.to_string(), w);
        }

        let handle = Arc::new(PaneHandle {
            surface,
            label: label.clone(),
            parent_label: parent_label.to_string(),
            partition: partition.to_string(),
            current_url: RwLock::new(Some(url.to_string())),
            stored_rect: RwLock::new(rect),
            paused: std::sync::atomic::AtomicBool::new(false),
        });
        self.panes
            .write()
            .map_err(|_| anyhow!("panes lock poisoned"))?
            .insert((pkg_id.to_string(), pane_id.to_string()), handle);

        #[cfg(target_os = "linux")]
        self.install_parent_listener(&parent_window);

        log::info!(
            "[pkg_webview] created `{label}` pkg={pkg_id} pane={pane_id} \
             partition={partition} url={url} rect=({},{} {}x{}) kind={}",
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            self.lookup(pkg_id, pane_id)
                .map(|h| h.surface.kind())
                .unwrap_or("?"),
        );
        Ok(label)
    }

    pub fn destroy(&self, pkg_id: &str, pane_id: &str) -> Result<()> {
        let removed = self
            .panes
            .write()
            .map_err(|_| anyhow!("panes lock poisoned"))?
            .remove(&(pkg_id.to_string(), pane_id.to_string()));
        if let Some(handle) = removed {
            if let Err(e) = handle.surface.close() {
                log::warn!(
                    "[pkg_webview] close `{}` failed (continuing): {e}",
                    handle.label
                );
            }
            log::info!("[pkg_webview] destroyed `{}`", handle.label);
        }
        Ok(())
    }

    pub fn navigate(&self, pkg_id: &str, pane_id: &str, url: &str) -> Result<()> {
        let handle = self
            .lookup(pkg_id, pane_id)
            .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;
        let parsed = url::Url::parse(url).with_context(|| format!("parse url `{url}`"))?;

        let cap = self
            .pkg_capabilities
            .read()
            .map_err(|_| anyhow!("pkg_capabilities lock poisoned"))?
            .get(pkg_id)
            .cloned()
            .unwrap_or_default();
        let origin = parsed.origin().unicode_serialization();
        if !origin_allowed(&origin, &cap.allowed_origins) {
            return Err(anyhow!(
                "origin `{origin}` is not in `capabilities.webview.allowed_origins` \
                 (declared: {:?})",
                cap.allowed_origins.as_deref().unwrap_or(&[])
            ));
        }
        handle
            .surface
            .navigate(parsed)
            .with_context(|| format!("navigate `{}` -> {url}", handle.label))?;
        if let Ok(mut cur) = handle.current_url.write() {
            *cur = Some(url.to_string());
        }
        Ok(())
    }

    pub fn eval(&self, pkg_id: &str, pane_id: &str, js: &str) -> Result<()> {
        let handle = self
            .lookup(pkg_id, pane_id)
            .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;
        handle
            .surface
            .eval(js)
            .with_context(|| format!("eval into `{}`", handle.label))?;
        Ok(())
    }

    pub fn set_paused(&self, pkg_id: &str, pane_id: &str, paused: bool) -> Result<bool> {
        let handle = self
            .lookup(pkg_id, pane_id)
            .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;
        let prev = handle
            .paused
            .swap(paused, std::sync::atomic::Ordering::SeqCst);
        log::info!("[pkg_webview] pane=({pkg_id},{pane_id}) paused {prev} -> {paused}");
        Ok(prev)
    }

    pub fn clear_session(&self, app: &AppHandle, pkg_id: &str, pane_id: &str) -> Result<()> {
        let (_partition, data_dir) = {
            let handle = self
                .lookup(pkg_id, pane_id)
                .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;
            
            #[cfg(target_os = "macos")]
            {
                if let PaneSurface::InWindow(w) = &handle.surface {
                    if let Err(e) = w.clear_all_browsing_data() {
                        log::warn!("[pkg_webview] clear_all_browsing_data failed on macOS: {e}");
                    }
                }
            }

            let partition = handle.partition.clone();
            let data_dir = partition_dir(app, pkg_id, &partition)?;
            (partition, data_dir)
        };

        // Ordering constraint (G-23): Destroy webview and release network process before unlinking
        if let Err(e) = self.destroy(pkg_id, pane_id) {
            log::warn!("[pkg_webview] clear_session destroy failed for ({pkg_id}, {pane_id}): {e}");
        }

        // Wipe session storage on disk
        if std::fs::metadata(&data_dir).is_ok() {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                std::fs::remove_dir_all(&data_dir)
                    .with_context(|| format!("remove webjars dir {}", data_dir.display()))?;
            }
        }
        Ok(())
    }

    pub fn cleanup_clear_on_exit(&self, app: &AppHandle) {
        // macOS: We cannot easily wipe WKWebView data store for a specific identifier without the webview running.
        // For now, we rely on the fact that WKWebView stores its data in the app's Library/WebKit directory,
        // but since we can't target the exact `clear-on-exit` partition easily without the webview, we only
        // clear the Linux/Windows data directories here.
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            if let Ok(app_data) = app.path().app_data_dir() {
                let webjars_dir = app_data.join("webjars");
                if let Ok(entries) = std::fs::read_dir(&webjars_dir) {
                    for entry in entries.flatten() {
                        let clear_on_exit_dir = entry.path().join("clear-on-exit");
                        if std::fs::metadata(&clear_on_exit_dir).is_ok() {
                            if let Err(e) = std::fs::remove_dir_all(&clear_on_exit_dir) {
                                log::warn!("[pkg_webview] failed to wipe clear-on-exit dir {}: {e}", clear_on_exit_dir.display());
                            } else {
                                log::info!("[pkg_webview] wiped clear-on-exit partition at {}", clear_on_exit_dir.display());
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn is_paused(&self, pkg_id: &str, pane_id: &str) -> Result<bool> {
        let handle = self
            .lookup(pkg_id, pane_id)
            .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;
        Ok(handle.paused.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub fn set_rect(&self, pkg_id: &str, pane_id: &str, rect: PaneRect) -> Result<()> {
        let handle = self
            .lookup(pkg_id, pane_id)
            .ok_or_else(|| anyhow!("no webview pane for ({pkg_id}, {pane_id})"))?;

        if let Ok(mut r) = handle.stored_rect.write() {
            *r = rect;
        }

        set_surface_rect(&handle.surface, &handle.label, &handle.parent_label, rect)
    }

    fn lookup(&self, pkg_id: &str, pane_id: &str) -> Option<Arc<PaneHandle>> {
        self.panes
            .read()
            .ok()?
            .get(&(pkg_id.to_string(), pane_id.to_string()))
            .cloned()
    }

    /// Per-pane: hook the child surface's events so external close (user
    /// clicks WM close button, compositor kills the surface, etc.) is
    /// reflected in the panes map. Uses `Weak<Self>` to avoid a cycle.
    /// Only meaningful on Linux (top-level window can be closed from the
    /// WM); macOS/Windows in-window children don't have a separate close
    /// affordance, but the listener is harmless there too.
    fn install_child_destroy_listener(
        self: &Arc<Self>,
        pkg_id: String,
        pane_id: String,
        window: &WebviewWindow,
    ) {
        let weak_self: Weak<Self> = Arc::downgrade(self);
        window.on_window_event(move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                if let Some(reg) = weak_self.upgrade() {
                    if let Ok(mut g) = reg.panes.write() {
                        if g.remove(&(pkg_id.clone(), pane_id.clone())).is_some() {
                            log::info!(
                                "[pkg_webview] surface externally destroyed; cleaned up ({pkg_id}, {pane_id})"
                            );
                        }
                    }
                }
            }
        });
    }

    /// Linux only. macOS/Windows in-window children auto-track their
    /// parent's geometry — no listener needed. On Linux the borderless
    /// top-level surface is a separate OS window that needs to be moved
    /// in lockstep with the main window; this listener does that.
    #[cfg(target_os = "linux")]
    fn install_parent_listener(self: &Arc<Self>, parent_window: &WebviewWindow) {
        let parent_label = parent_window.label().to_string();
        // One listener per distinct parent window (idempotent for the same
        // label). Previously this was a single OnceLock listener bound to
        // "main", so panes in any other window never repositioned on move /
        // resize (multi-window: WP-04).
        {
            let mut installed = match self.parent_listeners.write() {
                Ok(g) => g,
                Err(_) => return,
            };
            // `insert` returns false if the label was already present.
            if !installed.insert(parent_label.clone()) {
                return;
            }
        }
        let registry = self.clone();
        let parent = parent_window.clone();
        let listener_label = parent_label.clone();
        parent_window.on_window_event(move |event| match event {
            WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
                let parent_pos = match parent.inner_position() {
                    Ok(p) => p,
                    Err(_) => return,
                };
                let panes = match registry.panes.read() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                for handle in panes.values() {
                    // Only reposition panes parented to THIS window.
                    if handle.parent_label != listener_label {
                        continue;
                    }
                    let rect = match handle.stored_rect.read() {
                        Ok(r) => *r,
                        Err(_) => continue,
                    };
                    let PaneSurface::TopLevel(w) = &handle.surface;
                    let _ = w.set_position(PhysicalPosition::new(
                        parent_pos.x + rect.x,
                        parent_pos.y + rect.y,
                    ));
                    let _ = w.set_size(LogicalSize::new(rect.w as f64, rect.h as f64));
                }
            }
            _ => {}
        });
        log::info!(
            "[pkg_webview] Linux parent-window event listener installed for `{parent_label}`"
        );
    }

    /// Tear down every pane parented to `parent_label` (that window was
    /// destroyed) and prune its parent-tracking bookkeeping. Cross-OS: on
    /// macOS/Windows the in-window child dies with the parent but its panes-map
    /// entry would otherwise linger until pkg uninstall; on Linux the top-level
    /// child surface plus the `parent_listeners` entry both leak (the label
    /// would stay in the set forever, so a future window reusing the label
    /// would never get a move/resize listener). Called from the window
    /// registry's Destroyed hook and its liveness reconcile.
    pub fn cleanup_for_parent(&self, parent_label: &str) {
        let keys: Vec<(String, String)> = match self.panes.read() {
            Ok(g) => g
                .iter()
                .filter(|(_, h)| h.parent_label == parent_label)
                .map(|(k, _)| k.clone())
                .collect(),
            Err(_) => return,
        };
        for (pkg_id, pane_id) in &keys {
            if let Err(e) = self.destroy(pkg_id, pane_id) {
                tracing::warn!(
                    "[pkg_webview] parent `{parent_label}` gone; pane ({pkg_id},{pane_id}) cleanup failed: {e}"
                );
            }
        }
        if let Ok(mut installed) = self.parent_listeners.write() {
            installed.remove(parent_label);
        }
        if !keys.is_empty() {
            tracing::info!(
                "[pkg_webview] cleaned up {} pane(s) parented to destroyed `{parent_label}`",
                keys.len()
            );
        }
    }

    pub fn statuses(&self) -> Vec<WebviewPaneStatus> {
        let g = match self.panes.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        g.iter()
            .map(|((pkg_id, pane_id), h)| {
                let stored_rect = h.stored_rect.read().ok().map(|r| *r).unwrap_or(PaneRect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                });
                WebviewPaneStatus {
                    pkg_id: pkg_id.clone(),
                    pane_id: pane_id.clone(),
                    webview_label: h.label.clone(),
                    current_url: h.current_url.read().ok().and_then(|c| c.clone()),
                    partition: h.partition.clone(),
                    stored_rect,
                    live_position: h.surface.position(),
                    live_size: h.surface.size(),
                    surface_kind: h.surface.kind(),
                    paused: h.paused.load(std::sync::atomic::Ordering::SeqCst),
                }
            })
            .collect()
    }
}

impl Registry for WebviewPanesRegistry {
    fn name(&self) -> &'static str {
        "webview_panes"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        let cap = pkg
            .manifest
            .capabilities
            .as_ref()
            .and_then(|c| c.webview.as_ref())
            .map(|w| PkgCapability {
                child_webviews: w.child_webviews,
                partitions: w.partitions.clone(),
                allowed_origins: w.allowed_origins.clone(),
            })
            .unwrap_or_default();
        self.pkg_capabilities
            .write()
            .map_err(|_| anyhow!("pkg_capabilities lock poisoned"))?
            .insert(pkg.manifest.id.clone(), cap);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let keys: Vec<_> = self
            .panes
            .read()
            .map(|g| {
                g.keys()
                    .filter(|(p, _)| p == pkg_id)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (pkg, pane) in keys {
            if let Err(e) = self.destroy(&pkg, &pane) {
                log::warn!("[pkg_webview] unregister cleanup for ({pkg},{pane}) failed: {e}");
            }
        }
        self.pkg_capabilities
            .write()
            .map_err(|_| anyhow!("pkg_capabilities lock poisoned"))?
            .remove(pkg_id);
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let entries = self.statuses();
        serde_json::json!({
            "count": entries.len(),
            "entries": entries,
        })
    }
}

// ── OS-specific surface construction ─────────────────────────────────────────

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn build_surface(
    _app: &AppHandle,
    parent_window: &WebviewWindow,
    label: &str,
    parsed_url: url::Url,
    rect: PaneRect,
    partition: &str,
    pkg_id: &str,
    data_dir: PathBuf,
) -> Result<PaneSurface> {
    // Only the macOS branch below mutates this.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut builder = WebviewBuilder::new(label, WebviewUrl::External(parsed_url))
        .auto_resize()
        .data_directory(data_dir)
        .on_page_load(move |webview, payload| {
            if matches!(payload.event(), PageLoadEvent::Finished) {
                log::debug!(
                    "[pkg_webview] `{}` page_load_finished url={}",
                    webview.label(),
                    payload.url()
                );
            }
        });

    #[cfg(target_os = "macos")]
    {
        let _ = (pkg_id, partition); // silence unused on non-macos branches
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(pkg_id.as_bytes());
        hasher.update(b"/");
        hasher.update(partition.as_bytes());
        let digest = hasher.finalize();
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        builder = builder.data_store_identifier(id);
    }
    #[cfg(target_os = "windows")]
    let _ = (pkg_id, partition);

    let webview = parent_window
        .as_ref()
        .window()
        .add_child(
            builder,
            LogicalPosition::new(rect.x as f64, rect.y as f64),
            LogicalSize::new(rect.w as f64, rect.h as f64),
        )
        .with_context(|| format!("add_child {label}"))?;

    Ok(PaneSurface::InWindow(webview))
}

#[cfg(target_os = "linux")]
fn build_surface(
    app: &AppHandle,
    parent_window: &WebviewWindow,
    label: &str,
    parsed_url: url::Url,
    rect: PaneRect,
    _partition: &str,
    _pkg_id: &str,
    data_dir: PathBuf,
) -> Result<PaneSurface> {
    // Translate pane rect (parent-window-client coords) → screen coords for
    // the borderless top-level window. inner_position is the screen
    // position of the parent webview's client area (excludes OS frame).
    let parent_pos = parent_window
        .inner_position()
        .context("read parent window inner_position")?;
    let screen_x = parent_pos.x + rect.x;
    let screen_y = parent_pos.y + rect.y;

    let builder = WebviewWindowBuilder::new(app, label, WebviewUrl::External(parsed_url))
        .decorations(false)
        .resizable(false)
        .skip_taskbar(true)
        .position(screen_x as f64, screen_y as f64)
        .inner_size(rect.w as f64, rect.h as f64)
        .data_directory(data_dir)
        .parent(parent_window)
        .context("set parent window for child webview")?;

    let window = builder.build().with_context(|| format!("build {label}"))?;

    // Wayland workaround: the builder's `.position(x, y)` is silently
    // ignored on Wayland (xdg-shell doesn't let apps choose top-level
    // positions). Some compositors honor `set_position` after the surface
    // is mapped — try once. To use this on a Wayland session, launch the
    // app with `GDK_BACKEND=x11` so it routes through XWayland.
    if let Err(e) = window.set_position(PhysicalPosition::new(screen_x, screen_y)) {
        log::warn!("[pkg_webview] post-build set_position for `{label}` failed (continuing): {e}");
    }

    Ok(PaneSurface::TopLevel(window))
}

/// Linux: returns the WebviewWindow underneath the surface so the destroy
/// listener can hook it. macOS/Windows: returns None (in-window children
/// don't have a separate window-event surface; their lifecycle is bonded
/// to the parent already).
fn surface_inner_window(surface: &PaneSurface) -> Option<WebviewWindow> {
    match surface {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        PaneSurface::InWindow(_) => None,
        #[cfg(target_os = "linux")]
        PaneSurface::TopLevel(w) => Some(w.clone()),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn set_surface_rect(
    surface: &PaneSurface,
    label: &str,
    _parent_label: &str,
    rect: PaneRect,
) -> Result<()> {
    // In-window children track their parent automatically — pane-relative
    // coords, parent label unused.
    let PaneSurface::InWindow(w) = surface;
    w.set_position(LogicalPosition::new(rect.x as f64, rect.y as f64))
        .with_context(|| format!("set_position `{label}`"))?;
    w.set_size(LogicalSize::new(rect.w as f64, rect.h as f64))
        .with_context(|| format!("set_size `{label}`"))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_surface_rect(
    surface: &PaneSurface,
    label: &str,
    parent_label: &str,
    rect: PaneRect,
) -> Result<()> {
    let PaneSurface::TopLevel(w) = surface;
    let app = w.app_handle();
    // Resolve the pane's OWN parent window — not the literal "main" — so a
    // pane in a detached window translates against the right client origin
    // (multi-window: WP-04).
    let parent_window = app
        .get_webview_window(parent_label)
        .ok_or_else(|| anyhow!("no webview window labeled `{parent_label}` for pane `{label}`"))?;
    let parent_pos = parent_window
        .inner_position()
        .context("read parent window inner_position")?;
    w.set_position(PhysicalPosition::new(
        parent_pos.x + rect.x,
        parent_pos.y + rect.y,
    ))
    .with_context(|| format!("set_position `{label}`"))?;
    w.set_size(LogicalSize::new(rect.w as f64, rect.h as f64))
        .with_context(|| format!("set_size `{label}`"))?;
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn webview_label(pkg_id: &str, pane_id: &str) -> String {
    let pkg_slug = pkg_id.replace('.', "-");
    let pane_slug: String = pane_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("pkg-{pkg_slug}-{pane_slug}")
}

fn partition_dir(app: &AppHandle, pkg_id: &str, partition: &str) -> Result<PathBuf> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| anyhow!("app_data_dir: {e}"))?;
    let pkg_slug = pkg_id.replace('.', "-");
    Ok(base.join("webjars").join(pkg_slug).join(partition))
}

/// Hop to the GTK / NSApplication main thread and run `f` there, awaiting
/// its result. All `WebviewPanesRegistry` methods that touch Tauri builder
/// APIs (`build`, `set_position`, `set_size`, `webview.eval`, `webview.close`,
/// `webview.navigate`) post to the main loop synchronously and block until
/// it pumps. Calling them from a tokio worker (axum handler, async Tauri
/// command) without this hop hangs on Linux WebKitGTK because the main
/// loop never gets a tick while our task is blocked.
///
/// Both caller surfaces are expected to wrap their kernel invocations in
/// this helper — Phase 3c report (`plans/shell/2026-05-13-pkg-browser-
/// phase-3c-report.md`) walks through the original diagnosis. The legacy
/// `on_main` in `iyke/browser_handlers.rs` is kept as a thin axum-typed
/// adapter so the StatusCode error surface there stays unchanged; this
/// version returns `anyhow::Result` for use from Tauri commands.
pub async fn run_on_main<F, R>(app: &tauri::AppHandle, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R> + Send + 'static,
    R: Send + 'static,
{
    
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| anyhow!("run_on_main_thread: {e}"))?;
    rx.await
        .map_err(|e| anyhow!("main-thread channel closed: {e}"))?
}

#[cfg(test)]
mod origin_tests {
    use super::{origin_allowed, origin_matches};

    fn list(v: &[&str]) -> Option<Vec<String>> {
        Some(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn absent_field_is_permissive_so_pre_v4_manifests_still_mount() {
        // G-28: `#[serde(default)]` on a Vec made "absent" indistinguishable
        // from "deny everything", which broke the ui-webview scaffold template.
        assert!(origin_allowed("https://anything.example.com", &None));
    }

    #[test]
    fn declared_but_empty_is_an_explicit_lockdown() {
        assert!(!origin_allowed("https://example.com", &list(&[])));
    }

    #[test]
    fn exact_origin_matches() {
        assert!(origin_allowed("https://sentry.io", &list(&["https://sentry.io"])));
        assert!(!origin_allowed("https://evil.io", &list(&["https://sentry.io"])));
    }

    #[test]
    fn star_allows_everything() {
        assert!(origin_allowed("https://whatever.test", &list(&["*"])));
    }

    #[test]
    fn subdomain_glob_matches_the_poc_manifests() {
        // G-35: the three PoC packages ship exactly these patterns, and the
        // old exact-comparison matcher rejected every one of them at mount.
        assert!(origin_allowed(
            "https://artists.spotify.com",
            &list(&["https://*.spotify.com"])
        ));
        assert!(origin_allowed("https://sentry.io", &list(&["https://*.sentry.io", "https://sentry.io"])));
        assert!(origin_allowed("https://www.notion.so", &list(&["https://*.notion.so"])));
    }

    #[test]
    fn subdomain_glob_is_strict_about_scheme_apex_and_suffix_boundaries() {
        assert!(!origin_matches("http://a.example.com", "https://*.example.com"));
        // bare apex does not match `*.example.com` — list it explicitly
        assert!(!origin_matches("https://example.com", "https://*.example.com"));
        // must break on a dot: notexample.com is not a subdomain of example.com
        assert!(!origin_matches("https://notexample.com", "https://*.example.com"));
        assert!(!origin_matches("https://evil.com/example.com", "https://*.example.com"));
    }

    #[test]
    fn malformed_patterns_fail_closed_rather_than_widening() {
        assert!(!origin_matches("https://a.example.com", "https://*"));
        assert!(!origin_matches("https://a.example.com", "*.example.com"));
        assert!(!origin_matches("https://a.example.com", "https://*./"));
    }
}
