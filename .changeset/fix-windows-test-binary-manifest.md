---
"ikenga-desktop": patch
---

`cargo test` works on Windows again. Test binaries were linked without the Common Controls v6 manifest that tauri-build only gives the app binary, so they loaded System32's comctl32 5.82 (no `TaskDialogIndirect`) and died with 0xc0000139 before running a single test. The manifest is now passed to the linker for every artifact on Windows MSVC targets; other targets are unchanged.
