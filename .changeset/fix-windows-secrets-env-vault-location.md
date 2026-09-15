---
"ikenga-desktop": patch
---

Stop writing vault env files to a world-readable folder on Windows. The runtime `env-vault` and the durable `env` copy used to fall back to `/tmp`, which resolves to `C:\tmp\ikenga-actions` where any local user can read them; they now live in `%LOCALAPPDATA%\ikenga-actions`, inside the user's own profile. Copies left in `C:\tmp\ikenga-actions` by older builds are deleted once per run, before the vault is opened, so the cleanup happens even if the vault fails to open. The cleanup only removes regular files and skips symlinks and junctions, so another local user cannot redirect it into your profile. macOS and Linux paths are unchanged.
