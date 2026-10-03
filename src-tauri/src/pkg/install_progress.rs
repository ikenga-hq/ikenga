//! Structured progress and cancellation for a registry pkg install.
//!
//! An install reports where it is as a sequence of stage events on
//! [`INSTALL_PROGRESS_EVENT`]: preparing, downloading (bytes / total when the
//! server sends a length), verifying integrity, extracting, installing
//! dependencies, registering, starting services, done. Each event carries
//! the install id the front end picked (so a multi-step dependency plan maps
//! onto one row), the pkg id of the step, a stage id, a short label, and an
//! optional percent.
//!
//! Cancellation is cooperative and only allowed before registering: the
//! pipeline checks the flag between stages (and between download chunks and
//! while npm runs), and once it has committed to registering, a cancel
//! request answers "too late" instead. A cancelled install fails with
//! [`CANCELLED_MESSAGE`] and runs the same cleanup as any other failure.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, Result};
use serde::Serialize;

/// The event name both transports subscribe to.
pub const INSTALL_PROGRESS_EVENT: &str = "pkg-install://progress";

/// The error text of a cancelled install. The front end matches on it.
pub const CANCELLED_MESSAGE: &str = "install cancelled";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStage {
    Resolving,
    Downloading,
    Verifying,
    Extracting,
    InstallingDeps,
    Registering,
    Starting,
    Done,
}

impl InstallStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Resolving => "Preparing",
            Self::Downloading => "Downloading",
            Self::Verifying => "Verifying integrity",
            Self::Extracting => "Extracting",
            Self::InstallingDeps => "Installing dependencies",
            Self::Registering => "Registering",
            Self::Starting => "Starting services",
            Self::Done => "Installed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProgressEvent {
    pub install_id: String,
    pub pkg_id: String,
    pub stage: InstallStage,
    pub label: String,
    /// 0–100 when the stage has a measurable size, else absent
    /// (an indeterminate bar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// A short extra line, e.g. "12 packages fetched".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// False once the install has committed to registering.
    pub cancellable: bool,
}

/// Where events go: the Tauri event bus in the app, a recorder in tests.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, ev: InstallProgressEvent);
}

/// Drops every event — for callers that want the pipeline without progress.
pub struct NoopSink;
impl ProgressSink for NoopSink {
    fn emit(&self, _ev: InstallProgressEvent) {}
}

/// Emits on the app's event bus.
pub struct AppEmitSink(pub tauri::AppHandle);
impl ProgressSink for AppEmitSink {
    fn emit(&self, ev: InstallProgressEvent) {
        use tauri::Emitter;
        if let Err(e) = self.0.emit(INSTALL_PROGRESS_EVENT, &ev) {
            log::debug!("[pkg_install] emit progress failed: {e}");
        }
    }
}

#[derive(Default)]
struct Flags {
    cancelled: bool,
    committed: bool,
}

#[derive(Default)]
struct CancelState {
    flags: Mutex<Flags>,
}

fn active() -> &'static Mutex<HashMap<String, Arc<CancelState>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<String, Arc<CancelState>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

/// The answer to a cancel request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    /// The install will stop at its next check and clean up.
    Requested,
    /// It is already registering; it will finish.
    TooLate,
    /// No install with that id is running.
    NotRunning,
}

/// Ask the install `install_id` to stop.
pub fn request_cancel(install_id: &str) -> CancelOutcome {
    let state = active()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(install_id)
        .cloned();
    let Some(state) = state else {
        return CancelOutcome::NotRunning;
    };
    let mut f = state.flags.lock().unwrap_or_else(|e| e.into_inner());
    if f.committed {
        return CancelOutcome::TooLate;
    }
    f.cancelled = true;
    CancelOutcome::Requested
}

pub fn cancelled_error() -> anyhow::Error {
    anyhow!(CANCELLED_MESSAGE)
}

struct Inner {
    install_id: String,
    pkg_id: String,
    sink: Arc<dyn ProgressSink>,
    state: Arc<CancelState>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut map = active().lock().unwrap_or_else(|e| e.into_inner());
        if map
            .get(&self.install_id)
            .is_some_and(|s| Arc::ptr_eq(s, &self.state))
        {
            map.remove(&self.install_id);
        }
    }
}

/// One install's progress + cancel handle. Cheap to clone; the install is
/// removed from the cancel table when the last clone drops.
#[derive(Clone)]
pub struct InstallReporter {
    inner: Arc<Inner>,
}

impl InstallReporter {
    pub fn new(install_id: impl Into<String>, pkg_id: impl Into<String>, sink: Arc<dyn ProgressSink>) -> Self {
        let install_id = install_id.into();
        let state = Arc::new(CancelState::default());
        active()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(install_id.clone(), state.clone());
        Self {
            inner: Arc::new(Inner { install_id, pkg_id: pkg_id.into(), sink, state }),
        }
    }

    pub fn pkg_id(&self) -> &str {
        &self.inner.pkg_id
    }

    fn flags(&self) -> std::sync::MutexGuard<'_, Flags> {
        self.inner.state.flags.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_cancelled(&self) -> bool {
        self.flags().cancelled
    }

    pub fn check_cancelled(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(cancelled_error())
        } else {
            Ok(())
        }
    }

    /// Pass the point of no return. False (and nothing changes) when a
    /// cancel already arrived.
    pub fn commit(&self) -> bool {
        let mut f = self.flags();
        if f.cancelled {
            return false;
        }
        f.committed = true;
        true
    }

    pub fn stage(&self, stage: InstallStage) {
        self.progress(stage, None, None, None, None);
    }

    pub fn progress(
        &self,
        stage: InstallStage,
        percent: Option<f64>,
        bytes: Option<u64>,
        total: Option<u64>,
        detail: Option<String>,
    ) {
        let cancellable = !self.flags().committed;
        self.inner.sink.emit(InstallProgressEvent {
            install_id: self.inner.install_id.clone(),
            pkg_id: self.inner.pkg_id.clone(),
            stage,
            label: stage.label().to_string(),
            percent: percent.map(|p| p.clamp(0.0, 100.0)),
            bytes,
            total,
            detail,
            cancellable,
        });
    }
}

/// The steps of a registry install. The real implementation lives in
/// `commands::pkg`; tests drive the pipeline with a fake.
pub(crate) trait InstallSteps {
    type Output;
    /// Clear leftovers and make the scratch dirs.
    fn prepare(&mut self) -> impl Future<Output = Result<()>> + Send;
    /// Stream the tarball to disk, reporting bytes on `rep`.
    fn download(&mut self, rep: &InstallReporter) -> impl Future<Output = Result<()>> + Send;
    /// Compare the downloaded bytes against the published integrity.
    fn verify(&mut self) -> impl Future<Output = Result<()>> + Send;
    /// Unpack, check the manifest, and move it into the install dir.
    fn extract(&mut self) -> impl Future<Output = Result<()>> + Send;
    /// npm dependencies, if the pkg needs any.
    fn install_deps(&mut self, rep: &InstallReporter) -> impl Future<Output = Result<()>> + Send;
    /// Hand the install dir to the kernel (this also starts its services).
    fn register(&mut self) -> impl Future<Output = Result<Self::Output>> + Send;
    /// Whether the pkg declares sidecars or long-lived MCP servers.
    fn has_services(&self) -> bool;
    /// Success: drop backups and scratch files.
    fn finish(&mut self) -> impl Future<Output = ()> + Send;
    /// Failure or cancel: remove anything partial and restore what was there.
    fn cleanup(&mut self) -> impl Future<Output = ()> + Send;
}

/// Run `steps` in order, reporting each stage and honouring cancel up to the
/// point of registering. On any failure `cleanup` runs before the error is
/// returned, so a failed install leaves no partial pkg directory behind.
pub(crate) async fn run_install<S: InstallSteps>(rep: &InstallReporter, steps: &mut S) -> Result<S::Output> {
    let result = async {
        rep.stage(InstallStage::Resolving);
        steps.prepare().await?;
        rep.check_cancelled()?;

        rep.stage(InstallStage::Downloading);
        steps.download(rep).await?;
        rep.check_cancelled()?;

        rep.stage(InstallStage::Verifying);
        steps.verify().await?;
        rep.check_cancelled()?;

        rep.stage(InstallStage::Extracting);
        steps.extract().await?;
        rep.check_cancelled()?;

        rep.stage(InstallStage::InstallingDeps);
        steps.install_deps(rep).await?;

        if !rep.commit() {
            return Err(cancelled_error());
        }
        rep.stage(InstallStage::Registering);
        let out = steps.register().await?;
        if steps.has_services() {
            rep.stage(InstallStage::Starting);
        }
        Ok(out)
    }
    .await;

    match result {
        Ok(out) => {
            steps.finish().await;
            rep.stage(InstallStage::Done);
            Ok(out)
        }
        Err(e) => {
            steps.cleanup().await;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<InstallProgressEvent>>);
    impl ProgressSink for Recorder {
        fn emit(&self, ev: InstallProgressEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }
    impl Recorder {
        fn stages(&self) -> Vec<InstallStage> {
            let mut out: Vec<InstallStage> = Vec::new();
            for e in self.0.lock().unwrap().iter() {
                if out.last() != Some(&e.stage) {
                    out.push(e.stage);
                }
            }
            out
        }
    }

    /// A fake install: records the calls, can fail or cancel at a step.
    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        fail_at: Option<&'static str>,
        cancel_at: Option<&'static str>,
        install_id: String,
        services: bool,
    }
    impl Fake {
        fn step(&mut self, name: &'static str) -> Result<()> {
            self.calls.push(name);
            if self.cancel_at == Some(name) {
                request_cancel(&self.install_id);
            }
            if self.fail_at == Some(name) {
                return Err(anyhow!("ENOSPC: no space left on device"));
            }
            Ok(())
        }
    }
    impl InstallSteps for Fake {
        type Output = &'static str;
        async fn prepare(&mut self) -> Result<()> {
            self.step("prepare")
        }
        async fn download(&mut self, rep: &InstallReporter) -> Result<()> {
            for (b, p) in [(512u64, 50.0), (1024, 100.0)] {
                rep.progress(InstallStage::Downloading, Some(p), Some(b), Some(1024), None);
            }
            self.step("download")
        }
        async fn verify(&mut self) -> Result<()> {
            self.step("verify")
        }
        async fn extract(&mut self) -> Result<()> {
            self.step("extract")
        }
        async fn install_deps(&mut self, _rep: &InstallReporter) -> Result<()> {
            self.step("install_deps")
        }
        async fn register(&mut self) -> Result<&'static str> {
            self.step("register")?;
            Ok("installed")
        }
        fn has_services(&self) -> bool {
            self.services
        }
        async fn finish(&mut self) {
            self.calls.push("finish");
        }
        async fn cleanup(&mut self) {
            self.calls.push("cleanup");
        }
    }

    fn reporter(id: &str) -> (InstallReporter, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        (InstallReporter::new(id, "com.test.pkg", rec.clone()), rec)
    }

    #[tokio::test]
    async fn emits_every_stage_in_order() {
        let (rep, rec) = reporter("order");
        let mut fake = Fake { services: true, install_id: "order".into(), ..Default::default() };
        assert_eq!(run_install(&rep, &mut fake).await.unwrap(), "installed");
        use InstallStage::*;
        assert_eq!(
            rec.stages(),
            vec![Resolving, Downloading, Verifying, Extracting, InstallingDeps, Registering, Starting, Done]
        );
        assert_eq!(
            fake.calls,
            vec!["prepare", "download", "verify", "extract", "install_deps", "register", "finish"]
        );
        let events = rec.0.lock().unwrap();
        let dl: Vec<_> = events.iter().filter_map(|e| e.percent).collect();
        assert_eq!(dl, vec![50.0, 100.0]);
        assert!(events.iter().all(|e| e.pkg_id == "com.test.pkg" && e.install_id == "order"));
        // Cancellable until registering, never after.
        let reg = events.iter().position(|e| e.stage == Registering).unwrap();
        assert!(events[..reg].iter().all(|e| e.cancellable));
        assert!(events[reg..].iter().all(|e| !e.cancellable));
    }

    #[tokio::test]
    async fn no_starting_stage_without_services() {
        let (rep, rec) = reporter("nosvc");
        let mut fake = Fake { install_id: "nosvc".into(), ..Default::default() };
        run_install(&rep, &mut fake).await.unwrap();
        assert!(!rec.stages().contains(&InstallStage::Starting));
        assert_eq!(rec.stages().last(), Some(&InstallStage::Done));
    }

    #[tokio::test]
    async fn a_failure_cleans_up_and_never_registers() {
        let (rep, rec) = reporter("fail");
        let mut fake = Fake { fail_at: Some("install_deps"), install_id: "fail".into(), ..Default::default() };
        let err = run_install(&rep, &mut fake).await.unwrap_err();
        assert!(format!("{err}").contains("ENOSPC"));
        assert_eq!(fake.calls.last(), Some(&"cleanup"));
        assert!(!fake.calls.contains(&"register"));
        assert!(!rec.stages().contains(&InstallStage::Registering));
        assert!(!rec.stages().contains(&InstallStage::Done));
    }

    #[tokio::test]
    async fn cancel_before_registering_stops_and_cleans_up() {
        let (rep, rec) = reporter("cancel");
        let mut fake = Fake { cancel_at: Some("download"), install_id: "cancel".into(), ..Default::default() };
        let err = run_install(&rep, &mut fake).await.unwrap_err();
        assert_eq!(format!("{err}"), CANCELLED_MESSAGE);
        assert_eq!(fake.calls, vec!["prepare", "download", "cleanup"]);
        assert_eq!(rec.stages().last(), Some(&InstallStage::Downloading));
    }

    #[tokio::test]
    async fn cancel_after_commit_is_too_late() {
        let (rep, _rec) = reporter("late");
        let mut fake = Fake { cancel_at: Some("register"), install_id: "late".into(), ..Default::default() };
        // The cancel issued during register is refused, so the install finishes.
        assert!(run_install(&rep, &mut fake).await.is_ok());
        assert!(fake.calls.contains(&"finish"));
    }

    #[test]
    fn cancel_outcomes() {
        assert_eq!(request_cancel("nobody"), CancelOutcome::NotRunning);
        let (rep, _rec) = reporter("outcomes");
        assert_eq!(request_cancel("outcomes"), CancelOutcome::Requested);
        assert!(rep.is_cancelled());
        assert!(!rep.commit());
        drop(rep);
        assert_eq!(request_cancel("outcomes"), CancelOutcome::NotRunning);

        let (rep, _rec) = reporter("committed");
        assert!(rep.commit());
        assert_eq!(request_cancel("committed"), CancelOutcome::TooLate);
    }
}
