---
"ikenga-desktop": patch
---

Fix secrets and env-vault writes on macOS: the symlink guard no longer refuses root-owned OS links such as `/var` and `/tmp`, so the env vault under `$TMPDIR` publishes again instead of latching the deny state. User-owned links are still refused.
