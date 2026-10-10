//! The daemon's rules for the git / npx fetch edge of the Ọba installers
//! (WP-18b part c).
//!
//! The desktop installs on behalf of the user sitting at the machine, so a
//! git URL may be `file://…` or a local path, a ref is whatever the user typed
//! and a spawn inherits the app's environment. A browser session on
//! `ikenga-server` is a remote token holder. This module is the stricter
//! policy that applies **only** there, switched on by [`scoped`] around the
//! blocking install core (the daemon wrappers in `install.rs`); with no scope
//! active every function here is a no-op and the desktop is byte-for-byte what
//! it was.
//!
//! What the policy does, in the order a fetch meets it:
//!
//! * **Source allow-list** ([`check_git_url`], [`check_npx_spec`],
//!   [`check_git_ref`]). A git URL must be `https://` to a public DNS name — no
//!   `file://`, `ext::`, `fd::`, `git@`, `ssh://`, plain `http`, local path,
//!   userinfo (credentials would be written into `registry.json`), IP literal,
//!   `localhost`/single-label/`.local`/`.internal` host or non-443 port (no
//!   SSRF probe of the box's own network). An npx spec must be `owner/repo`
//!   (optionally `github:`-prefixed) or such a URL. Nothing starting with `-`
//!   ever reaches a command line. The same allow-list is enforced again at the
//!   spawn by `GIT_ALLOW_PROTOCOL=https`, which also covers the redirects and
//!   submodule URLs a fetched repo can name.
//! * **No local sources** ([`refuse_local`]). `oba_install_local` stays
//!   desktop-only; a `local` source smuggled in through `oba_install_with_deps`
//!   (its `source` and its catalog rows are caller-supplied) is refused where
//!   it is staged.
//! * **The principal's environment, minus the host's** ([`apply_env`]). A
//!   spawn is given this process's environment (HOME, PATH, proxies, the
//!   account's own granted variables — under T1 this process IS the account's
//!   child) with every `IKENGA_*`, every `GIT_*` and `SSH_AUTH_SOCK` removed:
//!   the child's own bearer token and the operator's `IKENGA_SECRET_*` defaults
//!   never reach a fetched program, and a `GIT_SSH_COMMAND` / `GIT_CONFIG_*` /
//!   `GIT_EXEC_PATH` smuggled into the environment cannot redirect git.
//!   `npm_config_ignore_scripts=true` means no lifecycle script of anything
//!   `npx` downloads runs (the `skills` CLI itself still does — that is what
//!   `npx skills add` is).
//! * **Bounded** ([`budget`], [`run_bounded`]). Every spawn has a deadline and,
//!   on expiry, its whole process group is killed; the sequence of spawns an
//!   install makes shares one wall-clock budget, so a dependency closure
//!   cannot run for ever. One daemon install runs at a time ([`scoped`]
//!   serialises them — `registry.json` is a read-modify-write file).
//! * **What lands is vetted** ([`vet_tree`]). Before a fetched tree is located,
//!   hashed or copied into the vault, every symlink in it must resolve inside
//!   the tree (the copy follows links, so one pointing at `~/.ssh/id_ed25519`
//!   would otherwise be lifted into the vault and read back by a share), no
//!   link may loop back on a parent, nothing but files, dirs and links may
//!   exist (a FIFO would hang the copy), and the tree is capped in bytes and
//!   entries.
//!
//! Whose process and whose home: the arms run in the signed-in account's
//! principal child under T1 (`executor::current()` there spawns as that uid
//! with that `HOME` / `TMPDIR`), or in the daemon's own process under T0
//! (single-tenant: the daemon user is the one account). Nothing here switches
//! user; it relies on that and refuses to widen it.

use std::cell::Cell;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::executor::{PipedOpts, SpawnSpec};

/// Wall-clock budget for one install / update / resolve (all its spawns).
pub(crate) const TOTAL_BUDGET: Duration = Duration::from_secs(20 * 60);
/// `git ls-remote`.
const LS_REMOTE_LIMIT: Duration = Duration::from_secs(30);
/// Any other `git` (clone, fetch, rev-parse, checkout).
const GIT_LIMIT: Duration = Duration::from_secs(180);
/// `npx --yes skills add` (downloads the CLI, then the source).
const NPX_LIMIT: Duration = Duration::from_secs(300);

/// A fetched tree larger than this is refused (bytes of regular files).
pub(crate) const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;
/// … or with more entries than this.
pub(crate) const MAX_TREE_ENTRIES: usize = 20_000;
const MAX_TREE_DEPTH: usize = 48;

/// The class of a spawn, which picks its time limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpawnClass {
    LsRemote,
    Git,
    Npx,
}

impl SpawnClass {
    fn limit(self) -> Duration {
        match self {
            SpawnClass::LsRemote => LS_REMOTE_LIMIT,
            SpawnClass::Git => GIT_LIMIT,
            SpawnClass::Npx => NPX_LIMIT,
        }
    }
}

thread_local! {
    /// The active scope's deadline on this thread. The cores are synchronous,
    /// so one thread runs a whole install; a thread that never entered
    /// [`scoped`] (the desktop's) sees `None` and every check passes through.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// One daemon install at a time: the registry is read, changed and rewritten
/// without a lock of its own.
static SERIAL: Mutex<()> = Mutex::new(());

/// Run `f` (a blocking install core) under the daemon policy. Not re-entrant:
/// the daemon wrappers call it once per request, from a blocking thread.
pub(crate) fn scoped<R>(f: impl FnOnce() -> R) -> R {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    struct Restore(Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DEADLINE.with(|d| d.set(self.0));
        }
    }
    let _restore = Restore(DEADLINE.with(|d| d.replace(Some(Instant::now() + TOTAL_BUDGET))));
    f()
}

/// Is the daemon policy in force on this thread?
pub(crate) fn active() -> bool {
    DEADLINE.with(|d| d.get()).is_some()
}

/// The time this spawn may take: `None` off-policy (the desktop's unbounded
/// blocking spawn), `Some(Err)` when the install's budget is spent, else the
/// class limit capped by what is left of the budget.
pub(crate) fn budget(class: SpawnClass) -> Option<Result<Duration, String>> {
    let deadline = DEADLINE.with(|d| d.get())?;
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Some(Err(format!(
            "the install ran past its {}-minute budget; nothing further was fetched",
            TOTAL_BUDGET.as_secs() / 60
        )));
    }
    Some(Ok(class.limit().min(left)))
}

// ─── source allow-list ───────────────────────────────────────────────────────

const HTTPS_ONLY: &str = "a remote install fetches over https from a public host";

/// Refuse (under the policy) a git URL that is not `https://<public DNS name>`.
pub(crate) fn check_git_url(url: &str) -> Result<(), String> {
    if !active() {
        return Ok(());
    }
    check_https_url(url)
}

fn check_https_url(url: &str) -> Result<(), String> {
    if url.is_empty() || url.len() > 2048 {
        return Err(format!("{HTTPS_ONLY}: the URL is empty or too long"));
    }
    if url != url.trim() || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("{HTTPS_ONLY}: the URL contains whitespace"));
    }
    if url.starts_with('-') {
        return Err(format!("{HTTPS_ONLY}: the URL may not start with `-`"));
    }
    let parsed = url::Url::parse(url)
        .map_err(|_| format!("{HTTPS_ONLY}: `{}` is not an https:// URL", shown(url)))?;
    if parsed.scheme() != "https" {
        return Err(format!(
            "{HTTPS_ONLY}: `{}` uses the `{}` transport (local paths, file://, ssh, git@, ext:: and plain http are desktop-only)",
            shown(url),
            parsed.scheme()
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "{HTTPS_ONLY}: credentials in a URL are not accepted (they would be recorded in the registry)"
        ));
    }
    // git and curl parse the RAW string, not the WHATWG form `url` checked,
    // so the raw text must say exactly what was checked: `https://` written
    // out, no backslash (WHATWG reads `\` as `/`, curl does not:
    // `https://github.com\@127.0.0.1/x` would connect to 127.0.0.1), and an
    // authority that is the checked host (optionally `:443`) verbatim.
    if !url.starts_with("https://") || url.contains('\\') {
        return Err(format!(
            "{HTTPS_ONLY}: `{}` is not a plain https:// URL",
            shown(url)
        ));
    }
    let raw_authority = url["https://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let host = match parsed.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(_) => {
            return Err(format!(
                "{HTTPS_ONLY}: an IP address is not a public host name"
            ))
        }
        None => return Err(format!("{HTTPS_ONLY}: the URL has no host")),
    };
    if raw_authority != host && raw_authority != format!("{host}:443") {
        return Err(format!(
            "{HTTPS_ONLY}: `{}` is not a plain https:// URL",
            shown(url)
        ));
    }
    // A trailing dot is the same name (`localhost.` resolves to 127.0.0.1).
    let host = host.trim_end_matches('.').to_string();
    let internal = host == "localhost"
        || !host.contains('.')
        || [
            ".localhost",
            ".local",
            ".internal",
            ".lan",
            ".home",
            ".corp",
        ]
        .iter()
        .any(|s| host.ends_with(s));
    if internal {
        return Err(format!("{HTTPS_ONLY}: `{host}` is not a public host name"));
    }
    if parsed.port().is_some_and(|p| p != 443) {
        return Err(format!(
            "{HTTPS_ONLY}: only the default https port is allowed"
        ));
    }
    if parsed.path().is_empty() || parsed.path() == "/" {
        return Err(format!("{HTTPS_ONLY}: the URL names no repository"));
    }
    Ok(())
}

/// The text for an error line: control-free and bounded.
fn shown(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect::<String>()
}

/// Refuse (under the policy) an npx spec that is not `owner/repo`,
/// `github:owner/repo` or an acceptable https URL.
pub(crate) fn check_npx_spec(spec: &str) -> Result<(), String> {
    if !active() {
        return Ok(());
    }
    if spec.starts_with("https://") {
        return check_https_url(spec);
    }
    let bare = spec.strip_prefix("github:").unwrap_or(spec);
    let seg_ok = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p.as_bytes()[0].is_ascii_alphanumeric()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    };
    let mut parts = bare.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(o), Some(r), None) if seg_ok(o) && seg_ok(r) => Ok(()),
        _ => Err(format!(
            "{HTTPS_ONLY}: `{}` is not an owner/repo spec or an https:// URL (local paths and file:// are desktop-only)",
            shown(spec)
        )),
    }
}

/// Refuse (under the policy) a git ref that could be read as an option or
/// that git would not treat as a plain branch / tag name.
pub(crate) fn check_git_ref(r: &str) -> Result<(), String> {
    if !active() {
        return Ok(());
    }
    let ok = !r.is_empty()
        && r.len() <= 200
        && !r.starts_with('-')
        && !r.contains("..")
        && r.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-+@".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "git ref `{}` is not a plain branch or tag name",
            shown(r)
        ))
    }
}

/// Refuse (under the policy) a `local` source.
pub(crate) fn refuse_local() -> Result<(), String> {
    if active() {
        return Err(
            "local-path installs are desktop-only: this server never reads a path it was handed \
             — install from a git URL instead"
                .to_string(),
        );
    }
    Ok(())
}

// ─── environment ─────────────────────────────────────────────────────────────

/// Whether `key` is withheld from a fetch spawn.
fn is_withheld(key: &str) -> bool {
    crate::pty::is_host_only_env(key)
        || key.starts_with("IKENGA_")
        || key.starts_with("GIT_")
        || key == "SSH_AUTH_SOCK"
}

/// `vars` minus what [`is_withheld`] names. Pure, so tests need not touch the
/// process environment.
pub(crate) fn scrubbed<I>(vars: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .filter(|(k, _)| !is_withheld(&k.to_string_lossy()))
        .collect()
}

/// Give `spec` the principal's environment minus the host's, explicitly
/// (`env_clear` first — filtering without clearing filters nothing). Call
/// before any other `.env()` on the spec: later entries override earlier ones.
/// A no-op off-policy.
pub(crate) fn apply_env(spec: &mut SpawnSpec) {
    if !active() {
        return;
    }
    spec.env_clear();
    spec.envs(scrubbed(std::env::vars_os()));
    // Re-state the transport allow-list in the spawn itself (git, and every
    // git the `skills` CLI starts), and never wait on a credential prompt.
    spec.env("GIT_ALLOW_PROTOCOL", "https")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never");
}

/// The extra environment of an `npx skills add` under the policy: no
/// lifecycle scripts, no update / fund / audit chatter, no telemetry. The npm
/// cache is a per-install dir under `cache` (the staging HOME).
pub(crate) fn apply_npx_env(spec: &mut SpawnSpec, cache: &Path) {
    if !active() {
        return;
    }
    spec.env("npm_config_ignore_scripts", "true")
        .env("npm_config_update_notifier", "false")
        .env("npm_config_fund", "false")
        .env("npm_config_audit", "false")
        .env("npm_config_cache", cache.join(".npm"))
        .env("DISABLE_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1");
}

// ─── bounded spawn ───────────────────────────────────────────────────────────

/// Spawn `spec` through `executor::current()` as a piped child, wait at most
/// `limit` for it and, on expiry, kill its whole process group (git spawns
/// `git-remote-https`; npx spawns node). Must run on a thread that is not
/// driving async tasks — the daemon wrappers call it from `spawn_blocking`.
pub(crate) fn run_bounded(spec: SpawnSpec, opts: PipedOpts, limit: Duration) -> io::Result<Output> {
    let mut opts = opts;
    opts.kill_on_drop = true;
    opts.new_process_group = true;

    // On a blocking thread of the daemon's runtime, use that runtime. With no
    // runtime (a plain thread), a private one: a `current_thread` runtime's
    // `Handle::block_on` does not drive its IO / child reaper, so it must be
    // the runtime's own `block_on`.
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let _enter = handle.enter();
            let child = crate::executor::current().spawn_piped(spec, opts)?;
            handle.block_on(wait_bounded(child, limit))
        }
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let _enter = rt.enter();
            let child = crate::executor::current().spawn_piped(spec, opts)?;
            rt.block_on(wait_bounded(child, limit))
        }
    }
}

async fn wait_bounded(child: tokio::process::Child, limit: Duration) -> io::Result<Output> {
    let pid = child.id();
    match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(out) => out,
        Err(_) => {
            kill_group(pid);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out after {}s and was stopped", limit.as_secs()),
            ))
        }
    }
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: plain syscall; the child led its own group (pgid == pid).
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

// ─── vetting a fetched tree ──────────────────────────────────────────────────

/// Refuse (under the policy) a fetched tree the vault must not copy.
///
/// `contain` is the root every symlink must resolve inside; `walk` are the
/// directories actually inspected (for a git clone both are the clone, `.git`
/// at its root skipped — it is never copied; for the `skills` CLI the three
/// skills dirs under a staging HOME that also holds its npm cache, which is
/// neither copied nor counted).
pub(crate) fn vet_tree(contain: &Path, walk: &[PathBuf]) -> Result<(), String> {
    if !active() {
        return Ok(());
    }
    let root = contain
        .canonicalize()
        .map_err(|e| format!("fetched tree unreadable: {e}"))?;
    let mut entries = 0usize;
    let mut bytes = 0u64;
    for dir in walk {
        if std::fs::symlink_metadata(dir).is_err() {
            continue;
        }
        let skip_git = dir == contain;
        walk_dir(dir, &root, 0, skip_git, &mut entries, &mut bytes)?;
    }
    Ok(())
}

fn walk_dir(
    dir: &Path,
    root: &Path,
    depth: usize,
    skip_git: bool,
    entries: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err("fetched tree is nested too deeply".to_string());
    }
    let rd = std::fs::read_dir(dir).map_err(|e| format!("fetched tree unreadable: {e}"))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("fetched tree unreadable: {e}"))?;
        let path = entry.path();
        if skip_git && entry.file_name() == ".git" {
            continue;
        }
        *entries += 1;
        if *entries > MAX_TREE_ENTRIES {
            return Err(format!(
                "fetched tree has more than {MAX_TREE_ENTRIES} entries; refusing to copy it"
            ));
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("fetched tree unreadable: {e}"))?;
        let ft = meta.file_type();
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if ft.is_symlink() {
            let target = path.canonicalize().map_err(|_| {
                format!(
                    "fetched tree has a symlink that does not resolve ({})",
                    shown(&rel.to_string_lossy())
                )
            })?;
            if !target.starts_with(root) {
                return Err(format!(
                    "fetched tree has a symlink that leaves the repository ({}); refusing to copy it",
                    shown(&rel.to_string_lossy())
                ));
            }
            // A link to a directory that contains the link itself would make
            // the copy recurse for ever.
            if target.is_dir() {
                let parent = path
                    .parent()
                    .and_then(|p| p.canonicalize().ok())
                    .unwrap_or_else(|| root.to_path_buf());
                if parent.starts_with(&target) {
                    return Err(format!(
                        "fetched tree has a symlink loop ({}); refusing to copy it",
                        shown(&rel.to_string_lossy())
                    ));
                }
            }
            if let Ok(m) = std::fs::metadata(&target) {
                if m.is_file() {
                    *bytes += m.len();
                }
            }
        } else if ft.is_dir() {
            walk_dir(&path, root, depth + 1, false, entries, bytes)?;
        } else if ft.is_file() {
            *bytes += meta.len();
        } else {
            return Err(format!(
                "fetched tree has a special file ({}); refusing to copy it",
                shown(&rel.to_string_lossy())
            ));
        }
        if *bytes > MAX_TREE_BYTES {
            return Err(format!(
                "fetched tree is larger than {} MiB; refusing to copy it",
                MAX_TREE_BYTES / (1024 * 1024)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on<R>(f: impl FnOnce() -> R) -> R {
        scoped(f)
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "oba-remote-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn off_policy_everything_passes_through() {
        assert!(!active());
        assert!(check_git_url("file:///etc").is_ok());
        assert!(check_git_url("/home/x/repo").is_ok());
        assert!(check_npx_spec("../x").is_ok());
        assert!(check_git_ref("--upload-pack=x").is_ok());
        assert!(refuse_local().is_ok());
        assert!(budget(SpawnClass::Git).is_none());
        let mut spec = SpawnSpec::new("git");
        apply_env(&mut spec);
        assert!(!spec.env.clear && spec.env.vars.is_empty());
    }

    #[test]
    fn scope_is_per_call_and_restores() {
        assert!(!active());
        on(|| assert!(active()));
        assert!(!active());
    }

    #[test]
    fn git_urls_must_be_public_https() {
        on(|| {
            for ok in [
                "https://github.com/anthropics/skills",
                "https://github.com/o/r.git",
                "https://gitlab.com/o/r",
                "https://git.example.org:443/o/r",
            ] {
                assert!(check_git_url(ok).is_ok(), "{ok}");
            }
            for bad in [
                "file:///tmp/x",
                "file:///home/u/.ssh",
                "/home/u/repo",
                "./repo",
                "~/repo",
                "../repo",
                "ext::sh -c touch /tmp/pwned",
                "fd::3",
                "git@github.com:o/r.git",
                "ssh://git@github.com/o/r",
                "git://github.com/o/r",
                "http://github.com/o/r",
                "--upload-pack=touch /tmp/x",
                "-ohttps://github.com/o/r",
                "https://user:pw@github.com/o/r",
                "https://github.com\\@127.0.0.1:1/x",
                "https://github.com\\@localhost/x",
                "https://localhost./o/r",
                "https://foo.local./o/r",
                "https:/github.com/o/r",
                "https:github.com/o/r",
                "HTTPS://localhost/o/r",
                "https://github.com:0443/o/r",
                "https://user@github.com/o/r",
                "https://127.0.0.1/o/r",
                "https://[::1]/o/r",
                "https://169.254.169.254/latest/meta-data",
                "https://localhost/o/r",
                "https://foo.localhost/o/r",
                "https://intranet/o/r",
                "https://box.internal/o/r",
                "https://github.com:8080/o/r",
                "https://github.com",
                "https://github.com/",
                " https://github.com/o/r",
                "https://github.com/o/r\n--x",
                "",
            ] {
                assert!(check_git_url(bad).is_err(), "{bad:?} must be refused");
            }
        });
    }

    #[test]
    fn npx_specs_must_be_owner_repo_or_https() {
        on(|| {
            for ok in [
                "anthropics/skills",
                "github:o/r",
                "o/r.js",
                "https://github.com/o/r",
            ] {
                assert!(check_npx_spec(ok).is_ok(), "{ok}");
            }
            for bad in [
                "",
                "./local",
                "../x",
                "/abs/path",
                "~/x",
                "a/b/c",
                "-rf/x",
                "o/-r",
                ".hidden/r",
                "o/r --skill x",
                "file:///tmp/x",
                "git@github.com:o/r",
                "http://github.com/o/r",
                "https://127.0.0.1/o/r",
                "github:",
                "owner",
            ] {
                assert!(check_npx_spec(bad).is_err(), "{bad:?} must be refused");
            }
        });
    }

    #[test]
    fn refs_are_plain_names() {
        on(|| {
            for ok in ["main", "v1.2.3", "feature/x-y", "release+1"] {
                assert!(check_git_ref(ok).is_ok(), "{ok}");
            }
            for bad in ["", "-x", "--upload-pack=x", "a..b", "a b", "a;b", "$(x)"] {
                assert!(check_git_ref(bad).is_err(), "{bad:?}");
            }
        });
    }

    #[test]
    fn local_is_refused_only_on_policy() {
        assert!(refuse_local().is_ok());
        on(|| assert!(refuse_local().unwrap_err().contains("desktop-only")));
    }

    #[test]
    fn env_scrub_drops_host_secrets_git_and_agent() {
        let vars: Vec<(OsString, OsString)> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/a"),
            ("HTTPS_PROXY", "http://p"),
            ("IKENGA_AUTH_TOKEN", "t"),
            ("IKENGA_VAULT_KEY", "k"),
            ("IKENGA_SECRET_FAL", "s"),
            ("IKENGA_DATA_DIR", "/d"),
            ("IKENGA_BOOTSTRAP_ADMIN", "x"),
            ("GIT_SSH_COMMAND", "evil"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_EXEC_PATH", "/evil"),
            ("SSH_AUTH_SOCK", "/s"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let kept: Vec<String> = scrubbed(vars)
            .into_iter()
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept, ["PATH", "HOME", "HTTPS_PROXY"]);
    }

    #[test]
    fn apply_env_clears_and_pins_the_transport_on_policy() {
        on(|| {
            let mut spec = SpawnSpec::new("git");
            apply_env(&mut spec);
            assert!(spec.env.clear);
            let get = |k: &str| {
                spec.env
                    .vars
                    .iter()
                    .rev()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.to_string_lossy().into_owned())
            };
            assert_eq!(get("GIT_ALLOW_PROTOCOL").as_deref(), Some("https"));
            assert_eq!(get("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
            assert!(get("IKENGA_AUTH_TOKEN").is_none());
            let mut npx = SpawnSpec::new("npx");
            apply_npx_env(&mut npx, Path::new("/stage"));
            assert!(npx
                .env
                .vars
                .iter()
                .any(|(k, v)| k == "npm_config_ignore_scripts" && v == "true"));
        });
    }

    #[test]
    fn budget_is_capped_by_what_is_left() {
        on(|| {
            let g = budget(SpawnClass::Git).unwrap().unwrap();
            assert!(g <= GIT_LIMIT);
            let l = budget(SpawnClass::LsRemote).unwrap().unwrap();
            assert!(l <= LS_REMOTE_LIMIT);
        });
        // A spent budget refuses.
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        DEADLINE.with(|d| d.set(Some(Instant::now() - Duration::from_secs(1))));
        let r = budget(SpawnClass::Git).unwrap();
        DEADLINE.with(|d| d.set(None));
        assert!(r.unwrap_err().contains("budget"));
    }

    #[cfg(unix)]
    #[test]
    fn vet_refuses_escaping_loop_and_dangling_links() {
        use std::os::unix::fs::symlink;
        let base = tmp("vet");
        let secret = base.join("secret.txt");
        std::fs::write(&secret, "s3cret").unwrap();

        // clean tree with an in-tree file link and a dir link: passes
        let ok = base.join("ok");
        std::fs::create_dir_all(ok.join("real")).unwrap();
        std::fs::write(ok.join("real/SKILL.md"), "x").unwrap();
        symlink("real/SKILL.md", ok.join("link.md")).unwrap();
        symlink("real", ok.join("dirlink")).unwrap();
        std::fs::create_dir_all(ok.join(".git")).unwrap();
        symlink(&secret, ok.join(".git/ignored")).unwrap(); // not walked
        on(|| assert!(vet_tree(&ok, std::slice::from_ref(&ok)).is_ok()));

        // absolute link out of the tree
        let esc = base.join("esc");
        std::fs::create_dir_all(&esc).unwrap();
        symlink(&secret, esc.join("SKILL.md")).unwrap();
        let e = on(|| vet_tree(&esc, std::slice::from_ref(&esc))).unwrap_err();
        assert!(e.contains("leaves the repository"), "{e}");

        // relative link out of the tree
        let rel = base.join("rel");
        std::fs::create_dir_all(&rel).unwrap();
        symlink("../secret.txt", rel.join("SKILL.md")).unwrap();
        assert!(on(|| vet_tree(&rel, std::slice::from_ref(&rel))).is_err());

        // dir link out (e.g. to the home dir)
        let dir_out = base.join("dir_out");
        std::fs::create_dir_all(&dir_out).unwrap();
        symlink(&base, dir_out.join("home")).unwrap();
        assert!(on(|| vet_tree(&dir_out, std::slice::from_ref(&dir_out))).is_err());

        // dangling
        let dang = base.join("dang");
        std::fs::create_dir_all(&dang).unwrap();
        symlink("nope", dang.join("x")).unwrap();
        let e = on(|| vet_tree(&dang, std::slice::from_ref(&dang))).unwrap_err();
        assert!(e.contains("does not resolve"), "{e}");

        // a link to its own parent loops the copy
        let lp = base.join("lp");
        std::fs::create_dir_all(lp.join("sub")).unwrap();
        symlink("..", lp.join("sub/up")).unwrap();
        let e = on(|| vet_tree(&lp, std::slice::from_ref(&lp))).unwrap_err();
        assert!(e.contains("loop"), "{e}");

        // off-policy: nothing is checked
        assert!(vet_tree(&esc, std::slice::from_ref(&esc)).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn vet_refuses_a_fifo() {
        let base = tmp("vet2");
        let fifo = base.join("pipe");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let e = on(|| vet_tree(&base, std::slice::from_ref(&base))).unwrap_err();
        assert!(e.contains("special file"), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn vet_walks_only_the_listed_dirs_but_contains_in_the_root() {
        use std::os::unix::fs::symlink;
        // The `skills` CLI layout: .claude/skills/x -> ../../.agents/skills/x
        // leaves `.claude/skills` yet stays inside the staging root.
        let stage = tmp("vet3");
        std::fs::create_dir_all(stage.join(".agents/skills/x")).unwrap();
        std::fs::write(stage.join(".agents/skills/x/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(stage.join(".claude/skills")).unwrap();
        symlink("../../.agents/skills/x", stage.join(".claude/skills/x")).unwrap();
        // A hostile npm cache is not walked.
        std::fs::create_dir_all(stage.join(".npm")).unwrap();
        symlink("/etc/passwd", stage.join(".npm/evil")).unwrap();
        let walk = vec![
            stage.join(".agents/skills"),
            stage.join(".claude/skills"),
            stage.join("skills"),
        ];
        assert!(on(|| vet_tree(&stage, &walk)).is_ok());
        let _ = std::fs::remove_dir_all(&stage);
    }

    #[cfg(unix)]
    const OPTS: PipedOpts = PipedOpts {
        stdin: crate::executor::StdioMode::Null,
        stdout: crate::executor::StdioMode::Piped,
        stderr: crate::executor::StdioMode::Piped,
        kill_on_drop: false,
        no_console_window: true,
        detached: false,
        new_process_group: false,
    };

    /// The bounded spawn goes through `executor::current()`, returns the
    /// child's output and exit status, and on expiry stops the child and the
    /// grandchildren in its process group.
    #[cfg(unix)]
    #[test]
    fn run_bounded_collects_output_and_stops_a_runaway_group() {
        let mut ok = SpawnSpec::new("sh");
        ok.args(["-c", "echo out; echo err >&2; exit 3"]);
        let out = run_bounded(ok, OPTS, Duration::from_secs(20)).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "out");
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err");
        assert_eq!(out.status.code(), Some(3));

        // A runaway child with a grandchild holding the pipes open.
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");
        let started = Instant::now();
        let mut slow = SpawnSpec::new("sh");
        slow.args([
            "-c".to_string(),
            format!("sleep 60 & echo $! > '{}'; sleep 60", pidfile.display()),
        ]);
        let err = run_bounded(slow, OPTS, Duration::from_millis(400)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
        assert!(err.to_string().contains("timed out"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "returned at the deadline, not when the child finished: {:?}",
            started.elapsed()
        );
        // The whole group is gone, not just the direct child.
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: signal 0 only probes for existence.
        while unsafe { libc::kill(grandchild, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_ne!(
            unsafe { libc::kill(grandchild, 0) },
            0,
            "grandchild {grandchild} survived the timeout"
        );

        // A program that does not exist is a spawn error, not a hang.
        let missing = SpawnSpec::new("definitely-not-a-program-oba");
        let err = run_bounded(missing, OPTS, Duration::from_secs(5)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "{err}");
    }

    /// Called from a blocking thread of a live runtime (how the daemon calls
    /// it), the bounded spawn uses that runtime.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_bounded_works_from_spawn_blocking() {
        let out = tokio::task::spawn_blocking(|| {
            let mut spec = SpawnSpec::new("sh");
            spec.args(["-c", "echo hi"]);
            run_bounded(spec, OPTS, Duration::from_secs(20))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi");
    }
}
