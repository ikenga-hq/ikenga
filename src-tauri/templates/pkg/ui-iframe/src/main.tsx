import { StrictMode, useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';

// AppBridge handshake — replace the inline type with `@ikenga/contract` once the
// pkg is added to ikenga-pkgs. Standalone dev (no parent shell) just skips it.
type HostContext = {
  theme?: { mode: 'light' | 'dark' };
  styles?: { variables?: Record<string, string> };
  supabase?: { url?: string; anonKey?: string } | null;
};

function readHostContext(ctx: HostContext | undefined) {
  if (!ctx) return;
  if (ctx.theme?.mode) document.documentElement.dataset.theme = ctx.theme.mode;
  // setSupabaseConfig({ url, anonKey }) — see pkgs/README.md for the lazy pattern.
}

function App() {
  const [ready, setReady] = useState(false);

  useEffect(() => {
    if (window.parent === window) {
      // Standalone dev mode — no shell, no handshake.
      setReady(true);
      return;
    }
    // Wait for the parent shell's hostContext message.
    const handler = (event: MessageEvent) => {
      if (event.data?.type === 'hostContext') {
        readHostContext(event.data.payload);
        setReady(true);
      }
    };
    window.addEventListener('message', handler);
    window.parent.postMessage({ type: 'connect' }, '*');
    return () => window.removeEventListener('message', handler);
  }, []);

  return (
    <main style={{ fontFamily: 'system-ui', padding: '2rem' }}>
      <h1>{{name}}</h1>
      <p>{ready ? 'Host connected.' : 'Connecting…'}</p>
    </main>
  );
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
