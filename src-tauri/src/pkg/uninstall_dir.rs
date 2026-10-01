//! On-disk side of `Kernel::uninstall` for pkgs that live in `pkgs_dir()`.
//!
//! Registry installs (`pkg_install_from_registry`) and CLI drops (`ikenga add`)
//! unpack to `<app_data>/pkgs/<id>/`. Boot's `Kernel::install_from_pkgs_dir`
//! re-registers any such folder that `pkg_installed` doesn't track, so an
//! uninstall that only drops the row would see the pkg come back as `Local`
//! after the next restart. This module retires the folder instead.
//!
//! **Retire = rename to a dot-prefixed backup, not delete.** On Windows a
//! sidecar / MCP child that is still exiting after `unregister` can hold open
//! handles inside the folder; a `remove_dir_all` then fails half-way and can
//! leave a partially deleted pkg that still has a `manifest.json`. A
//! same-volume directory rename is atomic (all or nothing) and keeps the files
//! recoverable. Backups are named `.uninstalled-<id>-<unix_millis>`; every
//! dot-prefixed entry is skipped by boot discovery, by the daemon's
//! `pkg_index`, and by the health scan, and backups older than
//! [`BACKUP_RETENTION`] are pruned at boot by [`sweep`].
//!
//! If the rename still fails after a short retry, the fallback renames just
//! `manifest.json` to [`TOMBSTONE`] (the manifest is read-and-closed, never
//! held open). A folder without `manifest.json` is invisible to discovery, so
//! the pkg still does not resurrect; [`sweep`] finishes the move at next boot.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};

use super::source::InstallSource;

/// Prefix of the backup folder an uninstalled pkg is moved to.
pub(crate) const BACKUP_PREFIX: &str = ".uninstalled-";
/// What `manifest.json` is renamed to when the whole folder can't be moved.
pub(crate) const TOMBSTONE: &str = ".manifest.json.uninstalled";
/// How long a backup is kept before boot prunes it.
pub(crate) const BACKUP_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Prefix of an installer's unpack dir (`.staging-<id>`) and, with
/// [`TARBALL_SUFFIX`], of its downloaded tarball (`.staging-<id>.tgz`).
pub(crate) const STAGING_PREFIX: &str = ".staging-";
/// Prefix of the dir an installer moves the previous install to while it
/// promotes the new one (`.bak-<id>`).
pub(crate) const INSTALL_BACKUP_PREFIX: &str = ".bak-";
/// Extension of the downloaded registry tarball next to the staging dir.
pub(crate) const TARBALL_SUFFIX: &str = ".tgz";
/// Installer scratch younger than this is left alone by
/// [`sweep_install_scratch`]: `ikenga add` (the CLI) installs into the same
/// pkgs dir with the same names and can be mid-install while the shell boots.
pub(crate) const INSTALL_SCRATCH_MIN_AGE: Duration = Duration::from_secs(10 * 60);
/// Backoff between folder-rename attempts. Short on purpose: `pkg_uninstall`
/// is a sync Tauri command. Worst case ~350ms, and only when files are locked.
const RENAME_BACKOFF_MS: &[u64] = &[100, 250];

/// `std::fs::rename` with the same short retry [`retire`] uses: a Windows
/// handle that is just being released (a child that exited a moment ago, an
/// AV scan) clears within a few hundred ms. Returns the last error.
pub(crate) fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last_err = None;
    for attempt in 0..=RENAME_BACKOFF_MS.len() {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(RENAME_BACKOFF_MS[attempt - 1]));
        }
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("rename failed")))
}

/// What [`retire`] did with a pkg's install folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Retired {
    /// Not ours to touch: Builtin / Dev source, or the folder isn't a direct
    /// child of `pkgs_dir`.
    NotManaged,
    /// Managed path, but nothing on disk (already gone).
    Missing,
    /// Folder moved to this backup path.
    BackedUp(PathBuf),
    /// Folder couldn't be moved; `manifest.json` renamed to [`TOMBSTONE`]
    /// inside it. [`sweep`] retries the move at boot.
    Tombstoned,
}

/// The discovery predicate shared by `Kernel::install_from_pkgs_dir`: a
/// directory whose name isn't dot-prefixed (installer staging, `.bak-*`,
/// uninstall backups) and that holds a `manifest.json`.
pub(crate) fn is_discoverable(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let dotted = path
        .file_name()
        .and_then(|n| n.to_str())
        .map_or(true, |n| n.starts_with('.'));
    !dotted && path.join("manifest.json").exists()
}

/// True when `install_path` is a direct, non-dot child of `pkgs_dir`. The
/// parent is canonicalized (not the leaf, so a symlinked entry is judged by
/// where the link sits, and only the link is ever moved).
fn is_direct_child(pkgs_dir: &Path, install_path: &Path) -> bool {
    let Some(name) = install_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if name.starts_with('.') || name.is_empty() {
        return false;
    }
    let Some(parent) = install_path.parent() else {
        return false;
    };
    if parent == pkgs_dir {
        return true;
    }
    match (parent.canonicalize(), pkgs_dir.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn backup_path(pkgs_dir: &Path, name: &str, millis: u128) -> PathBuf {
    pkgs_dir.join(format!("{BACKUP_PREFIX}{name}-{millis}"))
}

/// Retire a just-uninstalled pkg's folder. Call after every registry has
/// unregistered the pkg (so supervised sidecars / MCP children were told to
/// stop). Errors only when neither the folder move nor the tombstone worked;
/// the caller logs it (the kernel side of the uninstall is already done).
pub(crate) fn retire(
    pkgs_dir: &Path,
    install_path: &Path,
    source: &InstallSource,
) -> Result<Retired> {
    if source.is_builtin() || source.is_dev() || !is_direct_child(pkgs_dir, install_path) {
        return Ok(Retired::NotManaged);
    }
    if std::fs::symlink_metadata(install_path).is_err() {
        return Ok(Retired::Missing);
    }
    let name = install_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("pkg")
        .to_string();

    let mut last_err = None;
    for attempt in 0..=RENAME_BACKOFF_MS.len() {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(RENAME_BACKOFF_MS[attempt - 1]));
        }
        let dest = backup_path(pkgs_dir, &name, now_millis());
        match std::fs::rename(install_path, &dest) {
            Ok(()) => return Ok(Retired::BackedUp(dest)),
            Err(e) => last_err = Some(e),
        }
    }
    let move_err = last_err.map(|e| e.to_string()).unwrap_or_default();

    let manifest = install_path.join("manifest.json");
    if !manifest.exists() {
        // Nothing discovery could pick up — already inert.
        return Ok(Retired::Tombstoned);
    }
    std::fs::rename(&manifest, install_path.join(TOMBSTONE)).map_err(|e| {
        anyhow!(
            "move {} to backup failed ({move_err}) and tombstoning its manifest failed too: {e}",
            install_path.display()
        )
    })?;
    log::warn!(
        "[pkg_kernel] could not move {} to a backup ({move_err}); manifest tombstoned, boot will finish the move",
        install_path.display()
    );
    Ok(Retired::Tombstoned)
}

/// Boot-time housekeeping, run before discovery:
/// - a folder carrying [`TOMBSTONE`] and no `manifest.json` is moved to a
///   backup now that nothing holds it (left as-is if still locked);
/// - a stale [`TOMBSTONE`] next to a real `manifest.json` (the pkg was
///   reinstalled in place) is deleted;
/// - backups older than `retention` are removed.
///
/// Best-effort: every failure is logged and skipped.
pub(crate) fn sweep(pkgs_dir: &Path, retention: Duration) {
    let Ok(entries) = std::fs::read_dir(pkgs_dir) else {
        return;
    };
    let now = now_millis();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
            continue;
        };
        if let Some(rest) = name.strip_prefix(BACKUP_PREFIX) {
            // Unparseable suffix → not ours to judge; leave it.
            let Some(ts) = rest
                .rsplit_once('-')
                .and_then(|(_, t)| t.parse::<u128>().ok())
            else {
                continue;
            };
            if now.saturating_sub(ts) > retention.as_millis() {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => log::info!("[pkg_kernel] pruned old uninstall backup {name}"),
                    Err(e) => log::warn!("[pkg_kernel] prune {} failed: {e}", path.display()),
                }
            }
            continue;
        }
        if name.starts_with('.') {
            continue;
        }
        let tomb = path.join(TOMBSTONE);
        if !tomb.exists() {
            continue;
        }
        if path.join("manifest.json").exists() {
            let _ = std::fs::remove_file(&tomb);
            continue;
        }
        let dest = backup_path(pkgs_dir, &name, now);
        match std::fs::rename(&path, &dest) {
            Ok(()) => log::info!(
                "[pkg_kernel] finished retiring uninstalled pkg dir {name} → {}",
                dest.display()
            ),
            Err(e) => log::warn!(
                "[pkg_kernel] retiring tombstoned {} still failing (stays inert): {e}",
                path.display()
            ),
        }
    }
}

/// Scratch paths a registry install of `pkg_id` uses under `pkgs_dir`:
/// `(staging_dir, tarball, backup_dir)` =
/// `(.staging-<id>, .staging-<id>.tgz, .bak-<id>)`. Every name carries the
/// full id. (Building the tarball as `staging_dir.with_extension("tgz")`
/// replaced the id's last dotted segment, so every `com.ikenga.*` install
/// shared one `.staging-com.ikenga.tgz`.) Same names as the CLI's
/// `ikenga add`.
pub(crate) fn install_scratch_paths(pkgs_dir: &Path, pkg_id: &str) -> (PathBuf, PathBuf, PathBuf) {
    (
        pkgs_dir.join(format!("{STAGING_PREFIX}{pkg_id}")),
        pkgs_dir.join(format!("{STAGING_PREFIX}{pkg_id}{TARBALL_SUFFIX}")),
        pkgs_dir.join(format!("{INSTALL_BACKUP_PREFIX}{pkg_id}")),
    )
}

/// Age of `path` by mtime; `None` when it can't be read. A future mtime
/// counts as age zero.
fn age(path: &Path) -> Option<Duration> {
    let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO),
    )
}

/// Boot-time reaping of installer scratch left by a crash / restart
/// mid-install (registry install in `commands::pkg` or the CLI's
/// `ikenga add`), which nothing else ever removes:
/// - `.staging-*` dirs and `.staging-*.tgz` files are removed (including
///   the legacy collided `.staging-com.ikenga.tgz`);
/// - `.bak-<id>` with no `<id>` beside it means the install died between
///   moving the old version aside and promoting the new one: the backup is
///   renamed back to `<id>` (restored);
/// - `.bak-<id>` with `<id>` present is a leftover of a promoted install:
///   removed.
///
/// Entries younger than `min_age` by mtime are skipped, and so is a
/// `.bak-<id>` while a young `.staging-<id>` / `.staging-<id>.tgz` exists:
/// the CLI writes the same names and may be installing while the shell
/// boots. (The shell's own installer can't race this: it runs from
/// `Kernel::boot` during Tauri setup, before `KernelState` is managed, so no
/// install command can reach the kernel yet.) `.uninstalled-*` backups and
/// pkg dirs are [`sweep`]'s business and untouched here. Best-effort: every
/// failure is logged and skipped.
pub(crate) fn sweep_install_scratch(pkgs_dir: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(pkgs_dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let is_young = |p: &Path| age(p).map_or(true, |a| a < min_age);

    for path in &paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(STAGING_PREFIX) || is_young(path) {
            continue;
        }
        if path.is_dir() {
            match std::fs::remove_dir_all(path) {
                Ok(()) => log::info!("[pkg_kernel] removed leftover install staging dir {name}"),
                Err(e) => log::warn!("[pkg_kernel] remove {} failed: {e}", path.display()),
            }
        } else if name.ends_with(TARBALL_SUFFIX) {
            match std::fs::remove_file(path) {
                Ok(()) => log::info!("[pkg_kernel] removed leftover install tarball {name}"),
                Err(e) => log::warn!("[pkg_kernel] remove {} failed: {e}", path.display()),
            }
        }
    }

    for path in &paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_prefix(INSTALL_BACKUP_PREFIX) else {
            continue;
        };
        if id.is_empty() || id.starts_with('.') || !path.is_dir() || is_young(path) {
            continue;
        }
        let (staging, tarball, _) = install_scratch_paths(pkgs_dir, id);
        if [&staging, &tarball]
            .iter()
            .any(|p| std::fs::symlink_metadata(p).is_ok() && is_young(p))
        {
            log::info!("[pkg_kernel] {name}: an install of `{id}` looks in flight — left as-is");
            continue;
        }
        let final_dir = pkgs_dir.join(id);
        if std::fs::symlink_metadata(&final_dir).is_ok() {
            match std::fs::remove_dir_all(path) {
                Ok(()) => log::info!("[pkg_kernel] removed leftover install backup {name}"),
                Err(e) => log::warn!("[pkg_kernel] remove {} failed: {e}", path.display()),
            }
        } else {
            match std::fs::rename(path, &final_dir) {
                Ok(()) => log::info!(
                    "[pkg_kernel] restored interrupted install: {name} → {id} (install died before promoting the new version)"
                ),
                Err(e) => log::warn!(
                    "[pkg_kernel] restore {} → {} failed: {e}",
                    path.display(),
                    final_dir.display()
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pkg(dir: &Path, id: &str) -> PathBuf {
        let p = dir.join(id);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("manifest.json"),
            format!(r#"{{"id":"{id}","name":"T","version":"1.0.0","ikenga_api":"1"}}"#),
        )
        .unwrap();
        std::fs::write(p.join("index.js"), "// payload").unwrap();
        p
    }

    /// Mirrors the loop head of `Kernel::install_from_pkgs_dir`: every entry
    /// boot discovery would try to register as `Local`.
    fn discovered(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_discoverable(p))
            .collect();
        v.sort();
        v
    }

    fn registry() -> InstallSource {
        InstallSource::Registry {
            url: "https://registry.test".into(),
            publisher_key: None,
        }
    }

    /// Bug repro: a registry install's folder that is left in place after its
    /// row is dropped is exactly what boot discovery re-registers.
    #[test]
    fn untouched_pkgs_dir_folder_is_rediscovered_at_boot() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = write_pkg(tmp.path(), "com.test.reg");
        assert_eq!(discovered(tmp.path()), vec![pkg]);
    }

    #[test]
    fn retire_backs_up_registry_pkg_and_discovery_skips_it() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        let pkg = write_pkg(pkgs, "com.test.reg");

        let out = retire(pkgs, &pkg, &registry()).unwrap();
        let Retired::BackedUp(backup) = out else {
            panic!("expected BackedUp, got {out:?}");
        };
        assert!(!pkg.exists(), "install folder is gone");
        assert_eq!(backup.parent(), Some(pkgs));
        let bname = backup.file_name().unwrap().to_str().unwrap();
        assert!(bname.starts_with(".uninstalled-com.test.reg-"), "{bname}");
        assert!(
            backup.join("manifest.json").exists(),
            "backup keeps the files"
        );
        assert!(
            discovered(pkgs).is_empty(),
            "boot discovery must not reinstall it"
        );
    }

    #[test]
    fn retire_cli_local_pkg_in_pkgs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = write_pkg(tmp.path(), "com.test.cli");
        let src = InstallSource::Local {
            path: pkg.display().to_string(),
        };
        assert!(matches!(
            retire(tmp.path(), &pkg, &src).unwrap(),
            Retired::BackedUp(_)
        ));
        assert!(discovered(tmp.path()).is_empty());
    }

    #[test]
    fn retire_leaves_local_and_dev_pkgs_outside_pkgs_dir_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path().join("pkgs");
        std::fs::create_dir_all(&pkgs).unwrap();
        let outside = tmp.path().join("workspace");
        let pkg = write_pkg(&outside, "com.test.side");

        for src in [
            InstallSource::Local {
                path: pkg.display().to_string(),
            },
            InstallSource::Dev {
                path: pkg.display().to_string(),
            },
            registry(),
        ] {
            assert_eq!(retire(&pkgs, &pkg, &src).unwrap(), Retired::NotManaged);
            assert!(pkg.join("manifest.json").exists());
            assert!(pkg.join("index.js").exists());
        }
        // A nested (non-direct-child) path under pkgs_dir is also left alone.
        let nested = write_pkg(&pkgs.join("group"), "com.test.nested");
        assert_eq!(
            retire(&pkgs, &nested, &registry()).unwrap(),
            Retired::NotManaged
        );
        assert!(nested.join("manifest.json").exists());
    }

    #[test]
    fn retire_never_touches_dev_or_builtin_even_inside_pkgs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = write_pkg(tmp.path(), "com.test.devin");
        let dev = InstallSource::Dev {
            path: pkg.display().to_string(),
        };
        assert_eq!(retire(tmp.path(), &pkg, &dev).unwrap(), Retired::NotManaged);
        assert_eq!(
            retire(tmp.path(), &pkg, &InstallSource::Builtin).unwrap(),
            Retired::NotManaged
        );
        assert!(pkg.join("manifest.json").exists());
    }

    #[test]
    fn retire_missing_folder_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("com.test.gone");
        assert_eq!(
            retire(tmp.path(), &gone, &registry()).unwrap(),
            Retired::Missing
        );
    }

    /// The locked-folder fallback: a tombstoned folder is inert to discovery,
    /// and the boot sweep finishes moving it once it's free.
    #[test]
    fn tombstoned_folder_is_skipped_then_swept_to_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        let pkg = write_pkg(pkgs, "com.test.locked");
        std::fs::rename(pkg.join("manifest.json"), pkg.join(TOMBSTONE)).unwrap();
        assert!(
            discovered(pkgs).is_empty(),
            "tombstoned dir must not be rediscovered"
        );

        sweep(pkgs, BACKUP_RETENTION);
        assert!(!pkg.exists());
        let backups: Vec<_> = std::fs::read_dir(pkgs)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(BACKUP_PREFIX))
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(discovered(pkgs).is_empty());
    }

    #[test]
    fn sweep_drops_stale_tombstone_when_pkg_reinstalled_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = write_pkg(tmp.path(), "com.test.again");
        std::fs::write(pkg.join(TOMBSTONE), "{}").unwrap();
        sweep(tmp.path(), BACKUP_RETENTION);
        assert!(pkg.join("manifest.json").exists());
        assert!(!pkg.join(TOMBSTONE).exists());
        assert_eq!(discovered(tmp.path()), vec![pkg]);
    }

    #[test]
    fn sweep_prunes_only_expired_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        let old = pkgs.join(format!("{BACKUP_PREFIX}com.test.a-1000"));
        let fresh = backup_path(pkgs, "com.test.b", now_millis());
        let odd = pkgs.join(format!("{BACKUP_PREFIX}not-a-timestamp"));
        let other_dot = pkgs.join(".bak-com.test.c");
        for d in [&old, &fresh, &odd, &other_dot] {
            std::fs::create_dir_all(d).unwrap();
        }
        sweep(pkgs, BACKUP_RETENTION);
        assert!(!old.exists(), "expired backup pruned");
        assert!(fresh.exists(), "recent backup kept for recovery");
        assert!(odd.exists(), "unrecognised name left alone");
        assert!(
            other_dot.exists(),
            "installer's own dot dirs are sweep_install_scratch's job, not sweep's"
        );
    }

    #[test]
    fn install_scratch_paths_carry_the_full_id() {
        let pkgs = Path::new("pkgs");
        let (s_studio, t_studio, b_studio) = install_scratch_paths(pkgs, "com.ikenga.studio");
        let (s_git, t_git, b_git) = install_scratch_paths(pkgs, "com.ikenga.git");
        assert_eq!(s_studio, pkgs.join(".staging-com.ikenga.studio"));
        assert_eq!(t_studio, pkgs.join(".staging-com.ikenga.studio.tgz"));
        assert_eq!(b_studio, pkgs.join(".bak-com.ikenga.studio"));
        assert_eq!(t_git, pkgs.join(".staging-com.ikenga.git.tgz"));
        assert_ne!(t_studio, t_git, "no two pkgs share a tarball path");
        assert_ne!(s_studio, s_git);
        assert_ne!(b_studio, b_git);
        for p in [&s_studio, &t_studio, &b_studio] {
            assert!(p.to_str().unwrap().contains("com.ikenga.studio"), "{p:?}");
        }
        assert_ne!(t_studio, pkgs.join(".staging-com.ikenga.tgz"));
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The live leftovers from an interrupted studio update (stray collided
    /// tarball, `.bak-` with the final dir still present) plus a fresh
    /// staging dir / tarball: all scratch goes, everything else stays.
    #[test]
    fn sweep_install_scratch_removes_staging_and_promoted_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        let studio = write_pkg(pkgs, "com.ikenga.studio");
        write_pkg(pkgs, ".bak-com.ikenga.studio");
        write_pkg(pkgs, ".staging-com.ikenga.git");
        std::fs::write(pkgs.join(".staging-com.ikenga.git.tgz"), b"tgz").unwrap();
        std::fs::write(pkgs.join(".staging-com.ikenga.tgz"), b"legacy").unwrap();
        let uninstalled = pkgs.join(format!("{BACKUP_PREFIX}com.test.a-{}", now_millis()));
        std::fs::create_dir_all(&uninstalled).unwrap();
        let other = write_pkg(pkgs, "com.test.other");

        sweep_install_scratch(pkgs, Duration::ZERO);

        let mut want = vec![
            uninstalled
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            "com.ikenga.studio".to_string(),
            "com.test.other".to_string(),
        ];
        want.sort();
        assert_eq!(names(pkgs), want);
        assert!(studio.join("manifest.json").exists(), "final install kept");
        assert!(other.join("index.js").exists(), "normal pkg dir untouched");
    }

    /// Install died between `<id>` → `.bak-<id>` and promoting staging: the
    /// previous version is put back so the pkg is not lost.
    #[test]
    fn sweep_install_scratch_restores_backup_when_final_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        write_pkg(pkgs, ".bak-com.ikenga.studio");
        write_pkg(pkgs, ".staging-com.ikenga.studio");
        std::fs::write(pkgs.join(".staging-com.ikenga.studio.tgz"), b"tgz").unwrap();

        sweep_install_scratch(pkgs, Duration::ZERO);

        assert_eq!(names(pkgs), vec!["com.ikenga.studio".to_string()]);
        let restored = pkgs.join("com.ikenga.studio");
        assert!(restored.join("manifest.json").exists());
        assert_eq!(discovered(pkgs), vec![restored]);
    }

    /// Scratch younger than the min age may belong to an in-flight CLI
    /// install: nothing is touched, not even a backup whose final is missing.
    #[test]
    fn sweep_install_scratch_skips_young_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        write_pkg(pkgs, ".bak-com.ikenga.studio");
        write_pkg(pkgs, ".staging-com.ikenga.studio");
        std::fs::write(pkgs.join(".staging-com.ikenga.studio.tgz"), b"tgz").unwrap();
        let before = names(pkgs);

        sweep_install_scratch(pkgs, INSTALL_SCRATCH_MIN_AGE);

        assert_eq!(names(pkgs), before);
    }

    #[test]
    fn sweep_install_scratch_clean_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let pkgs = tmp.path();
        let a = write_pkg(pkgs, "com.test.a");
        let tomb = write_pkg(pkgs, "com.test.tomb");
        std::fs::rename(tomb.join("manifest.json"), tomb.join(TOMBSTONE)).unwrap();
        let before = names(pkgs);

        sweep_install_scratch(pkgs, Duration::ZERO);
        sweep_install_scratch(&pkgs.join("absent"), Duration::ZERO);

        assert_eq!(names(pkgs), before);
        assert!(a.join("manifest.json").exists());
        assert!(tomb.join(TOMBSTONE).exists());
    }
}
