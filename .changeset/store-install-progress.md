---
"ikenga-desktop": patch
---

Installing from the Store now shows what is happening: the Install button turns into a progress row that names each step (downloading, verifying, extracting, installing dependencies, registering, starting services) with a bar, and you can cancel before the app registers. Several installs, and Update all, each show their own progress. When an install fails you get a short explanation with a next step, such as running out of disk space or a network problem, plus Retry and a "Show details" view with the cleaned-up log and the npm log path. A failed install no longer leaves a half-installed folder behind. Apps that ask to be pinned on install now appear on the rail again.
