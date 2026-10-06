---
"ikenga-desktop": patch
---

Fix Windows "not a valid Win32 application" (os error 193) when starting pkg sidecars that declare a .js bin (e.g. Meetings recording): run them through the bundled Bun.
