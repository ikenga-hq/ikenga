// Thin detached-window bootstrap (plans/multi-window WP-05).
//
// The performance lever. Mounts only the surface-host root + the shared
// QueryClient — none of the router tree, route modules, or workspace chrome
// that `boot/primary.tsx` pulls in. Code-split behind a dynamic import in
// `main.tsx`, so a detached window never parses the heavy primary bundle.
//
// One QueryClient per window by construction: this imports the same
// module-scope `queryClient` the primary uses, and each OS window is its own
// JS context, so there is exactly one instance per window and no second cache
// mirroring the first.

import React from 'react';
import { createRoot } from 'react-dom/client';
import { QueryClientProvider } from '@tanstack/react-query';

import { queryClient } from '@/lib/query-client';
import { installIkengaDomSync, useIkengaStore } from '@/lib/ikenga/theme-store';
import { windowContext } from '@/lib/window/window-context';
import { DetachedRoot } from '@/shell/detached/detached-root';

import { ErrorBoundary } from '@/components/ui/error-boundary';

/** Boot a thin detached single-surface window. */
export function bootDetached(): void {
	const ctx = windowContext();

	// Mirror appearance data-attrs onto <html> before first paint. The theme
	// store is window-namespaced now (its localStorage cache starts at
	// defaults in a fresh detached window), so pull the authoritative, shared,
	// Rust-owned appearance to match the primary. Best-effort: a detached
	// window's minimal capability set may not yet grant the settings command
	// (WP-03/WP-06 widen per surface), in which case it keeps the default
	// theme — the call catches its own rejection.
	installIkengaDomSync();
	void useIkengaStore.getState().hydrateAppearanceFromRust();

	createRoot(document.getElementById('root')!).render(
		<React.StrictMode>
			<ErrorBoundary>
				<QueryClientProvider client={queryClient}>
					<DetachedRoot ctx={ctx} />
				</QueryClientProvider>
			</ErrorBoundary>
		</React.StrictMode>
	);
}
