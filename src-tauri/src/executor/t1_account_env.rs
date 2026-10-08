//! T1 — per-account environment from a root-owned secrets file
//! (`/etc/ikenga/secrets/<unix_name>.env`).
//!
//! The provisioner (`scripts/server/provision.sh`, `sync_secrets`) writes the
//! secrets an operator granted one account to a file owned `root:<account
//! gid>`, mode `0640`, one `NAME=value` per line. Until this module they
//! reached only a shell (`/etc/profile.d`). The **root broker** reads the file
//! when it launches that account's `ikenga-server` child
//! ([`crate::server::broker::children::T1Launcher::launch`]) and hands the
//! entries over as the child's process environment through
//! [`crate::executor::t1::T1Executor::spawn_piped_with_envs`]. The child then
//! passes them on by the inheritance it already has: the PTY rebuild
//! (`pty::spawn_inner`: `env_clear` + every var that is not host-only), Chi
//! runs and engine CLIs (`chi_exec::inherit_scrubbed_env`), the detached
//! chi-runner, and pkg MCP servers and sidecars (`server/rpc_exec.rs`
//! `sidecar_spec`: the child's environment minus host-only names). Nothing in the
//! child reads the file, so the child needs no access logic and cannot be
//! tricked into reading another account's file.
//!
//! ## Why the broker reads it
//!
//! The broker already assembles the child's whole environment (`environment`
//! in `executor/t1.rs`, `T1Launcher::host_env`) and is the only party that
//! knows which principal it is launching, so "account A's file is never read
//! for account B" is a property of one call site: the path is derived from the
//! launching principal's `unix_name` and the group check pins the file to that
//! principal's gid. It also runs as root, so the checks below can demand a
//! root-owned file and a root-owned directory.
//!
//! ## Trust checks (fail closed: the file is skipped, nothing partial is used)
//!
//! * the account name is a plain file-name component (no `/`, no `..`);
//! * the directory is a real directory (not a symlink), owned by root and not
//!   group/other-writable;
//! * the file is opened `O_NOFOLLOW | O_NONBLOCK` and **then** examined with
//!   `fstat` on the open descriptor (no lstat/open race): a regular file,
//!   owned by root, group == the account's gid, no world bits and no group
//!   write bit;
//! * bounded: [`MAX_FILE_BYTES`], [`MAX_LINES`], [`MAX_VARS`],
//!   [`MAX_VALUE_BYTES`].
//!
//! Values are literal: everything after the first `=`, no quoting, no
//! expansion, no trimming. A bad *line* is skipped on its own; a bad *file* is
//! skipped whole. Log lines carry names (and line numbers for lines whose name
//! is unusable), never values.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use zeroize::Zeroize;

use crate::executor::Principal;

/// Where the provisioner puts the per-account files (`SECRETS_DIR`).
pub const DEFAULT_DIR: &str = "/etc/ikenga/secrets";
/// Larger files are refused whole.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;
/// More lines than this and the file is refused whole.
pub const MAX_LINES: usize = 1024;
/// More distinct variables than this: the extras are skipped.
pub const MAX_VARS: usize = 256;
/// A longer value is skipped (execve's per-string limit is 128 KiB).
pub const MAX_VALUE_BYTES: usize = 16 * 1024;

/// Names an account secret may not take. The first block mirrors
/// `secret_name_ok` in `scripts/server/provision.sh` exactly (the provisioner
/// refuses them at write time; this is the daemon's own check on read, so a
/// hand-edited file gets no further). The second block is what the daemon and
/// its children rely on and would break or be redirected by: locale, the
/// `XDG_*` base dirs, `RUST_*` (log level / backtraces, `RUST_LOG` is set by
/// the launcher), the loader and resolver knobs.
pub fn is_refused_name(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        // provision.sh `secret_name_ok`
        "IKENGA_",
        "LD_",
        "DYLD_",
        "BASH_",
        "GIT_CONFIG",
        // the daemon's own
        "XDG_",
        "LC_",
        "RUST_",
        "MALLOC_",
    ];
    const EXACT: &[&str] = &[
        // provision.sh `secret_name_ok`
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "IFS",
        "ENV",
        "PS1",
        "PS2",
        "PS4",
        "PROMPT_COMMAND",
        "CDPATH",
        "GLOBIGNORE",
        "SHELLOPTS",
        "TMPDIR",
        "TERM",
        "PWD",
        "OLDPWD",
        "GIT_ASKPASS",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_PROXY_COMMAND",
        "GIT_EXEC_PATH",
        "GIT_DIR",
        "GIT_WORK_TREE",
        // the daemon's own: the floor / locale it sets, loader and resolver knobs
        "LANG",
        "LANGUAGE",
        "TZ",
        "TMP",
        "TEMP",
        "COLORTERM",
        "SHLVL",
        "_",
        "BASHOPTS",
        "GLIBC_TUNABLES",
        "GCONV_PATH",
        "LOCPATH",
        "NLSPATH",
        "HOSTALIASES",
        "RES_OPTIONS",
        "LOCALDOMAIN",
    ];
    EXACT.contains(&name) || PREFIXES.iter().any(|p| name.starts_with(p))
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`.
fn is_valid_name(name: &[u8]) -> bool {
    match name.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_alphabetic() || *first == b'_')
                && rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        }
        None => false,
    }
}

/// Is `name` something we would use as a path component under the secrets
/// directory? (`unix_name`s are `ik-<login>`; adopted accounts may differ.)
fn is_plain_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && !name.contains("..")
}

/// What the file must look like. Production is `owner_uid: 0`; tests use their
/// own uid because they cannot create root-owned files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub owner_uid: u32,
    pub gid: u32,
}

/// Why a whole file (or its directory) was not used. Display carries no
/// content and no path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    BadAccountName,
    DirIsSymlink,
    DirNotDirectory,
    DirNotRootOwned,
    DirWritable,
    Symlink,
    NotRegular,
    WrongOwner,
    WrongGroup,
    WorldAccessible,
    GroupWritable,
    TooLarge,
    TooManyLines,
    Unreadable(std::io::ErrorKind),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadAccountName => f.write_str("the account name is not a plain file name"),
            Self::DirIsSymlink => f.write_str("the secrets directory is a symlink"),
            Self::DirNotDirectory => f.write_str("the secrets directory is not a directory"),
            Self::DirNotRootOwned => f.write_str("the secrets directory is not owned by root"),
            Self::DirWritable => f.write_str("the secrets directory is group- or other-writable"),
            Self::Symlink => f.write_str("the file is a symlink"),
            Self::NotRegular => f.write_str("the file is not a regular file"),
            Self::WrongOwner => f.write_str("the file is not owned by root"),
            Self::WrongGroup => f.write_str("the file's group is not the account's group"),
            Self::WorldAccessible => f.write_str("the file has world permission bits"),
            Self::GroupWritable => f.write_str("the file is group-writable"),
            Self::TooLarge => write!(f, "the file is larger than {MAX_FILE_BYTES} bytes"),
            Self::TooManyLines => write!(f, "the file has more than {MAX_LINES} lines"),
            Self::Unreadable(kind) => write!(f, "the file could not be read ({kind})"),
        }
    }
}

/// Why one entry was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No `=` on the line (it may be a bare secret: the line is never echoed).
    NoEquals,
    /// The name is not `^[A-Za-z_][A-Za-z0-9_]*$`.
    BadName,
    /// The name is on the deny list ([`is_refused_name`]).
    Denylisted,
    /// A NUL or CR in the value (a CRLF file would otherwise leak `\r` into
    /// every value).
    BadByte,
    ValueTooLong,
    TooManyVars,
}

/// A skipped entry: by name when the name itself is a usable identifier,
/// otherwise only by line number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    Name(String, SkipReason),
    Line(usize, SkipReason),
}

/// The accepted entries (in file order, a repeated name keeps its last value,
/// as a shell loader's repeated `export` would) and what was skipped.
///
/// Values are zeroed on drop (best effort: the spawn copies them).
#[derive(Default)]
pub struct Parsed {
    pub vars: Vec<(OsString, OsString)>,
    pub skipped: Vec<Skipped>,
}

impl Parsed {
    pub fn names(&self) -> Vec<String> {
        self.vars
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect()
    }

    /// Hand the entries over, leaving nothing behind to zero.
    pub fn into_vars(mut self) -> Vec<(OsString, OsString)> {
        std::mem::take(&mut self.vars)
    }
}

impl fmt::Debug for Parsed {
    // Names only: a `{:?}` of this can never print a value.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Parsed")
            .field("names", &self.names())
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl Drop for Parsed {
    fn drop(&mut self) {
        for (_, v) in &mut self.vars {
            let mut bytes = std::mem::take(v).into_vec();
            bytes.zeroize();
        }
    }
}

/// Parse a file's bytes. Pure: no filesystem, no logging.
pub fn parse(bytes: &[u8]) -> Result<Parsed, Refusal> {
    let mut out = Parsed::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut lines = bytes.split(|b| *b == b'\n').peekable();
    let mut n = 0usize;
    while let Some(line) = lines.next() {
        // A trailing newline leaves one empty final piece; it is not a line.
        if lines.peek().is_none() && line.is_empty() {
            break;
        }
        n += 1;
        if n > MAX_LINES {
            return Err(Refusal::TooManyLines);
        }
        if line.iter().all(u8::is_ascii_whitespace) || line.first() == Some(&b'#') {
            continue;
        }
        let Some(eq) = line.iter().position(|b| *b == b'=') else {
            out.skipped.push(Skipped::Line(n, SkipReason::NoEquals));
            continue;
        };
        let (name_bytes, value) = (&line[..eq], &line[eq + 1..]);
        if !is_valid_name(name_bytes) {
            out.skipped.push(Skipped::Line(n, SkipReason::BadName));
            continue;
        }
        // Valid names are ASCII.
        let name = String::from_utf8_lossy(name_bytes).into_owned();
        let reason = if is_refused_name(&name) {
            Some(SkipReason::Denylisted)
        } else if value.iter().any(|b| *b == 0 || *b == b'\r') {
            Some(SkipReason::BadByte)
        } else if value.len() > MAX_VALUE_BYTES {
            Some(SkipReason::ValueTooLong)
        } else if !index.contains_key(&name) && index.len() >= MAX_VARS {
            Some(SkipReason::TooManyVars)
        } else {
            None
        };
        if let Some(reason) = reason {
            out.skipped.push(Skipped::Name(name, reason));
            continue;
        }
        let entry = (OsString::from(&name), OsString::from_vec(value.to_vec()));
        match index.get(&name) {
            Some(i) => out.vars[*i] = entry,
            None => {
                index.insert(name, out.vars.len());
                out.vars.push(entry);
            }
        }
    }
    Ok(out)
}

/// Read and validate `<dir>/<unix_name>.env`. `Ok(None)`: there is no file
/// (the usual case for an account with no grants).
pub fn read(dir: &Path, unix_name: &str, expect: Expect) -> Result<Option<Parsed>, Refusal> {
    if !is_plain_component(unix_name) {
        return Err(Refusal::BadAccountName);
    }
    // The directory first: if it is not root's, nothing in it means anything.
    let dir_meta = match std::fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Refusal::Unreadable(e.kind())),
    };
    if dir_meta.file_type().is_symlink() {
        return Err(Refusal::DirIsSymlink);
    }
    if !dir_meta.is_dir() {
        return Err(Refusal::DirNotDirectory);
    }
    if dir_meta.uid() != expect.owner_uid {
        return Err(Refusal::DirNotRootOwned);
    }
    if dir_meta.mode() & 0o022 != 0 {
        return Err(Refusal::DirWritable);
    }

    let path = dir.join(format!("{unix_name}.env"));
    // O_NOFOLLOW: a symlink is ELOOP, never followed. O_NONBLOCK: a FIFO
    // planted there cannot hang the broker (it is then "not regular").
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => return Err(Refusal::Symlink),
        Err(e) => return Err(Refusal::Unreadable(e.kind())),
    };
    // fstat on the descriptor we will read: what was checked is what is read.
    let meta = file.metadata().map_err(|e| Refusal::Unreadable(e.kind()))?;
    if !meta.is_file() {
        return Err(Refusal::NotRegular);
    }
    if meta.uid() != expect.owner_uid {
        return Err(Refusal::WrongOwner);
    }
    if meta.gid() != expect.gid {
        return Err(Refusal::WrongGroup);
    }
    let mode = meta.mode();
    if mode & 0o007 != 0 {
        return Err(Refusal::WorldAccessible);
    }
    if mode & 0o020 != 0 {
        return Err(Refusal::GroupWritable);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Refusal::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    let result = file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Refusal::Unreadable(e.kind()))
        .and_then(|_| {
            if bytes.len() as u64 > MAX_FILE_BYTES {
                Err(Refusal::TooLarge)
            } else {
                parse(&bytes)
            }
        });
    bytes.zeroize();
    result.map(Some)
}

/// The environment to add to `principal`'s child: its own file, validated as
/// root would, or nothing. Logs by name only; never a value. Never an error:
/// a bad file must not stop the account from logging in, it just gets no
/// secrets (and the operator gets a warning).
pub fn for_principal(dir: &Path, principal: &Principal) -> Vec<(OsString, OsString)> {
    for_principal_as(
        dir,
        principal,
        Expect {
            owner_uid: 0,
            gid: principal.gid,
        },
    )
}

/// [`for_principal`] with the expected owner spelled out (tests).
pub(crate) fn for_principal_as(
    dir: &Path,
    principal: &Principal,
    expect: Expect,
) -> Vec<(OsString, OsString)> {
    let account = principal.unix_name.as_str();
    match read(dir, account, expect) {
        Ok(None) => Vec::new(),
        Ok(Some(parsed)) => {
            for s in &parsed.skipped {
                match s {
                    Skipped::Name(name, reason) => tracing::warn!(
                        account,
                        name = name.as_str(),
                        ?reason,
                        "account secrets: entry skipped"
                    ),
                    Skipped::Line(line, reason) => {
                        tracing::warn!(account, line, ?reason, "account secrets: line skipped")
                    }
                }
            }
            tracing::info!(
                account,
                names = ?parsed.names(),
                "account secrets: {} variable(s) added to the child's environment",
                parsed.vars.len()
            );
            parsed.into_vars()
        }
        Err(refusal) => {
            tracing::warn!(
                account,
                dir = %dir.display(),
                "account secrets file not used: {refusal}"
            );
            Vec::new()
        }
    }
}

/// The directory the broker reads, from `--account-secrets-dir`.
pub fn dir_or_default(flag: Option<&Path>) -> PathBuf {
    flag.map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::PrincipalId;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn me() -> (u32, u32) {
        // SAFETY: plain syscalls.
        unsafe { (libc::geteuid(), libc::getegid()) }
    }

    fn expect() -> Expect {
        let (uid, gid) = me();
        Expect {
            owner_uid: uid,
            gid,
        }
    }

    /// A trusted directory (our uid, 0755) with `ik-ada.env` (0640) in it.
    fn setup(contents: &[u8]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        put(tmp.path(), "ik-ada", contents, 0o640);
        tmp
    }

    fn put(dir: &Path, account: &str, contents: &[u8], mode: u32) {
        let f = dir.join(format!("{account}.env"));
        std::fs::write(&f, contents).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn vars(p: &Parsed) -> Vec<(String, String)> {
        p.vars
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    fn principal(name: &str) -> Principal {
        let (uid, gid) = me();
        Principal {
            id: PrincipalId::new_v7(),
            username: name.trim_start_matches("ik-").into(),
            unix_name: name.into(),
            uid,
            gid,
            home: "/home/x".into(),
            shell: "/bin/sh".into(),
        }
    }

    // ---- parser

    #[test]
    fn values_are_literal() {
        let src = b"# managed by provision.sh\n\
            A=plain\n\
            B=has = equals and # hash\n\
            C=  leading and trailing  \n\
            D=\"quoted\" 'single'\n\
            E=$(touch /tmp/pwned) `id` ${HOME} $HOME\n\
            F=back\\slash\\n\n\
            G=\n\
            H=\xff\xfe-not-utf8\n";
        let p = parse(src).unwrap();
        let got = vars(&p);
        assert_eq!(got[0], ("A".into(), "plain".into()));
        assert_eq!(got[1], ("B".into(), "has = equals and # hash".into()));
        assert_eq!(got[2], ("C".into(), "  leading and trailing  ".into()));
        assert_eq!(got[3], ("D".into(), "\"quoted\" 'single'".into()));
        assert_eq!(
            got[4],
            ("E".into(), "$(touch /tmp/pwned) `id` ${HOME} $HOME".into())
        );
        assert_eq!(got[5], ("F".into(), "back\\slash\\n".into()));
        assert_eq!(got[6], ("G".into(), "".into()));
        assert_eq!(p.vars[7].1.as_encoded_bytes(), b"\xff\xfe-not-utf8");
        assert!(p.skipped.is_empty(), "{:?}", p.skipped);
    }

    #[test]
    fn blanks_comments_and_a_missing_final_newline() {
        let p = parse(b"\n   \n# c\n#X=1\nA=1\n\nB=2").unwrap();
        assert_eq!(
            vars(&p),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        assert!(p.skipped.is_empty());
    }

    #[test]
    fn a_repeated_name_keeps_the_last_value_in_place() {
        let p = parse(b"A=1\nB=2\nA=3\n").unwrap();
        assert_eq!(
            vars(&p),
            vec![("A".into(), "3".into()), ("B".into(), "2".into())]
        );
    }

    #[test]
    fn bad_names_are_skipped_by_line_number_never_echoed() {
        let src = b"just-a-bare-secret-value\n\
            1BAD=x\n\
            has-dash=x\n\
            =novalue\n\
            sp ace=x\n\
            ok=1\n";
        let p = parse(src).unwrap();
        assert_eq!(vars(&p), vec![("ok".into(), "1".into())]);
        assert_eq!(
            p.skipped,
            vec![
                Skipped::Line(1, SkipReason::NoEquals),
                Skipped::Line(2, SkipReason::BadName),
                Skipped::Line(3, SkipReason::BadName),
                Skipped::Line(4, SkipReason::BadName),
                Skipped::Line(5, SkipReason::BadName),
            ]
        );
        // The Debug form names names, never values.
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("bare-secret"), "{dbg}");
    }

    #[test]
    fn denylisted_names_are_refused_like_the_provisioner_and_more() {
        // Everything provision.sh `secret_name_ok` refuses.
        for name in [
            "IKENGA_AUTH_TOKEN",
            "IKENGA_SECRET_X",
            "IKENGA_PRINCIPAL_SECRETS_KEY",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "BASH_ENV",
            "BASH_FUNC_x%%",
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "SHELL",
            "IFS",
            "ENV",
            "PS1",
            "PS2",
            "PS4",
            "PROMPT_COMMAND",
            "CDPATH",
            "GLOBIGNORE",
            "SHELLOPTS",
            "TMPDIR",
            "TERM",
            "PWD",
            "OLDPWD",
            "GIT_ASKPASS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_PROXY_COMMAND",
            "GIT_EXEC_PATH",
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_COUNT",
            // The daemon's own.
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
            "LANG",
            "LC_ALL",
            "TZ",
            "RUST_LOG",
            "RUST_BACKTRACE",
            "GLIBC_TUNABLES",
        ] {
            assert!(is_refused_name(name), "{name} should be refused");
        }
        // What the point of the exercise is: provider keys and friends.
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "FAL_KEY",
            "GITHUB_TOKEN",
            "GIT_TOKEN",
            "HTTPS_PROXY",
            "MY_IKENGA_SECRET",
            "lowercase_ok",
            "_UNDERSCORE_LEAD",
        ] {
            assert!(!is_refused_name(name), "{name} should be allowed");
        }
        let p = parse(b"LD_PRELOAD=/x.so\nPATH=/evil\nGIT_SSH_COMMAND=x\nFAL_KEY=k\n").unwrap();
        assert_eq!(vars(&p), vec![("FAL_KEY".into(), "k".into())]);
        assert_eq!(
            p.skipped,
            vec![
                Skipped::Name("LD_PRELOAD".into(), SkipReason::Denylisted),
                Skipped::Name("PATH".into(), SkipReason::Denylisted),
                Skipped::Name("GIT_SSH_COMMAND".into(), SkipReason::Denylisted),
            ]
        );
    }

    #[test]
    fn bad_bytes_and_oversized_values_skip_the_entry_by_name() {
        let mut src = b"CR=a\rb\nNUL=a\0b\nOK=1\n".to_vec();
        src.extend_from_slice(b"BIG=");
        src.extend(std::iter::repeat(b'x').take(MAX_VALUE_BYTES + 1));
        src.push(b'\n');
        let p = parse(&src).unwrap();
        assert_eq!(vars(&p), vec![("OK".into(), "1".into())]);
        assert_eq!(
            p.skipped,
            vec![
                Skipped::Name("CR".into(), SkipReason::BadByte),
                Skipped::Name("NUL".into(), SkipReason::BadByte),
                Skipped::Name("BIG".into(), SkipReason::ValueTooLong),
            ]
        );
    }

    #[test]
    fn caps_on_lines_and_variables() {
        let many_lines = "# c\n".repeat(MAX_LINES + 1);
        assert_eq!(
            parse(many_lines.as_bytes()).err(),
            Some(Refusal::TooManyLines)
        );
        let ok_lines = "# c\n".repeat(MAX_LINES);
        assert!(parse(ok_lines.as_bytes()).is_ok());

        let mut vars_src = String::new();
        for i in 0..(MAX_VARS + 3) {
            vars_src.push_str(&format!("V{i}=x\n"));
        }
        let p = parse(vars_src.as_bytes()).unwrap();
        assert_eq!(p.vars.len(), MAX_VARS);
        assert_eq!(p.skipped.len(), 3);
        assert!(p
            .skipped
            .iter()
            .all(|s| matches!(s, Skipped::Name(_, SkipReason::TooManyVars))));
        // Re-setting an accepted name past the cap still works.
        let mut again = vars_src.clone();
        again.push_str("V0=new\n");
        let p = parse(again.as_bytes()).unwrap();
        assert_eq!(vars(&p)[0], ("V0".into(), "new".into()));
    }

    // ---- file checks

    #[test]
    fn a_good_file_loads() {
        let tmp = setup(b"FAL_KEY=fake-fal\nANTHROPIC_API_KEY=fake-ant\n");
        let p = read(tmp.path(), "ik-ada", expect()).unwrap().unwrap();
        assert_eq!(
            vars(&p),
            vec![
                ("FAL_KEY".into(), "fake-fal".into()),
                ("ANTHROPIC_API_KEY".into(), "fake-ant".into())
            ]
        );
    }

    #[test]
    fn no_file_is_not_an_error() {
        let tmp = setup(b"A=1\n");
        assert!(read(tmp.path(), "ik-grace", expect()).unwrap().is_none());
        assert!(read(&tmp.path().join("nope"), "ik-ada", expect())
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_symlink_file_is_refused() {
        let tmp = setup(b"A=1\n");
        // Even a symlink to a perfectly good file.
        symlink(tmp.path().join("ik-ada.env"), tmp.path().join("ik-eve.env")).unwrap();
        assert_eq!(
            read(tmp.path(), "ik-eve", expect()).err(),
            Some(Refusal::Symlink)
        );
        // And a dangling one.
        symlink("/nonexistent", tmp.path().join("ik-bob.env")).unwrap();
        assert_eq!(
            read(tmp.path(), "ik-bob", expect()).err(),
            Some(Refusal::Symlink)
        );
    }

    #[test]
    fn a_non_regular_file_is_refused_without_blocking() {
        let tmp = setup(b"A=1\n");
        std::fs::create_dir(tmp.path().join("ik-dir.env")).unwrap();
        assert_eq!(
            read(tmp.path(), "ik-dir", expect()).err(),
            Some(Refusal::NotRegular)
        );
        let fifo = tmp.path().join("ik-fifo.env");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: plain syscall on a valid C string.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o640) }, 0);
        assert_eq!(
            read(tmp.path(), "ik-fifo", expect()).err(),
            Some(Refusal::NotRegular)
        );
    }

    #[test]
    fn the_wrong_owner_or_group_is_refused() {
        let tmp = setup(b"A=1\n");
        let (uid, gid) = me();
        assert_eq!(
            read(
                tmp.path(),
                "ik-ada",
                Expect {
                    owner_uid: uid,
                    gid: gid.wrapping_add(1)
                }
            )
            .err(),
            Some(Refusal::WrongGroup)
        );
        // The file is ours; "root" is someone else. (The directory check
        // comes first and says the same thing about the directory.)
        let err = read(
            tmp.path(),
            "ik-ada",
            Expect {
                owner_uid: uid.wrapping_add(1),
                gid,
            },
        )
        .err();
        assert_eq!(err, Some(Refusal::DirNotRootOwned));
    }

    #[test]
    fn a_file_owned_by_someone_else_in_a_trusted_dir_is_refused() {
        // Needs a second uid to own the file; only root can chown. Cover the
        // branch when we are root (CI containers), else rely on the group
        // test above and the directory check.
        let (uid, gid) = me();
        if uid != 0 {
            return;
        }
        let tmp = setup(b"A=1\n");
        let f = tmp.path().join("ik-ada.env");
        std::os::unix::fs::chown(&f, Some(12345), Some(gid)).unwrap();
        assert_eq!(
            read(tmp.path(), "ik-ada", expect()).err(),
            Some(Refusal::WrongOwner)
        );
    }

    #[test]
    fn world_bits_and_group_write_are_refused() {
        for mode in [0o644, 0o604, 0o602, 0o641, 0o660, 0o666, 0o770] {
            let tmp = setup(b"A=1\n");
            put(tmp.path(), "ik-ada", b"A=1\n", mode);
            let want = if mode & 0o007 != 0 {
                Refusal::WorldAccessible
            } else {
                Refusal::GroupWritable
            };
            assert_eq!(
                read(tmp.path(), "ik-ada", expect()).err(),
                Some(want),
                "mode {mode:o}"
            );
        }
        // 0600 and 0640 are both fine.
        for mode in [0o600, 0o640, 0o440] {
            let tmp = setup(b"A=1\n");
            put(tmp.path(), "ik-ada", b"A=1\n", mode);
            assert!(read(tmp.path(), "ik-ada", expect()).is_ok(), "{mode:o}");
        }
    }

    #[test]
    fn an_untrusted_directory_is_refused() {
        for (mode, want) in [
            (0o775, Refusal::DirWritable),
            (0o757, Refusal::DirWritable),
            (0o777, Refusal::DirWritable),
        ] {
            let tmp = setup(b"A=1\n");
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(read(tmp.path(), "ik-ada", expect()).err(), Some(want));
        }
        // 0711 (what the provisioner makes) is fine.
        let tmp = setup(b"A=1\n");
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
        assert!(read(tmp.path(), "ik-ada", expect()).unwrap().is_some());

        // A symlinked directory.
        let real = setup(b"A=1\n");
        let holder = tempfile::tempdir().unwrap();
        let link = holder.path().join("secrets");
        symlink(real.path(), &link).unwrap();
        assert_eq!(
            read(&link, "ik-ada", expect()).err(),
            Some(Refusal::DirIsSymlink)
        );
        // A regular file where the directory should be.
        let f = holder.path().join("file");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(
            read(&f, "ik-ada", expect()).err(),
            Some(Refusal::DirNotDirectory)
        );
    }

    #[test]
    fn size_cap_refuses_the_whole_file() {
        let tmp = setup(b"A=1\n");
        let big = vec![b'#'; (MAX_FILE_BYTES + 1) as usize];
        put(tmp.path(), "ik-ada", &big, 0o640);
        assert_eq!(
            read(tmp.path(), "ik-ada", expect()).err(),
            Some(Refusal::TooLarge)
        );
        let exact = vec![b'#'; MAX_FILE_BYTES as usize];
        put(tmp.path(), "ik-ada", &exact, 0o640);
        assert!(read(tmp.path(), "ik-ada", expect()).is_ok());
    }

    #[test]
    fn the_account_name_cannot_escape_the_directory() {
        let tmp = setup(b"A=1\n");
        // A secret file one level up that a traversal would reach.
        let outer = tmp.path().parent().unwrap().join("ik-evil.env");
        let _ = std::fs::write(&outer, b"EVIL=1\n");
        for bad in [
            "../ik-evil",
            "..",
            ".",
            "a/b",
            "/etc/passwd",
            "",
            ".hidden",
            "a..b",
            "x y",
            "a\0b",
        ] {
            assert_eq!(
                read(tmp.path(), bad, expect()).err(),
                Some(Refusal::BadAccountName),
                "{bad:?}"
            );
        }
        let _ = std::fs::remove_file(outer);
    }

    // ---- propagation inside the child

    /// What the child does with its own environment next: the PTY rebuild
    /// keeps everything that is not `pty::is_host_only_env`, and Chi runs /
    /// engine CLIs / the chi-runner start from `chi_exec::scrubbed_env`
    /// (pkg MCP servers and sidecars do not clear at all). So an accepted
    /// variable is carried everywhere, and the box-wide `IKENGA_SECRET_*`
    /// the child also holds is carried nowhere.
    #[test]
    fn accepted_variables_survive_the_childs_scrubs_and_box_wide_ones_do_not() {
        let p = parse(
            b"FAL_KEY=k\nANTHROPIC_API_KEY=a\nhttps_proxy=http://p\nIKENGA_SECRET_FAL=smuggled\n",
        )
        .unwrap();
        assert_eq!(p.vars.len(), 3);
        for (k, _) in &p.vars {
            assert!(
                !crate::pty::is_host_only_env(&k.to_string_lossy()),
                "{k:?} would be stripped from PTYs"
            );
        }
        // The child's process env: the account's vars + the broker's hand-off.
        let mut child_env = p.vars.clone();
        child_env.push(("IKENGA_SECRET_DEMO_KEY".into(), "box-wide".into()));
        child_env.push(("IKENGA_AUTH_TOKEN".into(), "tok".into()));
        let chi = crate::server::shared::chi_exec::scrubbed_env(child_env);
        let names: Vec<String> = chi
            .iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["FAL_KEY", "ANTHROPIC_API_KEY", "https_proxy"]);
    }

    /// The file can never address a name the daemon strips on purpose.
    #[test]
    fn nothing_host_only_can_be_named_in_the_file() {
        for name in [
            "IKENGA_AUTH_TOKEN",
            "IKENGA_VAULT_KEY",
            "IKENGA_PKG_DB_TOKEN",
            crate::secrets::principal_store::WRAP_KEY_ENV,
            "IKENGA_SECRET_FAL",
            "IKENGA_BOOTSTRAP_ADMIN",
        ] {
            assert!(is_refused_name(name), "{name}");
        }
    }

    // ---- per-principal isolation

    #[test]
    fn one_accounts_file_is_never_read_for_another() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        put(
            tmp.path(),
            "ik-ada",
            b"ADA_ONLY=fake-ada\nSHARED=ada\n",
            0o640,
        );
        put(
            tmp.path(),
            "ik-grace",
            b"GRACE_ONLY=fake-grace\nSHARED=grace\n",
            0o640,
        );
        let e = expect();
        let ada = for_principal_as(tmp.path(), &principal("ik-ada"), e);
        let grace = for_principal_as(tmp.path(), &principal("ik-grace"), e);
        let none = for_principal_as(tmp.path(), &principal("ik-nobody"), e);
        let names = |v: &[(OsString, OsString)]| -> Vec<String> {
            v.iter()
                .map(|(k, _)| k.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names(&ada), vec!["ADA_ONLY", "SHARED"]);
        assert_eq!(names(&grace), vec!["GRACE_ONLY", "SHARED"]);
        assert!(none.is_empty());
        assert_eq!(ada[1].1, OsString::from("ada"));
        assert_eq!(grace[1].1, OsString::from("grace"));
        // A principal whose own file is bad gets nothing, not a neighbour's.
        put(tmp.path(), "ik-ada", b"ADA_ONLY=fake-ada\n", 0o644);
        assert!(for_principal_as(tmp.path(), &principal("ik-ada"), e).is_empty());
        assert_eq!(
            for_principal_as(tmp.path(), &principal("ik-grace"), e).len(),
            2
        );
    }

    #[test]
    fn production_expects_root_and_the_principals_gid() {
        // `for_principal` is `for_principal_as` with owner 0 and the
        // principal's gid; unless we are root, our own file does not pass.
        let tmp = setup(b"A=1\n");
        let (uid, _) = me();
        let got = for_principal(tmp.path(), &principal("ik-ada"));
        if uid == 0 {
            assert_eq!(got.len(), 1);
        } else {
            assert!(got.is_empty(), "a non-root-owned directory must be refused");
        }
    }

    #[test]
    fn default_dir_and_override() {
        assert_eq!(dir_or_default(None), PathBuf::from("/etc/ikenga/secrets"));
        assert_eq!(
            dir_or_default(Some(Path::new("/srv/s"))),
            PathBuf::from("/srv/s")
        );
    }
}
