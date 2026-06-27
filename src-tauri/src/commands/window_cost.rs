//! Per-window cost measurement spike (multi-window WP-01). Debug-only.
//!
//! Spawns three throwaway WebviewWindows in sequence and measures:
//!  - Spawn latency   — time from `build()` call to handle returned.
//!  - First-paint     — time until a JS `invoke('window_cost_ping')` round-trips
//!                      back (thin + full configs only; empty has no Tauri JS).
//!  - Renderer RSS    — Linux: VmRSS of the new `WebKitWebProcess` entry in
//!                      `/proc/<pid>/status`; documented-manual on macOS/Windows.
//!
//! Three configs tested in sequence:
//!  (a) "empty" — bare `about:blank`, no app content, no Tauri JS bridge.
//!  (b) "thin"  — `WindowRegistry::spawn` with `kind=single-surface, surfaces=chat`.
//!  (c) "full"  — `WindowRegistry::spawn` with `kind=workspace, surfaces=[]` (all).
//!
//! ── Per-OS RSS measurement ──────────────────────────────────────────────────
//! Linux   : This command reads VmRSS directly from `/proc/<pid>/status`,
//!           detecting new `WebKitWebProcess` children after each spawn.
//!           The sum of new-process RSS is the marginal renderer cost per window.
//!
//! macOS   : WebKit uses `WKProcessPool` — extra windows may SHARE the renderer
//!           process (`WKProcessPool.default`). To measure:
//!             before=`ps -A -o pid,rss,comm | grep WebContent`
//!             spawn the window
//!             after=same command
//!           delta=0 → shared pool (Flavor B is cheap). delta≈200 MB → per-window
//!           process (Flavor B carries same cost as Flavor C). This answers the
//!           WKProcessPool question that gates the Phase-3 slim-vs-cap decision.
//!
//! Windows : Use Process Explorer or:
//!             before=`Get-Process | ? Name -match 'Edge' | Measure WorkingSet -Sum`
//!             spawn; after=same command. delta is the renderer RSS cost.
//!
//! ── First-paint signal ──────────────────────────────────────────────────────
//! Thin/full windows load the same app URL as `main`. In dev builds,
//! `main.tsx` → `lib/dev/index.ts` → `lib/dev/window-cost.ts` installs
//! `window.__windowCostPing()`, which invokes `window_cost_ping` back into
//! Rust. `window_cost_run` polls `eval()` until the hook fires.
//!
//! ── How to run ──────────────────────────────────────────────────────────────
//! Dev build only. Open DevTools in the main window and:
//!
//!   const r = await window.__TAURI__.core.invoke('window_cost_run', {});
//!   console.table(r.rows.map(row => ({
//!     config: row.config,
//!     spawn_ms: row.spawnMs,
//!     first_paint_ms: row.firstPaintMs,
//!     rss_kb: row.rssKb,
//!     error: row.error,
//!   })));
//!   console.log('OS:', r.os, '|', r.osRssNote);
//!
//! Or via the typed FE wrapper (from DevTools or a dev-only component):
//!
//!   await window.windowCostRun();
//!
//! ── Interpreting results ────────────────────────────────────────────────────
//! The numbers feed into `plans/multi-window/04-discussion.md` (a future Round)
//! to set the Flavor-B slim-vs-cap decision and the Flavor-C cold-open budget.
//! Proposed budgets (pre-measurement, to be revised with real data):
//!   • cold-open (thin, Linux/macOS) < 500 ms → Flavor C "pass"
//!   • renderer RSS delta (thin vs empty, Linux) < 80 MB → slim-webview viable
//!
//! Hard-removed after Phase-3 sign-off (same lifecycle as bg_spike.rs).

#![cfg(debug_assertions)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::window::descriptor::{WindowDescriptor, WindowKind};
use crate::window::registry::WindowRegistry;

// ── Ping-back state ──────────────────────────────────────────────────────────

/// Holds the pending oneshot sender set by `window_cost_run` before each
/// first-paint eval poll. `window_cost_ping` takes it and fires the reply.
///
/// Single-item `Option<Sender>` is sufficient because configs run sequentially.
#[derive(Default)]
pub struct WindowCostState {
    pending: Mutex<Option<oneshot::Sender<Instant>>>,
}

pub type WindowCostStateRef = Arc<WindowCostState>;

pub fn new_state() -> WindowCostStateRef {
    Arc::new(WindowCostState::default())
}

/// Invoked by the FE `window.__windowCostPing()` hook eval'd into thin/full
/// windows. Records `Instant::now()` and fires the oneshot so `window_cost_run`
/// can compute first-paint latency.
///
/// Safe to call multiple times — after the first fire `take()` drains the
/// sender and subsequent calls are no-ops.
#[tauri::command]
pub fn window_cost_ping(cost_state: State<'_, WindowCostStateRef>) {
    let now = Instant::now();
    if let Some(tx) = cost_state
        .pending
        .lock()
        .expect("window_cost pending poisoned")
        .take()
    {
        let _ = tx.send(now);
    }
}

// ── Report types ─────────────────────────────────────────────────────────────

/// One row in the cost table, one per config.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCostRow {
    /// "empty" | "thin" | "full"
    pub config: &'static str,
    /// Milliseconds from `build()` call to WebviewWindowBuilder returning.
    pub spawn_ms: u64,
    /// Milliseconds from `build()` call to first `window_cost_ping` reply.
    /// `None` for the empty config (no Tauri JS bridge on about:blank) or
    /// if the 15-second polling window expired before a reply arrived.
    pub first_paint_ms: Option<u64>,
    /// Sum of VmRSS (kB) of all new `WebKitWebProcess` entries that appeared
    /// after this config's spawn. Linux only; `None` on macOS/Windows or when
    /// no new process was detected within 3 seconds.
    pub rss_kb: Option<u64>,
    /// Per-OS note on how RSS is (or should be) measured.
    pub rss_note: &'static str,
    /// Non-fatal error detail if spawning or eval failed.
    pub error: Option<String>,
}

/// Top-level report returned to the caller.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowCostReport {
    pub rows: Vec<WindowCostRow>,
    /// "linux" | "macos" | "windows" | "other"
    pub os: &'static str,
    /// Reminder of the per-OS manual RSS steps (see module-level doc).
    pub os_rss_note: &'static str,
}

// ── OS helpers ───────────────────────────────────────────────────────────────

fn os_name() -> &'static str {
    #[cfg(target_os = "linux")]
    return "linux";
    #[cfg(target_os = "macos")]
    return "macos";
    #[cfg(target_os = "windows")]
    return "windows";
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return "other";
}

fn os_rss_note() -> &'static str {
    #[cfg(target_os = "linux")]
    return "Linux: VmRSS from /proc/<pid>/status of new WebKitWebProcess children \
            (auto-measured). Sum over all new pids = marginal renderer cost per window.";
    #[cfg(target_os = "macos")]
    return "macOS (manual): run `ps -A -o pid,rss,comm | grep WebContent` before + \
            after each spawn. delta=0 -> shared WKProcessPool (Flavor-B cheap); \
            delta~200MB -> per-window process (same cost as Flavor-C).";
    #[cfg(target_os = "windows")]
    return "Windows (manual): Get-Process | ? Name -match 'Edge' | Measure WorkingSet -Sum \
            before + after. delta = WebView2 renderer RSS cost per window.";
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    return "RSS measurement not implemented for this OS.";
}

/// Enumerate PIDs of all `WebKitWebProcess` children by reading `/proc/*/status`.
/// Linux-only; returns an empty `Vec` on other OSes.
#[cfg(target_os = "linux")]
fn webkit_process_pids() -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy();
        let Ok(pid) = fname.parse::<u32>() else {
            continue;
        };
        let status_path = format!("/proc/{pid}/status");
        if let Ok(status) = std::fs::read_to_string(&status_path) {
            // Match the exact tab-separated Name field as written by the kernel.
            if status.lines().any(|l| l == "Name:\tWebKitWebProcess") {
                pids.push(pid);
            }
        }
    }
    pids
}

#[cfg(not(target_os = "linux"))]
fn webkit_process_pids() -> Vec<u32> {
    Vec::new()
}

/// Read `VmRSS` in kibibytes from `/proc/<pid>/status`. Returns `None` if the
/// process is already gone or the field is absent.
#[cfg(target_os = "linux")]
fn read_vm_rss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // Field format: "VmRSS:\t<value> kB"
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_vm_rss_kb(_pid: u32) -> Option<u64> {
    None
}

/// Poll `/proc` for up to `poll_secs` seconds for NEW `WebKitWebProcess` PIDs
/// not present in `pids_before`. Returns the summed VmRSS of any found, or
/// `None` if none appeared within the window.
async fn sample_new_webkit_rss(pids_before: &[u32], poll_secs: u64) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(poll_secs);
    while Instant::now() < deadline {
        let pids_now = webkit_process_pids();
        let new_pids: Vec<u32> = pids_now
            .iter()
            .filter(|p| !pids_before.contains(p))
            .copied()
            .collect();
        if !new_pids.is_empty() {
            // Wait a further 500 ms for the process to settle its RSS before
            // reading — newly-started processes have low initial RSS that rises
            // as page resources are loaded.
            tokio::time::sleep(Duration::from_millis(500)).await;
            let total: u64 = new_pids
                .iter()
                .filter_map(|p| read_vm_rss_kb(*p))
                .sum();
            return if total > 0 { Some(total) } else { None };
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

// ── Config measurement helpers ───────────────────────────────────────────────

/// The JS snippet eval'd into thin/full windows to fire the first-paint signal.
/// Checks for `window.__windowCostPing` installed by `lib/dev/window-cost.ts`.
const PING_JS: &str =
    "if (typeof window.__windowCostPing === 'function') { window.__windowCostPing(); }";

/// Poll `eval(PING_JS)` into `window` until `rx` fires or `timeout_secs` elapses.
/// Returns `Some(elapsed_ms_from_t0)` on first ping; `None` on timeout.
///
/// Polling cadence: 200 ms. Each eval is fire-and-forget; `window_cost_ping`
/// sets the oneshot on the first successful call.
async fn poll_first_paint(
    window: &tauri::WebviewWindow,
    cost_state: &WindowCostStateRef,
    t0: Instant,
    timeout_secs: u64,
) -> Option<u64> {
    let (tx, mut rx) = oneshot::channel::<Instant>();
    {
        *cost_state
            .pending
            .lock()
            .expect("window_cost pending poisoned") = Some(tx);
    }

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if Instant::now() >= deadline {
            // Timeout: drain the pending slot so stale pings don't affect the
            // next config's measurement.
            cost_state
                .pending
                .lock()
                .expect("window_cost pending poisoned")
                .take();
            return None;
        }
        // Eval is fire-and-forget; errors are benign (page not yet ready).
        let _ = window.eval(PING_JS);
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Non-blocking check: did the ping land?
        match rx.try_recv() {
            Ok(t1) => return Some(t1.saturating_duration_since(t0).as_millis() as u64),
            Err(oneshot::error::TryRecvError::Empty) => continue,
            Err(oneshot::error::TryRecvError::Closed) => return None,
        }
    }
}

// ── Main command ─────────────────────────────────────────────────────────────

/// Run the three-config cost measurement sequence and return a [`WindowCostReport`].
///
/// **Caution**: this command is async and takes up to ~45 seconds in total
/// (3 configs × up to 15 s first-paint timeout). Run from DevTools, not from
/// the UI — there is no progress feedback.
#[tauri::command]
pub async fn window_cost_run(
    app: AppHandle,
    registry: State<'_, WindowRegistry>,
    cost_state: State<'_, WindowCostStateRef>,
) -> Result<WindowCostReport, String> {
    let cost_arc: WindowCostStateRef = cost_state.inner().clone();

    // ── Config (a): empty ────────────────────────────────────────────────────
    let row_a = {
        let pids_before = webkit_process_pids();
        let t0 = Instant::now();

        // `about:blank` is a valid URL per RFC 6694 and is accepted by all
        // three WebKit backends. No Tauri JS bridge loads → no first-paint
        // signal. We measure spawn_ms and renderer RSS only.
        let spawn_result = (|| -> Result<tauri::WebviewWindow, String> {
            let url = url::Url::parse("about:blank").map_err(|e| e.to_string())?;
            WebviewWindowBuilder::new(&app, "wpcost-a", WebviewUrl::External(url))
                .title("Ikenga — cost probe (empty)")
                .inner_size(800.0, 600.0)
                .visible(false) // keep off-screen; we're measuring cost, not UX
                .build()
                .map_err(|e| e.to_string())
        })();

        let spawn_ms = t0.elapsed().as_millis() as u64;

        match spawn_result {
            Ok(window) => {
                // Wait briefly for the renderer process to appear.
                let rss_kb = sample_new_webkit_rss(&pids_before, 3).await;
                let _ = window.close();
                // Allow the OS to fully reclaim the window before the next config.
                tokio::time::sleep(Duration::from_millis(800)).await;
                WindowCostRow {
                    config: "empty",
                    spawn_ms,
                    first_paint_ms: None,
                    rss_kb,
                    rss_note: os_rss_note(),
                    error: None,
                }
            }
            Err(e) => {
                tokio::time::sleep(Duration::from_millis(400)).await;
                WindowCostRow {
                    config: "empty",
                    spawn_ms,
                    first_paint_ms: None,
                    rss_kb: None,
                    rss_note: os_rss_note(),
                    error: Some(e),
                }
            }
        }
    };

    // ── Config (b): thin (single-surface) ────────────────────────────────────
    let row_b = {
        let pids_before = webkit_process_pids();
        let t0 = Instant::now();

        let desc = WindowDescriptor {
            label: "wpcost-b".to_string(),
            kind: WindowKind::SingleSurface,
            // "chat" is the canonical first Flavor-C surface (WP-06); using it
            // here ensures the URL params match what WP-05 will honour.
            surface_set: vec!["chat".to_string()],
            project_id: None,
            layout_key: "wpcost-b".to_string(),
        };
        let spawn_result = registry
            .spawn(&app, desc)
            .map_err(|e| e.to_string())
            .and_then(|label| {
                app.get_webview_window(&label)
                    .ok_or_else(|| "window vanished after spawn".to_string())
            });

        let spawn_ms = t0.elapsed().as_millis() as u64;

        match spawn_result {
            Ok(window) => {
                let first_paint_ms =
                    poll_first_paint(&window, &cost_arc, t0, 15).await;
                let rss_kb = sample_new_webkit_rss(&pids_before, 3).await;
                let _ = registry.close(&app, "wpcost-b");
                tokio::time::sleep(Duration::from_millis(800)).await;
                WindowCostRow {
                    config: "thin",
                    spawn_ms,
                    first_paint_ms,
                    rss_kb,
                    rss_note: os_rss_note(),
                    error: None,
                }
            }
            Err(e) => {
                tokio::time::sleep(Duration::from_millis(400)).await;
                WindowCostRow {
                    config: "thin",
                    spawn_ms,
                    first_paint_ms: None,
                    rss_kb: None,
                    rss_note: os_rss_note(),
                    error: Some(e),
                }
            }
        }
    };

    // ── Config (c): full (workspace = all surfaces) ───────────────────────────
    let row_c = {
        let pids_before = webkit_process_pids();
        let t0 = Instant::now();

        let desc = WindowDescriptor {
            label: "wpcost-c".to_string(),
            // `Workspace` = full second workspace window; WP-05 will render all
            // surfaces into it. Gives the "full shell" cost baseline for Flavor B.
            kind: WindowKind::Workspace,
            surface_set: vec![], // empty = all surfaces (WP-05 interprets this)
            project_id: None,
            layout_key: "wpcost-c".to_string(),
        };
        let spawn_result = registry
            .spawn(&app, desc)
            .map_err(|e| e.to_string())
            .and_then(|label| {
                app.get_webview_window(&label)
                    .ok_or_else(|| "window vanished after spawn".to_string())
            });

        let spawn_ms = t0.elapsed().as_millis() as u64;

        match spawn_result {
            Ok(window) => {
                let first_paint_ms =
                    poll_first_paint(&window, &cost_arc, t0, 15).await;
                let rss_kb = sample_new_webkit_rss(&pids_before, 3).await;
                let _ = registry.close(&app, "wpcost-c");
                tokio::time::sleep(Duration::from_millis(800)).await;
                WindowCostRow {
                    config: "full",
                    spawn_ms,
                    first_paint_ms,
                    rss_kb,
                    rss_note: os_rss_note(),
                    error: None,
                }
            }
            Err(e) => {
                tokio::time::sleep(Duration::from_millis(400)).await;
                WindowCostRow {
                    config: "full",
                    spawn_ms,
                    first_paint_ms: None,
                    rss_kb: None,
                    rss_note: os_rss_note(),
                    error: Some(e),
                }
            }
        }
    };

    Ok(WindowCostReport {
        rows: vec![row_a, row_b, row_c],
        os: os_name(),
        os_rss_note: os_rss_note(),
    })
}
