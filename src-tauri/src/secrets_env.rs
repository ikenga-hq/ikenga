//! Environment-backed secret reads for the headless daemon.
//!
//! Lives outside `commands/` for the same reason `path_allow` does: the
//! daemon needs it and `commands/` is desktop-only, built around
//! `#[tauri::command]` and `AppHandle` (whose default type parameter IS
//! `Wry`, which a `--no-default-features` build does not link).
//!
//! # There is no vault here, and that is the decision — not an omission
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
//! There are none. [`WRITE_REFUSAL`] is the operator-facing explanation the
//! RPC layer returns for every write command, so a reader hitting it can tell
//! *decided* from *unfinished*.

use serde::Serialize;

/// Prefix the operator uses to opt a credential into the remote namespace.
pub const ENV_PREFIX: &str = "IKENGA_SECRET_";

/// `VaultStatus::mode` for the daemon's env-backed store.
pub const MODE_ENV: &str = "env";

/// `VaultStatus::mode` for the desktop app's Stronghold vault.
pub const MODE_STRONGHOLD: &str = "stronghold";

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
    "project- and pkg-scoped secrets exist only in the desktop app's Stronghold vault."
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
/// `("stronghold", true)`. Dropping either of the original three breaks every
/// connector silently, so this struct is a superset and never a rename.
#[derive(Debug, Serialize)]
pub struct VaultStatus {
    pub available: bool,
    pub keychain_backend: String,
    pub error: Option<String>,
    pub mode: String,
    pub writable: bool,
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
        for field in ["available", "keychain_backend", "error", "mode", "writable"] {
            assert!(obj.contains_key(field), "missing field {field}");
        }
        assert_eq!(obj.len(), 5);
    }
}
