---
"ikenga-desktop": patch
---

Packages bound to the Default project now count as personal, so they stay loaded whichever project is active. Before this, every package on an existing install was stamped with the Default project on each start, and switching to any other project parked all of them, built-in packages included. A migration clears the existing Default stamps, the start-up backfill no longer re-stamps packages, and installs or scope changes that target the Default project are stored as personal. With Default active, the Store's install button now reads "Install to personal", and Default no longer appears as a separate install target.
