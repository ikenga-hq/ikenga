---
"ikenga-desktop": patch
---

Pin `@tauri-apps/api` to `~2.11.1` to match the `tauri` crate (2.11.x, locked in `Cargo.lock`). `@tauri-apps/api` 2.12.0 was published to npm on 2026-09-26. The release workflow installs without a frozen lockfile, so `^2.1.1` picked 2.12.0, Tauri's version check refused the mismatch, and the v0.15.0 build failed on every platform. v0.15.1 ships Phase 6.
