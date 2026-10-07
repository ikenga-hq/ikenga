---
"ikenga-desktop": patch
---

When WSL can't be asked, engine detection now says so instead of reporting the engine as missing. An engine whose WSL lookup failed shows "WSL unavailable — <reason>" in the onboarding engine step, Settings › Engines, Ngwa's Health › Engines panel and the Companion target picker, rather than "Not on PATH" or "not installed". Such an engine can't be picked in onboarding and no longer counts toward "no engine installed". Your default engine stays available in the Companion target picker, tagged "WSL unavailable", so a run on it fails with the WSL reason instead of the engine disappearing; other unchecked engines aren't offered. Detection results gain an optional `unavailable: { kind, reason }` field; it is omitted for every engine that was checked, so older consumers see the same shape as before.
