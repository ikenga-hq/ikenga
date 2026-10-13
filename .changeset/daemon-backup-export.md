---
"ikenga-desktop": patch
---

The Ikenga daemon can now back up and restore an account's data from the browser. You can export your database as a `.ikbak` file and download it, and import one to put your data back. An import is checked against the current database layout before anything is replaced, so a broken or foreign backup is refused and your existing data is left alone. If the restore fails part-way, the daemon puts your previous database back. This works on a single-user daemon and, through the broker, on multi-user servers, where each account only ever restores into its own data.
