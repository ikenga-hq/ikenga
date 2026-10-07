---
"ikenga-desktop": patch
---

Engine detection and Chi runs no longer mistake an infrastructure failure for a verdict. An auth probe that couldn't run (timeout, spawn failure, WSL down, no network) now reports sign-in as unknown instead of "not signed in", so the engine stays in the Chi target picker. A failed run's error names the cause found in the engine's stderr — e.g. "network unreachable from the engine (EAI_AGAIN)" or "WSL failed to start" — without exposing the raw stderr. WSL CLI detection now tolerates login-shell banners printed before `which` output.
