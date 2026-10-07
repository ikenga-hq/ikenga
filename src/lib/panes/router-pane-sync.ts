// Bidirectional sync between the workspace browser-history router (the
// address bar) and the focused pane's route. Mounted exactly once at
// the workspace level. Mirrors the use-iyke-shell-sync pattern (single
// subscription that re-fires when focus or tree mutates).
//
// Loop avoidance: each direction skips when the two sides already agree
// on the path. Idempotent state changes don't re-fire the other side
// because the comparison is on the path string, not object identity.

import { useEffect } from 'react';
import { useRouter, type AnyRouter } from '@tanstack/react-router';

import { usePaneStore } from './pane-store';
import { findLeaf } from './pane-reducer';
import { NON_ROUTE_URL, sanitizeRoutePath } from './url-sync';
import { modeForRoute } from '@/lib/shell/mode-routes';
import { useShellStore } from '@/lib/shell/shell-store';
import { isRemoteWebSession } from '@/lib/transport';

function focusedRoute(): string | null {
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	if (!leaf) return null;
	const view = leaf.tabs[leaf.activeTabIdx];
	if (!view || view.kind !== 'route') return null;
	return view.path;
}

// The workspace router's current path *including* its query string. Pane paths
// are stored as `pathname?search` (Ngwa threads `?surface=&scope=&kind=&sys=`,
// Pkgs threads `?filter=`), so the sync must mirror search too — comparing or
// propagating `pathname` alone silently strips those params and snaps deep-link
// surfaces back to their defaults. `searchStr` is '' or '?k=v…'.
function browserPath(router: AnyRouter): string {
	const l = router.state.location;
	return l.pathname + (l.searchStr ?? '');
}

// Set for the duration of a pane-router write-back (see
// `syncPaneRouterLocation`). Zustand notifies subscribers synchronously inside
// `set`, so Direction B reads it while it is still true.
let paneRouterWriteBack = false;

/**
 * Write a route pane's own memory-router location back into the pane store
 * (remote web sessions — the caller, route-view.tsx, gates). An in-pane
 * `<Link>` only moves that pane's memory router; this is what lets the pane
 * store, the pane's address bar and, through Direction B below, the browser
 * address bar follow it. Direction B mirrors a write-back with `replace`.
 * The path is sanitised before it is stored (a credential param never lands
 * in the persisted layout either); route-view's path→router effect then
 * moves the pane's router onto the cleaned path, so the two settle.
 */
export function syncPaneRouterLocation(paneId: string, uid: string, path: string): void {
	paneRouterWriteBack = true;
	try {
		usePaneStore.getState().syncRouteTabPath(paneId, uid, sanitizeRoutePath(path));
	} finally {
		paneRouterWriteBack = false;
	}
}

export function useRouterPaneSync(): void {
	const router = useRouter();

	useEffect(() => {
		// Remote web session: the workspace router's location IS the tab's
		// address bar, so it shows the focused pane — a non-route pane as
		// `NON_ROUTE_URL` — and never carries a credential param (url-sync.ts).
		// History: pane-store navigations (sidebar, palette, a typed pane
		// address) keep pushing an entry, which Direction A turns back into a
		// pane navigation on Back/Forward. A focus change and an in-pane link
		// only REPLACE: Direction A always navigates the *focused* pane, so a
		// pushed focus change would make Back load the other pane's route into
		// this one. Desktop windows keep the original behaviour exactly; their
		// URL is not visible.
		const web = isRemoteWebSession();

		// Direction A: workspace router (browser-history) → focused pane.
		// Fires on popstate, deep links, manual router.navigate calls outside
		// the pane scope.
		const unsubA = router.subscribe('onResolved', () => {
			const browser = web ? sanitizeRoutePath(browserPath(router)) : browserPath(router);
			const paneRoute = focusedRoute();
			if (paneRoute === null) return; // focused pane shows non-route
			if (paneRoute === browser) return;
			usePaneStore.getState().navigateFocused(browser);
		});

		// Direction B: focused pane → workspace router.
		// Re-fires when the tree mutates (any pane action) or focus changes.
		let lastSyncedPath: string | null = null;
		const unsubB = usePaneStore.subscribe((state, prev) => {
			if (state.root === prev.root && state.focusedId === prev.focusedId) {
				return;
			}
			const replace = web && (paneRouterWriteBack || state.focusedId !== prev.focusedId);
			const path = focusedRoute();
			if (path === null) {
				if (!web || lastSyncedPath === NON_ROUTE_URL) return;
				lastSyncedPath = NON_ROUTE_URL;
				if (browserPath(router) !== NON_ROUTE_URL) {
					void router.navigate({ to: NON_ROUTE_URL, replace: true });
				}
				return;
			}
			// Direction C — re-sync the activity mode to the focused route's
			// exclusive owner (v16: ngwa, settings, chi) so the rail
			// + sidebar follow a programmatic / deep-link / restored navigation
			// the same way they follow a rail-icon click. Shared routes
			// (sessions, artifacts, /, …) return null and leave the current
			// mode untouched. Mirrors the iyke `go` path (control-listener).
			const mode = modeForRoute(path);
			if (mode) {
				const cur = useShellStore.getState().activeMode;
				if (cur !== mode) useShellStore.getState().setActiveMode(mode);
			}
			const target = web ? sanitizeRoutePath(path) : path;
			if (target === lastSyncedPath) return;
			if (browserPath(router) === target) {
				lastSyncedPath = target;
				return;
			}
			lastSyncedPath = target;
			void router.navigate(replace ? { to: target, replace: true } : { to: target });
		});

		// Cold-start overlay: align workspace router with focused pane (if
		// it's a route view). Tauri starts at '/' on launch, so this only
		// fires for the case where the persisted focused pane has a route
		// other than '/'.
		const path = focusedRoute();
		const coldTarget = web ? (path === null ? NON_ROUTE_URL : sanitizeRoutePath(path)) : path;
		if (coldTarget && browserPath(router) !== coldTarget) {
			void router.navigate({ to: coldTarget, replace: true });
		}
		// Cold-start Direction C: a persisted focused pane on a mode-owned
		// route re-syncs the activity mode on launch (same rule as Direction B).
		const coldMode = path ? modeForRoute(path) : null;
		if (coldMode) {
			const cur = useShellStore.getState().activeMode;
			if (cur !== coldMode) useShellStore.getState().setActiveMode(coldMode);
		}

		return () => {
			unsubA();
			unsubB();
		};
	}, [router]);
}
