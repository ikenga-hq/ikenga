//! The `run_finished` / `run_failed` producer (WP-40), moved out of the
//! desktop-only `crate::notifications::producers` (WP-P10) so the headless
//! daemon's Chi runs record the same notification the desktop's do. Pure
//! builder: the writer is `super::record_with_db`.
//!
//! The text helpers (`truncate`, `first_line`, …) came with it; the desktop
//! producers import them from here.

use serde_json::{json, Value};

use super::{Coalesce, NewNotification, NotificationKind};

pub const SOURCE_CHI: &str = "chi";

pub(crate) const TITLE_MAX: usize = 120;
pub(crate) const BODY_MAX: usize = 240;

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let trimmed: String = s.chars().take(max).collect();
        format!("{trimmed}…")
    }
}

pub(crate) fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Last path component, accepting both separators (Windows cwd values).
pub(crate) fn basename(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

pub(crate) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

pub(crate) fn join_parts(parts: &[Option<String>]) -> Option<String> {
    let joined: Vec<&str> = parts
        .iter()
        .filter_map(|p| p.as_deref())
        .filter(|p| !p.is_empty())
        .collect();
    if joined.is_empty() {
        None
    } else {
        Some(truncate(&joined.join(" · "), BODY_MAX))
    }
}

/// A Chi run reaching a terminal state (`chi_cache` status `done` /
/// `failed`). `cancelled` (a human did it) and any non-terminal status
/// produce nothing. No artifacts; see [`run_terminal_with_artifacts`].
pub fn run_terminal(
    run_id: &str,
    status: &str,
    engine_id: &str,
    brief: Option<&str>,
    cwd: Option<&str>,
    error: Option<&str>,
) -> Option<NewNotification> {
    run_terminal_with_artifacts(run_id, status, engine_id, brief, cwd, error, None)
}

/// `(count, first path)` of a `chi_cache.artifacts` value (a JSON array of
/// `{ path, mime, producedBy }`).
fn artifact_summary(artifacts: Option<&Value>) -> (usize, Option<String>) {
    let Some(items) = artifacts.and_then(Value::as_array) else {
        return (0, None);
    };
    let first = items
        .iter()
        .find_map(|a| a.get("path").and_then(Value::as_str))
        .map(str::to_string);
    (items.len(), first)
}

/// [`run_terminal`] plus what the run produced: the action carries
/// `artifactCount` and `firstArtifactPath` (D-07 "Opens what it produced" /
/// "Open artifact"), and the body leads with "N artifacts".
pub fn run_terminal_with_artifacts(
    run_id: &str,
    status: &str,
    engine_id: &str,
    brief: Option<&str>,
    cwd: Option<&str>,
    error: Option<&str>,
    artifacts: Option<&Value>,
) -> Option<NewNotification> {
    let (artifact_count, first_artifact) = artifact_summary(artifacts);
    let produced = match artifact_count {
        0 => None,
        1 => Some("1 artifact".to_string()),
        n => Some(format!("{n} artifacts")),
    };
    let kind = match status {
        "done" => NotificationKind::RunFinished,
        "failed" => NotificationKind::RunFailed,
        _ => return None,
    };
    let label = brief
        .map(first_line)
        .filter(|l| !l.is_empty())
        .map(|l| truncate(l, 60))
        .unwrap_or_else(|| format!("Chi run {}", short_id(run_id)));
    let (title, body) = match kind {
        NotificationKind::RunFinished => (
            format!("{label} finished"),
            join_parts(&[
                produced.clone(),
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
        _ => (
            format!("{label} failed"),
            join_parts(&[
                error.map(|e| truncate(first_line(e), 120)),
                produced.clone(),
                Some(engine_id.to_string()),
                cwd.map(|c| basename(c).to_string()),
            ]),
        ),
    };
    Some(NewNotification {
        kind,
        title: truncate(&title, TITLE_MAX),
        body,
        action: Some(json!({
            "kind": "open.chi_run",
            "runId": run_id,
            "status": status,
            "artifactCount": artifact_count,
            "firstArtifactPath": first_artifact,
        })),
        source: SOURCE_CHI.into(),
        // Status in the key: a resumed run that failed and then finished must
        // not fold its `run_finished` into the unread `run_failed` row.
        dedupe_key: Some(format!("run:{run_id}:{status}")),
        coalesce: Coalesce::WhileUnread,
    })
}
