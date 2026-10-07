---
"ikenga-desktop": patch
---

Chi runs on an engine that lives inside WSL now check WSL's network before they start. If WSL has no network, can't resolve names, or answers with an error, the run fails at once with "Can't start the engine in WSL — <cause>" instead of launching an engine that can't sign in, and the same WSL health notification the terminal raises appears with its fixes (once per problem, not once per run). The check reuses a result from the last 30 seconds, waits at most 20 seconds, and engines installed on Windows itself are never checked. The run still starts when the check can't tell: when Windows itself is offline, when the check runs out of time, or when `wsl.exe` only timed out (it had just answered the engine lookup).

A persistent Chi run of an engine installed only inside WSL no longer goes to the detached runner, which launches engines from the Windows PATH and can't start it: it runs in-process instead (through WSL, with the same pre-run check) and its result carries the warning "<engine> is installed only inside WSL, which persistent runs don't support yet. This run will NOT survive quitting the app." A persistent run whose engine can't start at all now fails at once with the reason.
