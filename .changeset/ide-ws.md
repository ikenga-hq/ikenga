---
'ikenga-desktop': patch
---

Make `claude --ide` actually work, and stop the IDE lock leaking the bridge token.

#155 wired `write_ide_lock_file` into `iyke::start`, but the lock was malformed,
world-readable, and pointed at a server that did not exist. Verified end to end
against a real `claude --ide` session this time, not by unit test.

- **The lock is now the shape Claude Code reads** — `pid`, `workspaceFolders`,
  `ideName`, `transport: "ws"`, `runningInWindows`, `authToken`, with the port
  as the file name. It previously wrote `{port, authToken, pid, lock_path}`,
  which the CLI cannot use.
- **Written 0600.** It carries the iyke bridge bearer token and went out at 0644
  via a plain `fs::write`. `control.json` and the per-terminal hook settings
  both use an explicit private write for exactly this reason.
- **A real MCP-over-WebSocket server** (`iyke::ide_ws`) now answers on the port
  the lock advertises, authenticating on `x-claude-code-ide-authorization`.
  Implements `initialize`, `tools/list` and `tools/call` for the tools the shell
  can honestly answer — `getWorkspaceFolders`, `openFile`, the selection pair
  and `getDiagnostics` — and returns an explicit error for the rest rather than
  a plausible-looking empty success.
- **The `mcp` subprotocol is selected.** Claude Code sends
  `Sec-WebSocket-Protocol: mcp` and hangs up ~30ms after the handshake without
  sending a frame if the server does not select it, so nothing above the
  handshake can detect the failure.
- **Stale locks are reaped at start.** `Drop` does not run on SIGKILL, and a
  stale lock points `claude` at a dead port. Reaping is narrow: only locks whose
  `ideName` is ours and whose pid is gone. `kill(pid, 0) == 0` alone was not
  enough — a live process we do not own fails with `EPERM`, which would have
  read as dead.
