// Browser address bar follows the focused pane (remote web sessions), and the
// desktop path stays exactly as it was. The workspace router is faked: what
// matters here is which `router.navigate` calls the sync makes.

import { cleanup, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LeafNode, PaneNode } from './types';

type NavOpts = { to: string; replace?: boolean };

const h = vi.hoisted(() => {
	const resolved: Array<() => void> = [];
	const router = {
		state: { location: { pathname: '/', searchStr: '' } },
		subscribe: (_event: string, fn: () => void) => {
			resolved.push(fn);
			return () => {
				const i = resolved.indexOf(fn);
				if (i >= 0) resolved.splice(i, 1);
			};
		},
		navigate: (_opts: NavOpts): Promise<void> => Promise.resolve(),
	};
	return { router, resolved, web: true };
});

vi.mock('@tanstack/react-router', async (orig) => ({
	...(await orig<typeof import('@tanstack/react-router')>()),
	useRouter: () => h.router,
}));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
}));

import { makeLeaf, tabUid } from './pane-reducer';
import { usePaneStore } from './pane-store';
import { syncPaneRouterLocation, useRouterPaneSync } from './router-pane-sync';

const navigate = vi.fn((opts: NavOpts) => {
	const [pathname, q] = opts.to.split('?');
	h.router.state.location = { pathname, searchStr: q ? `?${q}` : '' };
	return Promise.resolve();
});

function setBrowser(path: string): void {
	const [pathname, q] = path.split('?');
	h.router.state.location = { pathname, searchStr: q ? `?${q}` : '' };
}

let ngwa: LeafNode;
let settings: LeafNode;
let term: LeafNode;

/** Three panes: Ngwa route (focused), Settings route, a terminal. */
function seed(): void {
	ngwa = makeLeaf({ kind: 'route', path: '/ngwa/installed' });
	settings = makeLeaf({ kind: 'route', path: '/settings' });
	term = makeLeaf({ kind: 'terminal', sessionId: 's1' });
	const root: PaneNode = {
		type: 'split',
		direction: 'horizontal',
		children: [ngwa, settings, term],
		sizes: [34, 33, 33],
	};
	usePaneStore.getState().hydrate({ root, focusedId: ngwa.id, closedHistory: [] });
}

function mount(): void {
	setBrowser('/ngwa/installed');
	renderHook(() => useRouterPaneSync());
	navigate.mockClear();
}

function allTargets(): string[] {
	return navigate.mock.calls.map(([o]) => o.to);
}

beforeEach(() => {
	h.web = true;
	h.resolved.length = 0;
	h.router.navigate = navigate;
	navigate.mockClear();
	seed();
});
afterEach(() => cleanup());

describe('remote web session', () => {
	it('mirrors a focus change between route panes with replace (no history entry)', () => {
		mount();
		usePaneStore.getState().focusPane(settings.id);
		expect(navigate).toHaveBeenCalledWith({ to: '/settings', replace: true });
	});

	it('mirrors an in-pane link navigation (memory-router write-back) with replace', () => {
		mount();
		const uid = tabUid(ngwa.tabs[0]);
		syncPaneRouterLocation(ngwa.id, uid, '/ngwa/store');
		const leaf = usePaneStore.getState().focusedView();
		expect(leaf).toEqual({ kind: 'route', path: '/ngwa/store' });
		expect(navigate).toHaveBeenCalledWith({ to: '/ngwa/store', replace: true });
	});

	it('keeps pushing for pane-store navigations (sidebar / palette / typed address)', () => {
		mount();
		usePaneStore.getState().navigateFocused('/ngwa/health');
		expect(navigate).toHaveBeenCalledWith({ to: '/ngwa/health' });
	});

	it('shows `/` while a non-route pane is focused, then the route again', () => {
		mount();
		usePaneStore.getState().focusPane(term.id);
		expect(navigate).toHaveBeenLastCalledWith({ to: '/', replace: true });
		// Never a file path / session id in the address bar.
		expect(allTargets()).toEqual(['/']);
		usePaneStore.getState().focusPane(ngwa.id);
		expect(navigate).toHaveBeenLastCalledWith({ to: '/ngwa/installed', replace: true });
	});

	it('never writes a token into the address bar', () => {
		mount();
		syncPaneRouterLocation(ngwa.id, tabUid(ngwa.tabs[0]), '/ngwa/store?token=s3cret&surface=x');
		// Nor into the pane store (which is persisted).
		expect(usePaneStore.getState().focusedView()).toEqual({
			kind: 'route',
			path: '/ngwa/store?surface=x',
		});
		usePaneStore.getState().navigateFocused('/sessions?token=s3cret');
		expect(allTargets()).toEqual(['/ngwa/store?surface=x', '/sessions']);
		expect(allTargets().some((t) => t.includes('token'))).toBe(false);
	});

	it('does not carry a token from the address bar into the pane (Direction A)', () => {
		mount();
		setBrowser('/ngwa/health?token=s3cret');
		for (const fn of [...h.resolved]) fn();
		expect(usePaneStore.getState().focusedView()).toEqual({ kind: 'route', path: '/ngwa/health' });
	});

	it('cold start with a non-route focused pane parks the address bar at `/`', () => {
		usePaneStore.getState().focusPane(term.id);
		setBrowser('/ngwa/installed');
		renderHook(() => useRouterPaneSync());
		expect(navigate).toHaveBeenCalledWith({ to: '/', replace: true });
	});
});

describe('desktop (Tauri) — unchanged', () => {
	beforeEach(() => {
		h.web = false;
	});

	it('focus change still navigates without replace', () => {
		mount();
		usePaneStore.getState().focusPane(settings.id);
		expect(navigate).toHaveBeenCalledWith({ to: '/settings' });
	});

	it('a non-route focused pane writes nothing', () => {
		mount();
		usePaneStore.getState().focusPane(term.id);
		expect(navigate).not.toHaveBeenCalled();
	});
});
