// Gap audit rank 24 stopgap — the workspace mounts the browser-session focus
// refetch for Claude-config queries. Without this mount the hook (tested on
// its own) never runs.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook } from '@testing-library/react';
import { createElement, type ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ refetch: vi.fn() }));

vi.mock('@/lib/queries/claude-config', async (orig) => ({
	...(await orig<typeof import('@/lib/queries/claude-config')>()),
	useRemoteConfigFocusRefetch: h.refetch,
}));

// The other effects need a router / Tauri; only the mount under test matters.
vi.mock('@/lib/panes/router-pane-sync', () => ({ useRouterPaneSync: () => {} }));
vi.mock('@/lib/shell/use-projects-sync', () => ({ useProjectsSync: () => {} }));
vi.mock('@/lib/use-pa-actions', () => ({ usePaActionsListener: () => {} }));
vi.mock('@/lib/use-screenshot-listener', () => ({ useScreenshotListener: () => {} }));
vi.mock('@/lib/iyke/control-listener', () => ({ useIykeControlListener: () => {} }));
vi.mock('@/lib/iyke/bridge', () => ({ useIykeBridge: () => {} }));

import { useWorkspaceEffects } from './workspace-effects';

afterEach(() => cleanup());

describe('useWorkspaceEffects', () => {
	it('mounts the remote config focus refetch', () => {
		const wrapper = ({ children }: { children: ReactNode }) =>
			createElement(QueryClientProvider, { client: new QueryClient() }, children);
		renderHook(() => useWorkspaceEffects(() => {}), { wrapper });
		expect(h.refetch).toHaveBeenCalled();
	});
});
