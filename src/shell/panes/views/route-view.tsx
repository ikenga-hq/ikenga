import {
	type AnyRouter,
	createMemoryHistory,
	createRouter,
	RouterProvider,
} from '@tanstack/react-router';
import { useEffect, useMemo } from 'react';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { syncPaneRouterLocation } from '@/lib/panes/router-pane-sync';
import { queryClient } from '@/lib/query-client';
import { isRemoteWebSession } from '@/lib/transport';
import { routeTree } from '@/routeTree.gen';
import { PaneScopeProvider } from '../pane-scope';
import { tabUid } from '../view-key';

// Re-exported for existing importers (`__root.tsx`) — the context itself now
// lives in the React-only `pane-scope` module to break an import cycle with the
// pkg iframe host (which the route tree imports). See pane-scope.tsx.
export { usePaneScope } from '../pane-scope';

// Each pane gets its own TanStack Router instance with memory history,
// keyed by `(paneId, tabUid)` rather than paneId alone — a pane can hold
// several route tabs, and a per-paneId cache made them all share one
// memory router/history stack, so switching tabs (or even just rendering
// two route tabs in one pane) fought over a single location. Composite
// keying gives each tab its own router (and its loader cache, match
// state, etc.), surviving remounts when the surrounding PanelGroup
// rebuilds on structural changes (split/close) AND when the tab is simply
// switched away from and back (Stage-2 keep-alive still needed to avoid
// the remount itself; this just means the remount doesn't lose history).
const routerCache = new Map<string, AnyRouter>();

function routerCacheKey(paneId: string, tabId: string): string {
	return `${paneId}:${tabId}`;
}

// Evict cache entries whose (paneId, tabUid) pair no longer corresponds to
// a live route tab — covers both a dead pane (old behavior) AND a tab that
// was closed or moved out of a still-live pane (new: previously this only
// checked paneId, so closing one route tab in a multi-tab pane never freed
// its router). One global subscription is enough — pane changes are
// infrequent.
usePaneStore.subscribe((state) => {
	const liveKeys = new Set<string>();
	for (const paneId of getLeafIdsInOrder(state.root)) {
		const leaf = findLeaf(state.root, paneId);
		if (!leaf) continue;
		for (const view of leaf.tabs) {
			if (view.kind === 'route') liveKeys.add(routerCacheKey(paneId, tabUid(view)));
		}
	}
	for (const key of Array.from(routerCache.keys())) {
		if (!liveKeys.has(key)) routerCache.delete(key);
	}
});

// HMR: when `routeTree.gen.ts` hot-updates (a route file was added,
// renamed, or removed), routers still in `routerCache` were built against
// the previous tree. Reusing them surfaces as TanStack's
// "Invariant failed: Duplicate routes found with id: __root__" because the
// old + new trees both register a `__root__`. Clear the cache so the next
// `getOrCreateRouter` mints fresh routers against the current tree.
//
// Cold boots don't hit this — the module loads once and the cache is
// empty. This guard is dev-only (no-op in prod via the `import.meta.hot`
// check, which Vite tree-shakes out of the production bundle).
if (import.meta.hot) {
	import.meta.hot.accept('@/routeTree.gen', () => {
		routerCache.clear();
	});
}

function getOrCreateRouter(paneId: string, tabId: string, initialPath: string): AnyRouter {
	const cacheKey = routerCacheKey(paneId, tabId);
	let router = routerCache.get(cacheKey);
	if (!router) {
		router = createRouter({
			routeTree,
			defaultPreload: 'intent',
			context: { queryClient },
			history: createMemoryHistory({ initialEntries: [initialPath || '/'] }),
		});
		routerCache.set(cacheKey, router);
	}
	return router;
}

interface RouteViewProps {
	paneId: string;
	path: string;
}

export function RouteView({ paneId, path }: RouteViewProps) {
	// RouteView is only ever rendered as a pane's *active* tab (pane.tsx
	// renders only `activeTab`, keyed so a tab switch remounts this
	// component), so the active tab found here is exactly the view this
	// instance represents — safe to resolve its identity from the store
	// rather than threading a tabId prop through PaneBody's dispatch switch.
	// Falls back to `paneId` alone if the lookup ever comes up empty (e.g.
	// a transient state before the tree reflects this tab) so the router
	// cache degrades to the old per-pane behavior instead of crashing.
	const tabId = usePaneStore((s) => {
		const leaf = findLeaf(s.root, paneId);
		const active = leaf?.tabs[leaf.activeTabIdx];
		return active && active.kind === 'route' ? tabUid(active) : null;
	});
	const router = useMemo(() => getOrCreateRouter(paneId, tabId ?? paneId, path), [paneId, tabId]);

	// Sync external path changes (sidebar nav, command palette) into this
	// pane's router. We compare to current location to avoid a redundant
	// navigate on first mount.
	useEffect(() => {
		// Compare against the full location (pathname + search). `path` carries the
		// query (`?surface=…`, `?filter=…`); comparing pathname alone fires a
		// redundant navigate on every query-only change.
		const current = router.state.location.pathname + (router.state.location.searchStr ?? '');
		if (current !== path) {
			void router.navigate({ to: path });
		}
	}, [path, router]);

	// The other direction, remote web sessions only: this pane's own
	// navigations (an in-pane `<Link>` such as Ngwa's Installed/Store tabs)
	// move just this memory router, so write the resolved location back into
	// the tab. That is what the pane's address bar and, via router-pane-sync,
	// the browser address bar follow. The effect above then sees router and
	// `path` agree and does nothing, so the two never ping-pong. Desktop is
	// left as it was (its pane paths persist and drive the activity mode).
	useEffect(() => {
		if (!tabId || !isRemoteWebSession()) return;
		return router.subscribe('onResolved', () => {
			const l = router.state.location;
			syncPaneRouterLocation(paneId, tabId, l.pathname + (l.searchStr ?? ''));
		});
	}, [router, paneId, tabId]);

	// Bound the route content to the pane height. The main-window render gets
	// this via content-pane.tsx's `<main className="flex h-full …">`; pane
	// routers render the route tree straight into the pane body slot, so
	// without this wrapper a fill-the-pane route (e.g. a pkg iframe with
	// `height:100%`) has no definite-height ancestor and grows to its full
	// content height — the pane then scrolls as one slab instead of the route
	// scrolling internally. (Lives here, not just in __root, because the
	// per-pane router is cached by id and won't pick up __root edits on HMR.)
	// Fill the pane body slot. The slot (pane.tsx) is `relative flex-1 min-h-0`
	// — bounded, but a `height:100%` child doesn't resolve through the flex
	// chain on WebKitGTK, so a fill-the-pane route (pkg iframe) grew to its full
	// content height and the pane scrolled as one slab. `absolute inset-0` fills
	// the bounded relative ancestor directly, sidestepping percentage-height
	// resolution. (The main-window render gets bounded height via content-pane's
	// flex column; pane routers render straight into the slot.)
	return (
		<PaneScopeProvider value={paneId}>
			<div className="absolute inset-0 flex flex-col overflow-hidden">
				<RouterProvider router={router} />
			</div>
		</PaneScopeProvider>
	);
}
