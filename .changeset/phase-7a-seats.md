---
"ikenga-desktop": minor
---

**Phase 7 Part A: Chi seats, and local app lock.** A **seat** is a named, per-project slot for an agent session: `seat:<project>/<name>`, for example `seat:royalti-co/lead`. You dispatch to the seat, not to whichever session happens to hold it. The seat keeps its name, its scratchpad and its address when that session ends.

- **The seat rail (D-09).** The Companion lists the active project's seats, followed by an "Unseated" group.
  - Selecting a seat targets dispatch to it and scopes the permission, cost and tool-feed panels to it.
  - Each row shows the seat's status (live, idle, running or vacant), its session, a scratchpad line and where it is mounted.
  - Figures an engine doesn't report read "—", with a "not reported by this engine yet" tooltip.
  - **New seat** makes an empty seat, or seats an open or past session. **Remove seat…** asks first and can be undone. **Clear** empties the seat and keeps its scratchpad.
- **Dispatch to a vacant seat** resumes its last session, then sends, and a toast says so. On an engine that can't resume after a restart (openrouter), the seat is marked "not resumable after restart"; it never silently starts fresh.
- **Moving a session into a seat** takes it out of any other seat, atomically.
- **Holds.** A seat held by another client shows "held by X since T". Taking it over is always an explicit step.
- **`/chi` is the seat board.** It is a table of every seat plus the unseated sessions, with a detail column and the rail's own menus. ⌘2 still opens the rail. ⌘2 pressed from the dispatch input opens the board (`chi.board`).
- **Pop out joins Window 2.** A seat's or a pane's Pop out puts it into the open secondary window as a tab. It opens a new window only when none exists. The seat stays addressable while it is out there. Closing Window 2 brings its panes back to the main window.
- **Actions.** A `chi` action can target a seat by name. The Editor has a seat field, and the runner's liveness, permission and trust guards still apply.
- **Settings → Profile, App lock and Devices (D-05, local).**
  - The app locks on idle or with Lock now (⌘⇧L), and unlocks with a PIN or passphrase.
  - The lock screen says what is still running underneath.
  - Devices shows the daemon's current exposure, read-only, and says plainly that it is not a security boundary.
- **Light mode.** The cost HUD, tool feed and permission inbox use theme colours instead of fixed dark greys.
- **`iyke`.** The bridge is at API level 5, with seat routes. The `iyke seat ls | create | resume | fill | clear | release` commands and `iyke terminal-send --seat` ship in iyke-cli v0.6.0.
