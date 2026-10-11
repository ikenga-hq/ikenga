//! Transcript usage for the headless daemon: a bounded, read-only, in-memory
//! scan of ONE account's `~/.claude/projects`.
//!
//! The desktop mirrors the corpus into its own `ikenga.db`
//! (`transcript::usage::scan_and_mirror_transcripts`), so a count outlives
//! Claude Code's 30-day eviction of the source files. The daemon cannot do
//! that: its database is shared by every account on the box, and a mirror in
//! it would hold one account's transcript facts where another could read them.
//! So the daemon scans on demand, keeps nothing on disk, and folds the scans
//! through the same extractor (`transcript::usage::scan_file_reach`) and the
//! same ownership rule (`snapshot_from_scans` = `load_usage_snapshot` without
//! SQLite). The cost is history: only what is on disk now is counted.
//!
//! ## Which corpus
//!
//! Exactly `<home>/.claude/projects` for the `home` the caller passes, which
//! is the router principal's home (`AppState::home`): under T1 the principal
//! child's `HOME`, under T0 the daemon user's. No request argument reaches
//! this module, so no input can name another account's home. The root is
//! canonicalised once and everything is read relative to that canonical root.
//!
//! ## Safety
//!
//! - **No symlink is followed below the root.** The walk uses `lstat`-style
//!   `DirEntry::file_type`, so a symlinked directory is not entered and a
//!   symlinked file is not a candidate; each candidate is then opened with
//!   `O_NOFOLLOW` and must be a regular file ([`Reach::NoFollow`]), which also
//!   covers a link swapped in after the walk. A subagent's `.meta.json` is
//!   read the same way.
//! - **Bounded**: [`Caps`] limits the entries walked, the files read, the
//!   bytes of one file, the bytes of the whole scan and the wall time. Newest
//!   files go first, so a cap trims the oldest history. A scan that stopped
//!   early says so in `sources.usage.error` (the count is a floor, not the
//!   truth); it never reads as complete.
//! - **Read-only**: nothing is written, anywhere.
//! - Results are cached per canonical root for [`CACHE_TTL`]: the Ngwa
//!   snapshot is re-requested on every refresh and a cold walk is not cheap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::server::shared::ngwa::UsageInput;
use crate::transcript::usage::{
    now_ms, scan_file_reach, snapshot_from_scans, FileScan, Reach, UsageSnapshot,
};

/// The reason shown when the daemon has no home to scan.
pub const NO_HOME: &str = "cannot resolve the home directory";

/// How long a scan result is reused.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// Cache entries kept before the oldest are dropped.
const CACHE_MAX_ENTRIES: usize = 32;

/// The limits of one scan.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Directory entries visited while looking for `.jsonl` files.
    pub max_entries: usize,
    /// `.jsonl` files read.
    pub max_files: usize,
    /// A single file bigger than this is skipped, not read.
    pub max_file_bytes: u64,
    /// Bytes read across the whole scan (sum of the sizes of files read).
    pub max_total_bytes: u64,
    /// Directory depth below the root that is entered.
    pub max_depth: usize,
    /// Wall time for walk plus reads.
    pub budget: Duration,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            max_entries: 200_000,
            max_files: 5_000,
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_depth: 8,
            budget: Duration::from_secs(12),
        }
    }
}

/// One scan's result.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub snapshot: UsageSnapshot,
    /// Why the count may be a floor (a cap was hit, or a file was unreadable);
    /// `None` = the whole corpus was read.
    pub note: Option<String>,
    /// `.jsonl` files found.
    pub files_found: usize,
    /// `.jsonl` files read.
    pub files_read: usize,
}

struct Candidate {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
}

#[derive(Default)]
struct Walk {
    files: Vec<Candidate>,
    entries: usize,
    /// Why the walk stopped early, if it did.
    stopped: Option<&'static str>,
}

/// Find the `.jsonl` regular files under `root` without following a symlink.
fn walk(root: &Path, caps: &Caps, deadline: Instant) -> Walk {
    let mut out = Walk::default();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            if out.entries >= caps.max_entries {
                out.stopped = Some("directory entry cap");
                return out;
            }
            if out.entries % 256 == 0 && Instant::now() >= deadline {
                out.stopped = Some("time budget");
                return out;
            }
            out.entries += 1;
            // `DirEntry::file_type` does not traverse a symlink: a link is
            // neither a dir nor a file here, and is skipped.
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if depth < caps.max_depth {
                    stack.push((path, depth + 1));
                }
            } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                let Ok(meta) = entry.metadata() else { continue };
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                out.files.push(Candidate {
                    path,
                    size: meta.len(),
                    mtime_ms,
                });
            }
        }
    }
    out
}

fn mib(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// Scan `<home>/.claude/projects` under `caps` and fold it into a snapshot.
/// Blocking. An absent corpus is a measured zero (this account has recorded
/// no transcripts), not an error: with no mirror there is no history that the
/// absence could be hiding. `Err` = the root exists but cannot be read.
pub fn scan_home(home: &Path, now_ms: i64, caps: &Caps) -> Result<Outcome, String> {
    let root = home.join(".claude").join("projects");
    let root = match root.canonicalize() {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Outcome {
                snapshot: snapshot_from_scans(now_ms, &[]),
                note: None,
                files_found: 0,
                files_read: 0,
            });
        }
        Err(e) => return Err(format!("transcript corpus unreadable: {e}")),
    };
    if !root.is_dir() {
        return Err(format!(
            "transcript corpus is not a directory: {}",
            root.display()
        ));
    }
    scan_root(&root, now_ms, caps)
}

/// [`scan_home`]'s body over an already-canonical `root`.
fn scan_root(root: &Path, now_ms: i64, caps: &Caps) -> Result<Outcome, String> {
    let deadline = Instant::now() + caps.budget;
    let mut found = walk(root, caps, deadline);
    let files_found = found.files.len();
    // Newest first, so a cap trims the oldest history (the 7/30-day windows
    // are what the UI shows). The path breaks ties for a stable result.
    found.files.sort_by(|a, b| {
        b.mtime_ms
            .cmp(&a.mtime_ms)
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut stopped = found.stopped;
    let mut scans: Vec<FileScan> = Vec::new();
    let mut total = 0u64;
    let (mut too_big, mut read_errors) = (0usize, 0usize);
    let mut first_error: Option<String> = None;
    for c in &found.files {
        if scans.len() >= caps.max_files {
            stopped.get_or_insert("file cap");
            break;
        }
        if Instant::now() >= deadline {
            stopped.get_or_insert("time budget");
            break;
        }
        if c.size > caps.max_file_bytes {
            too_big += 1;
            continue;
        }
        if total.saturating_add(c.size) > caps.max_total_bytes {
            stopped.get_or_insert("byte budget");
            break;
        }
        total += c.size;
        if let Some(scan) = scan_file_reach(&c.path, None, Reach::NoFollow) {
            if let Some(e) = &scan.read_error {
                read_errors += 1;
                first_error.get_or_insert_with(|| e.clone());
            }
            scans.push(scan);
        }
    }

    let mut notes: Vec<String> = Vec::new();
    if let Some(why) = stopped {
        notes.push(format!(
            "partially scanned ({why}): read {} of {files_found} transcript file(s), newest first",
            scans.len()
        ));
    }
    if too_big > 0 {
        notes.push(format!(
            "{too_big} transcript file(s) over {} MiB skipped",
            mib(caps.max_file_bytes)
        ));
    }
    if read_errors > 0 {
        notes.push(format!(
            "{read_errors} transcript file(s) could not be fully read; first: {}",
            first_error.unwrap_or_default()
        ));
    }
    Ok(Outcome {
        snapshot: snapshot_from_scans(now_ms, &scans),
        note: (!notes.is_empty()).then(|| notes.join("; ")),
        files_found,
        files_read: scans.len(),
    })
}

type Cache = Mutex<HashMap<PathBuf, (Instant, UsageSnapshot, Option<String>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The Ngwa snapshot's `usage` input for the account whose home is `home`.
/// `None` = no home could be resolved: that is a failure to report, not an
/// empty corpus. Runs the scan on the blocking pool; reuses a result younger
/// than [`CACHE_TTL`] for the same canonical root.
pub async fn usage_for_home(home: Option<PathBuf>) -> UsageInput {
    let Some(home) = home else {
        return UsageInput::unavailable(NO_HOME);
    };
    let scanned = tokio::task::spawn_blocking(move || {
        // Key the cache by the root the scan would read, so two homes never
        // share an entry; an absent corpus is cheap and not cached.
        let key = home.join(".claude").join("projects").canonicalize().ok();
        if let Some(key) = &key {
            if let Ok(map) = cache().lock() {
                if let Some((at, snap, note)) = map.get(key) {
                    if at.elapsed() < CACHE_TTL {
                        return Ok((snap.clone(), note.clone()));
                    }
                }
            }
        }
        let now = now_ms();
        let outcome = scan_home(&home, now, &Caps::default())?;
        if let Some(key) = key {
            if let Ok(mut map) = cache().lock() {
                if map.len() >= CACHE_MAX_ENTRIES {
                    map.clear();
                }
                map.insert(
                    key,
                    (
                        Instant::now(),
                        outcome.snapshot.clone(),
                        outcome.note.clone(),
                    ),
                );
            }
        }
        Ok::<_, String>((outcome.snapshot, outcome.note))
    })
    .await;
    match scanned {
        Ok(Ok((snapshot, note))) => UsageInput {
            snapshot,
            error: note,
        },
        Ok(Err(e)) => {
            tracing::warn!("[ngwa_snapshot] transcript scan failed: {e}");
            UsageInput::unavailable(e)
        }
        Err(e) => UsageInput::unavailable(format!("transcript scan task failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::usage::UsageKind;

    const NOW: i64 = 1_791_000_000_000; // 2026-10-03, so Sept-20 fixtures are in the 30d window

    fn assistant(session: &str, msg: &str, ts: &str, skill: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"{session}","uuid":"u-{msg}","timestamp":"{ts}","attributionSkill":"{skill}","message":{{"id":"{msg}","usage":{{"input_tokens":10,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"content":[]}}}}"#
        )
    }

    fn write_session(dir: &Path, name: &str, session: &str, skill: &str, n: usize) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let body: String = (0..n)
            .map(|i| {
                assistant(
                    session,
                    &format!("{session}-m{i}"),
                    &format!("2026-09-2{}T10:00:0{}.000Z", i % 9, i % 9),
                    skill,
                ) + "\n"
            })
            .collect();
        let p = dir.join(format!("{name}.jsonl"));
        std::fs::write(&p, body).unwrap();
        p
    }

    fn corpus() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().canonicalize().unwrap().join("home");
        let proj = home.join(".claude/projects/-p");
        write_session(&proj, "s1", "s1", "skill-x", 2);
        write_session(&proj, "s2", "s2", "skill-x", 1);
        write_session(&proj, "s3", "s3", "skill-y", 1);
        (tmp, home)
    }

    fn sessions(o: &Outcome, name: &str) -> i64 {
        o.snapshot
            .for_primitive(UsageKind::Skill, name)
            .map(|a| a.count_30d)
            .unwrap_or(-1)
    }

    #[test]
    fn counts_distinct_sessions_per_skill() {
        let (_t, home) = corpus();
        let o = scan_home(&home, NOW, &Caps::default()).unwrap();
        assert!(o.note.is_none(), "{:?}", o.note);
        assert_eq!((o.files_found, o.files_read), (3, 3));
        assert_eq!(sessions(&o, "skill-x"), 2);
        assert_eq!(sessions(&o, "skill-y"), 1);
        assert_eq!(sessions(&o, "never-used"), 0, "measured zero, not unknown");
        assert!(o.snapshot.is_available());
    }

    #[test]
    fn an_absent_corpus_is_a_measured_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let o = scan_home(tmp.path(), NOW, &Caps::default()).unwrap();
        assert!(o.snapshot.is_available() && o.note.is_none());
        assert_eq!(o.snapshot.total_sessions(), 0);
    }

    #[test]
    fn a_corpus_that_is_a_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        std::fs::write(tmp.path().join(".claude/projects"), "x").unwrap();
        assert!(scan_home(tmp.path(), NOW, &Caps::default()).is_err());
    }

    /// A symlinked file and a symlinked directory pointing outside the root
    /// are never read: their sessions do not appear, and nothing is flagged.
    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_corpus_are_not_followed() {
        let (t, home) = corpus();
        let outside = t.path().join("outside");
        let secret = write_session(&outside, "secret", "secret", "skill-secret", 3);
        let proj = home.join(".claude/projects/-p");
        std::os::unix::fs::symlink(&secret, proj.join("link.jsonl")).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".claude/projects/linked-dir")).unwrap();
        // A symlink to a file inside the root is skipped too: the rule is "no
        // links", not "no escapes", so it cannot be raced into an escape.
        std::os::unix::fs::symlink(proj.join("s1.jsonl"), proj.join("alias.jsonl")).unwrap();

        let o = scan_home(&home, NOW, &Caps::default()).unwrap();
        assert_eq!(o.files_found, 3, "only the three real files");
        assert_eq!(sessions(&o, "skill-secret"), 0);
        assert_eq!(sessions(&o, "skill-x"), 2, "alias did not double-count");
        assert!(o.note.is_none(), "{:?}", o.note);
    }

    /// A link swapped in AFTER the walk (the race `Reach::NoFollow` closes)
    /// is refused at open: the scan reads nothing from it.
    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_link_after_the_walk_is_refused() {
        let (t, home) = corpus();
        let outside = t.path().join("outside");
        let secret = write_session(&outside, "secret", "secret", "skill-secret", 3);
        let proj = home.join(".claude/projects/-p");
        let target = proj.join("s3.jsonl");
        std::fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&secret, &target).unwrap();
        assert!(scan_file_reach(&target, None, Reach::NoFollow).is_none());
        assert!(
            scan_file_reach(&target, None, Reach::Follow).is_some(),
            "the guard is the mode"
        );
    }

    /// A symlinked `.meta.json` next to a subagent transcript is not read, so
    /// an outside file cannot choose the agent type.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_subagent_meta_is_not_read() {
        let (t, home) = corpus();
        let sub = home.join(".claude/projects/-p/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        let line = |agent: &str| {
            format!(
                r#"{{"type":"assistant","sessionId":"s1","agentId":"{agent}","timestamp":"2026-09-20T10:00:00.000Z","message":{{"id":"m-{agent}","usage":{{"input_tokens":1,"output_tokens":1}},"content":[]}}}}"#
            ) + "\n"
        };
        std::fs::write(sub.join("agent-a1.jsonl"), line("a1")).unwrap();
        let meta = t.path().join("outside-meta.json");
        std::fs::write(&meta, r#"{"agentType":"stolen-type"}"#).unwrap();
        std::os::unix::fs::symlink(&meta, sub.join("agent-a1.meta.json")).unwrap();
        let o = scan_home(&home, NOW, &Caps::default()).unwrap();
        assert!(
            o.snapshot
                .for_primitive(UsageKind::Agent, "stolen-type")
                .unwrap()
                .count_30d
                == 0
        );
    }

    #[test]
    fn the_file_cap_stops_the_scan_and_says_so() {
        let (_t, home) = corpus();
        let caps = Caps {
            max_files: 2,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert_eq!((o.files_found, o.files_read), (3, 2));
        let note = o.note.unwrap();
        assert!(note.contains("partially scanned (file cap)"), "{note}");
        assert!(note.contains("read 2 of 3"), "{note}");
    }

    #[test]
    fn the_byte_budget_stops_the_scan_and_says_so() {
        let (_t, home) = corpus();
        let one = std::fs::metadata(home.join(".claude/projects/-p/s2.jsonl"))
            .unwrap()
            .len();
        // Room for roughly one small file, not three.
        let caps = Caps {
            max_total_bytes: one + 1,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert!(o.files_read < 3, "{}", o.files_read);
        assert!(o.note.unwrap().contains("(byte budget)"));
    }

    #[test]
    fn an_oversized_file_is_skipped_not_read() {
        let (_t, home) = corpus();
        let big = home.join(".claude/projects/-p/s1.jsonl");
        let small_limit = std::fs::metadata(&big).unwrap().len() - 1;
        let caps = Caps {
            max_file_bytes: small_limit,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert_eq!(o.files_read, 2, "s1 (the two-message file) was skipped");
        assert_eq!(sessions(&o, "skill-x"), 1);
        assert!(
            o.note.unwrap().contains("over 0 MiB skipped"),
            "caps print in MiB"
        );
    }

    #[test]
    fn a_zero_time_budget_reads_nothing_and_says_so() {
        let (_t, home) = corpus();
        let caps = Caps {
            budget: Duration::ZERO,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert_eq!(o.files_read, 0);
        assert!(o.note.unwrap().contains("time budget"));
    }

    #[test]
    fn the_entry_cap_stops_the_walk() {
        let (_t, home) = corpus();
        let caps = Caps {
            max_entries: 1,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert!(o.note.unwrap().contains("directory entry cap"));
    }

    #[test]
    fn depth_is_bounded() {
        let (_t, home) = corpus();
        let deep = home.join(".claude/projects/a/b/c/d");
        write_session(&deep, "deep", "deep", "skill-deep", 1);
        let caps = Caps {
            max_depth: 2,
            ..Caps::default()
        };
        let o = scan_home(&home, NOW, &caps).unwrap();
        assert_eq!(sessions(&o, "skill-deep"), 0);
        let o = scan_home(&home, NOW, &Caps::default()).unwrap();
        assert_eq!(sessions(&o, "skill-deep"), 1);
    }

    #[tokio::test]
    async fn no_home_is_a_failure_not_an_empty_corpus() {
        let u = usage_for_home(None).await;
        assert!(!u.snapshot.is_available());
        assert_eq!(u.error.as_deref(), Some(NO_HOME));
    }

    /// Two homes never share a cache entry or a count.
    #[tokio::test]
    async fn each_home_sees_only_its_own_corpus() {
        let (_a, ha) = corpus();
        let tmp = tempfile::tempdir().unwrap();
        let hb = tmp.path().canonicalize().unwrap().join("home");
        write_session(&hb.join(".claude/projects/-q"), "sb", "sb", "skill-b", 1);
        let (ua, ub) = (
            usage_for_home(Some(ha.clone())).await,
            usage_for_home(Some(hb)).await,
        );
        assert_eq!(
            (ua.snapshot.total_sessions(), ub.snapshot.total_sessions()),
            (3, 1)
        );
        // A second call for the same home is served from the cache, unchanged.
        let again = usage_for_home(Some(ha)).await;
        assert_eq!(again.snapshot.total_sessions(), 3);
    }
}
