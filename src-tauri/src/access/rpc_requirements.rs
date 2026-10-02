//! Route → capability mapping for every `server/rpc.rs` arm (G-ACCESS §1.6).
//!
//! **Append-only.** A WP that adds an `rpc.rs` arm appends its row here (and
//! re-renders `src/lib/access/rpc-requirements.gen.ts`, §1.5) in the same
//! change; A-1 fails otherwise. Changing an existing row's requirement is a
//! G-ACCESS erratum. At runtime an unmapped command needs all seven caps and
//! is owner-class (§1.6 rule 2, [`Requirement::UNMAPPED`]).
//!
//! Requirements are conjunctions (rule 3). Operator-class rows carry all
//! seven caps: only the T0 operator bearer reaches them, and it holds all
//! seven.

use super::caps::{Cap, Requirement};

use Cap::{Approve, Dispatch, Files, Install, Secrets, Sessions, Settings};

const fn shared(caps: &[Cap]) -> Requirement {
    Requirement::shared(caps)
}
const fn owner(caps: &[Cap]) -> Requirement {
    Requirement::owner(caps)
}
const OPERATOR: Requirement = Requirement::operator();
const ACCESS: Requirement = Requirement::access();
const INTERNAL: Requirement = Requirement::internal();
const ALL7: &[Cap] = &Cap::ALL;

/// Every served RPC verb and what it needs. Exactly one row per arm literal
/// (A-1, `tests::every_rpc_arm_is_mapped`).
pub const RPC_REQUIREMENTS: &[(&str, Requirement)] = &[
    // ── PTY ──────────────────────────────────────────────────────────────
    ("pty_list", owner(&[Sessions])),
    ("pty_terminal_list", owner(&[Sessions])),
    ("pty_foreground", owner(&[Sessions])),
    ("pty_foreground_snapshot", owner(&[Sessions])),
    ("pty_spawn", owner(&[Dispatch])),
    ("pty_write", owner(&[Dispatch])),
    ("pty_resize", owner(&[Dispatch])),
    ("pty_kill", owner(&[Dispatch])),
    // ── fs read ──────────────────────────────────────────────────────────
    ("fs_exists", shared(&[Files])),
    ("fs_read", shared(&[Files])),
    ("fs_list", shared(&[Files])),
    ("fs_kind", shared(&[Files])),
    ("fs_mime", shared(&[Files])),
    ("fs_search", shared(&[Files])),
    ("fs_roots_list", owner(&[Files])),
    ("fs_home", owner(&[Files])),
    // ── fs write (P-2: files + dispatch) ─────────────────────────────────
    ("fs_write", shared(&[Files, Dispatch])),
    ("fs_mkdir", shared(&[Files, Dispatch])),
    ("fs_rename", shared(&[Files, Dispatch])),
    // ── raw DB ───────────────────────────────────────────────────────────
    ("db_query", owner(&[Files, Sessions, Settings])),
    ("db_exec", owner(ALL7)),
    // ── pkg UI ───────────────────────────────────────────────────────────
    ("pkg_content_html", shared(&[Files])),
    ("pkg_content_revoke", shared(&[Files])),
    ("pkg_kernel_status", shared(&[Files])),
    ("list_skill_actions", shared(&[Files])),
    ("list_all_skill_actions", shared(&[Files])),
    ("pkg_settings_get", shared(&[Files])),
    // ── secrets ──────────────────────────────────────────────────────────
    ("secrets_get", owner(&[Secrets])),
    ("secrets_get_scoped", owner(&[Secrets])),
    ("secrets_list_keys", owner(&[Settings])),
    ("secrets_list_keys_scoped", owner(&[Settings])),
    ("secrets_index_names", owner(&[Settings])),
    ("secrets_vault_status", owner(&[Settings])),
    ("secrets_set", owner(&[Settings, Secrets])),
    ("secrets_delete", owner(&[Settings, Secrets])),
    ("secrets_set_scoped", owner(&[Settings, Secrets])),
    ("secrets_delete_scoped", owner(&[Settings, Secrets])),
    // ── supabase ─────────────────────────────────────────────────────────
    ("supabase_config_get", owner(&[Secrets])),
    ("supabase_config_set", owner(&[Settings, Secrets])),
    ("supabase_config_clear", owner(&[Settings, Secrets])),
    // ── settings ─────────────────────────────────────────────────────────
    ("settings_get", shared(&[Files])),
    ("settings_read_file", shared(&[Files])),
    ("settings_get_all", owner(&[Files])),
    ("settings_set", shared(&[Settings])),
    ("settings_write_field", shared(&[Settings])),
    ("settings_clear_all", owner(&[Settings])),
    // ── data / backup ────────────────────────────────────────────────────
    ("data_health_scan", owner(&[Settings])),
    ("data_health_db_size", owner(&[Settings])),
    ("backup_list", owner(&[Settings])),
    ("backup_delete", owner(&[Settings])),
    // ── chi / agent-ops / identity ───────────────────────────────────────
    ("chi_status", shared(&[Sessions])),
    ("chi_list", shared(&[Sessions])),
    ("agent_ops_list_jobs", owner(&[Sessions])),
    ("agent_ops_tail_run", owner(&[Sessions])),
    ("agent_ops_upsert_job", owner(&[Settings, Dispatch])),
    ("agent_ops_delete_job", owner(&[Settings, Dispatch])),
    ("agent_ops_set_enabled", owner(&[Settings, Dispatch])),
    ("os_username", owner(&[])),
    // ── notifications ────────────────────────────────────────────────────
    ("notifications_list", shared(&[Sessions])),
    ("notifications_unread_count", shared(&[Sessions])),
    ("notifications_mark_read", owner(&[])),
    ("notifications_mark_all_read", owner(&[])),
    ("notifications_mute_state", owner(&[])),
    ("notifications_mute_kind", owner(&[])),
    ("notifications_unmute_kind", owner(&[])),
    // ── projects ─────────────────────────────────────────────────────────
    ("project_list", shared(&[Files])),
    ("project_get_active", shared(&[Files])),
    ("project_inventory", shared(&[Files])),
    ("project_skills_list", shared(&[Files])),
    ("project_artifacts_walk", shared(&[Files])),
    ("project_create", owner(&[Settings])),
    ("project_update", owner(&[Settings])),
    ("project_archive", owner(&[Settings])),
    ("project_set_active", owner(&[Settings])),
    ("project_scaffold_claude", owner(&[Settings])),
    // ── activity bar (the viewer's own chrome) ───────────────────────────
    ("activity_sections_list", owner(&[])),
    ("activity_sections_create", owner(&[])),
    ("activity_sections_update", owner(&[])),
    ("activity_sections_remove", owner(&[])),
    ("activity_pins_list", owner(&[])),
    ("activity_pins_add", owner(&[])),
    ("activity_pins_resolve_artifact", owner(&[])),
    ("activity_pins_touch_open", owner(&[])),
    ("activity_pins_remove", owner(&[])),
    ("activity_pins_reorder", owner(&[])),
    // ── comments ─────────────────────────────────────────────────────────
    ("comment_get", shared(&[Files])),
    ("comment_list", shared(&[Files])),
    ("comment_create", shared(&[Files])),
    ("comment_set_status", shared(&[Files])),
    ("comment_delete", shared(&[Files])),
    ("comment_record_routing", shared(&[Dispatch])),
    // ── studio threads ───────────────────────────────────────────────────
    ("studio_thread_get", shared(&[Sessions])),
    ("studio_thread_list_recent", shared(&[Sessions])),
    ("studio_message_list", shared(&[Sessions])),
    ("studio_thread_get_or_create", shared(&[Dispatch])),
    ("studio_message_append", shared(&[Dispatch])),
    ("studio_thread_delete", owner(&[Dispatch])),
    // ── claude config / detect ───────────────────────────────────────────
    ("claude_config_load", owner(&[Files])),
    ("claude_config_read_file", owner(&[Files])),
    ("claude_config_resolve_cascade", owner(&[Files])),
    ("detect_agent_config", owner(&[Files])),
    ("list_claude_projects", owner(&[Files])),
    ("list_agent_projects", owner(&[Files])),
    ("claude_asset_list_pins", owner(&[Files])),
    ("engine_layout", owner(&[])),
    ("terminal_detect_shells", owner(&[])),
    ("claude_asset_pin", owner(&[Settings])),
    ("claude_asset_unpin", owner(&[Settings])),
    // ── transcripts ──────────────────────────────────────────────────────
    ("claude_list_sessions", shared(&[Sessions])),
    ("claude_read_jsonl", shared(&[Sessions])),
    ("claude_session_list", shared(&[Sessions])),
    // ── Ngwa vault ───────────────────────────────────────────────────────
    ("claude_store_list", shared(&[Files])),
    ("oba_dependents", shared(&[Files])),
    ("oba_missing_requires", shared(&[Files])),
    ("claude_primitive_enable_for", shared(&[Install])),
    ("claude_primitive_disable_for", shared(&[Install])),
    ("claude_primitive_remove_for", shared(&[Install])),
    ("claude_store_import", owner(&[Install])),
    ("claude_primitive_enable", owner(&[Install])),
    ("claude_primitive_disable", owner(&[Install])),
    ("claude_primitive_remove", owner(&[Install])),
    ("claude_primitive_copy", owner(&[Install])),
    ("claude_primitive_move", owner(&[Install])),
    ("claude_primitive_copy_batch", owner(&[Install])),
    ("oba_backfill_registry", owner(&[Install])),
    ("oba_forget", owner(&[Install])),
    ("oba_safe_delete", owner(&[Install])),
    ("oba_set_auto_update", owner(&[Install])),
    ("oba_relink_dependents", owner(&[Install])),
    ("oba_unlink_one", owner(&[Install])),
    // ── actions / trust ──────────────────────────────────────────────────
    ("actions_read_files", shared(&[Files])),
    ("actions_trust_status", shared(&[Files])),
    ("actions_write", shared(&[Settings])),
    ("keybindings_write", shared(&[Settings])),
    ("actions_trust_grant", owner(&[Settings])),
    ("actions_trust_revoke", owner(&[Settings])),
    // ── approve gate ─────────────────────────────────────────────────────
    ("pa_actions_list", owner(&[Sessions])),
    ("pa_actions_pause", owner(&[Approve])),
    ("pa_actions_update", owner(&[Approve])),
    ("pa_actions_commit", owner(&[Approve])),
    ("pa_actions_retry", owner(&[Approve])),
    ("pa_actions_reject", owner(&[Approve])),
    // ── pkg diagnostics ──────────────────────────────────────────────────
    ("pkg_permission_violations_list", owner(&[Settings])),
    ("pkg_permission_violations_clear", owner(&[Settings])),
    ("pkg_db_diag", owner(&[Settings])),
    // ── atelier / git ────────────────────────────────────────────────────
    ("atelier_file_read", shared(&[Files])),
    ("action_git_branch", shared(&[Files])),
    ("atelier_file_write", shared(&[Files, Dispatch])),
    // ── slice 8 ──────────────────────────────────────────────────────────
    ("pin_screenshot_write", shared(&[Files])),
    ("scaffold_agent_config", owner(&[Settings])),
    ("pkg_preview_manifest", owner(&[Files])),
    ("pkg_discover_workspace", owner(&[Files])),
    ("pkg_scaffold", owner(&[Install])),
    // ── G-ACCESS (§9.1; registered skeleton-first by WP-74a) ─────────────
    ("access_status", ACCESS),
    ("access_devices_list", ACCESS),
    ("access_device_set_tier", ACCESS),
    ("access_device_revoke", ACCESS),
    ("access_pair_begin", ACCESS),
    ("access_pair_cancel", ACCESS),
    ("access_pair_pending", ACCESS),
    ("access_pair_decide", ACCESS),
    ("access_routing_get", ACCESS),
    ("access_routing_set", ACCESS),
    ("access_members_list", ACCESS),
    ("access_member_set_role", ACCESS),
    ("access_member_remove", ACCESS),
    ("access_member_restore", ACCESS),
    ("access_policy_get", ACCESS),
    ("access_policy_set_cell", ACCESS),
    ("access_policy_set_owner_approval", ACCESS),
    ("access_invite_issue", ACCESS),
    ("access_invite_revoke", ACCESS),
    ("access_shares_list", ACCESS),
    ("access_audit_list", ACCESS),
    ("access_audit_verify", ACCESS),
    ("access_audit_export", ACCESS),
    ("access_audit_record_local", ACCESS),
    ("access_audit_reseal", ACCESS),
    ("permission_decide", shared(&[Approve])),
    ("notifications_record_access", INTERNAL),
    ("share_project_info", INTERNAL),
    ("permission_relay_put", OPERATOR),
    ("permission_relay_take", OPERATOR),
    ("permission_relay_resolve", OPERATOR),
    // ── WP-21 (W3) arms: append on rebase, defaults from §1.6 ────────────
    // Not arms on WP-74a's base, so A-1 forbids them here until WP-21's
    // arms land; the orchestrator (or the rebase) uncomments the ones
    // WP-21 actually serves, confirming each name:
    // ("app_lock_status", owner(&[])),
    // ("app_lock_clear_secret", owner(&[Settings])),
    // ("app_lock_configure", owner(&[Settings])),
    // ("app_lock_lock", owner(&[Settings])),
    // ("app_lock_set_secret", owner(&[Settings])),
    // ("app_lock_touch", owner(&[Settings])),
    // ("app_lock_unlock", owner(&[Settings])),
    // ("app_lock_unlock_biometric", owner(&[Settings])),
    // ("secrets_lock_state", owner(&[])),
    // ("secrets_lock", owner(&[Settings])),
    // ("secrets_unlock", owner(&[Settings, Secrets])),
    // ("secrets_set_passphrase", owner(&[Settings, Secrets])),
    // ("fs_roots_add", OPERATOR),
    // ("fs_roots_remove", OPERATOR),
    // ("fs_roots_reset", OPERATOR),
];

/// What `cmd` needs: its row, or the §1.6 rule 2 fail-closed default.
pub fn requirement(cmd: &str) -> Requirement {
    RPC_REQUIREMENTS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, r)| *r)
        .unwrap_or(Requirement::UNMAPPED)
}

/// Non-RPC routes (§1.6 "Non-RPC routes"). `None` for a route this table
/// doesn't govern (public routes, the SPA).
pub fn route_requirement(path: &str) -> Option<Requirement> {
    if path == "/api/shutdown" {
        return Some(OPERATOR);
    }
    if path.starts_with("/ws/pty/") {
        return Some(owner(&[Sessions]));
    }
    if path.starts_with("/ws/chat/") {
        return Some(shared(&[Sessions]));
    }
    if path == "/ws/fs" {
        return Some(shared(&[Files]));
    }
    // Any other socket path (an encoded `/ws/%70ty/…`, a future socket):
    // fail closed, as an unmapped RPC is (§1.6 rule 2).
    if path == "/ws" || path.starts_with("/ws/") {
        return Some(Requirement::UNMAPPED);
    }
    if path == "/pkgs" || path.starts_with("/pkgs/") {
        return Some(shared(&[Files]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const RPC_RS: &str = include_str!("../server/rpc.rs");

    /// A-1: every `rpc.rs` arm literal has exactly one entry, and no entry
    /// names a non-arm. Reuses the parity ratchet's arm lexer (a naive regex
    /// is not allowed, §1.6 rule 1).
    #[test]
    fn every_rpc_arm_is_mapped() {
        let served = crate::server::parity::served_verbs(RPC_RS);
        assert!(served.len() > 150, "arm lexer drifted: {}", served.len());

        let mut seen = BTreeSet::new();
        let dups: Vec<&str> = RPC_REQUIREMENTS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| !seen.insert(*n))
            .collect();
        assert!(dups.is_empty(), "duplicate RPC_REQUIREMENTS rows: {dups:?}");

        let mapped: BTreeSet<String> = seen.into_iter().map(str::to_string).collect();
        let unmapped: Vec<&String> = served.difference(&mapped).collect();
        assert!(
            unmapped.is_empty(),
            "rpc.rs arms with no RPC_REQUIREMENTS row (append one in the same change, \
             G-ACCESS §1.6 rule 1): {unmapped:?}"
        );
        let stale: Vec<&String> = mapped.difference(&served).collect();
        assert!(
            stale.is_empty(),
            "RPC_REQUIREMENTS rows that name no rpc.rs arm: {stale:?}"
        );
    }

    #[test]
    fn unmapped_commands_fail_closed() {
        let r = requirement("no_such_arm");
        assert_eq!(r, Requirement::UNMAPPED);
        assert_eq!(r.caps, super::super::caps::CapSet::ALL);
        assert_eq!(r.class, super::super::caps::ArmClass::Owner);
    }

    /// P-3: PTY, raw DB, secrets, personal settings and the approve gate are
    /// never reachable through a share.
    #[test]
    fn p3_families_are_owner_class() {
        use super::super::caps::ArmClass;
        for (name, req) in RPC_REQUIREMENTS {
            let p3 = name.starts_with("pty_")
                || name.starts_with("db_")
                || name.starts_with("secrets_")
                || name.starts_with("supabase_")
                || name.starts_with("pa_actions_")
                || *name == "settings_get_all"
                || *name == "settings_clear_all";
            if p3 {
                assert_eq!(req.class, ArmClass::Owner, "{name}");
            }
            if req.caps.contains(Cap::Secrets) && req.class != ArmClass::Operator {
                assert_eq!(req.class, ArmClass::Owner, "{name} returns secrets");
            }
        }
    }

    #[test]
    fn non_rpc_routes_match_the_table() {
        use super::super::caps::ArmClass;
        assert_eq!(
            route_requirement("/api/shutdown").unwrap().class,
            ArmClass::Operator
        );
        assert_eq!(route_requirement("/ws/pty/abc"), Some(owner(&[Sessions])));
        assert_eq!(route_requirement("/ws/chat/t1"), Some(shared(&[Sessions])));
        assert_eq!(route_requirement("/ws/fs"), Some(shared(&[Files])));
        assert_eq!(
            route_requirement("/pkgs/x/index.html"),
            Some(shared(&[Files]))
        );
        assert_eq!(route_requirement("/api/health"), None);
        assert_eq!(
            route_requirement("/ws/%70ty/x"),
            Some(Requirement::UNMAPPED)
        );
        assert_eq!(route_requirement("/api/rpc"), None);
    }
}
