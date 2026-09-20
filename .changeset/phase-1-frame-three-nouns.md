---
"ikenga-desktop": minor
---

Phase 1 of the shell UX rearchitecture: the workspace is now organised around three nouns — Project, Chi and Ngwa.

- **Rail** is exactly Project · Chi · Ngwa · pins · Settings. The old per-pkg activity modes are gone; a v15 persisted store migrates to v16 (unknown modes snap to `project`, file roots fold into the active project, and the pre-migration blob is kept under a `__v15_backup` key). Rail pins are seeded once from the kernel activity-bar registry at upgrade.
- **Project** gets an Explorer sidebar scoped to the active project, with a section registry and eight built-in sections (files, artifacts, sessions, ngwa-project, automations, todos, scratchpads, views), single-action empty states and a project switcher. Location collapses onto the active project rather than being tracked separately.
- **Chi** gets a Companion panel: dispatch bar with target picker, session tabs, permission cards, cost HUD, tool feed and a collapsed resting strip. Dispatching with no terminal focused starts a headless `chi_run`.
- **Ngwa** gets a single `/ngwa/*` route family with an in-route facet bar (kind, system, scope) instead of rail entries. Legacy routes redirect.
- **Frame chrome**: a title row with exactly two controls, one banner slot, a status bar, a merged single-tab pane row with a tools reveal, and a Shortcuts view generated from a new read-only keymap registry.
- **Bridge**: `iyke state` reports `active_project` and `bridge_api`, and `GET /iyke/keys` exposes the keymap.

Verified against the locked D-01 design: 32 resting interactive controls at the default layout against a budget of 132, WCAG 2.5.7 / 2.5.8 / 1.4.11 / 1.4.13 checks green in the browser-mode e2e harness.
