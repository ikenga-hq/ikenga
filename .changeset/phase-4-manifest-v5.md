---
"ikenga-desktop": minor
---

Phase 4 — manifest v5 contributions and workflows (soft-warn release). The kernel accepts `ui.views[]`, `ui.explorer_sections[]`, `ui.companion_panels[]`, `ui.context_actions[]`, `ui.widgets[]` and top-level `workflows[]` (`@ikenga/contract@0.20.0`); `ui.nav` still parses with one warning per package and is removed in the next release (DEC-37). Adds `pin_on_install`, the `GET /iyke/ngwa/snapshot` and `GET /iyke/explorer/sections` bridge routes (`BRIDGE_API` 3), workflow importers (groundwork, Claude Code workflows, hooks, cron) with a non-executing parser, the Ngwa flow tab and Explorer Automations listing.
