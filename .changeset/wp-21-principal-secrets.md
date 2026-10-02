---
"ikenga-desktop": patch
---

Per-principal secret store for T1 (WP-21): each principal's own secrets, sealed under a key the broker derives from a root-held KEK, layered over the IKENGA_SECRET_* operator default.
