# {{name}}

Ikenga MCP server pkg. Exposes tools to the AI engine over stdio
JSON-RPC.

## Develop

```bash
pnpm install
ikenga dev .             # mount into the running shell
```

The supervisor watches `src/**/*.ts` (per `restart_when_changed`) and
respawns the server on save — your tools become callable from the
chat without a shell restart. Manifest edits trigger a full pkg
reload.

## Tools shipped

| Tool | Description |
|---|---|
| `ping` | Smoke-test tool — returns `pong`. |

Add more in `src/index.ts` using `server.tool(name, description, schema, handler)`.

## Build for publish

```bash
pnpm build
```

See [`docs/pkg-patterns/03-mcp-server.md`](../../docs/pkg-patterns/03-mcp-server.md).
