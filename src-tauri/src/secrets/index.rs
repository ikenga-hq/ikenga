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
        if !path.exists() {
            return Ok(Self {
                path,
                names: BTreeSet::new(),
            });
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("read secrets index {}: {e}", path.display()))?;
        if raw.trim().is_empty() {
            return Err(format!("secrets index is empty: {}", path.display()));
        }
        let values: Vec<String> = serde_json::from_str(&raw)
            .map_err(|e| format!("parse secrets index {}: {e}", path.display()))?;
        let mut names = BTreeSet::new();
        for value in values {
            validate_name(&value)?;
            names.insert(value);
        }
        Ok(Self { path, names })
    }

    pub fn names(&self) -> Vec<String> {
        self.names.iter().cloned().collect()
    }

    pub fn insert(&mut self, name: &str) -> Result<(), String> {
        validate_name(name)?;
        self.names.insert(name.to_string());
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<(), String> {
        validate_name(name)?;
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
        if !path.exists() {
            return Ok(Self::default());
        }
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

pub fn validate_name(name: &str) -> Result<(), String> {
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

pub fn item_name(name: &str) -> Result<String, String> {
    validate_name(name)?;
    Ok(format!("{ITEM_PREFIX}{name}"))
}

pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|e| format!("create file parent {}: {e}", parent.display()))?;
    let temp = temp_path(path);
    let _ = fs::remove_file(&temp);
    let mut file =
        fs::File::create(&temp).map_err(|e| format!("create temp file {}: {e}", temp.display()))?;
    file.write_all(body)
        .map_err(|e| format!("write temp file {}: {e}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("protect temp file {}: {e}", temp.display()))?;
    }
    file.sync_all()
        .map_err(|e| format!("sync temp file {}: {e}", temp.display()))?;
    replace_file(&temp, path)
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
    fn item_names_use_the_platform_prefix() {
        assert_eq!(
            item_name("workspace::TOKEN").unwrap(),
            "ikenga:workspace::TOKEN"
        );
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
