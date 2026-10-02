//! The per-principal secret store and its envelope (remote-access WP-21,
//! DEC-R18-1; G-PRINCIPAL §4, §5 rows 15–16, §9.3, §10).
//!
//! Mounted as `crate::secrets_env::principal` (compiled in the headless
//! build; `crate::secrets` itself is desktop-only). Unix-only: T1 is.
//!
//! # Keys (DEC-R18-1: a server-held KEK)
//!
//! ```text
//!  broker (root)                         principal child (uid)
//!  ─────────────                         ─────────────────────
//!  operator/secrets-kek.json  (KEK)
//!        │ HKDF-SHA256(salt, KEK, info = "…" ‖ key_version ‖ principal_id)
//!        ▼
//!  per-principal key  ── host-only env ──▶ IKENGA_SECRET__PRINCIPAL_KEY
//!                                         (read once, removed from env)
//!                                              │ AES-256-GCM unwrap
//!                                              ▼
//!                                    <data>/secrets/dek.json   (wrapped DEK)
//!                                              │ AES-256-GCM, AAD = id ‖ name
//!                                              ▼
//!                                    <data>/secrets/secrets.json (values)
//! ```
//!
//! * The **KEK** never leaves the broker. It is 32 random bytes, created on
//!   the first child launch of a T1 boot if absent, in the root-only
//!   `operator/` dir: `0600`, owned by the broker's euid, never a symlink.
//! * Each principal gets a **derived key** (HKDF with the principal id and a
//!   key-version byte in `info`), so one child's key opens only that
//!   principal's DEK. It reaches the child as a host-only environment
//!   variable: the T1 executor's env floor and the PTY denylist both drop
//!   `IKENGA_SECRET_*` (`pty::is_host_only_env`), and the child removes it
//!   from its own environment before it serves anything ([`take_handoff`]).
//! * Each store has its own random **DEK**, wrapped by the derived key.
//!   Values are sealed under the DEK with the principal id and the entry name
//!   as AAD, so a ciphertext copied to another name or principal fails.
//!
//! Background work (detached chi-runs, cron, MCP) can decrypt while the user
//! is signed out — that is the point. Root can decrypt — accepted (T1's
//! "mutually trusted team", D1). A password-derived key and a double wrap
//! were rejected for v1. Rotation is out of scope; the `key_version` byte is
//! carried in the derivation, the hand-off value and `dek.json` so a later
//! rotation can tell generations apart.
//!
//! **Accepted residue.** Linux keeps a process's *initial* environment in
//! `/proc/<pid>/environ`, readable by the same uid; removing the variable
//! does not scrub that copy. So a process running as the principal can read
//! the principal's derived key. That key opens nothing but the principal's
//! own store, which the same uid can already read through `secrets_get`, so
//! it widens nothing; the master KEK is never in any child's environment.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD, Engine};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::crypto::{self, DEK_LEN};
use super::store::{SecretMeta, SecretsStore, StoreError};
use crate::executor::PrincipalId;

/// The host-only variable the broker hands the derived key through. Inside
/// the `IKENGA_SECRET_*` namespace on purpose: that is what the T1 executor
/// floor and the PTY denylist already drop (`pty::is_host_only_env`), and
/// what `server/src/main.rs` does *not* strip before `run_server` (it strips
/// `IKENGA_VAULT_KEY` there). The double underscore makes it a reserved name
/// the `IKENGA_SECRET_*` RPC namespace never lists or serves
/// (`secrets_env::is_valid_key`).
pub const HANDOFF_ENV: &str = "IKENGA_SECRET__PRINCIPAL_KEY";

/// The KEK generation every key is derived for. Only `1` exists.
pub const KEY_VERSION: u8 = 1;

/// `operator/secrets-kek.json`.
pub const KEK_FILENAME: &str = "secrets-kek.json";
/// `<data>/secrets/`.
pub const SECRETS_DIR: &str = "secrets";
/// `<data>/secrets/dek.json`: the wrapped DEK.
pub const DEK_FILENAME: &str = "dek.json";
/// `<data>/secrets/secrets.json`: the sealed values.
pub const VALUES_FILENAME: &str = "secrets.json";

const KEY_LEN: usize = 32;
const FORMAT: u32 = 1;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const HKDF_SALT: &[u8] = b"ikenga.principal-secrets.v1";
const HKDF_INFO: &[u8] = b"ikenga/principal-secrets/dek-wrap-key";
const DEK_AAD: &[u8] = b"ikenga/principal-secrets/dek";
const VALUE_AAD: &[u8] = b"ikenga/principal-secrets/value";

// ─── HMAC / HKDF (RFC 2104 / RFC 5869) over the `sha2` dependency ─────────
//
// `hkdf` and `hmac` are in the lock only transitively; adding them as direct
// dependencies would touch `Cargo.toml`, which this wave does not own. Both
// are a few lines over `sha2`, and the RFC 5869 vectors pin them (tests).

const SHA256_BLOCK: usize = 64;

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    let mut block = Zeroizing::new([0u8; SHA256_BLOCK]);
    if key.len() > SHA256_BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = Zeroizing::new([0x36u8; SHA256_BLOCK]);
    let mut opad = Zeroizing::new([0x5cu8; SHA256_BLOCK]);
    for i in 0..SHA256_BLOCK {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad[..]);
    for part in parts {
        inner.update(part);
    }
    let inner = Zeroizing::new(<[u8; 32]>::from(inner.finalize()));
    let mut outer = Sha256::new();
    outer.update(&opad[..]);
    outer.update(&inner[..]);
    Zeroizing::new(outer.finalize().into())
}

/// HKDF-SHA256 for an output of at most one hash length (all this needs).
fn hkdf_sha256_32(salt: &[u8], ikm: &[u8], info: &[&[u8]]) -> Zeroizing<[u8; 32]> {
    let prk = hmac_sha256(salt, &[ikm]);
    let mut parts: Vec<&[u8]> = info.to_vec();
    parts.push(&[1u8]);
    hmac_sha256(&prk[..], &parts)
}

// ─── the broker's KEK ──────────────────────────────────────────────────────

/// The master key-encryption key. Held by the broker only.
pub struct BrokerKek(Zeroizing<[u8; KEY_LEN]>);

impl std::fmt::Debug for BrokerKek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrokerKek(<redacted>)")
    }
}

#[derive(Serialize, Deserialize)]
struct KekFile {
    version: u8,
    kek: String,
}

impl BrokerKek {
    /// Load `operator_dir/secrets-kek.json`, creating it (0600) if absent.
    /// Refuses a symlink, a non-regular file, a file not owned by this
    /// process's euid or one with any group/other permission bit, and a
    /// `version` other than [`KEY_VERSION`]. Two concurrent first launches
    /// agree on one key: the new file is published with `link(2)`, which
    /// never replaces an existing one.
    pub fn load_or_create(operator_dir: &Path) -> io::Result<Self> {
        let path = operator_dir.join(KEK_FILENAME);
        match read_private(&path, 4096) {
            Ok(body) => return Self::parse(&path, &body),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        OsRng.fill_bytes(&mut key[..]);
        let body = Zeroizing::new(
            serde_json::to_vec(&KekFile {
                version: KEY_VERSION,
                kek: hex::encode(&key[..]),
            })
            .map_err(io::Error::other)?,
        );
        let tmp = operator_dir.join(format!(".{KEK_FILENAME}.{}.tmp", hex::encode(nonce8())));
        write_new_private(&tmp, &body)?;
        let linked = fs::hard_link(&tmp, &path);
        let _ = fs::remove_file(&tmp);
        match linked {
            Ok(()) => {
                sync_dir(operator_dir);
                tracing::info!("created the principal-secrets KEK at {}", path.display());
                Ok(Self(key))
            }
            // Another launch won the race: use its key.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                Self::parse(&path, &read_private(&path, 4096)?)
            }
            Err(e) => Err(e),
        }
    }

    fn parse(path: &Path, body: &[u8]) -> io::Result<Self> {
        let bad = |why: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {why}", path.display()),
            )
        };
        let file: KekFile = serde_json::from_slice(body).map_err(|_| bad("not a KEK file"))?;
        if file.version != KEY_VERSION {
            return Err(bad(&format!(
                "KEK version {} is not supported (this build knows {KEY_VERSION}; rotation is \
                 not implemented)",
                file.version
            )));
        }
        let raw = Zeroizing::new(hex::decode(&file.kek).map_err(|_| bad("KEK is not hex"))?);
        if raw.len() != KEY_LEN {
            return Err(bad("KEK is not 32 bytes"));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&raw);
        Ok(Self(key))
    }

    #[cfg(test)]
    pub(crate) fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The key `id`'s child gets. Deterministic, so a respawned child (or a
    /// rebooted broker) unwraps the same DEK.
    pub fn derive(&self, id: PrincipalId) -> PrincipalKey {
        let id_text = id.to_string();
        let key = hkdf_sha256_32(
            HKDF_SALT,
            &self.0[..],
            &[HKDF_INFO, &[0u8, KEY_VERSION], id_text.as_bytes()],
        );
        PrincipalKey {
            id,
            version: KEY_VERSION,
            key,
        }
    }
}

// ─── the hand-off ──────────────────────────────────────────────────────────

/// A principal's derived key, as the broker hands it to that principal's
/// child.
pub struct PrincipalKey {
    id: PrincipalId,
    version: u8,
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for PrincipalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalKey")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl PrincipalKey {
    pub fn id(&self) -> PrincipalId {
        self.id
    }

    pub fn version(&self) -> u8 {
        self.version
    }

    /// `<version>:<principal id>:<hex key>`. The id rides along so the child
    /// can check the key is for the data dir it was given.
    pub fn to_env_value(&self) -> Zeroizing<String> {
        Zeroizing::new(format!(
            "{}:{}:{}",
            self.version,
            self.id,
            hex::encode(&self.key[..])
        ))
    }

    pub fn from_env_value(value: &str) -> Result<Self, String> {
        let bad = || format!("{HANDOFF_ENV} is malformed");
        let mut parts = value.splitn(3, ':');
        let (Some(version), Some(id), Some(key)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad());
        };
        let version: u8 = version.parse().map_err(|_| bad())?;
        if version != KEY_VERSION {
            return Err(format!(
                "{HANDOFF_ENV} carries key version {version}; this build knows {KEY_VERSION}"
            ));
        }
        let id: PrincipalId = id.parse().map_err(|_| bad())?;
        let raw = Zeroizing::new(hex::decode(key).map_err(|_| bad())?);
        if raw.len() != KEY_LEN {
            return Err(bad());
        }
        let mut out = Zeroizing::new([0u8; KEY_LEN]);
        out.copy_from_slice(&raw);
        Ok(Self {
            id,
            version,
            key: out,
        })
    }
}

/// Serializes the tests that set [`HANDOFF_ENV`] (process-global).
#[cfg(test)]
pub(crate) static HANDOFF_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Read [`HANDOFF_ENV`] and remove it from this process's environment, so
/// nothing the child spawns later (an engine, a pkg sidecar, a call site that
/// forgets the PTY filter) can inherit it. Always removes it, also when it is
/// malformed. `Ok(None)` when it is not set (T0, or a broker without one).
pub fn take_handoff() -> Result<Option<PrincipalKey>, String> {
    let value = std::env::var_os(HANDOFF_ENV);
    if value.is_none() {
        return Ok(None);
    }
    std::env::remove_var(HANDOFF_ENV);
    let value = Zeroizing::new(
        value
            .and_then(|v| v.into_string().ok())
            .ok_or_else(|| format!("{HANDOFF_ENV} is not UTF-8"))?,
    );
    PrincipalKey::from_env_value(&value).map(Some)
}

// ─── the per-principal store ───────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DekFile {
    format: u32,
    key_version: u8,
    principal: String,
    /// base64(nonce ‖ AES-256-GCM(DEK)).
    wrapped_dek: String,
}

#[derive(Serialize, Deserialize, Default)]
struct ValuesFile {
    format: u32,
    /// name → base64(nonce ‖ AES-256-GCM(value)).
    entries: BTreeMap<String, String>,
}

/// One principal's encrypted store in `<data>/secrets/`. The child owns the
/// directory (its uid, 0700) and is its only opener (the data-dir flock,
/// I-3); the mutex serializes this process's read-modify-write cycles.
pub struct PrincipalStore {
    id: PrincipalId,
    dir: PathBuf,
    dek: Zeroizing<[u8; DEK_LEN]>,
    io: Mutex<()>,
}

impl std::fmt::Debug for PrincipalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalStore")
            .field("id", &self.id)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

fn dek_aad(id: PrincipalId, version: u8) -> Vec<u8> {
    let mut aad = DEK_AAD.to_vec();
    aad.extend_from_slice(&[0, version, 0]);
    aad.extend_from_slice(id.to_string().as_bytes());
    aad
}

fn value_aad(id: PrincipalId, name: &str) -> Vec<u8> {
    let mut aad = VALUE_AAD.to_vec();
    aad.push(0);
    aad.extend_from_slice(id.to_string().as_bytes());
    aad.push(0);
    aad.extend_from_slice(name.as_bytes());
    aad
}

impl PrincipalStore {
    /// Open (creating on first use) the store under `data_dir/secrets/`.
    ///
    /// * The directory is created 0700; an existing one must be a real
    ///   directory owned by this euid with no group/other bits.
    /// * A missing `dek.json` mints a DEK — unless `secrets.json` exists,
    ///   which would mean ciphertext under a lost key: refused, never
    ///   overwritten.
    /// * A `dek.json` that does not unwrap with `key` (another principal's
    ///   key, a replaced KEK) is refused.
    pub fn open(data_dir: &Path, key: &PrincipalKey) -> Result<Self, StoreError> {
        let dir = data_dir.join(SECRETS_DIR);
        ensure_private_dir(&dir).map_err(|e| {
            StoreError::unavailable(format!("principal secret store {}: {e}", dir.display()))
        })?;
        let dek_path = dir.join(DEK_FILENAME);
        let dek = match read_private(&dek_path, 4096) {
            Ok(body) => unwrap_dek_file(&body, key)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if fs::symlink_metadata(dir.join(VALUES_FILENAME)).is_ok() {
                    return Err(StoreError::unavailable(format!(
                        "{} is missing but {} exists: refusing to mint a new data key over \
                         existing ciphertext",
                        dek_path.display(),
                        VALUES_FILENAME
                    )));
                }
                let dek = crypto::random_dek();
                let body = wrap_dek_file(&dek, key)?;
                write_private_atomic(&dek_path, &body).map_err(|e| {
                    StoreError::unavailable(format!("write {}: {e}", dek_path.display()))
                })?;
                dek
            }
            Err(e) => {
                return Err(StoreError::unavailable(format!(
                    "{}: {e}",
                    dek_path.display()
                )))
            }
        };
        Ok(Self {
            id: key.id,
            dir,
            dek,
            io: Mutex::new(()),
        })
    }

    pub fn principal(&self) -> PrincipalId {
        self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn values_path(&self) -> PathBuf {
        self.dir.join(VALUES_FILENAME)
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self) -> Result<ValuesFile, StoreError> {
        let path = self.values_path();
        match read_private(&path, MAX_FILE_BYTES) {
            Ok(body) => {
                let file: ValuesFile = serde_json::from_slice(&body).map_err(|_| {
                    StoreError::unavailable(format!("{} is not a value file", path.display()))
                })?;
                if file.format != FORMAT {
                    return Err(StoreError::unavailable(format!(
                        "{}: unsupported format {}",
                        path.display(),
                        file.format
                    )));
                }
                Ok(file)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ValuesFile {
                format: FORMAT,
                entries: BTreeMap::new(),
            }),
            Err(e) => Err(StoreError::unavailable(format!("{}: {e}", path.display()))),
        }
    }

    fn save(&self, file: &ValuesFile) -> Result<(), StoreError> {
        let path = self.values_path();
        let body = serde_json::to_vec(file)
            .map_err(|e| StoreError::uncommitted(format!("serialize: {e}")))?;
        write_private_atomic(&path, &body)
            .map_err(|e| StoreError::uncommitted(format!("write {}: {e}", path.display())))
    }

    fn seal(&self, name: &str, value: &str) -> Result<String, StoreError> {
        let sealed = crypto::encrypt_value(&self.dek, value.as_bytes(), &value_aad(self.id, name))
            .map_err(|e| StoreError::uncommitted(e.to_string()))?;
        Ok(STANDARD.encode(sealed))
    }

    fn open_value(&self, name: &str, sealed: &str) -> Result<String, StoreError> {
        let raw = STANDARD
            .decode(sealed)
            .map_err(|_| StoreError::unknown(format!("secret {name:?} is not base64")))?;
        let plain = crypto::decrypt_value(&self.dek, &raw, &value_aad(self.id, name))
            .map_err(|_| StoreError::unknown(format!("secret {name:?} does not decrypt")))?;
        String::from_utf8(plain.to_vec())
            .map_err(|_| StoreError::unknown(format!("secret {name:?} is not UTF-8")))
    }
}

fn check_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty() || name.len() > 1024 || name.contains('\0') {
        return Err(StoreError::invalid("invalid secret name"));
    }
    Ok(())
}

impl SecretsStore for PrincipalStore {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
        check_name(name)?;
        let _g = self.guard();
        let file = self.load()?;
        file.entries
            .get(name)
            .map(|sealed| self.open_value(name, sealed))
            .transpose()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
        check_name(name)?;
        let _g = self.guard();
        let mut file = self.load()?;
        file.entries
            .insert(name.to_string(), self.seal(name, value)?);
        self.save(&file)
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        check_name(name)?;
        let _g = self.guard();
        let mut file = self.load()?;
        if file.entries.remove(name).is_some() {
            self.save(&file)?;
        }
        Ok(())
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        let _g = self.guard();
        Ok(self
            .load()?
            .entries
            .into_keys()
            .map(|name| SecretMeta { name })
            .collect())
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        let _g = self.guard();
        let mut file = ValuesFile {
            format: FORMAT,
            entries: BTreeMap::new(),
        };
        for (name, value) in values {
            check_name(name)?;
            file.entries.insert(name.clone(), self.seal(name, value)?);
        }
        self.save(&file)?;
        Ok(values.len())
    }

    fn probe(&self) -> Result<(), StoreError> {
        let _g = self.guard();
        self.load().map(|_| ())
    }

    /// Every value is sealed on write; there is no plaintext format.
    fn prepare_encryption(&self) -> Result<(), StoreError> {
        Ok(())
    }

    /// Always encrypted at rest under the broker's KEK.
    fn detect_configuration(&self) -> Result<bool, StoreError> {
        Ok(true)
    }

    fn backend_label(&self) -> &'static str {
        "principal store (encrypted, operator-held key)"
    }
}

fn wrap_dek_file(dek: &[u8; DEK_LEN], key: &PrincipalKey) -> Result<Vec<u8>, StoreError> {
    let sealed = crypto::encrypt_value(&key.key, &dek[..], &dek_aad(key.id, key.version))
        .map_err(|e| StoreError::unavailable(e.to_string()))?;
    serde_json::to_vec(&DekFile {
        format: FORMAT,
        key_version: key.version,
        principal: key.id.to_string(),
        wrapped_dek: STANDARD.encode(sealed),
    })
    .map_err(|e| StoreError::unavailable(e.to_string()))
}

fn unwrap_dek_file(
    body: &[u8],
    key: &PrincipalKey,
) -> Result<Zeroizing<[u8; DEK_LEN]>, StoreError> {
    let bad = |why: String| StoreError::unavailable(format!("{DEK_FILENAME}: {why}"));
    let file: DekFile =
        serde_json::from_slice(body).map_err(|_| bad("not a wrapped-DEK file".into()))?;
    if file.format != FORMAT {
        return Err(bad(format!("unsupported format {}", file.format)));
    }
    if file.key_version != key.version {
        return Err(bad(format!(
            "wrapped for key version {}, but the broker handed over version {} (rotation is \
             not implemented)",
            file.key_version, key.version
        )));
    }
    if file.principal != key.id.to_string() {
        return Err(bad("belongs to another principal".into()));
    }
    let raw = STANDARD
        .decode(&file.wrapped_dek)
        .map_err(|_| bad("wrapped DEK is not base64".into()))?;
    let plain =
        crypto::decrypt_value(&key.key, &raw, &dek_aad(key.id, key.version)).map_err(|_| {
            bad(
                "the data key does not unwrap with the key the broker handed over (a replaced \
             operator KEK?)"
                    .into(),
            )
        })?;
    if plain.len() != DEK_LEN {
        return Err(bad("unwrapped DEK has the wrong length".into()));
    }
    let mut dek = Zeroizing::new([0u8; DEK_LEN]);
    dek.copy_from_slice(&plain);
    Ok(dek)
}

// ─── private file io ───────────────────────────────────────────────────────

fn nonce8() -> [u8; 8] {
    let mut n = [0u8; 8];
    OsRng.fill_bytes(&mut n);
    n
}

fn euid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::geteuid() }
}

/// Owned by this euid, no group/other bits.
fn check_private(meta: &fs::Metadata, path: &Path) -> io::Result<()> {
    let refuse = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing {}: {why}", path.display()),
        ))
    };
    if meta.uid() != euid() {
        return refuse(format!("owned by uid {}, not {}", meta.uid(), euid()));
    }
    if meta.mode() & 0o077 != 0 {
        return refuse(format!(
            "mode {:o} grants group/other access",
            meta.mode() & 0o7777
        ));
    }
    Ok(())
}

/// Read a private regular file: never through a symlink, bounded.
fn read_private(path: &Path, max: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("refusing {}: is a symlink", path.display()),
                )
            } else {
                e
            }
        })?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("refusing {}: not a regular file", path.display()),
        ));
    }
    check_private(&meta, path)?;
    if meta.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is larger than {max} bytes", path.display()),
        ));
    }
    let mut body = Zeroizing::new(Vec::with_capacity(meta.len() as usize));
    file.take(max + 1).read_to_end(&mut body)?;
    Ok(body)
}

/// Create `path` 0600 exclusively (never following or replacing anything).
fn write_new_private(path: &Path, body: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(body)?;
    file.sync_all()
}

/// Write a sibling temp file (0600, exclusive) and rename it over `path`.
fn write_private_atomic(path: &Path, body: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.tmp", hex::encode(nonce8())));
    if let Err(e) = write_new_private(&tmp, body) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(parent);
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// `<data>/secrets/`: create 0700, or check an existing one is a real
/// directory owned by this euid with no group/other bits (G-PRINCIPAL §4).
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "is not a directory (or is a symlink)",
                ));
            }
            check_private(&meta, dir)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_for(kek: &BrokerKek) -> (PrincipalId, PrincipalKey) {
        let id = PrincipalId::new_v7();
        (id, kek.derive(id))
    }

    /// RFC 5869 test case 1 (HKDF-SHA256), truncated to the 32 bytes the
    /// store uses, pins the hand-rolled HMAC/HKDF.
    #[test]
    fn hkdf_matches_rfc5869_case_1() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0x00u8..=0x0c).collect();
        let info: Vec<u8> = (0xf0u8..=0xf9).collect();
        let okm = hkdf_sha256_32(&salt, &ikm, &[&info]);
        assert_eq!(
            hex::encode(&okm[..]),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf"
        );
    }

    /// RFC 4231 test case 2 (HMAC-SHA256, key shorter than a block) and 6
    /// (key longer than a block, hashed first).
    #[test]
    fn hmac_matches_rfc4231() {
        assert_eq!(
            hex::encode(&hmac_sha256(b"Jefe", &[b"what do ya want ", b"for nothing?"])[..]),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex::encode(
                &hmac_sha256(
                    &[0xaau8; 131],
                    &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
                )[..]
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn derivation_is_deterministic_per_principal_and_kek() {
        let kek = BrokerKek::from_bytes([7; 32]);
        let id = PrincipalId::new_v7();
        assert_eq!(kek.derive(id).key[..], kek.derive(id).key[..]);
        assert_ne!(
            kek.derive(id).key[..],
            kek.derive(PrincipalId::new_v7()).key[..],
            "another principal, another key"
        );
        let other = BrokerKek::from_bytes([8; 32]);
        assert_ne!(kek.derive(id).key[..], other.derive(id).key[..]);
        assert_ne!(&kek.derive(id).key[..], &kek.0[..], "never the KEK itself");
        assert_eq!(kek.derive(id).version(), KEY_VERSION);
    }

    #[test]
    fn env_value_round_trips_and_carries_the_key_version() {
        let kek = BrokerKek::from_bytes([1; 32]);
        let (id, key) = key_for(&kek);
        let text = key.to_env_value();
        assert!(text.starts_with(&format!("{KEY_VERSION}:{id}:")));
        let back = PrincipalKey::from_env_value(&text).unwrap();
        assert_eq!(back.id(), id);
        assert_eq!(back.key[..], key.key[..]);
        // Debug never prints the key.
        assert!(!format!("{back:?}").contains(&hex::encode(&key.key[..])));

        let wrong_version = text.replacen(&format!("{KEY_VERSION}:"), "2:", 1);
        assert!(PrincipalKey::from_env_value(&wrong_version)
            .unwrap_err()
            .contains("key version 2"));
        assert!(PrincipalKey::from_env_value("1:not-an-id:00").is_err());
        assert!(PrincipalKey::from_env_value(&format!("1:{id}:abcd")).is_err());
        assert!(PrincipalKey::from_env_value("").is_err());
    }

    #[test]
    fn take_handoff_strips_the_variable_even_when_malformed() {
        let _env = HANDOFF_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(HANDOFF_ENV, "garbage");
        assert!(take_handoff().is_err());
        assert!(std::env::var_os(HANDOFF_ENV).is_none());

        let kek = BrokerKek::from_bytes([2; 32]);
        let (id, key) = key_for(&kek);
        std::env::set_var(HANDOFF_ENV, key.to_env_value().as_str());
        let taken = take_handoff().unwrap().unwrap();
        assert_eq!(taken.id(), id);
        assert!(
            std::env::var_os(HANDOFF_ENV).is_none(),
            "stripped after reading"
        );
        assert!(take_handoff().unwrap().is_none());
    }

    #[test]
    fn the_handoff_variable_is_host_only() {
        assert!(crate::pty::is_host_only_env(HANDOFF_ENV));
        assert!(
            !crate::secrets_env::is_valid_key(HANDOFF_ENV.strip_prefix("IKENGA_SECRET_").unwrap()),
            "reserved: never listed or served as an operator default"
        );
    }

    #[test]
    fn values_round_trip_and_persist_across_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([3; 32]);
        let (_, key) = key_for(&kek);
        let store = PrincipalStore::open(tmp.path(), &key).unwrap();
        assert_eq!(store.get("workspace::A").unwrap(), None);
        store.set("workspace::A", "alpha").unwrap();
        store.set("project::p1::B", "beta").unwrap();
        assert_eq!(store.get("workspace::A").unwrap().as_deref(), Some("alpha"));
        drop(store);

        let store = PrincipalStore::open(tmp.path(), &key).unwrap();
        let names: Vec<String> = store
            .list_meta()
            .unwrap()
            .into_iter()
            .map(|m| m.name)
            .collect();
        assert_eq!(names, vec!["project::p1::B", "workspace::A"]);
        assert_eq!(
            store.get("project::p1::B").unwrap().as_deref(),
            Some("beta")
        );
        store.delete("workspace::A").unwrap();
        store.delete("workspace::missing").unwrap();
        assert_eq!(store.get("workspace::A").unwrap(), None);

        // Nothing is stored in the clear.
        let raw = fs::read_to_string(tmp.path().join("secrets").join(VALUES_FILENAME)).unwrap();
        assert!(!raw.contains("beta"));
    }

    #[test]
    fn a_wrong_key_cannot_open_the_store() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([4; 32]);
        let (id, key) = key_for(&kek);
        PrincipalStore::open(tmp.path(), &key)
            .unwrap()
            .set("workspace::A", "alpha")
            .unwrap();

        // Same principal, replaced KEK.
        let other = BrokerKek::from_bytes([5; 32]).derive(id);
        let err = PrincipalStore::open(tmp.path(), &other).unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(err.to_string().contains("does not unwrap"), "{err}");

        // Another principal's key (same KEK).
        let (_, theirs) = key_for(&kek);
        let err = PrincipalStore::open(tmp.path(), &theirs).unwrap_err();
        assert!(err.to_string().contains("another principal"), "{err}");
    }

    #[test]
    fn the_dek_file_records_the_key_version() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([6; 32]);
        let (_, key) = key_for(&kek);
        PrincipalStore::open(tmp.path(), &key).unwrap();
        let path = tmp.path().join("secrets").join(DEK_FILENAME);
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(json["keyVersion"], u64::from(KEY_VERSION));
        json["keyVersion"] = 9.into();
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        let err = PrincipalStore::open(tmp.path(), &key).unwrap_err();
        assert!(err.to_string().contains("key version 9"), "{err}");
    }

    #[test]
    fn a_value_moved_to_another_name_does_not_decrypt() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([9; 32]);
        let (_, key) = key_for(&kek);
        let store = PrincipalStore::open(tmp.path(), &key).unwrap();
        store.set("workspace::A", "alpha").unwrap();
        let path = tmp.path().join("secrets").join(VALUES_FILENAME);
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let sealed = json["entries"]["workspace::A"].clone();
        json["entries"]["workspace::B"] = sealed;
        fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        assert!(store.get("workspace::B").is_err());
        assert_eq!(store.get("workspace::A").unwrap().as_deref(), Some("alpha"));
    }

    #[test]
    fn never_mints_a_dek_over_existing_ciphertext() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([10; 32]);
        let (_, key) = key_for(&kek);
        PrincipalStore::open(tmp.path(), &key)
            .unwrap()
            .set("workspace::A", "alpha")
            .unwrap();
        fs::remove_file(tmp.path().join("secrets").join(DEK_FILENAME)).unwrap();
        let err = PrincipalStore::open(tmp.path(), &key).unwrap_err();
        assert!(err.to_string().contains("refusing to mint"), "{err}");
    }

    #[test]
    fn modes_and_symlinks_are_checked() {
        let tmp = tempfile::tempdir().unwrap();
        let kek = BrokerKek::from_bytes([11; 32]);
        let (_, key) = key_for(&kek);
        let store = PrincipalStore::open(tmp.path(), &key).unwrap();
        store.set("workspace::A", "alpha").unwrap();
        let dir = tmp.path().join("secrets");
        let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o7777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(DEK_FILENAME)), 0o600);
        assert_eq!(mode(&dir.join(VALUES_FILENAME)), 0o600);

        // A group-readable value file is refused, not read.
        fs::set_permissions(dir.join(VALUES_FILENAME), fs::Permissions::from_mode(0o640)).unwrap();
        assert!(store.get("workspace::A").unwrap_err().is_unavailable());
        fs::set_permissions(dir.join(VALUES_FILENAME), fs::Permissions::from_mode(0o600)).unwrap();

        // An open directory is refused.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrincipalStore::open(tmp.path(), &key).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

        // A symlinked DEK file is refused.
        let real = tmp.path().join("elsewhere.json");
        fs::rename(dir.join(DEK_FILENAME), &real).unwrap();
        std::os::unix::fs::symlink(&real, dir.join(DEK_FILENAME)).unwrap();
        let err = PrincipalStore::open(tmp.path(), &key).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");

        // A symlinked secrets dir is refused.
        let tmp2 = tempfile::tempdir().unwrap();
        let target = tmp2.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, tmp2.path().join("secrets")).unwrap();
        assert!(PrincipalStore::open(tmp2.path(), &key).is_err());
    }

    #[test]
    fn kek_is_created_once_private_and_reused() {
        let tmp = tempfile::tempdir().unwrap();
        let op = tmp.path();
        let a = BrokerKek::load_or_create(op).unwrap();
        let path = op.join(KEK_FILENAME);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
        let b = BrokerKek::load_or_create(op).unwrap();
        assert_eq!(a.0[..], b.0[..], "a second boot reuses the KEK");
        let id = PrincipalId::new_v7();
        assert_eq!(a.derive(id).key[..], b.derive(id).key[..]);
        // No temp files left behind.
        assert_eq!(fs::read_dir(op).unwrap().count(), 1);

        // Group/other-readable: refused.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            BrokerKek::load_or_create(op).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        // A symlink: refused, never followed or replaced.
        let real = op.join("real-kek.json");
        fs::rename(&path, &real).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(
            BrokerKek::load_or_create(op).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn kek_owned_by_another_uid_is_refused() {
        if euid() != 0 {
            return; // needs root to chown
        }
        let tmp = tempfile::tempdir().unwrap();
        BrokerKek::load_or_create(tmp.path()).unwrap();
        let path = tmp.path().join(KEK_FILENAME);
        std::os::unix::fs::chown(&path, Some(65_000), None).unwrap();
        let err = BrokerKek::load_or_create(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("owned by uid 65000"), "{err}");
    }

    #[test]
    fn concurrent_first_launches_agree_on_one_kek() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || BrokerKek::load_or_create(&dir).unwrap())
            })
            .collect();
        let keys: Vec<BrokerKek> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for k in &keys[1..] {
            assert_eq!(k.0[..], keys[0].0[..]);
        }
    }
}
