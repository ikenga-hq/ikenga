//! Where the two files live and how one is read (G-ACTIONS §1.1), on the
//! G-SETTINGS substrate: `settings::scope::ikenga_file` for the paths,
//! `read_document_bytes` / `write_document_bytes_atomic` for recovery,
//! link rejection and the atomic write.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::settings::scope::{
    ikenga_file, read_document_bytes, write_document_bytes_atomic, IkengaDocument,
};
use crate::settings::SettingsScope;

use super::schema::{self, FileKind, ValidateContext, Validation};

pub fn personal_file(home: &Path, kind: FileKind) -> PathBuf {
    ikenga_file(home, kind.file_name())
}

pub fn project_file(root: &Path, kind: FileKind) -> PathBuf {
    ikenga_file(root, kind.file_name())
}

/// The `.ikenga/` directory a file sits in (what the watcher watches).
pub fn ikenga_dir(root: &Path) -> PathBuf {
    project_file(root, FileKind::Actions)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join(".ikenga"))
}

/// Recovery-file check for the substrate: a `.recovery` file (or the live
/// file next to it) is sound when it is strict JSON with a supported
/// `version`. Field-level validity is the read path's to report, not a
/// reason to leave a crash-orphaned backup in place.
fn validate_recoverable(bytes: &[u8]) -> Result<(), String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("not strict JSON: {e}"))?;
    match value.get("version").and_then(Value::as_u64) {
        Some(version) if version <= schema::SCHEMA_VERSION => Ok(()),
        Some(version) => Err(format!("version {version} is newer than supported")),
        None => Err("version is missing".into()),
    }
}

pub const ACTIONS_DOCUMENT: IkengaDocument = IkengaDocument {
    label: "actions",
    validate: validate_recoverable,
};

/// One file as read from disk.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileState {
    pub kind: FileKind,
    pub scope: SettingsScope,
    pub path: String,
    /// The file exists on disk. Absent = empty (§1.1).
    pub present: bool,
    /// The document in force: the file as read when it is valid; otherwise
    /// the last valid document this session read from the same path
    /// (`stale: true`), or `None`. A malformed file is never wiped.
    pub document: Option<Value>,
    pub stale: bool,
    pub validation: Validation,
    /// A read failure outside validation (linked path, I/O).
    pub error: Option<String>,
}

/// A read before the last-valid fallback is applied.
pub struct RawRead {
    pub present: bool,
    pub document: Option<Value>,
    pub validation: Validation,
    pub error: Option<String>,
}

pub fn read_raw(path: &Path, ctx: ValidateContext<'_>) -> RawRead {
    match read_document_bytes(path, ACTIONS_DOCUMENT) {
        Ok(None) => RawRead {
            present: false,
            document: None,
            validation: Validation::default(),
            error: None,
        },
        Ok(Some(bytes)) => {
            let (document, validation) = schema::validate_bytes(ctx, &bytes);
            RawRead {
                present: true,
                document,
                validation,
                error: None,
            }
        }
        Err(error) => RawRead {
            present: path.exists(),
            document: None,
            validation: Validation::default(),
            error: Some(error),
        },
    }
}

/// Only the parsed documents that validated — used to collect the user
/// action ids a keybindings file may name.
pub fn valid_document(read: &RawRead) -> Option<&Value> {
    if read.error.is_none() && read.validation.is_ok() {
        read.document.as_ref()
    } else {
        None
    }
}

pub fn collect_action_ids<'a>(documents: impl IntoIterator<Item = &'a Value>) -> HashSet<String> {
    documents
        .into_iter()
        .flat_map(schema::defined_action_ids)
        .map(str::to_string)
        .collect()
}

/// Whether the file exists, after the substrate's link check: a symlinked /
/// reparse-point file or `.ikenga/` parent is an error (a dangling link
/// included). For callers that hand the path to something that follows
/// links, such as the OS opener.
pub fn present_unlinked(path: &Path) -> Result<bool, String> {
    read_document_bytes(path, ACTIONS_DOCUMENT).map(|bytes| bytes.is_some())
}

/// Validates and writes one document. Returns the validation; on any error
/// nothing is written and the file is untouched.
pub fn write_validated(
    path: &Path,
    ctx: ValidateContext<'_>,
    document: &Value,
) -> Result<Validation, String> {
    let validation = schema::validate_document(ctx, document);
    if !validation.is_ok() {
        return Ok(validation);
    }
    let bytes = schema::document_bytes(ctx.kind, document)?;
    write_document_bytes_atomic(path, &bytes, ACTIONS_DOCUMENT)?;
    Ok(validation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(kind: FileKind, scope: SettingsScope) -> ValidateContext<'static> {
        ValidateContext {
            kind,
            scope,
            user_action_ids: None,
        }
    }

    #[test]
    fn paths_share_the_settings_dot_directory() {
        assert_eq!(
            personal_file(Path::new("/h"), FileKind::Actions),
            Path::new("/h/.ikenga/actions.json")
        );
        assert_eq!(
            project_file(Path::new("/p"), FileKind::Keybindings),
            Path::new("/p/.ikenga/keybindings.json")
        );
        assert_eq!(ikenga_dir(Path::new("/p")), Path::new("/p/.ikenga"));
    }

    #[test]
    fn write_then_read_round_trips() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = project_file(temp.path(), FileKind::Keybindings);
        let c = ctx(FileKind::Keybindings, SettingsScope::Project);
        let document = json!({ "version": 1, "bindings": [ { "key": "mod+shift+r", "command": "refresh-pulse" } ] });
        let v = write_validated(&path, c, &document).unwrap();
        assert!(v.is_ok());
        let read = read_raw(&path, c);
        assert!(read.present);
        let mut expected = document.clone();
        expected["$schema"] = json!("urn:ikenga:keybindings:v1");
        assert_eq!(read.document.unwrap(), expected);
    }

    #[test]
    fn an_invalid_write_leaves_the_file_untouched() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = project_file(temp.path(), FileKind::Keybindings);
        let c = ctx(FileKind::Keybindings, SettingsScope::Project);
        let good = json!({ "version": 1, "bindings": [] });
        write_validated(&path, c, &good).unwrap();
        let before = std::fs::read(&path).unwrap();
        let os_rule = json!({ "version": 1, "bindings": [ { "key": "alt+space", "command": "os.summon", "scope": "os" } ] });
        let v = write_validated(&path, c, &os_rule).unwrap();
        assert_eq!(v.errors[0].code, "E_OS_LAYER");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_malformed_file_is_surfaced_not_wiped() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = personal_file(temp.path(), FileKind::Actions);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ \"version\": 1, ").unwrap();
        let read = read_raw(&path, ctx(FileKind::Actions, SettingsScope::Personal));
        assert!(read.present);
        assert!(read.document.is_none());
        assert_eq!(read.validation.errors[0].code, "E_JSON");
        assert_eq!(std::fs::read(&path).unwrap(), b"{ \"version\": 1, ");
    }

    #[test]
    fn an_absent_file_is_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let read = read_raw(
            &personal_file(temp.path(), FileKind::Actions),
            ctx(FileKind::Actions, SettingsScope::Personal),
        );
        assert!(!read.present);
        assert!(read.document.is_none());
        assert!(read.validation.is_ok());
        assert!(read.error.is_none());
        assert!(!present_unlinked(&personal_file(temp.path(), FileKind::Actions)).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn linked_files_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::TempDir::new().unwrap();
        let victim = temp.path().join("victim.json");
        std::fs::write(&victim, br#"{"version":1}"#).unwrap();
        let path = project_file(&temp.path().join("p"), FileKind::Actions);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&victim, &path).unwrap();
        let c = ctx(FileKind::Actions, SettingsScope::Project);
        assert!(read_raw(&path, c).error.is_some());
        assert!(write_validated(&path, c, &json!({ "version": 1 })).is_err());
        assert!(present_unlinked(&path).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), br#"{"version":1}"#);
        // A dangling link is refused too, not treated as absent.
        let dangling = project_file(&temp.path().join("q"), FileKind::Actions);
        std::fs::create_dir_all(dangling.parent().unwrap()).unwrap();
        symlink(temp.path().join("missing.command"), &dangling).unwrap();
        assert!(present_unlinked(&dangling).is_err());
    }
}
