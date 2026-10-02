---
"ikenga-desktop": patch
---

Part B follow-ups (WP-78a): the T0 daemon's permission routing shares the server's one `ikenga.db` writer; refused hook decisions are shown, never presented as answered, and the no-row hook fallback fails closed without a routing runtime; a desktop restarted after a crash reports its orphaned asks so the daemon retracts an unapplied `permission.decided`; the T1 broker audits `routing_refused` decisions; the notification popover shows why a routed-away ask has no Allow / Deny; `--max-accounts` caps every account-creation path (CLI and bootstrap included); `secrets_default_names` tells your own bare keys from operator-default overrides; `share.artifact_viewed` is written for Guest and artifact-scope reads; WebSocket closes 4401 / 4403 route to sign-in and "access changed"; and secrets-declaring pkgs' MCP servers feed the sensitive-ask classifier.
