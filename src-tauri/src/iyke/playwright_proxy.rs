//! Reverse-proxy to the Playwright sidecar (ADR-018 / `plans/playwright-adoption`
//! WP-04 + A1). This is the replacement for the in-process chromiumoxide engine:
//! the shell no longer drives Chrome itself — it forwards the `engine=chrome`
//! (later `mode:attach|managed`) `/iyke/browser/*` requests to a long-lived Node
//! sidecar (`@ikenga/pkg-browser`) that owns the Playwright
//! sessions.
//!
//! Lifecycle: lazy-spawn `node <sidecar.js>` on the first chrome request, read
//! the `IKENGA_PW_READY {port}` line it prints, cache the port + child, and
//! reverse-proxy every browser verb to `http://127.0.0.1:{port}/iyke/browser/*`.
//! A small pane set records which `(pkg_id, pane_id)` are Playwright-backed so
//! the verb handlers route the same way the old `ChromeEngineRegistry::is_chrome`
//! did — without holding any browser state in-process.
//!
//! Sidecar resolution (A1, WP-A1.1): the proxy no longer carries a hardcoded
//! path. On first spawn it resolves the `dist/sidecar.js` entry by, in order,
//! (1) the `IKENGA_PW_SIDECAR` env override, (2) the installed pkg's
//! `install_path` from `pkg_installed` (`com.ikenga.sidecar-playwright-browser`),
//! (3) the in-workspace dev fallback. If none resolve, chrome verbs fail with a
//! precise install-offer error instead of hanging (WP-A1.4). If `node` itself is
//! missing, a precise Node-prerequisite error surfaces (WP-A1.5).
//!
//! This is the live forwarder behind `/iyke/browser/*` — every `engine=chrome`
//! verb in `browser_handlers` routes through `proxy_post`/`proxy_get`. The A1
//! lazy by-id resolution is the only delta over the original cutover.
#![allow(dead_code)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::commands::db::PaDb;
use crate::platform::NoConsoleWindow;

/// The installed pkg id whose `install_path` carries the prebuilt sidecar.
const SIDECAR_PKG_ID: &str = "com.ikenga.sidecar-playwright-browser";

/// In-workspace dev fallback (WP-A1.1 step 3): when the pkg isn't
/// registry-installed and no env override is set, fall back to the prebuilt
/// `dist/sidecar.js` in the monorepo so the `ikenga dev` loop keeps working.
const DEV_FALLBACK_ENTRY: &str = "/home/nedjamez/royalti-co/ikenga/ikenga-pkgs/packages/sidecars/playwright-browser/dist/sidecar.js";

/// User-facing message when no sidecar entry resolves (WP-A1.4).
/// NB: `ikenga add` is registry-gated and this pkg isn't in the signed Ọba
/// registry yet, so we point at the working npm + env path instead (see the
/// pkg README). Update to `ikenga add …` once the registry entry lands.
const NOT_INSTALLED_MSG: &str =
    "browser engine not installed — install it with: npm i -g @ikenga/pkg-browser \
     && export IKENGA_PW_SIDECAR=\"$(npm root -g)/@ikenga/pkg-browser/dist/sidecar.js\" \
     (needs Node + Google Chrome; see the @ikenga/pkg-browser README)";

/// User-facing message when `node` isn't on PATH (WP-A1.5).
const NODE_MISSING_MSG: &str =
    "Node.js is required for the browser engine; install Node and ensure it's on PATH";

pub struct PlaywrightProxy {
    inner: Mutex<Inner>,
    client: reqwest::Client,
    /// Handle to the shell DB so the sidecar entry can be resolved by pkg-id
    /// from `pkg_installed` on first spawn.
    pa_db: Arc<PaDb>,
}

struct Inner {
    port: Option<u16>,
    child: Option<Child>,
    /// Resolved + cached absolute path to `dist/sidecar.js`. Resolved once on
    /// first `ensure_port`; an in-place pkg update won't take until the proxy
    /// re-resolves (next boot / shutdown) — R-A1.2, low severity.
    sidecar_entry: Option<PathBuf>,
    /// `(pkg_id, pane_id)` pairs currently backed by the Playwright sidecar.
    panes: HashSet<(String, String)>,
}

impl PlaywrightProxy {
    pub fn new(pa_db: Arc<PaDb>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                port: None,
                child: None,
                sidecar_entry: None,
                panes: HashSet::new(),
            }),
            client: reqwest::Client::new(),
            pa_db,
        }
    }

    /// Resolve the absolute `dist/sidecar.js` entry, in priority order:
    /// (1) `IKENGA_PW_SIDECAR` env override, (2) installed-pkg `install_path`
    /// from `pkg_installed`, (3) in-workspace dev fallback. Returns the
    /// precise install-offer error (WP-A1.4) if none of them point at an
    /// existing file. Read-only — uses the reader pool.
    async fn resolve_entry(&self) -> Result<PathBuf> {
        // (1) Explicit override always wins — used by tests + the dev loop.
        if let Ok(p) = std::env::var("IKENGA_PW_SIDECAR") {
            if !p.trim().is_empty() {
                return Ok(PathBuf::from(p));
            }
        }

        // (2) Installed pkg: SELECT install_path FROM pkg_installed WHERE id=?
        // (mirrors the scan in pkg/kernel.rs). The sidecar entry is
        // `<install_path>/dist/sidecar.js`.
        match self.pa_db.ensure_reader_pool().await {
            Ok(pool) => {
                let row: Option<(String,)> =
                    sqlx::query_as("SELECT install_path FROM pkg_installed WHERE id = ?")
                        .bind(SIDECAR_PKG_ID)
                        .fetch_optional(&pool)
                        .await
                        .map_err(|e| anyhow!("read pkg_installed for {SIDECAR_PKG_ID}: {e}"))?;
                if let Some((install_path,)) = row {
                    let entry = PathBuf::from(install_path).join("dist").join("sidecar.js");
                    if entry.is_file() {
                        return Ok(entry);
                    }
                    // Row present but the dist file is missing → fall through to
                    // the dev fallback / not-installed error rather than spawn
                    // against a nonexistent path.
                    tracing::warn!(
                        "[playwright] {SIDECAR_PKG_ID} installed but {} missing",
                        entry.display()
                    );
                }
            }
            Err(e) => {
                // DB unavailable is not fatal to resolution — fall through to the
                // dev fallback, but record why the installed lookup was skipped.
                tracing::warn!("[playwright] pkg_installed lookup skipped (db: {e})");
            }
        }

        // (3) Dev fallback — in-workspace prebuilt dist.
        let dev = PathBuf::from(DEV_FALLBACK_ENTRY);
        if dev.is_file() {
            return Ok(dev);
        }

        // Nothing resolved → precise, actionable install-offer error (WP-A1.4).
        Err(anyhow!("{NOT_INSTALLED_MSG}"))
    }

    /// Spawn the sidecar if it isn't running yet and return its localhost port.
    async fn ensure_port(&self) -> Result<u16> {
        let mut g = self.inner.lock().await;
        if let Some(p) = g.port {
            return Ok(p);
        }

        // Resolve (and cache) the sidecar entry on first spawn (WP-A1.1).
        let entry = match g.sidecar_entry.clone() {
            Some(e) => e,
            None => {
                let e = self.resolve_entry().await?;
                g.sidecar_entry = Some(e.clone());
                e
            }
        };

        // Runtime = NODE, not Bun: Playwright's `connectOverCDP` (attach mode)
        // hangs on Bun's WebSocket transport; Node connects fine (managed mode
        // works on both). `entry` points at the prebuilt `dist/sidecar.js` (no
        // `tsx` at runtime). cwd is the pkg root (`dist/`'s parent) so node
        // resolves `playwright` from the pkg's `node_modules`.
        let pkg_dir = entry
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf());
        let mut cmd = Command::new("node");
        // Augment PATH the same way agent spawns do so an nvm-managed `node`
        // (invisible to the app's inherited GUI-launch PATH) still resolves
        // (WP-A1.5; ADR-013 §Addendum Decision 2).
        // NOTE: we no longer force IKENGA_PW_HEADLESS — managed mode defaults to
        // HEADFUL so a human can watch / log in / review (Need-1). Autonomous
        // callers opt into headless per-pane via the `headless` field on open.
        cmd.arg(&entry)
            .env("PATH", crate::runtime::augmented_path())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .no_console_window();
        if let Some(dir) = &pkg_dir {
            cmd.current_dir(dir);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // `node` not on PATH → precise prerequisite error (WP-A1.5).
                return Err(anyhow!("{NODE_MISSING_MSG}"));
            }
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("spawn playwright sidecar: node {:?}", entry)));
            }
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("sidecar stdout not captured"))?;
        let mut lines = BufReader::new(stdout).lines();

        // Read until the sidecar announces its bound port.
        let mut found: Option<u16> = None;
        while let Some(line) = lines.next_line().await.context("read sidecar stdout")? {
            if let Some(rest) = line.strip_prefix("IKENGA_PW_READY ") {
                found = rest.trim().parse().ok();
                break;
            }
        }
        let port = found.ok_or_else(|| anyhow!("sidecar exited before announcing a port"))?;
        tracing::info!("[playwright] sidecar ready on 127.0.0.1:{port}");
        g.port = Some(port);
        g.child = Some(child);
        Ok(port)
    }

    /// Forward a `/iyke/browser/*` POST verb to the sidecar; return its JSON body.
    pub async fn proxy_post(&self, path: &str, body: &Value) -> Result<Value> {
        let port = self.ensure_port().await?;
        let resp = self
            .client
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("content-type", "application/json")
            .body(serde_json::to_string(body).context("serialize proxy body")?)
            .send()
            .await
            .with_context(|| format!("proxy POST {path} to playwright sidecar"))?;
        let status = resp.status();
        let text = resp.text().await.context("read sidecar response")?;
        if !status.is_success() {
            return Err(anyhow!("sidecar {path} returned {status}: {text}"));
        }
        serde_json::from_str(&text).context("parse sidecar response")
    }

    /// Forward a `/iyke/browser/list`-style GET verb.
    pub async fn proxy_get(&self, path: &str, query: &str) -> Result<Value> {
        let port = self.ensure_port().await?;
        let resp = self
            .client
            .get(format!("http://127.0.0.1:{port}{path}?{query}"))
            .send()
            .await
            .with_context(|| format!("proxy GET {path} to playwright sidecar"))?;
        let status = resp.status();
        let text = resp.text().await.context("read sidecar response")?;
        if !status.is_success() {
            return Err(anyhow!("sidecar {path} returned {status}: {text}"));
        }
        serde_json::from_str(&text).context("parse sidecar response")
    }

    pub async fn track(&self, pkg_id: &str, pane_id: &str) {
        self.inner
            .lock()
            .await
            .panes
            .insert((pkg_id.to_string(), pane_id.to_string()));
    }

    pub async fn untrack(&self, pkg_id: &str, pane_id: &str) {
        self.inner
            .lock()
            .await
            .panes
            .remove(&(pkg_id.to_string(), pane_id.to_string()));
    }

    /// True iff this pane is Playwright-backed (the old `is_chrome` role).
    pub async fn has_pane(&self, pkg_id: &str, pane_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .panes
            .contains(&(pkg_id.to_string(), pane_id.to_string()))
    }

    pub async fn shutdown(&self) {
        let mut g = self.inner.lock().await;
        if let Some(mut child) = g.child.take() {
            let _ = child.start_kill();
        }
        g.port = None;
        // Drop the cached entry so a pkg update is picked up on next spawn.
        g.sidecar_entry = None;
        g.panes.clear();
    }
}
