---
"ikenga-desktop": patch
---

Pin the `@tauri-apps/plugin-*` npm packages to the minor versions of their Rust crates. The v0.16.0 release build failed on all three platforms with Tauri's version-mismatch check, because the release installs without a frozen lockfile and the `^` ranges resolved to newly published 2.4/2.5/2.8 plugins against 2.3/2.4/2.7 crates.
