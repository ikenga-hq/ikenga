# {{name}}

Ikenga UI webview pkg. Mounts a native child webview at `/pkg/{{id}}/`
loading `{{webview_source}}`.

Pick this archetype only when the target site blocks iframe embedding
via CSP `frame-ancestors` or `X-Frame-Options`. Otherwise prefer the
iframe archetype.

## Develop

```bash
ikenga dev .             # mount into the running shell
```

Manifest-only — no build. Edit `manifest.json` and the kernel
re-registers on save.

## Driving the webview

If you want to script the page, ship a paired MCP server pkg that uses
the kernel-only `pkg_webview_eval` surface. See
[`docs/pkg-patterns/02-ui-webview.md`](../../docs/pkg-patterns/02-ui-webview.md)
and [`docs/pkg-patterns/03-mcp-server.md`](../../docs/pkg-patterns/03-mcp-server.md).

## Gotchas

- Cookie partition data persists across uninstall. Pick a partition
  name that reflects the site (`"spotify"`, not `"default"`) so users
  understand what they're isolating.
- On Linux, the child surface is a separate top-level X11 window
  parented to the main window. Wayland positioning is silently
  ignored — launch the shell with `GDK_BACKEND=x11`.
