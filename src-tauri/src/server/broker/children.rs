//! Per-principal children (G-PRINCIPAL §3 topology B, OD-13; G-ACCESS R-6
//! `<broker-child>`).
//!
//! The broker lazily launches one `ikenga-server --executor-tier t1
//! --principal-child` per principal, **through the T1 executor**
//! (`spawn_piped` with `SpawnSpec.principal`), so the child runs as that
//! principal's uid with every §9.2 check behind it. The transport is
//! loopback TCP (P-7): the child binds `127.0.0.1:0`, gets a random
//! per-child `IKENGA_AUTH_TOKEN`, and reports its bound port through its own
//! `<data>/daemon.json`. That file is in a principal-writable directory, so
//! the broker also checks that a loopback **listener of the principal's own
//! uid** holds that port before it sends anything there (a principal can
//! at most point their own traffic at their own process; the planned UDS in
//! a broker-owned directory closes even that).
//!
//! Idle reap (OD-13: "no PTY and no WS"): **the child's own idle watcher**
//! decides, with the timeout the broker passes it (default 30 min) — it
//! counts PTY sessions, open WebSockets and in-flight or recent requests
//! (§5 row 13), which the broker can't see all of. The broker never reaps a
//! live child for idleness; it notices the exit and respawns on the
//! principal's next request.
//!
//! When the broker does stop a child (disable, shutdown) it sends SIGTERM
//! and waits up to [`STOP_GRACE`] for it to drain (PTYs, discovery file,
//! data-dir lock) before SIGKILL. A child that refused a connection is
//! killed and **reaped** before a replacement launches, so the replacement
//! never races the old one for the data-dir lock (I-3).

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
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

use crate::executor::t1::T1Executor;
use crate::executor::{PipedOpts, Principal, PrincipalId, SpawnSpec, StdioMode};
use crate::secrets::principal_store::{WrapKey, WRAP_KEY_ENV};
use crate::server::auth::BoxFuture;
use crate::server::operator::secrets_kek::{KekOwner, SecretsKek};
use crate::server::operator::OperatorRoot;

/// OD-13's default.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long a launched child gets to report its port.
pub const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a SIGTERMed child gets to drain before SIGKILL.
pub const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long the broker waits for a SIGKILLed child to be reaped.
pub const KILL_WAIT: Duration = Duration::from_secs(5);

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
    /// SIGTERM, up to `grace` to drain, then SIGKILL. Resolves once the
    /// process is reaped (or the SIGKILL wait gave up).
    fn shutdown(self: Box<Self>, grace: Duration) -> BoxFuture<'static, ()>;
    /// SIGKILL, and wait up to `wait` for it to be reaped.
    fn kill(self: Box<Self>, wait: Duration) -> BoxFuture<'static, ()>;
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
}

type Slot = Arc<tokio::sync::Mutex<Option<Live>>>;

/// The running children, one slot per principal. A slot's async mutex
/// serializes that principal's launches, so two concurrent first requests
/// start one child, not two (the second would fail its flock anyway, I-3).
pub struct Children {
    launcher: Arc<dyn ChildLauncher>,
    slots: Mutex<HashMap<PrincipalId, Slot>>,
}

fn mint_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

impl Children {
    pub fn new(launcher: Arc<dyn ChildLauncher>) -> Self {
        Self {
            launcher,
            slots: Mutex::new(HashMap::new()),
        }
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
    /// last one exited).
    pub async fn endpoint(&self, principal: &Principal) -> anyhow::Result<ChildEndpoint> {
        let slot = self.slot(principal.id);
        let mut live = slot.lock().await;
        if let Some(l) = live.as_mut() {
            if !l.process.has_exited() {
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
        });
        Ok(endpoint)
    }

    /// The child at `endpoint` refused a connection: if it is still the
    /// current one, kill it and wait for it to be reaped (holding the slot,
    /// so the next [`endpoint`](Self::endpoint) can't launch a replacement
    /// that loses the data-dir lock to it, I-3).
    pub async fn invalidate(&self, id: PrincipalId, endpoint: &ChildEndpoint) {
        let slot = self.slot(id);
        let mut live = slot.lock().await;
        if live.as_ref().is_some_and(|l| &l.endpoint == endpoint) {
            if let Some(l) = live.take() {
                l.process.kill(KILL_WAIT).await;
            }
        }
    }

    /// Stop the principal's child, if any (disable, §7.3): SIGTERM, drain,
    /// then SIGKILL after [`STOP_GRACE`].
    pub async fn stop(&self, id: PrincipalId) -> bool {
        let slot = self.slot(id);
        let mut live = slot.lock().await;
        match live.take() {
            Some(l) => {
                l.process.shutdown(STOP_GRACE).await;
                true
            }
            None => false,
        }
    }

    /// Stop every child (broker shutdown), draining them concurrently.
    pub async fn stop_all(&self) {
        let mut stopping = Vec::new();
        for (_, slot) in self.all_slots() {
            if let Some(l) = slot.lock().await.take() {
                stopping.push(l.process.shutdown(STOP_GRACE));
            }
        }
        futures_util::future::join_all(stopping).await;
    }

    /// The endpoints of the children running right now, without waiting:
    /// a slot that is busy (a launch in progress) is skipped and makes the
    /// result `partial`. Never launches anything, unlike
    /// [`endpoint`](Self::endpoint). For the update routes' terminal count.
    pub fn running_endpoints(&self) -> (Vec<(PrincipalId, ChildEndpoint)>, bool) {
        let mut out = Vec::new();
        let mut partial = false;
        for (id, slot) in self.all_slots() {
            match slot.try_lock() {
                Ok(mut live) => {
                    if let Some(l) = live.as_mut() {
                        if !l.process.has_exited() {
                            out.push((id, l.endpoint.clone()));
                        }
                    }
                }
                Err(_) => partial = true,
            }
        }
        (out, partial)
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
}

// ─── the real launcher ─────────────────────────────────────────────────────

/// A `tokio::process::Child` launched through the T1 executor.
pub struct TokioChild(pub tokio::process::Child);

impl TokioChild {
    async fn kill_and_reap(&mut self, wait: Duration) {
        let _ = self.0.start_kill();
        if tokio::time::timeout(wait, self.0.wait()).await.is_err() {
            tracing::warn!(
                "principal child {:?} not reaped {}s after SIGKILL",
                self.0.id(),
                wait.as_secs()
            );
        }
    }
}

impl ChildProcess for TokioChild {
    fn has_exited(&mut self) -> bool {
        !matches!(self.0.try_wait(), Ok(None))
    }

    /// The process is owned by this future until it is reaped, so
    /// `kill_on_drop` (kept as the safety net for a broker that unwinds)
    /// never cuts the drain short.
    fn shutdown(mut self: Box<Self>, grace: Duration) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let Some(pid) = self.0.id() else { return };
            // SAFETY: plain syscall on a pid we spawned and have not reaped
            // (`id()` is `None` once it has been).
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(grace, self.0.wait()).await.is_err() {
                tracing::warn!(
                    "principal child {pid} still running {}s after SIGTERM; killing it",
                    grace.as_secs()
                );
                self.kill_and_reap(KILL_WAIT).await;
            }
        })
    }

    fn kill(mut self: Box<Self>, wait: Duration) -> BoxFuture<'static, ()> {
        Box::pin(async move { self.kill_and_reap(wait).await })
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
    /// own token, its own secret store's wrapping key (WP-21 — derived from
    /// the operator KEK for this principal alone; the KEK itself never
    /// leaves the broker), and the `IKENGA_SECRET_*` operator defaults (§5
    /// row 15). The child scrubs and unsets the wrapping key at startup.
    pub fn host_env(token: &str, secrets_key: &WrapKey) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = vec![
            ("IKENGA_AUTH_TOKEN".into(), token.to_string().into()),
            (
                WRAP_KEY_ENV.into(),
                secrets_key.to_env_value().as_str().into(),
            ),
        ];
        env.extend(
            std::env::vars_os().filter(|(k, _)| k.to_string_lossy().starts_with("IKENGA_SECRET_")),
        );
        env
    }

    /// The operator KEK (created on the first launch) and this principal's
    /// wrapping key from it. Fails the launch rather than starting a child
    /// with no store, which would serve the operator default in place of the
    /// principal's own credential — including when the KEK file is missing
    /// but one existed before (`secrets_kek::KekLost`: restore it from
    /// backup; a new one is never minted over existing stores).
    ///
    /// `accounts.db` (whose `operator_meta` records KEK creation) is opened
    /// here, per launch, on its own connection: the launcher holds no pool,
    /// and a launch is rare next to the work it starts.
    async fn secrets_key(&self, principal: &Principal) -> anyhow::Result<WrapKey> {
        let mut meta = SqliteConnection::connect_with(
            &SqliteConnectOptions::new()
                .filename(self.root.accounts_db())
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await
        .map_err(|e| anyhow::anyhow!("operator secrets KEK: opening accounts.db: {e}"))?;
        let kek = SecretsKek::load_or_create(&self.root, KekOwner::Root, &mut meta).await;
        let _ = meta.close().await;
        let kek = kek.map_err(|e| anyhow::anyhow!("operator secrets KEK unusable: {e}"))?;
        Ok(kek.wrap_key_for(principal.id))
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
            let secrets_key = self.secrets_key(principal).await?;
            let child = self.executor.spawn_piped_with_host_env(
                spec,
                opts,
                &Self::host_env(token, &secrets_key),
            )?;
            let pid = child
                .id()
                .ok_or_else(|| anyhow::anyhow!("the principal child exited at once"))?;
            let mut process = TokioChild(child);
            match wait_for_port(&data, pid, principal.uid, &mut process, READY_TIMEOUT).await {
                Ok(addr) => Ok(LaunchedChild {
                    addr,
                    process: Box::new(process),
                }),
                Err(e) => {
                    process.kill_and_reap(KILL_WAIT).await;
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

/// Whether a socket of `uid` is listening on `127.0.0.1:port`, per
/// `/proc/net/tcp` (needs no privilege beyond reading it). The port came
/// from a file the principal can write; this keeps the broker from sending
/// the principal's traffic (and the child's bearer) to another uid's
/// listener.
pub fn loopback_listener_uid_is(port: u16, uid: u32) -> bool {
    let Ok(table) = fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    // `sl local_address rem_address st tx:rx tr:when retrnsmt uid ...`;
    // the address is the kernel's u32 in host order, the port big-endian.
    let local = format!("{:08X}:{:04X}", u32::from_ne_bytes([127, 0, 0, 1]), port);
    table.lines().skip(1).any(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        f.len() > 7 && f[1] == local && f[3] == "0A" && f[7].parse::<u32>().ok() == Some(uid)
    })
}

async fn wait_for_port(
    data_dir: &Path,
    pid: u32,
    uid: u32,
    process: &mut dyn ChildProcess,
    timeout: Duration,
) -> anyhow::Result<SocketAddr> {
    let deadline = Instant::now() + timeout;
    let mut warned = false;
    loop {
        if let Some(port) = read_child_port(data_dir, pid) {
            if loopback_listener_uid_is(port, uid) {
                return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
            }
            if !std::mem::replace(&mut warned, true) {
                tracing::warn!(
                    "the principal child's daemon.json names port {port}, which no loopback \
                     listener of uid {uid} holds; waiting"
                );
            }
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
        fn shutdown(self: Box<Self>, _grace: Duration) -> BoxFuture<'static, ()> {
            self.terminated.store(true, Ordering::SeqCst);
            self.exited.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn kill(self: Box<Self>, _wait: Duration) -> BoxFuture<'static, ()> {
            self.exited.store(true, Ordering::SeqCst);
            Box::pin(async {})
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
        let children = Arc::new(Children::new(launcher.clone()));
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
        let children = Children::new(launcher.clone());
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

    /// S3-3: idleness is the child's call (it counts PTYs; the broker can't
    /// see them). The broker keeps a live child however long it was unused.
    #[tokio::test]
    async fn the_broker_never_reaps_a_live_child_for_idleness() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Children::new(launcher.clone());
        let p = principal();
        let first = children.endpoint(&p).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(children.running().await, vec![p.id]);
        assert_eq!(children.endpoint(&p).await.unwrap(), first);
        assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    }

    fn sh(script: &str) -> TokioChild {
        TokioChild(
            tokio::process::Command::new("/bin/sh")
                .args(["-c", script])
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
        )
    }

    /// S3-3: a stopped child gets to drain — SIGTERM, then its own shutdown
    /// path runs — rather than being SIGKILLed by `kill_on_drop` at once.
    #[tokio::test]
    async fn shutdown_lets_the_child_drain_then_escalates() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("drained");
        let child = sh(&format!(
            "trap 'sleep 0.2; echo ok > {}; exit 0' TERM; while :; do sleep 0.05; done",
            marker.display()
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        Box::new(child).shutdown(Duration::from_secs(5)).await;
        assert_eq!(
            fs::read_to_string(&marker).unwrap_or_default().trim(),
            "ok",
            "the SIGTERM handler ran to completion"
        );

        // A child that ignores SIGTERM is killed once the grace runs out.
        let mut stubborn = sh("trap '' TERM; while :; do sleep 0.05; done");
        let pid = stubborn.0.id().unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!stubborn.has_exited());
        let started = Instant::now();
        Box::new(stubborn)
            .shutdown(Duration::from_millis(300))
            .await;
        assert!(started.elapsed() < Duration::from_secs(4));
        // Killed and reaped: the pid is gone.
        assert_ne!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
    }

    /// S3-7: the port named by a principal-writable file is used only if a
    /// loopback listener of the principal's uid holds it.
    #[tokio::test]
    async fn only_a_loopback_listener_of_the_principals_uid_is_trusted() {
        let me = unsafe { libc::geteuid() };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(loopback_listener_uid_is(port, me));
        assert!(!loopback_listener_uid_is(port, me.wrapping_add(1)));
        drop(listener);
        assert!(!loopback_listener_uid_is(port, me), "nobody listens now");
    }

    #[tokio::test]
    async fn stop_and_invalidate_forget_the_child() {
        let launcher = Arc::new(CountingLauncher::default());
        let children = Children::new(launcher.clone());
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

        let kek = [5u8; 32];
        let key = WrapKey::derive(&kek, &p.id.to_string()).unwrap();
        let env = T1Launcher::host_env("tok", &key);
        assert_eq!(env[0], ("IKENGA_AUTH_TOKEN".into(), "tok".into()));
        assert_eq!(
            env[1],
            (WRAP_KEY_ENV.into(), key.to_env_value().as_str().into())
        );
        assert!(env
            .iter()
            .all(|(k, _)| crate::pty::is_host_only_env(&k.to_string_lossy())));
        // WP-21: only this principal's derived key — never the KEK.
        let kek_hex = hex::encode(kek);
        assert!(env
            .iter()
            .all(|(_, v)| !v.to_string_lossy().contains(&kek_hex)));
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
