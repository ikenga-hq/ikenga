//! Runtime permission checks fired at kernel spawn sites.
//!
//! `permissions.shell_execute` enforcement lives here, not in
//! `registries::permissions`. The Tauri-ACL path can't gate kernel-driven
//! spawns: `pkg::lifecycle::SidecarSupervisor::spawn_and_handshake` and
//! `pkg::mcp_runtime::call_tool` both call `tokio::process::Command` directly
//! and never go through `tauri-plugin-shell`, so a `shell:allow-spawn`
//! capability has nothing to enforce against. We do the check here in plain
//! Rust against the manifest's declared allowlist, before the spawn.
//!
//! Allowlist semantics (post-2026-05-15):
//! - Each entry in `permissions.shell_execute` is a glob pattern matched
//!   against the resolved command — the value handed to `Command::new`.
//! - Examples: `"claude"` (binary on PATH), `"bun"`, `"/usr/local/bin/foo"`,
//!   `"pa-mypkg-*"` (glob match).
//! - Empty allowlist means: cannot spawn anything via the gated paths. A
//!   pkg that ships an MCP server but declares no `shell.execute` entries
//!   is a manifest bug — surface it loudly.
//!
//! Audit recording is best-effort and lives behind the same DB pool as the
//! rest of `pkg_*` state. A failed audit insert does not block the deny
//! decision.

use anyhow::{anyhow, Result};
use glob::Pattern;

/// Outcome of a denied spawn — the caller hands this to the audit recorder.
#[derive(Debug, Clone)]
pub struct ShellExecuteDenied {
    pub pkg_id: String,
    pub command: String,
    /// Comma-joined snapshot of the manifest's allowlist at attempt time.
    /// Stored verbatim in the audit row so a later allowlist edit doesn't
    /// rewrite history.
    pub declared: String,
}

/// Returns `Ok(())` if `command` matches any glob in `shell_execute`.
/// Returns `Err(ShellExecuteDenied)` otherwise — caller should record the
/// denial and refuse to spawn.
///
/// Malformed globs are skipped (logged at warn) so a single bad pattern
/// doesn't lock the pkg out of all its other valid entries.
pub fn check_shell_execute(
    pkg_id: &str,
    shell_execute: &[String],
    command: &str,
) -> std::result::Result<(), ShellExecuteDenied> {
    for raw in shell_execute {
        match Pattern::new(raw) {
            Ok(pat) => {
                if pat.matches(command) {
                    return Ok(());
                }
            }
            Err(e) => {
                log::warn!(
                    "[pkg.permissions_check] pkg `{pkg_id}` shell.execute entry `{raw}` \
                     is not a valid glob: {e}"
                );
            }
        }
    }
    Err(ShellExecuteDenied {
        pkg_id: pkg_id.into(),
        command: command.into(),
        declared: shell_execute.join(","),
    })
}

/// Persist a denial to `pkg_permission_violations`. Best-effort: returns Err
/// only if the SQL itself fails; callers log and proceed regardless.
///
/// Migration `0020_pkg_permission_violations` defines the table; this
/// helper is its only writer.
pub async fn record_violation(
    pool: &sqlx::SqlitePool,
    scope_kind: &str,
    denial: &ShellExecuteDenied,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO pkg_permission_violations
         (pkg_id, scope_kind, attempted, declared, occurred_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&denial.pkg_id)
    .bind(scope_kind)
    .bind(&denial.command)
    .bind(&denial.declared)
    .bind(now)
    .execute(pool)
    .await
    .map_err(|e| anyhow!("insert pkg_permission_violations: {e}"))?;
    // WP-40 `violation` producer. Every denial path funnels through this
    // helper (shell.execute, http.fetch, capabilities.secrets/invoke), so one
    // call here covers them all. Best-effort: a failed notification write
    // never turns a recorded denial into an error.
    crate::notifications::record_best_effort(
        pool,
        crate::notifications::producers::violation(
            &denial.pkg_id,
            scope_kind,
            &denial.command,
        ),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match_allows() {
        assert!(check_shell_execute("p", &s(&["claude"]), "claude").is_ok());
    }

    #[test]
    fn empty_allowlist_denies() {
        let err = check_shell_execute("p", &[], "claude").unwrap_err();
        assert_eq!(err.command, "claude");
        assert_eq!(err.declared, "");
        assert_eq!(err.pkg_id, "p");
    }

    #[test]
    fn non_match_denies() {
        let err = check_shell_execute("p", &s(&["bun", "node"]), "claude").unwrap_err();
        assert_eq!(err.command, "claude");
        assert_eq!(err.declared, "bun,node");
    }

    #[test]
    fn glob_star_matches() {
        assert!(check_shell_execute("p", &s(&["pa-foo-*"]), "pa-foo-worker").is_ok());
        assert!(check_shell_execute("p", &s(&["pa-foo-*"]), "pa-bar-worker").is_err());
    }

    #[test]
    fn absolute_path_matches() {
        assert!(
            check_shell_execute("p", &s(&["/usr/local/bin/foo"]), "/usr/local/bin/foo").is_ok()
        );
    }

    #[test]
    fn malformed_glob_is_skipped_not_fatal() {
        // `[` opens a character class — `[abc` is unterminated and
        // glob::Pattern rejects it. Other entries should still work.
        let allow = s(&["[abc", "claude"]);
        assert!(check_shell_execute("p", &allow, "claude").is_ok());
        assert!(check_shell_execute("p", &allow, "bun").is_err());
    }

    #[test]
    fn declared_preserves_order() {
        let err = check_shell_execute("p", &s(&["a", "b", "c"]), "x").unwrap_err();
        assert_eq!(err.declared, "a,b,c");
    }

    /// WP-40: `record_violation` is the `violation` producer. Repeated denials
    /// of the same target fold into one unread row with a denial count.
    #[tokio::test]
    async fn record_violation_produces_a_folded_violation_notification() {
        use crate::notifications::{self, ListQuery, NotificationKind};

        let tmp = tempfile::tempdir().unwrap();
        let db = crate::commands::db::PaDb::new(tmp.path().join("ikenga.db"));
        let pool = db.ensure_pool().await.unwrap();
        let denial = check_shell_execute("com.ikenga.pkg-browser", &s(&["bun"]), "ffmpeg")
            .unwrap_err();
        for _ in 0..3 {
            record_violation(&pool, "shell.execute", &denial).await.unwrap();
        }

        let rows = notifications::list(
            &pool,
            &ListQuery {
                kinds: Some(vec![NotificationKind::Violation]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "three denials fold into one unread row");
        assert_eq!(rows[0].count, 3);
        assert_eq!(rows[0].title, "pkg-browser was blocked from spawning ffmpeg");
        assert_eq!(
            notifications::unread_count(&pool, &[]).await.unwrap().total,
            1
        );
        // Violation can never be muted, so a caller passing it as muted is the
        // only way to hide it — the command layer never does (see mute.rs).
        assert!(!NotificationKind::Violation.is_mutable());
    }
}
