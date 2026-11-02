---
"ikenga-desktop": minor
---

**Check now** for server updates. Settings › About › Server updates gets a button that asks the box to look for a release at once, instead of waiting for the daily check.

- New admin-only route `POST /api/server/update/check`, with the same access rules as `/apply`. It writes `update-check-request.json` next to the apply request, at most once a minute. The view adds `check_pending`, `check_stalled` and `can_check`.
- `provision.sh` adds the `check-request` action and two units, `ikenga-update-check-request.path` and `.service`. The service removes the request when it stops, and skips the check if one ran in the last minute.
- **Operators:** upgrades don't refresh `provision.sh` or its units. Run `sudo scripts/server/provision.sh install-update-units` once from a checkout of this release to enable the button. Until then, the panel says the check was never picked up.
