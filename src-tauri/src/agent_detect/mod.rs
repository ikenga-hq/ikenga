//! First-run wizard discovery: system + agent + agent-config inventory.
//!
//! All three Tauri commands are async (the agent scan runs subprocesses)
//! and return rich JSON-serializable structs the wizard renders verbatim.

pub use crate::server::shared::agents;
// Moved to the ungated `server::shared::agent_config` (WP-19 slice 5b); the
// daemon's `detect_agent_config` arm counts with it.
pub use crate::server::shared::agent_config as config_claude;
#[allow(unused_imports)]
pub use crate::server::shared::known;
// Moved to the ungated `server::shared::agent_scaffold` (WP-19 slice 8); the
// daemon's `scaffold_agent_config` arm runs it confined.
pub use crate::server::shared::agent_scaffold as scaffold;
pub mod system;

use std::path::PathBuf;

use tauri::Manager;

pub use agents::DetectedAgent;
pub use config_claude::AgentConfigInventory;
pub use scaffold::{ScaffoldRequest, ScaffoldResponse};
pub use system::SystemReport;

#[tauri::command]
pub async fn detect_system(app: tauri::AppHandle) -> Result<SystemReport, String> {
    let dir: PathBuf = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?;
    let backend = match app.try_state::<crate::commands::secrets::SecretsLock>() {
        Some(lock) => system::SecretsBackend::from_probe(lock.probe(&app)),
        None => system::SecretsBackend::NoBackend(
            "the secrets service is not running in this process".into(),
        ),
    };
    Ok(system::build_report(dir, backend))
}

#[tauri::command]
pub async fn detect_agents() -> Result<Vec<DetectedAgent>, String> {
    Ok(agents::detect_all().await)
}

#[tauri::command]
pub async fn detect_agent(agent_id: String) -> Result<Option<DetectedAgent>, String> {
    Ok(agents::detect_by_id(&agent_id).await)
}

#[tauri::command]
pub async fn detect_agent_config(
    agent_id: String,
    root_path: String,
) -> Result<AgentConfigInventory, String> {
    Ok(config_claude::build_inventory(&agent_id, &root_path))
}

pub use crate::server::shared::agent_projects::*;

/// Scan `~/.claude/projects/` (and WSL on Windows) for project session directories.
#[tauri::command]
pub async fn list_claude_projects() -> Result<Vec<ClaudeProjectEntry>, String> {
    Ok(list_claude_projects_in(
        crate::platform::home_dir().as_deref(),
    ))
}

/// Generic project/conversation history lister across supported AI agents.
#[tauri::command]
pub async fn list_agent_projects(agent_id: String) -> Result<Vec<ClaudeProjectEntry>, String> {
    Ok(list_agent_projects_in(
        &agent_id,
        crate::platform::home_dir().as_deref(),
    ))
}

/// Phase 6 — agent-config scaffolder. Lays down the starter set of
/// agents/skills/commands for `provider` under `<root_path>/.claude/` (or
/// the provider's equivalent config dir). `mode` selects conflict
/// behaviour: `augment` (default, only writes missing files), `replace`
/// (overwrites everything), or `skip_conflicts` (same as augment but the
/// response records each skipped path so the wizard can show counts).
///
/// Backwards-compatible with the Phase 4 wrapper signature — it didn't
/// pass `mode`, so an absent value falls back to `augment` inside
/// `scaffold::scaffold`.
#[tauri::command]
pub async fn scaffold_agent_config(
    provider: String,
    root_path: String,
    profile: String,
    mode: Option<String>,
) -> Result<ScaffoldResponse, String> {
    scaffold::scaffold(ScaffoldRequest {
        provider,
        root_path,
        profile,
        mode,
    })
}

#[cfg(test)]
mod claude_slug_tests {
    use super::*;
    use std::collections::HashSet;

    fn probe<'a>(set: &'a HashSet<&'static str>) -> impl Fn(&str) -> bool + 'a {
        move |p| set.contains(p)
    }

    #[test]
    fn naive_decoder_replaces_all_dashes() {
        // No FS context — pure transform.
        assert_eq!(
            decode_claude_slug_naive("-Users-alice-work-stuff-proj"),
            "/Users/alice/work/stuff/proj"
        );
        assert_eq!(decode_claude_slug_naive("plain"), "plain");
    }

    #[test]
    fn greedy_decoder_keeps_hyphenated_component_when_disk_says_so() {
        // Hypothetical disk: `/Users/alice/work-stuff/proj` exists, but
        // `/Users/alice/work` does not. The greedy walk should prefer the
        // dash join at the `work` → `stuff` boundary.
        let set: HashSet<&'static str> = [
            "/Users/alice",
            "/Users/alice/work-stuff",
            "/Users/alice/work-stuff/proj",
        ]
        .into_iter()
        .collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-alice-work-stuff-proj", probe(&set));
        assert_eq!(path, "/Users/alice/work-stuff/proj");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_returns_canonical_slashed_form_when_nothing_exists() {
        // No FS info available. Walk defaults to '/' joins for every
        // unknown boundary — same shape as the old naive fallback but
        // produced by the walk itself.
        let set: HashSet<&'static str> = HashSet::new();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-alice-work-stuff-proj", probe(&set));
        assert_eq!(path, "/Users/alice/work/stuff/proj");
        assert!(!verified);
    }

    #[test]
    fn greedy_decoder_preserves_verified_prefix_when_tail_missing() {
        // The regression case from the onboarding screenshot:
        // `~/royalti-co/royalti-client-2.5` doesn't exist on this machine,
        // but `~/royalti-co` does. We must preserve the dash boundary that
        // FS proved, instead of collapsing the whole path to slashes.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-co"].into_iter().collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-co-royalti-client-2-5", probe(&set));
        assert_eq!(path, "/home/x/royalti-co/royalti/client/2/5");
        assert!(!verified);
    }

    #[test]
    fn greedy_decoder_resolves_dot_separator() {
        // Claude Code encodes `.` as `-` in slugs, so `royalti-client-2.5`
        // becomes `-...-royalti-client-2-5`. The greedy walk must try
        // `2.5` as a candidate when the FS knows about it.
        let set: HashSet<&'static str> = [
            "/Users/alice",
            "/Users/alice/work",
            "/Users/alice/work/v2.5",
        ]
        .into_iter()
        .collect();
        let (path, verified) = decode_claude_slug_with_probe("-Users-alice-work-v2-5", probe(&set));
        assert_eq!(path, "/Users/alice/work/v2.5");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_resolves_underscore_separator() {
        // Underscores in original paths get encoded to '-' too. The walk
        // tries '_' once '/' and '-' both fail.
        let set: HashSet<&'static str> = ["/Users/alice", "/Users/alice/my_proj"]
            .into_iter()
            .collect();
        let (path, verified) = decode_claude_slug_with_probe("-Users-alice-my-proj", probe(&set));
        assert_eq!(path, "/Users/alice/my_proj");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_handles_canonical_slash_path() {
        // Every prefix exists with slashes — should hand back the
        // canonical slashed form verbatim.
        let set: HashSet<&'static str> = [
            "/Users",
            "/Users/iyke",
            "/Users/iyke/projects",
            "/Users/iyke/projects/foo",
        ]
        .into_iter()
        .collect();
        let (path, verified) =
            decode_claude_slug_with_probe("-Users-iyke-projects-foo", probe(&set));
        assert_eq!(path, "/Users/iyke/projects/foo");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_prefers_slash_when_both_candidates_exist() {
        // Edge case: both `/a/b` and `/a-b` exist. Slash wins (canonical
        // Claude encoding) so the user lands on the more common case.
        let set: HashSet<&'static str> = ["/a", "/a/b", "/a-b"].into_iter().collect();
        let (path, _) = decode_claude_slug_with_probe("-a-b", probe(&set));
        assert_eq!(path, "/a/b");
    }

    #[test]
    fn greedy_decoder_handles_windows_drive_slugs() {
        let set: HashSet<&'static str> = [
            "C:\\Users",
            "C:\\Users\\nedJamez",
            "C:\\Users\\nedJamez\\Documents",
            "C:\\Users\\nedJamez\\Documents\\royalti-co",
            "C:\\Users\\nedJamez\\Documents\\royalti-co\\royalti-server-v2-6",
        ]
        .into_iter()
        .collect();

        let (path, verified) = decode_claude_slug_with_probe(
            "C--Users-nedJamez-Documents-royalti-co-royalti-server-v2-6",
            probe(&set),
        );
        assert_eq!(
            path,
            "C:\\Users\\nedJamez\\Documents\\royalti-co\\royalti-server-v2-6"
        );
        assert!(verified);
    }
    #[test]
    fn greedy_decoder_absorbs_a_multi_separator_component() {
        // The component itself contains separators the slug flattened, so no
        // prefix of it exists on disk. Stepping one token at a time can never
        // find it; only lookahead over the whole run does.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-co", "/home/x/royalti-co/api-server-v2-6"]
            .into_iter()
            .collect();

        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-co-api-server-v2-6", probe(&set));
        assert_eq!(path, "/home/x/royalti-co/api-server-v2-6");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_prefers_the_longest_component_that_exists() {
        // Both `a-b` and `a-b-c` exist. Longest-first must win, otherwise the
        // walk stops at `a-b` and splits the rest.
        let set: HashSet<&'static str> = ["/r", "/r/a-b", "/r/a-b-c"].into_iter().collect();

        let (path, verified) = decode_claude_slug_with_probe("-r-a-b-c", probe(&set));
        assert_eq!(path, "/r/a-b-c");
        assert!(verified);
    }

    #[test]
    fn greedy_decoder_leaves_mixed_separator_components_unresolved() {
        // Documented limitation: a component mixing `-` and `.` (a real case is
        // `royalti-server-v2.6`) needs a combinatorial search, and the slug
        // alone cannot disambiguate it. The verified prefix is still kept and
        // only the unknown tail defaults to slashes.
        let set: HashSet<&'static str> = ["/home/x", "/home/x/royalti-server-v2.6"]
            .into_iter()
            .collect();

        let (path, verified) =
            decode_claude_slug_with_probe("-home-x-royalti-server-v2-6", probe(&set));
        assert_eq!(path, "/home/x/royalti/server/v2/6");
        assert!(!verified);
    }

}
