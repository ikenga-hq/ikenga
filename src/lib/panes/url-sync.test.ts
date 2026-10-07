// Pure halves of the browser-URL ↔ focused-pane sync (url-sync.ts) and the
// write-back reducer it relies on (`setRouteTabPath` / `syncRouteTabPath`).

import { beforeEach, describe, expect, it } from 'vitest';
import { findLeaf, makeLeaf, setRouteTabPath, tabUid } from './pane-reducer';
import { usePaneStore } from './pane-store';
import type { LeafNode, PaneNode } from './types';
import {
	NON_ROUTE_URL,
	applyBootDeepLink,
	bootRoutePath,
	sanitizeRoutePath,
	urlPathForView,
} from './url-sync';

describe('sanitizeRoutePath', () => {
	it('strips ?token= and keeps every other param', () => {
		expect(sanitizeRoutePath('/ngwa/store?token=s3cret&surface=x')).toBe('/ngwa/store?surface=x');
		expect(sanitizeRoutePath('/?token=s3cret')).toBe('/');
	});

	it('drops the hash (pair codes and invite tokens ride there)', () => {
		expect(sanitizeRoutePath('/remote/pair#c=ABCD')).toBe('/remote/pair');
		expect(sanitizeRoutePath('/a?x=1#frag')).toBe('/a?x=1');
	});

	it('leaves a path with no sensitive param byte-for-byte', () => {
		// No re-serialising: `%20` must not become `+`, order must not change.
		expect(sanitizeRoutePath('/ngwa/installed?b=2&a=hello%20world')).toBe(
			'/ngwa/installed?b=2&a=hello%20world'
		);
		expect(sanitizeRoutePath('/ngwa/health')).toBe('/ngwa/health');
	});
});

describe('urlPathForView', () => {
	it('is the route path for a route view, sanitised', () => {
		expect(urlPathForView({ kind: 'route', path: '/settings?token=t&tab=a' })).toBe(
			'/settings?tab=a'
		);
	});

	it('is NON_ROUTE_URL for terminal, file, Studio and scratchpad views', () => {
		expect(urlPathForView({ kind: 'terminal', sessionId: 's1' })).toBe(NON_ROUTE_URL);
		expect(urlPathForView({ kind: 'artifact', path: '/home/u/notes.md' })).toBe(NON_ROUTE_URL);
		expect(
			urlPathForView({ kind: 'artifact-studio', path: '/home/u/a.html', density: 'loupe' })
		).toBe(NON_ROUTE_URL);
		expect(urlPathForView({ kind: 'scratchpad', scope: 'p', name: 'n' })).toBe(NON_ROUTE_URL);
		expect(urlPathForView(null)).toBe(NON_ROUTE_URL);
	});
});

describe('bootRoutePath', () => {
	it('keeps the query in a browser tab, minus the token', () => {
		expect(
			bootRoutePath({ pathname: '/ngwa/installed', search: '?token=t&surface=x' }, false)
		).toBe('/ngwa/installed?surface=x');
	});

	it('keeps the bare pathname on desktop (detached-window params are not route state)', () => {
		expect(bootRoutePath({ pathname: '/', search: '?detached=1&surface=x' }, true)).toBe('/');
	});
});

function routeLeaf(...paths: string[]): LeafNode {
	const leaf = makeLeaf({ kind: 'route', path: paths[0] });
	return { ...leaf, tabs: paths.map((path) => ({ kind: 'route', path })) };
}

function activePath(root: PaneNode, leafId: string): string | undefined {
	const leaf = findLeaf(root, leafId);
	const v = leaf?.tabs[leaf.activeTabIdx];
	return v?.kind === 'route' ? v.path : undefined;
}

describe('applyBootDeepLink', () => {
	it('navigates the restored focused route pane in place', () => {
		const leaf = routeLeaf('/sessions');
		const uid = tabUid(leaf.tabs[0]);
		const out = applyBootDeepLink(
			{ root: leaf, focusedId: leaf.id, closedHistory: [] },
			'/ngwa/health'
		);
		expect(activePath(out.root, leaf.id)).toBe('/ngwa/health');
		expect(tabUid((out.root as LeafNode).tabs[0])).toBe(uid);
	});

	it('switches to a tab that already holds the path', () => {
		const leaf = routeLeaf('/sessions', '/ngwa/health');
		const out = applyBootDeepLink(
			{ root: leaf, focusedId: leaf.id, closedHistory: [] },
			'/ngwa/health'
		);
		expect((out.root as LeafNode).activeTabIdx).toBe(1);
		expect((out.root as LeafNode).tabs).toHaveLength(2);
	});

	it('treats `/` and null as "no deep link" — the saved layout is kept as-is', () => {
		const leaf = routeLeaf('/sessions');
		const snap = { root: leaf, focusedId: leaf.id, closedHistory: [] };
		expect(applyBootDeepLink(snap, '/')).toBe(snap);
		expect(applyBootDeepLink(snap, null)).toBe(snap);
		expect(applyBootDeepLink(snap, '/?token=t')).toBe(snap);
	});

	it('never carries a token into the restored tree', () => {
		const leaf = routeLeaf('/sessions');
		const out = applyBootDeepLink(
			{ root: leaf, focusedId: leaf.id, closedHistory: [] },
			'/ngwa/health?token=t'
		);
		expect(activePath(out.root, leaf.id)).toBe('/ngwa/health');
	});
});

describe('setRouteTabPath / syncRouteTabPath', () => {
	beforeEach(() => {
		const root = routeLeaf('/ngwa/installed', '/sessions');
		usePaneStore.getState().hydrate({ root, focusedId: root.id, closedHistory: [] });
	});

	it('rewrites the tab by uid in place — identity kept, active tab untouched', () => {
		const { root } = usePaneStore.getState();
		const leaf = root as LeafNode;
		const uid = tabUid(leaf.tabs[0]);
		usePaneStore.getState().syncRouteTabPath(leaf.id, uid, '/ngwa/store');
		const after = usePaneStore.getState().root as LeafNode;
		expect(after.tabs[0]).toEqual({ kind: 'route', path: '/ngwa/store' });
		expect(tabUid(after.tabs[0])).toBe(uid);
		expect(after.activeTabIdx).toBe(0);
		expect(after.tabs[1]).toBe(leaf.tabs[1]);
	});

	it('is a no-op (same root reference) for an unchanged path or an unknown uid', () => {
		const { root } = usePaneStore.getState();
		const leaf = root as LeafNode;
		expect(setRouteTabPath(root, leaf.id, tabUid(leaf.tabs[0]), '/ngwa/installed')).toBe(root);
		expect(setRouteTabPath(root, leaf.id, 'nope', '/x')).toBe(root);
		expect(setRouteTabPath(root, 'no-leaf', tabUid(leaf.tabs[0]), '/x')).toBe(root);
	});

	it('keeps the pin on a pinned tab', () => {
		const leaf = makeLeaf({ kind: 'route', path: '/a', pinned: true });
		const out = setRouteTabPath(leaf, leaf.id, tabUid(leaf.tabs[0]), '/b') as LeafNode;
		expect(out.tabs[0]).toEqual({ kind: 'route', path: '/b', pinned: true });
	});

	it('sanitises at the store boundary: a ?token= never lands in the tree', () => {
		const { root } = usePaneStore.getState();
		const leaf = root as LeafNode;
		const uid = tabUid(leaf.tabs[0]);
		usePaneStore.getState().syncRouteTabPath(leaf.id, uid, '/ngwa/store?token=s3cret&tab=x#frag');
		const after = usePaneStore.getState().root as LeafNode;
		expect(after.tabs[0]).toEqual({ kind: 'route', path: '/ngwa/store?tab=x' });
		expect(JSON.stringify(after)).not.toContain('s3cret');

		// Reducer-level too, and a path that sanitises to the current one is a no-op.
		const out = setRouteTabPath(root, leaf.id, uid, '/ngwa/installed?token=t') as LeafNode;
		expect(out.tabs[0]).toEqual({ kind: 'route', path: '/ngwa/installed' });
		expect(setRouteTabPath(out, leaf.id, uid, '/ngwa/installed?token=other')).toBe(out);
	});
});
