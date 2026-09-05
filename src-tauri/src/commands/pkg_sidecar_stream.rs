//! Streaming RPC for long-lived pkg-declared sidecars.
//!
//! `pkg_sidecar_call` (sibling module) handles one-shot invocations: spawn,
//! pass args + stdin, wait for exit, return captured stdout. That's wrong for
//! sidecars that maintain in-memory state across many requests — language
//! servers, persistent daemons, anything where every call would re-pay the
//! cold-start tax.
//!
//! This module exposes a streaming pair:
//!
//!   • `pkg_sidecar_rpc_send(pkg_id, name, line)` — lazy-spawn the binary on
//!     first call, then write `line + '\n'` to its stdin. Subsequent calls
//!     reuse the live child.
//!   • `pkg://sidecar/{pkg_id}/{name}/message` — Tauri event emitted once
//!     per UTF-8 line read from the child's stdout. Subscribe on the FE via
//!     `@tauri-apps/api/event::listen`.
//!
//! Restart policy: on child exit (clean or crashed) the entry is dropped.
//! Next `pkg_sidecar_rpc_send` spawns a fresh child. No backoff is applied
//! here — pipeline assumes the FE caller is the one that decides whether to
//! retry. (For complex circuit-breaker semantics, prefer declaring the
//! process as a long-lived MCP server instead.)
//!
//! Permission model mirrors `pkg_sidecar_call`: the registry binds each
//! sidecar `name` to a single `pkg_id`; cross-pkg invocation is refused.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};

use dashmap::DashMap;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::pkg::KernelState;
use crate::commands::pkg_sidecar::SidecarsRegistryState;

/// Key into the streaming map: `(pkg_id, sidecar_name)`.
type StreamKey = (String, String);

/// Result of a single spawn attempt. `Ok` carries the stdin Arc that all
/// concurrent callers will share; `Err` carries the spawn error string so
/// every waiter sees the same failure without re-spawning.
type SpawnResult = Result<Arc<AsyncMutex<ChildStdin>>, String>;

/// Slot in the children map. `OnceCell` guarantees the bridge spawn runs
/// AT MOST ONCE per (pkg_id, name) — concurrent callers find the same
/// `Arc<OnceCell<…>>` in the map and `get_or_init` serialises them
/// without us needing to hold the std::sync::Mutex across the (async)
/// spawn. This is what makes the double-spawn race impossible.
type SpawnSlot = Arc<tokio::sync::OnceCell<SpawnResult>>;

#[derive(Default)]
pub struct StreamingSidecarManager {
    // DashMap gives us atomic `entry().or_insert_with()` semantics across
    // threads. The earlier std::sync::Mutex<HashMap> approach exhibited a
    // visibility bug under Tauri's multi-thread command dispatch (two
    // concurrent invocations both saw `size_before=0` for the same map
    // address inside what should have been mutually-exclusive locked
    // regions). With DashMap each shard owns its own lock and atomic
    // insert is built in.
    children: DashMap<StreamKey, SpawnSlot>,
}

impl StreamingSidecarManager {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Global singleton. We were seeing "B sees empty map" symptoms when
/// state-managed via `tauri::State` — concurrent invocations behaved as if
/// they were holding different `Arc`s despite identical address logs. A
/// static singleton sidesteps any Tauri State quirks and gives us an
/// unambiguous, single shared instance.
static STREAMING_MANAGER: OnceLock<Arc<StreamingSidecarManager>> = OnceLock::new();

fn global_manager() -> &'static Arc<StreamingSidecarManager> {
    STREAMING_MANAGER.get_or_init(|| Arc::new(StreamingSidecarManager::new()))
}

pub struct StreamingSidecarManagerState(#[allow(dead_code)] pub Arc<StreamingSidecarManager>);

/// FE -> kernel: append one line to the supervised child's stdin. Lazily
/// spawns the child on first call. The newline terminator is appended by
/// this function; callers send the JSON-RPC payload only.
#[tauri::command]
pub async fn pkg_sidecar_rpc_send(
    app: AppHandle,
    kernel: State<'_, KernelState>,
    sidecars: State<'_, SidecarsRegistryState>,
    pkg_id: String,
    name: String,
    message: String,
) -> Result<(), String> {
    let manager = global_manager().clone();
    let install_path = kernel
        .0
        .installed_path(&pkg_id)
        .ok_or_else(|| format!("pkg `{pkg_id}` is not installed"))?;

    let entry = sidecars
        .0
         .resolve(&name)
        .ok_or_else(|| {
            format!(
                "sidecar `{name}` is not registered (pkg may not be installed or declares no such sidecar)"
            )
        })?;
    if entry.pkg_id != pkg_id {
        return Err(format!(
            "sidecar `{name}` belongs to `{}`, not `{pkg_id}`",
            entry.pkg_id
        ));
    }

    let key: StreamKey = (pkg_id.clone(), name.clone());

    let slot: SpawnSlot = manager
        .children
        .entry(key.clone())
        .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
        .clone();

    let bin_path = entry.bin_path.clone();
    let key_for_spawn = key.clone();
    let app_for_spawn = app.clone();
    let manager_for_spawn = manager.clone();
    let slot_for_spawn = slot.clone();
    let spawned: &SpawnResult = slot
        .get_or_init(|| async move {
            log::info!(
                "[pkg_sidecar_rpc_send] spawning fresh sidecar for {}::{}",
                key_for_spawn.0,
                key_for_spawn.1
            );
            spawn_streaming_child_sync(
                app_for_spawn,
                manager_for_spawn,
                key_for_spawn,
                slot_for_spawn,
                bin_path,
                install_path,
            )
        })
        .await;

    let stdin = spawned.as_ref().map_err(|e| e.clone())?.clone();
    write_line(stdin, &message).await
}

async fn write_line(stdin: Arc<AsyncMutex<ChildStdin>>, line: &str) -> Result<(), String> {
    let mut guard = stdin.lock().await;
    guard
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    guard
        .write_all(b"\n")
        .await
        .map_err(|e| format!("write: {e}"))?;
    guard.flush().await.map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

fn spawn_streaming_child_sync(
    app: AppHandle,
    manager: Arc<StreamingSidecarManager>,
    key: StreamKey,
    slot: SpawnSlot,
    bin_path: PathBuf,
    install_path: PathBuf,
) -> Result<Arc<AsyncMutex<ChildStdin>>, String> {
    log::info!(
        "[pkg_sidecar_rpc_send] spawning streaming sidecar pkg={} name={} bin={}",
        key.0,
        key.1,
        bin_path.display()
    );

    let mut cmd = Command::new(&bin_path);
    cmd.current_dir(&install_path);
    // WP-23 (D-18): hand this pkg its scoped database accessor —
    // `IKENGA_PKG_DB_URL` + a per-pkg `IKENGA_PKG_DB_TOKEN` good only for the
    // two `/iyke/pkg-db/*` routes, enforced against this pkg's own
    // `permissions["sqlite.tables"]`. See `pkg::db_scope`.
    crate::pkg::db_scope::inject_env(&mut cmd, &key.0, &install_path);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // F-9: inject this pkg's settings-declared secret env (e.g. FAL_KEY),
    // resolved from Stronghold under the pkg's own scope. Best-effort — a
    // missing secret leaves the inherited process-env fallback intact. Scoped
    // strictly to the spawning pkg's OWN manifest.
    match crate::pkg::manifest::Package::load(&install_path) {
        Ok(pkg) => {
            if let Some(settings) = pkg.manifest.settings.as_ref() {
                for (name, value) in crate::commands::secrets::resolve_settings_secret_env(
                    &app,
                    &pkg.manifest.id,
                    &settings.schema,
                ) {
                    cmd.env(name, value);
                }
            }
        }
        Err(e) => log::warn!(
            "[pkg_sidecar_rpc_send] could not load manifest at {} for settings-secret env: {e}",
            install_path.display()
        ),
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn `{}`: {e}", bin_path.display()))?;
    let pid = child.id().unwrap_or(0);

    let stdin = child.stdin.take().ok_or_else(|| "no stdin".to_string())?;
    let stdout = child.stdout.take().ok_or_else(|| "no stdout".to_string())?;
    let stderr = child.stderr.take();

    // Tauri rejects `.` in event names; substitute with `_` so a pkg id like
    // `com.ikenga.tsserver-lsp` becomes a valid `com_ikenga_tsserver-lsp`
    // event-channel segment. Consumers must apply the same substitution.
    let safe_pkg = key.0.replace('.', "_");
    let safe_name = key.1.replace('.', "_");
    let event_name = format!("pkg://sidecar/{safe_pkg}/{safe_name}/message");
    let exit_event = format!("pkg://sidecar/{safe_pkg}/{safe_name}/exit");

    // stdout reader → Tauri event
    {
        let app = app.clone();
        let event_name = event_name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']);
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Err(e) = app.emit(&event_name, trimmed.to_string()) {
                            log::warn!("[pkg_sidecar_rpc_send] emit failed: {e}");
                        }
                    }
                    Err(e) => {
                        log::warn!("[pkg_sidecar_rpc_send] stdout read error pid={pid}: {e}");
                        break;
                    }
                }
            }
            log::info!("[pkg_sidecar_rpc_send] stdout closed pid={pid}");
        });
    }

    // stderr drain → debug log
    if let Some(stderr) = stderr {
        let key_for_log = key.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        log::info!(
                            "[sidecar {}::{}] {}",
                            key_for_log.0,
                            key_for_log.1,
                            line.trim_end()
                        );
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let stdin_arc = Arc::new(AsyncMutex::new(stdin));

    // Lifecycle watcher. When the child exits, evict the slot from the map
    // so the next RPC send spawns fresh. Guard against a stale eviction:
    // only remove if the slot Arc in the map is still ours (Arc::ptr_eq).
    // A fresh respawn after our death would replace the entry with a new
    // OnceCell; we shouldn't disturb that.
    {
        let app = app.clone();
        let manager = manager.clone();
        let key = key.clone();
        let our_slot = slot;
        tokio::spawn(async move {
            let status = child.wait().await;
            log::info!(
                "[pkg_sidecar_rpc_send] child exited pkg={} name={} pid={} status={:?}",
                key.0,
                key.1,
                pid,
                status
            );
            // remove_if guarantees the predicate runs under the per-shard
            // lock; the entry is only removed if it's still our slot.
            manager
                .children
                .remove_if(&key, |_, slot_in_map| Arc::ptr_eq(slot_in_map, &our_slot));
            let _ = app.emit(&exit_event, ());
        });
    }

    Ok(stdin_arc)
}

/// Explicit shutdown — drop the live entry so the child sees stdin EOF.
/// Optional for callers; the child also dies if the app exits (kill_on_drop).
#[tauri::command]
pub async fn pkg_sidecar_rpc_shutdown(pkg_id: String, name: String) -> Result<bool, String> {
    let manager = global_manager().clone();
    Ok(manager.children.remove(&(pkg_id, name)).is_some())
}

/// Evict every live streaming sidecar owned by `pkg_id`, returning the count
/// reaped. Called from `Kernel::reload_pkg` (dev hot-reload / re-register):
/// re-registering a pkg only re-adds sidecar *metadata*, so without this the
/// previous long-running child would still be in the map and get reused on the
/// next `pkg_sidecar_rpc_send` — a freshly rebuilt binary would never spawn.
/// Teardown is the same stdin-EOF path `pkg_sidecar_rpc_shutdown` relies on:
/// dropping the map slot drops the shared `ChildStdin`, the child sees EOF and
/// exits, and its lifecycle watcher's `remove_if` becomes a no-op.
pub fn shutdown_pkg_sidecars(pkg_id: &str) -> usize {
    let manager = global_manager();
    let keys: Vec<StreamKey> = manager
        .children
        .iter()
        .filter(|e| e.key().0 == pkg_id)
        .map(|e| e.key().clone())
        .collect();
    let mut reaped = 0usize;
    for k in keys {
        if manager.children.remove(&k).is_some() {
            reaped += 1;
        }
    }
    reaped
}
