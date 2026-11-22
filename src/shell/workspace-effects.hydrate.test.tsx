// `useWorkspaceEffects` itself restores the saved layout through the
// deep-link-aware hydrate (remote web session), and as saved on the desktop.
// Every other workspace hook is stubbed; the pane store and url-sync are real.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PaneTreeSnapshot } from '@/lib/panes/pane-persistence';

const h = vi.hoisted(() => ({
	web: true,
	snapshot: null as PaneTreeSnapshot | null,
	loadDelayMs: 0,
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
	isTauri: () => false,
}));
vi.mock('@/lib/panes/pane-persistence', async (orig) => ({
	...(await orig<typeof import('@/lib/panes/pane-persistence')>()),
	loadPaneTree: vi.fn(async () => {
		if (h.loadDelayMs) await new Promise((r) => setTimeout(r, h.loadDelayMs));
		return h.snapshot;
	}),
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
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	projectGetActive: vi.fn(async () => ({ id: 'p', root_path: '/srv/proj' })),
}));
vi.mock('@/terminal/session-store', () => ({
	useTerminalStore: { getState: () => ({ rehydrated: true }) },
}));

import { resetLayoutRestoredForTests } from '@/lib/panes/layout-restored';
import { findLeaf, makeLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { LeafNode, PaneNode } from '@/lib/panes/types';
import { openSharedArtifact } from './people/share-landing';
import { useWorkspaceEffects } from './workspace-effects';

// useWorkspaceEffects reads the query client (remote focus refetch).
const wrapper = ({ children }: { children: ReactNode }) => (
	<QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>
);

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
	h.loadDelayMs = 0;
	resetLayoutRestoredForTests();
	// The page's first pane, opened on the URL's route before the layout loads.
	const first = makeLeaf({ kind: 'route', path: '/ngwa/health' });
	usePaneStore.getState().hydrate({ root: first, focusedId: first.id, closedHistory: [] });
});
afterEach(() => cleanup());

describe('useWorkspaceEffects layout restore', () => {
	it('remote web session: restores the layout with the deep link in the saved focused pane', async () => {
		h.web = true;
		renderHook(() => useWorkspaceEffects(() => {}), { wrapper });
		await waitFor(() => expect(usePaneStore.getState().focusedId).toBe(routePane.id));
		expect(usePaneStore.getState().focusedView()).toEqual({
			kind: 'route',
			path: '/ngwa/health',
		});
	});

	it('desktop: restores the layout as saved', async () => {
		h.web = false;
		renderHook(() => useWorkspaceEffects(() => {}), { wrapper });
		await waitFor(() => expect(usePaneStore.getState().focusedId).toBe(routePane.id));
		expect(usePaneStore.getState().focusedView()).toEqual({ kind: 'route', path: '/settings' });
	});

	// Live defect D3: a Guest accepting an artifact invite reloads into the
	// share and the landing opens the shared file at boot, while the layout is
	// still being restored. The restore replaces the whole pane tree, so the
	// landing must open the file after it, not before.
	it('the share landing opened at boot survives the layout restore', async () => {
		h.web = true;
		h.loadDelayMs = 50; // the saved layout arrives over the network
		renderHook(() => useWorkspaceEffects(() => {}), { wrapper });
		const landed = openSharedArtifact('notes.md');
		await expect(landed).resolves.toBe(true);
		// Past the restore, whichever finished first.
		await new Promise((r) => setTimeout(r, 4 * h.loadDelayMs));
		const s = usePaneStore.getState();
		const leaf = findLeaf(s.root, s.focusedId);
		expect(leaf?.tabs[leaf.activeTabIdx]).toMatchObject({
			kind: 'artifact',
			path: '/srv/proj/notes.md',
		});
	});
});
