//! Cron registry — schedules per-package recurring jobs.
//!
//! Each manifest cron entry is `{ id, expr, handler }`. `expr` is a 6-field
//! cron expression (sec min hour day month dow — tokio-cron-scheduler's
//! native shape). `handler` reuses the same parser as the iyke routes
//! registry today: `event:<name>` emits a Tauri event named `pkg://<name>`,
//! `sidecar:<name> <sub>` resolves the named sidecar via the
//! [`SidecarsRegistry`], spawns it with the given subcommand, and emits a
//! `pkg://cron/<pkg_id>/<cron_id>/result` event with captured stdout/stderr.
//!
//! Job-key shape: `<pkg_id>::<cron_id>`. We keep a `pkg_id → [(cron_id,
//! Uuid)]` map so uninstall can remove the right scheduler entries without
//! scanning everything. The scheduler itself is lazy-initialized on first
//! register so packages with no cron entries don't pay the startup cost.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::process::Command;
use tokio_cron_scheduler::{Job, JobScheduler};
use uuid::Uuid;

use crate::pkg::manifest::Package;
use crate::pkg::registries::SidecarsRegistry;
use crate::pkg::registry::Registry;
use crate::platform::NoConsoleWindow;

/// Hard cap on cron-fired sidecar runs. Pollers/sends are quick; if a job
/// blows past 10 minutes something is wrong and we'd rather kill it than
/// stack overlapping schedules.
const CRON_SIDECAR_TIMEOUT_SECS: u64 = 600;

/// One scheduled job, surfaced in the kernel snapshot. `job_uuid` is the
/// scheduler-internal id we use on remove.
#[derive(Debug, Clone)]
struct CronJob {
    pkg_id: String,
    cron_id: String,
    expr: String,
    handler: String,
    job_uuid: Uuid,
}

pub struct CronRegistry {
    app: AppHandle,
    /// Resolved at fire time so `sidecar:<name> <sub>` handlers can find
    /// their binary by name. Held as Arc so multiple closures share one
    /// registry without per-job clones of the index itself.
    sidecars: Arc<SidecarsRegistry>,
    /// `JobScheduler` is async; lazy-init under a tokio Mutex so multiple
    /// registers don't race the first construction.
    sched: tokio::sync::Mutex<Option<JobScheduler>>,
    jobs: RwLock<HashMap<String, Vec<CronJob>>>,
}

impl CronRegistry {
    pub fn new(app: AppHandle, sidecars: Arc<SidecarsRegistry>) -> Self {
        Self {
            app,
            sidecars,
            sched: tokio::sync::Mutex::new(None),
            jobs: RwLock::new(HashMap::new()),
        }
    }

    async fn ensure_sched(&self) -> Result<JobScheduler> {
        let mut guard = self.sched.lock().await;
        if let Some(s) = guard.as_ref() {
            return Ok(s.clone());
        }
        let s = JobScheduler::new()
            .await
            .map_err(|e| anyhow!("create job scheduler: {e}"))?;
        s.start()
            .await
            .map_err(|e| anyhow!("start job scheduler: {e}"))?;
        *guard = Some(s.clone());
        Ok(s)
    }

    fn parse_handler(spec: &str) -> Result<HandlerSpec> {
        let spec = spec.trim();
        if let Some(rest) = spec.strip_prefix("event:") {
            let name = rest.trim();
            if name.is_empty() {
                return Err(anyhow!("event handler missing name"));
            }
            return Ok(HandlerSpec::Event(name.to_string()));
        }
        if let Some(rest) = spec.strip_prefix("sidecar:") {
            let mut parts = rest.trim().splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("").trim();
            let sub = parts.next().unwrap_or("").trim();
            if name.is_empty() || sub.is_empty() {
                return Err(anyhow!(
                    "sidecar handler must be `sidecar:<name> <subcommand>`"
                ));
            }
            return Ok(HandlerSpec::Sidecar {
                name: name.to_string(),
                subcommand: sub.to_string(),
            });
        }
        Err(anyhow!(
            "unknown cron handler `{spec}` (use `event:<name>` or `sidecar:<name> <sub>`)"
        ))
    }
}

enum HandlerSpec {
    Event(String),
    Sidecar { name: String, subcommand: String },
}

impl Registry for CronRegistry {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn register(&self, pkg: &Package) -> Result<()> {
        if pkg.manifest.cron.is_empty() {
            return Ok(());
        }

        // Idempotent: drop any existing jobs for this pkg first so a re-
        // register (boot replay) doesn't stack scheduler entries.
        self.unregister(&pkg.manifest.id).ok();

        // Validate everything before touching the scheduler.
        let mut parsed: Vec<(String, String, HandlerSpec)> = Vec::new();
        for entry in &pkg.manifest.cron {
            let h = Self::parse_handler(&entry.handler)
                .map_err(|e| anyhow!("cron `{}/{}`: {e}", pkg.manifest.id, entry.id))?;
            parsed.push((entry.id.clone(), entry.expr.clone(), h));
        }

        let pkg_id = pkg.manifest.id.clone();
        let install_path = pkg.install_path.clone();
        let app = self.app.clone();
        let sidecars = self.sidecars.clone();
        let mut new_jobs: Vec<CronJob> = Vec::new();

        // Scheduler ops are async; do them all in one block_on.
        let sched_jobs: Vec<(String, String, String, Uuid)> =
            tauri::async_runtime::block_on(async move {
                let sched = self.ensure_sched().await?;
                let mut out: Vec<(String, String, String, Uuid)> = Vec::new();
                for (cron_id, expr, handler) in parsed {
                    let app_inner = app.clone();
                    let pkg_inner = pkg_id.clone();
                    let cron_inner = cron_id.clone();
                    let handler_for_log = match &handler {
                        HandlerSpec::Event(n) => format!("event:{n}"),
                        HandlerSpec::Sidecar { name, subcommand } => {
                            format!("sidecar:{name} {subcommand}")
                        }
                    };
                    let job = match handler {
                        HandlerSpec::Event(name) => Job::new(expr.as_str(), move |_uuid, _l| {
                            let event = format!("pkg://{name}");
                            let payload = json!({
                                "pkg_id": pkg_inner,
                                "cron_id": cron_inner,
                            });
                            if let Err(e) = app_inner.emit(&event, payload) {
                                log::warn!("[pkg.cron] emit `{event}` failed: {e}");
                            }
                        })
                        .map_err(|e| anyhow!("build cron job `{cron_id}` (`{expr}`): {e}"))?,
                        HandlerSpec::Sidecar { name, subcommand } => {
                            // Capture once, clone per fire — the closure has to be
                            // FnMut + Send, so all captured state must be owned
                            // and Send-safe.
                            let sidecars_for_job = sidecars.clone();
                            let install_path_for_job = install_path.clone();
                            Job::new(expr.as_str(), move |_uuid, _l| {
                                let app2 = app_inner.clone();
                                let pkg2 = pkg_inner.clone();
                                let cron2 = cron_inner.clone();
                                let name2 = name.clone();
                                let sub2 = subcommand.clone();
                                let sidecars2 = sidecars_for_job.clone();
                                let install_path2 = install_path_for_job.clone();
                                // tokio-cron-scheduler hands us a sync closure;
                                // do the spawn-and-wait work on the runtime so
                                // it doesn't block scheduler ticks.
                                tauri::async_runtime::spawn(async move {
                                    run_sidecar_cron(
                                        app2,
                                        pkg2,
                                        cron2,
                                        name2,
                                        sub2,
                                        sidecars2,
                                        install_path2,
                                    )
                                    .await;
                                });
                            })
                            .map_err(|e| anyhow!("build cron job `{cron_id}` (`{expr}`): {e}"))?
                        }
                    };
                    let job_uuid = sched
                        .add(job)
                        .await
                        .map_err(|e| anyhow!("schedule cron `{cron_id}`: {e}"))?;
                    out.push((cron_id, expr, handler_for_log, job_uuid));
                }
                Ok::<_, anyhow::Error>(out)
            })?;

        for (cron_id, expr, handler, job_uuid) in sched_jobs {
            new_jobs.push(CronJob {
                pkg_id: pkg.manifest.id.clone(),
                cron_id,
                expr,
                handler,
                job_uuid,
            });
        }

        let mut map = self
            .jobs
            .write()
            .map_err(|_| anyhow!("cron registry lock poisoned"))?;
        map.insert(pkg.manifest.id.clone(), new_jobs);
        Ok(())
    }

    fn unregister(&self, pkg_id: &str) -> Result<()> {
        let to_remove: Vec<Uuid> = {
            let mut map = self
                .jobs
                .write()
                .map_err(|_| anyhow!("cron registry lock poisoned"))?;
            map.remove(pkg_id)
                .map(|v| v.into_iter().map(|j| j.job_uuid).collect())
                .unwrap_or_default()
        };
        if to_remove.is_empty() {
            return Ok(());
        }
        let _ = tauri::async_runtime::block_on(async move {
            // Only acquire the scheduler if it was already initialized; if not,
            // there are no jobs to remove anyway.
            let sched = {
                let guard = self.sched.lock().await;
                guard.clone()
            };
            if let Some(s) = sched {
                for uuid in to_remove {
                    if let Err(e) = s.remove(&uuid).await {
                        log::warn!("[pkg.cron] remove job {uuid} failed: {e}");
                    }
                }
            }
            Ok::<_, anyhow::Error>(())
        });
        Ok(())
    }

    fn snapshot(&self) -> Value {
        let map = match self.jobs.read() {
            Ok(g) => g,
            Err(_) => return json!({ "error": "lock poisoned" }),
        };
        let entries: Vec<Value> = map
            .values()
            .flatten()
            .map(|j| {
                json!({
                    "pkg_id": j.pkg_id,
                    "cron_id": j.cron_id,
                    "expr": j.expr,
                    "handler": j.handler,
                    "job_uuid": j.job_uuid.to_string(),
                })
            })
            .collect();
        json!({ "count": entries.len(), "entries": entries })
    }
}

/// Cron-fired sidecar runner. Resolves the binary, spawns it with the
/// declared subcommand, captures stdout/stderr/exit, emits a result event,
/// and logs. Best-effort throughout — failures are reported via the event +
/// log; we never panic the scheduler thread.
///
/// Event shape: `pkg://cron/<pkg_id>/<cron_id>/result` with payload
/// `{ pkg_id, cron_id, sidecar_name, subcommand, ok, exit_code, stdout,
///   stderr, duration_ms, timed_out }`. Pkg iframes / observability UI
/// can subscribe to a wildcard.
#[allow(clippy::too_many_arguments)]
async fn run_sidecar_cron(
    app: AppHandle,
    pkg_id: String,
    cron_id: String,
    sidecar_name: String,
    subcommand: String,
    sidecars: Arc<SidecarsRegistry>,
    install_path: PathBuf,
) {
    let started = Instant::now();
    let event = format!("pkg://cron/{pkg_id}/{cron_id}/result");

    let entry = match sidecars.resolve(&sidecar_name) {
        Some(e) => e,
        None => {
            let msg = format!(
                "sidecar `{sidecar_name}` not registered (pkg `{pkg_id}` may not be installed)"
            );
            log::warn!("[pkg.cron] {msg}");
            let _ = app.emit(
                &event,
                json!({
                    "pkg_id": pkg_id,
                    "cron_id": cron_id,
                    "sidecar_name": sidecar_name,
                    "subcommand": subcommand,
                    "ok": false,
                    "error": msg,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "timed_out": false,
                }),
            );
            return;
        }
    };

    if entry.pkg_id != pkg_id {
        // Defense in depth: SidecarsRegistry already prevents cross-pkg
        // collisions at register time. If we got here, something is very
        // wrong — surface it loudly and bail.
        log::error!(
            "[pkg.cron] sidecar `{sidecar_name}` resolves to `{}` but cron belongs to `{pkg_id}` — refusing to run",
            entry.pkg_id
        );
        return;
    }

    log::info!(
        "[pkg.cron] firing `{pkg_id}::{cron_id}` → {} {subcommand}",
        entry.bin_path.display()
    );

    let mut cmd = Command::new(&entry.bin_path);
    cmd.arg(&subcommand);
    cmd.current_dir(&install_path);
    cmd.no_console_window();
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let timeout = std::time::Duration::from_secs(CRON_SIDECAR_TIMEOUT_SECS);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("spawn `{}`: {e}", entry.bin_path.display());
            log::warn!("[pkg.cron] {msg}");
            let _ = app.emit(
                &event,
                json!({
                    "pkg_id": pkg_id,
                    "cron_id": cron_id,
                    "sidecar_name": sidecar_name,
                    "subcommand": subcommand,
                    "ok": false,
                    "error": msg,
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "timed_out": false,
                }),
            );
            return;
        }
    };

    let (output, timed_out) = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => (Some(out), false),
        Ok(Err(e)) => {
            log::warn!("[pkg.cron] wait `{pkg_id}::{cron_id}`: {e}");
            (None, false)
        }
        Err(_) => {
            log::warn!(
                "[pkg.cron] `{pkg_id}::{cron_id}` timed out after {}s",
                timeout.as_secs()
            );
            (None, true)
        }
    };

    let payload = match output {
        Some(out) => json!({
            "pkg_id": pkg_id,
            "cron_id": cron_id,
            "sidecar_name": sidecar_name,
            "subcommand": subcommand,
            "ok": out.status.success(),
            "exit_code": out.status.code(),
            "stdout": String::from_utf8_lossy(&out.stdout).into_owned(),
            "stderr": String::from_utf8_lossy(&out.stderr).into_owned(),
            "duration_ms": started.elapsed().as_millis() as u64,
            "timed_out": false,
        }),
        None => json!({
            "pkg_id": pkg_id,
            "cron_id": cron_id,
            "sidecar_name": sidecar_name,
            "subcommand": subcommand,
            "ok": false,
            "error": if timed_out { "timed out" } else { "wait failed" },
            "duration_ms": started.elapsed().as_millis() as u64,
            "timed_out": timed_out,
        }),
    };
    if let Err(e) = app.emit(&event, payload) {
        log::warn!("[pkg.cron] emit `{event}` failed: {e}");
    }
}
