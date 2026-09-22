# {{name}} engine adapter

Ikenga engine adapter pkg. Registers `{{slug}}` as a chat engine.

## Required follow-up — Rust adapter

The manifest declares the contract; the runtime adapter that
implements the `Engine` trait lives in the **shell** at
`shell/src-tauri/src/pkg/engine_adapters/{{slug}}.rs`. Adding a new
engine is a paired change across this pkg and the shell.

See [`docs/pkg-patterns/04-engine.md`](../../docs/pkg-patterns/04-engine.md)
and [ADR-012](../../docs/adr/012-multi-engine-pkg-portability.md).

## Capability flags

The `engine.capabilities` block in `manifest.json` is a **superset**
of fields every adapter must answer. Set implemented flags to `true`
and the rest to `false`. Lying about a flag silently breaks features
for users — the runtime won't fail loudly.

## Develop

```bash
pnpm install
ikenga dev .             # mount the pkg into the running shell
# ...iterate on shell/src-tauri/src/pkg/engine_adapters/{{slug}}.rs
# (the shell still needs a full restart for adapter changes)
```
