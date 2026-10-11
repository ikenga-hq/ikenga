---
"ikenga-desktop": patch
---

Ngwa on a server: what the headless daemon does not run (package runtime, trust, engine placement) now reads as one muted "On this server: … are desktop-only" line with a Details list, instead of a red error banner per capability. The daemon marks those sources with a typed `unavailable` field, so a real failure keeps its red alert. The daemon also now serves session usage: it scans only the signed-in account's own `~/.claude/projects` (read-only, no symlinks followed, capped by files, bytes and time), so the Sessions column fills in instead of showing a notice.
