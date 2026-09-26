use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const INDEX_FILENAME: &str = "secrets-index.json";
pub const INDEX_PENDING_FILENAME: &str = "secrets-index.pending.json";
pub const ITEM_PREFIX: &str = "ikenga:";

#[derive(Debug, Clone)]
pub struct SecretIndex {
    path: PathBuf,
    names: BTreeSet<String>,
}

impl SecretIndex {
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    names: BTreeSet::new(),
                });
            }
            Err(error) => return Err(format!("inspect secrets index {}: {error}", path.display())),
        }
        reject_link(&path, "secrets index")?;
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("read secrets index {}: {e}", path.display()))?;
        if raw.trim().is_empty() {
            return Err(format!("secrets index is empty: {}", path.display()));
        }
        let values: Vec<String> = serde_json::from_str(&raw)
            .map_err(|e| format!("parse secrets index {}: {e}", path.display()))?;
        let mut names = BTreeSet::new();
        for value in values {
            // Permissive on load: names written by WP-33 and earlier only had
            // to be non-empty and NUL-free. Rejecting them here would make
            // the whole store unloadable (and the values unreachable).
            validate_legacy_name(&value)?;
            names.insert(value);
        }
        Ok(Self { path, names })
    }

    pub fn names(&self) -> Vec<String> {
        self.names.iter().cloned().collect()
    }

    pub fn insert(&mut self, name: &str) -> Result<(), String> {
        validate_legacy_name(name)?;
        self.names.insert(name.to_string());
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        validate_legacy_name(name)?;
        self.names.remove(name);
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        let values: Vec<&String> = self.names.iter().collect();
        let mut body = serde_json::to_vec_pretty(&values)
            .map_err(|e| format!("serialize secrets index: {e}"))?;
        body.push(b'\n');
        write_atomic(&self.path, &body)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexPending {
    #[serde(default)]
    pub upserts: BTreeSet<String>,
    #[serde(default)]
    pub deletes: BTreeSet<String>,
}

impl IndexPending {
    pub fn load(path: &Path) -> Result<Self, String> {
        match fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(format!(
                    "inspect pending secrets index {}: {error}",
                    path.display()
                ));
            }
        }
        reject_link(path, "pending secrets index")?;
        serde_json::from_slice(
            &fs::read(path)
                .map_err(|e| format!("read pending secrets index {}: {e}", path.display()))?,
        )
        .map_err(|e| format!("parse pending secrets index {}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if self.upserts.is_empty() && self.deletes.is_empty() {
            return Self::clear(path);
        }
        let body = serde_json::to_vec_pretty(self)
            .map_err(|e| format!("serialize pending secrets index: {e}"))?;
        write_atomic(path, &body)
    }

    pub fn clear(path: &Path) -> Result<(), String> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "remove pending secrets index {}: {error}",
                path.display()
            )),
        }
    }
}

pub fn pending_path(index_path: &Path) -> PathBuf {
    index_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(INDEX_PENDING_FILENAME)
}

/// Permissive name check for names that may already exist in the store:
/// index load, pending-index recovery, keychain item names, migration from
/// Stronghold, backup restore, and the store layer's own writes (which
/// re-encrypt legacy values under their existing names). This is exactly the
/// WP-33 contract — non-empty, not reserved, no NUL, fits the keychain — so
/// every name an earlier build could have written keeps loading.
///
/// New names minted through the command surface go through the strict
/// [`validate_name`] / [`validate_key`] instead.
pub fn validate_legacy_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("secret name is empty".into());
    }
    if name == "__manifest" || name == "__manifest_v2" {
        return Err("secret name is reserved".into());
    }
    if name.contains('\0') {
        return Err("secret name contains a null byte".into());
    }
    if name.len().saturating_add(ITEM_PREFIX.len()) > 32_767 {
        return Err("secret name is too long for the platform keychain".into());
    }
    Ok(())
}

/// Strict name check for NEW writes (`secrets_set`, `secrets_set_scoped`):
/// the legacy rules plus an `[A-Za-z0-9_.:-]` charset. Never used on load,
/// recovery or migration paths — see [`validate_legacy_name`].
pub fn validate_name(name: &str) -> Result<(), String> {
    validate_legacy_name(name)?;
    if !name.is_ascii()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err("secret name contains unsupported characters".into());
    }
    Ok(())
}

/// Strict check for a bare (unscoped) key on a new write.
pub fn validate_key(key: &str) -> Result<(), String> {
    validate_name(key)?;
    if key.contains("::") {
        return Err("secret key contains a scope delimiter".into());
    }
    Ok(())
}

/// Permissive check for a bare key that addresses an existing (possibly
/// legacy) entry through the unscoped read/delete commands. Keeps the scope
/// delimiter ban so the unscoped commands cannot reach into a scoped name,
/// but accepts any legacy charset so pre-WP-34 names stay readable and
/// deletable.
pub fn validate_legacy_key(key: &str) -> Result<(), String> {
    validate_legacy_name(key)?;
    if key.contains("::") {
        return Err("secret key contains a scope delimiter".into());
    }
    Ok(())
}

pub fn validate_scope_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("scope id is empty".into());
    }
    if !id.is_ascii()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("scope id contains unsupported characters".into());
    }
    Ok(())
}

pub fn item_name(name: &str) -> Result<String, String> {
    validate_legacy_name(name)?;
    Ok(format!("{ITEM_PREFIX}{name}"))
}

/// [`item_name`] under a keychain service other than the production one.
/// The service doubles as the item prefix because the Windows credential
/// target is the item name itself: a different service with the production
/// prefix would still address (and overwrite) the production credentials.
pub(crate) fn item_name_for_service(service: &str, name: &str) -> Result<String, String> {
    validate_legacy_name(name)?;
    Ok(format!("{service}:{name}"))
}

pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_directory_chain(parent)?;
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if is_link_or_reparse_point(&metadata) {
            return Err(format!(
                "refusing to replace linked file {}",
                path.display()
            ));
        }
    }
    let temp = temp_path(path);
    if let Ok(metadata) = fs::symlink_metadata(&temp) {
        if is_link_or_reparse_point(&metadata) {
            return Err(format!(
                "refusing to replace linked temp file {}",
                temp.display()
            ));
        }
        fs::remove_file(&temp)
            .map_err(|error| format!("remove temp file {}: {error}", temp.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("create temp file {}: {error}", temp.display()))?;
    file.write_all(body)
        .map_err(|error| format!("write temp file {}: {error}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect temp file {}: {error}", temp.display()))?;
    }
    file.sync_all()
        .map_err(|error| format!("sync temp file {}: {error}", temp.display()))?;
    replace_file(&temp, path)
}

fn ensure_directory_chain(path: &Path) -> Result<(), String> {
    let mut chain = Vec::new();
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        chain.push(ancestor);
    }
    for directory in chain.into_iter().rev() {
        match fs::symlink_metadata(directory) {
            Ok(metadata) => {
                if is_link_or_reparse_point(&metadata) {
                    return Err(format!(
                        "refusing linked secrets directory {}",
                        directory.display()
                    ));
                }
                if !metadata.is_dir() {
                    return Err(format!(
                        "secrets path is not a directory: {}",
                        directory.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(directory).map_err(|error| {
                    format!("create directory {}: {error}", directory.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "inspect directory {}: {error}",
                    directory.display()
                ));
            }
        }
    }
    Ok(())
}

fn reject_link(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {label} {}: {error}", path.display()))?;
    if is_link_or_reparse_point(&metadata) {
        return Err(format!(
            "{label} is a symlink or reparse point: {}",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{label} is not a regular file: {}", path.display()));
    }
    Ok(())
}

fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "atomic".to_string());
    name.push_str(".tmp");
    path.with_file_name(name)
}

#[cfg(unix)]
pub(crate) fn replace_file(temp: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temp, destination).map_err(|error| {
        format!(
            "install {} from {}: {error}",
            destination.display(),
            temp.display()
        )
    })?;
    if let Some(parent) = destination.parent() {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("sync destination directory {}: {error}", parent.display()))?;
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn replace_file(temp: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(format!(
            "install {} from {}: {}",
            destination.display(),
            temp.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn replace_file(temp: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_file(destination)
            .map_err(|error| format!("remove destination {}: {error}", destination.display()))?;
    }
    fs::rename(temp, destination).map_err(|error| {
        format!(
            "install {} from {}: {error}",
            destination.display(),
            temp.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trip_contains_names_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILENAME);
        let mut index = SecretIndex::load(&path).unwrap();
        index.insert("workspace::TOKEN").unwrap();
        index.insert("pkg::com.ikenga.demo::TOKEN").unwrap();
        index.save().unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("workspace::TOKEN"));
        assert!(raw.contains("pkg::com.ikenga.demo::TOKEN"));
        assert!(!raw.contains("secret-value"));

        let loaded = SecretIndex::load(&path).unwrap();
        assert_eq!(
            loaded.names(),
            vec![
                "pkg::com.ikenga.demo::TOKEN".to_string(),
                "workspace::TOKEN".to_string()
            ]
        );
    }

    #[test]
    fn empty_or_truncated_existing_index_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILENAME);
        for body in [b" \n".as_slice(), b"[\"workspace::TOKEN\"".as_slice()] {
            fs::write(&path, body).unwrap();
            assert!(SecretIndex::load(&path).is_err());
        }
    }

    #[test]
    fn pending_index_contains_names_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_PENDING_FILENAME);
        let mut pending = IndexPending::default();
        pending.upserts.insert("workspace::TOKEN".to_string());
        pending.deletes.insert("workspace::OLD".to_string());
        pending.save(&path).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("workspace::TOKEN"));
        assert!(raw.contains("workspace::OLD"));
        assert!(!raw.contains("secret-value"));
        IndexPending::clear(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn reserved_and_empty_names_are_rejected() {
        assert!(validate_name("").is_err());
        assert!(validate_name("__manifest").is_err());
        assert!(validate_name("__manifest_v2").is_err());
        assert!(validate_name("bad\0name").is_err());
    }

    #[test]
    fn new_writes_are_strict_but_legacy_names_are_accepted() {
        assert!(validate_key("TOKEN").is_ok());
        assert!(validate_key("My Token").is_err());
        assert!(validate_key("TOKEN::extra").is_err());
        assert!(validate_name("caf\u{e9}").is_err());
        assert!(validate_legacy_name("My Token").is_ok());
        assert!(validate_legacy_name("caf\u{e9}").is_ok());
        assert!(validate_legacy_key("My Token").is_ok());
        assert!(validate_legacy_key("My::Token").is_err());
        assert!(validate_legacy_name("").is_err());
        assert!(validate_legacy_name("__manifest").is_err());
        assert!(validate_legacy_name("bad\0name").is_err());
    }

    #[test]
    fn legacy_index_name_with_a_space_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILENAME);
        fs::write(&path, br#"["My Token", "workspace::TOKEN"]"#).unwrap();
        let mut index = SecretIndex::load(&path).unwrap();
        assert_eq!(
            index.names(),
            vec!["My Token".to_string(), "workspace::TOKEN".to_string()]
        );
        assert_eq!(item_name("My Token").unwrap(), "ikenga:My Token");
        index.remove("My Token").unwrap();
        index.insert("My Token").unwrap();
        index.save().unwrap();
        assert!(SecretIndex::load(&path)
            .unwrap()
            .names()
            .contains(&"My Token".to_string()));
    }

    #[test]
    fn item_names_use_the_platform_prefix() {
        assert_eq!(
            item_name("workspace::TOKEN").unwrap(),
            "ikenga:workspace::TOKEN"
        );
    }

    #[test]
    fn service_item_names_match_the_default_and_isolate_other_services() {
        assert_eq!(
            item_name_for_service("ikenga", "workspace::TOKEN").unwrap(),
            item_name("workspace::TOKEN").unwrap()
        );
        assert_eq!(
            item_name_for_service("ikenga-rehearsal-5a", "workspace::TOKEN").unwrap(),
            "ikenga-rehearsal-5a:workspace::TOKEN"
        );
        assert!(item_name_for_service("ikenga-rehearsal-5a", "__manifest").is_err());
    }

    #[test]
    fn atomic_write_overwrites_existing_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILENAME);
        write_atomic(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
}
