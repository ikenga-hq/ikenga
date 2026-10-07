// `useWorkspaceEffects` itself restores the saved layout through the
// deep-link-aware hydrate (remote web session), and as saved on the desktop.
// Every other workspace hook is stubbed; the pane store and url-sync are real.

import { cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneTreeSnapshot } from '@/lib/panes/pane-persistence';

const h = vi.hoisted(() => ({
	web: true,
	snapshot: null as PaneTreeSnapshot | null,
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
	isTauri: () => false,
}));
vi.mock('@/lib/panes/pane-persistence', async (orig) => ({
	...(await orig<typeof import('@/lib/panes/pane-persistence')>()),
	loadPaneTree: vi.fn(async () => h.snapshot),
	persistPaneTree: vi.fn(),
}));
vi.mock('@/lib/panes/router-pane-sync', () => ({ useRouterPaneSync: () => {} }));
vi.mock('@/lib/iyke/bridge', () => ({ useIykeBridge: () => {} }));
vi.mock('@/lib/iyke/control-listener', () => ({ useIykeControlListener: () => {} }));
vi.mock('@/lib/iyke/use-iyke-shell-sync', () => ({ useIykeShellSync: () => {} }));
vi.mock('@/lib/use-screenshot-listener', () => ({ useScreenshotListener: () => {} }));
vi.mock('@/lib/shell/use-projects-sync', () => ({ useProjectsSync: () => {} }));
vi.mock('@/lib/use-preload-viewers', () => ({ usePreloadViewers: () => {} }));
vi.mock('@/lib/use-pa-actions', () => ({ usePaActionsListener: () => {} }));
vi.mock('@/lib/dnd/os-file-drop', async (orig) => ({
	...(await orig<typeof import('@/lib/dnd/os-file-drop')>()),
	initOsFileDrop: vi.fn(async () => () => {}),
}));
vi.mock('@/lib/shell/panel-sizes', () => ({
	loadPanelSizes: vi.fn(async () => null),
	registerPanelSizesSetter: () => () => {},
}));
vi.mock('@/terminal/claude-settings', () => ({ loadClaudeSettingsPath: vi.fn(async () => null) }));
vi.mock('@/terminal/session-store', () => ({
	useTerminalStore: { getState: () => ({ rehydrated: true }) },
}));

import { makeLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { LeafNode, PaneNode } from '@/lib/panes/types';
import { useWorkspaceEffects } from './workspace-effects';

let routePane: LeafNode;
let termPane: LeafNode;

beforeEach(() => {
	routePane = makeLeaf({ kind: 'route', path: '/settings' });
	termPane = makeLeaf({ kind: 'terminal', sessionId: 's1' });
	const root: PaneNode = {
		type: 'split',
		direction: 'horizontal',
		children: [routePane, termPane],
		sizes: [50, 50],
	};
	h.snapshot = { root, focusedId: routePane.id, closedHistory: [] };
	// The page's first pane, opened on the URL's route before the layout loads.
	const first = makeLeaf({ kind: 'route', path: '/ngwa/health' });
	usePaneStore.getState().hydrate({ root: first, focusedId: first.id, closedHistory: [] });
});
afterEach(() => cleanup());

describe('useWorkspaceEffects layout restore', () => {
	it('remote web session: restores the layout with the deep link in the saved focused pane', async () => {
		h.web = true;
		renderHook(() => useWorkspaceEffects(() => {}));
		await waitFor(() => expect(usePaneStore.getState().focusedId).toBe(routePane.id));
		expect(usePaneStore.getState().focusedView()).toEqual({
			kind: 'route',
			path: '/ngwa/health',
		});
	});

	it('desktop: restores the layout as saved', async () => {
		h.web = false;
		renderHook(() => useWorkspaceEffects(() => {}));
		await waitFor(() => expect(usePaneStore.getState().focusedId).toBe(routePane.id));
		expect(usePaneStore.getState().focusedView()).toEqual({ kind: 'route', path: '/settings' });
	});
});
