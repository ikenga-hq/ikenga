//! HTTP handlers for `/iyke/browser/*` — the shell-side bridge that the
//! pkg-browser MCP server (and any other localhost client with a bearer
//! token) calls into.
//!
//! Pane addressing across this boundary is **kernel-shaped**:
//! `(pkg_id, pane_id)`, where `pkg_id` is the calling pkg (e.g.
//! `com.ikenga.mcp-browser`) and `pane_id` is whatever string the caller
//! chose (`b1`, `b2`, etc. for the pkg-browser MCP — but the shell
//! treats it as an opaque key). The kernel's webview capability check
//! happens inside `WebviewPanesRegistry::create`, so callers don't get
//! to spin up child webviews for pkgs that haven't declared
//! `capabilities.webview.child_webviews = true`.
//!
//! Three flavors of handler:
//! - **kernel-direct**: open / close / list / goto — just call the
//!   matching `WebviewPanesRegistry` method.
//! - **eval-only (fire-and-forget)**: back / forward / reload — eval a
//!   one-line JS command into the child webview.
//! - **eval + reply RPC**: snapshot / read_text / click / fill / select /
//!   press_key / wait_for / eval — eval into the child webview a closure
//!   that runs `window.__ikengaPkgBrowser.<helper>(...)` and POSTs the
//!   result back to `/iyke/browser/_reply` via `BrowserRpc::request`.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Json as JsonBody, Query},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pkg::webview::{emit_if_origin_blocked, PaneRect, WebviewPanesRegistry};

use super::browser_rpc::{BrowserRpc, ReplyAck, ReplyEnvelope};
use super::playwright_proxy::PlaywrightProxy;

/// Hop to the GTK / NSApplication main thread to invoke a `WebviewPanesRegistry`
/// method that touches Tauri builder APIs. The kernel uses synchronous
/// `WebviewWindowBuilder::build()` / `Webview::eval()` internally; those post
/// to the main loop and block until it pumps. Calling them from an axum-tokio
/// worker without an explicit hop hangs on Linux WebKitGTK because the main
/// loop never gets a tick while our task is blocked.
async fn on_main<R, F>(app: &tauri::AppHandle, f: F) -> Result<R, (StatusCode, String)>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(f());
    })
    .map_err(|e| err500(format!("run_on_main_thread: {e}")))?;
    rx.await
        .map_err(|e| err500(format!("main-thread channel closed: {e}")))
}

const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const INTERACTION_TIMEOUT: Duration = Duration::from_secs(5);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const WAIT_FOR_DEFAULT_MS: u64 = 10_000;
const WAIT_FOR_MAX_MS: u64 = 60_000;
const EVAL_TIMEOUT: Duration = Duration::from_secs(30);

/// Carries the live Iyke port to handlers that need to bake it into eval
/// closures (so the in-page reply fetch knows where to POST).
#[derive(Clone, Copy)]
pub struct BrowserPort(pub u16);

// ── Common body shapes ───────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct PaneKey {
    pub pkg_id: String,
    pub pane_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PaneKeyQuery {
    #[serde(default)]
    pub pkg_id: Option<String>,
}

#[derive(Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

fn ok() -> Json<OkResponse> {
    Json(OkResponse { ok: true })
}

fn err500<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

fn err400<S: Into<String>>(msg: S) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

// ── open / close / list ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct OpenBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub url: String,
    #[serde(default)]
    pub partition: Option<String>,
    /// Engine backing this pane. `None`/absent → webkit (contract default).
    /// `"chrome"` routes to the Playwright sidecar.
    #[serde(default)]
    pub engine: Option<String>,
    /// Playwright mode when `engine="chrome"`: `"managed"` (default — launches a
    /// dedicated-profile Chrome) or `"attach"` (drives the user's everyday Chrome
    /// over CDP). Forwarded to the sidecar; ignored on the webkit path.
    #[serde(default)]
    pub mode: Option<String>,
    /// Initial pane rect. Required for webkit (in-shell child webview); ignored
    /// for chrome (Managed Chrome owns its own OS window — G-05), hence optional.
    #[serde(default)]
    pub rect: Option<PaneRect>,
    /// Attach-mode target selection (`engine="chrome"` + `mode="attach"` only):
    /// `"new"` (default) opens a fresh tab without disturbing existing ones,
    /// `"active"` adopts the first open tab (the pre-WP behavior), or a specific
    /// CDP `<targetId>` adopts that page. Forwarded verbatim to the sidecar;
    /// ignored on the webkit path and in managed mode.
    #[serde(default)]
    pub attach_target: Option<String>,
    /// Managed-mode window visibility (`engine="chrome"` + `mode="managed"`):
    /// omit/`false` → HEADFUL (visible, for human review/login — the default);
    /// `true` → headless (autonomous). Forwarded to the sidecar; ignored on the
    /// webkit + attach paths.
    #[serde(default)]
    pub headless: Option<bool>,
}

#[derive(Serialize)]
pub struct OpenResult {
    pub pkg_id: String,
    pub pane_id: String,
    pub webview_label: String,
    pub partition: String,
    /// The resolved engine backing this pane, echoed back.
    pub engine: &'static str,
}

pub async fn post_browser_open(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<OpenBody>,
) -> Result<Json<OpenResult>, (StatusCode, String)> {
    // Engine is decided here at open and remembered (chrome panes are tracked by
    // the Playwright proxy; absence there means webkit). Subsequent verbs
    // dispatch off that recorded engine.
    if body.engine.as_deref() == Some("chrome") {
        // Managed Chrome owns its own OS window — `rect` is ignored (G-05).
        // The managed-profile name doubles as the partition.
        let partition = body.partition.clone().unwrap_or_else(|| "default".to_string());
        // Forward the open verb to the Playwright sidecar verbatim, then record
        // the pane locally so later verbs route to the proxy.
        proxy
            .proxy_post("/iyke/browser/open", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        proxy.track(&body.pkg_id, &body.pane_id).await;
        return Ok(Json(OpenResult {
            pkg_id: body.pkg_id,
            pane_id: body.pane_id,
            // No shell webview label for a Managed-Chrome OS window.
            webview_label: String::new(),
            partition,
            engine: "chrome",
        }));
    }

    // WebKit (default) — unchanged kernel-direct path; rect is required.
    let rect = body
        .rect
        .ok_or_else(|| err400("rect required for webkit panes"))?;
    let app_for_main = app.clone();
    let panes_for_main = panes.clone();
    let pkg_id = body.pkg_id.clone();
    let pane_id = body.pane_id.clone();
    let url = body.url.clone();
    let partition_opt = body.partition.clone();
    let webview_label = on_main(&app, move || {
        panes_for_main.create(
            &app_for_main,
            &pkg_id,
            &pane_id,
            &url,
            rect,
            partition_opt.as_deref(),
            // Agent-driven (iyke bridge) panes parent to the PRIMARY window —
            // the bridge is app-level, not window-scoped yet (multi-window:
            // WP-04; WP-05 will route to a target window).
            "main",
        )
    })
    .await?
    .map_err(err500)?;
    Ok(Json(OpenResult {
        pkg_id: body.pkg_id,
        pane_id: body.pane_id,
        webview_label,
        partition: body.partition.unwrap_or_else(|| "default".to_string()),
        engine: "webkit",
    }))
}

pub async fn post_browser_close(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        // Forward the close to the sidecar (which owns the Playwright session +
        // OS window teardown), then drop the local pane record.
        proxy
            .proxy_post("/iyke/browser/close", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        proxy.untrack(&body.pkg_id, &body.pane_id).await;
        return Ok(ok());
    }
    let panes_for_main = panes.clone();
    on_main(&app, move || {
        panes_for_main.destroy(&body.pkg_id, &body.pane_id)
    })
    .await?
    .map_err(err500)?;
    Ok(ok())
}

pub async fn get_browser_list(
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Query(q): Query<PaneKeyQuery>,
) -> Json<Value> {
    let mut out = serde_json::Map::new();
    let webkit: Vec<Value> = panes
        .statuses()
        .into_iter()
        .filter(|s| q.pkg_id.as_ref().is_none_or(|p| p == &s.pkg_id))
        .map(|s| {
            serde_json::json!({
                "pkg_id": s.pkg_id,
                "pane_id": s.pane_id,
                "webview_label": s.webview_label,
                "current_url": s.current_url,
                "partition": s.partition,
                "surface_kind": s.surface_kind,
                "paused": s.paused,
                "engine": "webkit",
                "process_state": Value::Null,
                "stored_rect": serde_json::to_value(&s.stored_rect).unwrap_or(Value::Null),
            })
        })
        .collect();

    let mut entries = webkit;

    // Chrome panes are owned by the Playwright sidecar; ask it for its rows and
    // splice them into the unified list (same wire shape it already emits). The
    // sidecar applies the `pkg_id` filter itself. Best-effort: if the sidecar is
    // unreachable (never spawned, no chrome panes opened), return webkit-only.
    let query = q
        .pkg_id
        .as_deref()
        .map(|p| format!("pkg_id={p}"))
        .unwrap_or_default();
    if let Ok(Value::Object(mut sidecar)) = proxy.proxy_get("/iyke/browser/list", &query).await {
        if let Some(Value::Array(rows)) = sidecar.remove("panes") {
            entries.extend(rows);
        }
    }

    out.insert("panes".into(), Value::Array(entries));
    Json(Value::Object(out))
}

// ── chrome-only global queries (profiles / targets / launch_profile) ─────────
//
// These three verbs are chrome-engine-only and need NO pane — they query OS
// Chrome state, not a Playwright session. The shell just forwards to the
// sidecar (which owns the cross-OS Local-State read, the CDP `/json` probe, and
// the Chrome spawn). The proxy lazy-spawns the sidecar on first call, so the
// FE picker can open before any chrome pane exists.

/// `GET /iyke/browser/profiles` → `{ profiles: [{ dir, name, running }] }`.
/// OS Chrome profiles from the user-data-dir's `Local State` — distinct from
/// the Ikenga named partitions under `/iyke/browser/session/*`.
pub async fn get_browser_profiles(
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let v = proxy
        .proxy_get("/iyke/browser/profiles", "")
        .await
        .map_err(err500)?;
    Ok(Json(v))
}

/// `GET /iyke/browser/targets` → `{ endpoint, targets: [{ targetId, title, url, kind }] }`.
/// Probes the attach CDP endpoint; `endpoint: null` + `targets: []` when no
/// debug Chrome is reachable (the FE uses that to show the launch hint). The
/// sidecar never throws here.
pub async fn get_browser_targets(
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let v = proxy
        .proxy_get("/iyke/browser/targets", "")
        .await
        .map_err(err500)?;
    Ok(Json(v))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LaunchProfileBody {
    pub dir: String,
    #[serde(default)]
    pub port: Option<u16>,
}

/// `POST /iyke/browser/launch_profile` → `{ ok, endpoint }`. Spawns the
/// installed Chrome for the chosen on-disk profile with a remote-debugging
/// port so it becomes attachable. The sidecar refuses (clear error) if that
/// profile is already running (singleton lock).
pub async fn post_browser_launch_profile(
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<LaunchProfileBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let v = proxy
        .proxy_post(
            "/iyke/browser/launch_profile",
            &serde_json::to_value(&body).map_err(err500)?,
        )
        .await
        .map_err(err500)?;
    Ok(Json(v))
}

// ── navigation ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct GotoBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub url: String,
}

pub async fn post_browser_goto(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<GotoBody>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/goto", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    let panes_for_main = panes.clone();
    let (pkg_id_evt, pane_id_evt) = (body.pkg_id.clone(), body.pane_id.clone());
    on_main(&app, move || {
        panes_for_main.navigate(&body.pkg_id, &body.pane_id, &body.url)
    })
    .await?
    .map_err(|e| {
        // WP-45: an agent-driven navigation the origin boundary rejected is
        // broadcast to the pane host too, so the pane can show `pkg-blocked`.
        emit_if_origin_blocked(&app, &pkg_id_evt, &pane_id_evt, &e);
        err500(e)
    })?;
    Ok(ok())
}

pub async fn post_browser_back(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/back", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    let panes_for_main = panes.clone();
    on_main(&app, move || {
        panes_for_main.eval(&body.pkg_id, &body.pane_id, "window.history.back()")
    })
    .await?
    .map_err(err500)?;
    Ok(ok())
}

pub async fn post_browser_forward(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/forward", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    let panes_for_main = panes.clone();
    on_main(&app, move || {
        panes_for_main.eval(&body.pkg_id, &body.pane_id, "window.history.forward()")
    })
    .await?
    .map_err(err500)?;
    Ok(ok())
}

pub async fn post_browser_reload(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/reload", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    let panes_for_main = panes.clone();
    on_main(&app, move || {
        panes_for_main.eval(&body.pkg_id, &body.pane_id, "window.location.reload()")
    })
    .await?
    .map_err(err500)?;
    Ok(ok())
}

// ── snapshot ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotBody {
    pub pkg_id: String,
    pub pane_id: String,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub all: bool,
}

pub async fn post_browser_snapshot(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<SnapshotBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        // The sidecar owns the snapshot + ref store and emits the same wire
        // shape ({ url, title, text, nodes, snapshotId, id }).
        let v = proxy
            .proxy_post("/iyke/browser/snapshot", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }

    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    let body_js = format!(
        "__ipb.snapshot({{ query: {q}, all: {all} }})",
        q = json::to_string(&body.query),
        all = body.all,
    );
    let payload = rpc
        .request(
            &app,
            &panes,
            port.0,
            &body.pkg_id,
            &body.pane_id,
            SNAPSHOT_TIMEOUT,
            &body_js,
        )
        .await
        .map_err(err500)?;
    let mut obj = match payload {
        Value::Object(m) => m,
        other => {
            return Err(err500(format!(
                "snapshot returned non-object payload: {other}"
            )))
        }
    };
    obj.insert("id".into(), Value::String(body.pane_id.clone()));
    Ok(Json(Value::Object(obj)))
}

// ── read_text ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ReadTextBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub r#ref: String,
}

pub async fn post_browser_read_text(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<ReadTextBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        // Forward to the sidecar; it decides whether read_text is supported for
        // its engine (and returns the same wire shape / error if not).
        let v = proxy
            .proxy_post("/iyke/browser/read-text", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    if !is_ref(&body.r#ref) {
        return Err(err400(format!("invalid ref: {:?}", body.r#ref)));
    }
    let body_js = format!("__ipb.readText({})", json::to_string(&body.r#ref));
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        INTERACTION_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

// ── screenshot (not yet implemented for child webviews) ──────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ScreenshotBody {
    pub pkg_id: String,
    pub pane_id: String,
    /// Override the default output path. Default:
    /// `<app_local_data>/screenshots/browser-<pkg>-<pane>-<ts>.png`.
    #[serde(default)]
    pub out_path: Option<String>,
}

pub async fn post_browser_screenshot(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<ScreenshotBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    // Chrome is the FIRST engine to satisfy /screenshot (WebKit child webviews
    // still 501 — the main-window screenshot command can't target them). The
    // sidecar returns `{ bytes, base64, width, height }`; the shell decodes +
    // writes to disk so the contract stays `{ path, width, height, bytes }`.
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/screenshot", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        let b64 = v
            .get("base64")
            .and_then(|x| x.as_str())
            .ok_or_else(|| err500("sidecar screenshot response missing base64"))?;
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| err500(format!("decode screenshot base64: {e}")))?;
        let out = screenshot_out_path(&app, &body)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| err500(format!("create screenshot dir: {e}")))?;
        }
        std::fs::write(&out, &png).map_err(|e| err500(format!("write screenshot: {e}")))?;
        return Ok(Json(serde_json::json!({
            "path": out.to_string_lossy(),
            "width": v.get("width").cloned().unwrap_or(Value::Null),
            "height": v.get("height").cloned().unwrap_or(Value::Null),
            "bytes": png.len(),
        })));
    }

    Err((
        StatusCode::NOT_IMPLEMENTED,
        "browser screenshot is only supported for engine=chrome — the WebKit child-webview \
         path is Phase 4+ (the existing screenshot command targets the main window/shell pane, \
         not pkg-owned child webviews)."
            .into(),
    ))
}

fn screenshot_out_path(
    app: &tauri::AppHandle,
    body: &ScreenshotBody,
) -> Result<std::path::PathBuf, (StatusCode, String)> {
    if let Some(p) = &body.out_path {
        return Ok(std::path::PathBuf::from(p));
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let slug = format!(
        "browser-{}-{}-{ts}.png",
        body.pkg_id.replace(['/', '.'], "_"),
        body.pane_id.replace(['/', '.'], "_"),
    );
    use tauri::Manager as _;
    let dir = app
        .path()
        .app_local_data_dir()
        .map(|d| d.join("screenshots"))
        .unwrap_or_else(|_| std::path::PathBuf::from("screenshots"));
    Ok(dir.join(slug))
}

// Suppress unused-warning for the timeout constant until the webkit path lands.
#[allow(dead_code)]
const _SCREENSHOT_TIMEOUT_HINT: Duration = SCREENSHOT_TIMEOUT;

// ── click / fill / select / press_key ───────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct ClickBody {
    pub pkg_id: String,
    pub pane_id: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

pub async fn post_browser_click(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<ClickBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/click", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    require_one_target(&body.r#ref, &body.selector, &body.text)?;
    if let Some(r) = &body.r#ref {
        if !is_ref(r) {
            return Err(err400(format!("invalid ref: {r:?}")));
        }
    }
    let spec = target_spec(&body.r#ref, &body.selector, &body.text);
    let body_js = format!("__ipb.click({spec})");
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        INTERACTION_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FillBody {
    pub pkg_id: String,
    pub pane_id: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    pub text: String,
    #[serde(default)]
    pub replace: bool,
}

pub async fn post_browser_fill(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<FillBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/fill", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    require_one_target(&body.r#ref, &body.selector, &None)?;
    if let Some(r) = &body.r#ref {
        if !is_ref(r) {
            return Err(err400(format!("invalid ref: {r:?}")));
        }
    }
    let spec = target_spec(&body.r#ref, &body.selector, &None);
    let body_js = format!(
        "__ipb.fill({spec}, {text}, {replace})",
        text = json::to_string(&body.text),
        replace = body.replace,
    );
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        INTERACTION_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SelectBody {
    pub pkg_id: String,
    pub pane_id: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    pub value: String,
}

pub async fn post_browser_select(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<SelectBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/select", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    require_one_target(&body.r#ref, &body.selector, &None)?;
    if let Some(r) = &body.r#ref {
        if !is_ref(r) {
            return Err(err400(format!("invalid ref: {r:?}")));
        }
    }
    let spec = target_spec(&body.r#ref, &body.selector, &None);
    let body_js = format!(
        "__ipb.select({spec}, {value})",
        value = json::to_string(&body.value),
    );
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        INTERACTION_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PressKeyBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub combo: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
}

pub async fn post_browser_press_key(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<PressKeyBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/press-key", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    if body.combo.trim().is_empty() {
        return Err(err400("combo must not be empty"));
    }
    if let Some(r) = &body.r#ref {
        if !is_ref(r) {
            return Err(err400(format!("invalid ref: {r:?}")));
        }
    }
    let spec = target_spec(&body.r#ref, &body.selector, &None);
    let body_js = format!(
        "__ipb.pressKey({spec}, {combo})",
        combo = json::to_string(&body.combo),
    );
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        INTERACTION_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

// ── wait_for ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct WaitForBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub kind: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

pub async fn post_browser_wait_for(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<WaitForBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if !matches!(
        body.kind.as_str(),
        "url" | "ref" | "text" | "gone-text" | "selector" | "gone-selector" | "idle"
    ) {
        return Err(err400(format!("unknown wait_for kind: {:?}", body.kind)));
    }

    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        // The sidecar owns the predicate mapping + timeout clamp and returns the
        // same `{ satisfied, elapsed_ms, message? }` wire shape.
        let v = proxy
            .proxy_post("/iyke/browser/wait-for", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }

    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    let timeout_ms = body
        .timeout_ms
        .unwrap_or(WAIT_FOR_DEFAULT_MS)
        .min(WAIT_FOR_MAX_MS);
    let value = body.value.unwrap_or_default();
    let body_js = format!(
        "__ipb.waitFor({kind}, {value}, {timeout_ms})",
        kind = json::to_string(&body.kind),
        value = json::to_string(&value),
        timeout_ms = timeout_ms,
    );
    // Allow the in-page wait to use the full timeout; give the eval round-trip a 2s slack.
    let rpc_timeout = Duration::from_millis(timeout_ms + 2_000);
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        rpc_timeout,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

// ── eval (escape hatch) ─────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct EvalBody {
    pub pkg_id: String,
    pub pane_id: String,
    pub script: String,
}

pub async fn post_browser_eval(
    Extension(app): Extension<tauri::AppHandle>,
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    Extension(rpc): Extension<BrowserRpc>,
    Extension(port): Extension<BrowserPort>,
    JsonBody(body): JsonBody<EvalBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        let v = proxy
            .proxy_post("/iyke/browser/eval", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(Json(v));
    }
    check_not_paused(&panes, &body.pkg_id, &body.pane_id)?;
    // The user script must evaluate to a JSON-serializable value (or a
    // Promise resolving to one). It runs inside the same closure that
    // calls `__ipb.sendReply` — anything thrown is surfaced as an error.
    let body_js = format!("(() => {{ {} }})()", body.script);
    rpc.request(
        &app,
        &panes,
        port.0,
        &body.pkg_id,
        &body.pane_id,
        EVAL_TIMEOUT,
        &body_js,
    )
    .await
    .map_err(err500)
    .map(Json)
}

// ── pause / resume (Phase 5) ─────────────────────────────────────────────────

pub async fn post_browser_pause(
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/pause", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    panes
        .set_paused(&body.pkg_id, &body.pane_id, true)
        .map_err(err500)?;
    Ok(ok())
}

pub async fn post_browser_resume(
    Extension(panes): Extension<Arc<WebviewPanesRegistry>>,
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        proxy
            .proxy_post("/iyke/browser/resume", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    panes
        .set_paused(&body.pkg_id, &body.pane_id, false)
        .map_err(err500)?;
    Ok(ok())
}

/// Guard helper: 409 Conflict when the pane is paused. Snapshot/interaction
/// handlers call this before the eval hop.
fn check_not_paused(
    panes: &WebviewPanesRegistry,
    pkg_id: &str,
    pane_id: &str,
) -> Result<(), (StatusCode, String)> {
    let paused = panes.is_paused(pkg_id, pane_id).map_err(err500)?;
    if paused {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "pane ({pkg_id}, {pane_id}) is paused — resume it via /iyke/browser/resume \
                 before sending snapshot/interaction calls"
            ),
        ));
    }
    Ok(())
}

// ── focus (no-op placeholder) ───────────────────────────────────────────────

pub async fn post_browser_focus(
    Extension(proxy): Extension<Arc<PlaywrightProxy>>,
    JsonBody(body): JsonBody<PaneKey>,
) -> Result<Json<OkResponse>, (StatusCode, String)> {
    if proxy.has_pane(&body.pkg_id, &body.pane_id).await {
        // G-05: chrome `/focus` → the sidecar brings the page to front.
        proxy
            .proxy_post("/iyke/browser/focus", &serde_json::to_value(&body).map_err(err500)?)
            .await
            .map_err(err500)?;
        return Ok(ok());
    }
    // WebKit: kernel doesn't expose focus today (macOS/Windows in-window
    // children and Linux borderless top-levels both need different OS-level
    // affordances). Returning ok preserves the tool surface; no-op for v1.
    Ok(ok())
}

// ── _reply (unauthed; oneshot_token-gated) ──────────────────────────────────

pub async fn post_browser_reply(
    Extension(rpc): Extension<BrowserRpc>,
    JsonBody(env): JsonBody<ReplyEnvelope>,
) -> Result<Json<ReplyAck>, (StatusCode, String)> {
    rpc.resolve(env)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    Ok(Json(ReplyAck { ok: true }))
}

// ── _reply (Tauri command path) ─────────────────────────────────────────────
//
// The HTTP reply route above works fine for clients that can talk plain
// HTTP, but the in-page reply from a child webview hitting an external
// origin (https://example.com etc.) is killed by browser network policy
// — mixed-content (HTTPS → HTTP localhost), CORS preflight, and (most
// importantly) Private Network Access — before the body reaches us. The
// canonical Tauri 2 way to round-trip a value back from a page on a
// remote origin is to call `window.__TAURI_INTERNALS__.invoke(...)` and
// let Tauri's own IPC carry the bytes. We expose `iyke_browser_reply`
// here for that path; the capability `pkg-browser-child.json` opens it
// up to webviews with labels matching `pkg-*` on any remote URL.
//
// Security: the command does no auth of its own. Spoofing is prevented
// by the same `oneshot_token` (122-bit UUID) check that gates the HTTP
// route — `BrowserRpc::resolve` validates it in constant time. The
// command is intentionally narrow: it accepts the reply envelope and
// nothing else.

#[tauri::command]
pub async fn iyke_browser_reply(
    rpc: tauri::State<'_, BrowserRpc>,
    request_id: String,
    oneshot_token: String,
    ok: bool,
    payload: Option<Value>,
    error: Option<String>,
) -> Result<ReplyAck, String> {
    let env = ReplyEnvelope {
        request_id,
        oneshot_token,
        ok,
        payload,
        error,
    };
    rpc.resolve(env).await.map_err(|e| format!("{e:#}"))?;
    Ok(ReplyAck { ok: true })
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn require_one_target(
    r#ref: &Option<String>,
    selector: &Option<String>,
    text: &Option<String>,
) -> Result<(), (StatusCode, String)> {
    let count = r#ref.is_some() as u8 + selector.is_some() as u8 + text.is_some() as u8;
    if count != 1 {
        return Err(err400(
            "must set exactly one of: ref, selector, text".to_string(),
        ));
    }
    Ok(())
}

fn target_spec(r#ref: &Option<String>, selector: &Option<String>, text: &Option<String>) -> String {
    let mut o = serde_json::Map::new();
    if let Some(r) = r#ref {
        o.insert("ref".into(), Value::String(r.clone()));
    }
    if let Some(s) = selector {
        o.insert("selector".into(), Value::String(s.clone()));
    }
    if let Some(t) = text {
        o.insert("text".into(), Value::String(t.clone()));
    }
    serde_json::to_string(&Value::Object(o)).unwrap_or_else(|_| "{}".to_string())
}

fn is_ref(s: &str) -> bool {
    if !s.starts_with('e') || s.len() < 2 {
        return false;
    }
    s[1..].chars().all(|c| c.is_ascii_digit())
}

// Re-export `serde_json` under `json` so the format!() body sites are succinct.
mod json {
    pub fn to_string<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_string(v).unwrap_or_else(|_| "null".to_string())
    }
}
