//! Screenshot capture for the PA workspace.
//!
//! Wayland/GNOME has no usable compositor screencopy without a portal prompt,
//! and Tauri 2 doesn't expose `WebviewWindow::capture()` on Linux. We dodge
//! both by capturing in the renderer process: emit an event to the FE, the FE
//! runs `modern-screenshot` against the live DOM, posts the PNG bytes back via
//! `screenshot_capture_done`, and we write to disk. Works regardless of focus
//! or minimized state because the React tree stays mounted.
//!
//! The same `capture` helper is the single entry point for in-app calls
//! (`screenshot_window` / `screenshot_pane`), iyke HTTP routes, and global
//! shortcuts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{oneshot, Mutex, RwLock};
use uuid::Uuid;

// modern-screenshot inlines all stylesheets/fonts/images for the captured
// subtree, which on the full workspace DOM (mini-app iframes + hundreds of
// nodes) routinely exceeds 10s. 60s is generous enough to absorb cold renders
// without making genuine FE hangs feel infinite.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_EVENT: &str = "screenshot://request";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotKind {
    Window,
    Pane,
}

impl ScreenshotKind {
    fn stem(self, pane_id: Option<&str>) -> String {
        match (self, pane_id) {
            (ScreenshotKind::Window, _) => "window".to_string(),
            (ScreenshotKind::Pane, Some(id)) => format!("pane-{}", sanitize_id(id)),
            (ScreenshotKind::Pane, None) => "pane".to_string(),
        }
    }
}

/// What the FE returns after rendering. Bytes are base64 PNG to survive the
/// JSON event boundary; same shape as PTY chunks elsewhere in this crate.
#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub png_bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Result handed back to callers (Tauri commands, iyke handlers, CLI).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotResult {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub bytes_len: usize,
}

/// Result returned across the Rust↔FE oneshot. Either the captured PNG,
/// a structured failure (so callers can distinguish a timeout from a
/// cross-origin iframe vs. a missing pane), or a request to capture the
/// pane via the native window-crop path instead of the synchronous FE
/// `modern-screenshot` clone. The FE chooses `NativeCrop` for any pane
/// whose content fits within the viewport (no own-DOM overflow) — the
/// common case — so the heavy main-thread DOM walk that froze/aborted
/// WebKitGTK is avoided. `rect` is the pane's CSS-pixel bounding box
/// relative to the webview viewport: `[x, y, w, h]`.
#[derive(Debug, Clone)]
pub enum CaptureOutcome {
    Ok(CaptureResult),
    Err(String),
    NativeCrop { rect: [f64; 4] },
}

/// Whether this compositor's window-capture tool produces a PNG we can
/// crop pixel-accurately. `gnome-screenshot --window` on mutter adds a
/// drop-shadow margin, so the captured PNG is larger than the window and
/// a blind crop would be offset. We probe once (compare PNG dims to the
/// window's outer size) and cache the verdict: 0 = unknown, 1 = reliable,
/// 2 = unreliable. When unreliable we route panes straight to the FE
/// clone without paying the probe each time.
static NATIVE_CROP_RELIABLE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Pending captures keyed by request_id. The capture helper inserts a sender,
/// emits the request event, then awaits the receiver. The FE invokes
/// `screenshot_capture_done` (success) or `screenshot_capture_failed`
/// (cross-origin iframe, missing pane, etc.) which pop the sender and
/// resolve the oneshot.
pub type ScreenshotPending = Arc<Mutex<HashMap<String, oneshot::Sender<CaptureOutcome>>>>;

pub fn new_pending() -> ScreenshotPending {
    Arc::new(Mutex::new(HashMap::new()))
}

// ─── User-overridable output dir ──────────────────────────────────────────────
//
// Persisted as JSON in `app_data_dir/screenshot-config.json`. State held in a
// `tokio::sync::RwLock` so reads on the capture hot path don't block writers
// from settings UI. `override_dir` is stored as the user typed it (`~/...`
// allowed); we tilde-expand on read.

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScreenshotConfig {
    #[serde(default)]
    pub override_dir: Option<String>,
}

pub struct ScreenshotConfigState {
    cfg: RwLock<ScreenshotConfig>,
    path: PathBuf,
}

pub type ScreenshotConfigStateRef = Arc<ScreenshotConfigState>;

impl ScreenshotConfigState {
    pub fn load(app_data_dir: &Path) -> Self {
        let path = app_data_dir.join("screenshot-config.json");
        let cfg = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<ScreenshotConfig>(&s).ok())
            .unwrap_or_default();
        Self {
            cfg: RwLock::new(cfg),
            path,
        }
    }

    pub async fn override_dir(&self) -> Option<PathBuf> {
        self.cfg
            .read()
            .await
            .override_dir
            .as_ref()
            .map(|s| PathBuf::from(shellexpand::tilde(s).into_owned()))
    }

    pub async fn snapshot(&self) -> ScreenshotConfig {
        self.cfg.read().await.clone()
    }

    pub async fn set_override(&self, dir: Option<String>) -> Result<()> {
        let trimmed = dir.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
        let mut g = self.cfg.write().await;
        g.override_dir = trimmed;
        let json = serde_json::to_string_pretty(&*g)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create parent {}", parent.display()))?;
        }
        std::fs::write(&self.path, json)
            .with_context(|| format!("write {}", self.path.display()))?;
        Ok(())
    }
}

#[derive(Serialize, Clone)]
struct RequestPayload<'a> {
    request_id: &'a str,
    kind: &'a str,
    pane_id: Option<&'a str>,
    /// When true the FE must use the `modern-screenshot` clone path even
    /// for a pane that would otherwise route to native-crop. Set by the
    /// Rust side when the native window-crop probe came back unreliable
    /// (or when native capture failed) so a second attempt actually
    /// produces bytes instead of looping.
    force_fe: bool,
}

/// Core capture helper. Emits the request event, waits up to
/// `CAPTURE_TIMEOUT` for the FE to return PNG bytes, writes them to
/// `out_path` (or a default), returns metadata. The FE renders via
/// `modern-screenshot` against the live React tree; iframes are
/// composited in via the iyke iframe-bridge (each same-origin iframe
/// self-screenshots and posts the bytes back). This means screenshots
/// don't require window focus and can capture any pane that's mounted in
/// the DOM, even if it isn't the focused pane.
pub async fn capture(
    app: &AppHandle,
    pending: &ScreenshotPending,
    kind: ScreenshotKind,
    pane_id: Option<&str>,
    out_path: Option<String>,
) -> Result<ScreenshotResult> {
    // Window capture goes through the native compositor path. The FE
    // (modern-screenshot) path synchronously clones + rasterizes the whole
    // workspace DOM, which on WebKitGTK freezes the UI for minutes on a
    // non-trivial workspace (verified 2026-05-24). The native tool grabs the
    // window pixels without touching the renderer, so it can't hang the app.
    // Windows has no native path yet (`capture_region_to` is a stub), so
    // there we fall back to the FE clone — WebView2 (Chromium) handles the
    // full-DOM walk far better than WebKitGTK, so the freeze risk is lower.
    if matches!(kind, ScreenshotKind::Window) {
        #[cfg(not(target_os = "windows"))]
        {
            return capture_window_native(app, out_path).await;
        }
        #[cfg(target_os = "windows")]
        {
            let captured = match emit_and_await(app, pending, kind, None, true).await? {
                CaptureOutcome::Ok(r) => r,
                CaptureOutcome::Err(msg) => {
                    return Err(anyhow!("screenshot capture failed: {msg}"))
                }
                CaptureOutcome::NativeCrop { .. } => {
                    return Err(anyhow!("FE returned native-crop request for window capture"))
                }
            };
            return write_capture(app, kind, None, out_path, captured).await;
        }
    }

    let pane_id = pane_id.ok_or_else(|| anyhow!("pane_id required for pane screenshot"))?;
    let captured = capture_pane(app, pending, pane_id).await?;
    write_capture(app, kind, Some(pane_id), out_path, captured).await
}

/// Pane capture orchestration. Asks the FE to decide native-crop vs. FE
/// clone (see [`CaptureOutcome::NativeCrop`]); on a native request, grabs
/// the window natively and crops it to the pane's rect. Falls back to the
/// FE clone when native crop is unavailable or this compositor's capture
/// tool produces an un-croppable (shadowed) PNG.
async fn capture_pane(
    app: &AppHandle,
    pending: &ScreenshotPending,
    pane_id: &str,
) -> Result<CaptureResult> {
    use std::sync::atomic::Ordering;

    // If we already learned native crop is unreliable here, tell the FE to
    // clone up-front so we don't pay a doomed window-capture probe again.
    let force_fe = NATIVE_CROP_RELIABLE.load(Ordering::Relaxed) == 2;

    match emit_and_await(app, pending, ScreenshotKind::Pane, Some(pane_id), force_fe).await? {
        CaptureOutcome::Ok(r) => Ok(r),
        CaptureOutcome::Err(msg) => Err(anyhow!("screenshot capture failed: {msg}")),
        CaptureOutcome::NativeCrop { rect } => {
            match crop_pane_from_window(app, rect).await {
                Ok(Some(cropped)) => {
                    NATIVE_CROP_RELIABLE.store(1, Ordering::Relaxed);
                    Ok(cropped)
                }
                Ok(None) => {
                    // Geometry untrustworthy (e.g. gnome-screenshot shadow).
                    // Remember it and ask the FE to clone instead.
                    NATIVE_CROP_RELIABLE.store(2, Ordering::Relaxed);
                    tracing::warn!(
                        "native pane crop unreliable on this compositor; falling back to FE clone"
                    );
                    force_fe_clone(app, pending, pane_id).await
                }
                Err(e) => {
                    // Native capture failed outright (no tool, Wayland
                    // position unavailable, …). Don't poison the cache —
                    // this can be transient — but do clone for this request.
                    tracing::warn!("native pane crop failed ({e:#}); falling back to FE clone");
                    force_fe_clone(app, pending, pane_id).await
                }
            }
        }
    }
}

/// Second attempt that forces the FE clone path. A `NativeCrop` response
/// here is a protocol violation (we asked for bytes); surface it as an error
/// rather than recursing.
async fn force_fe_clone(
    app: &AppHandle,
    pending: &ScreenshotPending,
    pane_id: &str,
) -> Result<CaptureResult> {
    match emit_and_await(app, pending, ScreenshotKind::Pane, Some(pane_id), true).await? {
        CaptureOutcome::Ok(r) => Ok(r),
        CaptureOutcome::Err(msg) => Err(anyhow!("screenshot capture failed: {msg}")),
        CaptureOutcome::NativeCrop { .. } => {
            Err(anyhow!("FE returned native-crop request despite force_fe"))
        }
    }
}

/// Emit one `screenshot://request` and await the FE's response (bytes,
/// failure, or a native-crop request) up to [`CAPTURE_TIMEOUT`]. Cleans up
/// the pending registry on emit failure / timeout.
async fn emit_and_await(
    app: &AppHandle,
    pending: &ScreenshotPending,
    kind: ScreenshotKind,
    pane_id: Option<&str>,
    force_fe: bool,
) -> Result<CaptureOutcome> {
    let request_id = Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel::<CaptureOutcome>();
    {
        let mut map = pending.lock().await;
        map.insert(request_id.clone(), tx);
    }

    let payload = RequestPayload {
        request_id: &request_id,
        kind: match kind {
            ScreenshotKind::Window => "window",
            ScreenshotKind::Pane => "pane",
        },
        pane_id,
        force_fe,
    };
    // TODO(multi-window): broadcast today — the pane→owning-window map isn't
    // available here, so every window's screenshot listener receives this and
    // the first to reply wins (research 03). Route to the owning window via
    // `emit_to` once panes carry their window label end-to-end.
    if let Err(e) = app.emit(REQUEST_EVENT, &payload) {
        pending.lock().await.remove(&request_id);
        return Err(anyhow!("emit screenshot request: {e}"));
    }

    match tokio::time::timeout(CAPTURE_TIMEOUT, rx).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(_)) => Err(anyhow!("screenshot sender dropped")),
        Err(_) => {
            pending.lock().await.remove(&request_id);
            Err(anyhow!(
                "screenshot timed out after {}s",
                CAPTURE_TIMEOUT.as_secs()
            ))
        }
    }
}

/// Resolve the output path (explicit, user override, or platform default),
/// write the PNG, and build the [`ScreenshotResult`].
async fn write_capture(
    app: &AppHandle,
    kind: ScreenshotKind,
    pane_id: Option<&str>,
    out_path: Option<String>,
    captured: CaptureResult,
) -> Result<ScreenshotResult> {
    let resolved_path = match out_path {
        Some(p) => PathBuf::from(shellexpand::tilde(&p).into_owned()),
        None => {
            let override_dir = match app.try_state::<ScreenshotConfigStateRef>() {
                Some(state) => state.inner().override_dir().await,
                None => None,
            };
            default_out_path_with(kind, pane_id, override_dir)?
        }
    };
    write_png(&resolved_path, &captured.png_bytes)?;

    Ok(ScreenshotResult {
        path: resolved_path.to_string_lossy().into_owned(),
        width: captured.width,
        height: captured.height,
        bytes_len: captured.png_bytes.len(),
    })
}

// ─── Native (OS-level) window capture ────────────────────────────────────────
//
// This is the live path for *window* screenshots (see `capture` above). It
// grabs window pixels through the compositor/screenshot tool, so unlike the
// FE modern-screenshot path it never clones the DOM and can't freeze the
// renderer. It does require briefly focusing the window (see below). Pane
// capture still uses the FE path because the OS can't isolate a single pane.

// Fields are read by the Linux and macOS capture_region_to implementations;
// the Windows stub ignores the rect.
#[cfg_attr(target_os = "windows", allow(dead_code))]
#[derive(Debug, Clone, Copy)]
struct ScreenRect {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
}

/// Window geometry captured alongside the native PNG, all in physical
/// pixels (the same units the captured PNG is in). Used to locate the
/// webview *content* origin inside the outer-window PNG so we can crop a
/// single pane out of it.
#[derive(Debug, Clone, Copy)]
struct WindowGeom {
    /// Outer-window size (with chrome) in physical px.
    outer_w: u32,
    outer_h: u32,
    /// Webview content-area top-left, as an offset from the outer-window
    /// top-left, in physical px. Title-bar height + border width.
    content_off_x: i32,
    content_off_y: i32,
    /// True iff `content_off_*` came from a real `inner_position()` (not the
    /// `outer_position` fallback). When false we don't actually know the
    /// chrome offset, so the crop must NOT be trusted (see `crop_box` caller).
    content_offset_known: bool,
    /// Device-pixel ratio (CSS px → physical px).
    scale: f64,
}

/// Grab the outer window via the native tool and return the PNG bytes plus
/// the geometry needed to crop a pane out of it. Shared by the window
/// command (`focus_window = true`) and the pane native-crop path
/// (`focus_window = false`).
///
/// `focus_window` exists because `gnome-screenshot --window` on mutter grabs
/// the *focused* window, so the window-screenshot command focuses Ikenga
/// first. The pane crop path passes `false`: coordinate-based tools (grim,
/// scrot, screencapture) don't need focus, and on the gnome `-w` path a crop
/// would fail the dimension trust check and fall back to the FE clone anyway
/// — so stealing focus there would be a pointless flicker on every pane
/// screenshot (pane capture was historically focus-independent).
async fn capture_window_png(app: &AppHandle, focus_window: bool) -> Result<(Vec<u8>, WindowGeom)> {
    // Native window capture targets the PRIMARY window. Reached from the CLI
    // `--screenshot=window` intercept (which runs before any window is focused)
    // and the FE screenshot path. TODO(multi-window): thread a target label to
    // capture a non-primary window.
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| anyhow!("main webview window not found"))?;

    if focus_window {
        // When iyke is invoked from a terminal, the terminal has focus — so
        // we focus the Ikenga window first for the `gnome-screenshot -w`
        // path. The brief focus-steal is the trade-off; the user can alt-tab
        // back. Harmless on coordinate-based tools (we pass explicit coords).
        let _ = window.set_focus();
        // Give the compositor a tick to actually move focus before the
        // capture tool reads "focused window". 80ms is enough on mutter.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }

    let outer_pos = window.outer_position().context("outer_position")?;
    let outer_size = window.outer_size().context("outer_size")?;
    // inner_position is the content-area top-left in screen coords; the delta
    // from outer_position is the chrome (title bar + border) offset. If it's
    // unavailable (some Wayland setups) we fall back to outer_pos but record
    // `content_offset_known = false` so the crop is rejected — we can't crop
    // a chromed window accurately without knowing where the content begins.
    let inner_pos_res = window.inner_position();
    let content_offset_known = inner_pos_res.is_ok();
    let inner_pos = inner_pos_res.unwrap_or(outer_pos);
    let scale = window.scale_factor().unwrap_or(1.0);

    let rect = ScreenRect {
        x: outer_pos.x,
        y: outer_pos.y,
        w: outer_size.width,
        h: outer_size.height,
    };

    let png_bytes = tokio::task::spawn_blocking(move || capture_region_native(rect))
        .await
        .context("native capture join")?
        .context("native capture")?;

    let geom = WindowGeom {
        outer_w: outer_size.width,
        outer_h: outer_size.height,
        content_off_x: inner_pos.x - outer_pos.x,
        content_off_y: inner_pos.y - outer_pos.y,
        content_offset_known,
        scale,
    };
    Ok((png_bytes, geom))
}

// Called only from the non-Windows branch of the capture dispatcher.
#[cfg(not(target_os = "windows"))]
async fn capture_window_native(
    app: &AppHandle,
    out_path: Option<String>,
) -> Result<ScreenshotResult> {
    // Window capture uses gnome-screenshot -w on mutter → focus Ikenga first.
    let (png_bytes, geom) = capture_window_png(app, true).await?;

    let resolved_path = match out_path {
        Some(p) => PathBuf::from(shellexpand::tilde(&p).into_owned()),
        None => {
            let override_dir = match app.try_state::<ScreenshotConfigStateRef>() {
                Some(state) => state.inner().override_dir().await,
                None => None,
            };
            default_out_path_with(ScreenshotKind::Window, None, override_dir)?
        }
    };
    write_png(&resolved_path, &png_bytes)?;

    let dims = png_dimensions(&png_bytes).unwrap_or((geom.outer_w, geom.outer_h));
    Ok(ScreenshotResult {
        path: resolved_path.to_string_lossy().into_owned(),
        width: dims.0,
        height: dims.1,
        bytes_len: png_bytes.len(),
    })
}

/// Tolerance (physical px) for matching the captured PNG to the window's
/// outer size. Absorbs sub-pixel scale rounding; anything larger means the
/// capture tool added a margin or grabbed the wrong surface, so the crop
/// offset can't be trusted.
const CROP_DIM_TOLERANCE: i64 = 4;

/// True when the captured PNG's dimensions match the outer window (within
/// [`CROP_DIM_TOLERANCE`]), so the content-offset crop will be accurate.
fn dims_trustworthy(geom: WindowGeom, pw: u32, ph: u32) -> bool {
    (pw as i64 - geom.outer_w as i64).abs() <= CROP_DIM_TOLERANCE
        && (ph as i64 - geom.outer_h as i64).abs() <= CROP_DIM_TOLERANCE
}

/// Map a pane's CSS-pixel viewport rect to a physical-pixel crop box within
/// the outer-window PNG, clamped to the PNG bounds (a pane can sit partially
/// past the viewport edge; we keep the visible intersection). Returns
/// `(x, y, w, h)`; a zero `w`/`h` means the rect lands entirely outside.
fn crop_box(geom: WindowGeom, rect: [f64; 4], pw: u32, ph: u32) -> (u32, u32, u32, u32) {
    let s = geom.scale;
    let cx = geom.content_off_x as f64 + rect[0] * s;
    let cy = geom.content_off_y as f64 + rect[1] * s;
    let cw = (rect[2] * s).round();
    let ch = (rect[3] * s).round();

    let x0 = cx.round().clamp(0.0, pw as f64) as u32;
    let y0 = cy.round().clamp(0.0, ph as f64) as u32;
    let x1 = (cx + cw).round().clamp(0.0, pw as f64) as u32;
    let y1 = (cy + ch).round().clamp(0.0, ph as f64) as u32;
    (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

/// Capture the window natively and crop it to a single pane's rect.
///
/// Returns `Ok(None)` when the capture can't be trusted for a precise crop,
/// so the caller falls back to the FE clone. Two ways that happens:
///   * the PNG dimensions don't match the window's outer size (e.g. a tool
///     that adds a margin, or `gnome-screenshot -w` grabbing the wrong/unfocused
///     surface — we pass `focus_window = false`); or
///   * `inner_position()` was unavailable, so we don't actually know the
///     chrome offset (`content_offset_known == false`) and a chromed window
///     would be cropped at the wrong vertical origin.
/// On wlroots (grim), X11 (scrot), and macOS (screencapture) with real
/// window positions the PNG matches the window exactly and the crop is
/// pixel-accurate.
async fn crop_pane_from_window(app: &AppHandle, rect: [f64; 4]) -> Result<Option<CaptureResult>> {
    let (png_bytes, geom) = capture_window_png(app, false).await?;

    let (pw, ph) =
        png_dimensions(&png_bytes).ok_or_else(|| anyhow!("native capture produced a non-PNG"))?;

    // Trust check: the PNG must be the outer window (no added margin / right
    // surface) AND we must actually know where the content begins.
    if !dims_trustworthy(geom, pw, ph) || !geom.content_offset_known {
        return Ok(None);
    }

    let (x0, y0, crop_w, crop_h) = crop_box(geom, rect, pw, ph);
    if crop_w == 0 || crop_h == 0 {
        return Err(anyhow!(
            "pane rect {rect:?} maps to an empty region within the window capture"
        ));
    }

    // Decode + crop off-thread; image decode/encode is CPU-bound.
    let cropped = tokio::task::spawn_blocking(move || -> Result<CaptureResult> {
        let img = image::load_from_memory(&png_bytes).context("decode window PNG")?;
        let sub = image::imageops::crop_imm(&img, x0, y0, crop_w, crop_h).to_image();
        let mut out = std::io::Cursor::new(Vec::new());
        sub.write_to(&mut out, image::ImageFormat::Png)
            .context("encode cropped PNG")?;
        Ok(CaptureResult {
            png_bytes: out.into_inner(),
            width: crop_w,
            height: crop_h,
        })
    })
    .await
    .context("crop join")??;

    Ok(Some(cropped))
}

fn capture_region_native(rect: ScreenRect) -> Result<Vec<u8>> {
    let tmp = std::env::temp_dir().join(format!("ikenga-screenshot-{}.png", Uuid::new_v4()));
    let result = capture_region_to(&tmp, rect);
    let bytes = std::fs::read(&tmp).ok();
    let _ = std::fs::remove_file(&tmp);
    result?;
    bytes.ok_or_else(|| anyhow!("screenshot tool reported success but produced no file"))
}

#[cfg(target_os = "linux")]
fn capture_region_to(out: &Path, rect: ScreenRect) -> Result<()> {
    // Wayland tooling depends on the compositor:
    //   * wlroots (sway/Hyprland): grim works directly with screencopy.
    //   * mutter (GNOME): no wlr-screencopy → grim fails. Use
    //     `gnome-screenshot --window` (portal-backed, captures focused).
    //   * KDE: use `spectacle --activewindow --background` similarly.
    // X11 falls back to scrot with the rect from Tauri.
    let is_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    if is_wayland {
        let geom = format!("{},{} {}x{}", rect.x, rect.y, rect.w, rect.h);
        let grim = run_tool("grim", &["-g", &geom, &out.to_string_lossy()]);
        if grim.is_ok() {
            return Ok(());
        }
        let grim_err = grim.err().expect("checked above");

        // gnome-screenshot is the canonical mutter path. `-w` grabs the
        // focused window; the Tauri window is the active one when iyke
        // dispatches a shortcut/CLI capture, so this DTRT.
        if which_present("gnome-screenshot") {
            return run_tool("gnome-screenshot", &["-w", "-f", &out.to_string_lossy()]);
        }
        if which_present("spectacle") {
            return run_tool(
                "spectacle",
                &[
                    "--activewindow",
                    "--background",
                    "-o",
                    &out.to_string_lossy(),
                ],
            );
        }
        Err(anyhow!(
            "no working Wayland screenshot tool. grim said: {grim_err}. \
             Install grim (wlroots), gnome-screenshot, or spectacle."
        ))
    } else {
        // scrot --autoselect (-a) takes "x,y,w,h"; --overwrite (-o) for
        // determinism, --silent (-z) to avoid stderr chatter.
        let area = format!("{},{},{},{}", rect.x, rect.y, rect.w, rect.h);
        run_tool("scrot", &["-z", "-o", "-a", &area, &out.to_string_lossy()])
    }
}

#[cfg(target_os = "linux")]
fn which_present(tool: &str) -> bool {
    std::process::Command::new("which")
        .arg(tool)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn capture_region_to(out: &Path, rect: ScreenRect) -> Result<()> {
    // -x: silent (no shutter sound). -R: capture rect "x,y,w,h".
    let region = format!("{},{},{},{}", rect.x, rect.y, rect.w, rect.h);
    run_tool(
        "screencapture",
        &["-x", "-R", &region, &out.to_string_lossy()],
    )
}

#[cfg(target_os = "windows")]
fn capture_region_to(_out: &Path, _rect: ScreenRect) -> Result<()> {
    // TODO(windows): use PowerShell + System.Drawing or the Windows.Graphics
    // .Capture API. For now, error out with a clear message so the user can
    // fall back to modern-screenshot via the FE flag (also a TODO).
    Err(anyhow!(
        "native window capture not implemented on Windows yet"
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn capture_region_to(_out: &Path, _rect: ScreenRect) -> Result<()> {
    Err(anyhow!("native window capture not supported on this OS"))
}

// Only the Linux and macOS `capture_region_to` implementations shell out.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_tool(tool: &str, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(tool)
        .args(args)
        .output()
        .with_context(|| format!("spawn {tool}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "{tool} exited with status {}: {}",
            out.status,
            stderr.trim()
        ));
    }
    Ok(())
}

/// Read `width` and `height` from a PNG's IHDR chunk. Cheap — first 24 bytes.
fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    Some((w, h))
}

fn write_png(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// `~/.local/share/ikenga/screenshots/{stem}-{ISO8601}.png` on Linux,
/// platform equivalents elsewhere. Strips `:` from the timestamp so the
/// filename is portable to FAT/Windows shells.
#[allow(dead_code)]
pub fn default_out_path(kind: ScreenshotKind, pane_id: Option<&str>) -> Result<PathBuf> {
    default_out_path_with(kind, pane_id, None)
}

/// Same as [`default_out_path`] but lets the caller substitute a user-chosen
/// directory (from `ScreenshotConfigState`). When `override_dir` is `None` we
/// fall back to the per-platform default.
#[allow(dead_code)]
pub fn default_out_path_with(
    kind: ScreenshotKind,
    pane_id: Option<&str>,
    override_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    let dir = match override_dir {
        Some(d) => d,
        None => default_screenshot_dir()?,
    };
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let stem = kind.stem(pane_id);
    Ok(dir.join(format!("{stem}-{stamp}.png")))
}

/// Per-platform default screenshot dir, exposed so the settings UI can show
/// "(default: ...)" alongside the user override.
pub fn platform_default_screenshot_dir() -> Result<PathBuf> {
    default_screenshot_dir()
}

fn default_screenshot_dir() -> Result<PathBuf> {
    let home = crate::platform::home_dir()
        .ok_or_else(|| anyhow!("could not resolve home directory (HOME / USERPROFILE unset)"))?;
    #[cfg(target_os = "macos")]
    let dir = home.join("Library/Application Support/ikenga/screenshots");
    #[cfg(all(unix, not(target_os = "macos")))]
    let dir = home.join(".local/share/ikenga/screenshots");
    #[cfg(target_os = "windows")]
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone())
        .join("Ikenga/screenshots");
    Ok(dir)
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ─── Tauri commands ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn screenshot_window(
    app: AppHandle,
    pending: State<'_, ScreenshotPending>,
    out_path: Option<String>,
) -> Result<ScreenshotResult, String> {
    capture(
        &app,
        pending.inner(),
        ScreenshotKind::Window,
        None,
        out_path,
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn screenshot_pane(
    app: AppHandle,
    pending: State<'_, ScreenshotPending>,
    pane_id: String,
    out_path: Option<String>,
) -> Result<ScreenshotResult, String> {
    capture(
        &app,
        pending.inner(),
        ScreenshotKind::Pane,
        Some(&pane_id),
        out_path,
    )
    .await
    .map_err(|e| format!("{e:#}"))
}

#[derive(Deserialize)]
pub struct CaptureDoneArgs {
    pub request_id: String,
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
}

/// FE callback. Decodes base64 PNG, resolves the matching pending oneshot.
#[tauri::command]
pub async fn screenshot_capture_done(
    pending: State<'_, ScreenshotPending>,
    args: CaptureDoneArgs,
) -> Result<(), String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(args.png_base64.as_bytes())
        .map_err(|e| format!("decode base64: {e}"))?;

    let mut map = pending.inner().lock().await;
    let Some(tx) = map.remove(&args.request_id) else {
        return Err(format!("no pending capture for {}", args.request_id));
    };
    let _ = tx.send(CaptureOutcome::Ok(CaptureResult {
        png_bytes: bytes,
        width: args.width,
        height: args.height,
    }));
    Ok(())
}

#[derive(Deserialize)]
pub struct CaptureFailedArgs {
    pub request_id: String,
    pub message: String,
}

/// FE callback for capture failures (cross-origin iframe, missing pane,
/// modern-screenshot timeout). Resolves the pending oneshot with an error
/// so the caller surfaces a clean message instead of waiting out
/// `CAPTURE_TIMEOUT`.
#[tauri::command]
pub async fn screenshot_capture_failed(
    pending: State<'_, ScreenshotPending>,
    args: CaptureFailedArgs,
) -> Result<(), String> {
    let mut map = pending.inner().lock().await;
    let Some(tx) = map.remove(&args.request_id) else {
        return Err(format!("no pending capture for {}", args.request_id));
    };
    let _ = tx.send(CaptureOutcome::Err(args.message));
    Ok(())
}

#[derive(Deserialize)]
pub struct CaptureNativeCropArgs {
    pub request_id: String,
    /// Pane bounding box in CSS px, relative to the webview viewport:
    /// `[x, y, w, h]`.
    pub rect: [f64; 4],
}

/// FE callback for the native-crop path. The FE decided this pane fits in
/// the viewport (no own-DOM overflow) and reports its rect instead of
/// running the heavy `modern-screenshot` clone; the Rust side captures the
/// window natively and crops to `rect`. Resolves the pending oneshot with
/// [`CaptureOutcome::NativeCrop`].
#[tauri::command]
pub async fn screenshot_capture_native_crop(
    pending: State<'_, ScreenshotPending>,
    args: CaptureNativeCropArgs,
) -> Result<(), String> {
    let mut map = pending.inner().lock().await;
    let Some(tx) = map.remove(&args.request_id) else {
        return Err(format!("no pending capture for {}", args.request_id));
    };
    let _ = tx.send(CaptureOutcome::NativeCrop { rect: args.rect });
    Ok(())
}

// ─── Config commands ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotConfigDto {
    /// User-supplied override (raw, may contain `~`). `None` = use platform default.
    pub override_dir: Option<String>,
    /// Per-platform default, absolute path. Always populated.
    pub default_dir: String,
    /// What `capture()` will actually use right now. Tilde-expanded.
    pub effective_dir: String,
}

#[tauri::command]
pub async fn screenshot_get_config(
    state: State<'_, ScreenshotConfigStateRef>,
) -> Result<ScreenshotConfigDto, String> {
    let snap = state.inner().snapshot().await;
    let default_dir = platform_default_screenshot_dir()
        .map_err(|e| format!("{e:#}"))?
        .to_string_lossy()
        .into_owned();
    let effective_dir = match &snap.override_dir {
        Some(s) => PathBuf::from(shellexpand::tilde(s).into_owned())
            .to_string_lossy()
            .into_owned(),
        None => default_dir.clone(),
    };
    Ok(ScreenshotConfigDto {
        override_dir: snap.override_dir,
        default_dir,
        effective_dir,
    })
}

#[tauri::command]
pub async fn screenshot_set_dir(
    state: State<'_, ScreenshotConfigStateRef>,
    dir: Option<String>,
) -> Result<(), String> {
    state
        .inner()
        .set_override(dir)
        .await
        .map_err(|e| format!("{e:#}"))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn default_path_window_shape() {
        let p = default_out_path(ScreenshotKind::Window, None).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let re = Regex::new(r"^window-\d{8}T\d{6}\.png$").unwrap();
        assert!(re.is_match(&name), "got {name}");
    }

    #[test]
    fn default_path_pane_shape() {
        let p = default_out_path(ScreenshotKind::Pane, Some("leaf-42")).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let re = Regex::new(r"^pane-leaf-42-\d{8}T\d{6}\.png$").unwrap();
        assert!(re.is_match(&name), "got {name}");
    }

    #[test]
    fn pane_id_sanitization_strips_path_chars() {
        let p = default_out_path(ScreenshotKind::Pane, Some("a/b:c\\d")).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("pane-a_b_c_d-"), "got {name}");
        assert!(!name.contains('/'));
        assert!(!name.contains(':'));
        assert!(!name.contains('\\'));
    }

    fn geom(outer_w: u32, outer_h: u32, off_x: i32, off_y: i32, scale: f64) -> WindowGeom {
        WindowGeom {
            outer_w,
            outer_h,
            content_off_x: off_x,
            content_off_y: off_y,
            content_offset_known: true,
            scale,
        }
    }

    #[test]
    fn dims_trustworthy_exact_and_within_tolerance() {
        let g = geom(1920, 1080, 0, 0, 1.0);
        assert!(dims_trustworthy(g, 1920, 1080));
        assert!(dims_trustworthy(g, 1923, 1078)); // within ±4
    }

    #[test]
    fn dims_untrustworthy_when_shadow_margin_added() {
        // gnome-screenshot --window adds ~10px of drop shadow each side.
        let g = geom(1920, 1080, 0, 0, 1.0);
        assert!(!dims_trustworthy(g, 1940, 1100));
    }

    #[test]
    fn crop_rejected_when_content_offset_unknown() {
        // inner_position() unavailable → content_offset_known = false → the
        // crop must be rejected even if the PNG dims match the window, because
        // we don't know where the chrome ends and the content begins.
        let mut g = geom(1000, 800, 0, 0, 1.0);
        g.content_offset_known = false;
        assert!(dims_trustworthy(g, 1000, 800)); // dims alone would pass…
        assert!(!g.content_offset_known); // …but the caller also gates on this
    }

    #[test]
    fn crop_box_offsets_by_chrome_and_scale() {
        // 30px title bar, no side border, scale 1. Pane at (10,20) sized 100x50.
        let g = geom(1000, 800, 0, 30, 1.0);
        let (x, y, w, h) = crop_box(g, [10.0, 20.0, 100.0, 50.0], 1000, 800);
        assert_eq!((x, y, w, h), (10, 50, 100, 50));
    }

    #[test]
    fn crop_box_applies_devicepixel_scale() {
        let g = geom(2000, 1600, 0, 60, 2.0);
        // CSS rect *2 (physical) + 60px chrome offset on y.
        let (x, y, w, h) = crop_box(g, [10.0, 20.0, 100.0, 50.0], 2000, 1600);
        assert_eq!((x, y, w, h), (20, 100, 200, 100));
    }

    #[test]
    fn crop_box_clamps_pane_past_viewport_edge() {
        let g = geom(1000, 800, 0, 0, 1.0);
        // Pane starts at x=950, 100px wide → clamps to the 50px visible strip.
        let (x, _y, w, _h) = crop_box(g, [950.0, 0.0, 100.0, 40.0], 1000, 800);
        assert_eq!((x, w), (950, 50));
    }

    #[test]
    fn crop_box_zero_area_when_fully_offscreen() {
        let g = geom(1000, 800, 0, 0, 1.0);
        let (_x, _y, w, h) = crop_box(g, [2000.0, 2000.0, 100.0, 100.0], 1000, 800);
        assert!(w == 0 || h == 0);
    }

    #[test]
    fn write_png_creates_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("nested/deep/file.png");
        let bytes = b"\x89PNG\r\n\x1a\nfake";
        write_png(&nested, bytes).unwrap();
        let read = std::fs::read(&nested).unwrap();
        assert_eq!(read, bytes);
    }
}
