//! `detect_system` — the high-level "is this machine ready for Ikenga?"
//! check the first-run wizard renders on its System step.
//!
//! Designed to be cheap: no subprocesses, no network. Disk-free uses
//! `sysinfo`'s `Disks` API (POSIX `statvfs` / Windows `GetDiskFreeSpaceEx`
//! under the hood). Writability is verified by a hidden tempfile so we
//! catch read-only mounts / SELinux-denied dirs the user couldn't ssh
//! their way out of.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sysinfo::Disks;

use crate::secrets::index::{SecretIndex, INDEX_FILENAME};
use crate::secrets::store::{StoreError, StoreErrorKind};

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CheckLevel {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug, Serialize)]
pub struct SystemCheck {
    pub id: String,
    pub level: CheckLevel,
    pub message: String,
    pub fix_hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SystemReport {
    pub os: String,
    pub arch: String,
    /// `None` when no mounted volume matched the app-data dir — the free
    /// space is unknown, not zero.
    pub disk_free_gb: Option<u64>,
    pub app_data_dir: String,
    pub app_data_writable: bool,
    pub secrets_store_ready: bool,
    pub vault_key_present: bool,
    pub claude_projects_dir_present: bool,
    pub checks: Vec<SystemCheck>,
}

/// What the secrets-backend probe found. Each failure names a different
/// cause and needs a different fix, so they stay apart all the way to the
/// check's message and hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretsBackend {
    Ready,
    /// Ikenga's own vault (passphrase lock) is locked.
    VaultLocked,
    /// The vault has no passphrase set up yet, so there is nothing to unlock.
    VaultNotConfigured,
    /// The platform keychain is there but refused or could not be reached
    /// (locked login keychain, Secret Service not running, ...).
    KeychainUnavailable(String),
    /// No keychain backend exists for this platform/build, or the secrets
    /// service isn't running in this process.
    NoBackend(String),
    /// Ikenga disabled the secret store at startup (configuration or
    /// migration failed); the keychain itself was never the problem.
    StoreDisabled(String),
    /// The probe ran and failed for a reason none of the above covers.
    Failed(String),
}

/// Prefix `UnavailableSecretStore` puts on every error: the store was
/// swapped out at startup, not refused by the keychain.
const STORE_DISABLED_PREFIX: &str = "secret store unavailable: ";

impl SecretsBackend {
    pub fn from_probe(result: Result<(), StoreError>) -> Self {
        let error = match result {
            Ok(()) => return Self::Ready,
            Err(error) => error,
        };
        let message = error.to_string();
        if let Some(reason) = message.strip_prefix(STORE_DISABLED_PREFIX) {
            return Self::StoreDisabled(reason.to_string());
        }
        if message.contains("unsupported platform keychain") {
            return Self::NoBackend(message);
        }
        match error.kind() {
            StoreErrorKind::Locked if error.is_not_configured() => Self::VaultNotConfigured,
            StoreErrorKind::Locked => Self::VaultLocked,
            StoreErrorKind::Unavailable => Self::KeychainUnavailable(message),
            StoreErrorKind::Invalid | StoreErrorKind::Unknown => Self::Failed(message),
        }
    }
}

/// Public entry point. `app_data_dir` is the value Tauri's
/// `app.path().app_data_dir()` resolves to — passed in so the command
/// handler doesn't have to teach this module about `AppHandle`.
pub fn build_report(app_data_dir: PathBuf, backend: SecretsBackend) -> SystemReport {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let app_data_writable = probe_writable(&app_data_dir);
    let index = SecretIndex::load(app_data_dir.join(INDEX_FILENAME)).map(|_| ());
    let secrets_store_ready = backend == SecretsBackend::Ready && index.is_ok();
    let claude_projects_dir = claude_projects_path();
    let claude_projects_dir_present = claude_projects_dir
        .as_ref()
        .map(|p| p.is_dir())
        .unwrap_or(false);
    let disk_free_gb = disk_free_gb_for(&app_data_dir);

    let mut checks = Vec::new();

    checks.push(SystemCheck {
        id: "app_data_dir".into(),
        level: if app_data_writable {
            CheckLevel::Pass
        } else {
            CheckLevel::Fail
        },
        message: if app_data_writable {
            format!("App data dir is writable ({})", app_data_dir.display())
        } else {
            format!("App data dir is not writable: {}", app_data_dir.display())
        },
        fix_hint: if app_data_writable {
            None
        } else {
            Some(
                "Check filesystem permissions on the app-data directory. \
                 Ikenga writes SQLite + secret metadata + logs here."
                    .into(),
            )
        },
    });

    checks.push(secrets_check(&index, &backend));

    checks.push(disk_free_check(disk_free_gb, &app_data_dir));

    checks.push(SystemCheck {
        id: "claude_projects".into(),
        level: if claude_projects_dir_present {
            CheckLevel::Pass
        } else {
            CheckLevel::Warn
        },
        message: if claude_projects_dir_present {
            format!(
                "Claude projects dir present: {}",
                claude_projects_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            )
        } else {
            "~/.claude/projects/ not found".into()
        },
        fix_hint: if claude_projects_dir_present {
            None
        } else {
            Some(
                "No previous Claude Code sessions detected. That's fine — \
                 we'll create the dir when you run a session for the first time."
                    .into(),
            )
        },
    });

    SystemReport {
        os,
        arch,
        disk_free_gb,
        app_data_dir: app_data_dir.display().to_string(),
        app_data_writable,
        secrets_store_ready,
        vault_key_present: secrets_store_ready,
        claude_projects_dir_present,
        checks,
    }
}

fn probe_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".ikenga-writable-probe");
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            // Best-effort cleanup; if removal fails we still consider the
            // dir writable (probably an antivirus race, not a permission
            // problem).
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn secrets_check(index: &Result<(), String>, backend: &SecretsBackend) -> SystemCheck {
    let warn = |message: String, fix_hint: &str| SystemCheck {
        id: "secrets_store".into(),
        level: CheckLevel::Warn,
        message,
        fix_hint: Some(fix_hint.into()),
    };
    // A corrupt index is reported first: the backend probe loads the same
    // index, so its failure would otherwise be blamed on the keychain.
    if let Err(error) = index {
        return warn(
            format!("Secret index is unreadable: {error}"),
            "The keychain is not the problem. The secrets index file in the app-data \
             directory is damaged; restore it from a backup, or move it aside and \
             re-add your secrets.",
        );
    }
    match backend {
        SecretsBackend::Ready => SystemCheck {
            id: "secrets_store".into(),
            level: CheckLevel::Pass,
            message: "Secret index and platform keychain are available".into(),
            fix_hint: None,
        },
        SecretsBackend::VaultLocked => warn(
            "Ikenga's secret vault is locked".into(),
            "Unlock the vault with your Ikenga passphrase in Settings › Secrets.",
        ),
        SecretsBackend::VaultNotConfigured => warn(
            "Ikenga's secret vault has no passphrase set up yet".into(),
            "Set a vault passphrase in Settings › Secrets before saving secrets.",
        ),
        SecretsBackend::KeychainUnavailable(error) => warn(
            format!("Platform keychain is locked or unreachable: {error}"),
            "Unlock the platform keychain (on Linux, make sure a Secret Service \
             provider such as GNOME Keyring is running) and retry.",
        ),
        SecretsBackend::NoBackend(error) => warn(
            format!("No platform keychain backend is available: {error}"),
            "Ikenga stores secrets in the OS keychain, and none is available here. \
             Secrets can't be saved on this system until one is.",
        ),
        SecretsBackend::StoreDisabled(reason) => warn(
            format!("Ikenga disabled the secret store at startup: {reason}"),
            "The keychain is not the problem. Check the app log for the startup \
             error, then restart Ikenga.",
        ),
        SecretsBackend::Failed(error) => warn(
            format!("Couldn't verify the secret store: {error}"),
            "Retry the check. If it keeps failing, the app log has the details.",
        ),
    }
}

fn disk_free_check(disk_free_gb: Option<u64>, app_data_dir: &Path) -> SystemCheck {
    let Some(gb) = disk_free_gb else {
        // No mounted volume matched, so free space is unknown, not zero;
        // that must not fail the preflight.
        return SystemCheck {
            id: "disk_free".into(),
            level: CheckLevel::Warn,
            message: format!(
                "Couldn't determine free space: no mounted volume matched {}",
                app_data_dir.display()
            ),
            fix_hint: Some(
                "This doesn't block setup. Make sure the app-data volume has at least \
                 5 GB free before installing packages."
                    .into(),
            ),
        };
    };
    SystemCheck {
        id: "disk_free".into(),
        level: if gb >= 5 {
            CheckLevel::Pass
        } else if gb >= 1 {
            CheckLevel::Warn
        } else {
            CheckLevel::Fail
        },
        message: format!("{gb} GB free on the app-data volume"),
        fix_hint: if gb >= 5 {
            None
        } else {
            Some("Free up disk before installing packages. We recommend at least 5 GB.".into())
        },
    }
}

fn claude_projects_path() -> Option<PathBuf> {
    Some(
        crate::platform::home_dir()?
            .join(".claude")
            .join("projects"),
    )
}

fn disk_free_gb_for(target: &Path) -> Option<u64> {
    // `Disks::new_with_refreshed_list()` enumerates mounted disks. We want
    // the longest mount-point prefix-match for `target` — that's the volume
    // the file would live on.
    let disks = Disks::new_with_refreshed_list();
    let canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    // Windows `canonicalize` always yields the verbatim form `\\?\C:\…`,
    // which never `starts_with("C:\")` returned by `sysinfo`. Strip the
    // prefix before prefix-matching so disk_free isn't always 0 GB.
    let normalized = strip_windows_verbatim(&canonical);
    best_mount_free_gb(
        &normalized,
        disks
            .list()
            .iter()
            .map(|disk| (disk.mount_point(), disk.available_space())),
    )
}

/// Free GB on the longest mount point that prefixes `normalized`, or `None`
/// when no mount matches.
fn best_mount_free_gb<'a>(
    normalized: &Path,
    mounts: impl IntoIterator<Item = (&'a Path, u64)>,
) -> Option<u64> {
    let mut best: Option<(usize, u64)> = None;
    for (mount, bytes) in mounts {
        if normalized.starts_with(mount) {
            let len = mount.as_os_str().len();
            match best {
                Some((cur_len, _)) if cur_len >= len => {}
                _ => best = Some((len, bytes)),
            }
        }
    }
    best.map(|(_, bytes)| bytes / 1_073_741_824)
}

#[cfg(windows)]
fn strip_windows_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

#[cfg(not(windows))]
fn strip_windows_verbatim(p: &Path) -> PathBuf {
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_has_expected_check_ids() {
        let tmp = std::env::temp_dir().join("ikenga-detect-test");
        let report = build_report(tmp.clone(), SecretsBackend::Failed("test".into()));
        let ids: std::collections::HashSet<_> =
            report.checks.iter().map(|c| c.id.as_str()).collect();
        for id in [
            "app_data_dir",
            "secrets_store",
            "disk_free",
            "claude_projects",
        ] {
            assert!(ids.contains(id), "missing check {id}");
        }
        // Cleanup probe artifact (build_report wrote one).
        let _ = std::fs::remove_dir_all(tmp);
    }

    fn secrets_row(report: &SystemReport) -> &SystemCheck {
        report
            .checks
            .iter()
            .find(|c| c.id == "secrets_store")
            .expect("secrets_store check")
    }

    #[test]
    fn secrets_health_requires_valid_index_and_backend_access() {
        let tmp = std::env::temp_dir().join("ikenga-detect-secrets-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(INDEX_FILENAME), b"not-json").unwrap();
        assert!(!build_report(tmp.clone(), SecretsBackend::Ready).secrets_store_ready);
        std::fs::write(tmp.join(INDEX_FILENAME), b"[]").unwrap();
        assert!(
            !build_report(tmp.clone(), SecretsBackend::KeychainUnavailable("x".into()))
                .secrets_store_ready
        );
        assert!(build_report(tmp.clone(), SecretsBackend::Ready).secrets_store_ready);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn corrupt_index_is_not_blamed_on_the_keychain() {
        let tmp = std::env::temp_dir().join("ikenga-detect-secrets-corrupt-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(INDEX_FILENAME), b"not-json").unwrap();
        // The backend probe loads the same index, so it fails too; the row
        // must still name the index, not the keychain.
        let report = build_report(tmp.clone(), SecretsBackend::Failed("parse".into()));
        let row = secrets_row(&report);
        assert_eq!(row.level, CheckLevel::Warn);
        assert!(
            row.message.contains("Secret index is unreadable"),
            "{}",
            row.message
        );
        let hint = row.fix_hint.as_deref().unwrap();
        assert!(!hint.starts_with("Unlock"), "{hint}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn each_backend_failure_gets_its_own_message_and_hint() {
        let index = Ok(());
        let locked = secrets_check(
            &index,
            &SecretsBackend::KeychainUnavailable("storage locked".into()),
        );
        assert!(locked.message.contains("keychain is locked or unreachable"));
        assert!(locked
            .fix_hint
            .as_deref()
            .unwrap()
            .contains("Unlock the platform keychain"));

        let vault = secrets_check(&index, &SecretsBackend::VaultLocked);
        assert!(vault.message.contains("vault is locked"));
        assert!(vault.fix_hint.as_deref().unwrap().contains("passphrase"));

        // No passphrase yet: don't say "locked" or tell them to unlock.
        let unset = secrets_check(&index, &SecretsBackend::VaultNotConfigured);
        assert!(
            unset.message.contains("no passphrase set up"),
            "{}",
            unset.message
        );
        assert!(!unset.message.contains("locked"));
        let unset_hint = unset.fix_hint.as_deref().unwrap();
        assert!(
            unset_hint.contains("Set a vault passphrase"),
            "{unset_hint}"
        );
        assert!(!unset_hint.contains("Unlock"));

        let none = secrets_check(&index, &SecretsBackend::NoBackend("unsupported".into()));
        assert!(none.message.contains("No platform keychain backend"));
        assert!(!none.fix_hint.as_deref().unwrap().contains("Unlock"));

        let disabled = secrets_check(&index, &SecretsBackend::StoreDisabled("migration".into()));
        assert!(disabled
            .message
            .contains("disabled the secret store at startup: migration"));
        assert!(!disabled.fix_hint.as_deref().unwrap().contains("Unlock"));

        let failed = secrets_check(&index, &SecretsBackend::Failed("boom".into()));
        assert!(failed
            .message
            .contains("Couldn't verify the secret store: boom"));
        assert!(!failed.fix_hint.as_deref().unwrap().contains("Unlock"));

        let ready = secrets_check(&index, &SecretsBackend::Ready);
        assert_eq!(ready.level, CheckLevel::Pass);
        assert!(ready.fix_hint.is_none());
    }

    #[test]
    fn probe_errors_map_to_distinct_backend_states() {
        assert_eq!(SecretsBackend::from_probe(Ok(())), SecretsBackend::Ready);
        assert_eq!(
            SecretsBackend::from_probe(Err(StoreError::locked())),
            SecretsBackend::VaultLocked
        );
        assert_eq!(
            SecretsBackend::from_probe(Err(StoreError::from(
                crate::secrets::unlock::UnlockError::NotConfigured
            ))),
            SecretsBackend::VaultNotConfigured
        );
        assert!(matches!(
            SecretsBackend::from_probe(Err(StoreError::unavailable(
                "platform keychain storage is unavailable or locked"
            ))),
            SecretsBackend::KeychainUnavailable(_)
        ));
        assert!(matches!(
            SecretsBackend::from_probe(Err(StoreError::unknown("unsupported platform keychain"))),
            SecretsBackend::NoBackend(_)
        ));
        let disabled = crate::secrets::store::UnavailableSecretStore::new("migration failed");
        let err = crate::secrets::store::SecretsStore::probe(&disabled).unwrap_err();
        assert_eq!(
            SecretsBackend::from_probe(Err(err)),
            SecretsBackend::StoreDisabled("migration failed".into())
        );
        assert!(matches!(
            SecretsBackend::from_probe(Err(StoreError::uncommitted("read mismatch"))),
            SecretsBackend::Failed(_)
        ));
    }

    #[test]
    fn unknown_disk_free_serializes_as_null() {
        let tmp = std::env::temp_dir().join(format!("ikenga-sysreport-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut report = build_report(tmp.clone(), SecretsBackend::Ready);
        report.disk_free_gb = None;
        let json = serde_json::to_value(&report).unwrap();
        assert!(json["disk_free_gb"].is_null(), "{json}");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn unmatched_volume_is_unknown_not_zero() {
        let target = Path::new("/srv/app-data");
        let other = Path::new("/mnt/elsewhere");
        assert_eq!(best_mount_free_gb(target, [(other, 10 << 30)]), None);
        assert_eq!(best_mount_free_gb(target, std::iter::empty()), None);

        let row = disk_free_check(None, target);
        assert_eq!(row.level, CheckLevel::Warn);
        assert!(
            row.message.contains("Couldn't determine free space"),
            "{}",
            row.message
        );
        assert!(!row.message.contains("0 GB"));
    }

    #[test]
    fn longest_matching_mount_wins_and_real_zero_still_fails() {
        let target = Path::new("/srv/app-data");
        let root = Path::new("/");
        let srv = Path::new("/srv");
        assert_eq!(
            best_mount_free_gb(target, [(root, 100 << 30), (srv, 2 << 30)]),
            Some(2)
        );
        assert_eq!(disk_free_check(Some(0), target).level, CheckLevel::Fail);
        assert_eq!(disk_free_check(Some(2), target).level, CheckLevel::Warn);
        assert_eq!(disk_free_check(Some(50), target).level, CheckLevel::Pass);
    }

    #[test]
    fn writable_probe_recognises_tempdir() {
        let tmp = std::env::temp_dir().join("ikenga-detect-write-test");
        assert!(probe_writable(&tmp));
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn os_arch_reflect_target() {
        let tmp = std::env::temp_dir().join("ikenga-detect-os-test");
        let report = build_report(tmp.clone(), SecretsBackend::Failed("test".into()));
        assert!(matches!(
            report.os.as_str(),
            "macos" | "linux" | "windows" | "freebsd" | "openbsd" | "netbsd" | "dragonfly"
        ));
        assert!(!report.arch.is_empty());
        let _ = std::fs::remove_dir_all(tmp);
    }
}
