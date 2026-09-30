---
"ikenga-desktop": patch
---

Uninstalling a registry or CLI-installed pkg no longer brings it back after a restart. The kernel now moves the pkg's folder under the app's `pkgs` dir to a hidden `.uninstalled-<id>-<time>` backup (kept 7 days, pruned at boot), so boot discovery no longer re-registers it as a local install. Builtin, dev, and out-of-tree local installs are never touched.
