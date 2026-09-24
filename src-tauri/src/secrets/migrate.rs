use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri_plugin_stronghold::stronghold::Stronghold;

use super::index::{pending_path, write_atomic, INDEX_FILENAME};
use super::keyring_store::KeyringStore;
use super::store::{SecretMeta, SecretsStore};

const LEGACY_FILENAME: &str = "secrets.stronghold";
const LEGACY_BACKUP_FILENAME: &str = "secrets.stronghold.bak";
const LEGACY_MIGRATED_FILENAME: &str = "secrets.stronghold.migrated";
const MIGRATION_MARKER_FILENAME: &str = "secrets-migration.json";
const ROLLBACK_MARKER_FILENAME: &str = "secrets-migration.rollback.json";
const LEGACY_KEY_FILENAME: &str = ".vault-key";
const LEGACY_CLIENT_NAME: &[u8] = b"pa";
const LEGACY_MANIFEST: &[u8] = b"__manifest";
const LEGACY_MANIFEST_V2: &[u8] = b"__manifest_v2";
const LEGACY_KEY_LEN: usize = 32;
const MARKER_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationOutcome {
    NoLegacyVault,
    Migrated { count: usize },
    AlreadyComplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct MigrationMarker {
    version: u32,
    source_sha256: String,
    backup_sha256: String,
    index_sha256: String,
    count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RollbackMarker {
    version: u32,
    backup_sha256: String,
    legacy_sha256: Option<String>,
    state: String,
}

struct MigrationPaths {
    legacy: PathBuf,
    backup: PathBuf,
    migrated: PathBuf,
    key: PathBuf,
    index: PathBuf,
    pending: PathBuf,
    marker: PathBuf,
    rollback_marker: PathBuf,
}

impl MigrationPaths {
    fn new(data_dir: &Path) -> Self {
        let index = data_dir.join(INDEX_FILENAME);
        Self {
            legacy: data_dir.join(LEGACY_FILENAME),
            backup: data_dir.join(LEGACY_BACKUP_FILENAME),
            migrated: data_dir.join(LEGACY_MIGRATED_FILENAME),
            key: data_dir.join(LEGACY_KEY_FILENAME),
            pending: pending_path(&index),
            index,
            marker: data_dir.join(MIGRATION_MARKER_FILENAME),
            rollback_marker: data_dir.join(ROLLBACK_MARKER_FILENAME),
        }
    }

    fn existing_backup(&self) -> Option<PathBuf> {
        if self.backup.exists() {
            Some(self.backup.clone())
        } else if self.migrated.exists() {
            Some(self.migrated.clone())
        } else {
            None
        }
    }
}

pub fn run(data_dir: &Path) -> Result<MigrationOutcome, String> {
    let paths = MigrationPaths::new(data_dir);
    if paths.rollback_marker.exists() {
        let marker: RollbackMarker = read_json(&paths.rollback_marker)?;
        return Err(format!(
            "rollback transition is active ({}); downgrade with the legacy app or remove {} to resume migration",
            marker.state,
            paths.rollback_marker.display()
        ));
    }
    let initial_migration = !paths.index.exists()
        && paths.legacy.exists()
        && !paths.backup.exists()
        && !paths.migrated.exists()
        && !paths.marker.exists();
    let store = if initial_migration {
        KeyringStore::new_for_migration(paths.index.clone())
    } else {
        KeyringStore::new(paths.index.clone())
    }
    .map_err(|error| error.to_string())?;
    migrate_with(data_dir, &store, |legacy_path, key_path| {
        read_legacy(legacy_path, key_path)
    })
}

pub fn rollback(data_dir: &Path) -> Result<bool, String> {
    let paths = MigrationPaths::new(data_dir);
    if paths.rollback_marker.exists() {
        return Err(format!(
            "rollback already active: {}",
            paths.rollback_marker.display()
        ));
    }
    let Some(backup) = paths.existing_backup() else {
        return Ok(false);
    };
    let backup_sha256 = fingerprint(&backup)?;
    let store = KeyringStore::new(paths.index.clone()).map_err(|error| error.to_string())?;
    let values = store.export_all().map_err(|error| error.to_string())?;
    write_json(
        &paths.rollback_marker,
        &RollbackMarker {
            version: MARKER_VERSION,
            backup_sha256,
            legacy_sha256: None,
            state: "preparing".into(),
        },
    )?;
    let legacy_sha256 = build_rollback_snapshot(&paths, &values)?;
    let marker = RollbackMarker {
        version: MARKER_VERSION,
        backup_sha256: fingerprint(&backup)?,
        legacy_sha256: Some(legacy_sha256),
        state: "ready".into(),
    };
    write_json(&paths.rollback_marker, &marker)?;
    store
        .replace_all(&BTreeMap::new())
        .map_err(|error| format!("retire keychain state for rollback: {error}"))?;
    remove_if_exists(&paths.index)?;
    remove_if_exists(&paths.pending)?;
    remove_if_exists(&paths.marker)?;
    remove_if_exists(&paths.backup)?;
    remove_if_exists(&paths.migrated)?;
    Ok(true)
}

fn migrate_with<F>(
    data_dir: &Path,
    store: &dyn SecretsStore,
    load_legacy: F,
) -> Result<MigrationOutcome, String>
where
    F: FnOnce(&Path, &Path) -> Result<BTreeMap<String, String>, String>,
{
    let paths = MigrationPaths::new(data_dir);
    if paths.rollback_marker.exists() {
        return Err("rollback transition is active; migration is blocked".into());
    }

    let marker = if paths.marker.exists() {
        let marker: MigrationMarker = read_json(&paths.marker)?;
        validate_migration_marker(&paths, &marker)?;
        Some(marker)
    } else {
        None
    };

    let legacy_fingerprint = if paths.legacy.exists() {
        Some(fingerprint(&paths.legacy)?)
    } else {
        None
    };

    if let Some(marker) = &marker {
        if legacy_fingerprint
            .as_deref()
            .map(|value| value == marker.source_sha256)
            .unwrap_or(true)
        {
            return Ok(MigrationOutcome::AlreadyComplete);
        }
    } else {
        if legacy_fingerprint.is_none() {
            if paths.existing_backup().is_some() {
                return Err("migration backup exists without a verified marker".into());
            }
            return Ok(MigrationOutcome::NoLegacyVault);
        }
        if paths.existing_backup().is_some() {
            return Err("legacy and backup snapshots exist without a verified marker".into());
        }
    }

    let legacy_fingerprint = legacy_fingerprint
        .ok_or_else(|| "a different legacy snapshot is required for migration".to_string())?;
    let values = load_legacy(&paths.legacy, &paths.key)?;
    let count = store
        .replace_all(&values)
        .map_err(|error| format!("keychain migration failed: {error}"))?;
    verify_store(store, &values)?;

    copy_atomic(&paths.legacy, &paths.backup)?;
    let source_sha256 = fingerprint(&paths.legacy)?;
    let backup_sha256 = fingerprint(&paths.backup)?;
    if source_sha256 != backup_sha256 || source_sha256 != legacy_fingerprint {
        return Err("migration source fingerprint changed during verification".into());
    }
    let index_sha256 = index_fingerprint(store)?;
    let marker = MigrationMarker {
        version: MARKER_VERSION,
        source_sha256,
        backup_sha256,
        index_sha256,
        count,
    };
    write_json(&paths.marker, &marker)?;
    let persisted_marker: MigrationMarker = read_json(&paths.marker)?;
    if persisted_marker != marker
        || persisted_marker.index_sha256 != index_fingerprint(store)?
        || persisted_marker.count != values.len()
    {
        return Err("migration marker verification failed".into());
    }
    if fingerprint(&paths.legacy)? == persisted_marker.source_sha256
        && fingerprint(&paths.backup)? == persisted_marker.backup_sha256
    {
        fs::remove_file(&paths.legacy).map_err(|error| {
            format!(
                "remove fully verified legacy snapshot {}: {error}",
                paths.legacy.display()
            )
        })?;
    }
    Ok(MigrationOutcome::Migrated { count })
}

fn validate_migration_marker(
    paths: &MigrationPaths,
    marker: &MigrationMarker,
) -> Result<(), String> {
    if marker.version != MARKER_VERSION {
        return Err("unsupported migration marker version".into());
    }
    if !valid_sha256(&marker.source_sha256)
        || !valid_sha256(&marker.backup_sha256)
        || !valid_sha256(&marker.index_sha256)
    {
        return Err("migration marker fingerprint is invalid".into());
    }
    if marker.source_sha256 != marker.backup_sha256 {
        return Err("migration marker source and backup differ".into());
    }
    let backup = paths
        .existing_backup()
        .ok_or_else(|| "migration marker exists without a rollback snapshot".to_string())?;
    if fingerprint(&backup)? != marker.backup_sha256 {
        return Err("migration backup fingerprint does not match marker".into());
    }
    if !paths.index.exists() {
        return Err("migration marker exists without secrets index".into());
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn verify_store(
    store: &dyn SecretsStore,
    expected: &BTreeMap<String, String>,
) -> Result<(), String> {
    let actual = store.export_all().map_err(|error| error.to_string())?;
    if actual != *expected {
        return Err("migrated keychain contents do not match the legacy snapshot".into());
    }
    Ok(())
}

fn index_fingerprint(store: &dyn SecretsStore) -> Result<String, String> {
    let mut names = store
        .list_meta()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|meta| meta.name)
        .collect::<Vec<_>>();
    names.sort();
    let mut hasher = Sha256::new();
    for name in names {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

fn fingerprint(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("fingerprint {}: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("serialize migration marker: {error}"))?;
    write_atomic(path, &body)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("read migration marker: {error}"))?,
    )
    .map_err(|error| format!("parse migration marker: {error}"))
}

fn copy_atomic(source: &Path, destination: &Path) -> Result<(), String> {
    let bytes = fs::read(source)
        .map_err(|error| format!("read migration source {}: {error}", source.display()))?;
    write_atomic(destination, &bytes)
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove {}: {error}", path.display())),
    }
}

fn build_rollback_snapshot(
    paths: &MigrationPaths,
    values: &BTreeMap<String, String>,
) -> Result<String, String> {
    let key = read_legacy_key(&paths.key)?;
    let temp = paths.legacy.with_extension("stronghold.rollback.tmp");
    let _ = fs::remove_file(&temp);
    let stronghold = Stronghold::new(&temp, key)
        .map_err(|error| format!("create rollback Stronghold snapshot: {error}"))?;
    stronghold
        .create_client(LEGACY_CLIENT_NAME)
        .map_err(|error| format!("create rollback Stronghold client: {error}"))?;
    let client = stronghold
        .get_client(LEGACY_CLIENT_NAME)
        .map_err(|error| format!("open rollback Stronghold client: {error}"))?;
    let store = client.store();
    let mut unscoped = Vec::new();
    let mut scoped = Vec::new();
    for (name, value) in values {
        store
            .insert(name.as_bytes().to_vec(), value.as_bytes().to_vec(), None)
            .map_err(|error| format!("write rollback value for `{name}`: {error}"))?;
        if is_scoped_name(name) {
            scoped.push(name.clone());
        } else {
            unscoped.push(name.clone());
        }
    }
    for (key, names) in [(LEGACY_MANIFEST, unscoped), (LEGACY_MANIFEST_V2, scoped)] {
        let json = serde_json::to_vec(&names)
            .map_err(|error| format!("serialize rollback manifest: {error}"))?;
        store
            .insert(key.to_vec(), json, None)
            .map_err(|error| format!("write rollback manifest: {error}"))?;
    }
    stronghold
        .save()
        .map_err(|error| format!("save rollback Stronghold snapshot: {error}"))?;
    let verified = read_legacy(&temp, &paths.key)?;
    if verified != *values {
        let _ = fs::remove_file(&temp);
        return Err("rollback Stronghold snapshot verification failed".into());
    }
    copy_atomic(&temp, &paths.legacy)?;
    remove_if_exists(&temp)?;
    fingerprint(&paths.legacy)
}

fn is_scoped_name(name: &str) -> bool {
    name.starts_with("workspace::") || name.starts_with("project::") || name.starts_with("pkg::")
}

fn read_legacy(legacy_path: &Path, key_path: &Path) -> Result<BTreeMap<String, String>, String> {
    let key = read_legacy_key(key_path)?;
    let stronghold = Stronghold::new(legacy_path, key)
        .map_err(|error| format!("open legacy Stronghold snapshot: {error}"))?;
    if stronghold.get_client(LEGACY_CLIENT_NAME).is_err()
        && stronghold.load_client(LEGACY_CLIENT_NAME).is_err()
    {
        return Err("legacy Stronghold client is missing".into());
    }
    let client = stronghold
        .get_client(LEGACY_CLIENT_NAME)
        .map_err(|error| format!("read legacy Stronghold client: {error}"))?;
    let store = client.store();
    let manifest = read_manifest(
        store
            .get(LEGACY_MANIFEST)
            .map_err(|error| format!("read legacy Stronghold manifest: {error}"))?,
    );
    let scoped_manifest = read_manifest(
        store
            .get(LEGACY_MANIFEST_V2)
            .map_err(|error| format!("read legacy Stronghold scoped manifest: {error}"))?,
    );
    let mut manifest_names = manifest;
    manifest_names.extend(scoped_manifest);
    let enumerated = store
        .keys()
        .map_err(|error| format!("enumerate legacy Stronghold keys: {error}"))?;
    let enumerated_names = validate_enumerated_names(enumerated)?;
    validate_manifest_completeness(&enumerated_names, &manifest_names)?;

    let mut values = BTreeMap::new();
    for name in enumerated_names {
        let bytes = store
            .get(name.as_bytes())
            .map_err(|_| format!("legacy Stronghold value for `{name}` is missing"))?
            .ok_or_else(|| format!("legacy Stronghold value for `{name}` is missing"))?;
        let value = String::from_utf8(bytes)
            .map_err(|_| format!("legacy Stronghold value for `{name}` is not UTF-8"))?;
        values.insert(name, value);
    }
    Ok(values)
}

fn validate_enumerated_names(keys: Vec<Vec<u8>>) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for key in keys {
        if key.as_slice() == LEGACY_MANIFEST || key.as_slice() == LEGACY_MANIFEST_V2 {
            continue;
        }
        let name = String::from_utf8(key)
            .map_err(|_| "legacy Stronghold contains a non-UTF-8 secret name".to_string())?;
        if name.is_empty() {
            return Err("legacy Stronghold contains an empty secret name".into());
        }
        names.insert(name);
    }
    Ok(names)
}

fn validate_manifest_completeness(
    enumerated: &BTreeSet<String>,
    manifest: &BTreeSet<String>,
) -> Result<(), String> {
    if enumerated == manifest {
        return Ok(());
    }
    let missing = enumerated.difference(manifest).cloned().collect::<Vec<_>>();
    let stale = manifest.difference(enumerated).cloned().collect::<Vec<_>>();
    Err(format!(
        "legacy Stronghold manifest is incomplete (missing: {missing:?}; stale: {stale:?})"
    ))
}

fn read_manifest(bytes: Option<Vec<u8>>) -> Result<BTreeSet<String>, String> {
    let Some(bytes) = bytes else {
        return Ok(BTreeSet::new());
    };
    let raw = String::from_utf8(bytes).map_err(|_| "legacy manifest is not UTF-8".to_string())?;
    serde_json::from_str(&raw)
        .map(|values: Vec<String>| values.into_iter().collect())
        .map_err(|error| format!("parse legacy manifest: {error}"))
}

fn read_legacy_key(path: &Path) -> Result<Vec<u8>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("read legacy Stronghold key {}: {error}", path.display()))?;
    let bytes = hex::decode(raw.trim())
        .map_err(|error| format!("decode legacy Stronghold key {}: {error}", path.display()))?;
    if bytes.len() != LEGACY_KEY_LEN {
        return Err(format!(
            "legacy Stronghold key has {} bytes; expected {LEGACY_KEY_LEN}",
            bytes.len()
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::secrets::store::StoreError;

    #[derive(Default)]
    struct MemoryStore {
        values: Mutex<BTreeMap<String, String>>,
        index_path: Option<PathBuf>,
        replace_calls: Mutex<usize>,
        fail_replace: bool,
    }

    impl MemoryStore {
        fn write_index(&self) -> Result<(), StoreError> {
            if let Some(path) = &self.index_path {
                let names: Vec<String> = self.values.lock().unwrap().keys().cloned().collect();
                fs::write(path, serde_json::to_vec(&names).unwrap())
                    .map_err(|error| StoreError::committed(format!("write test index: {error}")))?;
            }
            Ok(())
        }
    }

    impl SecretsStore for MemoryStore {
        fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
            Ok(self.values.lock().unwrap().get(name).cloned())
        }

        fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
            self.values
                .lock()
                .unwrap()
                .insert(name.to_string(), value.to_string());
            self.write_index()
        }

        fn delete(&self, name: &str) -> Result<(), StoreError> {
            self.values.lock().unwrap().remove(name);
            self.write_index()
        }

        fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .map(|name| SecretMeta { name })
                .collect())
        }

        fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
            *self.replace_calls.lock().unwrap() += 1;
            if self.fail_replace {
                return Err(StoreError::uncommitted("Secret Service write failed"));
            }
            *self.values.lock().unwrap() = values.clone();
            self.write_index()?;
            Ok(values.len())
        }

        fn probe(&self) -> Result<(), StoreError> {
            Ok(())
        }

        fn backend_label(&self) -> &'static str {
            "memory"
        }
    }

    fn memory_store(index_path: Option<PathBuf>) -> MemoryStore {
        MemoryStore {
            index_path,
            ..MemoryStore::default()
        }
    }

    fn write_completed_marker(paths: &MigrationPaths, source: &Path) {
        let hash = fingerprint(source).unwrap();
        write_json(
            &paths.marker,
            &MigrationMarker {
                version: MARKER_VERSION,
                source_sha256: hash.clone(),
                backup_sha256: hash,
                index_sha256: hex::encode(Sha256::digest(b"index")),
                count: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn migration_verifies_exact_store_writes_backup_and_marker() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.legacy, b"legacy-snapshot").unwrap();
        let store = memory_store(Some(paths.index.clone()));
        let mut values = BTreeMap::new();
        values.insert("workspace::SHARED".to_string(), "one".to_string());
        values.insert("project::alpha::TOKEN".to_string(), "two".to_string());

        let outcome = migrate_with(dir.path(), &store, |_, _| Ok(values.clone())).unwrap();
        assert_eq!(outcome, MigrationOutcome::Migrated { count: 2 });
        assert!(!paths.legacy.exists());
        assert_eq!(fs::read(&paths.backup).unwrap(), b"legacy-snapshot");
        assert!(paths.marker.exists());
    }

    #[test]
    fn completed_backup_returns_already_complete_without_replacing_store() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.legacy, b"legacy-snapshot").unwrap();
        let store = memory_store(Some(paths.index.clone()));
        let mut initial = BTreeMap::new();
        initial.insert("workspace::TOKEN".to_string(), "one".to_string());
        migrate_with(dir.path(), &store, |_, _| Ok(initial)).unwrap();
        store.set("workspace::ADDED", "after-migration").unwrap();
        let calls = *store.replace_calls.lock().unwrap();

        let outcome = migrate_with(dir.path(), &store, |_, _| {
            panic!("completed migration must not enumerate or replace")
        })
        .unwrap();
        assert_eq!(outcome, MigrationOutcome::AlreadyComplete);
        assert_eq!(*store.replace_calls.lock().unwrap(), calls);
        assert_eq!(
            store.get("workspace::ADDED").unwrap().as_deref(),
            Some("after-migration")
        );
    }

    #[test]
    fn matching_legacy_snapshot_returns_already_complete() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.legacy, b"legacy-snapshot").unwrap();
        fs::write(&paths.backup, b"legacy-snapshot").unwrap();
        fs::write(&paths.index, b"[]").unwrap();
        write_completed_marker(&paths, &paths.legacy);
        let store = memory_store(Some(paths.index.clone()));

        let outcome = migrate_with(dir.path(), &store, |_, _| {
            panic!("matching legacy source must not replace")
        })
        .unwrap();
        assert_eq!(outcome, MigrationOutcome::AlreadyComplete);
    }

    #[test]
    fn different_legacy_snapshot_replaces_store() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.backup, b"old-backup").unwrap();
        fs::write(&paths.index, b"[]").unwrap();
        write_completed_marker(&paths, &paths.backup);
        fs::write(&paths.legacy, b"new-legacy").unwrap();
        let store = memory_store(Some(paths.index.clone()));
        let mut values = BTreeMap::new();
        values.insert("workspace::NEW".to_string(), "new".to_string());

        migrate_with(dir.path(), &store, |_, _| Ok(values)).unwrap();
        assert!(!paths.legacy.exists());
        assert_eq!(fs::read(&paths.backup).unwrap(), b"new-legacy");
        assert_eq!(store.get("workspace::NEW").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn backup_without_marker_is_rejected_before_replace() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.backup, b"backup").unwrap();
        fs::write(&paths.index, b"[]").unwrap();
        let store = memory_store(Some(paths.index.clone()));
        let error = migrate_with(dir.path(), &store, |_, _| {
            panic!("unverified backup must not enumerate")
        })
        .unwrap_err();
        assert!(error.contains("without a verified marker"));
        assert_eq!(*store.replace_calls.lock().unwrap(), 0);
    }

    #[test]
    fn rollback_marker_blocks_next_migration() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        write_json(
            &paths.rollback_marker,
            &RollbackMarker {
                version: MARKER_VERSION,
                backup_sha256: hex::encode(Sha256::digest(b"backup")),
                legacy_sha256: Some(hex::encode(Sha256::digest(b"legacy"))),
                state: "ready".into(),
            },
        )
        .unwrap();
        let store = memory_store(Some(paths.index.clone()));
        let error = migrate_with(dir.path(), &store, |_, _| {
            panic!("rollback must block migration")
        })
        .unwrap_err();
        assert!(error.contains("rollback transition is active"));
        assert_eq!(*store.replace_calls.lock().unwrap(), 0);
    }

    #[test]
    fn failed_migration_preserves_legacy_and_publishes_no_runtime_state() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.legacy, b"legacy-snapshot").unwrap();
        let store = MemoryStore {
            fail_replace: true,
            ..MemoryStore::default()
        };
        let mut values = BTreeMap::new();
        values.insert("workspace::TOKEN".to_string(), "value".to_string());

        let error = migrate_with(dir.path(), &store, |_, _| Ok(values)).unwrap_err();
        assert!(error.contains("Secret Service write failed"));
        assert!(paths.legacy.exists());
        assert!(!paths.marker.exists());
    }

    #[test]
    fn manifest_completeness_rejects_missing_and_stale_names() {
        let enumerated = BTreeSet::from(["A".to_string(), "B".to_string()]);
        assert!(validate_manifest_completeness(&enumerated, &enumerated).is_ok());
        assert!(
            validate_manifest_completeness(&enumerated, &BTreeSet::from(["A".to_string()]))
                .is_err()
        );
        assert!(validate_manifest_completeness(
            &enumerated,
            &BTreeSet::from(["A".to_string(), "B".to_string(), "C".to_string()])
        )
        .is_err());
    }

    #[test]
    fn rollback_snapshot_round_trips_keychain_values() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.key, hex::encode([7u8; 32])).unwrap();
        let mut values = BTreeMap::new();
        values.insert("LEGACY".to_string(), "one".to_string());
        values.insert("workspace::TOKEN".to_string(), "two".to_string());

        let hash = build_rollback_snapshot(&paths, &values).unwrap();
        assert_eq!(hash, fingerprint(&paths.legacy).unwrap());
        assert_eq!(read_legacy(&paths.legacy, &paths.key).unwrap(), values);
    }

    #[test]
    fn migrated_alias_is_accepted_only_with_valid_marker() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MigrationPaths::new(dir.path());
        fs::write(&paths.migrated, b"migrated-snapshot").unwrap();
        fs::write(&paths.index, b"[]").unwrap();
        write_completed_marker(&paths, &paths.migrated);
        let store = memory_store(Some(paths.index.clone()));

        let outcome = migrate_with(dir.path(), &store, |_, _| {
            panic!("completed migrated alias must not replace")
        })
        .unwrap();
        assert_eq!(outcome, MigrationOutcome::AlreadyComplete);
    }
}
