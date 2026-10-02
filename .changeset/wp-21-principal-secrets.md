---
"ikenga-desktop": patch
---

Per-principal secret store for T1 (WP-21): each principal's own secrets, sealed under a key the broker derives from a root-held KEK (`operator/secrets-kek`, back it up), layered over the IKENGA_SECRET_* operator default. T0 is unchanged, including its secrets lock-state answers, so Settings → Secrets stays read-only there. A lost KEK is never re-minted over existing stores: the launch fails with "restore operator/secrets-kek from backup".
