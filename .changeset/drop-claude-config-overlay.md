---
'ikenga-desktop': patch
---

Stop pointing terminals at a throwaway `CLAUDE_CONFIG_DIR` overlay, so `claude`
in an Ikenga terminal uses your real `~/.claude` (#149).

Every PTY the shell spawned got `CLAUDE_CONFIG_DIR` set to
`$XDG_RUNTIME_DIR/ikenga/claude-overlay`, with the user's real assets symlinked
in. Two of those symlinks silently never happened: the builder looked for
`~/.claude/credentials.json` and `~/.claude/.claude.json`, while the real files
are `~/.claude/.credentials.json` and `~/.claude.json` (in `$HOME`, not inside
`.claude/`). Both `if target.exists()` checks failed and skipped, so `claude`
started against a config dir with no credentials and no `projects` map — asked
you to log in and to trust the folder, then wrote a SECOND config there. On a
real machine that meant a 2 KB / 0-project config shadowing a 116 KB /
23-project one, and because `$XDG_RUNTIME_DIR` is wiped on reboot, a fresh login
after every restart.

The overlay's one legitimate purpose was seeding `settings.json` so the shell
could inject its statusline and hooks invisibly. That never worked either:
`ensure_claude_overlay_dir` hardcoded `port: 0` and no token, so it wrote
`curl … http://127.0.0.1:0/iyke/statusline/event` and the same for hooks, on
`PreToolUse` / `PostToolUse` / `SessionStart` / `SessionEnd` /
`UserPromptSubmit`. A failing curl on every tool call is worse than no
injection, so nothing of value is lost by removing it.

Terminals now use the user's real config natively — which is what the
2026-07-20 overlay retirement already did for the chat path and missed here.
`src/pty/overlay.rs` and the two `configure_overlay_*` writers are deleted;
`write_ide_lock_file` is kept but marked uncalled, because its only caller was
the overlay passing `port: 0` and a literal `"ikenga-token"`, meaning IDE
lock-file discovery has never actually worked. Re-wiring it must pass the live
iyke bridge port and bearer token.

A caller that sets `CLAUDE_CONFIG_DIR` explicitly (via `opts.env` or the app's
own environment) is still honoured.
