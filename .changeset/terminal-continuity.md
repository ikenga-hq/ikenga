---
'ikenga-desktop': patch
---

Make Ikenga-launched Claude terminals continuous across refreshes and restarts,
and fix hook attribution so each terminal tab sees only its own events.

- **Per-terminal hook settings.** The `claude --settings` file is now written at
  PTY-spawn time (`claude-hooks-<terminalId>.json`) rather than once at bridge
  start, and every hook/statusline URL carries `?terminal=<id>`. Rust adds
  `ikenga_terminal_id` to each event, so the tool-call feed, permission inbox,
  git ledger, and cost HUD can distinguish terminal A from terminal B even when
  both share the same cwd.
- **Refresh reattach.** `rehydrateFromDb` now calls `ptyTerminalList()` before
  respawning and restores the `ptyId` of any matching live PTY. `SingleTerminal`
  attaches via the existing atomic `Pty.attach` handshake instead of spawning a
  duplicate.
- **Restart resume.** `SessionStart` hook `session_id` is captured and persisted
  on the terminal tab. When the tab is later respawned (app restart or manual
  restart), `claude` is launched with `--resume <session_id>` so the previous
  conversation returns.
- **Statusline per terminal.** The `statusline://snapshot` event and
  `GET /iyke/statusline/snapshot` endpoint now carry a per-terminal map;
  `CostHud` filters to the terminal it belongs to.
- **Daemon-backed terminals (#102): deferred.** This PR fixes continuity without
  moving PTYs into the server. Daemon-backed PTYs remain the right long-term
  architecture for multi-window / remote sessions and will be scoped as a
  separate work package against #102.
- **Package npm dependency materialization (#150).** Registry, local, dev, and
  iyke installs now run `npm install --omit=dev` for packages that declare a
  `lifecycle: "long-lived"` MCP server, preventing `ERR_MODULE_NOT_FOUND` on
  first boot. Falls back to `bun install --production` if npm fails.
