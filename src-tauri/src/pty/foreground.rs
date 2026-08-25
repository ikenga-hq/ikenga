//! Foreground-process detection for managed PTYs.
//!
//! Given a PTY's shell PID, return the foreground process — the one whose
//! pgrp is the controlling terminal's foreground process group (set by
//! `tcsetpgrp`). When the user runs `claude` in a terminal pane, the shell
//! sets `claude` as the foreground PG; that's what we surface to the
//! routing dispatcher so pin clicks can target the right PTY.
//!
//! Linux: parse `/proc/<shell_pid>/stat` (field 8 = `tpgid`), then
//! read `/proc/<tpgid>/comm` and `/proc/<tpgid>/cmdline`. No new deps.
//!
//! macOS: `sysctl(KERN_PROC_PID, shell_pid)` returns a `kinfo_proc`
//! whose `kp_eproc.e_tpgid` is the foreground process group leader of
//! the controlling terminal — the direct semantic equivalent of Linux's
//! `tpgid`. A second sysctl on that PID extracts the basename from
//! `kp_proc.p_comm`. We don't read argv on macOS (KERN_PROCARGS2 is
//! fiddly and the routing dispatcher only filters on `name`).
//!
//! Each lookup is cached for 1 second per PTY to keep the routing hot
//! path off the filesystem on repeated reads.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForegroundProcess {
    /// PID of the foreground process (the one with the controlling terminal).
    pub pid: i32,
    /// Executable basename (e.g. `"claude"`, `"bash"`, `"vim"`). Matches what
    /// the routing dispatcher uses to detect claude PTYs.
    pub name: String,
    /// Full argv, null-byte-stripped. Empty when the kernel returned no
    /// cmdline (kernel-thread, zombie). Best-effort.
    pub args: Vec<String>,
    /// Live working directory of the foreground process.
    pub cwd: Option<String>,
}

#[derive(Clone)]
struct CacheEntry {
    fetched_at: Instant,
    value: Option<ForegroundProcess>,
}

const CACHE_TTL: Duration = Duration::from_secs(1);

fn cache() -> &'static Mutex<HashMap<i32, CacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<i32, CacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up the foreground process for a PTY identified by its shell PID
/// (the result of `MasterPty::process_group_leader()`).
///
/// Returns `None` when:
/// - the platform isn't supported (macOS, Windows in v0)
/// - the PID no longer exists (PTY died, race with the wait reaper)
/// - the `tpgid` is invalid (no foreground job; rare but possible)
pub fn lookup(shell_pid: i32) -> Option<ForegroundProcess> {
    // Cache check
    if let Ok(map) = cache().lock() {
        if let Some(entry) = map.get(&shell_pid) {
            if entry.fetched_at.elapsed() < CACHE_TTL {
                return entry.value.clone();
            }
        }
    }

    let value = lookup_uncached(shell_pid);

    if let Ok(mut map) = cache().lock() {
        map.insert(
            shell_pid,
            CacheEntry {
                fetched_at: Instant::now(),
                value: value.clone(),
            },
        );
        // Prune entries older than 30s to keep the cache bounded.
        let now = Instant::now();
        map.retain(|_, entry| now.duration_since(entry.fetched_at) < Duration::from_secs(30));
    }

    value
}

#[cfg(target_os = "linux")]
fn lookup_uncached(shell_pid: i32) -> Option<ForegroundProcess> {
    let stat = std::fs::read_to_string(format!("/proc/{shell_pid}/stat")).ok()?;
    // Format: `pid (comm) state ppid pgrp session tty_nr tpgid ...`.
    // `comm` is parenthesized and can contain spaces, so locate the closing
    // paren and split the rest by whitespace.
    let close = stat.rfind(')')?;
    let tail = &stat[close + 1..];
    let mut fields = tail.split_whitespace();
    let _state = fields.next()?; // 3
    let _ppid = fields.next()?; // 4
    let _pgrp = fields.next()?; // 5
    let _sess = fields.next()?; // 6
    let _tty_nr = fields.next()?; // 7
    let tpgid: i32 = fields.next()?.parse().ok()?; // 8

    // tpgid == -1 means no controlling terminal foreground group, and
    // tpgid == shell_pid (or its pgrp) means the shell itself is foregrounded
    // — both legitimate, both valid as "what's in front."
    if tpgid <= 0 {
        return None;
    }

    let comm = std::fs::read_to_string(format!("/proc/{tpgid}/comm"))
        .ok()?
        .trim()
        .to_string();
    let cmdline_bytes = std::fs::read(format!("/proc/{tpgid}/cmdline")).ok()?;
    let args: Vec<String> = cmdline_bytes
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();

    let cwd = std::fs::read_link(format!("/proc/{tpgid}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    Some(ForegroundProcess {
        pid: tpgid,
        name: comm,
        args,
        cwd,
    })
}

#[cfg(not(target_os = "linux"))]
fn lookup_uncached(_shell_pid: i32) -> Option<ForegroundProcess> {
    // macOS + Windows: not implemented yet. Returning None means the routing
    // dispatcher falls back to side-pane Chat, which is the documented
    // behavior.
    //
    // The previous macOS implementation used `libc::kinfo_proc` + `sysctl`,
    // but the `kinfo_proc` type is missing from the libc crate's apple
    // module (only the FreeBSD/Net/OpenBSD branches define it). Re-enabling
    // macOS foreground detection requires either (a) manually declaring the
    // struct against `<sys/sysctl.h>`, or (b) pulling in the `libproc`
    // crate. Tracked for a follow-up release.
    None
}

/// True when the foreground process for `shell_pid` is `claude` (or
/// `claude-code`, or any binary whose basename starts with `claude`).
///
/// The routing dispatcher uses this to filter PTYs when picking the active
/// claude session. Match is on the executable basename so an alias like
/// `claude-code` or a wrapper script named `claude` all qualify.
pub fn is_claude(shell_pid: i32) -> bool {
    lookup(shell_pid)
        .map(|fp| fp.name.starts_with("claude"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_is_consistent() {
        // Self-PID — guaranteed to be a real, observable process. We don't
        // care about the result value, only that two consecutive calls
        // return the same thing (cache hit).
        let pid = std::process::id() as i32;
        let a = lookup(pid);
        let b = lookup(pid);
        assert_eq!(a, b);
    }

    #[test]
    fn lookup_for_nonexistent_pid_is_none() {
        // PID 2^31 - 1 is functionally impossible to be live.
        let result = lookup(i32::MAX - 1);
        assert!(result.is_none());
    }
}
