//! Launch options shared by every `claude` spawn the shell makes (WP-11):
//! the model a role resolves to, `--append-system-prompt`, and the
//! `CLAUDE_CODE_PLUGIN_DIRS` env that loads plugin folders into the session.
//!
//! Pure, AppHandle-free helpers so the streaming chat spawn
//! (`claude::session::spawn_streaming`) and its tests share one
//! implementation, and the headless build compiles them. When none of the
//! options is set, every helper returns nothing, so the spawn is unchanged.

use std::ffi::OsString;

use super::model_catalog::default_model_for_role;

/// Env var Claude Code reads plugin folders from, for sessions where no
/// `--plugin-dir` flag can be given. One or more absolute paths (`~`
/// allowed) joined by the platform's path-list separator (`:` on Unix, `;`
/// on Windows).
pub const PLUGIN_DIRS_ENV: &str = "CLAUDE_CODE_PLUGIN_DIRS";

/// The model to pass as `--model`: an explicit, non-empty `model` wins;
/// otherwise the catalog default for `role`. `None` (no flag) when neither is
/// set or the role is unknown, so Claude Code's own default applies.
pub fn resolve_model(model: Option<&str>, role: Option<&str>) -> Option<String> {
    if let Some(m) = model.filter(|m| !m.is_empty()) {
        return Some(m.to_string());
    }
    role.and_then(default_model_for_role).map(str::to_string)
}

/// `["--append-system-prompt", s]` when `s` is set and non-empty.
pub fn append_system_prompt_args(prompt: Option<&str>) -> Vec<String> {
    match prompt.filter(|p| !p.is_empty()) {
        Some(p) => vec!["--append-system-prompt".to_string(), p.to_string()],
        None => Vec::new(),
    }
}

/// `(CLAUDE_CODE_PLUGIN_DIRS, joined)` for a non-empty list. Empty entries
/// are dropped; an entry that itself contains the separator can't be joined
/// and is an error rather than a silently split path.
pub fn plugin_dirs_env(dirs: &[String]) -> Result<Option<(&'static str, OsString)>, String> {
    let dirs: Vec<&str> = dirs
        .iter()
        .map(String::as_str)
        .filter(|d| !d.is_empty())
        .collect();
    if dirs.is_empty() {
        return Ok(None);
    }
    let joined = std::env::join_paths(&dirs).map_err(|e| format!("{PLUGIN_DIRS_ENV}: {e}"))?;
    Ok(Some((PLUGIN_DIRS_ENV, joined)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_model_wins_over_role() {
        assert_eq!(
            resolve_model(Some("claude-haiku-4-5"), Some("plan")).as_deref(),
            Some("claude-haiku-4-5")
        );
    }

    #[test]
    fn role_resolves_from_catalog_when_model_unset() {
        assert_eq!(
            resolve_model(None, Some("chi")).as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            resolve_model(None, Some("pane")).as_deref(),
            Some("claude-sonnet-5-5")
        );
        assert_eq!(
            resolve_model(None, Some("plan")).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            resolve_model(Some(""), Some("plan")).as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn nothing_set_means_no_model_flag() {
        assert_eq!(resolve_model(None, None), None);
        assert_eq!(resolve_model(None, Some("unknown-role")), None);
    }

    #[test]
    fn append_system_prompt_only_when_set() {
        assert!(append_system_prompt_args(None).is_empty());
        assert!(append_system_prompt_args(Some("")).is_empty());
        assert_eq!(
            append_system_prompt_args(Some("be terse")),
            vec!["--append-system-prompt".to_string(), "be terse".to_string()]
        );
    }

    #[test]
    fn plugin_dirs_join_with_the_platform_separator() {
        assert_eq!(plugin_dirs_env(&[]).unwrap(), None);
        assert_eq!(plugin_dirs_env(&[String::new()]).unwrap(), None);
        let (k, v) = plugin_dirs_env(&["/a/one".into(), "/b/two".into()])
            .unwrap()
            .unwrap();
        assert_eq!(k, "CLAUDE_CODE_PLUGIN_DIRS");
        let sep = if cfg!(windows) { ";" } else { ":" };
        assert_eq!(v, OsString::from(format!("/a/one{sep}/b/two")));
    }

    #[test]
    fn plugin_dir_containing_the_separator_is_an_error() {
        let bad = if cfg!(windows) { "C:\\a;b" } else { "/a:b" };
        assert!(plugin_dirs_env(&[bad.into()]).is_err());
    }
}
