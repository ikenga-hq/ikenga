---
"ikenga-desktop": patch
---

Remote permission routing (WP-75, G-ACCESS §5): the per-principal "this device only" / "any paired device" preference capped by each device's grant, the sensitive-ask classifier and the §5.4 who-may-decide algorithm (Owner escalation in shares, secret-material asks always to the Owner, members never persist rules), one `permission_decide` core for hook and ACP asks (desktop in-process, daemon over the relay), the T0 desktop → daemon ask relay so a paired device can answer the desktop's asks, attribution on permission rows, the remote inbox's read-only states, and the Ngwa trust sheet's operator-policy split on T1.
