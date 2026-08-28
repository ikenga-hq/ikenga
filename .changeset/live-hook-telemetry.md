---
'ikenga-desktop': patch
---

Make the terminal's live view of a Claude session actually receive events —
cost HUD, tool-call feed, permission inbox, git ledger (#149).

All four are mounted in `shell/panes/views/terminal-view.tsx` and none had ever
received a single event, for three independent reasons:

1. **Nothing was posting.** The hooks and `statusLine` were installed by the
   `CLAUDE_CONFIG_DIR` overlay, which hardcoded `port: 0` — so every hook fired
   `curl … http://127.0.0.1:0/iyke/hooks/event`. The overlay is gone; the wiring
   now rides `claude --settings <file>`, documented by the CLI as loading
   *additional* settings, layered over user/project/local rather than replacing
   them. The user's real `~/.claude` is discovered natively and never written.
2. **Nothing could read.** `tool-call-feed.tsx`, `cost-hud.tsx` and
   `permission-inbox.tsx` each hardcoded `http://127.0.0.1:4000`, a port the
   bridge has never bound — it takes a dynamic one. They now go through
   `iykeFetch`, which resolves the live endpoint and bearer token.
3. **One route did not exist.** `permission-inbox.tsx` POSTed to
   `/iyke/hooks/decision`, which was never registered, so the operator's
   approve/deny 404'd. Added as record-and-broadcast.

The settings document is written by `iyke::hook_settings` at bridge start —
the one moment the real port and token both exist — beside `control.json`, 0600
because it carries a bearer token, and removed with it on shutdown. A stale port
is therefore impossible by construction rather than merely unlikely, and every
curl in the document is authenticated. Only terminals Ikenga launches are wired;
a `claude` started by hand in a plain shell is untouched.

Two limits stated plainly. `/iyke/hooks/decision` is **record-only**: the
`PreToolUse` hook has already returned `{"continue": true}` by the time a human
sees the request, so it cannot retroactively gate the call — a real gate means
holding that response open, which is a separate design. And the IDE lock file
(`~/.claude/ide/<port>.lock`) is still unwired; `write_ide_lock_file` only ever
got `port: 0` and the literal token `"ikenga-token"`, so IDE discovery has never
worked either. Both tracked in #149.
