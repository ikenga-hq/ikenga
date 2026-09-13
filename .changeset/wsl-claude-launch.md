---
"ikenga-desktop": patch
---

Fix launching Claude terminals on Windows when `claude` lives only in WSL: the `--settings` path is now translated to `/mnt/<drive>/…`, and `wsl.exe -e` stops the distro login shell from expanding the wrapper's `$__status` early.
