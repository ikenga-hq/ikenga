//! Generic read command for per-project Atelier skill files.
//!
//! Atelier skills persist their per-project config under a well-known layout:
//! `<project_root>/.atelier/<skill>/<file>` (e.g. the Tasks pkg's roster lives
//! at `.atelier/skill-tasks/roster.json`, written by `skill-tasks setup`). The
//! shell reads these at iframe-mount time and injects the parsed result into a
//! pkg's `hostContext` (see `06-skill-action-contract.md`). This command is the
//! one generic reader; callers name the `<skill>`/`<file>` — the roster read is
//! just `atelier_file_read(root, "skill-tasks", "roster.json")` on the FE.
//!
//! # Security model
//!
//! The `project_root` argument comes from the shell's own `projects` table
//! (via `useShellStore.getState().projects`), not from untrusted pkg input.
//! The `.atelier` prefix is a compile-time constant, and both the `skill` and
//! `file` segments are validated (`is_safe_segment`) to reject path traversal
//! (`..`) and embedded separators, so a caller cannot read outside
//! `<project_root>/.atelier/`. This is analogous to how `agent_ops` reads
//! `$HOME/.agent-ops/…` without routing through the FS allowlist — privileged
//! intra-app reads keyed to well-known project data, not arbitrary paths.
//!
//! # Return value
//!
//! Returns the raw file contents on success. Returns `None` when the file is
//! absent, the project root is `None`/empty, a segment is unsafe, or any IO
//! error occurs. The caller (FE) is responsible for parsing and validation; an
//! absent or malformed file causes the consuming pkg to fall back to its static
//! defaults.
//!
//! WP-19 slice 6: the bodies live in `server::shared::atelier` (as
//! `Reach::Follow`, byte-identical to before), which the headless daemon
//! serves too — confined there to its fs allowlist and the canonical root,
//! since a remote caller's `project_root` has no such provenance.

use crate::server::shared::atelier::{self, Reach};

#[cfg(test)]
use crate::server::shared::atelier::is_safe_segment;

/// Read `<project_root>/.atelier/<skill>/<file>`.
///
/// `project_root` is the project's `root_path` column from the `projects`
/// table — an absolute path string, or `None` for the default project that has
/// no root configured. Returns `None` on any error (absent file, permission
/// denied, unset root, unsafe segment) so the FE falls back silently.
#[tauri::command]
pub async fn atelier_file_read(
    project_root: Option<String>,
    skill: String,
    file: String,
) -> Option<String> {
    // `Reach::Follow` never errs: every failure is already `Ok(None)`.
    atelier::read(project_root.as_deref(), &skill, &file, Reach::Follow).unwrap_or(None)
}

/// Atomically write `<project_root>/.atelier/<skill>/<file>` with `content`.
///
/// The write-path sibling of [`atelier_file_read`]. Same security model:
/// `project_root` comes from the shell's own `projects` table (not untrusted pkg
/// input), the `.atelier` prefix is a compile-time constant, and both `skill`
/// and `file` segments are `is_safe_segment`-validated to reject traversal
/// (`..`) and embedded separators — so a caller can never write outside
/// `<project_root>/.atelier/`. Parent directories are created as needed.
///
/// The write is atomic: `content` lands in a uniquely-named sibling temp file
/// which is then `rename`d over the target. Rename is atomic on the same
/// filesystem, so a concurrent [`atelier_file_read`] observes either the old
/// bytes or the new bytes — never a half-written file.
///
/// Unlike the reader (which swallows every error into `None` so consumers fall
/// back to defaults), write failures are surfaced as `Err(String)`: the setup
/// surface must know whether the confirm-write actually landed. On success
/// returns the written absolute path.
///
/// This command is intentionally generic — it writes whatever bytes it is given.
/// Envelope validation (`skill` / `template_version` / `settings`) and the
/// `configured_at` stamp are the caller's (the setup surface's) responsibility,
/// mirroring how the reader leaves parsing to the FE.
#[tauri::command]
pub async fn atelier_file_write(
    project_root: Option<String>,
    skill: String,
    file: String,
    content: String,
) -> Result<String, String> {
    atelier::write(
        project_root.as_deref(),
        &skill,
        &file,
        &content,
        Reach::Follow,
    )
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_segments_accepted() {
        for seg in ["skill-tasks", "roster.json", "skill-agent-ops", "jobs.json"] {
            assert!(is_safe_segment(seg), "expected safe: {seg:?}");
        }
    }

    #[test]
    fn traversal_and_separators_rejected() {
        for seg in ["", "..", "../etc", "a/b", "a\\b", "..\\x", "foo/..", "."] {
            // `.` alone is harmless but not a real skill/file; only `..` and
            // separators must be rejected. Assert the traversal cases here.
            if seg == "." {
                assert!(is_safe_segment(seg), "bare dot has no traversal: {seg:?}");
            } else {
                assert!(!is_safe_segment(seg), "expected rejected: {seg:?}");
            }
        }
    }

    #[tokio::test]
    async fn none_root_returns_none_without_fs_access() {
        assert!(
            atelier_file_read(None, "skill-tasks".into(), "roster.json".into())
                .await
                .is_none()
        );
        assert!(
            atelier_file_read(Some(String::new()), "skill-tasks".into(), "roster.json".into())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn unsafe_segment_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        assert!(
            atelier_file_read(Some(root.clone()), "..".into(), "roster.json".into())
                .await
                .is_none()
        );
        assert!(
            atelier_file_read(Some(root), "skill-tasks".into(), "../../etc/passwd".into())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn reads_existing_atelier_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".atelier").join("skill-tasks");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("roster.json"), r#"{"ok":true}"#).expect("write");

        let got = atelier_file_read(
            Some(tmp.path().to_string_lossy().to_string()),
            "skill-tasks".into(),
            "roster.json".into(),
        )
        .await;
        assert_eq!(got.as_deref(), Some(r#"{"ok":true}"#));
    }

    // ── write path ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn write_creates_dirs_and_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        let path = atelier_file_write(
            Some(root),
            "skill-mail".into(),
            "manifest.json".into(),
            r#"{"skill":"mail"}"#.into(),
        )
        .await
        .expect("write ok");

        let expected = tmp.path().join(".atelier").join("skill-mail").join("manifest.json");
        assert_eq!(std::path::Path::new(&path), expected);
        assert!(expected.exists(), "file created");
        assert_eq!(
            std::fs::read_to_string(&expected).unwrap(),
            r#"{"skill":"mail"}"#
        );
    }

    #[tokio::test]
    async fn write_overwrites_atomically_no_temp_leftover() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        for body in [r#"{"template_version":1}"#, r#"{"template_version":2}"#] {
            atelier_file_write(
                Some(root.clone()),
                "skill-mail".into(),
                "manifest.json".into(),
                body.into(),
            )
            .await
            .expect("write ok");
        }
        let dir = tmp.path().join(".atelier").join("skill-mail");
        assert_eq!(
            std::fs::read_to_string(dir.join("manifest.json")).unwrap(),
            r#"{"template_version":2}"#,
            "second write wins"
        );
        // No `.manifest.json.tmp-*` sidecar survives a successful commit.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no temp file leftover: {leftovers:?}");
    }

    #[tokio::test]
    async fn write_allows_sidecar_files_in_same_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        atelier_file_write(
            Some(root.clone()),
            "skill-mail".into(),
            "manifest.json".into(),
            "{}".into(),
        )
        .await
        .expect("manifest write");
        atelier_file_write(
            Some(root),
            "skill-mail".into(),
            "roster.json".into(),
            r#"{"members":[]}"#.into(),
        )
        .await
        .expect("sidecar write");
        let dir = tmp.path().join(".atelier").join("skill-mail");
        assert!(dir.join("manifest.json").exists());
        assert!(dir.join("roster.json").exists());
    }

    #[tokio::test]
    async fn write_rejects_unsafe_segment_without_touching_fs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        assert!(
            atelier_file_write(Some(root.clone()), "..".into(), "manifest.json".into(), "{}".into())
                .await
                .is_err()
        );
        assert!(
            atelier_file_write(
                Some(root),
                "skill-mail".into(),
                "../../etc/passwd".into(),
                "pwned".into()
            )
            .await
            .is_err()
        );
        // The traversal target must not exist.
        assert!(!tmp.path().parent().unwrap().join("etc").join("passwd").exists());
    }

    #[tokio::test]
    async fn write_none_or_empty_root_errs() {
        assert!(
            atelier_file_write(None, "skill-mail".into(), "manifest.json".into(), "{}".into())
                .await
                .is_err()
        );
        assert!(
            atelier_file_write(
                Some(String::new()),
                "skill-mail".into(),
                "manifest.json".into(),
                "{}".into()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().to_string();
        let body = r#"{"skill":"mail","template_version":1,"settings":{"inbox_label":"INBOX"}}"#;
        atelier_file_write(
            Some(root.clone()),
            "skill-mail".into(),
            "manifest.json".into(),
            body.into(),
        )
        .await
        .expect("write ok");
        let got =
            atelier_file_read(Some(root), "skill-mail".into(), "manifest.json".into()).await;
        assert_eq!(got.as_deref(), Some(body));
    }
}
