---
"ikenga-desktop": patch
---

WP-21: per-principal secrets for T1 — each principal child keeps its own encrypted store under `<data>/secrets/` (a DEK wrapped by a key the broker derives from a root-only KEK in `operator/`, handed over as host-only env), layered over the `IKENGA_SECRET_*` operator default; the daemon now serves every scope of `secrets_*`, the vault lock-state arms, and the `app_lock_*` arms per data dir.
