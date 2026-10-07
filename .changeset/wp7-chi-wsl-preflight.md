---
"ikenga-desktop": patch
---

Chi runs on an engine that lives inside WSL now check WSL's network before they start. If WSL has no network, can't resolve names, or isn't starting, the run fails at once with "WSL has no network — <cause>" instead of launching an engine that can't sign in, and the same WSL health notification the terminal raises appears with its fixes. The check reuses a result from the last 30 seconds, and engines installed on Windows itself are never checked. When Windows itself is offline, the run still starts, because the problem isn't WSL's.
