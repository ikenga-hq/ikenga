//! The executor-routed arms and the pkg-settings write (gap audit 2026-10-06
//! ranks 21, 23 and part of 20): `pkg_sidecar_call`, `action_exec`,
//! `comment_route`, `agent_ops_run_now` and `pkg_settings_set`.
//!
//! Each body is the desktop command's, over the shared core it calls
//! (`shared::{sidecar_call, action_exec, comment_route, agent_ops}`,
//! `pkg::settings_values`). What makes them servable is where they run:
//! every spawn goes through `executor::current()`, and under T1 this handler
//! runs in the signed-in principal's own child — that principal's uid, home,
//! `ikenga.db` and PTYs. Nothing here takes an identity from the caller; a
//! request can only ever act as the process that answers it. All five are
//! owner-class (`access::rpc_requirements`), so a share member reaches none.
//!
//! The deltas from the desktop, each a narrowing:
//!
//! * **`pkg_sidecar_call`** resolves the sidecar from the daemon's read-only
//!   `--pkgs-dir` index ([`PkgIndex::sidecar`]): only a sidecar the named pkg
//!   itself declares, only a binary that canonicalizes inside that pkg's
//!   directory. The child gets this process's environment minus the
//!   host-only secrets (`pty::is_host_only_env`) and no scoped-DB accessor
//!   (that is the desktop's iyke bridge, which the daemon does not run — a
//!   sidecar sees exactly what it saw before WP-23). The sidecar is a
//!   program run as the principal, like `pty_spawn` (also owner-class): the
//!   uid, not the fs allowlist, bounds what it can reach.
//! * **`action_exec`** runs only an action pinned in the principal's own
//!   files and trusted at its hash (the shared `load_pinned`), in a working
//!   directory inside the fs allowlist and outside the daemon's state.
//! * **`comment_route`**'s `terminal` sink reaches only this process's PTYs,
//!   and its `chi` sink is `chi_run` (WP-P10) with the artifact's directory
//!   as cwd — refused unless that directory is inside the fs allowlist.
//! * **`agent_ops_run_now`** fires only a job listed in the principal's own
//!   `jobs.json`, and only at the agent-ops daemon named by the principal's
//!   own `daemon.lock` — on Linux, only when that daemon runs as this
//!   process's uid. That binds the lock's *pid*, not its *port*: the request
//!   is still gated by the lock's secret and the principal's own job list, but
//!   a lock pairing an own pid with a foreign port is not refused here.
//! * **`pkg_settings_set`** writes only a setting the pkg's manifest
//!   declares, for a pkg the index holds (see [`ensure_owner_row`] for how
//!   the row satisfies the `pkg_installed` foreign key).
//!
//! [`PkgIndex::sidecar`]: super::pkg_index::PkgIndex::sidecar

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::AppState;
use super::rpc::RpcResponse;
use super::rpc_local::{arg, opt_str, pa_db, req_str, respond};
use super::rpc_shell::targ;
use super::shared::action_exec::{self, ActionExecRequest, ActionExecResult};
use super::shared::comment_route::{self as route, RouteResult, RouteSink};
use super::shared::{agent_ops, chi_exec, comments, sidecar_call};
use crate::executor::SpawnSpec;

// ─── pkg_sidecar_call (rank 21) ──────────────────────────────────────────────

/// The desktop's arg names (`tauri-cmd.ts` `pkgSidecarCall`): `pkgId`,
/// `name`, `args`, `stdin`, `timeoutSecs`. A pkg / sidecar that does not
/// resolve is an `ok: false` result naming why, as on the desktop — the
/// title-row branch chip and the Explorer badges read that as "no repo
/// data" and stay hidden; a bad argument is an RPC error.
pub(super) async fn pkg_sidecar_call(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id = req_str(args, &["pkgId", "pkg_id"])?;
        let name = req_str(args, &["name"])?;
        let call_args: Vec<String> = match arg(args, &["args"]) {
            None => Vec::new(),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|_| "`args` must be an array of strings".to_string())?,
        };
        let stdin = opt_str(args, &["stdin"])?;
        let timeout_secs = match arg(args, &["timeoutSecs", "timeout_secs"]) {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| "`timeoutSecs` must be a non-negative integer".to_string())?,
            ),
        };
        let resolved = match state.pkg_index.sidecar(&pkg_id, &name) {
            Ok(r) => r,
            Err(e) => return Ok(sidecar_call::PkgSidecarCallResult::failed(e)),
        };
        tracing::info!(
            "[pkg_sidecar_call] pkg={pkg_id} name={name} bin={} args={:?}",
            resolved.bin_path.display(),
            call_args
        );
        let spec = sidecar_spec(&resolved.bin_path, &resolved.install_path, &call_args);
        Ok(sidecar_call::run_one_shot(spec, &resolved.bin_path, stdin, timeout_secs).await)
    }
    .await;
    respond("pkg_sidecar_call", r)
}

/// The spawn for one daemon-side sidecar call: the binary (through
/// `runtime::sidecar_program`, or `node` for a script `--pkgs-dir` left
/// without an exec bit — the desktop chmods at install, the daemon may not
/// write the operator's dir), cwd the pkg's install dir, and this process's
/// environment minus the host-only secrets.
fn sidecar_spec(bin: &Path, install_path: &Path, call_args: &[String]) -> SpawnSpec {
    let (program, pre_args) = sidecar_program(bin);
    let mut spec = SpawnSpec::new(&program);
    chi_exec::inherit_scrubbed_env(&mut spec);
    spec.args(&pre_args)
        .args(call_args)
        .current_dir(install_path)
        .env("PATH", crate::runtime::augmented_path());
    spec
}

fn sidecar_program(bin: &Path) -> (PathBuf, Vec<PathBuf>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let is_script = bin
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "js" | "mjs" | "cjs"));
        let executable = std::fs::metadata(bin)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if is_script && !executable {
            return (PathBuf::from("node"), vec![bin.to_path_buf()]);
        }
    }
    crate::runtime::sidecar_program(bin)
}

// ─── action_exec (rank 23) ───────────────────────────────────────────────────

/// `{ request }` exactly as `src/lib/actions/runner/shell.ts` sends it. The
/// trust check and every refusal are the shared core's; a run whose working
/// directory the fs allowlist refuses is `invalid-cwd`, never spawned.
pub(super) async fn action_exec(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let request: ActionExecRequest = targ(args, &["request"])?;
        let manager = super::rpc_files::actions(state)?;
        state.path_guard.ready()?;
        let result = match action_exec::load_pinned(manager, &request).await {
            Ok((run, pinned_root)) => {
                let variables =
                    action_exec::with_pinned_root(&request.variables, pinned_root.as_deref());
                let guard = state.path_guard.clone();
                let may_run_in = move |dir: &Path| -> Result<(), String> {
                    let canonical = dir
                        .canonicalize()
                        .map_err(|e| format!("working directory `{}`: {e}", dir.display()))?;
                    guard.check(&canonical)
                };
                action_exec::exec_in(
                    &run,
                    &variables,
                    request.timeout_secs,
                    state.home.clone(),
                    &may_run_in,
                )
                .await
            }
            Err(refusal) => ActionExecResult::refused(refusal),
        };
        tracing::info!(
            "[action_exec] scope={} action={} exit={:?} timed_out={} refusal={:?}",
            request.scope,
            request.action_id,
            result.exit_code,
            result.timed_out,
            result.refusal
        );
        Ok(result)
    }
    .await;
    respond("action_exec", r)
}

// ─── comment_route (rank 23) ─────────────────────────────────────────────────

/// The desktop dispatcher (`commands::comment_route`), over this process's
/// PTYs and `chi_run`. Same auto-detect (a live claude PTY, else clipboard —
/// never chi unless asked), same fall-through. The audit row is written for
/// a terminal delivery only (see the note at the write).
pub(super) async fn comment_route(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let id: i64 = targ(args, &["id"])?;
        let override_sink: Option<RouteSink> = targ(args, &["overrideSink", "override_sink"])?;
        let preferred_pty_id: Option<String> = targ(args, &["preferredPtyId", "preferred_pty_id"])?;
        let db = pa_db(state)?;
        let comment = comments::get(db, id).await?;

        let claude_pty = route::pick_claude_pty(&state.pty_manager, preferred_pty_id.as_deref());
        let chosen = override_sink.unwrap_or(if claude_pty.is_some() {
            RouteSink::Terminal
        } else {
            RouteSink::Clipboard
        });

        let mut pty_id_used: Option<String> = None;
        let mut pty_foreground_used: Option<String> = None;
        let mut run_id_used: Option<String> = None;
        let mut clipboard_text: Option<String> = None;

        match chosen {
            RouteSink::Terminal => {
                if let Some((pty_id, fg_name)) = &claude_pty {
                    if state
                        .pty_manager
                        .write(pty_id, route::terminal_line(&comment).as_bytes())
                        .is_ok()
                    {
                        pty_id_used = Some(pty_id.clone());
                        pty_foreground_used = Some(fg_name.clone());
                    }
                }
                if pty_id_used.is_none() {
                    clipboard_text = Some(route::standalone_prompt(&comment));
                }
            }
            RouteSink::Chi => {
                let cwd = match route::parent_dir(&comment.artifact_path) {
                    Some(dir) => Some(allowlisted_dir(state, &dir)?),
                    None => None,
                };
                let opts = chi_exec::ChiRunOpts {
                    engine_id: "claude-code".to_string(),
                    prompt: route::standalone_prompt(&comment),
                    cwd,
                    model: None,
                    mode: None,
                    timeout_seconds: None,
                    parent_id: None,
                    resume_session_id: None,
                    persistent: false,
                };
                let env = super::rpc_local::chi_env(state)?;
                let run =
                    chi_exec::spawn_run(&env, &chi_exec::NoInProcessEngines, opts, "pin").await?;
                run_id_used = Some(run.run_id);
            }
            RouteSink::Clipboard => {
                clipboard_text = Some(route::standalone_prompt(&comment));
            }
        }

        let recorded_sink = match chosen {
            RouteSink::Terminal if pty_id_used.is_none() => RouteSink::Clipboard,
            other => other,
        };
        let updated = comments::record_routing(
            db,
            comment.id,
            recorded_sink.as_str().to_string(),
            None,
            None,
        )
        .await?;
        let recorded_sink = recorded_sink.as_str();

        Ok(RouteResult {
            sink: Some(recorded_sink.to_string()),
            pty_id: pty_id_used,
            pty_foreground: pty_foreground_used,
            run_id: run_id_used,
            clipboard_text,
            comment: updated,
        })
    }
    .await;
    respond("comment_route", r)
}

/// `dir` (an absolute path from a comment row) as a chi cwd: it must exist
/// and canonicalize inside the fs allowlist, outside the daemon's state.
fn allowlisted_dir(state: &AppState, dir: &str) -> Result<String, String> {
    state.path_guard.ready()?;
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(format!("pin artifact directory is not absolute: {dir}"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("pin artifact directory {dir}: {e}"))?;
    state.path_guard.check(&canonical)?;
    Ok(canonical.to_string_lossy().into_owned())
}

// ─── agent_ops_run_now (rank 23) ─────────────────────────────────────────────

/// The desktop's `{ ok, code, status, error }` value. Two refusals come
/// first, both as that same value (so the schedule table renders them like
/// any other run-now failure): `not_found` for a job that is not in the
/// principal's own `jobs.json`, and `forbidden` / `daemon_down` when the
/// agent-ops daemon the principal's lock names is not this uid's process.
pub(super) async fn agent_ops_run_now(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let job_id = req_str(args, &["jobId", "job_id"])?;
        let home = state.home.as_deref();
        let listed = agent_ops::list_jobs(home, agent_ops::MissingConfig::Empty).await?;
        if listed.get("ok").and_then(Value::as_bool) != Some(true) {
            return Ok(listed);
        }
        let own = listed
            .get("jobs")
            .and_then(Value::as_array)
            .is_some_and(|jobs| {
                jobs.iter()
                    .any(|j| j.get("id").and_then(Value::as_str) == Some(job_id.as_str()))
            });
        if !own {
            return Ok(agent_ops::err_value(
                "not_found",
                None,
                format!("`{job_id}` is not one of your agent-ops jobs"),
            ));
        }
        if let Err((code, why)) = lock_daemon_is_ours(home).await {
            return Ok(agent_ops::err_value(code, None, why));
        }
        agent_ops::run_now(home, &job_id).await
    }
    .await;
    respond("agent_ops_run_now", r)
}

/// On Linux, the agent-ops daemon named by `<home>/.agent-ops/daemon.lock`
/// must be a live process owned by this process's effective uid: the lock's
/// secret is about to go to whatever listens on its port, and the job will
/// run as whoever that daemon is. A missing or unreadable lock is left to
/// `run_now`, which reports it as `daemon_down`. Elsewhere (a T0 macOS /
/// Windows daemon — T1 is Linux-only) the lock is trusted as the desktop
/// trusts it.
async fn lock_daemon_is_ours(home: Option<&Path>) -> Result<(), (&'static str, String)> {
    let Ok(lock) = agent_ops::read_daemon_lock(home).await else {
        return Ok(());
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = std::fs::metadata(format!("/proc/{}", lock.pid))
            .map(|m| m.uid())
            .map_err(|_| {
                (
                    "daemon_down",
                    format!("the agent-ops daemon (pid {}) is not running", lock.pid),
                )
            })?;
        // SAFETY: geteuid has no preconditions and cannot fail.
        let me = unsafe { libc::geteuid() };
        if owner != me {
            return Err((
                "forbidden",
                format!(
                    "the agent-ops daemon in your daemon.lock (pid {}) runs as uid {owner}, \
                     not as you (uid {me}); run-now only triggers your own daemon",
                    lock.pid
                ),
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = lock;
    Ok(())
}

// ─── pkg_settings_set (rank 20, WP-19) ───────────────────────────────────────

/// `{ pkgId, key, value }` (`tauri-cmd.ts` `pkgSettingsSet`). Stricter than
/// the desktop, which upserts any key: the pkg must be in this daemon's
/// index and `key` must be a field its manifest declares — every caller
/// (`pkg-loupe`, Ngwa's reset) only ever writes declared fields, and the
/// daemon has no kernel to vouch for anything else.
pub(super) async fn pkg_settings_set(state: &AppState, args: &Value) -> RpcResponse {
    let r = async {
        let pkg_id = req_str(args, &["pkgId", "pkg_id"])?;
        let key = req_str(args, &["key"])?;
        let value = args
            .get("value")
            .cloned()
            .ok_or_else(|| "`value` is required".to_string())?;
        let db = pa_db(state)?;
        let pkg = state
            .pkg_index
            .live_pkg(&pkg_id)
            .ok_or_else(|| format!("pkg `{pkg_id}` is not installed on this server"))?;
        let declared = state
            .pkg_index
            .settings_schema(&pkg_id)
            .is_some_and(|fields| fields.iter().any(|f| f.key == key));
        if !declared {
            return Err(format!("pkg `{pkg_id}` declares no setting `{key}`"));
        }
        ensure_owner_row(db, pkg).await?;
        crate::pkg::settings_values::set(db, &pkg_id, &key, &value).await
    }
    .await;
    respond("pkg_settings_set", r)
}

/// `pkg_settings.pkg_id REFERENCES pkg_installed(id)`, and the daemon
/// installs nothing, so the first write for a pkg records the row that owns
/// its settings: the indexed manifest and path, `source: local` (what
/// `pkg_kernel_status` already reports for it), and **`enabled = 0`**. The
/// daemon never reads `pkg_installed` back (its `--pkgs-dir` index is the
/// whole truth about what it serves), and a disabled row is what makes this
/// safe if a desktop kernel ever opened the same `ikenga.db`: it lists the
/// pkg as installed-but-off instead of replaying its registrations and
/// spawning its sidecars. An existing row — a real desktop install — is
/// never touched (`ON CONFLICT DO NOTHING`). Under T1 the db is the
/// principal's own, so so are the rows.
async fn ensure_owner_row(
    db: &crate::db::PaDb,
    pkg: &crate::pkg::manifest::Package,
) -> Result<(), String> {
    let manifest_path = pkg.install_path.join("manifest.json");
    let manifest_json = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let install_path = pkg.install_path.display().to_string();
    let source = serde_json::to_string(&crate::pkg::InstallSource::Local {
        path: install_path.clone(),
    })
    .map_err(|e| format!("serialize install source: {e}"))?;
    let pool = db.ensure_pool().await?;
    sqlx::query(
        "INSERT INTO pkg_installed
           (id, version, ikenga_api, manifest_json, install_path, installed_at, enabled, source_json)
         VALUES (?, ?, ?, ?, ?, ?, 0, ?)
         ON CONFLICT(id) DO NOTHING",
    )
    .bind(&pkg.manifest.id)
    .bind(&pkg.manifest.version)
    .bind(&pkg.manifest.ikenga_api)
    .bind(&manifest_json)
    .bind(&install_path)
    .bind(chrono::Utc::now().timestamp_millis())
    .bind(&source)
    .execute(&pool)
    .await
    .map_err(|e| format!("record pkg `{}` for its settings: {e}", pkg.manifest.id))?;
    Ok(())
}

#[cfg(test)]
mod tests;
