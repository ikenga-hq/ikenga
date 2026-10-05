//! The built-in `/etc` writer (G-PRINCIPAL §7.2, OD-4): the fallback backend
//! for images with no `shadow` package (no `useradd`), as the G-73 spike's
//! `probe.py` `ensure_user` did. It edits `/etc/group`, `/etc/passwd` and —
//! when they exist — `/etc/gshadow` and `/etc/shadow`:
//!
//! * under the system shadow lock (`lckpwdf(3)`), so it serializes with
//!   `useradd`/`passwd` and with another writer;
//! * each file is rewritten whole to `<file>+` (same owner and mode as the
//!   original), fsynced, then `rename(2)`d over the original — a reader sees
//!   the old file or the new one, never a torn line;
//! * the password field is always locked (`!`): principals log in through
//!   the daemon, never with a Unix password.
//!
//! Production always writes the real `/etc`. Unit tests point it at a temp
//! prefix, where a plain `flock` on `<prefix>/etc/.pwd.lock` stands in for
//! `lckpwdf` (which only ever locks the real `/etc/.pwd.lock`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::sys;

/// A user the writer adds. Every field is checked for `:` and newlines.
#[derive(Debug, Clone)]
pub(crate) struct NewUser<'a> {
    pub name: &'a str,
    pub uid: u32,
    pub gid: u32,
    pub gecos: &'a str,
    pub home: &'a Path,
    pub shell: &'a Path,
    /// Also add the user-private group `name`/`gid`. `false`: that group
    /// already exists (reconcile recreating only a lost passwd entry).
    pub own_group: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EtcFiles {
    prefix: PathBuf,
}

enum EtcLock {
    System(#[allow(dead_code)] sys::PwdLock),
    #[cfg(test)]
    File(#[allow(dead_code)] File),
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn check_field(what: &str, value: &str) -> io::Result<()> {
    if value.contains(':') || value.contains('\n') || value.contains('\0') {
        return Err(invalid(format!(
            "{what} `{value}` can't go in an /etc database"
        )));
    }
    Ok(())
}

/// The name in field 0 and the id in field 2 of a passwd/group line, skipping
/// comments, blanks and NIS `+`/`-` compat lines.
fn name_and_id(line: &str) -> Option<(&str, Option<u32>)> {
    if line.is_empty() || line.starts_with('#') || line.starts_with('+') || line.starts_with('-') {
        return None;
    }
    let mut fields = line.split(':');
    let name = fields.next()?;
    let id = fields.nth(1).and_then(|f| f.parse().ok());
    Some((name, id))
}

fn days_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

impl EtcFiles {
    /// The real `/etc`.
    pub(crate) fn system() -> Self {
        Self {
            prefix: PathBuf::from("/"),
        }
    }

    /// `<prefix>/etc/…` — test seam only.
    #[cfg(test)]
    pub(crate) fn at(prefix: impl Into<PathBuf>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    pub(crate) fn is_system(&self) -> bool {
        self.prefix == Path::new("/")
    }

    fn file(&self, name: &str) -> PathBuf {
        self.prefix.join("etc").join(name)
    }

    fn lock(&self) -> io::Result<EtcLock> {
        if self.is_system() {
            return sys::PwdLock::acquire().map(EtcLock::System);
        }
        #[cfg(test)]
        {
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(self.file(".pwd.lock"))?;
            // SAFETY: a valid fd for the duration of the call.
            if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(EtcLock::File(file))
        }
        #[cfg(not(test))]
        unreachable!("EtcFiles is only ever the system /etc outside tests")
    }

    fn read(&self, name: &str) -> io::Result<Option<String>> {
        match fs::read_to_string(self.file(name)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Rewrite `name` atomically with `edit` applied. A missing optional file
    /// (`shadow`, `gshadow`) is left missing; a missing required one errors.
    fn rewrite(
        &self,
        name: &str,
        required: bool,
        edit: impl FnOnce(&str) -> String,
    ) -> io::Result<bool> {
        let path = self.file(name);
        let Some(current) = self.read(name)? else {
            if required {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} does not exist", path.display()),
                ));
            }
            return Ok(false);
        };
        let meta = fs::metadata(&path)?;
        let next = edit(&current);
        let tmp = path.with_file_name(format!("{name}+"));
        // A stale `+` file is a crashed writer's; we hold the lock now.
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut f: File = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let written = (|| {
            f.write_all(next.as_bytes())?;
            // Same owner and mode as the file it replaces (shadow is 0640
            // root:shadow on Debian, 0000 on Fedora, …).
            if sys::geteuid() == 0 {
                std::os::unix::fs::fchown(&f, Some(meta.uid()), Some(meta.gid()))?;
            }
            f.set_permissions(fs::Permissions::from_mode(meta.mode() & 0o7777))?;
            f.sync_all()
        })();
        if let Err(e) = written {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        drop(f);
        fs::rename(&tmp, &path)?;
        if let Some(dir) = path.parent() {
            File::open(dir)?.sync_all()?;
        }
        Ok(true)
    }

    fn contains(&self, name: &str, pred: impl Fn(&str, Option<u32>) -> bool) -> io::Result<bool> {
        Ok(self
            .read(name)?
            .map(|s| s.lines().filter_map(name_and_id).any(|(n, id)| pred(n, id)))
            .unwrap_or(false))
    }

    pub(crate) fn uid_taken(&self, uid: u32) -> io::Result<bool> {
        self.contains("passwd", |_, id| id == Some(uid))
    }

    pub(crate) fn gid_taken(&self, gid: u32) -> io::Result<bool> {
        self.contains("group", |_, id| id == Some(gid))
    }

    /// The passwd entry matching `pred`, parsed (§8 step 7 reconcile).
    fn passwd_entry(
        &self,
        pred: impl Fn(&str, Option<u32>) -> bool,
    ) -> io::Result<Option<sys::PasswdInfo>> {
        let Some(text) = self.read("passwd")? else {
            return Ok(None);
        };
        for line in text.lines() {
            let Some((name, id)) = name_and_id(line) else {
                continue;
            };
            if !pred(name, id) {
                continue;
            }
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 7 {
                return Err(invalid(format!("malformed passwd line for {name}")));
            }
            let num = |v: &str| {
                v.parse::<u32>()
                    .map_err(|_| invalid(format!("malformed passwd id `{v}` for {name}")))
            };
            return Ok(Some(sys::PasswdInfo {
                name: name.to_string(),
                uid: num(f[2])?,
                gid: num(f[3])?,
                home: PathBuf::from(f[5]),
                shell: PathBuf::from(f[6]),
            }));
        }
        Ok(None)
    }

    pub(crate) fn passwd_by_name(&self, name: &str) -> io::Result<Option<sys::PasswdInfo>> {
        self.passwd_entry(|n, _| n == name)
    }

    pub(crate) fn passwd_by_uid(&self, uid: u32) -> io::Result<Option<sys::PasswdInfo>> {
        self.passwd_entry(|_, id| id == Some(uid))
    }

    /// The group entry matching `pred`, parsed (§8 step 7 reconcile).
    fn group_entry(
        &self,
        pred: impl Fn(&str, Option<u32>) -> bool,
    ) -> io::Result<Option<sys::GroupInfo>> {
        let Some(text) = self.read("group")? else {
            return Ok(None);
        };
        for (name, id) in text.lines().filter_map(name_and_id) {
            if !pred(name, id) {
                continue;
            }
            let gid = id.ok_or_else(|| invalid(format!("malformed group line for {name}")))?;
            return Ok(Some(sys::GroupInfo {
                name: name.to_string(),
                gid,
            }));
        }
        Ok(None)
    }

    pub(crate) fn group_by_gid(&self, gid: u32) -> io::Result<Option<sys::GroupInfo>> {
        self.group_entry(|_, id| id == Some(gid))
    }

    pub(crate) fn group_by_name(&self, name: &str) -> io::Result<Option<sys::GroupInfo>> {
        self.group_entry(|n, _| n == name)
    }

    /// Add group `name`/`gid` (and its gshadow line), refusing if either is
    /// already in `group`.
    pub(crate) fn add_group(&self, name: &str, gid: u32) -> io::Result<()> {
        check_field("name", name)?;
        let _lock = self.lock()?;
        if self.contains("group", |n, id| n == name || id == Some(gid))? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "group {name} or gid {gid} is already in {}",
                    self.file("group").display()
                ),
            ));
        }
        self.rewrite("group", true, append_line(format!("{name}:x:{gid}:")))?;
        if let Err(e) = self.rewrite("gshadow", false, append_line(format!("{name}:!::"))) {
            let _ = self.rewrite("group", false, |cur| without(cur, name));
            return Err(e);
        }
        Ok(())
    }

    pub(crate) fn name_taken(&self, name: &str) -> io::Result<bool> {
        Ok(self.contains("passwd", |n, _| n == name)?
            || self.contains("group", |n, _| n == name)?)
    }

    /// Add `user` with a user-private group (`gid`), refusing if the name,
    /// uid or gid is already in the files. All-or-nothing: a failure part way
    /// removes what this call wrote.
    pub(crate) fn add_user(&self, user: &NewUser<'_>) -> io::Result<()> {
        check_field("name", user.name)?;
        check_field("gecos", user.gecos)?;
        let home = user
            .home
            .to_str()
            .ok_or_else(|| invalid("home is not UTF-8".into()))?;
        let shell = user
            .shell
            .to_str()
            .ok_or_else(|| invalid("shell is not UTF-8".into()))?;
        check_field("home", home)?;
        check_field("shell", shell)?;

        let _lock = self.lock()?;
        let group_clash = if user.own_group {
            self.contains("group", |n, _| n == user.name)? || self.gid_taken(user.gid)?
        } else {
            // The group must already be there, under that gid.
            self.group_by_gid(user.gid)?.map(|g| g.name).as_deref() != Some(user.name)
        };
        if self.contains("passwd", |n, _| n == user.name)?
            || self.uid_taken(user.uid)?
            || group_clash
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} or uid/gid {}/{} is already in {}",
                    user.name,
                    user.uid,
                    user.gid,
                    self.file("passwd").display()
                ),
            ));
        }
        let has_shadow = self.read("shadow")?.is_some();
        let append = append_line;
        let steps: [(&str, bool, String); 4] = [
            ("group", true, format!("{}:x:{}:", user.name, user.gid)),
            ("gshadow", false, format!("{}:!::", user.name)),
            (
                "passwd",
                true,
                format!(
                    "{}:{}:{}:{}:{}:{}:{}",
                    user.name,
                    if has_shadow { "x" } else { "!" },
                    user.uid,
                    user.gid,
                    user.gecos,
                    home,
                    shell
                ),
            ),
            (
                "shadow",
                false,
                format!("{}:!:{}:0:99999:7:::", user.name, days_since_epoch()),
            ),
        ];
        let mut done: Vec<&str> = Vec::new();
        for (file, required, line) in steps {
            if !user.own_group && (file == "group" || file == "gshadow") {
                continue;
            }
            match self.rewrite(file, required, append(line)) {
                Ok(_) => done.push(file),
                Err(e) => {
                    for file in done.into_iter().rev() {
                        let _ = self.rewrite(file, false, |cur| without(cur, user.name));
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Remove every line for `name` from all four files. Absent is fine.
    pub(crate) fn remove_user(&self, name: &str) -> io::Result<()> {
        check_field("name", name)?;
        let _lock = self.lock()?;
        for file in ["shadow", "passwd", "gshadow", "group"] {
            self.rewrite(file, false, |cur| without(cur, name))?;
        }
        Ok(())
    }

    /// Set `name`'s login shell. `false` when there is no such passwd line.
    pub(crate) fn set_shell(&self, name: &str, shell: &Path) -> io::Result<bool> {
        check_field("name", name)?;
        let shell = shell
            .to_str()
            .ok_or_else(|| invalid("shell is not UTF-8".into()))?;
        check_field("shell", shell)?;
        let _lock = self.lock()?;
        let mut found = false;
        self.rewrite("passwd", true, |cur| {
            map_lines(cur, name, |fields| {
                found = true;
                if fields.len() >= 7 {
                    fields[6] = shell.to_string();
                }
            })
        })?;
        Ok(found)
    }

    /// Make sure `name`'s password field is locked (`!`-prefixed) in shadow,
    /// or in passwd when there is no shadow file.
    pub(crate) fn lock_password(&self, name: &str) -> io::Result<()> {
        check_field("name", name)?;
        let _lock = self.lock()?;
        let lock_field = |fields: &mut Vec<String>| {
            if fields.len() > 1 && !fields[1].starts_with('!') && fields[1] != "x" {
                fields[1] = format!("!{}", fields[1]);
            }
        };
        if !self.rewrite("shadow", false, |cur| map_lines(cur, name, lock_field))? {
            self.rewrite("passwd", true, |cur| map_lines(cur, name, lock_field))?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn path_for_tests(&self, file: &str) -> PathBuf {
        self.file(file)
    }

    #[cfg(test)]
    pub(crate) fn line(&self, file: &str, name: &str) -> Option<String> {
        self.read(file)
            .unwrap()?
            .lines()
            .find(|l| l.split(':').next() == Some(name))
            .map(str::to_string)
    }
}

/// An edit appending `line` (and a newline before it if the file lacks one).
fn append_line(line: String) -> impl FnOnce(&str) -> String {
    move |cur: &str| {
        let mut s = cur.to_string();
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&line);
        s.push('\n');
        s
    }
}

fn without(cur: &str, name: &str) -> String {
    let mut out = String::with_capacity(cur.len());
    for line in cur.lines() {
        if line.split(':').next() == Some(name) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn map_lines(cur: &str, name: &str, mut f: impl FnMut(&mut Vec<String>)) -> String {
    let mut out = String::with_capacity(cur.len());
    for line in cur.lines() {
        if line.split(':').next() == Some(name) {
            let mut fields: Vec<String> = line.split(':').map(str::to_string).collect();
            f(&mut fields);
            out.push_str(&fields.join(":"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal Debian-ish `/etc` under a temp prefix.
    pub(crate) fn fake_etc(with_shadow: bool) -> (tempfile::TempDir, EtcFiles) {
        let tmp = tempfile::tempdir().unwrap();
        let etc = tmp.path().join("etc");
        fs::create_dir(&etc).unwrap();
        fs::write(etc.join("passwd"), "root:x:0:0:root:/root:/bin/bash\n# comment\nhostuser:x:3900000003:3900000003::/home/h:/bin/sh\n").unwrap();
        fs::write(etc.join("group"), "root:x:0:\nhostgroup:x:3900000004:\n").unwrap();
        fs::set_permissions(etc.join("passwd"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(etc.join("group"), fs::Permissions::from_mode(0o644)).unwrap();
        if with_shadow {
            fs::write(etc.join("shadow"), "root:*:19000:0:99999:7:::\n").unwrap();
            fs::write(etc.join("gshadow"), "root:*::\n").unwrap();
            fs::set_permissions(etc.join("shadow"), fs::Permissions::from_mode(0o640)).unwrap();
            fs::set_permissions(etc.join("gshadow"), fs::Permissions::from_mode(0o640)).unwrap();
        }
        let files = EtcFiles::at(tmp.path());
        (tmp, files)
    }

    fn user<'a>(name: &'a str, id: u32, home: &'a Path) -> NewUser<'a> {
        NewUser {
            name,
            uid: id,
            gid: id,
            gecos: "ikenga principal",
            home,
            shell: Path::new("/bin/sh"),
            own_group: true,
        }
    }

    /// Review S2-4: a lost group comes back alone; a user can be re-added
    /// onto its surviving group; clashes are refused.
    #[test]
    fn add_group_and_add_user_onto_an_existing_group() {
        let (_tmp, etc) = fake_etc(true);
        etc.add_group("ik-ada", 3_900_000_000).unwrap();
        assert_eq!(etc.line("group", "ik-ada").unwrap(), "ik-ada:x:3900000000:");
        assert_eq!(etc.line("gshadow", "ik-ada").unwrap(), "ik-ada:!::");
        assert_eq!(
            etc.group_by_gid(3_900_000_000).unwrap().map(|g| g.name),
            Some("ik-ada".to_string())
        );
        assert_eq!(
            etc.group_by_name("ik-ada").unwrap().map(|g| g.gid),
            Some(3_900_000_000)
        );
        assert!(etc.add_group("ik-ada", 3_900_000_001).is_err());
        assert!(etc.add_group("other", 3_900_000_000).is_err());
        assert!(
            etc.add_group("other", 3_900_000_004).is_err(),
            "hostgroup's gid"
        );

        let home = Path::new("/h");
        let mut u = user("ik-ada", 3_900_000_000, home);
        // own_group = true would clash with the group just added.
        assert!(etc.add_user(&u).is_err());
        u.own_group = false;
        etc.add_user(&u).unwrap();
        assert!(etc.line("passwd", "ik-ada").is_some());
        let groups = fs::read_to_string(etc.file("group")).unwrap();
        assert_eq!(groups.matches("ik-ada:").count(), 1);
        // own_group = false needs that group, under that gid and name.
        let mut v = user("ik-bob", 3_900_000_001, home);
        v.own_group = false;
        assert!(etc.add_user(&v).is_err());
        assert!(etc.line("passwd", "ik-bob").is_none());
    }

    #[test]
    fn add_writes_all_four_files_and_keeps_modes() {
        let (_tmp, etc) = fake_etc(true);
        let home = Path::new("/srv/root/principals/x/home");
        etc.add_user(&user("ik-ada", 3_900_000_000, home)).unwrap();
        assert_eq!(
            etc.line("passwd", "ik-ada").unwrap(),
            "ik-ada:x:3900000000:3900000000:ikenga principal:/srv/root/principals/x/home:/bin/sh"
        );
        assert_eq!(etc.line("group", "ik-ada").unwrap(), "ik-ada:x:3900000000:");
        assert_eq!(etc.line("gshadow", "ik-ada").unwrap(), "ik-ada:!::");
        assert!(etc
            .line("shadow", "ik-ada")
            .unwrap()
            .starts_with("ik-ada:!:"));
        // Untouched lines and comments survive; modes are preserved.
        assert!(etc.line("passwd", "root").is_some());
        assert!(fs::read_to_string(etc.file("passwd"))
            .unwrap()
            .contains("# comment"));
        assert_eq!(
            fs::metadata(etc.file("passwd")).unwrap().mode() & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(etc.file("shadow")).unwrap().mode() & 0o777,
            0o640
        );
        assert!(!etc.file("passwd+").exists());
        assert!(etc.uid_taken(3_900_000_000).unwrap());
        assert!(etc.gid_taken(3_900_000_000).unwrap());
        assert!(etc.name_taken("ik-ada").unwrap());
    }

    #[test]
    fn no_shadow_file_locks_the_passwd_field_instead() {
        let (_tmp, etc) = fake_etc(false);
        etc.add_user(&user("ik-ada", 3_900_000_000, Path::new("/h")))
            .unwrap();
        assert!(etc
            .line("passwd", "ik-ada")
            .unwrap()
            .starts_with("ik-ada:!:"));
        assert!(!etc.file("shadow").exists());
    }

    #[test]
    fn duplicates_are_refused_and_nothing_is_written() {
        let (_tmp, etc) = fake_etc(true);
        let before = fs::read_to_string(etc.file("passwd")).unwrap();
        for u in [
            user("hostuser", 3_900_000_000, Path::new("/h")),
            user("ik-new", 3_900_000_003, Path::new("/h")),
            user("ik-new", 3_900_000_004, Path::new("/h")),
            user("hostgroup", 3_900_000_001, Path::new("/h")),
        ] {
            let err = etc.add_user(&u).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{u:?}");
        }
        assert_eq!(fs::read_to_string(etc.file("passwd")).unwrap(), before);
        assert!(etc.line("group", "ik-new").is_none());
    }

    #[test]
    fn colons_and_newlines_never_reach_the_files() {
        let (_tmp, etc) = fake_etc(true);
        for bad in ["/h:/x", "/h\nevil::0:0::/:/bin/sh"] {
            assert!(etc
                .add_user(&user("ik-ada", 3_900_000_000, Path::new(bad)))
                .is_err());
        }
        assert!(etc.line("passwd", "ik-ada").is_none());
    }

    #[test]
    fn passwd_lookups_parse_the_entry() {
        let (tmp, files) = fake_etc(true);
        let home = tmp.path().join("h");
        files.add_user(&user("ik-ada", 20_000, &home)).unwrap();
        let by_name = files.passwd_by_name("ik-ada").unwrap().unwrap();
        assert_eq!((by_name.uid, by_name.gid), (20_000, 20_000));
        assert_eq!(by_name.home, home);
        assert_eq!(files.passwd_by_uid(20_000).unwrap(), Some(by_name));
        assert_eq!(files.passwd_by_name("ik-bob").unwrap(), None);
        assert_eq!(files.passwd_by_uid(20_001).unwrap(), None);
    }

    #[test]
    fn remove_set_shell_and_lock() {
        let (_tmp, etc) = fake_etc(true);
        etc.add_user(&user("ik-ada", 3_900_000_000, Path::new("/h")))
            .unwrap();
        assert!(etc
            .set_shell("ik-ada", Path::new("/usr/sbin/nologin"))
            .unwrap());
        assert!(etc
            .line("passwd", "ik-ada")
            .unwrap()
            .ends_with(":/usr/sbin/nologin"));
        assert!(!etc.set_shell("ik-nobody", Path::new("/bin/sh")).unwrap());
        etc.lock_password("ik-ada").unwrap();
        assert!(etc
            .line("shadow", "ik-ada")
            .unwrap()
            .starts_with("ik-ada:!:"));
        etc.remove_user("ik-ada").unwrap();
        for file in ["passwd", "group", "shadow", "gshadow"] {
            assert!(etc.line(file, "ik-ada").is_none(), "{file}");
        }
        assert!(etc.line("passwd", "root").is_some());
        // Idempotent.
        etc.remove_user("ik-ada").unwrap();
    }
}
