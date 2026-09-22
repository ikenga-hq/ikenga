// MCP Apps SDK bridge — canonical iframe⇄host protocol Ikenga uses.
//
// Lifecycle (from @modelcontextprotocol/ext-apps Quickstart + the shell's
// pkg-iframe-host.tsx implementation):
//   1. new App(...) — register handlers before connect
//   2. await app.connect() — runs ui/initialize handshake automatically
//   3. app.getHostContext() — read theme / styles / supabase / royaltiAuth
//   4. app.callServerTool({ name: 'host.<x>', arguments }) — invoke host
//      tools (the shell intercepts `host.*` names in dispatchHostCall before
//      forwarding to pkg MCP servers).
//
// The host re-emits hostContext on theme change via onhostcontextchanged.

// IMPORTANT: use the bundled `app-with-deps` build, NOT the default entry.
// The default pulls `zod/v4` as a peer-via-esm.sh and resolution sometimes
// produces a Zod build missing `.custom()` (throws `t.custom is not a
// function` at module init). The bundled variant inlines its deps so it
// works regardless of esm.sh's resolver state.
import {
  App,
  applyDocumentTheme,
  applyHostStyleVariables,
  applyHostFonts,
} from 'https://esm.sh/@modelcontextprotocol/ext-apps@1.7.1/app-with-deps';

let app = null;

export async function connectBridge({ name, version, onContextChange } = {}) {
  app = new App({ name, version }, {
    tools: { listChanged: false },
  });

  app.onerror = (err) => console.error('[{{slug}}] bridge error', err);
  app.onhostcontextchanged = (ctx) => {
    applyContext(ctx);
    onContextChange?.(ctx);
  };
  app.onteardown = async () => ({});

  await app.connect();
  const ctx = app.getHostContext();
  if (ctx) applyContext(ctx);
  return ctx;
}

function applyContext(ctx) {
  if (!ctx) return;
  if (ctx.theme) applyDocumentTheme(ctx.theme);
  if (ctx.styles?.variables) applyHostStyleVariables(ctx.styles.variables);
  if (ctx.styles?.css?.fonts) applyHostFonts(ctx.styles.css.fonts);
}

/** Navigate the focused shell pane (cross-pkg or in-pkg sub-route). */
export async function hostNavigate(path) {
  if (!app) throw new Error('bridge not connected');
  return app.callServerTool({ name: 'host.navigate', arguments: { path } });
}

/** Open an external link via the host. */
export async function openLink(url) {
  if (!app) throw new Error('bridge not connected');
  return app.openLink({ url });
}

/** Read the current hostContext snapshot. */
export function getContext() {
  return app?.getHostContext() ?? null;
}

/** Detect standalone-dev (no parent shell). */
export function isStandalone() {
  return typeof window !== 'undefined' && window.parent === window;
}
