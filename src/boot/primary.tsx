// Primary-window bootstrap (plans/multi-window WP-05).
//
// This is the full shell entry — verbatim what `main.tsx` did before the thin
// detached path landed, lifted into a function and code-split behind a dynamic
// import. The detached path (`boot/detached.tsx`) never imports this module, so
// a thin window doesn't pay the parse cost of the router tree, every route, or
// the workspace chrome.

import { QueryClientProvider } from '@tanstack/react-query';
import { ReactQueryDevtools } from '@tanstack/react-query-devtools';
import { createRouter, RouterProvider } from '@tanstack/react-router';
import React from 'react';
import { createRoot } from 'react-dom/client';
import { installIkengaDomSync, useIkengaStore } from '@/lib/ikenga/theme-store';
import { queryClient } from '@/lib/query-client';
import { initDefaultCwd } from '@/lib/shell/default-cwd';
import { seedPinsFromRail } from '@/lib/shell/seed-pins';
import { useShellStore } from '@/lib/shell/shell-store';
import { startActionsStore } from '@/lib/actions/store';
import { installKeyDispatcher, startOsShortcutSync } from '@/lib/keymap/dispatcher';
import { isTauri } from '@/lib/transport';
import { initDetachedSurfaceTracking } from '@/lib/window/detached-surfaces';
import { installNativeMenu } from '@/shell/native-menu';
import { SecretsUnlockSheetProvider } from '@/shell/secrets/unlock-sheet';
import { ErrorBoundary } from '@/components/ui/error-boundary';
import { FilepickerModal } from '@/components/ui/filepicker-modal';
import { ReauthOverlay } from '@/components/ui/reauth-overlay';
import { routeTree } from '../routeTree.gen';

import '@xterm/xterm/css/xterm.css';

const router = createRouter({
	routeTree,
	defaultPreload: 'intent',
	context: { queryClient },
});

declare module '@tanstack/react-router' {
	interface Register {
		router: typeof router;
	}
}

/** Boot the full primary workspace window. */
export async function bootPrimary(): Promise<void> {
	// Dev-only globals (e.g. `window.ikengaAcpSmoke` for the ACP migration
	// Phase 3 smoke test). Lazy-imported so production builds tree-shake the
	// helper entirely.
	if (import.meta.env?.DEV) {
		void import('@/lib/dev');
	}

	// Sync Ikenga data-attrs onto <html> before first React render so the very
	// first paint already has the right theme/mode/density/workspace applied.
	installIkengaDomSync();

	// Install native menu best-effort (Mac-only; silently no-ops elsewhere).
	void installNativeMenu();

	// WP-54 (DEC-56): the one key dispatcher — every frame key fires through
	// the registry — and the effective model it reads, booted once here at
	// the app root so `getKeymap()` is the effective keymap (personal /
	// project rules, package grants) before any surface mounts. The primary
	// window also owns the OS-wide shortcuts (G-ACTIONS §6): it pushes the
	// effective default + personal OS rules to `lib.rs` and re-pushes on
	// every change.
	installKeyDispatcher();
	void startActionsStore().catch((err) => {
		console.warn('[boot] actions model did not load; keys stay on the defaults', err);
	});
	if (isTauri()) startOsShortcutSync();

	// Resolve $HOME once so `defaultCwd()` (used by terminal/session
	// fallbacks) can return it synchronously. Fire-and-forget — failure leaves
	// the helper falling back to '~'.
	void initDefaultCwd();

	// Pull the durable settings_kv mirror (migration 0013). Same fire-and-forget
	// semantics — failures leave the localStorage-hydrated snapshot in place,
	// successes overwrite Zustand state with the Tauri-side authoritative copy.
	await useShellStore.getState().hydrateSettingsFromRust();
	void useIkengaStore.getState().hydrateAppearanceFromRust();

	// WP-22: one-shot reconciler that seeds activity_pins from the kernel's
	// activity-bar registry so pkg rail icons a user already has survive the
	// move to a pinned rail. Fire-and-forget; internally guarded by a KV flag
	// and never throws into boot.
	void seedPinsFromRail();

	// Pull the durable projects list + active project id (migration 0015,
	// Phase 0). The Rust side owns the truth; this just seeds the in-memory
	// Zustand mirror for the activity-bar indicator and command palette.
	void useShellStore.getState().refreshProjects();

	// Track which surfaces are popped out into detached windows so the primary
	// window renders a reclaim placeholder instead of a live duplicate
	// (plans/multi-window). Primary-window only; seeds + subscribes to the
	// window:// lifecycle bus.
	initDetachedSurfaceTracking();

	createRoot(document.getElementById('root')!).render(
		<React.StrictMode>
			<ErrorBoundary>
				<QueryClientProvider client={queryClient}>
					<SecretsUnlockSheetProvider>
						<RouterProvider router={router} />
					</SecretsUnlockSheetProvider>
					<FilepickerModal />
					<ReauthOverlay />
					{import.meta.env?.DEV && <ReactQueryDevtools buttonPosition="bottom-right" />}
				</QueryClientProvider>
			</ErrorBoundary>
		</React.StrictMode>
	);
}
