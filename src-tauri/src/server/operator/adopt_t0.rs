//! `ikenga-server accounts adopt-t0 --from <old-data-dir> --home <old-home>
//! <username>` (G-PRINCIPAL §11.2, G-ACCESS R-10): move a T0 install into a
//! T1 principal.
//!
//! The operator stops the T0 daemon first; this refuses while the pid in
//! `<old>/daemon.json` is alive. Two shapes (OD-14):
//!
//! * **Adopt** — the account was created with `accounts create <name>
//!   --adopt-unix-user <t0-user>` (a VPS install that ran as a non-root
//!   user). Its home is the T0 user's own home, so nothing in it moves and
//!   paths under it stay valid. The data dir moves into
//!   `principals/<id>/data/` by **rename** when it is on the same filesystem
//!   and not a mount point ([`Mode::AdoptRename`]), else by copy + verify
//!   ([`Mode::AdoptCopy`]).
//! * **Copy** — root/Docker installs (root can't be adopted, I-1). The
//!   account is a fresh, allocated principal (created by this command when
//!   it doesn't exist yet). The data dir is copied + verified into
//!   `principals/<id>/data/`, the engine/app dot-dirs of the old home
//!   ([`HOME_ENTRIES`]) are copied into `principals/<id>/home/`, and
//!   `fs_roots.json` entries under the old home are rewritten onto the new
//!   one ([`Mode::Copy`]).
//!
//! Either way:
//!
//! * the old dir is kept as `<old>.t0-migrated-<ts>`, root-owned and
//!   read-only. Under rename only what never moves is left in it;
//! * **R-10:** `access.db`, `access.db-wal` and `access.db-shm` (the T0
//!   access store: device hashes and the audit chain) go to that archive and
//!   are **never** carried into the principal-writable data dir. When the
//!   archive has to be a copy (the old dir is a mount point and can't be
//!   renamed), they are deleted from the source once the copy verifies. The
//!   old `daemon.json` (it carries the T0 bearer token) is archived the same
//!   way;
//! * the principal's dir is held root-owned while the migration runs, so no
//!   process of that uid can reach into it half-way;
//! * everything moved in is chowned to the principal and stripped of
//!   group/other bits, then checked against **I-9**;
//! * a migration report lists what moved, what was rewritten, and every
//!   `ikenga.db` column that still holds a path under the old home or old
//!   data dir. Those are **not** rewritten (§11.2).
//!
//! Chi runs that were live under T0 are reconciled as `Unverified` by the
//! existing sweep: the pid probe now runs as a different uid (§11.2).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Row};

use super::accounts::Account;
use super::{OperatorRoot, Ownership, T0_MARKERS};

/// The T0 access store (G-ACCESS §2.5). R-10: archived, never migrated.
pub const ACCESS_STORE_FILES: [&str; 3] = ["access.db", "access.db-wal", "access.db-shm"];

/// Top-level entries of the old data dir that go to the archive and never
/// into the principal's data dir: the access store (R-10) and the T0
/// discovery file, which carries the old bearer token.
const ARCHIVE_ONLY: [&str; 4] = ["access.db", "access.db-wal", "access.db-shm", "daemon.json"];

/// What a copy migration takes from the old home (§11.2), relative to it.
/// `.claude.json` sits beside `.claude/` and holds the Claude Code login, so
/// it travels with it.
pub const HOME_ENTRIES: &[&str] = &[
    ".ikenga",
    ".local/share/app.ikenga",
    ".agent-ops",
    ".atelier",
    ".claude",
    ".claude.json",
    ".codex",
    ".gemini",
];

/// The report written into the archive.
pub const REPORT_FILE: &str = "MIGRATION-REPORT.json";

/// How the data dir is moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Adopted Unix user, same filesystem: one `rename(2)` of the data dir.
    AdoptRename,
    /// Adopted Unix user, but the data dir can't be renamed into place
    /// (another filesystem, or a mount point): copy + verify.
    AdoptCopy,
    /// Fresh principal (root/Docker T0): copy + verify the data dir and the
    /// home dot-dirs; rewrite `fs_roots.json`.
    Copy,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::AdoptRename => "adopt (rename)",
            Mode::AdoptCopy => "adopt (copy)",
            Mode::Copy => "copy into a fresh principal",
        }
    }
}

/// The checked inputs of a migration. Built by [`preflight`] **before** any
/// account is created, so a bad `--from` never leaves a half-made principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preflight {
    /// The old T0 `--data-dir`, canonical.
    pub from: PathBuf,
    /// The home the T0 daemon ran with, canonical.
    pub old_home: PathBuf,
    /// `<from>.t0-migrated-<ts>`; does not exist yet.
    pub archive: PathBuf,
}

/// One `ikenga.db` column holding paths under the old home or data dir.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DbPathHit {
    pub table: String,
    pub column: String,
    /// `old_home` or `old_data_dir`.
    pub under: &'static str,
    pub rows: i64,
    /// Up to three values, each cut to 160 chars.
    pub samples: Vec<String>,
}

/// What [`migrate`] did. Printed by the CLI and written to
/// `<archive>/MIGRATION-REPORT.json`.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub principal_id: String,
    pub username: String,
    pub unix_name: String,
    pub unix_uid: u32,
    pub mode: Mode,
    pub from: PathBuf,
    pub old_home: PathBuf,
    pub data_dir: PathBuf,
    pub home: PathBuf,
    pub archive: PathBuf,
    /// The old dir itself became the archive (always, except when it is a
    /// mount point under a copy migration).
    pub archive_is_old_dir: bool,
    /// The old dir could not be renamed (a mount point); it stays where it
    /// was, read-only, and only the archive-only files moved out of it.
    pub old_dir_left_in_place: bool,
    /// R-10: access-store files now in the archive (and not in the data dir).
    pub access_store_archived: Vec<String>,
    pub files_copied: u64,
    pub bytes_copied: u64,
    pub home_entries_copied: Vec<String>,
    /// Already present in the new home; left as they were.
    pub home_entries_skipped: Vec<String>,
    /// Sockets, FIFOs and devices are not copied.
    pub special_files_skipped: Vec<String>,
    /// `fs_roots.json` entries rewritten from the old home onto the new one.
    pub fs_roots_rewritten: Vec<(String, String)>,
    /// `fs_roots.json` entries under the old data dir: not rewritten.
    pub fs_roots_under_old_data_dir: Vec<String>,
    /// §11.2: other absolute paths in `ikenga.db` are not rewritten.
    pub db_paths: Vec<DbPathHit>,
    pub db_scan_error: Option<String>,
    pub notes: Vec<String>,
}

impl MigrationReport {
    /// The operator-facing summary.
    pub fn summary(&self) -> String {
        let mut s = format!(
            "migrated T0 data dir {} into {} ({}) as {} uid {} [{}]\n",
            self.from.display(),
            self.username,
            self.principal_id,
            self.unix_name,
            self.unix_uid,
            self.mode.as_str()
        );
        s += &format!("  data:    {}\n", self.data_dir.display());
        s += &format!("  home:    {}", self.home.display());
        if self.home_entries_copied.is_empty() {
            s += "\n";
        } else {
            s += &format!(" (copied {})\n", self.home_entries_copied.join(", "));
        }
        s += &format!(
            "  archive: {} (root-only, read-only{})\n",
            self.archive.display(),
            if self.access_store_archived.is_empty() {
                String::new()
            } else {
                format!("; holds {}", self.access_store_archived.join(", "))
            }
        );
        if !self.fs_roots_rewritten.is_empty() {
            s += &format!(
                "  fs_roots.json: {} entr{} rewritten onto the new home\n",
                self.fs_roots_rewritten.len(),
                if self.fs_roots_rewritten.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
        }
        for root in &self.fs_roots_under_old_data_dir {
            s += &format!("  fs_roots.json: {root} is under the old data dir (not rewritten)\n");
        }
        if !self.db_paths.is_empty() {
            s += &format!(
                "  ikenga.db: {} column(s) hold paths under the old home or data dir (not \
                 rewritten):\n",
                self.db_paths.len()
            );
            for hit in &self.db_paths {
                s += &format!(
                    "    {}.{} — {} row(s) under {}\n",
                    hit.table, hit.column, hit.rows, hit.under
                );
            }
        }
        if let Some(e) = &self.db_scan_error {
            s += &format!("  ikenga.db: could not scan for old paths: {e}\n");
        }
        for skipped in &self.special_files_skipped {
            s += &format!("  skipped (not a file, dir or symlink): {skipped}\n");
        }
        for note in &self.notes {
            s += &format!("  note: {note}\n");
        }
        s += &format!("  report:  {}\n", self.archive.join(REPORT_FILE).display());
        s
    }
}

// ─── preflight ──────────────────────────────────────────────────────────────

/// The archive stamp: UTC, `20261001T120000Z`.
pub fn stamp_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// `<from>.t0-migrated-<stamp>`, beside `from`.
pub fn archive_path(from: &Path, stamp: &str) -> PathBuf {
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".into());
    from.with_file_name(format!("{name}.t0-migrated-{stamp}"))
}

/// The pid in `<from>/daemon.json`, if that process is alive. `Ok(None)`
/// when there is no file or the pid is gone; an unreadable file is an error
/// (we can't tell, so we refuse).
pub fn t0_daemon_alive(from: &Path) -> anyhow::Result<Option<u32>> {
    let path = from.join("daemon.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not JSON, so whether the T0 daemon still runs can't be told; stop it and \
             remove the file",
            path.display()
        )
    })?;
    let pid = json
        .get("pid")
        .and_then(serde_json::Value::as_u64)
        .and_then(|p| i32::try_from(p).ok())
        .filter(|p| *p > 0)
        .with_context(|| format!("{} carries no valid pid", path.display()))?;
    // SAFETY: signal 0 only checks that `pid` exists and may be signalled.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Ok(Some(pid as u32));
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Ok(None),
        // Exists, but not ours to signal: still alive.
        Some(libc::EPERM) => Ok(Some(pid as u32)),
        _ => Err(io::Error::last_os_error()).context("kill(pid, 0)"),
    }
}

fn canonical_dir(path: &Path, what: &str) -> anyhow::Result<PathBuf> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("{what} {}: cannot resolve it", path.display()))?;
    if !fs::metadata(&canonical)?.is_dir() {
        anyhow::bail!("{what} {} is not a directory", path.display());
    }
    Ok(canonical)
}

/// Check `--from` and `--home` before anything is written: both exist; the
/// old dir is a T0 data dir (and not a T1 root); it and the operator root
/// are separate trees; its T0 daemon is stopped; the archive name is free.
pub fn preflight(
    root: &OperatorRoot,
    from: &Path,
    old_home: &Path,
    stamp: &str,
) -> anyhow::Result<Preflight> {
    let from = canonical_dir(from, "--from")?;
    let old_home = canonical_dir(old_home, "--home")?;
    if from.parent().is_none() {
        anyhow::bail!("--from can't be /");
    }
    let root_dir = fs::canonicalize(root.root()).unwrap_or_else(|_| root.root().to_path_buf());
    if from.starts_with(&root_dir) || root_dir.starts_with(&from) {
        anyhow::bail!(
            "the T0 data dir {} and the operator root {} overlap; the T1 --data-dir must be a \
             separate directory (move the T0 one aside first, e.g. `mv /opt/ikenga/data \
             /opt/ikenga/data-t0`, then pass --from /opt/ikenga/data-t0)",
            from.display(),
            root_dir.display()
        );
    }
    if from.join("operator").exists() || from.join("principals").exists() {
        anyhow::bail!(
            "{} is a T1 operator root, not a T0 data dir",
            from.display()
        );
    }
    if !T0_MARKERS.iter().any(|m| from.join(m).exists()) {
        anyhow::bail!(
            "{} does not look like a T0 data dir (none of {} is in it)",
            from.display(),
            T0_MARKERS.join(", ")
        );
    }
    if old_home.starts_with(&from) {
        anyhow::bail!(
            "--home {} is inside --from {}",
            old_home.display(),
            from.display()
        );
    }
    if let Some(entry) = HOME_ENTRIES
        .iter()
        .find(|e| from.starts_with(old_home.join(e)))
    {
        anyhow::bail!(
            "--from {} is inside {entry} of the old home, which a copy migration also copies",
            from.display()
        );
    }
    if let Some(pid) = t0_daemon_alive(&from)? {
        anyhow::bail!(
            "the T0 daemon of {} is still running (pid {pid}, from its daemon.json); stop it \
             first. If that pid now belongs to something else, the daemon crashed: remove {}",
            from.display(),
            from.join("daemon.json").display()
        );
    }
    let archive = archive_path(&from, stamp);
    if fs::symlink_metadata(&archive).is_ok() {
        anyhow::bail!("{} already exists", archive.display());
    }
    Ok(Preflight {
        from,
        old_home,
        archive,
    })
}

// ─── filesystem helpers ─────────────────────────────────────────────────────

fn mkdir_0700(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn exists_no_follow(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_cross_device(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EXDEV) | Some(libc::EBUSY))
}

/// Byte-for-byte equality.
fn files_equal(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            // Equal lengths: b must be at EOF too.
            return Ok(fb.read(&mut bb[..1])? == 0);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// Copy one regular file to a new `dst` (never overwriting) and verify it.
fn copy_file_verified(src: &Path, dst: &Path, src_mode: u32) -> io::Result<u64> {
    let bytes = {
        let mut reader = File::open(src)?;
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dst)?;
        let n = io::copy(&mut reader, &mut writer)?;
        writer.sync_all()?;
        n
    };
    fs::set_permissions(dst, fs::Permissions::from_mode((src_mode & 0o700) | 0o600))?;
    if !files_equal(src, dst)? {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "verify failed: {} differs from {} after the copy",
                dst.display(),
                src.display()
            ),
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Default)]
struct CopyStats {
    files: u64,
    bytes: u64,
    special: Vec<String>,
}

/// Copy `src` to the new path `dst` without following symlinks (they are
/// recreated as symlinks), verifying every file; skip `skip_top` names
/// directly under `src`. Sockets, FIFOs and devices are listed, not copied.
fn copy_tree(src: &Path, dst: &Path, skip_top: &[&str], stats: &mut CopyStats) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if ft.is_dir() {
        mkdir_0700(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            if skip_top.iter().any(|s| name == **s) {
                continue;
            }
            copy_tree(&entry.path(), &dst.join(&name), &[], stats)?;
        }
    } else if ft.is_file() {
        stats.bytes += copy_file_verified(src, dst, meta.mode())?;
        stats.files += 1;
    } else {
        stats.special.push(src.display().to_string());
    }
    Ok(())
}

/// I-9 for a migrated tree: chown everything to `uid:gid` (`lchown`, never
/// following a symlink) and strip group/other and set-id bits. Directories
/// become `0700`.
fn normalize_tree(path: &Path, uid: u32, gid: u32, ownership: Ownership) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            normalize_tree(&entry?.path(), uid, gid, ownership)?;
        }
    }
    if !meta.file_type().is_symlink() {
        let mode = if meta.is_dir() {
            0o700
        } else {
            (meta.mode() & 0o700) | 0o600
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    if ownership == Ownership::Enforce {
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))?;
    }
    Ok(())
}

/// Make the archive (or an old dir left in place) read-only and root-only:
/// directories `0500`, files lose every write and group/other bit, owner
/// root under [`Ownership::Enforce`].
fn seal_tree(path: &Path, ownership: Ownership) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            seal_tree(&entry?.path(), ownership)?;
        }
    }
    if ownership == Ownership::Enforce {
        std::os::unix::fs::lchown(path, Some(0), Some(0))?;
    }
    if !meta.file_type().is_symlink() {
        let mode = if meta.is_dir() {
            0o500
        } else {
            (meta.mode() & 0o500) | 0o400
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// I-9: every path under `dir` (itself included) is owned by `uid:gid`
/// (when `check_owner`) and carries no group/other or set-id bit. Returns
/// one line per violation.
pub fn i9_violations(dir: &Path, uid: u32, gid: u32, check_owner: bool) -> io::Result<Vec<String>> {
    fn walk(
        path: &Path,
        uid: u32,
        gid: u32,
        check_owner: bool,
        out: &mut Vec<String>,
    ) -> io::Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if check_owner && (meta.uid(), meta.gid()) != (uid, gid) {
            out.push(format!(
                "{}: owned by {}:{}, not {uid}:{gid}",
                path.display(),
                meta.uid(),
                meta.gid()
            ));
        }
        if !meta.file_type().is_symlink() && meta.mode() & 0o6077 != 0 {
            out.push(format!(
                "{}: mode {:o} has group/other or set-id bits",
                path.display(),
                meta.mode() & 0o7777
            ));
        }
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                walk(&entry?.path(), uid, gid, check_owner, out)?;
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(dir, uid, gid, check_owner, &mut out)?;
    Ok(out)
}

/// Holds `<root>/principals/<id>` root-owned while the migration runs, so no
/// process of the principal's uid can enter it (it is `0700`); hands it back
/// on drop, success or not.
struct PrincipalDirHold {
    path: PathBuf,
    uid: u32,
    gid: u32,
    ownership: Ownership,
}

impl PrincipalDirHold {
    fn take(path: &Path, uid: u32, gid: u32, ownership: Ownership) -> io::Result<Self> {
        if ownership == Ownership::Enforce {
            std::os::unix::fs::lchown(path, Some(0), Some(0))?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            uid,
            gid,
            ownership,
        })
    }
}

impl Drop for PrincipalDirHold {
    fn drop(&mut self) {
        if self.ownership == Ownership::Enforce {
            if let Err(e) = std::os::unix::fs::lchown(&self.path, Some(self.uid), Some(self.gid)) {
                tracing::error!(
                    "adopt-t0: could not hand {} back to {}:{}: {e}",
                    self.path.display(),
                    self.uid,
                    self.gid
                );
            }
        }
    }
}

/// The principal has never run: `data/` holds at most an empty `tmp/`.
fn require_fresh_data(data: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(data).with_context(|| format!("{}", data.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let empty_tmp = name == "tmp"
            && entry.file_type()?.is_dir()
            && fs::read_dir(entry.path())?.next().is_none();
        if !empty_tmp {
            anyhow::bail!(
                "{} already holds {}: adopt-t0 only migrates into a principal that has never \
                 run (create a new account for it)",
                data.display(),
                name.to_string_lossy()
            );
        }
    }
    Ok(())
}

/// Remove the fresh (empty) `data/` so something can be renamed onto it.
fn remove_fresh_data(data: &Path) -> io::Result<()> {
    let tmp = data.join("tmp");
    if tmp.exists() {
        fs::remove_dir(&tmp)?;
    }
    fs::remove_dir(data)
}

/// Put back an empty `data/` + `tmp/`, owned by the principal, after a
/// failed swap.
fn restore_fresh_data(data: &Path, uid: u32, gid: u32, ownership: Ownership) {
    for dir in [data.to_path_buf(), data.join("tmp")] {
        if !exists_no_follow(&dir) {
            let _ = mkdir_0700(&dir);
        }
        if ownership == Ownership::Enforce {
            let _ = std::os::unix::fs::lchown(&dir, Some(uid), Some(gid));
        }
    }
}

/// The archive-only files of `from` by copy (the archive is on another
/// filesystem): copy, verify, and only then delete the source (R-10).
fn archive_by_copy(from: &Path, archive: &Path) -> io::Result<Vec<String>> {
    mkdir_0700(archive)?;
    let mut moved = Vec::new();
    for name in ARCHIVE_ONLY {
        let src = from.join(name);
        let Ok(meta) = fs::symlink_metadata(&src) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&src)?, archive.join(name))?;
        } else {
            copy_file_verified(&src, &archive.join(name), meta.mode())?;
        }
        fs::remove_file(&src)?;
        moved.push(name.to_string());
    }
    Ok(moved)
}

// ─── fs_roots.json ──────────────────────────────────────────────────────────

/// What [`rewrite_fs_roots`] decided.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FsRootsRewrite {
    /// The new file text, when anything changed.
    pub text: Option<String>,
    pub rewritten: Vec<(String, String)>,
    pub under_old_data_dir: Vec<String>,
}

/// Rewrite absolute `roots` under `old_home` onto `new_home` (§11.2). `~`
/// and `$HOME` forms are left alone: they resolve against the principal's
/// own `HOME` at run time. Entries under the old data dir are listed, not
/// rewritten. Unknown keys are kept.
pub(crate) fn rewrite_fs_roots(
    text: &str,
    old_home: &Path,
    new_home: Option<&Path>,
    old_data_dir: &Path,
) -> anyhow::Result<FsRootsRewrite> {
    let mut json: serde_json::Value = serde_json::from_str(text).context("fs_roots.json")?;
    let mut out = FsRootsRewrite::default();
    let Some(roots) = json.get_mut("roots").and_then(|r| r.as_array_mut()) else {
        return Ok(out);
    };
    for root in roots.iter_mut() {
        let Some(s) = root.as_str() else { continue };
        let path = Path::new(s);
        if !path.is_absolute() {
            continue;
        }
        if let (Some(new_home), Ok(rest)) = (new_home, path.strip_prefix(old_home)) {
            let next = if rest.as_os_str().is_empty() {
                new_home.to_path_buf()
            } else {
                new_home.join(rest)
            };
            let next = next.to_string_lossy().into_owned();
            out.rewritten.push((s.to_string(), next.clone()));
            *root = serde_json::Value::String(next);
        } else if path.starts_with(old_data_dir) {
            out.under_old_data_dir.push(s.to_string());
        }
    }
    if !out.rewritten.is_empty() {
        out.text = Some(serde_json::to_string_pretty(&json)?);
    }
    Ok(out)
}

// ─── ikenga.db path scan ────────────────────────────────────────────────────

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Every text column of every table in `db` holding a value under one of
/// `prefixes` (`(label, path)`): equal to the path, or containing
/// `<path>/` anywhere (JSON blobs included).
async fn scan_db_paths(
    db: &Path,
    prefixes: &[(&'static str, &Path)],
) -> anyhow::Result<Vec<DbPathHit>> {
    let mut conn = SqliteConnectOptions::new()
        .filename(db)
        .read_only(true)
        .create_if_missing(false)
        .connect()
        .await?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )
    .fetch_all(&mut conn)
    .await?;
    let mut hits = Vec::new();
    for table in tables {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
                .bind(&table)
                .fetch_all(&mut conn)
                .await?;
        for column in columns {
            for (under, prefix) in prefixes {
                let exact = prefix.to_string_lossy().into_owned();
                let inner = format!("{exact}/");
                let (t, c) = (quote_ident(&table), quote_ident(&column));
                let filter = format!(
                    "FROM {t} WHERE typeof({c}) = 'text' AND ({c} = ?1 OR instr({c}, ?2) > 0)"
                );
                let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) {filter}"))
                    .bind(&exact)
                    .bind(&inner)
                    .fetch_one(&mut conn)
                    .await?;
                if rows == 0 {
                    continue;
                }
                let samples =
                    sqlx::query(&format!("SELECT substr({c}, 1, 160) AS v {filter} LIMIT 3"))
                        .bind(&exact)
                        .bind(&inner)
                        .fetch_all(&mut conn)
                        .await?
                        .iter()
                        .filter_map(|r| r.try_get::<String, _>("v").ok())
                        .collect();
                hits.push(DbPathHit {
                    table: table.clone(),
                    column: column.clone(),
                    under,
                    rows,
                    samples,
                });
            }
        }
    }
    conn.close().await?;
    Ok(hits)
}

// ─── the migration ──────────────────────────────────────────────────────────

fn dev_of(path: &Path) -> io::Result<u64> {
    Ok(fs::metadata(path)?.dev())
}

/// Pick the [`Mode`] for `account`.
fn choose_mode(account: &Account, pre: &Preflight, principal_dir: &Path) -> anyhow::Result<Mode> {
    if !account.adopted {
        return Ok(Mode::Copy);
    }
    let home = fs::canonicalize(&account.home).unwrap_or_else(|_| account.home.clone());
    if home != pre.old_home {
        anyhow::bail!(
            "{} is adopted onto {} whose home is {}, but --home is {}; an adopted account keeps \
             its home, so --home must name it",
            account.username,
            account.unix_name,
            home.display(),
            pre.old_home.display()
        );
    }
    let parent = pre.from.parent().unwrap_or(Path::new("/"));
    let mount_point = dev_of(&pre.from)? != dev_of(parent)?;
    let same_fs = dev_of(&pre.from)? == dev_of(principal_dir)?;
    Ok(if same_fs && !mount_point {
        Mode::AdoptRename
    } else {
        Mode::AdoptCopy
    })
}

/// [`Mode::AdoptRename`]: archive-only files to a new archive dir, then the
/// whole old dir onto `data/` in one rename. Undone on failure.
fn move_by_rename(
    pre: &Preflight,
    data: &Path,
    (uid, gid, ownership): (u32, u32, Ownership),
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    mkdir_0700(&pre.archive).with_context(|| format!("{}", pre.archive.display()))?;
    let mut moved: Vec<&str> = Vec::new();
    let undo = |moved: &[&str]| {
        for name in moved {
            let _ = fs::rename(pre.archive.join(name), pre.from.join(name));
        }
        let _ = fs::remove_dir(&pre.archive);
    };
    for name in ARCHIVE_ONLY {
        if !exists_no_follow(&pre.from.join(name)) {
            continue;
        }
        if let Err(e) = fs::rename(pre.from.join(name), pre.archive.join(name)) {
            undo(&moved);
            return Err(e).with_context(|| format!("archiving {name}"));
        }
        moved.push(name);
    }
    if let Err(e) = remove_fresh_data(data) {
        restore_fresh_data(data, uid, gid, ownership);
        undo(&moved);
        return Err(e).with_context(|| format!("{}", data.display()));
    }
    if let Err(e) = fs::rename(&pre.from, data) {
        restore_fresh_data(data, uid, gid, ownership);
        undo(&moved);
        return Err(e)
            .with_context(|| format!("renaming {} onto {}", pre.from.display(), data.display()));
    }
    report.access_store_archived = moved
        .iter()
        .filter(|n| ACCESS_STORE_FILES.contains(n))
        .map(|n| n.to_string())
        .collect();
    report.archive_is_old_dir = false;
    report.notes.push(format!(
        "the data dir was renamed into place; {} keeps only the files that never migrate",
        pre.archive.display()
    ));
    Ok(())
}

/// [`Mode::AdoptCopy`] / [`Mode::Copy`]: copy + verify into a staging dir
/// beside `data/` (and, for a fresh principal, the home dot-dirs), archive
/// the old dir, then swap the staging dir onto `data/`.
fn move_by_copy(
    pre: &Preflight,
    principal_dir: &Path,
    data: &Path,
    new_home: Option<&Path>,
    (uid, gid, ownership): (u32, u32, Ownership),
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    let staging = principal_dir.join("data.t0-adopt");
    if exists_no_follow(&staging) {
        // A failed earlier run; the principal dir is held, nothing else wrote it.
        fs::remove_dir_all(&staging)?;
    }
    let mut stats = CopyStats::default();
    // Top-level paths this run created in the new home, for the undo.
    let mut created_in_home: Vec<PathBuf> = Vec::new();
    let undo = |created: &[PathBuf]| {
        let _ = fs::remove_dir_all(&staging);
        for path in created {
            let _ = if fs::symlink_metadata(path)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
        }
    };

    if let Err(e) = copy_tree(&pre.from, &staging, &ARCHIVE_ONLY, &mut stats) {
        undo(&created_in_home);
        return Err(e).with_context(|| format!("copying {}", pre.from.display()));
    }

    if let Some(new_home) = new_home {
        for entry in HOME_ENTRIES {
            let src = pre.old_home.join(entry);
            if !exists_no_follow(&src) {
                continue;
            }
            let dst = new_home.join(entry);
            if exists_no_follow(&dst) {
                report.home_entries_skipped.push(entry.to_string());
                continue;
            }
            // `.local/share/app.ikenga`: create the missing parents, and
            // remember the outermost one this run created.
            let mut first_created = None;
            let mut parent = new_home.to_path_buf();
            let rel = Path::new(entry);
            let components: Vec<_> = rel.components().collect();
            for comp in &components[..components.len().saturating_sub(1)] {
                parent.push(comp);
                if !exists_no_follow(&parent) {
                    if let Err(e) = mkdir_0700(&parent) {
                        undo(&created_in_home);
                        return Err(e).with_context(|| format!("{}", parent.display()));
                    }
                    first_created.get_or_insert_with(|| parent.clone());
                }
            }
            // A top-level entry may be a symlink to a volume (Docker mounts
            // `/root/.claude`): copy what it points at, not the link — the
            // link would point the principal at root's files.
            let src = match fs::symlink_metadata(&src) {
                Ok(m) if m.file_type().is_symlink() => match fs::canonicalize(&src) {
                    Ok(target) => {
                        report.notes.push(format!(
                            "{} is a symlink; copied its target {}",
                            src.display(),
                            target.display()
                        ));
                        target
                    }
                    Err(_) => src,
                },
                _ => src,
            };
            let result = copy_tree(&src, &dst, &[], &mut stats);
            created_in_home.push(first_created.unwrap_or_else(|| dst.clone()));
            if let Err(e) = result {
                undo(&created_in_home);
                return Err(e).with_context(|| format!("copying {}", src.display()));
            }
            report.home_entries_copied.push(entry.to_string());
        }
    }

    // The archive: the old dir itself when it can be renamed.
    match fs::rename(&pre.from, &pre.archive) {
        Ok(()) => {
            report.archive_is_old_dir = true;
            report.access_store_archived = ACCESS_STORE_FILES
                .iter()
                .filter(|n| exists_no_follow(&pre.archive.join(n)))
                .map(|n| n.to_string())
                .collect();
        }
        Err(e) if is_cross_device(&e) => {
            // A mount point (a Docker volume): it stays, read-only; the
            // archive-only files leave it by verified copy (R-10).
            match archive_by_copy(&pre.from, &pre.archive) {
                Ok(moved) => {
                    report.access_store_archived = moved
                        .into_iter()
                        .filter(|n| ACCESS_STORE_FILES.contains(&n.as_str()))
                        .collect();
                }
                Err(e) => {
                    undo(&created_in_home);
                    return Err(e).with_context(|| {
                        format!("archiving the access store into {}", pre.archive.display())
                    });
                }
            }
            report.old_dir_left_in_place = true;
            report.notes.push(format!(
                "{} could not be renamed (a mount point); it stays in place, read-only. The \
                 archive {} is on the parent filesystem — keep a copy if that is not persistent",
                pre.from.display(),
                pre.archive.display()
            ));
            if let Err(e) = seal_tree(&pre.from, ownership) {
                report.notes.push(format!(
                    "could not make {} read-only: {e}",
                    pre.from.display()
                ));
            }
        }
        Err(e) => {
            undo(&created_in_home);
            return Err(e).with_context(|| {
                format!(
                    "renaming {} to {}",
                    pre.from.display(),
                    pre.archive.display()
                )
            });
        }
    }

    if let Err(e) = remove_fresh_data(data).and_then(|()| fs::rename(&staging, data)) {
        restore_fresh_data(data, uid, gid, ownership);
        return Err(e).with_context(|| {
            format!(
                "the copy is complete at {} and the old dir is archived at {}, but it could not \
                 be moved onto {}; move it by hand",
                staging.display(),
                pre.archive.display(),
                data.display()
            )
        });
    }
    report.files_copied = stats.files;
    report.bytes_copied = stats.bytes;
    report.special_files_skipped = stats.special;
    Ok(())
}

/// Migrate `pre.from` into `account`'s principal (see the module docs).
/// `ownership` is [`Ownership::Enforce`] in production.
pub async fn migrate(
    root: &OperatorRoot,
    ownership: Ownership,
    account: &Account,
    pre: &Preflight,
) -> anyhow::Result<MigrationReport> {
    if account.is_disabled() {
        anyhow::bail!("{} is disabled; enable it first", account.username);
    }
    let id = account.principal_id;
    let (uid, gid) = (account.unix_uid, account.unix_gid);
    let principal_dir = root.principal_dir(id);
    let data = root.principal_data(id);
    let mode = choose_mode(account, pre, &principal_dir)?;
    // Only a fresh principal's home is ours to fill; an adopted one keeps
    // the T0 user's own home, untouched.
    let new_home = (mode == Mode::Copy).then(|| account.home.clone());

    let mut report = MigrationReport {
        principal_id: id.to_string(),
        username: account.username.clone(),
        unix_name: account.unix_name.clone(),
        unix_uid: uid,
        mode,
        from: pre.from.clone(),
        old_home: pre.old_home.clone(),
        data_dir: data.clone(),
        home: account.home.clone(),
        archive: pre.archive.clone(),
        archive_is_old_dir: false,
        old_dir_left_in_place: false,
        access_store_archived: Vec::new(),
        files_copied: 0,
        bytes_copied: 0,
        home_entries_copied: Vec::new(),
        home_entries_skipped: Vec::new(),
        special_files_skipped: Vec::new(),
        fs_roots_rewritten: Vec::new(),
        fs_roots_under_old_data_dir: Vec::new(),
        db_paths: Vec::new(),
        db_scan_error: None,
        notes: vec![
            "chi runs that were live under T0 are reconciled as Unverified by the existing \
             sweep (their pid is probed as a different uid now)"
                .into(),
        ],
    };

    {
        let _hold = PrincipalDirHold::take(&principal_dir, uid, gid, ownership)
            .with_context(|| format!("{}", principal_dir.display()))?;
        require_fresh_data(&data)?;
        // Again, under the hold: the daemon may have been started since.
        if let Some(pid) = t0_daemon_alive(&pre.from)? {
            anyhow::bail!(
                "the T0 daemon of {} is running again (pid {pid})",
                pre.from.display()
            );
        }

        match mode {
            Mode::AdoptRename => move_by_rename(pre, &data, (uid, gid, ownership), &mut report)?,
            Mode::AdoptCopy | Mode::Copy => move_by_copy(
                pre,
                &principal_dir,
                &data,
                new_home.as_deref(),
                (uid, gid, ownership),
                &mut report,
            )?,
        }
        if !exists_no_follow(&data.join("tmp")) {
            mkdir_0700(&data.join("tmp"))?;
        }

        let fs_roots = data.join("fs_roots.json");
        if let Ok(text) = fs::read_to_string(&fs_roots) {
            match rewrite_fs_roots(&text, &pre.old_home, new_home.as_deref(), &pre.from) {
                Ok(rw) => {
                    if let Some(next) = &rw.text {
                        fs::write(&fs_roots, next)?;
                    }
                    report.fs_roots_rewritten = rw.rewritten;
                    report.fs_roots_under_old_data_dir = rw.under_old_data_dir;
                }
                Err(e) => report
                    .notes
                    .push(format!("fs_roots.json was not rewritten: {e:#}")),
            }
        }

        let db = data.join("ikenga.db");
        if db.exists() {
            let mut prefixes: Vec<(&'static str, &Path)> =
                vec![("old_data_dir", pre.from.as_path())];
            if new_home.is_some() && pre.old_home != Path::new("/") {
                prefixes.insert(0, ("old_home", pre.old_home.as_path()));
            }
            match scan_db_paths(&db, &prefixes).await {
                Ok(hits) => report.db_paths = hits,
                Err(e) => report.db_scan_error = Some(format!("{e:#}")),
            }
        }

        // Owned by the principal, private (I-9) — including anything the
        // scan above created as root (a `-shm`).
        normalize_tree(&data, uid, gid, ownership)?;
        if let Some(home) = &new_home {
            normalize_tree(home, uid, gid, ownership)?;
        }
    }

    let violations = i9_violations(&principal_dir, uid, gid, ownership == Ownership::Enforce)?;
    if !violations.is_empty() {
        anyhow::bail!(
            "I-9 does not hold under {} after the migration:\n  {}",
            principal_dir.display(),
            violations.join("\n  ")
        );
    }

    let report_path = pre.archive.join(REPORT_FILE);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&report_path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(serde_json::to_string_pretty(&report)?.as_bytes())
        })
        .with_context(|| format!("{}", report_path.display()))?;
    seal_tree(&pre.archive, ownership)
        .with_context(|| format!("sealing {}", pre.archive.display()))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::PrincipalId;
    use crate::server::operator::test_support;

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    /// A T0 data dir: a real ikenga.db with paths under the old home and
    /// data dir, fs_roots.json, the access store, a stale daemon.json, and a
    /// nested dir.
    async fn t0_install(base: &Path) -> (PathBuf, PathBuf) {
        let base = fs::canonicalize(base).unwrap();
        let from = base.join("t0-data");
        let home = base.join("t0-home");
        fs::create_dir_all(from.join("chi-cache")).unwrap();
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".local/share/app.ikenga/store")).unwrap();
        fs::create_dir_all(home.join("Documents")).unwrap();
        let (from_s, home_s) = (from.to_string_lossy(), home.to_string_lossy());

        let db = from.join("ikenga.db");
        let mut conn = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
            .connect()
            .await
            .unwrap();
        sqlx::query("CREATE TABLE projects (id INTEGER PRIMARY KEY, root TEXT, meta TEXT)")
            .execute(&mut conn)
            .await
            .unwrap();
        for (root, meta) in [
            (format!("{home_s}/code/app"), "{}".to_string()),
            (
                "/srv/elsewhere".to_string(),
                format!(r#"{{"cache":"{from_s}/chi-cache/x"}}"#),
            ),
            (home_s.to_string(), "{}".to_string()),
        ] {
            sqlx::query("INSERT INTO projects (root, meta) VALUES (?, ?)")
                .bind(root)
                .bind(meta)
                .execute(&mut conn)
                .await
                .unwrap();
        }
        conn.close().await.unwrap();

        fs::write(
            from.join("fs_roots.json"),
            format!(
                r#"{{"roots":["{home_s}/code","~/notes","/srv/shared","{from_s}/inbox"],"x":1}}"#
            ),
        )
        .unwrap();
        for f in ACCESS_STORE_FILES {
            fs::write(from.join(f), format!("{f} secret")).unwrap();
        }
        // A pid that is certainly gone.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        fs::write(
            from.join("daemon.json"),
            format!(r#"{{"pid":{dead},"token":"t0-bearer"}}"#),
        )
        .unwrap();
        fs::write(from.join("supabase.json"), "{}").unwrap();
        fs::write(from.join("chi-cache/run.json"), vec![7u8; 200_000]).unwrap();
        fs::set_permissions(
            from.join("supabase.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        fs::write(home.join(".claude/creds"), "claude login").unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        fs::write(home.join(".local/share/app.ikenga/store/s"), "store").unwrap();
        fs::write(home.join("Documents/private"), "not copied").unwrap();
        (from, home)
    }

    /// A principal laid out as `create_in` / `adopt_in` leave it (owners not
    /// enforced: these tests run unprivileged).
    fn principal(root: &OperatorRoot, adopted_home: Option<&Path>) -> Account {
        let id = PrincipalId::new_v7();
        let dir = root.principal_dir(id);
        mkdir_0700(&dir).unwrap();
        mkdir_0700(&root.principal_data(id)).unwrap();
        mkdir_0700(&root.principal_data(id).join("tmp")).unwrap();
        let home = match adopted_home {
            Some(h) => h.to_path_buf(),
            None => {
                mkdir_0700(&root.principal_home(id)).unwrap();
                root.principal_home(id)
            }
        };
        let (uid, gid) = (unsafe { libc::geteuid() }, unsafe { libc::getegid() });
        Account {
            principal_id: id,
            username: "ada".into(),
            password_phc: None,
            unix_name: if adopted_home.is_some() {
                "ikenga".into()
            } else {
                "ik-ada".into()
            },
            unix_uid: uid,
            unix_gid: gid,
            home,
            shell: "/bin/sh".into(),
            is_admin: true,
            session_epoch: 0,
            adopted: adopted_home.is_some(),
            disabled_at: None,
            created_at: 0,
            updated_at: 0,
            password_changed_at: None,
        }
    }

    #[test]
    fn fs_roots_under_the_old_home_move_and_the_rest_stay() {
        let text = r#"{"roots":["/root/code","/root","~/notes","$HOME/x","/rootfs/a","/opt/ikenga/data/inbox","/srv"],"keep":true}"#;
        let rw = rewrite_fs_roots(
            text,
            Path::new("/root"),
            Some(Path::new("/srv/ik/principals/p/home")),
            Path::new("/opt/ikenga/data"),
        )
        .unwrap();
        assert_eq!(
            rw.rewritten,
            [
                (
                    "/root/code".to_string(),
                    "/srv/ik/principals/p/home/code".to_string()
                ),
                ("/root".to_string(), "/srv/ik/principals/p/home".to_string()),
            ]
        );
        assert_eq!(rw.under_old_data_dir, ["/opt/ikenga/data/inbox"]);
        let json: serde_json::Value = serde_json::from_str(rw.text.as_deref().unwrap()).unwrap();
        assert_eq!(json["keep"], true);
        assert_eq!(
            json["roots"],
            serde_json::json!([
                "/srv/ik/principals/p/home/code",
                "/srv/ik/principals/p/home",
                "~/notes",
                "$HOME/x",
                "/rootfs/a",
                "/opt/ikenga/data/inbox",
                "/srv"
            ])
        );
        // Home unchanged (adopted): nothing rewritten, nothing written.
        let rw = rewrite_fs_roots(
            text,
            Path::new("/root"),
            None,
            Path::new("/opt/ikenga/data"),
        )
        .unwrap();
        assert!(rw.text.is_none() && rw.rewritten.is_empty());
        assert_eq!(rw.under_old_data_dir.len(), 1);
    }

    #[test]
    fn a_live_t0_daemon_is_detected_and_a_dead_one_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(t0_daemon_alive(tmp.path()).unwrap(), None);
        let own = std::process::id();
        fs::write(
            tmp.path().join("daemon.json"),
            format!(r#"{{"pid":{own}}}"#),
        )
        .unwrap();
        assert_eq!(t0_daemon_alive(tmp.path()).unwrap(), Some(own));
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        fs::write(
            tmp.path().join("daemon.json"),
            format!(r#"{{"pid":{dead}}}"#),
        )
        .unwrap();
        assert_eq!(t0_daemon_alive(tmp.path()).unwrap(), None);
        // Can't tell → refuse.
        fs::write(tmp.path().join("daemon.json"), "garbage").unwrap();
        assert!(t0_daemon_alive(tmp.path()).is_err());
        fs::write(tmp.path().join("daemon.json"), r#"{"pid":0}"#).unwrap();
        assert!(t0_daemon_alive(tmp.path()).is_err());
    }

    #[tokio::test]
    async fn preflight_refuses_what_it_must() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let stamp = "20261001T000000Z";
        let ok = preflight(&root, &from, &home, stamp).unwrap();
        assert_eq!(
            ok.archive,
            fs::canonicalize(tmp.path())
                .unwrap()
                .join("t0-data.t0-migrated-20261001T000000Z")
        );

        // The operator root itself, or anything inside it.
        assert!(preflight(&root, root.root(), &home, stamp).is_err());
        let inside = root.root().join("old");
        fs::create_dir(&inside).unwrap();
        fs::write(inside.join("ikenga.db"), b"").unwrap();
        let err = preflight(&root, &inside, &home, stamp).unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
        // Not a T0 dir.
        let err = preflight(&root, &home, tmp.path(), stamp).unwrap_err();
        assert!(err.to_string().contains("does not look like"), "{err}");
        // A T1 root.
        fs::create_dir(from.join("operator")).unwrap();
        assert!(preflight(&root, &from, &home, stamp).is_err());
        fs::remove_dir(from.join("operator")).unwrap();
        // A live daemon.
        fs::write(
            from.join("daemon.json"),
            format!(r#"{{"pid":{}}}"#, std::process::id()),
        )
        .unwrap();
        let err = preflight(&root, &from, &home, stamp).unwrap_err();
        assert!(err.to_string().contains("still running"), "{err}");
        fs::remove_file(from.join("daemon.json")).unwrap();
        // The archive name is taken.
        fs::create_dir(&ok.archive).unwrap();
        assert!(preflight(&root, &from, &home, stamp).is_err());
        // Missing paths.
        assert!(preflight(&root, &tmp.path().join("nope"), &home, stamp).is_err());
    }

    /// §11.2 copy (root/Docker T0) + R-10, unprivileged: the data dir and
    /// the home dot-dirs are copied, `fs_roots.json` is rewritten, the old
    /// dir becomes the archive, the access store never reaches `data/`, and
    /// the report lists the old paths left in `ikenga.db`.
    #[tokio::test]
    async fn copy_migration_into_a_fresh_principal() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let account = principal(&root, None);
        // A Docker-style volume behind a symlinked dot-dir.
        let volume = home.parent().unwrap().join("gemini-volume");
        fs::create_dir(&volume).unwrap();
        fs::write(volume.join("oauth"), "gemini login").unwrap();
        std::os::unix::fs::symlink(&volume, home.join(".gemini")).unwrap();
        let pre = preflight(&root, &from, &home, "20261001T000001Z").unwrap();
        let report = migrate(&root, Ownership::SkipForTests, &account, &pre)
            .await
            .unwrap();
        assert_eq!(report.mode, Mode::Copy);

        let data = root.principal_data(account.principal_id);
        let new_home = root.principal_home(account.principal_id);
        // R-10: never in the principal's data dir; all in the archive.
        for f in ARCHIVE_ONLY {
            assert!(!exists_no_follow(&data.join(f)), "{f} leaked into data/");
            assert!(pre.archive.join(f).exists(), "{f} not archived");
        }
        assert_eq!(report.access_store_archived, ACCESS_STORE_FILES);
        assert!(report.archive_is_old_dir && !report.old_dir_left_in_place);
        assert!(!pre.from.exists(), "the old dir is now the archive");
        // The data moved over, byte-identical.
        assert_eq!(
            fs::read(data.join("chi-cache/run.json")).unwrap(),
            vec![7u8; 200_000]
        );
        assert!(data.join("ikenga.db").exists() && data.join("tmp").is_dir());
        // Data: db, fs_roots, supabase, run.json; home: creds, .claude.json,
        // s, oauth.
        assert_eq!(report.files_copied, 8, "{report:?}");
        // Home dot-dirs only.
        assert_eq!(
            fs::read_to_string(new_home.join(".claude/creds")).unwrap(),
            "claude login"
        );
        assert!(new_home.join(".claude.json").exists());
        assert!(new_home.join(".local/share/app.ikenga/store/s").exists());
        assert!(!new_home.join("Documents").exists());
        assert_eq!(
            report.home_entries_copied,
            [
                ".local/share/app.ikenga",
                ".claude",
                ".claude.json",
                ".gemini"
            ]
        );
        // The symlinked entry arrives as the directory it pointed at.
        assert!(fs::symlink_metadata(new_home.join(".gemini"))
            .unwrap()
            .is_dir());
        assert_eq!(
            fs::read_to_string(new_home.join(".gemini/oauth")).unwrap(),
            "gemini login"
        );
        // fs_roots.json: home entries rewritten, the rest kept and listed.
        let roots: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(data.join("fs_roots.json")).unwrap()).unwrap();
        assert_eq!(roots["roots"][0], format!("{}/code", new_home.display()));
        assert_eq!(roots["roots"][1], "~/notes");
        assert_eq!(roots["x"], 1);
        assert_eq!(report.fs_roots_rewritten.len(), 1);
        assert_eq!(
            report.fs_roots_under_old_data_dir,
            [format!("{}/inbox", pre.from.display())]
        );
        // ikenga.db: listed, not rewritten.
        let under: Vec<_> = report
            .db_paths
            .iter()
            .map(|h| (h.column.as_str(), h.under, h.rows))
            .collect();
        assert_eq!(
            under,
            [("root", "old_home", 2), ("meta", "old_data_dir", 1)],
            "{:?}",
            report.db_paths
        );
        assert!(report.db_scan_error.is_none());
        // I-9 modes (owners are a root test); the archive is sealed.
        assert!(
            i9_violations(&root.principal_dir(account.principal_id), 0, 0, false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(mode_of(&data.join("supabase.json")), 0o600);
        assert_eq!(mode_of(&pre.archive), 0o500);
        assert_eq!(mode_of(&pre.archive.join("access.db")) & 0o222, 0);
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pre.archive.join(REPORT_FILE)).unwrap())
                .unwrap();
        assert_eq!(written["mode"], "copy");
        assert!(report.summary().contains("archive:"));
        // Undo the seal so the tempdir can be removed.
        fs::set_permissions(&pre.archive, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// §11.2 adopt on one filesystem: one rename. The archive keeps only
    /// what never migrates (R-10), and the home is not touched.
    #[tokio::test]
    async fn adopt_migration_renames_the_data_dir() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let account = principal(&root, Some(&home));
        let pre = preflight(&root, &from, &home, "20261001T000002Z").unwrap();
        let ino = fs::metadata(&from).unwrap().ino();
        let report = migrate(&root, Ownership::SkipForTests, &account, &pre)
            .await
            .unwrap();
        assert_eq!(report.mode, Mode::AdoptRename);
        let data = root.principal_data(account.principal_id);
        assert_eq!(
            fs::metadata(&data).unwrap().ino(),
            ino,
            "renamed, not copied"
        );
        assert_eq!(report.files_copied, 0);
        for f in ARCHIVE_ONLY {
            assert!(!exists_no_follow(&data.join(f)), "{f} leaked into data/");
            assert!(pre.archive.join(f).exists(), "{f} not archived");
        }
        let mut archived: Vec<_> = fs::read_dir(&pre.archive)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        archived.sort();
        assert_eq!(
            archived,
            [
                REPORT_FILE,
                "access.db",
                "access.db-shm",
                "access.db-wal",
                "daemon.json"
            ]
        );
        assert!(!report.archive_is_old_dir);
        // The home stayed as it was; fs_roots.json is unchanged.
        assert!(report.home_entries_copied.is_empty());
        assert!(home.join("Documents/private").exists());
        let roots: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(data.join("fs_roots.json")).unwrap()).unwrap();
        assert_eq!(roots["roots"][0], format!("{}/code", home.display()));
        // Only the data-dir paths are reported: the home didn't move.
        assert!(report.db_paths.iter().all(|h| h.under == "old_data_dir"));
        assert_eq!(report.db_paths.len(), 1);
        assert!(
            i9_violations(&root.principal_dir(account.principal_id), 0, 0, false)
                .unwrap()
                .is_empty()
        );
        fs::set_permissions(&pre.archive, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[tokio::test]
    async fn an_adopted_account_needs_its_own_home_and_a_fresh_data_dir() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let pre = preflight(&root, &from, &home, "20261001T000003Z").unwrap();

        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let wrong_home = principal(&root, Some(&elsewhere));
        let err = migrate(&root, Ownership::SkipForTests, &wrong_home, &pre)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--home must name it"), "{err}");

        let used = principal(&root, None);
        fs::write(
            root.principal_data(used.principal_id).join("ikenga.db"),
            b"",
        )
        .unwrap();
        let err = migrate(&root, Ownership::SkipForTests, &used, &pre)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("never run"), "{err}");

        let mut disabled = principal(&root, None);
        disabled.disabled_at = Some(1);
        assert!(migrate(&root, Ownership::SkipForTests, &disabled, &pre)
            .await
            .is_err());
        // Nothing moved.
        assert!(from.join("ikenga.db").exists() && from.join("access.db").exists());
        assert!(!pre.archive.exists());
    }

    /// A copy that fails part-way leaves the old dir, the principal's data
    /// dir and its home as they were. (Unprivileged only: root reads the
    /// unreadable file.)
    #[tokio::test]
    async fn a_failed_copy_leaves_everything_as_it_was() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let account = principal(&root, None);
        let pre = preflight(&root, &from, &home, "20261001T000004Z").unwrap();
        // Fails inside the home copy, after the data copy succeeded.
        fs::set_permissions(
            home.join(".claude/creds"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        assert!(migrate(&root, Ownership::SkipForTests, &account, &pre)
            .await
            .is_err());
        let data = root.principal_data(account.principal_id);
        require_fresh_data(&data).unwrap();
        let new_home = root.principal_home(account.principal_id);
        assert_eq!(fs::read_dir(&new_home).unwrap().count(), 0, "home cleaned");
        assert!(!root
            .principal_dir(account.principal_id)
            .join("data.t0-adopt")
            .exists());
        assert!(from.join("access.db").exists() && !pre.archive.exists());
    }

    /// R-10's copy branch: copied, verified, and only then removed from the
    /// source.
    #[test]
    fn archive_by_copy_deletes_the_source_after_verifying() {
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("access.db"), "chain").unwrap();
        fs::write(from.join("access.db-wal"), "wal").unwrap();
        fs::write(from.join("ikenga.db"), "stays").unwrap();
        let archive = tmp.path().join("archive");
        let moved = archive_by_copy(&from, &archive).unwrap();
        assert_eq!(moved, ["access.db", "access.db-wal"]);
        assert_eq!(
            fs::read_to_string(archive.join("access.db")).unwrap(),
            "chain"
        );
        assert!(!from.join("access.db").exists() && !from.join("access.db-wal").exists());
        assert!(from.join("ikenga.db").exists());
    }

    #[test]
    fn files_equal_compares_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, vec![1u8; 70_000]).unwrap();
        fs::write(&b, vec![1u8; 70_000]).unwrap();
        assert!(files_equal(&a, &b).unwrap());
        let mut other = vec![1u8; 70_000];
        other[69_999] = 2;
        fs::write(&b, other).unwrap();
        assert!(!files_equal(&a, &b).unwrap());
        fs::write(&b, vec![1u8; 10]).unwrap();
        assert!(!files_equal(&a, &b).unwrap());
    }

    /// The I-9 checker itself: modes everywhere, owners when asked.
    #[test]
    fn i9_checker_flags_group_other_and_setid_bits_and_foreign_owners() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("p");
        mkdir_0700(&dir).unwrap();
        mkdir_0700(&dir.join("data")).unwrap();
        fs::write(dir.join("data/ok"), "").unwrap();
        fs::set_permissions(dir.join("data/ok"), fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("data/link")).unwrap();
        let (uid, gid) = (unsafe { libc::geteuid() }, unsafe { libc::getegid() });
        assert!(i9_violations(&dir, uid, gid, true).unwrap().is_empty());

        fs::write(dir.join("data/loose"), "").unwrap();
        fs::set_permissions(dir.join("data/loose"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(dir.join("data"), fs::Permissions::from_mode(0o2700)).unwrap();
        let v = i9_violations(&dir, uid, gid, false).unwrap();
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|l| l.contains("loose") && l.contains("644")));
        assert!(v.iter().any(|l| l.contains("2700")));
        let foreign = i9_violations(&dir, uid.wrapping_add(1), gid, true).unwrap();
        assert!(foreign.iter().filter(|l| l.contains("owned by")).count() >= 4);
    }

    /// Real owners (I-9 under root): `create` + `adopt-t0` copy of a root
    /// T0 install, and `create --adopt-unix-user` + `adopt-t0` rename of a
    /// non-root one, both leave `<root>/principals/<id>` wholly owned by the
    /// principal with no group/other bits, and the archive root-only.
    /// `cargo test --lib -- --ignored --test-threads=1 t1_root`.
    mod t1_root {
        use super::*;
        use crate::executor::t1::tests::t1_root::require_root;
        use crate::server::operator::accounts::Actor;
        use crate::server::operator::provision::{Adopt, Provisioner, ProvisioningMode, UidRange};
        use crate::server::operator::{open_accounts, Opener};

        const PW: &str = "correct horse battery staple";

        struct HostUser(&'static str);
        impl Drop for HostUser {
            fn drop(&mut self) {
                for (tool, arg) in [("userdel", self.0), ("groupdel", self.0)] {
                    let _ = std::process::Command::new(tool)
                        .arg(arg)
                        .stderr(std::process::Stdio::null())
                        .status();
                }
                let _ = crate::server::operator::etc_files::EtcFiles::system().remove_user(self.0);
            }
        }

        fn enforced_root() -> (tempfile::TempDir, OperatorRoot) {
            let tmp = tempfile::tempdir().unwrap();
            fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            let root = OperatorRoot::new(tmp.path().join("root")).unwrap();
            root.prepare(Ownership::Enforce).unwrap();
            (tmp, root)
        }

        fn owner_mode(path: &Path) -> (u32, u32, u32) {
            let m = fs::symlink_metadata(path).unwrap();
            (m.uid(), m.gid(), m.mode() & 0o7777)
        }

        fn chown_tree(path: &Path, uid: u32) {
            std::os::unix::fs::lchown(path, Some(uid), Some(uid)).unwrap();
            if fs::symlink_metadata(path).unwrap().is_dir() {
                for e in fs::read_dir(path).unwrap() {
                    chown_tree(&e.unwrap().path(), uid);
                }
            }
        }

        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_adopt_t0_copy_from_root_satisfies_i9() {
            require_root();
            let _cleanup = HostUser("ik-t1root-ado");
            let (tmp, root) = enforced_root();
            let (from, home) = t0_install(tmp.path()).await;
            let prov = Provisioner::new(
                root.clone(),
                UidRange::new(28_060, 28_070).unwrap(),
                ProvisioningMode::Auto,
                Actor::Cli,
            );
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let pre = preflight(&root, &from, &home, "20261001T000010Z").unwrap();
            let a = prov
                .create(&pool, "t1root-ado", PW, true, None)
                .await
                .unwrap();
            let report = migrate(&root, Ownership::Enforce, &a, &pre).await.unwrap();
            assert_eq!(report.mode, Mode::Copy);

            let pdir = root.principal_dir(a.principal_id);
            let v = i9_violations(&pdir, a.unix_uid, a.unix_gid, true).unwrap();
            assert!(v.is_empty(), "I-9: {v:#?}");
            assert_eq!(owner_mode(&pdir), (a.unix_uid, a.unix_gid, 0o700));
            let data = root.principal_data(a.principal_id);
            assert_eq!(
                owner_mode(&data.join("ikenga.db")),
                (a.unix_uid, a.unix_gid, 0o600)
            );
            assert_eq!(
                owner_mode(&a.home.join(".claude/creds")).0,
                a.unix_uid,
                "copied home entries belong to the principal"
            );
            // R-10 + the archive: root-only, read-only, holding the store.
            assert_eq!(owner_mode(&pre.archive), (0, 0, 0o500));
            let (u, g, m) = owner_mode(&pre.archive.join("access.db"));
            assert_eq!((u, g, m & 0o277), (0, 0, 0));
            assert!(!data.join("access.db").exists());
            // The principal's child could open its db: the uid owns the dir chain.
            assert_eq!(owner_mode(&data).0, a.unix_uid);
        }

        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_adopt_t0_rename_of_a_non_root_install_satisfies_i9() {
            require_root();
            let name = "ik-t1root-vps";
            let _cleanup = HostUser("ik-t1root-vps");
            let (tmp, root) = enforced_root();
            let (from, home) = t0_install(tmp.path()).await;
            let uid = 28_080;
            crate::server::operator::etc_files::EtcFiles::system()
                .add_user(&crate::server::operator::etc_files::NewUser {
                    name,
                    uid,
                    gid: uid,
                    gecos: "t1root",
                    home: &home,
                    shell: Path::new("/bin/sh"),
                    own_group: true,
                })
                .unwrap();
            // The T0 daemon ran as that user: it owned its data and home.
            chown_tree(&from, uid);
            chown_tree(&home, uid);
            let prov = Provisioner::new(
                root.clone(),
                UidRange::new(28_090, 28_099).unwrap(),
                ProvisioningMode::Auto,
                Actor::Cli,
            );
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let pre = preflight(&root, &from, &home, "20261001T000011Z").unwrap();
            let a = prov
                .create(&pool, "t1root-vps", PW, true, Some(Adopt::user(name)))
                .await
                .unwrap();
            assert!(a.adopted);
            let report = migrate(&root, Ownership::Enforce, &a, &pre).await.unwrap();
            assert_eq!(report.mode, Mode::AdoptRename);
            let pdir = root.principal_dir(a.principal_id);
            let v = i9_violations(&pdir, uid, uid, true).unwrap();
            assert!(v.is_empty(), "I-9: {v:#?}");
            assert_eq!(owner_mode(&pdir), (uid, uid, 0o700));
            assert_eq!(owner_mode(&pre.archive), (0, 0, 0o500));
            assert_eq!(owner_mode(&pre.archive.join("daemon.json")).0, 0);
            // The adopted home is untouched (still the user's, still 0755-ish).
            assert_eq!(owner_mode(&home.join("Documents/private")).0, uid);
        }
    }
}
