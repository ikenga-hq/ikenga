---
"ikenga-desktop": patch
---

Accepting a share invite no longer loops back to the spent invite link. The workspace saved `/remote/invite` as an open tab, so the reload after accepting restored it and sent the browser back to the used link. Invite and pair pages are no longer restored with the saved layout.
