//! G-SEATS name grammar (§1.2, §3.1): the seat-name and project-slug rules.
//!
//! Moved out of `iyke::seats` (desktop-only) in WP-19 slice 5a, unchanged, so
//! the G-ACTIONS schema (`server::shared::actions::schema`), which checks a
//! `chi` run's `seat` value against them, compiles into the headless daemon.
//! `iyke::seats` re-exports both, so the seat store and the schema still share
//! one copy of the grammar.

/// P-1: seat names are 1–32 chars.
const SEAT_NAME_MAX: usize = 32;
/// `projects.rs::validate_slug` bounds.
const PROJECT_SLUG_MAX: usize = 64;

/// §1.2: `[a-z0-9] ( [a-z0-9-]{0,30} [a-z0-9] )?` — 1–32 chars, lowercase
/// ASCII letters, digits and `-`, starting and ending with a letter or digit.
/// Every valid seat name is also a valid scratchpad name.
pub(crate) fn validate_seat_name(name: &str) -> Result<(), String> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > SEAT_NAME_MAX {
        return Err(format!("a seat name is 1–{SEAT_NAME_MAX} characters"));
    }
    let edge = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !bytes.iter().all(|&b| edge(b) || b == b'-') {
        return Err("a seat name uses only lowercase letters, digits and '-'".to_string());
    }
    if !edge(bytes[0]) || !edge(bytes[bytes.len() - 1]) {
        return Err("a seat name starts and ends with a lowercase letter or digit".to_string());
    }
    Ok(())
}

/// Shared copy of `commands/projects.rs::validate_slug` (private there):
/// 1–64 chars, first `[a-z0-9]`, then `[a-z0-9_-]`. The test
/// `project_slug_copy_agrees_with_projects_rs` (in `iyke::seats`) holds the
/// two together (§3.2).
pub(crate) fn validate_project_slug(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > PROJECT_SLUG_MAX {
        return Err(format!(
            "invalid project id length: {} (1..={PROJECT_SLUG_MAX})",
            id.len()
        ));
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(format!(
            "invalid project id {id:?}: must start with [a-z0-9]"
        ));
    }
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return Err(format!(
                "invalid project id {id:?}: only [a-z0-9_-] allowed after first char"
            ));
        }
    }
    Ok(())
}
