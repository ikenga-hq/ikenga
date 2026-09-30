---
"ikenga-desktop": patch
---

Fix the Ngwa **Installed** tab being stuck on "Scanning equipment catalogue…" forever. Building the snapshot called `Kernel::status()` from async code. The settings registry's `snapshot()` reads `pkg_settings` with a blocking `block_on`, so as soon as any installed pkg declared a settings schema (e.g. Meetings 0.2.1), every snapshot panicked with "Cannot start a runtime from within a runtime". The panic affected both the `ngwa_snapshot` command and `GET /iyke/ngwa/snapshot`.

- Both callers now take the kernel status on the blocking pool.
- The settings registry reads values safely from any context: no runtime, a multi-thread runtime (`block_in_place`), or a current-thread runtime, where it degrades to schema-only instead of panicking.
