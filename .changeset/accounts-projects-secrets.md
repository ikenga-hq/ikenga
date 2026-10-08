---
"ikenga-desktop": minor
---

Multi-user servers (T1):

- **Per-account secrets reach everything an account runs.** The server loads each account's `/etc/ikenga/secrets/<account>.env` into that account's sessions, so its terminals, Chi runs, engine CLIs and package sidecars see its secrets — and only its own. The file must be root-owned and group-owned by the account; dangerous names (`LD_*`, `PATH`, `IKENGA_*`, …) are refused.
- **`provision.sh sync-accounts`:** shared project downloads (one root-owned, read-only mirror per project; each account gets its own clone on its own branch, sharing objects on disk) and one root-only, scoped secrets file (`everyone` / `agents` / a named account) that hands each account only what it may have.
