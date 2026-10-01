---
"ikenga-desktop": patch
---

ikenga-server: T1 local accounts groundwork (WP-20 slice 1) — UUIDv7 principal ids and the full `Principal`, the operator root and `operator/accounts.db` (set-keyed migrations, `auth_events`), argon2id passwords with a login verifier and backoff, the provisioning core (uid allocator, useradd or built-in /etc writer, `create_in` guard) and `ikenga-server accounts create|passwd|disable|enable|revoke-sessions|list`.
