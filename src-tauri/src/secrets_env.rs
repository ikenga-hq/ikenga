//! The headless daemon's secrets: the per-principal store (T1) layered over
//! the `IKENGA_SECRET_*` operator default.
//!
//! Lives outside `commands/` for the same reason `path_allow` does: the
//! daemon needs it and `commands/` is desktop-only, built around
//! `#[tauri::command]` and `AppHandle` (whose default type parameter IS
//! `Wry`, which a `--no-default-features` build does not link).
//!
//! # Two layers (remote-access WP-21; ADR-023 §4; G-PRINCIPAL §5 rows 15–16)
//!
//! 1. **The principal's own store** — only in a T1 principal child. Its
//!    files live under the child's `<data>/secrets/` (0700, the principal's
//!    uid) and are sealed under a DEK wrapped by a key the broker derives from
//!    its root-held KEK for this principal alone (founder decision
//!    DEC-R18-1; `secrets::principal_store`). It is readable **and writable**
//!    by its owner: `secrets_set` / `secrets_delete` and the scoped writes
//!    land here, and the project / pkg scopes are servable.
//! 2. **The operator default** — environment variables named
//!    `IKENGA_SECRET_<KEY>`, read-only, set by the operator on the host. The
//!    broker passes them to every child (§5 row 15). A principal's own value
//!    for a key overrides the default; deleting it reveals the default again.
//!
//! A T0 daemon has only layer 2, and behaves exactly as it did before WP-21:
//! a flat, read-only namespace, writes refused with [`WRITE_REFUSAL`],
//! project / pkg scopes refused with [`SCOPE_REFUSAL`] — unless its data dir
//! holds a principal store's files, when it fails closed instead (every
//! secret read errors; DEC-R18-1), exactly as a T1 child does whose store
//! will not open or whose broker handed it no key. Taking the key as a
//! bare env var name would have made the default layer a remote `printenv`
//! for every credential the process inherited, hence the prefix.
//!
//! The desktop app is untouched: its keychain backend stays desktop-only
//! (ADR-022) and it never reads this module's layers.
//!
//! # These are RPC-only and never reach a shell — do not "fix" that
//!
//! [`crate::pty`]'s `is_host_only_env` denylists `IKENGA_SECRET_*` (alongside
//! `IKENGA_AUTH_TOKEN`, `IKENGA_VAULT_KEY` and the principal store's
//! `IKENGA_PRINCIPAL_SECRETS_KEY`) from every PTY child. That is deliberate
//! and load-bearing: these values are fetched deliberately through an
//! authenticated RPC, they are not shell environment. A future reader who
//! sees a secret "missing" inside a remote terminal should reach for this
//! module's RPC, not relax that filter.
//!
//! # No passphrase layer
//!
//! Under DEC-R18-1 the key is server-held, so background work decrypts while
//! the user is signed out. There is nothing to set, unlock or lock. In a T1
//! principal child `secrets_lock_state` reports `{configured: true,
//! locked: false}` (the same `configured` `secrets_vault_status` reports),
//! `secrets_lock` is a no-op answering that state, and
//! `secrets_set_passphrase` / `secrets_unlock` are refused with
//! [`NO_PASSPHRASE`].
//!
//! **A T0 daemon does not serve that family at all** ([`DaemonSecrets::lock_state`]
//! is `None`): the four commands answer the unknown-command error they
//! always have. That is load-bearing for the frontend, not an oversight.
//! Settings → Secrets (`src/routes/settings/secrets.tsx`) treats the vault
//! as writable when `available && configured && !locked`; T0 reports
//! `available: true`, so an unlocked lock state there would show add / edit
//! controls whose every write is refused with [`WRITE_REFUSAL`]. With the
//! query erroring the page falls back to `configured: false, locked: true`
//! and renders read-only, exactly as it did before WP-21.

#[cfg(test)]
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::secrets::index::validate_key;
use crate::secrets::scope::{
    checked_vault_key, existing_scoped_name, parse_scoped, validate_existing_bare_name,
    validate_existing_scope, Scope,
};
use crate::secrets::{LockState, PrincipalStore, SecretsStore, WrapKey};
use serde::Serialize;

/// Prefix the operator uses to opt a credential into the remote namespace.
pub const ENV_PREFIX: &str = "IKENGA_SECRET_";

/// `VaultStatus::mode` for the daemon's env-backed store.
pub const MODE_ENV: &str = "env";

/// `VaultStatus::mode` for the desktop app's OS keychain.
pub const MODE_KEYCHAIN: &str = "keychain";

/// Human-readable backend label. Surfaced verbatim by Settings → API Keys
/// ("Vault unlocked via {keychainBackend}"), so it has to read as a sentence
/// fragment, not an enum tag.
pub const BACKEND_LABEL: &str = "host environment (IKENGA_SECRET_*)";

/// Operator runbook returned by every write command in the headless daemon.
///
/// Deliberately not the generic unknown-command fallthrough: the difference
/// between the two is whether the next person to read the error concludes
/// this is unfinished or decided.
pub const WRITE_REFUSAL: &str = concat!(
    "not available in the headless daemon: it has no vault, by design. ",
    "The daemon reads secrets only from IKENGA_SECRET_<KEY> environment variables, ",
    "which the operator sets on the host. ",
    "To add or change one: set IKENGA_SECRET_<KEY>=<value> in the daemon's environment ",
    "(systemd unit EnvironmentFile, container env, or the shell that launches ikenga-server), ",
    "then restart ikenga-server. ",
    "Remote writes are refused on purpose — a writable store reachable over the bearer-token ",
    "boundary is a remote credential store, which is exactly what the no-vault decision avoids. ",
    "Use the desktop app for vault-backed secret management."
);

/// Explanation returned for scoped *reads* the flat namespace cannot honour.
pub const SCOPE_REFUSAL: &str = concat!(
    "the headless daemon's secret store is flat: IKENGA_SECRET_<KEY> environment variables, ",
    "with no project or pkg partitioning. Only {\"kind\":\"workspace\"} is servable here; ",
    "project- and pkg-scoped secrets exist only in the desktop app's OS keychain."
);

/// A key is servable iff it can name an environment variable: non-empty,
/// ASCII alphanumerics and underscores only.
///
/// Note this rejects the dotted convention the desktop vault uses for
/// pkg-scoped keys (`studio.fal`) — such a key cannot be expressed as an
/// environment variable at all, so it is simply not reachable from the daemon.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Read one secret. `Ok(None)` means "not set", which is a normal answer and
/// matches the desktop `secrets_get` contract (bare `string | null`).
pub fn get(key: &str) -> Result<Option<String>, String> {
    if !is_valid_key(key) {
        return Err(format!(
            "invalid key {key:?}: expected ASCII alphanumerics and underscores only"
        ));
    }
    Ok(std::env::var(format!("{ENV_PREFIX}{key}")).ok())
}

/// Names only, prefix stripped, sorted — never values. Matches the desktop
/// `secrets_list_keys` contract (a bare array of unprefixed key names).
pub fn list_keys() -> Vec<String> {
    list_keys_from(std::env::vars().map(|(k, _)| k))
}

/// Testable core of [`list_keys`]: takes the variable *names* only, so a test
/// never has to mutate process-global environment.
fn list_keys_from<I: IntoIterator<Item = String>>(names: I) -> Vec<String> {
    let mut out: Vec<String> = names
        .into_iter()
        .filter_map(|name| name.strip_prefix(ENV_PREFIX).map(str::to_string))
        .filter(|k| is_valid_key(k))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Wire shape of `secrets_vault_status`, shared by both builds.
///
/// `available`, `keychain_backend` and `error` are the fields the frontend
/// has always destructured (`src/lib/tauri-cmd.ts::secretsVaultStatus`), and
/// every connector gates on `available`. `mode` and `writable` are additive:
/// the daemon reports `("env", false)` so the Settings UI can stop offering
/// buttons that cannot work, and the desktop app reports
/// `("keychain", true)`. Dropping either of the original three breaks every
/// connector silently, so this struct is a superset and never a rename.
#[derive(Debug, Serialize)]
pub struct VaultStatus {
    pub available: bool,
    pub keychain_backend: String,
    pub error: Option<String>,
    pub mode: String,
    pub writable: bool,
    pub locked: bool,
    pub configured: bool,
    pub idle_timeout_secs: u64,
    pub last_activity_unix_ms: Option<u64>,
}

/// The daemon's store is always readable — it is just process environment —
/// so `available` is true even when the operator has opted zero keys in. An
/// empty [`list_keys`] is "no secrets configured", not "vault broken", and
/// reporting `available: false` would red-flag a perfectly healthy daemon.
pub fn status() -> VaultStatus {
    VaultStatus {
        available: true,
        keychain_backend: BACKEND_LABEL.to_string(),
        error: None,
        mode: MODE_ENV.to_string(),
        writable: false,
        locked: false,
        configured: true,
        idle_timeout_secs: 0,
        last_activity_unix_ms: None,
    }
}

/// `VaultStatus::mode` for a T1 principal child's own store over the
/// operator default.
pub const MODE_PRINCIPAL: &str = "principal";

/// Backend label when the principal layer is present.
pub const PRINCIPAL_BACKEND_LABEL: &str =
    "per-principal store over the host environment (IKENGA_SECRET_*)";

/// Returned by `secrets_set_passphrase` / `secrets_unlock` in the daemon.
pub const NO_PASSPHRASE: &str = concat!(
    "not available in the headless daemon: its secret stores have no passphrase layer. ",
    "A T1 principal's store is sealed under a key the server holds for that principal ",
    "(so background work can read it while you are signed out), and the operator default ",
    "is IKENGA_SECRET_<KEY> host environment. There is nothing to set, unlock or lock."
);

/// Where the operator-default layer reads from: the process environment in
/// production, a fixed table in tests (which never mutate process env).
enum EnvSource {
    Process,
    #[cfg(test)]
    Fixed(BTreeMap<String, String>),
}

impl EnvSource {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self {
            Self::Process => get(key),
            #[cfg(test)]
            Self::Fixed(map) => {
                if !is_valid_key(key) {
                    return Err(format!("invalid key {key:?}"));
                }
                Ok(map.get(key).cloned())
            }
        }
    }

    fn keys(&self) -> Vec<String> {
        match self {
            Self::Process => list_keys(),
            #[cfg(test)]
            Self::Fixed(map) => list_keys_from(map.keys().map(|k| format!("{ENV_PREFIX}{k}"))),
        }
    }
}

/// The principal layer, if this daemon has one.
enum PrincipalLayer {
    /// T0, or a test router: the operator default alone.
    Absent,
    Open(Arc<dyn SecretsStore>),
    /// A T1 child whose store could not be opened. Fail closed: every secret
    /// read errors rather than falling through to the operator default,
    /// which would silently swap a principal's own credential for the
    /// operator's.
    Broken(String),
    /// A daemon that is not a T1 principal child (T0) over a data dir that
    /// holds a principal store (a downgraded or copied T1 data dir). Fails
    /// closed like [`Self::Broken`] — answering from the operator default
    /// would hide the principal's own values behind the operator's — but
    /// stays T0 on the wire: env-mode status, no lock family.
    Stranded(String),
}

/// The daemon's two-layer secrets (see the module docs). One per daemon
/// process, in `AppState`.
pub struct DaemonSecrets {
    principal: PrincipalLayer,
    env: EnvSource,
}

impl std::fmt::Debug for DaemonSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layer = match &self.principal {
            PrincipalLayer::Absent => "absent".to_string(),
            PrincipalLayer::Open(store) => format!("open ({})", store.backend_label()),
            PrincipalLayer::Broken(why) => format!("broken: {why}"),
            PrincipalLayer::Stranded(why) => format!("stranded: {why}"),
        };
        f.debug_struct("DaemonSecrets")
            .field("principal", &layer)
            .finish_non_exhaustive()
    }
}

fn store_err(error: crate::secrets::StoreError) -> String {
    error.to_string()
}

impl DaemonSecrets {
    /// The operator default alone (T0, and every test router).
    pub fn env_only() -> Self {
        Self {
            principal: PrincipalLayer::Absent,
            env: EnvSource::Process,
        }
    }

    /// Built once per daemon, while its `AppState` is: takes the broker's
    /// wrapping key out of the environment ([`WrapKey::take_from_env`] —
    /// always, whatever the tier, so it never outlives startup) and, in a T1
    /// principal child, opens `<data_dir>/secrets/` with it.
    pub fn for_daemon(data_dir: Option<&Path>, tier: crate::executor::ExecutorTier) -> Self {
        let key = WrapKey::take_from_env();
        if tier != crate::executor::ExecutorTier::T1 {
            if key.is_some() {
                tracing::warn!(
                    "{} is only honoured by a T1 principal child; ignored (and removed from \
                     this process's environment)",
                    crate::secrets::principal_store::WRAP_KEY_ENV
                );
            }
            if let Some(file) =
                data_dir.and_then(crate::secrets::principal_store::leftover_store_file)
            {
                let why = format!(
                    "{} belongs to a per-principal secret store, which only that principal's \
                     T1 child can open; this {tier} daemon refuses every secret rather than \
                     answer from the IKENGA_SECRET_* default alone. Serve this data dir under \
                     --executor-tier t1, or move its secrets/ directory aside to serve the \
                     operator default",
                    file.display()
                );
                tracing::error!("secrets unavailable: {why}");
                return Self {
                    principal: PrincipalLayer::Stranded(why),
                    env: EnvSource::Process,
                };
            }
            return Self::env_only();
        }
        let principal = match (key, data_dir) {
            (Some(Ok(key)), Some(dir)) => match PrincipalStore::open(dir, &key) {
                Ok(store) => {
                    tracing::info!("secrets: principal store open at {}", store.dir().display());
                    PrincipalLayer::Open(Arc::new(store))
                }
                Err(e) => PrincipalLayer::Broken(format!("principal secret store: {e}")),
            },
            (Some(Err(e)), _) => PrincipalLayer::Broken(e),
            (Some(Ok(_)), None) => PrincipalLayer::Broken(
                "principal secret store: no --data-dir to hold <data>/secrets/".into(),
            ),
            (None, _) => PrincipalLayer::Broken(format!(
                "principal secret store: no {} from the broker",
                crate::secrets::principal_store::WRAP_KEY_ENV
            )),
        };
        if let PrincipalLayer::Broken(why) = &principal {
            tracing::error!("secrets unavailable: {why}");
        }
        Self {
            principal,
            env: EnvSource::Process,
        }
    }

    /// A principal layer over the process environment (tests and embedders).
    pub fn with_principal_store(store: Arc<dyn SecretsStore>) -> Self {
        Self {
            principal: PrincipalLayer::Open(store),
            env: EnvSource::Process,
        }
    }

    #[cfg(test)]
    fn fixed(principal: PrincipalLayer, env: &[(&str, &str)]) -> Self {
        Self {
            principal,
            env: EnvSource::Fixed(
                env.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
        }
    }

    /// The principal store, `None` without one, `Err` when it is broken.
    fn store(&self) -> Result<Option<&Arc<dyn SecretsStore>>, String> {
        match &self.principal {
            PrincipalLayer::Absent => Ok(None),
            PrincipalLayer::Open(store) => Ok(Some(store)),
            PrincipalLayer::Broken(why) | PrincipalLayer::Stranded(why) => Err(why.clone()),
        }
    }

    /// Whether writes and the project / pkg scopes are servable.
    pub fn writable(&self) -> bool {
        matches!(self.principal, PrincipalLayer::Open(_))
    }

    /// `secrets_get`: the principal's own value, else the operator default.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        let mut principal_name_ok = false;
        if let Some(store) = self.store()? {
            if validate_existing_bare_name(key).is_ok() {
                principal_name_ok = true;
                if let Some(value) = store.get(key).map_err(store_err)? {
                    return Ok(Some(value));
                }
            }
        }
        if is_valid_key(key) {
            return self.env.get(key);
        }
        if principal_name_ok {
            // A name the principal store accepts but no env var can carry.
            return Ok(None);
        }
        Err(format!("invalid key {key:?}"))
    }

    /// `secrets_list_keys`: unscoped names of both layers, sorted, deduped.
    pub fn list_keys(&self) -> Result<Vec<String>, String> {
        let mut keys = self.env.keys();
        if let Some(store) = self.store()? {
            keys.extend(
                store
                    .list_meta()
                    .map_err(store_err)?
                    .into_iter()
                    .map(|m| m.name)
                    .filter(|name| parse_scoped(name).is_none()),
            );
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// `secrets_index_names`: every name either layer holds (scoped ones in
    /// their full `workspace::` / `project::` / `pkg::` form), never a value.
    pub fn index_names(&self) -> Result<Vec<String>, String> {
        let mut names = self.env.keys();
        if let Some(store) = self.store()? {
            names.extend(
                store
                    .list_meta()
                    .map_err(store_err)?
                    .into_iter()
                    .map(|m| m.name),
            );
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn writable_store(&self) -> Result<&Arc<dyn SecretsStore>, String> {
        self.store()?.ok_or_else(|| WRITE_REFUSAL.to_string())
    }

    /// `secrets_set`: into the principal's own store (never the default).
    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let store = self.writable_store()?;
        validate_key(key).map_err(|_| format!("invalid key {key:?}"))?;
        store.set(key, value).map_err(store_err)
    }

    /// `secrets_delete`: the principal's own value only; the operator
    /// default (if any) shows through again afterwards.
    pub fn delete(&self, key: &str) -> Result<(), String> {
        let store = self.writable_store()?;
        validate_existing_bare_name(key).map_err(|_| format!("invalid key {key:?}"))?;
        store.delete(key).map_err(store_err)
    }

    /// `secrets_get_scoped`. Workspace scope falls back to the operator
    /// default (the env namespace is workspace-wide); project and pkg scopes
    /// exist only in a principal store.
    pub fn get_scoped(&self, scope: &Scope, key: &str) -> Result<Option<String>, String> {
        match self.store()? {
            Some(store) => {
                let name =
                    existing_scoped_name(scope, key).map_err(|_| format!("invalid key {key:?}"))?;
                if let Some(value) = store.get(&name).map_err(store_err)? {
                    return Ok(Some(value));
                }
                if *scope == Scope::Workspace && is_valid_key(key) {
                    return self.env.get(key);
                }
                Ok(None)
            }
            None if *scope == Scope::Workspace => self.env.get(key),
            None => Err(scope_refusal(scope)),
        }
    }

    /// `secrets_list_keys_scoped`.
    pub fn list_keys_scoped(&self, scope: &Scope) -> Result<Vec<String>, String> {
        match self.store()? {
            Some(store) => {
                validate_existing_scope(scope)?;
                let mut keys: Vec<String> = store
                    .list_meta()
                    .map_err(store_err)?
                    .into_iter()
                    .filter_map(|m| parse_scoped(&m.name))
                    .filter(|(s, _)| s == scope)
                    .map(|(_, key)| key)
                    .collect();
                if *scope == Scope::Workspace {
                    keys.extend(self.env.keys());
                }
                keys.sort();
                keys.dedup();
                Ok(keys)
            }
            None if *scope == Scope::Workspace => Ok(self.env.keys()),
            None => Err(scope_refusal(scope)),
        }
    }

    /// `secrets_set_scoped`.
    pub fn set_scoped(&self, scope: &Scope, key: &str, value: &str) -> Result<(), String> {
        let store = self.writable_store()?;
        let name = checked_vault_key(scope, key)?;
        store.set(&name, value).map_err(store_err)
    }

    /// `secrets_delete_scoped`.
    pub fn delete_scoped(&self, scope: &Scope, key: &str) -> Result<(), String> {
        let store = self.writable_store()?;
        let name = existing_scoped_name(scope, key).map_err(|_| format!("invalid key {key:?}"))?;
        store.delete(&name).map_err(store_err)
    }

    /// `secrets_vault_status`. Without a principal layer this is exactly
    /// [`status`], unchanged since before WP-21.
    pub fn status(&self) -> VaultStatus {
        let principal_status = |available: bool, error: Option<String>| VaultStatus {
            available,
            keychain_backend: PRINCIPAL_BACKEND_LABEL.to_string(),
            error,
            mode: MODE_PRINCIPAL.to_string(),
            writable: available,
            locked: false,
            configured: true,
            idle_timeout_secs: 0,
            last_activity_unix_ms: None,
        };
        match &self.principal {
            PrincipalLayer::Absent => status(),
            PrincipalLayer::Open(store) => match store.probe() {
                Ok(()) => principal_status(true, None),
                Err(e) => principal_status(false, Some(e.to_string())),
            },
            PrincipalLayer::Broken(why) => principal_status(false, Some(why.clone())),
            PrincipalLayer::Stranded(why) => VaultStatus {
                available: false,
                error: Some(why.clone()),
                ..status()
            },
        }
    }

    /// `secrets_lock_state` (and `secrets_lock`, which has nothing to lock).
    ///
    /// With a principal layer (T1, open or broken): a server-held key is
    /// always configured and never locked — the same `configured`
    /// [`Self::status`] reports. A broken store still answers this; its
    /// `available: false` status is what keeps the FE read-only.
    ///
    /// `None` without one (T0): the daemon does not serve the lock family,
    /// so the RPC answers the unknown-command error it always has and the
    /// FE stays read-only (see the module docs, "No passphrase layer").
    pub fn lock_state(&self) -> Option<LockState> {
        match self.principal {
            PrincipalLayer::Absent | PrincipalLayer::Stranded(_) => None,
            PrincipalLayer::Open(_) | PrincipalLayer::Broken(_) => Some(LockState {
                configured: true,
                locked: false,
                idle_timeout_secs: 0,
                last_activity_unix_ms: None,
            }),
        }
    }
}

fn scope_refusal(scope: &Scope) -> String {
    let kind = match scope {
        Scope::Workspace => "workspace",
        Scope::Project { .. } => "project",
        Scope::Pkg { .. } => "pkg",
    };
    format!("scope {kind:?} is not servable — {SCOPE_REFUSAL}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_keys_are_env_var_names() {
        assert!(is_valid_key("ANTHROPIC_API_KEY"));
        assert!(is_valid_key("A1"));
        assert!(!is_valid_key(""));
        // The dotted pkg-scope convention cannot be an env var.
        assert!(!is_valid_key("studio.fal"));
        assert!(!is_valid_key("A-B"));
        assert!(!is_valid_key("A B"));
        // No traversal into the wider environment via a crafted name.
        assert!(!is_valid_key("PATH}${"));
    }

    #[test]
    fn get_rejects_invalid_keys_before_touching_env() {
        assert!(get("").is_err());
        assert!(get("studio.fal").is_err());
    }

    #[test]
    fn list_strips_prefix_sorts_and_ignores_everything_else() {
        let names = [
            "IKENGA_SECRET_RESEND_API_KEY",
            "IKENGA_SECRET_ANTHROPIC_API_KEY",
            // Not in the namespace — must never be listed.
            "AWS_SECRET_ACCESS_KEY",
            "IKENGA_AUTH_TOKEN",
            "IKENGA_VAULT_KEY",
            // Prefix with an empty remainder is not a key.
            "IKENGA_SECRET_",
            // Prefix with a name env vars cannot express.
            "IKENGA_SECRET_bad-key",
        ]
        .map(str::to_string);

        assert_eq!(
            list_keys_from(names),
            vec![
                "ANTHROPIC_API_KEY".to_string(),
                "RESEND_API_KEY".to_string()
            ]
        );
    }

    #[test]
    fn status_is_readable_but_never_writable() {
        let s = status();
        assert!(s.available, "an env-backed store is always readable");
        assert!(!s.writable);
        assert_eq!(s.mode, MODE_ENV);
        assert!(s.error.is_none());
        assert!(!s.keychain_backend.is_empty());
    }

    /// The three fields the frontend destructures must survive verbatim, in
    /// snake_case. A rename here reads as `undefined` in TS and silently
    /// reports every connector as unconfigured.
    #[test]
    fn wire_shape_is_a_superset_of_the_desktop_contract() {
        let v = serde_json::to_value(status()).expect("serialize");
        let obj = v.as_object().expect("object");
        for field in [
            "available",
            "keychain_backend",
            "error",
            "mode",
            "writable",
            "locked",
            "configured",
            "idle_timeout_secs",
            "last_activity_unix_ms",
        ] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        assert_eq!(obj.len(), 9);
    }

    // ─── the two layers (WP-21) ───────────────────────────────────────────

    use crate::secrets::principal_store::{PrincipalStore, WrapKey};

    const KEK: [u8; 32] = [3u8; 32];
    const ADA: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

    fn principal_layer(dir: &std::path::Path) -> PrincipalLayer {
        let key = WrapKey::derive(&KEK, ADA).unwrap();
        PrincipalLayer::Open(Arc::new(PrincipalStore::open(dir, &key).unwrap()))
    }

    const DEFAULTS: &[(&str, &str)] = &[("OPENAI_API_KEY", "op-openai"), ("SHARED", "op-shared")];

    /// ADR-023 §4: principal store → `IKENGA_SECRET_*` operator default.
    #[test]
    fn the_principal_value_overrides_the_operator_default() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonSecrets::fixed(principal_layer(tmp.path()), DEFAULTS);
        assert_eq!(
            s.get("OPENAI_API_KEY").unwrap().as_deref(),
            Some("op-openai")
        );
        s.set("OPENAI_API_KEY", "ada-openai").unwrap();
        assert_eq!(
            s.get("OPENAI_API_KEY").unwrap().as_deref(),
            Some("ada-openai")
        );
        assert_eq!(s.get("SHARED").unwrap().as_deref(), Some("op-shared"));
        // Deleting the principal's value reveals the default again.
        s.delete("OPENAI_API_KEY").unwrap();
        assert_eq!(
            s.get("OPENAI_API_KEY").unwrap().as_deref(),
            Some("op-openai")
        );
        assert_eq!(s.get("NOPE").unwrap(), None);

        // Names a principal store can hold but an env var can't.
        s.set("studio.fal", "fal").unwrap();
        assert_eq!(s.get("studio.fal").unwrap().as_deref(), Some("fal"));
        s.set("OWN", "mine").unwrap();
        assert_eq!(
            s.list_keys().unwrap(),
            vec!["OPENAI_API_KEY", "OWN", "SHARED", "studio.fal"]
        );
        assert!(s.set("bad key", "x").is_err());
        assert!(s.get("").is_err());
    }

    #[test]
    fn scopes_layer_workspace_over_the_default_and_serve_project_and_pkg() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonSecrets::fixed(principal_layer(tmp.path()), DEFAULTS);
        let ws = Scope::Workspace;
        let proj = Scope::project("p1");
        let pkg = Scope::pkg("com.x.y");

        assert_eq!(
            s.get_scoped(&ws, "SHARED").unwrap().as_deref(),
            Some("op-shared")
        );
        s.set_scoped(&ws, "SHARED", "ada-shared").unwrap();
        assert_eq!(
            s.get_scoped(&ws, "SHARED").unwrap().as_deref(),
            Some("ada-shared")
        );
        // The unscoped namespace is separate from `workspace::`, as on the desktop.
        assert_eq!(s.get("SHARED").unwrap().as_deref(), Some("op-shared"));

        assert_eq!(
            s.get_scoped(&proj, "SHARED").unwrap(),
            None,
            "no default below workspace"
        );
        s.set_scoped(&proj, "TOKEN", "p").unwrap();
        s.set_scoped(&pkg, "TOKEN", "k").unwrap();
        assert_eq!(s.get_scoped(&proj, "TOKEN").unwrap().as_deref(), Some("p"));
        assert_eq!(s.get_scoped(&pkg, "TOKEN").unwrap().as_deref(), Some("k"));
        assert_eq!(s.list_keys_scoped(&proj).unwrap(), vec!["TOKEN"]);
        assert_eq!(
            s.list_keys_scoped(&ws).unwrap(),
            vec!["OPENAI_API_KEY", "SHARED"]
        );
        assert!(s.list_keys().unwrap().iter().all(|k| !k.contains("::")));
        assert!(s
            .index_names()
            .unwrap()
            .contains(&"project::p1::TOKEN".to_string()));
        s.delete_scoped(&proj, "TOKEN").unwrap();
        assert_eq!(s.get_scoped(&proj, "TOKEN").unwrap(), None);
        // A scope id can't address another scope's entry.
        assert!(s.set_scoped(&Scope::project("a::b"), "c", "x").is_err());
    }

    /// T0 is unchanged: read-only flat namespace, project/pkg refused.
    #[test]
    fn without_a_principal_store_the_daemon_is_the_env_namespace() {
        let s = DaemonSecrets::fixed(PrincipalLayer::Absent, DEFAULTS);
        assert!(!s.writable());
        assert_eq!(s.get("SHARED").unwrap().as_deref(), Some("op-shared"));
        assert!(s.get("studio.fal").is_err());
        assert_eq!(s.list_keys().unwrap(), vec!["OPENAI_API_KEY", "SHARED"]);
        assert_eq!(s.set("K", "v").unwrap_err(), WRITE_REFUSAL);
        assert_eq!(s.delete("K").unwrap_err(), WRITE_REFUSAL);
        assert_eq!(
            s.set_scoped(&Scope::Workspace, "K", "v").unwrap_err(),
            WRITE_REFUSAL
        );
        assert!(s
            .get_scoped(&Scope::project("p"), "K")
            .unwrap_err()
            .contains(SCOPE_REFUSAL));
        let st = s.status();
        assert_eq!(
            (st.mode.as_str(), st.writable, st.available),
            (MODE_ENV, false, true)
        );
        // The lock family stays unserved on T0, as before WP-21: an
        // unlocked state here would make Settings → Secrets offer writes
        // that WRITE_REFUSAL then rejects.
        assert!(s.lock_state().is_none());
    }

    /// Fail closed: a T1 child whose store won't open never silently serves
    /// the operator default in place of the principal's own credential.
    #[test]
    fn a_broken_principal_store_fails_closed() {
        let s = DaemonSecrets::fixed(PrincipalLayer::Broken("no key".into()), DEFAULTS);
        assert_eq!(s.get("SHARED").unwrap_err(), "no key");
        assert!(s.list_keys().is_err());
        assert!(s.get_scoped(&Scope::Workspace, "SHARED").is_err());
        assert!(s.set("K", "v").is_err());
        let st = s.status();
        assert!(!st.available && !st.writable);
        assert_eq!(st.mode, MODE_PRINCIPAL);
        assert_eq!(st.error.as_deref(), Some("no key"));
        // Still answered (T1), but `available: false` keeps the FE read-only.
        assert!(s.lock_state().is_some());
    }

    #[test]
    fn principal_status_is_writable_and_unlocked() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonSecrets::fixed(principal_layer(tmp.path()), DEFAULTS);
        let st = s.status();
        assert!(st.available && st.writable && st.configured && !st.locked);
        assert_eq!(st.mode, MODE_PRINCIPAL);
        let v = serde_json::to_value(&st).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 9, "same wire shape");
        let lock = s.lock_state().expect("T1 serves the lock family");
        assert!(
            lock.configured && !lock.locked,
            "consistent with status().configured"
        );
    }

    /// A T0 daemon never opens a principal store, and building one never
    /// leaves the wrapping-key variable behind in the process environment.
    #[test]
    fn t0_for_daemon_is_env_only() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonSecrets::for_daemon(Some(tmp.path()), crate::executor::ExecutorTier::T0);
        assert!(!s.writable());
        assert!(!tmp.path().join("secrets").exists());
    }

    /// L21-2 / DEC-R18-1: a T1 principal child the broker handed no key
    /// never serves the operator default alone in place of the principal's
    /// own store.
    #[test]
    fn a_t1_child_without_a_handoff_key_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let s = DaemonSecrets::for_daemon(Some(tmp.path()), crate::executor::ExecutorTier::T1);
        assert!(!s.writable());
        assert!(s
            .get("ANY")
            .unwrap_err()
            .contains("no IKENGA_PRINCIPAL_SECRETS_KEY"));
        assert!(s.list_keys().is_err());
        assert!(s.index_names().is_err());
        assert!(s.get_scoped(&Scope::Workspace, "ANY").is_err());
        let st = s.status();
        assert!(!st.available && !st.writable);
        assert!(!tmp.path().join("secrets").exists(), "nothing minted");
    }

    /// L21-2: a T0 daemon pointed at a data dir that holds a principal store
    /// (a downgraded or copied T1 data dir) fails closed too, rather than
    /// silently answering from the operator default. The lock family stays
    /// unserved there (T0), and status reports the env mode, unavailable.
    #[test]
    fn t0_over_a_principal_store_fails_closed() {
        for leave in ["both", "values-only", "envelope-only"] {
            let tmp = tempfile::tempdir().unwrap();
            let store =
                PrincipalStore::open(tmp.path(), &WrapKey::derive(&KEK, ADA).unwrap()).unwrap();
            store.set("K", "ada").unwrap();
            drop(store);
            let dir = tmp.path().join("secrets");
            match leave {
                "values-only" => std::fs::remove_file(dir.join("envelope.json")).unwrap(),
                "envelope-only" => std::fs::remove_file(dir.join("values.json")).unwrap(),
                _ => {}
            }
            let s = DaemonSecrets::for_daemon(Some(tmp.path()), crate::executor::ExecutorTier::T0);
            assert!(!s.writable(), "{leave}");
            assert!(s.get("K").is_err(), "{leave}: never the default alone");
            assert!(s.list_keys().is_err(), "{leave}");
            assert!(s.get_scoped(&Scope::Workspace, "K").is_err(), "{leave}");
            let st = s.status();
            assert!(!st.available && !st.writable, "{leave}");
            assert_eq!(st.mode, MODE_ENV, "{leave}");
            assert!(st.error.is_some(), "{leave}");
            assert!(
                s.lock_state().is_none(),
                "{leave}: T0 never serves the lock family"
            );
        }
        // An empty secrets/ dir holds nothing to lose: T0 as before.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("secrets")).unwrap();
        let s = DaemonSecrets::for_daemon(Some(tmp.path()), crate::executor::ExecutorTier::T0);
        assert!(s.status().available);
    }
}
