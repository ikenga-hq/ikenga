// Browser address bar ↔ focused pane, for remote web sessions.
//
// The workspace router (browser history) already follows the pane store via
// `router-pane-sync.ts`. What it could not see is a pane's OWN navigation: a
// route pane renders on a memory router (route-view.tsx), so an in-pane
// `<Link>` (Ngwa's Installed/Store/Scopes tabs) moved that router and nothing
// else — the pane store, the pane's address bar and the browser URL all kept
// the old path. These helpers are the pure half of the fix; the wiring lives
// in route-view.tsx (write-back) and router-pane-sync.ts (URL write).
//
// Desktop windows are untouched: their URL is invisible, their pane paths
// are persisted, and detached windows carry `?…` window params in their URL,
// so every caller gates on `isRemoteWebSession()` / `isTauri()`.

import { navigateFocused } from './pane-reducer';
import { sanitizeRoutePath } from './route-path';
import type { PaneTreeSnapshot } from './pane-persistence';
import type { PaneView } from './types';

export { sanitizeRoutePath };

/** What the address bar shows while the focused pane is not a route
 *  (terminal, file, Studio, scratchpad). None of those kinds has a route of
 *  its own — writing a file path there would reload as an unknown route —
 *  and `/` already means "no particular route: restore the saved layout"
 *  (desktop boots there too). So a reload while a terminal is focused
 *  round-trips to exactly that layout instead of grafting the last route
 *  onto the terminal's pane. */
export const NON_ROUTE_URL = '/';

/** The address-bar path for the focused pane's active view. */
export function urlPathForView(view: PaneView | null | undefined): string {
	if (!view || view.kind !== 'route') return NON_ROUTE_URL;
	return sanitizeRoutePath(view.path);
}

/** `isTauri()` from `@/lib/transport`, restated for the one caller that runs
 *  at module load (pane-store's initial tree): importing the transport there
 *  would make every test that mocks `@/lib/transport` without `isTauri`
 *  throw on import. Same two globals, same answer. */
export function hasTauriGlobals(): boolean {
	if (typeof window === 'undefined') return false;
	return '__TAURI_INTERNALS__' in window || '__TAURI__' in window;
}

/**
 * The route the first pane opens on at boot, from the page's own URL.
 * Desktop keeps the bare pathname: a detached window's `?…` is window
 * params, not route state. A browser tab keeps the query too, so a deep link
 * such as `/ngwa/installed?surface=…` lands on its surface — sanitised,
 * because this runs at module load, before `getAuthToken` has stripped a
 * `?token=` link from the address bar.
 */
export function bootRoutePath(
	loc: Pick<Location, 'pathname' | 'search'>,
	desktop: boolean
): string {
	const pathname = loc.pathname || '/';
	if (desktop) return pathname;
	return sanitizeRoutePath(pathname + (loc.search ?? ''));
}

/**
 * Re-apply a browser deep link over a restored layout. The pane tree is
 * restored asynchronously, after the first pane already opened the URL's
 * route; a plain `hydrate` would replace that pane and snap the address bar
 * to the saved focus, so loading `/ngwa/health` would never show Health.
 *
 * The saved layout is kept; only its focused pane navigates (in place when
 * it shows a route, switching to a tab that already holds the path, else a
 * new route tab — `navigateFocused` semantics). `/` is not a deep link (see
 * {@link NON_ROUTE_URL}) and leaves the snapshot as saved.
 */
export function applyBootDeepLink(
	snapshot: PaneTreeSnapshot,
	deepLink: string | null
): PaneTreeSnapshot {
	if (!deepLink || deepLink === NON_ROUTE_URL) return snapshot;
	const path = sanitizeRoutePath(deepLink);
	if (path === NON_ROUTE_URL) return snapshot;
	return { ...snapshot, root: navigateFocused(snapshot.root, snapshot.focusedId, path) };
}
