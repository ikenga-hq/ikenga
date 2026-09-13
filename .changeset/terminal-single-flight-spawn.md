---
"ikenga-desktop": patch
---

Open each terminal tab's PTY exactly once. Restored and newly opened tabs used to spawn several PTYs (the rehydrate auto-resume and every re-run of the tab's mount effect each started one) and kill all but the last within milliseconds; PTY opens are now single-flight per tab, and a tab closed mid-spawn no longer leaves an orphan process.
