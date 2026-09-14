//! Shared test infrastructure for engine adapters.
//!
//! The adapter tests mutate the process-global `HOME` env var to isolate
//! writes to `~/.claude/`, `~/.gemini/`, and `~/.codex/`. Before this module
//! each adapter had its own `test_lock` static `Mutex`, so cargo's parallel
//! runner could interleave tests across modules — one module's `HomeGuard::
//! drop` could restore the real `$HOME` mid-write of another module's test,
//! leaking adapter writes onto the developer's actual home directory. (Seen
//! 2026-05-18: a leftover `ikenga.p.svc` entry from a Rust test landed in
//! the real `~/.gemini/settings.json`.)
//!
//! Fix: one workspace-wide `LOCK` shared across every adapter test module
//! that mutates `HOME`. All three adapters' `#[cfg(test)] mod tests` now
//! `use super::super::test_util::{HomeGuard, test_lock}` so they serialize
//! against each other (the `super::super` because the `use` lives inside
//! each adapter's `mod tests` — its `super` is the adapter, the adapter's
//! `super` is `engine_adapters`).

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Workspace-wide `HOME`-mutation lock. Every adapter test that constructs
/// a `HomeGuard` must hold this first. Poisoning is tolerated — a panicking
/// test inside the critical section shouldn't wedge every subsequent run.
pub(super) fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Scratch `HOME` guard. Saves the current value, points `HOME` at a fresh
/// tempdir for the duration of the test, then restores on drop. Must be
/// held while the `test_lock` guard is still in scope — otherwise a sibling
/// module's test could observe the temp HOME or the missing HOME between
/// `set_var` and `remove_var`.
pub(super) struct HomeGuard {
    previous: Option<std::ffi::OsString>,
    previous_userprofile: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}

impl HomeGuard {
    pub(super) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());
        // `home_dir()` prefers USERPROFILE on Windows, so it must also be
        // pointed at the tempdir there or adapter code under test would
        // resolve the real user profile instead of the isolated fixture.
        let previous_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("USERPROFILE", tmp.path());
        Self {
            previous,
            previous_userprofile,
            _tmp: tmp,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match self.previous_userprofile.take() {
            Some(h) => std::env::set_var("USERPROFILE", h),
            None => std::env::remove_var("USERPROFILE"),
        }
    }
}
