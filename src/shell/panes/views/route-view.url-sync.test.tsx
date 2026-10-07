// An in-pane `<Link>` moves only the pane's memory router. In a remote web
// session RouteView writes that navigation back into the pane store (which
// the browser address bar follows via router-pane-sync); on the desktop it
// does not. Real TanStack router, two-route stand-in tree.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ web: true }));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
}));

vi.mock('@/routeTree.gen', async () => {
	const { createRootRoute, createRoute, Link, Outlet } = await import('@tanstack/react-router');
	const rootRoute = createRootRoute({ component: () => <Outlet /> });
	const installed = createRoute({
		getParentRoute: () => rootRoute,
		path: '/ngwa/installed',
		component: () => (
			<div>
				<span>installed page</span>
				<Link to="/ngwa/store">Store</Link>
			</div>
		),
	});
	const store = createRoute({
		getParentRoute: () => rootRoute,
		path: '/ngwa/store',
		component: () => <span>store page</span>,
	});
	return { routeTree: rootRoute.addChildren([installed, store]) };
});

import { makeLeaf, tabUid } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { LeafNode } from '@/lib/panes/types';
import { RouteView } from './route-view';

let leaf: LeafNode;

beforeEach(() => {
	// TanStack's scroll restoration calls it on every resolve; jsdom lacks it.
	window.scrollTo = vi.fn() as unknown as typeof window.scrollTo;
	leaf = makeLeaf({ kind: 'route', path: '/ngwa/installed' });
	usePaneStore.getState().hydrate({ root: leaf, focusedId: leaf.id, closedHistory: [] });
});
afterEach(() => cleanup());

async function clickStore(): Promise<void> {
	render(<RouteView paneId={leaf.id} path="/ngwa/installed" />);
	const link = await screen.findByText('Store');
	await act(async () => {
		fireEvent.click(link);
	});
	await screen.findByText('store page');
}

describe('RouteView in-pane navigation write-back', () => {
	it('remote web: an in-pane <Link> updates the tab path in place', async () => {
		h.web = true;
		const uid = tabUid(leaf.tabs[0]);
		await clickStore();
		await waitFor(() =>
			expect(usePaneStore.getState().focusedView()).toEqual({ kind: 'route', path: '/ngwa/store' })
		);
		expect(tabUid((usePaneStore.getState().root as LeafNode).tabs[0])).toBe(uid);
	});

	it('desktop: the pane store is left alone (no write-back)', async () => {
		h.web = false;
		await clickStore();
		expect(usePaneStore.getState().focusedView()).toEqual({
			kind: 'route',
			path: '/ngwa/installed',
		});
	});
});
