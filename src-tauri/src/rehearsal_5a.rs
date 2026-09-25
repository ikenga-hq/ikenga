//! WP-37 — Phase 5a migration rehearsal against a COPY of real app data.
//!
//! `#[ignore]`d and inert unless `IKENGA_REHEARSAL_5A_DIR` names a rehearsal
//! root whose `source/` holds a copy of the app data dir (`secrets.stronghold`,
//! `.vault-key`, `ikenga.db` + `-wal`/`-shm`). Nothing here reads or writes the
//! live app data dir, the production keychain service, or `~/.ikenga`:
//!
//! * every keychain write goes through [`REHEARSAL_SERVICE`] (the service is
//!   also the item prefix, so Windows credential targets cannot collide);
//! * the settings migration runs against a copied SQLite file, a temp home and
//!   project roots remapped into the rehearsal dir;
//! * the real personal settings file and real project settings files are
//!   fingerprinted before and after and must be unchanged.
//!
//! No secret value is ever printed or asserted with a value-bearing message:
//! comparisons happen in-process and only names, counts, lengths and booleans
//! reach `results.json` / stdout.
//!
//! ```text
//! IKENGA_REHEARSAL_5A_DIR=<root> cargo test --lib rehearsal_5a -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing_subscriber::layer::SubscriberExt;

use crate::secrets::migrate::{
    read_legacy, rollback_with_service, run_with_service, MigrationOutcome,
};
use crate::secrets::{
    EncryptedStore, KeyringStore, SecretsStore, UnlockError, UnlockState, DEFAULT_IDLE_TIMEOUT,
    UNLOCK_ENVELOPE_FILENAME,
};
use crate::settings::migrate::{migrate_from_kv, FILE_MIGRATION_KEY};
use crate::settings::schema::{self, LegacyTarget, SettingsDocument};
use crate::settings::scope::{personal_path, project_path, read_document, write_document};
use crate::settings::{effective_document, project_roots_from_pool, SettingsScope};

const ENV_DIR: &str = "IKENGA_REHEARSAL_5A_DIR";
const REHEARSAL_SERVICE: &str = "ikenga-rehearsal-5a";
const PRODUCTION_SERVICE: &str = "ikenga";
const APP_IDENTIFIER: &str = "app.ikenga";
const INDEX: &str = "secrets-index.json";
const LEGACY: &str = "secrets.stronghold";
const BACKUP: &str = "secrets.stronghold.bak";
const MARKER: &str = "secrets-migration.json";
const ROLLBACK_MARKER: &str = "secrets-migration.rollback.json";
const VAULT_KEY: &str = ".vault-key";
const ENCRYPTED_PREFIX: &str = "ikenga-secret:v1:";
const OVERRIDE_FIELD: &str = "appearance.theme";

// ---------------------------------------------------------------- log capture

#[derive(Default)]
struct Captured {
    /// (target, rendered text). Never printed; only scanned in-process.
    records: Vec<(String, String)>,
}

fn captured() -> &'static Mutex<Captured> {
    static CAPTURED: OnceLock<Mutex<Captured>> = OnceLock::new();
    CAPTURED.get_or_init(|| Mutex::new(Captured::default()))
}

fn push_record(target: &str, text: String) {
    if let Ok(mut guard) = captured().lock() {
        guard.records.push((target.to_string(), text));
    }
}

struct LogCapture;

impl log::Log for LogCapture {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        push_record(
            record.target(),
            format!("{} {} {}", record.level(), record.target(), record.args()),
        );
    }

    fn flush(&self) {}
}

struct FieldText(String);

impl tracing::field::Visit for FieldText {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        // Raw, unescaped: a Debug rendering would escape quotes/backslashes
        // and could hide a secret from the substring scan.
        self.0.push_str(&format!(" {}={}", field.name(), value));
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={:?}", field.name(), value));
    }
}

struct TraceCapture;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for TraceCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut text = FieldText(format!(
            "{} {}",
            event.metadata().level(),
            event.metadata().target()
        ));
        event.record(&mut text);
        push_record(event.metadata().target(), text.0);
    }
}

fn install_capture() -> Result<(), String> {
    static LOG_CAPTURE: LogCapture = LogCapture;
    log::set_logger(&LOG_CAPTURE).map_err(|_| "a log logger is already set")?;
    log::set_max_level(log::LevelFilter::Trace);
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(TraceCapture))
        .map_err(|_| "a tracing subscriber is already set".to_string())
}

// ------------------------------------------------------------------ reporting

#[derive(Default)]
struct Report {
    checks: Vec<(String, bool)>,
    facts: BTreeMap<String, Value>,
}

impl Report {
    fn check(&mut self, id: &str, pass: bool) {
        println!("[rehearsal-5a] {} {id}", if pass { "PASS" } else { "FAIL" });
        self.checks.push((id.to_string(), pass));
    }

    fn fact(&mut self, key: &str, value: Value) {
        self.facts.insert(key.to_string(), value);
    }

    fn failures(&self) -> usize {
        self.checks.iter().filter(|(_, pass)| !pass).count()
    }
}

// -------------------------------------------------------------------- helpers

fn sha256_file(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
}

fn copy_into(source: &Path, dest_dir: &Path, name: &str) -> Result<(), String> {
    fs::copy(source.join(name), dest_dir.join(name))
        .map(|_| ())
        .map_err(|error| format!("copy {name}: {error}"))
}

fn scope_of(name: &str) -> &'static str {
    if name.starts_with("workspace::") {
        "workspace"
    } else if name.starts_with("project::") {
        "project"
    } else if name.starts_with("pkg::") {
        "pkg"
    } else if name.contains("::") {
        "other_scoped"
    } else {
        "legacy_bare"
    }
}

fn scope_counts(names: impl IntoIterator<Item = String>) -> Value {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for scope in ["workspace", "project", "pkg", "other_scoped", "legacy_bare"] {
        counts.insert(scope, 0);
    }
    for name in names {
        *counts.entry(scope_of(&name)).or_default() += 1;
    }
    json!(counts)
}

/// Raw stored bytes as the platform keychain returns them (no decryption).
fn raw_store(dir: &Path) -> Result<KeyringStore, String> {
    KeyringStore::new_with_service(dir.join(INDEX), REHEARSAL_SERVICE).map_err(String::from)
}

/// Remove every named item from the rehearsal namespace. Uses a scratch index
/// so it can reach items no index lists (e.g. after a crashed run).
fn sweep_rehearsal_items(scratch: &Path, names: &BTreeSet<String>) -> Result<usize, String> {
    fs::create_dir_all(scratch).map_err(|error| format!("mkdir sweep: {error}"))?;
    let store = KeyringStore::new_with_service(scratch.join(INDEX), REHEARSAL_SERVICE)
        .map_err(String::from)?;
    let mut removed = 0;
    for name in names {
        if store.get(name).map_err(String::from)?.is_some() {
            removed += 1;
        }
        // `set` then `delete` so the index agrees and the item is gone even
        // if it was never indexed here.
        store.set(name, "sweep").map_err(String::from)?;
        store.delete(name).map_err(String::from)?;
    }
    Ok(removed)
}

fn count_present(dir: &Path, names: &BTreeSet<String>) -> Result<usize, String> {
    fs::create_dir_all(dir).map_err(|error| format!("mkdir verify: {error}"))?;
    let store =
        KeyringStore::new_with_service(dir.join(INDEX), REHEARSAL_SERVICE).map_err(String::from)?;
    let mut present = 0;
    for name in names {
        if store.get(name).map_err(String::from)?.is_some() {
            present += 1;
        }
    }
    Ok(present)
}

/// Stat-only fingerprint of a real file we must not modify.
fn untouched_fingerprint(path: &Path) -> Value {
    match fs::metadata(path) {
        Ok(meta) => json!({
            "exists": true,
            "len": meta.len(),
            "modified": meta.modified().ok().map(|time| format!("{time:?}")),
            "sha256": sha256_file(path),
        }),
        Err(_) => json!({ "exists": false }),
    }
}

fn write_synthetic_stronghold(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    use tauri_plugin_stronghold::stronghold::Stronghold;

    let key: Vec<u8> = (0..2)
        .flat_map(|_| uuid::Uuid::new_v4().into_bytes())
        .collect();
    fs::write(dir.join(VAULT_KEY), hex::encode(&key)).map_err(|e| format!("write key: {e}"))?;
    let mut values = BTreeMap::new();
    for name in [
        "LEGACY_BARE_TOKEN",
        "workspace::SYNTH_WORKSPACE_TOKEN",
        "project::alpha::SYNTH_PROJECT_TOKEN",
        "pkg::com.ikenga.demo::SYNTH_PKG_TOKEN",
    ] {
        values.insert(
            name.to_string(),
            format!("synthetic-{}", uuid::Uuid::new_v4().simple()),
        );
    }
    let bare: Vec<&String> = values.keys().filter(|name| !name.contains("::")).collect();
    let scoped: Vec<&String> = values.keys().filter(|name| name.contains("::")).collect();
    let stronghold =
        Stronghold::new(dir.join(LEGACY), key).map_err(|e| format!("create stronghold: {e}"))?;
    let client = stronghold
        .create_client(b"pa".to_vec())
        .map_err(|e| format!("create client: {e}"))?;
    let store = client.store();
    for (name, value) in &values {
        store
            .insert(name.as_bytes().to_vec(), value.as_bytes().to_vec(), None)
            .map_err(|e| format!("insert: {e}"))?;
    }
    store
        .insert(
            b"__manifest".to_vec(),
            serde_json::to_vec(&bare).unwrap(),
            None,
        )
        .map_err(|e| format!("insert manifest: {e}"))?;
    store
        .insert(
            b"__manifest_v2".to_vec(),
            serde_json::to_vec(&scoped).unwrap(),
            None,
        )
        .map_err(|e| format!("insert manifest v2: {e}"))?;
    stronghold
        .write_client(b"pa".to_vec())
        .map_err(|e| format!("write client: {e}"))?;
    stronghold
        .save()
        .map_err(|e| format!("save stronghold: {e}"))?;
    Ok(values)
}

/// Raw key names present in a Stronghold snapshot (for manifest booleans).
fn stronghold_key_names(dir: &Path) -> Result<BTreeSet<String>, String> {
    use tauri_plugin_stronghold::stronghold::Stronghold;

    let raw = fs::read_to_string(dir.join(VAULT_KEY)).map_err(|e| format!("read key: {e}"))?;
    let key = hex::decode(raw.trim()).map_err(|e| format!("decode key: {e}"))?;
    let stronghold =
        Stronghold::new(dir.join(LEGACY), key).map_err(|e| format!("open stronghold: {e}"))?;
    let client = stronghold
        .load_client(b"pa".to_vec())
        .or_else(|_| stronghold.get_client(b"pa".to_vec()))
        .map_err(|e| format!("load client: {e}"))?;
    let keys = client.store().keys().map_err(|e| format!("keys: {e}"))?;
    Ok(keys
        .into_iter()
        .map(|key| String::from_utf8_lossy(&key).into_owned())
        .collect())
}

// ------------------------------------------------------------------- step A

struct Source {
    label: &'static str,
    /// Prepares a fresh working dir holding a Stronghold snapshot + key.
    dir: PathBuf,
    values: BTreeMap<String, String>,
    sha256: String,
}

fn prepare_workdir(source: &Source, run: &Path, step: &str) -> Result<PathBuf, String> {
    let dir = run.join(format!("{step}-{}", source.label));
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {step}: {e}"))?;
    copy_into(&source.dir, &dir, LEGACY)?;
    copy_into(&source.dir, &dir, VAULT_KEY)?;
    Ok(dir)
}

fn step_a(report: &mut Report, source: &Source, run: &Path) -> Result<(), String> {
    let p = format!("A.{}", source.label);
    let dir = prepare_workdir(source, run, "a")?;
    let names: BTreeSet<String> = source.values.keys().cloned().collect();

    let raw_keys = stronghold_key_names(&dir)?;
    report.fact(
        &format!("{p}.source"),
        json!({
            "secret_count": source.values.len(),
            "scopes": scope_counts(names.iter().cloned()),
            "has___manifest": raw_keys.contains("__manifest"),
            "has___manifest_v2": raw_keys.contains("__manifest_v2"),
            "value_lengths": source.values.values().map(String::len).collect::<Vec<_>>(),
        }),
    );

    let outcome = run_with_service(&dir, REHEARSAL_SERVICE);
    report.check(
        &format!("{p}.migrate_returns_migrated_with_source_count"),
        matches!(outcome, Ok(MigrationOutcome::Migrated { count }) if count == source.values.len()),
    );
    if let Err(error) = &outcome {
        report.fact(&format!("{p}.migrate_error"), json!(error));
    }

    let store = raw_store(&dir)?;
    let mut all_equal = true;
    for (name, expected) in &source.values {
        match store.get(name) {
            Ok(Some(actual)) if actual == *expected => {}
            _ => all_equal = false,
        }
    }
    report.check(
        &format!("{p}.every_scoped_key_readable_and_equal"),
        all_equal,
    );
    report.check(
        &format!("{p}.export_all_equals_source"),
        store
            .export_all()
            .map(|all| all == source.values)
            .unwrap_or(false),
    );
    let listed: BTreeSet<String> = store
        .list_meta()
        .map(|metas| metas.into_iter().map(|meta| meta.name).collect())
        .unwrap_or_default();
    report.check(
        &format!("{p}.manifests_not_migrated"),
        !listed.contains("__manifest") && !listed.contains("__manifest_v2"),
    );
    report.check(
        &format!("{p}.index_lists_exactly_source_names"),
        listed == names,
    );

    let index_bytes = fs::read(dir.join(INDEX)).unwrap_or_default();
    let index_names: Option<BTreeSet<String>> = serde_json::from_slice::<Vec<String>>(&index_bytes)
        .ok()
        .map(|names| names.into_iter().collect());
    let index_text = String::from_utf8_lossy(&index_bytes);
    report.check(
        &format!("{p}.secrets_index_is_names_only"),
        index_names.as_ref() == Some(&names)
            && source
                .values
                .values()
                .all(|value| value.is_empty() || !index_text.contains(value.as_str())),
    );
    report.check(
        &format!("{p}.backup_present_and_byte_identical"),
        sha256_file(&dir.join(BACKUP)).as_deref() == Some(source.sha256.as_str()),
    );
    report.check(
        &format!("{p}.legacy_snapshot_retired"),
        !dir.join(LEGACY).exists(),
    );
    report.check(&format!("{p}.marker_written"), dir.join(MARKER).exists());
    report.check(
        &format!("{p}.rerun_is_already_complete"),
        matches!(
            run_with_service(&dir, REHEARSAL_SERVICE),
            Ok(MigrationOutcome::AlreadyComplete)
        ),
    );
    drop(store);

    let rolled_back = rollback_with_service(&dir, REHEARSAL_SERVICE);
    report.check(&format!("{p}.rollback_ok"), matches!(rolled_back, Ok(true)));
    if let Err(error) = &rolled_back {
        report.fact(&format!("{p}.rollback_error"), json!(error));
    }
    report.check(
        &format!("{p}.rollback_restores_snapshot_bytes"),
        sha256_file(&dir.join(LEGACY)).as_deref() == Some(source.sha256.as_str()),
    );
    report.check(
        &format!("{p}.rollback_snapshot_readable_and_equal"),
        read_legacy(&dir.join(LEGACY), &dir.join(VAULT_KEY))
            .map(|values| values == source.values)
            .unwrap_or(false),
    );
    report.check(
        &format!("{p}.rollback_retires_keychain_state"),
        count_present(&run.join("probe-a").join(source.label), &names)? == 0
            && !dir.join(INDEX).exists()
            && !dir.join(MARKER).exists()
            && !dir.join(BACKUP).exists(),
    );
    report.check(
        &format!("{p}.rollback_marker_blocks_remigration"),
        dir.join(ROLLBACK_MARKER).exists() && run_with_service(&dir, REHEARSAL_SERVICE).is_err(),
    );
    Ok(())
}

// ------------------------------------------------------------------- step B

fn step_b(
    report: &mut Report,
    source: &Source,
    run: &Path,
    passphrase: &str,
) -> Result<(), String> {
    let p = format!("B.{}", source.label);
    let dir = prepare_workdir(source, run, "b")?;
    let migrated = run_with_service(&dir, REHEARSAL_SERVICE);
    report.check(&format!("{p}.migrated"), migrated.is_ok());

    // Same composition as `commands::secrets::ensure_store`.
    let keyring = Arc::new(raw_store(&dir)?);
    keyring.persist_index().map_err(String::from)?;
    let unlock = UnlockState::with_path(dir.join(UNLOCK_ENVELOPE_FILENAME), DEFAULT_IDLE_TIMEOUT);
    let store = EncryptedStore::new(keyring.clone(), Arc::new(unlock.clone()));

    // Same sequence as `secrets_set_passphrase`.
    let set = store
        .probe()
        .and_then(|_| store.detect_configuration().map(|_| ()))
        .map_err(String::from)
        .and_then(|_| {
            unlock
                .set_or_rotate(passphrase, None)
                .map_err(|e| e.to_string())
        })
        .and_then(|_| store.prepare_encryption().map_err(String::from));
    report.check(
        &format!("{p}.set_passphrase_and_prepare_encryption"),
        set.is_ok(),
    );
    if let Err(error) = &set {
        report.fact(&format!("{p}.set_error"), json!(error));
    }
    report.check(
        &format!("{p}.envelope_written"),
        dir.join(UNLOCK_ENVELOPE_FILENAME).exists(),
    );

    let mut every_encrypted = true;
    for (name, plain) in &source.values {
        match keyring.get(name) {
            Ok(Some(raw)) => {
                if !raw.starts_with(ENCRYPTED_PREFIX)
                    || raw == *plain
                    || (!plain.is_empty() && raw.contains(plain.as_str()))
                {
                    every_encrypted = false;
                }
            }
            _ => every_encrypted = false,
        }
    }
    report.check(
        &format!("{p}.existing_values_encrypted_at_rest"),
        every_encrypted,
    );
    report.check(
        &format!("{p}.unlocked_reads_equal_originals"),
        store
            .export_all()
            .map(|all| all == source.values)
            .unwrap_or(false),
    );

    let locked = unlock.lock().is_ok();
    let probe_name = source
        .values
        .keys()
        .next()
        .cloned()
        .unwrap_or_else(|| "workspace::REHEARSAL_ABSENT".to_string());
    report.check(
        &format!("{p}.lock_then_get_returns_locked"),
        locked
            && store
                .get(&probe_name)
                .err()
                .is_some_and(|error| error.is_locked())
            && unlock.state().locked,
    );
    report.check(
        &format!("{p}.wrong_passphrase_rejected"),
        matches!(
            unlock.unlock("rehearsal-wrong-passphrase"),
            Err(UnlockError::WrongPassphrase)
        ),
    );
    // A fresh process (new UnlockState over the same envelope) must also
    // reject the wrong passphrase and accept the right one.
    let fresh = UnlockState::with_path(dir.join(UNLOCK_ENVELOPE_FILENAME), DEFAULT_IDLE_TIMEOUT);
    report.check(
        &format!("{p}.fresh_process_wrong_passphrase_rejected"),
        matches!(
            fresh.unlock("rehearsal-wrong-passphrase"),
            Err(UnlockError::WrongPassphrase)
        ),
    );
    let unlocked = unlock.unlock(passphrase).is_ok() && store.prepare_encryption().is_ok();
    report.check(
        &format!("{p}.unlock_then_values_equal_originals"),
        unlocked
            && store
                .export_all()
                .map(|all| all == source.values)
                .unwrap_or(false),
    );

    // Retire: remove every item this step wrote.
    keyring
        .replace_all(&BTreeMap::new())
        .map_err(|error| format!("retire B items: {error}"))?;
    Ok(())
}

fn step_b_passthrough(report: &mut Report, source: &Source, run: &Path) -> Result<(), String> {
    let p = format!("B.{}.no_passphrase", source.label);
    let dir = prepare_workdir(source, run, "b-plain")?;
    report.check(
        &format!("{p}.migrated"),
        run_with_service(&dir, REHEARSAL_SERVICE).is_ok(),
    );
    let keyring = Arc::new(raw_store(&dir)?);
    keyring.persist_index().map_err(String::from)?;
    let unlock = UnlockState::with_path(dir.join(UNLOCK_ENVELOPE_FILENAME), DEFAULT_IDLE_TIMEOUT);
    let store = EncryptedStore::new(keyring.clone(), Arc::new(unlock.clone()));

    report.check(&format!("{p}.state_not_locked"), !unlock.state().locked);
    report.check(
        &format!("{p}.reads_equal_originals"),
        store
            .export_all()
            .map(|all| all == source.values)
            .unwrap_or(false),
    );
    let mut raw_is_plain = true;
    for (name, plain) in &source.values {
        if !matches!(keyring.get(name), Ok(Some(raw)) if raw == *plain) {
            raw_is_plain = false;
        }
    }
    report.check(
        &format!("{p}.values_stay_plaintext_passthrough"),
        raw_is_plain,
    );
    let write_name = "workspace::REHEARSAL_PASSTHROUGH_WRITE";
    let write_value = format!("rehearsal-{}", uuid::Uuid::new_v4().simple());
    let wrote = store.set(write_name, &write_value).is_ok()
        && matches!(keyring.get(write_name), Ok(Some(raw)) if raw == write_value)
        && matches!(store.get(write_name), Ok(Some(value)) if value == write_value);
    report.check(&format!("{p}.new_write_passthrough"), wrote);
    report.check(
        &format!("{p}.no_envelope_created"),
        !dir.join(UNLOCK_ENVELOPE_FILENAME).exists(),
    );
    keyring
        .replace_all(&BTreeMap::new())
        .map_err(|error| format!("retire passthrough items: {error}"))?;
    Ok(())
}

// ------------------------------------------------------------------- step C

async fn step_c(report: &mut Report, source_dir: &Path, run: &Path) -> Result<(), String> {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let db_dir = run.join("c-db");
    fs::create_dir_all(&db_dir).map_err(|e| format!("mkdir db: {e}"))?;
    for name in ["ikenga.db", "ikenga.db-wal", "ikenga.db-shm"] {
        if source_dir.join(name).exists() {
            copy_into(source_dir, &db_dir, name)?;
        }
    }
    let options = SqliteConnectOptions::new()
        .filename(db_dir.join("ikenga.db"))
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|e| format!("open copied db: {e}"))?;

    let real_home = crate::platform::home_dir().ok_or("no home dir")?;
    let real_personal = personal_path(&real_home);
    let real_personal_before = untouched_fingerprint(&real_personal);

    let real_roots = project_roots_from_pool(&pool).await?;
    let real_project_before: Vec<Value> = real_roots
        .values()
        .map(|root| untouched_fingerprint(&project_path(root)))
        .collect();

    let home = run.join("c-home");
    fs::create_dir_all(&home).map_err(|e| format!("mkdir home: {e}"))?;
    let mut roots = std::collections::HashMap::new();
    let mut ids: Vec<&String> = real_roots.keys().collect();
    ids.sort();
    for (index, id) in ids.iter().enumerate() {
        let root = run.join("c-projects").join(format!("p{index}"));
        fs::create_dir_all(&root).map_err(|e| format!("mkdir project: {e}"))?;
        roots.insert((*id).clone(), root);
    }
    report.check(
        "C.all_writes_confined_to_rehearsal_dir",
        home.starts_with(run) && roots.values().all(|root| root.starts_with(run)),
    );

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings_kv ORDER BY key ASC")
            .fetch_all(&pool)
            .await
            .map_err(|e| format!("read settings_kv: {e}"))?;
    let bound: Vec<&(String, String)> = rows
        .iter()
        .filter(|(key, _)| schema::legacy_binding(key).is_some())
        .collect();
    let unbound: Vec<&String> = rows
        .iter()
        .filter(|(key, _)| schema::legacy_binding(key).is_none())
        .map(|(key, _)| key)
        .collect();
    let marker_before = rows
        .iter()
        .find(|(key, _)| key == FILE_MIGRATION_KEY)
        .map(|(_, value)| value.clone());
    report.fact(
        "C.source",
        json!({
            "kv_rows": rows.len(),
            "settings_owned_rows": bound.len(),
            "settings_owned_keys": bound.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            "other_domain_keys": unbound,
            "file_migration_marker_before": marker_before,
            "active_project_roots": real_roots.len(),
        }),
    );

    let migrated = migrate_from_kv(&pool, &home, &roots).await;
    report.check("C.migrate_from_kv_ok", migrated.is_ok());
    match &migrated {
        Ok(result) => report.fact(
            "C.migration_report",
            json!({
                "personal_created": result.personal_created,
                "project_files_created": result.project_files_created,
                "migrated_keys": result.migrated_keys,
                "skipped_keys": result.skipped_keys,
                "already_migrated": result.already_migrated,
            }),
        ),
        Err(error) => report.fact("C.migrate_error", json!(error)),
    }

    let personal_file = personal_path(&home);
    let personal = read_document(&personal_file).ok().flatten();
    report.check("C.personal_settings_written", personal.is_some());
    report.check(
        "C.personal_settings_validates",
        personal.as_ref().is_some_and(|doc| doc.validate().is_ok())
            && fs::read(&personal_file)
                .ok()
                .is_some_and(|bytes| SettingsDocument::parse(&bytes).is_ok()),
    );
    let personal = personal.unwrap_or_default();

    let mut without_destination = Vec::new();
    for (key, raw) in &bound {
        let binding = schema::legacy_binding(key).expect("filtered to bound keys");
        let document = match binding.target {
            LegacyTarget::Personal => Some(personal.clone()),
            LegacyTarget::Project => schema::legacy_project_id(key)
                .and_then(|id| roots.get(id))
                .and_then(|root| read_document(&project_path(root)).ok().flatten()),
        };
        let present = document
            .as_ref()
            .is_some_and(|doc| schema::legacy_value_present(doc, key));
        // An empty legacy value is a deletion: it must NOT be present.
        let represented = if raw.is_empty() { !present } else { present };
        if !represented {
            without_destination.push(key.clone());
        }
    }
    report.fact("C.rows_without_destination", json!(without_destination));
    report.check(
        "C.every_settings_owned_kv_row_has_a_destination",
        without_destination.is_empty(),
    );
    let marker_after: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings_kv WHERE key = ?")
            .bind(FILE_MIGRATION_KEY)
            .fetch_optional(&pool)
            .await
            .map_err(|e| format!("read marker: {e}"))?;
    report.check(
        "C.file_migration_marker_done",
        marker_after.as_deref() == Some("done"),
    );
    report.check(
        "C.rerun_is_already_migrated",
        migrate_from_kv(&pool, &home, &roots)
            .await
            .is_ok_and(|result| result.already_migrated),
    );

    // Project override, then revert by removing the override field.
    let project_root = match roots.values().next() {
        Some(root) => root.clone(),
        None => {
            let root = run.join("c-projects").join("synthetic");
            fs::create_dir_all(&root).map_err(|e| format!("mkdir synthetic project: {e}"))?;
            report.fact(
                "C.override_project",
                json!("synthetic (no active project roots)"),
            );
            root
        }
    };
    let personal_effective = effective_document(SettingsScope::Project, &personal, None, true);
    let personal_value = personal_effective.get_field(OVERRIDE_FIELD).cloned();
    let override_value = ["A", "B", "C"]
        .into_iter()
        .map(|value| Value::String(value.to_string()))
        .find(|value| Some(value) != personal_value.as_ref())
        .expect("three themes, one personal value");
    let project_file = project_path(&project_root);
    let mut project_doc = read_document(&project_file)
        .ok()
        .flatten()
        .unwrap_or_default();
    let personal_sha_before = sha256_file(&personal_file);
    project_doc
        .set_field(OVERRIDE_FIELD, override_value.clone())
        .map_err(|e| format!("set override: {e}"))?;
    write_document(&project_file, &project_doc)?;
    let reread = read_document(&project_file)
        .ok()
        .flatten()
        .unwrap_or_default();
    let overlay = reread.project_overlay();
    let effective = effective_document(SettingsScope::Project, &personal, Some(&overlay), true);
    report.check(
        "C.project_file_overrides_personal",
        effective.get_field(OVERRIDE_FIELD) == Some(&override_value) && reread.validate().is_ok(),
    );

    let mut reverted = reread.clone();
    reverted
        .remove_field(OVERRIDE_FIELD)
        .map_err(|e| format!("remove override: {e}"))?;
    write_document(&project_file, &reverted)?;
    let reread = read_document(&project_file)
        .ok()
        .flatten()
        .unwrap_or_default();
    let overlay = reread.project_overlay();
    let effective = effective_document(SettingsScope::Project, &personal, Some(&overlay), true);
    report.check(
        "C.removing_override_reverts_to_personal",
        reread.get_field(OVERRIDE_FIELD).is_none()
            && effective.get_field(OVERRIDE_FIELD) == personal_value.as_ref(),
    );
    report.check(
        "C.revert_did_not_write_personal_file",
        sha256_file(&personal_file) == personal_sha_before,
    );

    report.check(
        "C.real_personal_settings_untouched",
        untouched_fingerprint(&real_personal) == real_personal_before,
    );
    let real_project_after: Vec<Value> = real_roots
        .values()
        .map(|root| untouched_fingerprint(&project_path(root)))
        .collect();
    report.check(
        "C.real_project_settings_untouched",
        real_project_after == real_project_before,
    );
    pool.close().await;
    Ok(())
}

// ------------------------------------------------------------------- driver

fn real_app_data_dirs() -> Vec<PathBuf> {
    ["APPDATA", "LOCALAPPDATA"]
        .into_iter()
        .filter_map(|var| std::env::var_os(var))
        .map(|base| PathBuf::from(base).join(APP_IDENTIFIER))
        .filter_map(|dir| fs::canonicalize(dir).ok())
        .collect()
}

#[tokio::test]
#[ignore = "WP-37 rehearsal: needs IKENGA_REHEARSAL_5A_DIR pointing at a copy of app data"]
async fn rehearsal_5a() {
    let Some(root) = std::env::var_os(ENV_DIR).map(PathBuf::from) else {
        println!("[rehearsal-5a] {ENV_DIR} not set; skipping");
        return;
    };
    assert_ne!(REHEARSAL_SERVICE, PRODUCTION_SERVICE);
    let root = fs::canonicalize(&root).expect("rehearsal root exists");
    for real in real_app_data_dirs() {
        assert!(
            !root.starts_with(&real) && !real.starts_with(&root),
            "rehearsal root overlaps the real app data dir"
        );
    }
    let source_dir = root.join("source");
    assert!(source_dir.join(VAULT_KEY).exists() || !source_dir.join(LEGACY).exists());

    install_capture().expect("log capture installs first");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let run = root.join(format!("run-{stamp}"));
    fs::create_dir_all(&run).unwrap();
    let mut report = Report::default();

    // Sources: the real copy (if it has a snapshot) and a synthetic fixture
    // covering every scope shape.
    let mut sources = Vec::new();
    if source_dir.join(LEGACY).exists() {
        let values = read_legacy(&source_dir.join(LEGACY), &source_dir.join(VAULT_KEY));
        report.check("A.real.source_snapshot_readable", values.is_ok());
        if let Err(error) = &values {
            report.fact("A.real.source_error", json!(error));
        }
        if let Ok(values) = values {
            sources.push(Source {
                label: "real",
                dir: source_dir.clone(),
                sha256: sha256_file(&source_dir.join(LEGACY)).unwrap(),
                values,
            });
        }
    } else {
        report.fact("A.real.source", json!("no Stronghold snapshot in the copy"));
    }
    let synthetic_dir = run.join("synthetic-source");
    fs::create_dir_all(&synthetic_dir).unwrap();
    let synthetic_values = write_synthetic_stronghold(&synthetic_dir).expect("synthetic fixture");
    sources.push(Source {
        label: "synthetic",
        sha256: sha256_file(&synthetic_dir.join(LEGACY)).unwrap(),
        dir: synthetic_dir.clone(),
        values: synthetic_values,
    });

    let passphrase = format!("rehearsal-pass-{}", uuid::Uuid::new_v4().simple());
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    for source in &sources {
        all_names.extend(source.values.keys().cloned());
    }
    all_names.insert("workspace::REHEARSAL_PASSTHROUGH_WRITE".to_string());
    let leftovers = sweep_rehearsal_items(&run.join("sweep-before"), &all_names)
        .expect("pre-sweep of the rehearsal namespace");
    report.fact("keychain.leftovers_removed_before_run", json!(leftovers));

    for source in &sources {
        if let Err(error) = step_a(&mut report, source, &run) {
            report.check(&format!("A.{}.completed", source.label), false);
            report.fact(&format!("A.{}.error", source.label), json!(error));
        }
        if let Err(error) = step_b(&mut report, source, &run, &passphrase) {
            report.check(&format!("B.{}.completed", source.label), false);
            report.fact(&format!("B.{}.error", source.label), json!(error));
        }
        if let Err(error) = step_b_passthrough(&mut report, source, &run) {
            report.check(
                &format!("B.{}.no_passphrase.completed", source.label),
                false,
            );
            report.fact(
                &format!("B.{}.no_passphrase.error", source.label),
                json!(error),
            );
        }
    }
    if let Err(error) = step_c(&mut report, &source_dir, &run).await {
        report.check("C.completed", false);
        report.fact("C.error", json!(error));
    }

    // Keychain cleanup: sweep every name ever written, then prove absence.
    let swept = sweep_rehearsal_items(&run.join("sweep-after"), &all_names);
    report.check("keychain.sweep_after_ok", swept.is_ok());
    let remaining = count_present(&run.join("sweep-verify"), &all_names).unwrap_or(usize::MAX);
    report.fact("keychain.items_remaining_after_cleanup", json!(remaining));
    report.check("keychain.rehearsal_namespace_empty", remaining == 0);

    // D: scan every captured record for every secret value (in-process).
    let mut secrets: Vec<String> = Vec::new();
    for source in &sources {
        secrets.extend(source.values.values().filter(|v| !v.is_empty()).cloned());
        if let Ok(key) = fs::read_to_string(source.dir.join(VAULT_KEY)) {
            secrets.push(key.trim().to_string());
        }
    }
    secrets.push(passphrase.clone());
    let records = captured().lock().unwrap().records.clone();
    let mut hits = 0usize;
    let mut hit_targets: BTreeSet<String> = BTreeSet::new();
    for (target, text) in &records {
        for secret in &secrets {
            if text.contains(secret.as_str()) {
                hits += 1;
                hit_targets.insert(target.clone());
            }
        }
    }
    let targets: BTreeSet<&String> = records.iter().map(|(target, _)| target).collect();
    report.fact(
        "D.log_scan",
        json!({
            "records_captured": records.len(),
            "distinct_targets": targets.len(),
            "secrets_scanned": secrets.len(),
            "min_secret_len": secrets.iter().map(String::len).min(),
            "hits": hits,
            "hit_targets": hit_targets,
        }),
    );
    report.check("D.no_secret_value_in_logs", hits == 0);

    // Also scan every file the run wrote (plaintext stores, markers, settings).
    let mut file_hits = 0usize;
    let mut files_scanned = 0usize;
    let mut stack = vec![run.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_name().is_some_and(|name| name == VAULT_KEY) {
                continue;
            }
            files_scanned += 1;
            let bytes = fs::read(&path).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            file_hits += secrets
                .iter()
                .filter(|secret| text.contains(secret.as_str()))
                .count();
        }
    }
    report.fact(
        "D.file_scan",
        json!({ "files_scanned": files_scanned, "hits": file_hits }),
    );
    report.check("D.no_secret_value_in_written_files", file_hits == 0);

    let results = json!({
        "service": REHEARSAL_SERVICE,
        "run_dir": run.to_string_lossy(),
        "checks": report.checks.iter().map(|(id, pass)| json!({"id": id, "pass": pass})).collect::<Vec<_>>(),
        "facts": report.facts,
        "failures": report.failures(),
    });
    let body = serde_json::to_string_pretty(&results).unwrap();
    assert!(
        secrets.iter().all(|secret| !body.contains(secret.as_str())),
        "results would contain a secret value; not written"
    );
    fs::write(run.join("results.json"), body).unwrap();
    println!(
        "[rehearsal-5a] checks={} failures={} results={}",
        report.checks.len(),
        report.failures(),
        run.join("results.json").display()
    );
    assert_eq!(
        report.failures(),
        0,
        "rehearsal checks failed; see results.json"
    );
}
