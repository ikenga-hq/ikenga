# {{name}}

Ikenga skill-only pkg. Ships Claude Code skills / commands / agents —
no UI, no process, no MCP server.

## What's bundled

- `skills/{{slug}}/SKILL.md` — the skill itself

Add `commands/<name>.md` for slash commands, `agents/<name>.md` for
agent personas. Each becomes available globally once the pkg is
installed.

## Develop

```bash
ikenga dev .             # install into the running shell + ~/.claude/{skills,commands,agents}/
```

Skill files are copied to `~/.claude/` at install time. After
installation, `/{{slug}}` (or the trigger phrase in your SKILL.md
`description`) fires the skill.

See [`docs/pkg-patterns/06-skill-only.md`](../../docs/pkg-patterns/06-skill-only.md).
