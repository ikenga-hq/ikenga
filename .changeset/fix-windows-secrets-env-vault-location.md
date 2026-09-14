---
"ikenga-desktop": patch
---

Stop writing vault env files to a world-readable folder on Windows. The runtime `env-vault` and the durable `env` copy used to fall back to `/tmp`, which resolves to `C:\tmp\ikenga-actions` where any local user can read them; they now live in `%LOCALAPPDATA%\ikenga-actions`, inside the user's own profile. Copies left in `C:\tmp\ikenga-actions` by older builds are deleted the first time the vault is dumped. macOS and Linux paths are unchanged.
