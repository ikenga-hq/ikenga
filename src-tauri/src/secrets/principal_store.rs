//! The per-principal `SecretsStore` (remote-access WP-21; ADR-023 §4;
//! G-PRINCIPAL §4 `<data>/secrets/`, §5 rows 15–16, §10).
//!
//! # Envelope (founder decision DEC-R18-1: a server-held KEK)
//!
//! ```text
//! operator/secrets-kek            root 0600   32 random bytes; never leaves the broker
//!        │ HKDF-SHA256(salt = "ikenga/secrets-kek/v1",
//!        │             info = "ikenga/secrets/v1/" + principal_id)
//!        ▼
//! per-principal wrapping key       handed to that principal's child only, as
//!        │                         host-only env (`IKENGA_PRINCIPAL_SECRETS_KEY`)
//!        │ AES-256-GCM, AAD bound to the principal id
//!        ▼
//! <data>/secrets/envelope.json     uid 0600   the wrapped DEK
//!        │ AES-256-GCM, AAD bound to principal id + secret name
//!        ▼
//! <data>/secrets/values.json       uid 0600   name → nonce‖ciphertext
//! ```
//!
//! The DEK is per store and never changes, so rotating the KEK only rewraps
//! one envelope ([`PrincipalStore::rewrap`]) and never touches a value. The
//! child holds its own wrapping key from launch, so background work (chi
//! runs, cron, MCP) decrypts while the user is signed out; the master KEK is
//! never in a child's memory or environment, so a principal (or anything
//! running as their uid) can at most open their **own** store — the same
//! secrets the `secrets_get` RPC already returns to them.
//!
//! # What never happens
//!
//! - A wrapping key that doesn't open the envelope (another principal's, a
//!   rotated KEK, a copied store) is an error. The envelope is **never**
//!   replaced on that path: minting a fresh DEK would orphan every value.
//! - An envelope missing beside values that exist is an error for the same
//!   reason, not a fresh store.
//! - The wrapping key never reaches a PTY: it is in
//!   [`crate::pty::is_host_only_env`]'s floor, the T1 executor drops it from
//!   any spec, and the child scrubs it from its own environment block (so
//!   `/proc/<pid>/environ` doesn't show it either) and unsets it at startup
//!   ([`WrapKey::take_from_env`]): before `main`, on Linux, after turning
//!   itself non-dumpable ([`capture_handoff_at_exec`]).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::crypto::{self, DEK_LEN};
use super::hkdf::hkdf_sha256;
use super::index::{validate_legacy_name, write_atomic};
use super::store::{SecretMeta, SecretsStore, StoreError, StoreOwner};

/// The host-only variable the broker hands a principal's child its wrapping
/// key in. Listed in `pty::is_host_only_env`.
pub const WRAP_KEY_ENV: &str = "IKENGA_PRINCIPAL_SECRETS_KEY";
/// `<data>/secrets/` (G-PRINCIPAL §4, reserved for WP-21), mode 0700.
pub const SECRETS_DIR: &str = "secrets";
pub const ENVELOPE_FILENAME: &str = "envelope.json";
pub const VALUES_FILENAME: &str = "values.json";
/// `SecretsStore::backend_label`.
pub const BACKEND_LABEL: &str = "per-principal store";
pub const KEY_LEN: usize = 32;

const WRAP_KEY_VERSION: &str = "v1";
const HKDF_SALT: &[u8] = b"ikenga/secrets-kek/v1";
const HKDF_INFO_PREFIX: &str = "ikenga/secrets/v1/";
const STORE_VERSION: u32 = 1;
const ALG: &str = "A256GCM-HKDF-SHA256";
const DEK_AAD_PREFIX: &str = "ikenga:secrets:principal-dek:v1:";
const VALUE_AAD_PREFIX: &str = "ikenga:secrets:principal-value:v1:";
const MAX_ENVELOPE_BYTES: u64 = 4 * 1024;
/// The largest `values.json` [`PrincipalStore`] reads, and so the largest it
/// ever writes: `save` refuses a body over it, because a file `load` refuses
/// would lock the principal out of every get / list / delete.
pub const MAX_VALUES_BYTES: u64 = 8 * 1024 * 1024;
/// The largest single secret value a [`PrincipalStore`] accepts (64 KiB —
/// API keys, tokens, PEM bundles; not files).
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_PRINCIPAL_LEN: usize = 64;

// ─── the wrapping key ──────────────────────────────────────────────────────

/// One principal's wrapping key: what the broker derives from the master KEK
/// and the only key material a principal's child ever holds.
pub struct WrapKey {
    principal: String,
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for WrapKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrapKey")
            .field("principal", &self.principal)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// A principal id as the store accepts it: the `PrincipalId` UUID text, or
/// anything else that is short, non-empty ASCII alphanumerics and `-`.
pub fn validate_principal(principal: &str) -> Result<(), String> {
    if principal.is_empty()
        || principal.len() > MAX_PRINCIPAL_LEN
        || !principal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(format!("invalid principal id {principal:?}"));
    }
    Ok(())
}

impl WrapKey {
    /// HKDF-SHA256 of the master KEK for one principal. Only the broker,
    /// which holds the KEK, calls this.
    pub fn derive(kek: &[u8; KEY_LEN], principal: &str) -> Result<Self, String> {
        validate_principal(principal)?;
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        let info = format!("{HKDF_INFO_PREFIX}{principal}");
        hkdf_sha256(HKDF_SALT, kek, info.as_bytes(), &mut key[..]);
        Ok(Self {
            principal: principal.to_string(),
            key,
        })
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// `v1:<principal_id>:<hex key>` — the value of [`WRAP_KEY_ENV`].
    pub fn to_env_value(&self) -> Zeroizing<String> {
        let hex = Zeroizing::new(hex::encode(&self.key[..]));
        Zeroizing::new(format!(
            "{WRAP_KEY_VERSION}:{}:{}",
            self.principal,
            hex.as_str()
        ))
    }

    pub fn from_env_value(value: &str) -> Result<Self, String> {
        let mut parts = value.splitn(3, ':');
        let (Some(version), Some(principal), Some(hex)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return Err(format!("{WRAP_KEY_ENV} is malformed"));
        };
        if version != WRAP_KEY_VERSION {
            return Err(format!(
                "{WRAP_KEY_ENV} has unsupported version {version:?}"
            ));
        }
        validate_principal(principal)?;
        let bytes =
            Zeroizing::new(hex::decode(hex).map_err(|_| format!("{WRAP_KEY_ENV} key is not hex"))?);
        if bytes.len() != KEY_LEN {
            return Err(format!("{WRAP_KEY_ENV} key is not {KEY_LEN} bytes"));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&bytes);
        Ok(Self {
            principal: principal.to_string(),
            key,
        })
    }

    /// The broker's hand-off, once: the key [`WRAP_KEY_ENV`] carried into
    /// this process, `None` when it carried none (or it was already taken).
    ///
    /// On Linux the variable was read, scrubbed and unset **before `main`**
    /// ([`capture_handoff_at_exec`], an `.init_array` entry), so by the time
    /// the daemon builds its `AppState` it is no longer in the environment
    /// and this only hands over what was captured. That ordering is the
    /// point: `remove_var` (glibc `unsetenv`) rearranges `environ` under
    /// Rust's env lock only, and by `AppState` time the multi-thread Tokio
    /// runtime and the daemon's tasks are running, any of which may be inside
    /// a libc `getenv` (DNS, time zones, locale) that takes no such lock.
    ///
    /// Without that capture (a non-Linux build, where no T1 child exists)
    /// the key is read and scrubbed in place now, but never `remove_var`'d
    /// late, for the same reason.
    pub fn take_from_env() -> Option<Result<Self, String>> {
        let mut slot = handoff_slot();
        match std::mem::replace(&mut *slot, Handoff::Taken) {
            Handoff::Captured(key) => key,
            Handoff::Taken => None,
            Handoff::NotCaptured => {
                if std::env::var_os(WRAP_KEY_ENV).is_some() {
                    tracing::warn!(
                        "{WRAP_KEY_ENV} was not captured at exec; taking it late (scrubbed in \
                         place, left set but empty)"
                    );
                }
                read_handoff(Unset::No)
            }
        }
    }

    fn dek_aad(&self) -> Vec<u8> {
        format!("{DEK_AAD_PREFIX}{}", self.principal).into_bytes()
    }

    fn wrap(&self, dek: &[u8; DEK_LEN]) -> Result<String, StoreError> {
        let wrapped = crypto::encrypt_value(&self.key, &dek[..], &self.dek_aad())
            .map_err(|e| StoreError::unknown(e.to_string()))?;
        Ok(STANDARD.encode(wrapped))
    }

    fn unwrap(&self, envelope: &Envelope) -> Result<Zeroizing<[u8; DEK_LEN]>, StoreError> {
        if envelope.principal != self.principal {
            return Err(StoreError::invalid(format!(
                "this secret store belongs to principal {}, not {}",
                envelope.principal, self.principal
            )));
        }
        let refuse = || {
            StoreError::invalid(
                "the wrapping key does not open this secret store (another principal's key, or \
                 the operator KEK changed without a rewrap); the store was left untouched",
            )
        };
        let wrapped = STANDARD
            .decode(envelope.wrapped_dek.as_bytes())
            .map_err(|_| refuse())?;
        let plain =
            crypto::decrypt_value(&self.key, &wrapped, &self.dek_aad()).map_err(|_| refuse())?;
        if plain.len() != DEK_LEN {
            return Err(refuse());
        }
        let mut dek = Zeroizing::new([0u8; DEK_LEN]);
        dek.copy_from_slice(&plain);
        Ok(dek)
    }
}

// ─── the hand-off, taken at exec ───────────────────────────────────────────

/// Where the process-start capture leaves the broker's hand-off for
/// [`WrapKey::take_from_env`].
enum Handoff {
    /// No capture ran (non-Linux).
    NotCaptured,
    /// What the capture found: `None` when the variable was not set.
    Captured(Option<Result<WrapKey, String>>),
    /// Handed over already.
    Taken,
}

static HANDOFF: Mutex<Handoff> = Mutex::new(Handoff::NotCaptured);

fn handoff_slot() -> std::sync::MutexGuard<'static, Handoff> {
    HANDOFF.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unset {
    Yes,
    No,
}

/// Read the hand-off out of the environment: make the process non-dumpable
/// first, then copy the value, overwrite it in the environment block, and
/// (with [`Unset::Yes`], only ever single-threaded) unset it.
fn read_handoff(unset: Unset) -> Option<Result<WrapKey, String>> {
    if !env_has(WRAP_KEY_ENV) {
        return None;
    }
    harden_before_reading_key();
    let raw = std::env::var_os(WRAP_KEY_ENV)?;
    scrub_env_value(WRAP_KEY_ENV);
    if unset == Unset::Yes {
        std::env::remove_var(WRAP_KEY_ENV);
    }
    let raw = Zeroizing::new(match raw.into_string() {
        Ok(s) => s,
        Err(_) => return Some(Err(format!("{WRAP_KEY_ENV} is not UTF-8"))),
    });
    Some(WrapKey::from_env_value(raw.as_str()))
}

/// Capture the broker's hand-off into [`HANDOFF`]. Idempotent. Runs from
/// [`CAPTURE_HANDOFF_AT_EXEC`] before `main`: one thread, no runtime, nothing
/// spawned, so the `remove_var` here races nothing. A process the variable
/// was not set for (the desktop app, the T1 broker, every test binary
/// without it) only records that.
pub fn capture_handoff_at_exec() {
    let mut slot = handoff_slot();
    if matches!(*slot, Handoff::NotCaptured) {
        *slot = Handoff::Captured(read_handoff(Unset::Yes));
    }
}

/// The `.init_array` entry: the dynamic loader runs it before `main`, so the
/// hand-off is out of the environment before `#[tokio::main]` builds its
/// multi-thread runtime — without `server/src/main.rs` having to call
/// anything. Lives in this module beside [`HANDOFF`], which
/// [`WrapKey::take_from_env`] reads, so the linker can't keep one without
/// the other. It must never panic (that would abort before `main`): nothing
/// it calls unwraps.
#[cfg(target_os = "linux")]
#[used]
#[link_section = ".init_array"]
static CAPTURE_HANDOFF_AT_EXEC: extern "C" fn() = {
    extern "C" fn capture() {
        capture_handoff_at_exec();
    }
    capture
};

/// Whether `name` is set, without copying its value out.
#[cfg(unix)]
fn env_has(name: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: a read-only lookup; the returned pointer is only null-checked.
    unsafe { !libc::getenv(name.as_ptr()).is_null() }
}

#[cfg(not(unix))]
fn env_has(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// A process handed a wrapping key turns itself non-dumpable before reading
/// it: `/proc/<pid>/environ`, `mem` and friends become root-owned and
/// `ptrace` / `process_vm_readv` need `CAP_SYS_PTRACE`, so the principal's
/// other processes (its own shells included) can read neither the hand-off
/// still in the environment block nor the key or DEK in this process's
/// memory. `execve` resets the flag, so what the child spawns is unaffected.
#[cfg(target_os = "linux")]
fn harden_before_reading_key() {
    // SAFETY: PR_SET_DUMPABLE takes one integer argument; nothing is read
    // or written through the others. It cannot fail for 0, and nothing could
    // be logged before `main` anyway; the key is scrubbed regardless.
    unsafe {
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
}

#[cfg(not(target_os = "linux"))]
fn harden_before_reading_key() {}

/// Overwrite the variable's value in place in the environment block (before
/// it is unset, if it is): `remove_var` only drops the pointer from
/// `environ`, and the initial block is what `/proc/<pid>/environ` reads.
/// Only the value bytes change, never the name, so a concurrent `getenv` of
/// any other variable (which compares names only) is unaffected.
#[cfg(unix)]
fn scrub_env_value(name: &str) {
    let Ok(name) = std::ffi::CString::new(name) else {
        return;
    };
    // SAFETY: `getenv` returns null or a pointer to the NUL-terminated value
    // inside the environment block; exactly `strlen` bytes are overwritten,
    // never the terminator. Nothing else in this process reads this variable
    // (it is read once, by `read_handoff`).
    unsafe {
        let value = libc::getenv(name.as_ptr());
        if !value.is_null() {
            let len = libc::strlen(value);
            std::ptr::write_bytes(value, 0, len);
        }
    }
}

#[cfg(not(unix))]
fn scrub_env_value(_name: &str) {}

// ─── on-disk shapes ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    version: u32,
    alg: String,
    principal: String,
    /// base64(nonce ‖ AES-256-GCM(wrapping key, DEK)).
    wrapped_dek: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Values {
    version: u32,
    principal: String,
    /// name → base64(nonce ‖ AES-256-GCM(DEK, value)).
    entries: BTreeMap<String, String>,
}

fn read_bounded(path: &Path, max: u64, what: &str) -> Result<Option<Vec<u8>>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_file() {
                return Err(StoreError::unavailable(format!(
                    "{what} is not a regular file: {}",
                    path.display()
                )));
            }
            if meta.len() > max {
                return Err(StoreError::unavailable(format!(
                    "{what} is larger than {max} bytes: {}",
                    path.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StoreError::unavailable(format!(
                "inspect {}: {e}",
                path.display()
            )))
        }
    }
    fs::read(path)
        .map(Some)
        .map_err(|e| StoreError::unavailable(format!("read {}: {e}", path.display())))
}

/// `<data>/secrets/`, created 0700 (and forced back to 0700). Never a
/// symlink. `data_dir` itself must exist.
fn ensure_private_dir(dir: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(StoreError::unavailable(format!(
                    "{} is not a directory",
                    dir.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(dir)
                .map_err(|e| StoreError::unavailable(format!("create {}: {e}", dir.display())))?;
        }
        Err(e) => {
            return Err(StoreError::unavailable(format!(
                "inspect {}: {e}",
                dir.display()
            )))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| StoreError::unavailable(format!("chmod {}: {e}", dir.display())))?;
    }
    Ok(())
}

fn read_envelope(path: &Path) -> Result<Option<Envelope>, StoreError> {
    let Some(body) = read_bounded(path, MAX_ENVELOPE_BYTES, "secret envelope")? else {
        return Ok(None);
    };
    let envelope: Envelope = serde_json::from_slice(&body)
        .map_err(|e| StoreError::invalid(format!("{}: {e}", path.display())))?;
    if envelope.version != STORE_VERSION || envelope.alg != ALG {
        return Err(StoreError::invalid(format!(
            "{}: unsupported envelope (version {}, alg {:?})",
            path.display(),
            envelope.version,
            envelope.alg
        )));
    }
    Ok(Some(envelope))
}

fn write_envelope(path: &Path, key: &WrapKey, dek: &[u8; DEK_LEN]) -> Result<(), StoreError> {
    let envelope = Envelope {
        version: STORE_VERSION,
        alg: ALG.to_string(),
        principal: key.principal.clone(),
        wrapped_dek: key.wrap(dek)?,
    };
    let mut body = serde_json::to_vec_pretty(&envelope)
        .map_err(|e| StoreError::uncommitted(format!("serialize envelope: {e}")))?;
    body.push(b'\n');
    write_atomic(path, &body).map_err(StoreError::uncommitted)
}

/// A principal store's file left in `<data_dir>/secrets/` (the envelope or
/// the values, whatever its file type), if there is one. A daemon that is not
/// the T1 principal child for it must not answer secrets from the operator
/// default alone over it (`crate::secrets_env`, fail closed).
pub fn leftover_store_file(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join(SECRETS_DIR);
    [ENVELOPE_FILENAME, VALUES_FILENAME]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| fs::symlink_metadata(path).is_ok())
}

// ─── the store ─────────────────────────────────────────────────────────────

/// One principal's secrets, under `<data>/secrets/`. One child per principal
/// opens it (the child's data-dir flock, I-3); the mutex serializes that
/// child's own concurrent requests.
pub struct PrincipalStore {
    dir: PathBuf,
    principal: String,
    dek: Zeroizing<[u8; DEK_LEN]>,
    io: Mutex<()>,
}

impl std::fmt::Debug for PrincipalStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalStore")
            .field("dir", &self.dir)
            .field("principal", &self.principal)
            .finish_non_exhaustive()
    }
}

impl PrincipalStore {
    /// Open (creating on first use) the store under `<data_dir>/secrets/`.
    ///
    /// A new store gets a fresh DEK wrapped under `key`. An existing one must
    /// open with `key`; if it doesn't, nothing is written.
    pub fn open(data_dir: &Path, key: &WrapKey) -> Result<Self, StoreError> {
        let dir = data_dir.join(SECRETS_DIR);
        ensure_private_dir(&dir)?;
        let envelope_path = dir.join(ENVELOPE_FILENAME);
        let dek = match read_envelope(&envelope_path)? {
            Some(envelope) => key.unwrap(&envelope)?,
            None => {
                let values = dir.join(VALUES_FILENAME);
                if fs::symlink_metadata(&values).is_ok() {
                    return Err(StoreError::unavailable(format!(
                        "{} exists but {} does not; refusing to mint a new key over existing \
                         ciphertext (restore the envelope, or move the values file aside)",
                        values.display(),
                        envelope_path.display()
                    )));
                }
                let dek = crypto::random_dek();
                write_envelope(&envelope_path, key, &dek)?;
                dek
            }
        };
        Ok(Self {
            dir,
            principal: key.principal.clone(),
            dek,
            io: Mutex::new(()),
        })
    }

    /// KEK rotation: rewrap this store's DEK from `current` to `next` in one
    /// atomic envelope replace. Values are not touched, so a crash at any
    /// point leaves either the old or the new envelope, each of which opens
    /// every value. A `current` that doesn't open the envelope writes
    /// nothing. Run it while the principal's child is stopped.
    ///
    /// **Not yet safe to call as root.** It opens, writes and renames by
    /// path inside `<data>/secrets/`, a directory the principal owns: root
    /// following those paths could be steered by a principal-planted
    /// symlink into writing elsewhere, or leave a root-owned envelope the
    /// principal's child cannot read. Until it is ported, call it only as
    /// the principal's uid (e.g. in a T1-executor child, as the store's own
    /// writes are) — or port it to fd-relative, never-follow I/O
    /// (`server::operator::safe_fs`, as `adopt_t0` does) with the result
    /// chowned to the uid. The deferred operator `rotate` command (WP-21
    /// follow-up) must do one of the two; today nothing calls this outside
    /// tests.
    pub fn rewrap(data_dir: &Path, current: &WrapKey, next: &WrapKey) -> Result<(), StoreError> {
        if current.principal != next.principal {
            return Err(StoreError::invalid(
                "a rewrap keeps the store's principal; the two keys name different principals",
            ));
        }
        let path = data_dir.join(SECRETS_DIR).join(ENVELOPE_FILENAME);
        let envelope = read_envelope(&path)?
            .ok_or_else(|| StoreError::unavailable(format!("{} does not exist", path.display())))?;
        let dek = current.unwrap(&envelope)?;
        write_envelope(&path, next, &dek)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    fn values_path(&self) -> PathBuf {
        self.dir.join(VALUES_FILENAME)
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn load(&self) -> Result<Values, StoreError> {
        let path = self.values_path();
        let Some(body) = read_bounded(&path, MAX_VALUES_BYTES, "secret values file")? else {
            return Ok(Values {
                version: STORE_VERSION,
                principal: self.principal.clone(),
                entries: BTreeMap::new(),
            });
        };
        let values: Values = serde_json::from_slice(&body)
            .map_err(|e| StoreError::invalid(format!("{}: {e}", path.display())))?;
        if values.version != STORE_VERSION {
            return Err(StoreError::invalid(format!(
                "{}: unsupported version {}",
                path.display(),
                values.version
            )));
        }
        if values.principal != self.principal {
            return Err(StoreError::invalid(format!(
                "{} belongs to principal {}, not {}",
                path.display(),
                values.principal,
                self.principal
            )));
        }
        Ok(values)
    }

    /// Never writes a body [`Self::load`] would refuse: past
    /// [`MAX_VALUES_BYTES`] the write is refused and the file on disk is the
    /// last good one, so the store stays readable (and deletable) instead of
    /// locking its owner out.
    fn save(&self, values: &Values) -> Result<(), StoreError> {
        let mut body = serde_json::to_vec_pretty(values)
            .map_err(|e| StoreError::uncommitted(format!("serialize values: {e}")))?;
        body.push(b'\n');
        if body.len() as u64 > MAX_VALUES_BYTES {
            return Err(StoreError::invalid(format!(
                "secret store is full: this write would make it {} bytes, over the {} byte \
                 limit; delete secrets you no longer need (nothing was written)",
                body.len(),
                MAX_VALUES_BYTES
            )));
        }
        write_atomic(&self.values_path(), &body).map_err(StoreError::uncommitted)
    }

    fn value_aad(&self, name: &str) -> Vec<u8> {
        format!("{VALUE_AAD_PREFIX}{}\0{name}", self.principal).into_bytes()
    }

    fn seal(&self, name: &str, value: &str) -> Result<String, StoreError> {
        let sealed = crypto::encrypt_value(&self.dek, value.as_bytes(), &self.value_aad(name))
            .map_err(|e| StoreError::uncommitted(e.to_string()))?;
        Ok(STANDARD.encode(sealed))
    }

    fn open_value(&self, name: &str, sealed: &str) -> Result<String, StoreError> {
        let undecryptable =
            || StoreError::invalid(format!("secret {name:?} could not be decrypted"));
        let bytes = STANDARD
            .decode(sealed.as_bytes())
            .map_err(|_| undecryptable())?;
        let plain = crypto::decrypt_value(&self.dek, &bytes, &self.value_aad(name))
            .map_err(|_| undecryptable())?;
        String::from_utf8(plain.to_vec()).map_err(|_| undecryptable())
    }
}

fn check_name(name: &str) -> Result<(), StoreError> {
    validate_legacy_name(name).map_err(StoreError::invalid)
}

fn check_value(name: &str, value: &str) -> Result<(), StoreError> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(StoreError::invalid(format!(
            "secret {name:?} is {} bytes; the limit is {MAX_VALUE_BYTES} (nothing was written)",
            value.len()
        )));
    }
    Ok(())
}

impl SecretsStore for PrincipalStore {
    fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
        check_name(name)?;
        let _guard = self.guard();
        let values = self.load()?;
        values
            .entries
            .get(name)
            .map(|sealed| self.open_value(name, sealed))
            .transpose()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
        check_name(name)?;
        check_value(name, value)?;
        let _guard = self.guard();
        let mut values = self.load()?;
        values
            .entries
            .insert(name.to_string(), self.seal(name, value)?);
        self.save(&values)
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        check_name(name)?;
        let _guard = self.guard();
        let mut values = self.load()?;
        if values.entries.remove(name).is_some() {
            self.save(&values)?;
        }
        Ok(())
    }

    fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
        let _guard = self.guard();
        Ok(self
            .load()?
            .entries
            .into_keys()
            .map(|name| SecretMeta { name })
            .collect())
    }

    fn replace_all(&self, values: &BTreeMap<String, String>) -> Result<usize, StoreError> {
        for (name, value) in values {
            check_name(name)?;
            check_value(name, value)?;
        }
        let _guard = self.guard();
        let mut next = self.load()?;
        next.entries.clear();
        for (name, value) in values {
            next.entries.insert(name.clone(), self.seal(name, value)?);
        }
        self.save(&next)?;
        Ok(values.len())
    }

    fn probe(&self) -> Result<(), StoreError> {
        let _guard = self.guard();
        self.load().map(drop)
    }

    /// Every value is sealed under the DEK from the first write.
    fn prepare_encryption(&self) -> Result<(), StoreError> {
        Ok(())
    }

    /// No passphrase layer exists here (DEC-R18-1: the key is server-held).
    fn detect_configuration(&self) -> Result<bool, StoreError> {
        Ok(false)
    }

    fn backend_label(&self) -> &'static str {
        BACKEND_LABEL
    }

    fn owner(&self) -> StoreOwner {
        StoreOwner::Principal(self.principal.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEK: [u8; KEY_LEN] = [7u8; KEY_LEN];
    const ADA: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
    const BOB: &str = "01890a5d-ac96-774b-bcce-b302099a8058";

    fn key(kek: &[u8; KEY_LEN], principal: &str) -> WrapKey {
        WrapKey::derive(kek, principal).unwrap()
    }

    fn data_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn derivation_is_deterministic_and_per_principal() {
        let a1 = key(&KEK, ADA);
        let a2 = key(&KEK, ADA);
        let b = key(&KEK, BOB);
        assert_eq!(*a1.key, *a2.key);
        assert_ne!(*a1.key, *b.key, "each principal gets its own key");
        assert_ne!(&a1.key[..], &KEK[..], "the KEK itself is never handed out");
        let other_kek = key(&[8u8; KEY_LEN], ADA);
        assert_ne!(*a1.key, *other_kek.key);
        assert!(WrapKey::derive(&KEK, "").is_err());
        assert!(WrapKey::derive(&KEK, "../etc").is_err());
    }

    #[test]
    fn env_value_round_trips_and_rejects_garbage() {
        let a = key(&KEK, ADA);
        let env = a.to_env_value();
        assert!(env.starts_with(&format!("v1:{ADA}:")));
        assert!(!env.contains(&hex::encode(KEK)), "never the KEK");
        let back = WrapKey::from_env_value(&env).unwrap();
        assert_eq!(back.principal(), ADA);
        assert_eq!(*back.key, *a.key);
        for bad in [
            "",
            "v1",
            "v1:abc",
            &format!("v2:{ADA}:{}", "00".repeat(32)),
            &format!("v1:{ADA}:zz"),
            &format!("v1:{ADA}:{}", "00".repeat(31)),
            &format!("v1:a b:{}", "00".repeat(32)),
        ] {
            assert!(WrapKey::from_env_value(bad).is_err(), "{bad:?}");
        }
        assert!(!format!("{a:?}").contains(&hex::encode(&a.key[..])));
    }

    #[test]
    fn envelope_wraps_and_unwraps_across_reopens() {
        let tmp = data_dir();
        let a = key(&KEK, ADA);
        let store = PrincipalStore::open(tmp.path(), &a).unwrap();
        store.set("OPENAI_API_KEY", "sk-ada").unwrap();
        store.set("workspace::TOKEN", "t").unwrap();
        drop(store);

        let reopened = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        assert_eq!(
            reopened.get("OPENAI_API_KEY").unwrap().as_deref(),
            Some("sk-ada")
        );
        assert_eq!(
            reopened
                .list_meta()
                .unwrap()
                .into_iter()
                .map(|m| m.name)
                .collect::<Vec<_>>(),
            vec!["OPENAI_API_KEY".to_string(), "workspace::TOKEN".to_string()]
        );
        reopened.delete("OPENAI_API_KEY").unwrap();
        reopened.delete("OPENAI_API_KEY").unwrap();
        assert_eq!(reopened.get("OPENAI_API_KEY").unwrap(), None);

        // Nothing plaintext at rest.
        let on_disk = fs::read_to_string(tmp.path().join("secrets/values.json")).unwrap();
        assert!(!on_disk.contains("sk-ada") && !on_disk.contains("\"t\""));
        let env = fs::read_to_string(tmp.path().join("secrets/envelope.json")).unwrap();
        assert!(env.contains(ADA));
    }

    #[cfg(unix)]
    #[test]
    fn store_dir_is_0700_and_files_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("K", "v").unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&tmp.path().join("secrets")), 0o700);
        assert_eq!(mode(&tmp.path().join("secrets/envelope.json")), 0o600);
        assert_eq!(mode(&tmp.path().join("secrets/values.json")), 0o600);
    }

    #[test]
    fn a_wrong_principals_key_never_opens_or_overwrites_the_store() {
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("K", "ada-secret").unwrap();
        drop(store);
        let envelope = tmp.path().join("secrets/envelope.json");
        let before = fs::read(&envelope).unwrap();

        // Bob's key (same KEK, other principal): the envelope names Ada.
        let err = PrincipalStore::open(tmp.path(), &key(&KEK, BOB)).unwrap_err();
        assert!(err.is_invalid(), "{err}");

        // Ada's id but a key from another KEK: AES-GCM refuses.
        let err = PrincipalStore::open(tmp.path(), &key(&[9u8; KEY_LEN], ADA)).unwrap_err();
        assert!(err.is_invalid(), "{err}");
        assert!(err.to_string().contains("left untouched"), "{err}");

        // A forged envelope that claims Bob but holds Ada's wrapped DEK: the
        // AAD binds the principal, so Bob's key still fails.
        let forged = String::from_utf8(before.clone()).unwrap().replace(ADA, BOB);
        fs::write(&envelope, forged).unwrap();
        assert!(PrincipalStore::open(tmp.path(), &key(&KEK, BOB))
            .unwrap_err()
            .is_invalid());
        fs::write(&envelope, &before).unwrap();

        assert_eq!(fs::read(&envelope).unwrap(), before, "never rewritten");
        let ada = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        assert_eq!(ada.get("K").unwrap().as_deref(), Some("ada-secret"));
    }

    #[test]
    fn values_are_bound_to_their_name() {
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("A", "alpha").unwrap();
        store.set("B", "beta").unwrap();
        // Swap the ciphertexts on disk: each must now fail to decrypt.
        let path = tmp.path().join("secrets/values.json");
        let mut values: Values = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let a = values.entries["A"].clone();
        let b = values.entries["B"].clone();
        values.entries.insert("A".into(), b);
        values.entries.insert("B".into(), a);
        fs::write(&path, serde_json::to_vec(&values).unwrap()).unwrap();
        assert!(store.get("A").unwrap_err().is_invalid());
        assert!(store.get("B").unwrap_err().is_invalid());
    }

    #[test]
    fn a_missing_envelope_beside_values_is_refused_not_reminted() {
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("K", "v").unwrap();
        drop(store);
        fs::remove_file(tmp.path().join("secrets/envelope.json")).unwrap();
        let err = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(!tmp.path().join("secrets/envelope.json").exists());
    }

    /// KEK rotation: rewrap is one atomic envelope replace; every value
    /// survives, the old key stops working, a wrong `current` writes nothing.
    #[test]
    fn rotation_rewraps_without_touching_values() {
        let tmp = data_dir();
        let old = key(&KEK, ADA);
        let new = key(&[42u8; KEY_LEN], ADA);
        let store = PrincipalStore::open(tmp.path(), &old).unwrap();
        store.set("K", "kept").unwrap();
        drop(store);
        let values_path = tmp.path().join("secrets/values.json");
        let values_before = fs::read(&values_path).unwrap();
        let envelope_path = tmp.path().join("secrets/envelope.json");
        let envelope_before = fs::read(&envelope_path).unwrap();

        // A wrong current key: refused, nothing written.
        assert!(PrincipalStore::rewrap(tmp.path(), &new, &old)
            .unwrap_err()
            .is_invalid());
        assert_eq!(fs::read(&envelope_path).unwrap(), envelope_before);
        // Keys for two principals: refused.
        assert!(PrincipalStore::rewrap(tmp.path(), &old, &key(&KEK, BOB)).is_err());

        PrincipalStore::rewrap(tmp.path(), &old, &new).unwrap();
        assert_eq!(
            fs::read(&values_path).unwrap(),
            values_before,
            "values untouched"
        );
        assert!(!tmp.path().join("secrets/envelope.json.tmp").exists());
        assert!(PrincipalStore::open(tmp.path(), &old)
            .unwrap_err()
            .is_invalid());
        let rotated = PrincipalStore::open(tmp.path(), &new).unwrap();
        assert_eq!(rotated.get("K").unwrap().as_deref(), Some("kept"));
    }

    #[test]
    fn replace_all_and_names() {
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("OLD", "x").unwrap();
        let next: BTreeMap<String, String> = [
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "2".to_string()),
        ]
        .into();
        assert_eq!(store.replace_all(&next).unwrap(), 2);
        assert_eq!(store.export_all().unwrap(), next);
        assert!(store.set("", "x").unwrap_err().is_invalid());
        assert!(store.set("__manifest", "x").unwrap_err().is_invalid());
        assert_eq!(store.owner(), StoreOwner::Principal(ADA.into()));
        assert_eq!(store.backend_label(), BACKEND_LABEL);
        store.probe().unwrap();
    }

    #[test]
    fn a_values_file_of_another_principal_is_refused() {
        let ada_dir = data_dir();
        let bob_dir = data_dir();
        let ada = PrincipalStore::open(ada_dir.path(), &key(&KEK, ADA)).unwrap();
        ada.set("K", "ada").unwrap();
        let bob = PrincipalStore::open(bob_dir.path(), &key(&KEK, BOB)).unwrap();
        // Ada's values copied into Bob's store.
        fs::copy(
            ada_dir.path().join("secrets/values.json"),
            bob_dir.path().join("secrets/values.json"),
        )
        .unwrap();
        assert!(bob.get("K").unwrap_err().is_invalid());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_secrets_dir_is_refused() {
        let tmp = data_dir();
        let elsewhere = data_dir();
        std::os::unix::fs::symlink(elsewhere.path(), tmp.path().join("secrets")).unwrap();
        assert!(PrincipalStore::open(tmp.path(), &key(&KEK, ADA))
            .unwrap_err()
            .is_unavailable());
    }

    /// L21-1: a store must never write itself into a file its own `load`
    /// refuses. Oversized values are refused outright; a write that would
    /// push `values.json` past the read cap is refused with nothing written;
    /// every read, list and delete keeps working.
    #[test]
    fn oversized_writes_are_refused_and_never_lock_the_store_out() {
        const VALUE_CAP: usize = MAX_VALUE_BYTES;
        const FILE_CAP: u64 = MAX_VALUES_BYTES;
        assert_eq!(VALUE_CAP, 64 * 1024);
        let tmp = data_dir();
        let store = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        store.set("SMALL", "kept").unwrap();

        let over = "x".repeat(VALUE_CAP + 1);
        assert!(store.set("BIG", &over).unwrap_err().is_invalid());
        let one: BTreeMap<String, String> = [("BIG".to_string(), over)].into();
        assert!(store.replace_all(&one).unwrap_err().is_invalid());
        assert_eq!(store.get("SMALL").unwrap().as_deref(), Some("kept"));

        let at_cap = "y".repeat(VALUE_CAP);
        let path = tmp.path().join("secrets/values.json");
        let mut stored = 1;
        let refusal = loop {
            match store.set(&format!("K{stored}"), &at_cap) {
                Ok(()) => stored += 1,
                Err(e) => break e,
            }
            assert!(stored < 1_000, "never refused");
        };
        assert!(refusal.is_invalid(), "{refusal}");
        assert!(fs::metadata(&path).unwrap().len() <= FILE_CAP);
        // Still fully usable: get, list, delete, and a write after a delete.
        assert_eq!(store.get("SMALL").unwrap().as_deref(), Some("kept"));
        assert_eq!(store.list_meta().unwrap().len(), stored);
        store.probe().unwrap();
        store.delete("K1").unwrap();
        store.set("AFTER", "ok").unwrap();
        drop(store);
        let reopened = PrincipalStore::open(tmp.path(), &key(&KEK, ADA)).unwrap();
        assert_eq!(reopened.get("AFTER").unwrap().as_deref(), Some("ok"));
    }
}
