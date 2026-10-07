// Boot deep link, end to end (remote web session): the page loads at a route
// carrying a `?token=` link credential, the pane store seeds its first pane
// from `window.location` at module load, the saved layout hydrates over it
// through `hydrateRestoredLayout` (the call useWorkspaceEffects makes), and
// the REAL workspace router — browser history, real `replaceState` — runs
// `useRouterPaneSync`. Only `isRemoteWebSession` and the route tree are
// stand-ins.

import { act, cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => {
	// Before any import: pane-store reads the URL when it is first loaded.
	window.history.replaceState(null, '', '/ngwa/health?token=s3cret&tab=x');
	return { web: true };
});

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
}));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	listen: vi.fn(async () => () => {}),
}));

import {
	createBrowserHistory,
	createRootRoute,
	createRoute,
	createRouter,
	Outlet,
	RouterProvider,
} from '@tanstack/react-router';
import { makeLeaf } from '@/lib/panes/pane-reducer';
import type { PaneTreeSnapshot } from '@/lib/panes/pane-persistence';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useRouterPaneSync } from '@/lib/panes/router-pane-sync';
import type { LeafNode, PaneNode } from '@/lib/panes/types';
import { hydrateRestoredLayout } from './workspace-effects';

/** Saved layout: a route pane on /settings (focused) beside a live terminal. */
function savedLayout(): { snap: PaneTreeSnapshot; routePane: LeafNode; termPane: LeafNode } {
	const routePane = makeLeaf({ kind: 'route', path: '/settings' });
	const termPane = makeLeaf({ kind: 'terminal', sessionId: 's1' });
	const root: PaneNode = {
		type: 'split',
		direction: 'horizontal',
		children: [routePane, termPane],
		sizes: [50, 50],
	};
	return { snap: { root, focusedId: routePane.id, closedHistory: [] }, routePane, termPane };
}

function WorkspaceRoot() {
	useRouterPaneSync();
	return <Outlet />;
}

function workspaceRouter() {
	const rootRoute = createRootRoute({ component: WorkspaceRoot });
	const page = (path: string) =>
		createRoute({
			getParentRoute: () => rootRoute,
			path,
			component: () => <span>page {path}</span>,
		});
	return createRouter({
		routeTree: rootRoute.addChildren([page('/'), page('/ngwa/health'), page('/settings')]),
		history: createBrowserHistory(),
	});
}

function addressBar(): string {
	return window.location.pathname + window.location.search;
}

afterEach(() => cleanup());

describe('boot deep link (remote web session)', () => {
	// Order matters: the first test reads the module-load seed.
	it('seeds the first pane from the page URL, token stripped', () => {
		expect(usePaneStore.getState().focusedView()).toEqual({
			kind: 'route',
			path: '/ngwa/health?tab=x',
		});
	});

	it('keeps the deep link over the restored layout, focus on the saved pane', async () => {
		const { snap, routePane, termPane } = savedLayout();
		hydrateRestoredLayout(snap, true);

		const s = usePaneStore.getState();
		expect(s.focusedId).toBe(routePane.id);
		expect(s.focusedView()).toEqual({ kind: 'route', path: '/ngwa/health?tab=x' });

		// The real workspace router mounts on the page URL (still carrying the
		// token here — transport's strip is not part of this test) and the
		// sync settles the address bar on the focused pane, credential-free.
		window.scrollTo = vi.fn() as unknown as typeof window.scrollTo;
		const router = workspaceRouter();
		await act(async () => {
			render(<RouterProvider router={router} />);
		});
		await screen.findByText('page /ngwa/health');
		expect(addressBar()).toBe('/ngwa/health?tab=x');

		// Focus changes REPLACE: no history entries, non-route pane shows `/`.
		const depth = window.history.length;
		await act(async () => {
			usePaneStore.getState().focusPane(termPane.id);
		});
		expect(addressBar()).toBe('/');
		await act(async () => {
			usePaneStore.getState().focusPane(routePane.id);
		});
		expect(addressBar()).toBe('/ngwa/health?tab=x');
		expect(window.history.length).toBe(depth);
		expect(addressBar()).not.toContain('token');
	});

	it('desktop hydrates the saved layout as saved', () => {
		usePaneStore.getState().navigateFocused('/ngwa/health');
		const { snap, routePane } = savedLayout();
		hydrateRestoredLayout(snap, false);
		expect(usePaneStore.getState().focusedId).toBe(routePane.id);
		expect(usePaneStore.getState().focusedView()).toEqual({ kind: 'route', path: '/settings' });
	});
});
