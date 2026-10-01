//! Per-principal children (G-PRINCIPAL §3 topology B, OD-13; G-ACCESS R-6
//! `<broker-child>`).
//!
//! The broker lazily launches one `ikenga-server --executor-tier t1
//! --principal-child` per principal, **through the T1 executor**
//! (`spawn_piped` with `SpawnSpec.principal`), so the child runs as that
//! principal's uid with every §9.2 check behind it. The transport is
//! loopback TCP (P-7): the child binds `127.0.0.1:0`, gets a random
//! per-child `IKENGA_AUTH_TOKEN`, and reports its bound port through its own
//! `<data>/daemon.json`.
//!
//! Idle reap (OD-13): a child with no proxied request for the idle timeout
//! (default 30 min) and no open proxied WebSocket is stopped (SIGTERM, so it
//! drains like any daemon), and respawned on the principal's next request.
//! The child also runs its own idle watcher with the same timeout, which
//! counts PTY sessions and open WebSockets (§5 row 13); whichever fires
//! first, the broker notices the exit and respawns.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rand::RngCore;

use crate::executor::t1::T1Executor;
use crate::executor::{PipedOpts, Principal, PrincipalId, SpawnSpec, StdioMode};
use crate::server::auth::BoxFuture;
use crate::server::operator::OperatorRoot;

/// OD-13's default.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long a launched child gets to report its port.
pub const READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Where and how to reach one running child.
#[derive(Clone, PartialEq, Eq)]
pub struct ChildEndpoint {
    pub addr: SocketAddr,
    /// The per-child bearer the broker presents (P-7). Never logged.
    pub token: Arc<str>,
}

impl std::fmt::Debug for ChildEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildEndpoint")
            .field("addr", &self.addr)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A launched child's process, as the registry manages it.
pub trait ChildProcess: Send {
    /// Whether it has exited (reaps it if so).
    fn has_exited(&mut self) -> bool;
    /// Ask it to shut down gracefully (SIGTERM).
    fn terminate(&mut self);
    /// Make sure it is gone (SIGKILL).
    fn kill(&mut self);
}

pub struct LaunchedChild {
    pub addr: SocketAddr,
    pub process: Box<dyn ChildProcess>,
}

/// Launches a principal's child. The real one is [`T1Launcher`]; tests use a
/// fake that serves in-process.
pub trait ChildLauncher: Send + Sync {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>>;
}

struct Live {
    endpoint: ChildEndpoint,
    process: Box<dyn ChildProcess>,
    last_used: Instant,
}

type Slot = Arc<tokio::sync::Mutex<Option<Live>>>;

/// The running children, one slot per principal. A slot's async mutex
/// serializes that principal's launches, so two concurrent first requests
/// start one child, not two (the second would fail its flock anyway, I-3).
pub struct Children {
    launcher: Arc<dyn ChildLauncher>,
    slots: Mutex<HashMap<PrincipalId, Slot>>,
    idle_timeout: Duration,
}

fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

impl Children {
    pub fn new(launcher: Arc<dyn ChildLauncher>, idle_timeout: Duration) -> Self {
        Self {
            launcher,
            slots: Mutex::new(HashMap::new()),
            idle_timeout,
        }
    }

    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    fn slot(&self, id: PrincipalId) -> Slot {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id)
            .or_default()
            .clone()
    }

    fn all_slots(&self) -> Vec<(PrincipalId, Slot)> {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, s)| (*id, s.clone()))
            .collect()
    }

    /// The principal's running child, launching one if there is none (or the
    /// last one exited). Marks it used.
    pub async fn endpoint(&self, principal: &Principal) -> anyhow::Result<ChildEndpoint> {
        let slot = self.slot(principal.id);
        let mut live = slot.lock().await;
        if let Some(l) = live.as_mut() {
            if !l.process.has_exited() {
                l.last_used = Instant::now();
                return Ok(l.endpoint.clone());
            }
            tracing::info!("principal child for {} exited; respawning", principal.id);
            *live = None;
        }
        let token = mint_token();
        let launched = self.launcher.launch(principal, &token).await?;
        tracing::info!(
            "principal child for {} listening on {}",
            principal.id,
            launched.addr
        );
        let endpoint = ChildEndpoint {
            addr: launched.addr,
            token: token.into(),
        };
        *live = Some(Live {
            endpoint: endpoint.clone(),
            process: launched.process,
            last_used: Instant::now(),
        });
        Ok(endpoint)
    }

    /// Mark the principal's child used (a proxied socket is still open).
    pub async fn touch(&self, id: PrincipalId) {
        if let Some(l) = self.slot(id).lock().await.as_mut() {
            l.last_used = Instant::now();
        }
    }

    /// The child at `endpoint` refused a connection: forget it if it is
    /// still the current one, so the next [`endpoint`](Self::endpoint)
    /// launches afresh.
    pub async fn invalidate(&self, id: PrincipalId, endpoint: &ChildEndpoint) {
        let slot = self.slot(id);
        let mut live = slot.lock().await;
        if live.as_ref().is_some_and(|l| &l.endpoint == endpoint) {
            if let Some(mut l) = live.take() {
                l.process.kill();
            }
        }
    }

    /// Stop the principal's child, if any (disable, §7.3).
    pub async fn stop(&self, id: PrincipalId) -> bool {
        let slot = self.slot(id);
        let mut live = slot.lock().await;
        match live.take() {
            Some(mut l) => {
                l.process.terminate();
                true
            }
            None => false,
        }
    }

    /// Stop every child (broker shutdown).
    pub async fn stop_all(&self) {
        for (_, slot) in self.all_slots() {
            if let Some(mut l) = slot.lock().await.take() {
                l.process.terminate();
            }
        }
    }

    /// Principals with a live (not exited) child.
    pub async fn running(&self) -> Vec<PrincipalId> {
        let mut out = Vec::new();
        for (id, slot) in self.all_slots() {
            let mut live = slot.lock().await;
            let exited = match live.as_mut() {
                Some(l) => l.process.has_exited(),
                None => continue,
            };
            if exited {
                *live = None;
            } else {
                out.push(id);
            }
        }
        out
    }

    /// OD-13: stop children idle past the timeout. `open_ws` counts a
    /// principal's open proxied sockets; any open socket keeps its child.
    pub async fn reap_idle(
        &self,
        open_ws: &(dyn Fn(PrincipalId) -> usize + Send + Sync),
    ) -> Vec<PrincipalId> {
        let mut reaped = Vec::new();
        for (id, slot) in self.all_slots() {
            let mut live = slot.lock().await;
            let Some(l) = live.as_mut() else { continue };
            if l.process.has_exited() {
                *live = None;
                continue;
            }
            if open_ws(id) > 0 {
                l.last_used = Instant::now();
                continue;
            }
            if l.last_used.elapsed() >= self.idle_timeout {
                tracing::info!(
                    "principal child for {id} idle for {}s; stopping it (respawned on demand)",
                    self.idle_timeout.as_secs()
                );
                if let Some(mut l) = live.take() {
                    l.process.terminate();
                }
                reaped.push(id);
            }
        }
        reaped
    }
}

// ─── the real launcher ─────────────────────────────────────────────────────

/// A `tokio::process::Child` launched through the T1 executor.
pub struct TokioChild(pub tokio::process::Child);

impl ChildProcess for TokioChild {
    fn has_exited(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }

    fn terminate(&mut self) {
        if let Some(pid) = self.0.id() {
            // SAFETY: plain syscall on a pid we spawned and have not reaped.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }

    fn kill(&mut self) {
        let _ = self.0.start_kill();
    }
}

/// How the broker launches real children.
pub struct T1Launcher {
    pub executor: Arc<T1Executor>,
    /// This `ikenga-server` binary (root-owned, never principal-writable).
    pub exe: PathBuf,
    pub root: OperatorRoot,
    pub pkgs_dir: Option<PathBuf>,
    pub idle_timeout: Duration,
}

impl T1Launcher {
    /// The child's argv (after the program).
    pub fn args(&self, principal: &Principal) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "--executor-tier".into(),
            "t1".into(),
            "--principal-child".into(),
            "--expected-uid".into(),
            principal.uid.to_string().into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            "0".into(),
            "--data-dir".into(),
            self.root.principal_data(principal.id).into_os_string(),
            "--idle-timeout".into(),
            self.idle_timeout.as_secs().max(1).to_string().into(),
        ];
        if let Some(pkgs) = &self.pkgs_dir {
            args.push("--pkgs-dir".into());
            args.push(pkgs.clone().into_os_string());
        }
        args
    }

    /// The host-only variables the child gets (§9.3's one exception): its
    /// own token, and the `IKENGA_SECRET_*` operator defaults (§5 row 15).
    pub fn host_env(token: &str) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> =
            vec![("IKENGA_AUTH_TOKEN".into(), token.to_string().into())];
        env.extend(
            std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("IKENGA_SECRET_")),
        );
        env
    }
}

impl ChildLauncher for T1Launcher {
    fn launch<'a>(
        &'a self,
        principal: &'a Principal,
        token: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
        Box::pin(async move {
            let data = self.root.principal_data(principal.id);
            let mut spec = SpawnSpec::new(&self.exe);
            spec.args(self.args(principal))
                .current_dir(&principal.home)
                .principal(Some(principal.clone()));
            if let Some(level) = std::env::var_os("RUST_LOG") {
                spec.env("RUST_LOG", level);
            }
            let opts = PipedOpts {
                stdin: StdioMode::Null,
                // The child's log joins the broker's (journald / docker logs).
                stdout: StdioMode::Inherit,
                stderr: StdioMode::Inherit,
                kill_on_drop: true,
                no_console_window: false,
                detached: false,
                // A Ctrl-C at the broker's terminal must not reach children
                // directly; the broker stops them itself.
                new_process_group: true,
            };
            let child =
                self.executor
                    .spawn_piped_with_host_env(spec, opts, &Self::host_env(token))?;
            let pid = child
                .id()
                .ok_or_else(|| anyhow::anyhow!("the principal child exited at once"))?;
            let mut process = TokioChild(child);
            match wait_for_port(&data, pid, &mut process, READY_TIMEOUT).await {
                Ok(addr) => Ok(LaunchedChild {
                    addr,
                    process: Box::new(process),
                }),
                Err(e) => {
                    process.kill();
                    Err(e)
                }
            }
        })
    }
}

/// The largest `daemon.json` the broker will read.
const MAX_DISCOVERY_BYTES: u64 = 64 * 1024;

/// Read `<data>/daemon.json` as the child wrote it: never through a symlink
/// (the directory is the principal's), bounded, and only the `pid`/`port`
/// fields. `None` while it isn't there yet or isn't this child's.
pub fn read_child_port(data_dir: &Path, pid: u32) -> Option<u16> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(data_dir.join("daemon.json"))
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_DISCOVERY_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if json.get("pid").and_then(|v| v.as_u64()) != Some(u64::from(pid)) {
        return None;
    }
    json.get("port")
        .and_then(|v| v.as_u64())
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p != 0)
}

async fn wait_for_port(
    data_dir: &Path,
    pid: u32,
    process: &mut dyn ChildProcess,
    timeout: Duration,
) -> anyhow::Result<SocketAddr> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(port) = read_child_port(data_dir, pid) {
            return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
        }
        if process.has_exited() {
            anyhow::bail!(
                "the principal child exited before it was ready (its log says why; a second \
                 child for the same principal fails its data-dir lock, I-3)"
            );
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "the principal child did not report its port within {}s",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    pub(crate) fn principal() -> Principal {
        Principal {
            id: PrincipalId::new_v7(),
            username: "ada".into(),
            unix_name: "ik-ada".into(),
            uid: 20_000,
            gid: 20_000,
            home: "/srv/ikenga/principals/x/home".into(),
            shell: "/bin/sh".into(),
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeProcess {
        pub exited: Arc<AtomicBool>,
        pub terminated: Arc<AtomicBool>,
    }

    impl ChildProcess for FakeProcess {
        fn has_exited(&mut self) -> bool {
            self.exited.load(Ordering::SeqCst)
        }
        fn terminate(&mut self) {
            self.terminated.store(true, Ordering::SeqCst);
            self.exited.store(true, Ordering::SeqCst);
        }
        fn kill(&mut self) {
            self.exited.store(true, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct CountingLauncher {
        launches: AtomicUsize,
        last_exited: Mutex<Option<Arc<AtomicBool>>>,
    }

    impl ChildLauncher for CountingLauncher {
        fn launch<'a>(
            &'a self,
            _principal: &'a Principal,
            token: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<LaunchedChild>> {
            Box::pin(async move {
                assert_eq!(token.len(), 64, "a 32-byte hex per-child token");
                let n = self.launches.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                let p = FakeProcess::default();
                *self.last_exited.lock().unwrap() = Some(p.exited.clone());
                Ok(LaunchedChild {
                    addr: SocketAddr::from(([127, 0, 0, 1], 40_000 + n as u16)),
                    process: Box::new(p),
                })
            })
        }
    }

    #[tokio::test]
    async fn one_child_per_principal_even_under_concurrent_first_requests() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Arc::new(Children::new(launcher.clone(), DEFAULT_IDLE_TIMEOUT));
        let p = principal();
        let (a, b) = tokio::join!(children.endpoint(&p), children.endpoint(&p));
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
        // Another principal gets its own child and token.
        let q = principal();
        let e = children.endpoint(&q).await.unwrap();
        assert_ne!(e.addr, children.endpoint(&p).await.unwrap().addr);
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
        assert_eq!(children.running().await.len(), 2);
    }

    #[tokio::test]
    async fn an_exited_child_is_respawned_on_the_next_request() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Children::new(launcher.clone(), DEFAULT_IDLE_TIMEOUT);
        let p = principal();
        let first = children.endpoint(&p).await.unwrap();
        launcher
            .last_exited
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store(true, Ordering::SeqCst);
        let second = children.endpoint(&p).await.unwrap();
        assert_ne!(first.addr, second.addr);
        assert_ne!(first.token, second.token, "a fresh token per child");
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn idle_reap_spares_open_sockets_and_stops_the_rest() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Children::new(launcher.clone(), Duration::from_millis(50));
        let (p, q) = (principal(), principal());
        children.endpoint(&p).await.unwrap();
        children.endpoint(&q).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let reaped = children.reap_idle(&|id| usize::from(id == p.id)).await;
        assert_eq!(reaped, vec![q.id], "p has an open socket");
        assert_eq!(children.running().await, vec![p.id]);
        // The next request respawns q's child.
        children.endpoint(&q).await.unwrap();
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn stop_and_invalidate_forget_the_child() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Children::new(launcher.clone(), DEFAULT_IDLE_TIMEOUT);
        let p = principal();
        let e = children.endpoint(&p).await.unwrap();
        // A stale endpoint doesn't knock out a newer child.
        let stale = ChildEndpoint {
            addr: SocketAddr::from(([127, 0, 0, 1], 1)),
            token: "x".into(),
        };
        children.invalidate(p.id, &stale).await;
        assert_eq!(children.running().await, vec![p.id]);
        children.invalidate(p.id, &e).await;
        assert!(children.running().await.is_empty());
        children.endpoint(&p).await.unwrap();
        assert!(children.stop(p.id).await);
        assert!(!children.stop(p.id).await);
    }

    #[test]
    fn the_child_argv_pins_tier_mode_uid_loopback_and_its_own_data_dir() {
        let root = OperatorRoot::new("/srv/ikenga").unwrap();
        let p = principal();
        let launcher = T1Launcher {
            executor: Arc::new(T1Executor::new(crate::executor::t1::T1Config {
                principals_dir: root.principals_dir(),
                principal_path: None,
            })),
            exe: "/usr/local/bin/ikenga-server".into(),
            root: root.clone(),
            pkgs_dir: Some("/opt/ikenga/pkgs".into()),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        };
        let args: Vec<String> = launcher
            .args(&p)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(
            joined.contains("--executor-tier t1 --principal-child"),
            "{joined}"
        );
        assert!(joined.contains("--expected-uid 20000"), "{joined}");
        assert!(joined.contains("--host 127.0.0.1 --port 0"), "{joined}");
        assert!(joined.contains(&format!(
            "--data-dir {}",
            root.principal_data(p.id).display()
        )));
        assert!(joined.contains("--idle-timeout 1800"), "{joined}");
        assert!(joined.contains("--pkgs-dir /opt/ikenga/pkgs"), "{joined}");
        assert!(!joined.contains("token"), "the token never rides argv");

        let env = T1Launcher::host_env("tok");
        assert_eq!(env[0], ("IKENGA_AUTH_TOKEN".into(), "tok".into()));
        assert!(env
            .iter()
            .all(|(k, _)| crate::pty::is_host_only_env(&k.to_string_lossy())));
    }

    #[test]
    fn the_port_is_read_only_from_this_childs_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(read_child_port(dir, 7), None, "not there yet");
        fs::write(dir.join("daemon.json"), r#"{"pid":6,"port":4242}"#).unwrap();
        assert_eq!(read_child_port(dir, 7), None, "a stale file of another pid");
        fs::write(dir.join("daemon.json"), r#"{"pid":7,"port":4242}"#).unwrap();
        assert_eq!(read_child_port(dir, 7), Some(4242));
        fs::write(dir.join("daemon.json"), r#"{"pid":7,"port":0}"#).unwrap();
        assert_eq!(read_child_port(dir, 7), None);
        fs::write(dir.join("daemon.json"), "not json").unwrap();
        assert_eq!(read_child_port(dir, 7), None);
        // Never through a symlink the principal planted.
        fs::remove_file(dir.join("daemon.json")).unwrap();
        fs::write(dir.join("real.json"), r#"{"pid":7,"port":4242}"#).unwrap();
        std::os::unix::fs::symlink(dir.join("real.json"), dir.join("daemon.json")).unwrap();
        assert_eq!(read_child_port(dir, 7), None);
    }
}
