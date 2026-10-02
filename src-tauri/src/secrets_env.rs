//! Secrets for the headless daemon: the `IKENGA_SECRET_*` operator
//! namespace, and — in a T1 principal child — the principal's own encrypted
//! store layered over it (remote-access WP-21).
//!
//! # Layers (G-PRINCIPAL §10, ADR-023 `:45`, DEC-R18-1)
//!
//! 1. **The principal store** ([`principal`]): only in a T1 principal child
//!    whose broker handed it a key. Per-principal, encrypted at rest under a
//!    DEK wrapped by a key the broker derives from its operator KEK. Holds
//!    every scope (`workspace::K`, `project::<id>::K`, `pkg::<id>::K`, the
//!    desktop vault's naming) and is writable.
//! 2. **The operator default**: `IKENGA_SECRET_<KEY>`, read-only, workspace
//!    scope only. A principal's own value for the same key wins.
//!
//! T0 (and any daemon without a hand-off) has only layer 2 and behaves
//! exactly as before WP-21: the "no vault" sections below describe it.
//!
//! `store` and `crypto` (the `SecretsStore` trait and the AES-GCM helpers),
//! `principal` and `app_lock` (the WP-72 core the daemon's `app_lock_*` arms
//! share with the desktop) live in `src/secrets/` but are mounted HERE,
//! because `lib.rs` gates `crate::secrets` on `desktop`; `crate::secrets`
//! re-exports `store` and `crypto`.
//!
//! # T0: environment-backed reads
//!
//! Lives outside `commands/` for the same reason `path_allow` does: the
//! daemon needs it and `commands/` is desktop-only, built around
//! `#[tauri::command]` and `AppHandle` (whose default type parameter IS
//! `Wry`, which a `--no-default-features` build does not link).
//!
//! # T0: there is no vault here, and that is the decision — not an omission
//!
//! (WP-21 changes this only for T1, where the store is the principal's own,
//! in a process running as the principal's uid, and the "remote client" is
//! that principal — see Layers above.)
//!
//! The headless build has **no server-side consumer of a secret at all**.
//! Every reader in the crate — `pkg_content`, `pkg_fetch`,
//! `pkg_sidecar_stream`, `pkg/lifecycle`, `iyke/secrets` — is
//! `#[cfg(feature = "desktop")]`. So a Stronghold-style store in the daemon
//! would ship an encrypted-at-rest blob whose *only* reader is `secrets_get`
//! reaching back across the bearer-token boundary. Encrypting at rest in
//! order to enable a remote `printenv` is a worse posture, not a better one.
//!
//! What the daemon serves instead is a flat, read-only namespace the operator
//! opts into explicitly: environment variables named `IKENGA_SECRET_<KEY>`.
//! The operator decides, on the host, which credentials the remote session may
//! see. Nothing else in the daemon's environment is reachable — taking the key
//! as a bare env var name would have made this a remote `printenv` for every
//! credential the process inherited.
//!
//! # These are RPC-only and never reach a shell — do not "fix" that
//!
//! [`crate::pty`]'s `is_host_only_env` denylists `IKENGA_SECRET_*` (alongside
//! `IKENGA_AUTH_TOKEN` and `IKENGA_VAULT_KEY`) from every PTY child. That is
//! deliberate and load-bearing: these values are fetched deliberately through
//! an authenticated RPC, they are not shell environment. A future reader who
//! sees a secret "missing" inside a remote terminal should reach for this
//! module's RPC, not relax that filter.
//!
//! # Writes
//!
//! On T0 there are none. [`WRITE_REFUSAL`] is the operator-facing explanation the
//! RPC layer returns for every write command, so a reader hitting it can tell
//! *decided* from *unfinished*. On T1 writes go to the principal store only;
//! the operator default stays read-only.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Serialize;

#[path = "secrets/app_lock.rs"]
pub mod app_lock;
#[path = "secrets/crypto.rs"]
pub mod crypto;
#[cfg(unix)]
#[path = "secrets/principal.rs"]
pub mod principal;
#[path = "secrets/store.rs"]
pub mod store;

use store::SharedSecretStore;

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
///
/// A leading underscore is reserved for host-only plumbing that rides the
/// `IKENGA_SECRET_` prefix to stay inside the PTY / executor deny floor —
/// [`principal::HANDOFF_ENV`] (`IKENGA_SECRET__PRINCIPAL_KEY`) — so such a
/// name is never listed or served.
pub fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('_')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
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

// ─── WP-21: the layered store ──────────────────────────────────────────────

/// `VaultStatus::mode` for a T1 principal child's own store.
pub const MODE_PRINCIPAL: &str = "principal";

/// Returned by the passphrase / lock arms on the daemon. The T0 env
/// namespace has no lock layer, and the principal store is keyed by the
/// broker (DEC-R18-1) precisely so it never needs one.
pub const LOCK_REFUSAL: &str = concat!(
    "the headless daemon's secret store has no passphrase or lock. ",
    "On T1 each principal's store is encrypted at rest under a key the broker derives from its ",
    "operator-held KEK (remote-access DEC-R18-1), so background work keeps decrypting while you ",
    "are signed out; a password-derived key was rejected for v1. On T0 the daemon reads only ",
    "IKENGA_SECRET_<KEY> environment variables. Signing out (T1) or revoking the token (T0) is ",
    "the boundary here."
);

/// `{ kind: "workspace" } | { kind: "project", id } | { kind: "pkg", id }`
/// (`VaultScope` in `src/lib/tauri-cmd.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Workspace,
    Project(String),
    Pkg(String),
}

impl Scope {
    fn prefix(&self) -> String {
        match self {
            Scope::Workspace => "workspace::".into(),
            Scope::Project(id) => format!("project::{id}::"),
            Scope::Pkg(id) => format!("pkg::{id}::"),
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Scope::Workspace => Ok(()),
            Scope::Project(id) | Scope::Pkg(id) => {
                if is_principal_token(id, 128) {
                    Ok(())
                } else {
                    Err(format!(
                        "invalid scope id {id:?}: expected 1-128 of [A-Za-z0-9_.-]"
                    ))
                }
            }
        }
    }

    /// The store name, desktop-vault style (`commands::secrets::vault_key`).
    fn name(&self, key: &str) -> Result<String, String> {
        self.validate()?;
        if !is_principal_token(key, 256) {
            return Err(format!(
                "invalid key {key:?}: expected 1-256 of [A-Za-z0-9_.-]"
            ));
        }
        Ok(format!("{}{key}", self.prefix()))
    }
}

/// `prctl(PR_SET_DUMPABLE, 0)`: `/proc/<pid>/{environ,mem,…}` become
/// root-owned and ptrace by the same uid is refused.
#[cfg(all(target_os = "linux", not(test)))]
fn set_non_dumpable() {
    // SAFETY: plain prctl with integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        tracing::warn!(
            "prctl(PR_SET_DUMPABLE, 0) failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

fn is_principal_token(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Wire shape of `secrets_lock_state` (the desktop's `secrets::LockState`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LockStateWire {
    pub configured: bool,
    pub locked: bool,
    pub idle_timeout_secs: u64,
    pub last_activity_unix_ms: Option<u64>,
}

/// The daemon has no lock layer in either mode, so this is the one true
/// answer: `configured` agrees with [`status`] (values are readable, the FE
/// lists them), and nothing is ever locked.
pub fn lock_state() -> LockStateWire {
    LockStateWire {
        configured: true,
        locked: false,
        idle_timeout_secs: 0,
        last_activity_unix_ms: None,
    }
}

/// Where the operator layer reads from: the process environment in
/// production, a fixed map in tests (so they never mutate global env).
enum EnvLayer {
    Process,
    #[cfg(test)]
    Fixed(std::collections::BTreeMap<String, String>),
}

impl EnvLayer {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self {
            EnvLayer::Process => get(key),
            #[cfg(test)]
            EnvLayer::Fixed(map) => {
                if !is_valid_key(key) {
                    return Err(format!("invalid key {key:?}"));
                }
                Ok(map.get(&format!("{ENV_PREFIX}{key}")).cloned())
            }
        }
    }

    fn list(&self) -> Vec<String> {
        match self {
            EnvLayer::Process => list_keys(),
            #[cfg(test)]
            EnvLayer::Fixed(map) => list_keys_from(map.keys().cloned()),
        }
    }
}

enum PrincipalLayer {
    /// T0, or a daemon nobody handed a key.
    Absent,
    Ready(SharedSecretStore),
    /// A principal child whose store could not be opened. Fails closed: no
    /// read falls through to the operator default, which could differ from
    /// the principal's own value.
    Broken(String),
}

/// The daemon's secret surface: the principal store (if any) over the
/// `IKENGA_SECRET_*` operator default. Backs every `secrets_*` RPC arm.
pub struct DaemonSecrets {
    principal: PrincipalLayer,
    env: EnvLayer,
}

impl std::fmt::Debug for DaemonSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let principal = match &self.principal {
            PrincipalLayer::Absent => "absent",
            PrincipalLayer::Ready(_) => "ready",
            PrincipalLayer::Broken(_) => "broken",
        };
        f.debug_struct("DaemonSecrets")
            .field("principal", &principal)
            .finish_non_exhaustive()
    }
}

impl DaemonSecrets {
    /// The env namespace only: T0, exactly as before WP-21.
    pub fn env_only() -> Self {
        Self {
            principal: PrincipalLayer::Absent,
            env: EnvLayer::Process,
        }
    }

    /// A principal store over the process-env default.
    pub fn with_principal(store: SharedSecretStore) -> Self {
        Self {
            principal: PrincipalLayer::Ready(store),
            env: EnvLayer::Process,
        }
    }

    /// Boot: in a T1 child, take the broker's hand-off (always removing it
    /// from this process's environment) and open the principal store in the
    /// child's data dir. T0 constructs no principal (G-PRINCIPAL §1).
    pub fn from_boot(
        tier: crate::executor::ExecutorTier,
        data_dir: Option<&std::path::Path>,
    ) -> Self {
        // Only a T1 child reads (and strips) the hand-off. Reading it on T0
        // would gain nothing — no broker hands one to a T0 daemon, and the
        // PTY / executor floors drop the name anyway — and would let every
        // T0 router (tests included) race the process-global variable.
        //
        // `tier == T1` here means a principal child: the T1 broker never
        // builds this router (`server::t1_boot` routes to the broker or to
        // `principal_child_boot`, and only the latter serves it).
        #[cfg(unix)]
        {
            if tier != crate::executor::ExecutorTier::T1 {
                return Self::t0(data_dir);
            }
            // The child holds the derived key in memory from here on, and its
            // initial copy stays in `/proc/self/environ` whatever `remove_var`
            // does: make the process non-dumpable first, so the principal's
            // other processes (same uid) can't read either through `/proc`
            // or ptrace. Not in tests, which share one process.
            #[cfg(all(target_os = "linux", not(test)))]
            set_non_dumpable();
            Self::from_handoff(principal::take_handoff(), tier, data_dir)
        }
        #[cfg(not(unix))]
        {
            let _ = (tier, data_dir);
            Self::env_only()
        }
    }

    /// T0: the env namespace — unless the data dir holds a principal store
    /// (`secrets/dek.json`), which a T0 daemon can't open: then fail closed
    /// rather than serve the operator defaults over it.
    fn t0(data_dir: Option<&std::path::Path>) -> Self {
        match data_dir.map(Self::sealed_store) {
            Some(Some(dek)) => Self::broken(format!(
                "{} holds a principal secret store, which only a T1 principal child can open",
                dek.display()
            )),
            _ => Self::env_only(),
        }
    }

    fn sealed_store(data_dir: &std::path::Path) -> Option<std::path::PathBuf> {
        let dek = data_dir.join("secrets").join("dek.json");
        std::fs::symlink_metadata(&dek).is_ok().then_some(dek)
    }

    /// [`from_boot`](Self::from_boot) after the hand-off was taken.
    #[cfg(unix)]
    fn from_handoff(
        taken: Result<Option<principal::PrincipalKey>, String>,
        tier: crate::executor::ExecutorTier,
        data_dir: Option<&std::path::Path>,
    ) -> Self {
        if tier != crate::executor::ExecutorTier::T1 {
            if !matches!(taken, Ok(None)) {
                tracing::warn!(
                    "{} was set, but this daemon is not a T1 principal child; stripped and \
                     ignored",
                    principal::HANDOFF_ENV
                );
            }
            return Self::t0(data_dir);
        }
        let key = match taken {
            // A principal child always gets a key from its broker. Without
            // one it must not quietly serve the operator defaults as if they
            // were the principal's (fail closed).
            Ok(None) => {
                tracing::error!(
                    "principal child started without {}; secret store disabled",
                    principal::HANDOFF_ENV
                );
                return Self::broken(format!(
                    "this principal child was started without a secrets key from its broker \
                     ({}); restart it through the T1 broker",
                    principal::HANDOFF_ENV
                ));
            }
            Ok(Some(key)) => key,
            Err(e) => {
                tracing::error!("principal secret store disabled: {e}");
                return Self::broken(e);
            }
        };
        let Some(data_dir) = data_dir else {
            return Self::broken("a principal child without --data-dir has no store".into());
        };
        // `<root>/principals/<id>/data`: the key must be this dir's.
        let dir_id = data_dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        if dir_id.as_deref() != Some(key.id().to_string().as_str()) {
            return Self::broken(format!(
                "the broker's key is for principal {}, but --data-dir {} is not that \
                 principal's",
                key.id(),
                data_dir.display()
            ));
        }
        match principal::PrincipalStore::open(data_dir, &key) {
            Ok(store) => {
                tracing::info!(
                    "principal secret store open at {} (key version {})",
                    store.dir().display(),
                    key.version()
                );
                Self::with_principal(Arc::new(store))
            }
            Err(e) => {
                tracing::error!("principal secret store unavailable: {e}");
                Self::broken(e.to_string())
            }
        }
    }

    fn broken(reason: String) -> Self {
        Self {
            principal: PrincipalLayer::Broken(reason),
            env: EnvLayer::Process,
        }
    }

    #[cfg(test)]
    fn for_test(
        principal: Option<SharedSecretStore>,
        env: std::collections::BTreeMap<String, String>,
    ) -> Self {
        Self {
            principal: principal.map_or(PrincipalLayer::Absent, PrincipalLayer::Ready),
            env: EnvLayer::Fixed(env),
        }
    }

    /// Whether this daemon has a principal layer (ready or broken).
    pub fn has_principal(&self) -> bool {
        !matches!(self.principal, PrincipalLayer::Absent)
    }

    fn store(&self) -> Result<Option<&SharedSecretStore>, String> {
        match &self.principal {
            PrincipalLayer::Absent => Ok(None),
            PrincipalLayer::Ready(store) => Ok(Some(store)),
            PrincipalLayer::Broken(reason) => Err(format!(
                "the principal secret store is unavailable: {reason}"
            )),
        }
    }

    /// Principal value first, then (workspace scope only) the operator
    /// default.
    pub fn get(&self, scope: &Scope, key: &str) -> Result<Option<String>, String> {
        let Some(store) = self.store()? else {
            return match scope {
                Scope::Workspace => self.env.get(key),
                _ => Err(SCOPE_REFUSAL.to_string()),
            };
        };
        let name = scope.name(key)?;
        if let Some(value) = store.get(&name).map_err(String::from)? {
            return Ok(Some(value));
        }
        match scope {
            // A key the env layer cannot name (dotted) simply has no default.
            Scope::Workspace if is_valid_key(key) => self.env.get(key),
            _ => Ok(None),
        }
    }

    /// The scope's key names across both layers, sorted, deduplicated.
    pub fn list_keys(&self, scope: &Scope) -> Result<Vec<String>, String> {
        let Some(store) = self.store()? else {
            return match scope {
                Scope::Workspace => Ok(self.env.list()),
                _ => Err(SCOPE_REFUSAL.to_string()),
            };
        };
        scope.validate()?;
        let prefix = scope.prefix();
        let mut out: BTreeSet<String> = store
            .list_meta()
            .map_err(String::from)?
            .into_iter()
            .filter_map(|m| m.name.strip_prefix(&prefix).map(str::to_string))
            // Neither ids nor keys contain `::`: a longer name belongs to
            // another scope's namespace, never to this one.
            .filter(|k| !k.contains("::"))
            .collect();
        if *scope == Scope::Workspace {
            out.extend(self.env.list());
        }
        Ok(out.into_iter().collect())
    }

    /// Write the principal's own value. The operator default is read-only.
    pub fn set(&self, scope: &Scope, key: &str, value: &str) -> Result<(), String> {
        let Some(store) = self.store()? else {
            return Err(WRITE_REFUSAL.to_string());
        };
        store.set(&scope.name(key)?, value).map_err(String::from)
    }

    /// Remove the principal's own value. An operator default for the same
    /// key becomes visible again; it cannot be deleted from here.
    pub fn delete(&self, scope: &Scope, key: &str) -> Result<(), String> {
        let Some(store) = self.store()? else {
            return Err(WRITE_REFUSAL.to_string());
        };
        store.delete(&scope.name(key)?).map_err(String::from)
    }

    /// `secrets_index_names`: the principal store's full names (desktop
    /// `secrets-index.json` style) plus the operator namespace's keys (as the
    /// T0 daemon has always answered).
    pub fn index_names(&self) -> Result<Vec<String>, String> {
        let mut out: BTreeSet<String> = BTreeSet::new();
        if let Some(store) = self.store()? {
            out.extend(
                store
                    .list_meta()
                    .map_err(String::from)?
                    .into_iter()
                    .map(|m| m.name),
            );
        }
        out.extend(self.env.list());
        Ok(out.into_iter().collect())
    }

    /// `secrets_vault_status`.
    pub fn status(&self) -> VaultStatus {
        match &self.principal {
            PrincipalLayer::Absent => status(),
            PrincipalLayer::Ready(store) => {
                let probe = store.probe();
                VaultStatus {
                    available: probe.is_ok(),
                    keychain_backend: store.backend_label().to_string(),
                    error: probe.err().map(String::from),
                    mode: MODE_PRINCIPAL.to_string(),
                    writable: true,
                    locked: false,
                    configured: true,
                    idle_timeout_secs: 0,
                    last_activity_unix_ms: None,
                }
            }
            PrincipalLayer::Broken(reason) => VaultStatus {
                available: false,
                keychain_backend: "principal store".to_string(),
                error: Some(reason.clone()),
                mode: MODE_PRINCIPAL.to_string(),
                writable: false,
                locked: false,
                configured: true,
                idle_timeout_secs: 0,
                last_activity_unix_ms: None,
            },
        }
    }
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
        // Leading underscore: reserved host-only plumbing (WP-21 hand-off).
        assert!(!is_valid_key("_PRINCIPAL_KEY"));
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

    // ─── WP-21 layering ────────────────────────────────────────────────

    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::store::{SecretMeta, SecretsStore, StoreError};

    /// An in-memory principal layer.
    #[derive(Default)]
    struct MemStore(Mutex<BTreeMap<String, String>>);

    impl SecretsStore for MemStore {
        fn get(&self, name: &str) -> Result<Option<String>, StoreError> {
            Ok(self.0.lock().unwrap().get(name).cloned())
        }
        fn set(&self, name: &str, value: &str) -> Result<(), StoreError> {
            self.0.lock().unwrap().insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<(), StoreError> {
            self.0.lock().unwrap().remove(name);
            Ok(())
        }
        fn list_meta(&self) -> Result<Vec<SecretMeta>, StoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .keys()
                .map(|n| SecretMeta { name: n.clone() })
                .collect())
        }
        fn replace_all(&self, _: &BTreeMap<String, String>) -> Result<usize, StoreError> {
            unimplemented!()
        }
        fn probe(&self) -> Result<(), StoreError> {
            Ok(())
        }
        fn prepare_encryption(&self) -> Result<(), StoreError> {
            Ok(())
        }
        fn detect_configuration(&self) -> Result<bool, StoreError> {
            Ok(true)
        }
        fn backend_label(&self) -> &'static str {
            "mem"
        }
    }

    fn env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("IKENGA_SECRET_ANTHROPIC_API_KEY".into(), "operator".into()),
            (
                "IKENGA_SECRET_RESEND_API_KEY".into(),
                "operator-resend".into(),
            ),
            (
                "IKENGA_SECRET__PRINCIPAL_KEY".into(),
                "1:never:listed".into(),
            ),
        ])
    }

    fn layered() -> DaemonSecrets {
        DaemonSecrets::for_test(Some(Arc::new(MemStore::default())), env())
    }

    #[test]
    fn principal_value_wins_over_the_operator_default() {
        let s = layered();
        let ws = Scope::Workspace;
        assert_eq!(
            s.get(&ws, "ANTHROPIC_API_KEY").unwrap().as_deref(),
            Some("operator"),
            "no principal value: the operator default"
        );
        s.set(&ws, "ANTHROPIC_API_KEY", "mine").unwrap();
        assert_eq!(
            s.get(&ws, "ANTHROPIC_API_KEY").unwrap().as_deref(),
            Some("mine")
        );
        s.delete(&ws, "ANTHROPIC_API_KEY").unwrap();
        assert_eq!(
            s.get(&ws, "ANTHROPIC_API_KEY").unwrap().as_deref(),
            Some("operator"),
            "deleting the override uncovers the default"
        );
        // A dotted key lives only in the principal layer.
        s.set(&ws, "studio.fal", "f").unwrap();
        assert_eq!(s.get(&ws, "studio.fal").unwrap().as_deref(), Some("f"));
        assert_eq!(s.get(&ws, "other.key").unwrap(), None);
    }

    #[test]
    fn project_and_pkg_scopes_are_principal_only() {
        let s = layered();
        let proj = Scope::Project("p1".into());
        assert_eq!(s.get(&proj, "ANTHROPIC_API_KEY").unwrap(), None);
        s.set(&proj, "TOKEN", "t").unwrap();
        s.set(&Scope::Pkg("studio".into()), "fal", "x").unwrap();
        assert_eq!(s.list_keys(&proj).unwrap(), vec!["TOKEN"]);
        assert_eq!(
            s.list_keys(&Scope::Pkg("studio".into())).unwrap(),
            vec!["fal"]
        );
        assert!(s.get(&Scope::Project("a::b".into()), "K").is_err());
        assert!(s.set(&proj, "a::b", "v").is_err());
    }

    #[test]
    fn listing_unions_both_layers_and_hides_reserved_names() {
        let s = layered();
        s.set(&Scope::Workspace, "MINE", "m").unwrap();
        s.set(&Scope::Workspace, "RESEND_API_KEY", "r").unwrap();
        s.set(&Scope::Project("p".into()), "P", "p").unwrap();
        assert_eq!(
            s.list_keys(&Scope::Workspace).unwrap(),
            vec!["ANTHROPIC_API_KEY", "MINE", "RESEND_API_KEY"]
        );
        assert_eq!(
            s.index_names().unwrap(),
            vec![
                "ANTHROPIC_API_KEY",
                "RESEND_API_KEY",
                "project::p::P",
                "workspace::MINE",
                "workspace::RESEND_API_KEY"
            ]
        );
        assert!(s
            .get(&Scope::Workspace, "_PRINCIPAL_KEY")
            .unwrap()
            .is_none());
    }

    #[test]
    fn without_a_principal_the_daemon_is_unchanged() {
        let s = DaemonSecrets::for_test(None, env());
        assert!(!s.has_principal());
        assert_eq!(
            s.get(&Scope::Workspace, "ANTHROPIC_API_KEY")
                .unwrap()
                .as_deref(),
            Some("operator")
        );
        assert!(s.get(&Scope::Workspace, "studio.fal").is_err());
        assert!(s
            .get(&Scope::Project("p".into()), "K")
            .unwrap_err()
            .contains("flat"));
        assert!(s
            .set(&Scope::Workspace, "K", "v")
            .unwrap_err()
            .contains("no vault, by design"));
        assert!(s.delete(&Scope::Workspace, "K").is_err());
        assert_eq!(
            s.list_keys(&Scope::Workspace).unwrap(),
            vec!["ANTHROPIC_API_KEY", "RESEND_API_KEY"]
        );
        assert_eq!(
            s.index_names().unwrap(),
            s.list_keys(&Scope::Workspace).unwrap()
        );
        let st = s.status();
        assert_eq!(st.mode, MODE_ENV);
        assert!(!st.writable);
    }

    #[test]
    fn a_broken_principal_store_fails_closed() {
        let s = DaemonSecrets {
            principal: PrincipalLayer::Broken("dek does not unwrap".into()),
            env: EnvLayer::Fixed(env()),
        };
        let err = s.get(&Scope::Workspace, "ANTHROPIC_API_KEY").unwrap_err();
        assert!(err.contains("dek does not unwrap"), "{err}");
        assert!(s.list_keys(&Scope::Workspace).is_err());
        assert!(s.set(&Scope::Workspace, "K", "v").is_err());
        let st = s.status();
        assert!(!st.available);
        assert_eq!(st.mode, MODE_PRINCIPAL);
    }

    #[test]
    fn principal_status_is_writable_configured_and_never_locked() {
        let st = layered().status();
        assert!(st.available && st.writable && st.configured && !st.locked);
        assert_eq!(st.mode, MODE_PRINCIPAL);
        let lock = lock_state();
        assert!(lock.configured && !lock.locked, "agrees with status");
    }

    #[cfg(unix)]
    #[test]
    fn boot_constructs_a_principal_only_for_a_t1_child_with_its_own_data_dir() {
        use crate::executor::{ExecutorTier, PrincipalId};
        use principal::{BrokerKek, PrincipalKey};
        let kek = BrokerKek::from_bytes([42; 32]);
        let id = PrincipalId::new_v7();
        let key = || -> Result<Option<PrincipalKey>, String> { Ok(Some(kek.derive(id))) };
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join(id.to_string()).join("data");
        std::fs::create_dir_all(&data).unwrap();

        // T0 constructs no principal, whatever it was handed (G-PRINCIPAL §1).
        let s = DaemonSecrets::from_handoff(key(), ExecutorTier::T0, Some(&data));
        assert!(!s.has_principal());
        let s = DaemonSecrets::from_handoff(Err("bad".into()), ExecutorTier::T0, Some(&data));
        assert!(!s.has_principal());
        // T1 (a principal child) without a hand-off: fails closed, never the
        // operator defaults alone.
        let s = DaemonSecrets::from_handoff(Ok(None), ExecutorTier::T1, Some(&data));
        assert!(s
            .get(&Scope::Workspace, "K")
            .unwrap_err()
            .contains("without a secrets key"));
        assert!(!s.status().available);
        assert!(
            DaemonSecrets::from_handoff(Ok(None), ExecutorTier::T1, Some(&data)).has_principal()
        );

        // T1 with its own data dir: ready, and it stores.
        let s = DaemonSecrets::from_handoff(key(), ExecutorTier::T1, Some(&data));
        assert_eq!(s.status().mode, MODE_PRINCIPAL);
        assert!(s.status().available);
        s.set(&Scope::Workspace, "K", "v").unwrap();
        assert_eq!(s.get(&Scope::Workspace, "K").unwrap().as_deref(), Some("v"));
        assert!(data.join("secrets").join(principal::DEK_FILENAME).exists());

        // Another principal's data dir: refused, fails closed.
        let other = tmp
            .path()
            .join(PrincipalId::new_v7().to_string())
            .join("data");
        std::fs::create_dir_all(&other).unwrap();
        let s = DaemonSecrets::from_handoff(key(), ExecutorTier::T1, Some(&other));
        assert!(!s.status().available);
        assert!(s.get(&Scope::Workspace, "K").is_err());
        // A malformed hand-off on T1 fails closed too.
        let s = DaemonSecrets::from_handoff(Err("malformed".into()), ExecutorTier::T1, Some(&data));
        assert!(s.get(&Scope::Workspace, "K").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn t0_fails_closed_over_a_data_dir_that_holds_a_principal_store() {
        use crate::executor::ExecutorTier;
        let tmp = tempfile::tempdir().unwrap();
        assert!(!DaemonSecrets::from_boot(ExecutorTier::T0, Some(tmp.path())).has_principal());
        std::fs::create_dir_all(tmp.path().join("secrets")).unwrap();
        std::fs::write(tmp.path().join("secrets").join("dek.json"), "{}").unwrap();
        let s = DaemonSecrets::from_boot(ExecutorTier::T0, Some(tmp.path()));
        assert!(s
            .get(&Scope::Workspace, "K")
            .unwrap_err()
            .contains("only a T1"));
        assert!(!s.status().available);
    }
}
