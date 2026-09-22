# {{name}}

Ikenga thin iframe pkg. No build step, no toolchain. Modules load from
[esm.sh](https://esm.sh) at runtime.

| | |
|---|---|
| Pkg id | `{{id}}` |
| Kind | `embedded` (UI iframe) |
| Surface | `/pkg/{{id}}/` |
| Build | none — `dist/` is the whole app |

## Layout

```
{{slug}}/
├── manifest.json
├── package.json          # manifest-only
├── README.md
└── dist/                 # served as the iframe's document root
    ├── index.html        # bootstrap (do NOT replace with <script src=...>)
    ├── app.js            # entry module
    └── lib/
        └── bridge.js     # MCP Apps SDK wrapper
```

## Why the bootstrap?

`index.html` uses a `document.baseURI` bootstrap instead of
`<script src="./app.js">`. **Do not change this** unless you understand
the trade-off. Short version: the kernel inlines relative-src scripts
to dodge a WebKitGTK srcdoc bug, but the inlined module then has
`about:srcdoc` as its base URL — so relative imports inside it fail.
Dynamic-importing app.js via `document.baseURI` makes app.js load as
a fetched file with a real URL; *its* relative imports then work.

Full explanation:
[`docs/pkg-patterns/01-ui-iframe.md` §"Srcdoc trap"](../../01-ui-iframe.md).

## Develop

```bash
# Mount into a running shell with hot manifest reload.
ikenga dev .

# Or open directly in a browser to iterate on UI without the shell:
xdg-open dist/index.html   # Linux
open dist/index.html       # macOS
```

The pkg auto-detects standalone mode (`window.parent === window`) and
skips the host handshake — useful for quick layout iteration.

## Fork in one step

```bash
cp -r {{slug}} ~/my-{{slug}}
# edit ~/my-{{slug}}/manifest.json (change the id) + dist/app.js
ikenga add ~/my-{{slug}}
```

No `pnpm install`, no `tsc`. The agent in your shell can do this for
you — just ask.

## Upgrade paths

- **Need multiple screens?** Add `dist/features/<x>.js` modules and
  import them from `app.js`. Use hash routing (`location.hash`) for
  internal nav. See `ikenga-pkgs/packages/apps/suite/` for the
  reference layout.
- **Need React?** Add `import * as React from 'https://esm.sh/react@19'`
  + `import htm from 'https://esm.sh/htm@3'`. No build step. The suite
  pkg uses this pattern.
- **Need TypeScript / heavy state / shadcn?** Switch to the
  `ui-iframe` (Vite-based) template.

## Use Supabase

Set `capabilities.supabase.required = true` in `manifest.json`, list
the tables you'll touch under `permissions.supabase.tables`, then add
to your app code:

```js
import { createClient } from 'https://esm.sh/@supabase/supabase-js@2';

let supabase = null;
// Pass this as the connectBridge onContextChange callback, or read
// app.getHostContext().supabase after connect.
export function configureSupabase(ctx) {
  if (ctx?.supabase?.url && ctx?.supabase?.anonKey && !supabase) {
    supabase = createClient(ctx.supabase.url, ctx.supabase.anonKey, {
      auth: { persistSession: false, autoRefreshToken: false, detectSessionInUrl: false },
    });
  }
}
```

The shell reads `VITE_SUPABASE_URL` and `VITE_SUPABASE_ANON_KEY` from
the `Workspace` scope of the Stronghold vault. If they're missing,
the pkg refuses to mint when `required: true`. To set them via iyke:

```bash
TOKEN=$(jq -r .token ~/.local/share/app.ikenga/control.json)
PORT=$(jq -r .port ~/.local/share/app.ikenga/control.json)
curl -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"scope":"workspace","key":"VITE_SUPABASE_URL","value":"https://YOUR.supabase.co"}' \
  http://127.0.0.1:$PORT/iyke/secret/set
```

(Repeat for `VITE_SUPABASE_ANON_KEY`. Run sequentially — Stronghold
serializes writes.)

## SDK import — use the bundled subpath

`lib/bridge.js` imports from
`https://esm.sh/@modelcontextprotocol/ext-apps@1.7.1/app-with-deps`
(NOT the default entry). The default pulls `zod/v4` from esm.sh and
the resolver sometimes serves a build missing `.custom()`, which
throws `t.custom is not a function`. The bundled subpath inlines its
deps.

## Layout rules

`pkg_content` only serves files under `<install_path>/dist/`. **Keep
your JS files at `dist/` root or in subdirectories.** Avoid a deep
`src/` subdirectory — when the kernel inlines `<script src="./app.js">`
(legacy pattern, not used here), ES imports inside resolve against the
document base URL, not the original file path. The bootstrap pattern
above sidesteps this entirely, but mistakes are easier to make if the
layout invites them.

See [`docs/pkg-patterns/01-ui-iframe.md`](../../01-ui-iframe.md) for
the full layout + lifecycle reference.

## CSP

The manifest declares `ui.csp` overrides allowing `esm.sh` (for the
SDK + any other CDN modules) and `*.supabase.co` (REST + Realtime).
Override keys **replace** the directive, so `'self'` is listed
explicitly.

To use a different CDN or self-host these modules, edit
`manifest.json` and the import URLs in `dist/lib/bridge.js`.
