# {{name}}

Ikenga UI iframe pkg. Renders at `/pkg/{{id}}/` inside the shell.

## Develop

```bash
pnpm install
pnpm dev                 # Vite on http://localhost:5173
ikenga dev .             # symlink + mount into the running shell
```

Iframe code changes flow through Vite HMR. Manifest changes trigger a
full pkg reload (no shell restart).

## Build for publish

```bash
pnpm build
# Update manifest.json's ui.routes[].source to "./dist/index.html"
# before bumping the version + opening a changeset.
```

See [`docs/pkg-patterns/01-ui-iframe.md`](../../docs/pkg-patterns/01-ui-iframe.md).
