---
"ikenga-desktop": patch
---

Windows terminal fixes. Copy/paste goes through the Tauri clipboard plugin instead of `navigator.clipboard` (no more WebView2 clipboard prompt; silent paste failure when it was dismissed), plain Ctrl+C with a selection copies and Ctrl+V pastes, and OSC 52 writes (claude's own copy) reach the system clipboard. Links that wrap across rows are clickable again under ConPTY. The PTY daemon now gets 3s to come up instead of 500ms (every cold start on Windows missed it), spawns without a console window, and a daemon that misses the window is killed rather than orphaned on port 4000. The log file is written to `%LOCALAPPDATA%\Ikenga\logs` again — `log_dir()` bailed on the unset `HOME` before reaching the Windows branch.
