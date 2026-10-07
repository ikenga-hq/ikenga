//! Route → capability mapping for every `rpc.rs` arm (G-ACCESS §1.6).
//!
//! **Append-only for arm-adding WPs** (§1.5, §9.3): a WP that adds an
//! `rpc.rs` arm appends its row here *and* regenerates
//! `src/lib/access/rpc-requirements.gen.ts` (`IKENGA_UPDATE_GENERATED=1 cargo
//! test --lib access::caps_ts`) in the same change. Changing an existing
//! row's requirement is a G-ACCESS erratum.
//!
//! A-1 (`tests::every_rpc_arm_is_mapped`) lexes `rpc.rs` with the parity
//! gate's own arm parser (`server::parity::served_verbs`) and fails when an
//! arm has no row, or a row names no arm. At runtime an unmapped command is
//! `{caps: all 7, class: owner}` (§1.6 rule 2, [`requirement`]).

use super::caps::Cap::{Approve, Dispatch, Files, Install, Secrets, Sessions, Settings};
use super::caps::{CapSet, Requirement};

const ALL: &[super::caps::Cap] = &[
    Files, Sessions, Dispatch, Approve, Install, Settings, Secrets,
];

macro_rules! req {
    (shared[$($c:ident),*]) => { Requirement::shared(&[$($c),*]) };
    (owner[$($c:ident),*]) => { Requirement::owner(&[$($c),*]) };
    (owner_all) => { Requirement::owner(ALL) };
    (operator) => { Requirement::operator() };
    (access) => { Requirement::access() };
    (internal) => { Requirement::internal() };
}

/// Every served verb → its requirement (§1.6 "RPC arms on `main`" + "New
/// (§9)"). Grouped by family; order is not significant (the generated TS is
/// sorted).
pub const RPC_REQUIREMENTS: &[(&str, Requirement)] = &[
    // ── PTY ──
    ("pty_list", req!(owner[Sessions])),
    ("pty_terminal_list", req!(owner[Sessions])),
    ("pty_foreground", req!(owner[Sessions])),
    ("pty_foreground_snapshot", req!(owner[Sessions])),
    ("pty_spawn", req!(owner[Dispatch])),
    ("pty_write", req!(owner[Dispatch])),
    ("pty_resize", req!(owner[Dispatch])),
    ("pty_kill", req!(owner[Dispatch])),
    // ── fs read ──
    ("fs_exists", req!(shared[Files])),
    ("fs_read", req!(shared[Files])),
    ("fs_list", req!(shared[Files])),
    ("fs_kind", req!(shared[Files])),
    ("fs_mime", req!(shared[Files])),
    ("fs_search", req!(shared[Files])),
    ("fs_roots_list", req!(owner[Files])),
    ("fs_home", req!(owner[Files])),
    // ── fs write (P-2: files + dispatch) ──
    ("fs_write", req!(shared[Files, Dispatch])),
    ("fs_trash", req!(shared[Files, Dispatch])),
    ("fs_mkdir", req!(shared[Files, Dispatch])),
    ("fs_rename", req!(shared[Files, Dispatch])),
    // ── raw DB (P-3: owner-class) ──
    ("db_query", req!(owner[Files, Sessions, Settings])),
    ("db_exec", req!(owner_all)),
    // ── pkg UI ──
    ("pkg_content_html", req!(shared[Files])),
    ("pkg_content_revoke", req!(shared[Files])),
    ("pkg_kernel_status", req!(shared[Files])),
    ("list_skill_actions", req!(shared[Files])),
    ("list_all_skill_actions", req!(shared[Files])),
    ("pkg_settings_get", req!(shared[Files])),
    ("pkg_activity_bar_set_badge", req!(shared[Files])),
    ("pkg_trust_list_pending", req!(shared[Files])),
    ("pkg_is_trusted_for_elevated", req!(shared[Files])),
    // ── secrets ──
    ("secrets_get", req!(owner[Secrets])),
    ("secrets_get_scoped", req!(owner[Secrets])),
    ("secrets_list_keys", req!(owner[Settings])),
    ("secrets_list_keys_scoped", req!(owner[Settings])),
    ("secrets_index_names", req!(owner[Settings])),
    ("secrets_default_names", req!(owner[Settings])),
    ("secrets_vault_status", req!(owner[Settings])),
    ("secrets_set", req!(owner[Settings, Secrets])),
    ("secrets_delete", req!(owner[Settings, Secrets])),
    ("secrets_set_scoped", req!(owner[Settings, Secrets])),
    ("secrets_delete_scoped", req!(owner[Settings, Secrets])),
    // WP-20-tagged lock family, served by WP-21 (§1.6 row "WP-20-tagged,
    // served by WP-21 in W3"; X-1). Names confirmed against WP-21's arm.
    ("secrets_lock_state", req!(owner[])),
    ("secrets_lock", req!(owner[Settings])),
    ("secrets_unlock", req!(owner[Settings, Secrets])),
    ("secrets_set_passphrase", req!(owner[Settings, Secrets])),
    // ── supabase ──
    ("supabase_config_get", req!(owner[Secrets])),
    ("supabase_config_set", req!(owner[Settings, Secrets])),
    ("supabase_config_clear", req!(owner[Settings, Secrets])),
    // ── settings (a share writes project scope only, §4.5.4) ──
    ("settings_get", req!(shared[Files])),
    ("settings_read_file", req!(shared[Files])),
    ("settings_get_all", req!(owner[Files])),
    ("settings_set", req!(shared[Settings])),
    ("settings_write_field", req!(shared[Settings])),
    ("settings_clear_all", req!(owner[Settings])),
    // ── data / backup ──
    ("data_health_scan", req!(owner[Settings])),
    ("data_health_db_size", req!(owner[Settings])),
    ("backup_list", req!(owner[Settings])),
    ("backup_delete", req!(owner[Settings])),
    // ── chi / agent-ops / identity ──
    ("chi_status", req!(shared[Sessions])),
    ("chi_list", req!(shared[Sessions])),
    // WP-P10. Owner-class: a run executes as the serving principal's uid with
    // that principal's engine logins, and neither a run's cwd nor a run id is
    // share-root-confined, so a share member may not dispatch, resume or
    // cancel one in the Owner's child (reads stay `shared[Sessions]`).
    ("chi_run", req!(owner[Dispatch])),
    ("chi_resume", req!(owner[Dispatch])),
    ("chi_cancel", req!(owner[Dispatch])),
    ("agent_ops_list_jobs", req!(owner[Sessions])),
    ("agent_ops_tail_run", req!(owner[Sessions])),
    ("agent_ops_upsert_job", req!(owner[Settings, Dispatch])),
    ("agent_ops_delete_job", req!(owner[Settings, Dispatch])),
    ("agent_ops_set_enabled", req!(owner[Settings, Dispatch])),
    ("os_username", req!(owner[])),
    // ── notifications (a share sees only the project's permission rows) ──
    ("notifications_list", req!(shared[Sessions])),
    ("notifications_unread_count", req!(shared[Sessions])),
    ("notifications_mark_read", req!(owner[])),
    ("notifications_mark_all_read", req!(owner[])),
    ("notifications_mute_state", req!(owner[])),
    ("notifications_mute_kind", req!(owner[])),
    ("notifications_unmute_kind", req!(owner[])),
    // ── projects ──
    ("project_list", req!(shared[Files])),
    ("project_get_active", req!(shared[Files])),
    ("project_inventory", req!(shared[Files])),
    ("project_skills_list", req!(shared[Files])),
    ("project_artifacts_walk", req!(shared[Files])),
    ("project_create", req!(owner[Settings])),
    ("project_update", req!(owner[Settings])),
    ("project_archive", req!(owner[Settings])),
    ("project_set_active", req!(owner[Settings])),
    ("project_scaffold_claude", req!(owner[Settings])),
    // ── activity bar (the viewer's own chrome) ──
    ("activity_sections_list", req!(owner[])),
    ("activity_sections_create", req!(owner[])),
    ("activity_sections_update", req!(owner[])),
    ("activity_sections_remove", req!(owner[])),
    ("activity_pins_list", req!(owner[])),
    ("activity_pins_add", req!(owner[])),
    ("activity_pins_resolve_artifact", req!(owner[])),
    ("activity_pins_touch_open", req!(owner[])),
    ("activity_pins_remove", req!(owner[])),
    ("activity_pins_reorder", req!(owner[])),
    // ── comments (non-Owner edits only their own rows, §4.5.4) ──
    ("comment_get", req!(shared[Files])),
    ("comment_list", req!(shared[Files])),
    ("comment_create", req!(shared[Files])),
    ("comment_set_status", req!(shared[Files])),
    ("comment_delete", req!(shared[Files])),
    ("comment_record_routing", req!(shared[Dispatch])),
    // ── studio threads ──
    ("studio_thread_get", req!(shared[Sessions])),
    ("studio_thread_list_recent", req!(shared[Sessions])),
    ("studio_message_list", req!(shared[Sessions])),
    ("studio_thread_get_or_create", req!(shared[Dispatch])),
    ("studio_message_append", req!(shared[Dispatch])),
    ("studio_thread_delete", req!(owner[Dispatch])),
    // ── claude config / detect ──
    ("claude_config_load", req!(owner[Files])),
    ("claude_config_read_file", req!(owner[Files])),
    ("claude_config_resolve_cascade", req!(owner[Files])),
    ("detect_agent", req!(owner[])),
    ("detect_agents", req!(owner[])),
    ("detect_agent_config", req!(owner[Files])),
    ("list_claude_projects", req!(owner[Files])),
    ("list_agent_projects", req!(owner[Files])),
    ("claude_asset_list_pins", req!(owner[Files])),
    ("engine_layout", req!(owner[])),
    ("terminal_detect_shells", req!(owner[])),
    ("claude_asset_pin", req!(owner[Settings])),
    ("claude_asset_unpin", req!(owner[Settings])),
    // ── transcripts (a share is confined to the project's cwd) ──
    ("claude_list_sessions", req!(shared[Sessions])),
    ("claude_read_jsonl", req!(shared[Sessions])),
    ("claude_session_list", req!(shared[Sessions])),
    // ── Ngwa vault ──
    ("claude_store_list", req!(shared[Files])),
    ("oba_dependents", req!(shared[Files])),
    ("oba_missing_requires", req!(shared[Files])),
    ("claude_primitive_enable_for", req!(shared[Install])),
    ("claude_primitive_disable_for", req!(shared[Install])),
    ("claude_primitive_remove_for", req!(shared[Install])),
    ("claude_store_import", req!(owner[Install])),
    ("claude_primitive_enable", req!(owner[Install])),
    ("claude_primitive_disable", req!(owner[Install])),
    ("claude_primitive_remove", req!(owner[Install])),
    ("claude_primitive_copy", req!(owner[Install])),
    ("claude_primitive_move", req!(owner[Install])),
    ("claude_primitive_copy_batch", req!(owner[Install])),
    ("oba_backfill_registry", req!(owner[Install])),
    ("oba_forget", req!(owner[Install])),
    ("oba_safe_delete", req!(owner[Install])),
    ("oba_set_auto_update", req!(owner[Install])),
    ("oba_relink_dependents", req!(owner[Install])),
    ("oba_unlink_one", req!(owner[Install])),
    // ── actions / trust ──
    ("actions_read_files", req!(shared[Files])),
    ("actions_trust_status", req!(shared[Files])),
    ("actions_write", req!(shared[Settings])),
    ("keybindings_write", req!(shared[Settings])),
    ("actions_trust_grant", req!(owner[Settings])),
    ("actions_trust_revoke", req!(owner[Settings])),
    // ── outbound approve gate ──
    ("pa_actions_list", req!(owner[Sessions])),
    ("pa_actions_pause", req!(owner[Approve])),
    ("pa_actions_update", req!(owner[Approve])),
    ("pa_actions_commit", req!(owner[Approve])),
    ("pa_actions_retry", req!(owner[Approve])),
    ("pa_actions_reject", req!(owner[Approve])),
    // ── pkg diagnostics ──
    ("pkg_permission_violations_list", req!(owner[Settings])),
    ("pkg_permission_violations_clear", req!(owner[Settings])),
    ("pkg_db_diag", req!(owner[Settings])),
    // ── atelier / git ──
    ("atelier_file_read", req!(shared[Files])),
    ("action_git_branch", req!(shared[Files])),
    ("atelier_file_write", req!(shared[Files, Dispatch])),
    // ── slice 8 ──
    ("pin_screenshot_write", req!(shared[Files])),
    ("scaffold_agent_config", req!(owner[Settings])),
    ("pkg_preview_manifest", req!(owner[Files])),
    ("pkg_discover_workspace", req!(owner[Files])),
    ("pkg_scaffold", req!(owner[Install])),
    // ── executor-routed + pkg settings (gap audit 2026-10-06 ranks 21/23/20).
    // Owner-class: each spawns (or writes the pkg config) as the serving
    // principal, and neither a sidecar's inputs, an action's run, a pin's
    // PTY / chi run nor an agent-ops job is share-root-confined. ──
    ("pkg_sidecar_call", req!(owner[Dispatch])),
    ("action_exec", req!(owner[Dispatch])),
    ("comment_route", req!(owner[Dispatch])),
    ("agent_ops_run_now", req!(owner[Dispatch])),
    ("pkg_settings_set", req!(owner[Settings])),
    // ── New (§9): G-ACCESS's own arms (WP-74a registers; W3–W5 fill) ──
    ("access_status", req!(access)),
    ("access_devices_list", req!(access)),
    ("access_device_set_tier", req!(access)),
    ("access_device_revoke", req!(access)),
    ("access_pair_begin", req!(access)),
    ("access_pair_cancel", req!(access)),
    ("access_pair_pending", req!(access)),
    ("access_pair_decide", req!(access)),
    ("access_routing_get", req!(access)),
    ("access_routing_set", req!(access)),
    ("access_members_list", req!(access)),
    ("access_member_set_role", req!(access)),
    ("access_member_remove", req!(access)),
    ("access_member_restore", req!(access)),
    ("access_policy_get", req!(access)),
    ("access_policy_set_cell", req!(access)),
    ("access_policy_set_owner_approval", req!(access)),
    ("access_invite_issue", req!(access)),
    ("access_invite_revoke", req!(access)),
    ("access_shares_list", req!(access)),
    ("access_audit_list", req!(access)),
    ("access_audit_verify", req!(access)),
    ("access_audit_export", req!(access)),
    ("access_audit_record_local", req!(access)),
    ("access_audit_reseal", req!(access)),
    // ── push (plans/pwa S2 §7) ──
    ("access_push_config", req!(access)),
    ("access_push_subscribe", req!(access)),
    ("access_push_update", req!(access)),
    ("access_push_unsubscribe", req!(access)),
    ("access_push_list", req!(access)),
    ("access_push_test", req!(access)),
    ("permission_decide", req!(shared[Approve])),
    ("notifications_record_access", req!(internal)),
    ("share_project_info", req!(internal)),
    // WP-P9: the broker's cross-principal open-terminal count.
    ("server_open_terminals", req!(internal)),
    ("permission_relay_put", req!(operator)),
    ("permission_relay_take", req!(operator)),
    ("permission_relay_resolve", req!(operator)),
];

/// The §1.6 defaults for the `WP-20`-tagged verbs WP-21 did **not** serve.
/// Not arms yet, so they are NOT in [`RPC_REQUIREMENTS`] (A-1 refuses a row
/// that names no arm). WP-21 served the four `secrets_*` lock verbs, which
/// moved into the table above (X-1); `app_lock_*` stays desktop-only (its
/// PIN record is `app-lock.json`, not the secrets store) and `fs_roots_*`
/// is not WP-21's. Whichever WP serves one of these moves its row up.
pub const PENDING_WP21: &[(&str, Requirement)] = &[
    ("app_lock_status", req!(owner[])),
    ("app_lock_clear_secret", req!(owner[Settings])),
    ("app_lock_configure", req!(owner[Settings])),
    ("app_lock_lock", req!(owner[Settings])),
    ("app_lock_set_secret", req!(owner[Settings])),
    ("app_lock_touch", req!(owner[Settings])),
    ("app_lock_unlock", req!(owner[Settings])),
    ("app_lock_unlock_biometric", req!(owner[Settings])),
    ("fs_roots_add", req!(operator)),
    ("fs_roots_remove", req!(operator)),
    ("fs_roots_reset", req!(operator)),
];

/// The requirement for `cmd`; §1.6 rule 2 for an unmapped one.
pub fn requirement(cmd: &str) -> Requirement {
    RPC_REQUIREMENTS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, r)| *r)
        .unwrap_or(Requirement::UNMAPPED)
}

/// Whether `cmd` has a row (unmapped arms are refused for non-operators at
/// the class check anyway; this is for diagnostics).
pub fn is_mapped(cmd: &str) -> bool {
    RPC_REQUIREMENTS.iter().any(|(name, _)| *name == cmd)
}

/// The caps `cmd` needs, for the `forbidden: missing=` message.
pub fn caps_for(cmd: &str) -> CapSet {
    requirement(cmd).caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::caps::ArmClass;
    use std::collections::BTreeSet;

    const RPC_RS: &str = include_str!("../server/rpc.rs");

    /// A-1: every `rpc.rs` arm literal has exactly one row, and no row names
    /// a non-arm. Uses the parity gate's lexer (handles `cmd @ ("a" | "b")`).
    #[test]
    fn every_rpc_arm_is_mapped() {
        let arms = crate::server::parity::served_verbs(RPC_RS);
        let mut seen = BTreeSet::new();
        let mut dupes = Vec::new();
        for (name, _) in RPC_REQUIREMENTS {
            if !seen.insert(name.to_string()) {
                dupes.push(*name);
            }
        }
        assert!(
            dupes.is_empty(),
            "duplicate RPC_REQUIREMENTS rows: {dupes:?}"
        );
        let unmapped: Vec<_> = arms.difference(&seen).collect();
        assert!(
            unmapped.is_empty(),
            "rpc.rs arms with no RPC_REQUIREMENTS row (append one in \
             src-tauri/src/access/rpc_requirements.rs, G-ACCESS §1.6 rule 1): {unmapped:?}"
        );
        let stale: Vec<_> = seen.difference(&arms).collect();
        assert!(
            stale.is_empty(),
            "RPC_REQUIREMENTS rows that name no rpc.rs arm: {stale:?}"
        );
    }

    #[test]
    fn pending_wp21_rows_are_not_double_listed() {
        for (name, _) in PENDING_WP21 {
            assert!(
                !is_mapped(name),
                "{name} is served now — drop it from PENDING_WP21"
            );
        }
    }

    #[test]
    fn unmapped_is_owner_all_seven() {
        let r = requirement("no_such_cmd");
        assert_eq!(r.class, ArmClass::Owner);
        assert_eq!(r.caps, CapSet::ALL);
    }

    /// P-3 spot checks: raw DB, PTY, secrets and the approve gate are never
    /// reachable through a share.
    #[test]
    fn p3_owner_class_families() {
        for cmd in [
            "db_exec",
            "db_query",
            "pty_spawn",
            "pty_write",
            "secrets_get",
            "supabase_config_get",
            "pa_actions_commit",
            "settings_get_all",
        ] {
            assert_eq!(requirement(cmd).class, ArmClass::Owner, "{cmd}");
        }
        assert!(
            requirement("fs_write").caps.contains(super::Dispatch),
            "P-2"
        );
    }

    #[test]
    fn every_access_arm_is_access_class() {
        for (name, r) in RPC_REQUIREMENTS {
            if name.starts_with("access_") {
                assert_eq!(r.class, ArmClass::Access, "{name}");
            }
        }
    }
}
