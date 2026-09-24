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
    pub disk_free_gb: u64,
    pub app_data_dir: String,
    pub app_data_writable: bool,
    pub secrets_store_ready: bool,
    pub vault_key_present: bool,
    pub claude_projects_dir_present: bool,
    pub checks: Vec<SystemCheck>,
}

/// Public entry point. `app_data_dir` is the value Tauri's
/// `app.path().app_data_dir()` resolves to — passed in so the command
/// handler doesn't have to teach this module about `AppHandle`.
pub fn build_report(app_data_dir: PathBuf, backend_accessible: bool) -> SystemReport {
    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let app_data_writable = probe_writable(&app_data_dir);
    let secrets_store_ready = backend_accessible && secrets_index_valid(&app_data_dir);
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

    checks.push(SystemCheck {
        id: "secrets_store".into(),
        level: if secrets_store_ready {
            CheckLevel::Pass
        } else {
            CheckLevel::Warn
        },
        message: if secrets_store_ready {
            "Secret index and platform keychain are available".into()
        } else {
            "Secret index or platform keychain is unavailable".into()
        },
        fix_hint: if secrets_store_ready {
            None
        } else {
            Some("Unlock the platform keychain and retry the secrets status probe.".into())
        },
    });

    checks.push(SystemCheck {
        id: "disk_free".into(),
        level: if disk_free_gb >= 5 {
            CheckLevel::Pass
        } else if disk_free_gb >= 1 {
            CheckLevel::Warn
        } else {
            CheckLevel::Fail
        },
        message: format!("{disk_free_gb} GB free on the app-data volume"),
        fix_hint: if disk_free_gb >= 5 {
            None
        } else {
            Some("Free up disk before installing packages. We recommend at least 5 GB.".into())
        },
    });

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

fn secrets_index_valid(app_data_dir: &Path) -> bool {
    SecretIndex::load(app_data_dir.join(INDEX_FILENAME)).is_ok()
}

fn claude_projects_path() -> Option<PathBuf> {
    Some(
        crate::platform::home_dir()?
            .join(".claude")
            .join("projects"),
    )
}

fn disk_free_gb_for(target: &Path) -> u64 {
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

    let mut best: Option<(usize, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if normalized.starts_with(mount) {
            let len = mount.as_os_str().len();
            let bytes = disk.available_space();
            match best {
                Some((cur_len, _)) if cur_len >= len => {}
                _ => best = Some((len, bytes)),
            }
        }
    }
    best.map(|(_, bytes)| bytes / 1_073_741_824).unwrap_or(0)
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
        let report = build_report(tmp.clone(), false);
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

    #[test]
    fn secrets_health_requires_valid_index_and_backend_access() {
        let tmp = std::env::temp_dir().join("ikenga-detect-secrets-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(INDEX_FILENAME), b"not-json").unwrap();
        assert!(!build_report(tmp.clone(), true).secrets_store_ready);
        std::fs::write(tmp.join(INDEX_FILENAME), b"[]").unwrap();
        assert!(!build_report(tmp.clone(), false).secrets_store_ready);
        assert!(build_report(tmp.clone(), true).secrets_store_ready);
        let _ = std::fs::remove_dir_all(tmp);
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
        let report = build_report(tmp.clone(), false);
        assert!(matches!(
            report.os.as_str(),
            "macos" | "linux" | "windows" | "freebsd" | "openbsd" | "netbsd" | "dragonfly"
        ));
        assert!(!report.arch.is_empty());
        let _ = std::fs::remove_dir_all(tmp);
    }
}
