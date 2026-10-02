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
//!   and not a mount point ([`Mode::AdoptRename`]), else — or when that
//!   rename fails with `EXDEV`/`EBUSY` (a bind mount) — by copy + verify
//!   ([`Mode::AdoptCopy`]). Run it from a root session that is not a login
//!   of the adopted user: that user's processes are all killed first.
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
//! * the old dir and the principal's dir are both held root-owned `0700`
//!   while the migration runs, and for an adopted user every process of
//!   its uid is killed first (§7.3), so nothing of that uid can reach into
//!   either half-way. Every walk root makes over the old tree goes through
//!   held directory fds and never follows a symlink ([`super::safe_fs`]);
//!   an adopted tree with a hard-linked file or a mount point inside is
//!   refused before anything moves (review S4-1);
//! * everything moved in is chowned to the principal and stripped of
//!   group/other bits, then checked against **I-9**;
//! * a failure after the data has moved still seals the archive and writes
//!   a partial report, and the error names both;
//! * a migration report lists what moved, what was rewritten, and every
//!   `ikenga.db` column that still holds a path under the old home or old
//!   data dir. Those are **not** rewritten (§11.2).
//!
//! Chi runs that were live under T0 are reconciled as `Unverified` by the
//! existing sweep: the pid probe now runs as a different uid (§11.2).

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection, Row};

use super::accounts::Account;
use super::provision::{ReapOutcome, UidReaper};
use super::safe_fs::{self, Dir, Kind, Stat};
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
    /// Set when the migration failed after the data had moved: the report
    /// is then partial (review S4-4).
    pub error: Option<String>,
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
/// when there is no file or the pid is gone; an unreadable file (or one
/// that is not a regular file) is an error (we can't tell, so we refuse).
pub fn t0_daemon_alive(from: &Path) -> anyhow::Result<Option<u32>> {
    let path = from.join("daemon.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        Ok(m) if !m.file_type().is_file() => {
            anyhow::bail!("{} is not a regular file; remove it", path.display())
        }
        Ok(_) => {}
    }
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
    };
    daemon_pid_alive(&text, &path)
}

/// [`t0_daemon_alive`] through the held old dir.
fn t0_daemon_alive_in(dir: &Dir, from: &Path) -> anyhow::Result<Option<u32>> {
    let path = from.join("daemon.json");
    let mut text = String::new();
    match dir.open_file(os("daemon.json")) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{}", path.display())),
        Ok((_, st)) if st.kind != Kind::File => {
            anyhow::bail!("{} is not a regular file; remove it", path.display())
        }
        Ok((mut f, _)) => {
            f.read_to_string(&mut text)
                .with_context(|| format!("{}", path.display()))?;
        }
    }
    daemon_pid_alive(&text, &path)
}

fn daemon_pid_alive(text: &str, path: &Path) -> anyhow::Result<Option<u32>> {
    let json: serde_json::Value = serde_json::from_str(text).with_context(|| {
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
//
// Root walks trees that an adopted T0 user owned, so every walk below goes
// through `safe_fs` (held directory fds, `O_NOFOLLOW`, changes made through
// the fd that was checked) — never a path that a racing rename could turn
// into a symlink (review S4-1). Plain paths are only used on the principal's
// side, under `principals/<id>`, which is held root-owned for the whole
// migration.

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

fn os(name: &str) -> &OsStr {
    OsStr::new(name)
}

fn other(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::Other, msg)
}

/// Prefix an error with the path it is about.
fn at_path(path: &Path) -> impl Fn(io::Error) -> io::Error + '_ {
    move |e| io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

fn changed(path: &Path) -> io::Error {
    other(format!(
        "{}: changed while adopt-t0 was working on it",
        path.display()
    ))
}

fn hard_linked(path: &Path, nlink: u64) -> io::Error {
    other(format!(
        "{}: has {nlink} hard links, so it may also be named outside the tree; adopt-t0 \
         won't carry or change it. Break the link (`cp -p f f.new && mv f.new f`) and run it \
         again",
        path.display()
    ))
}

fn mount_inside(path: &Path) -> io::Error {
    other(format!(
        "{}: is a mount point; adopt-t0 doesn't cross into another filesystem",
        path.display()
    ))
}

/// Fill `buf` from `r` as far as it goes; `Ok(n < buf.len())` only at EOF.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Byte-for-byte equality of two readers.
fn readers_equal(a: &mut impl Read, b: &mut impl Read) -> io::Result<bool> {
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let (n, m) = (read_full(a, &mut ba)?, read_full(b, &mut bb)?);
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Copy the open regular file `src` to the new entry `dst_name` of `dst`
/// (never overwriting, never through a symlink) and verify it. `at` names
/// the source, for messages.
fn copy_file_verified(
    mut src: File,
    src_st: &Stat,
    dst: &Dir,
    dst_name: &OsStr,
    at: &Path,
) -> io::Result<u64> {
    let bytes = {
        let mut writer = dst.create_file(dst_name, 0o600)?;
        let n = io::copy(&mut src, &mut writer)?;
        writer.sync_all()?;
        writer.set_permissions(fs::Permissions::from_mode((src_st.mode & 0o700) | 0o600))?;
        n
    };
    src.seek(SeekFrom::Start(0))?;
    let (mut back, back_st) = dst.open_file(dst_name)?;
    if back_st.kind != Kind::File || !readers_equal(&mut src, &mut back)? {
        return Err(other(format!(
            "verify failed: the copy of {} differs from it",
            at.display()
        )));
    }
    Ok(bytes)
}

#[derive(Debug, Default)]
struct CopyStats {
    files: u64,
    bytes: u64,
    special: Vec<String>,
}

/// Copy the entry `name` of `src` to the new entry `dst_name` of `dst`
/// without following symlinks (they are recreated as symlinks), verifying
/// every file. Sockets, FIFOs and devices are listed, not copied. With
/// `refuse_links`, a file or symlink with a second hard link is an error:
/// its content may be someone else's file. `at` is `src/name`, for
/// messages.
fn copy_entry(
    src: &Dir,
    name: &OsStr,
    dst: &Dir,
    dst_name: &OsStr,
    at: &Path,
    refuse_links: bool,
    stats: &mut CopyStats,
) -> io::Result<()> {
    let st = src.stat_at(name).map_err(at_path(at))?;
    match st.kind {
        Kind::Symlink => {
            if refuse_links && st.nlink > 1 {
                return Err(hard_linked(at, st.nlink));
            }
            let target = src.read_link(name).map_err(at_path(at))?;
            dst.symlink(&target, dst_name).map_err(at_path(at))?;
        }
        Kind::Dir => {
            let sub = src.open_dir(name).map_err(at_path(at))?;
            if !sub.stat()?.same_inode(&st) {
                return Err(changed(at));
            }
            let dsub = dst.mkdir(dst_name, 0o700).map_err(at_path(at))?;
            copy_dir_contents(&sub, at, &dsub, &[], refuse_links, stats)?;
        }
        Kind::File => {
            let (file, fst) = src.open_file(name).map_err(at_path(at))?;
            if fst.kind != Kind::File || !fst.same_inode(&st) {
                return Err(changed(at));
            }
            if refuse_links && fst.nlink > 1 {
                return Err(hard_linked(at, fst.nlink));
            }
            stats.bytes +=
                copy_file_verified(file, &fst, dst, dst_name, at).map_err(at_path(at))?;
            stats.files += 1;
        }
        Kind::Other => stats.special.push(at.display().to_string()),
    }
    Ok(())
}

/// [`copy_entry`] for every entry of `src` except the `skip` names.
fn copy_dir_contents(
    src: &Dir,
    at: &Path,
    dst: &Dir,
    skip: &[&str],
    refuse_links: bool,
    stats: &mut CopyStats,
) -> io::Result<()> {
    for name in src.entries().map_err(at_path(at))? {
        if skip.iter().any(|s| name == **s) {
            continue;
        }
        copy_entry(src, &name, dst, &name, &at.join(&name), refuse_links, stats)?;
    }
    Ok(())
}

/// Before anything moves: nothing inside the source is a mount point, and —
/// with `refuse_links` (the tree is not root's) — no file or symlink in it
/// has a second hard link. One line per problem.
fn source_problems(
    dir: &Dir,
    at: &Path,
    dev: u64,
    refuse_links: bool,
    out: &mut Vec<String>,
) -> io::Result<()> {
    for name in dir.entries().map_err(at_path(at))? {
        let p = at.join(&name);
        let st = dir.stat_at(&name).map_err(at_path(&p))?;
        if st.dev != dev {
            out.push(format!("{}: a mount point", p.display()));
            continue;
        }
        match st.kind {
            Kind::Dir => {
                let sub = dir.open_dir(&name).map_err(at_path(&p))?;
                source_problems(&sub, &p, dev, refuse_links, out)?;
            }
            Kind::File | Kind::Symlink if refuse_links && st.nlink > 1 => {
                out.push(format!("{}: {} hard links", p.display(), st.nlink));
            }
            _ => {}
        }
    }
    Ok(())
}

/// I-9 for a migrated tree: everything under `dir` (itself included) is
/// chowned to `owner` and stripped of group/other and set-id bits;
/// directories become `0700`. Every change goes through the fd that was
/// checked. It never crosses into another filesystem and never changes an
/// entry with a second hard link (that would change its other names too):
/// both are errors.
fn normalize_tree(
    dir: &Dir,
    at: &Path,
    owner: (u32, u32),
    ownership: Ownership,
    dev: u64,
) -> io::Result<()> {
    let enforce = ownership == Ownership::Enforce;
    for name in dir.entries().map_err(at_path(at))? {
        let p = at.join(&name);
        let st = dir.stat_at(&name).map_err(at_path(&p))?;
        if st.dev != dev {
            return Err(mount_inside(&p));
        }
        if st.kind == Kind::Dir {
            let sub = dir.open_dir(&name).map_err(at_path(&p))?;
            if !sub.stat()?.same_inode(&st) {
                return Err(changed(&p));
            }
            normalize_tree(&sub, &p, owner, ownership, dev)?;
            continue;
        }
        let (fd, fst) = dir.open_path(&name).map_err(at_path(&p))?;
        if !fst.same_inode(&st) {
            return Err(changed(&p));
        }
        if fst.nlink > 1 {
            return Err(hard_linked(&p, fst.nlink));
        }
        if fst.kind != Kind::Symlink {
            safe_fs::chmod_fd(fd.as_raw_fd(), (fst.mode & 0o700) | 0o600).map_err(at_path(&p))?;
        }
        if enforce {
            safe_fs::chown_fd(fd.as_raw_fd(), owner.0, owner.1).map_err(at_path(&p))?;
        }
    }
    dir.chmod(0o700).map_err(at_path(at))?;
    if enforce {
        dir.chown(owner.0, owner.1).map_err(at_path(at))?;
    }
    Ok(())
}

/// Make the contents of `dir` read-only and root-only, **top-down**: each
/// directory is made root-owned `0500` before anything in it is touched;
/// files lose every write and group/other bit, owner root under
/// [`Ownership::Enforce`]. A mount point inside, or an entry with a second
/// hard link, is left as it was and listed in `skipped` (sealing it would
/// change what is mounted there, or the entry's other names).
fn seal_contents(
    dir: &Dir,
    at: &Path,
    ownership: Ownership,
    dev: u64,
    skipped: &mut Vec<String>,
) -> io::Result<()> {
    let enforce = ownership == Ownership::Enforce;
    for name in dir.entries().map_err(at_path(at))? {
        let p = at.join(&name);
        let st = dir.stat_at(&name).map_err(at_path(&p))?;
        if st.dev != dev {
            skipped.push(format!("{}: a mount point, not sealed", p.display()));
            continue;
        }
        if st.kind == Kind::Dir {
            let sub = dir.open_dir(&name).map_err(at_path(&p))?;
            if !sub.stat()?.same_inode(&st) {
                return Err(changed(&p));
            }
            seal_tree(&sub, &p, ownership, dev, skipped)?;
            continue;
        }
        let (fd, fst) = dir.open_path(&name).map_err(at_path(&p))?;
        if !fst.same_inode(&st) {
            return Err(changed(&p));
        }
        if fst.nlink > 1 {
            skipped.push(format!(
                "{}: {} hard links, not sealed",
                p.display(),
                fst.nlink
            ));
            continue;
        }
        if enforce {
            safe_fs::chown_fd(fd.as_raw_fd(), 0, 0).map_err(at_path(&p))?;
        }
        if fst.kind != Kind::Symlink {
            safe_fs::chmod_fd(fd.as_raw_fd(), (fst.mode & 0o500) | 0o400).map_err(at_path(&p))?;
        }
    }
    Ok(())
}

/// [`seal_contents`] of `dir`, after making `dir` itself root-owned `0500`
/// (top-down: from then on no other uid can look anything up in it).
fn seal_tree(
    dir: &Dir,
    at: &Path,
    ownership: Ownership,
    dev: u64,
    skipped: &mut Vec<String>,
) -> io::Result<()> {
    if ownership == Ownership::Enforce {
        dir.chown(0, 0).map_err(at_path(at))?;
    }
    dir.chmod(0o500).map_err(at_path(at))?;
    seal_contents(dir, at, ownership, dev, skipped)
}

/// I-9: every path under `dir` (itself included) is owned by `uid:gid`
/// (when `check_owner`) and carries no group/other or set-id bit. Returns
/// one line per violation. Walks through fds, never following a symlink.
pub fn i9_violations(dir: &Path, uid: u32, gid: u32, check_owner: bool) -> io::Result<Vec<String>> {
    fn check(p: &Path, st: &Stat, uid: u32, gid: u32, check_owner: bool, out: &mut Vec<String>) {
        if check_owner && (st.uid, st.gid) != (uid, gid) {
            out.push(format!(
                "{}: owned by {}:{}, not {uid}:{gid}",
                p.display(),
                st.uid,
                st.gid
            ));
        }
        if st.kind != Kind::Symlink && st.mode & 0o6077 != 0 {
            out.push(format!(
                "{}: mode {:o} has group/other or set-id bits",
                p.display(),
                st.mode
            ));
        }
    }
    fn walk(
        dir: &Dir,
        at: &Path,
        uid: u32,
        gid: u32,
        check_owner: bool,
        out: &mut Vec<String>,
    ) -> io::Result<()> {
        for name in dir.entries().map_err(at_path(at))? {
            let p = at.join(&name);
            let st = dir.stat_at(&name).map_err(at_path(&p))?;
            check(&p, &st, uid, gid, check_owner, out);
            if st.kind == Kind::Dir {
                let sub = dir.open_dir(&name).map_err(at_path(&p))?;
                walk(&sub, &p, uid, gid, check_owner, out)?;
            }
        }
        Ok(())
    }
    let canonical = fs::canonicalize(dir).map_err(at_path(dir))?;
    let top = Dir::open_no_symlinks(&canonical)?;
    let mut out = Vec::new();
    check(&canonical, &top.stat()?, uid, gid, check_owner, &mut out);
    walk(&top, &canonical, uid, gid, check_owner, &mut out)?;
    Ok(out)
}

/// Holds a directory root-owned `0700` while the migration runs, so no
/// process of another uid can start a lookup in it (one already inside
/// through an open fd or cwd is what the uid-wide kill is for). On drop it
/// gets back the owner and mode it had, unless [`Hold::release`]d because
/// the directory was moved into place or archived.
struct Hold {
    dir: Dir,
    restore: Option<(u32, u32, u32)>,
    ownership: Ownership,
    path: PathBuf,
}

impl Hold {
    fn take(dir: Dir, path: &Path, ownership: Ownership) -> io::Result<Self> {
        let st = dir.stat()?;
        if ownership == Ownership::Enforce {
            dir.chown(0, 0)?;
        }
        dir.chmod(0o700)?;
        Ok(Self {
            dir,
            restore: Some((st.uid, st.gid, st.mode)),
            ownership,
            path: path.to_path_buf(),
        })
    }

    fn dir(&self) -> &Dir {
        &self.dir
    }

    fn release(&mut self) {
        self.restore = None;
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let Some((uid, gid, mode)) = self.restore else {
            return;
        };
        let owner = if self.ownership == Ownership::Enforce {
            self.dir.chown(uid, gid)
        } else {
            Ok(())
        };
        if let Err(e) = owner.and_then(|()| self.dir.chmod(mode)) {
            tracing::error!(
                "adopt-t0: could not give {} back its owner {uid}:{gid} and mode {mode:o}: {e}",
                self.path.display()
            );
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
    if exists_no_follow(&tmp) {
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

/// The old data dir, held open: its parent, its name there, the dir itself
/// and what it was when opened.
struct Source<'a> {
    parent: &'a Dir,
    name: &'a OsStr,
    dir: &'a Dir,
    st: Stat,
    path: &'a Path,
}

/// The archive-only files of the source by copy (the archive is on another
/// filesystem): copy and verify **all** of them first, and only then delete
/// them from the source (R-10). A failed copy removes the half-made archive
/// and leaves the source as it was. Returns the archive, the names moved,
/// and notes for any source file that could not be removed afterwards.
fn archive_by_copy(
    src: &Source<'_>,
    archive_name: &OsStr,
    archive_path: &Path,
    refuse_links: bool,
) -> io::Result<(Dir, Vec<String>, Vec<String>)> {
    let archive = src
        .parent
        .mkdir(archive_name, 0o700)
        .map_err(at_path(archive_path))?;
    let mut copied: Vec<&str> = Vec::new();
    for name in ARCHIVE_ONLY {
        let at = src.path.join(name);
        let result = match src.dir.try_stat_at(os(name)) {
            Ok(None) => continue,
            Ok(Some(st)) if !matches!(st.kind, Kind::File | Kind::Symlink) => {
                Err(other(format!("{}: not a file", at.display())))
            }
            Ok(Some(_)) => copy_entry(
                src.dir,
                os(name),
                &archive,
                os(name),
                &at,
                refuse_links,
                &mut CopyStats::default(),
            ),
            Err(e) => Err(at_path(&at)(e)),
        };
        if let Err(e) = result {
            for done in copied.iter().chain([&name]) {
                let _ = archive.unlink(os(done));
            }
            let _ = src.parent.rmdir(archive_name);
            return Err(e);
        }
        copied.push(name);
    }
    let mut notes = Vec::new();
    for name in &copied {
        if let Err(e) = src.dir.unlink(os(name)) {
            notes.push(format!(
                "{name} is archived but could not be removed from {}: {e}; remove it by hand",
                src.path.display()
            ));
        }
    }
    Ok((
        archive,
        copied.iter().map(|n| n.to_string()).collect(),
        notes,
    ))
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

/// `SQLITE_DBCONFIG_DEFENSIVE` (sqlite3.h).
const SQLITE_DBCONFIG_DEFENSIVE: libc::c_int = 1010;

extern "C" {
    // The bundled SQLite that sqlx links (libsqlite3-sys); declared here so
    // the scan can turn on defensive mode without a direct dependency.
    fn sqlite3_db_config(db: *mut libc::c_void, op: libc::c_int, ...) -> libc::c_int;
}

/// The files SQLite may open beside `ikenga.db`. All of them must be plain,
/// singly-linked files (or absent) before root lets SQLite open the
/// database: SQLite follows symlinks and opens `-shm` read-write.
const DB_FILES: [&str; 4] = [
    "ikenga.db",
    "ikenga.db-wal",
    "ikenga.db-shm",
    "ikenga.db-journal",
];

/// Whether `data/ikenga.db` (and its companions) may be opened by root:
/// `Ok(false)` when there is no database. An `Err` names what is wrong.
fn db_files_safe(data: &Dir) -> Result<bool, String> {
    let mut any = false;
    for (i, name) in DB_FILES.iter().enumerate() {
        match data.try_stat_at(os(name)) {
            Ok(None) if i == 0 => return Ok(false),
            Ok(None) => {}
            Ok(Some(st)) if st.kind == Kind::File && st.nlink == 1 => any = true,
            Ok(Some(_)) => {
                return Err(format!(
                    "{name} is not a plain, singly-linked file; the database was not opened"
                ))
            }
            Err(e) => return Err(format!("{name}: {e}")),
        }
    }
    Ok(any)
}

/// Every text column of every ordinary table in `db` holding a value under
/// one of `prefixes` (`(label, path)`): equal to the path, or containing
/// `<path>/` anywhere (JSON blobs included).
///
/// The database was written by the adopted uid and root opens it (review
/// S4-5), so: read-only and `query_only`; `trusted_schema = OFF` (no SQL
/// function from the schema runs — views, triggers, generated columns); and
/// `SQLITE_DBCONFIG_DEFENSIVE`. Virtual tables (their module code would run)
/// and hidden or generated columns are skipped.
async fn scan_db_paths(
    db: &Path,
    prefixes: &[(&'static str, &Path)],
) -> anyhow::Result<Vec<DbPathHit>> {
    let mut conn = SqliteConnectOptions::new()
        .filename(db)
        .read_only(true)
        .create_if_missing(false)
        .foreign_keys(false)
        .pragma("trusted_schema", "OFF")
        .pragma("query_only", "ON")
        .connect()
        .await?;
    {
        let mut handle = conn.lock_handle().await?;
        let raw = handle.as_raw_handle().as_ptr().cast::<libc::c_void>();
        // SAFETY: a live connection handle, held locked; DEFENSIVE takes an
        // int (1 = on) and an optional int* for the new setting.
        let rc = unsafe {
            sqlite3_db_config(
                raw,
                SQLITE_DBCONFIG_DEFENSIVE,
                1 as libc::c_int,
                std::ptr::null_mut::<libc::c_int>(),
            )
        };
        if rc != 0 {
            anyhow::bail!("could not enable SQLite defensive mode (rc {rc})");
        }
    }
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_list WHERE schema = 'main' AND type = 'table' AND name \
         NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&mut conn)
    .await?;
    let mut hits = Vec::new();
    for table in tables {
        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_xinfo(?) WHERE hidden = 0 ORDER BY cid",
        )
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

/// The copy is built here, beside `data/`, then renamed onto it.
const STAGING: &str = "data.t0-adopt";

/// Pick the [`Mode`] for `account` from the `fstat`s of the held source,
/// its parent and the principal's dir.
fn choose_mode(
    account: &Account,
    pre: &Preflight,
    (src, src_parent, principal_dir): (&Stat, &Stat, &Stat),
) -> anyhow::Result<Mode> {
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
    // Same st_dev isn't proof a rename works (a bind mount of the same
    // filesystem shares it): `move_by_rename` falls back to a copy on EXDEV.
    let mount_point = src.dev != src_parent.dev;
    let same_fs = src.dev == principal_dir.dev;
    Ok(if same_fs && !mount_point {
        Mode::AdoptRename
    } else {
        Mode::AdoptCopy
    })
}

/// Adopt modes: stop every process of the adopted uid (§7.3's uid-wide
/// kill) once both trees are held, so none is still inside the old dir
/// through an open fd or cwd the holds can't revoke (review S4-1).
fn stop_adopted_uid(
    reaper: &dyn UidReaper,
    account: &Account,
    ownership: Ownership,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    let uid = account.unix_uid;
    if ownership == Ownership::Enforce {
        if let Some(pid) = super::reaper::ancestor_holding_uid(uid)? {
            anyhow::bail!(
                "this command runs under process {pid} of uid {uid} ({}), the user being adopted. \
                 adopt-t0 first kills every process of that uid, which would end this session \
                 half-way. Run it from a root login, or outside this session: `sudo systemd-run \
                 --wait --pipe --collect ikenga-server accounts adopt-t0 …`. Nothing was changed",
                account.unix_name
            );
        }
    }
    match reaper.kill_all(&account.principal())? {
        ReapOutcome::Killed => report.notes.push(format!(
            "every process of uid {uid} was killed before the old dir was touched (§7.3)"
        )),
        ReapOutcome::Unavailable(why) if ownership != Ownership::Enforce => report.notes.push(
            format!("the processes of uid {uid} were not stopped: {why}"),
        ),
        ReapOutcome::Unavailable(why) => {
            anyhow::bail!("can't stop the processes of uid {uid} ({why}); nothing was moved")
        }
        ReapOutcome::Failed(why) => anyhow::bail!(
            "could not stop every process of uid {uid}, which owns the old dir ({why}); nothing \
             was moved"
        ),
    }
    Ok(())
}

/// What [`move_by_rename`] did.
enum Renamed {
    /// Moved; the archive dir holds the archive-only files.
    Done(Dir),
    /// The rename crossed a mount (`EXDEV`/`EBUSY`): everything was undone.
    CrossDevice(io::Error),
}

/// [`Mode::AdoptRename`]: archive-only files to a new archive dir, then the
/// whole old dir onto `data/` in one rename. `Err` and
/// [`Renamed::CrossDevice`] leave everything as it was.
fn move_by_rename(
    src: &Source<'_>,
    pdir: &Dir,
    data: &Path,
    pre: &Preflight,
    (uid, gid, ownership): (u32, u32, Ownership),
    report: &mut MigrationReport,
) -> anyhow::Result<Renamed> {
    let archive_name = pre.archive.file_name().context("archive path")?;
    let archive = src
        .parent
        .mkdir(archive_name, 0o700)
        .with_context(|| format!("{}", pre.archive.display()))?;
    let mut moved: Vec<&str> = Vec::new();
    let undo = |moved: &[&str]| {
        for name in moved {
            let _ = archive.rename(os(name), src.dir, os(name));
        }
        let _ = src.parent.rmdir(archive_name);
    };
    for name in ARCHIVE_ONLY {
        let step = match src.dir.try_stat_at(os(name)) {
            Ok(None) => continue,
            Ok(Some(_)) => src.dir.rename(os(name), &archive, os(name)),
            Err(e) => Err(e),
        };
        if let Err(e) = step {
            undo(&moved);
            // The old dir is its own mount (a bind mount): nothing renames
            // out of it.
            if is_cross_device(&e) {
                return Ok(Renamed::CrossDevice(e));
            }
            return Err(e).with_context(|| format!("archiving {name}"));
        }
        moved.push(name);
    }
    if let Err(e) = remove_fresh_data(data) {
        restore_fresh_data(data, uid, gid, ownership);
        undo(&moved);
        return Err(e).with_context(|| format!("{}", data.display()));
    }
    if let Err(e) = src.parent.rename(src.name, pdir, os("data")) {
        restore_fresh_data(data, uid, gid, ownership);
        undo(&moved);
        if is_cross_device(&e) {
            return Ok(Renamed::CrossDevice(e));
        }
        return Err(e)
            .with_context(|| format!("renaming {} onto {}", pre.from.display(), data.display()));
    }
    // What landed is the held dir, not something renamed into its place.
    if !matches!(pdir.stat_at(os("data")), Ok(st) if st.same_inode(&src.st)) {
        let _ = pdir.rename(os("data"), src.parent, src.name);
        restore_fresh_data(data, uid, gid, ownership);
        undo(&moved);
        anyhow::bail!("{} was replaced during the migration", pre.from.display());
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
    Ok(Renamed::Done(archive))
}

/// Copy the [`HOME_ENTRIES`] of the old home into `new_home` (a fresh
/// principal's). Symlinks are followed only in an old home owned by root
/// (the root/Docker case, where a dot-dir may be a volume): elsewhere the
/// home's owner could point one at anything root can read.
fn copy_home_entries(
    pre: &Preflight,
    new_home: &Path,
    created: &mut Vec<PathBuf>,
    stats: &mut CopyStats,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    // Root's own home in production (this runs as root); the test user's
    // in unprivileged tests.
    let old_home_is_ours = Dir::open_no_symlinks(&pre.old_home)
        .and_then(|d| d.stat())
        .with_context(|| format!("{}", pre.old_home.display()))?
        .uid
        == super::sys::geteuid();
    let home_canonical =
        fs::canonicalize(new_home).with_context(|| format!("{}", new_home.display()))?;
    for entry in HOME_ENTRIES {
        let src_path = pre.old_home.join(entry);
        if !exists_no_follow(&src_path) {
            continue;
        }
        let dst_path = new_home.join(entry);
        if exists_no_follow(&dst_path) {
            report.home_entries_skipped.push(entry.to_string());
            continue;
        }
        // A symlinked entry (Docker mounts `/root/.claude`) is copied as
        // what it points at, not as the link — the link would point the
        // principal at root's files.
        let target = if old_home_is_ours {
            let target =
                fs::canonicalize(&src_path).with_context(|| format!("{}", src_path.display()))?;
            if target != src_path {
                report.notes.push(format!(
                    "{} is a symlink; copied its target {}",
                    src_path.display(),
                    target.display()
                ));
            }
            target
        } else {
            src_path.clone()
        };
        let (Some(parent), Some(name)) = (target.parent(), target.file_name()) else {
            continue;
        };
        let not_followed = |why: String, report: &mut MigrationReport| {
            report.notes.push(format!(
                "{} was not copied: {why} (a symlink in a home root doesn't own is never \
                 followed)",
                src_path.display()
            ))
        };
        let parent_dir = match Dir::open_no_symlinks(parent) {
            Ok(dir) => dir,
            Err(e) if !old_home_is_ours => {
                not_followed(e.to_string(), report);
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if !old_home_is_ours && parent_dir.stat_at(name)?.kind == Kind::Symlink {
            not_followed("it is a symlink".into(), report);
            continue;
        }
        // `.local/share/app.ikenga`: create the missing parents, and
        // remember the outermost one this run created.
        let comps: Vec<&OsStr> = Path::new(entry).iter().collect();
        let (last, parents) = comps.split_last().context("empty home entry")?;
        let mut dst_dir = Dir::open_no_symlinks(&home_canonical)?;
        let mut dst_at = home_canonical.clone();
        let mut first_created = None;
        for comp in parents {
            dst_at.push(comp);
            dst_dir = if dst_dir.try_stat_at(comp)?.is_some() {
                dst_dir.open_dir(comp).map_err(at_path(&dst_at))?
            } else {
                let made = dst_dir.mkdir(comp, 0o700).map_err(at_path(&dst_at))?;
                first_created.get_or_insert_with(|| {
                    new_home.join(dst_at.strip_prefix(&home_canonical).unwrap_or(&dst_at))
                });
                made
            };
        }
        let result = copy_entry(
            &parent_dir,
            name,
            &dst_dir,
            last,
            &src_path,
            !old_home_is_ours,
            stats,
        );
        created.push(first_created.unwrap_or_else(|| dst_path.clone()));
        result.with_context(|| format!("copying {}", src_path.display()))?;
        report.home_entries_copied.push(entry.to_string());
    }
    Ok(())
}

/// [`Mode::AdoptCopy`] / [`Mode::Copy`]: copy + verify into a staging dir
/// beside `data/` (and, for a fresh principal, the home dot-dirs), then
/// archive the old dir. `Err` leaves everything as it was. `Ok(None)`: the
/// old dir itself became the archive; `Ok(Some(dir))`: the old dir is a
/// mount point and stays (sealed) in place, `dir` is the new archive. The
/// caller swaps the staging dir onto `data/` ([`swap_in_staging`]).
#[allow(clippy::too_many_arguments)]
fn move_by_copy(
    src: &Source<'_>,
    pdir: &Dir,
    principal_dir: &Path,
    new_home: Option<&Path>,
    pre: &Preflight,
    refuse_links: bool,
    ownership: Ownership,
    report: &mut MigrationReport,
) -> anyhow::Result<Option<Dir>> {
    let staging_path = principal_dir.join(STAGING);
    if exists_no_follow(&staging_path) {
        // A failed earlier run; the principal dir is held, nothing else wrote it.
        fs::remove_dir_all(&staging_path)?;
    }
    let mut stats = CopyStats::default();
    // Top-level paths this run created in the new home, for the undo.
    let mut created_in_home: Vec<PathBuf> = Vec::new();
    let undo = |created: &[PathBuf]| {
        let _ = fs::remove_dir_all(&staging_path);
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

    let copied = pdir
        .mkdir(os(STAGING), 0o700)
        .map_err(at_path(&staging_path))
        .and_then(|staging| {
            copy_dir_contents(
                src.dir,
                &pre.from,
                &staging,
                &ARCHIVE_ONLY,
                refuse_links,
                &mut stats,
            )
        });
    if let Err(e) = copied {
        undo(&created_in_home);
        return Err(e).with_context(|| format!("copying {}", pre.from.display()));
    }
    if let Some(new_home) = new_home {
        if let Err(e) = copy_home_entries(pre, new_home, &mut created_in_home, &mut stats, report) {
            undo(&created_in_home);
            return Err(e);
        }
    }

    // The archive: the old dir itself when it can be renamed.
    let archive_name = pre.archive.file_name().context("archive path")?;
    let archive = match src.parent.rename(src.name, src.parent, archive_name) {
        Ok(()) => {
            if !matches!(src.parent.stat_at(archive_name), Ok(st) if st.same_inode(&src.st)) {
                let _ = src.parent.rename(archive_name, src.parent, src.name);
                undo(&created_in_home);
                anyhow::bail!("{} was replaced during the migration", pre.from.display());
            }
            report.archive_is_old_dir = true;
            report.access_store_archived = ACCESS_STORE_FILES
                .iter()
                .filter(|n| matches!(src.dir.try_stat_at(os(n)), Ok(Some(_))))
                .map(|n| n.to_string())
                .collect();
            None
        }
        Err(e) if is_cross_device(&e) => {
            // A mount point (a Docker volume): it stays, read-only; the
            // archive-only files leave it by verified copy (R-10).
            let (archive, moved, notes) =
                match archive_by_copy(src, archive_name, &pre.archive, refuse_links) {
                    Ok(done) => done,
                    Err(e) => {
                        undo(&created_in_home);
                        return Err(e).with_context(|| {
                            format!("archiving the access store into {}", pre.archive.display())
                        });
                    }
                };
            report.access_store_archived = moved
                .into_iter()
                .filter(|n| ACCESS_STORE_FILES.contains(&n.as_str()))
                .collect();
            report.notes.extend(notes);
            report.old_dir_left_in_place = true;
            report.notes.push(format!(
                "{} could not be renamed (a mount point); it stays in place, read-only. The \
                 archive {} is on the parent filesystem — keep a copy if that is not persistent",
                pre.from.display(),
                pre.archive.display()
            ));
            let mut skipped = Vec::new();
            if let Err(e) = seal_tree(src.dir, &pre.from, ownership, src.st.dev, &mut skipped) {
                report.notes.push(format!(
                    "could not make {} read-only: {e}",
                    pre.from.display()
                ));
            }
            report.notes.extend(skipped);
            Some(archive)
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
    };
    report.files_copied = stats.files;
    report.bytes_copied = stats.bytes;
    report.special_files_skipped = stats.special;
    Ok(archive)
}

/// After a copy: the staging dir onto `data/`.
fn swap_in_staging(
    pdir: &Dir,
    principal_dir: &Path,
    data: &Path,
    pre: &Preflight,
    (uid, gid, ownership): (u32, u32, Ownership),
) -> anyhow::Result<()> {
    if let Err(e) =
        remove_fresh_data(data).and_then(|()| pdir.rename(os(STAGING), pdir, os("data")))
    {
        restore_fresh_data(data, uid, gid, ownership);
        return Err(e).with_context(|| {
            format!(
                "the copy is complete at {} and the old dir is archived at {}, but it could not \
                 be moved onto {}; move it by hand",
                principal_dir.join(STAGING).display(),
                pre.archive.display(),
                data.display()
            )
        });
    }
    Ok(())
}

/// `fs_roots.json` in the migrated data dir (§11.2), read and replaced
/// through `data`'s fd.
fn rewrite_fs_roots_in(
    data: &Dir,
    data_path: &Path,
    pre: &Preflight,
    new_home: Option<&Path>,
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    const NAME: &str = "fs_roots.json";
    const NEXT: &str = "fs_roots.json.t0-adopt";
    let Some(st) = data.try_stat_at(os(NAME))? else {
        return Ok(());
    };
    if st.kind != Kind::File || st.nlink != 1 {
        report.notes.push(format!(
            "{NAME} is not a plain, singly-linked file; not rewritten"
        ));
        return Ok(());
    }
    let mut text = String::new();
    let read = data.open_file(os(NAME)).and_then(|(mut f, fst)| {
        if !fst.same_inode(&st) {
            return Err(changed(&data_path.join(NAME)));
        }
        f.read_to_string(&mut text)
    });
    if let Err(e) = read {
        report.notes.push(format!("{NAME} was not rewritten: {e}"));
        return Ok(());
    }
    match rewrite_fs_roots(&text, &pre.old_home, new_home, &pre.from) {
        Ok(rw) => {
            if let Some(next) = &rw.text {
                let _ = data.unlink(os(NEXT));
                let mut f = data.create_file(os(NEXT), 0o600)?;
                f.write_all(next.as_bytes())?;
                f.sync_all()?;
                data.rename(os(NEXT), data, os(NAME))?;
            }
            report.fs_roots_rewritten = rw.rewritten;
            report.fs_roots_under_old_data_dir = rw.under_old_data_dir;
        }
        Err(e) => report
            .notes
            .push(format!("{NAME} was not rewritten: {e:#}")),
    }
    Ok(())
}

/// Everything after the data has moved: the staging swap, `tmp/`,
/// `fs_roots.json`, the `ikenga.db` scan and I-9's owners and modes. Runs
/// under the principal-dir hold.
#[allow(clippy::too_many_arguments)]
async fn finish(
    pdir: &Dir,
    principal_dir: &Path,
    data_path: &Path,
    staged: bool,
    new_home: Option<&Path>,
    pre: &Preflight,
    owner: (u32, u32, Ownership),
    report: &mut MigrationReport,
) -> anyhow::Result<()> {
    let (uid, gid, ownership) = owner;
    if staged {
        swap_in_staging(pdir, principal_dir, data_path, pre, owner)?;
    }
    let data = pdir
        .open_dir(os("data"))
        .with_context(|| format!("{}", data_path.display()))?;
    if data.try_stat_at(os("tmp"))?.is_none() {
        data.mkdir(os("tmp"), 0o700)?;
    }

    rewrite_fs_roots_in(&data, data_path, pre, new_home, report)?;

    match db_files_safe(&data) {
        Ok(false) => {}
        Err(why) => report.db_scan_error = Some(why),
        Ok(true) => {
            let mut prefixes: Vec<(&'static str, &Path)> =
                vec![("old_data_dir", pre.from.as_path())];
            if new_home.is_some() && pre.old_home != Path::new("/") {
                prefixes.insert(0, ("old_home", pre.old_home.as_path()));
            }
            match scan_db_paths(&data_path.join("ikenga.db"), &prefixes).await {
                Ok(hits) => report.db_paths = hits,
                Err(e) => report.db_scan_error = Some(format!("{e:#}")),
            }
        }
    }

    // Owned by the principal, private (I-9) — including anything the scan
    // above created as root (a `-shm`).
    normalize_tree(&data, data_path, (uid, gid), ownership, data.stat()?.dev)?;
    if let Some(home) = new_home {
        let canonical = fs::canonicalize(home)?;
        let dir = Dir::open_no_symlinks(&canonical)?;
        let dev = dir.stat()?.dev;
        normalize_tree(&dir, home, (uid, gid), ownership, dev)?;
    }
    Ok(())
}

/// The report into the archive (`0400`), then the archive sealed: its top
/// dir is root-owned `0700` first, its contents sealed top-down, the report
/// written, and the top made `0500` last.
fn seal_archive(
    archive: &Dir,
    pre: &Preflight,
    ownership: Ownership,
    report: &mut MigrationReport,
) -> io::Result<()> {
    if ownership == Ownership::Enforce {
        archive.chown(0, 0).map_err(at_path(&pre.archive))?;
    }
    archive.chmod(0o700).map_err(at_path(&pre.archive))?;
    let mut skipped = Vec::new();
    let sealed = archive
        .stat()
        .and_then(|st| seal_contents(archive, &pre.archive, ownership, st.dev, &mut skipped));
    report.notes.extend(skipped);
    if let Err(e) = &sealed {
        report
            .notes
            .push(format!("the archive could not be fully sealed: {e}"));
    }
    let report_path = pre.archive.join(REPORT_FILE);
    let json = serde_json::to_string_pretty(&*report).map_err(io::Error::other)?;
    let written = archive
        .create_file(os(REPORT_FILE), 0o400)
        .and_then(|mut f| f.write_all(json.as_bytes()))
        .map_err(at_path(&report_path));
    let top = archive.chmod(0o500).map_err(at_path(&pre.archive));
    sealed.and(written).and(top)
}

/// Migrate `pre.from` into `account`'s principal (see the module docs).
/// `ownership` is [`Ownership::Enforce`] in production; `reaper` stops the
/// adopted uid's processes (the CLI's is [`super::reaper::T1Reaper`]).
///
/// A failure before the data moves leaves everything as it was. A failure
/// after it still seals the archive and writes the (partial) report, and
/// the error names both (review S4-4).
pub async fn migrate(
    root: &OperatorRoot,
    ownership: Ownership,
    account: &Account,
    pre: &Preflight,
    reaper: &dyn UidReaper,
) -> anyhow::Result<MigrationReport> {
    if account.is_disabled() {
        anyhow::bail!("{} is disabled; enable it first", account.username);
    }
    let id = account.principal_id;
    let (uid, gid) = (account.unix_uid, account.unix_gid);
    let principal_dir = root.principal_dir(id);
    let data_path = root.principal_data(id);

    // The old dir, opened without following a symlink anywhere: `pre.from`
    // is canonical, so a symlink found on the way now was put there since.
    let from_name = pre.from.file_name().context("--from has no name")?;
    let from_parent = pre.from.parent().context("--from has no parent")?;
    let src_parent =
        Dir::open_no_symlinks(from_parent).with_context(|| format!("{}", from_parent.display()))?;
    let src_dir = src_parent
        .open_dir(from_name)
        .with_context(|| format!("{}", pre.from.display()))?;
    let src_st = src_dir.stat()?;
    let pdir = Dir::open_no_symlinks(
        &fs::canonicalize(&principal_dir)
            .with_context(|| format!("{}", principal_dir.display()))?,
    )?;
    let mode = choose_mode(account, pre, (&src_st, &src_parent.stat()?, &pdir.stat()?))?;
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
        data_dir: data_path.clone(),
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
        error: None,
    };

    // Hold both trees root-owned 0700: from here no other uid can start a
    // lookup in either. Then stop the adopted uid's processes, which may
    // already be inside the old dir.
    let mut src_hold = Hold::take(src_dir, &pre.from, ownership)
        .with_context(|| format!("{}", pre.from.display()))?;
    let principal_hold = Hold::take(pdir, &principal_dir, ownership)
        .with_context(|| format!("{}", principal_dir.display()))?;
    let src = Source {
        parent: &src_parent,
        name: from_name,
        dir: src_hold.dir(),
        st: src_st,
        path: &pre.from,
    };
    require_fresh_data(&data_path)?;
    // Again, under the hold: the daemon may have been started since.
    if let Some(pid) = t0_daemon_alive_in(src.dir, &pre.from)? {
        anyhow::bail!(
            "the T0 daemon of {} is running again (pid {pid})",
            pre.from.display()
        );
    }
    if mode != Mode::Copy {
        stop_adopted_uid(reaper, account, ownership, &mut report)?;
    }
    // A tree that isn't ours (root's, in production) may hold hard links to
    // files outside it. Checked after the kill: nothing can add one now, and
    // every later step re-checks the link count on the fd it changes.
    let refuse_links = mode != Mode::Copy || src_st.uid != super::sys::geteuid();
    let mut problems = Vec::new();
    source_problems(src.dir, &pre.from, src_st.dev, refuse_links, &mut problems)?;
    if !problems.is_empty() {
        anyhow::bail!(
            "{} can't be migrated as it is; nothing was moved:\n  {}\nUnmount (or move out) a \
             mount point inside it. A hard-linked file may also be named outside the tree, so \
             root won't carry or chown it: break the link (`cp -p f f.new && mv f.new f`) first",
            pre.from.display(),
            problems.join("\n  ")
        );
    }

    let owner = (uid, gid, ownership);
    let pdir = principal_hold.dir();
    let (archive, staged) = match mode {
        Mode::AdoptRename => match move_by_rename(&src, pdir, &data_path, pre, owner, &mut report)?
        {
            Renamed::Done(archive) => (Some(archive), false),
            Renamed::CrossDevice(e) => {
                // Same st_dev, but another mount (a bind mount): copy (S4-3).
                report.mode = Mode::AdoptCopy;
                report.notes.push(format!(
                    "{} could not be renamed into place ({e}: another mount of the same \
                     filesystem?), so it was copied instead",
                    pre.from.display()
                ));
                let archive = move_by_copy(
                    &src,
                    pdir,
                    &principal_dir,
                    None,
                    pre,
                    refuse_links,
                    ownership,
                    &mut report,
                )?;
                (archive, true)
            }
        },
        Mode::AdoptCopy | Mode::Copy => {
            let archive = move_by_copy(
                &src,
                pdir,
                &principal_dir,
                new_home.as_deref(),
                pre,
                refuse_links,
                ownership,
                &mut report,
            )?;
            (archive, true)
        }
    };
    // The data has moved: the old dir is data/, the archive, or sealed in
    // place. From here a failure still seals the archive and writes the
    // report.
    src_hold.release();

    let mut outcome = finish(
        pdir,
        &principal_dir,
        &data_path,
        staged,
        new_home.as_deref(),
        pre,
        owner,
        &mut report,
    )
    .await;
    // Hand the principal's dir back before checking I-9 over it.
    drop(principal_hold);
    if outcome.is_ok() {
        outcome = i9_violations(&principal_dir, uid, gid, ownership == Ownership::Enforce)
            .map_err(anyhow::Error::from)
            .and_then(|violations| {
                if violations.is_empty() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(
                        "I-9 does not hold under {} after the migration:\n  {}",
                        principal_dir.display(),
                        violations.join("\n  ")
                    ))
                }
            });
    }
    report.error = outcome.as_ref().err().map(|e| format!("{e:#}"));

    let archive_dir = archive.as_ref().unwrap_or(src_hold.dir());
    let sealed = seal_archive(archive_dir, pre, ownership, &mut report);
    let report_path = pre.archive.join(REPORT_FILE);
    match (outcome, sealed) {
        (Ok(()), Ok(())) => Ok(report),
        (Ok(()), Err(e)) => Err(anyhow::Error::from(e).context(format!(
            "the data was migrated into {}, but the archive {} could not be sealed and its \
             report {} may be missing; make the archive root-only and read-only by hand",
            data_path.display(),
            pre.archive.display(),
            report_path.display()
        ))),
        (Err(e), sealed) => Err(e.context(format!(
            "adopt-t0 failed after the T0 data had moved into {}: the old dir is archived at {} \
             ({}) and the partial report is {}. Finish by hand from the report; running \
             adopt-t0 again won't (the principal is no longer fresh)",
            data_path.display(),
            pre.archive.display(),
            match &sealed {
                Ok(()) => "sealed".to_string(),
                Err(se) => format!("NOT sealed: {se}"),
            },
            report_path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Principal, PrincipalId};
    use crate::server::operator::provision::NoReaper;
    use crate::server::operator::test_support;
    use std::os::unix::fs::MetadataExt;

    /// Records the uids it is asked to kill; kills nothing.
    #[derive(Default)]
    struct CountingReaper(std::sync::Mutex<Vec<u32>>);
    impl UidReaper for CountingReaper {
        fn kill_all(&self, p: &Principal) -> anyhow::Result<ReapOutcome> {
            self.0.lock().unwrap().push(p.uid);
            Ok(ReapOutcome::Killed)
        }
    }

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
        let reaper = CountingReaper::default();
        let report = migrate(&root, Ownership::SkipForTests, &account, &pre, &reaper)
            .await
            .unwrap();
        assert_eq!(report.mode, Mode::Copy);
        assert!(
            reaper.0.lock().unwrap().is_empty(),
            "a fresh principal's uid owns nothing of the old dir: no kill"
        );

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
        let reaper = CountingReaper::default();
        let report = migrate(&root, Ownership::SkipForTests, &account, &pre, &reaper)
            .await
            .unwrap();
        assert_eq!(report.mode, Mode::AdoptRename);
        // S4-1: the adopted uid's processes were stopped first.
        assert_eq!(*reaper.0.lock().unwrap(), [account.unix_uid]);
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
        let err = migrate(&root, Ownership::SkipForTests, &wrong_home, &pre, &NoReaper)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--home must name it"), "{err}");

        let used = principal(&root, None);
        fs::write(
            root.principal_data(used.principal_id).join("ikenga.db"),
            b"",
        )
        .unwrap();
        let err = migrate(&root, Ownership::SkipForTests, &used, &pre, &NoReaper)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("never run"), "{err}");

        let mut disabled = principal(&root, None);
        disabled.disabled_at = Some(1);
        assert!(
            migrate(&root, Ownership::SkipForTests, &disabled, &pre, &NoReaper)
                .await
                .is_err()
        );
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
        assert!(
            migrate(&root, Ownership::SkipForTests, &account, &pre, &NoReaper)
                .await
                .is_err()
        );
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

    /// R-10's copy branch: copied and verified — all of it — and only then
    /// removed from the source. A failed copy leaves the source as it was.
    #[test]
    fn archive_by_copy_deletes_the_source_after_verifying() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let from = base.join("from");
        fs::create_dir(&from).unwrap();
        fs::write(from.join("access.db"), "chain").unwrap();
        fs::write(from.join("access.db-wal"), "wal").unwrap();
        fs::write(from.join("ikenga.db"), "stays").unwrap();
        let parent = Dir::open_no_symlinks(&base).unwrap();
        let dir = parent.open_dir(os("from")).unwrap();
        let src = Source {
            parent: &parent,
            name: os("from"),
            dir: &dir,
            st: dir.stat().unwrap(),
            path: &from,
        };
        let archive = base.join("archive");
        let (_, moved, notes) = archive_by_copy(&src, os("archive"), &archive, true).unwrap();
        assert_eq!(moved, ["access.db", "access.db-wal"]);
        assert!(notes.is_empty());
        assert_eq!(
            fs::read_to_string(archive.join("access.db")).unwrap(),
            "chain"
        );
        assert!(!from.join("access.db").exists() && !from.join("access.db-wal").exists());
        assert!(from.join("ikenga.db").exists());

        // The second file fails (a hard link): the first is not deleted
        // from the source, and no half archive is left.
        fs::write(from.join("access.db"), "chain").unwrap();
        fs::write(from.join("access.db-wal"), "wal").unwrap();
        fs::hard_link(from.join("access.db-wal"), base.join("elsewhere")).unwrap();
        let err = archive_by_copy(&src, os("archive2"), &base.join("archive2"), true).unwrap_err();
        assert!(err.to_string().contains("hard links"), "{err}");
        assert!(from.join("access.db").exists() && from.join("access.db-wal").exists());
        assert!(!base.join("archive2").exists());
    }

    #[test]
    fn readers_equal_compares_bytes() {
        let eq = |a: &[u8], b: &[u8]| readers_equal(&mut &a[..], &mut &b[..]).unwrap();
        assert!(eq(&[1u8; 70_000], &[1u8; 70_000]));
        let mut other = vec![1u8; 70_000];
        other[69_999] = 2;
        assert!(!eq(&[1u8; 70_000], &other));
        assert!(!eq(&[1u8; 70_000], &[1u8; 10]));
        assert!(eq(&[], &[]));
    }

    /// S4-1: a file in an adopted tree with a second hard link may be
    /// someone else's (`ln /etc/shadow data-t0/x`): refused before anything
    /// moves, and the hold on the old dir is given back.
    #[tokio::test]
    async fn an_adopted_tree_with_a_hard_link_is_refused_before_anything_moves() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        let outside = from.parent().unwrap().join("outside");
        fs::write(&outside, "not the principal's").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644)).unwrap();
        fs::hard_link(&outside, from.join("chi-cache/x")).unwrap();
        let from_mode = mode_of(&from);
        let account = principal(&root, Some(&home));
        let pre = preflight(&root, &from, &home, "20261001T000005Z").unwrap();
        let err = migrate(&root, Ownership::SkipForTests, &account, &pre, &NoReaper)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("hard links") && msg.contains("nothing was moved"),
            "{msg}"
        );
        assert_eq!(mode_of(&outside), 0o644);
        assert_eq!(mode_of(&from), from_mode, "the hold was given back");
        assert!(from.join("access.db").exists() && !pre.archive.exists());
        require_fresh_data(&root.principal_data(account.principal_id)).unwrap();
    }

    /// S4-1: symlinks in an adopted tree are moved as links; nothing they
    /// point at is chmodded, chowned or copied, even a directory link.
    #[tokio::test]
    async fn symlinks_in_a_migrated_tree_are_never_followed() {
        for adopted in [true, false] {
            let (tmp, root) = test_support::temp_root();
            let (from, home) = t0_install(tmp.path()).await;
            let base = from.parent().unwrap().to_path_buf();
            let (secret, secret_dir) = (base.join("secret"), base.join("secret-dir"));
            fs::write(&secret, "root's").unwrap();
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).unwrap();
            fs::create_dir(&secret_dir).unwrap();
            fs::write(secret_dir.join("inner"), "root's too").unwrap();
            fs::set_permissions(&secret_dir, fs::Permissions::from_mode(0o755)).unwrap();
            std::os::unix::fs::symlink(&secret, from.join("chi-cache/to-file")).unwrap();
            std::os::unix::fs::symlink(&secret_dir, from.join("to-dir")).unwrap();
            let account = principal(&root, adopted.then_some(home.as_path()));
            let pre = preflight(&root, &from, &home, "20261001T000006Z").unwrap();
            let report = migrate(&root, Ownership::SkipForTests, &account, &pre, &NoReaper)
                .await
                .unwrap();
            assert_eq!(report.mode == Mode::AdoptRename, adopted);
            let data = root.principal_data(account.principal_id);
            for link in ["chi-cache/to-file", "to-dir"] {
                assert!(fs::symlink_metadata(data.join(link))
                    .unwrap()
                    .file_type()
                    .is_symlink());
            }
            assert_eq!(mode_of(&secret), 0o644);
            assert_eq!(mode_of(&secret_dir), 0o755);
            assert_eq!(mode_of(&secret_dir.join("inner")) & 0o044, 0o044);
            fs::set_permissions(&pre.archive, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// S4-4: a failure after the data has moved still writes the (partial)
    /// report and seals the archive, and the error names both.
    #[tokio::test]
    async fn a_failure_after_the_move_still_seals_the_archive_and_reports() {
        let (tmp, root) = test_support::temp_root();
        let (from, home) = t0_install(tmp.path()).await;
        // fs_roots.json is rewritten (copy mode) through a temp name; a
        // non-empty dir already there makes that fail after the data moved.
        fs::create_dir_all(from.join("fs_roots.json.t0-adopt/x")).unwrap();
        let account = principal(&root, None);
        let pre = preflight(&root, &from, &home, "20261001T000007Z").unwrap();
        let err = migrate(&root, Ownership::SkipForTests, &account, &pre, &NoReaper)
            .await
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("after the T0 data had moved"), "{msg}");
        assert!(
            msg.contains("(sealed)") && msg.contains(REPORT_FILE),
            "{msg}"
        );
        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pre.archive.join(REPORT_FILE)).unwrap())
                .unwrap();
        assert!(written["error"].as_str().is_some(), "{written}");
        assert_eq!(mode_of(&pre.archive), 0o500);
        assert_eq!(mode_of(&pre.archive.join("access.db")) & 0o222, 0);
        fs::set_permissions(&pre.archive, fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// S4-5: the scan never reads virtual tables (their module would run)
    /// or generated columns, and a database whose files aren't plain,
    /// singly-linked files is not opened at all.
    #[tokio::test]
    async fn the_db_scan_skips_virtual_tables_and_generated_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let db = base.join("ikenga.db");
        let mut conn = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
            .connect()
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE t (id INTEGER PRIMARY KEY, p TEXT, g TEXT GENERATED ALWAYS AS \
             (p || '/gen') VIRTUAL)",
            "INSERT INTO t (p) VALUES ('/old/data/a')",
            "CREATE VIRTUAL TABLE v USING fts5(p)",
            "INSERT INTO v (p) VALUES ('/old/data/b')",
        ] {
            sqlx::query(sql).execute(&mut conn).await.unwrap();
        }
        conn.close().await.unwrap();
        let hits = scan_db_paths(&db, &[("old_data_dir", Path::new("/old/data"))])
            .await
            .unwrap();
        let cols: Vec<_> = hits
            .iter()
            .map(|h| (h.table.as_str(), h.column.as_str()))
            .collect();
        assert_eq!(cols, [("t", "p")], "{hits:?}");

        let dir = Dir::open_no_symlinks(&base).unwrap();
        assert_eq!(db_files_safe(&dir), Ok(true));
        std::os::unix::fs::symlink("/etc/passwd", base.join("ikenga.db-shm")).unwrap();
        assert!(db_files_safe(&dir).unwrap_err().contains("ikenga.db-shm"));
        fs::remove_file(base.join("ikenga.db-shm")).unwrap();
        fs::hard_link(&db, base.join("other")).unwrap();
        assert!(db_files_safe(&dir).is_err());
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            db_files_safe(
                &Dir::open_no_symlinks(&fs::canonicalize(empty.path()).unwrap()).unwrap()
            ),
            Ok(false)
        );
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

        /// The real §7.3 kill, through the lib test binary's helper entry.
        fn reaper(root: &OperatorRoot) -> crate::server::operator::reaper::T1Reaper {
            crate::server::operator::reaper::T1Reaper::new(
                root,
                crate::server::operator::reaper::tests::test_helper(),
            )
        }

        /// A host user `name` (uid = gid = `uid`) whose T0 install is
        /// `t0_install`'s, owned by it as a non-root T0 daemon would leave it,
        /// adopted by a new account.
        async fn adopted_install(
            name: &'static str,
            uid: u32,
            uids: (u32, u32),
        ) -> (tempfile::TempDir, OperatorRoot, PathBuf, PathBuf, Account) {
            let (tmp, root) = enforced_root();
            let (from, home) = t0_install(tmp.path()).await;
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
            chown_tree(&from, uid);
            chown_tree(&home, uid);
            let prov = Provisioner::new(
                root.clone(),
                UidRange::new(uids.0, uids.1).unwrap(),
                ProvisioningMode::Auto,
                Actor::Cli,
            );
            let pool = open_accounts(&root, Opener::Cli).await.unwrap();
            let username = name.trim_start_matches("ik-");
            let a = prov
                .create(&pool, username, PW, false, Some(Adopt::user(name)))
                .await
                .unwrap();
            pool.close().await;
            (tmp, root, from, home, a)
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
            let report = migrate(&root, Ownership::Enforce, &a, &pre, &NoReaper)
                .await
                .unwrap();
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
            let report = migrate(&root, Ownership::Enforce, &a, &pre, &reaper(&root))
                .await
                .unwrap();
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

        /// S4-1 under root: a process of the adopted uid already inside the
        /// old dir (cwd held there) is killed before the tree is touched; a
        /// hard link to a root-owned file outside is refused before anything
        /// moves; nothing outside the tree changes owner or mode.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_adopt_t0_kills_the_uid_and_never_touches_what_is_outside() {
            require_root();
            let name = "ik-t1root-race";
            let _cleanup = HostUser(name);
            let uid = 28_100;
            let (_tmp, root, from, home, a) = adopted_install(name, uid, (28_101, 28_109)).await;
            let victim = from.parent().unwrap().join("victim");
            fs::write(&victim, "root's").unwrap();
            fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
            let exec = crate::executor::t1::T1Executor::new(crate::executor::t1::tests::config());
            let sleeper = || {
                use crate::executor::SessionExecutor;
                let mut spec = crate::executor::SpawnSpec::new("/bin/sh");
                spec.arg("-c")
                    .arg("exec sleep 300")
                    .current_dir(from.join("chi-cache"))
                    .principal(Some(a.principal()));
                let mut opts = crate::executor::t1::tests::piped();
                opts.detached = true;
                exec.spawn_piped(spec, opts).unwrap()
            };
            let killed = |mut child: tokio::process::Child| async move {
                use std::os::unix::process::ExitStatusExt;
                let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                    .await
                    .expect("the uid's process was killed")
                    .unwrap();
                assert_eq!(status.signal(), Some(libc::SIGKILL));
            };

            // A hard link to a root-owned file: refused, nothing moved.
            fs::hard_link(&victim, from.join("chi-cache/shadow")).unwrap();
            let from_before = owner_mode(&from);
            let child = sleeper();
            let pre = preflight(&root, &from, &home, "20261001T000012Z").unwrap();
            let err = migrate(&root, Ownership::Enforce, &a, &pre, &reaper(&root))
                .await
                .unwrap_err();
            assert!(format!("{err:#}").contains("hard links"), "{err:#}");
            killed(child).await;
            assert_eq!(owner_mode(&victim), (0, 0, 0o644));
            assert_eq!(owner_mode(&from), from_before, "the hold was given back");
            assert!(!pre.archive.exists());

            // Without it: the process inside is killed, the rename lands,
            // and the file outside is still root's.
            fs::remove_file(from.join("chi-cache/shadow")).unwrap();
            let child = sleeper();
            let pre = preflight(&root, &from, &home, "20261001T000013Z").unwrap();
            let report = migrate(&root, Ownership::Enforce, &a, &pre, &reaper(&root))
                .await
                .unwrap();
            killed(child).await;
            assert_eq!(report.mode, Mode::AdoptRename);
            assert!(report.notes.iter().any(|n| n.contains("was killed")));
            assert_eq!(owner_mode(&victim), (0, 0, 0o644));
            let pdir = root.principal_dir(a.principal_id);
            let v = i9_violations(&pdir, uid, uid, true).unwrap();
            assert!(v.is_empty(), "I-9: {v:#?}");
        }

        /// S4-3: a bind mount of the same filesystem shares its st_dev, so
        /// the rename is tried and fails (EBUSY/EXDEV); the migration falls
        /// back to a copy instead of failing.
        #[tokio::test]
        #[ignore = "t1-root"]
        async fn t1_root_adopt_t0_falls_back_to_a_copy_across_a_bind_mount() {
            require_root();
            let name = "ik-t1root-bind";
            let _cleanup = HostUser(name);
            let uid = 28_110;
            let (_tmp, root, from, home, a) = adopted_install(name, uid, (28_111, 28_119)).await;
            let mounted = std::process::Command::new("mount")
                .arg("--bind")
                .arg(&from)
                .arg(&from)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !mounted {
                eprintln!("skipped: mount --bind is not permitted here");
                return;
            }
            struct Umount(PathBuf);
            impl Drop for Umount {
                fn drop(&mut self) {
                    let _ = std::process::Command::new("umount").arg(&self.0).status();
                }
            }
            let _umount = Umount(from.clone());
            let pre = preflight(&root, &from, &home, "20261001T000014Z").unwrap();
            let report = migrate(&root, Ownership::Enforce, &a, &pre, &reaper(&root))
                .await
                .unwrap();
            assert_eq!(report.mode, Mode::AdoptCopy);
            assert!(report.old_dir_left_in_place, "{report:?}");
            assert!(report.notes.iter().any(|n| n.contains("copied instead")));
            let data = root.principal_data(a.principal_id);
            assert_eq!(
                fs::read(data.join("chi-cache/run.json")).unwrap(),
                vec![7u8; 200_000]
            );
            for f in ARCHIVE_ONLY {
                assert!(!data.join(f).exists(), "R-10: {f}");
                assert!(!from.join(f).exists(), "{f} left the old dir");
                assert!(pre.archive.join(f).exists(), "{f} archived");
            }
            let v = i9_violations(&root.principal_dir(a.principal_id), uid, uid, true).unwrap();
            assert!(v.is_empty(), "I-9: {v:#?}");
            assert_eq!(owner_mode(&from), (0, 0, 0o500), "left in place, sealed");
        }
    }
}
