---
'ikenga-desktop': patch
---

Close the follow-ups left open by the terminal-continuity work: a permission
gate that can actually gate, working `claude --ide` discovery, visible parked
packages, and an opt-in terminal resume.

- **A real `PreToolUse` gate (#154).** `/iyke/hooks/event` now holds the hook
  response open while a human decides in the permission inbox, opt-in per
  terminal via `permissions.hold_terminal_<id>` (default off). The three
  timeouts nest explicitly — server hold 30s < `curl --max-time` 35s < Claude
  Code hook timeout 40s — because a hold that outlives the hook's own curl is
  a gate that silently passes every tool call through. Deny is expressed as
  `hookSpecificOutput.permissionDecision`, not `continue: false`, so denying
  one tool call does not end the conversation.
- **`claude --ide` discovery (#155).** `write_ide_lock_file` is wired into
  `iyke::start` with the live bridge port and token and removed on shutdown, so
  a stale lock cannot point `claude` at a dead port. It was previously correct
  but unreachable, and its only historical caller passed `port: 0` and a
  placeholder token.
- **Parked packages are visible and recoverable (#157).** The activity-bar rail
  badges a parked pkg instead of showing an entry indistinguishable from a
  healthy one, and the pkg sidebar surfaces the park reason with a retry.
  `ActivityBarEntry` now carries the pkg name, so a multi-view pkg is
  identifiable by its own name rather than by its first `ui.nav` view.
- **Opt-in terminal resume (#133).** A default-off `terminal.resume_on_start`
  setting; when enabled, rehydration respawns previously running tabs including
  those in unfocused panes, preserving the Claude session id, rather than
  waiting for someone to look at the pane.
