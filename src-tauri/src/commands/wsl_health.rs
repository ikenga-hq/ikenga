//! `wsl_health_*` Tauri commands (WP-2): probe WSL's network and run the
//! fixes. Thin wrappers over `crate::server::shared::wsl_health`, plus the
//! `fix.wsl_network` notification (D-2, D-8): a fresh probe that finds a
//! WSL-caused failure records it (one row per episode per distro); a fresh
//! `ok` probe resolves it. The report is shared with the Chi pre-run probe
//! (`server::shared::notifications::wsl::report_with_db`, D-21).
//!
//! Desktop-only forever (`server/desktop_only.toml`): they drive `wsl.exe`,
//! the Windows event log and a UAC prompt on the user's own machine.
//!
//! FE contract: `src/lib/tauri-cmd.ts` (`wslHealthProbe`, `wslHealthFix`).

use std::sync::Arc;

use tauri::State;

use crate::commands::db::PaDb;
use crate::server::shared::notifications::wsl::report_with_db;
use crate::server::shared::wsl::{configured_distro, normalise_distro};
use crate::server::shared::wsl_health::{
    self, valid_distro_name, WslFixAction, WslFixOutcome, WslHealth,
};

/// `distro` as given (`"default"` / blank = the default distro), else
/// `engines.agentWslDistro`. Refuses a name that isn't a plain distro name.
fn resolve_distro(distro: Option<String>) -> Result<Option<String>, String> {
    let chosen = match distro {
        Some(raw) => normalise_distro(Some(&raw)),
        None => configured_distro(),
    };
    match chosen {
        Some(d) if !valid_distro_name(&d) => Err(format!("not a WSL distribution name: {d:?}")),
        other => Ok(other),
    }
}

/// Probe WSL networking in `distro` (omit for `engines.agentWslDistro`).
/// Reuses a result younger than 30 s unless `force`.
#[tauri::command]
pub async fn wsl_health_probe(
    db: State<'_, Arc<PaDb>>,
    distro: Option<String>,
    force: Option<bool>,
) -> Result<WslHealth, String> {
    let distro = resolve_distro(distro)?;
    let (health, fresh) = wsl_health::probe(distro.as_deref(), force.unwrap_or(false)).await;
    if fresh {
        report_with_db(&db, &health).await;
    }
    Ok(health)
}

/// Run one fix in `distro` (omit for `engines.agentWslDistro`). On `done`
/// the outcome carries a forced re-probe, which also updates the
/// notification. `restart_networking` and `switch_to_nat` shut WSL down:
/// the caller confirms first and relaunches its sessions afterwards (D-5).
#[tauri::command]
pub async fn wsl_health_fix(
    db: State<'_, Arc<PaDb>>,
    action: WslFixAction,
    distro: Option<String>,
) -> Result<WslFixOutcome, String> {
    let distro = resolve_distro(distro)?;
    let outcome = wsl_health::fix(action, distro.as_deref()).await;
    if let WslFixOutcome::Done { health } = &outcome {
        report_with_db(&db, health).await;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_distros_are_validated_and_default_means_none() {
        assert_eq!(
            resolve_distro(Some("Ubuntu".into())),
            Ok(Some("Ubuntu".into()))
        );
        assert_eq!(resolve_distro(Some("default".into())), Ok(None));
        assert_eq!(resolve_distro(Some("  ".into())), Ok(None));
        assert!(resolve_distro(Some("Ubuntu; wsl --shutdown".into())).is_err());
        assert!(resolve_distro(Some("-u".into())).is_err());
    }
}
