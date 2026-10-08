---
"ikenga-desktop": patch
---

Pkg capability refusals now follow `@ikenga/contract` 0.23.0: every confirmed denial on a `host.*` verb carries `reason: 'scope-denied'`, and a check the shell couldn't run carries `reason: 'check-unavailable'`, so pkgs can map either to its RPC code with `hostRefusalCode()` instead of matching message text. Message text is unchanged.
