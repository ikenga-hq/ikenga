---
"ikenga-desktop": patch
---

Honest failure states: a check that couldn't run no longer shows up as a confident "no".

- Settings › Engines: if your saved custom shells can't be read, Ikenga says so and pauses adding/removing them, instead of showing none and then overwriting the saved list on the next add. The "Resume terminals on start" setting shows a read error instead of an unchecked box.
- Explorer: Todos, Automations and Sessions show a short error row with the reason when their data can't be loaded, instead of an empty state. A package whose manifest can't be read is listed as unreadable, and an unreadable saved terminal list pauses saving until you dismiss the notice, so it isn't overwritten.
- Chi target picker: when engine detection fails it says "Couldn't check installed engines" with a Retry, instead of "No engine installed". The signed-out copy no longer guesses the cause.
- Onboarding: the offline-engine install names the actual cause (network, disk space, permissions, integrity, signature) instead of always blaming the registry, and an engine whose sign-in couldn't be checked shows "sign-in unknown".
- Pkg iframes: when the shell can't read a pkg's manifest to check a capability, the call is still refused but reports `reason: "check-unavailable"` instead of "pkg lacks the capability". Denials are unchanged.
- Remote pairing: a temporarily unavailable sign-in check on the computer is its own outcome, not "This browser didn't keep the pairing".
