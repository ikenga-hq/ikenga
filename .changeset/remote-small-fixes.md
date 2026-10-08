---
"ikenga-desktop": patch
---

Remote small fixes:
- Ngwa Health trust card: shows "not available on this server" in the subtitle when trust sources are unserved on the daemon, instead of "unsigned".
- Ngwa Store: Install and Update buttons in browser remote sessions now honestly state "Not available on this server yet" (via `NOT_AVAILABLE_ON_SERVER_YET`) when actions are unavailable.
- Terminal hooks: `term_hooks_statusline_snapshot` on a daemon without `--data-dir` reports an honest reason why it is unavailable, which is displayed in CostHud.
- Artifact comments routing: migration 0071 widens the `artifact_comments.sink` CHECK constraint to allow `'clipboard'` and `'chi'`, preventing routing audit failures after delivery on desktop and daemon.
