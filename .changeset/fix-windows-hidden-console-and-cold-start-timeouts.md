---
"ikenga-desktop": patch
---

Fix two Windows-only papercuts. Console-subsystem children (node, npm.cmd, wsl.exe, taskkill, …) spawned from the GUI process no longer flash a visible console window — every production spawn site now goes through a shared `NoConsoleWindow::no_console_window()` helper in `platform.rs` instead of each callsite re-deriving `CREATE_NO_WINDOW`. Separately, three timeouts tuned on macOS/Linux were too tight for a Windows cold start (Defender scanning a freshly-spawned binary on first exec can add 500ms-1.7s+): the MCP per-call handshake budget is now split from the `tools/call` budget (15s to spawn+initialize, 5s for the call itself), the long-lived sidecar `INIT_TIMEOUT` moved from 5s to 15s, and the agent `--version` probe timeout moved from 2s to 5s.
