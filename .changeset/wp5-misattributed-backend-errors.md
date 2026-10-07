---
"ikenga-desktop": patch
---

The onboarding preflight and the Claude store now name the real cause when a check fails. The secrets row tells a damaged secrets index, a locked or unreachable keychain, a missing keychain backend and a store disabled at startup apart, each with its own fix, instead of always saying "unlock the keychain". When the free-space check can't match the app-data folder to a volume, it says it couldn't tell (a warning) instead of reporting 0 GB free and failing. Installing from the Claude store with a missing working folder now says the folder is missing rather than claiming `git` or `npx` isn't installed.
