---
"ikenga-desktop": patch
---

Remote daemon: serve Ngwa (`ngwa_snapshot`, `pkg_health_scan`, `pkg_trust_list`). What the daemon cannot evaluate — trust, pkg runtime, usage, install records — reads "Not available on this server" instead of empty or healthy. The Store no longer blames the registry for a snapshot failure, and the Create tab's hardcoded "live" label is gone.
