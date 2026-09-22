# {{name}}

Ikenga sidecar pkg. Supervised long-running process exposed via
iyke RPC routes.

## Develop

```bash
pnpm install
bash scripts/build.sh    # compile bin/<target>/{{slug}} for every target triple
ikenga dev .             # mount into the running shell
```

The supervisor watches `bin/**/*` (per `restart_when_changed`) and
respawns the sidecar on rebuild. State transitions emit
`pkg://lifecycle` events.

## Endpoints

| Method | Path | Handler |
|---|---|---|
| `POST` | `/pkg/{{id}}/run` | `sidecar:pa-{{pkg_slug}}-main run` |

Add more in `manifest.json` under `iyke.routes`. Each handler is
`"sidecar:<name> <subcommand>"` — the subcommand is what the kernel
sends on stdin as the JSON-RPC method.

## Gotchas

- Sidecar name **must** start with `pa-{{pkg_slug}}-`. The kernel
  rejects mismatches at install time.
- `bin/{target}/...` — `{target}` is the host's Rust target triple,
  expanded at load. Missing binaries for the host's target → sidecar
  enters `Parked` state.
- 3 crashes in 60s trips the breaker. Document the failure mode in
  stderr or you'll lose users.

See [`docs/pkg-patterns/05-sidecar.md`](../../docs/pkg-patterns/05-sidecar.md).
