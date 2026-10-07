//! WP-53: the desktop `action_exec` / `action_git_branch` commands. The
//! `shell` run kind's core — the pinned-run trust check, `{{var}}`
//! interpolation and the executor-routed spawn — lives in
//! [`crate::server::shared::action_exec`], which the daemon serves too (gap
//! audit 2026-10-06 rank 23); this module keeps every
//! `commands::action_exec::…` path and adds the two Tauri wrappers.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::State;

use crate::actions::ActionsManager;
pub use crate::server::shared::action_exec::*;

/// `shell` run kind: load the pinned run, check it, run it (module note).
#[tauri::command]
pub async fn action_exec(
    manager: State<'_, Arc<ActionsManager>>,
    request: ActionExecRequest,
) -> Result<ActionExecResult, String> {
    let manager: &ActionsManager = manager.inner().as_ref();
    let result = match load_pinned(manager, &request).await {
        Ok((run, pinned_root)) => {
            let variables = with_pinned_root(&request.variables, pinned_root.as_deref());
            exec(
                &run,
                &variables,
                request.timeout_secs,
                crate::platform::home_dir(),
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

/// The branch checked out at `root`, read from `.git/HEAD` (no subprocess).
/// `None` when `root` is not a git work tree or HEAD is detached. Lives in
/// `server::shared::git` (WP-19 slice 6), which the daemon serves too.
pub use crate::server::shared::git::git_branch_at;

/// `{{branch}}` (§8.2): the current git branch at `root`, `null` if none.
#[tauri::command]
pub async fn action_git_branch(root: String) -> Result<Option<String>, String> {
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Ok(None);
    }
    Ok(tokio::task::spawn_blocking(move || git_branch_at(&root))
        .await
        .unwrap_or(None))
}
