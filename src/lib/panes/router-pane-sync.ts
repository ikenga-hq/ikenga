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
import { modeForRoute } from '@/lib/shell/mode-routes';
import { useShellStore } from '@/lib/shell/shell-store';

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

export function useRouterPaneSync(): void {
	const router = useRouter();

	useEffect(() => {
		// Direction A: workspace router (browser-history) → focused pane.
		// Fires on popstate, deep links, manual router.navigate calls outside
		// the pane scope.
		const unsubA = router.subscribe('onResolved', () => {
			const browser = browserPath(router);
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
			const path = focusedRoute();
			if (path === null) return;
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
			if (path === lastSyncedPath) return;
			if (browserPath(router) === path) {
				lastSyncedPath = path;
				return;
			}
			lastSyncedPath = path;
			void router.navigate({ to: path });
		});

		// Cold-start overlay: align workspace router with focused pane (if
		// it's a route view). Tauri starts at '/' on launch, so this only
		// fires for the case where the persisted focused pane has a route
		// other than '/'.
		const path = focusedRoute();
		if (path && browserPath(router) !== path) {
			void router.navigate({ to: path, replace: true });
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
